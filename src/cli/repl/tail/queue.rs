//! 活动区里的排队消息与流式片段。
//!
//! 用户在回合跑着时敲的内容先进队列，显示在活动区下方。片段（chunk）攒着批量
//! 刷（`flush_pending_chunks`）——每来一个 token 就重绘一次，终端跟不上。

use crate::cli::repl::tail::*;

impl LiveReplTail {
    pub(in crate::cli) fn enqueue(&mut self, prompt: QueuedPrompt) -> Result<()> {
        let output_cursor = self.output_cursor;
        self.suspend()?;
        self.append_queued(prompt);
        self.resume_at(output_cursor)
    }

    pub(in crate::cli) fn append_queued(&mut self, prompt: QueuedPrompt) {
        self.queued.push(prompt);
        self.queued.sort_by_key(|prompt| prompt.seq);
    }

    /// `at` 是这一片所在事件的时刻（没有就是 `None`）。挨着的同类片段并成一段，留第一片
    /// 的时刻：思考转正文的那一刻，就是这一段开头到的那一刻。
    pub(in crate::cli) fn queue_stream_chunk(
        &mut self,
        chunk: ChatStreamChunk,
        at: Option<Instant>,
    ) {
        if let Some((pending, _)) = self
            .pending_chunks
            .last_mut()
            .filter(|(pending, _)| pending.kind == chunk.kind)
        {
            pending.text.push_str(&chunk.text);
        } else {
            self.pending_chunks.push((chunk, at));
        }
    }

    /// 攒着的片段冲进渲染器，每一段按它自己到的那一刻（事件时钟，`event_clock.rs`），冲完
    /// 把时钟放回原样。
    pub(in crate::cli) fn flush_pending_chunks(
        &mut self,
        renderer: &mut render::StreamRenderer,
    ) -> Result<()> {
        let sticky = renderer.event_clock();
        for (chunk, at) in std::mem::take(&mut self.pending_chunks) {
            renderer.set_event_clock(at.or(sticky));
            renderer.write_chunk(chunk)?;
        }
        renderer.set_event_clock(sticky);
        Ok(())
    }

    pub(in crate::cli) fn discard_pending_chunks(&mut self) {
        self.pending_chunks.clear();
    }

    /// 提交回显与活动区重画放在同一个同步块里。
    ///
    /// 回显把屏幕顶满时,光标会被推到最后一行左端。以前这里只写回显,输入
    /// 框要等 daemon 接受回合之后才画回去,隐藏着的光标在左下角停一百多
    /// 毫秒——kitty 的 cursor_trail 不看光标隐不隐藏,照样把拖尾画到左下角
    /// 再飞回输入框(09-09 无头 kitty 逐帧截图坐实,pyte 回放看不见是因为
    /// 光标本体确实隐藏着)。现在回显写完立刻把活动区画回去,光标从不在
    /// 左下角落脚;写完后的输出光标按帧追踪器推算,同步块内零 ESC[6n。
    pub(in crate::cli) fn commit_submission(&mut self, submission: &LiveSubmission) -> Result<()> {
        let (cols, rows) = terminal::size().unwrap_or((80, 24));
        // 全屏：正文归缓冲。直接打 stdout 的话这句话进不了历史，回翻就
        // 找不到自己刚说了什么。
        if self.screen.is_some() {
            // 全屏下**斜杠命令不回显**：它不是一句话，是一次操作。inline 那边
            // 回显是因为命令结果直接打在下面、不回显就看不出这一行是谁触发的；
            // 全屏有菜单也有历史，回显只会在正文里堆一堆 `/models`。
            if matches!(
                parse_repl_input(submission.content.trim_start()),
                ReplInput::Slash(..)
            ) {
                self.editor.clear();
                let cursor = self.output_cursor;
                return self.resume_at(cursor);
            }
            // 发出去就回到底部：翻着历史打字然后回车，结果自己刚说的话看不见，
            // 是最容易让人以为「没发出去」的一种。
            if let Some(screen) = &mut self.screen {
                screen.follow_bottom();
            }
            let frame = committed_user_messages_frame(
                &[(submission.display_content.as_str(), self.editor.mode)],
                true,
                0,
                usize::from(cols),
            );
            // 这一轮从这儿开始：`/undo` 截回到这个标记处。
            let frame = format!("{}{frame}", miyu_hosts::render::blocks::TURN_START_MARKER);
            return self.apply_output_frame(frame.as_bytes());
        }
        self.suspend()?;
        // suspend 已把光标 MoveTo(output_cursor):列已知。
        let frame = committed_user_messages_frame(
            &[(submission.display_content.as_str(), self.editor.mode)],
            true,
            self.output_cursor.0,
            usize::from(cols),
        );
        let mut stdout = io::stdout();
        write!(stdout, "{frame}")?;
        stdout.flush()?;
        let output_cursor = cursor_after_frame(frame.as_bytes(), self.output_cursor, cols, rows);
        self.output_cursor = output_cursor;
        self.resume_at(output_cursor)
    }

    pub(in crate::cli) fn commit_empty_submission(&mut self) -> Result<()> {
        let mode = self.editor.mode;
        self.editor.clear();
        if self.screen.is_some() {
            // 全屏下空回车什么都不留。inline 那边回显一条空竖条是"按下去了"的
            // 反馈，反正会随正文滚走；全屏是一块固定的画布，敲几次就攒几段空
            // 气泡，看着像发出去了几条空消息。
            let cursor = self.output_cursor;
            return self.resume_at(cursor);
        }
        self.suspend()?;
        write_committed_user_messages(&[("", mode)], true)?;
        let output_cursor = cursor_position_or(self.output_cursor);
        self.output_cursor = output_cursor;
        self.resume_at(output_cursor)
    }

    /// Print a background-command wake reply into the scrollback while the
    /// REPL idles: dim header, then the assistant's report.
    pub(in crate::cli) fn show_background_report(
        &mut self,
        report: &BackgroundReport,
        preview_lines: usize,
    ) -> Result<()> {
        let fullscreen = render::blocks::enabled();
        // 全屏下**不让屏**。
        //
        // `suspend` 是把活动区擦掉、把终端让给外部输出用的；而这段汇报本来就该
        // 进缓冲。让一次屏的代价是活动区（输入框 + 后台状态行）整块擦掉再画
        // 回来——屏幕上就是闪一下（用户原话「状态行还是在闪动一次」）。
        if !fullscreen {
            self.suspend()?;
        }
        let mut stdout = io::stdout();
        let glyph = if fullscreen {
            render::timeline::glyph_notice()
        } else {
            "⚙"
        };
        let mut text = match miyu_core::state::parse_cross_session_message(&report.headline) {
            // 另一个会话里的 AI 发来的那条（09-23）：先画它那一块，再接这边的回话。
            Some(message) => {
                let mut block = Vec::new();
                render::timeline::write_cross_session_message(
                    &mut block,
                    &miyu_core::state::cross_session_headline(
                        &message.from_name,
                        &message.from_session,
                    ),
                    &message.body,
                    preview_lines,
                )?;
                let block = String::from_utf8_lossy(&block).replace('\n', "\r\n");
                if fullscreen {
                    // 这一块自带缩进与竖线，不能再过 `indent_body`。
                    self.apply_output_frame(block.as_bytes())?;
                    String::new()
                } else {
                    block
                }
            }
            // daemon 重启后接着跑的那一轮（09-24）：一行转圈箭头的提示，再接回话。
            None => match miyu_core::state::service_restart_attempt(&report.headline) {
                Some(attempt) => format!(
                    "\x1b[2m{} {}\x1b[0m\r\n\r\n",
                    if fullscreen {
                        render::timeline::glyph_restart()
                    } else {
                        "↻"
                    },
                    miyu_core::state::service_restart_headline(attempt)
                ),
                // 后台任务报告：全屏下铃铛那一行点得开，看唤醒附的结果段（09-26）。
                None if fullscreen => {
                    let mut block = Vec::new();
                    render::timeline::write_job_report_notice(
                        &mut block,
                        &job_wake_headline(&report.headline),
                        report.job_report.as_ref(),
                    )?;
                    self.apply_output_frame(&block)?;
                    String::new()
                }
                None => format!(
                    "\x1b[2m{glyph} {}\x1b[0m\r\n\r\n",
                    job_wake_headline(&report.headline)
                ),
            },
        };
        for line in report.reply.lines() {
            text.push_str(&render::render_markdown_line(line));
            text.push_str("\r\n");
        }
        text.push_str("\r\n");
        // 回复末尾那行 `✻`（09-26），和实时收尾、回放一个样子。
        if let Some(end) = &report.turn_end {
            text.push_str(&render::timeline::turn_end_styled(
                &render::timeline::TurnEnd {
                    turn_id: &report.turn_id,
                    model: end.model.as_deref(),
                    elapsed: end.elapsed,
                    finished_at: end.finished_at,
                    interrupted: end.interrupted,
                },
            ));
            text.push_str("\r\n\r\n");
        }
        // 全屏：这段也是正文，一样缩进、一样自己折行。不然后台任务的汇报
        // 贴着第 0 列，整屏只有它不在装订边上。
        if fullscreen {
            let text = render::timeline::indent_body(&text);
            // 走缓冲：直接打屏的话下一帧按缓冲重画就把它抹了。
            self.apply_output_frame(text.as_bytes())?;
            let cursor = self.output_cursor;
            return self.resume_at_own(cursor);
        }
        queue!(stdout, Print(text))?;
        stdout.flush()?;
        self.output_cursor = cursor_position_or(self.output_cursor);
        let output_cursor = self.output_cursor;
        self.resume_at(output_cursor)
    }

    /// 后台任务完成的那一行：暗色铃铛 + 抬头，末尾留一个空行。
    ///
    /// 位置是**收尾行之后**：这一轮先收成 `Worked for …`，再报「这件事完成了」，
    /// 然后空一行接着说（用户 09-21 看过实际效果后定的版式）。和 REPL 空闲时
    /// 那条报告（`show_background_report`）长相一致，只是不带正文。全屏下点得开，
    /// 看唤醒附的结果段（09-26）。
    pub(in crate::cli) fn show_job_wake_notice(
        &mut self,
        headline: &str,
        report: Option<&miyu_core::state::JobReportResult>,
    ) -> Result<()> {
        if !render::blocks::enabled() {
            return self.show_notice_line("⚙", headline);
        }
        let mut block = Vec::new();
        render::timeline::write_job_report_notice(&mut block, headline, report)?;
        // 同 `show_notice_line`：全屏下 `apply_output_frame` 自己就把画面接回去了。
        self.apply_output_frame(&block)
    }

    /// daemon 重启打断了上一轮、新 daemon 替这个会话接着跑（09-24）：一行暗色提示，
    /// 和后台任务完成那行同一个样式，只是图标换成转圈的箭头。
    pub(in crate::cli) fn show_restart_notice(&mut self, attempt: u32) -> Result<()> {
        let glyph = if render::blocks::enabled() {
            render::timeline::glyph_restart()
        } else {
            "↻"
        };
        self.show_notice_line(glyph, &miyu_core::state::service_restart_headline(attempt))
    }

    fn show_notice_line(&mut self, glyph: &str, headline: &str) -> Result<()> {
        let fullscreen = render::blocks::enabled();
        let text = format!("\x1b[2m{glyph} {headline}\x1b[0m\r\n\r\n");
        if fullscreen {
            let text = render::timeline::indent_body(&text);
            // 全屏下 `apply_output_frame` 自己就把画面接回去了，再跟一次
            // `resume_at_own` 是白多一次整屏重画。这一行是在**回合流着的时候**
            // 打的（后台任务完成），那一下闪看得见——走查 item13「流式输出期间
            // 不整屏擦」就是被它顶红的。
            return self.apply_output_frame(text.as_bytes());
        }
        self.suspend()?;
        let mut stdout = io::stdout();
        queue!(stdout, Print(text))?;
        stdout.flush()?;
        self.output_cursor = cursor_position_or(self.output_cursor);
        let output_cursor = self.output_cursor;
        self.resume_at(output_cursor)
    }

    /// Remove queued bubbles without committing them as sent messages —
    /// the daemon dropped these prompts (explicit cancel), they were never
    /// answered and never entered the conversation.
    pub(in crate::cli) fn drop_queued(&mut self, prompt_ids: &[String]) -> Result<()> {
        let ids = prompt_ids.iter().collect::<std::collections::HashSet<_>>();
        if !self
            .queued
            .iter()
            .any(|prompt| ids.contains(&prompt.prompt_id))
        {
            return Ok(());
        }
        let output_cursor = self.output_cursor;
        self.suspend()?;
        self.queued
            .retain(|prompt| !ids.contains(&prompt.prompt_id));
        self.resume_at(output_cursor)
    }

    /// 这批排队消息里，哪几条是 daemon 合成的通知（后台任务报告、跨会话消息）。
    /// 把它们从队列里摘走并返回——它们不是谁敲的话，要走时间线上的通知那条路，
    /// 而不是画成用户气泡、顺带把这一轮收尾（用户 09-21）。
    pub(in crate::cli) fn take_queued_notices(
        &mut self,
        prompt_ids: &[String],
    ) -> Vec<QueuedNotice> {
        let ids = prompt_ids.iter().collect::<std::collections::HashSet<_>>();
        let mut notices = Vec::new();
        self.queued.retain(|prompt| {
            if !ids.contains(&prompt.prompt_id) {
                return true;
            }
            let display = &prompt.display_content;
            if let Some(message) = miyu_core::state::parse_cross_session_message(display) {
                notices.push(QueuedNotice::CrossSession(message));
                return false;
            }
            if let Some(attempt) = miyu_core::state::service_restart_attempt(display) {
                notices.push(QueuedNotice::Restart(attempt));
                return false;
            }
            if is_job_wake_headline(display) {
                // 从库里重载的排队消息带着原文，当场拆得出结果段；事件送来的只有给人看的
                // 那一行，调用方再按 `prompt_id` 去库里补（`fill_job_reports`）。
                notices.push(QueuedNotice::JobReport {
                    prompt_id: prompt.prompt_id.clone(),
                    headline: job_wake_headline(display),
                    report: miyu_core::state::job_report_result(&prompt.content),
                });
                return false;
            }
            true
        });
        notices
    }

    /// 把一条通知落进正文：后台任务报告是一行抬头，跨会话消息是抬头加正文预览。
    pub(in crate::cli) fn show_queued_notice(
        &mut self,
        notice: &QueuedNotice,
        preview_lines: usize,
    ) -> Result<()> {
        match notice {
            QueuedNotice::JobReport {
                headline, report, ..
            } => self.show_job_wake_notice(headline, report.as_ref()),
            QueuedNotice::CrossSession(message) => {
                self.show_cross_session_message(message, preview_lines)
            }
            QueuedNotice::Restart(attempt) => self.show_restart_notice(*attempt),
        }
    }

    /// 另一个会话里的 AI 发来的那条（09-23）：铃铛 +「从 xxx 收到消息」，底下
    /// 竖线串着正文，先露 `preview_lines` 行，全屏下点开看全文。
    pub(in crate::cli) fn show_cross_session_message(
        &mut self,
        message: &miyu_core::state::CrossSessionMessage,
        preview_lines: usize,
    ) -> Result<()> {
        let mut frame = Vec::new();
        render::timeline::write_cross_session_message(
            &mut frame,
            &miyu_core::state::cross_session_headline(&message.from_name, &message.from_session),
            &message.body,
            preview_lines,
        )?;
        self.apply_output_frame(&frame)
    }

    /// 挂到子会话正在跑的第一轮上：开头那句是主会话派的任务，不是谁敲的话。
    pub(in crate::cli) fn show_parent_task(
        &mut self,
        body: &str,
        preview_lines: usize,
    ) -> Result<()> {
        let mut frame = Vec::new();
        crate::cli::history_replay::write_parent_task(&mut frame, body, preview_lines)?;
        self.apply_output_frame(&frame)
    }

    /// 这批里还有要画成气泡的吗。没有的话就别为它收尾时间线。
    pub(in crate::cli) fn has_queued(&self, prompt_ids: &[String]) -> bool {
        let ids = prompt_ids.iter().collect::<std::collections::HashSet<_>>();
        self.queued
            .iter()
            .any(|prompt| ids.contains(&prompt.prompt_id))
    }

    pub(in crate::cli) fn consume_queued(
        &mut self,
        prompt_ids: &[String],
        mode: PersonaLane,
    ) -> Result<()> {
        let ids = prompt_ids.iter().collect::<std::collections::HashSet<_>>();
        // 全屏这条路要先把文本拷出来再动队列——借着 `self.queued` 的切片
        // 和随后的 `retain` 不能同时存在。
        if self.screen.is_some() {
            let (cols, _) = terminal::size().unwrap_or((80, 24));
            let owned = self
                .queued
                .iter()
                .filter(|prompt| ids.contains(&prompt.prompt_id))
                .map(|prompt| prompt.display_content.clone())
                .collect::<Vec<_>>();
            let borrowed = owned
                .iter()
                .map(|text| (text.as_str(), mode))
                .collect::<Vec<_>>();
            let frame = committed_user_messages_frame(&borrowed, true, 0, usize::from(cols));
            self.queued
                .retain(|prompt| !ids.contains(&prompt.prompt_id));
            return self.apply_output_frame(frame.as_bytes());
        }
        self.suspend()?;
        let consumed = self
            .queued
            .iter()
            .filter(|prompt| ids.contains(&prompt.prompt_id))
            .map(|prompt| (prompt.display_content.as_str(), mode))
            .collect::<Vec<_>>();
        write_committed_user_messages(&consumed, true)?;
        self.queued
            .retain(|prompt| !ids.contains(&prompt.prompt_id));
        let output_cursor = cursor_position_or(self.output_cursor);
        self.output_cursor = output_cursor;
        self.resume_at(output_cursor)
    }

    pub(in crate::cli) fn reload_queue(&mut self, state: &StateStore) -> Result<()> {
        let output_cursor = self.output_cursor;
        self.suspend()?;
        self.queued = state.load_queued_prompts()?;
        self.resume_at(output_cursor)
    }
}

/// 回合里排进去的 `/compact` 在排队区的那一行（09-25）。它不是用户说的话：压缩一开始、或者
/// 这一轮结束就撤，不落成气泡（回放时也没有它）。
pub(in crate::cli) const QUEUED_COMPACT_ID: &str = "queued-compact";

pub(in crate::cli) fn queued_compact_marker() -> QueuedPrompt {
    QueuedPrompt {
        prompt_id: QUEUED_COMPACT_ID.to_string(),
        // 排在所有真消息后面：它在检查点上和插话一起被取走，谁先谁后看不出来，放最后最省心。
        seq: i64::MAX,
        content: "/compact".to_string(),
        display_content: "/compact".to_string(),
        attachments: Vec::new(),
        uploaded_attachments: Vec::new(),
        submitted_at: String::new(),
    }
}

impl LiveReplTail {
    /// 撤掉排队区里的 `/compact`（见 [`QUEUED_COMPACT_ID`]）。
    pub(in crate::cli) fn drop_compact_marker(&mut self) -> Result<()> {
        synchronized_terminal_update(CursorAfterUpdate::Preserve, || {
            self.drop_queued(&[QUEUED_COMPACT_ID.to_string()])
        })
    }
}

/// 排队消息里 daemon 合成的那几种（判据见 `jobs::is_daemon_notice`）。
pub(in crate::cli) enum QueuedNotice {
    /// 后台任务报告：一行抬头，全屏下点开是唤醒附的结果段（09-26）。
    JobReport {
        prompt_id: String,
        headline: String,
        report: Option<miyu_core::state::JobReportResult>,
    },
    /// 另一个会话里的 AI 发来的跨会话消息（09-23）。
    CrossSession(miyu_core::state::CrossSessionMessage),
    /// daemon 重启后的续跑消息（09-24），带第几次。
    Restart(u32),
}

/// 事件送来的后台任务报告只有给人看的那一行：结果段按 `prompt_id` 去库里补。补不上（库打不开、
/// 老 daemon）就还是点不开的那一行。
pub(in crate::cli) fn fill_job_reports(paths: &MiyuPaths, notices: &mut [QueuedNotice]) {
    let missing = notices
        .iter()
        .any(|notice| matches!(notice, QueuedNotice::JobReport { report: None, .. }));
    if !missing || !render::blocks::enabled() {
        return;
    }
    let Ok(store) = StateStore::new(paths) else {
        return;
    };
    for notice in notices {
        if let QueuedNotice::JobReport {
            prompt_id,
            report: report @ None,
            ..
        } = notice
        {
            *report = store.queued_job_report(prompt_id).ok().flatten();
        }
    }
}
