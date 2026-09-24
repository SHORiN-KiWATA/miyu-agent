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
}

/// 一页回放快照，按时间从旧到新。
#[derive(Debug, Clone, Default)]
pub struct ReplayPage {
    pub turns: Vec<TurnReplay>,
    /// 更早的回合还有：下一页把它当 `before_seq` 往前取。
    pub older: Option<i64>,
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
        Ok(TurnPage { turns, older })
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
                    turn_id, seq
               FROM turns
              WHERE session_id = ?1 AND hidden = 0 AND is_summary = 0
                AND status IN ('completed', 'interrupted')
                AND seq < ?2
              ORDER BY seq DESC
              LIMIT ?3",
            synthetic = crate::state::synthetic_user_content_sql("user_content"),
        ))?;
        let mut rows = stmt
            .query_map(
                params![session_id, before_seq.unwrap_or(i64::MAX), limit as i64 + 1],
                |row| {
                    Ok((
                        row.get::<_, i64>(9)?,
                        row.get::<_, Option<String>>(8)?,
                        TurnReplay {
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
            .map(|(_, _, replay)| replay)
            .collect::<Vec<_>>();
        turns.reverse();
        Ok(ReplayPage { turns, older })
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
