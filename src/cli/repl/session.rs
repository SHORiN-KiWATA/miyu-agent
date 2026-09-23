//! REPL 的会话选择与切换。
//!
//! 一次性会话（`create_ephemeral_session`）用完就删，`EphemeralSessionGuard` 的
//! `Drop` 保证 Ctrl-C 退出时也删得掉——否则每次中断都在库里留一个空会话。
//!
//! 远端回合（`RemoteTurn*`）有三种结局：正常、被取消、被分离到后台。分离不是
//! 错误，所以 `is_remote_turn_detached` 单独判——当成错误会让用户以为出事了。

use crate::cli::*;

/// Which session a one-shot CLI turn lands in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::cli) enum TurnSession {
    /// The terminal session — what shell-hook and `miyu new`/`session` drive.
    Current,
    /// An explicit `--session` target, resolved to a session id.
    Explicit(String),
    /// A throwaway session created for this turn and deleted right after, so a
    /// quick question never lands in a conversation the user cares about.
    Ephemeral,
}

/// Picks the session for `miyu ask` / a bare `miyu '<message>'`. Both default
/// to a throwaway session; `--session` and `--continue` opt back into a real
/// one (clap already rejects passing both).
pub(in crate::cli) async fn one_shot_session(
    paths: &MiyuPaths,
    session_arg: Option<&str>,
    continue_session: bool,
) -> Result<TurnSession> {
    if let Some(arg) = session_arg {
        // 与 `miyu session list` 同一份列表、同一套编号,找不到退出码 3。
        return Ok(TurnSession::Explicit(
            crate::cli::turn_request::resolve_managed_session(paths, arg)
                .await?
                .id,
        ));
    }
    if continue_session {
        return Ok(TurnSession::Current);
    }
    Ok(TurnSession::Ephemeral)
}

/// Named rather than left blank on purpose: a row that leaks past the sweep is
/// recognisable, and a non-empty name also skips the daemon's auto-title LLM
/// call (`maybe_auto_name_session`) for a session about to be deleted.
pub(in crate::cli) fn ephemeral_session_name() -> String {
    t("One-shot", "一次性对话").to_string()
}

/// `mode`(normal/dev)决定阅后即焚会话建在哪个人格名下;None = 普通。
pub(in crate::cli) async fn create_ephemeral_session(
    paths: &MiyuPaths,
    mode: Option<&str>,
) -> Result<String> {
    let (_, data) = session_admin(
        paths,
        IpcCommand::CreateSession {
            name: Some(ephemeral_session_name()),
            switch: false,
            kind: Some(miyu_core::state::ASK_SESSION_KIND.to_string()),
            mode: mode.map(str::to_string),
        },
    )
    .await?;
    data.get("session")
        .and_then(|session| session.get("session_id"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("Miyu core returned an invalid response"))
}

/// Tears a throwaway session down. Background jobs go first so nothing is left
/// pointing at a session that is about to disappear. Best effort: a daemon
/// that has gone away leaves a row the startup sweep collects.
pub(in crate::cli) async fn discard_ephemeral_session(paths: &MiyuPaths, session_id: &str) {
    // CLI 中转(claude-code/antigravity)的联动:直连形态没有 daemon,DeleteSession
    // 那条路上的 forget 不会跑到,这里自己收——续传映射与 CLI 侧转录都在本进程。
    miyu_core::llm::forget_relay_sessions(session_id);
    let _ = send_ipc_admin(
        paths,
        IpcCommand::StopSessionJobs {
            session_id: session_id.to_string(),
        },
    )
    .await;
    let _ = send_ipc_admin(
        paths,
        IpcCommand::DeleteSession {
            target: miyu_core::ipc::SessionRef::Id {
                id: session_id.to_string(),
            },
        },
    )
    .await;
}

/// Deletes the throwaway session however the direct-mode turn unwinds — error,
/// cancelled question, or early return.
pub(in crate::cli) struct EphemeralSessionGuard {
    pub(in crate::cli) state: StateStore,
    pub(in crate::cli) session_id: String,
}

impl Drop for EphemeralSessionGuard {
    fn drop(&mut self) {
        miyu_core::llm::forget_relay_sessions(&self.session_id);
        let _ = self.state.delete_session(&self.session_id);
    }
}

pub(in crate::cli) struct RemoteTurnSummary {
    pub(in crate::cli) result: ChatResult,
    pub(in crate::cli) context_tokens: u64,
    pub(in crate::cli) context_window: Option<usize>,
    pub(in crate::cli) cumulative_tokens: TurnTokens,
}

/// Marker error for a remote turn interrupted by the user (Ctrl+C) or a
/// cancel from another client. The REPL catches it and returns to the prompt
/// instead of exiting; one-shot mode surfaces it as a normal error message.
///
/// 带着 daemon 在 `run.cancelled` 里报回的上下文数（有的话）：REPL 拿它刷 footer，
/// 不必再同步问一次 daemon——那一次 daemon 要为这条会话现造 agent、重估上下文，
/// 长会话里「已取消」之后输入框要等一两秒才回来（09-23）。
#[derive(Debug, Default)]
pub(in crate::cli) struct RemoteTurnCancelled {
    pub(in crate::cli) context: Option<CancelledContext>,
}

/// `run.cancelled` 里的上下文数，键名与 `run.completed` 相同。窗口不在其中：
/// 取消不会改变窗口。
#[derive(Clone, Copy, Debug)]
pub(in crate::cli) struct CancelledContext {
    pub(in crate::cli) context_tokens: u64,
    pub(in crate::cli) cumulative_tokens: TurnTokens,
}

impl CancelledContext {
    /// daemon 只在这一轮更新了上下文时才带数；没带就是 None。
    pub(in crate::cli) fn from_event(data: &serde_json::Value) -> Option<Self> {
        let number = |key: &str| data.get(key).and_then(serde_json::Value::as_u64);
        Some(Self {
            context_tokens: number("context_tokens")?,
            cumulative_tokens: TurnTokens {
                total: number("cumulative_tokens").unwrap_or_default(),
                prompt: number("cumulative_prompt_tokens").unwrap_or_default(),
                cache_read: number("cumulative_cache_read_tokens").unwrap_or_default(),
            },
        })
    }
}

/// 取消错误里带回来的上下文数。
pub(in crate::cli) fn cancelled_context(error: &anyhow::Error) -> Option<CancelledContext> {
    error
        .downcast_ref::<RemoteTurnCancelled>()
        .and_then(|cancelled| cancelled.context)
}

impl std::fmt::Display for RemoteTurnCancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(t("cancelled", "已取消"))
    }
}

impl std::error::Error for RemoteTurnCancelled {}

/// 前端退出但回合继续:daemon 拥有回合,REPL 只是观众离席(验收:
/// dsh 语义,前端退出任务照跑)。
#[derive(Debug)]
pub(in crate::cli) struct RemoteTurnDetached;

impl std::fmt::Display for RemoteTurnDetached {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(t("detached", "已脱离"))
    }
}

impl std::error::Error for RemoteTurnDetached {}

/// 回合跑着的时候敲了一条要占屏的斜杠命令（用户 09-20：「应该区分能执行和
/// 不能执行的命令」）。
///
/// 这一轮**分离到后台**（daemon 照跑），由上层执行这条命令，执行完再按
/// `last_event_id` 挂回来接着看——已经看过的那半截不会再来一遍。
///
/// 走「分离」而不是就地执行，是因为命令的实现都挂在 `RemoteRepl` 上（它同时
/// 持有活动区的可变借用），回合循环够不到；在回合循环里重写一份就是第二套
/// 事实来源。代价是正文上会留一道接缝（渲染器收口再重开）。
///
/// `/models` `/session` 的面板不走这条（09-20）：它们寄宿在回合循环里跑完，
/// 只有「人挑了另一条会话」这一件事回合循环自己做不了，才借这个错误把选好的
/// 会话带回 `RemoteRepl`（见 [`SuspendedAction::SwitchSession`]）。
#[derive(Debug)]
pub(in crate::cli) struct RemoteTurnSuspended {
    pub(in crate::cli) action: SuspendedAction,
    pub(in crate::cli) run_id: String,
    /// 最后看到的事件号；挂回来时从它之后接着看。0 = 一个都没看到。
    pub(in crate::cli) last_event_id: u64,
    pub(in crate::cli) session_id: String,
}

/// 回合循环暂离之后要上层做的事。
#[derive(Debug, Clone)]
pub(in crate::cli) enum SuspendedAction {
    /// 命令还没执行：交给 `RemoteRepl::dispatch_slash`，做完按事件号挂回来。
    Command {
        command: miyu_core::slash_commands::ReplSlashCommand,
        args: String,
    },
    /// `/session` 面板已经在回合里跑完、人挑了另一条会话（09-20）：换会话要
    /// 动 footer / 历史 / 车道，只有 `RemoteRepl` 做得了。换走之后这一轮不再
    /// 跟——它在 daemon 里继续跑，属于原来那条会话。
    SwitchSession(ipc::SessionState),
}

impl std::fmt::Display for RemoteTurnSuspended {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(t("suspended for a command", "为执行命令暂离"))
    }
}

impl std::error::Error for RemoteTurnSuspended {}

pub(in crate::cli) fn take_remote_turn_suspended(
    error: &anyhow::Error,
) -> Option<&RemoteTurnSuspended> {
    error.downcast_ref::<RemoteTurnSuspended>()
}

pub(in crate::cli) fn is_remote_turn_detached(error: &anyhow::Error) -> bool {
    error.downcast_ref::<RemoteTurnDetached>().is_some()
}

pub(in crate::cli) fn is_remote_turn_cancelled(error: &anyhow::Error) -> bool {
    error.downcast_ref::<RemoteTurnCancelled>().is_some()
}

#[allow(clippy::too_many_arguments)]
/// 触发终端指纹。shellhook/单次 CLI 的 stdin 常被管道占用(--stdin 喂正文),
/// 所以按 stderr→stdout→stdin 找第一个 tty;父进程就是触发它的 shell。后台任务
/// 完成后 daemon 凭这份指纹校验「shell 还活着、仍在这个 tty、空闲在提示符」,
/// 才把跟进回复写回终端。检测不到(纯管道/重定向/cron)就不带。
pub(in crate::cli) fn detect_origin_tty() -> Option<miyu_core::ipc::OriginTty> {
    let fd = [2, 1, 0]
        .into_iter()
        .find(|&fd| unsafe { libc::isatty(fd) } == 1)?;
    let path = std::fs::read_link(format!("/proc/self/fd/{fd}")).ok()?;
    if !path.starts_with("/dev/") {
        return None;
    }
    Some(miyu_core::ipc::OriginTty {
        path,
        shell_pid: std::os::unix::process::parent_id(),
    })
}

pub(in crate::cli) async fn send_ipc_command(paths: &MiyuPaths, command: IpcCommand) -> Result<()> {
    let mut stream = ipc::connect(&paths.ipc_socket()).await?;
    ipc::send(&mut stream, &IpcRequest::new(command)).await?;
    validate_ipc_command_response(ipc::receive::<IpcFrame>(&mut stream).await?)
}

pub(in crate::cli) fn validate_ipc_command_response(frame: Option<IpcFrame>) -> Result<()> {
    match frame {
        Some(IpcFrame::Ack) | Some(IpcFrame::Ready { .. }) | Some(IpcFrame::AdminResult { .. }) => {
            Ok(())
        }
        Some(IpcFrame::Error { message, .. }) => bail!("{message}"),
        Some(other) => bail!("Miyu core returned an unexpected response: {other:?}"),
        None => bail!("Miyu core closed the connection without a response"),
    }
}

/// Refreshes REPL-local state after the daemon switched to another session:
/// input history, queue tray, and the footer's token accounting.
/// Writes one line of REPL feedback through the live tail so the output
/// cursor stays in sync; never use bare `println!` inside the remote REPL.
/// 一句话的状态提示。
///
/// 全屏下短提示走**通知条**（浮在输入框上方，几秒后自己消失），长的照旧进正文。
/// 判据是行数：`/help` 那种整页清单浮起来没法看，而「已取消」写进正文只会让
/// 回翻时满屏都是碎片。
/// 回合失败写给人看的那一段。
///
/// 报错现在是多行的（一句结论 + 每个端点一行 + 一句该怎么办，见
/// `chat::all_endpoints_failed_message`），原来那句
/// `eprintln!("\x1b[31m错误: {err}")` 会把它糊成一大坨红字。这里给它和工具失败
/// 同一套视觉语言：抬头带错误图标、明细压暗缩进。
pub(in crate::cli) fn error_frame(error: &impl std::fmt::Display) -> String {
    let text = error.to_string();
    let mut lines = text.lines();
    let headline = lines.next().unwrap_or_default();
    let mut frame = format!(
        "\x1b[31m{} {}: {headline}\x1b[0m\n",
        miyu_hosts::render::timeline::glyph_err(),
        t("error", "错误")
    );
    for line in lines {
        // 明细压暗:它们是给排查用的,别和结论抢眼睛(暗色要 SGR2 + 灰 245,
        // 光 SGR2 在 kitty 里看不出)。
        frame.push_str(&format!("\x1b[2m\x1b[38;5;245m{line}\x1b[0m\n"));
    }
    frame
}

pub(in crate::cli) fn repl_note(live: &mut LiveReplTail, text: &str) -> Result<()> {
    if live.toast_note(text) {
        return Ok(());
    }
    live.apply_output_frame(format!("{text}\n").as_bytes())
}

/// Client-side display fallback for sessions the server has not named yet.
pub(in crate::cli) fn display_session_name(name: &str) -> &str {
    if name.trim().is_empty() {
        t("New session", "新会话")
    } else {
        name
    }
}

/// 会话有没有可见回合。空会话挂 banner、Tab 可换车道;读不到就当非空(保守)。
pub(in crate::cli) fn session_is_empty(paths: &MiyuPaths, session_id: &str) -> bool {
    // 只问有没有可见回合：原来把整条会话连大 JSON 列一起读出来再看长度，每换一次
    // 会话都是一次整段读（09-23）。读不到照旧当非空。
    StateStore::new(paths).is_ok_and(|store| store.session_is_empty(session_id))
}

/// 全屏：把这条会话最近几轮（`display.repl_replay_turns`）按当前宽度重画到正文
/// 顶上。换会话、撤销之后都走它——画布已经擦过了，屏上只该有库里现在还有的东西。
pub(in crate::cli) fn replay_recent_turns(
    config: &AppConfig,
    mode: PersonaLane,
    store: &StateStore,
    live_repl: &mut LiveReplTail,
) -> Result<()> {
    if config.display.repl_replay_turns == 0 {
        return Ok(());
    }
    match store.session_replay(config.display.repl_replay_turns) {
        Ok(replays) if !replays.is_empty() => {
            let (cols, _) = terminal::size().unwrap_or((80, 24));
            let cols = crate::cli::content_viewport()
                .map(|(cols, _)| cols)
                .unwrap_or(cols);
            let endpoint_line = crate::cli::model_cmds::show_mixed_model_endpoint(
                &crate::cli::model_cmds::session_scoped_config(store, config),
                true,
            );
            let frame = session_replay_frame(
                &replays,
                mode,
                config,
                usize::from(cols.max(1)),
                endpoint_line,
            )?;
            live_repl.apply_output_frame(&frame)?;
        }
        Ok(_) => {}
        Err(error) => tracing::debug!(error = %error, "session replay unavailable"),
    }
    Ok(())
}

/// 撤销之后把撤掉的那一轮从屏上拿掉（用户 09-18：「/undo 并没有去掉那条消息已经
/// 渲染出来的内容」）。
///
/// 全屏：正文缓冲截回这一轮开头的标记处——**只截这一轮**，前面的滚动历史原样
/// 留着，往上翻还在（用户：整段回放会把历史丢掉，不行）。缓冲里找不到标记
///（这一屏不是本进程画的）才退回换画布 + 回放最近几轮。撤成空会话就回大厅。
/// inline 擦不掉已经打出去的，只留那行「已撤销」。
pub(in crate::cli) fn redraw_after_undo(
    paths: &MiyuPaths,
    config: &AppConfig,
    mode: PersonaLane,
    session_id: &str,
    live_repl: &mut LiveReplTail,
) -> Result<()> {
    if !crate::cli::in_fullscreen() {
        return Ok(());
    }
    let empty = session_is_empty(paths, session_id);
    if empty {
        // 回大厅（`set_session_empty` 里顺手丢画布）。
        live_repl.set_session_empty(config, paths, true);
        return Ok(());
    }
    let truncated = synchronized_terminal_update(CursorAfterUpdate::Preserve, || {
        live_repl.truncate_last_turn()
    })?;
    if truncated {
        return Ok(());
    }
    synchronized_terminal_update(CursorAfterUpdate::Preserve, || live_repl.wipe_transcript())?;
    let store = StateStore::new(paths)?.pinned(session_id);
    replay_recent_turns(config, mode, &store, live_repl)
}

pub(in crate::cli) async fn apply_repl_session_switch(
    paths: &MiyuPaths,
    config: &AppConfig,
    mode: PersonaLane,
    state: &ipc::SessionState,
    active_session_id: &mut String,
    history: &mut Vec<ReplHistoryEntry>,
    live_repl: &mut LiveReplTail,
    footer: &mut ReplFooterStatus,
    cumulative_tokens: &mut TurnTokens,
) -> Result<()> {
    if state.session_id.is_empty() {
        bail!("{}", t("session state has no id", "会话状态缺少 ID"));
    }
    let store = StateStore::new(paths)?.pinned(&state.session_id);
    active_session_id.clone_from(&state.session_id);
    // 换了会话，显示就是这条会话自己的车道：大厅里按 Tab 换的那一下作废。
    live_repl.lobby_lane_pending = false;
    *history = load_repl_input_history(&store, paths)?;
    live_repl.editor.history = history.clone();
    live_repl.editor.history_index = live_repl.editor.history.len();
    live_repl.editor.history_clean_index = None;
    live_repl.editor.input.clear();
    live_repl.editor.cursor = 0;
    // 只读是会话自己的开关(09-23),换到哪个会话就显示哪个会话的。
    live_repl.set_readonly(state.sandbox_readonly);
    // 每一次换会话都经过这里:空会话挂 banner、Tab 可换车道,非空就钉死。
    let empty = session_is_empty(paths, &state.session_id);
    live_repl.set_session_empty(config, paths, empty);
    // 全屏：换会话就换画布。上一个会话的正文整个丢掉，目标会话最近几轮回放到
    // 屏顶——新会话就是一张空画布（大厅），切回旧会话能看到它的对话（用户实测：
    // /new 不清屏，看着还是旧会话）。正文顶部对齐之后不能再用「顶出视口」：
    // 回放会缩在屏底、上面一大截空白。空会话的画布在 set_session_empty 里已经
    // 丢过了。inline 照旧只打一行提示。
    let fullscreen = crate::cli::in_fullscreen();
    if fullscreen && !empty {
        synchronized_terminal_update(CursorAfterUpdate::Preserve, || live_repl.wipe_transcript())?;
    }
    if !std::mem::take(&mut live_repl.suppress_switch_note) {
        repl_note(
            live_repl,
            &format!(
                "\x1b[2m{}: {}\x1b[0m\n",
                t("switched to session", "已切换到会话"),
                display_session_name(&state.session_name)
            ),
        )?;
    }
    if fullscreen && !empty {
        replay_recent_turns(config, mode, &store, live_repl)?;
    }
    synchronized_terminal_update(CursorAfterUpdate::Shown, || live_repl.reload_queue(&store))?;
    // Rebuild rather than reset: the target session may pin its own model
    // pool, so provider/model/thinking have to be re-derived alongside the
    // token numbers. `refresh_footer` repaints straight away — merely storing
    // the footer left the previous session's numbers on screen until the next
    // turn finished.
    *cumulative_tokens = state_cumulative(&state);
    let session_config = footer_config_for_session(paths, config, &state.session_id);
    *footer =
        ReplFooterStatus::from_config(&session_config, state.context_tokens, *cumulative_tokens);
    let client = OpenAiCompatibleClient::from_config(&session_config, paths)?;
    footer.update_thinking_variant(client.thinking_variant_summary().as_deref());
    footer.update_context_window(state.context_window, state.context_window_assumed);
    live_repl.refresh_footer(footer.clone())?;
    // Every REPL session change funnels through here, so this is the one place
    // the REPL lane needs to be remembered. Best effort: losing the write only
    // means the next REPL starts on the terminal session.
    let _ = await_in_lobby(
        live_repl,
        send_ipc_admin(
            paths,
            IpcCommand::SetReplSession {
                target: miyu_core::ipc::SessionRef::Id {
                    id: state.session_id.clone(),
                },
            },
        ),
    )
    .await;
    Ok(())
}

/// One row of the daemon's session list, parsed from `ListSessions` JSON.
#[derive(Clone, Debug)]
pub(in crate::cli) struct SessionListEntry {
    pub(in crate::cli) id: String,
    pub(in crate::cli) name: String,
    pub(in crate::cli) is_current: bool,
    pub(in crate::cli) turns: u64,
    pub(in crate::cli) snippet: String,
    /// `/sandbox` 绑的根;None = 没绑。
    pub(in crate::cli) sandbox: Option<String>,
    /// 绑的时候给了 `--allow-read`:只锁写,读不设限。
    pub(in crate::cli) sandbox_read_all: bool,
    /// "dev" | "normal",由 daemon 按会话人格推导。
    pub(in crate::cli) mode: String,
    /// 当前上下文(词元):当前会话是活数,其余是库里最近一轮记的;还没跑过回合为 None。
    pub(in crate::cli) context_tokens: Option<u64>,
}

pub(in crate::cli) fn session_list_entries(data: &serde_json::Value) -> Vec<SessionListEntry> {
    data.get("sessions")
        .and_then(serde_json::Value::as_array)
        .map(|sessions| sessions.iter().map(session_list_entry).collect())
        .unwrap_or_default()
}

pub(in crate::cli) fn session_list_entry(session: &serde_json::Value) -> SessionListEntry {
    let text = |key: &str| {
        session
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    };
    SessionListEntry {
        id: text("session_id").unwrap_or_default(),
        name: text("name").unwrap_or_default(),
        is_current: session
            .get("is_current")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        turns: session
            .get("turn_count")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        snippet: text("last_user_content")
            .map(|content| {
                let cleaned = content.trim().replace(['\n', '\r'], " ");
                let truncated: String = cleaned.chars().take(24).collect();
                if cleaned.chars().count() > 24 {
                    format!("{truncated}…")
                } else {
                    truncated
                }
            })
            .unwrap_or_default(),
        sandbox: text("sandbox"),
        sandbox_read_all: session
            .get("sandbox_read_all")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        mode: text("mode").unwrap_or_else(|| "normal".to_string()),
        context_tokens: session
            .get("context_tokens")
            .and_then(serde_json::Value::as_u64),
    }
}

/// Maps a user-facing 1-based session number to a session id ref.
pub(in crate::cli) fn session_ref_from_index(
    entries: &[SessionListEntry],
    index: usize,
) -> Option<miyu_core::ipc::SessionRef> {
    index
        .checked_sub(1)
        .and_then(|index| entries.get(index))
        .map(|entry| miyu_core::ipc::SessionRef::Id {
            id: entry.id.clone(),
        })
}

pub(in crate::cli) fn session_entry_is_active(
    entry: &SessionListEntry,
    active_session_id: Option<&str>,
) -> bool {
    active_session_id.map_or(entry.is_current, |session_id| entry.id == session_id)
}

pub(in crate::cli) fn session_select_line(
    entry: &SessionListEntry,
    active_session_id: Option<&str>,
) -> String {
    let marker = if session_entry_is_active(entry, active_session_id) {
        "* "
    } else {
        "  "
    };
    // 09-18 定版:「模式 · 当前上下文 · 标题」(用户:上下文只要当前的数,不要模型
    // 窗口;摘要不进行,搜索仍认它)。此前是「模式:名称 · 摘要」。
    let mut line = format!(
        "{marker}{} · {} · {}",
        session_mode_label(&entry.mode),
        session_context_label(entry),
        display_session_name(&entry.name),
    );
    line.push_str(&sandbox_tag(entry));
    line
}

/// 列表行里的「当前上下文」:12k 这种短写;还没跑过回合的会话给 0。
pub(in crate::cli) fn session_context_label(entry: &SessionListEntry) -> String {
    render::format_compact_count(entry.context_tokens.unwrap_or(0))
}

/// 列表行尾的沙盒标。读放开的会话要看得出来——否则「关进去了」和「只关了写」
/// 在列表里长得一模一样。
pub(in crate::cli) fn sandbox_tag(entry: &SessionListEntry) -> String {
    match (&entry.sandbox, entry.sandbox_read_all) {
        (Some(root), true) => format!("  [sandbox {root}, {}]", t("writes only", "只锁写")),
        (Some(root), false) => format!("  [sandbox {root}]"),
        (None, _) => String::new(),
    }
}

pub(in crate::cli) fn session_select_search(entry: &SessionListEntry) -> String {
    format!(
        "{} {} {} {}",
        display_session_name(&entry.name),
        session_mode_label(&entry.mode),
        entry.snippet,
        entry.sandbox.as_deref().unwrap_or_default()
    )
}

/// 会话类型标(验收:列表看不出普通/开发)。
pub(in crate::cli) fn session_mode_label(mode: &str) -> &'static str {
    if mode == "dev" {
        t("dev", "开发")
    } else {
        t("normal", "普通")
    }
}

pub(in crate::cli) fn session_initial_selection(
    entries: &[SessionListEntry],
    active_session_id: Option<&str>,
) -> usize {
    entries
        .iter()
        .position(|entry| session_entry_is_active(entry, active_session_id))
        .unwrap_or(0)
}

/// What the interactive session picker came back with.
pub(in crate::cli) enum SessionPick {
    Cancelled,
    Switch(miyu_core::ipc::SessionRef),
    /// Deletion confirmed inside the picker. `index` is where the cursor sat,
    /// so the caller can reopen the refreshed list at the same spot.
    Delete {
        session_id: String,
        index: usize,
    },
}

pub(in crate::cli) fn select_session_target(
    entries: &[SessionListEntry],
    active_session_id: Option<&str>,
    cursor: Option<usize>,
) -> Result<SessionPick> {
    let lines = entries
        .iter()
        .map(|entry| session_select_line(entry, active_session_id))
        .collect::<Vec<_>>();
    let search = entries
        .iter()
        .map(session_select_search)
        .collect::<Vec<_>>();
    let labels = entries
        .iter()
        .map(|entry| display_session_name(&entry.name).to_string())
        .collect::<Vec<_>>();
    let initial = cursor
        .map(|index| index.min(entries.len().saturating_sub(1)))
        .unwrap_or_else(|| session_initial_selection(entries, active_session_id));
    Ok(
        match inline_single_select_deletable(
            t("Select session", "选择会话"),
            &lines,
            &search,
            initial,
            Some(&labels),
        )? {
            InlineSelectOutcome::Cancelled => SessionPick::Cancelled,
            InlineSelectOutcome::Chosen(index) => {
                SessionPick::Switch(miyu_core::ipc::SessionRef::Id {
                    id: entries[index].id.clone(),
                })
            }
            InlineSelectOutcome::Deleted(index) => SessionPick::Delete {
                session_id: entries[index].id.clone(),
                index,
            },
        },
    )
}

/// Resolves a user-typed `/session` / `/delete` argument into a session ref:
/// a number picks from the visible session list, anything else is a name.
/// REPL 会话列表的作用域：普通 + 开发两侧合并（daemon 的 `all` 档，管理面
/// `miyu session list`、WebUI 侧栏、模型的 session 工具早就这么列）。原来这儿只可能
/// 给 `None`/`"dev"`，普通模式看不见开发会话、反之亦然（用户 09-17）。每行本来
/// 就带「普通/开发」标签；选中另一侧的会话时车道跟着切（`switch_to_session`）。
pub(in crate::cli) fn repl_list_mode(_mode: PersonaLane) -> Option<String> {
    Some("all".to_string())
}

/// `/session` 列表的次序：当前车道的会话排前面，另一侧的排后面（各自仍按 daemon
/// 给的更新时间序）。两侧合并之后按时间混排，开发/普通交错着看着乱（用户 09-18
/// 截图）。菜单和 `/session <序号>` 都按这个顺序编号。
pub(in crate::cli) fn order_entries_for_lane(
    entries: Vec<SessionListEntry>,
    mode: PersonaLane,
) -> Vec<SessionListEntry> {
    let mine = if mode.is_dev() { "dev" } else { "normal" };
    let (first, rest): (Vec<_>, Vec<_>) = entries.into_iter().partition(|entry| entry.mode == mine);
    first.into_iter().chain(rest).collect()
}

/// REPL 的 `/session` 看得见的那些会话：**不含「终端集成会话」**。
///
/// 用户 09-20：「终端集成会话不应该出现在 /session 里」。它是 shell 无缝对话
/// 那条路的会话（`default`），REPL 压根不会停在它上面——`ensure_repl_session`
/// 把指到它的指针视同缺失、就地自举一条新的。既然进不去，列出来只会让人误选，
/// 还会在「当前会话被删掉」时被兜底逻辑挑中（用户实测：回车跳进了终端集成会话）。
/// 要用它还是走 shell 那条路或 `miyu session`。
pub(in crate::cli) fn repl_visible_entries(
    data: &serde_json::Value,
    mode: PersonaLane,
) -> Vec<SessionListEntry> {
    let entries = session_list_entries(data)
        .into_iter()
        .filter(|entry| entry.id != miyu_core::state::DEFAULT_SESSION_ID)
        .collect();
    order_entries_for_lane(entries, mode)
}

pub(in crate::cli) async fn resolve_repl_session_target(
    paths: &MiyuPaths,
    live: &mut LiveReplTail,
    mode: PersonaLane,
    arg: &str,
) -> Result<Option<miyu_core::ipc::SessionRef>> {
    let index = arg.parse::<usize>().ok();
    // 名字寻址在 daemon 侧按"当前人格"检索,够不着另一侧的会话;统一走列表
    // （两侧合并）在客户端配对,再降成不可猜的 id 显式寻址。
    let Some((_, data)) = repl_ipc_admin(
        paths,
        live,
        IpcCommand::ListSessions {
            mode: repl_list_mode(mode),
        },
    )
    .await?
    else {
        return Ok(None);
    };
    let entries = repl_visible_entries(&data, mode);
    let target = match index {
        Some(index) => session_ref_from_index(&entries, index),
        None => entries.iter().find(|entry| entry.name == arg).map(|entry| {
            miyu_core::ipc::SessionRef::Id {
                id: entry.id.clone(),
            }
        }),
    };
    let Some(target) = target else {
        repl_note(
            live,
            &format!(
                "\x1b[2m{}: {arg}\x1b[0m\n",
                t("no such session", "没有这个会话")
            ),
        )?;
        return Ok(None);
    };
    Ok(Some(target))
}

pub(in crate::cli) fn reload_repl_queue(
    live: &mut LiveReplTail,
    paths: &MiyuPaths,
    session_id: &str,
) -> Result<()> {
    let store = StateStore::new(paths)?.pinned(session_id);
    synchronized_terminal_update(CursorAfterUpdate::Shown, || live.reload_queue(&store))
}

pub(in crate::cli) fn confirm_inline(live: &mut LiveReplTail, prompt: &str) -> Result<bool> {
    live.apply_output_frame(format!("{prompt} [y/N] ").as_bytes())?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes" | "YES"))
}

pub(in crate::cli) fn confirm_stdin(prompt: &str) -> Result<bool> {
    print!("{prompt} [y/N] ");
    io::stdout().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes" | "YES"))
}

/// 在大厅里等一个 future(多半是 daemon 的应答)时,每 40ms 推一帧 banner——
/// 星空与扫光不因为「命令在等 daemon」而定格。没有 banner(会话视图、inline)
/// 就是普通的 await。
///
/// 09-17 用户报的「/config 退出后重载配置那几秒动画停了」就是这段:大厅先画
/// 回来,然后 REPL 等 ReloadConfig(真机上要重载 MCP 等,几秒),泵没在跑,
/// 画面定格。
pub(in crate::cli) async fn await_in_lobby<T>(
    live: &mut LiveReplTail,
    future: impl std::future::Future<Output = T>,
) -> T {
    if live.banner.is_none() {
        return future.await;
    }
    tokio::pin!(future);
    let mut ticker = tokio::time::interval(std::time::Duration::from_millis(40));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            result = &mut future => return result,
            _ = ticker.tick() => {
                let _ = live.tick_banner();
            }
        }
    }
}

/// Tab / Shift+Tab 切只读(用户 09-23)。发给 daemon,成功就翻状态行上那两个字——
/// 不另打回执(用户 09-23:「只读已开：能读，哪儿都写不了」这种通知 AI 味太重,
/// 状态行本身就是回执)。只有失败才说一句。
///
/// 回合进行中也走这里(`in_turn`):不用分离回合,daemon 那边下一次工具调用就按新
/// 设置来。全屏用 toast;inline 空闲时退回打一行,回合中不往正文里插字。
pub(in crate::cli) async fn toggle_repl_readonly(
    paths: &MiyuPaths,
    live: &mut LiveReplTail,
    session_id: &str,
    in_turn: bool,
) -> Result<()> {
    let next = !live.editor.readonly;
    let command = IpcCommand::SetSandboxReadonly {
        target: miyu_core::ipc::SessionRef::Id {
            id: session_id.to_string(),
        },
        readonly: next,
    };
    match await_in_lobby(live, send_ipc_admin(paths, command)).await {
        Ok(_) => {
            live.set_readonly(next);
            if !live.external_output_active {
                synchronized_terminal_update(CursorAfterUpdate::Preserve, || live.redraw())?;
            }
        }
        Err(error) => {
            let text = format!(
                "{}: {error:#}",
                t("could not toggle read-only", "切换只读失败")
            );
            if !live.toast_note_at(&text, true) && !in_turn {
                repl_note(live, &format!("\x1b[31m{text}\x1b[0m\n"))?;
            }
        }
    }
    Ok(())
}

/// Sends an admin command from inside the REPL loop, printing failures (core
/// busy, core restarting, …) through the live tail instead of propagating
/// them so the REPL survives.
pub(in crate::cli) async fn repl_ipc_admin(
    paths: &MiyuPaths,
    live: &mut LiveReplTail,
    command: IpcCommand,
) -> Result<Option<(ipc::SessionState, serde_json::Value)>> {
    match await_in_lobby(live, send_ipc_admin(paths, command)).await {
        Ok(result) => Ok(Some(result)),
        Err(err) => {
            repl_note(live, &error_frame(&err))?;
            Ok(None)
        }
    }
}

pub(in crate::cli) async fn repl_get_session_state(
    paths: &MiyuPaths,
    live: &mut LiveReplTail,
    target: miyu_core::ipc::SessionRef,
) -> Result<Option<ipc::SessionState>> {
    // 带上 REPL 的当前目录:下一轮 StartTurn 也带它,默认沙盒的根跟着它走。
    let cwd = std::env::current_dir().ok();
    Ok(
        repl_ipc_admin(paths, live, IpcCommand::GetSessionState { target, cwd })
            .await?
            .map(|(state, _)| state),
    )
}

/// Resolve a user-requested switch without replaying the session already on
/// screen. Other state refreshes still use `repl_get_session_state` directly.
pub(in crate::cli) async fn repl_get_session_switch(
    paths: &MiyuPaths,
    live: &mut LiveReplTail,
    target: miyu_core::ipc::SessionRef,
    active_session_id: &str,
) -> Result<Option<ipc::SessionState>> {
    if matches!(&target, miyu_core::ipc::SessionRef::Id { id } if id == active_session_id) {
        return Ok(None);
    }
    Ok(repl_get_session_state(paths, live, target)
        .await?
        .filter(|state| state.session_id != active_session_id))
}

pub(in crate::cli) async fn repl_fallback_session_state(
    paths: &MiyuPaths,
    live: &mut LiveReplTail,
    mode: PersonaLane,
) -> Result<Option<ipc::SessionState>> {
    // 两条车道都走 `GetReplSession`：它会治愈死掉的指针（刚被删的那条），
    // 没有可用的就地自举一条新的，而且**绝不会给出终端集成会话**
    // （`ensure_repl_session` 把指到它的指针视同缺失）。
    //
    // 普通车道原来是「列会话 → 挑 `is_current` 或第一条」。`is_current` 说的是
    // **daemon 的当前会话**，那通常正是终端集成会话——于是删掉当前会话之后
    // 一回车就跳了进去（用户 09-20 实测）。
    Ok(repl_ipc_admin(
        paths,
        live,
        IpcCommand::GetReplSession {
            mode: mode.is_dev().then(|| "dev".to_string()),
            // 兜底取回一条能用的会话，不是启动。
            fresh: false,
        },
    )
    .await?
    .map(|(state, _)| state))
}

/// Runs the interactive session picker inside the REPL, servicing Ctrl+D
/// deletions in place. Returns the session state to switch to — a fallback
/// session when the REPL's own session was one of the ones deleted, so backing
/// out never strands the REPL on a session that no longer exists.
pub(in crate::cli) async fn repl_pick_session(
    paths: &MiyuPaths,
    live: &mut LiveReplTail,
    mode: PersonaLane,
    active_session_id: &str,
) -> Result<Option<ipc::SessionState>> {
    let mut cursor = None;
    // 09-20 起「删掉自己待着的那条」当场就返回兜底会话（见下面的 Delete
    // 分支），所以循环里不再有「我的会话已经没了但还在挑」这个状态——原来
    // 那个 `lost_active` 标记随之消失。
    loop {
        let Some((_, data)) = repl_ipc_admin(
            paths,
            live,
            IpcCommand::ListSessions {
                mode: repl_list_mode(mode),
            },
        )
        .await?
        else {
            return Ok(None);
        };
        let entries = repl_visible_entries(&data, mode);
        if entries.is_empty() {
            repl_note(
                live,
                &format!("\x1b[2m{}\x1b[0m\n", t("no sessions", "没有会话")),
            )?;
            return Ok(None);
        }
        let picked = if live.screen.is_some() {
            super::session_picker::pick(live, &entries, active_session_id, cursor)
        } else {
            synchronized_terminal_update(CursorAfterUpdate::Hidden, || live.suspend())?;
            let picked = select_session_target(&entries, Some(active_session_id), cursor);
            synchronized_terminal_update(CursorAfterUpdate::Shown, || live.resume())?;
            picked
        };
        match picked? {
            SessionPick::Cancelled => return Ok(None),
            SessionPick::Switch(target) => {
                return repl_get_session_switch(paths, live, target, active_session_id).await;
            }
            SessionPick::Delete { session_id, index } => {
                let was_active = session_id == active_session_id;
                let deleted = repl_ipc_admin(
                    paths,
                    live,
                    IpcCommand::DeleteSession {
                        target: miyu_core::ipc::SessionRef::Id { id: session_id },
                    },
                )
                .await?;
                if deleted.is_none() {
                    return Ok(None);
                }
                // 删掉的是**自己正待着**的那条：当场离开，别继续挂在一条已经
                // 不存在的会话的画面上接着挑（用户 09-20 实测：删完还看得见被
                // 删会话的正文）。落到本车道的一条可用会话上，没有就自举一条
                // 新的空会话——`repl_fallback_session_state` 管这件事。
                if was_active {
                    return repl_fallback_session_state(paths, live, mode).await;
                }
                // The rows below shift up, so holding the index parks the
                // cursor on the next session instead of jumping to the top.
                cursor = Some(index);
            }
        }
    }
}

pub(in crate::cli) async fn repl_active_or_default_state(
    paths: &MiyuPaths,
    active_session_id: &str,
) -> Result<(ipc::SessionState, bool)> {
    match send_ipc_admin(
        paths,
        IpcCommand::GetSessionState {
            target: miyu_core::ipc::SessionRef::Id {
                id: active_session_id.to_string(),
            },
            cwd: std::env::current_dir().ok(),
        },
    )
    .await
    {
        Ok((state, _)) => Ok((state, false)),
        Err(_) => {
            let (state, _) = send_ipc_admin(paths, IpcCommand::GetStatus).await?;
            let changed = state.session_id != active_session_id;
            Ok((state, changed))
        }
    }
}

/// 按**会话作用域**重算一份 footer：模型标签、思考档位、上下文窗口、累计
/// 词元都从会话钉的模型池推导（验收 #23 的同源约束）。返回 (footer, 累计)。
///
/// `/models` 改完会话模型要它；回合中寄宿的 `/models` 面板（09-20）也要它
/// ——两处各写一遍迟早分叉。取会话状态那一趟 IPC 套 `await_in_lobby`：大厅
/// 里开 `/models` 时星空不能定格（09-17）。
pub(in crate::cli) async fn session_footer_status(
    paths: &MiyuPaths,
    config: &AppConfig,
    live: &mut LiveReplTail,
    session_id: &str,
) -> Result<(ReplFooterStatus, TurnTokens)> {
    let session_config = footer_config_for_session(paths, config, session_id);
    let (state, _) = await_in_lobby(live, repl_active_or_default_state(paths, session_id)).await?;
    let cumulative = state_cumulative(&state);
    let mut footer =
        ReplFooterStatus::from_config(&session_config, state.context_tokens, cumulative);
    let client = OpenAiCompatibleClient::from_config(&session_config, paths)?;
    footer.update_thinking_variant(client.thinking_variant_summary().as_deref());
    footer.update_context_window(state.context_window, state.context_window_assumed);
    Ok((footer, cumulative))
}

/// Ensures the daemon is running, then sends one admin command; used by the
/// one-shot session subcommands (`miyu new/session/rename/...`).
pub(in crate::cli) async fn session_admin(
    paths: &MiyuPaths,
    command: IpcCommand,
) -> Result<(ipc::SessionState, serde_json::Value)> {
    session_admin_streaming(paths, command, |_, _| Ok(())).await
}

/// `session_admin` + 中途事件回调,见 [`send_ipc_admin_streaming`]。
pub(in crate::cli) async fn session_admin_streaming<F>(
    paths: &MiyuPaths,
    command: IpcCommand,
    on_event: F,
) -> Result<(ipc::SessionState, serde_json::Value)>
where
    F: FnMut(&str, &serde_json::Value) -> Result<()>,
{
    ipc::ensure_daemon(paths, None).await?;
    let refreshed = MiyuPaths::new()?;
    send_ipc_admin_streaming(&refreshed, command, on_event).await
}

/// `/goal edit`（无参数）的编辑器内变身：把「/goal edit <当前目标>」放进
/// 输入行，改几个字就能回车——终端里的「可编辑文本框」。
///
/// 必须在提交**之前**拦：提交会把原文回显成一条消息块，用户看到的是
/// 「/goal edit 被当作消息发出去了」。返回 true 表示已变身（调用方跳过这次
/// 提交并重绘输入行）；没有目标时返回 false，走正常提交让命令层去报错。
pub(in crate::cli) fn prefill_goal_edit_input(
    paths: &MiyuPaths,
    session_id: Option<&str>,
    live: &mut LiveReplTail,
) -> bool {
    let Some(session) = session_id else {
        return false;
    };
    let Some(objective) = StateStore::new(paths)
        .ok()
        .and_then(|store| store.goal(session).ok().flatten())
        .map(|goal| goal.objective)
    else {
        return false;
    };
    live.editor.input = format!("/goal edit {objective}");
    live.editor.cursor = live.editor.input.chars().count();
    live.editor.history_clean_index = None;
    true
}

pub(in crate::cli) async fn send_ipc_admin(
    paths: &MiyuPaths,
    command: IpcCommand,
) -> Result<(ipc::SessionState, serde_json::Value)> {
    send_ipc_admin_streaming(paths, command, |_, _| Ok(())).await
}

/// 同上,但把终局帧之前到达的事件逐条交给 `on_event`(kind, data)。
///
/// 管理面的绝大多数命令是一问一答,只有压缩会在中间吐 `context.compact_*`
/// ——它要跑一次完整的摘要调用,几十秒不吭声的话终端看着就是死的。所以这里
/// 收帧改成循环而不是只读一帧;不关心事件的调用方用上面那层薄壳,行为不变。
pub(in crate::cli) async fn send_ipc_admin_streaming<F>(
    paths: &MiyuPaths,
    command: IpcCommand,
    mut on_event: F,
) -> Result<(ipc::SessionState, serde_json::Value)>
where
    F: FnMut(&str, &serde_json::Value) -> Result<()>,
{
    let mut stream = ipc::connect(&paths.ipc_socket()).await?;
    ipc::send(&mut stream, &IpcRequest::new(command)).await?;
    loop {
        match ipc::receive::<IpcFrame>(&mut stream).await? {
            Some(IpcFrame::Event { kind, data, .. }) => on_event(&kind, &data)?,
            Some(IpcFrame::AdminResult { state, data }) => return Ok((state, data)),
            Some(IpcFrame::Error { message, .. }) => bail!("{message}"),
            _ => bail!("Miyu core returned an invalid admin response"),
        }
    }
}

/// 同 [`send_ipc_admin_streaming`],但等结果期间每隔 `tick_every` 调一次 `on_tick`
/// (footer 转轮要有人喂)。收帧放在单独的任务里经通道转过来:`ipc::receive`
/// 是按长度前缀分帧的,直接在 `select!` 里和定时器抢会把读到一半的帧丢掉。
pub(in crate::cli) async fn send_ipc_admin_streaming_ticked<F, T>(
    paths: &MiyuPaths,
    command: IpcCommand,
    mut on_event: F,
    tick_every: Duration,
    mut on_tick: T,
) -> Result<(ipc::SessionState, serde_json::Value)>
where
    F: FnMut(&str, &serde_json::Value) -> Result<()>,
    T: FnMut() -> Result<()>,
{
    let mut stream = ipc::connect(&paths.ipc_socket()).await?;
    ipc::send(&mut stream, &IpcRequest::new(command)).await?;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Result<Option<IpcFrame>>>();
    let reader = tokio::spawn(async move {
        loop {
            let frame = ipc::receive::<IpcFrame>(&mut stream).await;
            let terminal = !matches!(frame, Ok(Some(IpcFrame::Event { .. })));
            if tx.send(frame).is_err() || terminal {
                break;
            }
        }
    });
    let mut tick = tokio::time::interval(tick_every);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    tick.tick().await;
    let outcome = loop {
        tokio::select! {
            frame = rx.recv() => match frame {
                Some(Ok(Some(IpcFrame::Event { kind, data, .. }))) => {
                    if let Err(error) = on_event(&kind, &data) {
                        break Err(error);
                    }
                }
                Some(Ok(Some(IpcFrame::AdminResult { state, data }))) => break Ok((state, data)),
                Some(Ok(Some(IpcFrame::Error { message, .. }))) => {
                    break Err(anyhow::anyhow!("{message}"))
                }
                Some(Ok(Some(_))) => {
                    break Err(anyhow::anyhow!("Miyu core returned an invalid admin response"))
                }
                Some(Ok(None)) | None => {
                    break Err(anyhow::anyhow!("Miyu core closed the connection"))
                }
                Some(Err(error)) => break Err(error),
            },
            _ = tick.tick() => {
                if let Err(error) = on_tick() {
                    break Err(error);
                }
            }
        }
    };
    reader.abort();
    outcome
}

// `ipc_text` / `ipc_u64` 随解码表一起住到 `runtime::ipc_events`(09-16),
// 这里只转一手,cli 内几十处调用不动。
pub(in crate::cli) use miyu_hosts::runtime::{ipc_text, ipc_u64};

pub(in crate::cli) fn ipc_mode_name(mode: PersonaLane) -> &'static str {
    mode.mode_word()
}

pub(in crate::cli) fn ipc_images(
    images: &[Option<miyu_base::clipboard::PastedImage>],
) -> Vec<Option<miyu_core::ipc::ImageAttachment>> {
    images
        .iter()
        .map(|image| {
            image.as_ref().map(|image| match image {
                miyu_base::clipboard::PastedImage::Binary(image) => {
                    miyu_core::ipc::ImageAttachment::Binary {
                        mime: image.mime.clone(),
                        data: image.data.clone(),
                    }
                }
                miyu_base::clipboard::PastedImage::Path(path) => {
                    miyu_core::ipc::ImageAttachment::Path { path: path.clone() }
                }
            })
        })
        .collect()
}
