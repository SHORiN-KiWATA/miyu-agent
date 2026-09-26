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

fn row(id: &str) -> crate::cli::repl::strip::SubagentRow {
    crate::cli::repl::strip::SubagentRow {
        session_id: id.into(),
        title: id.into(),
        state: "running".into(),
        dev: false,
        job_id: None,
        running_descendants: 0,
        peek: String::new(),
        tokens_label: String::new(),
        running_since_ms: None,
    }
}

fn parent_row() -> crate::cli::repl::strip::ParentRow {
    crate::cli::repl::strip::ParentRow {
        session_id: "root".into(),
        title: "root".into(),
        root: true,
    }
}

/// 切进子代理再退回来：主会话名下的子代理表和它自己的任务一直在手里，任务条不空一拍（用户
/// 09-25：切回来的时候底下的状态行会消失一瞬间——原来退回来那一下先把兄弟表清了、任务表按
/// 半新半旧的范围摘了，要等下一轮轮询才补回来）。
#[test]
fn going_back_keeps_the_parents_rows_and_jobs() {
    let feed = SharedJobsFeed::default();
    feed.set_scope("root", &[]);
    feed.publish_children("root", vec![row("c1"), row("c2")]);
    feed.publish_jobs(vec![
        job("rootcmd", Some("root"), Some("root")),
        job("c1cmd", Some("c1"), Some("root")),
    ]);

    feed.set_scope("c1", &["root".to_string()]);
    feed.publish_children("c1", vec![row("g1")]);
    assert_eq!(ids(&feed.jobs.lock().unwrap()), ["rootcmd", "c1cmd"]);

    feed.set_scope("root", &[]);
    assert_eq!(ids(&feed.jobs.lock().unwrap()), ["rootcmd", "c1cmd"]);
    let items = feed.strip_items(&[], &feed.jobs.lock().unwrap().clone());
    assert_eq!(
        items.len(),
        4,
        "主会话那一行、两个子代理、主会话的命令: {items:#?}"
    );
    assert!(
        !feed.children.lock().unwrap().contains_key("c1"),
        "访问路径以外的子代理表不留"
    );
}

/// 任务条从轮询那份取：主会话那一行在最上面，正在看的这条挂在它下面，它名下的再往下挂一层，
/// 兄弟跟在后面。
#[test]
fn the_feed_nests_the_current_sessions_children_under_it() {
    use crate::cli::repl::strip::{Place, StripItem};
    let feed = SharedJobsFeed::default();
    feed.set_scope("c1", &["root".to_string()]);
    feed.publish_children("root", vec![row("c1"), row("c2")]);
    feed.publish_children("c1", vec![row("g1")]);
    feed.publish_children("elsewhere", vec![row("x")]);

    let items = feed.strip_items(&[parent_row()], &[]);
    let shape: Vec<(String, usize)> = items
        .iter()
        .map(|item| match item {
            StripItem::Root { row: root, .. } => {
                (format!("root:{}", root.session_id), item.depth())
            }
            StripItem::Agent { row, place, .. } => {
                let tag = if *place == Place::Current {
                    "current"
                } else {
                    "agent"
                };
                (format!("{tag}:{}", row.session_id), item.depth())
            }
            StripItem::Job { job, .. } => (format!("job:{}", job.job_id), item.depth()),
        })
        .collect();
    assert_eq!(
        shape,
        [
            ("root:root".to_string(), 0),
            ("current:c1".to_string(), 0),
            ("agent:g1".to_string(), 1),
            ("agent:c2".to_string(), 0),
        ]
    );
    assert!(
        !feed.children.lock().unwrap().contains_key("elsewhere"),
        "不在这一段树上的不收"
    );
}
