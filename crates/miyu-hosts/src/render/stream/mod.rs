//! 回合输出的流式渲染器。
//!
//! `StreamRenderer` 是终端这一侧的总入口：模型的文本、推理、工具调用、命令输
//! 出全从它过一遍再落到屏幕上。
//!
//! `SentMemeStreamFilter` 处理的是「已经作为图片发出去的表情，不要在正文里再
//! 打一遍」——过滤要在流式条件下做，所以得记住最长的部分匹配前缀
//! （`longest_sent_meme_prefix_suffix`），不能等看完整段。

mod event_clock;
mod reasoning_phase;
mod reply_tail;
pub(crate) mod surface;
pub mod timeline;
mod tool_summary;

use crate::render::*;

pub(crate) fn rendered_physical_rows(widths: &[usize], terminal_width: usize) -> u16 {
    let columns = terminal_width.max(1);
    widths
        .iter()
        .map(|width| (*width).max(1).div_ceil(columns))
        .sum::<usize>()
        .min(u16::MAX as usize) as u16
}

pub(crate) enum RenderOutput {
    Terminal,
    Buffered(Vec<u8>),
}

impl Write for RenderOutput {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        match self {
            Self::Terminal => io::stdout().write(bytes),
            Self::Buffered(buffer) => buffer.write(bytes),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Terminal => io::stdout().flush(),
            Self::Buffered(_) => Ok(()),
        }
    }
}

pub struct StreamRenderer {
    /// 工具产出的、该落在**时间线收缩行之后**的东西（图片占位格、todo 表…）。
    ///
    /// 它们是"这一步干出来的结果"，不是过程。夹在时间线中间的话，这一步自己
    /// 反而排到了结果下面——屏幕上就成了「Worked for… ／ 图 ／ 1 tool」，因果
    /// 倒过来了（用户实测报的表情包、搜图、todo、提问全是这一个毛病）。
    /// `cut_timeline` 把这一段收成一行之后再把它们放出来。
    pub(crate) pending_after_timeline: Vec<String>,
    /// 这一批工具里有清单:收完这批就把这一段收成 `Worked for`,表跟在
    /// 它后面。由 `timeline_push_tools` 记下,批次边界消费(见
    /// `settle_tool_batch`)——不在推步骤的路上就地收段。
    pub(crate) timeline_ends_after_tools: bool,
    /// 正在跑的每个工具各自那一块（工具事件名 → 块 id）。见 `refresh_live_block`。
    pub(crate) live_tool_blocks: BTreeMap<String, u64>,
    /// 这一轮里每个子代理**至此**烧掉的词元（工具事件名 → 数）。
    ///
    /// 子代理的用量要等它跑完、审计会话落盘之后才进会话累计，而一个子代理能跑
    /// 好几分钟——那几分钟里 Σ 一动不动。跑着的时候先按这份实时加上去，回合收尾
    /// 时会话累计从库里重读，加数跟着清掉，不会算两遍。
    /// 记的是**各自的最新值**不是增量，所以并行几个也不会互相叠加出鬼数。
    pub(crate) subagent_tokens: BTreeMap<String, u64>,
    /// 前台子代理此刻的样子（`subagent.progress`，工具事件名 → 状态）：状态行那一行的
    /// 窥视、词元从这儿取（会话项目第 4 段之二）。
    pub(crate) subagent_status:
        BTreeMap<String, miyu_engine::tools::subagent::status::SubagentStatus>,
    pub(crate) reasoning_mode: ReasoningDisplayMode,
    pub(crate) tool_call_mode: ToolCallDisplayMode,
    pub(crate) plain: bool,
    pub(crate) mode: Option<ChatStreamKind>,
    pub(crate) cursor_hidden: bool,
    pub(crate) external_cursor_control: bool,
    pub(crate) output: RenderOutput,
    pub(crate) markdown: MarkdownStreamRenderer,
    pub(crate) reasoning_text: String,
    /// `reasoning_text` 折好的行，只往后补（见 `timeline::ThoughtRows`）。读它的
    /// 地方大多只拿 `&self`，所以装在 `RefCell` 里。
    pub(crate) thought_rows: std::cell::RefCell<timeline::ThoughtRows>,
    pub(crate) reasoning_tokens: usize,
    pub(crate) reasoning_title: Option<String>,
    pub(crate) reasoning_started_at: Option<std::time::Instant>,
    pub(crate) reasoning_elapsed: Option<std::time::Duration>,
    /// 最后一片思考（正文或标题）到的时刻。思考停了多久拿它量，见
    /// [`Self::settle_stalled_reasoning`]。
    pub(crate) reasoning_last_delta_at: Option<std::time::Instant>,
    /// 点不开的面 + 完整档：这一段思考的正文边想边往下流。见 [`timeline::ThoughtStream`]。
    /// `None` = 这一段没走这条路（全屏 / 摘要档 / 还没来第一条 delta）。
    pub(crate) thought_stream: Option<timeline::ThoughtStream>,
    /// 同步输出块的嵌套深度。DEC 2026 是个布尔不是栈：只有最外层那对才真的发
    /// 标记，里层（落地时顺手收转轮）什么都不发，免得里层的结束把外层提前结掉。
    pub(crate) sync_depth: usize,
    pub(crate) tool_stats: BTreeMap<String, ToolStats>,
    pub(crate) tool_seq: usize,
    pub(crate) readable_tool_names: bool,
    /// 抬头底下露几行**命令**(不是输出)。配置键仍叫 `command_output_lines`——
    /// 语义 09-17 从「输出行数」改成「命令行数」,但键名不动:改了的话用户
    /// 已经设过的值会掉回默认。
    pub(crate) command_display_lines: usize,
    /// 「思考中」抬头底下那扇窗露最近几行（「展开思考内容」关着时）。见
    /// `DisplayConfig::thinking_scroll_lines`。0 = 不开窗，抬头后面只跟一截窥视。
    pub thinking_scroll_lines: usize,
    /// 跨会话 AI 消息（发出去那一步）抬头底下露几行正文。见
    /// `DisplayConfig::cross_session_preview_lines`。
    pub cross_session_preview_lines: usize,
    /// 全屏里不把过程收成 `Worked for …`(用户 todolist:21)。只在能点开的面上
    /// 有意义:逐步落地的面本来就不收。
    /// 一段过程跑完收成一行 `Worked for …` 吗。见 `DisplayConfig::fold_timeline`。
    pub fold_timeline: bool,
    pub(crate) command_display: Option<CommandLiveDisplay>,
    /// 这一刻的收尾是不是「给外部输出让路」（表情包要往终端写图那种）。
    ///
    /// 真时：还没返回的工具**留在统计里**，不收进时间线——它还在跑，不是被
    /// 打断了。见 `prepare_for_external_output`。
    pub(crate) finalizing_for_external_output: bool,
    pub(crate) summary_line_active: bool,
    pub(crate) summary_lines_active: u16,
    pub(crate) last_tool_summary: String,
    /// 目的地是不是一个有活动区的终端。**只该由 `use_terminal_surface` /
    /// `use_piped_surface` 设**——选面要说出面的名字，别拿这个字段名冒充。
    /// （拆 crate 之后它对外仍是 `pub`：跨 crate 的夹具还在直接读写。）
    pub live_summary: bool,
    pub wait_spinner: Option<WaitSpinner>,
    pub(crate) last_tick: Option<std::time::Instant>,
    /// 提问面板开着：转轮不动（她在等人回答，没什么在跑）。面板改成活动区上的层之后回合
    /// 循环照转（会话项目第 3 段），不冻的话转轮还在面板上头动；把整块转轮收掉又会连
    /// 「已思考」那几行一起抹掉（09-20 修过一次的「面板一开，刚才的过程就没了」）。
    /// 见 `prepare_for_panel` / `resume_after_panel`。
    pub(crate) spinner_frozen: bool,
    /// 等待转轮上钉死的文案(压缩上下文那种不属于任何一步的等待):在,每帧都用它,
    /// 不让时间线的「正在思考/工具名」盖掉。
    pub(crate) custom_waiting_phase: Option<String>,
    /// 全屏：自动压缩时流进来的摘要先攒着，压完收成一块（`finish_compact`）。
    pub(crate) compact_text: String,
    /// 正在喂的这个事件是什么时候发生的（`event_clock.rs`）。
    pub(crate) event_clock: Option<std::time::Instant>,
    pub(crate) preparing_question_started_at: Option<std::time::Instant>,
    /// Phase text and start time for the "still receiving arguments" hint.
    /// Sticky like `preparing_question_started_at` and for the same reason:
    /// `tick_spinner` re-derives the phase from renderer state on every tick,
    /// so a phase merely pushed into the spinner is overwritten before it can
    /// be drawn.
    /// 正在流参数的那个工具：提示语、它自己的图标、从什么时候开始。图标跟工具走
    ///（准备编辑=铅笔、准备执行=`$`），不是一个通用齿轮（用户 09-14 要求）。
    pub(crate) tool_preparing: Option<(&'static str, &'static str, std::time::Instant)>,
    /// 整个准备窗口的起点，跨 write_tool_call 存活。
    ///
    /// `tool_preparing` 每次工具调用完成就被清掉，计时锚点跟着它走的话，
    /// 批量调用里第二个工具一到就归零——屏幕上的秒数来回横跳，反映不出
    /// 已经等了多久。窗口真正结束（新一轮思考／外部输出／工具跑完／回合
    /// 结束）才清这个。
    pub(crate) tool_preparing_since: Option<std::time::Instant>,
    pub(crate) subagent_mode: Option<ChatStreamKind>,
    pub(crate) sent_meme_filter: SentMemeStreamFilter,
    /// 模型正文/思维链的流式转义过滤状态:与命令输出同一套状态机,
    /// 拦截 `\x1b[2J`/OSC 等正文里的终端控制序列(清屏/藏光标/伪造 UI)。
    pub(crate) stream_control: TerminalControlState,
    /// 全屏下这一段连续过程的时间线。inline 模式全程为空。
    pub(crate) timeline: timeline::Timeline,
    /// 「正在进行」那一行的块 id。每帧重发标记但**id 不变**，否则每 tick
    /// 都会在登记处攒一个新块。想完/跑完就清掉。
    pub(crate) live_block: Option<u64>,
    /// 正文还没落下的那一截（见 `reply_tail`）。
    pub(crate) reply_tail: reply_tail::ReplyTail,
}

impl StreamRenderer {
    pub fn new(
        reasoning_mode: ReasoningDisplayMode,
        tool_call_mode: ToolCallDisplayMode,
        plain: bool,
        readable_tool_names: bool,
        command_display_lines: usize,
    ) -> Self {
        Self {
            reasoning_mode,
            tool_call_mode,
            plain,
            mode: None,
            cursor_hidden: false,
            external_cursor_control: false,
            output: RenderOutput::Terminal,
            markdown: MarkdownStreamRenderer::new(),
            reasoning_text: String::new(),
            thought_rows: Default::default(),
            reasoning_tokens: 0,
            reasoning_title: None,
            reasoning_started_at: None,
            thought_stream: None,
            sync_depth: 0,
            reasoning_elapsed: None,
            reasoning_last_delta_at: None,
            tool_stats: BTreeMap::new(),
            tool_seq: 0,
            readable_tool_names,
            command_display_lines,
            thinking_scroll_lines: 10,
            cross_session_preview_lines: 10,
            fold_timeline: true,
            pending_after_timeline: Vec::new(),
            timeline_ends_after_tools: false,
            live_tool_blocks: BTreeMap::new(),
            subagent_tokens: BTreeMap::new(),
            subagent_status: BTreeMap::new(),
            command_display: None,
            finalizing_for_external_output: false,
            summary_line_active: false,
            summary_lines_active: 0,
            last_tool_summary: String::new(),
            live_summary: io::stdout().is_terminal(),
            wait_spinner: None,
            last_tick: None,
            spinner_frozen: false,
            custom_waiting_phase: None,
            compact_text: String::new(),
            event_clock: None,
            preparing_question_started_at: None,
            tool_preparing: None,
            tool_preparing_since: None,
            subagent_mode: None,
            sent_meme_filter: SentMemeStreamFilter::default(),
            stream_control: TerminalControlState::default(),
            timeline: timeline::Timeline::default(),
            live_block: None,
            reply_tail: Default::default(),
        }
    }

    pub fn use_external_cursor_control(&mut self) {
        self.external_cursor_control = true;
    }

    pub fn use_buffered_output(&mut self) {
        self.output = RenderOutput::Buffered(Vec::new());
    }

    /// 字节最终落进一个**真终端**（有活动区、有转轮、能回翻），哪怕本进程的
    /// stdout 不是。
    ///
    /// `live_summary` 出厂取 `stdout().is_terminal()`——那问的是「我的 stdout 是
    /// 不是终端」。回写这条路上两者分家：daemon 的 stdout 是管道，目的地却是
    /// shellhook 那个 tty（`web/actor/job_wake.rs`）；测试夹具把帧收进缓冲，量的
    /// 却是「用户看到的那一条」。这些地方过去各自把字段掰成 `true`，等于拿一个
    /// 字段名冒充 surface 选择（`docs/plan/2026-09-17-render-unification.md`
    /// §5 第 2 条）。
    pub fn use_terminal_surface(&mut self) {
        self.live_summary = true;
    }

    /// 反过来：字节进管道，没有活动区（surface S1，老的一行摘要）。非终端环境
    /// 里出厂就是这个值，显式写出来是为了让用例说清自己在量哪一面。
    pub fn use_piped_surface(&mut self) {
        self.live_summary = false;
    }

    pub fn take_output_frame(&mut self) -> Vec<u8> {
        match &mut self.output {
            RenderOutput::Terminal => Vec::new(),
            RenderOutput::Buffered(buffer) => std::mem::take(buffer),
        }
    }

    pub fn write_chunk(&mut self, chunk: ChatStreamChunk) -> Result<()> {
        if chunk.kind == ChatStreamKind::ToolCall {
            if chunk.text == "ask_question" {
                self.start_preparing_question()?;
            }
            return Ok(());
        }
        if matches!(
            chunk.kind,
            ChatStreamKind::ReasoningPartStart
                | ChatStreamKind::ReasoningPartEnd
                | ChatStreamKind::ReasoningReset
        ) {
            return Ok(());
        }
        if !self.plain {
            self.hide_cursor()?;
        }
        let text = normalize_stream_text(&chunk.text);
        // 正文/思维链与命令输出同权:全部过转义状态机,模型输出里的
        // `\x1b[2J`、OSC 8 等控制序列不能直接打到用户终端上生效。
        // 状态跨 delta 持有,序列被 delta 切断也拦得住。
        let text = sanitize_stream_chunk(&mut self.stream_control, &text);
        let text = if chunk.kind == ChatStreamKind::Content {
            self.sent_meme_filter.push(&text)
        } else {
            text
        };
        if text.is_empty() {
            return Ok(());
        }
        if self.plain && chunk.kind == ChatStreamKind::Reasoning {
            return Ok(());
        }
        if self.reasoning_mode == ReasoningDisplayMode::Hidden
            && chunk.kind == ChatStreamKind::Reasoning
        {
            return Ok(());
        }
        if self.captures_reasoning() && chunk.kind == ChatStreamKind::Reasoning {
            // 真·交错思考:正文行还开着就先收行,转轮画在自己的行上,
            // 不然 MoveToColumn(0)+清行会抹掉半行正文。
            if self.mode == Some(ChatStreamKind::Content) {
                self.end_active_stream_line()?;
            }
            self.finalize_tools_summary()?;
            self.record_reasoning_text(&text);
            // 点不开的面 + 完整档：想完整行的正文当场落地（半行留在 live 区）。
            self.stream_thought_progress()?;
            self.mode = Some(ChatStreamKind::Reasoning);
            self.ensure_waiting_phase(self.reasoning_live_text(), self.wait_style())?;
            return Ok(());
        }
        // 只停转轮：正文的活尾巴留着，下面有整行落下时和正文同一帧交接。
        self.stop_spinner()?;
        if self.mode != Some(chunk.kind) {
            if chunk.kind == ChatStreamKind::Content {
                self.finalize_reasoning_summary()?;
                self.finalize_tools_summary()?;
                // 模型开始说正文了：这一段连续过程到此为止，收成一行。
                self.cut_timeline()?;
            } else if chunk.kind == ChatStreamKind::Reasoning {
                self.finalize_tools_summary()?;
            }
            self.switch_mode(chunk.kind)?;
        }
        if chunk.kind == ChatStreamKind::Reasoning {
            write_full_reasoning_chunk(&mut self.output, &text)?;
        } else if self.plain {
            write!(self.output, "{text}")?;
        } else {
            let rendered = self.markdown.push(&text);
            // 全屏：正文也缩进两格，和时间线、用户消息共用一条装订边。
            // `push` 只吐**整行**（半行留在它自己的缓冲里），所以这里逐行加
            // 前缀不会把一行切成两半。
            let rendered = if self.caps().expandable {
                timeline::indent_body(&rendered)
            } else {
                rendered
            };
            self.write_committed_body(&rendered)?;
        }
        self.output.flush()?;
        Ok(())
    }

    pub fn write_command_output(
        &mut self,
        name: &str,
        stream: CommandOutputStream,
        chunk: &[u8],
    ) -> Result<()> {
        if self.plain || !is_command_tool(name) {
            return Ok(());
        }
        if let Some(display) = &mut self.command_display {
            display.push(stream, chunk);
        }
        Ok(())
    }

    /// 面板要抢屏，但**先别切时间线**。
    ///
    /// 切了之后"询问用户"那一步就只能落进下一段——屏幕上成了
    /// 「Worked for… ／ 问答块 ／ 询问用户」，因果整个倒过来。正确顺序是：
    /// 面板退场、答案到手、把这一步补进当前这一段，再连着一起收。
    /// 给面板让屏（她反问的提问面板、`/models` `/session` 这些）。
    ///
    /// **要把时间线切进回放缓冲**，不能只把活动区撤下来：活动区那几行（「已思考
    /// · 334 词元 · 3.4s」「准备问题 · 644ms」）还没落盘，撤掉就等于从屏幕上
    /// 消失，面板收掉才重新冒出来——用户 09-20 截图：「面板开启时当前输出内容
    /// 会消失，面板关闭后又出现。我记得之前只是顶上去而已啊？」
    ///
    /// 和 `prepare_for_external_output`（给终端图片让路）同一套动作，连那面
    /// 「这次收尾只是让路、不是回合结束」的旗子一起用：不挡这一下的话，正在跑
    /// 的那一步会被记成红色的「已中断」，而真结果回来时又记一次（09-19 在
    /// shellhook 里实测过一次发图两行报错）。
    pub fn prepare_for_panel(&mut self) -> Result<()> {
        // 「准备问题 / 准备编辑」是转轮画的那一行：清掉状态、按新状态立刻重画一次（④，
        // 09-24 goal_question 走查），已经跑完的那几步（「已思考 · …」）原样留着。然后冻住
        // 转轮（`spinner_frozen`）：面板开着时她在等人回答，没什么在跑。
        //
        // 集成时先取过「整块转轮收掉」（`stop_waiting`）：转轮是不动了，可全屏下时间线
        // 那几行就画在转轮那块里，「已思考」跟着一起没了——正是 09-20 修过的「面板一开，
        // 刚才的过程就没了」（09-25 红绿账 panel_keeps_body 抓到，会话分支上就红）。
        let was_preparing = self.preparing_question_started_at.take().is_some()
            | self.tool_preparing.take().is_some();
        self.tool_preparing_since = None;
        self.clear_reply_tail()?;
        if was_preparing && self.wait_spinner.is_some() {
            self.set_waiting_phase(self.waiting_phase_text());
            self.last_tick = None;
            self.tick_spinner()?;
        }
        self.spinner_frozen = true;
        self.show_cursor()?;
        Ok(())
    }

    /// 提问面板收掉了（答了、关了、没法显示）：转轮接着动。
    pub fn resume_after_panel(&mut self) {
        self.spinner_frozen = false;
        self.last_tick = None;
    }

    pub fn prepare_for_external_output(&mut self) -> Result<()> {
        self.preparing_question_started_at = None;
        self.tool_preparing = None;
        self.tool_preparing_since = None;
        self.release_transient_output()?;
        // 这次收尾只是**给外部输出让路**,不是回合结束:正在跑的那个工具(往终端
        // 写图的表情包就是它自己)还没返回,不能当成「没跑完 = 已中断」收进时间
        // 线。不挡这一下的话它会被记成一步红色的「已中断」,而真结果回来时统计
        // 已经清空、`calls` 归零,`settled()` 仍是假——于是**又记一次**,一次发图
        // 两行报错(用户 09-19 在 shellhook 里实测)。
        self.finalizing_for_external_output = true;
        let finalized = self.finalize_tools_summary();
        self.finalizing_for_external_output = false;
        finalized?;
        self.cut_timeline()?;
        self.show_cursor()?;
        Ok(())
    }

    pub fn write_system_message(&mut self, message: &str) -> Result<()> {
        self.prepare_for_external_output()?;
        // 能力位要在借走 `self.output` 之前问:借用检查不让同时拿。
        let expandable = self.caps().expandable;
        let stdout = &mut self.output;
        // 全屏：系统提示和时间线里的通知一个样子——暗色、带图标、退两格，
        // 不是贴着第 0 列的一行灰字。
        if expandable {
            let line = timeline::indent_body(&format!(
                "\x1b[2m{} {message}\x1b[0m\n",
                timeline::glyph_notice()
            ));
            write!(stdout, "{line}")?;
            stdout.flush()?;
            return Ok(());
        }
        execute!(stdout, SetForegroundColor(Color::DarkGrey), MoveToColumn(0))?;
        writeln!(stdout, "{message}")?;
        execute!(stdout, ResetColor)?;
        stdout.flush()?;
        Ok(())
    }

    pub fn write_compact_chunk(&mut self, chunk: &ChatStreamChunk) -> Result<()> {
        if chunk.kind != ChatStreamKind::Content {
            return Ok(());
        }
        // 全屏：摘要先攒着，压完收成一块点开看。整段灰字流到正文里，几十行
        // 摘要把对话冲散了（用户：压缩上下文没有任何输出吗——inline 那套灰字在
        // 全屏下本来就该折起来）。
        if self.caps().expandable {
            self.compact_text.push_str(&chunk.text);
            return Ok(());
        }
        self.prepare_for_external_output()?;
        let stdout = &mut self.output;
        execute!(stdout, SetForegroundColor(Color::DarkGrey))?;
        write!(stdout, "{}", chunk.text)?;
        execute!(stdout, ResetColor)?;
        stdout.flush()?;
        Ok(())
    }

    pub fn finish_compact(&mut self) -> Result<()> {
        if self.caps().expandable {
            let summary = std::mem::take(&mut self.compact_text);
            timeline::write_compact_summary(
                &mut self.output,
                t("context compacted", "上下文已压缩"),
                &summary,
            )?;
            self.output.flush()?;
            return Ok(());
        }
        let stdout = &mut self.output;
        execute!(stdout, ResetColor)?;
        writeln!(stdout)?;
        stdout.flush()?;
        Ok(())
    }

    pub fn finish(&mut self) -> Result<()> {
        self.preparing_question_started_at = None;
        self.tool_preparing = None;
        self.tool_preparing_since = None;
        self.stop_waiting()?;
        if let Some(mut display) = self.command_display.take() {
            if self.timeline_enabled() {
                // 时间线下命令块只是个累加器，从不自己上屏。回合在它跑到一半
                // 时收尾（Ctrl+C、断线），就把它收成一步「已中断」——直接
                // `commit` 的话，inline 那套 `$ 运行命令×1 运行中 / ↳ / │`
                // 卡片会整块漏到全屏画面里（用户实测截图）。
                self.interrupt_command_display(display);
            } else {
                display.commit(
                    &mut self.output,
                    self.tool_call_mode == ToolCallDisplayMode::Summary,
                )?;
            }
        }
        self.end_subagent_stream_line()?;
        if self.mode == Some(ChatStreamKind::Content) && !self.plain {
            // 能力位要在借走 `self.output` 之前问:借用检查不让同时拿。
            let expandable = self.caps().expandable;
            let stdout = &mut self.output;
            let pending = self.sent_meme_filter.finish();
            if !pending.is_empty() {
                let rendered = self.markdown.push(&pending);
                let rendered = if expandable {
                    timeline::indent_body(&rendered)
                } else {
                    rendered
                };
                write!(stdout, "{rendered}")?;
            }
            let rendered = self.markdown.flush();
            let rendered = if expandable {
                timeline::indent_body(&rendered)
            } else {
                rendered
            };
            write!(stdout, "{rendered}")?;
            stdout.flush()?;
        }
        if self.mode == Some(ChatStreamKind::Reasoning) {
            execute!(self.output, ResetColor)?;
        }
        if stream_needs_terminating_newline(self.mode, self.captures_reasoning()) {
            writeln!(self.output)?;
        }
        self.finalize_reasoning_summary()?;
        self.finalize_tools_summary()?;
        self.cut_timeline()?;
        if self.summary_line_active {
            self.clear_summary_lines()?;
        }
        // 这一轮的子代理用量交还给会话累计：回合收尾时调用方会从库里重读 Σ，
        // 那时审计会话已经落盘，实时加数留着就是算两遍。
        self.subagent_tokens.clear();
        self.subagent_status.clear();
        self.mode = None;
        self.show_cursor()?;
        Ok(())
    }

    pub(crate) fn switch_mode(&mut self, mode: ChatStreamKind) -> Result<()> {
        let timeline = self.timeline_enabled();
        let stdout = &mut self.output;
        match mode {
            // 中转侧工具卡片不改变流式排版模式。
            ChatStreamKind::RemoteToolPreparing
            | ChatStreamKind::RemoteToolStarted
            | ChatStreamKind::RemoteToolFinished => {}
            ChatStreamKind::Reasoning => {
                if self.mode.is_some() {
                    writeln!(stdout)?;
                }
            }
            ChatStreamKind::Content => {
                if self.mode == Some(ChatStreamKind::Reasoning) {
                    execute!(stdout, ResetColor)?;
                    // 这两行空是给「思考正文直接铺在屏上」那种排版留的间距。
                    // 时间线下思考根本没往流里写（它进了时间线的一步），再补两行
                    // 就是收缩行和正文之间白白空三行。
                    if !timeline {
                        writeln!(stdout)?;
                        writeln!(stdout)?;
                    }
                }
            }
            ChatStreamKind::ToolCall => return Ok(()),
            ChatStreamKind::ReasoningPartStart | ChatStreamKind::ReasoningPartEnd => return Ok(()),
            ChatStreamKind::ReasoningReset => return Ok(()),
        }
        stdout.flush()?;
        self.mode = Some(mode);
        Ok(())
    }

    pub(crate) fn end_active_stream_line(&mut self) -> Result<()> {
        // 半行要冲出去了：先把活尾巴擦掉，冲出去的字落在它原来的位置上。
        self.clear_reply_tail()?;
        if self.captures_reasoning() && self.mode == Some(ChatStreamKind::Reasoning) {
            self.mode = None;
            return Ok(());
        }
        let was_reasoning = self.mode == Some(ChatStreamKind::Reasoning);
        if was_reasoning {
            execute!(self.output, ResetColor)?;
        } else if self.mode == Some(ChatStreamKind::Content) && !self.plain {
            // 能力位要在借走 `self.output` 之前问:借用检查不让同时拿。
            let expandable = self.caps().expandable;
            let stdout = &mut self.output;
            let rendered = self.markdown.flush();
            let rendered = if expandable {
                timeline::indent_body(&rendered)
            } else {
                rendered
            };
            write!(stdout, "{rendered}")?;
            stdout.flush()?;
        }
        if self.mode.is_some() {
            writeln!(self.output)?;
            if was_reasoning {
                writeln!(self.output)?;
            }
            self.mode = None;
        }
        Ok(())
    }

    pub(crate) fn hide_cursor(&mut self) -> Result<()> {
        if self.external_cursor_control {
            return Ok(());
        }
        if !self.cursor_hidden && !self.plain && self.wait_spinner.is_none() {
            execute!(self.output, Hide)?;
            self.cursor_hidden = true;
        }
        Ok(())
    }

    pub(crate) fn show_cursor(&mut self) -> Result<()> {
        if self.external_cursor_control {
            return Ok(());
        }
        if self.cursor_hidden && !self.plain {
            execute!(self.output, Show)?;
            self.cursor_hidden = false;
        }
        Ok(())
    }

    pub(crate) fn release_transient_output(&mut self) -> Result<()> {
        self.stop_waiting()?;
        // 时间线下命令块留着：它是那一步的累加器，跑完由 `write_tool_result`
        // 收进时间线。这儿提交的话，同一批里第二个工具一来（或者一张图要落），
        // 正在跑的命令就以 inline 那套卡片的样子漏到屏上。
        if !self.timeline_enabled() {
            if let Some(mut display) = self.command_display.take() {
                display.commit(
                    &mut self.output,
                    self.tool_call_mode == ToolCallDisplayMode::Summary,
                )?;
            }
        }
        self.end_subagent_stream_line()?;
        self.end_active_stream_line()?;
        self.finalize_reasoning_summary()?;
        self.clear_summary_lines()
    }

    /// 命令那一步的两份内容:抬头底下露着的几行、点开看到的全部。
    ///
    /// 详情就地印的面(没处点开)把命令那几行当成它能给的全部;能点开的面把命令
    /// 留在抬头底下、把命令加输出收进块里。这一手原来在两处逐字重复
    /// (`interrupt_command_display` 与 `write_tool_result`),改一处忘一处的经典
    /// 形状。
    fn command_step_parts(
        &self,
        display: &mut CommandLiveDisplay,
        ok: bool,
    ) -> (Vec<String>, Vec<String>) {
        let width = timeline::detail_width();
        let rows = display.command_rows(width, self.command_display_lines, !ok);
        if self.caps().detail_inline() {
            (rows, Vec::new())
        } else {
            (display.timeline_detail(width), rows)
        }
    }

    /// 回合收尾时命令还在跑：把它此刻的样子记到统计上，随后
    /// `finalize_tools_summary` 会把它收成一步（没跑完 = 已中断，红色打叉）。
    fn interrupt_command_display(&mut self, mut display: CommandLiveDisplay) {
        display.set_result(false);
        let (detail, tail) = self.command_step_parts(&mut display, false);
        // 命令工具在统计里叫什么名字（`run_command` / `Bash`）由事件决定，
        // 找那个还没跑完的就是它。
        let name = self
            .ordered_tool_stats()
            .into_iter()
            .find(|(name, stats)| is_command_tool(name) && !stats.settled())
            .map(|(name, _)| name.clone());
        if let Some(name) = name {
            let now = self.event_now();
            let stats = self.tool_stats_entry(&name);
            stats.elapsed = stats.started_at.map(|at| now.saturating_duration_since(at));
            stats.detail = detail;
            stats.tail = tail;
        }
    }
}

/// 「已回答 N 个问题」+ 每题一行「标题：答案」——**只出文字**，折行、裁宽、
/// 配色由各自的面去做。
///
/// 同一份内容有两个长相：静态时间线把它当那一步的正文（折行、跟着连线穿过去），
/// 全屏写成一块独立竖条（裁到面宽）。两边原来各抄了一遍措辞，靠人眼对齐——改一
/// 处忘一处就是两边说法不一样，而这两条路同一台机器上都会走到。
pub(crate) fn question_answer_text(
    request: &miyu_base::question::QuestionRequest,
    answers: &[Vec<String>],
) -> (String, Vec<String>) {
    let heading = format!(
        "{} {} {}",
        t("Answered", "已回答"),
        request.questions.len(),
        t("questions", "个问题")
    );
    let lines = request
        .questions
        .iter()
        .zip(answers)
        .map(|(prompt, selected)| {
            format!(
                "{}：{}",
                prompt.header.trim(),
                selected.join("、").replace('\n', " ")
            )
        })
        .collect();
    (heading, lines)
}

/// 收尾要不要补一个换行：只有**真往屏上流过字**的那条路要。
///
/// `captures_reasoning` 为真时思考一个字都没打到屏上（它进了时间线那一步），
/// 补换行就是凭空多一行。
pub(crate) fn stream_needs_terminating_newline(
    mode: Option<ChatStreamKind>,
    captures_reasoning: bool,
) -> bool {
    mode.is_some() && !(mode == Some(ChatStreamKind::Reasoning) && captures_reasoning)
}

#[derive(Default)]
pub(crate) struct SentMemeStreamFilter {
    pub(crate) pending: String,
    pub(crate) inside_tag: bool,
}

impl SentMemeStreamFilter {
    pub fn push(&mut self, text: &str) -> String {
        self.pending.push_str(text);
        let mut output = String::new();
        loop {
            if self.inside_tag {
                if let Some(end) = self.pending.find("</sent_meme>") {
                    let after = end + "</sent_meme>".len();
                    self.pending.drain(..after);
                    self.inside_tag = false;
                    continue;
                }
                self.pending.clear();
                return output;
            }

            let Some(start) = self.pending.find("<sent_meme>") else {
                let keep = longest_sent_meme_prefix_suffix(&self.pending);
                let emit_len = self.pending.len().saturating_sub(keep);
                output.push_str(&self.pending[..emit_len]);
                self.pending.drain(..emit_len);
                return output;
            };

            output.push_str(&self.pending[..start]);
            self.pending.drain(..start + "<sent_meme>".len());
            self.inside_tag = true;
        }
    }

    pub(crate) fn finish(&mut self) -> String {
        if self.inside_tag {
            self.pending.clear();
            self.inside_tag = false;
            return String::new();
        }
        std::mem::take(&mut self.pending)
    }
}

pub(crate) fn longest_sent_meme_prefix_suffix(text: &str) -> usize {
    const TAG: &str = "<sent_meme>";
    let max = TAG.len().saturating_sub(1).min(text.len());
    for len in (1..=max).rev() {
        if text.ends_with(&TAG[..len]) {
            return len;
        }
    }
    0
}

pub(crate) fn truncate_chars(text: &str, max_chars: usize) -> String {
    let total = text.chars().count();
    if total <= max_chars {
        return text.to_string();
    }
    let omitted = total - max_chars;
    format!(
        "{}\n... {} {omitted} {} ...",
        text.chars().take(max_chars).collect::<String>(),
        t("truncated", "已截断"),
        t("chars", "字符")
    )
}

pub use miyu_base::terminal::clip_progress_line;

pub(crate) fn clip_progress_line_preserving_spaces(text: &str, max_chars: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max_chars {
        text.to_string()
    } else {
        format!(
            "{}...",
            text.chars()
                .take(max_chars.saturating_sub(3))
                .collect::<String>()
        )
    }
}

impl Drop for StreamRenderer {
    fn drop(&mut self) {
        let _ = self.stop_waiting();
        if let Some(mut display) = self.command_display.take() {
            let _ = display.clear(&mut self.output);
        }
        if self.summary_line_active {
            let _ = self.clear_summary_lines();
            eprintln!();
        }
        let _ = self.show_cursor();
        if !self.plain {
            let _ = execute!(self.output, ResetColor);
        }
    }
}

pub(crate) fn normalize_stream_text(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// 流式版终端转义过滤:与 [`sanitize_terminal_text`] 同一状态机,但状态由
/// 调用方跨 delta 持有,转义序列被流切成两半也能整段拦下。
pub(crate) fn sanitize_stream_chunk(state: &mut TerminalControlState, text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    for ch in text.chars() {
        if let Some(ch) = sanitize_terminal_char(state, ch) {
            output.push(ch);
        }
    }
    output
}

pub(crate) fn write_full_reasoning_chunk(writer: &mut impl Write, text: &str) -> Result<()> {
    execute!(writer, SetForegroundColor(Color::Green))?;
    write!(writer, "{text}")?;
    Ok(())
}

pub(crate) fn print_reasoning(reasoning: &str) -> Result<()> {
    let mut stdout = io::stdout();
    execute!(stdout, SetForegroundColor(Color::Green))?;
    for line in reasoning.trim().lines() {
        writeln!(stdout, "  {line}")?;
    }
    execute!(stdout, ResetColor)?;
    if terminal::size().is_ok() {
        writeln!(stdout)?;
    }
    Ok(())
}
