//! 按格子比对一行：屏上是旧版，只写新版里变了的那几段。
//!
//! 大厅星空（09-23 实测）：一帧只变百来格，按行比对却要把三十多行整行重写，每帧
//! 14KB、每秒 359KB 灌给终端。按格子比对之后只发变了的星星，每秒 112KB。无头 kitty
//! （软件渲染）的 CPU 几乎不随字节变（168% → 163%，大头是每帧重画），省下的是终端
//! 解析的量，以及一帧能一口气收完。
//!
//! 前提是**屏上确实是旧版**：调用方要先对过「这一行上一帧画的就是它」，对不上就
//! 整行重写，别在不知道底色的地方打补丁。

use super::ansi::{spans_to_ansi, AnsiSpan};
use ratatui::style::Style;
use std::fmt::Write as _;
use unicode_width::UnicodeWidthChar;

/// 两段变化之间隔着这么几格没变的，就连着一起重写：一次光标定位（`ESC[行;列H`
/// 六到八个字节）比重写几个空格还贵。
const BRIDGE: usize = 4;

/// 屏上的一格。宽字符占两格，后一格的 `ch` 是 `'\0'`（接续格，不单独画）。
#[derive(Clone, Copy, PartialEq, Eq)]
struct Cell<'a> {
    ch: char,
    /// 跟在这个字后面的零宽记号（组合附标、kitty 占位符的行列记号）。
    marks: &'a str,
    style: Style,
    link: Option<&'a str>,
}

/// 一个字占几格。
///
/// 可见 ASCII 直接给 1：大厅一帧要展开近万个格子，绝大多数是空格，debug 构建下
/// 每个都去查 Unicode 宽度表，光这一项就让大厅的 CPU 比整行重写时还高（09-23）。
fn char_width(ch: char) -> usize {
    match ch {
        ' '..='~' => 1,
        _ if ch.is_ascii() => 0,
        _ => ch.width().unwrap_or(0),
    }
}

/// 一行展开成逐列的格子。
fn cells(spans: &[AnsiSpan]) -> Vec<Cell<'_>> {
    // 字节数是格子数的上界（宽字符占两格，但至少三个字节），一次分配到位。
    let mut out: Vec<Cell> = Vec::with_capacity(spans.iter().map(|span| span.text.len()).sum());
    for span in spans {
        let link = span.link.as_deref();
        let text = span.text.as_str();
        // 这一段里最近一个有宽度的字：它在 `out` 里的下标、它的零宽记号从哪个字节起。
        let mut lead: Option<(usize, usize)> = None;
        for (at, ch) in text.char_indices() {
            let end = at + ch.len_utf8();
            let width = char_width(ch);
            if width == 0 {
                // 零宽记号挂在前一格上，和终端里「组合进前一个字」一致。段首就是
                // 零宽记号（不该有）：没有前一格可挂，丢掉。
                if let Some((index, marks_start)) = lead {
                    out[index].marks = &text[marks_start..end];
                }
                continue;
            }
            let cell = Cell {
                ch,
                marks: "",
                style: span.style,
                link,
            };
            lead = Some((out.len(), end));
            out.push(cell);
            if width == 2 {
                out.push(Cell { ch: '\0', ..cell });
            }
        }
    }
    out
}

/// 一段格子画回 ANSI（样式与链接照原样，接续格跳过）。
fn render_cells(cells: &[Cell]) -> String {
    let mut spans: Vec<AnsiSpan> = Vec::new();
    for cell in cells {
        if cell.ch == '\0' {
            continue;
        }
        match spans.last_mut() {
            Some(last) if last.style == cell.style && last.link.as_deref() == cell.link => {
                last.text.push(cell.ch);
                last.text.push_str(cell.marks);
            }
            _ => {
                let mut text = String::new();
                text.push(cell.ch);
                text.push_str(cell.marks);
                spans.push(AnsiSpan {
                    text,
                    style: cell.style,
                    link: cell.link.map(str::to_string),
                });
            }
        }
    }
    spans_to_ansi(&spans)
}

/// 把屏幕第 `y` 行从 `old` 改成 `new` 要写的字节。两边都从第 0 列画起；一样就是空串。
///
/// 只写变了的段：每段先定位（同一行里第二段起用只改列的 `ESC[列G`），再按新版的
/// 样式写字；新版比旧版短，只在旧版有字的格子上补空格（不擦行尾，见 `cell_patch`）。
/// 写完样式复位，不留给下一笔。
pub(in crate::cli) fn patch_row(old: &[AnsiSpan], new: &[AnsiSpan], y: u16) -> String {
    aligned_patch(old, new, y).unwrap_or_else(|| cell_patch(old, new, y))
}

/// 一段占几格。
fn span_width(span: &AnsiSpan) -> usize {
    span.text.chars().map(char_width).sum()
}

fn single_char(span: &AnsiSpan) -> bool {
    let mut chars = span.text.chars();
    chars.next().is_some() && chars.next().is_none()
}

/// 两版切成了同样的片段、变了的段都只有一个字时，按片段比，不展开成格子。
///
/// 大厅就是这样：一格一段，星星原地闪、扫光原地换色，片段边界帧帧不动。逐格比对
/// 要把新旧两版都展开再逐格比，debug 下一帧 1.2ms，大厅的 CPU 因此比整行重写还高
/// （09-23 实测 8.6% 对 5.7%）；按片段比一帧 0.4ms，CPU 6.7%。这条路写出的字节和
/// 逐格比对一模一样。对不齐（段数不同、变了的段不止一个字或宽度变了）返回 `None`，
/// 交给逐格比对。
fn aligned_patch(old: &[AnsiSpan], new: &[AnsiSpan], y: u16) -> Option<String> {
    if old.len() != new.len() {
        return None;
    }
    let mut out = String::new();
    // 正在攒的一段改动：起始列、起止片段；后面跟着多少列没变的。
    let mut run: Option<(usize, usize, usize)> = None;
    let mut gap = 0;
    let mut col = 0;
    for (index, (was, now)) in old.iter().zip(new).enumerate() {
        let width = span_width(now);
        if was == now {
            if run.is_some() {
                gap += width;
                if gap > BRIDGE {
                    emit_run(&mut out, new, run.take(), y);
                }
            }
        } else {
            // 只认一个字的段：多字的段整段重写会比逐格多写，精度也丢了；零宽的一段
            // （组合附标自成一段）定位过去再写会挂到别的字上。都交给逐格比对。
            if !single_char(now) || !single_char(was) || width == 0 || span_width(was) != width {
                return None;
            }
            match &mut run {
                Some((_, _, end)) => *end = index + 1,
                None => run = Some((col, index, index + 1)),
            }
            gap = 0;
        }
        col += width;
    }
    emit_run(&mut out, new, run, y);
    Some(out)
}

/// 写出一段改动：定位（本行第一段用 `ESC[行;列H`，之后只改列），再照新版画。
fn emit_run(out: &mut String, new: &[AnsiSpan], run: Option<(usize, usize, usize)>, y: u16) {
    let Some((col, start, end)) = run else {
        return;
    };
    if out.is_empty() {
        let _ = write!(out, "\x1b[{};{}H", y + 1, col + 1);
    } else {
        let _ = write!(out, "\x1b[{}G", col + 1);
    }
    out.push_str(&spans_to_ansi(&new[start..end]));
}

/// 行尾之外的格子：默认样式的空格（行尾的空白本来就被 `trim_end_spans` 去掉了）。
const BLANK: Cell<'static> = Cell {
    ch: ' ',
    marks: "",
    style: Style::new(),
    link: None,
};

/// 逐格比对：片段对不齐时的通用做法。
///
/// 两行比到较长的那一行为止，行尾之外按空格算：新版短了，只在旧版有字的格子上补空格；
/// 新版长了，也只写新版有字的格子。**新旧两版都是空白的格子永不写、永不擦。**以前短了
/// 用 `ESC[K` 擦行尾、长了把旧行尾之后整段当成「变了」重写，大厅里叠在星空行上的输入框
/// 和面板就被连着擦掉，同一帧里再压回去——不支持同步输出的终端上是一闪（09-25）。
fn cell_patch(old: &[AnsiSpan], new: &[AnsiSpan], y: u16) -> String {
    let old = cells(old);
    let mut new = cells(new);
    let width = old.len().max(new.len());
    new.resize(width, BLANK);
    let same = |x: usize| old.get(x).copied().unwrap_or(BLANK) == new[x];
    let mut out = String::new();
    let mut x = 0;
    while x < new.len() {
        if same(x) {
            x += 1;
            continue;
        }
        // 从宽字符的后半格开始变的话，前半格也得重写——终端不认半个字。
        let start = if new[x].ch == '\0' {
            x.saturating_sub(1)
        } else {
            x
        };
        let mut end = x + 1;
        loop {
            while end < new.len() && !same(end) {
                end += 1;
            }
            match (end..new.len()).find(|&at| !same(at)) {
                Some(next) if next - end <= BRIDGE => end = next,
                _ => break,
            }
        }
        // 段尾落在宽字符的前半格上：把后半格也带上，字才完整。
        if end < new.len() && new[end].ch == '\0' {
            end += 1;
        }
        if out.is_empty() {
            let _ = write!(out, "\x1b[{};{}H", y + 1, start + 1);
        } else {
            let _ = write!(out, "\x1b[{}G", start + 1);
        }
        out.push_str(&render_cells(&new[start..end]));
        x = end;
    }
    out
}

/// 去掉行尾默认样式的空白：和 `spans_to_ansi(..).trim_end()` 画出来的是同一行，
/// 格子比对的两边才对得上整行重写的那一版。
pub(in crate::cli) fn trim_end_spans(mut spans: Vec<AnsiSpan>) -> Vec<AnsiSpan> {
    while let Some(last) = spans.last_mut() {
        if last.style != Style::new() || last.link.is_some() {
            break;
        }
        let kept = last.text.trim_end_matches(' ').len();
        if kept > 0 {
            last.text.truncate(kept);
            break;
        }
        spans.pop();
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::repl::tail::screen::term::Term;
    use ratatui::style::Color;

    fn span(text: &str, fg: Option<Color>) -> AnsiSpan {
        let style = fg.map_or(Style::new(), |color| Style::new().fg(color));
        AnsiSpan::styled(text, style)
    }

    /// `Term` 不认绝对定位（正文流里本来就不该有），补丁画在第 0 行，把开头那句
    /// `ESC[1;列H` 换成「回车 + 改列」，意思一样。
    fn on_row_zero(patch: &str) -> String {
        let Some(rest) = patch.strip_prefix("\x1b[1;") else {
            return patch.to_string();
        };
        let end = rest.find('H').expect("补丁以定位开头");
        format!("\r\x1b[{}G{}", &rest[..end], &rest[end + 1..])
    }

    /// 在虚拟终端里先画旧版、再打补丁，得到的那一行要和直接画新版一模一样。
    fn assert_patch_lands(old: &[AnsiSpan], new: &[AnsiSpan]) {
        let patch = patch_row(old, new, 0);
        let mut patched = Term::default();
        patched.set_cols(80);
        let mut fresh = Term::default();
        fresh.set_cols(80);
        let paint = |term: &mut Term, spans: &[AnsiSpan]| {
            term.feed(format!("\r\x1b[K{}", spans_to_ansi(spans)).as_bytes());
        };
        paint(&mut patched, old);
        patched.feed(on_row_zero(&patch).as_bytes());
        paint(&mut fresh, new);
        // 补丁不擦行尾，旧版多出来的格子补成空格：行尾的空白去掉再比。
        assert_eq!(
            trim_end_spans(patched.row_spans(0)),
            trim_end_spans(fresh.row_spans(0)),
            "patch {patch:?}"
        );
    }

    #[test]
    fn identical_rows_need_no_bytes() {
        let row = vec![span("  ✦    ", Some(Color::Blue)), span("MIYU", None)];
        assert_eq!(patch_row(&row, &row, 5), "");
    }

    #[test]
    fn a_twinkling_star_rewrites_only_its_cell() {
        let old = vec![
            span("      ", None),
            span("+", Some(Color::Blue)),
            span("      end", None),
        ];
        let new = vec![
            span("      ", None),
            span("✦", Some(Color::Cyan)),
            span("      end", None),
        ];
        let patch = patch_row(&old, &new, 0);
        assert!(patch.starts_with("\x1b[1;7H"), "{patch:?}");
        assert!(!patch.contains("end"), "没变的格子不该重写：{patch:?}");
        assert_patch_lands(&old, &new);
    }

    #[test]
    fn far_apart_changes_become_separate_runs_on_the_same_row() {
        let old = vec![span("a                    b", None)];
        let new = vec![span("x                    y", None)];
        let patch = patch_row(&old, &new, 2);
        assert!(patch.starts_with("\x1b[3;1H"), "{patch:?}");
        assert!(patch.contains("\x1b[22G"), "第二段只改列：{patch:?}");
        assert_patch_lands(&old, &new);
    }

    #[test]
    fn a_shorter_row_blanks_only_the_cells_it_lost() {
        let old = vec![span("abcdef", Some(Color::Red))];
        let new = vec![span("abc", Some(Color::Red))];
        let patch = patch_row(&old, &new, 0);
        assert!(
            patch.starts_with("\x1b[1;4H"),
            "从丢掉的第一格起：{patch:?}"
        );
        assert!(!patch.contains("\x1b[K"), "不擦行尾：{patch:?}");
        assert_patch_lands(&old, &new);
    }

    /// 大厅的输入框、面板叠在星空行上，星空行在那几列留白：两版都是空白的格子一格都
    /// 不能碰，不管新版比旧版短（以前擦行尾）还是长（以前把旧行尾之后整段重写）。
    #[test]
    fn cells_blank_in_both_rows_are_never_written() {
        let star = |text: &str| span(text, Some(Color::Blue));
        // 右边那颗星灭了：只在第 31 列补一个空格。
        let old = vec![
            span("  ", None),
            star("+"),
            span(&" ".repeat(27), None),
            star("✦"),
        ];
        let new = vec![span("  ", None), star("+")];
        let patch = patch_row(&old, &new, 0);
        assert_eq!(
            patch,
            format!("\x1b[1;31H{}", spans_to_ansi(&[span(" ", None)]))
        );
        assert_patch_lands(&old, &new);
        // 右边亮了一颗：只写那一格，中间的空白不重写。
        let patch = patch_row(&new, &old, 0);
        assert!(patch.starts_with("\x1b[1;31H"), "{patch:?}");
        assert!(!patch.contains("   "), "中间的空白不该写：{patch:?}");
        assert_patch_lands(&new, &old);
    }

    #[test]
    fn wide_characters_are_never_split() {
        let old = vec![span("普通 · 开发", None)];
        let new = vec![span("普通 · 开发", Some(Color::Magenta))];
        assert_patch_lands(&old, &new);
        // 变化从宽字符的后半格开始：前半格要一起重写。
        let old = vec![span("ab模式", None)];
        let new = vec![span("ab模x", None)];
        assert_patch_lands(&old, &new);
        let old = vec![span("你好", None)];
        let new = vec![span("你ab", None)];
        assert_patch_lands(&old, &new);
    }

    /// 大厅那样一格一段的行：随机闪几颗星、换几个色，按片段比与逐格比写出的字节
    /// 必须一模一样，落到终端里也和直接画新版一样。
    #[test]
    fn aligned_rows_patch_exactly_like_the_cell_path() {
        let glyphs = [" ", ".", "+", "✦", "✶"];
        let colors = [
            None,
            Some(Color::Blue),
            Some(Color::Cyan),
            Some(Color::Rgb(90, 80, 70)),
        ];
        let mut seed: u64 = 0x5eed;
        let mut next = move |bound: usize| {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 33) as usize % bound
        };
        for _ in 0..200 {
            let old: Vec<AnsiSpan> = (0..60)
                .map(|_| span(glyphs[next(glyphs.len())], colors[next(colors.len())]))
                .collect();
            let mut new = old.clone();
            for _ in 0..next(12) {
                let at = next(new.len());
                new[at] = span(glyphs[next(glyphs.len())], colors[next(colors.len())]);
            }
            for y in [0, 7] {
                assert_eq!(
                    aligned_patch(&old, &new, y),
                    Some(cell_patch(&old, &new, y)),
                    "{old:?} -> {new:?}"
                );
            }
            assert_patch_lands(&old, &new);
        }
    }

    #[test]
    fn rows_that_do_not_line_up_fall_back_to_cells() {
        // 段数不同。
        let old = vec![span("ab", None), span("cd", Some(Color::Red))];
        let new = vec![span("abcd", None)];
        assert_eq!(aligned_patch(&old, &new, 0), None);
        assert_patch_lands(&old, &new);
        // 段数相同，一个字的段变宽（半角换成全角）。
        let old = vec![span("a", None), span("x", None)];
        let new = vec![span("你", None), span("x", None)];
        assert_eq!(aligned_patch(&old, &new, 0), None);
        assert_patch_lands(&old, &new);
        // 变了的段不止一个字：整段重写会丢掉格级的精度。
        let old = vec![
            span("  ", None),
            span("MIYU", Some(Color::Blue)),
            span(" x", None),
        ];
        let new = vec![
            span("  ", None),
            span("MIKU", Some(Color::Blue)),
            span(" x", None),
        ];
        assert_eq!(aligned_patch(&old, &new, 0), None);
        assert!(!patch_row(&old, &new, 0).contains("MI"), "只重写变了的那格");
        assert_patch_lands(&old, &new);
        // 短了一截：尾巴由逐格那条路补空格。
        let old = vec![span("abc", None)];
        let new = vec![span("ab", None)];
        assert_eq!(aligned_patch(&old, &new, 0), None);
        assert_patch_lands(&old, &new);
    }

    /// 一格一段的行里夹着全角字：宽度不变就照样走片段比。
    #[test]
    fn single_wide_characters_stay_on_the_aligned_path() {
        let old = vec![
            span("你", None),
            span(" ", None),
            span("+", Some(Color::Blue)),
        ];
        let new = vec![
            span("好", None),
            span(" ", None),
            span("✦", Some(Color::Cyan)),
        ];
        assert_eq!(
            aligned_patch(&old, &new, 3),
            Some(cell_patch(&old, &new, 3))
        );
        assert_patch_lands(&old, &new);
    }

    #[test]
    fn zero_width_marks_ride_on_the_cell_before_them() {
        let spans = vec![
            span("e\u{301}\u{302}你\u{301}x", None),
            span("\u{301}y", None),
        ];
        let expanded = cells(&spans);
        let shape: Vec<(char, &str)> = expanded.iter().map(|cell| (cell.ch, cell.marks)).collect();
        assert_eq!(
            shape,
            vec![
                ('e', "\u{301}\u{302}"),
                ('你', "\u{301}"),
                ('\0', ""),
                ('x', ""),
                // 段首的零宽记号没有前一格可挂，丢掉。
                ('y', ""),
            ]
        );
    }

    #[test]
    fn links_survive_a_partial_rewrite() {
        let mut linked = span("docs", Some(Color::Blue));
        linked.link = Some("https://example.com".into());
        let old = vec![span("see ", None), linked.clone(), span(" now", None)];
        let mut changed = linked;
        changed.text = "DOCS".into();
        let new = vec![span("see ", None), changed, span(" now", None)];
        let patch = patch_row(&old, &new, 0);
        assert!(
            patch.contains("\x1b]8;;https://example.com\x1b\\"),
            "{patch:?}"
        );
        assert_patch_lands(&old, &new);
    }

    #[test]
    fn trimming_matches_the_trimmed_ansi_string() {
        let spans = vec![
            span("  ", None),
            span("✦", Some(Color::Blue)),
            span("    ", None),
        ];
        let trimmed = trim_end_spans(spans.clone());
        assert_eq!(spans_to_ansi(&trimmed), spans_to_ansi(&spans).trim_end());
        assert_eq!(
            trim_end_spans(vec![span("   ", None)]),
            Vec::<AnsiSpan>::new()
        );
    }
}
