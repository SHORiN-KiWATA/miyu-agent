//! 任务条上的会话行与切进子代理会话（会话项目第 3 段；09-26 改成从主会话画起的树）。真机
//! 走查见 `testkit/tui/subagent_visit.py`、`testkit/tui/strip_tree.py`。

use super::shared::detached_tail;
use crate::cli::footer::FooterBadges;
use crate::cli::repl::jobs::{format_job_duration, JOB_SPINNER_FRAMES};
use crate::cli::repl::strip::*;
use crate::cli::repl::strip_tree::{home_scroll, strip_items, StripScope};
use crate::cli::*;
use miyu_base::i18n::text;
use miyu_engine::tools::jobs::JobOverview;
use std::collections::HashMap;

fn job_in(id: &str, kind: &str, session: &str, metric: Option<&str>) -> JobOverview {
    JobOverview {
        job_id: id.into(),
        title: format!("{id} 标题"),
        command: String::new(),
        kind: kind.into(),
        dev: false,
        session_id: Some(session.into()),
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

fn agent_row(id: &str, state: &str, job_id: Option<&str>, below: u64) -> SubagentRow {
    SubagentRow {
        session_id: id.into(),
        title: format!("查{id}"),
        state: state.into(),
        dev: false,
        job_id: job_id.map(str::to_string),
        running_descendants: below,
    }
}

fn root() -> ParentRow {
    ParentRow {
        session_id: "root".into(),
        title: "修登录页".into(),
        root: true,
    }
}

fn step(id: &str) -> ParentRow {
    ParentRow {
        session_id: id.into(),
        title: format!("查{id}"),
        root: false,
    }
}

fn tree(entries: &[(&str, Vec<SubagentRow>)]) -> HashMap<String, Vec<SubagentRow>> {
    entries
        .iter()
        .map(|(owner, rows)| (owner.to_string(), rows.clone()))
        .collect()
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

fn draw(items: &[StripItem], view: StripView) -> Vec<String> {
    plain(strip_lines(items, 0, 80, view))
}

fn home(items: &[StripItem]) -> StripView {
    StripView {
        pinned: crate::cli::repl::strip_tree::pinned_rows(items),
        scroll: home_scroll(items),
        ..StripView::default()
    }
}

fn plus(n: u64) -> String {
    if miyu_base::i18n::is_zh() {
        format!("（+{n}）")
    } else {
        format!(" (+{n})")
    }
}

/// 会话行按树上的顺序是哪几条、哪条是正在看的。
fn sessions(items: &[StripItem]) -> Vec<(String, bool)> {
    items
        .iter()
        .filter_map(|item| Some((item.session_id()?.to_string(), item.is_current())))
        .collect()
}

/// 主会话里（没在访问）：第一层只列它自己的子代理和命令。子代理空心 `○`，名下还在跑的收成
/// 「（+N）」（用户 09-25：`开发中（+3）`）；后台命令照旧转轮；后台子代理的镜像任务不单列，量
/// 和用时挂到会话那一行上；后代的命令不在第一层（原来挂个 `↳` 列出来），跑完的子代理不列。
#[test]
fn the_main_session_lists_its_own_rows_and_folds_the_rest() {
    let children = tree(&[(
        "root",
        vec![
            agent_row("c1", "running", Some("j1"), 3),
            agent_row("c2", "waiting", None, 0),
            agent_row("done", "done", None, 0),
        ],
    )]);
    let jobs = vec![
        job_in("j1", "subagent", "root", Some("≈3.1K")),
        job_in("cmd1", "command", "root", None),
        job_in("deep", "command", "c1", None),
    ];
    let items = strip_items(
        &StripScope {
            current: Some("root"),
            children: Some(&children),
            ..Default::default()
        },
        &jobs,
    );
    let lines = draw(&items, StripView::default());

    assert_eq!(lines.len(), 4, "{lines:#?}");
    assert!(lines[0].is_empty(), "头上一行空的: {lines:#?}");
    let spinner = JOB_SPINNER_FRAMES[0];
    assert!(
        lines[1].starts_with(&format!("  ○ {}{}", text("agent", "子代理"), plus(3))),
        "{:?}",
        lines[1]
    );
    assert!(
        lines[1].contains("查c1")
            && lines[1].ends_with(&format!("≈3.1K  {}", format_job_duration(65))),
        "镜像任务的量和用时挂到会话行上: {:?}",
        lines[1]
    );
    assert!(
        lines[2].starts_with("  ○ ")
            && lines[2].ends_with(&format!(
                "查c2 · {}",
                text("waiting on background work", "等待后台")
            )),
        "{:?}",
        lines[2]
    );
    assert!(
        lines[3].starts_with(&format!("  {spinner} ")) && lines[3].contains("cmd1 · cmd1 标题"),
        "{:?}",
        lines[3]
    );
    let joined = lines.join("\n");
    assert!(!joined.contains("j1"), "镜像任务不该再单列一行: {lines:#?}");
    assert!(!joined.contains("deep"), "后代的命令收进（+N）: {lines:#?}");
    assert!(!joined.contains("查done"), "{lines:#?}");
    let title_col = |line: &str| visible_width(&line[..line.find("查").unwrap()]);
    assert_eq!(
        title_col(&lines[1]),
        title_col(&lines[2]),
        "带（+N）的那行和别的行标题竖着对齐: {lines:#?}"
    );
    // 点进去：访问栈就是主会话这一层（标题由切的那一头补）。原来这里是空的，切进去之后它被当成
    // 主会话，任务条上没有「主会话」那一行、footer 也没有层数（09-26 走查）。
    assert_eq!(
        items[0].action(),
        Some(StripAction::Go {
            session_id: "c1".into(),
            path: vec![ParentRow {
                session_id: "root".into(),
                title: String::new(),
                root: true,
            }],
        })
    );
}

/// 在子代理会话里（用户 09-26 照 Claude Code 定的样子）：`○ 主会话` 在最上面；主会话的子代理
/// 里正在看的这条实心 `●`、不挂（+N），它名下的子代理和命令用 `├`/`└` 挂在它下面；别的空心、
/// 挂（+N）；主会话自己的命令列在第一层。点哪条就切到哪条，访问栈跟着换成它的那一串祖先。
#[test]
fn inside_a_subagent_the_tree_starts_at_the_main_session() {
    let children = tree(&[
        (
            "root",
            vec![
                agent_row("c1", "running", None, 2),
                agent_row("c2", "running", Some("jc2"), 0),
                agent_row("c3", "running", None, 1),
            ],
        ),
        ("c2", vec![agent_row("g1", "running", None, 1)]),
    ]);
    let jobs = vec![
        job_in("jc2", "subagent", "root", None),
        job_in("gcmd", "command", "c2", None),
        job_in("rootcmd", "command", "root", None),
    ];
    let path = [root()];
    let items = strip_items(
        &StripScope {
            current: Some("c2"),
            path: &path,
            children: Some(&children),
        },
        &jobs,
    );
    let lines = draw(&items, home(&items));
    let spinner = JOB_SPINNER_FRAMES[0];

    assert!(
        lines[1].starts_with(&format!("  ○ {}", text("main", "主会话")))
            && lines[1].ends_with("修登录页"),
        "{lines:#?}"
    );
    assert!(
        lines[2].starts_with("  ○ ") && lines[2].contains(&plus(2)),
        "{lines:#?}"
    );
    assert!(
        lines[3].starts_with(&format!("  ● {} ", text("agent", "子代理")))
            && lines[3].contains("查c2")
            && !lines[3].contains("(+")
            && !lines[3].contains("（+"),
        "{lines:#?}"
    );
    assert!(
        lines[4].starts_with("  ├ ○ ") && lines[4].contains("查g1") && lines[4].contains(&plus(1)),
        "{lines:#?}"
    );
    assert!(
        lines[5].starts_with(&format!("  └ {spinner} ")) && lines[5].contains("gcmd"),
        "{lines:#?}"
    );
    assert!(
        lines[6].starts_with("↓"),
        "五行露不下，底下说还有几个: {lines:#?}"
    );
    assert_eq!(items.len(), 7, "c3 和主会话自己的命令在下面: {items:#?}");
    assert!(matches!(&items[6], StripItem::Job { job, depth: 0, .. } if job.job_id == "rootcmd"));

    let go = |id: &str, path: Vec<ParentRow>| {
        Some(StripAction::Go {
            session_id: id.into(),
            path,
        })
    };
    assert_eq!(items[0].action(), go("root", Vec::new()));
    assert_eq!(items[1].action(), go("c1", vec![root()]));
    assert_eq!(items[2].action(), Some(StripAction::Stay));
    assert_eq!(items[3].action(), go("g1", vec![root(), step("c2")]));
    assert_eq!(items[4].action(), None, "命令行点开的是日志面板");
}

/// 切到孙代理还是同一棵树：从主会话画起，路上的子代理照样展开，只是实心圆挪到孙代理上（用户
/// 09-26：原来树根跟着父会话走，切到孙代理只剩「上一层」和它自己）。孙代理自己的命令挂在它下面，
/// 父辈那一列的竖线接着画。
#[test]
fn switching_to_a_grandchild_keeps_the_same_tree() {
    let children = tree(&[
        (
            "root",
            vec![
                agent_row("c1", "running", None, 0),
                agent_row("c2", "running", None, 2),
                agent_row("c3", "running", None, 0),
            ],
        ),
        (
            "c2",
            vec![
                agent_row("g1", "running", None, 0),
                agent_row("g2", "running", None, 0),
            ],
        ),
    ]);
    let in_child = [root()];
    let at_child = strip_items(
        &StripScope {
            current: Some("c2"),
            path: &in_child,
            children: Some(&children),
        },
        &[],
    );
    let in_grandchild = [root(), step("c2")];
    let at_grandchild = strip_items(
        &StripScope {
            current: Some("g1"),
            path: &in_grandchild,
            children: Some(&children),
        },
        &[],
    );

    let rows = |items: &[StripItem]| {
        sessions(items)
            .into_iter()
            .map(|(id, _)| id)
            .collect::<Vec<_>>()
    };
    assert_eq!(rows(&at_child), rows(&at_grandchild), "{at_grandchild:#?}");
    let current = |items: &[StripItem]| {
        sessions(items)
            .into_iter()
            .find(|(_, current)| *current)
            .map(|(id, _)| id)
    };
    assert_eq!(current(&at_child).as_deref(), Some("c2"));
    assert_eq!(current(&at_grandchild).as_deref(), Some("g1"));
    let lines = draw(&at_grandchild, home(&at_grandchild));
    let line = |id: &str| {
        lines
            .iter()
            .find(|line| line.contains(&format!("查{id}")))
            .cloned()
            .unwrap_or_default()
    };
    assert!(
        line("c2").starts_with("  ○ ") && !line("c2").contains("（+"),
        "{lines:#?}"
    );
    assert!(line("g1").starts_with("  ├ ● "), "{lines:#?}");
    assert!(line("g2").starts_with("  └ ○ "), "{lines:#?}");

    let with_command = strip_items(
        &StripScope {
            current: Some("g1"),
            path: &in_grandchild,
            children: Some(&children),
        },
        &[job_in("gcmd", "command", "g1", None)],
    );
    let lines = draw(&with_command, home(&with_command));
    let spinner = JOB_SPINNER_FRAMES[0];
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with(&format!("  │ └ {spinner} ")) && line.contains("gcmd")),
        "孙代理的命令挂在它下面，c2 那一列的竖线接着画: {lines:#?}"
    );
}

/// 露不下时：主会话那一行钉在顶上，下面那一截停在露出正在看的这条和挂在它下面的地方
/// （用户 09-26 拍板），兄弟的顺序不变。
#[test]
fn the_way_back_stays_pinned_while_the_rest_scrolls_to_the_current_one() {
    let children = tree(&[
        (
            "root",
            (1..=5)
                .map(|n| agent_row(&format!("c{n}"), "running", None, 0))
                .collect(),
        ),
        (
            "c4",
            vec![
                agent_row("g1", "running", None, 0),
                agent_row("g2", "running", None, 0),
            ],
        ),
    ]);
    let path = [root()];
    let items = strip_items(
        &StripScope {
            current: Some("c4"),
            path: &path,
            children: Some(&children),
        },
        &[],
    );
    let view = home(&items);
    let lines = draw(&items, view);

    assert_eq!(view.visible(items.len()), vec![0, 3, 4, 5, 6], "{lines:#?}");
    assert!(lines[1].contains(text("main", "主会话")), "{lines:#?}");
    assert!(lines[2].contains("查c3"), "{lines:#?}");
    assert!(
        lines[3].starts_with("  ● ") && lines[3].contains("查c4"),
        "{lines:#?}"
    );
    assert!(
        lines[4].contains("├ ○") && lines[4].contains("查g1"),
        "{lines:#?}"
    );
    assert!(
        lines[5].contains("└ ○") && lines[5].contains("查g2"),
        "{lines:#?}"
    );
    assert!(
        lines[6].starts_with("↓") && lines[6].contains('1'),
        "{lines:#?}"
    );
}

/// 点任务条：钉住的那一行和滚动那一截各对各的；头上那行空的、底下「↓ 还有」都不算哪一条。
/// 点会话行是「切过去」（由 REPL 去做），点后台命令还是开日志面板——行内模式没有面板，
/// 这一下不吃。
#[test]
fn clicks_land_on_the_rows_that_are_showing() {
    let mut live = detached_tail();
    live.visits.push(root());
    let children = tree(&[
        (
            "root",
            (1..=5)
                .map(|n| agent_row(&format!("c{n}"), "running", None, 0))
                .collect(),
        ),
        (
            "c4",
            vec![
                agent_row("g1", "running", None, 0),
                agent_row("g2", "running", None, 0),
            ],
        ),
    ]);
    live.strip_items = strip_items(
        &StripScope {
            current: Some("c4"),
            path: &live.visits,
            children: Some(&children),
        },
        &[job_in("cmd1", "command", "root", None)],
    );
    live.job_strip_start = 20;
    live.job_strip_rows = 7;

    assert_eq!(live.strip_index_at(20), None);
    assert_eq!(live.strip_index_at(21), Some(0), "钉住的主会话那一行");
    assert_eq!(live.strip_index_at(22), Some(3), "滚动那一截从 c3 露起");
    assert_eq!(live.strip_index_at(25), Some(6));
    assert_eq!(live.strip_index_at(26), None, "「↓ 还有」那一行");

    assert!(live.activate_strip_row(0).unwrap());
    assert_eq!(
        live.take_strip_action(),
        Some(StripAction::Go {
            session_id: "root".into(),
            path: Vec::new()
        })
    );
    assert!(live.activate_strip_row(3).unwrap());
    assert_eq!(
        live.take_strip_action(),
        Some(StripAction::Go {
            session_id: "c3".into(),
            path: vec![root()]
        })
    );
    assert!(live.activate_strip_row(5).unwrap());
    assert_eq!(
        live.take_strip_action(),
        Some(StripAction::Go {
            session_id: "g1".into(),
            path: vec![root(), step("c4")]
        })
    );
    let last = live.strip_items.len() - 1;
    assert!(
        !live.activate_strip_row(last).unwrap(),
        "行内模式没有日志面板"
    );
    assert_eq!(live.take_strip_action(), None);
}

/// Ctrl+C 第三级认的「这条会话名下的后台活」：没在访问时是任务条上的每一行；在子代理会话
/// 里只是挂在它下面的那几行——主会话自己的命令不算，停完也只压这几条。
#[test]
fn background_work_is_what_hangs_under_the_current_session() {
    let mut live = detached_tail();
    let top = tree(&[]);
    live.strip_items = strip_items(
        &StripScope {
            current: Some("root"),
            children: Some(&top),
            ..Default::default()
        },
        &[job_in("rootcmd", "command", "root", None)],
    );
    assert!(live.has_background_work());
    assert_eq!(live.background_job_ids(), ["rootcmd"]);

    live.visits.push(root());
    let children = tree(&[("root", vec![agent_row("c1", "running", Some("jc1"), 0)])]);
    let jobs = [
        job_in("jc1", "subagent", "root", None),
        job_in("rootcmd", "command", "root", None),
    ];
    live.strip_items = strip_items(
        &StripScope {
            current: Some("c1"),
            path: &live.visits,
            children: Some(&children),
        },
        &jobs,
    );
    assert!(
        !live.has_background_work(),
        "主会话的命令不是它的: {:#?}",
        live.strip_items
    );
    assert!(live.background_job_ids().is_empty());

    let jobs = [
        job_in("jc1", "subagent", "root", None),
        job_in("rootcmd", "command", "root", None),
        job_in("own", "command", "c1", None),
    ];
    live.strip_items = strip_items(
        &StripScope {
            current: Some("c1"),
            path: &live.visits,
            children: Some(&children),
        },
        &jobs,
    );
    assert!(live.has_background_work());
    assert_eq!(live.background_job_ids(), ["own"]);
}

/// 切进子会话之后，任务条第一行就是主会话那一行；多了一行要整个重画活动区。
#[test]
fn visiting_puts_the_way_back_on_the_strip() {
    let mut live = detached_tail();
    assert!(!live.set_jobs(Vec::new()));
    live.visits.push(root());
    assert!(live.set_jobs(Vec::new()), "多了一行，活动区要整个重画");
    assert!(matches!(
        live.strip_items.as_slice(),
        [StripItem::Root(row)] if row.session_id == "root"
    ));
    assert!(!live.set_jobs(Vec::new()), "没变就不重画");
    live.visits.clear();
    assert!(live.set_jobs(Vec::new()));
    assert!(live.strip_items.is_empty());
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
        cache_breaks: 0,
    };

    assert!(!left(FooterBadges::default(), 120).contains('↳'));
    assert!(left(visiting(1), 120).contains(&badge(1)));
    let both = left(
        FooterBadges {
            readonly: true,
            visit_depth: 2,
            cache_breaks: 0,
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

/// 方向键停着的那一行，`›` 占行首单独留出来的两列，记号照常在它后面（用户 09-25：原来 `›`
/// 直接顶掉转轮）。别的行那两列空着，竖着对齐。
#[test]
fn the_arrow_takes_its_own_column_and_keeps_the_marker() {
    let items = strip_items(
        &StripScope::default(),
        &[
            job_in("cmd1", "command", "root", None),
            job_in("cmd2", "command", "root", None),
        ],
    );
    let lines = draw(
        &items,
        StripView {
            focused: Some(1),
            ..StripView::default()
        },
    );
    let spinner = JOB_SPINNER_FRAMES[0];
    assert!(
        lines[1].starts_with(&format!("  {spinner} ")),
        "{:?}",
        lines[1]
    );
    assert!(
        lines[2].starts_with(&format!("› {spinner} ")),
        "{:?}",
        lines[2]
    );
}
