//! 会话引用解析、对话重置与状态快照。
//!
//! 与 `http` 的分工：这里是「拿到一条会话该怎么算」，`http` 是「请求怎么进来」。

use crate::web::*;

/// `session_id` 是不是侧栏里最后一个会话(普通 + dev 分组,终端集成会话不算);
/// 是的话先建一个空会话顶上,并广播 `session.created`。
pub(super) fn replacement_for_last_session(
    state: &DaemonState,
    identity: &WebIdentity,
    session_id: &str,
) -> std::result::Result<Option<miyu_core::state::SessionRecord>, ApiError> {
    let persona = active_persona_scope(state);
    let own_store = state
        .stores
        .for_identity(identity)
        .map_err(ApiError::internal)?;
    let others_remain = sessions_with_dev(&own_store, &persona, identity.owner_key())
        .map_err(ApiError::internal)?
        .iter()
        .any(|overview| {
            let id = overview.record.session_id.as_str();
            id != session_id && id != miyu_core::state::DEFAULT_SESSION_ID
        });
    if others_remain {
        return Ok(None);
    }
    // 只在被删的确实是个可见会话时才顶替:id 打错了直接让删除那步报 404。
    let exists = own_store
        .session_record(session_id)
        .map_err(ApiError::internal)?
        .is_some();
    if !exists {
        return Ok(None);
    }
    // 成员的顶替会话归成员、挂他当前的人格。
    let persona = if identity.admin {
        persona
    } else {
        member_session_persona(state, identity.owner_key())
    };
    let record = own_store
        .create_session_for_owner(
            &persona,
            "",
            miyu_core::state::USER_SESSION_KIND,
            None,
            identity.owner_key(),
        )
        .map_err(ApiError::internal)?;
    state
        .stores
        .note_session_owner(&record.session_id, identity.owner_key());
    publish_session_created(state, &record);
    Ok(Some(record))
}

/// Same, but for the two callers that must also reach one-shot `ask` sessions
/// (running their turn, then deleting them). `SessionRef::Name` still cannot
/// find those — the DB lookup filters to user sessions — so only the client
/// holding the freshly minted id can address one.
/// `owner`(阶段 5):Some(归属键) 时只放行该账号名下的会话——HTTP 路径
/// 一律带;IPC/工具桥/测试传 None(终端就是管理员,不再另查)。
pub(in crate::web) fn resolve_local_session_ref_with_kinds(
    state: &DaemonState,
    target: &ipc::SessionRef,
    kinds: &[&str],
    owner: Option<&str>,
) -> std::result::Result<miyu_core::state::SessionRecord, String> {
    // 归属键给了就用那个人的库(成员自己一份);IPC/桥(None)按 id 找会话在
    // 谁的库里(HTTP 路径已经用身份验过归属才走到这),其余 = 管理员库。
    let store = match owner {
        Some(owner) if !owner.is_empty() => state
            .stores
            .for_owner(owner)
            .map_err(|error| safe_error_message(&error))?,
        _ => match target {
            ipc::SessionRef::Id { id } => state.stores.for_session(id),
            _ => state.state_store.clone(),
        },
    };
    let store = &store;
    let persona = active_persona_scope(state);
    let record = match target {
        ipc::SessionRef::Current => match owner {
            Some(owner) if !owner.is_empty() => {
                let id = member_current_session(state, owner)?;
                store
                    .session_record(&id)
                    .map_err(|error| safe_error_message(&error))?
            }
            _ => store
                .session_record(&store.session_id())
                .map_err(|error| safe_error_message(&error))?,
        },
        ipc::SessionRef::Id { id } => store
            .session_record(id)
            .map_err(|error| safe_error_message(&error))?,
        ipc::SessionRef::Name { name } => store
            .find_local_session_by_name(&persona, name)
            .map_err(|error| safe_error_message(&error))?,
    };
    let Some(record) = record else {
        return Err(t("session not found", "找不到该会话").to_string());
    };
    let is_platform = store
        .is_platform_session(&record.session_id)
        .map_err(|error| safe_error_message(&error))?;
    // 人格过滤只约束按名寻址与当前指针:显式 id 是不可猜测的能力凭据,
    // 且 dev 会话(保留人格 "dev")必须能被 dev REPL 按 id 操作——否则
    // 起回合/切换/指针全部 404(验收问题二:dev 首启即被踢回默认会话)。
    // 成员的会话可能挂在私有人格上:归属对得上就不看人格。
    let member_owned = owner.is_some_and(|owner| !owner.is_empty() && record.owner == owner);
    let persona_ok = record.persona == persona
        || record.persona == miyu_core::state::DEV_PERSONA
        || matches!(target, ipc::SessionRef::Id { .. })
        || member_owned;
    let owner_ok = owner.is_none_or(|owner| record.owner == owner);
    if !persona_ok || !owner_ok || !kinds.contains(&record.kind.as_str()) || is_platform {
        return Err(t("session not found", "找不到该会话").to_string());
    }
    Ok(record)
}

pub(in crate::web) async fn reset_conversation(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Json(request): Json<ResetConversationRequest>,
) -> std::result::Result<StatusCode, ApiError> {
    require_mutation(&headers, &state)?;
    let identity = require_identity(&headers, &state)?;
    let session_id = match request.session_id {
        Some(session_id) => session_id,
        None => resolve_turn_session(&state, Some(identity.owner_key()), None)
            .map_err(session_api_error)?
            .to_string(),
    };
    require_local_web_session(&state, &headers, &session_id)?;
    let store = state.stores.for_session(&session_id).pinned(&session_id);
    if store.has_running_turns().map_err(ApiError::internal)? {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "a conversation turn is already running",
        ));
    }
    // reset = 从头来过:子代理树连根拔(09-18 会话化,用户拍板)。
    teardown_subagent_tree(&state, &session_id).await;
    reserve_admin_for_session(&state.manager, &session_id)?;
    let (reply, receiver) = oneshot::channel();
    if state
        .actor_tx
        .send(ActorCommand::ResetConversation {
            session_id: session_id.into(),
            reply,
        })
        .is_err()
    {
        release_admin(&state.manager);
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "agent worker is unavailable",
        ));
    }
    match receiver.await {
        Ok(Ok(())) => Ok(StatusCode::NO_CONTENT),
        Ok(Err(AdminFailure::Invalid(message))) => {
            Err(ApiError::new(StatusCode::CONFLICT, message))
        }
        Ok(Err(AdminFailure::Internal(message))) => {
            tracing::error!(
                error = %message,
                "{}",
                t("WebUI conversation reset failed", "WebUI 对话重置失败")
            );
            Err(ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                safe_error_message(&message),
            ))
        }
        Err(_) => {
            release_admin(&state.manager);
            Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "agent worker stopped before resetting the conversation",
            ))
        }
    }
}

/// Best-effort AI pass over the truncated default session name: ask the
/// main model pool for a concise title and apply it only if the
/// auto-generated name is still in place (a user rename wins). Runs
/// detached on the actor's LocalSet — never blocks the turn.
pub(in crate::web) fn spawn_session_title_refinement(
    config: &AppConfig,
    paths: &MiyuPaths,
    store: &StateStore,
    events: &EventHub,
    fallback: String,
    seed: &str,
) {
    // 标题走 model_tiers.roles.session_title 指定的档位池(未配置=主池):
    // 一条 16 字标题不值一次旗舰调用。
    let Ok(client) = OpenAiCompatibleClient::from_aux_role(
        config,
        paths,
        miyu_base::config::AuxRole::SessionTitle,
    ) else {
        return;
    };
    // 标题生成是侧信道:scope 留在默认 "chat" 会让缓存记账把它算进主对话
    // (08-10 调研 P2),claude-code 中转还会为它建持久会话且不进会话映射
    // (清空联动删不到)。
    let client = client.with_request_scope("session-title");
    let store = store.clone();
    let events = events.clone();
    let seed = seed.to_string();
    tokio::task::spawn_local(async move {
        let session_id = store.session_id();
        let prompt = format!(
            "为下面这条用户消息生成一个简洁的会话标题：不超过 16 个字，概括主题，只输出标题本身，不要引号、句号或解释。

用户消息：{seed}"
        );
        let result = client
            .chat_stream(
                vec![
                    miyu_core::llm::ChatMessage::system("你是会话标题生成器，只输出标题本身。"),
                    miyu_core::llm::ChatMessage::plain("user", prompt),
                ],
                Vec::new(),
                |_| Ok(()),
            )
            .await;
        let Ok(result) = result else { return };
        let title = sanitize_session_title(&result.content);
        if title.is_empty() {
            return;
        }
        let Ok(Some(record)) = store.session_record(&session_id) else {
            return;
        };
        if record.name != fallback {
            return;
        }
        if store.rename_session(&record.session_id, &title).is_ok() {
            events.publish(
                "session.renamed",
                json!({ "session_id": record.session_id, "name": title }),
            );
        }
        if let Some(usage) = result.usage.as_ref() {
            let meta = miyu_core::state::UsageMeta {
                source: "agent",
                provider: result.provider_id.as_deref(),
                model: result.model.as_deref(),
                kind: None,
            };
            let _ = store.add_auxiliary_usage(usage, meta);
        }
    });
}

/// Cleans an LLM-generated title down to a single short line: first line
/// only, surrounding quotes/punctuation stripped, clipped to 20 chars.
pub(in crate::web) fn sanitize_session_title(raw: &str) -> String {
    let cleaned = raw
        .trim()
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .trim_matches(|c: char| {
            matches!(
                c,
                '"' | '\''
                    | '“'
                    | '”'
                    | '‘'
                    | '’'
                    | '「'
                    | '」'
                    | '《'
                    | '》'
                    | '。'
                    | '.'
                    | '，'
                    | ','
            )
        })
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    cleaned.chars().take(20).collect()
}

pub(in crate::web) fn session_state_for(
    state: &DaemonState,
    session_id: &str,
) -> Result<ipc::SessionState> {
    let session_store = state.stores.for_session(session_id);
    let record = session_store
        .session_record(session_id)?
        .with_context(|| format!("session not found: {session_id}"))?;
    let current_session_id = state.state_store.session_id();
    // 会话钉了模型池就按那个池算窗口。回合路(turns/task.rs)与压缩路
    // (actor)都套了这条覆盖,快照路此前漏了:打开页面、刷新、切换会话时
    // 上下文条显示的都是全局池的窗口,要跑完一轮才被 run.completed 纠正。
    let mut config = state.manager.lock().unwrap().config.clone();
    apply_session_model_override_to(&mut config, &session_store, session_id);
    let mut context = if &*current_session_id == session_id {
        state.manager.lock().unwrap().context
    } else {
        // dev 会话按 dev 装配估算：系统提示词、工具表、记忆钥匙都跟着模式
        // 走，拿 Normal 硬算的话，dev 空会话和普通空会话永远是同一个数。
        let (config, mode) = if record.persona == miyu_core::state::DEV_PERSONA {
            (config.dev_scoped(), PersonaLane::Dev)
        } else {
            (config.clone(), PersonaLane::Active)
        };
        let store = session_store.pinned(session_id);
        current_context(&build_session_agent(&config, &state.paths, &store, mode)?)?
    };
    if let Some((window, source)) = config.active_context_window_with_source()? {
        context.window = Some(window);
        context.window_assumed = matches!(source, miyu_base::config::ContextWindowSource::Assumed);
    }
    // `/sandbox` 查看与状态行:摘要来自真正会装进规则集的策略(清单里不存在的路径
    // 不列),跟回合走同一个 session_scope——绑定、默认沙盒、只读三种都照实报。
    // 成员不在这里报(他们的沙盒不归自己管,查看照旧说「没绑」)。
    let member = member_owns_session(&state.state_store, &state.stores, session_id);
    let policy = (!member)
        .then(|| {
            session_scope(
                &state.paths,
                &state.state_store,
                &state.stores,
                &config,
                session_id,
                None,
            )
            .policy
        })
        .flatten();
    let (sandbox_writable, sandbox_readable) = policy
        .as_ref()
        .map(|policy| {
            (
                policy.writable_summary.clone(),
                policy.readable_summary.clone(),
            )
        })
        .unwrap_or_default();
    // 默认根跟客户端目录走(自动检测),查看时就是回合那边算出来的那一个:
    // 没绑、没说过不要、默认开着时,策略的根就是它。
    let default_root = (!member
        && record.sandbox.is_none()
        && !record.sandbox_opt_out
        && !record.sandbox_readonly
        && config.tools.sandbox.default_enabled)
        .then(|| policy.as_ref().map(|policy| policy.root.clone()))
        .flatten();
    let sandbox_default = default_root.is_some();
    let sandbox = record
        .sandbox
        .clone()
        .or_else(|| default_root.map(|root| root.to_string_lossy().into_owned()));
    Ok(ipc::SessionState {
        context_tokens: context.tokens,
        context_window: context.window,
        context_window_assumed: context.window_assumed,
        cumulative_tokens: context.cumulative_tokens,
        cumulative_prompt_tokens: context.cumulative_prompt_tokens,
        cumulative_cache_read_tokens: context.cumulative_cache_read_tokens,
        mode: super::session_mode_label(&record).to_string(),
        session_id: record.session_id,
        session_name: record.name,
        sandbox,
        sandbox_default,
        sandbox_readonly: !member && record.sandbox_readonly,
        sandbox_writable,
        sandbox_readable,
    })
}
