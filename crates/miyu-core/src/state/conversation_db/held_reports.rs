//! 平台会话里还没交出去的后台任务汇报（09-26 起子代理只在后台跑）。
//!
//! QQ 会话里同一轮派出去的几个子代理，要等最后一个跑完、合成一份再起一轮；账号掉线时汇报也
//! 得留着，连上再发（daemon 重启也不丢）。一份汇报一行，按批取，挂会话级联删除。

use crate::state::conversation_db::*;

/// 留着的一份汇报。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldJobReport {
    pub id: i64,
    pub session_id: String,
    /// 哪一批：派这些子代理的那一轮（命令类任务一份自成一批，就是它自己的任务号）。
    pub batch: String,
    pub job_id: String,
    /// 起那一轮的平台发起者（QQ 号）：唤醒轮凭它继承权限。
    pub initiator: Option<String>,
    pub content: String,
}

impl ConversationDb {
    pub fn hold_job_report(
        &self,
        session_id: &str,
        batch: &str,
        job_id: &str,
        initiator: Option<&str>,
        content: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO held_job_reports (session_id, batch, job_id, initiator, content, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                session_id,
                batch,
                job_id,
                initiator,
                content,
                Utc::now().to_rfc3339()
            ],
        )?;
        Ok(())
    }

    /// 这条会话留着的汇报，先来的在前。
    pub fn held_job_reports(&self, session_id: &str) -> Result<Vec<HeldJobReport>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, session_id, batch, job_id, initiator, content FROM held_job_reports
              WHERE session_id = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map(params![session_id], |row| {
            Ok(HeldJobReport {
                id: row.get(0)?,
                session_id: row.get(1)?,
                batch: row.get(2)?,
                job_id: row.get(3)?,
                initiator: row.get(4)?,
                content: row.get(5)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// 有汇报留着的会话（账号连上时挨个补发）。
    pub fn sessions_with_held_job_reports(&self) -> Result<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT session_id FROM held_job_reports GROUP BY session_id ORDER BY MIN(id)",
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// 交出去了（或者认领去交了）：删掉这几行。返回删了几行。
    pub fn release_held_job_reports(&self, ids: &[i64]) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let mut released = 0;
        for id in ids {
            released += conn.execute("DELETE FROM held_job_reports WHERE id = ?1", params![id])?;
        }
        Ok(released)
    }
}
