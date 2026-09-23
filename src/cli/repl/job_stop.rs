//! 停后台任务：浮层里按 x 那一下，空闲时与回合进行中共用这一份。
//!
//! 用户 09-24：AI 跑着的时候点状态行打开浮层、按 x，任务不停。浮层只记下要停哪一个
//! （`LiveReplTail::pending_stop_job`）再把自己关掉，真正去停原来只有空闲的输入循环
//! 会做（`LiveReplOutcome::StopJob`）；回合循环从不看这个标记，任务要等这一轮说完
//! 才停。所以「停」收在这里，回合循环把按键交给浮层之后也来这儿问一声。

use crate::cli::*;

/// 停一个后台任务，状态行当场摘掉，结果用一句提示说。
///
/// daemon 管的任务走 IPC；直连模式（`JobsFeed::Local`）的任务就在本进程里。
pub(in crate::cli) async fn stop_background_job(
    paths: &MiyuPaths,
    feed: &JobsFeed,
    live: &mut LiveReplTail,
    job_id: &str,
) -> Result<()> {
    let stopped = match feed {
        JobsFeed::Shared(_) => send_ipc_command(
            paths,
            IpcCommand::StopJob {
                job_id: job_id.to_string(),
            },
        )
        .await
        .is_ok(),
        JobsFeed::Local(_) => miyu_engine::tools::jobs::stop_job(job_id).await.is_ok(),
    };
    let note = if stopped {
        // 压住它：紧接着那次轮询还带着它，状态行会闪一下。
        live.suppress_jobs(std::iter::once(job_id));
        let remaining = live
            .jobs
            .iter()
            .filter(|job| job.job_id != job_id)
            .cloned()
            .collect();
        live.set_jobs(remaining);
        t("background task stopped", "已停止这个后台任务")
    } else {
        t("could not stop the task", "没能停掉这个后台任务")
    };
    repl_note(live, &format!("\x1b[2m{note}\x1b[0m\n"))
}

/// 浮层里按过 x 就当场停掉那个任务。回合循环把按键交给浮层之后调；空闲时这件事
/// 由主循环经 `LiveReplOutcome::StopJob` 做，不走这里。
pub(in crate::cli) async fn stop_pending_job(
    paths: &MiyuPaths,
    feed: &JobsFeed,
    live: &mut LiveReplTail,
) -> Result<()> {
    let Some(job_id) = live.pending_stop_job.take() else {
        return Ok(());
    };
    stop_background_job(paths, feed, live, &job_id).await?;
    // 回合跑着时没有下一圈主循环来重画：状态行这就摘掉。正文正在往外写的那一刻
    // 不插这一帧，下一次转轮 tick 会带上。
    if !live.external_output_active {
        synchronized_terminal_update(CursorAfterUpdate::Preserve, || live.redraw())?;
    }
    Ok(())
}
