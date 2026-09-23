//! 跨会话消息端口（09-23）：一个会话里的 AI 给同一个人别的开着的会话发话
//! （`send_to_other_running_session` 工具）。
//!
//! 名单（谁开着、谁在跑）与投递（排进对方正在跑的那一轮，或替它起一轮）都是场所层
//! 的事，工具层只认这条窄接口；`web::cross_session` 在 daemon 启动时装实现。非 daemon
//! 进程（REPL 直连、测试）里没人装，取到 `None`。

use anyhow::Result;
use futures_util::future::BoxFuture;
use std::sync::{Arc, RwLock};

/// 名单里的一条会话。字段名就是工具输出给模型看的键。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PeerSession {
    pub session_id: String,
    /// id 末尾那段随机串，界面上显示的就是它；`send` 也认。
    pub short_id: String,
    pub name: String,
    /// `normal` / `dev`。
    pub mode: String,
    /// 它下一轮的工作目录。
    pub cwd: String,
    /// 此刻有一轮在跑。
    pub running: bool,
    /// 此刻有终端或网页开着它。
    pub open: bool,
    /// 会话内容所在的库（`turns` 表，按 `session_id` 取）。
    pub data: String,
}

/// 消息怎么到的。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerDelivery {
    /// 排进了对方正在跑的那一轮。
    Queued,
    /// 对方闲着，替它起了一轮。
    Started,
}

pub trait CrossSessionPort: Send + Sync {
    /// `from_session` 能发到的会话：同一个人名下、开着或正在跑的，不含它自己。
    fn peers(&self, from_session: &str) -> Result<Vec<PeerSession>>;
    /// 发件会话自己叫什么。名单里要单独写明「这条是你」：模型不知道自己的会话 id
    /// （主机环境块里没有），名单里只剩一条时会把它当成自己（09-24 真机）。
    fn own_name(&self, session_id: &str) -> Option<String>;
    /// 投一条消息。`to_session` 是完整 id，或名单里唯一匹配的短 id；对方不在
    /// [`peers`](Self::peers) 名单里就报错。
    fn send(
        &self,
        from_session: &str,
        to_session: &str,
        message: &str,
    ) -> BoxFuture<'static, Result<(PeerSession, PeerDelivery)>>;
}

static CROSS_SESSION: RwLock<Option<Arc<dyn CrossSessionPort>>> = RwLock::new(None);

pub fn install_cross_session_port(port: Arc<dyn CrossSessionPort>) {
    *CROSS_SESSION.write().unwrap() = Some(port);
}

/// 跨会话消息端口；非 daemon 进程里为 `None`。
pub fn cross_session_port() -> Option<Arc<dyn CrossSessionPort>> {
    CROSS_SESSION.read().unwrap().clone()
}
