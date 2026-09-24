//! 过程时间线的文案形状。
//!
//! 这些断言看着琐碎，但它们是用户唯一能看见的东西：秒数的量级、窥视截到哪里、
//! 收缩那一行怎么措辞。改动它们就是改动界面，所以钉死。

use crate::render::stream::timeline::{
    format_seconds, peek_tail, summary_line, undecorate, Counts,
};
use crate::render::t;
use std::time::Duration;

#[test]
fn seconds_change_precision_with_magnitude() {
    // 不到一秒报毫秒:`0.3s` 这种读数把「快不快」压没了,`340ms` 才是真数字
    // (用户 09-17)。
    assert_eq!(format_seconds(Duration::from_millis(340)), "340ms");
    assert_eq!(format_seconds(Duration::from_micros(400)), "<1ms");
    // 一秒起按秒,给一位小数
    assert_eq!(format_seconds(Duration::from_millis(2_450)), "2.5s");
    // 十秒以上小数没意义
    assert_eq!(format_seconds(Duration::from_millis(12_400)), "12s");
    // 进了分钟换成 m/s
    assert_eq!(format_seconds(Duration::from_secs(75)), "1m 15s");
}

fn counts(commands: usize, edits: usize, tools: usize, thoughts: usize, errors: usize) -> Counts {
    Counts {
        commands,
        edits,
        tools,
        thoughts,
        errors,
    }
}

/// 09-24 用户拍板的收缩行写法:跑过命令就 `Ran` 打头、耗时挂末尾;没跑命令用
/// `Worked for` 打头;动作在前、思考在后、报错垫底;为零的项不写。
#[test]
fn summary_leads_with_commands_then_edits_tools_thoughts() {
    let twelve = Duration::from_millis(12_300);
    assert_eq!(
        summary_line(twelve, counts(3, 2, 2, 2, 1)),
        "Ran 3 commands · 2 edits · 2 tools · 2 thoughts · 1 err · 12s"
    );
    assert_eq!(
        summary_line(Duration::from_millis(2_500), counts(1, 0, 0, 0, 0)),
        "Ran 1 command · 2.5s"
    );
    // 没跑命令:Worked for 打头,edits 与 tools 分开数。
    assert_eq!(
        summary_line(twelve, counts(0, 2, 1, 0, 0)),
        "Worked for 12s · 2 edits · 1 tool"
    );
    assert_eq!(
        summary_line(twelve, counts(0, 0, 3, 1, 0)),
        "Worked for 12s · 3 tools · 1 thought"
    );
    // 只想了想:不写 `0 tools`,也不写思考次数。
    assert_eq!(
        summary_line(Duration::from_millis(400), counts(0, 0, 0, 2, 0)),
        "Thought for 400ms"
    );
}

/// 回放历史时没有计时。报 `Worked for 0.0s` 会让人以为"这一轮瞬间就完了"，
/// 不如干脆不报时间;`Ran` 打头的那种也就不挂末尾的秒数。
#[test]
fn summary_without_timing_drops_the_duration() {
    assert_eq!(
        summary_line(Duration::ZERO, counts(3, 2, 0, 1, 0)),
        "Ran 3 commands · 2 edits · 1 thought"
    );
    assert_eq!(
        summary_line(Duration::ZERO, counts(0, 0, 2, 1, 0)),
        "2 tools · 1 thought"
    );
    assert_eq!(
        summary_line(Duration::ZERO, counts(0, 0, 0, 2, 0)),
        "2 thoughts"
    );
    // 什么都没有时也得说句人话，不能给个空串
    assert!(!summary_line(Duration::ZERO, Counts::default()).is_empty());
}

#[test]
fn a_run_without_timing_reports_what_it_did_not_zero_seconds() {
    // 回放没有计时；那条路上时间线还是会现场掐一次表，量出来是几十微秒。
    // 打印成 `Worked for 0.0s` 看着像"这一轮瞬间就完了"，不如不报。
    assert_eq!(
        summary_line(Duration::from_micros(40), counts(0, 0, 1, 2, 0)),
        "1 tool · 2 thoughts"
    );
    assert_eq!(
        summary_line(Duration::from_millis(2_500), counts(0, 0, 1, 2, 0)),
        "Worked for 2.5s · 1 tool · 2 thoughts"
    );
}

/// 分类按工具名:命令只认 run_command / 中转线的 Bash;edits 只数改磁盘文件的
/// (含中转线三家的写法);知识库、artifact、删文件和其余一切算 tools。
#[test]
fn tools_are_counted_by_kind() {
    let mut tally = Counts::default();
    for (name, failed) in [
        ("run_command", false),
        ("Bash", true),
        ("edit", false),
        ("Edit", false),
        ("write_to_file", false),
        ("kb", false),
        ("artifact", false),
        ("trash_path", false),
        ("read", false),
        ("subagent:查资料", false),
    ] {
        tally.record_tool(name, failed);
    }
    assert_eq!(
        (tally.commands, tally.edits, tally.tools, tally.errors),
        (2, 3, 5, 1)
    );
}

#[test]
fn peek_takes_the_tail_and_marks_the_cut() {
    // 放得下就整段给,不加省略号
    assert_eq!(peek_tail("短句", 20), "短句");
    // 放不下取**末尾**——想到哪儿了比想过什么更有用
    let peek = peek_tail("一二三四五六七八九十", 8);
    assert!(peek.starts_with('…'), "截断了要有记号: {peek}");
    assert!(peek.ends_with("九十"), "取的该是末尾: {peek}");
    // 换行和多余空白压成一行,不然会把 live 区顶开
    assert_eq!(peek_tail("上\n  下", 20), "上 下");
    assert_eq!(peek_tail("", 20), "");
    assert_eq!(peek_tail("随便什么", 0), "");
}

#[test]
fn expanded_detail_drops_the_inline_decorations() {
    // 时间线已经用连线说明了从属关系，`↳` / `│` 是同一件事说第二遍，
    // 而且两套缩进对不齐（用户：「没必要有那个箭头和竖线」）。
    let lines = undecorate(vec![
        "  ↳ ls -la".to_string(),
        "  │ total 4".to_string(),
        "  普通一行".to_string(),
    ]);
    assert_eq!(
        lines,
        vec![
            "  ls -la".to_string(),
            "  total 4".to_string(),
            "  普通一行".to_string()
        ]
    );
    // 行首的颜色留着，只摘那一个记号
    let colored = undecorate(vec!["\x1b[2m  ↳ 带色的\x1b[0m".to_string()]);
    assert_eq!(colored, vec!["\x1b[2m  带色的\x1b[0m".to_string()]);
}

/// 测试之间共用同一个进程级开关，串行跑免得互相掀桌子。
pub(super) fn with_blocks<T>(body: impl FnOnce() -> T) -> T {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    crate::render::blocks::set_enabled(true);
    let out = body();
    crate::render::blocks::set_enabled(false);
    out
}

pub(super) fn timeline_renderer() -> crate::render::StreamRenderer {
    timeline_renderer_with_preview_rows(10)
}

/// `display.command_output_lines`:命令跑着/跑完时抬头底下露几行输出。
pub(super) fn timeline_renderer_with_preview_rows(rows: usize) -> crate::render::StreamRenderer {
    let mut renderer = crate::render::StreamRenderer::new(
        crate::render::ReasoningDisplayMode::Summary,
        crate::render::ToolCallDisplayMode::Summary,
        false,
        true,
        rows,
    );
    renderer.live_summary = false;
    renderer
}

/// 一行里挂着的那一块的 id（行首的私有 OSC 标记）。
pub(super) fn block_id_in(line: &str) -> Option<u64> {
    let rest = line.split_once("\x1b]1337;miyu-block=")?.1;
    rest.split_once('\u{7}')?.0.parse().ok()
}

/// 主线上「想」的那一行是暗的，不是绿的。
///
/// 绿色留给展开出来的思考正文。两边都绿等于没有区分，而抬头绿、正文白又把轻重
/// 说反了——用户先要求过改回去（「主体的思考行的颜色还是换回去吧」）。
#[test]
fn the_thinking_row_itself_is_not_green() {
    with_blocks(|| {
        let mut renderer = timeline_renderer();
        renderer.reasoning_text = "先看一眼再说".into();
        renderer.timeline_push_thought().unwrap();
        let line = renderer
            .timeline_step_lines()
            .into_iter()
            .find(|line| crate::render::strip_ansi_text(line).contains(t("thought", "已思考")))
            .expect("没有想的那一步");
        assert!(!line.contains("38;5;10"), "想的那一行还是绿的: {line:?}");
    });
}

/// 子代理面板的第一步是「差事」，点开是派它出去时给的全文。
///
/// 面板里原来全是它自己的动作，唯独没有"它被要求干什么"——而那件事只有派它
/// 出去的那一轮知道（用户：子代理的开头应该是 prompt 工具行）。
#[test]
fn a_subagent_panel_opens_with_the_task_it_was_given() {
    with_blocks(|| {
        let mut renderer = timeline_renderer();
        renderer
            .write_tool_call(
                "subagent",
                r#"{"description":"查目录","prompt":"把 README 里的错别字挑出来"}"#,
            )
            .unwrap();
        let id = renderer
            .subagent_overlay_id("subagent")
            .expect("子代理那块没登记");
        let lines = crate::render::blocks::get(id).expect("块没了");
        let first = lines.first().cloned().unwrap_or_default();
        let first_text = crate::render::strip_ansi_text(&first);
        assert!(
            first_text.contains(t("prompt", "提示词")),
            "面板第一步不是差事: {lines:?}"
        );
        // 抬头只给个开头，全文点开才看——所以它在那一步自己的块里。
        let nested = lines
            .iter()
            .filter_map(|line| block_id_in(line))
            .filter_map(crate::render::blocks::get)
            .flatten()
            .map(|line| crate::render::strip_ansi_text(&line))
            .collect::<Vec<_>>();
        assert!(
            nested.iter().any(|line| line.contains("错别字")),
            "差事的全文没挂进去: {nested:?}"
        );
    });
}

/// 并排跑的工具，每一行挂**各自**那一块。
///
/// 共用一个 id 的话，展开层会把同一块内容插好几遍——行号、偏移、点击命中全跟着
/// 错位，表现出来就是"所有工具行都点不开了"（用户实测）。
#[test]
fn parallel_running_tools_each_get_their_own_block() {
    with_blocks(|| {
        let mut renderer = timeline_renderer();
        renderer
            .write_tool_call("web_search", r#"{"query":"第一个"}"#)
            .unwrap();
        renderer
            .write_tool_call("web_fetch", r#"{"url":"https://example.com"}"#)
            .unwrap();
        renderer.refresh_live_block();
        let rows = renderer.timeline_running_tool_lines();
        assert_eq!(rows.len(), 2, "两个工具没各占一行: {rows:?}");
        let ids = rows.iter().filter_map(|row| row.target).collect::<Vec<_>>();
        assert_eq!(ids.len(), 2, "有工具行没挂块，点开就是死的: {rows:?}");
        assert_ne!(ids[0], ids[1], "两行共用同一块: {rows:?}");
        for id in ids {
            let lines = crate::render::blocks::get(id).expect("块没了");
            assert!(
                lines.iter().any(|line| !line.trim().is_empty()),
                "块是空的，点开等于没点: {lines:?}"
            );
        }
    });
}

/// 跑着的子代理那一行点开的是**面板**，不是就地展开。
#[test]
fn a_running_subagent_row_points_at_its_panel() {
    with_blocks(|| {
        let mut renderer = timeline_renderer();
        renderer
            .write_tool_call("subagent", r#"{"description":"查目录","prompt":"去看看"}"#)
            .unwrap();
        renderer.refresh_live_block();
        let rows = renderer.timeline_running_tool_lines();
        let target = &rows.first().expect("子代理那一行没了").target;
        assert_eq!(
            *target,
            renderer.subagent_overlay_id("subagent"),
            "子代理那一行没指向它的面板: {rows:?}"
        );
        assert!(target.is_some(), "子代理那一行没挂东西，点了没反应");
    });
}

/// 回放：有工具的那一轮，思考那一步也要在。
///
/// 收缩行上要数得出来（`1 tool · 1 thought`），点开那一块里也要有那一步。
#[test]
fn replay_keeps_the_thought_when_the_turn_also_ran_tools() {
    with_blocks(|| {
        let mut renderer = timeline_renderer();
        renderer.use_external_cursor_control();
        renderer.use_buffered_output();
        renderer
            .write_chunk(miyu_core::llm::ChatStreamChunk {
                kind: miyu_core::llm::ChatStreamKind::Reasoning,
                text: "先想一下".into(),
            })
            .unwrap();
        renderer
            .write_tool_call("run_command", r#"{"command":"ls"}"#)
            .unwrap();
        renderer
            .write_tool_result("run_command", true, "out")
            .unwrap();
        renderer
            .write_chunk(miyu_core::llm::ChatStreamChunk {
                kind: miyu_core::llm::ChatStreamKind::Content,
                text: "好了".into(),
            })
            .unwrap();
        renderer.finish().unwrap();
        let frame = String::from_utf8_lossy(&renderer.take_output_frame()).to_string();
        assert!(frame.contains("thought"), "收缩行没数到思考: {frame}");
        let steps = frame
            .lines()
            .filter_map(block_id_in)
            .filter_map(crate::render::blocks::get)
            .flatten()
            .map(|line| crate::render::strip_ansi_text(&line))
            .collect::<Vec<_>>();
        assert!(
            steps
                .iter()
                .any(|line| line.contains(t("thought", "已思考"))),
            "点开之后没有思考那一步: {steps:?}"
        );
    });
}

/// 子代理面板每刷新一次都新登记一批块 = 把登记处刷爆。
///
/// 一段思考是**一小段一小段**来的，这条路一秒要走好几次。新登记的话登记处几秒
/// 就满，而淘汰会先端掉最久没碰过的那些——这个子代理自己那块覆盖层登记得最早，
/// 正是第一个受害者：面板于是不再刷新、行也点不开了（用户实测）。
#[test]
fn a_subagent_panel_reuses_its_step_blocks_across_refreshes() {
    with_blocks(|| {
        let mut renderer = timeline_renderer();
        renderer
            .write_tool_call("subagent", r#"{"description":"查目录","prompt":"去看看"}"#)
            .unwrap();
        renderer.subagent_thought("subagent", "先想一下，");
        let id = renderer.subagent_overlay_id("subagent").expect("没登记");
        let ids_of = |renderer: &crate::render::StreamRenderer| {
            let _ = renderer;
            crate::render::blocks::get(id)
                .unwrap_or_default()
                .iter()
                .filter_map(|line| block_id_in(line))
                .collect::<Vec<_>>()
        };
        let before = ids_of(&renderer);
        assert!(!before.is_empty(), "面板里一步都没有可点开的块");
        for _ in 0..20 {
            renderer.subagent_thought("subagent", "再想一点，");
        }
        let after = ids_of(&renderer);
        assert_eq!(before, after, "每刷新一次就换一批块 id");
        // 覆盖层自己那块还得在——它是被淘汰算法第一个盯上的那个。
        assert!(
            crate::render::blocks::get(id).is_some(),
            "子代理自己那块覆盖层被端掉了"
        );
    });
}

/// 跑着的那一行要裁到屏宽。
///
/// 窥视是子代理内层的思考末尾，长度不受这一行控制；不裁的话它能把行顶出屏幕，
/// 缓冲把它折成两行，块的起止就跨了行——点上去命中不到，整行变成死的。
#[test]
fn a_running_row_is_clipped_to_the_screen() {
    with_blocks(|| {
        let mut renderer = timeline_renderer();
        renderer
            .write_tool_call(
                "subagent",
                r#"{"description":"一个很长很长很长很长的描述占满一截","prompt":"去看看"}"#,
            )
            .unwrap();
        renderer.subagent_thought("subagent", &"想得很长".repeat(80));
        renderer.refresh_live_block();
        let width = crate::render::command_terminal_width();
        for crate::render::timeline::LiveRow { line, .. } in renderer.timeline_running_tool_lines()
        {
            assert!(
                crate::render::visible_width(&line) <= width,
                "这一行没裁，会被折行: {} 列 / 屏宽 {width}",
                crate::render::visible_width(&line)
            );
        }
    });
}

/// 抬头和窥视之间用 `·` 分开，和「名字 · 秒数」那半截一个写法。
#[test]
fn the_peek_is_separated_by_a_dot() {
    with_blocks(|| {
        let mut renderer = timeline_renderer();
        renderer
            .write_tool_call(
                "subagent",
                r#"{"description":"查目录","prompt":"去看看目录里有什么"}"#,
            )
            .unwrap();
        let id = renderer.subagent_overlay_id("subagent").expect("没登记");
        let first = crate::render::blocks::get(id)
            .unwrap_or_default()
            .first()
            .cloned()
            .unwrap_or_default();
        let text = crate::render::strip_ansi_text(&first);
        assert!(
            text.contains(&format!(
                "{}{}",
                t("prompt", "提示词"),
                crate::render::timeline::PEEK_SEP
            )),
            "差事那一行没用 `·` 分隔: {text:?}"
        );
    });
}

/// 子代理面板里每一步都得在框里，一步就是一行。
///
/// 面板比整屏窄六列；按整屏宽排的话，那些行进面板要折成两行——一步占两行，
/// 时间线的竖线跟着对不上列。
#[test]
fn subagent_panel_rows_fit_the_panel() {
    with_blocks(|| {
        let mut renderer = timeline_renderer();
        renderer
            .write_tool_call(
                "subagent",
                r#"{"description":"查目录","prompt":"把 README 里所有的错别字挑出来，逐条列清楚，别漏"}"#,
            )
            .unwrap();
        renderer.subagent_thought("subagent", &"想得很长很长".repeat(60));
        renderer.subagent_tool(
            "subagent",
            "run_command",
            "运行命令",
            &format!("{{\"command\":\"{}\"}}", "ls -la /very/long/path".repeat(8)),
            true,
            "输出",
        );
        let id = renderer.subagent_overlay_id("subagent").expect("没登记");
        // 面板里能写多宽：屏幕宽减掉左右各两列留白（没有竖线）。
        // `command_terminal_width()` 本身就是"屏幕宽减四"，正好是它。
        let inner = crate::render::command_terminal_width();
        for line in crate::render::blocks::get(id).unwrap_or_default() {
            let width = crate::render::visible_width(&line);
            assert!(
                width <= inner,
                "这一行进面板要折行: {width} 列 / 面板 {inner}: {:?}",
                crate::render::strip_ansi_text(&line)
            );
        }
    });
}

/// 回放要把 `Worked for …` 算回来。
///
/// 回放是一瞬间喂完的，墙上时间是零——那一截于是整个消失，重开之后只剩
/// `1 tool · 2 thoughts`（用户实测对比图）。每一步自己带着耗时，累加起来
/// 就是这一段的下限。
#[test]
fn a_replayed_segment_still_says_how_long_it_took() {
    with_blocks(|| {
        let mut renderer = timeline_renderer();
        renderer.use_external_cursor_control();
        renderer.use_buffered_output();
        renderer
            .write_chunk(miyu_core::llm::ChatStreamChunk {
                kind: miyu_core::llm::ChatStreamKind::Reasoning,
                text: "先想一下".into(),
            })
            .unwrap();
        renderer.replay_reasoning_elapsed(Duration::from_millis(2_400));
        renderer
            .write_tool_call("run_command", r#"{"command":"ls"}"#)
            .unwrap();
        renderer.replay_tool_elapsed("run_command", Duration::from_millis(1_200));
        renderer
            .write_tool_result("run_command", true, "out")
            .unwrap();
        renderer
            .write_chunk(miyu_core::llm::ChatStreamChunk {
                kind: miyu_core::llm::ChatStreamKind::Content,
                text: "好了".into(),
            })
            .unwrap();
        renderer.finish().unwrap();
        let frame = String::from_utf8_lossy(&renderer.take_output_frame()).to_string();
        // 想了 2.4s + 跑了 1.2s 的命令:09-24 起跑过命令就 `Ran` 打头、耗时挂末尾。
        assert!(
            frame.contains("Ran 1 command · 1 thought · 3.6s"),
            "回放没把耗时算回来: {frame}"
        );
    });
}

/// 面板里「思考中」那一行也要能点开。
///
/// 它常常是面板最下面那一行，而正在想什么恰恰是此刻最值得看的
///（用户实测：浮层内最下面一行无法交互）。
#[test]
fn the_live_thinking_row_in_a_panel_is_clickable() {
    with_blocks(|| {
        let mut renderer = timeline_renderer();
        renderer
            .write_tool_call("subagent", r#"{"description":"查目录","prompt":"去看看"}"#)
            .unwrap();
        renderer.subagent_thought("subagent", "正在想这件事该怎么办");
        let id = renderer.subagent_overlay_id("subagent").expect("没登记");
        let lines = crate::render::blocks::get(id).unwrap_or_default();
        let live = lines
            .iter()
            .find(|line| crate::render::strip_ansi_text(line).contains(t("thinking", "思考中")))
            .expect("没有思考中那一行");
        let block = block_id_in(live).expect("思考中那一行没挂块，点了没反应");
        let detail = crate::render::blocks::get(block).unwrap_or_default();
        assert!(
            detail
                .iter()
                .any(|line| line.contains("正在想这件事该怎么办")),
            "点开看不到正在想什么: {detail:?}"
        );
    });
}

/// 查看系统信息用「核心」那个图标，和装包分开。
///
/// 它原来跟 `install_aur_package` 挤在一类里用包裹图标——查机器和装包不是
/// 一回事（用户指名要 CoreOS 那个圆里嵌核的标）。
#[test]
fn checking_the_machine_gets_the_core_glyph() {
    // 这条钉的是 **Nerd Font 那张表**；`MIYU_TUI_ASCII=1` 下所有工具本来就统一
    // 退到 `⚙`（`timeline.rs`「没有 Nerd Font 的时候别凑」），拿它去比是两把尺。
    if std::env::var_os("MIYU_TUI_ASCII").is_some() {
        return;
    }
    let core = crate::render::tool_glyph_for("check_os_info");
    assert_eq!(core, "\u{f305}", "系统信息的图标不对");
    assert_ne!(
        core,
        crate::render::tool_glyph_for("install_aur_package"),
        "查机器和装包不该共用一个图标"
    );
}

/// 子代理内层的「编辑文件」点开是 **diff**，不是一团原始 JSON。
///
/// 工具自己跑那条路会用改前改后算真 diff（`__patch_preview__`），但那条只到
/// 发起它的渲染器；子代理内层的编辑手上只有调用参数里的信封（用户截图实录：
/// 展开之后是 `{"patchText": …}` 加一份结果 JSON）。
#[test]
fn a_subagent_edit_step_opens_into_a_diff() {
    with_blocks(|| {
        let mut renderer = timeline_renderer();
        renderer
            .write_tool_call("subagent", r#"{"description":"改文件","prompt":"去改"}"#)
            .unwrap();
        let patch = "*** Begin Patch\n*** Update File: /tmp/a.svg\n@@\n-旧的一行\n+新的一行\n*** End Patch\n";
        let args = serde_json::json!({ "patchText": patch }).to_string();
        renderer.subagent_tool(
            "subagent",
            "edit",
            "编辑文件",
            &args,
            true,
            r#"{"ok":true,"files_changed":1,"operation":"apply_patch"}"#,
        );
        let id = renderer.subagent_overlay_id("subagent").expect("没登记");
        let lines = crate::render::blocks::get(id).unwrap_or_default();
        // 行上的窥视是路径，不是那团 JSON。
        let row = lines
            .iter()
            .map(|line| crate::render::strip_ansi_text(line))
            .find(|line| line.contains("编辑文件"))
            .expect("没有编辑那一步");
        assert!(row.contains("/tmp/a.svg"), "窥视不是路径: {row:?}");
        assert!(!row.contains("patchText"), "窥视甩出了原始 JSON: {row:?}");
        // 点开是 diff：加的那行和减的那行都在，结果 JSON 不在。
        let detail = lines
            .iter()
            .filter_map(|line| block_id_in(line))
            .filter_map(crate::render::blocks::get)
            .flatten()
            .map(|line| crate::render::strip_ansi_text(&line))
            .collect::<Vec<_>>();
        assert!(
            detail.iter().any(|line| line.contains("新的一行")),
            "没画出加的那行: {detail:?}"
        );
        assert!(
            detail.iter().any(|line| line.contains("旧的一行")),
            "没画出减的那行: {detail:?}"
        );
        assert!(
            !detail.iter().any(|line| line.contains("apply_patch")),
            "结果 JSON 还在里面: {detail:?}"
        );
    });
}

/// 子代理那一行按「名字 · 烧了多少 · 跑了多久」写，而且不把描述说两遍。
#[test]
fn a_subagent_row_carries_its_token_count() {
    with_blocks(|| {
        let mut renderer = timeline_renderer();
        renderer
            .write_tool_call(
                "subagent:画鹅鹅",
                r#"{"description":"画鹅鹅","prompt":"去画"}"#,
            )
            .unwrap();
        renderer
            .write_tool_progress(
                "subagent:画鹅鹅",
                "__subagent_metric__≈3.1K\t3100\t工具调用 5 次　消耗词元 ≈3.1K",
            )
            .unwrap();
        renderer.refresh_live_block();
        let line = renderer
            .timeline_running_tool_lines()
            .into_iter()
            .next()
            .expect("没有跑着的那一行")
            .line;
        assert!(line.contains("≈3.1K"), "跑着的那一行没有量: {line:?}");
        // 收进时间线之后也要有，而且不再把描述当窥视说第二遍。
        renderer
            .write_tool_result("subagent:画鹅鹅", true, "done")
            .unwrap();
        // 收进时间线（正常是模型开始说正文时触发），但别 `finish`——那会把
        // 整条线剪走。
        renderer.finalize_tools_summary().unwrap();
        let step = renderer
            .timeline_step_lines()
            .into_iter()
            .map(|line| crate::render::strip_ansi_text(&line))
            .find(|line| line.contains("画鹅鹅"))
            .expect("没有子代理那一步");
        assert!(step.contains("≈3.1K"), "收起来之后没有量: {step:?}");
        assert_eq!(step.matches("画鹅鹅").count(), 1, "描述说了两遍: {step:?}");
    });
}

/// 子代理开口说正文，前面那一段过程要收成一行 `Worked for …`。
///
/// 面板里一路平铺着几十步的话，真正的产出反而被埋在最底下（用户提议：把主体
/// 相同的逻辑放到浮层里）。
#[test]
fn a_subagent_panel_folds_its_steps_once_it_starts_talking() {
    with_blocks(|| {
        let mut renderer = timeline_renderer();
        renderer
            .write_tool_call("subagent", r#"{"description":"查目录","prompt":"去看看"}"#)
            .unwrap();
        for index in 0..3 {
            renderer.subagent_thought("subagent", &format!("想第 {index} 次"));
            renderer.subagent_tool(
                "subagent",
                "run_command",
                "运行命令",
                &format!(r#"{{"command":"ls {index}"}}"#),
                true,
                "输出",
            );
        }
        let id = renderer.subagent_overlay_id("subagent").expect("没登记");
        let before = crate::render::blocks::get(id).unwrap_or_default();
        let steps_before = before
            .iter()
            .filter(|line| crate::render::strip_ansi_text(line).contains("运行命令"))
            .count();
        assert_eq!(steps_before, 3, "三步没都在: {before:?}");

        renderer.subagent_content("subagent", "查完了，目录里有三个文件。");
        let after = crate::render::blocks::get(id).unwrap_or_default();
        let text = after
            .iter()
            .map(|line| crate::render::strip_ansi_text(line))
            .collect::<Vec<_>>();
        // 收缩行长这样：`› Ran 3 commands · 3 thoughts`（09-24 起命令单独数）。
        // 测试里这一段只花了几十微秒，`summary_line` 按设计不报耗时（回放也是
        // 这个规矩），所以只认计数。
        assert!(
            text.iter()
                .any(|line| line.contains('›') && line.contains("Ran 3 commands · 3 thoughts")),
            "没收成一行: {text:?}"
        );
        assert!(
            !text.iter().any(|line| line.contains("运行命令")),
            "收完之后那几步还平铺着: {text:?}"
        );
        assert!(
            text.iter().any(|line| line.contains("查完了")),
            "正文没进面板: {text:?}"
        );
        // 收起来的那几步点开还在。
        let inner = after
            .iter()
            .filter_map(|line| block_id_in(line))
            .filter_map(crate::render::blocks::get)
            .flatten()
            .map(|line| crate::render::strip_ansi_text(&line))
            .collect::<Vec<_>>();
        assert!(
            inner
                .iter()
                .filter(|line| line.contains("运行命令"))
                .count()
                >= 3,
            "点开之后那几步不见了: {inner:?}"
        );
        // 「提示词」那一行钉在最前面，不参与收缩。
        assert!(
            crate::render::strip_ansi_text(&after[0]).contains(t("prompt", "提示词")),
            "提示词那行被收进去了: {text:?}"
        );
    });
}

/// 后台任务工具用清单图标，和 todo 清单分得开。
#[test]
fn the_background_jobs_tool_gets_the_list_glyph() {
    // 这条钉的是 **Nerd Font 那张表**；`MIYU_TUI_ASCII=1` 下所有工具本来就统一
    // 退到 `⚙`（`timeline.rs`「没有 Nerd Font 的时候别凑」），拿它去比是两把尺。
    if std::env::var_os("MIYU_TUI_ASCII").is_some() {
        return;
    }
    assert_eq!(crate::render::tool_glyph_for("job"), "\u{f0572}");
    assert_ne!(
        crate::render::tool_glyph_for("job"),
        crate::render::tool_glyph_for("todowrite")
    );
}

/// 清单也是这一轮做过的一件事，时间线里得有它那一步。
///
/// 原来 `todowrite` 跑完会把**整批** `tool_stats` 清空（inline 那边表已经就地
/// 画出来了，不想再留一行状态），全屏下连带把这一步也抹了——用户看不到那个
/// tag 行，同一批里别的工具也跟着消失。
#[test]
fn the_todo_tool_still_leaves_a_step_on_the_timeline() {
    with_blocks(|| {
        let mut renderer = timeline_renderer();
        renderer.use_buffered_output();
        for (name, args) in [
            ("run_command", r#"{"command":"ls"}"#),
            ("todowrite", r#"{"todos":[]}"#),
        ] {
            renderer.write_tool_call(name, args).unwrap();
            renderer.write_tool_result(name, true, "out").unwrap();
        }
        renderer.finalize_tools_summary().unwrap();
        // 清单是一段的句点(09-16):两步都在收缩块的展开内容里,不在 live 时间线上。
        let frame = String::from_utf8_lossy(&renderer.take_output_frame()).into_owned();
        let steps = frame
            .lines()
            .filter_map(block_id_in)
            .filter_map(crate::render::blocks::get)
            .flatten()
            .map(|line| crate::render::strip_ansi_text(&line))
            .collect::<Vec<_>>();
        assert!(
            steps
                .iter()
                .any(|line| line.contains(t("Todo list", "任务列表"))),
            "清单那一步没了: {steps:?}"
        );
        assert!(
            steps
                .iter()
                .any(|line| line.contains(t("Run command", "运行命令"))),
            "同一批里别的工具被连累了: {steps:?}"
        );
    });
}

/// 已经跑完的那几步在 live 区里**就能点开**，不用等收成 `Worked for …`。
///
/// 原来只有正在跑的那一行挂块，跑完的步骤要等模型开口说正文、整段收缩之后
/// 才登记——于是"编辑文件"的 diff 要等 AI 输出完所有内容才看得到（用户实测）。
#[test]
fn completed_steps_in_the_live_area_are_clickable_right_away() {
    with_blocks(|| {
        let mut renderer = timeline_renderer();
        renderer.use_buffered_output();
        renderer
            .write_tool_call("edit", r#"{"patchText":"*** Begin Patch\n*** Update File: /tmp/a.txt\n@@\n-旧的一行\n+新的一行\n*** End Patch\n"}"#)
            .unwrap();
        let preview = serde_json::json!({
            "path": "/tmp/a.txt",
            "diff": "--- a/a.txt\n+++ b/a.txt\n@@ -1,1 +1,1 @@\n-旧的一行\n+新的一行\n",
        })
        .to_string();
        renderer
            .write_tool_progress("edit", &format!("__patch_preview__{preview}"))
            .unwrap();
        renderer
            .write_tool_result("edit", true, r#"{"ok":true}"#)
            .unwrap();
        // 这一步已经收进时间线了，模型还没开口。live 区里它得挂着块。
        let (_, live) = renderer.timeline_waiting();
        let live = live.expect("live 区是空的");
        let row = live
            .lines()
            .find(|line| crate::render::strip_ansi_text(line).contains(t("Edit file", "编辑文件")))
            .expect("编辑那一步不在 live 区里");
        let id = block_id_in(row).expect("跑完的那一步没挂块，点不开");
        let detail = crate::render::blocks::get(id)
            .unwrap_or_default()
            .into_iter()
            .map(|line| crate::render::strip_ansi_text(&line))
            .collect::<Vec<_>>();
        assert!(
            detail.iter().any(|line| line.contains("新的一行")),
            "点开不是 diff: {detail:?}"
        );
        // 收成 `Worked for …` 之后用的还是同一块：展开状态跟着走。
        renderer.cut_timeline().unwrap();
        let frame = String::from_utf8_lossy(&renderer.take_output_frame()).into_owned();
        let head = block_id_in(&frame).expect("收缩行没挂块");
        let inner = crate::render::blocks::get(head).unwrap_or_default();
        let ids = inner
            .iter()
            .filter_map(|line| block_id_in(line))
            .collect::<Vec<_>>();
        assert!(
            ids.contains(&id),
            "收缩之后那一步换了块 id: {ids:?} vs {id}"
        );
    });
}

/// 跑着的命令点开是**流式**的输出：每刷新一次，新吐出来的行就在里面。
#[test]
fn a_running_command_expands_to_its_streaming_output() {
    with_blocks(|| {
        let mut renderer = timeline_renderer();
        renderer
            .write_tool_call("run_command", r#"{"command":"tail -f log"}"#)
            .unwrap();
        renderer
            .write_command_output(
                "run_command",
                miyu_engine::tools::CommandOutputStream::Stdout,
                b"first line\n",
            )
            .unwrap();
        renderer.refresh_live_block();
        let id = renderer
            .live_tool_blocks
            .get("run_command")
            .copied()
            .expect("跑着的命令没挂块");
        let detail = crate::render::blocks::get(id)
            .unwrap_or_default()
            .join("\n");
        assert!(detail.contains("tail -f log"), "点开没有命令: {detail:?}");
        assert!(
            detail.contains("first line"),
            "点开没有已经吐出来的输出: {detail:?}"
        );
        renderer
            .write_command_output(
                "run_command",
                miyu_engine::tools::CommandOutputStream::Stdout,
                b"second line\n",
            )
            .unwrap();
        renderer.refresh_live_block();
        let detail = crate::render::blocks::get(id)
            .unwrap_or_default()
            .join("\n");
        assert!(
            detail.contains("second line"),
            "展开着的内容没跟着输出长: {detail:?}"
        );
    });
}

/// Ctrl+C 打断时命令还在跑：它收成时间线上一步「已中断」，而不是让 inline 那套
/// `$ 运行命令×1 运行中 / ↳ / │` 卡片漏到全屏画面里（用户实测截图）。
#[test]
fn finishing_mid_command_folds_it_in_as_interrupted() {
    with_blocks(|| {
        let mut renderer = timeline_renderer();
        renderer.use_buffered_output();
        renderer
            .write_tool_call("run_command", r#"{"command":"sleep 15"}"#)
            .unwrap();
        renderer
            .write_command_output(
                "run_command",
                miyu_engine::tools::CommandOutputStream::Stdout,
                b"started\n",
            )
            .unwrap();
        renderer.finish().unwrap();
        let frame = String::from_utf8_lossy(&renderer.take_output_frame()).into_owned();
        assert!(
            !frame.contains("×1") && !frame.contains("↳"),
            "inline 的命令卡片漏出来了: {frame:?}"
        );
        // 收缩行点开是时间线，里面那一步是红的、写着已中断，点开还有已经吐出的输出。
        let head = block_id_in(&frame).expect("收缩行没挂块");
        let inner = crate::render::blocks::get(head).unwrap_or_default();
        let step = inner
            .iter()
            .find(|line| crate::render::strip_ansi_text(line).contains("sleep 15"))
            .expect("命令那一步不在时间线里");
        assert!(step.contains("\x1b[31m"), "被打断的那一步没标红: {step:?}");
        assert!(
            crate::render::strip_ansi_text(step).contains(t("interrupted", "已中断")),
            "没说明是被打断的: {step:?}"
        );
        let detail = block_id_in(step)
            .and_then(crate::render::blocks::get)
            .unwrap_or_default()
            .join("\n");
        assert!(detail.contains("started"), "打断前的输出丢了: {detail:?}");
    });
}

/// 子代理说过一段话之后又接着想、接着动手：那段话留在它说出来的位置上，
/// 新的思考排在它**后面**——原来正文一直挂在面板最底下，于是「思考中」跑到了
/// 它上面（用户实测截图）。
#[test]
fn a_subagent_speech_keeps_its_place_when_it_thinks_again() {
    with_blocks(|| {
        let mut renderer = timeline_renderer();
        renderer
            .write_tool_call("subagent", r#"{"description":"审计","prompt":"去看看"}"#)
            .unwrap();
        renderer.subagent_thought("subagent", "先想一下");
        renderer.subagent_tool(
            "subagent",
            "run_command",
            "运行命令",
            r#"{"command":"ls"}"#,
            true,
            "a b c",
        );
        renderer.subagent_content("subagent", "我先说一句中间话。");
        renderer.subagent_thought("subagent", "然后再想第二轮");
        let id = renderer.subagent_overlay_id("subagent").expect("没登记");
        let text = crate::render::blocks::get(id)
            .unwrap_or_default()
            .iter()
            .map(|line| crate::render::strip_ansi_text(line))
            .collect::<Vec<_>>();
        let speech = text
            .iter()
            .position(|line| line.contains("中间话"))
            .unwrap_or_else(|| panic!("说的话没进面板: {text:?}"));
        let thinking = text
            .iter()
            .position(|line| line.contains(t("thinking", "思考中")))
            .unwrap_or_else(|| panic!("第二轮思考没在面板里: {text:?}"));
        assert!(speech < thinking, "说过的话排到了后来的思考下面: {text:?}");
        // 第二轮的工具落下来之后，它仍然在说的话之后；最后说的话仍在最底下。
        renderer.subagent_tool(
            "subagent",
            "run_command",
            "运行命令",
            r#"{"command":"pwd"}"#,
            true,
            "/tmp",
        );
        renderer.subagent_content("subagent", "最后的结论。");
        let text = crate::render::blocks::get(id)
            .unwrap_or_default()
            .iter()
            .map(|line| crate::render::strip_ansi_text(line))
            .collect::<Vec<_>>();
        let speech = text
            .iter()
            .position(|line| line.contains("中间话"))
            .unwrap();
        // 再次开口说话时，中间那一段过程（想 + pwd）收成一行 `⌄ …`，它在
        // 第一段话之后、最后那段话之前；点开还是那两步。（第一段话之前还有
        // 一条收缩行，所以取**最后**那条。）
        let fold = text
            .iter()
            .rposition(|line| line.contains('›'))
            .unwrap_or_else(|| panic!("中间那段过程没收成一行: {text:?}"));
        let last = text
            .iter()
            .position(|line| line.contains("最后的结论"))
            .unwrap();
        assert!(speech < fold && fold < last, "时序乱了: {text:?}");
        // 命令 09-17 起不在抬头上,要多钻一层:收缩行 → 那一步 → 命令全文。
        let fold_content = crate::render::blocks::get(id)
            .unwrap_or_default()
            .iter()
            .filter_map(|line| block_id_in(line))
            .filter_map(crate::render::blocks::get)
            .flatten()
            .collect::<Vec<_>>();
        let inner = fold_content
            .iter()
            .filter_map(|line| block_id_in(line))
            .filter_map(crate::render::blocks::get)
            .flatten()
            .map(|line| crate::render::strip_ansi_text(&line))
            .collect::<Vec<_>>();
        assert!(
            inner.iter().any(|line| line.contains("pwd")),
            "收进去的那一步点开不见了: {inner:?}"
        );
    });
}

/// 一行开头有几个空格：面板里各步是不是同一列，就看这个。
fn column_of(line: &str) -> usize {
    line.chars().take_while(|c| *c == ' ').count()
}

/// 子代理面板里的收缩行点开是一条时间线：抬头底下接连线，收起来的每一步和抬头
/// 同一列——和主线那条 `Worked for …` 一个样子，不是往右缩进的一段正文
///（用户实测：worked for 底下的内容缩进不对，timeline 也不对）。
#[test]
fn the_fold_opens_into_a_timeline_not_an_indented_body() {
    with_blocks(|| {
        let mut renderer = timeline_renderer();
        renderer
            .write_tool_call("subagent", r#"{"description":"查目录","prompt":"去看看"}"#)
            .unwrap();
        renderer.subagent_thought("subagent", "先想想");
        renderer.subagent_tool(
            "subagent",
            "run_command",
            "运行命令",
            r#"{"command":"ls"}"#,
            true,
            "输出",
        );
        renderer.subagent_content("subagent", "查完了。");
        let id = renderer.subagent_overlay_id("subagent").expect("没登记");
        let panel = crate::render::blocks::get(id).unwrap_or_default();
        let fold = panel
            .iter()
            .find(|line| crate::render::strip_ansi_text(line).contains("Ran 1 command"))
            .unwrap_or_else(|| panic!("没收成一行: {panel:?}"));
        let fold_id = block_id_in(fold).expect("收缩行没挂块");
        // 合着是 `›`，点开（块内容第一行）翻成 `⌄`——和主线那条一样。
        assert!(
            crate::render::strip_ansi_text(fold)
                .trim_start()
                .starts_with('›'),
            "合着的收缩行不是 ›: {fold:?}"
        );
        let detail: Vec<String> = crate::render::blocks::get(fold_id)
            .unwrap_or_default()
            .iter()
            .map(|line| crate::render::strip_ansi_text(line))
            .collect();
        assert!(
            detail[0].trim_start().starts_with('⌄'),
            "点开的抬头不是 ⌄: {detail:?}"
        );
        assert_eq!(detail[1].trim(), "│", "抬头底下不是连线: {detail:?}");
        let head_col = column_of(&detail[0]);
        let thought = detail
            .iter()
            .position(|line| line.contains(t("thought", "已思考")))
            .unwrap_or_else(|| panic!("收起来的思考不见了: {detail:?}"));
        assert!(
            !detail[thought].contains("0.0s"),
            "思考那一步报了个 0.0s: {detail:?}"
        );
        let tool = detail
            .iter()
            .position(|line| line.contains("运行命令"))
            .unwrap_or_else(|| panic!("收起来的工具不见了: {detail:?}"));
        assert_eq!(
            column_of(&detail[thought]),
            head_col,
            "思考那一步没和抬头同一列: {detail:?}"
        );
        assert_eq!(
            column_of(&detail[tool]),
            head_col,
            "工具那一步没和抬头同一列: {detail:?}"
        );
        assert_eq!(
            detail[thought + 1].trim(),
            "│",
            "两步之间没有连线: {detail:?}"
        );
    });
}

/// 排在时间线后面的那一块（图、清单表）上下各空一行。
///
/// 09-19 用户报「全屏 TUI 里缺空行」：图紧贴着 `Worked for …` 那一行长出来，
/// 而图和后面的正文之间反倒空了两行。上面那行空谁都没出，下面那行空出了两遍
/// （收段一次、投递方自己又补一个 `\n`）。
#[test]
fn a_queued_result_block_is_fenced_by_one_blank_line_on_each_side() {
    with_blocks(|| {
        let mut renderer = timeline_renderer();
        renderer.use_buffered_output();
        renderer
            .write_tool_call("use_meme", r#"{"action":"show","id":"x"}"#)
            .unwrap();
        renderer
            .write_tool_result("use_meme", true, "sent meme x")
            .unwrap();
        // 图占位格进缓冲的形状：逐行、行末带换行。
        renderer.queue_after_timeline("  ▉▉▉\r\n  ▉▉▉\r\n".to_string());
        renderer.cut_timeline().unwrap();
        let frame = String::from_utf8_lossy(&renderer.take_output_frame()).into_owned();
        let rows = frame
            .lines()
            .map(|line| crate::render::strip_ansi_text(line).trim_end().to_string())
            .collect::<Vec<_>>();
        // 收缩行：`› …`（这一轮耗时是 0，所以措辞是 `1 tool` 而不是 `Worked for`）
        let head = rows
            .iter()
            .position(|line| line.trim_start().starts_with('\u{203a}'))
            .unwrap_or_else(|| panic!("没有收缩行: {rows:?}"));
        let first = rows
            .iter()
            .position(|line| line.contains('▉'))
            .unwrap_or_else(|| panic!("图那几行没落下来: {rows:?}"));
        let last = rows.iter().rposition(|line| line.contains('▉')).unwrap();
        assert_eq!(
            first - head,
            2,
            "收缩行和图之间不是正好一行空: {:?}",
            &rows[head..=first]
        );
        assert!(
            rows.get(last + 1).is_some_and(|line| line.is_empty()),
            "图下面没有空行: {:?}",
            &rows[last..]
        );
        assert!(
            rows.get(last + 2).is_none_or(|line| !line.is_empty()),
            "图下面空了不止一行: {:?}",
            &rows[last..]
        );
    });
}

/// 还在跑的工具要终端腾地方时，不能被当成「已中断」收掉。
///
/// 09-19 用户：shellhook 里每发一次表情包就多两行 `✗ 表情包 · 已中断`，而那次
/// 其实是成功的。发图要先请渲染器收尾（`prepare_for_external_output`），收尾
/// 那一刻这次调用**还没返回**——判成「没跑完 = 中断」收一次，真结果回来统计已经
/// 被清空、又当成新的一次收一次。
#[test]
fn a_tool_still_running_when_the_terminal_is_borrowed_is_not_cut_as_interrupted() {
    with_blocks(|| {
        const MEME_GLYPH: char = '\u{f118}';
        let mut renderer = timeline_renderer();
        renderer.use_buffered_output();
        renderer
            .write_tool_call("use_meme", r#"{"action":"show","id":"x"}"#)
            .unwrap();
        renderer.prepare_for_external_output().unwrap();
        let cut = String::from_utf8_lossy(&renderer.take_output_frame()).into_owned();
        assert!(
            !cut.contains(&t("interrupted", "已中断")),
            "腾地方时把还在跑的工具收成了中断: {cut}"
        );
        // 真结果回来，这才轮到它落地——而且只落一次。
        renderer
            .write_tool_result("use_meme", true, "sent meme x")
            .unwrap();
        renderer.finish().unwrap();
        let frame = String::from_utf8_lossy(&renderer.take_output_frame()).into_owned();
        // 收缩行只报一次工具、零个错。原来是「2 tools · 2 errs」。
        let summary = frame
            .lines()
            .map(crate::render::strip_ansi_text)
            .find(|line| line.trim_start().starts_with('\u{203a}'))
            .unwrap_or_else(|| panic!("没有收缩行: {frame:?}"));
        assert!(
            summary.contains("1 tool") && !summary.contains("err"),
            "收缩行把一次发图记成了多次/记了错: {summary:?}"
        );
        // 点开也只有那一步。
        let head = block_id_in(&frame).expect("收缩行没挂块");
        let rows = crate::render::blocks::get(head)
            .unwrap_or_default()
            .iter()
            .map(|line| crate::render::strip_ansi_text(line))
            .filter(|line| line.contains(MEME_GLYPH))
            .collect::<Vec<_>>();
        assert_eq!(rows.len(), 1, "表情包那一步落了不止一次: {rows:?}");
        assert!(
            !rows[0].contains(&t("interrupted", "已中断")),
            "落下来的那一步还是中断态: {rows:?}"
        );
    });
}
