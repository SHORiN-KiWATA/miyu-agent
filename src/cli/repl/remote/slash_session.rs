//! 远端 REPL 的会话类斜杠命令:/new /session /rename /delete /sandbox /goal。09-17 从 `run_remote_repl` 抽出。

use super::interactive::{LoopStep, RemoteRepl};
use crate::cli::*;

impl RemoteRepl {
    pub(super) async fn cmd_new(&mut self, command_args: &str) -> Result<LoopStep> {
        let name = command_args.trim();
        // 当前这条本来就是白纸：不新建（用户 09-20 拍板）。
        //
        // 规则和启动（`miyu` / `miyu dev`）、`/dev new` 一致，统一成一句话：
        // **空会话永远只有一条**。原来 `/new` 无条件新建，是唯一会攒空会话的
        // 口子——连敲两次不说话就多两条，列表里全是「新会话」。
        // 带名字就把当前这条改名，你的意图不丢。
        if session_is_empty(&self.paths, &self.active_session_id) {
            if !name.is_empty() {
                let renamed = repl_ipc_admin(
                    &self.paths,
                    &mut self.live_repl,
                    IpcCommand::RenameSession {
                        target: miyu_core::ipc::SessionRef::Id {
                            id: self.active_session_id.clone(),
                        },
                        name: name.to_string(),
                    },
                )
                .await?;
                if renamed.is_none() {
                    return Ok(LoopStep::Continue);
                }
            }
            repl_note(
                &mut self.live_repl,
                &format!(
                    "\x1b[2m{}\x1b[0m\n",
                    t("already on a new session", "已经是一条新会话了")
                ),
            )?;
            return Ok(LoopStep::Continue);
        }
        let Some((_, data)) = repl_ipc_admin(
            &self.paths,
            &mut self.live_repl,
            IpcCommand::CreateSession {
                name: (!name.is_empty()).then(|| name.to_string()),
                switch: false,
                kind: None,
                mode: self.mode.is_dev().then(|| "dev".to_string()),
            },
        )
        .await?
        else {
            return Ok(LoopStep::Continue);
        };
        let Some(session_id) = data
            .get("session")
            .and_then(|session| session.get("session_id"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
        else {
            repl_note(
                &mut self.live_repl,
                &format!(
                    "\x1b[31m{}\x1b[0m\n",
                    t("created session has no id", "新会话缺少 ID")
                ),
            )?;
            return Ok(LoopStep::Continue);
        };
        let Some((state, _)) = repl_ipc_admin(
            &self.paths,
            &mut self.live_repl,
            IpcCommand::GetSessionState {
                target: miyu_core::ipc::SessionRef::Id { id: session_id },
                cwd: std::env::current_dir().ok(),
            },
        )
        .await?
        else {
            return Ok(LoopStep::Continue);
        };
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
        Ok(LoopStep::Continue)
    }

    /// `/dev` 与 `/normal`：去那条车道上的会话（车道指针指向的那条），**不是**
    /// 把当前会话改成另一个模式：模式钉在会话上（daemon `turn_mode_for_session`），
    /// 换模式就是换会话。所以它不受 Tab 那条「空会话才能换车道」的限制。已经在
    /// 那条车道上就只提示一句。
    ///
    /// 加 `new`（`/dev new` / `/normal new`，用户 09-20 要的）就**开一条新的**，
    /// 语义和 `miyu dev` / `miyu` 启动完全一样（`fresh`：那条车道指针指的会话
    /// 本来就空就原地用，不然新建）——一条没说过话的会话和一条新建的会话用户
    /// 分不出来，但会话列表分得出来。已经在那条车道上时 `new` 照做，不再只
    /// 提示一句：你要的就是新会话，跟人在哪条车道没关系。
    pub(super) async fn cmd_lane(&mut self, lane: PersonaLane, args: &str) -> Result<LoopStep> {
        let args = args.trim();
        let fresh = args.eq_ignore_ascii_case("new");
        if !args.is_empty() && !fresh {
            repl_note(
                &mut self.live_repl,
                &format!(
                    "\x1b[2m{}\x1b[0m\n",
                    t(
                        "only `new` is accepted here (start a fresh session)",
                        "这里只认 new（开一条新会话）"
                    )
                ),
            )?;
            return Ok(LoopStep::Continue);
        }
        if self.mode == lane && !fresh {
            let note = if lane.is_dev() {
                t(
                    "already in dev mode; /normal goes back",
                    "已经在开发模式；/normal 回普通模式",
                )
            } else {
                t(
                    "already in normal mode; /dev goes to the dev lane",
                    "已经在普通模式；/dev 去开发模式",
                )
            };
            repl_note(&mut self.live_repl, &format!("\x1b[2m{note}\x1b[0m\n"))?;
            return Ok(LoopStep::Continue);
        }
        let Some((state, _)) = repl_ipc_admin(
            &self.paths,
            &mut self.live_repl,
            IpcCommand::GetReplSession {
                mode: lane.is_dev().then(|| "dev".to_string()),
                // 不带参数时**保留**「切到那条车道最近用的会话」(用户 09-20
                // 明确要留)；`new` 走和启动同一条「开新会话」的路。
                fresh,
            },
        )
        .await?
        else {
            return Ok(LoopStep::Continue);
        };
        self.switch_to_session(&state).await?;
        Ok(LoopStep::Continue)
    }

    /// 切到一条会话，**车道跟着会话走**：目标是另一侧（普通 ↔ 开发）的会话，
    /// 输入框竖条、banner、footer 一并换过去——不然会话已经跑在 dev 人格上，
    /// 屏幕上还是普通模式的样子（初诊 §五 ③）。老 daemon 不报 `mode` 就留在原车道。
    pub(super) async fn switch_to_session(&mut self, state: &ipc::SessionState) -> Result<()> {
        // 换会话整段攒成一帧：清屏、回放、footer、挂上正在跑的那一轮（09-25）。
        crate::cli::repl::tail::begin_frame_hold();
        let lane = self.lane_of(state);
        if lane != self.mode {
            // 先换色再切：切换的回执行和输入框竖条都按新模式画（和 Tab 换车道一样）。
            self.live_repl.set_mode(lane);
        }
        apply_repl_session_switch(
            &self.paths,
            &self.config,
            lane,
            state,
            &mut self.active_session_id,
            &mut self.history,
            &mut self.live_repl,
            &mut self.footer,
            &mut self.cumulative_tokens,
        )
        .await?;
        self.mode = lane;
        // 任务条/目标提示都按**这个 REPL 的会话**过滤，换了会话要跟着换，
        // 否则状态行上还挂着上一条会话的东西。
        self.jobs_shared.set_repl_session(&self.active_session_id);
        self.follow_pending = true;
        Ok(())
    }

    /// 这条会话的车道：模式钉在会话上。老 daemon 不报 `mode` 就留在原车道。
    pub(super) fn lane_of(&self, state: &ipc::SessionState) -> PersonaLane {
        match state.mode.as_str() {
            "dev" => PersonaLane::Dev,
            "normal" => PersonaLane::Active,
            _ => self.mode,
        }
    }

    /// 切进这条会话之后：它要是有**正在跑**的回合，就从头挂上去跟着看。
    ///
    /// 换会话会先 `wipe_transcript` 再按库回放，而 `session_replay` 只收
    /// `completed` / `interrupted` 的轮——**正在跑的那一轮不在回放里**（它的
    /// 正文还没落库）。不挂回去的话，那一轮连同用户刚说的那句话在屏幕上整个
    /// 消失（用户 09-20 实测：回合跑着时 `/dev` 切走再 `/normal` 切回来，
    /// 「之前说的那句话就看不到了」）。
    ///
    /// 从头挂（`from_start`）而不是接实时：要的就是被回放漏掉的那前半截，
    /// 用户消息由 `turn.started` 的 `display_content` 画出来——和同一个会话
    /// 开第二个 TUI 是同一条路（09-19）。
    pub(super) async fn follow_active_run_here(&mut self) -> Result<()> {
        // 这条路会递归：跟随 → 中途敲命令 → 换会话 → 又挂上去跟随。每次都是
        // 用户亲手切一次，正常用不了几层；加道闸免得哪天循环起来。
        if self.follow_depth >= 8 {
            return Ok(());
        }
        // 元组的第四项是 `(run_id, session_id)`：所有活动回合（own 的过滤是
        // 客户端自己做的，这里要的正是自己刚分离掉的那一轮）。
        let Ok((_, _, _, active_runs)) = fetch_jobs_overview(&self.paths).await else {
            return crate::cli::repl::tail::release_frame_hold();
        };
        let session = self.active_session_id.clone();
        let Some((run_id, _)) = active_runs
            .into_iter()
            .find(|(_, run_session)| run_session == &session)
        else {
            // 这条会话没有在跑的：切换画面到这儿就画完了。
            return crate::cli::repl::tail::release_frame_hold();
        };
        // 空闲循环也会认领「同一会话里别人起的轮」：记下来，免得它看完之后又被认领、
        // 从头再画一遍（切进正跑着的子会话最容易撞上）。
        self.jobs_feed.mark_followed(&run_id);
        // `Box::pin`：`follow_run_with_commands` 会走回这里，async fn 的自递归
        // 要装箱才编得过。
        self.follow_depth += 1;
        let outcome = Box::pin(self.follow_run_with_commands(&run_id, "", true, None)).await;
        self.follow_depth -= 1;
        outcome?;
        Ok(())
    }

    pub(super) async fn cmd_session(&mut self, command_args: &str) -> Result<LoopStep> {
        let arg = command_args.trim();
        if arg.is_empty() {
            self.pick_session(None).await?;
            return Ok(LoopStep::Continue);
        }
        let target =
            match resolve_repl_session_target(&self.paths, &mut self.live_repl, self.mode, arg)
                .await?
            {
                Some(target) => target,
                None => return Ok(LoopStep::Continue),
            };
        if let Some(state) = repl_get_session_switch(
            &self.paths,
            &mut self.live_repl,
            target,
            &self.active_session_id,
        )
        .await?
        {
            self.switch_to_session(&state).await?;
        }
        Ok(LoopStep::Continue)
    }

    /// `/session` 面板：挑一条切过去，或者连着删。删掉自己待着的那条会先落到兜底会话上，
    /// 再在原位把面板开回来（09-25）。
    pub(super) async fn pick_session(&mut self, mut cursor: Option<usize>) -> Result<()> {
        loop {
            let home = self.picker_home();
            match repl_pick_session(&self.paths, &mut self.live_repl, self.mode, &home, cursor)
                .await?
            {
                SessionPickOutcome::Stayed => return Ok(()),
                SessionPickOutcome::Switch(state) => return self.switch_to_session(&state).await,
                SessionPickOutcome::Fallback { state, cursor: at } => {
                    self.switch_to_session(&state).await?;
                    cursor = Some(at);
                }
            }
        }
    }

    /// `/session` 列表里哪一行算「自己」：切进子代理会话看的时候是这棵树的根——子会话不进
    /// 列表，原来一个「*」都没有、光标落在第 0 行，而第 0 行往往就是正跑着的主会话（09-25）。
    pub(super) fn picker_home(&self) -> String {
        self.live_repl
            .visits
            .first()
            .map(|root| root.session_id.clone())
            .unwrap_or_else(|| self.active_session_id.clone())
    }

    pub(super) async fn cmd_rename(&mut self, command_args: &str) -> Result<LoopStep> {
        let name = command_args.trim().to_string();
        if name.is_empty() {
            repl_note(
                &mut self.live_repl,
                &format!(
                    "\x1b[2m{}\x1b[0m\n",
                    t("usage: /rename <name>", "用法：/rename <新名称>")
                ),
            )?;
            return Ok(LoopStep::Continue);
        }
        if repl_ipc_admin(
            &self.paths,
            &mut self.live_repl,
            IpcCommand::RenameSession {
                target: miyu_core::ipc::SessionRef::Id {
                    id: self.active_session_id.clone(),
                },
                name: name.clone(),
            },
        )
        .await?
        .is_some()
        {
            repl_note(
                &mut self.live_repl,
                &format!(
                    "\x1b[2m{}: {name}\x1b[0m\n",
                    t("session renamed", "会话已重命名")
                ),
            )?;
        }
        Ok(LoopStep::Continue)
    }

    pub(super) async fn cmd_delete(&mut self, command_args: &str) -> Result<LoopStep> {
        let arg = command_args.trim();
        let target = if arg.is_empty() {
            miyu_core::ipc::SessionRef::Id {
                id: self.active_session_id.clone(),
            }
        } else {
            match resolve_repl_session_target(&self.paths, &mut self.live_repl, self.mode, arg)
                .await?
            {
                Some(target) => target,
                None => return Ok(LoopStep::Continue),
            }
        };
        let Some(target_state) =
            repl_get_session_state(&self.paths, &mut self.live_repl, target).await?
        else {
            return Ok(LoopStep::Continue);
        };
        let deleted_active = target_state.session_id == self.active_session_id;
        if !confirm_inline(
            &mut self.live_repl,
            t(
                "delete this session and all of its self.history?",
                "确认删除该会话及其全部历史？",
            ),
        )? {
            repl_note(
                &mut self.live_repl,
                &format!("\x1b[2m{}\x1b[0m\n", t("cancelled", "已取消")),
            )?;
            return Ok(LoopStep::Continue);
        }
        let Some((_, _)) = repl_ipc_admin(
            &self.paths,
            &mut self.live_repl,
            IpcCommand::DeleteSession {
                target: miyu_core::ipc::SessionRef::Id {
                    id: target_state.session_id,
                },
            },
        )
        .await?
        else {
            return Ok(LoopStep::Continue);
        };
        repl_note(
            &mut self.live_repl,
            &format!("\x1b[2m{}\x1b[0m", t("session deleted", "会话已删除")),
        )?;
        if deleted_active {
            let Some(state) =
                repl_fallback_session_state(&self.paths, &mut self.live_repl, self.mode).await?
            else {
                return Ok(LoopStep::Continue);
            };
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
        Ok(LoopStep::Continue)
    }

    pub(super) async fn cmd_sandbox(&mut self, command_args: &str) -> Result<LoopStep> {
        let (arg, allow_read) =
            miyu_core::slash_commands::take_repl_flag(command_args, "--allow-read");
        if arg.is_empty() && allow_read {
            repl_note(
                &mut self.live_repl,
                &format!(
                    "\x1b[31m{}\x1b[0m\n",
                    t(
                        "usage: /sandbox <path> [--allow-read]",
                        "用法:/sandbox <路径> [--allow-read]"
                    )
                ),
            )?;
            return Ok(LoopStep::Continue);
        }
        if arg.is_empty() {
            let Some(state) = repl_get_session_state(
                &self.paths,
                &mut self.live_repl,
                miyu_core::ipc::SessionRef::Id {
                    id: self.active_session_id.clone(),
                },
            )
            .await?
            else {
                return Ok(LoopStep::Continue);
            };
            // 09-23 起是一张表(根、只读、可写、可读),默认沙盒与只读照实报。
            let note = sandbox_state_table(&state);
            repl_note(&mut self.live_repl, &note)?;
            return Ok(LoopStep::Continue);
        }
        if arg.eq_ignore_ascii_case("clear") {
            if repl_ipc_admin(
                &self.paths,
                &mut self.live_repl,
                IpcCommand::SetSandbox {
                    target: miyu_core::ipc::SessionRef::Id {
                        id: self.active_session_id.clone(),
                    },
                    root: None,
                    allow_read: false,
                },
            )
            .await?
            .is_some()
            {
                // daemon 解绑时把只读一起清了(`set_session_sandbox`),状态行跟上。
                self.live_repl.set_readonly(false);
                repl_note(
                    &mut self.live_repl,
                    &format!("\x1b[2m{}\x1b[0m\n", t("Sandbox unbound.", "已解绑沙盒。")),
                )?;
            }
            return Ok(LoopStep::Continue);
        }
        let path = match std::fs::canonicalize(expand_tilde(arg)) {
            Ok(path) => path,
            Err(error) => {
                repl_note(
                    &mut self.live_repl,
                    &format!(
                        "\x1b[31m{}: {arg} ({error})\x1b[0m\n",
                        t("invalid sandbox path", "无效的沙盒路径")
                    ),
                )?;
                return Ok(LoopStep::Continue);
            }
        };
        if repl_ipc_admin(
            &self.paths,
            &mut self.live_repl,
            IpcCommand::SetSandbox {
                target: miyu_core::ipc::SessionRef::Id {
                    id: self.active_session_id.clone(),
                },
                root: Some(path.clone()),
                allow_read,
            },
        )
        .await?
        .is_some()
        {
            // 绑一个根 = 要在这里写,daemon 把只读清了,状态行跟上。
            self.live_repl.set_readonly(false);
            // 回执跟查看是同一张表(09-23)。读放开是把「防提示注入读走密钥」那一半
            // 关掉,表格下面照样说一句——网络本来就不在 Landlock 管辖内。
            if let Some(state) = repl_get_session_state(
                &self.paths,
                &mut self.live_repl,
                miyu_core::ipc::SessionRef::Id {
                    id: self.active_session_id.clone(),
                },
            )
            .await?
            {
                let mut note = sandbox_state_table(&state);
                if allow_read {
                    note.push_str(&format!(
                        "\x1b[2m{}\x1b[0m\n",
                        t(
                            "~/.ssh and your API keys are readable too.",
                            "~/.ssh 和 API key 也读得到。"
                        )
                    ));
                }
                repl_note(&mut self.live_repl, &note)?;
            }
        }
        Ok(LoopStep::Continue)
    }

    pub(super) async fn cmd_goal(&mut self, command_args: &str) -> Result<LoopStep> {
        // 走 IPC 而不是直连库：目标本身在库里，但「是否自动续跑」
        // 驻在 daemon 内存，REPL 进程自己设那个标记，续轮驱动器
        // 根本看不见。
        let Some((_, data)) = repl_ipc_admin(
            &self.paths,
            &mut self.live_repl,
            IpcCommand::Goal {
                target: miyu_core::ipc::SessionRef::Id {
                    id: self.active_session_id.clone(),
                },
                input: command_args.to_string(),
            },
        )
        .await?
        else {
            return Ok(LoopStep::Continue);
        };
        // 终端里没有 WebUI 那条常驻状态行，所以这里必须回一句
        // ——设完目标到第一轮真正开跑之间有一段静默（驱动器要等
        // 会话空下来），一个字都不说的话，用户只会以为命令没生效。
        // 但也就一句：状态、轮次这些留给 `/goal` 自己去查。
        let text = data
            .get("text")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        // 光敲 `/goal edit` 到不了这里：输入泵在提交前就原地变身
        // 成「/goal edit <当前目标>」（`prefill_goal_edit_input`），
        // 只有没目标时才落进来打提示。
        let summary = if command_args.trim().is_empty() {
            text.to_string()
        } else {
            // 多行的详情压成一句：命令回执不该占半屏。
            text.lines().next().unwrap_or_default().to_string()
        };
        // 输入框右上角那行 `/goal …` 当场跟上：命令的回执只说一句话，状态是否
        // 真的换了（设上了、暂停了、清掉了）要看那行提示变没变。回执里带着命令
        // 执行完之后的目标状态，直接用。
        //
        // 写的是轮询线程那份快照而不是 footer：空闲循环每一拍都拿快照去盖
        // footer，不同步的话刚清掉的目标会自己回来待满一秒。
        let goal = goal_hint_from_admin_data(&data);
        // 目标要开跑了就撤大厅：接下来的续轮会往屏幕上写正文，而大厅那层星空
        // 是盖在正文之上的——不撤的话第一轮跑完了屏幕上还是一片星空，什么都
        // 没有（用户 09-19 实测：空会话里 `/goal` 建目标不退出大厅）。
        //
        // 只在「真的会自己往前跑」时撤：`/goal` 查状态、`/goal pause`、
        // `/goal clear` 都不该把大厅弄没。
        if goal.as_ref().is_some_and(miyu_core::ipc::GoalHint::running) {
            self.live_repl
                .set_session_empty(&self.config, &self.paths, false);
        }
        self.jobs_feed.set_goal(goal.clone());
        self.live_repl.tick_goal_hint(goal)?;
        // 暗色 + 图标：这是系统回执，不是模型正文，得和邻居们
        // （工作目录绑定、后台任务表头）长得一族。单个 \n 收尾，
        // 和它们一致——多一个就空两行。
        repl_note(&mut self.live_repl, &format!("\x1b[2m◎ {summary}\x1b[0m\n"))?;
        Ok(LoopStep::Continue)
    }
}
