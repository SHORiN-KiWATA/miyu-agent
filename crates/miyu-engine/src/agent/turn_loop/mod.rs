//! 回合主循环：模型请求 ↔ 工具调用的往返。
//!
//! `chat_with_tools` 是整个 agent 的心脏——发请求、收流、认出工具调用、执行、
//! 把结果拼回去、再发一轮，直到模型不再要工具。
//!
//! 这里同时要处理三种打断：用户排队了新消息（`consume_queued_prompts`）、回合
//! 被更新的同一回合超越、上下文溢出。三者都可能落在任意一次 await 上，所以状
//! 态推进都写成「先落库再改内存」，中途挂掉能从库里接着走。
//!
//! `run_concurrent_segment` 负责一批里相邻可并发调用的一起执行：**输出必须按请求
//! 顺序映射回去**，不能按完成顺序——否则模型看到的结果和它发的调用对不上。

mod catalog_refresh;
mod finish;
mod model_round;
mod overflow_recovery;
mod parallel;
mod queue;
mod redo;
mod repeat_gate;
mod round_request;
mod round_state;
mod stream;
mod tool_call;
mod tool_exec;

use overflow_recovery::RoundRecovery;
use repeat_gate::{REPEAT_FUSE_THRESHOLD, REPEAT_SKIP_THRESHOLD};
use round_state::RoundState;
use tool_exec::ToolBatchOutcome;

/// 回合内问题最多等这么久(与 web/bridge_question.rs 的桥问题同档)。
const QUESTION_WAIT_LIMIT: std::time::Duration = std::time::Duration::from_secs(30 * 60);

use crate::agent::*;

impl Agent {
    pub(in crate::agent) async fn chat_with_tools<F>(
        &mut self,
        current_turn_id: &str,
        messages: &mut Vec<ChatMessage>,
        used_tools: &mut Vec<String>,
        persisted_tool_reports: &mut Vec<(String, String)>,
        replay_start: usize,
        base_tool_reports: &[String],
        initial_tool_rounds: usize,
        initial_question_rounds: usize,
        control: Option<&AgentTurnControl>,
        on_event: &mut F,
    ) -> Result<ChatResult>
    where
        F: FnMut(AgentEvent) -> Result<()>,
    {
        let loaded_tools = self.initial_loaded_tools(messages)?;
        self.runtime
            .pending_remote_tool_calls
            .lock()
            .unwrap()
            .clear();
        let mut st = RoundState::new(
            messages,
            replay_start,
            initial_tool_rounds,
            initial_question_rounds,
            loaded_tools,
        );
        loop {
            let rounds_exhausted =
                self.core.max_tool_rounds > 0 && st.tool_round >= self.core.max_tool_rounds;
            let tool_limit_reached = rounds_exhausted || st.repeat_fused;

            self.refresh_tool_catalogs().await;

            // 复读保险丝熔断是端点故障态，收走工具逼模型成文（08-24 设计，不动）；
            // 轮数用完则照带同一份工具、只是不许调（09-24 B5）：tools 排在前缀最前面，
            // 原来一收走，整段缓存在上下文最大的这一轮全部作废。
            let definitions = self.round_tool_definitions(st.repeat_fused);
            let tool_choice_none = rounds_exhausted && !definitions.is_empty();

            on_event(AgentEvent::ReasoningStart {
                received_at: Instant::now(),
            })?;
            let request_messages = self.build_round_request(current_turn_id, messages, &st)?;
            if self.core.config.cache.write_grace_ms > 0 {
                if let Some(previous) = st.last_round_completed_at {
                    let grace =
                        std::time::Duration::from_millis(self.core.config.cache.write_grace_ms);
                    let elapsed = previous.elapsed();
                    if elapsed < grace {
                        tokio::time::sleep(grace - elapsed).await;
                    }
                }
            }
            if self.core.config.cache.keepalive_seconds > 0 && st.responses_continuation.is_none() {
                self.runtime.last_request_snapshot =
                    Some((request_messages.clone(), definitions.clone()));
            }
            let mut model_round = self
                .run_model_round(
                    current_turn_id,
                    request_messages.clone(),
                    definitions,
                    tool_choice_none,
                    st.responses_continuation.as_deref(),
                    control,
                    on_event,
                )
                .await?;
            let round = model_round.outcome.take();
            let round = match round {
                Some(Err(error)) => {
                    match self
                        .recover_round_error(
                            current_turn_id,
                            messages,
                            &mut st,
                            initial_tool_rounds,
                            initial_question_rounds,
                            model_round.streamed,
                            error,
                            on_event,
                        )
                        .await?
                    {
                        RoundRecovery::Retry => continue,
                        RoundRecovery::Fail(error) => return Err(error),
                    }
                }
                Some(Ok(result)) => Some(result),
                None => None,
            };
            let Some(result) = round else {
                self.handle_superseded_round(
                    current_turn_id,
                    messages,
                    base_tool_reports,
                    persisted_tool_reports,
                    &mut st,
                    control,
                    on_event,
                )
                .await?;
                continue;
            };
            self.finish_round_stream(
                current_turn_id,
                messages,
                &request_messages,
                &result,
                &mut model_round,
                &mut st,
                on_event,
            )?;
            if result.tool_calls.is_empty() || !self.core.tools_enabled {
                match self
                    .finish_without_tools(
                        current_turn_id,
                        messages,
                        base_tool_reports,
                        persisted_tool_reports,
                        control,
                        result,
                        &mut st,
                        on_event,
                    )
                    .await?
                {
                    Some(result) => return Ok(result),
                    None => continue,
                }
            }
            if tool_limit_reached {
                return self.finish_at_tool_limit(result, &st, on_event);
            }
            let mut result = result;
            let question_round_allowed = match self
                .execute_round_tool_calls(
                    current_turn_id,
                    messages,
                    used_tools,
                    persisted_tool_reports,
                    &mut result,
                    &mut st,
                    on_event,
                )
                .await?
            {
                ToolBatchOutcome::Truncated => continue,
                ToolBatchOutcome::Executed {
                    question_round_allowed,
                } => question_round_allowed,
            };
            self.after_tool_round(
                current_turn_id,
                messages,
                base_tool_reports,
                persisted_tool_reports,
                control,
                &result,
                question_round_allowed,
                &mut st,
                on_event,
            )
            .await?;
        }
    }

    /// 把「到目前为止调过哪些工具、拿到什么结果」落一次盘。
    ///
    /// 与回合结束时那次写入(`stream.rs`)同一套派生与裁剪,`set_turn_tool_flow`
    /// 是 UPDATE,后写覆盖先写,重复调用幂等。
    ///
    /// **失败只告警不中断回合**:这是一次耐久性检查点,不是回合的产出。为了它
    /// 把一个正在跑的回合掐掉,比丢掉这份检查点糟得多——回合结束时那次写入仍然
    /// 是 `?`,真有持久化问题跑不掉。
    fn checkpoint_tool_flow(&self, turn_id: &str, messages: &[ChatMessage], replay_start: usize) {
        let mut tool_flow = derive_tool_flow(messages, replay_start, false);
        self.append_remote_tool_flow(&mut tool_flow);
        if tool_flow.is_empty() {
            return;
        }
        if let Err(error) = self.state.set_turn_tool_flow(turn_id, &tool_flow) {
            tracing::warn!(
                turn_id,
                error = %error,
                "tool flow checkpoint failed; a crash here would lose what the turn already did"
            );
        }
    }
}

impl Agent {
    /// 把收集到的中转侧工具活动折成一条 remote 轮,附到 tool_flow 尾部。
    /// 检查点与最终写入共用;drain 语义幂等(检查点后新活动继续累积)。
    fn append_remote_tool_flow(&self, tool_flow: &mut Vec<miyu_core::state::ToolFlowRound>) {
        let calls = self
            .runtime
            .pending_remote_tool_calls
            .lock()
            .unwrap()
            .clone();
        if calls.is_empty() {
            return;
        }
        tool_flow.push(miyu_core::state::ToolFlowRound {
            remote: true,
            assistant_content: String::new(),
            assistant_reasoning: None,
            calls,
            ..Default::default()
        });
    }
}

/// 中转侧工具活动的收集:RemoteToolStarted/Finished 的 JSON 载荷折成
/// ToolFlowCall,失败结果加 "tool error: " 前缀让 SafeToolCall 的 ok 判定
/// 复用既有规则。
///
/// 返回值是这次调用该记进 `turns.tool_footprint` 的增量:只在 Finished 且
/// 成功时给(与本地工具"成功才记"同一口径),用 Started 时存下的名字与参数算。
/// 中转轮永远进不了本地那条 `tool_call_footprint` 分支,`live_rounds` 又按
/// 契约过滤 remote 轮——不在这里记,`<modified-files>` 与压后回灌在三条中转线
/// 上就永远是空的(09-10 活库取证 0/42)。
pub(in crate::agent) fn record_remote_tool_chunk(
    chunk: &ChatStreamChunk,
    pending: &std::sync::Mutex<Vec<miyu_core::state::ToolFlowCall>>,
) -> Option<miyu_core::state::ToolFootprint> {
    let parse = |text: &str| serde_json::from_str::<serde_json::Value>(text).ok();
    match chunk.kind {
        ChatStreamKind::RemoteToolStarted => {
            let value = parse(&chunk.text)?;
            let field = |key: &str| {
                value
                    .get(key)
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            };
            pending
                .lock()
                .unwrap()
                .push(miyu_core::state::ToolFlowCall {
                    id: field("id"),
                    name: field("name"),
                    arguments: value
                        .get("input")
                        .map(|input| input.to_string())
                        .unwrap_or_default(),
                    output: String::new(),
                    started_ms: None,
                    finished_ms: None,
                    sub_trace: None,
                    child_session_id: None,
                });
            None
        }
        ChatStreamKind::RemoteToolFinished => {
            let value = parse(&chunk.text)?;
            let id = value
                .get("id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let ok = value
                .get("ok")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(true);
            let output = value
                .get("output")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let mut pending = pending.lock().unwrap();
            let call = pending.iter_mut().rev().find(|call| call.id == id)?;
            call.output = if ok {
                output.to_string()
            } else {
                format!("tool error: {output}")
            };
            ok.then(|| tool_call_footprint(&call.name, &call.arguments))
                .flatten()
        }
        _ => None,
    }
}

/// 一次模型请求的流式块到达时刻:首块到末块的间隔就是「生成时长」,
/// 首字等待和工具执行都不在里面。只有一个块时长为零,视为测不到。
#[derive(Default)]
struct RoundTiming {
    first: Option<Instant>,
    last: Option<Instant>,
}

impl RoundTiming {
    fn observe(&mut self, at: Instant) {
        self.first = Some(self.first.map_or(at, |first| first.min(at)));
        self.last = Some(self.last.map_or(at, |last| last.max(at)));
    }

    fn generation_ms(&self) -> u64 {
        match (self.first, self.last) {
            (Some(first), Some(last)) => last.saturating_duration_since(first).as_millis() as u64,
            _ => 0,
        }
    }
}

fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

/// 给一批刚压进 `messages` 的工具结果盖执行起止:一次调用一段迭代,迭代内压进
/// 的 tool 消息就是它的结果。已经盖过的不重盖。
fn stamp_tool_spans(messages: &mut [ChatMessage], started_ms: u64, finished_ms: u64) {
    for message in messages {
        if message.role == "tool" && message.tool_span_ms.is_none() {
            message.tool_span_ms = Some((started_ms, finished_ms));
        }
    }
}
