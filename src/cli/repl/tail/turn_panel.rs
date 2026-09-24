//! 回合里开着的面板画在活动区的位置上（会话项目第 3 段，B4）。
//!
//! 面板是活动区的另一种样子：输入框、footer、任务条这会儿都让给它，正文照常往上面画、照常
//! 跟着底走。面板本身（`/models`、`/session` 是哪个、收掉后做什么）见 `repl::midturn_panel`。

use super::*;
use crate::cli::repl::midturn_panel::{PanelDone, TurnPanel};
use crate::cli::repl::panel::{Panel, PanelFrame, PanelModel};

impl LiveReplTail {
    pub(in crate::cli) fn open_turn_panel(&mut self, panel: TurnPanel) -> Result<()> {
        // 回合跑着就不该还在大厅里；万一还挂着，面板画不出来却照样收按键。
        if self.banner.is_some() {
            self.leave_lobby();
        }
        self.turn_panel = Some(panel);
        self.leave_strip();
        self.repaint_turn_panel()
    }

    /// 收掉面板，活动区回到输入框。
    pub(in crate::cli) fn close_turn_panel(&mut self) -> Result<Option<TurnPanel>> {
        let panel = self.turn_panel.take();
        if panel.is_some() {
            self.repaint_turn_panel()?;
        }
        Ok(panel)
    }

    /// 这个事件归不归面板：面板开着时按键和粘贴归它，鼠标、窗口变化照旧归回合循环。
    pub(in crate::cli) fn turn_panel_takes(&self, event: &Event) -> bool {
        self.turn_panel.is_some() && matches!(event, Event::Key(_) | Event::Paste(_))
    }

    /// 面板上的一次按键。PageUp/PageDown 翻上面的正文（提问面板正在打字时除外）；粘贴给
    /// 提问面板的输入框。返回面板要收掉时交出的结果，`None` = 面板还开着（这一下的画面
    /// 已经重画）。
    pub(in crate::cli) fn turn_panel_event(&mut self, event: &Event) -> Result<Option<PanelDone>> {
        let Some(panel) = self.turn_panel.as_mut() else {
            return Ok(None);
        };
        if let Event::Paste(text) = event {
            panel.paste(text);
            self.repaint_turn_panel()?;
            return Ok(None);
        }
        let Event::Key(KeyEvent {
            code,
            modifiers,
            kind,
            ..
        }) = event
        else {
            return Ok(None);
        };
        if *kind == KeyEventKind::Release {
            return Ok(None);
        }
        if matches!(code, KeyCode::PageUp | KeyCode::PageDown) && panel.cursor().is_none() {
            let page = self.tail_start.max(1) as isize;
            let delta = if *code == KeyCode::PageUp {
                -page
            } else {
                page
            };
            if let Some(screen) = &mut self.screen {
                screen.scroll_by(delta);
            }
            self.repaint_turn_panel()?;
            return Ok(None);
        }
        let done = panel.on_key(*code, *modifiers);
        if done.is_none() {
            self.repaint_turn_panel()?;
        }
        Ok(done)
    }

    fn repaint_turn_panel(&mut self) -> Result<()> {
        if !self.rendered || self.external_output_active {
            return Ok(());
        }
        synchronized_terminal_update(CursorAfterUpdate::Preserve, || self.redraw())
    }

    /// `paint_tail` 在面板开着时走这里：正文让出面板那几行，面板画在输入框的位置上。上下各
    /// 空一行（正文和面板之间一行、面板底下一行），和改前阻塞画的面板一个样子
    /// （`panel::body_panel`）。光标只在提问面板打自定义答案时露出来。
    pub(in crate::cli::repl::tail) fn paint_turn_panel(
        &mut self,
        cols: u16,
        rows: u16,
        output_col: u16,
    ) -> Result<()> {
        let (Some(panel), Some(screen)) = (self.turn_panel.as_mut(), self.screen.as_mut()) else {
            return Ok(());
        };
        let height = panel.desired_rows().min(rows.saturating_sub(2).max(1));
        let tail_height = height.saturating_add(2);
        let previous = self
            .rendered
            .then_some((self.tail_start, self.tail_rows))
            .filter(|(_, rows)| *rows > 0);
        let next_body = screen.body_for(tail_height);
        screen.invalidate_activity_rows(previous, Some((next_body, height.saturating_add(1))));
        // 敲命令时浮在输入框上方的候选框：命令已经执行成面板了，别留在面板头上。
        screen.set_command_hint(Vec::new());
        let separator = screen.paint(tail_height)?;
        screen.set_input_rows(Vec::new());
        let top = separator.saturating_add(1);
        let bar = panel.bar(self.editor.mode);
        let geometry = Panel {
            left: 0,
            top,
            width: cols,
            rows: height,
        };
        let frame = PanelFrame {
            panel: geometry,
            width: usize::from(cols).saturating_sub(visible_width(&bar)),
            visible: usize::from(height.saturating_sub(2)),
        };
        let mut lines = panel.content(&frame);
        lines.resize(usize::from(height), String::new());
        crate::cli::repl::panel::paint_panel(&geometry, &bar, &lines)?;
        let below = top.saturating_add(height);
        let cursor = panel.cursor().map(|(row, column)| {
            (
                u16::try_from(column)
                    .unwrap_or(u16::MAX)
                    .min(cols.saturating_sub(1)),
                top.saturating_add(u16::try_from(row).unwrap_or(u16::MAX)),
            )
        });
        let mut stdout = term_out();
        // 分隔那一行不归正文也不归面板：翻页、换题、改窗口大小之后上面残留的字要擦掉。
        queue!(stdout, MoveTo(0, separator), Clear(ClearType::CurrentLine))?;
        queue!(stdout, MoveTo(0, below), Clear(ClearType::CurrentLine))?;
        match cursor {
            Some((column, row)) => queue!(stdout, MoveTo(column, row), crossterm::cursor::Show)?,
            None => queue!(stdout, crossterm::cursor::Hide)?,
        }
        stdout.flush()?;
        self.footer_offset = None;
        self.job_strip_start = 0;
        self.job_strip_rows = 0;
        self.input_cursor = cursor.unwrap_or((0, below));
        self.output_cursor = (output_col, separator.saturating_sub(1));
        self.tail_start = separator;
        self.tail_rows = height.saturating_add(1);
        self.rendered = true;
        Ok(())
    }
}
