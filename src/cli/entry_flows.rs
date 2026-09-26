//! 两条非 REPL 的总入口：新手引导与一次性回合。从 `src/cli/mod.rs` 搬来（09-16 拆分），逻辑未改。

use crate::cli::*;

/// 跑新手引导,返回「接下来要不要进 REPL」。
///
/// 开场就退出(Esc / Ctrl+C)什么都不写、也不进 REPL,下次裸 `miyu` 还会再来;
/// 选了「进入设置界面」就先开完整设置再进;做完或跳过直接进——空会话的
/// banner 就是第一帧,不做完成页。引导写了配置,顺手让活着的 daemon 重读。
pub(super) async fn run_oobe_flow(paths: &MiyuPaths) -> Result<bool> {
    spawn_hangup_watchdog();
    // 后面是全屏 REPL 的话,备用屏一路不退,中间不闪 shell 画面。
    let keep_alt = crate::cli::repl::tail::screen::requested();
    let outcome = crate::oobe::run(paths, keep_alt)?;
    if outcome != crate::oobe::Outcome::Aborted {
        let _ = send_ipc_command(paths, IpcCommand::ReloadConfig).await;
    }
    match outcome {
        crate::oobe::Outcome::Aborted => {
            miyu_base::terminal::release_alt_screen_if_held();
            Ok(false)
        }
        crate::oobe::Outcome::OpenSettings => {
            if keep_alt {
                crate::config_tui::run_embedded(paths)?;
            } else {
                crate::config_tui::run(paths)?;
            }
            let _ = send_ipc_command(paths, IpcCommand::ReloadConfig).await;
            Ok(true)
        }
        crate::oobe::Outcome::Completed | crate::oobe::Outcome::Skipped => Ok(true),
    }
}

/// 一次性回合的总入口(`miyu ask …` 与裸 `miyu "…"`)。
///
/// 没用到任何程序驱动特性时走原路(直连/阅后即焚/终端渲染),行为一字不改;
/// 带了 `--create/--mode/--model/…` 或 JSON 输出时走新路:会话由
/// `turn_request` 定,覆盖随 StartTurn 走,需要 daemon。
pub(super) async fn run_one_shot(
    paths: &MiyuPaths,
    options: TurnOptions,
    message: String,
    read_stdin: bool,
    plain: bool,
    mode: PersonaLane,
) -> Result<()> {
    let message = if read_stdin {
        append_stdin_to_eof(message)?
    } else {
        append_stdin_if_piped(message).await
    };
    let format = if plain {
        OutputFormat::Text
    } else {
        options.output_format.unwrap_or_default()
    };
    let plain = plain || options.quiet;
    let overrides = turn_request::build_overrides(paths, &options)?;
    let programmatic = options.create
        || options.mode.is_some()
        || overrides.is_some()
        || format != OutputFormat::Text
        || !options.image.is_empty()
        || options.cwd.is_some()
        || options.timeout.is_some();
    if !programmatic {
        let session =
            one_shot_session(paths, options.session.as_deref(), options.continue_session).await?;
        return run_chat_with_options(
            paths,
            message,
            None,
            plain,
            mode,
            session,
            None,
            AfterTurn::AwaitSubagents,
        )
        .await;
    }
    if message.is_empty() {
        return Err(exit_code::usage_error(t(
            "a message is required",
            "需要给一条消息",
        )));
    }
    let session = turn_request::resolve_turn_session(paths, &options).await?;
    let outcome = match format {
        OutputFormat::Text => {
            if let Some(cwd) = options.cwd.as_deref() {
                std::env::set_current_dir(cwd).map_err(|error| {
                    exit_code::usage_error(format!(
                        "{}: {} ({error})",
                        t("cannot enter --cwd", "进不去 --cwd 目录"),
                        cwd.display()
                    ))
                })?;
            }
            let images = options
                .image
                .iter()
                .map(|path| {
                    Some(miyu_base::clipboard::PastedImage::Path(
                        path.to_string_lossy().into_owned(),
                    ))
                })
                .collect::<Vec<_>>();
            let turn_session = match session.session_id.clone() {
                Some(session_id) => TurnSession::Explicit(session_id),
                None => TurnSession::Current,
            };
            repl::direct::run_chat_with_images_and_options(
                paths,
                message,
                images,
                plain,
                mode,
                turn_session,
                overrides,
            )
            .await
        }
        OutputFormat::Json | OutputFormat::StreamJson => {
            output::run_json_one_shot(
                paths,
                output::turn_client::TurnRequest {
                    content: message,
                    session_id: session.session_id.clone(),
                    images: options.image.clone(),
                    cwd: options.cwd.clone(),
                    overrides,
                    timeout: options.timeout.map(Duration::from_secs),
                },
                format,
            )
            .await
        }
    };
    if session.ephemeral {
        if let Some(session_id) = session.session_id.as_deref() {
            discard_ephemeral_session(paths, session_id).await;
        }
    }
    outcome
}
