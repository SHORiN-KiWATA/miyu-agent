//! QQ 会话里的后台任务汇报怎么交出去（09-26 起子代理只在后台跑）。
//!
//! 汇报先落库留着（会话库 `held_job_reports`，daemon 重启也不丢），再看能不能交：
//! - 同一轮派出去的子代理还有没跑完的，就等最后一个，几份合成一份、起一轮（用户拍板「合成一份
//!   发」：群里一次说完，不一个子代理叫醒一次）；
//! - 账号没连上就留着，连上再发（连接建立时调 [`deliver_for_account`]）。
//!
//! 交之前先把这一批从库里认领走（删掉），两份汇报同时到也不会把同一批发两遍；账号在这当口掉了、
//! 唤醒没起来，就原样放回去。

use crate::platforms::onebot::*;
use miyu_core::state::HeldJobReport;

/// 认领只是几次库操作，一把锁就够了，不会久占。
fn claim_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// 一份汇报归哪一批：子代理按派它的那一轮合批；认不出轮号的、命令类任务自成一批。
pub(crate) fn report_batch(state: &DaemonState, job_id: &str, is_subagent: bool) -> String {
    is_subagent
        .then(|| miyu_engine::tools::subagent::child_session_of_job(job_id))
        .flatten()
        .and_then(|child| spawned_by_turn(state, &child))
        .unwrap_or_else(|| job_id.to_string())
}

fn spawned_by_turn(state: &DaemonState, child: &str) -> Option<String> {
    state
        .stores
        .for_session(child)
        .session_record(child)
        .ok()
        .flatten()
        .and_then(|record| record.spawned_by_turn)
}

/// 这一批还在等谁吗：同一轮派出去的子代理里，还挂在任务总览上（没跑完、或者跑完了还没交汇报）
/// 却没有汇报留在库里的。只看 `running` 不够：两个几乎同时跑完时，后一个已经不算在跑、汇报却还没
/// 落库，前一个就会把这一批拆开先发。任务交完汇报才从总览里摘（`job_wake::handle_job_completion`）。
fn batch_waiting(
    state: &DaemonState,
    session_id: &str,
    batch: &str,
    held_jobs: &std::collections::HashSet<String>,
) -> bool {
    miyu_engine::tools::jobs::overview().iter().any(|job| {
        job.session_id.as_deref() == Some(session_id)
            && matches!(job.kind.as_str(), "subagent" | "dev")
            && !held_jobs.contains(&job.job_id)
            && job
                .child_session_id
                .as_deref()
                .and_then(|child| spawned_by_turn(state, child))
                .as_deref()
                == Some(batch)
    })
}

/// 这条会话绑在哪个 QQ 对话上。
pub(crate) fn platform_binding(
    state: &DaemonState,
    session_id: &str,
) -> Option<miyu_core::state::PlatformSessionBinding> {
    let persona = state.manager.lock().unwrap().config.active_persona_scope();
    state
        .state_store
        .platform_session_bindings(&persona, "onebot")
        .ok()?
        .into_iter()
        .find(|binding| binding.session_id == session_id)
}

/// 能交的几批：按到来先后分批，还在等兄弟任务的那批先不动。
fn deliverable_batches(
    held: Vec<HeldJobReport>,
    waiting: impl Fn(&str) -> bool,
) -> Vec<Vec<HeldJobReport>> {
    let mut batches: Vec<Vec<HeldJobReport>> = Vec::new();
    for report in held {
        match batches
            .iter_mut()
            .find(|batch| batch[0].batch == report.batch)
        {
            Some(batch) => batch.push(report),
            None => batches.push(vec![report]),
        }
    }
    batches.retain(|batch| !waiting(&batch[0].batch));
    batches
}

/// 几份汇报合成一条唤醒：原样一份接一份，每份自带外壳（模型认得出是几件事）。
fn merged_content(batch: &[HeldJobReport]) -> String {
    batch
        .iter()
        .map(|report| report.content.as_str())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// 把这条会话留着的汇报里能交的交出去：每一批起一轮。账号没连上就都留着。
pub(crate) async fn deliver_held_reports(state: &DaemonState, session_id: &str) {
    let Some(binding) = platform_binding(state, session_id) else {
        tracing::debug!(
            session_id,
            "held job reports wait: the session has no QQ binding"
        );
        return;
    };
    if !account_connected(state, &binding.key.account_id) {
        tracing::info!(
            session_id,
            account = %binding.key.account_id,
            "held job reports wait for the QQ account to reconnect"
        );
        return;
    }
    let store = state.stores.for_session(session_id);
    let claimed = {
        let _claim = claim_lock().lock().unwrap();
        let held = store.held_job_reports(session_id).unwrap_or_default();
        let held_jobs = held
            .iter()
            .map(|report| report.job_id.clone())
            .collect::<std::collections::HashSet<_>>();
        let batches = deliverable_batches(held, |batch| {
            batch_waiting(state, session_id, batch, &held_jobs)
        });
        let ids = batches
            .iter()
            .flatten()
            .map(|report| report.id)
            .collect::<Vec<_>>();
        if let Err(error) = store.release_held_job_reports(&ids) {
            tracing::warn!(session_id, %error, "failed to claim held job reports");
            return;
        }
        batches
    };
    for batch in claimed {
        let initiator = batch.iter().find_map(|report| report.initiator.clone());
        let outcome = wake_conversation_for_job(
            state,
            &binding.key.account_id,
            &binding.key.conversation_kind,
            &binding.key.conversation_id,
            initiator.as_deref(),
            merged_content(&batch),
            batch.len(),
        )
        .await;
        match outcome {
            Ok(()) => tracing::info!(
                session_id,
                reports = batch.len(),
                "held job reports delivered to QQ"
            ),
            // 掉线了：原样放回去，连上再发。其余的失败（回合本身报错）照旧只记一笔。
            Err(error) if !account_connected(state, &binding.key.account_id) => {
                tracing::info!(session_id, %error, "QQ went offline; job reports held again");
                for report in &batch {
                    let _ = store.hold_job_report(
                        &report.session_id,
                        &report.batch,
                        &report.job_id,
                        report.initiator.as_deref(),
                        &report.content,
                    );
                }
            }
            Err(error) => tracing::warn!(
                session_id,
                %error,
                "failed to wake the model for background job reports in QQ"
            ),
        }
    }
}

/// 账号连上了：各会话留着的汇报补发（绑在别的账号上的会自己等着）。
pub(crate) async fn deliver_for_account(state: DaemonState, self_id: i64) {
    let sessions = state
        .state_store
        .sessions_with_held_job_reports()
        .unwrap_or_default();
    for session_id in sessions {
        let belongs = platform_binding(&state, &session_id)
            .is_some_and(|binding| binding.key.account_id == self_id.to_string());
        if belongs {
            deliver_held_reports(&state, &session_id).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(id: i64, batch: &str) -> HeldJobReport {
        HeldJobReport {
            id,
            session_id: "qq".to_string(),
            batch: batch.to_string(),
            job_id: format!("job-{id}"),
            initiator: None,
            content: format!("<background-job-report>{id}</background-job-report>"),
        }
    }

    #[test]
    fn a_batch_waits_for_its_last_subagent_and_goes_out_as_one() {
        let held = vec![report(1, "turn-a"), report(2, "job-x"), report(3, "turn-a")];
        // 派出 turn-a 那一批的子代理还有一个没交汇报：那一批留着，自成一批的命令先交。
        let batches = deliverable_batches(held.clone(), |batch| batch == "turn-a");
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0][0].batch, "job-x");
        // 都跑完了：按先来的顺序分批，同一批的合成一份。
        let batches = deliverable_batches(held, |_| false);
        assert_eq!(
            batches
                .iter()
                .map(|batch| batch.iter().map(|report| report.id).collect::<Vec<_>>())
                .collect::<Vec<_>>(),
            [vec![1, 3], vec![2]]
        );
        assert_eq!(
            merged_content(&batches[0]),
            "<background-job-report>1</background-job-report>\n\n<background-job-report>3</background-job-report>"
        );
    }
}
