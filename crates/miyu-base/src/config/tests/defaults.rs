//! 其余默认值与显示设置。

use crate::config::*;

#[test]
fn context_overflow_defaults_to_compact() {
    assert_eq!(ContextConfig::default().on_overflow, "compact");

    let deserialized: ContextConfig = serde_json::from_value(serde_json::json!({})).unwrap();
    assert_eq!(deserialized.on_overflow, "compact");
}

/// 上下文处置按档位二分，不混合：对话走压缩，平台（`pop`）走裁剪。
///
/// 压缩的触发线默认继承 `trim_at_ratio`——09-23 实测把它单独调低（0.8）会让
/// 压缩触发次数翻倍，而每压缩一次前缀就断一次，整体命中率反而从 86.3% 掉到
/// 84.0%。要提高命中率是让压缩更少更晚，不是更早。
#[test]
fn the_compaction_trigger_inherits_the_trim_watermark_by_default() {
    let context = ContextConfig::default();
    assert_eq!(context.compact_at_ratio, None);
    assert_eq!(context.effective_compact_at_ratio(), context.trim_at_ratio);
    assert!(context.compact_force_ratio >= context.effective_compact_at_ratio());
}

/// 单独调压缩水位是允许的，但强制线不能排在它前面。
#[test]
fn a_force_watermark_below_the_compaction_trigger_is_rejected() {
    let mut config = AppConfig::default();
    config.context.compact_at_ratio = Some(0.95);
    config.context.compact_force_ratio = 0.9;
    let error = config.validate().unwrap_err().to_string();
    assert!(error.contains("compact_force_ratio"), "{error}");
}

#[test]
fn vision_timeouts_have_stable_defaults() {
    let vision: VisionPluginConfig = serde_json::from_value(serde_json::json!({})).unwrap();
    // 09-03 用户裁定:三个超时只剩"防僵尸"一个用途,统一一小时。
    assert_eq!(vision.response_header_timeout_seconds, 3600);
    assert_eq!(vision.stream_idle_timeout_seconds, 3600);
    assert_eq!(vision.image_timeout_seconds, 3600);
}

#[test]
fn mixed_context_window_uses_the_global_default_when_model_metadata_is_missing() {
    let mut config = AppConfig::default();
    let provider = &mut config.providers[0];
    let provider_id = provider.id.clone();
    provider.models = vec![
        "miyu-known-window-model".to_string(),
        "miyu-unknown-window-model".to_string(),
    ];
    provider.default_model = provider.models[0].clone();
    provider
        .model_context_window
        .insert(provider.models[0].clone(), 200_000);
    config.active_provider_models = Some(vec![
        ActiveProviderModelConfig {
            provider_id: provider_id.clone(),
            model: provider.models[0].clone(),
        },
        ActiveProviderModelConfig {
            provider_id,
            model: provider.models[1].clone(),
        },
    ]);

    assert_eq!(config.active_context_window().unwrap(), Some(168_000));
    config.providers[0]
        .model_context_window
        .insert("miyu-unknown-window-model".to_string(), 128_000);
    assert_eq!(config.active_context_window().unwrap(), Some(128_000));
}

#[test]
fn display_readable_tool_names_defaults_enabled() {
    let display: DisplayConfig = serde_json::from_str(r#"{"tool_calls":"summary"}"#).unwrap();
    assert_eq!(display.language, "auto");
    assert!(display.readable_tool_names);
    assert!(!display.show_token_usage);
    assert_eq!(display.mixed_model_endpoint_display, "interactive");
    // 09-17:这个数管的从「露几行输出」改成「露几行命令」,默认跟着 10 → 8。
    assert_eq!(display.command_output_lines, 8);

    let display: DisplayConfig = serde_json::from_str(r#"{"command_output_lines":3}"#).unwrap();
    assert_eq!(display.command_output_lines, 3);
    assert!(serde_json::to_string(&display)
        .unwrap()
        .contains(r#""command_output_lines":3"#));

    let mut config = AppConfig::default();
    config.display.command_output_lines = MAX_COMMAND_OUTPUT_LINES + 1;
    assert!(config.validate().is_err());

    let display: DisplayConfig = serde_json::from_str(r#"{"show_token_usage":true}"#).unwrap();
    assert!(display.show_token_usage);

    let display: DisplayConfig =
        serde_json::from_str(r#"{"show_mixed_model_endpoint":false}"#).unwrap();
    assert_eq!(display.mixed_model_endpoint_display, "off");

    let display: DisplayConfig =
        serde_json::from_str(r#"{"show_mixed_model_endpoint":true}"#).unwrap();
    assert_eq!(display.mixed_model_endpoint_display, "all");
}

#[test]
fn display_language_roundtrips_and_rejects_unknown_values() {
    let display: DisplayConfig = serde_json::from_str(r#"{"language":"zh"}"#).unwrap();
    assert_eq!(display.language, "zh");
    assert!(serde_json::to_string(&display)
        .unwrap()
        .contains(r#""language":"zh""#));

    let mut config = AppConfig::default();
    config.display.language = "fr".to_string();
    assert!(config.validate().is_err());
    config.display.language.clear();
    assert!(config.validate().is_err());
}

#[test]
fn display_language_hint_reads_jsonc_without_loading_full_config() {
    let temp = tempfile::tempdir().unwrap();
    let config_file = temp.path().join("config.jsonc");
    std::fs::write(
        &config_file,
        "{\n  // UI preference\n  \"display\": { \"language\": \"en\" }\n}\n",
    )
    .unwrap();
    let paths = MiyuPaths {
        root_dir: temp.path().to_path_buf(),
        config_dir: temp.path().to_path_buf(),
        config_file,
        skills_dir: temp.path().join("skills"),
        data_dir: temp.path().join("data"),
        cache_dir: temp.path().join("cache"),
        state_dir: temp.path().join("state"),
        pictures_dir: temp.path().join("pictures"),
        fish_hook_file: temp.path().join("miyu.fish"),
        bash_hook_file: temp.path().join("miyu.bash"),
        zsh_hook_file: temp.path().join("miyu.zsh"),
        scripts_dir: temp.path().join("scripts"),
        system_scripts_dir: temp.path().join("system-scripts"),
    };

    assert_eq!(
        AppConfig::display_language_hint(&paths).as_deref(),
        Some("en")
    );
}

#[test]
fn memory_diary_lifecycle_defaults_and_roundtrip_are_stable() {
    let defaults: MemoryConfig = serde_json::from_str("{}").unwrap();
    assert_eq!(defaults.diary_batch_size, 14);
    assert_eq!(defaults.short_diary_retention_days, 14);
    assert_eq!(defaults.diary_promotion_recalls, 3);
    assert_eq!(defaults.organizer_timeout_seconds, 120);
    assert!(!defaults.auto_skill_enabled);

    let parsed: MemoryConfig = serde_json::from_str(
        r#"{
            "diary_batch_size": 20,
            "short_diary_retention_days": 7,
            "diary_promotion_recalls": 4,
            "organizer_timeout_seconds": 90
        }"#,
    )
    .unwrap();
    assert_eq!(parsed.diary_batch_size, 20);
    assert_eq!(parsed.short_diary_retention_days, 7);
    assert_eq!(parsed.diary_promotion_recalls, 4);
    assert_eq!(parsed.organizer_timeout_seconds, 90);
}

/// 更新的二进制先把版本号抬上去、加了当前版本仍不认识的字段，这边读同一份配置
/// 不拒绝、不降级，不认识的字段原样留着写回——两个二进制来回用谁也不弄丢谁的。
#[test]
fn a_newer_config_is_read_as_is_and_its_unknown_fields_survive() {
    let raw = serde_json::json!({
        "config_version": 99,
        "active_provider": "stub",
        "providers": [],
        "future_top_level_option": true,
        "display": {"future_display_option": false, "reasoning": "summary"},
    });
    let mut config: AppConfig = serde_json::from_value(raw).expect("更新的配置读不进");
    config.migrate().expect("更新的配置不该被拒绝");
    assert_eq!(config.config_version, 99, "版本号被降了");
    assert_eq!(
        config.extra.get("future_top_level_option"),
        Some(&serde_json::Value::Bool(true)),
        "顶层的陌生字段没留住"
    );
    assert!(!config.display.expand_reasoning, "默认不展开思考");
    let out = serde_json::to_value(&config).expect("写不出");
    assert_eq!(out["config_version"], 99);
    assert_eq!(
        out["future_top_level_option"],
        serde_json::Value::Bool(true)
    );
    assert_eq!(
        out["display"]["future_display_option"],
        serde_json::Value::Bool(false),
        "display 里的陌生字段没写回"
    );
}

/// 自己认识的版本照旧迁移、照旧盖成当前版本。
#[test]
fn an_older_config_still_migrates_to_the_current_version() {
    let raw = serde_json::json!({"config_version": 1, "active_provider": "stub", "providers": []});
    let mut config: AppConfig = serde_json::from_value(raw).unwrap();
    config.migrate().unwrap();
    assert_eq!(config.config_version, CURRENT_CONFIG_VERSION);
    assert!(config.extra.is_empty());
}

/// 通知提示音选哪一声：关了静音、配了文件放文件、文件没了退回内置音。
/// 「退回」这条是要紧的——路径写错变成哑的，人只会以为功能坏了。
#[test]
fn notification_tone_falls_back_to_the_builtin_sound() {
    use crate::notify::{NotifySound, NotifyTone};

    let builtin = |sound| match NotificationsConfig::default().tone(sound) {
        // 取不到家目录的机器上退到主题音,那台机器上这条断言不成立也正常。
        NotifyTone::File(path) => Some(path),
        _ => None,
    };
    let mut notifications = NotificationsConfig::default();
    if let Some(path) = builtin(NotifySound::TurnDone) {
        assert!(
            path.ends_with("cache/sounds/wake.wav"),
            "默认走内置音,落在缓存里: {}",
            path.display()
        );
        assert!(path.is_file(), "内置音得真的落到盘上,播放器只认文件");
        // 用户 09-18 拍板:回复完成和提问都用 wake 那一声,盘上只落一份。
        assert_eq!(builtin(NotifySound::Question), Some(path));
    }

    notifications.sound = false;
    assert_eq!(
        notifications.tone(NotifySound::TurnDone),
        NotifyTone::Silent
    );

    notifications.sound = true;
    notifications.sound_file = "/nonexistent/miyu-ding.wav".into();
    assert_eq!(
        notifications.tone(NotifySound::TurnDone),
        NotificationsConfig::default().tone(NotifySound::TurnDone),
        "路径写错了也得响,退回默认那一声,不能变哑的"
    );

    let existing = std::env::temp_dir().join("miyu-tone-test.wav");
    std::fs::write(&existing, b"RIFF").unwrap();
    notifications.sound_file = existing.to_string_lossy().into_owned();
    assert_eq!(
        notifications.tone(NotifySound::TurnDone),
        NotifyTone::File(existing.clone())
    );
    // 提问没单配就跟着完成那一声走。
    assert_eq!(
        notifications.tone(NotifySound::Question),
        NotifyTone::File(existing.clone())
    );

    let other = std::env::temp_dir().join("miyu-tone-test-question.wav");
    std::fs::write(&other, b"RIFF").unwrap();
    notifications.question_sound_file = other.to_string_lossy().into_owned();
    assert_eq!(
        notifications.tone(NotifySound::Question),
        NotifyTone::File(other.clone())
    );
    assert_eq!(
        notifications.tone(NotifySound::TurnDone),
        NotifyTone::File(existing.clone()),
        "单配的只管提问那一声"
    );
    let _ = std::fs::remove_file(existing);
    let _ = std::fs::remove_file(other);
}

/// 关掉提示音不该把通知本身也关掉,而且老配置读上来是开的。
#[test]
fn notification_sound_defaults_to_on() {
    let notifications: NotificationsConfig = serde_json::from_value(serde_json::json!({})).unwrap();
    assert!(notifications.sound);
    assert!(notifications.enabled);
    assert!(notifications.sound_file.is_empty());
}
