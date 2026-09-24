//! daemon 重启后把被打断的回合接着跑（09-24 断点续跑，用户定为关键项）。
//!
//! 上一个 daemon 崩溃、被杀或有序关停时，正在跑的回合在库里留着「执行中」。谁先碰到那个
//! 库，谁就把它收成「已中断」并记一笔认领（`conversation_db::restart`）。daemon 起来后在
//! 这里翻出没结案的认领，一律接着跑（用户 09-24：被重启打断的就该接着跑，不设次数上限、
//! 不设时间范围，子代理、`/goal` 续轮、QQ 都一样）：
//! - 会话里投一条续跑消息（`<service-restart attempt="N">`），替它起一轮。模型看到被打断
//!   那一轮的回放（`<interrupted-turn-recovery>` + 流水账，没跑完的工具调用标着被打断，
//!   系统不替它重放）再接着做。
//! - 子代理先接、深的先接：挂回父会话名下起一条后台镜像任务，在子会话里接着跑，跑完照
//!   后台子代理那套把结果送回父会话。父会话的续跑消息列出它们的新任务号，免得模型再派
//!   一遍。自己的回合已经结束、在等孙代理的子代理，挂一条只等它收尾的任务。
//! - `/goal` 续轮：接着跑这一轮，自动续轮重新武装。
//! - QQ：等账号连上再接，多久都等。
//!
//! 不接的只有接不了的，见 [`resume_decision`]。每条认领最后都结案（resumed / skipped /
//! failed + 原因），下次启动不再翻出来。QQ 账号一直没连上时不结案，下次启动再接。

use crate::web::*;
use miyu_base::workspace::TurnOrigin;
use miyu_core::state::{
    ContinuingSubagent, RestartOrphan, ServiceRestart, SessionRecord, SubagentTaskState,
    SUBAGENT_SESSION_KIND, USER_SESSION_KIND,
};
use std::collections::{HashMap, HashSet};

/// daemon 刚起来时 QQ 账号多半还没连上：隔这么久看一眼。
const PLATFORM_CONNECT_POLL: Duration = Duration::from_secs(2);

/// 一条认领的去向。
#[derive(Debug, PartialEq, Eq)]
pub(in crate::web) enum ResumeDecision {
    Resume { attempt: u32, goal_round: bool },
    Skip(&'static str),
}

/// 决定一条认领接不接着跑。不接的只有接不了的：
/// - 会话没了、归档了、不是对话会话（一次性 `miyu "…"` 的会话在客户端退出时就删了）。
/// - 这个会话后来又有了别的轮：人已经接着聊下去了（同一会话里更早的孤儿也走这条）。
pub(in crate::web) fn resume_decision(
    orphan: &RestartOrphan,
    record: Option<&SessionRecord>,
    newer_turn_exists: bool,
) -> ResumeDecision {
    let Some(record) = record else {
        return ResumeDecision::Skip("the session no longer exists");
    };
    if record.kind != USER_SESSION_KIND && record.kind != SUBAGENT_SESSION_KIND {
        return ResumeDecision::Skip("not a conversation session");
    }
    if record.archived {
        return ResumeDecision::Skip("the session is archived");
    }
    if newer_turn_exists {
        return ResumeDecision::Skip("the conversation already moved on");
    }
    let content = &orphan.user_content;
    ResumeDecision::Resume {
        attempt: miyu_core::state::restart_chain_attempt(content)
            .map_or(1, |previous| previous.saturating_add(1)),
        goal_round: content
            .trim_start()
            .starts_with(miyu_core::state::GOAL_ROUND_TAG)
            || miyu_core::state::restart_chain_is_goal_round(content),
    }
}

/// daemon 起来、投递与平台都装好之后调。在后台跑，不挡启动。
pub(in crate::web) fn spawn_restart_resumes(state: &DaemonState) {
    let state = state.clone();
    tokio::spawn(async move {
        for store in account_stores(&state) {
            resume_store(&state, store).await;
        }
    });
}

/// 管理员的库加上每个成员的库。成员库在这里第一次打开时会顺手收一遍孤儿。
fn account_stores(state: &DaemonState) -> Vec<StateStore> {
    let mut stores = vec![state.state_store.clone()];
    let accounts = state.state_store.list_accounts().unwrap_or_default();
    for account in accounts.iter().filter(|account| !account.is_admin()) {
        match state.stores.for_owner(&account.id) {
            Ok(store) => stores.push(store),
            Err(error) => tracing::warn!(
                account = %account.id,
                error = %error,
                "restart resume: opening a member database failed"
            ),
        }
    }
    stores
}

/// 一条要接着跑的认领。
struct Plan {
    orphan: RestartOrphan,
    record: SessionRecord,
    attempt: u32,
    goal_round: bool,
}

/// 一次接续的结果：当场结案，或者交给后台（QQ 等账号连上）自己结案。
enum Outcome {
    Settled(&'static str, String),
    Deferred,
}

/// 这一趟接回来的子代理，按父会话记。父会话的续跑消息据它列新任务号。
#[derive(Default)]
struct Reattached {
    by_parent: HashMap<String, Vec<ContinuingSubagent>>,
    /// 挂过「只等不跑」任务的子代理，一个只挂一条。
    watched: HashSet<String>,
}

async fn resume_store(state: &DaemonState, store: StateStore) {
    // 还标着「执行中」、主人已经死了的，先在这里收尾记认领（成员库第一次打开时已经
    // 收过一遍，重复调无害）。
    if let Err(error) = store.recover_stale_turns() {
        tracing::warn!(error = %error, "restart resume: stale turn recovery failed");
    }
    let orphans = match store.pending_restart_orphans() {
        Ok(orphans) => orphans,
        Err(error) => {
            tracing::warn!(error = %error, "restart resume: listing interrupted turns failed");
            return;
        }
    };
    let mut plans = Vec::new();
    for orphan in orphans {
        let record = store.session_record(&orphan.session_id).ok().flatten();
        let newer_turn_exists = store
            .has_turn_after(&orphan.session_id, orphan.seq)
            .unwrap_or(true);
        match (
            resume_decision(&orphan, record.as_ref(), newer_turn_exists),
            record,
        ) {
            (
                ResumeDecision::Resume {
                    attempt,
                    goal_round,
                },
                Some(record),
            ) => plans.push(Plan {
                orphan,
                record,
                attempt,
                goal_round,
            }),
            (ResumeDecision::Skip(reason), _) => close(&store, &orphan, "skipped", reason),
            (ResumeDecision::Resume { .. }, None) => {
                close(&store, &orphan, "skipped", "the session no longer exists")
            }
        }
    }
    // 深的先接：孙代理要先于等它的子代理挂好，子代理要先于列出它们的父会话。
    plans.sort_by_key(|plan| std::cmp::Reverse(plan.record.depth));
    let orphaned: HashSet<String> = plans
        .iter()
        .map(|plan| plan.orphan.session_id.clone())
        .collect();
    let mut reattached = Reattached::default();
    for plan in plans {
        let subagents = reattached
            .by_parent
            .remove(&plan.orphan.session_id)
            .unwrap_or_default();
        let restart = ServiceRestart {
            attempt: plan.attempt,
            goal_round: plan.goal_round,
            subagents: &subagents,
        };
        let outcome = if plan.record.kind == SUBAGENT_SESSION_KIND {
            settled(resume_subagent(&store, &plan, &restart, &orphaned, &mut reattached).await)
        } else {
            resume_conversation(state, &store, &plan, &restart).await
        };
        if let Outcome::Settled(outcome, detail) = outcome {
            close(&store, &plan.orphan, outcome, &detail);
        }
    }
}

fn settled(result: std::result::Result<String, String>) -> Outcome {
    match result {
        Ok(detail) => Outcome::Settled("resumed", detail),
        Err(reason) => Outcome::Settled("failed", reason),
    }
}

fn close(store: &StateStore, orphan: &RestartOrphan, outcome: &str, detail: &str) {
    tracing::info!(
        session_id = %orphan.session_id,
        turn_id = %orphan.turn_id,
        interrupted_at = ?orphan.last_activity,
        outcome,
        detail = %detail,
        "turn interrupted by a daemon restart"
    );
    if let Err(error) = store.close_restart_orphan(&orphan.turn_id, outcome, detail) {
        tracing::warn!(error = %error, "restart resume: closing the claim failed");
    }
}

/// 回合当时在哪个目录干活，续跑就还在那儿（目录没了就交给会话的默认工作区）。
fn turn_workdir(orphan: &RestartOrphan) -> Option<std::path::PathBuf> {
    orphan
        .workspace
        .as_deref()
        .map(std::path::PathBuf::from)
        .filter(|path| path.is_dir())
}

/// 子代理：挂回父会话名下，在子会话里接着跑。父会话若也是子代理、自己没有被打断的
/// 回合，它是在等这一个，给它挂一条只等不跑的任务，它收尾时结果才回得到主会话。
async fn resume_subagent(
    store: &StateStore,
    plan: &Plan,
    restart: &ServiceRestart<'_>,
    orphaned: &HashSet<String>,
    reattached: &mut Reattached,
) -> std::result::Result<String, String> {
    let port = miyu_base::host_ports::subagent_port()
        .ok_or_else(|| "the subagent host is not installed".to_string())?;
    let child = &plan.record;
    let parent = child
        .parent_session_id
        .clone()
        .ok_or_else(|| "the subagent has no parent session".to_string())?;
    // 先标成在跑：等它的上一级（下面那条只等不跑的任务）据此知道还有事没完。
    let _ = store.set_session_task_state(&child.session_id, SubagentTaskState::Running);
    let job_id = match reattach(
        &port,
        &parent,
        child,
        Some(miyu_core::state::compose_service_restart_message(restart)),
        turn_workdir(&plan.orphan),
    )
    .await
    {
        Ok(job_id) => job_id,
        Err(reason) => {
            // 没挂上就别留着「在跑」：等它的上一级会一直等下去。
            let _ = store.set_session_task_state(&child.session_id, SubagentTaskState::Interrupted);
            return Err(reason);
        }
    };
    reattached
        .by_parent
        .entry(parent.clone())
        .or_default()
        .push(continuing(&job_id, child));
    if !orphaned.contains(&parent) && reattached.watched.insert(parent.clone()) {
        watch_waiting_parent(store, &port, &parent, reattached).await;
    }
    Ok(format!(
        "attempt {}, job {job_id} under {parent}",
        restart.attempt
    ))
}

async fn watch_waiting_parent(
    store: &StateStore,
    port: &Arc<dyn miyu_base::host_ports::SubagentHostPort>,
    parent: &str,
    reattached: &mut Reattached,
) {
    let Ok(Some(record)) = store.session_record(parent) else {
        return;
    };
    let Some(grandparent) = record
        .parent_session_id
        .clone()
        .filter(|_| record.kind == SUBAGENT_SESSION_KIND)
    else {
        return;
    };
    match reattach(port, &grandparent, &record, None, None).await {
        Ok(job_id) => reattached
            .by_parent
            .entry(grandparent)
            .or_default()
            .push(continuing(&job_id, &record)),
        Err(reason) => tracing::warn!(
            session = %parent,
            %reason,
            "restart resume: could not watch a subagent waiting on its own subagents"
        ),
    }
}

async fn reattach(
    port: &Arc<dyn miyu_base::host_ports::SubagentHostPort>,
    parent: &str,
    child: &SessionRecord,
    message: Option<String>,
    workdir: Option<std::path::PathBuf>,
) -> std::result::Result<String, String> {
    miyu_engine::tools::subagent::reattach_background_child(
        port.clone(),
        miyu_engine::tools::subagent::ReattachChild {
            parent_session: parent.to_string(),
            child_session: child.session_id.clone(),
            name: child.name.clone(),
            dev: child.persona == miyu_core::state::DEV_PERSONA,
            message,
            workdir,
        },
    )
    .await
    .map_err(|error| format!("{error:#}"))
}

fn continuing(job_id: &str, child: &SessionRecord) -> ContinuingSubagent {
    ContinuingSubagent {
        job_id: job_id.to_string(),
        session_id: child.session_id.clone(),
        name: child.name.clone(),
    }
}

async fn resume_conversation(
    state: &DaemonState,
    store: &StateStore,
    plan: &Plan,
    restart: &ServiceRestart<'_>,
) -> Outcome {
    let binding = store
        .platform_binding_for_session(&plan.orphan.session_id)
        .ok()
        .flatten();
    match binding {
        Some(binding) => {
            // 等账号连上可能要很久：放到后台，别挡着后面的认领。
            let state = state.clone();
            let store = store.clone();
            let orphan = plan.orphan.clone();
            let message = miyu_core::state::compose_service_restart_message(restart);
            let attempt = restart.attempt;
            tokio::spawn(async move {
                let (outcome, detail) =
                    resume_platform(&state, &store, &orphan, &binding, message, attempt).await;
                close(&store, &orphan, outcome, &detail);
            });
            Outcome::Deferred
        }
        None => settled(resume_local(state, store, plan, restart).await),
    }
}

async fn resume_local(
    state: &DaemonState,
    store: &StateStore,
    plan: &Plan,
    restart: &ServiceRestart<'_>,
) -> std::result::Result<String, String> {
    let session_id = plan.orphan.session_id.as_str();
    // `/goal` 续轮：目标还在跑就照续轮的身份接着跑（报完成的资格、`/goal pause` 停得住
    // 都认这个），自动续轮重新武装（用户 09-24）。目标已经不在跑了，就只把这一轮接完。
    let goal = plan
        .goal_round
        .then(|| store.goal(session_id).ok().flatten())
        .flatten()
        .filter(|goal| goal.phase == miyu_core::state::GoalPhase::Active);
    let turn_origin = match &goal {
        Some(goal) => TurnOrigin::GoalRound {
            goal_id: goal.goal_id.clone(),
            revision: goal.revision,
            round: goal.rounds_started,
        },
        None => TurnOrigin::ServiceRestart {
            attempt: restart.attempt,
        },
    };
    if goal.is_some() {
        miyu_engine::tools::goal::set_armed(session_id, true);
    }
    let content = miyu_core::state::compose_service_restart_message(restart);
    let delivery = Delivery {
        // 外壳本身就是给界面认的那一份：终端和网页按 attempt 用自己的语言画那一行。
        display_content: content.clone(),
        wake_label: miyu_core::state::service_restart_headline(restart.attempt),
        turn_origin,
        cwd: turn_workdir(&plan.orphan),
        origin_tty: None,
        content,
    };
    let goal_note = if goal.is_some() { ", goal round" } else { "" };
    match deliver(state, session_id.into(), delivery).await {
        Delivered::Woke(run) => {
            if goal.is_some() {
                // 驱动器靠这笔登记判「这一轮干没干活」，再决定接不接着开下一轮。
                miyu_engine::tools::goal::track_goal_run(&run.run_id, session_id);
            }
            Ok(format!(
                "attempt {}{goal_note}, run {}",
                restart.attempt, run.run_id
            ))
        }
        Delivered::Queued => Ok(format!(
            "attempt {}{goal_note}, joined a running turn",
            restart.attempt
        )),
        Delivered::Failed(reason) => {
            if goal.is_some() {
                miyu_engine::tools::goal::set_armed(session_id, false);
            }
            Err(reason)
        }
    }
}

/// 返回结案的 (outcome, detail)。
async fn resume_platform(
    state: &DaemonState,
    store: &StateStore,
    orphan: &RestartOrphan,
    binding: &miyu_core::state::PlatformSessionBinding,
    message: String,
    attempt: u32,
) -> (&'static str, String) {
    let key = &binding.key;
    if key.platform != "onebot" {
        return ("failed", format!("unsupported platform {}", key.platform));
    }
    while !crate::platforms::onebot::account_connected(state, &key.account_id) {
        tokio::time::sleep(PLATFORM_CONNECT_POLL).await;
    }
    // 等的这段时间里群里可能已经有人开了新的一轮。
    if store
        .has_turn_after(&orphan.session_id, orphan.seq)
        .unwrap_or(true)
    {
        return (
            "skipped",
            "the conversation moved on while QQ was reconnecting".to_string(),
        );
    }
    // 发起者不知道（库里没存哪条消息触发了那一轮）：私聊按对端，群聊降级成机器人自己，
    // 工具面是受限那一张，不凭空给权限（`wake_sender_user_id`）。
    match crate::platforms::onebot::wake_conversation_for_restart(
        state,
        &key.account_id,
        &key.conversation_kind,
        &key.conversation_id,
        None,
        message,
    )
    .await
    {
        Ok(()) => (
            "resumed",
            format!("attempt {attempt}, QQ {}", key.conversation_kind),
        ),
        Err(error) => ("failed", format!("{error:#}")),
    }
}
