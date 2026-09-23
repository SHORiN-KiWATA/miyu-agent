//! 跨会话消息（09-23）：名单只列同一个人开着或在跑的会话，消息排进正在跑的那一轮
//! 或替闲着的会话起一轮。

use super::shared::*;
use crate::web::*;
use miyu_base::host_ports::PeerDelivery;
use miyu_base::workspace::TurnOrigin;

fn user_session(state: &DaemonState, name: &str, owner: &str) -> String {
    state
        .state_store
        .create_session_for_owner(
            &active_persona_scope(state),
            name,
            miyu_core::state::USER_SESSION_KIND,
            None,
            owner,
        )
        .unwrap()
        .session_id
}

/// 往 manager 里塞一条 `session_id` 上跑着的轮，队列目标就绪。
fn run_in(state: &DaemonState, session_id: &str, turn_id: &str) -> StateStore {
    let store = state.state_store.pinned_for_turn(session_id);
    store
        .start_turn(turn_id, turn_id, std::process::id())
        .unwrap();
    let (cancel, _cancel_rx) = tokio::sync::watch::channel(false);
    state.manager.lock().unwrap().active_runs.insert(
        format!("run-{turn_id}"),
        RunInfo {
            session_id: session_id.into(),
            mode: PersonaLane::Active,
            audience: PromptAudience::Owner,
            cancel,
            turn_id: Some(turn_id.to_string()),
            queue_target: Some(store.queue_target(turn_id)),
            supersede: Arc::new(miyu_engine::agent::TurnSupersedeSignal::default()),
            platform_followup: None,
            operation: RunOperation::Create,
            job_wake: false,
            turn_origin: TurnOrigin::Human,
            job_wake_label: None,
            first_event_id: None,
        },
    );
    store
}

#[test]
fn peers_are_this_users_other_sessions_that_are_open_or_running() {
    let temp = tempfile::tempdir().unwrap();
    let state = DaemonState::for_test(test_paths(temp.path()), 8300).unwrap();
    let me = user_session(&state, "写代码", "");
    let open = user_session(&state, "查资料", "");
    let running = user_session(&state, "跑测试", "");
    let closed = user_session(&state, "早就关了", "");
    let members = user_session(&state, "成员的", "member-1");
    let child = state
        .state_store
        .create_subagent_session(
            &active_persona_scope(&state),
            "子代理",
            &open,
            "",
            1,
            None,
            false,
        )
        .unwrap()
        .session_id;
    for (viewer, session) in [
        ("tui-1", &me),
        ("tui-2", &open),
        ("tab-1", &members),
        ("tab-2", &child),
    ] {
        state
            .presence
            .report(viewer, Some(session), crate::runtime::WEB_PRESENCE_TTL);
    }
    run_in(&state, &running, "turn-running");

    let peers = peer_sessions(&state, &me).unwrap();
    let mut ids: Vec<_> = peers.iter().map(|peer| peer.session_id.clone()).collect();
    ids.sort();
    let mut want = vec![open.clone(), running.clone()];
    want.sort();
    assert_eq!(ids, want, "不含自己、关着的、别人的、子代理");
    let open_peer = peers.iter().find(|peer| peer.session_id == open).unwrap();
    assert!(open_peer.open && !open_peer.running);
    assert_eq!(open_peer.name, "查资料");
    assert_eq!(open_peer.mode, "normal");
    assert!(
        open_peer.data.ends_with("conversation.db"),
        "{}",
        open_peer.data
    );
    let running_peer = peers
        .iter()
        .find(|peer| peer.session_id == running)
        .unwrap();
    assert!(running_peer.running && !running_peer.open);
    assert!(!closed.is_empty());
}

#[tokio::test]
async fn a_message_to_a_running_session_joins_its_turn() {
    let temp = tempfile::tempdir().unwrap();
    let state = DaemonState::for_test(test_paths(temp.path()), 8300).unwrap();
    let me = user_session(&state, "写代码", "");
    let target = user_session(&state, "跑测试", "");
    let store = run_in(&state, &target, "turn-target");
    let port = CrossSessionPortProbe(state.clone());

    let (peer, delivered) = port
        .send(&me, &target, "构建好了，你那边可以跑了")
        .await
        .unwrap();
    assert_eq!(delivered, PeerDelivery::Queued);
    assert_eq!(peer.session_id, target);
    let queued = store.load_queued_prompts().unwrap();
    assert_eq!(queued.len(), 1);
    let message =
        miyu_core::state::parse_cross_session_message(&queued[0].content).expect("envelope");
    assert_eq!(message.from_name, "写代码");
    assert_eq!(message.from_session, me);
    assert_eq!(message.body, "构建好了，你那边可以跑了");
}

#[tokio::test]
async fn an_idle_open_session_gets_a_turn_started_for_it() {
    let temp = tempfile::tempdir().unwrap();
    let (state, _actor) = DaemonState::for_test_with_actor(test_paths(temp.path()), 8300).unwrap();
    let me = user_session(&state, "写代码", "");
    let target = user_session(&state, "查资料", "");
    state
        .presence
        .report("tab-1", Some(&target), crate::runtime::WEB_PRESENCE_TTL);

    let (_, delivered) = CrossSessionPortProbe(state.clone())
        .send(&me, &target, "帮我查一下")
        .await
        .unwrap();
    assert_eq!(delivered, PeerDelivery::Started);
}

#[tokio::test]
async fn sessions_outside_the_list_are_refused() {
    let temp = tempfile::tempdir().unwrap();
    let state = DaemonState::for_test(test_paths(temp.path()), 8300).unwrap();
    let me = user_session(&state, "写代码", "");
    let closed = user_session(&state, "早就关了", "");
    let port = CrossSessionPortProbe(state.clone());

    let to_self = port.send(&me, &me, "x").await.unwrap_err();
    assert!(to_self.to_string().contains("is this session"), "{to_self}");
    let to_closed = port.send(&me, &closed, "x").await.unwrap_err();
    assert!(
        to_closed
            .to_string()
            .contains("not an open or running session"),
        "{to_closed}"
    );
    assert!(state.manager.lock().unwrap().active_runs.is_empty());
}

/// 界面上显示的是短 id（用户 09-24），她照着说「发给 acd86d38」也得发得到。
#[tokio::test]
async fn a_short_id_reaches_the_session_it_names() {
    let temp = tempfile::tempdir().unwrap();
    let state = DaemonState::for_test(test_paths(temp.path()), 8300).unwrap();
    let me = user_session(&state, "写代码", "");
    let target = user_session(&state, "跑测试", "");
    let store = run_in(&state, &target, "turn-target");
    let short = miyu_core::state::short_session_id(&target).to_string();
    assert_ne!(short, target);

    let (peer, delivered) = CrossSessionPortProbe(state.clone())
        .send(&me, &short, "短 id 也认")
        .await
        .unwrap();
    assert_eq!(peer.session_id, target);
    assert_eq!(peer.short_id, short);
    assert_eq!(delivered, PeerDelivery::Queued);
    assert_eq!(store.load_queued_prompts().unwrap().len(), 1);

    let own_short = miyu_core::state::short_session_id(&me).to_string();
    let to_self = CrossSessionPortProbe(state.clone())
        .send(&me, &own_short, "x")
        .await
        .unwrap_err();
    assert!(to_self.to_string().contains("is this session"), "{to_self}");
}

/// 测试直接调端口实现，不经全局装入（别的测试可能装了假端口）。
struct CrossSessionPortProbe(DaemonState);

impl CrossSessionPortProbe {
    async fn send(
        &self,
        from: &str,
        to: &str,
        message: &str,
    ) -> anyhow::Result<(miyu_base::host_ports::PeerSession, PeerDelivery)> {
        send_to_peer(&self.0, from, to, message).await
    }
}
