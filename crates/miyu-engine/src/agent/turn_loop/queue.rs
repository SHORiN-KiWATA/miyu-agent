//! 回合中途排队消息的消费:这一轮被同一回合的更新超越时,把排队的提示词接进对话再发。
//! 09-17 从 `chat_with_tools` 里抽出。

use super::round_state::RoundState;
use crate::agent::*;

impl Agent {
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn handle_superseded_round<F>(
        &mut self,
        current_turn_id: &str,
        messages: &mut Vec<ChatMessage>,
        base_tool_reports: &[String],
        persisted_tool_reports: &mut Vec<(String, String)>,
        st: &mut RoundState,
        control: Option<&AgentTurnControl>,
        on_event: &mut F,
    ) -> Result<()>
    where
        F: FnMut(AgentEvent) -> Result<()>,
    {
        if let Some(control) = control {
            if let Some(generation) = control.pending_supersede_generation() {
                control.mark_supersede_seen(generation);
            }
        }
        let queued = self.state.load_queued_prompts()?;
        if queued.is_empty() {
            return Ok(());
        }
        let prompt_ids = queued
            .iter()
            .map(|prompt| prompt.prompt_id.clone())
            .collect::<Vec<_>>();
        on_event(AgentEvent::GenerationSuperseded { prompt_ids })?;
        let checkpoint = redo_checkpoint_payload(
            messages,
            st.replay_start,
            base_tool_reports,
            persisted_tool_reports,
            st.tool_round,
            st.question_rounds,
        );
        let continuation_context_index = st.responses_continuation.as_ref().map(|_| {
            st.continuation_context
                .as_ref()
                .map(|(index, _)| *index)
                .unwrap_or(messages.len())
        });
        self.consume_queued_prompts(
            current_turn_id,
            messages,
            queued,
            (None, None, None, None),
            checkpoint,
            control.expect("supersede requires turn control"),
            on_event,
        )
        .await?;
        // 插话按活体位置记进 flow(见 `after_tool_round` 末尾的检查点)。
        self.checkpoint_tool_flow(current_turn_id, messages, st.replay_start);
        if let Some(index) = continuation_context_index {
            st.continuation_context = Some((
                index,
                vec![
                    ChatMessage::turn_context(continuation_system_prompt(
                        &self.system_prompt,
                        self.core.dev,
                    )),
                    ChatMessage::turn_context(runtime_context(
                        self.input.platform_context.is_some(),
                    )),
                ],
            ));
        }
        Ok(())
    }

    /// 一批工具跑完之后:tool_flow 落一次盘、注入 goal 侧挂起的步间指令、提问轮不占工具
    /// 轮数,最后把回合中途排队的提示词接进对话(被超越的话不带上一轮正文)。
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn after_tool_round<F>(
        &mut self,
        current_turn_id: &str,
        messages: &mut Vec<ChatMessage>,
        base_tool_reports: &[String],
        persisted_tool_reports: &mut Vec<(String, String)>,
        control: Option<&AgentTurnControl>,
        result: &ChatResult,
        question_round_allowed: bool,
        st: &mut RoundState,
        on_event: &mut F,
    ) -> Result<()>
    where
        F: FnMut(AgentEvent) -> Result<()>,
    {
        // 本轮工具结果已经全部进了 `messages`,趁这里把 tool_flow 落一次盘。
        //
        // 崩溃恢复时正文和工具报告都能从流水物化出来(`interrupted_projection`
        // 与追加型的 `turn_tool_reports`),唯独 tool_flow 不能——它以前只在整
        // 个回合跑完后写一次(`stream.rs` 的 `set_turn_tool_flow`),进程中途死
        // 掉这份就从没存在过。而 tool_flow 正是 `history.rs` 回放给模型的那份
        // 「调过哪些工具、拿到什么结果」,丢了它模型下一轮只看到半截文字,会把
        // 已经跑过的命令、读过的文件原样再来一遍。
        self.checkpoint_tool_flow(current_turn_id, messages, st.replay_start);
        // 下面插进对话的 goal 通知与插话也要按活体位置记进 flow(被打断时回放
        // 原样放回、续上上游的前缀缓存),插了就在末尾再落一次。
        let inserted_from = messages.len();
        // goal 侧挂起的步间指令在这里取走注入:自主轮报了完成/受阻之后的
        // 收尾指令(不注入的话,工具返回了 JSON,模型没有理由再说什么,
        // 一个跑了十几轮的目标就无声停住);以及人在续轮中途 `/goal edit`
        // 之后的目标变更通知(不注入的话,模型整轮都在推进旧目标)。
        if let Some(session) = miyu_base::workspace::try_session() {
            if let Some(wrapup) = crate::tools::goal::take_turn_notices(&session) {
                // `turn_context` 而不是 `system`：中途插一条 system 会把
                // 提供方模板里的 system 前置块整体挪位，前缀缓存全废
                // （`ChatMessage::turn_context` 的注释里有实测数据）。
                messages.push(ChatMessage::turn_context(wrapup));
            }
        }
        if question_round_allowed {
            st.tool_round = st.tool_round.saturating_sub(1);
        }
        if let Some(control) = control {
            if let Some(queue_ingress) = control.queue_ingress.as_ref() {
                queue_ingress.wait_for_reserved_ingress().await;
            }
            let queued = self.state.load_queued_prompts()?;
            if !queued.is_empty() {
                let supersede_generation = control.pending_supersede_generation();
                if supersede_generation.is_some() {
                    let prompt_ids = queued
                        .iter()
                        .map(|prompt| prompt.prompt_id.clone())
                        .collect();
                    on_event(AgentEvent::GenerationSuperseded { prompt_ids })?;
                }
                let checkpoint = redo_checkpoint_payload(
                    messages,
                    st.replay_start,
                    base_tool_reports,
                    persisted_tool_reports,
                    st.tool_round,
                    st.question_rounds,
                );
                let preceding_assistant = if supersede_generation.is_some() {
                    (None, None, None, None)
                } else {
                    (
                        Some(result.content.as_str()),
                        result.reasoning.as_deref(),
                        result.provider_id.as_deref(),
                        result.model.as_deref(),
                    )
                };
                let continuation_context_index = st.responses_continuation.as_ref().map(|_| {
                    st.continuation_context
                        .as_ref()
                        .map(|(index, _)| *index)
                        .unwrap_or(messages.len())
                });
                self.consume_queued_prompts(
                    current_turn_id,
                    messages,
                    queued,
                    preceding_assistant,
                    checkpoint,
                    control,
                    on_event,
                )
                .await?;
                if let Some(index) = continuation_context_index {
                    st.continuation_context = Some((
                        index,
                        vec![
                            ChatMessage::turn_context(continuation_system_prompt(
                                &self.system_prompt,
                                self.core.dev,
                            )),
                            ChatMessage::turn_context(runtime_context(
                                self.input.platform_context.is_some(),
                            )),
                        ],
                    ));
                }
                if let Some(generation) = supersede_generation {
                    control.mark_supersede_seen(generation);
                }
            }
        }
        if messages.len() > inserted_from {
            self.checkpoint_tool_flow(current_turn_id, messages, st.replay_start);
        }
        // 排队的 `/compact` 和插话同一个时机（09-25）：压掉本轮之前的历史，拼回来接着跑。
        self.run_queued_compact(current_turn_id, messages, st, control, true, on_event)
            .await
    }
}
