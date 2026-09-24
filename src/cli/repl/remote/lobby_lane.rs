//! 空会话（大厅）里的车道：按 Tab 只换显示，真要用会话时才向后端要。
//!
//! 用户 09-23：「切换模式不就只是切换一个前端显示吗？后端在发送的时候才会定这个
//! 会话的模式？」原来每按一次 Tab 都真的换一次会话：退出 raw 模式、两三次 IPC、
//! daemon 为那条车道现造 agent 估上下文、客户端读库重建客户端、整屏重画三遍——
//! 慢，中间敲的键还会被终端回显（光标在输入框里左右晃）。大厅里还什么都没有，
//! 没有东西需要后端知道。
//!
//! 所以 Tab 只改显示（配色、模式行、footer），会话留在原车道上、记一面旗；发消息、
//! 敲命令、切只读之前（`materialize_lobby_lane`）才把会话换到显示的那条车道。

use super::interactive::RemoteRepl;
use crate::cli::*;

/// 两条车道各自最近一次知道的空会话上下文（系统提示词 + 工具表的词元数）。
/// 换过去时有数就显示它，没数显示「—」——不拿另一条车道的数冒充。
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct LaneContext {
    normal: Option<u64>,
    dev: Option<u64>,
}

impl LaneContext {
    pub(super) fn get(&self, mode: PersonaLane) -> Option<u64> {
        if mode.is_dev() {
            self.dev
        } else {
            self.normal
        }
    }

    pub(super) fn set(&mut self, mode: PersonaLane, tokens: u64) {
        if mode.is_dev() {
            self.dev = Some(tokens);
        } else {
            self.normal = Some(tokens);
        }
    }
}

fn other_lane(mode: PersonaLane) -> PersonaLane {
    match mode {
        PersonaLane::Active => PersonaLane::Dev,
        PersonaLane::Dev => PersonaLane::Active,
    }
}

impl RemoteRepl {
    /// 大厅里按了 Tab：只换显示，一帧画完。
    pub(super) fn toggle_lobby_lane(&mut self, next: PersonaLane) -> Result<()> {
        // 从会话本来的车道换走：先记下它此刻的读数，换回来原样显示。
        if !self.live_repl.lobby_lane_pending {
            if let Some(tokens) = self.footer.session_tokens() {
                self.lane_context.set(self.mode, tokens);
            }
        }
        self.show_lobby_lane(next)?;
        // 两条车道来回切：按偶数次就回到了会话本来的车道，旗随之落下。
        self.live_repl.lobby_lane_pending = !self.live_repl.lobby_lane_pending;
        Ok(())
    }

    /// 大厅里换到的车道正显示「—」，而事先那一问后来回来了（输入循环空闲时已经把数
    /// 补到屏上那份 footer 里）：手里这份也收下，免得主循环整份覆盖时又盖回「—」。
    pub(super) fn adopt_lane_baseline(&mut self) {
        if !self.live_repl.lobby_lane_pending || self.footer.session_tokens().is_some() {
            return;
        }
        if let Some(tokens) = self.live_repl.footer.session_tokens() {
            self.footer.update_session_tokens(tokens);
        }
    }

    /// 放弃按 Tab 换的车道，显示回到会话本来的那条（大厅里被别处起的回合接走时用）。
    pub(super) fn revert_lobby_lane(&mut self) -> Result<()> {
        if std::mem::take(&mut self.live_repl.lobby_lane_pending) {
            self.show_lobby_lane(other_lane(self.mode))?;
        }
        Ok(())
    }

    fn show_lobby_lane(&mut self, mode: PersonaLane) -> Result<()> {
        self.mode = mode;
        self.live_repl.set_mode(mode);
        // 先用这条车道上次见过的数，再用事先问好的「开一条新会话时的上下文」，都没有
        //（刚启动那一瞬、老 daemon）才显示「—」。
        match self
            .lane_context
            .get(mode)
            .or_else(|| self.jobs_feed.empty_session_context(mode))
        {
            Some(tokens) => self.footer.update_session_tokens(tokens),
            None => self.footer.mark_session_tokens_unknown(),
        }
        self.live_repl.set_footer(self.footer.clone());
        synchronized_terminal_update(CursorAfterUpdate::Preserve, || self.live_repl.redraw())
    }

    /// 按过 Tab 的话，把会话换到显示的那条车道上。发消息、敲命令、切只读之前调；
    /// 没按过（或按了偶数次）就什么都不做。返回 `false` = 没换成（错误已经显示
    /// 过了），这一次输入就此作罢。
    pub(super) async fn materialize_lobby_lane(&mut self) -> Result<bool> {
        if !self.live_repl.lobby_lane_pending {
            return Ok(true);
        }
        let Some((state, _)) = repl_ipc_admin(
            &self.paths,
            &mut self.live_repl,
            IpcCommand::GetReplSession {
                mode: self.mode.is_dev().then(|| "dev".to_string()),
                // 那条车道指针上的会话本来就空就复用，不空就新开（`fresh_repl_session`），
                // 同时把车道指针钉过去——和原来按 Tab 时一样，只是推迟到了现在。
                fresh: true,
            },
        )
        .await?
        else {
            return Ok(false);
        };
        self.live_repl.lobby_lane_pending = false;
        self.active_session_id = state.session_id.clone();
        self.jobs_shared.set_repl_session(&self.active_session_id);
        self.live_repl.set_readonly(state.sandbox_readonly);
        self.lane_context.set(self.mode, state.context_tokens);
        // footer 按这条会话自己的作用域重算：它可能钉了模型（和换会话同一套）。
        self.cumulative_tokens = state_cumulative(&state);
        let session_config =
            footer_config_for_session(&self.paths, &self.config, &state.session_id);
        let mut footer = ReplFooterStatus::from_config(
            &session_config,
            state.context_tokens,
            self.cumulative_tokens,
        );
        let client = OpenAiCompatibleClient::from_config(&session_config, &self.paths)?;
        footer.update_thinking_variant(client.thinking_variant_summary().as_deref());
        footer.update_context_window(state.context_window, state.context_window_assumed);
        self.footer = footer;
        self.live_repl.set_footer(self.footer.clone());
        Ok(true)
    }
}
