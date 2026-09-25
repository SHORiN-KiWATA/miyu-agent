//! 后台任务完成后的唤醒。
//!
//! 任务跑完要把结论送到用户面前，但「面前」有三种：网页（走事件流）、发起它的
//! 终端（`stream_job_wake_to_origin_tty`）、平台会话（`wake_platform_session_for_job`）。
//!
//! 写回终端前要确认它还在、还停在提示符处——正在跑别的命令时插一段输出会把人家
//! 的界面搅乱。

use crate::web::*;

/// Background-job completions wake the model so it can follow up on the
/// result autonomously. Local sessions get a real turn (or a queued
/// followup when the session is mid-turn); platform-bound sessions get a
/// plain-text broadcast into the conversation — a self-initiated platform
/// turn would need synthetic sender semantics the plugins aren't built for.
/// goal 续轮驱动器(任务#10,dsh goal-round-driver 的 daemon 化)。
/// 订阅 run 生命周期事件,在会话空闲检查点推进 armed 的 active 目标:
/// - run.completed → 尝试认领下一轮(四道栅栏见 maybe_continue_goal)
/// - run.failed → disarm(异常不自动重试,dsh 同款;等人 resume)
/// 取消→pause 的语义在 ipc Cancel 处理器里(那里能拿到被取消 run 的来源)。
pub(in crate::web) fn install_background_job_hook(state: &DaemonState) {
    let started_state = state.clone();
    tools::jobs::set_started_hook(Arc::new(move |mut overview| {
        // 子代理树的根(09-18 会话化):主会话的任务条按它把后代的任务也列出来。
        annotate_job_roots(&started_state, std::slice::from_mut(&mut overview));
        // session_id 放**顶层**:事件归属过滤(EventOwnerFilter)只认顶层的
        // session_id/run_id,没有就只发给管理员——成员的后台任务因此在成员端
        // 完全不显示(09-11 用户报「后台任务 UI 没了」)。带上归属会话即可让
        // 成员收到自己那份。
        started_state.events.publish(
            "job.started",
            json!({ "job": overview, "session_id": overview.session_id }),
        );
    }));
    // 后台子代理的实时进度上 SSE。标记在这儿收成任务条那一行要的三样(窥视、词元、
    // 子会话,会话项目第 4 段之二,`tools::subagent::status`):网页不解析标记,点那一行
    // 打开子会话。节流窗里的、不是子代理标记的,不发。
    let progress_state = state.clone();
    let feeds: Arc<Mutex<HashMap<String, tools::subagent::status::SubagentStatusFeed>>> =
        Arc::default();
    let finished_feeds = feeds.clone();
    tools::jobs::set_progress_hook(Arc::new(move |job_id, message| {
        let absorbed = feeds
            .lock()
            .unwrap()
            .entry(job_id.to_string())
            .or_default()
            .absorb(message);
        let tools::subagent::status::Absorbed::Report { status, .. } = absorbed else {
            return;
        };
        progress_state.events.publish(
            "job.progress",
            json!({
                "job_id": job_id,
                "session_id": tools::jobs::job_session_id(job_id),
                "peek": status.peek,
                "tokens_label": status.tokens_label,
                "child_session_id": status.session_id,
            }),
        );
    }));
    let hook_state = state.clone();
    tools::jobs::set_completion_hook(Arc::new(move |completion| {
        // 跑完了，它那份进度状态不再用得着。
        finished_feeds.lock().unwrap().remove(&completion.job_id);
        let state = hook_state.clone();
        tokio::spawn(async move {
            handle_job_completion(state, completion).await;
        });
    }));
}

pub(in crate::web) async fn handle_job_completion(
    state: DaemonState,
    completion: tools::jobs::JobCompletion,
) {
    state.events.publish(
        "job.finished",
        json!({
            "job_id": completion.job_id,
            "title": completion.title,
            "status": completion.state_label,
            "runtime_seconds": completion.runtime_seconds,
            // 顶层 session_id:成员才收得到自己后台任务的完成事件(见 started)。
            "session_id": completion.session_id.as_deref(),
        }),
    );
    tracing::info!(
        job_id = %completion.job_id,
        wake_requested = completion.wake_requested,
        has_session = completion.session_id.is_some(),
        has_origin_tty = completion.origin_tty.is_some(),
        "background job finished"
    );
    if !completion.wake_requested {
        // The model stopped this command itself; clean the strips quietly.
        tools::jobs::acknowledge(&completion.job_id);
        state.events.publish(
            "job.acknowledged",
            json!({ "job_id": completion.job_id, "session_id": completion.session_id.as_deref() }),
        );
        return;
    }
    let command_short = completion.command.chars().take(120).collect::<String>();
    let mut pending_wake_run: Option<JobWakeRun> = None;
    if let Some(session_id) = completion.session_id.clone() {
        match state.state_store.is_platform_session(&session_id) {
            Ok(true) => {
                wake_platform_session_for_job(&state, &session_id, &completion).await;
            }
            Ok(false) => {
                pending_wake_run =
                    wake_local_session_for_job(&state, session_id, &completion, &command_short)
                        .await;
            }
            Err(error) => {
                tracing::warn!(
                    job_id = %completion.job_id,
                    error = %error,
                    "failed to resolve the session of a finished background command"
                );
            }
        }
    }
    // Keep the finished job visible in UI strips until its wake turn is done
    // (the report is what replaces the strip line); everything else clears
    // right away.
    if let Some(wake) = pending_wake_run {
        // 流式回写与等待循环并行:回合一开跑就把思考/工具/正文追加进触发
        // 终端,acknowledge 只关心回合何时结束。
        if completion.origin_tty.is_some() {
            let stream_state = state.clone();
            let stream_completion = completion.clone();
            let stream_wake = wake.clone();
            tokio::spawn(async move {
                stream_job_wake_to_origin_tty(stream_state, stream_completion, stream_wake).await;
            });
        }
        // 事件驱动：run 结束由 finish_run 的 runs_changed 通知，不再
        // 500ms 拿全局锁轮询。notified() 在查条件**之前**注册，堵死
        // 「查完没在等、通知恰好落空」的竞态；60s 慢速兜底纯属防御。
        let deadline = tokio::time::Instant::now() + Duration::from_secs(600);
        let notify = state.manager.lock().unwrap().runs_changed.clone();
        loop {
            let notified = notify.notified();
            let still_running = state
                .manager
                .lock()
                .unwrap()
                .active_runs
                .contains_key(&wake.run_id);
            if !still_running || tokio::time::Instant::now() >= deadline {
                break;
            }
            tokio::select! {
                _ = notified => {}
                _ = tokio::time::sleep_until(deadline) => {}
                _ = tokio::time::sleep(Duration::from_secs(60)) => {}
            }
        }
    }
    tools::jobs::acknowledge(&completion.job_id);
    state.events.publish(
        "job.acknowledged",
        json!({ "job_id": completion.job_id, "session_id": completion.session_id.as_deref() }),
    );
}

/// 本地会话唤醒回合的标识:run id + 事件订阅起点(在回合入队前取,保证
/// 从 turn.started 起一帧不漏)。
#[derive(Clone)]
pub(in crate::web) struct JobWakeRun {
    pub(in crate::web) run_id: String,
    pub(in crate::web) events_after: u64,
}

/// 把唤醒回合流式渲染进当初触发 shellhook/单次 CLI 的终端:思考(暗色,按
/// display.reasoning 配置)、工具行、正文逐行 Markdown。触发端进程早已退出,
/// 由 daemon 直接写 tty 设备。三道闸全过才动笔:
/// 1. `notifications.job_writeback_to_terminal` 开关(默认开);
/// 2. 触发 shell 还活着且 stdin 仍指向记录的 tty——终端关闭、pid 复用都拦下;
/// 3. shell 空闲在前台提示符(tpgid==pgrp)——正开着 vim/htop 时绝不能撕屏。
/// 追加式输出,无光标控制;每次落笔前重查第 3 道闸,中途被占立即收笔并补
/// 桌面通知。物理写入走专职线程,^S 流控卡死也只占一根线程。
pub(in crate::web) async fn stream_job_wake_to_origin_tty(
    state: DaemonState,
    completion: tools::jobs::JobCompletion,
    wake: JobWakeRun,
) {
    let Some(origin) = completion.origin_tty.clone() else {
        return;
    };
    let config = miyu_base::config::AppConfig::load_or_default(&state.paths).unwrap_or_default();
    if !config.notifications.job_writeback_to_terminal {
        return;
    }
    let notify_fallback = |reason: &str| {
        tracing::info!(job_id = %completion.job_id, reason, "job wake writeback fell back to a notification");
        if config.notifications.enabled {
            miyu_base::notify::notify(
                &format!(
                    "{} · {}",
                    t("Miyu background task follow-up", "Miyu 后台任务跟进"),
                    completion.title
                ),
                t(
                    "The task is done; the follow-up reply is in the session (the terminal was not at a prompt, so nothing was written there).",
                    "任务已完成,跟进回复在会话里(终端不在提示符,没有直接写入)。",
                ),
            );
        }
    };
    if !origin_shell_at_prompt(&origin) {
        notify_fallback("shell not at prompt");
        return;
    }
    use std::os::unix::fs::OpenOptionsExt;
    let tty = match std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_NOCTTY)
        .open(&origin.path)
    {
        Ok(tty) => tty,
        Err(error) => {
            tracing::debug!(job_id = %completion.job_id, %error, "origin tty open failed");
            notify_fallback("tty open failed");
            return;
        }
    };
    tracing::info!(
        job_id = %completion.job_id,
        run_id = %wake.run_id,
        tty = %origin.path.display(),
        shell_pid = origin.shell_pid,
        "streaming job wake reply to the originating terminal"
    );

    let (ops_tx, ops_rx) = std::sync::mpsc::channel::<TtyWriteOp>();
    let shell_pid = origin.shell_pid;
    let setup = TtyRenderSetup::from_config(&config, tty_cols(&tty), completion.title.clone());
    let writer = std::thread::Builder::new()
        .name("miyu-tty-writeback".to_string())
        .spawn(move || origin_tty_writer(tty, shell_pid, ops_rx, setup));
    if writer.is_err() {
        notify_fallback("writer thread spawn failed");
        return;
    }

    // 抬头、转轮、正文都由写线程画（渲染器在它手里）。这儿只把回合的事件原样
    // 转过去，落笔前重查前台闸。
    let mut subscription = state.events.subscribe_after(wake.events_after);
    let deadline = std::time::Instant::now() + Duration::from_secs(900);
    let mut last_id = wake.events_after;
    let mut aborted = false;
    let mut gate_checked_at = std::time::Instant::now();
    loop {
        if std::time::Instant::now() > deadline {
            aborted = true;
            break;
        }
        let record = if let Some(record) = subscription.pending.pop_front() {
            record
        } else {
            match tokio::time::timeout(Duration::from_secs(30), subscription.receiver.recv()).await
            {
                Ok(Ok(record)) => record,
                Ok(Err(broadcast::error::RecvError::Lagged(_))) => {
                    subscription.pending = state.events.replay_after(last_id);
                    continue;
                }
                Ok(Err(broadcast::error::RecvError::Closed)) => break,
                Err(_) => {
                    // 静默期顺手确认回合还活着,免得错过终态事件后干等。
                    if !state
                        .manager
                        .lock()
                        .unwrap()
                        .active_runs
                        .contains_key(&wake.run_id)
                    {
                        break;
                    }
                    continue;
                }
            }
        };
        last_id = record.id;
        if record.run_id.as_deref() != Some(wake.run_id.as_str()) {
            if !state
                .manager
                .lock()
                .unwrap()
                .active_runs
                .contains_key(&wake.run_id)
            {
                break;
            }
            continue;
        }
        // 落笔前重查前台闸:用户开了全屏程序就立即收笔,已写的留在屏上。
        // 一条 delta 一次 /proc 太勤,四分之一秒查一回够了。
        if gate_checked_at.elapsed() >= Duration::from_millis(250) {
            gate_checked_at = std::time::Instant::now();
            if !origin_shell_at_prompt(&origin) {
                aborted = true;
                break;
            }
        }
        let Ok(data) = serde_json::from_str::<Value>(&record.data) else {
            continue;
        };
        let terminal = matches!(
            record.kind.as_str(),
            "run.completed" | "run.failed" | "run.cancelled"
        );
        let _ = ops_tx.send(TtyWriteOp::Event {
            kind: record.kind.clone(),
            data,
        });
        if terminal {
            let _ = ops_tx.send(TtyWriteOp::Finish {
                interrupted: record.kind != "run.completed",
            });
            tracing::info!(
                job_id = %completion.job_id,
                outcome = %record.kind,
                "job wake reply streamed to the originating terminal"
            );
            return;
        }
    }
    let _ = ops_tx.send(TtyWriteOp::Abort);
    if aborted {
        notify_fallback("interrupted mid-stream");
    }
}

/// 专职写线程:tty 是同步阻塞设备(^S 流控可以永久卡住 write),隔离在自己
/// 的线程里,卡死也只占一根线程,不拖累 daemon 的 async runtime。
///
/// 渲染也在这条线程上：事件原样转进来，喂给和 shellhook 自己那一轮**同一台**
/// `StreamRenderer`（静态时间线那一档），画出来的字节写进 tty——跟进那一轮和
/// 触发它的那一轮长得一样（用户实测：跟进后的渲染和 inline / 真 TUI 都不一样，
/// 没有时间线）。渲染器量宽度问的是 `terminal::size()`，这儿得先把那个 tty 的
/// 宽度报给它（线程局部）。
pub(in crate::web) fn origin_tty_writer(
    mut tty: std::fs::File,
    shell_pid: u32,
    ops: std::sync::mpsc::Receiver<TtyWriteOp>,
    setup: TtyRenderSetup,
) {
    use std::io::Write;
    crate::render::set_cols_override(setup.cols);
    let mut renderer = crate::render::StreamRenderer::new(
        setup.reasoning_mode,
        setup.tool_call_mode,
        false,
        setup.readable_tool_names,
        setup.command_output_lines,
    );
    renderer.thinking_scroll_lines = setup.thinking_scroll_lines;
    renderer.cross_session_preview_lines = setup.cross_session_preview_lines;
    // daemon 的 stdout 是管道，可这些字节要进 shellhook 那个 tty——选的是那一面。
    renderer.use_terminal_surface();
    renderer.use_external_cursor_control();
    renderer.use_buffered_output();
    fn flush(renderer: &mut crate::render::StreamRenderer, tty: &mut std::fs::File) -> bool {
        let frame = renderer.take_output_frame();
        if frame.is_empty() {
            return true;
        }
        tty.write_all(&frame).is_ok() && tty.flush().is_ok()
    }
    // 抬头和 REPL 里后台任务完成那一行一个样子：暗色齿轮 + 任务名。
    let header = format!(
        "\r\n\x1b[2m⚙ {} · {}\x1b[0m\r\n\r\n",
        t("background task follow-up", "后台任务跟进"),
        setup.title
    );
    if tty.write_all(header.as_bytes()).is_err() {
        return;
    }
    let _ = renderer.start_waiting();
    if !flush(&mut renderer, &mut tty) {
        return;
    }
    let mut finished = false;
    loop {
        match ops.recv_timeout(Duration::from_millis(80)) {
            Ok(TtyWriteOp::Write(text)) => {
                if tty.write_all(text.as_bytes()).is_err() {
                    return;
                }
            }
            Ok(TtyWriteOp::Event { kind, data }) => {
                tracing::debug!(kind = %kind, "tty writeback event");
                match crate::runtime::decode_ipc_event(&kind, &data) {
                    crate::runtime::DecodedIpc::Event(event) => {
                        if crate::render::apply_agent_event(&mut renderer, event).is_err() {
                            return;
                        }
                    }
                    crate::runtime::DecodedIpc::RunCompleted => {
                        if !finished {
                            finished = true;
                            let _ = renderer.finish();
                        }
                    }
                    // 问题没法在别人的提示符上弹面板，图片也画不了：照旧跳过。
                    _ => {}
                }
                if !flush(&mut renderer, &mut tty) {
                    return;
                }
            }
            Ok(TtyWriteOp::Finish { interrupted }) => {
                if !finished {
                    let _ = renderer.finish();
                }
                if !flush(&mut renderer, &mut tty) {
                    return;
                }
                if interrupted {
                    let note = format!(
                        "\x1b[2m({})\x1b[0m\r\n",
                        t("follow-up interrupted", "跟进中断")
                    );
                    let _ = tty.write_all(note.as_bytes());
                }
                // fish/zsh 收到 SIGWINCH 重绘提示符时,会从光标行向上清掉
                // 自家提示符高度的行数再画(starship 双行提示符实测清 2 行)。
                // 垫两行空白当牺牲品,免得清到正文末行。
                let _ = tty.write_all(b"\r\n\r\n\r\n");
                let _ = tty.flush();
                // 提示符被我们的输出推到半空,SIGWINCH 让 shell(fish/zsh/新
                // bash 的 readline 都处理)原地重绘一行干净的提示符。
                unsafe {
                    libc::kill(shell_pid as i32, libc::SIGWINCH);
                }
                return;
            }
            Ok(TtyWriteOp::Abort) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                // 中途收笔：转轮那几行擦掉，已写的正文留在屏上。
                if !finished {
                    let _ = renderer.finish();
                }
                let _ = flush(&mut renderer, &mut tty);
                return;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if !finished {
                    let _ = renderer.tick_spinner();
                    if !flush(&mut renderer, &mut tty) {
                        return;
                    }
                }
            }
        }
    }
}

pub(in crate::web) async fn wake_local_session_for_job(
    state: &DaemonState,
    session_id: Arc<str>,
    completion: &tools::jobs::JobCompletion,
    command_short: &str,
) -> Option<JobWakeRun> {
    let noun = if completion.is_subagent {
        "后台子代理"
    } else {
        "后台命令"
    };
    // 结果直接附在唤醒里,不再让模型「先去查一次再汇报」。子代理给完整结论
    // (它就是交付物),命令给日志尾部;剩下的自己判断——只给事实和日志路径,
    // 不给动作指示。
    let result_block = tools::jobs::completion_result(
        &completion.log_path,
        completion.is_subagent,
        completion.exit_code == Some(0),
    )
    .map(|(label, body)| format!("- {label}:\n{body}\n"))
    .unwrap_or_default();
    // 开头标签是「这一轮是合成的」的唯一判据（回放、上键历史都认它），别改成别的。
    let tag = miyu_core::state::BACKGROUND_JOB_REPORT_TAG;
    let content = format!(
        "{tag}{noun}「{}」已执行完毕：\n\
         - job_id: {}\n- 任务: {}\n- 状态: {}（运行 {} 秒）\n\
         - 日志: {}\n{result_block}\
         这是系统自动触发的跟进，不是用户消息。\
         </background-job-report>",
        completion.title,
        completion.job_id,
        command_short,
        completion.state_label,
        completion.runtime_seconds,
        completion.log_path.display(),
    );
    // 前缀 `[后台任务完成]` 是前端解析合成轮的判据(见 app.js 的
    // isSyntheticTurnContent),逐字保留;前缀之后是给人看的,跟界面语言走
    // (2026-09-23 WebUI 双语)。
    let display_content = format!(
        "[后台任务完成] {} {} · {}",
        if completion.is_subagent {
            t("Subagent finished", "子代理完成")
        } else {
            t("Command finished", "命令完成")
        },
        completion.job_id,
        completion.title
    );

    let delivery = Delivery {
        content,
        display_content,
        wake_label: format!(
            "{} {} · {}",
            if completion.is_subagent {
                t("Subagent finished", "子代理完成")
            } else {
                t("Command finished", "命令完成")
            },
            completion.job_id,
            completion.title
        ),
        turn_origin: miyu_base::workspace::TurnOrigin::JobWake,
        cwd: Some(completion.workspace.clone()),
        origin_tty: completion.origin_tty.clone(),
    };
    // 正在跑就排进那一轮，闲着就替它起一轮；那一轮刚起步或正在结束时等它，不再直接
    // 放弃（09-23：原来一轮刚起步时这条汇报被静默丢掉，而任务只报这一次）。
    match deliver(state, session_id, delivery).await {
        Delivered::Woke(run) => Some(run),
        Delivered::Queued => {
            // 走查 `testkit/tui/bg_followup.py` 认这一句判「走的是插进正在跑的那一轮」。
            tracing::info!(job_id = %completion.job_id, "job wake joining the session's active run");
            None
        }
        Delivered::Failed(reason) => {
            tracing::warn!(job_id = %completion.job_id, %reason, "job wake not delivered");
            None
        }
    }
}

pub(in crate::web) async fn wake_platform_session_for_job(
    state: &DaemonState,
    session_id: &Arc<str>,
    completion: &tools::jobs::JobCompletion,
) {
    let persona = state.manager.lock().unwrap().config.active_persona_scope();
    let binding = state
        .state_store
        .platform_session_bindings(&persona, "onebot")
        .ok()
        .and_then(|bindings| {
            bindings
                .into_iter()
                .find(|binding| binding.session_id == **session_id)
        });
    let Some(binding) = binding else {
        tracing::debug!(job_id = %completion.job_id, "job wake skipped: no platform binding");
        return;
    };
    let noun = if completion.is_subagent {
        "后台子代理"
    } else {
        "后台命令"
    };
    // 与本地唤醒同款:结果直接附在唤醒里(子代理给完整结论,命令给日志尾部),
    // 只给事实,不再指示模型「先去查一次再汇报」。
    let result_block = tools::jobs::completion_result(
        &completion.log_path,
        completion.is_subagent,
        completion.exit_code == Some(0),
    )
    .map(|(label, body)| format!("- {label}:\n{body}\n"))
    .unwrap_or_default();
    // 开头标签是「这一轮是合成的」的唯一判据（回放、上键历史都认它），别改成别的。
    let tag = miyu_core::state::BACKGROUND_JOB_REPORT_TAG;
    let content = format!(
        "{tag}{noun}「{}」已执行完毕：\n- job_id: {}\n- 任务: {}\n- 状态: {}（运行 {} 秒）\n\
         {result_block}这是系统自动触发的跟进，不是用户消息。\
         </background-job-report>",
        completion.title,
        completion.job_id,
        completion.command.chars().take(200).collect::<String>(),
        completion.state_label,
        completion.runtime_seconds
    );
    if let Err(error) = crate::platforms::onebot::wake_conversation_for_job(
        state,
        &binding.key.account_id,
        &binding.key.conversation_kind,
        &binding.key.conversation_id,
        completion.platform_sender.as_deref(),
        content,
    )
    .await
    {
        tracing::warn!(
            job_id = %completion.job_id,
            error = %error,
            "failed to wake the model for a background command in QQ"
        );
    }
}
