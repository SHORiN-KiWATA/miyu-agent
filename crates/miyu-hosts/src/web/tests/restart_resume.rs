//! 断点续跑的去留（09-24）：被重启打断的一律接着跑——不设次数上限、不设时间范围，
//! 子代理与 `/goal` 续轮也接（用户 09-24）。只有接不了的才不接：会话没了、归档了、
//! 一次性会话、人已经接着聊下去了。

use super::shared::*;
use crate::web::*;
use chrono::{Duration as ChronoDuration, Utc};
use miyu_core::state::{compose_service_restart_message, RestartOrphan, ServiceRestart};

fn session(state: &DaemonState, kind: &str) -> miyu_core::state::SessionRecord {
    state
        .state_store
        .create_session(&active_persona_scope(state), "work", kind, None)
        .unwrap()
}

fn orphan(content: &str, minutes_ago: i64) -> RestartOrphan {
    RestartOrphan {
        turn_id: "turn_1".to_string(),
        session_id: "sess_1".to_string(),
        seq: 1,
        user_content: content.to_string(),
        workspace: None,
        last_activity: Some(Utc::now() - ChronoDuration::minutes(minutes_ago)),
    }
}

fn resume(attempt: u32, goal_round: bool) -> ResumeDecision {
    ResumeDecision::Resume {
        attempt,
        goal_round,
    }
}

#[test]
fn a_fresh_user_turn_is_resumed_as_the_first_attempt() {
    let temp = tempfile::tempdir().unwrap();
    let state = DaemonState::for_test(test_paths(temp.path()), 8300).unwrap();
    let record = session(&state, miyu_core::state::USER_SESSION_KIND);
    assert_eq!(
        resume_decision(&orphan("fix the build", 2), Some(&record), false),
        resume(1, false)
    );
}

/// 续跑的那一轮又被打断：次数接着数（界面写「第 n 次」），没有上限。退回三次封顶
/// 这条会红。
#[test]
fn resumes_keep_counting_without_a_limit() {
    let temp = tempfile::tempdir().unwrap();
    let state = DaemonState::for_test(test_paths(temp.path()), 8300).unwrap();
    let record = session(&state, miyu_core::state::USER_SESSION_KIND);
    for previous in [1, 2, 3, 4, 20] {
        let content = compose_service_restart_message(&ServiceRestart::new(previous));
        assert_eq!(
            resume_decision(&orphan(&content, 1), Some(&record), false),
            resume(previous + 1, false)
        );
    }
}

/// 打断多久都接着跑，本地会话和 QQ 一样。退回时间窗这条会红。
#[test]
fn an_old_interruption_is_resumed_too() {
    let temp = tempfile::tempdir().unwrap();
    let state = DaemonState::for_test(test_paths(temp.path()), 8300).unwrap();
    let record = session(&state, miyu_core::state::USER_SESSION_KIND);
    let three_days = 3 * 24 * 60;
    assert_eq!(
        resume_decision(&orphan("task", three_days), Some(&record), false),
        resume(1, false)
    );
}

/// 子代理自己被打断的那一轮也接（挂回父会话名下跑）。退回「子代理等人回复」这条会红。
#[test]
fn a_subagent_turn_is_resumed() {
    let temp = tempfile::tempdir().unwrap();
    let state = DaemonState::for_test(test_paths(temp.path()), 8300).unwrap();
    let record = session(&state, miyu_core::state::SUBAGENT_SESSION_KIND);
    assert_eq!(
        resume_decision(&orphan("investigate the crash", 1), Some(&record), false),
        resume(1, false)
    );
}

/// `/goal` 续轮接着跑、自动续轮重新武装；续跑的续轮再被打断也认得出来。
#[test]
fn goal_rounds_are_resumed_and_stay_recognisable() {
    let temp = tempfile::tempdir().unwrap();
    let state = DaemonState::for_test(test_paths(temp.path()), 8300).unwrap();
    let record = session(&state, miyu_core::state::USER_SESSION_KIND);
    let round = orphan(
        &format!("{}\nRound 3 of 0", miyu_core::state::GOAL_ROUND_TAG),
        1,
    );
    assert_eq!(
        resume_decision(&round, Some(&record), false),
        resume(1, true)
    );
    let again = compose_service_restart_message(&ServiceRestart {
        goal_round: true,
        ..ServiceRestart::new(1)
    });
    assert_eq!(
        resume_decision(&orphan(&again, 1), Some(&record), false),
        resume(2, true)
    );
}

#[test]
fn sessions_that_cannot_resume_are_skipped() {
    let temp = tempfile::tempdir().unwrap();
    let state = DaemonState::for_test(test_paths(temp.path()), 8300).unwrap();
    let user = session(&state, miyu_core::state::USER_SESSION_KIND);
    let ask = session(&state, miyu_core::state::ASK_SESSION_KIND);
    let mut archived = user.clone();
    archived.archived = true;
    let fresh = orphan("task", 1);
    for (record, why) in [
        (None, "missing session"),
        (Some(&ask), "one-shot session"),
        (Some(&archived), "archived session"),
    ] {
        assert!(
            matches!(
                resume_decision(&fresh, record, false),
                ResumeDecision::Skip(_)
            ),
            "{why} must not resume"
        );
    }
    assert!(
        matches!(
            resume_decision(&fresh, Some(&user), true),
            ResumeDecision::Skip(_)
        ),
        "a newer turn means the conversation moved on"
    );
}
