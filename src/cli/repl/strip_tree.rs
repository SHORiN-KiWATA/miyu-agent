//! 任务条上列哪些行、按什么顺序、挂在哪一层（09-26 照 Claude Code 改成树，用户拍板）。
//!
//! - 没在访问：这条会话自己的子代理（折起来，名下还在跑的收成「（+N）」）和它自己的后台命令；
//! - 在子代理会话里：回去的那一行钉在顶上，然后父会话自己的子代理——正在看的这条展开，它的
//!   子代理和后台命令用 `├`/`└` 挂在它下面——最后是父会话自己的后台命令。
//!
//! 后代的任务不在第一层单列（原来挂个 `↳` 列在主会话的任务条上），切进去才展开。

use crate::cli::repl::strip::{
    Branch, ParentRow, Place, StripItem, SubagentRow, STRIP_VISIBLE_ROWS,
};
use miyu_engine::tools::jobs::JobOverview;
use std::collections::HashSet;

/// 排任务条要知道的：在哪条会话、从哪儿切进来的、这两条名下各有哪些子代理。
#[derive(Clone, Copy, Debug, Default)]
pub(in crate::cli) struct StripScope<'a> {
    /// 正在看的会话。`None` = 不知道（直连模式：任务表本来就只有这条会话的）。
    pub(in crate::cli) current: Option<&'a str>,
    /// 回去的那条（访问栈顶）；没在访问是 `None`。
    pub(in crate::cli) parent: Option<&'a ParentRow>,
    /// 父会话名下的子代理（什么状态都有，这里挑）。
    pub(in crate::cli) parent_children: &'a [SubagentRow],
    /// 正在看的这条名下的子代理。
    pub(in crate::cli) children: &'a [SubagentRow],
}

/// 任务条此刻的每一行，排好序、挂好层。
pub(in crate::cli) fn strip_items(scope: &StripScope<'_>, jobs: &[JobOverview]) -> Vec<StripItem> {
    let listed = listed_mirrors(scope);
    let Some(parent) = scope.parent else {
        let mut items = agents(scope.children, Place::Child, Branch::Top, jobs);
        items.extend(commands(scope.current, jobs, &listed, Branch::Top, true));
        return items;
    };
    let mut items = vec![StripItem::Parent(parent.clone())];
    let current = scope.current.unwrap_or_default();
    let mut listed_current = false;
    for row in scope.parent_children {
        if row.session_id == current {
            listed_current = true;
            items.push(agent(row.clone(), Place::Current, Branch::Top, jobs));
            items.extend(under_current(scope, jobs, &listed));
        } else if row.is_live() {
            items.push(agent(row.clone(), Place::Sibling, Branch::Top, jobs));
        }
    }
    // 父会话的子代理表还没拉回来（刚切进来那一下）：正在看的这条照样先立住，名下的挂在它下面。
    if !listed_current && scope.current.is_some() {
        let row = SubagentRow {
            session_id: current.to_string(),
            title: String::new(),
            state: String::new(),
            dev: false,
            job_id: None,
            running_descendants: 0,
        };
        items.insert(1, agent(row, Place::Current, Branch::Top, jobs));
        let nested = under_current(scope, jobs, &listed);
        items.splice(2..2, nested);
    }
    items.extend(commands(
        Some(parent.session_id.as_str()),
        jobs,
        &listed,
        Branch::Top,
        true,
    ));
    items
}

/// 滚动那一截平时停在哪：露出正在看的那条和挂在它下面的（露不全就先露它自己和前几条）。
/// 没在访问的时候从头露。
pub(in crate::cli) fn home_scroll(items: &[StripItem]) -> usize {
    let pinned = pinned_rows(items);
    let Some(current) = items.iter().position(StripItem::is_current) else {
        return pinned;
    };
    let nested = items[current + 1..]
        .iter()
        .take_while(|item| item.branch() != Branch::Top)
        .count();
    let slots = STRIP_VISIBLE_ROWS.saturating_sub(pinned).max(1);
    pinned.max(current.min((current + nested + 1).saturating_sub(slots)))
}

/// 顶上钉住不滚的几条：在子代理会话里，回去的那一行（用户 09-26）。
pub(in crate::cli) fn pinned_rows(items: &[StripItem]) -> usize {
    usize::from(matches!(items.first(), Some(StripItem::Parent(_))))
}

/// 正在看的这条名下的：它的子代理、它自己的后台命令，挂在它下面，最后一条画 `└`。
fn under_current(
    scope: &StripScope<'_>,
    jobs: &[JobOverview],
    listed: &HashSet<&str>,
) -> Vec<StripItem> {
    let branch = Branch::Under { last: false };
    let mut nested = agents(scope.children, Place::Child, branch, jobs);
    nested.extend(commands(scope.current, jobs, listed, branch, false));
    if let Some(last) = nested.last_mut() {
        set_branch(last, Branch::Under { last: true });
    }
    nested
}

fn agents(
    rows: &[SubagentRow],
    place: Place,
    branch: Branch,
    jobs: &[JobOverview],
) -> Vec<StripItem> {
    rows.iter()
        .filter(|row| row.is_live())
        .map(|row| agent(row.clone(), place, branch, jobs))
        .collect()
}

fn agent(row: SubagentRow, place: Place, branch: Branch, jobs: &[JobOverview]) -> StripItem {
    let mirror = row
        .job_id
        .as_deref()
        .and_then(|id| jobs.iter().find(|job| job.job_id == id))
        .cloned();
    StripItem::Agent {
        row,
        place,
        branch,
        mirror,
    }
}

/// 已经由会话行代表了的镜像任务：列出来的子代理（还在干活的，加上正在看的那条）的。
fn listed_mirrors<'a>(scope: &StripScope<'a>) -> HashSet<&'a str> {
    scope
        .children
        .iter()
        .chain(scope.parent_children)
        .filter(|row| row.is_live() || Some(row.session_id.as_str()) == scope.current)
        .filter_map(|row| row.job_id.as_deref())
        .collect()
}

/// `owner` 这条会话自己的后台任务。已经由会话行代表的镜像任务不再单列；还没对上会话行的
/// （子代理表还没拉回来）照旧列成一行，不至于看不见。`legacy` 连没挂会话的老任务一起列（台账
/// 恢复的那种，只放第一层）。`owner` 为 `None`（直连模式）时任务表本来就只有这条会话的，全列。
fn commands(
    owner: Option<&str>,
    jobs: &[JobOverview],
    listed: &HashSet<&str>,
    branch: Branch,
    legacy: bool,
) -> Vec<StripItem> {
    jobs.iter()
        .filter(|job| match (owner, job.session_id.as_deref()) {
            (_, None) => legacy,
            (None, Some(_)) => true,
            (Some(owner), Some(session)) => owner == session,
        })
        .filter(|job| !listed.contains(job.job_id.as_str()))
        .map(|job| StripItem::Job {
            job: job.clone(),
            branch,
        })
        .collect()
}

fn set_branch(item: &mut StripItem, to: Branch) {
    match item {
        StripItem::Agent { branch, .. } | StripItem::Job { branch, .. } => *branch = to,
        StripItem::Parent(_) => {}
    }
}
