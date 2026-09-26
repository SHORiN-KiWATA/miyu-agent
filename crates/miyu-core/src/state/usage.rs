//! 用量账本：每次 LLM 调用的累计与明细。
//!
//! 账本是机器级的，所有账号共用，明细的 `acct` 列区分账号，所以独立于按账号分的
//! 会话库。09-24 起存在 `state/usage.db`（会话项目第 1 段）。以前是 `usage.json`
//! （累计）加 `usage-history.jsonl`（明细），有三个毛病：
//! - 每记一次账都要把 usage.json 整个读出来、改、fsync、改名。缓存保活、QQ 每条
//!   消息的主动回复判断，都要这么来一遍；
//! - 统计每次都把几 MB 的明细从头解析一遍；
//! - 进程内锁挡不住别的进程（设置界面改供应商名、没开 daemon 时的 reset）。
//!
//! 老文件第一次开账本时导进来（见 `legacy`），本版保留。

use crate::llm::Usage;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;

mod db;
mod legacy;
mod stats;
pub use db::UsageDb;
pub use stats::*;

fn u64_is_zero(value: &u64) -> bool {
    *value == 0
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// 打开（或拿到本进程已经开着的）用量账本 `state_dir/usage.db`。
pub fn ledger(state_dir: &Path) -> Result<Arc<UsageDb>> {
    UsageDb::shared(state_dir)
}

#[derive(Default, Serialize, Deserialize)]
pub(in crate::state::usage) struct UsageState {
    requests: u64,
    prompt_tokens: u64,
    completion_tokens: u64,
    total_tokens: u64,
    #[serde(default)]
    conversation_tokens: u64,
    /// Cumulative provider-cache accounting (v7 Release 1). cache_read is the
    /// portion of prompt_tokens served from the provider's prompt cache.
    #[serde(default)]
    cache_read_tokens: u64,
    #[serde(default)]
    cache_write_tokens: u64,
    #[serde(default)]
    reasoning_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_usage: Option<Usage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_conversation_usage: Option<Usage>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Default)]
pub struct UsageSnapshot {
    pub requests: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub conversation_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub reasoning_tokens: u64,
    pub last_usage: Option<Usage>,
    pub last_conversation_usage: Option<Usage>,
}

impl From<UsageState> for UsageSnapshot {
    fn from(state: UsageState) -> Self {
        let last_conversation_usage = state
            .last_conversation_usage
            .clone()
            .or_else(|| state.last_usage.clone());
        Self {
            requests: state.requests,
            prompt_tokens: state.prompt_tokens,
            completion_tokens: state.completion_tokens,
            total_tokens: state.total_tokens,
            conversation_tokens: state.conversation_tokens,
            cache_read_tokens: state.cache_read_tokens,
            cache_write_tokens: state.cache_write_tokens,
            reasoning_tokens: state.reasoning_tokens,
            last_usage: state.last_usage,
            last_conversation_usage,
        }
    }
}

/// 一次 LLM 调用的历史记录。`src` 标来源:"agent"(终端/WebUI/定时/子代理)
/// 或平台 id(如 "qq");旧记录缺省归 "agent"。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UsageRecord {
    #[serde(default)]
    pub ts: i64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub src: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub provider: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub model: String,
    #[serde(default)]
    pub prompt: u64,
    #[serde(default)]
    pub completion: u64,
    #[serde(default)]
    pub total: u64,
    #[serde(default, skip_serializing_if = "u64_is_zero")]
    pub cache_read: u64,
    #[serde(default, skip_serializing_if = "u64_is_zero")]
    pub cache_write: u64,
    #[serde(default, skip_serializing_if = "is_false")]
    pub aux: bool,
    /// 细项标签(见 [`UsageMeta::kind`]);老记录没有,空串=主线。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub kind: String,
    /// 归属账号 id(阶段 5 多用户);空串 = 管理员/遗留/平台。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub acct: String,
    /// 计费估算(USD),读取时按当前价目计算,不落盘。
    #[serde(skip_deserializing, skip_serializing_if = "Option::is_none")]
    pub cost: Option<f64>,
}

/// 调用元数据,由各埋点交代。provider/model 拿不到就 None(如缓存保活)。
#[derive(Debug, Clone, Copy, Default)]
pub struct UsageMeta<'a> {
    pub source: &'a str,
    pub provider: Option<&'a str>,
    pub model: Option<&'a str>,
    /// 细项标签:同一来源里再分一层(如 "judge"=主动回复判断)。None=主线
    /// 调用。统计据此在来源下挂出子项,不改变来源本身的归属与合计。
    pub kind: Option<&'a str>,
}

/// 主动回复判断(QQ 每条消息都跑)的细项标签。
pub const USAGE_KIND_JUDGE: &str = "judge";
/// 好感度更新。
pub const USAGE_KIND_AFFECTION: &str = "affection";
/// 入群审批。
pub const USAGE_KIND_GROUP_JOIN: &str = "group_join";

#[cfg(test)]
mod tests;

#[cfg(test)]
mod probes;

#[cfg(any(test, feature = "testkit"))]
mod test_support;
#[cfg(any(test, feature = "testkit"))]
#[allow(unused_imports)]
pub use test_support::*;
