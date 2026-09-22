//! Agent 的状态分组(09-16,core/normal 接口治理 Phase 2)。
//!
//! `Agent` 原来是 44 个平铺字段的大对象:回合里不变的配置、场所每回合塞进来的
//! 输入、跨回合的运行态、记忆子系统句柄混在一起,看一眼分不清哪些该在回合结束时
//! 清、哪些改了会掰缓存前缀。这里按**生命周期**分四组,字段本身、语义与初始值
//! 一个都没变(请求字节由 `tests/request_shape.rs` 量尺钉着):
//!
//! - [`CoreTurnSnapshot`]:构造 / `reload_config` 时定下,回合中不变;
//! - [`TurnInput`]:场所或插件在回合开始前设置、回合里取走或覆盖;
//! - [`TurnRuntime`]:跨回合的会话运行态;
//! - [`MemorySubsystem`]:记忆整套的句柄与库身份。
//!
//! 接下来给扩展的只读视图(`PromptContext` 一类)从这四组里挑字段组,不再把
//! `&Agent` 整个递出去。

use super::*;

/// 构造/重载时定下、回合中不变的快照。`dev` 在 `switch_mode`、`max_tool_rounds`
/// 在 `cap_tool_rounds_for_platform`、`spinner_interval` 在 `with_headless_pacing` 各改一次,
/// 都发生在回合之外。
pub(in crate::agent) struct CoreTurnSnapshot {
    /// 这一会话跑的是不是保留人格 `dev`(一行开发提示词、无人格全家、精简工具面)。
    /// 场所传进来的 [`PersonaLane`] 在构造 / `switch_lane` 边界折成这一位布尔;回合引擎
    /// 内部没有「模式」这个概念,dev 只是一张启用集为空的内置人格(09-16 退役)。
    pub(in crate::agent) dev: bool,
    /// 这一会话是不是子代理会话(09-18 会话化,`AgentProfile::Subagent`):非 dev 时
    /// 系统提示词换成通用子代理那份,子系统整套不构造,预设对话跳过。dev 子代理
    /// 就是 dev 人格本身,这一位只影响「非 dev」那半。
    pub(in crate::agent) subagent: bool,
    pub(in crate::agent) prompt_audience: PromptAudience,
    pub(in crate::agent) config: AppConfig,
    pub(in crate::agent) paths: MiyuPaths,
    /// 人格清单 × 机器配置折出来的子系统启用快照(`config::subsystems`):构造时
    /// 取一次、`reload_config` 重取;记忆前言、记忆库初始化、人格提醒都只看它,
    /// 不再各自读 persona.toml。中途改清单只影响之后新建的 Agent(daemon 每回合新建)。
    pub(in crate::agent) subsystems: miyu_base::config::EnabledSubsystems,
    pub(in crate::agent) trim_at_ratio: f32,
    /// 压缩的触发水位,低于 `trim_at_ratio`(见 config::ContextConfig)。
    pub(in crate::agent) compact_at_ratio: f32,
    pub(in crate::agent) trim_batch_ratio: f32,
    pub(in crate::agent) tools_enabled: bool,
    pub(in crate::agent) max_tool_rounds: usize,
    pub(in crate::agent) on_overflow: String,
    /// SpinnerTick 的发射周期。终端直连形态用 40ms 驱动动画；daemon 内
    /// 的回合（平台/WebUI/子代理）tick 出不了进程（event_map 丢弃），
    /// 唯一作用是给 journal 尾部冲刷兜底，200ms 足够——25Hz 定时器在
    /// 每次 LLM 往返全程空转是活跃期最大的无谓唤醒源。
    pub(in crate::agent) spinner_interval: std::time::Duration,
}

/// 场所/插件在回合前塞进来的本回合输入。每个字段都有对应的 setter(`setup.rs`),
/// 回合里 `take()` 或读一次;daemon 每回合新建 Agent,REPL 直连形态跨回合复用时由
/// setter 覆盖。
pub(in crate::agent) struct TurnInput {
    /// Raw user input snapshot taken before platform plugins wrapped the turn
    /// content (instruction boilerplate, group history, …). The memory diary
    /// records this instead of the wrapped prompt — the minimal C10 "记忆只读
    /// raw_content" separation. `None` on paths whose input is already raw
    /// (terminal, WebUI) and on redo replays.
    pub(in crate::agent) memory_content: Option<String>,
    pub(in crate::agent) suppress_session_history: bool,
    /// Per-run system additions supplied by a transport/plugin. They are
    /// intentionally excluded from prompt-change hashing and persistence.
    pub(in crate::agent) runtime_system_context: Vec<String>,
    /// Per-message transport context (sender identity JSON, message ids, …)
    /// rendered as a tail system message after the user turn. Kept out of the
    /// system prompt so the stable prefix stays byte-identical across turns.
    pub(in crate::agent) turn_system_context: Vec<String>,
    /// 程序驱动 CLI 的「整体替换提示词」。设了就顶掉人格/模式提示词与
    /// 属主主机环境块(风格锁等人格附件对纯后端用法是噪音);运行时追加段
    /// 与记忆前言照旧。刻意不进指纹:否则每个带覆盖的回合都翻转指纹文件。
    pub(in crate::agent) system_prompt_override: Option<String>,
    /// 程序驱动 CLI 的「本回合上下文窗口」。走字段而不改 config,免得冲刷
    /// 以整份 config 为键的 TurnResourceCache。
    pub(in crate::agent) context_window_override: Option<usize>,
    pub(in crate::agent) turn_display_content: Option<String>,
    pub(in crate::agent) attachment_run_id: Option<String>,
    pub(in crate::agent) image_platform: Option<String>,
    pub(in crate::agent) image_platform_label: Option<String>,
    /// 平台回合的窄端口(`platform_port::PlatformTurn`);终端 / WebUI 回合为 `None`。
    pub(in crate::agent) platform_context: Option<Arc<dyn PlatformTurn>>,
    pub(in crate::agent) context_images: Vec<PlatformContextImageRef>,
    /// Files from structured platform history that `read_platform_file` may
    /// resolve by their context id in this turn.
    pub(in crate::agent) context_files: Vec<PlatformContextFileRef>,
}

/// 跨回合的会话运行态。
pub(in crate::agent) struct TurnRuntime {
    /// 本回合的人格提醒全文(构造期为 None,`resolve_persona_reminder` 每回合
    /// 解析)。08-16 起不再浮动:每隔 `prompt.persona_reminder_interval` 轮以
    /// 化石身份进 `messages`(`history.rs` 注入,间隔按历史里最近一份提醒化石数),
    /// 纯追加、不掰前缀。蒸馏与缓存见 persona_hint 模块头。
    pub(in crate::agent) persona_reminder: Option<String>,
    /// Exact (messages, tools) of the most recent live request; feeds the
    /// idle cache-keepalive pings (v7 DeepSeek 高命中策略). Only populated
    /// while `cache.keepalive_seconds > 0`.
    pub(in crate::agent) last_request_snapshot:
        Option<(Vec<ChatMessage>, Vec<miyu_core::llm::ToolDefinition>)>,
    /// 中转(claude-code)侧闭环执行的工具活动,随回合收集、持久化成
    /// remote 标记的 ToolFlowRound(仅供 UI 重绘,不回放)。Mutex 只为在
    /// llm future 借用 self.client 期间也能从事件泵写入。
    pub(in crate::agent) pending_remote_tool_calls:
        std::sync::Mutex<Vec<miyu_core::state::ToolFlowCall>>,
    /// 上一条真实请求最终落在哪个 endpoint(provider_id, model):keepalive
    /// ping 必须钉住同一缓存域,轮转调度下打到别家=白花钱不保温
    /// (deepseek 报告 P2)。
    pub(in crate::agent) last_request_endpoint: Option<(String, String)>,
    /// Cancels the currently running keepalive loop, if any.
    pub(in crate::agent) keepalive_cancel: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// Consecutive auto-compactions that failed to bring the context back
    /// under the trigger. A healthy compaction lands below the trigger; two
    /// in a row mean the verbatim floor alone exceeds it (window too small),
    /// so auto-compaction latches off until the context drops (`compact_stuck`).
    pub(in crate::agent) consecutive_compacts: std::sync::atomic::AtomicU32,
    pub(in crate::agent) compact_stuck: std::sync::atomic::AtomicBool,
    /// Max turn seq observed right after the previous auto-compaction (-1 =
    /// none yet). A new compaction firing within a few turns of the last one
    /// means some single item (a huge paste or tool output) refills the
    /// window instantly — compacting harder won't help ("thrashing").
    pub(in crate::agent) last_compact_max_seq: std::sync::atomic::AtomicI64,
    pub(in crate::agent) rapid_compacts: std::sync::atomic::AtomicU32,
    /// 本回合已发出请求的累计用量镜像,回合守卫在打断时照它记账
    /// (见 `control::TurnUsageMirror`)。
    pub(in crate::agent) turn_usage: TurnUsageMirror,
}

/// 记忆子系统整套。`store` 是否真的建库由 `config::subsystems` 的快照裁决
/// (关着时 `database_id` 为空、`generation` 为 0)。
pub(in crate::agent) struct MemorySubsystem {
    pub(in crate::agent) store: MemoryStore,
    pub(in crate::agent) organizer: Option<MemoryOrganizerHandle>,
    pub(in crate::agent) origin: MemoryOrigin,
    pub(in crate::agent) database_id: String,
    pub(in crate::agent) generation: i64,
}
