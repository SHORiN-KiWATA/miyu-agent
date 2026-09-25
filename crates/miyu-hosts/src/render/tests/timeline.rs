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

/// 按界面语言挑期望值（收缩行 09-26 起中英两套）。
fn say(en: &str, zh: &str) -> String {
    if miyu_base::i18n::is_zh() { zh } else { en }.to_string()
}

/// 09-26 用户拍板的收缩行写法：动作在前、思考在后、出错垫底，为零的项不写；不再有
/// `Worked for`、不挂耗时（一轮花了多久看末尾的 `✻` 那行）。中文统一「动词了 + 次数」。
#[test]
fn summary_leads_with_commands_then_edits_tools_thoughts() {
    let twelve = Duration::from_millis(12_300);
    assert_eq!(
        summary_line(twelve, counts(3, 2, 2, 2, 1)),
        say(
            "Ran 3 commands · 2 edits · 2 tools · 2 thoughts · 1 err",
            "运行了 3 次命令 · 编辑了 2 次 · 用了 2 个工具 · 思考了 2 次 · 出错了 1 次"
        )
    );
    assert_eq!(
        summary_line(Duration::from_millis(2_500), counts(1, 0, 0, 0, 0)),
        say("Ran 1 command", "运行了 1 次命令")
    );
    // 没跑命令：不再拿 `Worked for` 打头，edits 与 tools 分开数。
    assert_eq!(
        summary_line(twelve, counts(0, 2, 1, 0, 0)),
        say("2 edits · 1 tool", "编辑了 2 次 · 用了 1 个工具")
    );
    assert_eq!(
        summary_line(twelve, counts(0, 0, 3, 1, 0)),
        say("3 tools · 1 thought", "用了 3 个工具 · 思考了 1 次")
    );
    // 只想了想：写想了多久，不写次数。
    assert_eq!(
        summary_line(Duration::from_millis(400), counts(0, 0, 0, 2, 0)),
        say("Thought for 400ms", "思考了不到 1 秒")
    );
    assert_eq!(
        summary_line(Duration::from_millis(5_200), counts(0, 0, 0, 2, 0)),
        say("Thought for 5.2s", "思考了 5.2 秒")
    );
}

/// 回放历史时没有计时。报 `思考了 0.0 秒` 会让人以为"这一轮瞬间就完了"，只想了想的
/// 那种就写想了几次。
#[test]
fn summary_without_timing_drops_the_duration() {
    assert_eq!(
        summary_line(Duration::ZERO, counts(3, 2, 0, 1, 0)),
        say(
            "Ran 3 commands · 2 edits · 1 thought",
            "运行了 3 次命令 · 编辑了 2 次 · 思考了 1 次"
        )
    );
    assert_eq!(
        summary_line(Duration::ZERO, counts(0, 0, 2, 1, 0)),
        say("2 tools · 1 thought", "用了 2 个工具 · 思考了 1 次")
    );
    assert_eq!(
        summary_line(Duration::ZERO, counts(0, 0, 0, 2, 0)),
        say("2 thoughts", "思考了 2 次")
    );
    // 什么都没有时也得说句人话，不能给个空串
    assert!(!summary_line(Duration::ZERO, Counts::default()).is_empty());
}

#[test]
fn a_run_without_timing_reports_what_it_did_not_zero_seconds() {
    // 回放没有计时；那条路上时间线还是会现场掐一次表，量出来是几十微秒。动过手的那种
    // 本来就不挂耗时，有没有计时写出来一个样。
    assert_eq!(
        summary_line(Duration::from_micros(40), counts(0, 0, 1, 2, 0)),
        say("1 tool · 2 thoughts", "用了 1 个工具 · 思考了 2 次")
    );
    assert_eq!(
        summary_line(Duration::from_millis(2_500), counts(0, 0, 1, 2, 0)),
        say("1 tool · 2 thoughts", "用了 1 个工具 · 思考了 2 次")
    );
    // 收缩行里再也不出现 `Worked for`（用户 09-26）。
    for elapsed in [Duration::ZERO, Duration::from_secs(12)] {
        assert!(!summary_line(elapsed, Counts::default()).contains("Worked for"));
    }
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

fn child_status(peek: &str) -> miyu_engine::tools::subagent::status::SubagentStatus {
    miyu_engine::tools::subagent::status::SubagentStatus {
        peek: peek.to_string(),
        tokens_label: "150".to_string(),
        tokens: 150,
        session_id: Some("sess_child".to_string()),
    }
}

/// 跑着的子代理那一行点下去是**切进它的会话**（会话项目第 3 段），那一行自己那块链着
/// 会话；原来指向一块覆盖层（09-25 退役）。
#[test]
fn a_running_subagent_row_links_its_session() {
    with_blocks(|| {
        let mut renderer = timeline_renderer();
        renderer
            .write_tool_call("subagent", r#"{"description":"查目录","prompt":"去看看"}"#)
            .unwrap();
        renderer.write_subagent_status("subagent", child_status("运行命令 · 列目录"));
        renderer.refresh_live_block();
        let rows = renderer.timeline_running_tool_lines();
        let target = rows.first().expect("子代理那一行没了").target;
        let target = target.expect("子代理那一行没挂块，点了没反应");
        assert_eq!(
            crate::render::blocks::linked_session(target).as_deref(),
            Some("sess_child"),
            "子代理那一行没链到它的会话: {rows:?}"
        );
    });
}

/// 跑完收进时间线的那一步照样链着会话：没有正文也登记一块，点下去切进去。
#[test]
fn a_settled_subagent_step_links_its_session() {
    with_blocks(|| {
        let mut renderer = timeline_renderer();
        renderer.use_external_cursor_control();
        renderer.use_buffered_output();
        renderer
            .write_tool_call("subagent", r#"{"description":"查目录","prompt":"去看看"}"#)
            .unwrap();
        renderer.write_subagent_status("subagent", child_status("查完了"));
        renderer
            .write_tool_result(
                "subagent",
                true,
                "subagent done (tier balanced, session sess_child): 好了",
            )
            .unwrap();
        renderer.finish().unwrap();
        let frame = String::from_utf8_lossy(&renderer.take_output_frame()).to_string();
        // 整段收成一行 `› 1 tool`，那一步在收缩行那一块的内容里。
        let fold = frame
            .lines()
            .find_map(block_id_in)
            .unwrap_or_else(|| panic!("没收成一行: {frame}"));
        let inside = crate::render::blocks::get(fold).unwrap_or_default();
        let linked = inside
            .iter()
            .filter_map(|line| block_id_in(line))
            .any(|id| crate::render::blocks::linked_session(id).as_deref() == Some("sess_child"));
        assert!(linked, "跑完那一步没链到会话: {inside:?}");
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
        assert!(
            frame.contains(t("thought", "思考了")),
            "收缩行没数到思考: {frame}"
        );
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
        renderer.write_subagent_status("subagent", child_status(&"想得很长".repeat(80)));
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
        renderer.write_subagent_status("subagent", child_status("运行命令"));
        renderer.refresh_live_block();
        let line = renderer
            .timeline_running_tool_lines()
            .first()
            .map(|row| crate::render::strip_ansi_text(&row.line))
            .unwrap_or_default();
        assert!(
            line.contains(&format!("{}运行命令", crate::render::timeline::PEEK_SEP)),
            "窥视没用 `·` 分隔: {line:?}"
        );
    });
}

/// 回放要把这一段想了多久算回来。
///
/// 回放是一瞬间喂完的，墙上时间是零——那一截于是整个消失，重开之后只剩「思考了 1 次」
/// （用户实测对比图）。每一步自己带着耗时，累加起来就是这一段的下限。动过手的那一段
/// 收缩行不挂耗时（09-26），只想了想的那一段还要写想了多久。
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
            .write_chunk(miyu_core::llm::ChatStreamChunk {
                kind: miyu_core::llm::ChatStreamKind::Content,
                text: "好了".into(),
            })
            .unwrap();
        renderer.finish().unwrap();
        let frame = String::from_utf8_lossy(&renderer.take_output_frame()).to_string();
        assert!(
            frame.contains(t("Thought for 2.4s", "思考了 2.4 秒")),
            "回放没把耗时算回来: {frame}"
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
        // 量走 `subagent.progress`（引擎按中继的 `__subagent_metric__` 收好的）。
        renderer.write_subagent_status(
            "subagent:画鹅鹅",
            miyu_engine::tools::subagent::status::SubagentStatus {
                tokens_label: "≈3.1K".to_string(),
                tokens: 3100,
                ..Default::default()
            },
        );
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

/// 一行开头有几个空格：面板里各步是不是同一列，就看这个。
fn column_of(line: &str) -> usize {
    line.chars().take_while(|c| *c == ' ').count()
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
            summary.contains(t("1 tool", "用了 1 个工具")) && !summary.contains(t("err", "出错")),
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

/// 前台子代理跑完（工具有了结果）之后，它烧的那份还留在 Σ 的实时加数里——它的会话已经落盘，
/// 但 footer 手里的会话累计要等下一次请求报上来才带上它；这时再撤（`absorb_settled_subagents`），
/// Σ 不闪也不算两遍。同名的又起一个，算新的在跑。
#[test]
fn a_finished_subagent_stays_in_the_live_share_until_the_next_session_total() {
    with_blocks(|| {
        let mut renderer = timeline_renderer();
        renderer.use_external_cursor_control();
        renderer.use_buffered_output();
        renderer
            .write_tool_call("subagent", r#"{"description":"查目录","prompt":"去看看"}"#)
            .unwrap();
        renderer.write_subagent_status("subagent", child_status("查完了"));
        renderer
            .write_tool_result(
                "subagent",
                true,
                "subagent done (tier balanced, session sess_child): 好了",
            )
            .unwrap();
        assert_eq!(
            renderer.running_subagent_tokens(),
            150,
            "跑完当场就撤，Σ 会往下闪"
        );

        renderer.absorb_settled_subagents();
        assert_eq!(
            renderer.running_subagent_tokens(),
            0,
            "会话累计里已经有它了"
        );

        renderer.write_subagent_status("subagent", child_status("又一个"));
        renderer.absorb_settled_subagents();
        assert_eq!(renderer.running_subagent_tokens(), 150, "新起的这个还在跑");
    });
}
