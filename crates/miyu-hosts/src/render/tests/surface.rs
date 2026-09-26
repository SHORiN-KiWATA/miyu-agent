//! 能力位是怎么算出来的：穷举模式位的每一种组合，逐位比对。
//!
//! 阶段 2 这里比的是「能力位 == 旧谓词」，因为那一步只改名。09-17 之后**旧谓词
//! 没了**：`timeline_static()` 不再看显示档位，`caps()` 也不再看。现在比的是
//! 那个更小的模型——
//!
//! - `expandable` = 有没有登记处（全屏）
//! - `commit_immediately` = 点不开的时间线，或者关了「收起成 Worked for」
//! - `fold` = 有时间线且不是逐步落地
//! - `detail` = 能点开就收在块后面，点不开的时间线就地印
//!
//! **显示档位（展开思考 / 展开工具）一位都不参与。** 它只决定内容默认看不看得
//! 见，不决定走哪条路——那正是用户 09-17 让拆掉的耦合。

use super::timeline::with_blocks;
use crate::render::stream::surface::DetailPlacement;
use crate::render::{ReasoningDisplayMode, StreamRenderer, ToolCallDisplayMode};

fn renderer(plain: bool, live_summary: bool, tool_calls: ToolCallDisplayMode) -> StreamRenderer {
    let mut renderer =
        StreamRenderer::new(ReasoningDisplayMode::Summary, tool_calls, plain, true, 8);
    renderer.use_buffered_output();
    renderer.live_summary = live_summary;
    renderer
}

/// 2 × 2 × 2 × 3 = 24 组。每组都比四位。
#[test]
fn surface_caps_follow_only_expandable_and_folding() {
    for blocks_on in [false, true] {
        let check = || {
            for plain in [false, true] {
                for live_summary in [false, true] {
                    for tool_calls in [
                        ToolCallDisplayMode::Hidden,
                        ToolCallDisplayMode::Summary,
                        ToolCallDisplayMode::Full,
                    ] {
                        let surface = renderer(plain, live_summary, tool_calls);
                        let caps = surface.caps();
                        let label = format!(
                            "blocks={blocks_on} plain={plain} live={live_summary} tools={tool_calls:?}"
                        );
                        assert_eq!(
                            caps.expandable,
                            crate::render::blocks::enabled(),
                            "expandable 与 blocks::enabled() 不符: {label}"
                        );
                        assert_eq!(
                            caps.commit_immediately,
                            surface.timeline_static(),
                            "收起成 Worked for 开着时，只有点不开的时间线才逐步落地: {label}"
                        );
                        assert_eq!(
                            caps.fold,
                            surface.timeline_enabled() && !caps.commit_immediately,
                            "fold 与 `有时间线且不就地落地` 不符: {label}"
                        );
                        assert_eq!(
                            caps.detail == DetailPlacement::Inline,
                            surface.timeline_static(),
                            "详情摆哪只看能不能点开: {label}"
                        );
                        // 档位换一遍，四位一个都不许动——思考以后要从时间线里
                        // 搬出去，这三件事之间不能有耦合（用户 09-17）。
                        let mut expanded = renderer(plain, live_summary, tool_calls);
                        expanded.reasoning_mode = ReasoningDisplayMode::Full;
                        assert_eq!(expanded.caps(), caps, "档位不该改变这个面: {label}");
                    }
                }
            }
        };
        if blocks_on {
            with_blocks(check);
        } else {
            check();
        }
    }
}

/// 三个值得钉的面各自长什么样（`docs/plan/2026-09-17-render-unification.md` §1.1）。
/// 这张表是「改名不改行为」的可读版：谁该收缩、谁该就地落地，一眼看得出。
#[test]
fn the_three_surfaces_have_the_shapes_we_expect() {
    // S1 管道:没有时间线,什么都不收不折。
    let pipe = renderer(false, false, ToolCallDisplayMode::Summary).caps();
    assert!(!pipe.expandable && !pipe.commit_immediately && !pipe.fold);

    // S3 静态时间线:每步立刻落地、详情就地印、没有 `Worked for`。
    // **档位不影响它是哪个面**：`Full` 档原来会把这条线整个换成旧卡片面（S2），
    // 那是 09-17 收掉的最后一处耦合。
    for tool_calls in [ToolCallDisplayMode::Summary, ToolCallDisplayMode::Full] {
        let caps = renderer(false, true, tool_calls).caps();
        assert!(caps.commit_immediately && !caps.fold && caps.detail_inline());
        assert!(
            renderer(false, true, tool_calls).timeline_enabled(),
            "非全屏 + {tool_calls:?} 该照样有时间线"
        );
    }
    let static_caps = renderer(false, true, ToolCallDisplayMode::Summary).caps();
    assert!(static_caps.commit_immediately, "静态版该立刻落地");
    assert!(!static_caps.fold, "静态版不该有收缩行");
    assert!(static_caps.detail_inline(), "静态版详情该就地印");
    assert!(!static_caps.expandable, "静态版没有登记处");

    // S4 全屏:能点开、攒着收段。
    with_blocks(|| {
        let full = renderer(false, true, ToolCallDisplayMode::Summary).caps();
        assert!(full.expandable, "全屏该能点开");
        assert!(full.fold, "全屏该收成 Worked for");
        assert!(!full.commit_immediately, "全屏不该逐步落地");
        assert!(!full.detail_inline(), "全屏详情该在块后面");
    });
}

/// 「TUI 不自动收起」开了之后:每一步就地落下去、段末没有 `Worked for …`
/// ——和 shell 无缝对话那条路一个样子(用户 todolist:21)。
///
/// 它**只管收不收段**：那些步照样挂块、照样点得开，详情照样收在块后面。
/// 09-17 之前 `commit_static_steps` 顺手把它们变成点不开、正文铺一地
///（`s4-full-open.ansi` 只有 5 个标记，`s4-full.ansi` 有 18 个）——用户原话：
///「即使不自动收起过程为 true，也不应该以 tag 行下预览的形式出现 tag 行的内容」。
/// 字节那一层由 `golden.rs` 的 `fullscreen_with_the_timeline_kept_open_is_frozen`
/// 钉着。
#[test]
fn keeping_the_timeline_open_stops_folding() {
    with_blocks(|| {
        let mut renderer = renderer(false, true, ToolCallDisplayMode::Summary);
        renderer.fold_timeline = false;
        let caps = renderer.caps();
        assert!(caps.expandable, "开着也该能点开");
        assert!(!caps.fold, "开着就不该再收成 Worked for");
        assert!(caps.commit_immediately, "开着该每一步就地落下去");
        assert!(
            !caps.detail_inline(),
            "能点开的面详情仍该收在块后面,不是就地铺开"
        );
    });
}
