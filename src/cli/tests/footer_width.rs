//! A footer must stay on its own row while its animation is redrawn.

use crate::cli::repl::layout::terminal_frame_layout;
use crate::cli::*;

fn status(model: &str, running: bool) -> ReplFooterStatus {
    ReplFooterStatus {
        goal: None,
        provider: "provider".to_string(),
        model: model.to_string(),
        mixed_models: false,
        thinking: Some("high".to_string()),
        token_usage: render::TokenMeter {
            session_tokens: 21_700,
            context_window: Some(1_000_000),
            cumulative_tokens: Some(180_100),
            ..Default::default()
        },
        running_spinner: running.then_some(7),
    }
}

fn assert_single_row(line: &str, cols: usize) {
    let plain = strip_terminal_control_sequences(line);
    let width = UnicodeWidthStr::width(plain.as_str());
    assert!(width <= cols, "cols={cols}, width={width}: {plain}");
    assert_eq!(width, cols, "footer redraw must erase the previous fields");
    let layout = terminal_frame_layout(line.as_bytes(), (0, 4), cols as u16, None);
    assert_eq!(layout.cursor.1, 4, "footer wrapped: cols={cols}: {plain}");
    assert_eq!(layout.occupied_bottom, Some(4));
}

#[test]
fn running_footer_reserves_wave_width_at_48_columns() {
    let footer = status("a-very-long-model-name-with-a-version-suffix", true);
    let line = repl_footer_line(PersonaLane::Active, false, &footer, 48);
    assert_single_row(&line, 48);
    assert!(line.contains(&sound_wave_frame(7, false)));
    assert!(line.contains(&primary_footer_text("high")));
}

#[test]
fn footer_stays_on_one_terminal_row_at_every_narrow_width() {
    for model in [
        "a-very-long-model-name-with-a-version-suffix",
        "中文模型名称附带很长版本后缀",
        "e\u{301}-family-👨‍👩‍👧‍👦-model-with-a-long-suffix",
        "ＡＢＣＤＥＦ-fullwidth-model-name",
    ] {
        for running in [false, true] {
            let footer = status(model, running);
            for mode in [PersonaLane::Active, PersonaLane::Dev] {
                for cols in 1..=160 {
                    assert_single_row(&repl_footer_line(mode, false, &footer, cols), cols);
                }
            }
        }
    }
}

#[test]
fn footer_left_also_respects_its_own_width_budget() {
    let footer = status("中文模型名称附带很长版本后缀", true);
    for width in 0..=80 {
        let left = repl_footer_left(PersonaLane::Active, false, &footer, width);
        let plain = strip_terminal_control_sequences(&left);
        assert!(
            UnicodeWidthStr::width(plain.as_str()) <= width,
            "width={width}: {plain}"
        );
    }
}

#[test]
fn wide_footer_keeps_all_fields_and_wave_unchanged() {
    let footer = status("test-model", true);
    for mode in [PersonaLane::Active, PersonaLane::Dev] {
        let expected = format!(
            "{} · test-model \x1b[2mprovider\x1b[0m · {}   {}",
            colored_footer_mode_label(mode),
            primary_footer_text("high"),
            sound_wave_frame(7, mode == PersonaLane::Dev),
        );
        assert_eq!(repl_footer_left(mode, false, &footer, 120), expected);
        let line = repl_footer_line(mode, false, &footer, 160);
        assert!(line.contains(&expected));
        assert_single_row(&line, 160);
    }
}
