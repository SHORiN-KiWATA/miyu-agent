//! 状态行的后台任务只认这个 REPL 自己的会话（todolist 09-24：新开的终端先闪一下
//! 别的会话的后台任务）。真机走查见 `testkit/tui/other_session_jobs.py`。

use crate::cli::repl::jobs::SharedJobsFeed;
use miyu_engine::tools::jobs::JobOverview;

fn job(id: &str, session: Option<&str>, root: Option<&str>) -> JobOverview {
    JobOverview {
        job_id: id.into(),
        title: id.into(),
        command: String::new(),
        kind: "command".into(),
        dev: false,
        session_id: session.map(str::to_string),
        root_session_id: root.map(str::to_string),
        status: "running".into(),
        running: true,
        runtime_seconds: 1,
        log_path: None,
        metric: None,
        metric_tokens: None,
        child_session_id: None,
    }
}

fn ids(jobs: &[JobOverview]) -> Vec<&str> {
    jobs.iter().map(|job| job.job_id.as_str()).collect()
}

/// 还不知道自己是哪条会话时一条都不认。原来这时「全都显示」，轮询线程第一次拉到
/// 的任务表原样上了状态行。
#[test]
fn a_feed_that_does_not_know_its_session_shows_no_jobs() {
    let feed = SharedJobsFeed::default();
    let shown = feed.publish_jobs(vec![job("a1", Some("a"), None), job("b1", Some("b"), None)]);
    assert!(shown.is_empty(), "{:?}", ids(&shown));
    assert!(feed.jobs.lock().unwrap().is_empty());
}

/// 知道会话之后：自己的、子代理树里的、没挂会话的老任务留下，别的会话的摘掉。
#[test]
fn a_feed_keeps_only_its_own_sessions_jobs() {
    let feed = SharedJobsFeed::default();
    feed.set_repl_session("a");
    let shown = feed.publish_jobs(vec![
        job("a1", Some("a"), None),
        job("child", Some("a-sub"), Some("a")),
        job("legacy", None, None),
        job("b1", Some("b"), None),
    ]);
    assert_eq!(ids(&shown), ["a1", "child", "legacy"]);
    assert_eq!(ids(&feed.jobs.lock().unwrap()), ["a1", "child", "legacy"]);
}

/// 换会话当场摘掉上一个会话的任务，不等下一轮轮询。
#[test]
fn switching_sessions_drops_the_previous_sessions_jobs_at_once() {
    let feed = SharedJobsFeed::default();
    feed.set_repl_session("a");
    feed.publish_jobs(vec![job("a1", Some("a"), None), job("legacy", None, None)]);
    feed.set_repl_session("b");
    assert_eq!(ids(&feed.jobs.lock().unwrap()), ["legacy"]);
    assert_eq!(feed.repl_session.lock().unwrap().as_deref(), Some("b"));
}
