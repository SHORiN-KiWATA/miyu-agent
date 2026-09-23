//! 哪些会话此刻有窗口开着（09-23，跨会话消息的名单用）。
//!
//! 按「窗口」记：每个终端进程、每个网页标签各一个编号，记它此刻在看哪条会话、
//! 什么时候过期。一个窗口从 A 切到 B，A 自然就不算开着了。终端顺着它本来 1～3 秒
//! 一次的任务轮询报；网页走心跳——浏览器会把后台标签页的定时器放慢到一分钟一次，
//! 所以网页的过期时间放宽。关页面时网页主动注销，终端退出靠过期。

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub(crate) const TERMINAL_PRESENCE_TTL: Duration = Duration::from_secs(15);
pub(crate) const WEB_PRESENCE_TTL: Duration = Duration::from_secs(150);
/// 编号是客户端自己起的，防一个客户端乱报把表撑大。
const MAX_VIEWERS: usize = 4096;

#[derive(Clone, Default)]
pub(crate) struct Presence(Arc<Mutex<HashMap<String, Viewing>>>);

struct Viewing {
    session_id: String,
    expires: Instant,
}

impl Presence {
    /// 窗口 `viewer` 正在看 `session_id`；`None` 或空 = 这个窗口关了。
    pub(crate) fn report(&self, viewer: &str, session_id: Option<&str>, ttl: Duration) {
        if viewer.is_empty() {
            return;
        }
        let now = Instant::now();
        let mut viewers = self.0.lock().unwrap();
        match session_id.filter(|session| !session.is_empty()) {
            Some(session) => {
                if viewers.len() >= MAX_VIEWERS && !viewers.contains_key(viewer) {
                    viewers.retain(|_, viewing| viewing.expires > now);
                    if viewers.len() >= MAX_VIEWERS {
                        return;
                    }
                }
                viewers.insert(
                    viewer.to_string(),
                    Viewing {
                        session_id: session.to_string(),
                        expires: now + ttl,
                    },
                );
            }
            None => {
                viewers.remove(viewer);
            }
        }
    }

    /// 此刻有窗口开着的会话；顺手清掉过期的。
    pub(crate) fn open_sessions(&self) -> HashSet<String> {
        let now = Instant::now();
        let mut viewers = self.0.lock().unwrap();
        viewers.retain(|_, viewing| viewing.expires > now);
        viewers
            .values()
            .map(|viewing| viewing.session_id.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_counts_until_it_moves_leaves_or_expires() {
        let presence = Presence::default();
        presence.report("tui-1", Some("a"), TERMINAL_PRESENCE_TTL);
        presence.report("tab-1", Some("b"), WEB_PRESENCE_TTL);
        assert_eq!(
            presence.open_sessions(),
            HashSet::from(["a".into(), "b".into()])
        );
        // 同一个窗口切到别的会话,原来那条就不算开着了。
        presence.report("tui-1", Some("c"), TERMINAL_PRESENCE_TTL);
        assert_eq!(
            presence.open_sessions(),
            HashSet::from(["b".into(), "c".into()])
        );
        presence.report("tab-1", None, WEB_PRESENCE_TTL);
        assert_eq!(presence.open_sessions(), HashSet::from(["c".into()]));
        presence.report("tui-1", Some("c"), Duration::ZERO);
        assert!(presence.open_sessions().is_empty(), "过期就不算开着");
    }
}
