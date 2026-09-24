//! 回合的查询：起始时间、归属会话、会话里最大的序号，以及跑着的回合（有没有、
//! 排队排到哪一轮、摘要）。daemon 重启后把卡在 running 的回合收尾也在这里。

use crate::state::conversation_db::*;

impl ConversationDb {
    /// 这一轮从什么时候开始（RFC3339 原文）。挂到一轮已经在跑的回合上时，输入框
    /// 旁的计时从这儿算起（09-24）。
    pub fn turn_started_at(&self, turn_id: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT user_timestamp FROM turns WHERE turn_id = ?1",
            params![turn_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn turn_session_id(&self, turn_id: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT session_id FROM turns WHERE turn_id = ?1",
            params![turn_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(Into::into)
    }

    /// Largest turn seq in a session (0 when empty).
    pub fn latest_turn_seq(&self, session_id: &str) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        Ok(conn.query_row(
            "SELECT COALESCE(MAX(seq), 0) FROM turns WHERE session_id = ?1",
            params![session_id],
            |row| row.get(0),
        )?)
    }

    pub fn has_running_turns(&self, session_id: &str) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM turns WHERE session_id = ?1 AND status = 'running'",
            params![session_id],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    pub fn running_turn_queue_target(
        &self,
        session_id: &str,
    ) -> Result<Option<(String, Option<String>, Option<u32>)>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT turns.turn_id,
                    COALESCE(
                        turns.queue_session_id,
                        (SELECT queued_prompts.queue_session_id
                           FROM queued_prompts
                          WHERE queued_prompts.owner_pid = turns.owner_pid
                            AND queued_prompts.queue_session_id IS NOT NULL
                          ORDER BY queued_prompts.seq DESC
                          LIMIT 1)
                    ),
                    turns.owner_pid
               FROM turns
              WHERE turns.session_id = ?1 AND turns.status = 'running'
              ORDER BY turns.seq DESC
              LIMIT 1",
            params![session_id],
            |row| {
                let owner_pid = row
                    .get::<_, Option<i64>>(2)?
                    .and_then(|pid| u32::try_from(pid).ok());
                Ok((row.get(0)?, row.get(1)?, owner_pid))
            },
        )
        .optional()
        .map_err(Into::into)
    }

    #[allow(dead_code)]
    pub fn running_turn_summaries(&self, session_id: &str) -> Result<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT user_content FROM turns
             WHERE session_id = ?1 AND status = 'running' ORDER BY seq ASC",
        )?;
        let summaries = stmt
            .query_map(params![session_id], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(summaries)
    }

    pub fn running_turn_summaries_excluding(
        &self,
        session_id: &str,
        exclude_turn_id: &str,
    ) -> Result<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT user_content FROM turns
             WHERE session_id = ?1 AND status = 'running' AND turn_id != ?2 ORDER BY seq ASC",
        )?;
        let summaries = stmt
            .query_map(params![session_id, exclude_turn_id], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(summaries)
    }

    pub fn recover_stale_running_turns(&self) -> Result<Vec<StaleTurnRecovery>> {
        let mut conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT turn_id, session_id, owner_pid, revision, queue_session_id
             FROM turns WHERE status = 'running'",
        )?;
        let stale_turn_ids: Vec<(String, String, i64, Option<String>)> = stmt
            .query_map([], |row| {
                let turn_id: String = row.get(0)?;
                let session_id: String = row.get(1)?;
                let owner_pid: Option<i64> = row.get(2)?;
                let revision: i64 = row.get(3)?;
                let queue_session_id: Option<String> = row.get(4)?;
                Ok((turn_id, session_id, owner_pid, revision, queue_session_id))
            })?
            .filter_map(|row| {
                let (turn_id, session_id, owner_pid, revision, queue_session_id) = row.ok()?;
                let alive = owner_pid
                    .map(|pid| crate::alarm::process_exists(pid as u32))
                    .unwrap_or(false);
                if alive {
                    None
                } else {
                    Some((turn_id, session_id, revision, queue_session_id))
                }
            })
            .collect();
        drop(stmt);
        if stale_turn_ids.is_empty() {
            return Ok(Vec::new());
        }
        let tx = conn.transaction()?;
        let now = Utc::now().to_rfc3339();
        let mut recoveries = Vec::with_capacity(stale_turn_ids.len());
        for (turn_id, session_id, revision, queue_session_id) in &stale_turn_ids {
            if restore_redo_backup_locked(&tx, turn_id, *revision)? {
                recoveries.push(StaleTurnRecovery {
                    turn_id: turn_id.clone(),
                    session_id: session_id.clone(),
                    restored_redo: true,
                });
                continue;
            }
            // 断点续跑的认领要带「死之前最后一次动静」，得在下面收尾写流水之前取。
            let last_activity = restart::last_turn_activity_locked(&tx, turn_id, *revision)?;
            consume_stale_queued_prompts_locked(
                &tx,
                turn_id,
                *revision,
                queue_session_id.as_deref(),
                &now,
            )?;
            let (content, reasoning) = interrupted_projection_locked(&tx, turn_id, *revision)?;
            let turn_affected = tx.execute(
                "UPDATE turns SET assistant_content = ?1, assistant_reasoning = ?2,
                        assistant_timestamp = ?3, status = 'interrupted'
                 WHERE turn_id = ?4 AND revision = ?5 AND status = 'running'",
                params![content, reasoning, now, turn_id, revision],
            )?;
            if turn_affected == 1 {
                bump_completion_seq_locked(&tx, turn_id)?;
                // daemon 换了进程(重启、崩溃)时走的就是这儿:上一条 daemon 跑到
                // 一半的回合在这里被判为陈旧。快照同样要存,否则重开 TUI 只看得
                // 见一句「已中断」,而流水账在库里躺着(用户 09-21 实测)。
                store_replay_journal(&tx, turn_id)?;
                tx.execute(
                    "UPDATE turn_journal_segments
                     SET status = 'interrupted', finished_at = ?1
                     WHERE turn_id = ?2 AND revision = ?3 AND status = 'running'",
                    params![now, turn_id, revision],
                )?;
                // 进程死了才走到这儿（人按停止走的是回合守卫那条路）：记一笔认领，
                // 下一个 daemon 据此决定接不接着跑（09-24 断点续跑，见 restart.rs）。
                restart::mark_restart_orphan_locked(
                    &tx,
                    turn_id,
                    *revision,
                    last_activity.as_deref(),
                )?;
                recoveries.push(StaleTurnRecovery {
                    turn_id: turn_id.clone(),
                    session_id: session_id.clone(),
                    restored_redo: false,
                });
            }
        }
        tx.commit()?;
        Ok(recoveries)
    }
}
