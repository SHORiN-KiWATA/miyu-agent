//! 帧格式与请求/响应类型。
//!
//! `MAX_FRAME_BYTES` 是必须的：帧长度是从对端读来的数字，照着它分配就等于把内
//! 存交给对端——即便对端是自己人，一个字段写错也够崩。
//!
//! `PROTOCOL_VERSION` 让新旧客户端能互相识别：版本不匹配时明确报出来，比字段
//! 缺失导致的怪行为好排查得多。

use crate::ipc::*;

pub const PROTOCOL_VERSION: u16 = 3;

pub(crate) const MAX_FRAME_BYTES: usize = 24 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionState {
    pub context_tokens: u64,
    pub context_window: Option<usize>,
    /// `context_window` 是不是猜出来的。默认 false——跟老 daemon 说话时按
    /// 「有出处」处理，显示跟以前一模一样，不会平白多出一堆波浪号。
    #[serde(default)]
    pub context_window_assumed: bool,
    pub cumulative_tokens: u64,
    /// Prompt and cache-read halves behind Σ's cache rate. Defaulted so a REPL
    /// talking to an older daemon degrades to "no cache reported" instead of
    /// failing to parse the state frame.
    #[serde(default)]
    pub cumulative_prompt_tokens: u64,
    #[serde(default)]
    pub cumulative_cache_read_tokens: u64,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub session_name: String,
    /// `/sandbox` 绑的根目录;None = 没绑(成员会话这里也是 None,他们的沙盒不在
    /// 会话记录里)。跟着全局「默认开启沙盒」走的会话,这里是默认根
    /// (`sandbox_default` 为真)。
    #[serde(default)]
    pub sandbox: Option<String>,
    /// 上面那个根来自全局默认,不是这个会话自己绑的(09-23)。
    #[serde(default)]
    pub sandbox_default: bool,
    /// 只读模式开着(09-23,Tab 切换):读全盘、哪儿都不许写。
    #[serde(default)]
    pub sandbox_readonly: bool,
    /// 绑了沙盒时,根之外还能写/读什么(给 `/sandbox` 查看用;`session_state_for` 填)。
    #[serde(default)]
    pub sandbox_writable: Vec<String>,
    #[serde(default)]
    pub sandbox_readable: Vec<String>,
    /// 这条会话跑在哪个模式上：`"dev"` | `"normal"`，daemon 按会话人格推导（模式钉在
    /// 会话上，见 `turn_mode_for_session`）。REPL 切到另一侧的会话时靠它把自己的
    /// 车道跟过去。老 daemon 给空串——客户端按当前模式处理。
    #[serde(default)]
    pub mode: String,
}

/// 输入框右上角那行 `/goal …` 提示要的全部信息。见 [`IpcCommand::GoalStatus`]。
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct GoalHint {
    /// `active` | `paused` | `blocked`（`complete` 不下发——没什么可提示的）。
    pub phase: String,
    /// 会不会自己往前跑。`active` 且没 armed 与 `paused` 对用户是同一件事：
    /// 「它停在这儿等人」，所以显示时合并。
    pub armed: bool,
    /// 上一轮一个工具都没调,驱动器停下来等人开口了。armed 仍是 true,
    /// 但它不会自己往前跑——对用户来说和暂停是同一件事。
    #[serde(default)]
    pub awaiting: bool,
    /// 已经起过几轮。
    pub rounds: i64,
    /// 这个状态是什么时候进入的（Unix 秒）。客户端拿它算「跑了多久」。
    pub since_unix: i64,
}

impl GoalHint {
    /// 它此刻会不会自己往前跑。
    ///
    /// `active` 但没 armed（被打断过、daemon 重启过），以及「上一轮空转、驱动器
    /// 在等人开口」，对人都和 `paused` 是同一件事：它停在这儿等你。
    pub fn running(&self) -> bool {
        self.phase == "active" && self.armed && !self.awaiting
    }
}

/// 记忆重置的范围。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryResetScope {
    /// 只清本会话产生的记忆。
    Session,
    /// 清这个人格的全部长期记忆。
    #[default]
    All,
}

/// Reference to a chat session in IPC commands.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionRef {
    /// The daemon's current session.
    Current,
    /// A session by exact id.
    Id { id: String },
    /// A user session of the active persona by (case-insensitive) name.
    Name { name: String },
}

/// 「仅本回合生效」的覆盖集。每一项都是 `None`/空 = 不覆盖。
///
/// 设计约束:这些值**不写 config、不写会话覆盖表**,回合结束即消失。取值
/// 无界的项(窗口、提示词)走 Agent 字段而不是改 config,免得把
/// `TurnResourceCache`(键=整份 config)冲刷掉。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TurnOverrides {
    /// 本回合模型池;空 = 沿用会话/全局池。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<miyu_base::config::ActiveProviderModelConfig>,
    /// 本回合上下文窗口(token)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<usize>,
    /// 整体替换人格/模式提示词(掉缓存,宿主自担)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    /// 追加在系统提示词末尾的宿主指令(进 system 侧,不化石)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub append_system_prompt: Option<String>,
    /// `Some(false)` = 本回合不写长期记忆/日记/经历,也不给 remember_fact。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_writes: Option<bool>,
    /// 工具白名单;`Some(vec![])` = 一个工具都不给。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_allowlist: Option<Vec<String>>,
}

impl TurnOverrides {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// 其中改变工具面的那两样。回合装配、MCP 桥、中转线三处读的是同一份,
    /// 所以从这里一处转换(09-23:中转线原来一样都不认)。
    pub fn tool_restrictions(&self) -> miyu_base::host_ports::TurnToolRestrictions {
        miyu_base::host_ports::TurnToolRestrictions {
            allowlist: self.tool_allowlist.clone(),
            no_memory_writes: self.memory_writes == Some(false),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Request {
    pub version: u16,
    #[serde(flatten)]
    pub command: Command,
}

impl Request {
    pub fn new(command: Command) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            command,
        }
    }
}

pub use miyu_base::workspace::OriginTty;

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum Command {
    Ping,
    Shutdown,
    ReloadConfig,
    GetStatus,
    /// Lightweight poll for the REPL background-command status strip.
    JobsOverview,
    /// 这个窗口此刻在看哪条会话（09-23，跨会话消息的「开着的会话」名单）。
    /// 终端顺着任务轮询一秒报一次，过期即算关了；`session` 缺省 = 这个窗口
    /// 关了。`viewer` 是客户端自己起的窗口编号。
    Presence {
        viewer: String,
        #[serde(default)]
        session: Option<String>,
    },
    /// Attach to a running turn and stream its event frames until it finishes.
    ///
    /// `from_start` = 把这一轮**从头**补一遍再接实时。同一个会话开第二个 TUI
    /// 时要的就是它：前面已经流过去的那半截得补上，不然挂上来只看得到后半截
    /// （用户 09-19：「我期望两个会话的内容是相同的」）。后台唤醒轮那条路不
    /// 传它，语义不变。
    FollowRun {
        run_id: String,
        #[serde(default)]
        from_start: bool,
        /// 从**这个事件号之后**接着看。回合中执行斜杠命令走的是「分离 → 执行
        /// → 挂回来」，挂回来时已经看过的那半截不能再来一遍（09-20）。
        /// 给了它就不看 `from_start`。
        #[serde(default)]
        after: Option<u64>,
    },
    /// Stop all running background commands of a session (REPL exit).
    StopSessionJobs {
        session_id: String,
    },
    /// 停掉**一个**后台任务。全屏 TUI 的详情面板里按 x 用它——
    /// 面板讲的就是这一个任务，停整会话的任务是另一回事。
    StopJob {
        job_id: String,
    },
    /// 后台子代理的**原始进度标记**，从绝对序号 `after` 之后取。
    ///
    /// 全屏 TUI 的后台子代理面板据它逐条跟，不再每 150ms 重读整份日志。日志那条
    /// 路留着当退路：daemon 重启后内存里的 trace 就没了，老任务也只有日志。
    ///
    /// 回 `{"markers": [...], "cursor": N, "reset": bool}`。`reset` 为真表示要的
    /// 位置已被环形缓冲挤掉，这一份要**重新攒**而不是接在旧的后面。
    JobTrace {
        job_id: String,
        after: u64,
    },
    GetSessionState {
        target: SessionRef,
        /// 发起查询的客户端当前目录(REPL 带,09-23):默认沙盒的根跟着它走(自动
        /// 检测),daemon 记下来,查看与工具桥用的就是下一轮会用的那个根。
        #[serde(default)]
        cwd: Option<std::path::PathBuf>,
    },
    /// Re-initializes one conversation: history, queue, per-session usage and
    /// the recall caches that belong to it. Only ever sent by the first-party
    /// frontends (CLI, REPL, WebUI) — platform sessions are rejected upstream
    /// and clear themselves through `ClearSessionContent`.
    ResetConversation {
        target: SessionRef,
    },
    /// 只清长期记忆(事实/日记/经历/待处理事件与外溢上下文存档),会话
    /// 历史与技能不动。`mode: "dev"` 清开发模式的独立记忆命名空间,
    /// 缺省清当前人格。不可逆,前端须先确认。
    ///
    /// `scope` 缺省 `All`:老客户端发不出这个字段,而它当年的语义就是全清,
    /// 默认值必须与那个语义一致,否则升级 daemon 会静默改掉旧命令的行为。
    ResetMemory {
        #[serde(default)]
        mode: Option<String>,
        #[serde(default)]
        scope: MemoryResetScope,
        /// 会话级重置清哪个会话;缺省用 daemon 当前指针指向的那个。
        #[serde(default)]
        session: Option<SessionRef>,
    },
    /// 出网请求录制开关(进程级,重启即关)。开着时每个 LLM 请求的完整
    /// 序列化体追加到 logs/requests-<日期>.jsonl,供审计注入内容。
    SetRequestLogging {
        enabled: bool,
    },
    /// Erases everything the persona accumulated: memory, every session's
    /// contents, group-chat contexts and auto-generated skills. Configuration
    /// is untouched. Irreversible; every frontend confirms before sending it.
    WipePersona,
    Undo {
        target: SessionRef,
    },
    Pop {
        target: SessionRef,
        turn_ids: Vec<String>,
    },
    Compact {
        target: SessionRef,
    },
    /// `/goal ...`：目标的读写都在 daemon 里做。
    ///
    /// 不在客户端直连库，是因为「是否自动续跑」（armed）驻在 daemon 内存——
    /// REPL 进程自己设那个标记，续轮驱动器根本看不见。
    Goal {
        target: SessionRef,
        input: String,
    },
    /// 这条会话的目标此刻是什么样（给输入框右上角那行提示用）。
    ///
    /// 单开一条命令而不是搭 `SessionState` 的车：`AdminResult` 里那份状态说的
    /// 是 **daemon 的当前会话**，而 REPL 有自己的会话指针（`GetReplSession`
    /// 不动当前会话），两者常常不是一条；而 `GetSessionState` 对非当前会话
    /// 要现装一个 Agent 估上下文，一秒一次的轮询用不起。这条只读一行库 +
    /// 两个内存标记。
    GoalStatus {
        target: SessionRef,
    },
    /// 某条车道开一条新会话时的上下文（系统提示词 + 工具表），**不建会话**。
    ///
    /// REPL 大厅里按 Tab 只换显示（09-23），换过去那条车道还没有会话；footer 上那个
    /// 数靠它事先算好（用户 09-24：「这个初始上下文数据不是可以事先计算好的吗？」）。
    EmptySessionContext {
        /// `dev` 算开发车道，缺省算普通车道。
        #[serde(default)]
        mode: Option<String>,
    },
    StartTurn {
        content: String,
        mode: String,
        #[serde(default)]
        images: Vec<Option<ImageAttachment>>,
        /// 触发本回合的终端身份(shellhook/单次 CLI)。后台任务完成后的
        /// 跟进回复据此写回原终端;缺省(REPL/WebUI/平台)不回写。
        #[serde(default)]
        origin_tty: Option<OriginTty>,
        /// Client working directory; used as the turn workspace when the
        /// target session has none bound.
        #[serde(default)]
        cwd: Option<std::path::PathBuf>,
        /// Target session id. Defaults to the global current session; when
        /// set, the turn runs there without moving the current pointer.
        #[serde(default)]
        session_id: Option<String>,
        /// 仅本回合生效的覆盖(模型/窗口/提示词/记忆/工具面),不落盘。
        /// 程序驱动的 CLI(`miyu ask --model …`、`miyu stdio`)用;REPL/WebUI
        /// 不传。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        overrides: Option<TurnOverrides>,
    },
    QueueTurnUpdate {
        run_id: String,
        turn_id: String,
        content: String,
        display_content: String,
        #[serde(default)]
        images: Vec<Option<ImageAttachment>>,
        #[serde(default)]
        supersede: bool,
    },
    Cancel {
        run_id: String,
    },
    /// 扩展(脚本)凭一次性令牌查宿主信息(09-16):方法与能力见
    /// `host_ports::host_query`,应答是 `AdminResult.data` 里的
    /// `{ok, data}` / `{ok:false, error:{code, message}}`。
    HostQuery {
        token: String,
        method: String,
        #[serde(default)]
        params: serde_json::Value,
    },
    AnswerQuestion {
        question_id: String,
        answers: QuestionAnswers,
    },
    /// Resolve a question without an answer, when the client cannot present it
    /// at all. Distinct from `Cancel`: the turn keeps going and the tool that
    /// asked simply learns nobody answered.
    CloseQuestion {
        question_id: String,
    },
    ListSessions {
        /// `dev` 列开发模式会话(保留人格 "dev" 名下),`all` 普通+dev
        /// 合并(管理面);缺省=当前人格。
        #[serde(default)]
        mode: Option<String>,
    },
    /// 某会话名下的子代理会话(09-18 会话化):直系子级,带任务状态、深度、上下文、
    /// 正在跑的 run id。`/subagent` 面板与任务条用它;子会话进不了 `ListSessions`。
    ListSubagentSessions {
        session_id: String,
    },
    CreateSession {
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        switch: bool,
        /// `user` (default) or `ask`; anything else is rejected. `ask` sessions
        /// back one-shot turns and stay out of every listing.
        #[serde(default)]
        kind: Option<String>,
        /// `"dev"` 建到保留人格 dev 名下(Build 模式会话);缺省=当前人格。
        #[serde(default)]
        mode: Option<String>,
    },
    /// 会话列表手动排序:按给定顺序重写展示序(WebUI 侧栏拖拽)。
    ReorderSessions {
        session_ids: Vec<String>,
    },
    /// Session the REPL was last on. Falls back to the current session and
    /// heals the stored pointer when it has gone stale.
    GetReplSession {
        /// `"dev"` 取 dev 人格的 REPL 指针(无则自举一个 dev 会话)。
        #[serde(default)]
        mode: Option<String>,
        /// **启动**一个 REPL:开新会话,除非指针那条本来就是空的(用户 09-20
        /// 拍板,见 `fresh_repl_session`)。会话里的 `/dev` `/normal` 不带它,
        /// 它们仍然切到那条车道最近用的会话。
        ///
        /// `serde(default)` 是为了老客户端:不带这个字段就是原来的语义。
        #[serde(default)]
        fresh: bool,
    },
    SetReplSession {
        target: SessionRef,
    },
    /// 工具桥(任务#12):`miyu tool-call` 打回 daemon,以指定会话的身份与
    /// 回合来源执行结构化工具——内层调用照走 guard/超时管线。bash 就是
    /// 编排层:中间数据在脚本里流动,不经模型上下文往返。
    ToolCall {
        #[serde(default)]
        session: Option<String>,
        name: String,
        #[serde(default)]
        arguments: String,
        /// 序列化的 TurnOrigin(来自 run_command 注入的 MIYU_TURN_ORIGIN)。
        #[serde(default)]
        origin: Option<String>,
        /// 递归深度(护栏,daemon 侧校验)。
        #[serde(default)]
        depth: u32,
    },
    /// 工具桥目录:`--list/--describe` 与 ToolCall 同一条解析链(会话→
    /// 模式→registry),杜绝"--list 列全量、调用却 unknown tool"的错位
    /// (dev 会话实测踩坑)。name=None 列全表,Some(name) 查单个合同。
    ToolCatalog {
        #[serde(default)]
        session: Option<String>,
        #[serde(default)]
        name: Option<String>,
        /// true 时列表项带完整合同(description+parameters):MCP 桥一次
        /// tools/list 拿全量,免去逐工具 describe 的 N 次往返。
        #[serde(default)]
        full: bool,
    },
    RenameSession {
        target: SessionRef,
        name: String,
    },
    DeleteSession {
        target: SessionRef,
    },
    /// `/sandbox <root>` / `/sandbox clear`:绑定或解绑会话沙盒根。daemon 侧校验
    /// 目录、探测内核 Landlock、拒绝成员会话;只影响之后的回合。
    SetSandbox {
        target: SessionRef,
        #[serde(default)]
        root: Option<std::path::PathBuf>,
        /// `--allow-read`:读放开到整个文件系统,写照旧只在根与放行清单里。
        /// 解绑(`root: None`)时忽略。默认 false = 读也锁,老客户端发来的帧照旧。
        #[serde(default)]
        allow_read: bool,
    },
    /// Tab / Shift+Tab:切只读模式(09-23)。读全盘、哪儿都不许写;下一次工具调用
    /// 就生效(回合中也是)。成员会话拒绝(他们固定关在自己家里)。
    SetSandboxReadonly {
        target: SessionRef,
        readonly: bool,
    },
    /// Pins the target session to its own model pool. An empty list clears
    /// the override so the session follows the global active pool again.
    SetSessionModels {
        target: SessionRef,
        #[serde(default)]
        models: Vec<miyu_base::config::ActiveProviderModelConfig>,
    },
    /// `miyu-voice` 进程注册的持久信令连接。应答 Ack 后双向裸交换 Event
    /// 帧(见 `voice::worker` 模块文档的信令表)。
    VoiceAttach,
    /// 客户端(REPL `/stt`、`miyu stt`)认领一条听写流:daemon 让语音前端
    /// 开听写窗,识别文本以 Event 帧 `voice.dictation {text}` 流回,窗口
    /// 结束发 `voice.dictation_ended`;连接断开即释放。
    StartDictation,
    /// 语音前端状态(二进制是否存在、是否在跑、设备名等)。应答
    /// Event `voice.status`。
    VoiceStatus,
    /// 让语音前端不用唤醒词直接进入等待指令状态(`miyu listen`,桌面
    /// 快捷键呼叫)。应答 Ack;语音未启用/前端未就绪/听写中为 Error。
    VoiceListen,
    /// 合成并播出一段文本(`miyu voice say`、设置页试听)。`tts` 为 Some 时用
    /// 这份配置(TUI 里试听尚未保存的音色/语速),否则用 daemon 当前配置。
    /// 应答 Ack(已交给前端播)或 Error。
    VoiceSpeak {
        text: String,
        #[serde(default)]
        tts: Option<miyu_base::config::VoiceTtsConfig>,
    },
    /// 删除唤醒对话的专属会话,下次唤醒重建(`miyu voice reset`)。应答 Ack。
    VoiceReset,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ImageAttachment {
    Binary { mime: String, data: Vec<u8> },
    Path { path: String },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Frame {
    Ready {
        pid: u32,
        #[serde(default)]
        web_port: u16,
        #[serde(default)]
        web_public: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        web_bind: Option<std::net::IpAddr>,
        #[serde(default)]
        build_id: String,
    },
    Accepted {
        run_id: String,
        /// Present when attaching to an already-running turn (FollowRun).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn_id: Option<String>,
    },
    TurnUpdateAccepted {
        run_id: String,
        turn_id: String,
        prompt_id: String,
        seq: i64,
        submitted_at: String,
    },
    Event {
        id: u64,
        kind: String,
        data: Value,
        /// 事件发生的时刻（Unix 毫秒）。挂上来的终端补发一轮时靠它按事件自己的时刻掐表
        /// （会话项目第 3 段收尾）；不带（老 daemon、不是回合事件）就按收到的那一刻。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        at_ms: Option<u64>,
    },
    Ack,
    AdminResult {
        state: SessionState,
        data: Value,
    },
    Error {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<ErrorCode>,
        message: String,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    Busy,
    #[serde(other)]
    Unknown,
}

impl Frame {
    pub fn error(message: impl Into<String>) -> Self {
        Self::Error {
            code: None,
            message: message.into(),
        }
    }

    pub fn coded_error(code: ErrorCode, message: impl Into<String>) -> Self {
        Self::Error {
            code: Some(code),
            message: message.into(),
        }
    }
}

pub(crate) fn expected_protocol_version(message: &str) -> Option<u16> {
    message
        .strip_prefix("unsupported IPC protocol version ")?
        .rsplit_once("; expected ")?
        .1
        .parse()
        .ok()
}

pub async fn send<T: Serialize>(stream: &mut UnixStream, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() > MAX_FRAME_BYTES {
        bail!("IPC frame exceeds the 24 MiB limit");
    }
    stream.write_u32(bytes.len() as u32).await?;
    stream.write_all(&bytes).await?;
    stream.flush().await?;
    Ok(())
}

pub async fn receive<T: DeserializeOwned>(stream: &mut UnixStream) -> Result<Option<T>> {
    let length = match stream.read_u32().await {
        Ok(length) => length as usize,
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if length == 0 || length > MAX_FRAME_BYTES {
        bail!("invalid IPC frame length: {length}");
    }
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes).await?;
    Ok(Some(serde_json::from_slice(&bytes)?))
}
