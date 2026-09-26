//! 跑一个平台回合。
//!
//! `run_platform_turn` 串起闸门、插件链、agent、投递。它长是因为每一步的失败
//! 都有不同的收尾——被限流要不要提示、被超越要不要保留排队、投递失败要不要
//! 回滚已记的送达。

use crate::platforms::*;

pub(crate) struct TurnOutcome {
    pub(crate) run_id: String,
    pub(crate) text: String,
    pub(crate) provider_id: Option<String>,
    pub(crate) model: Option<String>,
    /// Image asset ids published during the turn (`tool.image` events);
    /// bridges load the bytes and re-send them platform-natively.
    pub(crate) image_assets: Vec<String>,
    /// `image_assets` 里由表情包工具产出的那些(09-21 用户要求)。真人不会把
    /// 一句话和一个表情塞进同一条消息,所以投递时表情单独成一条——这里只标
    /// 出身,拆不拆由 `onebot::outbound` 决定。
    pub(crate) meme_assets: std::collections::BTreeSet<String>,
    /// Byte ranges produced after confirmed direct long-image tool sends.
    /// Direct-send acknowledgements are removed from the final fallback text.
    pub(crate) suppressed_reply_ranges: Vec<(usize, usize)>,
    /// The last response segment was delivered by a successful direct tool
    /// send, so an otherwise empty platform reply must not add a placeholder.
    pub(crate) final_reply_already_sent: bool,
}

pub(crate) enum TurnDispatch {
    Completed(TurnOutcome),
    Failed(String),
    /// 回合被取消(消息撤回、被新消息取代、/stop):不是错误,静默收场,
    /// 不给用户回"出错了",也不按内部错误记日志(09-03 用户报)。
    Cancelled,
}

/// Drives one agent turn for an inbound IM message and waits for the
/// final result. Mirrors `handle_ipc_turn`, minus the client stream.
pub(crate) async fn run_platform_turn(
    state: &DaemonState,
    session_id: Arc<str>,
    content: String,
    images: Vec<Option<ImageAttachment>>,
    mut profile: TurnProfile,
) -> Result<TurnDispatch> {
    let content = validate_content(content).map_err(|error| anyhow!(error.message))?;
    state.state_store.recover_stale_turns()?;

    let _global_permit = state
        .platforms
        .turn_permits
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| anyhow!("the platform turn scheduler is closed"))?;

    let run_id = random_id("run", 18);
    let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
    let platform_context = profile.platform.clone();
    let intermediate_replies = platform_context.as_ref().is_some_and(|context| {
        let qq = &context.config.platforms.qq;
        match context.conversation.kind {
            ConversationKind::Group => qq.group_intermediate_messages,
            ConversationKind::Private => qq.private_intermediate_messages,
        }
    });
    let platform_followup = platform_context
        .as_ref()
        .map(|context| PlatformFollowupRun::new(context.clone()));
    profile.followup = platform_followup.clone();
    {
        let mut manager = state.manager.lock().unwrap();
        if manager.admin_busy {
            bail!("Miyu is busy with another operation");
        }
        manager.active_runs.insert(
            run_id.clone(),
            RunInfo {
                session_id: session_id.clone(),
                mode: PersonaLane::Active,
                audience: PromptAudience::External,
                cancel: cancel_tx.clone(),
                turn_id: None,
                queue_target: None,
                supersede: Arc::new(miyu_engine::agent::TurnSupersedeSignal::default()),
                platform_followup,
                operation: crate::runtime::RunOperation::Create,
                job_wake: false,
                job_wake_label: None,
                first_event_id: None,
                // 平台真实入站消息;wake 合成轮的来源细分待平台 goal 支持时一并做。
                turn_origin: miyu_base::workspace::TurnOrigin::Human,
            },
        );
    }
    if let Some(context) = platform_context.as_ref() {
        context.turn_started(cancel_tx);
    }
    if platform_context
        .as_ref()
        .is_some_and(|context| context.turn_is_superseded())
    {
        crate::runtime::finish_run(&state.manager, &run_id, None);
        return Ok(TurnDispatch::Failed(
            miyu_base::i18n::text("the turn was superseded", "本轮已被新消息覆盖").to_string(),
        ));
    }
    let after = state.events.latest_id();
    let mut subscription = state.events.subscribe_after(after);
    if state
        .actor_tx
        .send(ActorCommand::StartTurn {
            run_id: run_id.clone(),
            session_id,
            display_content: content.clone(),
            content,
            attachment_run_id: None,
            mode: PersonaLane::Active,
            images,
            cwd: None,
            origin_tty: None,
            audience: PromptAudience::External,
            profile: Some(profile),
            overrides: None,
            cancel: cancel_rx,
            turn_origin: Box::new(miyu_base::workspace::TurnOrigin::Human),
        })
        .is_err()
    {
        crate::runtime::finish_run(&state.manager, &run_id, None);
        bail!("Miyu core worker is unavailable");
    }
    // Cancels the run if this task dies before the turn settles.
    let mut run_guard = IpcRunGuard {
        manager: state.manager.clone(),
        run_id: run_id.clone(),
        finished: false,
        one_shot: false,
        questions: None,
    };

    let deadline = tokio::time::Instant::now() + PLATFORM_TURN_TIMEOUT;
    let mut text = String::new();
    let mut image_assets = Vec::new();
    let mut meme_assets = std::collections::BTreeSet::new();
    let mut reply_suppression = ReplySuppression::default();
    // 这次 use_meme 调用声明了「表情就是全部回复」(用户 09-22:表情够表达时
    // 真人只发表情、一个字不说)。在 started 记下、finished 成功时才落闸——
    // 失败也落的话,她连「表情没发出去」这句解释都发不了。
    let mut meme_is_the_whole_reply = false;
    let mut last_id = after;
    let dispatch = loop {
        let record = if let Some(record) = subscription.pending.pop_front() {
            record
        } else {
            match tokio::time::timeout_at(deadline, subscription.receiver.recv()).await {
                Err(_) => {
                    break TurnDispatch::Failed(
                        miyu_base::i18n::text("the reply timed out", "回复超时，本轮已取消")
                            .to_string(),
                    );
                }
                Ok(Ok(record)) => record,
                Ok(Err(broadcast::error::RecvError::Lagged(_))) => {
                    subscription.pending = state.events.replay_after(last_id);
                    continue;
                }
                Ok(Err(broadcast::error::RecvError::Closed)) => {
                    break TurnDispatch::Failed(
                        miyu_base::i18n::text("Miyu core stopped", "Miyu 核心已停止").to_string(),
                    );
                }
            }
        };
        if record.kind == "resync_required" {
            break TurnDispatch::Failed(
                miyu_base::i18n::text(
                    "event history was exhausted; the turn was cancelled",
                    "事件缓冲耗尽，本轮已取消",
                )
                .to_string(),
            );
        }
        last_id = record.id;
        if record.run_id.as_deref() != Some(run_id.as_str()) {
            continue;
        }
        let Ok(data) = serde_json::from_str::<Value>(&record.data) else {
            continue;
        };
        match record.kind.as_str() {
            "reasoning.start" => {
                if intermediate_replies {
                    if let Some(context) = platform_context.as_ref() {
                        flush_intermediate_reply(context, &text, &reply_suppression).await;
                    }
                }
                start_model_reply(&mut text, &mut reply_suppression);
            }
            "assistant.delta" => {
                if let Some(delta) = data.get("delta").and_then(Value::as_str) {
                    text.push_str(delta);
                }
            }
            "generation.superseded" => {
                text.clear();
                reply_suppression.model_started();
            }
            // 端点重试(08-22 复读取证第 4 条):上一 attempt 已流出的半截正文
            // 作废,不丢弃就会与重试的完整正文拼接,经 split_reply 变成
            // "变体复读"。语义同 reasoning.start,但绝不 flush——半截正文
            // 正是要丢的东西。
            "reasoning.reset" => {
                start_model_reply(&mut text, &mut reply_suppression);
            }
            // 步与步之间的正文在这里就发,不等回合收尾。
            //
            // `reasoning.start` 一个人做不到这件事:它每次 **LLM 请求** 发一次,
            // 而中转线(claude-code / codex / antigravity)的工具循环在对端,
            // Miyu 的回合循环整轮只发一次,于是那一次落在回合开头、text 还空着,
            // flush 空转,中间说的话全积到 run.completed 一起投递。09-08 取证:
            // 六天 42 次私聊回合、2570 次工具调用,只 flush 出 1 条,还是端点
            // 切换换来的第二次 reasoning.start 的副作用。本地工具线一并受益
            // ——不必等工具跑完才见到上一段正文。
            "tool.started" => {
                let readable = format_platform_tool_started_log(&run_id, &data);
                tracing::info!(target: "miyu::qq", "\n{readable}");
                let tool_name = data.get("name").and_then(Value::as_str);
                if tool_name == Some("use_meme") {
                    meme_is_the_whole_reply = meme_alone_requested(&data);
                }
                let host_authored =
                    tool_name.is_some_and(crate::platforms::plugins::tool_authors_host_reply);
                if intermediate_replies && !host_authored {
                    if let Some(context) = platform_context.as_ref() {
                        flush_intermediate_reply(context, &text, &reply_suppression).await;
                        text.clear();
                        reply_suppression.round_flushed();
                    }
                }
            }
            "tool.image" => {
                if let Some(id) = data
                    .get("asset")
                    .and_then(|asset| asset.get("id"))
                    .and_then(Value::as_str)
                {
                    // 事件里的 name 是真工具名(event_map 的 real_tool_name 已经
                    // 把 "use_meme:show" 剥成 "use_meme")。资产表的 tool_id 存的
                    // 是调用 id(call_00_…),认不出工具,所以认这里。
                    if data.get("name").and_then(Value::as_str) == Some("use_meme") {
                        meme_assets.insert(id.to_string());
                    }
                    image_assets.push(id.to_string());
                } else {
                    // 无 asset 的 tool.image 是资产落库失败/turn 未知的错误
                    // 事件;静默吞掉=平台图片凭空消失且无迹可查。
                    let error = data
                        .get("error")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown")
                        .to_string();
                    tracing::warn!(
                        target: "miyu::qq",
                        run_id = %run_id,
                        error = %error,
                        "平台工具图片事件不带资产,已跳过(图片不会投递)"
                    );
                }
            }
            "tool.finished" => {
                let readable = format_platform_tool_finished_log(&run_id, &data);
                tracing::info!(target: "miyu::qq", "\n{readable}");
                let suppression_start = platform_context
                    .as_ref()
                    .and_then(|context| context.take_final_reply_suppression_start(text.len()));
                if let Some(start) = suppression_start {
                    reply_suppression.direct_send_succeeded(start);
                }
                // alone 那一路:发成了就把此后的正文整段截掉,只留表情那一条。
                if std::mem::take(&mut meme_is_the_whole_reply) && tool_call_succeeded(&data) {
                    reply_suppression.direct_send_succeeded(text.len());
                }
            }
            "queue.consumed" => {
                // Flush before the suppression reset below: the flushed text
                // still needs the direct-send ranges of the round it came
                // from, and the next round answers the newly consumed prompt.
                if intermediate_replies {
                    if let Some(context) = platform_context.as_ref() {
                        flush_intermediate_reply(context, &text, &reply_suppression).await;
                    }
                    text.clear();
                }
                reply_suppression.queued_prompt_consumed();
            }
            "run.completed" => {
                run_guard.finish();
                let (suppressed_reply_ranges, final_reply_already_sent) =
                    reply_suppression.finish(text.len());
                break TurnDispatch::Completed(TurnOutcome {
                    run_id: run_id.clone(),
                    text,
                    provider_id: data
                        .get("provider_id")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    model: data
                        .get("model")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    image_assets,
                    meme_assets,
                    suppressed_reply_ranges,
                    final_reply_already_sent,
                });
            }
            "run.failed" => {
                run_guard.finish();
                let message = data
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown error")
                    .to_string();
                break TurnDispatch::Failed(message);
            }
            "run.cancelled" => {
                run_guard.finish();
                break TurnDispatch::Cancelled;
            }
            _ => {}
        }
    };
    Ok(dispatch)
}

/// 这次 `use_meme` 调用声明了「表情就是全部回复」吗。
///
/// 读的是 `tool.started` 事件里的 `arguments`,而**它是一段 JSON 字符串**,
/// 不是对象(日志那侧一直是 `Value::as_str`)。09-22 第一版当对象取,于是
/// `alone` 永远读不到——真模型三轮都传了 `alone: true`,闸一次没落,而单测喂的
/// 是对象所以跟着一起绿。用例现在按事件的真形态写。
fn meme_alone_requested(data: &Value) -> bool {
    let Some(raw) = data.get("arguments") else {
        return false;
    };
    let parsed;
    let arguments = match raw {
        Value::String(text) => {
            parsed = serde_json::from_str::<Value>(text).unwrap_or(Value::Null);
            &parsed
        }
        other => other,
    };
    arguments
        .get("alone")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// `tool.finished` 说这次调用成功了吗。失败不落闸:否则她连「表情没发出去」
/// 这句解释都发不了。
fn tool_call_succeeded(data: &Value) -> bool {
    data.get("ok").and_then(Value::as_bool) == Some(true)
}

#[cfg(test)]
mod meme_alone_tests {
    use super::*;
    use serde_json::json;

    /// 用户 09-22:表情够表达意思时真人只发表情、一个字不说。这条钉住事件里
    /// 读的字段——名字写错只会静默失效。
    /// **事件里的 `arguments` 是一段 JSON 字符串**,用例必须按这个形态写。
    /// 第一版按对象写,于是代码里那个「当对象取」的 bug 跟着一起绿了,直到真
    /// 模型三轮都传了 `alone: true` 而闸一次没落才暴露。
    #[test]
    fn alone_is_read_from_the_started_arguments() {
        let event = |arguments: &str| json!({ "name": "use_meme", "arguments": arguments });
        assert!(meme_alone_requested(&event(
            r#"{"action":"show","alone":true,"id":"5e6d70f"}"#
        )));
        assert!(!meme_alone_requested(&event(
            r#"{"action":"show","alone":false}"#
        )));
        // 没传就是不声明:老行为(配字照发)不变。
        assert!(!meme_alone_requested(&event(r#"{"action":"show"}"#)));
        assert!(!meme_alone_requested(&event("")));
        assert!(!meme_alone_requested(&event("not json")));
        assert!(!meme_alone_requested(&json!({ "name": "use_meme" })));
        // 字符串 "true" 不算:宁可不落闸,也不要把她的话吃掉。
        assert!(!meme_alone_requested(&event(r#"{"alone":"true"}"#)));
        // 万一哪天事件改成直接给对象,也照样认。
        assert!(meme_alone_requested(
            &json!({ "name": "use_meme", "arguments": { "alone": true } })
        ));
    }

    /// 只有成功才落闸。
    #[test]
    fn only_a_successful_call_arms_the_latch() {
        assert!(tool_call_succeeded(&json!({ "ok": true })));
        assert!(!tool_call_succeeded(&json!({ "ok": false })));
        assert!(!tool_call_succeeded(&json!({})));
    }
}
