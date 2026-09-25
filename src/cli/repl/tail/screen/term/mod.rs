//! 一个够用的终端模拟器：吃 ANSI 字节，维护带样式的行缓冲。
//!
//! 全屏后端拿它替掉「把字节写进 scrollback」这一步。为什么非要模拟而不是
//! 按 `\n` 切行追加：**帧里有光标控制**。spinner 每一帧是「上移 N 行 → 清行
//! → 重画」，命令块的实时输出也是原地刷新的。只追加的话 spinner 会每帧堆一行，
//! 几秒钟就把历史撑爆。
//!
//! 模拟了才能做到这次重做的前提——**输出方一行都不用改**。渲染器、spinner、
//! 命令块、图片照旧往外吐它们那套字节，这里照单全收。
//!
//! 只实现 REPL 真的会发的那些：SGR、换行、回车、上下左右、定列、清行、清屏。
//! 没实现的（滚动区、制表位、备用缓冲切换）在这条路上不会出现，遇到就忽略，
//! 最坏是掉一次样式而不是乱码。

use super::ansi::AnsiSpan;
use ratatui::style::{Color, Modifier, Style};
use unicode_width::UnicodeWidthChar;
use vte::{Params, Parser, Perform};

mod edges;
mod live;

/// 一格。宽字符占两格，第二格是 `continuation`，画的时候跳过。
#[derive(Clone, PartialEq)]
struct Cell {
    ch: char,
    /// 跟在 `ch` 后面的组合记号。
    ///
    /// 只有真带记号的格子才分配（正文里几乎没有），所以不必让每一格都背一个
    /// `String`。**kitty 的图片占位符全靠它**：每一格是
    /// `U+10EEEE + 行号记号 + 列号记号`，记号丢了终端就不知道这一格该放图的
    /// 哪一块，整张图都出不来。
    marks: Option<Box<str>>,
    /// 这一格属于哪个链接（OSC 8 的目标）。
    ///
    /// 存了它，全屏下才点得开链接：鼠标被程序捕获走之后终端自己那套"点链接"
    /// 就失效了，得自己认。`Arc` 是为了克隆便宜——一行几十格共享同一个目标。
    link: Option<std::sync::Arc<str>>,
    style: Style,
    continuation: bool,
}

impl Cell {
    fn blank() -> Self {
        Self {
            ch: ' ',
            marks: None,
            link: None,
            style: Style::new(),
            continuation: false,
        }
    }

    /// 这一格画出来是什么——基字符加上它带的组合记号。
    fn push_text(&self, out: &mut String) {
        out.push(self.ch);
        if let Some(marks) = &self.marks {
            out.push_str(marks);
        }
    }
}

/// 只有最近这么多行保持「可随机写入」的格子形态。
///
/// 命令块的原地刷新只动尾巴，更早的行永远不会再被改。把它们压成 span 存档能省
/// 一个数量级的内存：一格 `Cell` 要带一份完整 `Style`（约 32 字节），而一整行
/// 中文压成 span 通常只有一两段。转轮那一段（活动区）例外：一轮里工具一多它能
/// 高过这个数，而它每帧都要重写——锚点及以下不管多高都留在可写段里（见 [`live`]）。
const LIVE_ROWS: usize = 256;

/// 行缓冲 + 光标 + 当前样式。
///
/// 两段式：`archive` 是压好的只读历史，`live` 是还可能被光标回头改写的尾巴。
pub(in crate::cli) struct Term {
    archive: Vec<ArchivedRow>,
    lines: Vec<Vec<Cell>>,
    row: usize,
    col: usize,
    style: Style,
    /// OSC 8 的目标，跟着写入的格子走。
    link: Option<std::sync::Arc<str>>,
    /// 解析器的状态跨帧保留——一个转义序列被帧边界切断也拦得住。
    parser: Parser,
    /// 屏幕宽度。写到边上要自己折行——真终端的 DECAWM 就是这么干的，
    /// 不折的话长段正文会在画面右边被直接切掉。
    cols: usize,
    /// 每一行的版本号（和 `lines` 平行）。写一次涨一次。
    ///
    /// 画面每帧要把可见的三十几行全部重排一遍（取 span、上色、拼 ANSI、比对），
    /// 而流式输出时**真正变的只有最后一两行**。有了版本号就能一眼看出"这一行
    /// 还是上一帧那一行"，直接跳过——AI 输出时拖选发涩就是这几十行的重排在和
    /// 鼠标抢时间。
    stamps: Vec<u64>,
    /// 这一行是怎么断的（和 `lines` 平行，和 `stamps` 一样按需长）。只有软换行
    /// 连起来的行才能在改宽度时并回一条逻辑行重排；`\n` 断的行是作者自己断的，
    /// 重排就把版式毁了。
    wraps: Vec<bool>,
    /// 正文区有多宽（`Screen` 报进来）。渲染器折的正文按它重排，才和新写进来的
    /// 内容一样宽——缓冲自己折的是按屏幕物理宽度折的，那些按 `cols` 重排。
    content_cols: usize,
    clock: u64,
    /// 已经收好的可展开块，按起始行升序。
    blocks: Vec<BlockSpan>,
    /// 刚见到起始标记、还没等到第一个字符落下的块。起始行要等到真有字符
    /// 写出来才算——渲染器会先 `MoveUp` 擦掉旧的那几行再重画，标记发出来
    /// 那一刻光标还停在块的**下面**。
    /// 下一个字符落下时要开的块：id + 「默认开着吗」。
    pending_block: Option<(u64, bool)>,
    /// 刚收到软换行标记：紧接着的那个 `\n` 是折出来的，不是作者断的。
    pending_soft_wrap: bool,
    /// 每一轮从第几行开始（提交回显、回放里每轮开头埋的 `TURN_START_MARKER`），
    /// 升序。`/undo` 把缓冲截回最后一个标记处，见 [`Term::truncate_rows`]。
    turn_starts: Vec<usize>,
    /// 各块压缩结果的起始行(`COMPACT_START_MARKER`),撤压缩时按它截。
    compact_starts: Vec<usize>,
    /// 活动区（转轮那一段）从第几行开始。见 [`live`]：每一帧回到这儿整段重写，
    /// 这一行及以下永远留在可写段里。`None` = 眼下没有活动区。
    live_anchor: Option<usize>,
    /// 这一次 `feed` 里回到锚点之前活动区的样子，吃完之后拿来比对、还回没变的行的版本号。
    live_stash: Option<live::Stash>,
}

/// 压进存档的一行：压好的 span + 「它是折下来的吗」。
///
/// 存档段不再被光标改写，但改宽度时还要参与重排，所以那个标志得跟着一起存。
struct ArchivedRow {
    spans: Vec<AnsiSpan>,
    wrapped: bool,
}

/// kitty 图片的 Unicode 占位符。带它的行整行不动：每一格是
/// `U+10EEEE + 行号记号 + 列号记号`，按新宽度重排等于把图撕了。
const IMAGE_PLACEHOLDER: char = '\u{10eeee}';

/// 一块可展开内容在缓冲里占的行。
#[derive(Clone, Copy, Debug)]
pub(in crate::cli) struct BlockSpan {
    pub(in crate::cli) id: u64,
    pub(in crate::cli) start: usize,
    pub(in crate::cli) end: usize,
    /// 视图第一次见到这一块时先替用户开一次（`显示思考过程 / 显示工具调用信息
    /// = 完整` 那一档）。见 `render::blocks::begin_marker_open`。
    pub(in crate::cli) open: bool,
}

impl Default for Term {
    fn default() -> Self {
        Self {
            archive: Vec::new(),
            lines: vec![Vec::new()],
            stamps: vec![0],
            wraps: Vec::new(),
            content_cols: 80,
            clock: 0,
            row: 0,
            col: 0,
            style: Style::new(),
            link: None,
            parser: Parser::new(),
            blocks: Vec::new(),
            pending_block: None,
            pending_soft_wrap: false,
            turn_starts: Vec::new(),
            compact_starts: Vec::new(),
            live_anchor: None,
            live_stash: None,
            cols: 80,
        }
    }
}

impl Term {
    /// 吃一帧字节。
    pub(in crate::cli) fn feed(&mut self, bytes: &[u8]) {
        // `Parser` 与 `Perform` 不能同时借 self，先把解析器换出来。
        let mut parser = std::mem::take(&mut self.parser);
        parser.advance(self, bytes);
        self.parser = parser;
        self.settle_live_rows();
    }

    /// 最后一行**有内容**的行号 + 1。
    ///
    /// 和 `content_rows` 的区别是它不管光标在哪儿。渲染器常在正文末尾多打两个
    /// 换行，光标于是停在两行空白之下，`content_rows` 为了给"下一段输出"留位置
    /// 会把那两行算进去——贴底排版照它算的话，那两行就变成正文和输入框之间的
    /// 空档。
    pub(in crate::cli) fn filled_rows(&self) -> usize {
        let mut last = self.lines.len();
        while last > 0 {
            let index = self.archive.len() + last - 1;
            if self
                .row_spans(index)
                .iter()
                .any(|span| !span.text.trim().is_empty())
            {
                break;
            }
            last -= 1;
        }
        self.archive.len() + last
    }

    /// 这一行现在是第几版。存档段是只读的，给 0。
    pub(in crate::cli) fn row_stamp(&self, index: usize) -> u64 {
        if index < self.archive.len() {
            return 0;
        }
        self.stamps
            .get(index - self.archive.len())
            .copied()
            .unwrap_or(0)
    }

    /// 记一笔：这一行动过了。
    fn touch(&mut self, row: usize) {
        self.clock = self.clock.wrapping_add(1);
        while self.stamps.len() <= row {
            self.stamps.push(0);
        }
        self.stamps[row] = self.clock;
    }

    pub(in crate::cli) fn line_count(&self) -> usize {
        self.archive.len() + self.lines.len()
    }

    /// 第 `index` 行的 span。
    pub(in crate::cli) fn row_spans(&self, index: usize) -> Vec<AnsiSpan> {
        if index < self.archive.len() {
            return self.archive[index].spans.clone();
        }
        let Some(line) = self.lines.get(index - self.archive.len()) else {
            return Vec::new();
        };
        Self::compress(line)
    }

    /// 相邻同样式的格子合并成一段，画出来才不会每个字一段 SGR。
    fn compress(line: &[Cell]) -> Vec<AnsiSpan> {
        let mut spans: Vec<AnsiSpan> = Vec::new();
        for cell in line {
            if cell.continuation {
                continue;
            }
            let link = cell.link.as_ref().map(|link| link.to_string());
            match spans.last_mut() {
                Some(last) if last.style == cell.style && last.link == link => {
                    cell.push_text(&mut last.text);
                }
                _ => {
                    let mut text = String::new();
                    cell.push_text(&mut text);
                    spans.push(AnsiSpan {
                        text,
                        style: cell.style,
                        link,
                    });
                }
            }
        }
        // 行尾的空白没有意义，去掉能让选区和「这行是不是空的」判断都干净。
        while let Some(last) = spans.last_mut() {
            let trimmed = last.text.trim_end();
            if trimmed.is_empty() {
                spans.pop();
            } else {
                last.text.truncate(trimmed.len());
                break;
            }
        }
        spans
    }

    /// 光标当前在第几行——活动区要接在正文后面画。
    pub(in crate::cli) fn cursor_row(&self) -> usize {
        self.archive.len() + self.row
    }

    /// 最后一轮从第几行开始（并把这个标记拿掉）。没有标记就 `None`。
    pub(in crate::cli) fn pop_turn_start(&mut self) -> Option<usize> {
        self.turn_starts.pop()
    }

    /// 最后一块压缩结果从第几行开始(并把标记拿掉)——只在它是缓冲里最后一样东西
    /// 时给:它后面又有一轮的话,撤掉的就不是它(那轮的标记在它后面),返回 `None`。
    pub(in crate::cli) fn pop_trailing_compact_start(&mut self) -> Option<usize> {
        let start = *self.compact_starts.last()?;
        if self.turn_starts.last().is_some_and(|turn| *turn > start) {
            return None;
        }
        self.compact_starts.pop()
    }

    /// 各轮的起始行（测试用）。
    #[cfg(test)]
    pub(in crate::cli) fn turn_starts(&self) -> &[usize] {
        &self.turn_starts
    }

    /// 只留前 `keep` 行，光标停在第 `keep` 行行首（一行空的），之后的输出接着写。
    /// `/undo` 用：撤掉的那一轮从缓冲里截掉，前面的原样留着。
    pub(in crate::cli) fn truncate_rows(&mut self, keep: usize) {
        if keep >= self.line_count() {
            return;
        }
        if keep <= self.archive.len() {
            self.archive.truncate(keep);
            self.lines = vec![Vec::new()];
            self.stamps.clear();
            self.wraps.clear();
            self.row = 0;
        } else {
            let live = keep - self.archive.len();
            self.lines.truncate(live);
            self.stamps.truncate(live);
            self.wraps.truncate(live.min(self.wraps.len()));
            self.lines.push(Vec::new());
            self.row = live;
        }
        self.col = 0;
        self.touch(self.row);
        // 截掉的行里的块跟着没了；正开着的块也不作数。
        self.blocks.retain(|block| block.end <= keep);
        self.pending_block = None;
        self.turn_starts.retain(|start| *start < keep);
        self.compact_starts.retain(|start| *start < keep);
        // 活动区整个被截掉了（`/undo` 之类）：锚也不作数。
        if self.live_anchor.is_some_and(|anchor| anchor > keep) {
            self.live_anchor = None;
        }
    }

    /// 收尾一块：结束标记发出来时光标停在最后一行上，那一行算在块里。
    fn close_block(&mut self) {
        self.pending_block = None;
        // 光标停在**行首**说明上一行刚写完换了行，这一行还没有内容——它不属于
        // 这一块。多算一行的后果很具体：紧跟在收缩行后面的那句话（「已取消」
        // 之类）会被当成块的一部分，能点、一点还把块收起来。
        let end = if self.col == 0 {
            self.cursor_row()
        } else {
            self.cursor_row().saturating_add(1)
        };
        if let Some(block) = self.blocks.last_mut() {
            // 原来只在「跨度还是空的」时候才写。自从开始标记会保留旧跨度
            //（见 `print`）,那条件就再也不成立了,块会一直停在上一帧的高度——
            // 内容长了也不跟着长。结束标记本来就只跟在自己那一块后面,直接按
            // 光标此刻的位置收口。
            if block.start < end {
                block.end = end.max(block.start.saturating_add(1));
            }
        }
    }

    /// 画面宽度变了：已经落下的行**按新宽度重排一遍**。
    ///
    /// 09-18 之前这里只改 `self.cols`、落下的行一律不动，于是拉宽窗口时正文还
    /// 按旧宽度断着、右边整片空着；收窄时超出新宽度的那一截被画面直接切掉
    /// （用户实测两样都撞上了）。
    ///
    /// 只并**软换行**（写到边上自己折的那一下）：`\n` 断的行是作者自己断的，
    /// 并起来重排等于把版式毁了。带 kitty 图片占位符的行整行不动，理由见
    /// [`IMAGE_PLACEHOLDER`]。
    pub(in crate::cli) fn set_cols(&mut self, cols: usize) {
        let cols = cols.max(1);
        if cols == self.cols {
            return;
        }
        self.cols = cols;
        self.reflow();
    }

    /// 这一行是折下来的（下一行是它的续行）。
    fn row_wrapped(&self, index: usize) -> bool {
        if index < self.archive.len() {
            return self.archive[index].wrapped;
        }
        self.wraps
            .get(index - self.archive.len())
            .copied()
            .unwrap_or(false)
    }

    /// 活动段第 `row` 行的折行标志。
    fn set_wrapped(&mut self, row: usize, wrapped: bool) {
        while self.wraps.len() <= row {
            self.wraps.push(false);
        }
        self.wraps[row] = wrapped;
    }

    /// 正文区宽度（`Screen` 每帧报一次）。渲染器折的正文按它重排。
    pub(in crate::cli) fn set_content_cols(&mut self, cols: usize) {
        self.content_cols = cols.max(1);
    }

    /// 整行不参与重排（kitty 图片占位符）。
    fn row_is_atomic(&self, index: usize) -> bool {
        self.row_spans(index)
            .iter()
            .any(|span| span.text.contains(IMAGE_PLACEHOLDER))
    }

    /// 按当前 `cols` 把所有行重排一遍。
    fn reflow(&mut self) {
        let total = self.line_count();
        if total == 0 {
            return;
        }
        // 光标落在逻辑行的第几**显示列**上——重排完要把它放回同一个字上，
        // 否则渲染器接着发的「上移 N 行重画」会打偏。
        let cursor_row = self.cursor_row();
        let cursor_offset = self.cursor_logical_offset(cursor_row);

        // 1. 软换行连起来的行并成一条逻辑行：`[起始旧行, 结束旧行)` + 内容 +
        //    续行的悬挂缩进（正文的续行自带装订边那两格，并的时候要摘掉、
        //    重排的时候再补回去，否则每折一次就多缩进两格）。
        let mut logical: Vec<(std::ops::Range<usize>, Vec<AnsiSpan>, usize, usize, bool)> =
            Vec::new();
        let mut index = 0;
        while index < total {
            let start = index;
            if self.row_is_atomic(index) {
                logical.push((start..index + 1, self.row_spans(index), 0, self.cols, true));
                index += 1;
                continue;
            }
            let mut spans = self.row_spans(index);
            let indent = Self::leading_spaces(&spans);
            while self.row_wrapped(index) && index + 1 < total && !self.row_is_atomic(index + 1) {
                index += 1;
                let mut next = self.row_spans(index);
                Self::strip_leading(&mut next, indent);
                spans.extend(next);
            }
            // 悬挂缩进按**首行的缩进**算,而不是「这条逻辑行现在占了几行」:
            // 一旦在宽窗口里并成一行,后者就永远是 0,再收窄回去装订边就没了。
            // 缩进宽到没地方放字时（几乎不可能）退回不缩进，免得断不动。
            let indent = if indent + 1 >= self.cols { 0 } else { indent };
            // 带装订边的是正文（渲染器按正文区宽度折的），重排也得按那个宽度，
            // 才和resize 之后新写进来的内容一样宽；没有装订边的是缓冲自己按屏幕
            // 边折的，按屏幕宽度重排。判据用缩进而不用「当初是谁折的」：一条
            // 逻辑行在宽窗口里并成一行之后，后者就丢了，再收窄回去宽度会不一致。
            // 活动区（锚点及以下）的行是转轮按屏宽裁好的，按屏宽折：按正文区宽度
            // 折的话，刚好贴边的行会被拆成两行，转轮下一帧按行数回来就对不上了。
            let live = self.live_anchor.is_some_and(|anchor| start >= anchor);
            let budget = if indent > 0 && !live {
                (indent + self.content_cols).min(self.cols)
            } else {
                self.cols
            };
            logical.push((start..index + 1, spans, indent, budget, false));
            index += 1;
        }

        // 2. 按新宽度重新断行，同时记「旧行 → 新行」。一条逻辑行里的旧行全部
        //    指到它重排后的第一行：块的起止、轮标记只要落在同一段内容上就够。
        let mut rows: Vec<(Vec<AnsiSpan>, bool)> = Vec::new();
        let mut old_to_new = vec![0usize; total + 1];
        let mut cursor_at = None;
        for (range, spans, indent, budget, atomic) in logical {
            let logical_start = rows.len();
            for old in range.clone() {
                old_to_new[old] = logical_start;
            }
            if atomic {
                rows.push((spans, false));
            } else {
                let split = Self::split_spans(&spans, budget, indent);
                let last = split.len().saturating_sub(1);
                for (offset, row) in split.into_iter().enumerate() {
                    rows.push((row, offset < last));
                }
            }
            if let Some(offset) = cursor_offset.filter(|_| range.contains(&cursor_row)) {
                cursor_at = Self::locate(&rows[logical_start..], offset, budget, indent)
                    .map(|(row, col)| (logical_start + row, col));
            }
        }
        old_to_new[total] = rows.len();
        let anchor = self
            .live_anchor
            .map(|anchor| old_to_new.get(anchor).copied().unwrap_or(rows.len()));

        // 3. 重建两段缓冲：最近 LIVE_ROWS 行留成可写的格子，其余压进存档。
        //    活动区（锚点及以下）不管多高都得留在可写段里。
        let live_from = rows
            .len()
            .saturating_sub(LIVE_ROWS.max(1))
            .min(anchor.unwrap_or(usize::MAX));
        let (archived, live) = rows.split_at(live_from);
        self.archive = archived
            .iter()
            .map(|(spans, wrapped)| ArchivedRow {
                spans: spans.clone(),
                wrapped: *wrapped,
            })
            .collect();
        self.lines = live
            .iter()
            .map(|(spans, _)| Self::to_cells(spans))
            .collect();
        self.wraps = live.iter().map(|(_, wrapped)| *wrapped).collect();
        if self.lines.is_empty() {
            self.lines.push(Vec::new());
            self.wraps.push(false);
        }
        // 每一行都变了，画面得整片重画（`Screen::resize` 也会整屏擦一次）。
        self.stamps = Vec::new();
        for row in 0..self.lines.len() {
            self.touch(row);
        }

        // 4. 行号全变了：块、轮标记、压缩标记、光标跟着搬。
        let remap = |index: &usize| old_to_new.get(*index).copied().unwrap_or(rows.len());
        for block in &mut self.blocks {
            block.start = remap(&block.start);
            block.end = remap(&block.end).max(block.start);
        }
        for start in &mut self.turn_starts {
            *start = remap(start);
        }
        for start in &mut self.compact_starts {
            *start = remap(start);
        }
        self.live_anchor = anchor;
        self.live_stash = None;
        let (row, col) = cursor_at.unwrap_or((rows.len().saturating_sub(1), 0));
        self.row = row.saturating_sub(self.archive.len());
        self.col = col;
    }

    /// 光标那一格在它所属逻辑行里的第几显示列（`None` = 缓冲里没这一行）。
    fn cursor_logical_offset(&self, cursor_row: usize) -> Option<usize> {
        if cursor_row >= self.line_count() {
            return None;
        }
        let mut start = cursor_row;
        while start > 0 && self.row_wrapped(start - 1) && !self.row_is_atomic(start) {
            start -= 1;
        }
        let mut offset = 0;
        for index in start..cursor_row {
            offset += Self::spans_width(&self.row_spans(index));
        }
        Some(offset + self.col)
    }

    /// 重排后的若干行里，第 `offset` 显示列（按摘掉悬挂缩进的逻辑坐标算）落在
    /// 第几行第几列。
    fn locate(
        rows: &[(Vec<AnsiSpan>, bool)],
        offset: usize,
        cols: usize,
        indent: usize,
    ) -> Option<(usize, usize)> {
        let mut seen = 0;
        for (index, (spans, _)) in rows.iter().enumerate() {
            let hang = if index == 0 { 0 } else { indent };
            // 满行按整宽算：行尾被宽字符挤掉的那一列也算在里头，不然偏一格。
            let width = if index + 1 < rows.len() {
                cols.saturating_sub(hang)
            } else {
                Self::spans_width(spans).saturating_sub(hang)
            };
            if offset < seen + width || index + 1 == rows.len() {
                return Some((index, hang + offset - seen));
            }
            seen += width;
        }
        None
    }

    /// 这一行开头有几个空格（续行的悬挂缩进就是按它算的）。
    fn leading_spaces(spans: &[AnsiSpan]) -> usize {
        let mut count = 0;
        for span in spans {
            for ch in span.text.chars() {
                if ch == ' ' {
                    count += 1;
                } else {
                    return count;
                }
            }
        }
        count
    }

    /// 摘掉行首最多 `count` 个空格。
    fn strip_leading(spans: &mut Vec<AnsiSpan>, mut count: usize) {
        while count > 0 {
            let Some(first) = spans.first_mut() else {
                return;
            };
            let take = first
                .text
                .chars()
                .take_while(|ch| *ch == ' ')
                .count()
                .min(count);
            if take == 0 {
                return;
            }
            first.text.drain(..take);
            count -= take;
            if first.text.is_empty() {
                spans.remove(0);
            }
        }
    }

    fn spans_width(spans: &[AnsiSpan]) -> usize {
        spans
            .iter()
            .flat_map(|span| span.text.chars())
            .map(|ch| ch.width().unwrap_or(0))
            .sum()
    }

    /// 一条逻辑行按 `cols` 断成若干行，续行前面补 `indent` 个空格（正文的装订
    /// 边）。零宽的组合记号跟着前一个字走。
    fn split_spans(spans: &[AnsiSpan], cols: usize, indent: usize) -> Vec<Vec<AnsiSpan>> {
        let hang = || AnsiSpan {
            text: " ".repeat(indent),
            style: Style::new(),
            link: None,
        };
        let mut rows: Vec<Vec<AnsiSpan>> = Vec::new();
        let mut current: Vec<AnsiSpan> = Vec::new();
        let mut width = 0;
        for span in spans {
            let mut text = String::new();
            for ch in span.text.chars() {
                let ch_width = ch.width().unwrap_or(0);
                if ch_width > 0 && width + ch_width > cols {
                    if !text.is_empty() {
                        current.push(AnsiSpan {
                            text: std::mem::take(&mut text),
                            style: span.style,
                            link: span.link.clone(),
                        });
                    }
                    rows.push(std::mem::take(&mut current));
                    width = indent;
                    if indent > 0 {
                        current.push(hang());
                    }
                }
                text.push(ch);
                width += ch_width;
            }
            if !text.is_empty() {
                current.push(AnsiSpan {
                    text,
                    style: span.style,
                    link: span.link.clone(),
                });
            }
        }
        rows.push(current);
        rows
    }

    /// span 还原成格子——活动段要能被光标随机改写，只能是格子形态。
    fn to_cells(spans: &[AnsiSpan]) -> Vec<Cell> {
        let mut cells: Vec<Cell> = Vec::new();
        for span in spans {
            let link = span.link.as_deref().map(std::sync::Arc::from);
            for ch in span.text.chars() {
                let width = ch.width().unwrap_or(0);
                if width == 0 {
                    // 组合记号追加到前一格上，理由同 `put`：kitty 的占位格靠它
                    // 带行列号。
                    if let Some(cell) = cells.last_mut() {
                        let mut marks = cell
                            .marks
                            .take()
                            .map_or_else(String::new, |marks| marks.into_string());
                        marks.push(ch);
                        cell.marks = Some(marks.into_boxed_str());
                    }
                    continue;
                }
                cells.push(Cell {
                    ch,
                    marks: None,
                    link: link.clone(),
                    style: span.style,
                    continuation: false,
                });
                for _ in 1..width {
                    cells.push(Cell {
                        ch: ' ',
                        marks: None,
                        link: link.clone(),
                        style: span.style,
                        continuation: true,
                    });
                }
            }
        }
        cells
    }

    pub fn blocks(&self) -> &[BlockSpan] {
        &self.blocks
    }

    /// 把 `live` 里跑远的行压进存档，只留最近 [`LIVE_ROWS`] 行可写。
    fn archive_old(&mut self) {
        // 活动区（锚点及以下）每一帧都要回头改写，不进存档。
        let writable_from = self.live_anchor.unwrap_or(usize::MAX);
        while self.lines.len() > LIVE_ROWS && self.row > 0 && self.archive.len() < writable_from {
            let line = self.lines.remove(0);
            if !self.stamps.is_empty() {
                self.stamps.remove(0);
            }
            let wrapped = if self.wraps.is_empty() {
                false
            } else {
                self.wraps.remove(0)
            };
            self.archive.push(ArchivedRow {
                spans: Self::compress(&line),
                wrapped,
            });
            self.row -= 1;
        }
    }

    fn line_mut(&mut self) -> &mut Vec<Cell> {
        while self.lines.len() <= self.row {
            self.lines.push(Vec::new());
        }
        let row = self.row;
        self.touch(row);
        &mut self.lines[row]
    }

    fn put(&mut self, ch: char) {
        let width = ch.width().unwrap_or(0);
        if width == 0 {
            // 组合记号：**追加**到前一格上，别自己占位，更别把基字符覆盖掉。
            //
            // 写成 `cell.ch = ch` 的后果很具体：kitty 的占位格是
            // `U+10EEEE + 行号记号 + 列号记号`，覆盖之后只剩最后一个记号，
            // 基字符没了、行列号也没了——终端收到一堆孤零零的记号，图一张都
            // 放不出来。真机截图里就是"占位格铺了几行，图没有"。
            let col = self.col;
            let line = self.line_mut();
            if col > 0 {
                if let Some(cell) = line.get_mut(col - 1) {
                    let mut marks = cell
                        .marks
                        .take()
                        .map_or_else(String::new, |marks| marks.into_string());
                    marks.push(ch);
                    cell.marks = Some(marks.into_boxed_str());
                }
            }
            return;
        }
        // 到边就折。真终端写满最后一格只是挂起「待折行」标志，下一个字符才
        // 真的换行；这里按「放不下就先换」处理，对纯输出流等价。
        if self.col + width > self.cols {
            // 这一下是**自己折**的，不是作者断的：记一笔，改宽度时这两行要并
            // 回一条逻辑行重排（见 `reflow`）。
            let row = self.row;
            self.set_wrapped(row, true);
            self.newline();
            self.col = 0;
        }
        let (col, style) = (self.col, self.style);
        let link = self.link.clone();
        let line = self.line_mut();
        while line.len() < col {
            line.push(Cell::blank());
        }
        let cell = Cell {
            ch,
            marks: None,
            link: link.clone(),
            style,
            continuation: false,
        };
        if col < line.len() {
            line[col] = cell;
        } else {
            line.push(cell);
        }
        if width == 2 {
            let tail = Cell {
                ch: ' ',
                marks: None,
                link: link.clone(),
                style,
                continuation: true,
            };
            if col + 1 < line.len() {
                line[col + 1] = tail;
            } else {
                line.push(tail);
            }
        }
        self.col += width;
    }

    fn newline(&mut self) {
        self.row += 1;
        while self.lines.len() <= self.row {
            self.lines.push(Vec::new());
        }
        self.archive_old();
    }

    /// 从光标清到行尾（`CSI K` 的默认形态）。
    fn clear_to_end(&mut self) {
        let col = self.col;
        let line = self.line_mut();
        line.truncate(col.min(line.len()));
        // 这一行不再顶到右边了，下一行也就不再是它的续行（spinner 每帧都是
        // 「上移 → 清行 → 重画」，标志不清掉会把两行错并成一条）。
        let row = self.row;
        self.set_wrapped(row, false);
    }

    fn clear_line(&mut self) {
        let line = self.line_mut();
        line.clear();
        let row = self.row;
        self.set_wrapped(row, false);
    }

    fn param(params: &Params, index: usize, default: usize) -> usize {
        params
            .iter()
            .nth(index)
            .and_then(|values| values.first().copied())
            .map(usize::from)
            .filter(|value| *value != 0)
            .unwrap_or(default)
    }

    fn apply_sgr(&mut self, params: &Params) {
        if params.is_empty() {
            self.style = Style::new();
            return;
        }
        let flat: Vec<&[u16]> = params.iter().collect();
        let mut index = 0;
        while index < flat.len() {
            let Some(&code) = flat[index].first() else {
                index += 1;
                continue;
            };
            match code {
                0 => self.style = Style::new(),
                1 => self.style = self.style.add_modifier(Modifier::BOLD),
                2 => self.style = self.style.add_modifier(Modifier::DIM),
                3 => self.style = self.style.add_modifier(Modifier::ITALIC),
                4 => self.style = self.style.add_modifier(Modifier::UNDERLINED),
                5 | 6 => self.style = self.style.add_modifier(Modifier::SLOW_BLINK),
                7 => self.style = self.style.add_modifier(Modifier::REVERSED),
                8 => self.style = self.style.add_modifier(Modifier::HIDDEN),
                9 => self.style = self.style.add_modifier(Modifier::CROSSED_OUT),
                22 => {
                    self.style = self.style.remove_modifier(Modifier::BOLD | Modifier::DIM);
                }
                23 => self.style = self.style.remove_modifier(Modifier::ITALIC),
                24 => self.style = self.style.remove_modifier(Modifier::UNDERLINED),
                25 => self.style = self.style.remove_modifier(Modifier::SLOW_BLINK),
                27 => self.style = self.style.remove_modifier(Modifier::REVERSED),
                28 => self.style = self.style.remove_modifier(Modifier::HIDDEN),
                29 => self.style = self.style.remove_modifier(Modifier::CROSSED_OUT),
                30..=37 => self.style = self.style.fg(basic(code - 30)),
                38 => {
                    let (color, used) = extended(&flat, index);
                    if let Some(color) = color {
                        self.style = self.style.fg(color);
                    }
                    index += used;
                }
                39 => self.style.fg = None,
                40..=47 => self.style = self.style.bg(basic(code - 40)),
                48 => {
                    let (color, used) = extended(&flat, index);
                    if let Some(color) = color {
                        self.style = self.style.bg(color);
                    }
                    index += used;
                }
                49 => self.style.bg = None,
                90..=97 => self.style = self.style.fg(bright(code - 90)),
                100..=107 => self.style = self.style.bg(bright(code - 100)),
                _ => {}
            }
            index += 1;
        }
    }
}

fn basic(offset: u16) -> Color {
    match offset {
        0 => Color::Black,
        1 => Color::Red,
        2 => Color::Green,
        3 => Color::Yellow,
        4 => Color::Blue,
        5 => Color::Magenta,
        6 => Color::Cyan,
        _ => Color::Gray,
    }
}

fn bright(offset: u16) -> Color {
    match offset {
        0 => Color::DarkGray,
        1 => Color::LightRed,
        2 => Color::LightGreen,
        3 => Color::LightYellow,
        4 => Color::LightBlue,
        5 => Color::LightMagenta,
        6 => Color::LightCyan,
        _ => Color::White,
    }
}

/// `38` / `48` 之后的扩展色。分号和冒号两种写法都认。
fn extended(flat: &[&[u16]], index: usize) -> (Option<Color>, usize) {
    let param = flat[index];
    if param.len() > 1 {
        return (from_parts(&param[1..]), 0);
    }
    let rest: Vec<u16> = flat[index + 1..]
        .iter()
        .filter_map(|values| values.first().copied())
        .collect();
    match rest.first() {
        Some(5) => (from_parts(&rest[..2.min(rest.len())]), 2),
        Some(2) => (from_parts(&rest[..4.min(rest.len())]), 4),
        _ => (None, 0),
    }
}

fn from_parts(parts: &[u16]) -> Option<Color> {
    match parts.first()? {
        5 => parts.get(1).map(|index| Color::Indexed(*index as u8)),
        2 => match (parts.get(1), parts.get(2), parts.get(3)) {
            (Some(r), Some(g), Some(b)) => Some(Color::Rgb(*r as u8, *g as u8, *b as u8)),
            _ => None,
        },
        _ => None,
    }
}

impl Perform for Term {
    fn print(&mut self, character: char) {
        if let Some((id, open)) = self.pending_block.take() {
            let start = self.cursor_row();
            // 同一块在原地被重写时,**把它原来的结束行留着**。
            //
            // 转轮那侧为了不闪,一帧只重写变了的行(`live_area::rewrite_changed_rows`)。
            // 滚动思考窗的第一行每帧都在变(转轮、秒数),后面几行常常没变——于是
            // 这一帧只带开始标记、不带结束标记。要是照旧把跨度清成 `start..start`,
            // 块就停在半开状态,展开层算出「要替换 0 行」,于是把展开内容**插进去**
            // 而不是替换掉:同一段思考在屏幕上出现两份,还随每帧长短乱跳
            //（用户 09-19：「点击展开之后就开始鬼畜」）。
            let reopened = self
                .blocks
                .iter()
                .find(|block| block.id == id && block.start == start)
                .map(|block| block.end)
                .filter(|end| *end > start);
            // 这一行（及其之后）要被重写了：原来记在那儿的块作废。
            // 活动区的实时那几行每一帧都是「上移 → 清行 → 重画」，不作废的话
            // 每 tick 都会多攒一个块，行号还全是错的。
            self.blocks.retain(|block| block.start < start);
            self.blocks.push(BlockSpan {
                id,
                start,
                end: reopened.unwrap_or(start),
                open,
            });
        }
        self.put(character);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            b'\n' | 0x0b | 0x0c => {
                if std::mem::take(&mut self.pending_soft_wrap) {
                    let row = self.row;
                    self.set_wrapped(row, true);
                }
                self.newline();
                self.col = 0;
            }
            b'\r' => self.col = 0,
            0x08 => self.col = self.col.saturating_sub(1),
            b'\t' => {
                let next = (self.col / 8 + 1) * 8;
                self.col = next;
            }
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        if ignore || !intermediates.is_empty() {
            return;
        }
        let count = Self::param(params, 0, 1);
        match action {
            'm' => self.apply_sgr(params),
            'A' => self.row = self.row.saturating_sub(count),
            'B' | 'e' => {
                self.row = self.row.saturating_add(count);
                while self.lines.len() <= self.row {
                    self.lines.push(Vec::new());
                }
            }
            'C' | 'a' => self.col = self.col.saturating_add(count),
            'D' => self.col = self.col.saturating_sub(count),
            'E' => {
                self.row = self.row.saturating_add(count);
                self.col = 0;
            }
            'F' => {
                self.row = self.row.saturating_sub(count);
                self.col = 0;
            }
            'G' | '`' => self.col = count.saturating_sub(1),
            'K' => match Self::param(params, 0, 0) {
                // 0=到行尾（默认），1=到行首，2=整行
                1 | 2 => self.clear_line(),
                _ => self.clear_to_end(),
            },
            'J' => {
                // 清屏：正文区整段作废，但历史留着——全屏模型里「清屏」
                // 的语义是把视口推空，不是把内容删掉。
                self.clear_to_end();
            }
            _ => {}
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        if let Some(b"1337") = params.first().copied() {
            let payload = params
                .get(1)
                .map(|value| String::from_utf8_lossy(value).into_owned())
                .unwrap_or_default();
            match miyu_hosts::render::blocks::parse_marker(&payload) {
                Some(miyu_hosts::render::blocks::BlockMarker::Begin { id, open }) => {
                    self.pending_block = Some((id, open));
                }
                Some(miyu_hosts::render::blocks::BlockMarker::End) => self.close_block(),
                // 这一轮从光标所在行开始；光标停在半行上（上一段正文没换行）就算
                // 下一行——截回去的时候那半行是上一轮的，得留着。
                Some(miyu_hosts::render::blocks::BlockMarker::TurnStart) => {
                    let start = if self.col == 0 {
                        self.cursor_row()
                    } else {
                        self.cursor_row().saturating_add(1)
                    };
                    self.turn_starts.push(start);
                }
                // 渲染器自己折的那一下：下一个换行按「折出来的」记。
                Some(miyu_hosts::render::blocks::BlockMarker::SoftWrap) => {
                    self.pending_soft_wrap = true;
                }
                Some(miyu_hosts::render::blocks::BlockMarker::LiveRewind { from }) => {
                    self.live_rewind(from)
                }
                Some(miyu_hosts::render::blocks::BlockMarker::LiveEnd) => self.live_end(),
                Some(miyu_hosts::render::blocks::BlockMarker::CompactStart) => {
                    let start = if self.col == 0 {
                        self.cursor_row()
                    } else {
                        self.cursor_row().saturating_add(1)
                    };
                    self.compact_starts.push(start);
                }
                None => {}
            }
            return;
        }
        let Some(b"8") = params.first().copied() else {
            return;
        };
        // 直接存成 `Arc`：每写一个字符都要给那一格挂一份，现转的话一行链接就
        // 分配几十次。
        self.link = params.get(2).filter(|uri| !uri.is_empty()).map(|uri| {
            std::sync::Arc::from(String::from_utf8_lossy(uri).into_owned().into_boxed_str())
        });
    }
}
