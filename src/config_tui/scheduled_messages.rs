//! QQ 定时消息插件的设置界面。
//!
//! 与其它插件不同，它的核心是**任务列表**而不是一组标量，所以不走单表单
//! 三段式：外层是「开关 + 任务行 + 新增」的菜单，任务行进出用同一张表单
//! （编辑态末尾多一个「删除」布尔项）。写回后统一走 `AppConfig::validate`
//! ——格式校验只在 `validate_qq_scheduled_messages_plugin_config` 一处。

use crate::config_tui::*;

const SCHEDULED_ID: &str = miyu_base::config::QQ_SCHEDULED_MESSAGES_PLUGIN_ID;

fn plugin_tasks(config: &AppConfig) -> Vec<serde_json::Value> {
    config
        .platforms
        .qq
        .plugins
        .get(SCHEDULED_ID)
        .and_then(|instance| instance.settings.get("tasks"))
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn task_row_label(task: &serde_json::Value) -> String {
    let conversation = task
        .get("conversation")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("?");
    let times = task
        .get("times")
        .and_then(serde_json::Value::as_array)
        .map(|times| {
            times
                .iter()
                .filter_map(serde_json::Value::as_str)
                .collect::<Vec<_>>()
                .join(",")
        })
        .unwrap_or_default();
    let message: String = task
        .get("message")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .chars()
        .take(16)
        .collect();
    format!("{conversation} | {times} | {message}")
}

/// 星期归一化:长名/大小写 → 规范短名(mon..sun)。存量配置里的长名在
/// 下一次表单往返时自动迁移成短名。
fn normalize_day(day: &str) -> String {
    let lower = day.trim().to_ascii_lowercase();
    match lower.as_str() {
        "monday" => "mon",
        "tuesday" => "tue",
        "wednesday" => "wed",
        "thursday" => "thu",
        "friday" => "fri",
        "saturday" => "sat",
        "sunday" => "sun",
        other => return other.to_string(),
    }
    .to_string()
}

fn field_list(value: &str) -> Vec<String> {
    value
        .split([',', '，', ' '])
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_string)
        .collect()
}

/// 任务的会话类型与号码。缺省当群聊。
fn conversation_parts(task: Option<&serde_json::Value>) -> (bool, String) {
    let conversation = task
        .and_then(|task| task.get("conversation"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    match conversation.split_once(':') {
        Some(("private", id)) => (false, id.to_string()),
        Some((_, id)) => (true, id.to_string()),
        None => (true, conversation.to_string()),
    }
}

/// 表单 → 任务 JSON。返回 None 表示用户勾了「移除」。
/// `group:`/`private:` 前缀由代码拼——让用户手打前缀曾直接撞校验器报错。
/// 会话类型是表单首项(choices 字段,回车弹菜单)——编辑存量任务不再被
/// 强制先选一遍类型(用户 08-20 点名)。
fn task_from_fields(fields: &[Field]) -> Result<serde_json::Value> {
    let is_group = fields[0].value != t("Private chat", "私聊");
    let target_id = fields[1].value.trim();
    if target_id.is_empty() || !target_id.bytes().all(|byte| byte.is_ascii_digit()) {
        anyhow::bail!(t(
            "The conversation id must be a plain number (group number or QQ number).",
            "会话号码必须是纯数字（群号或 QQ 号）。"
        ));
    }
    let mut task = serde_json::Map::new();
    task.insert(
        "conversation".to_string(),
        serde_json::json!(format!(
            "{}:{target_id}",
            if is_group { "group" } else { "private" }
        )),
    );
    task.insert(
        "times".to_string(),
        serde_json::json!(field_list(&fields[2].value)),
    );
    task.insert(
        "message".to_string(),
        serde_json::json!(fields[3].value.trim()),
    );
    let days = field_list(&fields[4].value)
        .iter()
        .map(|day| normalize_day(day))
        .collect::<Vec<_>>();
    if !days.is_empty() {
        task.insert("days".to_string(), serde_json::json!(days));
    }
    let account = fields[5].value.trim();
    if !account.is_empty() {
        let account: i64 = account
            .parse()
            .map_err(|_| anyhow::anyhow!(t("Invalid account number.", "账号必须是数字。")))?;
        task.insert("account".to_string(), serde_json::json!(account));
    }
    Ok(serde_json::Value::Object(task))
}

fn task_fields(task: Option<&serde_json::Value>) -> Vec<Field> {
    let text = |key: &str| {
        task.and_then(|task| task.get(key))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let list = |key: &str| {
        task.and_then(|task| task.get(key))
            .and_then(serde_json::Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .unwrap_or_default()
    };
    let account = task
        .and_then(|task| task.get("account"))
        .and_then(serde_json::Value::as_i64)
        .map(|account| account.to_string())
        .unwrap_or_default();
    let (is_group, target_id) = conversation_parts(task);
    let kind_label = if is_group {
        t("Group chat", "群聊")
    } else {
        t("Private chat", "私聊")
    };
    let fields = vec![
        Field::new(t("Conversation type", "会话类型"), kind_label.to_string())
            .choices(&[t("Group chat", "群聊"), t("Private chat", "私聊")]),
        Field::new(t("Group number / QQ number", "群号 / QQ 号"), target_id),
        Field::new(
            t(
                "Times (HH:MM, comma separated)",
                "时间点（HH:MM，逗号分隔）",
            ),
            list("times"),
        ),
        Field::new(t("Message text", "发送内容"), text("message")),
        // 回车弹多选菜单(08-23 用户点名:手填 mon..sun 太不方便);读取时
        // 顺手把存量长名归一成短名。
        Field::new(
            t(
                "Weekdays (Enter to pick, empty = every day)",
                "星期（回车多选，留空=每天）",
            ),
            field_list(&list("days"))
                .iter()
                .map(|day| normalize_day(day))
                .collect::<Vec<_>>()
                .join(","),
        )
        .multi_choices(&["mon", "tue", "wed", "thu", "fri", "sat", "sun"]),
        Field::new(
            t(
                "Bot account (empty = first connected)",
                "机器人账号（留空=第一个已连接）",
            ),
            account,
        ),
    ];
    // "保存时移除"复选框已撤(08-23 用户点名语义费解):删除统一走列表 d 键。
    fields
}

fn write_back(
    ui: &mut Ui,
    config: &mut AppConfig,
    enabled: bool,
    tasks: Vec<serde_json::Value>,
) -> Result<()> {
    let mut candidate = config.clone();
    let instance = candidate
        .platforms
        .qq
        .plugins
        .entry(SCHEDULED_ID.to_string())
        .or_default();
    // 缺省是关闭,所以显式记录 true;false 时保留显式值,菜单状态才不骗人。
    instance.enabled = Some(enabled);
    instance
        .settings
        .insert("tasks".to_string(), serde_json::json!(tasks));
    if let Err(error) = candidate.validate() {
        message(ui, &error.to_string())?;
        return Ok(());
    }
    *config = candidate;
    Ok(())
}

pub(in crate::config_tui) fn edit_scheduled_messages(
    ui: &mut Ui,
    config: &mut AppConfig,
) -> Result<()> {
    let mut selected = 0usize;
    loop {
        let enabled = config
            .platforms
            .qq
            .plugins
            .get(SCHEDULED_ID)
            .map(|instance| instance.enabled_or(false))
            .unwrap_or(false);
        let tasks = plugin_tasks(config);
        let mut options = vec![format!(
            "{}: {}",
            t("Plugin", "插件状态"),
            if enabled {
                t("enabled", "已启用")
            } else {
                t("disabled", "未启用")
            }
        )];
        options.extend(tasks.iter().map(task_row_label));
        options.push(t("Add task…", "新增任务…").to_string());
        selected = selected.min(options.len() - 1);
        draw_menu(
            ui,
            t(" QQ SCHEDULED MESSAGES ", " QQ 定时消息 "),
            &options,
            selected,
            t(
                "[Enter]toggle/edit [d]delete [j/k]move [q]back",
                "[Enter]切换/编辑 [d]删除 [j/k]移动 [q]返回",
            ),
        )?;
        match read_key(ui)? {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
            KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => selected = (selected + 1).min(options.len() - 1),
            KeyCode::Enter | KeyCode::Char(' ') => {
                if selected == 0 {
                    write_back(ui, config, !enabled, tasks)?;
                } else if selected <= tasks.len() {
                    let index = selected - 1;
                    let mut fields = task_fields(tasks.get(index));
                    if run_edit_form(ui, t(" EDIT TASK ", " 编辑任务 "), &mut fields)? {
                        let mut tasks = tasks;
                        match task_from_fields(&fields) {
                            Ok(task) => tasks[index] = task,
                            Err(error) => {
                                message(ui, &error.to_string())?;
                                continue;
                            }
                        }
                        write_back(ui, config, enabled, tasks)?;
                    }
                } else {
                    let mut fields = task_fields(None);
                    if run_form(ui, t(" ADD TASK ", " 新增任务 "), &mut fields)? {
                        match task_from_fields(&fields) {
                            Ok(task) => {
                                let mut tasks = tasks;
                                tasks.push(task);
                                write_back(ui, config, enabled, tasks)?;
                            }
                            Err(error) => message(ui, &error.to_string())?,
                        }
                    }
                }
            }
            KeyCode::Char('d') if selected >= 1 && selected <= tasks.len() => {
                let index = selected - 1;
                let mut tasks = tasks;
                tasks.remove(index);
                write_back(ui, config, enabled, tasks)?;
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn form_round_trips_a_task() {
        let task = serde_json::json!({
            "conversation": "group:123",
            "times": ["08:30", "21:00"],
            "message": "早安",
            "days": ["mon", "fri"],
            "account": 10001
        });
        let (is_group, _) = conversation_parts(Some(&task));
        assert!(is_group);
        let fields = task_fields(Some(&task));
        assert_eq!(fields[0].value, t("Group chat", "群聊"));
        assert_eq!(fields[1].value, "123");
        assert_eq!(fields[2].value, "08:30,21:00");
        assert_eq!(fields[4].value, "mon,fri");
        assert_eq!(fields[5].value, "10001");
        let rebuilt = task_from_fields(&fields).unwrap();
        assert_eq!(rebuilt, task);
    }

    #[test]
    fn private_conversation_round_trips_and_bad_ids_are_rejected() {
        let task = serde_json::json!({
            "conversation": "private:10001", "times": ["07:00"], "message": "hi"
        });
        let (is_group, target_id) = conversation_parts(Some(&task));
        assert!(!is_group);
        assert_eq!(target_id, "10001");
        let fields = task_fields(Some(&task));
        assert_eq!(fields[0].value, t("Private chat", "私聊"));
        assert_eq!(fields[1].value, "10001");
        let rebuilt = task_from_fields(&fields).unwrap();
        assert_eq!(rebuilt, task);

        let mut bad = task_fields(Some(&task));
        bad[1].value = "not-a-number".to_string();
        assert!(task_from_fields(&bad).is_err());
        bad[1].value = "".to_string();
        assert!(task_from_fields(&bad).is_err());
    }

    /// 存量长名/大小写星期在表单往返时自动迁移成规范短名。
    #[test]
    fn legacy_weekday_names_are_normalized_on_round_trip() {
        let task = serde_json::json!({
            "conversation": "group:1", "times": ["08:00"], "message": "hi",
            "days": ["Monday", "FRIDAY", "sun"]
        });
        let fields = task_fields(Some(&task));
        assert_eq!(fields[4].value, "mon,fri,sun");
        let rebuilt = task_from_fields(&fields).unwrap();
        assert_eq!(rebuilt["days"], serde_json::json!(["mon", "fri", "sun"]));
    }

    #[test]
    fn optional_fields_are_omitted_when_empty() {
        let fields = vec![
            Field::new("kind", t("Private chat", "私聊").to_string()),
            Field::new("id", "9".to_string()),
            Field::new("t", "07:15".to_string()),
            Field::new("m", "hello".to_string()),
            Field::new("d", "".to_string()),
            Field::new("a", "".to_string()),
        ];
        let task = task_from_fields(&fields).unwrap();
        assert_eq!(
            task,
            serde_json::json!({ "conversation": "private:9", "times": ["07:15"], "message": "hello" })
        );
    }
}
