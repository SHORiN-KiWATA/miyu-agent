//! 平台插件的设置结构与校验。
//!
//! 每个插件的设置存成自由 JSON，由 `PLATFORM_PLUGIN_VALIDATORS` 按 ID 分派到
//! 对应的校验函数——这样加插件不用改配置的类型定义。
//!
//! 迁移逻辑（`migrate_*`、`merge_*`、`DEPRECATED_REAL_CONTEXT_SETTINGS`）是这里
//! 最容易出错的部分：旧配置里的键换了位置或改了单位，必须能一路读上来，而且
//! **迁移过一次就不能再迁第二次**。

mod qq;
mod real_context;
pub use qq::*;
pub use real_context::*;

use crate::config::*;

pub type PlatformPluginsConfig = BTreeMap<String, PlatformPluginInstanceConfig>;

pub(crate) type PlatformPluginConfigValidator = fn(&PlatformPluginInstanceConfig) -> Result<()>;

pub const REAL_CONTEXT_PLUGIN_ID: &str = "real_context";

pub const QQ_MESSAGE_HISTORY_PLUGIN_ID: &str = "qq_message_history";

pub const QQ_GROUP_MANAGEMENT_PLUGIN_ID: &str = "qq_group_management";

pub const QQ_MESSAGE_RECALL_PLUGIN_ID: &str = "qq_message_recall";

pub const QQ_MEME_COLLECTOR_PLUGIN_ID: &str = "qq_meme_collector";

pub const QQ_GROUP_JOIN_APPROVAL_PLUGIN_ID: &str = "qq_group_join_approval";

pub const QQ_SCHEDULED_MESSAGES_PLUGIN_ID: &str = "qq_scheduled_messages";

pub(crate) const PLATFORM_PLUGIN_VALIDATORS: &[(&str, PlatformPluginConfigValidator)] = &[
    ("reply_processor", validate_reply_processor_plugin_config),
    (REAL_CONTEXT_PLUGIN_ID, validate_real_context_plugin_config),
    (
        QQ_MESSAGE_HISTORY_PLUGIN_ID,
        validate_qq_message_history_plugin_config,
    ),
    (
        QQ_GROUP_MANAGEMENT_PLUGIN_ID,
        validate_qq_group_management_plugin_config,
    ),
    (
        QQ_MESSAGE_RECALL_PLUGIN_ID,
        validate_qq_message_recall_plugin_config,
    ),
    (
        QQ_MEME_COLLECTOR_PLUGIN_ID,
        validate_qq_meme_collector_plugin_config,
    ),
    (
        QQ_GROUP_JOIN_APPROVAL_PLUGIN_ID,
        validate_qq_group_join_approval_plugin_config,
    ),
    (
        QQ_SCHEDULED_MESSAGES_PLUGIN_ID,
        validate_qq_scheduled_messages_plugin_config,
    ),
];

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PlatformPluginInstanceConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub settings: serde_json::Map<String, serde_json::Value>,
}

impl PlatformPluginInstanceConfig {
    pub fn is_empty(&self) -> bool {
        self.enabled.is_none() && self.settings.is_empty()
    }

    pub fn enabled_or(&self, default: bool) -> bool {
        self.enabled.unwrap_or(default)
    }
}

pub(crate) fn validate_reply_processor_plugin_config(
    instance: &PlatformPluginInstanceConfig,
) -> Result<()> {
    let settings = &instance.settings;
    for key in [
        "default_enabled",
        "followup_mention",
        "strip_period",
        "context_notice",
        "send_tool_intercept",
    ] {
        if settings.get(key).is_some_and(|value| !value.is_boolean()) {
            bail!("platform plugin reply_processor.{key} must be a boolean");
        }
    }
    for (key, min, max) in [
        ("threshold", 1_u64, 100_000_u64),
        ("font_size", 24, 56),
        ("code_font_size", 20, 46),
        ("padding", 36, 120),
        ("ttl_hours", 1, 168),
        ("max_records", 1, 10),
    ] {
        if let Some(value) = settings.get(key) {
            let value = value.as_u64().with_context(|| {
                format!("platform plugin reply_processor.{key} must be an unsigned integer")
            })?;
            if !(min..=max).contains(&value) {
                bail!("platform plugin reply_processor.{key} must be between {min} and {max}");
            }
        }
    }
    validate_plugin_string_choice(settings, "mode", &["image", "forward"])?;
    validate_plugin_string_choice(settings, "theme", &["paper", "light", "dark"])?;
    for key in ["font", "title_font", "code_font", "emoji_font"] {
        if let Some(value) = settings.get(key) {
            let value = value.as_str().with_context(|| {
                format!("platform plugin reply_processor.{key} must be a string")
            })?;
            if value.len() > 4_096 || value.contains('\0') {
                bail!("platform plugin reply_processor.{key} is invalid");
            }
        }
    }
    Ok(())
}

/// 定时消息插件的配置校验。格式错误在保存/启动阶段就要炸出来，运行时的
/// 解析器只做防御性跳过。时间/星期解析逻辑刻意与运行侧保持独立小实现，
/// 避免 config 层反向依赖 platforms 层。
pub(crate) fn validate_qq_scheduled_messages_plugin_config(
    instance: &PlatformPluginInstanceConfig,
) -> Result<()> {
    let Some(tasks) = instance.settings.get("tasks") else {
        return Ok(());
    };
    let tasks = tasks
        .as_array()
        .context("platform plugin qq_scheduled_messages.tasks must be an array")?;
    if tasks.len() > 64 {
        bail!("platform plugin qq_scheduled_messages.tasks supports at most 64 tasks");
    }
    for (index, task) in tasks.iter().enumerate() {
        let task = task.as_object().with_context(|| {
            format!("platform plugin qq_scheduled_messages.tasks[{index}] must be an object")
        })?;
        let conversation = task
            .get("conversation")
            .and_then(serde_json::Value::as_str)
            .with_context(|| {
                format!(
                    "platform plugin qq_scheduled_messages.tasks[{index}].conversation must be a string like \"group:123\" or \"private:456\""
                )
            })?;
        let valid_conversation = conversation
            .trim()
            .split_once(':')
            .is_some_and(|(kind, id)| {
                matches!(kind, "group" | "private")
                    && !id.is_empty()
                    && id.bytes().all(|byte| byte.is_ascii_digit())
            });
        if !valid_conversation {
            bail!(
                "platform plugin qq_scheduled_messages.tasks[{index}].conversation must be \"group:<id>\" or \"private:<id>\""
            );
        }
        let times = task
            .get("times")
            .and_then(serde_json::Value::as_array)
            .with_context(|| {
                format!("platform plugin qq_scheduled_messages.tasks[{index}].times must be an array of \"HH:MM\" strings")
            })?;
        if times.is_empty() || times.len() > 48 {
            bail!(
                "platform plugin qq_scheduled_messages.tasks[{index}].times must contain 1 to 48 entries"
            );
        }
        for time in times {
            let valid_time = time.as_str().is_some_and(|time| {
                time.trim().split_once(':').is_some_and(|(hour, minute)| {
                    hour.parse::<u32>().is_ok_and(|hour| hour < 24)
                        && minute.parse::<u32>().is_ok_and(|minute| minute < 60)
                })
            });
            if !valid_time {
                bail!(
                    "platform plugin qq_scheduled_messages.tasks[{index}].times entries must be \"HH:MM\" (got {time})"
                );
            }
        }
        let message = task
            .get("message")
            .and_then(serde_json::Value::as_str)
            .with_context(|| {
                format!(
                    "platform plugin qq_scheduled_messages.tasks[{index}].message must be a string"
                )
            })?;
        if message.trim().is_empty() || message.chars().count() > 4_096 {
            bail!(
                "platform plugin qq_scheduled_messages.tasks[{index}].message must be non-empty and at most 4096 characters"
            );
        }
        if let Some(days) = task.get("days") {
            let days = days.as_array().with_context(|| {
                format!("platform plugin qq_scheduled_messages.tasks[{index}].days must be an array of weekday names")
            })?;
            if days.is_empty() {
                bail!(
                    "platform plugin qq_scheduled_messages.tasks[{index}].days must not be empty when present"
                );
            }
            for day in days {
                let valid_day = day.as_str().is_some_and(|day| {
                    matches!(
                        day.trim().to_ascii_lowercase().as_str(),
                        "mon"
                            | "monday"
                            | "tue"
                            | "tuesday"
                            | "wed"
                            | "wednesday"
                            | "thu"
                            | "thursday"
                            | "fri"
                            | "friday"
                            | "sat"
                            | "saturday"
                            | "sun"
                            | "sunday"
                    )
                });
                if !valid_day {
                    bail!(
                        "platform plugin qq_scheduled_messages.tasks[{index}].days entries must be weekday names like \"mon\" (got {day})"
                    );
                }
            }
        }
        if let Some(account) = task.get("account") {
            if !account.is_i64() {
                bail!(
                    "platform plugin qq_scheduled_messages.tasks[{index}].account must be an integer QQ account id"
                );
            }
        }
    }
    Ok(())
}

pub(crate) fn validate_plugin_string_choice(
    settings: &serde_json::Map<String, serde_json::Value>,
    key: &str,
    choices: &[&str],
) -> Result<()> {
    let Some(value) = settings.get(key) else {
        return Ok(());
    };
    let value = value
        .as_str()
        .with_context(|| format!("platform plugin reply_processor.{key} must be a string"))?;
    if !choices.contains(&value) {
        bail!(
            "platform plugin reply_processor.{key} must be one of: {}",
            choices.join(", ")
        );
    }
    Ok(())
}
