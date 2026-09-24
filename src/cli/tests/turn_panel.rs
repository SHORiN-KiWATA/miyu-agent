//! 回合里开的面板是活动区上的一层（会话项目第 3 段，B4）：按键归它，鼠标照旧归回合循环，
//! 交出结果时收掉。画面见走查 `testkit/tui/panel_streams.py`。

use super::shared::detached_tail;
use crate::cli::repl::midturn_panel::{PanelDone, TurnPanel};
use crate::cli::*;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn sessions() -> Vec<SessionListEntry> {
    ["甲", "乙", "丙"]
        .iter()
        .enumerate()
        .map(|(index, name)| {
            session_list_entry(&serde_json::json!({
                "session_id": format!("s{index}"),
                "name": name,
            }))
        })
        .collect()
}

#[test]
fn keys_go_to_the_open_panel_and_mouse_does_not() {
    let mut live = detached_tail();
    let mouse = Event::Mouse(MouseEvent {
        kind: MouseEventKind::ScrollUp,
        column: 0,
        row: 0,
        modifiers: KeyModifiers::NONE,
    });
    assert!(
        !live.turn_panel_takes(&key(KeyCode::Down)),
        "没开面板时按键不归它"
    );

    live.open_turn_panel(TurnPanel::Session {
        picker: crate::cli::repl::session_picker::SessionPicker::new(sessions(), "s0", None),
    })
    .unwrap();
    assert!(live.turn_panel_takes(&key(KeyCode::Down)));
    assert!(live.turn_panel_takes(&Event::Paste("粘贴".into())));
    assert!(!live.turn_panel_takes(&mouse), "滚轮照旧翻正文");

    assert!(live
        .turn_panel_event(&key(KeyCode::Down))
        .unwrap()
        .is_none());
    match live.turn_panel_event(&key(KeyCode::Enter)).unwrap() {
        Some(PanelDone::Session(SessionPick::Switch(miyu_core::ipc::SessionRef::Id { id }))) => {
            assert_eq!(id, "s1")
        }
        _ => panic!("回车该交出挑中的那条会话"),
    }
    assert!(live.close_turn_panel().unwrap().is_some());
    assert!(live.turn_panel.is_none());
    assert!(!live.turn_panel_takes(&key(KeyCode::Down)));
}

/// 开面板时任务条上停着的方向键焦点收回来：面板开着时任务条不在屏上。
#[test]
fn opening_a_panel_leaves_the_strip() {
    let mut live = detached_tail();
    live.strip_focus = Some(0);
    live.open_turn_panel(TurnPanel::Session {
        picker: crate::cli::repl::session_picker::SessionPicker::new(sessions(), "s0", None),
    })
    .unwrap();
    assert_eq!(live.strip_focus, None);
}
