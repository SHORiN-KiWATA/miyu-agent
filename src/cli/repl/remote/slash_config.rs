//! 远端 REPL 的帮助 / 听写 / 历史 / 清屏 / 用量与配置类斜杠命令(/persona /models /config /effort)。09-17 从 `run_remote_repl` 抽出。

use super::interactive::{LoopStep, RemoteRepl};
use crate::cli::repl::tail::*;
use crate::cli::*;

impl RemoteRepl {
    pub(super) async fn cmd_help(&mut self) -> Result<LoopStep> {
        // 走缓冲而不是 `println!`：全屏下直接打 stdout 的字节不在
        // 缓冲里，下一帧重画就没了，回翻也找不到。
        repl_note(
            &mut self.live_repl,
            crate::cli::repl::commands::repl_help_text().trim_end(),
        )?;
        Ok(LoopStep::Continue)
    }

    pub(super) async fn cmd_stt(&mut self) -> Result<LoopStep> {
        if crate::cli::repl::dictation::is_active() {
            crate::cli::repl::dictation::stop();
            repl_note(
                &mut self.live_repl,
                &format!("\x1b[2m{}\x1b[0m\n", t("dictation stopped", "听写已停止")),
            )?;
        } else if crate::cli::repl::dictation::start(
            &self.paths,
            self.config.voice.dictation_auto_submit,
        ) {
            repl_note(
                &mut self.live_repl,
                &format!(
                    "\x1b[2m{}\x1b[0m\n",
                    t(
                        "listening… speak; Esc stops, Enter sends (10s of silence also ends it)",
                        "在听…请讲;Esc 停止,回车发送(静默 10 秒也会自动结束)"
                    )
                ),
            )?;
        }
        Ok(LoopStep::Continue)
    }

    pub(super) async fn cmd_history(&mut self) -> Result<LoopStep> {
        let state = StateStore::new(&self.paths)?.pinned(&self.active_session_id);
        run_history_with_state(
            &state,
            HistoryArgs {
                limit: 20,
                raw: false,
                no_thinking: false,
            },
        )?;
        Ok(LoopStep::Continue)
    }

    pub(super) async fn cmd_usage(&mut self) -> Result<LoopStep> {
        let snapshot = StateStore::new(&self.paths)?.usage_snapshot()?;
        let usage = self.footer.token_usage;
        let context = Some((usage.session_tokens, usage.context_window));
        repl_note(
            &mut self.live_repl,
            &format!("{}\n\n", usage_overview_text(&snapshot, context)),
        )?;
        Ok(LoopStep::Continue)
    }

    pub(super) async fn cmd_persona(&mut self, command_args: &str) -> Result<LoopStep> {
        // 全屏：面板贴在大厅提示下方 / 会话正文底部，结果走缓冲；行内还是老的 println 路。
        let result = if self.live_repl.screen.is_some() {
            self.pick_persona_fullscreen(command_args)
        } else {
            run_persona_picker(&self.paths, command_args)
        };
        match result {
            Ok(true) => {
                let _ = repl_ipc_admin(&self.paths, &mut self.live_repl, IpcCommand::ReloadConfig)
                    .await;
                self.config = AppConfig::load(&self.paths)?;
                // 人格是会话的命名空间维度:切人格后 daemon 的当前会话
                // 指针已经指向新人格的会话,前端必须重取并把
                // active_session_id / history / footer 一起换过去。
                // 漏了这步(09-01 前的实现)会让前端还挂在旧人格的
                // 会话上等事件流,而 daemon 在新人格语境里跑,消息发出
                // 去永远等不到回执——UI 卡死在「加载中 0ms」。
                let (daemon_state, _) = await_in_lobby(
                    &mut self.live_repl,
                    send_ipc_admin(
                        &self.paths,
                        IpcCommand::GetReplSession {
                            mode: self.mode.is_dev().then(|| "dev".to_string()),
                            // 换人格后重取当前会话,不是启动。
                            fresh: false,
                        },
                    ),
                )
                .await?;
                apply_repl_session_switch(
                    &self.paths,
                    &self.config,
                    self.mode,
                    &daemon_state,
                    &mut self.active_session_id,
                    &mut self.history,
                    &mut self.live_repl,
                    &mut self.footer,
                    &mut self.cumulative_tokens,
                )
                .await?;
                // 换过去那条会话要是有正在跑的回合，挂上去跟着看——换会话的
                // 回放不收正在跑的轮，不挂的话那一轮在屏幕上就没了（09-20，
                // 和 `/dev` `/normal` 同一个毛病）。
                self.follow_active_run_here().await?;
                repl_note(
                    &mut self.live_repl,
                    &format!("{}\n", t("configuration reloaded", "配置已重新加载")),
                )?;
            }
            Ok(false) => {}
            Err(error) => repl_note(&mut self.live_repl, &format!("\x1b[31m{error:#}\x1b[0m\n"))?,
        }
        Ok(LoopStep::Continue)
    }

    /// 全屏下的 /persona：面板版菜单，结果走缓冲而不是 println（全屏下直接打
    /// stdout 的字节不在缓冲里，下一帧重画就没了）。
    fn pick_persona_fullscreen(&mut self, command_args: &str) -> Result<bool> {
        let choices = PersonaChoices::load(&self.paths)?;
        let argument = command_args.trim();
        let target = if !argument.is_empty() {
            choices.resolve(argument)?
        } else {
            let (items, initial) = choices.menu();
            let Some(target) = pick_single(
                &mut self.live_repl,
                t("Select persona", "选择人格"),
                &items,
                initial,
            )?
            .and_then(|index| choices.at_menu_index(index)) else {
                return Ok(false);
            };
            target
        };
        let (changed, message) = choices.apply(&self.paths, target)?;
        repl_note(&mut self.live_repl, &format!("{message}\n"))?;
        Ok(changed)
    }

    pub(super) async fn cmd_models(&mut self, command_args: &str) -> Result<LoopStep> {
        // Switches this session's pinned model; the change takes
        // effect from the next turn without a daemon reload.
        let argument = command_args.trim();
        let result = if self.live_repl.screen.is_some() && argument.is_empty() {
            // 全屏：菜单走面板（大厅贴提示下方、会话贴正文底部），结果走缓冲。
            self.pick_models_fullscreen().await
        } else {
            // 选择器与它的结果行都是直接往 stdout 打的(println):活动区
            // 还挂着时它们会落在输入框下面、活动区也不知道多了几行,
            // 于是「已恢复跟随全局」孤零零留在输入框底下。先收起活动区,
            // 打完再按真实光标位置重新挂回去。
            synchronized_terminal_update(CursorAfterUpdate::Shown, || self.live_repl.suspend())?;
            let result = run_models_for_session(
                &self.paths,
                parse_models_argument(argument),
                Some(&self.active_session_id),
            )
            .await;
            synchronized_terminal_update(CursorAfterUpdate::Shown, || self.live_repl.resume())?;
            result
        };
        let changed = match result {
            Ok(changed) => changed,
            Err(error) => {
                repl_note(&mut self.live_repl, &format!("\x1b[31m{error:#}\x1b[0m\n"))?;
                return Ok(LoopStep::Continue);
            }
        };
        let (footer, cumulative) = session_footer_status(
            &self.paths,
            &self.config,
            &mut self.live_repl,
            &self.active_session_id,
        )
        .await?;
        self.cumulative_tokens = cumulative;
        self.footer = footer;
        // 重绘着推进去:set_footer 只换数据不画,后面那条提示走的
        // 输出帧也不重画 footer 行,于是模型标签要等下一次按键才换
        // (09-10 用户截图:提示已说「已更新」,footer 仍是旧模型)。
        self.live_repl.refresh_footer(self.footer.clone())?;
        // Esc 退出选择器时什么都没改，这句"已更新"就是假消息
        //（用户实测）。选择器自己该说的话它已经说过了。
        if changed {
            repl_note(
                &mut self.live_repl,
                &format!(
                    "\x1b[2m{}\x1b[0m\n",
                    t(
                        "session model updated; takes effect from the next turn",
                        "会话模型已更新，下一轮生效"
                    )
                ),
            )?;
        }
        Ok(LoopStep::Continue)
    }

    /// 全屏下不带参数的 /models：面板多选。返回真的改了没。
    ///
    /// 流程本体在 `pickers::pick_models_panel`：回合跑着时寄宿的那条路
    /// （`midturn_panel`，09-20）也是它，这里只是把 `RemoteRepl` 的字段递过去。
    async fn pick_models_fullscreen(&mut self) -> Result<bool> {
        pick_models_panel(&self.paths, &mut self.live_repl, &self.active_session_id).await
    }

    pub(super) async fn cmd_config(&mut self) -> Result<LoopStep> {
        crate::config_tui::run(&self.paths)?;
        // 设置界面退出时画面原样留着、光标藏着：在一个同步块里把 REPL
        // 整屏画回来，光标直接出现在输入框，中间不经过左上角。
        if crate::cli::in_fullscreen() {
            synchronized_terminal_update(CursorAfterUpdate::Shown, || self.live_repl.resume())?;
        }
        let Some((_, _)) =
            repl_ipc_admin(&self.paths, &mut self.live_repl, IpcCommand::ReloadConfig).await?
        else {
            return Ok(LoopStep::Continue);
        };
        let refreshed = AppConfig::load(&self.paths)?;
        self.config = refreshed;
        let (state, changed) = await_in_lobby(
            &mut self.live_repl,
            repl_active_or_default_state(&self.paths, &self.active_session_id),
        )
        .await?;
        if changed {
            apply_repl_session_switch(
                &self.paths,
                &self.config,
                self.mode,
                &state,
                &mut self.active_session_id,
                &mut self.history,
                &mut self.live_repl,
                &mut self.footer,
                &mut self.cumulative_tokens,
            )
            .await?;
        }
        self.cumulative_tokens = state_cumulative(&state);
        // 同源约束(验收#23):标签与思考程度都取会话作用域配置。
        let session_config =
            footer_config_for_session(&self.paths, &self.config, &self.active_session_id);
        self.footer = ReplFooterStatus::from_config(
            &session_config,
            state.context_tokens,
            self.cumulative_tokens,
        );
        let client = OpenAiCompatibleClient::from_config(&session_config, &self.paths)?;
        let thinking_summary = client.thinking_variant_summary();
        self.footer
            .update_thinking_variant(thinking_summary.as_deref());
        self.footer
            .update_context_window(state.context_window, state.context_window_assumed);
        self.live_repl.set_footer(self.footer.clone());
        repl_note(
            &mut self.live_repl,
            &format!("{}\n", t("configuration reloaded", "配置已重新加载")),
        )?;
        Ok(LoopStep::Continue)
    }

    pub(super) async fn cmd_effort(&mut self, command_args: &str) -> Result<LoopStep> {
        if !miyu_base::models_cache::is_loaded() {
            repl_note(
                &mut self.live_repl,
                &format!(
                    "{}\n",
                    t(
                        "model metadata is still loading; try /effort again shortly",
                        "模型元数据仍在加载，请稍后重试 /effort"
                    )
                ),
            )?;
            return Ok(LoopStep::Continue);
        }
        let selected = command_args.trim();
        // 档位跟着**这个会话正在用的模型**走，不是全局文本模型：会话用 /models
        // 钉了别的模型时，以前这里拿全局配置建 client，列出来的是全局那个模型的
        // 档位——全局模型只有 default 就只剩一个 default，怎么切都切不到会话模型
        // 的档位上（用户实测）。footer 与 /models 早就按会话作用域取配置，这里对齐。
        // 存盘按「供应商 + 模型」记，daemon 下一轮按会话模型回读，改完就生效。
        let session_config =
            footer_config_for_session(&self.paths, &self.config, &self.active_session_id);
        let mut client = OpenAiCompatibleClient::from_config(&session_config, &self.paths)?;
        // 全屏走面板（大厅贴提示下方、会话贴正文底部）；行内还是光标处的老菜单。
        let fullscreen = self.live_repl.screen.is_some();
        match execute_variant(
            &self.paths,
            &mut client,
            (!selected.is_empty()).then_some(selected),
            "/effort",
            |options| {
                if fullscreen {
                    pick_effort(&mut self.live_repl, options)
                } else {
                    inline_variant_select(options)
                }
            },
        )? {
            VariantOutcome::Updated => {
                let Some((_, _)) =
                    repl_ipc_admin(&self.paths, &mut self.live_repl, IpcCommand::ReloadConfig)
                        .await?
                else {
                    return Ok(LoopStep::Continue);
                };
                self.config = AppConfig::load(&self.paths)?;
                let (state, changed) = await_in_lobby(
                    &mut self.live_repl,
                    repl_active_or_default_state(&self.paths, &self.active_session_id),
                )
                .await?;
                if changed {
                    apply_repl_session_switch(
                        &self.paths,
                        &self.config,
                        self.mode,
                        &state,
                        &mut self.active_session_id,
                        &mut self.history,
                        &mut self.live_repl,
                        &mut self.footer,
                        &mut self.cumulative_tokens,
                    )
                    .await?;
                }
                self.cumulative_tokens = state_cumulative(&state);
                self.footer = ReplFooterStatus::from_config(
                    &footer_config_for_session(&self.paths, &self.config, &self.active_session_id),
                    state.context_tokens,
                    self.cumulative_tokens,
                );
                let thinking_summary = client.thinking_variant_summary();
                self.footer
                    .update_thinking_variant(thinking_summary.as_deref());
                self.footer
                    .update_context_window(state.context_window, state.context_window_assumed);
                self.live_repl.set_footer(self.footer.clone());
                repl_note(
                    &mut self.live_repl,
                    &format!("{}\n", t("thinking variants updated", "已更新思考档位")),
                )?;
            }
            VariantOutcome::Cancelled => {}
            VariantOutcome::Rejected(message) => {
                repl_note(&mut self.live_repl, &format!("\x1b[31m{message}\x1b[0m"))?;
            }
        }
        Ok(LoopStep::Continue)
    }
}
