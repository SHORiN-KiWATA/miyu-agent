mod context;
mod context_meter;
mod control;
mod history;
mod images;
mod input;
mod instruction_source;
mod interrupted_replay;
mod platform_port;
pub use platform_port::PlatformTurn;
mod journal;
pub(crate) mod prompt;
pub use prompt::prompt_strip_tagged;
mod pruning;
mod reasoning;
mod reports;
mod setup;
pub use setup::AgentProfile;
mod tool_report;
mod turn_state;
// dto 要按同一条规则判定工具成败(见 web::dto::tool_call_succeeded),
// 规则只能有一份。
use context::*;
use control::*;
pub use tool_report::tool_output_succeeded;
// agent 之外要用的几样：CLI、web、runtime、platforms 都会拿回合控制与模式，
// 子模块本身是私有的，得显式再导出
pub use context::archive_and_delete_visible_turns;
pub use control::{
    AgentTurnControl, QueueIngressBarrier, QueueIngressReservation, RedoPromptInput,
    TurnCompactRequest, TurnSupersedeSignal,
};
// 平台侧的 PDF 工具要问同一个能力判定,不能自己另写一份"池吃不吃 PDF"。
pub use images::active_text_pool_supports_pdf;
use images::*;
use instruction_source::*;
use interrupted_replay::*;
use journal::*;
use prompt::*;
use reasoning::*;
use reports::*;
use tool_report::*;
use turn_state::*;
mod compact;
mod compact_analysis;
mod compact_extras;
mod compact_structure;
mod compact_transcript;
mod conversation;
mod describe;
pub(crate) mod overflow;
mod turn_loop;

use crate::tools::{self, memes, vision, ToolRegistry};
use anyhow::{bail, Context, Result};
use base64::Engine;
use chrono::Local;
use miyu_base::clipboard::{ClipboardImage, PastedImage};
pub use miyu_base::config::PersonaLane;
use miyu_base::config::{AppConfig, PromptAudience};
use miyu_base::host_info::xml_attr_escape;
use miyu_base::paths::MiyuPaths;
use miyu_base::platform_types::{PlatformContextFileRef, PlatformContextImageRef};
use miyu_base::question::{
    answered_tool_output, closed_tool_output, unavailable_tool_output, QuestionCancelled,
    QuestionExchange, QuestionRequest, QuestionResponse,
};
use miyu_core::llm::{
    ChatContent, ChatContentPart, ChatMessage, ChatResult, ChatStreamChunk, ChatStreamKind,
    GenerationSpeed, ImageUrlContent, OpenAiCompatibleClient, ToolCall, ToolCallFunction,
    TurnTokens, Usage,
};
use miyu_core::memory::{
    EvictedTurn, MemoryAccess, MemoryOrganizerHandle, MemoryOrigin, MemoryResetSummary, MemoryStore,
};
use miyu_core::persona_hint;
use miyu_core::state::{
    QueuedPrompt, QueuedPromptAttachment, RedoCandidate, RedoInputKind, StateStore, TurnCompletion,
    TurnFinishExtras, TurnRedoCheckpointPayload,
};
use serde_json::Value;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot, Notify};

#[derive(Debug)]
pub enum AgentEvent {
    TurnStarted {
        turn_id: String,
    },
    Chunk(ChatStreamChunk),
    /// Raw provider reasoning, persisted before the UI title/body filter.
    /// This event is consumed by `TurnJournalSink` and is never shown to a
    /// transport directly.
    RawReasoning(ChatStreamChunk),
    /// Internal durability barrier used before non-stream state mutations that
    /// create journal boundaries.
    FlushJournal,
    ReasoningStart {
        received_at: Instant,
    },
    ReasoningReset {
        received_at: Instant,
    },
    ReasoningPartStart {
        received_at: Instant,
    },
    ReasoningPartEnd {
        received_at: Instant,
    },
    ReasoningTitle(String),
    ToolCall {
        call_id: String,
        name: String,
        arguments: String,
    },
    ToolPreparing {
        name: String,
        /// 本轮里这不是第一个工具调用。渲染层据此在工具自己没有提示词时
        /// 退到通用的「准备工具」。
        batch: bool,
    },
    ToolResult {
        call_id: String,
        name: String,
        ok: bool,
        output: String,
    },
    ToolProgress {
        call_id: String,
        name: String,
        message: String,
    },
    /// 前台子代理此刻的样子：窥视、词元、子会话（会话项目第 4 段之二）。子会话里的过程
    /// 不再原样转成工具进度，界面只画状态行那一行，点它切进子会话看全程。
    SubagentProgress {
        call_id: String,
        name: String,
        status: tools::subagent::status::SubagentStatus,
    },
    CommandOutput {
        call_id: String,
        name: String,
        stream: tools::CommandOutputStream,
        chunk: Vec<u8>,
    },
    PrepareForExternalOutput {
        ready: oneshot::Sender<bool>,
    },
    Image {
        call_id: String,
        name: String,
        path: PathBuf,
        alt: String,
        size: Option<String>,
    },
    Artifact {
        call_id: String,
        name: String,
        path: PathBuf,
        title: String,
    },
    AskQuestion {
        call_id: String,
        request: QuestionRequest,
        responder: oneshot::Sender<QuestionResponse>,
    },
    QueuedPromptsConsumed {
        prompt_ids: Vec<String>,
        mode: PersonaLane,
        provider_id: Option<String>,
        model: Option<String>,
    },
    GenerationSuperseded {
        prompt_ids: Vec<String>,
    },
    /// 回合内每次模型请求结束时的用量快照:`round` 是刚结束这次请求的
    /// 用量(其 prompt+completion ≈ 当前上下文占用),`turn` 是回合开始
    /// 至今的累计。终端 footer 和 WebUI 用它逐请求刷新计量,不必等整个
    /// 回合(可能含多轮工具调用)结束。
    RoundUsage {
        round: Box<Usage>,
        turn: TurnTokens,
        /// 会话实时累计:已落库的各回合 + 已完成的子代理子会话 + 本回合至今的 `turn`。
        /// WebUI 据它逐请求刷新输入框那个「累计」,不必等整回合结束(#131:子代理跑
        /// 完的花销也随之在下一个主回合体现)。
        cumulative: TurnTokens,
        /// 回合至今的输出速度样本(见 `Usage::generation_ms`)。
        speed: GenerationSpeed,
        estimated: bool,
        /// 刚结束这次请求实际应答的端点,供日志/前端标注(08-24 需求)。
        provider_id: Option<String>,
        model: Option<String>,
    },
    SpinnerTick,
    CompactStart,
    CompactChunk(ChatStreamChunk),
    CompactEnd,
    PopStart,
    PopEnd,
    /// One-shot operational notice shown to the user (e.g. auto-compaction
    /// paused because the window is too small).
    Notice {
        text: String,
    },
}

fn emit_tool_progress<F>(
    on_event: &mut F,
    call_id: &str,
    name: &str,
    progress: tools::ToolProgressEvent,
) -> Result<()>
where
    F: FnMut(AgentEvent) -> Result<()>,
{
    match progress {
        tools::ToolProgressEvent::Message(message) => on_event(AgentEvent::ToolProgress {
            call_id: call_id.to_string(),
            name: name.to_string(),
            message,
        }),
        tools::ToolProgressEvent::PrepareForExternalOutput { ready } => {
            on_event(AgentEvent::PrepareForExternalOutput { ready })
        }
        tools::ToolProgressEvent::Image { path, alt, size } => on_event(AgentEvent::Image {
            call_id: call_id.to_string(),
            name: name.to_string(),
            path,
            alt,
            size,
        }),
        tools::ToolProgressEvent::Artifact { path, title } => on_event(AgentEvent::Artifact {
            call_id: call_id.to_string(),
            name: name.to_string(),
            path,
            title,
        }),
        tools::ToolProgressEvent::CommandOutput { stream, chunk } => {
            on_event(AgentEvent::CommandOutput {
                call_id: call_id.to_string(),
                name: name.to_string(),
                stream,
                chunk,
            })
        }
    }
}

pub struct Agent {
    state: StateStore,
    client: OpenAiCompatibleClient,
    system_prompt: String,
    /// 场所/插件在回合前塞进来的本回合输入(平台上下文、附件、系统侧追加段等),回合取走或覆盖。
    input: TurnInput,
    /// 构造/重载时定下、回合中不变的快照:模式、受众、配置、路径、子系统启用集与几个上限。
    core: CoreTurnSnapshot,
    tools: Arc<Mutex<ToolRegistry>>,
    /// 情境化工具的原件(dev 专用,见 `apply_situational_tools`):判据要会话
    /// 状态,建表时拿不到,所以每回合装配时增删。REPL 的 Agent 跨回合复用,
    /// 判据可能从假翻真(切到 stub 档、`/pop` 出内容),摘掉之后必须放得回来
    /// ——原件留在这里,不重建。
    situational_tools: Vec<Arc<crate::tools::ToolSpec>>,
    /// 记忆子系统整套:库句柄、整理器、归属与库身份(按人格清单构造,见 config::subsystems)。
    memory: MemorySubsystem,
    /// 跨回合的会话运行态:人格提醒、缓存保活快照、中转工具活动、压缩自保护计数。
    runtime: TurnRuntime,
    /// 人类新输入(新回合/排队插话)重置;注入的提醒只进本轮工作消息,
    /// 不进化石。
    /// 预设对话(begin_dialogs):system 之后、真实历史之前的 user/assistant
    /// 示例对,每请求注入、永不落库。构造时从当前人格 scope 的
    /// dialogs/<scope>.md 加载。
    preset_dialogs: Vec<(String, String)>,
}

struct PreparedUserInput {
    content: String,
    message: ChatMessage,
    hints: Vec<ChatMessage>,
}

impl Agent {
    /// /reset-all-memory:清空本模式人格的长期记忆(会话历史/技能不动),
    /// 然后重建句柄。dev 作用域由构造期的 dev_scoped 配置自动继承。
    pub fn wipe_memory(&mut self) -> Result<()> {
        self.memory.store.reset_all()?;
        self.reset_memory()
    }

    /// /reset-memory:只清本会话产生的那部分记忆。改动之前存下的旧行没有
    /// 会话标记,只能走 `wipe_memory`。
    pub fn wipe_session_memory(&mut self) -> Result<MemoryResetSummary> {
        let summary = self
            .memory
            .store
            .reset_session(&self.memory.origin.session_id)?;
        self.reset_memory()?;
        Ok(summary)
    }

    pub fn reset_memory(&mut self) -> Result<()> {
        let (access, writer_principal, writer_display_name) = self.memory.store.request_context();
        self.memory.store = MemoryStore::new(&self.core.config, &self.core.paths)
            .with_request_context(access, writer_principal, writer_display_name)
            .with_session_id(&self.memory.origin.session_id);
        self.memory.store.init()?;
        (self.memory.database_id, self.memory.generation) = self.memory.store.identity()?;
        Ok(())
    }

    pub async fn chat_stream<F>(&mut self, input: &str, on_event: F) -> Result<ChatResult>
    where
        F: FnMut(AgentEvent) -> Result<()>,
    {
        self.chat_stream_with_images(input, &[], on_event).await
    }

    pub async fn chat_stream_with_images<F>(
        &mut self,
        input: &str,
        images: &[Option<PastedImage>],
        on_event: F,
    ) -> Result<ChatResult>
    where
        F: FnMut(AgentEvent) -> Result<()>,
    {
        self.chat_stream_with_images_inner(input, images, None, on_event)
            .await
    }

    pub async fn chat_stream_with_control<F>(
        &mut self,
        input: &str,
        images: &[Option<PastedImage>],
        control: &AgentTurnControl,
        on_event: F,
    ) -> Result<ChatResult>
    where
        F: FnMut(AgentEvent) -> Result<()>,
    {
        self.chat_stream_with_images_inner(input, images, Some(control), on_event)
            .await
    }

    pub async fn redo_stream_with_control<F>(
        &mut self,
        candidate: &RedoCandidate,
        prompts: Vec<RedoPromptInput>,
        control: &AgentTurnControl,
        on_event: F,
    ) -> Result<ChatResult>
    where
        F: FnMut(AgentEvent) -> Result<()>,
    {
        let session = self.state.session_id();
        miyu_base::workspace::with_session(
            session,
            self.redo_stream_turn(candidate, prompts, control, on_event),
        )
        .await
    }

    fn wake_memory_organizer(&self) {
        if let Some(organizer) = &self.memory.organizer {
            organizer.wake(
                self.core.config.clone(),
                self.core.paths.clone(),
                self.state.clone(),
            );
        }
    }

    pub async fn handle_overflow_after_turn<F>(
        &self,
        context_tokens: u64,
        on_event: F,
    ) -> Result<Option<ChatResult>>
    where
        F: FnMut(AgentEvent) -> Result<()>,
    {
        let mut on_event = on_event;
        let Some(compact) = self.handle_overflow(context_tokens, &mut on_event).await? else {
            return Ok(None);
        };
        self.state.add_auxiliary_usage(
            &compact.usage,
            miyu_core::state::UsageMeta {
                source: self.usage_source(),
                provider: compact.provider_id.as_deref(),
                model: None,
                kind: None,
            },
        )?;
        Ok(Some(ChatResult {
            content: String::new(),
            reasoning: None,
            usage: Some(compact.usage),
            usage_estimated: compact.usage_estimated,
            tool_calls: Vec::new(),
            provider_id: None,
            model: None,
            finish_reason: None,
            thinking_signature: None,
            last_request_usage: None,
            responses_continuation: None,
        }))
    }

    pub async fn compact_now<F>(&self, on_event: F) -> Result<Option<ChatResult>>
    where
        F: FnMut(AgentEvent) -> Result<()>,
    {
        let mut on_event = on_event;
        let context_window = match self.context_window() {
            Some(window) => Some(window),
            None if miyu_base::models_cache::is_loaded() => None,
            None => {
                // refresh_blocking 持全局 REFRESH_LOCK 做最长 30s 的阻塞
                // 网络请求:在 actor 的单线程 runtime 上同步调用会把所有
                // 会话所有 turn 一起冻结,必须移到阻塞线程池。
                let paths = self.core.paths.clone();
                let refreshed = tokio::task::spawn_blocking(move || {
                    miyu_base::models_cache::refresh_blocking(&paths).is_ok()
                })
                .await
                .unwrap_or(false);
                if refreshed {
                    self.context_window()
                } else {
                    None
                }
            }
        };
        let Some(context_window) = context_window else {
            let missing = self.client.models_without_context_window(&self.core.config);
            if missing.is_empty() {
                bail!(
                    "{}",
                    miyu_base::i18n::text(
                        "The current model's context window is not loaded or configured, so the context cannot be compacted",
                        "当前模型的上下文窗口尚未加载或未配置，无法压缩上下文"
                    )
                );
            }
            bail!(
                "{}{}",
                miyu_base::i18n::text(
                    "The context windows for these active models are not loaded or configured, so the context cannot be compacted: ",
                    "以下活动模型的上下文窗口尚未加载或未配置，无法压缩上下文："
                ),
                missing.join(", ")
            );
        };
        let visible_count = self.state.load_visible_turns()?.len();
        if visible_count == 0 {
            return Ok(None);
        }
        // 手动压缩不看水位,这里只借它算 reserved_tokens。
        let check =
            overflow::OverflowCheck::new(Some(context_window), self.core.compact_at_ratio, None);
        on_event(AgentEvent::CompactStart)?;
        let compactor = compact::Compactor::new(
            self.client.clone(),
            self.state.clone(),
            context_window,
            check.reserved_tokens,
            self.compact_tail_budget(context_window),
            self.preset_dialogs.len(),
        )
        .with_extras(self.compact_extras_policy());
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
        // Manual /compact is an explicit user request: bypass the
        // fold-economics gate (but tail retention still applies).
        let compact = match compactor
            .perform_compact(true, false, fork_builder, &mut on_chunk)
            .await
        {
            Ok(result) => {
                on_event(AgentEvent::CompactEnd)?;
                result
            }
            Err(err) => {
                on_event(AgentEvent::CompactEnd)?;
                return Err(err);
            }
        };
        let Some(compact) = compact else {
            return Ok(None);
        };
        self.state.add_auxiliary_usage(
            &compact.usage,
            miyu_core::state::UsageMeta {
                source: self.usage_source(),
                provider: compact.provider_id.as_deref(),
                model: None,
                kind: None,
            },
        )?;
        Ok(Some(ChatResult {
            content: String::new(),
            reasoning: None,
            usage: Some(compact.usage),
            usage_estimated: compact.usage_estimated,
            tool_calls: Vec::new(),
            provider_id: None,
            model: None,
            finish_reason: None,
            thinking_signature: None,
            last_request_usage: None,
            responses_continuation: None,
        }))
    }
}

/// keepalive 循环是 `tokio::spawn` 出去的独立任务，只认那个 `AtomicBool`。
///
/// `Agent` 被丢掉时——回合结束、会话切换、平台回合收尾——没人翻这个标志，
/// 任务就会继续按 interval 发请求。那不只是内存和线程，**是真的在花钱**：
/// 每次 ping 都是一次带完整前缀的 LLM 请求。
///
/// 原来只在「新回合开始」时取消（`chat_stream_turn` / `redo_stream_turn`），
/// 而每个平台回合用的是一个临时 `Agent`，跑完就丢，那条路上永远轮不到取消。
impl Drop for Agent {
    fn drop(&mut self) {
        self.cancel_cache_keepalive();
    }
}

#[derive(Default)]
struct UsageAccumulator {
    prompt_tokens: u64,
    completion_tokens: u64,
    total_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
    reasoning_tokens: u64,
    cache_reported: bool,
    has_usage: bool,
    estimated: bool,
    generation_tokens: u64,
    generation_ms: u64,
}

impl UsageAccumulator {
    /// 一次模型请求的用量入账,返回这次请求贡献的 completion tokens
    /// (中转线的结果帧是整轮累计,所以用入账前后的差值而不是直接读)。
    fn add_result(&mut self, result: &ChatResult, request_messages: &[ChatMessage]) -> u64 {
        let before = self.completion_tokens;
        self.add_result_inner(result, request_messages);
        self.completion_tokens.saturating_sub(before)
    }

    /// 一次请求的输出速度样本:只在用量是供应商真报的、且流里至少有两个
    /// 块(测得出时长)时入账。
    fn add_generation_sample(&mut self, tokens: u64, millis: u64, estimated: bool) {
        if estimated || tokens == 0 || millis == 0 {
            return;
        }
        self.generation_tokens = self.generation_tokens.saturating_add(tokens);
        self.generation_ms = self.generation_ms.saturating_add(millis);
    }

    fn add_result_inner(&mut self, result: &ChatResult, request_messages: &[ChatMessage]) {
        if let Some(usage) = &result.usage {
            self.add_usage(usage, false);
            return;
        }

        let prompt_tokens = overflow::estimate_messages_tokens(request_messages) as u64;
        let completion_tokens = estimate_result_tokens(result) as u64;
        self.add_usage(
            &Usage {
                prompt_tokens,
                completion_tokens,
                total_tokens: prompt_tokens.saturating_add(completion_tokens),
                ..Usage::default()
            },
            true,
        );
    }

    fn add_usage(&mut self, usage: &Usage, estimated: bool) {
        self.prompt_tokens = self.prompt_tokens.saturating_add(usage.prompt_tokens);
        self.completion_tokens = self
            .completion_tokens
            .saturating_add(usage.completion_tokens);
        let total = usage.effective_total_tokens();
        self.total_tokens = self.total_tokens.saturating_add(total);
        self.cache_read_tokens = self
            .cache_read_tokens
            .saturating_add(usage.cache_read_tokens);
        self.cache_write_tokens = self
            .cache_write_tokens
            .saturating_add(usage.cache_write_tokens);
        self.reasoning_tokens = self.reasoning_tokens.saturating_add(usage.reasoning_tokens);
        self.cache_reported |= usage.cache_reported;
        self.has_usage = true;
        self.estimated |= estimated;
    }

    fn usage(&self) -> Option<Usage> {
        self.has_usage.then_some(Usage {
            prompt_tokens: self.prompt_tokens,
            completion_tokens: self.completion_tokens,
            total_tokens: self.total_tokens,
            cache_read_tokens: self.cache_read_tokens,
            cache_write_tokens: self.cache_write_tokens,
            reasoning_tokens: self.reasoning_tokens,
            cache_reported: self.cache_reported,
            generation_tokens: self.generation_tokens,
            generation_ms: self.generation_ms,
            ..Usage::default()
        })
    }

    fn generation_speed(&self) -> GenerationSpeed {
        GenerationSpeed {
            tokens: self.generation_tokens,
            millis: self.generation_ms,
        }
    }
}

/// 落库保留模型原样发来的参数——包括半截 JSON。发上线前的合法性由
/// `ChatMessage::assistant` 统一收口；这里净化掉反而会毁掉唯一的取证来源
/// （08-17 那次 500 就是靠 turn_flow 里存着的 `{"action": "mute",
/// "duration_seconds": ` 才定位到的）。

/// Appends the static host block to the stable prefix.
///
/// It belongs here rather than in the per-turn `<runtime …/>` tail: the tail is
/// fossilized into `turns.context_messages` and replayed byte-for-byte by every
/// later turn, so a process-constant put there is re-sent once per turn and
/// piles up in the request; in the system prompt it is paid once and then
/// served from the provider's prefix cache.
///
/// Only owner sessions get it. A QQ reply has no use for kernel versions, and
/// skipping the append outright — rather than adding an empty block — keeps
/// those sessions' system prompt byte-identical to what the provider already
/// has cached, so the platform side sees no cold start at all.

#[cfg(test)]
mod tests;
