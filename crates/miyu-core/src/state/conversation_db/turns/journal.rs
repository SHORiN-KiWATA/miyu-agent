//! 回合跑的过程中逐条落的记录：事件日志、工具报告、化石化的瞬态尾巴、结构化
//! 工具流、足迹、问答。
//!
//! `merge_turn_footprint` 是增量的：工具在回合中途不断产出足迹（读过哪些文件、
//! 记住了什么），要能一次次并进去而不是最后一次性写。

use crate::state::conversation_db::*;

impl ConversationDb {
    #[allow(clippy::too_many_arguments)]
    pub fn append_turn_journal_event(
        &self,
        turn_id: &str,
        revision: i64,
        segment_index: i64,
        kind: &str,
        call_id: Option<&str>,
        name: Option<&str>,
        text_payload: Option<&str>,
        blob_payload: Option<&[u8]>,
        ok: Option<bool>,
    ) -> Result<()> {
        if text_payload.is_some_and(|payload| payload.len() > MAX_JOURNAL_TEXT_EVENT_BYTES) {
            bail!("turn journal text event exceeds the 64 MiB limit");
        }
        if blob_payload.is_some_and(|payload| payload.len() > MAX_JOURNAL_BLOB_EVENT_BYTES) {
            bail!("turn journal binary event exceeds the 8 MiB limit");
        }
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let valid: bool = tx.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM turns t
                 INNER JOIN turn_journal_segments s
                   ON s.turn_id = t.turn_id AND s.revision = t.revision
                  AND s.segment_index = ?3
                 WHERE t.turn_id = ?1 AND t.revision = ?2
                   AND t.status = 'running' AND s.status != 'superseded'
             )",
            params![turn_id, revision, segment_index],
            |row| row.get(0),
        )?;
        if !valid {
            bail!("turn journal generation is no longer active");
        }
        tx.execute(
            "INSERT INTO turn_journal_events
                (turn_id, revision, segment_index, kind, call_id, name,
                 text_payload, blob_payload, ok, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                turn_id,
                revision,
                segment_index,
                kind,
                call_id,
                name,
                text_payload,
                blob_payload,
                ok.map(i64::from),
                Utc::now().to_rfc3339(),
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn supersede_turn_journal_segment(
        &self,
        turn_id: &str,
        revision: i64,
        segment_index: i64,
    ) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let affected = tx.execute(
            "UPDATE turn_journal_segments
             SET status = 'superseded', finished_at = ?1
             WHERE turn_id = ?2 AND revision = ?3 AND segment_index = ?4
               AND status = 'running'",
            params![Utc::now().to_rfc3339(), turn_id, revision, segment_index],
        )?;
        if affected != 1 {
            bail!("turn journal segment changed before supersession");
        }
        tx.execute(
            "INSERT INTO turn_journal_events
                (turn_id, revision, segment_index, kind, created_at)
             VALUES (?1, ?2, ?3, 'generation_superseded', ?4)",
            params![turn_id, revision, segment_index, Utc::now().to_rfc3339()],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// 批量追加。见 [`Self::append_tool_report`]——同样一条读都不做。
    pub fn append_tool_reports(&self, turn_id: &str, reports: &[String]) -> Result<()> {
        if reports.is_empty() {
            return Ok(());
        }
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        {
            let mut stmt =
                tx.prepare("INSERT INTO turn_tool_reports (turn_id, report) VALUES (?1, ?2)")?;
            for report in reports {
                stmt.execute(params![turn_id, report])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Stores the fossilized transient tail for a turn (v7 append-only).
    pub fn set_turn_context_messages(&self, turn_id: &str, messages: &[ChatMessage]) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE turns SET context_messages = ?1 WHERE turn_id = ?2",
            params![serde_json::to_string(messages)?, turn_id],
        )?;
        Ok(())
    }

    /// 完成后落一次结构化工具流。独立 UPDATE 而非扩 complete 签名:调用点多,
    /// 且流为空(无工具回合)时根本不写。
    pub fn set_turn_tool_flow(&self, turn_id: &str, flow: &[ToolFlowRound]) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE turns SET tool_flow = ?1 WHERE turn_id = ?2",
            params![serde_json::to_string(flow)?, turn_id],
        )?;
        Ok(())
    }

    /// Unions `delta` into the turn's stored footprint. Read-modify-write is
    /// safe here: the turn is running and owned by exactly one process.
    pub fn merge_turn_footprint(&self, turn_id: &str, delta: &ToolFootprint) -> Result<()> {
        if delta.is_empty() {
            return Ok(());
        }
        let conn = self.conn.lock().unwrap();
        let existing: Option<Option<String>> = conn
            .query_row(
                "SELECT tool_footprint FROM turns WHERE turn_id = ?1",
                params![turn_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(existing) = existing else {
            return Ok(());
        };
        let mut footprint = existing
            .as_deref()
            .and_then(|json| serde_json::from_str::<ToolFootprint>(json).ok())
            .unwrap_or_default();
        footprint.merge(delta.clone());
        conn.execute(
            "UPDATE turns SET tool_footprint = ?1 WHERE turn_id = ?2",
            params![serde_json::to_string(&footprint)?, turn_id],
        )?;
        Ok(())
    }

    /// Merged footprint across the given turns (summary rows included — they
    /// carry the accumulated footprint of everything they folded).
    pub fn load_merged_footprint(
        &self,
        session_id: &str,
        turn_ids: &[String],
    ) -> Result<ToolFootprint> {
        let conn = self.conn.lock().unwrap();
        let mut merged = ToolFootprint::default();
        let mut stmt = conn
            .prepare("SELECT tool_footprint FROM turns WHERE session_id = ?1 AND turn_id = ?2")?;
        for turn_id in turn_ids {
            let value: Option<Option<String>> = stmt
                .query_row(params![session_id, turn_id], |row| row.get(0))
                .optional()?;
            if let Some(Some(json)) = value {
                if let Ok(footprint) = serde_json::from_str::<ToolFootprint>(&json) {
                    merged.merge(footprint);
                }
            }
        }
        Ok(merged)
    }

    pub fn append_question_exchange(
        &self,
        turn_id: &str,
        exchange: &QuestionExchange,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let next_index: i64 = conn.query_row(
            "SELECT COALESCE(MAX(exchange_index), -1) + 1
             FROM question_exchanges WHERE turn_id = ?1",
            params![turn_id],
            |row| row.get(0),
        )?;
        conn.execute(
            "INSERT INTO question_exchanges (turn_id, exchange_index, payload)
             VALUES (?1, ?2, ?3)",
            params![turn_id, next_index, serde_json::to_string(exchange)?],
        )?;
        Ok(())
    }
}
