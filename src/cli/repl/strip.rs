//! 任务条：footer 底下那几行。
//!
//! 会话项目第 3 段（09-18 子代理会话化的前端半边）起，任务条先列会话行，再列后台任务：
//! - 在子会话里时，第一行「↑ 主会话」，点它回去；
//! - 这条会话名下还在干活的子代理会话，点它切进去看、接着聊。以前点开的是盖在正文上
//!   的浮层；
//! - 后台命令照旧一行一个，点开是日志面板。
//!
//! 后台子代理另有一个镜像任务（停它、完成唤醒都走任务那一套）。同一件事只列会话这一行，
//! 右边的量和用时从镜像任务来。

use crate::cli::repl::jobs::{format_job_duration, JOB_SPINNER_FRAMES};
use crate::cli::*;
use miyu_engine::tools::jobs::JobOverview;

/// 任务条上一条子代理会话。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::cli) struct SubagentRow {
    pub(in crate::cli) session_id: String,
    pub(in crate::cli) title: String,
    /// `running` / `waiting` 才上任务条。别的状态（跑完、失败、中断、取消）在
    /// `/subagent` 里列着。
    pub(in crate::cli) state: String,
    pub(in crate::cli) dev: bool,
    /// 后台子代理的镜像任务。
    pub(in crate::cli) job_id: Option<String>,
}

/// 正在访问子会话时，回去是哪一条。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::cli) struct ParentRow {
    pub(in crate::cli) session_id: String,
    pub(in crate::cli) title: String,
    /// 回去就是车道上的那条会话（不是又一层子会话）：那一行叫「主会话」。
    pub(in crate::cli) root: bool,
}

/// 任务条上的会话行，排在后台任务前面。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::cli) enum StripSession {
    Parent(ParentRow),
    Child(SubagentRow),
}

/// 点任务条上的会话行（或者在那一行上回车）要做的事。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::cli) enum StripAction {
    Visit(String),
    Back,
}

impl StripSession {
    pub(in crate::cli) fn action(&self) -> StripAction {
        match self {
            Self::Parent(_) => StripAction::Back,
            Self::Child(row) => StripAction::Visit(row.session_id.clone()),
        }
    }
}

/// 任务条上的一行。
#[derive(Clone, Copy, Debug)]
pub(in crate::cli) enum StripRow<'a> {
    /// 第二项是后台子代理的镜像任务，量和用时从它来。
    Session(&'a StripSession, Option<&'a JobOverview>),
    Job(&'a JobOverview),
}

/// 会话行在前，后台任务在后。已经由会话行代表的镜像任务不再单列。
pub(in crate::cli) fn strip_rows<'a>(
    sessions: &'a [StripSession],
    jobs: &'a [JobOverview],
) -> Vec<StripRow<'a>> {
    let mirrored = |session: &StripSession| match session {
        StripSession::Child(child) => child
            .job_id
            .as_deref()
            .and_then(|id| jobs.iter().find(|job| job.job_id == id)),
        StripSession::Parent(_) => None,
    };
    sessions
        .iter()
        .map(|session| StripRow::Session(session, mirrored(session)))
        .chain(
            jobs.iter()
                .filter(|job| !job_listed_as_session(&job.job_id, sessions))
                .map(StripRow::Job),
        )
        .collect()
}

fn job_listed_as_session(job_id: &str, sessions: &[StripSession]) -> bool {
    sessions.iter().any(
        |row| matches!(row, StripSession::Child(child) if child.job_id.as_deref() == Some(job_id)),
    )
}

impl StripRow<'_> {
    fn kind_word(&self) -> &'static str {
        match self {
            // 开发模式的子代理单列一类：那一条是去写代码的，「开发中」比「子代理」
            // 更说明它在干嘛。
            Self::Job(job) => match job.kind.as_str() {
                "dev" => miyu_base::i18n::text("dev", "开发中"),
                "subagent" => miyu_base::i18n::text("agent", "子代理"),
                _ => miyu_base::i18n::text("cmd", "命令"),
            },
            Self::Session(StripSession::Child(child), _) if child.dev => {
                miyu_base::i18n::text("dev", "开发中")
            }
            Self::Session(StripSession::Child(_), _) => miyu_base::i18n::text("agent", "子代理"),
            Self::Session(StripSession::Parent(parent), _) if parent.root => {
                miyu_base::i18n::text("main", "主会话")
            }
            Self::Session(StripSession::Parent(_), _) => miyu_base::i18n::text("back", "上一层"),
        }
    }

    fn marker(&self, spinner_phase: usize) -> char {
        match self {
            Self::Session(StripSession::Parent(_), _) => '↑',
            _ => JOB_SPINNER_FRAMES[spinner_phase % JOB_SPINNER_FRAMES.len()],
        }
    }

    /// kind 那一栏右边的字。
    fn body(&self) -> String {
        match self {
            Self::Job(job) => {
                // 后代的任务(子代理开的后台命令、后台孙代理)前面挂个 ↳,看得出不是这一层开的。
                let nested = job
                    .root_session_id
                    .as_deref()
                    .is_some_and(|root| job.session_id.as_deref() != Some(root));
                format!(
                    "{}{} · {}",
                    if nested { "↳ " } else { "" },
                    job.job_id,
                    job.title
                )
            }
            Self::Session(StripSession::Child(child), _) => match child.state.as_str() {
                // 转轮已经说了「在跑」。
                "running" => child.title.clone(),
                _ => format!(
                    "{} · {}",
                    child.title,
                    miyu_base::i18n::text("waiting on background work", "等待后台")
                ),
            },
            Self::Session(StripSession::Parent(parent), _) => parent.title.clone(),
        }
    }

    /// 右对齐的那一栏：时间左边先报量。一条子代理跑五分钟，光有秒数看不出它是在
    /// 干活还是卡住了（用户：这里时间左侧应该有一个 token 记述）。命令类任务没有这个
    /// 概念，那儿只有时间。前台子代理会话没有镜像任务，这一栏空着。
    fn timer(&self) -> String {
        let job = match self {
            Self::Job(job) | Self::Session(_, Some(job)) => job,
            Self::Session(_, None) => return String::new(),
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

/// 任务条：头上一行空的，然后一条一行，用时右对齐到终端宽度。
///
/// `hovered`：鼠标正悬在哪一条上（下标，不算头上的空行），那一行不 dim——和正文里
/// 可点的块一个规矩：悬浮提亮，好让人知道这行能点（用户 09-18：任务条行悬浮没有高亮）。
pub(in crate::cli) fn strip_lines(
    rows: &[StripRow<'_>],
    spinner_phase: usize,
    cols: usize,
    hovered: Option<usize>,
) -> Vec<String> {
    if rows.is_empty() {
        return Vec::new();
    }
    // kind 补到同一栏宽，混着命令和子代理时 id、标题照样竖着对齐。
    let kind_col = rows
        .iter()
        .map(|row| visible_width(row.kind_word()))
        .max()
        .unwrap_or(0);
    let mut lines = vec![String::new()];
    for (index, row) in rows.iter().enumerate() {
        let kind_word = row.kind_word();
        let kind_pad = " ".repeat(kind_col.saturating_sub(visible_width(kind_word)));
        let mut left = format!(
            "{} {kind_word}{kind_pad} {}",
            row.marker(spinner_phase),
            row.body()
        );
        let timer = row.timer();
        let timer_width = visible_width(&timer);
        // Never exceed the terminal width: a wrapped strip line would shift
        // the whole tail and flicker.
        let max_left = cols.saturating_sub(timer_width).saturating_sub(2);
        while visible_width(&left) > max_left && !left.is_empty() {
            left.pop();
        }
        let pad = cols
            .saturating_sub(visible_width(&left))
            .saturating_sub(timer_width)
            .max(1);
        let dim = if hovered == Some(index) {
            ""
        } else {
            "\x1b[2m"
        };
        lines.push(format!("{dim}{left}{}{timer}\x1b[0m", " ".repeat(pad)));
    }
    lines
}

/// 任务条上该列哪些会话行：访问子会话时先列回去的那一行，再列这条会话名下还在干活的
/// 子代理。子代理那几行由轮询线程按这个 REPL 的会话拉回来（直连模式没有，就是空的）。
pub(in crate::cli) fn strip_sessions(parent: Option<&ParentRow>) -> Vec<StripSession> {
    let mut rows = Vec::new();
    if let Some(parent) = parent {
        rows.push(StripSession::Parent(parent.clone()));
    }
    if let Some(feed) = super::jobs::feed() {
        rows.extend(
            feed.subagents
                .lock()
                .unwrap()
                .iter()
                .cloned()
                .map(StripSession::Child),
        );
    }
    rows
}

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
        Some(IpcFrame::AdminResult { data, .. }) => Ok(subagent_rows(&data)
            .into_iter()
            .filter(|row| matches!(row.state.as_str(), "running" | "waiting"))
            .collect()),
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
        })
        .filter(|row| !row.session_id.is_empty())
        .collect()
}
