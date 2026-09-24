//! 会话化子代理的后台镜像任务(09-18):子会话的一次运行包进后台任务注册表里等,
//! 任务条、`job(action=stop)`、完成唤醒全走后台命令那一套,唤醒报告里带的是子会话
//! 最后一轮的正文。
//!
//! daemon 重启后把还在干活的子代理挂回父会话名下(09-24 断点续跑)也走这里:那时
//! 没有正在跑的工具调用可以回话,所以只登记任务、不出工具回执。

use super::*;
use futures_util::future::BoxFuture;
use miyu_base::host_ports::WatchChildRequest;
use std::path::PathBuf;

/// 镜像任务的本体:日志开头写任务原文,子会话的事件经日志桥进任务日志,子会话 id
/// 一报上来就登记到任务名下(模型追话时拿的是 job_id),结束时把交付物追加到日志
/// 尾巴上(唤醒报告读它)。
pub(super) async fn run_mirrored_child(
    job_id: String,
    log_path: PathBuf,
    header: String,
    run: impl FnOnce(SubagentProgressSink) -> BoxFuture<'static, Result<ChildOutcome>>,
) -> crate::tools::jobs::JobState {
    write_subagent_prompt_header(&log_path, &header);
    let bridge = spawn_subagent_log_bridge(job_id.clone(), log_path.clone());
    let sink: SubagentProgressSink = {
        let job_id = job_id.clone();
        Arc::new(move |message: String| {
            if let Some(session) = message.strip_prefix(SUBAGENT_SESSION_MARKER) {
                background_children()
                    .lock()
                    .unwrap()
                    .insert(job_id.clone(), session.to_string());
            }
            bridge.report(message);
        })
    };
    let outcome = run(sink).await;
    let (state_label, tail) = match &outcome {
        Ok(ChildOutcome::Finished(result)) => (
            result.state.as_str(),
            format!(
                "\n{}\nsession: {}\n{}\n",
                crate::tools::jobs::SUBAGENT_RESULT_MARKER,
                result.session_id,
                result.final_text
            ),
        ),
        Ok(ChildOutcome::Queued { session_id }) => (
            "done",
            format!(
                "\n{}\nsession: {session_id}\n(follow-up queued)\n",
                crate::tools::jobs::SUBAGENT_RESULT_MARKER
            ),
        ),
        Err(error) => (
            "error",
            format!("\n{}\n{error}\n", crate::tools::jobs::SUBAGENT_ERROR_MARKER),
        ),
    };
    let _ = std::fs::OpenOptions::new()
        .append(true)
        .open(&log_path)
        .and_then(|mut file| {
            use std::io::Write as _;
            file.write_all(tail.as_bytes())
        });
    tracing::debug!(job_id = %job_id, state = %state_label, "background subagent session finished");
    match state_label {
        "done" => crate::tools::jobs::JobState::Exited { code: Some(0) },
        _ => crate::tools::jobs::JobState::Exited { code: None },
    }
}

/// 要挂回父会话名下的一个子代理。
pub struct ReattachChild {
    pub parent_session: String,
    pub child_session: String,
    /// 任务条上的名字(子会话名)。
    pub name: String,
    pub dev: bool,
    /// 有:在子会话里接着跑一轮(续跑消息)。`None`:不起新一轮,只等它走到终态——
    /// 它自己的回合已经结束,在等被另外接回的孙代理,孙代理跑完会叫醒它。
    pub message: Option<String>,
    pub workdir: Option<PathBuf>,
}

/// daemon 重启后把一个还在干活的子代理挂回父会话名下(09-24 断点续跑):起一条后台
/// 镜像任务,结果照后台子代理那套送回父会话。返回新任务号。
pub async fn reattach_background_child(
    port: Arc<dyn SubagentHostPort>,
    request: ReattachChild,
) -> Result<String> {
    let ReattachChild {
        parent_session,
        child_session,
        name,
        dev,
        message,
        workdir,
    } = request;
    let header = message.clone().unwrap_or_else(|| name.clone());
    let owner: Arc<str> = parent_session.as_str().into();
    let task_workdir = workdir.clone();
    let register = async move {
        crate::tools::jobs::register_background_subagent(
            None,
            &name,
            dev,
            move |job_id, log_path| {
                run_mirrored_child(job_id, log_path, header, move |progress| match message {
                    Some(message) => port.continue_child(ContinueChildRequest {
                        parent_session,
                        child_session,
                        message,
                        workdir,
                        progress,
                    }),
                    None => port.watch_child(WatchChildRequest {
                        parent_session,
                        child_session,
                        progress,
                    }),
                })
            },
        )
    };
    // 任务归哪个会话、在哪个目录,登记时取 task-local:在父会话的作用域里登记,
    // 完成时才叫醒得了父会话。
    let scoped = miyu_base::workspace::with_session(owner, register);
    let registered = match task_workdir {
        Some(dir) => miyu_base::workspace::with_workspace(dir, scoped).await,
        None => scoped.await,
    };
    registered.map(|(job_id, _)| job_id)
}
