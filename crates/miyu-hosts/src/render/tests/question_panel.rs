//! 提问面板开着时转轮怎么让（09-25 红绿账 `panel_keeps_body` 抓到的回归）。
//!
//! 会话项目第 3 段把提问面板改成活动区上的层，回合循环照转；为了不让「准备问题」挂在
//! 面板上头一直动，开面板时把整块转轮收掉了——而全屏下时间线那几行（「已思考 · …」）
//! 就画在这一块里，跟着一起没了，退回了 09-20 修过的「面板一开，刚才的过程就没了」。
//! 现在的做法：清掉「准备问题」、重画一次、冻住转轮，面板收掉再接着动。

use crate::render::*;

fn fullscreen_renderer() -> StreamRenderer {
    let mut renderer = StreamRenderer::new(
        ReasoningDisplayMode::Summary,
        ToolCallDisplayMode::Summary,
        false,
        true,
        8,
    );
    renderer.use_external_cursor_control();
    renderer.use_buffered_output();
    renderer.live_summary = true;
    renderer
}

fn tick_after_interval(renderer: &mut StreamRenderer) -> Vec<u8> {
    let _ = renderer.take_output_frame();
    std::thread::sleep(SPINNER_INTERVAL + std::time::Duration::from_millis(10));
    renderer.tick_spinner().unwrap();
    renderer.take_output_frame()
}

#[test]
fn the_question_panel_freezes_the_spinner_instead_of_erasing_it() {
    crate::render::set_cols_override(120);
    let mut renderer = fullscreen_renderer();
    renderer
        .write_chunk(ChatStreamChunk {
            kind: ChatStreamKind::Reasoning,
            text: "想一下该问什么。".to_string(),
        })
        .unwrap();
    renderer.start_preparing_question().unwrap();
    renderer.tick_spinner().unwrap();
    assert!(renderer.wait_spinner.is_some(), "准备问题时转轮在跑");

    renderer.prepare_for_panel().unwrap();

    // 转轮那块还在：全屏下「已思考」那几行就画在里面，收掉就一起没了。
    assert!(renderer.wait_spinner.is_some(), "开面板不能把转轮那块收掉");
    // 「准备问题」那一行清掉了（④）。
    assert!(renderer.preparing_question_started_at.is_none());
    assert!(!renderer.waiting_phase_text().contains("准备问题"));
    // 面板开着：她在等人回答，画面不动。
    assert!(
        tick_after_interval(&mut renderer).is_empty(),
        "面板开着时转轮不该再出帧"
    );
    // 面板收掉：接着动。
    renderer.resume_after_panel();
    assert!(!tick_after_interval(&mut renderer).is_empty());
    crate::render::set_cols_override(0);
}

/// 答完之后 `start_waiting` 也要解冻：转轮还在（没被收过），它会早退，冻结标记得先清。
#[test]
fn start_waiting_after_an_answer_unfreezes_the_spinner() {
    crate::render::set_cols_override(120);
    let mut renderer = fullscreen_renderer();
    renderer.start_waiting().unwrap();
    renderer.prepare_for_panel().unwrap();
    assert!(tick_after_interval(&mut renderer).is_empty());
    renderer.start_waiting().unwrap();
    assert!(!tick_after_interval(&mut renderer).is_empty());
    crate::render::set_cols_override(0);
}
