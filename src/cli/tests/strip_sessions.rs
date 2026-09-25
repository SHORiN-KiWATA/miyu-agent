//! 任务条上的会话行与切进子代理会话（会话项目第 3 段）。真机走查见
//! `testkit/tui/subagent_visit.py`。

use super::shared::detached_tail;
use crate::cli::footer::FooterBadges;
use crate::cli::repl::jobs::format_job_duration;
use crate::cli::repl::strip::*;
use crate::cli::*;
use miyu_base::i18n::text;
use miyu_engine::tools::jobs::JobOverview;

fn job(id: &str, kind: &str, metric: Option<&str>) -> JobOverview {
    JobOverview {
        job_id: id.into(),
        title: format!("{id} 标题"),
        command: String::new(),
        kind: kind.into(),
        dev: false,
        session_id: Some("root".into()),
        root_session_id: Some("root".into()),
        status: "running".into(),
        running: true,
        runtime_seconds: 65,
        log_path: None,
        metric: metric.map(str::to_string),
        metric_tokens: None,
        child_session_id: None,
    }
}

fn child(id: &str, state: &str, job_id: Option<&str>) -> StripSession {
    StripSession::Child(SubagentRow {
        session_id: id.into(),
        title: format!("查{id}"),
        state: state.into(),
        dev: false,
        job_id: job_id.map(str::to_string),
    })
}

fn parent() -> ParentRow {
    ParentRow {
        session_id: "root".into(),
        title: "修登录页".into(),
        root: true,
    }
}

fn plain(lines: Vec<String>) -> Vec<String> {
    lines
        .iter()
        .map(|line| {
            strip_terminal_control_sequences(line)
                .trim_end()
                .to_string()
        })
        .collect()
}

/// 会话行排在后台任务前面；后台子代理的镜像任务不再单列，量和用时挂到会话那一行上。
#[test]
fn session_rows_come_first_and_absorb_their_mirror_job() {
    let sessions = vec![
        StripSession::Parent(parent()),
        child("c1", "running", Some("j1")),
        child("c2", "waiting", None),
    ];
    let jobs = vec![
        job("j1", "subagent", Some("≈3.1K")),
        job("cmd1", "command", None),
    ];
    let lines = plain(strip_lines(
        &strip_rows(&sessions, &jobs),
        0,
        80,
        StripView::default(),
    ));

    assert_eq!(lines.len(), 5, "{lines:#?}");
    assert!(lines[0].is_empty(), "头上一行空的: {lines:#?}");
    assert!(
        lines[1].starts_with(&format!("↑ {}", text("main", "主会话")))
            && lines[1].ends_with("修登录页"),
        "{:?}",
        lines[1]
    );
    assert!(lines[2].contains("查c1"), "{:?}", lines[2]);
    assert!(
        lines[2].ends_with(&format!("≈3.1K  {}", format_job_duration(65))),
        "镜像任务的量和用时挂到会话行上: {:?}",
        lines[2]
    );
    assert!(
        lines[3].ends_with(&format!(
            "查c2 · {}",
            text("waiting on background work", "等待后台")
        )),
        "{:?}",
        lines[3]
    );
    assert!(lines[4].contains("cmd1 · cmd1 标题"), "{:?}", lines[4]);
    assert!(
        !lines.iter().any(|line| line.contains("j1")),
        "镜像任务不该再单列一行: {lines:#?}"
    );
}

/// 点会话行是「切过去」（由 REPL 去做），点后台命令还是开日志面板——行内模式没有
/// 面板，这一下不吃。头上那行空的不算任何一条。
#[test]
fn clicking_a_session_row_asks_the_repl_to_switch() {
    let mut live = detached_tail();
    live.strip_sessions = vec![StripSession::Parent(parent()), child("c1", "running", None)];
    live.jobs = vec![job("cmd1", "command", None)];
    live.job_strip_start = 20;
    live.job_strip_rows = 4;

    assert_eq!(live.strip_index_at(20), None);
    assert_eq!(live.strip_index_at(21), Some(0));
    assert_eq!(live.strip_index_at(23), Some(2));
    assert_eq!(live.strip_index_at(24), None);

    assert!(live.activate_strip_row(0).unwrap());
    assert_eq!(live.take_strip_action(), Some(StripAction::Back));
    assert!(live.activate_strip_row(1).unwrap());
    assert_eq!(
        live.take_strip_action(),
        Some(StripAction::Visit("c1".into()))
    );
    assert!(!live.activate_strip_row(2).unwrap());
    assert_eq!(live.take_strip_action(), None);
}

/// 切进子会话之后，任务条第一行就是回去的路；多了一行要整个重画活动区。
#[test]
fn visiting_puts_the_way_back_on_the_strip() {
    let mut live = detached_tail();
    assert!(!live.set_jobs(Vec::new()));
    live.visits.push(parent());
    assert!(live.set_jobs(Vec::new()), "多了一行，活动区要整个重画");
    assert!(matches!(
        live.strip_sessions.as_slice(),
        [StripSession::Parent(row)] if row.session_id == "root"
    ));
    assert!(!live.set_jobs(Vec::new()), "没变就不重画");
    live.visits.clear();
    assert!(live.set_jobs(Vec::new()));
    assert!(live.strip_sessions.is_empty());
}

/// footer 上看得出切进了几层；窄屏时先裁模型名，层数留着。
#[test]
fn the_footer_says_how_deep_into_subagent_sessions_we_are() {
    let config = AppConfig::default();
    let footer = ReplFooterStatus::from_config(&config, 0, TurnTokens::default());
    let left = |badges: FooterBadges, width: usize| {
        strip_terminal_control_sequences(&repl_footer_left(
            PersonaLane::Active,
            badges,
            &footer,
            width,
        ))
    };
    let badge = |depth: usize| format!("{} ↳{depth}", text("subagent", "子代理"));
    let visiting = |depth| FooterBadges {
        readonly: false,
        visit_depth: depth,
    };

    assert!(!left(FooterBadges::default(), 120).contains('↳'));
    assert!(left(visiting(1), 120).contains(&badge(1)));
    let both = left(
        FooterBadges {
            readonly: true,
            visit_depth: 2,
        },
        120,
    );
    assert!(
        both.contains(text("read-only", "只读")) && both.contains(&badge(2)),
        "{both:?}"
    );
    assert!(
        left(visiting(1), 24).contains(&badge(1)),
        "{:?}",
        left(visiting(1), 24)
    );
}

/// 子代理会话的第一轮是主会话派的任务：回放画成「来自主会话的任务」那一块，不是用户
/// 气泡。
#[test]
fn a_replayed_parent_task_is_not_drawn_as_a_user_prompt() {
    let config = AppConfig::default();
    let task = miyu_core::state::TurnReplay {
        display_content: "去查一下日志".to_string(),
        assistant_content: "查完了。".to_string(),
        from_parent: true,
        ..Default::default()
    };
    let frame = session_replay_frame(&[task], PersonaLane::Active, &config, 80, false).unwrap();
    let frame = String::from_utf8_lossy(&frame);
    assert!(
        frame.contains(text("task from the main session", "来自主会话的任务")),
        "{frame}"
    );
    assert!(frame.contains("去查一下日志"), "{frame}");
    assert!(
        !frame.contains(&submitted_echo_bar(PersonaLane::Active)),
        "{frame}"
    );
}
