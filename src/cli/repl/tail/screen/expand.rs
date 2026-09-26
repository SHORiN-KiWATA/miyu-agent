//! 点击展开：折叠的块点一下摊开，再点收起，**可以嵌套**。
//!
//! 展开**不改缓冲**。`Term` 里那份是「终端真的收到过什么」，改了它光标行号、
//! 存档边界、正在流入的那一行全要跟着算，得不偿失。这里在缓冲之上加一层视图
//! 映射：视图行 = 缓冲行 + 前面所有已展开块撑出来的行数。折叠回去只是把映射
//! 撤掉，缓冲从头到尾没动过。
//!
//! **嵌套**靠一个巧劲：展开内容本身也是一段 ANSI，里面照样可以带块标记。把它
//! 喂进一个临时 [`Term`]，拿回来的就是同样的「行 + 块区间」结构——于是展开一层
//! 和展开顶层走的是同一套代码，递归下去就行。`Worked for 12s · 3 tools` 展开成
//! 时间线、时间线里每一项再展开看详情，就是这么来的。
//!
//! 画面、选区、点击都只认视图行号，[`Screen::view_row`] 是唯一的取行入口。

use super::ansi::AnsiSpan;
use super::term::{BlockSpan, Term};
use super::Screen;
use std::collections::HashMap;

/// 一段内容：行，加上它内部的块区间。顶层是 `Term` 自己，展开出来的每一段
/// 也是这个形状——所以能递归。
pub(in crate::cli) struct Body {
    rows: Vec<Vec<AnsiSpan>>,
    blocks: Vec<BlockSpan>,
    /// 取到的是登记处的哪一版。内容还在长的块（正在想的那一步）要靠它知道
    /// 该重取了——展开着却一直显示刚点开那一瞬的内容，等于没在流。
    version: u64,
}

impl Body {
    /// 把一段 ANSI 解析成行 + 块。复用终端模拟器，不另写解析。
    ///
    /// `cols` 必须是**画面真实宽度**：临时缓冲的默认宽度和屏幕不一样的话，
    /// 展开出来的长行会按错误的宽度折，看着像随机断行。
    pub(in crate::cli) fn parse(text: &str, cols: usize) -> Self {
        let mut term = Term::default();
        term.set_cols(cols);
        term.feed(text.as_bytes());
        let rows = (0..term.line_count())
            .map(|index| term.row_spans(index))
            .collect();
        Self {
            rows,
            blocks: term.blocks().to_vec(),
            version: 0,
        }
    }
}

/// 已展开的块：id → 解析好的内容。
pub(in crate::cli) type Expanded = HashMap<u64, Body>;

/// 一层内容的只读视图。顶层看 `Term`，展开层看 `Body`——两者行为一致，
/// 递归代码只写一遍。
pub(in crate::cli) enum Layer<'a> {
    Live(&'a Term),
    Body(&'a Body),
}

impl Layer<'_> {
    fn len(&self) -> usize {
        match self {
            // 用 `filled_rows` 而不是 `content_rows`：两者只差"光标停在正文
            // 末尾几行空白之下"的那几行。把它们算进视图的话，跟随会一路滚到
            // 它们那儿，屏幕底下于是空出几行，正文被顶得老高。
            Layer::Live(term) => term.filled_rows(),
            Layer::Body(body) => body.rows.len(),
        }
    }

    fn row(&self, index: usize) -> Vec<AnsiSpan> {
        match self {
            Layer::Live(term) => term.row_spans(index),
            Layer::Body(body) => body.rows.get(index).cloned().unwrap_or_default(),
        }
    }

    fn blocks(&self) -> &[BlockSpan] {
        match self {
            Layer::Live(term) => term.blocks(),
            Layer::Body(body) => &body.blocks,
        }
    }
}

/// 这一层展开后一共多少行。
pub(in crate::cli) fn layer_len(layer: &Layer, expanded: &Expanded) -> usize {
    // 一个块都没展开时长度就是原样。不早退的话这儿要把**所有**块走一遍、
    // 每个做一次哈希查找——而这个函数每帧要调好几次（算滚动上限、算贴底垫多少）。
    // 聊久了缓冲里攒着几百个块，这一下就是几千次无用功。
    if expanded.is_empty() {
        return layer.len();
    }
    let mut total = layer.len();
    for block in layer.blocks() {
        let Some(body) = expanded.get(&block.id) else {
            continue;
        };
        let collapsed = block.end.saturating_sub(block.start);
        total = total
            .saturating_add(layer_len(&Layer::Body(body), expanded))
            .saturating_sub(collapsed);
    }
    total
}

/// 递归取行。`index` 是这一层展开后的行号。
pub(in crate::cli) fn layer_row(layer: &Layer, expanded: &Expanded, index: usize) -> Vec<AnsiSpan> {
    // 同 `layer_len`：没展开就没有偏移可算。这个函数**每行每帧**都要调。
    if expanded.is_empty() {
        return layer.row(index);
    }
    let mut offset = 0usize;
    for block in layer.blocks() {
        let Some(body) = expanded.get(&block.id) else {
            continue;
        };
        let start = block.start.saturating_add(offset);
        if index < start {
            break;
        }
        let inner = Layer::Body(body);
        let height = layer_len(&inner, expanded);
        if index < start.saturating_add(height) {
            return layer_row(&inner, expanded, index - start);
        }
        let collapsed = block.end.saturating_sub(block.start);
        offset = offset.saturating_add(height).saturating_sub(collapsed);
    }
    layer.row(index.saturating_sub(offset))
}

/// 这一块展开之后里面还有别的块吗？
///
/// 有（`Worked for …` 展开成一条时间线）就只有**表头**可点：里面每一项都是
/// 自己的把手，整片都能收起来的话，想点开某一步反而把整条线收没了。
/// 没有（一步的详情正文）就整片可点——那一整块讲的是同一件事。
fn has_children(body: &Body) -> bool {
    !body.blocks.is_empty()
}

/// 递归命中测试。返回**最内层**命中的块——嵌套时点到哪一层就收哪一层。
pub(in crate::cli) fn layer_hit(
    layer: &Layer,
    expanded: &Expanded,
    index: usize,
) -> Option<(u64, usize)> {
    // 没展开时块的位置就是它自己的位置，直接二分。线性扫的代价很具体：
    // 鼠标悬浮和选区都按行调它，一屏三十几行、缓冲里几百个块，一帧就是上万次
    // 比较——AI 正在输出时两边叠在一起，手上就是"选文本好卡"。
    if expanded.is_empty() {
        let blocks = layer.blocks();
        let found = blocks.partition_point(|block| block.start <= index);
        let block = blocks.get(found.checked_sub(1)?)?;
        return (index < block.end).then_some((block.id, block.start));
    }
    let mut offset = 0usize;
    for block in layer.blocks() {
        let start = block.start.saturating_add(offset);
        if index < start {
            return None;
        }
        let collapsed = block.end.saturating_sub(block.start);
        match expanded.get(&block.id) {
            Some(body) => {
                let inner = Layer::Body(body);
                let height = layer_len(&inner, expanded);
                if index < start.saturating_add(height) {
                    // 先问内层：点在嵌套块上就收那一个。
                    if let Some(hit) = layer_hit(&inner, expanded, index - start) {
                        return Some((hit.0, start.saturating_add(hit.1)));
                    }
                    // 表头永远可点：它是把手。
                    if index == start {
                        return Some((block.id, start));
                    }
                    // 里面没有别的块时整片都算这一块——**空行也算**。
                    //
                    // 不算的话，鼠标在展开的正文里扫过空行时高亮一闪一闪，点上去
                    // 还没反应；可那一片明明看着就是一整块（用户原话「在思考内容的
                    // 空行的地方会是非交互状态，导致鼠标划过会一闪一闪的」）。
                    if !has_children(body) {
                        return Some((block.id, start));
                    }
                    return None;
                }
                offset = offset.saturating_add(height).saturating_sub(collapsed);
            }
            None => {
                if index < start.saturating_add(collapsed) {
                    return Some((block.id, start));
                }
            }
        }
    }
    None
}

/// 展开一块：从登记处取内容、解析成 `Body`。取不到就没得展开。
pub(in crate::cli) fn load_body(id: u64, cols: usize) -> Option<Body> {
    let lines = miyu_hosts::render::blocks::get(id)?;
    if lines.is_empty() {
        return None;
    }
    // 不补结尾换行：补了会在缓冲里多出一行空的。
    let mut body = Body::parse(&lines.join("\r\n"), cols);
    body.version = miyu_hosts::render::blocks::version(id);
    (!body.rows.is_empty()).then_some(body)
}

/// 「默认开着」的块第一次露面时替用户开一次。返回真表示视图变了。
///
/// `显示思考过程 / 显示工具调用信息 = 完整` 就落在这儿：那一档不再往抬头底下
/// 挂一截预览（那是第四种形态，谁都没设计过），而是让**这一步本身出来就是展开
/// 态**——再点一次照样收得回去（用户 09-17 原话）。
///
/// `seen` 记着已经替用户开过的 id，**只开一次**：活动区每 tick 把同样的标记重写
/// 一遍，不记的话用户刚收起来下一帧就被顶开。
///
/// 嵌套也要走到：`Worked for …` 收缩行点开之后，里面那几步的档位照样算数，所以
/// 每插进一层就重新扫一遍，直到没有新的为止。
pub(in crate::cli) fn seed_open(
    layer: &Layer,
    expanded: &mut Expanded,
    seen: &mut std::collections::HashSet<u64>,
    cols: usize,
) -> bool {
    let mut changed = false;
    // 每开一层都可能露出新的一层。层数是时间线的嵌套深度（正文 → 收缩行 →
    // 步），给个上限免得哪天内容自引用转不出来。
    for _ in 0..8 {
        let mut pending = Vec::new();
        collect_open(layer, expanded, seen, &mut pending);
        if pending.is_empty() {
            break;
        }
        for id in pending {
            seen.insert(id);
            if let Some(body) = load_body(id, cols) {
                expanded.insert(id, body);
                changed = true;
            }
        }
    }
    changed
}

/// 这一层（连同已经展开的内层）里还没开过的「默认开着」的块。
fn collect_open(
    layer: &Layer,
    expanded: &Expanded,
    seen: &std::collections::HashSet<u64>,
    out: &mut Vec<u64>,
) {
    for block in layer.blocks() {
        if block.open && !seen.contains(&block.id) && !out.contains(&block.id) {
            out.push(block.id);
        }
        if let Some(body) = expanded.get(&block.id) {
            collect_open(&Layer::Body(body), expanded, seen, out);
        }
    }
}

/// 在一份 `expanded` 表上开合一块。返回真表示视图变了。
/// 正文和覆盖层用的是同一套，各自带自己的表。
pub(in crate::cli) fn toggle_in(expanded: &mut Expanded, id: u64, cols: usize) -> bool {
    if expanded.remove(&id).is_some() {
        return true;
    }
    match load_body(id, cols) {
        Some(body) => {
            expanded.insert(id, body);
            true
        }
        None => false,
    }
}

/// 把展开着的那几块的内容重取一遍（块还在长的时候用）。
///
/// `Expanded` 存的是**点开那一刻的快照**：块后来被更新，屏幕上那一片还是旧的。
/// 面板里的子代理一边跑一边写，不重取就等于点开之后冻住了。取不到的（块被
/// 淘汰了）直接丢掉。
pub(in crate::cli) fn reload_expanded(expanded: &mut Expanded, cols: usize) {
    if expanded.is_empty() {
        return;
    }
    let ids = expanded.keys().copied().collect::<Vec<_>>();
    for id in ids {
        match load_body(id, cols) {
            Some(body) => {
                expanded.insert(id, body);
            }
            None => {
                expanded.remove(&id);
            }
        }
    }
}

/// 这一行是不是**展开出来的内容**。画暗底用：展开的那一片和正文得分得开。
/// 面板用：这一行是不是它自己那张展开表里展开出来的内容。
pub(in crate::cli) fn body_in_expansion(body: &Body, expanded: &Expanded, index: usize) -> bool {
    layer_in_expansion(&Layer::Body(body), expanded, index)
}

fn layer_in_expansion(layer: &Layer, expanded: &Expanded, index: usize) -> bool {
    // 没展开就不可能"在展开区里"。少了这一句，画每一行都要把所有块走一遍。
    if expanded.is_empty() {
        return false;
    }
    let mut offset = 0usize;
    for block in layer.blocks() {
        let Some(body) = expanded.get(&block.id) else {
            continue;
        };
        let start = block.start.saturating_add(offset);
        if index < start {
            return false;
        }
        let inner = Layer::Body(body);
        let height = layer_len(&inner, expanded);
        if index < start.saturating_add(height) {
            // 只有**叶子块**画底。`Worked for` 展开出来的是一条时间线——给整条
            // 加底等于把正文一大片染色，喧宾夺主；真正需要"这是一整块"的是点开
            // 某一步之后那一片。里面还有块就继续往里问。
            if has_children(body) {
                return layer_in_expansion(&inner, expanded, index - start);
            }
            return true;
        }
        let collapsed = block.end.saturating_sub(block.start);
        offset = offset.saturating_add(height).saturating_sub(collapsed);
    }
    false
}

/// 缓冲和展开表的一个「版本戳」：这两样没变，视图索引就还是对的。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::cli) struct ViewStamp {
    lines: usize,
    blocks: usize,
    last_block: (u64, usize, usize),
    expanded_gen: u64,
}

/// 顶层视图索引：缓冲里每一块在视图里落在哪儿、展开后占几行。
///
/// 展开着东西的时候，视图行 ↔ 缓冲行的换算原来是**每一行**把缓冲里全部块走一遍
/// （取行 / 命中 / 判底色三条路各走一遍，每遇到一个展开块再把它的子树重算一次
/// 高）。一屏三十几行 × 三遍 × 几百上千块，每帧上万次哈希查找——长会话里一帧
/// 从几十微秒涨到几十毫秒，手感就是"内容多了之后巨卡"（量尺 `tests::tui_perf`：
/// 500 个展开块时 27ms/帧，debug 档）。这张表只在缓冲或展开表变了才重算，之后每
/// 一行只做一次二分。
pub(in crate::cli) struct ViewIndex {
    entries: Vec<ViewBlock>,
    /// 全部展开块撑出来的行数之和。
    extra: usize,
    stamp: ViewStamp,
}

#[derive(Clone, Copy, Debug)]
struct ViewBlock {
    id: u64,
    /// 缓冲里从第几行开始。
    start: usize,
    /// 在视图里从第几行开始（= `start` + 前面的块撑出来的行数）。
    view_start: usize,
    /// 视图里占几行：合着就是这一块在缓冲里占的行数，开着是摊开后的总高。
    height: usize,
    /// 到这一块（含）为止，展开一共撑出来多少行。
    offset_after: usize,
    expanded: bool,
}

/// 一个视图行落在哪儿。
enum Located {
    /// 就是缓冲的第几行（块外，或合着的块里）。
    Raw(usize),
    /// 在某个展开块摊开的内容里：块 id + 那一块在视图里的起始行。
    Inside { id: u64, view_start: usize },
}

impl ViewIndex {
    fn build(term: &Term, expanded: &Expanded, stamp: ViewStamp) -> Self {
        let mut entries = Vec::with_capacity(term.blocks().len());
        let mut offset = 0usize;
        for block in term.blocks() {
            let collapsed = block.end.saturating_sub(block.start);
            let view_start = block.start.saturating_add(offset);
            let (height, is_expanded) = match expanded.get(&block.id) {
                Some(body) => (layer_len(&Layer::Body(body), expanded), true),
                None => (collapsed, false),
            };
            if is_expanded {
                offset = offset.saturating_add(height).saturating_sub(collapsed);
            }
            entries.push(ViewBlock {
                id: block.id,
                start: block.start,
                view_start,
                height,
                offset_after: offset,
                expanded: is_expanded,
            });
        }
        Self {
            entries,
            extra: offset,
            stamp,
        }
    }

    /// 最后一个从 `index` 或它之前开始的块。
    fn entry_at(&self, index: usize) -> Option<&ViewBlock> {
        let found = self
            .entries
            .partition_point(|entry| entry.view_start <= index);
        self.entries.get(found.checked_sub(1)?)
    }

    fn locate(&self, index: usize) -> Located {
        let Some(entry) = self.entry_at(index) else {
            return Located::Raw(index);
        };
        if entry.expanded && index < entry.view_start.saturating_add(entry.height) {
            return Located::Inside {
                id: entry.id,
                view_start: entry.view_start,
            };
        }
        Located::Raw(index.saturating_sub(entry.offset_after))
    }

    /// 缓冲行 → 视图行：加上它前面所有展开块撑出来的行。
    fn view_of(&self, buffer_row: usize) -> usize {
        let found = self
            .entries
            .partition_point(|entry| entry.start < buffer_row);
        let offset = found
            .checked_sub(1)
            .map(|index| self.entries[index].offset_after)
            .unwrap_or(0);
        buffer_row.saturating_add(offset)
    }
}

impl Screen {
    /// 这一视图行是不是展开出来的内容。
    pub(in crate::cli) fn in_expansion(&self, index: usize) -> bool {
        if self.expanded.is_empty() {
            return false;
        }
        self.with_index(|view| {
            let Some(entry) = view.entry_at(index) else {
                return false;
            };
            if !entry.expanded || index >= entry.view_start.saturating_add(entry.height) {
                return false;
            }
            let Some(body) = self.expanded.get(&entry.id) else {
                return false;
            };
            // 只有**叶子块**画底（见 `layer_in_expansion`）。
            if has_children(body) {
                return layer_in_expansion(
                    &Layer::Body(body),
                    &self.expanded,
                    index - entry.view_start,
                );
            }
            true
        })
    }

    fn layer(&self) -> Layer<'_> {
        Layer::Live(&self.term)
    }

    fn view_stamp(&self) -> ViewStamp {
        let blocks = self.term.blocks();
        ViewStamp {
            lines: self.term.line_count(),
            blocks: blocks.len(),
            last_block: blocks
                .last()
                .map(|block| (block.id, block.start, block.end))
                .unwrap_or_default(),
            expanded_gen: self.expanded_gen,
        }
    }

    /// 拿着当前的视图索引干点事。索引过期（缓冲长了、展开表变了）就先重算。
    fn with_index<R>(&self, f: impl FnOnce(&ViewIndex) -> R) -> R {
        let stamp = self.view_stamp();
        {
            let cached = self.view_index.borrow();
            if let Some(view) = cached.as_ref().filter(|view| view.stamp == stamp) {
                return f(view);
            }
        }
        let built = ViewIndex::build(&self.term, &self.expanded, stamp);
        let result = f(&built);
        *self.view_index.borrow_mut() = Some(built);
        result
    }

    /// 展开表变了：索引作废。所有改 `expanded` 的地方都要过这儿。
    fn note_expanded_changed(&mut self) {
        self.expanded_gen = self.expanded_gen.wrapping_add(1);
    }

    /// 视图一共多少行。
    pub(in crate::cli) fn view_len(&self) -> usize {
        if self.expanded.is_empty() {
            return self.term.filled_rows();
        }
        self.term.filled_rows() + self.with_index(|view| view.extra)
    }

    /// 缓冲行 → 视图行。只对块**之外**的行有意义（块内的行折叠时压根不存在）。
    pub(in crate::cli) fn view_of(&self, buffer_row: usize) -> usize {
        if self.expanded.is_empty() {
            return buffer_row;
        }
        self.with_index(|view| view.view_of(buffer_row))
    }

    /// 取视图里的一行。
    pub(in crate::cli) fn view_row(&self, index: usize) -> Vec<AnsiSpan> {
        if self.expanded.is_empty() {
            return self.term.row_spans(index);
        }
        self.with_index(|view| match view.locate(index) {
            Located::Raw(row) => self.term.row_spans(row),
            Located::Inside { id, view_start } => match self.expanded.get(&id) {
                Some(body) => layer_row(&Layer::Body(body), &self.expanded, index - view_start),
                None => Vec::new(),
            },
        })
    }

    /// 这一视图行的内容来自哪儿、第几版——行级缓存的键。
    ///
    /// 块外的行是缓冲行的版本号；展开出来的行是那一块的 id + 内容版本 + 块内行号
    ///（内容还在长的块每次重取都换版本，见 `refresh_expanded`）。原来展开着东西
    /// 就整个不缓存，视口里三十几行每帧全部重排。
    pub(in crate::cli) fn row_source_stamp(&self, index: usize) -> u64 {
        if self.expanded.is_empty() {
            return self.term.row_stamp(index);
        }
        self.with_index(|view| match view.locate(index) {
            Located::Raw(row) => (row as u64) << 32 | (self.term.row_stamp(row) & 0xffff_ffff),
            Located::Inside { id, view_start } => {
                let version = self.expanded.get(&id).map(|body| body.version).unwrap_or(0);
                (1u64 << 63)
                    | ((id & 0xffff) << 46)
                    | ((version & 0x3fff) << 32)
                    | ((index - view_start) as u64 & 0xffff_ffff)
            }
        })
    }

    /// 视图行落在哪一块里（最内层）。返回块 id 和它在视图里的起始行。
    pub(in crate::cli) fn block_at(&self, index: usize) -> Option<(u64, usize)> {
        if self.expanded.is_empty() {
            return layer_hit(&self.layer(), &self.expanded, index);
        }
        self.with_index(|view| {
            let entry = view.entry_at(index)?;
            if index >= entry.view_start.saturating_add(entry.height) {
                return None;
            }
            if !entry.expanded {
                return Some((entry.id, entry.view_start));
            }
            let body = self.expanded.get(&entry.id)?;
            let inner = Layer::Body(body);
            // 先问内层：点在嵌套块上就收那一个。
            if let Some(hit) = layer_hit(&inner, &self.expanded, index - entry.view_start) {
                return Some((hit.0, entry.view_start.saturating_add(hit.1)));
            }
            // 表头永远可点；里面没有别的块时整片都算这一块（含空行）。
            if index == entry.view_start || !has_children(body) {
                return Some((entry.id, entry.view_start));
            }
            None
        })
    }

    /// 展开 / 收起。返回真表示视图变了，得重画。
    ///
    /// 展开时**钉住视口**：块的头行留在屏幕原处，展开的内容往下撑，先看到的是开头。
    /// 贴着底的时候也一样（用户 09-26 拍板）：原来贴底就一直贴底，展开出来的行全堆到
    /// 点的那一行上面，看到的是尾巴；代价是下面的回复被推到视口外，往下翻或 End 回去。
    ///
    /// 收起之后跟随要能**自己回来**。写死 `follow = false` 的代价很隐蔽：
    /// 点开又收起以后视口就再也不跟新输出了，命令结果落在视口下面，
    /// 看起来像「命令没反应」。按「是不是已经贴底」重算才对。
    pub(in crate::cli) fn toggle_block(&mut self, id: u64) -> bool {
        let following = self.following();
        if self.expanded.remove(&id).is_some() {
            // 告诉登记处一声：这一步从「正在跑」落成「跑完了」时要换一块新的,
            // 换的时候得知道用户手上这块是开是关(用户 09-19:点开过的行,想完/
            // 跑完都不该被自动收回去)。
            miyu_hosts::render::blocks::set_user_open(id, false);
            self.note_expanded_changed();
            self.restore_follow(following);
            self.invalidate();
            return true;
        }
        let Some(body) = load_body(id, usize::from(self.cols)) else {
            return false;
        };
        miyu_hosts::render::blocks::set_user_open(id, true);
        self.expanded.insert(id, body);
        self.note_expanded_changed();
        // 视口顶端不动（`scroll` 记的是顶端在第几行），撑出来的行都在点的那一行下面；
        // 跟不跟底按展开后的实际位置重判——撑长了就不再贴底。
        self.refresh_follow();
        self.invalidate();
        true
    }

    /// 展开着的块如果内容变了就重取。正在想的那一步点开之后要能**继续**流，
    /// 不然点开的一瞬间就定格了。
    pub(in crate::cli) fn refresh_expanded(&mut self) -> bool {
        if self.expanded.is_empty() {
            return false;
        }
        // 一把锁问完所有版本号：几百个展开块每帧各拿一次登记处的锁不值当。
        let ids: Vec<u64> = self.expanded.keys().copied().collect();
        let versions = miyu_hosts::render::blocks::versions(&ids);
        let stale: Vec<u64> = ids
            .iter()
            .zip(versions)
            .filter(|(id, version)| {
                self.expanded
                    .get(id)
                    .is_some_and(|body| body.version != *version)
            })
            .map(|(id, _)| *id)
            .collect();
        if stale.is_empty() {
            return false;
        }
        let cols = usize::from(self.cols);
        for id in stale {
            match load_body(id, cols) {
                Some(body) => {
                    self.expanded.insert(id, body);
                }
                None => {
                    self.expanded.remove(&id);
                }
            }
        }
        // 不整屏重画：行缓存认得展开出来的行是哪一块的第几版（`row_source_stamp`），
        // 变了的那几行自己会重画。原来这儿每次都 `invalidate()`，而正在想的那一块
        // 每帧都在长——「展开思考内容」开着时整屏每帧重写（BUG-07 初诊 §6）。
        self.note_expanded_changed();
        true
    }

    /// 丢掉已经不在缓冲里的块的展开内容——留着只是占内存，视图早看不见了。
    pub(in crate::cli) fn prune_expanded(&mut self) {
        let alive: std::collections::HashSet<u64> =
            self.term.blocks().iter().map(|block| block.id).collect();
        let before = self.expanded.len();
        self.expanded.retain(|id, _| alive.contains(id));
        if self.expanded.len() != before {
            self.note_expanded_changed();
        }
        // 「开过一次」的记号跟着块一起走：块都滚出缓冲了，再留着只是占内存。
        self.open_seeded.retain(|id| alive.contains(id));
    }

    /// 缓冲里新落下来的「默认开着」的块，替用户开一次。见 [`seed_open`]。
    pub(in crate::cli) fn seed_open_blocks(&mut self) -> bool {
        // 缓冲和展开表都没动过，就没有新的块要替用户开——每帧把全部块扫一遍是
        // 白干（扫的那一遍和视图索引一样贵）。
        let stamp = self.view_stamp();
        if self.seed_stamp == Some(stamp) {
            return false;
        }
        let cols = usize::from(self.cols);
        let Screen {
            term,
            expanded,
            open_seeded,
            ..
        } = self;
        if !seed_open(&Layer::Live(term), expanded, open_seeded, cols) {
            self.seed_stamp = Some(stamp);
            return false;
        }
        self.note_expanded_changed();
        // 开了新块，戳变了；记新戳，下一帧才不会再扫一遍。
        self.seed_stamp = Some(self.view_stamp());
        true
    }
}

#[cfg(test)]
mod tests {
    //! 视图索引和老的逐块遍历算法逐行等价——索引是它的缓存，不是另一套规矩。
    use super::*;
    use miyu_hosts::render::blocks;

    fn markers(id: u64, open: bool, text: &str) -> String {
        format!(
            "{}{text}{}",
            blocks::begin_marker_in(id, open),
            blocks::END_MARKER
        )
    }

    /// 老算法（当时的实现原样留着，只在测试里用）：顶层也逐块遍历。
    fn old_view_of(term: &Term, expanded: &Expanded, buffer_row: usize) -> usize {
        let mut offset = 0usize;
        for block in term.blocks() {
            if block.start >= buffer_row {
                break;
            }
            let Some(body) = expanded.get(&block.id) else {
                continue;
            };
            let collapsed = block.end.saturating_sub(block.start);
            offset = offset
                .saturating_add(layer_len(&Layer::Body(body), expanded))
                .saturating_sub(collapsed);
        }
        buffer_row.saturating_add(offset)
    }

    #[test]
    fn the_view_index_matches_the_block_walk_row_for_row() {
        blocks::set_enabled(true);
        let mut screen = Screen::detached(80, 20);
        // 三种块：默认开着的思考步、合着的工具步、里面还嵌着两步的收缩行。
        let inner_a =
            blocks::register(vec!["  内层甲第一行".into(), "  内层甲第二行".into()]).unwrap();
        let inner_b = blocks::register(vec!["  内层乙".into()]).unwrap();
        let fold = blocks::register(vec![
            "  ⌄ Worked for 1s".into(),
            markers(inner_a, true, "  ✳ 内层甲"),
            "  │".into(),
            markers(inner_b, false, "  $ 内层乙"),
            String::new(),
        ])
        .unwrap();
        for i in 0..12 {
            let thought =
                blocks::register((0..3).map(|k| format!("    想 {i}-{k}")).collect()).unwrap();
            let tool = blocks::register(vec![format!("    命令输出 {i}")]).unwrap();
            let text = format!(
                "{}\r\n  │\r\n{}\r\n正文 {i}\r\n{}\r\n\r\n",
                markers(thought, i % 2 == 0, &format!("  ✳ 已思考 {i}")),
                markers(tool, false, &format!("  $ 运行命令 {i}")),
                markers(fold, false, "  › Worked for 1s · 2 tools"),
            );
            screen.feed_for_test(text.as_bytes());
        }
        // 替用户开默认开着的；再手点开几块（含收缩行，它里面还有默认开着的一层）。
        screen.seed_open_blocks();
        assert!(screen.toggle_block(fold));
        let some_tool = screen.term.blocks()[1].id;
        assert!(screen.toggle_block(some_tool));
        assert!(!screen.expanded.is_empty(), "什么都没展开，测的就不是索引");

        let layer = Layer::Live(&screen.term);
        let total = layer_len(&layer, &screen.expanded);
        assert_eq!(screen.view_len(), total, "视图总行数不等");
        for index in 0..total + 3 {
            assert_eq!(
                screen.view_row(index),
                layer_row(&layer, &screen.expanded, index),
                "第 {index} 行内容不等"
            );
            assert_eq!(
                screen.block_at(index),
                layer_hit(&layer, &screen.expanded, index),
                "第 {index} 行命中不等"
            );
            assert_eq!(
                screen.in_expansion(index),
                layer_in_expansion(&layer, &screen.expanded, index),
                "第 {index} 行是否在展开区里不等"
            );
        }
        for buffer_row in 0..screen.term.line_count() + 2 {
            assert_eq!(
                screen.view_of(buffer_row),
                old_view_of(&screen.term, &screen.expanded, buffer_row),
                "缓冲行 {buffer_row} 的视图行不等"
            );
        }
        // 收起一块之后索引要跟着变（不是拿着旧表）。
        assert!(screen.toggle_block(fold));
        let layer = Layer::Live(&screen.term);
        let total = layer_len(&layer, &screen.expanded);
        assert_eq!(screen.view_len(), total);
        for index in 0..total {
            assert_eq!(
                screen.view_row(index),
                layer_row(&layer, &screen.expanded, index)
            );
        }
        blocks::set_enabled(false);
    }
}
