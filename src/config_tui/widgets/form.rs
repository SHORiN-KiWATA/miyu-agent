//! 表单：字段、编辑、按钮。
//!
//! `run_form_from` 是主循环，`Field` 是它的单元。字段的显示值与内部值分开
//! （`field_display_value`）——密钥要显示成掩码，布尔要显示成「开/关」。

use crate::config_tui::*;
use miyu_base::terminal::chrome::{ln, nil, View};
use miyu_base::terminal::palette::{BLUE, DIM};
use ratatui::text::{Line, Span};

pub(in crate::config_tui) fn edit_u16_value(
    ui: &mut Ui,
    label: &'static str,
    current: u16,
) -> Result<Option<u16>> {
    let mut fields = vec![Field::new(label, current.to_string())];
    if !run_edit_form_editing(ui, t(" EDIT VALUE ", " 编辑数值 "), &mut fields)? {
        return Ok(None);
    }
    match fields[0].value.trim().parse() {
        Ok(value) => Ok(Some(value)),
        Err(_) => {
            message(ui, t("Invalid number.", "数值无效。"))?;
            Ok(None)
        }
    }
}

pub(in crate::config_tui) fn edit_inline_value(
    ui: &mut Ui,
    title: &str,
    current: &str,
    sensitive: bool,
) -> Result<Option<String>> {
    let mut value = current.to_string();
    let mut cursor = value.chars().count();
    let mut fcitx = FcitxState::new();
    fcitx.enter_editing();
    loop {
        draw_inline_editor(ui, title, &value, cursor, sensitive)?;
        match read_key(ui)? {
            KeyCode::Esc => {
                fcitx.leave_editing();
                return Ok(None);
            }
            KeyCode::Enter => {
                fcitx.leave_editing();
                return Ok(Some(value));
            }
            KeyCode::Left => cursor = cursor.saturating_sub(1),
            KeyCode::Right => cursor = (cursor + 1).min(value.chars().count()),
            KeyCode::Home => cursor = 0,
            KeyCode::End => cursor = value.chars().count(),
            KeyCode::Backspace if cursor > 0 => remove_char_before_cursor(&mut value, &mut cursor),
            KeyCode::Delete => remove_char_at_cursor(&mut value, cursor),
            KeyCode::Char(ch) => insert_char_at_cursor(&mut value, &mut cursor, ch),
            _ => {}
        }
    }
}

/// 单行输入框。整屏只有这一件事，所以正文区就放它：一条输入行 + 底下一根
/// 线（线的颜色区分「正在打字」）。
pub(in crate::config_tui) fn draw_inline_editor(
    ui: &mut Ui,
    title: &str,
    value: &str,
    cursor: usize,
    sensitive: bool,
) -> Result<()> {
    let cx = ui.cx();
    let (lines, caret_row, caret_col) = cx.field_at(value, "", true, true, sensitive, cursor);
    let mut body = vec![nil()];
    let base = body.len();
    body.extend(lines);
    ui.show(
        title,
        View {
            cursor_row: base + caret_row,
            caret: Some((base + caret_row, caret_col)),
            body,
            keys: vec![
                (
                    t("←→ Home/End", "←→ Home/End").to_string(),
                    t("move caret", "移动光标").to_string(),
                ),
                ("⏎".to_string(), t("save", "保存").to_string()),
                ("Esc".to_string(), t("cancel", "取消").to_string()),
            ],
            ..View::default()
        },
    )
}

pub(in crate::config_tui) fn run_form(
    ui: &mut Ui,
    title: &str,
    fields: &mut [Field],
) -> Result<bool> {
    run_form_from(ui, title, fields, false)
}

/// 一张表单是怎么结束的。
///
/// `Link` = 回车落在了跳转行（[`Field::link`]）上。表单自己不认识那一行指向哪
/// 个菜单，把下标交还调用方：调用方打开菜单、刷新那一行的显示值，再用
/// [`run_edit_form_linked`] 从同一行接着跑同一份 `fields`——还没收下的改动不会丢。
pub(in crate::config_tui) enum FormOutcome {
    Saved,
    Cancelled,
    Link(usize),
}

/// `start_editing` puts the caret in the first field straight away, for forms
/// reached from a menu row that already showed the value: the row said what it
/// was, Enter said "change it", so a second Enter to begin typing is a keypress
/// that asks a question nobody had.
pub(in crate::config_tui) fn run_form_editing(
    ui: &mut Ui,
    title: &str,
    fields: &mut [Field],
) -> Result<bool> {
    run_form_from(ui, title, fields, true)
}

pub(in crate::config_tui) fn run_form_from(
    ui: &mut Ui,
    title: &str,
    fields: &mut [Field],
    start_editing: bool,
) -> Result<bool> {
    Ok(matches!(
        run_form_outcome(ui, title, fields, start_editing, 0, true)?,
        FormOutcome::Saved
    ))
}

/// 编辑已有项的表单（用户 09-26）：不挂「保存 / 返回」两行，Esc 退出，写盘归主菜单的
/// 「保存并退出」。返回改没改过：一个字没动就当没进来过，调用方照旧走原来「返回」那条路
/// ——不收、也不触发收下时的附带动作（编辑模型收下会把当前供应商切过去）。
pub(in crate::config_tui) fn run_edit_form(
    ui: &mut Ui,
    title: &str,
    fields: &mut [Field],
) -> Result<bool> {
    run_edit_form_from(ui, title, fields, false)
}

/// 同 [`run_edit_form`]，一进来插入点就落在第一格（见 [`run_form_editing`]）。
pub(in crate::config_tui) fn run_edit_form_editing(
    ui: &mut Ui,
    title: &str,
    fields: &mut [Field],
) -> Result<bool> {
    run_edit_form_from(ui, title, fields, true)
}

/// 新增和编辑共用一张表单时：新增的保留「保存 / 返回」（Esc 不该建出半成品），
/// 编辑已有项的走 [`run_edit_form`]（用户 09-26）。
pub(in crate::config_tui) fn run_item_form(
    ui: &mut Ui,
    title: &str,
    fields: &mut [Field],
    existing: bool,
) -> Result<bool> {
    if existing {
        run_edit_form(ui, title, fields)
    } else {
        run_form(ui, title, fields)
    }
}

/// 同 [`run_item_form`]，一进来插入点就落在第一格。
pub(in crate::config_tui) fn run_item_form_editing(
    ui: &mut Ui,
    title: &str,
    fields: &mut [Field],
    existing: bool,
) -> Result<bool> {
    if existing {
        run_edit_form_editing(ui, title, fields)
    } else {
        run_form_editing(ui, title, fields)
    }
}

/// 带跳转行、编辑已有项的表单：不挂按钮，Esc 退出时交回 `Saved`（= 看完了），回车落在
/// 跳转行交回 `Link`。收不收由调用方拿进来时的 [`field_values`] 比对决定——跳转一次就要
/// 重进一次表单，改没改过得从最早那一刻算。
pub(in crate::config_tui) fn run_edit_form_linked(
    ui: &mut Ui,
    title: &str,
    fields: &mut [Field],
    selected: usize,
) -> Result<FormOutcome> {
    run_form_outcome(ui, title, fields, false, selected, false)
}

/// 各字段此刻的值，给「改没改过」做底。
pub(in crate::config_tui) fn field_values(fields: &[Field]) -> Vec<String> {
    fields.iter().map(|field| field.value.clone()).collect()
}

/// 和 [`field_values`] 拿到的那份比，有没有哪一格变了。
pub(in crate::config_tui) fn fields_changed(fields: &[Field], before: &[String]) -> bool {
    fields.len() != before.len()
        || fields
            .iter()
            .zip(before)
            .any(|(field, value)| field.value != *value)
}

fn run_edit_form_from(
    ui: &mut Ui,
    title: &str,
    fields: &mut [Field],
    start_editing: bool,
) -> Result<bool> {
    let before = field_values(fields);
    let mut selected = 0;
    let mut start_editing = start_editing;
    // 这里没人接跳转行：回车落在上面等于没按，原地接着跑。
    while let FormOutcome::Link(index) =
        run_form_outcome(ui, title, fields, start_editing, selected, false)?
    {
        selected = index;
        start_editing = false;
    }
    Ok(fields_changed(fields, &before))
}

/// 表单主循环。`buttons` = 末尾挂「保存 / 返回」两行：Esc 是放弃，`s` 是保存。不挂的时候
/// Esc 是「看完了」（交回 `Saved`，收不收由调用方比对），光标只在字段之间走。
fn run_form_outcome(
    ui: &mut Ui,
    title: &str,
    fields: &mut [Field],
    start_editing: bool,
    start_selected: usize,
    buttons: bool,
) -> Result<FormOutcome> {
    let last = if buttons {
        fields.len() + 1
    } else {
        fields.len().saturating_sub(1)
    };
    let mut selected = start_selected.min(last);
    let mut fcitx = FcitxState::new();
    // Only a plain text field can be typed into directly; the others open
    // their own picker on Enter, so landing "inside" them would mean typing
    // free text where a choice was expected.
    let mut editing = start_editing
        && fields.first().is_some_and(|field| {
            !field.boolean
                && !field.textarea
                && !field.modalities
                && !field.link
                && field.multi_choices.is_empty()
                && field.choices.is_empty()
        });
    if editing {
        fcitx.enter_editing();
    }
    let mut cursors = fields
        .iter()
        .map(|field| field.value.chars().count())
        .collect::<Vec<_>>();
    loop {
        draw_form(ui, title, fields, selected, editing, &cursors, buttons)?;
        match read_key(ui)? {
            KeyCode::Esc if editing => {
                fcitx.leave_editing();
                editing = false;
            }
            KeyCode::Esc | KeyCode::Char('q') if !editing => {
                return Ok(if buttons {
                    FormOutcome::Cancelled
                } else {
                    FormOutcome::Saved
                })
            }
            KeyCode::Enter if editing => {
                fcitx.leave_editing();
                editing = false;
            }
            KeyCode::Enter if !editing && buttons && selected == fields.len() => {
                return Ok(FormOutcome::Saved)
            }
            KeyCode::Enter if !editing && buttons && selected == fields.len() + 1 => {
                return Ok(FormOutcome::Cancelled)
            }
            KeyCode::Enter if !editing && fields[selected].link => {
                return Ok(FormOutcome::Link(selected))
            }
            KeyCode::Enter if !editing && fields[selected].boolean => {
                let value = select_bool(
                    ui,
                    fields[selected].label,
                    parse_bool_field(&fields[selected].value)?,
                )?;
                fields[selected].value = value.to_string();
                cursors[selected] = fields[selected].value.chars().count();
            }
            KeyCode::Enter if !editing && !fields[selected].multi_choices.is_empty() => {
                fields[selected].value = select_multi_choice(
                    ui,
                    fields[selected].label,
                    &fields[selected].value,
                    &fields[selected].multi_choices.clone(),
                )?;
                cursors[selected] = fields[selected].value.chars().count();
            }
            KeyCode::Enter if !editing && fields[selected].modalities => {
                fields[selected].value = select_multi_choice(
                    ui,
                    fields[selected].label,
                    &fields[selected].value,
                    &["text", "image", "audio", "video", "pdf"]
                        .iter()
                        .map(|item| item.to_string())
                        .collect::<Vec<_>>(),
                )?;
                cursors[selected] = fields[selected].value.chars().count();
            }
            KeyCode::Enter if !editing && !fields[selected].choices.is_empty() => {
                fields[selected].value = select_choice(
                    ui,
                    fields[selected].label,
                    &fields[selected].value,
                    &fields[selected].choices,
                    fields[selected].empty_choice_label,
                    fields[selected].raw_choice_labels,
                )?;
                cursors[selected] = fields[selected].value.chars().count();
            }
            KeyCode::Enter if !editing && fields[selected].dialog_list => {
                edit_dialog_list(ui, &mut fields[selected].value)?;
                cursors[selected] = fields[selected].value.chars().count();
            }
            KeyCode::Enter if !editing && fields[selected].string_list => {
                edit_string_list(ui, fields[selected].label, &mut fields[selected].value)?;
                cursors[selected] = fields[selected].value.chars().count();
            }
            KeyCode::Enter if !editing && fields[selected].textarea => {
                edit_textarea(ui, &mut fields[selected].value)?;
                cursors[selected] = fields[selected].value.chars().count();
                if !fields[selected].sensitive {
                    return Ok(FormOutcome::Saved);
                }
            }
            KeyCode::Enter if !editing => {
                if !fields[selected].boolean {
                    fcitx.enter_editing();
                    editing = true;
                }
            }
            KeyCode::Char('s') if !editing && buttons => return Ok(FormOutcome::Saved),
            KeyCode::Up | KeyCode::Char('k') if !editing => selected = selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') if !editing => selected = (selected + 1).min(last),
            KeyCode::Left | KeyCode::Char('h')
                if !editing && buttons && selected == fields.len() + 1 =>
            {
                selected = fields.len()
            }
            KeyCode::Right | KeyCode::Char('l')
                if !editing && buttons && selected == fields.len() =>
            {
                selected = fields.len() + 1
            }
            KeyCode::Left if editing => cursors[selected] = cursors[selected].saturating_sub(1),
            KeyCode::Right if editing => {
                cursors[selected] =
                    (cursors[selected] + 1).min(fields[selected].value.chars().count())
            }
            KeyCode::Home if editing => cursors[selected] = 0,
            KeyCode::End if editing => cursors[selected] = fields[selected].value.chars().count(),
            KeyCode::Backspace if editing => {
                if cursors[selected] > 0 {
                    remove_char_before_cursor(&mut fields[selected].value, &mut cursors[selected]);
                }
            }
            KeyCode::Delete if editing => {
                remove_char_at_cursor(&mut fields[selected].value, cursors[selected])
            }
            KeyCode::Char(char) if editing => {
                insert_char_at_cursor(&mut fields[selected].value, &mut cursors[selected], char)
            }
            _ => {}
        }
    }
}

/// 没有「保存 / 返回」的表单：Esc 退出，调用方照单收下（全局设置、开发模式、语音这几张）。
/// 和 [`run_edit_form`] 同一个主循环，只是不管改没改过。
pub(in crate::config_tui) fn run_form_without_buttons(
    ui: &mut Ui,
    title: &str,
    fields: &mut [Field],
) -> Result<()> {
    run_edit_form(ui, title, fields).map(|_| ())
}

/// 预设对话列表式编辑器(验收 #19):每行一对 user/assistant,回车编辑、
/// [a] 新增、[d] 删除;退出时把列表写回 `user:`/`assistant:` 行格式,
/// 与手写 dialogs 文件同构,存量文件无需迁移。
pub(in crate::config_tui) fn edit_dialog_list(ui: &mut Ui, value: &mut String) -> Result<()> {
    let mut pairs = miyu_core::persona_hint::parse_dialogs(value);
    let mut selected = 0usize;
    loop {
        let mut options: Vec<String> = pairs
            .iter()
            .map(|(question, answer)| {
                format!(
                    "user: {}  assistant: {}",
                    truncate(question.lines().next().unwrap_or(""), 20),
                    truncate(answer.lines().next().unwrap_or(""), 20),
                )
            })
            .collect();
        if options.is_empty() {
            options.push(t("(no preset dialogs)", "(暂无预设对话)").to_string());
        }
        selected = selected.min(options.len() - 1);
        draw_menu(
            ui,
            t(" PRESET DIALOGS ", " 预设对话 "),
            &options,
            selected,
            t(
                "[Enter]edit [a]add [d]delete [j/k]move [q]done",
                "[Enter]编辑 [a]新增 [d]删除 [j/k]移动 [q]完成",
            ),
        )?;
        match read_key(ui)? {
            KeyCode::Esc | KeyCode::Char('q') => {
                *value = miyu_core::persona_hint::format_dialogs(&pairs);
                return Ok(());
            }
            KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => selected = (selected + 1).min(options.len() - 1),
            KeyCode::Char('a') => {
                if let Some(pair) = edit_dialog_pair(ui, t(" NEW DIALOG ", " 新增对话 "), "", "")?
                {
                    pairs.push(pair);
                    selected = pairs.len() - 1;
                }
            }
            KeyCode::Enter if !pairs.is_empty() => {
                let (question, answer) = pairs[selected].clone();
                if let Some(pair) =
                    edit_dialog_pair(ui, t(" EDIT DIALOG ", " 编辑对话 "), &question, &answer)?
                {
                    pairs[selected] = pair;
                }
            }
            KeyCode::Char('d') if !pairs.is_empty() => {
                pairs.remove(selected);
            }
            _ => {}
        }
    }
}

/// user/assistant 双框表单:打开即落在 user 框内直接输入,回车确认后
/// j 移到 assistant 框。空的一侧视为放弃(与 `parse_dialogs` 丢弃
/// 空对的语义一致)。
/// 字符串列表式编辑器(唤醒词这类"几个短词"):回车编辑、[a] 新增、
/// [d] 删除、[j/k] 移动;value 是逗号分隔的序列化文本。
pub(in crate::config_tui) fn edit_string_list(
    ui: &mut Ui,
    title: &'static str,
    value: &mut String,
) -> Result<()> {
    let mut items: Vec<String> = miyu_base::config::split_wake_keywords(value);
    let mut selected = 0usize;
    loop {
        let mut options: Vec<String> = items.clone();
        if options.is_empty() {
            options.push(t("(empty)", "(空)").to_string());
        }
        selected = selected.min(options.len() - 1);
        draw_menu(
            ui,
            &format!(" {title} "),
            &options,
            selected,
            t(
                "[Enter]edit [a]add [d]delete [j/k]move [q]done",
                "[Enter]编辑 [a]新增 [d]删除 [j/k]移动 [q]完成",
            ),
        )?;
        match read_key(ui)? {
            KeyCode::Esc | KeyCode::Char('q') => {
                *value = items.join(", ");
                return Ok(());
            }
            KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => selected = (selected + 1).min(options.len() - 1),
            KeyCode::Char('a') => {
                if let Some(item) = edit_single_line(ui, t(" ADD ", " 新增 "), title, "")? {
                    if !items.contains(&item) {
                        items.push(item);
                        selected = items.len() - 1;
                    }
                }
            }
            KeyCode::Enter if !items.is_empty() => {
                let current = items[selected].clone();
                if let Some(item) = edit_single_line(ui, t(" EDIT ", " 编辑 "), title, &current)?
                {
                    items[selected] = item;
                }
            }
            KeyCode::Char('d') if !items.is_empty() => {
                items.remove(selected);
            }
            _ => {}
        }
    }
}

/// 弹一个单行输入表单;取消或留空返回 None。
pub(in crate::config_tui) fn edit_single_line(
    ui: &mut Ui,
    title: &str,
    label: &'static str,
    initial: &str,
) -> Result<Option<String>> {
    let mut fields = vec![Field::new(label, initial.to_string())];
    if !run_form_editing(ui, title, &mut fields)? {
        return Ok(None);
    }
    let text = fields[0].value.trim().to_string();
    Ok((!text.is_empty()).then_some(text))
}

pub(in crate::config_tui) fn edit_dialog_pair(
    ui: &mut Ui,
    title: &str,
    question: &str,
    answer: &str,
) -> Result<Option<(String, String)>> {
    let mut fields = vec![
        Field::new("user", question.to_string()),
        Field::new("assistant", answer.to_string()),
    ];
    if !run_form_editing(ui, title, &mut fields)? {
        return Ok(None);
    }
    let question = fields[0].value.trim().to_string();
    let answer = fields[1].value.trim().to_string();
    if question.is_empty() || answer.is_empty() {
        return Ok(None);
    }
    Ok(Some((question, answer)))
}

pub(in crate::config_tui) fn edit_textarea(ui: &mut Ui, value: &mut String) -> Result<()> {
    execute!(
        io::stdout(),
        Show,
        LeaveAlternateScreen,
        Clear(ClearType::All),
        MoveTo(0, 0)
    )?;
    io::stdout().flush()?;
    terminal::disable_raw_mode()?;
    let mut file = tempfile::NamedTempFile::new()?;
    file.write_all(value.as_bytes())?;
    let path = file.path().to_path_buf();
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vim".to_string());
    let status = Command::new(&editor)
        .arg(&path)
        .status()
        .or_else(|_| Command::new("nano").arg(&path).status());
    if let Err(err) = status {
        if is_zh() {
            eprintln!("无法打开编辑器: {err}");
        } else {
            eprintln!("Failed to open editor: {err}");
        }
    }
    *value = std::fs::read_to_string(&path)?.trim().to_string();
    terminal::enable_raw_mode()?;
    execute!(
        io::stdout(),
        EnterAlternateScreen,
        Clear(ClearType::All),
        Hide
    )?;
    // 备用屏被 $EDITOR 翻过一遍，ratatui 手里那份「屏幕现在长什么样」已经不作数。
    ui.invalidate();
    Ok(())
}

/// 表单：一行一个字段，左边名字、右边当前值；`show_buttons` 时末尾挂「保存 /
/// 返回」两个动作行。
///
/// 编辑态那一行不走 [`Cx::row`]：插入点要落在值的第几个字上，得自己算左边占了
/// 多宽——`row` 内部的补白是「至少两格」的弹性值，外面算不准。
pub(in crate::config_tui) fn draw_form(
    ui: &mut Ui,
    title: &str,
    fields: &[Field],
    selected: usize,
    editing: bool,
    cursors: &[usize],
    show_buttons: bool,
) -> Result<()> {
    let cx = ui.cx();
    let theme = ui.theme();
    let name_col = fields
        .iter()
        .map(|field| display_width(field.label) + 4)
        .max()
        .unwrap_or(NAME_COL_MIN)
        .clamp(NAME_COL_MIN, NAME_COL_MAX)
        .min(ui.body_width().saturating_sub(12).max(NAME_COL_MIN));

    let mut body: Vec<Line<'static>> = Vec::with_capacity(fields.len() + 3);
    let mut caret = None;
    for (index, field) in fields.iter().enumerate() {
        let here = index == selected;
        let value = field_display_value(field, here && editing);
        if here && editing {
            let head = format!("{}{}", theme.cursor(), field.label);
            let head_w = display_width(&head);
            let start = name_col.max(head_w + 2);
            let typed = take_chars(&field.value.replace('\n', " "), cursors[index]);
            caret = Some((body.len(), start + display_width(&typed)));
            body.push(cx.select(ln(vec![
                Span::styled(head, theme.fg(BLUE)),
                Span::raw(" ".repeat(start - head_w)),
                Span::raw(value),
            ])));
        } else {
            body.push(cx.row(here, field.label, &value, name_col));
        }
    }
    if show_buttons {
        body.push(nil());
        body.push(cx.action(
            selected == fields.len() && !editing,
            t(" Save ", " 保存 ").trim(),
        ));
        body.push(cx.action(
            selected == fields.len() + 1 && !editing,
            t(" Back ", " 返回 ").trim(),
        ));
    }
    // 光标行：按钮前面插了一个空行，光标跟随要把它算进去。
    let cursor_row = if selected < fields.len() {
        selected
    } else {
        selected + 1
    };

    let help = if editing {
        t(
            "[⏎]finish editing [Esc]finish editing",
            "[⏎]结束编辑 [Esc]结束编辑",
        )
    } else if show_buttons {
        t(
            "[↑↓ jk]move [⏎]edit / open editor [s]confirm [Esc]back",
            "[↑↓ jk]移动 [⏎]编辑 / 打开编辑器 [s]确认 [Esc]返回",
        )
    } else {
        t(
            "[↑↓ jk]move [⏎]edit / open editor [Esc]back",
            "[↑↓ jk]移动 [⏎]编辑 / 打开编辑器 [Esc]返回",
        )
    };
    let (_, keys) = key_bar(&cx, help);
    // 横线上方那行说的是「现在是什么状态」，不是按键——按键在底下那条。只在打字时写
    // 「编辑中」；导航时那句「导航中」是废话（用户 09-26），留一个空行，进出编辑版面不跳。
    let mode = if editing {
        t("Editing", "编辑中")
    } else {
        ""
    };
    ui.show(
        title,
        View {
            body,
            cursor_row,
            caret,
            footer: vec![cx.txt(mode, theme.dim(DIM))],
            counter: (!fields.is_empty())
                .then(|| format!("{}/{}", (selected + 1).min(fields.len()), fields.len())),
            keys,
            ..View::default()
        },
    )
}

pub(in crate::config_tui) fn field_display_value(field: &Field, reveal_sensitive: bool) -> String {
    if field.dialog_list {
        // 列表式字段没有 $EDITOR;摘要成对数,原始序列化文本不上屏。
        let pairs = miyu_core::persona_hint::parse_dialogs(&field.value).len();
        return if pairs == 0 {
            t("(empty; Enter opens the list)", "(空,回车进列表)").to_string()
        } else if is_zh() {
            format!("[{pairs} 对对话]")
        } else {
            format!("[{pairs} dialog pair(s)]")
        };
    }
    if field.sensitive && !field.value.is_empty() && !reveal_sensitive {
        if field.textarea {
            if is_zh() {
                format!("[已配置 {} 项]", parse_key_list(&field.value).len())
            } else {
                format!("[{} configured]", parse_key_list(&field.value).len())
            }
        } else {
            "********".to_string()
        }
    } else if !field.choices.is_empty() && field.value.is_empty() {
        field.empty_choice_label.to_string()
    } else if !field.choices.is_empty() {
        choice_display_label(
            &field.value,
            field.empty_choice_label,
            field.raw_choice_labels,
        )
    } else if field.boolean {
        match parse_bool_field(&field.value) {
            Ok(value) => boolean_label(value).to_string(),
            Err(_) => field.value.clone(),
        }
    } else if field.modalities {
        parse_modalities(&field.value)
            .iter()
            .map(|value| choice_label(value, ""))
            .collect::<Vec<_>>()
            .join(", ")
    } else {
        truncate(&field.value.replace('\n', " "), 70)
    }
}

pub(in crate::config_tui) struct Field {
    pub(in crate::config_tui) label: &'static str,
    pub(in crate::config_tui) value: String,
    pub(in crate::config_tui) textarea: bool,
    /// 预设对话列表:Enter 进入列表式子编辑器而不是 $EDITOR(验收 #19),
    /// value 仍是 `user:`/`assistant:` 行格式的序列化文本。
    pub(in crate::config_tui) dialog_list: bool,
    /// 短字符串列表(唤醒词):Enter 进入 [a]/[d] 列表编辑器,value 为逗号分隔文本。
    pub(in crate::config_tui) string_list: bool,
    pub(in crate::config_tui) sensitive: bool,
    pub(in crate::config_tui) boolean: bool,
    pub(in crate::config_tui) modalities: bool,
    /// 非空=回车弹通用多选菜单(Tab 勾选),value 为逗号分隔的选中项。
    pub(in crate::config_tui) multi_choices: Vec<String>,
    pub(in crate::config_tui) choices: Vec<String>,
    pub(in crate::config_tui) empty_choice_label: &'static str,
    pub(in crate::config_tui) raw_choice_labels: bool,
    /// 跳转行：值只是展示，回车交给调用方开另一个菜单（[`FormOutcome::Link`]）。
    /// 给「真相在别处」的设置用——在这里复制一份可编辑的值，两处迟早对不上
    /// （09-23 知识库那行 embedding 就是这么显示「未配置」的）。
    pub(in crate::config_tui) link: bool,
}

impl Field {
    pub fn new(label: &'static str, value: String) -> Self {
        Self {
            label,
            value,
            textarea: false,
            dialog_list: false,
            string_list: false,
            sensitive: false,
            boolean: false,
            modalities: false,
            multi_choices: Vec::new(),
            choices: Vec::new(),
            empty_choice_label: t("Use current provider", "使用当前 Provider"),
            raw_choice_labels: false,
            link: false,
        }
    }

    pub(in crate::config_tui) fn boolean(label: &'static str, value: bool) -> Self {
        Self {
            label,
            value: value.to_string(),
            textarea: false,
            dialog_list: false,
            string_list: false,
            sensitive: false,
            boolean: true,
            modalities: false,
            multi_choices: Vec::new(),
            choices: Vec::new(),
            empty_choice_label: t("Use current provider", "使用当前 Provider"),
            raw_choice_labels: false,
            link: false,
        }
    }

    pub(in crate::config_tui) fn textarea(label: &'static str, value: String) -> Self {
        Self {
            label,
            value,
            textarea: true,
            dialog_list: false,
            string_list: false,
            sensitive: false,
            boolean: false,
            modalities: false,
            multi_choices: Vec::new(),
            choices: Vec::new(),
            empty_choice_label: t("Use current provider", "使用当前 Provider"),
            raw_choice_labels: false,
            link: false,
        }
    }

    pub(in crate::config_tui) fn string_list(label: &'static str, value: String) -> Self {
        Self {
            string_list: true,
            ..Self::new(label, value)
        }
    }

    pub(in crate::config_tui) fn dialog_list(label: &'static str, value: String) -> Self {
        Self {
            dialog_list: true,
            ..Self::textarea(label, value)
        }
    }

    pub(in crate::config_tui) fn link(label: &'static str, value: String) -> Self {
        Self {
            link: true,
            ..Self::new(label, value)
        }
    }

    pub(in crate::config_tui) fn choices(mut self, choices: &[&str]) -> Self {
        self.choices = choices.iter().map(|item| item.to_string()).collect();
        self
    }

    pub(in crate::config_tui) fn multi_choices(mut self, choices: &[&str]) -> Self {
        self.multi_choices = choices.iter().map(|item| item.to_string()).collect();
        self
    }

    pub(in crate::config_tui) fn sensitive(mut self) -> Self {
        self.sensitive = true;
        self
    }

    pub(in crate::config_tui) fn modalities(label: &'static str, value: String) -> Self {
        Self {
            label,
            value,
            textarea: false,
            dialog_list: false,
            string_list: false,
            sensitive: false,
            boolean: false,
            modalities: true,
            multi_choices: Vec::new(),
            choices: Vec::new(),
            empty_choice_label: t("Use current provider", "使用当前 Provider"),
            raw_choice_labels: false,
            link: false,
        }
    }

    pub(in crate::config_tui) fn choices_owned(mut self, choices: Vec<String>) -> Self {
        self.choices = choices;
        self
    }

    pub(in crate::config_tui) fn empty_choice_label(mut self, label: &'static str) -> Self {
        self.empty_choice_label = label;
        self
    }

    pub(in crate::config_tui) fn raw_choice_labels(mut self) -> Self {
        self.raw_choice_labels = true;
        self
    }
}
