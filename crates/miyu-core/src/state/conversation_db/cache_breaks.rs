//! 断缓存记录（09-25，判定见 `llm::cache_break`）：一次一行，挂会话级联删除。次数和明细按会话树
//! （主会话 + 名下所有子代理）现算，和 footer 上 Σ 的口径一致。

use crate::llm::{CacheBreak, CacheBreakCause};
use crate::state::conversation_db::*;

/// `/usage` 里列的一条。
#[derive(Debug, Clone, Serialize)]
pub struct CacheBreakRecord {
    pub session_id: String,
    /// RFC 3339，UTC。
    pub at: String,
    pub lost_tokens: u64,
    pub cause: CacheBreakCause,
    pub idle_secs: u64,
}

/// 以 `?1` 为根的会话树。
const TREE: &str = "WITH RECURSIVE tree(session_id) AS (
     SELECT ?1
     UNION ALL
     SELECT child.session_id FROM sessions child JOIN tree ON child.parent_session_id = tree.session_id
      WHERE child.kind = 'subagent'
 )";

impl ConversationDb {
    pub fn record_cache_break(&self, entry: &CacheBreak) -> Result<()> {
        let cause = serde_json::to_string(&entry.cause)?;
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO cache_breaks (session_id, turn_id, at, lost_tokens, cause, idle_secs)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                entry.session_id,
                entry.turn_id,
                Utc::now().to_rfc3339(),
                entry.lost_tokens as i64,
                cause,
                entry.idle_secs as i64,
            ],
        )?;
        Ok(())
    }

    /// 会话树里一共断过几次。
    pub fn cache_break_count(&self, root: &str) -> Result<u64> {
        let conn = self.conn.lock().unwrap();
        let count: i64 = conn.query_row(
            &format!(
                "{TREE} SELECT COUNT(*) FROM cache_breaks WHERE session_id IN (SELECT session_id FROM tree)"
            ),
            params![root],
            |row| row.get(0),
        )?;
        Ok(count.max(0) as u64)
    }

    /// 会话树里最近的几次，新的在前。原因认不出来的（以后加了新种类、老程序来读）略过。
    pub fn recent_cache_breaks(&self, root: &str, limit: usize) -> Result<Vec<CacheBreakRecord>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&format!(
            "{TREE} SELECT session_id, at, lost_tokens, cause, idle_secs FROM cache_breaks
              WHERE session_id IN (SELECT session_id FROM tree)
              ORDER BY id DESC LIMIT ?2"
        ))?;
        let rows = stmt.query_map(params![root, limit as i64], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })?;
        let mut records = Vec::new();
        for row in rows {
            let (session_id, at, lost_tokens, cause, idle_secs) = row?;
            let Ok(cause) = serde_json::from_str::<CacheBreakCause>(&cause) else {
                continue;
            };
            records.push(CacheBreakRecord {
                session_id,
                at,
                lost_tokens: lost_tokens.max(0) as u64,
                cause,
                idle_secs: idle_secs.max(0) as u64,
            });
        }
        Ok(records)
    }
}
