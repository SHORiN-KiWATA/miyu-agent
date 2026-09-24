//! 通用设置项。
//!
//! 界面语言、工具加载模式、混合端点显示——都是单值开关，没有跨项约束，所以
//! 一个表单装得下。

use crate::config_tui::*;

/// true = save and exit, false = discard and exit. A choice is mandatory:
/// `q`/`Esc` are ignored so an accidental key press cannot lose edits.
pub(in crate::config_tui) fn confirm_save_on_exit(ui: &mut Ui) -> Result<bool> {
    let options = [
        t("Save", "保存").to_string(),
        t("Discard", "不保存").to_string(),
    ];
    let mut selected = 0usize;
    loop {
        draw_menu(
            ui,
            t(" SAVE EDITED CHANGES? ", " 是否保存已编辑内容 "),
            &options,
            selected,
            t("[j/k]move [Enter]confirm", "[j/k]移动 [Enter]确认"),
        )?;
        match read_key(ui)? {
            KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => selected = (selected + 1).min(1),
            KeyCode::Enter => return Ok(selected == 0),
            _ => {}
        }
    }
}

pub(in crate::config_tui) fn edit_settings(ui: &mut Ui, config: &mut AppConfig) -> Result<()> {
    let language = language_choice_value(&config.display.language).unwrap_or("auto");
    let mut fields = vec![
        // 界面语言排第一(用户 09-23 拍板):它跟 WebUI 的「语言」卡同一个字段
        // (display.language),而且整份界面都用它——放最上面才找得到。
        Field::new(t("Interface language", "界面语言"), language.to_string())
            .choices(&["auto", "en", "zh"]),
        Field::boolean(t("Enable tools", "工具启用"), config.tools.enabled),
        Field::new(
            t("Maximum tool rounds", "工具最大轮数"),
            config.tools.max_rounds.to_string(),
        ),
        Field::new(
            t("Tool loading mode", "工具加载模式"),
            normalize_tools_loading_mode(&config.tools.loading_mode),
        )
        .choices(&["full", "stub"]),
        Field::boolean(
            t("Remember loaded tools", "记住已加载工具"),
            config.tools.persist_loaded_tools,
        ),
        Field::boolean(t("Enable skills", "Skills 启用"), config.skills.enabled),
        Field::boolean(
            t("Allow command execution", "允许执行命令"),
            config.skills.allow_command_execution,
        ),
        // 三值档（隐藏/摘要/完整）09-17 拆成布尔、隐藏档删掉——用户原话
        //「是否展开思考内容 / 是否展开工具内容 / 是否缩起成 Worked for，
        // 这样更简洁」。三位互不相干：思考以后要从时间线里搬出去。
        Field::boolean(
            t("Expand reasoning", "展开思考内容"),
            config.display.expand_reasoning,
        ),
        Field::boolean(
            t("Expand tool details", "展开工具内容"),
            config.display.expand_tool_calls,
        ),
        Field::new(
            t("Thinking scroll lines", "思考滚动显示行数"),
            config.display.thinking_scroll_lines.to_string(),
        ),
        Field::new(
            t("Command lines", "命令显示行数"),
            config.display.command_output_lines.to_string(),
        ),
        Field::boolean(
            t("Readable tool names", "工具名可读显示"),
            config.display.readable_tool_names,
        ),
        Field::boolean(
            t(
                "Show token usage in shell conversations",
                "Shell 无缝对话显示 Token 计数",
            ),
            config.display.show_token_usage,
        ),
        Field::new(
            t(
                "Show current provider/model in Mixed mode",
                "Mixed 时显示本次供应商/模型",
            ),
            parse_mixed_endpoint_display(&config.display.mixed_model_endpoint_display),
        )
        .choices(&["off", "interactive", "all"]),
        // Appended rather than inserted: the read-back below is positional.
        Field::new(
            t(
                "Turns replayed when reopening the TUI",
                "重开 TUI 回放的轮数",
            ),
            config.display.repl_replay_turns.to_string(),
        ),
        Field::boolean(
            t("Block dangerous commands", "高危命令拦截"),
            config.tools.block_dangerous_commands,
        ),
        Field::boolean(
            t(
                "Fold finished steps into a one-line summary",
                "过程收起成一行摘要",
            ),
            config.display.fold_timeline,
        ),
        Field::new(
            t("Terminal session default mode", "终端集成会话默认模式"),
            if config.terminal_session_is_dev() {
                "dev"
            } else {
                "normal"
            }
            .to_string(),
        )
        .choices(&["normal", "dev"]),
        // 09-23:没对会话说过要不要沙盒的会话,读全盘、只能写 `home/<属主>/workspace`。
        Field::boolean(
            t("Sandbox mode on by default", "默认开启沙盒模式"),
            config.tools.sandbox.default_enabled,
        ),
        // 09-23:跨会话 AI 消息(收到的、发出去的)先露几行正文。
        Field::new(
            t(
                "Cross-session AI message preview lines",
                "跨会话AI消息预览行数",
            ),
            config.display.cross_session_preview_lines.to_string(),
        ),
    ];
    // The read-back below is by index, so an insert in the middle silently
    // writes every later value into the wrong setting. This catches that in
    // debug builds; new fields go on the end (09-23 的例外:界面语言按用户要求
    // 提到第一行,后面的索引一并重排,见下面逐行对应)。
    debug_assert_eq!(
        fields.len(),
        20,
        "global settings fields changed: update the positional read-back below"
    );
    run_form_without_buttons(ui, t(" GLOBAL SETTINGS ", " 全局设置 "), &mut fields)?;
    config.display.language = language_choice_value(&fields[0].value)
        .unwrap_or("auto")
        .to_string();
    config.tools.enabled = parse_bool_field(&fields[1].value)?;
    config.tools.max_rounds = fields[2].value.trim().parse::<usize>()?;
    config.tools.loading_mode = normalize_tools_loading_mode(&fields[3].value);
    config.tools.persist_loaded_tools = parse_bool_field(&fields[4].value)?;
    config.skills.enabled = parse_bool_field(&fields[5].value)?;
    config.skills.allow_command_execution = parse_bool_field(&fields[6].value)?;
    config.display.expand_reasoning = parse_bool_field(&fields[7].value)?;
    config.display.expand_tool_calls = parse_bool_field(&fields[8].value)?;
    config.display.thinking_scroll_lines = fields[9]
        .value
        .trim()
        .parse::<usize>()?
        .min(MAX_THINKING_SCROLL_LINES);
    config.display.command_output_lines = fields[10]
        .value
        .trim()
        .parse::<usize>()?
        .min(MAX_COMMAND_OUTPUT_LINES);
    config.display.readable_tool_names = parse_bool_field(&fields[11].value)?;
    config.display.show_token_usage = parse_bool_field(&fields[12].value)?;
    config.display.mixed_model_endpoint_display = parse_mixed_endpoint_display(&fields[13].value);
    config.display.repl_replay_turns = fields[14]
        .value
        .trim()
        .parse::<usize>()?
        .min(MAX_REPL_REPLAY_TURNS);
    config.tools.block_dangerous_commands = parse_bool_field(&fields[15].value)?;
    config.display.fold_timeline = parse_bool_field(&fields[16].value)?;
    config.terminal_session_mode = if fields[17].value.trim().eq_ignore_ascii_case("dev") {
        "dev"
    } else {
        "normal"
    }
    .to_string();
    config.tools.sandbox.default_enabled = parse_bool_field(&fields[18].value)?;
    config.display.cross_session_preview_lines = fields[19]
        .value
        .trim()
        .parse::<usize>()?
        .min(MAX_CROSS_SESSION_PREVIEW_LINES);
    Ok(())
}

pub(in crate::config_tui) fn language_choice_label(value: &str, zh: bool) -> Option<&'static str> {
    match (value.trim(), zh) {
        ("auto", false) => Some("Auto"),
        ("auto", true) => Some("自动"),
        ("en", false) => Some("English"),
        ("en", true) => Some("英语"),
        ("zh", false) => Some("Simplified Chinese"),
        ("zh", true) => Some("简体中文"),
        _ => None,
    }
}

pub(in crate::config_tui) fn language_choice_value(value: &str) -> Option<&'static str> {
    match value.trim() {
        "auto" | "Auto" | "自动" => Some("auto"),
        "en" | "English" | "英语" => Some("en"),
        "zh" | "Simplified Chinese" | "简体中文" => Some("zh"),
        _ => None,
    }
}

pub(in crate::config_tui) fn parse_mixed_endpoint_display(value: &str) -> String {
    match value.trim() {
        "关" | "Off" | "off" => "off".to_string(),
        "全部模式" | "All modes" | "all" => "all".to_string(),
        _ => "interactive".to_string(),
    }
}

pub(in crate::config_tui) fn normalize_tools_loading_mode(value: &str) -> String {
    // hybrid 档 09-01 删除;它和 lazy 同属懒加载家族,历史值一律归入需加载,
    // 悄悄升成 full 会让旧配置的工具面字节数翻好几倍。
    match value.trim() {
        "full" => "full".to_string(),
        _ => "stub".to_string(),
    }
}

pub(in crate::config_tui) fn parse_bool_field(value: &str) -> Result<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "yes" | "y" | "1" | "on" | "启用" | "是" => Ok(true),
        "false" | "no" | "n" | "0" | "off" | "禁用" | "否" => Ok(false),
        value => {
            if is_zh() {
                bail!("无效的布尔值: {value}")
            } else {
                bail!("Invalid boolean value: {value}")
            }
        }
    }
}
