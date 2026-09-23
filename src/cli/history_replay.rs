//! 上键历史那条记录，以及「把存下来的回合重画成一帧」。从 `src/cli/mod.rs` 搬来（09-16 拆分），逻辑未改。

use crate::cli::*;

/// 上键历史里的一条。
///
/// 以前存的是展开后的全文:粘贴折成的 `[粘贴 1: ~40 行]` 一进历史就散成
/// 四十行裸文本,上键回来把输入框撑满;`[Image 1]` 则相反,原样进历史却
/// 丢了图。现在存**输入框里的样子**加载荷,回忆时占位符照旧是活的——退格
/// 整块删、提交时照常展开、图片重新接回缓存文件。
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(super) struct ReplHistoryEntry {
    pub(super) display: String,
    /// 按 `[粘贴 N]` 的序号排;被整块删掉的占位符留 None,序号才对得上。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) pasted_texts: Vec<Option<String>>,
    /// 按 `[Image N]` 的序号排的缓存文件路径。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) images: Vec<Option<String>>,
}

impl ReplHistoryEntry {
    pub(super) fn plain(text: &str) -> Self {
        Self {
            display: text.to_string(),
            ..Self::default()
        }
    }

    pub(super) fn from_submission(submission: &LiveSubmission) -> Self {
        let pasted_texts = submission
            .pasted_texts
            .iter()
            .map(|payload| payload.as_ref().map(|pasted| pasted.text.clone()))
            .collect::<Vec<_>>();
        let images = submission
            .images
            .iter()
            .map(|image| image.as_ref().and_then(|image| image.history_path()))
            .collect::<Vec<_>>();
        Self {
            display: submission.display_content.clone(),
            pasted_texts: trim_trailing_none(pasted_texts),
            images: trim_trailing_none(images),
        }
    }

    pub(super) fn has_payload(&self) -> bool {
        !self.pasted_texts.is_empty() || !self.images.is_empty()
    }

    pub(super) fn pasted_texts(&self) -> Vec<Option<PastedText>> {
        self.pasted_texts
            .iter()
            .map(|text| text.as_ref().map(|text| PastedText { text: text.clone() }))
            .collect()
    }

    /// 只接回还在的缓存文件:清理掉的图片留 None,占位符就成了普通文字。
    pub(super) fn pasted_images(&self) -> Vec<Option<miyu_base::clipboard::PastedImage>> {
        self.images
            .iter()
            .map(|path| {
                path.as_ref()
                    .filter(|path| std::path::Path::new(path).is_file())
                    .map(|path| miyu_base::clipboard::PastedImage::Path(path.clone()))
            })
            .collect()
    }

    /// 模型实际收到的样子;对话记录里的用户消息就是这个形态,合并去重用它。
    pub(super) fn expanded(&self) -> String {
        if self.pasted_texts.is_empty() {
            return self.display.clone();
        }
        expand_pasted_text_placeholders(&self.display, &self.pasted_texts())
    }

    /// 落盘一行:没载荷的照旧写成 JSON 字符串,老版本读得懂,文件也不膨胀。
    pub(super) fn to_json_line(&self) -> Option<String> {
        if self.has_payload() {
            serde_json::to_string(self).ok()
        } else {
            serde_json::to_string(&self.display).ok()
        }
    }

    pub(super) fn parse_line(line: &str) -> Option<Self> {
        if let Ok(text) = serde_json::from_str::<String>(line) {
            return Some(Self::plain(&text));
        }
        serde_json::from_str::<Self>(line).ok()
    }
}

/// Redraws finished turns of a session as one ANSI frame.
///
/// Feeds the stored transcript back through the same `StreamRenderer` a live
/// turn uses, so tool blocks and prose come out identical — and re-wrapped for
/// the terminal's *current* width, which a saved byte transcript could not do.
/// Turns older than the transcript column fall back to prompt + final reply.
pub(super) fn session_replay_frame(
    replays: &[miyu_core::state::TurnReplay],
    mode: PersonaLane,
    config: &AppConfig,
    cols: usize,
    endpoint_line: bool,
) -> Result<Vec<u8>> {
    use miyu_core::state::ReplayEntry;
    let mut frame = Vec::new();
    for replay in replays {
        // 每一轮开头埋一个标记，重开之后 `/undo` 也知道该截到哪儿。
        if render::blocks::enabled() {
            frame.extend_from_slice(render::blocks::TURN_START_MARKER.as_bytes());
        }
        if replay.display_content.starts_with("[目标续轮]") {
            // 目标续轮什么都不画——实时渲染也不打表头。一个长任务几十轮，
            // 每轮一行只会把真正的输出挤散。
        } else if let Some(message) =
            miyu_core::state::parse_cross_session_message(&replay.display_content)
        {
            // 另一个会话里的 AI 发来的那条（09-23）：和实时渲染同一块，整段外壳
            // 当一行提示印出来就是一屏标签。
            frame.push(b'\n');
            render::timeline::write_cross_session_message(
                &mut frame,
                &miyu_core::state::cross_session_headline(
                    &message.from_name,
                    &message.from_session,
                ),
                &message.body,
                config.display.cross_session_preview_lines,
            )?;
        } else if replay.is_synthetic {
            // daemon 自己合成的轮：实时渲染画的是一条暗色 `⚙` 提示，回放要
            // 对齐，不能变成用户气泡。
            let notice = format!(
                "\n\x1b[2m{} {}\x1b[0m\n\n",
                if render::blocks::enabled() {
                    render::timeline::glyph_notice()
                } else {
                    "⚙"
                },
                job_wake_headline(&replay.display_content)
            );
            let notice = if render::blocks::enabled() {
                render::timeline::indent_body(&notice)
            } else {
                notice
            };
            frame.extend_from_slice(notice.as_bytes());
        } else if !replay.display_content.trim().is_empty() {
            frame.extend_from_slice(
                committed_user_messages_text(&[(&replay.display_content, mode)], true, cols)
                    .as_bytes(),
            );
        }
        let mut renderer = render::StreamRenderer::new(
            // 全屏：思考在时间线里只占一行，回放时补上正好补齐"重开之后
            // 少一块"的缺口。inline 照旧不放——那边一放就是整段，回放会刷屏。
            if render::blocks::enabled() {
                render::ReasoningDisplayMode::Summary
            } else {
                render::ReasoningDisplayMode::Hidden
            },
            render::ToolCallDisplayMode::from_expand(config.display.expand_tool_calls),
            false,
            config.display.readable_tool_names,
            config.display.command_output_lines,
        );
        renderer.fold_timeline = config.display.fold_timeline;
        renderer.thinking_scroll_lines = config.display.thinking_scroll_lines;
        renderer.cross_session_preview_lines = config.display.cross_session_preview_lines;
        renderer.use_external_cursor_control();
        renderer.use_buffered_output();
        // 流水账里带着思考就按它的位置放，别再用 `assistant_reasoning` 那一列
        // 补一遍。那一列只留得住**最后一回合**的思考：想完就去调工具、最后一
        // 回合直接交卷的那种轮，它是空的——重开之后时间线上的思考那一步整个
        // 没了（用户实测）。老轮（这次改动之前记下的）流水账里没有思考，那就
        // 还是拿那一列兜底。
        let journal_has_reasoning = replay
            .entries
            .iter()
            .any(|entry| matches!(entry, ReplayEntry::Reasoning { .. }));
        if !journal_has_reasoning {
            if let Some(reasoning) = replay
                .assistant_reasoning
                .as_deref()
                .filter(|text| !text.trim().is_empty())
            {
                renderer.write_chunk(ChatStreamChunk {
                    kind: miyu_core::llm::ChatStreamKind::Reasoning,
                    text: reasoning.to_string(),
                })?;
            }
        }
        if replay.entries.is_empty() {
            // 被中断的轮：正文尾巴上那段 `<system-reminder>` 是写给模型的，
            // 不给人看。
            let content = if replay.interrupted {
                miyu_core::state::interrupted_prefix(&replay.assistant_content)
            } else {
                replay.assistant_content.clone()
            };
            renderer.write_chunk(ChatStreamChunk {
                kind: miyu_core::llm::ChatStreamKind::Content,
                text: content,
            })?;
        } else {
            for entry in &replay.entries {
                match entry {
                    ReplayEntry::Text { text } => renderer.write_chunk(ChatStreamChunk {
                        kind: miyu_core::llm::ChatStreamKind::Content,
                        text: text.clone(),
                    })?,
                    ReplayEntry::Reasoning { text, elapsed_ms } => {
                        renderer.write_chunk(ChatStreamChunk {
                            kind: miyu_core::llm::ChatStreamKind::Reasoning,
                            text: text.clone(),
                        })?;
                        renderer.replay_reasoning_elapsed(std::time::Duration::from_millis(
                            *elapsed_ms,
                        ));
                    }
                    ReplayEntry::ToolCall { name, arguments } => {
                        renderer.write_tool_call(name, arguments)?
                    }
                    ReplayEntry::ToolResult {
                        name,
                        ok,
                        output,
                        elapsed_ms,
                    } => {
                        renderer.replay_tool_elapsed(
                            name,
                            std::time::Duration::from_millis(*elapsed_ms),
                        );
                        renderer.write_tool_result(name, *ok, output)?
                    }
                }
            }
        }
        renderer.finish()?;
        frame.extend_from_slice(&renderer.take_output_frame());
        // 混合模型池：实时那一轮末尾有「本次供应商 / 模型」，回放也补上，重开之后
        // 才对得上（提交的是哪家答的，库里记着）。
        if endpoint_line {
            if let (Some(provider), Some(model)) = (
                replay
                    .assistant_provider_id
                    .as_deref()
                    .filter(|s| !s.is_empty()),
                replay.assistant_model.as_deref().filter(|s| !s.is_empty()),
            ) {
                frame.extend_from_slice(
                    crate::cli::model_cmds::mixed_model_endpoint_frame(provider, model, None)
                        .as_bytes(),
                );
            }
        }
        if replay.interrupted {
            // 标一行：这一轮没说完。和后台任务那条提示一个样子。
            let notice = format!(
                "\x1b[2m{} {}\x1b[0m\n\n",
                if render::blocks::enabled() {
                    render::timeline::glyph_notice()
                } else {
                    "⚙"
                },
                t("interrupted", "已中断")
            );
            let notice = if render::blocks::enabled() {
                render::timeline::indent_body(&notice)
            } else {
                notice
            };
            frame.extend_from_slice(notice.as_bytes());
        }
    }
    Ok(frame)
}

/// Queues a submission for the turn currently running in the daemon, using
/// the cross-process queue target so the daemon consumes it mid-turn.
pub(super) async fn persist_remote_queued_submission(
    paths: &MiyuPaths,
    run_id: &str,
    turn_id: &str,
    submission: &LiveSubmission,
) -> Result<QueuedPrompt> {
    let mut stream = ipc::connect(&paths.ipc_socket()).await?;
    ipc::send(
        &mut stream,
        &IpcRequest::new(IpcCommand::QueueTurnUpdate {
            run_id: run_id.to_string(),
            turn_id: turn_id.to_string(),
            content: submission.content.clone(),
            display_content: submission.display_content.clone(),
            images: ipc_images(&submission.images),
            supersede: false,
        }),
    )
    .await?;
    match ipc::receive::<IpcFrame>(&mut stream).await? {
        Some(IpcFrame::TurnUpdateAccepted {
            prompt_id,
            seq,
            submitted_at,
            ..
        }) => Ok(QueuedPrompt {
            prompt_id,
            seq,
            content: submission.content.clone(),
            display_content: submission.display_content.clone(),
            attachments: queued_prompt_attachments(&submission.images),
            uploaded_attachments: Vec::new(),
            submitted_at,
        }),
        Some(IpcFrame::Error { message, .. }) => bail!("{message}"),
        Some(_) => bail!("Miyu core returned an invalid queue response"),
        None => bail!("Miyu core closed the queue connection"),
    }
}

pub(super) fn run_history_with_state(state: &StateStore, args: HistoryArgs) -> Result<()> {
    for entry in state.history(args.limit)? {
        if args.raw {
            println!("{}", serde_json::to_string(&entry)?);
            continue;
        }
        let display_role = if entry.role.ends_with("_clarification") {
            entry.role.trim_end_matches("_clarification")
        } else {
            entry.role.as_str()
        };
        println!("{} {display_role}", entry.timestamp);
        if entry.role.starts_with("assistant") {
            let response = miyu_core::llm::ChatResult {
                content: entry.content,
                reasoning: if args.no_thinking {
                    None
                } else {
                    entry.reasoning
                },
                usage: None,
                usage_estimated: false,
                tool_calls: Vec::new(),
                provider_id: None,
                model: None,
                finish_reason: None,
                thinking_signature: None,
                last_request_usage: None,
                responses_continuation: None,
            };
            render::print_assistant_response(&response, !args.no_thinking)?;
        } else {
            println!("{}", entry.content);
        }
        println!();
    }
    Ok(())
}
