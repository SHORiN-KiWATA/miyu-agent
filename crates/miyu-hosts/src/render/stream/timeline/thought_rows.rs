//! 正在想的那段正文折好的行：按宽度缓存，只往后补。
//!
//! 思考正文只会往后长，原来每一拍都拿**整段**重折——全屏下转轮一秒三十拍，滚动窗
//! 取末尾十行折一遍、「思考中」那一块点开用的内容整份重灌又折一遍；终端集成那条
//! 静态路每来一截还在循环里反复折。想到三万词元时一拍要量几万个字的宽度，TUI 吃满
//! 一个核、敲一个字要等好几秒（用户 09-24 截图「思考中 · 30206 词元」）。
//!
//! 折行是按逻辑行（换行切开）各折各的：以换行结尾的行以后再也不会变，折一次记住。
//! 最后那半行还在写，但它里面也只有末尾两个物理行会变（还在写的那一行可能把上一行
//! 的尾词拽下来），前面的同样记住，每次只从末尾两行的起点接着折——不然一大段不换行
//! 的思考照样越长越卡。宽度变了、正文清空了就从头来。

use super::*;

#[derive(Default)]
pub(crate) struct ThoughtRows {
    width: usize,
    /// 已经折过的字节数：到最后一个换行为止。
    done: usize,
    /// 以换行结尾的那些行折出来的物理行（不带样式）。
    rows: Vec<String>,
    /// 最后那半行里已经定了的物理行（除末尾两行以外的）。
    settled: Vec<String>,
    /// 最后那半行从哪儿接着折（相对 `done` 的字节数）：末尾两行的起点。
    resume: usize,
    /// 末尾那两行。每次 `sync` 现折。
    tail: Vec<String>,
}

impl ThoughtRows {
    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }

    /// 跟上正文 `text`：新写完的行折进缓存，最后那半行只重折末尾两行。
    pub(crate) fn sync(&mut self, text: &str, width: usize) {
        // 正文变短（清空后又开始了新的一段）、宽度变了：从头来。
        if width != self.width || text.len() < self.done || !text.is_char_boundary(self.done) {
            *self = Self {
                width,
                ..Self::default()
            };
        }
        let cut = text.rfind('\n').map_or(0, |index| index + 1);
        if cut > self.done {
            self.rows.extend(wrap_lines(&text[self.done..cut], width));
            self.done = cut;
            self.settled.clear();
            self.resume = 0;
        }
        let partial = &text[self.done..];
        // 和 `wrap_lines` 同一个口径：没有字就没有行，全是空白就是一行空的。
        if partial.trim().is_empty() {
            self.settled.clear();
            self.resume = 0;
            self.tail = if partial.is_empty() {
                Vec::new()
            } else {
                vec![String::new()]
            };
            return;
        }
        let mut rows = crate::render::wrap_display_rows(&partial[self.resume..], width);
        if rows.len() > 2 {
            let keep = rows.len() - 2;
            self.resume += rows[keep].1;
            self.settled.extend(rows.drain(..keep).map(|(row, _)| row));
        }
        self.tail = rows.into_iter().map(|(row, _)| row).collect();
    }

    pub(crate) fn len(&self) -> usize {
        self.rows.len() + self.settled.len() + self.tail.len()
    }

    pub(crate) fn range(&self, from: usize, count: usize) -> Vec<String> {
        self.rows
            .iter()
            .chain(self.settled.iter())
            .chain(self.tail.iter())
            .skip(from)
            .take(count)
            .cloned()
            .collect()
    }
}

/// 同 `wrap_detail`，宽度由调用方给：空行留一行空的。
fn wrap_lines(text: &str, width: usize) -> Vec<String> {
    text.lines()
        .flat_map(|line| {
            if line.trim().is_empty() {
                return vec![String::new()];
            }
            crate::render::wrap_display_text(line, width)
        })
        .collect()
}

impl StreamRenderer {
    fn synced_thought_rows(&self) -> std::cell::RefMut<'_, ThoughtRows> {
        let mut rows = self.thought_rows.borrow_mut();
        rows.sync(&self.reasoning_text, detail_width());
        rows
    }

    /// 正在想的正文折好的物理行一共几行。
    pub(crate) fn thought_row_count(&self) -> usize {
        self.synced_thought_rows().len()
    }

    /// 从第 `from` 行起的 `count` 行（不带样式）。
    pub(crate) fn thought_rows_range(&self, from: usize, count: usize) -> Vec<String> {
        self.synced_thought_rows().range(from, count)
    }

    /// 全部物理行（不带样式）。只在一段思考收尾、或者点开用的内容要全文时用。
    pub(crate) fn thought_rows_all(&self) -> Vec<String> {
        self.thought_rows_range(0, usize::MAX)
    }

    /// 末尾 `count` 行（不带样式）。滚动窗用。
    pub(crate) fn thought_rows_last(&self, count: usize) -> Vec<String> {
        let rows = self.synced_thought_rows();
        let from = rows.len().saturating_sub(count);
        rows.range(from, count)
    }
}

/// 给思考正文的物理行上色：滚动窗、想完落地、点开看到的都是这一个颜色。
pub(crate) fn style_thought_rows(rows: Vec<String>) -> Vec<String> {
    rows.into_iter()
        .map(|line| format!("{THOUGHT_BODY_STYLE}{line}\x1b[0m"))
        .collect()
}
