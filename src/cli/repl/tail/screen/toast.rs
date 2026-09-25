//! 通知条：一句话的状态，浮在输入框上方，自己消失。
//!
//! 「已取消」「已停止 1 个后台任务」「会话模型已更新」这类话不是对话内容——
//! 它们说的是**刚才那一下发生了什么**，几秒后就不再有意义。写进正文的代价是
//! 回翻时满屏都是这种碎片，而且它们会把真正的对话推上去。
//!
//! 长的东西照旧进正文（`/help` 那种整页清单浮起来也没法看），判据就是行数。

use super::Screen;
use crossterm::{
    cursor::MoveTo,
    queue,
    style::Print,
    terminal::{Clear, ClearType},
};
use miyu_hosts::render::visible_width;
use std::time::{Duration, Instant};

/// 浮层靠哪边。
#[derive(Clone, Copy, PartialEq)]
enum FloatAlign {
    Left,
    Right,
    /// 从指定列起画(大厅里对齐输入框)。
    Column(u16),
}

/// 停留多久。够读完一句话，又不至于赖在屏幕上。
/// 通知停多久。
///
/// 四秒太长了——「已复制」这种话看一眼就完事，它却一直压在那儿。两秒够看清，
/// 又不至于赖着不走。
const LINGER: Duration = Duration::from_millis(2200);
/// 超过这么多行就不是「一句话」了，老老实实进正文。
pub(in crate::cli) const MAX_TOAST_LINES: usize = 2;

pub(in crate::cli) struct Toast {
    lines: Vec<String>,
    born: Instant,
    /// 浮在哪儿。多数通知和正文无关，去右上角最不挡事；但「要退出请按 Ctrl+D」
    /// 这类**讲输入框的**提示得待在输入框旁边，跑到右上角就对不上号了。
    align: FloatAlign,
}

impl Toast {
    fn expired(&self) -> bool {
        self.born.elapsed() > LINGER
    }
}

impl Screen {
    /// 弹一条通知。返回真表示接下了（调用方就不用再往正文里写）。
    /// 贴着输入框弹一条通知。讲输入框的事才用它。
    pub(in crate::cli) fn toast_near_input(&mut self, text: &str) -> bool {
        self.toast_with(text, FloatAlign::Left)
    }

    pub(in crate::cli) fn toast(&mut self, text: &str) -> bool {
        self.toast_with(text, FloatAlign::Right)
    }

    fn toast_with(&mut self, text: &str, align: FloatAlign) -> bool {
        let lines: Vec<String> = text
            .lines()
            .map(str::trim_end)
            .filter(|line| !line.trim().is_empty())
            .map(str::to_string)
            .collect();
        if lines.is_empty() || lines.len() > MAX_TOAST_LINES {
            return false;
        }
        self.toast = Some(Toast {
            lines,
            born: Instant::now(),
            align,
        });
        self.invalidate();
        true
    }

    /// 到点了就收掉。返回真表示画面变了。
    pub(in crate::cli) fn expire_toast(&mut self) -> bool {
        if self.toast.as_ref().is_some_and(Toast::expired) {
            self.toast = None;
            self.invalidate();
            return true;
        }
        false
    }

    /// 立刻收掉通知。Ctrl+L 是「给我一块干净的屏」，浮层也算脏东西。
    pub(in crate::cli) fn dismiss_toast(&mut self) {
        if self.toast.take().is_some() {
            self.invalidate();
        }
    }

    /// 画通知条。浮在**右上角**。
    ///
    /// 原来贴着输入框上方，正好压住刚写完的那几行正文——而通知说的多半是
    /// 「已复制」「已停止」这类和正文无关的事，没有理由挡着正文。右上角是
    /// 屏幕上最空的一块。
    pub(in crate::cli) fn paint_toast(
        &mut self,
        stdout: &mut impl std::io::Write,
        body: u16,
    ) -> anyhow::Result<()> {
        let Some(toast) = &self.toast else {
            return Ok(());
        };
        let lines = toast.lines.clone();
        let align = toast.align;
        match align {
            FloatAlign::Right => self.paint_float_at(stdout, 0, &lines, align),
            FloatAlign::Left | FloatAlign::Column(_) => {
                // 大厅里输入框不在屏底：「讲输入框的」通知跟着候选面板的锚点走，
                // 待在输入框底下。掉到左下角就和输入框对不上号了（用户实测）。
                if let Some((top, left)) = self.float_anchor {
                    return self.paint_float_at(stdout, top, &lines, FloatAlign::Column(left));
                }
                self.paint_float(stdout, body, &lines)
            }
        }
    }

    /// 斜杠命令候选面板。内容每帧由外面给，`None` = 不显示。
    ///
    /// inline 那边是在 footer 位置塞一行挤在一起的命令名；全屏有地方，就做成
    /// 浮在输入框上方的一小块，每条带上它是干什么的。
    pub(in crate::cli) fn set_command_hint(&mut self, lines: Vec<String>) -> bool {
        if self.command_hint == lines {
            return false;
        }
        self.command_hint = lines;
        self.invalidate();
        true
    }

    /// Esc：先关掉候选面板。关了就返回真。
    pub(in crate::cli) fn dismiss_command_hint(&mut self) -> bool {
        if self.command_hint.is_empty() {
            return false;
        }
        self.command_hint.clear();
        self.hint_dismissed = true;
        self.invalidate();
        true
    }

    /// 输入变了就重新允许弹面板——关掉只针对当时那一串。
    pub(in crate::cli) fn allow_command_hint(&mut self) {
        self.hint_dismissed = false;
    }

    pub(in crate::cli) fn command_hint_dismissed(&self) -> bool {
        self.hint_dismissed
    }

    /// 画候选面板。浮在输入框上方——和通知条（右上角）各占各的地方，不打架。
    pub(in crate::cli) fn paint_command_hint(
        &mut self,
        stdout: &mut impl std::io::Write,
        body: u16,
    ) -> anyhow::Result<()> {
        if self.command_hint.is_empty() {
            return Ok(());
        }
        let lines = self.command_hint.clone();
        if let Some((top, left)) = self.float_anchor {
            return self.paint_float_at(stdout, top, &lines, FloatAlign::Column(left));
        }
        self.paint_float(stdout, body, &lines)
    }

    /// 浮层的通用画法：细线框 + 暗色，贴着正文底部。
    fn paint_float(
        &mut self,
        stdout: &mut impl std::io::Write,
        body: u16,
        lines: &[String],
    ) -> anyhow::Result<()> {
        let height = u16::try_from(lines.len() + 2).unwrap_or(3);
        if body < height {
            return Ok(());
        }
        self.paint_float_at(stdout, body - height, lines, FloatAlign::Left)
    }

    /// 在指定行画一个浮层框，左对齐或右对齐。
    fn paint_float_at(
        &mut self,
        stdout: &mut impl std::io::Write,
        top: u16,
        lines: &[String],
        align: FloatAlign,
    ) -> anyhow::Result<()> {
        let cols = usize::from(self.cols);
        let width = lines
            .iter()
            .map(|line| visible_width(line))
            .max()
            .unwrap_or(0)
            .min(cols.saturating_sub(6).max(8));
        let height = u16::try_from(lines.len() + 2).unwrap_or(3);
        if top.saturating_add(height) > self.rows {
            return Ok(());
        }
        let left = match align {
            FloatAlign::Left => 2,
            FloatAlign::Column(column) => column.min(
                u16::try_from(cols.saturating_sub(width + 4))
                    .unwrap_or(2)
                    .max(2),
            ),
            FloatAlign::Right => u16::try_from(cols.saturating_sub(width + 6))
                .unwrap_or(2)
                .max(2),
        };
        // 通知用主色（和代码块、表头一个色），暗色那套是"附注"的语气——
        // 而通知是要人看见的。候选面板照旧走暗色：它是打字时的陪衬。
        let dim = if matches!(align, FloatAlign::Right) {
            miyu_hosts::render::PRIMARY_STYLE
        } else {
            "\x1b[2m"
        };
        let reset = "\x1b[0m";
        let inner = width + 2;
        queue!(
            stdout,
            MoveTo(left, top),
            Clear(ClearType::UntilNewLine),
            Print(format!("{dim}╭{}╮{reset}", "─".repeat(inner)))
        )?;
        for (offset, line) in lines.iter().enumerate() {
            let row = top + 1 + u16::try_from(offset).unwrap_or(0);
            let pad = width.saturating_sub(visible_width(line));
            queue!(
                stdout,
                MoveTo(left, row),
                Clear(ClearType::UntilNewLine),
                Print(format!(
                    "{dim}│{reset} {line}{}{dim} │{reset}",
                    " ".repeat(pad)
                ))
            )?;
        }
        queue!(
            stdout,
            MoveTo(left, top + height - 1),
            Clear(ClearType::UntilNewLine),
            Print(format!("{dim}╰{}╯{reset}", "─".repeat(inner)))
        )?;
        // 盖住的那几行下一帧要重画——浮层消失之后不能留个洞。这一帧它们也算擦写过
        // （浮层从左列一路擦到行尾）。
        for offset in 0..height {
            self.touch(top + offset);
            let row = usize::from(top + offset);
            if let Some(slot) = self.painted.get_mut(row) {
                slot.clear();
                slot.push('\u{0}');
            }
        }
        Ok(())
    }
}

/// 通知里的文字要去掉自带的样式转义——框里用统一的暗色，混着别处的颜色
/// （比如绿色的「已复制」）会让它看起来像另一个控件。
pub(in crate::cli) fn plain(text: &str) -> String {
    // 按行去样式再拼回去：只取首行的话，多行内容会被压成一行，
    // 然后被当成「一句话」浮起来（`/help` 整页清单就这么消失过）。
    super::ansi::parse_ansi(text)
        .into_iter()
        .map(|spans| spans.into_iter().map(|span| span.text).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}
