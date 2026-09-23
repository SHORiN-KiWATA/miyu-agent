//! 大厅动画在「命令等 daemon」期间也要走:`await_in_lobby` 每 40ms 推一帧。

use crate::cli::repl::banner::BannerScene;
use crate::cli::repl::editor::*;
use crate::cli::*;

fn lobby_tail(banner: Option<BannerScene>) -> LiveReplTail {
    let config = AppConfig::default();
    LiveReplTail {
        editor: LiveReplEditor::new(PersonaLane::Active, Vec::new()),
        queued: Vec::new(),
        pending_chunks: Vec::new(),
        footer: ReplFooterStatus::from_config(&config, 0, TurnTokens::default()),
        round_base_footer: None,
        footer_offset: None,
        footer_spinner_last: None,
        goal_hint_drawn: String::new(),
        output_cursor: (0, 0),
        tail_start: 0,
        tail_rows: 0,
        job_strip_start: 0,
        job_strip_rows: 0,
        job_hover: None,
        last_mouse_move: None,
        pending_stop_job: None,
        input_cursor: (0, 0),
        // 画过了才会推帧;`banner_rows == 0` 让 inline 路径推完帧就返回,不往
        // 测试的 stdout 打字节。
        rendered: true,
        external_output_active: false,
        raw_mode_handoff: false,
        screen: None,
        banner,
        banner_rows: 0,
        lobby_panel_rows: 0,
        suppress_switch_note: false,
        session_footer_stale: false,
        cumulative_from_poll: false,
        lobby_lane_pending: false,
        jobs: Vec::new(),
        suppressed_jobs: std::collections::HashMap::new(),
        live_turn_tokens: 0,
        job_spinner: 0,
        job_spinner_started: std::time::Instant::now(),
    }
}

/// 等 220ms 的 future,banner 至少走 3 帧;没有 banner 就是普通 await。
#[tokio::test]
async fn awaiting_in_the_lobby_keeps_ticking_the_banner() {
    let mut live = lobby_tail(Some(BannerScene::builtin_for_tests(PersonaLane::Active)));
    await_in_lobby(
        &mut live,
        tokio::time::sleep(std::time::Duration::from_millis(220)),
    )
    .await;
    let frames = live.banner.as_ref().map(BannerScene::frame).unwrap();
    assert!(frames >= 3, "banner advanced only {frames} frames in 220ms");

    let mut live = lobby_tail(None);
    assert_eq!(await_in_lobby(&mut live, async { 7 }).await, 7);
}
