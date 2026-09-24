//! 思考停了、别的也还没来：收成「已思考」，live 区只留转轮（见 `timeline/stall.rs`）。

use super::shared::strip_ansi_for_test;
use super::timeline::{timeline_renderer, with_blocks};
use crate::render::{t, StreamRenderer};
use miyu_core::llm::{ChatStreamChunk, ChatStreamKind};
use std::time::{Duration, Instant};

fn before(base: Instant, seconds: f64) -> Instant {
    base.checked_sub(Duration::from_secs_f64(seconds))
        .expect("单调钟能往回倒几秒")
}

/// 请求发出去想了一秒，然后静默了 `silent` 秒。
///
/// 两个时刻从同一个基准倒推：分两次取「现在」的话，中间那几十毫秒会让
/// 「想了一秒」四舍五入成 `1.1s`。
fn thinking_then_silent(silent: f64) -> StreamRenderer {
    let base = Instant::now();
    let mut renderer = timeline_renderer();
    renderer.use_external_cursor_control();
    renderer.use_buffered_output();
    renderer.use_terminal_surface();
    renderer
        .start_reasoning_phase(before(base, 1.0 + silent))
        .unwrap();
    renderer
        .write_chunk(ChatStreamChunk {
            kind: ChatStreamKind::Reasoning,
            text: "先看一眼需求，再决定怎么改。".to_string(),
        })
        .unwrap();
    renderer.reasoning_last_delta_at = Some(before(base, silent));
    renderer.last_tick = None;
    renderer.tick_spinner().unwrap();
    renderer
}

fn live_text(renderer: &StreamRenderer) -> String {
    strip_ansi_for_test(&renderer.timeline_waiting().1.unwrap_or_default())
}

/// 线路把整段工具调用扣着不放（opencodego 的 deepseek-v4.1-flash、bigmodel 的
/// glm-5.3-flash）：思考停了好几秒，屏上不该还是「思考中」在走秒。
#[test]
fn a_stalled_thought_lands_and_leaves_only_the_spinner() {
    with_blocks(|| {
        // 转轮只在「字节落进真终端」时才转（`WaitSpinner::supported`）。
        crate::render::set_cols_override(100);
        let mut renderer = thinking_then_silent(2.0);
        let live = live_text(&renderer);
        assert!(
            !live.contains(&t("thinking", "思考中")),
            "停了两秒还在说思考中:\n{live}"
        );
        assert!(
            live.contains(&t("thought", "已思考")),
            "那段思考该收成一步:\n{live}"
        );
        // 秒数停在最后一片思考上：想了一秒，不是一秒加两秒的空档。
        assert!(live.contains("1.0s"), "思考计时把空档也算进去了:\n{live}");
        // 收完之后的最后一行只有转轮和连线，没有字。
        let last = live.lines().last().unwrap_or_default();
        assert!(
            last.chars()
                .all(|c| !c.is_alphanumeric() && !('\u{4e00}'..='\u{9fff}').contains(&c)),
            "只该剩转轮:{last:?}"
        );

        // 又想起来了：另起一段「思考中」，不崩也不丢字。
        renderer
            .write_chunk(ChatStreamChunk {
                kind: ChatStreamKind::Reasoning,
                text: "还得再核对一下。".to_string(),
            })
            .unwrap();
        renderer.last_tick = None;
        renderer.tick_spinner().unwrap();
        assert!(
            live_text(&renderer).contains(&t("thinking", "思考中")),
            "接着想的那一段没接上:\n{}",
            live_text(&renderer)
        );
        crate::render::set_cols_override(0);
    });
}

/// 想到一半停一下（实测最长 0.17s）照旧是「思考中」。
#[test]
fn a_short_pause_mid_thought_is_still_thinking() {
    with_blocks(|| {
        crate::render::set_cols_override(100);
        let renderer = thinking_then_silent(0.5);
        let live = live_text(&renderer);
        assert!(
            live.contains(&t("thinking", "思考中")),
            "半秒的停顿不该收:\n{live}"
        );
        assert!(renderer.reasoning_started_at.is_some());
        crate::render::set_cols_override(0);
    });
}

/// 收进来的时刻比思考结束晚了一截空档；`Worked for` 要从思考开始算，那截空档
/// 也是这一轮在干活（模型在写参数），不能漏。
#[test]
fn the_silent_gap_still_counts_toward_worked_for() {
    with_blocks(|| {
        crate::render::set_cols_override(100);
        let mut renderer = thinking_then_silent(2.0);
        renderer
            .write_tool_call("read", r#"{"path":"/tmp/a"}"#)
            .unwrap();
        renderer.write_tool_result("read", true, "ok").unwrap();
        renderer
            .write_chunk(ChatStreamChunk {
                kind: ChatStreamKind::Content,
                text: "改好了。\n".to_string(),
            })
            .unwrap();
        let out = strip_ansi_for_test(&String::from_utf8_lossy(&renderer.take_output_frame()));
        let seconds: f64 = out
            .split("Worked for ")
            .nth(1)
            .and_then(|rest| rest.split('s').next())
            .and_then(|number| number.trim().parse().ok())
            .unwrap_or_else(|| panic!("没有收缩行:\n{out}"));
        assert!(
            seconds >= 2.9,
            "空档没算进 Worked for（{seconds}s）:\n{out}"
        );
        crate::render::set_cols_override(0);
    });
}
