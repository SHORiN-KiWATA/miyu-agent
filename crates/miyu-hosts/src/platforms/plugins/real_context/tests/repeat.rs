//! 复读只判第一条(10-22 群里五个人刷「miyu似了」被挨个回)。

use super::shared::*;
use crate::platforms::plugins::real_context::repeat::*;
use crate::platforms::plugins::real_context::*;

fn said(message_id: &str, sender_id: &str, text: &str, sent_at: i64) -> HistoryMessage {
    let mut message = history_message(message_id, text);
    message.sender_id = sender_id.to_string();
    message.sent_at = sent_at;
    message.ingress_order = Some(sent_at);
    message
}

#[test]
fn the_key_ignores_spacing_punctuation_symbols_and_case() {
    assert_eq!(repeat_key("Miyu 似了！"), repeat_key("miyu似了"));
    assert_eq!(repeat_key("  MIYU，似了~~ "), "miyu似了");
    assert_eq!(repeat_key("😨😨"), "");
    assert_ne!(repeat_key("miyu似了"), repeat_key("miyu活了"));
}

#[test]
fn a_line_someone_else_just_said_is_a_repeat_and_points_at_the_first() {
    let history = vec![
        said("a", "1", "miyu似了", 100),
        said("other", "2", "别的话", 105),
        said("b", "3", "miyu 似了", 110),
    ];
    assert_eq!(
        repeat_head("miyu似了", "4", 115, &history).as_deref(),
        Some("a")
    );
}

#[test]
fn the_chain_is_measured_between_neighbours_not_from_the_first() {
    // 每两条隔 100 秒,头尾隔 200 秒,仍是一串。
    let history = vec![said("a", "1", "似了", 0), said("b", "2", "似了", 100)];
    assert_eq!(
        repeat_head("似了", "3", 200, &history).as_deref(),
        Some("a")
    );
}

#[test]
fn a_gap_wider_than_the_window_breaks_the_chain() {
    let history = vec![said("a", "1", "似了", 0)];
    assert_eq!(
        repeat_head("似了", "2", REPEAT_GAP_SECONDS + 1, &history),
        None
    );
}

#[test]
fn saying_it_again_yourself_is_not_a_repeat() {
    let history = vec![said("a", "1", "似了", 0)];
    assert_eq!(repeat_head("似了", "1", 10, &history), None);
}

#[test]
fn the_bots_own_line_and_recalled_lines_do_not_start_a_chain() {
    let mut own = said("own", "10000", "似了", 0);
    own.is_bot = true;
    let mut recalled = said("gone", "1", "似了", 5);
    recalled.recalled_at = Some(6);
    assert_eq!(repeat_head("似了", "2", 10, &[own, recalled]), None);
}

async fn record(
    plugin: &RealContextPlugin,
    context: &PlatformTurnContext,
    message: HistoryMessage,
) {
    plugin
        .store(context)
        .record_message(NewHistoryMessage {
            group: group_key(context).unwrap(),
            message_id: message.message_id,
            sender_id: message.sender_id,
            sender_name: message.sender_name,
            content: message.content,
            reply_to_message_id: None,
            is_bot: message.is_bot,
            sent_at: message.sent_at,
            ingress_order: message.ingress_order,
        })
        .await
        .unwrap();
}

fn repeating_context(is_admin: bool) -> (tempfile::TempDir, PlatformTurnContext) {
    let (temp, context) = availability_context(BotSendAvailability::Available);
    let mut event = context.inbound_event().unwrap().clone();
    event.message_id = "second".to_string();
    event.text = "Miyu 似了".to_string();
    event.timestamp = 110;
    event.ingress_order = Some(110);
    let context = PlatformTurnContext::new(
        context.conversation.clone(),
        context.sender_id.clone(),
        context.sender_display_name.clone(),
        is_admin,
        context.config.clone(),
        context.paths.clone(),
        context.state_store.clone(),
        context.adapter.clone(),
        context.plugins.clone(),
    )
    .with_inbound_event(event);
    (temp, context)
}

async fn direct(plugin: &RealContextPlugin, context: &PlatformTurnContext) -> TriggerDecision {
    let settings = RealContextPluginSettings {
        takeover_direct_trigger_enable: false,
        active_reply_reaction_enable: false,
        ..RealContextPluginSettings::default()
    };
    let event = context.inbound_event().unwrap();
    let mut decision = TriggerDecision {
        should_reply: true,
        content: event.text.clone(),
        response_target: None,
    };
    plugin
        .decide_group_trigger(context, event, &mut decision, &settings)
        .await
        .unwrap();
    decision
}

#[tokio::test]
async fn a_direct_trigger_that_repeats_someone_else_is_not_answered_again() {
    let (_temp, context) = repeating_context(false);
    let plugin = RealContextPlugin::new();
    record(&plugin, &context, said("first", "40000", "miyu似了", 100)).await;
    let decision = direct(&plugin, &context).await;
    assert!(
        !decision.should_reply,
        "only the first of a repeat chain is judged"
    );

    // 回串首那条时不带引用和 @。
    let (_temp2, first) = availability_context(BotSendAvailability::Available);
    let mut event = first.inbound_event().unwrap().clone();
    event.message_id = "first".to_string();
    let first = first.with_inbound_event(event);
    assert!(plugin.replying_to_repeated(&first));
}

#[tokio::test]
async fn the_terminal_admin_is_not_held_back_by_a_repeat() {
    let (_temp, context) = repeating_context(true);
    let plugin = RealContextPlugin::new();
    record(&plugin, &context, said("first", "40000", "miyu似了", 100)).await;
    assert!(direct(&plugin, &context).await.should_reply);
}

#[tokio::test]
async fn a_line_nobody_else_said_is_answered_as_usual() {
    let (_temp, context) = repeating_context(false);
    let plugin = RealContextPlugin::new();
    record(&plugin, &context, said("first", "40000", "miyu活了", 100)).await;
    assert!(direct(&plugin, &context).await.should_reply);
    assert!(!plugin.replying_to_repeated(&context));
}
