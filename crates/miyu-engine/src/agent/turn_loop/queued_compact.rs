//! 回合里排队的 `/compact`（09-25，用户：「像 followup 消息一样排队」）。
//!
//! 回合跑着的时候敲 `/compact`，原来要么被客户端拦下、要么撞守护进程一句 busy。现在守护
//! 进程把它记在这一轮的控制句柄上（[`TurnCompactRequest`]），回合循环在接插话的那两处——
//! 一批工具跑完、模型要收尾时——看一眼，立着就把**本轮之前**的历史压掉：本轮的用户消息和
//! 已经跑过的工具往来原样留在尾巴上，拼回新前缀后面接着跑（和溢出恢复同一个拼法）。
//!
//! [`TurnCompactRequest`]: crate::agent::TurnCompactRequest

use super::round_state::RoundState;
use crate::agent::*;

impl Agent {
    /// 这一轮排着一次压缩就做掉。压缩失败不算回合失败：说一声，接着跑。`splice` 为假时只压库
    /// 里的历史、不动手里的消息数组（模型要收尾了，下一轮从库里重建）。
    pub(super) async fn run_queued_compact<F>(
        &mut self,
        current_turn_id: &str,
        messages: &mut Vec<ChatMessage>,
        st: &mut RoundState,
        control: Option<&AgentTurnControl>,
        splice: bool,
        on_event: &mut F,
    ) -> Result<()>
    where
        F: FnMut(AgentEvent) -> Result<()>,
    {
        if !control.is_some_and(AgentTurnControl::take_compact_request) {
            return Ok(());
        }
        let Some(window) = self.context_window() else {
            tracing::warn!("queued compact skipped: the context window is unknown");
            return Ok(());
        };
        // 手动压缩不看水位，这里只借它算 reserved_tokens（同 `compact_now`）。
        let check = overflow::OverflowCheck::new(Some(window), self.core.compact_at_ratio, None);
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
        let compacted = {
            let mut on_chunk = |chunk: ChatStreamChunk| on_event(AgentEvent::CompactChunk(chunk));
            let fork_builder = |fold_ids: &[String]| -> Result<compact::CompactForkParts> {
                Ok((
                    self.compact_fork_prefix(fold_ids)?,
                    self.live_tool_definitions()?,
                ))
            };
            let fork_builder: Option<compact::CompactForkBuilder<'_>> = self
                .core
                .config
                .context
                .compact_cache_reuse
                .then_some(&fork_builder);
            compactor
                .perform_compact(true, false, fork_builder, &mut on_chunk)
                .await
        };
        on_event(AgentEvent::CompactEnd)?;
        let result = match compacted {
            Ok(Some(result)) => result,
            Ok(None) => return Ok(()),
            Err(error) => {
                tracing::warn!(error = %error, "queued compact failed; the turn goes on");
                return Ok(());
            }
        };
        self.state.add_auxiliary_usage(
            &result.usage,
            miyu_core::state::UsageMeta {
                source: self.usage_source(),
                provider: result.provider_id.as_deref(),
                model: None,
                kind: None,
            },
        )?;
        if splice {
            self.splice_compacted_prefix(current_turn_id, messages, st)?;
            // 前缀换了，Responses 续传链接不上了：下一轮发全量。
            st.responses_continuation = None;
            st.continuation_context = None;
        }
        tracing::info!(
            folded = result.folded_turns,
            kept = result.kept_turns,
            "queued compact folded the history before the running turn"
        );
        Ok(())
    }

    /// 把压缩后重建的历史前缀拼到本轮用户消息前面。本轮的活尾巴（用户输入、运行时戳、提示、
    /// 已经跑过的工具往来）逐字节不动。
    pub(super) fn splice_compacted_prefix(
        &self,
        current_turn_id: &str,
        messages: &mut Vec<ChatMessage>,
        st: &mut RoundState,
    ) -> Result<()> {
        let user_index = live_user_index(messages, st.replay_start)
            .unwrap_or_else(|| st.replay_start.min(messages.len()));
        let (rebuilt, rebuilt_user_index) = self.chat_messages(current_turn_id, "")?;
        let tail = messages.split_off(user_index);
        messages.clear();
        messages.extend(rebuilt.into_iter().take(rebuilt_user_index));
        messages.extend(tail);
        // 活跃轮边界随尾巴整体平移:新前缀长 + 尾内偏移。
        st.replay_start = rebuilt_user_index + (st.replay_start - user_index);
        st.continuation_input_start = messages.len();
        Ok(())
    }
}
