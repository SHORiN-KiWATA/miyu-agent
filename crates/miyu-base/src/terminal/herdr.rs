//! herdr 往 pane 里注入的环境变量：谁该带着、谁不该带着。
//!
//! [herdr](https://herdr.dev/) 给 pane 里的进程注入 `HERDR_ENV` /
//! `HERDR_SOCKET_PATH` / `HERDR_BIN_PATH`，外加这个 pane 的三个坐标
//! `HERDR_WORKSPACE_ID` / `HERDR_TAB_ID` / `HERDR_PANE_ID`。装了 herdr 集成的
//! agent（claude / codex / agy 的钩子）一启动就拿 `HERDR_PANE_ID` 往那个 pane
//! 报「这里是我的官方会话」。herdr 0.8.2 从此把同一个 pane 上 `custom:*` 来源的
//! 上报**全部静默丢掉**（它的 `src/terminal/state.rs` 里的 owner conflict，
//! API 照样回成功），直到 pane 关掉都不恢复。
//!
//! 所以只有**真坐在 pane 里的进程**（TUI、一次性 / shellhook）可以带着坐标。
//! daemon 不在任何 pane 里：它由哪个 pane 里的客户端拉起，就继承了哪个 pane
//! 的坐标；中转线的 CLI 再继承 daemon，于是每跑一轮中转线就把那个 pane 认领
//! 一次，Miyu 回合开头报的 working 之后，结尾的 idle 再也进不去（用户 09-23：
//! 「任务完成了 herdr 还显示进行中」，真 herdr 复现见 `testkit/herdr/real_herdr.py`）。

use std::ffi::OsString;

/// 定位一个 pane 的三个坐标。没有它们，herdr 的任何集成都不知道该往哪儿报。
pub const PANE_COORDINATE_VARS: [&str; 3] = ["HERDR_PANE_ID", "HERDR_TAB_ID", "HERDR_WORKSPACE_ID"];

/// 这个进程是不是坐在 herdr 的某个 pane 里。判据和 herdr 自带的集成脚本一样：
/// 盖了 `HERDR_ENV=1` 的章，而且知道自己是哪个 pane。
pub fn in_pane() -> bool {
    in_pane_from(
        std::env::var("HERDR_ENV").ok().as_deref(),
        std::env::var("HERDR_PANE_ID").ok().as_deref(),
    )
}

fn in_pane_from(env: Option<&str>, pane_id: Option<&str>) -> bool {
    env == Some("1") && pane_id.is_some_and(|id| !id.is_empty())
}

/// daemon 启动时调：忘掉拉起它的那个 pane。
///
/// 只去坐标，`HERDR_ENV` 与 socket 路径留着：daemon 给 shellhook 画公式、
/// mermaid 时还靠它们判「herdr 画不画得了 kitty 图」（`kitty.rs`），那一支和
/// pane 归属无关，这次不动它。
///
/// 改进程环境变量不是线程安全的，必须在起任何线程之前调。
pub fn forget_pane_coordinates() {
    for key in PANE_COORDINATE_VARS {
        std::env::remove_var(key);
    }
}

/// 起一个**不在任何 pane 里**的子进程（中转线的 CLI）时，要从它的环境里去掉的
/// 变量：本进程环境里所有的 `HERDR_*`，再加上三个坐标——不管眼下在不在，
/// 一律去掉，daemon 之外的起法（直连模式、测具）也就一起兜住了。
pub fn detached_child_removals() -> Vec<OsString> {
    removals_from(std::env::vars_os().map(|(key, _)| key))
}

fn removals_from(present: impl Iterator<Item = OsString>) -> Vec<OsString> {
    let mut keys: Vec<OsString> = PANE_COORDINATE_VARS.iter().map(OsString::from).collect();
    for key in present {
        let herdr_owned = key.to_str().is_some_and(|name| name.starts_with("HERDR_"));
        if herdr_owned && !keys.contains(&key) {
            keys.push(key);
        }
    }
    keys
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_stamped_process_with_a_pane_id_is_in_a_pane() {
        assert!(in_pane_from(Some("1"), Some("w7:p1")));
        assert!(!in_pane_from(None, Some("w7:p1")));
        assert!(!in_pane_from(Some("0"), Some("w7:p1")));
        assert!(!in_pane_from(Some("1"), None));
        // daemon 忘掉坐标之后就是这个样子：章还在，坐标没了。
        assert!(!in_pane_from(Some("1"), Some("")));
    }

    /// 中转线的 CLI 一个 `HERDR_*` 都不带——坐标尤其不能漏，哪怕眼下的环境里
    /// 恰好没有（直连模式、别的起法）。和 herdr 无关的变量一个不碰。
    #[test]
    fn a_detached_child_drops_every_herdr_variable_and_always_the_coordinates() {
        let present = [
            "HERDR_ENV",
            "HERDR_SOCKET_PATH",
            "HERDR_PANE_ID",
            "PATH",
            "HERDRX",
            "MIYU_HOME",
        ]
        .into_iter()
        .map(OsString::from);
        let removals = removals_from(present);
        for key in PANE_COORDINATE_VARS {
            assert!(removals.contains(&OsString::from(key)), "漏了坐标 {key}");
        }
        assert!(removals.contains(&OsString::from("HERDR_ENV")));
        assert!(removals.contains(&OsString::from("HERDR_SOCKET_PATH")));
        assert!(!removals.contains(&OsString::from("PATH")));
        assert!(!removals.contains(&OsString::from("HERDRX")));
        assert!(!removals.contains(&OsString::from("MIYU_HOME")));
        let unique: std::collections::HashSet<_> = removals.iter().collect();
        assert_eq!(unique.len(), removals.len(), "同一个变量去了两遍");
    }
}
