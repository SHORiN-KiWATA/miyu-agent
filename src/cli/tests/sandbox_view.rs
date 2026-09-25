//! `/sandbox` 查看与绑定回执的表格(09-23)。

use crate::cli::*;

fn state(sandbox: Option<&str>, default: bool, readonly: bool) -> ipc::SessionState {
    ipc::SessionState {
        context_tokens: 0,
        context_window: None,
        context_window_assumed: false,
        cumulative_tokens: 0,
        cumulative_prompt_tokens: 0,
        cumulative_cache_read_tokens: 0,
        session_id: "s".to_string(),
        session_name: String::new(),
        sandbox: sandbox.map(str::to_string),
        sandbox_default: default,
        sandbox_readonly: readonly,
        sandbox_writable: vec!["root".into(), "/tmp".into(), "~/.cargo".into()],
        sandbox_readable: vec!["everything (read-only)".into()],
        mode: "normal".to_string(),
        cache_breaks: 0,
    }
}

/// 表格给人看:摘要里给模型的英文固定词换成界面语言(一个槽两拨人用就是中英
/// 混杂的根,AGENTS §1.5.1),路径原样;默认根带标记;只读那一行照实报。
#[test]
fn sandbox_table_speaks_the_ui_language_and_keeps_paths() {
    let table = strip_terminal_control_sequences(&sandbox_state_table(&state(
        Some("/home/me/.miyu/home/me/workspace"),
        true,
        false,
    )));
    assert!(
        table.contains("/home/me/.miyu/home/me/workspace"),
        "{table}"
    );
    assert!(
        table.contains(t(" (default)", "（默认）").trim()),
        "{table}"
    );
    assert!(
        table.contains("~/.cargo") && table.contains("/tmp"),
        "{table}"
    );
    assert!(
        table.contains(t("the root above", "上面的根目录")),
        "{table}"
    );
    assert!(table.contains(t("everything", "全部")), "{table}");
    if crate::cli::is_zh() {
        assert!(
            !table.contains("everything") && !table.contains("root,"),
            "{table}"
        );
    }
    assert!(table.lines().count() >= 5, "是一张表而不是几行字: {table}");

    let readonly =
        strip_terminal_control_sequences(&sandbox_state_table(&state(None, false, true)));
    let on_row = readonly
        .lines()
        .find(|line| line.contains(t("Read-only", "只读")))
        .unwrap_or_default();
    assert!(on_row.contains(t("on", "开")), "{readonly}");

    let none = strip_terminal_control_sequences(&sandbox_state_table(&state(None, false, false)));
    assert_eq!(none.lines().count(), 1, "没沙盒就一行字: {none}");
}

/// 「只读」是金色(用户 09-23),跟空会话提示按键的那个金同源、按同一个色深降级。
#[test]
fn read_only_label_is_gold() {
    use miyu_base::terminal::palette::{Depth, Theme, GOLD};
    let config = AppConfig::default();
    let footer = ReplFooterStatus::from_config(&config, 0, TurnTokens::default());
    let line = repl_footer_left(PersonaLane::Active, true, &footer, 120);
    let gold = Theme::detect().fg_ansi(GOLD);
    assert!(line.contains(&format!("\x1b[1m{gold}")), "{line:?}");
    let (r, g, b) = GOLD;
    let truecolor = Theme {
        depth: Depth::True,
        ascii: false,
    };
    assert_eq!(truecolor.fg_ansi(GOLD), format!("\x1b[38;2;{r};{g};{b}m"));
}
