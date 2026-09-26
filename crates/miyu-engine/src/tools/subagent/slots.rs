//! 子代理并发（09-26 起只在后台跑）：同一个父会话同时最多 N 个在跑（`tools.subagent_concurrency`，
//! 默认 4），多出来的排队——用户拍板按父会话算，别的会话（QQ 群、另一个终端）不受影响。
//!
//! 以前前台子代理由同一批工具调用分波（`parallel.rs`），一波跑完才起下一波；只在后台跑之后
//! 每个调用当场返回，分波管不住了，改在镜像任务里拿许可：拿到才起子会话那一轮，一直攥到它
//! 名下的事全部收尾（孙代理也算）。排着的任务在任务条上写「排队中」。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

fn slots() -> &'static Mutex<HashMap<String, Arc<Semaphore>>> {
    static SLOTS: OnceLock<Mutex<HashMap<String, Arc<Semaphore>>>> = OnceLock::new();
    SLOTS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 这个父会话的名额。第一次用时按当时的上限建，之后改配置要等 daemon 重启才换（名额里正攥着
/// 的许可不能凭空多出来或少掉）。
pub(super) fn slots_for(parent_session: &str, limit: usize) -> Arc<Semaphore> {
    slots()
        .lock()
        .unwrap()
        .entry(parent_session.to_string())
        .or_insert_with(|| Arc::new(Semaphore::new(limit.max(1))))
        .clone()
}

/// 拿一个名额。当场拿不到就先在任务条上标「排队中」，再等。
pub(super) async fn take_slot(slots: Arc<Semaphore>, job_id: &str) -> OwnedSemaphorePermit {
    if let Ok(permit) = slots.clone().try_acquire_owned() {
        return permit;
    }
    crate::tools::jobs::set_metric(job_id, miyu_base::i18n::text("queued", "排队中"), None);
    slots
        .acquire_owned()
        .await
        .expect("subagent slots are never closed")
}
