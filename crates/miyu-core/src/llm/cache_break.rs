//! 断缓存：这一次请求本来能从缓存读到的，有一大截重算了（09-25，用户拍板照 Claude Code 的判法）。
//!
//! footer 上的 C% 是「累计缓存读 / 累计输入」，和 dsh、Claude Code、Gemini 同一类口径；每一步新追加的
//! 内容本来就不可能在缓存里，所以它天然只有八九成，看不出缓存有没有被掰断。断缓存补的就是这一块：
//! 逐请求和同一会话的上一请求比——
//!
//! - 本来能读到的量 = min(上一请求的完整 prompt, 本次完整 prompt)（完整 = 含缓存读的全部输入）；
//! - 本次缓存读不到它的 95%，且缺口 ≥ 2000 token，就记一次；
//! - 原因按请求前缀的差异归类（`cache_prefix`）：系统提示词变了、工具表变了、历史里某条被改写，
//!   或者我们一字未改、供应商没接住；
//! - 压缩之后的下一次请求是**预期重建**，不记（`note_cache_rebuild`）；
//! - 进程里没有上一请求可比的（会话第一次、daemon 重启后第一次）是冷启动，不记；供应商不报缓存
//!   用量的（agy 这类）不判。
//!
//! 判出来的先攒在进程里，回合循环每次请求后按会话取走（`take_cache_breaks`）落库。

use crate::llm::cache_prefix::PrefixDiff;
use crate::llm::Usage;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

/// 只判主对话（含子代理会话）的请求。判官、标题、压缩这些旁路各有各的前缀，不算。
const MAIN_SCOPE: &str = "chat";
/// 本次缓存读至少要到「本来能读到的量」的这个比例（Claude Code 同款）。
const HIT_RATIO: f64 = 0.95;
/// 缺口少于这么多 token 不算：缓存按块取整（DeepSeek 128），零头每次都有。
const MISS_FLOOR: u64 = 2_000;

/// 断缓存的原因。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CacheBreakCause {
    /// 系统提示词变了（请求的第一条消息就不一样）。
    SystemPrompt,
    /// 工具表变了；`changes` 同 cache-usage 日志的 `tools_diff`（`+新增`、`-移除`、`~改了`）。
    Tools { changes: String },
    /// 历史里第 `at` 条（从 0 数，角色 `role`）被改写了。
    History { at: usize, role: String },
    /// 我们这边一字未改（纯追加），供应商没接住：逐出、过期、打到冷节点。
    Provider,
}

/// 一次断缓存。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheBreak {
    pub session_id: String,
    pub turn_id: Option<String>,
    /// 本来能从缓存读到、却重算了的 token 数。
    pub lost_tokens: u64,
    pub cause: CacheBreakCause,
    /// 离上一次请求过了多久（秒）。空闲很久之后的「供应商没接住」多半是缓存过期了。
    pub idle_secs: u64,
}

struct LastRequest {
    prompt: u64,
    at: Instant,
}

#[derive(Default)]
struct Tracker {
    last: HashMap<String, LastRequest>,
    /// 刚压缩过、下一次请求是预期重建的会话。
    rebuilding: HashSet<String>,
    /// 判出来、还没被回合循环取走的。
    pending: HashMap<String, Vec<CacheBreak>>,
}

static TRACKER: OnceLock<Mutex<Tracker>> = OnceLock::new();

fn tracker() -> &'static Mutex<Tracker> {
    TRACKER.get_or_init(Mutex::default)
}

/// 一次请求成功之后判一判（`chat.rs` 在记 cache-usage 那一行的同一处调）。
pub(crate) fn judge(
    scope: &str,
    session: Option<&str>,
    turn: Option<&str>,
    prefix: &PrefixDiff,
    usage: Option<&Usage>,
) {
    let (Some(session), Some(usage)) = (session, usage) else {
        return;
    };
    if scope != MAIN_SCOPE || !usage.cache_reported {
        return;
    }
    let Ok(mut tracker) = tracker().lock() else {
        return;
    };
    let now = Instant::now();
    let rebuilt = tracker.rebuilding.remove(session);
    let previous = tracker.last.insert(
        session.to_string(),
        LastRequest {
            prompt: usage.prompt_tokens,
            at: now,
        },
    );
    let Some(previous) = previous else {
        return;
    };
    let reachable = previous.prompt.min(usage.prompt_tokens);
    let lost = reachable.saturating_sub(usage.cache_read_tokens);
    if (usage.cache_read_tokens as f64) >= HIT_RATIO * reachable as f64 || lost < MISS_FLOOR {
        return;
    }
    if rebuilt {
        return;
    }
    let cause = if prefix.same == 0 {
        CacheBreakCause::SystemPrompt
    } else if prefix.tools_changed {
        CacheBreakCause::Tools {
            changes: prefix.tools_diff.clone().unwrap_or_default(),
        }
    } else if let Some((at, role)) = prefix.rewritten_at {
        CacheBreakCause::History {
            at,
            role: role.to_string(),
        }
    } else {
        CacheBreakCause::Provider
    };
    tracker
        .pending
        .entry(session.to_string())
        .or_default()
        .push(CacheBreak {
            session_id: session.to_string(),
            turn_id: turn.map(str::to_string),
            lost_tokens: lost,
            cause,
            idle_secs: now.duration_since(previous.at).as_secs(),
        });
}

/// 这条会话刚压缩过：下一次请求前缀从折叠处断开是预期的，不算断缓存。
pub fn note_cache_rebuild(session: &str) {
    if let Ok(mut tracker) = tracker().lock() {
        tracker.rebuilding.insert(session.to_string());
    }
}

/// 取走这条会话判出来、还没落库的断缓存。
pub fn take_cache_breaks(session: &str) -> Vec<CacheBreak> {
    tracker()
        .lock()
        .ok()
        .and_then(|mut tracker| tracker.pending.remove(session))
        .unwrap_or_default()
}

/// 会话没了就把它的记录丢掉，别让进程级的表一直长。
pub(crate) fn forget_session(session: &str) {
    if let Ok(mut tracker) = tracker().lock() {
        tracker.last.remove(session);
        tracker.rebuilding.remove(session);
        tracker.pending.remove(session);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(prompt: u64, read: u64) -> Usage {
        Usage {
            prompt_tokens: prompt,
            cache_read_tokens: read,
            cache_reported: true,
            ..Usage::default()
        }
    }

    /// 纯追加：上一次的 `previous` 条一条不差都还在。
    fn append(previous: usize) -> PrefixDiff {
        PrefixDiff {
            messages: previous + 2,
            previous: Some(previous),
            same: previous,
            ..PrefixDiff::default()
        }
    }

    /// 同一会话先发一次（冷启动，不判），再发第二次，返回判出来的。
    fn second(session: &str, prefix: &PrefixDiff, now: Usage) -> Vec<CacheBreak> {
        judge(
            "chat",
            Some(session),
            Some("t1"),
            &append(4),
            Some(&usage(100_000, 0)),
        );
        judge("chat", Some(session), Some("t2"), prefix, Some(&now));
        take_cache_breaks(session)
    }

    #[test]
    fn a_cold_first_request_and_small_gaps_are_not_breaks() {
        judge(
            "chat",
            Some("cb-cold"),
            None,
            &append(4),
            Some(&usage(100_000, 0)),
        );
        assert!(take_cache_breaks("cb-cold").is_empty());
        // 读到 95% 以上、或者缺口不到 2000：都不算。
        assert!(second("cb-hit", &append(6), usage(110_000, 96_000)).is_empty());
        assert!(second("cb-floor", &append(6), usage(3_000, 1_500)).is_empty());
    }

    #[test]
    fn a_pure_append_that_missed_is_on_the_provider() {
        let breaks = second("cb-provider", &append(6), usage(120_000, 10_240));
        assert_eq!(breaks.len(), 1);
        assert_eq!(breaks[0].cause, CacheBreakCause::Provider);
        assert_eq!(breaks[0].lost_tokens, 100_000 - 10_240);
        assert_eq!(breaks[0].turn_id.as_deref(), Some("t2"));
    }

    #[test]
    fn our_own_rewrites_are_named() {
        let system = PrefixDiff {
            same: 0,
            rewritten_at: Some((0, "system")),
            ..append(6)
        };
        let tools = PrefixDiff {
            tools_changed: true,
            tools_diff: Some("+send_qq_message,+qq_contacts".into()),
            ..append(6)
        };
        let history = PrefixDiff {
            same: 3,
            rewritten_at: Some((3, "tool")),
            ..append(6)
        };
        assert_eq!(
            second("cb-system", &system, usage(120_000, 0))[0].cause,
            CacheBreakCause::SystemPrompt
        );
        assert_eq!(
            second("cb-tools", &tools, usage(120_000, 13_000))[0].cause,
            CacheBreakCause::Tools {
                changes: "+send_qq_message,+qq_contacts".into()
            }
        );
        assert_eq!(
            second("cb-history", &history, usage(120_000, 30_000))[0].cause,
            CacheBreakCause::History {
                at: 3,
                role: "tool".into()
            }
        );
    }

    /// 压缩之后下一次请求从折叠处断开是预期重建；只豁免那一次。
    #[test]
    fn the_request_after_a_compaction_is_an_expected_rebuild() {
        judge(
            "chat",
            Some("cb-rebuild"),
            None,
            &append(4),
            Some(&usage(100_000, 0)),
        );
        note_cache_rebuild("cb-rebuild");
        let rewritten = PrefixDiff {
            same: 1,
            rewritten_at: Some((1, "user")),
            ..append(6)
        };
        judge(
            "chat",
            Some("cb-rebuild"),
            None,
            &rewritten,
            Some(&usage(40_000, 12_000)),
        );
        assert!(take_cache_breaks("cb-rebuild").is_empty());
        judge(
            "chat",
            Some("cb-rebuild"),
            None,
            &append(6),
            Some(&usage(45_000, 0)),
        );
        assert_eq!(take_cache_breaks("cb-rebuild").len(), 1);
    }

    #[test]
    fn side_requests_and_providers_without_cache_accounting_are_not_judged() {
        judge(
            "qq-judge",
            Some("cb-side"),
            None,
            &append(4),
            Some(&usage(100_000, 0)),
        );
        judge(
            "qq-judge",
            Some("cb-side"),
            None,
            &append(6),
            Some(&usage(120_000, 0)),
        );
        assert!(take_cache_breaks("cb-side").is_empty());
        let unreported = Usage {
            cache_reported: false,
            ..usage(120_000, 0)
        };
        judge("chat", Some("cb-agy"), None, &append(4), Some(&unreported));
        judge("chat", Some("cb-agy"), None, &append(6), Some(&unreported));
        assert!(take_cache_breaks("cb-agy").is_empty());
    }
}
