//! `/sandbox` 查看与绑定回执的表格(用户 09-23:单单几行字太素)。
//!
//! 可写/可读那两栏的原料是给模型看的英文摘要(`SandboxPolicy::*_summary`,进
//! `<sandbox>` 尾巴);给人看的这一份在这里换成界面语言——一个槽两拨人用就是
//! 中英混杂的根(AGENTS §1.5.1)。路径原样。

use crate::cli::*;

/// 摘要里的固定词换成给人看的说法;路径与没见过的词原样。
fn human_item(item: &str) -> String {
    match item {
        "root" => t("the root above", "上面的根目录").to_string(),
        "system dirs" => t("system dirs", "系统目录").to_string(),
        "nothing" => t("nowhere", "无").to_string(),
        "everything"
        | "everything (read-only)"
        | "everything (this backend confines writes only)" => t("everything", "全部").to_string(),
        other => other.to_string(),
    }
}

fn human_list(items: &[String]) -> String {
    items
        .iter()
        .map(|item| human_item(item))
        .collect::<Vec<_>>()
        .join(", ")
}

/// 表格单元里的 `|` 会被当成分隔符。
fn cell(text: &str) -> String {
    text.replace('|', "\\|")
}

/// 沙盒现状的表格;没绑、也没开只读时是一行字。
pub(in crate::cli) fn sandbox_state_table(state: &ipc::SessionState) -> String {
    if state.sandbox.is_none() && !state.sandbox_readonly {
        return format!(
            "\x1b[2m{}\x1b[0m\n",
            t(
                "No sandbox. Reads and writes are unrestricted.",
                "没有沙盒，读写不受限。"
            )
        );
    }
    let mut rows: Vec<(String, String)> = Vec::new();
    if let Some(root) = state.sandbox.as_deref() {
        let root = if state.sandbox_default {
            format!("{root}{}", t(" (default)", "（默认）"))
        } else {
            root.to_string()
        };
        rows.push((t("Root", "根目录").to_string(), root));
    }
    rows.push((
        t("Read-only", "只读").to_string(),
        if state.sandbox_readonly {
            t("on", "开")
        } else {
            t("off", "关")
        }
        .to_string(),
    ));
    rows.push((
        t("Writable", "可写").to_string(),
        human_list(&state.sandbox_writable),
    ));
    rows.push((
        t("Readable", "可读").to_string(),
        human_list(&state.sandbox_readable),
    ));
    let mut lines = vec![
        format!("| {} | {} |", t("Sandbox", "沙盒"), t("Setting", "设置")),
        "| --- | --- |".to_string(),
    ];
    lines.extend(
        rows.iter()
            .map(|(key, value)| format!("| {} | {} |", cell(key), cell(value))),
    );
    let mut table = render::render_table(&lines);
    if !table.ends_with('\n') {
        table.push('\n');
    }
    table
}
