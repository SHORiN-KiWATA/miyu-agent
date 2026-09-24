//! 提问那一步：进时间线，以及把问答写进正文。
//!
//! 从 `timeline.rs` 搬来（09-16 拆分，那份超过了文件规模基线）。只搬不改。

use super::*;

impl StreamRenderer {
    /// 提问也算一步。
    ///
    /// 问用户是这一轮真真切切做过的一件事，时间线里不该没有它——尤其是被取消
    /// 的那次：正文里什么都不留（那是对的），时间线里再不留就彻底查无此事了。
    pub fn timeline_push_question(
        &mut self,
        request: &miyu_base::question::QuestionRequest,
        response: &miyu_base::question::QuestionResponse,
    ) -> anyhow::Result<()> {
        use miyu_base::question::QuestionResponse;
        if !self.timeline_enabled() {
            return Ok(());
        }
        let answered = matches!(response, QuestionResponse::Answered(_));
        // 面板开着的那段时间也算进这一段过程里：人在那儿看题、想答案，那就是
        // 这一轮真正花掉的时间。不算的话收缩行会写成光秃秃的 `1 tool`。
        let waited = self
            .preparing_question_started_at
            .map(|at| at.elapsed())
            .unwrap_or_default();
        self.timeline.note_start_since(waited);
        let headline = request
            .questions
            .first()
            .map(|prompt| prompt.header.trim().to_string())
            .filter(|header| !header.is_empty())
            .unwrap_or_else(|| t("question", "提问").to_string());
        let status = if answered {
            t("answered", "已回答")
        } else {
            t("cancelled", "已取消")
        };
        let label = format!(
            "{} · {status}{PEEK_SEP}{headline}",
            t("Ask the user", "询问用户")
        );
        let body = request
            .questions
            .iter()
            .flat_map(|prompt| {
                let mut lines = wrap_detail(prompt.question.trim());
                lines.push(String::new());
                lines
            })
            .collect::<Vec<_>>();
        self.timeline.counts.record_tool("ask_question", !answered);
        let glyph = tool_glyph("ask_question");
        // 静态版：面板退场时不留它自己那块「已回答」（那块带自己的竖条，落在
        // 抬头**上面**，和时间线是两套东西——用户实测截图）。一问一答改成这一步
        // 的正文，从连线穿过去，和别的步一个样子：
        //
        // ```text
        //    询问用户 · 已回答 · 今晚的打算
        //   │
        //   │ 已回答 3 个问题
        //   │ 今晚的打算：只是测工具（推荐）
        //   │
        // ```
        let body = if self.caps().detail_inline() {
            match response {
                QuestionResponse::Answered(answers) => {
                    let (heading, answered) = question_answer_text(request, answers);
                    let mut lines = vec![String::new(), format!("\x1b[2m{heading}\x1b[0m")];
                    for line in answered {
                        lines.extend(
                            wrap_detail(&line)
                                .into_iter()
                                .map(|piece| format!("\x1b[2m{piece}\x1b[0m")),
                        );
                    }
                    // 收尾不再空一行：下一步之前本来就有一根连线，两根叠着就是
                    // 两行 `│`。
                    lines
                }
                _ => Vec::new(),
            }
        } else {
            body
        };
        let mut step = Step::new(
            if answered {
                step_line(glyph, &label)
            } else {
                step_line_failed(glyph, &label)
            },
            body,
            None,
        );
        // 问用户也算这一轮做过的一件事——`timeline.tools` 上面刚 +1 过。
        step.kind = StepKind::Tool;
        self.timeline.steps.push(step);
        self.settle_new_steps()
    }

    /// 把一次提问与它的答案写进正文。
    ///
    /// 照搬 inline 那份的样子：一条暗竖条 + 「已回答 N 个问题」+ 每题一行
    /// 「标题：答案」。一问一答各占一行的写法（`? …` / `↳ …`）在屏幕上散成
    /// 一片，而这份本来就是给"扫一眼当时选了什么"用的。
    pub fn write_question_exchange(
        &mut self,
        request: &miyu_base::question::QuestionRequest,
        response: &miyu_base::question::QuestionResponse,
    ) -> anyhow::Result<()> {
        use miyu_base::question::QuestionResponse;
        use std::io::Write as _;
        // 只有全屏需要：面板是盖上去的，退场就没了。普通终端里提问面板自己
        // 会把「已回答」那几行留在原地，再写一遍就是两份。
        if !self.caps().expandable {
            return Ok(());
        }
        // 取消/关闭没产生任何**内容**：它只是"这一下没成"。往正文里逐题写一遍
        // 「已取消」，等于把一次误触变成永久的一屏垃圾。
        let QuestionResponse::Answered(answers) = response else {
            return Ok(());
        };
        self.stop_waiting()?;
        let indent = indent();
        let bar = "\x1b[2m\x1b[90m┃\x1b[0m";
        let width = crate::render::command_terminal_width()
            .saturating_sub(indent.len() + 3)
            .max(20);
        let (heading, answered) = question_answer_text(request, answers);
        let stdout = &mut self.output;
        writeln!(stdout, "{indent}{bar} \x1b[2m\x1b[90m{heading}\x1b[0m")?;
        for line in answered {
            writeln!(
                stdout,
                "{indent}{bar} \x1b[2m\x1b[90m{}\x1b[0m",
                crate::render::clip_to_display_width(&line, width)
            )?;
        }
        writeln!(stdout)?;
        stdout.flush()?;
        Ok(())
    }
}
