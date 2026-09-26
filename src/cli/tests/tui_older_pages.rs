//! 全屏往前翻页（会话项目第 2 段）：翻到顶从库里补更早的一页，接在最前面，
//! 看到的内容原地不动；块、轮标记跟着排好，`/undo` 照样只截最后一轮。

use super::tui_blocks::with_blocks;
use crate::cli::history_replay::ReplayScreenPage;
use crate::cli::repl::tail::screen::{OlderPages, Screen};
use miyu_hosts::render::blocks;

fn turn(id: u64, prompt: &str, reply: &str) -> String {
    format!(
        "{}\r\n  {prompt}\r\n{}  ✳ 已思考{}\r\n{reply}\r\n",
        blocks::TURN_START_MARKER,
        blocks::begin_marker(id),
        blocks::END_MARKER
    )
}

fn text(screen: &Screen) -> String {
    (0..screen.view_len())
        .map(|index| {
            screen
                .view_row(index)
                .iter()
                .map(|span| span.text.clone())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn scrolling_to_the_top_prepends_the_older_page_in_place() {
    with_blocks(|| {
        let mut screen = Screen::detached(80, 10);
        let newer = blocks::register(vec!["  又想了一下".into()]).unwrap();
        screen.feed_for_test(turn(newer, "第二句", "第二句的回复").as_bytes());
        let older_block = blocks::register(vec!["  想了一下".into()]).unwrap();
        let older_frame = turn(older_block, "第一句", "第一句的回复").into_bytes();
        let asked = std::rc::Rc::new(std::cell::Cell::new(None));
        let asked_in_loader = asked.clone();
        screen.set_older_pages(Some(OlderPages::new(
            7,
            Box::new(move |before| {
                asked_in_loader.set(Some(before));
                Ok(Some(ReplayScreenPage {
                    frame: older_frame.clone(),
                    older: None,
                }))
            }),
        )));
        let view_before = screen.view_len();

        assert!(screen.load_older_at_top(), "没往前补");

        assert_eq!(asked.get(), Some(7), "游标没交给取页的");
        let after = text(&screen);
        let first = after.find("第一句的回复").expect("更早那页没进来");
        let second = after.find("第二句的回复").expect("原来的内容没了");
        assert!(first < second, "更早的应该在上面: {after}");
        assert_eq!(
            screen.scroll_for_test(),
            screen.view_len() - view_before,
            "补进来之后看到的内容挪位了"
        );
        assert_eq!(screen.block_count(), 2, "补进来那页的块没接上");

        // 轮标记按先后排好：撤销照样只截最后一轮。
        assert!(screen.truncate_last_turn());
        let undone = text(&screen);
        assert!(undone.contains("第一句的回复"), "{undone}");
        assert!(!undone.contains("第二句"), "{undone}");

        // 没有更早的了：再翻到顶也不再补。
        screen.scroll_by(-1_000);
        assert!(!screen.load_older_at_top());
    });
}

/// 视口没顶到最上面就不补：补页只在「翻到顶」那一下发生。
#[test]
fn nothing_is_loaded_until_the_viewport_reaches_the_top() {
    with_blocks(|| {
        let mut screen = Screen::detached(80, 4);
        let body = (1..=20)
            .map(|index| format!("第 {index} 行\r\n"))
            .collect::<String>();
        screen.feed_for_test(body.as_bytes());
        screen.scroll_by(1_000);
        assert!(screen.scroll_for_test() > 0);
        screen.set_older_pages(Some(OlderPages::new(
            1,
            Box::new(|_| panic!("没翻到顶就去库里取了")),
        )));
        assert!(!screen.load_older_at_top());
    });
}
