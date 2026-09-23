//! footer 上两个读数的来源纪律：Σ 别被旧读数盖回去，上下文没数过就说没数过。

use crate::cli::repl::jobs::{JobsFeed, SharedJobsFeed};
use crate::cli::*;
use std::sync::atomic::Ordering;
use std::sync::Arc;

fn totals(total: u64) -> TurnTokens {
    TurnTokens {
        total,
        prompt: total / 2,
        cache_read: total / 4,
    }
}

/// 用户 09-23：「取消之后它会先回到最开始的数然后再加上去」。回合跑着时轮询读到
/// 的是回合前的 Σ；footer 刷成新数之后，那份旧读数不能再拿来用。
#[test]
fn a_poll_read_taken_before_the_footer_refresh_is_ignored() {
    let shared = Arc::new(SharedJobsFeed::default());
    let feed = JobsFeed::Shared(shared.clone());
    *shared.cumulative.lock().unwrap() = Some((0, totals(1_000)));
    assert_eq!(feed.cumulative(), Some(totals(1_000)));

    // footer 被显式刷新（回合结束 / 取消）：之前开读的那份作废。
    shared.footer_generation.fetch_add(1, Ordering::AcqRel);
    assert_eq!(feed.cumulative(), None);

    // 刷新之后才开读的照常用。
    *shared.cumulative.lock().unwrap() = Some((1, totals(1_500)));
    assert_eq!(feed.cumulative(), Some(totals(1_500)));
}

/// 空闲时轮询只改界面上那份 footer；主循环整份覆盖之前得先收回来，不然屏上的 Σ
/// 被手里那份旧的盖回去。
#[test]
fn the_main_loop_keeps_the_cumulative_the_idle_poll_already_showed() {
    let config = AppConfig::default();
    let mut held = ReplFooterStatus::from_config(&config, 2_000, totals(1_000));
    let mut shown = held.clone();
    assert!(shown.update_cumulative_tokens(totals(1_500)));

    let adopted = held.adopt_cumulative(&shown);

    assert_eq!(adopted, totals(1_500));
    assert_eq!(held.token_usage.cumulative_tokens, Some(1_500));
    assert_eq!(held.token_usage.cumulative_prompt_tokens, 750);
    assert_eq!(held.token_usage.cumulative_cached_tokens, 375);
    // 上下文那半边不动：轮询不管它。
    assert_eq!(held.token_usage.session_tokens, 2_000);
}

/// 大厅里按 Tab 换到另一条车道、那边的会话还没开：上下文显示「—」，不拿 0 或另一条
/// 车道的数冒充，也不出百分比。数一到就恢复正常。
#[test]
fn an_uncounted_lane_shows_a_dash_for_its_context() {
    let config = AppConfig::default();
    let mut footer = ReplFooterStatus::from_config(&config, 2_000, TurnTokens::default());
    footer.update_context_window(Some(200_000), false);

    footer.mark_session_tokens_unknown();
    assert_eq!(footer.session_tokens(), None);
    let line =
        strip_terminal_control_sequences(&repl_footer_line(PersonaLane::Dev, false, &footer, 80));
    assert!(line.contains("—/200k"), "{line}");
    assert!(!line.contains('%'), "{line}");

    footer.update_session_tokens(3_100);
    assert_eq!(footer.session_tokens(), Some(3_100));
    let line =
        strip_terminal_control_sequences(&repl_footer_line(PersonaLane::Dev, false, &footer, 80));
    assert!(line.contains("3.1k/200k(1.6%)"), "{line}");
}
