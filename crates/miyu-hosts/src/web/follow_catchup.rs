//! 挂上来的终端追不回一轮的开头时，从库里的流水补（会话项目第 3 段）。
//!
//! 事件环只留 4096 条（4 MB）。长回合跑到一半才切进来（切进子代理、第二个 TUI、
//! 回合中开面板之后挂回来），开头早就被挤掉了。以前 `follow_run` 在这儿直接报错
//! 收工，挂上来什么也看不到。回合的流水是先落库再显示的，库里一直有全的。

use crate::web::*;

/// 这一轮到现在为止的样子，外加从哪个事件号接着看实时。回合已经不在跑、或者还
/// 没有回合号，就给 None，调用方照旧报错。
///
/// 先记下此刻的事件号，再读流水：两步之间流水正好刷了一段的话，那段会补一次、
/// 实时再来一次（重复一小截），好过漏掉。
pub(in crate::web) fn catch_up_from_journal(
    state: &DaemonState,
    run_id: &str,
) -> Option<(u64, IpcFrame)> {
    let (session_id, turn_id) = {
        let manager = state.manager.lock().unwrap();
        let info = manager.active_runs.get(run_id)?;
        (info.session_id.clone(), info.turn_id.clone()?)
    };
    let resume_at = state.events.latest_id();
    let replay = state
        .stores
        .for_session(&session_id)
        .running_turn_replay(&turn_id)
        .ok()
        .flatten()?;
    let data = json!({
        "run_id": run_id,
        "turn_id": turn_id,
        "replay": replay,
    });
    Some((
        resume_at,
        IpcFrame::Event {
            id: resume_at,
            kind: "turn.catchup".to_string(),
            data,
        },
    ))
}
