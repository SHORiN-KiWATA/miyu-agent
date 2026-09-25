//! 前台子代理的进度收成状态行要的三样：它这会儿在干什么、烧了多少词元、是哪条会话
//! （会话项目第 4 段之二）。
//!
//! 中继把子会话的事件翻成标记串（`protocol.rs`）。原来原样转进父回合的工具进度，终端和
//! 网页各自解析、在父会话里把子代理的过程再画一遍。子代理 09-18 起是一条会话，点它就切
//! 进去看全程，父会话这边只要这三样。标记协议留在引擎里：后台任务的日志桥照旧吃它。

use super::protocol::{tool_line_text, REASONING_DONE_MARKER};
use super::SUBAGENT_SESSION_MARKER;
use std::time::{Duration, Instant};

/// 窥视留多长。界面各自按宽度再截，这里只防一段长思考整段搬过去。
const PEEK_CHARS: usize = 240;

/// 只有窥视变了的话，隔多久才报一次：思考和正文是逐字来的。
const PEEK_EVERY: Duration = Duration::from_millis(200);

/// 状态行上那一个子代理的样子。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SubagentStatus {
    /// 它这会儿在干什么，一行。
    pub peek: String,
    /// 词元的显示串（`≈1.2K`），给状态行。
    pub tokens_label: String,
    /// 词元的数，给会话累计。
    pub tokens: u64,
    /// 它的子会话。
    pub session_id: Option<String>,
}

/// 一次前台子代理调用的进度：吃标记、按节流吐状态。
#[derive(Default)]
pub struct SubagentStatusFeed {
    status: SubagentStatus,
    /// 正在想 / 正在说的那一段，窥视取它的尾巴。
    thought: String,
    speech: String,
    /// 上一次报出去的样子、什么时候报的。
    reported: Option<(SubagentStatus, Instant)>,
}

/// 吃一条进度之后该怎么办。
#[derive(Debug, PartialEq, Eq)]
pub enum Absorbed {
    /// 不是子代理的标记：照旧当普通工具进度发。
    NotSubagent,
    /// 是，但这会儿不用报（没变，或者只有窥视变了、还在节流窗里）。
    Quiet,
    /// 报一次。`new_session`：这一条带来了子会话 id（调用方要把它记进这一步）。
    Report {
        status: SubagentStatus,
        new_session: bool,
    },
}

impl SubagentStatusFeed {
    /// 吃一条工具进度文字。
    pub fn absorb(&mut self, message: &str) -> Absorbed {
        self.absorb_at(message, Instant::now())
    }

    pub(crate) fn absorb_at(&mut self, message: &str, now: Instant) -> Absorbed {
        if !super::is_subagent_marker(message) {
            return Absorbed::NotSubagent;
        }
        let had_session = self.status.session_id.is_some();
        self.fold(message);
        let new_session = !had_session && self.status.session_id.is_some();
        let urgent = match &self.reported {
            None => true,
            Some((last, _)) => {
                last.session_id != self.status.session_id
                    || last.tokens_label != self.status.tokens_label
            }
        };
        let changed = self
            .reported
            .as_ref()
            .is_none_or(|(last, _)| *last != self.status);
        let due = self
            .reported
            .as_ref()
            .is_none_or(|(_, at)| now.saturating_duration_since(*at) >= PEEK_EVERY);
        if changed && (urgent || due) {
            self.reported = Some((self.status.clone(), now));
            return Absorbed::Report {
                status: self.status.clone(),
                new_session,
            };
        }
        Absorbed::Quiet
    }

    /// 节流窗里压着没报的最新样子（工具收尾、转轮那一拍时补报）。
    pub fn pending(&mut self) -> Option<SubagentStatus> {
        let stale = self
            .reported
            .as_ref()
            .is_some_and(|(last, _)| *last != self.status);
        if !stale {
            return None;
        }
        self.reported = Some((self.status.clone(), Instant::now()));
        Some(self.status.clone())
    }

    pub fn status(&self) -> &SubagentStatus {
        &self.status
    }

    fn fold(&mut self, message: &str) {
        if let Some(session) = message.strip_prefix(SUBAGENT_SESSION_MARKER) {
            let session = session.trim();
            if !session.is_empty() {
                self.status.session_id = Some(session.to_string());
            }
        } else if let Some(delta) = message.strip_prefix("__subagent_reasoning__") {
            if !delta.is_empty() {
                self.speech.clear();
                self.thought.push_str(delta);
                self.status.peek = tail(&self.thought);
            }
        } else if message.starts_with(REASONING_DONE_MARKER) {
            self.thought.clear();
        } else if let Some(delta) = message.strip_prefix("__subagent_content__") {
            if !delta.is_empty() {
                self.thought.clear();
                self.speech.push_str(delta);
                self.status.peek = tail(&self.speech);
            }
        } else if let Some(json) = message
            .strip_prefix("__subtool_call__")
            .or_else(|| message.strip_prefix("__subtool_result__"))
        {
            self.thought.clear();
            self.speech.clear();
            self.status.peek = tool_peek(json);
        } else if let Some(name) = message.strip_prefix("__subtool_preparing__") {
            self.thought.clear();
            self.speech.clear();
            self.status.peek = crate::tools::readable_tool_name(name.trim()).to_string();
        } else if let Some(metric) = message.strip_prefix("__subagent_metric__") {
            // `显示串\t数\t人话`（`subagent_host.rs` / 老 runner 的 `report_metric`）。
            let mut parts = metric.split('\t');
            let label = parts.next().unwrap_or_default().trim();
            if !label.is_empty() {
                self.status.tokens_label = label.to_string();
            }
            if let Some(total) = parts.next().and_then(|raw| raw.trim().parse().ok()) {
                self.status.tokens = total;
            }
        }
    }
}

/// 工具那一步压成一句：`运行命令 ok · 2.0s · 跑个命令`（去掉打头的工具 id）。
fn tool_peek(json: &str) -> String {
    let line = tool_line_text(json);
    let line = line
        .split_once('\t')
        .map_or(line.as_str(), |(_, rest)| rest);
    tail(line)
}

/// 一段字的最后一行，截到 `PEEK_CHARS` 个字（留尾巴：最新的在后面）。
fn tail(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())
        .unwrap_or_default();
    let count = line.chars().count();
    if count <= PEEK_CHARS {
        return line.to_string();
    }
    line.chars().skip(count - PEEK_CHARS).collect()
}
