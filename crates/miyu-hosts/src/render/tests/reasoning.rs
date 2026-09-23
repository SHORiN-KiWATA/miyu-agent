//! 推理阶段的计时、标题与状态。

use super::shared::*;
use crate::render::*;

#[test]
fn full_reasoning_reapplies_color_for_every_chunk() {
    let mut green = Vec::new();
    execute!(green, SetForegroundColor(Color::Green)).unwrap();
    let green = String::from_utf8(green).unwrap();
    let mut output = Vec::new();

    write_full_reasoning_chunk(&mut output, "用户").unwrap();
    execute!(output, ResetColor).unwrap();
    write_full_reasoning_chunk(&mut output, "询问明天几号").unwrap();

    let output = String::from_utf8(output).unwrap();
    assert_eq!(output.matches(&green).count(), 2);
    assert!(output.ends_with("询问明天几号"));
}

#[test]
fn external_cursor_control_suppresses_renderer_visibility_changes() {
    let mut renderer = StreamRenderer::new(
        ReasoningDisplayMode::Summary,
        ToolCallDisplayMode::Summary,
        false,
        true,
        10,
    );
    renderer.use_external_cursor_control();

    renderer.hide_cursor().unwrap();
    assert!(!renderer.cursor_hidden);
    renderer.cursor_hidden = true;
    renderer.show_cursor().unwrap();
    assert!(renderer.cursor_hidden);
}

/// 参数从「哪一档」换成了「这一档到底往屏上流没流字」（`captures_reasoning`）：
/// 全屏下 `Full` 也收进时间线，看档位就答错了。
#[test]
fn pending_summary_reasoning_does_not_add_a_leading_newline_on_finish() {
    // 收进时间线了：一个字都没打到屏上，补换行就是凭空多一行。
    assert!(!stream_needs_terminating_newline(
        Some(ChatStreamKind::Reasoning),
        true,
    ));
    // 直接往屏上流的那条路（没有时间线的面）：收尾要补。
    assert!(stream_needs_terminating_newline(
        Some(ChatStreamKind::Reasoning),
        false,
    ));
    // 正文与思考怎么显示无关。
    assert!(stream_needs_terminating_newline(
        Some(ChatStreamKind::Content),
        true,
    ));
}

#[test]
fn finish_keeps_pending_reasoning_summary_state() {
    let mut renderer = StreamRenderer::new(
        ReasoningDisplayMode::Summary,
        ToolCallDisplayMode::Summary,
        false,
        true,
        10,
    );
    renderer.reasoning_title = Some("检查摘要状态".to_string());
    renderer.reasoning_text = "some reasoning".to_string();
    renderer.reasoning_started_at = Some(std::time::Instant::now());
    renderer.finish().unwrap();
    assert!(renderer.reasoning_text.is_empty());
    assert!(renderer.reasoning_title.is_none());
    assert!(renderer.reasoning_started_at.is_none());
    assert!(!renderer.summary_line_active);
}

#[test]
fn reasoning_summary_counts_tokens_and_uses_title() {
    let mut renderer = StreamRenderer::new(
        ReasoningDisplayMode::Summary,
        ToolCallDisplayMode::Summary,
        false,
        true,
        10,
    );
    renderer.record_reasoning_text("one\nt");
    renderer.record_reasoning_text("wo\nthree");
    renderer.reasoning_title = Some("分析摘要协议".to_string());
    // 词元数按 chunk 增量累加(避免每 chunk 对全文 O(n²) 重算),
    // 期望值即各 chunk 估算之和;跨 chunk 切词处与全文重算略有出入。
    let expected = miyu_base::token_estimate::estimate_tokens("one\nt")
        + miyu_base::token_estimate::estimate_tokens("wo\nthree");
    let summary = renderer.reasoning_summary_text();
    let title_separator = t(": ", "：");
    assert!(summary.starts_with(&format!(
        "{}{title_separator}分析摘要协议 · ",
        t("thinking", "思考")
    )));
    assert!(summary.contains(&format!("{expected} {}", t("tokens", "词元"))));
    assert!(!summary.contains("字符"));
    assert!(!summary.contains(" 行"));
}

#[test]
fn reasoning_without_title_still_estimates_summary_tokens() {
    let mut renderer = StreamRenderer::new(
        ReasoningDisplayMode::Summary,
        ToolCallDisplayMode::Summary,
        true,
        true,
        10,
    );
    renderer
        .start_reasoning_phase(std::time::Instant::now())
        .unwrap();
    renderer.record_reasoning_text("Plain summary content without a title.");

    let expected = miyu_base::token_estimate::estimate_tokens(&renderer.reasoning_text);
    let live = renderer.waiting_phase_text();
    assert!(live.starts_with(&format!("{} · ", t("thinking", "思考"))));
    assert!(live.contains(&format!("{expected} {}", t("tokens", "词元"))));
}

#[test]
fn reasoning_part_end_commits_state_and_starts_next_timer_at_boundary() {
    let mut renderer = StreamRenderer::new(
        ReasoningDisplayMode::Summary,
        ToolCallDisplayMode::Summary,
        false,
        true,
        10,
    );
    let started_at = std::time::Instant::now();
    let ended_at = started_at + std::time::Duration::from_millis(750);
    renderer.start_reasoning_phase(started_at).unwrap();
    renderer.reasoning_title = Some("检查当前阶段".to_string());
    renderer.record_reasoning_text("summary body");

    renderer.finish_reasoning_part(ended_at).unwrap();

    assert!(renderer.reasoning_title.is_none());
    assert!(renderer.reasoning_text.is_empty());
    assert_eq!(renderer.reasoning_tokens, 0);
    assert_eq!(renderer.reasoning_started_at, Some(ended_at));
    assert!(renderer.reasoning_elapsed.is_none());
}

#[test]
fn new_reasoning_part_starts_a_fresh_timer_and_estimate() {
    let mut renderer = StreamRenderer::new(
        ReasoningDisplayMode::Summary,
        ToolCallDisplayMode::Summary,
        false,
        true,
        10,
    );
    let started_at = std::time::Instant::now();
    let next_part_at = started_at + std::time::Duration::from_millis(900);
    renderer.start_reasoning_phase(started_at).unwrap();
    renderer.reasoning_title = Some("上一阶段".to_string());
    renderer.record_reasoning_text("old body");

    renderer.start_reasoning_part(next_part_at).unwrap();

    assert!(renderer.reasoning_title.is_none());
    assert!(renderer.reasoning_text.is_empty());
    assert_eq!(renderer.reasoning_tokens, 0);
    assert_eq!(renderer.reasoning_started_at, Some(next_part_at));
    assert!(renderer.reasoning_elapsed.is_none());
}

#[test]
fn frozen_reasoning_elapsed_ignores_renderer_processing_delay() {
    let mut renderer = StreamRenderer::new(
        ReasoningDisplayMode::Summary,
        ToolCallDisplayMode::Summary,
        true,
        true,
        10,
    );
    let started_at = std::time::Instant::now() - std::time::Duration::from_secs(30);
    renderer.reasoning_started_at = Some(started_at);
    renderer.freeze_reasoning_elapsed_at(started_at + std::time::Duration::from_millis(1_500));
    renderer.reasoning_title = Some("检查事件排队".to_string());

    assert_eq!(
        renderer.reasoning_elapsed,
        Some(std::time::Duration::from_millis(1_500))
    );
    assert!(renderer.reasoning_summary_text().ends_with(" · 1.5s"));
}

#[test]
fn reasoning_title_is_not_truncated_at_forty_characters() {
    let mut renderer = StreamRenderer::new(
        ReasoningDisplayMode::Summary,
        ToolCallDisplayMode::Summary,
        false,
        true,
        10,
    );
    renderer.live_summary = false;
    let title = "a".repeat(60);

    renderer.write_reasoning_title(&title).unwrap();

    assert_eq!(renderer.reasoning_title.as_deref(), Some(title.as_str()));
}

#[test]
fn reasoning_elapsed_uses_milliseconds_then_decimal_seconds() {
    assert_eq!(format_reasoning_elapsed(std::time::Duration::ZERO), "<1ms");
    assert_eq!(
        format_reasoning_elapsed(std::time::Duration::from_nanos(1)),
        "<1ms"
    );
    assert_eq!(
        format_reasoning_elapsed(std::time::Duration::from_millis(38)),
        "38ms"
    );
    assert_eq!(
        format_reasoning_elapsed(std::time::Duration::from_millis(976)),
        "976ms"
    );
    assert_eq!(
        format_reasoning_elapsed(std::time::Duration::from_millis(11_700)),
        "11.7s"
    );
    // 过了一分钟与其它用时同一个写法(09-23)。
    assert_eq!(
        format_reasoning_elapsed(std::time::Duration::from_secs(3_725)),
        "1h 02m 05s"
    );
}

#[test]
fn reasoning_phase_starts_as_neutral_waiting_without_content() {
    let mut renderer = StreamRenderer::new(
        ReasoningDisplayMode::Summary,
        ToolCallDisplayMode::Summary,
        true,
        true,
        10,
    );

    renderer
        .start_reasoning_phase(std::time::Instant::now() - std::time::Duration::from_millis(1_200))
        .unwrap();

    assert!(renderer.reasoning_title.is_none());
    assert_eq!(renderer.waiting_phase_text(), "1.2s");
    assert!(!renderer.waiting_phase_text().contains("思考"));
    assert!(!renderer.waiting_phase_text().contains("词元"));
}

#[test]
fn preparing_question_phase_overrides_reasoning_timer_until_handoff() {
    let mut renderer = StreamRenderer::new(
        ReasoningDisplayMode::Summary,
        ToolCallDisplayMode::Summary,
        true,
        true,
        10,
    );
    renderer.reasoning_started_at =
        Some(std::time::Instant::now() - std::time::Duration::from_secs(30));
    renderer.preparing_question_started_at =
        Some(std::time::Instant::now() - std::time::Duration::from_millis(1_200));

    let phase = renderer.waiting_phase_text();

    assert!(phase.starts_with(t("~ Preparing question · ", "~ 准备问题 · ")));
    assert!(phase.ends_with("1.2s"));
    renderer.prepare_for_external_output().unwrap();
    assert!(renderer.preparing_question_started_at.is_none());
}

/// 批量里的计时是整个准备窗口的，不是每个工具各算各的。
///
/// `write_tool_call` 会清掉 `tool_preparing`，锚点若跟着它走，第二个
/// 工具一到秒数就归零，屏幕上来回横跳。
#[test]
fn batch_preparation_timer_spans_the_whole_window() {
    let mut renderer = StreamRenderer::new(
        ReasoningDisplayMode::Summary,
        ToolCallDisplayMode::Summary,
        false,
        true,
        10,
    );
    renderer.use_external_cursor_control();
    renderer.use_buffered_output();
    renderer.live_summary = true;

    renderer.write_tool_preparing("apply_patch", false).unwrap();
    let first = renderer.tool_preparing.expect("准备状态已建立").2;
    // 参数流完 → 这个工具的准备结束，但整批还没完。
    renderer.write_tool_call("apply_patch", "{}").unwrap();
    assert!(renderer.tool_preparing.is_none());
    renderer.write_tool_preparing("write_file", false).unwrap();
    assert_eq!(
        renderer.tool_preparing.expect("第二个工具的准备状态").2,
        first,
        "同一批里的计时起点必须复用"
    );

    // 工具真跑起来了 = 窗口结束，下一批重新计时。
    renderer
        .write_tool_result("write_file", true, "ok")
        .unwrap();
    renderer.write_tool_preparing("apply_patch", false).unwrap();
    assert_ne!(
        renderer.tool_preparing.expect("新一批的准备状态").2,
        first,
        "工具跑完之后应该重新起算"
    );
}

#[test]
fn buffered_output_returns_complete_frames_without_terminal_queries() {
    let mut renderer = StreamRenderer::new(
        ReasoningDisplayMode::Hidden,
        ToolCallDisplayMode::Hidden,
        true,
        true,
        10,
    );
    renderer.use_external_cursor_control();
    renderer.use_buffered_output();
    renderer
        .write_chunk(ChatStreamChunk {
            kind: ChatStreamKind::Content,
            text: "hello".to_string(),
        })
        .unwrap();

    assert_eq!(renderer.take_output_frame(), b"hello");
    assert!(renderer.take_output_frame().is_empty());

    renderer.finish().unwrap();
    let frame = renderer.take_output_frame();
    assert_eq!(frame, b"\n");
    assert!(!frame.windows(5).any(|bytes| bytes == b"?2026"));
    assert!(!frame.windows(3).any(|bytes| bytes == b"[6n"));
}

#[test]
fn full_reasoning_waiting_phase_is_empty() {
    let renderer = StreamRenderer::new(
        ReasoningDisplayMode::Full,
        ToolCallDisplayMode::Summary,
        true,
        true,
        10,
    );

    assert!(renderer.waiting_phase_text().is_empty());
}

/// 等待计时器要在每次工具跑完后重新起算(08-26 用户点名"进度条计时是累计
/// 的")。锚点是 ReasoningStart,而它每次 LLM 请求才发一次;claude-code 的工具
/// 在同一条流里闭环执行,整轮只有一次请求,不重锚就会一路累加。
#[test]
fn wait_timer_reanchors_after_each_tool_result() {
    let mut renderer = StreamRenderer::new(
        ReasoningDisplayMode::Summary,
        ToolCallDisplayMode::Summary,
        true,
        true,
        10,
    );
    let started = std::time::Instant::now() - std::time::Duration::from_secs(30);
    renderer.reasoning_started_at = Some(started);

    renderer.reanchor_wait_timer();
    let reanchored = renderer.reasoning_started_at.expect("锚点仍在");
    assert!(
        reanchored > started,
        "工具跑完后计时应从此刻重新起算,而不是接着回合开头累加"
    );
    assert!(renderer.reasoning_elapsed.is_none());

    // 有待落地的思考摘要时不动:那条要显示的是它自己那段的耗时。
    renderer.reasoning_started_at = Some(started);
    renderer.reasoning_title = Some("盘算".to_string());
    renderer.reanchor_wait_timer();
    assert_eq!(renderer.reasoning_started_at, Some(started));

    // 没有在计时就不凭空造一个锚点。
    renderer.reasoning_title = None;
    renderer.reasoning_started_at = None;
    renderer.reanchor_wait_timer();
    assert!(renderer.reasoning_started_at.is_none());
}

/// 09-10 终端截图:正文行开着时来一个空的推理分段(没有推理文本、没有待写
/// 的摘要),不能把正文截成一行一段。
#[test]
fn empty_reasoning_part_mid_content_does_not_break_the_line() {
    let mut renderer = StreamRenderer::new(
        ReasoningDisplayMode::Summary,
        ToolCallDisplayMode::Summary,
        false,
        true,
        10,
    );
    renderer.use_external_cursor_control();
    renderer.use_buffered_output();
    let now = std::time::Instant::now();
    let content = |text: &str| ChatStreamChunk {
        kind: ChatStreamKind::Content,
        text: text.to_string(),
    };

    renderer.write_chunk(content("问我的模型")).unwrap();
    renderer.start_reasoning_part(now).unwrap();
    renderer.finish_reasoning_part(now).unwrap();
    renderer.write_chunk(content("的话")).unwrap();
    renderer.start_reasoning_part(now).unwrap();
    renderer.finish_reasoning_part(now).unwrap();
    renderer.write_chunk(content("，这有什么好讲的。")).unwrap();
    renderer.finish().unwrap();

    let frame = String::from_utf8(renderer.take_output_frame()).unwrap();
    let plain = strip_ansi_for_test(&frame);
    // 正文之间不许夹换行;收尾的换行(markdown 行尾 + finish)不算。
    assert_eq!(
        plain.trim_end_matches('\n'),
        "问我的模型的话，这有什么好讲的。",
        "正文应仍是一整行,实得:{plain:?}"
    );
}

/// 真·交错思考:推理文本落在正文中间时,先收正文行再起转轮,不能在半行
/// 正文上 MoveToColumn(0)+清行。
#[test]
fn real_reasoning_mid_content_ends_the_line_before_the_spinner() {
    let mut renderer = StreamRenderer::new(
        ReasoningDisplayMode::Summary,
        ToolCallDisplayMode::Summary,
        false,
        true,
        10,
    );
    renderer.use_external_cursor_control();
    renderer.use_buffered_output();
    let now = std::time::Instant::now();

    renderer
        .write_chunk(ChatStreamChunk {
            kind: ChatStreamKind::Content,
            text: "前半句".to_string(),
        })
        .unwrap();
    renderer.start_reasoning_part(now).unwrap();
    renderer
        .write_chunk(ChatStreamChunk {
            kind: ChatStreamKind::Reasoning,
            text: "想一下".to_string(),
        })
        .unwrap();
    let frame = String::from_utf8(renderer.take_output_frame()).unwrap();
    let plain = strip_ansi_for_test(&frame);
    assert!(
        plain.starts_with("前半句\n"),
        "推理文本到来时正文行应先被收掉,实得:{plain:?}"
    );
    assert_eq!(renderer.mode, Some(ChatStreamKind::Reasoning));
}

/// 「显示思考过程 = 详细」在全屏下走时间线，不再绕过它直接打屏。
///
/// 原来那一档走的是 `write_full_reasoning_chunk`——把思考正文原样流到屏上，
/// **没有连线、没有那一步**，`Worked for` 也数不到它（用户 todolist:11「不是把
/// timeline 的思考内容自动展开，而是用的旧版本的 inline」）。实测那时的输出是
///
/// ```text
/// 先看一眼需求，再决定怎么下手。  好了。
/// ```
///
/// **正在想**的那一行也算这一步：一步的大半辈子是在 live 区里度过的。只给落
/// 下来的那一份的话，用户看到的是「想完了才展开」（用户 09-17 实测原话：「思考
/// 的展开是思考完成才展开，思考完成之前还是单行窥视的状态」）。
#[test]
fn the_live_thinking_row_is_expanded_too() {
    use crate::render::{ReasoningDisplayMode, StreamRenderer, ToolCallDisplayMode};
    use miyu_core::llm::{ChatStreamChunk, ChatStreamKind};

    fn run(mode: ReasoningDisplayMode) -> String {
        let mut renderer = StreamRenderer::new(mode, ToolCallDisplayMode::Summary, false, true, 8);
        renderer.use_external_cursor_control();
        renderer.use_buffered_output();
        renderer.use_terminal_surface();
        renderer
            .start_reasoning_phase(std::time::Instant::now())
            .unwrap();
        renderer
            .write_chunk(ChatStreamChunk {
                kind: ChatStreamKind::Reasoning,
                text: "还在想，这一段正文只在展开之后才看得到。".to_string(),
            })
            .unwrap();
        // 转轮走一帧：live 区那几行就是这一帧写出去的。
        renderer.last_tick = None;
        renderer.tick_spinner().unwrap();
        String::from_utf8_lossy(&renderer.take_output_frame()).into_owned()
    }

    super::timeline::with_blocks(|| {
        // 转轮只在「字节落进真终端」时才转（`WaitSpinner::supported`），而
        // live 区那几行是转轮那一帧写出去的。测试里靠宽度覆盖告诉它这一点。
        crate::render::set_cols_override(100);
        let full = run(ReasoningDisplayMode::Full);
        assert!(
            full.contains("miyu-block-open="),
            "还在想的那一行没带「默认开着」: {full:?}"
        );
        assert!(
            !run(ReasoningDisplayMode::Summary).contains("miyu-block-open="),
            "收起档不该默认展开"
        );
        crate::render::set_cols_override(0);
    });
}

/// 现在它和收起档走同一条路，差别只在**这一步默认开着还是合着**：展开档那一步
/// 出来就是展开态（块标记是 `miyu-block-open=`），再点一次收回去；收起档出来就
/// 是合着的。
///
/// 它一度是「全文摆在抬头底下当预览」——那一行明明还能点，点开看到的又是几乎
/// 同一份内容。用户 09-17 拍掉了那种形态：「不应该以 tag 行下预览的形式出现
/// tag 行的内容」。
#[test]
fn expanded_reasoning_opens_its_step_instead_of_previewing_under_the_head() {
    use crate::render::{ReasoningDisplayMode, StreamRenderer, ToolCallDisplayMode};
    use miyu_core::llm::{ChatStreamChunk, ChatStreamKind};

    fn run(mode: ReasoningDisplayMode) -> String {
        let mut renderer = StreamRenderer::new(mode, ToolCallDisplayMode::Summary, false, true, 8);
        renderer.use_buffered_output();
        renderer.use_terminal_surface();
        for text in ["先看一眼需求，", "再决定怎么下手。"] {
            renderer
                .write_chunk(ChatStreamChunk {
                    kind: ChatStreamKind::Reasoning,
                    text: text.to_string(),
                })
                .unwrap();
        }
        // 起一个工具让这一段继续：思考那一步于是留在 live 区里，连同它底下
        // 露出来的内容一起重画（段末收成 `Worked for …` 之后就要点开才看得到，
        // 那是另一件事）。
        renderer
            .write_tool_call("read", r#"{"path":"/tmp/a"}"#)
            .unwrap();
        String::from_utf8_lossy(&renderer.take_output_frame()).into_owned()
    }

    super::timeline::with_blocks(|| {
        // 转轮只在「字节落进真终端」时才转（`WaitSpinner::supported`），而
        // live 区那几行是转轮那一帧写出去的。测试里靠宽度覆盖告诉它这一点。
        crate::render::set_cols_override(100);
        let full = run(ReasoningDisplayMode::Full);
        let full_text = super::shared::strip_ansi_for_test(&full);
        assert!(
            full_text.contains(&t("thought", "已思考")),
            "展开档没有落成时间线里的一步:\n{full_text}"
        );
        assert!(
            full.contains("miyu-block-open="),
            "展开档那一步该出来就是展开态"
        );
        // 全文**不**再铺在抬头底下：它是那一块的内容，展开态里看到的就是它。
        assert!(
            !full_text.contains("│ 先看一眼需求，再决定怎么下手。"),
            "还在往 tag 行底下挂预览:\n{full_text}"
        );

        // 收起档同一段过程也该有那一步，只是出来是合着的。
        let summary = run(ReasoningDisplayMode::Summary);
        let summary_text = super::shared::strip_ansi_for_test(&summary);
        assert!(
            summary_text.contains(&t("thought", "已思考")),
            "收起档丢了那一步:\n{summary_text}"
        );
        assert!(!summary.contains("miyu-block-open="), "收起档不该默认展开");
        assert!(summary.contains("miyu-block="), "收起档那一步总该点得开");
    });
}
