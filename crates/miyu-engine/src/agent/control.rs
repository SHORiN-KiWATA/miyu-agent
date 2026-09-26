//! 回合的并发控制：占位守卫、超越信号、入队栅栏。
//!
//! 一个会话同时只能跑一个回合，但「同时」这件事有好几种发生方式：用户连发两
//! 条、重做撞上正在跑的回合、工具追加和主回合抢入队。这里的三个守卫各管一种。
//!
//! 全部实现了 `Drop`，且**回滚逻辑写在 Drop 里而不是正常路径上**——回合可能在
//! 任何一个 await 点被取消，只有 Drop 保证跑到。

use crate::agent::*;

pub(in crate::agent) const MAX_QUESTION_ROUNDS_PER_TURN: usize = 8;

/// 回合内「已经发出去的请求」累计用量的共享镜像。
///
/// 回合用量本来只在跑完那一刻写库,被打断的轮因此永远记 0——Σ 在打断那一刻
/// 掉回基线,本轮烧掉的全部消失(09-22 实测)。累计器本身是 `chat_with_tools`
/// 的栈上局部态,打断时随栈销毁,守卫够不着;所以每次请求入账后往这里同步
/// 一份,守卫 Drop 时照它记账。
///
/// 顺带记最后一次请求结束时的上下文占用:「本会话用量」工具在回合中途被调时,
/// 库里还只有上一轮的数,footer 显示的却是这一次请求的(09-24)。
#[derive(Clone, Default)]
pub(in crate::agent) struct TurnUsageMirror(Arc<Mutex<LiveTurnUsage>>);

#[derive(Clone, Default)]
struct LiveTurnUsage {
    tokens: TurnTokens,
    context: Option<u64>,
    endpoint: TurnEndpoint,
}

/// 最后一次请求是哪家哪个模型答的。被打断的轮原来不记，回放时收尾那行 `✻` 写不出模型，
/// 实时那一下却写着（09-26）；每次请求入账时和用量一起同步，打断时一起落库。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(in crate::agent) struct TurnEndpoint {
    pub(in crate::agent) provider_id: Option<String>,
    pub(in crate::agent) model: Option<String>,
}

impl TurnUsageMirror {
    pub(in crate::agent) fn set(&self, tokens: TurnTokens) {
        if let Ok(mut slot) = self.0.lock() {
            slot.tokens = tokens;
        }
    }

    pub(in crate::agent) fn set_context(&self, tokens: u64) {
        if let Ok(mut slot) = self.0.lock() {
            slot.context = Some(tokens);
        }
    }

    pub(in crate::agent) fn set_endpoint(
        &self,
        provider_id: Option<String>,
        model: Option<String>,
    ) {
        if let Ok(mut slot) = self.0.lock() {
            slot.endpoint = TurnEndpoint { provider_id, model };
        }
    }

    pub(in crate::agent) fn endpoint(&self) -> TurnEndpoint {
        self.0
            .lock()
            .map(|slot| slot.endpoint.clone())
            .unwrap_or_default()
    }

    pub(in crate::agent) fn reset(&self) {
        if let Ok(mut slot) = self.0.lock() {
            *slot = LiveTurnUsage::default();
        }
    }

    pub(in crate::agent) fn get(&self) -> TurnTokens {
        self.0.lock().map(|slot| slot.tokens).unwrap_or_default()
    }

    /// 这一轮最后一次请求结束时的上下文占用;这一轮还没发过请求就是 None。
    pub(in crate::agent) fn context(&self) -> Option<u64> {
        self.0.lock().ok().and_then(|slot| slot.context)
    }
}

pub struct PendingTurnGuard {
    pub(in crate::agent) state: StateStore,
    pub(in crate::agent) turn_id: String,
    pub(in crate::agent) completed: bool,
    pub(in crate::agent) usage: TurnUsageMirror,
}

impl PendingTurnGuard {
    pub fn new(state: StateStore, turn_id: String) -> Self {
        Self {
            state,
            turn_id,
            completed: false,
            usage: TurnUsageMirror::default(),
        }
    }

    /// 打断时按这份镜像记账。
    pub(in crate::agent) fn with_usage_mirror(mut self, usage: TurnUsageMirror) -> Self {
        self.usage = usage;
        self
    }

    /// 收尾：完成标记和 `extras` 同一个事务落库。失败时守卫照常在 Drop 里把这一轮
    /// 收成已中断——库里不会出现「已完成、工具流还是上一个检查点」的轮。
    pub fn finish(
        mut self,
        done: &TurnCompletion<'_>,
        extras: &TurnFinishExtras<'_>,
    ) -> Result<()> {
        self.state.finish_turn(&self.turn_id, done, extras)?;
        self.completed = true;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn interrupt(&mut self) -> Result<()> {
        if !self.completed {
            self.state
                .interrupt_turn_with_usage(&self.turn_id, self.usage.get())?;
            record_endpoint(&self.state, &self.turn_id, &self.usage.endpoint());
            self.completed = true;
        }
        Ok(())
    }
}

impl Drop for PendingTurnGuard {
    fn drop(&mut self) {
        if !self.completed {
            if let Err(error) = settle_unfinished_turn(
                &self.state,
                &self.turn_id,
                self.usage.get(),
                &self.usage.endpoint(),
                miyu_base::process::daemon_shutting_down(),
            ) {
                tracing::error!(
                    turn_id = %self.turn_id,
                    error = %error,
                    "failed to persist an interrupted turn"
                );
            }
        }
    }
}

/// 没跑完就被丢下的回合怎么落库。人按停止（或回合出错）：收成「已中断」。daemon
/// 有序关停（重启、换二进制）：只记用量、留着「执行中」——下一个 daemon 才认得出
/// 这一轮是被重启打断的，接着跑（09-24 断点续跑）。两种都记账。
pub(in crate::agent) fn settle_unfinished_turn(
    state: &StateStore,
    turn_id: &str,
    usage: TurnTokens,
    endpoint: &TurnEndpoint,
    daemon_shutting_down: bool,
) -> Result<()> {
    if daemon_shutting_down {
        return state.suspend_turn_with_usage(turn_id, usage);
    }
    state.interrupt_turn_with_usage(turn_id, usage)?;
    record_endpoint(state, turn_id, endpoint);
    Ok(())
}

/// 被打断的轮记下最后一次请求的供应商和模型；记不上不算打断失败。
fn record_endpoint(state: &StateStore, turn_id: &str, endpoint: &TurnEndpoint) {
    if endpoint.provider_id.is_none() && endpoint.model.is_none() {
        return;
    }
    if let Err(error) = state.record_turn_endpoint(
        turn_id,
        endpoint.provider_id.as_deref(),
        endpoint.model.as_deref(),
    ) {
        tracing::debug!(turn_id, error = %error, "interrupted turn endpoint not recorded");
    }
}

pub(in crate::agent) struct PendingRedoGuard {
    pub(in crate::agent) state: StateStore,
    pub(in crate::agent) turn_id: String,
    pub(in crate::agent) revision: i64,
    pub(in crate::agent) completed: bool,
    pub(in crate::agent) usage: TurnUsageMirror,
}

impl PendingRedoGuard {
    pub fn new(state: StateStore, turn_id: String, revision: i64) -> Self {
        Self {
            state,
            turn_id,
            revision,
            completed: false,
            usage: TurnUsageMirror::default(),
        }
    }

    /// 打断时按这份镜像记账,同 `PendingTurnGuard`。
    pub(in crate::agent) fn with_usage_mirror(mut self, usage: TurnUsageMirror) -> Self {
        self.usage = usage;
        self
    }

    /// 同 `PendingTurnGuard::finish`，写的是这一版修订。
    pub(in crate::agent) fn finish(
        mut self,
        done: &TurnCompletion<'_>,
        extras: &TurnFinishExtras<'_>,
    ) -> Result<()> {
        self.state
            .finish_turn_revision(&self.turn_id, self.revision, done, extras)?;
        self.completed = true;
        Ok(())
    }
}

impl Drop for PendingRedoGuard {
    fn drop(&mut self) {
        if !self.completed {
            match self.state.interrupt_turn_revision_with_usage(
                &self.turn_id,
                self.revision,
                self.usage.get(),
            ) {
                Ok(()) => record_endpoint(&self.state, &self.turn_id, &self.usage.endpoint()),
                Err(error) => tracing::error!(
                    turn_id = %self.turn_id,
                    revision = self.revision,
                    error = %error,
                    "failed to recover an interrupted redo generation"
                ),
            }
        }
    }
}

pub struct RedoPromptInput {
    pub prompt_id: String,
    pub content: String,
    pub display_content: String,
    pub images: Vec<Option<PastedImage>>,
}

#[derive(Clone)]
pub struct AgentTurnControl {
    pub(in crate::agent) lane: Arc<Mutex<PersonaLane>>,
    pub(in crate::agent) active_tools: ToolRegistry,
    pub(in crate::agent) dev_tools: ToolRegistry,
    pub(in crate::agent) queue_ingress: Option<Arc<QueueIngressBarrier>>,
    pub(in crate::agent) supersede: Option<Arc<TurnSupersedeSignal>>,
    pub(in crate::agent) supersede_seen: Arc<AtomicU64>,
    pub(in crate::agent) compact: Option<Arc<TurnCompactRequest>>,
}

/// 回合跑着的时候有人敲了 `/compact`（09-25，用户：「像 followup 消息一样排队」）。回合
/// 循环在接插话的那两处（一批工具跑完、模型要收尾时）看一眼，立着就先把本轮之前的历史
/// 压掉再接着跑。守护进程按会话各存一份，这一轮没走到那两处就退场的，由它在回合退场后补压。
#[derive(Default)]
pub struct TurnCompactRequest {
    requested: std::sync::atomic::AtomicBool,
}

impl TurnCompactRequest {
    pub fn request(&self) {
        self.requested.store(true, Ordering::Release);
    }

    pub fn is_pending(&self) -> bool {
        self.requested.load(Ordering::Acquire)
    }

    /// 取走请求：只有一处能拿到 `true`（同一会话里并行的几轮、回合与守护进程之间不会压两遍）。
    pub fn take(&self) -> bool {
        self.requested.swap(false, Ordering::AcqRel)
    }
}

#[derive(Default)]
pub struct TurnSupersedeSignal {
    pub(in crate::agent) generation: AtomicU64,
    pub(in crate::agent) changed: Notify,
}

impl TurnSupersedeSignal {
    pub fn trigger(&self) -> u64 {
        let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        self.changed.notify_waiters();
        generation
    }

    pub(in crate::agent) fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    pub(in crate::agent) async fn wait_after(&self, observed: u64) {
        loop {
            let changed = self.changed.notified();
            if self.generation() != observed {
                return;
            }
            changed.await;
        }
    }
}

#[derive(Default)]
pub struct QueueIngressBarrier {
    pub(in crate::agent) state: Mutex<QueueIngressState>,
    pub(in crate::agent) changed: Notify,
}

#[derive(Default)]
pub(in crate::agent) struct QueueIngressState {
    pub(in crate::agent) active_calls: HashSet<String>,
    pub(in crate::agent) reservations: usize,
    pub(in crate::agent) closed: bool,
}

pub struct QueueIngressReservation {
    pub(in crate::agent) barrier: Arc<QueueIngressBarrier>,
}

impl QueueIngressBarrier {
    pub fn tool_started(&self, call_id: &str) {
        let mut state = self.state.lock().unwrap();
        if !state.closed {
            state.active_calls.insert(call_id.to_string());
        }
    }

    pub fn tool_finished(&self, call_id: &str) {
        self.state.lock().unwrap().active_calls.remove(call_id);
        self.changed.notify_waiters();
    }

    pub fn try_reserve(self: &Arc<Self>) -> Option<QueueIngressReservation> {
        let mut state = self.state.lock().unwrap();
        if state.closed || state.active_calls.is_empty() {
            return None;
        }
        state.reservations = state.reservations.saturating_add(1);
        Some(QueueIngressReservation {
            barrier: self.clone(),
        })
    }

    pub fn close(&self) {
        let mut state = self.state.lock().unwrap();
        state.closed = true;
        state.active_calls.clear();
        self.changed.notify_waiters();
    }

    pub(in crate::agent) async fn wait_for_reserved_ingress(&self) {
        loop {
            let changed = self.changed.notified();
            if self.state.lock().unwrap().reservations == 0 {
                return;
            }
            changed.await;
        }
    }
}

impl Drop for QueueIngressReservation {
    fn drop(&mut self) {
        let mut state = self.barrier.state.lock().unwrap();
        state.reservations = state.reservations.saturating_sub(1);
        self.barrier.changed.notify_waiters();
    }
}

impl AgentTurnControl {
    pub fn new(lane: PersonaLane, active_tools: ToolRegistry, dev_tools: ToolRegistry) -> Self {
        Self {
            lane: Arc::new(Mutex::new(lane)),
            active_tools,
            dev_tools,
            queue_ingress: None,
            supersede: None,
            supersede_seen: Arc::new(AtomicU64::new(0)),
            compact: None,
        }
    }

    pub fn set_compact_request(&mut self, request: Arc<TurnCompactRequest>) {
        self.compact = Some(request);
    }

    /// 这一轮排着一次压缩就取走它（见 [`TurnCompactRequest`]）。
    pub(in crate::agent) fn take_compact_request(&self) -> bool {
        self.compact.as_ref().is_some_and(|request| request.take())
    }

    pub fn set_queue_ingress(&mut self, ingress: Arc<QueueIngressBarrier>) {
        self.queue_ingress = Some(ingress);
    }

    pub fn set_supersede_signal(&mut self, signal: Arc<TurnSupersedeSignal>) {
        self.supersede = Some(signal);
    }

    pub(in crate::agent) fn pending_supersede_generation(&self) -> Option<u64> {
        let generation = self.supersede.as_ref()?.generation();
        (generation != self.supersede_seen.load(Ordering::Acquire)).then_some(generation)
    }

    pub(in crate::agent) fn mark_supersede_seen(&self, generation: u64) {
        self.supersede_seen.store(generation, Ordering::Release);
    }

    pub fn lane(&self) -> PersonaLane {
        *self.lane.lock().unwrap()
    }

    pub fn set_lane(&self, lane: PersonaLane) {
        *self.lane.lock().unwrap() = lane;
    }

    pub(in crate::agent) fn tools(&self, lane: PersonaLane) -> ToolRegistry {
        match lane {
            PersonaLane::Active => self.active_tools.clone(),
            PersonaLane::Dev => self.dev_tools.clone(),
        }
    }
}
