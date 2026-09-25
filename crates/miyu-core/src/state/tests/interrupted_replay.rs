//! 被中断的回合：已经说出口的话要留得住。
//!
//! 用户 09-21：关掉 TUI 之后重开，那一轮只剩一句「已中断」，可 `turn_journal_events`
//! 里 5158 条事件一条不少。两处漏：`interrupt_turn` 从不存回放快照（跑完的两条路
//! 都存），而 `assistant_content` 的投影只取**最后一段**——一轮里每被插一条排队
//! 消息就切一段，前面几段说的话就此蒸发。

use super::shared::*;
use crate::state::*;
use std::path::Path;

/// 再开一段流水账。真实场景里这是排队消息被消费时切的（`consume_queued_prompts`）——
/// 后台任务的报告插进正在跑的那一轮就会切一段。这里直接插行，免得为了造一个段
/// 把整套排队流程都搭一遍。
fn open_segment(store: &StateStore, temp: &Path, turn_id: &str, segment_index: i64) {
    let path = find_db(temp).expect("找不到会话库");
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute(
        "UPDATE turn_journal_segments SET status = 'completed'
          WHERE turn_id = ?1 AND revision = 0 AND segment_index = ?2",
        rusqlite::params![turn_id, segment_index - 1],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO turn_journal_segments
            (turn_id, revision, segment_index, status, started_at)
         VALUES (?1, 0, ?2, 'running', ?3)",
        rusqlite::params![turn_id, segment_index, chrono::Utc::now().to_rfc3339()],
    )
    .unwrap();
    let _ = store;
}

#[test]
fn an_interrupted_turn_keeps_the_prose_from_every_segment() {
    let (temp, store) = test_store();
    store.init_files().unwrap();
    store.start_turn("t1", "长活", 999_999).unwrap();
    let db = store.conv_db();
    db.append_turn_journal_event(
        "t1",
        0,
        0,
        "assistant_content",
        None,
        None,
        Some("第一段就说了这句。"),
        None,
        None,
    )
    .unwrap();
    open_segment(&store, temp.path(), "t1", 1);
    db.append_turn_journal_event(
        "t1",
        0,
        1,
        "assistant_content",
        None,
        None,
        Some("第二段接着说。"),
        None,
        None,
    )
    .unwrap();
    store.interrupt_turn("t1").unwrap();

    let turn = store.load_turns().unwrap().remove(0);
    assert_eq!(turn.status, TurnStatus::Interrupted);
    assert!(
        turn.assistant_content.contains("第一段就说了这句。"),
        "第一段的正文丢了：{}",
        turn.assistant_content
    );
    assert!(
        turn.assistant_content.contains("第二段接着说。"),
        "第二段的正文丢了：{}",
        turn.assistant_content
    );
}

#[test]
fn an_interrupted_turn_snapshots_its_replay_journal() {
    let (_temp, store) = test_store();
    store.init_files().unwrap();
    store.start_turn("t1", "长活", 999_999).unwrap();
    let db = store.conv_db();
    for (kind, call_id, name, payload, ok) in [
        ("assistant_reasoning", None, None, Some("先想一句。"), None),
        ("assistant_content", None, None, Some("这就去做。"), None),
        (
            "tool_call",
            Some("c1"),
            Some("run_command"),
            Some("{}"),
            None,
        ),
        ("tool_result", Some("c1"), None, Some("跑完了"), Some(true)),
    ] {
        db.append_turn_journal_event("t1", 0, 0, kind, call_id, name, payload, None, ok)
            .unwrap();
    }
    store.interrupt_turn("t1").unwrap();

    let replays = store.session_replay(5).unwrap();
    assert_eq!(replays.len(), 1);
    assert!(replays[0].interrupted);
    let entries = &replays[0].entries;
    assert!(
        entries
            .iter()
            .any(|entry| matches!(entry, ReplayEntry::Reasoning { .. })),
        "思考没进回放：{entries:?}"
    );
    assert!(
        entries
            .iter()
            .any(|entry| matches!(entry, ReplayEntry::ToolResult { .. })),
        "工具结果没进回放：{entries:?}"
    );
    assert!(entries.iter().any(|entry| matches!(
        entry,
        ReplayEntry::Text { text } if text.contains("这就去做。")
    )));
}

/// 这次改动**之前**被中断的那些轮：库里 `replay_journal` 是空的，流水账却还在。
/// 回放要当场收一份，不然用户已有的会话永远补不回来。
#[test]
fn an_old_interrupted_turn_recovers_its_replay_from_the_journal() {
    let (temp, store) = test_store();
    store.init_files().unwrap();
    store.start_turn("t1", "长活", 999_999).unwrap();
    let db = store.conv_db();
    db.append_turn_journal_event(
        "t1",
        0,
        0,
        "assistant_content",
        None,
        None,
        Some("中断之前说过的话。"),
        None,
        None,
    )
    .unwrap();
    store.interrupt_turn("t1").unwrap();
    // 倒回老样子：快照抹掉，事件留着。
    let path = find_db(temp.path()).expect("找不到会话库");
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute("UPDATE turns SET replay_journal = NULL", [])
        .unwrap();
    drop(conn);

    let replays = store.session_replay(5).unwrap();
    assert_eq!(replays.len(), 1);
    assert!(
        replays[0].entries.iter().any(|entry| matches!(
            entry,
            ReplayEntry::Text { text } if text.contains("中断之前说过的话。")
        )),
        "老中断轮没能从流水账里补回来：{:?}",
        replays[0].entries
    );
}

/// daemon 换了进程（重启、崩溃）之后，上一条 daemon 跑到一半的回合走的是
/// `recover_stale_running_turns`——用户 09-21 那两轮的 `owner_pid` 各不相同，
/// 撞的就是这条路，而不是谁按了取消。它和 `interrupt_turn` 要给出同样的结果。
#[test]
fn a_turn_orphaned_by_a_daemon_restart_keeps_its_transcript() {
    let (_temp, store) = test_store();
    store.init_files().unwrap();
    store.start_turn("t1", "长活", 999_999).unwrap();
    let db = store.conv_db();
    for (kind, call_id, name, payload, ok) in [
        ("assistant_reasoning", None, None, Some("想一想。"), None),
        (
            "assistant_content",
            None,
            None,
            Some("正在做这件事。"),
            None,
        ),
        (
            "tool_call",
            Some("c1"),
            Some("run_command"),
            Some("{}"),
            None,
        ),
        ("tool_result", Some("c1"), None, Some("跑完了"), Some(true)),
    ] {
        db.append_turn_journal_event("t1", 0, 0, kind, call_id, name, payload, None, ok)
            .unwrap();
    }
    // 属主换人。清成 NULL 表示「属主不在了」——死掉的 pid 走的是同一条
    // `alive = false` 分支，而挑一个具体的号码要么可能撞上活进程（大数），
    // 要么根本不算死（0 号是进程组，`kill(0, 0)` 会成功）。
    let path = find_db(_temp.path()).expect("找不到会话库");
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute("UPDATE turns SET owner_pid = NULL", [])
        .unwrap();
    drop(conn);

    let recovered = store.recover_stale_turns().unwrap();
    assert_eq!(recovered, 1, "陈旧轮没被认出来");

    let replays = store.session_replay(5).unwrap();
    assert_eq!(replays.len(), 1);
    assert!(replays[0].interrupted);
    assert!(
        replays[0].entries.iter().any(|entry| matches!(
            entry,
            ReplayEntry::Text { text } if text.contains("正在做这件事。")
        )),
        "daemon 重启把过程吃掉了：{:?}",
        replays[0].entries
    );
    assert!(
        replays[0]
            .entries
            .iter()
            .any(|entry| matches!(entry, ReplayEntry::ToolResult { .. })),
        "工具那一步没留住：{:?}",
        replays[0].entries
    );
}
