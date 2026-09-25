mod draw;
mod edit;
mod panel;
pub use draw::BAR as QUESTION_BAR;
pub(in crate::question_tui) use draw::*;
pub(in crate::question_tui) use edit::*;
pub use panel::{QuestionPanel, QuestionView};

use anyhow::{bail, Result};
use crossterm::cursor::{Hide, MoveTo, Show};
use crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use crossterm::terminal::{self, Clear, ClearType};
use crossterm::{execute, queue};
use miyu_base::i18n::text as t;
use miyu_base::question::{
    validate_answers, QuestionAnswers, QuestionPrompt, QuestionRequest, QuestionResponse,
    MAX_CUSTOM_ANSWER_CHARS,
};
use std::io::{self, IsTerminal, Write};
use std::time::{Duration, Instant};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const CANCEL_CONFIRM_WINDOW: Duration = Duration::from_secs(2);

pub fn available(plain: bool) -> bool {
    if plain || !io::stdout().is_terminal() {
        return false;
    }
    #[cfg(unix)]
    {
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/tty")
            .is_ok()
    }
    #[cfg(not(unix))]
    {
        io::stdin().is_terminal()
    }
}

/// 问一轮，允许调用方接管「面板上面那截正文」的回翻。
///
/// 面板开着的时候输入泵停了，滚轮和 PgUp 都没人接——可这正是最想往回看的时候
/// （要答的问题往往就指着上面那几行）。`scroll` 收到的是 `(方向, 面板占了几行)`，
/// 高度包含底部空行。首次绘制及布局变化也会调用它（方向为 0），让正文
/// 先为面板留出空间；滚动与绘制使用同一个正文视口。
/// `leave_summary`：答完之后要不要把「已回答 N 个问题」那几行留在面板原来的
/// 位置上。inline 的老样子是留；静态时间线自己把一问一答写成那一步的正文，
/// 面板得整个擦干净、光标放回面板顶上，那一步才落在原位。
pub fn ask_with(
    request: &QuestionRequest,
    scroll: Option<&mut dyn FnMut(isize, u16)>,
    leave_summary: bool,
) -> Result<QuestionResponse> {
    ask_watched(request, scroll, None, leave_summary).map(Asked::into_response)
}

/// 面板是怎么收场的。
#[derive(Debug)]
pub enum Asked {
    /// 在这块面板上答的 / 关的：结果要由调用方回发给 daemon。
    Here(QuestionResponse),
    /// 别处先有了结果（同一个会话的另一个终端或网页答了、回合在别处结束了），
    /// 面板自己收了场。结果已经在 daemon 那边，不用再回发。
    Elsewhere(QuestionResponse),
}

impl Asked {
    pub fn into_response(self) -> QuestionResponse {
        match self {
            Asked::Here(response) | Asked::Elsewhere(response) => response,
        }
    }
}

/// 同 [`ask_with`]，外加 `watch`：面板每一拍（约 100ms）问一次「别处有结果了吗」，
/// 有就按那个结果收场（09-24）。
///
/// 面板开着时事件泵是停着的，不这么问，另一个终端答完了这边的面板还一直挂着，
/// 按两下 Esc 更会把人家正跑着的回合掐掉。
pub fn ask_watched(
    request: &QuestionRequest,
    mut scroll: Option<&mut dyn FnMut(isize, u16)>,
    mut watch: Option<&mut dyn FnMut() -> Option<QuestionResponse>>,
    leave_summary: bool,
) -> Result<Asked> {
    request.validate()?;
    if !available(false) {
        bail!("interactive terminal is unavailable");
    }

    let panel_lines = terminal::size()
        .map(|(_, rows)| rows.saturating_sub(1).clamp(1, MAX_PANEL_LINES))
        .unwrap_or(12);
    reserve_space(panel_lines)?;
    let mut session = QuestionSession::start(panel_lines)?;
    let mut panel = QuestionPanel::new(request.clone())?;

    loop {
        if let Some(outcome) = watch.as_deref_mut().and_then(|watch| watch()) {
            match &outcome {
                QuestionResponse::Answered(answers) => {
                    session.finish_answered(request, answers, leave_summary)?
                }
                _ => session.finish_cancelled()?,
            }
            return Ok(Asked::Elsewhere(outcome));
        }
        panel.expire_cancel();
        draw(&mut session, &mut panel, &mut scroll)?;

        if !event::poll(Duration::from_millis(100))? {
            continue;
        }
        let event = event::read()?;
        // 回翻交给调用方：滚轮、PgUp/PgDn 都只动面板上面那截正文。
        if let Some(scroll) = scroll.as_deref_mut() {
            let delta = match &event {
                Event::Mouse(mouse) => match mouse.kind {
                    event::MouseEventKind::ScrollUp => Some(-3),
                    event::MouseEventKind::ScrollDown => Some(3),
                    _ => None,
                },
                Event::Key(key) if key.kind == KeyEventKind::Press && !panel.editing() => {
                    match key.code {
                        KeyCode::PageUp => Some(-10),
                        KeyCode::PageDown => Some(10),
                        _ => None,
                    }
                }
                _ => None,
            };
            if let Some(delta) = delta {
                scroll(delta, session.occupied_rows());
                continue;
            }
        }
        match event {
            Event::Resize(_, rows) => session.resize_to_terminal(rows),
            Event::Paste(text) => panel.on_paste(&text),
            Event::Key(key) => match panel.on_key(key)? {
                Some(QuestionResponse::Answered(answers)) => {
                    session.finish_answered(request, &answers, leave_summary)?;
                    return Ok(Asked::Here(QuestionResponse::Answered(answers)));
                }
                Some(other) => {
                    session.finish_cancelled()?;
                    return Ok(Asked::Here(other));
                }
                None => {}
            },
            _ => {}
        }
    }
}

struct QuestionState {
    tab: usize,
    selected: Vec<usize>,
    scroll_starts: Vec<usize>,
    answers: QuestionAnswers,
    custom_answers: Vec<String>,
    editing: bool,
    edit_buffer: String,
    edit_cursor: usize,
    cancel_armed_until: Option<Instant>,
}

impl QuestionState {
    fn new(request: &QuestionRequest) -> Self {
        Self {
            tab: 0,
            selected: vec![0; request.questions.len()],
            scroll_starts: vec![0; request.questions.len() + usize::from(request.needs_review())],
            answers: vec![Vec::new(); request.questions.len()],
            custom_answers: vec![String::new(); request.questions.len()],
            editing: false,
            edit_buffer: String::new(),
            edit_cursor: 0,
            cancel_armed_until: None,
        }
    }

    fn on_confirm(&self, request: &QuestionRequest) -> bool {
        request.needs_review() && self.tab == request.questions.len()
    }

    fn tab_count(&self, request: &QuestionRequest) -> usize {
        request.questions.len() + usize::from(request.needs_review())
    }

    fn previous_tab(&mut self, request: &QuestionRequest) {
        let count = self.tab_count(request);
        self.tab = (self.tab + count - 1) % count;
    }

    fn next_tab(&mut self, request: &QuestionRequest) {
        self.tab = (self.tab + 1) % self.tab_count(request);
    }

    fn previous_option(&mut self, question: &QuestionPrompt) {
        let count = option_count(question);
        if count > 0 {
            let selected = &mut self.selected[self.tab];
            *selected = (*selected + count - 1) % count;
        }
    }

    fn next_option(&mut self, question: &QuestionPrompt) {
        let count = option_count(question);
        if count > 0 {
            self.selected[self.tab] = (self.selected[self.tab] + 1) % count;
        }
    }

    fn activate_current(&mut self, request: &QuestionRequest) -> Result<()> {
        let question = &request.questions[self.tab];
        let selected = self.selected[self.tab];
        if selected == question.options.len() && question.custom {
            self.editing = true;
            self.edit_buffer = self.custom_answers[self.tab].clone();
            self.edit_cursor = self.edit_buffer.chars().count();
            return Ok(());
        }
        let Some(option) = question.options.get(selected) else {
            bail!("selected question option is out of range");
        };
        if question.multiple {
            toggle_answer(&mut self.answers[self.tab], &option.label);
        } else {
            self.answers[self.tab] = vec![option.label.clone()];
            self.advance_after_single(request);
        }
        Ok(())
    }

    fn toggle_current(&mut self, request: &QuestionRequest) -> Result<()> {
        let question = &request.questions[self.tab];
        let selected = self.selected[self.tab];
        if selected == question.options.len() && question.custom {
            let custom = self.custom_answers[self.tab].trim();
            if custom.is_empty() {
                return self.activate_current(request);
            }
            toggle_answer(&mut self.answers[self.tab], custom);
            return Ok(());
        }
        self.activate_current(request)
    }

    fn advance_after_single(&mut self, request: &QuestionRequest) {
        if request.questions.len() == 1 && !request.needs_review() {
            return;
        }
        self.tab = (self.tab + 1).min(self.tab_count(request) - 1);
    }

    fn go_to_first_unanswered(&mut self, request: &QuestionRequest) {
        if let Some(index) = self.answers.iter().position(Vec::is_empty) {
            self.tab = index.min(request.questions.len().saturating_sub(1));
        }
    }
}

fn submitted_answers(
    request: &QuestionRequest,
    state: &QuestionState,
) -> Result<Option<QuestionAnswers>> {
    if state.editing || state.answers.iter().any(Vec::is_empty) {
        return Ok(None);
    }
    if request.needs_review() && !state.on_confirm(request) {
        return Ok(None);
    }
    validate_answers(request, &state.answers)?;
    Ok(Some(state.answers.clone()))
}

fn option_count(question: &QuestionPrompt) -> usize {
    question.options.len() + usize::from(question.custom)
}

fn toggle_answer(answers: &mut Vec<String>, value: &str) {
    if let Some(index) = answers.iter().position(|answer| answer == value) {
        answers.remove(index);
    } else {
        answers.push(value.to_string());
    }
}

struct QuestionSession {
    stdout: io::Stdout,
    anchor_y: u16,
    panel_lines: u16,
    painted_rows: u16,
    geometry: Option<(u16, u16, u16)>,
    keyboard_enhancement_active: bool,
    /// 面板是不是终端模式的所有者。
    ///
    /// REPL 的 `LiveRawMode` 早就把终端切进了 raw,面板是嵌在它里面跑的。
    /// 此前 `start` 无条件 `enable_raw_mode`、`Drop` 无条件
    /// `disable_raw_mode`,退出时把 REPL 的 raw 一起关掉,终端落回
    /// canonical——内核开始回显、Ctrl+C 显示成 `^C`、按键要等 Enter 才整行
    /// 送进来,回车后回显被重绘抹掉,只剩正文发出去。实测回答完毕后 pty 是
    /// `ICANON=True ECHO=True`,修复后是 `False/False`。
    ///
    /// bracketed paste 与键盘增强同理:关掉了自己没开的东西;键盘协议多推
    /// 一层就得多弹一层,数量对不上终端就停在错误的协议上。
    ///
    /// 因此只有「进来时终端还没被切进 raw」才动这些开关,嵌套运行时一概
    /// 不碰,原样交还。
    owns_terminal: bool,
}

impl QuestionSession {
    fn start(panel_lines: u16) -> Result<Self> {
        let owns_terminal = !terminal::is_raw_mode_enabled().unwrap_or(false);
        if owns_terminal {
            terminal::enable_raw_mode()?;
        }
        // 嵌在 REPL 里时看门狗早已在跑(Once 幂等);独立持有终端时这里兜底。
        crate::cli::spawn_hangup_watchdog();
        let mut stdout = io::stdout();
        let entered = if owns_terminal {
            execute!(stdout, EnableBracketedPaste, Hide)
        } else {
            // 嵌套在 REPL 里:bracketed paste 已经由 `LiveRawMode` 开着。
            execute!(stdout, Hide)
        };
        if let Err(err) = entered {
            if owns_terminal {
                let _ = execute!(stdout, DisableBracketedPaste);
                let _ = terminal::disable_raw_mode();
            }
            let _ = execute!(stdout, Show);
            return Err(err.into());
        }
        // 1. 尽量启用键盘增强，使 Shift+Enter 可被识别
        // 2. Windows 旧控制台可能不支持，失败时仍保持普通输入
        // 嵌套时 REPL 已经 push 过一层,不再叠加。
        let keyboard_enhancement_active = if cfg!(windows) || !owns_terminal {
            false
        } else {
            execute!(
                stdout,
                PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
            )
            .is_ok()
        };
        // 全屏下**别去问光标在哪**。
        //
        // `crossterm::cursor::position()` 走的是 ESC[6n：应答要从 stdin 读，而
        // 全屏这条路上 stdin 正被输入泵盯着，问不到就退回一个假值
        // （`panel_lines - 1`）——于是 anchor 落到 0，面板跑到屏幕最上面，
        // 底下空出一大片（用户实测「为什么离底边框那么远」）。
        // 位置本来就是算得出来的：`reserve_space` 已经把它定在底边。
        let anchor_y = if crate::cli::in_fullscreen() {
            let rows = crossterm::terminal::size()
                .map(|(_, rows)| rows)
                .unwrap_or(24);
            rows.saturating_sub(panel_lines)
        } else {
            let (_, cursor_y) =
                crossterm::cursor::position().unwrap_or((0, panel_lines.saturating_sub(1)));
            cursor_y.saturating_sub(panel_lines.saturating_sub(1))
        };
        Ok(Self {
            stdout,
            anchor_y,
            panel_lines,
            painted_rows: 0,
            geometry: None,
            keyboard_enhancement_active,
            owns_terminal,
        })
    }

    fn finish_answered(
        &mut self,
        request: &QuestionRequest,
        answers: &QuestionAnswers,
        leave_summary: bool,
    ) -> Result<()> {
        self.clear()?;
        if !leave_summary {
            // 什么都不留：光标回到面板顶上，调用方接着在这儿写它自己的那一步。
            queue!(self.stdout, MoveTo(0, self.anchor_y), Show)?;
            self.stdout.flush()?;
            return Ok(());
        }
        let width = terminal::size().map(|(cols, _)| cols).unwrap_or(80) as usize;
        let content_width = width.saturating_sub(3).max(1);
        let available_rows = self.occupied_rows();
        let keeps_blank_line = available_rows > 1;
        let content_rows = available_rows
            .saturating_sub(u16::from(keeps_blank_line))
            .max(1);
        let answer_capacity = content_rows.saturating_sub(1) as usize;
        let omitted = request.questions.len().saturating_sub(answer_capacity);
        let mut row = 0u16;
        let heading = if omitted == 0 {
            format!(
                "{} {} {}",
                t("Answered", "已回答"),
                request.questions.len(),
                t("questions", "个问题")
            )
        } else {
            format!(
                "{} {} {} · {} {}",
                t("Answered", "已回答"),
                request.questions.len(),
                t("questions", "个问题"),
                t("omitted", "省略"),
                omitted
            )
        };
        self.write_answered_line(row, &heading, content_width)?;
        row += 1;
        for (question, selected) in request.questions.iter().zip(answers).take(answer_capacity) {
            self.write_answered_line(
                row,
                &format!(
                    "{}: {}",
                    question.header,
                    display_inline(&selected.join("、"))
                ),
                content_width,
            )?;
            row += 1;
        }
        if keeps_blank_line {
            queue!(
                self.stdout,
                MoveTo(0, self.anchor_y.saturating_add(row)),
                Clear(ClearType::CurrentLine),
                crossterm::style::Print("\r\n")
            )?;
        } else {
            queue!(
                self.stdout,
                MoveTo(0, self.anchor_y.saturating_add(row.saturating_sub(1))),
                crossterm::style::Print("\r\n")
            )?;
        }
        queue!(self.stdout, Clear(ClearType::CurrentLine), Show)?;
        self.stdout.flush()?;
        Ok(())
    }

    fn finish_cancelled(&mut self) -> Result<()> {
        self.clear()?;
        queue!(
            self.stdout,
            MoveTo(0, self.anchor_y),
            crossterm::style::Print(format!(
                "{BAR} \x1b[2m{}\x1b[0m",
                t("Question cancelled", "已取消提问")
            )),
            MoveTo(0, self.anchor_y.saturating_add(1)),
            Clear(ClearType::CurrentLine),
            Show
        )?;
        self.stdout.flush()?;
        Ok(())
    }

    fn write_answered_line(&mut self, row: u16, text: &str, width: usize) -> Result<()> {
        queue!(
            self.stdout,
            MoveTo(0, self.anchor_y.saturating_add(row)),
            Clear(ClearType::CurrentLine),
            crossterm::style::Print(ANSWERED_BAR),
            crossterm::style::Print(" \x1b[2m\x1b[90m"),
            crossterm::style::Print(truncate_width(text, width)),
            crossterm::style::Print("\x1b[0m")
        )?;
        Ok(())
    }

    fn occupied_rows(&self) -> u16 {
        if crate::cli::in_fullscreen() {
            self.painted_rows.saturating_add(1)
        } else {
            self.panel_lines
        }
    }

    fn clear(&mut self) -> Result<()> {
        let rows = if crate::cli::in_fullscreen() {
            self.painted_rows
        } else {
            self.panel_lines
        };
        for row in 0..rows {
            queue!(
                self.stdout,
                MoveTo(0, self.anchor_y.saturating_add(row)),
                Clear(ClearType::CurrentLine)
            )?;
        }
        Ok(())
    }

    fn resize_to_terminal(&mut self, rows: u16) {
        self.panel_lines = rows.saturating_sub(1).clamp(1, MAX_PANEL_LINES);
        self.anchor_y = self.anchor_y.min(rows.saturating_sub(self.panel_lines));
    }
}

impl Drop for QuestionSession {
    fn drop(&mut self) {
        // 1. 恢复括号粘贴与光标
        // 2. 若启用过键盘增强则 Pop
        // 3. 退出 raw mode
        // 光标一定要交还;raw / bracketed paste / 键盘协议只还自己借的那份。
        let _ = execute!(self.stdout, Show);
        if self.owns_terminal {
            let _ = execute!(self.stdout, DisableBracketedPaste);
            if self.keyboard_enhancement_active {
                let _ = execute!(self.stdout, PopKeyboardEnhancementFlags);
                self.keyboard_enhancement_active = false;
            }
            let _ = terminal::disable_raw_mode();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use miyu_base::question::QuestionOption;

    fn multi_request() -> QuestionRequest {
        QuestionRequest {
            questions: vec![QuestionPrompt {
                header: "范围".to_string(),
                question: "选择范围".to_string(),
                options: vec![
                    QuestionOption {
                        label: "代码".to_string(),
                        description: String::new(),
                    },
                    QuestionOption {
                        label: "文档".to_string(),
                        description: String::new(),
                    },
                ],
                multiple: true,
                custom: true,
            }],
        }
    }

    #[test]
    fn multi_activation_toggles_selected_option() {
        let request = multi_request();
        let mut state = QuestionState::new(&request);
        state.activate_current(&request).unwrap();
        assert_eq!(state.answers[0], vec!["代码"]);
        state.activate_current(&request).unwrap();
        assert!(state.answers[0].is_empty());
    }

    #[test]
    fn left_and_right_cycle_question_tabs() {
        let mut request = multi_request();
        request.questions.push(request.questions[0].clone());
        let mut state = QuestionState::new(&request);
        state.next_tab(&request);
        assert_eq!(state.tab, 1);
        state.previous_tab(&request);
        assert_eq!(state.tab, 0);
    }

    #[test]
    fn custom_input_is_sanitized_and_bounded() {
        let mut value = String::new();
        let mut cursor = 0;
        let input = format!("a\u{1b}\t{}", "b".repeat(MAX_CUSTOM_ANSWER_CHARS));
        insert_text(&mut value, &mut cursor, &input);
        assert!(!value.contains('\u{1b}'));
        assert!(!value.contains('\t'));
        assert_eq!(value.chars().count(), MAX_CUSTOM_ANSWER_CHARS);
    }

    #[test]
    fn editor_view_keeps_caret_visible() {
        let (view, cursor) = editor_view("abcdefghijkl", 10, 6);
        assert!(view.starts_with('…'));
        assert!(cursor <= 6);
        assert!(UnicodeWidthStr::width(view.as_str()) <= 6);
    }

    #[test]
    fn final_answer_waits_on_review_tab() {
        let mut request = multi_request();
        request.questions[0].multiple = false;
        request.questions[0].custom = false;
        request.questions.push(request.questions[0].clone());
        let mut state = QuestionState::new(&request);
        state.activate_current(&request).unwrap();
        state.activate_current(&request).unwrap();
        assert!(state.on_confirm(&request));
        assert!(submitted_answers(&request, &state).unwrap().is_some());
    }

    #[test]
    fn existing_custom_answer_reopens_for_editing() {
        let request = QuestionRequest {
            questions: vec![QuestionPrompt {
                header: "范围".to_string(),
                question: "选择范围".to_string(),
                options: Vec::new(),
                multiple: false,
                custom: true,
            }],
        };
        let mut state = QuestionState::new(&request);
        state.custom_answers[0] = "已有答案".to_string();
        state.answers[0] = vec!["已有答案".to_string()];
        state.activate_current(&request).unwrap();
        assert!(state.editing);
        assert_eq!(state.edit_buffer, "已有答案");
    }

    #[test]
    fn existing_multi_custom_answer_can_be_toggled_off() {
        let mut request = multi_request();
        let mut state = QuestionState::new(&request);
        state.selected[0] = request.questions[0].options.len();
        state.custom_answers[0] = "已有答案".to_string();
        state.answers[0] = vec!["已有答案".to_string()];
        state.toggle_current(&request).unwrap();
        assert!(state.answers[0].is_empty());

        request.questions[0].multiple = false;
        state.activate_current(&request).unwrap();
        assert!(state.editing);
    }

    #[test]
    fn option_rows_have_no_numbers_and_put_description_below_title() {
        let lines = option_lines("烧烤", "烤肉串、烤鸡翅、烤韭菜", true, false, false, 16);
        let visible = lines
            .iter()
            .map(|line| strip_ansi(line))
            .collect::<Vec<_>>();
        assert_eq!(visible[0], "› 烧烤");
        assert!(!visible.iter().any(|line| line.contains("1.")));
        assert!(visible[1..].iter().all(|line| line.starts_with("  ")));
        assert!(lines[1..].iter().all(|line| line.contains("\x1b[2m")));
    }

    #[test]
    fn multi_option_rows_keep_checkbox_without_number() {
        let lines = option_lines("代码", "修改实现和测试", true, true, true, 18);
        assert_eq!(strip_ansi(&lines[0]), "› [✓] 代码");
        assert!(strip_ansi(&lines[1]).starts_with("      "));
    }

    #[test]
    fn description_soft_wrap_preserves_indentation_budget() {
        let lines = option_lines("烧烤", "烤肉串烤鸡翅烤韭菜", false, false, false, 10);
        assert!(lines.len() > 2);
        for line in &lines[1..] {
            assert!(UnicodeWidthStr::width(strip_ansi(line).as_str()) <= 10);
            assert!(strip_ansi(line).starts_with("  "));
        }
    }

    #[test]
    fn long_option_label_soft_wraps_within_width() {
        let lines = option_lines("烧烤烤肉串烤鸡翅烤韭菜拼盘", "", true, false, false, 10);
        assert!(lines.len() > 1);
        for line in &lines {
            assert!(UnicodeWidthStr::width(strip_ansi(line).as_str()) <= 10);
        }
        assert!(strip_ansi(&lines[1]).starts_with("  "));
        assert!(lines[1].contains("\x1b[35m"));
    }

    #[test]
    fn long_top_section_keeps_minimum_body_rows() {
        let layout = panel_layout(14, 6, 2, 16, Some(0), 0);
        assert!(layout.body_capacity >= 3);
        assert_eq!(layout.top_start + layout.top_budget, 14);
    }

    #[test]
    fn resize_recovers_panel_height_after_terminal_grows() {
        let mut session = std::mem::ManuallyDrop::new(QuestionSession {
            stdout: io::stdout(),
            anchor_y: 8,
            panel_lines: 12,
            painted_rows: 0,
            geometry: None,
            keyboard_enhancement_active: false,
            owns_terminal: false,
        });
        session.resize_to_terminal(3);
        assert_eq!(session.panel_lines, 2);
        session.resize_to_terminal(24);
        assert_eq!(session.panel_lines, MAX_PANEL_LINES);
    }

    #[test]
    fn truncation_honors_very_narrow_widths() {
        assert_eq!(truncate_width("abcdef", 1), ".");
        assert_eq!(truncate_width("abcdef", 2), "..");
        assert_eq!(
            UnicodeWidthStr::width(truncate_width("中文测试", 3).as_str()),
            3
        );
    }

    #[test]
    fn selected_option_uses_color_without_bold() {
        let lines = option_lines("烧烤", "", true, false, false, 20);
        assert!(lines[0].contains("\x1b[35m"));
        assert!(!lines[0].contains("\x1b[1m"));
    }

    #[test]
    fn custom_editor_has_no_extra_ascii_pointer() {
        let line = editor_option_line(false, false, "自定义内容");
        assert_eq!(strip_ansi(&line), "› 自定义内容");
        assert!(!strip_ansi(&line).contains('>'));
    }

    #[test]
    fn ctrl_j_inserts_custom_answer_newline() {
        let request = QuestionRequest {
            questions: vec![QuestionPrompt {
                header: "说明".to_string(),
                question: "补充说明".to_string(),
                options: Vec::new(),
                multiple: false,
                custom: true,
            }],
        };
        let mut state = QuestionState::new(&request);
        state.editing = true;
        state.edit_buffer = "前".to_string();
        state.edit_cursor = 1;
        handle_editing_key(
            &request,
            &mut state,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL),
        )
        .unwrap();
        assert_eq!(state.edit_buffer, "前\n");
    }

    #[test]
    fn shift_enter_inserts_custom_answer_newline() {
        let request = QuestionRequest {
            questions: vec![QuestionPrompt {
                header: "说明".to_string(),
                question: "补充说明".to_string(),
                options: Vec::new(),
                multiple: false,
                custom: true,
            }],
        };
        let mut state = QuestionState::new(&request);
        state.editing = true;
        state.edit_buffer = "前".to_string();
        state.edit_cursor = 1;
        handle_editing_key(
            &request,
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT),
        )
        .unwrap();
        assert_eq!(state.edit_buffer, "前\n");
    }

    #[test]
    fn scrolling_only_changes_body_window() {
        let first = panel_layout(3, 30, 1, 16, Some(0), 0);
        let last = panel_layout(3, 30, 1, 16, Some(29), first.body_start);
        assert_eq!(first.top_budget, 3);
        assert_eq!(last.top_budget, 3);
        assert_eq!(first.footer_start, 0);
        assert_eq!(last.footer_start, 0);
        assert_eq!(first.body_capacity, 12);
        assert_ne!(first.body_start, last.body_start);
    }

    #[test]
    fn scrolling_waits_until_focus_crosses_viewport_edge() {
        let inside = panel_layout(2, 12, 1, 8, Some(4), 0);
        assert_eq!(inside.body_capacity, 5);
        assert_eq!(inside.body_start, 0);

        let below = panel_layout(2, 12, 1, 8, Some(5), inside.body_start);
        assert_eq!(below.body_start, 1);

        let still_inside = panel_layout(2, 12, 1, 8, Some(4), below.body_start);
        assert_eq!(still_inside.body_start, 1);

        let above = panel_layout(2, 12, 1, 8, Some(0), still_inside.body_start);
        assert_eq!(above.body_start, 0);
    }
}
