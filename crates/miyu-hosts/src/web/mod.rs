use miyu_base::config::{ActiveProviderModelConfig, AppConfig, PromptAudience, ProviderConfig};
use miyu_base::i18n::text as t;
use miyu_base::paths::MiyuPaths;
use miyu_base::question::{self, QuestionAnswers};
use miyu_core::args::WebArgs;
use miyu_core::ipc::{
    self, Command as IpcCommand, Frame as IpcFrame, ImageAttachment, Request as IpcRequest,
};
use miyu_core::llm::{
    thinking_variant_options_for_model, ChatResult, ChatStreamKind, OpenAiCompatibleClient,
    ThinkingVariantOptions, ThinkingVariantPreferences, Usage,
};
use miyu_core::memory::{
    MemoryAccess, MemoryOrganizer, MemoryOrganizerHandle, MemoryOrigin, MemoryStore,
};
use miyu_engine::agent::{
    archive_and_delete_visible_turns, Agent, AgentEvent, AgentTurnControl, PersonaLane,
};
// daemon 运行时的共享状态已下沉到 runtime：web 只是它的消费者之一，IPC 与
// 平台适配是另外两个。放在 web 里会让平台层反过来依赖 HTTP 服务。
mod accounts_api;
mod actor;
mod assets;
mod attachments;
mod bridge_progress;
mod bridge_question;
mod commands_api;
mod config_api;
mod cross_session;
mod dashboards;
mod dto;
mod event_map;
mod goal_driver;
mod link_preview;
mod member_persona;
mod ownership;
mod persona;
mod prompt_files;
mod providers_api;
mod qq_history;
mod sandbox_scope;
mod security;
mod selection_menu;
mod server;
mod session_cmds;
mod session_effort;
mod sessions;
mod shared_files;
mod subagent_host;
#[cfg(test)]
mod tests;
mod tty;
mod turns;
mod ui_locale;
mod ui_prefs;
mod voice_api;
pub mod voice_bridge;
pub mod voice_tts;
// 叫 ipc_server 而不是 ipc：`web::ipc` 会把 `miyu_core::ipc` 遮住，本文件里几十处
// `ipc::send` 会突然解析到子模块上——编译期就报，但报错信息（找不到 send）
// 离真正的原因很远。
mod ipc_server;

use accounts_api::*;
use actor::*;
use assets::*;
use attachments::*;
use bridge_progress::*;
use bridge_question::*;
use commands_api::*;
use config_api::*;
use cross_session::*;
use dashboards::affection::*;
use dashboards::kb::*;
use dashboards::ledger::*;
use dashboards::memes::*;
use dashboards::memory::*;
use dashboards::qq::*;
use dashboards::scripts::*;
use dashboards::sponsor::*;
use dto::*;
use event_map::*;
use goal_driver::*;
use ipc_server::*;
use ownership::*;
use persona::*;
use prompt_files::*;
use providers_api::*;
use qq_history::*;
use sandbox_scope::*;
use security::*;
use selection_menu::*;
pub(crate) use server::run;
use server::*;
use session_cmds::*;
use session_effort::*;
use sessions::*;
use shared_files::*;
use subagent_host::*;
use tty::*;
use turns::*;
use ui_prefs::*;
use voice_api::*;

use crate::runtime::{
    cold_context, enqueue_turn_update, finish_run, random_id, random_token, release_admin,
    reset_platform_persona_state, safe_error_message, startup_context, validate_content,
    ActorCommand, AdminFailure, AnswerFailure, ApiError, ContextSnapshot, DaemonState, EventHub,
    EventRecord, IpcRunGuard, LoginFailure, ManagerState, PlatformPersonaResetError,
    PromptDocument, PromptDocuments, QuestionBroker, RedoWebPrompt, RunInfo, RunOperation,
    SafeQueuedPrompt, SafeUserAttachment, StoreRegistry, ThinkingVariantUpdate, TurnEngineState,
    TurnResourceCache, TurnUpdateMode, TurnUpdateReceipt, TurnUpdateRequest, WebAuth, WebIdentity,
};
use anyhow::{bail, Context, Result};
use axum::body::Bytes;
use axum::extract::{ConnectInfo, DefaultBodyLimit, Path, Query, State};
use axum::http::header::{
    ACCEPT_ENCODING, ACCESS_CONTROL_ALLOW_METHODS, ACCESS_CONTROL_ALLOW_ORIGIN,
    ACCESS_CONTROL_MAX_AGE, CACHE_CONTROL, CONTENT_DISPOSITION, CONTENT_ENCODING, CONTENT_LENGTH,
    CONTENT_SECURITY_POLICY, CONTENT_TYPE, COOKIE, HOST, ORIGIN, REFERRER_POLICY, RETRY_AFTER,
    SET_COOKIE, X_CONTENT_TYPE_OPTIONS,
};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post, put};
use axum::{Json, Router};
use base64::Engine;
use futures_util::stream::{self, Stream};
use futures_util::StreamExt;
use miyu_core::state::{
    ArtifactAsset, ImageAsset, PlatformPluginScopeKey, QueuedPrompt, StateStore, Turn,
    TurnFollowup, TurnStatus, UsageSnapshot, UserAttachment, USER_ATTACHMENT_KIND_FILE,
    USER_ATTACHMENT_KIND_IMAGE, USER_ATTACHMENT_KIND_TEXT,
};
use miyu_engine::tools::build_tool_registry;
use miyu_engine::tools::{self, CommandOutputStream};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet, VecDeque};
use std::convert::Infallible;
use std::future::IntoFuture;
use std::io::Write;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path as FilePath, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::sync::{broadcast, mpsc, oneshot, Semaphore};
use tokio::task::JoinHandle as TokioJoinHandle;

use crate::platforms::{self, PlatformRuntime};

const JSON_BODY_LIMIT: usize = 4 * 1024 * 1024;
const PERSONA_ASSET_LIMIT: usize = 8 * 1024 * 1024;
const MAX_PROMPT_DOCUMENT_CHARS: usize = 200_000;
const MAX_PROMPT_DOCUMENTS: usize = 128;

const INDEX_HTML: &str = include_str!("../../../../web/index.html");
const STYLES_CSS: &str = include_str!("../../../../web/styles.css");
const APP_JS: &str = include_str!("../../../../web/app.js");
// 斜杠命令层单独一个文件:app.js 已经 9500 行,再往里长就找不到东西了。
const COMMANDS_JS: &str = include_str!("../../../../web/commands.js");
const LIGHTBOX_JS: &str = include_str!("../../../../web/lightbox.js");
const PREVIEW_JS: &str = include_str!("../../../../web/preview.js");
const LINKCARDS_JS: &str = include_str!("../../../../web/linkcards.js");
const TODOS_JS: &str = include_str!("../../../../web/todos.js");
// 跨会话消息(09-23):在线登记心跳与消息外壳的显示。
const CROSS_SESSION_JS: &str = include_str!("../../../../web/crosssession.js");
// 侧栏会话批量删除(09-24):选择模式与操作栏。
const SESSION_SELECT_JS: &str = include_str!("../../../../web/sessionselect.js");
// 聊天正文选中文字的右键菜单(2026-09-14)。
const SELECTION_MENU_JS: &str = include_str!("../../../../web/selectionmenu.js");
// 代码块语法高亮:只用 Prism 的分词器,上色的 DOM 由这个文件亲手搭。
const HIGHLIGHT_JS: &str = include_str!("../../../../web/highlight.js");
// 文件分享面板:独立文件,与 artifact 演示区无关。
const SHARED_JS: &str = include_str!("../../../../web/shared.js");
// WebUI 双语运行时与英文词典(2026-09-23):语言由服务端注入,见 ui_locale.rs。
const I18N_JS: &str = include_str!("../../../../web/i18n.js");
const I18N_EN_JS: &str = include_str!("../../../../web/i18n-en.js");
// 文件编辑工具的 diff 渲染:把 patchText 参数画成增删配色的 diff 卡。
const DIFF_JS: &str = include_str!("../../../../web/diff.js");
// 插件 dashboard 脚本走 assets.rs 的 DASH_SCRIPTS 静态表,加面板只改那一行。
// KaTeX 0.18.4(vendored):公式渲染;字体只带 woff2(css 里 woff2 列首,
// 现代浏览器不会去请求 woff/ttf 回退项)。
const KATEX_JS: &str = include_str!("../../../../web/vendor/katex/katex.min.js");
// PrismJS 1.29.0(vendored,MIT):core + 18 门常用语言,47KB。头部注释里写了
// 拼装顺序,换版本照那个顺序重拼即可。
const PRISM_JS: &str = include_str!("../../../../web/vendor/prism/prism.min.js");
const KATEX_CSS: &str = include_str!("../../../../web/vendor/katex/katex.min.css");
// Apache ECharts 6.1.0(vendored,Apache-2.0):artifact 里画图表用的。
// **存的是 gzip 后的字节**(1096KB → 359KB),响应直接带 Content-Encoding: gzip
// 发出去,服务端不解压。更新照做:
//   curl -sL https://cdn.jsdelivr.net/npm/echarts@<版本>/dist/echarts.min.js \
//     | gzip -9 -n > web/vendor/echarts/echarts.min.js.gz
// `-n` 不能少——带上文件名和时间戳的话每次压出来的字节都不一样,构建就不可复现了。
const ECHARTS_JS_GZ: &[u8] = include_bytes!("../../../../web/vendor/echarts/echarts.min.js.gz");
static KATEX_FONTS: &[(&str, &[u8])] = &[
    (
        "KaTeX_AMS-Regular.woff2",
        include_bytes!("../../../../web/vendor/katex/fonts/KaTeX_AMS-Regular.woff2"),
    ),
    (
        "KaTeX_Caligraphic-Bold.woff2",
        include_bytes!("../../../../web/vendor/katex/fonts/KaTeX_Caligraphic-Bold.woff2"),
    ),
    (
        "KaTeX_Caligraphic-Regular.woff2",
        include_bytes!("../../../../web/vendor/katex/fonts/KaTeX_Caligraphic-Regular.woff2"),
    ),
    (
        "KaTeX_Fraktur-Bold.woff2",
        include_bytes!("../../../../web/vendor/katex/fonts/KaTeX_Fraktur-Bold.woff2"),
    ),
    (
        "KaTeX_Fraktur-Regular.woff2",
        include_bytes!("../../../../web/vendor/katex/fonts/KaTeX_Fraktur-Regular.woff2"),
    ),
    (
        "KaTeX_Main-Bold.woff2",
        include_bytes!("../../../../web/vendor/katex/fonts/KaTeX_Main-Bold.woff2"),
    ),
    (
        "KaTeX_Main-BoldItalic.woff2",
        include_bytes!("../../../../web/vendor/katex/fonts/KaTeX_Main-BoldItalic.woff2"),
    ),
    (
        "KaTeX_Main-Italic.woff2",
        include_bytes!("../../../../web/vendor/katex/fonts/KaTeX_Main-Italic.woff2"),
    ),
    (
        "KaTeX_Main-Regular.woff2",
        include_bytes!("../../../../web/vendor/katex/fonts/KaTeX_Main-Regular.woff2"),
    ),
    (
        "KaTeX_Math-BoldItalic.woff2",
        include_bytes!("../../../../web/vendor/katex/fonts/KaTeX_Math-BoldItalic.woff2"),
    ),
    (
        "KaTeX_Math-Italic.woff2",
        include_bytes!("../../../../web/vendor/katex/fonts/KaTeX_Math-Italic.woff2"),
    ),
    (
        "KaTeX_SansSerif-Bold.woff2",
        include_bytes!("../../../../web/vendor/katex/fonts/KaTeX_SansSerif-Bold.woff2"),
    ),
    (
        "KaTeX_SansSerif-Italic.woff2",
        include_bytes!("../../../../web/vendor/katex/fonts/KaTeX_SansSerif-Italic.woff2"),
    ),
    (
        "KaTeX_SansSerif-Regular.woff2",
        include_bytes!("../../../../web/vendor/katex/fonts/KaTeX_SansSerif-Regular.woff2"),
    ),
    (
        "KaTeX_Script-Regular.woff2",
        include_bytes!("../../../../web/vendor/katex/fonts/KaTeX_Script-Regular.woff2"),
    ),
    (
        "KaTeX_Size1-Regular.woff2",
        include_bytes!("../../../../web/vendor/katex/fonts/KaTeX_Size1-Regular.woff2"),
    ),
    (
        "KaTeX_Size2-Regular.woff2",
        include_bytes!("../../../../web/vendor/katex/fonts/KaTeX_Size2-Regular.woff2"),
    ),
    (
        "KaTeX_Size3-Regular.woff2",
        include_bytes!("../../../../web/vendor/katex/fonts/KaTeX_Size3-Regular.woff2"),
    ),
    (
        "KaTeX_Size4-Regular.woff2",
        include_bytes!("../../../../web/vendor/katex/fonts/KaTeX_Size4-Regular.woff2"),
    ),
    (
        "KaTeX_Typewriter-Regular.woff2",
        include_bytes!("../../../../web/vendor/katex/fonts/KaTeX_Typewriter-Regular.woff2"),
    ),
];
// 这两张是 `pics/` 里原图的**显示尺寸副本**，不是原图。原图 1254×1254 和
// 3344×1882，而 WebUI 里头像只显示 38/64 px、看板图最大 330×178 px——浏览器
// 解码是按像素数来的，原图会占掉 30 MiB GPU 纹理去画两个缩略图，还让二进制
// 多背 7.2 MiB。降到 256×256 和 1280×720（2x DPR 仍有富余）后纹理 3.7 MiB。
// 原图留在 `pics/` 不动：README、终端演示、外部链接还在引用。
// 重新生成见 `scripts/gen_web_assets.py`。
const MIYU_LOGO: &[u8] = include_bytes!("../../../../web/assets/miyu-logo.png");
const MIYU_WALLPAPER: &[u8] = include_bytes!("../../../../web/assets/miyuwallpaper.png");

impl From<QueuedPrompt> for SafeQueuedPrompt {
    fn from(prompt: QueuedPrompt) -> Self {
        Self {
            id: prompt.prompt_id,
            content: prompt.display_content,
            submitted_at: prompt.submitted_at,
            attachments: prompt
                .uploaded_attachments
                .into_iter()
                .map(SafeUserAttachment::from)
                .collect(),
        }
    }
}

impl From<UserAttachment> for SafeUserAttachment {
    fn from(attachment: UserAttachment) -> Self {
        Self {
            url: format!("/api/attachments/{}", attachment.attachment_id),
            id: attachment.attachment_id,
            name: attachment.file_name,
            mime: attachment.mime,
            kind: attachment.kind,
            size: attachment.size_bytes,
            width: attachment.width,
            height: attachment.height,
        }
    }
}

/// 一条会话清空或删掉了：它名下常驻的外部进程一并忘掉——中转线 CLI 的续传映射、
/// MCP 服务器进程（09-25）。原来只有前一样，各处各调一遍；合成一处，免得再加一种时漏掉几处。
pub(in crate::web) fn forget_session_processes(session_id: &str) {
    miyu_core::llm::forget_relay_sessions(session_id);
    miyu_engine::tools::forget_mcp_session(session_id);
}

// ── spawn_actor ──

impl DaemonState {}

#[cfg(test)]
mod test_support;
