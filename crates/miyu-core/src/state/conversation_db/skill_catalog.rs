//! 技能目录的会话快照（09-24 B13）。
//!
//! `load_skill` 的完整描述里拼着技能目录：目录一变，tools 的字节就变，在线会话下一轮
//! 整段缓存作废。所以按会话冻结——同一个压缩周期里一直发第一次发出去的那份。存在
//! `app_state`（键 `skill_catalog:<会话 id>`），不加迁移；删会话、重置时连带清掉。

use super::*;

const KEY_PREFIX: &str = "skill_catalog:";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillCatalogSnapshot {
    /// 冻结时最近一份摘要的轮 id，没有摘要就是空串。它变了就说明压缩过。
    pub epoch: String,
    pub description: String,
}

fn key(session_id: &str) -> String {
    format!("{KEY_PREFIX}{session_id}")
}

/// 会话已经不在了的快照一并删掉（子代理会话随父会话删掉时也在这里清）。
pub(super) fn prune_orphan_snapshots(tx: &Transaction<'_>) -> Result<()> {
    tx.execute(
        "DELETE FROM app_state
         WHERE key LIKE 'skill_catalog:%'
           AND substr(key, length('skill_catalog:') + 1) NOT IN (SELECT session_id FROM sessions)",
        [],
    )?;
    Ok(())
}

impl ConversationDb {
    pub fn skill_catalog_snapshot(&self, session_id: &str) -> Result<Option<SkillCatalogSnapshot>> {
        let conn = self.conn.lock().unwrap();
        let value: Option<String> = conn
            .query_row(
                "SELECT value FROM app_state WHERE key = ?1",
                params![key(session_id)],
                |row| row.get(0),
            )
            .optional()?;
        Ok(value.and_then(|json| serde_json::from_str(&json).ok()))
    }

    pub fn set_skill_catalog_snapshot(
        &self,
        session_id: &str,
        snapshot: &SkillCatalogSnapshot,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO app_state (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key(session_id), serde_json::to_string(snapshot)?],
        )?;
        Ok(())
    }

    /// 重置会话时连快照一起丢：重置之后的对话从头开始，该看当时的目录。
    pub(super) fn forget_skill_catalog_snapshot(
        tx: &Transaction<'_>,
        session_id: &str,
    ) -> Result<()> {
        tx.execute(
            "DELETE FROM app_state WHERE key = ?1",
            params![key(session_id)],
        )?;
        Ok(())
    }
}
