//! 请求前缀指纹：断了缓存之后能说清「是谁掰的」。
//!
//! 命中率掉下来只有两种可能：**我们**把前缀改了（系统提示词、工具面、历史里
//! 某条消息被重写），或者**上游**把缓存丢了（逐出、过期、负载均衡打到冷节
//! 点）。`cache-usage.jsonl` 原来只记 prompt/cache_read 两个数，这两种情况
//! 长得一模一样——09-22 排查「mimo 命中率低」时就卡在这里：同一会话连续两次
//! 请求、间隔 16 秒、prompt 只涨了 534，第二次却 `cache_read=0`，光看数字没法
//! 判定该去修代码还是该去骂中转。
//!
//! 所以每次请求按**逐条消息的累积哈希**算一条链，和同一 (scope, session) 上
//! 一次请求的链比对，得出「前多少条相同」「第一条不同的是第几条、什么角色」。
//! 于是：
//!
//! | 现象 | 判定 |
//! |---|---|
//! | `cache_read=0` 且 `same=0` | 前缀开头就变了——我们的锅，去看系统提示词 |
//! | `cache_read=0` 但 `same` ≈ 上次条数 | 请求是纯追加的——上游丢了缓存 |
//! | `same` 卡在中间某条 | 那条消息被重写了，`at`/`role` 直接指出是哪条 |
//!
//! 只记哈希，不记任何正文。要看正文用 `request_log`（`MIYU_LOG_REQUESTS=1`），
//! 那个默认关着且体积以百 KB 计；这里是常开的、每请求几十字节的那一份。

use crate::llm::{ChatMessage, ToolDefinition};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// 一次请求的前缀指纹：逐条消息的累积哈希 + 工具表哈希。
///
/// `chain[i]` 是「前 i+1 条消息」的累积哈希，所以两条链的共同前缀长度就是
/// 共同的消息条数——正是供应商前缀缓存能命中的那一段。
pub(crate) struct PrefixChain {
    chain: Vec<u64>,
    tools: u64,
    roles: Vec<&'static str>,
}

/// 角色名收敛成 'static 串，免得指纹里为了记个 role 再拷一份 String。
fn role_tag(role: &str) -> &'static str {
    match role {
        "system" => "system",
        "user" => "user",
        "assistant" => "assistant",
        "tool" => "tool",
        "developer" => "developer",
        _ => "other",
    }
}

fn hash64(bytes: &[u8]) -> u64 {
    let digest = blake3::hash(bytes);
    u64::from_le_bytes(digest.as_bytes()[..8].try_into().expect("8 bytes"))
}

impl PrefixChain {
    /// 按**序列化后的**消息算：`skip_serializing` 的字段本来就不上线，不该
    /// 参与判定（`thinking_signature`、`tool_span_ms` 这些每轮都在变，算进去
    /// 会把「纯追加」误报成「前缀被改」）。
    pub(crate) fn of(messages: &[ChatMessage], tools: &[ToolDefinition]) -> Self {
        let mut chain = Vec::with_capacity(messages.len());
        let mut roles = Vec::with_capacity(messages.len());
        let mut running = blake3::Hasher::new();
        for message in messages {
            let encoded = serde_json::to_vec(message).unwrap_or_default();
            running.update(&encoded);
            chain.push(hash64(running.finalize().as_bytes()));
            roles.push(role_tag(&message.role));
        }
        let tools = serde_json::to_vec(tools)
            .map(|encoded| hash64(&encoded))
            .unwrap_or(0);
        Self {
            chain,
            tools,
            roles,
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.chain.len()
    }
}

/// 与同一 (scope, session) 上一次请求比出来的差异。
#[derive(Clone, Copy)]
pub(crate) struct PrefixDiff {
    /// 本次请求的消息条数。
    pub(crate) messages: usize,
    /// 上一次请求的消息条数；没有上一次时为 `None`。
    pub(crate) previous: Option<usize>,
    /// 与上一次相同的前缀消息条数。
    pub(crate) same: usize,
    /// 第一条不同的消息下标（`same`），仅在它落在上一次请求范围内时有意义
    /// ——纯追加的话第一条“不同”的其实就是新消息，不算改写。
    pub(crate) rewritten_at: Option<(usize, &'static str)>,
    /// 工具表变了没有：工具面逐轮换脸同样会把整段前缀掰掉。
    pub(crate) tools_changed: bool,
}

impl PrefixDiff {
    /// 纯追加 = 上一次的每一条都还在、且一字未改。这一类请求如果还
    /// `cache_read=0`，责任就不在我们这边。
    ///
    /// 判定本身在读日志的那一端做（`testkit/cache-forensics/report.py`），
    /// 这里留着是给测试钉住语义用的。
    #[cfg(test)]
    pub(crate) fn append_only(&self) -> bool {
        match self.previous {
            Some(previous) => self.same >= previous && !self.tools_changed,
            None => false,
        }
    }
}

struct LastChain {
    chain: Vec<u64>,
    tools: u64,
}

static LAST: OnceLock<Mutex<HashMap<String, LastChain>>> = OnceLock::new();

/// 比对并记下本次链，返回与上一次的差异。
///
/// 按 (scope, session) 分桶：判官、压缩这些旁路各有各的前缀，混在一起比会
/// 把每一次都判成「全变了」。
pub(crate) fn compare_and_store(
    scope: &str,
    session: Option<&str>,
    chain: &PrefixChain,
) -> PrefixDiff {
    let key = format!("{scope}|{}", session.unwrap_or("-"));
    let mutex = LAST.get_or_init(|| Mutex::new(HashMap::new()));
    let Ok(mut last) = mutex.lock() else {
        return PrefixDiff {
            messages: chain.len(),
            previous: None,
            same: 0,
            rewritten_at: None,
            tools_changed: false,
        };
    };
    let diff = match last.get(&key) {
        Some(previous) => {
            let same = chain
                .chain
                .iter()
                .zip(previous.chain.iter())
                .take_while(|(now, before)| now == before)
                .count();
            // 落在上一次范围内才叫「改写」；等于上一次条数是纯追加。
            let rewritten_at = (same < previous.chain.len())
                .then(|| (same, chain.roles.get(same).copied().unwrap_or("none")));
            PrefixDiff {
                messages: chain.len(),
                previous: Some(previous.chain.len()),
                same,
                rewritten_at,
                tools_changed: previous.tools != chain.tools,
            }
        }
        None => PrefixDiff {
            messages: chain.len(),
            previous: None,
            same: 0,
            rewritten_at: None,
            tools_changed: false,
        },
    };
    last.insert(
        key,
        LastChain {
            chain: chain.chain.clone(),
            tools: chain.tools,
        },
    );
    diff
}

/// 会话没了就把它的链丢掉，别让进程级的表一直长。
pub(crate) fn forget_session(session: &str) {
    let Some(mutex) = LAST.get() else { return };
    let Ok(mut last) = mutex.lock() else { return };
    last.retain(|key, _| !key.ends_with(&format!("|{session}")));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::FunctionDefinition;

    fn message(role: &str, text: &str) -> ChatMessage {
        match role {
            "system" => ChatMessage::system(text),
            "assistant" => ChatMessage::assistant(text, None),
            _ => ChatMessage::plain(role, text),
        }
    }

    fn tool(name: &str) -> ToolDefinition {
        ToolDefinition {
            kind: "function",
            function: FunctionDefinition {
                name: name.to_string(),
                description: String::new(),
                parameters: serde_json::json!({}),
            },
        }
    }

    #[test]
    fn append_only_requests_report_the_whole_previous_prefix_as_shared() {
        let tools = Vec::new();
        let first = vec![message("system", "s"), message("user", "a")];
        let second = vec![
            message("system", "s"),
            message("user", "a"),
            message("assistant", "b"),
        ];
        let scope = "t-append";
        compare_and_store(scope, Some("s1"), &PrefixChain::of(&first, &tools));
        let diff = compare_and_store(scope, Some("s1"), &PrefixChain::of(&second, &tools));
        assert_eq!(diff.same, 2);
        assert_eq!(diff.previous, Some(2));
        assert!(diff.rewritten_at.is_none());
        assert!(diff.append_only());
    }

    #[test]
    fn a_rewritten_system_prompt_points_at_the_first_changed_message() {
        let tools = Vec::new();
        let first = vec![message("system", "s"), message("user", "a")];
        let second = vec![message("system", "s-changed"), message("user", "a")];
        let scope = "t-rewrite";
        compare_and_store(scope, Some("s1"), &PrefixChain::of(&first, &tools));
        let diff = compare_and_store(scope, Some("s1"), &PrefixChain::of(&second, &tools));
        assert_eq!(diff.same, 0);
        assert_eq!(diff.rewritten_at, Some((0, "system")));
        assert!(!diff.append_only());
    }

    #[test]
    fn a_changed_tool_table_is_reported_even_when_messages_match() {
        let first = vec![message("system", "s")];
        let scope = "t-tools";
        compare_and_store(scope, Some("s1"), &PrefixChain::of(&first, &[]));
        let tools = vec![tool("read")];
        let diff = compare_and_store(scope, Some("s1"), &PrefixChain::of(&first, &tools));
        assert_eq!(diff.same, 1);
        assert!(diff.tools_changed);
        assert!(!diff.append_only());
    }

    #[test]
    fn separate_sessions_do_not_contaminate_each_others_chains() {
        let tools = Vec::new();
        let scope = "t-sessions";
        let a = vec![message("system", "a")];
        let b = vec![message("system", "b")];
        compare_and_store(scope, Some("sa"), &PrefixChain::of(&a, &tools));
        compare_and_store(scope, Some("sb"), &PrefixChain::of(&b, &tools));
        let diff = compare_and_store(scope, Some("sa"), &PrefixChain::of(&a, &tools));
        assert_eq!(diff.same, 1);
        assert!(diff.append_only());
    }
}
