//! 一轮模型往返之后的收尾:排空流里剩下的块、记用量并发 `RoundUsage` 事件;模型不再要
//! 工具时把结果交还(先把排队的提示词接进来);到了轮数上限或复读保险丝熔断时收束。
//! 09-17 从 `chat_with_tools` 里抽出。

use super::model_round::ModelRound;
use super::record_remote_tool_chunk;
use super::round_state::RoundState;
use crate::agent::*;

impl Agent {
    /// 结果帧先到、块后到是常态:把剩下的块排空,收推理标题,再把这一轮的用量入账并广播。
    #[allow(clippy::too_many_arguments)]
    pub(super) fn finish_round_stream<F>(
        &self,
        current_turn_id: &str,
        messages: &[ChatMessage],
        request_messages: &[ChatMessage],
        result: &ChatResult,
        model_round: &mut ModelRound,
        st: &mut RoundState,
        on_event: &mut F,
    ) -> Result<()>
    where
        F: FnMut(AgentEvent) -> Result<()>,
    {
        while let Ok((chunk, received_at)) = model_round.chunk_rx.try_recv() {
            model_round.timing.observe(received_at);
            if let Some(delta) =
                record_remote_tool_chunk(&chunk, &self.runtime.pending_remote_tool_calls)
            {
                self.state.merge_turn_footprint(current_turn_id, &delta)?;
            }
            emit_model_chunk_at(
                chunk,
                received_at,
                &mut model_round.reasoning_filter,
                &mut model_round.tool_calls_seen,
                on_event,
            )?;
        }
        let (title, text) = model_round.reasoning_filter.finish();
        if let Some(title) = title {
            on_event(AgentEvent::ReasoningTitle(title))?;
        }
        if let Some(text) = text {
            on_event(AgentEvent::Chunk(ChatStreamChunk {
                kind: ChatStreamKind::Reasoning,
                text,
            }))?;
        }
        let round_completion = st.usage_accumulator.add_result(result, messages);
        st.usage_accumulator.add_generation_sample(
            round_completion,
            model_round.timing.generation_ms(),
            result.usage.is_none(),
        );
        if let Some(turn_usage) = st.usage_accumulator.usage() {
            // 上下文表读数优先取供应商标注的"最后一次请求"口径。
            let round = result
                .last_request_usage
                .clone()
                .or_else(|| result.usage.clone())
                .unwrap_or_else(|| {
                    let prompt = overflow::estimate_messages_tokens(&request_messages) as u64;
                    let completion = estimate_result_tokens(&result) as u64;
                    Usage {
                        prompt_tokens: prompt,
                        completion_tokens: completion,
                        total_tokens: prompt.saturating_add(completion),
                        ..Usage::default()
                    }
                });
            let turn_tokens = TurnTokens::from_usage(Some(&turn_usage));
            // 打断记账的依据:累计器本身是这个函数的栈上局部态,打断时随栈没了,
            // 回合守卫够不着。每次请求入账后往共享镜像同步一份。
            self.runtime.turn_usage.set(turn_tokens);
            // 与 footer 同一个口径:这次请求结束时的上下文 = prompt + completion。
            self.runtime
                .turn_usage
                .set_context(round.prompt_tokens.saturating_add(round.completion_tokens));
            // 会话实时累计 = 已落库(往轮 + 已完成子代理子会话)+ 本回合至今。
            // session_cumulative_token_totals 不含当前回合(回合末才 add_usage),所以
            // 这里补上 turn_tokens;子代理跑完那一刻它的子会话行已记好,下一个主回合
            // 读这个总数就把子代理花销带进来了(#131)。
            let mut cumulative = self
                .state
                .session_cumulative_token_totals()
                .unwrap_or_default();
            cumulative.add(turn_tokens);
            on_event(AgentEvent::RoundUsage {
                round: Box::new(round),
                turn: turn_tokens,
                cumulative,
                speed: st.usage_accumulator.generation_speed(),
                estimated: st.usage_accumulator.estimated,
                provider_id: result.provider_id.clone(),
                model: result.model.clone(),
            })?;
        }
        st.last_round_completed_at = Some(Instant::now());
        Ok(())
    }

    /// 模型没有再要工具(或工具关着):`Some(result)` 是回合的最终结果;`None` 表示排队的
    /// 提示词已经接进对话,回到循环顶再发一轮。
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn finish_without_tools<F>(
        &mut self,
        current_turn_id: &str,
        messages: &mut Vec<ChatMessage>,
        base_tool_reports: &[String],
        persisted_tool_reports: &mut Vec<(String, String)>,
        control: Option<&AgentTurnControl>,
        result: ChatResult,
        st: &mut RoundState,
        on_event: &mut F,
    ) -> Result<Option<ChatResult>>
    where
        F: FnMut(AgentEvent) -> Result<()>,
    {
        st.responses_continuation = None;
        st.continuation_input_start = messages.len();
        st.continuation_context = None;
        if let Some(control) = control {
            let queued = self.state.load_queued_prompts()?;
            if !queued.is_empty() {
                if let Some(generation) = control.pending_supersede_generation() {
                    let prompt_ids = queued
                        .iter()
                        .map(|prompt| prompt.prompt_id.clone())
                        .collect();
                    on_event(AgentEvent::GenerationSuperseded { prompt_ids })?;
                    let checkpoint = redo_checkpoint_payload(
                        messages,
                        st.replay_start,
                        base_tool_reports,
                        persisted_tool_reports,
                        st.tool_round,
                        st.question_rounds,
                    );
                    self.consume_queued_prompts(
                        current_turn_id,
                        messages,
                        queued,
                        (None, None, None, None),
                        checkpoint,
                        control,
                        on_event,
                    )
                    .await?;
                    self.checkpoint_tool_flow(current_turn_id, messages, st.replay_start);
                    control.mark_supersede_seen(generation);
                    return Ok(None);
                }
                push_assistant_context_messages(
                    messages,
                    &result.content,
                    result.reasoning.as_deref(),
                    true,
                );
                let checkpoint = redo_checkpoint_payload(
                    messages,
                    st.replay_start,
                    base_tool_reports,
                    persisted_tool_reports,
                    st.tool_round,
                    st.question_rounds,
                );
                self.consume_queued_prompts(
                    current_turn_id,
                    messages,
                    queued,
                    (
                        Some(&result.content),
                        result.reasoning.as_deref(),
                        result.provider_id.as_deref(),
                        result.model.as_deref(),
                    ),
                    checkpoint,
                    control,
                    on_event,
                )
                .await?;
                // 插话前那段正文与插话本身按活体位置记进 flow(见 `after_tool_round`)。
                self.checkpoint_tool_flow(current_turn_id, messages, st.replay_start);
                return Ok(None);
            }
        }
        let mut result = result;
        if let Some(usage) = st.usage_accumulator.usage() {
            // 供应商已给出"最后一次请求"的口径(claude-code 中转:
            // 结果帧是整轮累计,真实上下文在流内最后一次调用里)时
            // 尊重之,不再用轮用量覆盖。
            let round_usage = result.usage.take();
            if result.last_request_usage.is_none() {
                result.last_request_usage = round_usage;
            }
            result.usage = Some(usage);
            result.usage_estimated = st.usage_accumulator.estimated;
        }
        Ok(Some(result))
    }

    /// 工具轮数到顶(或复读保险丝熔断):丢掉这一轮的工具调用,把正文当最终结果收束。
    pub(super) fn finish_at_tool_limit<F>(
        &self,
        result: ChatResult,
        st: &RoundState,
        on_event: &mut F,
    ) -> Result<ChatResult>
    where
        F: FnMut(AgentEvent) -> Result<()>,
    {
        let mut result = result;
        // 复读保险丝收束:不产任何警告文本(08-24 用户裁定),模型在
        // 无工具轮已有机会正常成文,这里只收尾。真 max_rounds 上限
        // 保留原提示,但只给所有者受众;平台正文=群消息,拼进去就是
        // 把系统文本发到群里(08-24 实录)。
        if st.repeat_fused {
            tracing::warn!("tool repeat fuse: loop closed without a text answer");
        } else if self.core.prompt_audience == PromptAudience::External {
            tracing::warn!(
                "tool calls reached the round limit of {}",
                self.core.max_tool_rounds
            );
        } else {
            let warning = format!(
                "Tool calls reached the limit of {} rounds; the remaining tool calls were not executed. Set `tools.max_rounds` to 0 to allow unlimited tool rounds.",
                self.core.max_tool_rounds
            );
            let warning_chunk = if result.content.trim().is_empty() {
                warning.clone()
            } else {
                format!("\n\n{warning}")
            };
            result.content.push_str(&warning_chunk);
            on_event(AgentEvent::Chunk(ChatStreamChunk {
                kind: ChatStreamKind::Content,
                text: warning_chunk,
            }))?;
        }
        result.tool_calls.clear();
        if let Some(usage) = st.usage_accumulator.usage() {
            let round_usage = result.usage.take();
            if result.last_request_usage.is_none() {
                result.last_request_usage = round_usage;
            }
            result.usage = Some(usage);
            result.usage_estimated = st.usage_accumulator.estimated;
        }
        Ok(result)
    }
}
