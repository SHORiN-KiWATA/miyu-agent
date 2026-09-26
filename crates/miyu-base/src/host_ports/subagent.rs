//! 子代理宿主端口(09-18 子代理会话化)。
//!
//! 子代理不再是工具层里自己转的小循环,而是一条真会话:建在父会话下、由 daemon
//! 的 actor 跑普通回合。建会话、起回合、等「任务完成」都是场所层的事,工具层
//! 只认这条窄接口;`web::subagent_host` 在 daemon 启动时装实现。非 daemon 进程
//! (REPL 直连、`miyu tool-call`)里没人装,取到 `None`,工具层退回进程内的老循环。
//!
//! 「任务完成」不是「一轮结束」:子会话没有活动回合**且**名下没有未完成的后台
//! 任务(后台命令、后台孙代理都算)才算。09-26 起子代理只在后台跑:后台镜像任务里
//! `continue_child` 的 future 等的就是这个终态。

use crate::config::ModelTier;
use anyhow::Result;
use futures_util::future::BoxFuture;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

/// 子会话回合里的事件折成 `__subagent_*` / `__subtool_*` 标记喂回来,进后台任务的日志桥。
/// 渲染层照旧认这些标记,一字不改。
pub type SubagentProgressSink = Arc<dyn Fn(String) + Send + Sync>;

/// 只建子会话、不起回合（09-26 子代理只在后台跑）：工具层先拿到子会话 id，写进回执、让父回合
/// 那一步链得过去；第一轮由后台镜像任务经 [`SubagentHostPort::continue_child`] 起。
pub struct CreateChildRequest {
    pub parent_session: String,
    pub description: String,
    pub dev: bool,
    pub tier: ModelTier,
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
}

pub enum ChildOutcome {
    /// 等到了任务终态。
    Finished(ChildTaskResult),
    /// 续接一个正在跑的子会话:话已排进它当前那一轮,不等。
    Queued { session_id: String },
}

pub trait SubagentHostPort: Send + Sync {
    /// 只建子会话(挂在父会话下、归属与沙盒跟父、档位落成会话级模型池),交回它的 id。
    fn create_child(&self, request: CreateChildRequest) -> Result<String>;
    /// 给已有子会话追话,只排不跑:它跑着就把话排进它当前那一轮、交回子会话 id,闲着就 `None`
    /// (调用方另起后台那一轮)。原来先问「跑着吗」再 [`Self::continue_child`],子会话恰好在两步之间
    /// 收尾时,`continue_child` 就会在父回合的这一步里把整轮跑完(09-26 审查)。
    fn queue_followup(
        &self,
        parent_session: &str,
        child_session: &str,
        message: &str,
    ) -> BoxFuture<'static, Result<Option<String>>>;
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
