//! 一批工具调用的并发执行与排队消息的消费。
//!
//! 输出必须**按请求顺序**映射回去，不能按完成顺序——模型看到的结果和它发出的
//! 调用对不上，后面全乱。
//!
//! 排队消息在工具轮次之间消费：这个时机是刻意的，回合中途插入用户消息只能落在
//! 一个完整的工具轮次边界上，否则会把工具调用和结果拆散。

use super::tool_call::ToolRunResult;
use super::unix_ms;
use crate::agent::*;

/// 一段可并发调用里一个调用的结果：跑出来的东西与起止时间（毫秒）。
pub(in crate::agent) struct SegmentRun {
    pub(in crate::agent) run: ToolRunResult,
    pub(in crate::agent) span_ms: (u64, u64),
}

impl Agent {
    /// 一段相邻的可并发调用一起跑（09-24，同轮工具并发；多个子代理的并行也走这里）。
    ///
    /// 只跑、不回填：结果按调用顺序交回，由调用方逐个 `commit_tool_result`——模型看到的
    /// 结果顺序必须和它发出的调用一致，和谁先跑完无关，回放也才对得上。按
    /// `tools.subagent_concurrency` 分波。全在同一个 task 里轮询、不 spawn：沙盒、工作
    /// 目录、会话这些 task-local 出了这个 task 就拿不到，路径检查会直接放行。
    pub(in crate::agent) async fn run_concurrent_segment<F>(
        &self,
        calls: &[miyu_core::llm::ToolCall],
        used_tools: &mut Vec<String>,
        on_event: &mut F,
    ) -> Result<Vec<SegmentRun>>
    where
        F: FnMut(AgentEvent) -> Result<()>,
    {
        struct Slot {
            position: usize,
            call_id: String,
            event_name: String,
            future: Option<tools::ToolFuture>,
            progress: mpsc::UnboundedReceiver<tools::ToolProgressEvent>,
            started_ms: u64,
        }
        enum WaveEvent {
            Done(usize, Result<String>),
            Progress(usize, tools::ToolProgressEvent),
            Spinner,
        }

        let mut runs: Vec<Option<SegmentRun>> = calls.iter().map(|_| None).collect();
        let limit = self.core.config.tools.subagent_concurrency.max(1);
        let positions = (0..calls.len()).collect::<Vec<_>>();
        for wave in positions.chunks(limit) {
            let mut slots: Vec<Slot> = Vec::new();
            for &position in wave {
                let call = &calls[position];
                used_tools.push(call.function.name.clone());
                let event_name = tool_event_name(&call.function.name, &call.function.arguments);
                on_event(AgentEvent::ToolCall {
                    call_id: call.id.clone(),
                    name: event_name.clone(),
                    arguments: call.function.arguments.clone(),
                })?;
                let (progress_tx, progress_rx) = mpsc::unbounded_channel();
                let future = {
                    let tools = self.tools.lock().unwrap();
                    tools.call_with_progress_future(
                        &call.function.name,
                        &call.function.arguments,
                        progress_tx,
                        &crate::tools::GuardCtx {
                            used_tools: used_tools.as_slice(),
                        },
                    )
                };
                let started_ms = unix_ms();
                match future {
                    Ok(future) => slots.push(Slot {
                        position,
                        call_id: call.id.clone(),
                        event_name,
                        future: Some(future),
                        progress: progress_rx,
                        started_ms,
                    }),
                    Err(err) => {
                        runs[position] = Some(SegmentRun {
                            run: Err(err),
                            span_ms: (started_ms, started_ms),
                        })
                    }
                }
            }
            let mut remaining = slots.len();
            // 前台子代理的进度收成状态再发（会话项目第 4 段之二，见 `subagent_feed`）。
            let mut feeds = super::subagent_feed::SubagentFeeds::default();
            let mut spinner_interval = tokio::time::interval(self.core.spinner_interval);
            spinner_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            spinner_interval.tick().await;
            while remaining > 0 {
                let event = {
                    let poll_slots = std::future::poll_fn(|context| {
                        for (index, slot) in slots.iter_mut().enumerate() {
                            if let std::task::Poll::Ready(Some(progress)) =
                                slot.progress.poll_recv(context)
                            {
                                return std::task::Poll::Ready(WaveEvent::Progress(
                                    index, progress,
                                ));
                            }
                            if let Some(future) = slot.future.as_mut() {
                                if let std::task::Poll::Ready(result) =
                                    future.as_mut().poll(context)
                                {
                                    slot.future = None;
                                    return std::task::Poll::Ready(WaveEvent::Done(index, result));
                                }
                            }
                        }
                        std::task::Poll::Pending
                    });
                    tokio::select! {
                        event = poll_slots => event,
                        _ = spinner_interval.tick() => WaveEvent::Spinner,
                    }
                };
                match event {
                    WaveEvent::Spinner => {
                        on_event(AgentEvent::SpinnerTick)?;
                        for slot in slots.iter().filter(|slot| slot.future.is_some()) {
                            feeds.flush(on_event, &slot.call_id, &slot.event_name)?;
                        }
                    }
                    WaveEvent::Progress(index, progress) => {
                        feeds.forward(
                            on_event,
                            &slots[index].call_id,
                            &slots[index].event_name,
                            progress,
                        )?;
                    }
                    WaveEvent::Done(index, result) => {
                        remaining -= 1;
                        while let Ok(progress) = slots[index].progress.try_recv() {
                            feeds.forward(
                                on_event,
                                &slots[index].call_id,
                                &slots[index].event_name,
                                progress,
                            )?;
                        }
                        feeds.flush(on_event, &slots[index].call_id, &slots[index].event_name)?;
                        runs[slots[index].position] = Some(SegmentRun {
                            run: result,
                            span_ms: (slots[index].started_ms, unix_ms()),
                        });
                    }
                }
            }
        }
        Ok(runs
            .into_iter()
            .map(|run| run.expect("every call in a concurrent segment runs once"))
            .collect())
    }

    pub(in crate::agent) async fn consume_queued_prompts<F>(
        &mut self,
        current_turn_id: &str,
        messages: &mut Vec<ChatMessage>,
        queued: Vec<QueuedPrompt>,
        preceding_assistant: (Option<&str>, Option<&str>, Option<&str>, Option<&str>),
        checkpoint: TurnRedoCheckpointPayload,
        control: &AgentTurnControl,
        on_event: &mut F,
    ) -> Result<()>
    where
        F: FnMut(AgentEvent) -> Result<()>,
    {
        on_event(AgentEvent::FlushJournal)?;
        // 排队消息=用户更新了请求,平台生图配额随之重置(非平台回合 no-op)。
        miyu_base::workspace::reset_image_gen_limit();
        let mut prepared = Vec::with_capacity(queued.len());
        for prompt in queued {
            let images = self.queued_prompt_images(&prompt)?;
            let input = self.prepare_user_input(&prompt.content, &images).await?;
            prepared.push((prompt, input));
        }

        let mode = control.lane();
        if self.persona_lane() != mode {
            self.switch_lane(mode, control.tools(mode));
            self.refresh_system_prompt()?;
        }
        replace_request_system_prompt(messages, &self.system_prompt);

        // 瞬态尾巴挂在 followup 正文之后,并与正文一起化石化(v31):runtime
        // 按首轮同一口径"变了才追加"(只挂第一条),图片路径/context-images
        // 提示原样跟随。live 推进 messages 的就是这同一份 tail,活体与化石
        // 逐字节一致——少了这一步,下一轮回放在 followup 处比活体短一截,
        // 缓存前缀与 CLI 续传链都在这里掰断(09-04 codex 线实证)。
        let runtime = runtime_context(self.input.platform_context.is_some());
        let runtime_block = (last_fossil_with_prefix(messages, "<runtime ")
            != Some(runtime.as_str()))
        .then(|| ChatMessage::turn_context(runtime));
        let mut consumed = Vec::with_capacity(prepared.len());
        let mut tails: Vec<Vec<ChatMessage>> = Vec::with_capacity(prepared.len());
        for (index, (prompt, input)) in prepared.iter().enumerate() {
            let mut tail = Vec::new();
            if index == 0 {
                tail.extend(runtime_block.clone());
            }
            tail.extend(fossil_context_messages(&input.hints));
            consumed.push((
                prompt.prompt_id.clone(),
                input.content.clone(),
                serde_json::to_string(&tail)?,
            ));
            tails.push(tail);
        }
        self.state.consume_queued_prompts_with_checkpoint(
            current_turn_id,
            &consumed,
            preceding_assistant
                .0
                .filter(|content| !content.trim().is_empty()),
            preceding_assistant
                .1
                .filter(|reasoning| !reasoning.trim().is_empty()),
            preceding_assistant
                .2
                .filter(|provider_id| !provider_id.trim().is_empty()),
            preceding_assistant
                .3
                .filter(|model| !model.trim().is_empty()),
            checkpoint,
        )?;
        for (prompt, _) in &prepared {
            if let Some(context) = self.input.platform_context.clone() {
                let files = context.take_queued_files(&prompt.prompt_id);
                if !files.is_empty() {
                    self.input.context_files.extend(files);
                    self.set_platform_context_files(context, self.input.context_files.clone());
                }
            }
        }
        on_event(AgentEvent::QueuedPromptsConsumed {
            prompt_ids: consumed.iter().map(|(id, _, _)| id.clone()).collect(),
            mode,
            provider_id: preceding_assistant.2.map(str::to_string),
            model: preceding_assistant.3.map(str::to_string),
        })?;

        for ((prompt, input), tail) in prepared.into_iter().zip(tails) {
            let mut message = input.message;
            // 带图的插话在 tool_flow 里只记这条排队消息的 id(见 `FlowMessage`)。
            message.followup_prompt = Some(prompt.prompt_id);
            messages.push(message);
            messages.extend(tail);
        }
        Ok(())
    }

    /// 浮动尾部人格提醒,所有会话形态(终端/WebUI/平台)一致生效:命中
    /// 缓存时只是一次小文件读,缓存未建时对同一 client 蒸馏一次(每份
    /// 人格内容一生只发生一次)。蒸馏失败降级为无提醒,绝不阻断回合。
    pub(in crate::agent) async fn resolve_persona_reminder(&self) -> Option<String> {
        // dev 无人格,自然无防失忆提醒(中途切到 dev 时子系统快照还是原人格的,
        // 所以这一位不能并进快照)。
        if self.core.dev {
            return None;
        }
        // 人格意愿 × `prompt.persona_reminder`,构造期折进子系统快照。
        if !self.core.subsystems.persona_reminder {
            return None;
        }
        match persona_hint::resolve(&self.core.config, &self.core.paths, &self.client).await {
            Ok(reminder) => reminder,
            Err(error) => {
                tracing::warn!(error = %error, "persona reminder distillation failed");
                None
            }
        }
    }
}
