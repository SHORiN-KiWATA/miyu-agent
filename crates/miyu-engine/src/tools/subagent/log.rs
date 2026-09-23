use super::*;

/// 流水账开头写一条「差事」，面板里就是第一步，点开看全文。
///
/// 后台子代理跑起来之后，能看到的全是它自己的动作；它到底被要求干什么，只有
/// 派它出去的那一轮知道。隔十分钟回来看这个面板的人是没有那一轮的。
pub(super) fn write_subagent_prompt_header(log_path: &std::path::Path, prompt: &str) {
    let line = prompt_header_line(prompt);
    if line.is_empty() {
        return;
    }
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .and_then(|mut file| {
            use std::io::Write as _;
            writeln!(file, "{line}")
        });
}

/// prompt → 流水账里那一行。多行压成一行：流水账是按行读的，`\u{1}` 在正文里
/// 不会出现，面板那边照它拆回来。空 prompt 返回空串（不写）。
fn prompt_header_line(prompt: &str) -> String {
    let prompt = prompt.trim();
    if prompt.is_empty() {
        return String::new();
    }
    format!("[提示] {}", prompt.replace('\r', "").replace('\n', "\u{1}"))
}

/// Bridge a detached subagent's progress stream into its job log so
/// `job_status` reads live progress the same way it reads command output.
pub(super) fn spawn_subagent_log_bridge(
    job_id: String,
    log_path: std::path::PathBuf,
) -> crate::tools::ToolProgress {
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        // 思考和正文都是**逐 delta** 来的。一条一行的话日志会变成每行一个词的
        // 字符梯，谁也读不下去（用户实测截图：整屏 `[正文] the` / `[正文] and`）。
        // 攒成段落，遇到别的事件或段落够长了才落盘。
        let mut thinking = StreamBuffer::new("[思考]", "[思考+]");
        let mut speech = StreamBuffer::new("[正文]", "[正文+]");
        // 上一次内层调用是什么时候发出的：结果回来时算耗时写进 `[结果]`。
        // 流水账里没有时间戳，面板那边"这一步花了多久""这一段 Worked for 多久"
        // 只能靠这个（用户实测：后台面板的收缩行没有 Worked for）。
        let mut last_call: Option<std::time::Instant> = None;
        // 这一段思考从什么时候开始的：落成 `[思考]` 行时把时长写在最前面。
        let mut thinking_since: Option<std::time::Instant> = None;
        while let Some(event) = receiver.recv().await {
            let crate::tools::ToolProgressEvent::Message(message) = event else {
                continue;
            };
            // 原始标记上 SSE(网页端据 job_id 渲染子过程流,与前台子代理工具行
            // 同款);人读的行落任务日志(job status 读它)。
            crate::tools::jobs::publish_job_progress(&job_id, &message);
            if let Some(text) = message.strip_prefix("__subagent_metric__") {
                // 制表符分隔：`<给人看的那串>\t<数字>\t<人话>`
                //（见 `SubagentRunner::report_metric`）。中途的量报只刷状态行上
                // 那串数，不落流水账——它一秒来好几次，落进去会把时间线撑满。
                let mut parts = text.split('\t');
                let display = parts.next().unwrap_or_default().trim().to_string();
                let raw = parts.next().and_then(|value| value.trim().parse().ok());
                crate::tools::jobs::set_metric(&job_id, &display, raw);
                continue;
            }
            let mut lines: Vec<String> = Vec::new();
            if let Some(text) = message.strip_prefix("__subagent_reasoning__") {
                flush_stream_buffer(&mut speech, &mut lines);
                if thinking.is_empty() && thinking_since.is_none() {
                    thinking_since = Some(std::time::Instant::now());
                }
                accumulate_stream(&mut thinking, text, &mut lines);
            } else if let Some(text) = message.strip_prefix("__subagent_content__") {
                flush_stream_buffer(&mut thinking, &mut lines);
                accumulate_stream(&mut speech, text, &mut lines);
            } else {
                flush_stream_buffer(&mut thinking, &mut lines);
                flush_stream_buffer(&mut speech, &mut lines);
                let elapsed = if message.starts_with("__subtool_call__") {
                    last_call = Some(std::time::Instant::now());
                    None
                } else if message.starts_with("__subtool_result__") {
                    last_call.take().map(|since| since.elapsed())
                } else {
                    None
                };
                let line = readable_subagent_log_line_timed(&message, elapsed);
                if !line.is_empty() {
                    lines.push(line);
                }
            }
            stamp_thought_lines(&mut lines, &mut thinking_since);
            if lines.is_empty() {
                continue;
            }
            let _ = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_path)
                .and_then(|mut file| {
                    use std::io::Write as _;
                    for line in &lines {
                        writeln!(file, "{line}")?;
                    }
                    Ok(())
                });
        }
        // 收尾：最后那段没等到分隔符的也要落盘。
        let mut lines: Vec<String> = Vec::new();
        flush_stream_buffer(&mut thinking, &mut lines);
        flush_stream_buffer(&mut speech, &mut lines);
        stamp_thought_lines(&mut lines, &mut thinking_since);
        if !lines.is_empty() {
            let _ = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_path)
                .and_then(|mut file| {
                    use std::io::Write as _;
                    for line in &lines {
                        writeln!(file, "{line}")?;
                    }
                    Ok(())
                });
        }
    });
    crate::tools::ToolProgress::new(sender)
}

/// 刚落下来的 `[思考]` 行带上这段想了多久：`[思考] 1.2s\t正文`。面板那边按它
/// 报「已思考 · 1.2s」，收缩行的 Worked for 也把它算进去。
fn stamp_thought_lines(lines: &mut [String], thinking_since: &mut Option<std::time::Instant>) {
    for line in lines.iter_mut() {
        let Some(text) = line.strip_prefix("[思考] ") else {
            continue;
        };
        let Some(since) = thinking_since.take() else {
            break;
        };
        let secs = miyu_base::durations::format_seconds(since.elapsed());
        *line = format!("[思考] {secs}\t{text}");
    }
}

/// 一直不出现空行时，最多攒这么久就落一条。
///
/// 原来只有 600 字符这一道闸。**实测（`testkit/tui/bg_latency.py`）：模型想一大段
/// 不带空行的时候，后台面板会整整 12.0 秒不动一下**——
///
/// ```text
///   2419 ms  [思考]  20 字符
///  14464 ms  [思考] 612 字符   ← 中间 12.0 秒，面板上什么都没有
/// ```
///
/// 渲染统一那份报告（§6.1）把这笔账记在「150ms 文件轮询」上，据此提出加一条 IPC
/// 直接订事件流。实测说明**主项不是轮询，是这道 600 字符的闸**：轮询最多耽误
/// 150ms，而这道闸耽误了 12 秒。
///
/// 加一道时间闸就够——面板把连续的思考并成一步、抬头取最新那一段当窥视，所以
/// 「落得更碎」正好是「它还活着」那个指示要的东西。
pub(super) const STREAM_FLUSH_INTERVAL: std::time::Duration = std::time::Duration::from_millis(300);

/// 把一小段流式文本攒进缓冲，攒够一个自然段（空行）、够长了、或者攒够久了就落
/// 一条。
///
/// **落下来的每一条都是原样的一截，不 `trim`。** 一条日志行不等于一行正文：
/// 上面那道 300ms 的时间闸会把一句话切成好几条，读那侧要原样拼回去；两头的
/// 空白一掐，英文就会粘成 `the quickbrown fox`。段里的换行也原样留着——写进
/// 日志就是续行，读那侧按续行拼（`LogEvent::Continuation`）。
pub(super) fn accumulate_stream(stream: &mut StreamBuffer, text: &str, lines: &mut Vec<String>) {
    if stream.buffer.is_empty() {
        stream.since = Some(std::time::Instant::now());
    }
    stream.buffer.push_str(text);
    while let Some(index) = stream.buffer.find("\n\n") {
        let chunk: String = stream.buffer.drain(..index + 2).collect();
        stream.push(lines, &chunk);
        stream.since = Some(std::time::Instant::now());
    }
    // 一直不出现空行的话也不能无限攒下去——攒够长、或者攒够久，都得落。
    let stale = stream
        .since
        .is_some_and(|at| at.elapsed() >= STREAM_FLUSH_INTERVAL);
    if !stream.buffer.trim().is_empty() && (stream.buffer.chars().count() > 600 || stale) {
        let chunk = std::mem::take(&mut stream.buffer);
        stream.push(lines, &chunk);
        stream.since = None;
    }
}

/// 一条流（思考／正文）攒到哪儿了。
pub(super) struct StreamBuffer {
    tag: &'static str,
    /// 同一段里的续截用的标签（`[正文+]`）——读那侧据此粘回去，而不是另起一行。
    continued_tag: &'static str,
    buffer: String,
    since: Option<std::time::Instant>,
    /// 这一段已经落过一截了：下一截是**续**，不是新的一行。
    continued: bool,
}

impl StreamBuffer {
    pub(super) fn new(tag: &'static str, continued_tag: &'static str) -> Self {
        Self {
            tag,
            continued_tag,
            buffer: String::new(),
            since: None,
            continued: false,
        }
    }

    fn push(&mut self, lines: &mut Vec<String>, chunk: &str) {
        if chunk.is_empty() {
            return;
        }
        let tag = if self.continued {
            self.continued_tag
        } else {
            self.tag
        };
        lines.push(format!("{tag} {chunk}"));
        // 攒够一个自然段（结尾是空行）落的那一截，本身就把段收掉了；
        // 中途被时间闸／长度闸切开的才算"还没说完"。
        self.continued = !chunk.ends_with("\n\n");
    }

    pub(super) fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    /// 测试用：假装这一段已经攒够久了。
    #[cfg(test)]
    pub(super) fn age(&mut self, by: std::time::Duration) {
        self.since = self.since.map(|at| at - by);
    }
}

/// 把缓冲里剩的那截落成一条（别的事件来了、或者收尾了）。
pub(super) fn flush_stream_buffer(stream: &mut StreamBuffer, lines: &mut Vec<String>) {
    if stream.buffer.trim().is_empty() {
        stream.buffer.clear();
        stream.continued = false;
        return;
    }
    // 同 `accumulate_stream`：原样落，拼接的活儿归读那侧。
    let chunk = std::mem::take(&mut stream.buffer);
    stream.push(lines, &chunk);
    // 别的事件插进来了：这一段到此为止，下次是新的一行。
    stream.continued = false;
}
/// 这条结果标记自己带着耗时吗（09-17 起发的那一侧会带）。
fn has_millis(json: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(json.trim())
        .ok()
        .and_then(|value| value.get("ms").and_then(serde_json::Value::as_u64))
        .is_some()
}

/// 内层工具事件压成一句人话。原样贴 JSON 的话日志里全是转义引号。
///
/// 拼法在 `protocol` 里，和「标记直接解成事件」那条路共用同一份——两处各拼一遍
/// 的话，同一次调用在两条路上的抬头会慢慢长得不一样。
fn subtool_summary(json: &str) -> String {
    super::protocol::tool_line_text(json)
}

/// 结果事件摊成 `[结果]` + 若干 `[输出]`。
///
/// 只写 `[结果]` 的话，面板里那一步点开是空的——那行里已经有的东西再说一遍而已
/// （用户实测：浮层里这些工具展开都没内容）。真正值得看的是工具吐了什么，而
/// `__subtool_result__` 本来就带着（`clip_detail` 已经截过）。这儿再收一道，
/// 免得一条 8KB 的输出把流水账撑成日志本体。
fn subtool_result_lines(json: &str, elapsed: Option<Duration>) -> String {
    let mut out = format!("[结果] {}", subtool_summary(json));
    // 耗时紧跟在 ok/err 后面：`运行命令 ok · 1.2s · ls`。面板去掉 ok 之后就是
    // 主线那一行的样子（名字 · 秒数 · 窥视）。
    // 再短也写：一段里几个快工具加起来才够得上一个 Worked for。
    //
    // **标记自己带了 `ms` 就别再加一遍**：09-17 起掐表的是发标记那一侧（那样订
    // 标记流的面板也有耗时），这儿那份 `last_call` 只给老 daemon 的标记兜底。
    let elapsed = elapsed.filter(|_| !has_millis(json));
    if let Some(elapsed) = elapsed {
        let secs = miyu_base::durations::format_seconds(elapsed);
        for status in [" ok", " err"] {
            if let Some(index) = out.find(&format!("{status} · ")) {
                out.insert_str(index + status.len(), &format!(" · {secs}"));
                break;
            }
            if out.ends_with(status) {
                out.push_str(&format!(" · {secs}"));
                break;
            }
        }
    }
    // 拆法与订标记流那一侧共用（`protocol::result_output_lines`），两条路露的输出
    // 一行不差。
    for line in super::protocol::result_output_lines(json) {
        out.push_str("\n[输出] ");
        out.push_str(&line);
    }
    out
}

/// 同上，`elapsed` 是这次内层调用从发出到结果回来花的时间（只有结果事件带）。
pub(super) fn readable_subagent_log_line_timed(message: &str, elapsed: Option<Duration>) -> String {
    if let Some(name) = message.strip_prefix("__subtool_preparing__") {
        // 参数还在流：面板把它当"正在准备"那一行。它不是一步，只有作为日志末尾
        // 那一行时才有意义，读日志的人看到它也只当"刚才准备过"。
        let name = name.trim();
        let phase = crate::tools::preparing_phase(name).unwrap_or("");
        return format!("[准备] {name}\t{phase}");
    }
    // 段末那条耗时**不进流水账**：日志那条路自己掐表，写的是 `[思考] 1.2s\t…`
    //（`stamp_thought_lines`）。两边各报一次的话，面板会把同一段的时间加两遍。
    if message.starts_with(super::protocol::REASONING_DONE_MARKER) {
        return String::new();
    }
    // 会话化子代理(09-18)报的子会话 id 是给父回合落库用的,不是过程;结果段里
    // 已经带 `session: …`,流水账不重复写。
    if message.starts_with(super::SUBAGENT_SESSION_MARKER) {
        return String::new();
    }
    // 原始标记是**逐 delta** 的一截，不是一行：写成 `+` 标签，读那侧才会粘回去
    // 而不是一句一个台阶。也不 trim——两头的空白就是词边界。
    if let Some(text) = message.strip_prefix("__subagent_reasoning__") {
        if text.is_empty() {
            return String::new();
        }
        return format!("[思考+] {text}");
    }
    if let Some(text) = message.strip_prefix("__subagent_content__") {
        if text.is_empty() {
            return String::new();
        }
        return format!("[正文+] {text}");
    }
    if let Some(text) = message.strip_prefix("__subtool_call__") {
        return format!("[工具] {}", subtool_summary(text));
    }
    if let Some(text) = message.strip_prefix("__subtool_result__") {
        return subtool_result_lines(text, elapsed);
    }
    if let Some(text) = message.strip_prefix("__subagent_brief__") {
        // 任务简介（Full 档才发）里带着 prompt——正是面板第一步要的那份。
        // 认下来，免得它以无标签原文的身份漏进流水账。
        let prompt = serde_json::from_str::<serde_json::Value>(text.trim())
            .ok()
            .and_then(|value| {
                value
                    .get("prompt")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_default();
        return prompt_header_line(&prompt);
    }
    // 中途的量报只用来刷标题和状态行，不进流水账——每调一次工具记一条
    // 「统计」的话，面板里的时间线会被这些节点撑满。跑完那一次走
    // `__subagent_stats__`，那条是留底的。
    if message.starts_with("__subagent_metric__") {
        return String::new();
    }
    if let Some(text) = message.strip_prefix("__subagent_stats__") {
        return format!("[统计] {}", text.trim());
    }
    message.trim().to_string()
}
