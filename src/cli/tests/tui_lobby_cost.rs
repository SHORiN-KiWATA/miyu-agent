//! 大厅每一帧的成本量尺：拼一帧（星空、渐变、扫光）与按格子补丁各花多久、每帧写
//! 多少字节（09-23：按格子补丁把字节砍到三分之一，debug 下 CPU 却从 5.7% 涨到 8.4%；
//! 补丁改成按片段比之后 6.7%，补丁一帧 1.2ms → 0.4ms）。
//!
//! 不是断言，是尺子。用户跑的是 debug 二进制，按 debug 量才是他手上的感觉。渐变与
//! 扫光只在真彩下画，要带上 `COLORTERM`：
//!
//!     COLORTERM=truecolor cargo test -p miyu --lib -- cli::tests::tui_lobby_cost --ignored --nocapture

use crate::cli::repl::banner::BannerScene;
use crate::cli::repl::tail::screen::cells::patch_row;
use miyu_base::config::PersonaLane;
use std::time::{Duration, Instant};

#[test]
#[ignore]
fn lobby_frame_cost() {
    let mut scene = BannerScene::builtin_for_tests(PersonaLane::Active);
    let (cols, rows, frames) = (120, 40, 250_u32);
    let mut shown = scene.lobby(cols, rows, 5);
    let (mut compose, mut patch) = (Duration::ZERO, Duration::ZERO);
    let (mut changed, mut patch_bytes, mut row_bytes) = (0_usize, 0_usize, 0_usize);
    for _ in 0..frames {
        scene.tick();
        let started = Instant::now();
        let lobby = scene.lobby(cols, rows, 5);
        compose += started.elapsed();
        let started = Instant::now();
        for (y, row) in lobby.rows.iter().enumerate() {
            if shown.rows.get(y) == Some(row) {
                continue;
            }
            changed += 1;
            row_bytes += row.len();
            let old = shown.spans.get(y).map(Vec::as_slice).unwrap_or(&[]);
            patch_bytes += patch_row(old, &lobby.spans[y], y as u16).len();
        }
        patch += started.elapsed();
        shown = lobby;
    }
    let per_frame = |total: Duration| total.as_secs_f64() * 1000.0 / f64::from(frames);
    eprintln!(
        "拼帧 {:.2}ms  补丁 {:.2}ms  每帧变 {} 行  补丁 {}B（整行重写 {}B）",
        per_frame(compose),
        per_frame(patch),
        changed / frames as usize,
        patch_bytes / frames as usize,
        row_bytes / frames as usize,
    );
}
