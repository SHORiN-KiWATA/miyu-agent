//! 命令块的实时显示、预览与终端安全。

use super::shared::*;
use crate::render::*;

#[test]
fn command_stream_handles_split_utf8_and_crlf() {
    let mut state = CommandStreamState::default();
    let text = "开始\r\n完成\n".as_bytes();
    let split = "开始".len() - 1;

    assert!(state.push(&text[..split], 1).is_empty());
    let completed = state.push(&text[split..], 2);

    assert_eq!(completed.len(), 2);
    assert_eq!(completed[0].text, "开始");
    assert_eq!(completed[1].text, "完成");
    assert!(state.current.is_empty());
}

#[test]
fn command_stream_carriage_return_replaces_current_line() {
    let mut state = CommandStreamState::default();

    assert!(state.push(b"progress 10%\r", 1).is_empty());
    let completed = state.push(b"progress 20%\n", 2);

    assert_eq!(completed.len(), 1);
    assert_eq!(completed[0].text, "progress 20%");
}

#[test]
fn command_stream_strips_split_terminal_sequences() {
    let mut state = CommandStreamState::default();

    assert!(state.push(b"safe\x1b[31", 1).is_empty());
    let completed = state.push(b"m red\x1b[0m\n", 2);

    assert_eq!(completed.len(), 1);
    assert_eq!(completed[0].text, "safe red");
}

#[test]
fn command_stream_finalizes_incomplete_utf8() {
    let mut state = CommandStreamState::default();

    assert!(state.push(&[0xe4, 0xb8], 1).is_empty());
    state.finalize_pending(1);

    assert_eq!(state.current, "�");
}

#[test]
fn command_text_strips_cursor_and_osc_sequences() {
    assert_eq!(
        sanitize_terminal_text("safe\x1b[2J text\x1b]52;c;secret\x07 end"),
        "safe text end"
    );
    assert_eq!(sanitize_terminal_text("a\x1b(Bb"), "ab");
}

#[test]
fn command_wrap_uses_terminal_width_for_wide_graphemes() {
    assert_eq!(wrap_plain_text("中文测试", 4), vec!["中文", "测试"]);
    assert_eq!(wrap_plain_text("a👨‍👩‍👧‍👦b", 3), vec!["a👨‍👩‍👧‍👦", "b"]);
    assert_eq!(wrap_plain_text("e\u{301}x", 1), vec!["e\u{301}", "x"]);
}

#[test]
fn display_width_clip_preserves_graphemes_and_reserves_last_column() {
    assert_eq!(clip_to_display_width("中文测试", 5), "中文…");
    assert_eq!(clip_to_display_width("a👨‍👩‍👧‍👦bc", 4), "a👨‍👩‍👧‍👦…");
    assert_eq!(clip_to_display_width("e\u{301}x", 2), "e\u{301}x");

    for columns in [20, 40, 80] {
        let lines = transient_summary_lines(&format!("思考：{}", "中文".repeat(80)), columns);
        assert_eq!(lines.len(), 1);
        assert!(UnicodeWidthStr::width(lines[0].as_str()) < columns);
    }
}

#[test]
fn command_preview_limits_physical_rows_and_keeps_tail() {
    let mut display = CommandLiveDisplay::new(r#"{"command":"demo"}"#, 3, true, false);
    display.push(CommandOutputStream::Stdout, b"one\ntwo\nthree\nfour\n");

    let lines = visible_command_lines(display.rendered_log_lines(80));

    assert_eq!(lines.len(), 3);
    assert!(lines[0].contains("omitted") || lines[0].contains("省略"));
    assert!(lines[1].ends_with("three"));
    assert!(lines[2].ends_with("four"));
}

#[test]
fn command_preview_counts_soft_wrapped_rows() {
    let mut display = CommandLiveDisplay::new(r#"{"command":"demo"}"#, 3, true, false);
    display.push(
        CommandOutputStream::Stdout,
        "第一行很长\n第二行\n".as_bytes(),
    );

    let lines = visible_command_lines(display.rendered_log_lines(4));

    assert_eq!(lines.len(), 3);
    assert!(lines[0].contains("omitted") || lines[0].contains("省略"));
    assert!(lines[1].ends_with("第二"));
    assert!(lines[2].ends_with("行"));
}

#[test]
fn command_preview_orders_interleaved_streams_and_colors_stderr() {
    let mut display = CommandLiveDisplay::new(r#"{"command":"demo"}"#, 4, true, false);
    display.push(CommandOutputStream::Stdout, b"out");
    display.push(CommandOutputStream::Stderr, b"err");

    let lines = display.rendered_log_lines(80);

    assert!(strip_ansi_for_test(&lines[0]).ends_with("out"));
    assert!(strip_ansi_for_test(&lines[1]).ends_with("err"));
    assert!(lines[0].contains("\x1b[2mout\x1b[0m"));
    assert!(!lines[0].contains("\x1b[33m"));
    assert!(lines[1].contains("\x1b[2m\x1b[31merr\x1b[0m"));
    assert!(lines[1].contains("\x1b[31m"));
}

#[test]
fn shared_command_output_preview_sanitizes_and_keeps_tail() {
    let mut output = CommandOutputTail::new(3);
    output.push(
        CommandOutputStream::Stdout,
        b"old\nprogress 10%\rprogress 20%\n",
    );
    output.push(CommandOutputStream::Stderr, b"\x1b[31mwarning\x1b[0m\n");
    let chinese = "完成".as_bytes();
    output.push(CommandOutputStream::Stdout, &chinese[..2]);
    output.push(CommandOutputStream::Stdout, &chinese[2..]);

    let preview = output.preview();

    assert!(preview.omitted);
    assert_eq!(preview.lines.len(), 3);
    assert_eq!(preview.lines[0].text, "progress 20%");
    assert_eq!(preview.lines[1].stream, "stderr");
    assert_eq!(preview.lines[1].text, "warning");
    assert_eq!(preview.lines[2].text, "完成");
}

#[test]
fn shared_command_output_preview_can_be_disabled() {
    let mut output = CommandOutputTail::new(0);
    output.push(CommandOutputStream::Stdout, b"hidden\n");

    let preview = output.preview();

    assert!(preview.lines.is_empty());
    assert!(!preview.omitted);
}

#[test]
fn command_heading_is_part_of_live_block_and_updates_status() {
    let mut display = CommandLiveDisplay::new(r#"{"command":"printf ok"}"#, 2, true, false);
    let running = visible_command_lines(display.rendered_lines(80, true));
    let command = t("run command", "运行命令");
    assert_eq!(
        running[0],
        format!("$ {command}×1 {}", t("running", "运行中"))
    );
    assert!(running[1].contains("printf ok"));

    display.set_result(true);
    let completed = visible_command_lines(display.rendered_lines(80, false));
    assert_eq!(completed[0], format!("$ {command}×1 ok"));
    assert_eq!(
        completed
            .iter()
            .filter(|line| line.starts_with(&format!("$ {command}")))
            .count(),
        1
    );
}

#[test]
fn compact_multiline_command_keeps_two_head_and_four_tail_lines() {
    let command = (1..=10)
        .map(|line| format!("command line {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    let arguments = serde_json::json!({ "command": command }).to_string();
    let display = CommandLiveDisplay::new(&arguments, 0, false, false);

    let lines = visible_command_lines(display.rendered_lines(120, false));

    assert_eq!(lines.len(), 8);
    assert!(lines[1].starts_with("  ↳ ") && lines[1].ends_with("command line 1"));
    assert!(lines[2].starts_with("  │ ") && lines[2].ends_with("command line 2"));
    assert!(lines[3].contains('4'));
    assert!(lines[3].contains("omitted") || lines[3].contains("省略"));
    assert!(lines[4].ends_with("command line 7"));
    assert!(lines[5].ends_with("command line 8"));
    assert!(lines[6].ends_with("command line 9"));
    assert!(lines[7].starts_with("  └ ") && lines[7].ends_with("command line 10"));
    assert!(!lines.iter().any(|line| line.ends_with("command line 3")));
    assert!(!lines.iter().any(|line| line.ends_with("command line 6")));
}

#[test]
fn full_multiline_command_keeps_every_logical_line() {
    let command = (1..=10)
        .map(|line| format!("command line {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    let arguments = serde_json::json!({ "command": command }).to_string();
    let display = CommandLiveDisplay::new(&arguments, 0, false, true);

    let lines = visible_command_lines(display.rendered_lines(120, false));

    assert_eq!(lines.len(), 11);
    assert!(lines.iter().any(|line| line.ends_with("command line 3")));
    assert!(lines.iter().any(|line| line.ends_with("command line 6")));
    assert!(!lines
        .iter()
        .any(|line| line.contains("omitted") || line.contains("省略")));
}

#[test]
fn multiline_command_soft_wraps_with_continuation_prefix() {
    let arguments = serde_json::json!({
        "command": "1234567890abcdef\nlast"
    })
    .to_string();
    let display = CommandLiveDisplay::new(&arguments, 0, false, false);

    let lines = visible_command_lines(display.rendered_lines(16, false));

    assert_eq!(lines[1], "  ↳ 123456789");
    assert_eq!(lines[2], "  │   0abcdef");
    assert_eq!(lines[3], "  └ last");
}

#[test]
fn final_multiline_command_wrap_closes_tree_on_last_physical_row() {
    let arguments = serde_json::json!({
        "command": "first\n1234567890abcdef"
    })
    .to_string();
    let display = CommandLiveDisplay::new(&arguments, 0, false, false);

    let lines = visible_command_lines(display.rendered_lines(16, false));

    assert_eq!(lines[2], "  │ 123456789");
    assert_eq!(lines[3], "  └   0abcdef");
}

#[test]
fn omitted_command_notice_wraps_within_narrow_width() {
    let command = (1..=10)
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");

    let lines = render_command_preview(&command, 12, false, false, 0);

    assert!(lines.iter().all(|line| command_ansi_width(line) <= 12));
    assert!(visible_command_lines(lines)
        .iter()
        .any(|line| line.contains('4')));
}

#[test]
fn command_display_detects_output_row_growth_before_redraw() {
    let mut display = CommandLiveDisplay::new(r#"{"command":"printf ok"}"#, 3, true, false);
    display.rendered_line_widths = display
        .rendered_lines(80, true)
        .iter()
        .map(|line| command_ansi_width(line))
        .collect();
    assert!(!display.tick_changes_layout_at_width(80));

    display.push(CommandOutputStream::Stdout, b"one\n");

    assert!(display.tick_changes_layout_at_width(80));
}

#[test]
fn committed_command_blocks_end_with_exactly_one_blank_line() {
    let mut live = Vec::new();
    write_command_block_gap(&mut live, false).unwrap();
    assert_eq!(live, b"\n\n");

    let mut already_terminated = Vec::new();
    write_command_block_gap(&mut already_terminated, true).unwrap();
    assert_eq!(already_terminated, b"\n");
}

#[test]
fn run_command_replaces_an_active_tool_summary() {
    let mut renderer = StreamRenderer::new(
        ReasoningDisplayMode::Summary,
        ToolCallDisplayMode::Summary,
        false,
        true,
        10,
    );
    renderer.live_summary = true;
    renderer.summary_line_active = true;
    renderer.summary_lines_active = 1;

    renderer
        .write_tool_call("run_command", r#"{"command":"printf ok"}"#)
        .unwrap();

    assert!(!renderer.summary_line_active);
    assert_eq!(renderer.summary_lines_active, 0);
    assert!(renderer.command_display.is_some());
    // 终端里走的是静态时间线：命令也是时间线上的一步，得在统计里占一格
    // （跑完收进时间线时清掉）；管道里才是"命令块自己画、不进统计"。
    assert!(renderer.tool_stats.contains_key("run_command"));
    renderer.live_summary = false;
    renderer.tool_stats.clear();
    renderer.command_display = None;
    renderer
        .write_tool_call("run_command", r#"{"command":"printf ok"}"#)
        .unwrap();
    assert!(renderer.command_display.is_some());
    assert!(renderer.tool_stats.is_empty());
}

#[test]
fn parses_command_result_json() {
    let result = parse_command_result(
        r#"{"success":false,"exit_code":1,"stdout":"unused","stderr":"not found"}"#,
    )
    .unwrap();
    assert!(!result.success);
    assert_eq!(result.exit_code, Some(1));
    assert_eq!(result.stdout, "unused");
    assert_eq!(result.stderr, "not found");
}

#[test]
fn command_tool_family_covers_native_bash() {
    assert!(is_command_tool("run_command"));
    assert!(is_command_tool("Bash"));
    assert!(!is_command_tool("read_file"));
}

/// 展开视图里的命令按语法着色；heredoc 体按**解释器**认语言。
///
/// 用户 09-19：AI 写了个 `python3 - <<'PY'` 塞五十行 Python 的命令，展开之后
/// 是一片单色，想要语法高亮。
#[test]
fn an_expanded_command_is_syntax_highlighted_per_heredoc_language() {
    use crate::render::command_line_languages;

    let command = "python3 - <<'PY'\nimport os\nx = 1  # 注释\nPY\necho done";
    assert_eq!(
        command_line_languages(command),
        vec!["bash", "python", "python", "bash", "bash"],
        "heredoc 体该按 python 着色，起止两行仍是 shell"
    );
    // 认不出解释器的（`cat > 文件`）是数据，不猜。
    assert_eq!(
        command_line_languages("cat > a.txt <<'EOF'\nhello\nEOF"),
        vec!["bash", "", "bash"]
    );
    // here-string（`<<<`）不开体。
    assert_eq!(
        command_line_languages("grep x <<< \"$y\"\necho ok"),
        vec!["bash", "bash"]
    );
    // `<<-` 与不加引号的标记都认。
    assert_eq!(
        command_line_languages("python3 <<-PY\npass\nPY"),
        vec!["bash", "python", "bash"]
    );
    // 没有 heredoc 时整条都是 shell。
    assert_eq!(command_line_languages("ls -la"), vec!["bash"]);
}

/// 着色真的落到展开内容里了——而且认不出语言的那几行仍然是暗的。
#[test]
fn the_expanded_command_detail_carries_highlight_escapes() {
    let args = serde_json::json!({ "command": "python3 - <<'PY'\nimport os\nPY" });
    let mut display = CommandLiveDisplay::new(&args.to_string(), 10, true, false);
    display.set_result(true);
    let detail = display.timeline_detail(80);
    let text = detail.join("\n");
    assert!(
        text.contains("\x1b[38;2;"),
        "展开内容里没有着色转义:\n{text:?}"
    );
    // 单行命令不着色：逐词上色只是整行变亮，没有信息增益，而「命令别比输出
    // 亮」是 09-17 定下的（用户 09-19 要的是长脚本读得清楚）。
    let one_line = serde_json::json!({ "command": "tail -f log" });
    let mut one_line = CommandLiveDisplay::new(&one_line.to_string(), 10, true, false);
    one_line.set_result(true);
    let plain = one_line.timeline_detail(80).join("\n");
    assert!(
        !plain.contains("\x1b[38;2;") && plain.contains("\x1b[2m"),
        "单行命令不该被着色:\n{plain:?}"
    );
    let data_args = serde_json::json!({ "command": "cat > a.txt <<'EOF'\nhello\nEOF" });
    let mut data = CommandLiveDisplay::new(&data_args.to_string(), 10, true, false);
    data.set_result(true);
    let body = data.timeline_detail(80);
    assert!(
        body.iter()
            .any(|line| line.contains("hello") && line.contains("\x1b[2m")),
        "heredoc 里的数据行该维持暗色:\n{body:?}"
    );
}
