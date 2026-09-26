//! 远端 REPL 的上下文类斜杠命令:/undo /pop /compact /reset-memory /reset-all-memory /reset /wipe。09-17 从 `run_remote_repl` 抽出。

use super::interactive::{LoopStep, RemoteRepl};
use crate::cli::*;

impl RemoteRepl {
    pub(super) async fn cmd_undo(&mut self) -> Result<LoopStep> {
        let Some((state, data)) = repl_ipc_admin(
            &self.paths,
            &mut self.live_repl,
            IpcCommand::Undo {
                target: miyu_core::ipc::SessionRef::Id {
                    id: self.active_session_id.clone(),
                },
            },
        )
        .await?
        else {
            return Ok(LoopStep::Continue);
        };
        let removed = data
            .get("removed")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        // 撤的是一次压缩(最后一条是摘要行,daemon 把折进去的轮放回来,没有可回填
        // 的提示词):屏上什么都不截——那一块「上下文已压缩」不是一轮,按轮标记截会
        // 把前一轮真正的对话一起截掉;留着它,底下补一行「已撤销上下文压缩」。
        let compaction_undone =
            removed > 0 && data.get("prompt").and_then(|v| v.as_str()).is_none();
        // 先把撤掉的那一轮从屏上拿掉，再打「已撤销」那一行——不然那一行被
        // 重画一起擦掉。
        if removed > 0 && !compaction_undone {
            redraw_after_undo(
                &self.paths,
                &self.config,
                self.mode,
                &self.active_session_id,
                &mut self.live_repl,
            )?;
        }
        if compaction_undone {
            // 全屏:那块「上下文已压缩」从屏上截掉(它还挂着能点开,用户 09-18);
            // 前一轮对话留着。重开过 TUI 的话屏上本来就没那块,截不到就算了。
            if crate::cli::in_fullscreen() {
                synchronized_terminal_update(CursorAfterUpdate::Preserve, || {
                    self.live_repl.truncate_last_compact()
                })?;
            }
            repl_note(
                &mut self.live_repl,
                &format!(
                    "\x1b[2m{}\x1b[0m\n",
                    t("context compaction undone", "已撤销上下文压缩")
                ),
            )?;
        } else {
            repl_note(
                &mut self.live_repl,
                &format!("{}: {removed}\n", t("undone messages", "已撤销消息数")),
            )?;
        }
        if let Some(prompt) = data.get("prompt").and_then(serde_json::Value::as_str) {
            self.live_repl.editor.input = prompt.to_string();
            self.live_repl.editor.cursor = self.live_repl.editor.input.chars().count();
            self.live_repl.editor.history_clean_index = None;
        }
        self.cumulative_tokens = state_cumulative(&state);
        self.footer.update_session_tokens(state.context_tokens);
        self.footer
            .update_context_window(state.context_window, state.context_window_assumed);
        self.footer.update_cumulative_tokens(self.cumulative_tokens);
        // 改了 footer 的数要当场重画：原来只改内存，上下文读数要等下一轮结束才变
        //（用户 09-18：「上下文也没有因为 undo 去掉了一些内容而即时刷新」）。
        self.live_repl.refresh_footer(self.footer.clone())?;
        Ok(LoopStep::Continue)
    }

    pub(super) async fn cmd_pop(&mut self, command_args: &str) -> Result<LoopStep> {
        let count = match parse_repl_pop_count(command_args) {
            Ok(count) => count,
            Err(err) => {
                repl_note(
                    &mut self.live_repl,
                    &crate::cli::repl::session::error_frame(&err),
                )?;
                return Ok(LoopStep::Continue);
            }
        };
        let state_store = StateStore::new(&self.paths)?.pinned(&self.active_session_id);
        state_store.recover_stale_turns()?;
        let candidates = state_store.oldest_evictable_visible_turns(count.unwrap_or(usize::MAX))?;
        let turn_ids = if count.is_some() {
            candidates.into_iter().map(|turn| turn.turn_id).collect()
        } else {
            let all = state_store.oldest_evictable_visible_turns(usize::MAX)?;
            if all.is_empty() {
                repl_note(&mut self.live_repl, &repl_nothing_to_pop_text())?;
                return Ok(LoopStep::Continue);
            }
            let Some(selected) = inline_pop_select(&all)? else {
                return Ok(LoopStep::Continue);
            };
            all.into_iter()
                .zip(selected)
                .filter_map(|(turn, selected)| selected.then_some(turn.turn_id))
                .collect()
        };
        let Some((state, data)) = repl_ipc_admin(
            &self.paths,
            &mut self.live_repl,
            IpcCommand::Pop {
                target: miyu_core::ipc::SessionRef::Id {
                    id: self.active_session_id.clone(),
                },
                turn_ids,
            },
        )
        .await?
        else {
            return Ok(LoopStep::Continue);
        };
        let turns = data
            .get("turns")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        if turns > 0 {
            let outcome = PopOutcome {
                turns: turns as usize,
                archived: data
                    .get("archived")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
            };
            repl_note(&mut self.live_repl, &repl_pop_outcome_text(outcome))?;
        } else {
            repl_note(&mut self.live_repl, &repl_nothing_to_pop_text())?;
        }
        self.cumulative_tokens = state_cumulative(&state);
        self.footer.update_session_tokens(state.context_tokens);
        self.footer
            .update_context_window(state.context_window, state.context_window_assumed);
        self.footer.update_cumulative_tokens(self.cumulative_tokens);
        // 同 `/undo`：footer 的数当场重画。
        self.live_repl.refresh_footer(self.footer.clone())?;
        Ok(LoopStep::Continue)
    }

    pub(super) async fn cmd_compact(&mut self) -> Result<LoopStep> {
        // 全屏：不用右上角的通知，写进正文——一行「正在压缩」，压完一行
        // 结果，摘要收成一块点开看（用户实测：压缩上下文只有右上角的通知）。
        // inline 照旧：两行提示 + 用量。
        let fullscreen = crate::cli::in_fullscreen();
        let notice = |text: &str| -> String {
            miyu_hosts::render::timeline::indent_body(&format!(
                "\x1b[2m{} {text}\x1b[0m\n",
                miyu_hosts::render::timeline::glyph_notice()
            ))
        };
        let compacting = t("compacting context…", "正在压缩上下文…");
        // 摘要边生成边转发过来,攒起来压完收成一块。等的这几十秒 footer 的转轮
        // 要转着(用户 09-18:正在压缩上下文应该有个 spinner),和回合里一样喂。
        let mut summary = String::new();
        // 转轮是时间线状态行上 logo 左侧那个点阵(用户 09-18 点名的那个),不是
        // footer 的声波:起一个渲染器、开等待态、把文案钉成「正在压缩上下文」,
        // 每帧和回合里一样喂(footer 的声波顺带也转)。
        let mut renderer = render::StreamRenderer::new(
            render::ReasoningDisplayMode::from_expand(self.config.display.expand_reasoning),
            render::ToolCallDisplayMode::from_expand(self.config.display.expand_tool_calls),
            false,
            self.config.display.readable_tool_names,
            self.config.display.command_output_lines,
        );
        renderer.fold_timeline = self.config.display.fold_timeline;
        renderer.thinking_scroll_lines = self.config.display.thinking_scroll_lines;
        renderer.cross_session_preview_lines = self.config.display.cross_session_preview_lines;
        renderer.use_external_cursor_control();
        renderer.use_buffered_output();
        renderer.start_waiting()?;
        renderer.set_custom_waiting_phase(Some(compacting.to_string()));
        self.live_repl.apply_renderer_frame(&mut renderer)?;
        // 转轮那一行已经写着「正在压缩上下文…」,再写一行静态的就是重复(用户
        // 09-18 截图);只有转轮起不来(plain / 非 TTY)才留静态那行。
        if !renderer.is_waiting() {
            if fullscreen {
                self.live_repl
                    .apply_output_frame(notice(compacting).as_bytes())?;
            } else {
                repl_note(&mut self.live_repl, &format!("\x1b[2m{compacting}\x1b[0m"))?;
            }
        }
        let live_repl = &mut self.live_repl;
        let outcome = send_ipc_admin_streaming_ticked(
            &self.paths,
            IpcCommand::Compact {
                target: miyu_core::ipc::SessionRef::Id {
                    id: self.active_session_id.clone(),
                },
            },
            |kind, data| {
                if kind == "context.compact_delta" {
                    summary.push_str(ipc_text(data, "delta"));
                }
                Ok(())
            },
            Duration::from_millis(33),
            || live_repl.tick_spinner(&mut renderer),
        )
        .await;
        renderer.set_custom_waiting_phase(None);
        renderer.finish()?;
        self.live_repl.apply_renderer_frame(&mut renderer)?;
        self.live_repl.stop_footer_spinner()?;
        let (state, data) = match outcome {
            Ok(result) => result,
            Err(err) => {
                repl_note(
                    &mut self.live_repl,
                    &crate::cli::repl::session::error_frame(&err),
                )?;
                return Ok(LoopStep::Continue);
            }
        };
        // 这条会话其实有一轮在跑（这边以为闲着，比如后台报告刚起的那一轮）：守护进程把压缩
        // 排进了那一轮（09-25），说一声就好。
        if data.get("queued").and_then(serde_json::Value::as_bool) == Some(true) {
            repl_note(
                &mut self.live_repl,
                &format!(
                    "\x1b[2m{}\x1b[0m\n",
                    t(
                        "a turn is running in this session; compaction is queued behind its next step",
                        "这条会话有一轮正在跑，压缩排进了它，走到下一步时做",
                    )
                ),
            )?;
            return Ok(LoopStep::Continue);
        }
        // 压完 footer 的上下文读数当场刷新(用户 09-18:压缩后 footer 没刷新)——
        // 原来只把数写进正文那一行,footer 要等下一轮结束才变。
        self.cumulative_tokens = state_cumulative(&state);
        self.footer.update_session_tokens(state.context_tokens);
        self.footer.update_cumulative_tokens(self.cumulative_tokens);
        if let Some(usage) = data
            .get("usage")
            .cloned()
            .filter(|value| !value.is_null())
            .map(serde_json::from_value::<Usage>)
            .transpose()?
        {
            let result = ChatResult {
                content: String::new(),
                reasoning: None,
                usage: Some(usage),
                usage_estimated: data
                    .get("usage_estimated")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
                tool_calls: Vec::new(),
                provider_id: None,
                model: None,
                finish_reason: None,
                thinking_signature: None,
                last_request_usage: None,
                responses_continuation: None,
            };
            self.footer.update_token_usage(
                &result,
                state.context_tokens,
                state.context_window,
                self.cumulative_tokens,
            );
            if fullscreen {
                let mut head = t("context compacted", "上下文已压缩").to_string();
                if let Some(usage_line) = chat_token_usage_text(
                    &result,
                    self.config.display.show_token_usage,
                    state.context_tokens,
                    state.context_window,
                    state_cumulative(&state),
                ) {
                    head.push_str(" · ");
                    head.push_str(&usage_line);
                }
                let mut frame = Vec::new();
                // 这一块的起点埋个标记:`/undo` 撤压缩时按它把这一块截掉。
                if miyu_hosts::render::blocks::enabled() {
                    frame.extend_from_slice(
                        miyu_hosts::render::blocks::COMPACT_START_MARKER.as_bytes(),
                    );
                }
                miyu_hosts::render::timeline::write_compact_summary(&mut frame, &head, &summary)?;
                self.live_repl.apply_output_frame(&frame)?;
            } else {
                repl_note(
                    &mut self.live_repl,
                    &format!("\x1b[2m{}\x1b[0m\n", t("context compacted", "上下文已压缩")),
                )?;
                print_chat_token_usage(
                    &result,
                    self.config.display.show_token_usage,
                    state.context_tokens,
                    state.context_window,
                    state_cumulative(&state),
                )?;
            }
        } else {
            let nothing = t("nothing to compact", "没有可压缩的上下文");
            if fullscreen {
                self.live_repl
                    .apply_output_frame(notice(nothing).as_bytes())?;
            } else {
                repl_note(&mut self.live_repl, &format!("\x1b[2m{nothing}\x1b[0m\n"))?;
            }
        }
        self.live_repl.refresh_footer(self.footer.clone())?;
        Ok(LoopStep::Continue)
    }

    pub(super) async fn cmd_reset_memory(&mut self) -> Result<LoopStep> {
        // 不二次确认:只清本会话记下的那部分,会话历史/技能/知识库
        // 都不动。会话点名发过去,免得 daemon 的全局指针早已换到
        // 别的会话上。
        let Some((_, data)) = repl_ipc_admin(
            &self.paths,
            &mut self.live_repl,
            IpcCommand::ResetMemory {
                mode: self.mode.is_dev().then(|| "dev".to_string()),
                scope: miyu_core::ipc::MemoryResetScope::Session,
                session: Some(miyu_core::ipc::SessionRef::Id {
                    id: self.active_session_id.clone(),
                }),
            },
        )
        .await?
        else {
            return Ok(LoopStep::Continue);
        };
        repl_note(
            &mut self.live_repl,
            &format!("\x1b[2m{}\x1b[0m\n", ipc_text(&data, "text")),
        )?;
        Ok(LoopStep::Continue)
    }

    pub(super) async fn cmd_reset_all_memory(&mut self) -> Result<LoopStep> {
        // 不二次确认:清的是长期记忆全量,会话历史/技能/知识库仍不动。
        let Some((_, _)) = repl_ipc_admin(
            &self.paths,
            &mut self.live_repl,
            IpcCommand::ResetMemory {
                mode: self.mode.is_dev().then(|| "dev".to_string()),
                scope: miyu_core::ipc::MemoryResetScope::All,
                session: None,
            },
        )
        .await?
        else {
            return Ok(LoopStep::Continue);
        };
        repl_note(
            &mut self.live_repl,
            &format!(
                "\x1b[2m{}\x1b[0m\n",
                t("all long-term memory erased", "全部长期记忆已清空")
            ),
        )?;
        Ok(LoopStep::Continue)
    }

    pub(super) async fn cmd_reset(&mut self) -> Result<LoopStep> {
        let Some((state, _)) = repl_ipc_admin(
            &self.paths,
            &mut self.live_repl,
            IpcCommand::ResetConversation {
                target: miyu_core::ipc::SessionRef::Id {
                    id: self.active_session_id.clone(),
                },
            },
        )
        .await?
        else {
            return Ok(LoopStep::Continue);
        };
        self.live_repl.editor.input.clear();
        self.live_repl.editor.cursor = 0;
        // The footer numbers are only half of it: the loop's own Σ
        // accumulator has to go too, or the next config reload
        // rebuilds the footer from the pre-reset total. The queue
        // rows were deleted in the store, so the strip has to be
        // reloaded rather than left showing them.
        self.cumulative_tokens = TurnTokens::default();
        self.footer
            .reset_token_usage(state.context_tokens, state.context_window);
        // 清空之后又是空会话:banner 回来,Tab 又能换车道。
        self.live_repl
            .set_session_empty(&self.config, &self.paths, true);
        // 存下新数字还不够:footer 不重绘,屏幕上的 Σ 就一直
        // 挂着重置前的累计(验收问题四)。
        self.live_repl.refresh_footer(self.footer.clone())?;
        reload_repl_queue(&mut self.live_repl, &self.paths, &self.active_session_id)?;
        // 全屏：画布随会话一起清空（`set_session_empty` 丢掉正文缓冲），
        // 下一句话从屏顶起。以前这里是 `clear_screen`（顶空一屏、往回翻
        // 还在），第一句话就接在那一屏空行后面、出现在屏底（用户实测）。
        repl_note(
            &mut self.live_repl,
            &format!(
                "\x1b[2m{}\x1b[0m\n",
                t(
                    "cleared current conversation self.history",
                    "已清空当前会话历史"
                )
            ),
        )?;
        Ok(LoopStep::Continue)
    }

    pub(super) async fn cmd_wipe(&mut self) -> Result<LoopStep> {
        repl_note(
            &mut self.live_repl,
            &format!("\x1b[2m{}\x1b[0m\n", wipe_summary()),
        )?;
        if !confirm_inline(&mut self.live_repl, t("wipe everything?", "确认全部抹掉？"))? {
            repl_note(
                &mut self.live_repl,
                &format!("\x1b[2m{}\x1b[0m\n", t("cancelled", "已取消")),
            )?;
            return Ok(LoopStep::Continue);
        }
        let Some((state, _)) =
            repl_ipc_admin(&self.paths, &mut self.live_repl, IpcCommand::WipePersona).await?
        else {
            return Ok(LoopStep::Continue);
        };
        self.live_repl.editor.input.clear();
        self.live_repl.editor.cursor = 0;
        self.cumulative_tokens = TurnTokens::default();
        self.footer
            .reset_token_usage(state.context_tokens, state.context_window);
        // 清空之后又是空会话:banner 回来,Tab 又能换车道。
        self.live_repl
            .set_session_empty(&self.config, &self.paths, true);
        self.live_repl.refresh_footer(self.footer.clone())?;
        reload_repl_queue(&mut self.live_repl, &self.paths, &self.active_session_id)?;
        repl_note(
            &mut self.live_repl,
            &format!("\x1b[2m{}\x1b[0m\n", print_wipe_message()),
        )?;
        Ok(LoopStep::Continue)
    }
}
