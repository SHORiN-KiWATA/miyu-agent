//! IPC 事件解码:把 daemon 发来的 `(kind, data)` 还原成 [`AgentEvent`]。
//!
//! 编码侧的权威表是 `web/event_map.rs` 的 `RunEventMapper::handle`(唯一穷尽
//! 全部变体的那张),这里是它的逆。以前解码这件事在 `cli/repl/remote/one_shot.rs`
//! 和 `cli/repl/wake.rs` 各写了一份,谁漏抄一个变体,那条路径就少一个功能:
//!
//! - wake 收不到 `question.requested`,唤醒里模型提问会挂;
//! - wake 收不到 `context.compact_*` / `context.pop_*` / `context.notice`;
//! - 两边都丢 `tool.artifact`;
//! - `chat.round_usage` 在 one_shot 里没有分支,逐请求计量整轮掉地上。
//!
//! 收敛成一份之后,加 IPC 事件只要改这里 + 编码侧两处。
//!
//! 住在 `runtime` 而不是 `cli`(09-16):daemon 往 shellhook 终端回写后台任务
//! 跟进(`web::actor::job_wake`)也要解这张表,以前只能反向 `use crate::cli`。
//! 事件记录本身(`EventRecord` 的 kind/data)就是这一层的东西,解码放在同一家。
//!
//! **纯函数**:不碰终端、不发 IPC、不 await。需要异步副作用的两个事件
//! (图片要从库里取资产再画、问题要弹面板再回发命令)只返回一个标记,
//! 由调用方拿着原始 `data` 自己做——那部分状态各路径不同,强行收敛只会
//! 把 `Option<&mut ...>` 的分叉搬个地方。

use miyu_core::llm::{ChatStreamChunk, ChatStreamKind, GenerationSpeed, TurnTokens, Usage};
use miyu_engine::agent::{AgentEvent, PersonaLane};
use miyu_engine::tools::CommandOutputStream;
use serde_json::Value;
use std::time::Instant;

/// 一条 IPC 事件解码后的去向。
pub enum DecodedIpc {
    /// 可以直接喂给渲染器 / UI 的事件。
    Event(AgentEvent),
    /// 需要调用方做异步副作用，原始 `data` 仍在调用方手里。
    #[allow(dead_code)] // IPC 解码 DTO 的载荷,cli 侧按需读
    Async(AsyncIpc),
    /// 队列里某条排队消息被 daemon 丢弃（从未进入对话）。没有对应的
    /// `AgentEvent` 变体——它不是回合里发生的事，是队列状态变更。
    #[allow(dead_code)] // IPC 解码 DTO 的载荷,cli 侧按需读
    QueueRemoved(Vec<String>),
    /// 回合跑完。这是控制流而不是 `AgentEvent`：它终止事件循环，
    /// `data` 里带着权威的用量数字（回合中逐请求的计量都是估的）。
    RunCompleted,
    /// 认识但这里不产生事件（调用方按需自己读 `data`），或者不认识。
    Ignored,
}

/// 需要调用方接手的异步副作用。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AsyncIpc {
    /// `tool.image`：`data` 里是落库后的资产而不是路径，要从库里取回来
    /// 再按本地终端的宽度画——daemon 量到的不是用户的终端。
    ToolImage,
    /// `tool.artifact`：同上，资产形态。
    ToolArtifact,
    /// `question.requested`：要弹面板，再按结果回发 `AnswerQuestion` /
    /// `CloseQuestion` / `Cancel` 三条命令之一。
    Question,
}

/// `data[key]` 的字符串,缺省空串。终端各路 IPC 处理共用。
pub fn ipc_text<'a>(data: &'a Value, key: &str) -> &'a str {
    data.get(key).and_then(Value::as_str).unwrap_or_default()
}

pub fn ipc_u64(data: &Value, key: &str) -> u64 {
    data.get(key).and_then(Value::as_u64).unwrap_or_default()
}

fn ipc_opt_text(data: &Value, key: &str) -> Option<String> {
    data.get(key)
        .and_then(Value::as_str)
        .map(std::string::ToString::to_string)
}

fn ipc_bool(data: &Value, key: &str) -> bool {
    data.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn ipc_str_array(data: &Value, key: &str) -> Vec<String> {
    data.get(key)
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str().map(std::string::ToString::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn chunk(kind: ChatStreamKind, text: &str) -> ChatStreamChunk {
    ChatStreamChunk {
        kind,
        text: text.to_string(),
    }
}

/// `(kind, data)` → 事件。`received_at` 由调用方传入而不是这里取
/// `Instant::now()`，这样重放录制的事件流时时序不会被解码这一刻污染。
/// 工具事件里 daemon 算好的显示名记到本进程的表里（见 `tools::register_display_name`）。
/// 三条把 IPC 事件翻成 `AgentEvent` 的路（这儿、REPL 的 `remote::one_shot`、
/// `wake`）都要过一遍，脚本在客户端的时间线上才有名字。
pub fn learn_tool_display_name(data: &Value) {
    miyu_engine::tools::register_display_name(
        ipc_text(data, "name"),
        ipc_text(data, "display_name"),
    );
    // 「加载」那一步点名的工具们：daemon 把它们的显示名一起带来了（见
    // `web::event_map::load_target_display_names`）。
    if let Some(targets) = data.get("display_names").and_then(Value::as_object) {
        for (name, display) in targets {
            if let Some(display) = display.as_str() {
                miyu_engine::tools::register_display_name(name, display);
            }
        }
    }
}

pub(crate) fn decode_ipc_event_at(kind: &str, data: &Value, received_at: Instant) -> DecodedIpc {
    if matches!(kind, "tool.preparing" | "tool.started") {
        learn_tool_display_name(data);
    }
    let event = match kind {
        "turn.started" => AgentEvent::TurnStarted {
            turn_id: ipc_text(data, "turn_id").to_string(),
        },
        "assistant.delta" => {
            AgentEvent::Chunk(chunk(ChatStreamKind::Content, ipc_text(data, "delta")))
        }
        "reasoning.delta" => {
            AgentEvent::Chunk(chunk(ChatStreamKind::Reasoning, ipc_text(data, "delta")))
        }
        "reasoning.start" => AgentEvent::ReasoningStart { received_at },
        "reasoning.reset" => AgentEvent::ReasoningReset { received_at },
        "reasoning.part_start" => AgentEvent::ReasoningPartStart { received_at },
        "reasoning.part_end" => AgentEvent::ReasoningPartEnd { received_at },
        "reasoning.title" => AgentEvent::ReasoningTitle(ipc_text(data, "title").to_string()),
        "tool.preparing" => AgentEvent::ToolPreparing {
            name: ipc_text(data, "name").to_string(),
            batch: ipc_bool(data, "batch"),
        },
        "tool.started" => AgentEvent::ToolCall {
            call_id: ipc_text(data, "tool_id").to_string(),
            name: ipc_text(data, "name").to_string(),
            arguments: ipc_text(data, "arguments").to_string(),
        },
        "tool.progress" => AgentEvent::ToolProgress {
            call_id: ipc_text(data, "tool_id").to_string(),
            name: ipc_text(data, "name").to_string(),
            message: ipc_text(data, "message").to_string(),
        },
        "subagent.progress" => AgentEvent::SubagentProgress {
            call_id: ipc_text(data, "tool_id").to_string(),
            name: ipc_text(data, "name").to_string(),
            status: subagent_status_from(data),
        },
        "tool.output" => AgentEvent::CommandOutput {
            call_id: ipc_text(data, "tool_id").to_string(),
            name: ipc_text(data, "name").to_string(),
            stream: if ipc_text(data, "stream") == "stderr" {
                CommandOutputStream::Stderr
            } else {
                CommandOutputStream::Stdout
            },
            chunk: ipc_text(data, "output").as_bytes().to_vec(),
        },
        "tool.finished" => AgentEvent::ToolResult {
            call_id: ipc_text(data, "tool_id").to_string(),
            name: ipc_text(data, "name").to_string(),
            ok: ipc_bool(data, "ok"),
            output: ipc_text(data, "output").to_string(),
        },
        "tool.image" => return DecodedIpc::Async(AsyncIpc::ToolImage),
        "tool.artifact" => return DecodedIpc::Async(AsyncIpc::ToolArtifact),
        "question.requested" => return DecodedIpc::Async(AsyncIpc::Question),
        "queue.consumed" => AgentEvent::QueuedPromptsConsumed {
            prompt_ids: ipc_str_array(data, "prompt_ids"),
            mode: match ipc_text(data, "mode") {
                "dev" => PersonaLane::Dev,
                _ => PersonaLane::Active,
            },
            provider_id: ipc_opt_text(data, "provider_id"),
            model: ipc_opt_text(data, "model"),
        },
        // 单数 `prompt_id`：编码侧一次只丢一条。
        "queue.removed" => {
            return match ipc_opt_text(data, "prompt_id") {
                Some(id) => DecodedIpc::QueueRemoved(vec![id]),
                None => DecodedIpc::Ignored,
            }
        }
        "generation.superseded" => AgentEvent::GenerationSuperseded {
            prompt_ids: ipc_str_array(data, "prompt_ids"),
        },
        "chat.round_usage" => {
            let round: Usage = data
                .get("usage")
                .cloned()
                .and_then(|usage| serde_json::from_value(usage).ok())
                .unwrap_or_default();
            AgentEvent::RoundUsage {
                round: Box::new(round),
                turn: TurnTokens {
                    total: ipc_u64(data, "turn_total"),
                    prompt: ipc_u64(data, "turn_prompt"),
                    cache_read: ipc_u64(data, "turn_cache_read"),
                },
                // 会话实时累计（已落库的各回合 + 子代理子会话 + 本回合至今）。
                // daemon 逐请求算好了送过来，终端这边不必自己叠。
                cumulative: TurnTokens {
                    total: ipc_u64(data, "cumulative_tokens"),
                    prompt: ipc_u64(data, "cumulative_prompt_tokens"),
                    cache_read: ipc_u64(data, "cumulative_cache_read_tokens"),
                },
                speed: GenerationSpeed {
                    tokens: ipc_u64(data, "turn_generation_tokens"),
                    millis: ipc_u64(data, "turn_generation_ms"),
                },
                estimated: ipc_bool(data, "estimated"),
                provider_id: ipc_opt_text(data, "provider_id"),
                model: ipc_opt_text(data, "model"),
            }
        }
        "context.compact_start" => AgentEvent::CompactStart,
        "context.compact_delta" => {
            AgentEvent::CompactChunk(chunk(ChatStreamKind::Content, ipc_text(data, "delta")))
        }
        "context.compact_end" => AgentEvent::CompactEnd,
        "context.pop_start" => AgentEvent::PopStart,
        "context.pop_end" => AgentEvent::PopEnd,
        "context.notice" => AgentEvent::Notice {
            text: ipc_text(data, "text").to_string(),
        },
        "run.completed" => return DecodedIpc::RunCompleted,
        _ => return DecodedIpc::Ignored,
    };
    DecodedIpc::Event(event)
}

/// `decode_ipc_event_at` 的常用形态：接收时刻即此刻。
pub fn decode_ipc_event(kind: &str, data: &Value) -> DecodedIpc {
    decode_ipc_event_at(kind, data, Instant::now())
}

/// IPC 事件解码表的用例。这张表以前抄在 `cli/repl/remote/one_shot.rs` 和
/// `cli/repl/wake.rs` 两处,漏抄的变体就是 bug 现场;收敛成一份后,这里的
/// 「全集覆盖」用例负责让下一次漏抄当场报红。
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// `web/event_map.rs` 里 `RunEventMapper::handle` 发出的全部 kind。
    /// 加 IPC 事件时这张表要一起加，否则新事件在终端侧静默掉地上。
    const ALL_KINDS: &[&str] = &[
        "turn.started",
        "assistant.delta",
        "reasoning.delta",
        "reasoning.start",
        "reasoning.reset",
        "reasoning.part_start",
        "reasoning.part_end",
        "reasoning.title",
        "tool.started",
        "tool.preparing",
        "tool.progress",
        "tool.output",
        "tool.finished",
        "tool.image",
        "tool.artifact",
        "question.requested",
        "queue.consumed",
        "queue.removed",
        "generation.superseded",
        "chat.round_usage",
        "context.compact_start",
        "context.compact_delta",
        "context.compact_end",
        "context.pop_start",
        "context.pop_end",
        "context.notice",
    ];

    #[test]
    fn every_published_kind_is_decoded() {
        for kind in ALL_KINDS {
            // `queue.removed` 要带上 prompt_id 才有内容可解。
            let data = json!({ "prompt_id": "p1" });
            assert!(
                !matches!(decode_ipc_event(kind, &data), DecodedIpc::Ignored),
                "IPC 事件 {kind} 没有解码分支——终端侧会静默丢掉它"
            );
        }
    }

    #[test]
    fn run_completed_is_control_flow_not_an_event() {
        // 它终止事件循环，不能被当成「不认识」丢掉——漏了就永远等不到回合结束。
        assert!(matches!(
            decode_ipc_event("run.completed", &json!({})),
            DecodedIpc::RunCompleted
        ));
    }

    #[test]
    fn unknown_kind_is_ignored() {
        assert!(matches!(
            decode_ipc_event("chat.made_up", &json!({})),
            DecodedIpc::Ignored
        ));
    }

    #[test]
    fn async_side_effects_are_flagged_not_decoded() {
        let image = decode_ipc_event("tool.image", &json!({}));
        assert!(matches!(image, DecodedIpc::Async(AsyncIpc::ToolImage)));
        let artifact = decode_ipc_event("tool.artifact", &json!({}));
        assert!(matches!(
            artifact,
            DecodedIpc::Async(AsyncIpc::ToolArtifact)
        ));
        let question = decode_ipc_event("question.requested", &json!({}));
        assert!(matches!(question, DecodedIpc::Async(AsyncIpc::Question)));
    }

    #[test]
    fn tool_finished_carries_identity_and_outcome() {
        let data = json!({
            "tool_id": "call-1",
            "name": "run_command",
            "ok": true,
            "output": "done",
        });
        let DecodedIpc::Event(AgentEvent::ToolResult {
            call_id,
            name,
            ok,
            output,
        }) = decode_ipc_event("tool.finished", &data)
        else {
            panic!("tool.finished 应当解码成 ToolResult");
        };
        assert_eq!(call_id, "call-1");
        assert_eq!(name, "run_command");
        assert!(ok);
        assert_eq!(output, "done");
    }

    #[test]
    fn missing_ok_flag_means_failure() {
        // 编码侧永远带 ok，但解码不能因为字段缺失就把失败读成成功。
        let DecodedIpc::Event(AgentEvent::ToolResult { ok, .. }) =
            decode_ipc_event("tool.finished", &json!({ "tool_id": "c" }))
        else {
            panic!("tool.finished 应当解码成 ToolResult");
        };
        assert!(!ok);
    }

    #[test]
    fn command_output_splits_streams() {
        let stderr = decode_ipc_event(
            "tool.output",
            &json!({ "tool_id": "c", "stream": "stderr", "output": "boom" }),
        );
        let DecodedIpc::Event(AgentEvent::CommandOutput { stream, chunk, .. }) = stderr else {
            panic!("tool.output 应当解码成 CommandOutput");
        };
        assert!(matches!(stream, CommandOutputStream::Stderr));
        assert_eq!(chunk, b"boom");

        let stdout = decode_ipc_event(
            "tool.output",
            &json!({ "tool_id": "c", "stream": "stdout", "output": "ok" }),
        );
        let DecodedIpc::Event(AgentEvent::CommandOutput { stream, .. }) = stdout else {
            panic!("tool.output 应当解码成 CommandOutput");
        };
        assert!(matches!(stream, CommandOutputStream::Stdout));
    }

    #[test]
    fn deltas_land_in_the_right_stream_kind() {
        let DecodedIpc::Event(AgentEvent::Chunk(content)) =
            decode_ipc_event("assistant.delta", &json!({ "delta": "hi" }))
        else {
            panic!("assistant.delta 应当解码成 Chunk");
        };
        assert_eq!(content.kind, miyu_core::llm::ChatStreamKind::Content);
        assert_eq!(content.text, "hi");

        let DecodedIpc::Event(AgentEvent::Chunk(reasoning)) =
            decode_ipc_event("reasoning.delta", &json!({ "delta": "think" }))
        else {
            panic!("reasoning.delta 应当解码成 Chunk");
        };
        assert_eq!(reasoning.kind, miyu_core::llm::ChatStreamKind::Reasoning);
    }

    #[test]
    fn queue_consumed_keeps_mode_and_endpoint() {
        let data = json!({
            "prompt_ids": ["a", "b"],
            "mode": "dev",
            "provider_id": "claude-code",
            "model": "opus",
        });
        let DecodedIpc::Event(AgentEvent::QueuedPromptsConsumed {
            prompt_ids,
            mode,
            provider_id,
            model,
        }) = decode_ipc_event("queue.consumed", &data)
        else {
            panic!("queue.consumed 应当解码成 QueuedPromptsConsumed");
        };
        assert_eq!(prompt_ids, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(mode, PersonaLane::Dev);
        assert_eq!(provider_id.as_deref(), Some("claude-code"));
        assert_eq!(model.as_deref(), Some("opus"));
    }

    #[test]
    fn unknown_mode_falls_back_to_normal() {
        let DecodedIpc::Event(AgentEvent::QueuedPromptsConsumed { mode, .. }) =
            decode_ipc_event("queue.consumed", &json!({ "mode": "moonshot" }))
        else {
            panic!("queue.consumed 应当解码成 QueuedPromptsConsumed");
        };
        assert_eq!(mode, PersonaLane::Active);
    }

    #[test]
    fn queue_removed_is_a_queue_change_not_an_event() {
        let removed = decode_ipc_event("queue.removed", &json!({ "prompt_id": "p7" }));
        let DecodedIpc::QueueRemoved(ids) = removed else {
            panic!("queue.removed 应当解码成 QueueRemoved");
        };
        assert_eq!(ids, vec!["p7".to_string()]);
    }

    #[test]
    fn round_usage_rebuilds_the_whole_snapshot() {
        // one_shot 以前只手解了 prompt/completion 两个字段，其余靠边；
        // `Usage` 本身可反序列化，整份还原才能让 footer 的口径和进程内那条路一致。
        let data = json!({
            "usage": {
                "prompt_tokens": 1000,
                "completion_tokens": 200,
                "total_tokens": 1200,
                "cache_read_tokens": 800,
            },
            "turn_total": 5000,
            "turn_prompt": 4000,
            "turn_cache_read": 3000,
            "cumulative_tokens": 9000,
            "cumulative_prompt_tokens": 7000,
            "cumulative_cache_read_tokens": 6000,
            "turn_generation_tokens": 200,
            "turn_generation_ms": 4000,
            "estimated": true,
            "provider_id": "codex",
            "model": "gpt",
        });
        let DecodedIpc::Event(AgentEvent::RoundUsage {
            round,
            turn,
            cumulative,
            speed,
            estimated,
            provider_id,
            model,
        }) = decode_ipc_event("chat.round_usage", &data)
        else {
            panic!("chat.round_usage 应当解码成 RoundUsage");
        };
        assert_eq!(round.prompt_tokens, 1000);
        assert_eq!(round.completion_tokens, 200);
        assert_eq!(round.cache_read_tokens, 800);
        // footer 的上下文占用口径：prompt + completion。
        assert_eq!(
            round.prompt_tokens.saturating_add(round.completion_tokens),
            1200
        );
        assert_eq!(turn.total, 5000);
        assert_eq!(turn.prompt, 4000);
        assert_eq!(turn.cache_read, 3000);
        // 会话实时累计由 daemon 算好送来（含已完成的子代理），终端不自己叠。
        assert_eq!(cumulative.total, 9000);
        assert_eq!(cumulative.prompt, 7000);
        assert_eq!(cumulative.cache_read, 6000);
        assert_eq!(speed.tokens, 200);
        assert_eq!(speed.millis, 4000);
        assert!(estimated);
        assert_eq!(provider_id.as_deref(), Some("codex"));
        assert_eq!(model.as_deref(), Some("gpt"));
    }

    #[test]
    fn malformed_usage_does_not_lose_the_event() {
        // 计量字段坏了也得把事件交出去，否则 footer 卡在上一帧还找不到原因。
        let DecodedIpc::Event(AgentEvent::RoundUsage { round, turn, .. }) =
            decode_ipc_event("chat.round_usage", &json!({ "usage": "not an object" }))
        else {
            panic!("chat.round_usage 应当解码成 RoundUsage");
        };
        assert_eq!(round.prompt_tokens, 0);
        assert_eq!(turn.total, 0);
    }

    #[test]
    fn superseded_carries_the_prompt_ids() {
        let DecodedIpc::Event(AgentEvent::GenerationSuperseded { prompt_ids }) =
            decode_ipc_event("generation.superseded", &json!({ "prompt_ids": ["x"] }))
        else {
            panic!("generation.superseded 应当解码成 GenerationSuperseded");
        };
        assert_eq!(prompt_ids, vec!["x".to_string()]);
    }

    #[test]
    fn context_lifecycle_events_decode() {
        assert!(matches!(
            decode_ipc_event("context.compact_start", &json!({})),
            DecodedIpc::Event(AgentEvent::CompactStart)
        ));
        assert!(matches!(
            decode_ipc_event("context.compact_end", &json!({})),
            DecodedIpc::Event(AgentEvent::CompactEnd)
        ));
        assert!(matches!(
            decode_ipc_event("context.pop_start", &json!({})),
            DecodedIpc::Event(AgentEvent::PopStart)
        ));
        assert!(matches!(
            decode_ipc_event("context.pop_end", &json!({})),
            DecodedIpc::Event(AgentEvent::PopEnd)
        ));
        let DecodedIpc::Event(AgentEvent::Notice { text }) = decode_ipc_event(
            "context.notice",
            &json!({ "text": "窗口太小，自动压缩暂停" }),
        ) else {
            panic!("context.notice 应当解码成 Notice");
        };
        assert_eq!(text, "窗口太小，自动压缩暂停");
    }

    #[test]
    fn reasoning_timestamps_come_from_the_caller() {
        // 重放录制的事件流时，时序不能被「解码这一刻」污染。
        let base = std::time::Instant::now();
        let DecodedIpc::Event(AgentEvent::ReasoningStart { received_at }) =
            decode_ipc_event_at("reasoning.start", &json!({}), base)
        else {
            panic!("reasoning.start 应当解码成 ReasoningStart");
        };
        assert_eq!(received_at, base);
    }
}

#[cfg(test)]
mod display_name_tests {
    use super::*;
    use serde_json::json;

    /// 脚本的显示名在客户端进程里本来是空的：事件里带过来就要认得。
    #[test]
    fn a_tool_started_event_teaches_the_client_the_display_name() {
        let data = json!({
            "tool_id": "call_1",
            "name": "walk_script_2026",
            "display_name": "打个招呼",
            "arguments": "{}",
        });
        let _ = decode_ipc_event("tool.started", &data);
        assert_eq!(
            miyu_engine::tools::readable_tool_name("walk_script_2026"),
            "打个招呼"
        );
        // 内建工具不受影响：事件里的名字是 daemon 那边 locale 下的，内建表先翻。
        learn_tool_display_name(&json!({"name": "run_command", "display_name": "Run command"}));
        assert_eq!(
            miyu_engine::tools::readable_tool_name("run_command"),
            miyu_engine::tools::readable_tool_name("run_command")
        );
        // 没带、或者和 id 一样的不记。
        learn_tool_display_name(&json!({"name": "walk_plain", "display_name": "walk_plain"}));
        assert_eq!(
            miyu_engine::tools::readable_tool_name("walk_plain"),
            "walk_plain"
        );
        // 「加载：某脚本」那一步：事件名带前缀，daemon 已经拼好了显示名，整名照收——
        // 那一刻脚本自己的名字还没学到。
        let _ = decode_ipc_event(
            "tool.preparing",
            &json!({
                "name": "load_tools:walk_script_later",
                "display_name": "加载：稍后才学到的脚本"
            }),
        );
        assert_eq!(
            miyu_engine::tools::readable_tool_name("load_tools:walk_script_later"),
            "加载：稍后才学到的脚本"
        );
    }
}

/// `subagent.progress` 事件里那个子代理的样子（`event_map.rs` 发的形状）。
pub fn subagent_status_from(data: &Value) -> miyu_engine::tools::subagent::status::SubagentStatus {
    miyu_engine::tools::subagent::status::SubagentStatus {
        peek: ipc_text(data, "peek").to_string(),
        tokens_label: ipc_text(data, "tokens_label").to_string(),
        tokens: data.get("tokens").and_then(Value::as_u64).unwrap_or(0),
        session_id: data
            .get("session_id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .map(str::to_string),
    }
}
