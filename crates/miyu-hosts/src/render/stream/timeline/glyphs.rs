//! 时间线上一步长什么样：工具图标、那一行怎么排、点开是什么。
//!
//! 从 `timeline.rs` 搬来（09-16 拆分，那份超过了文件规模基线）。只搬不改。

use super::*;

/// 工具图标。一眼分出「这一步在干什么」比认出具体是哪个工具更有用，
/// 所以按**动作类型**归组，不是一个工具一个图标。
pub(crate) fn tool_glyph(name: &str) -> &'static str {
    // 事件名可能带后缀（`subagent:<描述>`、`use_meme:<名字>`），按基名认。
    let name = crate::render::tool_event_base_name(name);
    if !nerd() {
        // 没有 Nerd Font 的时候**别凑**。
        //
        // 通用符号来自不同的字表，粗细、基线、占几格全不一样，排成一列参差不齐
        // （用户原话「ASCII 字符是大小不一的」）。与其凑一堆半像不像的，不如只
        // 留几个**一眼认得出**的：跑命令的提示符、问号，其余统一一个齿轮——
        // "这是个工具"本来就够用了，分得清哪一类是 Nerd Font 那一档的事。
        return match name {
            "run_command" => "$",
            "ask_question" => "?",
            _ => "⚙",
        };
    }

    match name {
        // 终端
        // 跑命令就是 `$`——提示符本身比任何图标都直白。终端那个图标让给脚本：
        // 「跑一条命令」和「管一个脚本」是两件事。
        "run_command" => "$",
        // 铅笔 / 垃圾桶
        "edit" => "\u{f040}",
        "trash_path" => "\u{f1f8}",
        // 文档
        "read" => "\u{f0f6}",
        // 放大镜
        "glob" | "grep" | "search_knowledge_base" | "search_evicted_context" | "kb" => "\u{f002}",
        // Arch 那一家子：官方包、AUR、Wiki、新闻都挂 Arch 的标（用户指名这个码位）。
        // 原来分散在"地球"和"包"两组里，认不出它们是同一家的。
        "aur"
        | "archlinux_official_package_query"
        | "archwiki_query"
        | "archlinux_news"
        | "install_aur_package"
        | "review_aur_package" => "\u{f08c7}",
        // 地球
        "web_search" | "web_fetch" | "search_web_images" => "\u{f0ac}",
        // 机器人：派出去的那个也是个"它"，不是一条连线。
        "subagent" | "task" => "\u{f06a9}",
        // 纸飞机：给另一个会话里的 AI 递话（09-23）。
        "send_to_other_running_session" => "\u{f1d8}",
        // 眼睛：看图和"贴一张图"是两件事——它是在**读**。
        "vision_analyze" => "\u{f0208}",
        // 图片
        "print_image" | "generate_image" | "share_file" | "artifact" | "present_artifact" => {
            "\u{f03e}"
        }
        // 表情包单列：圆圈笑脸。它和"贴一张图"不是一回事——一眼看出是在发表情。
        "use_meme" | "manage_meme" => "\u{f118}",
        // 大脑
        "remember_fact" | "recall_memories" => "\u{f09d1}",
        // 清单
        "todowrite" | "goal" | "alarm" => "\u{f03a}",
        // 后台任务：列一列有哪些在跑（用户指名这个码位）。
        "job" => "\u{f0572}",
        // 计算器
        "ledger" | "manage_ledger" | "get_exchange_rate" => "\u{f00ec}",
        // 查看系统信息：CoreOS 那个圆里嵌核的标（用户指名「核心的那个」）。
        // 它原来跟装包挤在一类里——查机器和装包不是一回事。
        "check_os_info" => "\u{f305}",
        // 问号
        "ask_question" => "\u{f128}",
        // 魔杖
        // 终端：脚本是"一段能跑的东西"。
        "manage_script" => "\u{f489}",
        // 文档：技能和工具清单都是"一份说明"，装上才有用。
        "manage_skill" | "load_skill" | "load_tools" => "\u{f4a5}",
        _ => "\u{f4bc}",
    }
}

/// 工具输出切成可展开的行。
///
/// JSON 先排版再给——工具的返回十有八九是一长串 JSON，原样贴出来是一行糊到
/// 屏幕外的字符汤，点开等于没点。整段走暗色：这是"想看再看"的附注，不该和
/// 正文抢注意力。
pub(crate) fn tool_output_lines(output: &str) -> Vec<String> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let formatted = crate::render::format_tool_payload(trimmed);
    // 先折行再上色：反过来的话折行要拆 ANSI，切在转义序列中间就是乱码。
    wrap_detail(&formatted)
        .into_iter()
        .take(MAX_DETAIL_LINES)
        .map(|line| format!("\x1b[2m{line}\x1b[0m"))
        .collect()
}

/// 一步展开后的样子：头行 + 空行 + 缩进的正文 + 空行。
///
/// 头行留着是因为它是把手（再点一次才收得回去）；上下各留一行空，否则展开的
/// 内容会和上下两步的连线糊成一片。
/// 时间线里一步占的那几行：抬头（挂着块的话包上标记），底下跟着它露出来的尾巴
///（跑完的命令留着的那几行输出，连线从中间穿过）。块的结束标记放在尾巴之后：
/// 点开时展开内容把抬头和尾巴**一起**换掉——和跑着的时候一个规矩。
pub fn step_rows(step: &Step, id: Option<u64>) -> String {
    // 起始标记带着「这一步默认开着吗」：`完整` 那一档的步出来就是展开态，
    // 再点一次照样收得回去（用户 09-17）。见 `Step::open`。
    let mut row = match id {
        Some(id) => format!("{}{}", blocks::begin_marker_in(id, step.open), step.line),
        None => step.line.clone(),
    };
    for extra in &step.tail {
        row.push('\n');
        row.push_str(&rail_prefix());
        row.push_str(extra);
    }
    if id.is_some() {
        row.push_str(blocks::END_MARKER);
    }
    row
}

/// 这一步点开之后是什么样。三处共用（主线、两块面板）。
pub fn step_detail_lines(step: &Step) -> Vec<String> {
    step_detail(step)
}

/// 收缩行点开是什么样：抬头（`›` 翻成 `⌄`）、连线、各步同一列，不缩进不铺底
///（用户拿主线那份对比：「这个才是正确的」）。
///
/// 主线、前台面板、**后台面板**三处都这么拼——后台那份在
/// `cli::repl::tail::screen::overlay` 里，逐行相同。
pub fn fold_open_lines(line: &str, rows: Vec<String>) -> Vec<String> {
    let mut detail = Vec::with_capacity(rows.len() + 3);
    detail.push(fold_line_open(line));
    detail.push(rail());
    detail.extend(rows);
    detail.push(String::new());
    detail
}

pub(crate) fn step_detail(step: &Step) -> Vec<String> {
    if step.kind == StepKind::Fold {
        return fold_open_lines(&step.line, step.body.clone());
    }
    let mut detail = Vec::with_capacity(step.body.len() + 2);
    detail.push(step.line.clone());
    detail.extend(indented_body(&step.body));
    detail
}
