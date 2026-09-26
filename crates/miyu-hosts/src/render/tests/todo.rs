//! 任务清单表在过程时间线里的位置。
//!
//! 单独成文件是因为 `timeline.rs` 已经超了 AGENTS §6.2 的行数上限,门禁不许它
//! 再长。

use super::timeline::{timeline_renderer, with_blocks};
use crate::render::t;

/// 用清单 = 这一段忙完了(WebUI 一直是这个样子,用户 09-16 裁定全屏与 shellhook
/// 都对齐它)。退回这条规则之前,表要等模型开口说正文、整段收完才出得来,而
/// shellhook 那边表就地插进还在继续的线程里,把左边那根竖线劈成两截。
#[test]
fn the_todo_tool_ends_the_timeline_segment() {
    with_blocks(|| {
        let mut renderer = timeline_renderer();
        renderer.use_buffered_output();
        renderer
            .write_tool_call("todowrite", r#"{"todos":[]}"#)
            .unwrap();
        renderer
            .write_tool_result("todowrite", true, "todo list updated")
            .unwrap();
        renderer.finalize_tools_summary().unwrap();
        let frame = String::from_utf8_lossy(&renderer.take_output_frame()).into_owned();
        let text = crate::render::strip_ansi_text(&frame);
        // 收缩行只有计数——认收缩把手与计数即可。
        assert!(
            text.contains('\u{203a}') && text.contains(t("tool", "工具")),
            "清单那一批没有收段: {text:?}"
        );
        assert!(
            renderer.timeline_step_lines().is_empty(),
            "收段之后 live 时间线上不该还留着步骤"
        );
    });
}
