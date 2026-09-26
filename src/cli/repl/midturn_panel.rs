//! 回合跑着时的 `/models` / `/session` 面板：开在活动区的位置上，正文照流（会话项目
//! 第 3 段，B4）。
//!
//! 历史：
//! - 最早这两条命令在回合中走「分离 → 开面板 → 挂回来」：每分离一次就把当前渲染段收尾
//!   定稿吐一行小结，挂回来又是全新的渲染器重新计时（用户 09-20：「我每 /models 一次就会
//!   多一行 Worked for」）。
//! - 09-20 改成寄宿在回合循环里：面板自己跑一个同步事件循环，IPC 事件在 socket 里排队，
//!   面板收掉再接着画。没有接缝了，可面板开着的那段时间正文停在那一刻（待办 BUG 第一条）。
//!
//! 现在面板是活动区的另一种样子：画在输入框的位置上（`LiveReplTail::turn_panel`），回合
//! 循环照常收事件、照常往上面的正文里画，按键先交给面板。面板收掉时交出一个结果，这里把
//! 它落地（换模型、换会话、删会话）。渲染器始终是同一个，所以也没有接缝。
//!
//! 她反问时弹的提问面板（`question_flow::QuestionLayer`）也是这样开的。
//!
//! 两条泵（自己起的轮 `remote::one_shot`、挂上去跟的轮 `wake`）共用这一份。只有全屏
//! TUI 有面板，行内 REPL 仍走分离。

use crate::cli::repl::panel::{PanelFrame, PanelModel};
use crate::cli::repl::pickers::FuzzyList;
use crate::cli::repl::question_flow::QuestionLayer;
use crate::cli::repl::session_picker::SessionPicker;
use crate::cli::repl::tail::*;
use crate::cli::*;

/// 面板收掉之后回合循环该怎么走。
pub(in crate::cli) enum HostedPanel {
    /// 留在这一轮上接着看（取消、或 `/models` 已就地落地）。
    Stayed,
    /// `/session` 挑了另一条会话：回合循环收口，把它带回 `RemoteRepl` 去切。
    SwitchSession(ipc::SessionState),
}

/// 回合里开着的面板。
pub(in crate::cli) enum TurnPanel {
    Models {
        list: FuzzyList,
        menu: SessionModelMenu,
    },
    Session {
        picker: SessionPicker,
    },
    Question(QuestionLayer),
}

/// 面板收掉时交出来的结果。
pub(in crate::cli) enum PanelDone {
    /// `None` = 按了 Ctrl+C，什么都不改。
    Models(Option<Vec<bool>>),
    Session(SessionPick),
    Question(miyu_base::question::QuestionResponse),
}

/// 回合循环手里、落地面板结果时要用的东西：提问面板答完要记进这一步、回发给 daemon、
/// 把 herdr 侧栏报回 working。
pub(in crate::cli) struct TurnScope<'a> {
    pub(in crate::cli) session_id: &'a str,
    pub(in crate::cli) run_id: &'a str,
    pub(in crate::cli) renderer: &'a mut render::StreamRenderer,
    pub(in crate::cli) herdr: Option<&'a crate::cli::repl::herdr::TurnGuard>,
}

impl TurnPanel {
    /// 行首那根竖条：选择面板跟输入框一个样子，提问面板是它自己那根。
    pub(in crate::cli) fn bar(&self, mode: PersonaLane) -> String {
        match self {
            Self::Question(_) => QuestionLayer::bar(),
            _ => input_prompt_bar(mode),
        }
    }

    /// 光标在面板第几行、第几列（列从行首竖条算起）。只有在打自定义答案时有。
    pub(in crate::cli) fn cursor(&self) -> Option<(usize, usize)> {
        match self {
            Self::Question(layer) => layer.cursor(),
            _ => None,
        }
    }

    pub(in crate::cli) fn paste(&mut self, text: &str) {
        if let Self::Question(layer) = self {
            layer.paste(text);
        }
    }
}

/// 也是一个普通面板：回合在面板开着时跑完了，剩下的就交给空闲时那套同步面板接着挑
/// （`RemoteRepl::finish_turn_panel`）。
impl PanelModel for TurnPanel {
    type Output = PanelDone;

    fn desired_rows(&self) -> u16 {
        match self {
            Self::Models { list, .. } => list.desired_rows(),
            Self::Session { picker } => picker.desired_rows(),
            Self::Question(layer) => layer.desired_rows(),
        }
    }

    fn content(&mut self, frame: &PanelFrame) -> Vec<String> {
        match self {
            Self::Models { list, .. } => list.content(frame),
            Self::Session { picker } => picker.content(frame),
            Self::Question(layer) => layer.content(frame.width, frame.panel.rows),
        }
    }

    fn on_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> Option<PanelDone> {
        match self {
            Self::Models { list, .. } => list.on_key(code, modifiers).map(PanelDone::Models),
            Self::Session { picker } => picker.on_key(code, modifiers).map(PanelDone::Session),
            Self::Question(layer) => layer.on_key(code, modifiers).map(PanelDone::Question),
        }
    }
}

/// 回合里敲了不带参数的 `/models` / `/session`：在活动区的位置上开面板，回合照跑。
///
/// 调用方要保证是全屏 TUI（`live.screen.is_some()`）。开不出来（没配模型、列不出会话）
/// 就说一句，回合照旧。
pub(in crate::cli) async fn open_turn_panel(
    paths: &MiyuPaths,
    live: &mut LiveReplTail,
    command: ReplSlashCommand,
    session_id: &str,
) -> Result<()> {
    let panel = match command {
        ReplSlashCommand::Models => match models_panel(paths, session_id) {
            Ok(panel) => panel,
            Err(error) => {
                repl_note(live, &format!("\x1b[31m{error:#}\x1b[0m\n"))?;
                return Ok(());
            }
        },
        ReplSlashCommand::Session => {
            let mode = live.mode();
            let Some(entries) = session_picker_entries(paths, live, mode).await? else {
                return Ok(());
            };
            TurnPanel::Session {
                picker: SessionPicker::new(entries, &picker_home(live, session_id), None),
            }
        }
        _ => return Ok(()),
    };
    live.open_turn_panel(panel)
}

/// `/session` 列表里哪一行算「自己」：切进子代理会话看的时候是这棵树的根（子会话不进列表，
/// 09-25），同 `RemoteRepl::picker_home`。
fn picker_home(live: &LiveReplTail, session_id: &str) -> String {
    live.visits
        .first()
        .map(|root| root.session_id.clone())
        .unwrap_or_else(|| session_id.to_string())
}

fn models_panel(paths: &MiyuPaths, session_id: &str) -> Result<TurnPanel> {
    let config = AppConfig::load(paths)?;
    let choices = config.text_provider_model_choices();
    if choices.is_empty() {
        bail!(
            "{}",
            t(
                "no configured provider models; configure a model first",
                "没有已配置的 provider 模型；请先配置模型",
            )
        );
    }
    let menu = SessionModelMenu::new(&config, choices, paths, Some(session_id))?;
    let list = FuzzyList::multi(
        t("Select model", "选择模型"),
        &menu.labels,
        menu.initial.clone(),
        Some(Box::new(menu.toggle_rule())),
    );
    Ok(TurnPanel::Models { list, menu })
}

/// 面板开着时来了一个事件。面板收掉了就把结果落地，告诉回合循环接下来怎么走；`None` =
/// 面板还开着，或者这一下不归面板（鼠标、窗口变化照旧交给回合循环）。
pub(in crate::cli) async fn turn_panel_event(
    paths: &MiyuPaths,
    live: &mut LiveReplTail,
    event: &Event,
    scope: &mut TurnScope<'_>,
) -> Result<Option<HostedPanel>> {
    let Some(done) = live.turn_panel_event(event)? else {
        return Ok(None);
    };
    let Some(panel) = live.close_turn_panel()? else {
        return Ok(None);
    };
    Ok(Some(match (panel, done) {
        (TurnPanel::Question(layer), PanelDone::Question(asked)) => {
            crate::cli::repl::question_flow::answer_question_layer(
                paths,
                live,
                scope.renderer,
                scope.run_id,
                scope.herdr,
                layer,
                asked,
            )
            .await?;
            HostedPanel::Stayed
        }
        (panel, done) => finish_turn_panel(paths, live, panel, done, scope.session_id).await?,
    }))
}

/// 面板交出结果之后把它落地：换模型、换会话、删会话。回合跑完了面板还开着时，空闲那边
/// 挑完也走这儿；提问面板那时候已经没人等了，不落地。
pub(in crate::cli) async fn finish_turn_panel(
    paths: &MiyuPaths,
    live: &mut LiveReplTail,
    panel: TurnPanel,
    done: PanelDone,
    session_id: &str,
) -> Result<HostedPanel> {
    Ok(match (panel, done) {
        (TurnPanel::Models { menu, .. }, PanelDone::Models(Some(active))) => {
            apply_models(paths, live, &menu, active, session_id).await?;
            HostedPanel::Stayed
        }
        (TurnPanel::Session { .. }, PanelDone::Session(pick)) => {
            session_picked(paths, live, pick, session_id).await?
        }
        _ => HostedPanel::Stayed,
    })
}

/// `/models` 面板的结果落成会话覆盖，footer 当场换模型标签。和空闲时 `cmd_models` 的
/// 全屏路一个流程。出错不往外抛：那是这条命令的事，不该把正在看的这一轮打断。
async fn apply_models(
    paths: &MiyuPaths,
    live: &mut LiveReplTail,
    menu: &SessionModelMenu,
    active: Vec<bool>,
    session_id: &str,
) -> Result<()> {
    let (changed, message) = match menu.apply(paths, Some(session_id), active).await {
        Ok(outcome) => outcome,
        Err(error) => {
            repl_note(live, &format!("\x1b[31m{error:#}\x1b[0m\n"))?;
            return Ok(());
        }
    };
    repl_note(live, &format!("\x1b[2m{message}\x1b[0m\n"))?;
    if !changed {
        return Ok(());
    }
    // footer 上的模型标签当场换掉（空闲时 `/models` 也是选完就换）。回合还在
    // 跑：运行转轮与右上角的目标提示都是 footer 上的活字段，重算出来的那份
    // 没有它们，得从旧的搬过来，不然转轮熄一下、提示闪一下。
    let config = AppConfig::load_or_default(paths)?;
    let (mut footer, _) = session_footer_status(paths, &config, live, session_id).await?;
    footer.goal = live.footer.goal.clone();
    footer.running_spinner = live.footer.running_spinner;
    live.refresh_footer(footer)?;
    // `RemoteRepl` 手里那份 footer 还是旧模型，回合一结束会盖回来；让它先重算。
    live.session_footer_stale = true;
    repl_note(
        live,
        &format!(
            "\x1b[2m{}\x1b[0m\n",
            t(
                "session model updated; takes effect from the next turn",
                "会话模型已更新，下一轮生效"
            )
        ),
    )?;
    Ok(())
}

/// `/session` 面板挑完了：切过去（交给 `RemoteRepl`）、或者删一条再把面板开回来，光标停在
/// 原位。删掉的是自己这条就落到本车道的一条可用会话上——和空闲时 `repl_pick_session` 一个
/// 规矩。
async fn session_picked(
    paths: &MiyuPaths,
    live: &mut LiveReplTail,
    pick: SessionPick,
    session_id: &str,
) -> Result<HostedPanel> {
    let mode = live.mode();
    match pick {
        SessionPick::Cancelled => Ok(HostedPanel::Stayed),
        SessionPick::Switch(target) => Ok(
            match repl_get_session_switch(paths, live, target, session_id).await? {
                Some(state) => HostedPanel::SwitchSession(state),
                None => HostedPanel::Stayed,
            },
        ),
        SessionPick::Delete {
            session_id: deleted,
            index,
        } => {
            let home = picker_home(live, session_id);
            let was_active = deleted == home;
            let notice = delete_session_from_picker(paths, live, deleted)
                .await?
                .err();
            // 删掉的是自己这条（或者正在看的子代理树的根）：落到兜底会话上，切完在原位把
            // 面板开回来接着删（09-25）。
            if notice.is_none() && was_active {
                return Ok(
                    match repl_fallback_session_state(paths, live, mode).await? {
                        Some(state) => {
                            live.reopen_session_picker = Some(index);
                            HostedPanel::SwitchSession(state)
                        }
                        None => HostedPanel::Stayed,
                    },
                );
            }
            // 删掉了别的、或者没删成（原因显示在面板里）：原位把面板开回来。
            if let Some(entries) = session_picker_entries(paths, live, mode).await? {
                live.open_turn_panel(TurnPanel::Session {
                    picker: SessionPicker::new(entries, &home, Some(index)).with_notice(notice),
                })?;
            }
            Ok(HostedPanel::Stayed)
        }
    }
}

/// 回合中排队执行的命令（`DuringTurn::Queue`，目前只有 `/compact`）：发给守护进程。回合还在跑
/// 就排进去、排队区挂一行；恰好刚跑完的话守护进程当场做了，什么都不用挂；出错说一句。
pub(in crate::cli) async fn queue_turn_command(
    paths: &MiyuPaths,
    live_tail: &mut LiveReplTail,
    command: miyu_core::slash_commands::ReplSlashCommand,
    session_id: &str,
) -> Result<()> {
    let target = miyu_core::ipc::SessionRef::Id {
        id: session_id.to_string(),
    };
    let request = match command {
        miyu_core::slash_commands::ReplSlashCommand::Compact => IpcCommand::Compact { target },
        _ => return Ok(()),
    };
    match send_ipc_admin(paths, request).await {
        Ok((_, data)) if data.get("queued").and_then(serde_json::Value::as_bool) == Some(true) => {
            let marker = crate::cli::repl::tail::queued_compact_marker();
            if live_tail.external_output_active {
                live_tail.append_queued(marker);
            } else {
                synchronized_terminal_update(CursorAfterUpdate::Preserve, || {
                    live_tail.enqueue(marker)
                })?;
            }
        }
        Ok(_) => {}
        Err(error) => {
            live_tail.toast_note(&format!("{error:#}"));
            if !live_tail.external_output_active {
                synchronized_terminal_update(CursorAfterUpdate::Preserve, || live_tail.redraw())?;
            }
        }
    }
    Ok(())
}
