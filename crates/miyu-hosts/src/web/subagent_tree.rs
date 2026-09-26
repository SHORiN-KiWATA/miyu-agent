//! 子代理树上的两件事（09-26）：任务条折叠行上「开发中（+3）」那个数，和在子代理会话里
//! 按停止时连它名下的一起停。
//!
//! 主会话不走这里：主会话的停止照旧只停这一轮——后台子代理本来就是要它自己跑下去的。

use crate::web::*;
use miyu_core::state::{SubagentTaskState, SUBAGENT_SESSION_KIND};

/// `session_id` 名下还在跑的后代有几个：没到终态的子代理会话（孙代理……），加上这一支里
/// 还在跑的后台命令（它自己的、后代的）。后台子代理的镜像任务不算，它和会话是同一件事，
/// 已经数过了。
///
/// 任务条第一层只列这条会话自己的子代理和命令，后代收进这个数里，切进去才展开（用户 09-25）。
pub(in crate::web) fn running_descendants(
    state: &DaemonState,
    session_id: &str,
    jobs: &[tools::jobs::JobOverview],
) -> usize {
    let descendants = state
        .stores
        .for_session(session_id)
        .descendant_task_states(session_id)
        .unwrap_or_default();
    let agents = descendants
        .iter()
        .filter(|(_, task_state)| is_pending(task_state.as_deref()))
        .count();
    let in_branch =
        |owner: &str| owner == session_id || descendants.iter().any(|(id, _)| id == owner);
    // 后台子代理（开发模式的叫 `dev`）的镜像任务和会话是同一件事，会话那边已经数过了。
    let commands = jobs
        .iter()
        .filter(|job| job.running && !matches!(job.kind.as_str(), "subagent" | "dev"))
        .filter(|job| job.session_id.as_deref().is_some_and(in_branch))
        .count();
    agents + commands
}

/// 这条会话是子代理会话吗。查不到记录的一律当不是：停止的规矩只在认得出时才放宽。
pub(in crate::web) fn is_subagent_session(state: &DaemonState, session_id: &str) -> bool {
    state
        .stores
        .for_session(session_id)
        .session_record(session_id)
        .ok()
        .flatten()
        .is_some_and(|record| record.kind == SUBAGENT_SESSION_KIND)
}

/// 在子代理会话里按了停止（终端 Ctrl+C、网页停止按钮），连它名下的一起停：它自己的后台
/// 命令、后台孙代理、孙代理自己的轮和命令（09-26 用户拍板「连同它名下的一起停」）。会话都
/// 留着，停在哪看得到；它自己闲着等后台的话，也记成被打断，等它的主会话照常收到汇报。
/// 返回停掉了几个后台任务。
///
/// 主会话闲着按 Ctrl+C（`StopSessionJobs`）也停这一整棵树，只是分两段走：后台任务当场停、当场
/// 回话，各层的轮在后台收（见 `stop_subtree_jobs`、`settle_subtree_runs`）。主会话自己没有任务
/// 状态，最后那一下「记成被打断」对它不起作用。
pub(in crate::web) async fn stop_subagent_subtree(state: &DaemonState, root: &str) -> usize {
    let (stopped, descendants) = stop_subtree_jobs(state, root).await;
    settle_subtree_runs(state, root, &descendants).await;
    stopped
}

/// 第一段：树上所有会话的后台任务一起停，返回停了几个和名下的后代会话（从浅到深）。
///
/// 镜像任务要在任何一轮被取消之前停掉：标成「已停止」的任务收尾时不叫醒谁。先停孙代理的轮的
/// 话，孙代理一收尾就把这条子代理叫醒、再跑一轮汇报——人刚按了停止，它自己又动起来了。原来
/// 一层层停、每停一层等一遍，子代理一多就要等很久（用户 09-26：打断的时候会卡住）。
pub(in crate::web) async fn stop_subtree_jobs(
    state: &DaemonState,
    root: &str,
) -> (usize, Vec<String>) {
    let descendants = state
        .stores
        .for_session(root)
        .descendant_session_ids(root)
        .unwrap_or_default();
    let stopped = futures_util::future::join_all(
        std::iter::once(root)
            .chain(descendants.iter().map(String::as_str))
            .map(tools::jobs::stop_session_jobs),
    )
    .await
    .into_iter()
    .sum();
    (stopped, descendants)
}

/// 第二段：各层子代理的轮一起取消、一起等它们退场（最多 5 秒，不再一层层各等 5 秒），再把还没
/// 到终态的记成被打断——由深到浅，根最后。
pub(in crate::web) async fn settle_subtree_runs(
    state: &DaemonState,
    root: &str,
    descendants: &[String],
) {
    let running: Vec<&str> = descendants
        .iter()
        .map(String::as_str)
        .filter(|id| state.manager.lock().unwrap().session_has_runs(id))
        .collect();
    futures_util::future::join_all(
        running
            .iter()
            .map(|id| stop_session_runs(state, id, Duration::from_secs(5))),
    )
    .await;
    for id in descendants.iter().rev() {
        interrupt_if_pending(state, id);
    }
    interrupt_if_pending(state, root);
    if !descendants.is_empty() {
        tracing::info!(root = %root, sessions = descendants.len(), "subagent subtree stopped");
    }
}

/// 还挂着「运行中 / 等待后台」的会话记成被打断，等着它的父回合（前台）或镜像任务（后台）
/// 跟着收尾。有轮在跑的不用管：轮一结束监督器就会记（`spawn_subagent_supervisor`）。
fn interrupt_if_pending(state: &DaemonState, session_id: &str) {
    if state.manager.lock().unwrap().session_has_runs(session_id) {
        return;
    }
    let store = state.stores.for_session(session_id);
    let pending = store
        .session_record(session_id)
        .ok()
        .flatten()
        .is_some_and(|record| is_pending(record.task_state.as_deref()));
    if pending {
        let _ = store.set_session_task_state(session_id, SubagentTaskState::Interrupted);
        resolve_waiter(session_id, SubagentTaskState::Interrupted);
    }
}

fn is_pending(task_state: Option<&str>) -> bool {
    task_state
        .and_then(SubagentTaskState::parse)
        .is_some_and(SubagentTaskState::is_pending)
}
