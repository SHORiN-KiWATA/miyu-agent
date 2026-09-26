//! 内置 CodeBuddy 特殊供应商的专用编辑表单。
//!
//! 与 `claude_code_form` 同构:CodeBuddy 是 Claude Code 的分叉,字段一一对应
//! （二进制 / 双四档工具作用域 / 权限模式 / 空闲看门狗）。单独成文件的理由也
//! 一样:它和通用供应商表单没有共享字段——没有 HTTP 概念,落盘在
//! `plugins.codebuddy`。

use crate::config_tui::*;

const TOOL_SCOPES: &[&str] = &["off", "dev", "normal", "all"];

/// CodeBuddy 特殊供应商的编辑表单。不是 HTTP 端点,所以没有
/// base_url/协议/API Key/超时/额外请求体。
pub(in crate::config_tui) fn edit_codebuddy_provider_form(
    ui: &mut Ui,
    provider: ProviderConfig,
    plugin: &mut miyu_base::config::CodeBuddyPluginConfig,
) -> Result<Option<ProviderConfig>> {
    let mut fields = vec![
        Field::new(
            t("Enabled (CodeBuddy relay)", "启用(中转 CodeBuddy)"),
            provider.enabled.to_string(),
        )
        .choices(&["true", "false"]),
        Field::new(t("Display name", "显示名称"), provider.display_name.clone()),
        Field::new(
            t(
                "codebuddy binary (empty = PATH)",
                "codebuddy 可执行文件(空=PATH)",
            ),
            plugin.binary.clone(),
        ),
        Field::new(
            t("CodeBuddy native tools scope", "CodeBuddy 原生工具作用域"),
            plugin.native_tools.clone(),
        )
        .choices(TOOL_SCOPES),
        Field::new(
            t(
                "Miyu tools via MCP bridge scope",
                "Miyu 工具挂给 codebuddy 的作用域",
            ),
            plugin.miyu_tools.clone(),
        )
        .choices(TOOL_SCOPES),
        Field::new(
            t("Permission mode for native tools", "原生工具权限模式"),
            plugin.permission_mode.clone(),
        )
        // CodeBuddy 的 `--permission-mode` 只认这四档(09-20 核过 `-h`),
        // 比 claude 少一个 `dontAsk`。
        .choices(&["bypassPermissions", "acceptEdits", "default", "plan"]),
        Field::new(
            t("Stream idle watchdog (seconds)", "流空闲看门狗(秒)"),
            plugin.idle_timeout_seconds.to_string(),
        ),
    ];
    loop {
        if !run_edit_form(ui, t(" EDIT CODEBUDDY ", " 编辑 CodeBuddy "), &mut fields)? {
            return Ok(None);
        }
        let enabled = match parse_bool_field(&fields[0].value) {
            Ok(value) => value,
            Err(error) => {
                message(ui, &format!("{error:#}"))?;
                continue;
            }
        };
        plugin.binary = fields[2].value.trim().to_string();
        plugin.native_tools = normalize_tool_scope(&fields[3].value);
        plugin.miyu_tools = normalize_tool_scope(&fields[4].value);
        plugin.permission_mode = fields[5].value.trim().to_string();
        plugin.idle_timeout_seconds = fields[6].value.trim().parse().unwrap_or(300);
        let mut updated = provider.clone();
        updated.enabled = enabled;
        let display_name = fields[1].value.trim();
        updated.display_name = if display_name.is_empty() {
            "CodeBuddy".to_string()
        } else {
            display_name.to_string()
        };
        return Ok(Some(updated));
    }
}

/// 手输的作用域值归一到四档;认不出的按 off 兜底(与运行时判定一致)。
fn normalize_tool_scope(value: &str) -> String {
    let value = value.trim().to_ascii_lowercase();
    if TOOL_SCOPES.contains(&value.as_str()) {
        value
    } else {
        "off".to_string()
    }
}
