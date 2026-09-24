//! 会话的 HTTP 出口：列表、增删改、排序、待办与回合快照。
//!
//! 这些 handler 只做鉴权、归属校验与形状转换，真正的解析与状态装配在
//! `state` 与 `mod` 里。

use crate::web::*;

use super::state::replacement_for_last_session;

pub(in crate::web) async fn list_sessions_http(
    State(state): State<DaemonState>,
    headers: HeaderMap,
) -> std::result::Result<Response, ApiError> {
    let identity = require_identity(&headers, &state)?;
    let persona = active_persona_scope(&state);
    // 侧栏按模式分组:普通+dev 一起下发,mode 字段区分(问题七)。
    // 归属(阶段 5):各人只看自己名下的;管理员名下 = 遗留 + 终端 + 自己建的。
    let store = state
        .stores
        .for_identity(&identity)
        .map_err(ApiError::internal)?;
    let sessions =
        sessions_with_dev(&store, &persona, identity.owner_key()).map_err(ApiError::internal)?;
    let current = current_session_for(&state, &identity, &sessions);
    let sessions = sessions
        .iter()
        .map(|overview| session_overview_json(overview, &current))
        .collect::<Vec<_>>();
    let data = json!({ "current": current, "sessions": sessions });
    Ok(Json(data).into_response())
}

#[derive(Deserialize)]
pub(in crate::web) struct CreateSessionRequest {
    #[serde(default)]
    pub(in crate::web) name: Option<String>,
    #[serde(default)]
    pub(in crate::web) switch: bool,
    /// "dev" 建 Build 模式会话(保留人格 dev);缺省=当前人格普通会话。
    #[serde(default)]
    pub(in crate::web) mode: Option<String>,
}

pub(in crate::web) async fn create_session_http(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Json(request): Json<CreateSessionRequest>,
) -> std::result::Result<Response, ApiError> {
    require_mutation(&headers, &state)?;
    let identity = require_identity(&headers, &state)?;
    if !identity.admin {
        // 成员的会话归成员名下,不动全局指针。dev 会话(run_command 等)成员也能开
        // (09-11 起有 Landlock 沙盒兜底):建到保留人格 dev 名下,模式由它推导。
        let persona = if request.mode.as_deref() == Some("dev") {
            miyu_core::state::DEV_PERSONA.to_string()
        } else {
            member_session_persona(&state, identity.owner_key())
        };
        let name = request
            .name
            .map(|name| name.trim().to_string())
            .unwrap_or_default();
        let record = state
            .stores
            .for_identity(&identity)
            .map_err(ApiError::internal)?
            .create_session_for_owner(
                &persona,
                &name,
                miyu_core::state::USER_SESSION_KIND,
                None,
                identity.owner_key(),
            )
            .map_err(ApiError::internal)?;
        state
            .stores
            .note_session_owner(&record.session_id, identity.owner_key());
        publish_session_created(&state, &record);
        let data = json!({ "session": session_record_json(&record) });
        return Ok((StatusCode::CREATED, Json(data)).into_response());
    }
    let data = handle_session_command(
        &state,
        IpcCommand::CreateSession {
            name: request.name,
            switch: request.switch,
            kind: None,
            mode: request.mode,
        },
    )
    .await
    .map_err(session_api_error)?;
    Ok((StatusCode::CREATED, Json(data)).into_response())
}

#[derive(Deserialize)]
pub(in crate::web) struct UpdateSessionRequest {
    #[serde(default)]
    pub(in crate::web) name: Option<String>,
    /// `Some("")` unbinds the session sandbox; a non-empty path binds it.
    #[serde(default)]
    pub(in crate::web) sandbox: Option<String>,
    /// `/sandbox <路径> --allow-read`:读放开到整个文件系统,写照旧锁在根里。
    /// 只在同一个请求里绑定时有意义。
    #[serde(default)]
    pub(in crate::web) sandbox_allow_read: bool,
}

#[derive(Deserialize)]
pub(in crate::web) struct ReorderSessionsRequest {
    pub(in crate::web) session_ids: Vec<String>,
}

/// 侧栏拖拽排序:按给定顺序重写会话展示序。
pub(in crate::web) async fn reorder_sessions_http(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Json(request): Json<ReorderSessionsRequest>,
) -> std::result::Result<Response, ApiError> {
    require_mutation(&headers, &state)?;
    for session_id in &request.session_ids {
        require_local_web_session(&state, &headers, session_id)?;
    }
    let data = handle_session_command(
        &state,
        IpcCommand::ReorderSessions {
            session_ids: request.session_ids,
        },
    )
    .await
    .map_err(session_api_error)?;
    Ok(Json(data).into_response())
}

pub(in crate::web) async fn update_session_http(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
    Json(request): Json<UpdateSessionRequest>,
) -> std::result::Result<Response, ApiError> {
    require_mutation(&headers, &state)?;
    require_local_web_session(&state, &headers, &session_id)?;
    let target = || ipc::SessionRef::Id {
        id: session_id.clone(),
    };
    if let Some(name) = request.name {
        handle_session_command(
            &state,
            IpcCommand::RenameSession {
                target: target(),
                name,
            },
        )
        .await
        .map_err(session_api_error)?;
    }
    if let Some(sandbox) = request.sandbox {
        let root = (!sandbox.trim().is_empty()).then(|| std::path::PathBuf::from(sandbox));
        handle_session_command(
            &state,
            IpcCommand::SetSandbox {
                target: target(),
                root,
                allow_read: request.sandbox_allow_read,
            },
        )
        .await
        .map_err(session_api_error)?;
    }
    Ok(Json(json!({})).into_response())
}

/// 某个会话当前的待办清单。
///
/// WebUI 侧边有一块常驻面板显示它。工具事件只在 `todowrite` 跑的那一刻发生
/// 一次，刷新页面或切回来就没了；这个接口让面板每次进会话都能拿到当前状态。
pub(in crate::web) async fn session_todos_http(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
) -> std::result::Result<Json<Value>, ApiError> {
    require_auth(&headers, &state)?;
    require_local_web_session(&state, &headers, &session_id)?;
    let store = state.stores.for_session(&session_id);
    let todos = tools::session_todos(&store, &session_id);
    Ok(Json(json!({ "todos": todos })))
}

/// Read-only snapshot of one session's conversation for per-view browsing:
/// turns, queued follow-ups, and its currently running turns. Does not touch
/// the global current-session pointer.
pub(in crate::web) async fn session_turns_http(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
) -> std::result::Result<Response, ApiError> {
    require_auth(&headers, &state)?;
    require_local_web_session(&state, &headers, &session_id)?;
    let store = state.stores.for_session(&session_id).pinned(&session_id);
    let mut assets_by_turn = HashMap::<String, Vec<ImageAsset>>::new();
    for asset in store.load_image_assets().map_err(ApiError::internal)? {
        assets_by_turn
            .entry(asset.turn_id.clone())
            .or_default()
            .push(asset);
    }
    let mut artifacts_by_turn = HashMap::<String, Vec<ArtifactAsset>>::new();
    for artifact in store.load_artifact_assets().map_err(ApiError::internal)? {
        artifacts_by_turn
            .entry(artifact.turn_id.clone())
            .or_default()
            .push(artifact);
    }
    let generation_by_turn = store
        .load_turn_generation(&session_id)
        .map_err(ApiError::internal)?;
    let turns: Vec<SafeTurn> = store
        .load_turns()
        .map_err(ApiError::internal)?
        .into_iter()
        .filter(|turn| !turn.is_summary)
        .map(|turn| {
            let assets = assets_by_turn.remove(&turn.turn_id).unwrap_or_default();
            let artifacts = artifacts_by_turn.remove(&turn.turn_id).unwrap_or_default();
            let mut safe = SafeTurn::from_turn(turn, assets, artifacts);
            if let Some((tokens, millis)) = generation_by_turn.get(&safe.id) {
                safe.generation_tokens = *tokens;
                safe.generation_ms = *millis;
            }
            safe
        })
        .collect();
    let running_target = store
        .running_turn_queue_target()
        .map_err(ApiError::internal)?;
    let queued_prompts: Vec<SafeQueuedPrompt> = match running_target.as_ref() {
        Some(target) => store
            .load_queued_prompts_for_target(target)
            .map_err(ApiError::internal)?,
        None => Vec::new(),
    }
    .into_iter()
    .map(SafeQueuedPrompt::from)
    .collect();
    let runs: Vec<Value> = state
        .manager
        .lock()
        .unwrap()
        .active_runs
        .iter()
        .filter(|(_, info)| &*info.session_id == session_id.as_str())
        .map(|(run_id, info)| {
            json!({
                "run_id": run_id,
                "session_id": &*info.session_id,
                "mode": mode_name(info.mode),
                "operation": info.operation.name(),
                "turn_id": info.operation.turn_id(),
                "input_id": info.operation.input_id(),
            })
        })
        .collect();
    let redo_candidate = if runs.is_empty() {
        store
            .redo_candidate()
            .map_err(ApiError::internal)?
            .map(SafeRedoCandidate::from)
    } else {
        None
    };
    let mut response = Json(json!({
        "session_id": session_id,
        "turns": turns,
        "queued_prompts": queued_prompts,
        "running_turn_id": running_target.as_ref().map(|target| target.turn_id.as_str()),
        "runs": runs,
        "redo_candidate": redo_candidate,
    }))
    .into_response();
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

pub(in crate::web) async fn delete_session_http(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
) -> std::result::Result<Response, ApiError> {
    require_mutation(&headers, &state)?;
    require_local_web_session(&state, &headers, &session_id)?;
    // 删的是侧栏里最后一个会话时,顶替的新会话由这里建,不留给客户端:
    // 每个开着这个会话的页面(桌面 + 手机、两个标签页)都会收到
    // session.deleted 并各自兜底新建,删一个凭空多出两个(09-10 复现)。
    // 先建后删,`session.created` 就排在 `session.deleted` 前面到达,
    // 其它客户端兜底时列表里已经有它,不会再自己 POST 一个。
    let identity = require_identity(&headers, &state)?;
    let replacement = replacement_for_last_session(&state, &identity, &session_id)?;
    let deleted = handle_session_command(
        &state,
        IpcCommand::DeleteSession {
            target: ipc::SessionRef::Id { id: session_id },
        },
    )
    .await;
    if let Err(error) = deleted {
        if let Some(record) = &replacement {
            // 没删成就不该多出一个空会话;删不掉也只是留个空壳,不遮原错误。
            let _ = state
                .stores
                .for_session(&record.session_id)
                .delete_session(&record.session_id);
            state.events.publish(
                "session.deleted",
                json!({ "session_id": record.session_id }),
            );
        }
        return Err(session_api_error(error));
    }
    Ok(Json(json!({ "fallback": replacement.as_ref().map(session_record_json) })).into_response())
}

/// 会话上下文占用快照，给输入框角落的上下文条用。
///
/// 切到非当前会话时 `session_state_for` 会冷装配一次该会话的上下文，有成本，
/// 但只在切换那一下发生；之后靠 run 事件携带的增量刷新。没有它，切换会话后
/// 上下文条一直显示上一个会话的数字，直到跑完一轮才纠正。
pub(in crate::web) async fn session_context_http(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    require_auth(&headers, &state)?;
    require_local_web_session(&state, &headers, &session_id)?;
    let snapshot = session_state_for(&state, &session_id).map_err(ApiError::internal)?;
    // 沙盒三件顺路带上:WebUI 的 `/sandbox`(不带参数)就靠这条看根与放行摘要。
    // 会话累计也带上:WebUI 换会话后「累计」要按这条会话的权威值(含子代理花销)
    // 重新起算,光靠回合求和会漏掉子代理(09-23)。
    Ok(Json(json!({
        "context_tokens": snapshot.context_tokens,
        "context_window": snapshot.context_window,
        "context_window_assumed": snapshot.context_window_assumed,
        "cumulative_tokens": snapshot.cumulative_tokens,
        "cumulative_prompt_tokens": snapshot.cumulative_prompt_tokens,
        "cumulative_cache_read_tokens": snapshot.cumulative_cache_read_tokens,
        "sandbox": snapshot.sandbox,
        "sandbox_writable": snapshot.sandbox_writable,
        "sandbox_readable": snapshot.sandbox_readable,
    })))
}

#[derive(Deserialize)]
pub(in crate::web) struct PresenceRequest {
    viewer: String,
    #[serde(default)]
    session_id: Option<String>,
}

/// 网页标签页的在线登记（09-23，跨会话消息的「开着的会话」名单）。
///
/// 标签页约 20 秒报一次心跳，浏览器会把后台标签的定时器放慢到一分钟一次，所以
/// 过期时间放宽到 150 秒（终端是 15 秒）。关页面时不带 `session_id` 来一次，当场注销。
pub(in crate::web) async fn presence_http(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Json(request): Json<PresenceRequest>,
) -> std::result::Result<Response, ApiError> {
    require_mutation(&headers, &state)?;
    let viewer = request.viewer.trim();
    if viewer.is_empty() || viewer.len() > 64 {
        return Err(ApiError::new(StatusCode::BAD_REQUEST, "invalid viewer id"));
    }
    let session = request.session_id.as_deref().filter(|id| !id.is_empty());
    if let Some(session_id) = session {
        require_local_web_session(&state, &headers, session_id)?;
    }
    // 加前缀：网页报的编号撞不到终端那一份。
    state.presence.report(
        &format!("web-{viewer}"),
        session,
        crate::runtime::WEB_PRESENCE_TTL,
    );
    Ok(StatusCode::NO_CONTENT.into_response())
}
