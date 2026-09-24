//! 回合的开始、结束与journal。
//!
//! 每个 `complete_*` / `interrupt_*` 都有「带用量」「带修订号」的变体：模型信息
//! 与用量不是每条路径都有（本地命令没有用量，重做有修订号），硬凑一个统一签名
//! 只会让调用方传一堆 `None`。

use crate::state::*;

impl StateStore {
    /// A clone pinned to the given session: it shares the database but holds
    /// its own session pointer, unaffected by later `switch_session` /
    /// `adopt_session` calls on other clones. Used by concurrently running
    /// turns so each keeps writing to the session it started in.
    pub fn pinned(&self, session_id: &str) -> Self {
        Self {
            state_dir: self.state_dir.clone(),
            artifacts_dir: self.artifacts_dir.clone(),
            shared_files_dir: self.shared_files_dir.clone(),
            conv_db: self.conv_db.clone(),
            platform_access: self.platform_access.clone(),
            session_id: Arc::new(std::sync::RwLock::new(session_id.into())),
            queue_session_id: self.queue_session_id.clone(),
            queue_owner_pid: self.queue_owner_pid,
            usage_account: self.usage_account.clone(),
        }
    }

    /// Like [`pinned`], but with a fresh queue identity so concurrently
    /// running turns in the same session never consume each other's queued
    /// follow-up prompts. Callers should `discard_queued_prompts()` when the
    /// turn finishes.
    pub fn pinned_for_turn(&self, session_id: &str) -> Self {
        let mut store = self.pinned(session_id);
        // 会话归属决定这一回合记在谁的账上(阶段 5):成员的会话归成员。
        if let Ok(Some(record)) = store.session_record(session_id) {
            if !record.owner.is_empty() {
                store.usage_account = record.owner.into();
            }
        }
        store.queue_session_id = format!(
            "queue_{}_{}_{}",
            store.queue_owner_pid,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0),
            rand::random::<u64>()
        )
        .into();
        store
    }

    pub fn queue_target(&self, turn_id: impl Into<String>) -> RunningTurnQueueTarget {
        RunningTurnQueueTarget {
            turn_id: turn_id.into(),
            queue_session_id: Some(self.queue_session_id.to_string()),
            owner_pid: Some(self.queue_owner_pid),
        }
    }

    pub fn latest_turn_seq(&self, session_id: &str) -> Result<i64> {
        self.conv_db.latest_turn_seq(session_id)
    }

    pub fn start_turn(&self, turn_id: &str, user_content: &str, owner_pid: u32) -> Result<()> {
        self.start_turn_with_display(turn_id, user_content, user_content, owner_pid, None)
    }

    pub fn start_turn_with_display(
        &self,
        turn_id: &str,
        user_content: &str,
        display_content: &str,
        owner_pid: u32,
        attachment_run_id: Option<&str>,
    ) -> Result<()> {
        // Record the ambient turn workspace (if any) so the turn row captures
        // where its tools operated; NULL outside a turn workspace scope.
        let workspace =
            miyu_base::workspace::try_workspace().map(|path| path.display().to_string());
        self.conv_db.start_turn(
            &self.session(),
            turn_id,
            user_content,
            display_content,
            owner_pid,
            &self.queue_session_id,
            workspace.as_deref(),
            attachment_run_id,
        )
    }

    #[allow(dead_code)]
    pub fn complete_turn(
        &self,
        turn_id: &str,
        content: &str,
        reasoning: Option<&str>,
    ) -> Result<()> {
        self.conv_db.complete_turn(turn_id, content, reasoning)
    }

    pub fn interrupt_turn(&self, turn_id: &str) -> Result<()> {
        self.interrupt_turn_with_usage(turn_id, TurnTokens::default())
    }

    /// 带用量的打断:见 `ConversationDb::interrupt_turn_with_usage`。
    pub fn interrupt_turn_with_usage(&self, turn_id: &str, tokens: TurnTokens) -> Result<()> {
        self.conv_db.interrupt_turn_with_usage(turn_id, tokens)?;
        let session_id = self.session_id();
        self.recover_journal_assets(&session_id, turn_id)
    }

    /// daemon 有序关停时的收尾：只记用量、不改状态，见
    /// `ConversationDb::suspend_turn_with_usage`（09-24 断点续跑）。
    pub fn suspend_turn_with_usage(&self, turn_id: &str, tokens: TurnTokens) -> Result<()> {
        self.conv_db.suspend_turn_with_usage(turn_id, tokens)
    }

    /// 还没结案的孤儿回合（这个库里所有会话的），见 `conversation_db::restart`。
    pub fn pending_restart_orphans(&self) -> Result<Vec<RestartOrphan>> {
        self.conv_db.pending_restart_orphans()
    }

    pub fn close_restart_orphan(&self, turn_id: &str, outcome: &str, detail: &str) -> Result<()> {
        self.conv_db.close_restart_orphan(turn_id, outcome, detail)
    }

    pub fn has_turn_after(&self, session_id: &str, seq: i64) -> Result<bool> {
        self.conv_db.has_turn_after(session_id, seq)
    }

    pub fn turn_started_at(&self, turn_id: &str) -> Result<Option<String>> {
        self.conv_db.turn_started_at(turn_id)
    }

    pub fn interrupt_turn_revision(&self, turn_id: &str, revision: i64) -> Result<()> {
        self.interrupt_turn_revision_with_usage(turn_id, revision, TurnTokens::default())
    }

    /// 带用量的重做打断:见 `ConversationDb::interrupt_turn_revision_with_usage`。
    pub fn interrupt_turn_revision_with_usage(
        &self,
        turn_id: &str,
        revision: i64,
        tokens: TurnTokens,
    ) -> Result<()> {
        let restored = self
            .conv_db
            .interrupt_turn_revision_with_usage(turn_id, revision, tokens)?;
        if restored {
            let session_id = self
                .conv_db
                .turn_session_id(turn_id)?
                .context("restored redo turn no longer exists")?;
            self.reconcile_managed_artifacts_for_turn(&session_id, turn_id)?;
        } else {
            let session_id = self
                .conv_db
                .turn_session_id(turn_id)?
                .context("interrupted turn no longer exists")?;
            self.recover_journal_assets(&session_id, turn_id)?;
        }
        Ok(())
    }

    pub fn complete_turn_with_usage_and_model(
        &self,
        turn_id: &str,
        content: &str,
        reasoning: Option<&str>,
        provider_id: Option<&str>,
        model: Option<&str>,
        tokens: TurnTokens,
        token_usage_estimated: bool,
    ) -> Result<()> {
        self.conv_db.complete_turn_with_usage(
            turn_id,
            content,
            reasoning,
            provider_id,
            model,
            tokens,
            token_usage_estimated,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn complete_turn_revision_with_usage_and_model(
        &self,
        turn_id: &str,
        revision: i64,
        content: &str,
        reasoning: Option<&str>,
        provider_id: Option<&str>,
        model: Option<&str>,
        tokens: TurnTokens,
        token_usage_estimated: bool,
    ) -> Result<()> {
        self.conv_db.complete_turn_revision_with_usage(
            turn_id,
            revision,
            content,
            reasoning,
            provider_id,
            model,
            tokens,
            token_usage_estimated,
        )
    }

    pub fn append_persisted_contexts(&self, turn_id: &str, reports: &[String]) -> Result<()> {
        self.conv_db.append_tool_reports(turn_id, reports)
    }

    /// Archives the transient system tail that was sent after the user message
    /// of this turn (v7 append-only fossilization). Replayed verbatim by
    /// history rendering so the byte stream stays a pure extension.
    pub fn set_turn_tool_flow(
        &self,
        turn_id: &str,
        flow: &[conversation_db::ToolFlowRound],
    ) -> Result<()> {
        self.conv_db.set_turn_tool_flow(turn_id, flow)
    }

    pub fn set_turn_context_end(&self, turn_id: &str, tokens: Option<u64>) -> Result<()> {
        self.conv_db.set_turn_context_end(turn_id, tokens)
    }

    pub fn set_turn_generation(&self, turn_id: &str, tokens: u64, millis: u64) -> Result<()> {
        self.conv_db.set_turn_generation(turn_id, tokens, millis)
    }

    pub fn load_turn_generation(
        &self,
        session_id: &str,
    ) -> Result<std::collections::HashMap<String, (u64, u64)>> {
        self.conv_db.load_turn_generation(session_id)
    }

    /// 见 `ConversationDb::latest_context_end_tokens`：含正在跑的这一轮。
    pub fn latest_context_end_tokens(&self) -> Result<Option<u64>> {
        self.conv_db.latest_context_end_tokens(&self.session())
    }

    pub fn load_context_anchor(&self) -> Result<Option<crate::state::ContextAnchor>> {
        self.conv_db.load_context_anchor(&self.session())
    }

    /// 最近一条实测锚点，连同它之后的可见回合（被打断、用量是估的那几轮）。
    /// 见 `ConversationDb::load_last_measured_anchor`。
    pub fn load_context_anchor_and_tail(
        &self,
    ) -> Result<Option<(crate::state::ContextAnchor, Vec<Turn>)>> {
        let session_id = self.session();
        let Some((anchor, seq)) = self.conv_db.load_last_measured_anchor(&session_id)? else {
            return Ok(None);
        };
        let tail = self.conv_db.load_visible_turns_after(&session_id, seq)?;
        Ok(Some((anchor, tail)))
    }

    /// 会话「当前上下文」落库(v38):算出来就写,`/session` 列表直接读。
    pub fn set_session_context_tokens(&self, session_id: &str, tokens: u64) -> Result<()> {
        self.conv_db.set_session_context_tokens(session_id, tokens)
    }

    pub fn set_turn_context_messages(
        &self,
        turn_id: &str,
        messages: &[crate::llm::ChatMessage],
    ) -> Result<()> {
        self.conv_db.set_turn_context_messages(turn_id, messages)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn append_turn_journal_event(
        &self,
        turn_id: &str,
        revision: i64,
        segment_index: i64,
        kind: &str,
        call_id: Option<&str>,
        name: Option<&str>,
        text_payload: Option<&str>,
        blob_payload: Option<&[u8]>,
        ok: Option<bool>,
    ) -> Result<()> {
        self.conv_db.append_turn_journal_event(
            turn_id,
            revision,
            segment_index,
            kind,
            call_id,
            name,
            text_payload,
            blob_payload,
            ok,
        )
    }

    pub fn supersede_turn_journal_segment(
        &self,
        turn_id: &str,
        revision: i64,
        segment_index: i64,
    ) -> Result<()> {
        self.conv_db
            .supersede_turn_journal_segment(turn_id, revision, segment_index)
    }

    pub fn append_question_exchange(
        &self,
        turn_id: &str,
        exchange: &miyu_base::question::QuestionExchange,
    ) -> Result<()> {
        self.conv_db.append_question_exchange(turn_id, exchange)
    }

    pub fn recover_stale_turns(&self) -> Result<usize> {
        let recoveries = self.conv_db.recover_stale_running_turns()?;
        for recovery in &recoveries {
            if recovery.restored_redo {
                self.reconcile_managed_artifacts_for_turn(&recovery.session_id, &recovery.turn_id)?;
            } else {
                self.recover_journal_assets(&recovery.session_id, &recovery.turn_id)?;
            }
        }
        Ok(recoveries.len())
    }

    pub fn merge_turn_footprint(
        &self,
        turn_id: &str,
        delta: &crate::state::ToolFootprint,
    ) -> Result<()> {
        self.conv_db.merge_turn_footprint(turn_id, delta)
    }

    pub fn load_merged_footprint(
        &self,
        turn_ids: &[String],
    ) -> Result<crate::state::ToolFootprint> {
        self.conv_db
            .load_merged_footprint(&self.session(), turn_ids)
    }

    #[allow(dead_code)]
    pub fn has_running_turns(&self) -> Result<bool> {
        self.conv_db.has_running_turns(&self.session())
    }

    #[allow(dead_code)]
    pub fn running_turn_summaries(&self) -> Result<Vec<String>> {
        self.conv_db.running_turn_summaries(&self.session())
    }

    #[allow(dead_code)]
    pub fn running_turn_summaries_excluding(&self, exclude_turn_id: &str) -> Result<Vec<String>> {
        self.conv_db
            .running_turn_summaries_excluding(&self.session(), exclude_turn_id)
    }
}

#[cfg(any(test, feature = "testkit"))]
mod test_support;
