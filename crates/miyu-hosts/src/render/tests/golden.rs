//! 渲染层的**逐字节快照**：一份固定事件脚本，在三个 surface 下各跑一遍，
//! 输出存进 `golden/*.ansi`，此后每次都必须逐字节相同。
//!
//! 为什么要它：过程渲染今天分散在四个模式位派生出的几条路上
//! （`docs/plan/2026-09-17-render-unification.md` §1.1），而那些路上的每一处
//! 细节几乎都是某次用户实测的结论。要动那个结构，先得有一张网接住"我没改变
//! 任何人看到的东西"——AGENTS §5.1 的「先证明现象」在重构里的形态就是它。
//!
//! 它同时是**拆 crate 的安全网**：那边只用 `request_shape_probe` 验了提示词
//! 字节，渲染输出一个字节都没验过。534 个文件搬完之后拿这套跑一遍，就知道
//! 有没有动过像素。
//!
//! 时间要打掩码：耗时来自真实时钟，不掩码的话每次都不一样。掩码只吃
//! `1.2s` / `1m 05s` 这两种形状，别的字节一律照原样比。
//!
//! 改动 golden 的唯一正当理由：**你确实想改那个表现**，并且在提交里单独说明
//! 改了哪一段、为什么。跑 `MIYU_GOLDEN_WRITE=1 cargo test --lib render::tests::golden`
//! 重写，然后**逐行读 diff**。

use super::timeline::with_blocks;
use crate::render::{ReasoningDisplayMode, StreamRenderer, ToolCallDisplayMode};
use miyu_core::llm::{ChatStreamChunk, ChatStreamKind};

/// 三个值得钉住的 surface（`docs/plan/2026-09-17-render-unification.md` §1.1）。
/// S0/S2/S5 不钉：S0 只有正文，S2/S5 是老卡片与谓词组合出来的混合态，本就要
/// 在能力位那一步被收拾掉。
#[derive(Clone, Copy)]
enum Surface {
    /// stdout 接管道：老的一行摘要，无转轮无时间线。
    Pipe,
    /// shellhook / 单次 CLI / inline REPL：静态时间线，每步落 scrollback。
    Static,
    /// 全屏 TUI：可展开时间线 + `Worked for` 收缩。
    Full,
    /// 全屏 + 关掉「收起成 Worked for」：每一步就地落下去、段末不收，**照样
    /// 点得开**（用户 todolist:21 / 09-17）。
    FullOpen,
    /// 全屏 + 两个「展开」开关都开着：每一步出来就是展开态。
    FullExpanded,
}

impl Surface {
    fn name(self) -> &'static str {
        match self {
            Surface::Pipe => "s1-pipe",
            Surface::Static => "s3-static",
            Surface::Full => "s4-full",
            Surface::FullOpen => "s4-full-open",
            Surface::FullExpanded => "s4-full-expanded",
        }
    }

    fn renderer(self) -> StreamRenderer {
        let expanded = matches!(self, Surface::FullExpanded);
        let mut renderer = StreamRenderer::new(
            if expanded {
                ReasoningDisplayMode::Full
            } else {
                ReasoningDisplayMode::Summary
            },
            if expanded {
                ToolCallDisplayMode::Full
            } else {
                ToolCallDisplayMode::Summary
            },
            false,
            true,
            8,
        );
        renderer.use_external_cursor_control();
        renderer.use_buffered_output();
        // `live_summary` 出厂取 `stdout().is_terminal()`,`cargo test` 下是 false。
        // 三个 surface 的差别全在这一位与块开关上,显式摆出来比继承环境可靠。
        renderer.live_summary = !matches!(self, Surface::Pipe);
        renderer.fold_timeline = !matches!(self, Surface::FullOpen);
        renderer
    }
}

/// 脚本：一轮里把过程渲染的各条分支都走一遍。
///
/// 顺序刻意贴近真实回合：先想，再写清单（它现在是一段的句点），再跑命令（抬头
/// 给 title、底下给命令），再编辑（抬头给 `+N -M`），再派一个子代理（面板），
/// 中间插一段正文（它会切段），最后收尾。
fn run_script(renderer: &mut StreamRenderer) {
    // 每一步之间歇一下:`timed_label` 对不到十分之一秒的耗时**不打秒数**,于是
    // 同一段代码两次跑可能一次带 `· 0.1s` 一次不带——那不是掩码能救的,是结构性
    // 的不确定。睡过那个阈值,秒数就稳定出现(值仍被掩码吃掉)。
    let tick = || std::thread::sleep(std::time::Duration::from_millis(120));
    let say = |renderer: &mut StreamRenderer, kind: ChatStreamKind, text: &str| {
        renderer
            .write_chunk(ChatStreamChunk {
                kind,
                text: text.to_string(),
            })
            .unwrap();
    };

    say(renderer, ChatStreamKind::Reasoning, "先看一眼需求，");
    tick();
    say(renderer, ChatStreamKind::Reasoning, "再决定怎么下手。");

    // 清单:一段的句点(09-16)。表格走 `__todo_table__` 侧信道。
    renderer
        .write_tool_call("todowrite", r#"{"todos":[]}"#)
        .unwrap();
    renderer
        .write_tool_progress(
            "todowrite",
            r#"__todo_table__{"todos":[{"content":"列清单","status":"completed","priority":"medium"},{"content":"跑命令","status":"in_progress","priority":"medium"},{"content":"收尾","status":"pending","priority":"medium"}]}"#,
        )
        .unwrap();
    tick();
    renderer
        .write_tool_result("todowrite", true, "todo list updated")
        .unwrap();

    // 命令:抬头给 title,底下给命令本身(09-17)。
    renderer
        .write_tool_call(
            "run_command",
            r#"{"command":"printf '第一行\n'\nprintf '第二行\n'","title":"看看输出"}"#,
        )
        .unwrap();
    renderer
        .write_command_output(
            "run_command",
            miyu_engine::tools::CommandOutputStream::Stdout,
            "第一行\n第二行\n".as_bytes(),
        )
        .unwrap();
    tick();
    renderer
        .write_tool_result("run_command", true, r#"{"success":true,"exit_code":0}"#)
        .unwrap();

    // 编辑:抬头给 `+N -M`(09-17),详情给 diff。
    renderer
        .write_tool_call(
            "edit",
            "{\"patchText\":\"*** Begin Patch\\n*** Update File: /tmp/golden.txt\\n@@\\n-旧的一行\\n+新的一行\\n+又一行\\n*** End Patch\\n\"}",
        )
        .unwrap();
    renderer
        .write_tool_progress(
            "edit",
            r#"__patch_preview__{"path":"/tmp/golden.txt","diff":"--- a/golden.txt\n+++ b/golden.txt\n@@ -1,1 +1,2 @@\n-旧的一行\n+新的一行\n+又一行\n"}"#,
        )
        .unwrap();
    tick();
    renderer
        .write_tool_result("edit", true, r#"{"path":"/tmp/golden.txt","applied":true}"#)
        .unwrap();

    // 提问:它也是一步。静态面把一问一答写成这一步的正文,全屏另写一块独立竖条
    // ——同一份文案两个长相,真实调用顺序就是这两句挨着(`cli/question_panel.rs`)。
    let request = miyu_base::question::QuestionRequest {
        questions: vec![
            miyu_base::question::QuestionPrompt {
                header: "今晚的打算".into(),
                question: "接下来先做哪一件？".into(),
                options: vec![
                    miyu_base::question::QuestionOption {
                        label: "只是测工具".into(),
                        description: "不改代码".into(),
                    },
                    miyu_base::question::QuestionOption {
                        label: "真的改".into(),
                        description: "动手".into(),
                    },
                ],
                multiple: false,
                custom: true,
            },
            miyu_base::question::QuestionPrompt {
                header: "要不要提交".into(),
                question: "改完直接提交吗？".into(),
                options: vec![miyu_base::question::QuestionOption {
                    label: "先看看".into(),
                    description: "不提交".into(),
                }],
                multiple: false,
                custom: true,
            },
        ],
    };
    let answered = miyu_base::question::QuestionResponse::Answered(vec![
        vec!["只是测工具".to_string()],
        vec!["先看看".to_string()],
    ]);
    renderer
        .timeline_push_question(&request, &answered)
        .unwrap();
    renderer
        .write_question_exchange(&request, &answered)
        .unwrap();
    tick();

    // 子代理:状态行那一行(会话项目第 4 段之二:前台走 `subagent.progress`,窥视、词元、
    // 子会话;过程在子会话里,父会话不再画一遍)。
    renderer
        .write_tool_call(
            "subagent",
            r#"{"description":"查目录","prompt":"去看看那个目录里有什么"}"#,
        )
        .unwrap();
    let status = |peek: &str, tokens: u64| miyu_engine::tools::subagent::status::SubagentStatus {
        peek: peek.to_string(),
        tokens_label: tokens.to_string(),
        tokens,
        session_id: Some("sess_golden_child".to_string()),
    };
    renderer.write_subagent_status("subagent", status("先列一下。", 150));
    tick();
    renderer.write_subagent_status("subagent", status("运行命令 · 列目录", 300));
    tick();
    renderer.write_subagent_status("subagent", status("里面是空的。", 450));
    tick();
    renderer
        .write_tool_result("subagent", true, "子代理跑完了")
        .unwrap();

    // 正文:它是时间线的分段点。
    say(renderer, ChatStreamKind::Content, "跑完了，");
    say(renderer, ChatStreamKind::Content, "结果如上。");

    // 失败那一步也要钉:跑砸的命令整段标红。
    renderer
        .write_tool_call("run_command", r#"{"command":"exit 3","title":"故意跑砸"}"#)
        .unwrap();
    renderer
        .write_command_output(
            "run_command",
            miyu_engine::tools::CommandOutputStream::Stderr,
            "出错了\n".as_bytes(),
        )
        .unwrap();
    tick();
    renderer
        .write_tool_result("run_command", false, r#"{"success":false,"exit_code":3}"#)
        .unwrap();

    renderer.finish().unwrap();
}

/// 把会变的东西换成固定占位:耗时、词元数、每秒词元。
///
/// 只吃这几种形状,别的字节原样比——掩码越宽,网眼越大。
fn mask_volatile(raw: &str) -> String {
    let mut out = raw.to_string();
    for (pattern, replacement) in [
        (r"\d+m \d\ds", "<M>m <S>s"),
        (r"\d+\.\ds", "<N>s"),
        (r"<?\d+ms", "<N>ms"),
        // 块 id 是全局自增计数,随整个测试进程里谁先跑而变——它不是表现。
        // 两种起始标记都要掩：`-open=` 是「出来就是展开态」那一档。
        (r"miyu-block-open=\d+", "miyu-block-open=<ID>"),
        (r"miyu-block=\d+", "miyu-block=<ID>"),
        (r"\d+(\.\d+)? tok/s", "<N> tok/s"),
        (r"\d+ 词元", "<N> 词元"),
        (r"\d+ tokens", "<N> tokens"),
        // 时长连**单位**一起掩掉。上面几条只把数字换成 <N>,可 `Worked for`
        // 写的是 `123ms` 还是 `1.2s`,取决于跑这份测试的机器有多快——runner 上
        // 同一段落到 `<N>s`,本机是 `<N>ms`,于是 golden 记住的是「谁录的、他机器
        // 多快」(09-23 macOS CI)。时长来自 `Instant::now().elapsed()`,测试控制
        // 不了,只能掩。**版式和顺序照样冻着**,掩掉的只有那个数和它的单位。
        (r"<M>m <S>s", "<DUR>"),
        (r"<N>ms", "<DUR>"),
        (r"<N>s", "<DUR>"),
    ] {
        out = fancy_regex::Regex::new(pattern)
            .expect("掩码正则")
            .replace_all(&out, replacement)
            .into_owned();
    }
    out
}

/// 「不收起成 Worked for」**只管收不收段**：那些步照样挂块、照样点得开，详情
/// 照样收在块后面。
///
/// 09-17 之前它顺手把每一步变成点不开、正文铺一地（那份 golden 当时只有 5 个
/// 块标记，`s4-full.ansi` 有 18 个）。用户原话：「即使不自动收起过程为 true，
/// 也不应该以 tag 行下预览的形式出现 tag 行的内容」。这条断言是那个 bug 的
/// 回归闸。
#[test]
fn keeping_the_timeline_open_still_hides_details_behind_blocks() {
    let open = std::fs::read_to_string(golden_path("s4-full-open")).expect("读 s4-full-open");
    let markers = open.matches("miyu-block=").count();
    assert!(
        markers >= 10,
        "不收段那一档的步骤又点不开了（只有 {markers} 个块标记）"
    );
    // 详情收在块里：编辑那一步的 diff 不该铺在正文行上。
    assert!(
        !open.contains("新的一行"),
        "diff 又铺在 tag 行底下了——那是被拍掉的第四种形态"
    );
    // 出厂档位是「收起」：两个展开开关都没开，一步都不该默认开着。
    assert!(
        !open.contains("miyu-block-open="),
        "出厂档位不该有默认展开的步"
    );
}

fn golden_path(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/render/tests/golden")
        .join(format!("{name}.ansi"))
}

/// 比对（或按 `MIYU_GOLDEN_WRITE=1` 重写）。
fn assert_golden(name: &str, actual: &str) {
    // golden 是**默认（Nerd Font）那一档**的快照。`MIYU_TUI_ASCII=1` 下图标本来
    // 就该不一样（那正是 `glyphs_never_mix_nerd_and_ascii` 要的东西），拿它去比
    // 是拿两把尺量同一件事。
    if std::env::var_os("MIYU_TUI_ASCII").is_some() {
        return;
    }
    let path = golden_path(name);
    if std::env::var_os("MIYU_GOLDEN_WRITE").is_some() {
        std::fs::create_dir_all(path.parent().expect("父目录")).expect("建目录");
        std::fs::write(&path, actual).expect("写 golden");
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "{} 不存在;用 MIYU_GOLDEN_WRITE=1 跑一次生成",
            path.display()
        )
    });
    if expected == actual {
        return;
    }
    let first_diff = expected
        .lines()
        .zip(actual.lines())
        .position(|(left, right)| left != right);
    panic!(
        "{} 的输出变了。第一处不同在第 {} 行:\n  golden: {:?}\n  现在  : {:?}\n\
         确实想改表现的话:MIYU_GOLDEN_WRITE=1 重写,并在提交里说明改了哪一段。",
        name,
        first_diff.map_or(0, |index| index + 1),
        first_diff.and_then(|index| expected.lines().nth(index)),
        first_diff.and_then(|index| actual.lines().nth(index)),
    );
}

/// golden 比的是**字节**，宽度必须钉死。
///
/// 不钉的话它走 `content_cols` → `crossterm::terminal::size()`，而那个在
/// `cargo test` 下**仍能从 /dev/tty 量到录制者终端的宽度**（ioctl 失败时还会
/// 回退读 `COLUMNS`）。于是这几份 .ansi 实际上编码了「谁录的、他终端多宽」，
/// 换台机器就红——2026-09-23 实测：本机 COLUMNS=80 绿，100/120/200 全红，
/// CI 上四条全红。手册里那条「测试不得依赖 runner 是否有控制终端或 TERM」
/// 说的就是这个。
///
/// 120 是常见的宽终端，够摆下时间线里最长的那几行而不触发截断——按它重录一次，
/// 之后这些文件就和跑它的人无关了。
const GOLDEN_COLS: u16 = 120;

fn capture(surface: Surface) -> String {
    crate::render::set_cols_override(GOLDEN_COLS);
    let mut renderer = surface.renderer();
    run_script(&mut renderer);
    let raw = String::from_utf8_lossy(&renderer.take_output_frame()).into_owned();
    // 线程级覆盖用完撤掉,别让同线程后面的测试继承。
    crate::render::set_cols_override(0);
    mask_volatile(&raw)
}

#[test]
fn pipe_surface_output_is_frozen() {
    let actual = capture(Surface::Pipe);
    assert_golden(Surface::Pipe.name(), &actual);
}

#[test]
fn static_timeline_output_is_frozen() {
    let actual = capture(Surface::Static);
    assert_golden(Surface::Static.name(), &actual);
}

#[test]
fn fullscreen_timeline_output_is_frozen() {
    with_blocks(|| {
        let actual = capture(Surface::Full);
        assert_golden(Surface::Full.name(), &actual);
    });
}

/// 关掉「收起成 Worked for」时长什么样:段末没有 `Worked for`,每一步就地留着
/// ——**而且照样点得开**。
///
/// 09-17 之前这一档还顺手把那些步变成点不开、正文铺一地（这份 golden 里当时
/// 只有 5 个块标记，`s4-full.ansi` 有 18 个）。那不是设计，是 `commit_immediately`
/// 一位管了三件事的副产品。用户原话：「即使不自动收起过程为 true，也不应该以
/// tag 行下预览的形式出现 tag 行的内容」。
#[test]
fn fullscreen_with_the_timeline_kept_open_is_frozen() {
    with_blocks(|| {
        let actual = capture(Surface::FullOpen);
        assert!(
            !actual.contains("Worked for"),
            "关了「收起成 Worked for」还收了段: {actual:?}"
        );
        // 09-17 之前这里只有 **5** 个（常规步骤一个都不挂标记）。数字本身不重要，
        // 重要的是「每一步都还点得开」——掉回个位数就是那个 bug 回来了。
        let markers = actual.matches("miyu-block=").count();
        assert!(
            markers >= 10,
            "不收段那一档的步骤点不开了（只有 {markers} 个块标记）"
        );
        assert_golden(Surface::FullOpen.name(), &actual);
    });
}

/// 两个「展开」开关都开着时长什么样：每一步的起始标记是 `miyu-block-open=`
/// ——出来就是展开态，再点一次收回去。**抬头底下不挂预览**（命令那一步露的
/// 那几行命令除外，那是用户指名要的）。
#[test]
fn fullscreen_with_both_switches_expanded_is_frozen() {
    with_blocks(|| {
        let actual = capture(Surface::FullExpanded);
        assert!(
            actual.contains("miyu-block-open="),
            "展开档一步都没默认开着: {actual:?}"
        );
        assert_golden(Surface::FullExpanded.name(), &actual);
    });
}

/// 同一份脚本跑两遍必须逐字节相同——不确定的输出当不了安全网。
#[test]
fn the_capture_is_deterministic() {
    let first = capture(Surface::Static);
    let second = capture(Surface::Static);
    if first == second {
        return;
    }
    let at = first
        .lines()
        .zip(second.lines())
        .position(|(left, right)| left != right);
    panic!(
        "两次抓取不一致,第 {} 行:\n  第一次: {:?}\n  第二次: {:?}",
        at.map_or(0, |index| index + 1),
        at.and_then(|index| first.lines().nth(index)),
        at.and_then(|index| second.lines().nth(index)),
    );
}
