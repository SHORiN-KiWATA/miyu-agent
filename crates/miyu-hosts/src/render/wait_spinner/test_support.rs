//! 测试夹具:只在 cfg(test) 编译,生产二进制零字节。从 `src/render/wait_spinner.rs` 搬来(09-16 夹具搬家)。
#![allow(dead_code)]
use super::*;

pub(crate) fn render_frame(frame: usize, state: &WaitSpinner) -> (String, u16) {
    let width = crate::render::terminal_cols(120);
    render_frame_at_width(frame, state, width)
}

impl WaitSpinner {
    pub(crate) fn tick_changes_layout_at_width(&self, terminal_width: usize) -> bool {
        let (output, _) = render_frame_at_width(self.frame, self, terminal_width);
        let next_widths = output
            .lines()
            .map(super::super::command_ansi_width)
            .collect::<Vec<_>>();
        super::super::rendered_physical_rows(&self.area.widths, terminal_width)
            != super::super::rendered_physical_rows(&next_widths, terminal_width)
    }
}
