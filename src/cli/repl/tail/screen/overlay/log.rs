//! 后台任务日志 → 时间线：流水账的解析与排版。从 `src/cli/repl/tail/screen/overlay.rs` 搬来（09-16 拆分），逻辑未改。

use miyu_engine::tools::subagent::protocol::{parse_log_line, LogEvent, ToolLine};
use miyu_hosts::render::timeline::{glyph_err, glyph_think, StepKind};
/// 读文件末尾 `budget` 字节。从中间切开的第一行丢掉，免得开头是半个字符。
pub(super) fn read_tail(path: &std::path::Path, budget: u64) -> String {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let Ok(mut file) = std::fs::File::open(path) else {
        return String::new();
    };
    let size = file.metadata().map(|meta| meta.len()).unwrap_or(0);
    let from = size.saturating_sub(budget);
    if from > 0 && file.seek(SeekFrom::Start(from)).is_err() {
        return String::new();
    }
    let mut buffer = Vec::new();
    if file.read_to_end(&mut buffer).is_err() {
        return String::new();
    }
    let text = String::from_utf8_lossy(&buffer).into_owned();
    if from > 0 {
        match text.find('\n') {
            Some(index) => text[index + 1..].to_string(),
            None => text,
        }
    } else {
        text
    }
}

/// 日志里的一步——**只是解码产物**，不是渲染模型。
///
/// 渲染模型是 `render::timeline::Step`（三处共用），由 `to_step` 从这儿转出去。
///
/// 抬头一律暗色，绿色留给展开之后的思考正文——和主线那边一个规矩。反过来
/// （抬头绿、正文白）看着像把手比内容还重要，而这一行本来就只是个把手（用户
/// 实测：浮层里思考行和思考展开内容的颜色反了）。所以这儿**没有**「这一步是不是
/// 绿的」这个字段：它曾经在，但从来没人读，只留着一句 `let _ =` 说明为什么不读。
#[derive(Default, Debug)]
pub(super) struct LogStep {
    /// 这一步**是什么**。见 [`StepKind`]。
    pub(super) kind: StepKind,
    pub(super) glyph: String,
    pub(super) head: String,
    pub(super) status: Option<&'static str>,
    pub(super) body: Vec<String>,
    /// 这一步花了多久（`[结果]` 行上带的 `· 1.2s`）。收缩行的 Worked for 靠它加。
    pub(super) elapsed: Option<std::time::Duration>,
    /// 收缩行：收起来的那几步。点开收缩行看到的是它们，每一步再点开才是正文。
    pub(super) inner: Vec<LogStep>,
    /// 日志末尾那个还没有结果的调用：它正在跑。
    pub(super) running: bool,
    /// 日志末尾的 `[准备]`：参数还在流。
    pub(super) preparing: bool,
    /// 这一步的主题（命令全文、路径、检索词——`[工具] 运行命令 · ls` 里 ` · ` 后面
    /// 那段）。点开之后正文第一段是它，不是把抬头再说一遍。
    pub(super) subject: Option<String>,
    /// 这次调用的参数原文。**只有标记流那条路有**（日志行上只留了拼好的抬头）。
    ///
    /// 编辑类工具靠它画 diff：子代理内层的编辑拿不到 `__patch_preview__` 的真
    /// diff，只有参数里那份信封（用户 09-17：「展开后的 diff 渲染也没有」）。
    pub(super) args: Option<String>,
    /// 这一步是哪个工具（`edit` / `run_command` …）。画 diff 要按它判。
    pub(super) tool: Option<String>,
    /// 抬头末尾那段加减行数。**从文本里摘出来单存**：写日志那一侧把它拼成纯文本
    /// `· +3 -1`（那样读日志和订标记流两条路都有），而屏幕上它该是绿加红减
    /// ——和主线、前台浮层一个样子（用户 09-17：「浮层里编辑文件的加减没有
    /// 颜色」）。留在文本里就只能是白的，所以在这儿摘走，画的时候再上色。
    pub(super) diff: Option<(usize, usize)>,
}

/// 把抬头末尾那段 ` · +3 -1` 摘下来。返回加减行数，`head` 里那一段去掉。
fn split_diff_stat(head: &mut String) -> Option<(usize, usize)> {
    let (rest, stat) = head.rsplit_once(" · ")?;
    let (added, removed) = stat.trim().split_once(' ')?;
    let added = added.strip_prefix('+')?.parse().ok()?;
    let removed = removed.strip_prefix('-')?.parse().ok()?;
    *head = rest.to_string();
    Some((added, removed))
}

impl LogStep {
    /// 命令那一步的命令全文。抬头上只有 title，命令归正文（用户 09-17 拍的版）。
    ///
    /// 只有标记流那条路拿得到（参数在那儿）；读日志时抬头里就只剩 title 了。
    ///
    /// **全文**，和主线同一份（`command_from_arguments`）。原来取的是 `tool_subject`
    /// ——那是给抬头当窥视用的，截成了一行 80 字：抬头底下只露一行加「..」，点开
    /// 也还是那一行（用户 09-24 截图）。
    pub(super) fn command_text(&self) -> Option<String> {
        let (tool, args) = (self.tool.as_deref()?, self.args.as_deref()?);
        if !miyu_base::tool_names::is_command_tool(miyu_base::tool_names::tool_event_base_name(
            tool,
        )) {
            return None;
        }
        let command = miyu_hosts::render::command_from_arguments(args);
        (!command.is_empty()).then_some(command)
    }

    /// 抬头底下该露命令吗。
    ///
    /// 模型没给 title 时抬头上**已经是命令本身**了（见 `tool_line_text` 那处
    /// 退路），底下再露一遍就是同一句话说两遍。
    pub(super) fn command_tail(&self) -> Option<String> {
        let args = self.args.as_deref()?;
        miyu_engine::tools::command_peek(args)?;
        self.command_text()
    }
}

/// `运行命令 · ls` → `ls`：抬头里 ` · ` 后面那段是主题。
fn subject_of(text: &str) -> Option<String> {
    text.split_once(" · ")
        .map(|(_, subject)| subject.trim().to_string())
        .filter(|subject| !subject.is_empty())
}

/// 认不出工具（老日志没带工具 id）时用哪个图标。
///
/// **§6.3 第 3(b) 项，用户 09-17 拍板：统一成芯片**——也就是主线那一份
/// （`glyph_tool`）。这边原来硬编码齿轮 `\u{f013}`，而那是主线早就换掉的旧字形：
/// 同一个「认不出的工具」，两块面板画的不是同一个东西。
fn unknown_tool_glyph() -> &'static str {
    miyu_hosts::render::timeline::glyph_tool()
}

/// 这一行用哪个图标：有工具 id 就按 id 挑，没有（老日志）退到通用的那个。
fn glyph_for(line: &ToolLine<'_>) -> String {
    match &line.tool {
        Some(tool) => miyu_hosts::render::tool_glyph_for(&tool).to_string(),
        None => unknown_tool_glyph().to_string(),
    }
}

/// 收缩行的图标。和主线那条 `⌄ Worked for …` 一个样子。
pub(super) const SUMMARY_GLYPH: &str = "⌄";

/// `[统计]` 那一行的图标。它不是工具调用：没有结果行，也永远不该被当成
/// 「末尾那个还没回来的调用」挂上转轮（测具截图：`⠏ 工具调用 3 次 · 运行中`）。
const STATS_GLYPH: &str = "\u{f200}";

/// 把已经走完的那几步收成一行 `⌄ Worked for …`，点开还是那几步。
///
/// 「提示词」那一行钉在最前面不参与收缩——它说的是"要干什么"，不是过程。
fn collapse_log_segment(steps: &mut Vec<LogStep>) {
    // 这一段从哪儿开始：**上一段正文之后**；没说过话就是提示词之后。原来一律
    // 取"提示词后面第一步"，第二次开口时把上一个收缩行、上一段正文连同新的几步
    // 全卷进一个收缩行——面板里永远只剩开头那一个 Worked for（用户实测）。
    let from = steps
        .iter()
        .rposition(|step| step.kind == StepKind::Speech)
        .map(|index| index + 1)
        .unwrap_or_else(|| {
            steps
                .iter()
                .position(|step| step.kind != StepKind::Prompt)
                .unwrap_or(steps.len())
        });
    if steps.len() <= from + 1 {
        return;
    }
    let collapsed: Vec<LogStep> = steps
        .drain(from..)
        // 「准备执行」只有作为**此刻**那一条时才算数（见函数末尾那道
        // `retain`）。收段是把已经过去的那几步卷起来，里面的准备全是过去式：
        // 留着的话收缩行点开是一串「准备执行」，而且它们 `kind == Tool`，
        // 连 `N tools` 都跟着虚高——真机日志实测 2 次工具报成 **17 tools**
        //（用户 09-17 截图里那一屏「准备执行」就是点开收缩行看到的）。
        //
        // 末尾那道 `retain` 管不到这儿：它只扫顶层，而这几步已经被 `drain`
        // 进收缩行的肚子里了。
        .filter(|step| !step.preparing)
        .collect();
    let tools = collapsed
        .iter()
        // `[统计]` 不算——同文件上面那条注释自己就写着「它不是工具调用：没有结果
        // 行」，而收缩行却把它数进 `N tools` 里。那一行的内容还正好是「工具调用
        // 3 次」，于是报出来的数比真跑过的多一个（报告 §6.3 第 2 项，前台一直
        // 不算）。要改回去就把 `StepKind::Stats` 加回来。
        .filter(|step| step.kind == StepKind::Tool)
        .count();
    let thoughts = collapsed
        .iter()
        .filter(|step| step.kind == StepKind::Thought)
        .count();
    let errors = collapsed
        .iter()
        .filter(|step| step.status == Some("err"))
        .count();
    // 这一段花了多久：每一步自己的耗时加起来（流水账里没有时间戳，只有 `[结果]`
    // 行上带的那个数）。思考没记时，所以这是下限——总比"什么都不报"强。
    let elapsed = collapsed
        .iter()
        .filter_map(|step| step.elapsed)
        .fold(std::time::Duration::ZERO, |sum, step| sum + step);
    let summary = miyu_hosts::render::timeline::summary_line(
        elapsed,
        miyu_hosts::render::timeline::Counts {
            tools,
            thoughts,
            errors,
        },
    );
    // 收起来的每一步原样留着（`inner`），渲染时各自登记成块——点开收缩行是
    // 时间线，时间线里每一步再点开才是它的正文。原来只把抬头串成一段文字，
    // 工具输出和思考全文在收缩那一刻就没了（用户实测：会丢失内容）。
    steps.push(LogStep {
        kind: StepKind::Fold,
        glyph: SUMMARY_GLYPH.to_string(),
        head: summary,
        elapsed: Some(elapsed),
        inner: collapsed,
        ..Default::default()
    });
}

/// 这一步是一次工具调用吗——结果与输出只认领这种。
///
/// 正文段和收缩行（`⌄ Worked for …`）都**不是**：它俩一度也被当成工具步，于是
/// 子代理开口说过话之后，下一条 `[结果]` 给收缩行盖了个 `ok`，跟着的 `[输出]`
/// 全贴进正文段里——面板里就是一段话底下拖着几十行裸 grep 输出（用户实测截图）。
fn is_tool_step(step: &LogStep) -> bool {
    step.kind == StepKind::Tool && !step.preparing
}

/// 后台任务的流水账 → 和主线**一模一样**的时间线。
///
/// 流水账是一行一条记录（`[思考] …` / `[工具] …` / `[结果] …`），直接一行一行贴
/// 出来是两个毛病：一是同一次工具调用会出现两遍（叫的时候一条、回来的时候一条），
/// 二是长记录被硬切或者硬折，整条线看着是散的。
///
/// 这里把它折成"步"：工具的调用与结果合成一条（结果只是给它盖个 ok/err），
/// 续行归到上一步的正文里。每一步都是一行窥视，点开才看全文——和主线一个规矩。
pub(super) fn log_steps(text: &str, fold: bool) -> Vec<LogStep> {
    steps_from_events(text.lines().map(parse_log_line), fold)
}

/// 同上，但**事件从哪来不管**——日志行解出来的，还是进度标记解出来的，都行。
///
/// 后台面板有两条进料口：读任务日志（daemon 重启后、老任务，走
/// `parse_log_line`），和直接订 `job.trace` 里的原始标记（走 `from_marker`）。
/// 两条路解出的是**同一个** `LogEvent`（`subagent/protocol.rs` 有一条测试钉着
/// 它们等价），所以攒步这一段只写一份。
pub(super) fn steps_from_events<'a>(
    events: impl Iterator<Item = LogEvent<'a>>,
    // 「过程收起成 Worked for」。关掉就每一步就地留着——面板原来是无条件收的，
    // 于是那个开关在浮层里等于不存在（用户 09-17）。
    fold: bool,
) -> Vec<LogStep> {
    let mut steps: Vec<LogStep> = Vec::new();
    for event in events {
        match event {
            LogEvent::Thought {
                text,
                elapsed,
                continues,
            } => {
                // 连续的思考并成一条：原样贴出来一次思考会在面板里占十几个节点，
                // 那是把一段话排成了梯子（用户原话「思考每一行都有标」）。全文攒
                // 在 `head` 里，点开看。
                //
                // 接不接得上由 `continues` 说，**不由记录边界说**——标记流是逐
                // delta 的，一条记录常常只有一个词。
                match steps.last_mut() {
                    Some(last) if last.kind == StepKind::Thought => {
                        if !continues {
                            last.head.push('\n');
                        }
                        last.head.push_str(&text);
                        if let Some(elapsed) = elapsed {
                            last.elapsed = Some(last.elapsed.unwrap_or_default() + elapsed);
                        }
                    }
                    _ => steps.push(LogStep {
                        kind: StepKind::Thought,
                        glyph: glyph_think().to_string(),
                        head: text.to_string(),
                        elapsed,
                        ..Default::default()
                    }),
                }
            }
            // 刚才那一段思考花了多久。它不是一步，是盖在上一步思考上的一个数
            // ——标记流里没有时间信息，这一条是发的那一侧专门补的。
            LogEvent::ThoughtElapsed(elapsed) => {
                if let Some(last) = steps
                    .iter_mut()
                    .rev()
                    .find(|step| step.kind == StepKind::Thought)
                {
                    last.elapsed = Some(elapsed);
                }
            }
            LogEvent::Prompt(rest) => {
                // 只留第一条：派出去时写一条，Full 档的任务简介又是一条，说的是
                // 同一件事。
                if steps.iter().any(|step| step.kind == StepKind::Prompt) {
                    continue;
                }
                // 交给它的差事。放在最前面，点开就知道这个子代理到底被要求干什么
                // ——否则一条跑了五分钟的后台子代理，面板里只剩它自己的碎碎念。
                let body = rest.split('\u{1}').map(str::to_string).collect::<Vec<_>>();
                // 抬头带上开头那一句：只写一个「差事」的话，这一行看着像个空标签，
                // 不点开根本不知道它派出去干什么。
                let peek = body
                    .iter()
                    .map(|line| line.trim())
                    .find(|line| !line.is_empty())
                    .unwrap_or_default();
                steps.push(LogStep {
                    kind: StepKind::Prompt,
                    glyph: miyu_hosts::render::prompt_glyph().to_string(),
                    head: format!(
                        "{}{}{peek}",
                        miyu_base::i18n::text("prompt", "提示词"),
                        miyu_hosts::render::timeline::PEEK_SEP
                    ),
                    status: None,
                    body,
                    ..Default::default()
                });
            }
            LogEvent::Speech { text, continues } => {
                // 它开口说话了：**前面那一段过程收成一行**，和主线一个规矩
                //（用户：可以把前面已经完成的 timeline 在浮层里缩成 Worked for）。
                // 面板里一路平铺着几十步的话，真正的产出反而被埋在最底下。
                if text.is_empty() {
                    continue;
                }
                match steps.last_mut() {
                    // **一条记录不是一行正文。** 后台面板优先订 `job.trace` 里的
                    // 原始标记流（阶段 4 那条路），而那条流是**逐 delta** 的：一
                    // 条记录常常只有一个词。各占一行的话，正文就成了一句一个台阶
                    //（用户 09-17：「子代理的浮层的正文现在是每个 token 都会换一
                    // 次行」；同一个坑 `subagent/log.rs` 第 40 行还记着上一次，
                    // 那次是日志那条路）。
                    //
                    // 所以换不换行只看 `continues`：真正的换行走它自己的记录
                    // （日志里的 `[正文]`）或续行（`Continuation`）。
                    Some(last) if last.kind == StepKind::Speech => match last.body.last_mut() {
                        Some(tail) if continues => tail.push_str(&text),
                        _ => last.body.push(text.to_string()),
                    },
                    _ => {
                        if fold {
                            collapse_log_segment(&mut steps);
                        }
                        steps.push(LogStep {
                            // 正文没有抬头也没有图标——整段就是内容。
                            kind: StepKind::Speech,
                            glyph: " ".to_string(),
                            head: String::new(),
                            status: None,
                            body: vec![text.to_string()],
                            ..Default::default()
                        });
                    }
                }
            }
            LogEvent::Output(rest) => {
                // 工具吐的东西，挂到刚才那一步的详情里。
                if let Some(step) = steps.iter_mut().rev().find(|step| is_tool_step(step)) {
                    step.body.push(rest.to_string());
                }
            }
            LogEvent::ToolCall(call) => {
                let mut head = call.text.to_string();
                let diff = split_diff_stat(&mut head);
                steps.push(LogStep {
                    kind: StepKind::Tool,
                    glyph: glyph_for(&call),
                    subject: subject_of(&head),
                    head,
                    diff,
                    status: None,
                    body: Vec::new(),
                    tool: call.tool.as_deref().map(str::to_string),
                    args: call.args.as_deref().map(str::to_string),
                    ..Default::default()
                })
            }
            LogEvent::ToolResult { line: result, ok } => {
                // `运行命令 ok · 1.2s · ls`：耗时摘出来单存，抬头照主线的写法
                // 「名字 · 秒数 · 窥视」。
                let (text, elapsed) = result.result_parts();
                // 结果配给最近那次还没有结果的调用：它俩说的是同一件事。
                let matched = steps
                    .iter_mut()
                    .rev()
                    .find(|step| step.status.is_none() && is_tool_step(step));
                match matched {
                    Some(step) => {
                        // 结果只负责给这一步盖个 ok/err（和耗时）。它的正文和调用
                        // 那一行是同一句话（同一个工具、同一份参数），塞进详情里就
                        // 是把抬头又说一遍（用户实测：展开之后第一行和标题一模
                        // 一样）。
                        step.status = Some(if ok { "ok" } else { "err" });
                        // 调用行没带参数（老日志）时，结果那条里也有一份。
                        if step.args.is_none() {
                            step.args = result.args.as_deref().map(str::to_string);
                        }
                        if step.tool.is_none() {
                            step.tool = result.tool.as_deref().map(str::to_string);
                        }
                        if !ok {
                            step.glyph = glyph_err().to_string();
                        }
                        if let Some(elapsed) = elapsed {
                            step.elapsed = Some(elapsed);
                            step.head = with_elapsed(&step.head, elapsed);
                        }
                        let _ = text;
                    }
                    // 没有对应的调用行——Full 档下 `run_command` 的调用事件是不发的
                    //（网页端由结果整块渲染）。那就拿结果这一行自己立一步：图标用
                    // **工具自己的**，正文里那个 ok/err 去掉（状态由 `status` 单独
                    // 盖，留着会读成「运行命令 ok · … · ok」）。
                    None => {
                        let mut head = match elapsed {
                            Some(elapsed) => with_elapsed(&text, elapsed),
                            None => text,
                        };
                        let diff = split_diff_stat(&mut head);
                        steps.push(LogStep {
                            kind: StepKind::Tool,
                            glyph: if ok {
                                glyph_for(&result)
                            } else {
                                glyph_err().to_string()
                            },
                            tool: result.tool.as_deref().map(str::to_string),
                            args: result.args.as_deref().map(str::to_string),
                            subject: subject_of(&head),
                            head,
                            diff,
                            status: Some(if ok { "ok" } else { "err" }),
                            elapsed,
                            ..Default::default()
                        })
                    }
                }
            }
            LogEvent::Preparing(preparing) => {
                // 参数还在流。只有作为日志**末尾**那一行时才是"此刻"，别处的都是
                // 已经过去的准备，收尾时统一扔掉。
                // 图标是那个工具自己的（`[准备] edit\t准备编辑` → 铅笔）。
                steps.push(LogStep {
                    kind: StepKind::Tool,
                    glyph: glyph_for(&preparing),
                    head: preparing.text.to_string(),
                    preparing: true,
                    ..Default::default()
                });
            }
            LogEvent::Stats(rest) => steps.push(LogStep {
                kind: StepKind::Stats,
                glyph: STATS_GLYPH.to_string(),
                head: rest.to_string(),
                status: None,
                body: Vec::new(),
                ..Default::default()
            }),
            LogEvent::Continuation(line) => match steps.last_mut() {
                // 没打标签的行只有跟在"思考"后面时才是续行（一段话里的换行）。
                // 跟在工具后面的那些是内层渲染器自己的进度回声
                //（`工具 #7: 编辑文件 · … ok`），和上一行说的是同一件事，
                // 贴进详情里只会让人以为出了两次（用户：「展开后的内容不太对」）。
                // 正文段也一样：桥按自然段落盘，一段里的换行原样写着（标题、
                // 表格行、列表项都是这么来的），丢掉就是整段缺句子、表格只剩
                // 表头（用户实测截图）。
                // 思考整段攒在 `head` 里（点开看的就是它），正文攒在 `body`
                // 里（每一项是一行）。两处的"另起一行"因此写法不同，但意思
                // 一样：`Speech` / `Thought` 事件是**粘**，续行是**换行**。
                Some(last) if last.kind == StepKind::Thought => {
                    last.head.push('\n');
                    last.head.push_str(&line);
                }
                Some(last) if last.kind == StepKind::Speech => last.body.push(line.to_string()),
                Some(_) => {}
                None => steps.push(LogStep {
                    glyph: " ".to_string(),
                    head: line.trim().to_string(),
                    ..Default::default()
                }),
            },
        }
    }
    // 「准备」只在日志末尾才算数；末尾那个没结果的调用就是正在跑的那个。
    let last = steps.len().saturating_sub(1);
    let mut index = 0;
    steps.retain(|step| {
        let keep = !step.preparing || index == last;
        index += 1;
        keep
    });
    if let Some(step) = steps.last_mut() {
        if step.status.is_none() && is_tool_step(step) {
            step.running = true;
        }
        // 末尾那段思考还没收到「想完了、花了多久」：它正在想。任务停了没有由面板
        // 另外判（见 `Overlay::job_running`）——这儿只看得见过程本身。
        if step.kind == StepKind::Thought && step.elapsed.is_none() {
            step.running = true;
        }
    }
    steps
}

/// 同 [`with_elapsed`]，但秒数是现成的（跑着的那一步自己掐的表）。
pub(super) fn with_elapsed_text(head: &str, secs: &str) -> String {
    match head.split_once(" · ") {
        Some((name, rest)) => format!("{name} · {secs} · {rest}"),
        None => format!("{head} · {secs}"),
    }
}

/// 把耗时插进抬头：`运行命令 · ls` → `运行命令 · 1.2s · ls`（名字后面、窥视前面，
/// 和主线一个次序）。
fn with_elapsed(head: &str, elapsed: std::time::Duration) -> String {
    // 不到十分之一秒的不报：`0.0s` 只是噪音（用户实测）。收缩行照样把它算进总数。
    let Some(secs) = miyu_hosts::render::timeline::reported_seconds(elapsed) else {
        return head.to_string();
    };
    match head.split_once(" · ") {
        Some((name, rest)) => format!("{name} · {secs} · {rest}"),
        None => format!("{head} · {secs}"),
    }
}

/// 一步展开之后看到的东西：头行 + 空行 + 折好行的正文。和主线的 `step_detail`
/// 是同一个形状。
/// 这一步点开之后的**正文**（抬头由 `Step` 自己带，见
/// `render::timeline::step_detail_lines`）。
///
/// 日志那一侧存的是**原文**：正文要按面板宽度折、按类型上色，都在这儿做完再落
/// 进 `Step`——前台那边是在攒步的时候就做好的，两边的差别只有「什么时候做」。
pub(super) fn log_detail_body(step: &LogStep) -> Vec<String> {
    let inner = miyu_hosts::render::timeline::panel_detail_width();
    // 编辑类工具：正文给 **diff**，不给那份结果 JSON——和前台浮层同一条规矩
    //（`timeline::subagent::subagent_tool`）。参数只有标记流那条路带得过来。
    if let (Some(tool), Some(args)) = (step.tool.as_deref(), step.args.as_deref()) {
        if let Some(lines) = miyu_hosts::render::patch_envelope_lines_from_args(tool, args, inner) {
            return lines;
        }
    }
    let color = if step.kind == StepKind::Thought {
        "\x1b[38;5;10m"
    } else {
        ""
    };
    let mut body: Vec<String> = Vec::new();
    // 正文第一段是这一步的**主题**（命令全文、路径、检索词），空一行，然后是输出
    // ——和主线那一步点开一个样子。原来是把抬头（`运行命令 · 5.3s · echo …`）整个
    // 再说一遍（用户实测：命令展开处理异常）。没有主题的（思考、提示词）还是
    // 抬头本身：行里那一份是裁过的，这儿这份是完整的。
    // 命令那一步的正文是**命令全文**。抬头给的是 title（用户 09-17 拍的版），
    // 所以命令只能在这儿露——不然点开看到的是一句 title 再说一遍。
    let command = step.command_text();
    let mut texts: Vec<&str> = Vec::new();
    match command.as_deref().or(step.subject.as_deref()) {
        Some(subject) => {
            texts.push(subject);
            if !step.body.is_empty() {
                texts.push("");
            }
        }
        None => texts.push(step.head.as_str()),
    }
    texts.extend(step.body.iter().map(String::as_str));
    for text in texts {
        if text.trim().is_empty() {
            body.push(String::new());
            continue;
        }
        for piece in miyu_hosts::render::wrap_display_text(text, inner) {
            body.push(format!("\x1b[2m{color}{piece}\x1b[0m"));
        }
    }
    body
}

#[cfg(test)]
#[path = "log_tests.rs"]
mod tests;
