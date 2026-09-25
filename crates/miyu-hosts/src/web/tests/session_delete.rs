//! 删会话（连同子代理树）：先占住、再停、再拆、最后删（09-25）。

use super::shared::*;
use crate::web::*;

fn fake_run(session_id: &str, cancel: tokio::sync::watch::Sender<bool>) -> RunInfo {
    RunInfo {
        session_id: session_id.into(),
        mode: PersonaLane::Active,
        audience: PromptAudience::External,
        cancel,
        turn_id: None,
        queue_target: None,
        supersede: Arc::new(miyu_engine::agent::TurnSupersedeSignal::default()),
        platform_followup: None,
        operation: RunOperation::Create,
        job_wake: false,
        turn_origin: miyu_base::workspace::TurnOrigin::Human,
        job_wake_label: None,
        first_event_id: None,
    }
}

/// 一条用户会话，名下挂一个子代理会话。
fn session_with_child(state: &DaemonState) -> (String, String) {
    let persona = active_persona_scope(state);
    state
        .state_store
        .adopt_sessions_for_persona(&persona)
        .unwrap();
    let parent = state
        .state_store
        .create_session(&persona, "主会话", "user", None)
        .unwrap()
        .session_id;
    let child = state
        .state_store
        .create_subagent_session(&persona, "调研", &parent, "", 1, None, false)
        .unwrap()
        .session_id;
    (parent, child)
}

fn exists(state: &DaemonState, session_id: &str) -> bool {
    state
        .state_store
        .session_record(session_id)
        .unwrap()
        .is_some()
}

/// 删一条正在跑、名下挂着子代理的会话。停下来的那一轮一退场，拆子代理引出的「后台任务
/// 报告」就想替它再起一轮（唤醒那条路认 `admin_blocks_session`，见 `start_wake_turn`）。
/// 改之前管理预约放在最后：这一轮溜了进去，删除报 busy——回合被掐、子代理删光、会话本身
/// 还在，面板一声不响地关掉（用户 09-25 实测，再按一次才真删掉）。
#[tokio::test]
async fn deleting_a_running_session_is_not_undone_by_the_wake_it_triggers() {
    let temp = tempfile::tempdir().unwrap();
    let state = DaemonState::for_test(test_paths(temp.path()), 8300).unwrap();
    let (parent, child) = session_with_child(&state);
    let (cancel, mut cancel_rx) = tokio::sync::watch::channel(false);
    state
        .manager
        .lock()
        .unwrap()
        .active_runs
        .insert("running".to_string(), fake_run(&parent, cancel));
    let waker = state.clone();
    let woken = parent.clone();
    let wake = tokio::spawn(async move {
        cancel_rx.changed().await.unwrap();
        let notify = {
            let mut manager = waker.manager.lock().unwrap();
            manager.active_runs.remove("running");
            manager.runs_changed.clone()
        };
        notify.notify_waiters();
        let mut manager = waker.manager.lock().unwrap();
        if !manager.admin_blocks_session(&woken) {
            let (cancel, _) = tokio::sync::watch::channel(false);
            manager
                .active_runs
                .insert("wake".to_string(), fake_run(&woken, cancel));
        }
    });

    let deleted = handle_session_command(
        &state,
        IpcCommand::DeleteSession {
            target: ipc::SessionRef::Id { id: parent.clone() },
        },
    )
    .await;
    wake.await.unwrap();

    assert!(deleted.is_ok(), "delete failed: {deleted:?}");
    assert!(!exists(&state, &parent));
    assert!(!exists(&state, &child));
    let manager = state.manager.lock().unwrap();
    assert!(!manager.admin_busy);
    assert!(manager.active_runs.is_empty());
}

/// 别的管理操作占着时删不了：那就什么都别碰——回合不停、子代理不拆，原样报错。改之前是
/// 先停、先拆，最后预约失败才报错，报错的时候东西已经没了。
#[tokio::test]
async fn a_refused_delete_leaves_the_turn_and_the_subagents_alone() {
    let temp = tempfile::tempdir().unwrap();
    let state = DaemonState::for_test(test_paths(temp.path()), 8300).unwrap();
    let (parent, child) = session_with_child(&state);
    let (cancel, cancel_rx) = tokio::sync::watch::channel(false);
    {
        let mut manager = state.manager.lock().unwrap();
        manager
            .active_runs
            .insert("running".to_string(), fake_run(&parent, cancel));
        manager.admin_busy = true;
        manager.admin_session = None;
    }

    let deleted = handle_session_command(
        &state,
        IpcCommand::DeleteSession {
            target: ipc::SessionRef::Id { id: parent.clone() },
        },
    )
    .await;

    assert!(deleted.is_err());
    assert!(!*cancel_rx.borrow(), "the running turn was stopped");
    assert!(exists(&state, &parent));
    assert!(exists(&state, &child), "the subagent tree was torn down");
    let mut manager = state.manager.lock().unwrap();
    manager.admin_busy = false;
    manager.active_runs.clear();
}
