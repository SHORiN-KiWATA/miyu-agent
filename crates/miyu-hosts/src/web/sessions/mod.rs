//! 会话的增删改查与解析。
//!
//! 「会话引用」不等于会话 ID：前端可以传 ID、也可以传 `current` 这类别名，
//! 还要按 kind 过滤（有些接口只接受能承载回合的会话）。`resolve_local_session_ref*`
//! 这一族就是把这些形态归一到一个真实会话上，失败时给出前端能理解的错误。
//!
//! 自动命名（`maybe_auto_name_session`）放在这里而不是回合模块：它是会话的属
//! 性变更，只是恰好由第一条消息触发。

mod empty_context;
mod http;
mod state;

use crate::web::*;

pub(in crate::web) use empty_context::empty_session_context;
pub(in crate::web) use http::*;
pub(in crate::web) use state::*;

/// 「当前会话」:管理员是 daemon 的全局指针(与 REPL 共用);成员没有全局
/// 指针,拿名下最近活跃的一条。
pub(in crate::web) fn current_session_for(
    state: &DaemonState,
    identity: &WebIdentity,
    sessions: &[miyu_core::state::SessionOverview],
) -> String {
    if identity.admin {
        return state.state_store.session_id().to_string();
    }
    sessions
        .iter()
        .max_by_key(|overview| overview.record.updated_at.clone())
        .map(|overview| overview.record.session_id.clone())
        .unwrap_or_default()
}

/// 成员的当前会话 id:没有就建一条(自动按第一句话命名)。
pub(in crate::web) fn member_current_session(
    state: &DaemonState,
    owner: &str,
) -> std::result::Result<String, String> {
    let store = state
        .stores
        .for_owner(owner)
        .map_err(|error| safe_error_message(&error))?;
    let sessions = store
        .list_owner_sessions(owner)
        .map_err(|error| safe_error_message(&error))?;
    if let Some(overview) = sessions
        .iter()
        .max_by_key(|overview| overview.record.updated_at.clone())
    {
        return Ok(overview.record.session_id.clone());
    }
    let persona = member_session_persona(state, owner);
    let record = store
        .create_session_for_owner(
            &persona,
            "",
            miyu_core::state::USER_SESSION_KIND,
            None,
            owner,
        )
        .map_err(|error| safe_error_message(&error))?;
    state.stores.note_session_owner(&record.session_id, owner);
    publish_session_created(state, &record);
    Ok(record.session_id)
}

/// 成员新会话挂哪个人格:settings 里指着自己的私有人格就用它的 scope,否则共享 Miyu。
pub(in crate::web) fn member_session_persona(state: &DaemonState, owner: &str) -> String {
    let username = state
        .state_store
        .account_by_id(owner)
        .ok()
        .flatten()
        .map(|account| account.username);
    if let Some(username) = username {
        if let Some(persona) = member_persona::active_persona(&state.paths, &username) {
            return persona.scope();
        }
    }
    active_persona_scope(state)
}

pub(in crate::web) fn publish_session_created(
    state: &DaemonState,
    record: &miyu_core::state::SessionRecord,
) {
    state.events.publish(
        "session.created",
        json!({
            "session_id": record.session_id,
            "name": record.name,
            "mode": session_mode_label(record),
        }),
    );
}

#[derive(Deserialize)]
pub(in crate::web) struct ResetConversationRequest {
    pub(in crate::web) session_id: Option<String>,
}

pub(in crate::web) fn resolve_local_session_ref(
    state: &DaemonState,
    target: &ipc::SessionRef,
) -> std::result::Result<miyu_core::state::SessionRecord, String> {
    resolve_local_session_ref_with_kinds(
        state,
        target,
        &[miyu_core::state::USER_SESSION_KIND],
        None,
    )
}

/// 桥与工具目录用的配置:与 turns/task.rs 的成员回合同源——会话归成员就把
/// 家目录(知识库/账本按人分家)与私有人格(提示词/清单/脚本白名单)套上,
/// 否则中转线(claude-code/codex/agy 只能从 MCP 桥拿工具)看到的是管理员的全量
/// 工具面:人格没勾记账也列出 ledger,勾了表情包也用不了。
pub(in crate::web) fn session_scoped_config(state: &DaemonState, session_id: &str) -> AppConfig {
    let mut config = state.manager.lock().unwrap().config.clone();
    let Some(owner) = state.stores.owner_of_session(session_id) else {
        return config;
    };
    if owner.is_empty() {
        return config;
    }
    let Ok(Some(account)) = state.state_store.account_by_id(&owner) else {
        return config;
    };
    config.accounts.home_dir = Some(
        state
            .paths
            .user_home_dir(&account.username)
            .display()
            .to_string(),
    );
    let scope = state
        .stores
        .for_session(session_id)
        .session_record(session_id)
        .ok()
        .flatten()
        .map(|record| record.persona)
        .unwrap_or_default();
    if let Some(persona) =
        member_persona::persona_for_scope(&state.paths, &account.username, &scope)
    {
        member_persona::apply_to_config(&mut config, &persona);
    }
    config
}

/// 工具桥专用的会话寻址:在本地会话之外**额外**放行"正有回合在跑"的平台
/// 会话。MCP 桥(claude-code 供应商唯一的工具通道)带的就是平台会话 id,被
/// 本地解析一律挡掉时,群聊里整套平台工具都调不到(08-26 实测 `tool-call
/// --list` 报"找不到该会话")。放行窗口卡在活回合上:回合结束登记即注销,
/// 桥也随之失去这条会话的寻址能力。
pub(in crate::web) fn resolve_tool_bridge_session_ref(
    state: &DaemonState,
    target: &ipc::SessionRef,
) -> std::result::Result<miyu_core::state::SessionRecord, String> {
    match resolve_local_session_ref_with_kinds(state, target, TURN_TARGET_KINDS, None) {
        Ok(record) => Ok(record),
        Err(error) => {
            let ipc::SessionRef::Id { id } = target else {
                return Err(error);
            };
            if crate::platforms::live_turn_context(id).is_none() {
                return Err(error);
            }
            state
                .state_store
                .session_record(id)
                .map_err(|error| safe_error_message(&error))?
                .ok_or(error)
        }
    }
}

pub(in crate::web) fn resolve_available_local_session_ref(
    state: &DaemonState,
    target: &ipc::SessionRef,
) -> std::result::Result<miyu_core::state::SessionRecord, String> {
    resolve_local_session_ref(state, target)
}

/// Turn targets and deletions additionally accept one-shot `ask` sessions,
/// and (09-18 会话化) subagent sessions: 访问子会话时给它发消息、删掉它都走这里。
/// `SetReplSession` 不用这张表,车道指针永远指不到子会话。
pub(in crate::web) const TURN_TARGET_KINDS: &[&str] = &[
    miyu_core::state::USER_SESSION_KIND,
    miyu_core::state::ASK_SESSION_KIND,
    miyu_core::state::VOICE_SESSION_KIND,
    miyu_core::state::SUBAGENT_SESSION_KIND,
];

/// 可以「看」的会话:主会话 + 子代理会话(`GetSessionState`、回放)。
pub(in crate::web) const VISITABLE_KINDS: &[&str] = &[
    miyu_core::state::USER_SESSION_KIND,
    miyu_core::state::SUBAGENT_SESSION_KIND,
];

/// Most recently updated other user session, or a fresh default session when
/// none is left.
pub(in crate::web) fn fallback_session_id(
    state: &DaemonState,
    exclude: &str,
) -> std::result::Result<String, String> {
    let persona = active_persona_scope(state);
    // 全局指针只在管理员名下的会话里挪,不能落到成员的会话上。
    let sessions = state
        .state_store
        .list_local_sessions_for_owner(&persona, "")
        .map_err(|error| safe_error_message(&error))?;
    if let Some(overview) = sessions
        .iter()
        .find(|overview| overview.record.session_id != exclude)
    {
        return Ok(overview.record.session_id.clone());
    }
    let record = state
        .state_store
        .create_session(
            &persona,
            t("Terminal session", "终端集成会话"),
            "user",
            None,
        )
        .map_err(|error| safe_error_message(&error))?;
    state.events.publish(
        "session.created",
        json!({ "session_id": record.session_id, "name": record.name }),
    );
    Ok(record.session_id)
}

/// 普通人格 + dev 保留人格的本地会话合并,按更新时间排。WebUI 侧栏与
/// `miyu session` 管理面共用:mode 字段(session_record_json)区分分组。
pub(in crate::web) fn sessions_with_dev(
    store: &StateStore,
    persona: &str,
    owner: &str,
) -> anyhow::Result<Vec<miyu_core::state::SessionOverview>> {
    // 成员(owner 非空)名下不分人格:他的会话可能挂在自己的私有人格上。
    let mut rows = if owner.is_empty() {
        store.list_local_sessions_for_owner(persona, owner)?
    } else {
        store.list_owner_sessions(owner)?
    };
    if owner.is_empty() && persona != miyu_core::state::DEV_PERSONA {
        rows.extend(store.list_local_sessions_for_owner(miyu_core::state::DEV_PERSONA, owner)?);
    }
    // 手动排序键优先(v28,越小越靠前);同键退回最近活跃。
    rows.sort_by(|a, b| {
        a.record
            .sort_key
            .cmp(&b.record.sort_key)
            .then_with(|| b.record.updated_at.cmp(&a.record.updated_at))
    });
    Ok(rows)
}

/// 会话模式由人格推导（创建时定死）。
///
/// 单独一个函数是因为它有两个发布口——REST 的会话对象和 `session.created`
/// 事件——而前端两条路都要用它分组。之前只有 REST 那份带上了，事件那份漏了，
/// 结果新建的 dev 会话在刷新之前一直显示在「普通模式」组里。
pub(in crate::web) fn session_mode_label(record: &miyu_core::state::SessionRecord) -> &'static str {
    if record.persona == miyu_core::state::DEV_PERSONA {
        "dev"
    } else {
        "normal"
    }
}

pub(in crate::web) fn session_record_json(record: &miyu_core::state::SessionRecord) -> Value {
    json!({
        "session_id": record.session_id,
        "name": record.name,
        "kind": record.kind,
        "sandbox": record.sandbox,
        "sandbox_read_all": record.sandbox_read_all,
        "sandbox_readonly": record.sandbox_readonly,
        "created_at": record.created_at,
        "updated_at": record.updated_at,
        "mode": session_mode_label(record),
    })
}

pub(in crate::web) fn session_overview_json(
    overview: &miyu_core::state::SessionOverview,
    current: &str,
) -> Value {
    let mut value = session_record_json(&overview.record);
    value["turn_count"] = json!(overview.turn_count);
    value["last_user_content"] = json!(overview.last_user_content);
    value["is_current"] = json!(overview.record.session_id == current);
    value
}

/// Resolves an optional turn-target session id: validates existence and that
/// it is a user or one-shot session; `None` falls back to the global current
/// session.
/// 会话模式创建时定死:dev 人格(DEV_PERSONA)会话永远 Dev,其余永远
/// Normal——客户端传什么都不构成中途切换路径。
/// 「终端集成会话默认模式」（`config.terminal_session_mode`）。
///
/// 终端集成车道 = `current_session` 指针指着的那条会话（shellhook / 裸 `miyu "…"`
/// 落进去的地方），**不是**固定的 `default`——换人格会把指针挪到新人格名下的会话
/// 上。模式钉在会话人格上，所以这个开关做两件事：
/// 1. 「终端集成会话」（`DEFAULT_SESSION_ID`）本身在 激活人格 ↔ dev 之间掰：dev 就
///    挂到保留人格 dev，normal 就换回激活人格。归别的人格（有历史）的不抢。
/// 2. 指针指着的会话不是这条车道该有的模式时，指到「终端集成会话」上；它也用不了
///    （归了别的人格）就退到原来的自举——车道人格名下最近的会话，没有就新建。
///
/// 第一版只掰 `default` 不动指针，用户实测「改了没生效」：指针停在另一条普通会话
/// 上，shell 提示符敲的话进的是那条（09-18）。daemon 启动和重载配置各对一次；
/// 启动时它顶替了原来按激活人格的 `ensure_local_current_session`——那一步会把
/// dev 人格的终端会话当「不可用」从指针上撵走。
pub(in crate::web) fn apply_terminal_session_mode(
    config: &AppConfig,
    store: &StateStore,
) -> Result<()> {
    let active = config.active_persona_scope();
    let dev = miyu_core::state::DEV_PERSONA;
    let lane = if config.terminal_session_is_dev() {
        dev.to_string()
    } else {
        active.clone()
    };
    let terminal = miyu_core::state::DEFAULT_SESSION_ID;
    if let Some(record) = store.session_record(terminal)? {
        let ours = record.persona == active || record.persona == dev;
        if ours && record.persona != lane {
            store.set_session_persona(terminal, &lane)?;
        }
    }
    let current = store.session_id();
    if is_available_local_session(store, &current, &lane)? {
        return Ok(());
    }
    if is_available_local_session(store, terminal, &lane)? {
        return store.switch_session(terminal);
    }
    ensure_local_current_session(store, &lane)
}

pub(in crate::web) fn turn_mode_for_session(
    store: &StateStore,
    session_id: &str,
    requested: PersonaLane,
) -> PersonaLane {
    match store.session_record(session_id) {
        Ok(Some(record)) if record.persona == miyu_core::state::DEV_PERSONA => PersonaLane::Dev,
        _ => {
            if requested == PersonaLane::Dev {
                tracing::debug!(%session_id, "client asked for dev mode on a non-dev session; forcing normal");
            }
            PersonaLane::Active
        }
    }
}

/// `owner` 同 [`resolve_local_session_ref_with_kinds`]:HTTP 路径传登录者的
/// 归属键,IPC 传 None。没给会话 id 时,管理员/IPC 落到全局当前会话,成员落到
/// 自己名下最近的一条(没有就建)。
pub(in crate::web) fn resolve_turn_session(
    state: &DaemonState,
    owner: Option<&str>,
    session_id: Option<String>,
) -> std::result::Result<Arc<str>, String> {
    match session_id {
        None => match owner {
            Some(owner) if !owner.is_empty() => Ok(member_current_session(state, owner)?.into()),
            _ => Ok(state.state_store.session_id()),
        },
        Some(session_id) => {
            let record = resolve_local_session_ref_with_kinds(
                state,
                &ipc::SessionRef::Id { id: session_id },
                TURN_TARGET_KINDS,
                owner,
            )?;
            Ok(record.session_id.into())
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(in crate::web) fn session_for_persona(
    state_store: &StateStore,
    manager: &Arc<Mutex<ManagerState>>,
    persona: &str,
) -> Result<String> {
    if let Some(session_id) = state_store.persona_current_session(persona)? {
        if is_available_local_session(state_store, &session_id, persona)? {
            return Ok(session_id);
        }
    }
    let remembered = manager
        .lock()
        .unwrap()
        .persona_session_ids
        .get(persona)
        .cloned();
    if let Some(session_id) = remembered {
        if is_available_local_session(state_store, &session_id, persona)? {
            return Ok(session_id);
        }
    }
    if let Some(overview) = state_store
        .list_local_sessions_for_owner(persona, "")?
        .into_iter()
        .next()
    {
        return Ok(overview.record.session_id);
    }
    Ok(state_store
        .create_session(persona, "", "user", None)?
        .session_id)
}

/// Auto-names a still-unnamed session from its first prompt once a turn has
/// run in it. Explicit names (given at creation or via rename) are never
/// overwritten.
pub(in crate::web) fn maybe_auto_name_session(
    state_store: &StateStore,
    events: &EventHub,
    seed: &str,
) -> Option<String> {
    // daemon 合成的轮（后台汇报、目标续轮、跨会话消息）不拿来起名：会话会被叫成
    // 「<background-job-report>…」（09-23）。等用户自己说第一句话再起。
    if miyu_core::state::is_synthetic_user_content(seed) {
        return None;
    }
    let session_id = state_store.session_id();
    let record = state_store.session_record(&session_id).ok().flatten()?;
    if !record.name.trim().is_empty() {
        return None;
    }
    let title = session_title_from_prompt(seed);
    if title.is_empty() {
        return None;
    }
    if state_store
        .rename_session(&record.session_id, &title)
        .is_ok()
    {
        events.publish(
            "session.renamed",
            json!({ "session_id": record.session_id, "name": title }),
        );
        return Some(title);
    }
    None
}

pub(in crate::web) fn session_title_from_prompt(prompt: &str) -> String {
    let cleaned = prompt
        .trim()
        .lines()
        .next()
        .unwrap_or("")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let mut title: String = cleaned.chars().take(20).collect();
    if cleaned.chars().count() > 20 {
        title.push('…');
    }
    title
}

/// 把目标会话钉的模型池套到 `config` 上。回合路与压缩路共用同一条规则，
/// 否则摘要会被路由到全局池里的另一家供应商，拿不到该会话的前缀缓存。
///
/// 钉的模型可能已经被供应商下架或改名。**一条都对不上时退回全局池并把这份覆盖
/// 清掉**——留着它的话 `from_config` 直接报「没有可用端点」，这条会话连打开都打
/// 不开（09-18 真机：某会话钉着 `opencodego / union-alpha`，供应商清单里早没了，
/// `miyu` 整个进不去，客户端只看到一句「invalid admin response」）。部分失效的
/// 只把失效那几条筛掉，剩下的照用。
///
/// 客户端/直连侧的同一道守卫在 `cli::model_cmds::apply_session_model_override`
/// （08-28 就加了），daemon 侧一直漏着。
///
/// 返回被清掉的那几条（`"供应商 / 模型"`），给调用方拿去告诉用户；没清就是空的。
pub(in crate::web) fn apply_session_model_override_to(
    config: &mut AppConfig,
    store: &StateStore,
    session_id: &str,
) -> Vec<String> {
    match store.session_model_override(session_id) {
        Ok(Some(models)) => {
            let stale = models
                .iter()
                .map(|active| format!("{} / {}", active.provider_id.trim(), active.model.trim()))
                .collect::<Vec<_>>();
            match config.usable_model_override(models) {
                Some(usable) => {
                    config.active_provider_models = Some(usable);
                    Vec::new()
                }
                None => {
                    tracing::warn!(
                        session_id,
                        models = %stale.join(", "),
                        "{}",
                        t(
                            "the session model override points at models the providers no longer list; falling back to the global pool",
                            "会话钉的模型已不在供应商清单里,退回全局模型池"
                        )
                    );
                    // 清掉而不是只让这一轮退回:留着的话每一轮都要重新踩一次,
                    // `/models` 里也还显示着一个用不了的池。
                    if let Err(error) = store.set_session_model_override(session_id, None) {
                        tracing::warn!(
                            error = %error,
                            session_id,
                            "{}",
                            t(
                                "clearing the stale session model override failed",
                                "清除失效的会话模型覆盖失败"
                            )
                        );
                    }
                    stale
                }
            }
        }
        Ok(None) => Vec::new(),
        Err(error) => {
            tracing::warn!(
                error = %error,
                session_id,
                "{}",
                t(
                    "loading the session model override failed",
                    "读取会话模型覆盖失败"
                )
            );
            Vec::new()
        }
    }
}

/// 输入框那行 `/goal …` 提示要的状态。完成的目标不下发——没什么可提示的。
pub(in crate::web) fn goal_hint(store: &StateStore, session_id: &str) -> Option<ipc::GoalHint> {
    use miyu_core::state::GoalPhase;
    let goal = store.goal(session_id).ok().flatten()?;
    if goal.phase == GoalPhase::Complete {
        return None;
    }
    Some(ipc::GoalHint {
        phase: goal.phase.as_str().to_string(),
        armed: miyu_engine::tools::goal::is_armed(session_id),
        awaiting: miyu_engine::tools::goal::is_awaiting_human(session_id),
        rounds: goal.rounds_started,
        // `updated_at` 是这个状态最后一次变动的时刻：起新一轮、被暂停、被卡住
        // 都会推它，正好就是「这个状态持续了多久」的起点。
        since_unix: chrono::DateTime::parse_from_rfc3339(&goal.updated_at)
            .map(|time| time.timestamp())
            .unwrap_or_default(),
    })
}

pub(in crate::web) fn build_session_agent(
    config: &AppConfig,
    paths: &MiyuPaths,
    state: &StateStore,
    mode: PersonaLane,
) -> Result<Agent> {
    miyu_base::models_cache::ensure_active_metadata(paths, config);
    let client = OpenAiCompatibleClient::from_config(config, paths)?;
    let registry = build_tool_registry(config, paths, mode, true)?;
    Ok(
        Agent::new(config.clone(), paths, state.clone(), client, registry, mode)?
            .with_headless_pacing(),
    )
}

pub(in crate::web) fn session_state(
    manager: &Arc<Mutex<ManagerState>>,
    state_store: &StateStore,
) -> Result<ipc::SessionState> {
    let context = manager.lock().unwrap().context;
    let session_id = state_store.session_id();
    let record = state_store.session_record(&session_id)?;
    // 会话跑在哪个模式上：REPL 切到另一侧的会话时靠它把车道跟过去。
    let mode = record
        .as_ref()
        .map(session_mode_label)
        .unwrap_or("normal")
        .to_string();
    Ok(ipc::SessionState {
        mode,
        context_tokens: context.tokens,
        context_window: context.window,
        context_window_assumed: context.window_assumed,
        cumulative_tokens: context.cumulative_tokens,
        cumulative_prompt_tokens: context.cumulative_prompt_tokens,
        cumulative_cache_read_tokens: context.cumulative_cache_read_tokens,
        session_id: session_id.to_string(),
        session_name: record
            .as_ref()
            .map(|record| record.name.clone())
            .unwrap_or_default(),
        sandbox_readonly: record
            .as_ref()
            .is_some_and(|record| record.sandbox_readonly),
        sandbox: record.and_then(|record| record.sandbox),
        sandbox_default: false,
        sandbox_writable: Vec::new(),
        sandbox_readable: Vec::new(),
    })
}

/// Global admin reservation (config/model changes): requires that no turn is
/// running in any session.
pub(in crate::web) fn reserve_admin(
    manager: &Arc<Mutex<ManagerState>>,
) -> std::result::Result<(), ApiError> {
    let mut manager = manager.lock().unwrap();
    if !manager.active_runs.is_empty() || manager.admin_busy {
        return Err(ApiError::new(StatusCode::CONFLICT, ipc::ADMIN_BUSY_MESSAGE));
    }
    manager.admin_busy = true;
    manager.admin_session = None;
    Ok(())
}

/// Per-session admin reservation (reset/undo/pop/compact/delete/archive):
/// only the target session must be idle; turns in other sessions keep
/// running.
pub(in crate::web) fn reserve_admin_for_session(
    manager: &Arc<Mutex<ManagerState>>,
    session_id: &str,
) -> std::result::Result<(), ApiError> {
    let mut manager = manager.lock().unwrap();
    if manager.admin_busy || manager.session_has_runs(session_id) {
        return Err(ApiError::new(StatusCode::CONFLICT, ipc::ADMIN_BUSY_MESSAGE));
    }
    manager.admin_busy = true;
    // 预约限定到这个会话:压缩/pop/undo 重写的是它自己的消息数组,别的
    // 会话该照常开回合。以前这里只置全局位,压一个会话等于停掉整台机器。
    manager.admin_session = Some(session_id.to_string());
    Ok(())
}

/// Light admin reservation (session/model updates): serializes against other
/// admin operations but is allowed while turns are running.
pub(in crate::web) fn reserve_admin_light(
    manager: &Arc<Mutex<ManagerState>>,
) -> std::result::Result<(), ApiError> {
    let mut manager = manager.lock().unwrap();
    if manager.admin_busy {
        return Err(ApiError::new(StatusCode::CONFLICT, ipc::ADMIN_BUSY_MESSAGE));
    }
    manager.admin_busy = true;
    manager.admin_session = None;
    Ok(())
}
