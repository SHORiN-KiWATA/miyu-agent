//! Anthropic 协议的流式、思考块与工具。

use super::shared::*;
use crate::llm::openai_compatible::*;
use crate::llm::{ChatContent, ChatContentPart, ImageUrlContent};

#[test]
fn auto_protocol_uses_anthropic_for_official_provider() {
    let provider = test_provider("anthropic", "https://api.anthropic.com/v1");
    let client = test_client(provider);

    assert!(client.uses_anthropic_messages());
}

#[test]
fn auto_protocol_keeps_openai_compatible_claude_proxy() {
    let mut provider = test_provider("openrouter", "https://openrouter.ai/api/v1");
    provider.default_model = "anthropic/claude-sonnet-4-5".to_string();
    let client = test_client(provider);

    assert!(!client.uses_anthropic_messages());
}

#[test]
fn protocol_config_accepts_explicit_anthropic() {
    let mut provider = test_provider("anthropic", "https://api.anthropic.com/v1");
    provider.protocol = "anthropic".to_string();

    assert_eq!(
        ProviderProtocol::from_provider(&provider).unwrap(),
        ProviderProtocol::Anthropic
    );
}

#[test]
fn protocol_config_accepts_anthropic_aliases() {
    let mut provider = test_provider("anthropic", "https://api.anthropic.com/v1");

    for protocol in ["anthropic-messages", "claude", "claude-messages"] {
        provider.protocol = protocol.to_string();
        assert_eq!(
            ProviderProtocol::from_provider(&provider).unwrap(),
            ProviderProtocol::Anthropic
        );
    }
}

#[test]
fn anthropic_lowering_keeps_remote_image_urls() {
    let content = lower_anthropic_user_content(Some(ChatContent::Parts(vec![
        ChatContentPart::ImageUrl {
            image_url: ImageUrlContent {
                url: "https://example.com/image.png".to_string(),
            },
        },
        ChatContentPart::Text {
            text: "describe".to_string(),
        },
    ])));
    let json = serde_json::to_value(content).unwrap();

    assert_eq!(json[0]["type"], "image");
    assert_eq!(json[0]["source"]["type"], "url");
    assert_eq!(json[0]["source"]["url"], "https://example.com/image.png");
    assert_eq!(json[1]["text"], "describe");
}

/// PDF 下放到 Anthropic:`document` 块 + base64 source,而且要摆在文本块
/// **之前**(官方规格)。Miyu 组装 parts 时正文在最前,所以这里必须前置。
#[test]
fn anthropic_lowering_puts_pdf_documents_before_text() {
    let content = lower_anthropic_user_content(Some(ChatContent::Parts(vec![
        ChatContentPart::Text {
            text: "总结这份文档".to_string(),
        },
        ChatContentPart::File {
            file: crate::llm::FileContent {
                filename: "report.pdf".to_string(),
                file_data: "data:application/pdf;base64,JVBERi0xLjQK".to_string(),
            },
        },
    ])));
    let json = serde_json::to_value(content).unwrap();

    assert_eq!(json[0]["type"], "document");
    assert_eq!(json[0]["source"]["type"], "base64");
    assert_eq!(json[0]["source"]["media_type"], "application/pdf");
    assert_eq!(json[0]["source"]["data"], "JVBERi0xLjQK");
    assert_eq!(json[1]["type"], "text");
    assert_eq!(json[1]["text"], "总结这份文档");
}

/// 前置是**稳定**分区:同类块之间的原有次序不能被打乱。这一层是字节纯度的
/// 一部分,排序不确定就等于每轮换一份前缀、缓存全废。
#[test]
fn anthropic_lowering_pdf_reorder_is_stable() {
    let pdf = |name: &str, data: &str| ChatContentPart::File {
        file: crate::llm::FileContent {
            filename: name.to_string(),
            file_data: format!("data:application/pdf;base64,{data}"),
        },
    };
    let content = lower_anthropic_user_content(Some(ChatContent::Parts(vec![
        ChatContentPart::Text {
            text: "one".to_string(),
        },
        pdf("a.pdf", "QQ=="),
        ChatContentPart::Text {
            text: "two".to_string(),
        },
        pdf("b.pdf", "Qg=="),
    ])));
    let json = serde_json::to_value(content).unwrap();

    assert_eq!(json[0]["source"]["data"], "QQ==");
    assert_eq!(json[1]["source"]["data"], "Qg==");
    assert_eq!(json[2]["text"], "one");
    assert_eq!(json[3]["text"], "two");
}

/// openai-chat 那条线是把 `ChatMessage` 直接 serde 出去的,所以变体的线格式
/// **就是** OpenAI 的 file 块。改名字就是发错请求,这个断言是那道闸。
#[test]
fn openai_chat_pdf_part_serializes_as_file_block() {
    let json = serde_json::to_value(ChatContentPart::File {
        file: crate::llm::FileContent {
            filename: "report.pdf".to_string(),
            file_data: "data:application/pdf;base64,JVBERi0xLjQK".to_string(),
        },
    })
    .unwrap();

    assert_eq!(json["type"], "file");
    assert_eq!(json["file"]["filename"], "report.pdf");
    assert_eq!(
        json["file"]["file_data"],
        "data:application/pdf;base64,JVBERi0xLjQK"
    );
}

/// Responses 用的是另一套字段名(`input_file`/`file_data`),照搬 chat 的形状
/// 会被拒。
#[test]
fn responses_pdf_part_uses_input_file() {
    let content =
        lower_responses_user_content(Some(ChatContent::Parts(vec![ChatContentPart::File {
            file: crate::llm::FileContent {
                filename: "report.pdf".to_string(),
                file_data: "data:application/pdf;base64,JVBERi0xLjQK".to_string(),
            },
        }])));

    assert_eq!(content[0]["type"], "input_file");
    assert_eq!(content[0]["filename"], "report.pdf");
    assert_eq!(
        content[0]["file_data"],
        "data:application/pdf;base64,JVBERi0xLjQK"
    );
}

#[test]
fn anthropic_stream_waits_for_message_stop() {
    let mut state = AnthropicStreamState::default();
    let mut on_chunk = |_| Ok(());

    let done = handle_anthropic_sse_data(
        r#"{"type":"message_delta","usage":{"input_tokens":3,"output_tokens":2},"delta":{"stop_reason":"end_turn"}}"#,
        &mut state,
        &mut on_chunk,
    )
    .unwrap();
    assert!(!done);

    let done =
        handle_anthropic_sse_data(r#"{"type":"message_stop"}"#, &mut state, &mut on_chunk).unwrap();
    assert!(done);
}

#[test]
fn official_anthropic_template_sets_messages_protocol() {
    let provider = ProviderConfig::default_anthropic();

    assert_eq!(provider.id, "anthropic");
    assert_eq!(provider.protocol, "anthropic");
    assert_eq!(provider.base_url, "https://api.anthropic.com/v1");
    assert_eq!(provider.api_key.as_deref(), Some("$env:ANTHROPIC_API_KEY"));
    assert!(provider.models.is_empty());
    assert!(provider.default_model.is_empty());
}

#[test]
fn anthropic_request_enables_adaptive_summarized_thinking_by_default() {
    let mut provider = test_provider("anthropic", "https://api.anthropic.com/v1");
    provider.default_model = "claude-sonnet-4-5".to_string();
    let client = test_client(provider);

    let request =
        client.anthropic_request(vec![ChatMessage::plain("user", "hi")], Vec::new(), true);
    let json = serde_json::to_value(request).unwrap();

    assert_eq!(json["thinking"]["type"], "adaptive");
    assert_eq!(json["thinking"]["display"], "summarized");
}

#[test]
fn anthropic_request_can_disable_thinking_for_fallback() {
    let mut provider = test_provider("anthropic", "https://api.anthropic.com/v1");
    provider.default_model = "claude-sonnet-4-5".to_string();
    let client = test_client(provider);

    let request =
        client.anthropic_request(vec![ChatMessage::plain("user", "hi")], Vec::new(), false);
    let json = serde_json::to_value(request).unwrap();

    assert!(json.get("thinking").is_none());
}

#[test]
fn anthropic_thinking_unsupported_detects_retryable_errors() {
    assert!(anthropic_thinking_unsupported(
        400,
        "invalid request: thinking is not supported by this model"
    ));
    assert!(anthropic_thinking_unsupported(
        422,
        "unknown parameter: thinking"
    ));
    assert!(!anthropic_thinking_unsupported(401, "invalid api key"));
    assert!(!anthropic_thinking_unsupported(
        400,
        "max_tokens is too low"
    ));
}

#[test]
fn anthropic_stream_emits_reasoning_content_and_usage() {
    let mut state = AnthropicStreamState::default();
    let mut chunks = Vec::new();
    let mut on_chunk = |chunk| {
        chunks.push(chunk);
        Ok(())
    };

    for data in [
        r#"{"type":"message_start","message":{"usage":{"input_tokens":3,"output_tokens":0}}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"想"}}"#,
        r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"答"}}"#,
        r#"{"type":"message_delta","usage":{"output_tokens":2},"delta":{"stop_reason":"end_turn"}}"#,
        r#"{"type":"message_stop"}"#,
    ] {
        let done = handle_anthropic_sse_data(data, &mut state, &mut on_chunk).unwrap();
        if data.contains("message_stop") {
            assert!(done);
        }
    }

    assert_eq!(chunks.len(), 4);
    assert_eq!(chunks[0].kind, ChatStreamKind::ReasoningPartStart);
    assert_eq!(chunks[1].kind, ChatStreamKind::Reasoning);
    assert_eq!(chunks[1].text, "想");
    assert_eq!(chunks[2].kind, ChatStreamKind::ReasoningPartEnd);
    assert_eq!(chunks[3].kind, ChatStreamKind::Content);
    assert_eq!(chunks[3].text, "答");
    let usage = state.usage.unwrap();
    assert_eq!(usage.prompt_tokens, 3);
    assert_eq!(usage.completion_tokens, 2);
    assert_eq!(usage.total_tokens, 5);
}

#[test]
fn anthropic_stream_accepts_thinking_signature_delta() {
    let mut state = AnthropicStreamState::default();
    let mut on_chunk = |_| Ok(());

    handle_anthropic_sse_data(
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig_123"}}"#,
        &mut state,
        &mut on_chunk,
    )
    .unwrap();

    assert_eq!(state.thinking_signature.as_deref(), Some("sig_123"));
    assert!(state.reasoning.is_empty());
}

#[test]
fn anthropic_stream_separates_multiple_thinking_blocks() {
    let mut state = AnthropicStreamState::default();
    let mut chunks = Vec::new();
    let mut on_chunk = |chunk| {
        chunks.push(chunk);
        Ok(())
    };

    for data in [
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"Planning"}}"#,
        r#"{"type":"content_block_stop","index":0}"#,
        r#"{"type":"content_block_start","index":1,"content_block":{"type":"thinking","thinking":"Designing"}}"#,
        r#"{"type":"content_block_stop","index":1}"#,
    ] {
        handle_anthropic_sse_data(data, &mut state, &mut on_chunk).unwrap();
    }

    assert_eq!(state.reasoning, "Planning\n\nDesigning");
    assert_eq!(
        chunks.iter().map(|chunk| chunk.kind).collect::<Vec<_>>(),
        vec![
            ChatStreamKind::ReasoningPartStart,
            ChatStreamKind::Reasoning,
            ChatStreamKind::ReasoningPartEnd,
            ChatStreamKind::ReasoningPartStart,
            ChatStreamKind::Reasoning,
            ChatStreamKind::ReasoningPartEnd,
        ]
    );
}

#[test]
fn anthropic_stream_collects_tool_calls() {
    let mut state = AnthropicStreamState::default();
    let mut on_chunk = |_| Ok(());

    for data in [
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"calc","input":{}}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"x\":"}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"1}"}}"#,
    ] {
        handle_anthropic_sse_data(data, &mut state, &mut on_chunk).unwrap();
    }

    let calls = state.tool_calls.finish();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].id, "toolu_1");
    assert_eq!(calls[0].function.name, "calc");
    assert_eq!(calls[0].function.arguments, r#"{"x":1}"#);
}

#[test]
fn anthropic_stream_announces_question_tool_when_block_starts() {
    let mut state = AnthropicStreamState::default();
    let mut chunks = Vec::new();
    let mut on_chunk = |chunk| {
        chunks.push(chunk);
        Ok(())
    };

    handle_anthropic_sse_data(
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"ask_question","input":{}}}"#,
        &mut state,
        &mut on_chunk,
    )
    .unwrap();

    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].kind, ChatStreamKind::ToolCall);
    assert_eq!(chunks[0].text, "ask_question");
}

#[test]
fn anthropic_budget_is_bounded_by_max_tokens() {
    assert_eq!(anthropic_reasoning_budget(4096, 2048), Some(2048));
    assert_eq!(anthropic_reasoning_budget(4096, 32_000), None);
    assert_eq!(anthropic_reasoning_budget(1024, 32_000), None);
}

#[test]
fn test_anthropic_request_extra_body_flatten() {
    use serde_json::json;

    let extra = json!({
        "system": "override",
        "max_tokens": 1,
        "thinking": {"type": "disabled"},
        "metadata": {"user_id": "123"}
    })
    .as_object()
    .cloned();
    let mut provider = test_provider("anthropic", "https://api.anthropic.com/v1");
    provider.default_model = "claude-3-opus".to_string();
    provider.extra_body = extra;
    let client = test_client(provider);
    let request = client.anthropic_request(
        vec![
            ChatMessage::plain("system", "You are helpful"),
            ChatMessage::plain("user", "Hello"),
        ],
        Vec::new(),
        true,
    );

    let serialized = serde_json::to_string(&request).unwrap();
    let value = serde_json::to_value(&request).unwrap();

    assert_eq!(value["metadata"]["user_id"], "123");
    assert_eq!(value["system"], "You are helpful");
    assert_eq!(value["thinking"]["type"], "adaptive");
    assert_eq!(value["model"], "claude-3-opus");
    assert_eq!(value["max_tokens"], 4096);
    assert!(value.get("extra_body").is_none());
    assert_eq!(serialized.matches("\"system\":").count(), 1);
    assert_eq!(serialized.matches("\"max_tokens\":").count(), 1);
    assert_eq!(serialized.matches("\"thinking\":").count(), 1);
}

/// 09-24（对照 opencode）：Anthropic 官方接口不打 cache_control 就完全不缓存。
/// 最后一件工具、system、对话尾巴各打一个断点，和 opencode 的默认策略一样。
#[tokio::test]
async fn anthropic_requests_carry_cache_breakpoints() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let head = read_http_request_head(&mut stream).await;
        let length = head
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
            .unwrap_or(0);
        let mut body = vec![0u8; length];
        stream.read_exact(&mut body).await.unwrap();
        let events = concat!(
            "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"m\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"claude\",\"stop_reason\":null,\"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}\n\n",
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"ok\"}}\n\n",
            "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n",
            "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
        );
        stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{events}",
                    events.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        serde_json::from_slice::<Value>(&body).unwrap()
    });
    let mut provider = test_provider("anthropic-cache-test", &url);
    provider.protocol = "anthropic".to_string();
    provider.default_model = "claude-sonnet-4-5".to_string();
    let client = test_client(provider);
    let tool = crate::llm::ToolDefinition {
        kind: "function",
        function: crate::llm::FunctionDefinition {
            name: "noop".to_string(),
            description: "does nothing".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        },
    };

    client
        .chat_stream(
            vec![
                ChatMessage::plain("system", "You are a test."),
                ChatMessage::plain("user", "hi"),
            ],
            vec![tool],
            |_| Ok(()),
        )
        .await
        .unwrap();
    let body = server.await.unwrap();

    let ephemeral = |value: &Value| value["cache_control"]["type"] == "ephemeral";
    let tools = body["tools"].as_array().expect("tools");
    assert!(ephemeral(tools.last().unwrap()), "{body:#}");
    let system = body["system"].as_array().expect("system as blocks");
    assert!(ephemeral(system.last().unwrap()), "{body:#}");
    let messages = body["messages"].as_array().unwrap();
    let last_blocks = messages.last().unwrap()["content"].as_array().unwrap();
    assert!(ephemeral(last_blocks.last().unwrap()), "{body:#}");
}
