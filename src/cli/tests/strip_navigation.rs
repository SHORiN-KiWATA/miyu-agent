//! 方向键：命令候选与任务条（会话项目第 3 段，用户口径第 8 条）。真机走查见
//! `testkit/tui/strip_keys.py`。

use super::shared::detached_tail;
use crate::cli::repl::strip::*;
use crate::cli::repl::tail::Navigated;
use crate::cli::*;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use miyu_engine::tools::jobs::JobOverview;

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn press(live: &mut LiveReplTail, code: KeyCode) -> bool {
    matches!(live.navigate_key(&key(code)).unwrap(), Navigated::Done)
}

fn jobs(count: usize) -> Vec<JobOverview> {
    (0..count)
        .map(|index| JobOverview {
            job_id: format!("job{index}"),
            title: format!("任务{index}"),
            command: String::new(),
            kind: "command".into(),
            dev: false,
            session_id: None,
            root_session_id: None,
            status: "running".into(),
            running: true,
            runtime_seconds: 1,
            log_path: None,
            metric: None,
            metric_tokens: None,
            child_session_id: None,
        })
        .collect()
}

fn lines(live: &LiveReplTail) -> Vec<String> {
    strip_lines(&live.strip_rows(), 0, 80, live.strip_view())
        .iter()
        .map(|line| {
            strip_terminal_control_sequences(line)
                .trim_end()
                .to_string()
        })
        .collect()
}

/// 空输入框按 ↓ 进任务条；多了露不下的底下写「↓ 还有 x 个」，往下挪一条少一个；
/// 最上面一条再按 ↑ 回输入框。
#[test]
fn down_walks_into_the_strip_and_scrolls_it() {
    let mut live = detached_tail();
    live.set_jobs(jobs(7));

    let shown = lines(&live);
    assert_eq!(shown.len(), 1 + STRIP_VISIBLE_ROWS + 1, "{shown:#?}");
    assert!(
        shown.last().unwrap().starts_with('↓') && shown.last().unwrap().contains('2'),
        "{shown:#?}"
    );

    assert!(press(&mut live, KeyCode::Down));
    assert_eq!(live.strip_focus, Some(0));
    assert!(lines(&live)[1].starts_with('›'), "{:#?}", lines(&live));
    for _ in 0..5 {
        assert!(press(&mut live, KeyCode::Down));
    }
    assert_eq!(live.strip_focus, Some(5));
    assert_eq!(live.strip_scroll, 1, "挪出窗口就往下滚一条");
    let shown = lines(&live);
    assert!(
        shown.last().unwrap().contains('1'),
        "还剩一条没露出来: {shown:#?}"
    );
    assert!(press(&mut live, KeyCode::Down));
    assert!(press(&mut live, KeyCode::Down));
    assert_eq!(live.strip_focus, Some(6), "到底就停住");
    let shown = lines(&live);
    assert_eq!(
        shown.len(),
        1 + STRIP_VISIBLE_ROWS + 1,
        "滚到底那一行空着、不收，输入框不跳: {shown:#?}"
    );
    assert!(shown.last().unwrap().is_empty(), "{shown:#?}");

    for _ in 0..6 {
        assert!(press(&mut live, KeyCode::Up));
    }
    assert_eq!((live.strip_focus, live.strip_scroll), (Some(0), 0));
    assert!(press(&mut live, KeyCode::Up));
    assert_eq!(live.strip_focus, None, "顶上再按 ↑ 回输入框");
}

/// 翻上键历史时、光标不在最后一行时，↓ 还是编辑器的。
#[test]
fn down_stays_with_the_editor_while_browsing_history_or_mid_input() {
    let mut live = detached_tail();
    live.set_jobs(jobs(2));
    live.editor.history = vec![ReplHistoryEntry::plain("上一句")];
    live.editor.history_index = 0;
    live.editor.input = "上一句".into();
    live.editor.cursor = 3;
    live.editor.history_clean_index = Some(0);
    assert!(!press(&mut live, KeyCode::Down));
    assert_eq!(live.strip_focus, None);

    live.editor.history_clean_index = None;
    live.editor.input = "第一行\n第二行".into();
    live.editor.cursor = 1;
    assert!(
        !press(&mut live, KeyCode::Down),
        "光标在第一行，↓ 是往下挪光标"
    );
    live.editor.cursor = live.editor.input.chars().count();
    assert!(press(&mut live, KeyCode::Down));
    assert_eq!(live.strip_focus, Some(0));
}

/// 任务条上：回车是点它；Esc、打字都回输入框，打的字照常交给编辑器。
#[test]
fn enter_activates_and_other_keys_leave_the_strip() {
    let mut live = detached_tail();
    live.visits.push(ParentRow {
        session_id: "root".into(),
        title: "主会话标题".into(),
        root: true,
    });
    live.set_jobs(jobs(1));
    assert!(press(&mut live, KeyCode::Down));
    assert!(press(&mut live, KeyCode::Enter));
    assert_eq!(live.take_strip_action(), Some(StripAction::Back));
    assert_eq!(live.strip_focus, None);

    assert!(press(&mut live, KeyCode::Down));
    assert!(press(&mut live, KeyCode::Esc));
    assert_eq!(live.strip_focus, None);

    assert!(press(&mut live, KeyCode::Down));
    assert!(!press(&mut live, KeyCode::Char('a')), "字要交给编辑器");
    assert_eq!(live.strip_focus, None);
}

/// 命令候选开着时 ↑↓ 在候选里挑：Tab 补全挑中的；回车直接执行，要参数的补上空格
/// 等人接着打；输入一变挑的就作废。
#[test]
fn arrows_pick_from_the_command_list() {
    let mut live = detached_tail();
    live.set_jobs(jobs(1));
    live.editor.input = "/s".into();
    live.editor.cursor = 2;
    let names = miyu_core::slash_commands::repl_command_suggestions("/s");
    assert!(names.len() >= 2, "{names:?}");

    assert!(press(&mut live, KeyCode::Down));
    assert_eq!(live.command_pick(), Some(0));
    assert_eq!(live.strip_focus, None, "候选开着时 ↓ 不进任务条");
    assert!(press(&mut live, KeyCode::Down));
    assert_eq!(live.command_pick(), Some(1));
    assert!(press(&mut live, KeyCode::Tab));
    assert_eq!(live.editor.input, names[1]);
    assert_eq!(live.command_pick(), None);

    live.editor.input = "/us".into();
    assert!(press(&mut live, KeyCode::Down));
    assert!(
        !press(&mut live, KeyCode::Enter),
        "回车照常交给编辑器去执行"
    );
    assert_eq!(live.editor.input, "/usage");

    live.editor.input = "/ren".into();
    assert!(press(&mut live, KeyCode::Down));
    assert!(press(&mut live, KeyCode::Enter));
    assert_eq!(live.editor.input, "/rename ", "要参数的只补全，不执行");

    live.editor.input = "/s".into();
    assert!(press(&mut live, KeyCode::Up), "↑ 从没挑跳到最后一条");
    assert_eq!(live.command_pick(), Some(names.len() - 1));
    live.editor.input.push('e');
    assert_eq!(live.command_pick(), None, "输入变了，挑的作废");
}

/// 挑着往下走时候选面板跟着滚，挑中的那条一直露着、画成选中的样子。
#[test]
fn the_command_list_follows_the_pick() {
    let names = miyu_core::slash_commands::repl_command_suggestions("/");
    assert!(names.len() > crate::cli::repl::commands::COMMAND_HINT_ROWS);
    let last = names.len() - 1;
    let lines = command_hint_lines("/", 120, Some(last));
    assert_eq!(lines.len(), crate::cli::repl::commands::COMMAND_HINT_ROWS);
    let picked = lines.last().unwrap();
    assert!(
        strip_terminal_control_sequences(picked).starts_with(names[last]),
        "{lines:#?}"
    );
    assert!(picked.contains("\x1b[35m"), "选中项上色: {picked:?}");
    let plain = command_hint_lines("/", 120, None);
    assert!(strip_terminal_control_sequences(&plain[0]).starts_with(names[0]));
    assert!(!plain.iter().any(|line| line.contains("\x1b[35m")));
}
