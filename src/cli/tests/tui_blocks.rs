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

/// 开一个后台命令的日志面板：日志写进临时文件（面板直接读盘，见 `overlay::LogFile`）。
/// 返回的目录要活到测试结束。
fn open_log_panel(screen: &mut Screen, name: &str, lines: &[String]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("临时目录");
    let path = dir.path().join(format!("{name}.log"));
    std::fs::write(&path, lines.join("\n") + "\n").expect("写日志");
    assert!(screen.open_log_overlay(path, "面板".into(), None, String::new()));
    dir
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

/// 贴着底的时候点开靠下的一块：点的那一行留在屏幕原处，展开的内容往下长，先看到的是
/// 开头（用户 09-26：点开收缩行居然是往上展开的，看到的是尾巴不是前面那几步）。原来展开
/// 前贴底就一直贴底，撑出来的行全堆到了点的那一行上面。
#[test]
fn expanding_near_the_bottom_grows_downward_from_the_clicked_row() {
    with_blocks(|| {
        let mut body = vec!["头".to_string()];
        body.extend((0..40).map(|i| format!("详情 {i}")));
        let id = blocks::register(body).expect("已开启");
        let mut screen = Screen::detached(80, 30);
        let mut text: String = (0..60).map(|i| format!("第 {i} 行\r\n")).collect();
        text.push_str(&format!(
            "{}折叠{}\r\n回复正文\r\n",
            blocks::begin_marker(id),
            blocks::END_MARKER
        ));
        screen.feed_for_test(text.as_bytes());
        screen.prepare_frame(6);
        assert!(screen.following());
        let header = (0..screen.view_len())
            .find(|&index| view_text(&screen, index) == "折叠")
            .expect("收缩行在视图里");
        let on_screen = header - screen.scroll_for_test();

        assert!(screen.toggle_block(id));
        screen.prepare_frame(6);
        let top = screen.scroll_for_test();
        assert_eq!(
            view_text(&screen, top + on_screen),
            "头",
            "点的那一行挪了位置"
        );
        assert_eq!(view_text(&screen, top + on_screen + 1), "详情 0");
        assert!(!screen.following(), "展开的内容还在视口下面，不该说在跟底");

        // 收起来就回到原样，也重新跟底。
        assert!(screen.toggle_block(id));
        screen.prepare_frame(6);
        assert!(screen.following());
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

// ---- 登记处 ------------------------------------------------------------

#[test]
fn a_block_picks_up_streamed_updates() {
    with_blocks(|| {
        let id = blocks::register(vec!["第一行".into()]).expect("已开启");
        let before = blocks::version(id);
        blocks::update(id, vec!["第一行".into(), "第二行".into()]);
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
        let lines: Vec<String> = (0..40).map(|index| format!("第 {index} 行输出")).collect();
        let _log = open_log_panel(&mut screen, "panel-body", &lines);
        assert_eq!(
            screen.follow_target(),
            before,
            "开了面板之后正文的落点变了——那就是把内容往上推"
        );
        assert!(screen.close_overlay());
        assert_eq!(screen.follow_target(), before, "关掉之后又变了");
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
        let mut screen = Screen::detached(80, 24);
        let lines = ["第一行内容".to_string(), "第二行内容".to_string()];
        let _log = open_log_panel(&mut screen, "select", &lines);

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
