//! 被拒请求的**形状**摘要：HTTP 报错时留下足以定位的结构，但不留正文。
//!
//! 09-22 撞到的那条 422 就是这个缺口的样本：
//!
//! ```text
//! opencodego / deepseek-v4.1-flash（key#1）：HTTP 422
//! Upstream request failed: [invalid_request_error] Input should be a valid string
//! ```
//!
//! 中转把上游的字段路径吞掉了，只剩一句「应该是个字符串」；那一轮失败后
//! 整个回滚，`turns` 表里一行都不剩，附件、队列全空——事后除了这三行报错
//! 什么都查不到。完整请求录制（`request_log`，`MIYU_LOG_REQUESTS=1`）默认
//! 关着，而这种事恰恰是偶发的，等不到下一次。
//!
//! 所以按角色和 content 形态数一遍，连同工具调用的配平一起打进
//! `tracing::error`——daemon 默认级别就记 error，不用任何开关、不额外落文件。
//! 「哪个字段该是 string 却不是」由这份摘要指出方向：`content_parts`
//! （多模态数组）、`content_null`（只有 tool_calls 的 assistant）、
//! `calls`/`results` 不配平，各对应一类已知的上游拒收。

use crate::llm::{ChatContent, ChatMessage, ToolDefinition};

/// 一行紧凑的形状摘要，直接进日志字段。
pub(crate) fn summarize(messages: &[ChatMessage], tools: &[ToolDefinition]) -> String {
    let mut system = 0usize;
    let mut user = 0usize;
    let mut assistant = 0usize;
    let mut tool = 0usize;
    let mut other = 0usize;
    let mut content_null = 0usize;
    let mut content_text = 0usize;
    let mut content_parts = 0usize;
    let mut calls = 0usize;
    let mut results = 0usize;
    let mut reasoning = 0usize;
    let mut empty_text = 0usize;
    for message in messages {
        match message.role.as_str() {
            "system" => system += 1,
            "user" => user += 1,
            "assistant" => assistant += 1,
            "tool" => tool += 1,
            _ => other += 1,
        }
        match &message.content {
            None => content_null += 1,
            Some(ChatContent::Text(text)) => {
                content_text += 1;
                if text.is_empty() {
                    empty_text += 1;
                }
            }
            Some(ChatContent::Parts(_)) => content_parts += 1,
        }
        calls += message
            .tool_calls
            .as_ref()
            .map(|list| list.len())
            .unwrap_or(0);
        if message.role == "tool" {
            results += 1;
        }
        if message.reasoning_content.is_some() {
            reasoning += 1;
        }
    }
    // 尾巴的角色序列：上游拒收多半跟最后几条的形态有关（工具结果没跟上、
    // 多模态块收尾之类），列出来比只给总数好读。
    let tail = messages
        .iter()
        .rev()
        .take(6)
        .map(|message| message.role.as_str())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "msgs={} sys={system} user={user} asst={assistant} tool={tool} other={other} \
         content_text={content_text} content_parts={content_parts} content_null={content_null} \
         empty_text={empty_text} reasoning={reasoning} calls={calls} results={results} \
         balanced={} tools={} tail=[{tail}]",
        messages.len(),
        calls == results,
        tools.len(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{ChatContentPart, ToolCall, ToolCallFunction};

    #[test]
    fn a_multimodal_message_shows_up_as_parts_not_text() {
        let messages = vec![
            ChatMessage::system("s"),
            ChatMessage::user_with_image("look", "data:image/png;base64,AA"),
        ];
        let summary = summarize(&messages, &[]);
        assert!(summary.contains("content_parts=1"), "{summary}");
        assert!(summary.contains("content_text=1"), "{summary}");
        assert!(summary.contains("tail=[system,user]"), "{summary}");
    }

    /// 工具调用没配平正是一类已知的上游硬拒（DeepSeek 400「tool_calls 后
    /// 必须跟每个 tool 结果」）。摘要要能一眼看出来。
    #[test]
    fn an_unbalanced_tool_round_reports_balanced_false() {
        let call = ToolCall {
            id: "call_1".into(),
            kind: "function".into(),
            function: ToolCallFunction {
                name: "read".into(),
                arguments: "{}".into(),
            },
        };
        let messages = vec![
            ChatMessage::system("s"),
            ChatMessage::assistant("", Some(vec![call])),
        ];
        let summary = summarize(&messages, &[]);
        assert!(summary.contains("calls=1"), "{summary}");
        assert!(summary.contains("results=0"), "{summary}");
        assert!(summary.contains("balanced=false"), "{summary}");
    }

    #[test]
    fn a_balanced_round_reports_balanced_true() {
        let call = ToolCall {
            id: "call_1".into(),
            kind: "function".into(),
            function: ToolCallFunction {
                name: "read".into(),
                arguments: "{}".into(),
            },
        };
        let messages = vec![
            ChatMessage::assistant("", Some(vec![call])),
            ChatMessage::tool("call_1", "ok"),
        ];
        let summary = summarize(&messages, &[]);
        assert!(summary.contains("balanced=true"), "{summary}");
    }

    #[test]
    fn parts_only_messages_are_counted_without_leaking_their_text() {
        let messages = vec![ChatMessage::user_parts(vec![ChatContentPart::Text {
            text: "secret".into(),
        }])];
        let summary = summarize(&messages, &[]);
        assert!(!summary.contains("secret"), "{summary}");
        assert!(summary.contains("content_parts=1"), "{summary}");
    }
}
