//! 会话级零碎状态的读写，连同老文件的一次性导入。
//!
//! 待办、提示词指纹、思考档位钉、终端上键历史，以前是 `state/` 下按会话 id
//! 命名的小文件，会话项目第 1 段把它们搬进了库。
//!
//! 老文件的处理：
//! - 本版保留，回退到上一版还能读到；
//! - 清空、删会话时一并删掉。

use crate::state::conversation_db::{LegacyContent, LegacyKind};
use crate::state::*;

/// 上键历史每个会话留多少条。读老文件时也按这个数截断。
pub const REPL_HISTORY_KEEP: usize = 200;

/// 会话 id 能不能直接当文件名：只认字母、数字、`_`、`-`。放宽了，一个 `../`
/// 就能把路径指到别处。
fn is_plain_session_id(session_id: &str) -> bool {
    !session_id.is_empty()
        && session_id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}

/// 路径片段消毒：只留 `[A-Za-z0-9._-]`，取前 48 位。会话 id 和 turn id 都由
/// 别处生成，进文件名之前一律过这里（spill、compact 转录的目录名就是它）。
pub fn safe_path_segment(raw: &str) -> String {
    raw.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                ch
            } else {
                '_'
            }
        })
        .take(48)
        .collect()
}

/// 老的上键历史文件名：不认的字符换成 `_`，不截断。规则照搬当初写它的
/// `cli::repl_history_file`，差一个字就导不到。
fn repl_history_stem(session_id: &str) -> String {
    session_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn read_legacy(path: &Path, kind: LegacyKind) -> Option<LegacyContent> {
    let text = std::fs::read_to_string(path).ok()?;
    Some(match kind {
        LegacyKind::ReplHistory => {
            let mut lines = text
                .lines()
                .filter(|line| !line.trim().is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>();
            if lines.len() > REPL_HISTORY_KEEP {
                lines.drain(..lines.len() - REPL_HISTORY_KEEP);
            }
            LegacyContent::Lines(lines)
        }
        LegacyKind::Value(SessionValueKind::PromptFingerprint) => {
            LegacyContent::Value(text.trim().to_string())
        }
        LegacyKind::Value(_) => LegacyContent::Value(text),
    })
}

fn remove_if_present(path: &Path) {
    let _ = std::fs::remove_file(path);
}

impl StateStore {
    pub fn session_value(
        &self,
        session_id: &str,
        kind: SessionValueKind,
    ) -> Result<Option<String>> {
        self.import_legacy(session_id, LegacyKind::Value(kind))?;
        self.conv_db.session_value(session_id, kind)
    }

    pub fn set_session_value(
        &self,
        session_id: &str,
        kind: SessionValueKind,
        value: &str,
    ) -> Result<()> {
        self.import_legacy(session_id, LegacyKind::Value(kind))?;
        self.conv_db.set_session_value(session_id, kind, value)
    }

    /// 读、改、写一次做完，见 `ConversationDb::update_session_value`。
    pub fn update_session_value(
        &self,
        session_id: &str,
        kind: SessionValueKind,
        update: impl FnOnce(Option<String>) -> Result<String>,
    ) -> Result<()> {
        self.import_legacy(session_id, LegacyKind::Value(kind))?;
        self.conv_db.update_session_value(session_id, kind, update)
    }

    /// 清空这份值，连老文件一起删：清空之后，老文件不能再被当成新内容导回来。
    pub fn clear_session_value(&self, session_id: &str, kind: SessionValueKind) -> Result<()> {
        self.conv_db.clear_session_value(session_id, kind)?;
        if let Some(path) = self.legacy_file(session_id, LegacyKind::Value(kind)) {
            remove_if_present(&path);
        }
        Ok(())
    }

    /// 上键历史，从旧到新，每条是终端写进去的那一行 JSON。
    pub fn repl_history(&self, session_id: &str) -> Result<Vec<String>> {
        self.import_legacy(session_id, LegacyKind::ReplHistory)?;
        self.conv_db.repl_history(session_id)
    }

    pub fn append_repl_history(&self, session_id: &str, entry: &str) -> Result<()> {
        self.import_legacy(session_id, LegacyKind::ReplHistory)?;
        self.conv_db
            .append_repl_history(session_id, entry, REPL_HISTORY_KEEP)
    }

    fn import_legacy(&self, session_id: &str, kind: LegacyKind) -> Result<()> {
        let path = self.legacy_file(session_id, kind);
        self.conv_db
            .import_legacy_once(session_id, kind, || read_legacy(path.as_deref()?, kind))
    }

    /// 老文件在哪。规则照搬当初写它们的地方，差一个字就导不到：
    /// - 待办：`todos/<会话>.json`（todowrite 工具）；
    /// - 提示词指纹：`prompt-fingerprints/<blake3>.sha256`；
    /// - 思考档位钉：`session-thinking-variants/<会话>.json`，成员在他自己家里；
    /// - 上键历史：`repl-history/<会话>.jsonl`（终端）。
    fn legacy_file(&self, session_id: &str, kind: LegacyKind) -> Option<PathBuf> {
        match kind {
            LegacyKind::Value(SessionValueKind::Todos) => {
                is_plain_session_id(session_id).then(|| {
                    self.state_dir
                        .join("todos")
                        .join(format!("{session_id}.json"))
                })
            }
            LegacyKind::Value(SessionValueKind::PromptFingerprint) => {
                let key = blake3::hash(session_id.as_bytes()).to_hex();
                Some(
                    self.state_dir
                        .join("prompt-fingerprints")
                        .join(format!("{key}.sha256")),
                )
            }
            LegacyKind::Value(SessionValueKind::ThinkingPins) => is_plain_session_id(session_id)
                .then(|| {
                    self.legacy_home
                        .join("session-thinking-variants")
                        .join(format!("{session_id}.json"))
                }),
            LegacyKind::ReplHistory => Some(
                self.state_dir
                    .join("repl-history")
                    .join(format!("{}.jsonl", repl_history_stem(session_id))),
            ),
        }
    }

    /// 删会话时把它散在库外的东西一起删掉：
    /// - 老式会话文件；
    /// - 超长工具输出的落盘（`spill/`）；
    /// - 压缩导出的转录（`compact/`）。
    ///
    /// 转录和落盘的路径只写在这个会话自己的回合里，会话没了，就再也没人引用它们。
    pub(crate) fn remove_session_files(&self, session_id: &str) {
        for kind in [
            LegacyKind::Value(SessionValueKind::Todos),
            LegacyKind::Value(SessionValueKind::PromptFingerprint),
            LegacyKind::Value(SessionValueKind::ThinkingPins),
            LegacyKind::ReplHistory,
        ] {
            if let Some(path) = self.legacy_file(session_id, kind) {
                remove_if_present(&path);
            }
        }
        // 只有 id 原样就是目录名时才删。消毒会把不同的 id 折成同一个名字（截到
        // 48 位），删了别人的就糟了；`.`、`..` 更是会指到上一层。正常的会话 id
        // 都够短、够干净。
        if !is_plain_session_id(session_id) || safe_path_segment(session_id) != session_id {
            return;
        }
        for dir in ["spill", "compact"] {
            let _ = std::fs::remove_dir_all(self.state_dir.join(dir).join(session_id));
        }
    }
}
