//! 全屏活动区：真转轮吐的字节喂进真缓冲，画出来的时间线不能错位。
//!
//! 09-23 用户截图：一轮里工具一多，活动区（整段时间线 + 正在想的那一步）超过缓冲
//! 可写的 256 行，转轮「上移 N 行」停在只读存档边上，重画落低一截，旧的那份留在
//! 原处——同一行「思考中」画出好几份、中间隔着大段空白，而且只有 `/reset` 清得
//! 掉。改窗口宽度是第二个诱因：转轮按新宽度数行，缓冲按另一套口径重排。
//!
//! 改成锚点之后（`render::blocks::LIVE_REWIND_MARKER`），转轮每帧回到锚点整段重写，
//! 没有行数可数错。

use crate::cli::repl::tail::screen::term::Term;
use crate::cli::tests::tui_blocks::with_blocks;
use miyu_hosts::render::set_cols_override;
use miyu_hosts::render::wait_spinner::{SpinnerStyle, WaitSpinner, BLOCK_MARKER};

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

/// 块模式的活动区：`count` 个已完成的步骤，最后一行是正在跑的那一步（挂转轮）。
fn live_area(count: usize, head: &str) -> String {
    let mut rows: Vec<String> = (0..count).map(|index| format!("  step {index}")).collect();
    rows.push(format!("  {BLOCK_MARKER}{head}"));
    rows.join("\n")
}

fn tick(spinner: &mut WaitSpinner, term: &mut Term, sub: String) {
    spinner.set_sub_phase(Some(sub));
    let mut out = Vec::new();
    spinner.tick(&mut out).unwrap();
    term.feed(&out);
}

fn count(rows: &[String], needle: &str) -> usize {
    rows.iter().filter(|row| row.contains(needle)).count()
}

#[test]
fn a_live_area_taller_than_the_writable_window_is_drawn_once() {
    with_blocks(|| {
        set_cols_override(100);
        let mut term = Term::default();
        term.set_cols(100);
        term.feed(b"SENTINEL\n");
        let mut spinner = WaitSpinner::start(String::new(), SpinnerStyle::Braille);
        for (steps, head) in [
            (250, "thinking A"),
            (259, "thinking B"),
            (260, "thinking C"),
            (261, "thinking D"),
            (261, "thinking E"),
        ] {
            tick(&mut spinner, &mut term, live_area(steps, head));
        }
        set_cols_override(0);
        let rows = text(&term);
        assert_eq!(count(&rows, "thinking"), 1, "正在跑的那一步只该画一份");
        assert!(rows.iter().any(|row| row.ends_with("thinking E")));
        assert_eq!(
            rows.iter().filter(|row| row.as_str() == "  step 0").count(),
            1
        );
        assert_eq!(count(&rows, "SENTINEL"), 1, "活动区上面的正文不能被抹掉");
    });
}

#[test]
fn stopping_after_a_tall_live_area_leaves_no_orphan_rows() {
    with_blocks(|| {
        set_cols_override(100);
        let mut term = Term::default();
        term.set_cols(100);
        term.feed(b"SENTINEL\n");
        let mut spinner = WaitSpinner::start(String::new(), SpinnerStyle::Braille);
        for steps in [258, 262] {
            tick(&mut spinner, &mut term, live_area(steps, "thinking"));
        }
        let mut out = Vec::new();
        spinner.stop(&mut out).unwrap();
        term.feed(&out);
        term.feed(b"  Worked for 12s\n");
        set_cols_override(0);
        let rows = text(&term);
        assert_eq!(count(&rows, "step"), 0, "收掉之后活动区一行都不留");
        assert_eq!(count(&rows, "Worked for"), 1);
        assert_eq!(rows[..2], ["SENTINEL", "  Worked for 12s"]);
    });
}

/// 窗口收窄：全屏下转轮按终端**此刻**的宽度出帧，缓冲却要等画面那一步才改宽度
/// （`apply_output_frame` 先喂字节、后 `resize`）。转轮按新宽度把每条长行算成两行、
/// 「上移」得比缓冲里实际的行数多，一路擦进活动区上面的正文（09-23 调研）。
#[test]
fn a_resize_between_ticks_does_not_eat_the_text_above() {
    with_blocks(|| {
        let area = |head: &str| {
            let mut rows: Vec<String> = (0..6)
                .map(|index| format!("  step {index} {}", "x".repeat(84)))
                .collect();
            rows.push(format!("  {BLOCK_MARKER}{head}"));
            rows.join("\n")
        };
        set_cols_override(100);
        let mut term = Term::default();
        term.set_content_cols(96);
        term.set_cols(100);
        term.feed(b"SENTINEL\n");
        let mut spinner = WaitSpinner::start(String::new(), SpinnerStyle::Braille);
        tick(&mut spinner, &mut term, area("thinking 1"));
        // 终端先变窄：转轮这一帧已经按 60 列出了，缓冲还是 100 列。
        set_cols_override(60);
        tick(&mut spinner, &mut term, area("thinking 2"));
        // 画面那一步才把新宽度交给缓冲。
        term.set_content_cols(56);
        term.set_cols(60);
        tick(&mut spinner, &mut term, area("thinking 3"));
        set_cols_override(0);
        let rows = text(&term);
        assert_eq!(
            count(&rows, "SENTINEL"),
            1,
            "活动区上面的正文被擦掉了：{rows:#?}"
        );
        assert_eq!(count(&rows, "thinking"), 1, "{rows:#?}");
        assert_eq!(count(&rows, "step 0"), 1, "{rows:#?}");
    });
}
