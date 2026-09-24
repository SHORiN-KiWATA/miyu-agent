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
        turn_started: None,
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
    let line = repl_footer_line(
        PersonaLane::Active,
        false,
        &footer,
        48,
        UsagePlacement::FooterRight,
    );
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
                    for usage in [UsagePlacement::FooterRight, UsagePlacement::RowBelow] {
                        assert_single_row(
                            &repl_footer_line(mode, false, &footer, cols, usage),
                            cols,
                        );
                    }
                    assert_single_row(&repl_usage_line(&footer, cols), cols);
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
        let line = repl_footer_line(mode, false, &footer, 160, UsagePlacement::FooterRight);
        assert!(line.contains(&expected));
        assert_single_row(&line, 160);
    }
}

/// 这一轮的总计时（09-24）：紧跟在声波右边，跑完就不显示。退回「跟在用量后面、
/// 跑完停在总用时上」那一版这条会红。
#[test]
fn the_turn_clock_rides_right_of_the_wave_only_while_running() {
    let mut footer = status("model-name", true);
    footer.turn_started =
        std::time::Instant::now().checked_sub(std::time::Duration::from_secs(3_723));
    let left = repl_footer_left(PersonaLane::Active, false, &footer, 120);
    let wave = sound_wave_frame(7, false);
    let clock_at = left.find("1h 02m 03s").expect("clock shown while running");
    assert!(left.find(&wave).unwrap() < clock_at, "{left}");
    for cols in [48, 60, 80, 160] {
        for usage in [UsagePlacement::FooterRight, UsagePlacement::RowBelow] {
            assert_single_row(
                &repl_footer_line(PersonaLane::Active, false, &footer, cols, usage),
                cols,
            );
        }
    }

    footer.running_spinner = None;
    let done = strip_terminal_control_sequences(&repl_footer_line(
        PersonaLane::Active,
        false,
        &footer,
        160,
        UsagePlacement::RowBelow,
    ));
    assert!(
        !done.contains("02m"),
        "a finished turn shows no clock: {done}"
    );
}

/// 全屏下用量挪到 footer 底下那一行（用户 09-24）：footer 只剩模式、模型、声波，
/// 用量那一行右对齐、整行垫满。退回「用量跟在 footer 右端」这条会红。
#[test]
fn fullscreen_moves_the_usage_to_its_own_row() {
    let mut footer = status("model-name", true);
    footer.token_usage.generation_tokens = 420;
    footer.token_usage.generation_ms = 10_000;
    let line = strip_terminal_control_sequences(&repl_footer_line(
        PersonaLane::Active,
        false,
        &footer,
        120,
        UsagePlacement::RowBelow,
    ));
    for gauge in ["tok/s", "Σ", "/1M"] {
        assert!(!line.contains(gauge), "{gauge} left the footer: {line}");
    }
    let usage = strip_terminal_control_sequences(&repl_usage_line(&footer, 120));
    assert_eq!(
        usage.trim_start(),
        "42 tok/s · 21.7k/1M(2.2%) · Σ180.1k",
        "{usage}"
    );
    // 窄了先丢速度，上下文表撑到最后。
    let narrow = strip_terminal_control_sequences(&repl_usage_line(&footer, 30));
    assert_eq!(narrow.trim_start(), "21.7k/1M(2.2%) · Σ180.1k", "{narrow}");
    assert!(
        usage.starts_with(' ') && !usage.ends_with(' '),
        "right-aligned: {usage:?}"
    );
}
