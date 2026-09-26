//! 一轮模型请求出错之后的自愈:Responses 续传被拒就清续传重发全量;上下文溢出且
//! 一个字都没流出过就压缩一次再重试(每回合只此一次)。都救不了才把原错误交回。
//! 09-17 从 `chat_with_tools` 里抽出。

use super::round_state::RoundState;
use crate::agent::*;

pub(super) enum RoundRecovery {
    /// 已经处理好,回到循环顶再发一轮。
    Retry,
    /// 救不了:把原错误原样往上抛。
    Fail(anyhow::Error),
}

impl Agent {
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn recover_round_error<F>(
        &mut self,
        current_turn_id: &str,
        messages: &mut Vec<ChatMessage>,
        st: &mut RoundState,
        initial_tool_rounds: usize,
        initial_question_rounds: usize,
        streamed: bool,
        error: anyhow::Error,
        on_event: &mut F,
    ) -> Result<RoundRecovery>
    where
        F: FnMut(AgentEvent) -> Result<()>,
    {
        // Responses 续传自愈(任务#16):上游不支持
        // previous_response_id 时,工具轮第二步只发增量会撞
        // "No tool call found for tool output" 类 400。此时清
        // 续传重发全量(messages 里工具结果已齐,无状态回放
        // 完整),并让客户端持久记该供应商不可续传——本会话
        // 与后续会话都不再发增量。
        if st.responses_continuation.is_some()
            && miyu_core::llm::is_responses_continuation_unsupported_error(&error)
        {
            tracing::warn!(
                error = %error,
                "responses continuation rejected; retrying this round with full stateless input"
            );
            self.client.mark_responses_continuation_unsupported();
            st.responses_continuation = None;
            return Ok(RoundRecovery::Retry);
        }
        // Passive overflow trigger (compact-and-retry). Only at
        // the turn's initial request, before any assistant output
        // was streamed: mid-loop the live tool exchange is not
        // rebuildable from the DB, and a partially shown answer
        // must not be silently retried (opencode's
        // hasAssistantStarted guard).
        let initial_request = st.tool_round == initial_tool_rounds
            && st.question_rounds == initial_question_rounds
            && st.responses_continuation.is_none()
            && !streamed;
        let window = self.context_window();
        if initial_request
            && !st.overflow_recovery_attempted
            && window.is_some()
            && miyu_core::llm::is_context_overflow_error(&error)
        {
            st.overflow_recovery_attempted = true;
            let window = window.unwrap();
            let check = overflow::OverflowCheck::new(Some(window), self.core.trim_at_ratio, None);
            on_event(AgentEvent::CompactStart)?;
            let compactor = compact::Compactor::new(
                self.client.clone(),
                self.state.clone(),
                window,
                check.reserved_tokens,
                self.compact_tail_budget(window),
                self.preset_dialogs.len(),
            )
            .with_extras(self.compact_extras_policy())
            .excluding_running_turn(current_turn_id);
            let mut on_compact_chunk =
                |chunk: ChatStreamChunk| on_event(AgentEvent::CompactChunk(chunk));
            // No fork here: a fork of an overflowing conversation
            // overflows identically — recovery must use the
            // isolated serialized path.
            let compacted = compactor
                .perform_compact(true, true, None, &mut on_compact_chunk)
                .await;
            on_event(AgentEvent::CompactEnd)?;
            if let Ok(Some(compact_result)) = compacted {
                self.state.add_auxiliary_usage(
                    &compact_result.usage,
                    miyu_core::state::UsageMeta {
                        source: self.usage_source(),
                        provider: compact_result.provider_id.as_deref(),
                        model: None,
                        kind: None,
                    },
                )?;
                self.splice_compacted_prefix(current_turn_id, messages, st)?;
                tracing::info!(
                    folded = compact_result.folded_turns,
                    kept = compact_result.kept_turns,
                    "context overflow recovered by compact-and-retry"
                );
                return Ok(RoundRecovery::Retry);
            }
            if let Err(compact_error) = compacted {
                tracing::warn!(
                    error = %compact_error,
                    "compact-and-retry failed; surfacing the original overflow"
                );
            }
        }
        Ok(RoundRecovery::Fail(error))
    }
}
