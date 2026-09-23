mod builtin_plugins;
mod context;
mod contracts;
mod defaults;
mod display;
pub mod feature_catalog;
mod io;
mod memory;
mod paths;
mod persona_lane;
mod persona_manifest;
mod persona_paths;
mod platform;
mod platform_ops;
mod platform_plugins;
mod pool_ref;
mod provider;
pub use provider::append_resolved_api_keys;
mod provider_ops;
pub(crate) mod subsystems;
pub use provider_ops::detect_provider_renames;
mod tool_plugins;
mod voice;
pub use crate::persona_lane::PersonaLane;
pub use builtin_plugins::{PluginKind, BUILTIN_PLUGINS, PLUGIN_IDS};
pub use context::*;
pub use contracts::SUPPORTED_CONTRACTS;
pub(crate) use defaults::*;
pub use display::*;
pub use memory::*;
pub use paths::*;
pub use persona_manifest::PersonaManifest;
pub use platform::*;
pub use platform_plugins::*;
pub use pool_ref::*;
pub use provider::*;
pub use subsystems::EnabledSubsystems;
pub use tool_plugins::*;
pub use voice::*;

use crate::default_models::{
    OPENCODE_DEFAULT_CHAT_MODEL, OPENCODE_DEFAULT_VISION_MODEL, OPENCODE_PROVIDER_ID,
    OPENCODE_ZEN_BASE_URL, OPENCODE_ZEN_GO_BASE_URL,
};
use crate::paths::MiyuPaths;
use crate::prompts::default_system_prompt;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

pub const MAX_COMMAND_OUTPUT_LINES: usize = 1_000;
/// 思考滚动窗最多露多少行——再高就顶到屏幕外了，没意义。
pub const MAX_THINKING_SCROLL_LINES: usize = 200;

/// Dev 模式提示词文件名(config 目录下,可编辑;清空=回退内置默认)。
pub const DEV_PROMPT_FILE: &str = "dev-prompt.md";
/// Dev 模式内置默认提示词。dsh 极简变体同款措辞——贴近编码 RL 训练分布
/// 是它强的主因(08-15 与用户讨论定稿,修正了社区传言的拼写错误)。
pub const DEFAULT_DEV_SYSTEM_PROMPT: &str = "You are a helpful software engineer assistant.";
/// Replay redraws whole turns, so a large value floods the screen on startup.
pub const MAX_REPL_REPLAY_TURNS: usize = 20;
pub const CURRENT_CONFIG_VERSION: u32 = 5;

/// dev 会话的保留人格 scope:dev 会话全部挂在它名下,借现有按人格隔离机制白拿
/// 会话 / 记忆 / REPL 指针的分家;是不是 dev 由会话的 persona==DEV_PERSONA 推导。
/// 09-16 从 state 搬来:config 是底座,不能反向认识 state。
pub const DEV_PERSONA: &str = "dev";
const LEGACY_DEFAULT_TEMPERATURE: f32 = 0.7;
/// 上下文窗口那个数是哪来的。
///
/// `Known` = 用户在配置里写死的，或 models.dev / 供应商 `/models` 报的。
/// `Assumed` = 谁都没给，用的是 `context.default_context_window` 那个通用常数
/// ——它跟具体模型没有任何关系，只是让溢出判定有个数可用。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContextWindowSource {
    Known,
    Assumed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub config_version: u32,
    /// 这个版本不认识的顶层字段，原样留着、原样写回。
    ///
    /// 几个分支的二进制会轮流读写同一份配置（比如另一个工作树先把版本号抬到 3、
    /// 加了 `oobe_done`）：这边不认识的字段要是读进来就丢、写回去就没了，那边
    /// 再启动时就当没设置过。字段跟着走，谁也不弄丢谁的东西。
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, serde_json::Value>,
    pub active_provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_provider_models: Option<Vec<ActiveProviderModelConfig>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_multimodal_provider_models: Option<Vec<ActiveProviderModelConfig>>,
    pub providers: Vec<ProviderConfig>,
    #[serde(default, skip_serializing_if = "EmbeddingConfig::is_default")]
    pub embedding: EmbeddingConfig,
    #[serde(default)]
    pub context: ContextConfig,
    #[serde(default)]
    pub tools: ToolsConfig,
    #[serde(default, skip_serializing_if = "CacheConfig::is_default")]
    pub cache: CacheConfig,
    #[serde(default)]
    pub mcp: McpConfig,
    #[serde(default)]
    pub skills: SkillsConfig,
    #[serde(default)]
    pub display: DisplayConfig,
    #[serde(default)]
    pub notifications: NotificationsConfig,
    #[serde(default)]
    pub prompt: PromptConfig,
    #[serde(default)]
    pub plugins: PluginsConfig,
    #[serde(default, skip_serializing)]
    pub memory: MemoryConfig,
    #[serde(default)]
    pub system_prompt_file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    /// 新手引导（OOBE）做完了或跳过了。新配置默认 false，裸 `miyu` 会先走引导；
    /// 旧版本升上来的配置在 `migrate` 里直接标成 true，老用户不会被拦。
    /// `miyu init` 不碰它：脚本化初始化不等于人已经设置过。
    #[serde(default)]
    pub oobe_done: bool,
    /// 终端集成车道（shellhook / 裸 `miyu "…"` 落进去的那条会话）跑在哪个模式：
    /// `"normal"`（默认）| `"dev"`。模式钉在会话的人格上（dev 人格 = 开发模式），
    /// 所以 daemon 按它给「终端集成会话」换人格并把车道指针指过去
    /// （`web::sessions::apply_terminal_session_mode`），启动和重载配置时各对一次。
    #[serde(default = "default_terminal_session_mode")]
    pub terminal_session_mode: String,
    /// Tiered model pools. The pre-09-05 key `subagent_tiers` stays readable.
    #[serde(
        default,
        alias = "subagent_tiers",
        skip_serializing_if = "ModelTiersConfig::is_empty"
    )]
    pub model_tiers: ModelTiersConfig,
    #[serde(default, skip_serializing_if = "PlatformsConfig::is_empty")]
    pub platforms: PlatformsConfig,
    /// 多用户(阶段 5/8):成员能用什么。
    #[serde(default)]
    pub accounts: AccountsConfig,
    /// 语音前端(`miyu-voice` 进程):唤醒词、本地识别、听写、提示音。
    #[serde(default)]
    pub voice: VoiceConfig,
}

/// Provider prompt-cache tuning (v7, DeepSeek 高命中策略实测产物). The
/// tuning knobs default to off — they trade a little latency or a few cheap
/// requests for prefix-cache hits on best-effort provider caches. The
/// accounting log defaults to on (numbers only, ~0.2 KB per request).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CacheConfig {
    /// Idle keepalive: while the agent waits for the next user turn, re-send
    /// the exact prompt prefix of the last request every N seconds as a
    /// non-streaming max_tokens=1 completion so hot-tier prefix caches
    /// (DeepSeek-style) keep the deep prefix alive across turn gaps. The ping
    /// is billed at the provider's cache-hit input price. 0 disables (the
    /// default — enable only after measuring your provider: on per-REQUEST
    /// billed endpoints every ping burns quota for nothing).
    /// Only effective in long-lived processes (daemon/REPL); one-shot `ask`
    /// exits before any ping fires.
    pub keepalive_seconds: u64,
    /// Stop pinging after this many keepalives per turn (bounds idle cost).
    pub keepalive_max_pings: u32,
    /// Provider cache writes are asynchronous (measured: a follow-up within
    /// ~2s can miss the prefix the previous request just computed). When >0,
    /// consecutive tool-loop requests wait until at least this many
    /// milliseconds have passed since the previous round completed.
    pub write_grace_ms: u64,
    /// Per-request cache accounting log: one JSONL line of absolute token
    /// numbers (prompt/cache_read/completion/…) per LLM request under
    /// cache/logs/cache-usage.<date>.jsonl. Numbers only — never prompt text.
    /// Roughly 0.2 KB per request; daily files, pruned by retention below.
    pub request_log: bool,
    /// Days of cache-usage JSONL files to keep (older files are deleted when
    /// a new line is written).
    pub request_log_retention_days: u64,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            keepalive_seconds: 0,
            keepalive_max_pings: 20,
            write_grace_ms: 0,
            request_log: true,
            request_log_retention_days: 14,
        }
    }
}

impl CacheConfig {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// Desktop notifications. Both kinds are suppressed while the REPL window has
/// focus — if you are looking at the terminal, a popup is only noise.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationsConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Notify when a reply finishes and Miyu is waiting on you again.
    #[serde(default = "default_true")]
    pub on_turn_complete: bool,
    /// 通知带一声提示音（默认是 Miyu 内置的木琴音，见 [`Self::tone`]）。
    #[serde(default = "default_true")]
    pub sound: bool,
    /// 换成自己的音频文件（`~/` 会展开）。留空 = 用 Miyu 内置的那一声。
    /// 指到不存在的文件也退回内置音，不会变成哑的。播放器认 wav/ogg/flac；
    /// mp3 得机器上有 ffplay。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sound_file: String,
    /// 她提问、等你回答时单独用的那个文件。留空 = 跟 `sound_file` 一样。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub question_sound_file: String,
    /// shellhook/单次 CLI 触发的后台任务完成后,把跟进回复写回触发它的那个
    /// 终端。仅在该 shell 仍活着、停在同一 tty 的前台提示符时才写;写不了退化
    /// 为桌面通知。
    #[serde(default = "default_true")]
    pub job_writeback_to_terminal: bool,
}

impl NotificationsConfig {
    /// 这一声响什么。顺序是：关了就静音 → 配了文件就放那个文件 → 内置的那两段
    /// 木琴音 → 系统声音主题（前面全落空才轮到它，比如连家目录都取不到）。
    ///
    /// 会碰盘：内置音是内嵌字节，外挂播放器只认文件，第一次用时落到
    /// `<家>/cache/sounds/` 下。
    pub fn tone(&self, sound: crate::notify::NotifySound) -> crate::notify::NotifyTone {
        use crate::notify::{NotifySound, NotifyTone};
        if !self.sound {
            return NotifyTone::Silent;
        }
        let configured = match sound {
            // 提问那一声没单配就跟着完成那一声走——大多数人只想换一个音。
            NotifySound::Question if !self.question_sound_file.is_empty() => {
                &self.question_sound_file
            }
            _ => &self.sound_file,
        };
        if !configured.is_empty() {
            let path = expand_home(configured);
            if path.is_file() {
                return NotifyTone::File(path);
            }
            // 配了个不存在的路径不该变成哑的，往下退到内置音。
        }
        crate::notify::builtin_sound_file(sound)
            .map(NotifyTone::File)
            .unwrap_or(NotifyTone::Theme(sound))
    }
}

/// `~/` 开头展开成家目录。配置里的路径都是人手写的，写 `~` 比写全路径顺手。
fn expand_home(value: &str) -> std::path::PathBuf {
    let value = value.trim();
    if let Some(rest) = value.strip_prefix("~/") {
        if let Some(dirs) = directories::BaseDirs::new() {
            return dirs.home_dir().join(rest);
        }
    }
    std::path::PathBuf::from(value)
}

impl Default for NotificationsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            on_turn_complete: true,
            sound: true,
            sound_file: String::new(),
            question_sound_file: String::new(),
            job_writeback_to_terminal: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptConfig {
    #[serde(default = "default_prompts_dir")]
    pub prompts_dir: String,
    #[serde(default = "default_identities_dir")]
    pub identities_dir: String,
    #[serde(default = "default_user_identity_file")]
    pub user_identity_file: String,
    #[serde(default)]
    pub active_persona: String,
    #[serde(default)]
    pub active_identity: String,
    /// 成员的私有人格目录(`home/<用户>/personas/<slug>`),**只在回合里由
    /// daemon 填**,不写进配置文件。填了之后:提示词读 `persona.md`,记忆/清单/
    /// 技能/脚本都在这个目录下,`active_persona_scope()` 变成 `home-<用户>-<slug>`。
    /// 参与序列化是为了进 TurnResourceCache 的键(工具面随人格清单变)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub private_persona_dir: Option<String>,
    /// 防失忆提醒(自动蒸馏,见 persona_hint 模块)。08-16 起改为
    /// 化石注入:每隔 `persona_reminder_interval` 轮进一次历史,纯追加
    /// 不再掰前缀缓存。A/B 实证干净体制下预设对话已足够→默认禁用。
    #[serde(default)]
    pub persona_reminder: bool,
    /// 相邻两次防失忆提醒之间至少间隔的轮数(>=1)。
    #[serde(default = "default_persona_reminder_interval")]
    pub persona_reminder_interval: u32,
}

/// Identifies who a model prompt is acting for. Only trusted local operator
/// turns may receive the configured user identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptAudience {
    Owner,
    External,
    Internal,
}

impl PromptAudience {
    fn includes_user_identity(self) -> bool {
        matches!(self, Self::Owner)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolsConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub max_rounds: usize,
    #[serde(default = "default_tools_loading_mode")]
    pub loading_mode: String,
    #[serde(default = "default_true")]
    pub persist_loaded_tools: bool,
    /// How many `subagent` runs from one tool batch may run concurrently.
    #[serde(default = "default_subagent_concurrency")]
    pub subagent_concurrency: usize,
    /// 工具执行兜底超时（秒），0=关闭。防没有自管超时的工具（MCP/web/生图
    /// 等）把回合无限挂死；run_command/subagent 等自管或长跑工具
    /// 在 descriptions JSON 里以 timeout_seconds=0 豁免。
    #[serde(default = "default_tools_timeout_secs")]
    pub default_timeout_secs: u64,
    /// run_command 命令拒绝子串。命中即拒（guard 层，回给模型 tool error）。
    /// 防提示注入与模型手滑；默认只收录几乎不可能误伤的毁灭性模式。
    #[serde(default = "default_command_deny")]
    pub command_deny: Vec<String>,
    /// 高危命令拦截：按命令位（argv[0]）拦下 `rm` 这类不可恢复的删除命令。
    /// 与上面的子串名单是两层：这一层认命令结构，`git rm`、`rmdir` 不误伤。
    /// 关掉之后 run_command 只剩子串名单把关。
    #[serde(default = "default_true")]
    pub block_dangerous_commands: bool,
    /// `/sandbox` 会话沙盒的放行清单(管理员绑定时生效;成员沙盒不看)。
    #[serde(default)]
    pub sandbox: SandboxConfig,
}

/// `/sandbox <路径>` 之外还放行什么。根、`/tmp`、系统目录、Miyu 自己的产出目录
/// 是固定的;这里只是工具链。清单进环境块,改了就是一次计划内的缓存冷启动。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SandboxConfig {
    /// 根之外额外**只读**的目录/文件(`~` 展开)。默认:`~/.rustup ~/.local ~/.gitconfig`。
    pub readable: Vec<String>,
    /// 根之外额外**可写**的目录(`~` 展开)。默认构建缓存:`~/.cargo ~/.npm`。
    pub writable: Vec<String>,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            readable: default_sandbox_readable(),
            writable: default_sandbox_writable(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub servers: Vec<McpServerConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub id: String,
    #[serde(default)]
    pub display_name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default = "default_mcp_timeout")]
    pub timeout_seconds: u64,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 服务器要向宿主查的信息(与脚本头部 `Capabilities:` 同一张词表,见
    /// docs/interfaces/host-capabilities.md);声明了就在拉起时注入一次性令牌,
    /// 令牌随服务器进程生灭。不认识的 id 记 warn 并忽略。
    #[serde(default)]
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SkillsConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub allow_command_execution: bool,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            config_version: CURRENT_CONFIG_VERSION,
            extra: BTreeMap::new(),
            active_provider: OPENCODE_PROVIDER_ID.to_string(),
            active_provider_models: None,
            active_multimodal_provider_models: None,
            providers: ProviderConfig::default_templates(),
            embedding: EmbeddingConfig::default(),
            context: ContextConfig::default(),
            tools: ToolsConfig::default(),
            cache: CacheConfig::default(),
            mcp: McpConfig::default(),
            skills: SkillsConfig::default(),
            display: DisplayConfig::default(),
            notifications: NotificationsConfig::default(),
            prompt: PromptConfig::default(),
            plugins: PluginsConfig::default(),
            memory: MemoryConfig::default(),
            system_prompt_file: Some("system-prompt.md".to_string()),
            system_prompt: None,
            oobe_done: false,
            terminal_session_mode: default_terminal_session_mode(),
            model_tiers: ModelTiersConfig::default(),
            platforms: PlatformsConfig::default(),
            voice: VoiceConfig::default(),
            accounts: AccountsConfig::default(),
        }
    }
}

/// 管理员给成员划的边界:成员自建人格能勾哪些插件、能不能自建人格。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AccountsConfig {
    /// 成员人格可启用的插件 id 白名单;None = 全部(见 `PLUGIN_IDS`)。
    pub member_plugins: Option<Vec<String>>,
    /// 成员能否创建自己的人格(关了就只能用共享的 Miyu)。
    pub member_personas: bool,
    /// 成员的家目录 `home/<用户>`,**只在回合/面板里由 daemon 填**,不写进配置
    /// 文件:知识库、账本这些「人的资料」按它分家。参与序列化是为了进
    /// TurnResourceCache 的键(工具捕获的路径随它变)。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub home_dir: Option<String>,
}

impl Default for AccountsConfig {
    fn default() -> Self {
        Self {
            member_plugins: None,
            member_personas: true,
            home_dir: None,
        }
    }
}

impl AppConfig {
    /// 这份配置是替哪个成员跑的:家目录(None = 管理员/终端,资料在根布局的
    /// 管理员家目录)。私有人格目录在 `home/<用户>/personas/<slug>` 下,没显式
    /// 填家目录时从它推。
    pub fn member_home_dir(&self) -> Option<PathBuf> {
        if let Some(dir) = self
            .accounts
            .home_dir
            .as_deref()
            .map(str::trim)
            .filter(|dir| !dir.is_empty())
        {
            return Some(PathBuf::from(dir));
        }
        let persona = self.private_persona_dir()?;
        persona.parent()?.parent().map(Path::to_path_buf)
    }
}

impl AccountsConfig {
    /// 成员可勾的插件:白名单 ∩ 已知 id。
    pub fn allowed_member_plugins(&self) -> Vec<String> {
        crate::config::PLUGIN_IDS
            .iter()
            .filter(|id| {
                self.member_plugins
                    .as_ref()
                    .is_none_or(|list| list.iter().any(|item| item == *id))
            })
            .map(|id| id.to_string())
            .collect()
    }
}

impl Default for PromptConfig {
    fn default() -> Self {
        Self {
            prompts_dir: default_prompts_dir(),
            identities_dir: default_identities_dir(),
            user_identity_file: default_user_identity_file(),
            active_persona: String::new(),
            active_identity: String::new(),
            private_persona_dir: None,
            persona_reminder: false,
            persona_reminder_interval: default_persona_reminder_interval(),
        }
    }
}

impl Default for McpConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            servers: Vec::new(),
        }
    }
}

impl Default for ToolsConfig {
    fn default() -> Self {
        Self {
            enabled: default_true(),
            max_rounds: 0,
            loading_mode: default_tools_loading_mode(),
            persist_loaded_tools: default_true(),
            subagent_concurrency: default_subagent_concurrency(),
            default_timeout_secs: default_tools_timeout_secs(),
            command_deny: default_command_deny(),
            block_dangerous_commands: default_true(),
            sandbox: SandboxConfig::default(),
        }
    }
}

impl Default for SkillsConfig {
    fn default() -> Self {
        Self {
            enabled: default_true(),
            allow_command_execution: default_true(),
        }
    }
}

impl AppConfig {}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod scaling_probe;
