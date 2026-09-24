//! 运行时状态与日志。

use crate::platforms::plugins::real_context::*;

#[test]
fn inactive_session_runtime_cache_has_a_hard_soft_limit() {
    let now = Instant::now();
    let mut runtime = RuntimeState::default();
    for index in 0..SESSION_STATE_SOFT_LIMIT + 32 {
        runtime.session_mut(&format!("group-{index}"), now);
    }

    runtime.prune(now);

    assert_eq!(runtime.sessions.len(), SESSION_STATE_SOFT_LIMIT);
}

#[test]
fn active_reply_decision_log_is_structured_for_humans() {
    let moderation = judge::ModerationResult {
        violation: false,
        severity: 0.0,
        ..judge::ModerationResult::default()
    };
    let log = ActiveReplyDecisionLog {
        account_id: "10000",
        group_id: "20000",
        sender_name: "测试用户",
        sender_id: "30000",
        mentioned_bot: false,
        message: "引用上一条消息\n继续讨论",
        trigger: TriggerKind::Continuation,
        should_reply: true,
        model_should_reply: Some(true),
        raw_score: 0.72,
        final_score: 0.91,
        threshold: 0.84,
        model_adjustment: 0.2,
        affection_level: "熟人",
        affection_adjustment: 0.03,
        emotion_adjustment: 0.0,
        continuation_adjustment: 0.05,
        system_adjustment: 0.0,
        reply_pressure: 3.0,
        restraint_threshold: 0.285,
        to_bot: Some(false),
        addressed: false,
        after_speaking_score_adjustment: 0.15,
        moderation: &moderation,
        reason: "当前消息延续了上一轮问题。",
        endpoint: Some("opencodego / mimo-v2.5"),
    };
    let rendered = format_active_reply_decision_log_for(&log, Locale::Zh);

    assert!(rendered.starts_with("【续聊窗口判断：回复】\n"));
    assert!(rendered.contains("会话：群聊 20000（机器人 QQ 10000）"));
    assert!(rendered.contains("发送者：测试用户（QQ 30000）"));
    assert!(rendered.contains("@机器人：否"));
    assert!(rendered.contains("模型：opencodego / mimo-v2.5"));
    assert!(rendered.contains("消息：引用上一条消息 继续讨论"));
    assert!(rendered.contains("触发：自然续聊 (continuation)"));
    assert!(rendered.contains("结果：回复"));
    assert!(rendered.contains("分数：0.910（原始 0.720，阈值 0.840）"));
    assert!(rendered.contains("回复倾向调整：应该回复 +0.200"));
    assert!(rendered.contains("好感度调整：熟人 +0.030"));
    assert!(rendered.contains("自然续聊调整：+0.050"));
    assert!(!rendered.contains("直接触发调整"));
    assert!(rendered.contains("冷静机制调整：阈值 +0.285（近期发言量 3.00）"));
    assert!(rendered.contains("指向机器人：否"));
    assert!(!rendered.contains("豁免"));
    assert!(!rendered.contains("短句阈值调整"));
    assert!(rendered.contains("刚说过话加分：+0.150"));
    assert!(!rendered.contains("安全初判"));
    assert!(rendered.ends_with("判断理由：当前消息延续了上一轮问题。"));
    assert_eq!(
        TriggerKind::Probability.decision_log_title(false, Locale::Zh),
        "【主动回复判断：不回复】"
    );
    let english = format_active_reply_decision_log_for(&log, Locale::En);
    assert!(english.starts_with("[Continuation decision: reply]\n"));
    assert!(english.contains("Conversation: group 20000 (bot QQ 10000)"));
    assert!(english.contains("Affection adjustment: 熟人 +0.030"));
    assert!(english.ends_with("Reason: 当前消息延续了上一轮问题。"));

    // 判官认定指向机器人:冷静豁免,但要写明,免得以为冷静没生效。
    let aimed = ActiveReplyDecisionLog {
        to_bot: Some(true),
        restraint_threshold: 0.0,
        ..log
    };
    let rendered = format_active_reply_decision_log_for(&aimed, Locale::Zh);
    assert!(rendered.contains("指向机器人：是"));
    assert!(rendered.contains("冷静机制调整：豁免（指向机器人，近期发言量 3.00）"));
    assert!(!rendered.contains("阈值 +"));

    // 被 @ 但判官说没指向机器人(@ 一串人约别人):@ 优先照样豁免,理由写「直接触发」。
    let mentioned = ActiveReplyDecisionLog {
        to_bot: Some(false),
        addressed: true,
        restraint_threshold: 0.0,
        ..log
    };
    let rendered = format_active_reply_decision_log_for(&mentioned, Locale::Zh);
    assert!(rendered.contains("指向机器人：否"));
    assert!(rendered.contains("冷静机制调整：豁免（直接触发，近期发言量 3.00）"));
    assert!(!rendered.contains("豁免（指向机器人"));
}

#[test]
fn muted_skip_log_names_the_trigger_or_falls_back_to_system() {
    assert_eq!(
        format_active_reply_muted_log_for(
            "10000",
            "20000",
            "测试用户",
            "30000",
            Some(TriggerKind::Direct),
            Locale::Zh,
        ),
        "（跳过：自认被禁言）\n会话：群聊 20000（机器人 QQ 10000）\n发送者：测试用户（QQ 30000）\n触发：直接触发 (direct)\n结果：不回复\n原因：查到机器人自己在本群处于禁言中，等解禁再说话"
    );
    assert!(
        format_active_reply_muted_log_for("10000", "20000", "User", "30000", None, Locale::En,)
            .contains("Trigger: system")
    );
}

#[test]
fn active_reply_skip_log_keeps_session_sender_and_reason() {
    assert_eq!(
        format_active_reply_skip_log_for(
            "10000",
            "20000",
            "测试用户",
            "30000",
            TriggerKind::Direct,
            "被新消息覆盖",
            Locale::Zh,
        ),
        "（跳过主动判断）\n会话：群聊 20000（机器人 QQ 10000）\n发送者：测试用户（QQ 30000）\n触发：直接触发 (direct)\n结果：跳过\n判断原因：被新消息覆盖"
    );
    assert!(format_active_reply_skip_log_for(
        "10000",
        "20000",
        "User",
        "30000",
        TriggerKind::Direct,
        "superseded",
        Locale::En,
    )
    .starts_with("[Active reply decision skipped]\nConversation: group 20000"));
}

/// 观察窗口有自己的开关（09-19）。
///
/// 以前它跟着 `continuation_enable` 走：想关续聊就必然把「她刚发完言、接下来
/// 一会儿都留意着」一起关掉，而配置面上没有任何提示。
#[test]
fn the_observation_window_has_its_own_switch() {
    let mut settings = RealContextPluginSettings::default();
    let now = Instant::now();
    let mut runtime = RuntimeState::default();
    let session = runtime.session_mut("group", now);
    session.last_reply = Some(now);
    let session = &*session;

    assert!(session.spoke_recently(now, &settings), "默认两个都开着");

    settings.continuation_enable = false;
    assert!(
        session.spoke_recently(now, &settings),
        "关掉续聊不该把观察窗口一起关掉"
    );

    settings.continuation_enable = true;
    settings.after_speaking_enable = false;
    assert!(!session.spoke_recently(now, &settings), "自己的开关要管用");
}
