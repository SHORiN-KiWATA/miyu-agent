//! 过程时间线:命令展开、预览行数、「准备xx」那一行这些「面板形态」的断言。子代理面板
//! 那几条 09-25 随浮层退役删了(子代理那一行点下去切进它的会话)。
//! 从 `timeline.rs` 拆出来(09-16,那份超过了文件规模基线);共用的夹具留在那边。

use super::timeline::{block_id_in, timeline_renderer, with_blocks};
use crate::render::t;

/// 主线上不到十分之一秒的步不报秒数：`· 0.0s` 只是噪音（用户实测）。
#[test]
fn a_quick_tool_step_does_not_report_zero_seconds() {
    with_blocks(|| {
        let mut renderer = timeline_renderer();
        renderer
            .write_tool_call("web_search", r#"{"query":"miyu 转轮"}"#)
            .unwrap();
        renderer
            .write_tool_result("web_search", true, "done")
            .unwrap();
        renderer.finalize_tools_summary().unwrap();
        let step = renderer
            .timeline_step_lines()
            .into_iter()
            .map(|line| crate::render::strip_ansi_text(&line))
            .find(|line| line.contains("miyu 转轮"))
            .expect("没有那一步");
        assert!(!step.contains("0.0s"), "报了个 0.0s: {step:?}");
        // 09-17 起不到一秒报毫秒：`0.0s` 那种没信息量的读数不该出现，但这一步
        // 确实花了时间，该有个真数字（原来是「什么都不报」，同一段代码两次跑
        // 时有时无，写不出稳定快照）。
        assert!(step.contains("ms"), "快步骤该报毫秒: {step:?}");
    });
}

#[test]
fn panel_speech_is_markdown_rendered() {
    let lines = crate::render::timeline::render_speech_lines(
        "**Phase 2** 与 `code` 完成\n\n- 一条\n- 两条",
        60,
    );
    let text = lines.join("\n");
    assert!(!text.contains("**"), "星号还裸着: {text:?}");
    assert!(text.contains("\x1b[1m"), "没有加粗样式: {text:?}");
    let plain = crate::render::strip_ansi_text(&text);
    assert!(
        plain.contains("Phase 2") && plain.contains("code"),
        "内容丢了: {plain:?}"
    );
    assert!(
        plain
            .lines()
            .filter(|line| line.contains("一条") || line.contains("两条"))
            .count()
            == 2,
        "列表项没了: {plain:?}"
    );
}

/// 面板里的正文按面板宽度渲染：代码块、表格都不能比面板宽，长行折进框里
///（用户实测截图：按整屏宽度排完再折进面板，是碎行和大片空白）。
#[test]
fn panel_speech_blocks_fit_the_panel_width() {
    let text = "```sh\nfor i in $(seq 1 120); do echo \"a very long command line that keeps going on and on\"; sleep 1; done\n```\n\n| Metric | Value |\n|---|---|\n| calls | 10 |\n";
    let lines = crate::render::timeline::render_speech_lines(text, 40);
    for line in &lines {
        let width = crate::render::command_ansi_width(line);
        assert!(width <= 40, "有一行比面板宽 ({width}): {line:?}");
    }
    let plain = lines
        .iter()
        .map(|line| crate::render::strip_ansi_text(line))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        plain.contains("sleep 1; done"),
        "代码长行被截掉了: {plain:?}"
    );
    assert!(
        plain.contains('┌') && plain.contains("calls"),
        "表格没画出来: {plain:?}"
    );
    // 渲染完把宽度还回去，别影响这条线程后面的渲染。
    assert_eq!(crate::render::cols_override(), 0);
}

/// Arch 那一家子的工具挂 Arch 的 Nerd Font 标（U+F08C7，用户指名），官方包、AUR、
/// Wiki、新闻一个样子。
#[test]
fn arch_family_tools_get_the_arch_logo() {
    if std::env::var_os("MIYU_TUI_ASCII").is_some() {
        return;
    }
    for name in [
        "aur",
        "archlinux_official_package_query",
        "archwiki_query",
        "archlinux_news",
        "install_aur_package",
        "review_aur_package",
    ] {
        assert_eq!(
            crate::render::tool_glyph_for(name),
            "\u{f08c7}",
            "{name} 没挂 Arch 的标"
        );
    }
    // 别的联网工具还是地球。
    assert_eq!(crate::render::tool_glyph_for("web_search"), "\u{f0ac}");
}

/// 「准备xx」那一行挂的是那个工具自己的图标：准备编辑=铅笔、准备执行=`$`，
/// 和它跑起来之后那一步一个样子（用户 09-14 要求）。
#[test]
fn a_preparing_row_wears_the_tools_own_glyph() {
    with_blocks(|| {
        let mut renderer = timeline_renderer();
        renderer.write_tool_preparing("edit", false).unwrap();
        let (glyph, text) = renderer.timeline_preparing_line().expect("没有准备那一行");
        assert_eq!(
            glyph,
            crate::render::tool_glyph_for("edit"),
            "准备编辑没挂铅笔"
        );
        assert!(text.contains(t("Preparing edit", "准备编辑")), "{text:?}");
    });
}

/// 参数每流一片就来一条准备事件：同一阶段的转轮不能每条都重起——重起就是在
/// 第 0、1 帧之间抖（用户实测：主体「准备xx」的转轮特别快、特别鬼畜）。
#[test]
fn repeated_preparing_events_do_not_restart_the_spinner() {
    with_blocks(|| {
        // 测试里 stdout 不是终端；报个宽度转轮才认自己在往终端画。
        crate::render::set_cols_override(100);
        let mut renderer = timeline_renderer();
        renderer.use_buffered_output();
        renderer.write_tool_preparing("edit", false).unwrap();
        let first = renderer.take_output_frame();
        assert!(!first.is_empty(), "第一条准备事件该把转轮画出来");
        for _ in 0..5 {
            renderer.write_tool_preparing("edit", false).unwrap();
        }
        let again = renderer.take_output_frame();
        assert!(
            again.is_empty(),
            "同一阶段的准备事件重画了转轮: {:?}",
            String::from_utf8_lossy(&again)
        );
        // 换了阶段（另一个工具开始流参数）才换文字，也不必重起。
        renderer.write_tool_preparing("run_command", false).unwrap();
        let (glyph, _) = renderer.timeline_preparing_line().expect("准备那一行");
        assert_eq!(glyph, crate::render::tool_glyph_for("run_command"));
        crate::render::set_cols_override(0);
    });
}

/// 全屏下的自动压缩：提示是时间线那种带图标的一行，摘要不往正文里流，压完收成
/// 一块 `› 上下文已压缩`，点开才是全文（用户：压缩上下文只有右上角的通知）。
#[test]
fn auto_compact_folds_its_summary_into_a_block_in_fullscreen() {
    with_blocks(|| {
        let mut renderer = timeline_renderer();
        renderer.use_buffered_output();
        renderer.write_system_message("正在压缩上下文...").unwrap();
        let notice = String::from_utf8_lossy(&renderer.take_output_frame()).into_owned();
        assert!(
            notice.contains(crate::render::timeline::glyph_notice())
                && notice.contains("正在压缩上下文"),
            "提示行没有图标: {notice:?}"
        );
        for piece in ["摘要第一段\n", "摘要第二段\n"] {
            renderer
                .write_compact_chunk(&miyu_core::llm::ChatStreamChunk {
                    kind: miyu_core::llm::ChatStreamKind::Content,
                    text: piece.to_string(),
                })
                .unwrap();
        }
        assert!(
            renderer.take_output_frame().is_empty(),
            "摘要流到正文里去了"
        );
        renderer.finish_compact().unwrap();
        let frame = String::from_utf8_lossy(&renderer.take_output_frame()).into_owned();
        let plain = crate::render::strip_ansi_text(&frame);
        assert!(
            plain.contains(&format!(
                "› {}",
                miyu_base::i18n::text("context compacted", "上下文已压缩")
            )),
            "没收成一块: {plain:?}"
        );
        assert!(!plain.contains("摘要第二段"), "摘要平铺出来了: {plain:?}");
        let id = block_id_in(&frame).expect("那一块没登记");
        let detail = crate::render::blocks::get(id)
            .unwrap_or_default()
            .join("\n");
        assert!(
            crate::render::strip_ansi_text(&detail).contains("摘要第二段"),
            "点开没有摘要: {detail:?}"
        );
    });
}
