//! 回合进行中活动区每一拍的成本量尺：一轮里跑的步数越多，渲染器拼一帧、缓冲吃一帧
//! 各花多久（09-23：60 步的长回合 debug 下 CPU 从 5% 一路涨到 16%）。
//!
//! 不是断言，是尺子。用户跑的是 debug 二进制，按 debug 量才是他手上的感觉：
//!
//!     cargo test -p miyu --lib -- cli::tests::tui_live_cost --ignored --nocapture

use super::tui_blocks::with_blocks;
use crate::cli::repl::tail::screen::term::Term;
use miyu_hosts::render::{
    set_cols_override, ReasoningDisplayMode, StreamRenderer, ToolCallDisplayMode,
};
use std::time::{Duration, Instant};

#[test]
#[ignore]
fn live_tick_cost_by_step_count() {
    with_blocks(|| {
        set_cols_override(160);
        let mut renderer = StreamRenderer::new(
            ReasoningDisplayMode::Summary,
            ToolCallDisplayMode::Summary,
            false,
            true,
            8,
        );
        renderer.use_external_cursor_control();
        renderer.use_buffered_output();
        renderer.use_terminal_surface();
        let mut term = Term::default();
        term.set_content_cols(156);
        term.set_cols(160);
        let arguments = r#"{"command":"printf 'a\\nb\\nc\\n'","title":"跑一条命令看看"}"#;
        let mut steps = 0;
        for target in [10, 50, 100, 150] {
            while steps < target {
                renderer.write_tool_call("run_command", arguments).unwrap();
                renderer
                    .write_tool_result("run_command", true, "a\nb\nc\n")
                    .unwrap();
                term.feed(&renderer.take_output_frame());
                steps += 1;
            }
            let (mut build, mut feed, mut bytes) = (Duration::ZERO, Duration::ZERO, 0);
            let ticks = 10;
            for _ in 0..ticks {
                // 转轮自己按 33ms 节流：等过这一拍它才真的出帧。
                std::thread::sleep(Duration::from_millis(35));
                let started = Instant::now();
                renderer.tick_spinner().unwrap();
                let frame = renderer.take_output_frame();
                let built = Instant::now();
                term.feed(&frame);
                build += built - started;
                feed += built.elapsed();
                bytes += frame.len();
            }
            eprintln!(
                "steps={target:>4}  拼帧 {:>6.2}ms  缓冲吃帧 {:>6.2}ms  每帧 {:>6}B  缓冲 {} 行",
                build.as_secs_f64() * 1000.0 / f64::from(ticks),
                feed.as_secs_f64() * 1000.0 / f64::from(ticks),
                bytes / ticks as usize,
                term.line_count(),
            );
        }
        set_cols_override(0);
    });
}
