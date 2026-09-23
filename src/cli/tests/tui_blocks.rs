//! 可展开块：标记进出、视图映射、inline 不受影响。
//!
//! 最要紧的一条是最后那组：**inline 下字节流必须逐字节和以前一样**。全屏是可选
//! 项，为它往所有人的终端里塞标记是不能接受的。

use miyu_hosts::render::blocks;

/// 测试之间共用同一个进程级开关，串行跑免得互相掀桌子。
pub(super) fn with_blocks<T>(body: impl FnOnce() -> T) -> T {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    blocks::set_enabled(true);
    let out = body();
    blocks::set_enabled(false);
    out
}

#[test]
fn inline_stream_carries_no_markers() {
    blocks::set_enabled(false);
    let mut out = Vec::new();
    blocks::write_expandable(&mut out, vec!["详情".into()], |writer| {
        std::io::Write::write_all(writer, b"summary")
    })
    .unwrap();
    // 关掉时连注册都不该发生,更别说标记。
    assert_eq!(out, b"summary");
}

#[test]
fn enabled_stream_wraps_the_collapsed_body() {
    with_blocks(|| {
        let mut out = Vec::new();
        blocks::write_expandable(&mut out, vec!["详情".into()], |writer| {
            std::io::Write::write_all(writer, b"summary")
        })
        .unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("summary"));
        assert!(text.starts_with("\x1b]1337;miyu-block="));
        assert!(text.ends_with(blocks::END_MARKER));
    });
}

#[test]
fn empty_detail_is_not_expandable() {
    with_blocks(|| {
        assert!(blocks::register(Vec::new()).is_none());
        let mut out = Vec::new();
        blocks::write_expandable(&mut out, Vec::new(), |writer| {
            std::io::Write::write_all(writer, b"summary")
        })
        .unwrap();
        assert_eq!(out, b"summary");
    });
}

#[test]
fn expand_under_keeps_the_summary_as_the_handle() {
    with_blocks(|| {
        let lines = blocks::expand_under(
            "思考 · 30 词元",
            vec!["正文".into()],
            miyu_hosts::render::SummaryStyle::Reasoning,
        );
        // 头行 + 详情 + 收尾空行:展开版永远比折叠版(摘要 + 空行)高,
        // 视图偏移不会为负。
        assert_eq!(lines.len(), 3);
        assert!(lines[0].contains("思考 · 30 词元"));
        assert_eq!(lines[1], "正文");
    });
}

#[test]
fn registry_evicts_oldest_beyond_the_line_budget() {
    with_blocks(|| {
        let first = blocks::register(vec!["x".to_string(); 9_000]).expect("已开启");
        assert!(blocks::get(first).is_some());
        // 再塞一块把总行数顶过上限(16000 行),最老的那块要被让出来。
        let second = blocks::register(vec!["y".to_string(); 9_000]).expect("已开启");
        assert!(blocks::get(second).is_some());
        assert!(blocks::get(first).is_none());
    });
}

#[test]
fn markers_parse_both_ways() {
    assert!(matches!(
        blocks::parse_marker("miyu-block=42"),
        Some(blocks::BlockMarker::Begin {
            id: 42,
            open: false
        })
    ));
    // 「出来就是展开态」那一档走另一个载荷（`展开思考内容 / 展开工具内容`）。
    assert!(matches!(
        blocks::parse_marker("miyu-block-open=42"),
        Some(blocks::BlockMarker::Begin { id: 42, open: true })
    ));
    assert!(matches!(
        blocks::parse_marker("miyu-block-end"),
        Some(blocks::BlockMarker::End)
    ));
    // 同号段的别家载荷(以及全屏自己的 paint trace)不能误判成块标记。
    assert!(blocks::parse_marker("paint=37").is_none());
    assert!(blocks::parse_marker("File=inline=1").is_none());
}

// ---- 视图映射 ----------------------------------------------------------

use crate::cli::repl::tail::screen::ansi::spans_text;
use crate::cli::repl::tail::screen::Screen;

/// 三行正文，中间那行是一块（折叠时 1 行，展开后 3 行）。
fn screen_with_one_block() -> (Screen, u64) {
    let id = blocks::register(vec!["头".into(), "详情甲".into(), "详情乙".into()]).expect("已开启");
    let mut screen = Screen::detached(80, 24);
    screen.feed_for_test(
        format!(
            "上\r\n{}折叠{}\r\n下\r\n",
            blocks::begin_marker(id),
            blocks::END_MARKER
        )
        .as_bytes(),
    );
    (screen, id)
}

fn view_text(screen: &Screen, index: usize) -> String {
    spans_text(&screen.view_row(index))
}

#[test]
fn collapsed_view_is_the_buffer_itself() {
    with_blocks(|| {
        let (screen, _) = screen_with_one_block();
        assert_eq!(view_text(&screen, 0), "上");
        assert_eq!(view_text(&screen, 1), "折叠");
        assert_eq!(view_text(&screen, 2), "下");
    });
}

#[test]
fn expanding_pushes_later_rows_down() {
    with_blocks(|| {
        let (mut screen, id) = screen_with_one_block();
        let before = screen.view_len();
        assert!(screen.toggle_block(id));
        assert_eq!(screen.view_len(), before + 2);
        assert_eq!(view_text(&screen, 0), "上");
        assert_eq!(view_text(&screen, 1), "头");
        assert_eq!(view_text(&screen, 2), "详情甲");
        assert_eq!(view_text(&screen, 3), "详情乙");
        // 块后面的行整体下移,不能被吃掉也不能重复
        assert_eq!(view_text(&screen, 4), "下");
    });
}

/// 「展开思考内容 / 展开工具内容 = 开」落到字节流上就是 `miyu-block-open=`：
/// 视图第一次见到就替用户开一次，**不用点**（用户 09-17：「不用我点击他的 tag
/// 行，他出来就是展开的效果」）。
///
/// 再点一次照样收得回去，而且**不会被下一帧顶开**——活动区每 tick 把同样的标记
/// 重写一遍，不记着"开过一次"的话用户根本收不起来。
#[test]
fn a_default_open_block_opens_itself_once_and_stays_where_the_user_put_it() {
    with_blocks(|| {
        let id = blocks::register(vec!["头".into(), "详情".into()]).expect("已开启");
        let mut screen = Screen::detached(80, 24);
        let row = format!(
            "上\r\n{}折叠{}\r\n下\r\n",
            blocks::begin_marker_open(id),
            blocks::END_MARKER
        );
        screen.feed_for_test(row.as_bytes());
        assert!(screen.seed_open_blocks(), "默认开着的块没被打开");
        assert_eq!(view_text(&screen, 0), "上");
        assert_eq!(view_text(&screen, 1), "头");
        assert_eq!(view_text(&screen, 2), "详情");

        // 用户收起来了。
        assert!(screen.toggle_block(id));
        assert_eq!(view_text(&screen, 1), "折叠");
        // 同样的字节再来一帧：不许顶开。
        screen.feed_for_test(row.as_bytes());
        assert!(!screen.seed_open_blocks(), "收起来的块被下一帧顶开了");
    });
}

/// 出厂档位（两个开关都关着）写的是普通标记：一步都不该自己开。
#[test]
fn a_plain_block_stays_collapsed() {
    with_blocks(|| {
        let (mut screen, _) = screen_with_one_block();
        assert!(!screen.seed_open_blocks(), "普通块不该自己开");
        assert_eq!(view_text(&screen, 1), "折叠");
    });
}

#[test]
fn collapsing_restores_the_original_view() {
    with_blocks(|| {
        let (mut screen, id) = screen_with_one_block();
        let before: Vec<String> = (0..screen.view_len())
            .map(|index| view_text(&screen, index))
            .collect();
        screen.toggle_block(id);
        screen.toggle_block(id);
        let after: Vec<String> = (0..screen.view_len())
            .map(|index| view_text(&screen, index))
            .collect();
        assert_eq!(before, after);
    });
}

#[test]
fn hit_test_covers_the_whole_block_both_ways() {
    with_blocks(|| {
        let (mut screen, id) = screen_with_one_block();
        // 折叠时只有块自己那一行命中
        assert_eq!(screen.block_at(0), None);
        assert_eq!(screen.block_at(1), Some((id, 1)));
        assert_eq!(screen.block_at(2), None);
        screen.toggle_block(id);
        // 展开后整块都是点击目标:点任意一行都能收起来
        assert_eq!(screen.block_at(0), None);
        for row in 1..=3 {
            assert_eq!(screen.block_at(row), Some((id, 1)), "第 {row} 行该在块里");
        }
        assert_eq!(screen.block_at(4), None);
    });
}

#[test]
fn unknown_block_id_is_a_no_op() {
    with_blocks(|| {
        let (mut screen, _) = screen_with_one_block();
        let before = screen.view_len();
        assert!(!screen.toggle_block(9_999_999));
        assert_eq!(screen.view_len(), before);
    });
}

#[test]
fn nested_blocks_expand_independently() {
    with_blocks(|| {
        // 里层：一行摘要，展开成两行详情
        let inner = blocks::register(vec!["工具头".into(), "工具详情".into()]).expect("已开启");
        // 外层：一行 `Worked for …`，展开成一条 timeline——**timeline 里那行
        // 自己又是一个块**，这就是嵌套。
        let outer = blocks::register(vec![
            "Worked for 1s".into(),
            format!(
                "{}  工具 · 1s{}",
                blocks::begin_marker(inner),
                blocks::END_MARKER
            ),
        ])
        .expect("已开启");
        let mut screen = Screen::detached(80, 24);
        screen.feed_for_test(
            format!(
                "上\r\n{}Worked for 1s{}\r\n下\r\n",
                blocks::begin_marker(outer),
                blocks::END_MARKER
            )
            .as_bytes(),
        );
        assert_eq!(view_text(&screen, 1), "Worked for 1s");

        // 展开外层：timeline 出来，里层还是折叠的一行
        assert!(screen.toggle_block(outer));
        assert_eq!(view_text(&screen, 1), "Worked for 1s");
        assert_eq!(view_text(&screen, 2), "  工具 · 1s");
        assert_eq!(view_text(&screen, 3), "下");

        // 点 timeline 里那一行 → 命中的是**里层**,不是外层
        assert_eq!(screen.block_at(2), Some((inner, 2)));
        assert!(screen.toggle_block(inner));
        assert_eq!(view_text(&screen, 2), "工具头");
        assert_eq!(view_text(&screen, 3), "工具详情");
        assert_eq!(view_text(&screen, 4), "下");

        // 收起外层,里层跟着一起消失(它在外层内部)
        assert!(screen.toggle_block(outer));
        assert_eq!(view_text(&screen, 1), "Worked for 1s");
        assert_eq!(view_text(&screen, 2), "下");
    });
}

// ---- 覆盖层 ------------------------------------------------------------

#[test]
fn overlay_blocks_open_a_panel_instead_of_expanding() {
    with_blocks(|| {
        let id = blocks::register_overlay("走查子代理".into(), vec!["子代理第一行".into()])
            .expect("已开启");
        assert!(blocks::is_overlay(id));
        let mut screen = Screen::detached(80, 24);
        screen.feed_for_test(
            format!(
                "上\r\n{}子代理 · 3s{}\r\n下\r\n",
                blocks::begin_marker(id),
                blocks::END_MARKER
            )
            .as_bytes(),
        );
        assert!(!screen.overlay_open());
        assert!(screen.open_overlay(id));
        assert!(screen.overlay_open());
        // 正文一行都没动:覆盖层是盖上去的,不是就地展开
        assert_eq!(view_text(&screen, 1), "子代理 · 3s");
        assert_eq!(view_text(&screen, 2), "下");
        assert!(screen.close_overlay());
        assert!(!screen.overlay_open());
        // 已经关了再关就没得关
        assert!(!screen.close_overlay());
    });
}

#[test]
fn overlay_picks_up_streamed_updates() {
    with_blocks(|| {
        let id = blocks::register_overlay("标题".into(), vec!["第一行".into()]).expect("已开启");
        let before = blocks::version(id);
        blocks::update(id, String::new(), vec!["第一行".into(), "第二行".into()]);
        assert!(blocks::version(id) > before, "灌新内容要把版本号推上去");
        assert_eq!(
            blocks::get(id),
            Some(vec!["第一行".to_string(), "第二行".to_string()])
        );
    });
}

/// 图形传输段得从字节流里分出来发给终端，不能进缓冲。
///
/// vte 0.15 把 APC（`ESC _ … ESC \\`）整段丢掉，一个回调都不给：传输段喂进缓冲
/// 就等于没发。屏幕上只剩一片占位格，kitty 手里没有对应的图——用户看到的就是
/// 正文中间凭空多出一块空白（表情包、LaTeX 公式都栽在这儿）。
#[test]
fn graphics_are_split_out_of_the_buffer_stream() {
    use crate::cli::repl::tail::screen::split_graphics;

    // 没有图的字节流原样放行，连拷贝都省了。
    assert!(split_graphics(b"plain text").is_none());

    let stream = b"before\x1b_Gq=2,i=7,a=T,U=1;AAAA\x1b\\\x1b_Gq=2,m=0;BBBB\x1b\\after";
    let (graphics, rest) = split_graphics(stream).expect("有传输段");
    assert_eq!(
        graphics,
        b"\x1b_Gq=2,i=7,a=T,U=1;AAAA\x1b\\\x1b_Gq=2,m=0;BBBB\x1b\\".to_vec(),
        "两段都要挑出来，顺序不能乱"
    );
    assert_eq!(rest, b"beforeafter".to_vec(), "剩下的才是正文");

    // 半截传输段（分包到一半）宁可整段当图发走，也别切坏了留在缓冲里。
    let (graphics, rest) = split_graphics(b"x\x1b_Gq=2,i=7;AAA").expect("有传输段");
    assert_eq!(graphics, b"\x1b_Gq=2,i=7;AAA".to_vec());
    assert_eq!(rest, b"x".to_vec());
}

/// 喂进去的图形传输段不能在正文里留下任何痕迹。
///
/// 这条和 `graphics_are_split_out_of_the_buffer_stream` 是一里一外：那条钉住拆分
/// 本身，这条钉住 `Screen::feed` 真的把拆分接上了——接漏了的话，占位格还在、
/// 图没了，正文中间就是一块空白。
#[test]
fn graphics_leave_no_trace_in_the_body() {
    with_blocks(|| {
        let mut screen = Screen::detached(80, 24);
        screen.feed_for_test(b"\x1b_Gq=2,i=7,a=T,U=1;AAAA\x1b\\\xef\xbf\xbd\r\n");
        assert_eq!(
            view_text(&screen, 0),
            "\u{fffd}",
            "传输段该走终端，缓冲里只该剩占位格"
        );
    });
}

/// 正文自己折行：续行也带装订边，转义序列不被切断，能断在空格就断在空格。
#[test]
fn body_wraps_itself_instead_of_letting_the_buffer_do_it() {
    use miyu_hosts::render::wrap_display_text;

    // 断在空格处，不硬切在词中间。
    assert_eq!(
        wrap_display_text("alpha beta gamma", 11),
        vec!["alpha beta".to_string(), "gamma".to_string()]
    );
    // 一个词比一行还长就只能硬断——总比顶出去强。
    assert_eq!(
        wrap_display_text("abcdefghij", 4),
        vec!["abcd".to_string(), "efgh".to_string(), "ij".to_string()]
    );
    // 转义序列不算宽度，也不会被从中间切开。
    let colored = "\x1b[31mabcdef\x1b[0m";
    let wrapped = wrap_display_text(colored, 3);
    assert_eq!(
        wrapped,
        vec!["\x1b[31mabc".to_string(), "def\x1b[0m".to_string()]
    );
    // 宽字符按两格算。
    assert_eq!(
        wrap_display_text("中文换行测试", 4),
        vec!["中文".to_string(), "换行".to_string(), "测试".to_string()]
    );
}

/// kitty 的图片占位格进了缓冲还得是一格一格的。
///
/// 每一格是 `U+10EEEE + 行号记号 + 列号记号`。组合记号要**追加**在基字符后面；
/// 写成覆盖的话一行几十格会并成一格，终端收到的是一堆孤零零的记号，图一张都
/// 放不出来——pyte 抓屏看不出这个（它只还原字符网格），只有真 kitty 的截图
/// 会告诉你"占位格铺了几行，图没有"。
#[test]
fn kitty_placeholder_cells_survive_the_buffer() {
    with_blocks(|| {
        let mut screen = Screen::detached(80, 24);
        // 三格，各带两个组合记号
        let row = "\u{10EEEE}\u{0305}\u{0305}\u{10EEEE}\u{0305}\u{030D}\u{10EEEE}\u{0305}\u{030E}";
        screen.feed_for_test(format!("\x1b[38;2;0;0;7m{row}\x1b[0m\r\n").as_bytes());
        let text = view_text(&screen, 0);
        assert_eq!(
            text.chars().filter(|ch| *ch == '\u{10EEEE}').count(),
            3,
            "占位格被并成一格了: {text:?}"
        );
        assert_eq!(
            text.chars().filter(|ch| *ch == '\u{0305}').count(),
            4,
            "行号记号丢了: {text:?}"
        );
    });
}

/// 点链接：OSC 8 的目标要认得出来，正文里的裸链接也要认得出来。
///
/// 全屏把鼠标捕获走了，终端自己那套"点链接"就失效了。markdown 链接在屏幕上只露
/// 一个标题（「点这里」），目标藏在转义序列里——只按文本认的话，最常见的那种
/// 链接恰好一个都点不开。
#[test]
fn clicking_a_link_finds_its_target() {
    use crate::cli::repl::tail::screen::ansi::parse_ansi_line;
    use crate::cli::repl::tail::screen::select::url_at;

    // 裸链接：按文本认，标点不算在里面
    let spans = parse_ansi_line("  见 https://example.com/a,");
    assert_eq!(
        url_at(&spans, 6).as_deref(),
        Some("https://example.com/a"),
        "裸链接没认出来"
    );
    // 点在链接外面就没有链接
    assert!(url_at(&spans, 0).is_none());

    // OSC 8：屏幕上只有标题，目标在转义里
    let mut screen = Screen::detached(80, 24);
    screen.feed_for_test(
        b"\x1b]8;;https://example.com/b\x07\xe7\x82\xb9\xe8\xbf\x99\xe9\x87\x8c\x1b]8;;\x07\r\n",
    );
    let row = screen.view_row(0);
    assert_eq!(
        url_at(&row, 1).as_deref(),
        Some("https://example.com/b"),
        "OSC 8 的目标丢了: {:?}",
        spans_text(&row)
    );

    // `file://`：图表下面那行「点开看大图」指的是缓存里那张 SVG，可见文字里
    // 一个 URL 都没有。只认 http 的话点了没反应（用户 09-20 实测）。
    let mut screen = Screen::detached(80, 24);
    screen.feed_for_test("\x1b]8;;file:///tmp/d.svg\x07点开看大图\x1b]8;;\x07\r\n".as_bytes());
    let row = screen.view_row(0);
    assert_eq!(
        url_at(&row, 1).as_deref(),
        Some("file:///tmp/d.svg"),
        "file:// 没被认成可点的链接: {:?}",
        spans_text(&row)
    );
}

/// 后台面板里的时间线：一次工具调用只占**一步**。
///
/// 流水账里调用和结果是两行（`[工具] …` / `[结果] … ok`），原样贴出来同一件事
/// 会出现两遍，连线也跟着翻倍——用户原话「子代理的完全不对」。结果该做的是给
/// 那一步盖个 ok/err，不是自己另起一行。
#[test]
fn job_panel_merges_a_call_with_its_result() {
    with_blocks(|| {
        let dir = std::env::temp_dir().join(format!("miyu-log-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建目录");
        let path = dir.join("job.log");
        std::fs::write(
            &path,
            "[思考] 先看一眼\n[工具] 运行命令 · ls -la\n[结果] 运行命令 ok · ls -la\n",
        )
        .expect("写日志");
        let mut screen = Screen::detached(80, 24);
        assert!(screen.open_log_overlay(path.clone(), "走查".into(), None, String::new()));
        let rows = screen.overlay_rows();
        let calls = rows.iter().filter(|row| row.contains("运行命令")).count();
        assert_eq!(calls, 1, "调用和结果没合成一步: {rows:?}");
        // 结果只是把这一步收掉，`ok` 不上抬头：主线和前台面板都不写（用户实测：
        // 后台面板每一步尾巴上都拖着 ` · ok`）。
        assert!(
            !rows.iter().any(|row| row.contains(" · ok")),
            "ok 盖到抬头上了: {rows:?}"
        );
        // 跑完的那一步不该还挂着转轮。（`· 运行中` 那三个字 09-17 去掉了：
        // 主线从来不写，左边距上转着的点阵已经把「它在跑」说清楚了。）
        assert!(
            !rows
                .iter()
                .any(|row| row.contains(miyu_hosts::render::timeline::LIVE_SPINNER_CELL)),
            "有结果的那一步还挂着转轮: {rows:?}"
        );
        // 抬头按主线的说法写 `已思考 · 1.2s`，**不带窥视**（§6.3 第 1 项，用户
        // 09-17 拍板取前台）。所以这儿认「已思考」，不认原文。
        assert!(
            rows.iter().any(|row| row.contains("已思考")),
            "思考那一步没了: {rows:?}"
        );
        assert!(
            !rows.iter().any(|row| row.contains("先看一眼")),
            "已结算的思考不该再带窥视: {rows:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    });
}

/// 后台面板：一次工具调用点开要**有东西**。
///
/// 原来只在抬头被裁过时才把抬头补进详情里，于是短命令点开就是一条空带子
/// （用户实测：浮层里这些工具展开都没内容）。现在抬头无条件给全，工具真正
/// 吐出来的东西也跟着进来（流水账里的 `[输出]`）。
#[test]
fn job_panel_tool_step_expands_to_its_output() {
    with_blocks(|| {
        let dir = std::env::temp_dir().join(format!("miyu-log-out-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建目录");
        let path = dir.join("job.log");
        std::fs::write(
            &path,
            concat!(
                "[工具] run_command\t运行命令 · ls\n",
                "[结果] run_command\t运行命令 ok · ls\n",
                "[输出] total 12\n",
                "[输出] drwxr-xr-x 2 shorin\n",
            ),
        )
        .expect("写日志");
        let mut screen = Screen::detached(80, 24);
        assert!(screen.open_log_overlay(path.clone(), "走查".into(), None, String::new()));
        let before = screen.overlay_rows();
        let row = before
            .iter()
            .position(|row| row.contains("运行命令"))
            .expect("没有那一步");
        assert!(screen.overlay_toggle(row), "这一步点不开: {before:?}");
        let after = screen.overlay_rows();
        assert!(
            after.len() > before.len(),
            "展开之后没长高，等于点开是空的: {after:?}"
        );
        assert!(
            after.iter().any(|line| line.contains("total 12")),
            "工具的输出没进详情: {after:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    });
}

/// 后台面板：第一步是「差事」，点开看到派它出去时给的全文。
#[test]
fn job_panel_starts_with_the_prompt_it_was_given() {
    with_blocks(|| {
        let dir = std::env::temp_dir().join(format!("miyu-log-prompt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建目录");
        let path = dir.join("job.log");
        std::fs::write(
            &path,
            concat!(
                "[提示] \u{1}把 README 里的错别字挑出来\u{1}第二段要求\n",
                "[工具] run_command\t运行命令 · ls\n",
            ),
        )
        .expect("写日志");
        let mut screen = Screen::detached(80, 24);
        assert!(screen.open_log_overlay(path.clone(), "走查".into(), None, String::new()));
        let before = screen.overlay_rows();
        assert!(
            !before[0].contains("运行命令"),
            "差事那一步没排在最前面: {before:?}"
        );
        assert!(
            before.iter().any(|row| row.contains("运行命令")),
            "差事之后的那一步没了: {before:?}"
        );
        assert!(screen.overlay_toggle(0), "差事那一步点不开: {before:?}");
        let after = screen.overlay_rows();
        assert!(
            after.iter().any(|line| line.contains("错别字")),
            "差事的全文没出来: {after:?}"
        );
        assert!(
            after.iter().any(|line| line.contains("第二段要求")),
            "差事的第二段没出来（换行还原错了）: {after:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    });
}

/// 想的那一步：**行**是暗的，**展开的正文**才是绿的。
///
/// 反过来（抬头绿、正文白）是用户实测到的那一版：「浮层里思考行和思考展开
/// 内容的颜色反了」。主线那边一直是正文绿，两处得一个规矩。
#[test]
fn job_panel_paints_thinking_body_green_not_its_handle() {
    with_blocks(|| {
        let dir = std::env::temp_dir().join(format!("miyu-log-green-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建目录");
        let path = dir.join("job.log");
        std::fs::write(
            &path,
            "[思考] 先看一眼再说\n[工具] run_command\t运行命令 · ls\n",
        )
        .expect("写日志");
        let mut screen = Screen::detached(80, 24);
        assert!(screen.open_log_overlay(path.clone(), "走查".into(), None, String::new()));
        let rows = screen.overlay_rows();
        // 抬头不带窥视了（§6.3 第 1 项），按「已思考」定位。原文在展开里。
        let row = rows
            .iter()
            .position(|row| row.contains("已思考"))
            .expect("没有思考那一步");
        let head = &screen.overlay_rows_ansi()[row];
        assert!(!head.contains("38;5;10"), "思考那一行还是绿的: {head:?}");
        assert!(screen.overlay_toggle(row), "思考那一步点不开");
        let opened = screen.overlay_rows_ansi();
        assert!(
            opened.iter().any(|line| line.contains("38;5;10")
                && miyu_hosts::render::strip_ansi_text(line).contains("先看一眼再说")),
            "展开的思考正文不是绿的: {opened:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    });
}

/// 面板里每一行都得在框里。
///
/// 面板里的排版一度是按**整屏宽**算的，而框只有屏宽减六：行长出框外，画的时候
/// 被硬裁一刀，右边那根竖线就参差不齐（用户实测：右侧边框有些 broken）。
#[test]
fn job_panel_rows_stay_inside_the_frame() {
    with_blocks(|| {
        let dir = std::env::temp_dir().join(format!("miyu-log-frame-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建目录");
        let path = dir.join("job.log");
        let long = "这是一段很长的思考".repeat(30);
        std::fs::write(
            &path,
            format!(
                concat!(
                    "[提示] {long}\n",
                    "[思考] {long}\n",
                    "[工具] run_command\t运行命令 · {long}\n",
                    "[结果] run_command\t运行命令 ok · {long}\n",
                    "[输出] {long}\n",
                ),
                long = long
            ),
        )
        .expect("写日志");
        let cols = 100u16;
        let mut screen = Screen::detached(cols, 30);
        assert!(screen.open_log_overlay(path.clone(), "走查".into(), None, String::new()));
        // 面板里能写多宽：屏宽 − 左右各 2 列留白（没有竖线）。
        let inner = usize::from(cols) - 4;
        let over = |rows: &[String]| {
            rows.iter()
                .map(|row| miyu_hosts::render::visible_width(row))
                .filter(|width| *width > inner)
                .collect::<Vec<_>>()
        };
        let rows = screen.overlay_rows();
        assert!(over(&rows).is_empty(), "有行长出框外: {:?}", over(&rows));
        // 每一步点开之后也一样。
        for index in 0..rows.len() {
            screen.overlay_toggle(index);
        }
        let opened = screen.overlay_rows();
        assert!(
            over(&opened).is_empty(),
            "展开之后有行长出框外: {:?}",
            over(&opened)
        );
        let _ = std::fs::remove_dir_all(&dir);
    });
}

/// 面板一边刷新，已经点开的那块**不能自己缩回去**，而且里面的内容要跟着长。
///
/// 原来每次刷新都把展开表整张清掉：子代理一秒刷好几次，刚点开的东西立刻合上，
/// 根本读不了一句（用户实测：窥视的动态刷新会导致已展开内容缩起）。
#[test]
fn an_open_expansion_survives_a_panel_refresh() {
    with_blocks(|| {
        let mut screen = Screen::detached(80, 24);
        let step = blocks::register(vec!["  详情第一行".into(), "  详情第二行".into()])
            .expect("步那块没登记");
        let line = format!(
            "{}  ✳ 一步{}",
            blocks::begin_marker(step),
            blocks::END_MARKER
        );
        let panel =
            blocks::register_overlay("面板".into(), vec![line.clone()]).expect("面板没登记");
        assert!(screen.open_overlay(panel));

        let row = screen
            .overlay_rows()
            .iter()
            .position(|row| row.contains("一步"))
            .expect("没有那一步");
        assert!(screen.overlay_toggle(row), "点不开");
        assert!(
            screen
                .overlay_rows()
                .iter()
                .any(|row| row.contains("详情第一行")),
            "没展开"
        );

        // 面板内容变了（子代理又想了一句），展开的那块也换了新内容。
        blocks::update(
            step,
            String::new(),
            vec!["  详情第一行".into(), "  详情又长了".into()],
        );
        blocks::update(panel, String::new(), vec![line, "  ✳ 又一步".into()]);
        screen.overlay_refresh();
        let rows = screen.overlay_rows();
        assert!(
            rows.iter().any(|row| row.contains("详情第一行")),
            "刷新之后自己缩回去了: {rows:?}"
        );
        assert!(
            rows.iter().any(|row| row.contains("详情又长了")),
            "展开的内容没跟着长: {rows:?}"
        );
    });
}

/// 后台子代理开口说正文，面板里前面那几步也要收成一行。
///
/// 和前台那种面板同一个规矩——取数的地方不同，长相不该不同。
#[test]
fn job_panel_folds_its_steps_once_the_subagent_talks() {
    with_blocks(|| {
        let dir = std::env::temp_dir().join(format!("miyu-log-fold-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建目录");
        let path = dir.join("job.log");
        std::fs::write(
            &path,
            concat!(
                "[提示] 去看看目录\n",
                "[思考] 先想想\n",
                "[工具] run_command\t运行命令 · ls\n",
                "[结果] run_command\t运行命令 ok · ls\n",
                "[思考] 再想想\n",
                "[工具] run_command\t运行命令 · pwd\n",
                "[结果] run_command\t运行命令 ok · pwd\n",
                "[正文] 看完了，目录里有三个文件。\n",
            ),
        )
        .expect("写日志");
        let mut screen = Screen::detached(100, 30);
        assert!(screen.open_log_overlay(path.clone(), "走查".into(), None, String::new()));
        let rows = screen.overlay_rows();
        assert!(
            rows.iter().any(|row| row.contains("2 tools")),
            "没收成一行: {rows:?}"
        );
        assert!(
            !rows.iter().any(|row| row.contains("运行命令")),
            "收完之后那几步还平铺着: {rows:?}"
        );
        assert!(
            rows.iter().any(|row| row.contains("看完了")),
            "正文没进面板: {rows:?}"
        );
        // 「提示词」那一行钉在最前面，不参与收缩。
        assert!(
            rows[0].contains(miyu_base::i18n::text("prompt", "提示词")),
            "提示词被收进去了: {rows:?}"
        );
        // 收起来的那几步点开还在。
        let head = rows
            .iter()
            .position(|row| row.contains("2 tools"))
            .expect("没有收缩行");
        assert!(screen.overlay_toggle(head), "收缩行点不开");
        let opened = screen.overlay_rows();
        assert!(
            opened.iter().filter(|row| row.contains("运行命令")).count() >= 2,
            "点开之后那几步不见了: {opened:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    });
}

/// 浮层是**盖**上去的：开它不该把正文挪位置。
///
/// 跟随的落点一度按"面板上方剩下的高度"算，等于开一次面板就把正文整体往上顶
/// 半屏（用户实测：点击前台子代理打开的浮层会把内容往上推）。四处画面路径共用
/// `follow_target`，而它**和面板没关系**。
#[test]
fn opening_a_panel_does_not_move_the_body() {
    with_blocks(|| {
        let mut screen = Screen::detached(80, 30);
        let body = (0..120)
            .map(|index| format!("正文第 {index} 行\r\n"))
            .collect::<String>();
        screen.feed_for_test(body.as_bytes());
        let before = screen.follow_target();
        let panel = blocks::register_overlay(
            "面板".into(),
            (0..40).map(|index| format!("  第 {index} 步")).collect(),
        )
        .expect("面板没登记");
        assert!(screen.open_overlay(panel));
        assert_eq!(
            screen.follow_target(),
            before,
            "开了面板之后正文的落点变了——那就是把内容往上推"
        );
        assert!(screen.close_overlay());
        assert_eq!(screen.follow_target(), before, "关掉之后又变了");
    });
}

/// 后台面板：子代理说过话之后，没有 `[工具]` 行的 `[结果]`（Full 档下
/// `run_command` 的调用事件不发）不能把正文段或收缩行当成那次调用——原来
/// 收缩行被盖了个 `ok`，跟着的 `[输出]` 全贴进正文里，面板里一段话底下拖着
/// 几十行裸 grep 输出（用户实测截图）。
#[test]
fn job_panel_does_not_hang_tool_output_on_speech_or_the_fold() {
    with_blocks(|| {
        let dir = std::env::temp_dir().join(format!("miyu-log-speech-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建目录");
        let path = dir.join("job.log");
        std::fs::write(
            &path,
            concat!(
                "[思考] 先想想\n",
                "[工具] run_command\t运行命令 · ls\n",
                "[结果] run_command\t运行命令 ok · ls\n",
                "[思考] 再想想\n",
                "[工具] run_command\t运行命令 · pwd\n",
                "[结果] run_command\t运行命令 ok · pwd\n",
                "[正文] 只有一处发同步标记。接着审计其它状态。\n",
                "[结果] run_command\t运行命令 ok · grep -n Hide src\n",
                "[输出] 32:use crossterm::terminal::Clear\n",
                "[输出] 252:crossterm::cursor::Hide,\n",
            ),
        )
        .expect("写日志");
        let mut screen = Screen::detached(100, 30);
        assert!(screen.open_log_overlay(path.clone(), "走查".into(), None, String::new()));
        let rows = screen.overlay_rows();
        let fold = rows
            .iter()
            .find(|row| row.contains("2 tools"))
            .unwrap_or_else(|| panic!("没收成一行: {rows:?}"));
        assert!(!fold.contains("ok"), "收缩行被盖了个 ok: {fold:?}");
        // 裸输出不能平铺在面板里——它属于 grep 那一步，点开才看。
        assert!(
            !rows.iter().any(|row| row.contains("32:use crossterm")),
            "工具输出贴到正文里了: {rows:?}"
        );
        let grep = rows
            .iter()
            .position(|row| row.contains("grep -n Hide"))
            .unwrap_or_else(|| panic!("没有 `[工具]` 行的结果没自己立一步: {rows:?}"));
        assert!(screen.overlay_toggle(grep), "那一步点不开");
        let opened = screen.overlay_rows();
        assert!(
            opened.iter().any(|row| row.contains("32:use crossterm")),
            "输出没挂到它自己那一步上: {opened:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    });
}

/// 后台日志面板：收缩行点开是时间线（抬头底下接连线、各步和抬头同一列），不到
/// 十分之一秒的耗时不报，还没回来的那一步左边距上是转轮占位格。
#[test]
fn a_log_fold_opens_into_a_timeline_and_the_running_step_spins() {
    with_blocks(|| {
        let dir = std::env::temp_dir().join(format!("miyu-log-fold-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建目录");
        let path = dir.join("job.log");
        std::fs::write(
            &path,
            concat!(
                "[思考] 0.0s\t先想想\n",
                "[工具] run_command\t运行命令 · ls\n",
                "[结果] run_command\t运行命令 ok · 0.0s · ls\n",
                "[工具] run_command\t运行命令 · pwd\n",
                "[结果] run_command\t运行命令 ok · 1.2s · pwd\n",
                "[正文] 说完了。\n",
                "[工具] run_command\t运行命令 · sleep 5\n",
            ),
        )
        .expect("写日志");
        let mut screen = Screen::detached(100, 30);
        assert!(screen.open_log_overlay(path.clone(), "走查".into(), None, String::new()));
        let rows = screen.overlay_rows();
        assert!(
            !rows.iter().any(|row| row.contains("0.0s")),
            "报了个 0.0s: {rows:?}"
        );
        let fold = rows
            .iter()
            .position(|row| row.contains("2 tools"))
            .unwrap_or_else(|| panic!("没收成一行: {rows:?}"));
        // 合着是 `›`，点开翻成 `⌄`——和主线那条一样。
        assert!(
            rows[fold].trim_start().starts_with('›'),
            "合着的收缩行不是 ›: {:?}",
            rows[fold]
        );
        assert!(screen.overlay_toggle(fold), "收缩行点不开");
        let opened = screen.overlay_rows();
        assert!(
            opened[fold].trim_start().starts_with('⌄'),
            "点开的收缩行不是 ⌄: {:?}",
            opened[fold]
        );
        let column = |line: &str| line.chars().take_while(|c| *c == ' ').count();
        let head_col = column(&opened[fold]);
        assert_eq!(opened[fold + 1].trim(), "│", "抬头底下不是连线: {opened:?}");
        // 「已思考」而不是窥视原文（§6.3 第 1 项）。
        for needle in ["已思考", "· ls", "1.2s · pwd"] {
            let row = opened
                .iter()
                .find(|row| row.contains(needle))
                .unwrap_or_else(|| panic!("收起来的 {needle} 不见了: {opened:?}"));
            assert_eq!(
                column(row),
                head_col,
                "{needle} 那一步没和抬头同一列: {opened:?}"
            );
        }
        let running = screen
            .overlay_rows_ansi()
            .into_iter()
            .find(|row| row.contains("sleep 5"))
            .unwrap_or_else(|| panic!("跑着的那一步不见了"));
        assert!(
            running.contains(miyu_hosts::render::timeline::LIVE_SPINNER_CELL),
            "跑着的那一步没有转轮占位: {running:?}"
        );
        // 命令那一步点开：正文第一段是命令本身，不是把抬头再说一遍。
        let pwd = opened
            .iter()
            .position(|row| row.contains("1.2s · pwd"))
            .expect("pwd 那一步不见了");
        assert!(screen.overlay_toggle(pwd), "pwd 那一步点不开");
        let deep = screen.overlay_rows();
        assert!(
            deep.iter().any(|row| row.trim() == "pwd"),
            "点开没有命令本身: {deep:?}"
        );
        assert_eq!(
            deep.iter().filter(|row| row.contains("运行命令")).count(),
            3,
            "抬头被再说了一遍: {deep:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    });
}

/// 日志末尾的 `[统计]` 行不是工具调用：不能被当成「还没回来的那个调用」标成
/// 运行中、挂上转轮（测具截图：`⠏ 工具调用 3 次　消耗词元 484 · 运行中`）。
#[test]
fn a_trailing_stats_line_is_not_a_running_step() {
    with_blocks(|| {
        let dir = std::env::temp_dir().join(format!("miyu-log-stats-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建目录");
        let path = dir.join("job.log");
        std::fs::write(
            &path,
            concat!(
                "[工具] run_command\t运行命令 · ls\n",
                "[结果] run_command\t运行命令 ok · 1.2s · ls\n",
                "[统计] 工具调用 1 次　消耗词元 84\n",
            ),
        )
        .expect("写日志");
        let mut screen = Screen::detached(100, 30);
        assert!(screen.open_log_overlay(path.clone(), "走查".into(), None, String::new()));
        let rows = screen.overlay_rows_ansi();
        let stats = rows
            .iter()
            .find(|row| row.contains("工具调用 1 次"))
            .unwrap_or_else(|| panic!("统计那一行不见了: {rows:?}"));
        assert!(!stats.contains("运行中"), "统计行被标成运行中: {stats:?}");
        assert!(
            !stats.contains(miyu_hosts::render::timeline::LIVE_SPINNER_CELL),
            "统计行挂了转轮: {stats:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    });
}

/// 浮层里的字要选得动、复制得走。
///
/// 面板原来整个不做选区（`tail_impl` 里那句「其余吞掉」），而它装的正是最想复制
/// 走的东西：子代理的输出、后台命令的日志（用户 09-17：「这样的浮层无法选中
/// 文字」）。
///
/// 「原地点一下 = 开合这一块 / 拖过 = 选区复制」共用一次按下-松开，只能靠有没有
/// 拖动来分——这条也一起钉住。
#[test]
fn the_overlay_can_select_and_copy_text() {
    with_blocks(|| {
        let panel = blocks::register_overlay(
            "面板".into(),
            vec!["  第一行内容".into(), "  第二行内容".into()],
        )
        .expect("面板没登记");
        let mut screen = Screen::detached(80, 24);
        assert!(screen.open_overlay(panel));

        // 从第 0 行第 2 列拖到第 1 行行尾。
        screen.overlay_select_span((0, 2), (1, u16::MAX));
        assert!(
            screen.overlay_select_finish().is_none(),
            "拖过了就不该当成点击"
        );
        let copied = screen.take_pending_copy().expect("什么都没进剪贴板");
        // 左边那两列缩进算"装饰"，不进剪贴板——和正文那侧一个规矩：
        // 把竖条和缩进也复制走的话，粘出去还得手动删一遍。
        assert_eq!(copied, "第一行内容\n第二行内容");

        // 原地点一下：不进剪贴板，交回给"开合这一块"。
        screen.overlay_select_span((0, 4), (0, 4));
        assert!(
            screen.overlay_select_finish().is_some(),
            "原地点一下该当成点击"
        );
        assert!(screen.take_pending_copy().is_none(), "点一下不该复制东西");
    });
}

/// 后台任务面板也跟着那两个「展开」开关走。
///
/// 它是从日志/标记流攒步的，手上原来没有配置入口——于是同一件事在前台面板跟着
/// 开关走、在后台面板永远收着（用户 09-17：「子代理浮层中的流式输出不受我们之前
/// 做的三个开关影响」）。
#[test]
fn the_job_panel_follows_the_expand_switches() {
    with_blocks(|| {
        let dir = std::env::temp_dir().join(format!("miyu-log-expand-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建目录");
        let path = dir.join("job.log");
        std::fs::write(
            &path,
            ["[提示] 去看看", "[思考] 1.2s\t先想一句，再动手。"].join("\n") + "\n",
        )
        .expect("写日志");

        // 收起档：只有抬头。
        let mut collapsed = Screen::detached(100, 40);
        assert!(collapsed.open_log_overlay(path.clone(), "走查".into(), None, String::new()));
        let rows = collapsed.overlay_rows();
        assert!(
            rows.iter().any(|row| row.contains("已思考")),
            "丢了思考那一步: {rows:?}"
        );
        assert!(
            !rows.iter().any(|row| row.contains("先想一句")),
            "收起档不该把正文摆出来: {rows:?}"
        );

        // 展开档：出来就是展开的，一次都不用点。
        let mut expanded = Screen::detached(100, 40);
        expanded.set_display_expand(true, true, true, 8, 10);
        assert!(expanded.open_log_overlay(path.clone(), "走查".into(), None, String::new()));
        let rows = expanded.overlay_rows();
        assert!(
            rows.iter().any(|row| row.contains("先想一句")),
            "展开档下面板里那一步没展开: {rows:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    });
}

/// 浮层里的 tag 行也要有悬浮提亮。
///
/// 面板原来把 `Moved` 和别的鼠标事件一起吞掉了，于是「哪儿能点」全靠猜——而正文
/// 那侧一直是有的（用户 09-17：「浮层的 tag 行没有悬浮变色的效果」）。
///
/// 提亮的是**整块**不是一行：一步的抬头和它露出来的几行是同一件事。
#[test]
fn the_overlay_highlights_the_row_under_the_mouse() {
    with_blocks(|| {
        let step = blocks::register(vec!["  详情".into()]).expect("步那块没登记");
        let line = format!(
            "{}  ✳ 一步{}",
            blocks::begin_marker(step),
            blocks::END_MARKER
        );
        let panel = blocks::register_overlay("面板".into(), vec!["  抬头".into(), line])
            .expect("面板没登记");
        let mut screen = Screen::detached(80, 24);
        assert!(screen.open_overlay(panel));

        let row = screen
            .overlay_rows()
            .iter()
            .position(|row| row.contains("一步"))
            .expect("没有那一步");
        assert!(screen.overlay_hover_index(Some(row)), "悬浮没认出来");
        assert_eq!(screen.hovered_overlay_block(), Some(step), "认错了块");
        // 同一块上不动：不该每次移动都重画。
        assert!(
            !screen.overlay_hover_index(Some(row)),
            "同一块又报了一次变化"
        );
        // 移到不能点的行上：提亮撤掉。
        assert!(screen.overlay_hover_index(None), "离开没撤提亮");
        assert_eq!(screen.hovered_overlay_block(), None);
    });
}

/// 后台任务面板也跟着「过程收起成 Worked for」走。
///
/// 浮层原来是无条件收段的，那个开关在浮层里等于不存在（用户 09-17：「子代理浮层
/// 也不受 `过程收起成 worked for` 这个开关的影响」）。
#[test]
fn the_job_panel_follows_the_fold_switch() {
    with_blocks(|| {
        let dir = std::env::temp_dir().join(format!("miyu-log-fold-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建目录");
        let path = dir.join("job.log");
        std::fs::write(
            &path,
            [
                "[提示] 去看看",
                "[思考] 1.2s\t先想一句。",
                "[工具] run_command\t运行命令 · ls",
                "[结果] run_command\t运行命令 ok · 0.3s · ls",
                // 它开口说话 = 前面那一段该收成 `Worked for …`（开着的话）。
                "[正文] 看完了。",
            ]
            .join("\n")
                + "\n",
        )
        .expect("写日志");

        let mut folded = Screen::detached(100, 40);
        assert!(folded.open_log_overlay(path.clone(), "走查".into(), None, String::new()));
        assert!(
            folded
                .overlay_rows()
                .iter()
                .any(|row| row.contains("Worked for")),
            "开着收段却没收: {:?}",
            folded.overlay_rows()
        );

        let mut open = Screen::detached(100, 40);
        open.set_display_expand(false, false, false, 8, 10);
        assert!(open.open_log_overlay(path.clone(), "走查".into(), None, String::new()));
        let rows = open.overlay_rows();
        assert!(
            !rows.iter().any(|row| row.contains("Worked for")),
            "关了收段还是收了: {rows:?}"
        );
        assert!(
            rows.iter().any(|row| row.contains("已思考")),
            "不收段那几步该就地留着: {rows:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    });
}

/// 命令那一步：抬头底下露的那几行要**在块里面**。
///
/// 面板原来自己拼 `begin_marker + line + END`、尾巴另发一项，尾巴因此落在块
/// 外面：点开时展开内容只换掉抬头那一行，命令于是出现两遍；收成 `Worked for`
/// 再点开时尾巴干脆没了（用户 09-17 逐条报的 1/2/3）。改成走主线那份
/// `step_rows`——「一步占哪几行」只该有一个实现。
#[test]
fn the_command_preview_lives_inside_the_clickable_block() {
    with_blocks(|| {
        let dir = std::env::temp_dir().join(format!("miyu-log-cmdtail-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建目录");
        let path = dir.join("job.log");
        // 日志那条路没有参数，露不出尾巴——这条要的是**标记流**那条路。
        let args =
            serde_json::json!({ "command": "echo 甲; echo 乙", "title": "看看" }).to_string();
        let call = serde_json::json!({
            "name": "run_command",
            "display": "运行命令",
            "args": args,
        })
        .to_string();
        std::fs::write(&path, "[提示] 去看看\n").expect("写日志");
        let mut screen = Screen::detached(100, 40);
        assert!(screen.open_log_overlay(path.clone(), "走查".into(), None, String::new()));
        screen.overlay_feed_markers_for_test(&[format!("__subtool_call__{call}")]);

        let rows = screen.overlay_rows();
        let shown = rows.iter().filter(|row| row.contains("echo 甲")).count();
        assert_eq!(shown, 1, "命令该露一遍: {rows:?}");
        let head = rows
            .iter()
            .position(|row| row.contains("看看"))
            .unwrap_or_else(|| panic!("没有那一步: {rows:?}"));
        // 点开：展开内容把抬头**和尾巴一起**换掉，不该冒出第二份命令。
        assert!(screen.overlay_toggle(head), "点不开");
        let rows = screen.overlay_rows();
        let shown = rows.iter().filter(|row| row.contains("echo 甲")).count();
        assert_eq!(shown, 1, "点开之后命令出现了两遍: {rows:?}");
        let _ = std::fs::remove_dir_all(&dir);
    });
}

/// 一份「正文在前、工具在后」的流水账。**一行一条写，别用字符串续行**——
/// rustfmt 会把续行的缩进拼进字符串里，`[正文]` 前面多出十几个空格就认不出标签，
/// 整条日志会静静地退化成「一堆无标签续行」。
const LOG_WITH_SPEECH_FIRST: &[&str] = &[
    "[提示] 去看看那个目录",
    "[正文] 我先看一眼再说。",
    "[工具] run_command\t运行命令 · ls",
    "[结果] run_command\t运行命令 ok · 0.3s · ls",
    "[输出] total 0",
];

/// 子代理**开过口之后**，后面那几步点开还得留得住。
///
/// 块 id 是按位置记的（`step_blocks[i]`），而正文段不挂块——它一度直接
/// `continue`，于是那张表的长度停在正文段那一格，后面每一步都记不进去。每帧
/// `to_step` 因此拿不到旧 id，`blocks::register` 现发一个新的，展开表按 id 记，
/// 下一帧就对不上了：**点开一瞬间自己合上**（用户 09-17 实测截图）。
#[test]
fn a_step_after_the_subagent_speaks_stays_open_across_refreshes() {
    with_blocks(|| {
        let dir =
            std::env::temp_dir().join(format!("miyu-log-after-speech-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建目录");
        let path = dir.join("job.log");
        let write = |extra: &[&str]| {
            let mut lines = LOG_WITH_SPEECH_FIRST.to_vec();
            lines.extend_from_slice(extra);
            std::fs::write(&path, lines.join("\n") + "\n").expect("写日志");
        };
        write(&[]);
        let mut screen = Screen::detached(100, 40);
        assert!(screen.open_log_overlay(path.clone(), "走查".into(), None, String::new()));
        let row = screen
            .overlay_rows()
            .iter()
            .position(|row| row.contains("运行命令"))
            .unwrap_or_else(|| panic!("没有工具那一步: {:#?}", screen.overlay_rows()));
        assert!(screen.overlay_toggle(row), "点不开");
        assert!(
            screen
                .overlay_rows()
                .iter()
                .any(|row| row.contains("total 0")),
            "没展开: {:?}",
            screen.overlay_rows()
        );

        // 日志又长了一行，面板重读一遍——展开的那一块不该跟着没。
        // 面板重读有 150ms 的节流，睡过它。
        std::thread::sleep(std::time::Duration::from_millis(200));
        write(&["[思考] 空的，那就不用往下翻了。"]);
        screen.overlay_refresh();
        let rows = screen.overlay_rows();
        assert!(
            rows.iter().any(|row| row.contains("total 0")),
            "刷新之后自己合上了（块 id 每帧在换）: {rows:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    });
}

/// `[正文+]` 是**一截**，不是一行：读日志时粘回上一条，不另起一行。
///
/// 后台面板优先订 `job.trace` 里的原始标记流，而那条流是**逐 delta** 的——一条
/// 记录常常只有一个词。当成"一条 = 一行"的话，正文就是一句一个台阶
///（用户 09-17：「子代理的浮层的正文现在是每个 token 都会换一次行」）。
#[test]
fn speech_continuation_records_are_glued_back_into_one_line() {
    with_blocks(|| {
        let dir = std::env::temp_dir().join(format!("miyu-log-speech-glue-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建目录");
        let path = dir.join("job.log");
        // 逐 delta 落下来的四条：两头的空白就是词边界，拼回去要一字不差。
        std::fs::write(
            &path,
            "[正文] The quick \n[正文+] brown \n[正文+] fox jumps.\n[正文] Next paragraph.\n",
        )
        .expect("写日志");
        let mut screen = Screen::detached(100, 40);
        assert!(screen.open_log_overlay(path.clone(), "走查".into(), None, String::new()));
        let rows = screen.overlay_rows();
        assert!(
            rows.iter()
                .any(|row| row.contains("The quick brown fox jumps.")),
            "逐 delta 的正文没粘回一行: {rows:?}"
        );
        // 不带 `+` 的那条才是新的一行。
        assert!(
            !rows
                .iter()
                .any(|row| row.contains("fox jumps.Next paragraph.")),
            "不带 `+` 的那条不该粘上去: {rows:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    });
}

/// 桥按自然段落盘 `[正文]`，一段里的换行原样写着（标题、表格行、列表项）：
/// 读日志时这些续行归到那一段里，不能丢（用户实测截图：整段缺句子、表格只剩表头）。
#[test]
fn a_multi_line_speech_paragraph_keeps_its_continuation_lines() {
    with_blocks(|| {
        let dir =
            std::env::temp_dir().join(format!("miyu-log-speech-lines-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建目录");
        let path = dir.join("job.log");
        std::fs::write(
            &path,
            "[工具] run_command\t运行命令 · ls\n[结果] run_command\t运行命令 ok · 0.3s · ls\n[正文] ## Final report\n### Tool calls made\nBreakdown of the calls: three reads.\n[正文] | Metric | Value |\n|---|---|\n| calls | 10 |\n",
        )
        .expect("写日志");
        let mut screen = Screen::detached(100, 40);
        assert!(screen.open_log_overlay(path.clone(), "走查".into(), None, String::new()));
        let rows = screen.overlay_rows();
        for needle in ["Tool calls made", "three reads", "calls"] {
            assert!(
                rows.iter().any(|row| row.contains(needle)),
                "续行 {needle} 丢了: {rows:?}"
            );
        }
        // 表格按面板宽度画成了框，不是裸的竖线。
        assert!(
            rows.iter()
                .any(|row| row.contains('┌') || row.contains('│')),
            "表格没画成框: {rows:?}"
        );
        assert!(
            !rows
                .iter()
                .any(|row| row.trim_start().starts_with("| Metric")),
            "表格还是裸的: {rows:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    });
}

/// 后台面板末尾的 `[准备] <工具>\t<提示>`：图标是那个工具自己的（准备编辑=铅笔）。
#[test]
fn a_log_preparing_row_wears_the_tools_own_glyph() {
    with_blocks(|| {
        let dir = std::env::temp_dir().join(format!("miyu-log-prep-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建目录");
        let path = dir.join("job.log");
        std::fs::write(
            &path,
            "[工具] run_command\t运行命令 · ls\n[结果] run_command\t运行命令 ok · 0.3s · ls\n[准备] edit\t准备编辑\n",
        )
        .expect("写日志");
        let mut screen = Screen::detached(100, 30);
        assert!(screen.open_log_overlay(path.clone(), "走查".into(), None, String::new()));
        let rows = screen.overlay_rows();
        let row = rows
            .iter()
            .find(|row| row.contains("准备编辑"))
            .unwrap_or_else(|| panic!("没有准备那一行: {rows:?}"));
        assert!(
            row.contains(miyu_hosts::render::tool_glyph_for("edit")),
            "准备编辑没挂铅笔: {row:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    });
}

/// 后台**命令**的浮层要在日志上面原样铺一行在跑的命令:标题栏那个 title 是短
/// 标签(模型给了就只有 16 字符),看不出真正在跑什么(用户 09-14)。
#[test]
fn job_panel_shows_the_command_it_is_running() {
    with_blocks(|| {
        let dir = std::env::temp_dir().join(format!("miyu-log-cmd-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建目录");
        let path = dir.join("job.log");
        // 纯输出,没有 [思考]/[工具] 这些标记——后台命令的日志就长这样。
        std::fs::write(&path, "total 12\ndrwxr-xr-x 2 shorin\n").expect("写日志");
        let mut screen = Screen::detached(80, 24);
        assert!(screen.open_log_overlay(
            path.clone(),
            "走查".into(),
            None,
            "seq 1 5 | sort -r".to_string(),
        ));
        let rows = screen.overlay_rows();
        assert!(
            rows.iter().any(|row| row.contains("$ seq 1 5 | sort -r")),
            "浮层没显示在跑的命令: {rows:?}"
        );
        assert!(
            rows.iter().any(|row| row.contains("drwxr-xr-x")),
            "日志正文没了: {rows:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    });
}
