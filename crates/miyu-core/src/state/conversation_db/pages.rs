//! 按页查询：界面要显示的那一页回合，不带模型才用的大字段。
//!
//! 给模型组上下文的 `load_turns` 要整段读，还要带上化石（`context_messages`）。
//! 界面只要最近一页、只要显示用的列。可网页打开会话、终端回放、上键历史以前
//! 走的都是模型那条路（09-24 调研）：
//! - 最重的会话打开一次，要解析约 3.5 MB、发出约 3 MB；
//! - 上键历史为了 31 KB 的原话，要读 4.6 MB。

use super::*;

/// 一页回合，按时间从旧到新。
#[derive(Debug, Clone, Default)]
pub struct TurnPage {
    pub turns: Vec<Turn>,
    /// 更早的回合还有：下一页把它当 `before_seq` 往前取。
    pub older: Option<i64>,
    /// 这一页之前那些回合的用量合计（口径同这一页：摘要轮不算，隐藏的照算）。
    /// 网页每轮显示「会话到这一轮为止累计多少」，只拿到一页时从这里起算。
    pub tokens_before: TurnTokens,
}

/// 一页回放快照，按时间从旧到新。
#[derive(Debug, Clone, Default)]
pub struct ReplayPage {
    pub turns: Vec<TurnReplay>,
    /// 更早的回合还有：下一页把它当 `before_seq` 往前取。
    pub older: Option<i64>,
}

/// 这一轮是不是主会话派给子代理的任务：子代理会话的第一轮（会话项目第 3 段）。
///
/// 按会话种类和先后判，不按内容：用户切进子会话自己敲的话照旧是用户气泡。第一轮被
/// 撤销（隐藏）了也不往后顺延，后面那轮不是任务。
fn from_parent_sql() -> String {
    format!(
        "(SELECT kind FROM sessions WHERE sessions.session_id = turns.session_id) = '{kind}'
         AND turns.seq = (SELECT min(earliest.seq) FROM turns AS earliest
                           WHERE earliest.session_id = turns.session_id)",
        kind = crate::state::SUBAGENT_SESSION_KIND,
    )
}

/// 后台任务唤醒那一轮的原文（拆结果段用）；别的轮不取，用户贴的长文不必跟着读出来。
fn job_report_sql() -> String {
    let tag = crate::state::BACKGROUND_JOB_REPORT_TAG;
    format!(
        "CASE WHEN substr(user_content, 1, {}) = '{tag}' THEN user_content END",
        tag.chars().count()
    )
}

fn tokens_before_locked(conn: &Connection, session_id: &str, seq: i64) -> Result<TurnTokens> {
    let (total, prompt, cache_read) = conn.query_row(
        "SELECT COALESCE(SUM(token_total), 0), COALESCE(SUM(token_prompt), 0),
                COALESCE(SUM(token_cache_read), 0)
           FROM turns
          WHERE session_id = ?1 AND is_summary = 0 AND seq < ?2",
        params![session_id, seq],
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
            ))
        },
    )?;
    Ok(TurnTokens {
        total: total.max(0) as u64,
        prompt: prompt.max(0) as u64,
        cache_read: cache_read.max(0) as u64,
    })
}

/// 按「seq 从新到旧」取的行，多取了一条。多出来的那条说明更早的还有：把它
/// 去掉，游标指向这一页最老的那条。
fn page_cursor<T>(rows: &mut Vec<T>, limit: usize, seq: impl Fn(&T) -> i64) -> Option<i64> {
    if rows.len() <= limit {
        return None;
    }
    rows.truncate(limit);
    rows.last().map(seq)
}

impl ConversationDb {
    /// 网页一页要显示的回合：摘要轮不算，隐藏的轮照旧带上（口径同网页原来的整段
    /// 读取）。`context_messages` 只有模型用，这里在固定列序里用 `'[]'` 顶位，
    /// `map_turn_row` 不用动。
    pub fn load_turn_page(
        &self,
        session_id: &str,
        before_seq: Option<i64>,
        limit: usize,
    ) -> Result<TurnPage> {
        if limit == 0 {
            return Ok(TurnPage::default());
        }
        let conn = self.conn.lock().unwrap();
        let columns = history::TURN_COLUMNS.replacen("context_messages", "'[]'", 1);
        let mut stmt = conn.prepare(&format!(
            "SELECT {columns}
               FROM turns
              WHERE session_id = ?1 AND is_summary = 0 AND seq < ?2
              ORDER BY seq DESC
              LIMIT ?3"
        ))?;
        let mut turns = stmt
            .query_map(
                params![session_id, before_seq.unwrap_or(i64::MAX), limit as i64 + 1],
                map_turn_row,
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(stmt);
        let older = page_cursor(&mut turns, limit, |turn| turn.seq);
        turns.reverse();
        attach_turn_children_locked(&conn, &mut turns)?;
        let tokens_before = match turns.first() {
            Some(first) => tokens_before_locked(&conn, session_id, first.seq)?,
            None => TurnTokens::default(),
        };
        Ok(TurnPage {
            turns,
            older,
            tokens_before,
        })
    }

    /// 会话里用户说的第一句（摘要轮不算）。网页拿它当对话标题，按页取的时候第一轮
    /// 不一定在手上。
    pub fn first_user_content(&self, session_id: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        Ok(conn
            .query_row(
                "SELECT user_content FROM turns
                  WHERE session_id = ?1 AND is_summary = 0
                  ORDER BY seq ASC
                  LIMIT 1",
                params![session_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten())
    }

    /// Display transcripts of the last `limit` visible turns of a session,
    /// oldest first. Turns finished before this column existed simply come
    /// back with an empty transcript, and the caller falls back to the plain
    /// prompt/reply pair.
    pub fn session_replay(&self, session_id: &str, limit: usize) -> Result<Vec<TurnReplay>> {
        Ok(self.session_replay_page(session_id, None, limit)?.turns)
    }

    /// `session_replay` 的翻页版：取 `before_seq` 之前的 `limit` 轮。
    pub fn session_replay_page(
        &self,
        session_id: &str,
        before_seq: Option<i64>,
        limit: usize,
    ) -> Result<ReplayPage> {
        if limit == 0 {
            return Ok(ReplayPage::default());
        }
        let conn = self.conn.lock().unwrap();
        // 第四列标出 daemon 自己合成的轮（后台任务唤醒、目标续轮、跨会话消息）。
        // 它们不是用户输入，回放时不能画成用户气泡。判据取 `user_content` 的开头
        // 标签：那是模型真正收到的东西，而 `display_content` 是给人看的，文案
        // 随时可能改。
        // 被中断的轮也回放：它已经进了上下文（模型下一轮看得见它），`/history`
        // 里也有，重开之后正文里却没有，看着像丢了一轮（用户实测：明明有历史
        // 记录，但是没有回放）。
        let mut stmt = conn.prepare(&format!(
            "SELECT display_content, assistant_content, replay_journal,
                    ({synthetic}),
                    assistant_reasoning,
                    status = 'interrupted',
                    assistant_provider_id, assistant_model,
                    turn_id, seq,
                    ({from_parent}),
                    ({job_report}),
                    user_timestamp, assistant_timestamp
               FROM turns
              WHERE session_id = ?1 AND hidden = 0 AND is_summary = 0
                AND status IN ('completed', 'interrupted')
                AND seq < ?2
              ORDER BY seq DESC
              LIMIT ?3",
            synthetic = crate::state::synthetic_user_content_sql("user_content"),
            from_parent = from_parent_sql(),
            job_report = job_report_sql(),
        ))?;
        let mut rows = stmt
            .query_map(
                params![session_id, before_seq.unwrap_or(i64::MAX), limit as i64 + 1],
                |row| {
                    Ok((
                        row.get::<_, i64>(9)?,
                        row.get::<_, Option<String>>(8)?,
                        TurnReplay {
                            seq: row.get(9)?,
                            display_content: row.get::<_, Option<String>>(0)?.unwrap_or_default(),
                            assistant_content: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                            entries: row
                                .get::<_, Option<String>>(2)?
                                .and_then(|json| serde_json::from_str(&json).ok())
                                .unwrap_or_default(),
                            is_synthetic: row.get::<_, i64>(3)? != 0,
                            assistant_reasoning: row.get::<_, Option<String>>(4)?,
                            interrupted: row.get::<_, i64>(5)? != 0,
                            assistant_provider_id: row.get::<_, Option<String>>(6)?,
                            assistant_model: row.get::<_, Option<String>>(7)?,
                            from_parent: row.get::<_, i64>(10)? != 0,
                            job_report: row
                                .get::<_, Option<String>>(11)?
                                .as_deref()
                                .and_then(crate::state::job_report_result),
                            started_at: row.get::<_, Option<String>>(12)?,
                            finished_at: row.get::<_, Option<String>>(13)?,
                            turn_id: row.get::<_, Option<String>>(8)?.unwrap_or_default(),
                        },
                    ))
                },
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(stmt);
        let older = page_cursor(&mut rows, limit, |(seq, _, _)| *seq);
        // 这次改动之前被中断的轮没留下回放快照(见 `interrupt_turn`),可流水账
        // 还在库里——当场收一份,老会话不必迁移也能把正文找回来。
        for (_, turn_id, replay) in rows.iter_mut() {
            if !replay.entries.is_empty() || !replay.interrupted {
                continue;
            }
            let Some(turn_id) = turn_id.as_deref() else {
                continue;
            };
            replay.entries = replay_entries_from_journal(&conn, turn_id).unwrap_or_default();
        }
        let mut turns = rows
            .into_iter()
            .map(|(_, _, mut replay)| {
                heal_clipped_reply(&mut replay);
                replay
            })
            .collect::<Vec<_>>();
        turns.reverse();
        Ok(ReplayPage { turns, older })
    }

    /// 一轮还在跑时，已经流出去的那部分：用户那句、说到一半的话、做过的工具。
    /// 从落库的流水里收（流水先落库再显示，见 `TurnJournalSink`）。挂上来的终端
    /// 用它补：事件环追不回这一轮开头的时候（会话项目第 3 段）。
    pub fn running_turn_replay(&self, turn_id: &str) -> Result<Option<TurnReplay>> {
        let conn = self.conn.lock().unwrap();
        let row = conn
            .query_row(
                &format!(
                    "SELECT seq, display_content, ({synthetic}),
                            assistant_provider_id, assistant_model, ({from_parent}),
                            ({job_report}), user_timestamp
                       FROM turns WHERE turn_id = ?1",
                    synthetic = crate::state::synthetic_user_content_sql("user_content"),
                    from_parent = from_parent_sql(),
                    job_report = job_report_sql(),
                ),
                params![turn_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                        row.get::<_, i64>(2)? != 0,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, i64>(5)? != 0,
                        row.get::<_, Option<String>>(6)?,
                        row.get::<_, Option<String>>(7)?,
                    ))
                },
            )
            .optional()?;
        let Some((
            seq,
            display_content,
            is_synthetic,
            provider,
            model,
            from_parent,
            report,
            started,
        )) = row
        else {
            return Ok(None);
        };
        Ok(Some(TurnReplay {
            seq,
            display_content,
            entries: replay_entries_from_journal(&conn, turn_id)?,
            is_synthetic,
            assistant_provider_id: provider,
            assistant_model: model,
            from_parent,
            job_report: report.as_deref().and_then(crate::state::job_report_result),
            started_at: started,
            turn_id: turn_id.to_string(),
            ..TurnReplay::default()
        }))
    }

    /// 这一轮是不是主会话派给子代理的任务（见 `from_parent_sql`）。挂到子会话正在跑的
    /// 第一轮上时，终端靠它把开头那句画成任务，而不是用户说的话。
    pub fn turn_from_parent(&self, turn_id: &str) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        Ok(conn
            .query_row(
                &format!(
                    "SELECT ({from_parent}) FROM turns WHERE turn_id = ?1",
                    from_parent = from_parent_sql(),
                ),
                params![turn_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .is_some_and(|flag| flag != 0))
    }

    /// 后台任务唤醒那一轮附的结果段（见 `job_report_result`）。终端挂到刚起的唤醒轮上时，
    /// 铃铛那一行点开要看它（09-26）。
    pub fn turn_job_report(&self, turn_id: &str) -> Result<Option<crate::state::JobReportResult>> {
        let conn = self.conn.lock().unwrap();
        let content = conn
            .query_row(
                &format!(
                    "SELECT ({job_report}) FROM turns WHERE turn_id = ?1",
                    job_report = job_report_sql(),
                ),
                params![turn_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten();
        Ok(content.as_deref().and_then(crate::state::job_report_result))
    }

    /// 这个会话里用户说过的话（每轮开头那句，加上中途追加的），按先后排。口径
    /// 同 `load_conversation` 里 role=user 的那几条，但不把整轮读出来。
    pub fn user_inputs(&self, session_id: &str) -> Result<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT content FROM (
                 SELECT t.seq AS turn_seq, 0 AS part, 0 AS sub, t.user_content AS content
                   FROM turns t
                  WHERE t.session_id = ?1 AND t.is_summary = 0
                 UNION ALL
                 SELECT t.seq, 1, q.seq, COALESCE(q.context_content, q.content)
                   FROM queued_prompts q
                   JOIN turns t ON t.turn_id = q.turn_id
                  WHERE t.session_id = ?1 AND t.is_summary = 0 AND q.status = 'consumed'
             )
             ORDER BY turn_seq, part, sub",
        )?;
        let inputs = stmt
            .query_map(params![session_id], |row| {
                Ok(row.get::<_, Option<String>>(0)?.unwrap_or_default())
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(inputs)
    }
}
