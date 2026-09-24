//! 单次远端回合（`miyu "问题"` 这种用法）。
//!
//! 和 [`super::interactive`] 共用同一套 IPC 事件流，但生命周期完全不同：跑完
//! 就退，不进 REPL 循环，也就不需要活动区与输入编辑那一整套。

use crate::cli::repl::editor::*;
use crate::cli::repl::tail::*;
use crate::cli::*;

/// 收口:这一轮不管怎么结束,footer 的声波都得熄。
///
/// 各个出口里那几处 `stop_footer_spinner()` 不能删——它们要在 `handoff_raw!()`
/// **之前**就把屏幕改对。这里是兜底,补的是「`?` 直接把错误抛出去」那一类:
/// 帧通道断了、IPC 写失败、终端尺寸读不到,都会跳过所有收尾代码,最后一帧
/// 波浪就冻在 footer 上(用户 09-21 报的是模型报错那条,机制是同一个)。
/// 幂等:已经熄过的直接返回,不会重画,也不会在交接之后往屏幕上乱写。
pub(in crate::cli) async fn try_run_remote_chat(
    paths: &MiyuPaths,
    mut live: Option<&mut LiveReplTail>,
    message: &str,
    show_reasoning: Option<bool>,
    plain: bool,
    mode: PersonaLane,
    images: &[Option<miyu_base::clipboard::PastedImage>],
    session_override: Option<String>,
    jobs_feed: Option<&JobsFeed>,
    overrides: Option<miyu_core::ipc::TurnOverrides>,
) -> Result<Option<RemoteTurnSummary>> {
    let outcome = run_remote_chat_inner(
        paths,
        live.as_deref_mut(),
        message,
        show_reasoning,
        plain,
        mode,
        images,
        session_override,
        jobs_feed,
        overrides,
    )
    .await;
    if let Some(live) = live.as_deref_mut() {
        let _ = live.stop_footer_spinner();
    }
    outcome
}

#[allow(clippy::too_many_arguments)]
async fn run_remote_chat_inner(
    paths: &MiyuPaths,
    mut live: Option<&mut LiveReplTail>,
    message: &str,
    show_reasoning: Option<bool>,
    plain: bool,
    mode: PersonaLane,
    images: &[Option<miyu_base::clipboard::PastedImage>],
    session_override: Option<String>,
    jobs_feed: Option<&JobsFeed>,
    overrides: Option<miyu_core::ipc::TurnOverrides>,
) -> Result<Option<RemoteTurnSummary>> {
    let refreshed_paths = if direct_mode_requested() {
        None
    } else {
        // ensure_daemon also restarts a daemon left over from an older build.
        // Re-resolve paths because that shutdown may complete legacy layout migration.
        ipc::ensure_daemon(paths, None).await?;
        Some(MiyuPaths::new()?)
    };
    let paths = refreshed_paths.as_ref().unwrap_or(paths);
    let mut stream = if direct_mode_requested() {
        match ipc::connect(&paths.ipc_socket()).await {
            Ok(stream) => stream,
            Err(_) => return Ok(None),
        }
    } else {
        ipc::connect(&paths.ipc_socket()).await?
    };
    // Turns run in parallel daemon-side: a running turn in this session does
    // not block a new one (the old multi-process placeholder semantics).
    let state_probe = StateStore::new(paths)?;
    // 这一轮跑在哪个会话上：流式渲染中途静默执行 `/goal` 需要它。
    let turn_session_id = session_override
        .clone()
        .unwrap_or_else(|| state_probe.session_id().to_string());
    let state_probe = session_override
        .as_deref()
        .map(|session_id| state_probe.pinned(session_id))
        .unwrap_or(state_probe);
    ipc::send(
        &mut stream,
        &IpcRequest::new(IpcCommand::StartTurn {
            content: message.to_string(),
            mode: ipc_mode_name(mode).to_string(),
            images: ipc_images(images),
            cwd: std::env::current_dir().ok(),
            session_id: session_override,
            // REPL 常驻连接,后台任务有自己的 FollowWake 通道;只有阅后即焚的
            // 单次/shellhook 触发才需要记下终端供 daemon 回写。
            origin_tty: if live.is_none() {
                detect_origin_tty()
            } else {
                None
            },
            overrides,
        }),
    )
    .await?;
    // 收帧走带缓冲的读取器：提问面板开着时要靠它往前看一眼事件流（09-24，见
    // `question_flow`）。
    let mut frames = ipc::FrameReader::new(stream);
    let Some(first) = frames.receive::<IpcFrame>().await? else {
        bail!("Miyu core closed the connection before accepting the turn");
    };
    // `TurnUpdateAccepted` = 这个会话已经有一轮在跑（另一个 TUI、另一个终端），
    // daemon 把这条消息**排进了那一轮**而不是并行起一轮。接下来推的是那一轮的
    // 事件，照常渲染；人得知道自己是在排队，不然会以为消息发丢了。
    let mut queued_into_running = false;
    let run_id = match first {
        IpcFrame::Accepted { run_id, .. } => run_id,
        IpcFrame::TurnUpdateAccepted { run_id, .. } => {
            queued_into_running = true;
            run_id
        }
        IpcFrame::Error { message, .. } => bail!("{message}"),
        _ => bail!("Miyu core returned an invalid response"),
    };
    if queued_into_running && live.is_none() {
        // 一次性/shellhook：没有活动区可以挂排队条，直说一行。REPL 那边
        // `queue.added` 会把它画进排队列表里，不必再打字。
        println!(
            "\x1b[2m{}\x1b[0m",
            t(
                "queued into the conversation already in progress",
                "已排进正在进行的对话"
            )
        );
    }
    // 记下「这一轮是我起的」。回合结束到 daemon 把它从活跃表里摘掉之间有个
    // 窗口，不记的话空闲循环会把自己刚跑完的那一轮当成「别人的」再画一遍。
    if let Some(jobs_feed) = jobs_feed {
        jobs_feed.mark_own_run(&run_id);
    }
    // 在 herdr 里跑的话，侧栏那行跟着这一轮亮起来（不在就是 no-op）。
    // 守卫负责收口：这个函数有九条出口，Drop 保证哪条走都报回 idle。
    // 先记下来：`live` 后面会被部分移动，那之后问不了它。
    let interactive_repl = live.is_some();
    let herdr_turn = if interactive_repl {
        herdr::TurnGuard::begin(&turn_session_id)
    } else {
        // 一次性 / shellhook：跑完进程就没了，收尾要把 pane 还回去。
        herdr::TurnGuard::begin_transient(&turn_session_id)
    };
    let mut turn_id: Option<String> = None;

    let config = AppConfig::load_or_default(paths)?;
    let reasoning_mode = if show_reasoning == Some(false) {
        render::ReasoningDisplayMode::Hidden
    } else {
        render::ReasoningDisplayMode::from_expand(config.display.expand_reasoning)
    };
    let tool_call_mode = if plain {
        render::ToolCallDisplayMode::Hidden
    } else {
        render::ToolCallDisplayMode::from_expand(config.display.expand_tool_calls)
    };
    let mut renderer = render::StreamRenderer::new(
        reasoning_mode,
        tool_call_mode,
        plain,
        config.display.readable_tool_names,
        config.display.command_output_lines,
    );
    renderer.fold_timeline = config.display.fold_timeline;
    renderer.thinking_scroll_lines = config.display.thinking_scroll_lines;
    renderer.cross_session_preview_lines = config.display.cross_session_preview_lines;
    let queue_state = Some(state_probe);
    if let Some(live) = live.as_deref_mut() {
        // 后台任务面板也跟着这两个开关走。每轮交一次：它和渲染器读的是同一份
        // 配置，节奏也该一样。
        live.set_display_expand(
            config.display.expand_reasoning,
            config.display.expand_tool_calls,
            config.display.fold_timeline,
            config.display.command_output_lines,
            config.display.thinking_scroll_lines,
        );
        renderer.use_external_cursor_control();
        renderer.use_buffered_output();
        live.external_output_active = false;
        if !live.rendered {
            live.resume_at(live.output_cursor)?;
        }
    }
    // Keep the terminal in raw mode during the turn so the editor stays
    // interactive: typed input is queued for the running turn, mirroring the
    // direct REPL's input pump.
    let mut raw = match live.as_deref_mut() {
        Some(live) => Some(if std::mem::take(&mut live.raw_mode_handoff) {
            LiveRawMode::adopt()
        } else {
            LiveRawMode::start()?
        }),
        None => None,
    };

    // 这一轮**怎么结束都**要把终端模式交给下一段，不能让守卫在半路 drop。
    //
    // drop 会关掉 raw、收回括号粘贴与焦点上报、并**弹出键盘增强协议**；
    // 紧接着编辑器那边又推回去。在这一来一回之间按下的键，终端按旧协议发、
    // crossterm 按新协议解，解不出来就当普通字符塞进输入框——用户看到的
    // 「Ctrl+C 之后输入框里冒出代表按键的怪字符」就是它。正常收尾的那条路
    // 一直是交接的，几条提前 return 漏了。
    macro_rules! handoff_raw {
        () => {
            if let Some(raw) = raw.as_mut() {
                if let Some(live) = live.as_deref_mut() {
                    raw.handoff();
                    live.raw_mode_handoff = true;
                }
            }
        };
    }
    renderer.start_waiting()?;
    if let Some(live) = live.as_deref_mut() {
        live.apply_renderer_frame(&mut renderer)?;
    }
    let mut content = String::new();
    let mut reasoning = String::new();
    // 最后看到的事件号。回合中执行斜杠命令走「分离 → 执行 → 挂回来」，挂回来
    // 时从它之后接着看，已经看过的那半截不会再来一遍（09-20）。
    let mut last_event_id = 0u64;
    // 点了任务条上的会话行、或者时间线上子代理那一行（会话项目第 3 段）：这一轮留在
    // daemon 里接着跑，人切走——和 `/session` 面板挑了别的会话同一条路。鼠标事件在
    // 下面那一批里就抽干了，走不到按键那条路，两处都要看一眼。
    macro_rules! suspend_for_strip {
        ($live_tail:ident) => {
            if let Some(action) = $live_tail.take_strip_action() {
                renderer.finish()?;
                $live_tail.stop_footer_spinner()?;
                $live_tail.apply_renderer_frame(&mut renderer)?;
                handoff_raw!();
                return Err(anyhow::Error::new(RemoteTurnSuspended {
                    action: SuspendedAction::Strip(action),
                    run_id: run_id.clone(),
                    last_event_id,
                    session_id: turn_session_id.clone(),
                }));
            }
        };
    }
    // 攒着还没打的图（非全屏那条路）。见 `tool.image` / `tool.finished`。
    let mut deferred_images: Vec<(serde_json::Value, Option<String>)> = Vec::new();
    let mut spinner_tick = tokio::time::interval(Duration::from_millis(33));
    let mut job_strip_tick: u32 = 0;
    spinner_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    spinner_tick.tick().await;
    let mut input_tick = tokio::time::interval(Duration::from_millis(16));
    input_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    input_tick.tick().await;
    let completion = loop {
        // 收帧的 future 跨 select 的每一拍都留着（读取器本身也经得起中途放弃，
        // 半截帧留在它的缓冲里）。装箱而不是 `tokio::pin!`：取到这一帧就把它
        // 放掉，分发表里提问那一支还要借同一个读取器往前看（09-24）。
        let mut recv = Box::pin(frames.receive::<IpcFrame>());
        let frame = loop {
            tokio::select! {
                biased;
                _ = input_tick.tick(), if live.is_some() => {
                    if terminal_hangup() {
                        // 终端没了但回合是 daemon 的:观众离席,戏照演。
                        crate::cli::exit_after_terminal_gone(0);
                    }
                    if !event::poll(Duration::ZERO)? {
                        continue;
                    }
                    // 鼠标事件在这儿**一次抽干**，别排队。
                    //
                    // 这条泵每 16ms 只取一个事件，而拖一下鼠标一秒能发上百个：
                    // 队列越积越长，选区落在光标后面好几百毫秒——用户原话
                    // 「AI 输出时选文字发涩，输出一停就没事了」。一停就没事是因为
                    // 那时走的是空闲循环，那边本来就一次把就绪事件全抽干。
                    //
                    // 抽干之后把**第一个非鼠标事件**交给下面那条老路，语义不变。
                    let mut pending: Option<Event> = None;
                    while event::poll(Duration::ZERO)? {
                        let next = event::read()?;
                        if matches!(next, Event::Mouse(_)) {
                            if let Some(live_tail) = live.as_deref_mut() {
                                live_tail.handle_screen_event(&next)?;
                                suspend_for_strip!(live_tail);
                            }
                            continue;
                        }
                        pending = Some(next);
                        break;
                    }
                    let Some(event) = pending else {
                        continue;
                    };
                    crate::cli::repl::input::hurry_pending_input(&mut input_tick)?;
                    let Some(live_tail) = live.as_deref_mut() else {
                        continue;
                    };
                    // 方向键先看命令候选和任务条（会话项目第 3 段），排在下面拦回车之前：
                    // 任务条上回车是点那一行，不是发消息。
                    if matches!(
                        live_tail.navigate_key(&event)?,
                        crate::cli::repl::tail::Navigated::Done
                    ) {
                        suspend_for_strip!(live_tail);
                        if !live_tail.external_output_active {
                            synchronized_terminal_update(CursorAfterUpdate::Preserve, || {
                                live_tail.redraw()
                            })?;
                        }
                        continue;
                    }
                    if matches!(
                        &event,
                        Event::Key(KeyEvent {
                            code: KeyCode::Enter,
                            kind,
                            ..
                        }) if *kind != KeyEventKind::Release
                    ) {
                        // 斜杠命令在编辑器处理回车之前拦（编辑器一处理就会
                        // 清空缓冲区）。流式渲染中间不打任何本地输出。
                        let line = live_tail.editor.input.trim_start().to_string();
                        match parse_repl_input(&line) {
                            ReplInput::Slash(
                                miyu_core::slash_commands::ReplSlashCommand::Goal,
                                args,
                            ) => {
                                let args = args.trim().to_string();
                                if args == "edit" {
                                    // 原地变身成「/goal edit <当前目标>」；
                                    // 没有目标就静默吞掉这次回车。
                                    if super::super::session::prefill_goal_edit_input(
                                        paths,
                                        Some(turn_session_id.as_str()),
                                        live_tail,
                                    ) && !live_tail.external_output_active
                                    {
                                        synchronized_terminal_update(
                                            CursorAfterUpdate::Preserve,
                                            || live_tail.redraw(),
                                        )?;
                                    }
                                    continue;
                                }
                                // 静默执行；后果由 daemon 体现（edit/pause
                                // 会掐掉正在跑的续轮）。
                                let _ = super::super::session::send_ipc_admin(
                                    paths,
                                    IpcCommand::Goal {
                                        target: miyu_core::ipc::SessionRef::Id {
                                            id: turn_session_id.clone(),
                                        },
                                        input: args,
                                    },
                                )
                                .await;
                                live_tail.editor.clear();
                                if !live_tail.external_output_active {
                                    synchronized_terminal_update(
                                        CursorAfterUpdate::Preserve,
                                        || live_tail.redraw(),
                                    )?;
                                }
                                continue;
                            }
                            // 其余命令按「回合中能不能做」分流（用户 09-20：
                            // 「应该区分能执行和不能执行的命令」）。原来这里
                            // 一律**静默**吞掉，屏幕上一点反应都没有。
                            ReplInput::Slash(command, args) => {
                                use miyu_core::slash_commands::DuringTurn;
                                match miyu_core::slash_commands::during_turn(command, args) {
                                    DuringTurn::Inline => continue,
                                    DuringTurn::Blocked { .. } => {
                                        if let Some(reason) = miyu_core::slash_commands::during_turn(
                                            command, args,
                                        )
                                        .reason()
                                        {
                                            // 输入原样留着：这一轮说完再回车
                                            // 就能执行，不用重打。
                                            //
                                            // 走**右上角**那条通知带，不是输入
                                            // 框旁边：命令候选面板就浮在输入框
                                            // 上方，放那儿会被它盖掉（09-20
                                            // 实测，屏幕上只看得到候选面板）。
                                            live_tail.toast_note(reason);
                                            if !live_tail.external_output_active {
                                                synchronized_terminal_update(
                                                    CursorAfterUpdate::Preserve,
                                                    || live_tail.redraw(),
                                                )?;
                                            }
                                        }
                                        continue;
                                    }
                                    // 面板寄宿在这个循环里跑（09-20）：不分离、
                                    // 不换渲染器，事件在 socket 里排队，面板收掉
                                    // 接着画——同一段思考不会被切成几行小结。
                                    // 只有 `/session` 挑了别的会话才借暂离那条路
                                    // 把结果带回 `RemoteRepl`。
                                    DuringTurn::Panel if live_tail.screen.is_some() => {
                                        live_tail.editor.clear();
                                        use crate::cli::repl::midturn_panel::{
                                            host_panel, HostedPanel,
                                        };
                                        match host_panel(
                                            paths,
                                            live_tail,
                                            &mut renderer,
                                            command,
                                            &turn_session_id,
                                        )
                                        .await?
                                        {
                                            HostedPanel::Stayed => continue,
                                            HostedPanel::SwitchSession(state) => {
                                                renderer.finish()?;
                                                live_tail.stop_footer_spinner()?;
                                                live_tail.apply_renderer_frame(&mut renderer)?;
                                                handoff_raw!();
                                                return Err(anyhow::Error::new(
                                                    RemoteTurnSuspended {
                                                        action: SuspendedAction::SwitchSession(
                                                            state,
                                                        ),
                                                        run_id: run_id.clone(),
                                                        last_event_id,
                                                        session_id: turn_session_id.clone(),
                                                    },
                                                ));
                                            }
                                        }
                                    }
                                    // 行内 REPL 没有面板：`Panel` 退回分离那条路。
                                    DuringTurn::Panel | DuringTurn::Detach => {
                                        let args = args.trim().to_string();
                                        live_tail.editor.clear();
                                        // 和 Ctrl+D 那条路同一套收尾：渲染器
                                        // 收口、把帧落到屏幕上、交接 raw 模式。
                                        renderer.finish()?;
                                        live_tail.stop_footer_spinner()?;
                                        live_tail.apply_renderer_frame(&mut renderer)?;
                                        handoff_raw!();
                                        return Err(anyhow::Error::new(RemoteTurnSuspended {
                                            action: SuspendedAction::Command { command, args },
                                            run_id: run_id.clone(),
                                            last_event_id,
                                            session_id: turn_session_id.clone(),
                                        }));
                                    }
                                }
                            }
                            ReplInput::Chat => {}
                        }
                    }
                    if live_tail.handle_screen_event(&event)? {
                        // 浮层里按了 x：回合跑着也当场停（见 `job_stop`）。
                        if let Some(feed) = jobs_feed {
                            stop_pending_job(paths, feed, live_tail).await?;
                        }
                        suspend_for_strip!(live_tail);
                        continue;
                    }
                    match live_tail.editor.handle_event(event, paths, true)? {
                        LiveEditorAction::None => {}
                        LiveEditorAction::Redraw if !live_tail.external_output_active => {
                            synchronized_terminal_update(CursorAfterUpdate::Preserve, || {
                                live_tail.redraw()
                            })?
                        }
                        // 回合跑着的时候不清屏。
                        //
                        // 全屏下清屏是"把视口顶空"，而正文还在往里写——顶完下一
                        // 帧新内容就接着冒出来，屏幕既没干净也没保住上文。
                        //（直连那条路在 `live_turn.rs` 里同样拦了一道；daemon 这条
                        // 才是全屏平时走的，上一轮只改了那边等于没改。）
                        LiveEditorAction::ClearScreen if crate::cli::in_fullscreen() => {}
                        LiveEditorAction::ClearScreen if !live_tail.external_output_active => {
                            synchronized_terminal_update(CursorAfterUpdate::Preserve, || {
                                live_tail.clear_screen()
                            })?
                        }
                        LiveEditorAction::Redraw | LiveEditorAction::ClearScreen => {}
                        LiveEditorAction::EmptySubmit => {}
                        LiveEditorAction::Submit(submission) => {
                            // 斜杠命令到不了这里：回车闸在编辑器处理之前就
                            // 拦下了。这里只剩普通消息，照常排队。
                            let Some(target_turn_id) = turn_id.as_deref() else {
                                live_tail.editor.input = submission.display_content.clone();
                                live_tail.editor.cursor = live_tail.editor.input.chars().count();
                                renderer.write_system_message(t(
                                    "the reply is still starting; try sending the follow-up again",
                                    "当前回复仍在启动，请稍后重新发送追加消息",
                                ))?;
                                live_tail.apply_renderer_frame(&mut renderer)?;
                                continue;
                            };
                            match persist_remote_queued_submission(
                                paths,
                                &run_id,
                                target_turn_id,
                                &submission,
                            ).await {
                                Ok(prompt) => {
                                    live_tail.editor.record_history(ReplHistoryEntry::from_submission(&submission));
                                    if live_tail.external_output_active {
                                        live_tail.append_queued(prompt);
                                    } else {
                                        synchronized_terminal_update(
                                            CursorAfterUpdate::Preserve,
                                            || live_tail.enqueue(prompt),
                                        )?;
                                    }
                                }
                                Err(_) => {
                                    live_tail.editor.input =
                                        submission.display_content.clone();
                                    live_tail.editor.cursor =
                                        live_tail.editor.input.chars().count();
                                    renderer.write_system_message(t(
                                        "could not queue the message; the reply may have just finished",
                                        "无法排队消息；当前回复可能刚刚结束",
                                    ))?;
                                    live_tail.apply_renderer_frame(&mut renderer)?;
                                }
                            }
                        }
                        LiveEditorAction::Interrupt => {
                            let _ = send_ipc_command(
                                paths,
                                IpcCommand::Cancel { run_id: run_id.clone() },
                            )
                            .await;
                        }
                        LiveEditorAction::ToggleMode => {}
                        LiveEditorAction::ToggleReadonly => {
                            toggle_repl_readonly(paths, live_tail, &turn_session_id, true).await?;
                        }
                        LiveEditorAction::Exit => {
                            renderer.finish()?;
                            if let Some(live) = live.as_deref_mut() {
                                live.stop_footer_spinner()?;
                                live.apply_renderer_frame(&mut renderer)?;
                            }
                            handoff_raw!();
                            return Err(anyhow::Error::new(RemoteTurnDetached));
                        }
                    }
                },
                frame = &mut recv => break frame?,
                _ = spinner_tick.tick() => {
                    // 外部输出期间活动区是挂起的(rendered=false),这时
                    // apply_output_frame 会把帧直接写在光标当下的位置——
                    // 也就是工具刚打完的图片中间。文本只盖住左边一截,右
                    // 边残留的 Kitty 占位字符继续渲染对应那行的图片切片,
                    // 屏幕上就是一条条横带。
                    //
                    // 进程内那条路(handle_live_agent_event)早就把外部输
                    // 出期间的 SpinnerTick 整个丢掉了,远端这条漏了。丢掉
                    // 不会少画:tool.finished 会先 resume_at 再统一出帧。
                    if live
                        .as_deref()
                        .is_some_and(|live| live.external_output_active)
                    {
                        continue;
                    }
                    renderer.tick_spinner()?;
                    if let Some(live) = live.as_deref_mut() {
                        live.apply_renderer_frame(&mut renderer)?;
                        // footer 里的运行转轮与等待动画同源推进。
                        live.tick_footer_spinner()?;
                        // The job strip is part of the live tail, so it keeps
                        // rendering during streaming. 转轮按时间定帧，这里每隔一个
                        // 转轮 tick（约 66ms）重画一次就跟得上 80ms 一帧。
                        if let Some(feed) = jobs_feed {
                            job_strip_tick = job_strip_tick.wrapping_add(1);
                            // 指针出了窗口就熄掉提亮——回合跑着的时候也得管。
                            live.expire_hover()?;
                            if job_strip_tick % 2 == 0 && !live.external_output_active {
                                if live.set_jobs(feed.current()) {
                                    synchronized_terminal_update(
                                        CursorAfterUpdate::Preserve,
                                        || live.redraw(),
                                    )?;
                                } else {
                                    live.tick_job_strip()?;
                                }
                            }
                        }
                    }
                }
                _ = tokio::signal::ctrl_c() => {
                    let _ = send_ipc_command(
                        paths,
                        IpcCommand::Cancel { run_id: run_id.clone() },
                    ).await;
                    renderer.finish()?;
                    if let Some(live) = live.as_deref_mut() {
                        live.apply_renderer_frame(&mut renderer)?;
                    }
                    handoff_raw!();
                    return Err(anyhow::Error::new(RemoteTurnCancelled::default()));
                }
            }
        };
        drop(recv);
        let Some(frame) = frame else {
            renderer.finish()?;
            if let Some(live) = live.as_deref_mut() {
                live.apply_renderer_frame(&mut renderer)?;
            }
            handoff_raw!();
            bail!("Miyu core disconnected during the turn");
        };
        if let IpcFrame::Event { id, .. } = &frame {
            last_event_id = *id;
        }
        let IpcFrame::Event { kind, data, .. } = frame else {
            if let IpcFrame::Error { message, .. } = frame {
                renderer.finish()?;
                if let Some(live) = live.as_deref_mut() {
                    live.apply_renderer_frame(&mut renderer)?;
                }
                bail!("{message}");
            }
            continue;
        };
        match kind.as_str() {
            "turn.started" => {
                let id = ipc_text(&data, "turn_id");
                if !id.is_empty() {
                    turn_id = Some(id.to_string());
                }
            }
            "assistant.delta" => {
                let delta = ipc_text(&data, "delta");
                content.push_str(delta);
                handle_agent_event(
                    &mut renderer,
                    AgentEvent::Chunk(ChatStreamChunk {
                        kind: miyu_core::llm::ChatStreamKind::Content,
                        text: delta.to_string(),
                    }),
                )?;
            }
            "reasoning.delta" => {
                let delta = ipc_text(&data, "delta");
                reasoning.push_str(delta);
                handle_agent_event(
                    &mut renderer,
                    AgentEvent::Chunk(ChatStreamChunk {
                        kind: miyu_core::llm::ChatStreamKind::Reasoning,
                        text: delta.to_string(),
                    }),
                )?;
            }
            "reasoning.start" => handle_agent_event(
                &mut renderer,
                AgentEvent::ReasoningStart {
                    received_at: Instant::now(),
                },
            )?,
            "reasoning.reset" => {
                reasoning.clear();
                handle_agent_event(
                    &mut renderer,
                    AgentEvent::ReasoningReset {
                        received_at: Instant::now(),
                    },
                )?;
            }
            "reasoning.part_start" => handle_agent_event(
                &mut renderer,
                AgentEvent::ReasoningPartStart {
                    received_at: Instant::now(),
                },
            )?,
            "reasoning.part_end" => handle_agent_event(
                &mut renderer,
                AgentEvent::ReasoningPartEnd {
                    received_at: Instant::now(),
                },
            )?,
            "reasoning.title" => handle_agent_event(
                &mut renderer,
                AgentEvent::ReasoningTitle(ipc_text(&data, "title").to_string()),
            )?,
            "tool.preparing" => {
                miyu_hosts::runtime::learn_tool_display_name(&data);
                handle_agent_event(
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
                handle_agent_event(
                    &mut renderer,
                    AgentEvent::ToolCall {
                        call_id: ipc_text(&data, "tool_id").to_string(),
                        name: ipc_text(&data, "name").to_string(),
                        arguments: ipc_text(&data, "arguments").to_string(),
                    },
                )?
            }
            "tool.progress" => handle_agent_event(
                &mut renderer,
                AgentEvent::ToolProgress {
                    call_id: ipc_text(&data, "tool_id").to_string(),
                    name: ipc_text(&data, "name").to_string(),
                    message: ipc_text(&data, "message").to_string(),
                },
            )?,
            "tool.output" => handle_agent_event(
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
            "tool.finished" => {
                handle_agent_event(
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
                )?;
                // 这一步已经落地（静态面当场写进正文，折叠面收段时进
                // `Worked for`），现在才轮到它打出来的图。
                let finished_tool = ipc_text(&data, "tool_id").to_string();
                let (mine, rest): (Vec<_>, Vec<_>) = deferred_images
                    .drain(..)
                    .partition(|(image, _)| ipc_text(image, "tool_id") == finished_tool);
                deferred_images = rest;
                let state = queue_state
                    .as_ref()
                    .expect("queue state exists for a remote turn");
                print_deferred_images(mine, state, &mut renderer, live.as_deref_mut()).await?;
                if let Some(live) = live.as_deref_mut() {
                    if live.external_output_active {
                        live.external_output_active = false;
                        live.output_cursor = cursor_position_or(live.output_cursor);
                        live.resume_at(live.output_cursor)?;
                        live.apply_renderer_frame(&mut renderer)?;
                    }
                }
            }
            "tool.image" => {
                let state = queue_state
                    .as_ref()
                    .expect("queue state exists for a remote turn");
                let size = remote_tool_image_size(
                    ipc_text(&data, "name"),
                    ipc_text(&data, "size"),
                    &config,
                );
                // 全屏：图片得**进缓冲**才留得住。传输段发给终端（它要收像素），
                // 占位格当普通文字进正文，于是重画、回翻都还在。
                let fullscreen = live.as_deref().is_some_and(|live| live.screen.is_some());
                image_trace(&format!(
                    "tool.image tool_id={} fullscreen={fullscreen} asset={} error={}",
                    ipc_text(&data, "tool_id"),
                    remote_tool_image_asset_id(&data).unwrap_or("-"),
                    ipc_text(&data, "error"),
                ));
                if fullscreen {
                    match remote_tool_image_parts(state, &data, size).await {
                        Ok((transfer, placeholder)) => {
                            image_trace(&format!(
                                "fullscreen parts transfer={}B placeholder={}B",
                                transfer.len(),
                                placeholder.len()
                            ));
                            // 传输段随时可以发：`U=1` 是虚拟放置，画在哪儿由
                            // 占位格说了算，和它什么时候到终端无关。
                            if !transfer.is_empty() {
                                use std::io::Write as _;
                                let mut stdout = std::io::stdout();
                                write!(stdout, "{transfer}")?;
                                stdout.flush()?;
                            }
                            // 占位格排到**时间线收完之后**：图是"这一步干出来的
                            // 结果"，不是过程。就地写的话，发图那一步自己反而排到
                            // 图下面去了（用户实测的表情包/搜图顺序错乱）。
                            // 缩进两格：它是正文的一部分，得在装订边上。
                            // 上下那两行空不在这儿补：`flush_after_timeline`
                            // 出上面那行、收段出下面那行。这儿自己再补一个
                            // `\n`，图下面就空两行了（用户 09-19）。
                            renderer.queue_after_timeline(
                                miyu_hosts::render::timeline::indent_body(&placeholder),
                            );
                            if let Some(live) = live.as_deref_mut() {
                                live.apply_renderer_frame(&mut renderer)?;
                            }
                        }
                        Err(error) => {
                            image_trace(&format!("fullscreen parts failed: {error}"));
                            renderer.write_system_message(&format!(
                                "{}: {error}",
                                t("Could not display tool image", "工具图片显示失败")
                            ))?;
                            if let Some(live) = live.as_deref_mut() {
                                live.apply_renderer_frame(&mut renderer)?;
                            }
                        }
                    }
                    continue;
                }
                // 非全屏：也要排到**这一步落地之后**再打。
                //
                // `tool.image` 是工具一开头就报的（它得先把图交出去，才谈得上
                // 打），当场打的话「表情包 · 67ms」那一行反而排到图下面——读起
                // 来不像「用了表情包工具，于是图出来了」（用户 09-19 截图）。
                // 全屏那条路早就把占位格排到时间线之后了，这里把顺序对齐：攒
                // 着，等这把工具的 `tool.finished` 到了再打。
                let _ = state;
                deferred_images.push((data.clone(), size));
            }
            "question.requested" => {
                // 她反问了：herdr 侧栏把整条 tab / workspace 标红，人在别的
                // pane 干活时余光就知道「这儿在等我回话」；答完报回 working。
                // 两件事都在 `question_flow` 里做（那边有 RAII 兜住所有出口）。
                crate::cli::repl::question_flow::handle_question_requested(
                    paths,
                    &config,
                    live.as_deref_mut(),
                    &mut renderer,
                    &data,
                    &run_id,
                    &mut frames,
                    Some(&herdr_turn),
                )
                .await?;
            }
            // 别的端往这一轮排了一条消息：画进自己的排队列表，两边看到的队列
            // 才是同一份（用户 09-19：「TUIA 发消息进入排队，TUIB 也能看到」）。
            // 自己排的那条提交时已经画过了，按 prompt_id 去重。
            "queue.added" => {
                let Some(live) = live.as_deref_mut() else {
                    continue;
                };
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
                if let Some(live) = live.as_deref_mut() {
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
                    // 后台任务的报告不是「谁说了句话」：这一轮先收成
                    // `Worked for …`，底下报一行「命令完成 …」，再空一行接着
                    // 说（用户 09-21 看过实际效果定的版式）。原来它被画成粉色
                    // 用户气泡、还带着内部抬头 `[后台任务完成]`。
                    let notices = live.take_queued_notices(&prompt_ids);
                    let visible = live.has_queued(&prompt_ids);
                    if !notices.is_empty() || visible {
                        renderer.prepare_for_external_output()?;
                        live.apply_renderer_frame(&mut renderer)?;
                    }
                    for notice in &notices {
                        live.show_queued_notice(
                            notice,
                            config.display.cross_session_preview_lines,
                        )?;
                    }
                    if visible {
                        synchronized_terminal_update(CursorAfterUpdate::Preserve, || {
                            live.suspend()?;
                            live.consume_queued(&prompt_ids, consumed_mode)
                        })?;
                    }
                }
            }
            "queue.removed" => {
                if let Some(live) = live.as_deref_mut() {
                    if let Some(prompt_id) =
                        data.get("prompt_id").and_then(serde_json::Value::as_str)
                    {
                        synchronized_terminal_update(CursorAfterUpdate::Preserve, || {
                            live.drop_queued(&[prompt_id.to_string()])
                        })?;
                    }
                }
            }
            "generation.superseded" => {
                content.clear();
                reasoning.clear();
                handle_agent_event(
                    &mut renderer,
                    AgentEvent::ReasoningReset {
                        received_at: Instant::now(),
                    },
                )?;
            }
            "context.compact_start" => handle_agent_event(&mut renderer, AgentEvent::CompactStart)?,
            "context.compact_delta" => handle_agent_event(
                &mut renderer,
                AgentEvent::CompactChunk(ChatStreamChunk {
                    kind: miyu_core::llm::ChatStreamKind::Content,
                    text: ipc_text(&data, "delta").to_string(),
                }),
            )?,
            "context.compact_end" => handle_agent_event(&mut renderer, AgentEvent::CompactEnd)?,
            "context.pop_start" => handle_agent_event(&mut renderer, AgentEvent::PopStart)?,
            "context.pop_end" => handle_agent_event(&mut renderer, AgentEvent::PopEnd)?,
            "context.notice" => handle_agent_event(
                &mut renderer,
                AgentEvent::Notice {
                    text: ipc_text(&data, "text").to_string(),
                },
            )?,
            // daemon 每完成一次模型请求就发这个,可这里没有对应分支,于是
            // 逐请求的计量在 IPC 这一段掉地上——回合跑在 daemon 里,CLI 拿
            // 不到就只能等 run.completed 的权威数字,footer 因此整轮不动。
            // WebUI 没这问题:它自己解 SSE。
            "chat.round_usage" => {
                if let Some(live) = live.as_deref_mut() {
                    let usage = data.get("usage").cloned().unwrap_or_default();
                    // prompt+completion 即该请求结束时的上下文实际占用,
                    // 与进程内那条路同一个口径。
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
            }
            "run.completed" => break data,
            "run.failed" => {
                renderer.finish()?;
                if let Some(live) = live.as_deref_mut() {
                    // 和下面取消那支同一个理由:提前 return 的路都得自己熄波浪。
                    // 08-20 为取消补过一次,报错这支漏了——用户 09-21 实录:回合
                    // 以报错收场时最后一帧波浪冻在 footer 上,按任意键才消失。
                    live.stop_footer_spinner()?;
                    live.apply_renderer_frame(&mut renderer)?;
                }
                handoff_raw!();
                bail!("{}", ipc_text(&data, "message"));
            }
            "run.cancelled" => {
                renderer.finish()?;
                if let Some(live) = live.as_deref_mut() {
                    // 提前 return 的取消路径也要熄波浪:漏掉它,输入框贴着
                    // 终端底部取消时最后一帧波浪就留在屏上(08-20 实测)。
                    live.stop_footer_spinner()?;
                    live.apply_renderer_frame(&mut renderer)?;
                }
                handoff_raw!();
                return Err(anyhow::Error::new(RemoteTurnCancelled {
                    context: CancelledContext::from_event(&data),
                }));
            }
            _ => {}
        }
        if let Some(live) = live.as_deref_mut() {
            live.apply_renderer_frame(&mut renderer)?;
        }
    };
    // 等到回合收尾还没打出去的图(等的那把工具的 `tool.finished` 一直没配上):
    // 原来整批静默丢掉,屏上只剩一句「已交给宿主显示」(09-23 macOS 真机,
    // 小红书登录二维码)。收尾时补打,并记一笔,好查为什么没配上。
    if !deferred_images.is_empty() {
        image_trace(&format!(
            "run.completed with {} image(s) still waiting for tool.finished: {:?}",
            deferred_images.len(),
            deferred_images
                .iter()
                .map(|(image, _)| ipc_text(image, "tool_id").to_string())
                .collect::<Vec<_>>()
        ));
        let state = queue_state
            .as_ref()
            .expect("queue state exists for a remote turn");
        print_deferred_images(
            std::mem::take(&mut deferred_images),
            state,
            &mut renderer,
            live.as_deref_mut(),
        )
        .await?;
        if let Some(live) = live.as_deref_mut() {
            if live.external_output_active {
                live.external_output_active = false;
                live.output_cursor = cursor_position_or(live.output_cursor);
                live.resume_at(live.output_cursor)?;
                live.apply_renderer_frame(&mut renderer)?;
            }
        }
    }
    renderer.finish()?;
    let focused = live.as_deref().map(|live| live.editor.focused);
    // 混合模型池的「本次供应商 / 模型」那行（BUG-05）：
    // - 「是不是混合」按**会话**的池判（会话钉了两个模型、全局只挂一个是常态），
    //   原来拿全局 config 判在这种配置下永远为假；
    // - `interactive` 档 = 只给交互 REPL：`live` 在就是交互，原来写死 false；
    // - 交互 REPL 走 tail 的帧通道落到正文里（全屏下裸 println 会落错位置），
    //   一次性/shellhook 仍走 stdout。
    let interactive = live.is_some();
    let endpoint_config = footer_config_for_session(paths, &config, &turn_session_id);
    let show_endpoint = show_mixed_model_endpoint(&endpoint_config, interactive);
    let endpoint = show_endpoint.then(|| {
        (
            completion
                .get("provider_id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("-")
                .to_string(),
            completion
                .get("model")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("-")
                .to_string(),
        )
    });
    if let Some(live) = live {
        live.stop_footer_spinner()?;
        live.apply_renderer_frame(&mut renderer)?;
        if let Some((provider, model)) = &endpoint {
            live.apply_output_frame(mixed_model_endpoint_frame(provider, model, None).as_bytes())?;
        }
        if let Some(raw) = raw.as_mut() {
            raw.handoff();
            live.raw_mode_handoff = true;
        }
    }

    let result = ChatResult {
        content,
        reasoning: (!reasoning.is_empty()).then_some(reasoning),
        usage: completion
            .get("usage")
            .cloned()
            .filter(|value| !value.is_null())
            .map(serde_json::from_value::<Usage>)
            .transpose()?,
        usage_estimated: completion
            .get("usage_estimated")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        tool_calls: Vec::new(),
        provider_id: completion
            .get("provider_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        model: completion
            .get("model")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        finish_reason: None,
        thinking_signature: None,
        last_request_usage: None,
        responses_continuation: None,
    };
    // 会话可能刚被自动命名（首条消息之后），标题跟着刷新一次。
    // **只在常驻 REPL 里设**：一次性 / shellhook 跑完就退出，改了标题没人改回来，
    // 人的终端标签页会被永久改名成「Miyu · 某某」。
    if interactive_repl {
        herdr::set_terminal_title_for_session(paths, &turn_session_id);
    }
    // 侧栏那几个自定义字段：模型和上下文占用。herdr 的 rows 里写 `$model`
    // `$ctx` 就能显示——这是 Claude Code 在 herdr 里都没有的。
    herdr::report_metadata(&[
        ("model", result.model.clone().unwrap_or_default()),
        (
            "ctx",
            completion
                .get("context_tokens")
                .and_then(serde_json::Value::as_u64)
                .map(|tokens| format!("{}k", tokens / 1000))
                .unwrap_or_default(),
        ),
    ]);
    if config.notifications.on_turn_complete {
        notify_if_unfocused(
            &config,
            focused,
            t("Miyu finished replying", "Miyu 回复完成"),
            // 正文不往通知里放：桌面通知是给**别人也可能看见的屏幕**发的，
            // 而且回复本身在窗口里就摆着，通知只需要说"该回来看了"。
            t("waiting for you", "正在等待处理"),
            miyu_base::notify::NotifySound::TurnDone,
        );
    }
    print_mixed_model_endpoint(show_endpoint && !interactive, &result, None);
    let context_tokens = completion
        .get("context_tokens")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or_default();
    let context_window = completion
        .get("context_window")
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| usize::try_from(value).ok());
    let completion_u64 = |key: &str| {
        completion
            .get(key)
            .and_then(serde_json::Value::as_u64)
            .unwrap_or_default()
    };
    let cumulative_tokens = TurnTokens {
        total: completion_u64("cumulative_tokens"),
        prompt: completion_u64("cumulative_prompt_tokens"),
        cache_read: completion_u64("cumulative_cache_read_tokens"),
    };
    print_chat_token_usage(
        &result,
        config.display.show_token_usage && !plain,
        context_tokens,
        context_window,
        TurnTokens::from_usage(result.usage.as_ref()),
    )?;
    Ok(Some(RemoteTurnSummary {
        result,
        context_tokens,
        context_window,
        cumulative_tokens,
    }))
}

/// 非全屏那条路攒下的图,一张张打出来。
///
/// 图片打完不用再单独「抬进页内」:残影的根因不在图片的位置,而在受限区滚动
/// 本身(见 tail/frame.rs 的 queue_lifted_frame),此后的帧都会改走整屏滚,活动区
/// resume 时自己会把光标下方的溢出滚掉。
async fn print_deferred_images(
    images: Vec<(serde_json::Value, Option<String>)>,
    state: &StateStore,
    renderer: &mut render::StreamRenderer,
    mut live: Option<&mut LiveReplTail>,
) -> Result<()> {
    for (image, size) in images {
        renderer.prepare_for_external_output()?;
        if let Some(live) = live.as_deref_mut() {
            live.apply_renderer_frame(renderer)?;
            synchronized_terminal_update(CursorAfterUpdate::Hidden, || live.suspend())?;
            live.external_output_active = true;
        }
        match render_remote_tool_image(state, &image, size).await {
            Ok(()) => image_trace(&format!(
                "printed deferred image tool_id={}",
                ipc_text(&image, "tool_id")
            )),
            Err(error) => {
                image_trace(&format!("deferred image failed: {error}"));
                renderer.write_system_message(&format!(
                    "{}: {error}",
                    t("Could not display tool image", "工具图片显示失败")
                ))?;
            }
        }
    }
    Ok(())
}

/// `MIYU_IMAGE_TRACE=1` 时把远端图片这一路记进 `image-trace.log`(与 chafa、
/// kitty 那两条同一个文件)。「图有时出不来」只能靠它在真机上抓现场。
fn image_trace(line: &str) {
    if miyu_base::terminal::chafa::trace_enabled() {
        miyu_base::terminal::chafa::trace(&format!("[remote] {line}"));
    }
}
