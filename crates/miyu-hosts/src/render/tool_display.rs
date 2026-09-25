//! 工具调用在终端里的呈现。
//!
//! 一行摘要要说清「在对什么做什么」，所以 `tool_subject` 按工具类型挑出最有信
//! 息量的那个参数（读文件挑路径、搜索挑关键词）。
//!
//! `redact_sensitive_inline` / `redact_bearer_token` 是必需的：工具参数里可能带
//! token 或密钥，而终端内容会被截图、会进日志。

use crate::render::*;

#[derive(Default)]
pub(crate) struct ToolStats {
    pub(crate) calls: usize,
    pub(crate) ok: usize,
    pub(crate) error: usize,
    pub(crate) subject: Option<String>,
    pub(crate) progress: Option<String>,
    pub(crate) final_progress: Option<String>,
    pub(crate) started_at: Option<std::time::Instant>,
    pub(crate) elapsed: Option<std::time::Duration>,
    /// The subagent handed itself off to the background. Its call returned at
    /// once, so the elapsed timer would only ever read `0s` — and worse, imply
    /// the work finished instantly. The job strip tracks it from here on.
    pub(crate) detached: bool,
    pub(crate) seq: usize,
    /// 时间线那一行右边的**单行窥视**：命令文本、检索词之类。
    /// 只在全屏下填。
    pub(crate) peek: Option<String>,
    /// 点开这一步看到的完整内容（命令块全文、工具输出）。只在全屏下填——
    /// inline 不需要，攥着它只是白占内存。
    pub(crate) detail: Vec<String>,
    /// 全屏时间线里这一步跑完之后抬头底下留着的那几行（命令输出的尾巴）。
    /// 点开看到的是 `detail`（全部），不点开也有这几行——和跑着的时候一个量。
    pub(crate) tail: Vec<String>,
}

impl ToolStats {
    pub(crate) fn elapsed(&self) -> Option<std::time::Duration> {
        self.elapsed
            .or_else(|| self.started_at.map(|started| started.elapsed()))
    }

    /// Every issued call has completed (ok or err) — nothing running.
    pub(crate) fn settled(&self) -> bool {
        self.calls > 0 && self.ok + self.error >= self.calls
    }
}

#[derive(Clone, Copy)]
pub enum SummaryStyle {
    Reasoning,
    Tool,
}

/// The still-line equivalent of a spinner style, for terminals that cannot
/// animate — so a phase keeps its identity (thinking vs tool) either way.
pub(crate) fn summary_style_for(style: SpinnerStyle) -> SummaryStyle {
    match style {
        SpinnerStyle::Scanner => SummaryStyle::Reasoning,
        SpinnerStyle::Braille => SummaryStyle::Tool,
    }
}

pub(crate) fn style_summary_text(text: &str, style: SummaryStyle) -> String {
    match style {
        SummaryStyle::Reasoning => format!("\x1b[38;5;10m{text}\x1b[0m"),
        SummaryStyle::Tool => format!("\x1b[2m{text}\x1b[0m"),
    }
}

pub(crate) fn write_activity_summary(
    writer: &mut impl Write,
    text: &str,
    style: SummaryStyle,
) -> Result<()> {
    writeln!(writer, "{}", style_summary_text(text, style))?;
    writeln!(writer)?;
    Ok(())
}

pub(crate) fn tool_status_text(name: &str, stats: &ToolStats, subagent: bool) -> String {
    let calls = stats.calls.max(stats.ok + stats.error).max(1);
    let running = stats.calls.saturating_sub(stats.ok + stats.error);
    let text = if calls == 1 && running > 0 {
        format!("{name}×1 {}", t("running", "运行中"))
    } else if calls == 1 && stats.error > 0 {
        format!("{name}×1 err")
    } else if calls == 1 && stats.ok > 0 {
        format!("{name}×1 ok")
    } else if running > 0 {
        let mut text = format!(
            "{name}×{calls} {}:{} ok:{}",
            t("running", "运行中"),
            running,
            stats.ok,
        );
        if stats.error > 0 {
            text.push_str(&format!(" err:{}", stats.error));
        }
        text
    } else if stats.error > 0 {
        format!("{name}×{calls} ok:{} err:{}", stats.ok, stats.error)
    } else {
        format!("{name}×{calls} ok:{}", stats.ok)
    };
    if subagent && !stats.detached {
        if let Some(elapsed) = stats.elapsed() {
            return format!("{text} · {}", format_elapsed(elapsed));
        }
    }
    text
}

pub(crate) fn tool_result_status(status: &str, elapsed: Option<std::time::Duration>) -> String {
    elapsed.map_or_else(
        || status.to_string(),
        |elapsed| format!("{status} · {}", format_elapsed(elapsed)),
    )
}

/// 工具与子代理用时:整秒的时分秒(过了一小时也带秒,09-23 统一)。
pub(crate) fn format_elapsed(elapsed: std::time::Duration) -> String {
    miyu_base::durations::format_hms(elapsed)
}

pub(crate) fn format_reasoning_elapsed(elapsed: std::time::Duration) -> String {
    if elapsed < std::time::Duration::from_millis(1) {
        "<1ms".to_string()
    } else if elapsed < std::time::Duration::from_secs(1) {
        format!("{}ms", elapsed.as_millis())
    } else if elapsed < std::time::Duration::from_secs(60) {
        format!("{:.1}s", elapsed.as_secs_f64())
    } else {
        miyu_base::durations::format_hms(elapsed)
    }
}

/// 输出不该打印的工具:它们自己已经把内容送到终端上了。
///
/// `use_meme:show` 带 action 后缀(见 `agent::reports::tool_event_name`)——
/// `use_meme` 里只有 show 是静默的,search 要照常显示摘要。
pub(crate) fn is_silent_tool(name: &str) -> bool {
    matches!(name, "use_meme:show" | "ask_question")
}

pub(crate) fn is_subagent_tool(name: &str) -> bool {
    let name = tool_event_base_name(name);
    matches!(name, "subagent" | "task")
}

pub(crate) use miyu_engine::tools::tool_event_base_name;

pub(crate) fn inline_tool_subject(name: &str) -> bool {
    // 回收站的 subject 是条数,贴在标题上比单占一行更紧凑,
    // 而成功时整个块本来就只有这一行。
    matches!(tool_event_base_name(name), "load_tools" | "trash_path")
}

// 工具调用的可读摘要已归位到 `miyu_engine::tools::tool_display`(它是工具层的事实,
// 渲染层只是消费者)。这条再导出保持 `crate::render::{tool_subject, tool_peek, …}`
// 老路径不变(09-16)。
pub(crate) use miyu_engine::tools::{redact_sensitive_inline, tool_subject};
// 这几件只有渲染层的测试还在按老路径调,生产构建里没有调用方(`tool_peek` 原来喂
// 子代理浮层的参数窥视,09-25 浮层退役)。
#[cfg(test)]
pub(crate) use miyu_engine::tools::{args_peek, safe_inline_subject, tool_peek};

pub(crate) fn readable_tool_name(name: &str) -> String {
    miyu_engine::tools::readable_tool_name(name)
}
