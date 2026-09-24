//! 几个测试文件共用的构造辅助。
use crate::cli::repl::editor::LiveReplEditor;
use crate::cli::*;

/// 一个没上屏的活动区：只测它的记账（任务条、访问栈），不往终端写字。
pub(super) fn detached_tail() -> LiveReplTail {
    let config = AppConfig::default();
    LiveReplTail {
        editor: LiveReplEditor::new(PersonaLane::Active, Vec::new()),
        queued: Vec::new(),
        pending_chunks: Vec::new(),
        footer: ReplFooterStatus::from_config(&config, 0, TurnTokens::default()),
        round_base_footer: None,
        footer_offset: None,
        usage_placement: UsagePlacement::FooterRight,
        footer_spinner_last: None,
        turn_started: None,
        goal_hint_drawn: String::new(),
        output_cursor: (0, 0),
        tail_start: 0,
        tail_rows: 0,
        job_strip_start: 0,
        job_strip_rows: 0,
        job_hover: None,
        strip_sessions: Vec::new(),
        visits: Vec::new(),
        pending_strip_action: None,
        strip_focus: None,
        strip_scroll: 0,
        command_pick: None,
        last_mouse_move: None,
        pending_stop_job: None,
        input_cursor: (0, 0),
        rendered: false,
        external_output_active: false,
        raw_mode_handoff: false,
        screen: None,
        banner: None,
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

pub(super) fn sample_pop_turn(status: TurnStatus) -> Turn {
    Turn {
        turn_id: "turn-1".to_string(),
        seq: 1,
        user_content: "first prompt line\nsecond prompt line".to_string(),
        display_content: "first prompt line\nsecond prompt line".to_string(),
        user_timestamp: "2026-07-19 10:42".to_string(),
        assistant_content: "first answer line\nsecond answer line".to_string(),
        assistant_reasoning: Some("private reasoning".to_string()),
        assistant_provider_id: None,
        assistant_model: None,
        assistant_timestamp: Some("2026-07-19 10:43".to_string()),
        status,
        tool_reports: vec!["hidden tool report".to_string()],
        tool_flow: Vec::new(),
        question_exchanges: Vec::new(),
        followups: Vec::new(),
        attachments: Vec::new(),
        hidden: false,
        is_summary: false,
        owner_pid: None,
        token_total: 0,
        token_prompt: 0,
        token_cache_read: 0,
        token_usage_estimated: false,
        revision: 0,
        journal_events: Vec::new(),
        context_messages: Vec::new(),
    }
}

pub(super) fn pop_test_paths(root: &std::path::Path) -> MiyuPaths {
    MiyuPaths {
        root_dir: root.to_path_buf(),
        config_dir: root.join("config"),
        config_file: root.join("config/config.jsonc"),
        skills_dir: root.join("config/skills"),
        data_dir: root.join("data"),
        cache_dir: root.join("cache"),
        state_dir: root.join("state"),
        pictures_dir: root.join("pictures"),
        fish_hook_file: root.join("fish/miyu.fish"),
        bash_hook_file: root.join("shell/bash-hook.sh"),
        zsh_hook_file: root.join("shell/zsh-hook.zsh"),
        scripts_dir: root.join("config/scripts"),
        system_scripts_dir: root.join("system/scripts"),
    }
}
