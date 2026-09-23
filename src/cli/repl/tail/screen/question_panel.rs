//! Questions reserve a viewport. Inspectable tool overlays keep their cover semantics.

use super::Screen;
use crossterm::cursor::MoveTo;
use crossterm::queue;
use crossterm::terminal::{Clear, ClearType};
use std::io::Write;

impl Screen {
    /// The input pump is suspended while asking, so resize and scrolling are owned here.
    pub(in crate::cli) fn scroll_question_body(
        &mut self,
        delta: isize,
        panel_rows: u16,
    ) -> anyhow::Result<()> {
        if let Ok((cols, rows)) = crossterm::terminal::size() {
            self.resize(cols, rows);
        }
        let top = self.layout_question_body(delta, panel_rows);
        let mut stdout = crate::cli::repl::tail::term_out();
        self.paint_body_above(&mut stdout, top, self.rows)?;
        // The separator belongs to neither the transcript nor the panel. Clear
        // its old contents after scrolling, changing questions, or resizing.
        if top < self.rows.saturating_sub(panel_rows) {
            queue!(stdout, MoveTo(0, top), Clear(ClearType::CurrentLine))?;
        }
        stdout.flush()?;
        Ok(())
    }

    fn layout_question_body(&mut self, delta: isize, panel_rows: u16) -> u16 {
        let top = self.rows.saturating_sub(panel_rows).saturating_sub(1);
        self.body = Some(top);
        if self.follow {
            self.scroll = self.follow_target();
        }
        self.scroll_by(delta);
        top
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conversation() -> Screen {
        let mut screen = Screen::detached(80, 32);
        for index in 1..=40 {
            screen.feed_for_test(format!("BODY-{index:02}\r\n").as_bytes());
        }
        screen
    }

    #[test]
    fn question_body_keeps_last_reply_reachable_while_panel_is_open() {
        let mut screen = conversation();
        let top = screen.layout_question_body(0, 12);
        assert_eq!(top, 19);
        assert_eq!(screen.body(), top);
        assert_eq!(screen.scroll_of() + usize::from(top), screen.content_rows());
        screen.layout_question_body(-10, 12);
        assert!(!screen.follow);
        screen.layout_question_body(100, 12);
        assert_eq!(screen.scroll_of() + usize::from(top), screen.content_rows());
        assert!(screen.follow);
    }

    #[test]
    fn question_height_changes_and_terminal_resize_preserve_following() {
        let mut screen = conversation();
        for (rows, panel_rows) in [(32, 12), (32, 7), (20, 17), (40, 10)] {
            screen.resize(80, rows);
            let top = screen.layout_question_body(0, panel_rows);
            assert_eq!(top, rows - panel_rows - 1);
            assert_eq!(screen.scroll_of() + usize::from(top), screen.content_rows());
            assert!(screen.follow);
        }
        // The first regular frame restores the input/footer viewport on exit.
        // Suspended paint exercises the layout without writing to the real terminal.
        screen.suspended = true;
        screen.paint(5).unwrap();
        assert_eq!(screen.body(), 35);
        assert_eq!(screen.scroll_of() + 35, screen.content_rows());
    }

    #[test]
    fn question_relayout_preserves_a_scrolled_reading_position() {
        let mut screen = conversation();
        screen.layout_question_body(-10, 12);
        let scroll = screen.scroll_of();
        screen.layout_question_body(0, 17);
        assert_eq!(screen.scroll_of(), scroll);
        assert!(!screen.follow);
    }
}
