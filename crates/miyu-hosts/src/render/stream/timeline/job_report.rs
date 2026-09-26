//! 后台任务完成那一行（09-26 起点得开）。
//!
//! ```text
//!   <铃铛> 子代理完成 79ea0b · 查资料
//! ```
//!
//! 点开是唤醒里附的结果段：子代理交上来的结论（照正文排版）、失败原因，或者命令输出的最后
//! 几行（和命令那一步的输出一个样子）。用户 09-26：「这个内容是可以点击展开看到返回内容的」。
//! 点不开的面（inline、shellhook）还是原来那一行。

use super::*;
use miyu_core::state::{JobReportKind, JobReportResult};

/// 铃铛那一行，有结果段就能点开。
pub fn write_job_report_notice<W: std::io::Write>(
    writer: &mut W,
    headline: &str,
    report: Option<&JobReportResult>,
) -> std::io::Result<()> {
    if !blocks::enabled() {
        writeln!(writer, "\x1b[2m⚙ {headline}\x1b[0m")?;
        return writeln!(writer);
    }
    let head = indent_body(&format!("\x1b[2m{} {headline}\x1b[0m", glyph_notice()));
    let collapsed = |writer: &mut W| -> std::io::Result<()> {
        writeln!(writer, "{head}")?;
        writeln!(writer)
    };
    let rows = report.map(report_rows).unwrap_or_default();
    if rows.is_empty() {
        return collapsed(writer);
    }
    let rail = rail_prefix();
    let mut expanded: Vec<String> = head.lines().map(str::to_string).collect();
    expanded.extend(rows.iter().map(|row| format!("{rail}{row}")));
    expanded.push(String::new());
    blocks::write_expandable(writer, expanded, collapsed)
}

/// 点开之后挂在铃铛底下的那几行（不带前缀）。
fn report_rows(report: &JobReportResult) -> Vec<String> {
    let label = |text: &str| format!("\x1b[2m{text}\x1b[0m");
    match report.kind {
        JobReportKind::Conclusion => render_speech_lines(report.body.trim_end(), detail_width()),
        JobReportKind::Failure => std::iter::once(label(t("Subagent failed:", "子代理失败：")))
            .chain(wrap_detail(report.body.trim_end()))
            .collect(),
        JobReportKind::OutputTail => {
            std::iter::once(label(t("Last lines of output:", "输出结尾：")))
                .chain(tool_output_lines(&report.body))
                .collect()
        }
    }
}
