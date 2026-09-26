//! 事件广播与重放。
//!
//! 订阅者可能中途接入或短暂断线，所以事件带序号、可从任意位置重放。
// 兄弟模块的类型互相引用（DaemonState 持有 EventHub、run 记录引用
// ManagerState 等），统一从 mod.rs 的再导出取，免得每个文件维护一份
// 交叉导入清单。
use super::*;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

// ── EventHub 事件广播与重放 ──
#[derive(Clone, Debug)]
pub(crate) struct EventRecord {
    pub(crate) id: u64,
    pub(crate) kind: String,
    pub(crate) data: String,
    /// 发布的那一刻（Unix 毫秒），转发给跟着看的终端（`IpcFrame::Event::at_ms`）。
    pub(crate) at_ms: u64,
    /// 路由字段：发布时从正文里取出。订阅者按它挑自己的事件，不用为了看一眼是哪一轮、
    /// 哪个会话就把整条 JSON 解析一遍（09-25 前七处订阅者每条事件各解析一次）。
    pub(crate) run_id: Option<String>,
    pub(crate) session_id: Option<String>,
}

/// 一条事件在重放缓冲、广播、各订阅者手里是同一份（09-25 前每处各拷一份正文）。
pub(crate) type SharedEvent = Arc<EventRecord>;

/// 现在的 Unix 毫秒。
pub(crate) fn unix_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}

#[derive(Clone)]
pub(crate) struct EventHub {
    inner: Arc<Mutex<EventHubInner>>,
    sender: broadcast::Sender<SharedEvent>,
}

pub(crate) struct EventHubInner {
    pub(crate) next_id: u64,
    pub(crate) records: VecDeque<SharedEvent>,
    /// `records` 里 kind+data 的字节和。上限只按条数（4096）时，流式
    /// delta 夹杂大工具输出可以把重放缓冲顶到 10MB+；字节水位把它有界化，
    /// 淘汰方向与条数满时一致（丢最旧）。
    pub(crate) bytes: usize,
}

/// 重放缓冲的字节水位。达到后从头部淘汰——语义与「条数满丢最旧」相同，
/// 只是多一个触发条件。
pub(crate) const EVENT_BYTE_CAPACITY: usize = 4 * 1024 * 1024;

pub(crate) struct EventSubscription {
    pub(crate) pending: VecDeque<SharedEvent>,
    pub(crate) receiver: broadcast::Receiver<SharedEvent>,
}

impl EventHub {
    pub fn new() -> Self {
        let (sender, _) = broadcast::channel(EVENT_CAPACITY);
        Self {
            inner: Arc::new(Mutex::new(EventHubInner {
                next_id: 1,
                records: VecDeque::with_capacity(EVENT_CAPACITY),
                bytes: 0,
            })),
            sender,
        }
    }

    pub(crate) fn publish(&self, kind: impl Into<String>, data: Value) -> u64 {
        let mut inner = self.inner.lock().unwrap();
        let id = inner.next_id;
        inner.next_id = inner.next_id.saturating_add(1);
        let route = |key: &str| data.get(key).and_then(Value::as_str).map(str::to_string);
        let record = Arc::new(EventRecord {
            id,
            kind: kind.into(),
            data: serde_json::to_string(&data)
                .unwrap_or_else(|_| "{\"error\":\"event serialization failed\"}".to_string()),
            at_ms: unix_millis(),
            run_id: route("run_id"),
            session_id: route("session_id"),
        });
        let record_bytes = record.kind.len() + record.data.len();
        inner.bytes += record_bytes;
        while inner.records.len() == EVENT_CAPACITY
            || (inner.bytes > EVENT_BYTE_CAPACITY && !inner.records.is_empty())
        {
            if let Some(evicted) = inner.records.pop_front() {
                inner.bytes -= evicted.kind.len() + evicted.data.len();
            }
        }
        inner.records.push_back(record.clone());
        let _ = self.sender.send(record);
        id
    }

    pub(crate) fn latest_id(&self) -> u64 {
        self.inner.lock().unwrap().next_id.saturating_sub(1)
    }

    /// 只订阅**此后**的事件，不要重放。
    ///
    /// 给的是后台驱动器用的：它们只关心「刚刚发生了什么」，重放历史既没用
    /// 也会让它们对着早就结束的回合空转一遍。
    pub(crate) fn subscribe_live(&self) -> broadcast::Receiver<SharedEvent> {
        self.sender.subscribe()
    }

    pub(crate) fn subscribe_after(&self, after: u64) -> EventSubscription {
        let mut inner = self.inner.lock().unwrap();
        let receiver = self.sender.subscribe();
        let pending = replay_records(&mut inner, after);
        EventSubscription { pending, receiver }
    }

    pub(crate) fn replay_after(&self, after: u64) -> VecDeque<SharedEvent> {
        replay_records(&mut self.inner.lock().unwrap(), after)
    }
}

pub(crate) fn replay_records(inner: &mut EventHubInner, after: u64) -> VecDeque<SharedEvent> {
    if after > inner.next_id.saturating_sub(1) {
        return resync_record(inner);
    }
    let Some(oldest) = inner.records.front().map(|record| record.id) else {
        return VecDeque::new();
    };
    if after < oldest.saturating_sub(1) {
        return resync_record(inner);
    }
    inner
        .records
        .iter()
        .filter(|record| record.id > after)
        .cloned()
        .collect()
}

pub(crate) fn resync_record(inner: &mut EventHubInner) -> VecDeque<SharedEvent> {
    let id = inner.next_id;
    inner.next_id = inner.next_id.saturating_add(1);
    VecDeque::from([Arc::new(EventRecord {
        id,
        kind: "resync_required".to_string(),
        data: json!({ "latest_event_id": id }).to_string(),
        at_ms: unix_millis(),
        run_id: None,
        session_id: None,
    })])
}
