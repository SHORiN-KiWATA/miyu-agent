//! 活动区按行记账：上一帧每一行写了什么，这一帧只重写变了的行（09-25）。
//!
//! 活动区（输入框、footer、排队行、任务条）原来每一帧都整块先擦再写：大厅每 40ms
//! 一拍、流式输出每来一块都是如此。支持同步输出的终端看不出来；不支持的，擦了还没写
//! 的那一刻就是一闪。这里把这一帧要写的字节按绝对定位切成一行一行，和上一帧比：
//! 没变、正文层这一帧也没擦写过这一行、屏幕也没整片失效，就不写。
//!
//! 只在全屏下用：那里活动区的每一笔都以 `ESC[行;列H` 起头，行号就是屏幕行。字节里
//! 出现换行、回车（inline 往下挤的写法）就不切，整块照写。

use std::collections::HashMap;

/// 每一行重写之前先复位样式：切开之后行与行之间不再接着上一笔的样式往下写。
const RESET: &[u8] = b"\x1b[0m";

#[derive(Default)]
pub(in crate::cli) struct RowMemo {
    /// 记账时屏幕整片失效的次数（`Screen::repaint_epoch`）。对不上就是屏上的东西
    /// 可能已经没了，这一帧整块重写。
    epoch: Option<u64>,
    /// 上一帧每一行写了哪些字节（同一行的几笔按原来的先后接在一起）。
    rows: HashMap<u16, Vec<u8>>,
}

impl RowMemo {
    /// 全部作废：下一帧活动区整块重写。
    pub(in crate::cli) fn clear(&mut self) {
        self.epoch = None;
        self.rows.clear();
    }

    /// 别处直接往这一行写过（footer 转轮、任务条、输入区反显）：下一帧这一行照写。
    pub(in crate::cli) fn forget_row(&mut self, row: u16) {
        self.rows.remove(&row);
    }

    /// 这一帧活动区要写的字节 `frame` → 真正要写出去的字节。`touched(行)` = 正文层
    /// 这一帧有没有整行擦写过那一行（擦过的话叠在上面的活动区已经没了，得重写）。
    pub(in crate::cli) fn diff(
        &mut self,
        frame: &[u8],
        epoch: u64,
        touched: impl Fn(u16) -> bool,
    ) -> Vec<u8> {
        let Some((preamble, rows)) = split_rows(frame) else {
            self.clear();
            return frame.to_vec();
        };
        let stale = self.epoch != Some(epoch);
        let mut out = preamble;
        let mut next = HashMap::with_capacity(rows.len());
        for (row, bytes) in rows {
            let unchanged = !stale && !touched(row) && self.rows.get(&row) == Some(&bytes);
            if !unchanged {
                out.extend_from_slice(RESET);
                out.extend_from_slice(&bytes);
            }
            next.insert(row, bytes);
        }
        self.rows = next;
        self.epoch = Some(epoch);
        out
    }
}

/// 读一个 `ESC[行;列H`（从 `at` 起），返回（0 基行号, 这个序列的长度）。
fn cursor_move(frame: &[u8], at: usize) -> Option<(u16, usize)> {
    let rest = frame.get(at..)?;
    let body = rest.strip_prefix(b"\x1b[")?;
    let end = body
        .iter()
        .position(|byte| !byte.is_ascii_digit() && *byte != b';')?;
    if body[end] != b'H' {
        return None;
    }
    let params = std::str::from_utf8(&body[..end]).ok()?;
    let (row, _col) = params.split_once(';')?;
    let row: u16 = row.parse().ok()?;
    Some((row.saturating_sub(1), 2 + end + 1))
}

/// 按 `ESC[行;列H` 把字节切成一行一行：（第一次定位之前的字节, [(行, 这一行的几笔)]），
/// 行按第一次出现的先后排。只定位、不写字的那一笔（收尾把光标挪走）不归任何一行。
/// 出现换行或回车就切不开，返回 `None`。
fn split_rows(frame: &[u8]) -> Option<(Vec<u8>, Vec<(u16, Vec<u8>)>)> {
    if frame.iter().any(|byte| matches!(byte, b'\n' | b'\r')) {
        return None;
    }
    let mut preamble = Vec::new();
    let mut order: Vec<u16> = Vec::new();
    let mut rows: HashMap<u16, Vec<u8>> = HashMap::new();
    let mut at = 0;
    let mut current: Option<(u16, usize)> = None;
    let mut flush =
        |current: Option<(u16, usize)>, end: usize, rows: &mut HashMap<u16, Vec<u8>>| {
            let Some((row, start)) = current else {
                return;
            };
            let segment = &frame[start..end];
            let move_len = cursor_move(frame, start).map_or(0, |(_, len)| len);
            if segment.len() <= move_len {
                return;
            }
            if !rows.contains_key(&row) {
                order.push(row);
            }
            rows.entry(row).or_default().extend_from_slice(segment);
        };
    while at < frame.len() {
        if let Some((row, len)) = cursor_move(frame, at) {
            flush(current, at, &mut rows);
            current = Some((row, at));
            at += len;
            continue;
        }
        if current.is_none() {
            preamble.push(frame[at]);
        }
        at += 1;
    }
    flush(current, frame.len(), &mut rows);
    let rows = order
        .into_iter()
        .map(|row| {
            let bytes = rows.remove(&row).unwrap_or_default();
            (row, bytes)
        })
        .collect();
    Some((preamble, rows))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(parts: &[(u16, u16, &str)]) -> Vec<u8> {
        parts
            .iter()
            .flat_map(|(row, col, text)| {
                format!("\x1b[{};{}H{text}", row + 1, col + 1).into_bytes()
            })
            .collect()
    }

    #[test]
    fn rows_group_every_stroke_in_order_and_bare_moves_belong_to_none() {
        let bytes = frame(&[
            (5, 2, "    "),
            (6, 2, "    "),
            (5, 2, "┃ hi"),
            (6, 2, "┃"),
            (5, 7, ""),
        ]);
        let (preamble, rows) = split_rows(&bytes).unwrap();
        assert!(preamble.is_empty());
        let rows: Vec<(u16, String)> = rows
            .into_iter()
            .map(|(row, bytes)| (row, String::from_utf8(bytes).unwrap()))
            .collect();
        assert_eq!(
            rows,
            vec![
                (5, "\x1b[6;3H    \x1b[6;3H┃ hi".to_string()),
                (6, "\x1b[7;3H    \x1b[7;3H┃".to_string()),
            ]
        );
    }

    #[test]
    fn newlines_are_not_split() {
        assert!(split_rows(b"\x1b[3;1Habc\r\ndef").is_none());
        let mut memo = RowMemo::default();
        assert_eq!(
            memo.diff(b"\x1b[3;1Habc\r\n", 0, |_| false),
            b"\x1b[3;1Habc\r\n"
        );
    }

    #[test]
    fn unchanged_rows_are_not_written_again() {
        let mut memo = RowMemo::default();
        let first = frame(&[(10, 0, "input"), (11, 0, "footer 1")]);
        // 第一帧整块写。
        assert!(memo.diff(&first, 7, |_| false).ends_with(b"footer 1"));
        // 只有 footer 变了：输入框那一行不写。
        let second = frame(&[(10, 0, "input"), (11, 0, "footer 2")]);
        let out = String::from_utf8(memo.diff(&second, 7, |_| false)).unwrap();
        assert!(!out.contains("input"), "{out:?}");
        assert!(out.contains("footer 2"), "{out:?}");
        // 一模一样：一个字节都不写。
        assert!(memo.diff(&second, 7, |_| false).is_empty());
    }

    #[test]
    fn touched_rows_forgotten_rows_and_a_new_epoch_are_written_again() {
        let mut memo = RowMemo::default();
        let bytes = frame(&[(10, 0, "input"), (11, 0, "footer")]);
        memo.diff(&bytes, 1, |_| false);
        // 正文层这一帧擦写过第 10 行。
        let out = String::from_utf8(memo.diff(&bytes, 1, |row| row == 10)).unwrap();
        assert!(out.contains("input") && !out.contains("footer"), "{out:?}");
        // 别处直接写过第 11 行。
        memo.forget_row(11);
        let out = String::from_utf8(memo.diff(&bytes, 1, |_| false)).unwrap();
        assert!(!out.contains("input") && out.contains("footer"), "{out:?}");
        // 屏幕整片失效过：整块重写。
        let out = String::from_utf8(memo.diff(&bytes, 2, |_| false)).unwrap();
        assert!(out.contains("input") && out.contains("footer"), "{out:?}");
    }
}
