//! 交互式远端 REPL。
//!
//! 主循环：读输入 → 发 IPC → 收事件流 → 刷活动区，中间还要处理后台任务唤醒、
//! 排队消息、窗口变化。这是终端的日常路径。
//!
//! 事件分发表在这里维护，和 [`crate::cli::repl::live_turn`] 的本地泵是两份——
//! 加新事件时两边都要过一遍（08-17「footer 不刷新」就是漏了一边）。
//!
//! 09-17 拆分:循环外的十个状态收成 [`RemoteRepl`],斜杠命令住 `slash_*.rs`,发回合住 `submit.rs`;
//! 这里只剩「起 daemon、建状态、回放尾巴、主循环」。

use crate::cli::repl::editor::*;
use crate::cli::repl::input::*;
use crate::cli::repl::tail::*;
use crate::cli::*;

pub(in crate::cli) async fn run_remote_repl(paths: &MiyuPaths, mode: PersonaLane) -> Result<()> {
    let _cursor_restore = ReplCursorRestore;
    ipc::ensure_daemon(paths, None).await?;
    let refreshed = MiyuPaths::new()?;
    let paths = &refreshed;
    initialize_models_cache(paths);
    let config = AppConfig::load_or_default(paths)?;
    // REPL 走的是自己的车道(不是 shellhook 那条终端会话),而**启动**一律
    // 开新会话:用户 09-20 拍板,敲 `miyu` 要的是一张白纸,接着上次聊是
    // `/session` 的事。指针那条本来就空就原地复用(见 `fresh_repl_session`)。
    let (daemon_state, repl_session_data) = send_ipc_admin(
        paths,
        IpcCommand::GetReplSession {
            mode: mode.is_dev().then(|| "dev".to_string()),
            fresh: true,
        },
    )
    .await?;
    let active_session_id = daemon_state.session_id.clone();
    // 上键历史是**按会话**存的。启动开新会话后这条会话还什么都没有,历史得从
    // 被换掉的那条 REPL 会话接着来(daemon 在 `previous_repl_session` 里带回),
    // 否则每次敲 `miyu` 上键都调不出昨天说过的话。
    let history_source = repl_session_data
        .get("previous_repl_session")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(active_session_id.as_str());
    let history_state = StateStore::new(paths)?.pinned(history_source);
    let history = load_repl_input_history(&history_state, paths)?;
    drop(history_state);
    let cumulative_tokens = state_cumulative(&daemon_state);
    // footer 的模型标签与思考程度必须同源:都从会话作用域配置推导。
    // 曾经 client 用全局配置,标签显示会话覆盖模型、·max 却算的全局
    // 模型,两边各说各话(验收#23)。
    let session_config = footer_config_for_session(paths, &config, &active_session_id);
    let mut footer = ReplFooterStatus::from_config(
        &session_config,
        daemon_state.context_tokens,
        cumulative_tokens,
    );
    let thinking_summary = footer_thinking_summary(paths, &session_config, &active_session_id)?;
    footer.update_thinking_variant(thinking_summary.as_deref());
    footer.update_context_window(
        daemon_state.context_window,
        daemon_state.context_window_assumed,
    );
    let mut live_repl = LiveReplTail::new(mode, history.clone(), Vec::new(), footer.clone())?;
    live_repl.set_readonly(daemon_state.sandbox_readonly);
    // 空会话:挂 banner,Tab 可换车道;有过回合的会话直接是输入框。
    live_repl.set_session_empty(&config, paths, session_is_empty(paths, &active_session_id));
    let jobs_shared = spawn_jobs_poll_thread(paths.clone(), &active_session_id);
    let jobs_feed = JobsFeed::Shared(jobs_shared.clone());
    // 在 herdr 的 pane 里跑的话，侧栏这就多一行 `miyu`（不在就是 no-op）。
    // 带上会话 id：`herdr agent list` 会显示它，将来做「重启后恢复」也靠它指回来。
    herdr::report(herdr::HerdrState::Idle, None, Some(&active_session_id));
    herdr::set_terminal_title_for_session(paths, &active_session_id);

    // Terminal closed (SIGHUP) or process killed (SIGTERM): the graceful
    // exit path at the bottom never runs. 收尾（把 herdr 的 pane 还回去、恢复
    // raw mode）交给一根专门的线程，理由见 `exit_on_termination_signals`。
    crate::cli::exit_on_termination_signals();

    // Redraw the tail of the session we just resumed. The tail is not on
    // screen yet (`rendered == false`), so `apply_output_frame` writes the
    // frame raw and re-reads the cursor — no layout budget applies and the
    // frame can be arbitrarily long.
    let replay_store = StateStore::new(paths)?.pinned(&active_session_id);
    crate::cli::repl::session::replay_recent_turns(&config, mode, &replay_store, &mut live_repl)?;

    // 这条会话钉的模型被供应商下架了：daemon 已经退回全局池并把覆盖清掉，得说
    // 一声——不说的话 footer 上的模型悄悄换了人，看着像自己乱跳。放在回放之后，
    // 免得插在历史前面。
    if let Some(listed) = repl_session_data
        .get("stale_model_override")
        .and_then(|value| value.as_array())
        .map(|entries| {
            entries
                .iter()
                .filter_map(serde_json::Value::as_str)
                .collect::<Vec<_>>()
                .join("、")
        })
        .filter(|listed| !listed.is_empty())
    {
        let note = t(
            "this session pinned models the providers no longer list ({}); back to the global pool",
            "这条会话钉的模型已不在供应商清单里（{}），已退回全局模型池",
        )
        .replace("{}", &listed);
        // 直接写进正文而不是走 `repl_note`：那条路在全屏下会变成 2.2 秒就散的
        // 浮层提示，而这是一次性的状态变更（钉的池没了），晚一眼看过去也得还在。
        live_repl.apply_output_frame(format!("\x1b[2m{note}\x1b[0m\n\n").as_bytes())?;
    }

    let mut repl = RemoteRepl {
        paths: refreshed,
        config,
        mode,
        active_session_id,
        history,
        cumulative_tokens,
        footer,
        live_repl,
        jobs_shared,
        jobs_feed,
        follow_depth: 0,
        follow_pending: false,
        lane_context: Default::default(),
    };
    let outcome = repl.run().await;
    // 交出去的 raw 模式没人接（发完一句紧接着 exit，或者中途出错）：收回来关掉，
    // 连同键盘增强一起还原，别把用户的 shell 留在 raw 模式里。
    repl.live_repl.release_raw_handoff();
    // 正常退出也要把 pane 的权威还回去（信号那条路另有一处）。跑不跑得成都要还，
    // 所以放在 `?` 之外。
    herdr::release_blocking();
    outcome
}

/// 一个远端 REPL 会话跑着时的全部状态:配置、车道、当前会话、输入历史、footer 读数、
/// 活动区与后台任务源。斜杠命令与发回合都是它的方法。
pub(super) struct RemoteRepl {
    pub(super) paths: MiyuPaths,
    pub(super) config: AppConfig,
    pub(super) mode: PersonaLane,
    pub(super) active_session_id: String,
    pub(super) history: Vec<ReplHistoryEntry>,
    pub(super) cumulative_tokens: TurnTokens,
    /// 「换会话后挂上它正在跑的那一轮」嵌了几层（见 `follow_active_run_here`）。
    pub(super) follow_depth: u8,
    /// 刚换过会话，它正在跑的那一轮还没挂上（会话项目第 3 段）。主循环回到顶上时挂，
    /// 不在换会话的地方就地挂：回合中点任务条来回切，就地挂会一层套一层。
    pub(super) follow_pending: bool,
    pub(super) footer: ReplFooterStatus,
    pub(super) live_repl: LiveReplTail,
    pub(super) jobs_shared: std::sync::Arc<SharedJobsFeed>,
    pub(super) jobs_feed: JobsFeed,
    /// 大厅里两条车道各自的空会话上下文，按 Tab 换显示时用（见 `lobby_lane`）。
    pub(super) lane_context: super::lobby_lane::LaneContext,
}

/// 斜杠命令或发回合之后主循环该怎么走。
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum LoopStep {
    Continue,
    Break,
}

impl RemoteRepl {
    /// 主循环:读输入 → 斜杠命令或发回合,直到退出。
    pub(super) async fn run(&mut self) -> Result<()> {
        loop {
            // Keep the poll thread's session filter in step with /new & /session.
            self.jobs_shared.set_repl_session(&self.active_session_id);
            // 跟着看的中途又换了会话（切进子会话、回去），换过去那条的轮接着挂。
            while std::mem::take(&mut self.follow_pending) {
                self.follow_active_run_here().await?;
            }
            // Σ：空闲时轮询只改界面上那份（上次显式刷新之后才开读的，见
            // `footer_generation`），这儿不收回来就被下面的整份覆盖盖回旧值，下一次
            // 轮询才又改回来——屏上先回到老数再加上去（用户 09-23）。放在追 footer
            // 之前：那边是刚从 daemon 取的，比轮询的新。
            if self.live_repl.cumulative_from_poll {
                self.cumulative_tokens = self.footer.adopt_cumulative(&self.live_repl.footer);
            }
            // 大厅里换过去的车道：事先那一问回来之后屏上已经有数了，收回来（见
            // `adopt_lane_baseline`）。
            self.adopt_lane_baseline();
            // 回合中 `/models` 改过会话模型的话，先把手里的 footer 追上（09-20）；
            // 下面那次 set_footer 是整份覆盖，不追就把旧模型标签盖回去。
            self.adopt_stale_footer().await?;
            // 目标提示是输入循环里一秒一拍自己往前走的（`tick_goal_hint` 写在
            // tail 的 footer 上），这儿先收回来：下面那次 set_footer 是整份覆盖，
            // 不收就拿上一轮的旧值盖掉它，一秒后才由下一拍补上——屏幕上是闪一下。
            self.footer.goal = self.live_repl.footer.goal.clone();
            self.live_repl.set_footer(self.footer.clone());
            let (next_mode, input, images, history_entry) = match read_live_repl_input(
                &mut self.live_repl,
                &self.paths,
                &self.jobs_feed,
                Some(&self.active_session_id),
            )? {
                LiveReplOutcome::Exit => break,
                LiveReplOutcome::StopJob { job_id } => {
                    stop_background_job(&self.paths, &self.jobs_feed, &mut self.live_repl, &job_id)
                        .await?;
                    continue;
                }
                LiveReplOutcome::StopJobs => {
                    let stopped = match repl_ipc_admin(
                        &self.paths,
                        &mut self.live_repl,
                        IpcCommand::StopSessionJobs {
                            session_id: self.active_session_id.clone(),
                        },
                    )
                    .await?
                    {
                        Some((_, data)) => data
                            .get("stopped")
                            .and_then(serde_json::Value::as_u64)
                            .unwrap_or(0),
                        None => 0,
                    };
                    // Drop the strip now instead of waiting out the ~1s jobs poll:
                    // every job of this session was just stopped, so an empty strip
                    // is the truth.
                    //
                    // 光清是不够的：紧接着那次轮询拿到的还是停之前的快照，状态行会
                    // 再冒出来一下——先把这些 id 压住，等轮询里真的没有了再放开。
                    let stopped_ids: Vec<String> = self
                        .live_repl
                        .jobs
                        .iter()
                        .map(|job| job.job_id.clone())
                        .collect();
                    self.live_repl
                        .suppress_jobs(stopped_ids.iter().map(String::as_str));
                    self.live_repl.set_jobs(Vec::new());
                    repl_note(
                        &mut self.live_repl,
                        &format!(
                            "\x1b[2m{}\x1b[0m\n",
                            if is_zh() {
                                format!("已停止 {stopped} 个后台任务")
                            } else {
                                format!("stopped {stopped} background task(s)")
                            }
                        ),
                    )?;
                    continue;
                }
                LiveReplOutcome::FollowWake {
                    run_id,
                    label,
                    from_start,
                } => {
                    // 接走的是这条会话上别处起的回合：大厅里按 Tab 换的显示作废。
                    self.revert_lobby_lane()?;
                    if self
                        .follow_run_with_commands(&run_id, &label, from_start, None)
                        .await?
                        == LoopStep::Break
                    {
                        break;
                    }
                    continue;
                }
                LiveReplOutcome::Submit(next_mode, input, images, entry) => {
                    (next_mode, input, images, entry)
                }
                LiveReplOutcome::ToggleReadonly => {
                    // 只读是会话自己的开关：按过 Tab 就先把会话换到显示的那条车道。
                    if !self.materialize_lobby_lane().await? {
                        continue;
                    }
                    toggle_repl_readonly(
                        &self.paths,
                        &mut self.live_repl,
                        &self.active_session_id,
                        false,
                    )
                    .await?;
                    continue;
                }
                LiveReplOutcome::SwitchMode(next) => {
                    // 只换显示，会话等真要用的时候再换（见 `lobby_lane`）。
                    self.toggle_lobby_lane(next)?;
                    continue;
                }
                LiveReplOutcome::Strip(action) => {
                    self.perform_strip_action(action).await?;
                    continue;
                }
            };
            self.mode = next_mode;
            let input = input.trim();
            if input.eq_ignore_ascii_case("exit") || input.eq_ignore_ascii_case("quit") {
                break;
            }
            let (slash_command, command_args) = match parse_repl_input(input) {
                ReplInput::Chat => (None, ""),
                ReplInput::Slash(command, args) => (Some(command), args),
            };
            // 大厅里按 Tab 换过车道的话，发消息、敲命令之前先把会话换过去。本身就是
            // 换会话的命令和 `/exit` 不用：换过去紧接着又换走（或退出），白开一条会话。
            let replaces_session = matches!(
                slash_command,
                Some(
                    ReplSlashCommand::Session
                        | ReplSlashCommand::Subagent
                        | ReplSlashCommand::Back
                        | ReplSlashCommand::Dev
                        | ReplSlashCommand::Normal
                        | ReplSlashCommand::Exit
                )
            );
            if !replaces_session && !self.materialize_lobby_lane().await? {
                continue;
            }
            // 第一条消息发出去,会话就不空了:banner 撤、模式钉死。
            if submission_leaves_lobby(input) {
                self.live_repl
                    .set_session_empty(&self.config, &self.paths, false);
            }
            if let Some(command) = slash_command {
                // 命令也进上方向键历史：`/goal 长长的目标` 打错一个字重敲一遍，
                // 和重敲一条消息一样冤。落盘历史仍只收消息（命令是操作不是对话）。
                push_history_capped(&mut self.history, ReplHistoryEntry::plain(input));
                self.live_repl
                    .editor
                    .record_history(ReplHistoryEntry::plain(input));
                let spec = repl_command_spec(command);
                if spec.arg_hint.is_empty() && !command_args.trim().is_empty() {
                    repl_note(
                        &mut self.live_repl,
                        &format!(
                            "\x1b[2m{}: {}\x1b[0m\n",
                            t("this command takes no arguments", "该命令不接受参数"),
                            spec.name
                        ),
                    )?;
                    continue;
                }
                let step = self.dispatch_slash(command, command_args).await?;
                if step == LoopStep::Break {
                    break;
                }
                continue;
            }
            if input.is_empty() {
                continue;
            }
            self.submit_chat(input, &images, &history_entry).await?;
        }
        Ok(())
    }

    /// 挂到一条正在跑的轮上看它说完，中途敲的斜杠命令照常能执行。
    ///
    /// 回合中要占屏的命令走「分离 → 执行 → 挂回来」（用户 09-20）。挂回来之后
    /// 还能再敲一条，所以这里是个**循环**而不是一次性的；每次都从上次看到的
    /// 事件号之后接着看，已经看过的那半截不会重来。
    pub(super) async fn follow_run_with_commands(
        &mut self,
        run_id: &str,
        label: &str,
        mut from_start: bool,
        mut after: Option<u64>,
    ) -> Result<LoopStep> {
        loop {
            let session_id = self.active_session_id.clone();
            let outcome = follow_wake_run(
                &self.paths,
                &mut self.live_repl,
                run_id,
                label,
                from_start,
                after,
                &session_id,
                &self.jobs_feed,
                &self.jobs_shared,
            )
            .await;
            // 这一轮不管怎么结束（断线、报错也算），footer 的声波和这一轮的计时都得停——
            // 和 `try_run_remote_chat` 同一个兜底，幂等。回合中执行完命令挂回来时，计时
            // 按这一轮真正开始的时刻重新接上（`follow_wake_run` 里 `set_turn_clock_start`）。
            let _ = self.live_repl.stop_footer_spinner();
            let error = match outcome {
                Ok(()) => return Ok(LoopStep::Continue),
                Err(error) => error,
            };
            let Some(suspended) = take_remote_turn_suspended(&error) else {
                tracing::debug!(error = %error, "wake follow detached with an error");
                return Ok(LoopStep::Continue);
            };
            let (action, last_event_id, suspended_session) = (
                suspended.action.clone(),
                suspended.last_event_id,
                suspended.session_id.clone(),
            );
            let step = self.perform_suspended_action(action).await?;
            if step == LoopStep::Break {
                return Ok(step);
            }
            // 命令把会话换走了（`/new` `/session` `/dev` `/normal`）就不挂回去：
            // 那一轮在 daemon 里继续跑，属于**另一条**会话。
            if self.active_session_id != suspended_session {
                return Ok(step);
            }
            // 0 = 一个事件都没看到（命令敲在刚挂上的那一瞬）：从头补，否则
            // 开头那截永远看不到了。
            from_start = last_event_id == 0;
            after = (last_event_id > 0).then_some(last_event_id);
        }
    }

    /// 回合循环暂离之后要做的事（`RemoteTurnSuspended`，09-20）：发回合和挂上去
    /// 跟的两条路都从这儿过。
    pub(super) async fn perform_suspended_action(
        &mut self,
        action: SuspendedAction,
    ) -> Result<LoopStep> {
        match action {
            SuspendedAction::Command { command, args } => self.dispatch_slash(command, &args).await,
            // `/session` 面板已经在回合里跑完，人挑好了：只管切过去。
            SuspendedAction::SwitchSession(state) => {
                self.switch_to_session(&state).await?;
                Ok(LoopStep::Continue)
            }
            SuspendedAction::Strip(action) => self.perform_strip_action(action).await,
        }
    }

    /// 回合中寄宿的 `/models` 面板改了会话模型（09-20）：活动区那份 footer 已经
    /// 按新模型重算过，这里手里的还是旧的——主循环每圈都会拿它盖回去。旗子立着
    /// 就先重算一次再盖。
    pub(super) async fn adopt_stale_footer(&mut self) -> Result<()> {
        if !std::mem::take(&mut self.live_repl.session_footer_stale) {
            return Ok(());
        }
        let (footer, cumulative) = session_footer_status(
            &self.paths,
            &self.config,
            &mut self.live_repl,
            &self.active_session_id,
        )
        .await?;
        self.cumulative_tokens = cumulative;
        self.footer = footer;
        Ok(())
    }

    /// 执行一条斜杠命令。**唯一**的执行入口：主循环走它，回合中暂离后回来补
    /// 执行的那条路（`RemoteTurnSuspended`，09-20）也走它——分两份迟早分叉。
    pub(super) async fn dispatch_slash(
        &mut self,
        command: ReplSlashCommand,
        command_args: &str,
    ) -> Result<LoopStep> {
        Ok(match command {
            ReplSlashCommand::Exit => LoopStep::Break,
            ReplSlashCommand::Help => self.cmd_help().await?,
            ReplSlashCommand::Stt => self.cmd_stt().await?,
            ReplSlashCommand::History => self.cmd_history().await?,
            ReplSlashCommand::New => self.cmd_new(command_args).await?,
            ReplSlashCommand::Session => self.cmd_session(command_args).await?,
            ReplSlashCommand::Subagent => self.cmd_subagent().await?,
            ReplSlashCommand::Back => self.cmd_back().await?,
            ReplSlashCommand::Dev => self.cmd_lane(PersonaLane::Dev, command_args).await?,
            ReplSlashCommand::Normal => self.cmd_lane(PersonaLane::Active, command_args).await?,
            ReplSlashCommand::Rename => self.cmd_rename(command_args).await?,
            ReplSlashCommand::Delete => self.cmd_delete(command_args).await?,
            ReplSlashCommand::Sandbox => self.cmd_sandbox(command_args).await?,
            ReplSlashCommand::Goal => self.cmd_goal(command_args).await?,
            ReplSlashCommand::Usage => self.cmd_usage().await?,
            ReplSlashCommand::Persona => self.cmd_persona(command_args).await?,
            ReplSlashCommand::Models => self.cmd_models(command_args).await?,
            ReplSlashCommand::Config => self.cmd_config().await?,
            ReplSlashCommand::Effort => self.cmd_effort(command_args).await?,
            ReplSlashCommand::Undo => self.cmd_undo().await?,
            ReplSlashCommand::Pop => self.cmd_pop(command_args).await?,
            ReplSlashCommand::Compact => self.cmd_compact().await?,
            ReplSlashCommand::ResetMemory => self.cmd_reset_memory().await?,
            ReplSlashCommand::ResetAllMemory => self.cmd_reset_all_memory().await?,
            ReplSlashCommand::Reset => self.cmd_reset().await?,
            ReplSlashCommand::Wipe => self.cmd_wipe().await?,
        })
    }
}
