//! 前台子代理的进度：标记收成状态再发（会话项目第 4 段之二，见 `tools::subagent::status`）。
//!
//! 串行、并发两条执行路共用。子代理的标记不再原样转成工具进度：收成窥视、词元、子会话，
//! 按节流发 `AgentEvent::SubagentProgress`；子会话 id 一到就记进这一步（回合收尾落库时挂上
//! `child_session_id`）。别的工具进度照旧。

use crate::agent::*;
use crate::tools::subagent::status::{Absorbed, SubagentStatusFeed};
use std::collections::HashMap;

/// 这一批里每个前台子代理调用一份。
#[derive(Default)]
pub(super) struct SubagentFeeds(HashMap<String, SubagentStatusFeed>);

impl SubagentFeeds {
    /// 发一条工具进度。返回真 = 这一条带来了子会话 id：调用方落一次检查点，中途刷新的网页
    /// 才链得上那条会话。
    pub(super) fn forward<F>(
        &mut self,
        on_event: &mut F,
        call_id: &str,
        name: &str,
        progress: tools::ToolProgressEvent,
    ) -> Result<bool>
    where
        F: FnMut(AgentEvent) -> Result<()>,
    {
        let tools::ToolProgressEvent::Message(message) = &progress else {
            emit_tool_progress(on_event, call_id, name, progress)?;
            return Ok(false);
        };
        // 「交到后台了」那句不是子会话里的过程，是这一次调用自己的结论：照旧当工具进度发
        // （界面据它把这一步记成交出去的、不报秒数，挂上那句说明）。
        if !tools::is_subagent_marker(message)
            || message.starts_with(tools::subagent::protocol::DETACH_MARKER)
        {
            emit_tool_progress(on_event, call_id, name, progress)?;
            return Ok(false);
        }
        let feed = self.0.entry(call_id.to_string()).or_default();
        match feed.absorb(message) {
            Absorbed::NotSubagent | Absorbed::Quiet => Ok(false),
            Absorbed::Report {
                status,
                new_session,
            } => {
                if let Some(session) = status.session_id.as_deref().filter(|_| new_session) {
                    tools::record_subagent_session(call_id, session);
                }
                on_event(AgentEvent::SubagentProgress {
                    call_id: call_id.to_string(),
                    name: name.to_string(),
                    status,
                })?;
                Ok(new_session)
            }
        }
    }

    /// 节流窗里压着没报的最新样子补报（转轮那一拍、这次调用收尾时）。
    pub(super) fn flush<F>(&mut self, on_event: &mut F, call_id: &str, name: &str) -> Result<()>
    where
        F: FnMut(AgentEvent) -> Result<()>,
    {
        let Some(status) = self
            .0
            .get_mut(call_id)
            .and_then(SubagentStatusFeed::pending)
        else {
            return Ok(());
        };
        on_event(AgentEvent::SubagentProgress {
            call_id: call_id.to_string(),
            name: name.to_string(),
            status,
        })
    }
}
