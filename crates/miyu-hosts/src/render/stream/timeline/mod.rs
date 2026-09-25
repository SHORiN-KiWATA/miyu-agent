//! 全屏 TUI 的过程时间线。
//!
//! inline REPL 把一轮里的工具和思考压成「一行摘要 + 详情行」，因为它没有回翻、
//! 没有点击，展开了就再也收不回去。全屏有屏幕也有鼠标，于是换成时间线：
//!
//! ```text
//!   │ ⚙ 运行命令 · 2.4s
//!   │ ✳ 已思考 · 320 词元 · 7.5s
//! ```
//!
//! 一段连续的过程结束（模型开始说正文／回合结束／面板要抢屏）就**收成一行**：
//!
//! ```text
//!   › Worked for 12.3s · 3 tools · 2 thoughts · 1 err
//! ```
//!
//! 这一行是可展开块，展开出来就是上面那条时间线；时间线里每一项**又**是可展开
//! 块，点开是那个工具的完整输出或那段思考的全文。嵌套由 `screen/expand.rs` 负责，
//! 这里只管把块标记按层套好。
//!
//! 全屏下是这套**可展开**的时间线（[`crate::render::blocks::enabled`]）。
//!
//! 不是全屏、但 stdout 是个终端的那些形态——shellhook、单次 `miyu "…"`——走同一条
//! 时间线的**静态**版（[`StreamRenderer::timeline_static`]）：长相一样，只是没有
//! 鼠标也没有回翻，所以没什么可展开的。每一步跑完就直接落进 scrollback，能展开的
//! 东西（补丁 diff、命令输出的尾巴）就地印在那一步底下；live 区只留一根连线和
//! 正在跑的那一行；也不写 `Worked for …` 收缩行——点不开的把手只是一行废话。
//!
//! 只有 stdout 不是终端（管道）时才还是老的一行摘要。
//!
//! 一共几个面、各自谁在用，见 [`super::surface`] 的表。

mod cross_session;
mod glyphs;
mod live;
mod panel_preview;
mod question;
mod stall;
mod subagent;
mod thought_rows;

// 后台子代理面板（根包 `cli::repl::tail::screen::overlay`）要按名字用这几样：
// 它和前台面板**共用**排版、收缩、点开的规则，取数的地方不同，长相不该不同。
pub use cross_session::write_cross_session_message;
pub(crate) use cross_session::SEND_TOOL;
pub use glyphs::{fold_open_lines, step_detail_lines, step_rows};
pub use panel_preview::{panel_command_tail, panel_thought_window};
pub use subagent::{fold_block_lines, thread_panel, PanelEntry};

use super::{question_answer_text, StreamRenderer};
use crate::render::blocks;
use crate::render::t;
use crate::render::ReasoningDisplayMode;
use crate::render::{prompt_glyph, THOUGHT_BODY_STYLE};
use std::time::{Duration, Instant};

// 搬走的帮手按老路径再导出：调用方写的还是 `timeline::…`（09-16 拆分）。
use glyphs::step_detail;
pub(crate) use glyphs::{tool_glyph, tool_output_lines};
use live::indented_body;
pub(crate) use live::undecorate;
pub use live::{indent_body, peek_tail, render_speech_lines, summary_line, write_compact_summary};
pub(crate) use subagent::SubagentLog;
pub(crate) use thought_rows::{style_thought_rows, ThoughtRows};

/// 竖线。它和 logo **同在一列**：logo 是这一步的节点，竖线是节点之间的连线，
/// 各占一行。分成两列的话左边会多出一根从头贯到尾的栏杆，那是画框不是时间线。
const RAIL: &str = "│";
/// 全屏下时间线整体的左缩进：第 0–1 列是页边距，正文、用户消息都从第 2 列起。
const INDENT: &str = "  ";

/// 这一刻时间线该缩进多少。
///
/// 两种形态都退两格。静态版一度贴着第 0 列（和原来那块 `~ 工具×1 ok` 卡片同一个
/// 位置），用户看了说整体太靠左、直接贴到边框了——时间线是"过程"，比正文退一步
/// 才读得出主次。
fn indent() -> &'static str {
    INDENT
}

/// 静态时间线里一步底下那几行的前缀：连线穿过去。
///
/// 原来正文那几行是缩进四格、上下各空一行——连线在每一步的正文处断掉，一屏
/// 看下来时间线是碎的（用户实测截图「timeline 断得很严重」）。竖线贯穿正文、
/// 不空行，一眼就能看出这几行属于上面那一步。
fn rail_prefix() -> String {
    format!("\x1b[2m{}{RAIL}\x1b[0m ", indent())
}
/// 图标用 Nerd Font 的字形（私有区）。
///
/// 之前那套 `⚙ ✎ ▤ ⌕` 是从通用符号里凑的：粗细、基线、留白各不相同，排在一列
/// 里参差不齐。Nerd Font 的图标是**同一套字体里画的**，一列排下来才齐。
///
/// 装不了 Nerd Font 的话设 `MIYU_TUI_ASCII=1` 退回通用符号——图标好看不该是
/// 用不了的理由。
pub fn nerd() -> bool {
    static NERD: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *NERD.get_or_init(|| std::env::var_os("MIYU_TUI_ASCII").is_none())
}

/// 认不出的工具。芯片——没归到哪一类，那就是"有个东西在跑"。
pub fn glyph_tool() -> &'static str {
    if nerd() {
        "\u{f4bc}"
    } else {
        "⚙"
    }
}

/// 出错。**错比「是什么工具」更要紧**，所以它盖过按类型挑的图标。
pub fn glyph_err() -> &'static str {
    if nerd() {
        "\u{f00d}"
    } else {
        "✗"
    }
}

/// 通知（后台任务完成之类）。铃铛：它不是某一步，是"有件事发生了"。
pub fn glyph_notice() -> &'static str {
    if nerd() {
        "\u{f0f3}"
    } else {
        "⚙"
    }
}

/// 重启续跑（09-24）：daemon 重启打断了上一轮，这一轮接着做。转圈的箭头。
pub fn glyph_restart() -> &'static str {
    if nerd() {
        "\u{f021}"
    } else {
        "↻"
    }
}

/// 思考。原子——脑子里的东西在转，灯泡那个更像"想到了"。
///
/// （一度以为截图里那个方框是缺字、把它换掉了，其实那**就是**原子那个字形。
/// 这台机器的字体里 MDI 段是全的，别再改。）
pub fn glyph_think() -> &'static str {
    if nerd() {
        "\u{f0768}"
    } else {
        "✳"
    }
}

/// 从工具统计里摘出来、还没成形的一步。
struct PendingStep {
    name: String,
    display: String,
    peek: Option<String>,
    /// 子代理烧了多少（短标）。见 `StreamRenderer::subagent_tokens_label`。
    tokens: Option<String>,
    detail: Vec<String>,
    /// 抬头底下留着的那几行。见 [`Step::tail`]。
    tail: Vec<String>,
    failed: bool,
    /// 收进来的时候还没跑完——只有回合被打断（Ctrl+C、断线）才会这样。
    interrupted: bool,
    elapsed: Option<Duration>,
    overlay: Option<u64>,
}

/// 一步：折叠时的那一行，加上点开能看到的正文。

/// 这一步**是什么**——由事件决定，不是看图标猜、也不是靠两个布尔编码。
///
/// **主线、前台面板、后台面板三处共用这一个枚举。** 之前它有两份:这边是
/// `speech: bool` + `fold: bool`(两个布尔编码三种互斥状态,「都为真」在类型上
/// 合法),后台面板那边是另一个同名枚举。后台那份的来历是个真 bug:那儿原来拿
/// **图标**判类型,而 `load_tools` 的图标和「差事」撞了,于是装工具那一步在面板里
/// 出现两遍、输出还丢了。
///
/// 两块面板的模型要合一,得先说同一种话(报告 §6 阶段 3)。
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub enum StepKind {
    /// 没打标签的裸行（后台流水账里认不出前缀的那些）。
    #[default]
    Plain,
    /// 交给子代理的差事。钉在最前面，不参与收缩。
    Prompt,
    /// 想的那一段。
    Thought,
    /// 一次工具调用。后台面板里**只有它**认领 `[结果]` 与 `[输出]`。
    Tool,
    /// 它说的一段正文：没有抬头，也不连线，`body` 就是整段话。按时序占位，
    /// 后面再想再动手也排不到它前头。
    Speech,
    /// 后台流水账里的 `[统计]`。不是工具调用：没有结果行，也不该被当成
    /// 「末尾那个还没回来的调用」挂上转轮。
    Stats,
    /// 收缩行：`body` 是收起来的那几步（各自已经是整行、带缩进、连好线），点开时
    /// 不再缩进——和主线 `Worked for …` 展开成时间线一个样子。
    Fold,
}

pub struct Step {
    kind: StepKind,
    line: String,
    body: Vec<String>,
    /// 子代理：点开是覆盖层而不是就地展开，用的是它自己那块流水账的 id。
    overlay: Option<u64>,
    /// 就地展开那一块的 id。**收进时间线那一刻就登记**，live 区和收缩之后用的是
    /// 同一个 id——原来只有收成 `Worked for …` 时才登记，于是回合还没结束时已经
    /// 跑完的那几步一个都点不开（用户实测：diff 要等 AI 输出完才看得到）。
    block: Option<u64>,
    /// 不点开也露在抬头底下的那几行。**只有命令那一步用**（用户 09-17 指名：
    /// 抬头给 title、底下给命令）。块的结束标记放在它们之后：点开时展开内容
    /// 把抬头和尾巴一起换掉。
    ///
    /// 它曾经还兼过第二份差事——「显示思考过程 / 显示工具调用信息 = 完整」时
    /// 把详情挂在抬头底下当预览。那是**第四种形态**，谁都没设计过：那一行明明
    /// 还能点，点开看到的又是几乎同一份内容（用户 09-17：「不应该以 tag 行下
    /// 预览的形式出现 tag 行的内容」）。现在那一档走 [`Step::open`]。
    tail: Vec<String>,
    /// 这一步出来就是**展开态**：块照常登记、照常能点（再点一次收回去），只是
    /// 视图第一次见到它时先替用户开一次。`显示思考过程 / 显示工具调用信息 =
    /// 完整` 落的就是这一位。
    ///
    /// 不能点开的面（shellhook、单次、管道）没有"展开态"可言：那儿这一位由
    /// `body` 印不印在抬头底下来表达，见 `commit_static_steps`。
    open: bool,
    /// 上一位的**来历**：用户亲手点开的（真），还是按配置默认开着的（假）。
    ///
    /// 两者在屏幕上一个样，收段时不一样：用户亲手点开过的，`Worked for …` 不能
    /// 把它一起收没（用户 09-19）；配置默认开着的照旧收起来——不然开了「展开
    /// 思考内容」的人每一段都会得到一个摊开的收缩行。
    user_open: bool,
}

impl Step {
    pub(crate) fn new(line: String, body: Vec<String>, overlay: Option<u64>) -> Self {
        Self {
            kind: StepKind::Plain,
            line,
            body,
            overlay,
            block: None,
            tail: Vec::new(),
            open: false,
            user_open: false,
        }
    }

    /// 从**已经渲染好的**抬头与正文造一步。后台面板用它：那边的抬头要按面板
    /// 宽度裁、正文要按面板宽度折，渲染时机和前台不同，但落进来的形状该是同一个。
    ///
    /// `block` 是这一步以前用过的块 id（按位置复用）——后台每帧重解析，不带着它
    /// 的话用户点开的那一块下一帧就换了 id、当场合上。
    pub fn panel(kind: StepKind, line: String, body: Vec<String>, block: Option<u64>) -> Self {
        Self {
            kind,
            line,
            body,
            overlay: None,
            block,
            tail: Vec::new(),
            open: false,
            user_open: false,
        }
    }

    /// 这一步登记到了哪一块（造完之后才知道，见 `fold_block_lines`）。
    pub fn block_id(&self) -> Option<u64> {
        self.block
    }

    pub fn set_block(&mut self, id: Option<u64>) {
        self.block = id;
    }

    /// 抬头底下露着的那几行。见 [`Step::tail`]——只有命令那一步用。
    pub fn set_tail(&mut self, tail: Vec<String>) {
        self.tail = tail;
    }

    pub fn tail(&self) -> &[String] {
        &self.tail
    }

    /// 这一步出来就是展开态吗。见 [`Step::open`]。
    pub fn set_open(&mut self, open: bool) {
        self.open = open;
    }

    pub fn open(&self) -> bool {
        self.open
    }

    pub fn kind(&self) -> StepKind {
        self.kind
    }

    pub fn line(&self) -> &str {
        &self.line
    }

    pub(crate) fn speech(body: Vec<String>) -> Self {
        Self {
            kind: StepKind::Speech,
            line: String::new(),
            body,
            overlay: None,
            block: None,
            tail: Vec::new(),
            open: false,
            user_open: false,
        }
    }
}

/// 抬头后面带上耗时：`已思考 · 2.6s`。不到十分之一秒的不带——`0.0s` 只是噪音
///（用户实测）。
pub(crate) fn timed_label(head: &str, elapsed: Duration) -> String {
    match reported_seconds(elapsed) {
        Some(secs) => format!("{head} · {secs}"),
        None => head.to_string(),
    }
}

/// 一步花了多久。始终报，不到一秒报毫秒（用户 09-17：加上 ms 的读秒，而不是仅 s）。
///
/// 原来对不到十分之一秒的耗时**什么都不报**，于是同一段代码两次跑可能一次带耗时
/// 一次不带——写快照时它是结构性的不确定，掩码救不了；读起来也像「这步没花时间」。
pub fn reported_seconds(elapsed: Duration) -> Option<String> {
    Some(crate::render::format_reasoning_elapsed(elapsed))
}

/// 面板里「正在进行」那一行左边距上的转轮占位格。
///
/// 面板内容是一段静态 ANSI，没人每帧重写它；画面板的那一层每一帧把这个格子换成
/// 当帧的点阵字形，转轮就转起来了。选私有区末尾的码位：不会和任何文字撞上，
/// 宽度也是一格。
pub const LIVE_SPINNER_CELL: char = '\u{10FFFD}';

/// 面板里正在进行的那一行：转轮占位在第 0 列，logo 留在第 2 列——和主线一样。
pub fn panel_live_step_line(glyph: &str, text: &str) -> String {
    let text = crate::render::clip_to_display_width(text, panel_step_width());
    format!("\x1b[2m{LIVE_SPINNER_CELL} {glyph} {text}\x1b[0m")
}

/// live 区里「正在进行」的一行。
#[derive(Debug)]
pub(crate) struct LiveRow {
    pub(crate) line: String,
    /// 点开去哪儿：就地那一块，或者子代理的面板。
    pub(crate) target: Option<u64>,
    /// 这一行底下跟着露出来的几行（静态时间线里跑着的命令露出来的输出尾巴）。
    pub(crate) tail: Vec<String>,
    /// 出来就是展开态。见 [`Step::open`]——**这一行也要这一位**：
    /// 「展开」说的是「这一步的内容默认看得见」，而一步的大半辈子是在 live 区里
    /// 度过的。只给落下来的那一份的话，用户看到的是「想完了才展开、跑完了才展开」
    ///（用户 09-17 实测原话）。
    pub(crate) open: bool,
}

/// 收缩行的图标。和主线那条 `⌄ Worked for …` 一个样子。
const SUMMARY_GLYPH: &str = "⌄";

/// 收缩行**合着**的时候的图标：`› Worked for …`。点开之后（块内容的第一行）才是
/// `⌄`——主线那条就是这么翻的，面板里原来一直是 `⌄`，合着开着一个样
///（用户实测：Worked for 左侧箭头异常）。
const FOLD_GLYPH_CLOSED: &str = "›";

pub fn fold_glyph_closed() -> &'static str {
    FOLD_GLYPH_CLOSED
}

/// 收缩行点开之后的抬头：`›` 换成 `⌄`。
pub fn fold_line_open(line: &str) -> String {
    line.replacen(FOLD_GLYPH_CLOSED, SUMMARY_GLYPH, 1)
}

/// 一段连续的过程。
#[derive(Default)]
pub(crate) struct Timeline {
    started: Option<Instant>,
    /// 这一段里每一步**自己**花掉的时间之和。见 [`Timeline::elapsed`]。
    spent: Duration,
    steps: Vec<Step>,
    /// 静态时间线：前多少步已经落进 scrollback 了。live 区只画这之后的。
    /// 全屏下一直是 0——那儿整段都留在 live 区里，收缩时一起写。
    committed: usize,
    counts: Counts,
}

impl Timeline {
    /// 记下这一段过程的起点，`spent` 是这一步**自己**已经花掉的时间。
    ///
    /// 起点不是"这一步被收进来的那一刻"——步是跑完才收的，两者正好差出这一步的
    /// 耗时。一轮只想了一次就交卷时，这个差就是整段思考，摘要于是报出刺眼的
    /// `Worked for 0.0s`：看着像这一轮瞬间就完了。
    ///
    /// `now` 是收进来的这一刻：补发的事件按它自己发生的时刻（`event_clock.rs`）。
    fn note_start_since(&mut self, spent: Duration, now: Instant) {
        let at = now.checked_sub(spent).unwrap_or(now);
        if self.started.is_none_or(|existing| at < existing) {
            self.started = Some(at);
        }
        self.spent = self.spent.saturating_add(spent);
    }

    /// 只把起点往前挪到 `at`，不记耗时。
    ///
    /// 给「步收进来的时刻」晚于「这一步结束的时刻」的那种收法用：按
    /// [`Self::note_start_since`] 从此刻倒推会把起点算晚，中间那截空档就从
    /// `Worked for` 里漏掉了。
    fn note_started_at(&mut self, at: Instant) {
        if self.started.is_none_or(|existing| at < existing) {
            self.started = Some(at);
        }
    }

    fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// 这一段过程花了多久。
    ///
    /// 平时就是墙上时间。**回放时墙上时间是零**——整段是一瞬间喂完的，于是
    /// `Worked for` 那一截整个消失，重开之后只剩 `1 tool · 2 thoughts`
    ///（用户实测对比图）。回放时每一步自己带着耗时，累加起来就是这一段的下限，
    /// 取两者的大者：实时不受影响，回放拿得回那个数。
    fn elapsed(&self, now: Instant) -> Duration {
        let wall = self
            .started
            .map(|at| now.saturating_duration_since(at))
            .unwrap_or_default();
        wall.max(self.spent)
    }
}

/// 秒数。亚秒给一位小数（`0.3s`），进了分钟就换成 `1m 02s`——
/// 「跑了多久」这件事在不同量级上关心的精度不一样。
pub(crate) use miyu_base::durations::format_seconds;

/// 命令的单行窥视:跟着 `tool_peek` 一族归位到工具层,这里保持老路径。
pub(crate) use miyu_engine::tools::command_peek;

/// 一步的详情最多留多少行。再多就不是「点开看看」而是把内存当日志用了。
const MAX_DETAIL_LINES: usize = 400;

/// 展开内容相对页边距再缩进多少。
const DETAIL_INDENT: &str = "  ";

/// 展开内容能用多宽。
///
/// **折行得自己折**：交给缓冲硬折的话，续行从第 0 列开始，冒到页边距外面去，
/// 看着就是"左边莫名其妙多出半个字"。所以内容在这儿就按这个宽度折好，
/// 每一行都自带缩进。
pub(crate) fn detail_width() -> usize {
    crate::render::command_terminal_width()
        .saturating_sub(indent().len() + DETAIL_INDENT.len() + 1)
        .max(20)
}

/// 一段纯文本按 `detail_width()` 折行。返回的每一行都**没有**缩进
/// （缩进由 `step_detail` 统一加，免得两处各加一次）。
pub(crate) fn wrap_detail(text: &str) -> Vec<String> {
    let width = detail_width();
    text.lines()
        .flat_map(|line| {
            if line.trim().is_empty() {
                return vec![String::new()];
            }
            crate::render::wrap_display_text(line, width)
        })
        .collect()
}

/// 边想边往下流的那一段思考的记账。
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ThoughtStream {
    /// 正文折好的物理行里，前多少行已经滚进 scrollback。按物理行记而不按字节：
    /// 一次只滚「多出来的那几行」，live 区高度稳在一屏上下，转轮才不上下跳；
    /// 按逻辑行滚的话一段十行的话一次滚十行，转轮跟着跳十行。
    pub(crate) flushed_rows: usize,
    /// 抬头「思考中」滚进 scrollback 了没。还在的话转轮挂在它上面、计数实时。
    pub(crate) heading_flushed: bool,
}

/// 正在想的正文最多在 live 区里占几行：整屏减去转轮行、连线与页边距的余量。
pub(crate) fn live_thought_rows() -> usize {
    crate::render::terminal_rows(24).saturating_sub(3).max(4)
}

/// 出错那一步：整行红色。
///
/// 只换图标不够——一屏暗色里多一个小记号根本扫不到，而"哪一步失败了"正是
/// 回头翻这条时间线时最想先看见的。
fn step_line_failed(glyph: &str, text: &str) -> String {
    step_line_failed_in(glyph, text, step_width())
}

fn step_line_failed_in(glyph: &str, text: &str, width: usize) -> String {
    let text = crate::render::clip_to_display_width(text, width);
    format!("\x1b[31m{}{glyph} {text}\x1b[0m", indent())
}

/// 一步：`  <glyph> <text>`。glyph 占的就是竖线那一列。
/// 抬头和窥视之间的分隔。
///
/// 原来是两个空格，和「名字 · 秒数」那半截的 `·` 不是一个写法，同一行上
/// 两种分隔读起来就是断句不齐（用户实测，指着「差事」和「已思考」两行说的）。
pub const PEEK_SEP: &str = " · ";

/// 时间线上一行能占多宽。
fn step_width() -> usize {
    crate::render::command_terminal_width()
        .saturating_sub(indent().len() + 3)
        .max(8)
}

/// 子代理面板里一步能占多宽。
///
/// 面板没有竖线，左右只各留两列——正好和正文那条装订边同宽，所以能占的宽度
/// 和主线时间线一样。留两列富余：图标是 Nerd Font 字形，某些终端把它算成两列。
/// 见 `cli::repl::tail::screen::overlay::panel_inner_width`。
fn panel_step_width() -> usize {
    step_width().saturating_sub(2).max(8)
}

fn step_line(glyph: &str, text: &str) -> String {
    step_line_in(glyph, text, step_width())
}

/// 整行裁到给定宽度：窥视可长可短，让它把一行挤成两行的话，时间线的竖线就
/// 对不上列了（续行从第 0 列开始）。
fn step_line_in(glyph: &str, text: &str, width: usize) -> String {
    let text = crate::render::clip_to_display_width(text, width);
    format!("\x1b[2m{}{glyph} {text}\x1b[0m", indent())
}

/// 面板里一步那一行。两种子代理面板（前台走事件、后台读日志）共用它——
/// 取数的地方不同，**长相必须是同一份代码**。
pub fn panel_step_line(glyph: &str, text: &str, failed: bool) -> String {
    if failed {
        step_line_failed_in(glyph, text, panel_step_width())
    } else {
        step_line_in(glyph, text, panel_step_width())
    }
}

/// 面板里两步之间的连线。见 [`panel_step_line`]。
pub fn panel_rail() -> String {
    rail()
}

/// 面板里一步点开之后是什么样。见 [`panel_step_line`]。
pub fn panel_step_detail(line: &str, body: &[String]) -> Vec<String> {
    let mut detail = Vec::with_capacity(body.len() + 3);
    detail.push(line.to_string());
    detail.extend(indented_body(body));
    detail
}

/// 面板里一步的**抬头**能占多宽：整行宽度减掉图标那一格和它后面的空格。
pub fn panel_step_width_for_head() -> usize {
    panel_step_width().saturating_sub(2)
}

/// 面板里正文那一层能用多宽（折行用）。
pub fn panel_detail_width() -> usize {
    detail_width()
}

/// 两步之间的连线：`  │`。
fn rail() -> String {
    format!("\x1b[2m{}{RAIL}\x1b[0m", indent())
}

/// 把若干步骤行用连线串起来。
fn thread(steps: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut out = Vec::new();
    for line in steps {
        if !out.is_empty() {
            out.push(rail());
        }
        out.push(line);
    }
    out
}

impl StreamRenderer {
    /// 有没有时间线：全屏的可展开版，或者普通终端里的静态版。
    ///
    /// `blocks::enabled()` 就是「全屏后端在驱动」的信号，不另设一个会和它走散
    /// 的开关。
    pub fn timeline_enabled(&self) -> bool {
        blocks::enabled() || self.timeline_static()
    }

    /// 静态时间线：不是全屏、但 stdout 是个终端（shellhook、单次 `miyu "…"`）。
    ///
    /// `live_summary` 就是「stdout 是终端」——管道里没有转轮也没有回翻，那儿
    /// 还是老的一行摘要。
    ///
    /// **这里原来还挂着 `tool_call_mode == Summary`。** 那是渲染统一没收干净的
    /// 尾巴：档位一调成「完整」，非全屏这条线整条时间线直接消失、退回旧卡片面，
    /// 连带 `captures_reasoning()` 也变假——于是「显示思考过程」那个开关在这条
    /// 路上一点用都没有（用户 09-17：「非全屏和全屏的路径不是已经统一了吗，
    /// 为什么你还在分」）。
    ///
    /// 统一之后档位**只决定内容默认看不看得见**，不决定走哪条路：这条线上
    /// 「完整」= 正文就地印在抬头底下，「摘要」= 只有抬头。
    pub fn timeline_static(&self) -> bool {
        !blocks::enabled() && self.live_summary && !self.plain
    }

    /// 工具跑完了：收成一步。详情是那个工具的完整块。
    pub(crate) fn timeline_push_tools(&mut self) -> anyhow::Result<()> {
        if self.tool_stats.is_empty() {
            return Ok(());
        }
        let caps = self.caps();
        let static_timeline = caps.detail_inline();
        // 「显示工具调用信息 = 完整」= 这一步的内容默认看得见。能点开的面上
        // 是**出来就展开**（`Step::open`），点不开的面上是就地印在抬头底下。
        // 两边都不再往抬头底下挂一截预览（用户 09-17）。
        let full_details = self.tool_call_mode == crate::render::ToolCallDisplayMode::Full;
        let open_by_default = full_details && caps.expandable;
        // 先收集再改：`ordered_tool_stats` 借着 `self`，循环里要往 `self.timeline`
        // 里写，借用检查过不去。
        // 「给外部输出让路」那一次收尾:还没返回的工具留着,别收。它还在跑——
        // 收进来就是一步红色的「已中断」,而真结果回来时又会记第二次
        //(用户 09-19:shellhook 里一次发图两行报错)。
        let hold_unsettled = self.finalizing_for_external_output;
        let entries: Vec<PendingStep> = self
            .ordered_tool_stats()
            .into_iter()
            .filter(|(_, stats)| !hold_unsettled || stats.settled())
            .map(|(name, stats)| PendingStep {
                name: name.to_string(),
                display: self.display_tool_name(name),
                // 子代理不给窥视：名字里已经带着描述了（`开发中·画鹅鹅骑车`），
                // 再把 `subject` 当窥视就是同一句话说两遍（用户截图实录）。
                peek: (!crate::render::is_subagent_tool(name))
                    .then(|| {
                        stats
                            .peek
                            .as_deref()
                            .or(stats.subject.as_deref())
                            .map(str::to_string)
                    })
                    .flatten(),
                tokens: self.subagent_tokens_label(name),
                // 真实输出优先：`tool_block_lines` 只是「跑没跑成」的统计，
                // 点开却看不到工具到底吐了什么，收起来就等于丢了。
                detail: if !stats.detail.is_empty() {
                    stats.detail.clone()
                } else if static_timeline && !full_details {
                    // 点不开的面 + 摘要档：主题那一行已经挂在抬头上了，统计那几行
                    // 就地印出来只是把时间线撑长。完整档要的就是它们，所以只在
                    // 摘要档丢。
                    Vec::new()
                } else {
                    // 丢掉 `tool_block_lines` 的表头（`名字×1 err` 那行）：
                    // 时间线那一步已经写了名字、耗时、成没成——展开之后又来一遍，
                    // 而且那一行的图标是通用齿轮、颜色也不是红的，看着像"展开之后
                    // 就不是报错的样子了"（用户原话）。
                    undecorate(
                        self.tool_block_lines(name, stats, false)
                            .into_iter()
                            .skip(1)
                            .collect(),
                    )
                },
                // 没跑完就被收进来 = 回合被打断了。它没成功，但也不是"报错"——
                // 抬头上要说清楚。
                tail: stats.tail.clone(),
                failed: stats.error > 0 || !stats.settled(),
                interrupted: !stats.settled(),
                // 不到十分之一秒的不报（`0.0s` 只是噪音）；交到后台的子代理是
                // 立刻返回的，它的秒数不是它干活的时间，也不报。
                elapsed: stats
                    .elapsed()
                    .filter(|elapsed| reported_seconds(*elapsed).is_some() && !stats.detached),
                overlay: crate::render::is_subagent_tool(name)
                    .then(|| self.subagent_overlay_id(name))
                    .flatten(),
            })
            .collect();
        // 这一批工具最早也是 `max(各自耗时)` 之前开始的——并发跑的话取最大值是
        // 唯一稳妥的下界，串行跑的话它也不会比真实起点晚太多。
        let spent = entries
            .iter()
            .filter_map(|entry| entry.elapsed)
            .max()
            .unwrap_or_default();
        let now = self.event_now();
        self.timeline.note_start_since(spent, now);
        // 清单是一段的句点:表落在收缩行之后、新一段之前(对齐 WebUI)。
        let ends_segment = entries.iter().any(|e| e.name == "todowrite" && !e.failed);
        for PendingStep {
            name,
            display,
            peek,
            tokens,
            detail,
            failed,
            interrupted,
            elapsed,
            overlay,
            tail,
        } in entries
        {
            let glyph = if failed {
                glyph_err()
            } else {
                tool_glyph(&name)
            };
            // 先名字、再秒数，窥视挂最后——秒数是这一步的度量，窥视是内容，
            // 夹在中间读起来像是「运行命令 ls -la 花了 2.4 秒」的断句错位。
            let mut label = match (tokens.as_deref(), elapsed) {
                (Some(tokens), Some(elapsed)) => {
                    format!("{display} · {tokens} · {}", format_seconds(elapsed))
                }
                (Some(tokens), None) => format!("{display} · {tokens}"),
                (None, Some(elapsed)) => format!("{display} · {}", format_seconds(elapsed)),
                (None, None) => display,
            };
            if interrupted {
                label.push_str(" · ");
                label.push_str(t("interrupted", "已中断"));
            }
            if let Some(peek) = peek {
                label.push_str(PEEK_SEP);
                label.push_str(&peek);
            }
            self.timeline.counts.record_tool(&name, failed);
            let line = if failed {
                step_line_failed(glyph, &label)
            } else {
                step_line(glyph, &label)
            };
            let mut step = Step::new(line, detail, overlay);
            step.kind = StepKind::Tool;
            // 命令那一步抬头底下露的那几行命令（用户指名的行数）——这是唯一
            // 一处尾巴，和档位无关。
            step.tail = tail;
            // 子代理点开是覆盖层，默认开着没有意义（也没处开）。
            // 用户在 live 区亲手点开过这个工具那一行,跑完同样不收回去。
            step.user_open = self
                .live_tool_blocks
                .get(&name)
                .copied()
                .is_some_and(blocks::user_open);
            step.open = (open_by_default || step.user_open) && overlay.is_none();
            self.timeline.steps.push(step);
        }
        // 留着的那几个(还在跑)不能清:它们的结果回来时要落在自己的统计上,
        // 不然 `calls` 归零、`settled()` 永远是假,又变成一步「已中断」。
        if hold_unsettled {
            self.tool_stats.retain(|_, stats| !stats.settled());
        } else {
            self.tool_stats.clear();
        }
        self.last_tool_summary.clear();
        self.live_block = None;
        self.live_tool_blocks.clear();
        // 只记意图:五个调用方各有各的后续动作(三个自己会 `cut_timeline`,两个
        // 紧接着重挂 live 区),就地收段会和它们打架。切段交回 `settle_tool_batch`。
        self.timeline_ends_after_tools |= ends_segment;
        self.settle_new_steps()
    }

    /// 刚收进来的那几步：全屏下登记成块（live 区里就能点开），静态版直接落进
    /// scrollback。
    fn settle_new_steps(&mut self) -> anyhow::Result<()> {
        if self.caps().commit_immediately {
            return self.commit_static_steps();
        }
        for step in &mut self.timeline.steps {
            if step.block.is_none() && step.overlay.is_none() && !step.body.is_empty() {
                step.block = blocks::register(step_detail(step));
                // 这一步是用户亲手点开的:换的这块新的也得带上这个来历,收段时
                // `Worked for …` 才知道不能把它收没(用户 09-19)。
                if step.user_open {
                    if let Some(id) = step.block {
                        blocks::set_user_open(id, true);
                    }
                }
            }
        }
        Ok(())
    }

    /// 静态时间线：把还没落地的步骤写进 scrollback。
    ///
    /// 每一步跑完就落，live 区只留连线和正在跑的那一行——全屏那种"整段留在
    /// live 区里、结束时一起收"在这儿不成立：没有回翻、没有点击，一段 diff
    /// 留在每帧重画的 live 区里只会闪，而且超过一屏就擦不干净了。
    fn commit_static_steps(&mut self) -> anyhow::Result<()> {
        use std::io::Write as _;
        let from = self.timeline.committed;
        if from >= self.timeline.steps.len() {
            return Ok(());
        }
        // 转轮先收掉：它那几行还留在屏上的话，新落的步骤会写在它们中间。擦和写
        // 裹在一个同步输出块里，终端一次成帧，不露出「擦了还没写」的空当。
        self.begin_synchronized()?;
        self.stop_waiting()?;
        // 能点开的面（全屏 + 「不自动收起过程」）走这儿时**照样挂块标记**：
        // 「不自动收起」说的只是段末不写 `Worked for …`，不该顺手把每一步变成
        // 点不开、正文铺一地（用户 09-17：「即使不自动收起过程为 true，也不
        // 应该以 tag 行下预览的形式出现 tag 行的内容」）。
        //
        // 这一点我上一轮写反过：`caps()` 里那段注释还记着当时的证据（`s4-full-
        // open.ansi` 只有 5 个块标记）。那不是设计，是 `commit_immediately` 一位
        // 同时管了三件事的副产品。
        let expandable = self.caps().expandable;
        let mut out = String::new();
        let prefix = rail_prefix();
        for offset in 0..self.timeline.steps.len() - from {
            let index = from + offset;
            if index > 0 {
                out.push_str(&rail());
                out.push('\n');
            }
            if expandable {
                let step = &self.timeline.steps[index];
                let id = step.overlay.or(step.block).or_else(|| {
                    (!step.body.is_empty()).then(|| blocks::register(step_detail(step)))?
                });
                if step.overlay.is_none() {
                    self.timeline.steps[index].block = id;
                }
                out.push_str(&step_rows(&self.timeline.steps[index], id));
                out.push('\n');
                continue;
            }
            let step = &self.timeline.steps[index];
            out.push_str(&step.line);
            out.push('\n');
            // 正文紧贴抬头、每一行都从连线穿过，不空行——见 `rail_prefix`。
            // 点不开的面上，「完整」那一档要看的内容就摆在这儿。
            for line in &step.body {
                out.push_str(&prefix);
                out.push_str(line);
                out.push('\n');
            }
        }
        let stdout = &mut self.output;
        write!(stdout, "{out}")?;
        self.end_synchronized()?;
        self.timeline.committed = self.timeline.steps.len();
        // 这一步干出来的结果（todo 表、图片占位）紧跟着它。
        self.flush_after_timeline()
    }

    /// 同步输出块的两头：擦转轮、落正文、重起转轮之间不让终端画中间态。
    pub(crate) fn begin_synchronized(&mut self) -> anyhow::Result<()> {
        if self.sync_depth == 0 {
            crossterm::queue!(self.output, crossterm::terminal::BeginSynchronizedUpdate)?;
        }
        self.sync_depth += 1;
        Ok(())
    }

    pub(crate) fn end_synchronized(&mut self) -> anyhow::Result<()> {
        use std::io::Write as _;
        self.sync_depth = self.sync_depth.saturating_sub(1);
        if self.sync_depth == 0 {
            crossterm::queue!(self.output, crossterm::terminal::EndSynchronizedUpdate)?;
            self.output.flush()?;
        }
        Ok(())
    }

    pub(crate) fn timeline_push_thought(&mut self) -> anyhow::Result<()> {
        if self.reasoning_title.is_none() && self.reasoning_text.trim().is_empty() {
            return Ok(());
        }
        let now = self.event_now();
        let elapsed = self
            .reasoning_elapsed
            .or_else(|| {
                self.reasoning_started_at
                    .map(|at| now.saturating_duration_since(at))
            })
            .unwrap_or_default();
        self.timeline.note_start_since(elapsed, now);
        let mut label = t("thought", "已思考").to_string();
        if self.reasoning_tokens > 0 {
            label = format!(
                "{label} · {} {}",
                self.reasoning_tokens,
                t("tokens", "词元")
            );
        }
        let label = timed_label(&label, elapsed);
        // 边想边落地的那一段：正文大半已经在 scrollback 里了，只剩半行和末尾那行计数。
        if let Some(stream) = self.thought_stream.take() {
            return self.finish_streamed_thought(stream, &label);
        }
        // 点不开的面 + 摘要档：思考全文就不留了——抬头上的词元数和秒数说明
        // "想过"，想了什么本来也只是折叠起来备查的。完整档要的正是那份全文，
        // 于是它就地印在抬头底下（`commit_static_steps`）。
        let full_reasoning = self.reasoning_mode == ReasoningDisplayMode::Full;
        let caps = self.caps();
        let detail = if caps.detail_inline() && !full_reasoning {
            Vec::new()
        } else {
            style_thought_rows(self.thought_rows_all())
        };
        // 「显示思考过程 = 完整」= 这一步的内容默认看得见。能点开的面上是
        // **出来就是展开态**（再点一次收回去）；点不开的面上是就地印在抬头
        // 底下。它曾经是往抬头底下挂一截预览——那一行明明还能点，点开看到的
        // 又是几乎同一份内容（用户 09-17 拍掉了这种形态）。
        self.timeline.counts.thoughts += 1;
        let mut step = Step::new(step_line(glyph_think(), &label), detail, None);
        step.kind = StepKind::Thought;
        // 用户在 live 区亲手点开过这一步,想完就不该把它收回去——「亲手点开」
        // 比「默认怎么显示」优先(用户 09-19)。落地换的是新的一块,所以要把
        // 手上那块的开合带过来。
        step.user_open = self.live_block.is_some_and(blocks::user_open);
        step.open = (full_reasoning || step.user_open) && caps.expandable;
        self.timeline.steps.push(step);
        self.clear_reasoning_phase();
        self.settle_new_steps()
    }

    /// 点不开的面 + 完整档：思考正文要不要边想边往下流。
    ///
    /// 那个面的 live 区是每帧「上移 → 擦 → 重画」的，高不过一屏；回复正文能一直
    /// 往下流，是因为它写进 scrollback 就不回头。思考照这个办：抬头带着转轮留在
    /// live 区顶上、正文在它底下长，整段高过一屏时最上面的行（先是抬头，再是最老
    /// 的整行）滚进 scrollback——和全屏里块高过视口、抬头滚出去是一个意思
    /// （用户 09-17：「没法做到一直往下流式输出吗？」「转轮要固定在思考的 logo
    /// 左侧」）。全屏面的块能原地改，不走这条路。
    pub(crate) fn thought_streams_inline(&self) -> bool {
        self.reasoning_mode == ReasoningDisplayMode::Full
            && self.captures_reasoning()
            && self.caps().detail_inline()
    }

    /// 每来一条思考 delta：开始记账，整段高过一屏就往上滚。
    pub(crate) fn stream_thought_progress(&mut self) -> anyhow::Result<()> {
        if !self.thought_streams_inline() {
            return Ok(());
        }
        if self.thought_stream.is_none() {
            self.thought_stream = Some(ThoughtStream::default());
        }
        self.trim_streamed_thought()
    }

    /// 整段高过一屏时，把最上面的行滚进 scrollback：先是抬头，再是最老的整行
    /// （只滚到换行为止——折行是按词断的，半行的折法还会变，落早了改不着）。
    fn trim_streamed_thought(&mut self) -> anyhow::Result<()> {
        use std::io::Write as _;
        let Some(mut stream) = self.thought_stream else {
            return Ok(());
        };
        let max_rows = live_thought_rows();
        loop {
            let total = self.thought_row_count();
            let live =
                total.saturating_sub(stream.flushed_rows) + usize::from(!stream.heading_flushed);
            if live <= max_rows {
                break;
            }
            if !stream.heading_flushed {
                // 抬头滚出去：落地的这份不带计数——计数还在涨，落了就改不着，
                // 想完由末尾那一行报（`finish_streamed_thought`）。
                let mut step = Step::new(
                    step_line(glyph_think(), t("thinking", "思考中")),
                    Vec::new(),
                    None,
                );
                step.kind = StepKind::Thought;
                self.timeline.steps.push(step);
                stream.heading_flushed = true;
                self.thought_stream = Some(stream);
                // 落地和重起转轮在同一个同步块里：中间那个「擦了还没画」的空当不露出来。
                self.begin_synchronized()?;
                self.commit_static_steps()?;
                self.ensure_waiting_phase(self.reasoning_live_text(), self.wait_style())?;
                self.end_synchronized()?;
                continue;
            }
            // 只滚多出来的那几行；末尾两行永远留着——折行按词断，还在写的那一行
            // 可能把上一行的尾词拽下来，再往上的行已经定了。
            let stable = total.saturating_sub(2);
            let flushable = stable.saturating_sub(stream.flushed_rows);
            if flushable == 0 {
                break;
            }
            let count = (live - max_rows).min(flushable);
            let from = stream.flushed_rows;
            let prefix = rail_prefix();
            let committed = style_thought_rows(self.thought_rows_range(from, count))
                .iter()
                .map(|line| format!("{prefix}{line}"))
                .collect::<Vec<_>>();
            // 首选就地交接：这几行本来就画在 live 区顶上，抹掉转轮字形、从账上划走
            // 就是了，一行不擦。办不到（软折行、转轮不在）才擦了重画——而且落地和
            // 重起转轮在同一个同步块里，不露空当。
            let width = crate::render::terminal_cols(120);
            let in_place = match self.wait_spinner.as_mut() {
                Some(spinner) => {
                    spinner.commit_leading_rows(&mut self.output, &committed, width)?
                }
                None => false,
            };
            if !in_place {
                self.begin_synchronized()?;
                self.stop_waiting()?;
                let mut out = String::new();
                for line in &committed {
                    out.push_str(line);
                    out.push('\n');
                }
                let stdout = &mut self.output;
                write!(stdout, "{out}")?;
                self.ensure_waiting_phase(self.reasoning_live_text(), self.wait_style())?;
                self.end_synchronized()?;
            }
            stream.flushed_rows += count;
            self.thought_stream = Some(stream);
        }
        self.thought_stream = Some(stream);
        Ok(())
    }

    /// 想完了。抬头还在 live 区就按原版式落地（`已思考 · N 词元 · Xs` 当抬头、
    /// 正文在它底下）；抬头已经滚出去了，就把剩下的正文落地、末尾收一行计数。
    fn finish_streamed_thought(
        &mut self,
        stream: ThoughtStream,
        label: &str,
    ) -> anyhow::Result<()> {
        use std::io::Write as _;
        let rest = style_thought_rows(self.thought_rows_range(stream.flushed_rows, usize::MAX));
        self.timeline.counts.thoughts += 1;
        if !stream.heading_flushed {
            let mut step = Step::new(step_line(glyph_think(), label), rest, None);
            step.kind = StepKind::Thought;
            self.timeline.steps.push(step);
            self.clear_reasoning_phase();
            return self.settle_new_steps();
        }
        self.begin_synchronized()?;
        self.stop_waiting()?;
        let prefix = rail_prefix();
        let mut out = String::new();
        for line in &rest {
            out.push_str(&prefix);
            out.push_str(line);
            out.push('\n');
        }
        out.push_str(&prefix);
        out.push_str(&format!("\x1b[2m{label}\x1b[0m"));
        out.push('\n');
        let stdout = &mut self.output;
        write!(stdout, "{out}")?;
        self.end_synchronized()?;
        self.clear_reasoning_phase();
        self.flush_after_timeline()
    }

    /// 这一段思考收完了：计数、正文、起点全清，等下一段。
    fn clear_reasoning_phase(&mut self) {
        self.reasoning_text.clear();
        self.thought_rows.borrow_mut().clear();
        self.reasoning_tokens = 0;
        self.reasoning_title = None;
        self.reasoning_started_at = None;
        self.reasoning_elapsed = None;
        self.thought_stream = None;
        self.live_block = None;
    }

    /// 回放：把某一步真实花掉的时间喂回去。
    ///
    /// 回放是一瞬间喂完的，墙上时间是零——`Worked for …` 那一截于是整个消失
    ///（用户实测对比图：重开前 `Worked for 6.6s · 1 tool · 2 thoughts`，
    /// 重开后只剩 `1 tool · 2 thoughts`）。把起点往回倒，后面的计时照常走，
    /// 连带那一步自己那行的 `· 1.2s` 也一并回来了。
    pub fn replay_tool_elapsed(&mut self, name: &str, elapsed: std::time::Duration) {
        if elapsed.is_zero() {
            return;
        }
        let now = self.event_now();
        let stats = self.tool_stats_entry(name);
        stats.started_at = now.checked_sub(elapsed);
    }

    /// 同 [`Self::replay_tool_elapsed`]，这一段思考想了多久。
    pub fn replay_reasoning_elapsed(&mut self, elapsed: std::time::Duration) {
        if elapsed.is_zero() {
            return;
        }
        self.reasoning_elapsed = Some(elapsed);
    }

    /// 收尾：把这一段连续过程压成一行 `Worked for …`，并把整条时间线挂成它的
    /// 展开内容。时间线里的每一项**自己也是块**，于是能再点开看详情。
    /// 把攒着的"结果"放出来。时间线收完才轮到它们——见
    /// [`StreamRenderer::pending_after_timeline`]。
    pub(crate) fn flush_after_timeline(&mut self) -> anyhow::Result<()> {
        use std::io::Write as _;
        if self.pending_after_timeline.is_empty() {
            return Ok(());
        }
        let pending = std::mem::take(&mut self.pending_after_timeline);
        let stdout = &mut self.output;
        // 上面空一行。这一块（图、清单表）是**正文**，不是时间线的一部分：
        // 紧贴着收缩行的话，图看着像是从 `Worked for …` 那一行长出来的
        //（用户 09-19：全屏下图和上面那行之间缺一行空）。段尾那行空由调用
        // 方按老规矩出，所以这儿只管上面。
        writeln!(stdout)?;
        let mut ends_with_newline = true;
        for chunk in pending {
            write!(stdout, "{chunk}")?;
            ends_with_newline = chunk.ends_with('\n');
        }
        // 自己不带收尾换行的块（万一）别把段尾那行空吃掉。
        if !ends_with_newline {
            writeln!(stdout)?;
        }
        stdout.flush()?;
        Ok(())
    }

    /// 把一段"结果"排到时间线后面去。
    pub fn queue_after_timeline(&mut self, text: String) {
        self.pending_after_timeline.push(text);
    }

    pub(crate) fn cut_timeline(&mut self) -> anyhow::Result<()> {
        use std::io::Write as _;
        self.timeline_ends_after_tools = false; // 段收了,意图作废
        if self.timeline.is_empty() {
            self.timeline = Timeline::default();
            // 没有时间线可收，攒着的结果也没有理由再等。
            return self.flush_after_timeline();
        }
        // live 区先收掉：不收的话它那几行留在屏上，摘要会接在它们下面，
        // 于是「收缩」看起来根本没发生。
        self.stop_waiting()?;
        // 逐步落地的面没有收缩行(用它而不是 `!fold`:后者在管道面下与旧条件不等价)。
        if self.caps().commit_immediately {
            // 静态版：步骤早就一步一步落下去了，这里只是这一段到此为止——
            // 空一行和后面的正文分开。没有 `Worked for …`：点不开的把手只是
            // 一行废话。
            self.commit_static_steps()?;
            // 段尾空一行。上一步欠的那一行空就是它，不再多空一行。
            self.timeline = Timeline::default();
            let stdout = &mut self.output;
            writeln!(stdout)?;
            stdout.flush()?;
            return self.flush_after_timeline();
        }
        let now = self.event_now();
        let timeline = std::mem::take(&mut self.timeline);
        let summary = summary_line(timeline.elapsed(now), timeline.counts);
        // 展开内容：头行 + 用连线串起来的每一步（各自包成块）。
        let mut steps = Vec::with_capacity(timeline.steps.len());
        for step in &timeline.steps {
            if step.overlay.is_none() && step.body.is_empty() {
                steps.push(step_rows(step, None));
                continue;
            }
            // 展开这一步时**头行留着**：它是把手，再点一次才收得回去；
            // 正文缩进到竖线右边，和折叠态对得上列。
            let target = match step.overlay {
                // 子代理直接挂它那块流水账：点开是覆盖层。
                Some(id) => Some(id),
                // live 区里用的那块，收缩之后还是它：点开着的保持点开。
                None => step.block.or_else(|| blocks::register(step_detail(step))),
            };
            steps.push(step_rows(step, target));
        }
        // 段尾那行空跟着这一段的最后一样东西走:有清单表就留到表后面。
        let result_follows = !self.pending_after_timeline.is_empty();
        let mut expanded = vec![format!("\x1b[2m{INDENT}⌄ {summary}\x1b[0m")];
        expanded.push(rail());
        expanded.extend(thread(steps));
        expanded.extend((!result_follows).then(String::new));
        // 这一段里只要还有用户亲手点开着的步,收段时就不能把它们一起收没:
        // 收缩行自己出来就是展开态,里面那一步照旧开着(用户 09-19)。
        let keep_open = timeline.steps.iter().any(|step| {
            step.user_open || step.block.or(step.overlay).is_some_and(blocks::user_open)
        });
        let stdout = &mut self.output;
        blocks::write_expandable_in(stdout, expanded, keep_open, |writer| {
            writeln!(writer, "\x1b[2m{INDENT}› {summary}\x1b[0m")?;
            if result_follows {
                return Ok(());
            }
            writeln!(writer)
        })?;
        stdout.flush()?;
        self.flush_after_timeline()?;
        if result_follows {
            writeln!(self.output)?;
        }
        self.output.flush()?;
        Ok(())
    }
}

/// 这一段里各做了多少件事。工具按类分开数(09-24:摘要写成
/// `Ran 3 commands · 2 edits · 4 tools`),分类的真相源是
/// `miyu_base::tool_names::tool_kind`。
#[derive(Clone, Copy, Default)]
pub struct Counts {
    pub commands: usize,
    pub edits: usize,
    /// 既不是命令也不是改文件的工具。
    pub tools: usize,
    pub thoughts: usize,
    pub errors: usize,
}

impl Counts {
    pub fn record_tool(&mut self, name: &str, failed: bool) {
        use miyu_base::tool_names::{tool_kind, ToolKind};
        match tool_kind(name) {
            ToolKind::Command => self.commands += 1,
            ToolKind::FileEdit => self.edits += 1,
            ToolKind::Other => self.tools += 1,
        }
        if failed {
            self.errors += 1;
        }
    }

    pub fn acted(&self) -> bool {
        self.commands + self.edits + self.tools > 0
    }
}

#[cfg(test)]
mod test_support;
