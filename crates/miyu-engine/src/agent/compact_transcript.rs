//! 隔离摘要路径喂给摘要器的对话文本，以及压缩切点用来估算体量的那份。
//!
//! fork 摘要直接复用活体前缀，模型看到的就是它当初看到的一切；隔离路径（fork 失败、
//! 关了缓存复用、溢出兜底）只能读这里拼出来的文本。09-25 前这份文本只有用户、助手、
//! 思考和 `tool_reports`——而 `tool_reports` 早就几乎为空，`tool_flow` 不在里面，
//! 开发会话在这条路径上压一次，命令输出和报错全丢（opencode 调研 3.2 第 2 条）。
//!
//! 防注入沿用 compact-plan Phase 3 的三条：工具输出、思考截到 2000 字；调用参数只留
//! 键名（子代理的 prompt 被摘要复述后，会以用户身份回到主会话）；只有真正的用户消息
//! 写成 `User:`，轮间注入的上下文块、goal 通知一律不写。

use crate::agent::tool_report::{chat_message_text, flow_is_interleaved, live_rounds};
use miyu_core::state::{FlowMessage, ToolFlowRound, Turn, TurnFollowup};

/// Per-item cap for tool output and reasoning fed to the summarizer. Long
/// tool payloads are the largest summary-input cost and a prompt-injection
/// vector when echoed back into the summary (sub-agent prompts re-entering
/// the session as history).
pub(in crate::agent) const SUMMARY_ITEM_MAX_CHARS: usize = 2000;

/// Head-truncate a summarizer input item at a char boundary, marking the cut.
pub(in crate::agent) fn truncate_for_summary(text: &str) -> std::borrow::Cow<'_, str> {
    if text.len() <= SUMMARY_ITEM_MAX_CHARS {
        return std::borrow::Cow::Borrowed(text);
    }
    let mut end = SUMMARY_ITEM_MAX_CHARS;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    std::borrow::Cow::Owned(format!(
        "{}\n[... {} more bytes truncated ...]",
        &text[..end],
        text.len() - end
    ))
}

/// 隔离摘要读的对话文本。一轮里的顺序：用户消息、澄清问答、插话与工具轮（按活体顺序）、
/// 最终回复。
pub(in crate::agent) fn turns_to_text(turns: &[&Turn]) -> String {
    let mut output = String::new();
    for (i, turn) in turns.iter().enumerate() {
        if turn.is_summary {
            continue;
        }
        output.push_str(&format!("--- Turn {} ---\n", i + 1));
        output.push_str("User: ");
        output.push_str(&turn.user_content);
        for exchange in &turn.question_exchanges {
            output.push_str("\nAssistant clarification: ");
            output.push_str(&miyu_base::question::assistant_exchange_text(exchange));
            output.push_str("\nUser clarification: ");
            output.push_str(&miyu_base::question::user_exchange_text(exchange));
        }
        push_rounds_and_followups(&mut output, turn);
        output.push_str("\nAssistant: ");
        output.push_str(&turn.assistant_content);
        if let Some(reasoning) = &turn.assistant_reasoning {
            push_reasoning(&mut output, reasoning);
        }
        for report in &turn.tool_reports {
            output.push_str("\n[Tool Report: ");
            output.push_str(&truncate_for_summary(report));
            output.push(']');
        }
        output.push('\n');
    }
    output
}

/// 工具轮连同插话。按活体顺序记了轮间消息的 flow（`interleaved`），插话就放回它当时
/// 的位置；老记录没有位置信息，插话照旧排在工具轮前面。
fn push_rounds_and_followups(output: &mut String, turn: &Turn) {
    let rounds = live_rounds(&turn.tool_flow);
    if !flow_is_interleaved(&turn.tool_flow) {
        for followup in &turn.followups {
            push_followup(output, followup);
        }
        for round in rounds {
            push_round(output, round);
        }
        return;
    }
    let mut placed = vec![false; turn.followups.len()];
    for round in rounds {
        push_flow_messages(output, &round.before, turn, &mut placed);
        push_round(output, round);
        push_flow_messages(output, &round.after, turn, &mut placed);
    }
    // flow 里没对上的插话（按理没有）不能丢，排在最终回复前面。
    for (followup, placed) in turn.followups.iter().zip(placed) {
        if !placed {
            push_followup(output, followup);
        }
    }
}

fn push_followup(output: &mut String, followup: &TurnFollowup) {
    if let Some(content) = &followup.preceding_assistant_content {
        if !content.trim().is_empty() {
            output.push_str("\nAssistant: ");
            output.push_str(content);
        }
    }
    if let Some(reasoning) = &followup.preceding_assistant_reasoning {
        push_reasoning(output, reasoning);
    }
    output.push_str("\nUser: ");
    output.push_str(&followup.content);
}

/// 轮间消息：插话写成 `User:`，插话前落进对话的正文写成 `Assistant:`。别的 user 消息是
/// 注入的上下文块（插话的瞬态尾巴、goal 通知），不是用户说的话，不写。认插话只认
/// 这一轮的排队消息：带图的只记了 id，纯文本的原样存着正文。
fn push_flow_messages(
    output: &mut String,
    entries: &[FlowMessage],
    turn: &Turn,
    placed: &mut [bool],
) {
    for entry in entries {
        match entry {
            FlowMessage::Followup { followup } => {
                if let Some(index) = turn
                    .followups
                    .iter()
                    .position(|candidate| &candidate.prompt_id == followup)
                {
                    placed[index] = true;
                    output.push_str("\nUser: ");
                    output.push_str(&turn.followups[index].content);
                }
            }
            FlowMessage::Message(message) => {
                let text = chat_message_text(message).unwrap_or_default();
                match message.role.as_str() {
                    "assistant" if !text.trim().is_empty() => {
                        output.push_str("\nAssistant: ");
                        output.push_str(&text);
                    }
                    "user" => {
                        let followup = (0..turn.followups.len())
                            .find(|&index| !placed[index] && turn.followups[index].content == text);
                        if let Some(index) = followup {
                            placed[index] = true;
                            output.push_str("\nUser: ");
                            output.push_str(&text);
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

/// 一轮工具调用：这一轮的正文与思考，每个调用的工具名、参数键名和截断后的输出。
fn push_round(output: &mut String, round: &ToolFlowRound) {
    if !round.assistant_content.trim().is_empty() {
        output.push_str("\nAssistant: ");
        output.push_str(&round.assistant_content);
    }
    if let Some(reasoning) = &round.assistant_reasoning {
        push_reasoning(output, reasoning);
    }
    for call in &round.calls {
        output.push_str("\n[Tool call: ");
        output.push_str(&call.name);
        output.push(' ');
        output.push_str(&argument_keys(&call.arguments));
        output.push_str("]\n[Tool result: ");
        output.push_str(&truncate_for_summary(&call.output));
        output.push(']');
    }
}

fn push_reasoning(output: &mut String, reasoning: &str) {
    if reasoning.trim().is_empty() {
        return;
    }
    output.push_str("\n[Reasoning: ");
    output.push_str(&truncate_for_summary(reasoning));
    output.push(']');
}

/// 调用参数降级成键名列表：`{content, path} (2 keys)`。
fn argument_keys(arguments: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(arguments) {
        Ok(serde_json::Value::Object(map)) => {
            let keys = map.keys().map(String::as_str).collect::<Vec<_>>();
            let unit = if keys.len() == 1 { "key" } else { "keys" };
            format!("{{{}}} ({} {unit})", keys.join(", "), keys.len())
        }
        _ => "(arguments omitted)".to_string(),
    }
}

/// 一轮的体量（压缩切点、分段按它估 token）。只拼正文不加标注，数的是模型实际要读的量。
pub(in crate::agent) fn turn_to_text(turn: &Turn) -> String {
    let mut output = String::new();
    output.push_str(&turn.user_content);
    for exchange in &turn.question_exchanges {
        output.push_str(&miyu_base::question::assistant_exchange_text(exchange));
        output.push_str(&miyu_base::question::user_exchange_text(exchange));
    }
    for followup in &turn.followups {
        if let Some(content) = &followup.preceding_assistant_content {
            output.push_str(content);
        }
        if let Some(reasoning) = &followup.preceding_assistant_reasoning {
            output.push_str(reasoning);
        }
        output.push_str(&followup.content);
    }
    output.push_str(&turn.assistant_content);
    if let Some(reasoning) = &turn.assistant_reasoning {
        output.push_str(reasoning);
    }
    // v20+ 工具密集回合的主体在 tool_flow 里(reports 多为空):漏计它,
    // 压缩预算会把"40 轮"当成保尾额度塞进 16K,压后必然仍超。
    for round in live_rounds(&turn.tool_flow) {
        output.push_str(&round.assistant_content);
        if let Some(reasoning) = &round.assistant_reasoning {
            output.push_str(reasoning);
        }
        for call in &round.calls {
            output.push_str(&call.name);
            output.push_str(&call.arguments);
            output.push_str(&call.output);
        }
    }
    for report in &turn.tool_reports {
        output.push_str(report);
    }
    output
}
