//! 行缓冲的两头：前面丢（超出上限）、前面补（全屏往前翻页）。
//!
//! 两件事是一对：行号全靠「在缓冲里第几行」记，块、轮标记、压缩标记、活动区
//! 锚点在两头增删时都得跟着挪。

use super::*;

impl Term {
    /// 丢掉最早的 `count` 行。历史留存上限用它。
    pub(in crate::cli) fn drop_front(&mut self, count: usize) {
        let from_archive = count.min(self.archive.len());
        self.archive.drain(..from_archive);
        let rest = count - from_archive;
        if rest > 0 {
            let rest = rest.min(self.lines.len());
            self.lines.drain(..rest);
            self.wraps.drain(..rest.min(self.wraps.len()));
            let rest = rest.min(self.stamps.len());
            self.stamps.drain(..rest);
            self.row = self.row.saturating_sub(rest);
        }
        // 整块都被丢掉的就不留了，点不开也没内容可给。
        self.blocks.retain(|block| block.start >= count);
        for block in &mut self.blocks {
            block.start -= count;
            block.end -= count;
        }
        self.turn_starts.retain(|start| *start >= count);
        for start in &mut self.turn_starts {
            *start -= count;
        }
        self.compact_starts.retain(|start| *start >= count);
        for start in &mut self.compact_starts {
            *start -= count;
        }
        self.live_anchor = self.live_anchor.map(|anchor| anchor.saturating_sub(count));
    }

    /// 把另一个缓冲（按同样宽度渲染好的、更早的内容）整个接到最前面，返回接进来
    /// 几行。
    ///
    /// 和 `drop_front` 对称：
    /// - 接进来的行直接进存档：它们在最上面，再也不会被光标改写；
    /// - 本缓冲的块、轮标记、压缩标记、活动区锚点一律往后挪；
    /// - 接进来那一段自己的块和标记排在最前面。
    ///
    /// 会话项目第 2 段：全屏往上翻到顶时，往前再补一页。
    pub(in crate::cli) fn prepend(&mut self, earlier: Term) -> usize {
        let mut count = earlier.line_count();
        // 末尾那行是光标停着的地方：空的就不算内容，不然每接一页都多出一个空行。
        if count > 0
            && earlier
                .row_spans(count - 1)
                .iter()
                .all(|span| span.text.trim().is_empty())
        {
            count -= 1;
        }
        if count == 0 {
            return 0;
        }
        let rows = (0..count)
            .map(|index| ArchivedRow {
                spans: earlier.row_spans(index),
                wrapped: earlier.row_wrapped(index),
            })
            .collect::<Vec<_>>();
        self.archive.splice(0..0, rows);
        for block in &mut self.blocks {
            block.start += count;
            block.end += count;
        }
        for start in self
            .turn_starts
            .iter_mut()
            .chain(self.compact_starts.iter_mut())
        {
            *start += count;
        }
        self.live_anchor = self.live_anchor.map(|anchor| anchor + count);
        let blocks = earlier
            .blocks
            .into_iter()
            .filter(|block| block.end <= count)
            .collect::<Vec<_>>();
        self.blocks.splice(0..0, blocks);
        let turns = earlier
            .turn_starts
            .into_iter()
            .filter(|start| *start < count)
            .collect::<Vec<_>>();
        self.turn_starts.splice(0..0, turns);
        let compacts = earlier
            .compact_starts
            .into_iter()
            .filter(|start| *start < count)
            .collect::<Vec<_>>();
        self.compact_starts.splice(0..0, compacts);
        count
    }
}
