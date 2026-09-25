mod anthropic;
mod antigravity;
mod builder;
mod chat;
mod chat_consume;
mod claude_code;
mod cli_relay;
mod codebuddy;
mod codex;
mod dsml;
mod endpoints;
mod errors;
mod lower;
mod protocol;
mod sse;
mod variants;
mod wire;
mod zen_headers;
mod zen_tools;
// 三条中转线的去重名单:断言「名单里都是真工具」的测试住在工具侧
// (`tools::relay_tests`,拿注册表核),这里按线别名出去(09-16)。
pub use antigravity::pool::{
    retire_all as retire_relay_processes, shutdown_all as shutdown_relay_processes,
};
pub use antigravity::remove_relay_files_now as remove_antigravity_relay_files;
use antigravity::AntigravityRuntime;
pub use antigravity::BRIDGE_DUPLICATE_TOOLS as ANTIGRAVITY_BRIDGE_DUPLICATE_TOOLS;
pub use chat::{ContentPolicyBlocked, ContextOverflowed};
use claude_code::ClaudeCodeRuntime;
pub use claude_code::BRIDGE_DUPLICATE_TOOLS as CLAUDE_CODE_BRIDGE_DUPLICATE_TOOLS;
pub use cli_relay::forget_relay_sessions;
use codebuddy::CodeBuddyRuntime;
use codex::CodexRuntime;
pub use codex::BRIDGE_DUPLICATE_TOOLS as CODEX_BRIDGE_DUPLICATE_TOOLS;
use dsml::*;
use endpoints::*;
use errors::*;
use lower::*;
pub use protocol::ThinkingVariantOptions;
use protocol::*;
pub use protocol::{
    remove_session_thinking_variants, thinking_variant_options_for_model,
    ThinkingVariantPreferences, ThinkingVariantScope, MODEL_DEFAULT_PIN,
};
use sse::*;
use wire::*;
pub use zen_tools::WIRE_ALIASES as ZEN_WIRE_ALIASES;

use super::{
    ChatMessage, ChatResult, ChatStreamChunk, ChatStreamKind, ResponsesContinuation, ToolCall,
    ToolCallFunction, ToolDefinition, Usage,
};
use anyhow::{bail, Context, Result};
use futures_util::{Stream, StreamExt};
use miyu_base::config::{AppConfig, ProviderConfig};
use miyu_base::i18n::text as t;
use miyu_base::models_cache::{self, ModelReasoningInfo, ReasoningSetting, ReasoningVariant};
use miyu_base::paths::MiyuPaths;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

static TOOL_CALL_COUNTER: AtomicU64 = AtomicU64::new(0);
static LLM_REQUEST_COUNTER: AtomicU64 = AtomicU64::new(0);
static LLM_SCHEDULER: LazyLock<Mutex<LlmScheduler>> =
    LazyLock::new(|| Mutex::new(LlmScheduler::default()));

fn gen_tool_call_id() -> String {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let n = TOOL_CALL_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("call_{ts}_{n}")
}

fn gen_llm_request_id() -> String {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let n = LLM_REQUEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("llm_{ts}_{n}")
}

#[derive(Clone)]
pub struct OpenAiCompatibleClient {
    client: Client,
    provider: ProviderConfig,
    api_key: String,
    endpoints: Arc<Vec<LlmEndpoint>>,
    thinking_variants: HashMap<String, String>,
    reasoning_visibility: ReasoningVisibility,
    /// True when partial output never reaches a person mid-request — platform
    /// turns buffer a round and post it as one message. Nothing is committed
    /// until the round ends, so a dropped stream can be retried invisibly.
    buffered_delivery: bool,
    detailed_reasoning_summary: bool,
    request_timeouts: Option<RequestTimeouts>,
    /// Per-clone completion cap. Auxiliary callers (compaction summaries)
    /// clone the client and set this so a runaway summary cannot eat the
    /// window; None leaves the provider default untouched.
    max_tokens_override: Option<u32>,
    continuation_health: ResponsesContinuationHealth,
    /// Scope tag for the per-request cache accounting log ("chat", "qq-judge",
    /// "compact", …). Auxiliary callers override it via `with_request_scope`
    /// so cache stats separate the main conversation from side channels.
    request_scope: &'static str,
    /// claude-code 协议的运行时参数;端点池里没有该协议的端点时为 None。
    claude_code: Option<Arc<ClaudeCodeRuntime>>,
    /// antigravity 协议的运行时参数;端点池里没有该协议的端点时为 None。
    antigravity: Option<Arc<AntigravityRuntime>>,
    /// codex 协议的运行时参数;端点池里没有该协议的端点时为 None。
    codex: Option<Arc<CodexRuntime>>,
    codebuddy: Option<Arc<CodeBuddyRuntime>>,
    /// 本会话是否 dev 模式(Agent 构造时置位),claude-code 的双四档工具
    /// 作用域(native_tools/miyu_tools)按它判定。
    claude_code_dev_mode: bool,
    /// 会话标识,只给 opencode Zen 的 `x-opencode-session` 用(Agent 构造时
    /// 由 `StateStore::session_id` 置位)。未置位时退回进程级的那个,见
    /// `zen_headers`。
    zen_session: Option<String>,
    /// 记账日志的归属:哪个会话、哪一轮。`cache-usage.jsonl` 少了这两维就
    /// 没法对账——09-22 排查缓存时只能按 prompt 单调递增去猜会话边界,还把
    /// 「新会话第一轮」误判成了断裂。与 `zen_session` 分开是因为那个的语义
    /// 是 opencode 的请求头,不该被日志绑住。
    log_identity: LogIdentity,
}

/// 记账日志的会话/回合归属。回合 id 每轮变而客户端跨回合存活,所以走一个
/// 共享槽;辅助客户端(压缩、判官)clone 过去时跟着一起继承。
#[derive(Clone, Default)]
pub(crate) struct LogIdentity {
    session: Option<Arc<str>>,
    turn: Arc<std::sync::Mutex<Option<Arc<str>>>>,
}

impl LogIdentity {
    pub(crate) fn session(&self) -> Option<&str> {
        self.session.as_deref()
    }

    pub(crate) fn turn(&self) -> Option<Arc<str>> {
        self.turn.lock().ok().and_then(|slot| slot.clone())
    }

    /// 认领一个会话,连同一个只属于它的回合槽。daemon 每轮从同一个共享客户端
    /// 克隆,若沿用克隆来的槽,并发的几个会话会共用它,日志里的 turn 就串到别的
    /// 会话头上(09-24 取证:两个会话的请求交替顶着对方的回合号)。
    pub(crate) fn for_session(self, session_id: &str) -> Self {
        Self {
            session: Some(session_id.into()),
            turn: Arc::default(),
        }
    }
}

#[cfg(test)]
mod log_identity_tests {
    use super::LogIdentity;

    fn set_turn(identity: &LogIdentity, turn: &str) {
        *identity.turn.lock().unwrap() = Some(turn.into());
    }

    #[test]
    fn sessions_cloned_from_one_client_keep_their_own_turn() {
        let shared = LogIdentity::default();
        let a = shared.clone().for_session("a");
        let b = shared.clone().for_session("b");
        set_turn(&a, "turn_a");
        set_turn(&b, "turn_b");
        assert_eq!(a.turn().as_deref(), Some("turn_a"));
        assert_eq!(b.turn().as_deref(), Some("turn_b"));
        // 同一会话派生出去的辅助客户端(压缩、判官)照旧跟着主客户端的回合走。
        let helper = a.clone();
        set_turn(&a, "turn_a2");
        assert_eq!(helper.turn().as_deref(), Some("turn_a2"));
    }
}

#[derive(Clone, Copy)]
struct RequestTimeouts {
    response_header: Duration,
    stream_idle: Duration,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReasoningVisibility {
    Hidden,
    Summary,
    Full,
}

impl OpenAiCompatibleClient {}

#[cfg(test)]
mod tests;
