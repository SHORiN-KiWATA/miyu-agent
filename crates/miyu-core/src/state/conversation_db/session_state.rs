//! 会话级零碎状态：待办、提示词指纹、思考档位钉、终端的上键历史。
//!
//! 以前是 `state/` 下按会话 id 命名的一堆小文件，删会话没人清。现在挂在
//! sessions 表下级联删除（表见 `migrations/named.rs`）。
//!
//! 这一层只存文本，格式归各自的主人：待办归工具、上键历史归终端、档位钉归
//! 模型客户端。

use super::*;

/// 每个会话每类存一份文本的状态，也是 `session_values.kind` 的取值。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionValueKind {
    Todos,
    PromptFingerprint,
    ThinkingPins,
}

impl SessionValueKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Todos => "todos",
            Self::PromptFingerprint => "prompt_fingerprint",
            Self::ThinkingPins => "thinking_pins",
        }
    }
}

/// 哪类老文件：`legacy_file_imports.kind` 的取值。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LegacyKind {
    Value(SessionValueKind),
    ReplHistory,
}

impl LegacyKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Value(kind) => kind.as_str(),
            Self::ReplHistory => "repl_history",
        }
    }
}

/// 从老文件读出来、要导进库的内容。
pub(crate) enum LegacyContent {
    /// 整个文件是一份值（待办、指纹、档位钉）。
    Value(String),
    /// 一行一条（上键历史），从旧到新。
    Lines(Vec<String>),
}

fn legacy_imported(conn: &Connection, session_id: &str, kind: LegacyKind) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM legacy_file_imports WHERE session_id = ?1 AND kind = ?2)",
        params![session_id, kind.as_str()],
        |row| row.get(0),
    )?)
}

fn session_exists(conn: &Connection, session_id: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sessions WHERE session_id = ?1)",
        params![session_id],
        |row| row.get(0),
    )?)
}

fn mark_imported(conn: &Connection, session_id: &str, kind: LegacyKind) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO legacy_file_imports (session_id, kind, imported_at)
         VALUES (?1, ?2, ?3)",
        params![session_id, kind.as_str(), Utc::now().to_rfc3339()],
    )?;
    Ok(())
}

impl ConversationDb {
    /// 第一次碰某个会话的某类状态之前调：老文件还没导过，就把 `legacy` 读出来的
    /// 内容导一次，和导入标记放在同一个事务里，这样老内容总排在新写入前面。
    ///
    /// 会话不在这个库里（选错了库）就什么也不做。后面的写入会被外键拦下，把
    /// 选错库的 bug 亮出来。
    pub(crate) fn import_legacy_once(
        &self,
        session_id: &str,
        kind: LegacyKind,
        legacy: impl FnOnce() -> Option<LegacyContent>,
    ) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        if legacy_imported(&conn, session_id, kind)? || !session_exists(&conn, session_id)? {
            return Ok(());
        }
        let content = legacy();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        // 别的进程可能刚刚抢先导完：拿到写锁以后再看一眼。
        if !legacy_imported(&tx, session_id, kind)? {
            let now = Utc::now().to_rfc3339();
            match (kind, content) {
                (LegacyKind::Value(value_kind), Some(LegacyContent::Value(value))) => {
                    tx.execute(
                        "INSERT OR IGNORE INTO session_values (session_id, kind, value, updated_at)
                         VALUES (?1, ?2, ?3, ?4)",
                        params![session_id, value_kind.as_str(), value, now],
                    )?;
                }
                (LegacyKind::ReplHistory, Some(LegacyContent::Lines(lines))) => {
                    for line in lines {
                        tx.execute(
                            "INSERT INTO repl_history (session_id, entry, created_at)
                             VALUES (?1, ?2, ?3)",
                            params![session_id, line, now],
                        )?;
                    }
                }
                _ => {}
            }
            mark_imported(&tx, session_id, kind)?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn session_value(
        &self,
        session_id: &str,
        kind: SessionValueKind,
    ) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        Ok(conn
            .query_row(
                "SELECT value FROM session_values WHERE session_id = ?1 AND kind = ?2",
                params![session_id, kind.as_str()],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn set_session_value(
        &self,
        session_id: &str,
        kind: SessionValueKind,
        value: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO session_values (session_id, kind, value, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(session_id, kind) DO UPDATE SET
                 value = excluded.value,
                 updated_at = excluded.updated_at",
            params![session_id, kind.as_str(), value, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    /// 读、改、写在一个事务里：两个进程同时改同一份（终端和网页同时调档位）时，
    /// 谁也不会把对方刚写的覆盖掉。
    pub fn update_session_value(
        &self,
        session_id: &str,
        kind: SessionValueKind,
        update: impl FnOnce(Option<String>) -> Result<String>,
    ) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = tx
            .query_row(
                "SELECT value FROM session_values WHERE session_id = ?1 AND kind = ?2",
                params![session_id, kind.as_str()],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let next = update(current)?;
        tx.execute(
            "INSERT INTO session_values (session_id, kind, value, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(session_id, kind) DO UPDATE SET
                 value = excluded.value,
                 updated_at = excluded.updated_at",
            params![session_id, kind.as_str(), next, Utc::now().to_rfc3339()],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// 清掉这份值，同时记成「老文件导过了」：清空之后老文件不能再被当成新内容导回来。
    pub fn clear_session_value(&self, session_id: &str, kind: SessionValueKind) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM session_values WHERE session_id = ?1 AND kind = ?2",
            params![session_id, kind.as_str()],
        )?;
        if session_exists(&conn, session_id)? {
            mark_imported(&conn, session_id, LegacyKind::Value(kind))?;
        }
        Ok(())
    }

    /// 上键历史，从旧到新。
    pub fn repl_history(&self, session_id: &str) -> Result<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt =
            conn.prepare("SELECT entry FROM repl_history WHERE session_id = ?1 ORDER BY id")?;
        let entries = stmt
            .query_map(params![session_id], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        Ok(entries)
    }

    /// 记一条上键历史，只留最近 `keep` 条。
    pub fn append_repl_history(&self, session_id: &str, entry: &str, keep: usize) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO repl_history (session_id, entry, created_at) VALUES (?1, ?2, ?3)",
            params![session_id, entry, Utc::now().to_rfc3339()],
        )?;
        tx.execute(
            "DELETE FROM repl_history
              WHERE session_id = ?1
                AND id NOT IN (SELECT id FROM repl_history
                                WHERE session_id = ?1
                                ORDER BY id DESC
                                LIMIT ?2)",
            params![session_id, keep as i64],
        )?;
        tx.commit()?;
        Ok(())
    }
}
