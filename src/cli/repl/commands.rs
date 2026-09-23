//! REPL 的斜杠命令表。
//!
//! `REPL_COMMAND_TABLE` 是单一事实来源：补全、帮助、解析全从它派生，加命令只
//! 用改这一处。

use crate::cli::*;

/// 帮助全文。**要能拿到字符串**：全屏下它得走缓冲进正文（直接 `println!`
/// 的话字节不在缓冲里，下一帧重画就被抹掉，回翻也找不到）。
pub(in crate::cli) fn repl_help_text() -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(out, "{}", t("commands:", "命令:"));
    let width = REPL_COMMAND_TABLE
        .iter()
        .map(|spec| {
            spec.name.len()
                + if spec.arg_hint.is_empty() {
                    0
                } else {
                    spec.arg_hint.len() + 1
                }
        })
        .max()
        .unwrap_or(0);
    for spec in REPL_COMMAND_TABLE {
        let invocation = if spec.arg_hint.is_empty() {
            spec.name.to_string()
        } else {
            format!("{} {}", spec.name, spec.arg_hint)
        };
        let alias_note = if spec.aliases.is_empty() {
            String::new()
        } else {
            let aliases = spec.aliases.join(" ");
            if is_zh() {
                format!("（别名 {aliases}）")
            } else {
                format!(" (alias: {aliases})")
            }
        };
        let _ = writeln!(
            out,
            "  {invocation:<width$}  {}{alias_note}",
            t(spec.help_en, spec.help_zh)
        );
    }
    let _ = writeln!(out, "{}", t("keys:", "快捷键:"));
    let _ = writeln!(
        out,
        "  Tab         {}",
        t(
            "complete slash commands; switch normal/dev in an empty session, toggle read-only otherwise",
            "补全斜杠命令；空会话时切换 普通/开发，否则切换只读"
        )
    );
    let _ = writeln!(
        out,
        "  Shift+Tab   {}",
        t("toggle full read-only mode", "切换全盘只读模式")
    );
    let _ = writeln!(out, "  Enter       {}", t("send message", "发送消息"));
    let _ = writeln!(out, "  Shift+Enter {}", t("insert newline", "插入换行"));
    let _ = writeln!(
        out,
        "  Ctrl+J      {}",
        t(
            "insert newline, same as Shift+Enter",
            "插入换行，与 Shift+Enter 相同"
        )
    );
    let _ = writeln!(
        out,
        "  Ctrl+V      {}",
        t(
            "paste image or text from clipboard",
            "从剪贴板粘贴图片或文本"
        )
    );
    let _ = writeln!(out, "  Ctrl+L      {}", t("clear screen", "清屏"));
    let _ = writeln!(
        out,
        "  Up/Down     {}",
        t("browse input history", "切换输入历史")
    );
    let _ = writeln!(
        out,
        "  Esc Esc     {}",
        t("interrupt running reply", "中断当前回复")
    );
    let _ = writeln!(
        out,
        "  Ctrl+C      {}",
        t(
            "clear the draft, else interrupt the reply, else stop background tasks, else exit",
            "先清空输入；输入为空则中断回复；再无回复则停止后台任务；都没有则退出"
        )
    );
    let _ = writeln!(out, "  Ctrl+D      {}", t("exit", "退出"));
    out
}

pub(in crate::cli) fn print_repl_help() {
    print!("{}", repl_help_text());
}

/// 斜杠命令候选面板的内容：每条一行「命令 + 它是干什么的」。
///
/// inline 那边只能在 footer 位置塞一行挤在一起的命令名——全屏有地方，就把
/// 说明也给上，省得记不住哪个是哪个。最多四条，再多就该 Tab 补全了。
pub(in crate::cli) fn command_hint_lines(input: &str, cols: usize) -> Vec<String> {
    let input = input.trim_start();
    let suggestions = miyu_core::slash_commands::repl_command_suggestions(input);
    if suggestions.is_empty() {
        return Vec::new();
    }
    // 打全了不撤面板：以前「只剩一条且已经打全」就把面板收掉，可 `/sessio` 有、
    // `/session` 反而没了，像打错了一样（用户实测）。打全的那条把参数提示也带上，
    // 顺手告诉你后面能接什么；在打参数的时候它也留着。
    let typed = input.split_whitespace().next().unwrap_or(input);
    let entries = suggestions
        .iter()
        .take(4)
        .map(|name| {
            let spec = repl_command_spec_for_name(name);
            let label = match spec {
                Some(spec) if name.eq_ignore_ascii_case(typed) && !spec.arg_hint.is_empty() => {
                    format!("{name} {}", spec.arg_hint)
                }
                _ => name.to_string(),
            };
            let help = spec.map(|spec| spec.help()).unwrap_or("");
            // 打的是别名：说明它等于哪条正名，免得两个名字像两条命令。
            let help = match spec {
                Some(spec) if !spec.name.eq_ignore_ascii_case(name) => {
                    format!("= {} · {help}", spec.name)
                }
                _ => help.to_string(),
            };
            (label, help)
        })
        .collect::<Vec<_>>();
    let width = cols.saturating_sub(10).max(20);
    let name_col = entries
        .iter()
        .map(|(label, _)| label.len())
        .max()
        .unwrap_or(0)
        .min(20);
    entries
        .iter()
        .map(|(label, help)| {
            let pad = " ".repeat(name_col.saturating_sub(label.len()));
            truncate_visible_width(&format!("{label}{pad}  \x1b[2m{help}\x1b[0m"), width)
        })
        .collect()
}

pub(in crate::cli) fn repl_command_suggestions_line(
    suggestions: &[&str],
    max_width: usize,
) -> String {
    let line = if suggestions.len() == 1 {
        suggestions[0].to_string()
    } else {
        suggestions.join("  ")
    };
    truncate_visible_width(&line, max_width)
}
