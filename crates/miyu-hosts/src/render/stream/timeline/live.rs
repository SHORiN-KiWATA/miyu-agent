//! live 区：还在跑的那几行，以及正文／详情的排版帮手。
//!
//! 从 `timeline.rs` 搬来（09-16 拆分，那份超过了文件规模基线）。只搬不改。

use super::*;

/// 压缩上下文收成的那一块：合着一行 `› 上下文已压缩 · …`，点开是摘要全文（暗色）。
/// 手动 `/compact` 和回合里的自动压缩都走它；摘要是空的就只留那一行提示。
pub fn write_compact_summary<W: std::io::Write>(
    writer: &mut W,
    head: &str,
    summary: &str,
) -> std::io::Result<()> {
    let indent = indent();
    if summary.trim().is_empty() {
        writeln!(writer, "\x1b[2m{indent}{} {head}\x1b[0m", glyph_notice())?;
        return writeln!(writer);
    }
    let mut expanded = vec![format!("\x1b[2m{indent}⌄ {head}\x1b[0m"), String::new()];
    expanded.extend(
        wrap_detail(summary.trim())
            .into_iter()
            .map(|line| format!("\x1b[2m{indent}{DETAIL_INDENT}{line}\x1b[0m")),
    );
    expanded.push(String::new());
    blocks::write_expandable(writer, expanded, |writer| {
        writeln!(writer, "\x1b[2m{indent}› {head}\x1b[0m")?;
        writeln!(writer)
    })
}

/// 面板里它说的正文：先过一遍 markdown，再按宽度折行。原来是裸文本——`**加粗**`
/// 的星号、反引号原样露着（用户实测截图：浮层里正文没有 md 渲染）。主线正文走的
/// 是同一套行渲染器，两边长相才一致。
pub fn render_speech_lines(text: &str, width: usize) -> Vec<String> {
    // 代码块、表格、公式问的是「终端多宽」——面板里得按面板宽度答，不然按整屏
    // 排完再折进面板就是碎行和大片空白（用户实测截图）。渲染完把宽度还回去。
    let previous = crate::render::cols_override();
    crate::render::set_cols_override(width.clamp(20, u16::MAX as usize) as u16);
    let mut renderer = crate::render::MarkdownLineRenderer::new();
    let mut rendered = String::new();
    for line in text.lines() {
        let piece = renderer.render_line(line);
        if piece.is_empty() {
            continue;
        }
        rendered.push_str(&piece);
        if !piece.ends_with('\n') {
            rendered.push('\n');
        }
    }
    let rest = renderer.flush();
    rendered.push_str(&rest);
    crate::render::set_cols_override(previous);
    rendered
        .lines()
        .flat_map(|line| {
            if line.trim().is_empty() {
                return vec![String::new()];
            }
            crate::render::wrap_display_text(line, width)
        })
        .collect()
}

/// 去掉 inline 那套从属装饰（`↳` / `│`）。
///
/// 时间线已经用连线表达了"这几行属于上面那一步"，再套一层箭头和竖条就是同一件事
/// 说两遍；两套缩进还对不齐，看着就是乱的。用户原话：「命令展开内容的对齐有问题，
/// 我觉得是不是没必要有那个箭头和竖线」。
///
/// 行首的转义序列（颜色）和空白（缩进）原样留着，只摘掉那一个记号。
pub(crate) fn undecorate(lines: Vec<String>) -> Vec<String> {
    lines
        .into_iter()
        .map(|line| {
            let mut head = String::new();
            let mut rest = line.as_str();
            loop {
                if let Some(len) = crate::render::escape_len(rest) {
                    head.push_str(&rest[..len]);
                    rest = &rest[len..];
                    continue;
                }
                match rest.strip_prefix(' ') {
                    Some(tail) => {
                        head.push(' ');
                        rest = tail;
                    }
                    None => break,
                }
            }
            for marker in ["↳ ", "│ ", "↳", "│"] {
                if let Some(tail) = rest.strip_prefix(marker) {
                    rest = tail;
                    break;
                }
            }
            head.push_str(rest);
            head
        })
        .collect()
}

/// 正文的左边距。
///
/// 时间线的节点在第 2 列，用户消息的竖条在第 0 列、文字在第 2 列——正文贴着第 0
/// 列的话整屏只有它一条不在同一条基准线上。缩进两格，左边就有了一条统一的
/// 装订边。
pub fn indent_body(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    let width = body_width();
    let mut out = String::with_capacity(text.len() + 8);
    let mut rest = text;
    loop {
        let (line, tail) = match rest.find('\n') {
            Some(position) => (&rest[..position], Some(&rest[position + 1..])),
            None => (rest, None),
        };
        push_wrapped(&mut out, line, width);
        match tail {
            Some(tail) => {
                out.push('\n');
                if tail.is_empty() {
                    break;
                }
                rest = tail;
            }
            None => break,
        }
    }
    out
}

/// 正文能用多宽。
///
/// 就是 `command_terminal_width()` 本身，不再减缩进：那个数已经把左右两条边距
/// 都刨掉了，表格、代码块也都是按它排的。这儿再减一次，就会把刚好排满的表格
/// 又折一道——比不折还难看。
fn body_width() -> usize {
    crate::render::command_terminal_width().max(20)
}

/// 一行正文：先按正文宽折好，每一折都自带缩进。
///
/// **折行得自己折**。交给缓冲硬折的话续行从第 0 列开始，装订边在那一行断掉，
/// 看着就是"左边莫名其妙冒出半句话"——用户报的「严重的软换行问题」就是这个。
fn push_wrapped(out: &mut String, line: &str, width: usize) {
    // 行尾的 `\r` 不算内容，折完再补回去（图片占位行走的是 `\r\n`）。
    let (line, carriage) = match line.strip_suffix('\r') {
        Some(stripped) => (stripped, true),
        None => (line, false),
    };
    if line.is_empty() {
        if carriage {
            out.push('\r');
        }
        return;
    }
    // 带图形传输段的行一个字都不折。宽度是按转义序列跳过算的（`escape_len`
    // 现在认 APC），理论上不会折到它头上——但折错一次的代价是整张图不出来，
    // 这条便宜的保险值得留着。
    if line.contains("\x1b_G") {
        out.push_str(INDENT);
        out.push_str(line);
        if carriage {
            out.push('\r');
        }
        return;
    }
    for (index, piece) in crate::render::wrap_display_text(line, width)
        .into_iter()
        .enumerate()
    {
        if index > 0 {
            // 折出来的换行埋个标记：全屏缓冲靠它知道这两行可以并回一条逻辑行，
            // 改窗口宽度时才重排得了（见 `SOFT_WRAP_MARKER`）。
            //
            // 只在全屏那条路上埋。inline 下字节流必须逐字节和以前一样——全屏是
            // 可选项，为它往所有人的终端里塞标记是不能接受的（`tui_blocks` 的
            // 最后一组测试钉的就是这条）。
            if crate::render::blocks::enabled() {
                out.push_str(crate::render::blocks::SOFT_WRAP_MARKER);
            }
            out.push('\n');
        }
        out.push_str(INDENT);
        out.push_str(&piece);
    }
    if carriage {
        out.push('\r');
    }
}

/// 正文那几行：前后各一行空，每行缩进到竖线右边。
pub(super) fn indented_body(body: &[String]) -> Vec<String> {
    let indent = indent();
    let mut lines = Vec::with_capacity(body.len() + 2);
    lines.push(String::new());
    lines.extend(body.iter().map(|line| {
        if line.trim().is_empty() {
            String::new()
        } else {
            format!("{indent}{DETAIL_INDENT}{line}")
        }
    }));
    lines.push(String::new());
    lines
}

impl StreamRenderer {
    /// 给等待转轮用的 live 画面：已完成的步骤照原样，后面接上**正在进行**的
    /// 那些（带转轮标记，由 `wait_spinner` 画上动画字形）。
    ///
    /// `current` 是一串，不是一条：并行派出去的几个子代理各占一行，各自点得开
    /// 自己的面板。压成一行加个 `+2` 的话，看着就像"只能跑一个"。
    pub(crate) fn timeline_live(&self, current: Vec<LiveRow>) -> (String, Option<String>) {
        // 已经跑完的那几步**也挂着块**：它们各自的 id 在收进来时就登记好了，
        // 收成 `Worked for …` 之后用的还是同一个，展开状态跟着走。
        let mut steps: Vec<String> = self.timeline.steps[self.timeline.committed..]
            .iter()
            .map(|step| step_rows(step, step.overlay.or(step.block)))
            .collect();
        let current_is_empty = current.is_empty();
        for LiveRow {
            line,
            target,
            tail,
            open,
        } in current
        {
            // 正在跑的这一步**不带 logo**：点阵转轮会落在那一列上，跑完了
            // 收进 `steps` 时才换回静态图标。
            //
            // 外面再包一层块标记：正在想的时候也该点得开看到「想到哪儿了」，
            // 不必等它结束。标记不占显示宽度，转轮那边照常裁剪。
            // 跑着的是子代理时，点开该进**它的面板**，不是就地展开一段窥视——
            // 那条线还在长，就地展开的行数每秒都在变。
            let (begin, close) = match target {
                Some(id) => (blocks::begin_marker_in(id, open), blocks::END_MARKER),
                None => (String::new(), ""),
            };
            // 转轮落在**左边距**那一列（第 0 列），logo 留在自己那一列——
            // 原来转轮顶掉 logo 的位置，跑完再换回来，一行两副面孔。
            //（用户：左边不是有一个边距吗，`<转轮><logo><抬头>` 就不用替代 logo 了。）
            // 抬头已经滚出屏幕的思考：正文行上不放转轮（用户 09-17：转轮跟着正文
            // 往下走反而碍眼），行首那格留空，列位不变。
            let marker = if self
                .thought_stream
                .is_some_and(|stream| stream.heading_flushed)
            {
                crate::render::wait_spinner::BLOCK_MARKER_IDLE
            } else {
                crate::render::wait_spinner::BLOCK_MARKER
            };
            let mut row = format!("{marker}{begin}{line}");
            // 底下跟着的几行（跑着的命令此刻的输出）和这一行是同一项：连线从
            // 它们中间穿过去。块的结束标记放在最后一行之后——点开的时候展开
            // 内容把抬头和这几行**一起**换掉（用户：如果展开命令的话就替换掉
            // 那个内容刷新行）。
            for extra in tail {
                row.push('\n');
                row.push_str(&rail_prefix());
                row.push_str(&extra);
            }
            row.push_str(close);
            steps.push(row);
        }
        // 什么都没在跑（上一个工具刚回来、下一次模型请求还在路上）：转轮独自
        // 落在 logo 那一列上。空着的话 live 区整个消失，等 `reasoning.start` 才
        // 回来——网络那一秒里整条时间线闪没了又闪回来。
        let lone = current_is_empty && (!steps.is_empty() || self.timeline.committed > 0);
        let mut lines = thread(steps);
        if lone {
            // 转轮在左边距、连线照常延续：`⠋ │`。
            lines.push(format!(
                "{}{RAIL}",
                crate::render::wait_spinner::BLOCK_MARKER
            ));
        }
        if lines.is_empty() {
            return (String::new(), None);
        }
        // 静态版：前面的步骤已经落进 scrollback 了，live 区从一根连线接上去。
        // 抬头已经滚出去的思考除外：转轮行直接接在刚滚出去的那行正文底下，中间不空一根线。
        if self.timeline.committed > 0
            && !self
                .thought_stream
                .is_some_and(|stream| stream.heading_flushed)
        {
            lines.insert(0, rail());
        }
        (String::new(), Some(lines.join("\n")))
    }

    /// 把「正在进行」那几行的可展开内容刷一遍。id 保持不变。
    ///
    /// **一个工具一块**。并行跑的时候，几行共用同一个 id 会让展开层把同一块内容
    /// 插好几遍——行号、偏移、点击命中全跟着错，表现出来就是"所有工具行都点不
    /// 开了"（用户实测）。
    pub(crate) fn refresh_live_block(&mut self) {
        // 静态版没有块：登记了也没人点。
        if !self.caps().expandable {
            return;
        }
        if !self.reasoning_text.trim().is_empty() || self.tool_stats.is_empty() {
            // 在想（或者什么都没跑）：还是那一块。
            let lines = self.live_block_lines();
            if lines.is_empty() {
                return;
            }
            match self.live_block {
                Some(id) => blocks::update(id, String::new(), lines),
                None => self.live_block = blocks::register(lines),
            }
            return;
        }
        let entries: Vec<(String, Vec<String>)> = self
            .ordered_tool_stats()
            .into_iter()
            .map(|(name, stats)| (name.clone(), self.live_tool_lines(name, stats)))
            .collect();
        for (name, lines) in entries {
            if lines.is_empty() {
                continue;
            }
            match self.live_tool_blocks.get(&name).copied() {
                Some(id) => blocks::update(id, String::new(), lines),
                None => {
                    if let Some(id) = blocks::register(lines) {
                        self.live_tool_blocks.insert(name, id);
                    }
                }
            }
        }
    }

    /// 跑着的某一个工具点开能看到什么。
    fn live_tool_lines(&self, name: &str, stats: &crate::render::ToolStats) -> Vec<String> {
        let indent = indent();
        let mut lines = vec![step_line(tool_glyph(name), &self.display_tool_name(name))];
        // 跑着的命令：完整命令 + 此刻为止的输出，每帧重算——展开着的那一块就
        // 跟着输出一起长。它的输出在 `command_display` 里，`stats` 上只有一句
        // 窥视（用户实测：命令展开后没有流式输出，展开内容居然是窥视行）。
        if crate::render::is_command_tool(name) {
            if let Some(display) = self.command_display.as_ref() {
                lines.extend(indented_body(&display.live_detail(detail_width())));
                return lines;
            }
        }
        // 工具已经吐出详情（比如编辑文件的 diff）就直接给：等这一段过程收完
        // 才看得到的话，"用了工具"和"看得到它改了什么"之间隔着整段回复
        //（用户实测：diff 是在 AI 正文回复结束之后才有）。
        if !stats.detail.is_empty() {
            lines.extend(indented_body(&stats.detail));
            return lines;
        }
        lines.push(String::new());
        if let Some(subject) = stats.peek.as_deref().or(stats.subject.as_deref()) {
            for piece in wrap_detail(subject) {
                lines.push(format!("\x1b[2m{indent}{DETAIL_INDENT}{piece}\x1b[0m"));
            }
        }
        if let Some(progress) = stats.progress.as_deref() {
            for line in progress.lines().filter(|line| !line.trim().is_empty()) {
                for piece in wrap_detail(line) {
                    lines.push(format!("\x1b[2m{indent}{DETAIL_INDENT}{piece}\x1b[0m"));
                }
            }
        }
        lines.push(String::new());
        lines
    }

    /// 正在进行那一步点开能看到什么：想到哪儿了 / 这个工具在忙什么。
    fn live_block_lines(&self) -> Vec<String> {
        let indent = indent();
        if !self.reasoning_text.trim().is_empty() {
            let mut lines = vec![
                step_line(
                    glyph_think(),
                    &format!("{} · {}", t("thinking", "思考中"), self.reasoning_tokens),
                ),
                String::new(),
            ];
            // `wrap_detail` 吐的是**没有缩进**的行——步那条路上缩进由
            // `step_detail` 统一加，而这儿是直接当块内容用的，得自己加。
            // 不加的话点开之后正文贴着第 0 列，比它的抬头还靠左。
            lines.extend(
                self.thought_rows_all().into_iter().map(|line| {
                    format!("{THOUGHT_BODY_STYLE}{indent}{DETAIL_INDENT}{line}\x1b[0m")
                }),
            );
            lines.push(String::new());
            return lines;
        }
        let mut lines = Vec::new();
        for (name, stats) in self.ordered_tool_stats() {
            lines.push(step_line(tool_glyph(name), &self.display_tool_name(name)));
            if let Some(subject) = stats.peek.as_deref().or(stats.subject.as_deref()) {
                lines.push(format!("    \x1b[2m{subject}\x1b[0m"));
            }
            if let Some(progress) = stats.progress.as_deref() {
                lines.extend(
                    progress
                        .lines()
                        .filter(|line| !line.trim().is_empty())
                        .map(|line| format!("    \x1b[2m{line}\x1b[0m")),
                );
            }
        }
        lines
    }

    /// live 区：已完成的步骤 + 正在做的那一件。
    pub fn timeline_waiting(&self) -> (String, Option<String>) {
        let width = crate::render::command_terminal_width();
        // 顺序即优先级：准备态 → 正在跑的工具 → 正在想。准备态排最前，
        // 因为它一定会被后面两者之一替换掉，本来就是个占位。
        let current: Vec<LiveRow> = if let Some((glyph, prepare)) = self.timeline_preparing_line() {
            // 准备态还没有内容可展开（参数才刚开始流），不替用户开。
            vec![LiveRow {
                line: format!("{glyph} {prepare}"),
                target: self.live_block,
                tail: Vec::new(),
                open: false,
            }]
        } else if !self.tool_stats.is_empty() {
            self.timeline_running_tool_lines()
        } else if self.reasoning_started_at.is_some() {
            if let Some(stream) = self.thought_stream {
                // 边想边往下流：还没滚进 scrollback 的正文整段挂在抬头底下。抬头还
                // 在时转轮就挂在它上面、计数实时；抬头滚出去了，转轮落在露着的第一
                // 行正文的左边距上。
                let mut lines =
                    style_thought_rows(self.thought_rows_range(stream.flushed_rows, usize::MAX));
                let line = if !stream.heading_flushed {
                    format!("{} {}", glyph_think(), self.thinking_head())
                } else if lines.is_empty() {
                    RAIL.to_string()
                } else {
                    format!("{RAIL} {}", lines.remove(0))
                };
                vec![LiveRow {
                    line,
                    target: self.live_block,
                    tail: lines,
                    open: true,
                }]
            } else if self.reasoning_mode != ReasoningDisplayMode::Full
                && self.thinking_scroll_lines > 0
            {
                // 「展开思考内容」关着：抬头底下开一扇窗，滚着露最近几行（用户
                // 09-17：「思考行不是单行窥视，而是有滚动」）；想完收成一行
                // `已思考 · …`（那一步照旧在时间线里）。行数按
                // `display.thinking_scroll_lines`。
                vec![LiveRow {
                    line: format!("{} {}", glyph_think(), self.thinking_head()),
                    target: self.live_block,
                    tail: self.thought_window_rows(),
                    open: false,
                }]
            } else {
                // 给窥视留下的宽度：整屏减掉「  │ ✳ 思考中 · 320 词元 · 7.5s  」
                vec![LiveRow {
                    line: format!(
                        "{} {}",
                        glyph_think(),
                        self.timeline_thinking_line(width.saturating_sub(46).max(12))
                    ),
                    target: self.live_block,
                    tail: Vec::new(),
                    // 「展开思考内容」开着时，**正在想**的这一步也是展开的：
                    // 点开看到的是它想到哪儿了（`live_block_lines` 每帧重灌）。
                    open: self.reasoning_mode == ReasoningDisplayMode::Full,
                }]
            }
        } else {
            Vec::new()
        };
        self.timeline_live(current)
    }

    /// 准备态那一行：`准备编辑 · 1.2s`。没有准备态就返回 `None`。
    pub(crate) fn timeline_preparing_line(&self) -> Option<(&'static str, String)> {
        if let Some(started_at) = self.preparing_question_started_at {
            return Some((
                tool_glyph("ask_question"),
                format!(
                    "{} · {}",
                    t("Preparing question", "准备问题"),
                    format_seconds(started_at.elapsed())
                ),
            ));
        }
        let (phase, glyph, started_at) = self.tool_preparing?;
        // 工具已经开跑了就不再报准备——那一行该让给真正的工具。
        if !self.tool_stats.is_empty() {
            return None;
        }
        // 图标是那个工具自己的：准备编辑挂铅笔、准备执行挂 `$`，和它跑起来之后
        // 那一步一个样子（用户 09-14 要求）。
        Some((
            glyph,
            format!("{phase} · {}", format_seconds(started_at.elapsed())),
        ))
    }

    /// 正在跑的**每一个**工具各一行，连同它点开之后该去哪儿。
    ///
    /// 原来是把第一个拿出来、后面缀个 `+2`。并行派三个子代理时屏幕上就只有一行
    /// 在转，看着像"只能跑一个"——而它们确实在同时跑（用户问的就是这个）。
    pub(crate) fn timeline_running_tool_lines(&self) -> Vec<LiveRow> {
        let expand_tools = self.tool_call_mode == crate::render::ToolCallDisplayMode::Full;
        let ordered = self.ordered_tool_stats();
        if ordered.is_empty() {
            return vec![LiveRow {
                line: format!("{} {}", glyph_tool(), t("running", "运行中")),
                target: self.live_block,
                tail: Vec::new(),
                open: expand_tools,
            }];
        }
        ordered
            .into_iter()
            .map(|(name, stats)| {
                let display = self.display_tool_name(name);
                // 子代理那一行按「名字 · 烧了多少 · 跑了多久」写：量放在时间前面
                //（用户拍板），而且它每报一次就变，正好也是"还活着"的指示。
                let tokens = self.subagent_tokens_label(name);
                let mut line = match (tokens.as_deref(), stats.elapsed()) {
                    (Some(tokens), Some(elapsed)) => {
                        format!("{display} · {tokens} · {}", format_seconds(elapsed))
                    }
                    (Some(tokens), None) => format!("{display} · {tokens}"),
                    (None, Some(elapsed)) => format!("{display} · {}", format_seconds(elapsed)),
                    (None, None) => display,
                };
                // 窥视：命令文本 / 检索词。这一行是这个工具在时间线上唯一
                // 露出来的信息，不给窥视就只剩一个名字。
                //
                // 子代理特殊：名字里已经带着描述了（`开发中·审查 xxx`），再把
                // 描述当窥视就是同一句话说两遍。它该露的是**此刻在干什么**，
                // 一直刷新——这也是"它还活着"的唯一指示。想看全的点开进面板。
                let subagent = crate::render::is_subagent_tool(name);
                let peek = if subagent {
                    self.subagent_peek(name)
                } else {
                    stats
                        .peek
                        .as_deref()
                        .or(stats.subject.as_deref())
                        .map(str::to_string)
                };
                if let Some(peek) = peek {
                    line.push_str(PEEK_SEP);
                    line.push_str(&peek);
                }
                // 整行裁到屏宽——**必须裁**。
                //
                // 窥视是子代理内层的思考末尾，长度不受这儿控制；裁之前它能把
                // 这一行顶出屏幕，于是缓冲把它折成两行，块的起止就跨了行：
                // 点上去命中不到，整行变成死的（用户实测：开始刷新思考窥视
                // 之后就没法交互了）。`step_line` 一直是裁的，这条路漏了。
                let line = crate::render::clip_to_display_width(&line, step_width());
                // logo 留在自己那一列，转轮另落在左边距上。
                let line = format!("{} {line}", tool_glyph(name));
                // 子代理优先进面板；面板还没登记出来就先退回它自己那一块，
                // 别让这一行变成点不开的死行。
                let own = self.live_tool_blocks.get(name).copied();
                let target = if subagent {
                    self.subagent_overlay_id(name).or(own)
                } else {
                    own
                };
                // 跑着的时候底下也是**命令本身**,和跑完落下来的那几行同一份,
                // 于是前后不跳版。输出点开才看;设 0 就一行都不露。
                let tail = if crate::render::is_command_tool(name) {
                    let rows = self.command_display_lines;
                    self.command_display
                        .as_ref()
                        .map(|display| display.command_rows(detail_width(), rows, false))
                        .unwrap_or_default()
                } else {
                    Vec::new()
                };
                // 「展开工具内容」开着时，跑着的这一步也该是展开的——点开看到的
                // 是它此刻的完整命令与输出（`live_tool_lines` 每帧重灌）。
                LiveRow {
                    line,
                    target,
                    tail,
                    open: expand_tools,
                }
            })
            .collect()
    }

    /// 「思考中」抬头底下那扇窗：正文折好行，取末尾 `thinking_scroll_lines` 行。
    /// 再多也不超过 live 区放得下的行数——静态面的 live 区高过一屏就擦不干净了。
    pub(crate) fn thought_window_rows(&self) -> Vec<String> {
        let keep = self
            .thinking_scroll_lines
            .min(live_thought_rows().saturating_sub(1));
        style_thought_rows(self.thought_rows_last(keep))
    }

    /// 正在想的抬头：`思考中 · N 词元 · Xs`（不带窥视）。
    pub(crate) fn thinking_head(&self) -> String {
        let elapsed = self
            .reasoning_started_at
            .map(|at| at.elapsed())
            .unwrap_or_default();
        if self.reasoning_tokens > 0 {
            format!(
                "{} · {} {} · {}",
                t("thinking", "思考中"),
                self.reasoning_tokens,
                t("tokens", "词元"),
                format_seconds(elapsed)
            )
        } else {
            format!("{} · {}", t("thinking", "思考中"), format_seconds(elapsed))
        }
    }

    /// 正在想的那一行：`思考中 · N 词元 · Xs   <窥视>`。
    ///
    /// 窥视取思考正文的**末尾**一段并压成一行——想到哪儿了比想过什么更有用，
    /// 而且它每帧都在变，正好当作「还活着」的指示。
    pub(crate) fn timeline_thinking_line(&self, peek_width: usize) -> String {
        let head = self.thinking_head();
        let peek = peek_tail(&self.reasoning_text, peek_width);
        if peek.is_empty() {
            head
        } else {
            format!("{head}{PEEK_SEP}{peek}")
        }
    }
}

/// `Worked for 12s · 3 tools · 2 thoughts · 1 err`。为零的项不写。
///
/// 措辞和 WebUI 的过程时间线一致（两端看到的是同一件事，不该换说法）。
pub fn summary_line(elapsed: Duration, counts: Counts) -> String {
    // 回放历史时**完全**没有计时（库里存的是做过什么，不是花了多久）。那种
    // 情况下报个 `Worked for 0.0s` 比不报还糟——它看着像"这一轮瞬间就完了"。
    //
    // 判据是「够不够一位小数」而不是「是不是零」：回放那条路上时间线还是会被
    // 现场掐一次表，量出来是几十微秒，比零大但照样打印成 `0.0s`。
    let mut parts = Vec::new();
    if elapsed >= Duration::from_millis(100) {
        parts.push(format!("Worked for {}", format_seconds(elapsed)));
    }
    if counts.tools > 0 {
        parts.push(format!(
            "{} {}",
            counts.tools,
            if counts.tools == 1 { "tool" } else { "tools" }
        ));
    }
    if counts.thoughts > 0 {
        parts.push(format!(
            "{} {}",
            counts.thoughts,
            if counts.thoughts == 1 {
                "thought"
            } else {
                "thoughts"
            }
        ));
    }
    if counts.errors > 0 {
        parts.push(format!("{} err", counts.errors));
    }
    if parts.is_empty() {
        return t("done", "已完成").to_string();
    }
    parts.join(" · ")
}

/// 取文本末尾能放进 `width` 的一段，压成单行（连续空白并成一个空格，两头的不要）。
///
/// 从末尾往回走，够一行宽就停。原来先把**整段**按空白拆开重拼一遍再取末尾——思考
/// 几万词元时，转轮每一拍都在重拼整段（用户 09-24：思考越长 TUI 越卡）。
pub fn peek_tail(text: &str, width: usize) -> String {
    use unicode_width::UnicodeWidthChar;
    if width == 0 {
        return String::new();
    }
    let mut taken: Vec<char> = Vec::new();
    let mut used = 0usize;
    let mut truncated = false;
    // 刚跨过一段空白：下一个字之前要补回那一个空格（只补在两个字之间）。
    let mut gap = false;
    for ch in text.chars().rev() {
        if ch.is_whitespace() {
            gap = !taken.is_empty();
            continue;
        }
        if gap {
            if used + 1 > width {
                truncated = true;
                break;
            }
            used += 1;
            taken.push(' ');
            gap = false;
        }
        let w = ch.width().unwrap_or(0);
        if used + w > width {
            truncated = true;
            break;
        }
        used += w;
        taken.push(ch);
    }
    taken.reverse();
    let peek: String = taken.into_iter().collect();
    if truncated {
        format!("…{peek}")
    } else {
        peek
    }
}
