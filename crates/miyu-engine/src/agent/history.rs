//! 历史消息的取回与拼装。
//!
//! `chat_messages` 是请求消息数组的唯一出口——**改这里等于改缓存前缀**，顺序、
//! 空白、字段一个都不能动（见 [`super::context`] 的说明）。
//!
//! `push_history_turn` 决定一个历史回合以什么形态回放：完整回放、只留摘要、还
//! 是连工具轮次一起带上。判断依据是它有没有被压缩过、工具报告还在不在。

use crate::agent::*;

/// 裁剪一次要裁到哪个 token 数。
///
/// 裁剪是压缩放弃之后的兜底，所以它必须**裁到压缩能重新接手的位置**。按
/// `trim_batch_ratio` 算出来的落点默认是窗口的 0.85，而压缩的自锁
/// (`compact_stuck`) 要等上下文掉到压缩触发线（0.8）以下才解开——0.85 还在
/// 线上，于是裁完压缩照样不跑，会话从此只能靠一轮轮删历史苟着：兜底把会话
/// 救活了，却没把压缩救回来。取两者更低的那个，裁一次就能把压缩放回来。
pub(in crate::agent) fn trim_target(
    context_window: usize,
    trim_batch_ratio: f32,
    compact_at_ratio: f32,
) -> usize {
    let batch = (context_window as f32 * (1.0 - trim_batch_ratio)).max(1.0) as usize;
    // 压到触发线再往下一点：正好压在线上仍算「未低于」，解不开那个闩。
    let unstick = (context_window as f32 * compact_at_ratio * 0.95).max(1.0) as usize;
    batch.min(unstick)
}

impl Agent {
    pub(in crate::agent) fn trim_visible_context(
        &self,
    ) -> Result<Vec<miyu_core::state::StoredConversationEntry>> {
        let Some(context_window) = self.context_window() else {
            // 窗口解析失败(池里有 config 查不到的 provider,Err 被 .ok() 吞掉)
            // 会静默关闭裁剪——68 万 token 膨胀事故的候选路径之一。日志打在
            // miyu::qq 这条 target 上:默认日志级别只有它是 INFO,打在别处等于
            // 没打(08-26 实证:昨天布的观测一行都没写出来)。
            tracing::warn!(
                target: "miyu::qq",
                "{}",
                miyu_base::i18n::text(
                    "context trim disabled: no context window could be resolved",
                    "上下文裁剪已停用:解析不出上下文窗口"
                )
            );
            return Ok(Vec::new());
        };
        let mut total = usize::try_from(self.effective_context_tokens()?).unwrap_or(usize::MAX);
        let trigger = (context_window as f32 * self.core.trim_at_ratio).max(1.0) as usize;
        if total < trigger {
            return Ok(Vec::new());
        }

        let target = trim_target(
            context_window,
            self.core.trim_batch_ratio,
            self.core.compact_at_ratio,
        );
        let turns = self.state.load_visible_turns()?;
        let mut count = 0usize;
        for turn in turns
            .iter()
            .filter(|turn| !turn.is_summary && turn.status != miyu_core::state::TurnStatus::Running)
        {
            if total <= target {
                break;
            }
            let turn_tokens = if turn.status == miyu_core::state::TurnStatus::Interrupted
                && !turn.journal_events.is_empty()
            {
                let mut replay = vec![self.turn_user_message(turn)];
                replay.extend(interrupted_turn_replay_messages(self, turn));
                overflow::estimate_messages_tokens(&replay)
            } else {
                turn_context_tokens(turn)
            };
            total = total.saturating_sub(turn_tokens);
            count += 1;
        }
        let turns = self.state.oldest_evictable_visible_turns(count)?;
        // 观测(08-25 膨胀取证:群会话 68 万 token 未被裁剪,归档 07:42 后
        // 静默停摆):水位越线的每次裁剪把门内数字全部留痕,零归档时告警。
        tracing::info!(
            target: "miyu::qq",
            total,
            trigger,
            evict_to = target,
            planned = count,
            evictable = turns.len(),
            "{}",
            miyu_base::i18n::text("context trim engaged", "上下文裁剪已触发")
        );
        if count > 0 && turns.is_empty() {
            tracing::warn!(
                target: "miyu::qq",
                planned = count,
                "{}",
                miyu_base::i18n::text(
                    "context trim planned evictions but found no evictable turns",
                    "上下文裁剪算出要逐出的回合,却一个可逐出的都没有"
                )
            );
        }
        archive_and_delete_visible_turns_checked(&self.state, &self.memory.store, &turns, None)
    }

    pub(in crate::agent) fn initial_loaded_tools(
        &self,
        messages: &[ChatMessage],
    ) -> Result<BTreeSet<String>> {
        if !self.core.config.tools.persist_loaded_tools {
            return Ok(BTreeSet::new());
        }
        let mut loaded = self.state.load_session_loaded_tools()?;
        if loaded.is_empty() {
            loaded = loaded_tools_from_messages(messages);
            if !loaded.is_empty() {
                let names = loaded.iter().cloned().collect::<Vec<_>>();
                self.state.add_session_loaded_tools(&names, None)?;
            }
        }
        if !loaded.is_empty() {
            let tools = self.tools.lock().unwrap();
            let available = tools.tool_names().into_iter().collect::<BTreeSet<_>>();
            loaded.retain(|name| available.contains(name));
        }
        Ok(loaded)
    }

    /// 返回 (消息序列, 当前用户消息下标)。用户消息之后是数量可变的
    /// 瞬态尾巴(runtime 投影可跳、防失忆提醒隔轮注入),调用方必须用
    /// 下标定位,绝不能再按"倒数第二条"猜(缓存调研 08-16 的复位地雷)。
    pub(in crate::agent) fn chat_messages(
        &self,
        current_turn_id: &str,
        current_input: &str,
    ) -> Result<(Vec<ChatMessage>, usize)> {
        let mut messages = vec![ChatMessage::system(self.system_prompt.clone())];
        // 预设对话(begin_dialogs):system 之后、历史之前,每请求注入、
        // 永不落库。模型把它当普通聊天记录,学的是轮次里的语气;作为
        // 常量前缀只在编辑时断一次缓存。compact_fork_prefix 同步注入,
        // 保持折叠请求与实况字节一致。
        for (user, assistant) in &self.preset_dialogs {
            messages.push(ChatMessage::plain("user", user.clone()));
            messages.push(ChatMessage::assistant(assistant.clone(), None));
        }
        if !self.input.suppress_session_history {
            if let Some(summary) = self.state.load_last_summary()? {
                messages.push(self.checkpoint_message(&summary)?);
            }
            let turns = self.state.load_visible_turns_excluding(current_turn_id)?;
            for turn in &turns {
                if turn.is_summary {
                    continue;
                }
                // A turn still running holds a placeholder that gets overwritten
                // with the real reply once it finishes, so replaying it would
                // put two different byte sequences at the same position and
                // drop the prefix cache for everyone after it. Roughly a fifth
                // of this group's turns overlap. The placeholder only ever said
                // "ignore me" anyway.
                if turn.status == miyu_core::state::TurnStatus::Running {
                    continue;
                }
                self.push_history_turn(&mut messages, turn);
            }
        }
        // v7 §三: the runtime stamp is transient tail and must sit AFTER the
        // current user message. When it preceded the user message, every next
        // turn's replayed history diverged from the provider's cached prefix
        // exactly at this position (verified byte-level against DeepSeek
        // prefix caching).
        let user_index = messages.len();
        messages.push(ChatMessage::plain("user", current_input));
        // dsh 式投影(08-16 缓存调研):运行时上下文"变了才注入"。终端面
        // 时间已降到小时级,同一小时内 cwd/环境不变 → 与历史里最近一份
        // 化石逐字节相同 → 本轮零新增;平台面保留分钟级,人格报时靠它。
        let runtime = runtime_context(self.input.platform_context.is_some());
        if last_fossil_with_prefix(&messages, "<runtime ") != Some(runtime.as_str()) {
            messages.push(ChatMessage::turn_context(runtime));
        }
        // 沙盒说明(09-23 起不在系统提示词里:按 Tab 随开随关,写在前缀里每切一次
        // 就掰断整段缓存)。整体替换了系统提示词的会话连主机环境块都没有,也不发。
        if self.input.system_prompt_override.is_none() {
            let last = last_fossil_with_prefix(&messages, "<sandbox ").map(str::to_string);
            if let Some(notice) = sandbox_tail(
                self.core.prompt_audience,
                self.input.platform_context.is_some(),
                last.as_deref(),
            ) {
                messages.push(ChatMessage::turn_context(notice));
            }
        }
        // 防失忆提醒(08-16 起):不再浮动,每隔 interval 轮以化石身份进
        // 历史——纯追加,不掰前缀。计数以历史里最近一份提醒化石所在的
        // 轮为锚。(08-23 试过"上一轮工具轮次多则提前补一针"的工具后
        // 重锚,A/B 实测 8/12→4/12 负增量,已回滚——探针轮贴着当前消息
        // 再注提醒反而伤风格,别再试。)
        if let Some(reminder) = self.runtime.persona_reminder.as_deref() {
            let interval = self.core.config.prompt.persona_reminder_interval.max(1) as usize;
            if turns_since_reminder_fossil(&self.state, current_turn_id)?
                .map_or(true, |since| since >= interval)
            {
                messages.push(ChatMessage::turn_context(format!(
                    "<persona-reminder>{reminder}</persona-reminder>"
                )));
            }
        }
        Ok((messages, user_index))
    }

    /// Renders one stored turn exactly as the live request rendered it
    /// (byte-identical replay incl. the fossilized transient tail), shared by
    /// the main request path and the compaction fork prefix.
    pub(in crate::agent) fn push_history_turn(
        &self,
        messages: &mut Vec<ChatMessage>,
        turn: &miyu_core::state::Turn,
    ) {
        messages.push(self.turn_user_message(turn));
        // Fossilized transient tail (v7 append-only): replay the
        // system messages that followed the user message in the live
        // request, byte-identical and in order, so this turn renders
        // as a pure extension of what the provider already cached.
        messages.extend(turn.context_messages.iter().map(replay_fossil));
        if turn.status == miyu_core::state::TurnStatus::Interrupted
            && !turn.journal_events.is_empty()
        {
            messages.extend(interrupted_turn_replay_messages(self, turn));
        } else {
            // 问答只回放一种形态:有结构化 tool_flow 的回合,ask_question
            // 已作为原生 tool_calls+tool 输出在 flow 里逐字节回放;再补
            // 纯文本问答对=同一轮发两遍且字节不同于活体,前缀在此掰断
            // (缓存调研 08-16,deepseek 报告 P0-2③实证)。纯文本对只给
            // 无 flow 的老回合兜底。
            let has_native_flow = turn.tool_flow.iter().any(|round| !round.remote);
            if !has_native_flow {
                for exchange in &turn.question_exchanges {
                    messages.push(ChatMessage::plain(
                        "assistant",
                        miyu_base::question::assistant_exchange_text(exchange),
                    ));
                    messages.push(ChatMessage::plain(
                        "user",
                        miyu_base::question::user_exchange_text(exchange),
                    ));
                }
            }
            for followup in &turn.followups {
                push_assistant_context_messages(
                    messages,
                    followup
                        .preceding_assistant_content
                        .as_deref()
                        .unwrap_or_default(),
                    followup.preceding_assistant_reasoning.as_deref(),
                    false,
                );
                messages.push(self.followup_user_message(followup));
                // followup 随发的瞬态尾巴(runtime/图片路径/context-images)同样
                // 化石回放,否则化石在这里比活体短一截,前缀与续传链都掰断。
                messages.extend(followup.context_messages().iter().map(replay_fossil));
            }
            // dsh 形态回放:每轮 assistant 带原生 tool_calls(参数原样字节),
            // 随后各 call 的 role:"tool" 输出;最终回复照旧收尾。老回合
            // (无结构化流)退回 private_tool_memory 压扁兜底。
            let inline_media = self.turn_inline_media_by_call(turn);
            let tool_form = self.core.config.active_pool_tool_result_media();
            for round in replay_rounds(&turn.tool_flow) {
                push_assistant_message_with_reasoning(
                    messages,
                    round.assistant_content.clone(),
                    round.assistant_reasoning.as_deref(),
                    None,
                    Some(
                        round
                            .calls
                            .iter()
                            .map(|call| ToolCall {
                                id: call.id.clone(),
                                kind: "function".to_string(),
                                function: ToolCallFunction {
                                    name: call.name.clone(),
                                    arguments: call.arguments.clone(),
                                },
                            })
                            .collect(),
                    ),
                    false,
                );
                for call in &round.calls {
                    // 工具的媒体块(vision_analyze inline / 剪贴板图片 / 旁路
                    // 描述)与活体同形态同位置回放。
                    push_tool_result_with_media(
                        messages,
                        ChatMessage::tool(call.id.clone(), call.output.clone()),
                        inline_media
                            .get(call.id.as_str())
                            .map(Vec::as_slice)
                            .unwrap_or(&[]),
                        tool_form,
                    );
                }
            }
            push_assistant_context_messages(
                messages,
                &turn.assistant_content,
                turn.assistant_reasoning.as_deref(),
                true,
            );
            if turn.tool_flow.is_empty() && !turn.tool_reports.is_empty() {
                messages.push(ChatMessage::turn_context(private_tool_memory(
                    &turn.tool_reports,
                )));
            }
        }
    }

    /// 本回合各次工具调用之后追加的媒体块,按 call_id 归组。只有流里出现
    /// 过会追加媒体的工具才查库,其余回合零开销。
    fn turn_inline_media_by_call(
        &self,
        turn: &miyu_core::state::Turn,
    ) -> std::collections::HashMap<String, Vec<miyu_core::state::TurnInlineMedia>> {
        let mut grouped = std::collections::HashMap::new();
        let relevant = turn.tool_flow.iter().any(|round| {
            round
                .calls
                .iter()
                .any(|call| INLINE_MEDIA_TOOLS.contains(&call.name.as_str()))
        });
        if !relevant {
            return grouped;
        }
        match self.state.load_turn_inline_media(&turn.turn_id) {
            Ok(items) => {
                for item in items {
                    grouped
                        .entry(item.call_id.clone())
                        .or_insert_with(Vec::new)
                        .push(item);
                }
            }
            Err(error) => {
                tracing::warn!(turn_id = %turn.turn_id, %error, "failed to load inline media for replay");
            }
        }
        grouped
    }

    /// The one place a summary row becomes a message. Both the live request
    /// and the compact fork prefix go through it — two renderings of the
    /// checkpoint would diverge byte for byte and cost the whole prefix cache.
    fn checkpoint_message(&self, summary: &miyu_core::state::Turn) -> Result<ChatMessage> {
        let extras = self
            .state
            .load_summary_extras_json(&summary.turn_id)?
            .and_then(|json| {
                serde_json::from_str::<crate::agent::compact_extras::CompactExtras>(&json).ok()
            })
            .map(|extras| extras.render())
            .filter(|text| !text.is_empty());
        Ok(summary_checkpoint_message(
            &summary.assistant_content,
            extras.as_deref(),
        ))
    }

    /// Byte-identical prefix of the live conversation covering exactly the
    /// turns about to fold: `[system][checkpoint][fold turns...]`. A fork
    /// summarization request built on this prefix re-reads the history at
    /// cached price instead of full price (the serialized fallback shares no
    /// bytes with the provider's cache).
    pub(in crate::agent) fn compact_fork_prefix(
        &self,
        fold_turn_ids: &[String],
    ) -> Result<Vec<ChatMessage>> {
        let fold: std::collections::HashSet<&str> =
            fold_turn_ids.iter().map(|id| id.as_str()).collect();
        let mut messages = vec![ChatMessage::system(self.system_prompt.clone())];
        // 与 chat_messages 的实况前缀字节一致:预设对话也在折叠前缀里。
        for (user, assistant) in &self.preset_dialogs {
            messages.push(ChatMessage::plain("user", user.clone()));
            messages.push(ChatMessage::assistant(assistant.clone(), None));
        }
        if let Some(summary) = self.state.load_last_summary()? {
            messages.push(self.checkpoint_message(&summary)?);
        }
        for turn in self.state.load_visible_turns()? {
            if turn.is_summary || !fold.contains(turn.turn_id.as_str()) {
                continue;
            }
            self.push_history_turn(&mut messages, &turn);
        }
        Ok(messages)
    }

    pub(in crate::agent) fn live_tool_definitions(
        &self,
    ) -> Result<Vec<miyu_core::llm::ToolDefinition>> {
        if !self.core.tools_enabled {
            return Ok(Vec::new());
        }
        let tools = self.tools.lock().unwrap();
        Ok(tools.request_definitions(tools::is_stub_loading_mode(
            &tools::effective_tools_loading_mode(&self.core.config),
        )))
    }

    pub(in crate::agent) fn followup_user_message(
        &self,
        followup: &miyu_core::state::TurnFollowup,
    ) -> ChatMessage {
        if !self.current_model_supports_vision() {
            return ChatMessage::plain("user", &followup.content);
        }
        let mut images = followup
            .attachments
            .iter()
            .filter_map(|attachment| match attachment {
                QueuedPromptAttachment::Binary { mime, data_base64 } => {
                    Some(ChatContentPart::ImageUrl {
                        image_url: ImageUrlContent {
                            url: format!("data:{mime};base64,{data_base64}"),
                        },
                    })
                }
                QueuedPromptAttachment::Path { .. } => None,
            })
            .collect::<Vec<_>>();
        images.extend(self.uploaded_attachment_image_parts(&followup.uploaded_attachments));
        if images.is_empty() {
            return ChatMessage::plain("user", &followup.content);
        }
        let mut parts = vec![ChatContentPart::Text {
            text: followup.content.clone(),
        }];
        parts.extend(images);
        ChatMessage::user_parts(parts)
    }

    pub(in crate::agent) fn turn_user_message(&self, turn: &miyu_core::state::Turn) -> ChatMessage {
        if !self.current_model_supports_vision() {
            return ChatMessage::plain("user", &turn.user_content);
        }
        let images = self.uploaded_attachment_image_parts(&turn.attachments);
        if images.is_empty() {
            return ChatMessage::plain("user", &turn.user_content);
        }
        let mut parts = vec![ChatContentPart::Text {
            text: turn.user_content.clone(),
        }];
        parts.extend(images);
        ChatMessage::user_parts(parts)
    }
}
