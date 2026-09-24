//! 终端的上键历史：按会话存在会话库里。
//!
//! 09-24 起入库，删会话时跟着走；以前是 `state/repl-history/<会话>.jsonl`，删会话
//! 没人清（实测 127/129 是孤儿）。老文件第一次读写时导进库，见
//! `StateStore::repl_history`。
//!
//! 终端会话都在管理员库里，所以这里直接开 `StateStore::new`。进程里的连接是共用的
//! （后台任务的轮询线程一直拿着），上键时再开一次不会重新开库。

use crate::cli::*;

/// 分会话之前的那个全局文件。**只读不写**：老记录都在里面，直接丢掉，用户会
/// 觉得「历史没了」。
pub(super) fn legacy_repl_history_file(paths: &MiyuPaths) -> PathBuf {
    paths.state_dir.join("repl-history.jsonl")
}

pub(super) fn read_repl_history_file(path: &std::path::Path) -> Vec<ReplHistoryEntry> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    content
        .lines()
        .filter_map(ReplHistoryEntry::parse_line)
        .filter(|entry| !entry.display.trim().is_empty())
        .collect()
}

/// Prompt history that survives /reset and restarts. Conversation resets
/// delete turns, so this is the durable source; the turns-derived list only
/// seeds sessions that predate it.
pub(super) fn load_persistent_repl_history(
    paths: &MiyuPaths,
    session_id: &str,
) -> Vec<ReplHistoryEntry> {
    let lines = StateStore::new(paths).and_then(|store| store.repl_history(session_id));
    match lines {
        Ok(lines) => lines
            .iter()
            .filter_map(|line| ReplHistoryEntry::parse_line(line))
            .filter(|entry| !entry.display.trim().is_empty())
            .collect(),
        Err(error) => {
            tracing::warn!(session_id, error = %error, "reading the input history failed");
            Vec::new()
        }
    }
}

pub(super) fn persist_repl_history_entry(
    paths: &MiyuPaths,
    session_id: &str,
    entry: &ReplHistoryEntry,
) {
    if entry.display.trim().is_empty() {
        return;
    }
    let Some(line) = entry.to_json_line() else {
        return;
    };
    let saved =
        StateStore::new(paths).and_then(|store| store.append_repl_history(session_id, &line));
    if let Err(error) = saved {
        tracing::warn!(session_id, error = %error, "saving the input history failed");
    }
}
