//! 切进子代理会话看、接着聊，再回来（会话项目第 3 段，09-18 子代理会话化的前端半边）。
//!
//! 访问不动车道指针：画面、footer、历史、只读和真正换会话是同一套（`present_session`），
//! 只是不发 `SetReplSession`（它本来也拒子会话）。回去的路记在 `LiveReplTail::visits`：
//! 任务条第一行「○ 主会话」、footer 上的「子代理 ↳N」、`/back` 都读它。真正换会话
//! （`/new` `/session` …）时清空。

use super::interactive::{LoopStep, RemoteRepl};
use crate::cli::repl::strip::{subagent_rows, ParentRow, StripAction, SubagentRow};
use crate::cli::*;

/// `/subagent` 往下列几层。孙代理没有 subagent 工具，实际最多两层；多留一层防以后放开。
const TREE_DEPTH: usize = 3;
/// `/subagent` 最多列几条。每一条都要问一次它名下的子会话。
const TREE_ROWS: usize = 60;

/// `/subagent` 列表里的一行。
struct TreeRow {
    depth: usize,
    row: SubagentRow,
    /// 从当前会话往下走到它、中间经过的那几层。切进去时一层层压进访问栈，`/back`
    /// 就一层层退回来。
    via: Vec<ParentRow>,
}

impl RemoteRepl {
    /// 任务条上点了会话行（或者时间线上点了子代理那一步）。
    pub(super) async fn perform_strip_action(&mut self, action: StripAction) -> Result<LoopStep> {
        match action {
            StripAction::Visit(session_id) => self.visit_session(Vec::new(), &session_id).await?,
            StripAction::Go { session_id, path } => self.go_to(path, &session_id).await?,
            StripAction::Stay => {}
        }
        // 在任务条上回车切的：光标停到切过去的那一条（切不过去也收掉这次记下的）。
        self.live_repl.apply_strip_refocus();
        Ok(LoopStep::Continue)
    }

    pub(super) async fn cmd_back(&mut self) -> Result<LoopStep> {
        self.leave_visit().await?;
        Ok(LoopStep::Continue)
    }

    /// `/subagent`：这条会话派出的子代理（连同它们的子代理），挑一个切进去。
    pub(super) async fn cmd_subagent(&mut self) -> Result<LoopStep> {
        let tree = subagent_tree(&self.paths, &self.active_session_id).await;
        if tree.is_empty() {
            repl_note(
                &mut self.live_repl,
                &format!(
                    "\x1b[2m{}\x1b[0m\n",
                    t(
                        "this session has not started any subagent",
                        "这条会话还没派出过子代理"
                    )
                ),
            )?;
            return Ok(LoopStep::Continue);
        }
        let items = tree.iter().map(tree_line).collect::<Vec<_>>();
        let title = t("Select subagent", "选择子代理");
        let picked = if self.live_repl.screen.is_some() {
            pick_single(&mut self.live_repl, title, &items, 0)?
        } else {
            synchronized_terminal_update(CursorAfterUpdate::Hidden, || self.live_repl.suspend())?;
            let picked = inline_single_select_deletable(title, &items, &items, 0, None);
            synchronized_terminal_update(CursorAfterUpdate::Shown, || self.live_repl.resume())?;
            match picked? {
                InlineSelectOutcome::Chosen(index) => Some(index),
                _ => None,
            }
        };
        let Some(chosen) = picked.and_then(|index| tree.into_iter().nth(index)) else {
            return Ok(LoopStep::Continue);
        };
        self.visit_session(chosen.via, &chosen.row.session_id)
            .await?;
        Ok(LoopStep::Continue)
    }

    /// 切进 `target` 这条子代理会话。`via` 是从当前会话往下、中间经过的几层（任务条上
    /// 点的是直系子会话，就是空的）。它正在跑的那一轮由主循环挂上去（`follow_pending`）。
    async fn visit_session(&mut self, via: Vec<ParentRow>, target: &str) -> Result<()> {
        if target == self.active_session_id {
            return Ok(());
        }
        let Some(state) = self.visitable_state(target).await? else {
            return Ok(());
        };
        let here = ParentRow {
            session_id: self.active_session_id.clone(),
            title: session_title(&self.paths, &self.active_session_id),
            root: self.live_repl.visits.is_empty(),
        };
        self.live_repl.visits.push(here);
        self.live_repl.visits.extend(via);
        self.present_visit(&state).await
    }

    /// 切到任务条那棵树上的任意一条（往下、往上、横着，09-26）：访问栈直接换成它的那一串祖先，
    /// `/back` 照旧一层层退。`path` 为空就是回主会话。
    async fn go_to(&mut self, mut path: Vec<ParentRow>, target: &str) -> Result<()> {
        if target == self.active_session_id {
            return Ok(());
        }
        let Some(state) = self.visitable_state(target).await? else {
            return Ok(());
        };
        for (index, row) in path.iter_mut().enumerate() {
            row.root = index == 0;
            if row.title.is_empty() {
                row.title = session_title(&self.paths, &row.session_id);
            }
        }
        self.live_repl.visits = path;
        self.present_visit(&state).await
    }

    /// 回到切进来之前那条会话（`/back`、任务条第一行）。
    async fn leave_visit(&mut self) -> Result<()> {
        let Some(parent) = self.live_repl.visits.last().cloned() else {
            repl_note(
                &mut self.live_repl,
                &format!(
                    "\x1b[2m{}\x1b[0m\n",
                    t("not inside a subagent session", "不在子代理会话里")
                ),
            )?;
            return Ok(());
        };
        let Some(state) = self.visitable_state(&parent.session_id).await? else {
            return Ok(());
        };
        self.live_repl.visits.pop();
        self.present_visit(&state).await
    }

    async fn visitable_state(&mut self, session_id: &str) -> Result<Option<ipc::SessionState>> {
        repl_get_session_state(
            &self.paths,
            &mut self.live_repl,
            miyu_core::ipc::SessionRef::Id {
                id: session_id.to_string(),
            },
        )
        .await
    }

    /// 换画面：访问栈已经摆好了，footer 的层数、任务条第一行跟着它画。车道跟着会话走，
    /// 和真正换会话一样（开发模式的子代理是开发车道）。
    async fn present_visit(&mut self, state: &ipc::SessionState) -> Result<()> {
        // 进出子会话整段攒成一帧（09-25）；在哪条会话里 footer 上的「子代理 ↳N」、任务条第一行
        // 已经说了，不再弹「已切换到会话」。
        crate::cli::repl::tail::begin_frame_hold();
        self.live_repl.suppress_switch_note = true;
        let lane = self.lane_of(state);
        if lane != self.mode {
            self.live_repl.set_mode(lane);
        }
        present_session(
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
        self.jobs_shared.set_repl_session(&self.active_session_id);
        self.refresh_strip_children().await?;
        self.follow_pending = true;
        Ok(())
    }

    /// 切进切出之后，这一段树上的子代理表当场拉一份（正在看的这条和访问路径上每一层的）：轮询
    /// 一秒一次，等它的话，挂在正在看的这条下面的孙代理要晚一拍才出来（用户 09-25：进去看不到
    /// 孙代理）。还在攒帧（`present_visit`），任务条补上这几行和切换画面是同一帧。
    async fn refresh_strip_children(&mut self) -> Result<()> {
        let Some(feed) = crate::cli::repl::jobs::feed() else {
            return Ok(());
        };
        let path: Vec<String> = self
            .live_repl
            .visits
            .iter()
            .map(|visit| visit.session_id.clone())
            .collect();
        for owner in std::iter::once(self.active_session_id.clone()).chain(path) {
            let rows = tokio::time::timeout(
                Duration::from_millis(500),
                crate::cli::repl::strip::fetch_subagent_rows(&self.paths, &owner),
            )
            .await;
            if let Ok(Ok(rows)) = rows {
                feed.publish_children(&owner, rows);
            }
        }
        let jobs = feed.jobs.lock().unwrap().clone();
        if self.live_repl.set_jobs(jobs) && !self.live_repl.external_output_active {
            synchronized_terminal_update(CursorAfterUpdate::Preserve, || self.live_repl.redraw())?;
        }
        Ok(())
    }
}

fn session_title(paths: &MiyuPaths, session_id: &str) -> String {
    let name = StateStore::new(paths)
        .ok()
        .and_then(|store| store.session_record(session_id).ok().flatten())
        .map(|record| record.name)
        .unwrap_or_default();
    display_session_name(&name).to_string()
}

/// 从 `root` 往下的子代理会话，先序排（父在子前），跑完的、中断的也列。
async fn subagent_tree(paths: &MiyuPaths, root: &str) -> Vec<TreeRow> {
    let mut pending = children_of(paths, root)
        .await
        .into_iter()
        .rev()
        .map(|row| (0, row, Vec::new()))
        .collect::<Vec<_>>();
    let mut tree = Vec::new();
    while let Some((depth, row, via)) = pending.pop() {
        if tree.len() >= TREE_ROWS {
            break;
        }
        if depth + 1 < TREE_DEPTH {
            let mut below = via.clone();
            below.push(ParentRow {
                session_id: row.session_id.clone(),
                title: display_session_name(&row.title).to_string(),
                root: false,
            });
            for child in children_of(paths, &row.session_id).await.into_iter().rev() {
                pending.push((depth + 1, child, below.clone()));
            }
        }
        tree.push(TreeRow { depth, row, via });
    }
    tree
}

async fn children_of(paths: &MiyuPaths, session_id: &str) -> Vec<SubagentRow> {
    match send_ipc_admin(
        paths,
        IpcCommand::ListSubagentSessions {
            session_id: session_id.to_string(),
        },
    )
    .await
    {
        Ok((_, data)) => subagent_rows(&data),
        Err(error) => {
            tracing::debug!(error = %error, "subagent sessions unavailable");
            Vec::new()
        }
    }
}

/// `  ↳ 开发中 · 修登录页 · 等待后台`：缩进看层级，状态词说它这会儿在干嘛。
fn tree_line(entry: &TreeRow) -> String {
    let kind = if entry.row.dev {
        t("dev", "开发中")
    } else {
        t("agent", "子代理")
    };
    let indent = if entry.depth == 0 {
        String::new()
    } else {
        format!("{}↳ ", "  ".repeat(entry.depth - 1))
    };
    format!(
        "{indent}{kind} · {} · {}",
        display_session_name(&entry.row.title),
        state_word(&entry.row.state)
    )
}

fn state_word(state: &str) -> &'static str {
    match state {
        "running" => t("running", "运行中"),
        "waiting" => t("waiting on background work", "等待后台"),
        "interrupted" => t("interrupted", "已中断"),
        "failed" => t("failed", "失败"),
        "cancelled" => t("cancelled", "已取消"),
        _ => t("done", "完成"),
    }
}
