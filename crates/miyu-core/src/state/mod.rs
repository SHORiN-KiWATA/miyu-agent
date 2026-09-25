mod accounts;
pub use accounts::*;
mod assets;
mod history;
mod shared_files;
pub use conversation_db::interrupted_prefix;
pub use conversation_db::SharedFile;
mod conversation_db;
mod cross_session;
pub use cross_session::*;
mod service_restart;
pub use service_restart::*;
mod migrations;
mod queue;
mod session_state;
pub use session_state::{safe_path_segment, REPL_HISTORY_KEEP};
mod sessions;
mod turns;
mod usage_ops;
pub use migrations::DEFAULT_SESSION_ID;
pub mod usage;

/// Newest `conversation.db` schema this build can open — the gate an import
/// checks before restoring a database written by a newer Miyu.
pub fn latest_schema_version() -> i64 {
    migrations::LATEST_VERSION
}

use crate::llm::{TurnTokens, Usage};
use anyhow::{bail, Context, Result};
use conversation_db::OpenRole;
use miyu_base::memory_types::EvictedTurn;
use miyu_base::paths::MiyuPaths;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::{Cursor, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, RwLock, Weak};

#[allow(unused_imports)]
pub use conversation_db::{
    interrupted_text, pending_placeholder, ArtifactAsset, ArtifactAssetData, CacheBreakRecord,
    ContextAnchor, ConversationDb, FlowMessage, GoalDenied, GoalPhase, GoalRecord, ImageAsset,
    ImageAssetData, NewSponsorRecord, PlatformAccessActor, PlatformAccessGrant,
    PlatformAccessGrantKey, PlatformMemeRefRecord, PlatformPluginScopeKey, PlatformSessionBinding,
    PlatformSessionBindingKey, QueuedPrompt, QueuedPromptAttachment, QueuedSyntheticPrompt,
    RedoCandidate, RedoInputKind, RedoStart, ReplayEntry, ReplayPage, RestartOrphan,
    SessionOverview, SessionRecord, SessionValueKind, SponsorOrder, SponsorRecord, SponsorSummary,
    SponsorTotal, ToolFlowCall, ToolFlowRound, ToolFootprint, Turn, TurnCompletion,
    TurnFinishExtras, TurnFollowup, TurnInlineMedia, TurnJournalEvent, TurnPage,
    TurnRedoCheckpointPayload, TurnReplay, TurnStatus, UserAttachment, UserAttachmentData,
    DEFAULT_MAX_GOAL_ROUNDS, GLOBAL_PLATFORM_ACCOUNT_SCOPE, INLINE_MEDIA_KIND_IMAGE,
    INLINE_MEDIA_KIND_PDF, INLINE_MEDIA_KIND_TEXT, INLINE_MEDIA_KIND_VIDEO,
    USER_ATTACHMENT_KIND_FILE, USER_ATTACHMENT_KIND_IMAGE, USER_ATTACHMENT_KIND_TEXT,
};
pub use usage::{
    UsageMeta, UsageRange, UsageSnapshot, UsageStats, USAGE_KIND_AFFECTION, USAGE_KIND_GROUP_JOIN,
    USAGE_KIND_JUDGE,
};

/// The only session kind users can list, name, switch to, or bind a platform
/// to. Everything else is infrastructure and stays out of the session list.
pub const USER_SESSION_KIND: &str = "user";
/// dev 会话的保留人格 scope(定义住 config,这里保留老路径)。
pub use miyu_base::config::DEV_PERSONA;
/// Backs a one-shot `miyu ask` / `miyu '<message>'` turn: created just before
/// the turn, deleted right after, and invisible to every listing in between.
pub const ASK_SESSION_KIND: &str = "ask";
/// 唤醒对话的专属会话:不进 WebUI 列表(列表只取 user),用 `miyu voice`
/// 命令组管理(reset / history)。
pub const VOICE_SESSION_KIND: &str = "voice";

type PlatformAccessSubjects = HashSet<String>;
type PlatformAccessKinds = HashMap<String, PlatformAccessSubjects>;
type PlatformAccessPermissions = HashMap<String, PlatformAccessKinds>;
type PlatformAccessScopes = HashMap<String, PlatformAccessPermissions>;

#[derive(Debug)]
struct SharedPlatformAccess {
    index: RwLock<PlatformAccessIndex>,
    mutations: Mutex<()>,
}

static PLATFORM_ACCESS_INDEXES: OnceLock<Mutex<HashMap<PathBuf, Weak<SharedPlatformAccess>>>> =
    OnceLock::new();

#[derive(Debug, Default)]
struct PlatformAccessIndex {
    platforms: HashMap<String, PlatformAccessScopes>,
}

impl PlatformAccessIndex {
    fn from_grants(grants: impl IntoIterator<Item = PlatformAccessGrant>) -> Self {
        let mut index = Self::default();
        for grant in grants {
            index.insert(&grant.key);
        }
        index
    }

    fn contains(
        &self,
        platform: &str,
        account_scope: &str,
        permission: &str,
        subject_kind: &str,
        subject_id: &str,
    ) -> bool {
        self.platforms
            .get(platform)
            .and_then(|scopes| scopes.get(account_scope))
            .and_then(|permissions| permissions.get(permission))
            .and_then(|kinds| kinds.get(subject_kind))
            .is_some_and(|subjects| subjects.contains(subject_id))
    }

    fn insert(&mut self, key: &PlatformAccessGrantKey) {
        self.platforms
            .entry(key.platform.clone())
            .or_default()
            .entry(key.account_scope.clone())
            .or_default()
            .entry(key.permission.clone())
            .or_default()
            .entry(key.subject_kind.clone())
            .or_default()
            .insert(key.subject_id.clone());
    }

    fn remove(&mut self, key: &PlatformAccessGrantKey) -> bool {
        if let Some(subjects) = self
            .platforms
            .get_mut(&key.platform)
            .and_then(|scopes| scopes.get_mut(&key.account_scope))
            .and_then(|permissions| permissions.get_mut(&key.permission))
            .and_then(|kinds| kinds.get_mut(&key.subject_kind))
        {
            return subjects.remove(&key.subject_id);
        }
        false
    }
}

fn shared_platform_access_index(
    state_dir: &Path,
    conv_db: &ConversationDb,
) -> Result<Arc<SharedPlatformAccess>> {
    let key = state_dir
        .canonicalize()
        .unwrap_or_else(|_| state_dir.to_path_buf());
    let indexes = PLATFORM_ACCESS_INDEXES.get_or_init(|| Mutex::new(HashMap::new()));
    let mut indexes = indexes.lock().unwrap();
    if let Some(index) = indexes.get(&key).and_then(Weak::upgrade) {
        return Ok(index);
    }
    indexes.retain(|_, index| index.strong_count() > 0);
    let index = Arc::new(SharedPlatformAccess {
        index: RwLock::new(PlatformAccessIndex::from_grants(
            conv_db.platform_access_grants(None)?,
        )),
        mutations: Mutex::new(()),
    });
    indexes.insert(key, Arc::downgrade(&index));
    Ok(index)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunningTurnQueueTarget {
    pub turn_id: String,
    pub queue_session_id: Option<String>,
    pub owner_pid: Option<u32>,
}

#[derive(Clone, Debug)]
pub struct PlatformAccessAuthorization {
    pub statically_authorized: bool,
    pub dynamic_key: PlatformAccessGrantKey,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlatformAccessMutation {
    Grant,
    Revoke,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlatformAccessMutationResult {
    Unauthorized,
    Unchanged,
    Changed,
}

#[derive(Debug, Clone)]
pub struct StateStore {
    state_dir: PathBuf,
    artifacts_dir: PathBuf,
    shared_files_dir: PathBuf,
    conv_db: Arc<ConversationDb>,
    platform_access: Arc<SharedPlatformAccess>,
    /// Active session. Shared across clones and swappable at runtime so a
    /// long-lived daemon switches every holder atomically.
    session_id: Arc<std::sync::RwLock<Arc<str>>>,
    queue_session_id: Arc<str>,
    queue_owner_pid: u32,
    /// 用量账本的账号列(阶段 5):`pinned_for_turn` 按会话归属填;空串 =
    /// 管理员/遗留。整理器、判官等自己起的 store 都记在空串名下。
    usage_account: Arc<str>,
    /// 这个账号的老式会话文件放在哪:管理员是 state 目录,成员是他自己家里
    /// (思考档位钉按账号分家,见 `session_state`)。
    legacy_home: PathBuf,
}

impl StateStore {
    /// 当前 store 记账归到哪个账号(空串 = 管理员/遗留)。
    pub fn usage_account(&self) -> &str {
        &self.usage_account
    }

    /// 客户端开库(终端、一次性命令、守护进程里临时开库的工具):同一进程共用
    /// 一个连接,库是当前版本就不做维护。守护进程开主库用 `open_maintained`。
    pub fn new(paths: &MiyuPaths) -> Result<Self> {
        Self::new_at(
            paths,
            &paths.conversation_db_dir(),
            paths.artifacts_dir(),
            OpenRole::Client,
            None,
        )
    }

    /// 守护进程开主库:它是库的看门人,体检、回收空闲页只在这里做。
    pub fn open_maintained(paths: &MiyuPaths) -> Result<Self> {
        Self::new_at(
            paths,
            &paths.conversation_db_dir(),
            paths.artifacts_dir(),
            OpenRole::Maintain,
            None,
        )
    }

    /// 成员自己的会话库:`home/<用户>/conversation.db`,artifact 也落他家里;
    /// 附件本体、用量账本仍在 state(机器级)。只有守护进程的 StoreRegistry
    /// 开成员库,所以按看门人开。
    pub fn open_member(paths: &MiyuPaths, username: &str) -> Result<Self> {
        let home = paths.user_home_dir(username);
        miyu_base::paths::ensure_private_dir(&paths.homes_dir())?;
        miyu_base::paths::ensure_private_dir(&home)?;
        Self::new_at(
            paths,
            &home,
            home.join("artifacts"),
            OpenRole::Maintain,
            Some(&home),
        )
    }

    /// 已知成员家目录时直接开他的会话库(工具侧只有 `config.member_home_dir()`
    /// 拿到的路径、没有用户名时用)。库/artifact 落点与 `open_member` 同口径。
    pub fn open_at_home(paths: &MiyuPaths, home: &Path) -> Result<Self> {
        miyu_base::paths::ensure_private_dir(home)?;
        Self::new_at(
            paths,
            home,
            home.join("artifacts"),
            OpenRole::Client,
            Some(home),
        )
    }

    fn new_at(
        paths: &MiyuPaths,
        db_dir: &Path,
        artifacts_dir: PathBuf,
        role: OpenRole,
        member_home: Option<&Path>,
    ) -> Result<Self> {
        let state_dir = paths.state_dir.clone();
        let legacy_home = member_home.map_or_else(|| state_dir.clone(), Path::to_path_buf);
        let conv_db = ConversationDb::shared(db_dir, &state_dir, role)?;
        let platform_access = shared_platform_access_index(&state_dir, &conv_db)?;
        let session_id = Arc::new(std::sync::RwLock::new(Arc::<str>::from(
            conv_db.resolve_current_session()?,
        )));
        let queue_owner_pid = std::process::id();
        let queue_session_id: Arc<str> = format!(
            "queue_{}_{}_{}",
            queue_owner_pid,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0),
            rand::random::<u64>()
        )
        .into();
        conv_db.discard_stale_queued_prompts(&queue_session_id, queue_owner_pid)?;
        Ok(Self {
            state_dir,
            artifacts_dir,
            shared_files_dir: paths.shared_files_dir(),
            conv_db,
            platform_access,
            session_id,
            queue_session_id,
            queue_owner_pid,
            usage_account: Arc::from(""),
            legacy_home,
        })
    }

    pub fn session_id(&self) -> Arc<str> {
        self.session_id.read().unwrap().clone()
    }

    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    pub fn init_files(&self) -> Result<()> {
        std::fs::create_dir_all(&self.state_dir)?;
        if !self.profile_file().exists() {
            std::fs::write(self.profile_file(), "# Miyu Profile\n\n")?;
        }
        Ok(())
    }

    #[allow(dead_code)]
    pub fn conv_db(&self) -> &ConversationDb {
        &self.conv_db
    }

    #[allow(dead_code)]
    pub fn migrate_from_jsonl(&self) -> Result<usize> {
        let jsonl_path = self.conversation_file();
        self.conv_db
            .migrate_from_jsonl(&self.session(), &jsonl_path)
    }

    fn conversation_file(&self) -> PathBuf {
        self.state_dir.join("conversation.jsonl")
    }

    fn profile_file(&self) -> PathBuf {
        self.state_dir.join("profile.md")
    }
}

fn artifact_media_type(path: &Path) -> (&'static str, &'static str) {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "md" | "markdown" => ("text/markdown; charset=utf-8", "markdown"),
        "html" | "htm" => ("text/html; charset=utf-8", "html"),
        "pdf" => ("application/pdf", "pdf"),
        "json" | "jsonl" => ("application/json; charset=utf-8", "json"),
        // SVG 自成一类:预览走图片通道(`<img>` 里的 SVG 浏览器强制禁脚本、禁外链,
        // 天然安全),但它同时是文本,源码视图也要能看——归进 text/code 就拿不到
        // 图片那套缩放平移了。**投递永远是 attachment**,理由见 assets.rs。
        "svg" => ("image/svg+xml", "svg"),
        // 位图:归 kind="image" + 正确 mime,前端预览走 <img> 通道(否则落到
        // application/octet-stream + kind="file",被当文本 dump 出一屏乱码,且
        // 服务端带 nosniff 连 <img> 都渲染不了——09-11 PNG 预览实测)。
        "png" => ("image/png", "image"),
        "jpg" | "jpeg" => ("image/jpeg", "image"),
        "webp" => ("image/webp", "image"),
        "gif" => ("image/gif", "image"),
        "bmp" => ("image/bmp", "image"),
        "ico" => ("image/x-icon", "image"),
        "avif" => ("image/avif", "image"),
        // csv/tsv 从 text 里单拎出来,前端才认得出该画成表格。
        "csv" => ("text/csv; charset=utf-8", "csv"),
        "tsv" => ("text/tab-separated-values; charset=utf-8", "csv"),
        "txt" | "log" => ("text/plain; charset=utf-8", "text"),
        "css" => ("text/css; charset=utf-8", "code"),
        "js" | "mjs" | "cjs" => ("text/javascript; charset=utf-8", "code"),
        "xml" => ("application/xml; charset=utf-8", "code"),
        "rs" | "jsx" | "ts" | "tsx" | "py" | "go" | "java" | "c" | "cc" | "cpp" | "h" | "hpp"
        | "cs" | "rb" | "php" | "swift" | "kt" | "kts" | "sh" | "bash" | "zsh" | "fish"
        | "toml" | "yaml" | "yml" | "scss" | "sql" => ("text/plain; charset=utf-8", "code"),
        _ => ("application/octet-stream", "file"),
    }
}

fn prompt_fingerprint(system_prompt: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(system_prompt.as_bytes());
    format!("{:x}", hasher.finalize())
}

#[allow(dead_code)]
fn turn_chars(turn: &Turn) -> usize {
    turn.user_content.chars().count()
        + turn.assistant_content.chars().count()
        + turn
            .assistant_reasoning
            .as_deref()
            .map(str::chars)
            .map(Iterator::count)
            .unwrap_or(0)
        + turn
            .tool_reports
            .iter()
            .map(|r| r.chars().count())
            .sum::<usize>()
        + turn
            .question_exchanges
            .iter()
            .filter_map(|exchange| serde_json::to_string(exchange).ok())
            .map(|exchange| exchange.chars().count())
            .sum::<usize>()
        + turn
            .followups
            .iter()
            .map(|followup| {
                followup.content.chars().count()
                    + followup
                        .preceding_assistant_content
                        .as_deref()
                        .map(str::chars)
                        .map(Iterator::count)
                        .unwrap_or(0)
                    + followup
                        .preceding_assistant_reasoning
                        .as_deref()
                        .map(str::chars)
                        .map(Iterator::count)
                        .unwrap_or(0)
            })
            .sum::<usize>()
}

/// daemon 自己合成的轮在 `user_content` 开头带的标签：后台任务唤醒、目标续轮。
///
/// 它们在库里就是一条普通的 `role == "user"` 行（`turns` 表没有 origin 列），只能
/// 靠这个开头认。产出方（`job_wake` / `goal_round_prompt`）、回放的 SQL、上键历史
/// 三处都得对着同一份写——原来各抄各的，历史那条路压根没抄（用户 09-17：「后台
/// 任务的完成报告居然会出现在上方向键可以调出来的历史输入里」）。
pub const BACKGROUND_JOB_REPORT_TAG: &str = "<background-job-report>";

/// 子代理会话的 `kind`(09-18 会话化):挂在父会话下、有自己的 turns、进不了 `user` 列表。
pub const SUBAGENT_SESSION_KIND: &str = "subagent";

/// 子代理会话的任务状态(`sessions.task_state`,v39)。
///
/// 「任务完成」不等于「一轮结束」:子代理起了后台命令或后台孙代理再结束回合,
/// 它是 `Waiting`,等名下的后台任务全部回来、再跑完一轮、名下为空,才进终态。
/// `Interrupted` 是停止或 daemon 重启打断,历史都在库里,回复它就接着跑。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubagentTaskState {
    Running,
    Waiting,
    Done,
    Failed,
    Cancelled,
    Interrupted,
}

impl SubagentTaskState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Waiting => "waiting",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "running" => Self::Running,
            "waiting" => Self::Waiting,
            "done" => Self::Done,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            "interrupted" => Self::Interrupted,
            _ => return None,
        })
    }

    /// 还在算「未完成」的两个状态:父会话等的就是它们翻成终态。
    pub fn is_pending(self) -> bool {
        matches!(self, Self::Running | Self::Waiting)
    }
}
pub const GOAL_ROUND_TAG: &str = "<goal_round>";

/// daemon 合成的「用户消息」开头标签，一处登记：识别函数与回放等 SQL 都从这张表生成，
/// 再加一种不会漏掉某一处（09-23 加跨会话消息时收拢；原来 SQL 里各抄一份）。
pub const SYNTHETIC_USER_CONTENT_TAGS: [&str; 4] = [
    BACKGROUND_JOB_REPORT_TAG,
    GOAL_ROUND_TAG,
    CROSS_SESSION_MESSAGE_TAG,
    SERVICE_RESTART_TAG,
];

/// 这条「用户消息」是不是 daemon 合成的（后台任务唤醒 / 目标续轮 / 跨会话消息 / 重启续跑），
/// 不是谁敲的。
pub fn is_synthetic_user_content(content: &str) -> bool {
    let content = content.trim_start();
    SYNTHETIC_USER_CONTENT_TAGS
        .iter()
        .any(|tag| content.starts_with(tag))
}

/// SQL 里认合成轮的条件：`column` 以任一合成标签开头。按开头若干字符整段比较，
/// 不用 LIKE——标签里的 `_` 在 LIKE 里是通配符。
pub fn synthetic_user_content_sql(column: &str) -> String {
    SYNTHETIC_USER_CONTENT_TAGS
        .iter()
        .map(|tag| format!("substr({column}, 1, {}) = '{tag}'", tag.chars().count()))
        .collect::<Vec<_>>()
        .join(" OR ")
}

fn turns_to_entries(turns: Vec<Turn>) -> Vec<StoredConversationEntry> {
    let mut entries = Vec::with_capacity(turns.len() * 3);
    for turn in turns {
        let ts = turn.assistant_timestamp.clone().unwrap_or_default();
        entries.push(StoredConversationEntry {
            timestamp: turn.user_timestamp,
            role: "user".to_string(),
            content: turn.user_content,
            reasoning: None,
        });
        for exchange in &turn.question_exchanges {
            entries.push(StoredConversationEntry {
                timestamp: exchange.answered_at.clone(),
                role: "assistant_clarification".to_string(),
                content: miyu_base::question::assistant_exchange_text(exchange),
                reasoning: None,
            });
            entries.push(StoredConversationEntry {
                timestamp: exchange.answered_at.clone(),
                role: "user_clarification".to_string(),
                content: miyu_base::question::user_exchange_text(exchange),
                reasoning: None,
            });
        }
        for followup in turn.followups {
            if followup
                .preceding_assistant_content
                .as_deref()
                .is_some_and(|content| !content.trim().is_empty())
                || followup
                    .preceding_assistant_reasoning
                    .as_deref()
                    .is_some_and(|reasoning| !reasoning.trim().is_empty())
            {
                entries.push(StoredConversationEntry {
                    timestamp: followup.submitted_at.clone(),
                    role: "assistant".to_string(),
                    content: followup.preceding_assistant_content.unwrap_or_default(),
                    reasoning: followup.preceding_assistant_reasoning,
                });
            }
            entries.push(StoredConversationEntry {
                timestamp: followup.submitted_at,
                role: "user".to_string(),
                content: followup.content,
                reasoning: None,
            });
        }
        entries.push(StoredConversationEntry {
            timestamp: ts.clone(),
            role: "assistant".to_string(),
            content: turn.assistant_content,
            reasoning: turn.assistant_reasoning,
        });
        for report in turn.tool_reports {
            entries.push(StoredConversationEntry {
                timestamp: ts.clone(),
                role: "assistant".to_string(),
                content: report,
                reasoning: None,
            });
        }
    }
    entries
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct StoredConversationEntry {
    pub timestamp: String,
    pub role: String,
    pub content: String,
    #[serde(default)]
    pub reasoning: Option<String>,
}

#[cfg(test)]
mod tests;
