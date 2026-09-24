//! 子代理宿主端口(09-18 子代理会话化)。
//!
//! 子代理不再是工具层里自己转的小循环,而是一条真会话:建在父会话下、由 daemon
//! 的 actor 跑普通回合。建会话、起回合、等「任务完成」都是场所层的事,工具层
//! 只认这条窄接口;`web::subagent_host` 在 daemon 启动时装实现。非 daemon 进程
//! (REPL 直连、`miyu tool-call`)里没人装,取到 `None`,工具层退回进程内的老循环。
//!
//! 「任务完成」不是「一轮结束」:子会话没有活动回合**且**名下没有未完成的后台
//! 任务(后台命令、后台孙代理都算)才算。前台调用的 future 等的就是这个终态。

use crate::config::ModelTier;
use anyhow::Result;
use futures_util::future::BoxFuture;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

/// 子会话回合里的事件折成 `__subagent_*` / `__subtool_*` 标记喂回来:前台原样转进
/// 父回合的进度通道,后台进任务日志桥。渲染层照旧认这些标记,一字不改。
pub type SubagentProgressSink = Arc<dyn Fn(String) + Send + Sync>;

pub struct SpawnChildRequest {
    pub parent_session: String,
    pub description: String,
    pub prompt: String,
    pub dev: bool,
    pub tier: ModelTier,
    /// 只是记在会话行上的元数据:等不等终态由调用方决定(后台 = 包进后台任务里等)。
    pub background: bool,
    pub max_steps: usize,
    /// 父会话里开它的那一轮。
    pub spawned_by_turn: Option<String>,
    /// 父回合的工作目录:子会话的回合沿用它,别退回 daemon 的 cwd。
    pub workdir: Option<PathBuf>,
    pub progress: SubagentProgressSink,
}

pub struct ContinueChildRequest {
    pub parent_session: String,
    pub child_session: String,
    pub message: String,
    pub workdir: Option<PathBuf>,
    pub progress: SubagentProgressSink,
}

/// 只等、不起新一轮(09-24 断点续跑):daemon 重启后,一个自己的回合已经结束、在等
/// 孙代理的子代理挂回父会话名下,等它的孙代理被另外接回、跑完叫醒它、它收尾。
pub struct WatchChildRequest {
    pub parent_session: String,
    pub child_session: String,
    pub progress: SubagentProgressSink,
}

pub struct ChildTaskResult {
    pub session_id: String,
    /// done / failed / cancelled / interrupted。
    pub state: String,
    /// 子会话最后一轮的正文:交付物。
    pub final_text: String,
    pub turns: i64,
    pub total_tokens: u64,
    pub provider_id: Option<String>,
    pub model: Option<String>,
}

pub enum ChildOutcome {
    /// 等到了任务终态。
    Finished(ChildTaskResult),
    /// 续接一个正在跑的子会话:话已排进它当前那一轮,不等。
    Queued { session_id: String },
}

pub trait SubagentHostPort: Send + Sync {
    /// 子会话此刻有没有活动回合。工具层据此决定追话是「排进去」还是「起新一轮」,
    /// 免得后台追话对着闲着的子会话空等。
    fn is_running(&self, child_session: &str) -> bool;
    /// 建子会话并起第一轮,等到任务终态才完成。future 被 drop(父回合被停)时
    /// 宿主要顺手取消子会话的活动回合——工具层没有取消令牌,只有这一条路。
    fn spawn(&self, request: SpawnChildRequest) -> BoxFuture<'static, Result<ChildOutcome>>;
    /// 给已有子会话追话:跑着就排 follow-up 立刻返回;闲着/中断了就起新一轮并等终态。
    fn continue_child(
        &self,
        request: ContinueChildRequest,
    ) -> BoxFuture<'static, Result<ChildOutcome>>;
    /// 等一个已有子会话走到任务终态,不给它起新一轮。名下已经没有未完成的事就当场返回。
    fn watch_child(&self, request: WatchChildRequest) -> BoxFuture<'static, Result<ChildOutcome>>;
}

static SUBAGENT: RwLock<Option<Arc<dyn SubagentHostPort>>> = RwLock::new(None);

pub fn install_subagent_port(port: Arc<dyn SubagentHostPort>) {
    *SUBAGENT.write().unwrap() = Some(port);
}

/// 子代理宿主端口;非 daemon 进程里为 `None`。
pub fn subagent_port() -> Option<Arc<dyn SubagentHostPort>> {
    SUBAGENT.read().unwrap().clone()
}
