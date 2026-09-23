//! 工具活动摘要的渲染。
//!
//! 摘要是**活的**：工具还在跑时逐帧刷新，跑完就固定下来
//! （`finish_subagent_timer`）。并行子代理各占一块，settled 的要冻在原地，不能
//! 因为别人还在跑就跟着重排。

use super::timeline::{command_peek, tool_output_lines};
use crate::render::*;

impl StreamRenderer {
    pub fn write_tool_call(&mut self, name: &str, arguments: &str) -> Result<()> {
        // The arguments finished arriving, so the "still receiving" hint has
        // done its job and hands the spinner back to the tool summary.
        self.tool_preparing = None;
        if self.plain {
            return Ok(());
        }
        if name == "ask_question" {
            return self.start_preparing_question();
        }
        self.release_transient_output()?;
        if is_silent_tool(name) {
            return Ok(());
        }
        if is_command_tool(name) {
            let mut display = CommandLiveDisplay::new(
                arguments,
                self.command_display_lines,
                self.tool_call_mode != ToolCallDisplayMode::Hidden,
                self.tool_call_mode == ToolCallDisplayMode::Full,
            );
            // 全屏：命令块不再自己往屏上画一大片，它在时间线里只占一行，
            // 命令文本当窥视；完整的命令与输出留到点开时才看。
            if self.timeline_enabled() {
                self.command_display = Some(display);
                let peek = command_peek(arguments);
                let stats = self.tool_stats_entry(name);
                stats.calls += 1;
                stats.started_at.get_or_insert_with(std::time::Instant::now);
                stats.peek = peek;
                self.ensure_tool_waiting_phase()?;
                return Ok(());
            }
            if self.live_summary {
                display.tick(&mut self.output)?;
                self.last_tick = None;
            }
            self.command_display = Some(display);
            return Ok(());
        }
        if is_subagent_tool(name) && self.tool_call_mode != ToolCallDisplayMode::Hidden {
            let stats = self.tool_stats_entry(name);
            stats.started_at = Some(std::time::Instant::now());
            stats.elapsed = None;
            // 面板的第一步：交给它的差事。派出去这一刻是唯一还看得见它的地方。
            self.subagent_prompt(name, arguments);
        }
        if self.captures_tools() {
            let stats = self.tool_stats_entry(name);
            stats.calls += 1;
            stats.subject = tool_subject(name, arguments);
            // 每个工具都掐表，不只命令和子代理。时间线上那一步要报秒数，收缩行的
            // `Worked for` 要把它算进去——原来普通工具不掐，一段里只有普通工具时
            // 收缩行就成了光秃秃的 `1 tool · 1 err`（用户实测）。
            stats.started_at.get_or_insert_with(std::time::Instant::now);
            if self.timeline_enabled() && tool_event_base_name(name) == timeline::SEND_TOOL {
                self.note_cross_session_send(name, arguments);
            }
            self.ensure_tool_waiting_phase()?;
        } else if self.tool_call_mode == ToolCallDisplayMode::Full {
            let display_name = self.display_tool_name(name);
            let stdout = &mut self.output;
            writeln!(stdout, "{} {}", t("tool", "工具"), display_name)?;
            write_tool_payload(stdout, t("args", "参数"), arguments)?;
            stdout.flush()?;
        }
        Ok(())
    }

    /// 工具要不要**收进时间线那一步**，而不是打成旧卡片直接上屏。
    ///
    /// 和 `captures_reasoning` 同一个道理：`Full` 那一档原来一律走
    /// 「`工具 x` / `参数 …` / `结果 x ok` / `输出 …`」的老卡片，**绕过时间线**
    /// ——于是全屏 + `显示工具调用信息 = 详细` 是个谁都没设计过的混合态：命令走
    /// 时间线、别的工具打卡片，两种版式在同一屏上叠着（报告 §1.1 的 S5，用户
    /// todolist:11「用的是旧版本的 inline」）。
    ///
    /// 有时间线就收进去，档位只决定**详情摆哪**：详细档把工具输出摆在那一步
    /// 底下，摘要档收在块里点开才看。没有时间线的面（管道、真·inline）照旧。
    pub(crate) fn captures_tools(&self) -> bool {
        match self.tool_call_mode {
            ToolCallDisplayMode::Hidden => false,
            ToolCallDisplayMode::Summary => true,
            ToolCallDisplayMode::Full => self.timeline_enabled(),
        }
    }

    pub fn write_tool_preparing(&mut self, name: &str, batch: bool) -> Result<()> {
        if self.plain {
            return Ok(());
        }
        // 工具自己的提示优先——它说得更具体；没有的话，批量场景退到通用
        // 提示，总比整段空白强。
        let phase = miyu_engine::tools::preparing_phase(name)
            .or_else(|| batch.then(miyu_engine::tools::batch_preparing_phase));
        let Some(phase) = phase else {
            return Ok(());
        };
        // 参数每流一片就来一条准备事件。同一阶段的转轮已经在转了，就只更新
        // 文字——原来每条都先 `release_transient_output`（顺手把转轮停掉）再起一
        // 个新的，转轮在第 0、1 帧之间反复重起，跟着参数流的节奏抖
        //（用户实测：主体「准备xx」的转轮特别快、特别鬼畜）。
        let same_phase = self
            .tool_preparing
            .is_some_and(|(current, _, _)| current == phase)
            && self.wait_spinner.is_some();
        if !same_phase {
            self.release_transient_output()?;
        }
        // Set before the spinner exists: `ensure_waiting_phase` ticks
        // immediately, and that tick re-derives the phase from renderer state.
        // Without the sticky field the tool summary or the reasoning timer wins
        // there and the hint never reaches the screen.
        let since = *self
            .tool_preparing_since
            .get_or_insert_with(std::time::Instant::now);
        self.tool_preparing = Some((phase, crate::render::tool_glyph_for(name), since));
        // Braille + the dim tool palette: this is a tool starting up, not the
        // model thinking, and the scanner/green pair reads as the latter.
        self.ensure_waiting_phase(self.waiting_phase_text(), SpinnerStyle::Braille)
    }

    pub fn write_tool_result(&mut self, name: &str, ok: bool, output: &str) -> Result<()> {
        if self.plain {
            return Ok(());
        }
        if is_silent_tool(name) && ok {
            return Ok(());
        }
        self.stop_waiting()?;
        self.tool_preparing_since = None;
        self.reanchor_wait_timer();
        self.end_subagent_stream_line()?;
        if is_subagent_tool(name) {
            self.finish_subagent_log(name);
        }
        let status = if ok { "ok" } else { "err" };
        let elapsed = self.finish_subagent_timer(name);
        if is_command_tool(name) {
            if self.timeline_enabled() {
                // `ok` 说的是"工具本身有没有出错"。命令退出码非零时工具照样是
                // Ok 的——只看 `ok` 的话，一条 `exit 3` 的命令在时间线上和跑成了
                // 长得一模一样。退出码才是用户眼里的"跑失败了"。
                let ok = ok
                    && crate::render::parse_command_result(output)
                        .is_none_or(|result| result.success);
                // 全屏：完整命令 + 完整输出，点开才看。静态版没处点开，就地
                // 抬头底下印**命令本身**,不是输出(用户 09-17 裁定:跑了什么
                // 要紧,输出退到点开里)。两份内容怎么分,见 `command_step_parts`。
                let (detail, tail) = match self.command_display.take() {
                    Some(mut display) => {
                        display.set_result(ok);
                        self.command_step_parts(&mut display, ok)
                    }
                    None => (Vec::new(), Vec::new()),
                };
                let stats = self.tool_stats_entry(name);
                if ok {
                    stats.ok += 1;
                } else {
                    stats.error += 1;
                }
                stats.elapsed = stats.started_at.map(|at| at.elapsed());
                stats.detail = detail;
                stats.tail = tail;
                return self.settle_tool_batch();
            }
            if let Some(mut display) = self.command_display.take() {
                display.set_result(ok);
                let include_output = self.tool_call_mode == ToolCallDisplayMode::Summary
                    || (self.tool_call_mode == ToolCallDisplayMode::Full && !ok);
                if self.live_summary {
                    display.commit(&mut self.output, include_output)?;
                } else {
                    display.write_static(&mut self.output, include_output)?;
                }
                self.last_tick = None;
            }
            if self.tool_call_mode == ToolCallDisplayMode::Full {
                let stdout = &mut self.output;
                write_command_result_blocks(stdout, output)?;
                stdout.flush()?;
            }
            return Ok(());
        }
        if name == "todowrite" && ok {
            self.release_transient_output()?;
            let stdout = &mut self.output;
            // 旧 JSON 输出(历史回放)在这里画表;新文本输出的表格已由
            // __todo_table__ progress 画过,这里只收掉状态行不再重复展示。
            let rendered = write_todo_table(stdout, output)?;
            if rendered {
                stdout.flush()?;
            }
            if rendered || output.trim_start().starts_with("todo list ") {
                // 全屏：清单也是这一轮真做过的一件事，时间线里得有它那一步
                //（用户实测：好像没看到 todolist 的工具 tag 行）。表本身排在
                // 时间线收完之后（`pending_after_timeline`），不占这一步的位置。
                //
                // inline 那边照旧把状态行整个收掉——那儿表已经就地画出来了，
                // 再留一行「任务清单×1 ok」是同一件事说两遍。注意**不能**顺手
                // `tool_stats.clear()`：同一批里别的工具会被一起抹掉。
                if self.timeline_enabled() {
                    let stats = self.tool_stats_entry(name);
                    stats.ok += 1;
                    stats.progress = None;
                    if stats.elapsed.is_none() {
                        stats.elapsed = stats.started_at.map(|at| at.elapsed());
                    }
                    // 和别的工具一样结算：不结算的话这一步要等下一个事件才收
                    // 进时间线，live 区里它会一直挂着转轮。
                    return self.settle_tool_batch();
                }
                if self.tool_call_mode == ToolCallDisplayMode::Summary {
                    let stats = self.tool_stats_entry(name);
                    stats.ok += 1;
                    stats.progress = None;
                    self.tool_stats.clear();
                    self.last_tool_summary.clear();
                }
                return Ok(());
            }
        }
        if self.captures_tools() {
            // 全屏：把工具**真实的输出**留下来。摘要那几行只说了「跑没跑成」，
            // 点开却什么都看不到的话，收起来就等于丢了。
            //
            // 点不开的面 + 收起档：普通工具只留那一行，成败都不印输出——输出是
            // 给模型看的，不是给人扫的；报错更多时候是一团裸 JSON，印出来只会丑
            //（用户拍板：除了命令，其他工具报错不需要报错信息）。
            // 「展开工具内容」开着时那份输出就是要看的东西，于是留下来：能点开
            // 的面上它是展开态的正文，点不开的面上它就地印在抬头底下。
            let expand_tools = self.tool_call_mode == ToolCallDisplayMode::Full;
            let detail = if self.caps().detail_inline() && !expand_tools {
                None
            } else {
                self.timeline_enabled().then(|| tool_output_lines(output))
            };
            let stats = self.tool_stats_entry(name);
            if ok {
                stats.ok += 1;
            } else {
                stats.error += 1;
            }
            // 跑完就把表停下：时间线上那一步报的是它自己花的时间，不是到收缩为止。
            if stats.elapsed.is_none() {
                stats.elapsed = stats.started_at.map(|at| at.elapsed());
            }
            // 已经有更好的详情（补丁 diff）就别用原始输出盖掉它。
            if let Some(detail) = detail {
                if stats.detail.is_empty() {
                    stats.detail = detail;
                }
            }
            stats.progress = None;
            if self.timeline_enabled() && tool_event_base_name(name) == timeline::SEND_TOOL {
                self.note_cross_session_result(name, ok, output);
            }
            self.settle_tool_batch()?;
        } else if self.tool_call_mode == ToolCallDisplayMode::Full {
            self.release_transient_output()?;
            let display_name = self.display_tool_name(name);
            let stdout = &mut self.output;
            writeln!(
                stdout,
                "{} {} {}",
                t("result", "结果"),
                display_name,
                tool_result_status(status, elapsed)
            )?;
            write_tool_payload(stdout, t("output", "输出"), output)?;
            stdout.flush()?;
            self.tool_stats.remove(name);
        }
        Ok(())
    }

    /// 一个工具跑完了：这一批都齐了就收进时间线（或写摘要），还有兄弟在跑就
    /// 只刷新 live 区。
    ///
    /// 全屏／静态时间线下，收完立刻把转轮再挂回去：收进去的那一刻 live 区是
    /// 空的，等下一次模型请求开始（`reasoning.start`）才会重新出现——网络那一
    /// 秒里整条时间线闪没了又闪回来。
    pub(crate) fn settle_tool_batch(&mut self) -> Result<()> {
        if self.tool_stats.values().any(|stats| !stats.settled()) {
            // Siblings still running (parallel subagents): freeze this
            // tool's block in the live area; commit only when the whole
            // batch settles.
            return self.update_tool_summary_display();
        }
        self.finalize_tools_summary()?;
        // 清单写完 = 这一段忙完了。收段要在这儿做,不在 `timeline_push_tools`
        // 里面——这里才是「收完这批再重挂 live 区」的那个时序。
        if std::mem::take(&mut self.timeline_ends_after_tools) {
            self.cut_timeline()?;
        }
        if self.timeline_enabled() && self.wait_spinner.is_none() && self.live_summary {
            self.ensure_tool_waiting_phase()?;
        }
        Ok(())
    }

    pub fn write_tool_progress(&mut self, name: &str, message: &str) -> Result<()> {
        if let Some(phase) = message.strip_prefix("__tool_phase__") {
            if self.plain {
                let stdout = &mut self.output;
                writeln!(stdout, "{phase}")?;
                stdout.flush()?;
            } else if self.wait_spinner.is_some() {
                self.set_waiting_phase(phase.to_string());
                self.tick_spinner()?;
            } else {
                self.render_summary_line(phase, SummaryStyle::Tool)?;
            }
            return Ok(());
        }
        if let Some(json) = message.strip_prefix("__patch_preview__") {
            // 全屏：diff 是"编辑文件"这一步的详情，不是正文。直接打屏的话它
            // 既不在时间线里（点不开、收不起），也不在缓冲里（重开就没了）。
            if self.timeline_enabled() {
                // 抬头那一行补上 `+3 -1`:光有路径看不出这次编辑是顺手一改还是
                // 大手术。真 diff 只有这条侧信道有,所以在这儿补(用户 09-17)。
                if let Some((added, removed)) = preview_diff_stat(json) {
                    let subject = self.tool_stats_entry(name).subject.clone();
                    let stat = diff_stat_label(added, removed);
                    self.tool_stats_entry(name).peek = Some(match subject {
                        Some(subject) => {
                            format!("{subject}{}{stat}", super::timeline::PEEK_SEP)
                        }
                        None => stat,
                    });
                }
                if let Some(diff) = patch_preview_lines(json, super::timeline::detail_width()) {
                    self.tool_stats_entry(name).detail = diff;
                }
                return Ok(());
            }
            self.release_transient_output()?;
            let stdout = &mut self.output;
            if write_patch_result(stdout, json)? {
                stdout.flush()?;
            }
            return Ok(());
        }
        // 08-21 token-diet:todowrite 不再向模型回显整表,表格数据改由
        // progress 侧信道送达,载荷形状与旧工具输出一致。
        if let Some(json) = message.strip_prefix("__todo_table__") {
            self.release_transient_output()?;
            // 全屏：表是"这一步的结果"，得排到时间线收完之后。就地写的话，
            // 后面几步会跑到表底下去，看着像"先出了表再去干活"。
            if self.timeline_enabled() {
                let mut buffer: Vec<u8> = Vec::new();
                if write_todo_table(&mut buffer, json)? {
                    let text = String::from_utf8_lossy(&buffer).into_owned();
                    self.queue_after_timeline(super::timeline::indent_body(&text));
                }
                return Ok(());
            }
            let stdout = &mut self.output;
            if write_todo_table(stdout, json)? {
                stdout.flush()?;
            }
            return Ok(());
        }
        if self.plain {
            return Ok(());
        }
        if message == "__external_output__" {
            self.prepare_for_external_output()?;
            return Ok(());
        }
        if let Some(text) = message.strip_prefix("__subagent_detach__") {
            if self.captures_tools() {
                // Lands as the block's `↳` subject line, not the final `✓`
                // stats line — detach is a fact about the call, not a result.
                let stats = self.tool_stats_entry(name);
                stats.subject = Some(text.to_string());
                stats.detached = true;
                self.update_tool_summary_display()?;
            } else if self.tool_call_mode == ToolCallDisplayMode::Full {
                self.release_transient_output()?;
                let display_name = self.display_tool_name(name);
                let stdout = &mut self.output;
                writeln!(stdout, "{} {}: {text}", t("progress", "进度"), display_name)?;
                stdout.flush()?;
            }
            return Ok(());
        }
        if let Some(text) = message.strip_prefix(miyu_engine::tools::TOOL_SUMMARY_PREFIX) {
            if self.captures_tools() {
                self.tool_stats_entry(name).final_progress = Some(text.to_string());
                self.update_tool_summary_display()?;
            } else if self.tool_call_mode == ToolCallDisplayMode::Full {
                self.release_transient_output()?;
                let stdout = &mut self.output;
                for line in text.lines() {
                    writeln!(stdout, "{line}")?;
                }
                stdout.flush()?;
            }
            return Ok(());
        }
        if let Some(text) = message.strip_prefix("__subagent_metric__") {
            // `<给人看的那串>\t<数字>\t<人话>`。数字进会话累计的实时加数，
            // 人话进面板抬头。
            let mut parts = text.splitn(3, '\t');
            // 第一段是给人看的短标（`≈3.1K`）：时间线那一行挂它。
            let display = parts.next().unwrap_or_default().trim().to_string();
            if let Some(raw) = parts
                .next()
                .and_then(|value| value.trim().parse::<u64>().ok())
            {
                self.subagent_tokens.insert(name.to_string(), raw);
            }
            let text = parts.next().unwrap_or(text);
            if self.timeline_enabled() {
                self.subagent_stats(name, text, Some(display.as_str()));
            }
            // Full 档不打：这条一秒来好几次，打出来就是刷屏。跑完那次
            // `__subagent_stats__` 照旧会留一行。
            if self.captures_tools() {
                self.tool_stats_entry(name).final_progress = Some(text.to_string());
                self.update_tool_summary_display()?;
            }
            return Ok(());
        }
        if let Some(text) = message.strip_prefix("__subagent_stats__") {
            if self.timeline_enabled() {
                // 跑完那一次只有人话，短标沿用中途量报记下的那份。
                self.subagent_stats(name, text, None);
            }
            if self.captures_tools() {
                self.tool_stats_entry(name).final_progress = Some(text.to_string());
                self.update_tool_summary_display()?;
            } else if self.tool_call_mode == ToolCallDisplayMode::Full {
                self.release_transient_output()?;
                let display_name = self.display_tool_name(name);
                let stdout = &mut self.output;
                writeln!(stdout, "{} {}: {text}", t("progress", "进度"), display_name)?;
                stdout.flush()?;
            }
            return Ok(());
        }
        if let Some(text) = message.strip_prefix("__subagent_content__") {
            // 子代理开口说正文了。全屏：进它自己那块面板（并把前面那一段过程
            // 收成一行）；别的档次一直是直接丢的——这个前缀只有终端渲染器认。
            if self.timeline_enabled() {
                self.subagent_content(name, &normalize_stream_text(text));
            }
            return Ok(());
        }
        if let Some(text) = message.strip_prefix("__subagent_reasoning__") {
            let text = normalize_stream_text(text);
            // 全屏：子代理的思考进它自己的时间线，不往正文里挤。
            if self.timeline_enabled() {
                self.subagent_thought(name, &text);
                return Ok(());
            }
            if self.tool_call_mode == ToolCallDisplayMode::Full {
                if self.subagent_mode != Some(ChatStreamKind::Reasoning) {
                    self.stop_waiting()?;
                    self.clear_summary_lines()?;
                    self.end_active_stream_line()?;
                    let stdout = &mut self.output;
                    writeln!(stdout)?;
                    stdout.flush()?;
                }
                let stdout = &mut self.output;
                write_full_reasoning_chunk(stdout, &text)?;
                stdout.flush()?;
                self.subagent_mode = Some(ChatStreamKind::Reasoning);
            }
            return Ok(());
        }
        if let Some(tool) = message.strip_prefix("__subtool_preparing__") {
            // 内层正在流工具参数：面板里露一行「准备xx」。别的档次一直是丢的。
            if self.timeline_enabled() {
                self.subagent_tool_preparing(name, tool.trim());
            }
            return Ok(());
        }
        if let Some(json) = message.strip_prefix("__subtool_call__") {
            if let Ok(value) = serde_json::from_str::<Value>(json) {
                let tool_name = value
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                // 全屏：调用本身不单独占一步，等结果回来连着输出一起记——
                // 一次调用和它的结果是同一件事，分成两行只是把面板撑长。
                // 但要在这儿掐表（内层事件自己不带耗时），并在面板里露出
                // 「正在跑」那一行。
                if self.timeline_enabled() {
                    let display = value
                        .get("display")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .unwrap_or_else(|| self.display_tool_name(tool_name));
                    let args = value.get("args").and_then(Value::as_str).unwrap_or("");
                    self.subagent_tool_started(name, tool_name, &display, args);
                    return Ok(());
                }
                if self.tool_call_mode == ToolCallDisplayMode::Full {
                    // 命令由结果那一条整块画（命令 + 状态 + 输出），这儿先画一遍
                    // 就是画两遍。
                    if tool_name == "run_command" {
                        return Ok(());
                    }
                    let args = value.get("args").and_then(Value::as_str).unwrap_or("");
                    self.release_transient_output()?;
                    let display_name = self.display_tool_name(tool_name);
                    let stdout = &mut self.output;
                    writeln!(stdout, "{} {}", t("tool", "工具"), display_name)?;
                    write_tool_payload(stdout, t("args", "参数"), args)?;
                    stdout.flush()?;
                }
            }
            return Ok(());
        }
        if let Some(json) = message.strip_prefix("__subtool_result__") {
            if let Ok(value) = serde_json::from_str::<Value>(json) {
                let tool_name = value
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                let ok = value.get("ok").and_then(Value::as_bool).unwrap_or(true);
                if self.timeline_enabled() {
                    let display = self.display_tool_name(tool_name);
                    let args = value.get("args").and_then(Value::as_str).unwrap_or("");
                    let output = value.get("output").and_then(Value::as_str).unwrap_or("");
                    self.subagent_tool(name, tool_name, &display, args, ok, output);
                    return Ok(());
                }
                if self.tool_call_mode == ToolCallDisplayMode::Full {
                    let args = value.get("args").and_then(Value::as_str).unwrap_or("");
                    let output = value.get("output").and_then(Value::as_str).unwrap_or("");
                    let status = if ok { "ok" } else { "err" };
                    self.release_transient_output()?;
                    let display_name = self.display_tool_name(tool_name);
                    let stdout = &mut self.output;
                    if tool_name == "run_command" {
                        write_command_block_with_status(
                            stdout,
                            args,
                            if ok {
                                CommandStatus::Ok
                            } else {
                                CommandStatus::Error
                            },
                        )?;
                        write_command_result_blocks(stdout, output)?;
                        write_command_block_gap(stdout, true)?;
                    } else {
                        writeln!(stdout, "{} {} {status}", t("result", "结果"), display_name)?;
                        write_tool_payload(stdout, t("output", "输出"), output)?;
                    }
                    stdout.flush()?;
                }
            }
            return Ok(());
        }
        if is_silent_tool(name) {
            return Ok(());
        }
        // 认不出的**子代理标记**一律咽掉：下面那个兜底分支是给人话进度用的，
        // 机器标记掉进去就是原样打到屏幕上——`__subagent_brief__` 已经这么漏过
        // 一次（`display.tool_calls = full` 的终端用户看到
        // `进度 子代理: __subagent_brief__{"description":…}`）。它只在 Full 档发，
        // 而前台早就从工具参数建好那一行了（`write_tool_call` → `subagent_prompt`），
        // 这条是给没有参数的后台面板用的，终端这边跳过才对。
        //
        // 咽掉而不是逐个认领：标记的词汇表在 `tools::subagent::protocol::MARKERS`，
        // 将来加一个新的，最坏情况是面板少显示点东西，而不是把 JSON 甩到用户脸上。
        if miyu_engine::tools::is_subagent_marker(message) {
            return Ok(());
        }
        if self.captures_tools() {
            self.tool_stats_entry(name).progress = Some(message.to_string());
            self.update_tool_summary_display()?;
        } else if self.tool_call_mode == ToolCallDisplayMode::Full {
            self.release_transient_output()?;
            let display_name = self.display_tool_name(name);
            let stdout = &mut self.output;
            writeln!(
                stdout,
                "{} {}: {message}",
                t("progress", "进度"),
                display_name
            )?;
            stdout.flush()?;
        }
        Ok(())
    }

    pub(crate) fn update_tool_summary_display(&mut self) -> Result<()> {
        self.end_subagent_stream_line()?;
        if self.wait_spinner.is_some() {
            let (header, sub) = if self.timeline_enabled() {
                self.timeline_waiting()
            } else {
                self.tool_summary_live()
            };
            self.set_tool_waiting_phase(&header, sub.as_deref());
        } else {
            self.end_active_stream_line()?;
            self.finalize_reasoning_summary()?;
            self.ensure_tool_waiting_phase()?;
        }
        Ok(())
    }

    pub(crate) fn end_subagent_stream_line(&mut self) -> Result<()> {
        let was_reasoning = self.subagent_mode == Some(ChatStreamKind::Reasoning);
        if was_reasoning {
            execute!(self.output, ResetColor)?;
        }
        if self.subagent_mode.is_some() {
            writeln!(self.output)?;
            if was_reasoning {
                writeln!(self.output)?;
            }
            self.subagent_mode = None;
        }
        Ok(())
    }

    pub(crate) fn finalize_tools_summary(&mut self) -> Result<()> {
        // 全屏：工具跑完不写摘要，收进时间线当一步。真正的输出等这一段
        // 连续过程被切断时才落（`cut_timeline`）。静态版也走这条，只是收进去
        // 的那一步当场就落地。
        if self.timeline_enabled() && self.tool_call_mode != ToolCallDisplayMode::Hidden {
            return self.timeline_push_tools();
        }
        if self.tool_call_mode == ToolCallDisplayMode::Summary && !self.tool_stats.is_empty() {
            self.stop_waiting()?;
            execute!(self.output, ResetColor)?;
            let summary = self.tool_summary_text();
            if self.summary_line_active {
                self.clear_summary_lines()?;
                self.summary_line_active = false;
                self.summary_lines_active = 0;
            }
            // 展开的那份是每个工具各自的完整块（表头 + 主题 + 进度）——摘要那
            // 一行把它们压成了「工具×3 ok:3」，点开才看得到分别干了什么。
            let detail = if self.caps().expandable {
                self.ordered_tool_stats()
                    .into_iter()
                    .flat_map(|(name, stats)| self.tool_block_lines(name, stats, false))
                    .map(|line| style_summary_text(&line, SummaryStyle::Tool))
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            };
            let expanded =
                crate::render::blocks::expand_under(&summary, detail, SummaryStyle::Tool);
            let stdout = &mut self.output;
            crate::render::blocks::write_expandable(stdout, expanded, |writer| {
                write_activity_summary(writer, &summary, SummaryStyle::Tool)
                    .map_err(std::io::Error::other)
            })?;
            stdout.flush()?;
            self.tool_stats.clear();
            self.last_tool_summary.clear();
        }
        Ok(())
    }

    pub(crate) fn render_summary_line(&mut self, text: &str, style: SummaryStyle) -> Result<()> {
        self.stop_waiting()?;
        if !self.live_summary {
            return Ok(());
        }
        self.clear_summary_lines()?;
        let stdout = &mut self.output;
        let lines = transient_summary_lines(text, command_terminal_width());
        for (index, line) in lines.iter().enumerate() {
            if index > 0 {
                writeln!(stdout)?;
            }
            execute!(stdout, MoveToColumn(0))?;
            write!(stdout, "{}\x1b[K", style_summary_text(line, style))?;
        }
        stdout.flush()?;
        self.summary_line_active = true;
        self.summary_lines_active = lines.len().max(1) as u16;
        Ok(())
    }

    pub(crate) fn clear_summary_lines(&mut self) -> Result<()> {
        if !self.summary_line_active {
            return Ok(());
        }
        let stdout = &mut self.output;
        let lines = self.summary_lines_active.max(1);
        for index in 0..lines {
            if index > 0 {
                execute!(stdout, crossterm::cursor::MoveUp(1))?;
            }
            execute!(stdout, MoveToColumn(0), Clear(ClearType::CurrentLine))?;
        }
        stdout.flush()?;
        self.summary_line_active = false;
        self.summary_lines_active = 0;
        Ok(())
    }

    /// Gets or creates a tool's stats entry, stamping first-seen order so
    /// parallel blocks render in launch order rather than name order.
    pub(crate) fn tool_stats_entry(&mut self, name: &str) -> &mut ToolStats {
        self.tool_seq += 1;
        let seq = self.tool_seq;
        self.tool_stats
            .entry(name.to_string())
            .or_insert_with(|| ToolStats {
                seq,
                ..ToolStats::default()
            })
    }

    /// Tools in first-seen order (stable for direct test inserts with seq 0).
    pub(crate) fn ordered_tool_stats(&self) -> Vec<(&String, &ToolStats)> {
        let mut entries: Vec<_> = self.tool_stats.iter().collect();
        entries.sort_by_key(|(_, stats)| stats.seq);
        entries
    }

    pub(crate) fn tool_summary_text(&self) -> String {
        self.ordered_tool_stats()
            .into_iter()
            .map(|(name, stats)| self.tool_block_lines(name, stats, false).join("\n"))
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    /// Builds one tool's display block: a `~`-prefixed status header plus
    /// its own subject/progress lines. In `live` mode a still-running tool's
    /// header carries [`wait_spinner::BLOCK_MARKER`] so the spinner animates
    /// it, and a settled tool freezes into its final `✓` stats in place. The
    /// committed variant (`live == false`) prefers `final_progress` with `✓`.
    pub(crate) fn tool_block_lines(
        &self,
        name: &str,
        stats: &ToolStats,
        live: bool,
    ) -> Vec<String> {
        let display = self.display_tool_name(name);
        let mut header = tool_status_text(&display, stats, is_subagent_tool(name));
        if inline_tool_subject(name) {
            if let Some(subject) = &stats.subject {
                header.push_str(" · ");
                header.push_str(subject);
            }
        }
        let header = self.tool_summary_with_prefix(header);
        // In live mode a running block's detail lines are indented to sit
        // under its spinner glyph; a settled block drops the glyph and sits
        // flush, matching the committed layout.
        let running_live = live && !stats.settled();
        // Detail lines always sit two columns in, matching command blocks
        // (`$ …` / `  ↳ cmd` / `  │ output`) and avoiding the leftward jump
        // a block used to make when it settled.
        let detail_indent = "  ";
        let mut lines = Vec::new();
        if running_live {
            lines.push(format!("{}{header}", wait_spinner::BLOCK_MARKER));
        } else {
            lines.push(header);
        }
        if !inline_tool_subject(name) {
            if let Some(subject) = &stats.subject {
                // Subagent headers already carry the description — don't
                // repeat it as a subject line.
                if !lines[0].contains(subject.as_str()) {
                    lines.push(format!("{detail_indent}↳ {subject}"));
                }
            }
        }
        let (progress_text, is_final) = if live {
            if stats.settled() {
                (stats.final_progress.as_ref(), true)
            } else {
                (stats.progress.as_ref(), false)
            }
        } else if stats.final_progress.is_some() {
            (stats.final_progress.as_ref(), true)
        } else {
            (stats.progress.as_ref(), false)
        };
        let progress_prefix = if is_final { "✓" } else { "↳" };
        if let Some(message) = progress_text {
            for line in message.lines().filter(|line| !line.trim().is_empty()) {
                let line = if is_final {
                    clip_progress_line_preserving_spaces(line, 120)
                } else {
                    clip_progress_line(line, 120)
                };
                // 自带记号的行原样保留:失败清单是 `✗ …`,再套一个 `✓` 就成了
                // 「✓ ✗ 权限不足」。
                if line.starts_with('✗') {
                    lines.push(format!("{detail_indent}{line}"));
                } else {
                    lines.push(format!("{detail_indent}{progress_prefix} {line}"));
                }
            }
        }
        lines
    }

    pub(crate) fn tool_summary_header(&self) -> String {
        let parts = self
            .ordered_tool_stats()
            .into_iter()
            .map(|(name, stats)| {
                let display = self.display_tool_name(name);
                let mut header = tool_status_text(&display, stats, is_subagent_tool(name));
                if inline_tool_subject(name) {
                    if let Some(subject) = &stats.subject {
                        header.push_str(" · ");
                        header.push_str(subject);
                    }
                }
                header
            })
            .collect::<Vec<_>>()
            .join(", ");
        self.tool_summary_with_prefix(parts)
    }

    /// Live status for the wait spinner. A single tool keeps the classic
    /// one-line phase + progress sub-block. Multiple tools (e.g. parallel
    /// subagents) switch the spinner into block mode: the phase line is
    /// empty and every tool renders as its own block — running blocks carry
    /// their own animated glyph, settled blocks freeze into their final
    /// stats, and blocks are separated by blank lines:
    ///
    /// ```text
    /// ⠋ ~ 子代理·任务A×1 运行中 · 3s
    ///   ↳ 任务A进度
    ///
    ///   ~ 子代理·任务B×1 ok · 2s
    ///   ✓ 工具调用 1 次
    /// ```
    pub(crate) fn tool_summary_live(&self) -> (String, Option<String>) {
        if self.tool_stats.len() <= 1 {
            return (self.tool_summary_header(), self.tool_summary_progress());
        }
        let blocks = self
            .ordered_tool_stats()
            .into_iter()
            .map(|(name, stats)| self.tool_block_lines(name, stats, true).join("\n"))
            .collect::<Vec<_>>()
            .join("\n\n");
        (String::new(), Some(blocks))
    }

    pub(crate) fn tool_summary_with_prefix(&self, parts: String) -> String {
        if self.tool_stats.len() == 1
            && self
                .tool_stats
                .keys()
                .next()
                .is_some_and(|name| name == "run_command")
        {
            format!("$ {parts}")
        } else {
            format!("~ {parts}")
        }
    }

    pub(crate) fn tool_summary_progress(&self) -> Option<String> {
        for (name, stats) in self.ordered_tool_stats() {
            let mut lines = Vec::new();
            if !inline_tool_subject(name) {
                if let Some(subject) = &stats.subject {
                    // Skip subjects already shown in the header (subagent
                    // descriptions are part of the display name).
                    if !self.display_tool_name(name).contains(subject.as_str()) {
                        lines.push(format!("  ↳ {subject}"));
                    }
                }
            }
            if let Some(message) = &stats.progress {
                let progress = message
                    .lines()
                    .filter(|line| !line.trim().is_empty())
                    .map(|line| format!("  ↳ {}", clip_progress_line(line, 120)))
                    .collect::<Vec<_>>()
                    .join("\n");
                if !progress.is_empty() {
                    lines.push(progress);
                }
            }
            if !lines.is_empty() {
                return Some(lines.join("\n"));
            }
        }
        None
    }

    pub(crate) fn has_running_subagent_timer(&self) -> bool {
        self.tool_stats
            .iter()
            .any(|(name, stats)| is_subagent_tool(name) && stats.started_at.is_some())
    }

    pub(crate) fn finish_subagent_timer(&mut self, name: &str) -> Option<std::time::Duration> {
        if !is_subagent_tool(name) {
            return None;
        }
        let stats = self.tool_stats.get_mut(name)?;
        let elapsed = stats.started_at.take()?.elapsed();
        stats.elapsed = Some(elapsed);
        Some(elapsed)
    }

    pub(crate) fn display_tool_name<'a>(&self, name: &'a str) -> String {
        // Subagents keep their per-call description so parallel subagent
        // calls show as separate lines: "子代理·<描述>".
        // "task:" 是改名前的事件名,历史回放里还在。
        // 开发模式那条单列一类：「开发中」比「子代理」更说明它在干嘛——那一条
        // 是去写代码的，不是去查资料的。前台后台一个口径。
        if let Some(description) = name
            .strip_prefix("subagent:dev:")
            .or_else(|| name.strip_prefix("task:dev:"))
        {
            return format!("{}·{description}", t("dev", "开发中"));
        }
        if let Some(description) = name
            .strip_prefix("subagent:")
            .or_else(|| name.strip_prefix("task:"))
        {
            let base = if self.readable_tool_names {
                readable_tool_name("subagent")
            } else {
                "subagent".to_string()
            };
            return format!("{base}·{description}");
        }
        let name = tool_event_base_name(name);
        if self.readable_tool_names {
            readable_tool_name(name)
        } else {
            name.to_string()
        }
    }
}
