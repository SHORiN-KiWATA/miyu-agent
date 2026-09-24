//! goal 的进程内运行时状态，集中在一张表里。
//!
//! 这些状态有意不落库（理由见 `mod.rs` 顶部）。早先散在六个各自为政的全局
//! map 里：取消路径漏清、会话删掉后条目永不回收、每个 map 自带一把锁。集中
//! 之后一把锁管全部，空条目随手剪掉，`forget_session` 一次清完。

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, OnceLock};

#[derive(Default)]
struct SessionState {
    /// 「是否自动续跑」。驻内存、绝不落库：daemon 重启后必须由人重新授权。唯一的例外是
    /// 被重启打断在半路的那一轮续轮：接着跑它时重新武装（`web::actor::restart_resume`，
    /// 用户 09-24「接着跑，自动续轮也恢复」）。
    armed: bool,
    /// 上一轮一个工具都没调（原地说话）——在人开口之前不再驱动。
    awaiting_human: bool,
    /// 下一个工具步要注入的指令（收尾 wrapup、目标被人改了 等）。
    notices: Vec<String>,
}

impl SessionState {
    fn is_empty(&self) -> bool {
        !self.armed && !self.awaiting_human && self.notices.is_empty()
    }
}

/// 驱动器起的续轮 run 的登记：属于哪个会话、这一轮有没有真的调过工具。
struct RunState {
    session_id: String,
    productive: bool,
}

#[derive(Default)]
struct GoalRuntime {
    sessions: HashMap<String, SessionState>,
    runs: HashMap<String, RunState>,
}

fn runtime() -> MutexGuard<'static, GoalRuntime> {
    static RUNTIME: OnceLock<Mutex<GoalRuntime>> = OnceLock::new();
    RUNTIME.get_or_init(Mutex::default).lock().unwrap()
}

/// 改完顺手剪掉空条目，表的大小始终只和「有目标状态的会话」成正比。
fn with_session(rt: &mut GoalRuntime, session_id: &str, mutate: impl FnOnce(&mut SessionState)) {
    let entry = rt.sessions.entry(session_id.to_string()).or_default();
    mutate(entry);
    if entry.is_empty() {
        rt.sessions.remove(session_id);
    }
}

pub fn is_armed(session_id: &str) -> bool {
    runtime()
        .sessions
        .get(session_id)
        .is_some_and(|state| state.armed)
}

pub fn set_armed(session_id: &str, armed: bool) {
    with_session(&mut runtime(), session_id, |state| state.armed = armed);
}

pub fn is_awaiting_human(session_id: &str) -> bool {
    runtime()
        .sessions
        .get(session_id)
        .is_some_and(|state| state.awaiting_human)
}

pub fn set_awaiting_human(session_id: &str, awaiting: bool) {
    with_session(&mut runtime(), session_id, |state| {
        state.awaiting_human = awaiting;
    });
}

/// 挂一段下个工具步要注入的指令。
pub fn push_turn_notice(session_id: &str, notice: String) {
    with_session(&mut runtime(), session_id, |state| {
        state.notices.push(notice);
    });
}

/// 取走全部待注入指令（取一次就清），多条按先后拼接。
pub fn take_turn_notices(session_id: &str) -> Option<String> {
    let mut rt = runtime();
    let mut taken = None;
    with_session(&mut rt, session_id, |state| {
        if !state.notices.is_empty() {
            taken = Some(std::mem::take(&mut state.notices).join("\n\n"));
        }
    });
    taken
}

/// 登记一条驱动器起的续轮 run。
pub fn track_goal_run(run_id: &str, session_id: &str) {
    runtime().runs.insert(
        run_id.to_string(),
        RunState {
            session_id: session_id.to_string(),
            productive: false,
        },
    );
}

/// 这个 run 调过工具了。没登记过的（人类轮等）直接忽略。
pub fn mark_run_productive(run_id: &str) {
    if let Some(run) = runtime().runs.get_mut(run_id) {
        run.productive = true;
    }
}

/// run 走到终点（完成/失败/取消都算）。是续轮就摘掉登记并返回
/// `(会话, 这一轮干过活没)`；不是就返回 `None`。
pub fn finish_run(run_id: &str) -> Option<(String, bool)> {
    runtime()
        .runs
        .remove(run_id)
        .map(|run| (run.session_id, run.productive))
}

/// 会话被删除/重置时一次清干净，别让条目在内存里陪跑到进程退出。
pub fn forget_session(session_id: &str) {
    let mut rt = runtime();
    rt.sessions.remove(session_id);
    rt.runs.retain(|_, run| run.session_id != session_id);
}
