//! 单个工具调用的执行(`chat_with_tools` 每批里的一个):`ask_question` 走回合内提问通道,
//! 其它工具真执行——带进度事件、子代理子过程的限流检查点、桩工具的契约补提示、内联媒体
//! 落库与视觉退回。09-17 从 `tool_exec.rs` 再拆一层。

use super::parallel;
use super::round_state::RoundState;
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

    /// 真执行一个工具并把结果压进 `messages`(各失败分支同样压一条 tool 消息后返回)。
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
    ) -> Result<()>
    where
        F: FnMut(AgentEvent) -> Result<()>,
    {
        used_tools.push(call.function.name.clone());
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
                &crate::tools::GuardCtx {
                    used_tools: &used_tools,
                },
            )
        };
        // 桩工具失败时把真契约补进返回体(每个工具每回合只补一次)。
        let mut attach_contract = |message: String| -> String {
            if !tools::is_stub_loading_mode(&self.core.config.tools.loading_mode) {
                return message;
            }
            if !st.contract_hinted.insert(call.function.name.clone()) {
                return message;
            }
            let tools = self.tools.lock().unwrap();
            if !tools.is_stub_presented(&call.function.name) {
                return message;
            }
            match tools.contract_text(&call.function.name) {
                Some(contract) => format!(
                    "{message}\n\nThis tool was declared with an empty parameter shell, so its real schema follows. Call it again with these arguments at the top level.{contract}"
                ),
                None => message,
            }
        };
        let tool_future = match tool_future {
            Ok(f) => f,
            Err(err) => {
                let output = attach_contract(format!("tool error: {err}"));
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
        tokio::pin!(tool_future);
        let mut spinner_interval = tokio::time::interval(self.core.spinner_interval);
        spinner_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        spinner_interval.tick().await;
        // 前台子代理跑到一半刷新页面就丢子过程(#5a 续:子过程只在内存里,
        // 回合收尾才落库,而单个前台子代理走的就是这条串行路,收尾在它
        // 整个跑完之后)。这里在子过程标记流上限流打检查点:此刻
        // `messages` 里已有那条调子代理的 assistant 消息(见上面 push),
        // checkpoint_tool_flow 走 peek 把当前累积的 sub_trace 落库,刷新时
        // renderPersistedTurn 就能把在跑的子过程时间线画出来,不再是空。
        // None = 还没落过,第一条子过程标记就立刻落一次(子代理常是先爆一小段
        // 标记再钻进一次长 LLM 应答里安静好一会儿,若等满 1.5s 那一窗就全错过了)。
        // `sub_dirty`:上次落库后又来过标记但被节流跳过了。子代理典型节奏是「爆一段
        // 标记 → 钻进长 LLM 应答安静十几秒」:首条落库只抓到爆发的第一条,后面几条
        // 全在 1.5s 窗内被跳过,然后一安静就再没有 recv 触发——那段就只活在实时流里、
        // 刷新即丢。所以工具空转的 spinner tick 上补一刀:脏了且过了节流窗就把尾巴落了。
        let mut last_sub_checkpoint: Option<std::time::Instant> = None;
        let mut sub_dirty = false;
        let (output, tool_succeeded) = loop {
            tokio::select! {
                result = &mut tool_future => {
                    break match result {
                        Ok(output) => {
                            while let Ok(progress) = progress_rx.try_recv() {
                                parallel::tee_subagent_trace(&call_id, &progress);
                                emit_tool_progress(on_event, &call_id, &event_name, progress)?;
                            }
                            (output, true)
                        }
                        Err(err) => {
                            while let Ok(progress) = progress_rx.try_recv() {
                                parallel::tee_subagent_trace(&call_id, &progress);
                                emit_tool_progress(on_event, &call_id, &event_name, progress)?;
                            }
                            let output = attach_contract(format!("tool error: {err}"));
                            on_event(AgentEvent::ToolResult {
                                call_id: call_id.clone(),
                                name: event_name.clone(),
                                ok: false,
                                output: output.clone(),
                            })?;
                            (output, false)
                        }
                    };
                }
                Some(progress) = progress_rx.recv() => {
                    let is_sub_marker = matches!(
                        &progress,
                        tools::ToolProgressEvent::Message(message)
                            if tools::is_subagent_marker(message)
                    );
                    parallel::tee_subagent_trace(&call_id, &progress);
                    emit_tool_progress(on_event, &call_id, &event_name, progress)?;
                    // 限流:首条立刻落,之后每 ~1.5s 一次(peek 不清空,幂等),
                    // 避免逐 token 写库。跳过的标记记脏,交给下面 spinner tick 补落。
                    if is_sub_marker {
                        sub_dirty = true;
                        if last_sub_checkpoint.map_or(true, |at| {
                            at.elapsed() >= std::time::Duration::from_millis(1500)
                        }) {
                            last_sub_checkpoint = Some(std::time::Instant::now());
                            sub_dirty = false;
                            self.checkpoint_tool_flow(
                                current_turn_id,
                                messages,
                                st.replay_start,
                            );
                        }
                    }
                }
                _ = spinner_interval.tick() => {
                    on_event(AgentEvent::SpinnerTick)?;
                    // 子代理安静下来(钻进长应答)后,把爆发尾巴那几条被节流跳过的
                    // 标记补落一次,不然刷新只剩爆发首条。
                    if sub_dirty
                        && last_sub_checkpoint.map_or(true, |at| {
                            at.elapsed() >= std::time::Duration::from_millis(1500)
                        })
                    {
                        last_sub_checkpoint = Some(std::time::Instant::now());
                        sub_dirty = false;
                        self.checkpoint_tool_flow(
                            current_turn_id,
                            messages,
                            st.replay_start,
                        );
                    }
                }
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
