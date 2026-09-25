//! 一轮模型往返:发请求、收流、认工具调用,同时盯着「同一回合被更新超越」与转轮 tick。
//! 09-17 从 `chat_with_tools` 里抽出;流里攒下的东西(块接收器、生成时长、推理标题过滤器)
//! 随 [`ModelRound`] 交回给调用方收尾。

use super::{record_remote_tool_chunk, RoundTiming};
use crate::agent::*;

/// 一轮模型往返的产出。`outcome == None` 表示这一轮被同一回合的更新超越,没有结果。
pub(super) struct ModelRound {
    pub(super) outcome: Option<Result<ChatResult>>,
    /// 流里还没吐完的块:结果先到、块后到是常态,调用方收尾时再排空一次。
    pub(super) chunk_rx: tokio::sync::mpsc::UnboundedReceiver<(ChatStreamChunk, Instant)>,
    pub(super) timing: RoundTiming,
    pub(super) reasoning_filter: ReasoningTitleFilter,
    pub(super) tool_calls_seen: usize,
    /// 这一轮有没有流出过任何块——溢出自愈只敢在一个字都没出过的时候重试。
    pub(super) streamed: bool,
}

impl Agent {
    pub(super) async fn run_model_round<F>(
        &mut self,
        current_turn_id: &str,
        request_messages: Vec<ChatMessage>,
        definitions: Vec<miyu_core::llm::ToolDefinition>,
        tool_choice_none: bool,
        responses_continuation: Option<&miyu_core::llm::ResponsesContinuation>,
        control: Option<&AgentTurnControl>,
        on_event: &mut F,
    ) -> Result<ModelRound>
    where
        F: FnMut(AgentEvent) -> Result<()>,
    {
        let (chunk_tx, mut chunk_rx) =
            tokio::sync::mpsc::unbounded_channel::<(ChatStreamChunk, Instant)>();
        let mut reasoning_filter = ReasoningTitleFilter::default();
        // 与 reasoning_filter 同生命周期:一轮模型调用 = 一条 assistant
        // 消息,批量提示要的正是"这条消息里的第几个工具调用"。
        let mut tool_calls_seen = 0usize;
        let round_streamed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut round_timing = RoundTiming::default();
        let round = {
            let streamed_flag = round_streamed.clone();
            let client = if tool_choice_none {
                self.client.with_tool_choice_none()
            } else {
                self.client.clone()
            };
            let llm_future = client.chat_stream_with_continuation(
                request_messages,
                definitions,
                responses_continuation,
                move |chunk| {
                    streamed_flag.store(true, Ordering::Relaxed);
                    let _ = chunk_tx.send((chunk, Instant::now()));
                    Ok(())
                },
            );
            tokio::pin!(llm_future);
            let mut spinner_interval = tokio::time::interval(self.core.spinner_interval);
            spinner_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            spinner_interval.tick().await;
            let supersede = control.and_then(|control| control.supersede.as_deref());
            let supersede_generation = control.and_then(|control| {
                supersede.map(|_| control.supersede_seen.load(Ordering::Acquire))
            });
            loop {
                tokio::select! {
                    biased;
                    _ = async {
                        match (supersede, supersede_generation) {
                            (Some(signal), Some(generation)) => signal.wait_after(generation).await,
                            _ => std::future::pending::<()>().await,
                        }
                    } => {
                        break None;
                    }
                    result = &mut llm_future => {
                        break Some(result);
                    }
                    Some((chunk, received_at)) = chunk_rx.recv() => {
                        round_timing.observe(received_at);
                        if let Some(delta) = record_remote_tool_chunk(
                            &chunk,
                            &self.runtime.pending_remote_tool_calls,
                        ) {
                            self.state.merge_turn_footprint(current_turn_id, &delta)?;
                        }
                        emit_model_chunk_at(
                            chunk,
                            received_at,
                            &mut reasoning_filter,
                            &mut tool_calls_seen,
                            on_event,
                        )?;
                    }
                    _ = spinner_interval.tick() => {
                        on_event(AgentEvent::SpinnerTick)?;
                    }
                }
            }
        };
        Ok(ModelRound {
            outcome: round,
            chunk_rx,
            timing: round_timing,
            reasoning_filter,
            tool_calls_seen,
            streamed: round_streamed.load(Ordering::Relaxed),
        })
    }
}
