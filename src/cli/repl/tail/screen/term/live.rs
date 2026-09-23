//! 活动区的锚点：全屏下转轮每一帧「回到锚点、截掉以下、重写」。
//!
//! 原来转轮每帧「上移 N 行、逐行比对重写」，N 是转轮按自己以为的行数算的。
//! 缓冲这边却只让最近 [`super::LIVE_ROWS`] 行可写（更早的压进只读存档），改窗口
//! 宽度时又按另一套口径重新折行——整段活动时间线一超过 256 行、或者宽度一变，
//! 两边的行数就对不上：上移停在存档边上，重画落低一截，旧的那份留在原处，同一
//! 行「思考中」画出好几份、中间隔着大段空白，而且错位留在缓冲里，只有 `/reset`
//! 清得掉（用户 09-23）。
//!
//! 锚点是一个**绝对行号**：转轮第一帧落在哪一行就是哪一行。之后每一帧都回到它、
//! 把它以下截掉再写，没有行数可数错；锚点以下永远留在可写的那一段，不进存档；
//! 改宽度重排时锚点跟着块和轮标记一起搬。
//!
//! 只重写变了的那一截：转轮说「从锚点下第 k 条逻辑行起」，缓冲按折行标志数过去
//! （重排折出来的续行不算一条）。上面那几行原样留着——一轮跑到上百步时，整段
//! 重写要把几百行逐字写一遍（09-23 实测 45 步时每帧 2.6ms，debug）。重写出来的
//! 行里和原来一模一样的，把**旧版本号**还回去，画面层照样跳过它们。

use super::{Cell, Term};

/// 回到锚点之前，锚点以下那一段的样子（每一行的格子与版本号）。
pub(super) struct Stash {
    anchor: usize,
    rows: Vec<(Vec<Cell>, u64)>,
}

impl Term {
    /// 活动区回到锚点下第 `from` 条逻辑行重画（见 `blocks::live_rewind_marker_at`）。
    /// 第一帧就地立锚。
    pub(super) fn live_rewind(&mut self, from: usize) {
        let fresh = self.live_anchor.is_none();
        let anchor = *self.live_anchor.get_or_insert(self.cursor_row());
        // 锚是这一下才立的（不该发生：转轮的第一帧总是从 0 起）就只能从锚写起，
        // 上面那几行没人补——宁可少几行，也别写到不知道哪儿去。
        let target = if fresh {
            anchor
        } else {
            self.logical_row_below(anchor, from)
        };
        // 一次 `feed` 里可能不止一帧：比对要拿这一批**之前**的样子，只记第一次。
        if self.live_stash.is_none() {
            let rows = self.live_rows_from(target);
            self.live_stash = Some(Stash {
                anchor: target,
                rows,
            });
        }
        // 活动区从一条新的逻辑行开始：它上面那一行要是还挂着「折下来接着写」，
        // 改宽度重排时活动区第一行会被并进上一段正文里。
        if target == anchor {
            if let Some(above) = anchor.checked_sub(1) {
                self.set_wrapped_at(above, false);
            }
        }
        self.truncate_rows(target);
    }

    /// 从第 `row` 行往下数过 `lines` 条逻辑行，落在哪一行。缓冲自己折出来的续行
    /// （改宽度重排之后常见）不算一条——转轮那边数的是它写出来的行。
    fn logical_row_below(&self, row: usize, lines: usize) -> usize {
        let total = self.line_count();
        let mut row = row;
        for _ in 0..lines {
            loop {
                row += 1;
                if row >= total || !self.row_wrapped(row - 1) {
                    break;
                }
            }
        }
        // 数过头了（缓冲里行不够）：从最后一行写起，不往空处跳。
        row.min(total.saturating_sub(1))
    }

    /// 活动区收掉（见 `blocks::LIVE_END_MARKER`）：回到锚、截掉锚以下、拔锚。
    pub(super) fn live_end(&mut self) {
        if let Some(anchor) = self.live_anchor.take() {
            self.truncate_rows(anchor);
        }
    }

    /// 一次 `feed` 吃完：锚点以下内容没变的行，把重写之前的版本号还回去。
    pub(super) fn settle_live_rows(&mut self) {
        let Some(stash) = self.live_stash.take() else {
            return;
        };
        let base = self.archive.len();
        for (offset, (cells, stamp)) in stash.rows.into_iter().enumerate() {
            let Some(live) = (stash.anchor + offset).checked_sub(base) else {
                continue;
            };
            if self.lines.get(live) != Some(&cells) {
                continue;
            }
            if let Some(slot) = self.stamps.get_mut(live) {
                *slot = stamp;
            }
        }
    }

    /// 锚点以下每一行的格子与版本号。锚点不在可写段里（不该发生）就是空的。
    fn live_rows_from(&self, anchor: usize) -> Vec<(Vec<Cell>, u64)> {
        let Some(from) = anchor.checked_sub(self.archive.len()) else {
            return Vec::new();
        };
        self.lines
            .iter()
            .enumerate()
            .skip(from)
            .map(|(index, cells)| (cells.clone(), self.stamps.get(index).copied().unwrap_or(0)))
            .collect()
    }

    /// 按绝对行号改「这一行是折下来的」标志（存档段也改得到）。
    fn set_wrapped_at(&mut self, row: usize, wrapped: bool) {
        match row.checked_sub(self.archive.len()) {
            Some(live) => self.set_wrapped(live, wrapped),
            None => self.archive[row].wrapped = wrapped,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use miyu_hosts::render::blocks::{live_rewind_marker_at, LIVE_END_MARKER, LIVE_REWIND_MARKER};

    fn text(term: &Term) -> Vec<String> {
        (0..term.line_count())
            .map(|row| {
                term.row_spans(row)
                    .iter()
                    .map(|span| span.text.as_str())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    /// 转轮一帧：回到锚点，然后整段写。
    fn frame(rows: &[String]) -> String {
        format!("{LIVE_REWIND_MARKER}{}", rows.join("\n"))
    }

    fn steps(count: usize, head: &str) -> Vec<String> {
        let mut rows: Vec<String> = (0..count).map(|index| format!("  step {index}")).collect();
        rows.push(format!("  {head}"));
        rows
    }

    /// 09-23 截图那一幕：活动区超过 256 行之后再长一行。原来上移停在存档边上，
    /// 重画落低一截，同一行抬头画出好几份。
    #[test]
    fn a_live_area_taller_than_the_writable_window_redraws_in_place() {
        let mut term = Term::default();
        term.set_cols(80);
        term.feed(b"SENTINEL\n");
        for (count, head) in [
            (259, "thinking 1s"),
            (260, "thinking 2s"),
            (261, "thinking 3s"),
        ] {
            term.feed(frame(&steps(count, head)).as_bytes());
            term.feed(frame(&steps(count, head)).as_bytes());
        }
        let rows = text(&term);
        let heads = rows.iter().filter(|row| row.contains("thinking")).count();
        assert_eq!(heads, 1, "抬头只该有一份");
        assert_eq!(
            rows.iter().filter(|row| row.as_str() == "SENTINEL").count(),
            1
        );
        assert_eq!(
            rows.iter().filter(|row| row.as_str() == "  step 0").count(),
            1
        );
        assert_eq!(rows.last().map(String::as_str), Some("  thinking 3s"));
    }

    /// 收掉之后接着写的内容从锚那一行起；活动区不留残行。
    #[test]
    fn ending_the_live_area_leaves_the_cursor_on_the_anchor_row() {
        let mut term = Term::default();
        term.set_cols(80);
        term.feed(b"before\n");
        term.feed(frame(&steps(3, "running")).as_bytes());
        term.feed(LIVE_END_MARKER.as_bytes());
        term.feed(b"after\n");
        assert_eq!(text(&term), vec!["before", "after", ""]);
    }

    /// 窗口收窄：活动区的行被重排折成两行之后，下一帧照样整段替换，不留旧的。
    #[test]
    fn a_resize_between_frames_cannot_leave_stale_rows() {
        let mut term = Term::default();
        term.set_content_cols(96);
        term.set_cols(100);
        term.feed(b"SENTINEL\n");
        let wide: Vec<String> = (0..4)
            .map(|index| format!("  {index} {}", "x".repeat(90)))
            .collect();
        term.feed(frame(&wide).as_bytes());
        term.set_content_cols(56);
        term.set_cols(60);
        let narrow: Vec<String> = (0..4)
            .map(|index| format!("  {index} {}", "x".repeat(50)))
            .collect();
        term.feed(frame(&narrow).as_bytes());
        let rows = text(&term);
        assert_eq!(rows[0], "SENTINEL");
        assert_eq!(&rows[1..5], narrow.as_slice(), "{rows:#?}");
        assert!(rows[5..].iter().all(String::is_empty), "{rows:#?}");
    }

    /// 整段重写不等于整段重画：没变的行版本号不动，画面层才会跳过它们。
    #[test]
    fn unchanged_live_rows_keep_their_version() {
        let mut term = Term::default();
        term.set_cols(80);
        term.feed(frame(&steps(5, "thinking 1s")).as_bytes());
        let before: Vec<u64> = (0..term.line_count())
            .map(|row| term.row_stamp(row))
            .collect();
        term.feed(frame(&steps(5, "thinking 2s")).as_bytes());
        let after: Vec<u64> = (0..term.line_count())
            .map(|row| term.row_stamp(row))
            .collect();
        assert_eq!(
            before[..5],
            after[..5],
            "五个没变的步骤行版本号要原样还回来"
        );
        assert_ne!(before[5], after[5], "抬头变了，版本号得变");
    }

    /// 活动区里的块每帧跟着重写：只留一份，行号对得上。
    #[test]
    fn live_blocks_are_not_duplicated_across_frames() {
        let mut term = Term::default();
        term.set_cols(80);
        term.feed(b"before\n");
        let block = |secs: u32| {
            vec![
                "  step".to_string(),
                format!(
                    "{}  thinking {secs}s{}",
                    miyu_hosts::render::blocks::begin_marker(42),
                    miyu_hosts::render::blocks::END_MARKER
                ),
            ]
        };
        for secs in 1..5 {
            term.feed(frame(&block(secs)).as_bytes());
        }
        let blocks: Vec<_> = term
            .blocks()
            .iter()
            .map(|block| (block.id, block.start, block.end))
            .collect();
        assert_eq!(blocks, vec![(42, 2, 3)]);
    }

    /// 只重写变了的那一截：上面几行原样留着（版本号都不动），下面换新。
    #[test]
    fn a_partial_rewind_keeps_the_rows_above_it() {
        let mut term = Term::default();
        term.set_cols(80);
        term.feed(b"SENTINEL\n");
        term.feed(frame(&steps(4, "thinking 1s")).as_bytes());
        let before: Vec<u64> = (0..term.line_count())
            .map(|row| term.row_stamp(row))
            .collect();
        term.feed(
            format!(
                "{}  step 3\n  thinking 2s\n  new row",
                live_rewind_marker_at(3)
            )
            .as_bytes(),
        );
        let rows = text(&term);
        assert_eq!(
            rows,
            vec![
                "SENTINEL",
                "  step 0",
                "  step 1",
                "  step 2",
                "  step 3",
                "  thinking 2s",
                "  new row"
            ]
        );
        let after: Vec<u64> = (0..term.line_count())
            .map(|row| term.row_stamp(row))
            .collect();
        assert_eq!(before[..4], after[..4], "没碰的行版本号不动");
    }

    /// 改宽度重排把活动区的一行折成了两行：「第 k 条逻辑行」要跳过折出来的续行。
    #[test]
    fn a_partial_rewind_counts_logical_rows_across_wraps() {
        let mut term = Term::default();
        term.set_content_cols(96);
        term.set_cols(100);
        let long = format!("  0 {}", "x".repeat(90));
        term.feed(frame(&[long.clone(), "  1 short".into(), "  head 1".into()]).as_bytes());
        // 收窄：第 0 行被折成两行。
        term.set_content_cols(56);
        term.set_cols(60);
        assert_eq!(term.line_count(), 4, "{:#?}", text(&term));
        term.feed(format!("{}  head 2", live_rewind_marker_at(2)).as_bytes());
        let rows = text(&term);
        assert_eq!(rows.len(), 4, "{rows:#?}");
        assert_eq!(rows[2], "  1 short", "{rows:#?}");
        assert_eq!(rows[3], "  head 2", "{rows:#?}");
    }

    /// 贴着屏宽的活动区行，重排时按屏宽折：不会因为正文区窄了两格就被拆开。
    #[test]
    fn live_rows_reflow_by_the_screen_width() {
        let mut term = Term::default();
        term.set_content_cols(96);
        term.set_cols(100);
        term.feed(b"SENTINEL\n");
        let edge = format!("  {}", "x".repeat(97));
        term.feed(frame(&[edge.clone(), "  head".into()]).as_bytes());
        term.set_content_cols(96);
        term.set_cols(101);
        term.set_cols(100);
        let rows = text(&term);
        assert_eq!(rows, vec!["SENTINEL".to_string(), edge, "  head".into()]);
    }
}
