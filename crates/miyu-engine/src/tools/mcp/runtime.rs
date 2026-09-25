//! MCP 子进程的收发跑在自己的线程上（09-25）。
//!
//! daemon 的 actor 与主 runtime 都是单线程，actor 有时会同步阻塞一阵。常驻连接的读写任务
//! 要是挂在它们上面，一个会话卡住，别的会话调同一件工具也跟着等。这条线程只管 MCP 子
//! 进程：起进程、读写管道、巡检闲置的连接。子进程也都从这条线程起（进程组、沙盒规则在
//! `pre_exec` 里装，和在哪条线程上起无关）。

use anyhow::{Context, Result};
use std::future::Future;
use std::sync::OnceLock;
use tokio::runtime::Handle;

pub(super) fn handle() -> &'static Handle {
    static HANDLE: OnceLock<Handle> = OnceLock::new();
    HANDLE.get_or_init(|| {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("MCP runtime");
        let handle = runtime.handle().clone();
        std::thread::Builder::new()
            .name("miyu-mcp".to_string())
            .spawn(move || runtime.block_on(std::future::pending::<()>()))
            .expect("MCP thread");
        handle
    })
}

/// 在 MCP 线程上跑 `future`，在调用方自己的 runtime 里等结果。调用方不等了（回合被
/// 取消、外层超时），那边的任务跟着撤掉——一次性的服务器进程随之收掉，不会泄漏。
pub(super) async fn run<F, T>(future: F) -> Result<T>
where
    F: Future<Output = Result<T>> + Send + 'static,
    T: Send + 'static,
{
    let task = AbortOnDrop(handle().spawn(future));
    task.wait().await
}

struct AbortOnDrop<T>(tokio::task::JoinHandle<Result<T>>);

impl<T> AbortOnDrop<T> {
    async fn wait(mut self) -> Result<T> {
        let joined = (&mut self.0).await;
        joined.context("MCP worker task failed")?
    }
}

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}
