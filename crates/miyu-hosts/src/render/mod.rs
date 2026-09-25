mod agent_events;
pub mod blocks;
mod code;
mod command;
pub(crate) mod diagram_style;
mod link;
mod markdown;
pub(crate) mod math;
pub(crate) mod mermaid;
mod patch;
pub(crate) mod stream;
mod style;
mod table;
mod tool_display;
mod usage;
pub use agent_events::*;
pub use code::*;
pub use command::*;
pub use link::*;
pub use markdown::*;
pub use patch::*;
pub use stream::*;
pub use style::*;
pub use table::*;
pub use tool_display::*;
pub use usage::*;

mod live_area;
pub mod wait_spinner;

/// 正文能用多宽。
///
/// 全屏下正文左右各留了页边距，按**整屏**排出来的表格、补丁、代码块会比可视区宽，
/// 落进缓冲被硬折一次，续行从第 0 列开始——屏幕左边就冒出半截边框。`fallback`
/// 是连终端尺寸都问不到时的兜底（各调用点历来的默认值不同，保持原样）。
/// 某个工具在时间线上的图标。面板里的那条时间线也按它来，两处一个样子。
pub fn tool_glyph_for(name: &str) -> &'static str {
    stream::timeline::tool_glyph(name)
}

pub(crate) fn content_cols(fallback: usize) -> usize {
    // 本线程报过的宽度最优先：面板里渲染一段正文时把面板宽度报进来，代码块、
    // 表格、公式才按面板宽度排，而不是按整屏宽度排完再被折成碎行。
    let forced = cols_override();
    if forced > 0 {
        return usize::from(forced);
    }
    if let Some((cols, _)) = miyu_base::terminal::content_viewport() {
        return usize::from(cols);
    }
    terminal_cols(fallback)
}

thread_local! {
    static COLS_OVERRIDE: std::cell::Cell<u16> = const { std::cell::Cell::new(0) };
}

/// 这条线程上的渲染宽度按这个数算。daemon 往别人的终端回写时，`terminal::size()`
/// 量的是自己的 stdout（根本不是终端），得由调用方把那个 tty 的宽度报进来。
/// 0 = 不覆盖。
pub fn set_cols_override(cols: u16) {
    COLS_OVERRIDE.with(|cell| cell.set(cols));
}

/// 本线程有没有报过宽度——报过就说明这条线程在往某个终端画（daemon 回写）。
pub(crate) fn cols_override_active() -> bool {
    cols_override() > 0
}

/// 本线程报过的宽度（0 = 没报）。
pub(crate) fn cols_override() -> u16 {
    COLS_OVERRIDE.with(|cell| cell.get())
}

/// 终端有多宽：先看本线程的覆盖值，再问 `terminal::size()`，都没有就用 `fallback`。
/// 终端高度（行）。拿不到就用 `fallback`。
pub(crate) fn terminal_rows(fallback: usize) -> usize {
    terminal::size()
        .map(|(_, height)| usize::from(height))
        .unwrap_or(fallback)
}

pub(crate) fn terminal_cols(fallback: usize) -> usize {
    let forced = COLS_OVERRIDE.with(|cell| cell.get());
    if forced > 0 {
        return usize::from(forced);
    }
    terminal::size()
        .map(|(width, _)| usize::from(width))
        .unwrap_or(fallback)
}

use crate::render::wait_spinner::{braille_frame, SpinnerStyle, WaitSpinner, SPINNER_INTERVAL};
use anyhow::Result;
use crossterm::cursor::{Hide, MoveToColumn, MoveUp, Show};
use crossterm::style::{Color, ResetColor, SetForegroundColor};
use crossterm::terminal::{Clear, ClearType};
use crossterm::{execute, terminal};
use miyu_base::i18n::text as t;
use miyu_core::llm::{ChatResult, ChatStreamChunk, ChatStreamKind, GenerationSpeed};
use miyu_engine::tools::CommandOutputStream;
use serde_json::Value;
use std::collections::{BTreeMap, VecDeque};
use std::io::{self, IsTerminal, Write};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReasoningDisplayMode {
    Hidden,
    Summary,
    Full,
}

impl ReasoningDisplayMode {
    /// 配置里现在只有一位布尔：**出来是不是展开的**（用户 09-17 把三值档拆成
    /// 布尔、删掉隐藏档）。`Hidden` 留在枚举里是给 `--plain` 用的——那条路上
    /// 屏幕只该有正文，和用户的显示偏好无关。
    pub fn from_expand(expand: bool) -> Self {
        if expand {
            Self::Full
        } else {
            Self::Summary
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolCallDisplayMode {
    Hidden,
    Summary,
    Full,
}

impl ToolCallDisplayMode {
    /// 见 [`ReasoningDisplayMode::from_expand`]。
    pub fn from_expand(expand: bool) -> Self {
        if expand {
            Self::Full
        } else {
            Self::Summary
        }
    }
}

#[cfg(test)]
mod tests;
