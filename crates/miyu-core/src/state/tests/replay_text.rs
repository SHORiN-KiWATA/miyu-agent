//! 长回复切走再切回来要画全（会话项目第 2 段的补丁，09-25）。
//!
//! 跑完的轮把流水收成 `turns.replay_journal`，原来正文每段截到 2048 字、整轮 8 KB：一段
//! 3000 多字的回复重开之后只剩前 2048 字加一个「…」，最后那句没了。正文是人要读的那部分，
//! 不截；预算只压工具和思考那几条。

use super::shared::*;
use crate::state::*;

/// 整轮预算（`REPLAY_JOURNAL_MAX_CHARS`，模块私有，这里照抄）。
const TURN_BUDGET: usize = 8 * 1024;

fn long_reply() -> String {
    let body: String = (0..300)
        .map(|index| format!("长回复第{index:04}行\n"))
        .collect();
    body + "长回复到此结束"
}

fn last_text(replay: &TurnReplay) -> &str {
    match replay.entries.last() {
        Some(ReplayEntry::Text { text }) => text,
        other => panic!("最后一条不是正文：{other:?}"),
    }
}

fn append(
    store: &StateStore,
    kind: &str,
    call_id: Option<&str>,
    name: Option<&str>,
    payload: &str,
) {
    let ok = (kind == "tool_result").then_some(true);
    store
        .conv_db()
        .append_turn_journal_event("t1", 0, 0, kind, call_id, name, Some(payload), None, ok)
        .unwrap();
}

/// 流式时正文是一小块一小块落进流水的。
fn stream_reply(store: &StateStore, reply: &str) {
    let chars: Vec<char> = reply.chars().collect();
    for chunk in chars.chunks(40) {
        append(
            store,
            "assistant_content",
            None,
            None,
            &chunk.iter().collect::<String>(),
        );
    }
}

#[test]
fn a_long_reply_replays_whole() {
    let (_temp, store) = test_store();
    store.init_files().unwrap();
    store.start_turn("t1", "写长一点", 999_999).unwrap();
    let reply = long_reply();
    assert!(reply.chars().count() > 3000);
    stream_reply(&store, &reply);
    store.complete_turn("t1", &reply, None).unwrap();

    let replays = store.session_replay(5).unwrap();
    assert_eq!(last_text(&replays[0]), reply);
}

/// 工具那几条照旧压在整轮预算里、从最早的丢起：留下来的是这一轮收尾时人正看着的那一截。
#[test]
fn tool_entries_still_fit_the_turn_budget() {
    let (_temp, store) = test_store();
    store.init_files().unwrap();
    store
        .start_turn("t1", "跑二十条命令再总结", 999_999)
        .unwrap();
    for index in 0..20 {
        let call = format!("c{index}");
        append(
            &store,
            "tool_call",
            Some(&call),
            Some("run_command"),
            &format!("{{\"command\":\"echo {index}\"}}"),
        );
        append(
            &store,
            "tool_result",
            Some(&call),
            None,
            &format!("{index}:{}", "x".repeat(3000)),
        );
    }
    let reply = long_reply();
    stream_reply(&store, &reply);
    store.complete_turn("t1", &reply, None).unwrap();

    let replays = store.session_replay(5).unwrap();
    let entries = &replays[0].entries;
    assert_eq!(last_text(&replays[0]), reply);
    let tools: Vec<&ReplayEntry> = entries
        .iter()
        .filter(|entry| !matches!(entry, ReplayEntry::Text { .. }))
        .collect();
    assert!(serde_json::to_string(&tools).unwrap().len() <= TURN_BUDGET);
    assert!(
        matches!(tools.last(), Some(ReplayEntry::ToolResult { output, .. }) if output.starts_with("19:")),
        "最后一条命令的结果该留着：{:?}",
        tools.last()
    );
}

/// 这次改动之前存下的快照：正文截在 2048 字、后面一个「…」。最后一段就是交出去的回复时，
/// 照 `assistant_content` 补回全文。
#[test]
fn an_old_clipped_snapshot_heals_from_the_reply() {
    let (temp, store) = test_store();
    store.init_files().unwrap();
    store.start_turn("t1", "写长一点", 999_999).unwrap();
    let reply = long_reply();
    store.complete_turn("t1", &reply, None).unwrap();
    let clipped: String = reply.chars().take(2048).collect::<String>() + "…";
    let snapshot = serde_json::to_string(&vec![ReplayEntry::Text { text: clipped }]).unwrap();
    let conn = rusqlite::Connection::open(find_db(temp.path()).unwrap()).unwrap();
    conn.execute(
        "UPDATE turns SET replay_journal = ?1 WHERE turn_id = 't1'",
        rusqlite::params![snapshot],
    )
    .unwrap();

    let replays = store.session_replay(5).unwrap();
    assert_eq!(last_text(&replays[0]), reply);
}

/// 补全只认得出「正好截在 2048 字」的那种：短回复自己以「…」结尾、或者截断那段不是这一轮
/// 最后交出去的回复（中间正文），都原样留着。
#[test]
fn only_the_old_clamp_is_healed() {
    let (temp, store) = test_store();
    store.init_files().unwrap();
    store.start_turn("t1", "说一半", 999_999).unwrap();
    store
        .complete_turn("t1", "话说到一半…后面还有", None)
        .unwrap();
    let conn = rusqlite::Connection::open(find_db(temp.path()).unwrap()).unwrap();
    let short = serde_json::to_string(&vec![ReplayEntry::Text {
        text: "话说到一半…".to_string(),
    }])
    .unwrap();
    conn.execute(
        "UPDATE turns SET replay_journal = ?1 WHERE turn_id = 't1'",
        rusqlite::params![short],
    )
    .unwrap();
    let replays = store.session_replay(5).unwrap();
    assert_eq!(last_text(&replays[0]), "话说到一半…");

    let other: String = "中间正文"
        .repeat(600)
        .chars()
        .take(2048)
        .collect::<String>()
        + "…";
    let snapshot = serde_json::to_string(&vec![ReplayEntry::Text {
        text: other.clone(),
    }])
    .unwrap();
    conn.execute(
        "UPDATE turns SET replay_journal = ?1 WHERE turn_id = 't1'",
        rusqlite::params![snapshot],
    )
    .unwrap();
    let replays = store.session_replay(5).unwrap();
    assert_eq!(last_text(&replays[0]), other);
}
