//! 程序驱动形态的「带着结论收尾」（09-26 起子代理只在后台跑）。
//!
//! 主回合派了子代理的话，这一轮收尾时说的只是「派出去了」，结论在子代理跑完、报告
//! 叫醒这条会话再起的那几轮里。一次性 JSON 输出和 `miyu stdio` 的一条消息都要带着结论
//! 收尾：主回合之后接着等（`subagent_wait`），叫醒的那几轮照样往外吐事件；终态 `done`
//! 只有一条、在最后，正文是最后那一轮的，用时算整条命令的。

use super::event::{ErrorKind, NoticeLevel, PublicEvent};
use super::turn_client::{
    follow_run, run_turn, CancelSignal, QuestionPolicy, TurnOutcome, TurnRequest,
};
use crate::cli::subagent_wait::{stop_subagents, turns_after_main, SubagentWait, WaitStep};
use anyhow::Result;
use miyu_base::paths::MiyuPaths;
use std::time::Instant;

/// 跑一轮，再等它派出去的子代理都收尾。取消 / 超时连整棵子树一起停。
pub async fn run_turn_to_conclusion(
    paths: &MiyuPaths,
    request: TurnRequest,
    policy: QuestionPolicy,
    cancel: Option<CancelSignal>,
    mut emit: impl FnMut(PublicEvent),
) -> Result<TurnOutcome> {
    let started_at = Instant::now();
    let deadline = request.timeout.map(|timeout| started_at + timeout);
    // 往外吐过的轮（`started` 里带的轮号）：收尾时对会话库，看哪一轮漏了。
    let mut shown = Vec::<String>::new();
    let mut relay = |event: PublicEvent, shown: &mut Vec<String>| {
        if let PublicEvent::Started {
            turn_id: Some(turn_id),
            ..
        } = &event
        {
            shown.push(turn_id.clone());
        }
        emit(event);
    };
    let main = run_turn(paths, request, policy, cancel.clone(), |event| {
        relay(event, &mut shown)
    })
    .await?;
    let outcome = main.outcome;
    let TurnOutcome::Completed(PublicEvent::Done {
        session_id, run_id, ..
    }) = &outcome
    else {
        return Ok(outcome);
    };
    let main_turn = shown.first().cloned();
    let session_id = session_id.clone();
    let mut wait = SubagentWait::new(&session_id, Some(run_id), main.first_event_id);
    let mut last = outcome;
    let mut announced = false;
    loop {
        let step = tokio::select! {
            step = wait.next(paths) => step?,
            kind = interrupted(cancel.clone(), deadline) => {
                stop_subagents(paths, &session_id).await;
                let message = match kind {
                    ErrorKind::Timeout => "turn timed out",
                    _ => "cancelled",
                };
                return Ok(TurnOutcome::failed(kind, message, Some(session_id)));
            }
        };
        match step {
            WaitStep::Waiting(running) => {
                if !announced && running > 0 {
                    announced = true;
                    relay(
                        PublicEvent::Notice {
                            level: NoticeLevel::Info,
                            message: format!(
                                "waiting for {running} background subagent(s) to report"
                            ),
                        },
                        &mut shown,
                    );
                }
            }
            WaitStep::Follow(run) => {
                let followed = follow_run(
                    paths,
                    &run.run_id,
                    &run.session_id,
                    run.first_event_id,
                    policy,
                    cancel.clone(),
                    deadline,
                    |event| relay(event, &mut shown),
                )
                .await?
                .outcome;
                if let TurnOutcome::Failed {
                    kind: ErrorKind::Cancelled | ErrorKind::Timeout | ErrorKind::Disconnected,
                    ..
                } = &followed
                {
                    stop_subagents(paths, &session_id).await;
                    return Ok(followed);
                }
                // 叫醒的那一轮自己失败了（端点报错之类）也记下：要是后面再没有别的轮，
                // 这就是整条命令的结局。
                last = followed;
            }
            WaitStep::Settled => break,
        }
    }
    // 会话里最后那一轮跟的时候漏了（两次看之间就跑完的）：结论从库里取。
    let final_turn = main_turn
        .as_deref()
        .and_then(|main_turn| turns_after_main(paths, &session_id, main_turn).ok())
        .and_then(|turns| turns.into_iter().last())
        .filter(|turn| !shown.contains(&turn.turn_id));
    Ok(conclude(last, final_turn.as_ref(), started_at))
}

/// 整条命令的终态：最后跟上的那一轮；会话里最后那一轮跟的时候漏了，正文换成库里它的。
/// 用时算整条命令的。
fn conclude(
    last: TurnOutcome,
    missed_final: Option<&miyu_core::state::TurnReplay>,
    started_at: Instant,
) -> TurnOutcome {
    match last {
        TurnOutcome::Completed(PublicEvent::Done {
            session_id,
            run_id,
            text,
            usage,
            usage_estimated,
            model,
            provider_id,
            context_tokens,
            context_window,
            ..
        }) => TurnOutcome::Completed(PublicEvent::Done {
            session_id,
            run_id,
            text: missed_final
                .filter(|turn| !turn.interrupted)
                .map(|turn| turn.assistant_content.clone())
                .unwrap_or(text),
            usage,
            usage_estimated,
            model,
            provider_id,
            context_tokens,
            context_window,
            elapsed_ms: started_at.elapsed().as_millis() as u64,
        }),
        other => other,
    }
}

/// 外面叫停了：宿主取消（stdio 的 `cancel` / EOF）、一次性调用里的 Ctrl+C、`--timeout`
/// 到点。返回按哪一种收场。
async fn interrupted(cancel: Option<CancelSignal>, deadline: Option<Instant>) -> ErrorKind {
    let deadline = async {
        match deadline {
            Some(deadline) => {
                tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await
            }
            None => std::future::pending::<()>().await,
        }
    };
    match cancel {
        Some(mut cancel) => {
            // 发送端没了（宿主那头收摊）也算取消，和 `turn_client` 收帧那一段一个口径。
            let cancelled = async {
                while !*cancel.borrow() {
                    if cancel.changed().await.is_err() {
                        break;
                    }
                }
            };
            tokio::select! {
                _ = cancelled => ErrorKind::Cancelled,
                _ = deadline => ErrorKind::Timeout,
            }
        }
        None => {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => ErrorKind::Cancelled,
                _ = deadline => ErrorKind::Timeout,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn done(text: &str) -> TurnOutcome {
        TurnOutcome::Completed(PublicEvent::Done {
            session_id: "s".to_string(),
            run_id: "r".to_string(),
            text: text.to_string(),
            usage: None,
            usage_estimated: false,
            model: None,
            provider_id: None,
            context_tokens: 0,
            context_window: None,
            elapsed_ms: 0,
        })
    }

    fn text_of(outcome: TurnOutcome) -> String {
        match outcome {
            TurnOutcome::Completed(PublicEvent::Done { text, .. }) => text,
            _ => panic!("expected a completed outcome"),
        }
    }

    #[test]
    fn the_conclusion_is_the_last_turn_even_when_it_was_missed_live() {
        assert_eq!(
            text_of(conclude(done("followed"), None, Instant::now())),
            "followed"
        );
        let missed = miyu_core::state::TurnReplay {
            assistant_content: "from the database".to_string(),
            ..Default::default()
        };
        assert_eq!(
            text_of(conclude(done("followed"), Some(&missed), Instant::now())),
            "from the database"
        );
        // 库里最后那一轮是被打断的：它没有结论，留跟上的那一轮。
        let interrupted = miyu_core::state::TurnReplay {
            interrupted: true,
            ..missed
        };
        assert_eq!(
            text_of(conclude(
                done("followed"),
                Some(&interrupted),
                Instant::now()
            )),
            "followed"
        );
    }
}
