//! 重做：找候选、开检查点、回滚。
//!
//! `begin_redo` 是全库最险的一段：它要把历史退回某个回合之前，同时保住后面那批
//! 排队消息、artifact 版本、工具报告。所有改动在一个事务里，失败整体回滚。
//!
//! 检查点带版本号。**读不懂的检查点一律当没有**——猜着用会把用户的历史改坏，
//! 而那是不可逆的。

use crate::state::conversation_db::*;

impl ConversationDb {
    pub fn redo_candidate(&self, session_id: &str) -> Result<Option<RedoCandidate>> {
        let conn = self.conn.lock().unwrap();
        let last = conn
            .query_row(
                "SELECT turn_id, revision, display_content, status, user_content
                 FROM turns
                 WHERE session_id = ?1 AND hidden = 0 AND is_summary = 0
                 ORDER BY seq DESC LIMIT 1",
                params![session_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                },
            )
            .optional()?;
        let Some((turn_id, revision, display_content, status, user_content)) = last else {
            return Ok(None);
        };
        // 最后一轮是 daemon 合成的（后台汇报、目标续轮、跨会话消息）就没有可重做的：
        // 那不是用户发的，重做等于替用户重发一条系统消息，还会把它之后的历史退回去（09-23）。
        if status == "running" || crate::state::is_synthetic_user_content(&user_content) {
            return Ok(None);
        }

        let consumed = {
            let mut stmt = conn.prepare(
                "SELECT prompt_id, display_content
                 FROM queued_prompts
                 WHERE turn_id = ?1 AND status = 'consumed'
                 ORDER BY seq ASC",
            )?;
            let rows = stmt
                .query_map(params![turn_id], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            rows
        };
        if consumed.is_empty() {
            return Ok(Some(RedoCandidate {
                input_id: turn_id.clone(),
                turn_id,
                revision,
                input_kind: RedoInputKind::Initial,
                display_content,
                batch_prompt_ids: Vec::new(),
            }));
        }

        let checkpoint = load_redo_checkpoint_locked(&conn, &turn_id)?;
        let Some(checkpoint) = checkpoint.filter(|checkpoint| checkpoint.payload.is_some()) else {
            return Ok(None);
        };
        if checkpoint.batch_prompt_ids.is_empty()
            || checkpoint.batch_prompt_ids.len() > consumed.len()
        {
            return Ok(None);
        }
        let suffix = &consumed[consumed.len() - checkpoint.batch_prompt_ids.len()..];
        if !suffix
            .iter()
            .map(|(prompt_id, _)| prompt_id)
            .eq(checkpoint.batch_prompt_ids.iter())
        {
            return Ok(None);
        }
        let (input_id, display_content) = suffix.last().cloned().expect("non-empty suffix");
        Ok(Some(RedoCandidate {
            turn_id,
            revision,
            input_id,
            input_kind: RedoInputKind::Followup,
            display_content,
            batch_prompt_ids: checkpoint.batch_prompt_ids,
        }))
    }

    pub fn load_redo_batch_prompts(
        &self,
        session_id: &str,
        turn_id: &str,
        prompt_ids: &[String],
    ) -> Result<Vec<QueuedPrompt>> {
        if prompt_ids.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT prompt_id, seq, COALESCE(context_content, content), display_content,
                    attachments, submitted_at
             FROM queued_prompts
             WHERE session_id = ?1 AND turn_id = ?2 AND status = 'consumed'
             ORDER BY seq ASC",
        )?;
        let mut prompts = stmt
            .query_map(params![session_id, turn_id], |row| {
                Ok(QueuedPrompt {
                    prompt_id: row.get(0)?,
                    seq: row.get(1)?,
                    content: row.get(2)?,
                    display_content: row.get(3)?,
                    attachments: serde_json::from_str(&row.get::<_, String>(4)?)
                        .unwrap_or_default(),
                    uploaded_attachments: Vec::new(),
                    submitted_at: row.get(5)?,
                })
            })?
            .filter_map(|row| match row {
                Ok(prompt) if prompt_ids.contains(&prompt.prompt_id) => Some(Ok(prompt)),
                Ok(_) => None,
                Err(error) => Some(Err(error)),
            })
            .collect::<std::result::Result<Vec<_>, _>>()?;
        drop(stmt);
        if prompts.len() != prompt_ids.len()
            || !prompts
                .iter()
                .map(|prompt| &prompt.prompt_id)
                .eq(prompt_ids.iter())
        {
            bail!("redo follow-up batch changed before it could be loaded");
        }
        attach_prompt_attachments_locked(&conn, &mut prompts)?;
        Ok(prompts)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn begin_redo(
        &self,
        session_id: &str,
        turn_id: &str,
        input_id: &str,
        input_kind: RedoInputKind,
        expected_revision: i64,
        content: &str,
        display_content: &str,
        owner_pid: u32,
        queue_session_id: &str,
    ) -> Result<RedoStart> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let latest = tx
            .query_row(
                "SELECT turn_id, revision, status
                 FROM turns
                 WHERE session_id = ?1 AND hidden = 0 AND is_summary = 0
                 ORDER BY seq DESC LIMIT 1",
                params![session_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?;
        let Some((latest_turn_id, revision, status)) = latest else {
            bail!("redo target no longer exists");
        };
        if latest_turn_id != turn_id || revision != expected_revision || status == "running" {
            bail!("conversation changed before redo could start");
        }
        let other_running: i64 = tx.query_row(
            "SELECT COUNT(*) FROM turns
             WHERE session_id = ?1 AND status = 'running' AND turn_id != ?2",
            params![session_id, turn_id],
            |row| row.get(0),
        )?;
        if other_running != 0 {
            bail!("another turn is already running in this conversation");
        }

        let (
            user_content,
            old_display_content,
            assistant_content,
            assistant_reasoning,
            assistant_provider_id,
            assistant_model,
            assistant_timestamp,
            tool_reports,
            old_owner_pid,
            old_queue_session_id,
            token_total,
            token_usage_estimated,
            token_prompt,
            token_cache_read,
        ) = tx.query_row(
            "SELECT user_content, display_content, assistant_content, assistant_reasoning,
                    assistant_provider_id, assistant_model, assistant_timestamp, tool_reports,
                    owner_pid, queue_session_id, token_total, token_usage_estimated,
                    token_prompt, token_cache_read
             FROM turns WHERE turn_id = ?1",
            params![turn_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                    row.get(10)?,
                    row.get(11)?,
                    row.get(12)?,
                    row.get(13)?,
                ))
            },
        )?;
        // v25：报告分两处（老的在列里，新的在子表）。备份只收列的话，redo 失败
        // 回退时子表那部分就永久没了——而那恰恰是本次回合刚产生的全部报告。
        let tool_reports = {
            let column: String = tool_reports;
            let mut all: Vec<String> = serde_json::from_str(&column).unwrap_or_default();
            let mut stmt = tx.prepare(
                "SELECT report FROM turn_tool_reports WHERE turn_id = ?1 ORDER BY report_id ASC",
            )?;
            let child = stmt
                .query_map(params![turn_id], |row| row.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            all.extend(child);
            serde_json::to_string(&all)?
        };
        let followup = if input_kind == RedoInputKind::Followup {
            tx.query_row(
                "SELECT content, display_content, context_content
                 FROM queued_prompts
                 WHERE prompt_id = ?1 AND turn_id = ?2 AND status = 'consumed'",
                params![input_id, turn_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .optional()?
        } else {
            None
        };
        let loaded_items = {
            let mut stmt = tx.prepare(
                "SELECT kind, name, source_turn_id, created_at, updated_at
                 FROM session_loaded_items WHERE session_id = ?1 ORDER BY kind, name",
            )?;
            let rows = stmt
                .query_map(params![session_id], |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            rows
        };
        let consumed_prompt_ids = {
            let mut stmt = tx.prepare(
                "SELECT prompt_id FROM queued_prompts
                 WHERE turn_id = ?1 AND status = 'consumed' ORDER BY seq",
            )?;
            let rows = stmt
                .query_map(params![turn_id], |row| row.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            rows
        };
        let checkpoint_backup = tx
            .query_row(
                "SELECT version, batch_prompt_ids, payload, unavailable_reason, created_at
                 FROM turn_redo_checkpoints WHERE turn_id = ?1",
                params![turn_id],
                |row| {
                    Ok(RedoCheckpointBackup {
                        version: row.get(0)?,
                        batch_prompt_ids: row.get(1)?,
                        payload: row.get(2)?,
                        unavailable_reason: row.get(3)?,
                        created_at: row.get(4)?,
                    })
                },
            )
            .optional()?;
        let backup = TurnRedoBackup {
            status,
            user_content,
            display_content: old_display_content,
            followup_content: followup.as_ref().map(|value| value.0.clone()),
            followup_display_content: followup.as_ref().map(|value| value.1.clone()),
            followup_context_content: followup.and_then(|value| value.2),
            assistant_content,
            assistant_reasoning,
            assistant_provider_id,
            assistant_model,
            assistant_timestamp,
            tool_reports,
            owner_pid: old_owner_pid,
            queue_session_id: old_queue_session_id,
            token_total,
            token_prompt,
            token_cache_read,
            token_usage_estimated,
            loaded_items,
            consumed_prompt_ids,
            checkpoint: checkpoint_backup,
        };
        let backup_payload = serde_json::to_vec(&backup)?;
        let redo_revision = expected_revision.saturating_add(1);
        let backup_created_at = Utc::now().to_rfc3339();
        tx.execute(
            "INSERT INTO turn_redo_backups (turn_id, revision, payload, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![turn_id, redo_revision, backup_payload, backup_created_at],
        )?;
        tx.execute(
            "INSERT INTO turn_redo_question_backups (turn_id, exchange_index, payload)
             SELECT turn_id, exchange_index, payload FROM question_exchanges WHERE turn_id = ?1",
            params![turn_id],
        )?;
        tx.execute(
            "INSERT INTO turn_redo_image_backups
                (turn_id, asset_id, tool_id, mime, width, height, alt, data, created_at)
             SELECT turn_id, asset_id, tool_id, mime, width, height, alt, data, created_at
             FROM image_assets WHERE turn_id = ?1",
            params![turn_id],
        )?;
        tx.execute(
            "INSERT INTO turn_redo_artifact_backups
                (turn_id, asset_id, tool_id, source_key, file_name, mime, kind,
                 size_bytes, data, created_at, updated_at)
             SELECT ?1, asset_id, tool_id, source_key, file_name, mime, kind,
                    size_bytes, data, created_at, updated_at
             FROM artifact_assets WHERE turn_id = ?1",
            params![turn_id],
        )?;

        let checkpoint = match input_kind {
            RedoInputKind::Initial => {
                if input_id != turn_id {
                    bail!("redo input no longer matches the initial prompt");
                }
                let followups: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM queued_prompts
                     WHERE turn_id = ?1 AND status = 'consumed'",
                    params![turn_id],
                    |row| row.get(0),
                )?;
                if followups != 0 {
                    bail!("the last input changed before redo could start");
                }
                tx.execute(
                    "DELETE FROM question_exchanges WHERE turn_id = ?1",
                    params![turn_id],
                )?;
                tx.execute(
                    "DELETE FROM image_assets WHERE turn_id = ?1",
                    params![turn_id],
                )?;
                tx.execute(
                    "DELETE FROM artifact_assets WHERE turn_id = ?1",
                    params![turn_id],
                )?;
                tx.execute(
                    "DELETE FROM session_loaded_items
                     WHERE session_id = ?1 AND source_turn_id = ?2",
                    params![session_id, turn_id],
                )?;
                tx.execute(
                    "UPDATE turns SET user_content = ?1, display_content = ?2
                     WHERE turn_id = ?3",
                    params![content, display_content, turn_id],
                )?;
                None
            }
            RedoInputKind::Followup => {
                let checkpoint = load_redo_checkpoint_locked(&tx, turn_id)?
                    .and_then(|checkpoint| checkpoint.payload)
                    .context("redo checkpoint is unavailable")?;
                let row = tx
                    .query_row(
                        "SELECT prompt_id FROM queued_prompts
                         WHERE turn_id = ?1 AND status = 'consumed'
                         ORDER BY seq DESC LIMIT 1",
                        params![turn_id],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()?;
                if row.as_deref() != Some(input_id) {
                    bail!("the last follow-up changed before redo could start");
                }
                tx.execute(
                    "DELETE FROM question_exchanges
                     WHERE turn_id = ?1 AND exchange_index >= ?2",
                    params![turn_id, checkpoint.prefix_question_count as i64],
                )?;
                let prefix_assets = checkpoint
                    .prefix_image_asset_ids
                    .iter()
                    .collect::<std::collections::HashSet<_>>();
                let current_assets = {
                    let mut stmt =
                        tx.prepare("SELECT asset_id FROM image_assets WHERE turn_id = ?1")?;
                    let rows = stmt
                        .query_map(params![turn_id], |row| row.get::<_, String>(0))?
                        .collect::<std::result::Result<Vec<_>, _>>()?;
                    rows
                };
                for asset_id in current_assets {
                    if !prefix_assets.contains(&asset_id) {
                        tx.execute(
                            "DELETE FROM image_assets WHERE asset_id = ?1",
                            params![asset_id],
                        )?;
                    }
                }
                let prefix_artifacts = checkpoint
                    .prefix_artifact_asset_ids
                    .iter()
                    .collect::<std::collections::HashSet<_>>();
                let current_artifacts = {
                    let mut stmt =
                        tx.prepare("SELECT asset_id FROM artifact_assets WHERE turn_id = ?1")?;
                    let rows = stmt
                        .query_map(params![turn_id], |row| row.get::<_, String>(0))?
                        .collect::<std::result::Result<Vec<_>, _>>()?;
                    rows
                };
                for asset_id in current_artifacts {
                    if !prefix_artifacts.contains(&asset_id) {
                        tx.execute(
                            "DELETE FROM artifact_assets WHERE asset_id = ?1",
                            params![asset_id],
                        )?;
                    }
                }
                tx.execute(
                    "DELETE FROM session_loaded_items WHERE session_id = ?1",
                    params![session_id],
                )?;
                let now = Utc::now().to_rfc3339();
                for (kind, name, source_turn_id) in &checkpoint.loaded_items {
                    tx.execute(
                        "INSERT INTO session_loaded_items
                            (session_id, kind, name, source_turn_id, created_at, updated_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
                        params![session_id, kind, name, source_turn_id, now],
                    )?;
                }
                tx.execute(
                    "UPDATE queued_prompts
                     SET content = ?1, display_content = ?2, context_content = ?1
                     WHERE prompt_id = ?3 AND turn_id = ?4 AND status = 'consumed'",
                    params![content, display_content, input_id, turn_id],
                )?;
                Some(checkpoint)
            }
        };

        let prefix_reports = checkpoint
            .as_ref()
            .map(|checkpoint| checkpoint.prefix_tool_reports.as_slice())
            .unwrap_or_default();
        let prefix_reports = serde_json::to_string(prefix_reports)?;
        let prefix_question_count = checkpoint
            .as_ref()
            .map(|checkpoint| checkpoint.prefix_question_count)
            .unwrap_or(0);
        let now = Utc::now().to_rfc3339();
        // 列被重置回 prefix 了，子表行也必须一起清——否则上一次尝试产生的报告
        // 会活下来，读回来变成「prefix + 上一轮的全部报告」。
        tx.execute(
            "DELETE FROM turn_tool_reports WHERE turn_id = ?1",
            params![turn_id],
        )?;
        let affected = tx.execute(
            "UPDATE turns SET
                assistant_content = ?1,
                assistant_reasoning = NULL,
                assistant_provider_id = NULL,
                assistant_model = NULL,
                assistant_timestamp = NULL,
                status = 'running',
                tool_reports = ?2,
                owner_pid = ?3,
                queue_session_id = ?4,
                token_total = 0,
                token_usage_estimated = 0,
                revision = revision + 1
             WHERE turn_id = ?5 AND session_id = ?6 AND revision = ?7 AND status != 'running'",
            params![
                PENDING_PLACEHOLDER,
                prefix_reports,
                owner_pid as i64,
                queue_session_id,
                turn_id,
                session_id,
                expected_revision
            ],
        )?;
        if affected != 1 {
            bail!("conversation changed before redo could be claimed");
        }
        tx.execute(
            "UPDATE sessions SET updated_at = ?2 WHERE session_id = ?1",
            params![session_id, now],
        )?;
        tx.execute(
            "INSERT INTO turn_journal_segments
                (turn_id, revision, segment_index, status, started_at)
             VALUES (?1, ?2, 0, 'running', ?3)",
            params![turn_id, redo_revision, now],
        )?;
        tx.execute(
            "INSERT INTO turn_journal_events
                (turn_id, revision, segment_index, kind, text_payload, created_at)
             VALUES (?1, ?2, 0, 'redo_prefix_question_count', ?3, ?4)",
            params![
                turn_id,
                redo_revision,
                prefix_question_count.to_string(),
                now
            ],
        )?;
        tx.commit()?;
        Ok(RedoStart {
            revision: expected_revision.saturating_add(1),
            checkpoint,
        })
    }
}
