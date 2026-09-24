//! 远端 REPL 发一个回合:记历史、走 IPC 跑回合、按结果刷 footer 与后台任务条。
//! 09-17 从 `run_remote_repl` 抽出。

use super::interactive::{LoopStep, RemoteRepl};
use crate::cli::*;

impl RemoteRepl {
    pub(super) async fn submit_chat(
        &mut self,
        input: &str,
        images: &[Option<miyu_base::clipboard::PastedImage>],
        history_entry: &ReplHistoryEntry,
    ) -> Result<LoopStep> {
        push_history_capped(&mut self.history, history_entry.clone());
        self.live_repl.editor.record_history(history_entry.clone());
        persist_repl_history_entry(&self.paths, &self.active_session_id, &history_entry);
        match try_run_remote_chat(
            &self.paths,
            Some(&mut self.live_repl),
            input,
            None,
            false,
            self.mode,
            &images,
            Some(self.active_session_id.clone()),
            Some(&self.jobs_feed),
            None,
        )
        .await
        {
            Ok(Some(summary)) => {
                // 回合中寄宿的 `/models` 改了会话模型（09-20）：先按新模型把
                // 手里这份 footer 重算，再往上叠这一轮的用量，不然下面那次
                // `refresh_footer` 会把旧模型标签盖回去。
                self.adopt_stale_footer().await?;
                self.cumulative_tokens = summary.cumulative_tokens;
                self.footer.update_token_usage(
                    &summary.result,
                    summary.context_tokens,
                    summary.context_window,
                    self.cumulative_tokens,
                );
                // Refresh the job strip right away — a background command
                // spawned this turn must show up without waiting a poll.
                if let Ok((jobs, _, wake_runs, peer_runs)) = fetch_jobs_overview(&self.paths).await
                {
                    let jobs = self.jobs_shared.publish_jobs(jobs);
                    *self.jobs_shared.wake_runs.lock().unwrap() = wake_runs;
                    *self.jobs_shared.peer_runs.lock().unwrap() = peer_runs;
                    self.live_repl.set_jobs(jobs);
                }
                self.live_repl.refresh_footer(self.footer.clone())?;
            }
            Ok(None) => bail!(
                "{}",
                t(
                    "the Miyu Web core stopped; start the REPL again to use direct self.mode",
                    "Miyu Web 核心已停止；请重新启动 REPL 以使用直连模式"
                )
            ),
            // 回合跑着的时候敲了一条要占屏的斜杠命令（用户 09-20）：这一轮
            // 已经分离到后台（daemon 照跑），这里执行命令，然后按事件号挂回来
            // 接着看——已经看过的那半截不会再来一遍。
            Err(err) if take_remote_turn_suspended(&err).is_some() => {
                let suspended = take_remote_turn_suspended(&err).expect("just matched");
                let (action, run_id, last_event_id, session_id) = (
                    suspended.action.clone(),
                    suspended.run_id.clone(),
                    suspended.last_event_id,
                    suspended.session_id.clone(),
                );
                let step = self.perform_suspended_action(action).await?;
                if step == LoopStep::Break {
                    return Ok(step);
                }
                // 命令把会话换走了（`/new` `/session` `/dev` `/normal`）就不挂
                // 回去了：那一轮在 daemon 里继续跑，属于**另一条**会话。
                if self.active_session_id != session_id {
                    return Ok(step);
                }
                // 0 = 一个事件都没看到（命令敲在回合刚起的那一瞬）：走「从头
                // 补」，否则开头那截永远看不到了。
                return self
                    .follow_run_with_commands(
                        &run_id,
                        "",
                        last_event_id == 0,
                        (last_event_id > 0).then_some(last_event_id),
                    )
                    .await;
            }
            Err(err) if is_remote_turn_detached(&err) => {
                let frame = format!(
                    "\x1b[2m{}\x1b[0m\n",
                    t(
                        "exited; the reply keeps running in the daemon",
                        "已退出；回复在 daemon 里继续运行"
                    )
                );
                self.live_repl.apply_output_frame(frame.as_bytes())?;
                return Ok(LoopStep::Break);
            }
            Err(err)
                if is_remote_turn_cancelled(&err)
                    || miyu_base::question::is_question_cancelled(&err) =>
            {
                // 走通知条：「已取消」不是对话内容，几秒之后就不再有意义。
                // 直接塞进正文的话它会贴着第 0 列、还会被前面那个收缩块吃进去
                // ——用户实测「这个已取消怎么不是通知，而是跟 Worked for 一起
                // 是可交互的」说的就是它。
                repl_note(
                    &mut self.live_repl,
                    &format!("\x1b[2m{}\x1b[0m", t("cancelled", "已取消")),
                )?;
                // The interrupted turn still entered the context; refresh the
                // footer from the daemon's post-cancel state.
                //
                // daemon 在「已取消」里就把数带回来了：直接用，和回合正常结束同一个
                // 做法。原来这里还要同步再问一次，daemon 为这条会话现造 agent、重估
                // 上下文，长会话里输入框要等一两秒才能打字（09-23）。
                if let Some(context) = cancelled_context(&err) {
                    self.cumulative_tokens = context.cumulative_tokens;
                    self.footer.update_session_tokens(context.context_tokens);
                    self.footer
                        .update_cumulative_tokens(context.cumulative_tokens);
                    self.live_repl.refresh_footer(self.footer.clone())?;
                } else if let Ok((state, _)) =
                    repl_active_or_default_state(&self.paths, &self.active_session_id).await
                {
                    self.cumulative_tokens = state_cumulative(&state);
                    self.footer.update_session_tokens(state.context_tokens);
                    self.footer
                        .update_cumulative_tokens(state_cumulative(&state));
                    self.footer
                        .update_context_window(state.context_window, state.context_window_assumed);
                    self.live_repl.refresh_footer(self.footer.clone())?;
                }
            }
            Err(err) => {
                let frame = format!("{}\n", crate::cli::repl::session::error_frame(&err));
                self.live_repl.apply_output_frame(frame.as_bytes())?;
                if let Ok((state, true)) =
                    repl_active_or_default_state(&self.paths, &self.active_session_id).await
                {
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
            }
        }
        Ok(LoopStep::Continue)
    }
}
