//! 按页查询：网页一页回合、终端回放一页、上键历史的用户原话（09-24 会话项目第 1 段）。

use super::shared::*;
use crate::llm::ChatMessage;
use crate::state::*;

/// 连着跑完 `count` 轮，第 n 轮的用户原话是 `q{n}`、回复是 `a{n}`。
fn completed_turns(store: &StateStore, count: usize) {
    for index in 1..=count {
        let turn_id = format!("t{index}");
        store
            .start_turn(&turn_id, &format!("q{index}"), std::process::id())
            .unwrap();
        store
            .set_turn_context_messages(&turn_id, &[ChatMessage::turn_context("<runtime/>")])
            .unwrap();
        store
            .complete_turn(&turn_id, &format!("a{index}"), None)
            .unwrap();
    }
}

/// 一页不带模型才用的化石，游标从最新往前走，走到头给 None。
#[test]
fn a_turn_page_walks_back_from_the_newest_turn() {
    let (_temp, store) = test_store();
    completed_turns(&store, 5);
    assert!(
        !store.load_turns().unwrap()[0].context_messages.is_empty(),
        "测具没存上化石"
    );

    let newest = store.turn_page(None, 2).unwrap();
    let ids = |page: &TurnPage| {
        page.turns
            .iter()
            .map(|turn| turn.turn_id.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(ids(&newest), ["t4", "t5"]);
    assert!(newest
        .turns
        .iter()
        .all(|turn| turn.context_messages.is_empty()));
    assert_eq!(newest.turns[1].assistant_content, "a5");

    let middle = store.turn_page(newest.older, 2).unwrap();
    assert_eq!(ids(&middle), ["t2", "t3"]);
    let oldest = store.turn_page(middle.older, 2).unwrap();
    assert_eq!(ids(&oldest), ["t1"]);
    assert_eq!(oldest.older, None);
}

/// 回放快照同一套翻法；不给游标就是原来的「最近几轮」。
#[test]
fn a_replay_page_walks_back_the_same_way() {
    let (_temp, store) = test_store();
    completed_turns(&store, 5);
    let replies = |page: &ReplayPage| {
        page.turns
            .iter()
            .map(|turn| turn.assistant_content.clone())
            .collect::<Vec<_>>()
    };

    let newest = store.replay_page(None, 2).unwrap();
    assert_eq!(replies(&newest), ["a4", "a5"]);
    let recent = store.session_replay(2).unwrap();
    assert_eq!(
        recent
            .iter()
            .map(|turn| turn.assistant_content.clone())
            .collect::<Vec<_>>(),
        ["a4", "a5"]
    );
    let middle = store.replay_page(newest.older, 2).unwrap();
    assert_eq!(replies(&middle), ["a2", "a3"]);
    let oldest = store.replay_page(middle.older, 2).unwrap();
    assert_eq!(replies(&oldest), ["a1"]);
    assert_eq!(oldest.older, None);
    assert!(store.replay_page(None, 5).unwrap().older.is_none());
}

/// 上键历史只取用户原话，得和整段读出来的对话里 role=user 的那几条一模一样，
/// 连中途追加的消息也按先后排在它那一轮后面。
#[test]
fn user_inputs_match_the_user_entries_of_the_conversation() {
    let (_temp, store) = test_store();
    completed_turns(&store, 2);
    store.start_turn("t3", "q3", std::process::id()).unwrap();
    store
        .enqueue_prompt("p1", "追加一句", "追加一句", &[])
        .unwrap();
    store
        .consume_queued_prompts_with_checkpoint(
            "t3",
            &[("p1".to_string(), "追加一句".to_string(), "[]".to_string())],
            Some("a3 前半"),
            None,
            None,
            None,
            TurnRedoCheckpointPayload {
                replay_messages: Vec::new(),
                prefix_tool_reports: Vec::new(),
                tool_rounds: 0,
                question_rounds: 0,
                loaded_items: Vec::new(),
                prefix_question_count: 0,
                prefix_image_asset_ids: Vec::new(),
                prefix_artifact_asset_ids: Vec::new(),
            },
        )
        .unwrap();
    store.complete_turn("t3", "a3", None).unwrap();

    let expected = store
        .load_conversation()
        .unwrap()
        .into_iter()
        .filter(|entry| entry.role == "user")
        .map(|entry| entry.content)
        .collect::<Vec<_>>();
    assert_eq!(expected, ["q1", "q2", "q3", "追加一句"]);
    assert_eq!(store.user_inputs().unwrap(), expected);
}
