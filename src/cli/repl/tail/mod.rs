//! 屏幕底部的活动区。
//!
//! REPL 把屏幕分成两半：上面是只增不改的对话正文，下面几行是随时重画的活动
//! 区——输入框、footer、排队提示、后台任务条。活动区要在正文不断追加的同时
//! 稳稳待在底部，所以这里全是绝对行号与光标账：哪一行是活动区起点、正文能
//! 用到第几行、重画时该滚几行。
//!
//! 这套记账是终端渲染最容易出错的地方（2026-08-17 的图片错位就出在这里），
//! 改动前先看 `trace_tail_redraw` 留下的诊断开关。

// 活动区还用着一批留在 cli::mod 的东西（footer 结构、队列渲染、job 条）。
mod frame;
mod queue;
pub(in crate::cli) mod screen;
mod update;

pub(in crate::cli) use update::{synchronized_terminal_update, term_out, TermOut};

#[cfg(test)]
pub(in crate::cli) use frame::queue_lifted_frame;

use crate::cli::repl::editor::*;
use crate::cli::*;

/// crossterm asks the terminal where the cursor is (`ESC[6n`) and gives up if
/// the reply does not arrive within a fixed wait. Over a laggy SSH link that
/// wait expires routinely, and every `?` on it used to take the whole REPL
/// down with "The cursor position could not be read within a normal duration".
/// The answer is only ever used to re-anchor a redraw, so a stale one costs a
/// single imperfect frame — losing the session costs the session.
/// 活动区重绘轨迹（`MIYU_TAIL_TRACE=1` 打开，落
/// `~/.miyu/cache/logs/tail-trace.log`）。
///
/// 这段重绘靠绝对屏幕行号 + DECSTBM 受限滚动区 + 插入/删除行来搬动活动
/// 区（上边距为 1 时，受限区里滚出去的行照样进 scrollback）。kitty 的占位
/// 符图片在受限区滚动下会留残影（见 frame.rs 的 `queue_lifted_frame`），所
/// 以发过图之后腾地方改走整屏滚。要定位就得看出错那一刻实际发了哪些序列。
#[allow(clippy::too_many_arguments)]
pub(in crate::cli) fn trace_tail_redraw(
    tail_start: u16,
    next_tail: u16,
    shift: i32,
    tail_rows: u16,
    output_cursor: (u16, u16),
    output_bottom: Option<u16>,
    leading_scroll: u16,
    terminal_rows: u16,
    transaction: &[u8],
) {
    use std::io::Write as _;
    // 只记会搬动屏幕内容的序列,纯重绘噪声太大。
    let escapes = String::from_utf8_lossy(transaction);
    let mut moves = Vec::new();
    for (marker, label) in [
        ("L", "IL 插入行"),
        ("M", "DL 删除行"),
        ("r", "DECSTBM 滚动区"),
    ] {
        let pattern = format!("\x1b[");
        let mut rest = escapes.as_ref();
        while let Some(index) = rest.find(&pattern) {
            rest = &rest[index + pattern.len()..];
            if let Some(end) = rest.find(|c: char| c.is_ascii_alphabetic()) {
                if &rest[end..end + 1] == marker {
                    moves.push(format!("{label}({})", &rest[..end]));
                }
            }
        }
    }
    let line = format!(
        "tail {tail_start}→{next_tail} shift={shift} rows={tail_rows} \
         cursor={output_cursor:?} bottom={output_bottom:?} \
         leading_scroll={leading_scroll} term_rows={terminal_rows} \
         | {}\n",
        if moves.is_empty() {
            "无搬动".to_string()
        } else {
            moves.join(" ")
        }
    );
    let path = std::path::Path::new(&std::env::var("HOME").unwrap_or_default())
        .join(".miyu/cache/logs/tail-trace.log");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = file.write_all(line.as_bytes());
    }
}

pub(in crate::cli) fn cursor_position_or(fallback: (u16, u16)) -> (u16, u16) {
    // 全屏下「终端光标在哪」这个问题没有意义——屏幕是我们自己画的，位置都
    // 是算出来的。更要命的是 `ESC[6n` 的应答要**从 stdin 读**，那会把用户
    // 正在打的字吞掉（09-11 走查里「打字不回显」就是这么来的）。
    if screen::in_fullscreen() {
        return fallback;
    }
    // 终端已挂断时 ESC[6n 永远等不到应答,而 crossterm 的应答等待对
    // HUP fd 会无限自旋(超时失效)——直接用回退值,让退出路径走完。
    if terminal_hangup() {
        return fallback;
    }
    let started = std::time::Instant::now();
    let answer = cursor::position();
    // 取证：这一问要是没答上来，活动区就会按**外部输出之前**的位置重画，
    // 正好盖在刚打出来的图上。终端渲染大图（sixel 动辄上百 KB）期间不会
    // 回答 ESC[6n，图越大越容易在这里超时——所以要看得见它。
    if miyu_base::terminal::chafa::trace_enabled() {
        miyu_base::terminal::chafa::trace(&format!(
            "CPR {:?} {}ms fallback={:?}{}",
            answer,
            started.elapsed().as_millis(),
            fallback,
            if answer.is_err() {
                "  ←问不到,退回旧位置"
            } else {
                ""
            },
        ));
    }
    answer.unwrap_or(fallback)
}

/// 从 `start` 起把 `frame` 写进终端后光标停在哪。追踪器不知道页高,顶到页底
/// 之后终端是滚动而不是继续往下,所以行号封在最后一行。
pub(in crate::cli) fn cursor_after_frame(
    frame: &[u8],
    start: (u16, u16),
    columns: u16,
    terminal_rows: u16,
) -> (u16, u16) {
    let layout = terminal_frame_layout(frame, start, columns, None);
    (
        layout.cursor.0,
        layout.cursor.1.min(terminal_rows.saturating_sub(1)),
    )
}

pub(in crate::cli) fn cursor_row_or(fallback: u16) -> u16 {
    cursor_position_or((0, fallback)).1
}

pub(in crate::cli) fn cursor_col_or(fallback: u16) -> u16 {
    cursor_position_or((fallback, 0)).0
}

pub(in crate::cli) struct LiveReplTail {
    pub(in crate::cli) editor: LiveReplEditor,
    pub(in crate::cli) queued: Vec<QueuedPrompt>,
    pub(in crate::cli) pending_chunks: Vec<ChatStreamChunk>,
    pub(in crate::cli) footer: ReplFooterStatus,
    /// 回合中途逐请求刷新计量时的基线(回合开始前的 footer 快照)。
    /// 每次 RoundUsage 事件都从基线重新叠加,避免累计值重复相加;
    /// 任何权威更新(set_footer)都会清掉它。
    pub(in crate::cli) round_base_footer: Option<Box<ReplFooterStatus>>,
    /// footer 行相对 tail_start 的偏移(每次输入区渲染时更新)。存偏移而非
    /// 绝对行:apply_output_frame 用 \x1b[L/M 整体平移 tail 时不重画,绝对
    /// 行号会过期——tick 在旧行覆写就画出第二份 footer(孤儿),取消回合后
    /// 那行永远没人清(用户 08-20 截图实锤)。
    pub(in crate::cli) footer_offset: Option<u16>,
    pub(in crate::cli) footer_spinner_last: Option<std::time::Instant>,
    /// 输入框右上角那行 `/goal …` 上一次画出去的**原文**。秒数每秒自己变，
    /// 光比 `GoalHint` 比不出来；比字符串则「没变就不重画」，空闲时一秒最多
    /// 一帧。见 [`Self::tick_goal_hint`]。
    pub(in crate::cli) goal_hint_drawn: String,
    pub(in crate::cli) jobs: Vec<miyu_engine::tools::jobs::JobOverview>,
    /// 已经下过"停"的任务 → 下达的时刻。见 `suppress_jobs`。
    pub(in crate::cli) suppressed_jobs: std::collections::HashMap<String, std::time::Instant>,
    /// Σ 上那份实时加数：这一轮里跑着的前台子代理此刻烧了多少。
    /// 见 [`Self::set_live_turn_tokens`]。
    pub(in crate::cli) live_turn_tokens: u64,
    pub(in crate::cli) job_spinner: usize,
    /// 状态行转轮的计时起点。帧按时间算（80ms 一帧）：谁来重画都画在该在的位置，
    /// 不再是「每次 tick_job_strip 进一帧」——AI 流式输出时那一路每 8 个转轮 tick
    /// 才来一次，状态行的转轮就一顿一顿（用户实测）。
    pub(in crate::cli) job_spinner_started: std::time::Instant,
    /// 后台状态行在屏幕上的起始行与行数。全屏下点它要能对上是哪一个任务。
    pub(in crate::cli) job_strip_start: u16,
    pub(in crate::cli) job_strip_rows: u16,
    /// 鼠标正悬在任务条的哪一条上(`jobs` 的下标),那一行画成不 dim。
    pub(in crate::cli) job_hover: Option<usize>,
    /// 最后一次鼠标移动落在哪、什么时候。
    ///
    /// 指针移出窗口时终端**什么都不发**——09-22 实测（`testkit/tui/
    /// pointer_leave_probe.py`）：kitty 不发焦点事件，也没有任何「离开」信号，
    /// 只是从此静默。可用的线索只有一条：移出去之前最后那一下必然落在**边缘**
    /// （实测一路报到第 1 列），而窗口内正常移动两次之间最多隔 0.4 秒。所以
    /// 「最后停在边缘 + 随后静默」就当指针出去了，把提亮熄掉。
    /// 停在正文当中不动不算——那是悬着看，提亮该留着。
    pub(in crate::cli) last_mouse_move: Option<((u16, u16), std::time::Instant)>,
    /// 用户在详情面板里按了 x：这个任务该停了。事件层不发 IPC（它没有
    /// 异步上下文），攒在这儿由主循环取走。
    pub(in crate::cli) pending_stop_job: Option<String>,
    pub(in crate::cli) output_cursor: (u16, u16),
    pub(in crate::cli) tail_start: u16,
    pub(in crate::cli) tail_rows: u16,
    pub(in crate::cli) input_cursor: (u16, u16),
    pub(in crate::cli) rendered: bool,
    pub(in crate::cli) external_output_active: bool,
    pub(in crate::cli) raw_mode_handoff: bool,
    /// 全屏后端。`None` 就是原来的 inline 行为——正文进 scrollback、
    /// 活动区靠 DECSTBM 钉在底下。`Some` 时正文改由它持有，活动区照旧
    /// 由 `render_repl_input_with_footer` 打，只是 `tail_start` 指向视口底部。
    pub(in crate::cli) screen: Option<screen::Screen>,
    /// 空会话的画面(渐变 MIYU + 星空 + 模式行)。会话一有回合就撤。
    pub(in crate::cli) banner: Option<crate::cli::repl::banner::BannerScene>,
    /// inline 后端里 banner 占了活动区顶上的几行(全屏下为 0,画在正文区)。
    pub(in crate::cli) banner_rows: u16,
    /// Bottom space temporarily reserved for a lobby selector.
    pub(in crate::cli) lobby_panel_rows: u16,
    /// 空会话按 Tab 换车道:下一次会话切换不打「已切换到会话」——用户看到的是
    /// 模式行变色,不是换会话。一次性,用过即清。
    pub(in crate::cli) suppress_switch_note: bool,
    /// 回合跑着时寄宿的 `/models` 面板改了会话模型（09-20）：这里的 footer 已
    /// 经按新模型重算过，可 `RemoteRepl` 手里那份还是旧的，回合一结束它会拿旧
    /// 的盖回来（主循环每圈 `set_footer`）。立这面旗让它在盖之前先重算一次。
    /// 一次性，用过即清。
    pub(in crate::cli) session_footer_stale: bool,
    /// 界面上的 Σ 是空闲循环按轮询改的（上次显式刷新之后才读的那份）。主循环整份
    /// 覆盖之前据此把它收回来，见 `ReplFooterStatus::adopt_cumulative`。
    pub(in crate::cli) cumulative_from_poll: bool,
    /// 大厅里按 Tab 换过车道，但会话还在原来那条车道上（用户 09-23：切模式只是换
    /// 显示，发第一句话时后端才定）。发消息、敲命令、切只读之前据此把会话换过去。
    pub(in crate::cli) lobby_lane_pending: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::cli) struct LiveTailPlacement {
    pub(in crate::cli) output_row: u16,
    pub(in crate::cli) tail_start: u16,
    pub(in crate::cli) overflow: u16,
    pub(in crate::cli) anchored: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::cli) struct TerminalFrameLayout {
    pub(in crate::cli) cursor: (u16, u16),
    pub(in crate::cli) occupied_bottom: Option<u16>,
}

/// 帧里一次「顶到页底、把正文滚上去一行」的事件。
///
/// `end` 是引发这次滚动的字节(换行符,或折行的那个字位)在帧里的**结束偏移**,
/// 帧从这里切开,前一段恰好滚了这么多次;`col_after` 是滚完之后光标停的列
/// (换行是 0,折行是那个字位的宽度),后一段从这里接着写。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::cli) struct FrameScroll {
    pub(in crate::cli) end: usize,
    pub(in crate::cli) col_after: u16,
}

pub(in crate::cli) struct TerminalFrameTracker {
    pub(in crate::cli) columns: usize,
    pub(in crate::cli) bottom_margin: Option<usize>,
    pub(in crate::cli) cursor_col: usize,
    pub(in crate::cli) cursor_row: usize,
    pub(in crate::cli) saved_cursor: (usize, usize, bool),
    pub(in crate::cli) pending_wrap: bool,
    pub(in crate::cli) pending_text: String,
    /// `pending_text` 里每个 char 在帧里的结束偏移,与其一一对应。
    pub(in crate::cli) pending_ends: Vec<usize>,
    pub(in crate::cli) occupied_bottom: Option<usize>,
    /// 正在喂给追踪器的字节在帧里的结束偏移(喂第 i 个字节时为 i+1)。
    pub(in crate::cli) byte_index: usize,
    pub(in crate::cli) scrolls: Vec<FrameScroll>,
}

impl TerminalFrameTracker {
    pub fn new(start: (u16, u16), columns: u16, bottom_margin: Option<u16>) -> Self {
        let columns = usize::from(columns.max(1));
        let cursor_col = usize::from(start.0).min(columns.saturating_sub(1));
        let cursor_row = usize::from(start.1);
        Self {
            columns,
            bottom_margin: bottom_margin.map(usize::from),
            cursor_col,
            cursor_row,
            saved_cursor: (cursor_col, cursor_row, false),
            pending_wrap: false,
            pending_text: String::new(),
            pending_ends: Vec::new(),
            occupied_bottom: None,
            byte_index: 0,
            scrolls: Vec::new(),
        }
    }

    pub(in crate::cli) fn finish(mut self) -> TerminalFrameLayout {
        self.flush_text();
        TerminalFrameLayout {
            cursor: (
                self.cursor_col.min(u16::MAX as usize) as u16,
                self.cursor_row.min(u16::MAX as usize) as u16,
            ),
            occupied_bottom: self
                .occupied_bottom
                .map(|row| row.min(u16::MAX as usize) as u16),
        }
    }

    pub(in crate::cli) fn finish_with_scrolls(mut self) -> (TerminalFrameLayout, Vec<FrameScroll>) {
        self.flush_text();
        let scrolls = std::mem::take(&mut self.scrolls);
        (self.finish(), scrolls)
    }

    pub(in crate::cli) fn flush_text(&mut self) {
        if self.pending_text.is_empty() {
            return;
        }
        let text = std::mem::take(&mut self.pending_text);
        let ends = std::mem::take(&mut self.pending_ends);
        // 折行引发的滚动要记在那个字位自己的结束偏移上,而不是记在触发
        // flush 的控制字节上——否则一个字位和紧随的换行会记成同一个偏移,
        // 帧就没法在两次滚动之间切开。
        let current = self.byte_index;
        let mut chars_seen = 0usize;
        for grapheme in text.graphemes(true) {
            chars_seen = chars_seen.saturating_add(grapheme.chars().count());
            self.byte_index = ends
                .get(chars_seen.saturating_sub(1))
                .copied()
                .unwrap_or(current);
            self.print_width(UnicodeWidthStr::width(grapheme));
        }
        self.byte_index = current;
    }

    pub(in crate::cli) fn print_width(&mut self, width: usize) {
        if width == 0 {
            return;
        }
        let mut scrolled = false;
        if self.pending_wrap || self.cursor_col.saturating_add(width) > self.columns {
            self.cursor_col = 0;
            scrolled = self.index();
            self.pending_wrap = false;
        }
        self.occupied_bottom = Some(
            self.occupied_bottom
                .map_or(self.cursor_row, |row| row.max(self.cursor_row)),
        );
        let next_col = self.cursor_col.saturating_add(width);
        if next_col >= self.columns {
            self.cursor_col = self.columns.saturating_sub(1);
            self.pending_wrap = true;
        } else {
            self.cursor_col = next_col;
        }
        if scrolled {
            if let Some(last) = self.scrolls.last_mut() {
                last.col_after = self.cursor_col.min(u16::MAX as usize) as u16;
            }
        }
    }

    /// 光标下移一行;顶在页底时记一次滚动并返回 true。
    pub(in crate::cli) fn index(&mut self) -> bool {
        if self
            .bottom_margin
            .is_some_and(|bottom| self.cursor_row >= bottom)
        {
            self.scrolls.push(FrameScroll {
                end: self.byte_index,
                col_after: self.cursor_col.min(u16::MAX as usize) as u16,
            });
            return true;
        }
        self.cursor_row = self.cursor_row.saturating_add(1);
        false
    }

    pub(in crate::cli) fn move_down(&mut self, count: usize) {
        self.pending_wrap = false;
        self.cursor_row = self.cursor_row.saturating_add(count);
        if let Some(bottom) = self.bottom_margin {
            self.cursor_row = self.cursor_row.min(bottom);
        }
    }

    pub(in crate::cli) fn move_up(&mut self, count: usize) {
        self.pending_wrap = false;
        self.cursor_row = self.cursor_row.saturating_sub(count);
    }

    pub(in crate::cli) fn move_right(&mut self, count: usize) {
        self.pending_wrap = false;
        self.cursor_col = self
            .cursor_col
            .saturating_add(count)
            .min(self.columns.saturating_sub(1));
    }

    pub(in crate::cli) fn move_left(&mut self, count: usize) {
        self.pending_wrap = false;
        self.cursor_col = self.cursor_col.saturating_sub(count);
    }

    pub(in crate::cli) fn set_row(&mut self, row: usize) {
        self.pending_wrap = false;
        self.cursor_row = row;
        if let Some(bottom) = self.bottom_margin {
            self.cursor_row = self.cursor_row.min(bottom);
        }
    }

    pub(in crate::cli) fn set_col(&mut self, col: usize) {
        self.pending_wrap = false;
        self.cursor_col = col.min(self.columns.saturating_sub(1));
    }

    pub(in crate::cli) fn param(params: &VteParams, index: usize, default: usize) -> usize {
        params
            .iter()
            .nth(index)
            .and_then(|param| param.first())
            .copied()
            .map(usize::from)
            .filter(|value| *value != 0)
            .unwrap_or(default)
    }
}

impl VtePerform for TerminalFrameTracker {
    fn print(&mut self, character: char) {
        self.pending_text.push(character);
        self.pending_ends.push(self.byte_index);
    }

    fn execute(&mut self, byte: u8) {
        self.flush_text();
        match byte {
            b'\n' => {
                self.cursor_col = 0;
                self.pending_wrap = false;
                self.index();
            }
            b'\r' => self.set_col(0),
            0x08 => self.move_left(1),
            b'\t' => {
                let next = (self.cursor_col / 8 + 1) * 8;
                self.set_col(next);
            }
            0x0b | 0x0c => {
                self.pending_wrap = false;
                self.index();
            }
            _ => {}
        }
    }

    fn csi_dispatch(
        &mut self,
        params: &VteParams,
        _intermediates: &[u8],
        ignore: bool,
        action: char,
    ) {
        self.flush_text();
        if ignore {
            return;
        }
        let count = Self::param(params, 0, 1);
        match action {
            'A' => self.move_up(count),
            'B' | 'e' => self.move_down(count),
            'C' | 'a' => self.move_right(count),
            'D' => self.move_left(count),
            'E' => {
                self.move_down(count);
                self.set_col(0);
            }
            'F' => {
                self.move_up(count);
                self.set_col(0);
            }
            'G' | '`' => self.set_col(count.saturating_sub(1)),
            'H' | 'f' => {
                self.set_row(Self::param(params, 0, 1).saturating_sub(1));
                self.set_col(Self::param(params, 1, 1).saturating_sub(1));
            }
            'd' => self.set_row(count.saturating_sub(1)),
            's' => {
                self.saved_cursor = (self.cursor_col, self.cursor_row, self.pending_wrap);
            }
            'u' => {
                (self.cursor_col, self.cursor_row, self.pending_wrap) = self.saved_cursor;
            }
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, _intermediates: &[u8], ignore: bool, byte: u8) {
        self.flush_text();
        if ignore {
            return;
        }
        match byte {
            b'7' => self.saved_cursor = (self.cursor_col, self.cursor_row, self.pending_wrap),
            b'8' => {
                (self.cursor_col, self.cursor_row, self.pending_wrap) = self.saved_cursor;
            }
            b'D' => {
                self.pending_wrap = false;
                self.index();
            }
            b'E' => {
                self.cursor_col = 0;
                self.pending_wrap = false;
                self.index();
            }
            b'M' => self.move_up(1),
            _ => {}
        }
    }
}

pub(in crate::cli) fn live_frame_output_bottom(
    frame_margin: u16,
    layout: TerminalFrameLayout,
) -> Option<u16> {
    let ends_on_free_line = layout.cursor.0 == 0
        && layout
            .occupied_bottom
            .is_none_or(|bottom| layout.cursor.1 > bottom);
    if ends_on_free_line {
        Some(frame_margin)
    } else {
        frame_margin.checked_sub(1)
    }
}

/// Places the tail below the output. `was_anchored` says the tail was already
/// pinned to the bottom: without it a tail that *shrinks* (a background job
/// strip or a queue bubble going away) would spring back up to the output
/// cursor, leaving blank rows under the input box until later output pushed it
/// down again — the input visibly bouncing.
pub(in crate::cli) fn live_tail_placement(
    output_col: u16,
    output_row: u16,
    total_rows: u16,
    terminal_rows: u16,
    was_anchored: bool,
) -> LiveTailPlacement {
    let terminal_rows = terminal_rows.max(1);
    let last_row = terminal_rows.saturating_sub(2);
    let natural_start = output_row.saturating_add(u16::from(output_col > 0));
    let natural_end = natural_start.saturating_add(total_rows.saturating_sub(1));
    let overflow = natural_end.saturating_sub(last_row);
    let output_row = output_row.saturating_sub(overflow);
    let natural_start = output_row.saturating_add(u16::from(output_col > 0));
    let anchored = was_anchored || overflow > 0 || natural_end == last_row;
    let anchored_start = last_row.saturating_add(1).saturating_sub(total_rows);
    let tail_start = if anchored {
        natural_start.max(anchored_start)
    } else {
        natural_start
    };
    // `output_row` is deliberately left where the output actually ended, even
    // when the tail re-anchors below it. It is the contract between the
    // renderer's byte frames and the terminal — the wait spinner erases itself
    // by moving relative to that cursor — so nudging it down to hug the tail
    // leaves orphaned spinner frames in the scrollback.
    LiveTailPlacement {
        output_row,
        tail_start,
        overflow,
        anchored,
    }
}

/// Where a streaming output frame should leave the tail.
///
/// Normally the tail follows the output cursor. A tail already pinned to the
/// bottom stays pinned instead: output fills the rows above it (the frame sets
/// a scroll region so it cannot reach the tail). Letting it slide back up is
/// what made the input box bounce — the rows a finished job strip freed would
/// be reclaimed on the very next frame, then handed back a line of output
/// later.
pub(in crate::cli) fn live_tail_next_start(
    current_start: u16,
    desired_tail: u16,
    max_tail: u16,
) -> u16 {
    if current_start >= max_tail {
        max_tail
    } else {
        desired_tail.min(max_tail)
    }
}

pub(in crate::cli) fn max_live_tail_start(terminal_rows: u16, tail_rows: u16) -> u16 {
    terminal_rows
        .max(1)
        .saturating_sub(1)
        .saturating_sub(tail_rows)
}

impl LiveReplTail {
    pub fn new(
        mode: PersonaLane,
        history: Vec<ReplHistoryEntry>,
        queued: Vec<QueuedPrompt>,
        footer: ReplFooterStatus,
    ) -> Result<Self> {
        Ok(Self {
            editor: LiveReplEditor::new(mode, history),
            queued,
            pending_chunks: Vec::new(),
            footer,
            round_base_footer: None,
            footer_offset: None,
            footer_spinner_last: None,
            goal_hint_drawn: String::new(),
            jobs: Vec::new(),
            suppressed_jobs: std::collections::HashMap::new(),
            live_turn_tokens: 0,
            job_spinner: 0,
            job_spinner_started: std::time::Instant::now(),
            output_cursor: cursor_position_or((0, 0)),
            tail_start: 0,
            tail_rows: 0,
            job_strip_start: 0,
            job_strip_rows: 0,
            job_hover: None,
            last_mouse_move: None,
            pending_stop_job: None,
            input_cursor: (0, 0),
            rendered: false,
            external_output_active: false,
            raw_mode_handoff: false,
            screen: {
                screen::trace_rss("tail-new");
                let built = screen::requested()
                    .then(screen::Screen::enter)
                    .transpose()?;
                if let Some(screen) = &built {
                    screen.trace_ready();
                }
                built
            },
            banner: None,
            banner_rows: 0,
            lobby_panel_rows: 0,
            suppress_switch_note: false,
            session_footer_stale: false,
            cumulative_from_poll: false,
            lobby_lane_pending: false,
        })
    }

    /// 会话空不空。空 → 挂 banner、Tab 可换车道;非空 → 撤掉、钉死模式。
    pub(in crate::cli) fn set_session_empty(
        &mut self,
        config: &AppConfig,
        paths: &MiyuPaths,
        empty: bool,
    ) {
        self.editor.mode_switchable = empty;
        if empty {
            if self.banner.is_none() {
                let mut banner =
                    crate::cli::repl::banner::BannerScene::load(config, paths, self.editor.mode);
                // 已经在画面里了(/reset、/new 回到大厅)就不淡入;只有启动那一次淡入。
                if self.rendered {
                    if let Some(banner) = &mut banner {
                        banner.settle();
                    }
                }
                if let Some(banner) = &mut banner {
                    banner.set_readonly(self.editor.readonly);
                }
                self.banner = banner;
            }
            // 回到大厅就是新会话：旧对话不再属于它，画布一起清掉，下一句话从
            // 屏顶起。启动那次还没画过，缓冲本来就是空的。
            if self.rendered {
                if let Some(screen) = &mut self.screen {
                    screen.wipe_transcript();
                }
            }
        } else {
            self.leave_lobby();
        }
    }

    /// 撤掉大厅：banner 退场、模式钉死。不需要配置，输入循环里回车那一帧就能调
    /// （见 `read_live_repl_input` 的提交分支）。
    pub(in crate::cli) fn leave_lobby(&mut self) {
        self.editor.mode_switchable = false;
        self.banner = None;
        self.banner_rows = 0;
        if let Some(screen) = &mut self.screen {
            screen.set_banner(None);
        }
    }

    /// 交出去的 raw 模式没人接（发完一句紧接着退出了）：收回来按正常路子关掉，
    /// 别把用户的 shell 留在 raw 模式里。
    pub(in crate::cli) fn release_raw_handoff(&mut self) {
        if std::mem::take(&mut self.raw_mode_handoff) {
            drop(LiveRawMode::adopt());
        }
    }

    /// 换车道:输入框竖条换色、banner 的模式行跟着走。
    /// 把 `展开思考内容` / `展开工具内容` 交给后台任务面板。
    ///
    /// 那块面板是从日志/标记流攒步的，手上没有配置入口——不交进来的话同一件事
    /// 在前台面板跟着开关走、在后台面板永远收着（用户 09-17 实测）。每轮交一次，
    /// `/config` 改完下一轮就生效，和渲染器那侧同一个节奏。
    pub(in crate::cli) fn set_display_expand(
        &mut self,
        reasoning: bool,
        tools: bool,
        fold: bool,
        command_lines: usize,
        thought_lines: usize,
    ) {
        if let Some(screen) = &mut self.screen {
            screen.set_display_expand(reasoning, tools, fold, command_lines, thought_lines);
        }
    }

    pub(in crate::cli) fn set_mode(&mut self, mode: PersonaLane) {
        self.editor.mode = mode;
        if let Some(banner) = &mut self.banner {
            banner.set_mode(mode);
        }
    }

    /// 只读模式(09-23):状态行的「只读」、大厅模式行的那个点,跟着这一处走。
    pub(in crate::cli) fn set_readonly(&mut self, readonly: bool) {
        self.editor.readonly = readonly;
        if let Some(banner) = &mut self.banner {
            banner.set_readonly(readonly);
        }
    }

    /// 空会话 banner 走一帧:星星闪、扫光过。全屏走整帧 diff,inline 只覆写那几行。
    pub(in crate::cli) fn tick_banner(&mut self) -> Result<()> {
        if !self.rendered || self.banner.is_none() {
            return Ok(());
        }
        if self
            .screen
            .as_ref()
            .is_some_and(screen::Screen::overlay_open)
        {
            return Ok(());
        }
        let advanced = self
            .banner
            .as_mut()
            .is_some_and(crate::cli::repl::banner::BannerScene::tick);
        if !advanced {
            return Ok(());
        }
        if self.screen.is_some() {
            // 先在同步块**外**把这一帧的大厅算好（整屏的行、ANSI 串），块里只剩
            // diff 与写。kitty 处理输入法预编辑时按**活光标**定位
            // （screen_update_overlay_text），块开着的那几毫秒活光标正在星星格子上，
            // 预编辑事件落在这段就把光标画到星星上、下一帧再弹回输入框——用户
            // 09-17 报的「用输入法时光标从别处瞬移回输入框」。块越短撞上的概率
            // 越小：debug 构建实测块时长 7~12ms → 约 1ms。
            if let Some(banner) = &self.banner {
                let (cols, rows) = terminal::size().unwrap_or((80, 24));
                banner.warm_lobby(
                    usize::from(cols),
                    usize::from(rows),
                    usize::from(self.tail_rows),
                    usize::from(self.lobby_panel_rows),
                );
            }
            // 一帧一个同步块：这一帧会把输入框那几行先擦再写，不裹起来的话终端
            // （和 pyte 探针）都可能撞见擦了还没写的半帧，看着像输入框闪没了。
            let cursor = self.output_cursor;
            return synchronized_terminal_update(CursorAfterUpdate::Preserve, || {
                self.resume_at_own(cursor)
            });
        }
        if self.banner_rows == 0 {
            return Ok(());
        }
        let (cols, _) = terminal::size().unwrap_or((80, 24));
        let lines = self
            .banner
            .as_ref()
            .map(|banner| banner.render_ansi(usize::from(cols), usize::from(self.banner_rows)))
            .unwrap_or_default();
        let start = self.tail_start.saturating_add(1);
        let input_cursor = self.input_cursor;
        synchronized_terminal_update(CursorAfterUpdate::Preserve, || {
            let mut stdout = io::stdout();
            let mut row = start;
            for line in &lines {
                queue!(
                    stdout,
                    MoveTo(0, row),
                    Clear(ClearType::CurrentLine),
                    Print(line)
                )?;
                row = row.saturating_add(1);
            }
            queue!(stdout, MoveTo(input_cursor.0, input_cursor.1))?;
            stdout.flush()?;
            Ok(())
        })
    }

    pub(in crate::cli) fn mode(&self) -> PersonaLane {
        self.editor.mode
    }

    pub(in crate::cli) fn set_footer(&mut self, footer: ReplFooterStatus) {
        self.footer = footer;
        self.round_base_footer = None;
        self.footer_spinner_last = None;
        // 刷新之前开读的 Σ 作废，别让空闲循环拿它盖回来（见 `footer_generation`）。
        crate::cli::repl::jobs::invalidate_polled_cumulative();
        self.cumulative_from_poll = false;
    }

    /// 回合收尾:熄掉运行转轮并原地重绘 footer 行(不整帧重绘)。
    pub(in crate::cli) fn stop_footer_spinner(&mut self) -> Result<()> {
        if self.footer.running_spinner.take().is_none() {
            return Ok(());
        }
        self.footer_spinner_last = None;
        let Some(offset) = self.footer_offset else {
            return Ok(());
        };
        let row = self.tail_start.saturating_add(offset);
        if !self.rendered {
            return Ok(());
        }
        let (cols, rows) = terminal::size().unwrap_or((80, 24));
        if row >= rows {
            return Ok(());
        }
        let line = crate::cli::footer::repl_footer_line(
            self.editor.mode,
            self.editor.readonly,
            &self.footer,
            usize::from(cols),
        );
        let input_cursor = self.input_cursor;
        synchronized_terminal_update(CursorAfterUpdate::Preserve, || {
            let mut stdout = term_out();
            queue!(stdout, MoveTo(0, row), Print(line))?;
            queue!(stdout, MoveTo(input_cursor.0, input_cursor.1))?;
            stdout.flush()?;
            Ok(())
        })
    }

    /// 回合内一次模型请求结束:用基线+回合累计刷新计量并立即重绘。
    /// `context_tokens` 取该请求 prompt+completion,即当前上下文占用的
    /// 最新实测;回合结束后外层会用权威数字覆盖(set_footer 清基线)。
    pub(in crate::cli) fn refresh_round_usage(
        &mut self,
        context_tokens: u64,
        turn: TurnTokens,
        speed: GenerationSpeed,
    ) -> Result<()> {
        let base = self
            .round_base_footer
            .get_or_insert_with(|| Box::new(self.footer.clone()));
        let mut display = (**base).clone();
        display.apply_round_usage(context_tokens, turn, speed);
        // 基线快照拍于回合开始(转轮未起),别让计量刷新把转轮拍灭。
        display.running_spinner = self.footer.running_spinner;
        self.footer = display;
        if self.rendered && !self.external_output_active {
            synchronized_terminal_update(CursorAfterUpdate::Shown, || self.redraw())?;
        }
        Ok(())
    }

    /// Replaces the footer and redraws the live editor immediately when it is
    /// already on screen. Without the redraw, token/context updates remain
    /// invisible until the next input event causes the editor to render.
    /// Update the background-command strip; returns true when a redraw is
    /// needed (content changed, or spinners/timers must advance).
    /// 已经下过"停"的任务：状态行里先别再显示它。
    ///
    /// 停完就把状态行清空是对的（那才是事实），但守护进程那边的任务快照是
    /// **一秒轮询一次**的——停完紧接着来的那一次轮询往往还带着它，于是状态行
    /// 消失一瞬间又冒出来，闪一下（用户实测）。压住它几秒，等快照追上来。
    ///
    /// 第一版是"轮询里没有了就解除压制"，看着合理，其实当场自废：停完紧跟着的
    /// 那一句 `set_jobs(空)` 就是一次"轮询里没有"，压制表立刻被清空，下一次真
    /// 轮询把它原样带了回来。所以只能按**时间**放，不能按"这一份列表里有没有"。
    pub(in crate::cli) fn suppress_jobs<'a>(&mut self, ids: impl Iterator<Item = &'a str>) {
        let now = std::time::Instant::now();
        for id in ids {
            self.suppressed_jobs.insert(id.to_string(), now);
        }
    }

    /// 这一轮里跑着的前台子代理此刻烧了多少——先记在 Σ 上。
    ///
    /// 它的审计会话是边跑边写的，但**回合跑着的时候客户端不会去重读 Σ**
    /// （那是空闲循环干的活），所以这一截得自己补。回合收尾时 Σ 从库里重读、
    /// 这份清零，不会算两遍。
    pub(in crate::cli) fn set_live_turn_tokens(&mut self, tokens: u64) -> bool {
        if self.live_turn_tokens == tokens {
            return false;
        }
        self.live_turn_tokens = tokens;
        self.footer.update_live_extra_tokens(tokens)
    }

    pub(in crate::cli) fn set_jobs(
        &mut self,
        jobs: Vec<miyu_engine::tools::jobs::JobOverview>,
    ) -> bool {
        let jobs: Vec<miyu_engine::tools::jobs::JobOverview> = if self.suppressed_jobs.is_empty() {
            jobs
        } else {
            let now = std::time::Instant::now();
            self.suppressed_jobs
                .retain(|_, at| now.duration_since(*at) < SUPPRESS_JOB_FOR);
            jobs.into_iter()
                .filter(|job| !self.suppressed_jobs.contains_key(&job.job_id))
                .collect()
        };
        // 后台子代理**不**在这儿往 Σ 上加：它的审计会话是边跑边写的，守护进程
        // 算出来的会话累计里已经有了，再加一遍就是算两遍。前台那一路才需要补
        // （见 `set_live_turn_tokens`）——回合跑着的时候客户端不会去重读 Σ。
        let changed = self.jobs.len() != jobs.len()
            || self
                .jobs
                .iter()
                .zip(jobs.iter())
                .any(|(a, b)| a.job_id != b.job_id || a.status != b.status);
        self.jobs = jobs;
        self.refresh_job_overlay_title();
        changed
    }

    /// 面板开着哪个任务，就把那个任务此刻的抬头推给它。
    fn refresh_job_overlay_title(&mut self) {
        let Some(job_id) = self
            .screen
            .as_ref()
            .and_then(screen::Screen::overlay_job_id)
        else {
            return;
        };
        let Some(job) = self.jobs.iter().find(|job| job.job_id == job_id) else {
            return;
        };
        let title = screen::job_panel_title(job);
        if let Some(screen) = &mut self.screen {
            screen.refresh_overlay_title(&title);
        }
    }

    /// Lightweight spinner/timer repaint of the job strip only — no full
    /// tail redraw, so it can run at animation frequency without flicker.
    /// 状态行转轮此刻该画哪一帧（80ms 一帧，按时间算）。
    pub(in crate::cli) fn job_spinner_frame(&self) -> usize {
        (self.job_spinner_started.elapsed().as_millis() / 80) as usize
    }

    pub(in crate::cli) fn tick_job_strip(&mut self) -> Result<()> {
        if !self.rendered || self.jobs.is_empty() {
            return Ok(());
        }
        // 详情面板开着时整屏归它。这时候还往活动区那几行写，两个画笔会在同一
        // 块地方来回抢，屏幕上就是输入框疯狂抖动。
        if self
            .screen
            .as_ref()
            .is_some_and(screen::Screen::overlay_open)
        {
            return Ok(());
        }
        self.job_spinner = self.job_spinner_frame();
        let (cols, _) = terminal::size().unwrap_or((80, 24));
        let lines = background_job_lines(
            &self.jobs,
            self.job_spinner,
            usize::from(cols),
            self.job_hover,
        );
        let rows = lines.len().min(u16::MAX as usize) as u16;
        if rows > self.tail_rows {
            return Ok(());
        }
        let start = self
            .tail_start
            .saturating_add(self.tail_rows)
            .saturating_sub(rows);
        let input_cursor = self.input_cursor;
        // Lines are padded to the full terminal width, so plain overwrites
        // suffice — no Clear, no intermediate blank state. The synchronized
        // block keeps the cursor hop invisible over slow links (SSH).
        synchronized_terminal_update(CursorAfterUpdate::Preserve, || {
            let mut stdout = term_out();
            let mut row = start;
            for line in &lines {
                queue!(stdout, MoveTo(0, row), Print(line))?;
                row = row.saturating_add(1);
            }
            queue!(stdout, MoveTo(input_cursor.0, input_cursor.1))?;
            stdout.flush()?;
            Ok(())
        })
    }

    pub(in crate::cli) fn refresh_footer(&mut self, footer: ReplFooterStatus) -> Result<()> {
        self.set_footer(footer);
        if self.rendered {
            synchronized_terminal_update(CursorAfterUpdate::Shown, || self.redraw())?;
        }
        Ok(())
    }

    pub(in crate::cli) fn tick_spinner(
        &mut self,
        renderer: &mut render::StreamRenderer,
    ) -> Result<()> {
        self.flush_pending_chunks(renderer)?;
        renderer.tick_spinner()?;
        self.apply_renderer_frame(renderer)?;
        self.tick_footer_spinner()
    }

    /// 输入框右上角那行 `/goal …` 走一拍。
    ///
    /// 它有两个走法：状态由 daemon 推（轮次、暂停、受阻，搭任务总览那趟一秒
    /// 一次的车回来），秒数则是自己每秒往上加。所以这里比的是**画出来的那串
    /// 字**——一样就什么都不做，不一样才重画一帧（空闲时一秒最多一次）。
    pub(in crate::cli) fn tick_goal_hint(
        &mut self,
        goal: Option<miyu_core::ipc::GoalHint>,
    ) -> Result<()> {
        self.footer.goal = goal;
        if !self.rendered || self.external_output_active {
            return Ok(());
        }
        let text = crate::cli::footer::goal_hint_text(self.footer.goal.as_ref());
        if text == self.goal_hint_drawn {
            return Ok(());
        }
        self.goal_hint_drawn = text;
        synchronized_terminal_update(CursorAfterUpdate::Preserve, || self.redraw())
    }

    /// footer 里的运行转轮:单行覆写(footer 行全宽 padding,直接盖不闪),
    /// 33ms 的 spinner tick 上节流到 ~80ms 一帧。
    pub(in crate::cli) fn tick_footer_spinner(&mut self) -> Result<()> {
        if !self.rendered || self.external_output_active {
            return Ok(());
        }
        // 详情面板开着时整屏归它。这一行画在 `tail_start + offset` 上，而那一带
        // 现在是面板的地盘——两个画笔一人一帧地抢同一行，屏幕底下就在"面板页脚"
        // 和"输入框 footer"之间反复横跳（用户实测：前台子代理一点开就疯狂鬼畜）。
        // `tick_job_strip` 早就有这道闸，这儿漏了。
        if self
            .screen
            .as_ref()
            .is_some_and(screen::Screen::overlay_open)
        {
            return Ok(());
        }
        let now = std::time::Instant::now();
        if self
            .footer_spinner_last
            .is_some_and(|last| now.duration_since(last) < std::time::Duration::from_millis(80))
        {
            return Ok(());
        }
        self.footer_spinner_last = Some(now);
        self.footer.running_spinner =
            Some(self.footer.running_spinner.map_or(0, |f| f.wrapping_add(1)));
        let Some(offset) = self.footer_offset else {
            return Ok(());
        };
        let row = self.tail_start.saturating_add(offset);
        let (cols, rows) = terminal::size().unwrap_or((80, 24));
        if row >= rows {
            return Ok(());
        }
        let line = crate::cli::footer::repl_footer_line(
            self.editor.mode,
            self.editor.readonly,
            &self.footer,
            usize::from(cols),
        );
        let input_cursor = self.input_cursor;
        synchronized_terminal_update(CursorAfterUpdate::Preserve, || {
            let mut stdout = term_out();
            queue!(stdout, MoveTo(0, row), Print(line))?;
            queue!(stdout, MoveTo(input_cursor.0, input_cursor.1))?;
            stdout.flush()?;
            Ok(())
        })
    }
}

/// 下过"停"之后压住状态行多久。守护进程的任务快照一秒轮询一次，留出几轮的余量。
const SUPPRESS_JOB_FOR: std::time::Duration = std::time::Duration::from_secs(5);

pub(in crate::cli) struct LiveRawMode {
    pub(in crate::cli) restore_terminal_on_drop: bool,
    pub(in crate::cli) keyboard_enhancement: KeyboardEnhancementState,
}

impl LiveRawMode {
    /// 进入 live REPL 的 raw 输入模式，并尽量启用键盘增强协议。
    ///
    /// 参数: 无
    ///
    /// 返回:
    /// - 成功时返回会在 Drop 时恢复终端的守卫对象
    pub fn start() -> Result<Self> {
        enable_live_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(error) = execute!(stdout, EnableBracketedPaste) {
            let _ = terminal::disable_raw_mode();
            return Err(error.into());
        }
        // Focus reporting is advisory: terminals that ignore it simply never
        // send the events, and the editor stays on its "focused" default.
        let _ = execute!(stdout, EnableFocusChange);
        Ok(Self {
            restore_terminal_on_drop: true,
            keyboard_enhancement: KeyboardEnhancementState::enable(&mut stdout),
        })
    }

    /// 接管上一段 live 输入已启用的终端模式，避免重复 Push 键盘增强。
    ///
    /// 参数: 无
    ///
    /// 返回:
    /// - 会在最终 Drop 时恢复终端的守卫对象
    pub(in crate::cli) fn adopt() -> Self {
        Self {
            restore_terminal_on_drop: true,
            keyboard_enhancement: KeyboardEnhancementState::assume_active(),
        }
    }

    pub(in crate::cli) fn handoff(&mut self) {
        self.restore_terminal_on_drop = false;
        // handoff 后由下一段 LiveRawMode::adopt 继续持有键盘增强状态
        self.keyboard_enhancement = KeyboardEnhancementState::default();
    }
}

pub(in crate::cli) fn enable_live_raw_mode() -> Result<()> {
    terminal::enable_raw_mode()?;
    spawn_hangup_watchdog();
    if let Err(error) = restore_live_output_processing() {
        let _ = terminal::disable_raw_mode();
        return Err(error);
    }
    // 全屏：鼠标捕获跟着 raw 模式走。raw 一关（斜杠命令等 daemon 的那几秒），
    // 终端回到行编辑+回显，这时鼠标上报还开着的话，鼠标一动终端就把
    // `ESC[<35;x;yM` 当输入回显到屏上——/compact 等了几秒，屏上就是一大段这个
    //（用户实测截图）。
    if crate::cli::in_fullscreen() {
        let _ = execute!(io::stdout(), crossterm::event::EnableMouseCapture);
    }
    Ok(())
}

impl Drop for LiveRawMode {
    fn drop(&mut self) {
        if !self.restore_terminal_on_drop {
            return;
        }
        let mut stdout = io::stdout();
        let _ = execute!(stdout, DisableBracketedPaste, DisableFocusChange, Show);
        // 全屏：raw 关了鼠标上报也得关，见 `enable_live_raw_mode`。
        if crate::cli::in_fullscreen() {
            let _ = execute!(stdout, crossterm::event::DisableMouseCapture);
        }
        // 1. 先 Pop 键盘增强协议
        // 2. 再退出 raw mode
        self.keyboard_enhancement.disable(&mut stdout);
        let _ = terminal::disable_raw_mode();
    }
}
