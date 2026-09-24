//! CLI 层的测试，按被测主题分文件。
//!
//! 原本挤在一个叫 `repl_input_tests` 的两千多行模块里，而里面测的其实是
//! 参数解析、日志格式化、footer、活动区、菜单——名字名不副实。
mod cli_args;
mod daemon_log;
mod footer_cumulative;
mod footer_tail;
mod footer_width;
mod golden_panel;
mod hangup;
mod herdr_notify;
mod input_editing;
mod jobs_feed;
mod lobby_anim;
mod lobby_layout;
mod pop_menu;
mod replay_pages;
mod sandbox_view;
mod shared;
mod slash_commands;
mod static_timeline;
mod strip_sessions;
mod tui_ansi;
mod tui_blocks;
mod tui_live_anchor;
mod tui_live_cost;
mod tui_lobby_cost;
mod tui_older_pages;
mod tui_perf;
mod tui_reflow;
mod tui_scroll;
mod tui_undo;
mod variant_menu;
mod wait_spinner_cursor;
