//! 活动区的画板：画上去、下一帧按行比对着改、收的时候擦掉的那几行。
//!
//! 转轮（`wait_spinner`）和正文的活尾巴（`stream::reply_tail`）共用这一块——09-25 之前它长在
//! 转轮里。点不开的面（inline、shellhook、单次、回写）直接对终端上移、清行、重写；全屏发锚点
//! 标记，由缓冲回到锚点重写（`blocks::LIVE_REWIND_MARKER`）。画面上同一时刻只能有一块：
//! 全屏缓冲只有一个锚，点不开的面上两块会叠在同一处。

use anyhow::Result;
use crossterm::cursor::{MoveDown, MoveToColumn, MoveUp};
use crossterm::terminal::{BeginSynchronizedUpdate, Clear, ClearType, EndSynchronizedUpdate};
use crossterm::{execute, queue};
use std::io::Write;

/// 全屏下一帧从哪一行起重写。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Rewrite {
    /// 从第一处变化起。行和缓冲里的逻辑行得一一对应（转轮的行裁过宽、不折）。
    FromFirstChange,
    /// 整段重写。行里有折出来的续行时用：缓冲按折行标志数逻辑行，行号对不上。
    /// 没变的行缓冲会把旧版本号还回去，画面层照样跳过它们（`Term::settle_live_rows`）。
    Whole,
}

/// 活动区上一帧画了什么。
#[derive(Default)]
pub(crate) struct LiveArea {
    /// 上一帧的每一行（含转义）。
    pub(super) lines: Vec<String>,
    /// 各行的显示宽度。
    pub(super) widths: Vec<usize>,
    /// 上一帧是按多宽的终端出的。全屏下宽度一变就整段重写，不按行比对。
    width: usize,
}

impl LiveArea {
    pub(crate) fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// 画一帧。`widths` 是各行的显示宽度；`synchronized` = 自己裹一对同步输出标记，
    /// 调用方已经在块里就传假（2026 是布尔不是栈，里层的结束会把外层提前结掉）。
    pub(crate) fn paint(
        &mut self,
        writer: &mut impl Write,
        lines: Vec<String>,
        widths: Vec<usize>,
        terminal_width: usize,
        synchronized: bool,
        rewrite: Rewrite,
    ) -> Result<()> {
        // 活动区里挂着一段正在长的字时，每帧整片擦了重画既费字节又闪（用户实测「流式
        // 输出的时候一闪一闪的」）：一帧裹进同步输出块，终端一次成帧；行数没变或只在
        // 末尾长了、又没有软折行，就只重写变了的行、追加新行。
        let diffable = lines.len() >= self.lines.len()
            && self
                .widths
                .iter()
                .chain(widths.iter())
                .all(|width| *width <= terminal_width);
        if crate::render::blocks::enabled() {
            self.write_anchored(writer, &lines, terminal_width, rewrite)?;
        } else {
            if synchronized {
                queue!(writer, BeginSynchronizedUpdate)?;
            }
            if diffable {
                rewrite_changed_rows(writer, &self.lines, &lines)?;
            } else {
                write_rows(writer, &lines, &self.widths, terminal_width)?;
            }
            if synchronized {
                queue!(writer, EndSynchronizedUpdate)?;
            }
        }
        writer.flush()?;
        self.lines = lines;
        self.widths = widths;
        self.width = terminal_width;
        Ok(())
    }

    /// 全屏：回到锚点重写，不数行（见 `blocks::LIVE_REWIND_MARKER`）。
    ///
    /// 宽度变了就整段重写——缓冲会按新宽度重排，行和行对不上了。一模一样的一帧
    /// 一个字节都不发：缓冲不用动，画面也不用重画。不裹同步标记：这些字节进的是
    /// 全屏的缓冲，不是终端（成帧由画面那一层管）。
    fn write_anchored(
        &self,
        writer: &mut impl Write,
        lines: &[String],
        terminal_width: usize,
        rewrite: Rewrite,
    ) -> Result<()> {
        let same_width = self.width == terminal_width;
        if same_width && lines == self.lines {
            return Ok(());
        }
        let from = match rewrite {
            Rewrite::FromFirstChange if same_width => live_rewrite_from(&self.lines, lines),
            _ => 0,
        };
        write!(
            writer,
            "{}{}",
            crate::render::blocks::live_rewind_marker_at(from),
            lines[from..].join("\n")
        )?;
        Ok(())
    }

    /// 擦掉。点不开的面上光标停在原来第一行的行首，接下来的输出从那儿写起；全屏回到锚、
    /// 截掉锚以下、拔锚。
    pub(crate) fn clear(&mut self, writer: &mut impl Write, synchronized: bool) -> Result<()> {
        if crate::render::blocks::enabled() {
            // 一帧都没画过就没有锚可拔。
            if !self.lines.is_empty() {
                write!(writer, "{}", crate::render::blocks::LIVE_END_MARKER)?;
            }
        } else {
            if synchronized {
                queue!(writer, BeginSynchronizedUpdate)?;
            }
            clear_rows(writer, &self.widths)?;
            if synchronized {
                queue!(writer, EndSynchronizedUpdate)?;
            }
        }
        writer.flush()?;
        self.lines.clear();
        self.widths.clear();
        Ok(())
    }

    /// 把最上面的几行「就地」交给 scrollback：内容不动（哪一行和已画的不一样才重写那
    /// 一行），只是从此不再归活动区管，下一帧从它们底下起手。有软折行、行数对不上、
    /// 或者在全屏（按锚点整段重写，没有就地交接这回事）时办不到，返回 false，调用方
    /// 走擦了重画。
    pub(crate) fn commit_leading_rows(
        &mut self,
        writer: &mut impl Write,
        rows: &[String],
        terminal_width: usize,
    ) -> Result<bool> {
        let total = self.lines.len();
        if rows.is_empty() || rows.len() >= total || crate::render::blocks::enabled() {
            return Ok(false);
        }
        let fits = |width: &usize| *width <= terminal_width;
        if !self.widths.iter().all(fits) {
            return Ok(false);
        }
        let widths = rows
            .iter()
            .map(|row| crate::render::command_ansi_width(row))
            .collect::<Vec<_>>();
        if !widths.iter().all(fits) {
            return Ok(false);
        }
        queue!(writer, BeginSynchronizedUpdate, MoveUp((total - 1) as u16))?;
        for (index, row) in rows.iter().enumerate() {
            if self.lines[index] != *row {
                queue!(writer, MoveToColumn(0), Clear(ClearType::CurrentLine))?;
                write!(writer, "{row}")?;
            }
            queue!(writer, MoveDown(1))?;
        }
        let back = total - 1 - rows.len();
        if back > 0 {
            queue!(writer, MoveDown(back as u16))?;
        }
        queue!(writer, EndSynchronizedUpdate)?;
        writer.flush()?;
        self.lines.drain(..rows.len());
        self.widths.drain(..rows.len());
        Ok(true)
    }
}

/// 全屏重写从第几行起：和上一帧第一处不一样的那一行，往前退到它所在块的开头——
/// 块的起止标记得成对重写，从块中间截的话缓冲里那一块就只剩半截。至少重写上一帧
/// 的最后一行：缓冲里它后面没有行可以「回到」。
fn live_rewrite_from(previous: &[String], lines: &[String]) -> usize {
    if previous.is_empty() {
        return 0;
    }
    let changed = previous
        .iter()
        .zip(lines)
        .position(|(old, new)| old != new)
        .unwrap_or_else(|| previous.len().min(lines.len()))
        .min(previous.len() - 1);
    let mut open = None;
    for (index, line) in lines[..changed].iter().enumerate() {
        for (at, _) in line.match_indices(BLOCK_OSC) {
            if line[at + BLOCK_OSC.len()..].starts_with("-end") {
                open = None;
            } else {
                open = Some(index);
            }
        }
    }
    open.unwrap_or(changed)
}

/// 块标记（起始 `miyu-block=` / `miyu-block-open=`，结束 `miyu-block-end`）的共同前缀。
const BLOCK_OSC: &str = "\x1b]1337;miyu-block";

/// 整片重写：先擦掉上一帧，再逐行写。
fn write_rows(
    writer: &mut impl Write,
    lines: &[String],
    previous_widths: &[usize],
    terminal_width: usize,
) -> Result<()> {
    if !previous_widths.is_empty() {
        clear_rows_with_writer(writer, previous_widths, terminal_width)?;
    }
    let output = lines.join("\n");
    let output_lines = output.lines().collect::<Vec<_>>();
    for (index, line) in output_lines.iter().enumerate() {
        execute!(writer, MoveToColumn(0), Clear(ClearType::CurrentLine))?;
        write!(writer, "{line}")?;
        if index + 1 < output_lines.len() {
            writeln!(writer)?;
        }
    }
    writer.flush()?;
    Ok(())
}

/// 只重写变了的行：上移到上一帧的第一行，逐行比对，没变的只是路过；比上一帧多
/// 出来的行用换行往下追加（到了屏底就让终端自己滚）。上一帧是空的时候就是整片
/// 写一遍。落笔停在最后一行上就行，列不管：活动区挂着时光标是藏着的，下一帧和
/// 收尾都从「上移 + 回到第 0 列」起手——写死一个列号反而把行宽（里面有会变的
/// 秒数）烤进字节，golden 就抖。
fn rewrite_changed_rows(
    writer: &mut impl Write,
    previous: &[String],
    lines: &[String],
) -> Result<()> {
    let old_rows = previous.len();
    let rows = lines.len();
    if old_rows > 1 {
        queue!(writer, MoveUp((old_rows - 1) as u16))?;
    }
    for (index, line) in lines.iter().enumerate() {
        let known = index < old_rows;
        if !known || previous[index] != *line {
            queue!(writer, MoveToColumn(0), Clear(ClearType::CurrentLine))?;
            write!(writer, "{line}")?;
        }
        if index + 1 < rows {
            if index + 1 < old_rows {
                queue!(writer, MoveDown(1))?;
            } else {
                writeln!(writer)?;
            }
        }
    }
    Ok(())
}

fn clear_rows(writer: &mut impl Write, widths: &[usize]) -> Result<()> {
    if widths.is_empty() {
        return Ok(());
    }
    let terminal_width = crate::render::terminal_cols(120);
    clear_rows_with_writer(writer, widths, terminal_width)?;
    writer.flush()?;
    Ok(())
}

fn clear_rows_with_writer(
    stdout: &mut impl Write,
    widths: &[usize],
    terminal_width: usize,
) -> Result<()> {
    if widths.is_empty() {
        return Ok(());
    }
    let rows = crate::render::rendered_physical_rows(widths, terminal_width);
    if rows > 1 {
        execute!(stdout, MoveUp(rows - 1))?;
    }
    for index in 0..rows {
        execute!(stdout, MoveToColumn(0), Clear(ClearType::CurrentLine))?;
        if index + 1 < rows {
            execute!(stdout, MoveDown(1))?;
        }
    }
    if rows > 1 {
        execute!(stdout, MoveUp(rows - 1))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 全屏重写起点：第一处变化；落在一个块中间就退回块头（起止标记得成对重写）；
    /// 至少重写上一帧的最后一行。
    #[test]
    fn a_partial_rewrite_starts_at_the_first_change_but_never_inside_a_block() {
        let lines = |tail: &str| -> Vec<String> {
            vec![
                "  step 0".into(),
                format!("\x1b]1337;miyu-block=7\x07  step 1"),
                "  │ out a".into(),
                format!("  │ {tail}\x1b]1337;miyu-block-end\x07"),
                "  head".into(),
            ]
        };
        let before = lines("out b");
        assert_eq!(live_rewrite_from(&before, &lines("out B")), 1, "退到块头");
        let mut changed_head = before.clone();
        changed_head[4] = "  head 2".into();
        assert_eq!(live_rewrite_from(&before, &changed_head), 4);
        assert_eq!(
            live_rewrite_from(&before, &before),
            4,
            "一样也至少重写最后一行"
        );
        let mut longer = before.clone();
        longer.push("  new".into());
        assert_eq!(live_rewrite_from(&before, &longer), 4);
        assert_eq!(
            live_rewrite_from(&before, &before[..2]),
            1,
            "变短：从截断处起"
        );
        assert_eq!(live_rewrite_from(&[], &before), 0);
    }

    #[test]
    fn clearing_multiline_rows_returns_cursor_to_the_top() {
        let mut output = Vec::new();
        clear_rows_with_writer(&mut output, &[20, 20], 80).unwrap();
        let output = String::from_utf8(output).unwrap();

        assert!(output.starts_with("\x1b[1A"));
        assert!(output.contains("\x1b[1B"));
        assert!(output.ends_with("\x1b[1A"));
    }

    #[test]
    fn clearing_counts_soft_wrapped_physical_rows() {
        let mut output = Vec::new();
        clear_rows_with_writer(&mut output, &[100, 20], 40).unwrap();
        let output = String::from_utf8(output).unwrap();

        assert!(output.starts_with("\x1b[3A"));
        assert_eq!(output.matches("\x1b[1B").count(), 3);
        assert!(output.ends_with("\x1b[3A"));
    }
}
