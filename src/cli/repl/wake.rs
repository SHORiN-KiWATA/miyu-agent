//! 后台任务唤醒的事件泵。
//!
//! 子代理跑完、闹钟到点这类事件会「唤醒」一个回合，daemon 把它的事件流推给
//! 终端。它和普通回合走的是两条独立的泵，各自维护分发表——08-17 那次「回合
//! 中途 footer 不刷新」就是因为只有一个泵接了 `chat.round_usage`，另一个漏了。

use crate::cli::repl::editor::*;
use crate::cli::repl::tail::*;
use crate::cli::*;

/// Attach to a daemon-initiated wake turn and render it live: streaming
/// content, reasoning, and tool activity, exactly like a user-started turn.
/// ESC detaches (the turn keeps running; the DB report is suppressed because
/// the turn was already rendered here). Typed submissions queue into the
/// wake turn as follow-ups.
pub(in crate::cli) async fn follow_wake_run(
    paths: &MiyuPaths,
    live: &mut LiveReplTail,
    run_id: &str,
    label: &str,
    // 把这一轮**从头**补一遍吗。后台唤醒轮是刚刚才起的，接实时就够；
    // 同一个会话的第二个 TUI 挂到一轮**已经在跑**的轮上时要补。
    from_start: bool,
    // 从这个事件号之后接着看。回合中执行斜杠命令后挂回来时给它（09-20）：
    // 已经看过的那半截不能再来一遍。给了它就不看 `from_start`。
    after: Option<u64>,
    // 附着期间静默执行的 `/goal` 要落在**这个 REPL 的会话**上，不能拿
    // daemon 的当前会话指针顶替——普通模式的 REPL 早就有自己的会话了。
    session_id: &str,
    jobs_feed: &JobsFeed,
    jobs_shared: &std::sync::Arc<SharedJobsFeed>,
) -> Result<()> {
    let config = AppConfig::load_or_default(paths)?;
    // 回合中执行完斜杠命令**挂回来**（09-20）：正文本来就在流，这里既不该打
    // 抬头（那不是一件新事），也不该起等待转轮（回放下一帧就接上了，先画一个
    // 转轮就是 09-19 那个「多一个错位的 spinner」）。
    let resuming = after.is_some();
    let mut stream = ipc::connect(&paths.ipc_socket()).await?;
    ipc::send(
        &mut stream,
        &IpcRequest::new(IpcCommand::FollowRun {
            run_id: run_id.to_string(),
            from_start,
            after,
        }),
    )
    .await?;
    // 收帧走带缓冲的读取器：提问面板开着时要靠它往前看一眼事件流（09-24，见
    // `question_flow`）。
    let mut frames = ipc::FrameReader::new(stream);
    let mut turn_id: Option<String> = match frames.receive::<IpcFrame>().await? {
        Some(IpcFrame::Accepted { turn_id, .. }) => turn_id,
        // Run already finished — the DB report path will print it instead.
        _ => return Ok(()),
    };
    // 挂上了就撤大厅：接下来这一轮要往屏幕上写正文，而大厅那层星空是盖在正文
    // 之上的——不撤的话整轮跑完屏幕上还是一片星空（用户 09-19 实测:空会话里
    // 建目标不退大厅）。目标续轮、后台任务的跟进回复都从这儿过，所以放这一处
    // 就够；命令自己那条路另有一处，那儿不必等 daemon 真的起轮。
    live.set_session_empty(&config, paths, false);
    // 这一轮的总计时按它真正开始的时刻起算（09-24）：挂上来之前它可能已经跑了一阵
    // （同一会话的第二个 TUI、回合中执行完命令挂回来）。
    if let Some(started) = turn_id
        .as_deref()
        .and_then(|turn_id| turn_started_instant(paths, turn_id))
    {
        live.set_turn_clock_start(started);
    }

    let mut renderer = render::StreamRenderer::new(
        render::ReasoningDisplayMode::from_expand(config.display.expand_reasoning),
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
    live.external_output_active = false;
    // Print the header straight into the scrollback (not the live frame):
    // it must survive the streaming render that follows.
    {
        // 目标续轮不打表头：一个长任务会连着跑几十轮，每轮顶一行「第 N 轮」
        // 只会把真正的输出挤散。轮次已经在 footer 上（那是它常驻的位置）。
        //
        // 挂到**别人起的轮**上也不打：那不是后台任务完成，是这个会话里另一个
        // 端正在说话。它自己的用户消息会从 `turn.started` 画出来，那才是该有
        // 的抬头（用户 09-19 实测：第二个 TUI 顶上写着「后台任务完成」）。
        let header =
            if from_start || resuming || label == miyu_engine::tools::goal::GOAL_ROUND_LABEL {
                String::new()
            } else if label.is_empty() {
                miyu_base::i18n::text("⚙ background task finished", "⚙ 后台任务完成").to_string()
            } else {
                format!("⚙ {label}")
            };
        if crate::cli::in_fullscreen() {
            // 全屏：表头走缓冲。`suspend` + 直写 stdout + `resume_at` 那条路是
            // inline 的写法——全屏下直写的字节进不了缓冲，而 `resume_at` 会整屏
            // 擦掉重画，开着的浮层跟着闪一下（用户实测：后台任务完成时已经开着
            // 的浮层会鬼畜抖动一下）。
            if !header.is_empty() {
                let glyph = miyu_hosts::render::timeline::glyph_notice();
                let text = header.trim_start_matches('⚙').trim_start();
                let line = miyu_hosts::render::timeline::indent_body(&format!(
                    "\x1b[2m{glyph} {text}\x1b[0m\r\n\r\n"
                ));
                live.apply_output_frame(line.as_bytes())?;
            }
        } else {
            live.suspend()?;
            let mut stdout = io::stdout();
            if !header.is_empty() {
                queue!(stdout, Print(format!("\x1b[2m{header}\x1b[0m\r\n\r\n")))?;
            }
            stdout.flush()?;
            live.output_cursor = cursor_position_or(live.output_cursor);
            let output_cursor = live.output_cursor;
            live.resume_at(output_cursor)?;
        }
    }
    // 挂到**已经在跑**的那一轮上时先别起转轮。
    //
    // 起了的话，它会先画在活动区里，紧接着回放的 `turn.started` 才把用户那句
    // 话写进正文——转轮就被孤零零地留在了用户消息上面（用户 09-19 截图，稳定
    // 复现）。后台唤醒轮那条路没这毛病：它的表头是在起转轮**之前**打的。
    //
    // 回放一来事件就接上了，转轮由渲染器自己按事件带起来；万一回放是空的
    // （那一轮刚好没赶上），下面收到第一条事件后补起一次，不会一直没有。
    // 回合中执行斜杠命令要按这个号挂回来（09-20），和 `one_shot.rs` 同一套。
    let mut last_event_id = after.unwrap_or(0);
    // 点了任务条上的会话行、时间线上子代理那一行，或者方向键停在会话行上回车（会话项目
    // 第 3 段）：这一轮留在 daemon 里接着跑，人切走——和 `/session` 面板挑了别的会话同一
    // 条路。
    macro_rules! suspend_for_strip {
        () => {
            if let Some(action) = live.take_strip_action() {
                renderer.finish()?;
                live.stop_footer_spinner()?;
                live.apply_renderer_frame(&mut renderer)?;
                return Err(anyhow::Error::new(
                    crate::cli::repl::session::RemoteTurnSuspended {
                        action: crate::cli::repl::session::SuspendedAction::Strip(action),
                        run_id: run_id.to_string(),
                        last_event_id,
                        session_id: session_id.to_string(),
                    },
                ));
            }
        };
    }
    let mut waiting_started = false;
    if !from_start && !resuming {
        renderer.start_waiting()?;
        live.apply_renderer_frame(&mut renderer)?;
        waiting_started = true;
    }
    // 跟着别人起的轮（另一个界面、后台任务唤醒）时，侧栏也该显示「在跑」——
    // 这条 REPL 自己没起轮，但屏幕上正在流内容，报 idle 是骗人的。守卫在这段
    // 结束时（跑完 / 脱离 / 中断）报回 idle。
    let herdr_follow = herdr::TurnGuard::begin(session_id);
    let mut raw = LiveRawMode::start()?;

    let mut spinner_tick = tokio::time::interval(Duration::from_millis(33));
    spinner_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    spinner_tick.tick().await;
    let mut input_tick = tokio::time::interval(Duration::from_millis(16));
    input_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    input_tick.tick().await;
    let mut follow_strip_tick: u32 = 0;

    'outer: loop {
        // 装箱而不是 `tokio::pin!`：取到这一帧就把它放掉，分发表里提问那一支
        // 还要借同一个读取器往前看（09-24）。
        let mut recv = Box::pin(frames.receive::<IpcFrame>());
        let frame = loop {
            tokio::select! {
                biased;
                _ = input_tick.tick() => {
                    if terminal_hangup() {
                        // 终端没了但回合是 daemon 的:观众离席,戏照演——和自己
                        // 起的回合那条路(`one_shot.rs`)同一个语义。原先这儿先
                        // 发一条 Cancel 再退,于是「关掉 TUI」就把正跑着的回合
                        // 掐了,重开只剩一句「已中断」(用户 09-21 实测,拍板:
                        // 仅退出 TUI 不该取消)。
                        crate::cli::exit_after_terminal_gone(0);
                    }
                    if !event::poll(Duration::ZERO)? {
                        continue;
                    }
                    let event = event::read()?;
                    crate::cli::repl::input::hurry_pending_input(&mut input_tick)?;
                    // 方向键先看命令候选和任务条（会话项目第 3 段），排在下面拦回车之前：
                    // 任务条上回车是点那一行，不是发消息。
                    if matches!(
                        live.navigate_key(&event)?,
                        crate::cli::repl::tail::Navigated::Done
                    ) {
                        suspend_for_strip!();
                        if !live.external_output_active {
                            synchronized_terminal_update(CursorAfterUpdate::Preserve, || {
                                live.redraw()
                            })?;
                        }
                        continue;
                    }
                    // 斜杠命令在**编辑器处理回车之前**拦：编辑器一旦处理
                    // Enter 就会清空缓冲区，「输入原样留着」就成了空话——
                    // 显示滞留旧文本，下一次按键才暴露缓冲区其实已经空了。
                    if matches!(
                        &event,
                        crossterm::event::Event::Key(crossterm::event::KeyEvent {
                            code: crossterm::event::KeyCode::Enter,
                            kind,
                            ..
                        }) if *kind != crossterm::event::KeyEventKind::Release
                    ) {
                        let line = live.editor.input.trim_start().to_string();
                        match crate::cli::repl::editor::parse_repl_input(&line) {
                            miyu_core::slash_commands::ReplInput::Slash(
                                miyu_core::slash_commands::ReplSlashCommand::Goal,
                                args,
                            ) => {
                                let args = args.trim().to_string();
                                if args == "edit" {
                                    // 原地变身成「/goal edit <当前目标>」，
                                    // 零输出；没有目标就静默吞掉这次回车。
                                    if crate::cli::repl::session::prefill_goal_edit_input(
                                        paths,
                                        Some(session_id),
                                        live,
                                    ) && !live.external_output_active
                                    {
                                        synchronized_terminal_update(
                                            CursorAfterUpdate::Preserve,
                                            || live.redraw(),
                                        )?;
                                    }
                                    continue;
                                }
                                // 静默执行：edit/pause/clear 会让 daemon 掐掉
                                // 当前续轮（edit 随后按新目标重开一轮），流的
                                // 中断与重启本身就是反馈。
                                if let Ok((_, data)) =
                                    crate::cli::repl::session::send_ipc_admin(
                                        paths,
                                        IpcCommand::Goal {
                                            target: miyu_core::ipc::SessionRef::Id {
                                                id: session_id.to_string(),
                                            },
                                            input: args,
                                        },
                                    )
                                    .await
                                {
                                    // 右上角那行提示是这条静默路径唯一的回执：
                                    // 暂停/清掉之后它当场换字，不必等轮询。
                                    let goal = goal_hint_from_admin_data(&data);
                                    jobs_feed.set_goal(goal.clone());
                                    live.tick_goal_hint(goal)?;
                                    // **被拒的那条得说话**：「静默」的前提是命令
                                    // 有可见后果（流被掐断）。`/goal <另一个目标>`
                                    // 撞上已有目标会被拒，流照跑、提示行也不变，
                                    // 不打这一句就真的一点反应都没有（用户 09-19
                                    // 实测）。全屏下 `repl_note` 走的是浮层提示，
                                    // 不碰正在流的那块画面。
                                    let rejected = data
                                        .get("ok")
                                        .and_then(serde_json::Value::as_bool)
                                        .is_some_and(|ok| !ok);
                                    if rejected {
                                        let text = data
                                            .get("text")
                                            .and_then(serde_json::Value::as_str)
                                            .unwrap_or_default();
                                        let line =
                                            text.lines().next().unwrap_or_default().to_string();
                                        if !line.is_empty() {
                                            repl_note(
                                                live,
                                                &format!("\x1b[2m◎ {line}\x1b[0m\n"),
                                            )?;
                                        }
                                    }
                                }
                                live.editor.clear();
                                if !live.external_output_active {
                                    synchronized_terminal_update(
                                        CursorAfterUpdate::Preserve,
                                        || live.redraw(),
                                    )?;
                                }
                                continue;
                            }
                            // 其余命令按「回合中能不能做」分流（用户 09-20），
                            // 和 `one_shot.rs` 那条路同一张表。
                            miyu_core::slash_commands::ReplInput::Slash(command, args) => {
                                use miyu_core::slash_commands::DuringTurn;
                                let verdict =
                                    miyu_core::slash_commands::during_turn(command, args);
                                match verdict {
                                    DuringTurn::Inline => continue,
                                    DuringTurn::Blocked { .. } => {
                                        if let Some(reason) = verdict.reason() {
                                            // 输入原样留着，这一轮说完再回车。
                                            live.toast_note(reason);
                                            if !live.external_output_active {
                                                synchronized_terminal_update(
                                                    CursorAfterUpdate::Preserve,
                                                    || live.redraw(),
                                                )?;
                                            }
                                        }
                                        continue;
                                    }
                                    // 面板寄宿在这个循环里跑（09-20），和
                                    // `one_shot.rs` 那条泵同一份处理。
                                    DuringTurn::Panel if live.screen.is_some() => {
                                        live.editor.clear();
                                        use crate::cli::repl::midturn_panel::{
                                            host_panel, HostedPanel,
                                        };
                                        match host_panel(
                                            paths,
                                            live,
                                            &mut renderer,
                                            command,
                                            session_id,
                                        )
                                        .await?
                                        {
                                            HostedPanel::Stayed => continue,
                                            HostedPanel::SwitchSession(state) => {
                                                renderer.finish()?;
                                                live.stop_footer_spinner()?;
                                                live.apply_renderer_frame(&mut renderer)?;
                                                return Err(anyhow::Error::new(
                                                    crate::cli::repl::session::RemoteTurnSuspended {
                                                        action: crate::cli::repl::session::SuspendedAction::SwitchSession(state),
                                                        run_id: run_id.to_string(),
                                                        last_event_id,
                                                        session_id: session_id.to_string(),
                                                    },
                                                ));
                                            }
                                        }
                                    }
                                    // 行内 REPL 没有面板：`Panel` 退回分离那条路。
                                    DuringTurn::Panel | DuringTurn::Detach => {
                                        let args = args.trim().to_string();
                                        live.editor.clear();
                                        renderer.finish()?;
                                        live.stop_footer_spinner()?;
                                        live.apply_renderer_frame(&mut renderer)?;
                                        return Err(anyhow::Error::new(
                                            crate::cli::repl::session::RemoteTurnSuspended {
                                                action: crate::cli::repl::session::SuspendedAction::Command {
                                                    command,
                                                    args,
                                                },
                                                run_id: run_id.to_string(),
                                                last_event_id,
                                                session_id: session_id.to_string(),
                                            },
                                        ));
                                    }
                                }
                            }
                            miyu_core::slash_commands::ReplInput::Chat => {}
                        }
                    }
                    if live.handle_screen_event(&event)? {
                        // 浮层里按了 x：跟着别处起的回合时也当场停（见 `job_stop`）。
                        stop_pending_job(paths, jobs_feed, live).await?;
                        suspend_for_strip!();
                        continue;
                    }
                    match live.editor.handle_event(event, paths, true)? {
                        LiveEditorAction::None => {}
                        LiveEditorAction::Redraw if !live.external_output_active => {
                            synchronized_terminal_update(CursorAfterUpdate::Preserve, || {
                                live.redraw()
                            })?
                        }
                        LiveEditorAction::Redraw | LiveEditorAction::ClearScreen => {}
                        LiveEditorAction::EmptySubmit => {}
                        LiveEditorAction::Submit(submission) => {
                            // 斜杠命令到不了这里：上面的回车闸在编辑器处理
                            // 之前就拦下了。这里只剩普通消息，照常排队。
                            let Some(target_turn) = turn_id.as_deref() else {
                                continue;
                            };
                            if let Ok(prompt) = persist_remote_queued_submission(
                                paths,
                                run_id,
                                target_turn,
                                &submission,
                            )
                            .await
                            {
                                live.editor.record_history(ReplHistoryEntry::from_submission(&submission));
                                synchronized_terminal_update(
                                    CursorAfterUpdate::Preserve,
                                    || live.enqueue(prompt),
                                )?;
                            }
                        }
                        LiveEditorAction::Interrupt => {
                            // 目标续轮上 Ctrl+C 的意图是「停」，走 WebUI 停止
                            // 按钮同一条取消路径（取消即解除武装）。仅脱离的话，
                            // 续轮在 daemon 里继续跑：用户面对的是一个看起来
                            // 停了、`/goal` 却说「进行中」、还在烧额度的幽灵轮。
                            // 其他后台唤醒保持仅脱离——那些回合不是它发起的。
                            if label == miyu_engine::tools::goal::GOAL_ROUND_LABEL {
                                let _ = send_ipc_command(
                                    paths,
                                    IpcCommand::Cancel {
                                        run_id: run_id.to_string(),
                                    },
                                )
                                .await;
                            }
                            break 'outer;
                        }
                        LiveEditorAction::Exit => {
                            // Detach only: the wake turn keeps running.
                            break 'outer;
                        }
                        LiveEditorAction::ToggleMode => {}
                        LiveEditorAction::ToggleReadonly => {
                            toggle_repl_readonly(paths, live, session_id, true).await?;
                        }
                    }
                }
                frame = &mut recv => break frame?,
                _ = spinner_tick.tick() => {
                    // SpinnerTick 经 live 路径冲刷 chunk 缓冲，流式输出靠它。
                    handle_live_agent_event(live, &mut renderer, AgentEvent::SpinnerTick)?;
                    // 状态条是 live tail 的一部分，附着期间同样要持续刷新。
                    follow_strip_tick = follow_strip_tick.wrapping_add(1);
                    if follow_strip_tick % 2 == 0 && !live.external_output_active {
                        // 目标续轮就是在这条路上跑的：右上角那行 `/goal running
                        // · 第 N 轮 · 12s` 得跟着一起走，不然一附着就冻住了。
                        live.tick_goal_hint(jobs_feed.goal())?;
                        // 同 `one_shot.rs`：指针出了窗口就熄掉提亮。
                        live.expire_hover()?;
                        if live.set_jobs(jobs_feed.current()) {
                            synchronized_terminal_update(CursorAfterUpdate::Preserve, || {
                                live.redraw()
                            })?;
                        } else {
                            live.tick_job_strip()?;
                        }
                    }
                }
            }
        };
        drop(recv);
        if let Some(IpcFrame::Event { id, .. }) = &frame {
            last_event_id = *id;
        }
        let Some(IpcFrame::Event { kind, data, .. }) = frame else {
            break;
        };
        match kind.as_str() {
            "turn.started" => {
                turn_id = Some(ipc_text(&data, "turn_id").to_string());
                // 挂到别人起的轮上时，这是**唯一**能知道「用户说了什么」的
                // 地方：这个 REPL 自己没提交过，画不出那条用户消息（用户
                // 09-19：两个 TUI 该看到一样的内容）。自己起的轮不走这儿，
                // 提交时早就画过了。
                if from_start {
                    let said = ipc_text(&data, "display_content").trim_end().to_string();
                    // 另一个会话里的 AI 发来的那条（09-23）起的这一轮：开头画成
                    // 「从 xxx 收到消息」那一块，不是这个会话里谁说的话。
                    if let Some(message) = miyu_core::state::parse_cross_session_message(&said) {
                        live.show_cross_session_message(
                            &message,
                            config.display.cross_session_preview_lines,
                        )?;
                    } else if let Some(attempt) = miyu_core::state::service_restart_attempt(&said) {
                        // daemon 重启后接着跑的这一轮（09-24）：一行提示，不是谁说的话。
                        live.show_restart_notice(attempt)?;
                    } else if turn_from_parent(paths, turn_id.as_deref()) {
                        // 切进子会话时它正跑着第一轮：开头那句是主会话派的任务（会话项目
                        // 第 3 段），和回放画成同一块。
                        live.show_parent_task(&said, config.display.cross_session_preview_lines)?;
                    } else if !said.trim().is_empty() {
                        let cols = crate::cli::terminal_cols();
                        let mut echo = submitted_echo_lines(live.mode(), &said, cols).join("\r\n");
                        echo.push_str("\r\n\r\n");
                        if crate::cli::in_fullscreen() {
                            live.apply_output_frame(echo.as_bytes())?;
                        } else {
                            live.suspend()?;
                            let mut stdout = io::stdout();
                            queue!(stdout, Print(&echo))?;
                            stdout.flush()?;
                            live.output_cursor = cursor_position_or(live.output_cursor);
                            let output_cursor = live.output_cursor;
                            live.resume_at(output_cursor)?;
                        }
                    }
                    // 用户那句话落进正文了，这时候起转轮才落在它**下面**。
                    if !waiting_started {
                        renderer.start_waiting()?;
                        live.apply_renderer_frame(&mut renderer)?;
                        waiting_started = true;
                    }
                }
            }
            // 事件环追不回这一轮的开头时，daemon 从库里的流水补一份到现在为止的样子
            //（会话项目第 3 段）。按回放那一套画出来，之后的实时事件接着往下写。
            "turn.catchup" => {
                turn_id = Some(ipc_text(&data, "turn_id").to_string());
                let replay = data
                    .get("replay")
                    .cloned()
                    .and_then(|value| serde_json::from_value(value).ok());
                if let Some(replay) = replay {
                    let (cols, _) = crate::cli::history_replay::replay_viewport();
                    let frame = crate::cli::history_replay::session_replay_frame(
                        std::slice::from_ref(&replay),
                        live.mode(),
                        &config,
                        cols,
                        false,
                    )?;
                    live.apply_output_frame(&frame)?;
                }
                if !waiting_started {
                    renderer.start_waiting()?;
                    live.apply_renderer_frame(&mut renderer)?;
                    waiting_started = true;
                }
            }
            "assistant.delta" => handle_live_agent_event(
                live,
                &mut renderer,
                AgentEvent::Chunk(ChatStreamChunk {
                    kind: miyu_core::llm::ChatStreamKind::Content,
                    text: ipc_text(&data, "delta").to_string(),
                }),
            )?,
            "reasoning.delta" => handle_live_agent_event(
                live,
                &mut renderer,
                AgentEvent::Chunk(ChatStreamChunk {
                    kind: miyu_core::llm::ChatStreamKind::Reasoning,
                    text: ipc_text(&data, "delta").to_string(),
                }),
            )?,
            "reasoning.start" => handle_live_agent_event(
                live,
                &mut renderer,
                AgentEvent::ReasoningStart {
                    received_at: Instant::now(),
                },
            )?,
            "reasoning.reset" => handle_live_agent_event(
                live,
                &mut renderer,
                AgentEvent::ReasoningReset {
                    received_at: Instant::now(),
                },
            )?,
            "reasoning.part_start" => handle_live_agent_event(
                live,
                &mut renderer,
                AgentEvent::ReasoningPartStart {
                    received_at: Instant::now(),
                },
            )?,
            "reasoning.part_end" => handle_live_agent_event(
                live,
                &mut renderer,
                AgentEvent::ReasoningPartEnd {
                    received_at: Instant::now(),
                },
            )?,
            "reasoning.title" => handle_live_agent_event(
                live,
                &mut renderer,
                AgentEvent::ReasoningTitle(ipc_text(&data, "title").to_string()),
            )?,
            "tool.preparing" => {
                miyu_hosts::runtime::learn_tool_display_name(&data);
                handle_live_agent_event(
                    live,
                    &mut renderer,
                    AgentEvent::ToolPreparing {
                        name: ipc_text(&data, "name").to_string(),
                        batch: data
                            .get("batch")
                            .and_then(serde_json::Value::as_bool)
                            .unwrap_or(false),
                    },
                )?
            }
            "tool.started" => {
                // 脚本的显示名只有 daemon 知道，事件里带过来，先记下再画。
                miyu_hosts::runtime::learn_tool_display_name(&data);
                handle_live_agent_event(
                    live,
                    &mut renderer,
                    AgentEvent::ToolCall {
                        call_id: ipc_text(&data, "tool_id").to_string(),
                        name: ipc_text(&data, "name").to_string(),
                        arguments: ipc_text(&data, "arguments").to_string(),
                    },
                )?
            }
            "tool.progress" => handle_live_agent_event(
                live,
                &mut renderer,
                AgentEvent::ToolProgress {
                    call_id: ipc_text(&data, "tool_id").to_string(),
                    name: ipc_text(&data, "name").to_string(),
                    message: ipc_text(&data, "message").to_string(),
                },
            )?,
            "tool.output" => handle_live_agent_event(
                live,
                &mut renderer,
                AgentEvent::CommandOutput {
                    call_id: ipc_text(&data, "tool_id").to_string(),
                    name: ipc_text(&data, "name").to_string(),
                    stream: if ipc_text(&data, "stream") == "stderr" {
                        tools::CommandOutputStream::Stderr
                    } else {
                        tools::CommandOutputStream::Stdout
                    },
                    chunk: ipc_text(&data, "output").as_bytes().to_vec(),
                },
            )?,
            "tool.finished" => handle_live_agent_event(
                live,
                &mut renderer,
                AgentEvent::ToolResult {
                    call_id: ipc_text(&data, "tool_id").to_string(),
                    name: ipc_text(&data, "name").to_string(),
                    ok: data
                        .get("ok")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false),
                    output: ipc_text(&data, "output").to_string(),
                },
            )?,
            // shellhook/唤醒形态此前没有这个分支,工具图片(表情包/生图/
            // print_image)在客户端被静默丢弃——REPL 形态(one_shot 事件循环)
            // 一直有,唯独这条流漏了。
            "tool.image" => {
                renderer.prepare_for_external_output()?;
                live.apply_renderer_frame(&mut renderer)?;
                synchronized_terminal_update(CursorAfterUpdate::Hidden, || live.suspend())?;
                live.external_output_active = true;
                let state = StateStore::new(paths)?;
                let size = remote_tool_image_size(
                    ipc_text(&data, "name"),
                    ipc_text(&data, "size"),
                    &config,
                );
                if let Err(error) = render_remote_tool_image(&state, &data, size).await {
                    renderer.write_system_message(&format!(
                        "{}: {error}",
                        t("Could not display tool image", "工具图片显示失败")
                    ))?;
                }
                live.external_output_active = false;
                live.output_cursor = cursor_position_or(live.output_cursor);
                live.resume_at(live.output_cursor)?;
                live.apply_renderer_frame(&mut renderer)?;
            }
            // 别的端往这一轮排了一条消息：画进自己的排队列表，两边看到的队列
            // 才是同一份（用户 09-19：「TUIA 发消息进入排队，TUIB 也能看到」）。
            // 自己排的那条提交时已经画过了，按 prompt_id 去重。
            "queue.added" => {
                let prompt = data.get("prompt").cloned().unwrap_or_default();
                let prompt_id = ipc_text(&prompt, "id").to_string();
                let already = live
                    .queued
                    .iter()
                    .any(|queued| queued.prompt_id == prompt_id);
                if !prompt_id.is_empty() && !already {
                    let content = ipc_text(&prompt, "content").to_string();
                    live.enqueue(miyu_core::state::QueuedPrompt {
                        prompt_id,
                        seq: data
                            .get("seq")
                            .and_then(serde_json::Value::as_i64)
                            .unwrap_or(0),
                        content: content.clone(),
                        display_content: content,
                        attachments: Vec::new(),
                        uploaded_attachments: Vec::new(),
                        submitted_at: ipc_text(&prompt, "submitted_at").to_string(),
                    })?;
                }
            }
            "queue.consumed" => {
                let prompt_ids: Vec<String> = data
                    .get("prompt_ids")
                    .and_then(serde_json::Value::as_array)
                    .map(|values| {
                        values
                            .iter()
                            .filter_map(|value| value.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                let consumed_mode = PersonaLane::from_mode_word(Some(ipc_text(&data, "mode")));
                // 同 `one_shot.rs`：收尾 → 通知行 → 空行。
                let notices = live.take_queued_notices(&prompt_ids);
                let visible = live.has_queued(&prompt_ids);
                if !notices.is_empty() || visible {
                    renderer.prepare_for_external_output()?;
                    live.apply_renderer_frame(&mut renderer)?;
                }
                for notice in &notices {
                    live.show_queued_notice(notice, config.display.cross_session_preview_lines)?;
                }
                if visible {
                    synchronized_terminal_update(CursorAfterUpdate::Preserve, || {
                        live.suspend()?;
                        live.consume_queued(&prompt_ids, consumed_mode)
                    })?;
                }
            }
            // daemon 一直在发这个事件,可这里没有对应分支,于是逐请求的
            // 计量在 IPC 这一段就掉地上了——WebUI 有(它自己解 SSE),终端
            // 直连模式也有(走本地事件),唯独日常的「终端连 daemon」要等整
            // 个回合结束才动。
            "chat.round_usage" => {
                let usage = data.get("usage").cloned().unwrap_or_default();
                // prompt+completion 即该请求结束时的上下文实际占用,与
                // 本地事件那条路取同一个口径。
                let context_tokens = ipc_u64(&usage, "prompt_tokens")
                    .saturating_add(ipc_u64(&usage, "completion_tokens"));
                live.refresh_round_usage(
                    context_tokens,
                    TurnTokens {
                        total: ipc_u64(&data, "turn_total"),
                        prompt: ipc_u64(&data, "turn_prompt"),
                        cache_read: ipc_u64(&data, "turn_cache_read"),
                    },
                    GenerationSpeed {
                        tokens: ipc_u64(&data, "turn_generation_tokens"),
                        millis: ipc_u64(&data, "turn_generation_ms"),
                    },
                )?;
            }
            "generation.superseded" => handle_live_agent_event(
                live,
                &mut renderer,
                AgentEvent::ReasoningReset {
                    received_at: Instant::now(),
                },
            )?,
            // daemon 自己开的轮里模型也会提问（目标续轮最常见）。这一条以前
            // 没接，事件落进底下的 `_ => {}`：面板不弹、没人回答，那一步就停在
            // 「准备问题」上直到回合循环 30 分钟的兜底超时（用户 09-20）。
            // 处理与自己起的那一轮共用一份，见 `question_flow`。
            "question.requested" => {
                crate::cli::repl::question_flow::handle_question_requested(
                    paths,
                    &config,
                    Some(live),
                    &mut renderer,
                    &data,
                    run_id,
                    &mut frames,
                    // 目标续轮 / 后台唤醒里她反问，herdr 侧栏一样要标红——这
                    // 条路原来一个状态都没报（09-20）。
                    Some(&herdr_follow),
                )
                .await?;
            }
            "run.completed" | "run.failed" | "run.cancelled" => {
                break;
            }
            _ => {}
        }
    }

    // Flush chunks still buffered when the terminal frame arrived — the
    // final content burst lands right before run.completed.
    live.flush_pending_chunks(&mut renderer)?;
    renderer.finish()?;
    live.apply_renderer_frame(&mut renderer)?;
    raw.handoff();
    live.raw_mode_handoff = true;
    // Suppress the duplicate DB report for a turn that was rendered live.
    if let Some(turn_id) = turn_id {
        let mut rendered = jobs_shared.rendered_turns.lock().unwrap();
        if rendered.len() >= JOBS_FEED_MARK_LIMIT {
            rendered.clear();
        }
        rendered.insert(turn_id);
    }
    Ok(())
}

/// 这一轮是不是主会话派给子代理的任务（子会话的第一轮）。读不到库就当不是。
fn turn_from_parent(paths: &MiyuPaths, turn_id: Option<&str>) -> bool {
    turn_id.is_some_and(|turn_id| {
        StateStore::new(paths).is_ok_and(|store| store.turn_from_parent(turn_id).unwrap_or(false))
    })
}

/// 库里这一轮开始的时刻，换算成本进程的 `Instant`（计时要的是单调钟）。
fn turn_started_instant(paths: &MiyuPaths, turn_id: &str) -> Option<std::time::Instant> {
    let started = StateStore::new(paths)
        .ok()?
        .turn_started_at(turn_id)
        .ok()
        .flatten()?;
    let started = chrono::DateTime::parse_from_rfc3339(&started).ok()?;
    let ago = (chrono::Utc::now() - started.with_timezone(&chrono::Utc))
        .to_std()
        .unwrap_or_default();
    std::time::Instant::now().checked_sub(ago)
}
