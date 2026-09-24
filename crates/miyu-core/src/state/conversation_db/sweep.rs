//! 按闲置时长清掉没人要的会话：中途被杀的一次性 `ask` 会话、过了保留期的子代理会话。
//!
//! 返回删掉的会话 id，调用方好把它们散在库外的文件一起清掉（09-24 会话项目
//! 第 1 段；以前只删行，spill、转录、artifact 都成了孤儿）。

use super::*;

impl ConversationDb {
    /// Deletes subagent audit sessions older than the retention window;
    /// their turns/images/queues cascade away.
    pub fn delete_subagent_sessions_older_than(&self, days: i64) -> Result<Vec<String>> {
        self.delete_idle_sessions("subagent", &format!("-{days} days"))
    }

    /// Deletes abandoned one-shot sessions older than the retention window. A
    /// `miyu ask` turn deletes its own session; anything still here was
    /// orphaned by a client that died mid-turn (Ctrl+C, SIGKILL).
    pub fn delete_ask_sessions_older_than(&self, hours: i64) -> Result<Vec<String>> {
        self.delete_idle_sessions(crate::state::ASK_SESSION_KIND, &format!("-{hours} hours"))
    }

    /// `idle_for` 是 SQLite 的时间修饰，例如 `-7 days`。
    fn delete_idle_sessions(&self, kind: &str, idle_for: &str) -> Result<Vec<String>> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let session_ids = {
            let mut stmt = tx.prepare(
                "SELECT session_id FROM sessions
                  WHERE kind = ?1 AND datetime(updated_at) < datetime('now', ?2)",
            )?;
            let ids = stmt
                .query_map(params![kind, idle_for], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            ids
        };
        for session_id in &session_ids {
            // queued_prompts.session_id 是 ALTER 加的列，没有级联外键，得先手动
            // 清（同 `delete_session`），否则留孤儿行。
            tx.execute(
                "DELETE FROM queued_prompts WHERE session_id = ?1",
                params![session_id],
            )?;
            tx.execute(
                "DELETE FROM sessions WHERE session_id = ?1",
                params![session_id],
            )?;
        }
        tx.commit()?;
        Ok(session_ids)
    }
}
