//! 回合中途逐请求刷新的 Σ（09-26）：取 daemon 报的会话累计，不在基线上再加一遍本回合；
//! 跑着的前台子代理那份加数照当下的算。真机走查见 `testkit/tui/strip_tree.py` 的 Σ 两项。

use super::shared::detached_tail;
use crate::cli::*;

fn tokens(total: u64) -> TurnTokens {
    TurnTokens {
        total,
        prompt: total,
        cache_read: 0,
    }
}

fn shown_total(live: &LiveReplTail) -> u64 {
    live.footer.token_usage.cumulative_tokens.unwrap_or(0)
        + live.footer.token_usage.live_extra_tokens
}

/// 切进一条回合正跑着的会话：快照里的 Σ 是 daemon 的实时数，本回合至今的 450 已经在里面。
/// 回放跟上来的每次请求再报「本回合 450、会话 450」——Σ 该是 450。原来在快照上再加一遍
/// 本回合，跟着看的那一阵是 900，回合一停又掉回 750（09-26 走查）。
#[test]
fn attaching_to_a_running_turn_does_not_count_it_twice() {
    let mut live = detached_tail();
    live.footer.token_usage.cumulative_tokens = Some(450);
    live.footer.token_usage.cumulative_prompt_tokens = 450;

    for turn in [150, 300, 450] {
        live.refresh_round_usage(900, tokens(turn), tokens(turn), GenerationSpeed::default())
            .unwrap();
    }

    assert_eq!(live.footer.token_usage.cumulative_tokens, Some(450));
    assert_eq!(live.footer.token_usage.cumulative_prompt_tokens, 450);
    assert_eq!(live.footer.token_usage.turn_tokens, 450);
}

/// 自己起的一轮：基线是回合前的 1000，请求报的会话累计 = 1000 + 本回合至今。
#[test]
fn an_own_turn_follows_the_reported_session_total() {
    let mut live = detached_tail();
    live.footer.token_usage.cumulative_tokens = Some(1000);

    live.refresh_round_usage(1200, tokens(200), tokens(1200), GenerationSpeed::default())
        .unwrap();
    assert_eq!(live.footer.token_usage.cumulative_tokens, Some(1200));

    // 老 daemon 不报会话累计：照旧在基线上叠加本回合。
    live.refresh_round_usage(
        1500,
        tokens(500),
        TurnTokens::default(),
        GenerationSpeed::default(),
    )
    .unwrap();
    assert_eq!(live.footer.token_usage.cumulative_tokens, Some(1500));
}

/// 每次请求结束刷新计量时，跑着的前台子代理那份加数是**现在**的：原来它跟着基线快照退回
/// 拍基线那一刻的数，Σ 往下闪一下，要等子代理下一次报数才补回来。
#[test]
fn a_round_refresh_keeps_the_running_subagents_share() {
    let mut live = detached_tail();
    live.footer.token_usage.cumulative_tokens = Some(1000);
    live.refresh_round_usage(1100, tokens(100), tokens(1100), GenerationSpeed::default())
        .unwrap();
    live.set_live_turn_tokens(300);
    assert_eq!(shown_total(&live), 1400);

    live.refresh_round_usage(1200, tokens(200), tokens(1200), GenerationSpeed::default())
        .unwrap();
    assert_eq!(live.footer.token_usage.live_extra_tokens, 300);
    assert_eq!(shown_total(&live), 1500);
}
