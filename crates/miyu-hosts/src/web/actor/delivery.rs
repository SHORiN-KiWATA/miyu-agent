//! 往本地会话投一条 daemon 合成的消息（后台任务汇报、跨会话消息）。
//!
//! 对方正在跑：排进它当前这一轮（Followup），模型在下一次调完工具的间隙、或这段
//! 回复写完时读到。对方闲着：替它起一轮（`RunInfo.job_wake`），终端按 wake_runs、
//! 网页按 run.started 自动接上显示。
//!
//! 09-23 在这里收口的两处竞态（原来消息悄悄没了）：
//! - 对方那一轮刚起步、还没 `queue_target`：等它就绪或结束，不再直接放弃——后台
//!   任务只会报这一次，放弃就是丢。
//! - 排队失败（那一轮恰好在结束）：等它走完再替会话起一轮。
//!
//! 回合结束时队列里剩下的合成消息也从这里另起一轮去回，不再被并进刚结束的那一轮
//! （见 `finish_turn_task` 与 [`redeliver_leftovers`]）。

use crate::web::*;
use miyu_base::workspace::TurnOrigin;
use std::sync::OnceLock;

/// 一条要投的合成消息。
pub(in crate::web) struct Delivery {
    /// 模型收到的原文，开头必须是合成标签（见 `miyu_core::state::SYNTHETIC_USER_CONTENT_TAGS`）。
    pub(in crate::web) content: String,
    /// 给人看的那一份。
    pub(in crate::web) display_content: String,
    /// 起新一轮时终端与网页顶上那一行（`⚙ …`）。
    pub(in crate::web) wake_label: String,
    pub(in crate::web) turn_origin: TurnOrigin,
    pub(in crate::web) cwd: Option<std::path::PathBuf>,
    pub(in crate::web) origin_tty: Option<miyu_core::ipc::OriginTty>,
}

pub(in crate::web) enum Delivered {
    /// 排进了正在跑的那一轮。
    Queued,
    /// 替会话起了一轮。
    Woke(JobWakeRun),
    /// 没投成：会话一直被占着到超时，或 actor 已经关了。
    Failed(String),
}

/// 等对方那一轮就绪或结束的上限。压缩、管理操作占着会话时要等它们做完。
const SETTLE: Duration = Duration::from_secs(60);
/// `queue_target` 就绪不发 runs_changed，只能兜底轮询。
const POLL: Duration = Duration::from_millis(200);

pub(in crate::web) async fn deliver(
    state: &DaemonState,
    session_id: Arc<str>,
    delivery: Delivery,
) -> Delivered {
    let deadline = tokio::time::Instant::now() + SETTLE;
    loop {
        // notified() 在查条件之前注册，堵死「查完没在等、通知恰好落空」。
        let notify = state.manager.lock().unwrap().runs_changed.clone();
        let notified = notify.notified();
        let running = {
            let manager = state.manager.lock().unwrap();
            manager
                .active_runs
                .iter()
                .filter(|(_, info)| *info.session_id == *session_id)
                .map(|(run_id, info)| (run_id.clone(), info.queue_target.clone(), info.audience))
                .max_by_key(|(_, target, _)| target.is_some())
        };
        match running {
            Some((run_id, Some(target), audience)) => {
                let request = TurnUpdateRequest {
                    run_id,
                    turn_id: target.turn_id,
                    session_id: Some(session_id.clone()),
                    audience,
                    content: delivery.content.clone(),
                    display_content: delivery.display_content.clone(),
                    attachments: Vec::new(),
                    uploaded_attachment_ids: Vec::new(),
                    mode: TurnUpdateMode::Followup,
                };
                match enqueue_turn_update(state, request) {
                    Ok(_) => return Delivered::Queued,
                    // 那一轮正在结束（库里已不是 running）或被管理操作占着：
                    // 等它走完，下一圈就是「闲着」那条路。
                    Err(error) => tracing::debug!(
                        session_id = %session_id,
                        error = %error,
                        "delivery could not join the running turn; waiting"
                    ),
                }
            }
            // 那一轮刚起步，还没准备好收排队消息。
            Some((_, None, _)) => {}
            None => match start_wake_turn(state, &session_id, &delivery) {
                Ok(Some(run)) => return Delivered::Woke(run),
                Ok(None) => {}
                Err(reason) => return Delivered::Failed(reason),
            },
        }
        if tokio::time::Instant::now() >= deadline {
            let reason = format!("session {session_id} stayed busy for {}s", SETTLE.as_secs());
            tracing::warn!(session_id = %session_id, "{reason}; message not delivered");
            return Delivered::Failed(reason);
        }
        tokio::select! {
            _ = notified => {}
            _ = tokio::time::sleep(POLL) => {}
        }
    }
}

/// 替闲着的会话起一轮。`Ok(None)` = 会话正被管理操作占着，稍后再试。
fn start_wake_turn(
    state: &DaemonState,
    session_id: &Arc<str>,
    delivery: &Delivery,
) -> std::result::Result<Option<JobWakeRun>, String> {
    let run_id = random_id("run", 18);
    let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
    // 订阅起点在登记这一轮之前取：流式回写从这儿接，别的端挂上来要把整轮补一遍
    // 也从这儿补（`FollowRun { from_start }`，终端靠它画出开头那条跨会话消息）。
    let events_after = state.events.latest_id();
    {
        let mut manager = state.manager.lock().unwrap();
        if manager.admin_blocks_session(session_id) {
            return Ok(None);
        }
        manager.active_runs.insert(
            run_id.clone(),
            RunInfo {
                session_id: session_id.clone(),
                mode: PersonaLane::Active,
                audience: PromptAudience::Owner,
                cancel: cancel_tx,
                turn_id: None,
                queue_target: None,
                supersede: Arc::new(miyu_engine::agent::TurnSupersedeSignal::default()),
                platform_followup: None,
                operation: RunOperation::Create,
                job_wake: true,
                turn_origin: delivery.turn_origin.clone(),
                first_event_id: Some(events_after),
                job_wake_label: Some(delivery.wake_label.clone()),
            },
        );
    }
    let sent = state.actor_tx.send(ActorCommand::StartTurn {
        run_id: run_id.clone(),
        session_id: session_id.clone(),
        content: delivery.content.clone(),
        display_content: delivery.display_content.clone(),
        attachment_run_id: None,
        mode: PersonaLane::Active,
        images: Vec::new(),
        cwd: delivery.cwd.clone(),
        origin_tty: delivery.origin_tty.clone().map(Box::new),
        audience: PromptAudience::Owner,
        profile: None,
        overrides: None,
        cancel: cancel_rx,
        turn_origin: Box::new(delivery.turn_origin.clone()),
    });
    if sent.is_err() {
        finish_run(&state.manager, &run_id, None);
        return Err("the daemon's turn actor has shut down".to_string());
    }
    Ok(Some(JobWakeRun {
        run_id,
        events_after,
    }))
}

/// daemon 启动时装进来：回合结束的清理路径（`finish_turn_task`）手上没有
/// `DaemonState`，剩下的合成消息要靠它另起一轮。
static DAEMON_STATE: OnceLock<DaemonState> = OnceLock::new();

pub(in crate::web) fn install_delivery_state(state: &DaemonState) {
    let _ = DAEMON_STATE.set(state.clone());
}

/// 回合结束时队列里剩下的合成消息（后台汇报、跨会话消息）：另起一轮去回。
/// 原来它们被并进刚结束的那一轮、当时没人回，要等用户下次开口才被顺带看到（09-23）。
/// 按原先的先后投：第一条起一轮，后面的排进那一轮。
pub(in crate::web) fn redeliver_leftovers(
    session_id: Arc<str>,
    leftovers: Vec<miyu_core::state::QueuedSyntheticPrompt>,
) {
    if leftovers.is_empty() {
        return;
    }
    let Some(state) = DAEMON_STATE.get().cloned() else {
        tracing::warn!(
            session_id = %session_id,
            count = leftovers.len(),
            "leftover synthetic messages dropped: delivery is not installed (not in the daemon)"
        );
        return;
    };
    tokio::spawn(async move {
        for prompt in leftovers {
            let delivery = leftover_delivery(prompt);
            if let Delivered::Failed(reason) = deliver(&state, session_id.clone(), delivery).await {
                tracing::warn!(session_id = %session_id, %reason, "leftover synthetic message not delivered");
            }
        }
    });
}

/// 重投时按原文认回它是哪一种：跨会话消息记发件会话，重启续跑记第几次，其余当后台汇报。
fn leftover_delivery(prompt: miyu_core::state::QueuedSyntheticPrompt) -> Delivery {
    let restart_attempt = miyu_core::state::service_restart_attempt(&prompt.content);
    let (wake_label, turn_origin) =
        match miyu_core::state::parse_cross_session_message(&prompt.content) {
            Some(message) => (
                miyu_core::state::cross_session_headline(&message.from_name, &message.from_session),
                TurnOrigin::CrossSession {
                    from_session: message.from_session,
                },
            ),
            None if restart_attempt.is_some() => {
                let attempt = restart_attempt.unwrap_or(1);
                (
                    miyu_core::state::service_restart_headline(attempt),
                    TurnOrigin::ServiceRestart { attempt },
                )
            }
            // 显示文本去掉给前端认的前缀（`[后台任务完成] `）就是那一行标题。
            None => (
                prompt
                    .display_content
                    .strip_prefix("[后台任务完成] ")
                    .unwrap_or(&prompt.display_content)
                    .chars()
                    .take(120)
                    .collect(),
                TurnOrigin::JobWake,
            ),
        };
    Delivery {
        wake_label,
        turn_origin,
        cwd: None,
        origin_tty: None,
        content: prompt.content,
        display_content: prompt.display_content,
    }
}
