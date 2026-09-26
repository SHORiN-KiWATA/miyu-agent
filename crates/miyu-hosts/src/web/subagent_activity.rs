//! 子代理会话此刻在干什么（09-26）：任务条每一行的窥视、词元、跑了多久。
//!
//! 原来这三样只从两处来：前台子代理经父回合的进度通道报给父会话的时间线，后台子代理报进
//! 镜像任务。停下之后在它会话里接着聊的那几轮、前台子代理，任务条上那一行就什么都没有
//! （用户 09-26：子代理的 token 计数没了）。这里不管前台后台、谁起的轮，只要是子代理会话在
//! 跑，就把它的事件翻成和中继同一套标记，喂进同一个 `SubagentStatusFeed`——窥视的说法和前台
//! 时间线上的一个样。

use crate::web::*;
use miyu_engine::tools::subagent::status::{SubagentStatus, SubagentStatusFeed};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// 子会话一轮里的事件翻成老循环那套 `__subagent_*` / `__subtool_*` 标记。父回合的进度通道
/// （`relay_child_events`）和这里的窥视都吃它。一个子会话一份：思考计时、工具计时、工具调用
/// 次数要跨事件记着。
#[derive(Default)]
pub(in crate::web) struct ChildMarkers {
    reasoning_since: Option<Instant>,
    tool_since: HashMap<String, Instant>,
    tool_calls: u64,
    /// 最近一次轮用量（会话累计）与是否估算：工具一起跑就报一次量，让面板抬头上的
    /// 「工具调用 N 次」当场涨，不等下一轮用量事件（老循环每步都报；TUI 走查 item06）。
    last_total: u64,
    last_estimated: bool,
}

impl ChildMarkers {
    /// 这一条事件要发的标记，按顺序（可能没有，可能几条）。
    pub(in crate::web) fn markers(&mut self, kind: &str, data: &Value) -> Vec<String> {
        let mut out = Vec::new();
        match kind {
            "reasoning.delta" => {
                let delta = field(data, "delta");
                if !delta.is_empty() {
                    self.reasoning_since.get_or_insert_with(Instant::now);
                    out.push(format!("__subagent_reasoning__{delta}"));
                }
            }
            "assistant.delta" => {
                let delta = field(data, "delta");
                if !delta.is_empty() {
                    out.extend(self.seal());
                    out.push(format!("__subagent_content__{delta}"));
                }
            }
            "tool.preparing" => {
                let name = field(data, "name");
                if miyu_engine::tools::preparing_phase(name).is_some() {
                    out.extend(self.seal());
                    out.push(format!("__subtool_preparing__{name}"));
                }
            }
            "tool.started" => {
                out.extend(self.seal());
                self.tool_since
                    .insert(field(data, "tool_id").to_string(), Instant::now());
                self.tool_calls += 1;
                out.push(format!(
                    "__subtool_call__{}",
                    json!({
                        "name": field(data, "name"),
                        "display": field(data, "display_name"),
                        "args": clip_detail(field(data, "arguments")),
                    })
                ));
                out.push(self.metric());
            }
            "tool.finished" => {
                let millis = self
                    .tool_since
                    .remove(field(data, "tool_id"))
                    .map(|since| since.elapsed().as_millis());
                out.push(format!(
                    "__subtool_result__{}",
                    json!({
                        "name": field(data, "name"),
                        "display": field(data, "display_name"),
                        "args": "",
                        "ok": data.get("ok").and_then(Value::as_bool).unwrap_or(false),
                        "ms": millis,
                        "output": clip_detail(field(data, "output")),
                    })
                ));
            }
            "chat.round_usage" => {
                // 会话累计（含这一轮之前的轮）：状态行上要的正是「这个子代理一共烧了多少」。
                self.last_total = data
                    .get("cumulative_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                self.last_estimated = data
                    .get("estimated")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                out.push(self.metric());
            }
            "run.completed" | "run.failed" | "run.cancelled" => out.extend(self.seal()),
            _ => {}
        }
        out
    }

    fn metric(&self) -> String {
        let tokens = miyu_engine::tools::subagent_runner::format_token_count(
            self.last_total,
            self.last_estimated,
        );
        let calls = self.tool_calls;
        let text = if miyu_base::i18n::is_zh() {
            format!("工具调用 {calls} 次　消耗词元 {tokens}")
        } else {
            format!("tool calls: {calls}　token cost: {tokens}")
        };
        format!("__subagent_metric__{tokens}\t{}\t{text}", self.last_total)
    }

    /// 一段思考收尾：报它想了多久。
    fn seal(&mut self) -> Option<String> {
        let started = self.reasoning_since.take()?;
        Some(format!(
            "{}{}",
            miyu_engine::tools::subagent::protocol::REASONING_DONE_MARKER,
            started.elapsed().as_millis()
        ))
    }
}

fn field<'a>(data: &'a Value, key: &str) -> &'a str {
    data.get(key).and_then(Value::as_str).unwrap_or_default()
}

/// 内层事件里单条输出最多带多少字节——与老循环的 `clip_detail` 同口径。
const MAX_DETAIL_BYTES: usize = 8 * 1024;

fn clip_detail(text: &str) -> String {
    if text.len() <= MAX_DETAIL_BYTES {
        return text.to_string();
    }
    let mut end = MAX_DETAIL_BYTES;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n…", &text[..end])
}

/// 一条子代理会话这会儿的样子。
struct Tracked {
    markers: ChildMarkers,
    feed: SubagentStatusFeed,
    /// 这一轮什么时候起的（unix 毫秒），任务条右边的用时从它算。
    running_since_ms: u64,
}

fn tracked() -> &'static Mutex<HashMap<String, Tracked>> {
    static MAP: std::sync::OnceLock<Mutex<HashMap<String, Tracked>>> = std::sync::OnceLock::new();
    MAP.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 这条子代理会话此刻在干什么、烧了多少、这一轮什么时候起的。没在跑就是 `None`。
pub(in crate::web) fn subagent_activity(session_id: &str) -> Option<(SubagentStatus, u64)> {
    let map = tracked().lock().unwrap();
    let entry = map.get(session_id)?;
    Some((entry.feed.status().clone(), entry.running_since_ms))
}

/// 跟着事件流记每条子代理会话在干什么：它一起轮就开始记，轮一结束就撤。
///
/// 只有 `run.started` 带会话号，增量和工具事件只带轮号：起轮时记下这一轮是哪条会话的。
pub(in crate::web) fn spawn_subagent_activity_tracker(state: DaemonState) {
    let mut receiver = state.events.subscribe_live();
    tokio::spawn(async move {
        let mut runs: HashMap<String, String> = HashMap::new();
        loop {
            let record = match receiver.recv().await {
                Ok(record) => record,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            };
            let Some(run_id) = record.run_id.clone() else {
                continue;
            };
            if record.kind == "run.started" {
                let Some(session_id) = record.session_id.clone() else {
                    continue;
                };
                if is_subagent_session(&state, &session_id) {
                    runs.insert(run_id, session_id.clone());
                    tracked().lock().unwrap().insert(
                        session_id,
                        Tracked {
                            markers: ChildMarkers::default(),
                            feed: SubagentStatusFeed::default(),
                            running_since_ms: now_ms(),
                        },
                    );
                }
                continue;
            }
            let Some(session_id) = runs.get(&run_id).cloned() else {
                continue;
            };
            if matches!(
                record.kind.as_str(),
                "run.completed" | "run.failed" | "run.cancelled"
            ) {
                runs.remove(&run_id);
                tracked().lock().unwrap().remove(&session_id);
                continue;
            }
            let Ok(data) = serde_json::from_str::<Value>(&record.data) else {
                continue;
            };
            let mut map = tracked().lock().unwrap();
            let Some(entry) = map.get_mut(&session_id) else {
                continue;
            };
            for marker in entry.markers.markers(&record.kind, &data) {
                entry.feed.absorb(&marker);
            }
        }
    });
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or_default()
}
