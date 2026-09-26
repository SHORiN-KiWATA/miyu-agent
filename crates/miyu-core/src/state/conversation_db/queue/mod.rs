//! 排队消息与重做。
//!
//! 回合在跑时用户又发了消息，就进队列。消费队列（`consume_queued_prompts`）必
//! 须和回合状态一起在同一个事务里改，否则会出现「消息出队了但回合没接住」。
//!
//! 重做（redo）先写检查点再动历史：`begin_redo` 之后任何一步失败都能靠
//! `TurnRedoBackup` 退回去。检查点带版本号，格式变了就当没有——**读不懂的检查
//! 点绝不能猜着用**，那会把用户的历史改坏。

mod consume;
mod redo;

use crate::state::conversation_db::*;

impl ConversationDb {
    pub fn enqueue_prompt(
        &self,
        session_id: &str,
        target_turn_id: Option<&str>,
        prompt_id: &str,
        content: &str,
        display_content: &str,
        attachments: &[QueuedPromptAttachment],
        uploaded_attachment_ids: &[String],
        queue_session_id: &str,
        owner_pid: u32,
    ) -> Result<QueuedPrompt> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let target_running: bool = match target_turn_id {
            Some(turn_id) => tx.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM turns
                     WHERE session_id = ?1 AND turn_id = ?2 AND status = 'running'
                       AND queue_session_id = ?3 AND owner_pid = ?4
                 )",
                params![session_id, turn_id, queue_session_id, owner_pid as i64],
                |row| row.get(0),
            )?,
            None => true,
        };
        if !target_running {
            bail!("the target turn is no longer accepting follow-up messages");
        }
        let submitted_at = Utc::now().to_rfc3339();
        let attachments_json = serde_json::to_string(attachments)?;
        tx.execute(
            "INSERT INTO queued_prompts
                (session_id, prompt_id, content, display_content, attachments, status, submitted_at,
                 queue_session_id, owner_pid)
             VALUES (?1, ?2, ?3, ?4, ?5, 'queued', ?6, ?7, ?8)",
            params![
                session_id,
                prompt_id,
                content,
                display_content,
                attachments_json,
                submitted_at,
                queue_session_id,
                owner_pid as i64
            ],
        )?;
        let seq = tx.last_insert_rowid();
        for attachment_id in uploaded_attachment_ids {
            let affected = tx.execute(
                "UPDATE user_attachments SET prompt_id = ?1
                 WHERE session_id = ?2 AND attachment_id = ?3
                   AND turn_id IS NULL AND prompt_id IS NULL AND run_id IS NULL",
                params![prompt_id, session_id, attachment_id],
            )?;
            if affected != 1 {
                bail!("attachment changed before it could be queued: {attachment_id}");
            }
        }
        tx.commit()?;
        drop(conn);
        let uploaded_attachments = self.user_attachments_for_prompt(prompt_id)?;
        Ok(QueuedPrompt {
            prompt_id: prompt_id.to_string(),
            seq,
            content: content.to_string(),
            display_content: display_content.to_string(),
            attachments: attachments.to_vec(),
            uploaded_attachments,
            submitted_at,
        })
    }

    pub fn load_queued_prompts(
        &self,
        session_id: &str,
        queue_session_id: &str,
    ) -> Result<Vec<QueuedPrompt>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT prompt_id, seq, content, display_content, attachments, submitted_at
             FROM queued_prompts
             WHERE status = 'queued' AND session_id = ?1 AND queue_session_id = ?2
             ORDER BY seq ASC",
        )?;
        let mut prompts = stmt
            .query_map(params![session_id, queue_session_id], |row| {
                let attachments_json: String = row.get(4)?;
                let attachments = serde_json::from_str(&attachments_json).unwrap_or_default();
                Ok(QueuedPrompt {
                    prompt_id: row.get(0)?,
                    seq: row.get(1)?,
                    content: row.get(2)?,
                    display_content: row.get(3)?,
                    attachments,
                    uploaded_attachments: Vec::new(),
                    submitted_at: row.get(5)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        attach_prompt_attachments_locked(&conn, &mut prompts)?;
        Ok(prompts)
    }

    /// 排进正在跑的那一轮的后台任务报告附的结果段（见 `job_report_result`）。终端从事件里
    /// 只拿得到给人看的那一行，铃铛点开要看的结果段在库里的原文里（09-26）。
    pub fn queued_job_report(
        &self,
        prompt_id: &str,
    ) -> Result<Option<crate::state::JobReportResult>> {
        let conn = self.conn.lock().unwrap();
        let content = conn
            .query_row(
                "SELECT content FROM queued_prompts WHERE prompt_id = ?1",
                params![prompt_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        Ok(content.as_deref().and_then(crate::state::job_report_result))
    }

    pub(crate) fn user_attachments_for_prompt(
        &self,
        prompt_id: &str,
    ) -> Result<Vec<UserAttachment>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT attachment_id, file_name, mime, kind, size_bytes, width,
                    height, created_at FROM user_attachments
             WHERE prompt_id = ?1 ORDER BY created_at, attachment_id",
        )?;
        let attachments = stmt
            .query_map(params![prompt_id], map_user_attachment_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(attachments)
    }
}
