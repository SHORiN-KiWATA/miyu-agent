//! 跨会话消息（09-23）在终端里的样子。
//!
//! 收到的那条（后台唤醒起的一轮、插进正在跑的那一轮、回放）：
//!
//! ```text
//!   <铃铛> 从 写代码（4b467f20）收到消息
//!   │ 构建好了，你那边可以跑了
//!   │ ⋮ 已省略
//! ```
//!
//! 发出去的那次是时间线上的一步（`send_to_other_running_session`）：抬头是
//! 「给其他会话发送消息 · <短 id> <会话名>」，底下露正文的头几行。会话 id 只写
//! 末尾那段随机串（用户 09-24：完整 id 太长）。
//!
//! 两边都先露 `display.cross_session_preview_lines` 行（用户 09-23 定默认 10），
//! 能点开的面（全屏）点抬头看全文，点不开的面（inline、shellhook）就停在预览。
//! 露着的那几行是附注，跟着抬头一起暗，点开的全文才是正常色——和命令预览同一个
//! 规矩（用户 09-24：「消息预览的颜色不对」）。

use super::*;

/// 发出去那一步的工具名（与引擎层的 `cross_session::TOOL_NAME` 同一个字）。
pub(crate) const SEND_TOOL: &str = "send_to_other_running_session";

/// 正文折好行、过一遍 markdown，每行不带前缀。
fn body_rows(body: &str) -> Vec<String> {
    render_speech_lines(body.trim_end(), detail_width())
}

/// 露在抬头底下的那几行：前 `preview` 行，装不下时最后一行换成 `⋮ 已省略`
/// （和命令那一步同一个规矩：留头不留尾）。整段暗色：markdown 的样式剥掉，行里的
/// 复位符会把暗色半路掐断。
fn preview_rows(rows: &[String], preview: usize) -> Vec<String> {
    if preview == 0 {
        return Vec::new();
    }
    let dim = |row: &String| format!("\x1b[2m{}\x1b[0m", crate::render::strip_ansi_text(row));
    if rows.len() <= preview {
        return rows.iter().map(dim).collect();
    }
    let mut kept: Vec<String> = rows[..preview.saturating_sub(1)].iter().map(dim).collect();
    kept.push(format!("\x1b[2m⋮ {}\x1b[0m", t("omitted", "已省略")));
    kept
}

/// 收到的那条。能点开的面：抬头 + 预览，点开是抬头 + 全文；点不开的面只写预览。
pub fn write_cross_session_message<W: std::io::Write>(
    writer: &mut W,
    headline: &str,
    body: &str,
    preview: usize,
) -> std::io::Result<()> {
    let head = format!("\x1b[2m{}{} {headline}\x1b[0m", indent(), glyph_notice());
    let rows = body_rows(body);
    let shown = preview_rows(&rows, preview);
    let rail = rail_prefix();
    // 预览没露全才值得点开（露全了的只是颜色不同，点开也还是这几行）。
    let expanded =
        if blocks::enabled() && (preview == 0 || rows.len() > preview) && !rows.is_empty() {
            let mut lines = vec![head.clone()];
            lines.extend(rows.iter().map(|row| format!("{rail}{row}")));
            lines.push(String::new());
            lines
        } else {
            Vec::new()
        };
    let collapsed = |writer: &mut W| -> std::io::Result<()> {
        writeln!(writer, "{head}")?;
        for row in &shown {
            writeln!(writer, "{rail}{row}")?;
        }
        writeln!(writer)
    };
    if expanded.is_empty() {
        return collapsed(writer);
    }
    blocks::write_expandable(writer, expanded, collapsed)
}

impl StreamRenderer {
    /// 发出去那一步开始：窥视填短 id（名字等结果回来再补），抬头底下挂正文
    /// 预览，点开是正文全文。点不开的面没有全文可看，预览就是它能给的全部。
    pub(crate) fn note_cross_session_send(&mut self, name: &str, arguments: &str) {
        let args: serde_json::Value = serde_json::from_str(arguments).unwrap_or_default();
        let action = args.get("action").and_then(serde_json::Value::as_str);
        let peek = match action {
            Some("list") => Some(t("list open sessions", "列出开着的会话").to_string()),
            _ => args
                .get("session_id")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|session| !session.is_empty())
                .map(|session| miyu_core::state::short_session_id(session).to_string()),
        };
        // 参数不是 JSON（回放时被截断过）就拿不到正文，那一步只剩抬头。
        let message = args
            .get("message")
            .and_then(serde_json::Value::as_str)
            .filter(|message| action == Some("send") && !message.trim().is_empty());
        let preview = self.cross_session_preview_lines;
        let detail_inline = self.caps().detail_inline();
        let stats = self.tool_stats_entry(name);
        stats.peek = peek;
        if let Some(message) = message {
            let rows = body_rows(message);
            stats.tail = preview_rows(&rows, preview);
            if !detail_inline {
                stats.detail = rows;
            }
        }
    }

    /// 结果回来：窥视补上对方的会话名；没发成的话把原因接在全文后面。
    pub(crate) fn note_cross_session_result(&mut self, name: &str, ok: bool, output: &str) {
        let target = serde_json::from_str::<serde_json::Value>(output)
            .ok()
            .and_then(|result| result.get("name")?.as_str().map(str::to_string))
            .filter(|target| !target.trim().is_empty());
        let detail_inline = self.caps().detail_inline();
        let stats = self.tool_stats_entry(name);
        if let (Some(target), Some(peek)) = (target, stats.peek.as_mut()) {
            peek.push(' ');
            peek.push_str(&target);
        }
        if !ok && !detail_inline && !stats.detail.is_empty() {
            stats.detail.push(String::new());
            stats.detail.extend(tool_output_lines(output));
        }
    }
}
