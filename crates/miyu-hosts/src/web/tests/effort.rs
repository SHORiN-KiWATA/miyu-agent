//! 思考档位（effort）的写入（09-24）：别的会话在跑也能改。

use super::shared::*;
use crate::web::*;
use miyu_base::workspace::TurnOrigin;

/// 退回修复前这条会红：管理员改档位走的是「全局没有任何回合在跑」的重预约，别的会话
/// 在跑就 409（用户 09-24：「有会话在运行的时候没法切换其中一个会话的 effort」）。
#[tokio::test]
async fn an_admin_can_change_effort_while_another_session_runs() {
    let temp = tempfile::tempdir().unwrap();
    let (state, _actor) = DaemonState::for_test_with_actor(test_paths(temp.path()), 8300).unwrap();
    let (cancel, _cancel_rx) = tokio::sync::watch::channel(false);
    state.manager.lock().unwrap().active_runs.insert(
        "run-elsewhere".to_string(),
        RunInfo {
            session_id: "sess_elsewhere".into(),
            mode: PersonaLane::Active,
            audience: PromptAudience::Owner,
            cancel,
            turn_id: Some("turn_elsewhere".to_string()),
            queue_target: None,
            supersede: Arc::new(miyu_engine::agent::TurnSupersedeSignal::default()),
            platform_followup: None,
            operation: RunOperation::Create,
            job_wake: false,
            turn_origin: TurnOrigin::Human,
            job_wake_label: None,
            first_event_id: None,
        },
    );
    let token = state.auth.issue(crate::runtime::WebIdentity::local_admin());
    let mut headers = HeaderMap::new();
    headers.insert(
        axum::http::header::COOKIE,
        format!("{AUTH_COOKIE}={token}").parse().unwrap(),
    );
    let choice = state
        .manager
        .lock()
        .unwrap()
        .config
        .active_provider_model_choices()
        .into_iter()
        .next()
        .unwrap();
    let result = set_thinking_variants(
        axum::extract::State(state.clone()),
        headers,
        axum::Json(SetThinkingVariantsRequest {
            updates: vec![ThinkingVariantUpdate {
                provider_id: choice.provider_id,
                model: choice.model,
                selected: None,
            }],
        }),
    )
    .await;
    assert!(
        result.is_ok(),
        "effort change was refused while another session ran"
    );
    assert!(
        !state.manager.lock().unwrap().admin_busy,
        "the reservation must be released"
    );
}

/// 会话级档位（09-24：effort 做成会话级）：网页改的是这个会话钉住的那一档，全局默认档
/// 不动；选「跟随全局」（null）就是拔掉钉子；没有的档位拒掉。退回「网页改全局」这条会红。
#[tokio::test]
async fn the_web_pins_effort_on_one_session_and_leaves_the_global_default_alone() {
    use miyu_core::llm::{ThinkingVariantPreferences, ThinkingVariantScope};
    let temp = tempfile::tempdir().unwrap();
    let state = DaemonState::for_test(test_paths(temp.path()), 8300).unwrap();
    // codex 线自带档位表，不靠 models.dev 元数据。
    let choice = {
        let mut manager = state.manager.lock().unwrap();
        let choice = manager
            .config
            .active_provider_model_choices()
            .into_iter()
            .next()
            .unwrap();
        let provider = manager
            .config
            .providers
            .iter_mut()
            .find(|provider| provider.id == choice.provider_id)
            .unwrap();
        provider.protocol = "codex".to_string();
        choice
    };
    let session = state
        .state_store
        .create_session(
            &active_persona_scope(&state),
            "work",
            miyu_core::state::USER_SESSION_KIND,
            None,
        )
        .unwrap();
    let token = state.auth.issue(crate::runtime::WebIdentity::local_admin());
    let mut headers = HeaderMap::new();
    headers.insert(
        axum::http::header::COOKIE,
        format!("{AUTH_COOKIE}={token}").parse().unwrap(),
    );
    let request = |selected: Option<&str>| {
        axum::Json(SetThinkingVariantsRequest {
            updates: vec![ThinkingVariantUpdate {
                provider_id: choice.provider_id.clone(),
                model: choice.model.clone(),
                selected: selected.map(str::to_string),
            }],
        })
    };
    let put = |selected: Option<&str>| {
        set_session_thinking_variants_http(
            axum::extract::State(state.clone()),
            headers.clone(),
            axum::extract::Path(session.session_id.clone()),
            request(selected),
        )
    };
    let pinned = || {
        ThinkingVariantPreferences::load_scoped(
            &state.paths,
            ThinkingVariantScope::Session {
                store: &state.state_store,
                session_id: &session.session_id,
            },
        )
        .selected(&choice.provider_id, &choice.model)
        .map(str::to_string)
    };

    assert!(put(Some("high")).await.is_ok());
    assert_eq!(pinned().as_deref(), Some("high"));
    assert_eq!(
        ThinkingVariantPreferences::load(&state.paths).selected(&choice.provider_id, &choice.model),
        None,
        "the global default stays where it was"
    );
    assert!(
        put(Some("ultra")).await.is_err(),
        "unknown levels are refused"
    );
    assert_eq!(pinned().as_deref(), Some("high"));
    // 「模型默认」是一种钉法（用户 09-24：选默认是模型默认，不是回到跟随全局）。
    assert!(put(Some(miyu_core::llm::MODEL_DEFAULT_PIN)).await.is_ok());
    assert_eq!(pinned().as_deref(), Some(miyu_core::llm::MODEL_DEFAULT_PIN));
    assert!(put(None).await.is_ok());
    assert_eq!(pinned(), None, "follow global removes the pin");
}
