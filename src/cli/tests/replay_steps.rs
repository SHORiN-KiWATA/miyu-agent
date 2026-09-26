//! 回放库里的轮要和实时那一轮画成一个样子（09-26）。只后台之后从子会话切回主会话必碰上——主会话
//! 那一轮已经跑完，改按库里的流水画：
//! - 提问：一问一答照实时那样，那一步记成「已回答」、问答写出来，不算出错（原来回放里提问只是一对
//!   静默的工具调用 / 结果，答案没人用）；
//! - 编辑：那一步点开是 diff（原来是结果那团 JSON——实时的 diff 走侧信道，流水里没有）。

use crate::cli::*;
use miyu_base::question::{answered_tool_output, QuestionExchange, QuestionRequest};
use miyu_core::state::{ReplayEntry, TurnReplay};

fn asked_turn(output: String) -> TurnReplay {
    let arguments = serde_json::json!({"questions": [{
        "header": "走查",
        "question": "走查用的问题：选一个",
        "options": [
            {"label": "甲选项", "description": "第一个"},
            {"label": "乙选项", "description": "第二个"},
        ],
    }]})
    .to_string();
    TurnReplay {
        display_content: "走查一句".to_string(),
        entries: vec![
            ReplayEntry::ToolCall {
                name: "ask_question".to_string(),
                arguments: arguments.clone(),
            },
            // 面板开着的那段时间也记在这一步的用时里（真流水就是这样，0 的话碰不到下面那个坑）。
            ReplayEntry::ToolResult {
                name: "ask_question".to_string(),
                ok: true,
                output,
                elapsed_ms: 707,
            },
            ReplayEntry::Text {
                text: "好的，收到。".to_string(),
            },
        ],
        turn_id: "turn_replayed_question".to_string(),
        ..Default::default()
    }
}

fn answered_output() -> String {
    let arguments = serde_json::json!({"questions": [{
        "header": "走查",
        "question": "走查用的问题：选一个",
        "options": [
            {"label": "甲选项", "description": "第一个"},
            {"label": "乙选项", "description": "第二个"},
        ],
    }]})
    .to_string();
    let request = QuestionRequest::parse(&arguments).unwrap();
    answered_tool_output(&QuestionExchange::new(request, vec![vec!["甲选项".to_string()]]).unwrap())
}

/// 全屏下回放（切回主会话、重开终端界面走的都是这一条）。
fn replay(turn: TurnReplay) -> String {
    let config = AppConfig::default();
    super::tui_blocks::with_blocks(|| {
        let frame =
            session_replay_frame(&[turn], PersonaLane::Active, &config, 100, false).unwrap();
        miyu_hosts::render::strip_ansi_text(&String::from_utf8_lossy(&frame))
    })
}

#[test]
fn a_replayed_question_is_drawn_as_answered() {
    let text = replay(asked_turn(answered_output()));
    assert!(text.contains("走查：甲选项"), "{text}");
    assert!(text.contains(t("Answered", "已回答")), "{text}");
    // 收起来那一行不把它算成出错，也不多出一个「已中断」的提问步。
    assert!(!text.contains(t("err", "出错")), "{text}");
    assert!(!text.contains(t("interrupted", "已中断")), "{text}");
}

fn edited_turn() -> TurnReplay {
    let arguments = serde_json::json!({
        "patchText": "*** Begin Patch\n*** Add File: /tmp/walk.txt\n+走查用的第一行\n+走查用的第二行\n*** End Patch\n",
    })
    .to_string();
    TurnReplay {
        display_content: "改一个文件".to_string(),
        entries: vec![
            ReplayEntry::ToolCall {
                name: "edit".to_string(),
                arguments,
            },
            ReplayEntry::ToolResult {
                name: "edit".to_string(),
                ok: true,
                output: serde_json::json!({
                    "files": [{"operation": "add", "path": "/tmp/walk.txt"}],
                    "files_changed": 1,
                    "ok": true,
                    "operation": "apply_patch",
                })
                .to_string(),
                elapsed_ms: 2,
            },
            ReplayEntry::Text {
                text: "改好了。".to_string(),
            },
        ],
        turn_id: "turn_replayed_edit".to_string(),
        ..Default::default()
    }
}

/// 一帧里点得开的内容全摊平：块里套块，点开一层才露出下一层的块号。
fn expandable_lines(frame: &str) -> Vec<String> {
    fn block_ids(text: &str) -> Vec<u64> {
        text.match_indices("miyu-block=")
            .filter_map(|(at, marker)| {
                text[at + marker.len()..]
                    .chars()
                    .take_while(char::is_ascii_digit)
                    .collect::<String>()
                    .parse()
                    .ok()
            })
            .collect()
    }
    let mut pending = block_ids(frame);
    let mut seen = std::collections::HashSet::new();
    let mut lines = Vec::new();
    while let Some(id) = pending.pop() {
        if !seen.insert(id) {
            continue;
        }
        for line in miyu_hosts::render::blocks::get(id).unwrap_or_default() {
            pending.extend(block_ids(&line));
            lines.push(miyu_hosts::render::strip_ansi_text(&line));
        }
    }
    lines
}

#[test]
fn a_replayed_edit_expands_to_its_diff() {
    let config = AppConfig::default();
    let lines = super::tui_blocks::with_blocks(|| {
        let frame =
            session_replay_frame(&[edited_turn()], PersonaLane::Active, &config, 100, false)
                .unwrap();
        expandable_lines(&String::from_utf8_lossy(&frame))
    });
    assert!(
        lines.iter().any(|line| line.contains("走查用的第一行")),
        "{lines:#?}"
    );
    assert!(
        !lines.iter().any(|line| line.contains("files_changed")),
        "{lines:#?}"
    );
    // 路径只在那一步的抬头上：diff 里不再带「已新建  路径」那种文件头。
    assert!(
        lines
            .iter()
            .filter(|line| line.contains("/tmp/walk.txt"))
            .all(|line| line.contains(t("Edit file", "编辑文件"))),
        "{lines:#?}"
    );
    // 新建的文件行号从 1 起。
    assert!(
        lines
            .iter()
            .any(|line| line.trim_start().starts_with("1 + 走查用的第一行")),
        "{lines:#?}"
    );
}

fn command_turn(command: &str, output: &str) -> TurnReplay {
    TurnReplay {
        display_content: "跑一条命令".to_string(),
        entries: vec![
            ReplayEntry::ToolCall {
                name: "run_command".to_string(),
                arguments: serde_json::json!({"command": command}).to_string(),
            },
            ReplayEntry::ToolResult {
                name: "run_command".to_string(),
                ok: true,
                output: output.to_string(),
                elapsed_ms: 18,
            },
            ReplayEntry::Text {
                text: "跑完了。".to_string(),
            },
        ],
        turn_id: "turn_replayed_command".to_string(),
        ..Default::default()
    }
}

fn replayed_expandable(turn: TurnReplay) -> Vec<String> {
    let config = AppConfig::default();
    super::tui_blocks::with_blocks(|| {
        let frame =
            session_replay_frame(&[turn], PersonaLane::Active, &config, 100, false).unwrap();
        expandable_lines(&String::from_utf8_lossy(&frame))
    })
}

#[test]
fn a_replayed_command_expands_to_its_output() {
    let lines = replayed_expandable(command_turn(
        "printf '走查用的命令输出\\n第二行\\n'",
        "走查用的命令输出\n第二行",
    ));
    // 输出是自己的一行（命令那一行里也印着这几个字，所以按整行认）。
    assert!(
        lines.iter().any(|line| line.trim() == "走查用的命令输出"),
        "{lines:#?}"
    );
    assert!(
        lines.iter().any(|line| line.trim() == "第二行"),
        "{lines:#?}"
    );

    let failed = replayed_expandable(command_turn(
        "printf '走查用的报错\\n' >&2; exit 3",
        "[stderr]\n走查用的报错\n[exit code: 3]",
    ));
    assert!(
        failed.iter().any(|line| line.trim() == "走查用的报错"),
        "{failed:#?}"
    );
    assert!(
        !failed.iter().any(|line| line.contains("[stderr]")),
        "{failed:#?}"
    );
}
