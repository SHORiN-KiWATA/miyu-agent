//! 覆盖层：盖住整屏看一个后台命令的日志，Esc 关掉。
//!
//! 命令还在跑、日志还在长，塞进正文里就地展开的话行数每秒都在变、视口跟着抖。盖一层
//! 就没这些问题：主线原封不动，面板里自己滚。
//!
//! 原来它还装子代理的内层时间线（子代理是它存在的最初理由）。子代理 09-18 起是一条会话，
//! 点它切进去看；那一半 09-25 随老标记中继退役（会话项目第 4 段之二）。

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
use std::io::Write;

mod log;
mod screen;

use log::*;

/// 面板的内容：盘上的日志文件。任务跑在 daemon 里，日志本来就落盘，直接读比再造一条
/// IPC 分页通道省事。
struct LogFile {
    path: std::path::PathBuf,
    size: u64,
    /// 上一次重读是什么时候。命令一边跑一边写，文件每帧都在长，帧帧重读重排把画面板的
    /// 那一帧拖慢，转轮就一顿一顿。
    last_reload: Option<std::time::Instant>,
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
    source: LogFile,
    /// 跑的是哪条命令。标题栏那个 title 是短标签(模型给的话只有 16 字符),
    /// 看不出真正在跑什么,所以日志上面原样铺一行(用户 09-14)。
    command: String,
    /// 这个面板讲的是哪个后台任务。有值才允许按 x 停。
    job_id: Option<String>,
    /// 画面宽度，重新解析内容时要用。
    cols: usize,
    title: String,
    body: Body,
    /// 面板里自己的展开状态，和正文那边互不相干。
    expanded: Expanded,
    /// 鼠标停在面板里哪一块上。可点的东西要看得出来「这里能点」——正文那侧
    /// 一直有（`Screen::hover`），面板这侧原来整个没有：`Moved` 和别的鼠标事件
    /// 一起被吞掉了（用户 09-17：「浮层的 tag 行没有悬浮变色的效果」）。
    hover: Option<u64>,
    /// 面板里的选区。行号是**面板自己的内容行**（`Overlay::row` 那套），不是
    /// 正文缓冲的绝对行——面板是另一张画布，滚动也是自己的。
    ///
    /// 面板原来整个不做选区（`tail_impl` 里那句「其余吞掉」），而面板里装的正是
    /// 最想复制走的东西：后台命令的日志（用户 09-17：「这样的浮层无法选中文字」）。
    selection: Option<super::select::Selection>,
    scroll: usize,
    /// 停在底部就跟着新内容走；自己往回翻过就别再拽他。
    follow: bool,
    /// 面板占多高。**开着的时候只涨不缩**。
    ///
    /// 按当前内容每帧重算的话，后台任务每写一行日志面板就长高一点、上边沿
    /// 跟着往上跳——AI 正在输出时日志一秒写十几行，面板就一直在抖
    /// （用户原话「后台命令再究极鬼畜…浮层的位置也不对」）。
    height: u16,
}

impl Overlay {
    fn from_file(
        path: std::path::PathBuf,
        title: String,
        job_id: Option<String>,
        command: String,
        cols: usize,
    ) -> Self {
        let mut panel = Self {
            source: LogFile {
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
            hover: None,
            selection: None,
            scroll: 0,
            follow: true,
            height: 0,
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
        // 宽度变了内容要重排，按行列记的选区会指到别处去。
        self.selection = None;
        self.reload_file(true);
    }

    fn file_path(&self) -> &std::path::Path {
        self.source.path.as_path()
    }

    fn reload_file(&mut self, force: bool) {
        let LogFile {
            path,
            size,
            last_reload,
        } = &mut self.source;
        if !force && last_reload.is_some_and(|last| last.elapsed() < LOG_RELOAD_INTERVAL) {
            return;
        }
        let current = std::fs::metadata(&*path)
            .map(|meta| meta.len())
            .unwrap_or(0);
        if !force && current == *size {
            return;
        }
        *size = current;
        *last_reload = Some(std::time::Instant::now());
        let text = read_tail(path, LOG_TAIL_BYTES);
        let lines = self.render_log(&text);
        self.body = parse_body(&lines, self.cols);
        // 展开着的那几块留着，只是把内容换成新的：一刷新就整张清掉的话，刚点开的东西
        // 立刻自己缩回去。
        super::expand::reload_expanded(&mut self.expanded, self.cols);
    }

    /// 后台命令的日志就是一堆输出行，没有"步"可言：原样折行，上面铺一行命令本身
    /// （标题栏那个 title 是短标签，看不出在跑什么）。
    fn render_log(&mut self, text: &str) -> Vec<String> {
        let indent = "  ";
        let width = self.cols.saturating_sub(6).max(20);
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
        head.into_iter()
            .chain(text.lines().flat_map(|line| {
                if line.trim().is_empty() {
                    return vec![String::new()];
                }
                miyu_hosts::render::wrap_display_text(line, width)
                    .into_iter()
                    .map(|piece| format!("{indent}{piece}"))
                    .collect::<Vec<_>>()
            }))
            .collect()
    }

    /// 内容有变就重取。
    fn refresh(&mut self) {
        self.reload_file(false);
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
