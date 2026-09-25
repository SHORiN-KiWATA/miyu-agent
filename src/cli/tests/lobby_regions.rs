//! 大厅每拍不碰输入框和面板（09-25，用户选的 B 方案）。
//!
//! 输入框和大厅面板都叠在星空行上：星空行在它们占的那几列留白，只要补丁不越界，
//! 它们一拍都不用重画。越界的写法（`ESC[K` 擦行尾、行尾之外补一串空格）会把它们连着
//! 擦掉，同一帧里再压回去——支持同步输出的终端看不出来，不支持的就是一闪。

use crate::cli::repl::banner::BannerScene;
use crate::cli::repl::tail::screen::cells::patch_row;
use miyu_base::config::PersonaLane;
use std::ops::Range;
use unicode_width::UnicodeWidthChar;

/// 一段补丁落在屏上的哪些格子：写了字的（行, 列），擦行尾的（行, 起始列）。都是 0 基。
#[derive(Default)]
struct Footprint {
    written: Vec<(u16, u16)>,
    erased_from: Vec<(u16, u16)>,
}

/// 读一段补丁：只认补丁里会出现的那几样（绝对定位、改列、擦行尾、SGR、OSC 8 链接）。
fn footprint(patch: &str) -> Footprint {
    let mut print = Footprint::default();
    let (mut row, mut col) = (0u16, 0u16);
    let mut chars = patch.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\x1b' {
            let width = ch.width().unwrap_or(0) as u16;
            for offset in 0..width {
                print.written.push((row, col + offset));
            }
            col += width;
            continue;
        }
        match chars.next() {
            Some('[') => {
                let mut params = String::new();
                let mut last = '\0';
                for next in chars.by_ref() {
                    if ('\x40'..='\x7e').contains(&next) {
                        last = next;
                        break;
                    }
                    params.push(next);
                }
                let numbers: Vec<u16> = params
                    .split(';')
                    .map(|part| part.parse().unwrap_or(1))
                    .collect();
                match last {
                    'H' => {
                        row = numbers.first().copied().unwrap_or(1).saturating_sub(1);
                        col = numbers.get(1).copied().unwrap_or(1).saturating_sub(1);
                    }
                    'G' => col = numbers.first().copied().unwrap_or(1).saturating_sub(1),
                    'K' => print.erased_from.push((row, col)),
                    _ => {}
                }
            }
            // OSC（链接）：读到 BEL 或 ST 为止。
            Some(']') => {
                while let Some(next) = chars.next() {
                    if next == '\x07' || (next == '\x1b' && chars.next_if_eq(&'\\').is_some()) {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    print
}

struct Region {
    name: &'static str,
    rows: Range<u16>,
    cols: Range<u16>,
}

/// 连推 `ticks` 拍，每一拍变了的星空行都按补丁写，断言写字和擦行尾都不落进输入框、
/// 面板（`panel_rows > 0` 时）那一块。
fn assert_star_patches_stay_out(
    cols: usize,
    rows: usize,
    activity_rows: usize,
    panel_rows: usize,
    ticks: usize,
) {
    let mut scene = BannerScene::builtin_for_tests(PersonaLane::Active);
    let mut shown = scene.lobby_with_bottom_space(cols, rows, activity_rows, panel_rows);
    let mut regions = vec![Region {
        name: "输入框",
        rows: shown.tail_start..shown.tail_start + activity_rows as u16,
        cols: shown.left..shown.left + shown.width,
    }];
    if panel_rows > 0 {
        regions.push(Region {
            name: "面板",
            rows: shown.below..shown.below + panel_rows as u16,
            cols: shown.left..shown.left + shown.width,
        });
    }
    for tick in 0..ticks {
        scene.tick();
        let next = scene.lobby_with_bottom_space(cols, rows, activity_rows, panel_rows);
        assert_eq!(
            (next.tail_start, next.left, next.width, next.below),
            (shown.tail_start, shown.left, shown.width, shown.below),
            "大厅的版面在拍与拍之间不该动"
        );
        for (y, spans) in next.spans.iter().enumerate() {
            let old = shown.spans.get(y).map(Vec::as_slice).unwrap_or(&[]);
            if old == spans.as_slice() {
                continue;
            }
            let patch = patch_row(old, spans, y as u16);
            let print = footprint(&patch);
            for region in &regions {
                for &(row, col) in &print.written {
                    assert!(
                        !(region.rows.contains(&row) && region.cols.contains(&col)),
                        "{cols}x{rows} 第 {tick} 拍：星空补丁写进了{}（行 {row} 列 {col}）：{patch:?}",
                        region.name
                    );
                }
                for &(row, col) in &print.erased_from {
                    assert!(
                        !(region.rows.contains(&row) && col < region.cols.end),
                        "{cols}x{rows} 第 {tick} 拍：星空补丁从列 {col} 擦行尾，擦到了{}（行 {row}）：{patch:?}",
                        region.name
                    );
                }
            }
        }
        shown = next;
    }
}

#[test]
fn lobby_star_patches_never_touch_the_input_box() {
    for (cols, rows) in [(120, 40), (100, 32), (80, 24)] {
        assert_star_patches_stay_out(cols, rows, 5, 0, 400);
    }
}

#[test]
fn lobby_star_patches_never_touch_an_open_panel() {
    for (cols, rows) in [(120, 40), (100, 32), (80, 24)] {
        assert_star_patches_stay_out(cols, rows, 5, 9, 400);
    }
}
