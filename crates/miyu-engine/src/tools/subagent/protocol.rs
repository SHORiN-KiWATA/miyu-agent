//! 子代理过程的**协议**：写日志那一侧写什么，读的那一侧就读什么。
//!
//! 后台子代理面板是另一套组装器：它从**日志行**攒步，而前台面板从**事件**攒步
//! （`docs/plan/2026-09-17-render-unification.md` §1.2）。两套加起来一千四百行
//! 做的是同一件事，已经漂移过三处（同文件 §2.3）。
//!
//! 收敛的第一步不是把两个组装器焊在一起，而是先把**「这一行说了什么」**从
//! **「这一行长什么样」**里拆出来。拆之前，解析与渲染是搅在一个 210 行的循环里
//! 的：`split_tool_line` 一边拆制表符一边挑图标，认不出工具 id 就地退回一个
//! **前台早就不用了的旧齿轮**——那处漂移就是这么藏进去的，读代码的人看不见它是
//! 个选择。
//!
//! 拆开之后：这里只回答「说了什么」（[`LogEvent`]），图标、耗时怎么写、收不收段
//! 全归组装器。三处漂移因此各自变成组装器里**一行明摆着的选择**，拍板时改那一
//! 行就够了，不用再通读两百行去找它藏在哪。
//!
//! 落在 `tools::subagent` 下面是因为格式由**写日志那一侧**定（`subagent.rs` 写，
//! 后台面板读）。`cli → tools` 是允许的方向；将来前台那条路也把 `__subtool_*`
//! 标记解成同一个 [`LogEvent`]，两个组装器才谈得上合并——那是报告 §6.1 路 A
//! 的落点，位置就是这儿（`tools/subagent/protocol.rs`）。

use std::borrow::Cow;
use std::time::Duration;

/// 流水账里会出现的全部标签——**由写的这一侧定**。
///
/// 读的那一侧按它们分支，测试夹具按它们造样本。手写样本时最容易写错的是
/// `[提示]`：面板渲染出来的抬头是「提示词 · …」，于是就有人（我）把标签也写成
/// `[提示词]`——解码器认不出来，那一行掉进「无标签续行」分支，收缩行还会多数一
/// 个 tool。冻住一份不存在的格式，安全网就是假的，所以把清单摆在这儿，两边都对
/// 着它（`subagent.rs` 与 `cli::tests::golden_panel` 各有一条测试钉着）。
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

/// 过程里的一条记录，**已经拆开、还没变成任何长相**。
///
/// 每个变体只带它**说了什么**。图标、抬头怎么拼、连续的思考并不并、收不收段
/// ——全是组装器的事。
///
/// 字段是 `Cow`，因为它有两个来源：[`parse_log_line`] 从日志行上**借**，
/// [`from_marker`] 从标记的 JSON 里**造**。两条路进同一个类型，组装器才有可能
/// 只写一份。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogEvent<'a> {
    /// `[提示] 交给它的差事`。换行在写的时候折成了 `\u{1}`，这里不拆——拆成几段
    /// 是长相问题。
    Prompt(Cow<'a, str>),
    /// `[思考] 先列一下` 或 `[思考] 1.2s\t先列一下`。老日志没有那个耗时。
    Thought {
        text: Cow<'a, str>,
        elapsed: Option<Duration>,
        /// 这一截**接着上一条，中间不换行**（`[思考+]`，或者标记流里的逐 delta）。
        /// 见 [`LogEvent::Speech`] 的那段说明。
        continues: bool,
    },
    /// `[正文] 它说的话`。
    ///
    /// `continues` 是这条记录里最要紧的一位：**一条记录不等于一行正文**。
    ///
    /// 有两条进料口，颗粒度差着几个数量级：日志那条是攒成段落才落一条
    /// （`log::accumulate_stream`），`job.trace` 那条是**逐 delta 的原始标记**
    /// ——一个词一条。两条都当成"一条 = 一行"的话，走标记流时正文就是一句一个
    /// 台阶（用户 09-17：「子代理的浮层的正文现在是每个 token 都会换一次行」）。
    ///
    /// 所以换行与否由这一位说，不由记录边界说：`continues` 为真就粘回上一截。
    Speech { text: Cow<'a, str>, continues: bool },
    /// `[工具] <工具 id>\t<中文名> · <主题>`。
    ToolCall(ToolLine<'a>),
    /// `[结果] <工具 id>\t<中文名> ok · 1.2s · <主题>`。正文与耗时用
    /// [`ToolLine::result_parts`] 拆。
    ToolResult { line: ToolLine<'a>, ok: bool },
    /// `[输出] 工具吐的一行`。挂到刚才那一步的详情里。
    Output(Cow<'a, str>),
    /// `[准备] <工具 id>\t准备编辑`。参数还在流，只有作为日志**末尾**那一行时
    /// 才是「此刻」。
    Preparing(ToolLine<'a>),
    /// `[统计] 词元 1234`。它**不是**一次工具调用：没有结果行，也不该被当成
    /// 「末尾那个还没回来的调用」挂上转轮。
    Stats(Cow<'a, str>),
    /// 刚才那一段思考花了多久。它不是一步，是盖在**上一步**思考上的一个数
    /// （见 [`REASONING_DONE_MARKER`]）。
    ThoughtElapsed(Duration),
    /// 没打标签的行。跟在思考／正文后面是续行（一段话里的换行），跟在工具后面是
    /// 内层渲染器自己的进度回声。
    Continuation(Cow<'a, str>),
}

/// `[工具]` / `[结果]` / `[准备]` 三种行共用的形状：工具 id + 去掉 id 的正文。
///
/// `tool` 是 `None` 表示**老日志**——改格式之前写的，没带那个制表符。挑不出图标
/// 时用什么，是组装器的选择（今天前台用 `glyph_tool()`、后台硬编码一个旧齿轮，
/// 那正是 §6.3 第 3 项要拍的），所以这里只如实说「没有 id」。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolLine<'a> {
    pub tool: Option<Cow<'a, str>>,
    pub text: Cow<'a, str>,
    /// 这次调用的**参数原文**。只有标记流那条路有——日志行上只留了拼好的抬头。
    ///
    /// 浮层画 diff 要靠它：子代理内层的编辑拿不到 `__patch_preview__` 的真 diff，
    /// 只有调用参数里那份信封（用户 09-17：「展开后的 diff 渲染也没有」）。
    /// 着色归渲染层，所以这儿只如实把参数带过去。
    pub args: Option<Cow<'a, str>>,
}

impl<'a> ToolLine<'a> {
    fn parse(rest: &'a str) -> Self {
        match rest.split_once('\t') {
            Some((tool, text)) => Self {
                tool: Some(Cow::Borrowed(tool.trim())),
                text: Cow::Borrowed(text.trim()),
                args: None,
            },
            None => Self {
                tool: None,
                text: Cow::Borrowed(rest.trim()),
                args: None,
            },
        }
    }

    /// 结果行的正文与耗时：`运行命令 ok · 1.2s · ls` → (`运行命令 · ls`, 1.2s)。
    ///
    /// ok/err 单独盖在 [`LogEvent::ToolResult`] 上，留在正文里会读成
    /// 「运行命令 ok · … · ok」。
    pub fn result_parts(&self) -> (String, Option<Duration>) {
        split_elapsed(&strip_result_status(&self.text))
    }
}

/// 一行流水账说了什么。
///
/// 认不出标签的一律是 [`LogEvent::Continuation`]——**不是**丢掉：正文段按自然段
/// 落盘，段里的换行原样写着（标题、表格行、列表项都是这么来的），丢掉就是整段缺
/// 句子、表格只剩表头（用户实测截图）。
pub fn parse_log_line(line: &str) -> LogEvent<'_> {
    // 带 `+` 的先认：`[思考]` 是 `[思考+]` 的前缀。
    if let Some(rest) = line.strip_prefix("[思考+]") {
        let (text, elapsed) = split_thought_elapsed(rest);
        return LogEvent::Thought {
            text: Cow::Borrowed(text),
            elapsed,
            continues: true,
        };
    }
    if let Some(rest) = line.strip_prefix("[正文+]") {
        return LogEvent::Speech {
            text: Cow::Borrowed(strip_separator(rest)),
            continues: true,
        };
    }
    if let Some(rest) = line.strip_prefix("[思考]") {
        let (text, elapsed) = split_thought_elapsed(rest);
        return LogEvent::Thought {
            text: Cow::Borrowed(text),
            elapsed,
            continues: false,
        };
    }
    if let Some(rest) = line.strip_prefix("[提示]") {
        return LogEvent::Prompt(Cow::Borrowed(rest.trim()));
    }
    if let Some(rest) = line.strip_prefix("[正文]") {
        // **只去掉标签后面那一个分隔空格**，别 trim：`[正文+]` 要粘回来，两头
        // 的空白正是词边界（见 `log::accumulate_stream`）。
        return LogEvent::Speech {
            text: Cow::Borrowed(strip_separator(rest)),
            continues: false,
        };
    }
    if let Some(rest) = line.strip_prefix("[输出]") {
        return LogEvent::Output(Cow::Borrowed(rest.trim_end()));
    }
    if let Some(rest) = line.strip_prefix("[工具]") {
        return LogEvent::ToolCall(ToolLine::parse(rest));
    }
    if let Some(rest) = line.strip_prefix("[结果]") {
        let rest = rest.trim();
        // ok/err 在整行里找（工具 id 那一截不含空格，不会误伤）。
        let ok = !rest.contains(" err");
        return LogEvent::ToolResult {
            line: ToolLine::parse(rest),
            ok,
        };
    }
    if let Some(rest) = line.strip_prefix("[准备]") {
        return LogEvent::Preparing(ToolLine::parse(rest));
    }
    if let Some(rest) = line.strip_prefix("[统计]") {
        return LogEvent::Stats(Cow::Borrowed(rest.trim()));
    }
    LogEvent::Continuation(Cow::Borrowed(line))
}

/// 进度标记 → 同一个 [`LogEvent`]，**不经过日志文件**。
///
/// 今天后台面板只有「读日志」这一条路；`job.trace` 里其实原样存着这些标记
/// （`tools/jobs/mod.rs`），网页端的后台面板就是靠它流式的。阶段 4（路 B）让
/// 终端面板也直接订那条流时，这个函数就是那条路的解码器——而且解出来的是**同
/// 一个** `LogEvent`，组装器一行都不用改。
///
/// 工具那三种的正文交给 [`tool_line_text`] 拼，和写日志那一侧共用同一份：两处
/// 各拼一遍的话，`__subtool_call__` 的抬头在两条路上会慢慢长得不一样，而那正是
/// 这次重构要根治的毛病。
///
/// 返回 `None` 表示「这不是一条该进过程的标记」：`__subagent_metric__` 是中途
/// 量报（每调一次工具来一条，进了时间线就把面板撑满），`__subagent_detach__`
/// 走的是外层通道、根本不属于内层这段过程。
pub fn from_marker(message: &str) -> Option<LogEvent<'static>> {
    if let Some(name) = message.strip_prefix("__subtool_preparing__") {
        let name = name.trim();
        let phase = crate::tools::preparing_phase(name).unwrap_or("");
        return Some(LogEvent::Preparing(ToolLine {
            tool: Some(Cow::Owned(name.to_string())),
            text: Cow::Owned(phase.to_string()),
            args: None,
        }));
    }
    // 标记流是**逐 delta** 的（`SubagentRunner::reasoning` / `content` 一个 delta
    // 报一条），所以每一条都是一截、不是一行：`continues` 恒真，换行靠正文里
    // 自带的 `\n`。原样带过去，不 trim——掐掉两头的空白，英文就会粘成
    // `the quickbrown fox`。
    if let Some(text) = message.strip_prefix("__subagent_reasoning__") {
        return (!text.is_empty()).then(|| LogEvent::Thought {
            text: Cow::Owned(text.to_string()),
            elapsed: None,
            continues: true,
        });
    }
    if let Some(millis) = message.strip_prefix(REASONING_DONE_MARKER) {
        return millis
            .trim()
            .parse()
            .ok()
            .map(|millis| LogEvent::ThoughtElapsed(Duration::from_millis(millis)));
    }
    if let Some(text) = message.strip_prefix("__subagent_content__") {
        return (!text.is_empty()).then(|| LogEvent::Speech {
            text: Cow::Owned(text.to_string()),
            continues: true,
        });
    }
    if let Some(json) = message.strip_prefix("__subtool_call__") {
        return Some(LogEvent::ToolCall(tool_line_of(json)));
    }
    if let Some(json) = message.strip_prefix("__subtool_result__") {
        let line = tool_line_of(json);
        let ok = serde_json::from_str::<serde_json::Value>(json.trim())
            .ok()
            .and_then(|value| value.get("ok").and_then(serde_json::Value::as_bool))
            .unwrap_or(true);
        return Some(LogEvent::ToolResult { line, ok });
    }
    if let Some(json) = message.strip_prefix("__subagent_brief__") {
        let prompt = serde_json::from_str::<serde_json::Value>(json.trim())
            .ok()
            .and_then(|value| {
                value
                    .get("prompt")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_default();
        let prompt = prompt.trim();
        // 写日志那一侧把换行折成 `\u{1}`(流水账是按行读的),这条路没这个约束,
        // 但**折成同一个形状**——面板拆的时候不该关心事件从哪条路来。
        return (!prompt.is_empty()).then(|| {
            LogEvent::Prompt(Cow::Owned(prompt.replace('\r', "").replace('\n', "\u{1}")))
        });
    }
    if let Some(text) = message.strip_prefix("__subagent_stats__") {
        let text = text.trim();
        return (!text.is_empty()).then(|| LogEvent::Stats(Cow::Owned(text.to_string())));
    }
    None
}

/// `{"name":…,"ok":…,"args":…}` → `<工具 id>` + `<中文名>[ ok/err][ · 主题]`。
///
/// 写日志那一侧(`subtool_summary`)与 [`from_marker`] 共用它——两处各拼一遍的话,
/// 同一次调用在两条路上的抬头会慢慢长得不一样。
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

/// 同上，再按制表符拆成 [`ToolLine`]。参数原文一并带上——浮层画 diff 要用它。
fn tool_line_of(json: &str) -> ToolLine<'static> {
    let text = tool_line_text(json);
    let args = serde_json::from_str::<serde_json::Value>(json.trim())
        .ok()
        .and_then(|value| {
            value
                .get("args")
                .and_then(serde_json::Value::as_str)
                .map(|args| Cow::Owned(args.to_string()))
        });
    match text.split_once('\t') {
        Some((tool, rest)) => ToolLine {
            tool: Some(Cow::Owned(tool.trim().to_string())),
            text: Cow::Owned(rest.trim().to_string()),
            args,
        },
        None => ToolLine {
            tool: None,
            text: Cow::Owned(text.trim().to_string()),
            args,
        },
    }
}

/// `[思考] 1.2s\t正文`：桥把这段想了多久写在最前面，制表符隔开。老日志没有。
fn split_thought_elapsed(rest: &str) -> (&str, Option<Duration>) {
    // 同 `[正文]`：正文那一截原样留着，只去掉标签后面那个分隔空格。
    let rest = strip_separator(rest);
    if let Some((secs, text)) = rest.split_once('\t') {
        if let Some(elapsed) = parse_seconds(secs.trim()) {
            return (text, Some(elapsed));
        }
    }
    (rest, None)
}

/// 去掉标签和正文之间那一个分隔空格（只去一个）。
fn strip_separator(rest: &str) -> &str {
    rest.strip_prefix(' ').unwrap_or(rest)
}

/// 去掉结果正文里那个 ok/err：状态单独盖，留着会读成「运行命令 ok · … · ok」。
fn strip_result_status(text: &str) -> String {
    for status in [" ok", " err"] {
        // 夹在中间：`运行命令 ok · ls` → `运行命令 · ls`。
        if let Some(index) = text.find(&format!("{status} · ")) {
            return format!("{}{}", &text[..index], &text[index + status.len()..]);
        }
        // 在末尾：`运行命令 ok` → `运行命令`。
        if let Some(head) = text.strip_suffix(status) {
            return head.to_string();
        }
    }
    text.to_string()
}

/// 从 `运行命令 · 1.2s · ls` 这种正文里把耗时摘出来，返回去掉耗时的正文和耗时。
fn split_elapsed(text: &str) -> (String, Option<Duration>) {
    let Some((head, rest)) = text.split_once(" · ") else {
        return (text.to_string(), None);
    };
    let (candidate, tail) = match rest.split_once(" · ") {
        Some((candidate, tail)) => (candidate, Some(tail)),
        None => (rest, None),
    };
    let Some(elapsed) = parse_seconds(candidate) else {
        return (text.to_string(), None);
    };
    let stripped = match tail {
        Some(tail) => format!("{head} · {tail}"),
        None => head.to_string(),
    };
    (stripped, Some(elapsed))
}

/// `format_seconds` 的逆：`0.3s` / `12s` / `1m 05s` / `1h 02m 05s`。
///
/// 09-23 之前过了一小时写成 `125m 03s`,老日志里的这种照样认(没有小时那一截)。
pub fn parse_seconds(text: &str) -> Option<Duration> {
    let text = text.trim();
    let (hours, text) = match text.split_once("h ") {
        Some((hours, rest)) => (hours.parse::<u64>().ok()?, rest),
        None => (0, text),
    };
    if let Some((minutes, seconds)) = text.split_once("m ") {
        let minutes: u64 = minutes.parse().ok()?;
        let seconds: u64 = seconds.strip_suffix('s')?.parse().ok()?;
        return Some(Duration::from_secs(hours * 3_600 + minutes * 60 + seconds));
    }
    if hours > 0 {
        return None;
    }
    let seconds: f64 = text.strip_suffix('s')?.parse().ok()?;
    (seconds.is_finite() && seconds >= 0.0).then(|| Duration::from_secs_f64(seconds))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试里写字面量用的：`Cow::Borrowed` 铺满断言会把意思埋掉。
    fn b(text: &str) -> Cow<'_, str> {
        Cow::Borrowed(text)
    }

    /// 八种标签各解成自己那一种。
    #[test]
    fn every_tag_parses_to_its_own_event() {
        assert_eq!(
            parse_log_line("[提示] 去看看"),
            LogEvent::Prompt(b("去看看"))
        );
        assert_eq!(
            parse_log_line("[思考] 1.2s\t先列一下"),
            LogEvent::Thought {
                text: b("先列一下"),
                elapsed: Some(Duration::from_secs_f64(1.2)),
                continues: false,
            }
        );
        assert_eq!(
            parse_log_line("[正文] 空的。"),
            LogEvent::Speech {
                text: b("空的。"),
                continues: false,
            }
        );
        // 带 `+` 的是**一截**，不是一行：读那侧粘回上一条。
        assert_eq!(
            parse_log_line("[正文+] 接着说"),
            LogEvent::Speech {
                text: b("接着说"),
                continues: true,
            }
        );
        assert_eq!(
            parse_log_line("[思考+] 接着想"),
            LogEvent::Thought {
                text: b("接着想"),
                elapsed: None,
                continues: true,
            }
        );
        // 两头的空白留着：那是拼回去时的词边界。
        assert_eq!(
            parse_log_line("[正文+]  fox"),
            LogEvent::Speech {
                text: b(" fox"),
                continues: true,
            }
        );
        // 输出行只去尾巴不去头：标签后面那个空格**留着**，面板把它当正文的
        // 缩进用了十几个版本。去掉就是所有工具输出整体左移一格。
        assert_eq!(
            parse_log_line("[输出] total 0"),
            LogEvent::Output(b(" total 0"))
        );
        assert_eq!(
            parse_log_line("[统计] 词元 12"),
            LogEvent::Stats(b("词元 12"))
        );
        assert_eq!(
            parse_log_line("[工具] run_command\t运行命令 · ls"),
            LogEvent::ToolCall(ToolLine {
                tool: Some(b("run_command")),
                text: b("运行命令 · ls"),
                args: None,
            })
        );
        assert_eq!(
            parse_log_line("[准备] edit\t准备编辑"),
            LogEvent::Preparing(ToolLine {
                tool: Some(b("edit")),
                text: b("准备编辑"),
                args: None,
            })
        );
    }

    /// 老日志：没有那个制表符，`tool` 就是 `None`——**不是**就地挑个图标顶上。
    /// 挑什么是组装器的选择（§6.3 第 3 项），协议只如实说「没有 id」。
    #[test]
    fn an_old_line_without_a_tool_id_says_so() {
        assert_eq!(
            parse_log_line("[工具] 编辑文件 · /tmp/a.txt"),
            LogEvent::ToolCall(ToolLine {
                tool: None,
                text: b("编辑文件 · /tmp/a.txt"),
                args: None,
            })
        );
        // 老日志的思考也没有耗时。
        assert_eq!(
            parse_log_line("[思考] 先想下一步"),
            LogEvent::Thought {
                text: b("先想下一步"),
                elapsed: None,
                continues: false,
            }
        );
    }

    /// 结果行：ok/err 单独拿出来，正文里那个去掉。
    #[test]
    fn a_result_line_carries_its_own_verdict() {
        let LogEvent::ToolResult { line, ok } =
            parse_log_line("[结果] run_command\t运行命令 ok · 1.2s · ls")
        else {
            panic!("该是结果行");
        };
        assert!(ok);
        assert_eq!(line.tool.as_deref(), Some("run_command"));
        let (text, elapsed) = line.result_parts();
        assert_eq!(text, "运行命令 · ls");
        assert_eq!(elapsed, Some(Duration::from_secs_f64(1.2)));

        let LogEvent::ToolResult { ok, .. } =
            parse_log_line("[结果] run_command\t运行命令 err · exit 3")
        else {
            panic!("该是结果行");
        };
        assert!(!ok);
    }

    /// 认不出标签的是续行，**不是**丢掉：正文段里的换行原样写着，丢掉就是整段
    /// 缺句子、表格只剩表头。
    #[test]
    fn an_untagged_line_is_a_continuation_not_garbage() {
        assert_eq!(
            parse_log_line("| 列一 | 列二 |"),
            LogEvent::Continuation(b("| 列一 | 列二 |"))
        );
        // 空行也是内容（自然段的分隔）。
        assert_eq!(parse_log_line(""), LogEvent::Continuation(b("")));
    }

    /// **编码器与解码器是一对**：写日志那一侧写出来的每一种行，读的那一侧都得
    /// 解回面板真正要用的那几样（工具 id、中文名、主题、ok/err、耗时）。
    ///
    /// 这条链今天一个测试都没有，而它是 AGENTS §2.3 的双兼容契约所在：标记串已经
    /// 落库在 `job.trace` 与 `tool_flow` 里，改了格式就得两边都认。日志行是标记的
    /// **有损**编码（参数 JSON、流式原文都没带过去），所以这里断言的是"面板要用的
    /// 那几样没丢"，不是逐字节相等。
    ///
    /// `protocol` 是 `subagent` 的子模块，够得着 `log` 里那个私有编码器——落点选在
    /// 这儿本来就是为了让这一对能对着测。
    #[test]
    fn what_the_writer_writes_the_reader_reads_back() {
        let result = serde_json::json!({
            "name": "run_command",
            "args": "{\"command\":\"ls -la\"}",
            "ok": true,
            "output": "total 0",
        })
        .to_string();
        let written = crate::tools::subagent::log::readable_subagent_log_line_timed(
            &format!("__subtool_result__{result}"),
            Some(Duration::from_millis(1_200)),
        );
        let mut lines = written.lines();

        let LogEvent::ToolResult { line, ok } = parse_log_line(lines.next().expect("结果行"))
        else {
            panic!("第一行该是结果: {written:?}");
        };
        assert!(ok, "ok:true 该解回 ok");
        assert_eq!(
            line.tool.as_deref(),
            Some("run_command"),
            "工具 id 丢了就挑不出图标"
        );
        let (text, elapsed) = line.result_parts();
        assert_eq!(elapsed, Some(Duration::from_secs_f64(1.2)), "{written:?}");
        assert!(text.starts_with(&crate::tools::readable_tool_name("run_command")));
        assert!(
            text.contains("ls -la"),
            "主题丢了抬头就只剩工具名: {text:?}"
        );

        // 工具吐的每一行跟在后面，各自一条 `[输出]`。
        assert!(
            lines.all(|line| matches!(parse_log_line(line), LogEvent::Output(_))),
            "{written:?}"
        );

        // 调用行：只有 id 与名字＋主题，没有状态。
        let call = serde_json::json!({"name": "edit", "args": "{\"path\":\"/tmp/a\"}"}).to_string();
        let written = crate::tools::subagent::log::readable_subagent_log_line_timed(
            &format!("__subtool_call__{call}"),
            None,
        );
        let LogEvent::ToolCall(line) = parse_log_line(&written) else {
            panic!("该是调用行: {written:?}");
        };
        assert_eq!(line.tool.as_deref(), Some("edit"));

        // 差事：换行折成 `\u{1}`，解回来还是那几段。
        let brief = serde_json::json!({"prompt": "第一行\n第二行"}).to_string();
        let written = crate::tools::subagent::log::readable_subagent_log_line_timed(
            &format!("__subagent_brief__{brief}"),
            None,
        );
        let LogEvent::Prompt(text) = parse_log_line(&written) else {
            panic!("该是差事行: {written:?}");
        };
        assert_eq!(
            text.split('\u{1}').collect::<Vec<_>>(),
            vec!["第一行", "第二行"]
        );

        // 跑完那次的量报是人话，不是工具。
        let written = crate::tools::subagent::log::readable_subagent_log_line_timed(
            "__subagent_stats__词元 1234",
            None,
        );
        assert_eq!(parse_log_line(&written), LogEvent::Stats(b("词元 1234")));

        // 中途的量报**不进流水账**——每调一次工具记一条的话，面板的时间线会被
        // 这些节点撑满。
        assert!(
            crate::tools::subagent::log::readable_subagent_log_line_timed(
                "__subagent_metric__x",
                None
            )
            .is_empty()
        );
    }

    /// **两条路必须解出同一个事件**。
    ///
    /// 后台面板今天走「标记 → 日志行 → 事件」，将来（阶段 4 路 B）要走
    /// 「标记 → 事件」，前台本来就在标记那一侧。这三条路只有解出同一个
    /// `LogEvent`，组装器才谈得上只写一份——否则「统一」只是把两份代码摞在
    /// 一个文件里。
    ///
    /// 逐个点名 `INNER_MARKERS`：漏掉哪个都会让那条路悄悄分叉。
    #[test]
    fn both_paths_decode_to_the_same_event() {
        let call = serde_json::json!({"name": "run_command", "args": "{\"command\":\"ls -la\"}"})
            .to_string();
        let result = serde_json::json!({
            "name": "run_command",
            "args": "{\"command\":\"ls -la\"}",
            "ok": false,
            "output": "boom",
        })
        .to_string();
        let brief = serde_json::json!({"prompt": "第一行\n第二行"}).to_string();
        let payload = |marker: &str| match marker {
            "__subagent_brief__" => brief.clone(),
            "__subagent_reasoning__" => "先列一下".to_string(),
            "__subagent_content__" => "里面是空的。".to_string(),
            "__subagent_metric__" => "≈3.1K\t3100\t工具调用 3 次".to_string(),
            REASONING_DONE_MARKER => "1234".to_string(),
            "__subagent_stats__" => "词元 1234".to_string(),
            "__subtool_preparing__" => "run_command".to_string(),
            "__subtool_call__" => call.clone(),
            "__subtool_result__" => result.clone(),
            other => panic!("{other} 是新标记，给它补个样例"),
        };
        for marker in INNER_MARKERS {
            let message = format!("{marker}{}", payload(marker));
            let direct = from_marker(&message);
            let written =
                crate::tools::subagent::log::readable_subagent_log_line_timed(&message, None);
            // 段末耗时只走标记流：它盖在上一步思考上，日志那条路自己掐表。
            if *marker == REASONING_DONE_MARKER {
                assert_eq!(
                    direct,
                    Some(LogEvent::ThoughtElapsed(Duration::from_millis(1234))),
                    "段末耗时没解出来"
                );
                assert!(written.is_empty(), "段末耗时不该进流水账: {written:?}");
                continue;
            }
            // 中途量报两条路都该不产出：进了时间线就把面板撑满。
            if *marker == "__subagent_metric__" {
                assert!(direct.is_none(), "中途量报不该解成过程事件");
                assert!(written.is_empty(), "中途量报不该进流水账");
                continue;
            }
            let direct = direct.unwrap_or_else(|| panic!("{marker} 直接解不出事件"));
            // 日志那条路要先落成行再解回来；结果事件还带着 `[输出]` 几行，只比
            // 第一行（`[结果]`）。
            let first = written.lines().next().unwrap_or_default();
            let via_log = parse_log_line(first);
            // **参数是例外**：标记流带得过来，日志行上只留了拼好的抬头（浮层画
            // diff 要用它，见 `ToolLine::args`）。比的时候把它摘出去——除了它，
            // 两条路必须一个字节都不差。
            let (direct_args, direct) = strip_args(direct);
            let (log_args, via_log) = strip_args(via_log);
            assert_eq!(
                direct, via_log,
                "{marker} 在两条路上解出来不一样\n  直接: {direct:?}\n  过日志: {via_log:?}"
            );
            assert!(log_args.is_none(), "日志行上不该有参数: {log_args:?}");
            if matches!(*marker, "__subtool_call__" | "__subtool_result__") {
                assert!(direct_args.is_some(), "{marker} 该把参数带过来");
            }
        }
    }

    /// 把 `ToolLine::args` 摘出来（比两条路时它是例外，见调用处）。
    fn strip_args(event: LogEvent<'_>) -> (Option<String>, LogEvent<'_>) {
        fn take(mut line: ToolLine<'_>) -> (Option<String>, ToolLine<'_>) {
            let args = line.args.take().map(|args| args.into_owned());
            (args, line)
        }
        match event {
            LogEvent::ToolCall(line) => {
                let (args, line) = take(line);
                (args, LogEvent::ToolCall(line))
            }
            LogEvent::Preparing(line) => {
                let (args, line) = take(line);
                (args, LogEvent::Preparing(line))
            }
            LogEvent::ToolResult { line, ok } => {
                let (args, line) = take(line);
                (args, LogEvent::ToolResult { line, ok })
            }
            other => (None, other),
        }
    }

    /// 带小时的新写法(09-23)与老日志里没有小时的写法都认。
    #[test]
    fn parse_seconds_reads_hours_and_the_old_minutes_only_form() {
        assert_eq!(
            parse_seconds("1h 02m 05s"),
            Some(Duration::from_secs(3_725))
        );
        assert_eq!(parse_seconds("125m 03s"), Some(Duration::from_secs(7_503)));
        assert_eq!(parse_seconds("1h 5s"), None);
    }

    /// `format_seconds` 的逆要认得它写出来的每一种形状。
    #[test]
    fn seconds_round_trip_through_the_formatter() {
        for millis in [
            1_u64, 340, 1_200, 12_000, 65_000, 3_600_000, 3_725_000, 90_061_000,
        ] {
            let source = Duration::from_millis(millis);
            let text = miyu_base::durations::format_seconds(source);
            // 毫秒那一档（`340ms`）不在 `parse_seconds` 的值域里，跳过。其余解不回来
            // 就是红——原来一律跳过，09-23 加了小时那一档、解析器没跟上时它照样绿。
            if text.ends_with("ms") {
                continue;
            }
            let parsed = parse_seconds(&text).unwrap_or_else(|| panic!("{text} 解不回来"));
            let drift = parsed.as_secs_f64() - source.as_secs_f64();
            assert!(
                drift.abs() < 1.0,
                "{text} 解回来是 {parsed:?}，原来是 {source:?}"
            );
        }
    }
}
