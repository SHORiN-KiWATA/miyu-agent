//! 回合的写入与终结。
//!
//! 一个回合有四种终结方式，各有各的落库形态：正常完成、带用量完成、修订版完
//! 成、被打断。分成独立方法而不是一个带 flag 的函数，是因为它们写的列不同，
//! 混在一起很容易漏写某一列。
//!
//! `merge_turn_footprint` 是增量的：工具在回合中途不断产出足迹（读过哪些文件、
//! 记住了什么），要能一次次并进去而不是最后一次性写。

use crate::state::conversation_db::*;

impl ConversationDb {
    pub fn start_turn(
        &self,
        session_id: &str,
        turn_id: &str,
        user_content: &str,
        display_content: &str,
        owner_pid: u32,
        queue_session_id: &str,
        workspace: Option<&str>,
        attachment_run_id: Option<&str>,
    ) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let seq = self.next_seq_locked(&tx, session_id)?;
        let now = Utc::now().to_rfc3339();
        tx.execute(
            "INSERT INTO turns (turn_id, session_id, seq, user_content, display_content, user_timestamp, assistant_content, status, owner_pid, queue_session_id, workspace)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'running', ?8, ?9, ?10)",
            params![
                turn_id,
                session_id,
                seq,
                user_content,
                display_content,
                now,
                PENDING_PLACEHOLDER,
                owner_pid as i64,
                queue_session_id,
                workspace
            ],
        )?;
        tx.execute(
            "INSERT INTO turn_journal_segments
                (turn_id, revision, segment_index, status, started_at)
             VALUES (?1, 0, 0, 'running', ?2)",
            params![turn_id, now],
        )?;
        if let Some(run_id) = attachment_run_id {
            tx.execute(
                "UPDATE user_attachments SET run_id = NULL, turn_id = ?1
                 WHERE session_id = ?2 AND run_id = ?3",
                params![turn_id, session_id, run_id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

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

    #[allow(dead_code)]
    pub fn complete_turn(
        &self,
        turn_id: &str,
        content: &str,
        reasoning: Option<&str>,
    ) -> Result<()> {
        self.complete_turn_with_usage(
            turn_id,
            content,
            reasoning,
            None,
            None,
            TurnTokens::default(),
            false,
        )
    }

    pub fn complete_turn_with_usage(
        &self,
        turn_id: &str,
        content: &str,
        reasoning: Option<&str>,
        provider_id: Option<&str>,
        model: Option<&str>,
        tokens: TurnTokens,
        token_usage_estimated: bool,
    ) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = Utc::now().to_rfc3339();
        let token_usage_estimated = i64::from(token_usage_estimated);
        let affected = tx.execute(
            "UPDATE turns SET assistant_content = ?1, assistant_reasoning = ?2,
                    assistant_provider_id = ?3, assistant_model = ?4, assistant_timestamp = ?5,
                    status = 'completed', token_total = ?6, token_usage_estimated = ?7,
                    token_prompt = ?9, token_cache_read = ?10
              WHERE turn_id = ?8 AND status = 'running'",
            params![
                content,
                reasoning,
                provider_id,
                model,
                now,
                tokens.total as i64,
                token_usage_estimated,
                turn_id,
                tokens.prompt as i64,
                tokens.cache_read as i64
            ],
        )?;
        if affected != 1 {
            bail!("turn changed before it could be completed");
        }
        bump_completion_seq_locked(&tx, turn_id)?;
        // Snapshot the display transcript before the journal goes: the tables
        // below are load-bearing for in-flight turn recovery, so they keep
        // being wiped on completion exactly as before.
        store_replay_journal(&tx, turn_id)?;
        tx.execute(
            "DELETE FROM turn_journal_segments WHERE turn_id = ?1",
            params![turn_id],
        )?;
        touch_session_last_request(&tx, turn_id)?;
        tx.commit()?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn complete_turn_revision_with_usage(
        &self,
        turn_id: &str,
        revision: i64,
        content: &str,
        reasoning: Option<&str>,
        provider_id: Option<&str>,
        model: Option<&str>,
        tokens: TurnTokens,
        token_usage_estimated: bool,
    ) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = Utc::now().to_rfc3339();
        let affected = tx.execute(
            "UPDATE turns SET assistant_content = ?1, assistant_reasoning = ?2,
                    assistant_provider_id = ?3, assistant_model = ?4, assistant_timestamp = ?5,
                    status = 'completed', token_total = ?6, token_usage_estimated = ?7,
                    token_prompt = ?10, token_cache_read = ?11
             WHERE turn_id = ?8 AND revision = ?9 AND status = 'running'",
            params![
                content,
                reasoning,
                provider_id,
                model,
                now,
                tokens.total as i64,
                i64::from(token_usage_estimated),
                turn_id,
                revision,
                tokens.prompt as i64,
                tokens.cache_read as i64
            ],
        )?;
        if affected != 1 {
            bail!("redo generation changed before it could be completed");
        }
        tx.execute(
            "DELETE FROM turn_redo_backups WHERE turn_id = ?1 AND revision = ?2",
            params![turn_id, revision],
        )?;
        // redo 重写了整个回合:先清掉旧修订的重放转写再按新修订快照,
        // 否则重开 REPL 仍显示被弃用的旧回复(空 journal 时也必须清)。
        tx.execute(
            "UPDATE turns SET replay_journal = NULL WHERE turn_id = ?1",
            params![turn_id],
        )?;
        store_replay_journal(&tx, turn_id)?;
        tx.execute(
            "DELETE FROM turn_journal_segments WHERE turn_id = ?1",
            params![turn_id],
        )?;
        touch_session_last_request(&tx, turn_id)?;
        tx.commit()?;
        Ok(())
    }

    pub fn interrupt_turn(&self, turn_id: &str) -> Result<()> {
        self.interrupt_turn_with_usage(turn_id, TurnTokens::default())
    }

    /// 打断也要记账:这一轮已经发出去的请求是真花了钱的。原先这条路只改
    /// status,于是被打断的轮 token 列永远是 0——会话累计 Σ 在打断那一刻
    /// 掉回打断前的基线,本轮烧掉的全部消失(09-22 实测:终端会话某个被打断
    /// 的轮实际 187,217 prompt / 123,520 cache_read,库里记 0/0)。
    /// `tokens` 为零(进程已死、由 `recover_stale_turns` 补标的残留轮拿不到
    /// 用量)时不动那几列,免得把别处写好的数覆盖成 0。
    pub fn interrupt_turn_with_usage(&self, turn_id: &str, tokens: TurnTokens) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let revision: Option<i64> = tx
            .query_row(
                "SELECT revision FROM turns WHERE turn_id = ?1 AND status = 'running'",
                params![turn_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(revision) = revision else {
            tx.commit()?;
            return Ok(());
        };
        let now = Utc::now().to_rfc3339();
        let (content, reasoning) = interrupted_projection_locked(&tx, turn_id, revision)?;
        if tokens.total > 0 || tokens.prompt > 0 {
            tx.execute(
                "UPDATE turns SET assistant_content = ?1, assistant_reasoning = ?2,
                        assistant_timestamp = ?3, status = 'interrupted',
                        token_total = ?6, token_prompt = ?7, token_cache_read = ?8
                 WHERE turn_id = ?4 AND revision = ?5 AND status = 'running'",
                params![
                    content,
                    reasoning,
                    now,
                    turn_id,
                    revision,
                    tokens.total as i64,
                    tokens.prompt as i64,
                    tokens.cache_read as i64
                ],
            )?;
        } else {
            tx.execute(
                "UPDATE turns SET assistant_content = ?1, assistant_reasoning = ?2,
                        assistant_timestamp = ?3, status = 'interrupted'
                 WHERE turn_id = ?4 AND revision = ?5 AND status = 'running'",
                params![content, reasoning, now, turn_id, revision],
            )?;
        }
        bump_completion_seq_locked(&tx, turn_id)?;
        // 跑完的两条路都会在这儿存一份回放快照,中断这条从前没存——于是重开
        // 之后这一轮只剩一句「已中断」,而流水账明明还在(用户 09-21 实测)。
        // segments 照旧不删:in-flight 恢复与 redo 都还要读它们。
        store_replay_journal(&tx, turn_id)?;
        tx.execute(
            "UPDATE turn_journal_segments
             SET status = 'interrupted', finished_at = ?1
             WHERE turn_id = ?2 AND revision = ?3 AND status = 'running'",
            params![now, turn_id, revision],
        )?;
        touch_session_last_request(&tx, turn_id)?;
        tx.commit()?;
        Ok(())
    }

    pub fn interrupt_turn_revision(&self, turn_id: &str, revision: i64) -> Result<bool> {
        self.interrupt_turn_revision_with_usage(turn_id, revision, TurnTokens::default())
    }

    /// 带用量的重做打断。回滚到备份那条路(`restored`)不记账:那一轮恢复成了
    /// 重做之前的内容,原来的数就是对的。
    pub fn interrupt_turn_revision_with_usage(
        &self,
        turn_id: &str,
        revision: i64,
        tokens: TurnTokens,
    ) -> Result<bool> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let restored = restore_redo_backup_locked(&tx, turn_id, revision)?;
        if !restored {
            let (content, reasoning) = interrupted_projection_locked(&tx, turn_id, revision)?;
            let now = Utc::now().to_rfc3339();
            if tokens.total > 0 || tokens.prompt > 0 {
                tx.execute(
                    "UPDATE turns SET assistant_content = ?1, assistant_reasoning = ?2,
                            assistant_timestamp = ?3, status = 'interrupted',
                            token_total = ?6, token_prompt = ?7, token_cache_read = ?8
                     WHERE turn_id = ?4 AND revision = ?5 AND status = 'running'",
                    params![
                        content,
                        reasoning,
                        now,
                        turn_id,
                        revision,
                        tokens.total as i64,
                        tokens.prompt as i64,
                        tokens.cache_read as i64
                    ],
                )?;
            } else {
                tx.execute(
                    "UPDATE turns SET assistant_content = ?1, assistant_reasoning = ?2,
                            assistant_timestamp = ?3, status = 'interrupted'
                     WHERE turn_id = ?4 AND revision = ?5 AND status = 'running'",
                    params![content, reasoning, now, turn_id, revision],
                )?;
            }
            store_replay_journal(&tx, turn_id)?;
            tx.execute(
                "UPDATE turn_journal_segments
                 SET status = 'interrupted', finished_at = ?1
                 WHERE turn_id = ?2 AND revision = ?3 AND status = 'running'",
                params![now, turn_id, revision],
            )?;
        }
        tx.commit()?;
        Ok(restored)
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

    /// 记下该回合的输出速度样本(tokens / 毫秒),都是 0 = 没测到。
    pub fn set_turn_generation(&self, turn_id: &str, tokens: u64, millis: u64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE turns SET generation_tokens = ?1, generation_ms = ?2 WHERE turn_id = ?3",
            params![tokens as i64, millis as i64, turn_id],
        )?;
        Ok(())
    }

    /// 一个会话里每条回合的输出速度样本(turn_id → (tokens, ms)),只带非零的。
    pub fn load_turn_generation(
        &self,
        session_id: &str,
    ) -> Result<std::collections::HashMap<String, (u64, u64)>> {
        let conn = self.conn.lock().unwrap();
        let mut statement = conn.prepare(
            "SELECT turn_id, generation_tokens, generation_ms FROM turns
              WHERE session_id = ?1 AND generation_tokens > 0 AND generation_ms > 0",
        )?;
        let rows = statement.query_map(params![session_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?.max(0) as u64,
                row.get::<_, i64>(2)?.max(0) as u64,
            ))
        })?;
        let mut map = std::collections::HashMap::new();
        for row in rows {
            let (turn_id, tokens, millis) = row?;
            map.insert(turn_id, (tokens, millis));
        }
        Ok(map)
    }

    /// 记下该回合最后一次请求的上下文占用(供应商真实计数)。None = 未知,
    /// 此时上下文表继续用本地估算。
    pub fn set_turn_context_end(&self, turn_id: &str, tokens: Option<u64>) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE turns SET token_context_end = ?1 WHERE turn_id = ?2",
            params![tokens.map(|value| value as i64), turn_id],
        )?;
        Ok(())
    }

    /// 最新一条可见回合的上下文锚点,且仅当它是「已完成的普通回合 + 真实
    /// (非估算)用量」时才算数。摘要行在尾(刚压完)、被打断的回合、估算用量
    /// 一律返回 None —— 那些位置的数字不代表下一次请求的前缀大小。
    /// 供应商最近一次报回来的上下文占用——**包括正在跑的这一轮**。
    ///
    /// `token_context_end` 是每**一次请求**结束就写一次的（见
    /// `turn_loop::stream`），所以一轮里调了五次工具，它就被刷新了五次。取它
    /// 的最新值，拿到的就是「上一次请求结束时」的实测数，这是发下一次请求
    /// 之前能知道的最新事实（用户 09-22：「这个信息不是最新的」）。
    ///
    /// 和 `load_context_anchor` 的差别：那个取「最新一条」再判状态，于是回合
    /// 跑着的时候最新一条就是它自己（running），一律返回 None——那是压缩触发线
    /// 要的语义（宁可退回估算），别动。这里要的恰恰相反：running 那一条的数
    /// 才是最新的。
    pub fn latest_context_end_tokens(&self, session_id: &str) -> Result<Option<u64>> {
        let conn = self.conn.lock().unwrap();
        let tokens: Option<i64> = conn
            .query_row(
                "SELECT token_context_end FROM turns
                  WHERE session_id = ?1 AND hidden = 0 AND is_summary = 0
                    AND token_usage_estimated = 0 AND token_context_end > 0
                  ORDER BY seq DESC LIMIT 1",
                params![session_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(tokens.map(|value| value as u64))
    }

    /// 最近一条带供应商实测占用的已完成普通回合，连同它的 `seq`——**不要求它是
    /// 最新一条**。它之后压缩过（出现摘要行）就不算：摘要改写了前缀，这个数作废。
    ///
    /// 和 `load_context_anchor` 的差别：那个只看最新一条，最新一条被打断了就返回
    /// None，调用方只好把整段历史重拼重数（09-23：打断一次要数两遍，80 万词元的
    /// 会话 debug 下每遍一两秒）。这里把锚点往前找，调用方只估锚点之后那几轮。
    pub fn load_last_measured_anchor(
        &self,
        session_id: &str,
    ) -> Result<Option<(ContextAnchor, i64)>> {
        let conn = self.conn.lock().unwrap();
        let row = conn
            .query_row(
                "SELECT seq, turn_id, assistant_provider_id, assistant_model, token_context_end
                   FROM turns
                  WHERE session_id = ?1 AND hidden = 0 AND is_summary = 0
                    AND status = 'completed' AND token_usage_estimated = 0
                    AND token_context_end > 0
                  ORDER BY seq DESC LIMIT 1",
                params![session_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, i64>(4)?,
                    ))
                },
            )
            .optional()?;
        let Some((seq, turn_id, provider_id, model, tokens)) = row else {
            return Ok(None);
        };
        let compacted_since: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM turns
                            WHERE session_id = ?1 AND hidden = 0 AND is_summary = 1 AND seq > ?2)",
            params![session_id, seq],
            |row| row.get(0),
        )?;
        if compacted_since {
            return Ok(None);
        }
        Ok(Some((
            ContextAnchor {
                turn_id,
                provider_id,
                model,
                tokens: tokens as u64,
            },
            seq,
        )))
    }

    pub fn load_context_anchor(&self, session_id: &str) -> Result<Option<ContextAnchor>> {
        let conn = self.conn.lock().unwrap();
        let row = conn
            .query_row(
                "SELECT turn_id, assistant_provider_id, assistant_model, token_context_end,
                        token_usage_estimated, is_summary, status
                   FROM turns
                  WHERE session_id = ?1 AND hidden = 0
                  ORDER BY seq DESC LIMIT 1",
                params![session_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<i64>>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, String>(6)?,
                    ))
                },
            )
            .optional()?;
        let Some((turn_id, provider_id, model, tokens, estimated, is_summary, status)) = row else {
            return Ok(None);
        };
        if estimated != 0 || is_summary != 0 || status != "completed" {
            return Ok(None);
        }
        let tokens = match tokens {
            Some(value) if value > 0 => value as u64,
            _ => return Ok(None),
        };
        Ok(Some(ContextAnchor {
            turn_id,
            provider_id,
            model,
            tokens,
        }))
    }
}

#[cfg(any(test, feature = "testkit"))]
mod test_support;
