//! 补发的事件按它自己发生的时刻掐表（会话项目第 3 段收尾，09-25）。
//!
//! 挂上来的终端从头补一轮时，事件是一口气喂进来的：原来按收到的那一刻掐表，做完的步骤全记
//! 成「<1ms」，回合中回到主会话时子代理那一步的秒数从补发那一刻重算（13s 变 1.4s）。daemon
//! 现在给每个事件记下发生的时刻，终端喂事件之前把它设成渲染器的事件时钟。

use super::timeline::{block_id_in, timeline_renderer, with_blocks};
use std::time::{Duration, Instant};

fn stats<'a>(
    renderer: &'a crate::render::StreamRenderer,
    name: &str,
) -> &'a crate::render::tool_display::ToolStats {
    renderer
        .tool_stats
        .iter()
        .find(|(key, _)| key.starts_with(name))
        .map(|(_, stats)| stats)
        .unwrap_or_else(|| panic!("没有 {name} 的统计：{:?}", renderer.tool_stats.keys()))
}

/// 补发时调用和结果是一口气喂进来的：跑完那一步报的是两个事件的时刻之差，不是「<1ms」。
/// 时钟停在最后那个事件上（终端就是这么喂的：一个事件设一次，不清）。
fn replayed_frame(name: &str, arguments: &str) -> String {
    with_blocks(|| {
        let mut renderer = timeline_renderer();
        renderer.use_external_cursor_control();
        renderer.use_buffered_output();
        let sent = Instant::now() - Duration::from_secs(30);
        renderer.set_event_clock(Some(sent));
        renderer.write_tool_call(name, arguments).unwrap();
        renderer.set_event_clock(Some(sent + Duration::from_secs(2)));
        renderer.write_tool_result(name, true, "done").unwrap();
        renderer.finish().unwrap();
        with_expanded(String::from_utf8_lossy(&renderer.take_output_frame()).to_string())
    })
}

/// 帧，连同它挂着的块点开之后的样子：收缩行上不挂耗时（09-26），每一步自己的秒数在块里。
fn with_expanded(frame: String) -> String {
    let expanded: Vec<String> = frame
        .lines()
        .filter_map(block_id_in)
        .filter_map(crate::render::blocks::get)
        .flatten()
        .collect();
    format!("{frame}\n{}", expanded.join("\n"))
}

#[test]
fn a_replayed_tool_keeps_its_own_duration() {
    let frame = replayed_frame("read", r#"{"path":"a.txt"}"#);
    assert!(
        frame.contains("2.0s") && !frame.contains("<1ms"),
        "补发的这一步花了 2 秒: {frame}"
    );
}

#[test]
fn a_replayed_command_keeps_its_own_duration() {
    let frame = replayed_frame("run_command", r#"{"command":"sleep 2"}"#);
    assert!(
        frame.contains("2.0s") && !frame.contains("<1ms"),
        "补发的命令花了 2 秒: {frame}"
    );
}

/// 回合中回到主会话：子代理还在跑，那一步的读数从它派出去那一刻算，不从补发那一刻算。
#[test]
fn a_running_subagent_counts_from_when_it_was_sent() {
    with_blocks(|| {
        let mut renderer = timeline_renderer();
        renderer.set_event_clock(Some(Instant::now() - Duration::from_secs(13)));
        renderer
            .write_tool_call("subagent", r#"{"description":"查一下","prompt":"查一下"}"#)
            .unwrap();
        renderer.set_event_clock(None);

        let started = stats(&renderer, "subagent")
            .started_at
            .expect("派出去就开始掐表");
        assert!(started.elapsed() >= Duration::from_secs(13));
    })
}

/// 没有事件时钟（直连模式、老 daemon）就照旧按收到的那一刻。
#[test]
fn without_a_clock_the_moment_of_arrival_counts() {
    with_blocks(|| {
        let mut renderer = timeline_renderer();
        renderer
            .write_tool_call("read", r#"{"path":"a.txt"}"#)
            .unwrap();
        let started = stats(&renderer, "read").started_at.unwrap();
        assert!(started.elapsed() < Duration::from_secs(1));
    })
}

/// 思考没有「分段结束」事件（OpenAI 兼容线都这样）时，是下一件事来了才收：正文的第一段
/// 到的那一刻。补发时那一刻是它自己的时刻。
#[test]
fn a_replayed_thought_keeps_its_own_duration() {
    let frame = with_blocks(|| {
        let mut renderer = timeline_renderer();
        renderer.use_external_cursor_control();
        renderer.use_buffered_output();
        let began = Instant::now() - Duration::from_secs(30);
        renderer.set_event_clock(Some(began));
        renderer.start_reasoning_phase(began).unwrap();
        renderer
            .write_chunk(miyu_core::llm::ChatStreamChunk {
                kind: miyu_core::llm::ChatStreamKind::Reasoning,
                text: "先想一想".into(),
            })
            .unwrap();
        renderer.set_event_clock(Some(began + Duration::from_secs(3)));
        renderer
            .write_tool_call("run_command", r#"{"command":"ls"}"#)
            .unwrap();
        renderer
            .write_tool_result("run_command", true, "out")
            .unwrap();
        renderer.finish().unwrap();
        with_expanded(String::from_utf8_lossy(&renderer.take_output_frame()).to_string())
    });
    assert!(
        frame.contains("3.0s") && !frame.contains("30s"),
        "补发的思考想了 3 秒: {frame}"
    );
}
