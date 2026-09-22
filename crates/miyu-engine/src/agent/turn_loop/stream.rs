//! 单轮流式请求。
//!
//! `chat_stream_turn` 是「发一次请求、把流吐给上层」的那一层，不含工具往返
//! （那在 [`super`] 的 `chat_with_tools` 里）。分开是因为缓存保活、子代理这些
//! 场景只需要这一层。

use crate::agent::*;

impl Agent {
    pub(in crate::agent) async fn chat_stream_turn<F>(
        &mut self,
        input: &str,
        images: &[Option<PastedImage>],
        control: Option<&AgentTurnControl>,
        on_event: F,
    ) -> Result<ChatResult>
    where
        F: FnMut(AgentEvent) -> Result<()>,
    {
        // A new turn is about to mutate the context; stop pinging the stale
        // prefix (the turn's own requests refresh the cache anyway).
        self.cancel_cache_keepalive();
        self.state.recover_stale_turns()?;
        // 走压缩的会话不跑裁剪(09-23 用户裁定)。
        //
        // 裁剪是**直接删最老的轮**,代价有两层:删掉的历史只归档进
        // `evicted_context.db`、不再回到上下文;而历史开头一变,前缀缓存就
        // 从头断一次。上下文该由压缩处理——它把旧轮折成摘要留在上下文里,
        // 也只断一次前缀,但信息还在。
        //
        // `on_overflow = "pop"` 是另一回事:那个档位的语义就是「不压缩、
        // 直接丢」,裁剪正是它的实现,所以保留。
        if self.core.on_overflow != "compact" {
            self.trim_visible_context()?;
        }
        self.runtime.persona_reminder = self.resolve_persona_reminder().await;
        // 人类新回合:重复链语境重置。goal 自动续轮/job 唤醒不算语境
        // 变化——跨自动轮的原样重复正是最需要打断的死循环(dsh 同款:
        // 只有 user 来源消息重置链)。
        if matches!(
            miyu_base::workspace::current_turn_origin(),
            miyu_base::workspace::TurnOrigin::Human
        ) {}
        let prepared = self.prepare_user_input(input, images).await?;
        let input = prepared.content.clone();
        let turn_id = format!(
            "turn_{}_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0),
            rand::random::<u16>()
        );
        let display_content = self
            .input
            .turn_display_content
            .take()
            .unwrap_or_else(|| input.clone());
        let attachment_run_id = self.input.attachment_run_id.take();
        self.state.start_turn_with_display(
            &turn_id,
            &input,
            &display_content,
            std::process::id(),
            attachment_run_id.as_deref(),
        )?;
        // 新回合从零起算:镜像跨回合复用(Agent 在 REPL 里跨回合存活)。
        self.runtime.turn_usage.reset();
        // 这一轮的每条记账行都带上轮 id,`cache-usage.jsonl` 才能与 turns 对账。
        self.client.set_log_turn(Some(&turn_id));
        let guard = PendingTurnGuard::new(self.state.clone(), turn_id.clone())
            .with_usage_mirror(self.runtime.turn_usage.clone());
        let mut on_event = on_event;
        on_event(AgentEvent::TurnStarted {
            turn_id: turn_id.clone(),
        })?;
        let (mut messages, user_index) = self.chat_messages(&turn_id, &input)?;
        // 按显式下标把占位用户消息换成带附件的成品;瞬态尾巴保持原位。
        if let Some(user) = messages.get_mut(user_index) {
            *user = prepared.message;
        }
        let replay_start = messages.len();
        // 回合尾巴按固定顺序收集、一次性提交(顺序即缓存前缀,挂接表见
        // docs/interfaces/subsystems.md):可信传输上下文 → 输入附带的提示 → 联想记忆 →
        // 表情包提醒。收集期间 `messages` 不变,各步的去重都按同一份已发内容判。
        let tail = self
            .assemble_turn_tail(&messages, &turn_id, &input, prepared.hints)
            .await?;
        messages.extend(tail);
        // v7 append-only fossilization ("注入了就别删"): archive the transient
        // system tail exactly as sent — runtime stamp, trusted transport
        // context, hints, associative memory, meme reminder — so future
        // history replay is a byte-exact extension of this request and the
        // provider prefix cache never sees a divergence at this turn.
        self.state.set_turn_context_messages(
            &turn_id,
            &fossil_context_messages(&messages[user_index + 1..]),
        )?;
        let mut used_tools = Vec::new();
        let mut persisted_tool_reports = Vec::new();
        let mut journal = TurnJournalSink::new(self.state.clone(), turn_id.clone(), 0);
        let stream_result = {
            let mut journaled_event = |event| journal.emit(event, &mut on_event);
            self.chat_with_tools(
                &turn_id,
                &mut messages,
                &mut used_tools,
                &mut persisted_tool_reports,
                replay_start,
                &[],
                0,
                0,
                control,
                &mut journaled_event,
            )
            .await
        };
        journal.finish(&mut on_event)?;
        let result = stream_result?;
        let reports = persisted_tool_reports
            .into_iter()
            .map(|(_, report)| report)
            .collect::<Vec<_>>();
        self.state.append_persisted_contexts(&turn_id, &reports)?;
        let tokens = TurnTokens::from_usage(result.usage.as_ref());
        guard.complete_with_model(
            &result.content,
            result.reasoning.as_deref(),
            result.provider_id.as_deref(),
            result.model.as_deref(),
            tokens,
            result.usage_estimated,
        )?;
        // 上下文锚点：这一轮最后一次请求的真实占用，下一次问上下文有多满时
        // 直接读它，不再本地估算。
        self.state.set_turn_context_end(
            &turn_id,
            crate::agent::context_meter::context_end_tokens(&result),
        )?;
        if let Some(usage) = result.usage.as_ref() {
            if usage.generation_tokens > 0 && usage.generation_ms > 0 {
                self.state.set_turn_generation(
                    &turn_id,
                    usage.generation_tokens,
                    usage.generation_ms,
                )?;
            }
        }
        if let (Some(provider), Some(model)) = (&result.provider_id, &result.model) {
            self.runtime.last_request_endpoint = Some((provider.clone(), model.clone()));
        }
        let mut tool_flow = derive_tool_flow(&messages, replay_start, true);
        prune_tool_flow(&mut tool_flow, &self.core.config.context);
        self.append_remote_tool_flow(&mut tool_flow);
        if !tool_flow.is_empty() {
            self.state.set_turn_tool_flow(&turn_id, &tool_flow)?;
        }
        if self.memory.store.process_after_turn(
            // C10 三份内容分离(最小实现):日记读平台包装前的原文快照,
            // 而不是带指令样板和群聊记录块的完整 prompt 内容。
            self.input.memory_content.as_deref().unwrap_or(&input),
            &result.content,
            &self.memory.origin,
            &self.memory.database_id,
            self.memory.generation,
        )? {
            self.wake_memory_organizer();
        }
        if let Some(usage) = result.usage.clone() {
            let meta = miyu_core::state::UsageMeta {
                source: self.usage_source(),
                provider: result.provider_id.as_deref(),
                model: result.model.as_deref(),
                kind: None,
            };
            self.state.add_usage(&usage, meta)?;
        }
        self.start_cache_keepalive();
        Ok(result)
    }

    /// 回合尾巴(v7 §三):用户消息之后、模型请求之前的瞬态注入,按固定顺序收齐后
    /// 一次性追加;顺序改一下就是缓存前缀改一下。`messages` 是收集前的请求(含化石),
    /// 传输上下文的常驻告示与联想记忆的跨轮去重都拿它判「这一行是不是已经在请求里了」。
    async fn assemble_turn_tail(
        &self,
        messages: &[ChatMessage],
        turn_id: &str,
        input: &str,
        hints: Vec<ChatMessage>,
    ) -> Result<Vec<ChatMessage>> {
        let mut tail = Vec::new();
        if !self.input.turn_system_context.is_empty() {
            // Trusted transport/control tail (v7 §三): host-derived per-message
            // context lands after the user message, before untrusted blocks.
            // Standing advisories (the `[SystemInfo:` class, e.g. long-reply
            // conversion records) repeat identical text turn after turn; when
            // the exact bytes are already visible in a replayed fossil the
            // repeat adds nothing and is skipped — the associative-memory
            // dedup reasoning. Everything else ("this turn is system
            // triggered", identity warnings, moderation prechecks) refers to
            // the CURRENT turn, so an identical old fossil is no substitute
            // and those blocks are always sent.
            let fresh = self
                .input
                .turn_system_context
                .iter()
                .filter(|block| {
                    !(block.starts_with(STANDING_ADVISORY_PREFIX)
                        && turn_context_block_visible(&messages, block))
                })
                .cloned()
                .collect::<Vec<_>>();
            if !fresh.is_empty() {
                tail.push(ChatMessage::turn_context(fresh.join("\n\n")));
            }
        }
        tail.extend(hints);
        // 记忆联想不再按模式关断:dev 的 MemoryStore 指向保留人格 "dev"
        // 的独立库(构造时作用域化),联想/落库都发生在自己的命名空间里。
        let association_exclusion =
            self.state
                .oldest_visible_turn_timestamp(&turn_id)?
                .map(|since| miyu_core::memory::AssociationExclusion {
                    session_id: self.state.session_id().to_string(),
                    since,
                });
        if let Some(mut association) = self
            .memory
            .store
            .association_with_semantic(&input, association_exclusion.as_ref())
            .await?
        {
            if association.organization_due {
                self.wake_memory_organizer();
            }
            if self.memory.store.association_dedup_enabled() {
                // Cross-turn dedup: fossils replay earlier associative
                // blocks byte-for-byte, so a line already visible in this
                // request adds nothing but tokens. Filtering only shrinks
                // the block being built this turn; once a carrying turn is
                // hidden by compact or trim, its lines leave the request
                // and the memory becomes eligible for injection again.
                let seen = visible_association_lines(&messages);
                self.memory
                    .store
                    .retain_unseen_association(&mut association, &seen);
            }
            if !association.facts.is_empty() || !association.episodes.is_empty() {
                // v7 Phase 1.1: the associative-memory block rides the turn
                // tail instead of `insert(1)`, so the stable history prefix
                // in front stays byte-identical for provider prefix caches.
                // It lands after `replay_start`, so redo checkpoints freeze
                // the recalled snapshot (decision 6).
                tail.push(ChatMessage::turn_context(
                    self.memory.store.format_association(&association),
                ));
            }
        }
        // 提醒只会指向 use_meme:这轮的工具面里没有它(dev 目录、人格清单没勾表情包、
        // 成员没开)就不发,否则模型去加载一个不存在的工具(09-11 成员实测)。
        let meme_tool_present = self.tools.lock().unwrap().contains("use_meme");
        if !self.core.dev && meme_tool_present {
            if let Some(reminder) = memes::auto_meme_reminder(
                &self.core.config,
                &input,
                self.input.platform_context.is_some(),
            ) {
                tail.push(ChatMessage::turn_context(reminder));
            }
        }
        Ok(tail)
    }
}
