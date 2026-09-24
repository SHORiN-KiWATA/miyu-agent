//! 子代理会话化的宿主侧(09-18)。
//!
//! 工具层的 `subagent` 只认 `miyu_base::host_ports::SubagentHostPort` 这条窄接口;
//! 这里是它在 daemon 里的实现:建子会话(挂在父会话下、归属跟父)、经 actor 起
//! 普通回合、把子会话的事件折回老标记喂给父回合的进度通道、等「任务终态」。
//!
//! 「任务完成」的判据只有一处——[`spawn_subagent_supervisor`]:订阅事件流,子会话
//! 一轮结束时看它名下还有没有后台任务(命令、后台孙代理的镜像任务)或未完成的
//! 子会话,有就 waiting、没有才进终态并叫醒等着的父回合。认的是终态**事件**而不是
//! `runs_changed`:`finish_run` 先于 `publish_completed` 触发,靠后者会读到空的
//! `active_runs` 却拿不到正文。
//!
//! 前台等待的 future 被 drop(父回合被用户停掉)时,[`ChildRunGuard`] 顺手取消子会话
//! 的活动回合——子的 chat future 被 drop 又会 drop 它等孙代理的那条,级联自动递归。
//! 工具层没有取消令牌,这是唯一的路。后台子代理不装这个守卫:它包在后台任务注册表
//! 的镜像任务里,`job(action=stop)` 中止那条任务时同样走 Drop。

use crate::web::*;
use miyu_base::host_ports::{
    ChildOutcome, ChildTaskResult, ContinueChildRequest, SpawnChildRequest, SubagentHostPort,
    SubagentProgressSink, WatchChildRequest,
};
use miyu_core::state::{SubagentTaskState, SUBAGENT_SESSION_KIND};
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::Instant;
use tokio::sync::oneshot;

/// 子代理树深度上限(用户 09-18 拍板写死):0 主会话、1 子代理、2 孙代理。
const MAX_SUBAGENT_DEPTH: i64 = 2;

/// 内层事件里单条输出最多带多少字节——与老循环的 `clip_detail` 同口径。
const MAX_DETAIL_BYTES: usize = 8 * 1024;

/// 追话时子会话的回合可能刚起、`queue_target` 还没就位:等它最多这么久。
const FOLLOWUP_SETTLE: Duration = Duration::from_secs(3);

// ── 等待者登记 ──
fn waiters() -> &'static Mutex<HashMap<String, oneshot::Sender<SubagentTaskState>>> {
    static WAITERS: std::sync::OnceLock<
        Mutex<HashMap<String, oneshot::Sender<SubagentTaskState>>>,
    > = std::sync::OnceLock::new();
    WAITERS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn resolve_waiter(session_id: &str, state: SubagentTaskState) {
    if let Some(sender) = waiters().lock().unwrap().remove(session_id) {
        let _ = sender.send(state);
    }
}

pub(in crate::web) struct SubagentHost {
    pub(in crate::web) state: DaemonState,
}

impl SubagentHostPort for SubagentHost {
    fn is_running(&self, child_session: &str) -> bool {
        self.state
            .manager
            .lock()
            .unwrap()
            .session_has_runs(child_session)
    }

    fn spawn(
        &self,
        request: SpawnChildRequest,
    ) -> futures_util::future::BoxFuture<'static, Result<ChildOutcome>> {
        let state = self.state.clone();
        Box::pin(async move { spawn_child(state, request).await })
    }

    fn continue_child(
        &self,
        request: ContinueChildRequest,
    ) -> futures_util::future::BoxFuture<'static, Result<ChildOutcome>> {
        let state = self.state.clone();
        Box::pin(async move { continue_child(state, request).await })
    }

    fn watch_child(
        &self,
        request: WatchChildRequest,
    ) -> futures_util::future::BoxFuture<'static, Result<ChildOutcome>> {
        let state = self.state.clone();
        Box::pin(async move { watch_child(state, request).await })
    }
}

pub(in crate::web) fn install_subagent_host(state: &DaemonState) {
    crate::runtime::install_subagent_port(Arc::new(SubagentHost {
        state: state.clone(),
    }));
}

/// 子会话的名字:描述截到 40 个字,列表里读得出是哪一个。
fn child_name(description: &str) -> String {
    let mut name: String = description.trim().chars().take(40).collect();
    if name.chars().count() < description.trim().chars().count() {
        name.push('…');
    }
    if name.is_empty() {
        name = t("subagent", "子代理").to_string();
    }
    name
}

async fn spawn_child(state: DaemonState, request: SpawnChildRequest) -> Result<ChildOutcome> {
    let parent_store = state.stores.for_session(&request.parent_session);
    let parent = parent_store
        .session_record(&request.parent_session)?
        .with_context(|| format!("parent session not found: {}", request.parent_session))?;
    let depth = parent.depth + 1;
    if depth > MAX_SUBAGENT_DEPTH {
        bail!(
            "subagent depth limit reached (depth {} cannot spawn further subagents)",
            parent.depth
        );
    }
    // dev 子代理 = dev 人格本身(车道由人格推);普通子代理跟父的人格,资源作用域
    // (脚本/技能白名单)照父的来。
    let persona = if request.dev {
        miyu_core::state::DEV_PERSONA.to_string()
    } else {
        parent.persona.clone()
    };
    // 子会话建在**父会话所属的库**里,归属跟父:成员开的子代理落成员库、记成员的账,
    // 管理员库里找不到它才对(老审计行落管理员库、owner 空,成员场景全错)。
    let child = parent_store.create_subagent_session(
        &persona,
        &child_name(&request.description),
        &parent.session_id,
        &parent.owner,
        depth,
        request.spawned_by_turn.as_deref(),
        request.background,
    )?;
    state
        .stores
        .note_session_owner(&child.session_id, &parent.owner);
    // 沙盒跟父:`/sandbox` 绑了根的会话,它开的子代理同一把锁;「明确不要沙盒」
    // 与只读模式也一起跟(09-23)——父会话按了只读,子代理不能反倒写得了。
    parent_store.copy_session_sandbox(&parent, &child.session_id)?;
    // 档位 → 会话级模型池覆盖:回合路按会话覆盖选池,子会话就吃这个池;配置里没配
    // 这一档就跟父的全局池(与 `from_tier` 的退回口径一致)。
    let config = state.manager.lock().unwrap().config.clone();
    let pool = config.tier_choices(request.tier);
    if !pool.is_empty() {
        let models: Vec<miyu_base::config::ActiveProviderModelConfig> = pool
            .iter()
            .map(|choice| miyu_base::config::ActiveProviderModelConfig {
                provider_id: choice.provider_id.clone(),
                model: choice.model.clone(),
            })
            .collect();
        parent_store.set_session_model_override(&child.session_id, Some(&models))?;
    }
    tracing::info!(
        parent = %parent.session_id,
        child = %child.session_id,
        depth,
        dev = request.dev,
        background = request.background,
        "subagent session spawned"
    );
    // 子会话 id 第一条就报出去:父回合的标记流据它落 `child_session_id`,后台镜像
    // 任务据它登记 job_id → 子会话。
    (request.progress)(format!(
        "{}{}",
        miyu_engine::tools::SUBAGENT_SESSION_MARKER,
        child.session_id
    ));
    run_child_turn(
        state,
        child.session_id,
        parent.session_id,
        request.prompt,
        request.workdir,
        request.progress,
    )
    .await
}

/// 只认这个父会话自己的子级:别的会话的子代理、主会话本身都不能拿来续或等。
fn own_child(
    store: &StateStore,
    parent: &str,
    child: &str,
) -> Result<miyu_core::state::SessionRecord> {
    let record = store
        .session_record(child)?
        .with_context(|| format!("subagent session not found: {child}"))?;
    if record.kind != SUBAGENT_SESSION_KIND || record.parent_session_id.as_deref() != Some(parent) {
        bail!("session {child} is not a subagent of this conversation");
    }
    Ok(record)
}

async fn continue_child(state: DaemonState, request: ContinueChildRequest) -> Result<ChildOutcome> {
    let store = state.stores.for_session(&request.child_session);
    let child = own_child(&store, &request.parent_session, &request.child_session)?;
    (request.progress)(format!(
        "{}{}",
        miyu_engine::tools::SUBAGENT_SESSION_MARKER,
        child.session_id
    ));
    // 跑着:排进它当前那一轮,立刻返回。
    if let Some(session_id) =
        queue_followup_into_child(&state, &child.session_id, &request.message).await?
    {
        return Ok(ChildOutcome::Queued { session_id });
    }
    run_child_turn(
        state,
        child.session_id,
        request.parent_session,
        request.message,
        request.workdir,
        request.progress,
    )
    .await
}

/// 只等不跑(09-24 断点续跑):子会话自己的回合已经结束,在等被另外接回的孙代理;孙代理
/// 跑完叫醒它、它收尾之后,监督器判出终态交到这里。
async fn watch_child(state: DaemonState, request: WatchChildRequest) -> Result<ChildOutcome> {
    let store = state.stores.for_session(&request.child_session);
    let child = own_child(&store, &request.parent_session, &request.child_session)?.session_id;
    (request.progress)(format!(
        "{}{}",
        miyu_engine::tools::SUBAGENT_SESSION_MARKER,
        child
    ));
    store.set_session_task_state(&child, SubagentTaskState::Waiting)?;
    let (sender, receiver) = oneshot::channel();
    waiters().lock().unwrap().insert(child.clone(), sender);
    let mut guard = ChildRunGuard {
        state: state.clone(),
        child: child.clone(),
        armed: true,
    };
    let events_after = state.events.latest_id();
    // 名下已经没有要等的(孙代理没接回来):当场收成中断,别让父会话干等一个不会来的结果。
    let busy = state.manager.lock().unwrap().session_has_runs(&child);
    let pending_jobs = tools::jobs::running_job_count_for_session(&child);
    let pending_children = store.pending_child_sessions(&child).unwrap_or(0);
    if !busy && pending_jobs == 0 && pending_children == 0 {
        let _ = store.set_session_task_state(&child, SubagentTaskState::Interrupted);
        resolve_waiter(&child, SubagentTaskState::Interrupted);
    }
    let relay = tokio::spawn(relay_child_events(
        state.clone(),
        child.clone(),
        events_after,
        request.progress,
    ));
    let task_state = receiver.await.unwrap_or(SubagentTaskState::Interrupted);
    guard.disarm();
    relay.abort();
    Ok(ChildOutcome::Finished(child_result(
        &state, &child, task_state,
    )))
}

/// 子会话有活动回合时把话排成 follow-up。回合刚起、`queue_target` 还没就位就等一小会;
/// 没有活动回合返回 `Ok(None)`。
async fn queue_followup_into_child(
    state: &DaemonState,
    child: &str,
    message: &str,
) -> Result<Option<String>> {
    let deadline = tokio::time::Instant::now() + FOLLOWUP_SETTLE;
    loop {
        let target = {
            let manager = state.manager.lock().unwrap();
            manager
                .active_runs
                .iter()
                .find(|(_, info)| &*info.session_id == child)
                .map(|(run_id, info)| {
                    (
                        run_id.clone(),
                        info.queue_target.clone(),
                        info.audience,
                        info.session_id.clone(),
                    )
                })
        };
        let Some((run_id, queue_target, audience, session_id)) = target else {
            return Ok(None);
        };
        if let Some(target) = queue_target {
            enqueue_turn_update(
                state,
                TurnUpdateRequest {
                    run_id,
                    turn_id: target.turn_id,
                    session_id: Some(session_id),
                    audience,
                    content: message.to_string(),
                    display_content: message.to_string(),
                    attachments: Vec::new(),
                    uploaded_attachment_ids: Vec::new(),
                    mode: TurnUpdateMode::Followup,
                },
            )?;
            return Ok(Some(child.to_string()));
        }
        if tokio::time::Instant::now() >= deadline {
            bail!("the subagent's turn is still starting; retry in a moment");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// 前台等待的守卫:future 被 drop 时取消子会话的活动回合、撤掉等待者。正常拿到
/// 终态后 `disarm`。
struct ChildRunGuard {
    state: DaemonState,
    child: String,
    armed: bool,
}

impl ChildRunGuard {
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ChildRunGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        waiters().lock().unwrap().remove(&self.child);
        let cancelled = {
            let manager = self.state.manager.lock().unwrap();
            let mut count = 0usize;
            for run in manager.active_runs.values() {
                if &*run.session_id == self.child.as_str() {
                    run.request_cancel();
                    count += 1;
                }
            }
            count
        };
        tracing::info!(
            child = %self.child,
            cancelled,
            "subagent wait dropped; child runs cancelled"
        );
        // 没有活动回合(在等自己的后台任务)时监督器收不到取消事件,这里直接标中断。
        if cancelled == 0 {
            let store = self.state.stores.for_session(&self.child);
            let _ = store.set_session_task_state(&self.child, SubagentTaskState::Interrupted);
        }
    }
}

/// 在子会话里起一轮并等任务终态。`prompt` 是这一轮的用户消息(父派的任务或追话)。
async fn run_child_turn(
    state: DaemonState,
    child: String,
    parent: String,
    prompt: String,
    workdir: Option<std::path::PathBuf>,
    progress: SubagentProgressSink,
) -> Result<ChildOutcome> {
    let store = state.stores.for_session(&child);
    store.set_session_task_state(&child, SubagentTaskState::Running)?;
    // 等待者先登记,再入队:终态事件不可能跑在登记前面。
    let (sender, receiver) = oneshot::channel();
    waiters().lock().unwrap().insert(child.clone(), sender);
    let mut guard = ChildRunGuard {
        state: state.clone(),
        child: child.clone(),
        armed: true,
    };
    // 订阅起点在入队前取:turn.started 起一帧不漏。
    let events_after = state.events.latest_id();
    let run_id = random_id("run", 18);
    let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
    let mode = turn_mode_for_session(&store, &child, PersonaLane::Active);
    let origin = miyu_base::workspace::TurnOrigin::Subagent {
        parent_session: parent.clone(),
    };
    {
        let mut manager = state.manager.lock().unwrap();
        if manager.admin_blocks_session(&child) {
            bail!("{}", ipc::ADMIN_BUSY_MESSAGE);
        }
        manager.active_runs.insert(
            run_id.clone(),
            RunInfo {
                session_id: child.clone().into(),
                mode,
                audience: PromptAudience::Owner,
                cancel: cancel_tx,
                turn_id: None,
                queue_target: None,
                supersede: Arc::new(miyu_engine::agent::TurnSupersedeSignal::default()),
                platform_followup: None,
                operation: RunOperation::Create,
                job_wake: false,
                turn_origin: origin.clone(),
                job_wake_label: None,
                first_event_id: None,
            },
        );
    }
    if state
        .actor_tx
        .send(ActorCommand::StartTurn {
            run_id: run_id.clone(),
            session_id: child.clone().into(),
            content: prompt.clone(),
            display_content: prompt,
            attachment_run_id: None,
            mode,
            images: Vec::new(),
            cwd: workdir,
            origin_tty: None,
            audience: PromptAudience::Owner,
            profile: None,
            overrides: None,
            cancel: cancel_rx,
            turn_origin: Box::new(origin),
        })
        .is_err()
    {
        finish_run(&state.manager, &run_id, None);
        bail!("Miyu core worker is unavailable");
    }
    let relay = tokio::spawn(relay_child_events(
        state.clone(),
        child.clone(),
        events_after,
        progress,
    ));
    let task_state = receiver.await.unwrap_or(SubagentTaskState::Interrupted);
    guard.disarm();
    relay.abort();
    Ok(ChildOutcome::Finished(child_result(
        &state, &child, task_state,
    )))
}

/// 子会话的交付物:最后一轮正文 + 轮数 + 整棵树的用量。
fn child_result(
    state: &DaemonState,
    child: &str,
    task_state: SubagentTaskState,
) -> ChildTaskResult {
    let store = state.stores.for_session(child).pinned(child);
    let last = store
        .session_replay(1)
        .ok()
        .and_then(|turns| turns.into_iter().last());
    let turns = store
        .load_visible_turns()
        .map(|turns| turns.len() as i64)
        .unwrap_or(0);
    let total_tokens = store
        .session_cumulative_token_totals()
        .map(|tokens| tokens.total)
        .unwrap_or(0);
    ChildTaskResult {
        session_id: child.to_string(),
        state: task_state.as_str().to_string(),
        final_text: last
            .as_ref()
            .map(|turn| turn.assistant_content.clone())
            .unwrap_or_default(),
        turns,
        total_tokens,
        provider_id: last
            .as_ref()
            .and_then(|turn| turn.assistant_provider_id.clone()),
        model: last.and_then(|turn| turn.assistant_model.clone()),
    }
}

fn clip_detail(text: &str) -> String {
    if text.len() <= MAX_DETAIL_BYTES {
        return text.to_string();
    }
    let mut end = MAX_DETAIL_BYTES;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n…", &text[..end])
}

fn field<'a>(data: &'a Value, key: &str) -> &'a str {
    data.get(key).and_then(Value::as_str).unwrap_or_default()
}

/// 把子会话的回合事件折成老循环那套 `__subagent_*` / `__subtool_*` 标记喂给父回合。
/// 渲染层(全屏 TUI 浮层、WebUI 子过程时间线、后台任务日志桥)一字不改照旧能画;
/// 前端改成「切进子会话看」之后这条中继只剩状态行要的那几样。
///
/// 跟的是**会话**不是单个 run:子会话被后台任务唤醒会再起一轮,`run.started` 带
/// 会话 id,据它把新 run 认进来。
async fn relay_child_events(
    state: DaemonState,
    child: String,
    after: u64,
    progress: SubagentProgressSink,
) {
    let mut subscription = state.events.subscribe_after(after);
    let mut runs: HashSet<String> = HashSet::new();
    let mut reasoning_since: Option<Instant> = None;
    let mut tool_since: HashMap<String, Instant> = HashMap::new();
    let mut tool_calls: u64 = 0;
    // 最近一次轮用量(会话累计)与是否估算:工具一起跑就报一次量,让面板抬头上的
    // 「工具调用 N 次」当场涨,不等下一轮用量事件(老循环每步都报;TUI 走查 item06)。
    let mut last_total: u64 = 0;
    let mut last_estimated = false;
    let mut last_id = after;
    let metric = |tool_calls: u64, total: u64, estimated: bool, progress: &SubagentProgressSink| {
        let tokens = miyu_engine::tools::subagent_runner::format_token_count(total, estimated);
        let text = if miyu_base::i18n::is_zh() {
            format!("工具调用 {tool_calls} 次　消耗词元 {tokens}")
        } else {
            format!("tool calls: {tool_calls}　token cost: {tokens}")
        };
        progress(format!("__subagent_metric__{tokens}\t{total}\t{text}"));
    };
    let seal = |since: &mut Option<Instant>, progress: &SubagentProgressSink| {
        if let Some(started) = since.take() {
            progress(format!(
                "{}{}",
                miyu_engine::tools::subagent::protocol::REASONING_DONE_MARKER,
                started.elapsed().as_millis()
            ));
        }
    };
    loop {
        let record = if let Some(record) = subscription.pending.pop_front() {
            record
        } else {
            match subscription.receiver.recv().await {
                Ok(record) => record,
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    subscription.pending = state.events.replay_after(last_id);
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        };
        if record.kind == "resync_required" {
            continue;
        }
        last_id = record.id;
        let Ok(data) = serde_json::from_str::<Value>(&record.data) else {
            continue;
        };
        if record.kind == "run.started" {
            if field(&data, "session_id") == child {
                runs.insert(field(&data, "run_id").to_string());
            }
            continue;
        }
        if !runs.contains(field(&data, "run_id")) {
            continue;
        }
        match record.kind.as_str() {
            "reasoning.delta" => {
                let delta = field(&data, "delta");
                if !delta.is_empty() {
                    reasoning_since.get_or_insert_with(Instant::now);
                    progress(format!("__subagent_reasoning__{delta}"));
                }
            }
            "assistant.delta" => {
                let delta = field(&data, "delta");
                if !delta.is_empty() {
                    seal(&mut reasoning_since, &progress);
                    progress(format!("__subagent_content__{delta}"));
                }
            }
            "tool.preparing" => {
                let name = field(&data, "name");
                if miyu_engine::tools::preparing_phase(name).is_some() {
                    seal(&mut reasoning_since, &progress);
                    progress(format!("__subtool_preparing__{name}"));
                }
            }
            "tool.started" => {
                seal(&mut reasoning_since, &progress);
                let name = field(&data, "name");
                tool_since.insert(field(&data, "tool_id").to_string(), Instant::now());
                tool_calls += 1;
                progress(format!(
                    "__subtool_call__{}",
                    json!({
                        "name": name,
                        "display": field(&data, "display_name"),
                        "args": clip_detail(field(&data, "arguments")),
                    })
                ));
                metric(tool_calls, last_total, last_estimated, &progress);
            }
            "tool.finished" => {
                let name = field(&data, "name");
                let millis = tool_since
                    .remove(field(&data, "tool_id"))
                    .map(|since| since.elapsed().as_millis());
                progress(format!(
                    "__subtool_result__{}",
                    json!({
                        "name": name,
                        "display": field(&data, "display_name"),
                        "args": "",
                        "ok": data.get("ok").and_then(Value::as_bool).unwrap_or(false),
                        "ms": millis,
                        "output": clip_detail(field(&data, "output")),
                    })
                ));
            }
            "chat.round_usage" => {
                // 会话累计(含这一轮之前的轮):状态行上要的正是「这个子代理一共烧了多少」。
                let total = data
                    .get("cumulative_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                let estimated = data
                    .get("estimated")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                last_total = total;
                last_estimated = estimated;
                metric(tool_calls, total, estimated, &progress);
            }
            "run.completed" | "run.failed" | "run.cancelled" => {
                seal(&mut reasoning_since, &progress);
                runs.remove(field(&data, "run_id"));
            }
            _ => {}
        }
    }
}

/// 监督器:子会话每轮结束时判「任务完没完」,并把终态交给等着的父回合。
///
/// 后台命令完成唤醒子会话走现有 `job_wake`(任务归属会话就是子会话),这里不用管
/// 「怎么再跑」,只管「跑完了算不算完」。
pub(in crate::web) fn spawn_subagent_supervisor(state: DaemonState) {
    let mut receiver = state.events.subscribe_live();
    tokio::spawn(async move {
        loop {
            let record = match receiver.recv().await {
                Ok(record) => record,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            };
            if !matches!(
                record.kind.as_str(),
                "run.started" | "run.completed" | "run.failed" | "run.cancelled"
            ) {
                continue;
            }
            let Ok(data) = serde_json::from_str::<Value>(&record.data) else {
                continue;
            };
            let session_id = field(&data, "session_id").to_string();
            if session_id.is_empty() {
                continue;
            }
            let store = state.stores.for_session(&session_id);
            let Ok(Some(session)) = store.session_record(&session_id) else {
                continue;
            };
            if session.kind != SUBAGENT_SESSION_KIND {
                continue;
            }
            if record.kind == "run.started" {
                let _ = store.set_session_task_state(&session_id, SubagentTaskState::Running);
                continue;
            }
            let pending_jobs = tools::jobs::running_job_count_for_session(&session_id);
            let pending_children = store.pending_child_sessions(&session_id).unwrap_or(0);
            let next = match record.kind.as_str() {
                // 用户按的停止(或父被停后级联):不管名下还有什么,都是中断。
                "run.cancelled" => SubagentTaskState::Interrupted,
                _ if pending_jobs > 0 || pending_children > 0 => SubagentTaskState::Waiting,
                "run.failed" => SubagentTaskState::Failed,
                _ => SubagentTaskState::Done,
            };
            let _ = store.set_session_task_state(&session_id, next);
            tracing::info!(
                session = %session_id,
                state = next.as_str(),
                pending_jobs,
                pending_children,
                "subagent turn ended"
            );
            if !next.is_pending() {
                resolve_waiter(&session_id, next);
            }
        }
    });
}

/// 给后台任务总览回填「树根」:子代理/孙代理开的后台任务归它们自己的会话,主会话
/// 的任务条(TUI / WebUI)按 `root_session_id` 把整棵树的任务列出来(用户 09-18:
/// 「没看到呀」——孙代理的后台子代理在主会话里一条都不显示)。沿 `parent_session_id`
/// 往上走到 kind != subagent 为止;主会话自己的任务根就是自己。
pub(in crate::web) fn annotate_job_roots(
    state: &DaemonState,
    jobs: &mut [tools::jobs::JobOverview],
) {
    let mut roots: HashMap<String, Option<String>> = HashMap::new();
    for job in jobs.iter_mut() {
        let Some(session_id) = job.session_id.clone() else {
            continue;
        };
        let root = roots
            .entry(session_id.clone())
            .or_insert_with(|| root_session_of(state, &session_id))
            .clone();
        job.root_session_id = root;
    }
}

fn root_session_of(state: &DaemonState, session_id: &str) -> Option<String> {
    let mut current = session_id.to_string();
    // 深度写死到 2,循环上限留点余量防坏数据成环。
    for _ in 0..4 {
        let record = state
            .stores
            .for_session(&current)
            .session_record(&current)
            .ok()
            .flatten()?;
        if record.kind != SUBAGENT_SESSION_KIND {
            return Some(record.session_id);
        }
        current = record.parent_session_id?;
    }
    None
}

/// reset / 删除会话:沿整棵子代理树停回合、停后台任务、收中转进程、删行(先深后浅),
/// 根会话自己的后台任务也一并停掉。删会话现状只停回合(不停任务、不收进程),这里
/// 连根会话的一起补上。
pub(in crate::web) async fn teardown_subagent_tree(state: &DaemonState, root: &str) {
    let store = state.stores.for_session(root);
    let descendants = store.descendant_session_ids(root).unwrap_or_default();
    for id in descendants.iter().rev() {
        resolve_waiter(id, SubagentTaskState::Cancelled);
        if state.manager.lock().unwrap().session_has_runs(id) {
            stop_session_runs(state, id, Duration::from_secs(5)).await;
        }
        tools::jobs::stop_session_jobs(id).await;
        miyu_core::llm::forget_relay_sessions(id);
        let child_store = state.stores.for_session(id);
        if let Err(error) = child_store.delete_session(id) {
            tracing::warn!(session = %id, error = %error, "failed to delete a subagent session");
        }
        state
            .events
            .publish("session.deleted", json!({ "session_id": id }));
    }
    tools::jobs::stop_session_jobs(root).await;
    if !descendants.is_empty() {
        tracing::info!(
            root = %root,
            removed = descendants.len(),
            "subagent tree torn down"
        );
    }
}
