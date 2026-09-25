//! 断缓存记录（09-25）：按会话树计数，子代理的算进主会话；删会话连它的记录一起走。

use super::shared::*;
use crate::llm::{CacheBreak, CacheBreakCause};
use crate::state::*;

fn db() -> (tempfile::TempDir, ConversationDb) {
    let temp = tempfile::tempdir().unwrap();
    let db = ConversationDb::open(&test_paths(temp.path()).state_dir).unwrap();
    (temp, db)
}

fn entry(session: &str, lost: u64, cause: CacheBreakCause) -> CacheBreak {
    CacheBreak {
        session_id: session.into(),
        turn_id: None,
        lost_tokens: lost,
        cause,
        idle_secs: 0,
    }
}

#[test]
fn breaks_are_counted_over_the_session_tree_newest_first() {
    let (_temp, db) = db();
    let root = db
        .create_session("default", "主会话", "user", None, "")
        .unwrap()
        .session_id;
    let child = db
        .create_subagent_session("default", "调研", &root, "", 1, None, false)
        .unwrap()
        .session_id;
    let other = db
        .create_session("default", "别的会话", "user", None, "")
        .unwrap()
        .session_id;
    db.record_cache_break(&entry(&root, 13_000, CacheBreakCause::Provider))
        .unwrap();
    db.record_cache_break(&entry(
        &child,
        237_000,
        CacheBreakCause::Tools {
            changes: "+send_qq_message".into(),
        },
    ))
    .unwrap();
    db.record_cache_break(&entry(&other, 5_000, CacheBreakCause::SystemPrompt))
        .unwrap();

    assert_eq!(
        db.cache_break_count(&root).unwrap(),
        2,
        "子代理的算进主会话"
    );
    assert_eq!(db.cache_break_count(&child).unwrap(), 1);
    let recent = db.recent_cache_breaks(&root, 10).unwrap();
    assert_eq!(recent.len(), 2);
    assert_eq!(recent[0].lost_tokens, 237_000, "新的在前");
    assert!(matches!(recent[0].cause, CacheBreakCause::Tools { .. }));
    assert_eq!(recent[1].cause, CacheBreakCause::Provider);

    db.delete_session(&root).unwrap();
    assert_eq!(
        db.cache_break_count(&root).unwrap(),
        0,
        "删会话连记录一起走"
    );
    assert_eq!(db.cache_break_count(&other).unwrap(), 1);
}
