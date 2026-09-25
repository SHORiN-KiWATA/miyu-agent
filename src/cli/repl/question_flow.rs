//! `question.requested` 的终端侧处理：弹面板、把答案回发给 daemon。
//!
//! 两条泵共用一份（09-20）。原来只有自己起的那一轮（`remote::one_shot`）处理
//! 它；daemon 自己开的轮（目标续轮、后台任务唤醒）走 `repl::wake` 的另一张
//! 手写分发表，那张表里没有这个分支，事件落进 `_ => {}`——面板不弹、没人回答，
//! 那一步就停在「准备问题」上，直到回合循环那 30 分钟的兜底超时才收场
//! （用户 09-20：`/goal 试试用问问题工具随意问我一个问题` 永远卡住）。
//!
//! `ipc_events.rs` 顶上记着同款教训：解码表抄两份，谁漏抄一个变体，那条路径
//! 就少一个功能。这次把**处理**也收成一份，第三条泵接上来只要调这个函数。
//!
//! 一道题不一定在这块面板上了结（09-24）：
//! - 挂上来补整轮时，补到的题可能早答完了——daemon 把结果附在事件上
//!   （`settled`），这里照结果画出一问一答，不弹面板；
//! - 同一个会话开着两个终端（或网页）时，别处先答了、回合在别处结束了——
//!   面板开着时往前看一眼事件流就知道，面板自己收场，不回发任何命令。

use crate::cli::repl::tail::*;
use crate::cli::*;
use miyu_base::question::{QuestionRequest, QuestionResponse};

/// 弹出提问面板，按结果回发 `AnswerQuestion` / `CloseQuestion` / `Cancel`。
///
/// `live` 为 `None` = 没有活动的 REPL 尾巴（一次性客户端）：面板照弹，只是不
/// 需要挂起/恢复那一套。
pub(in crate::cli) async fn handle_question_requested(
    paths: &MiyuPaths,
    config: &AppConfig,
    mut live: Option<&mut LiveReplTail>,
    renderer: &mut render::StreamRenderer,
    data: &serde_json::Value,
    run_id: &str,
    // 这一轮的事件流。面板开着时泵是停着的，靠它往前看一眼别处有没有了结这道题。
    frames: &mut ipc::FrameReader,
    // 在 herdr 里跑时这一轮的守卫。她反问 = 「卡住等人」，侧栏把整条 tab /
    // workspace 标红；答完报回 `working`。两条泵都要，所以放在这里而不是各自
    // 的分发表里（09-20：one_shot 那边只报了 blocked 没报 resumed，wake 那边
    // 两个都没报）。
    herdr_turn: Option<&crate::cli::repl::herdr::TurnGuard>,
) -> Result<()> {
    let request = QuestionRequest {
        questions: serde_json::from_value(data.get("questions").cloned().unwrap_or_default())?,
    };
    // 补发来的、早已了结的题：照当时的结果画出来就完了。弹面板等于让人再答
    // 一遍一道答不了的题，Esc 关掉还会回发「取消这一轮」把正跑着的回合掐掉
    // （用户 09-24：答完退出 TUI 再进同一个会话，又进了同一个提问界面）。
    if let Some(settled) = settled_outcome(data) {
        // 收掉「准备问题」那一行：弹面板的路由它收，这里没有面板也得收。
        renderer.prepare_for_panel()?;
        record_exchange(renderer, &request, &settled)?;
        if matches!(settled, QuestionResponse::Answered(_)) {
            renderer.start_waiting()?;
        }
        if let Some(live) = live.as_deref_mut() {
            live.apply_renderer_frame(renderer)?;
        }
        return Ok(());
    }
    let question_id = ipc_text(data, "question_id").to_string();
    // 全屏：面板开在活动区的位置上，回合照流（会话项目第 3 段，B4）。行内没有活动区
    // 可让，照旧自己跑一个面板。
    if let Some(live) = live.as_deref_mut().filter(|live| live.screen.is_some()) {
        return open_question_layer(
            paths,
            config,
            live,
            renderer,
            request,
            question_id,
            run_id,
            herdr_turn,
        )
        .await;
    }
    // 报回 `working` 走 RAII：这个函数有好几条出口（答完、关掉、取消、各种
    // `?`），只在成功路径上报的话，答完侧栏还一直红着——herdr 那一项就是这么
    // 栽的（九条出口只报了一条）。
    let _resume_on_exit = HerdrBlocked::begin(
        herdr_turn,
        data.get("questions")
            .and_then(|questions| questions.get(0))
            .and_then(|question| question.get("question"))
            .and_then(serde_json::Value::as_str),
    );
    // 只让屏、不切线：这一步得等答案到手才补得进去。
    renderer.prepare_for_panel()?;
    if let Some(live) = live.as_deref_mut() {
        live.apply_renderer_frame(renderer)?;
        synchronized_terminal_update(CursorAfterUpdate::Hidden, || live.suspend())?;
    }
    notify_if_unfocused(
        &config,
        live.as_deref().map(|live| live.editor.focused),
        t("Miyu is waiting on you", "Miyu 在等你回答"),
        // 问题正文同样不外泄，理由同上。
        t("waiting for you", "正在等待处理"),
        miyu_base::notify::NotifySound::Question,
    );
    // A panel that cannot be shown is not a reason to abort the
    // turn: fall through to the same path a closed panel takes, so
    // the daemon gets an answer instead of the run dying on an
    // error the user cannot act on. The direct-mode handler has
    // always done this; this branch used to propagate instead.
    let asked = {
        let mut scroll = |delta: isize, panel_rows: u16| {
            if let Some(live) = live.as_deref_mut() {
                if let Some(screen) = live.screen.as_mut() {
                    let _ = screen.scroll_question_body(delta, panel_rows);
                }
            }
        };
        let mut watch = || settled_elsewhere(frames, &question_id, run_id);
        // 详情就地印的面自己把一问一答写成那一步的正文，面板退场别留东西。
        let leave_summary = !renderer.caps().detail_inline();
        crate::question_tui::ask_watched(
            &request,
            Some(&mut scroll),
            Some(&mut watch),
            leave_summary,
        )
        .unwrap_or_else(|err| {
            crate::question_tui::Asked::Here(QuestionResponse::Unavailable(err.to_string()))
        })
    };
    match asked {
        crate::question_tui::Asked::Here(asked) => {
            record_exchange(renderer, &request, &asked)?;
            reply(paths, renderer, asked, question_id, run_id).await?;
        }
        // 别处了结的：daemon 早就有结果了，这边只把它画出来，一条命令都不回发
        // ——尤其不能把「回合在别处结束了」当成这边的取消再发一遍。
        crate::question_tui::Asked::Elsewhere(settled) => {
            record_exchange(renderer, &request, &settled)?;
            if matches!(settled, QuestionResponse::Answered(_)) {
                renderer.start_waiting()?;
            }
        }
    }
    if let Some(live) = live.as_deref_mut() {
        live.external_output_active = false;
        live.output_cursor = cursor_position_or(live.output_cursor);
        live.resume_at(live.output_cursor)?;
    }
    Ok(())
}

/// 回合里开在活动区位置上的提问面板（会话项目第 3 段，B4）：面板开着正文照流，按键归它。
/// 画法和按键都是 `question_tui::QuestionPanel` 那一份，自己跑终端的面板也用它。
pub(in crate::cli) struct QuestionLayer {
    panel: crate::question_tui::QuestionPanel,
    question_id: String,
    /// 上一帧光标该在哪（正在输入自定义答案时）。
    cursor: Option<(usize, usize)>,
}

impl QuestionLayer {
    /// 面板高度：内容要几行就几行，最多 `QuestionPanel::max_rows`。
    pub(in crate::cli) fn desired_rows(&self) -> u16 {
        let width = crate::cli::terminal_cols().saturating_sub(3).max(1);
        u16::try_from(self.panel.rows_needed(width))
            .unwrap_or(u16::MAX)
            .clamp(1, crate::question_tui::QuestionPanel::max_rows())
    }

    /// 这一帧的行。`width` 是扣掉行首竖条之后的宽，面板自己再留一列边。
    pub(in crate::cli) fn content(&mut self, width: usize, rows: u16) -> Vec<String> {
        self.panel.expire_cancel();
        let view = self
            .panel
            .view(width.saturating_sub(1).max(1), usize::from(rows));
        self.cursor = view.cursor;
        view.lines
    }

    pub(in crate::cli) fn on_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> Option<QuestionResponse> {
        self.panel
            .on_key(KeyEvent::new(code, modifiers))
            .unwrap_or_else(|error| Some(QuestionResponse::Unavailable(error.to_string())))
    }

    pub(in crate::cli) fn paste(&mut self, text: &str) {
        self.panel.on_paste(text);
    }

    /// 光标在第几行、第几列（列从行首竖条算起）。
    pub(in crate::cli) fn cursor(&self) -> Option<(usize, usize)> {
        self.cursor
    }

    /// 行首那根竖条，和自己跑的提问面板一个样子。
    pub(in crate::cli) fn bar() -> String {
        format!("{} ", crate::question_tui::QUESTION_BAR)
    }
}

/// 全屏下开提问面板：收掉「准备问题」那一行、侧栏标红、没焦点就提醒，然后面板开在活动区
/// 的位置上，回合接着收事件。
#[allow(clippy::too_many_arguments)]
async fn open_question_layer(
    paths: &MiyuPaths,
    config: &AppConfig,
    live: &mut LiveReplTail,
    renderer: &mut render::StreamRenderer,
    request: QuestionRequest,
    question_id: String,
    run_id: &str,
    herdr_turn: Option<&crate::cli::repl::herdr::TurnGuard>,
) -> Result<()> {
    renderer.prepare_for_panel()?;
    live.apply_renderer_frame(renderer)?;
    let panel = match crate::question_tui::QuestionPanel::new(request.clone()) {
        Ok(panel) => panel,
        // 画不出来不是掐掉这一轮的理由：和面板关掉同一条路，daemon 拿到一个结果。
        Err(error) => {
            let asked = QuestionResponse::Unavailable(error.to_string());
            record_exchange(renderer, &request, &asked)?;
            reply(paths, renderer, asked, question_id, run_id).await?;
            return live.apply_renderer_frame(renderer);
        }
    };
    if let Some(guard) = herdr_turn {
        guard.blocked(
            request
                .questions
                .first()
                .map(|question| question.question.as_str()),
        );
    }
    notify_if_unfocused(
        config,
        Some(live.editor.focused),
        t("Miyu is waiting on you", "Miyu 在等你回答"),
        // 问题正文不外泄。
        t("waiting for you", "正在等待处理"),
        miyu_base::notify::NotifySound::Question,
    );
    live.open_turn_panel(crate::cli::repl::midturn_panel::TurnPanel::Question(
        QuestionLayer {
            panel,
            question_id,
            cursor: None,
        },
    ))
}

/// 在活动区的面板上答了 / 关了：记进这一步，回发给 daemon。
pub(in crate::cli) async fn answer_question_layer(
    paths: &MiyuPaths,
    live: &mut LiveReplTail,
    renderer: &mut render::StreamRenderer,
    run_id: &str,
    herdr_turn: Option<&crate::cli::repl::herdr::TurnGuard>,
    layer: QuestionLayer,
    asked: QuestionResponse,
) -> Result<()> {
    if let Some(guard) = herdr_turn {
        guard.resumed();
    }
    record_exchange(renderer, layer.panel.request(), &asked)?;
    reply(paths, renderer, asked, layer.question_id, run_id).await?;
    live.apply_renderer_frame(renderer)
}

/// 回合循环收到 `question.answered` / `question.closed`：开着的正是这道题，就说明别处先
/// 了结了（另一个终端、网页）。面板收掉，照那边的结果画出来，一条命令都不回发。
pub(in crate::cli) fn settle_question_layer(
    live: &mut LiveReplTail,
    renderer: &mut render::StreamRenderer,
    kind: &str,
    data: &serde_json::Value,
    herdr_turn: Option<&crate::cli::repl::herdr::TurnGuard>,
) -> Result<()> {
    let open = matches!(
        &live.turn_panel,
        Some(crate::cli::repl::midturn_panel::TurnPanel::Question(layer))
            if layer.question_id == ipc_text(data, "question_id")
    );
    if !open {
        return Ok(());
    }
    let outcome = match kind {
        "question.answered" => {
            match serde_json::from_value(data.get("answers").cloned().unwrap_or_default()) {
                Ok(answers) => QuestionResponse::Answered(answers),
                Err(_) => return Ok(()),
            }
        }
        _ => QuestionResponse::Closed,
    };
    close_question_layer(live, renderer, outcome, herdr_turn)
}

/// 回合结束了面板还开着（别处按了停、daemon 那头超时）：没人再等这个答案，收掉，记成
/// 取消。
pub(in crate::cli) fn abandon_question_layer(
    live: &mut LiveReplTail,
    renderer: &mut render::StreamRenderer,
    herdr_turn: Option<&crate::cli::repl::herdr::TurnGuard>,
) -> Result<()> {
    if !matches!(
        live.turn_panel,
        Some(crate::cli::repl::midturn_panel::TurnPanel::Question(_))
    ) {
        return Ok(());
    }
    close_question_layer(live, renderer, QuestionResponse::Cancelled, herdr_turn)
}

fn close_question_layer(
    live: &mut LiveReplTail,
    renderer: &mut render::StreamRenderer,
    outcome: QuestionResponse,
    herdr_turn: Option<&crate::cli::repl::herdr::TurnGuard>,
) -> Result<()> {
    let Some(crate::cli::repl::midturn_panel::TurnPanel::Question(layer)) =
        live.close_turn_panel()?
    else {
        return Ok(());
    };
    if let Some(guard) = herdr_turn {
        guard.resumed();
    }
    record_exchange(renderer, layer.panel.request(), &outcome)?;
    if matches!(outcome, QuestionResponse::Answered(_)) {
        renderer.start_waiting()?;
    }
    live.apply_renderer_frame(renderer)
}

/// 在这块面板上答的 / 关的：把结果回发给 daemon。
async fn reply(
    paths: &MiyuPaths,
    renderer: &mut render::StreamRenderer,
    asked: QuestionResponse,
    question_id: String,
    run_id: &str,
) -> Result<()> {
    match asked {
        QuestionResponse::Answered(answers) => {
            send_ipc_command(
                paths,
                IpcCommand::AnswerQuestion {
                    question_id,
                    answers,
                },
            )
            .await?;
            renderer.start_waiting()?;
        }
        // Nobody could be shown the panel — no tty, or it failed to
        // open. That is not the user calling the turn off, so the
        // question is resolved and the turn carries on; the tool
        // that asked finds out that nobody answered and can say so.
        QuestionResponse::Unavailable(_) => {
            let _ = send_ipc_command(paths, IpcCommand::CloseQuestion { question_id }).await;
        }
        // The terminal question UI maps its close gestures to
        // Cancelled; that one really is "stop this turn".
        QuestionResponse::Closed | QuestionResponse::Cancelled => {
            let _ = send_ipc_command(
                paths,
                IpcCommand::Cancel {
                    run_id: run_id.to_string(),
                },
            )
            .await;
        }
    }
    Ok(())
}

/// 把一问一答记进这一步，全屏下再写进正文。
fn record_exchange(
    renderer: &mut render::StreamRenderer,
    request: &QuestionRequest,
    response: &QuestionResponse,
) -> Result<()> {
    // 全屏下面板退场之后，下一帧就按缓冲恢复正文和输入区，
    // 问了什么、答了什么会一起消失（用户原话「回答完问题也没输出」）。
    // 写进缓冲它才算进了历史、回翻找得到。
    renderer.timeline_push_question(request, response)?;
    // 这一步补进去了，现在才切：屏幕上的顺序就成了
    // 「…询问用户 → Worked for… → 问答块」，和实际发生的顺序一致。
    //
    // 静态时间线不切：一问一答已经是那一步的正文了，切了这一段就断
    // 成两截（问答块底下空一行、下一步没有连线接上来）。
    if !renderer.caps().commit_immediately {
        renderer.prepare_for_external_output()?;
        renderer.write_question_exchange(request, response)?;
    }
    Ok(())
}

/// daemon 补发时附上的结果：这道题早就了结了。
fn settled_outcome(data: &serde_json::Value) -> Option<QuestionResponse> {
    serde_json::from_value(data.get("settled")?.clone()).ok()
}

/// 往前看一眼事件流：这道题是不是已经在别处了结了。
///
/// 只看不取——看到的帧面板关掉后泵照样按顺序处理，`question.answered` 那几条
/// 在分发表里本来就不做事。回合结束了也算了结：没人会再等这个答案。
fn settled_elsewhere(
    frames: &mut ipc::FrameReader,
    question_id: &str,
    run_id: &str,
) -> Option<QuestionResponse> {
    let mut outcome = None;
    // 读坏了的帧泵自己收的时候会报，这里不抢着报。
    let _ = frames.look_ahead::<IpcFrame>(|frame| {
        let IpcFrame::Event { kind, data, .. } = frame else {
            return false;
        };
        let this_question = ipc_text(&data, "question_id") == question_id;
        outcome = match kind.as_str() {
            "question.answered" if this_question => {
                serde_json::from_value(data.get("answers").cloned().unwrap_or_default())
                    .ok()
                    .map(QuestionResponse::Answered)
            }
            "question.closed" if this_question => Some(QuestionResponse::Closed),
            "run.completed" | "run.failed" | "run.cancelled"
                if ipc_text(&data, "run_id") == run_id =>
            {
                Some(QuestionResponse::Cancelled)
            }
            _ => None,
        };
        outcome.is_some()
    });
    outcome
}

/// 「她在等你回话」的 herdr 状态，作用域结束（不管怎么结束）就报回 `working`。
struct HerdrBlocked<'a>(Option<&'a crate::cli::repl::herdr::TurnGuard>);

impl<'a> HerdrBlocked<'a> {
    fn begin(
        guard: Option<&'a crate::cli::repl::herdr::TurnGuard>,
        question: Option<&str>,
    ) -> Self {
        if let Some(guard) = guard {
            guard.blocked(question);
        }
        Self(guard)
    }
}

impl Drop for HerdrBlocked<'_> {
    fn drop(&mut self) {
        if let Some(guard) = self.0 {
            guard.resumed();
        }
    }
}
