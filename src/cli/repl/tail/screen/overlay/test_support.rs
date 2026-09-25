//! 测试夹具:只在 cfg(test) 编译,生产二进制零字节。从 `src/cli/repl/tail/screen/overlay.rs` 搬来(09-16 夹具搬家)。
#![allow(dead_code)]
use super::*;

impl Screen {
    /// 开合面板里第 `index` 行挂着的那一块（测试用）。
    ///
    /// `overlay_click` 要的是屏幕行，而屏幕行要等面板画过一次才算得准
    /// （高度只涨不缩，见 `Overlay::height`）；测试里没有那一次绘制。
    /// 这条走的是同一段命中与开合逻辑，只是省掉了几何换算。
    pub(in crate::cli) fn overlay_toggle(&mut self, index: usize) -> bool {
        let Some(panel) = &mut self.overlay else {
            return false;
        };
        let Some((id, _)) = layer_hit(&Layer::Body(&panel.body), &panel.expanded, index) else {
            return false;
        };
        super::super::expand::toggle_in(&mut panel.expanded, id, panel.cols)
    }

    /// 按新内容刷一遍面板（测试用）。产品里这一步在 `paint_overlay` 里做。
    pub(in crate::cli) fn overlay_refresh(&mut self) {
        if let Some(panel) = &mut self.overlay {
            panel.refresh();
        }
    }

    /// 面板里现在是哪几行，**带转义**（测试用）：颜色也要能断言。
    pub(in crate::cli) fn overlay_rows_ansi(&self) -> Vec<String> {
        let Some(panel) = &self.overlay else {
            return Vec::new();
        };
        (0..panel.len())
            .map(|index| spans_to_ansi(&panel.row(index)))
            .collect()
    }

    /// 面板里现在是哪几行（测试用）。
    pub(in crate::cli) fn overlay_rows(&self) -> Vec<String> {
        let Some(panel) = &self.overlay else {
            return Vec::new();
        };
        (0..panel.len())
            .map(|index| super::super::ansi::spans_text(&panel.row(index)))
            .collect()
    }
}

impl Screen {
    /// 在面板里拖一个选区（测试用）：`(内容行, 显示列)` 两头直接给，省掉屏幕行
    /// 到内容行的几何换算——那套换算要面板画过一次才算得准（高度只涨不缩）。
    pub(in crate::cli) fn overlay_select_span(
        &mut self,
        anchor: (usize, u16),
        cursor: (usize, u16),
    ) {
        if let Some(panel) = &mut self.overlay {
            panel.selection = Some(super::super::select::Selection {
                anchor,
                cursor,
                dragging: true,
            });
        }
    }

    /// 鼠标停在面板里第 `index` 行上（测试用）：同 `overlay_toggle`，省掉屏幕行
    /// 到内容行的几何换算。返回真表示悬浮目标变了。
    pub(in crate::cli) fn overlay_hover_index(&mut self, index: Option<usize>) -> bool {
        let Some(panel) = &mut self.overlay else {
            return false;
        };
        let next = index.and_then(|index| {
            super::super::expand::layer_hit(
                &super::super::expand::Layer::Body(&panel.body),
                &panel.expanded,
                index,
            )
            .map(|(id, _)| id)
        });
        if next == panel.hover {
            return false;
        }
        panel.hover = next;
        true
    }

    /// 面板里鼠标停在**哪一块**上（测试用）。
    ///
    /// 不叫 `overlay_hovered`:生产侧 09-22 加了个同名的 `-> bool`(「有没有提亮
    /// 着的一行」),两边同名同接收者、只有返回类型不同——文本上不冲突所以合得
    /// 干干净净,但 cfg(test) 一开两个都在,`cargo test` 直接编不过(E0592/E0034)。
    /// `cargo build` 是好的,所以合的时候看不出来。
    pub(in crate::cli) fn hovered_overlay_block(&self) -> Option<u64> {
        self.overlay.as_ref().and_then(|panel| panel.hover)
    }

    /// 松手之后待写进剪贴板的那段文字（测试用）。
    pub(in crate::cli) fn take_pending_copy(&mut self) -> Option<String> {
        self.pending_copy.take()
    }
}
