//! 从终端读一次 REPL 输入。
//!
//! 这一层负责把按键事件变成一次提交：读键、分发给编辑器、处理粘贴与括号粘贴
//! 模式、在需要时重绘输入区。它是编辑器（纯状态）与终端（真实字节）之间的桥。

// 输入层还用着一批留在 cli::mod 的辅助，以及编辑器与宽度计算。
use crate::cli::repl::editor::*;
use crate::cli::repl::width::*;
use crate::cli::*;

pub(in crate::cli) fn read_live_repl_input(
    live: &mut LiveReplTail,
    paths: &MiyuPaths,
    jobs_feed: &JobsFeed,
    // 这个 REPL 的会话：唤醒回合按它认领，输入历史也按它刷新。
    repl_session: Option<&str>,
) -> Result<LiveReplOutcome> {
    let mut raw_mode = if std::mem::take(&mut live.raw_mode_handoff) {
        LiveRawMode::adopt()
    } else {
        let guard = LiveRawMode::start()?;
        // 全屏：raw 模式断过一段（斜杠命令等 daemon 的那几秒终端在回显模式），
        // 屏上可能落了回显进来的字符，整屏按缓冲重画一遍把它们盖掉。
        if crate::cli::repl::tail::screen::in_fullscreen() {
            live.rendered = false;
        }
        guard
    };
    if !live.rendered {
        synchronized_terminal_update(CursorAfterUpdate::Shown, || live.resume())?;
    }
    let mut last_key_at = Instant::now();
    // 大厅动画的下一拍。**按时刻推**，不按「这一轮没有输入」推。
    //
    // 原来只有空闲分支（`!has_input`）里才 `tick_banner()`：按键比 40ms 一拍还
    // 密时，`poll` 每次都立刻报就绪，那条分支一次都进不去，星空和扫光就定格了
    // （用户 09-20 实测「空会话按住退格键时动画会暂停」，真机验证本修复有效）。
    // 09-17 修面板动画时踩的是同一个坑，当时只改了面板那条路。
    //
    // **沙箱走查复现不出来**（`testkit/tui/lobby_anim.py` 的 holding-* 三场改前
    // 改后都是 6/6）：pty 里灌按键和真实终端的按键重复不是一回事。所以这条修复
    // 的证据是用户真机，不是走查——走查只负责守住「按住键时还在动」这条线。
    const BANNER_TICK: Duration = Duration::from_millis(40);
    let mut next_banner_at = Instant::now() + BANNER_TICK;
    /// 到点就推一帧大厅动画。放在**每一处会长时间不回到循环顶端的地方**。
    macro_rules! tick_banner_if_due {
        () => {
            if live.banner.is_some() && Instant::now() >= next_banner_at {
                live.tick_banner()?;
                next_banner_at = Instant::now() + BANNER_TICK;
            }
        };
    }
    loop {
        tick_banner_if_due!();
        // 等待权自持:PTY 死亡后 crossterm 的 poll 会在内部对 HUP fd
        // 无限自旋、永不返回(实测),所以不能把"等 80ms"交给它——用裸
        // poll 等待并率先识别挂断,有输入就绪时才让 crossterm 取事件。
        let mut pollfd = libc::pollfd {
            fd: libc::STDIN_FILENO,
            events: libc::POLLIN,
            revents: 0,
        };
        // 开着面板时轮询放快一倍：面板里的转轮 80ms 一帧，轮询也是 80ms 的话
        // 差一毫秒就漏一帧，看着一顿一顿。大厅 banner 挂着时同理（星空、扫光）。
        // 指针停在边缘、提亮还亮着：轮询也得跟上，否则 100ms 的判定等在 80ms
        // 的轮询上，反应还是接近两百毫秒（用户 09-22 连着两次要更灵敏）。这一
        // 档只在「指针正停在最外一圈且有提亮等着熄」时启用，不是常态开销。
        let wait_ms = if live.hover_pending_leave() {
            20
        } else if live.overlay_open() || live.banner.is_some() {
            40
        } else {
            80
        };
        let ready = unsafe { libc::poll(&mut pollfd, 1, wait_ms) };
        if ready == 1 && (pollfd.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL)) != 0 {
            return Ok(LiveReplOutcome::Exit);
        }
        // 就绪判定必须问 crossterm(它的内部缓冲对裸 poll 不可见):
        // 一次 read 会把 fd 里的字节全部吞进内部缓冲,只看 fd 会让
        // 积压按键滞留到下一次按键或超时才放行,打字手感直接变卡。
        let has_input = event::poll(Duration::ZERO)?;
        if !has_input {
            // Idle tick: structural changes redraw the whole tail; otherwise
            // only the strip repaints. While the user is actively typing the
            // animation pauses so the two repaint sources never interleave.
            let reports = jobs_feed.take_reports();
            if !reports.is_empty() {
                // 补印难得一次，现读配置就够（跨会话消息露几行正文要看它）。
                let preview_lines = AppConfig::load_or_default(paths)
                    .map(|config| config.display.cross_session_preview_lines)
                    .unwrap_or(10);
                for report in reports {
                    synchronized_terminal_update(CursorAfterUpdate::Preserve, || {
                        live.show_background_report(&report, preview_lines)
                    })?;
                }
            }
            // 听写:识别出的句子填进编辑框(或按配置直接提交)。
            for (event, auto_submit) in crate::cli::repl::dictation::poll() {
                use crate::cli::repl::dictation::DictationEvent;
                match event {
                    DictationEvent::Utterance(text) if auto_submit => {
                        live.editor.input = text;
                        live.editor.cursor = live.editor.input.chars().count();
                        if let Some(submission) = live.editor.submit() {
                            let mode = live.mode();
                            synchronized_terminal_update(CursorAfterUpdate::Shown, || {
                                live.commit_submission(&submission)
                            })?;
                            let entry = ReplHistoryEntry::from_submission(&submission);
                            return Ok(LiveReplOutcome::Submit(
                                mode,
                                submission.content,
                                submission.images,
                                entry,
                            ));
                        }
                    }
                    DictationEvent::Utterance(text) => {
                        if !live.editor.input.is_empty()
                            && !live.editor.input.ends_with(char::is_whitespace)
                        {
                            live.editor.input.push(' ');
                        }
                        live.editor.input.push_str(&text);
                        live.editor.cursor = live.editor.input.chars().count();
                        synchronized_terminal_update(CursorAfterUpdate::Preserve, || {
                            live.redraw()
                        })?;
                    }
                    DictationEvent::Ended => {
                        repl_note(
                            live,
                            &format!("\x1b[2m{}\x1b[0m\n", t("dictation ended", "听写结束")),
                        )?;
                    }
                    DictationEvent::Error(message) => {
                        repl_note(
                            live,
                            &format!(
                                "\x1b[31m{}: {message}\x1b[0m\n",
                                t("dictation failed", "听写失败")
                            ),
                        )?;
                    }
                }
            }
            // 打字期间暂停动画，是 inline 的历史包袱：那边状态条和活动区是
            // 两处各自往终端打，叠在一起会写坏帧。全屏下整屏由一个画笔按 diff
            // 重画，没有这个冲突——再暂停就只剩「一交互进度条就卡住」的坏处。
            let typing = !crate::cli::repl::tail::screen::in_fullscreen()
                && last_key_at.elapsed() < Duration::from_millis(350);
            if typing {
                continue;
            }
            if let Some(session) = repl_session {
                if let Some(run) = jobs_feed.claim_wake_run(session) {
                    return Ok(LiveReplOutcome::FollowWake {
                        run_id: run.run_id,
                        label: run.label,
                        from_start: run.from_start,
                    });
                }
                // 同一个会话里**别人**起的轮（第二个 TUI、另一个终端的
                // shellhook）：挂上去，从这一轮开头补一遍。
                if let Some((run_id, label)) = jobs_feed.claim_peer_run(session) {
                    return Ok(LiveReplOutcome::FollowWake {
                        run_id,
                        label,
                        from_start: true,
                    });
                }
            }
            // 输入框右上角那行 `/goal …`：目标跑着的时候屏幕上只有正文，
            // 这行提示是唯一还在动的东西（用户 09-19）。一秒一拍。
            live.tick_goal_hint(jobs_feed.goal())?;
            let cumulative_changed = jobs_feed
                .cumulative()
                .is_some_and(|totals| live.footer.update_cumulative_tokens(totals));
            // 主循环整份覆盖 footer 之前要把这份收回去（见 `cumulative_from_poll`）。
            live.cumulative_from_poll |= cumulative_changed;
            // 大厅里按 Tab 换到的车道还显示「—」：事先那一问（空会话上下文）回来了就
            // 补上。刚启动就按 Tab 时会碰上它还没回来。
            let lane_counted = live.lobby_lane_pending
                && live.footer.session_tokens().is_none()
                && jobs_feed
                    .empty_session_context(live.mode())
                    .is_some_and(|tokens| {
                        live.footer.update_session_tokens(tokens);
                        true
                    });
            live.expire_toast()?;
            live.expire_hover()?;
            live.tick_overlay()?;
            if let Some(job_id) = live.pending_stop_job.take() {
                return Ok(LiveReplOutcome::StopJob { job_id });
            }
            if live.set_jobs(jobs_feed.current()) || cumulative_changed || lane_counted {
                synchronized_terminal_update(CursorAfterUpdate::Preserve, || live.redraw())?;
            } else {
                live.tick_job_strip()?;
                // banner 的帧不在这儿推了：循环顶上按时刻推，打字期间也照走
                // （见 `next_banner_at`）。留在这里会变成一拍推两帧。
            }
            continue;
        }
        // 抽干本轮就绪的全部事件再回到等待,粘贴/快速输入不积压。
        //
        // **这条循环出不来时也要推帧**：按住键时按键来得比处理得快（每下都要
        // 重画一次活动区），`poll(ZERO)` 一直报就绪，这里能连转很久都回不到
        // 循环顶端。只在循环顶端放一处挡不住这种情形。
        //
        // 编辑引起的重画攒到抽干之后画一次（09-23）：输入法一次上屏一串字，
        // crossterm 拆成一串按键，原来一个键一帧，终端被迫连收连画好几帧。
        let mut redraw_pending = false;
        while event::poll(Duration::ZERO)? {
            tick_banner_if_due!();
            // read 前再验挂断:HUP 的 fd 会让 poll 报就绪却读不出事件,
            // 直接 read 就掉进 crossterm 的自旋。
            if terminal_hangup() {
                return Ok(LiveReplOutcome::Exit);
            }
            last_key_at = Instant::now();
            let event = event::read()?;
            // 听写中 Esc = 停止听写,已听写的文字留在编辑框里(编辑框有内容时
            // 打不出 /stt,所以停止不能靠命令)。
            if crate::cli::repl::dictation::is_active()
                && matches!(
                    &event,
                    Event::Key(KeyEvent {
                        code: KeyCode::Esc,
                        kind,
                        ..
                    }) if *kind != KeyEventKind::Release
                )
            {
                crate::cli::repl::dictation::stop();
                repl_note(
                    live,
                    &format!("\x1b[2m{}\x1b[0m\n", t("dictation stopped", "听写已停止")),
                )?;
                synchronized_terminal_update(CursorAfterUpdate::Preserve, || live.redraw())?;
                continue;
            }
            // 上键开始翻历史之前，先把别的 REPL 刚落盘的输入补进来。历史只在
            // 启动时读一次，两个 REPL 同时开着时先开的那个原本永远看不到后开
            // 的那个敲了什么（见 `refresh_repl_input_history`）。
            //
            // 只在输入框为空、也就是「从头开始翻」时刷：翻到一半重载会让
            // history_index 指错行。
            if live.editor.input.is_empty()
                && matches!(
                    &event,
                    Event::Key(KeyEvent {
                        code: KeyCode::Up,
                        kind,
                        ..
                    }) if *kind != KeyEventKind::Release
                )
            {
                if let Some(session) = repl_session {
                    if refresh_repl_input_history(&mut live.editor.history, paths, session) {
                        live.editor.history_index = live.editor.history.len();
                    }
                }
            }
            // 全屏下先给视口一次机会（回翻、滚轮）；inline 下这里是空操作。
            if live.handle_screen_event(&event)? {
                continue;
            }
            // 又打字了：候选面板可以重新弹出来（Esc 只关「当时那一串」）。
            if matches!(&event, Event::Key(KeyEvent { kind, .. }) if *kind != KeyEventKind::Release)
            {
                live.allow_command_hint();
            }
            match live.editor.handle_event(event, paths, false)? {
                LiveEditorAction::None => {}
                LiveEditorAction::Redraw => redraw_pending = true,
                LiveEditorAction::ClearScreen => {
                    synchronized_terminal_update(CursorAfterUpdate::Preserve, || {
                        live.clear_screen()
                    })?
                }
                LiveEditorAction::EmptySubmit => {
                    synchronized_terminal_update(CursorAfterUpdate::Preserve, || {
                        live.commit_empty_submission()
                    })?
                }
                LiveEditorAction::Submit(submission) => {
                    // 回车发送即结束听写,不让麦克风继续往下一条消息里灌字。
                    crate::cli::repl::dictation::stop();
                    // `/goal edit`（无参数）在提交前原地变身成可编辑的
                    // 「/goal edit <当前目标>」，不回显、不产生任何输出。
                    if submission.content.trim() == "/goal edit"
                        && crate::cli::repl::session::prefill_goal_edit_input(
                            paths,
                            repl_session,
                            live,
                        )
                    {
                        synchronized_terminal_update(CursorAfterUpdate::Preserve, || {
                            live.redraw()
                        })?;
                        continue;
                    }
                    let mode = live.mode();
                    // 这一句要送进模型（不是斜杠命令）。
                    let chat = submission_leaves_lobby(&submission.content);
                    // 回显和活动区重画在一个同步块里完成:光标不在左下角
                    // 落脚,kitty 的 cursor_trail 就没有东西可画(见 commit_submission)。
                    //
                    // 空会话里大厅也在同一帧撤掉（用户 09-23「回车提交会闪一下」）：
                    // 原来这一帧画的是「输入框清空了的大厅」，要等主循环、开回合的
                    // 往返回来才换成正文布局，两帧之间光标还露在大厅输入框里。
                    synchronized_terminal_update(CursorAfterUpdate::Shown, || {
                        if chat && live.banner.is_some() {
                            live.leave_lobby();
                        }
                        live.commit_submission(&submission)
                    })?;
                    // 紧接着就是回合：raw 模式直接交给回合循环，不在中间关一下再开。
                    // 关着的那一小段终端回到回显模式，这时敲的键会被回显、回车会变
                    // 成换行；关的那一下还会把光标露出来。没人接（发完就退出）由
                    // 主循环收尾时 `release_raw_handoff` 收回。
                    if chat {
                        raw_mode.handoff();
                        live.raw_mode_handoff = true;
                    }
                    let entry = ReplHistoryEntry::from_submission(&submission);
                    return Ok(LiveReplOutcome::Submit(
                        mode,
                        submission.content,
                        submission.images,
                        entry,
                    ));
                }
                LiveEditorAction::ToggleReadonly => return Ok(LiveReplOutcome::ToggleReadonly),
                LiveEditorAction::ToggleMode => {
                    let next = match live.mode() {
                        PersonaLane::Active => PersonaLane::Dev,
                        PersonaLane::Dev => PersonaLane::Active,
                    };
                    // 切车道只换显示（用户 09-23），用不着回显模式：raw 模式交给下一
                    // 次读输入。原来这里一关一开，中间敲的 Tab 被终端回显成跳到下一
                    // 个制表位，光标在输入框里左右晃，回来还要整屏重画一遍。
                    raw_mode.handoff();
                    live.raw_mode_handoff = true;
                    return Ok(LiveReplOutcome::SwitchMode(next));
                }
                // Ctrl+C rung 3: the draft was empty and no reply is running, but
                // this session still has background work — stop that before the
                // press is allowed to mean "quit". `live.jobs` holds only running
                // jobs of this session, refreshed on every idle tick. Ctrl+D
                // (`Exit`) always quits outright.
                LiveEditorAction::Interrupt if !live.jobs.is_empty() => {
                    return Ok(LiveReplOutcome::StopJobs);
                }
                // Ctrl+C 的最后一级在全屏下不退出。
                //
                // inline 下退出无所谓——scrollback 还在，往上翻就都看得到。
                // 全屏是一块自己的画布，退出等于整屏一起没，为了一次误触付这个
                // 代价太贵。阶梯照旧（清草稿 → 中断回复 → 停后台任务），只是
                // 最后一级改成提示走 Ctrl+D。
                LiveEditorAction::Interrupt if crate::cli::repl::tail::screen::in_fullscreen() => {
                    live.toast_note_at(t("press Ctrl+D to exit", "要退出请按 Ctrl+D"), true);
                    continue;
                }
                LiveEditorAction::Interrupt | LiveEditorAction::Exit => {
                    synchronized_terminal_update(CursorAfterUpdate::Hidden, || live.suspend())?;
                    return Ok(LiveReplOutcome::Exit);
                }
            }
        }
        if redraw_pending {
            synchronized_terminal_update(CursorAfterUpdate::Preserve, || live.redraw())?;
        }
    }
}

/// 回合跑着时的输入泵：后面还排着输入，就让下一拍立刻到。
///
/// 三条回合泵（自己起的轮、挂上去跟的轮、直连）都是 16ms 一拍、一拍只收一个键。
/// 输入法一次上屏一串字，crossterm 拆成一串按键，于是六个字要九十毫秒才出全
/// （09-23 实测，空闲时 1.5ms）；按住键不放也会越攒越多。一拍收一个的逻辑不动，
/// 只是不再白等下一拍。
pub(in crate::cli) fn hurry_pending_input(tick: &mut tokio::time::Interval) -> Result<()> {
    if event::poll(Duration::ZERO)? {
        tick.reset_immediately();
    }
    Ok(())
}

pub(in crate::cli) fn read_repl_input(
    paths: &MiyuPaths,
    mode: PersonaLane,
    prefill: Option<String>,
    history: &[ReplHistoryEntry],
    footer: &ReplFooterStatus,
    show_shortcut_hint: bool,
) -> Result<
    Option<(
        PersonaLane,
        String,
        Vec<Option<miyu_base::clipboard::PastedImage>>,
    )>,
> {
    let mut stdout = io::stdout();
    let mut input = strip_terminal_control_sequences(&prefill.unwrap_or_default());
    let mut cursor = input.chars().count();
    let mut history_index = history.len();
    let mut history_clean_index: Option<usize> = None;
    let plain_prefix = "  ";
    let cursor_col = cursor_col_or(0);
    if cursor_col != 0 {
        writeln!(stdout)?;
        stdout.flush()?;
    }
    terminal::enable_raw_mode()?;
    spawn_hangup_watchdog();
    execute!(stdout, EnableBracketedPaste)?;
    let mut keyboard_enhancement = KeyboardEnhancementState::enable(&mut stdout);
    let mut input_row = cursor_row_or(0);
    let mut rendered_rows = 0u16;
    let mut raw_pasted_lines = 0usize;
    let mut pasted_images: Vec<Option<miyu_base::clipboard::PastedImage>> = Vec::new();
    let mut pasted_texts: Vec<Option<PastedText>> = Vec::new();
    // 1. 局部退出时统一恢复终端协议
    // 2. 避免多处 return 漏 Pop 键盘增强
    let restore_terminal = |stdout: &mut io::Stdout,
                            keyboard_enhancement: &mut KeyboardEnhancementState|
     -> Result<()> {
        execute!(stdout, DisableBracketedPaste)?;
        keyboard_enhancement.disable(stdout);
        terminal::disable_raw_mode()?;
        Ok(())
    };
    let render_repl_input = |stdout: &mut io::Stdout,
                             input_row: &mut u16,
                             rendered_rows: &mut u16,
                             mode: PersonaLane,
                             input: &str,
                             cursor: usize,
                             raw_pasted_lines: usize| {
        render_repl_input_with_footer(
            stdout,
            input_row,
            rendered_rows,
            &mut Vec::new(),
            mode,
            // 老的非 live 输入只剩直连模式在用,直连没有沙盒可切。
            false,
            input,
            cursor,
            raw_pasted_lines,
            footer,
            show_shortcut_hint,
            None,
            crate::cli::footer::UsagePlacement::FooterRight,
        )
    };
    render_repl_input(
        &mut stdout,
        &mut input_row,
        &mut rendered_rows,
        mode,
        &input,
        cursor,
        raw_pasted_lines,
    )?;
    loop {
        match event::read()? {
            Event::Paste(text) => {
                let raw_lines =
                    insert_pasted_text_at_cursor(&mut input, &mut cursor, text, &mut pasted_texts);
                history_clean_index = None;
                raw_pasted_lines = raw_pasted_lines.saturating_add(raw_lines);
                render_repl_input(
                    &mut stdout,
                    &mut input_row,
                    &mut rendered_rows,
                    mode,
                    &input,
                    cursor,
                    raw_pasted_lines,
                )?;
            }
            Event::Key(KeyEvent {
                code, modifiers, ..
            }) => match code {
                KeyCode::Tab => {
                    if input.starts_with('/') {
                        if let Some(completed) = complete_repl_command(&input) {
                            input = completed.to_string();
                            cursor = input.chars().count();
                            history_clean_index = None;
                        }
                    } else {
                        // 会话模式创建时定死:Tab 切换已随闲聊模式一并删除。
                    }
                    raw_pasted_lines = 0;
                    render_repl_input(
                        &mut stdout,
                        &mut input_row,
                        &mut rendered_rows,
                        mode,
                        &input,
                        cursor,
                        raw_pasted_lines,
                    )?;
                }
                KeyCode::Esc => {
                    input.clear();
                    cursor = 0;
                    history_clean_index = None;
                    raw_pasted_lines = 0;
                    pasted_images.clear();
                    pasted_texts.clear();
                    render_repl_input(
                        &mut stdout,
                        &mut input_row,
                        &mut rendered_rows,
                        mode,
                        &input,
                        cursor,
                        raw_pasted_lines,
                    )?;
                }
                KeyCode::Left => {
                    if let Some((start, _)) = placeholder_at_cursor(&input, cursor) {
                        cursor = start;
                    } else {
                        cursor = cursor.saturating_sub(1);
                    }
                    render_repl_input(
                        &mut stdout,
                        &mut input_row,
                        &mut rendered_rows,
                        mode,
                        &input,
                        cursor,
                        raw_pasted_lines,
                    )?;
                }
                KeyCode::Right => {
                    if let Some((_, end)) = placeholder_at_cursor(&input, cursor) {
                        cursor = end;
                    } else {
                        cursor = (cursor + 1).min(input.chars().count());
                    }
                    render_repl_input(
                        &mut stdout,
                        &mut input_row,
                        &mut rendered_rows,
                        mode,
                        &input,
                        cursor,
                        raw_pasted_lines,
                    )?;
                }
                KeyCode::Home => {
                    cursor = 0;
                    render_repl_input(
                        &mut stdout,
                        &mut input_row,
                        &mut rendered_rows,
                        mode,
                        &input,
                        cursor,
                        raw_pasted_lines,
                    )?;
                }
                KeyCode::End => {
                    cursor = input.chars().count();
                    render_repl_input(
                        &mut stdout,
                        &mut input_row,
                        &mut rendered_rows,
                        mode,
                        &input,
                        cursor,
                        raw_pasted_lines,
                    )?;
                }
                KeyCode::Up => {
                    if !history.is_empty()
                        && repl_should_browse_history(&input, history, history_clean_index)
                    {
                        if input.is_empty() {
                            history_index = history.len();
                        }
                        history_index = history_index.saturating_sub(1);
                        restore_history_entry(
                            &history.get(history_index).cloned().unwrap_or_default(),
                            &mut input,
                            &mut cursor,
                            &mut pasted_images,
                            &mut pasted_texts,
                            &mut raw_pasted_lines,
                        );
                        history_clean_index = Some(history_index);
                    } else {
                        cursor = repl_move_cursor_vertical(&plain_prefix, &input, cursor, -1);
                    }
                    render_repl_input(
                        &mut stdout,
                        &mut input_row,
                        &mut rendered_rows,
                        mode,
                        &input,
                        cursor,
                        raw_pasted_lines,
                    )?;
                }
                KeyCode::Down => {
                    if repl_history_is_clean(&input, history, history_clean_index) {
                        if history_index + 1 < history.len() {
                            history_index += 1;
                            restore_history_entry(
                                &history.get(history_index).cloned().unwrap_or_default(),
                                &mut input,
                                &mut cursor,
                                &mut pasted_images,
                                &mut pasted_texts,
                                &mut raw_pasted_lines,
                            );
                            history_clean_index = Some(history_index);
                        } else {
                            history_index = history.len();
                            input.clear();
                            cursor = 0;
                            history_clean_index = None;
                            raw_pasted_lines = 0;
                            pasted_images.clear();
                            pasted_texts.clear();
                        }
                    } else {
                        cursor = repl_move_cursor_vertical(&plain_prefix, &input, cursor, 1);
                    }
                    render_repl_input(
                        &mut stdout,
                        &mut input_row,
                        &mut rendered_rows,
                        mode,
                        &input,
                        cursor,
                        raw_pasted_lines,
                    )?;
                }
                KeyCode::Enter if modifiers.contains(KeyModifiers::SHIFT) => {
                    // Shift+Enter 与 Ctrl+J 相同：在光标处插入换行，不提交
                    insert_newline_at_cursor(&mut input, &mut cursor);
                    history_clean_index = None;
                    raw_pasted_lines = 0;
                    render_repl_input(
                        &mut stdout,
                        &mut input_row,
                        &mut rendered_rows,
                        mode,
                        &input,
                        cursor,
                        raw_pasted_lines,
                    )?;
                }
                KeyCode::Enter => {
                    let submitted_echo = strip_terminal_control_sequences(&input);
                    input = expand_pasted_text_placeholders(&submitted_echo, &pasted_texts);
                    replace_repl_input_with_user_echo(
                        &mut stdout,
                        input_row,
                        rendered_rows,
                        mode,
                        &submitted_echo,
                    )?;
                    restore_terminal(&mut stdout, &mut keyboard_enhancement)?;
                    return Ok(Some((mode, input, pasted_images)));
                }
                KeyCode::Char('j') if modifiers.contains(KeyModifiers::CONTROL) => {
                    insert_newline_at_cursor(&mut input, &mut cursor);
                    history_clean_index = None;
                    raw_pasted_lines = 0;
                    render_repl_input(
                        &mut stdout,
                        &mut input_row,
                        &mut rendered_rows,
                        mode,
                        &input,
                        cursor,
                        raw_pasted_lines,
                    )?;
                }
                KeyCode::Char('c')
                    if modifiers.contains(KeyModifiers::CONTROL)
                        && !modifiers.contains(KeyModifiers::SHIFT) =>
                {
                    if !input.is_empty() {
                        input.clear();
                        cursor = 0;
                        history_clean_index = None;
                        raw_pasted_lines = 0;
                        pasted_images.clear();
                        pasted_texts.clear();
                        render_repl_input(
                            &mut stdout,
                            &mut input_row,
                            &mut rendered_rows,
                            mode,
                            &input,
                            cursor,
                            raw_pasted_lines,
                        )?;
                        continue;
                    }
                    move_after_repl_input(&mut stdout, input_row, rendered_rows)?;
                    restore_terminal(&mut stdout, &mut keyboard_enhancement)?;
                    return Ok(None);
                }
                KeyCode::Char('d')
                    if modifiers.contains(KeyModifiers::CONTROL) && input.is_empty() =>
                {
                    move_after_repl_input(&mut stdout, input_row, rendered_rows)?;
                    restore_terminal(&mut stdout, &mut keyboard_enhancement)?;
                    return Ok(None);
                }
                KeyCode::Char('l') if modifiers.contains(KeyModifiers::CONTROL) => {
                    queue!(stdout, Clear(ClearType::All), MoveTo(0, 0))?;
                    stdout.flush()?;
                    input_row = 0;
                    rendered_rows = 0;
                    render_repl_input(
                        &mut stdout,
                        &mut input_row,
                        &mut rendered_rows,
                        mode,
                        &input,
                        cursor,
                        raw_pasted_lines,
                    )?;
                }
                KeyCode::Char('w') if modifiers.contains(KeyModifiers::CONTROL) => {
                    remove_word_before_cursor(
                        &mut input,
                        &mut cursor,
                        &mut pasted_images,
                        &mut pasted_texts,
                    );
                    history_clean_index = None;
                    raw_pasted_lines = 0;
                    render_repl_input(
                        &mut stdout,
                        &mut input_row,
                        &mut rendered_rows,
                        mode,
                        &input,
                        cursor,
                        raw_pasted_lines,
                    )?;
                }
                KeyCode::Backspace => {
                    if cursor > 0 {
                        if let Some((start, end)) = placeholder_before_or_at_cursor(&input, cursor)
                        {
                            clear_placeholder_payload(
                                &input,
                                start,
                                end,
                                &mut pasted_images,
                                &mut pasted_texts,
                            );
                            remove_range_chars(&mut input, start, end);
                            cursor = start;
                        } else {
                            remove_char_before_cursor(&mut input, &mut cursor);
                        }
                        history_clean_index = None;
                    }
                    raw_pasted_lines = 0;
                    render_repl_input(
                        &mut stdout,
                        &mut input_row,
                        &mut rendered_rows,
                        mode,
                        &input,
                        cursor,
                        raw_pasted_lines,
                    )?;
                }
                KeyCode::Delete => {
                    if let Some((start, end)) = placeholder_after_or_at_cursor(&input, cursor) {
                        clear_placeholder_payload(
                            &input,
                            start,
                            end,
                            &mut pasted_images,
                            &mut pasted_texts,
                        );
                        remove_range_chars(&mut input, start, end);
                    } else {
                        remove_char_at_cursor(&mut input, cursor);
                    }
                    history_clean_index = None;
                    raw_pasted_lines = 0;
                    render_repl_input(
                        &mut stdout,
                        &mut input_row,
                        &mut rendered_rows,
                        mode,
                        &input,
                        cursor,
                        raw_pasted_lines,
                    )?;
                }
                KeyCode::Char('c' | 'C')
                    if modifiers.contains(KeyModifiers::CONTROL)
                        && modifiers.contains(KeyModifiers::SHIFT) =>
                {
                    if let Some(selected) =
                        placeholder_text_near_cursor(&input, cursor, &pasted_texts)
                    {
                        let _ = miyu_base::clipboard::write_clipboard_text(&selected)?;
                    }
                }
                KeyCode::Char('v') if modifiers.contains(KeyModifiers::CONTROL) => {
                    match miyu_base::clipboard::read_clipboard() {
                        Ok(miyu_base::clipboard::ClipboardContent::Image(img)) => {
                            let index = pasted_images.len() + 1;
                            // 占位符只认序号,文件名纯属显示噪音(模型侧路径
                            // 由 rewrite_image_placeholders_with_paths 另拼)。
                            let _ = img.write_temp_file(&paths.cache_dir, index);
                            let placeholder = format!("[Image {}]", index);
                            insert_str_at_cursor(&mut input, &mut cursor, &placeholder);
                            history_clean_index = None;
                            pasted_images
                                .push(Some(miyu_base::clipboard::PastedImage::Binary(img)));
                            raw_pasted_lines = 0;
                            render_repl_input(
                                &mut stdout,
                                &mut input_row,
                                &mut rendered_rows,
                                mode,
                                &input,
                                cursor,
                                raw_pasted_lines,
                            )?;
                        }
                        Ok(miyu_base::clipboard::ClipboardContent::MediaPath(path)) => {
                            let index = pasted_images.len() + 1;
                            let label = media_placeholder_label(&path);
                            let placeholder = format!("[{label} {index}]");
                            insert_str_at_cursor(&mut input, &mut cursor, &placeholder);
                            history_clean_index = None;
                            pasted_images.push(Some(miyu_base::clipboard::PastedImage::Path(path)));
                            raw_pasted_lines = 0;
                            render_repl_input(
                                &mut stdout,
                                &mut input_row,
                                &mut rendered_rows,
                                mode,
                                &input,
                                cursor,
                                raw_pasted_lines,
                            )?;
                        }
                        Ok(miyu_base::clipboard::ClipboardContent::TextPath(path)) => {
                            insert_str_at_cursor(&mut input, &mut cursor, &path);
                            history_clean_index = None;
                            raw_pasted_lines = 0;
                            render_repl_input(
                                &mut stdout,
                                &mut input_row,
                                &mut rendered_rows,
                                mode,
                                &input,
                                cursor,
                                raw_pasted_lines,
                            )?;
                        }
                        _ => {
                            if let Ok(Some(text)) = miyu_base::clipboard::read_clipboard_text() {
                                let raw_lines = insert_pasted_text_at_cursor(
                                    &mut input,
                                    &mut cursor,
                                    text,
                                    &mut pasted_texts,
                                );
                                history_clean_index = None;
                                raw_pasted_lines = raw_pasted_lines.saturating_add(raw_lines);
                                render_repl_input(
                                    &mut stdout,
                                    &mut input_row,
                                    &mut rendered_rows,
                                    mode,
                                    &input,
                                    cursor,
                                    raw_pasted_lines,
                                )?;
                            }
                        }
                    }
                }
                KeyCode::Char(ch) if !modifiers.contains(KeyModifiers::CONTROL) => {
                    if !is_disallowed_control_char(ch) {
                        if let Some((_, end)) = placeholder_at_cursor(&input, cursor) {
                            cursor = end;
                        }
                        insert_char_at_cursor(&mut input, &mut cursor, ch);
                        history_clean_index = None;
                    }
                    raw_pasted_lines = 0;
                    render_repl_input(
                        &mut stdout,
                        &mut input_row,
                        &mut rendered_rows,
                        mode,
                        &input,
                        cursor,
                        raw_pasted_lines,
                    )?;
                }
                _ => {}
            },
            _ => {}
        }
    }
}

pub(in crate::cli) fn render_repl_input_with_footer(
    stdout: &mut impl Write,
    input_row: &mut u16,
    rendered_rows: &mut u16,
    // `drawn`：画出去的输入行（屏幕行号 + 这一行的文字）。全屏下拿它做选区——
    // 输入区不在正文缓冲里，不记下来就没法知道某一格上是什么字。
    drawn: &mut Vec<(u16, String)>,
    mode: PersonaLane,
    // 只读模式开着(09-23):状态行模式标签后面跟「只读」。
    readonly: bool,
    input: &str,
    cursor: usize,
    raw_pasted_lines: usize,
    footer: &ReplFooterStatus,
    show_shortcut_hint: bool,
    // 全屏空会话的大厅:输入框不在屏底、也不全宽,而是嵌在 banner 下面的一个
    // 窄框里。None = 老样子,从第 0 列画到终端右边。
    layout: Option<EditorBox>,
    // 用量画在 footer 右端,还是 footer 底下那一行(调用方替它留好了那一行)。
    usage: crate::cli::footer::UsagePlacement,
) -> Result<Option<u16>> {
    let suggestions = repl_command_suggestions(input);
    let lines = repl_input_lines(input);
    let prompt_prefix = input_prompt_bar(mode);
    let plain_prefix = "  ";
    // 这一处的 `cols` 是**框的宽度**,不是终端宽度:折行、光标、footer 截断
    // 全都按它算。大厅窄框里两者差着几十列,混用就是「字折了行、光标还留在
    // 屏幕右边」。
    let cols = box_cols(layout, terminal_cols());
    let x0 = box_left(layout);
    let blank = layout.map(|area| " ".repeat(area.width));
    let display_lines = repl_visible_input_lines(
        &plain_prefix,
        &lines,
        REPL_MAX_VISIBLE_INPUT_ROWS,
        raw_pasted_lines,
    );
    let display_rows = repl_wrapped_input_rows_for_cols(&plain_prefix, &display_lines, cols);
    let display_rows: Vec<String> = display_rows
        .iter()
        .map(|line| colorize_repl_placeholders(line))
        .collect();
    let input_rows = display_rows.len().max(1).min(u16::MAX as usize) as u16;
    let show_hint = show_shortcut_hint && suggestions.is_empty();
    let current_rows = input_rows.saturating_add(if show_hint { 4 } else { 3 });
    let rows_to_clear = (*rendered_rows).max(current_rows).max(1);
    ensure_repl_space(stdout, input_row, rows_to_clear)?;
    for row_offset in 0..rows_to_clear {
        queue!(stdout, MoveTo(x0, (*input_row).saturating_add(row_offset)))?;
        // 窄框只擦自己那一段:两侧是 banner 的星空,不能整行清掉。
        match &blank {
            Some(blank) => queue!(stdout, Print(blank))?,
            None => queue!(stdout, Clear(ClearType::CurrentLine))?,
        }
    }
    let mut row_offset = 0u16;
    let footer_row;
    queue!(stdout, MoveTo(x0, *input_row), Print(&prompt_prefix))?;
    // 输入框顶那一行右端：长任务跑起来之后屏幕上只有正文，看不出「它还在自己
    // 往前跑吗、第几轮了」（用户 09-19）。这行常驻提示只说这一件事，不进
    // footer——那儿已经挤着模型名和用量了。
    let goal_hint = crate::cli::footer::goal_hint_text(footer.goal.as_ref());
    if !goal_hint.is_empty() {
        let hint_width = visible_width(&goal_hint);
        let prefix_width = visible_width(&prompt_prefix);
        // 放不下就整条不画：截断出来的 `/goal runn` 比没有更糟。
        if cols > prefix_width.saturating_add(hint_width).saturating_add(2) {
            let column = u16::try_from(cols.saturating_sub(hint_width))
                .unwrap_or(u16::MAX)
                .saturating_add(x0);
            // 和左侧那根粗线、左下角的模式标签同一个高亮色（用户 09-19）。
            let style = crate::cli::footer::goal_hint_style(mode);
            queue!(
                stdout,
                MoveTo(column, *input_row),
                Print(format!("{style}{goal_hint}\x1b[0m"))
            )?;
        }
    }
    row_offset = row_offset.saturating_add(1);
    let pad = " ".repeat(usize::from(x0));
    for line in &display_rows {
        let row = (*input_row).saturating_add(row_offset);
        queue!(stdout, MoveTo(x0, row))?;
        queue!(stdout, Print(&prompt_prefix), Print(line))?;
        drawn.push((row, format!("{pad}{prompt_prefix}{line}")));
        row_offset = row_offset.saturating_add(1);
    }
    queue!(
        stdout,
        MoveTo(x0, (*input_row).saturating_add(row_offset)),
        Print(&prompt_prefix)
    )?;
    row_offset = row_offset.saturating_add(1);
    // 全屏下候选走输入框上方的浮层（`command_hint_lines`），footer 留着——
    // 挤掉 footer 的话打命令时连模型名和用量都看不见了。
    if !suggestions.is_empty() && !crate::cli::in_fullscreen() {
        let suggestion_width = cols.saturating_sub(visible_width(&prompt_prefix)).max(1);
        queue!(
            stdout,
            MoveTo(x0, (*input_row).saturating_add(row_offset)),
            Print(&prompt_prefix),
            Print(format!(
                "\x1b[2m{}\x1b[0m",
                repl_command_suggestions_line(&suggestions, suggestion_width)
            ))
        )?;
        footer_row = None;
    } else {
        footer_row = Some((*input_row).saturating_add(row_offset));
        queue!(
            stdout,
            MoveTo(x0, (*input_row).saturating_add(row_offset)),
            Print(repl_footer_line(mode, readonly, footer, cols, usage))
        )?;
        // 用量那一行不算进输入区的行数:它占的是活动区底下本来就留着的空行。
        if usage == crate::cli::footer::UsagePlacement::RowBelow {
            queue!(
                stdout,
                MoveTo(
                    x0,
                    (*input_row).saturating_add(row_offset).saturating_add(1)
                ),
                Print(crate::cli::footer::repl_usage_line(footer, cols))
            )?;
        }
        if show_hint {
            row_offset = row_offset.saturating_add(1);
            queue!(
                stdout,
                MoveTo(x0, (*input_row).saturating_add(row_offset)),
                Print(repl_shortcut_hint_line(mode, cols))
            )?;
        }
    }
    let (cursor_col, cursor_row_offset) = if display_lines.len() == lines.len() {
        repl_cursor_position_for_cols(&plain_prefix, input, cursor, cols)
    } else {
        let last_line = display_lines.last().map(String::as_str).unwrap_or_default();
        let (col, _) = repl_cursor_position_for_line_for_cols(
            &plain_prefix,
            last_line,
            last_line.chars().count(),
            cols,
        );
        (
            col,
            repl_prompt_rows_for_cols(&plain_prefix, &display_lines, cols).saturating_sub(1),
        )
    };
    queue!(
        stdout,
        MoveTo(
            cursor_col.saturating_add(x0),
            (*input_row)
                .saturating_add(1)
                .saturating_add(cursor_row_offset)
        )
    )?;
    stdout.flush()?;
    *rendered_rows = current_rows;
    Ok(footer_row)
}

pub(in crate::cli) fn move_after_repl_input(
    stdout: &mut io::Stdout,
    input_row: u16,
    rendered_rows: u16,
) -> Result<()> {
    queue!(
        stdout,
        MoveTo(0, input_row.saturating_add(rendered_rows.max(1)))
    )?;
    stdout.flush()?;
    Ok(())
}

pub(in crate::cli) fn replace_repl_input_with_user_echo(
    stdout: &mut io::Stdout,
    input_row: u16,
    rendered_rows: u16,
    mode: PersonaLane,
    input: &str,
) -> Result<()> {
    let cols = terminal_cols();
    let echo_lines = submitted_echo_lines(mode, input.trim_end(), cols);
    let echo_rows = echo_lines.len().min(u16::MAX as usize) as u16;
    let rows_to_clear = rendered_rows.max(echo_rows).max(1);
    for row_offset in 0..rows_to_clear {
        queue!(
            stdout,
            MoveTo(0, input_row.saturating_add(row_offset)),
            Clear(ClearType::CurrentLine)
        )?;
    }
    for (offset, line) in echo_lines.iter().enumerate() {
        queue!(
            stdout,
            MoveTo(
                0,
                input_row.saturating_add(offset.min(u16::MAX as usize) as u16)
            ),
            Print(line)
        )?;
    }
    queue!(
        stdout,
        MoveTo(0, input_row.saturating_add(echo_rows).saturating_add(1))
    )?;
    stdout.flush()?;
    Ok(())
}
