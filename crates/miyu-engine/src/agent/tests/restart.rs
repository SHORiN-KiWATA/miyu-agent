//! 没跑完就被丢下的回合怎么落库(09-24 断点续跑):人按停止收成「已中断」;daemon 有序
//! 关停只记用量、留着「执行中」,下一个 daemon 才认得出是被重启打断的。

use super::shared::*;
use crate::agent::control::settle_unfinished_turn;
use crate::agent::*;

fn status_and_usage(state: &StateStore, turn_id: &str) -> (miyu_core::state::TurnStatus, u64) {
    let turns = state.load_turns().unwrap();
    let turn = turns.iter().find(|turn| turn.turn_id == turn_id).unwrap();
    (turn.status, turn.token_total)
}

fn usage() -> TurnTokens {
    TurnTokens {
        total: 900,
        prompt: 800,
        cache_read: 500,
    }
}

#[test]
fn a_stopped_turn_is_interrupted_with_its_usage() {
    let temp = tempfile::tempdir().unwrap();
    let state = StateStore::new(&test_paths(temp.path())).unwrap();
    state.init_files().unwrap();
    state
        .start_turn("turn_stop", "task", std::process::id())
        .unwrap();
    settle_unfinished_turn(&state, "turn_stop", usage(), false).unwrap();
    assert_eq!(
        status_and_usage(&state, "turn_stop"),
        (miyu_core::state::TurnStatus::Interrupted, 900)
    );
}

/// 退回修复前(关停也按停止收尾)这条会红:库里成了普通的「已中断」,新 daemon
/// 分不出它是被重启打断的,也就不会接着跑。
#[test]
fn a_turn_cut_by_shutdown_stays_running_with_its_usage() {
    let temp = tempfile::tempdir().unwrap();
    let state = StateStore::new(&test_paths(temp.path())).unwrap();
    state.init_files().unwrap();
    state
        .start_turn("turn_down", "task", std::process::id())
        .unwrap();
    settle_unfinished_turn(&state, "turn_down", usage(), true).unwrap();
    assert_eq!(
        status_and_usage(&state, "turn_down"),
        (miyu_core::state::TurnStatus::Running, 900)
    );
}
