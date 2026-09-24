//! 判断通过后贴在对方消息上的「在看了」表情。
//!
//! 09-24 用户拍板:抽样与刚说过话这两路是她自己凑过去插话,没人在等她,不贴。
//! 按主触发归类——同时被 @、在续聊窗口里的照贴;补救消息顶替时沿用原始触发。

use super::shared::*;
use crate::platforms::plugins::real_context::*;

type Recorded = Arc<Mutex<Vec<(String, String, bool)>>>;

fn reaction_context() -> (tempfile::TempDir, PlatformTurnContext, Recorded) {
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let (temp, context) = test_context(Arc::new(ReactionAdapter {
        reactions: recorded.clone(),
    }));
    (temp, context, recorded)
}

fn marked(recorded: &Recorded, message_id: &str) -> bool {
    recorded
        .lock()
        .unwrap()
        .iter()
        .any(|(id, _, active)| id == message_id && *active)
}

/// 已承诺的回复被同一发送者的补救消息顶替:这是判官放行后唯一不需要判官模型
/// 就能走到贴表情那一步的路,测试环境里用它代表「判断通过」。
async fn take_over(trigger: TriggerKind) -> (bool, bool) {
    let (_temp, context, recorded) = reaction_context();
    let plugin = RealContextPlugin::new();
    let first = inbound_event();
    plugin.register_committed_pending(
        &context,
        trigger,
        Vec::new(),
        vec![active_reply_target(&first)],
        false,
    );
    let mut correction = inbound_event();
    correction.message_id = "message-2".to_string();
    correction.text = "接着刚才那句再补一条".to_string();
    let mut decision = TriggerDecision {
        should_reply: false,
        content: correction.text.clone(),
        response_target: None,
    };
    plugin
        .decide_group_trigger(
            &context,
            &correction,
            &mut decision,
            &RealContextPluginSettings::default(),
        )
        .await
        .unwrap();
    (decision.should_reply, marked(&recorded, "message-2"))
}

#[tokio::test]
async fn sampled_and_after_speaking_replies_leave_the_message_unmarked() {
    for trigger in [TriggerKind::Probability, TriggerKind::AfterSpeaking] {
        let (replied, reacted) = take_over(trigger).await;
        assert!(replied, "{trigger:?}: 承诺沿用,补救消息应直接回复");
        assert!(!reacted, "{trigger:?}: 自己凑过去插话不该先贴表情");
    }
}

#[tokio::test]
async fn called_or_continuing_replies_still_mark_the_message() {
    for trigger in [TriggerKind::Direct, TriggerKind::Continuation] {
        let (replied, reacted) = take_over(trigger).await;
        assert!(replied, "{trigger:?}: 承诺沿用,补救消息应直接回复");
        assert!(reacted, "{trigger:?}: 被叫到或正聊着的照贴表情");
    }
}

/// 回合已在跑时的补救(`confirm_supersede`)同样沿用原始触发。
#[tokio::test]
async fn a_running_sampled_turn_does_not_mark_the_correction() {
    for (trigger, wants) in [
        (TriggerKind::Probability, false),
        (TriggerKind::AfterSpeaking, false),
        (TriggerKind::Direct, true),
    ] {
        let (_temp, context, recorded) = reaction_context();
        let plugin = RealContextPlugin::new();
        let first = inbound_event();
        plugin.register_committed_pending(
            &context,
            trigger,
            Vec::new(),
            vec![active_reply_target(&first)],
            false,
        );
        let mut correction = inbound_event();
        correction.message_id = "message-2".to_string();
        plugin.confirm_supersede(&context, &correction).await;
        assert_eq!(
            marked(&recorded, "message-2"),
            wants,
            "{trigger:?} 的补救消息贴表情与否不符预期"
        );
    }
}

#[test]
fn only_self_initiated_triggers_skip_the_reaction() {
    for (trigger, wants) in [
        (TriggerKind::Probability, false),
        (TriggerKind::AfterSpeaking, false),
        (TriggerKind::Direct, true),
        (TriggerKind::Continuation, true),
        (TriggerKind::Supersede, true),
        (TriggerKind::Moderation, true),
    ] {
        assert_eq!(trigger.marks_with_reaction(), wants, "{trigger:?}");
    }
}
