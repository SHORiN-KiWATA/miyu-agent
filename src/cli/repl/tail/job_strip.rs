//! 活动区里的任务条：数据跟着轮询换、转轮单独补帧、点哪一行做什么。
//!
//! 行模型和画法在 `repl::strip`。会话项目第 3 段从 `tail/mod.rs` 搬出来，加上会话行
//! （切进子代理会话、回主会话）。

use super::*;
use crate::cli::repl::strip::{self, StripAction, StripItem, StripView};
use crate::cli::repl::strip_tree;

/// 下过"停"之后压住状态行多久。守护进程的任务快照一秒轮询一次，留出几轮的余量。
const SUPPRESS_JOB_FOR: std::time::Duration = std::time::Duration::from_secs(5);

impl LiveReplTail {
    /// 已经下过"停"的任务：状态行里先别再显示它。
    ///
    /// 停完就把状态行清空是对的（那才是事实），但守护进程那边的任务快照是
    /// **一秒轮询一次**的——停完紧接着来的那一次轮询往往还带着它，于是状态行
    /// 消失一瞬间又冒出来，闪一下（用户实测）。压住它几秒，等快照追上来。
    ///
    /// 第一版是"轮询里没有了就解除压制"，看着合理，其实当场自废：停完紧跟着的
    /// 那一句 `set_jobs(空)` 就是一次"轮询里没有"，压制表立刻被清空，下一次真
    /// 轮询把它原样带了回来。所以只能按**时间**放，不能按"这一份列表里有没有"。
    pub(in crate::cli) fn suppress_jobs<'a>(&mut self, ids: impl Iterator<Item = &'a str>) {
        let now = std::time::Instant::now();
        for id in ids {
            self.suppressed_jobs.insert(id.to_string(), now);
        }
    }

    /// 换上最新的后台任务，会话行顺带从轮询那份重取。返回要不要整个重画活动区：
    /// 行数、哪几行、状态变了才要，转轮和用时由 `tick_job_strip` 单独补帧。
    pub(in crate::cli) fn set_jobs(
        &mut self,
        jobs: Vec<miyu_engine::tools::jobs::JobOverview>,
    ) -> bool {
        let jobs: Vec<miyu_engine::tools::jobs::JobOverview> = if self.suppressed_jobs.is_empty() {
            jobs
        } else {
            let now = std::time::Instant::now();
            self.suppressed_jobs
                .retain(|_, at| now.duration_since(*at) < SUPPRESS_JOB_FOR);
            jobs.into_iter()
                .filter(|job| !self.suppressed_jobs.contains_key(&job.job_id))
                .collect()
        };
        // 后台子代理**不**在这儿往 Σ 上加：它的审计会话是边跑边写的，守护进程
        // 算出来的会话累计里已经有了，再加一遍就是算两遍。前台那一路才需要补
        // （见 `set_live_turn_tokens`）——回合跑着的时候客户端不会去重读 Σ。
        let items = match crate::cli::repl::jobs::feed() {
            Some(feed) => feed.strip_items(&self.visits, &jobs),
            None => strip_tree::strip_items(
                &strip_tree::StripScope {
                    path: &self.visits,
                    ..Default::default()
                },
                &jobs,
            ),
        };
        let changed = self.strip_items.len() != items.len()
            || self
                .strip_items
                .iter()
                .zip(items.iter())
                .any(|(a, b)| a.shape() != b.shape());
        self.jobs = jobs;
        self.strip_items = items;
        self.clamp_strip_view();
        self.refresh_job_overlay_title();
        changed
    }

    /// 任务条此刻的每一行，排好序、挂好层（`strip_tree`）。
    pub(in crate::cli) fn strip_rows(&self) -> &[StripItem] {
        &self.strip_items
    }

    /// 任务条此刻怎么画：在子代理会话里回去那一行钉在顶上；没在用方向键挪的时候，下面那一截
    /// 停在露出正在看的那条和它名下的地方（用户 09-26），挪的时候跟着方向键走。
    pub(in crate::cli) fn strip_view(&self) -> StripView {
        let items = self.strip_rows();
        StripView {
            scroll: match self.strip_focus {
                Some(_) => self.strip_scroll,
                None => strip_tree::home_scroll(items),
            },
            hovered: self.job_hover,
            focused: self.strip_focus,
            pinned: strip_tree::pinned_rows(items),
        }
    }

    /// 这条会话名下还有后台的活（Ctrl+C 的第三级先停它们）：没在访问时任务条上的每一行都是
    /// 它的；在子代理会话里是挂在它下面的那几行。
    pub(in crate::cli) fn has_background_work(&self) -> bool {
        !strip_tree::current_session_rows(self.strip_rows()).is_empty()
    }

    /// 上面那些活对应的后台任务号（停完先压住，见 `suppress_jobs`）。
    pub(in crate::cli) fn background_job_ids(&self) -> Vec<String> {
        let items = self.strip_rows();
        strip_tree::current_session_rows(items)
            .into_iter()
            .filter_map(|index| items[index].job())
            .map(|job| job.job_id.clone())
            .collect()
    }

    /// 在任务条上回车切了会话：切完之后光标还停在任务条上，停在切过去的那一条（用户 09-26：
    /// 原来每切一次就回到输入框）。它不在任务条上了（回到了主会话，那儿没有主会话那一行）就停在
    /// 刚才待着的那一条。没有要停的就不动。
    pub(in crate::cli) fn apply_strip_refocus(&mut self) {
        let Some((target, fallback)) = self.strip_refocus.take() else {
            return;
        };
        let items = self.strip_rows();
        let Some(index) = [Some(target), fallback]
            .iter()
            .flatten()
            .find_map(|id| items.iter().position(|item| item.session_id() == Some(id)))
        else {
            return;
        };
        let home = StripView {
            scroll: strip_tree::home_scroll(items),
            pinned: strip_tree::pinned_rows(items),
            ..StripView::default()
        };
        self.strip_scroll = home.scroll_to_show(index, items.len());
        self.strip_focus = Some(index);
    }

    /// 正在看的这条会话在任务条上那一行（子代理会话里才有）。
    pub(in crate::cli) fn current_strip_session(&self) -> Option<String> {
        self.strip_rows()
            .iter()
            .find(|item| item.is_current())
            .and_then(StripItem::session_id)
            .map(str::to_string)
    }

    /// 行数变了（任务跑完、子代理收工）：方向键停的那条、露出来的那一截跟着收回来。
    fn clamp_strip_view(&mut self) {
        let len = self.strip_rows().len();
        self.strip_focus = self
            .strip_focus
            .and_then(|focus| (len > 0).then(|| focus.min(len - 1)));
        if let Some(focus) = self.strip_focus {
            self.strip_scroll = self.strip_view().scroll_to_show(focus, len);
        }
        self.job_hover = self.job_hover.filter(|index| *index < len);
    }

    /// 面板开着哪个任务，就把那个任务此刻的抬头推给它。
    fn refresh_job_overlay_title(&mut self) {
        let Some(job_id) = self
            .screen
            .as_ref()
            .and_then(screen::Screen::overlay_job_id)
        else {
            return;
        };
        let Some(job) = self.jobs.iter().find(|job| job.job_id == job_id) else {
            return;
        };
        let title = screen::job_panel_title(job);
        if let Some(screen) = &mut self.screen {
            screen.refresh_overlay_title(&title);
        }
    }

    /// 状态行转轮此刻该画哪一帧（80ms 一帧，按时间算）。
    pub(in crate::cli) fn job_spinner_frame(&self) -> usize {
        (self.job_spinner_started.elapsed().as_millis() / 80) as usize
    }

    /// Lightweight spinner/timer repaint of the job strip only — no full
    /// tail redraw, so it can run at animation frequency without flicker.
    pub(in crate::cli) fn tick_job_strip(&mut self) -> Result<()> {
        if !self.rendered || self.strip_items.is_empty() {
            return Ok(());
        }
        // 回合里开着面板时活动区归面板（B4），任务条不在屏上。
        if self.turn_panel.is_some() {
            return Ok(());
        }
        // 详情面板开着时整屏归它。这时候还往活动区那几行写，两个画笔会在同一
        // 块地方来回抢，屏幕上就是输入框疯狂抖动。
        if self
            .screen
            .as_ref()
            .is_some_and(screen::Screen::overlay_open)
        {
            return Ok(());
        }
        self.job_spinner = self.job_spinner_frame();
        let (cols, _) = terminal::size().unwrap_or((80, 24));
        let lines = strip::strip_lines(
            &self.strip_rows(),
            self.job_spinner,
            usize::from(cols),
            self.strip_view(),
        );
        let rows = lines.len().min(u16::MAX as usize) as u16;
        if rows > self.tail_rows {
            return Ok(());
        }
        let start = self
            .tail_start
            .saturating_add(self.tail_rows)
            .saturating_sub(rows);
        let input_cursor = self.input_cursor;
        // 这几行绕开活动区的账（`row_memo`）直接写：记账作废，下一帧照写。
        for offset in 0..rows {
            self.row_memo.forget_row(start.saturating_add(offset));
        }
        // Lines are padded to the full terminal width, so plain overwrites
        // suffice — no Clear, no intermediate blank state. The synchronized
        // block keeps the cursor hop invisible over slow links (SSH).
        synchronized_terminal_update(CursorAfterUpdate::Preserve, || {
            let mut stdout = term_out();
            let mut row = start;
            for line in &lines {
                queue!(stdout, MoveTo(0, row), Print(line))?;
                row = row.saturating_add(1);
            }
            queue!(stdout, MoveTo(input_cursor.0, input_cursor.1))?;
            stdout.flush()?;
            Ok(())
        })
    }

    /// 屏幕第 `row` 行落在任务条的哪一条上（下标，不算头上那行空的）。
    pub(in crate::cli) fn strip_index_at(&self, row: u16) -> Option<usize> {
        if self.job_strip_rows == 0 || row < self.job_strip_start {
            return None;
        }
        let offset = usize::from(row - self.job_strip_start);
        if offset >= usize::from(self.job_strip_rows) {
            return None;
        }
        // `strip_lines` 头一行是空的分隔行，任务从第二行起；露不下时最后还有一行「↓ 还有
        // x 个」，它不是哪一条。
        let visible = self.strip_view().visible(self.strip_rows().len());
        offset
            .checked_sub(1)
            .and_then(|slot| visible.get(slot).copied())
    }

    /// 在任务条第 `index` 条上点了一下（以后方向键选中回车也走这儿）。返回真表示这一下
    /// 有了着落：
    /// - 会话行：记下要切进哪条会话、还是回去，由空闲循环或回合循环来取——换会话要动
    ///   footer、历史、车道，只有 `RemoteRepl` 做得了；
    /// - 后台命令：开它的日志面板（只有全屏有面板）。
    pub(in crate::cli) fn activate_strip_row(&mut self, index: usize) -> Result<bool> {
        let (action, job) = match self.strip_rows().get(index) {
            Some(item @ StripItem::Job { job, .. }) => (item.action(), Some(job.clone())),
            Some(item) => (item.action(), None),
            None => return Ok(false),
        };
        if let Some(action) = action {
            self.pending_strip_action = Some(action);
            return Ok(true);
        }
        let Some((job, path)) = job.and_then(|job| {
            let path = job.log_path.clone()?;
            Some((job, path))
        }) else {
            return Ok(false);
        };
        let title = screen::job_panel_title(&job);
        let Some(panel) = self.screen.as_mut() else {
            return Ok(false);
        };
        panel.open_log_overlay(
            std::path::PathBuf::from(path),
            title,
            Some(job.job_id),
            job.command,
        );
        self.repaint_screen()?;
        Ok(true)
    }

    /// 取走点任务条会话行记下的那件事。
    pub(in crate::cli) fn take_strip_action(&mut self) -> Option<StripAction> {
        self.pending_strip_action.take()
    }
}
