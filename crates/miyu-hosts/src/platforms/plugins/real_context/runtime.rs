//! 插件的运行时状态。
//!
//! 每个群一份状态，带软上限与空闲 TTL（`SESSION_STATE_*`）——群会一直增加，
//! 不淘汰就是慢性泄漏。
//!
//! `DynamicGate` 控制并发判定：同一个群同时只跑一个「要不要插话」的判断，
//! permit 用 `Drop` 释放。`wait_for_supersede` 处理的是判断跑到一半又来了新消
//! 息——旧判断的结论已经过时，直接让位。

use crate::platforms::plugins::real_context::*;

pub(in crate::platforms::plugins::real_context) const SESSION_STATE_SOFT_LIMIT: usize = 512;

pub(in crate::platforms::plugins::real_context) const SESSION_STATE_IDLE_TTL: Duration =
    Duration::from_secs(24 * 60 * 60);

pub(in crate::platforms::plugins::real_context) const PENDING_REPLY_TTL: Duration =
    Duration::from_secs(31 * 60);

#[derive(Default)]
pub(in crate::platforms::plugins::real_context) struct RuntimeState {
    pub(in crate::platforms::plugins::real_context) sessions: HashMap<String, SessionRuntime>,
    pub(in crate::platforms::plugins::real_context) next_generation: u64,
}

impl RuntimeState {
    pub(in crate::platforms::plugins::real_context) fn session_mut(
        &mut self,
        key: &str,
        now: Instant,
    ) -> &mut SessionRuntime {
        let session = self
            .sessions
            .entry(key.to_string())
            .or_insert_with(|| SessionRuntime::new(now));
        session.last_touched = now;
        session
    }

    pub(in crate::platforms::plugins::real_context) fn prune(&mut self, now: Instant) {
        for session in self.sessions.values_mut() {
            session
                .pending
                .retain(|_, pending| now.duration_since(pending.started) <= PENDING_REPLY_TTL);
        }
        if self.sessions.len() > SESSION_STATE_SOFT_LIMIT {
            self.sessions.retain(|_, session| {
                !session.pending.is_empty()
                    || now.duration_since(session.last_touched) <= SESSION_STATE_IDLE_TTL
            });
        }
        let removable = self.sessions.len().saturating_sub(SESSION_STATE_SOFT_LIMIT);
        if removable > 0 {
            let mut inactive = self
                .sessions
                .iter()
                .filter(|(_, session)| session.pending.is_empty())
                .map(|(key, session)| (key.clone(), session.last_touched))
                .collect::<Vec<_>>();
            inactive.sort_unstable_by_key(|(_, touched)| *touched);
            for (key, _) in inactive.into_iter().take(removable) {
                self.sessions.remove(&key);
            }
        }
    }
}

pub(in crate::platforms::plugins::real_context) struct SessionRuntime {
    pub(in crate::platforms::plugins::real_context) last_touched: Instant,
    pub(in crate::platforms::plugins::real_context) last_reply: Option<Instant>,
    /// 近期发言量(冷静机制,见 restraint.rs)。只在内存里:半衰期是分钟级,
    /// daemon 重启丢了也几分钟内就追平。
    pub(in crate::platforms::plugins::real_context) pressure: ReplyPressure,
    pub(in crate::platforms::plugins::real_context) continuation: Option<Continuation>,
    pub(in crate::platforms::plugins::real_context) pending: HashMap<String, PendingReply>,
}

impl SessionRuntime {
    pub fn new(now: Instant) -> Self {
        Self {
            last_touched: now,
            last_reply: None,
            pressure: ReplyPressure::new(now),
            continuation: None,
            pending: HashMap::new(),
        }
    }

    pub(in crate::platforms::plugins::real_context) fn reply_pressure(&self, now: Instant) -> f64 {
        self.pressure.level(now)
    }

    /// 她在这个群真发出去一轮回复:记一笔。回 @ 的也记(用户 09-24 拍板)。
    pub(in crate::platforms::plugins::real_context) fn record_reply(
        &mut self,
        now: Instant,
        settings: &RealContextPluginSettings,
    ) {
        if settings.reply_restraint_enable {
            self.pressure.record(now);
        }
    }

    pub(in crate::platforms::plugins::real_context) fn continuation_match(
        &mut self,
        sender_id: &str,
        now: Instant,
        enabled: bool,
    ) -> bool {
        if !enabled {
            self.continuation = None;
            return false;
        }
        let Some(continuation) = self.continuation.as_ref() else {
            return false;
        };
        // Only the clock and the speaker bound a continuation. There used to be
        // a turn cap as well, which cut a conversation off mid-flow purely
        // because it had gone on for a few exchanges — the window itself is
        // what expresses "we are still talking".
        if now > continuation.expires_at || continuation.user_id != sender_id {
            self.continuation = None;
            return false;
        }
        true
    }

    /// 她刚在这个群发过言吗(开关 after_speaking_enable,窗口
    /// after_speaking_window_seconds,默认 30s)。
    ///
    /// 直接拿 `last_reply` 算,不另存状态:那个字段就是「最后一次真发出去的回复」,
    /// 与 mark_continuation 在同一处更新。窗口内**任何人**的消息都会来一次判断——
    /// 人发完言大概率会看到接下来的消息;门槛另加(见 inject 的 after_speaking)。
    pub(in crate::platforms::plugins::real_context) fn spoke_recently(
        &self,
        now: Instant,
        settings: &RealContextPluginSettings,
    ) -> bool {
        settings.after_speaking_enable
            && self.last_reply.is_some_and(|at| {
                now.saturating_duration_since(at)
                    <= Duration::from_secs(settings.after_speaking_window_seconds)
            })
    }

    pub(in crate::platforms::plugins::real_context) fn mark_continuation(
        &mut self,
        sender_id: &str,
        now: Instant,
        settings: &RealContextPluginSettings,
    ) {
        if !settings.continuation_enable {
            self.continuation = None;
            return;
        }
        // Every reply we actually send restarts the clock, including one the
        // continuation window itself prompted: answering inside the window is
        // exactly the evidence that the exchange is still live, so it should
        // extend the window rather than count down against it.
        self.continuation = Some(Continuation {
            user_id: sender_id.to_string(),
            expires_at: now + Duration::from_secs(settings.continuation_window_seconds),
        });
    }
}

pub(in crate::platforms::plugins::real_context) struct Continuation {
    pub(in crate::platforms::plugins::real_context) user_id: String,
    pub(in crate::platforms::plugins::real_context) expires_at: Instant,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(in crate::platforms::plugins::real_context) struct ActiveReplyTarget {
    pub(in crate::platforms::plugins::real_context) message_id: String,
    pub(in crate::platforms::plugins::real_context) sender_id: String,
    pub(in crate::platforms::plugins::real_context) sender_name: String,
    pub(in crate::platforms::plugins::real_context) timestamp: i64,
    pub(in crate::platforms::plugins::real_context) content: String,
    pub(in crate::platforms::plugins::real_context) reply_message_id: Option<String>,
    pub(in crate::platforms::plugins::real_context) reply_sender_id: Option<String>,
    pub(in crate::platforms::plugins::real_context) reply_sender_name: Option<String>,
    pub(in crate::platforms::plugins::real_context) reply_content: Option<String>,
    #[serde(default)]
    pub(in crate::platforms::plugins::real_context) mentioned_user_ids: Vec<String>,
    #[serde(default)]
    pub(in crate::platforms::plugins::real_context) mentioned_users: Vec<PlatformMention>,
    pub(in crate::platforms::plugins::real_context) supplemental: bool,
}

pub(in crate::platforms::plugins::real_context) struct PendingReply {
    pub(in crate::platforms::plugins::real_context) owner: TurnOwnership,
    pub(in crate::platforms::plugins::real_context) generation: u64,
    pub(in crate::platforms::plugins::real_context) started: Instant,
    pub(in crate::platforms::plugins::real_context) trigger: TriggerKind,
    /// 回复承诺已成立(直触发,或主动判断已通过)。补救窗口内的新消息
    /// 直接顶替目标而不再重新判断;未承诺(仍在判断中)则取消旧判断、
    /// 对新消息重新判断。
    pub(in crate::platforms::plugins::real_context) committed: bool,
    pub(in crate::platforms::plugins::real_context) reactions: Vec<(String, String)>,
    pub(in crate::platforms::plugins::real_context) targets: Vec<ActiveReplyTarget>,
    pub(in crate::platforms::plugins::real_context) cancel: tokio::sync::watch::Sender<bool>,
}

impl PendingReply {
    pub(in crate::platforms::plugins::real_context) fn supersede_for(&self, owner: &TurnOwnership) {
        // 活跃回合接管仍由同一个 context 消费后续消息，不能取消宿主回合。
        if !self.owner.same_turn(owner) {
            self.owner.supersede();
            self.cancel.send_replace(true);
        }
    }
}

pub(in crate::platforms::plugins::real_context) async fn wait_for_supersede(
    receiver: &mut tokio::sync::watch::Receiver<bool>,
) {
    if *receiver.borrow() {
        return;
    }
    while receiver.changed().await.is_ok() {
        if *receiver.borrow() {
            return;
        }
    }
}

#[derive(Default)]
pub(in crate::platforms::plugins::real_context) struct DynamicGate {
    pub(in crate::platforms::plugins::real_context) active: AtomicUsize,
    pub(in crate::platforms::plugins::real_context) notify: Notify,
}

impl DynamicGate {
    pub(in crate::platforms::plugins::real_context) async fn acquire(
        &self,
        limit: usize,
        timeout: Duration,
    ) -> Option<DynamicGatePermit<'_>> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let current = self.active.load(Ordering::Acquire);
            if current < limit.max(1)
                && self
                    .active
                    .compare_exchange(current, current + 1, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
            {
                return Some(DynamicGatePermit { gate: self });
            }
            if tokio::time::timeout_at(deadline, self.notify.notified())
                .await
                .is_err()
            {
                return None;
            }
        }
    }
}

pub(in crate::platforms::plugins::real_context) struct DynamicGatePermit<'a> {
    pub(in crate::platforms::plugins::real_context) gate: &'a DynamicGate,
}

impl Drop for DynamicGatePermit<'_> {
    fn drop(&mut self) {
        self.gate.active.fetch_sub(1, Ordering::AcqRel);
        self.gate.notify.notify_one();
    }
}

pub(super) fn group_key(context: &PlatformTurnContext) -> Result<GroupKey> {
    group_key_for(context, &context.conversation.conversation_id)
}

pub(super) fn group_key_for(context: &PlatformTurnContext, group_id: &str) -> Result<GroupKey> {
    GroupKey::new(
        context.conversation.platform.clone(),
        context.conversation.account_id.clone(),
        group_id.to_string(),
    )
}

pub(in crate::platforms::plugins::real_context) fn runtime_session_key(
    context: &PlatformTurnContext,
) -> String {
    format!(
        "{}|persona:{}",
        context.conversation.scope_key(),
        context.config.active_persona_scope()
    )
}

pub(in crate::platforms::plugins::real_context) fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
