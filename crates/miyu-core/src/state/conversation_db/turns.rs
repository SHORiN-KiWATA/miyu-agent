//! 回合的写入与终结。
//!
//! 一个回合有四种终结方式，各有各的落库形态：正常完成、带用量完成、修订版完
//! 成、被打断。分成独立方法而不是一个带 flag 的函数，是因为它们写的列不同，
//! 混在一起很容易漏写某一列。
//!
//! 回合跑的过程中逐条落的记录在 `journal`，查跑着的回合与重启收尾在
//! `running`，输出速度与上下文锚点在 `metrics`。

mod journal;
mod metrics;
mod running;

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
}

#[cfg(any(test, feature = "testkit"))]
mod test_support;
