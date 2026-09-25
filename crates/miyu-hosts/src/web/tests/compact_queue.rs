//! 回合跑着时的 `/compact` 排进那一轮（09-25，见 `compact_queue`）。

use super::shared::*;
use crate::web::*;

fn fake_run(session_id: &str) -> RunInfo {
    let (cancel, _) = tokio::sync::watch::channel(false);
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

fn pending(state: &DaemonState, session_id: &str) -> bool {
    state
        .manager
        .lock()
        .unwrap()
        .compact_requests
        .get(session_id)
        .is_some_and(|request| request.is_pending())
}

/// IPC `/compact` 撞上正在跑的回合：原来回一句「Miyu is busy with another operation」，现在
/// 排进那一轮、回 `queued`。
#[tokio::test]
async fn compact_during_a_running_turn_is_queued_instead_of_busy() {
    let temp = tempfile::tempdir().unwrap();
    let state = DaemonState::for_test(test_paths(temp.path()), 8300).unwrap();
    let session = state.state_store.session_id().to_string();
    state
        .manager
        .lock()
        .unwrap()
        .active_runs
        .insert("running".to_string(), fake_run(&session));

    let (mut client, server) = tokio::net::UnixStream::pair().unwrap();
    let server_state = state.clone();
    let task = tokio::spawn(async move { handle_ipc_connection(server_state, server).await });
    ipc::send(
        &mut client,
        &IpcRequest::new(IpcCommand::Compact {
            target: ipc::SessionRef::Id {
                id: session.clone(),
            },
        }),
    )
    .await
    .unwrap();
    let response = ipc::receive::<IpcFrame>(&mut client)
        .await
        .unwrap()
        .unwrap();
    task.await.unwrap().unwrap();

    let IpcFrame::AdminResult { data, .. } = response else {
        panic!("expected an admin result, got {response:?}");
    };
    assert_eq!(data["queued"], json!(true));
    assert!(pending(&state, &session));
    assert!(!state.manager.lock().unwrap().admin_busy);
    state.manager.lock().unwrap().active_runs.clear();
}

#[test]
fn only_a_running_session_with_no_other_admin_work_queues() {
    let temp = tempfile::tempdir().unwrap();
    let state = DaemonState::for_test(test_paths(temp.path()), 8300).unwrap();
    // 没有回合：照常当场压。
    assert!(!queue_compact_if_running(&state, "idle"));
    assert!(!pending(&state, "idle"));
    // 别的管理操作正占着：照常报 busy，不排。
    {
        let mut manager = state.manager.lock().unwrap();
        manager
            .active_runs
            .insert("running".to_string(), fake_run("busy"));
        manager.admin_busy = true;
    }
    assert!(!queue_compact_if_running(&state, "busy"));
    state.manager.lock().unwrap().admin_busy = false;
    assert!(queue_compact_if_running(&state, "busy"));
    assert!(pending(&state, "busy"));
}
