//! 回到正跑着的会话时，快照报这一轮的实时数（09-25，用户：「切进去之前上下文几十 k，出来变
//! 13k」——这一轮还没落库，快照拿库里现估，只剩系统提示词加前几轮）。

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

#[test]
fn a_running_session_reports_its_live_figures_until_the_run_leaves() {
    let temp = tempfile::tempdir().unwrap();
    let state = DaemonState::for_test(test_paths(temp.path()), 8300).unwrap();
    let persona = active_persona_scope(&state);
    let session = state
        .state_store
        .create_session(&persona, "主会话", "user", None)
        .unwrap()
        .session_id;
    let stored = session_state_for(&state, &session).unwrap();
    {
        let mut manager = state.manager.lock().unwrap();
        manager
            .active_runs
            .insert("running".to_string(), fake_run(&session));
        manager.live_turns.insert(
            session.clone(),
            crate::runtime::LiveTurnFigures {
                context_tokens: 133_900,
                cumulative_tokens: 760_200,
                cumulative_prompt_tokens: 686_806,
                cumulative_cache_read_tokens: 557_568,
            },
        );
    }

    let live = session_state_for(&state, &session).unwrap();
    assert_eq!(live.context_tokens, 133_900);
    assert_eq!(live.cumulative_tokens, 760_200);
    assert_eq!(live.cumulative_prompt_tokens, 686_806);
    assert_eq!(live.cumulative_cache_read_tokens, 557_568);

    crate::runtime::finish_run(&state.manager, "running", None);
    assert!(state.manager.lock().unwrap().live_turns.is_empty());
    let after = session_state_for(&state, &session).unwrap();
    assert_eq!(after.context_tokens, stored.context_tokens);
}

/// 唤醒轮跑完还在总览里留一会儿（09-26）：一次性命令等子代理是隔一会儿看一眼的，两次之间
/// 就收了的唤醒轮（端点当场报错、秒回）还得找得到、按起点补看。人起的轮、补不出起点的轮不留。
#[test]
fn finished_wake_runs_stay_discoverable_for_a_while() {
    let temp = tempfile::tempdir().unwrap();
    let state = DaemonState::for_test(test_paths(temp.path()), 8300).unwrap();
    let wake = |first_event_id| RunInfo {
        job_wake: true,
        job_wake_label: Some("子代理完成 job-1 · 走查".to_string()),
        first_event_id,
        ..fake_run("sess-main")
    };
    {
        let mut manager = state.manager.lock().unwrap();
        manager
            .active_runs
            .insert("run-wake".to_string(), wake(Some(42)));
        manager
            .active_runs
            .insert("run-blind".to_string(), wake(None));
        manager
            .active_runs
            .insert("run-human".to_string(), fake_run("sess-main"));
    }
    for run_id in ["run-wake", "run-blind", "run-human"] {
        crate::runtime::finish_run(&state.manager, run_id, None);
    }
    let mut manager = state.manager.lock().unwrap();
    let recent = manager
        .recent_wakes
        .list()
        .map(|wake| (wake.run_id.clone(), wake.first_event_id, wake.label.clone()))
        .collect::<Vec<_>>();
    assert_eq!(
        recent,
        [(
            "run-wake".to_string(),
            42,
            Some("子代理完成 job-1 · 走查".to_string())
        )]
    );
}
