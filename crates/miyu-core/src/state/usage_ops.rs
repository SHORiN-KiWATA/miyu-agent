//! 用量的累计、落账与统计。
//!
//! 用量分两条：当前回合的（内存里累加）和历史的（记进机器级的用量账本
//! `state/usage.db`，见 `usage`）。子代理的用量要算进
//! 发起它的会话（`record_subagent_usage`），否则「这次对话花了多少」是错的。

use crate::state::*;

impl StateStore {
    // ---- 断缓存（09-25）----
    //
    // 纯转发。SQL 在 `conversation_db/cache_breaks.rs`，判定在 `llm::cache_break`。

    pub fn record_cache_break(&self, entry: &crate::llm::CacheBreak) -> Result<()> {
        self.conv_db.record_cache_break(entry)
    }

    /// 会话树（这条会话 + 名下所有子代理）一共断过几次缓存。
    pub fn cache_break_count(&self, root: &str) -> Result<u64> {
        self.conv_db.cache_break_count(root)
    }

    pub fn recent_cache_breaks(
        &self,
        root: &str,
        limit: usize,
    ) -> Result<Vec<crate::state::CacheBreakRecord>> {
        self.conv_db.recent_cache_breaks(root, limit)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn record_subagent_usage(
        &self,
        session_id: &str,
        provider_id: Option<&str>,
        model: Option<&str>,
        context_window: Option<i64>,
        prompt_tokens: i64,
        completion_tokens: i64,
        total_tokens: i64,
        cache_read_tokens: i64,
    ) -> Result<()> {
        self.conv_db.record_subagent_usage(
            session_id,
            provider_id,
            model,
            context_window,
            prompt_tokens,
            completion_tokens,
            total_tokens,
            cache_read_tokens,
        )
    }

    /// 机器级的用量账本(`state/usage.db`),所有账号共用。
    fn usage_ledger(&self) -> Result<Arc<usage::UsageDb>> {
        usage::ledger(&self.state_dir)
    }

    pub fn reset_conversation_usage(&self) -> Result<()> {
        self.usage_ledger()?.reset_conversation()
    }

    pub fn add_usage(&self, usage: &Usage, meta: UsageMeta<'_>) -> Result<()> {
        self.init_files()?;
        self.usage_ledger()?.add(usage, true)?;
        self.record_usage_history(usage, meta, false);
        Ok(())
    }

    pub fn add_auxiliary_usage(&self, usage: &Usage, meta: UsageMeta<'_>) -> Result<()> {
        self.init_files()?;
        self.usage_ledger()?.add(usage, false)?;
        self.record_usage_history(usage, meta, true);
        Ok(())
    }

    /// 明细落账失败只告警:累计是正账,明细缺一行不该让整个回合报错。
    pub(crate) fn record_usage_history(&self, usage: &Usage, meta: UsageMeta<'_>, aux: bool) {
        let recorded = self
            .usage_ledger()
            .and_then(|ledger| ledger.record(usage, meta, aux, &self.usage_account));
        if let Err(error) = recorded {
            tracing::warn!(error = %error, "recording usage history failed");
        }
    }

    /// 清空逐次调用明细。累计不动,见 [`usage::UsageDb::clear_history`]。
    pub fn clear_usage_history(&self) -> Result<()> {
        self.usage_ledger()?.clear_history()
    }

    /// 供应商改名后同步用量账本;见 [`usage::UsageDb::rename_provider`]。
    pub fn rename_usage_provider(&self, old: &str, new: &str) -> Result<usize> {
        self.usage_ledger()?.rename_provider(old, new)
    }

    /// `config` 提供时按 models.dev 单价做计费估算;None 则费用字段全零。
    pub fn usage_stats(
        &self,
        range: UsageRange,
        config: Option<&miyu_base::config::AppConfig>,
    ) -> Result<usage::UsageStats> {
        self.usage_stats_for_account(range, config, None)
    }

    /// `account` 为 Some 时只统计该账号(空串 = 管理员/遗留);None = 全部并按人拆分。
    pub fn usage_stats_for_account(
        &self,
        range: UsageRange,
        config: Option<&miyu_base::config::AppConfig>,
        account: Option<&str>,
    ) -> Result<usage::UsageStats> {
        let ledger = self.usage_ledger()?;
        match config {
            Some(config) => {
                let price = miyu_base::models_cache::pricing_resolver(config);
                ledger.stats(range, &price, account)
            }
            None => ledger.stats(range, &|_, _| None, account),
        }
    }

    pub fn usage_details_for_account(
        &self,
        limit: usize,
        src: Option<&str>,
        model: Option<&str>,
        config: Option<&miyu_base::config::AppConfig>,
        account: Option<&str>,
    ) -> Result<Vec<usage::UsageRecord>> {
        let ledger = self.usage_ledger()?;
        match config {
            Some(config) => {
                let price = miyu_base::models_cache::pricing_resolver(config);
                ledger.details(limit, src, model, &price, account)
            }
            None => ledger.details(limit, src, model, &|_, _| None, account),
        }
    }

    #[allow(dead_code)]
    pub fn usage_snapshot(&self) -> Result<UsageSnapshot> {
        self.usage_ledger()?.snapshot()
    }

    /// Same Σ, plus the prompt and cache-read halves the cumulative cache rate
    /// is computed from.
    pub fn session_cumulative_token_totals(&self) -> Result<TurnTokens> {
        self.conv_db.session_token_totals(&self.session())
    }

    pub fn clear_last_usage(&self) -> Result<()> {
        self.usage_ledger()?.clear_last_usage()
    }
}
