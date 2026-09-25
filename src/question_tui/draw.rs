//! 提问面板的绘制。
//!
//! 面板高度有上限（`MAX_PANEL_LINES`）：问题可能很长，占满整屏会把上下文全推
//! 走，用户就看不到自己在回答什么了。超出部分靠 `wrap_display_text` 折行后
//! 滚动。

use crate::question_tui::*;

pub(in crate::question_tui) const MAX_PANEL_LINES: u16 = 16;

pub const BAR: &str = "\x1b[1m\x1b[35m┃\x1b[0m";

pub(in crate::question_tui) const ANSWERED_BAR: &str = "\x1b[2m\x1b[90m┃\x1b[0m";

/// 面板这一刻的内容：抬头（问题正文）、选项、底下的说明，外加光标该落哪儿。只读状态，
/// 不碰终端。
pub(in crate::question_tui) struct Sections {
    top: Vec<String>,
    body: Vec<String>,
    footer: Vec<String>,
    focused_body_index: Option<usize>,
    edit_body_index: Option<usize>,
    /// 光标所在列（从行首竖条算起）。
    edit_cursor_x: usize,
}

impl Sections {
    pub(in crate::question_tui) fn line_count(&self) -> usize {
        self.top.len() + self.body.len() + self.footer.len()
    }
}

pub(in crate::question_tui) fn sections(
    request: &QuestionRequest,
    state: &QuestionState,
    content_width: usize,
) -> Sections {
    let mut top_lines = Vec::new();
    let mut body_lines = Vec::new();
    let mut footer_lines = Vec::new();
    let mut edit_body_index = None;
    let mut edit_cursor_offset = 0usize;
    let mut edit_cursor_column = 0usize;
    let mut focused_body_index = None;

    if request.needs_review() {
        top_lines.push(tab_line(request, state));
        top_lines.push(String::new());
    }
    if state.on_confirm(request) {
        for (question, selected) in request.questions.iter().zip(&state.answers) {
            if selected.is_empty() {
                body_lines.push(format!(
                    "{}: \x1b[31m{}\x1b[0m",
                    question.header,
                    t("unanswered", "未回答")
                ));
                continue;
            }
            let prefix = format!("{}: ", question.header);
            let plain = format!("{prefix}{}", display_inline(&selected.join("、")));
            for (index, line) in wrap_display_text(&plain, content_width)
                .into_iter()
                .enumerate()
            {
                let rendered = match line.strip_prefix(prefix.as_str()) {
                    Some(rest) if index == 0 => format!("{prefix}\x1b[2m{rest}\x1b[0m"),
                    _ => format!("\x1b[2m{line}\x1b[0m"),
                };
                body_lines.push(rendered);
            }
        }
        footer_lines.push(String::new());
        footer_lines.push(format!(
            "\x1b[2m{}\x1b[0m",
            t(
                "Enter submit · Left/Right switch · Esc twice cancel",
                "Enter 提交 · ←/→ 换题 · Esc 两次取消",
            )
        ));
    } else {
        let question = &request.questions[state.tab];
        top_lines.extend(
            wrap_display_text(question.question.trim(), content_width)
                .into_iter()
                .map(|line| format!("\x1b[1m{line}\x1b[0m")),
        );
        top_lines.push(String::new());
        for (index, option) in question.options.iter().enumerate() {
            let picked = state.answers[state.tab].contains(&option.label);
            if state.selected[state.tab] == index {
                focused_body_index = Some(body_lines.len());
            }
            body_lines.extend(option_lines(
                &option.label,
                &option.description,
                state.selected[state.tab] == index,
                picked,
                question.multiple,
                content_width,
            ));
        }
        if question.custom {
            let index = question.options.len();
            let custom = &state.custom_answers[state.tab];
            let picked = !custom.is_empty() && state.answers[state.tab].contains(custom);
            if state.selected[state.tab] == index {
                focused_body_index = Some(body_lines.len());
            }
            if state.editing && state.selected[state.tab] == index {
                edit_body_index = Some(body_lines.len());
                let editor_prefix_width = if question.multiple { 6 } else { 2 };
                let (editor, cursor_offset) = editor_view(
                    &state.edit_buffer,
                    state.edit_cursor,
                    content_width.saturating_sub(editor_prefix_width),
                );
                edit_cursor_offset = cursor_offset;
                edit_cursor_column = UnicodeWidthStr::width(if question.multiple {
                    "┃ › [ ] "
                } else {
                    "┃ › "
                });
                body_lines.push(editor_option_line(question.multiple, picked, &editor));
            } else {
                let label = if custom.is_empty() {
                    t("Type your own answer", "输入其他答案").to_string()
                } else {
                    format!("{}: {}", t("Custom", "自定义"), display_inline(custom))
                };
                body_lines.extend(option_lines(
                    &label,
                    "",
                    state.selected[state.tab] == index,
                    picked,
                    question.multiple,
                    content_width,
                ));
            }
        }
        footer_lines.push(String::new());
        if state.editing {
            footer_lines.push(format!(
                "\x1b[2m{}\x1b[0m",
                t(
                    "Enter save · Shift+Enter newline · Ctrl+J newline · Esc stop editing",
                    "Enter 保存 · Shift+Enter 换行 · Ctrl+J 换行 · Esc 退出编辑"
                )
            ));
        } else {
            let help = if question.multiple {
                t(
                    "Up/Down select · Tab/Space toggle · Enter select/edit · Left/Right switch",
                    "↑/↓ 选择 · Tab/Space 切换 · Enter 选择/编辑 · ←/→ 换题",
                )
            } else {
                t(
                    "Up/Down select · Enter submit · Left/Right switch",
                    "↑/↓ 选择 · Enter 提交 · ←/→ 换题",
                )
            };
            footer_lines.push(format!(
                "\x1b[2m{help} · Esc ×2 {}\x1b[0m",
                t("cancel", "取消")
            ));
        }
    }

    if state.cancel_armed_until.is_some() {
        footer_lines.push(format!(
            "\x1b[1m\x1b[33m{}\x1b[0m",
            t(
                "Press Esc again to cancel this response",
                "再次按 Esc 取消本轮回复"
            )
        ));
    }

    Sections {
        top: top_lines,
        body: body_lines,
        footer: footer_lines,
        focused_body_index,
        edit_body_index,
        edit_cursor_x: edit_cursor_column.saturating_add(edit_cursor_offset),
    }
}

/// 最多 `max_lines` 行的样子：选项多了按选中项滚，顶上的空行不画。
pub(in crate::question_tui) fn view(
    request: &QuestionRequest,
    state: &mut QuestionState,
    content_width: usize,
    max_lines: usize,
) -> QuestionView {
    let parts = sections(request, state, content_width);
    let layout = panel_layout(
        parts.top.len(),
        parts.body.len(),
        parts.footer.len(),
        max_lines,
        parts.focused_body_index,
        state.scroll_starts[state.tab],
    );
    state.scroll_starts[state.tab] = layout.body_start;
    let visible_lines: Vec<&String> = parts
        .top
        .iter()
        .skip(layout.top_start)
        .chain(
            parts
                .body
                .iter()
                .skip(layout.body_start)
                .take(layout.body_capacity),
        )
        .chain(parts.footer.iter().skip(layout.footer_start))
        .collect();
    // 顶上那几行空的不画：布局按"最多能放多少"切，切出来常常开头就是空行，
    // 而面板是贴着底往上长的——空行于是变成框顶上多出来的一条空带。
    let lead = visible_lines
        .iter()
        .take_while(|line| line.trim().is_empty())
        .count();
    let cursor = parts
        .edit_body_index
        .filter(|index| {
            state.editing
                && *index >= layout.body_start
                && *index < layout.body_start.saturating_add(layout.body_capacity)
        })
        .map(|index| {
            (
                (layout.top_budget + index - layout.body_start).saturating_sub(lead),
                parts.edit_cursor_x,
            )
        });
    QuestionView {
        lines: visible_lines
            .into_iter()
            .skip(lead)
            .map(|line| truncate_width(line, content_width))
            .collect(),
        cursor,
    }
}

pub(in crate::question_tui) fn draw(
    session: &mut QuestionSession,
    panel: &mut QuestionPanel,
    scroll: &mut Option<&mut dyn FnMut(isize, u16)>,
) -> Result<()> {
    let (cols, rows) = terminal::size().unwrap_or((80, 24));
    let content_width = (cols as usize).saturating_sub(3).max(1);
    let view = panel.view(content_width, session.panel_lines as usize);
    // 先清旧面板，再让正文按实际高度重排，最后画新面板。
    // 清屏范围不能用高度上限，否则短问题仍会抹掉上方正文。
    session.clear()?;
    let height = u16::try_from(view.lines.len()).unwrap_or(MAX_PANEL_LINES);
    let base = if crate::cli::in_fullscreen() {
        let geometry = (cols, rows, height);
        if session.geometry != Some(geometry) {
            if let Some(scroll) = scroll.as_deref_mut() {
                scroll(0, height.saturating_add(1));
            }
            session.geometry = Some(geometry);
        }
        rows.saturating_sub(height.saturating_add(1))
    } else {
        session.anchor_y
    };
    session.anchor_y = base;
    session.painted_rows = height;
    for (row, line) in view.lines.iter().enumerate() {
        queue!(
            session.stdout,
            MoveTo(0, base.saturating_add(row as u16)),
            Clear(ClearType::CurrentLine),
            crossterm::style::Print(BAR),
            crossterm::style::Print(" "),
            crossterm::style::Print(line)
        )?;
    }
    if crate::cli::in_fullscreen() {
        queue!(
            session.stdout,
            MoveTo(0, rows.saturating_sub(1)),
            Clear(ClearType::CurrentLine)
        )?;
    }
    match view.cursor {
        Some((row, column)) => queue!(
            session.stdout,
            MoveTo(
                column.min(cols.saturating_sub(1) as usize) as u16,
                base.saturating_add(row as u16)
            ),
            Show
        )?,
        None if panel.editing() => queue!(session.stdout, Show)?,
        None => queue!(session.stdout, Hide)?,
    }
    session.stdout.flush()?;
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
pub(in crate::question_tui) struct PanelLayout {
    pub(in crate::question_tui) top_start: usize,
    pub(in crate::question_tui) top_budget: usize,
    pub(in crate::question_tui) body_start: usize,
    pub(in crate::question_tui) body_capacity: usize,
    pub(in crate::question_tui) footer_start: usize,
}

pub(in crate::question_tui) fn panel_layout(
    top_len: usize,
    body_len: usize,
    footer_len: usize,
    max_lines: usize,
    focused_body_index: Option<usize>,
    current_body_start: usize,
) -> PanelLayout {
    let footer_budget = footer_len.min(max_lines);
    // 长正文折行后 top 可能占满面板；给选项区保底几行，超出的正文由 top_start 保留尾部
    let reserved_body = body_len
        .min(3)
        .min(max_lines.saturating_sub(footer_budget) / 2);
    let top_budget = top_len.min(
        max_lines
            .saturating_sub(footer_budget)
            .saturating_sub(reserved_body),
    );
    let body_capacity = max_lines.saturating_sub(top_budget + footer_budget);
    let max_body_start = body_len.saturating_sub(body_capacity);
    let mut body_start = current_body_start.min(max_body_start);
    if body_capacity == 0 {
        body_start = 0;
    } else if let Some(index) = focused_body_index {
        if index < body_start {
            body_start = index;
        } else if index >= body_start.saturating_add(body_capacity) {
            body_start = index
                .saturating_add(1)
                .saturating_sub(body_capacity)
                .min(max_body_start);
        }
    }
    PanelLayout {
        top_start: top_len.saturating_sub(top_budget),
        top_budget,
        body_start,
        body_capacity,
        footer_start: footer_len.saturating_sub(footer_budget),
    }
}

pub(in crate::question_tui) fn tab_line(
    request: &QuestionRequest,
    state: &QuestionState,
) -> String {
    let mut parts = Vec::new();
    for (index, question) in request.questions.iter().enumerate() {
        let answered = !state.answers[index].is_empty();
        let label = if answered {
            format!("{} ✓", question.header)
        } else {
            question.header.clone()
        };
        if state.tab == index {
            parts.push(format!("\x1b[7m {label} \x1b[0m"));
        } else {
            parts.push(format!("\x1b[2m{label}\x1b[0m"));
        }
    }
    if request.needs_review() {
        if state.tab == request.questions.len() {
            parts.push(format!("\x1b[7m {} \x1b[0m", t("Review", "确认")));
        } else {
            parts.push(format!("\x1b[2m{}\x1b[0m", t("Review", "确认")));
        }
    }
    parts.join("  ")
}

pub(in crate::question_tui) fn option_lines(
    label: &str,
    description: &str,
    active: bool,
    picked: bool,
    multiple: bool,
    content_width: usize,
) -> Vec<String> {
    let marker = if multiple {
        if picked {
            "\x1b[35m[✓]\x1b[0m "
        } else {
            "\x1b[2m[ ]\x1b[0m "
        }
    } else {
        ""
    };
    let pointer = if active { "\x1b[35m›\x1b[0m " } else { "  " };
    let label_prefix_width = if multiple { 6 } else { 2 };
    let label_indent = " ".repeat(label_prefix_width);
    let label_width = content_width.saturating_sub(label_prefix_width).max(1);
    let mut lines = Vec::new();
    for (index, part) in wrap_display_text(label, label_width)
        .into_iter()
        .enumerate()
    {
        let part = if active || picked {
            format!("\x1b[35m{part}\x1b[0m")
        } else {
            part
        };
        if index == 0 {
            lines.push(format!("{pointer}{marker}{part}"));
        } else {
            lines.push(format!("{label_indent}{part}"));
        }
    }
    if lines.is_empty() {
        lines.push(format!("{pointer}{marker}"));
    }
    let description = description.trim();
    if !description.is_empty() {
        let indent = if multiple { "      " } else { "  " };
        let width = content_width
            .saturating_sub(UnicodeWidthStr::width(indent))
            .max(1);
        lines.extend(
            wrap_display_text(description, width)
                .into_iter()
                .map(|line| format!("{indent}\x1b[2m{line}\x1b[0m")),
        );
    }
    lines
}

pub(in crate::question_tui) fn editor_option_line(
    multiple: bool,
    picked: bool,
    editor: &str,
) -> String {
    let marker = if multiple {
        if picked {
            "\x1b[35m[✓]\x1b[0m "
        } else {
            "\x1b[2m[ ]\x1b[0m "
        }
    } else {
        ""
    };
    let value = if editor.is_empty() {
        format!(
            "\x1b[2m{}\x1b[0m",
            t("Type your own answer", "输入其他答案")
        )
    } else {
        editor.to_string()
    };
    format!("\x1b[35m›\x1b[0m {marker}{value}")
}

pub fn wrap_display_text(value: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut current_width = 0usize;
    for ch in value.chars() {
        let char_width = ch.width().unwrap_or(0);
        if current_width > 0 && current_width.saturating_add(char_width) > width {
            lines.push(std::mem::take(&mut current));
            current_width = 0;
        }
        current.push(ch);
        current_width = current_width.saturating_add(char_width);
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

/// 给面板腾出底部 `lines` 行。
///
/// inline 下靠打换行把画面顶上去——顶出去的进 scrollback，回翻还找得到。
/// **全屏下不能这么干**：备用屏没有 scrollback，顶出去就是没了，用户看到的是
/// 「一提问，正文全被清空」。全屏下只把光标放到底部；实际高度算出后，
/// 调用方从正文缓冲重排上方视口，面板退场再恢复输入区布局。
pub(in crate::question_tui) fn reserve_space(lines: u16) -> Result<()> {
    if crate::cli::in_fullscreen() {
        let rows = crossterm::terminal::size()
            .map(|(_, rows)| rows)
            .unwrap_or(24);
        crossterm::execute!(
            io::stdout(),
            crossterm::cursor::MoveTo(0, rows.saturating_sub(1))
        )?;
        return Ok(());
    }
    for _ in 1..lines {
        println!();
    }
    io::stdout().flush()?;
    Ok(())
}

pub(in crate::question_tui) fn display_inline(value: &str) -> String {
    value
        .chars()
        .filter_map(|ch| match ch {
            '\n' | '\r' => Some('↵'),
            '\t' => Some(' '),
            ch if ch.is_control() => None,
            ch => Some(ch),
        })
        .collect()
}

pub(in crate::question_tui) fn editor_view(
    value: &str,
    cursor: usize,
    width: usize,
) -> (String, usize) {
    if width == 0 {
        return (String::new(), 0);
    }
    let display = display_inline(value);
    let before = display_inline(&value.chars().take(cursor).collect::<String>());
    let cursor_width = UnicodeWidthStr::width(before.as_str());
    if UnicodeWidthStr::width(display.as_str()) <= width {
        return (display, cursor_width.min(width));
    }
    if cursor_width < width {
        return (truncate_plain_width(&display, width), cursor_width);
    }

    let tail_budget = width.saturating_sub(1);
    let mut tail = String::new();
    let mut tail_width = 0usize;
    for ch in before.chars().rev() {
        let ch_width = ch.width().unwrap_or(0);
        if tail_width + ch_width > tail_budget {
            break;
        }
        tail.insert(0, ch);
        tail_width += ch_width;
    }
    let after = display
        .chars()
        .skip(before.chars().count())
        .collect::<String>();
    let mut view = format!("…{tail}");
    let remaining = width.saturating_sub(1 + tail_width);
    view.push_str(&truncate_plain_width(&after, remaining));
    (view, (1 + tail_width).min(width))
}

pub(in crate::question_tui) fn truncate_plain_width(value: &str, max_width: usize) -> String {
    let mut output = String::new();
    let mut width = 0usize;
    for ch in value.chars() {
        let ch_width = ch.width().unwrap_or(0);
        if width + ch_width > max_width {
            break;
        }
        output.push(ch);
        width += ch_width;
    }
    output
}

pub(in crate::question_tui) fn truncate_width(value: &str, max_width: usize) -> String {
    if UnicodeWidthStr::width(strip_ansi(value).as_str()) <= max_width {
        return value.to_string();
    }
    if max_width <= 3 {
        return ".".repeat(max_width);
    }
    let budget = max_width.saturating_sub(3);
    let mut output = String::new();
    let mut width = 0usize;
    let mut in_escape = false;
    for ch in value.chars() {
        if ch == '\x1b' {
            in_escape = true;
            output.push(ch);
            continue;
        }
        if in_escape {
            output.push(ch);
            if ch == 'm' {
                in_escape = false;
            }
            continue;
        }
        let char_width = ch.width().unwrap_or(0);
        if width + char_width > budget {
            break;
        }
        output.push(ch);
        width += char_width;
    }
    output.push_str("...\x1b[0m");
    output
}

pub(in crate::question_tui) fn strip_ansi(value: &str) -> String {
    let mut output = String::new();
    let mut in_escape = false;
    for ch in value.chars() {
        if ch == '\x1b' {
            in_escape = true;
        } else if in_escape {
            if ch == 'm' {
                in_escape = false;
            }
        } else {
            output.push(ch);
        }
    }
    output
}
