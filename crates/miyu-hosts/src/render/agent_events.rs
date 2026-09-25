//! 把回合事件画到流式渲染器上——[`AgentEvent`] → [`StreamRenderer`] 的唯一一张表。
//!
//! 进程内直连(`cli::repl::direct`)、IPC 解码后的终端回放(`cli::repl::remote`)、
//! daemon 往 shellhook 终端的后台回写(`web::actor::job_wake`)都用它。以前这张
//! 表长在 `cli` 里,daemon 回写只能反向 `use crate::cli`(09-16 搬来)。
//!
//! 只有 `AskQuestion` 不在这里消化——弹面板是入口自己的事(终端问题面板在
//! `cli`,daemon 回写根本没法在别人的提示符上弹),原样交回调用方(`Some`);
//! 其余事件都画完返回 `None`。

use super::StreamRenderer;
use anyhow::Result;
use miyu_base::i18n::text as t;
use miyu_engine::agent::AgentEvent;

pub fn apply_agent_event(
    renderer: &mut StreamRenderer,
    event: AgentEvent,
) -> Result<Option<AgentEvent>> {
    match event {
        AgentEvent::TurnStarted { .. } => Ok(()),
        AgentEvent::RawReasoning(_) => Ok(()),
        AgentEvent::FlushJournal => Ok(()),
        // 单次输出模式没有常驻 footer,逐请求计量快照无处可画。
        AgentEvent::RoundUsage { .. } => Ok(()),
        AgentEvent::Chunk(chunk) => {
            renderer.write_chunk(chunk)?;
            renderer.tick_spinner()
        }
        AgentEvent::ReasoningStart { received_at } => renderer.start_reasoning_phase(received_at),
        AgentEvent::ReasoningReset { received_at } => renderer.reset_reasoning_phase(received_at),
        AgentEvent::ReasoningPartStart { received_at } => {
            renderer.start_reasoning_part(received_at)
        }
        AgentEvent::ReasoningPartEnd { received_at } => renderer.finish_reasoning_part(received_at),
        AgentEvent::ReasoningTitle(title) => {
            renderer.write_reasoning_title(&title)?;
            renderer.tick_spinner()
        }
        AgentEvent::ToolCall {
            name, arguments, ..
        } => {
            renderer.write_tool_call(&name, &arguments)?;
            renderer.tick_spinner()
        }
        AgentEvent::ToolPreparing { name, batch } => {
            renderer.write_tool_preparing(&name, batch)?;
            renderer.tick_spinner()
        }
        AgentEvent::ToolResult {
            name, ok, output, ..
        } => {
            renderer.write_tool_result(&name, ok, &output)?;
            renderer.tick_spinner()
        }
        AgentEvent::ToolProgress { name, message, .. } => {
            renderer.write_tool_progress(&name, &message)?;
            renderer.tick_spinner()
        }
        AgentEvent::SubagentProgress { name, status, .. } => {
            renderer.write_subagent_status(&name, status);
            renderer.tick_spinner()
        }
        AgentEvent::CommandOutput {
            name,
            stream,
            chunk,
            ..
        } => {
            renderer.write_command_output(&name, stream, &chunk)?;
            renderer.tick_spinner()
        }
        AgentEvent::PrepareForExternalOutput { ready } => {
            renderer.prepare_for_external_output()?;
            let _ = ready.send(true);
            Ok(())
        }
        AgentEvent::Image { .. } | AgentEvent::Artifact { .. } => Ok(()),
        // 弹面板是入口自己的事(终端问题面板在 `cli`):原样交回。
        event @ AgentEvent::AskQuestion { .. } => return Ok(Some(event)),
        AgentEvent::QueuedPromptsConsumed { .. } => Ok(()),
        AgentEvent::GenerationSuperseded { .. } => Ok(()),
        AgentEvent::SpinnerTick => renderer.tick_spinner(),
        AgentEvent::CompactStart => {
            let text = t("Compacting context...", "正在压缩上下文...");
            renderer.write_system_message(text)?;
            // 转轮上也写着「正在压缩」,别让它还显示上一步的「正在思考」。
            renderer.set_custom_waiting_phase(Some(text.to_string()));
            renderer.tick_spinner()
        }
        AgentEvent::CompactChunk(chunk) => {
            renderer.write_compact_chunk(&chunk)?;
            renderer.tick_spinner()
        }
        AgentEvent::CompactEnd => {
            renderer.set_custom_waiting_phase(None);
            renderer.finish_compact()?;
            renderer.tick_spinner()
        }
        AgentEvent::PopStart => renderer.tick_spinner(),
        AgentEvent::PopEnd => renderer.tick_spinner(),
        AgentEvent::Notice { text } => {
            renderer.write_system_message(&text)?;
            renderer.tick_spinner()
        }
    }?;
    Ok(None)
}
