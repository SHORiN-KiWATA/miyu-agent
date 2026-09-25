//! 回放按屏分页（会话项目第 2 段）：一页铺满一屏就停，游标指着更早的那些，
//! 一页页往前翻，每一轮都正好出现一次。

use super::shared::pop_test_paths;
use crate::cli::history_replay::replay_screen_page;
use crate::cli::*;

#[test]
fn a_replay_page_fills_one_screen_and_walks_back_through_every_turn() {
    let temp = tempfile::tempdir().unwrap();
    let paths = pop_test_paths(temp.path());
    let store = StateStore::new(&paths).unwrap();
    for index in 1..=6 {
        let turn_id = format!("t{index}");
        store
            .start_turn(&turn_id, &format!("问题 {index}"), std::process::id())
            .unwrap();
        let reply = (1..=4)
            .map(|line| format!("第 {index} 轮回复的第 {line} 行"))
            .collect::<Vec<_>>()
            .join("\n\n");
        store.complete_turn(&turn_id, &reply, None).unwrap();
    }
    let config = AppConfig::default();
    let page = |before| {
        replay_screen_page(
            &store,
            before,
            PersonaLane::Active,
            &config,
            (80, 12),
            false,
        )
        .unwrap()
        .expect("还有回合可回放")
    };

    let newest = page(None);
    let shown = String::from_utf8_lossy(&newest.frame).to_string();
    assert!(shown.contains("第 6 轮回复"), "最新那轮不在第一页");
    assert!(
        !shown.contains("第 1 轮回复"),
        "一屏装不下六轮，第一页不该有最早那轮"
    );

    let mut seen = vec![shown];
    let mut cursor = newest.older;
    while let Some(before) = cursor {
        let older = page(Some(before));
        seen.push(String::from_utf8_lossy(&older.frame).to_string());
        cursor = older.older;
        assert!(seen.len() <= 6, "翻不到头");
    }
    for index in 1..=6 {
        let hits = seen
            .iter()
            .filter(|page| page.contains(&format!("第 {index} 轮回复的第 1 行")))
            .count();
        assert_eq!(hits, 1, "第 {index} 轮出现了 {hits} 次");
    }
}
