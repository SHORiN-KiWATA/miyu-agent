//! 插件设置的默认值与校验。

use crate::config_tui::{
    apply_plugin_fields, apply_real_context_values, apply_reply_processor_values,
    embedding_model_label, field_display_value, group_join_approval_group_label,
    group_join_approval_values, parse_real_context_identity_lines, parse_real_context_string_lines,
    parse_reply_processor_fields, plugin_fields, real_context_values, reply_processor_fields,
    reply_processor_mode_label, reply_processor_mode_value, reply_processor_values, t,
    upsert_group_join_approval_group, validate_reply_processor_settings,
    ReplyProcessorSettingsForm, REPLY_PROCESSOR_PLUGIN_ID,
};
use miyu_base::config::{
    AppConfig, PlatformPluginInstanceConfig, QqGroupJoinApprovalGroupConfig,
    RealContextPluginSettings, REAL_CONTEXT_PLUGIN_ID,
};

#[test]
fn group_join_approval_defaults_to_enabled_with_empty_groups() {
    let config = AppConfig::default();
    let (enabled, settings) = group_join_approval_values(&config).unwrap();
    assert!(enabled);
    assert!(settings.groups.is_empty());
    assert_eq!(settings.timeout_seconds, 60);
    assert_eq!(settings.max_retries, 1);
    assert_eq!(
        settings.text_models.tier_ref(),
        Some(miyu_base::config::ModelTier::Lite)
    );
}

#[test]
fn group_join_approval_upsert_keeps_one_entry_per_group() {
    let mut groups = vec![
        QqGroupJoinApprovalGroupConfig {
            group_id: 1,
            approve_condition: "first".to_string(),
        },
        QqGroupJoinApprovalGroupConfig {
            group_id: 2,
            approve_condition: "second".to_string(),
        },
    ];
    upsert_group_join_approval_group(
        &mut groups,
        QqGroupJoinApprovalGroupConfig {
            group_id: 1,
            approve_condition: "replaced".to_string(),
        },
    );
    assert_eq!(groups.len(), 2);
    assert_eq!(
        groups[0],
        QqGroupJoinApprovalGroupConfig {
            group_id: 1,
            approve_condition: "replaced".to_string(),
        }
    );
    assert!(
        group_join_approval_group_label(&groups[1]).starts_with("2 · "),
        "group label should contain the group id"
    );
}

#[test]
fn reply_processor_defaults_match_platform_contract() {
    let config = AppConfig::default();
    let (enabled, settings) = reply_processor_values(&config).unwrap();

    assert!(enabled);
    assert!(settings.default_enabled);
    assert_eq!(settings.threshold, 200);
    assert_eq!(settings.mode, "image");
    assert!(settings.followup_mention);
    assert!(settings.strip_period);
    assert_eq!(settings.theme, "paper");
    assert_eq!(settings.font_size, 36);
    assert_eq!(settings.code_font_size, 30);
    assert_eq!(settings.padding, 64);
    assert!(settings.context_notice);
    assert_eq!(settings.ttl_hours, 24);
    assert_eq!(settings.max_records, 3);
    assert!(settings.send_tool_intercept);
    assert!(settings.font.is_empty());
    assert!(settings.title_font.is_empty());
    assert!(settings.code_font.is_empty());
    assert!(settings.emoji_font.is_empty());
}

#[test]
fn reply_processor_mode_labels_preserve_config_values() {
    assert_eq!(
        reply_processor_mode_label("image"),
        t("Convert to image", "转图片")
    );
    assert_eq!(
        reply_processor_mode_label("forward"),
        t("Merged forward", "合并转发")
    );
    assert_eq!(reply_processor_mode_value("转图片"), Some("image"));
    assert_eq!(
        reply_processor_mode_value("Merged forward"),
        Some("forward")
    );
    assert_eq!(reply_processor_mode_value("unsupported"), None);
}

#[test]
fn reply_processor_settings_use_generic_map_and_preserve_unknown_keys() {
    let mut config = AppConfig::default();
    let mut instance = PlatformPluginInstanceConfig {
        enabled: Some(false),
        ..PlatformPluginInstanceConfig::default()
    };
    instance
        .settings
        .insert("future_option".to_string(), serde_json::json!({"value": 1}));
    config
        .platforms
        .qq
        .plugins
        .insert(REPLY_PROCESSOR_PLUGIN_ID.to_string(), instance);
    let settings = ReplyProcessorSettingsForm {
        threshold: 512,
        mode: "forward".to_string(),
        ..ReplyProcessorSettingsForm::default()
    };

    apply_reply_processor_values(&mut config, true, &settings).unwrap();

    let instance = &config.platforms.qq.plugins[REPLY_PROCESSOR_PLUGIN_ID];
    assert_eq!(instance.enabled, None);
    assert_eq!(instance.settings["threshold"], 512);
    assert_eq!(instance.settings["mode"], "forward");
    assert_eq!(instance.settings["future_option"]["value"], 1);
    let (enabled, reparsed) = reply_processor_values(&config).unwrap();
    assert!(enabled);
    assert_eq!(reparsed, settings);
}

/// 表单各行和解析下标对得上（09-24 删掉「长图最大高度」，后面的下标整体前移
/// 一位）：每项填一个不同的值，拼成表单再解析回来必须原样一致。
#[test]
fn reply_processor_form_round_trips_every_field() {
    let settings = ReplyProcessorSettingsForm {
        default_enabled: false,
        threshold: 321,
        mode: "forward".to_string(),
        followup_mention: false,
        strip_period: true,
        theme: "dark".to_string(),
        font_size: 40,
        code_font_size: 28,
        padding: 80,
        context_notice: false,
        ttl_hours: 12,
        max_records: 5,
        send_tool_intercept: true,
        font: "/fonts/body.ttf".to_string(),
        title_font: "/fonts/title.ttf".to_string(),
        code_font: "/fonts/code.ttf".to_string(),
        emoji_font: "/fonts/emoji.ttf".to_string(),
    };
    let fields = reply_processor_fields(true, &settings);
    assert_eq!(parse_reply_processor_fields(&fields).unwrap(), settings);
}

#[test]
fn reply_processor_range_validation_rejects_unsafe_render_settings() {
    assert!(validate_reply_processor_settings(&ReplyProcessorSettingsForm::default()).is_ok());
    assert!(
        validate_reply_processor_settings(&ReplyProcessorSettingsForm {
            threshold: 0,
            ..ReplyProcessorSettingsForm::default()
        })
        .is_err()
    );
    assert!(
        validate_reply_processor_settings(&ReplyProcessorSettingsForm {
            ttl_hours: 169,
            ..ReplyProcessorSettingsForm::default()
        })
        .is_err()
    );
}

#[test]
fn real_context_settings_use_generic_map_and_preserve_unknown_keys() {
    let mut config = AppConfig::default();
    let mut instance = PlatformPluginInstanceConfig::default();
    instance
        .settings
        .insert("future_option".to_string(), serde_json::json!({"value": 1}));
    config
        .platforms
        .qq
        .plugins
        .insert(REAL_CONTEXT_PLUGIN_ID.to_string(), instance);
    let settings = RealContextPluginSettings {
        reply_threshold: 0.9,
        reply_context_window: 42,
        judge_persona_prompt: "judge persona".to_string(),
        ..RealContextPluginSettings::default()
    };

    apply_real_context_values(&mut config, false, &settings);

    let instance = &config.platforms.qq.plugins[REAL_CONTEXT_PLUGIN_ID];
    assert_eq!(instance.enabled, Some(false));
    assert_eq!(instance.settings["reply_threshold"], 0.9);
    assert_eq!(instance.settings["reply_context_window"], 42);
    assert_eq!(instance.settings["judge_persona_prompt"], "judge persona");
    assert_eq!(instance.settings["future_option"]["value"], 1);
    let (enabled, reparsed) = real_context_values(&config).unwrap();
    assert!(!enabled);
    assert_eq!(reparsed, settings);
}

#[test]
fn real_context_batch_parsers_are_line_based_and_deduplicated() {
    let mappings =
        parse_real_context_identity_lines("# 昵称<Tab>QQ号\nMiyu\t123\n小羽 = 456").unwrap();
    assert_eq!(mappings.len(), 2);
    assert_eq!(mappings[0].nickname, "Miyu");
    assert_eq!(mappings[0].user_id, 123);
    assert!(parse_real_context_identity_lines("Miyu\t123\nMiyu\t456").is_err());
    assert!(parse_real_context_identity_lines("Miyu 123").is_err());

    assert_eq!(
        parse_real_context_string_lines("晚安\n 晚安 \nMiyu", 128).unwrap(),
        vec!["晚安", "Miyu"]
    );
}

/// 09-23 用户反馈：主页写着「本地 · bge-small-zh…」，知识库设置里那一行却说
/// 「未配置 Embedding」——它读的是运行时不用的旧字段
/// `plugins.knowledge_base.embedding_*`。退回修复前这条报红。
#[test]
fn knowledge_base_embedding_row_shows_the_global_model() {
    let mut config = AppConfig::default();
    assert!(config
        .plugins
        .knowledge_base
        .embedding_provider_id
        .is_empty());

    let fields = plugin_fields(&config, "knowledge_base");
    let shown = field_display_value(&fields[9], false);
    assert_eq!(shown, embedding_model_label(&config), "与主页同一口径");
    assert!(
        shown.contains(config.embedding.local_model.trim()),
        "用内置本地模型时应显示它的名字，实际：{shown}"
    );

    // 全局切到远程，这一行跟着变；旧字段依旧是空的。
    config.embedding.provider_id = "remote-embed".to_string();
    config.embedding.model = "bge-m3".to_string();
    let fields = plugin_fields(&config, "knowledge_base");
    assert_eq!(
        field_display_value(&fields[9], false),
        "remote-embed/bge-m3"
    );
}

/// 「语义最低分」「Embedding 超时秒数」运行时读的是全局那份，知识库表单不再摆
/// （用户 09-23 拍板）。表单按下标读回，删两行之后后面的下标要跟着对齐。
#[test]
fn knowledge_base_form_hides_dead_fields_and_keeps_indices_aligned() {
    let mut config = AppConfig::default();
    let mut fields = plugin_fields(&config, "knowledge_base");
    let labels: Vec<&str> = fields.iter().map(|field| field.label).collect();
    assert!(!labels.contains(&t("Minimum semantic score", "语义最低分")));
    assert!(!labels.contains(&t("Embedding timeout (seconds)", "Embedding 超时秒数")));
    assert_eq!(
        fields[13].label,
        t("Strong keyword match threshold", "关键词强命中阈值")
    );

    let min_score = config.plugins.knowledge_base.semantic_min_score;
    fields[13].value = "123".to_string();
    apply_plugin_fields(&mut config, "knowledge_base", &fields).unwrap();
    let kb = &config.plugins.knowledge_base;
    assert_eq!(kb.keyword_strong_score_threshold, 123.0);
    assert_eq!(kb.semantic_min_score, min_score, "隐藏的字段原样保留");
    // 跳转行只展示，不写回旧字段——写了下次加载会被迁移悄悄拷进全局。
    assert!(kb.embedding_provider_id.is_empty() && kb.embedding_model.is_empty());
}
