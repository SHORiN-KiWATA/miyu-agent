//! 待发回复的登记与撤销。
//!
//! 「决定要回」和「真的发出去」之间有一段窗口，用户可能在这期间补一句话。这套
//! 登记让新消息能**超越**旧的待发回复：旧的作废、反应表情搬过去、窗口重新计时。
//!
//! `is_current_pending` 是并发关键：判定跑完时可能已经换了一轮，写回前必须确认
//! 自己还是当前那个。

use crate::platforms::plugins::real_context::*;

impl RealContextPlugin {
    pub(in crate::platforms::plugins::real_context) fn watermark_scope(
        context: &PlatformTurnContext,
    ) -> miyu_core::state::PlatformPluginScopeKey {
        miyu_core::state::PlatformPluginScopeKey {
            plugin_id: "real_context".to_string(),
            platform: context.conversation.platform.clone(),
            account_id: context.conversation.account_id.clone(),
            conversation_kind: context.conversation.kind.as_str().to_string(),
            conversation_id: context.conversation.conversation_id.clone(),
        }
    }

    /// Highest ingress order already rendered into this conversation's replayed
    /// history. Best effort: losing it only costs one oversized turn, never
    /// correctness, because the block is additive either way.
    pub(in crate::platforms::plugins::real_context) fn reply_watermark(
        &self,
        context: &PlatformTurnContext,
    ) -> Option<i64> {
        context
            .state_store
            .plugin_get_json::<i64>(&Self::watermark_scope(context), REPLY_WATERMARK_KEY)
            .ok()
            .flatten()
    }

    pub(in crate::platforms::plugins::real_context) fn store_reply_watermark(
        &self,
        context: &PlatformTurnContext,
        high: i64,
    ) {
        if let Err(error) = context.state_store.plugin_put_json(
            &Self::watermark_scope(context),
            REPLY_WATERMARK_KEY,
            &high,
        ) {
            tracing::warn!(
                target: "miyu::qq",
                error = %error,
                "{}",
                miyu_base::i18n::text(
                    "failed to persist the group reply watermark",
                    "保存群聊回复水位线失败"
                )
            );
        }
    }

    /// 直触发确定要回复:接管补救窗口内的旧 pending(表情转移到本消息)、
    /// 贴表情,并登记一条「已承诺」的 pending,使后续补救消息能够覆盖本次回复。
    pub(in crate::platforms::plugins::real_context) async fn commit_direct_reply(
        &self,
        context: &PlatformTurnContext,
        event: &PlatformInboundEvent,
        settings: &RealContextPluginSettings,
    ) {
        let now = Instant::now();
        let session_key = runtime_session_key(context);
        let window = Duration::from_secs(settings.active_reply_supersede_window_seconds);
        let (old_reactions, mut targets) = {
            let mut runtime = self.runtime.lock().unwrap();
            let session = runtime.session_mut(&session_key, now);
            match session.pending.remove(&event.sender_id) {
                Some(pending)
                    if settings.active_reply_supersede_enable
                        && now.duration_since(pending.started) <= window =>
                {
                    pending.supersede_for(&context.ownership);
                    (pending.reactions, pending.targets)
                }
                _ => (Vec::new(), Vec::new()),
            }
        };
        for (message_id, reaction_id) in old_reactions {
            self.cancel_reaction_expiration(context, &message_id, &reaction_id);
            if let Err(error) = context
                .set_message_reaction(&message_id, &reaction_id, false)
                .await
            {
                tracing::debug!(error = %error, %message_id, "{}", miyu_base::i18n::text("superseded QQ reaction could not be removed", "无法移除已被新消息覆盖的 QQ 表情回应"));
            }
        }
        targets.push(active_reply_target(event));
        normalize_active_targets(&mut targets, &event.sender_id);
        set_active_targets(context, &targets);
        let reactions = self
            .add_reactions(context, event, settings, TriggerKind::Direct)
            .await;
        self.register_committed_pending(context, TriggerKind::Direct, reactions, targets, false);
    }

    pub(in crate::platforms::plugins::real_context) fn register_committed_pending(
        &self,
        context: &PlatformTurnContext,
        trigger: TriggerKind,
        reactions: Vec<(String, String)>,
        targets: Vec<ActiveReplyTarget>,
        supersede_previous: bool,
    ) {
        let now = Instant::now();
        let (cancel, _receiver) = tokio::sync::watch::channel(false);
        let mut runtime = self.runtime.lock().unwrap();
        runtime.next_generation = runtime.next_generation.wrapping_add(1).max(1);
        let generation = runtime.next_generation;
        let previous = runtime
            .session_mut(&runtime_session_key(context), now)
            .pending
            .insert(
                context.sender_id.clone(),
                PendingReply {
                    owner: context.ownership.clone(),
                    generation,
                    started: now,
                    trigger,
                    committed: true,
                    reactions,
                    targets,
                    cancel,
                },
            );
        if supersede_previous {
            if let Some(previous) = previous {
                previous.supersede_for(&context.ownership);
            }
        }
    }

    pub(in crate::platforms::plugins::real_context) async fn add_reactions(
        &self,
        context: &PlatformTurnContext,
        event: &PlatformInboundEvent,
        settings: &RealContextPluginSettings,
        trigger: TriggerKind,
    ) -> Vec<(String, String)> {
        if !settings.active_reply_reaction_enable
            || !trigger.marks_with_reaction()
            || event.message_id.is_empty()
        {
            return Vec::new();
        }
        let mut active = Vec::new();
        for reaction_id in &settings.active_reply_reaction_emoji_ids {
            let reaction_id = reaction_id.to_string();
            match context
                .set_message_reaction(&event.message_id, &reaction_id, true)
                .await
            {
                Ok(()) => {
                    active.push((event.message_id.clone(), reaction_id.clone()));
                    let expiration = context.schedule_message_reaction_removal(
                        event.message_id.clone(),
                        reaction_id.clone(),
                        Duration::from_secs(settings.active_reply_reaction_timeout_seconds),
                    );
                    let key = (
                        runtime_session_key(context),
                        event.message_id.clone(),
                        reaction_id,
                    );
                    let mut expirations = self.reaction_expirations.lock().unwrap();
                    expirations.retain(|_, handle| !handle.is_finished());
                    if let Some(previous) = expirations.insert(key, expiration) {
                        previous.abort();
                    }
                }
                Err(error) => tracing::debug!(
                    error = %error,
                    message_id = %event.message_id,
                    "{}",
                    miyu_base::i18n::text(
                        "QQ active-reply reaction could not be added",
                        "无法添加 QQ 主动回复表情回应",
                    )
                ),
            }
        }
        active
    }

    /// 判过要回的新消息并进了同一个人正在跑的那一轮(`context` 是那一轮的)。
    ///
    /// 判断时给新消息登记的待回复改记在那一轮名下:回复发出时由它清掉,那一轮
    /// 中止时由它撤表情。表情只留在最新那条上,那一轮原先挂表情的消息摘掉,和
    /// 覆盖窗口里换贴一个样(用户 09-26:「贴,但是换贴」)。
    pub(in crate::platforms::plugins::real_context) async fn adopt_merged_followup(
        &self,
        context: &PlatformTurnContext,
        event: &PlatformInboundEvent,
        settings: &RealContextPluginSettings,
    ) {
        if let Some(pending) = self
            .runtime
            .lock()
            .unwrap()
            .sessions
            .get_mut(&runtime_session_key(context))
            .and_then(|session| session.pending.get_mut(&event.sender_id))
        {
            pending.owner = context.ownership.clone();
        }
        let holder = reaction_holder(context);
        context.set_plugin_value(REACTION_HOLDER_KEY, Value::String(event.message_id.clone()));
        if let Some(holder) = holder.filter(|holder| *holder != event.message_id) {
            self.remove_reply_reactions(context, &holder, settings)
                .await;
        }
    }

    /// 摘掉一条消息上的「要回」表情,连同它的定时摘除。
    pub(in crate::platforms::plugins::real_context) async fn remove_reply_reactions(
        &self,
        context: &PlatformTurnContext,
        message_id: &str,
        settings: &RealContextPluginSettings,
    ) {
        for reaction in &settings.active_reply_reaction_emoji_ids {
            let reaction = reaction.to_string();
            self.cancel_reaction_expiration(context, message_id, &reaction);
            if let Err(error) = context
                .set_message_reaction(message_id, &reaction, false)
                .await
            {
                tracing::debug!(error = %error, %message_id, "{}", miyu_base::i18n::text("QQ reply reaction could not be removed", "无法移除 QQ 回复表情回应"));
            }
        }
    }

    pub(in crate::platforms::plugins::real_context) fn cancel_reaction_expiration(
        &self,
        context: &PlatformTurnContext,
        message_id: &str,
        reaction_id: &str,
    ) {
        let key = (
            runtime_session_key(context),
            message_id.to_string(),
            reaction_id.to_string(),
        );
        if let Some(expiration) = self.reaction_expirations.lock().unwrap().remove(&key) {
            expiration.abort();
        }
    }

    /// 别人的、已经放行还没发完的回复对应的消息,给判官看(见 judge::AlreadyAnswering)。
    pub(in crate::platforms::plugins::real_context) fn already_answering(
        &self,
        session_key: &str,
        sender_id: &str,
    ) -> Vec<judge::AlreadyAnswering> {
        let runtime = self.runtime.lock().unwrap();
        let Some(session) = runtime.sessions.get(session_key) else {
            return Vec::new();
        };
        let mut items = session
            .pending
            .iter()
            .filter(|(owner, pending)| owner.as_str() != sender_id && pending.committed)
            .flat_map(|(_, pending)| {
                pending
                    .targets
                    .iter()
                    .map(move |target| (pending.started, target))
            })
            .filter(|(_, target)| !target.supplemental && !target.content.trim().is_empty())
            .collect::<Vec<_>>();
        items.sort_by_key(|(started, target)| (*started, target.timestamp));
        items
            .into_iter()
            .map(|(_, target)| judge::AlreadyAnswering {
                sender_name: target.sender_name.clone(),
                sender_id: target.sender_id.clone(),
                text: target.content.clone(),
            })
            .collect()
    }

    pub(in crate::platforms::plugins::real_context) fn drop_pending(
        &self,
        session_key: &str,
        sender_id: &str,
        generation: u64,
    ) -> bool {
        let mut runtime = self.runtime.lock().unwrap();
        let Some(session) = runtime.sessions.get_mut(session_key) else {
            return false;
        };
        if session
            .pending
            .get(sender_id)
            .is_some_and(|pending| pending.generation == generation)
        {
            session.pending.remove(sender_id);
            true
        } else {
            false
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(in crate::platforms::plugins::real_context) fn fail_current_attempt(
        &self,
        context: &PlatformTurnContext,
        decision: &mut TriggerDecision,
        core_fallback: Option<&TriggerDecision>,
        session_key: &str,
        sender_id: &str,
        generation: u64,
        settings: &RealContextPluginSettings,
        trigger: TriggerKind,
        reason: &str,
    ) {
        let was_current = self.drop_pending(session_key, sender_id, generation);
        self.log_skip(context, settings, trigger, reason);
        if was_current {
            if let Some(fallback) = core_fallback {
                restore_core_trigger(context, decision, fallback);
            }
        }
    }

    pub(in crate::platforms::plugins::real_context) fn is_current_pending(
        &self,
        session_key: &str,
        sender_id: &str,
        generation: u64,
    ) -> bool {
        self.runtime
            .lock()
            .unwrap()
            .sessions
            .get(session_key)
            .and_then(|session| session.pending.get(sender_id))
            .is_some_and(|pending| pending.generation == generation)
    }

    pub(in crate::platforms::plugins::real_context) async fn clear_cancelled_pending(
        &self,
        context: &PlatformTurnContext,
        sender_id: &str,
    ) {
        let session_key = runtime_session_key(context);
        let reactions = {
            let mut runtime = self.runtime.lock().unwrap();
            let session = runtime.sessions.get_mut(&session_key);
            let cancelled = session
                .as_ref()
                .and_then(|session| session.pending.get(sender_id))
                .is_some_and(|pending| *pending.cancel.borrow());
            if !cancelled {
                return;
            }
            session
                .and_then(|session| session.pending.remove(sender_id))
                .map(|pending| pending.reactions)
                .unwrap_or_default()
        };
        for (message_id, reaction_id) in reactions {
            self.cancel_reaction_expiration(context, &message_id, &reaction_id);
            if let Err(error) = context
                .set_message_reaction(&message_id, &reaction_id, false)
                .await
            {
                tracing::debug!(error = %error, %message_id, %reaction_id, "{}", miyu_base::i18n::text("cancelled QQ reaction could not be removed", "无法移除已取消的 QQ 表情回应"));
            }
        }
    }
}

/// 这一轮的「要回」表情眼下挂在哪条消息上(见 `REACTION_HOLDER_KEY`)。
pub(in crate::platforms::plugins::real_context) fn reaction_holder(
    context: &PlatformTurnContext,
) -> Option<String> {
    context
        .plugin_value(REACTION_HOLDER_KEY)
        .and_then(|value| value.as_str().map(str::to_string))
        .or_else(|| {
            context
                .inbound_event()
                .map(|event| event.message_id.clone())
        })
        .filter(|message_id| !message_id.is_empty())
}
