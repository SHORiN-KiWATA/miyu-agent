//! 事件游标与提问应答。

use super::shared::*;
use crate::runtime::{normalize_answers, EVENT_CAPACITY, MAX_CONTENT_CHARS};
use crate::web::*;
use miyu_base::question::QuestionResponse;

#[test]
fn stale_event_cursor_receives_resync_marker() {
    let events = EventHub::new();
    for index in 0..=EVENT_CAPACITY {
        events.publish("test", json!({ "index": index }));
    }
    let replay = events.replay_after(0);
    assert_eq!(replay.len(), 1);
    assert_eq!(replay[0].kind, "resync_required");
    assert_eq!(replay[0].id, events.latest_id());
    let next = events.publish("after-resync", json!({}));
    assert!(next > replay[0].id);
}

/// resync 后从 `latest_event_id` 续读能收到后续事件——这是 handle_ipc_turn
/// 「resync 时续流而非误报已取消」修复(09-01 跨会话误取消)的核心前提:一个
/// 会话的密集事件把另一会话的游标挤出共享缓冲时,后者的回合仍在跑,应从
/// resync 标记带的 `latest_event_id` 继续,最终照常收到 run.completed。
#[test]
fn resync_marker_carries_a_cursor_that_resumes_the_stream() {
    let events = EventHub::new();
    for index in 0..=EVENT_CAPACITY {
        events.publish("flood", json!({ "index": index }));
    }
    let replay = events.replay_after(0);
    assert_eq!(replay[0].kind, "resync_required");
    // 修复就是从这个 latest_event_id 续流,而不是断流报「已取消」。
    let cursor: u64 = serde_json::from_str::<serde_json::Value>(&replay[0].data)
        .ok()
        .and_then(|value| value.get("latest_event_id").and_then(|id| id.as_u64()))
        .unwrap_or(replay[0].id);
    assert_eq!(cursor, replay[0].id, "resync 标记应带最新游标");

    // 续读:此后发布的事件应能从该游标取到,不再立刻二次 resync。
    let after = events.publish("run.completed", json!({ "run_id": "r1" }));
    let resumed = events.replay_after(cursor);
    assert_eq!(resumed.len(), 1);
    assert_eq!(resumed[0].id, after);
    assert_eq!(resumed[0].kind, "run.completed");
}

#[test]
fn replay_after_cursor_is_ordered_and_exclusive() {
    let events = EventHub::new();
    events.publish("one", json!({}));
    events.publish("two", json!({}));
    events.publish("three", json!({}));
    let replay = events.replay_after(1);
    assert_eq!(
        replay.iter().map(|record| record.id).collect::<Vec<_>>(),
        vec![2, 3]
    );
}

#[test]
fn future_event_cursor_requests_resync_after_server_restart() {
    let events = EventHub::new();
    let replay = events.replay_after(42);
    assert_eq!(replay.len(), 1);
    assert_eq!(replay[0].kind, "resync_required");
}

#[test]
fn answer_validation_trims_values_and_rejects_control_characters() {
    let request = sample_question();
    assert_eq!(
        normalize_answers(&request, vec![vec!["  All  ".to_string()]]).unwrap(),
        vec![vec!["All".to_string()]]
    );
    assert!(normalize_answers(&request, vec![vec!["bad\nanswer".to_string()]]).is_err());
}

#[test]
fn invalid_answer_keeps_question_pending() {
    let broker = QuestionBroker::new();
    let (responder, mut response) = oneshot::channel();
    let question_id = broker.insert("run_test", sample_question(), responder);
    let invalid = broker.answer(&question_id, vec![Vec::new()], |_, _| {
        panic!("invalid answer must not be published")
    });
    assert!(matches!(invalid, Err(AnswerFailure::Invalid(_))));
    assert!(broker.pending.lock().unwrap().contains_key(&question_id));

    broker
        .answer(
            &question_id,
            vec![vec![" All ".to_string()]],
            |run_id, answers| {
                assert_eq!(run_id, "run_test");
                assert_eq!(answers, &vec![vec!["All".to_string()]]);
            },
        )
        .unwrap();
    assert!(matches!(
        response.try_recv().unwrap(),
        QuestionResponse::Answered(answers) if answers == vec![vec!["All".to_string()]]
    ));
}

#[test]
fn closed_question_responder_does_not_publish_an_answer() {
    let broker = QuestionBroker::new();
    let (responder, response) = oneshot::channel();
    drop(response);
    let question_id = broker.insert("run_test", sample_question(), responder);
    let mut published = false;
    let result = broker.answer(&question_id, vec![vec!["All".to_string()]], |_, _| {
        published = true
    });
    assert!(matches!(result, Err(AnswerFailure::Gone)));
    assert!(!published);
}

#[test]
fn closed_question_receiver_does_not_publish_close_event() {
    let broker = QuestionBroker::new();
    let (responder, response) = oneshot::channel();
    drop(response);
    let question_id = broker.insert("run_test", sample_question(), responder);
    let mut published = false;

    let result = broker.close(&question_id, |_| published = true);

    assert!(matches!(result, Err(AnswerFailure::Gone)));
    assert!(!published);
}

#[test]
fn content_limit_counts_characters() {
    assert!(validate_content("x".repeat(MAX_CONTENT_CHARS)).is_ok());
    let error = validate_content("界".repeat(MAX_CONTENT_CHARS + 1)).unwrap_err();
    assert_eq!(error.status, StatusCode::PAYLOAD_TOO_LARGE);
}

/// 补发给后挂上来的终端时，已了结的题要带上结果（09-24：答完退出 TUI 再进，
/// 又进了同一个提问界面）；还在等的题照发不动。
#[test]
fn replayed_question_carries_its_outcome_once_settled() {
    let broker = QuestionBroker::new();
    let (responder, _response) = oneshot::channel();
    let question_id = broker.insert("run_test", sample_question(), responder);
    let requested = || json!({ "run_id": "run_test", "question_id": question_id });

    let mut waiting = requested();
    mark_settled_question(&broker, &mut waiting);
    assert!(waiting.get("settled").is_none(), "还在等的题不该带结果");

    broker
        .answer(&question_id, vec![vec![" All ".to_string()]], |_, _| {})
        .unwrap();
    let mut replayed = requested();
    mark_settled_question(&broker, &mut replayed);
    assert_eq!(
        replayed["settled"],
        json!({ "status": "answered", "detail": [["All"]] })
    );
    // 终端那头按同一份定义解回来。
    assert_eq!(
        serde_json::from_value::<QuestionResponse>(replayed["settled"].clone()).unwrap(),
        QuestionResponse::Answered(vec![vec!["All".to_string()]])
    );
}

#[test]
fn every_way_a_question_ends_is_remembered() {
    let broker = QuestionBroker::new();

    let (responder, _closed_rx) = oneshot::channel();
    let closed = broker.insert("run_a", sample_question(), responder);
    broker.close(&closed, |_| {}).unwrap();
    assert_eq!(broker.settled(&closed), Some(QuestionResponse::Closed));

    let (responder, _cancelled_rx) = oneshot::channel();
    let cancelled = broker.insert("run_b", sample_question(), responder);
    assert_eq!(broker.settled(&cancelled), None);
    broker.cancel_run("run_b");
    assert_eq!(
        broker.settled(&cancelled),
        Some(QuestionResponse::Cancelled)
    );

    // 回合循环等超时放弃了：题还在表里，但等答案的那一头已经走了。
    let (responder, timed_out_rx) = oneshot::channel();
    let timed_out = broker.insert("run_c", sample_question(), responder);
    drop(timed_out_rx);
    assert!(matches!(
        broker.settled(&timed_out),
        Some(QuestionResponse::Unavailable(_))
    ));

    // 查无此题也不当「还在等」：弹一个答不了的面板比什么都不弹更糟。
    assert!(matches!(
        broker.settled("question_unknown"),
        Some(QuestionResponse::Unavailable(_))
    ));
}
