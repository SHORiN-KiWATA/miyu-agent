//! 全屏后端。
//!
//! inline 模型把正文写进终端 scrollback，用 DECSTBM 在底下钉一块活动区；
//! 全屏模型把正文留在自己手里（[`term::Term`] 那份行缓冲），每帧按视口画。
//! 换来的是可回翻、可拖选、可点击。
//!
//! **控制流一点没动**。主循环、23 个斜杠命令、提问面板、选择器、图片、听写、
//! 排队气泡全部照旧，因为它们看到的接口没变：
//!
//! | 接口 | inline | 全屏 |
//! |---|---|---|
//! | `apply_output_frame(&[u8])` | DECSTBM 受限区写进 scrollback | 喂给终端模拟器，画视口 |
//! | `suspend()` / `resume()` | 收起 / 重画活动区 | 让出屏幕 / 整屏重画 |
//! | 活动区 | `MoveTo(0, tail_start)` 再打 | **一模一样**，只是 `tail_start` 由视口算 |
//!
//! 最后那行是这次重做能省下大半工作量的原因：活动区的渲染本来就是「定位再打」，
//! 全屏下只要把 `tail_start` 指到视口底部，`render_repl_input_with_footer`
//! 一个字都不用改。

pub(in crate::cli) mod ansi;
pub(in crate::cli) mod cells;
mod draw;
pub(in crate::cli) mod expand;
mod older;
pub(in crate::cli) use older::{OlderPageLoader, OlderPages};
pub(in crate::cli) mod overlay;
mod question_panel;
pub(in crate::cli) mod select;
mod tail_impl;
pub(in crate::cli) mod term;
pub(in crate::cli) mod toast;

use ansi::spans_to_ansi;
use anyhow::Result;
use crossterm::cursor::MoveTo;
use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use crossterm::style::Print;
use crossterm::terminal::{Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{execute, queue};
use select::{decoration_of, highlight_columns, slice_columns};
use std::io::Write;
use term::Term;

mod preference;
pub(in crate::cli) use preference::requested;

/// 全屏是否已经生效。
///
/// 这是个全局标志而不是参数，因为要它的地方是 `cursor_position_or` 那种
/// 自由函数——散在四条回合路径上，一个个传参数只会把签名搞脏。
static FULLSCREEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 后台任务面板的抬头：`标题 · 状态 · 量`。
///
/// 带上量：跑了多少词元是判断"它在干活还是卡住"的唯一线索，状态行上有、面板里
/// 没有说不通（用户实测：在这里我期望有的 token 消耗记录也没有）。命令类任务
/// 没有这个概念，那一截就不出现。
pub(in crate::cli) fn job_panel_title(job: &miyu_engine::tools::jobs::JobOverview) -> String {
    match job.metric.as_deref().filter(|text| !text.trim().is_empty()) {
        Some(metric) => format!("{} · {} · {}", job.title, job.status, metric.trim()),
        None => format!("{} · {}", job.title, job.status),
    }
}

pub(in crate::cli) fn in_fullscreen() -> bool {
    FULLSCREEN.load(std::sync::atomic::Ordering::Relaxed)
}

/// 正文区有多大（列, 行）。全屏之外返回 `None`。
///
/// 列数**不含**左右边距：拿到它的人直接按它排版，缩进由 `indent_body` 统一加。
///
/// 图片、表格、公式都得按**这个**算，不是整屏：全屏下正文左边有页边距、
/// 下边压着活动区，按整屏算出来的东西会顶出可视范围——一张按整屏高度铺的图
/// 能把输入框挤到屏幕外面去。
pub(in crate::cli) fn content_viewport() -> Option<(u16, u16)> {
    if !in_fullscreen() {
        return None;
    }
    miyu_base::terminal::content_viewport()
}

/// 把 kitty 的图形传输段（`ESC _ G … ESC \`）从字节流里分出来。
///
/// 返回 `(传输段, 剩下的)`；一段都没有就返回 `None`（免得白拷一遍）。
pub(in crate::cli) fn split_graphics(bytes: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    if !bytes.windows(3).any(|window| window == b"\x1b_G") {
        return None;
    }
    let mut rest = Vec::with_capacity(bytes.len());
    let mut graphics = Vec::new();
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] == 0x1b
            && bytes.get(index + 1) == Some(&b'_')
            && bytes.get(index + 2) == Some(&b'G')
        {
            // 找 `ESC \` 收尾。分包分到一半就整段当传输——宁可多发一段，
            // 也别把半截转义序列留在缓冲里当正文打出来。
            let mut end = index + 3;
            let mut terminated = false;
            while end + 1 < bytes.len() {
                if bytes[end] == 0x1b && bytes[end + 1] == b'\\' {
                    end += 2;
                    terminated = true;
                    break;
                }
                end += 1;
            }
            let end = if terminated { end } else { bytes.len() };
            graphics.extend_from_slice(&bytes[index..end]);
            index = end;
            continue;
        }
        rest.push(bytes[index]);
        index += 1;
    }
    Some((graphics, rest))
}

/// 大厅的一帧交给正文层的样子：每行的 ANSI（按行比对）与同一份的片段（按格子补丁）。
#[derive(Clone)]
pub(in crate::cli) struct BannerRows {
    pub(in crate::cli) text: std::rc::Rc<Vec<String>>,
    pub(in crate::cli) spans: std::rc::Rc<Vec<Vec<ansi::AnsiSpan>>>,
}

/// 正文最多留多少行。再多就从头丢，`Term` 里那份也一起丢。
const MAX_LINES: usize = 20_000;

pub(in crate::cli) struct Screen {
    /// 正文。所有滚出视口的内容都还在这里，这就是「能往回翻」。
    term: Term,
    /// 视口顶端落在正文的第几行。
    scroll: usize,
    /// 跟着底部走。往上翻过就停，翻回底部自动恢复。
    follow: bool,
    /// 上一帧每行画了什么，只发变化的行。
    painted: Vec<String>,
    cols: u16,
    rows: u16,
    /// 外部输出（选择器 / 提问面板 / 图片）正占着屏。
    suspended: bool,
    /// 鼠标拖选。坐标是「缓冲绝对行 + 显示列」，视口滚动不会让它失效。
    selection: Option<select::Selection>,
    /// 松手之后待写进剪贴板的文本。
    pending_copy: Option<String>,
    /// 下一帧先整屏擦一次。外部输出滚过屏幕之后，逐行重画盖不住残留。
    needs_clear: bool,
    /// 面板转轮的计时起点。见 `overlay_spinner_frame`。
    overlay_spinner_started: Option<std::time::Instant>,
    /// 上一帧的正文高度，`paint` 写、回翻与点选读。
    body: Option<u16>,
    /// 已展开的块：id → 摊开后的内容（内部还可以再有块）。空表示全折叠。
    expanded: std::collections::HashMap<u64, expand::Body>,
    /// `expanded` 每变一次 +1。视图索引（`expand::ViewIndex`）靠它知道该重算。
    expanded_gen: u64,
    /// 顶层视图索引的缓存。见 `expand::ViewIndex`——它只在缓冲或展开表变了才重算。
    view_index: std::cell::RefCell<Option<expand::ViewIndex>>,
    /// 上一次替用户开「默认开着」的块时缓冲/展开表长什么样。没变就不用再扫。
    seed_stamp: Option<expand::ViewStamp>,
    /// 「默认开着」的块里已经替用户开过的那些。见 `expand::seed_open`——
    /// 活动区每 tick 重写同样的标记，不记着的话用户收起来下一帧就被顶开。
    open_seeded: std::collections::HashSet<u64>,
    /// `展开思考内容` / `展开工具内容`。
    ///
    /// 后台任务面板是从**日志/标记流**攒步的，手上没有配置入口（渲染统一那份
    /// 报告 §2.4 记着这个缺口），于是同一件事在前台面板跟着开关走、在后台面板
    /// 永远收着（用户 09-17：「子代理浮层中的流式输出不受我们之前做的三个开关
    /// 影响」）。由 REPL 每轮把当前配置交进来。
    display_expand: (bool, bool),
    /// `过程收起成一行摘要`。同 `display_expand`。
    display_fold: bool,
    /// `命令显示行数`：命令那一步抬头底下露几行命令。同 `display_expand`。
    display_command_lines: usize,
    /// `思考滚动窗行数`：面板里正在想的那一步底下露最近几行。同 `display_expand`。
    display_thought_lines: usize,
    /// 盖在正文上的详情面板（子代理）。开着时正文与活动区都不画。
    overlay: Option<overlay::Overlay>,
    /// 鼠标停在哪一块上。可交互的东西要看得出来「这里能点」。
    hover: Option<u64>,
    /// 活动区里输入框那几行（屏幕行号 → 这一行的文字）。
    /// 输入区不在正文缓冲里，要选它就得另记一份。
    input_rows: Vec<(u16, String)>,
    /// 输入区里的选区：起止都是屏幕坐标（行, 列）。
    input_selection: Option<((u16, u16), (u16, u16))>,
    /// 鼠标还按着没有。只有按着的时候拖动才改选区。
    input_dragging: bool,
    /// 浮在输入框上方的一句话通知。
    toast: Option<toast::Toast>,
    /// Ctrl+L 顶上去的那一屏：视口至少能滚到这一行。
    floor: usize,
    /// 回放没画到的更早那部分，往上翻到顶时再补。见 `older`。
    older: Option<OlderPages>,
    /// 每一屏幕行上一帧画的是什么（行号 + 版本 + 装饰）。见 `row_key`。
    row_keys: Vec<Option<(usize, u64, u64)>>,
    /// 斜杠命令候选（浮在输入框上方）。空 = 不显示。
    command_hint: Vec<String>,
    /// 这一串输入的候选面板被 Esc 关过了，别再自己弹回来。
    hint_dismissed: bool,
    /// 下一帧强制全量重画。
    ///
    /// 不能靠「清空 `painted`」来表达这件事：空行画出来就是空串，和清空后
    /// 的初值一模一样，diff 会认为「没变」而跳过，于是屏幕上的旧内容擦不掉
    /// （Ctrl+L 之后视口清不干净就是这么来的）。
    force: bool,
    /// 空会话的画面:正文区不画正文(反正是空的),画这几行。`Some` 时每帧由
    /// 活动区那边重新生成(星星在动),这里比对着往屏上写。
    banner: Option<BannerRows>,
    /// 屏上现在是哪一版大厅。按格子补丁要以它为底（见 `draw.rs`）。
    banner_shown: Option<BannerRows>,
    /// 大厅里浮层(斜杠命令候选)的落点:(顶行, 左列)。None = 贴正文底部。
    float_anchor: Option<(u16, u16)>,
}

/// 诊断用：当前进程的 RSS（KB）。
pub(in crate::cli) fn rss_kb() -> u64 {
    std::fs::read_to_string("/proc/self/smaps_rollup")
        .ok()
        .and_then(|text| {
            text.lines()
                .find(|line| line.starts_with("Rss:"))
                .and_then(|line| line.split_whitespace().nth(1))
                .and_then(|value| value.parse().ok())
        })
        .unwrap_or(0)
}

pub(in crate::cli) fn trace_rss(tag: &str) {
    if std::env::var_os("MIYU_SCREEN_TRACE").is_none() {
        return;
    }
    let note = format!("{tag} rss={}\n", rss_kb());
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("/tmp/miyu-screen-trace.log")
    {
        let _ = std::io::Write::write_all(&mut file, note.as_bytes());
    }
}

/// 正文区有多宽：左右各两列边距（左边那条是装订边）。`draw` 报给渲染器的、
/// 缓冲重排正文时用的，都是这个数。
pub(in crate::cli) fn content_cols(cols: u16) -> usize {
    usize::from(cols.saturating_sub(4).max(20))
}

impl Screen {
    pub(in crate::cli) fn enter() -> Result<Self> {
        trace_rss("screen-enter-before");
        let mut stdout = std::io::stdout();
        // 捕获鼠标：拖选、滚轮回翻都由程序接管。捕获的前提是自己真的实现了
        // 选区——只捕获不实现的话，连终端原生的拖选复制都会被夺走。
        // Shift+拖 仍然走终端原生（kitty 等终端的既定行为），留作后路。
        // 先藏光标再进副屏：副屏的光标初始在 (0,0) 且可见，第一帧画出来之前
        // 它会在左上角明晃晃地停一下。
        // 引导刚把备用屏交过来的话就不再进一次:再进会把上一帧清掉,闪一下。
        if miyu_base::terminal::take_held_alt_screen() {
            execute!(stdout, crossterm::cursor::Hide, EnableMouseCapture)?;
        } else {
            execute!(
                stdout,
                crossterm::cursor::Hide,
                EnterAlternateScreen,
                EnableMouseCapture
            )?;
        }
        // 渲染器从这一刻起给可折叠的块留展开内容。inline 下不开，字节流
        // 一个标记都不多。
        miyu_hosts::render::blocks::set_enabled(true);
        FULLSCREEN.store(true, std::sync::atomic::Ordering::Relaxed);
        let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
        // 宽度要在这儿就交给缓冲：`resize` 只在**尺寸变化**时才设，而初值
        // 就是真实尺寸，于是它一次都不会被调到，缓冲会一直按默认 80 折行。
        let mut term = Term::default();
        term.set_content_cols(content_cols(cols));
        term.set_cols(usize::from(cols));
        // 正文区的尺寸也得**现在**就登记。原来只在第一次 `paint` 时才存，而开
        // 全屏之后紧接着就是历史回放——那时 `content_viewport()` 还是 None，
        // 表格按整屏宽排，再加两格装订边就比屏幕宽一格，右边那根边框折到下一
        // 行的第 0 列（用户实测：真 TUI 里表格没有 inline 的效果好）。行数先按
        // 整屏减活动区估，第一帧 `paint` 会用真实的正文高度盖掉它。
        miyu_base::terminal::set_content_viewport(Some((
            content_cols(cols) as u16,
            rows.saturating_sub(6).max(4),
        )));
        Ok(Self {
            term,
            scroll: 0,
            follow: true,
            painted: Vec::new(),
            cols,
            rows,
            suspended: false,
            selection: None,
            pending_copy: None,
            needs_clear: true,
            overlay_spinner_started: None,
            body: None,
            expanded: std::collections::HashMap::new(),
            expanded_gen: 0,
            view_index: std::cell::RefCell::new(None),
            seed_stamp: None,
            open_seeded: std::collections::HashSet::new(),
            display_expand: (false, false),
            display_fold: true,
            display_command_lines: 8,
            display_thought_lines: 10,
            overlay: None,
            hover: None,
            input_rows: Vec::new(),
            input_selection: None,
            input_dragging: false,
            toast: None,
            floor: 0,
            older: None,
            row_keys: Vec::new(),
            command_hint: Vec::new(),
            hint_dismissed: false,
            force: true,
            banner: None,
            banner_shown: None,
            float_anchor: None,
        })
    }

    /// 诊断钩子，构造完之后调。
    /// 空会话 banner 的行(已经是带 SGR 的整行)。挂上/撤掉都要整屏重画一次:
    /// 正文区的 diff 键是按缓冲行号记的,和 banner 行对不上。
    /// 大厅里浮层往输入框下面摆,不贴屏底(屏底离输入框太远,读起来要跨半屏)。
    pub(in crate::cli) fn set_float_anchor(&mut self, anchor: Option<(u16, u16)>) {
        self.float_anchor = anchor;
    }

    /// 上一帧活动区盖过、这一帧盖不到的那几行：强制重画。
    ///
    /// 正文按行 diff，而活动区是 `paint` 之后才打上去的，`painted` 里记的
    /// 仍是「正文以为自己画了什么」。缩行时旧 footer 留在上面那一行，正文
    /// 那一行又恰好没变，diff 判「没变」直接跳过，旧 footer 就擦不掉。
    /// inline 那边靠 `suspend()` 先擦旧活动区，这是它的全屏版。
    pub(in crate::cli) fn invalidate_activity_rows(
        &mut self,
        previous: Option<(u16, u16)>,
        current: Option<(u16, u16)>,
    ) {
        for row in crate::cli::repl::input_layout::stale_activity_rows(previous, current) {
            let slot = usize::from(row);
            if self.painted.len() <= slot {
                self.painted.resize(slot + 1, String::new());
            }
            // 哨兵：任何真实行都不等于它，于是这一行必被重画。空串不行——
            // 空行画出来就是空串，diff 会认为「没变」（见 `force` 的注释）。
            self.painted[slot] = "\u{0}".into();
            if let Some(key) = self.row_keys.get_mut(slot) {
                *key = None;
            }
        }
    }

    /// `paint(tail_height)` 会把正文画到第几行为止。要在 `paint` **之前**
    /// 知道活动区落点的调用方用它（擦旧活动区要先知道新活动区在哪）。
    pub(in crate::cli) fn body_for(&self, tail_height: u16) -> u16 {
        self.body_height(tail_height)
    }

    pub(in crate::cli) fn set_banner(&mut self, rows: Option<BannerRows>) {
        if rows.is_some() != self.banner.is_some() {
            self.invalidate();
            self.banner_shown = None;
        }
        self.banner = rows;
    }

    pub(in crate::cli) fn trace_ready(&self) {
        trace_rss("screen-enter-after");
    }

    /// 一帧正文。字节原样交给终端模拟器——spinner 的原地刷新、命令块的
    /// 实时输出都靠它按光标动作落到对的行上。
    pub(in crate::cli) fn feed(&mut self, bytes: &[u8]) {
        // 图形传输段（kitty 的 APC `\x1b_G…\x1b\\`）是**发给终端**的指令，
        // 不是正文。塞进缓冲就等于被吞掉：屏幕上只剩占位符格子，图片、表情包、
        // LaTeX 块全变成一片空白或几个怪字符。
        //
        // 拆出来直接写终端，占位符格子留在缓冲里——它们是普通字符，跟着正文
        // 一起重画、回翻。传输段自己留一份，整屏擦之后要补发（擦掉的是放置，
        // 图还在终端里，但补一次最省心）。
        let bytes = self.take_graphics(bytes);
        self.term.feed(&bytes);
        if self.term.line_count() > MAX_LINES {
            let excess = self.term.line_count() - MAX_LINES;
            self.term.drop_front(excess);
            self.scroll = self.scroll.saturating_sub(excess);
            self.floor = self.floor.saturating_sub(excess);
            self.prune_expanded();
        }
    }

    /// 把图形传输段挑出来直接发给终端，返回剩下的（该进缓冲的）字节。
    ///
    /// 不留底、不补发：传输段用的是 kitty 的 **Unicode 占位符**（`U=1`），图交过去
    /// 之后是一个"虚拟放置"，画在哪儿由占位格说了算。整屏擦掉的只是那些格子，
    /// 重画一遍图就回来了——再传一次几百个分块纯属浪费。
    fn take_graphics(&mut self, bytes: &[u8]) -> Vec<u8> {
        let Some((graphics, rest)) = split_graphics(bytes) else {
            return bytes.to_vec();
        };
        use std::io::Write as _;
        let mut stdout = std::io::stdout();
        let _ = stdout.write_all(&graphics);
        let _ = stdout.flush();
        rest
    }

    pub(in crate::cli) fn resize(&mut self, cols: u16, rows: u16) {
        if (self.cols, self.rows) != (cols, rows) {
            self.cols = cols;
            self.rows = rows;
            // 先报正文区宽度再改屏幕宽度：`set_cols` 当场就会按新宽度重排，正文
            // 那几行要按正文区的宽度折（和 `draw` 里报给渲染器的是同一个数），
            // 不然重排出来的正文比新写进来的宽两格。
            self.term.set_content_cols(content_cols(cols));
            self.term.set_cols(usize::from(cols));
            self.resize_overlay(cols);
            self.invalidate();
            // 尺寸一变，终端自己会按新宽度把屏上的东西重排一遍，而我们的缓冲
            // 里存的是按**旧**宽度落下的行——逐行重画盖不住重排后多出来的残留，
            // 只能整屏擦一次（用户实测：改窗口大小之后满屏错位／泄漏）。
            self.needs_clear = true;
        }
    }

    /// 正文一共多少行。
    ///
    /// 就是视图长度——**不**把光标那一行额外算上。流式写到一半的那一行本来就有
    /// 字，已经在视图里了；光标比内容低的唯一情形是"正文末尾多打了两个换行"，
    /// 把那几行算成内容只会在屏幕底下空出一截。
    ///
    /// `floor` 是 Ctrl+L 顶上去的那一屏：空行不算内容（否则末尾几行空白会变成
    /// 正文和输入框之间的空档），所以"把视口顶空"得另记一笔。
    fn content_rows(&self) -> usize {
        self.view_len().max(self.floor)
    }

    /// 光标落在第几视图行之后。外部输出要接着这儿往下写。
    fn cursor_rows(&self) -> usize {
        self.content_rows()
            .max(self.view_of(self.term.cursor_row()) + 1)
    }

    /// 跟随时正文该滚到哪。
    ///
    /// 工具浮层是盖上去的，盖住谁谁就先看不见，不该把底下的东西
    /// 挤走（用户实测：点开浮层会把内容往上推）。按面板上方剩下的高度算的话，
    /// 等于开一次面板就把正文整体往上顶半屏。`paint`、`paint_overlay`、
    /// `overlay_click`、`scroll_above_panel` 四处共用它，口径不一致会互相拉扯。
    /// 提问面板则先缩小 `body`，正文与滚动上限一起让出空间。
    pub(in crate::cli) fn follow_target(&self) -> usize {
        self.content_rows().saturating_sub(usize::from(self.body()))
    }

    pub(in crate::cli) fn scroll_by(&mut self, delta: isize) {
        let body = usize::from(self.body());
        let max = self.content_rows().saturating_sub(body);
        let next = if delta < 0 {
            self.scroll.saturating_sub(delta.unsigned_abs())
        } else {
            self.scroll.saturating_add(delta as usize)
        };
        let next = next.min(max);
        // 自己滚回底部就恢复跟随，不用另设一个「回底」键。
        self.follow = next >= max;
        // 没动就不重画：到底之后再按 PgDn，原来每按一次都整屏重写一遍（用户
        // 09-17：「已经到页的底部了，pagedown 还是会有动画效果」）。
        if next == self.scroll {
            return;
        }
        self.scroll = next;
        self.invalidate();
    }

    /// 回到底部并恢复跟随。
    pub(in crate::cli) fn follow_bottom(&mut self) {
        self.follow = true;
        self.invalidate();
    }

    /// 鼠标移到了某个视图行上。返回真表示悬浮目标变了，要重画。
    pub(in crate::cli) fn hover_at(&mut self, index: Option<usize>) -> bool {
        let next = index
            .and_then(|index| self.block_at(index))
            .map(|(id, _)| id);
        if next == self.hover {
            return false;
        }
        self.hover = next;
        // 同选区：提亮只改那几行的内容，交给逐行 diff。
        true
    }

    pub(in crate::cli) fn hovered(&self) -> Option<u64> {
        self.hover
    }

    pub(in crate::cli) fn set_input_rows(&mut self, rows: Vec<(u16, String)>) {
        self.input_rows = rows;
    }

    /// 这一屏幕行是不是输入框的文字行。
    fn input_row_text(&self, row: u16) -> Option<&str> {
        self.input_rows
            .iter()
            .find(|(at, _)| *at == row)
            .map(|(_, text)| text.as_str())
    }

    /// 在输入区里按下。返回真表示这一下归输入区。
    pub(in crate::cli) fn input_select_begin(&mut self, column: u16, row: u16) -> bool {
        if self.input_row_text(row).is_none() {
            return false;
        }
        self.input_selection = Some(((row, column), (row, column)));
        self.input_dragging = true;
        self.invalidate();
        true
    }

    pub(in crate::cli) fn input_select_extend(&mut self, column: u16, row: u16) -> bool {
        if !self.input_dragging {
            return false;
        }
        let Some((anchor, _)) = self.input_selection else {
            return false;
        };
        // 只在输入区内部拖；拖出去就钉在最后一行上，别让选区断掉。
        let row = if self.input_row_text(row).is_some() {
            row
        } else {
            anchor.0
        };
        self.input_selection = Some((anchor, (row, column)));
        self.invalidate();
        true
    }

    /// 松手：把选中的字送进剪贴板。返回真表示这一下归输入区。
    ///
    /// 选区**留在屏上**。原来是 `take()` 掉——手一松反显就没了，看着像"刚选的
    /// 又被取消了"（用户实测）。正文那边的选区也是松手之后还在，两处该一致；
    /// 下一次按下会重新开一段，Esc 也清得掉。
    pub(in crate::cli) fn input_select_finish(&mut self) -> bool {
        if !self.input_dragging {
            return false;
        }
        self.input_dragging = false;
        let Some((anchor, cursor)) = self.input_selection else {
            return false;
        };
        self.invalidate();
        if anchor == cursor {
            self.input_selection = None;
            return true;
        }
        let (start, end) = if anchor <= cursor {
            (anchor, cursor)
        } else {
            (cursor, anchor)
        };
        let mut picked = Vec::new();
        for row in start.0..=end.0 {
            let Some(text) = self.input_row_text(row) else {
                continue;
            };
            let spans = ansi::parse_ansi_line(text);
            let from = if row == start.0 { start.1 } else { 0 };
            let to = if row == end.0 { end.1 } else { u16::MAX };
            picked.push(slice_columns(&spans, from, to, decoration_of(&spans)));
        }
        let text = picked.join(
            "
",
        );
        if !text.trim().is_empty() {
            self.pending_copy = Some(text);
        }
        true
    }

    /// 输入区里被选中的那一段，画出来要反白。
    pub(in crate::cli) fn input_selection_span(&self, row: u16) -> Option<(u16, u16)> {
        let (anchor, cursor) = self.input_selection?;
        let (start, end) = if anchor <= cursor {
            (anchor, cursor)
        } else {
            (cursor, anchor)
        };
        if row < start.0 || row > end.0 {
            return None;
        }
        let from = if row == start.0 { start.1 } else { 0 };
        let to = if row == end.0 { end.1 } else { u16::MAX };
        Some((from, to))
    }

    /// 视口停在第几行。命中测试要把屏幕行换算成视图行。
    pub(in crate::cli) fn scroll_of(&self) -> usize {
        self.scroll
    }

    pub(in crate::cli) fn cols(&self) -> u16 {
        self.cols
    }

    /// 展开/收起之后把跟随状态放回去。
    pub(in crate::cli) fn restore_follow(&mut self, following: bool) {
        if following {
            self.follow = true;
        } else {
            self.refresh_follow();
        }
    }

    /// 按「视口是不是已经贴底」重算跟随。展开/收起之后用——内容长短变了，
    /// 跟随与否得跟着重判，不能写死。
    pub(in crate::cli) fn refresh_follow(&mut self) {
        let body = usize::from(self.body());
        let max = self.content_rows().saturating_sub(body);
        self.follow = self.scroll >= max;
    }

    /// 把视口推空：往正文里补一屏空行，滚到底。
    ///
    /// 这是 Ctrl+L 该有的样子——和终端 `clear` 一个意思，**内容没删**，
    /// 只是顶上去了，往回翻还能看到。
    pub(in crate::cli) fn push_blank_screen(&mut self) {
        // 记一条地板：视口要停在内容**之后**整整一屏的位置。光靠灌空行不行——
        // 空行不算内容（见 `content_rows`），灌完视图长度一点没变。
        self.floor = self
            .view_len()
            .saturating_add(usize::from(self.body()))
            .max(self.floor);
        let blanks = vec![b'\n'; usize::from(self.rows)];
        self.term.feed(&blanks);
        self.follow = true;
        self.invalidate();
    }

    /// 会话清空了，画布也清空：正文缓冲整个丢掉，下一句话从第 0 行起。
    ///
    /// 和 `push_blank_screen`（Ctrl+L）不一样：那个是把视口顶空、往回翻还在；
    /// 这里是 `/reset`、`/new` 回到大厅——旧对话已经不属于这个会话了，留着的话
    /// 下一句话会接在它后面、出现在屏底而不是屏顶（09-14 用户实测）。
    pub(in crate::cli) fn wipe_transcript(&mut self) {
        // 上一条会话还没画的更早部分跟着一起丢：换了会话再往上翻，不能补出别的会话来。
        self.older = None;
        self.term = Term::default();
        self.term.set_cols(usize::from(self.cols));
        self.scroll = 0;
        self.follow = true;
        self.floor = 0;
        self.expanded.clear();
        self.expanded_gen = self.expanded_gen.wrapping_add(1);
        self.open_seeded.clear();
        self.hover = None;
        self.selection = None;
        self.pending_copy = None;
        // 行缓存按 (行号, 时间戳) 记，新缓冲的时间戳从 0 重来，会和旧的撞上。
        self.row_keys.clear();
        self.invalidate();
        self.needs_clear = true;
    }

    /// `/undo`：把最后一轮从画布上截掉，前面的滚动历史原样留着。缓冲里没有
    /// 轮标记（这一屏不是本进程画出来的）就返回假，调用方再走整段回放。
    pub(in crate::cli) fn truncate_last_turn(&mut self) -> bool {
        let Some(start) = self.term.pop_turn_start() else {
            return false;
        };
        self.term.truncate_rows(start);
        self.prune_expanded();
        self.floor = self.floor.min(start);
        self.hover = None;
        self.selection = None;
        self.pending_copy = None;
        self.row_keys.clear();
        self.follow = true;
        self.invalidate();
        true
    }

    /// 撤掉压缩:最后那块「上下文已压缩」从缓冲里截掉(它得是缓冲里最后一样东西,
    /// 后面又有一轮的话不动,返回 false)。前面的行、块、滚动历史原样留着。
    pub(in crate::cli) fn truncate_last_compact(&mut self) -> bool {
        let Some(start) = self.term.pop_trailing_compact_start() else {
            return false;
        };
        self.term.truncate_rows(start);
        self.prune_expanded();
        self.floor = self.floor.min(start);
        self.hover = None;
        self.selection = None;
        self.pending_copy = None;
        self.row_keys.clear();
        self.follow = true;
        self.invalidate();
        true
    }

    /// 下一帧全量重画。
    fn invalidate(&mut self) {
        self.painted.clear();
        self.force = true;
    }

    fn body_height(&self, tail_height: u16) -> u16 {
        self.rows.saturating_sub(tail_height).max(1)
    }

    /// 上一帧正文占了多少行。活动区高度由调用方给，`Screen` 只能记下来——
    /// 回翻上限、点选行号都得按**同一个**正文高算，各算各的就会差几行。
    pub(in crate::cli) fn body(&self) -> u16 {
        self.body.unwrap_or_else(|| self.body_height(0))
    }

    /// 视口有没有停在历史中间——停住时新内容不该把用户拽回底部。
    pub(in crate::cli) fn following(&self) -> bool {
        self.follow
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        miyu_base::terminal::set_content_viewport(None);
        // 只有真进过全屏才还原终端。`swap` 兼作闸：测试里构造的 `Screen`
        // 没进过 alt screen，往真 stdout 吐一串还原序列会把 `cargo test`
        // 的输出弄脏，也会真的把别人的鼠标捕获关掉。
        if !FULLSCREEN.swap(false, std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        miyu_hosts::render::blocks::set_enabled(false);
        let mut stdout = std::io::stdout();
        // 同理，回主屏那一下也别让光标先跳到左上角：inline 那边接手后会把它
        // 放到该在的位置再显示出来。
        let _ = execute!(
            stdout,
            crossterm::cursor::Hide,
            DisableMouseCapture,
            LeaveAlternateScreen
        );
    }
}

#[cfg(test)]
mod test_support;
