//! 面板里抬头底下露的那几行：命令预览与思考窗。
//!
//! 前台子代理面板（`subagent.rs`，攒事件）与后台任务面板（`cli::…::overlay`，读日志
//! 或标记流）原来各拼各的：一边露的是截成一行 80 字的「主题」、点开也还是那一行，
//! 一边什么都不露；正在想的时候只有一截单行窥视，主线那扇「最近几行」的窗一直没
//! 进面板（用户 09-24：「子代理浮层的命令预览不对，只有一行，而且展开后看不到具体
//! 内容；并且思考的预览也没有」）。规矩照主线那一份，两块面板都从这儿拿。

use super::*;

/// 命令那一步抬头底下露的命令：**全文**按面板正文宽度折行，取前 `max_lines` 行，
/// 暗色（它是附注不是正文）。露几行是用户的「命令显示行数」
/// （`display.command_output_lines`），和主线同一个设置。
pub fn panel_command_tail(command: &str, max_lines: usize) -> Vec<String> {
    let width = panel_detail_width();
    command
        .lines()
        .flat_map(|line| crate::render::wrap_display_text(line, width))
        .take(max_lines)
        .map(|line| format!("\x1b[2m{line}\x1b[0m"))
        .collect()
}

/// 「思考中」抬头底下那扇窗：思考正文按面板宽度折行，露**末尾** `max_lines` 行
/// ——想到哪儿了比想过什么更有用。颜色和主线那扇窗（`thought_window_rows`）一样。
/// 露几行是 `display.thinking_scroll_lines`，0 = 不开窗。
///
/// 从最后几行往回折，够窗口那么高就停——只折用得上的那几行（面板每次刷新都会调它，
/// 整段重折的话子代理想得越久主界面越卡，同主线那扇窗，见 `ThoughtRows`）。
pub fn panel_thought_window(text: &str, max_lines: usize) -> Vec<String> {
    let width = panel_detail_width();
    let mut rows: Vec<String> = Vec::new();
    for line in text.lines().rev() {
        if rows.len() >= max_lines {
            break;
        }
        let mut wrapped: Vec<String> = crate::render::wrap_display_text(line, width)
            .into_iter()
            .filter(|row| !row.trim().is_empty())
            .collect();
        wrapped.append(&mut rows);
        rows = wrapped;
    }
    let keep = max_lines.min(rows.len());
    rows[rows.len() - keep..]
        .iter()
        .map(|row| format!("{THOUGHT_BODY_STYLE}{row}\x1b[0m"))
        .collect()
}
