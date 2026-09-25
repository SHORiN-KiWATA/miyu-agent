//! 正文的活尾巴（09-25）：没收到换行的半行、没闭合的代码块，流着的时候就露在活动区里，
//! 写完了照常落进正文——只落一份，活动区不留残影。
//!
//! 帧喂进 `Term`（全屏那台终端模拟器）再读屏幕：活动区是「上移 → 清行 → 重画」或锚点
//! 重写，直接按 `\n` 切字节流看到的是错的。静态面（S3：shellhook、inline、单次、回写）和
//! 全屏（S4）各走一遍。

use crate::cli::repl::tail::screen::term::Term;
use crate::cli::tests::tui_blocks::with_blocks;
use miyu_core::llm::{ChatStreamChunk, ChatStreamKind};
use miyu_hosts::render::{
    set_cols_override, ReasoningDisplayMode, StreamRenderer, ToolCallDisplayMode,
};

/// 静态面或全屏的渲染器。写线程报了宽度，活动区才认自己是在往终端画。
fn renderer() -> StreamRenderer {
    let mut renderer = StreamRenderer::new(
        ReasoningDisplayMode::Summary,
        ToolCallDisplayMode::Summary,
        false,
        true,
        4,
    );
    renderer.use_external_cursor_control();
    renderer.use_buffered_output();
    renderer.use_terminal_surface();
    renderer
}

struct Screen {
    term: Term,
}

impl Screen {
    fn new() -> Self {
        let mut term = Term::default();
        term.set_cols(100);
        Self { term }
    }

    fn feed(&mut self, renderer: &mut StreamRenderer) {
        let frame = renderer.take_output_frame();
        self.term.feed(&frame);
    }

    fn lines(&self) -> Vec<String> {
        (0..self.term.line_count())
            .map(|index| {
                self.term
                    .row_spans(index)
                    .into_iter()
                    .map(|span| span.text)
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    fn count(&self, needle: &str) -> usize {
        self.lines()
            .iter()
            .filter(|line| line.contains(needle))
            .count()
    }
}

fn content(renderer: &mut StreamRenderer, screen: &mut Screen, text: &str) {
    renderer
        .write_chunk(ChatStreamChunk {
            kind: ChatStreamKind::Content,
            text: text.to_string(),
        })
        .unwrap();
    screen.feed(renderer);
}

/// 过一拍再走一帧：尾巴只露搁了至少一拍还没落下的字。
fn settle(renderer: &mut StreamRenderer, screen: &mut Screen) {
    std::thread::sleep(std::time::Duration::from_millis(45));
    renderer.tick_spinner().unwrap();
    screen.feed(renderer);
}

/// 两个面各跑一遍（全屏的块开关是线程局部的）。
fn on_both_surfaces(body: impl Fn(&str)) {
    set_cols_override(100);
    body("static");
    with_blocks(|| body("full-screen"));
    set_cols_override(0);
}

#[test]
fn a_half_line_shows_up_before_its_newline_and_lands_once() {
    on_both_surfaces(|surface| {
        let mut renderer = renderer();
        let mut screen = Screen::new();
        content(&mut renderer, &mut screen, "Hello wor");
        assert_eq!(screen.count("Hello wor"), 0, "{surface}: 还没到一拍不露");
        settle(&mut renderer, &mut screen);
        assert_eq!(
            screen.count("Hello wor"),
            1,
            "{surface}: 半行该露出来: {:?}",
            screen.lines()
        );
        content(&mut renderer, &mut screen, "ld, all done.\nNext li");
        settle(&mut renderer, &mut screen);
        renderer.finish().unwrap();
        screen.feed(&mut renderer);
        let lines = screen.lines();
        assert_eq!(
            screen.count("Hello world, all done."),
            1,
            "{surface}: {lines:?}"
        );
        assert_eq!(
            lines
                .iter()
                .filter(|line| line.trim() == "Hello wor")
                .count(),
            0,
            "{surface}: 活动区留了残影: {lines:?}"
        );
        assert_eq!(screen.count("Next li"), 1, "{surface}: {lines:?}");
    });
}

#[test]
fn an_open_code_block_streams_in_its_final_frame_and_lands_once() {
    on_both_surfaces(|surface| {
        let mut renderer = renderer();
        let mut screen = Screen::new();
        content(
            &mut renderer,
            &mut screen,
            "Before.\n```rust\nfn main() {\n    let answer = 42;\n",
        );
        settle(&mut renderer, &mut screen);
        let lines = screen.lines();
        assert_eq!(screen.count("code rust"), 1, "{surface}: {lines:?}");
        assert_eq!(screen.count("let answer = 42;"), 1, "{surface}: {lines:?}");
        content(&mut renderer, &mut screen, "    println!(\"{answer}\");\n");
        settle(&mut renderer, &mut screen);
        assert_eq!(
            screen.count("println!"),
            1,
            "{surface}: {:?}",
            screen.lines()
        );
        content(&mut renderer, &mut screen, "}\n```\nAfter.\n");
        renderer.finish().unwrap();
        screen.feed(&mut renderer);
        let lines = screen.lines();
        for needle in [
            "Before.",
            "code rust",
            "fn main() {",
            "let answer = 42;",
            "println!",
            "After.",
        ] {
            assert_eq!(screen.count(needle), 1, "{surface}: {needle}: {lines:?}");
        }
    });
}

#[test]
fn a_long_code_block_shows_only_its_last_rows() {
    on_both_surfaces(|surface| {
        let mut renderer = renderer();
        renderer.thinking_scroll_lines = 5;
        let mut screen = Screen::new();
        let body = (1..=30)
            .map(|index| format!("line_{index:02}\n"))
            .collect::<String>();
        content(&mut renderer, &mut screen, &format!("```\n{body}"));
        settle(&mut renderer, &mut screen);
        let visible = screen
            .lines()
            .iter()
            .filter(|line| line.contains("line_"))
            .count();
        assert!(
            (1..5).contains(&visible),
            "{surface}: 最多露 5 行（含底框）: {:?}",
            screen.lines()
        );
        assert_eq!(screen.count("line_30"), 1, "{surface}: 露的是最后几行");
        assert_eq!(screen.count("line_01"), 0, "{surface}: 前面的不露");
        content(&mut renderer, &mut screen, "```\n");
        renderer.finish().unwrap();
        screen.feed(&mut renderer);
        for index in 1..=30 {
            let needle = format!("line_{index:02}");
            assert_eq!(screen.count(&needle), 1, "{surface}: {needle}");
        }
    });
}

#[test]
fn a_tool_call_mid_line_lands_the_half_line_once() {
    on_both_surfaces(|surface| {
        let mut renderer = renderer();
        let mut screen = Screen::new();
        content(&mut renderer, &mut screen, "Let me look it up");
        settle(&mut renderer, &mut screen);
        assert_eq!(screen.count("Let me look it up"), 1, "{surface}");
        renderer
            .write_tool_call("web_search", r#"{"query":"x"}"#)
            .unwrap();
        screen.feed(&mut renderer);
        settle(&mut renderer, &mut screen);
        renderer.finish().unwrap();
        screen.feed(&mut renderer);
        assert_eq!(
            screen.count("Let me look it up"),
            1,
            "{surface}: {:?}",
            screen.lines()
        );
    });
}

/// 流得快、一拍之内就写完的行照旧直接落下：和从来不 tick 的时候一个字节都不差。
#[test]
fn a_line_finished_within_one_frame_never_passes_through_the_tail() {
    set_cols_override(100);
    let run = |tick: bool| {
        let mut renderer = renderer();
        for piece in ["quick ", "line\n", "and another\n"] {
            renderer
                .write_chunk(ChatStreamChunk {
                    kind: ChatStreamKind::Content,
                    text: piece.to_string(),
                })
                .unwrap();
            if tick {
                renderer.tick_spinner().unwrap();
            }
        }
        renderer.finish().unwrap();
        renderer.take_output_frame()
    };
    let ticked = run(true);
    let plain = run(false);
    set_cols_override(0);
    assert_eq!(
        String::from_utf8_lossy(&ticked),
        String::from_utf8_lossy(&plain)
    );
}

/// 管道那一面没有活动区：半行搁多久也不露。
#[test]
fn a_piped_surface_has_no_tail() {
    let mut renderer = renderer();
    renderer.use_piped_surface();
    let mut screen = Screen::new();
    content(&mut renderer, &mut screen, "half a line");
    settle(&mut renderer, &mut screen);
    assert_eq!(screen.count("half a line"), 0, "{:?}", screen.lines());
    renderer.finish().unwrap();
    screen.feed(&mut renderer);
    assert_eq!(screen.count("half a line"), 1);
}
