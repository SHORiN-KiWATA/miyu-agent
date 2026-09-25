//! 子代理面板：内层时间线的记录、收缩与刷屏。
//!
//! 从 `timeline.rs` 搬来（09-16 拆分，那份超过了文件规模基线）。只搬不改。

use super::*;

mod panel;

use panel::{subagent_lines, subagent_title, PanelLook};
pub use panel::{thread_panel, PanelEntry};

/// 子代理面板最多留多少步。
const SUBAGENT_LOG_STEPS: usize = 400;

/// 「差事」那一步的图标（文档）。与后台任务面板里那条同一个，见
/// `cli::repl::tail::screen::overlay::PROMPT_GLYPH`。

/// 一段话的开头，给抬头用。
fn peek_head(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    crate::render::clip_to_display_width(&flat, max)
}

/// 把攒着的那段思考结算成一步。
///
/// `expand`：「展开思考内容」——这一步出来就是展开态，再点一次收回去。面板跟
/// 主线那两个开关走（用户 todolist:11「子代理浮层里的也要跟随这两个开关」）。
/// 它原来是把全文挂在抬头底下当预览，09-17 用户拍掉了那种形态。
fn flush_subagent_thought(log: &mut SubagentLog, expand: bool) {
    let text = std::mem::take(&mut log.reasoning);
    log.reasoning_rows.clear();
    let elapsed = log
        .reasoning_since
        .take()
        .map(|at| at.elapsed())
        .unwrap_or_default();
    if text.trim().is_empty() {
        return;
    }
    let body = wrap_detail(&text)
        .into_iter()
        .map(|line| format!("{THOUGHT_BODY_STYLE}{line}\x1b[0m"))
        .collect::<Vec<_>>();
    log.segment.counts.thoughts += 1;
    log.segment.note_start_since(elapsed, Instant::now());
    let mut step = Step::new(
        step_line_in(
            glyph_think(),
            &timed_label(t("thought", "已思考"), elapsed),
            panel_step_width(),
        ),
        body.clone(),
        None,
    );
    step.kind = StepKind::Thought;
    step.set_open(expand);
    log.steps.push(step);
    trim_subagent(log);
}

/// 它刚才说的那段话封成一步，钉在此刻的位置上。
///
/// 正文原来一直挂在面板最底下（`log.speech`）——说完一段再接着想、接着动手时，
/// 新的「思考中」和后面的步骤都排到了它**上面**，看着像它还没开口就先想了下一
/// 步（用户实测截图）。说过的话是时间线上的一件事，按时序占位；只有**还在说**
/// 的那段才留在底下。
fn seal_subagent_speech(log: &mut SubagentLog) {
    let text = std::mem::take(&mut log.speech);
    if text.trim().is_empty() {
        return;
    }
    log.steps.push(Step::speech(render_speech_lines(
        text.trim(),
        detail_width(),
    )));
    trim_subagent(log);
}

/// 这一段过程从第几步开始：「提示词」那一行和已经说过的正文都不算过程。
fn segment_start(log: &SubagentLog) -> usize {
    log.steps
        .iter()
        .rposition(|step| step.kind == StepKind::Speech)
        .map(|index| index + 1)
        .unwrap_or(usize::from(log.has_prompt))
}

/// 收起来的那几步长什么样：各自包成块、用连线串起来。
///
/// 主线的 `cut_timeline` 和面板的 `collapse_subagent_segment` 原来各写一份
/// （报告 §2.1 第 4 条数出「`Worked for` 收缩有三份实现」）。两份的规则本来就是
/// 同一条——**块 id 优先取它自己的**（子代理挂它那块流水账、live 区登记过的那块
/// 接着用），没有就现登记一个；正文为空的那些不登记。
///
/// 面板那一份还少了两样，合并之后一并补上：
///
/// - **走 `step_rows`**：它原来拿 `step.line` 自己拼行，于是命令那一步抬头底下
///   露着的几行、以及「出来就展开」那一位，一收段就都没了。
/// - **块 id 复用**：它每次收段都现登记一个新 id，而主线那份接着用 live 区
///   那块——点开着的那一步收段之后仍然点开着。
pub fn fold_block_lines(steps: &mut [Step]) -> Vec<String> {
    thread(steps.iter_mut().map(|step| {
        // 正文为空、也不是子代理：没什么可点开的，不占登记处。
        if step.overlay.is_none() && step.body.is_empty() {
            return step_rows(step, None);
        }
        // 展开这一步时**头行留着**：它是把手，再点一次才收得回去；
        // 正文缩进到竖线右边，和折叠态对得上列。
        let target = match step.overlay {
            // 子代理直接挂它那块流水账：点开是覆盖层。
            Some(id) => Some(id),
            // live 区里用的那块，收缩之后还是它：点开着的保持点开。**内容要重灌**
            // ——后台面板每帧重解析，这一步的正文可能又长了一截；主线那边内容不
            // 再变，重灌是同一份，代价只有一次版本号自增。
            None => match step.block {
                Some(id) => {
                    blocks::update(id, String::new(), step_detail(step));
                    Some(id)
                }
                None => {
                    let id = blocks::register(step_detail(step));
                    // 记回去：调用方要拿它按位置复用（后台面板每帧重解析，不复用
                    // 的话用户点开的那一块下一帧就换 id、当场合上）。
                    step.block = id;
                    id
                }
            },
        };
        step_rows(step, target)
    }))
}

/// 把面板里已经走完的那几步收成一行 `⌄ Worked for …`，点开还是那几步。
///
/// 「提示词」那一行钉在最前面不参与收缩——它说的是"要干什么"，不是过程。
/// 说过的正文也不收：它是产出，收缩的是产出之前的过程。
fn collapse_subagent_segment(log: &mut SubagentLog) {
    let from = segment_start(log);
    if log.steps.len() <= from + 1 {
        // 一步（或没有）就不值得收：收完那一行比原来还长。
        log.segment = Timeline::default();
        return;
    }
    let counts = log.segment.counts;
    let summary = summary_line(log.segment.elapsed(Instant::now()), counts);
    let collapsed: Vec<Step> = log.steps.drain(from..).collect();
    // 收起来的每一步**还是块**：点开收缩行看到的是时间线，时间线里每一步再点开
    // 才是它的正文。原来只把抬头串起来，工具输出、思考全文在收缩那一刻就没了
    //（用户实测：浮层中的收缩行为异常，会丢失内容）。规则与主线共用一份。
    let mut collapsed = collapsed;
    let body = fold_block_lines(&mut collapsed);
    log.step_blocks.truncate(from);
    // 收缩行点开是时间线（`fold`）：抬头、连线、各步同一列，不再当正文缩进。
    let mut fold = Step::new(
        step_line_in(FOLD_GLYPH_CLOSED, &summary, panel_step_width()),
        body,
        None,
    );
    fold.kind = StepKind::Fold;
    log.steps.push(fold);
    log.segment = Timeline::default();
}

fn trim_subagent(log: &mut SubagentLog) {
    if log.steps.len() > SUBAGENT_LOG_STEPS {
        let excess = log.steps.len() - SUBAGENT_LOG_STEPS;
        // 「差事」那一行钉在最前面：它是整块面板里唯一说得清"在干什么"的一行，
        // 被挤掉之后剩下的全是过程。
        let from = usize::from(log.has_prompt);
        log.steps.drain(from..from + excess);
        // 块 id 是按位置对齐的，步挪了它也得跟着挪。
        let end = (from + excess).min(log.step_blocks.len());
        if end > from {
            log.step_blocks.drain(from..end);
        }
    }
}

/// 一个子代理的内层时间线。
///
/// 形状和主线一模一样——子代理干的事和主体是同一类事，没有理由换一套看法。
#[derive(Default)]
pub(crate) struct SubagentLog {
    pub(crate) id: Option<u64>,
    /// 它的会话（09-18 起子代理是一条会话）。有了它，点时间线上这一行就切进那条
    /// 会话（会话项目第 3 段），面板那块登记时一并挂上。
    session: Option<String>,
    /// 最近一次统计（工具次数 / 词元估算）。挂在面板标题上。
    stats: Option<String>,
    /// 同一份量的短标（`≈3.1K`）。挂在**时间线那一行**上——不点开就想知道
    /// 它烧了多少（用户实测：前台子代理完成后没有显示 token 消耗）。
    tokens: Option<String>,
    /// 跑完了没有。跑完的那个不再开窗——它已经收成主线时间线里的一步了。
    finished: bool,
    steps: Vec<Step>,
    /// 正在累积的思考。下一步工具落下时（或收尾时）结算成一步。
    reasoning: String,
    /// `reasoning` 折好的行，只往后补（见 `ThoughtRows`）：「思考中」那一块点开用的全文
    /// 每次刷新面板都要，整段重折的话子代理想得越久越卡。
    reasoning_rows: ThoughtRows,
    /// 这一段思考是什么时候开始的。
    reasoning_since: Option<Instant>,
    /// 正在跑的那个子工具是什么时候开始的。内层事件本身不带耗时，只能自己掐表。
    tool_since: Option<Instant>,
    started: Option<Instant>,
    /// 第一步是不是「差事」那一行。见 [`StreamRenderer::subagent_prompt`]。
    has_prompt: bool,
    /// 每一步对应的块 id，按位置复用（0 表示这一步没有正文、不登记）。
    /// 见 [`subagent_lines`]。
    step_blocks: Vec<u64>,
    /// 「思考中」那一行的块 id。同样复用，见 [`subagent_lines`]。
    live_block: Option<u64>,
    /// 内层正在流工具参数：`准备编辑 · 1.2s`。主线有这一行，面板里原来没有
    ///（用户实测：浮层中没有「准备xx」系列输出）。
    preparing: Option<(&'static str, &'static str, Instant)>,
    /// 内层正在跑的工具：`(图标, 名字, 窥视, 起点)`。原来调用发出到结果回来
    /// 之间面板里什么都没有，看着像卡住了。
    /// 最后一项是命令那一步抬头底下露的命令（不是命令就空着）。
    running: Option<(&'static str, String, Option<String>, Instant, Vec<String>)>,
    /// 它正在说的那段正文。见 [`StreamRenderer::subagent_content`]。
    speech: String,
    /// 这一段过程的计数与起点，收成 `Worked for …` 那一行时要用。
    segment: Timeline,
}

impl StreamRenderer {
    /// 跑着的那个子代理此刻在干什么，压成一行。
    ///
    /// 取它内层时间线的**最后一步**：正在想就露想到哪儿了，正在跑工具就露那个
    /// 工具，正在说话就露说到哪儿了。每帧都在变，一眼看得出它还活着。
    /// 命令那一步抬头底下露的命令（见 `panel_command_tail`）。模型没给 title 时抬头
    /// 上已经是命令本身了，底下再露一遍就是同一句话说两遍，那就不露。
    fn command_tail(&self, tool: &str, args: &str) -> Vec<String> {
        if !crate::render::is_command_tool(tool) || super::command_peek(args).is_none() {
            return Vec::new();
        }
        panel_command_tail(
            &crate::render::command_from_arguments(args),
            self.command_display_lines,
        )
    }

    pub(super) fn subagent_peek(&self, name: &str) -> Option<String> {
        let log = self.subagent_logs.get(name)?;
        let width = crate::render::command_terminal_width()
            .saturating_sub(48)
            .max(16);
        if !log.reasoning.trim().is_empty() {
            return Some(peek_tail(&log.reasoning, width));
        }
        if !log.speech.trim().is_empty() {
            return Some(peek_tail(&log.speech, width));
        }
        let last = log.steps.last()?;
        if last.kind == StepKind::Speech {
            return Some(peek_tail(&last.body.join(" "), width));
        }
        // 步那一行自带缩进和颜色，窥视要的是干净的一句话。
        let text = crate::render::strip_ansi_text(&last.line);
        Some(peek_tail(text.trim(), width))
    }

    /// 子代理想了一段。先攒着，等它开始动手（或收尾）才结算成一步——
    /// 每来一个 delta 就记一步的话，面板里全是碎片。
    pub(crate) fn subagent_thought(&mut self, name: &str, text: &str) {
        if !self.timeline_enabled() || text.trim().is_empty() {
            return;
        }
        let log = self.subagent_logs.entry(name.to_string()).or_default();
        log.started.get_or_insert_with(Instant::now);
        // 说完一段又开始想 = 那段话说完了，按时序封成一步。
        seal_subagent_speech(log);
        // 新一轮思考开始了，上一轮的「准备xx」不再作数。
        log.preparing = None;
        log.reasoning_since.get_or_insert_with(Instant::now);
        log.reasoning.push_str(text);
        self.publish_subagent(name);
    }

    /// 子代理正在流某个工具的参数（主线那种「准备编辑」）。
    pub(crate) fn subagent_tool_preparing(&mut self, name: &str, tool: &str) {
        if !self.timeline_enabled() {
            return;
        }
        let Some(phase) = miyu_engine::tools::preparing_phase(tool) else {
            return;
        };
        let expand_thought = self.reasoning_mode == ReasoningDisplayMode::Full;
        let log = self.subagent_logs.entry(name.to_string()).or_default();
        log.started.get_or_insert_with(Instant::now);
        // 参数开始流 = 这一段想完了、话也说完了：先按时序封掉，「准备xx」才排
        // 在它们后面。原来思考要等结果回来才结算，面板里「准备执行」一直压在
        // 「思考中」上头，思考的耗时还把工具跑的时间算了进去。
        seal_subagent_speech(log);
        flush_subagent_thought(log, expand_thought);
        if log.preparing.is_none() {
            log.preparing = Some((phase, tool_glyph(tool), Instant::now()));
        }
        self.publish_subagent(name);
    }

    /// 子代理开始调一个工具：掐表，并在面板里露出「正在跑」那一行。
    /// 内层事件不带耗时，不自己记就只能不显示。
    pub(crate) fn subagent_tool_started(
        &mut self,
        name: &str,
        tool: &str,
        display: &str,
        args: &str,
    ) {
        if !self.timeline_enabled() {
            return;
        }
        // 命令给 title、底下露命令——和跑完的那一步（`subagent_tool`）、和主线一个
        // 样子。原来跑着时窥视是命令本身、截成一行，跑完又换成 title，同一步前后
        // 两个样子。
        let command_tail = self.command_tail(tool, args);
        let peek = if crate::render::is_command_tool(tool) {
            super::command_peek(args).or_else(|| crate::render::tool_peek(tool, args))
        } else {
            crate::render::tool_peek(tool, args)
        }
        .filter(|subject| !subject.trim().is_empty())
        .map(|subject| crate::render::clip_to_display_width(&subject, 72));
        let expand_thought = self.reasoning_mode == ReasoningDisplayMode::Full;
        let log = self.subagent_logs.entry(name.to_string()).or_default();
        log.started.get_or_insert_with(Instant::now);
        seal_subagent_speech(log);
        flush_subagent_thought(log, expand_thought);
        log.preparing = None;
        log.running = Some((
            tool_glyph(tool),
            display.to_string(),
            peek,
            Instant::now(),
            command_tail,
        ));
        log.tool_since = Some(Instant::now());
        self.publish_subagent(name);
    }

    /// 子代理调完一个工具：结算掉在它之前那段思考，再记这一步。
    pub(crate) fn subagent_tool(
        &mut self,
        name: &str,
        tool: &str,
        display: &str,
        args: &str,
        ok: bool,
        output: &str,
    ) {
        if !self.timeline_enabled() {
            return;
        }
        // 窥视按工具自己的规矩摘一句，摘不出来才退回原文。原样甩一行
        // `{"patchText": "*** Begin Patch\n…"}` 出来，那一行就再也读不出是在
        // 改哪个文件了（用户实测截图）。
        // 命令的主题是**全文**：`tool_peek` 那份截成了一行 80 字，点开看到的也只是
        // 那一行加「..」（用户 09-24 截图）。
        let subject = if crate::render::is_command_tool(tool) {
            crate::render::command_from_arguments(args)
        } else {
            crate::render::tool_peek(tool, args).unwrap_or_default()
        };
        let command_tail = self.command_tail(tool, args);
        // 和主线同一套规矩:命令给 title,编辑给路径加 `+3 -1`。浮层拿不到
        // `__patch_preview__` 的真 diff,按调用参数里那份信封数。
        let peek = if crate::render::is_command_tool(tool) {
            super::command_peek(args)
        } else if subject.trim().is_empty() {
            None
        } else {
            let head = crate::render::clip_to_display_width(&subject, 72);
            Some(match crate::render::envelope_diff_stat(tool, args) {
                Some((added, removed)) => {
                    format!(
                        "{head}{PEEK_SEP}{}",
                        crate::render::diff_stat_label(added, removed)
                    )
                }
                None => head,
            })
        };
        let mut body = Vec::new();
        // 编辑类工具：正文给 **diff**，不给参数也不给那份结果 JSON。
        // 工具自己跑那条路会用改前改后算真 diff（`__patch_preview__`），但那条
        // 只到发起它的渲染器；子代理内层的编辑手上只有调用参数里的信封，照它画。
        let diff = crate::render::patch_envelope_lines_from_args(tool, args, detail_width());
        match diff {
            Some(lines) => body.extend(lines),
            None => {
                // 和主线那一步点开一个样子：主题（命令全文／路径）一段、空一行、
                // 然后是输出。`$` 是抬头上的图标，正文里不再写一遍。
                if !subject.trim().is_empty() {
                    body.extend(
                        wrap_detail(&subject)
                            .into_iter()
                            .map(|piece| format!("\x1b[2m{piece}\x1b[0m")),
                    );
                }
                let output = tool_output_lines(output);
                if !body.is_empty() && !output.is_empty() {
                    body.push(String::new());
                }
                body.extend(output);
            }
        }
        // 档位要在借走 `subagent_logs` 之前问：借用检查不让同时拿。
        let expand_details = self.tool_call_mode == crate::render::ToolCallDisplayMode::Full;
        let expand_thought = self.reasoning_mode == ReasoningDisplayMode::Full;
        let log = self.subagent_logs.entry(name.to_string()).or_default();
        log.started.get_or_insert_with(Instant::now);
        seal_subagent_speech(log);
        flush_subagent_thought(log, expand_thought);
        log.running = None;
        log.preparing = None;
        let glyph = if ok { tool_glyph(tool) } else { glyph_err() };
        let failed = !ok;
        let elapsed_of_step = log
            .tool_since
            .take()
            .map(|since| since.elapsed())
            .unwrap_or_default();
        let mut label = timed_label(display, elapsed_of_step);
        if let Some(peek) = peek {
            label.push_str(PEEK_SEP);
            label.push_str(&peek);
        }
        log.segment.counts.record_tool(tool, failed);
        log.segment
            .note_start_since(elapsed_of_step, Instant::now());
        let mut step = Step::new(
            if failed {
                step_line_failed_in(glyph, &label, panel_step_width())
            } else {
                step_line_in(glyph, &label, panel_step_width())
            },
            body.clone(),
            None,
        );
        step.kind = StepKind::Tool;
        step.set_tail(command_tail);
        // 「展开工具内容」：这一步出来就是展开态——浮层跟着主线那两个开关走
        //（用户 todolist:11 最后一句）。
        step.set_open(expand_details);
        log.steps.push(step);
        trim_subagent(log);
        self.publish_subagent(name);
    }

    /// 每个 tick 把还活着的面板重新灌一遍，「准备执行 · 1.2s」、标题上的秒数才会
    /// 走。面板内容不归转轮管，只在事件到来时重生成——不灌的话它停在上一次事件
    /// 那一刻（用户实测：浮层里 `准备执行 · 0.0s` 不动）。十分之一秒灌一次够了，
    /// 秒数就是这个精度。
    pub(crate) fn refresh_subagent_panels(&mut self) {
        if !self.caps().expandable {
            return;
        }
        let now = Instant::now();
        if self
            .last_subagent_refresh
            .is_some_and(|last| now.duration_since(last) < Duration::from_millis(100))
        {
            return;
        }
        self.last_subagent_refresh = Some(now);
        let live: Vec<String> = self
            .subagent_logs
            .iter()
            .filter(|(_, log)| log.id.is_some() && !log.finished)
            .map(|(name, _)| name.clone())
            .collect();
        for name in live {
            self.publish_subagent(&name);
        }
    }

    /// 把一个子代理的时间线灌进它的覆盖层块。
    fn publish_subagent(&mut self, name: &str) {
        let display = self.display_tool_name(name);
        let look = PanelLook {
            thought_lines: self.thinking_scroll_lines,
            expand_thought: self.reasoning_mode == ReasoningDisplayMode::Full,
        };
        let Some(log) = self.subagent_logs.get_mut(name) else {
            return;
        };
        let title = subagent_title(log, &display);
        let lines = subagent_lines(log, look);
        let id = log.id;
        match id {
            Some(id) => blocks::update(id, title, lines),
            None => {
                let id = blocks::register_overlay(title, lines);
                if let Some(log) = self.subagent_logs.get_mut(name) {
                    log.id = id;
                    if let (Some(id), Some(session)) = (id, log.session.as_deref()) {
                        blocks::link_session(id, session);
                    }
                }
            }
        }
    }

    /// 这个子代理的会话 id 到了：派出去时的标记（实时），或者结果里带的（回放）。
    pub(crate) fn subagent_session(&mut self, name: &str, session_id: &str) {
        let session_id = session_id.trim();
        if session_id.is_empty() {
            return;
        }
        let log = self.subagent_logs.entry(name.to_string()).or_default();
        log.session = Some(session_id.to_string());
        if let Some(id) = log.id {
            blocks::link_session(id, session_id);
        }
    }

    /// 子代理收尾：把最后一段思考也结算掉。
    /// 派出去时交给它的差事。面板里的第一步，点开看全文。
    ///
    /// 面板里原来全是它自己的动作——思考、调工具——唯独没有"它被要求干什么"。
    /// 那件事只有派它出去的那一轮知道，而隔几分钟回来看这个面板的人是没有那
    /// 一轮的（用户：子代理的开头应该是 prompt 工具行）。
    pub(crate) fn subagent_prompt(&mut self, name: &str, arguments: &str) {
        if !self.timeline_enabled() {
            return;
        }
        let prompt = serde_json::from_str::<serde_json::Value>(arguments)
            .ok()
            .and_then(|value| {
                ["prompt", "task", "instructions"].iter().find_map(|key| {
                    value
                        .get(*key)
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string)
                })
            })
            .unwrap_or_default();
        let prompt = prompt.trim().to_string();
        if prompt.is_empty() {
            return;
        }
        let log = self.subagent_logs.entry(name.to_string()).or_default();
        if log.has_prompt {
            return;
        }
        log.started.get_or_insert_with(Instant::now);
        log.has_prompt = true;
        let body = wrap_detail(&prompt)
            .into_iter()
            .map(|line| format!("\x1b[2m{line}\x1b[0m"))
            .collect::<Vec<_>>();
        let head = format!(
            "{}{PEEK_SEP}{}",
            t("prompt", "提示词"),
            peek_head(&prompt, panel_step_width())
        );
        // 插在最前面 → 后面每一步的位置都往后挪了一格，块表按位置对齐，重来一轮。
        log.step_blocks.clear();
        let mut prompt = Step::new(
            step_line_in(prompt_glyph(), &head, panel_step_width()),
            body,
            None,
        );
        prompt.kind = StepKind::Prompt;
        log.steps.insert(0, prompt);
        self.publish_subagent(name);
    }

    /// 子代理开口说正文了。
    ///
    /// 和主线一个规矩：**开始说话就把前面那一段过程收成一行** `Worked for …`
    /// （用户：可以把前面已经完成的 timeline 在浮层里缩成 Worked for）。面板里
    /// 一路平铺着几十步的话，真正的产出反而被埋在最底下。
    pub(crate) fn subagent_content(&mut self, name: &str, text: &str) {
        if !self.timeline_enabled() || text.is_empty() {
            return;
        }
        let expand_thought = self.reasoning_mode == ReasoningDisplayMode::Full;
        // 收不收段跟着「过程收起成一行摘要」走——浮层原来是无条件收的，于是
        // 那个开关在浮层里等于不存在（用户 09-17：「子代理浮层也不受这个开关的
        // 影响」）。
        let fold = self.fold_timeline;
        let log = self.subagent_logs.entry(name.to_string()).or_default();
        log.started.get_or_insert_with(Instant::now);
        if log.speech.is_empty() {
            flush_subagent_thought(log, expand_thought);
            if fold {
                collapse_subagent_segment(log);
            }
        }
        log.speech.push_str(text);
        self.publish_subagent(name);
    }

    /// 子代理报了一次统计。
    pub(crate) fn subagent_stats(&mut self, name: &str, text: &str, tokens: Option<&str>) {
        if !self.timeline_enabled() {
            return;
        }
        let log = self.subagent_logs.entry(name.to_string()).or_default();
        log.started.get_or_insert_with(Instant::now);
        log.stats = Some(text.to_string());
        if let Some(tokens) = tokens.map(str::trim).filter(|text| !text.is_empty()) {
            log.tokens = Some(tokens.to_string());
        }
        self.publish_subagent(name);
    }

    /// 这个子代理至此烧了多少（短标，给时间线那一行用）。
    pub(crate) fn subagent_tokens_label(&self, name: &str) -> Option<String> {
        self.subagent_logs.get(name)?.tokens.clone()
    }

    pub(crate) fn finish_subagent_log(&mut self, name: &str) {
        if !self.timeline_enabled() {
            return;
        }
        let expand_thought = self.reasoning_mode == ReasoningDisplayMode::Full;
        if let Some(log) = self.subagent_logs.get_mut(name) {
            flush_subagent_thought(log, expand_thought);
            // 跑完了就不再开那扇四行的窗——它已经收成主线上的一步了。
            log.finished = true;
        }
        self.publish_subagent(name);
    }

    /// 这一刻跑着的子代理**一共**烧了多少词元。
    ///
    /// 会话累计（footer 上的 Σ）要等子代理跑完、审计会话落盘才动；而一个子代理
    /// 能跑好几分钟，那几分钟里 Σ 纹丝不动（用户问：这个 token 消耗记录有每步
    /// 刷新到会话累计吗）。跑着的时候先把这份加上去，回合收尾时 Σ 从库里重读、
    /// 这份清零，不会算两遍。
    pub fn running_subagent_tokens(&self) -> u64 {
        self.subagent_tokens.values().copied().sum()
    }

    /// 这个子代理的覆盖层 id（没有就没有）。
    pub(crate) fn subagent_overlay_id(&self, name: &str) -> Option<u64> {
        self.subagent_logs.get(name).and_then(|log| log.id)
    }
}
