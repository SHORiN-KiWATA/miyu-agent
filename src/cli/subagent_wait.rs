//! 一次性命令等子代理（09-26 起子代理只在后台跑）。
//!
//! 子代理派出去，派它的那一轮当场就收尾了；结论在子代理跑完、报告叫醒这条会话再起的
//! 那几轮里。一次性命令（`miyu "…"`、`miyu ask`、JSON 输出、`miyu stdio` 的一条消息）
//! 要带着结论退出，所以主回合收尾后留在前台：隔一会儿看一眼任务总览，这条会话起了唤醒
//! 轮就跟上去（终端画出来 / JSON 往外吐），整棵子树（孙代理也算）的任务都收了尾再走。
//!
//! 判「报告都回完了」看的是任务而不是回合：跑完的任务要等它叫醒的那一轮结束才从总览里
//! 摘掉（`job_wake.rs`），所以「这棵树名下一个子代理任务都不剩」就是报告都回完了，中间
//! 没有空档。唤醒轮一闪而过（端点当场报错、桩模型秒回）、两次看之间就跑完的，daemon 在
//! 总览里还留它一分钟（`recent_wake_runs`），照样按起点整轮补看；事件环都补不回来的，
//! 收尾时对一遍会话库（[`turns_after_main`]）。
//!
//! 画 / 吐是两个前端各自的事：终端那一面在 `repl::remote::subagent_follow`，JSON 那一面
//! 在 `output::conclusion`。这里只管「下一步是什么」。

use crate::cli::repl::jobs::{fetch_wake_overview, WakeRun};
use crate::cli::*;
use miyu_engine::tools::jobs::JobOverview;
use std::collections::HashSet;

/// 两次看任务总览之间隔多久。唤醒轮要先发一次模型请求，比这长得多。
const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// 收尾时往回翻几轮找漏画的。主回合之后的轮数不会超过派出去的子代理数太多。
const MISSED_TURNS_LOOKBACK: usize = 64;

/// 等的下一步。
#[derive(Debug, PartialEq, Eq)]
pub(in crate::cli) enum WaitStep {
    /// 还有 `n` 个子代理在跑（数变了才报一次，给等待文案用）。
    Waiting(usize),
    /// 这条会话起了一轮（报告叫醒的、目标续轮、跨会话消息）：跟上去。
    Follow(WakeRun),
    /// 整棵子树都收了尾，报告也都回完了。
    Settled,
}

/// 一次性命令主回合之后的等待。
pub(in crate::cli) struct SubagentWait {
    session_id: String,
    followed: HashSet<String>,
    /// 主回合在 daemon 事件流里的第一个号。起点比它早的唤醒轮是这条命令之前的事
    /// （同一条会话上一条命令留下的），不跟。
    not_before: Option<u64>,
    reported: Option<usize>,
}

impl SubagentWait {
    /// `main_run`：一次性命令自己起的那一轮，已经画过了，不再跟；`main_first_event`：
    /// 它的第一个事件号。
    pub(in crate::cli) fn new(
        session_id: &str,
        main_run: Option<&str>,
        main_first_event: Option<u64>,
    ) -> Self {
        Self {
            session_id: session_id.to_string(),
            followed: main_run.map(str::to_string).into_iter().collect(),
            not_before: main_first_event,
            reported: None,
        }
    }

    pub(in crate::cli) fn session_id(&self) -> &str {
        &self.session_id
    }

    /// 等下一步。随时可以丢掉这个 future（Ctrl+C、宿主取消、超时），不留半截状态。
    pub(in crate::cli) async fn next(&mut self, paths: &MiyuPaths) -> Result<WaitStep> {
        loop {
            let (jobs, wake_runs) = fetch_wake_overview(paths).await?;
            if let Some(step) = self.step(&jobs, wake_runs) {
                return Ok(step);
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// 按一份总览定下一步；None = 接着等（在跑的数也没变）。
    fn step(&mut self, jobs: &[JobOverview], wake_runs: Vec<WakeRun>) -> Option<WaitStep> {
        if let Some(run) = wake_runs.into_iter().find(|run| self.is_new_wake(run)) {
            self.followed.insert(run.run_id.clone());
            return Some(WaitStep::Follow(run));
        }
        let tree = jobs
            .iter()
            .filter(|job| is_subagent_job(job) && self.in_tree(job))
            .collect::<Vec<_>>();
        if tree.is_empty() {
            return Some(WaitStep::Settled);
        }
        let running = tree.iter().filter(|job| job.running).count();
        if self.reported == Some(running) {
            return None;
        }
        self.reported = Some(running);
        Some(WaitStep::Waiting(running))
    }

    /// 这条会话里、主回合之后起的、还没跟过的一轮。
    fn is_new_wake(&self, run: &WakeRun) -> bool {
        run.session_id == self.session_id
            && !self.followed.contains(&run.run_id)
            && match (run.first_event_id, self.not_before) {
                (Some(first), Some(not_before)) => first >= not_before,
                _ => true,
            }
    }

    /// 这个任务挂在这条会话的子树上（它自己派的，或者它的子代理再派的）。
    fn in_tree(&self, job: &JobOverview) -> bool {
        job.session_id.as_deref() == Some(self.session_id.as_str())
            || job.root_session_id.as_deref() == Some(self.session_id.as_str())
    }
}

fn is_subagent_job(job: &JobOverview) -> bool {
    matches!(job.kind.as_str(), "subagent" | "dev")
}

/// 停下这条会话名下的各层子代理和它们的轮（Ctrl+C、宿主取消、超时）。尽力而为：
/// daemon 没了也就没什么可停的。
pub(in crate::cli) async fn stop_subagents(paths: &MiyuPaths, session_id: &str) {
    let _ = send_ipc_admin(
        paths,
        IpcCommand::StopSessionJobs {
            session_id: session_id.to_string(),
        },
    )
    .await;
}

/// 主回合之后这条会话里的各轮（完成的、被打断的），按先后排，从会话库里取。收尾时拿它
/// 对一遍跟的时候漏掉的（两次看之间就跑完的唤醒轮、事件环已经补不回来的）。主回合在库里
/// 找不到（被隐藏了之类）就一轮都不给，免得把老轮当新轮。
pub(in crate::cli) fn turns_after_main(
    paths: &MiyuPaths,
    session_id: &str,
    main_turn: &str,
) -> Result<Vec<miyu_core::state::TurnReplay>> {
    let turns = StateStore::new(paths)?
        .pinned(session_id)
        .session_replay(MISSED_TURNS_LOOKBACK)?;
    Ok(after_main(turns, main_turn))
}

fn after_main(
    turns: Vec<miyu_core::state::TurnReplay>,
    main_turn: &str,
) -> Vec<miyu_core::state::TurnReplay> {
    let Some(main_index) = turns.iter().position(|turn| turn.turn_id == main_turn) else {
        return Vec::new();
    };
    turns.into_iter().skip(main_index + 1).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(session: &str, root: Option<&str>, kind: &str, running: bool) -> JobOverview {
        serde_json::from_value(serde_json::json!({
            "job_id": format!("job-{session}-{kind}-{running}"),
            "title": "t",
            "kind": kind,
            "session_id": session,
            "root_session_id": root,
            "status": if running { "running" } else { "finished" },
            "running": running,
            "runtime_seconds": 1,
        }))
        .unwrap()
    }

    fn wake(run_id: &str, session: &str) -> WakeRun {
        WakeRun {
            run_id: run_id.to_string(),
            session_id: session.to_string(),
            label: String::new(),
            from_start: false,
            first_event_id: Some(7),
        }
    }

    #[test]
    fn waits_for_the_whole_subtree_and_follows_each_wake_once() {
        let mut wait = SubagentWait::new("main", Some("run-main"), Some(5));
        // 主回合自己那一轮还挂在活跃表里：不再跟它。孙代理挂在子会话下、根是主会话。
        let jobs = vec![
            job("main", Some("main"), "subagent", true),
            job("child", Some("main"), "dev", true),
            job("elsewhere", Some("elsewhere"), "subagent", true),
            job("main", Some("main"), "command", true),
        ];
        assert_eq!(
            wait.step(&jobs, vec![wake("run-main", "main")]),
            Some(WaitStep::Waiting(2))
        );
        // 数没变就不再报。
        assert_eq!(wait.step(&jobs, Vec::new()), None);
        // 报告叫醒了主会话：跟一次，第二次看到同一轮不再跟；别的会话的唤醒轮不管。
        let woke = vec![wake("run-1", "main"), wake("run-x", "elsewhere")];
        assert_eq!(
            wait.step(&jobs, woke.clone()),
            Some(WaitStep::Follow(wake("run-1", "main")))
        );
        // 跑完了的任务要等它叫醒的那一轮结束才摘掉：还在表上就接着等。
        let finished = vec![
            job("main", Some("main"), "subagent", false),
            job("child", Some("main"), "dev", true),
        ];
        assert_eq!(wait.step(&finished, woke), Some(WaitStep::Waiting(1)));
        // 命令类后台任务不等；子树里一个子代理任务都不剩才算收尾。
        let only_command = vec![job("main", Some("main"), "command", true)];
        assert_eq!(
            wait.step(&only_command, Vec::new()),
            Some(WaitStep::Settled)
        );
    }

    #[test]
    fn wakes_from_before_the_main_turn_are_not_followed() {
        let mut wait = SubagentWait::new("main", Some("run-main"), Some(100));
        let jobs = vec![job("main", Some("main"), "subagent", true)];
        // 上一条命令在同一条会话里留下的唤醒轮（刚跑完、还在总览里）：起点早于主回合，不跟。
        let mut old = wake("run-old", "main");
        old.first_event_id = Some(40);
        assert_eq!(wait.step(&jobs, vec![old]), Some(WaitStep::Waiting(1)));
        // 主回合之后起的（哪怕已经跑完了）要跟；起点不知道的（老 daemon）也跟。
        let mut fresh = wake("run-fresh", "main");
        fresh.first_event_id = Some(180);
        assert_eq!(
            wait.step(&jobs, vec![fresh.clone()]),
            Some(WaitStep::Follow(fresh))
        );
        let mut unknown = wake("run-unknown", "main");
        unknown.first_event_id = None;
        assert_eq!(
            wait.step(&jobs, vec![unknown.clone()]),
            Some(WaitStep::Follow(unknown))
        );
    }

    fn replay(turn_id: &str) -> miyu_core::state::TurnReplay {
        miyu_core::state::TurnReplay {
            turn_id: turn_id.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn only_turns_after_the_main_turn_count() {
        let turns = vec![
            replay("old"),
            replay("main"),
            replay("wake-1"),
            replay("wake-2"),
        ];
        let after = after_main(turns.clone(), "main");
        assert_eq!(
            after
                .iter()
                .map(|turn| turn.turn_id.as_str())
                .collect::<Vec<_>>(),
            ["wake-1", "wake-2"]
        );
        // 主回合不在库里（被隐藏了）：一轮都不给，老轮不会被当成新的。
        assert!(after_main(turns, "gone").is_empty());
    }
}
