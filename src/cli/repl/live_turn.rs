//! 直接在终端里驱动的回合（不经过 daemon）。
//!
//! `run_live_agent_turn` 与 `handle_live_agent_event` 是本地事件泵。和
//! [`super::remote`] 的远端泵是两条独立的分发表——08-17 那次「回合中途 footer
//! 不刷新」正是因为只有一边接了 `chat.round_usage`，加事件时两边都要过一遍。

use crate::cli::repl::editor::*;
use crate::cli::repl::tail::*;
use crate::cli::*;

pub(in crate::cli) async fn handle_live_post_turn_overflow(
    live: &mut LiveReplTail,
    agent: &Agent,
    renderer: &mut render::StreamRenderer,
    context_tokens: u64,
    show_token_usage: bool,
    cumulative_tokens: Option<&mut TurnTokens>,
) -> Result<Option<miyu_core::llm::ChatResult>> {
    let compact_result = agent
        .handle_overflow_after_turn(context_tokens, |event| {
            handle_live_agent_event(live, renderer, event)
        })
        .await?;
    renderer.finish()?;
    live.apply_renderer_frame(renderer)?;
    if let Some(compact_result) = compact_result {
        let mut cumulative_display = TurnTokens::default();
        if let Some(total) = cumulative_tokens {
            if let Some(usage) = compact_result.usage.as_ref() {
                total.add(TurnTokens::from_usage(Some(usage)));
                cumulative_display = *total;
            }
        }
        if show_token_usage {
            if let Some(usage) = compact_result.usage.as_ref() {
                let frame = render::token_usage_output(
                    &turn_meter(
                        TurnTokens::from_usage(Some(usage)),
                        GenerationSpeed::default(),
                        agent.effective_context_tokens()?,
                        agent.context_window(),
                        cumulative_display,
                    ),
                    compact_result.usage_estimated,
                );
                live.apply_output_frame(frame.strip_suffix('\n').unwrap_or(&frame).as_bytes())?;
            }
        }
        return Ok(Some(compact_result));
    }
    Ok(None)
}

pub(in crate::cli) fn handle_live_agent_event(
    live: &mut LiveReplTail,
    renderer: &mut render::StreamRenderer,
    event: AgentEvent,
) -> Result<()> {
    let event = match event {
        AgentEvent::Chunk(chunk) => {
            live.queue_stream_chunk(chunk);
            return Ok(());
        }
        AgentEvent::RoundUsage {
            round, turn, speed, ..
        } => {
            // 一次模型请求刚结束:立即刷新 footer 计量,不等整个回合。
            // prompt+completion 即该请求结束时的上下文实际占用。
            let context_tokens = round.prompt_tokens.saturating_add(round.completion_tokens);
            return live.refresh_round_usage(context_tokens, turn, speed);
        }
        event => event,
    };
    if live.external_output_active && matches!(&event, AgentEvent::SpinnerTick) {
        return Ok(());
    }
    if matches!(&event, AgentEvent::SpinnerTick) {
        return live.tick_spinner(renderer);
    }
    match event {
        AgentEvent::PrepareForExternalOutput { ready } => {
            let result = (|| {
                live.flush_pending_chunks(renderer)?;
                renderer.prepare_for_external_output()?;
                live.apply_renderer_frame(renderer)?;
                synchronized_terminal_update(CursorAfterUpdate::Hidden, || live.suspend())?;
                live.external_output_active = true;
                Ok(())
            })();
            if result.is_ok() {
                let _ = ready.send(true);
            }
            result
        }
        AgentEvent::QueuedPromptsConsumed {
            prompt_ids, mode, ..
        } => {
            live.flush_pending_chunks(renderer)?;
            renderer.prepare_for_external_output()?;
            live.apply_renderer_frame(renderer)?;
            synchronized_terminal_update(CursorAfterUpdate::Preserve, || {
                live.suspend()?;
                live.consume_queued(&prompt_ids, mode)
            })
        }
        event => {
            let finishes_external_output =
                live.external_output_active && matches!(&event, AgentEvent::ToolResult { .. });
            if live.external_output_active && !finishes_external_output {
                handle_agent_event(renderer, event)?;
                return live.apply_renderer_frame(renderer);
            }
            if let AgentEvent::AskQuestion {
                request, responder, ..
            } = event
            {
                live.flush_pending_chunks(renderer)?;
                renderer.prepare_for_external_output()?;
                live.apply_renderer_frame(renderer)?;
                synchronized_terminal_update(CursorAfterUpdate::Hidden, || live.suspend())?;
                let mut scroll = |delta: isize, panel_rows: u16| {
                    if let Some(screen) = live.screen.as_mut() {
                        let _ = screen.scroll_question_body(delta, panel_rows);
                    }
                };
                // 她反问了：侧栏标红（直连模式这儿没有会话 id 在手，只报状态；
                // herdr 那边的会话 id 是可选字段，不带就是不更新它）。
                herdr::report(herdr::HerdrState::Blocked, None, None);
                question_panel::answer(renderer, request, responder, Some(&mut scroll))?;
                herdr::report(herdr::HerdrState::Working, None, None);
                // 问题面板只关闭 raw mode 与括号粘贴，键盘增强仍由外层 LiveRawMode 持有
                enable_live_raw_mode()?;
                execute!(io::stdout(), EnableBracketedPaste)?;
                synchronized_terminal_update(CursorAfterUpdate::Shown, || live.resume())?;
                return live.apply_renderer_frame(renderer);
            }
            live.flush_pending_chunks(renderer)?;
            handle_agent_event(renderer, event)?;
            live.apply_renderer_frame(renderer)?;
            if finishes_external_output {
                live.external_output_active = false;
                synchronized_terminal_update(CursorAfterUpdate::Shown, || live.resume())?;
            }
            Ok(())
        }
    }
}

pub(in crate::cli) async fn run_live_agent_turn(
    live: &mut LiveReplTail,
    paths: &MiyuPaths,
    state: &StateStore,
    agent: &mut Agent,
    input: LiveAgentInput<'_>,
    control: &AgentTurnControl,
    renderer: &mut render::StreamRenderer,
) -> Result<Option<miyu_core::llm::ChatResult>> {
    renderer.use_external_cursor_control();
    renderer.use_buffered_output();
    let mut raw = if std::mem::take(&mut live.raw_mode_handoff) {
        LiveRawMode::adopt()
    } else {
        LiveRawMode::start()?
    };
    live.external_output_active = false;
    if !live.rendered {
        live.resume_at(live.output_cursor)?;
    }
    renderer.start_waiting()?;
    live.apply_renderer_frame(renderer)?;
    // 直连模式（`MIYU_DIRECT=1`，不经 daemon）也上报：留半套的话，同一台机器上
    // 换个跑法侧栏就不动了，查起来比没有还费劲。
    let _herdr_turn = herdr::TurnGuard::begin(&state.session_id());

    let result = {
        let live_cell = std::cell::RefCell::new(&mut *live);
        let renderer_cell = std::cell::RefCell::new(&mut *renderer);
        let chat = agent.chat_stream_with_control(input.content, input.images, control, |event| {
            handle_live_agent_event(
                &mut live_cell.borrow_mut(),
                &mut renderer_cell.borrow_mut(),
                event,
            )
        });
        tokio::pin!(chat);
        let mut input_tick = tokio::time::interval(Duration::from_millis(16));
        input_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        input_tick.tick().await;
        // 本进程的任务表，按这条会话过滤。状态行与浮层上的 x 都用它。
        let jobs_feed = JobsFeed::Local(Some(state.session_id().to_string()));
        let mut strip_tick = 0u32;
        loop {
            tokio::select! {
                biased;
                _ = input_tick.tick() => {
                    if terminal_hangup() {
                        crate::cli::exit_after_terminal_gone(0);
                    }
                    // 状态行跟着任务走，约 130ms 一次（远端回合在转轮 tick 里做同一件事）。
                    // 原来直连回合里一次都不刷：这一轮里新开的后台任务要等说完才上状态行，
                    // 回合中点不开、也停不了它。
                    strip_tick = strip_tick.wrapping_add(1);
                    if strip_tick % 8 == 0 {
                        let mut live = live_cell.borrow_mut();
                        live.expire_hover()?;
                        if !live.external_output_active {
                            if live.set_jobs(jobs_feed.current()) {
                                synchronized_terminal_update(CursorAfterUpdate::Preserve, || {
                                    live.redraw()
                                })?;
                            } else {
                                live.tick_job_strip()?;
                            }
                        }
                    }
                    if !event::poll(Duration::ZERO)? {
                        continue;
                    }
                    // 鼠标事件一次抽干，理由同 `remote/one_shot.rs`：一 tick 一个
                    // 的话，拖动时选区会落在光标后面。
                    let mut pending: Option<Event> = None;
                    while event::poll(Duration::ZERO)? {
                        let next = event::read()?;
                        if matches!(next, Event::Mouse(_)) {
                            live_cell.borrow_mut().handle_screen_event(&next)?;
                            continue;
                        }
                        pending = Some(next);
                        break;
                    }
                    let Some(event) = pending else {
                        continue;
                    };
                    crate::cli::repl::input::hurry_pending_input(&mut input_tick)?;
                    let mut live = live_cell.borrow_mut();
                    if matches!(
                        &event,
                        Event::Key(KeyEvent {
                            code: KeyCode::Enter,
                            kind,
                            ..
                        }) if *kind != KeyEventKind::Release
                    ) && matches!(
                        // 只拦真命令。以前是 `starts_with('/')`,于是
                        // 「/home/x 这是什么」在回合中途按回车既不发送也不排队,
                        // 文本卡在输入框里——用户看到的是「回车没反应」。
                        parse_repl_input(live.editor.input.trim_start()),
                        ReplInput::Slash(..)
                    ) {
                        let ReplInput::Slash(command, args) =
                            parse_repl_input(live.editor.input.trim_start())
                        else {
                            unreachable!("just matched")
                        };
                        // 直连模式（`MIYU_DIRECT=1`）没有 daemon，回合就跑在这
                        // 个进程里，做不了远端那条路的「分离 → 执行 → 挂回来」
                        // （09-20）。所以这里仍然吞掉这次回车——但**把原因说
                        // 出来**：原来是全静默，屏幕上一点反应都没有，用户以为
                        // 回车坏了。输入原样留着，这一轮结束后再回车即可。
                        let reason = miyu_core::slash_commands::during_turn(command, args)
                            .reason()
                            .unwrap_or(t(
                                "commands run after this reply finishes (direct mode)",
                                "直连模式下命令要等这一轮说完才能执行",
                            ));
                        live.toast_note(reason);
                        let _ = synchronized_terminal_update(CursorAfterUpdate::Preserve, || {
                            live.redraw()
                        });
                        continue;
                    }
                    // 浮层开着时按键先给它（Esc 关、翻页、x 停任务），它不要的才进输入框。
                    // 原来直连回合里按键直接进输入框：浮层上按 x 没反应，连按两下 Esc 想关
                    // 浮层反倒把这一轮打断了（远端那条路在 `remote/one_shot.rs` 早就是先给浮层）。
                    if live.handle_screen_event(&event)? {
                        stop_pending_job(paths, &jobs_feed, &mut live).await?;
                        continue;
                    }
                    let mode_before = live.mode();
                    match live.editor.handle_event(event, paths, true)? {
                        LiveEditorAction::None => {}
                        LiveEditorAction::Redraw if !live.external_output_active => {
                            synchronized_terminal_update(CursorAfterUpdate::Preserve, || {
                                live.redraw()
                            })?
                        }
                        // 回合跑着的时候不清屏。
                        //
                        // 全屏下清屏是"把视口顶空"，而正文还在往里写——顶完下一
                        // 帧新内容就接着冒出来，屏幕既没干净也没保住上文，纯粹
                        // 添乱。等它说完再清。
                        LiveEditorAction::ClearScreen if crate::cli::in_fullscreen() => {}
                        LiveEditorAction::Redraw | LiveEditorAction::ClearScreen => {}
                        LiveEditorAction::EmptySubmit => {}
                        LiveEditorAction::Submit(submission) => {
                            let prompt = persist_queued_submission(state, &submission)?;
                            live.editor.record_history(ReplHistoryEntry::from_submission(&submission));
                            if live.external_output_active {
                                live.append_queued(prompt);
                            } else {
                                synchronized_terminal_update(CursorAfterUpdate::Preserve, || {
                                    live.enqueue(prompt)
                                })?;
                            }
                        }
                        LiveEditorAction::Interrupt | LiveEditorAction::Exit => break Ok(None),
                        // 回合跑着的时候会话已经不空了,不会出现;出现也不理。
                        LiveEditorAction::ToggleMode => {}
                        // 直连模式不经 daemon,没有沙盒可切(`direct.rs` 模块头)。
                        LiveEditorAction::ToggleReadonly => {
                            live.toast_note_at(
                                t(
                                    "Direct mode has no read-only mode.",
                                    "直连模式不支持只读。",
                                ),
                                true,
                            );
                        }
                    }
                    if live.mode() != mode_before {
                        control.set_lane(live.mode());
                    }
                },
                result = &mut chat => break result.map(Some),
            }
        }
    };

    if matches!(&result, Ok(None)) {
        live.discard_pending_chunks();
    }
    live.external_output_active = false;
    live.flush_pending_chunks(renderer)?;
    renderer.finish()?;
    live.apply_renderer_frame(renderer)?;
    raw.handoff();
    live.raw_mode_handoff = true;
    result
}
