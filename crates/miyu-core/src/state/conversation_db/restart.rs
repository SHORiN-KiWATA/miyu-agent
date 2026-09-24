//! 断点续跑的认领记录（09-24）。
//!
//! 「执行中」的回合（`turns.status = 'running'` + `owner_pid`）本身就是认领。所属进程
//! 死了之后，谁先碰到这个库，谁就在 `recover_stale_running_turns` 里把它收成「已中断」：
//! 新 daemon 自己、终端进程、成员库第一次打开都可能。所以收的那一刻在这一轮的流水账
//! 里记一笔 `restart_orphaned`，下一个 daemon 起来按这笔决定接不接着跑，处理完再记一笔
//! `restart_resume` 结案。
//!
//! 记在流水账而不是新表：不升 schema。升到 v41 会让还没合并的并行分支的二进制拒绝
//! 打开这个库（它们只认到 v40），而流水账的读者（回放、投影、资产恢复）都只认自己
//! 那几种事件，多出来的两种它们看不见。

use crate::state::conversation_db::*;
use chrono::DateTime;

const RESTART_ORPHANED_EVENT: &str = "restart_orphaned";
const RESTART_RESUME_EVENT: &str = "restart_resume";

/// 一条等着决定要不要接着跑的孤儿回合。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestartOrphan {
    pub turn_id: String,
    pub session_id: String,
    pub seq: i64,
    pub user_content: String,
    /// 回合当时的工作目录（`turns.workspace`），续跑的那一轮照旧在这里干活。
    pub workspace: Option<String>,
    /// 进程死之前最后一次动静：流水账最后一条的时间，没有就是回合开始的时间。只进日志：
    /// 打断多久都接着跑（用户 09-24：不设时间范围）。
    pub last_activity: Option<DateTime<Utc>>,
}

/// 在 `recover_stale_running_turns` 的事务里、那一轮刚收成「已中断」之后调。
/// `last_activity` 要在收尾写入流水之前取（收尾会追加 `queued_prompts_consumed`）。
pub(super) fn mark_restart_orphan_locked(
    tx: &Transaction<'_>,
    turn_id: &str,
    revision: i64,
    last_activity: Option<&str>,
) -> Result<()> {
    let payload = serde_json::json!({ "last_activity": last_activity }).to_string();
    insert_lifecycle_event_locked(tx, turn_id, revision, RESTART_ORPHANED_EVENT, &payload)
}

/// 进程死之前这一轮最后一次动静的时间（RFC3339 原文）。
pub(super) fn last_turn_activity_locked(
    tx: &Transaction<'_>,
    turn_id: &str,
    revision: i64,
) -> Result<Option<String>> {
    let last_event: Option<String> = tx.query_row(
        "SELECT MAX(created_at) FROM turn_journal_events
         WHERE turn_id = ?1 AND revision = ?2",
        params![turn_id, revision],
        |row| row.get(0),
    )?;
    if last_event.is_some() {
        return Ok(last_event);
    }
    tx.query_row(
        "SELECT user_timestamp FROM turns WHERE turn_id = ?1",
        params![turn_id],
        |row| row.get(0),
    )
    .optional()
    .map(Option::flatten)
    .map_err(Into::into)
}

/// 生命周期事件挂在这一轮当前修订的最后一段上（流水账的每条事件都属于某一段）。
fn insert_lifecycle_event_locked(
    tx: &Transaction<'_>,
    turn_id: &str,
    revision: i64,
    kind: &str,
    payload: &str,
) -> Result<()> {
    let segment_index: i64 = tx.query_row(
        "SELECT COALESCE(MAX(segment_index), 0) FROM turn_journal_segments
         WHERE turn_id = ?1 AND revision = ?2",
        params![turn_id, revision],
        |row| row.get(0),
    )?;
    tx.execute(
        "INSERT INTO turn_journal_events
            (turn_id, revision, segment_index, kind, text_payload, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            turn_id,
            revision,
            segment_index,
            kind,
            payload,
            Utc::now().to_rfc3339()
        ],
    )?;
    Ok(())
}

impl ConversationDb {
    /// 还没结案的孤儿回合（记过 `restart_orphaned`、没记过 `restart_resume`），按会话
    /// 与先后排好。先按「已中断」筛 turns（有 status 索引），流水账只按 turn_id 去查
    /// ——那张表可能上百万行，不能按事件种类整表扫。
    pub fn pending_restart_orphans(&self) -> Result<Vec<RestartOrphan>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT t.turn_id, t.session_id, t.seq, t.user_content, t.workspace,
                    (SELECT e.text_payload FROM turn_journal_events e
                      WHERE e.turn_id = t.turn_id AND e.kind = ?1
                      ORDER BY e.event_id DESC LIMIT 1)
               FROM turns t
              WHERE t.status = 'interrupted'
                AND EXISTS (SELECT 1 FROM turn_journal_events e
                             WHERE e.turn_id = t.turn_id AND e.kind = ?1)
                AND NOT EXISTS (SELECT 1 FROM turn_journal_events h
                                 WHERE h.turn_id = t.turn_id AND h.kind = ?2)
              ORDER BY t.session_id, t.seq",
        )?;
        let orphans = stmt
            .query_map(
                params![RESTART_ORPHANED_EVENT, RESTART_RESUME_EVENT],
                |row| {
                    let payload: Option<String> = row.get(5)?;
                    Ok(RestartOrphan {
                        turn_id: row.get(0)?,
                        session_id: row.get(1)?,
                        seq: row.get(2)?,
                        user_content: row.get(3)?,
                        workspace: row.get(4)?,
                        last_activity: payload.as_deref().and_then(parse_last_activity),
                    })
                },
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(orphans)
    }

    /// 结案：接着跑了（`resumed`）还是不接（`skipped` / `failed`），`detail` 写为什么。
    /// 结过的案下次启动不再翻出来；同一轮重复结案无害（只认有没有）。
    pub fn close_restart_orphan(&self, turn_id: &str, outcome: &str, detail: &str) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let revision: Option<i64> = tx
            .query_row(
                "SELECT revision FROM turns WHERE turn_id = ?1",
                params![turn_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(revision) = revision else {
            tx.commit()?;
            return Ok(());
        };
        let payload = serde_json::json!({ "outcome": outcome, "detail": detail }).to_string();
        insert_lifecycle_event_locked(&tx, turn_id, revision, RESTART_RESUME_EVENT, &payload)?;
        tx.commit()?;
        Ok(())
    }

    /// 这个会话在 `seq` 之后还有没有别的轮。有的话，人已经接着聊下去了，孤儿不再续。
    pub fn has_turn_after(&self, session_id: &str, seq: i64) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM turns WHERE session_id = ?1 AND seq > ?2)",
            params![session_id, seq],
            |row| row.get(0),
        )
        .map_err(Into::into)
    }

    /// daemon 有序关停时回合的收尾：记下已经花掉的用量，但**不改状态**，库里仍是
    /// 「执行中」。下一个 daemon 把它当孤儿收尾、记一笔认领，才接得着跑；按用户取消
    /// 那样收成「已中断」就分不出来了。用量照记：打断丢账（09-22）同样适用于这里。
    pub fn suspend_turn_with_usage(&self, turn_id: &str, tokens: TurnTokens) -> Result<()> {
        if tokens.total == 0 && tokens.prompt == 0 {
            return Ok(());
        }
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE turns SET token_total = ?2, token_prompt = ?3, token_cache_read = ?4
             WHERE turn_id = ?1 AND status = 'running'",
            params![
                turn_id,
                tokens.total as i64,
                tokens.prompt as i64,
                tokens.cache_read as i64
            ],
        )?;
        Ok(())
    }
}

fn parse_last_activity(payload: &str) -> Option<DateTime<Utc>> {
    let value = serde_json::from_str::<serde_json::Value>(payload).ok()?;
    let text = value.get("last_activity")?.as_str()?;
    DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|time| time.with_timezone(&Utc))
}
