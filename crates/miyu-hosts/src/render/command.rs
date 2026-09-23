//! 命令执行的实时显示与输出预览。
//!
//! 命令可能输出几万行，终端里只该看到头尾。`CommandOutputTail` 是个环形缓冲，
//! 中间部分直接丢掉——保留全部再截断意味着一个 `yes` 就能把内存吃光。
//!
//! `sanitize_terminal_text` 是安全边界而非美化：命令输出里的 ANSI 转义序列如果
//! 原样打出去，可以移动光标、改标题、甚至清屏。这里只放行已知安全的一小撮。
//!
//! `decode_utf8_prefix` 处理的是流式解码——一个字符可能被切在两次读取之间。

use crate::render::*;

// 终端文本原语(转义识别/显示宽度/命令流解码)已下沉到 `miyu_base::terminal::text`:
// 工具层也要用它们,而工具层不许引用渲染层。老路径 `crate::render::…` 靠这条
// 再导出保持不变(09-16)。
pub use miyu_base::terminal::{
    clip_to_display_width, sanitize_terminal_char, sanitize_terminal_text, CommandLogLine,
    CommandStreamState, TerminalControlState,
};

use miyu_base::terminal::display_width_skipping_escapes;

#[derive(Clone, serde::Serialize)]
pub(crate) struct CommandOutputPreviewLine {
    pub(crate) stream: &'static str,
    pub(crate) text: String,
}

#[derive(Clone, serde::Serialize)]
pub(crate) struct CommandOutputPreview {
    pub(crate) lines: Vec<CommandOutputPreviewLine>,
    pub(crate) omitted: bool,
}

pub(crate) struct CommandOutputTail {
    pub(crate) max_output_rows: usize,
    pub(crate) stdout: CommandStreamState,
    pub(crate) stderr: CommandStreamState,
    pub(crate) completed: VecDeque<CommandLogLine>,
    pub(crate) omitted_lines: bool,
    pub(crate) sequence: u64,
}

impl CommandOutputTail {
    pub fn new(max_output_rows: usize) -> Self {
        Self {
            max_output_rows,
            stdout: CommandStreamState::default(),
            stderr: CommandStreamState::default(),
            completed: VecDeque::new(),
            omitted_lines: false,
            sequence: 0,
        }
    }

    pub fn push(&mut self, stream: CommandOutputStream, chunk: &[u8]) {
        self.sequence = self.sequence.wrapping_add(1);
        let completed = match stream {
            CommandOutputStream::Stdout => self.stdout.push(chunk, self.sequence),
            CommandOutputStream::Stderr => self.stderr.push(chunk, self.sequence),
        };
        self.completed.extend(completed.into_iter().map(|mut line| {
            line.stream = stream;
            line
        }));
        let keep = self.max_output_rows.saturating_mul(4).max(100);
        while self.completed.len() > keep {
            self.completed.pop_front();
            self.omitted_lines = true;
        }
    }

    pub(crate) fn finalize(&mut self) {
        self.stdout.finalize_pending(self.sequence);
        self.stderr.finalize_pending(self.sequence);
    }

    pub(crate) fn preview(&self) -> CommandOutputPreview {
        if self.max_output_rows == 0 {
            return CommandOutputPreview {
                lines: Vec::new(),
                omitted: false,
            };
        }
        let logical = self.logical_lines();
        let omitted = self.omitted_lines || logical.len() > self.max_output_rows;
        let start = logical.len().saturating_sub(self.max_output_rows);
        let lines = logical[start..]
            .iter()
            .map(|line| CommandOutputPreviewLine {
                stream: match line.stream {
                    CommandOutputStream::Stdout => "stdout",
                    CommandOutputStream::Stderr => "stderr",
                },
                text: line.text.clone(),
            })
            .collect();
        CommandOutputPreview { lines, omitted }
    }

    pub(crate) fn logical_lines(&self) -> Vec<CommandLogLine> {
        let mut logical = self.completed.iter().cloned().collect::<Vec<_>>();
        let mut pending = [
            (CommandOutputStream::Stdout, &self.stdout),
            (CommandOutputStream::Stderr, &self.stderr),
        ];
        pending.sort_by_key(|(_, state)| state.last_update);
        for (stream, state) in pending {
            if !state.current.is_empty() {
                logical.push(CommandLogLine {
                    stream,
                    text: state.current.clone(),
                    sequence: state.current_sequence.unwrap_or(state.last_update),
                });
            }
        }
        logical.sort_by_key(|line| line.sequence);
        logical
    }
}

pub struct CommandLiveDisplay {
    pub(crate) command: String,
    pub(crate) status: CommandStatus,
    pub(crate) max_output_rows: usize,
    pub(crate) show_output: bool,
    pub(crate) show_full_command: bool,
    pub(crate) output: CommandOutputTail,
    pub(crate) frame: usize,
    pub(crate) rendered_line_widths: Vec<usize>,
}

impl CommandLiveDisplay {
    pub fn new(
        arguments: &str,
        max_output_rows: usize,
        show_output: bool,
        show_full_command: bool,
    ) -> Self {
        Self {
            command: command_from_arguments(arguments),
            status: CommandStatus::Running,
            max_output_rows,
            show_output,
            show_full_command,
            output: CommandOutputTail::new(max_output_rows),
            frame: 0,
            rendered_line_widths: Vec::new(),
        }
    }

    pub fn set_result(&mut self, ok: bool) {
        self.status = if ok {
            CommandStatus::Ok
        } else {
            CommandStatus::Error
        };
    }

    pub fn push(&mut self, stream: CommandOutputStream, chunk: &[u8]) {
        self.output.push(stream, chunk);
    }

    pub fn tick(&mut self, writer: &mut impl Write) -> Result<()> {
        self.redraw(writer, true)?;
        self.frame = self.frame.wrapping_add(1);
        Ok(())
    }

    pub(crate) fn redraw(&mut self, writer: &mut impl Write, spinning: bool) -> Result<()> {
        let width = command_terminal_width();
        let lines = self.rendered_lines(width, spinning);
        self.clear(writer)?;
        for (index, line) in lines.iter().enumerate() {
            execute!(writer, MoveToColumn(0), Clear(ClearType::CurrentLine))?;
            write!(writer, "{line}")?;
            if index + 1 < lines.len() {
                writeln!(writer)?;
            }
        }
        writer.flush()?;
        self.rendered_line_widths = lines.iter().map(|line| command_ansi_width(line)).collect();
        Ok(())
    }

    pub fn commit(&mut self, writer: &mut impl Write, include_output: bool) -> Result<()> {
        self.output.finalize();
        let show_output = self.show_output;
        self.show_output = include_output && show_output;
        let expanded = self.expandable_lines();
        crate::render::blocks::write_expandable(writer, expanded, |writer| {
            self.redraw(writer, false).map_err(std::io::Error::other)
        })?;
        self.show_output = show_output;
        if !self.rendered_line_widths.is_empty() {
            write_command_block_gap(writer, false)?;
            writer.flush()?;
            self.rendered_line_widths.clear();
        }
        Ok(())
    }

    pub fn write_static(&mut self, writer: &mut impl Write, include_output: bool) -> Result<()> {
        self.output.finalize();
        let show_output = self.show_output;
        self.show_output = include_output && show_output;
        let lines = self.rendered_lines(command_terminal_width(), false);
        let expanded = self.expandable_lines();
        self.show_output = show_output;
        crate::render::blocks::write_expandable(writer, expanded, |writer| {
            for line in lines {
                writeln!(writer, "{line}")?;
            }
            Ok(())
        })?;
        write_command_block_gap(writer, true)?;
        writer.flush()?;
        Ok(())
    }

    pub(crate) fn clear(&mut self, writer: &mut impl Write) -> Result<()> {
        if self.rendered_line_widths.is_empty() {
            return Ok(());
        }
        let rendered_rows =
            rendered_physical_rows(&self.rendered_line_widths, command_terminal_width());
        if rendered_rows > 1 {
            execute!(writer, MoveUp(rendered_rows - 1))?;
        }
        for index in 0..rendered_rows {
            execute!(writer, MoveToColumn(0), Clear(ClearType::CurrentLine))?;
            if index + 1 < rendered_rows {
                writeln!(writer)?;
            }
        }
        if rendered_rows > 1 {
            execute!(writer, MoveUp(rendered_rows - 1))?;
        }
        execute!(writer, MoveToColumn(0))?;
        writer.flush()?;
        self.rendered_line_widths.clear();
        Ok(())
    }

    /// 时间线里这一步点开看到的内容：完整命令 + 完整输出。
    ///
    /// 和 `expanded_lines` 的区别只在「总是给」——时间线那一行只有一句窥视，
    /// 不点开就什么都看不到，所以不能因为「折叠版已经说全了」而返回空。
    /// 时间线里这一步点开看到的内容：完整命令 + 完整输出。
    ///
    /// **不带 `↳` `│` 那套装饰**：那是 inline 下用来在一片正文里圈出"这是命令块"
    /// 的，全屏里这一段本来就缩进在一步之下、上下各有空行，再画一遍前缀只会
    /// 让每一行的起始列对不齐。命令和输出之间空一行就够分。
    pub(crate) fn timeline_detail(&mut self, width: usize) -> Vec<String> {
        self.output.finalize();
        self.live_detail(width)
    }

    /// 还在跑的时候点开看到的内容：和 [`Self::timeline_detail`] 一个形状，
    /// 只是不结算尾巴上那半行——命令还没完，最后一行可能正写到一半。
    ///
    /// 每一帧都会重算一遍（`refresh_live_block`），于是展开着的那一块跟着输出
    /// 一起长。原来这一块只有一句窥视，命令跑到一半点开什么都看不到
    ///（用户实测：命令展开后没有流式输出，展开内容居然是窥视行）。
    pub(crate) fn live_detail(&self, width: usize) -> Vec<String> {
        let mut lines = Vec::new();
        // 展开之后的命令按语法着色（用户 09-19：「展开后给一个语法高亮」）。
        //
        // 09-17 定过「命令那几行跟着抬头一起暗，输出才是主角」——那条**只留给
        // 抬头底下那份窥视**了。点开来看的时候，长命令（AI 自己写的几十行脚本）
        // 本身就是要读的东西，着色帮的正是这个忙。
        //
        // 先折行、再逐段着色：反过来的话折行会把转义序列算进宽度
        //（`render_code_block` 一直是这个顺序）。逐行词法着色本来就不跨行，
        // 折一次不会更糟。
        // **只对多行命令着色**。单行的 `ls -la`、`tail -f log` 逐词上色等于整行
        // 变亮，没有任何信息增益——而「命令别比输出亮」正是 09-17 那条裁定的
        // 来意。值得看清楚的是 AI 自己写的那种几十行脚本，它们必然是多行的。
        let highlight = self.command.lines().count() > 1;
        let languages = crate::render::command_line_languages(&self.command);
        for (index, line) in self.command.lines().enumerate() {
            let language = if highlight {
                languages.get(index).copied().unwrap_or("bash")
            } else {
                ""
            };
            lines.extend(wrap_plain_text(line, width).into_iter().map(|piece| {
                if language.is_empty() {
                    // 认不出语言（heredoc 里装的是数据）：维持原来的暗色。
                    format!("\x1b[2m{piece}\x1b[0m")
                } else {
                    crate::render::highlight_code_line(language, &piece)
                }
            }));
        }
        if self.show_output {
            let logical = self.output.logical_lines();
            if !logical.is_empty() {
                lines.push(String::new());
            }
            // 跑砸了整段红（省略标记也红）：报错信息得一眼认得出是报错
            //（用户实测：真 TUI 里报错展开不是红的）。
            let failed = matches!(self.status, CommandStatus::Error);
            if self.output.omitted_lines {
                let style = if failed { "\x1b[31m" } else { "\x1b[2m" };
                lines.push(format!(
                    "{style}⋮ {}\x1b[0m",
                    t("earlier output omitted", "已省略较早输出")
                ));
            }
            if failed {
                lines.extend(output_rows_styled(&logical, width, |_| "\x1b[31m"));
            } else {
                lines.extend(output_rows(&logical, width));
            }
        }
        lines
    }

    /// 抬头底下留着的那几行：**命令本身**，最多 `max_rows` 行，装不下时在
    /// 底部补一行 `⋮ 已省略`。
    ///
    /// 09-17 之前这里放的是命令输出的尾巴。用户裁定反过来：跑了什么命令是要紧
    /// 的，输出不是——输出退到点开里（`timeline_detail` 本来就给「完整命令 +
    /// 完整输出」）。命令要留**头**不留尾，所以省略标记在底部，和输出那边（留尾
    /// 不留头、标记在顶部）正相反。
    ///
    pub(crate) fn command_rows(&self, width: usize, max_rows: usize, failed: bool) -> Vec<String> {
        if max_rows == 0 {
            return Vec::new();
        }
        let mut rows = Vec::new();
        for line in self.command.lines() {
            rows.extend(wrap_plain_text(line, width));
        }
        // 露在抬头底下的这几行是**附注**，跟着抬头一起暗——抬头是暗色而它是正常
        // 文字色的话，附注比把手还显眼（用户 09-17：「tag 行是暗色，而命令预览是
        // 正常文字颜色，这不合理」）。点开看到的那份仍是正常色：那儿它才是内容。
        // 跑砸了整段红：和输出那边同一个规矩，一眼认得出是报错。
        let style = if failed { "\x1b[31m" } else { "\x1b[2m" };
        let reset = "\x1b[0m";
        let omitted = rows.len() > max_rows;
        let keep = if omitted {
            max_rows.saturating_sub(1)
        } else {
            max_rows
        };
        rows.truncate(keep);
        let mut lines: Vec<String> = rows
            .into_iter()
            .map(|line| format!("{style}{line}{reset}"))
            .collect();
        if omitted {
            let style = if failed { "\x1b[31m" } else { "\x1b[2m" };
            lines.push(format!("{style}⋮ {}\x1b[0m", t("omitted", "已省略")));
        }
        lines
    }

    /// 值得展开才给出展开内容：折叠版已经把话说全了就返回空，
    /// `write_expandable` 会当作「这块没什么可点的」。
    fn expandable_lines(&self) -> Vec<String> {
        if !crate::render::blocks::enabled() {
            return Vec::new();
        }
        let width = command_terminal_width();
        let expanded = self.expanded_lines(width);
        if expanded.len() <= self.rendered_lines(width, false).len() {
            return Vec::new();
        }
        expanded
    }

    pub(crate) fn rendered_lines(&self, width: usize, spinning: bool) -> Vec<String> {
        let usable = width.saturating_sub(1).max(5);
        let body_width = usable.saturating_sub(4).max(1);
        let command_lines = render_command_preview(
            &self.command,
            usable,
            self.show_full_command,
            spinning,
            self.frame,
        );
        let mut output = Vec::with_capacity(command_lines.len() + self.max_output_rows + 1);
        output.push(command_heading_line(self.status));
        output.extend(command_lines);
        if self.show_output && self.max_output_rows > 0 {
            output.extend(self.rendered_log_lines(body_width));
        }
        output
    }

    pub(crate) fn rendered_log_lines(&self, body_width: usize) -> Vec<String> {
        self.log_lines_capped(body_width, self.max_output_rows)
    }

    /// 展开后的完整块：命令不省略中间、输出不砍前面。点开看到的就是这份。
    ///
    /// 「完整」以渲染器还留着的为准——`CommandOutputTail` 本身只保 `max_output_rows`
    /// 的四倍，更早的行在收集时就丢了，这里变不出来。
    pub(crate) fn expanded_lines(&self, width: usize) -> Vec<String> {
        let usable = width.saturating_sub(1).max(5);
        let body_width = usable.saturating_sub(4).max(1);
        let mut output = vec![command_heading_line(self.status)];
        output.extend(render_command_preview(
            &self.command,
            usable,
            true,
            false,
            self.frame,
        ));
        if self.show_output {
            output.extend(self.log_lines_capped(body_width, usize::MAX));
        }
        output
    }

    fn log_lines_capped(&self, body_width: usize, max_output_rows: usize) -> Vec<String> {
        if max_output_rows == 0 {
            return Vec::new();
        }
        let logical = self.output.logical_lines();
        let mut rows = Vec::new();
        for line in logical {
            for text in wrap_plain_text(&line.text, body_width) {
                rows.push(CommandLogLine {
                    stream: line.stream,
                    text,
                    sequence: line.sequence,
                });
            }
        }
        let omitted = self.output.omitted_lines || rows.len() > max_output_rows;
        let keep = if omitted && max_output_rows > 1 {
            max_output_rows - 1
        } else {
            max_output_rows
        };
        let start = rows.len().saturating_sub(keep);
        let mut output = Vec::with_capacity(rows.len().min(max_output_rows));
        if omitted && max_output_rows > 1 {
            output.push(format!(
                "\x1b[2m  ⋮ {}\x1b[0m",
                t("earlier output omitted", "已省略较早输出")
            ));
        }
        output.extend(rows[start..].iter().map(|line| {
            let style = match line.stream {
                CommandOutputStream::Stdout => "\x1b[2m",
                CommandOutputStream::Stderr => "\x1b[2m\x1b[31m",
            };
            format!("\x1b[2m  │\x1b[0m {style}{}\x1b[0m", line.text)
        }));
        output
    }
}

/// 输出行折到 `width`、按流上色（stderr 红）。**不带 `│` 装饰**——时间线里这一段
/// 本来就缩进在一步之下。
fn output_rows(logical: &[CommandLogLine], width: usize) -> Vec<String> {
    output_rows_styled(logical, width, |stream| match stream {
        CommandOutputStream::Stdout => "\x1b[2m",
        CommandOutputStream::Stderr => "\x1b[2m\x1b[31m",
    })
}

fn output_rows_styled(
    logical: &[CommandLogLine],
    width: usize,
    style: impl Fn(CommandOutputStream) -> &'static str,
) -> Vec<String> {
    let mut rows = Vec::new();
    for line in logical {
        let style = style(line.stream);
        for row in wrap_plain_text(&line.text, width) {
            rows.push(format!("{style}{row}\x1b[0m"));
        }
    }
    rows
}

pub(crate) fn write_command_block_gap(
    writer: &mut impl Write,
    line_terminated: bool,
) -> Result<()> {
    if !line_terminated {
        writeln!(writer)?;
    }
    writeln!(writer)?;
    Ok(())
}

#[derive(Clone, Copy)]
pub(crate) enum CommandStatus {
    Running,
    Ok,
    Error,
}

pub(crate) fn command_heading_line(status: CommandStatus) -> String {
    let status = match status {
        CommandStatus::Running => t("running", "运行中"),
        CommandStatus::Ok => "ok",
        CommandStatus::Error => "err",
    };
    format!(
        "\x1b[2m$ {}×1 {status}\x1b[0m",
        t("run command", "运行命令")
    )
}

/// 正文能用多宽。
///
/// 全屏下要减掉页边距：按整屏宽排出来的表格、代码块会比可视区宽两列，落到
/// 缓冲里被硬折一次，续行从第 0 列开始——屏幕左边就冒出半截边框。
pub(crate) fn command_terminal_width() -> usize {
    crate::render::content_cols(120)
}

/// 命令家族:走 `CommandLiveDisplay`(带色命令行+输出尾巴)的工具。
/// claude-code 原生 Bash 的入参同样是 `command` 键,与 run_command 同构。
pub(crate) use miyu_engine::tools::is_command_tool;

pub fn command_from_arguments(arguments: &str) -> String {
    let parsed = serde_json::from_str::<Value>(arguments).ok();
    let command = parsed
        .as_ref()
        .and_then(|value| value.get("command"))
        .and_then(Value::as_str)
        .unwrap_or(arguments);
    sanitize_terminal_text(command).trim().to_string()
}

pub(crate) const COMMAND_PREVIEW_HEAD_LINES: usize = 2;

pub(crate) const COMMAND_PREVIEW_TAIL_LINES: usize = 4;

#[derive(Clone, Copy)]
pub(crate) enum CommandPreviewPrefix {
    First,
    Middle,
    Last,
    SoftWrap,
    LastSoftWrap,
}

pub(crate) fn render_command_preview(
    command: &str,
    width: usize,
    full: bool,
    spinning: bool,
    frame: usize,
) -> Vec<String> {
    let total_lines = command.split('\n').count();
    let compact_lines = COMMAND_PREVIEW_HEAD_LINES + COMMAND_PREVIEW_TAIL_LINES;
    let omitted_lines = if !full && total_lines > compact_lines {
        Some(total_lines - compact_lines)
    } else {
        None
    };
    let logical_lines = if omitted_lines.is_some() {
        command
            .split('\n')
            .take(COMMAND_PREVIEW_HEAD_LINES)
            .chain(
                command
                    .split('\n')
                    .skip(total_lines - COMMAND_PREVIEW_TAIL_LINES),
            )
            .collect::<Vec<_>>()
    } else {
        command.split('\n').collect::<Vec<_>>()
    };
    // Soft-wrap rows have two extra indentation columns after the tree marker.
    let content_width = width.saturating_sub(6).max(1);
    let mut rows = Vec::new();
    for (index, logical_line) in logical_lines.iter().enumerate() {
        if index == COMMAND_PREVIEW_HEAD_LINES {
            if let Some(omitted) = omitted_lines {
                let message = format!(
                    "{} {omitted} {}",
                    t("omitted", "已省略中间"),
                    t("middle lines", "行")
                );
                rows.extend(
                    wrap_plain_text(&message, content_width)
                        .into_iter()
                        .enumerate()
                        .map(|(wrapped_index, text)| {
                            let prefix = if wrapped_index == 0 {
                                "  ⋮ "
                            } else {
                                "  │   "
                            };
                            format!("\x1b[2m{prefix}{text}\x1b[0m")
                        }),
                );
            }
        }
        let wrapped = wrap_plain_text(logical_line, content_width);
        for (wrapped_index, text) in wrapped.iter().enumerate() {
            let first_logical_line = index == 0;
            let last_logical_line = index + 1 == logical_lines.len();
            let last_wrapped_row = wrapped_index + 1 == wrapped.len();
            let prefix = if first_logical_line && wrapped_index == 0 {
                CommandPreviewPrefix::First
            } else if last_logical_line && last_wrapped_row {
                if wrapped_index == 0 {
                    CommandPreviewPrefix::Last
                } else {
                    CommandPreviewPrefix::LastSoftWrap
                }
            } else if wrapped_index > 0 {
                CommandPreviewPrefix::SoftWrap
            } else {
                CommandPreviewPrefix::Middle
            };
            rows.push(format_command_preview_line(prefix, text, spinning, frame));
        }
    }
    rows
}

pub(crate) fn format_command_preview_line(
    prefix: CommandPreviewPrefix,
    text: &str,
    spinning: bool,
    frame: usize,
) -> String {
    let prefix = match prefix {
        CommandPreviewPrefix::First if spinning => format!(
            "\x1b[2m\x1b[36m{}\x1b[0m \x1b[2m↳\x1b[0m ",
            braille_frame(frame)
        ),
        CommandPreviewPrefix::First => "  \x1b[2m↳\x1b[0m ".to_string(),
        CommandPreviewPrefix::Middle => "  \x1b[2m│\x1b[0m ".to_string(),
        CommandPreviewPrefix::Last => "  \x1b[2m└\x1b[0m ".to_string(),
        CommandPreviewPrefix::SoftWrap => "  \x1b[2m│\x1b[0m   ".to_string(),
        CommandPreviewPrefix::LastSoftWrap => "  \x1b[2m└\x1b[0m   ".to_string(),
    };
    format!("{prefix}\x1b[33m{text}\x1b[0m")
}

pub(crate) fn wrap_plain_text(text: &str, width: usize) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut current_width = 0usize;
    for grapheme in text.graphemes(true) {
        let grapheme_width = UnicodeWidthStr::width(grapheme);
        if current_width > 0 && current_width + grapheme_width > width {
            lines.push(std::mem::take(&mut current));
            current_width = 0;
        }
        current.push_str(grapheme);
        current_width += grapheme_width;
    }
    lines.push(current);
    lines
}

/// 按**显示宽度**折一行，转义序列不算宽度、也不会被从中间切断。
///
/// 和 `wrap_plain_text` 的区别是这个认转义序列——正文是带颜色的，拿整串去量宽度
/// 会把一行当成比它实际宽得多，于是提前断行；切点还可能落在转义序列中间。
///
/// 能在空格处断就在空格处断。硬断在词中间是终端的默认行为，但既然折行这件事
/// 已经自己接管了，顺手做对。
pub fn wrap_display_text(text: &str, width: usize) -> Vec<String> {
    wrap_display_rows(text, width)
        .into_iter()
        .map(|(row, _)| row)
        .collect()
}

/// 同 [`wrap_display_text`]，每一行再带上它在原文里的起点（字节）。
///
/// 从某一行的起点接着往后折，折出来的和整段一起折一模一样（断点只看这一行里的
/// 东西）。正在想的正文只往后长，靠它只重折末尾那两行，前面的记住
///（`timeline::ThoughtRows`，用户 09-24：一大段不换行的思考越长越卡）。
pub fn wrap_display_rows(text: &str, width: usize) -> Vec<(String, usize)> {
    let width = width.max(1);
    if display_width_skipping_escapes(text) <= width {
        return vec![(text.to_string(), 0)];
    }
    let mut lines = Vec::new();
    let mut current = String::new();
    // `current` 在原文里从哪儿起：它永远是原文里连续的一段。
    let mut current_start = 0usize;
    let mut current_width = 0usize;
    // 最近一个可断处：`current` 里的字节位置 + 那时的显示宽度。
    let mut break_at: Option<(usize, usize)> = None;
    let mut rest = text;
    while !rest.is_empty() {
        if let Some(len) = escape_len(rest) {
            current.push_str(&rest[..len]);
            rest = &rest[len..];
            continue;
        }
        let Some(grapheme) = rest.graphemes(true).next() else {
            break;
        };
        let grapheme_len = grapheme.len();
        let grapheme_width = UnicodeWidthStr::width(grapheme);
        if current_width > 0 && current_width + grapheme_width > width {
            match break_at.take() {
                Some((position, width_before)) if width_before > 0 => {
                    let tail = current.split_off(position);
                    let head = current.trim_end().to_string();
                    lines.push((head, current_start));
                    current = tail;
                    current_start += position;
                    current_width = current_width.saturating_sub(width_before);
                }
                _ => {
                    lines.push((std::mem::take(&mut current), current_start));
                    current_start = text.len() - rest.len();
                    current_width = 0;
                }
            }
        }
        current.push_str(grapheme);
        current_width += grapheme_width;
        if grapheme == " " {
            break_at = Some((current.len(), current_width));
        }
        rest = &rest[grapheme_len..];
    }
    if !current.is_empty() {
        lines.push((current, current_start));
    }
    if lines.is_empty() {
        lines.push((String::new(), 0));
    }
    lines
}

pub(crate) fn transient_summary_lines(text: &str, terminal_width: usize) -> Vec<String> {
    let max_width = terminal_width.saturating_sub(1).max(1);
    let mut lines = text
        .lines()
        .map(|line| clip_to_display_width(line, max_width))
        .collect::<Vec<_>>();
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

pub(crate) fn command_ansi_width(text: &str) -> usize {
    let mut plain = String::new();
    let mut state = TerminalControlState::Text;
    for ch in text.chars() {
        if let Some(ch) = sanitize_terminal_char(&mut state, ch) {
            plain.push(ch);
        }
    }
    UnicodeWidthStr::width(plain.as_str())
}

pub(crate) fn write_command_block_with_status(
    stdout: &mut impl Write,
    arguments: &str,
    status: CommandStatus,
) -> Result<()> {
    let command = command_from_arguments(arguments);
    writeln!(stdout, "{}", command_heading_line(status))?;
    let terminal_width = crate::render::content_cols(120);
    let usable = terminal_width.saturating_sub(1).max(5);
    for line in render_command_preview(&command, usable, true, false, 0) {
        writeln!(stdout, "{line}")?;
    }
    Ok(())
}

pub(crate) fn write_command_result_blocks(stdout: &mut impl Write, output: &str) -> Result<()> {
    let Some(result) = parse_command_result(output) else {
        return write_tool_payload(stdout, t("output", "输出"), &sanitize_terminal_text(output));
    };
    if !result.stdout.trim().is_empty() {
        write_fenced_block(stdout, t("output", "输出"), &result.stdout)?;
    }
    if !result.stderr.trim().is_empty() {
        let label = result
            .exit_code
            .map(|code| format!("err exit {code}"))
            .unwrap_or_else(|| "err".to_string());
        write_fenced_block(stdout, &label, &result.stderr)?;
    } else if !result.success {
        let label = result
            .exit_code
            .map(|code| format!("err exit {code}"))
            .unwrap_or_else(|| "err".to_string());
        write_fenced_block(
            stdout,
            &label,
            t(
                "command failed without stderr",
                "命令失败，但没有 stderr 输出",
            ),
        )?;
    }
    Ok(())
}

pub(crate) fn write_fenced_block(stdout: &mut impl Write, label: &str, text: &str) -> Result<()> {
    writeln!(stdout, "\x1b[2m,-- {label}\x1b[0m")?;
    let sanitized = sanitize_terminal_text(text);
    let style = if label.starts_with("err") {
        "\x1b[2m\x1b[31m"
    } else {
        "\x1b[2m"
    };
    for line in truncate_chars(sanitized.trim(), 2400).lines() {
        writeln!(stdout, "{style}{line}\x1b[0m")?;
    }
    writeln!(stdout, "\x1b[2m`--\x1b[0m")?;
    Ok(())
}

pub(crate) struct CommandResult {
    pub(crate) success: bool,
    pub(crate) exit_code: Option<i64>,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
}

/// 解析 dsh 式纯文本命令结果(08-17 起 run_command/grep/glob 的形态):
/// 正文是 stdout,可选 `[stderr]` 段,末尾可选 `[exit code: N]` /
/// `[killed by signal]` 标记。老的 JSON 形态仍然认——历史回合里还躺着
/// 一批,渲染层不能因为换了形态就把它们变成裸 JSON。
pub(crate) fn parse_command_result(output: &str) -> Option<CommandResult> {
    let text = output.trim();
    if let Ok(value) = serde_json::from_str::<Value>(text) {
        if let Some(success) = value.get("success").and_then(Value::as_bool) {
            return Some(CommandResult {
                success,
                exit_code: value.get("exit_code").and_then(Value::as_i64),
                stdout: value
                    .get("stdout")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                stderr: value
                    .get("stderr")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            });
        }
        return None;
    }

    let mut body = text;
    let mut exit_code = Some(0);
    let mut success = true;
    if let Some(rest) = body.strip_suffix("]") {
        if let Some((head, marker)) = rest.rsplit_once("\n[") {
            if let Some(code) = marker.strip_prefix("exit code: ") {
                if let Ok(code) = code.trim().parse::<i64>() {
                    body = head;
                    exit_code = Some(code);
                    success = code == 0;
                }
            } else if marker == "killed by signal" {
                body = head;
                exit_code = None;
                success = false;
            }
        }
    }
    let (stdout, stderr) = match body.split_once("[stderr]\n") {
        Some((out, err)) => (out.trim_end(), err),
        None => (body, ""),
    };
    let stdout = if stdout.trim() == "(no output)" {
        ""
    } else {
        stdout
    };
    Some(CommandResult {
        success,
        exit_code,
        stdout: stdout.to_string(),
        stderr: stderr.to_string(),
    })
}

#[cfg(test)]
mod test_support;
