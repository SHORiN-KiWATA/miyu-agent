//! 正在跑的那一轮对工具面的限制(单轮覆盖项里的工具白名单与「不写记忆」)。
//!
//! 程序驱动 `miyu ask --tools / --no-tools / --no-memory` 给的这几样,原来只在回合
//! 装配里裁剪。中转线(claude-code / codebuddy / codex / agy)的 Miyu 工具只从 MCP
//! 桥拿,桥按会话另建工具面、看不到这一轮带了什么,于是模型照样拿到全部工具,
//! `--no-memory` 的回合还摆着 remember_fact(09-23 todolist)。
//!
//! 这里是回合与桥、中转线之间的那张登记表:回合装配时登记、回合结束随守卫撤掉;
//! 桥(`attach_owner_turn_tools`)和中转线(原生工具开关、续传档位)按会话来读。
//! 形状照 `live_turn` 的宿主工具位,但允许同一会话里几轮同时登记——桥只知道会话、
//! 不知道这次调用来自哪一轮,几轮叠在一起时按最严的算:限制是调用方要求的,
//! 宁可多收也不能漏过去。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

/// 一轮对工具面的限制。缺省 = 不限制。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TurnToolRestrictions {
    /// 工具白名单。`Some(空)` = 一个工具都不给(`--no-tools`)。
    pub allowlist: Option<Vec<String>>,
    /// 本轮不写长期记忆:摘掉 remember_fact(`--no-memory`)。
    pub no_memory_writes: bool,
}

impl TurnToolRestrictions {
    pub fn is_empty(&self) -> bool {
        self.allowlist.is_none() && !self.no_memory_writes
    }

    /// 工具要不要留下。
    pub fn allows(&self, name: &str) -> bool {
        if self.no_memory_writes && name == "remember_fact" {
            return false;
        }
        self.allowlist
            .as_ref()
            .is_none_or(|allow| allow.iter().any(|allowed| allowed == name))
    }

    /// 叠上另一轮的限制,取最严:白名单取交集,不写记忆取或。
    pub fn tighten(&mut self, other: &Self) {
        self.no_memory_writes |= other.no_memory_writes;
        self.allowlist = match (self.allowlist.take(), &other.allowlist) {
            (None, None) => None,
            (Some(mine), None) => Some(mine),
            (None, Some(theirs)) => Some(theirs.clone()),
            (Some(mine), Some(theirs)) => Some(
                mine.into_iter()
                    .filter(|name| theirs.contains(name))
                    .collect(),
            ),
        };
    }

    /// 进中转线续传键、agy 进程复用指纹的稳定签名:同一份限制恒出同一串,顺序与
    /// 重复无关。不限制时是空串——和落盘旧数据的缺省值一致,老映射照常续传。
    pub fn signature(&self) -> String {
        if self.is_empty() {
            return String::new();
        }
        let tools = match &self.allowlist {
            None => "*".to_string(),
            Some(allow) => {
                let mut names: Vec<&str> = allow.iter().map(String::as_str).collect();
                names.sort_unstable();
                names.dedup();
                names.join(",")
            }
        };
        let memory = if self.no_memory_writes { "off" } else { "on" };
        format!("tools={tools};memory={memory}")
    }
}

type Registry = HashMap<String, Vec<(u64, TurnToolRestrictions)>>;

fn registry() -> &'static Mutex<Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(Mutex::default)
}

/// 一轮的登记。活多久,这一轮的限制就在桥和中转线眼里生效多久。
pub struct LiveTurnToolRestrictionsGuard {
    session_id: String,
    id: Option<u64>,
}

impl LiveTurnToolRestrictionsGuard {
    /// 登记这一轮的限制。不限制、或者没有会话身份时什么都不记。
    pub fn register(session_id: &str, restrictions: TurnToolRestrictions) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        if session_id.is_empty() || restrictions.is_empty() {
            return Self {
                session_id: session_id.to_string(),
                id: None,
            };
        }
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        registry()
            .lock()
            .unwrap()
            .entry(session_id.to_string())
            .or_default()
            .push((id, restrictions));
        Self {
            session_id: session_id.to_string(),
            id: Some(id),
        }
    }
}

impl Drop for LiveTurnToolRestrictionsGuard {
    fn drop(&mut self) {
        let Some(id) = self.id else {
            return;
        };
        let mut registry = registry().lock().unwrap();
        if let Some(entries) = registry.get_mut(&self.session_id) {
            entries.retain(|(entry, _)| *entry != id);
            if entries.is_empty() {
                registry.remove(&self.session_id);
            }
        }
    }
}

/// 这个会话眼下所有在跑的回合的限制,叠成最严的一份。没有 = 不限制。
pub fn live_turn_tool_restrictions(session_id: &str) -> TurnToolRestrictions {
    let registry = registry().lock().unwrap();
    let mut merged = TurnToolRestrictions::default();
    let mut first = true;
    for (_, restrictions) in registry.get(session_id).into_iter().flatten() {
        if first {
            merged = restrictions.clone();
            first = false;
        } else {
            merged.tighten(restrictions);
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allow(names: &[&str]) -> Option<Vec<String>> {
        Some(names.iter().map(|name| name.to_string()).collect())
    }

    #[test]
    fn restrictions_live_exactly_as_long_as_their_turn() {
        let session = "restrictions-lifetime";
        assert!(live_turn_tool_restrictions(session).is_empty());
        let guard = LiveTurnToolRestrictionsGuard::register(
            session,
            TurnToolRestrictions {
                allowlist: allow(&["read"]),
                no_memory_writes: false,
            },
        );
        assert_eq!(
            live_turn_tool_restrictions(session).allowlist,
            allow(&["read"])
        );
        drop(guard);
        assert!(live_turn_tool_restrictions(session).is_empty());
    }

    /// 桥只知道会话:两轮叠在一起时按最严的算,先结束的那一轮不能把另一轮的限制带走。
    #[test]
    fn overlapping_turns_merge_to_the_strictest_and_leave_one_by_one() {
        let session = "restrictions-overlap";
        let first = LiveTurnToolRestrictionsGuard::register(
            session,
            TurnToolRestrictions {
                allowlist: allow(&["read", "web_fetch"]),
                no_memory_writes: false,
            },
        );
        let second = LiveTurnToolRestrictionsGuard::register(
            session,
            TurnToolRestrictions {
                allowlist: allow(&["read", "run_command"]),
                no_memory_writes: true,
            },
        );
        let merged = live_turn_tool_restrictions(session);
        assert_eq!(merged.allowlist, allow(&["read"]));
        assert!(merged.no_memory_writes);
        drop(second);
        let left = live_turn_tool_restrictions(session);
        assert_eq!(left.allowlist, allow(&["read", "web_fetch"]));
        assert!(!left.no_memory_writes);
        drop(first);
        assert!(live_turn_tool_restrictions(session).is_empty());
    }

    #[test]
    fn an_unrestricted_turn_registers_nothing() {
        let session = "restrictions-none";
        let _guard =
            LiveTurnToolRestrictionsGuard::register(session, TurnToolRestrictions::default());
        assert!(!registry().lock().unwrap().contains_key(session));
    }

    #[test]
    fn allows_honours_both_restrictions() {
        let no_memory = TurnToolRestrictions {
            allowlist: None,
            no_memory_writes: true,
        };
        assert!(!no_memory.allows("remember_fact"));
        assert!(no_memory.allows("run_command"));
        let only_read = TurnToolRestrictions {
            allowlist: allow(&["read", "remember_fact"]),
            no_memory_writes: true,
        };
        assert!(only_read.allows("read"));
        assert!(!only_read.allows("run_command"));
        assert!(
            !only_read.allows("remember_fact"),
            "白名单点了名也不行:这一轮不写记忆"
        );
        let nothing = TurnToolRestrictions {
            allowlist: Some(Vec::new()),
            no_memory_writes: false,
        };
        assert!(!nothing.allows("read"));
    }

    /// 签名进落盘的续传映射:同一份限制恒出同一串(顺序、重复无关),不限制是空串。
    #[test]
    fn the_signature_is_stable_and_empty_when_unrestricted() {
        assert_eq!(TurnToolRestrictions::default().signature(), "");
        let a = TurnToolRestrictions {
            allowlist: allow(&["web_fetch", "read", "read"]),
            no_memory_writes: false,
        };
        let b = TurnToolRestrictions {
            allowlist: allow(&["read", "web_fetch"]),
            no_memory_writes: false,
        };
        assert_eq!(a.signature(), b.signature());
        assert_eq!(a.signature(), "tools=read,web_fetch;memory=on");
        let none = TurnToolRestrictions {
            allowlist: Some(Vec::new()),
            no_memory_writes: false,
        };
        assert_eq!(none.signature(), "tools=;memory=on");
        let no_memory = TurnToolRestrictions {
            allowlist: None,
            no_memory_writes: true,
        };
        assert_eq!(no_memory.signature(), "tools=*;memory=off");
    }
}
