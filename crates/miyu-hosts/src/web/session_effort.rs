//! 会话级的思考档位（09-24：「effort 做成会话级」）。
//!
//! 会话钉住的档位存在这个会话自己那一份偏好里（`ThinkingVariantScope::Session`），回合
//! 开始时盖在全局（成员是自己家里那份）档位上（`turns::task`）。改它不碰共享的 agent、
//! 不用预约管理锁：别的会话跑着也能改，这个会话下一轮就用上。没钉的模型跟着全局走，
//! 选「跟随全局」（`selected: null`）就是把钉子拔掉；选「模型默认」钉的是
//! `MODEL_DEFAULT_PIN`，全局设了档位也不带。全局默认档还在 `miyu config` 里改。

use crate::web::*;
use miyu_core::llm::ThinkingVariantScope;

#[derive(Serialize)]
pub(in crate::web) struct SessionThinkingVariantsResponse {
    /// 这个会话钉住的档位；没钉的模型不在里面。
    pinned: Vec<PinnedThinkingVariant>,
}

#[derive(Serialize)]
struct PinnedThinkingVariant {
    provider_id: String,
    model: String,
    selected: String,
}

/// 会话那份档位放在谁家：成员的会话在成员家里，和回合那边同一个口径。
pub(in crate::web) fn session_variant_paths(state: &DaemonState, owner: &str) -> MiyuPaths {
    if owner.is_empty() {
        return state.paths.clone();
    }
    match state.state_store.account_by_id(owner).ok().flatten() {
        Some(account) => state.paths.member_thinking_view(&account.username),
        None => state.paths.clone(),
    }
}

fn pinned_variants(
    config: &AppConfig,
    paths: &MiyuPaths,
    session_id: &str,
) -> Vec<PinnedThinkingVariant> {
    let pinned =
        ThinkingVariantPreferences::load_scoped(paths, ThinkingVariantScope::Session(session_id));
    config
        .provider_model_choices()
        .into_iter()
        .filter_map(|choice| {
            let selected = pinned.selected(&choice.provider_id, &choice.model)?;
            Some(PinnedThinkingVariant {
                selected: selected.to_string(),
                provider_id: choice.provider_id,
                model: choice.model,
            })
        })
        .collect()
}

pub(in crate::web) async fn get_session_thinking_variants_http(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
) -> std::result::Result<Json<SessionThinkingVariantsResponse>, ApiError> {
    require_auth(&headers, &state)?;
    let record = require_local_web_session(&state, &headers, &session_id)?;
    let config = state.manager.lock().unwrap().config.clone();
    let paths = session_variant_paths(&state, &record.owner);
    Ok(Json(SessionThinkingVariantsResponse {
        pinned: pinned_variants(&config, &paths, &record.session_id),
    }))
}

pub(in crate::web) async fn set_session_thinking_variants_http(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
    Json(request): Json<SetThinkingVariantsRequest>,
) -> std::result::Result<Json<SessionThinkingVariantsResponse>, ApiError> {
    require_mutation(&headers, &state)?;
    let record = require_local_web_session(&state, &headers, &session_id)?;
    let updates = validate_thinking_variant_updates(request.updates)?;
    let config = state.manager.lock().unwrap().config.clone();
    let paths = session_variant_paths(&state, &record.owner);
    let options = active_thinking_variant_options(&config, &paths).map_err(ApiError::internal)?;
    let scope = ThinkingVariantScope::Session(&record.session_id);
    let mut pinned = ThinkingVariantPreferences::load_scoped(&paths, scope);
    for update in &updates {
        let option = options
            .iter()
            .find(|option| option.provider_id == update.provider_id && option.model == update.model)
            .ok_or_else(|| {
                ApiError::new(
                    StatusCode::BAD_REQUEST,
                    format!("unknown model: {}/{}", update.provider_id, update.model),
                )
            })?;
        if let Some(selected) = &update.selected {
            let known = selected == miyu_core::llm::MODEL_DEFAULT_PIN
                || option.variants.iter().any(|variant| variant == selected);
            if !known {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    format!(
                        "thinking variant is unavailable for {} / {}: {selected}",
                        update.provider_id, update.model
                    ),
                ));
            }
        }
        pinned.set(&update.provider_id, &update.model, update.selected.clone());
    }
    pinned
        .save_scoped(&paths, scope)
        .map_err(ApiError::internal)?;
    let pinned = pinned_variants(&config, &paths, &record.session_id);
    state.events.publish(
        "session.updated",
        json!({ "session_id": record.session_id, "thinking_variants": pinned }),
    );
    Ok(Json(SessionThinkingVariantsResponse { pinned }))
}
