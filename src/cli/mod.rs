use miyu_base::config::{ActiveProviderModelConfig, AppConfig};
use miyu_base::i18n::{is_zh, text as t};
use miyu_base::paths::MiyuPaths;
use miyu_core::ipc::{self, Command as IpcCommand, Frame as IpcFrame, Request as IpcRequest};
use miyu_core::llm::{
    ChatResult, ChatStreamChunk, GenerationSpeed, OpenAiCompatibleClient, ThinkingVariantOptions,
    TurnTokens, Usage,
};
use miyu_core::memory::{MemoryOrganizer, MemoryStore};
use miyu_engine::agent::{
    archive_and_delete_visible_turns, Agent, AgentEvent, AgentTurnControl, PersonaLane,
};
mod args;
mod daemon_cmds;
mod entry_flows;
pub(crate) mod exit_code;
mod inline_picker;
mod localize;
mod mcp_schema;
mod mcp_serve;
mod output;
mod question_panel;
mod session_cmds;
mod setup;
mod stdin_input;
mod stdio;
mod tool_cmds;
mod turn_request;
mod usage_view;
use args::*;
use daemon_cmds::*;
use entry_flows::*;
use inline_picker::*;
use localize::*;
use mcp_serve::*;
use setup::*;
use stdin_input::*;
use tool_cmds::*;
use usage_view::*;
mod alarm_worker;
mod daemon_log;
mod data_cmds;
mod embed_cmds;
use embed_cmds::*;
mod footer;
mod history_replay;
mod host_cmds;
mod layout_cmds;
mod migrate_cmds;
mod model_cmds;
mod pm_cmds;
mod pop_cmds;
mod repl;
mod repl_history;
mod select;
mod shell_bridge;
mod stt;
mod terminal_guard;
mod variant_menu;

// 日志读取与格式化已拆到 daemon_log。
use alarm_worker::*;
use daemon_log::*;
use data_cmds::*;
use footer::*;
use history_replay::*;
use host_cmds::*;
use layout_cmds::*;
use migrate_cmds::*;
use model_cmds::*;
use pm_cmds::*;
use pop_cmds::*;
use select::*;
use shell_bridge::*;
use stt::*;
pub(crate) use terminal_guard::*;
use variant_menu::*;
#[cfg(test)]
mod tests;

// 宽度计算与输入编辑已拆到 repl 子模块，这里引回来。
// repl 下几个新拆的子模块整组导入（原本就在 cli/mod.rs 里，平铺可见）
pub(in crate::cli) use repl::{
    commands::*, input_layout::*, job_stop::*, jobs::*, layout::*, pickers::*, placeholder::*,
    sandbox_view::*, session::*,
};
// 命令表已上提到 crate 级与 WebUI 共用；这里再导出一次，cli 内的调用点不变。
pub(in crate::cli) use miyu_core::slash_commands::*;
use repl::direct::{run_chat_with_images, run_chat_with_options, run_direct_repl};
use repl::editor::{load_repl_input_history, repl_input_lines};
pub(in crate::cli) use repl::herdr;
use repl::input::render_repl_input_with_footer;
use repl::live_turn::{
    handle_live_agent_event, handle_live_post_turn_overflow, run_live_agent_turn,
};
use repl::remote::{run_remote_repl, try_run_remote_chat};
use repl::tail::{
    cursor_col_or, cursor_row_or, synchronized_terminal_update, FrameScroll, LiveRawMode,
    LiveReplTail, TerminalFrameLayout, TerminalFrameTracker,
};
use repl::wake::follow_wake_run;
use repl::width::{truncate_visible_width, visible_width, wrap_visible_width};
use repl_history::{
    legacy_repl_history_file, load_persistent_repl_history, persist_repl_history_entry,
    read_repl_history_file,
};

use miyu_engine::tools::build_tool_registry;
use miyu_hosts::render;

// 参数类型已下沉到基础层；这里 re-export，外部按 `cli::WebArgs` 引用不断。
use anyhow::{bail, Context, Result};
use base64::Engine;
use chrono::{DateTime, Local};
use clap::{Arg, ArgAction, Args, CommandFactory, FromArgMatches, Parser, Subcommand};
use crossterm::cursor::{self, Hide, MoveTo, Show};
use crossterm::event::{
    self, DisableBracketedPaste, DisableFocusChange, EnableBracketedPaste, EnableFocusChange,
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
};
use crossterm::style::{Color, Print, Stylize};
use crossterm::terminal::{self, Clear, ClearType};
use crossterm::{execute, queue};
use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use miyu_base::shell;
pub use miyu_core::args::WebArgs;
use miyu_core::state::{QueuedPrompt, QueuedPromptAttachment, StateStore, Turn, TurnStatus};
use miyu_engine::tools;
use std::ffi::OsString;
use std::io::Cursor;
use std::io::{self, IsTerminal, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;
use vte::{Params as VteParams, Parser as VteParser, Perform as VtePerform};

mod keyboard_enhancement;

use keyboard_enhancement::KeyboardEnhancementState;

pub fn parse() -> Cli {
    let args = apply_pm_shim(std::env::args_os().collect());
    parse_args(args).unwrap_or_else(|err| err.exit())
}

/// `miyupm …` 是 `miyu pm …` 的 shim:按 argv[0] 的文件名识别(打包时做个符号链接
/// 即可,不用第二个二进制)。`pm` 在帮助里已隐藏,这条显式入口照旧。
pub(in crate::cli) fn apply_pm_shim(mut args: Vec<std::ffi::OsString>) -> Vec<std::ffi::OsString> {
    let invoked_as_pm = args
        .first()
        .map(std::path::PathBuf::from)
        .and_then(|path| path.file_name().map(|name| name.to_os_string()))
        .is_some_and(|name| name == "miyupm");
    if invoked_as_pm {
        args.insert(1, std::ffi::OsString::from("pm"));
    }
    args
}

/// 装着的 shell hook 和这个二进制对不上就悄悄换成新的。
///
/// hook 是**生成**的文件（顶上那行写着版本和内容指纹），升级之后本该跟着换，
/// 否则新版的补全、按键绑定、拦截逻辑要等用户想起来手动跑一次 `fish-init`
/// 才生效。只动已经存在的文件：没装过 shell 集成的人不会被自作主张装上，
/// `.bashrc` / `.zshrc` 里那段 source 也一个字不碰。
///
/// 换完只对**新开的 shell** 生效，所以不打字——当前这条命令的输出里冒出一行
/// 「已更新 hook」只会碍事（shellhook 那条路上还会糊进正文）。
///
/// **隔离家目录下不做**：fish 的 hook 在 `~/.config/fish/conf.d/` 下，
/// 不跟着 `MIYU_HOME` 走，沙箱/测具一跑就会把用户真正在用的那份改掉。
/// 走查要验这条路的话，把 `XDG_CONFIG_HOME` 也指进沙箱，再用
/// `MIYU_SHELL_HOOK_SYNC=1` 强制打开。
fn refresh_shell_hooks(paths: &MiyuPaths) {
    let forced = std::env::var_os("MIYU_SHELL_HOOK_SYNC").is_some_and(|value| value == "1");
    if !forced && std::env::var_os("MIYU_HOME").is_some() {
        return;
    }
    let updated = miyu_base::shell::sync_installed_hooks(paths);
    if !updated.is_empty() {
        tracing::info!(
            shells = updated.join(", "),
            version = env!("CARGO_PKG_VERSION"),
            "refreshed installed shell hooks"
        );
    }
}

pub async fn run(cli: Cli, paths: MiyuPaths) -> Result<()> {
    if cli.shell_classify {
        let shell_name = cli.shell.as_deref().unwrap_or("fish");
        let message = shell_message_from_input(cli.stdin, cli.message)?;
        return run_shell_classify(shell_name, &message);
    }

    if cli.clipboard_paste {
        return run_clipboard_paste(&paths);
    }
    // A log viewer must not append its own startup record to the file it is
    // about to display. Apart from being confusing, that made `-n 1` return
    // the viewer's initialization line instead of the daemon's latest event.
    let skip_diagnostic_logging = matches!(
        &cli.command,
        Some(Command::Daemon(DaemonArgs {
            command: Some(DaemonCommand::Logs(_)),
            ..
        }))
    );
    let _logging_guard = if skip_diagnostic_logging {
        None
    } else {
        match miyu_base::logging::init(&paths, cli.debug) {
            Ok(guard) => Some(guard),
            Err(err) => {
                eprintln!(
                    "{}: {err:#}",
                    t(
                        "warning: diagnostic logging is unavailable",
                        "警告：诊断日志不可用"
                    )
                );
                None
            }
        }
    };
    refresh_shell_hooks(&paths);
    // 终端集成会话那条车道按配置选模式（daemon 侧按会话人格强制，这里只管
    // 直连路和 IPC 里那个遗留字段）。
    let mode = terminal_lane_mode(&paths);

    if cli.shell_intercept {
        let shell_name = cli.shell.as_deref().unwrap_or("fish");
        let message = shell_message_from_input(cli.stdin, cli.message)?;
        return run_shell_intercept(&paths, shell_name, message).await;
    }

    if !paths.config_file.exists()
        && !matches!(
            cli.command,
            Some(Command::Init)
                | Some(Command::FishInit)
                | Some(Command::BashInit)
                | Some(Command::ZshInit)
                | Some(Command::RemoveShellHook)
                | Some(Command::Paths)
                | Some(Command::Layout(_))
                | Some(Command::Pm(_))
                | Some(Command::Host(_))
                | Some(Command::Import(_))
        )
    {
        // 紧接着就进引导或全屏画面的,初始化不打字:那几行会留在屏上。
        let quiet = cli.banner
            || (matches!(cli.command, None | Some(Command::Oobe))
                && cli.message.is_empty()
                && io::stdin().is_terminal());
        run_init(
            &paths,
            if quiet {
                InitKind::Quiet
            } else {
                InitKind::FirstRun
            },
        )?;
    }
    if cli.banner {
        let config = AppConfig::load_or_default(&paths)?;
        return crate::cli::repl::banner::preview::run(&config, &paths);
    }

    // Captured before `cli.command` is moved out: one-shot entry points below
    // need them to pick the session their turn lands in.
    let session_arg = cli.turn.session.clone();
    let continue_session = cli.turn.continue_session;
    let root_turn = cli.turn.clone();
    let plain = cli.stdout;
    let root_stdin = cli.stdin;

    match cli.command {
        Some(Command::AlarmWorker(args)) => run_alarm_worker(args),
        Some(Command::DaemonWorker(args)) => {
            // 谁起的谁死就跟着死（真 daemon 除外，它带着 detached 标记）。
            // 放在最前面：越早捆上，能漏掉的窗口越小。
            miyu_base::orphan_guard::tie_lifetime_to_launcher();
            let _logging_guard = miyu_base::logging::init(&paths, cli.debug).ok();
            // daemon 的 stdout/stderr 被重定向进 daemon.log，而 tracing 写的是
            // 另一个按天滚动的文件。出了事翻错文件是常态——排查一次长回复不转
            // 图片，我在 daemon.log 里绕了很久，真正的 warning 一直躺在
            // miyu.YYYY-MM-DD.log 里。所以在这条日志的开头指一次路。
            println!(
                "{}",
                miyu_base::i18n::text(
                    "Detailed logs (warnings, tool failures) go to miyu.YYYY-MM-DD.log in the same directory; this file only carries startup output.",
                    "详细日志（警告、工具失败）在同目录的 miyu.YYYY-MM-DD.log；本文件只有启动输出。"
                )
            );
            miyu_hosts::daemon::run(paths, args).await
        }
        Some(Command::Tool(args)) => run_tool(&paths, mode, args).await,
        Some(Command::Ask(args)) => {
            let options = root_turn.merged(args.turn);
            run_one_shot(
                &paths,
                options,
                join_message(args.message),
                root_stdin || args.read_stdin,
                plain,
                mode,
            )
            .await
        }
        Some(Command::Stt) => {
            let session =
                one_shot_session(&paths, session_arg.as_deref(), continue_session).await?;
            run_stt_once(&paths, cli.stdout, mode, session).await
        }
        Some(Command::Listen) => run_listen(&paths).await,
        Some(Command::Voice(args)) => run_voice_command(&paths, args.command).await,
        Some(Command::Init) => run_init(&paths, InitKind::Explicit),
        Some(Command::Paths) => {
            paths.print();
            Ok(())
        }
        Some(Command::Layout(args)) => run_layout(&paths, args),
        Some(Command::Pm(args)) => run_pm(&paths, args).await,
        Some(Command::Host(args)) => run_host(&paths, args).await,
        Some(Command::Config(args)) => {
            let saved = run_config(&paths, args).await?;
            if saved && ipc::daemon_info(&paths).await.is_some() {
                reload_daemon_if_running(&paths).await
            } else {
                if saved {
                    let config = AppConfig::load_or_default(&paths)?;
                    if config.platforms.qq.enabled {
                        println!(
                            "{}",
                            t(
                                "Tencent QQ is enabled; run `miyu daemon start` to begin listening.",
                                "腾讯 QQ 已启用；执行 `miyu daemon start` 后开始监听。",
                            )
                        );
                    }
                }
                Ok(())
            }
        }
        Some(Command::Reload) => run_reload(&paths).await,
        Some(Command::Models(args)) => {
            initialize_models_cache(&paths);
            run_models(&paths, args).await
        }
        Some(Command::Export(args)) => run_export(&paths, args),
        Some(Command::Import(args)) => run_import(&paths, args).await,
        Some(Command::ListModels) => {
            initialize_models_cache(&paths);
            run_list_models(&paths)
        }
        Some(Command::Variant(args)) => {
            initialize_models_cache(&paths);
            run_variant(&paths, args)?;
            reload_daemon_if_running(&paths).await
        }
        Some(Command::FishInit) => shell::fish::install(&paths),
        Some(Command::BashInit) => shell::bash::install(&paths),
        Some(Command::ZshInit) => shell::zsh::install(&paths),
        Some(Command::RemoveShellHook) => remove_shell_hooks(&paths),
        Some(Command::History(args)) => run_history(&paths, args),
        Some(Command::Pop(args)) => {
            if let Some(target) = args.session.as_deref().or(session_arg.as_deref()) {
                let count = args.count.ok_or_else(|| {
                    exit_code::usage_error(t(
                        "--session pop needs a count",
                        "按会话 pop 需要给数量",
                    ))
                })?;
                return session_cmds::run_session_command(
                    &paths,
                    SessionCommand::Pop {
                        target: target.to_string(),
                        count,
                    },
                    plain,
                )
                .await;
            }
            if ipc::daemon_info(&paths).await.is_some() {
                run_pop_via_daemon(&paths, args).await
            } else {
                run_pop(&paths, args)
            }
        }
        Some(Command::Compact(args)) => match args.session.as_deref().or(session_arg.as_deref()) {
            Some(target) => {
                let entry = turn_request::resolve_managed_session(&paths, target).await?;
                let name = entry.name.clone();
                session_cmds::compact_session(
                    &paths,
                    miyu_core::ipc::SessionRef::Id { id: entry.id },
                    Some(&name),
                    plain,
                )
                .await
            }
            None => {
                session_cmds::compact_session(
                    &paths,
                    miyu_core::ipc::SessionRef::Current,
                    None,
                    plain,
                )
                .await
            }
        },
        Some(Command::Kb(args)) => run_kb(&paths, args).await,
        Some(Command::Embed(args)) => run_embed(&paths, args).await,
        Some(Command::UpdateDefaultKb) => run_update_default_kb(&paths).await,
        Some(Command::Memory(args)) => run_memory(&paths, args),
        Some(Command::Skills(args)) => run_skills(&paths, args),
        Some(Command::ResetMemoryCli) => run_reset_memory_command(&paths).await,
        Some(Command::ResetAllMemoryCli) => run_reset_all_memory_command(&paths).await,
        Some(Command::Reset(args)) => {
            if let Some(target) = args.session.as_deref().or(session_arg.as_deref()) {
                let entry = turn_request::resolve_managed_session(&paths, target).await?;
                send_ipc_admin(
                    &paths,
                    IpcCommand::ResetConversation {
                        target: miyu_core::ipc::SessionRef::Id { id: entry.id },
                    },
                )
                .await?;
            } else if ipc::daemon_info(&paths).await.is_some() {
                send_ipc_admin(
                    &paths,
                    IpcCommand::ResetConversation {
                        target: miyu_core::ipc::SessionRef::Current,
                    },
                )
                .await?;
            } else {
                run_reset(&paths).await?;
            }
            print_reset_message();
            Ok(())
        }
        Some(Command::Wipe(args)) => run_wipe(&paths, args.yes).await,
        Some(Command::ToolCallCmd(args)) => run_tool_call(&paths, args).await,
        Some(Command::McpServe) => run_mcp_serve(&paths).await,
        Some(Command::Session(args)) => {
            session_cmds::run_session_command(&paths, args.command, plain).await
        }
        Some(Command::Stdio) => stdio::run_stdio(&paths).await,
        Some(Command::Dev) => run_repl(&paths, PersonaLane::Dev).await,
        Some(Command::Oobe) => {
            if run_oobe_flow(&paths).await? {
                let result = run_repl(&paths, PersonaLane::Active).await;
                // REPL 没能接过备用屏(启动失败)就自己退回主屏,别把终端留在备用屏上。
                miyu_base::terminal::release_alt_screen_if_held();
                result
            } else {
                Ok(())
            }
        }
        Some(Command::Web(args)) => run_web(&paths, args).await,
        Some(Command::Daemon(args)) => run_daemon_command(&paths, args).await,
        None => {
            let message = join_message(cli.message);
            if message.is_empty() && io::stdin().is_terminal() {
                if session_arg.is_some() || continue_session {
                    bail!(
                        "{}",
                        t(
                            "--session and --continue only apply to one-shot commands; use /session inside the REPL",
                            "--session 与 --continue 仅用于一次性命令；REPL 内请使用 /session 切换"
                        )
                    );
                }
                // 裸 miyu = 普通 REPL(`miyu dev` 才是开发预设)。第一次先走
                // 新手引导;老配置在 migrate 里已标成做过,不会被拦。
                let config = AppConfig::load_or_default(&paths)?;
                if crate::oobe::needed(&config) && !run_oobe_flow(&paths).await? {
                    return Ok(());
                }
                let result = run_repl(&paths, PersonaLane::Active).await;
                miyu_base::terminal::release_alt_screen_if_held();
                result
            } else {
                run_one_shot(&paths, root_turn, message, root_stdin, plain, mode).await
            }
        }
    }
}

/// 「终端集成会话默认模式」：单次 / shellhook 那条路的模式。
fn terminal_lane_mode(paths: &MiyuPaths) -> PersonaLane {
    match AppConfig::load_or_default(paths) {
        Ok(config) if config.terminal_session_is_dev() => PersonaLane::Dev,
        _ => PersonaLane::Active,
    }
}

async fn run_repl(paths: &MiyuPaths, initial_mode: PersonaLane) -> Result<()> {
    if direct_mode_requested() {
        run_direct_repl(paths, initial_mode).await
    } else {
        run_remote_repl(paths, initial_mode).await
    }
}

fn direct_mode_requested() -> bool {
    std::env::var_os("MIYU_DIRECT").is_some_and(|value| value != "0")
}

fn reload_repl_config(
    paths: &MiyuPaths,
    state: &StateStore,
    config: &mut AppConfig,
    client: &mut OpenAiCompatibleClient,
) -> Result<()> {
    *config = AppConfig::load(paths)?;
    apply_session_model_override(state, config);
    *client = OpenAiCompatibleClient::from_config(config, paths)?;
    Ok(())
}

/// 会话内输入历史的容量上限:REPL 常开数天时防无界增长,超限丢最老。
const REPL_HISTORY_LIMIT: usize = 500;

fn push_history_capped(history: &mut Vec<ReplHistoryEntry>, entry: ReplHistoryEntry) {
    history.push(entry);
    if history.len() > REPL_HISTORY_LIMIT {
        let excess = history.len() - REPL_HISTORY_LIMIT;
        history.drain(..excess);
    }
}

struct LiveSubmission {
    content: String,
    display_content: String,
    images: Vec<Option<miyu_base::clipboard::PastedImage>>,
    /// 提交时输入框里的粘贴载荷(按占位符序号),给上键历史留着。
    pasted_texts: Vec<Option<PastedText>>,
}

fn trim_trailing_none<T>(mut items: Vec<Option<T>>) -> Vec<Option<T>> {
    while matches!(items.last(), Some(None)) {
        items.pop();
    }
    items
}

/// 把一条历史并进列表:同一次提交可能同时来自对话记录(展开全文)和历史
/// 文件(占位符+载荷),按展开后的文本认作同一条,带载荷的那份胜出并留在原位。
/// 返回是否新增了条目。
fn merge_history_entry(history: &mut Vec<ReplHistoryEntry>, entry: ReplHistoryEntry) -> bool {
    let expanded = entry.expanded();
    if let Some(position) = history
        .iter()
        .position(|existing| existing.expanded() == expanded)
    {
        if entry.has_payload() && !history[position].has_payload() {
            history[position] = entry;
        }
        return false;
    }
    push_history_capped(history, entry);
    true
}

struct LiveAgentInput<'a> {
    content: &'a str,
    images: &'a [Option<miyu_base::clipboard::PastedImage>],
}

fn queued_prompt_lines(prompts: &[QueuedPrompt], mode: PersonaLane, cols: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for prompt in prompts {
        // 后台任务的报告不是「有人排着队等说话」：它排在队里不该占一条气泡
        // 加一行「排队中」。这一轮吃进它的时候，它会作为时间线上的一条通知
        // 出现（用户 09-21 截图）。跨会话消息同理（09-23）。
        if repl::jobs::is_daemon_notice(&prompt.display_content) {
            continue;
        }
        if !lines.is_empty() {
            lines.push(String::new());
        }
        lines.extend(submitted_echo_lines(mode, &prompt.display_content, cols));
        lines.push(format!(
            "{} {}",
            submitted_echo_bar(mode),
            primary_footer_text(t("Queued", "排队中"))
        ));
    }
    lines
}

fn write_committed_user_messages(
    messages: &[(&str, PersonaLane)],
    leading_gap: bool,
) -> Result<()> {
    write_committed_user_messages_from(messages, leading_gap, None)
}

/// `known_col`:调用方已知的当前光标列。提交路径的同步块内禁止 ESC[6n
/// 查询(等应答会让 kitty 同步超时、提前提交半成品帧——光标闪屏),
/// suspend 之后列是确定的,直接传进来。
fn write_committed_user_messages_from(
    messages: &[(&str, PersonaLane)],
    leading_gap: bool,
    known_col: Option<u16>,
) -> Result<()> {
    if messages.is_empty() {
        return Ok(());
    }
    let mut stdout = io::stdout();
    let col = known_col.unwrap_or_else(|| cursor_col_or(0));
    write!(
        stdout,
        "{}",
        committed_user_messages_frame(messages, leading_gap, col, terminal_cols())
    )?;
    stdout.flush()?;
    Ok(())
}

/// 回显要写到终端的全部字节:光标不在行首就先换行,再接回显正文。
/// 单独成函数是为了让提交路径能拿同一串字节去推算写完后的光标位置。
fn committed_user_messages_frame(
    messages: &[(&str, PersonaLane)],
    leading_gap: bool,
    col: u16,
    cols: usize,
) -> String {
    let mut frame = String::new();
    if col > 0 {
        frame.push('\n');
    }
    frame.push_str(&committed_user_messages_text(messages, leading_gap, cols));
    frame
}

fn committed_user_messages_text(
    messages: &[(&str, PersonaLane)],
    leading_gap: bool,
    cols: usize,
) -> String {
    let mut output = String::new();
    if leading_gap {
        output.push('\n');
    }
    for (index, (content, mode)) in messages.iter().enumerate() {
        if index > 0 {
            output.push('\n');
        }
        for line in submitted_echo_lines(*mode, content, cols) {
            output.push_str(&line);
            output.push('\n');
        }
    }
    output.push('\n');
    output
}

fn queued_prompt_attachments(
    images: &[Option<miyu_base::clipboard::PastedImage>],
) -> Vec<QueuedPromptAttachment> {
    images
        .iter()
        .filter_map(|image| match image {
            Some(miyu_base::clipboard::PastedImage::Binary(image)) => {
                Some(QueuedPromptAttachment::Binary {
                    mime: image.mime.clone(),
                    data_base64: base64::engine::general_purpose::STANDARD.encode(&image.data),
                })
            }
            Some(miyu_base::clipboard::PastedImage::Path(path)) => {
                Some(QueuedPromptAttachment::Path { path: path.clone() })
            }
            None => None,
        })
        .collect()
}

fn persist_queued_submission(
    state: &StateStore,
    submission: &LiveSubmission,
) -> Result<QueuedPrompt> {
    let prompt_id = format!(
        "queued_{}_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis())
            .unwrap_or(0),
        rand::random::<u16>()
    );
    state.enqueue_prompt(
        &prompt_id,
        &submission.content,
        &submission.display_content,
        &queued_prompt_attachments(&submission.images),
    )
}

enum LiveReplOutcome {
    Exit,
    Submit(
        PersonaLane,
        String,
        Vec<Option<miyu_base::clipboard::PastedImage>>,
        /// 进上键历史的样子(占位符+载荷),不是展开后的全文。
        ReplHistoryEntry,
    ),
    /// 这个会话里有一轮**不是我起的**在跑，调用方该挂上去实时渲染。
    ///
    /// 两种来源：daemon 自己起的唤醒轮（后台任务跑完、目标续轮），以及同一个
    /// 会话里**另一个客户端**起的轮（第二个 TUI、另一个终端的 shellhook）。
    /// 后者要 `from_start`：它挂上来时那一轮可能已经流了一半。
    FollowWake {
        run_id: String,
        label: String,
        from_start: bool,
    },
    /// Ctrl+C on an empty line while this session has background work: stop
    /// the work and stay in the REPL. Pressing it again then exits.
    StopJobs,
    /// 全屏详情面板里按了 x：停掉**这一个**后台任务，人留在 REPL 里。
    StopJob {
        job_id: String,
    },
    /// 空会话里按了 Tab:换到另一条车道(普通 ↔ 开发)。调用方负责重绑会话。
    SwitchMode(PersonaLane),
    /// Tab(非空会话)/ Shift+Tab:切只读模式(09-23)。
    ToggleReadonly,
}

fn repl_history_is_clean(
    input: &str,
    history: &[ReplHistoryEntry],
    history_clean_index: Option<usize>,
) -> bool {
    history_clean_index
        .and_then(|index| history.get(index))
        .map(|entry| entry.display == input)
        .unwrap_or(false)
}

fn repl_should_browse_history(
    input: &str,
    history: &[ReplHistoryEntry],
    history_clean_index: Option<usize>,
) -> bool {
    input.is_empty() || repl_history_is_clean(input, history, history_clean_index)
}

fn run_history(paths: &MiyuPaths, args: HistoryArgs) -> Result<()> {
    let state = StateStore::new(paths)?;
    run_history_with_state(&state, args)
}

#[cfg(test)]
mod default_kb_progress_tests {
    use super::*;

    #[test]
    fn progress_is_emitted_as_a_complete_line() {
        let stage = miyu_engine::default_kb::UpdateStage::FetchingRepository;
        let mut output = Vec::new();

        write_default_kb_update_progress(&mut output, stage).unwrap();

        assert_eq!(
            String::from_utf8(output).unwrap(),
            format!("[default-kb] {}\n", stage.message())
        );
    }
}

fn join_message(parts: Vec<String>) -> String {
    parts.join(" ").trim().to_string()
}

/// 终端入口的事件处理:渲染交给 `render::apply_agent_event`,只有模型提问要在
/// 这里弹面板(问题面板是终端入口自己的东西)。
pub(crate) fn handle_agent_event(
    renderer: &mut render::StreamRenderer,
    event: AgentEvent,
) -> Result<()> {
    match render::apply_agent_event(renderer, event)? {
        Some(AgentEvent::AskQuestion {
            request, responder, ..
        }) => {
            renderer.prepare_for_external_output()?;
            question_panel::answer(renderer, request, responder, None)
        }
        _ => Ok(()),
    }
}
