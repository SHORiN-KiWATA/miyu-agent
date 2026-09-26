//! 单个工具调用的执行(`chat_with_tools` 每批里的一个):`ask_question` 走回合内提问通道,
//! 其它工具真执行——带进度事件、子代理子过程的限流检查点、桩工具的契约补提示、内联媒体
//! 落库与视觉退回。09-17 从 `tool_exec.rs` 再拆一层。

use super::round_state::RoundState;

/// 一次工具调用跑出来的东西：`Ok` 是工具输出，`Err` 是没跑成的错误（收尾时补契约、报失败）。
pub(super) type ToolRunResult = std::result::Result<String, anyhow::Error>;
use super::QUESTION_WAIT_LIMIT;
use crate::agent::*;

impl Agent {
    /// `ask_question`:一批只许一个、每回合有上限;答案(或超时 / 关闭)当工具输出回灌。
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn run_ask_question<F>(
        &mut self,
        current_turn_id: &str,
        messages: &mut Vec<ChatMessage>,
        call: ToolCall,
        call_id: String,
        event_name: String,
        question_round_allowed: bool,
        on_event: &mut F,
    ) -> Result<()>
    where
        F: FnMut(AgentEvent) -> Result<()>,
    {
        if !question_round_allowed {
            let output = format!(
                "tool error: ask_question exceeded the per-turn limit of {MAX_QUESTION_ROUNDS_PER_TURN}"
            );
            on_event(AgentEvent::ToolResult {
                call_id: call_id.clone(),
                name: event_name.clone(),
                ok: false,
                output: output.clone(),
            })?;
            messages.push(ChatMessage::tool(call.id, output));
            return Ok(());
        }
        let request = match QuestionRequest::parse(&call.function.arguments) {
            Ok(request) => request,
            Err(err) => {
                // 报错要说自己真正知道的:实测模型看到裸的 serde 消息
                // （"invalid type: string, expected a sequence"）之后
                // 反复重试同样的形状,最后判定成「接口不支持」放弃。
                // 补一句期望形状,它才知道该改什么。
                let output = format!(
                    "tool error: invalid ask_question request: {err}\n\
                     expected {{\"questions\": [{{\"header\": ..., \"question\": ..., \
                     \"options\": [{{\"label\": ..., \"description\": ...}}]}}]}} \
                     — questions and options must be real JSON arrays, not strings"
                );
                on_event(AgentEvent::ToolResult {
                    call_id: call_id.clone(),
                    name: event_name.clone(),
                    ok: false,
                    output: output.clone(),
                })?;
                messages.push(ChatMessage::tool(call.id, output));
                return Ok(());
            }
        };
        let (response_tx, response_rx) = oneshot::channel();
        on_event(AgentEvent::AskQuestion {
            call_id: call_id.clone(),
            request: request.clone(),
            responder: response_tx,
        })?;
        // 没人回答也得有个头:一次性客户端(shellhook)断线后没人能再
        // 应答,回合会永远卡在 running,被历史组装跳过——用户看到的
        // 是"上一轮失忆"(09-09)。超时当无人应答,回合正常收尾。
        let response = match tokio::time::timeout(QUESTION_WAIT_LIMIT, response_rx).await {
            Ok(response) => response.unwrap_or(QuestionResponse::Cancelled),
            Err(_) => {
                QuestionResponse::Unavailable("nobody answered within the time limit".to_string())
            }
        };
        let output = match response {
            QuestionResponse::Answered(answers) => {
                let exchange = QuestionExchange::new(request, answers)?;
                self.state
                    .append_question_exchange(current_turn_id, &exchange)?;
                answered_tool_output(&exchange)
            }
            QuestionResponse::Closed => closed_tool_output(),
            QuestionResponse::Cancelled => return Err(QuestionCancelled.into()),
            QuestionResponse::Unavailable(reason) => unavailable_tool_output(&reason),
        };
        messages.push(ChatMessage::tool(call.id, output.clone()));
        on_event(AgentEvent::ToolResult {
            call_id: call_id.clone(),
            name: event_name,
            ok: true,
            output,
        })?;
        Ok(())
    }

    /// 串行路径：真执行一个工具，再把结果压进 `messages`(失败同样压一条 tool 消息)。
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn run_tool_call<F>(
        &mut self,
        current_turn_id: &str,
        messages: &mut Vec<ChatMessage>,
        used_tools: &mut Vec<String>,
        persisted_tool_reports: &mut Vec<(String, String)>,
        call: ToolCall,
        call_id: String,
        event_name: String,
        st: &mut RoundState,
        on_event: &mut F,
        companions: &mut Vec<ChatMessage>,
    ) -> Result<()>
    where
        F: FnMut(AgentEvent) -> Result<()>,
    {
        used_tools.push(call.function.name.clone());
        let run = self
            .execute_tool_call(
                current_turn_id,
                messages,
                used_tools,
                &call,
                &call_id,
                &event_name,
                st,
                on_event,
            )
            .await?;
        self.commit_tool_result(
            current_turn_id,
            messages,
            persisted_tool_reports,
            call,
            call_id,
            event_name,
            st,
            on_event,
            run,
            companions,
        )
        .await
    }

    /// 桩工具失败时把真契约补进返回体(每个工具每回合只补一次)。
    fn attach_stub_contract(
        &self,
        st: &mut RoundState,
        tool_name: &str,
        message: String,
    ) -> String {
        if !tools::is_stub_loading_mode(&self.core.config.tools.loading_mode) {
            return message;
        }
        if !st.contract_hinted.insert(tool_name.to_string()) {
            return message;
        }
        let tools = self.tools.lock().unwrap();
        if !tools.is_stub_presented(tool_name) {
            return message;
        }
        match tools.contract_text(tool_name) {
            Some(contract) => format!(
                "{message}\n\nThis tool was declared with an empty parameter shell, so its real schema follows. Call it again with these arguments at the top level.{contract}"
            ),
            None => message,
        }
    }

    /// 只跑工具：进度事件、转圈、子代理子过程的限流检查点。回填交给 `commit_tool_result`
    /// ——并发的一段也是先一起跑、再按调用顺序逐个收尾（09-24，同轮工具并发）。
    #[allow(clippy::too_many_arguments)]
    async fn execute_tool_call<F>(
        &mut self,
        current_turn_id: &str,
        messages: &mut Vec<ChatMessage>,
        used_tools: &[String],
        call: &ToolCall,
        call_id: &str,
        event_name: &str,
        st: &mut RoundState,
        on_event: &mut F,
    ) -> Result<ToolRunResult>
    where
        F: FnMut(AgentEvent) -> Result<()>,
    {
        // 模式级 ReadOnly 权限门随闲聊模式一并删除:拒绝层现在是
        // registry 的单调 guard(软失败),不可用工具靠 registry 组合
        // 不注册(平台 restricted 同理),未知工具在分发处软失败。
        let (progress_tx, mut progress_rx) = mpsc::unbounded_channel();
        let tool_future = {
            let tools = self.tools.lock().unwrap();
            // AUR 互斥等回合级规则已迁入 guard 层,凭 used_tools 上下文判定。
            tools.call_with_progress_future(
                &call.function.name,
                &call.function.arguments,
                progress_tx,
                &crate::tools::GuardCtx { used_tools },
            )
        };
        let tool_future = match tool_future {
            Ok(f) => f,
            Err(err) => return Ok(Err(err)),
        };
        tokio::pin!(tool_future);
        let mut spinner_interval = tokio::time::interval(self.core.spinner_interval);
        spinner_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        spinner_interval.tick().await;
        // 前台子代理的进度收成状态行那一行再发（`subagent_feed.rs`）。子会话 id 一到就落一次
        // 检查点：前台子代理跑到一半刷新页面时，卡片靠落了库的 `child_session_id` 链到那条
        // 会话（原来这里按标记流限流落 `sub_trace`，在父会话里把子过程再画一遍）。
        let mut feeds = super::subagent_feed::SubagentFeeds::default();
        let run = loop {
            tokio::select! {
                result = &mut tool_future => {
                    while let Ok(progress) = progress_rx.try_recv() {
                        feeds.forward(on_event, call_id, event_name, progress)?;
                    }
                    feeds.flush(on_event, call_id, event_name)?;
                    break result;
                }
                Some(progress) = progress_rx.recv() => {
                    if feeds.forward(on_event, call_id, event_name, progress)? {
                        self.checkpoint_tool_flow(current_turn_id, messages, st.replay_start);
                    }
                }
                _ = spinner_interval.tick() => {
                    on_event(AgentEvent::SpinnerTick)?;
                    feeds.flush(on_event, call_id, event_name)?;
                }
            }
        };
        Ok(run)
    }

    /// 收尾：失败补契约并报结果；外溢、复读闸记账、内联媒体（落库、视觉退回）、load_tools
    /// 记账、推 tool 消息、足迹、结果事件、报告。一批里按调用顺序逐个调用——实时发出
    /// 去的顺序必须和回放一样，缓存前缀才不在这里断。
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn commit_tool_result<F>(
        &mut self,
        current_turn_id: &str,
        messages: &mut Vec<ChatMessage>,
        persisted_tool_reports: &mut Vec<(String, String)>,
        call: ToolCall,
        call_id: String,
        event_name: String,
        st: &mut RoundState,
        on_event: &mut F,
        run: ToolRunResult,
        companions: &mut Vec<ChatMessage>,
    ) -> Result<()>
    where
        F: FnMut(AgentEvent) -> Result<()>,
    {
        let (output, tool_succeeded) = match run {
            Ok(output) => (output, true),
            Err(err) => {
                let output = self.attach_stub_contract(
                    st,
                    &call.function.name,
                    format!("tool error: {err}"),
                );
                on_event(AgentEvent::ToolResult {
                    call_id: call_id.clone(),
                    name: event_name.clone(),
                    ok: false,
                    output: output.clone(),
                })?;
                (output, false)
            }
        };
        let inline_media = if tool_succeeded {
            inline_media_from_tool_result(&call.function.name, &output)
        } else {
            Vec::new()
        };
        let model_output = self
            .spill_tool_output(current_turn_id, &call.id, &call.function.name, &output)
            .unwrap_or_else(|| output.clone());
        // 复读闸记账:下一轮同参跳过时按键回灌这份字节。(dsh 式
        // advisory 重复提醒于 08-24 整体退役:222 连发与 08-23/24
        // 两次故障实录证明提示文本对故障态模型无效,防线全部交给
        // 结构化的 repeat_gate。)
        st.repeat_gate
            .record_output(&call.function.name, &call.function.arguments, &model_output);
        // tool 消息要等媒体块定下来再推:图直接进它的内容 parts(供应商
        // 不认时才退回"之后补一条用户消息")。
        let tool_message = ChatMessage::tool(call.id.clone(), model_output);
        if tool_succeeded && call.function.name == "load_tools" {
            let loaded = loaded_items_from_output(&output);
            for name in &loaded.tools {
                st.loaded_tools.insert(name.clone());
            }
            if self.core.config.tools.persist_loaded_tools {
                self.state
                    .add_session_loaded_tools(&loaded.tools, Some(current_turn_id))?;
                self.state
                    .add_session_loaded_targets(&loaded.targets, Some(current_turn_id))?;
            }
        }
        let stamped = if !inline_media.is_empty() {
            let supports_vision = self.current_model_supports_vision();
            let needs_fallback = !supports_vision
                && inline_media
                    .iter()
                    .any(|item| item.kind == miyu_core::state::INLINE_MEDIA_KIND_IMAGE);
            let uses_vision_fallback = needs_fallback && self.core.config.plugins.vision.enabled;
            if needs_fallback {
                let message = if self.core.config.plugins.vision.enabled {
                    if miyu_base::i18n::is_zh() {
                        "视觉分析."
                    } else {
                        "Vision analysis."
                    }
                } else if miyu_base::i18n::is_zh() {
                    "当前模型不支持图片，且未启用视觉模型，无法分析这张图片。"
                } else {
                    "The current model does not support images and the vision plugin is disabled, so the image cannot be analyzed."
                };
                on_event(AgentEvent::ToolProgress {
                    call_id: call_id.clone(),
                    name: event_name.clone(),
                    message: message.to_string(),
                })?;
            }
            let items = if uses_vision_fallback {
                let describe_future = self.describe_inline_media(inline_media);
                tokio::pin!(describe_future);
                let mut spinner_interval = tokio::time::interval(self.core.spinner_interval);
                spinner_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                spinner_interval.tick().await;
                let mut progress_interval = tokio::time::interval(Duration::from_millis(900));
                progress_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                progress_interval.tick().await;
                let mut progress_tick = 0usize;
                loop {
                    tokio::select! {
                        result = &mut describe_future => {
                            break result?;
                        }
                        _ = progress_interval.tick() => {
                            progress_tick = progress_tick.wrapping_add(1);
                            on_event(AgentEvent::ToolProgress {
                                call_id: call_id.clone(),
                                name: event_name.clone(),
                                message: vision_analysis_progress(progress_tick),
                            })?;
                        }
                        _ = spinner_interval.tick() => {
                            on_event(AgentEvent::SpinnerTick)?;
                        }
                    }
                }
            } else if needs_fallback {
                Vec::new()
            } else {
                inline_media
            };
            // 先落库再推进对话:重放读的就是这批字节,活体与重放
            // 同源(1.2 化石化)。
            let stamped = items
                .into_iter()
                .enumerate()
                .map(|(seq, mut item)| {
                    item.call_id = call.id.clone();
                    item.seq = seq as i64;
                    item
                })
                .collect::<Vec<_>>();
            if !stamped.is_empty() {
                self.state
                    .save_turn_inline_media(current_turn_id, &stamped)?;
            }
            stamped
        } else {
            Vec::new()
        };
        push_tool_result_with_media(
            messages,
            tool_message,
            &stamped,
            self.core.config.active_pool_tool_result_media(),
            companions,
        );
        if tool_succeeded {
            let result_ok = tool_output_succeeded(&output);
            if result_ok {
                if let Some(delta) =
                    tool_call_footprint(&call.function.name, &call.function.arguments)
                {
                    self.state.merge_turn_footprint(current_turn_id, &delta)?;
                }
            }
            on_event(AgentEvent::ToolResult {
                call_id,
                name: event_name.clone(),
                ok: result_ok,
                output: output.clone(),
            })?;
            if let Some(report) = extract_persistable_tool_report(&call.function.name, &output) {
                persisted_tool_reports.push((call.function.name.clone(), report));
            }
        }
        Ok(())
    }
}
