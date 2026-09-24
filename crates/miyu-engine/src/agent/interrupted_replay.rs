//! 被打断的回合怎么回放。
//!
//! 被打断的回合没有正式回复,库里只有到最后一个检查点为止的 tool_flow,以及一路
//! 落盘的流水账。回放分两段:
//!
//! - 已经发出去的:最后一个检查点之前的工具轮与轮间消息,照完成轮的样子原样放回
//!   (`push_tool_flow`)。这些字节上一条请求刚发过,上游有缓存。
//! - 没收尾的:检查点之后、打断之前的那一截(在飞的正文、调到一半的工具)从流水账
//!   补上,最后接恢复提示。
//!
//! 09-24 之前恢复提示放在这一轮最前面、整轮从流水账拼,下一轮的前缀于是在被打断
//! 那一轮的起点就断——dev 会话里约四分之一的回合被打断过,生产上单次丢过 7.9 万
//! token。老记录(flow 没按活体顺序记)与重做分支仍走老路子,字节不变。

use crate::agent::*;

/// 老位置(这一轮最前面)的恢复提示。老记录照旧回放它,字节不能动。
const RECOVERY_NOTE_BEFORE: &str = "<interrupted-turn-recovery>The previous reply was interrupted. Below is the model output and tool progress that had already been persisted before the interruption; do not re-run tools that already completed — continue handling the current user request from this content.</interrupted-turn-recovery>";

/// 新位置(这一轮末尾)的恢复提示。
const RECOVERY_NOTE_AFTER: &str = "<interrupted-turn-recovery>The reply above was interrupted. It shows the output and tool progress saved before the cut. Do not re-run tools that already finished. Continue the current request from there.</interrupted-turn-recovery>";

pub(in crate::agent) fn interrupted_turn_replay_messages(
    agent: &Agent,
    turn: &miyu_core::state::Turn,
) -> Vec<ChatMessage> {
    let legacy =
        turn.revision > 0 || (!turn.tool_flow.is_empty() && !flow_is_interleaved(&turn.tool_flow));
    if legacy {
        return legacy_interrupted_replay(agent, turn);
    }
    let mut messages = Vec::new();
    agent.push_tool_flow(&mut messages, turn);
    let unfinished = journal_after_flow(&turn.journal_events, &turn.tool_flow);
    let in_flow = followups_in_flow(turn);
    replay_journal_events(agent, turn, unfinished, &in_flow, &mut messages);
    messages.push(ChatMessage::turn_context(RECOVERY_NOTE_AFTER));
    messages
}

/// 已经随 flow 放回原位的插话(按排队 id 记的,或原样存的那条文本)。检查点在
/// 每次并入插话之后都会落,正常都在这里面;不在的(检查点没来得及落就被打断)
/// 照老办法从流水账补上,不丢。
fn followups_in_flow(turn: &miyu_core::state::Turn) -> HashSet<String> {
    let entries = live_rounds(&turn.tool_flow)
        .into_iter()
        .flat_map(|round| round.before.iter().chain(round.after.iter()))
        .collect::<Vec<_>>();
    turn.followups
        .iter()
        .filter(|followup| {
            entries.iter().any(|entry| match entry {
                miyu_core::state::FlowMessage::Followup { followup: id } => {
                    *id == followup.prompt_id
                }
                miyu_core::state::FlowMessage::Message(message) => {
                    message.role == "user"
                        && matches!(
                            message.content.as_ref(),
                            Some(ChatContent::Text(text)) if *text == followup.content
                        )
                }
            })
        })
        .map(|followup| followup.prompt_id.clone())
        .collect()
}

/// 流水账里检查点还没收进 flow 的那一截:以 flow 里最后出现的调用为界,它之后的
/// 事件就是在飞的正文与调到一半的工具。flow 为空时整本流水账都是。
fn journal_after_flow<'a>(
    events: &'a [miyu_core::state::TurnJournalEvent],
    flow: &[miyu_core::state::ToolFlowRound],
) -> &'a [miyu_core::state::TurnJournalEvent] {
    let calls = live_rounds(flow)
        .into_iter()
        .flat_map(|round| round.calls.iter().map(|call| call.id.as_str()))
        .collect::<HashSet<_>>();
    if calls.is_empty() {
        return events;
    }
    let last = events.iter().rposition(|event| {
        event
            .call_id
            .as_deref()
            .is_some_and(|call_id| calls.contains(call_id))
    });
    // 流水账里找不到 flow 的调用说明两边对不上:以 flow 为准,不再从流水账补,
    // 免得同一轮工具被放两遍。
    match last {
        Some(index) => &events[index + 1..],
        None => &events[events.len()..],
    }
}

/// 09-24 之前的回放:恢复提示在前,重做分支补上已提交的提问/插话前缀,整轮从
/// 流水账拼。
fn legacy_interrupted_replay(agent: &Agent, turn: &miyu_core::state::Turn) -> Vec<ChatMessage> {
    let mut messages = vec![ChatMessage::turn_context(RECOVERY_NOTE_BEFORE)];

    // A redo revision only journals the new branch. Preserve the already
    // committed clarification/follow-up prefix from the turn row before
    // replaying the new branch's events.
    let replayed_prompt_ids = turn
        .journal_events
        .iter()
        .filter(|event| event.kind == "queued_prompts_consumed")
        .flat_map(|event| {
            event
                .text_payload
                .as_deref()
                .and_then(|payload| serde_json::from_str::<Vec<String>>(payload).ok())
                .unwrap_or_default()
        })
        .collect::<HashSet<_>>();
    if turn.revision > 0 {
        let prefix_question_count = turn
            .journal_events
            .iter()
            .find(|event| event.kind == "redo_prefix_question_count")
            .and_then(|event| event.text_payload.as_deref())
            .and_then(|count| count.parse::<usize>().ok())
            .unwrap_or_else(|| {
                let branch_answers = turn
                    .journal_events
                    .iter()
                    .filter(|event| {
                        event.kind == "tool_result"
                            && event.name.as_deref() == Some("ask_question")
                            && event
                                .text_payload
                                .as_deref()
                                .and_then(|payload| serde_json::from_str::<Value>(payload).ok())
                                .and_then(|payload| {
                                    payload
                                        .get("status")
                                        .and_then(Value::as_str)
                                        .map(|status| status == "answered")
                                })
                                .unwrap_or(false)
                    })
                    .count();
                turn.question_exchanges.len().saturating_sub(branch_answers)
            });
        for exchange in turn.question_exchanges.iter().take(prefix_question_count) {
            messages.push(ChatMessage::plain(
                "assistant",
                miyu_base::question::assistant_exchange_text(exchange),
            ));
            messages.push(ChatMessage::plain(
                "user",
                miyu_base::question::user_exchange_text(exchange),
            ));
        }
        for followup in &turn.followups {
            if replayed_prompt_ids.contains(&followup.prompt_id) {
                continue;
            }
            push_assistant_context_messages(
                &mut messages,
                followup
                    .preceding_assistant_content
                    .as_deref()
                    .unwrap_or_default(),
                followup.preceding_assistant_reasoning.as_deref(),
                false,
            );
            messages.push(agent.followup_user_message(followup));
            messages.extend(followup.context_messages().iter().map(replay_fossil));
        }
    }

    replay_journal_events(
        agent,
        turn,
        &turn.journal_events,
        &HashSet::new(),
        &mut messages,
    );
    messages
}

/// 把一段流水账还原成对话消息:正文与思考攒到下一个工具结果处落成 assistant,
/// 没等到结果的调用标成被打断。`in_flow` 里的插话已经随 flow 回放,这里不再重复。
fn replay_journal_events(
    agent: &Agent,
    turn: &miyu_core::state::Turn,
    events: &[miyu_core::state::TurnJournalEvent],
    in_flow: &HashSet<String>,
    messages: &mut Vec<ChatMessage>,
) {
    let mut assistant_text = String::new();
    let mut assistant_reasoning = String::new();
    let mut pending_calls = Vec::<ToolCall>::new();
    let mut open_calls = Vec::<ToolCall>::new();
    let mut progress = HashMap::<String, String>::new();
    let mut command_tail = HashMap::<String, Vec<u8>>::new();

    for event in events {
        match event.kind.as_str() {
            "assistant_content" => {
                if let Some(text) = &event.text_payload {
                    assistant_text.push_str(text);
                }
            }
            "assistant_reasoning" => {
                if let Some(text) = &event.text_payload {
                    assistant_reasoning.push_str(text);
                }
            }
            "reasoning_reset" => assistant_reasoning.clear(),
            "tool_call" => {
                let Some(call_id) = event.call_id.clone() else {
                    continue;
                };
                let Some(name) = event.name.as_deref() else {
                    continue;
                };
                pending_calls.push(ToolCall {
                    id: call_id,
                    kind: "function".to_string(),
                    function: ToolCallFunction {
                        name: replay_tool_function_name(name),
                        arguments: event.text_payload.clone().unwrap_or_default(),
                    },
                });
            }
            "tool_result" => {
                open_calls.extend(flush_interrupted_assistant(
                    messages,
                    &mut assistant_reasoning,
                    &mut assistant_text,
                    &mut pending_calls,
                ));
                if let Some(call_id) = &event.call_id {
                    // 空的 tool 结果同样不可回放:多数上游把它当协议错误。
                    let output = event
                        .text_payload
                        .as_deref()
                        .map(str::trim)
                        .filter(|text| !text.is_empty())
                        .unwrap_or("(no output)");
                    messages.push(ChatMessage::tool(call_id, truncate_chars(output, 48_000)));
                    open_calls.retain(|call| call.id != *call_id);
                    progress.remove(call_id);
                    command_tail.remove(call_id);
                }
            }
            "tool_progress" => {
                if let Some(call_id) = &event.call_id {
                    progress.insert(
                        call_id.clone(),
                        truncate_chars(event.text_payload.as_deref().unwrap_or_default(), 4_000),
                    );
                }
            }
            "command_stdout" | "command_stderr" => {
                if let Some(call_id) = &event.call_id {
                    let tail = command_tail.entry(call_id.clone()).or_default();
                    if let Some(bytes) = &event.blob_payload {
                        tail.extend_from_slice(bytes);
                        const MAX_COMMAND_TAIL: usize = 8 * 1024;
                        if tail.len() > MAX_COMMAND_TAIL {
                            let start = tail.len() - MAX_COMMAND_TAIL;
                            tail.drain(..start);
                        }
                    }
                }
            }
            "queued_prompts_consumed" => {
                open_calls.extend(flush_interrupted_assistant(
                    messages,
                    &mut assistant_reasoning,
                    &mut assistant_text,
                    &mut pending_calls,
                ));
                append_interrupted_tool_results(
                    messages,
                    &mut open_calls,
                    &mut progress,
                    &mut command_tail,
                );
                let prompt_ids = event
                    .text_payload
                    .as_deref()
                    .and_then(|payload| serde_json::from_str::<Vec<String>>(payload).ok())
                    .unwrap_or_default();
                // 已经随 flow 放回原位的插话不再补一遍。
                let pending = prompt_ids
                    .into_iter()
                    .filter(|prompt_id| !in_flow.contains(prompt_id))
                    .collect::<Vec<_>>();
                if pending.is_empty() {
                    continue;
                }
                for prompt_id in pending {
                    if let Some(followup) = turn
                        .followups
                        .iter()
                        .find(|followup| followup.prompt_id == prompt_id)
                    {
                        messages.push(agent.followup_user_message(followup));
                    }
                }
            }
            _ => {}
        }
    }

    open_calls.extend(flush_interrupted_assistant(
        messages,
        &mut assistant_reasoning,
        &mut assistant_text,
        &mut pending_calls,
    ));
    append_interrupted_tool_results(messages, &mut open_calls, &mut progress, &mut command_tail);
}

pub(in crate::agent) fn flush_interrupted_assistant(
    messages: &mut Vec<ChatMessage>,
    assistant_reasoning: &mut String,
    assistant_text: &mut String,
    pending_calls: &mut Vec<ToolCall>,
) -> Vec<ToolCall> {
    if assistant_reasoning.trim().is_empty()
        && assistant_text.trim().is_empty()
        && pending_calls.is_empty()
    {
        return Vec::new();
    }
    if !assistant_reasoning.trim().is_empty() {
        if let Some(reasoning) = private_reasoning_memory(assistant_reasoning) {
            messages.push(ChatMessage::turn_context(reasoning));
        }
    }
    assistant_reasoning.clear();
    let text = std::mem::take(assistant_text);
    let calls = std::mem::take(pending_calls);
    let replay_calls = (!calls.is_empty()).then(|| calls.clone());
    messages.push(ChatMessage::assistant(text, replay_calls));
    calls
}

pub(in crate::agent) fn append_interrupted_tool_results(
    messages: &mut Vec<ChatMessage>,
    open_calls: &mut Vec<ToolCall>,
    progress: &mut HashMap<String, String>,
    command_tail: &mut HashMap<String, Vec<u8>>,
) {
    for call in std::mem::take(open_calls) {
        let mut output =
            "tool execution was interrupted before a final result was persisted".to_string();
        if let Some(message) = progress.remove(&call.id) {
            output.push_str("\nlast progress: ");
            output.push_str(&message);
        }
        if let Some(bytes) = command_tail.remove(&call.id) {
            let tail = String::from_utf8_lossy(&bytes);
            if !tail.trim().is_empty() {
                output.push_str("\nlast command output:\n");
                output.push_str(&truncate_chars(&tail, 8_000));
            }
        }
        messages.push(ChatMessage::tool(call.id, output));
    }
}

pub(in crate::agent) fn replay_tool_function_name(name: &str) -> String {
    match name.split_once(':').map(|(prefix, _)| prefix) {
        Some("load_skill") | Some("load_tools") | Some("subagent") | Some("task") => {
            name.split(':').next().unwrap_or(name).to_string()
        }
        _ => name.to_string(),
    }
}
