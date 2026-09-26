//! 一次性命令等子代理的终端那一面（09-26 起子代理只在后台跑）。
//!
//! 主回合画完之后，报告叫醒的那几轮一轮轮接着画（和主回合同一台渲染，见
//! `RemoteTurnSource::Follow`）；两轮之间等的时候，转轮上写还有几个子代理在跑。
//! Ctrl+C 连整棵子树一起停，按取消收场——和主回合里按 Ctrl+C 一个样。下一步是什么
//! 由 `subagent_wait` 定。

use super::one_shot::{run_remote_turn, RemoteTurnSource};
use crate::cli::subagent_wait::{stop_subagents, turns_after_main, SubagentWait, WaitStep};
use crate::cli::*;
use std::collections::HashSet;

/// 主回合之后：跟着报告叫醒的那几轮画，直到整棵子树收尾。
pub(in crate::cli) async fn follow_subagent_reports(
    paths: &MiyuPaths,
    main: &RemoteTurnSummary,
    show_reasoning: Option<bool>,
    plain: bool,
    mode: PersonaLane,
) -> Result<()> {
    let config = AppConfig::load_or_default(paths)?;
    let mut wait = SubagentWait::new(&main.session_id, Some(&main.run_id), main.first_event_id);
    let mut shown = main.turn_id.iter().cloned().collect::<HashSet<_>>();
    loop {
        let run = match next_step(paths, &mut wait, &config, plain).await? {
            WaitStep::Follow(run) => run,
            WaitStep::Settled | WaitStep::Waiting(_) => break,
        };
        print_wake_header(&run.label, plain)?;
        let source = RemoteTurnSource::Follow {
            run_id: run.run_id.clone(),
            session_id: run.session_id.clone(),
            after: run.first_event_id,
        };
        match run_remote_turn(paths, None, source, show_reasoning, plain, mode, None).await {
            Ok(summary) => shown.extend(summary.and_then(|summary| summary.turn_id)),
            Err(error) if is_remote_turn_cancelled(&error) => {
                stop_subagents(paths, wait.session_id()).await;
                return Err(error);
            }
            // 叫醒的那一轮自己失败了（端点报错之类）：照实打出来，接着等别的报告。
            Err(error) => print!("{}", error_frame(&error)),
        }
    }
    // 跟的时候没画到的轮（一闪而过的、事件环补不回来的）从会话库补上。
    if let Some(main_turn) = main.turn_id.as_deref() {
        let missed = turns_after_main(paths, &main.session_id, main_turn)?
            .into_iter()
            .filter(|turn| !shown.contains(&turn.turn_id))
            .collect::<Vec<_>>();
        print_missed_turns(&missed, &config, mode, plain)?;
    }
    Ok(())
}

/// 等下一轮可跟（或者整棵子树收尾）。等的时候转轮上写还有几个子代理在跑。
async fn next_step(
    paths: &MiyuPaths,
    wait: &mut SubagentWait,
    config: &AppConfig,
    plain: bool,
) -> Result<WaitStep> {
    let session_id = wait.session_id().to_string();
    // 在 herdr 里跑的话，等的这一阵侧栏也亮着——命令还没退出，人会回来看。
    let _herdr = herdr::TurnGuard::begin_transient(&session_id);
    // 转轮等真要等了再起：头一眼就有一轮可跟时，屏上什么都不该多。
    let mut spinner: Option<render::StreamRenderer> = None;
    let mut ticker = tokio::time::interval(Duration::from_millis(80));
    let step = loop {
        let step = {
            let next = wait.next(paths);
            tokio::pin!(next);
            loop {
                tokio::select! {
                    step = &mut next => break step,
                    _ = ticker.tick() => {
                        if let Some(spinner) = spinner.as_mut() {
                            spinner.tick_spinner()?;
                        }
                    }
                    _ = tokio::signal::ctrl_c() => {
                        if let Some(spinner) = spinner.as_mut() {
                            spinner.finish()?;
                        }
                        stop_subagents(paths, &session_id).await;
                        return Err(RemoteTurnCancelled::default().into());
                    }
                }
            }
        }?;
        let WaitStep::Waiting(running) = step else {
            break step;
        };
        let phase = waiting_phase(running);
        match spinner.as_mut() {
            Some(spinner) => spinner.set_custom_waiting_phase(Some(phase)),
            None => {
                let mut renderer = waiting_renderer(config, plain);
                renderer.start_waiting()?;
                renderer.set_custom_waiting_phase(Some(phase.clone()));
                // 转轮起不来（不是终端）：静态说一句，走 stderr 不混进正文。
                if !renderer.is_waiting() && !plain {
                    eprintln!("\x1b[2m{phase}\x1b[0m");
                }
                spinner = Some(renderer);
            }
        }
    };
    if let Some(mut spinner) = spinner {
        spinner.set_custom_waiting_phase(None);
        spinner.finish()?;
    }
    Ok(step)
}

fn waiting_renderer(config: &AppConfig, plain: bool) -> render::StreamRenderer {
    render::StreamRenderer::new(
        render::ReasoningDisplayMode::from_expand(config.display.expand_reasoning),
        render::ToolCallDisplayMode::from_expand(config.display.expand_tool_calls),
        plain,
        config.display.readable_tool_names,
        config.display.command_output_lines,
    )
}

fn waiting_phase(running: usize) -> String {
    if running == 0 {
        return t(
            "subagents finished, waiting for their reports",
            "子代理跑完了，等报告回来",
        )
        .to_string();
    }
    if is_zh() {
        format!("等 {running} 个子代理跑完 · Ctrl+C 全部停下")
    } else if running == 1 {
        "waiting for 1 subagent · Ctrl+C stops it".to_string()
    } else {
        format!("waiting for {running} subagents · Ctrl+C stops them")
    }
}

/// 叫醒的那一轮开头那一行，和 REPL 里后台任务完成那一行一个样子。目标续轮不打：
/// 一个长任务几十轮，每轮一行只会把真正的输出挤散。
fn print_wake_header(label: &str, plain: bool) -> Result<()> {
    if label == miyu_engine::tools::goal::GOAL_ROUND_LABEL {
        return Ok(());
    }
    let label = if label.is_empty() {
        t("background task finished", "后台任务完成")
    } else {
        label
    };
    let mut stdout = io::stdout();
    if plain {
        writeln!(stdout, "\n⚙ {label}\n")?;
    } else {
        writeln!(stdout, "\n\x1b[2m⚙ {label}\x1b[0m\n")?;
    }
    stdout.flush()?;
    Ok(())
}

/// 从会话库补画的轮：和重开会话时的回放同一套画法；plain 只要正文。
fn print_missed_turns(
    missed: &[miyu_core::state::TurnReplay],
    config: &AppConfig,
    mode: PersonaLane,
    plain: bool,
) -> Result<()> {
    if missed.is_empty() {
        return Ok(());
    }
    let mut stdout = io::stdout();
    if plain {
        for turn in missed {
            writeln!(stdout, "\n{}", turn.assistant_content.trim_end())?;
        }
    } else {
        let (cols, _) = replay_viewport();
        let endpoint_line = show_mixed_model_endpoint(config, false);
        stdout.write_all(&session_replay_frame(
            missed,
            mode,
            config,
            cols,
            endpoint_line,
        )?)?;
    }
    stdout.flush()?;
    Ok(())
}
