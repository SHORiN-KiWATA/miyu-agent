//! daemon 记的事件时刻换算成本机 `Instant`（会话项目第 3 段收尾，09-25）。补发的一轮按事件
//! 自己的时刻掐表，见 `render::stream::event_clock`。

use crate::cli::repl::live_turn::event_instant;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

#[test]
fn an_old_event_maps_to_an_instant_that_long_ago() {
    let at = event_instant(Some(now_ms() - 5_000)).expect("带了时刻就换算");
    let age = at.elapsed();
    assert!(
        age >= Duration::from_millis(4_900) && age < Duration::from_millis(6_000),
        "{age:?}"
    );
}

/// 没带时刻（老 daemon、不是回合事件）就不设时钟，渲染器照旧按收到的那一刻。
#[test]
fn no_time_means_no_clock() {
    assert!(event_instant(None).is_none());
}

/// 墙上时钟被往回拨过、事件时刻比现在还晚：当成现在，别算出负的耗时。
#[test]
fn a_time_from_the_future_counts_as_now() {
    let before = Instant::now();
    let at = event_instant(Some(now_ms() + 60_000)).unwrap();
    assert!(at >= before && at.elapsed() < Duration::from_secs(1));
}
