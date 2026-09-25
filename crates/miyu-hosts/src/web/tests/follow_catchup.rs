//! 事件环追不回一轮的开头时，从库里的流水补（会话项目第 3 段）。

use super::shared::*;
use crate::web::*;
use miyu_base::workspace::TurnOrigin;
use miyu_core::state::{ReplayEntry, TurnReplay};

/// 会话里一轮跑着的回合，流水里已经说了一半；`first_event_id` 给个早被挤掉的号。
fn running_turn(state: &DaemonState) {
    let session = state.state_store.session_id().to_string();
    let store = state.state_store.pinned_for_turn(&session);
    store
        .start_turn("t-live", "长回合", std::process::id())
        .unwrap();
    store
        .append_turn_journal_event(
            "t-live",
            0,
            0,
            "assistant_content",
            None,
            None,
            Some("已经说了一半"),
            None,
            None,
        )
        .unwrap();
    let (cancel, _cancel_rx) = tokio::sync::watch::channel(false);
    state.manager.lock().unwrap().active_runs.insert(
        "run-live".to_string(),
        RunInfo {
            session_id: session.as_str().into(),
            mode: PersonaLane::Active,
            audience: PromptAudience::Owner,
            cancel,
            turn_id: Some("t-live".to_string()),
            queue_target: Some(store.queue_target("t-live")),
            supersede: Arc::new(miyu_engine::agent::TurnSupersedeSignal::default()),
            platform_followup: None,
            operation: RunOperation::Create,
            job_wake: false,
            turn_origin: TurnOrigin::Human,
            job_wake_label: None,
            first_event_id: Some(1),
        },
    );
}

#[test]
fn a_run_whose_start_left_the_event_ring_is_caught_up_from_its_journal() {
    let temp = tempfile::tempdir().unwrap();
    let state = DaemonState::for_test(test_paths(temp.path()), 8300).unwrap();
    running_turn(&state);

    let (resume_at, frame) = crate::web::follow_catchup::catch_up_from_journal(&state, "run-live")
        .expect("跑着的轮该补得出来");
    assert_eq!(resume_at, state.events.latest_id());
    let IpcFrame::Event { kind, data, .. } = frame else {
        panic!("补的应该是一条事件");
    };
    assert_eq!(kind, "turn.catchup");
    let replay: TurnReplay = serde_json::from_value(data["replay"].clone()).unwrap();
    assert_eq!(replay.display_content, "长回合");
    assert!(matches!(&replay.entries[0], ReplayEntry::Text { text } if text == "已经说了一半"));

    // 回合已经跑完（不在 active_runs 里）就不补，调用方照旧报错。
    state.manager.lock().unwrap().active_runs.remove("run-live");
    assert!(crate::web::follow_catchup::catch_up_from_journal(&state, "run-live").is_none());
}

async fn next(client: &mut tokio::net::UnixStream) -> IpcFrame {
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        ipc::receive::<IpcFrame>(client),
    )
    .await
    .expect("等帧超时")
    .unwrap()
    .unwrap()
}

/// 真把事件环挤满：从头跟这一轮，先收到按流水补的那一份，之后的实时事件照常到，
/// 跑完就收。以前在这儿直接报「event history was exhausted」收工。
#[tokio::test]
async fn following_a_run_past_the_event_ring_catches_up_and_keeps_streaming() {
    let temp = tempfile::tempdir().unwrap();
    let state = DaemonState::for_test(test_paths(temp.path()), 8300).unwrap();
    running_turn(&state);
    for index in 0..5_000 {
        state.events.publish("noise", json!({ "n": index }));
    }
    let (mut client, mut server) = tokio::net::UnixStream::pair().unwrap();
    let follower = state.clone();
    let task = tokio::spawn(async move {
        crate::web::server::follow_run(&follower, &mut server, "run-live".to_string(), true, None)
            .await
    });
    assert!(matches!(next(&mut client).await, IpcFrame::Accepted { .. }));
    let catchup = next(&mut client).await;
    assert!(
        matches!(&catchup, IpcFrame::Event { kind, .. } if kind == "turn.catchup"),
        "{catchup:?}"
    );
    state.events.publish(
        "assistant.delta",
        json!({ "run_id": "run-live", "text": "接着说" }),
    );
    let live = next(&mut client).await;
    assert!(
        matches!(&live, IpcFrame::Event { kind, .. } if kind == "assistant.delta"),
        "{live:?}"
    );
    state
        .events
        .publish("run.completed", json!({ "run_id": "run-live" }));
    let done = next(&mut client).await;
    assert!(
        matches!(&done, IpcFrame::Event { kind, .. } if kind == "run.completed"),
        "{done:?}"
    );
    task.await.unwrap().unwrap();
}
