//! 回合跑着时敲的 `/compact`：像插话一样排进这一轮（09-25，用户：「像 followup 消息一样排队」）。
//!
//! 原来压缩一律要先占住会话，会话里有回合就回一句 busy——挂着子代理等十几分钟的那种回合，
//! 这十几分钟里压不了。现在它记在会话的 [`TurnCompactRequest`] 上：回合在接插话的那两处
//! （一批工具跑完、模型要收尾时）取走它，压掉本轮之前的历史接着跑。这一轮被打断、出错，没走到
//! 那两处就退场的，由这里的驱动器接手：每有回合退场看一眼，会话闲下来了、也没被别的管理操作
//! 占着，就替它压一次（和手动 `/compact` 同一条 actor 路）。
//!
//! [`TurnCompactRequest`]: miyu_engine::agent::TurnCompactRequest

use crate::web::*;

/// 会话里有回合在跑：把压缩排进去，返回 `true`。没有回合（照常当场压）、或者别的管理操作正
/// 占着（照常报 busy）返回 `false`。
pub(in crate::web) fn queue_compact_if_running(state: &DaemonState, session_id: &str) -> bool {
    let mut manager = state.manager.lock().unwrap();
    if manager.admin_busy || !manager.session_has_runs(session_id) {
        return false;
    }
    manager.compact_request(session_id).request();
    // 排进去的那一刻回合可能正好退场，错过了它的退场通知：叫驱动器自己再看一眼。
    manager.runs_changed.notify_waiters();
    true
}

/// 回合退场后补压没被取走的排队压缩。
pub(in crate::web) fn spawn_queued_compact_driver(state: DaemonState) {
    tokio::spawn(async move {
        let changed = state.manager.lock().unwrap().runs_changed.clone();
        loop {
            // 先登记再看：看的时候退场的那一轮不会漏掉通知（同 `stop_session_runs`）。
            let notified = changed.notified();
            for session_id in idle_sessions_with_queued_compact(&state) {
                compact_idle_session(&state, session_id).await;
            }
            notified.await;
        }
    });
}

fn idle_sessions_with_queued_compact(state: &DaemonState) -> Vec<Arc<str>> {
    let manager = state.manager.lock().unwrap();
    manager
        .compact_requests
        .iter()
        .filter(|(session_id, request)| {
            request.is_pending()
                && !manager.session_has_runs(session_id)
                && !manager.admin_blocks_session(session_id)
        })
        .map(|(session_id, _)| Arc::from(session_id.as_str()))
        .collect()
}

async fn compact_idle_session(state: &DaemonState, session_id: Arc<str>) {
    let taken = state
        .manager
        .lock()
        .unwrap()
        .compact_requests
        .get(&*session_id)
        .is_some_and(|request| request.take());
    if !taken {
        return;
    }
    if reserve_admin_for_session(&state.manager, &session_id).is_err() {
        // 又起了一轮：放回去，让那一轮在检查点取。
        state
            .manager
            .lock()
            .unwrap()
            .compact_request(&session_id)
            .request();
        return;
    }
    let (reply, receiver) = tokio::sync::oneshot::channel();
    if state
        .actor_tx
        .send(ActorCommand::Compact {
            session_id: session_id.clone(),
            events: None,
            reply,
        })
        .is_err()
    {
        release_admin(&state.manager);
        return;
    }
    // actor 那头压完自己放预约；只有它中途没了才轮到这里放。
    match receiver.await {
        Ok(Ok(_)) => {
            state.events.publish(
                "session.compacted",
                json!({ "session_id": &*session_id, "queued": true }),
            );
        }
        Ok(Err(AdminFailure::Invalid(message) | AdminFailure::Internal(message))) => {
            tracing::warn!(session = %session_id, error = %message, "queued compact failed");
        }
        Err(_) => release_admin(&state.manager),
    }
}
