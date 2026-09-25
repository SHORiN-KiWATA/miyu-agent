//! 任务条上列哪些行、按什么顺序、挂在哪一层（09-26 照 Claude Code 改成树，用户拍板）。
//!
//! - 没在访问：这条会话自己的子代理（折起来，名下还在跑的收成「（+N）」）和它自己的命令；
//! - 在子代理会话里：树根一直是主会话（钉在顶上的那一行），从它往下到正在看的这条，路上每一层都
//!   展开，名下的用 `├`/`└` 挂在下面；正在看的这条实心，它名下的子代理和命令也挂在它下面；路以外
//!   的折起来。切到孙代理还是同一棵树，只是实心圆挪过去（用户 09-26：原来树根跟着父会话走，切到
//!   孙代理只剩「上一层」和它自己）。
//!
//! 后代的任务不在第一层单列（原来挂个 `↳` 列在主会话的任务条上），切进去才展开。

use crate::cli::repl::strip::{ParentRow, Place, StripItem, SubagentRow, STRIP_VISIBLE_ROWS};
use miyu_engine::tools::jobs::JobOverview;
use std::collections::{HashMap, HashSet};

/// 排任务条要知道的：在哪条会话、从哪儿一路切进来的、路上各条会话名下有哪些子代理。
#[derive(Clone, Copy, Debug, Default)]
pub(in crate::cli) struct StripScope<'a> {
    /// 正在看的会话。`None` = 不知道（直连模式：任务表本来就只有这条会话的）。
    pub(in crate::cli) current: Option<&'a str>,
    /// 访问栈：栈底是车道上那条会话（主会话），往上一层层到正在看的这条的父会话。没在访问是空的。
    pub(in crate::cli) path: &'a [ParentRow],
    /// 各条会话名下的子代理（什么状态都有，这里挑）。
    pub(in crate::cli) children: Option<&'a HashMap<String, Vec<SubagentRow>>>,
}

/// 任务条此刻的每一行，排好序、挂好层。
pub(in crate::cli) fn strip_items(scope: &StripScope<'_>, jobs: &[JobOverview]) -> Vec<StripItem> {
    // 从主会话往下、要一层层展开的那条路（不含主会话自己）。
    let chain: Vec<&str> = scope
        .path
        .iter()
        .skip(1)
        .map(|row| row.session_id.as_str())
        .chain(scope.current.filter(|_| !scope.path.is_empty()))
        .collect();
    let mut builder = Builder {
        scope,
        jobs,
        listed: listed_mirrors(scope, &chain),
        items: Vec::new(),
    };
    match scope.path.first() {
        // 没在访问：切进这一层的子代理，访问栈就是这条会话自己（标题由切的那一头补上）。
        None => {
            let here: Vec<ParentRow> = scope
                .current
                .map(|current| ParentRow {
                    session_id: current.to_string(),
                    title: String::new(),
                    root: true,
                })
                .into_iter()
                .collect();
            builder.level(scope.current, 0, "", &[], &here, true);
            // 有子代理在跑，主会话那一行就在最上面（用户 09-26：不用等切进子代理才出现）。
            let agents = builder
                .items
                .iter()
                .any(|item| matches!(item, StripItem::Agent { .. }));
            if let (true, Some(root)) = (agents, here.first()) {
                builder.items.insert(
                    0,
                    StripItem::Root {
                        row: root.clone(),
                        current: true,
                    },
                );
            }
        }
        Some(root) => {
            let root = root.clone();
            builder.items.push(StripItem::Root {
                row: root.clone(),
                current: false,
            });
            builder.level(
                Some(&root.session_id),
                0,
                "",
                &chain,
                std::slice::from_ref(&root),
                true,
            );
        }
    }
    builder.items
}

/// 滚动那一截平时停在哪：露出正在看的那条和挂在它下面的（露不全就先露它自己和前几条）。
/// 没在访问的时候从头露。
pub(in crate::cli) fn home_scroll(items: &[StripItem]) -> usize {
    let pinned = pinned_rows(items);
    let Some(current) = items.iter().position(StripItem::is_current) else {
        return pinned;
    };
    let nested = own_rows(items, current).len();
    let slots = STRIP_VISIBLE_ROWS.saturating_sub(pinned).max(1);
    pinned.max(current.min((current + nested + 1).saturating_sub(slots)))
}

/// 顶上钉住不滚的几条：主会话那一行（用户 09-26）。
pub(in crate::cli) fn pinned_rows(items: &[StripItem]) -> usize {
    usize::from(matches!(items.first(), Some(StripItem::Root { .. })))
}

/// 正在看的这条会话自己名下的那几行（它的子代理、它的命令）的下标。没在访问（没有标着
/// 「正在看」的那一行）时，任务条上的每一行都是它的。
pub(in crate::cli) fn current_session_rows(items: &[StripItem]) -> Vec<usize> {
    match items.iter().position(StripItem::is_current) {
        Some(current) => own_rows(items, current),
        None => (pinned_rows(items)..items.len()).collect(),
    }
}

/// 挂在第 `at` 行下面的那一串（比它深的，连着的）。主会话那一行比它名下的第一层还高一层。
fn own_rows(items: &[StripItem], at: usize) -> Vec<usize> {
    let level = items[at].level();
    (at + 1..items.len())
        .take_while(|&index| items[index].level() > level)
        .collect()
}

struct Builder<'s, 'j> {
    scope: &'s StripScope<'s>,
    jobs: &'j [JobOverview],
    listed: HashSet<String>,
    items: Vec<StripItem>,
}

impl Builder<'_, '_> {
    /// `owner` 名下的一层：它的子代理（在路上的展开、正在看的实心、别的折起来），再是它自己的
    /// 命令。`chain` 是从这一层往下还没走完的那段路，第一个就是这一层里要展开的那条；`path` 是
    /// 切到这一层的会话时的访问栈；`legacy` 连没挂会话的老任务一起列（只放第一层）。
    fn level(
        &mut self,
        owner: Option<&str>,
        depth: usize,
        indent: &str,
        chain: &[&str],
        path: &[ParentRow],
        legacy: bool,
    ) {
        let next = chain.first().copied();
        let mut rows: Vec<SubagentRow> = owner
            .and_then(|owner| self.children_of(owner))
            .into_iter()
            .flatten()
            .filter(|row| row.is_live() || Some(row.session_id.as_str()) == next)
            .cloned()
            .collect();
        // 这一层的子代理表还没拉回来（刚切进来那一下）：要展开的那条照样先立住。
        if let Some(next) = next.filter(|next| !rows.iter().any(|row| row.session_id == *next)) {
            rows.insert(0, placeholder_row(next, self.scope.path));
        }
        let commands = self.commands_of(owner, legacy);
        let count = rows.len() + commands.len();
        let agent_rows = rows.len();
        for (index, row) in rows.into_iter().enumerate() {
            let last = index + 1 == count;
            let twig = twig(depth, indent, last);
            let mirror = self.mirror_of(&row);
            if Some(row.session_id.as_str()) != next {
                self.items.push(StripItem::Agent {
                    row,
                    place: Place::Other,
                    depth,
                    twig,
                    path: path.to_vec(),
                    mirror,
                });
                continue;
            }
            let place = if chain.len() == 1 {
                Place::Current
            } else {
                Place::Path
            };
            let below = match depth {
                0 => String::new(),
                _ => format!("{indent}{}", if last { "  " } else { "│ " }),
            };
            let mut deeper = path.to_vec();
            deeper.push(ParentRow {
                session_id: row.session_id.clone(),
                title: row.title.clone(),
                root: false,
            });
            let session = row.session_id.clone();
            self.items.push(StripItem::Agent {
                row,
                place,
                depth,
                twig,
                path: path.to_vec(),
                mirror,
            });
            self.level(
                Some(&session),
                depth + 1,
                &below,
                &chain[1..],
                &deeper,
                false,
            );
        }
        for (index, job) in commands.into_iter().enumerate() {
            let twig = twig(depth, indent, agent_rows + index + 1 == count);
            self.items.push(StripItem::Job { job, depth, twig });
        }
    }

    fn children_of(&self, owner: &str) -> Option<&Vec<SubagentRow>> {
        self.scope.children?.get(owner)
    }

    fn mirror_of(&self, row: &SubagentRow) -> Option<JobOverview> {
        let id = row.job_id.as_deref()?;
        self.jobs.iter().find(|job| job.job_id == id).cloned()
    }

    /// `owner` 这条会话自己的后台任务。已经由会话行代表的镜像任务不再单列；还没对上会话行的
    /// （子代理表还没拉回来）照旧列成一行，不至于看不见。`owner` 为 `None`（直连模式）时任务表
    /// 本来就只有这条会话的，全列。
    fn commands_of(&self, owner: Option<&str>, legacy: bool) -> Vec<JobOverview> {
        self.jobs
            .iter()
            .filter(|job| match (owner, job.session_id.as_deref()) {
                (_, None) => legacy,
                (None, Some(_)) => true,
                (Some(owner), Some(session)) => owner == session,
            })
            .filter(|job| !self.listed.contains(&job.job_id))
            .cloned()
            .collect()
    }
}

/// 行首的树枝：第一层不画（和主会话那一行对齐），往下一层挂 `├`/`└`，更深的前面补上父辈那一列
/// 的竖线。
fn twig(depth: usize, indent: &str, last: bool) -> String {
    match depth {
        0 => String::new(),
        _ => format!("{indent}{}", if last { "└ " } else { "├ " }),
    }
}

/// 要展开、但子代理表里还没有的那条：访问栈里有它的标题就用上。
fn placeholder_row(session_id: &str, path: &[ParentRow]) -> SubagentRow {
    SubagentRow {
        session_id: session_id.to_string(),
        title: path
            .iter()
            .find(|row| row.session_id == session_id)
            .map(|row| row.title.clone())
            .unwrap_or_default(),
        state: String::new(),
        dev: false,
        job_id: None,
        running_descendants: 0,
        peek: String::new(),
        tokens_label: String::new(),
        running_since_ms: None,
    }
}

/// 已经由会话行代表了的镜像任务：列出来的子代理（还在干活的，加上路上要展开的）的。
fn listed_mirrors(scope: &StripScope<'_>, chain: &[&str]) -> HashSet<String> {
    let Some(children) = scope.children else {
        return HashSet::new();
    };
    scope
        .path
        .iter()
        .map(|row| row.session_id.as_str())
        .chain(scope.current)
        .filter_map(|owner| children.get(owner))
        .flatten()
        .filter(|row| row.is_live() || chain.contains(&row.session_id.as_str()))
        .filter_map(|row| row.job_id.clone())
        .collect()
}
