//! Anthropic Messages 与 OpenAI Responses 两条线。
//!
//! 放在一起是因为它们共享一个约束：**都是有状态的**。Responses 要带
//! `previous_response_id` 续接，且续接必须钉在原来的端点上；Anthropic 的思考块
//! 有签名，跨端点重发会被拒。这个约束在 Chat Completions 上不存在。

use crate::llm::openai_compatible::*;

impl OpenAiCompatibleClient {
    pub(crate) async fn chat_anthropic_stream<F>(
        &self,
        messages: Vec<ChatMessage>,
        tools: Vec<ToolDefinition>,
        request_id: &str,
        on_chunk: &mut F,
    ) -> Result<ChatResult>
    where
        F: FnMut(ChatStreamChunk) -> Result<()>,
    {
        let mut response = self
            .send_anthropic_request(
                &self.anthropic_request(messages.clone(), tools.clone(), true),
                request_id,
                "anthropic.send",
            )
            .await?;
        let status = response.status();
        if !status.is_success() {
            let retry_after = parse_retry_after(response.headers());
            let body = response.text().await.unwrap_or_default();
            let sent_thinking_blocks = messages.iter().any(|message| {
                message.thinking_signature.is_some()
                    && message
                        .reasoning_content
                        .as_deref()
                        .is_some_and(|text| !text.trim().is_empty())
            });
            if sent_thinking_blocks && anthropic_thinking_unsupported(status.as_u16(), &body) {
                // The request already carried well-formed thinking blocks, so a
                // thinking-shaped 400 here is a protocol bug on our side, not a
                // capability gap. Surface it instead of silently downgrading the
                // whole tool loop (double request per round + split cache).
                return Err(anyhow::anyhow!(
                    "{} ({status}): {body}",
                    t(
                        "anthropic messages stream rejected replayed thinking blocks",
                        "Anthropic Messages 拒绝了回传的 thinking 块"
                    )
                )
                .context(HttpStatusFailure::classify(status.as_u16(), &body)));
            }
            if anthropic_thinking_unsupported(status.as_u16(), &body) {
                response = self
                    .send_anthropic_request(
                        &self.anthropic_request(messages, tools, false),
                        request_id,
                        "anthropic.retry_without_thinking",
                    )
                    .await?;
                let status = response.status();
                if status.is_success() {
                    return self.consume_anthropic_stream(response, on_chunk).await;
                }
                let body = response.text().await.unwrap_or_default();
                return Err(anyhow::anyhow!(
                    "{} ({status}): {body}",
                    t(
                        "anthropic messages stream request failed",
                        "Anthropic Messages 流式请求失败"
                    )
                )
                .context(HttpStatusFailure::classify(status.as_u16(), &body)));
            }
            return Err(anyhow::anyhow!(
                "{} ({status}): {body}",
                t(
                    "anthropic messages stream request failed",
                    "Anthropic Messages 流式请求失败"
                )
            )
            .context(
                HttpStatusFailure::classify(status.as_u16(), &body).with_retry_after(retry_after),
            ));
        }

        self.consume_anthropic_stream(response, on_chunk).await
    }

    pub(in crate::llm::openai_compatible) fn anthropic_request(
        &self,
        messages: Vec<ChatMessage>,
        tools: Vec<ToolDefinition>,
        thinking: bool,
    ) -> AnthropicRequest {
        let (variant_thinking, variant_extra) = self.anthropic_variant(thinking);
        let extra_body = merge_extra_body(
            sanitize_extra_body(
                self.provider.extra_body.clone(),
                ANTHROPIC_RESERVED_BODY_KEYS,
            ),
            variant_extra,
        );
        let tool_choice = (self.tool_choice_none && !tools.is_empty())
            .then(|| serde_json::json!({ "type": "none" }));
        AnthropicRequest {
            model: self.provider.default_model.clone(),
            system: lower_anthropic_system(&messages),
            messages: lower_anthropic_messages(messages),
            tools: (!tools.is_empty()).then(|| lower_anthropic_tools(tools)),
            tool_choice,
            stream: true,
            max_tokens: self
                .max_tokens_override
                .map(|cap| cap.min(self.provider.anthropic_max_tokens))
                .unwrap_or(self.provider.anthropic_max_tokens),
            temperature: Some(self.provider.effective_temperature()),
            thinking: variant_thinking,
            extra_body,
        }
    }

    pub(in crate::llm::openai_compatible) async fn send_anthropic_request(
        &self,
        request: &AnthropicRequest,
        request_id: &str,
        stage: &'static str,
    ) -> Result<reqwest::Response> {
        let url = format!("{}/messages", self.provider.base_url.trim_end_matches('/'));
        let body = with_cache_breakpoints(serde_json::to_value(request)?);
        crate::llm::request_log::record(
            &self.provider.id,
            &self.provider.default_model,
            "anthropic",
            self.request_scope,
            &url,
            &body,
        );
        self.send_with_transport_retry(request_id, stage, || {
            self.client
                .post(&url)
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", "2023-06-01")
                .json(&body)
        })
        .await
    }

    pub(crate) async fn consume_anthropic_stream<F>(
        &self,
        response: reqwest::Response,
        on_chunk: &mut F,
    ) -> Result<ChatResult>
    where
        F: FnMut(ChatStreamChunk) -> Result<()>,
    {
        let dsml = dsml_enabled_for(&self.provider);
        let mut state = AnthropicStreamState::default();
        let mut buffer = SseDataBuffer::default();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = self
            .next_response_chunk(&mut stream, "anthropic.stream")
            .await?
        {
            for data in buffer.push(&chunk)? {
                if handle_anthropic_sse_data(&data, &mut state, &mut *on_chunk)? {
                    let signature = state.thinking_signature.take();
                    let stop_reason = state.stop_reason.take();
                    let mut result = finalize_stream_result(
                        state.content,
                        state.reasoning,
                        state.usage,
                        state.tool_calls.finish(),
                        dsml,
                    )?;
                    result.thinking_signature = signature;
                    result.finish_reason = map_anthropic_stop_reason(stop_reason);
                    return Ok(result);
                }
            }
        }
        for data in buffer.finish()? {
            let _ = handle_anthropic_sse_data(&data, &mut state, &mut *on_chunk)?;
        }
        flush_buffer(
            &state.reasoning,
            &mut state.reasoning_emitted,
            ChatStreamKind::Reasoning,
            &mut *on_chunk,
            true,
        )?;
        flush_buffer(
            &state.content,
            &mut state.content_emitted,
            ChatStreamKind::Content,
            &mut *on_chunk,
            true,
        )?;
        let reasoning_part_active = state.reasoning_part_active;
        let signature = state.thinking_signature.take();
        let stop_reason = state.stop_reason.take();
        let mut result = finalize_stream_result(
            state.content,
            state.reasoning,
            state.usage,
            state.tool_calls.finish(),
            dsml,
        )?;
        result.thinking_signature = signature;
        result.finish_reason = map_anthropic_stop_reason(stop_reason);
        if reasoning_part_active {
            on_chunk(ChatStreamChunk {
                kind: ChatStreamKind::ReasoningPartEnd,
                text: String::new(),
            })?;
        }
        Ok(result)
    }

    pub(crate) async fn chat_responses_stream<F>(
        &self,
        messages: Vec<ChatMessage>,
        tools: Vec<ToolDefinition>,
        previous_response_id: Option<&str>,
        request_id: &str,
        on_chunk: &mut F,
    ) -> Result<Option<ChatResult>>
    where
        F: FnMut(ChatStreamChunk) -> Result<()>,
    {
        let extra_body = sanitize_extra_body(
            self.provider.extra_body.clone(),
            RESPONSES_RESERVED_BODY_KEYS,
        );
        let store_disabled = extra_body
            .as_ref()
            .and_then(|body| body.get("store"))
            .and_then(Value::as_bool)
            == Some(false);
        if store_disabled && !tools.is_empty() {
            bail!("OpenAI Responses tools require response storage; remove store=false or disable tools");
        }
        if previous_response_id.is_some() && store_disabled {
            bail!("OpenAI Responses tool continuation requires response storage; remove store=false or disable tools");
        }
        let request = ResponsesRequest {
            model: self.provider.default_model.clone(),
            input: lower_responses_messages(messages),
            instructions: None,
            previous_response_id: previous_response_id.map(str::to_string),
            stream: true,
            tool_choice: (self.tool_choice_none && !tools.is_empty()).then_some("none"),
            tools: (!tools.is_empty()).then(|| lower_responses_tools(tools)),
            prompt_cache_key: self.prompt_cache_key(),
            reasoning: self.responses_reasoning(),
            temperature: Some(self.provider.effective_temperature()),
            extra_body,
        };
        let reasoning_effort = request
            .reasoning
            .as_ref()
            .and_then(|reasoning| reasoning.effort.as_deref())
            .unwrap_or("disabled");
        let reasoning_summary = request
            .reasoning
            .as_ref()
            .and_then(|reasoning| reasoning.summary.as_deref())
            .unwrap_or("disabled");
        tracing::debug!(
            request_id,
            provider = %self.provider.id,
            model = %self.provider.default_model,
            reasoning_effort,
            reasoning_summary,
            "{}",
            t("Responses request configured", "Responses 请求配置完成")
        );
        let url = format!("{}/responses", self.provider.base_url.trim_end_matches('/'));
        crate::llm::request_log::record(
            &self.provider.id,
            &self.provider.default_model,
            "responses",
            self.request_scope,
            &url,
            &request,
        );
        let response = self
            .send_with_transport_retry(request_id, "responses.send", || {
                self.client
                    .post(&url)
                    .bearer_auth(&self.api_key)
                    .json(&request)
            })
            .await?;
        let status = response.status();
        if !status.is_success() {
            let retry_after = parse_retry_after(response.headers());
            let body = response.text().await.unwrap_or_default();
            if responses_unsupported(status.as_u16(), &body) {
                return Ok(None);
            }
            return Err(anyhow::anyhow!(
                "{} ({status}): {body}",
                t("responses stream request failed", "Responses 流式请求失败")
            )
            .context(
                HttpStatusFailure::classify(status.as_u16(), &body).with_retry_after(retry_after),
            ));
        }

        let dsml = dsml_enabled_for(&self.provider);
        let mut buffer = Utf8LineBuffer::default();
        let mut content = String::new();
        let mut content_emitted = 0usize;
        let mut reasoning = String::new();
        let mut reasoning_emitted = 0usize;
        let mut reasoning_part_active = false;
        let mut usage = None;
        let mut content_started = false;
        let mut output_text_delta_parts = HashSet::new();
        let mut refusal_delta_parts = HashSet::new();
        let mut response_id = None;
        let mut tool_calls = ResponsesToolAccumulator::default();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = self
            .next_response_chunk(&mut stream, "responses.stream")
            .await?
        {
            for line in buffer.push(&chunk)? {
                if handle_responses_sse_line(
                    &line,
                    &mut content,
                    &mut content_emitted,
                    &mut reasoning,
                    &mut reasoning_emitted,
                    &mut reasoning_part_active,
                    &mut usage,
                    &mut content_started,
                    &mut output_text_delta_parts,
                    &mut refusal_delta_parts,
                    &mut response_id,
                    &mut tool_calls,
                    &mut *on_chunk,
                )? {
                    return finalize_responses_stream_result(
                        content,
                        reasoning,
                        usage,
                        tool_calls.finish(),
                        dsml,
                        response_id,
                        store_disabled,
                        self.responses_continuation_suppressed(),
                    )
                    .map(Some);
                }
            }
        }
        let mut terminal_event_received = false;
        for line in buffer.finish()? {
            if handle_responses_sse_line(
                &line,
                &mut content,
                &mut content_emitted,
                &mut reasoning,
                &mut reasoning_emitted,
                &mut reasoning_part_active,
                &mut usage,
                &mut content_started,
                &mut output_text_delta_parts,
                &mut refusal_delta_parts,
                &mut response_id,
                &mut tool_calls,
                &mut *on_chunk,
            )? {
                terminal_event_received = true;
                break;
            }
        }
        if !terminal_event_received {
            // 与 chat 路径 (:finish_reason 缺失分支) 同型:截断按传输失败
            // 分类,冷却/重试机制才会把这种端点降权,而不是裸错误穿透。
            return Err(
                anyhow::anyhow!("OpenAI Responses stream ended before a terminal event").context(
                    TransportFailure {
                        stage: "responses.stream",
                        kind: TransportFailureKind::Other,
                    },
                ),
            );
        }
        finalize_responses_stream_result(
            content,
            reasoning,
            usage,
            tool_calls.finish(),
            dsml,
            response_id,
            store_disabled,
            self.responses_continuation_suppressed(),
        )
        .map(Some)
    }
}

/// Anthropic 官方接口不打 `cache_control` 就完全不缓存（09-24，对照 opencode 的默认策略）。
/// 三个断点：最后一件工具、system、对话尾巴（最后一条消息的最后一个内容块）。
/// 上限是 4 个；思考块不能打，往前找；用户在 extra_body 里自己配了 `cache_control`
/// 的请求原样不动。
pub(in crate::llm::openai_compatible) fn with_cache_breakpoints(mut body: Value) -> Value {
    if body.get("cache_control").is_some() {
        return body;
    }
    let mark = || serde_json::json!({ "type": "ephemeral" });
    if let Some(last_tool) = body
        .get_mut("tools")
        .and_then(Value::as_array_mut)
        .and_then(|tools| tools.last_mut())
        .and_then(Value::as_object_mut)
    {
        last_tool.insert("cache_control".to_string(), mark());
    }
    if let Some(system) = body
        .get("system")
        .and_then(Value::as_str)
        .map(str::to_string)
    {
        body["system"] = serde_json::json!([
            { "type": "text", "text": system, "cache_control": mark() }
        ]);
    }
    if let Some(block) = body
        .get_mut("messages")
        .and_then(Value::as_array_mut)
        .and_then(|messages| messages.last_mut())
        .and_then(|message| message.get_mut("content"))
        .and_then(Value::as_array_mut)
        .and_then(|blocks| {
            blocks.iter_mut().rev().find(|block| {
                !matches!(
                    block.get("type").and_then(Value::as_str),
                    Some("thinking" | "redacted_thinking")
                )
            })
        })
        .and_then(Value::as_object_mut)
    {
        block.insert("cache_control".to_string(), mark());
    }
    body
}
