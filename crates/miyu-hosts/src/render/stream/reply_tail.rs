//! 正文的活尾巴（09-25）：还没落下的那一截——没收到换行的半行、没闭合的代码块——先在
//! 活动区里露出来，写完了照常落进正文。
//!
//! 正文按整行落（`MarkdownStreamRenderer::push` 只吐整行），代码块要等闭合的围栏：长代码块
//! 流着的时候正文区一动不动，写完才一口气出来。思考早就有滚动窗，正文这边补上同一样东西。
//!
//! 落下去的字节一个不变：尾巴只画在活动区（`live_area`），有整行落下时同一帧里擦掉、写正文、
//! 再画。和转轮不同时在屏上——活动区只有一块（全屏缓冲只有一个锚），起转轮、半行冲出去、
//! 回合收尾之前都先擦掉（`stop_waiting` 顺带擦）。
//!
//! 一截字至少搁了一拍（[`SPINNER_INTERVAL`]）还没落下才露出来：流得快、马上就写完的行照旧
//! 直接落下，不闪一下；一口气喂完、不 tick 的用例（金样）字节不变。

use super::timeline;
use crate::render::live_area::{LiveArea, Rewrite};
use crate::render::*;
use crossterm::queue;
use crossterm::terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate};
use std::time::Instant;

#[derive(Default)]
pub(crate) struct ReplyTail {
    area: LiveArea,
    /// 上一次画的时候，那一截的指纹、终端宽度、最多几行。没变就不重算。
    painted: Option<(u64, usize, usize)>,
    /// 这一截从什么时候起搁着没落下（还没画出来时才记）。
    pending_since: Option<Instant>,
    /// 上一次画是什么时候：流着的时候每拍最多画一次。
    last_paint: Option<Instant>,
}

impl ReplyTail {
    pub(crate) fn is_visible(&self) -> bool {
        !self.area.is_empty()
    }
}

impl StreamRenderer {
    /// 这个面有没有活尾巴：有活动区的终端（全屏或静态时间线）才有。管道、`--plain` 没有。
    fn reply_tail_enabled(&self) -> bool {
        !self.plain && self.timeline_enabled() && WaitSpinner::supported()
    }

    /// 最多露几行：跟思考滚动窗同一个设置，再多也不超过活动区放得下的（静态面的活动区
    /// 高过一屏就擦不干净）。思考不开窗（设成 0）也至少露那半行。
    fn reply_tail_rows_cap(&self) -> usize {
        self.thinking_scroll_lines
            .min(timeline::live_thought_rows().saturating_sub(1))
            .max(1)
    }

    /// 每一拍（`tick_spinner`）：正文流着、活动区空着（没有转轮）时，把还没落下的那一截画上去。
    pub(crate) fn refresh_reply_tail(&mut self, now: Instant) -> Result<()> {
        if !self.reply_tail_enabled()
            || self.mode != Some(ChatStreamKind::Content)
            || self.wait_spinner.is_some()
        {
            return Ok(());
        }
        let key = (
            self.markdown.pending_fingerprint(),
            crate::render::terminal_cols(120),
            self.reply_tail_rows_cap(),
        );
        if self.reply_tail.painted == Some(key) {
            return Ok(());
        }
        if !self.reply_tail.is_visible() {
            let since = *self.reply_tail.pending_since.get_or_insert(now);
            if now.saturating_duration_since(since) < SPINNER_INTERVAL {
                return Ok(());
            }
        } else if self
            .reply_tail
            .last_paint
            .is_some_and(|last| now.saturating_duration_since(last) < SPINNER_INTERVAL)
        {
            return Ok(());
        }
        let own_block = self.sync_depth == 0;
        self.paint_reply_tail(key, own_block)?;
        self.reply_tail.last_paint = Some(now);
        Ok(())
    }

    /// 写一段已经落定的正文。尾巴挂着的话同一帧里擦掉、写、接着画剩下的那一截，不留空当。
    pub(crate) fn write_committed_body(&mut self, rendered: &str) -> Result<()> {
        let now = Instant::now();
        if rendered.is_empty() {
            // 这一片没有整行落下：那一截从头一回搁着起计时。
            self.reply_tail.pending_since.get_or_insert(now);
            return Ok(());
        }
        if !self.reply_tail.is_visible() {
            // 落下了一段：剩下那一截从现在起计时。
            self.reply_tail.pending_since = Some(now);
            write!(self.output, "{rendered}")?;
            return Ok(());
        }
        // 全屏的字节进的是缓冲，成帧由画面那一层管，不裹同步标记。
        let own_block = self.sync_depth == 0 && !self.caps().expandable;
        if own_block {
            queue!(self.output, BeginSynchronizedUpdate)?;
        }
        self.reply_tail.area.clear(&mut self.output, false)?;
        write!(self.output, "{rendered}")?;
        let key = (
            self.markdown.pending_fingerprint(),
            crate::render::terminal_cols(120),
            self.reply_tail_rows_cap(),
        );
        self.paint_reply_tail(key, false)?;
        if own_block {
            queue!(self.output, EndSynchronizedUpdate)?;
        }
        self.output.flush()?;
        Ok(())
    }

    /// 擦掉活尾巴（起转轮、半行冲出去、回合收尾之前）。那一截还在 markdown 缓冲里，落下时
    /// 照常写出来。
    pub(crate) fn clear_reply_tail(&mut self) -> Result<()> {
        self.reply_tail.painted = None;
        self.reply_tail.pending_since = None;
        if !self.reply_tail.is_visible() {
            return Ok(());
        }
        let own_block = self.sync_depth == 0;
        self.reply_tail.area.clear(&mut self.output, own_block)
    }

    /// 按 `key`（指纹、宽度、行数上限）画这一截；没东西了就擦掉。
    fn paint_reply_tail(&mut self, key: (u64, usize, usize), synchronized: bool) -> Result<()> {
        let (_, width, cap) = key;
        let rows = self.reply_tail_frame(width, cap);
        self.reply_tail.painted = Some(key);
        if rows.is_empty() {
            self.reply_tail.pending_since = None;
            return self.reply_tail.area.clear(&mut self.output, synchronized);
        }
        let widths = rows
            .iter()
            .map(|row| crate::render::command_ansi_width(row))
            .collect();
        self.reply_tail.area.paint(
            &mut self.output,
            rows,
            widths,
            width,
            synchronized,
            Rewrite::Whole,
        )
    }

    /// 这一截折好的行，只留最后 `cap` 行。全屏按正文那套缩进折（和落下之后一个样子，续行
    /// 带折行标记，所以整段重写）；静态面按终端宽度减一折好，不留软折行（活动区按行比对
    /// 与擦除都靠一行就是一行）。
    fn reply_tail_frame(&self, width: usize, cap: usize) -> Vec<String> {
        let preview = self.markdown.pending_preview(cap);
        if preview.is_empty() {
            return Vec::new();
        }
        let rows: Vec<String> = if self.caps().expandable {
            timeline::indent_body(&preview.join("\n"))
                .split('\n')
                .map(str::to_string)
                .collect()
        } else {
            let usable = width.saturating_sub(1).max(1);
            preview
                .iter()
                .flat_map(|row| crate::render::wrap_display_text(row, usable))
                .collect()
        };
        let keep = rows.len().saturating_sub(cap);
        rows[keep..].to_vec()
    }
}
