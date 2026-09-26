//! 子代理过程的**协议**：中继往外发的标记、任务日志里写的标签，由**写的这一侧**定。
//!
//! 读的那一侧原来有两家：终端浮层按日志行和标记流把子代理的过程再攒成一条时间线，
//! 网页按标记画卡片里的 `.sub-blocks`。子代理 09-18 起是一条会话，点它切进去看全程；
//! 09-25 起引擎把标记收成状态行要的三样（`status.rs`），终端浮层那套读侧解析随之退役
//! （会话项目第 4 段之二）。留下的是写的那一侧：任务日志的标签（模型读的
//! `job(action=status)` 就是它）、标记常量、工具那一行怎么写。

use std::time::Duration;

/// 流水账里会出现的全部标签——**由写的这一侧定**。
///
/// 写日志那一侧按它们落行，`subagent/tests.rs` 钉着「写出来的每一行都打着清单里的
/// 标签」。模型读的任务日志就是这份格式。
pub const LOG_TAGS: &[&str] = &[
    "[提示]",
    "[思考]",
    "[思考+]",
    "[正文]",
    "[正文+]",
    "[工具]",
    "[结果]",
    "[输出]",
    "[准备]",
    "[统计]",
];

/// 内层那次跑自己发的标记——**由发的那一侧定**（`subagent.rs` /
/// `subagent_runner.rs` 发，前台面板与日志桥读）。日志桥收得到，所以写日志那一侧
/// 必须每个都认。
///
/// 认不出的标记会掉进 `write_tool_progress` 末尾那个「原样打出来」的兜底分支，
/// 屏幕上就是 `进度 子代理: __subagent_brief__{"description":…}`——
/// `__subagent_brief__` 已经这么漏过一次（`display.tool_calls = full` 的终端
/// 用户）。清单摆在这儿，两侧各有一条测试对着它点名。
pub const INNER_MARKERS: &[&str] = &[
    "__subagent_brief__",
    "__subagent_reasoning__",
    REASONING_DONE_MARKER,
    "__subagent_content__",
    "__subagent_metric__",
    "__subagent_stats__",
    "__subtool_preparing__",
    "__subtool_call__",
    "__subtool_result__",
];

/// 一段思考到此为止，花了这么多毫秒：`__subagent_reasoning_done__1234`。
///
/// 标记流里原来没有任何时间信息（写日志那一侧是自己掐表的），后台面板 09-17 起
/// 优先订标记流，于是面板上的思考全成了光秃秃的「已思考」。掐表只能在发的那一
/// 侧做——它是唯一知道这一段什么时候开始、什么时候结束的人。
///
/// 它**不进流水账**：那条路自己掐表，写的是 `[思考] 1.2s\t…`（见
/// `log::stamp_thought_lines`）。
pub const REASONING_DONE_MARKER: &str = "__subagent_reasoning_done__";

/// 「已后台运行」那一条走的是**外层** `subagent` 工具的通道
/// （`tools/jobs/mod.rs`），只到主渲染器，不进流水账——所以它不在
/// [`INNER_MARKERS`] 里，但渲染器那侧同样不许让它漏成原文。
pub const DETACH_MARKER: &str = "__subagent_detach__";

/// 进度通道上会出现的全部标记。
pub fn all_markers() -> impl Iterator<Item = &'static str> {
    INNER_MARKERS
        .iter()
        .copied()
        .chain(std::iter::once(DETACH_MARKER))
}

/// 一次工具结果最多带几行输出进过程。流水账和标记流是同一个数。
pub const RESULT_OUTPUT_LINES: usize = 24;

/// `__subtool_result__` 里那份输出 → 过程里该露的那几行。
///
/// 写日志那一侧的 `[输出]` 行用它。标记里的输出发的时候截过一道（`clip_detail`，
/// 8KB），这儿再收到几十行，免得一条输出把任务日志撑成输出本体。
pub fn result_output_lines(json: &str) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(json.trim()) else {
        return Vec::new();
    };
    let Some(output) = value.get("output").and_then(serde_json::Value::as_str) else {
        return Vec::new();
    };
    output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .take(RESULT_OUTPUT_LINES)
        .filter_map(|line| {
            // 工具吐的是**原始输出**，里面有转义序列、回车、制表符。面板把它当普通
            // 字符排版——原样留着，一行的真实宽度和算出来的宽度就对不上，右边那根
            // 竖线跟着参差不齐。
            let line = miyu_base::terminal::strip_ansi_text(line);
            let line = line
                .chars()
                .map(|ch| if ch == '\t' { ' ' } else { ch })
                .filter(|ch| !ch.is_control())
                .collect::<String>();
            let line = line.trim_end();
            (!line.is_empty()).then(|| miyu_base::terminal::clip_to_display_width(line, 400))
        })
        .collect()
}

/// `{"name":…,"ok":…,"args":…}` → `<工具 id>` + `<中文名>[ ok/err][ · 主题]`。
///
/// 写日志那一侧(`subtool_summary`)和状态行的窥视(`status.rs`)共用它——各拼一遍的
/// 话,同一次调用在两处的说法会慢慢长得不一样。
pub(crate) fn tool_line_text(json: &str) -> String {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(json.trim()) else {
        return json.trim().to_string();
    };
    let name = value
        .get("name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("?");
    // 前面带上工具 id（制表符分隔）：面板那边要按 id 挑图标，光有中文名挑不出来
    // ——所有工具就只能共用一个齿轮了。读日志的人看不到它（渲染时会切掉）。
    let mut out = format!("{name}\t{}", crate::tools::readable_tool_name(name));
    if let Some(ok) = value.get("ok").and_then(serde_json::Value::as_bool) {
        out.push_str(if ok { " ok" } else { " err" });
    }
    // 这一步跑了多久。**由发的那一侧掐表**（`SubagentProgress::tool_end`）——
    // 标记流里没有时间戳，读那侧只能靠这个数；写日志那一侧原来自己用 `last_call`
    // 掐，订标记流的面板于是一个工具的耗时都看不到。次序和主线一样：名字（和
    // ok/err）之后、窥视之前。
    if let Some(millis) = value.get("ms").and_then(serde_json::Value::as_u64) {
        out.push_str(" · ");
        out.push_str(&miyu_base::durations::format_seconds(
            Duration::from_millis(millis),
        ));
    }
    if let Some(args) = value.get("args").and_then(serde_json::Value::as_str) {
        let args = args.trim();
        if !args.is_empty() {
            // 命令那一步的窥视是 **title**，不是命令全文——命令全文归正文
            //（用户 09-17 拍的版：抬头给 title、正文给命令）。主线和前台浮层
            // 一直是这么写的，后台这条路却走 `tool_peek`，那个对命令工具先摘
            // 出的是命令本身：同一步在两块面板上长得不一样（用户实测截图：
            // 「命令不对啊，正确的是这样的」）。
            let peek = if miyu_base::tool_names::is_command_tool(
                miyu_base::tool_names::tool_event_base_name(name),
            ) {
                // 没给 title 的命令退回命令本身：主线那儿命令还露在抬头底下，
                // 这条路上抬头是唯一的落点，空着比长一点更糟。
                crate::tools::command_peek(args).or_else(|| crate::tools::tool_peek(name, args))
            } else {
                // 先按工具自己的规矩摘一句主题（检索词、路径……），摘不出来就把
                // 参数的值串起来，**不**原样甩 JSON——`{"action": "info",
                // "package_name": "zzq"}` 在面板里读起来是一团括号引号（用户实测：
                // 浮层的参数窥视是裸 JSON）。什么都摘不出来就不带主题。
                crate::tools::tool_peek(name, args)
            };
            if let Some(subject) = peek {
                out.push_str(" · ");
                out.push_str(&miyu_base::terminal::clip_to_display_width(&subject, 200));
            }
            // 编辑类工具再带上改了多少行——主线和前台浮层的抬头一直有 `+3 -1`，
            // 后台这条路上一直没有（用户 09-17：「子代理浮层的编辑文件没有 diff
            // 信息，tag 行后的加减多少没有」）。写在**文本里**，于是读日志和订
            // 标记流两条路都有。
            if let Some((added, removed)) = crate::tools::envelope_diff_stat(name, args) {
                out.push_str(&format!(" · +{added} -{removed}"));
            }
        }
    }
    out
}
