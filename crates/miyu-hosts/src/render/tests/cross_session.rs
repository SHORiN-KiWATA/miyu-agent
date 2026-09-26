//! 跨会话消息（09-23）在终端里的两种样子：收到的那一块、发出去的那一步。

use super::timeline::{block_id_in, timeline_renderer, with_blocks};

fn twelve_lines() -> String {
    (1..=12)
        .map(|index| format!("第 {index} 行"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 收到的那一块：抬头 + 竖线串着的前几行，露不全的底部换成省略标记；点开是全文。
#[test]
fn a_received_message_previews_its_first_lines_and_expands_to_all() {
    with_blocks(|| {
        let mut frame = Vec::new();
        crate::render::timeline::write_cross_session_message(
            &mut frame,
            "从 写代码 收到消息",
            &twelve_lines(),
            4,
        )
        .unwrap();
        let text = String::from_utf8_lossy(&frame).into_owned();
        let plain = crate::render::strip_ansi_text(&text);
        assert!(plain.contains("从 写代码 收到消息"), "{plain:?}");
        assert!(
            plain.contains("│ 第 1 行") && plain.contains("│ 第 3 行"),
            "{plain:?}"
        );
        assert!(
            plain.contains("⋮") && !plain.contains("第 4 行"),
            "只露三行加省略: {plain:?}"
        );
        // 露着的几行是附注,跟抬头一起暗(用户 09-24「消息预览的颜色不对」)。
        assert!(text.contains("\x1b[2m第 1 行"), "预览行该是暗色: {text:?}");
        let id = block_id_in(&text).expect("露不全就该能点开");
        let raw_expanded = crate::render::blocks::get(id)
            .unwrap_or_default()
            .join("\n");
        let expanded = crate::render::strip_ansi_text(&raw_expanded);
        assert!(
            expanded.contains("第 12 行") && !expanded.contains("⋮"),
            "{expanded:?}"
        );
        // 点开的全文才是正常色。
        assert!(
            !raw_expanded.contains("\x1b[2m第 12 行"),
            "{raw_expanded:?}"
        );
    });
}

/// 露得下就不挂块（没什么可点开的）；点不开的面只写预览。
#[test]
fn a_short_message_or_a_static_surface_writes_no_block() {
    with_blocks(|| {
        let mut frame = Vec::new();
        crate::render::timeline::write_cross_session_message(&mut frame, "抬头", "一句话", 10)
            .unwrap();
        let text = String::from_utf8_lossy(&frame).into_owned();
        assert!(block_id_in(&text).is_none(), "{text:?}");
        assert!(crate::render::strip_ansi_text(&text).contains("│ 一句话"));
    });
    let mut frame = Vec::new();
    crate::render::timeline::write_cross_session_message(&mut frame, "抬头", &twelve_lines(), 4)
        .unwrap();
    let text = crate::render::strip_ansi_text(&String::from_utf8_lossy(&frame));
    assert!(text.contains("⋮") && !text.contains("第 12 行"), "{text:?}");
}

/// 发出去那一步：抬头右边是 `<会话 id> <会话名>`（名字等结果回来补上），底下露正文
/// 前几行；点开那一步看到的是正文全文。
#[test]
fn the_send_step_shows_the_target_and_a_preview_of_the_message() {
    with_blocks(|| {
        let mut renderer = timeline_renderer();
        renderer.cross_session_preview_lines = 4;
        renderer.use_buffered_output();
        let arguments = serde_json::json!({
            "action": "send",
            "session_id": "sess_1790180210926_42",
            "message": twelve_lines(),
        });
        renderer
            .write_tool_call("send_to_other_running_session", &arguments.to_string())
            .unwrap();
        renderer
            .write_tool_result(
                "send_to_other_running_session",
                true,
                r#"{"ok":true,"session_id":"sess_1790180210926_42","name":"跑测试","delivered":"started a new turn there"}"#,
            )
            .unwrap();
        renderer.finalize_tools_summary().unwrap();
        let (_, live) = renderer.timeline_live(Vec::new());
        let raw = live.unwrap_or_default();
        assert!(raw.contains("\x1b[2m第 1 行"), "预览行该是暗色: {raw:?}");
        let live = crate::render::strip_ansi_text(&raw);
        assert!(
            live.contains("给其他会话发送消息") || live.contains("Message another session"),
            "{live:?}"
        );
        assert!(
            live.contains("42 跑测试") && !live.contains("sess_1790180210926"),
            "抬头右边该是短 id 加会话名: {live:?}"
        );
        assert!(live.contains("│ 第 1 行") && live.contains("⋮"), "{live:?}");
        assert!(!live.contains("第 12 行"), "{live:?}");
        renderer.cut_timeline().unwrap();
        let frame = String::from_utf8_lossy(&renderer.take_output_frame()).into_owned();
        let id = block_id_in(&frame).expect("收缩行没挂块");
        let detail = crate::render::blocks::get(id)
            .unwrap_or_default()
            .iter()
            .filter_map(|line| block_id_in(line))
            .filter_map(crate::render::blocks::get)
            .flatten()
            .map(|line| crate::render::strip_ansi_text(&line))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(detail.contains("第 12 行"), "点开该有正文全文: {detail:?}");
    });
}

/// 列名单那一步：抬头直接叫「列出其他会话」，右边不再挂「列出开着的会话」那句同义说明
/// （用户 09-26：列名单就不该写成「给其他会话发送消息」）。
#[test]
fn the_list_step_is_titled_list_other_sessions() {
    with_blocks(|| {
        let mut renderer = timeline_renderer();
        renderer.use_buffered_output();
        renderer
            .write_tool_call(
                "send_to_other_running_session",
                &serde_json::json!({"action": "list"}).to_string(),
            )
            .unwrap();
        renderer
            .write_tool_result(
                "send_to_other_running_session",
                true,
                r#"{"ok":true,"sessions":[]}"#,
            )
            .unwrap();
        renderer.finalize_tools_summary().unwrap();
        let (_, live) = renderer.timeline_live(Vec::new());
        let live = crate::render::strip_ansi_text(&live.unwrap_or_default());
        let title = miyu_base::i18n::text("List other sessions", "列出其他会话");
        assert!(live.contains(title), "{live:?}");
        let send_title = miyu_base::i18n::text("Message another session", "给其他会话发送消息");
        assert!(!live.contains(send_title), "列名单不该还叫发消息: {live:?}");
        let old_peek = miyu_base::i18n::text("list open sessions", "列出开着的会话");
        assert!(!live.contains(old_peek), "右边不该再挂同义说明: {live:?}");
    });
}
