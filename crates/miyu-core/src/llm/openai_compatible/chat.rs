//! Chat Completions 协议的请求与流处理。
//!
//! `send_with_transport_retry` 是重试的收口：只有传输层失败才在同一端点重试，
//! 其余交给端点调度（见 [`super::endpoints`]）。
//!
//! `cache_keepalive` 定期发一个极小的请求，把供应商的前缀缓存续上——缓存有 TTL，
//! 让它过期等于下一轮从 token 0 开始重算。

use crate::llm::openai_compatible::*;

impl OpenAiCompatibleClient {
    pub async fn chat_stream<F>(
        &self,
        messages: Vec<ChatMessage>,
        tools: Vec<ToolDefinition>,
        mut on_chunk: F,
    ) -> Result<ChatResult>
    where
        F: FnMut(ChatStreamChunk) -> Result<()>,
    {
        self.chat_stream_inner(messages, tools, None, false, &mut on_chunk)
            .await
    }

    pub async fn chat_stream_with_continuation<F>(
        &self,
        messages: Vec<ChatMessage>,
        tools: Vec<ToolDefinition>,
        continuation: Option<&ResponsesContinuation>,
        mut on_chunk: F,
    ) -> Result<ChatResult>
    where
        F: FnMut(ChatStreamChunk) -> Result<()>,
    {
        self.chat_stream_inner(messages, tools, continuation, false, &mut on_chunk)
            .await
    }

    /// Runs an internal completion without exposing partial output. Since no
    /// chunk is committed to a user, a failed endpoint can be safely replaced
    /// even after it emitted an incomplete response.
    pub async fn chat_buffered(
        &self,
        messages: Vec<ChatMessage>,
        tools: Vec<ToolDefinition>,
    ) -> Result<ChatResult> {
        self.chat_stream_inner(messages, tools, None, true, &mut |_| Ok(()))
            .await
    }

    /// Cache keepalive ping (v7 DeepSeek 高命中策略): re-sends the exact
    /// prompt prefix of the last live request as a non-streaming
    /// max_tokens=1 completion so best-effort provider caches keep the deep
    /// prefix alive between user turns. The messages/tools serialization goes
    /// through the same path as live chat, so the server-rendered prompt is
    /// byte-identical (measured: extra body params like max_tokens do not
    /// affect the provider prefix cache key). Returns the reported usage, or
    /// None when the selected endpoint speaks a protocol where the ping does
    /// not apply (Anthropic / OpenAI Responses).
    pub async fn cache_keepalive(
        &self,
        messages: Vec<ChatMessage>,
        tools: Vec<ToolDefinition>,
        endpoint_hint: Option<&(String, String)>,
    ) -> Result<Option<Usage>> {
        let endpoints = self.endpoints.as_ref();
        // 钉住上一条真实请求的 endpoint:缓存按 (供应商, 前缀) 存活,
        // 轮转选出的"下一家"没有这份前缀,ping 过去只是白买 miss。
        let hinted = endpoint_hint.and_then(|(provider, model)| {
            endpoints.iter().position(|endpoint| {
                endpoint.provider.id == *provider && endpoint.provider.default_model == *model
            })
        });
        let index = hinted.unwrap_or_else(|| {
            ordered_endpoint_indices(endpoints)
                .first()
                .copied()
                .unwrap_or(0)
        });
        let endpoint = endpoints
            .get(index)
            .context("no LLM endpoint configured for cache keepalive")?;
        let client = self.with_endpoint(endpoint);
        if client.uses_openai_responses()
            || client.uses_anthropic_messages()
            || provider_uses_cli_relay(&client.provider)
            // 保温 ping 是非流式的,而 Zen 免费档只放行 stream:true(09-20 实测)。
            // 打过去必然 403,而 403 会按认证失败把这家端点冷却 600 秒——为了
            // 省一次 miss 把整家端点摁死十分钟,不划算。
            || zen_tools::aliases_apply(&client.provider)
        {
            return Ok(None);
        }
        client.cache_keepalive_single(messages, tools).await
    }

    pub(crate) async fn cache_keepalive_single(
        &self,
        messages: Vec<ChatMessage>,
        tools: Vec<ToolDefinition>,
    ) -> Result<Option<Usage>> {
        let request_id = gen_llm_request_id();
        let extra_body = merge_extra_body(
            sanitize_extra_body(self.provider.extra_body.clone(), CHAT_RESERVED_BODY_KEYS),
            self.chat_variant_extra_body(),
        );
        let messages = prepare_chat_messages_for_provider(&self.provider, messages);
        let request = ChatRequest {
            model: self.provider.default_model.clone(),
            messages,
            temperature: self.provider.effective_temperature(),
            stream: false,
            stream_options: None,
            max_tokens: Some(1),
            tools: (!tools.is_empty()).then_some(tools),
            tool_choice: None,
            prompt_cache_key: self.prompt_cache_key(),
            chat_template_kwargs: taotoken_glm_chat_template_kwargs(&self.provider),
            extra_body,
        };
        let url = format!(
            "{}/chat/completions",
            self.provider.base_url.trim_end_matches('/')
        );
        let response = self
            .send_chat_completion_request(&url, &request, &request_id, "chat.cache_keepalive")
            .await?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if !status.is_success() {
            bail!("cache keepalive ping failed with HTTP {status}: {body}");
        }
        let value: serde_json::Value = serde_json::from_str(&body)
            .with_context(|| "cache keepalive response was not valid JSON")?;
        let usage = value
            .get("usage")
            .cloned()
            .and_then(|usage| serde_json::from_value::<Usage>(usage).ok())
            .map(|mut usage| {
                usage.normalize_cache_fields();
                usage
            });
        // 保温 ping 也进 cache-usage 记账:不然命中率诊断里多出一段
        // "看不见的流量"(deepseek 报告 P2 的观测盲区)。
        crate::llm::cache_log::record(
            "keepalive",
            &self.provider.id,
            &self.provider.default_model,
            0,
            &request_id,
            usage.as_ref(),
        );
        Ok(usage)
    }

    pub(crate) async fn chat_stream_inner<F>(
        &self,
        messages: Vec<ChatMessage>,
        tools: Vec<ToolDefinition>,
        continuation: Option<&ResponsesContinuation>,
        buffered: bool,
        on_chunk: &mut F,
    ) -> Result<ChatResult>
    where
        F: FnMut(ChatStreamChunk) -> Result<()>,
    {
        let request_id = gen_llm_request_id();
        // 前缀指纹要在**发出去之前**算并比对:比对结果依赖「上一次请求」,
        // 而响应是乱序回来的,放到 record 那一刻算会把并发请求比串。
        let prefix = crate::llm::cache_prefix::compare_and_store(
            self.request_scope,
            self.log_identity.session(),
            &crate::llm::cache_prefix::PrefixChain::of(&messages, &tools),
        );
        let endpoints = self.endpoints.as_ref();
        let mut errors = Vec::new();
        // 这一轮有没有被 agy 的内容策略拦过（见下面 push 失败行那一处）。
        let mut content_policy_blocked = false;
        let mut order = if let Some(continuation) = continuation {
            let index = endpoints
                .iter()
                .position(|endpoint| endpoint.id() == continuation.endpoint_id)
                .with_context(|| {
                    format!(
                        "Responses continuation endpoint is no longer available: {}",
                        continuation.endpoint_id
                    )
                })?;
            vec![index]
        } else {
            ordered_endpoint_indices(endpoints)
        };
        // Every endpoint is cooling down. Refusing outright would strand a
        // single-endpoint user for the whole cooldown, so one probe still goes
        // out — but exactly one, and to whichever endpoint recovers first.
        // Refilling the pool here (and then padding it below) meant a rate
        // limit cost three requests per turn *for the entire cooldown*, which
        // made the cooldown worse than useless.
        let probe_only = order.is_empty();
        if probe_only {
            tracing::warn!(
                request_id,
                endpoint_count = endpoints.len(),
                all_endpoints_cooling_down = true,
                "{}",
                t(
                    "All LLM endpoints are cooling down; sending a single probe",
                    "所有 LLM 端点均在冷却；仅发送一次探测请求"
                )
            );
            order = soonest_ready_endpoint_index(endpoints)
                .into_iter()
                .collect();
        }
        // A dropped stream or a 5xx is a moment in time, not a verdict on the
        // endpoint. Tying the number of attempts to the number of configured
        // endpoints meant someone with a single model got no retry at all,
        // which is backwards: they are the ones with nowhere else to go. Pad
        // the attempt list by cycling so every setup gets the same budget.
        // Errors that a retry cannot fix still stop on the first attempt —
        // `endpoint_failover_allowed` returns before the next one is tried,
        // and `same_endpoint_retry_allowed` skips the padded repeats of an
        // endpoint that answered 429/401.
        if !probe_only && !order.is_empty() && order.len() < MIN_ENDPOINT_ATTEMPTS {
            let cycle: Vec<usize> = order.clone();
            while order.len() < MIN_ENDPOINT_ATTEMPTS {
                order.extend(cycle.iter().copied());
            }
            order.truncate(MIN_ENDPOINT_ATTEMPTS);
        }
        tracing::debug!(
            request_id,
            endpoint_count = order.len(),
            message_count = messages.len(),
            tool_count = tools.len(),
            continued = continuation.is_some(),
            "{}",
            t("LLM request started", "LLM 请求已开始")
        );
        let mut exhausted: Vec<String> = Vec::new();
        let mut previous_endpoint: Option<String> = None;
        for (attempt, index) in order.into_iter().enumerate() {
            let endpoint = &endpoints[index];
            if exhausted.contains(&endpoint.id()) {
                continue;
            }
            // 同端点的填充重试稍作退避:5xx/断流是瞬时故障,零间隔连打同
            // 一端点多半撞上同一故障;切到不同端点仍零间隔(failover 不变)。
            if previous_endpoint.as_deref() == Some(endpoint.id().as_str()) {
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            previous_endpoint = Some(endpoint.id());
            let client = self.with_endpoint(endpoint);
            if attempt > 0 {
                on_chunk(ChatStreamChunk {
                    kind: ChatStreamKind::ReasoningReset,
                    text: String::new(),
                })?;
            }
            let started = Instant::now();
            tracing::debug!(
                request_id,
                attempt = attempt + 1,
                provider = %endpoint.provider.id,
                model = %endpoint.provider.default_model,
                key_index = endpoint.key_index + 1,
                "{}",
                t("LLM endpoint attempt started", "LLM 端点尝试已开始")
            );
            let mut attempt_committed = false;
            let result = {
                let buffered = buffered || self.buffered_delivery;
                let mut attempt_on_chunk = |chunk: ChatStreamChunk| {
                    if !buffered {
                        attempt_committed |=
                            stream_chunk_commits_attempt(&chunk, client.reasoning_visibility);
                    }
                    on_chunk(chunk)
                };
                client
                    .chat_stream_single(
                        messages.clone(),
                        tools.clone(),
                        continuation.map(|continuation| continuation.response_id.as_str()),
                        &request_id,
                        &mut attempt_on_chunk,
                    )
                    .await
            };
            match result {
                Ok(mut result) => {
                    result.provider_id = Some(endpoint.provider.id.clone());
                    result.model = Some(endpoint.provider.default_model.clone());
                    if let Some(next) = result.responses_continuation.as_mut() {
                        next.endpoint_id = endpoint.id();
                    }
                    mark_endpoint_success(endpoint);
                    let turn = self.log_identity.turn();
                    crate::llm::cache_log::record_with_context(
                        self.request_scope,
                        &endpoint.provider.id,
                        &endpoint.provider.default_model,
                        endpoint.key_index,
                        &request_id,
                        result.usage.as_ref(),
                        &crate::llm::cache_log::RecordContext {
                            session: self.log_identity.session(),
                            turn: turn.as_deref(),
                            prefix: Some(&prefix),
                        },
                    );
                    tracing::debug!(
                        request_id,
                        attempt = attempt + 1,
                        provider = %endpoint.provider.id,
                        model = %endpoint.provider.default_model,
                        elapsed_ms = started.elapsed().as_millis(),
                        "{}",
                        t("LLM endpoint succeeded", "LLM 端点请求成功")
                    );
                    return Ok(result);
                }
                Err(err) => {
                    let cooldown = mark_endpoint_failure(endpoint, &err);
                    let endpoint_cooling_down = cooldown.is_some();
                    let cooldown_seconds = cooldown.map(|duration| duration.as_secs()).unwrap_or(0);
                    if let Some(failure) = err.downcast_ref::<TransportFailure>() {
                        tracing::error!(
                            request_id,
                            attempt = attempt + 1,
                            provider = %endpoint.provider.id,
                            model = %endpoint.provider.default_model,
                            stage = failure.stage,
                            transport_kind = %failure.kind,
                            endpoint_cooling_down,
                            cooldown_seconds,
                            elapsed_ms = started.elapsed().as_millis(),
                            error = %format!("{err:#}"),
                            "{}",
                            t("LLM endpoint transport failure", "LLM 端点传输失败")
                        );
                    } else if let Some(failure) = err.downcast_ref::<HttpStatusFailure>() {
                        tracing::error!(
                            request_id,
                            attempt = attempt + 1,
                            provider = %endpoint.provider.id,
                            model = %endpoint.provider.default_model,
                            status = failure.status,
                            failure_kind = %failure.kind,
                            endpoint_cooling_down,
                            cooldown_seconds,
                            elapsed_ms = started.elapsed().as_millis(),
                            // 被拒请求的形状(不含正文):中转常把上游的字段
                            // 路径吞掉,只剩「应该是个字符串」这类话,而失败
                            // 的那一轮会整个回滚、库里不留痕。见 request_shape。
                            shape = %crate::llm::request_shape::summarize(&messages, &tools),
                            session = self.log_identity.session().unwrap_or("-"),
                            "{}",
                            t("LLM endpoint HTTP failure", "LLM 端点 HTTP 请求失败")
                        );
                    } else {
                        tracing::error!(
                            request_id,
                            attempt = attempt + 1,
                            provider = %endpoint.provider.id,
                            model = %endpoint.provider.default_model,
                            endpoint_cooling_down,
                            cooldown_seconds,
                            elapsed_ms = started.elapsed().as_millis(),
                            error = %format!("{err:#}"),
                            "{}",
                            t(
                                "LLM endpoint failed outside the HTTP send stage",
                                "LLM 端点在 HTTP 发送阶段之外失败"
                            )
                        );
                    }
                    // agy 的内容策略拦截要一路带到回合收尾：被拦的那一轮留在
                    // 上下文里，之后每一轮都会把同一句话再发一遍、再被拦一次，
                    // 整条会话就哑了（用户 09-20 在 QQ 群里实测）。只认 agy
                    // （用户 09-20 拍板「仅 agy 时」）：它的拦截是会话级粘性的，
                    // 别家的内容策略多半是一次性的，不该据此删用户的话。
                    content_policy_blocked |= agy_content_policy_block(&endpoint.provider, &err);
                    errors.push(endpoint_failure_line(endpoint, &err, cooldown));
                    if !same_endpoint_retry_allowed(&err) {
                        exhausted.push(endpoint.id());
                    }
                    // 这两句会被 anyhow 顶到错误链最前面，也就是用户第一眼看到的
                    // 那一句——原来是英文长句，真正的原因（429 之类）被压在链尾
                    // （BUG-16）。改短、改成中文，并且把原因先说了。
                    if attempt_committed {
                        return Err(err.context(t(
                            "the stream failed after output had started, so no other endpoint was tried",
                            "已经开始输出之后才失败，没有再换别的端点",
                        )));
                    }
                    if !endpoint_failover_allowed(&err) {
                        return Err(err.context(t(
                            "the request itself was rejected, so no other endpoint was tried",
                            "这条请求本身被拒了，换端点也没用",
                        )));
                    }
                }
            }
        }
        let message = all_endpoints_failed_message(&errors, &request_id);
        if content_policy_blocked {
            return Err(anyhow::Error::new(ContentPolicyBlocked { message }));
        }
        bail!("{message}")
    }

    pub(crate) async fn chat_stream_single<F>(
        &self,
        messages: Vec<ChatMessage>,
        tools: Vec<ToolDefinition>,
        previous_response_id: Option<&str>,
        request_id: &str,
        on_chunk: &mut F,
    ) -> Result<ChatResult>
    where
        F: FnMut(ChatStreamChunk) -> Result<()>,
    {
        let protocol = ProviderProtocol::from_provider(&self.provider)?;
        // CLI 中转线的 future 装箱:三条线的状态机(子进程泵/事件解析)都很
        // 大,内联进本函数的 future 会把 debug 构建下 2MB 的线程栈撑爆
        // (agent 层测试实录),装箱后外层只剩一个指针。
        if protocol == ProviderProtocol::ClaudeCode {
            return Box::pin(self.chat_claude_code_stream(messages, tools, request_id, on_chunk))
                .await;
        }
        if protocol == ProviderProtocol::Antigravity {
            return Box::pin(self.chat_antigravity_stream(messages, tools, request_id, on_chunk))
                .await;
        }
        if protocol == ProviderProtocol::Codex {
            return Box::pin(self.chat_codex_stream(messages, tools, request_id, on_chunk)).await;
        }
        if protocol == ProviderProtocol::CodeBuddy {
            return Box::pin(self.chat_codebuddy_stream(messages, tools, request_id, on_chunk))
                .await;
        }
        let uses_responses = protocol == ProviderProtocol::OpenAiResponses
            || (protocol == ProviderProtocol::Auto && self.uses_openai_responses());
        if previous_response_id.is_some() && !uses_responses {
            bail!("Responses continuation endpoint no longer uses the Responses protocol");
        }
        if protocol == ProviderProtocol::Anthropic
            || (protocol == ProviderProtocol::Auto && self.uses_anthropic_messages())
        {
            return self
                .chat_anthropic_stream(messages, tools, request_id, on_chunk)
                .await;
        }
        if uses_responses {
            if let Some(result) = self
                .chat_responses_stream(
                    messages.clone(),
                    tools.clone(),
                    previous_response_id,
                    request_id,
                    on_chunk,
                )
                .await?
            {
                return Ok(result);
            }
            if previous_response_id.is_some() {
                bail!("OpenAI Responses continuation is not supported by this provider");
            }
            if protocol == ProviderProtocol::OpenAiResponses {
                bail!("OpenAI Responses protocol is not supported by this provider");
            }
            if let Some((info, variant)) = self.selected_reasoning_variant() {
                if !reasoning_variant_supported_for_protocol(
                    &self.provider,
                    &info,
                    &variant,
                    ProviderProtocol::OpenAiChat,
                ) {
                    bail!(
                        "thinking variant '{}' cannot be applied after falling back from OpenAI Responses to Chat Completions",
                        variant.id
                    );
                }
            }
        }
        let extra_body = merge_extra_body(
            sanitize_extra_body(self.provider.extra_body.clone(), CHAT_RESERVED_BODY_KEYS),
            self.chat_variant_extra_body(),
        );
        let mut messages = prepare_chat_messages_for_provider(&self.provider, messages);
        // Zen 免费档按工具名认客户端(09-20 实测,见 zen_tools)。改名要连历史里
        // 的 tool_calls 一起改,不然清单报 `shell`、回放叫 `run_command`,两边对
        // 不上。
        let mut tools = tools;
        zen_tools::lower_tools(&self.provider, &mut tools);
        zen_tools::lower_messages(&self.provider, &mut messages);
        let mut request = ChatRequest {
            model: self.provider.default_model.clone(),
            messages,
            temperature: self.provider.effective_temperature(),
            stream: true,
            stream_options: Some(ChatStreamOptions {
                include_usage: true,
            }),
            max_tokens: self.max_tokens_override,
            tool_choice: (self.tool_choice_none && !tools.is_empty()).then_some("none"),
            tools: (!tools.is_empty()).then_some(tools),
            prompt_cache_key: self.prompt_cache_key(),
            chat_template_kwargs: taotoken_glm_chat_template_kwargs(&self.provider),
            extra_body,
        };
        let url = format!(
            "{}/chat/completions",
            self.provider.base_url.trim_end_matches('/')
        );
        let mut response = self
            .send_chat_completion_request(&url, &request, request_id, "chat.send")
            .await?;
        let mut status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            // Zen 不走这条:它只放行 stream:true,非流式重试必然再挨一个 403,
            // 白白把端点冷却掉(09-20)。
            if non_stream_quota_fallback_candidate(status.as_u16(), &body)
                && !zen_tools::aliases_apply(&self.provider)
            {
                let mut retry = request.clone();
                retry.stream = false;
                retry.stream_options = None;
                let response = self
                    .send_chat_completion_request(
                        &url,
                        &retry,
                        request_id,
                        "chat.retry_without_streaming",
                    )
                    .await?;
                let retry_status = response.status();
                if retry_status.is_success() {
                    tracing::info!(
                        request_id,
                        provider = %self.provider.id,
                        model = %self.provider.default_model,
                        "{}",
                        t(
                            "streaming quota was unavailable; non-streaming compatibility retry succeeded",
                            "流式配额不可用；非流式兼容重试成功"
                        )
                    );
                    return self
                        .consume_chat_completion_response(response, on_chunk)
                        .await;
                }
                let retry_body = response.text().await.unwrap_or_default();
                tracing::debug!(
                    request_id,
                    status = retry_status.as_u16(),
                    "{}",
                    t(
                        "non-streaming quota compatibility retry returned an HTTP error",
                        "非流式配额兼容重试返回 HTTP 错误"
                    )
                );
                return self.bail_chat_completion_failure(retry_status.as_u16(), &retry_body);
            }
            if request.tool_choice.is_some() && tool_choice_unsupported(status.as_u16(), &body) {
                // 有的网关不认 `tool_choice`（09-24 B5）：退回原来那种「这一轮不带工具」，
                // 这一轮的缓存照旧断，但回答不受影响。
                request.tool_choice = None;
                request.tools = None;
                response = self
                    .send_chat_completion_request(
                        &url,
                        &request,
                        request_id,
                        "chat.retry_without_tool_choice",
                    )
                    .await?;
                status = response.status();
                if status.is_success() {
                    return self
                        .consume_chat_completion_stream(response, on_chunk)
                        .await;
                }
                let body = response.text().await.unwrap_or_default();
                return self.bail_chat_completion_failure(status.as_u16(), &body);
            }
            if stream_options_unsupported(status.as_u16(), &body) {
                request.stream_options = None;
                response = self
                    .send_chat_completion_request(
                        &url,
                        &request,
                        request_id,
                        "chat.retry_without_stream_options",
                    )
                    .await?;
                status = response.status();
                if status.is_success() {
                    return self
                        .consume_chat_completion_stream(response, on_chunk)
                        .await;
                }
                let body = response.text().await.unwrap_or_default();
                if let Some(result) = self
                    .try_zen_chat_completion_compat_retry(
                        &url,
                        &request,
                        status.as_u16(),
                        &body,
                        request_id,
                        on_chunk,
                    )
                    .await?
                {
                    return Ok(result);
                }
                return self.bail_chat_completion_failure(status.as_u16(), &body);
            }
            if let Some(result) = self
                .try_zen_chat_completion_compat_retry(
                    &url,
                    &request,
                    status.as_u16(),
                    &body,
                    request_id,
                    on_chunk,
                )
                .await?
            {
                return Ok(result);
            }
            return self.bail_chat_completion_failure(status.as_u16(), &body);
        }

        self.consume_chat_completion_stream(response, on_chunk)
            .await
    }
}

/// 一个端点失败了，写给人看的一行。
///
/// 原来是 `provider / model key#N: {err:#}`——纯英文、把整条错误链原样倒出来
/// （里头常常是供应商的原始 JSON），而最要紧的两件事「这是什么毛病」「这个端点
/// 要停多久」一个都没有（BUG-16）。
pub(in crate::llm::openai_compatible) fn endpoint_failure_line(
    endpoint: &LlmEndpoint,
    error: &anyhow::Error,
    cooldown: Option<Duration>,
) -> String {
    let head = format!(
        "{} / {}（key#{}）：",
        endpoint.provider.id,
        endpoint.provider.default_model,
        endpoint.key_index + 1
    );
    // 有分类就只说分类那一句（它自带供应商的原话）：把整条错误链倒出来的话，
    // 后面跟的是一整坨原始 JSON，既没信息又把别的端点挤掉（BUG-16）。
    let reason = match error.downcast_ref::<HttpStatusFailure>() {
        Some(failure) => failure.to_string(),
        None => clip_reason(&format!("{error:#}")),
    };
    let mut line = format!("{head}{reason}");
    if let Some(cooldown) = cooldown {
        // 冷却秒数以前只进 tracing，UI 一个字都拿不到——而「这个端点要停 10
        // 分钟」恰恰是撞上限流时最有用的一条。
        line.push_str(&t("; cooled down for ", "；该端点暂停 "));
        line.push_str(&humanize_duration(cooldown));
    }
    line
}

/// 这一条端点失败，是不是「**agy** 因为内容策略拒了这条提示词」。
///
/// 只认 agy（用户 09-20 拍板「仅 agy 时」）：它的拦截是**会话级粘性**的——被拦
/// 的那一轮留在上下文里，之后每一轮都会把同一句话再发一遍、再被拦一次，整条会话
/// 就哑了。别家的内容策略多半是一次性的，不该据此把用户的话从上下文里删掉。
pub(in crate::llm::openai_compatible) fn agy_content_policy_block(
    provider: &ProviderConfig,
    error: &anyhow::Error,
) -> bool {
    provider_uses_antigravity(provider)
        && error
            .downcast_ref::<HttpStatusFailure>()
            .is_some_and(|failure| failure.kind == HttpFailureKind::ContentPolicy)
}

/// 整池都失败了，**而且其中有 agy 因为内容策略拒了这条提示词**。
///
/// 单开一个类型是为了把这件事带到回合收尾（`finish_failed_run`）：那边只拿得到
/// 一个 `anyhow::Error`，而聚合消息是纯字符串，失败分类早就丢了。收尾处据此把
/// 这一轮踢出后续上下文——不踢的话它每轮都会被重发、每轮都被拦，整条会话就哑了
/// （用户 09-20 在 QQ 群里实测）。
#[derive(Debug)]
pub struct ContentPolicyBlocked {
    pub message: String,
}

impl std::fmt::Display for ContentPolicyBlocked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ContentPolicyBlocked {}

/// 一条端点的失败理由裁到能读的长度。
///
/// 整条消息在 daemon 那头会被 `safe_error_message` 砍到 1000 字，砍掉的正好是
/// 排在后面的端点明细（也就是「还试过谁、各自为什么不行」）。按条裁就不会整段
/// 丢尾巴。
pub(in crate::llm::openai_compatible) fn clip_reason(reason: &str) -> String {
    const MAX_CHARS: usize = 160;
    let single_line = reason.split_whitespace().collect::<Vec<_>>().join(" ");
    if single_line.chars().count() <= MAX_CHARS {
        return single_line;
    }
    single_line.chars().take(MAX_CHARS).collect::<String>() + "…"
}

pub(in crate::llm::openai_compatible) fn humanize_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    if seconds >= 60 {
        return format!("{} {}", seconds / 60, t("min", "分钟"));
    }
    format!("{seconds} {}", t("s", "秒"))
}

/// 池里每个端点都失败了，写给人看的那一段。
///
/// 形状是「一句结论 + 每个端点一行」：原来是一句英文
/// `no LLM provider/model endpoint succeeded` 加一串原始错误链，人看完不知道
/// 发生了什么（BUG-16）。
///
/// 09-19 曾在末尾再加一句 `→ 该怎么办`（等冷却结束、检查 API key 之类），
/// 09-20 用户点名去掉：每条端点行里已经写了「被限流」「该端点暂停 10 分钟」，
/// 那句话没有增量信息，只是把同一件事再说一遍。
pub(in crate::llm::openai_compatible) fn all_endpoints_failed_message(
    errors: &[String],
    request_id: &str,
) -> String {
    let headline = if errors.len() == 1 {
        t("the model endpoint failed", "模型端点没跑通")
    } else {
        t("every model endpoint failed", "所有模型端点都没跑通")
    };
    let mut message = format!("{headline}（{}）：", request_id);
    for line in errors {
        message.push_str("\n  · ");
        message.push_str(line);
    }
    message
}
