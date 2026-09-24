//! 覆盖层：盖住整屏看一段内容，Esc 关掉。
//!
//! 子代理是它存在的理由——子代理里面自己还在流（它的思考、它调的工具），塞进
//! 正文里就地展开会把主线冲散，而且它**还在长**，就地展开的行数每秒都在变、
//! 视口跟着抖。盖一层就没这些问题：主线原封不动，面板里自己滚。
//!
//! 内容取自 `render::blocks` 的登记处，每帧按版本号看要不要重取——于是「边跑
//! 边看」是白拿的：渲染方往那块里灌，面板下一帧就跟上。

use super::ansi::spans_to_ansi;
use super::expand::{layer_hit, layer_len, layer_row, Body, Expanded, Layer};
use super::Screen;
use crate::cli::t;
use crossterm::{
    cursor::MoveTo,
    queue,
    style::Print,
    terminal::{Clear, ClearType},
};
use miyu_hosts::render::blocks;
use miyu_hosts::render::timeline::StepKind;
use std::io::Write;

mod log;
mod screen;

use log::*;

/// 面板的内容从哪来。
enum Source {
    /// 渲染方登记的一块（子代理的内层流水账）。
    Block { id: u64, version: u64 },
    /// 盘上的日志文件（后台任务）。任务跑在 daemon 里，日志本来就落盘，
    /// 直接读比再造一条 IPC 分页通道省事。
    File {
        path: std::path::PathBuf,
        size: u64,
        /// 上一次重读是什么时候。子代理一边跑一边写，文件每帧都在长，帧帧重读
        /// 重排（含正文的 markdown 渲染）把画面板的那一帧拖慢，转轮就一顿一顿。
        last_reload: Option<std::time::Instant>,
    },
}

/// 日志文件在长的时候最快多久重读一次。
const LOG_RELOAD_INTERVAL: std::time::Duration = std::time::Duration::from_millis(150);

/// 日志最多读末尾多少字节。后台任务能跑很久，整个读进来没意义。
const LOG_TAIL_BYTES: u64 = 256 * 1024;

/// 面板左右各留几列。留白之外还有个作用：一眼看得出面板到哪儿为止。
const PANEL_MARGIN: u16 = 2;

/// 面板里真正能写字的宽度：屏幕宽减掉左右留白。
///
/// **面板里的一切都按它算**——排版按整屏宽算的话，行会长出去，画的时候再硬裁
/// 一刀，右边就参差不齐（用户实测：右侧边框有些 broken）。
fn panel_inner_width(cols: u16) -> usize {
    usize::from(cols)
        .saturating_sub(usize::from(PANEL_MARGIN) * 2)
        .max(8)
}

/// 上下各留一行空白。
///
/// 不留的话最后一行贴着按键提示、第一行贴着标题，读起来像是内容被框夹住了
///（用户原话：最后一行离底部的按键提示太近了，顶部也是）。
const PANEL_PAD: u16 = 1;

/// 框线 + 上下留白一共占几行。
const PANEL_CHROME: u16 = 2 + PANEL_PAD * 2;
const DIM: &str = "\x1b[2m";
const RESET: &str = "\x1b[0m";

/// 面板的上下两条横线：`── 标题 ──────── 右边那串 ──`。
///
/// **只有上下，没有左右，也没有圆角**（用户拍板）。左右两根竖线并不解释任何
/// 东西——面板占满整行，上下两条线已经说清楚它从哪到哪；竖线只是让每一行都少
/// 两列可用宽度，还逼着内容再裁一刀。
///
/// 整条线连同标题、按键提示**一律暗色**：它是取景框，不是内容。
fn frame_line(width: usize, label: &str, trailing: Option<&str>) -> String {
    let tail = trailing.map(|text| format!(" {text} ")).unwrap_or_default();
    let tail_width = miyu_hosts::render::visible_width(&tail);
    let label_room = width.saturating_sub(tail_width + 8);
    let label = miyu_hosts::render::clip_to_display_width(label, label_room.max(4));
    let label_width = miyu_hosts::render::visible_width(&label);
    let fill = width.saturating_sub(label_width + tail_width + 6).max(1);
    format!("{DIM}── {label} {}{tail}──{RESET}", "─".repeat(fill))
}

pub(in crate::cli) struct Overlay {
    source: Source,
    /// 跑的是哪条命令。标题栏那个 title 是短标签(模型给的话只有 16 字符),
    /// 看不出真正在跑什么,所以日志上面原样铺一行(用户 09-14)。
    command: String,
    /// 这个面板讲的是哪个后台任务。有值才允许按 x 停。
    job_id: Option<String>,
    /// 画面宽度，重新解析内容时要用。
    cols: usize,
    title: String,
    body: Body,
    /// 面板里自己的展开状态。子代理内层也是一条时间线，那里每一步同样能点开，
    /// 而它的开合和正文那边互不相干，所以各带各的表。
    expanded: Expanded,
    /// 「默认开着」的块里已经替用户开过的那些。面板每帧重解析，不记着的话
    /// 收起来下一帧就被顶开。见 `screen::expand::seed_open`。
    open_seeded: std::collections::HashSet<u64>,
    /// `展开思考内容` / `展开工具内容`，由 `Screen` 交进来（见
    /// `Screen::set_display_expand`）。面板自己是从日志/标记流攒步的，手上没有
    /// 配置入口。
    display_expand: (bool, bool),
    /// `过程收起成一行摘要`。见 `display_expand` 那段——同一条路交进来。
    display_fold: bool,
    /// `命令显示行数`。同上。
    display_command_lines: usize,
    /// `思考滚动窗行数`：正在想的那一步底下露最近几行。同上。
    display_thought_lines: usize,
    /// 正在跑的那一步是从什么时候开始的（抬头上那个计时）。
    ///
    /// 标记流和流水账里都没有时间戳，跑完那一步的耗时是结果行带回来的；**跑着
    /// 的时候没人给**，于是面板上那一行一直没有计时，而主线是有的（主线自己
    /// 掐表）。这儿照主线的办法自己掐：抬头变了就重新计时。
    running_since: Option<(String, std::time::Instant)>,
    /// 鼠标停在面板里哪一块上。可点的东西要看得出来「这里能点」——正文那侧
    /// 一直有（`Screen::hover`），面板这侧原来整个没有：`Moved` 和别的鼠标事件
    /// 一起被吞掉了（用户 09-17：「浮层的 tag 行没有悬浮变色的效果」）。
    hover: Option<u64>,
    /// 面板里的选区。行号是**面板自己的内容行**（`Overlay::row` 那套），不是
    /// 正文缓冲的绝对行——面板是另一张画布，滚动也是自己的。
    ///
    /// 面板原来整个不做选区（`tail_impl` 里那句「其余吞掉」），而面板里装的正是
    /// 最想复制走的东西：子代理的输出、后台命令的日志（用户 09-17：「这样的浮层
    /// 无法选中文字」）。
    selection: Option<super::select::Selection>,
    scroll: usize,
    /// 停在底部就跟着新内容走；自己往回翻过就别再拽他。
    follow: bool,
    /// 每一步对应的块 id，按位置复用。见 `render_log`。
    step_blocks: Vec<u64>,
    /// 收缩行里每一步的块 id，按 `(收缩行位置, 步位置)` 复用。见 `fold_detail`。
    fold_blocks: std::collections::HashMap<(usize, usize), u64>,
    /// 面板占多高。**开着的时候只涨不缩**。
    ///
    /// 按当前内容每帧重算的话，后台任务每写一行日志面板就长高一点、上边沿
    /// 跟着往上跳——AI 正在输出时日志一秒写十几行，面板就一直在抖
    /// （用户原话「后台命令再究极鬼畜…浮层的位置也不对」）。
    height: u16,
}

impl Overlay {
    fn from_block(id: u64, cols: usize) -> Option<Self> {
        let lines = blocks::get(id)?;
        Some(Self {
            source: Source::Block {
                id,
                version: blocks::version(id),
            },
            job_id: None,
            cols,
            title: blocks::title(id).unwrap_or_else(|| t("detail", "详情").to_string()),
            command: String::new(),
            body: parse_body(&lines, cols),
            expanded: Expanded::new(),
            open_seeded: std::collections::HashSet::new(),
            display_expand: (false, false),
            display_fold: true,
            display_command_lines: 8,
            display_thought_lines: 10,
            running_since: None,
            hover: None,
            selection: None,
            scroll: 0,
            follow: true,
            height: 0,
            step_blocks: Vec::new(),
            fold_blocks: std::collections::HashMap::new(),
        })
    }

    fn from_file(
        path: std::path::PathBuf,
        title: String,
        job_id: Option<String>,
        command: String,
        cols: usize,
        // 第一次排版就要按档位来：构造函数里已经读了一遍日志，晚一步交进来的话
        // 面板刚打开那一帧是收着的，下一帧才展开——闪一下。
        display_expand: (bool, bool),
        display_fold: bool,
        display_command_lines: usize,
        display_thought_lines: usize,
    ) -> Self {
        let mut panel = Self {
            source: Source::File {
                path,
                size: 0,
                last_reload: None,
            },
            job_id,
            title,
            command,
            body: parse_body(&[], cols),
            cols,
            expanded: Expanded::new(),
            open_seeded: std::collections::HashSet::new(),
            display_expand,
            display_fold,
            display_command_lines,
            display_thought_lines,
            running_since: None,
            hover: None,
            selection: None,
            scroll: 0,
            follow: true,
            height: 0,
            step_blocks: Vec::new(),
            fold_blocks: std::collections::HashMap::new(),
        };
        panel.reload_file(true);
        panel
    }

    /// 屏幕宽变了：面板里的一切按新宽度重排一遍。
    ///
    /// 不重排的话，框跟着新宽度画、内容还按旧宽度折，两边就对不上了。
    fn set_cols(&mut self, cols: usize) {
        if self.cols == cols {
            return;
        }
        self.cols = cols;
        self.expanded.clear();
        self.open_seeded.clear();
        // 宽度变了内容要重排，按行列记的选区会指到别处去。
        self.selection = None;
        match &mut self.source {
            Source::Block { id, version } => {
                let id = *id;
                *version = blocks::version(id);
                if let Some(lines) = blocks::get(id) {
                    self.body = parse_body(&lines, cols);
                }
            }
            Source::File { .. } => self.reload_file(true),
        }
    }

    fn block_id(&self) -> Option<u64> {
        match &self.source {
            Source::Block { id, .. } => Some(*id),
            Source::File { .. } => None,
        }
    }

    fn file_path(&self) -> Option<&std::path::Path> {
        match &self.source {
            Source::File { path, .. } => Some(path.as_path()),
            Source::Block { .. } => None,
        }
    }

    /// 这个面板讲的后台任务还在跑吗：daemon 那条路查轮询线程的任务表，直连模式查
    /// 本进程的任务表。不是任务面板、表里没有它，都算停了。
    fn job_running(&self) -> bool {
        let Some(job_id) = self.job_id.as_deref() else {
            return false;
        };
        match crate::cli::repl::jobs::feed() {
            Some(feed) => feed
                .jobs
                .lock()
                .is_ok_and(|jobs| jobs.iter().any(|job| job.job_id == job_id && job.running)),
            None => miyu_engine::tools::jobs::overview()
                .iter()
                .any(|job| job.job_id == job_id && job.running),
        }
    }

    fn reload_file(&mut self, force: bool) {
        // 面板开着就告诉轮询线程「我在看这个任务」，它顺带把原始标记流拉回来。
        // 标记流不受日志那道「按自然段落盘」的闸限制——实测那道闸能让面板整整
        // 12 秒不动（`testkit/tui/bg_latency.py`）。
        let traced = self.job_id.clone().and_then(|job_id| {
            let feed = crate::cli::repl::jobs::feed()?;
            *feed.trace_job.lock().ok()? = Some(job_id.clone());
            let trace = feed.trace.lock().ok()?;
            match trace.as_ref() {
                Some((id, markers, _)) if *id == job_id && !markers.is_empty() => {
                    Some(markers.clone())
                }
                _ => None,
            }
        });
        let Source::File {
            path,
            size,
            last_reload,
        } = &mut self.source
        else {
            return;
        };
        if !force && last_reload.is_some_and(|last| last.elapsed() < LOG_RELOAD_INTERVAL) {
            return;
        }
        let current = std::fs::metadata(&*path)
            .map(|meta| meta.len())
            .unwrap_or(0);
        // 标记流在手时不看文件大小：日志半天不动，标记流可能已经走了好几步。
        if !force && traced.is_none() && current == *size {
            return;
        }
        *size = current;
        *last_reload = Some(std::time::Instant::now());
        let lines = match traced {
            // 有标记流就走它（路 B）。攒步照旧无状态重算——省的是「等段落」那份
            // 延迟，不是重算那点开销。
            Some(markers) => {
                let events = markers
                    .iter()
                    .flat_map(|marker| miyu_engine::tools::subagent::protocol::from_marker(marker));
                let steps = log::steps_from_events(events, self.display_fold);
                self.render_steps(steps)
            }
            // 退路（路 A）：daemon 重启后内存里的 trace 就没了，老任务也只有日志;
            // 后台**命令**任务从来不走标记流。
            None => {
                let text = read_tail(path, LOG_TAIL_BYTES);
                self.render_log(&text)
            }
        };
        self.body = parse_body(&lines, self.cols);
        // 展开着的那几块留着，只是把内容换成新的——同 `refresh` 里那条注释：
        // 一刷新就整张清掉的话，刚点开的东西立刻自己缩回去。
        super::expand::reload_expanded(&mut self.expanded, self.cols);
        self.seed_open();
    }

    /// 「完整」那一档的步：面板里同样是出来就展开，不用点。
    fn seed_open(&mut self) {
        super::expand::seed_open(
            &Layer::Body(&self.body),
            &mut self.expanded,
            &mut self.open_seeded,
            self.cols,
        );
    }

    /// 把流水账渲染成一条**能点开**的时间线。
    ///
    /// 每一步登记成一块（`blocks`），行里带上标记，面板自己那张展开表就认得它。
    /// 块 id **按位置复用**：日志是只增的，第 i 步永远是第 i 步；每次重读都新登记
    /// 一批的话，登记处几秒就被刷爆，而且用户点开的那一块会在下一次刷新时变成
    /// 另一个 id、当场合上。
    fn render_log(&mut self, text: &str) -> Vec<String> {
        let indent = "  ";
        // 后台**命令**的日志就是一堆输出行，没有"步"可言——按时间线排会变成
        // 每行一个节点、行行之间一条连线，那是把日志排成了梯子。原样折行就好。
        // 认标签用**协议那一份清单**，别在这儿手抄：抄的那份漏了 `[准备]`，
        // 后来又漏了 `[思考+]` / `[正文+]`——只落了这几种标签的日志（子代理刚起头
        // 就在准备工具、或者读到的那一截尾巴全是续截）会被判成"没有步"，整块原样
        // 铺出来，**整条时间线当场消失**（用户 09-17：「这次更糟糕，甚至完全没有
        // timeline 了」）。
        if !text.lines().any(|line| {
            miyu_engine::tools::subagent::protocol::LOG_TAGS
                .iter()
                .any(|tag| line.starts_with(tag))
        }) {
            self.step_blocks.clear();
            let width = self.cols.saturating_sub(6).max(20);
            // 标题栏那个 title 是短标签,看不出在跑什么;日志上面原样铺一行。
            let mut head = Vec::new();
            let command = self.command.trim().to_string();
            if !command.is_empty() {
                head.extend(
                    miyu_hosts::render::wrap_display_text(&format!("$ {command}"), width)
                        .into_iter()
                        .map(|piece| format!("{indent}{piece}")),
                );
                head.push(String::new());
            }
            return head
                .into_iter()
                .chain(text.lines().flat_map(|line| {
                    if line.trim().is_empty() {
                        return vec![String::new()];
                    }
                    miyu_hosts::render::wrap_display_text(line, width)
                        .into_iter()
                        .map(|piece| format!("{indent}{piece}"))
                        .collect::<Vec<_>>()
                }))
                .collect();
        }
        self.render_steps(log_steps(text, self.display_fold))
    }

    /// 把已经攒好的那几步排成行。两条进料口（读日志 / 订标记流）共用。
    fn render_steps(&mut self, steps: Vec<LogStep>) -> Vec<String> {
        let indent = "  ";
        let head_width = miyu_hosts::render::timeline::panel_step_width_for_head().max(12);
        if steps.len() < self.step_blocks.len() {
            // 日志被从头截断过（只读末尾那一段），位置对不上了，重来一轮。
            self.step_blocks.clear();
        }
        // 「步与步之间怎么空行」的规矩**只有一份**，在 `thread_panel` 里（前台那块
        // 面板也用它）。这儿只负责把每一步变成一个 `PanelEntry`——取数的地方不同，
        // 长相不该不同（用户原话：后台子代理和前台子代理应该是一回事啊）。
        //
        // 原来这儿另有一份四状态的 `Previous` 状态机，和那边三状态的那份做的是
        // 同一件事（报告 §2.2 项 D）。合并之后 golden 逐字节不变。
        let mut entries: Vec<miyu_hosts::render::timeline::PanelEntry> = Vec::new();
        let job_running = self.job_running();
        for (index, step) in steps.iter().enumerate() {
            // 正文不是"一步"：没有抬头、不挂块、也不连线，整段照排。
            if step.kind == StepKind::Speech {
                // 不挂块也要**占住这一格**。见 `remember_block` 那段原委。
                self.remember_block(index, None);
                // 过一遍 markdown 再折行——和前台面板、主线正文一个样子。
                entries.push(miyu_hosts::render::timeline::PanelEntry::Text(
                    miyu_hosts::render::timeline::render_speech_lines(
                        &step.body.join("\n"),
                        self.cols.saturating_sub(indent.len()),
                    ),
                ));
                continue;
            }
            // 跑着的那一步自己掐表：抬头上要有 `· 1.2s`，和主线一个样子。
            //
            // 思考那一步的「正在想」只在任务还跑着时算数：停在半截的思考（任务被
            // 停了）不该一直挂着「思考中」。它的抬头就是思考正文、每来一截都在变，
            // 计时不能拿它当钥匙，否则每一截都从零算起。
            let running = step.running && (step.kind != StepKind::Thought || job_running);
            let live = running.then(|| {
                let key = if step.kind == StepKind::Thought {
                    format!("\u{1}thought#{index}")
                } else {
                    step.head.clone()
                };
                match &self.running_since {
                    Some((seen, at)) if *seen == key => at.elapsed(),
                    _ => {
                        let now = std::time::Instant::now();
                        self.running_since = Some((key, now));
                        std::time::Duration::ZERO
                    }
                }
            });
            let block = self.step_blocks.get(index).copied().filter(|id| *id != 0);
            let step = self.to_step(index, step, head_width, block, live);
            self.remember_block(index, step.block_id());
            // **走 `step_rows`，别手拼**。它是主线那份「一步占哪几行」的唯一
            // 实现：抬头、底下露着的那几行、块的起止，都在它里面。
            //
            // 这儿原来是自己拼 `begin_marker + line + END`，尾巴另发一个
            // `PanelEntry::Tail`——**尾巴因此落在块外面**，于是：点开时展开内容
            // 只换掉抬头那一行，尾巴还留在底下，命令出现两遍；而收成
            // `Worked for` 再点开时尾巴干脆没了（用户 09-17 逐条报的 1/2/3）。
            // 用户那句话是对的：同一个东西不该有两份拼法。
            let row = miyu_hosts::render::timeline::step_rows(&step, step.block_id());
            // 「提示词」那一行是抬头，不是时间线的一步：它和第一步之间不连线、
            // 空一行——连着画的话，思考那一步和提示词看着是一条线上的两步，收缩
            // 时就像被提示词绑住了（用户原话）。
            if step.kind() == StepKind::Prompt {
                entries.push(miyu_hosts::render::timeline::PanelEntry::Header(row));
            } else {
                entries.push(miyu_hosts::render::timeline::PanelEntry::Step(row));
            }
        }
        miyu_hosts::render::timeline::thread_panel(entries)
    }

    /// 给一步穿上「档位」那几样：默认展开态、抬头底下露的那几行。
    ///
    /// **只有这一处**：可见的那几步和收缩行里的那几步都过它。原来收缩行那份是
    /// 另起一行 `Step::panel(...)` 就完事，于是收起来再点开，命令预览就没了
    ///（用户 09-17 第 3 条）。
    fn dress_step(
        &self,
        step: &mut miyu_hosts::render::timeline::Step,
        log: &LogStep,
        // 这一步此刻正在进行（跑着的工具、还在想的思考）。
        live: bool,
    ) {
        // 「展开思考内容 / 展开工具内容」在这块面板上也算数：出来就是展开态。
        step.set_open(match log.kind {
            StepKind::Thought => self.display_expand.0,
            StepKind::Tool => self.display_expand.1,
            _ => false,
        });
        // 命令那一步抬头底下露几行**命令**——和主线一个样子（抬头给 title、
        // 正文给命令）。露几行由用户的「命令显示行数」说了算；整段暗色，它是
        // 附注不是正文（用户 09-17：「tag 行是暗色，而命令预览是正常文字颜色，
        // 这不合理」）。
        //
        // 正在想的那一步底下开一扇窗，露最近几行——和主线、前台面板一个样子（用户
        // 09-24：「思考的预览也没有」）。「展开思考内容」开着时这一步本来就是展开
        // 的，不另开窗。
        if log.kind == StepKind::Thought {
            if live && !self.display_expand.0 && self.display_thought_lines > 0 {
                step.set_tail(miyu_hosts::render::timeline::panel_thought_window(
                    &log.head,
                    self.display_thought_lines,
                ));
            }
            return;
        }
        if self.display_command_lines == 0 {
            return;
        }
        let Some(command) = log.command_tail() else {
            return;
        };
        step.set_tail(miyu_hosts::render::timeline::panel_command_tail(
            &command,
            self.display_command_lines,
        ));
    }

    /// 第 `index` 步用的是哪一块——**按位置记**，不挂块的那些用 `0` 占位。
    ///
    /// 占位这件事一度漏了：正文段（`Speech`）不挂块就直接 `continue`，于是
    /// `step_blocks.len()` 停在它那一格，后面每一步的 `len() == index` 都不成立、
    /// 一个都记不住。表现出来是**点开一下，下一帧自己就合上了**——每帧
    /// `to_step` 拿不到旧 id，`blocks::register` 现发一个新的，而展开表是按 id
    /// 记的（用户 09-17 实测：「存在的 tag 行我点击之后它展开了一瞬间又会自己
    /// 收起来」）。顺带还把登记处当垃圾场用：每帧几十块，4000 行的上限几秒
    /// 就把别人的块挤掉。
    fn remember_block(&mut self, index: usize, id: Option<u64>) {
        while self.step_blocks.len() < index {
            self.step_blocks.push(0);
        }
        if self.step_blocks.len() == index {
            self.step_blocks.push(id.unwrap_or(0));
        }
    }

    /// 一条日志步 → 一个渲染用的 `Step`：抬头按面板宽度拼好，正文折好上色。
    ///
    /// 「什么时候渲染」是这两块面板**唯一**剩下的差别：前台攒步时就渲染好，后台
    /// 每帧重解析、在这儿渲染。渲染完之后落进的是同一个 `Step`，后面排版、收缩、
    /// 点开的规则就都共用了（`thread_panel` / `fold_block_lines` /
    /// `step_detail_lines`）。
    fn to_step(
        &mut self,
        index: usize,
        log: &LogStep,
        head_width: usize,
        block: Option<u64>,
        live: Option<std::time::Duration>,
    ) -> miyu_hosts::render::timeline::Step {
        let line = self.step_line(log, head_width, live);
        let body = if log.inner.is_empty() {
            log_detail_body(log)
        } else {
            self.fold_body(index, &log.inner, head_width)
        };
        let mut step = miyu_hosts::render::timeline::Step::panel(log.kind, line, body, block);
        self.dress_step(&mut step, log, live.is_some());
        // 这一步自己那块：按位置复用，内容每帧重灌（日志还在长）。
        let detail = miyu_hosts::render::timeline::step_detail_lines(&step);
        let id = match block {
            Some(id) => {
                blocks::update(id, String::new(), detail);
                Some(id)
            }
            None => blocks::register(detail),
        };
        step.set_block(id);
        step
    }

    /// 这一步那一行长什么样。
    fn step_line(
        &self,
        log: &LogStep,
        head_width: usize,
        live: Option<std::time::Duration>,
    ) -> String {
        // 收缩行合着的时候是 `›`，点开才翻成 `⌄`（和主线那条一样）。
        // 按 `kind` 认，不比图标：比图标那条路已经让装工具那一步出现过两遍
        //（`load_tools` 的图标和「差事」撞了）。
        let glyph = if log.kind == StepKind::Fold {
            miyu_hosts::render::timeline::fold_glyph_closed()
        } else {
            log.glyph.as_str()
        };
        // 想的那一步按主线的说法写：`已思考 · 1.2s`。面板里光甩一句原文出来，
        // 看不出那是"在想"还是工具吐的东西。
        //
        // **§6.3 第 1 项，用户 09-17 拍板：两边都不带窥视，取前台那份。** 在此
        // 之前这儿有两种写法互相打架：可见的那几行不带耗时、带窥视；收缩里的那
        // 几步带耗时、也带窥视（那份构造在合并步模型时成了死代码，耗时因此从
        // 折叠里一起丢了，而 `#![allow(dead_code)]` 把警告盖住了）。现在一种。
        let head = if log.kind == StepKind::Thought {
            // 还在想：`思考中 · 3.2s`（计时是面板自己掐的，见 `render_steps`），
            // 想完才是 `已思考 · …`——和主线、前台面板一个说法。原来不管想没想完
            // 都写「已思考」。
            let (label, secs) = match live {
                Some(live) => (
                    miyu_base::i18n::text("thinking", "思考中"),
                    Some(miyu_base::durations::format_seconds(live)),
                ),
                None => (
                    miyu_base::i18n::text("thought", "已思考"),
                    log.elapsed
                        .and_then(miyu_hosts::render::timeline::reported_seconds),
                ),
            };
            let mut head = label.to_string();
            if let Some(secs) = secs {
                head.push_str(" · ");
                head.push_str(&secs);
            }
            head
        } else {
            // 跑着的那一步把计时插进去：`运行命令 · 1.2s · 看看输出`——名字后面、
            // 窥视前面，和主线一个次序（用户 09-17 点名的那个形状）。原来这儿
            // 是在末尾缀一句 `· 运行中`，而主线从来不写那三个字：左边距上转着
            // 的点阵已经把「它在跑」说清楚了。
            match live.and_then(miyu_hosts::render::timeline::reported_seconds) {
                Some(secs) => with_elapsed_text(&log.head, &secs),
                None => log.head.clone(),
            }
        };
        // 加减行数单独上色（绿加红减），和主线、前台浮层一个样子。它是从文本里
        // 摘出来的（见 `LogStep::diff`），所以要自己留出宽度再接回去——先裁剩下
        // 那截，不然裁刀会从数字中间切过去。
        let stat = log
            .diff
            .map(|(added, removed)| miyu_hosts::render::diff_stat_label(added, removed));
        let room = match &stat {
            // `+123 -45` 那几个字符是可见宽度，转义序列不占列。
            Some(_) => head_width.saturating_sub(
                log.diff
                    .map(|(added, removed)| format!(" · +{added} -{removed}").chars().count())
                    .unwrap_or(0),
            ),
            None => head_width,
        };
        let mut head = miyu_hosts::render::clip_to_display_width(&head, room.max(8));
        if let Some(stat) = stat {
            head.push_str(" · ");
            head.push_str(&stat);
        }
        // 正在跑／正在准备／正在想的那一行左边距上转着点阵，和主线一样。认 `live`
        // 不认 `running`：停在半截的思考（任务已经停了）不算「正在」。
        if live.is_some() || log.preparing {
            miyu_hosts::render::timeline::panel_live_step_line(glyph, &head)
        } else {
            miyu_hosts::render::timeline::panel_step_line(glyph, &head, log.status == Some("err"))
        }
    }

    /// 收缩行点开是什么样：收起来的那几步串成时间线，每一步各自登记成块，
    /// 再点开才是它的正文。块 id 按 `(收缩行位置, 步位置)` 复用，刷新不换 id。
    ///
    /// 串行与登记的规则与主线、前台面板共用（`fold_block_lines`）——那三份原来
    /// 逐行相同地各写了一遍（报告 §2.1 第 4 条）。
    fn fold_body(
        &mut self,
        fold_index: usize,
        inner: &[LogStep],
        head_width: usize,
    ) -> Vec<String> {
        let mut children: Vec<miyu_hosts::render::timeline::Step> = inner
            .iter()
            .enumerate()
            .map(|(child_index, log)| {
                let line = self.step_line(log, head_width, None);
                let mut step = miyu_hosts::render::timeline::Step::panel(
                    log.kind,
                    line,
                    log_detail_body(log),
                    self.fold_blocks.get(&(fold_index, child_index)).copied(),
                );
                // 收起来的那几步和可见的那几步是同一种东西：档位一样要穿。收起来的
                // 都已经走完了，不会是「正在」。
                self.dress_step(&mut step, log, false);
                step
            })
            .collect();
        let rows = miyu_hosts::render::timeline::fold_block_lines(&mut children);
        for (child_index, child) in children.iter().enumerate() {
            if let Some(id) = child.block_id() {
                self.fold_blocks.insert((fold_index, child_index), id);
            }
        }
        rows
    }

    /// 一串原始进度标记 → 面板内容。和 `reload_file` 里订标记流那一支同一条路。
    #[cfg(test)]
    pub(super) fn render_from_markers(&mut self, markers: &[String]) {
        let events = markers
            .iter()
            .flat_map(|marker| miyu_engine::tools::subagent::protocol::from_marker(marker));
        let steps = log::steps_from_events(events, self.display_fold);
        let lines = self.render_steps(steps);
        self.body = parse_body(&lines, self.cols);
        super::expand::reload_expanded(&mut self.expanded, self.cols);
        self.seed_open();
    }

    /// 不管节流，立刻按当前档位重排一次（`/config` 改完那一下）。
    fn force_reload(&mut self) {
        match &mut self.source {
            Source::Block { version, .. } => *version = u64::MAX,
            Source::File { .. } => self.reload_file(true),
        }
    }

    /// 内容有变就重取。
    fn refresh(&mut self) {
        match &mut self.source {
            Source::Block { id, version } => {
                let current = blocks::version(*id);
                if current == *version {
                    return;
                }
                *version = current;
                let id = *id;
                if let Some(title) = blocks::title(id) {
                    self.title = title;
                }
                if let Some(lines) = blocks::get(id) {
                    self.body = parse_body(&lines, self.cols);
                    // 展开着的那几块**留着**，只是把内容换成新的。
                    //
                    // 原来是整表清掉：子代理一边跑一边刷新（一秒好几次），
                    // 于是刚点开的东西立刻自己缩回去，根本读不了一句
                    //（用户实测：窥视的动态刷新会导致已展开内容缩起）。
                    // 块 id 现在是按位置复用的，第 i 步永远是第 i 步，
                    // 保住它是安全的。
                    super::expand::reload_expanded(&mut self.expanded, self.cols);
                    self.seed_open();
                }
            }
            Source::File { .. } => self.reload_file(false),
        }
    }
}

fn parse_body(lines: &[String], cols: usize) -> Body {
    Body::parse(&lines.join("\r\n"), cols)
}

impl Overlay {
    fn layer(&self) -> Layer<'_> {
        Layer::Body(&self.body)
    }

    fn len(&self) -> usize {
        layer_len(&self.layer(), &self.expanded)
    }

    fn row(&self, index: usize) -> Vec<super::ansi::AnsiSpan> {
        layer_row(&self.layer(), &self.expanded, index)
    }
}

#[cfg(test)]
mod test_support;
