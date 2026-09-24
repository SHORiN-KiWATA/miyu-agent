//! 断点续跑的认领（09-24）：进程死掉的回合收尾时记一笔，人按停止的不记；结案后不再
//! 翻出来；关停时只记用量、状态留在「执行中」。

use super::shared::*;
use crate::llm::TurnTokens;
use crate::state::*;

/// 没人认领的 pid：`recover_stale_turns` 判它已死。
const DEAD_PID: u32 = 999_999;

fn turn<'a>(turns: &'a [Turn], turn_id: &str) -> &'a Turn {
    turns
        .iter()
        .find(|turn| turn.turn_id == turn_id)
        .expect("turn exists")
}

/// 退回修复前（收尾不记认领）这条会红：进程死掉的回合收成「已中断」后，再也分不出
/// 它是被重启打断的。
#[test]
fn a_turn_whose_process_died_leaves_a_resume_claim() {
    let (_temp, store) = test_store();
    store
        .start_turn("turn_dead", "build the thing", DEAD_PID)
        .unwrap();
    store
        .append_turn_journal_event(
            "turn_dead",
            0,
            0,
            "assistant_content",
            None,
            None,
            Some("working on it"),
            None,
            None,
        )
        .unwrap();

    assert_eq!(store.recover_stale_turns().unwrap(), 1);

    let orphans = store.pending_restart_orphans().unwrap();
    assert_eq!(orphans.len(), 1, "{orphans:?}");
    let orphan = &orphans[0];
    assert_eq!(orphan.turn_id, "turn_dead");
    assert_eq!(orphan.session_id, *store.session_id());
    assert_eq!(orphan.user_content, "build the thing");
    let last_activity = orphan.last_activity.expect("last activity recorded");
    let age = chrono::Utc::now() - last_activity;
    assert!(age < chrono::Duration::minutes(1), "{last_activity}");

    let turns = store.load_turns().unwrap();
    assert_eq!(turn(&turns, "turn_dead").status, TurnStatus::Interrupted);
}

/// 人按停止走的是回合守卫那条路（进程还活着），不留认领——停了就是停了。
#[test]
fn a_turn_the_user_stopped_leaves_no_claim() {
    let (_temp, store) = test_store();
    store
        .start_turn("turn_stopped", "stop me", std::process::id())
        .unwrap();
    store
        .interrupt_turn_with_usage("turn_stopped", TurnTokens::default())
        .unwrap();
    assert_eq!(store.recover_stale_turns().unwrap(), 0);
    assert!(store.pending_restart_orphans().unwrap().is_empty());
}

#[test]
fn a_closed_claim_is_not_offered_again() {
    let (_temp, store) = test_store();
    store.start_turn("turn_dead", "task", DEAD_PID).unwrap();
    store.recover_stale_turns().unwrap();
    assert_eq!(store.pending_restart_orphans().unwrap().len(), 1);

    store
        .close_restart_orphan("turn_dead", "resumed", "run_x")
        .unwrap();
    assert!(store.pending_restart_orphans().unwrap().is_empty());
    // 重复结案无害，没有这一轮也无害。
    store
        .close_restart_orphan("turn_dead", "skipped", "again")
        .unwrap();
    store
        .close_restart_orphan("turn_missing", "skipped", "gone")
        .unwrap();
    assert!(store.pending_restart_orphans().unwrap().is_empty());
}

/// 打断多久都接着跑（用户 09-24：不设时间范围）。退回 48 小时回看窗口这条会红：
/// 机器关了三天、daemon 再起来，那一轮一样要接上。
#[test]
fn an_orphan_from_days_ago_is_still_offered() {
    let (temp, store) = test_store();
    store.start_turn("turn_dead", "task", DEAD_PID).unwrap();
    store.recover_stale_turns().unwrap();
    {
        let backdated = (chrono::Utc::now() - chrono::Duration::days(3)).to_rfc3339();
        let db_path = temp.path().join("state").join("conversation.db");
        let conn = rusqlite::Connection::open(db_path).unwrap();
        conn.execute(
            "UPDATE turns SET user_timestamp = ?1, assistant_timestamp = ?1",
            rusqlite::params![backdated],
        )
        .unwrap();
    }
    let orphans = store.pending_restart_orphans().unwrap();
    assert_eq!(orphans.len(), 1, "{orphans:?}");
}

/// 没有任何流水的孤儿（刚起步就死了）拿回合开始的时间当最后动静。
#[test]
fn an_orphan_without_journal_events_falls_back_to_the_turn_start() {
    let (_temp, store) = test_store();
    store.start_turn("turn_dead", "task", DEAD_PID).unwrap();
    store.recover_stale_turns().unwrap();
    let orphans = store.pending_restart_orphans().unwrap();
    assert!(orphans[0].last_activity.is_some(), "{orphans:?}");
}

/// 关停时的收尾只记用量，库里仍是「执行中」；等下一个进程把它当孤儿收尾，用量还在、
/// 认领也有。退回修复前（关停按用户取消收尾）这条会红。
#[test]
fn a_suspended_turn_stays_running_and_keeps_its_usage() {
    let (_temp, store) = test_store();
    store
        .start_turn("turn_live", "long task", DEAD_PID)
        .unwrap();
    let tokens = TurnTokens {
        total: 1_200,
        prompt: 1_000,
        cache_read: 800,
    };
    store.suspend_turn_with_usage("turn_live", tokens).unwrap();
    let turns = store.load_turns().unwrap();
    let suspended = turn(&turns, "turn_live");
    assert_eq!(suspended.status, TurnStatus::Running);
    assert_eq!(suspended.token_total, 1_200);
    assert_eq!(suspended.token_prompt, 1_000);
    assert_eq!(suspended.token_cache_read, 800);

    store.recover_stale_turns().unwrap();
    let turns = store.load_turns().unwrap();
    let recovered = turn(&turns, "turn_live");
    assert_eq!(recovered.status, TurnStatus::Interrupted);
    assert_eq!(recovered.token_total, 1_200, "recovery must keep the usage");
    assert_eq!(store.pending_restart_orphans().unwrap().len(), 1);
}

#[test]
fn has_turn_after_sees_later_turns_of_the_same_session() {
    let (_temp, store) = test_store();
    store.start_turn("turn_1", "first", DEAD_PID).unwrap();
    store.recover_stale_turns().unwrap();
    let seq = store.pending_restart_orphans().unwrap()[0].seq;
    let session = store.session_id();
    assert!(!store.has_turn_after(&session, seq).unwrap());
    store
        .start_turn("turn_2", "second", std::process::id())
        .unwrap();
    assert!(store.has_turn_after(&session, seq).unwrap());
}

/// 认领事件不能改变模型看到的东西：回放快照只收正文、思考与工具那几种。
#[test]
fn the_claim_stays_out_of_the_replay_snapshot() {
    let (_temp, store) = test_store();
    store.start_turn("turn_dead", "task", DEAD_PID).unwrap();
    store
        .append_turn_journal_event(
            "turn_dead",
            0,
            0,
            "assistant_content",
            None,
            None,
            Some("partial answer"),
            None,
            None,
        )
        .unwrap();
    store.recover_stale_turns().unwrap();
    let turns = store.load_turns().unwrap();
    let recovered = turn(&turns, "turn_dead");
    assert!(recovered
        .journal_events
        .iter()
        .any(|event| event.kind == "restart_orphaned"));
    let replay = store.session_replay(10).unwrap();
    let entries = format!("{:?}", replay[0].entries);
    assert!(!entries.contains("restart_orphaned"), "{entries}");
    assert!(entries.contains("partial answer"), "{entries}");
}
