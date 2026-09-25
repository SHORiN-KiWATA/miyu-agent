//! 任务条：footer 底下那几行。
//!
//! 09-26 照 Claude Code 改成一棵树（用户拍板）：
//! - 没在访问：这条会话自己的子代理（空心 `○`，名下还在跑的收成「开发中（+3）」）和它自己的
//!   后台命令（转轮）；
//! - 在子代理会话里：树根一直是主会话，`○ 主会话` 钉在顶上；从主会话到正在看的这条，路上经过的
//!   每一层都展开（名下的用 `├`/`└` 挂在下面），正在看的这条实心 `●`，别的折起来。切到孙代理还是
//!   同一棵树，只是实心圆挪过去（用户 09-26）。
//!
//! 行怎么排在 `strip_tree`；这里是行本身（点它做什么、画成什么样）和露出来哪几行。后台子代理
//! 另有一个镜像任务（停它、完成唤醒都走任务那一套）：同一件事只列会话这一行，右边的量和用时从
//! 镜像任务来。

use crate::cli::repl::jobs::{format_job_duration, JOB_SPINNER_FRAMES};
use crate::cli::*;
use miyu_engine::tools::jobs::JobOverview;

/// 任务条上一条子代理会话（`ListSubagentSessions` 的一行）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::cli) struct SubagentRow {
    pub(in crate::cli) session_id: String,
    pub(in crate::cli) title: String,
    /// `running` / `waiting` 才上任务条（正在看的那条例外）。别的状态（跑完、失败、中断、
    /// 取消）在 `/subagent` 里列着。
    pub(in crate::cli) state: String,
    pub(in crate::cli) dev: bool,
    /// 后台子代理的镜像任务。
    pub(in crate::cli) job_id: Option<String>,
    /// 它名下还在跑的后代（孙代理、这一支的后台命令）：折起来时写成「（+N）」。
    pub(in crate::cli) running_descendants: u64,
}

impl SubagentRow {
    /// 还在干活，该上任务条。
    pub(in crate::cli) fn is_live(&self) -> bool {
        matches!(self.state.as_str(), "running" | "waiting")
    }
}

/// 访问栈的一层：切进子代理会话一路经过的会话，栈底是车道上那条（主会话）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::cli) struct ParentRow {
    pub(in crate::cli) session_id: String,
    pub(in crate::cli) title: String,
    /// 车道上的那条会话（栈底）。
    pub(in crate::cli) root: bool,
}

/// 一条子代理会话行和正在看的会话是什么关系。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::cli) enum Place {
    /// 就是正在看的这条：实心 `●`，名下的挂在它下面。
    Current,
    /// 从主会话到正在看的这条、路上经过的：空心，也展开。
    Path,
    /// 别的：空心，折起来，名下还在跑的写成「（+N）」。
    Other,
}

/// 任务条上排好序的一行（`strip_tree::strip_items` 排的）。
#[derive(Clone, Debug)]
pub(in crate::cli) enum StripItem {
    /// 车道上那条会话（主会话）：在子代理会话里时钉在顶上，点它回去。
    Root(ParentRow),
    /// 一条子代理会话。`depth` 是第几层（主会话名下的是 0），`twig` 是画在行首的树枝；`path`
    /// 是切过去之后的访问栈（从主会话往下到它的父会话）；`mirror` 是后台子代理的镜像任务，
    /// 量和用时从它来。
    Agent {
        row: SubagentRow,
        place: Place,
        depth: usize,
        twig: String,
        path: Vec<ParentRow>,
        mirror: Option<JobOverview>,
    },
    /// 一条后台命令（或者还没对上会话行的后台子代理任务）。
    Job {
        job: JobOverview,
        depth: usize,
        twig: String,
    },
}

/// 点任务条上的会话行（或者在那一行上回车）要做的事。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::cli) enum StripAction {
    /// 切进这条会话名下的子代理会话，访问栈压一层（时间线上点子代理那一步）。
    Visit(String),
    /// 切到树上的这条会话，访问栈换成 `path`：任务条上往下、往上、横着切都走这儿。
    Go {
        session_id: String,
        path: Vec<ParentRow>,
    },
    /// 点的就是正在看的这条。
    Stay,
}

impl StripItem {
    /// 会话行点下去做什么；后台命令行是 `None`（点开日志面板，由活动区管）。
    pub(in crate::cli) fn action(&self) -> Option<StripAction> {
        match self {
            Self::Root(root) => Some(StripAction::Go {
                session_id: root.session_id.clone(),
                path: Vec::new(),
            }),
            Self::Agent {
                place: Place::Current,
                ..
            } => Some(StripAction::Stay),
            Self::Agent { row, path, .. } => Some(StripAction::Go {
                session_id: row.session_id.clone(),
                path: path.clone(),
            }),
            Self::Job { .. } => None,
        }
    }

    /// 这一行是哪条会话（后台命令行不是）。
    pub(in crate::cli) fn session_id(&self) -> Option<&str> {
        match self {
            Self::Root(root) => Some(&root.session_id),
            Self::Agent { row, .. } => Some(&row.session_id),
            Self::Job { .. } => None,
        }
    }

    /// 第几层：主会话那一行和它名下的是 0，再往下一层加一。
    pub(in crate::cli) fn depth(&self) -> usize {
        match self {
            Self::Root(_) => 0,
            Self::Agent { depth, .. } | Self::Job { depth, .. } => *depth,
        }
    }

    pub(in crate::cli) fn is_current(&self) -> bool {
        matches!(
            self,
            Self::Agent {
                place: Place::Current,
                ..
            }
        )
    }

    /// 这一行对应的后台任务（命令本身，或者子代理的镜像任务）。
    pub(in crate::cli) fn job(&self) -> Option<&JobOverview> {
        match self {
            Self::Job { job, .. } => Some(job),
            Self::Agent { mirror, .. } => mirror.as_ref(),
            Self::Root(_) => None,
        }
    }

    /// 行的样子由哪些东西决定：这一串变了才要整个重画活动区，转轮和用时另有补帧。
    pub(in crate::cli) fn shape(&self) -> String {
        match self {
            Self::Root(root) => format!("root|{}|{}", root.session_id, root.title),
            Self::Agent {
                row,
                place,
                depth,
                twig,
                mirror,
                ..
            } => format!(
                "agent|{}|{}|{}|{}|{}|{place:?}|{depth}|{twig}|{}",
                row.session_id,
                row.title,
                row.state,
                row.dev,
                row.running_descendants,
                mirror.as_ref().map(|job| job.status.as_str()).unwrap_or("")
            ),
            Self::Job { job, depth, twig } => {
                format!(
                    "job|{}|{}|{}|{depth}|{twig}",
                    job.job_id, job.status, job.title
                )
            }
        }
    }

    fn kind_word(&self) -> &'static str {
        match self {
            // 开发模式的子代理单列一类：那一条是去写代码的，「开发中」比「子代理」
            // 更说明它在干嘛。
            Self::Job { job, .. } => match job.kind.as_str() {
                "dev" => miyu_base::i18n::text("dev", "开发中"),
                "subagent" if job.dev => miyu_base::i18n::text("dev", "开发中"),
                "subagent" => miyu_base::i18n::text("agent", "子代理"),
                _ => miyu_base::i18n::text("cmd", "命令"),
            },
            Self::Agent { row, .. } if row.dev => miyu_base::i18n::text("dev", "开发中"),
            Self::Agent { .. } => miyu_base::i18n::text("agent", "子代理"),
            Self::Root(_) => miyu_base::i18n::text("main", "主会话"),
        }
    }

    /// kind 那一栏：折起来的子代理后面挂着名下还在跑的有几个（用户 09-25：`开发中（+3）`）。
    /// 展开的那条（正在看的）不挂，它名下的就列在下面。
    fn kind_label(&self) -> String {
        let word = self.kind_word();
        match self {
            Self::Agent { row, place, .. }
                if *place == Place::Other && row.running_descendants > 0 =>
            {
                if miyu_base::i18n::is_zh() {
                    format!("{word}（+{}）", row.running_descendants)
                } else {
                    format!("{word} (+{})", row.running_descendants)
                }
            }
            _ => word.to_string(),
        }
    }

    /// 行首的记号：子代理空心 `○`、正在看的实心 `●`（和 Claude Code 一样，用户 09-25），
    /// 后台命令照旧转轮。
    fn marker(&self, spinner_phase: usize) -> char {
        match self {
            Self::Agent {
                place: Place::Current,
                ..
            } => '●',
            Self::Root(_) | Self::Agent { .. } => '○',
            Self::Job { job, .. } if job.kind == "subagent" || job.kind == "dev" => '○',
            Self::Job { .. } => JOB_SPINNER_FRAMES[spinner_phase % JOB_SPINNER_FRAMES.len()],
        }
    }

    /// 行首到 kind 那一栏：挂在下面的先画树枝。
    fn head(&self, spinner_phase: usize) -> String {
        let twig = match self {
            Self::Agent { twig, .. } | Self::Job { twig, .. } => twig.as_str(),
            Self::Root(_) => "",
        };
        format!("{twig}{} {}", self.marker(spinner_phase), self.kind_label())
    }

    /// kind 那一栏右边的字。
    fn body(&self) -> String {
        match self {
            Self::Job { job, .. } => format!("{} · {}", job.job_id, job.title),
            Self::Agent { row, .. } => match row.state.as_str() {
                // 记号已经说了「在这儿」，不另写状态。
                "running" | "" => row.title.clone(),
                "waiting" => format!(
                    "{} · {}",
                    row.title,
                    miyu_base::i18n::text("waiting on background work", "等待后台")
                ),
                _ => row.title.clone(),
            },
            Self::Root(root) => root.title.clone(),
        }
    }

    /// 右对齐的那一栏：时间左边先报量。一条子代理跑五分钟，光有秒数看不出它是在
    /// 干活还是卡住了（用户：这里时间左侧应该有一个 token 记述）。命令类任务没有这个
    /// 概念，那儿只有时间。前台子代理会话没有镜像任务，这一栏空着。
    fn timer(&self) -> String {
        let Some(job) = self.job() else {
            return String::new();
        };
        match job.metric.as_deref().filter(|text| !text.trim().is_empty()) {
            Some(metric) => format!(
                "{}  {}",
                metric.trim(),
                format_job_duration(job.runtime_seconds)
            ),
            None => format_job_duration(job.runtime_seconds),
        }
    }
}

/// 任务条最多露几条（用户 09-25：状态行最多显示 5 个，多了底下写「↓ 还有 x 个」）。
pub(in crate::cli) const STRIP_VISIBLE_ROWS: usize = 5;

/// 任务条此刻怎么画：钉在顶上几条、下面从第几条露起、鼠标悬在哪条、方向键停在哪条。下标
/// 都是 `strip_items` 里的下标。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(in crate::cli) struct StripView {
    /// 滚动的那一截从第几条露起（不小于 `pinned`）。
    pub(in crate::cli) scroll: usize,
    pub(in crate::cli) hovered: Option<usize>,
    pub(in crate::cli) focused: Option<usize>,
    /// 顶上钉住不滚的几条：在子代理会话里，回去的那一行（用户 09-26）。
    pub(in crate::cli) pinned: usize,
}

impl StripView {
    /// 滚动那一截露几条。
    fn slots(&self) -> usize {
        STRIP_VISIBLE_ROWS.saturating_sub(self.pinned).max(1)
    }

    /// 滚动那一截从第几条起：不越过钉住的，也不滚过头留出空行。
    fn start(&self, len: usize) -> usize {
        let pinned = self.pinned.min(len);
        let last_start = len.saturating_sub(self.slots()).max(pinned);
        self.scroll.clamp(pinned, last_start)
    }

    /// 露出来的是哪几条：钉住的在前，后面接滚动那一截。
    pub(in crate::cli) fn visible(&self, len: usize) -> Vec<usize> {
        let pinned = self.pinned.min(len);
        let start = self.start(len);
        (0..pinned)
            .chain(start..(start + self.slots()).min(len))
            .collect()
    }

    /// 要露出第 `index` 条，滚动那一截该从哪儿起。
    pub(in crate::cli) fn scroll_to_show(&self, index: usize, len: usize) -> usize {
        let start = self.start(len);
        if index < self.pinned || (start..start + self.slots()).contains(&index) {
            start
        } else if index < start {
            index
        } else {
            index + 1 - self.slots()
        }
    }
}

/// 任务条：头上一行空的，然后一条一行，用时右对齐到终端宽度。露不下的在底下写一行
/// 「↓ 还有 x 个」。
///
/// 悬浮的那一条不 dim——和正文里可点的块一个规矩：悬浮提亮，好让人知道这行能点（用户
/// 09-18：任务条行悬浮没有高亮）。正在看的那条（`●`）也不 dim、加粗，一眼看得出在哪。
/// 方向键停着的那一条和选择面板的选中项一个样子：行首一个 `›`，整行加粗。`›` 占行首单独
/// 留出来的两列，记号照常在它后面（用户 09-25：原来 `›` 直接顶掉转轮）。
pub(in crate::cli) fn strip_lines(
    rows: &[StripItem],
    spinner_phase: usize,
    cols: usize,
    view: StripView,
) -> Vec<String> {
    if rows.is_empty() {
        return Vec::new();
    }
    // 记号和 kind 补到同一栏宽，混着命令、子代理、挂在下面的几行时标题照样竖着对齐。
    let head_col = rows
        .iter()
        .map(|row| visible_width(&row.head(spinner_phase)))
        .max()
        .unwrap_or(0);
    let visible = view.visible(rows.len());
    let mut lines = vec![String::new()];
    for &index in &visible {
        let row = &rows[index];
        let focused = view.focused == Some(index);
        let head = row.head(spinner_phase);
        let pad_head = " ".repeat(head_col.saturating_sub(visible_width(&head)));
        let gutter = if focused { '›' } else { ' ' };
        let mut left = format!("{gutter} {head}{pad_head} {}", row.body());
        let timer = row.timer();
        let timer_width = visible_width(&timer);
        // Never exceed the terminal width: a wrapped strip line would shift
        // the whole tail and flicker.
        let max_left = cols.saturating_sub(timer_width).saturating_sub(2);
        while visible_width(&left) > max_left && !left.is_empty() {
            left.pop();
        }
        let pad = " ".repeat(
            cols.saturating_sub(visible_width(&left))
                .saturating_sub(timer_width)
                .max(1),
        );
        lines.push(if focused {
            let rest = left.strip_prefix('›').unwrap_or(&left);
            format!("\x1b[1m\x1b[35m›\x1b[0m\x1b[1m{rest}{pad}{timer}\x1b[0m")
        } else if row.is_current() {
            format!("\x1b[1m{left}{pad}{timer}\x1b[0m")
        } else if view.hovered == Some(index) {
            format!("{left}{pad}{timer}\x1b[0m")
        } else {
            format!("\x1b[2m{left}{pad}{timer}\x1b[0m")
        });
    }
    // 露不下的时候底下那一行一直留着，滚到底了就空着：不然往下挪到底那一下它没了，
    // 输入框整个往下跳一行。
    if rows.len() > STRIP_VISIBLE_ROWS {
        let shown_to = visible.last().map_or(0, |last| last + 1);
        let hidden = rows.len().saturating_sub(shown_to);
        let more = match hidden {
            0 => String::new(),
            _ if miyu_base::i18n::is_zh() => format!("↓ 还有 {hidden} 个"),
            _ => format!("↓ {hidden} more"),
        };
        let pad = " ".repeat(cols.saturating_sub(visible_width(&more)));
        lines.push(format!("\x1b[2m{more}{pad}\x1b[0m"));
    }
    lines
}

/// `session_id` 名下的子代理会话，什么状态都要（任务条上挑哪些由 `strip_tree` 定）。
pub(in crate::cli) async fn fetch_subagent_rows(
    paths: &MiyuPaths,
    session_id: &str,
) -> Result<Vec<SubagentRow>> {
    let mut stream = ipc::connect(&paths.ipc_socket()).await?;
    ipc::send(
        &mut stream,
        &IpcRequest::new(IpcCommand::ListSubagentSessions {
            session_id: session_id.to_string(),
        }),
    )
    .await?;
    match ipc::receive::<IpcFrame>(&mut stream).await? {
        Some(IpcFrame::AdminResult { data, .. }) => Ok(subagent_rows(&data)),
        _ => Ok(Vec::new()),
    }
}

/// `ListSubagentSessions` 的回包 → 行，什么状态都要。
pub(in crate::cli) fn subagent_rows(data: &serde_json::Value) -> Vec<SubagentRow> {
    let text = |value: &serde_json::Value, key: &str| {
        value
            .get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    data.get("sessions")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .map(|session| SubagentRow {
            session_id: text(session, "session_id"),
            title: text(session, "name"),
            state: text(session, "task_state"),
            dev: session
                .get("dev")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
            job_id: session
                .get("job_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            running_descendants: session
                .get("running_descendants")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0),
        })
        .filter(|row| !row.session_id.is_empty())
        .collect()
}
