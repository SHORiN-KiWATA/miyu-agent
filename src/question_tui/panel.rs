//! 提问面板的状态与按键，不碰终端（会话项目第 3 段，B4）。
//!
//! 自己跑终端的 `ask_watched` 用它；全屏终端里回合循环把它开在活动区的位置上，面板开着
//! 正文照流（`cli::repl::question_flow`）。

use crate::question_tui::*;

pub struct QuestionPanel {
    request: QuestionRequest,
    state: QuestionState,
}

/// 这一帧要画的行和光标。
pub struct QuestionView {
    /// 每行不含行首的 `┃ `（[`QUESTION_BAR`] 加一个空格）。
    pub lines: Vec<String>,
    /// 正在输入自定义答案、而且那一行露在面板里时，光标在第几行（`lines` 的下标）、
    /// 第几列（从行首竖条算起）。
    pub cursor: Option<(usize, usize)>,
}

impl QuestionPanel {
    pub fn new(request: QuestionRequest) -> Result<Self> {
        request.validate()?;
        let state = QuestionState::new(&request);
        Ok(Self { request, state })
    }

    pub fn request(&self) -> &QuestionRequest {
        &self.request
    }

    /// 正在输入自定义答案（这时 PgUp/PgDn 归输入框，不翻正文）。
    pub fn editing(&self) -> bool {
        self.state.editing
    }

    /// 最多几行：问题再长也不占满整屏，不然看不到自己在回答什么。
    pub fn max_rows() -> u16 {
        MAX_PANEL_LINES
    }

    /// 按 `content_width` 折行之后一共要几行（面板的高度按它定，再被 [`Self::max_rows`]
    /// 截住）。
    pub fn rows_needed(&self, content_width: usize) -> usize {
        sections(&self.request, &self.state, content_width).line_count()
    }

    /// 最多 `max_lines` 行、每行最宽 `content_width` 的样子。
    pub fn view(&mut self, content_width: usize, max_lines: usize) -> QuestionView {
        view(&self.request, &mut self.state, content_width, max_lines)
    }

    /// 「再按一次 Esc 取消」过了时限，那行提示撤掉。
    pub fn expire_cancel(&mut self) {
        if self
            .state
            .cancel_armed_until
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.state.cancel_armed_until = None;
        }
    }

    pub fn on_paste(&mut self, text: &str) {
        if self.state.editing {
            insert_text(
                &mut self.state.edit_buffer,
                &mut self.state.edit_cursor,
                text,
            );
        }
    }

    /// 一次按键。`Some` = 这道题有结果了：答完、Ctrl+C、连按两次 Esc。
    pub fn on_key(&mut self, key: KeyEvent) -> Result<Option<QuestionResponse>> {
        let (request, state) = (&self.request, &mut self.state);
        if key.kind != KeyEventKind::Press {
            return Ok(None);
        }
        if matches!(key.code, KeyCode::Char('c')) && key.modifiers.contains(KeyModifiers::CONTROL) {
            return Ok(Some(QuestionResponse::Cancelled));
        }
        if state.editing {
            if handle_editing_key(request, state, key)? && !request.needs_review() {
                if let Some(answers) = submitted_answers(request, state)? {
                    return Ok(Some(QuestionResponse::Answered(answers)));
                }
            }
            return Ok(None);
        }
        if key.code == KeyCode::Esc {
            if state
                .cancel_armed_until
                .is_some_and(|deadline| Instant::now() < deadline)
            {
                return Ok(Some(QuestionResponse::Cancelled));
            }
            state.cancel_armed_until = Some(Instant::now() + CANCEL_CONFIRM_WINDOW);
            return Ok(None);
        }
        state.cancel_armed_until = None;

        if state.on_confirm(request) {
            match key.code {
                KeyCode::Left | KeyCode::Char('h') => state.previous_tab(request),
                KeyCode::Right | KeyCode::Char('l') => state.next_tab(request),
                KeyCode::Enter => {
                    if let Some(answers) = submitted_answers(request, state)? {
                        return Ok(Some(QuestionResponse::Answered(answers)));
                    }
                    state.go_to_first_unanswered(request);
                }
                _ => {}
            }
            return Ok(None);
        }

        let question = &request.questions[state.tab];
        match key.code {
            KeyCode::Left | KeyCode::Char('h') => state.previous_tab(request),
            KeyCode::Right | KeyCode::Char('l') => state.next_tab(request),
            KeyCode::Up | KeyCode::Char('k') => state.previous_option(question),
            KeyCode::Down | KeyCode::Char('j') => state.next_option(question),
            KeyCode::Tab | KeyCode::Char(' ') if question.multiple => {
                state.toggle_current(request)?;
            }
            KeyCode::Enter if question.multiple => {
                state.activate_current(request)?;
            }
            KeyCode::Enter => {
                state.activate_current(request)?;
                if !request.needs_review() {
                    if let Some(answers) = submitted_answers(request, state)? {
                        return Ok(Some(QuestionResponse::Answered(answers)));
                    }
                }
            }
            _ => {}
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use miyu_base::question::QuestionOption;

    fn request(custom: bool) -> QuestionRequest {
        QuestionRequest {
            questions: vec![QuestionPrompt {
                header: "范围".to_string(),
                question: "改哪一块？".to_string(),
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
                multiple: false,
                custom,
            }],
        }
    }

    fn press(panel: &mut QuestionPanel, code: KeyCode) -> Option<QuestionResponse> {
        panel
            .on_key(KeyEvent::new(code, KeyModifiers::NONE))
            .unwrap()
    }

    /// 自己跑的面板和开在活动区上的面板认同一套键：↓ 挪、回车答。
    #[test]
    fn down_and_enter_answer_a_single_question() {
        let mut panel = QuestionPanel::new(request(false)).unwrap();
        assert_eq!(press(&mut panel, KeyCode::Down), None);
        match press(&mut panel, KeyCode::Enter) {
            Some(QuestionResponse::Answered(answers)) => assert_eq!(answers, vec![vec!["文档"]]),
            other => panic!("{other:?}"),
        }
    }

    /// Esc 要连按两次才取消（手滑一下不该掐掉这一轮）；Ctrl+C 一下就取消。
    #[test]
    fn esc_twice_or_ctrl_c_cancels() {
        let mut panel = QuestionPanel::new(request(false)).unwrap();
        assert_eq!(press(&mut panel, KeyCode::Esc), None);
        assert!(matches!(
            press(&mut panel, KeyCode::Esc),
            Some(QuestionResponse::Cancelled)
        ));
        let mut panel = QuestionPanel::new(request(false)).unwrap();
        assert!(matches!(
            panel.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Ok(Some(QuestionResponse::Cancelled))
        ));
    }

    /// 自定义答案：选到那一行回车进编辑，粘贴进输入框，光标露在那一行上，回车交卷。
    #[test]
    fn a_custom_answer_is_typed_pasted_and_submitted() {
        let mut panel = QuestionPanel::new(request(true)).unwrap();
        press(&mut panel, KeyCode::Down);
        press(&mut panel, KeyCode::Down);
        assert_eq!(press(&mut panel, KeyCode::Enter), None);
        assert!(panel.editing());
        panel.on_paste("都改");
        let view = panel.view(60, 12);
        let (row, _) = view.cursor.expect("在打字，光标得露出来");
        assert!(view.lines[row].contains("都改"), "{:?}", view.lines);
        match press(&mut panel, KeyCode::Enter) {
            Some(QuestionResponse::Answered(answers)) => assert_eq!(answers, vec![vec!["都改"]]),
            other => panic!("{other:?}"),
        }
    }

    /// 面板高度按内容定：短问题不占满上限。
    #[test]
    fn a_short_question_needs_few_rows() {
        let panel = QuestionPanel::new(request(false)).unwrap();
        let rows = panel.rows_needed(60);
        assert!(
            rows > 3 && rows < usize::from(QuestionPanel::max_rows()),
            "{rows}"
        );
        let mut panel = panel;
        assert!(panel.view(60, rows).lines.len() <= rows);
    }
}
