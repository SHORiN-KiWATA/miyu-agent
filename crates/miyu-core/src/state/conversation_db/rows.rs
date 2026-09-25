//! 行映射与子表挂载。
//!
//! 一个回合的完整形态散在六张表里（流水、附件、提问、追加、排队附件……）。
//! `attach_*_locked` 一族把它们挂回同一个 `Turn` 上。
//!
//! 都带 `_locked` 后缀是提醒：这些函数假设调用方已经持有连接锁，自己不再取。

use crate::state::conversation_db::*;

pub(crate) const SESSION_COLUMNS: &str = "session_id, persona, name, kind, parent_session_id, workspace, sandbox_read_all, sandbox_opt_out, sandbox_readonly, archived, created_at, updated_at, sort_key, owner, depth, task_state, spawned_by_turn, background";

pub(crate) fn session_record_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionRecord> {
    Ok(SessionRecord {
        session_id: row.get("session_id")?,
        persona: row.get("persona")?,
        name: row.get("name")?,
        kind: row.get("kind")?,
        parent_session_id: row.get("parent_session_id")?,
        sandbox: row.get("workspace")?,
        sandbox_read_all: row.get("sandbox_read_all")?,
        sandbox_opt_out: row.get("sandbox_opt_out")?,
        sandbox_readonly: row.get("sandbox_readonly")?,
        archived: row.get("archived")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
        sort_key: row.get("sort_key")?,
        owner: row.get("owner")?,
        depth: row.get("depth")?,
        task_state: row.get("task_state")?,
        spawned_by_turn: row.get("spawned_by_turn")?,
        background: row.get("background")?,
    })
}

pub fn interrupted_prefix(content: &str) -> String {
    let suffix = format!("\n\n{INTERRUPTED_TEXT}");
    content
        .strip_suffix(&suffix)
        .unwrap_or_else(|| content.strip_suffix(INTERRUPTED_TEXT).unwrap_or(content))
        .to_string()
}

#[allow(dead_code)]
pub fn pending_placeholder() -> &'static str {
    PENDING_PLACEHOLDER
}

#[allow(dead_code)]
pub fn interrupted_text() -> &'static str {
    INTERRUPTED_TEXT
}

pub(crate) fn map_turn_row(row: &rusqlite::Row) -> rusqlite::Result<Turn> {
    let tool_reports_json: String = row.get(11)?;
    let tool_reports: Vec<String> = serde_json::from_str(&tool_reports_json).unwrap_or_default();
    let context_messages_json: String = row.get::<_, Option<String>>(18)?.unwrap_or_default();
    let context_messages: Vec<ChatMessage> =
        serde_json::from_str(&context_messages_json).unwrap_or_default();
    let tool_flow_json: String = row.get::<_, Option<String>>(21)?.unwrap_or_default();
    let tool_flow: Vec<ToolFlowRound> = serde_json::from_str(&tool_flow_json).unwrap_or_default();
    Ok(Turn {
        turn_id: row.get(0)?,
        seq: row.get(1)?,
        user_content: row.get(2)?,
        display_content: row.get(3)?,
        user_timestamp: row.get(4)?,
        assistant_content: row.get(5)?,
        assistant_reasoning: row.get(6)?,
        assistant_provider_id: row.get(7)?,
        assistant_model: row.get(8)?,
        assistant_timestamp: row.get(9)?,
        status: TurnStatus::from_str(row.get::<_, String>(10)?.as_str()),
        tool_reports,
        tool_flow,
        question_exchanges: Vec::new(),
        followups: Vec::new(),
        attachments: Vec::new(),
        hidden: row.get::<_, i64>(12)? != 0,
        is_summary: row.get::<_, i64>(13)? != 0,
        owner_pid: row.get(14)?,
        token_total: row.get::<_, i64>(15)?.max(0) as u64,
        token_prompt: row.get::<_, i64>(19)?.max(0) as u64,
        token_cache_read: row.get::<_, i64>(20)?.max(0) as u64,
        token_usage_estimated: row.get::<_, i64>(16)? != 0,
        revision: row.get(17)?,
        journal_events: Vec::new(),
        context_messages,
    })
}

pub(crate) fn map_user_attachment_row(row: &rusqlite::Row) -> rusqlite::Result<UserAttachment> {
    Ok(UserAttachment {
        attachment_id: row.get(0)?,
        file_name: row.get(1)?,
        mime: row.get(2)?,
        kind: row.get(3)?,
        size_bytes: row.get::<_, i64>(4)?.max(0) as u64,
        width: row.get::<_, i64>(5)?.max(0) as u32,
        height: row.get::<_, i64>(6)?.max(0) as u32,
        created_at: row.get(7)?,
    })
}

pub(crate) fn map_image_asset_row(row: &rusqlite::Row) -> rusqlite::Result<ImageAsset> {
    Ok(ImageAsset {
        asset_id: row.get(0)?,
        turn_id: row.get(1)?,
        tool_id: row.get(2)?,
        mime: row.get(3)?,
        width: row.get::<_, i64>(4)?.max(0) as u32,
        height: row.get::<_, i64>(5)?.max(0) as u32,
        alt: row.get(6)?,
        created_at: row.get(7)?,
    })
}

pub(crate) fn map_artifact_asset_row(row: &rusqlite::Row) -> rusqlite::Result<ArtifactAsset> {
    Ok(ArtifactAsset {
        asset_id: row.get(0)?,
        turn_id: row.get(1)?,
        tool_id: row.get(2)?,
        source_key: row.get(3)?,
        file_name: row.get(4)?,
        mime: row.get(5)?,
        kind: row.get(6)?,
        size_bytes: row.get::<_, i64>(7)?.max(0) as u64,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
    })
}

pub(crate) fn attach_turn_children_locked(conn: &Connection, turns: &mut [Turn]) -> Result<()> {
    attach_tool_reports_locked(conn, turns)?;
    attach_question_exchanges_locked(conn, turns)?;
    attach_followups_locked(conn, turns)?;
    attach_turn_attachments_locked(conn, turns)?;
    attach_turn_journal_events_locked(conn, turns)
}

/// Folds the live journal of a just-finished turn into `turns.replay_journal`.
/// Progress ticks and command output blobs — the parts only the live view
/// needed — are dropped; what is left is the ordered prose/思考/tool sequence
/// the REPL redraws when the session is reopened.
pub(crate) fn store_replay_journal(tx: &Transaction, turn_id: &str) -> Result<()> {
    let mut entries = replay_entries_from_journal(tx, turn_id)?;
    if entries.is_empty() {
        return Ok(());
    }
    // Whole-turn budget for the tool and thinking entries: drop the oldest of
    // them, so what survives is the tail the user was actually looking at when
    // the turn ended. Prose is exempt — it is what the user reads, and a long
    // reply used to come back cut at 2048 chars (09-25).
    let mut used: usize = entries.iter().map(budget_cost).sum();
    let mut index = 0;
    while used > REPLAY_JOURNAL_MAX_CHARS && index < entries.len() {
        let cost = budget_cost(&entries[index]);
        if cost == 0 {
            index += 1;
            continue;
        }
        used -= cost;
        entries.remove(index);
    }
    let encoded = serde_json::to_string(&entries)?;
    tx.execute(
        "UPDATE turns SET replay_journal = ?1 WHERE turn_id = ?2",
        params![encoded, turn_id],
    )?;
    Ok(())
}

/// 一条回放条目占整轮预算多少：正文不占（见 `store_replay_journal`）。
fn budget_cost(entry: &ReplayEntry) -> usize {
    match entry {
        ReplayEntry::Text { .. } => 0,
        other => serde_json::to_string(other).map_or(0, |json| json.len()),
    }
}

/// 09-25 之前存下的快照把正文截在 `REPLAY_ENTRY_MAX_CHARS` 字、后面补一个「…」。最后一段
/// 正文就是这一轮交出去的回复时，`assistant_content` 里有全文，照它补回来；认不出来的
/// （截的是中间正文、几段连着的正文拼在一起）原样留着。
pub(crate) fn heal_clipped_reply(replay: &mut TurnReplay) {
    let Some(ReplayEntry::Text { text }) = replay.entries.last_mut() else {
        return;
    };
    let Some(kept) = text.strip_suffix('…') else {
        return;
    };
    if kept.chars().count() != REPLAY_ENTRY_MAX_CHARS {
        return;
    }
    let full = if replay.interrupted {
        interrupted_prefix(&replay.assistant_content)
    } else {
        replay.assistant_content.clone()
    };
    if full.len() > kept.len() && full.starts_with(kept) {
        *text = full;
    }
}

/// 流水账收成回放条目。只读——中断轮的快照当场没落下时，回放那条路拿它
/// 现收一份（`session_replay`），老会话不必迁移就能把正文找回来。
pub(crate) fn replay_entries_from_journal(
    conn: &Connection,
    turn_id: &str,
) -> Result<Vec<ReplayEntry>> {
    // 只取当前修订的事件:被 redo 的 interrupted 回合会同时残留新旧两个
    // revision 的事件(interrupt 不删 segments),混着快照会串台。
    let mut stmt = conn.prepare(
        "SELECT kind, call_id, name, text_payload, ok, created_at
           FROM turn_journal_events
          WHERE turn_id = ?1
            AND revision = (SELECT revision FROM turns WHERE turn_id = ?1)
            AND kind IN ('assistant_content', 'assistant_reasoning',
                         'tool_call', 'tool_result')
          ORDER BY event_id",
    )?;
    let rows = stmt
        .query_map(params![turn_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<i64>>(4)?,
                row.get::<_, Option<String>>(5)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    // 回合开始的时刻：第一段思考是从这儿起算的。
    let turn_started: Option<chrono::DateTime<chrono::Utc>> = conn
        .query_row(
            "SELECT user_timestamp FROM turns WHERE turn_id = ?1",
            params![turn_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .ok()
        .flatten()
        .and_then(|text| parse_journal_time(&text));

    let mut entries: Vec<ReplayEntry> = Vec::new();
    let mut call_names: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    // 每次调用是什么时候发出去的：结果回来时一减就是这一步的耗时。
    let mut call_times: std::collections::HashMap<String, chrono::DateTime<chrono::Utc>> =
        std::collections::HashMap::new();
    // 上一条事件的时刻。一段思考的耗时 = 它最后一条事件的时刻 − 它开始之前那
    // 一刻（思考是攒批落的，同一段可能只有一条事件，自己跟自己减恒为零）。
    let mut prev_at = turn_started;
    let mut reasoning_from: Option<chrono::DateTime<chrono::Utc>> = None;
    let mut text = String::new();
    // 正文不截：它就是人要读的那部分（见 `store_replay_journal`）。
    let flush_text = |entries: &mut Vec<ReplayEntry>, text: &mut String| {
        if !text.trim().is_empty() {
            entries.push(ReplayEntry::Text {
                text: std::mem::take(text),
            });
        }
        text.clear();
    };
    for (kind, call_id, name, payload, ok, created_at) in rows {
        let at = created_at.as_deref().and_then(parse_journal_time);
        let span = |from: Option<chrono::DateTime<chrono::Utc>>| -> u64 {
            match (from, at) {
                (Some(from), Some(at)) => (at - from).num_milliseconds().max(0) as u64,
                _ => 0,
            }
        };
        if kind != "assistant_reasoning" {
            reasoning_from = None;
        }
        match kind.as_str() {
            "assistant_content" => text.push_str(payload.as_deref().unwrap_or_default()),
            "assistant_reasoning" => {
                let chunk = payload.as_deref().unwrap_or_default();
                if chunk.trim().is_empty() {
                    prev_at = at.or(prev_at);
                    continue;
                }
                let from = reasoning_from.or(prev_at);
                reasoning_from = Some(from.unwrap_or_else(chrono::Utc::now));
                // 同一段思考是分好几条事件落下来的（journal 按字节攒批），
                // 挨着的就并回一条，别在时间线上排成一串「已思考」。
                if let Some(ReplayEntry::Reasoning {
                    text: last,
                    elapsed_ms,
                }) = entries.last_mut()
                {
                    last.push_str(chunk);
                    *elapsed_ms = span(from);
                } else {
                    flush_text(&mut entries, &mut text);
                    entries.push(ReplayEntry::Reasoning {
                        text: chunk.to_string(),
                        elapsed_ms: span(from),
                    });
                }
            }
            "tool_call" => {
                flush_text(&mut entries, &mut text);
                let Some(name) = name else { continue };
                if let Some(call_id) = call_id {
                    call_names.insert(call_id.clone(), name.clone());
                    if let Some(at) = at {
                        call_times.insert(call_id, at);
                    }
                }
                entries.push(ReplayEntry::ToolCall {
                    name,
                    arguments: truncate_chars_owned(
                        payload.as_deref().unwrap_or_default(),
                        REPLAY_ENTRY_MAX_CHARS,
                    ),
                });
            }
            "tool_result" => {
                flush_text(&mut entries, &mut text);
                let name = call_id
                    .as_deref()
                    .and_then(|id| call_names.get(id).cloned())
                    .or(name)
                    .unwrap_or_default();
                if name.is_empty() {
                    continue;
                }
                let started = call_id
                    .as_deref()
                    .and_then(|id| call_times.get(id).copied());
                entries.push(ReplayEntry::ToolResult {
                    name,
                    ok: ok.unwrap_or(1) != 0,
                    output: truncate_chars_owned(
                        payload.as_deref().unwrap_or_default(),
                        REPLAY_ENTRY_MAX_CHARS,
                    ),
                    elapsed_ms: span(started),
                });
            }
            _ => {}
        }
        prev_at = at.or(prev_at);
    }
    flush_text(&mut entries, &mut text);
    for entry in &mut entries {
        if let ReplayEntry::Reasoning { text, .. } = entry {
            *text = truncate_chars_owned(text, REPLAY_REASONING_MAX_CHARS);
        }
    }
    Ok(entries)
}

/// journal 里的时刻是 RFC3339。解不出来就当没有——耗时退回 0，不至于连回放
/// 都没了。
fn parse_journal_time(text: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(text.trim())
        .ok()
        .map(|at| at.with_timezone(&chrono::Utc))
}

pub(crate) fn truncate_chars_owned(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_string();
    }
    let kept: String = value.chars().take(max).collect();
    format!("{kept}…")
}

pub(crate) fn attach_turn_journal_events_locked(
    conn: &Connection,
    turns: &mut [Turn],
) -> Result<()> {
    // BTreeMap keeps the chunking below deterministic; HashMap iteration order
    // would shuffle turn ids across the 900-id chunks between calls.
    let indexes = turns
        .iter()
        .enumerate()
        .filter(|(_, turn)| turn.status != TurnStatus::Completed)
        .map(|(index, turn)| (turn.turn_id.clone(), index))
        .collect::<std::collections::BTreeMap<_, _>>();
    if indexes.is_empty() {
        return Ok(());
    }
    let turn_ids = indexes.keys().collect::<Vec<_>>();
    for chunk in turn_ids.chunks(900) {
        let placeholders = std::iter::repeat_n("?", chunk.len())
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT e.turn_id, e.event_id, e.revision, e.segment_index, e.kind,
                    e.call_id, e.name, e.text_payload, e.blob_payload, e.ok
             FROM turn_journal_events e
             INNER JOIN turn_journal_segments s
               ON s.turn_id = e.turn_id AND s.revision = e.revision
              AND s.segment_index = e.segment_index
             INNER JOIN turns t ON t.turn_id = e.turn_id AND t.revision = e.revision
             WHERE e.turn_id IN ({placeholders})
                AND (
                    s.status != 'superseded'
                    OR (
                        e.kind IN (
                            'tool_call', 'tool_result', 'tool_progress',
                            'command_stdout', 'command_stderr', 'image', 'artifact'
                        )
                        AND EXISTS(
                            SELECT 1 FROM turn_journal_events result_event
                            WHERE result_event.turn_id = e.turn_id
                              AND result_event.revision = e.revision
                              AND result_event.segment_index = e.segment_index
                              AND result_event.kind = 'tool_result'
                              AND result_event.call_id = e.call_id
                        )
                    )
                )
             ORDER BY e.event_id"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |row| {
            Ok((
                row.get::<_, String>(0)?,
                TurnJournalEvent {
                    event_id: row.get(1)?,
                    revision: row.get(2)?,
                    segment_index: row.get(3)?,
                    kind: row.get(4)?,
                    call_id: row.get(5)?,
                    name: row.get(6)?,
                    text_payload: row.get(7)?,
                    blob_payload: row.get(8)?,
                    ok: row.get::<_, Option<i64>>(9)?.map(|value| value != 0),
                },
            ))
        })?;
        for row in rows {
            let (turn_id, event) = row?;
            if let Some(index) = indexes.get(&turn_id).copied() {
                turns[index].journal_events.push(event);
            }
        }
    }
    Ok(())
}

pub(crate) fn attach_turn_attachments_locked(conn: &Connection, turns: &mut [Turn]) -> Result<()> {
    if turns.is_empty() {
        return Ok(());
    }
    let indexes = turns
        .iter()
        .enumerate()
        .map(|(index, turn)| (turn.turn_id.clone(), index))
        .collect::<std::collections::HashMap<_, _>>();
    let turn_ids = indexes.keys().collect::<Vec<_>>();
    for chunk in turn_ids.chunks(900) {
        let placeholders = std::iter::repeat_n("?", chunk.len())
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT turn_id, attachment_id, file_name, mime, kind, size_bytes,
                    width, height, created_at FROM user_attachments
             WHERE turn_id IN ({placeholders}) ORDER BY created_at, attachment_id"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |row| {
            Ok((
                row.get::<_, String>(0)?,
                UserAttachment {
                    attachment_id: row.get(1)?,
                    file_name: row.get(2)?,
                    mime: row.get(3)?,
                    kind: row.get(4)?,
                    size_bytes: row.get::<_, i64>(5)?.max(0) as u64,
                    width: row.get::<_, i64>(6)?.max(0) as u32,
                    height: row.get::<_, i64>(7)?.max(0) as u32,
                    created_at: row.get(8)?,
                },
            ))
        })?;
        for row in rows {
            let (turn_id, attachment) = row?;
            if let Some(index) = indexes.get(&turn_id).copied() {
                turns[index].attachments.push(attachment);
            }
        }
    }
    Ok(())
}

pub(crate) fn attach_question_exchanges_locked(
    conn: &Connection,
    turns: &mut [Turn],
) -> Result<()> {
    if turns.is_empty() {
        return Ok(());
    }
    let indexes = turns
        .iter()
        .enumerate()
        .map(|(index, turn)| (turn.turn_id.clone(), index))
        .collect::<std::collections::HashMap<_, _>>();
    let turn_ids = indexes.keys().collect::<Vec<_>>();
    for chunk in turn_ids.chunks(900) {
        let placeholders = std::iter::repeat_n("?", chunk.len())
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT turn_id, payload FROM question_exchanges
             WHERE turn_id IN ({placeholders}) ORDER BY turn_id, exchange_index"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (turn_id, payload) = row?;
            let Some(index) = indexes.get(&turn_id).copied() else {
                continue;
            };
            let exchange = serde_json::from_str::<QuestionExchange>(&payload)
                .with_context(|| format!("invalid question exchange for turn {turn_id}"))?;
            turns[index].question_exchanges.push(exchange);
        }
    }
    Ok(())
}

pub(crate) fn attach_followups_locked(conn: &Connection, turns: &mut [Turn]) -> Result<()> {
    if turns.is_empty() {
        return Ok(());
    }
    let indexes = turns
        .iter()
        .enumerate()
        .map(|(index, turn)| (turn.turn_id.clone(), index))
        .collect::<std::collections::HashMap<_, _>>();
    let turn_ids = indexes.keys().collect::<Vec<_>>();
    for chunk in turn_ids.chunks(900) {
        let placeholders = std::iter::repeat_n("?", chunk.len())
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT prompt_id, turn_id, COALESCE(context_content, content), display_content,
                    attachments, submitted_at, preceding_assistant_content,
                    preceding_assistant_reasoning, preceding_assistant_provider_id,
                    preceding_assistant_model, COALESCE(context_messages, '[]')
             FROM queued_prompts
             WHERE status = 'consumed' AND turn_id IN ({placeholders})
             ORDER BY seq ASC"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |row| {
            Ok((
                row.get::<_, String>(1)?,
                TurnFollowup {
                    prompt_id: row.get(0)?,
                    content: row.get(2)?,
                    display_content: row.get(3)?,
                    attachments: serde_json::from_str(&row.get::<_, String>(4)?)
                        .unwrap_or_default(),
                    uploaded_attachments: Vec::new(),
                    submitted_at: row.get(5)?,
                    preceding_assistant_content: row.get(6)?,
                    preceding_assistant_reasoning: row.get(7)?,
                    preceding_assistant_provider_id: row.get(8)?,
                    preceding_assistant_model: row.get(9)?,
                    context_messages_json: row.get(10)?,
                },
            ))
        })?;
        for row in rows {
            let (turn_id, followup) = row?;
            let Some(index) = indexes.get(&turn_id).copied() else {
                continue;
            };
            turns[index].followups.push(followup);
        }
    }
    attach_followup_attachments_locked(conn, turns)?;
    Ok(())
}

pub(crate) fn attach_prompt_attachments_locked(
    conn: &Connection,
    prompts: &mut [QueuedPrompt],
) -> Result<()> {
    let indexes = prompts
        .iter()
        .enumerate()
        .map(|(index, prompt)| (prompt.prompt_id.clone(), index))
        .collect::<std::collections::HashMap<_, _>>();
    if indexes.is_empty() {
        return Ok(());
    }
    let prompt_ids = indexes.keys().collect::<Vec<_>>();
    for chunk in prompt_ids.chunks(900) {
        let placeholders = std::iter::repeat_n("?", chunk.len())
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT prompt_id, attachment_id, file_name, mime, kind, size_bytes,
                    width, height, created_at FROM user_attachments
             WHERE prompt_id IN ({placeholders}) ORDER BY created_at, attachment_id"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |row| {
            Ok((
                row.get::<_, String>(0)?,
                UserAttachment {
                    attachment_id: row.get(1)?,
                    file_name: row.get(2)?,
                    mime: row.get(3)?,
                    kind: row.get(4)?,
                    size_bytes: row.get::<_, i64>(5)?.max(0) as u64,
                    width: row.get::<_, i64>(6)?.max(0) as u32,
                    height: row.get::<_, i64>(7)?.max(0) as u32,
                    created_at: row.get(8)?,
                },
            ))
        })?;
        for row in rows {
            let (prompt_id, attachment) = row?;
            if let Some(index) = indexes.get(&prompt_id).copied() {
                prompts[index].uploaded_attachments.push(attachment);
            }
        }
    }
    Ok(())
}

pub(crate) fn attach_followup_attachments_locked(
    conn: &Connection,
    turns: &mut [Turn],
) -> Result<()> {
    let mut locations = std::collections::HashMap::new();
    for (turn_index, turn) in turns.iter().enumerate() {
        for (followup_index, followup) in turn.followups.iter().enumerate() {
            locations.insert(followup.prompt_id.clone(), (turn_index, followup_index));
        }
    }
    if locations.is_empty() {
        return Ok(());
    }
    let prompt_ids = locations.keys().collect::<Vec<_>>();
    for chunk in prompt_ids.chunks(900) {
        let placeholders = std::iter::repeat_n("?", chunk.len())
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT prompt_id, attachment_id, file_name, mime, kind, size_bytes,
                    width, height, created_at FROM user_attachments
             WHERE prompt_id IN ({placeholders}) ORDER BY created_at, attachment_id"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |row| {
            Ok((
                row.get::<_, String>(0)?,
                UserAttachment {
                    attachment_id: row.get(1)?,
                    file_name: row.get(2)?,
                    mime: row.get(3)?,
                    kind: row.get(4)?,
                    size_bytes: row.get::<_, i64>(5)?.max(0) as u64,
                    width: row.get::<_, i64>(6)?.max(0) as u32,
                    height: row.get::<_, i64>(7)?.max(0) as u32,
                    created_at: row.get(8)?,
                },
            ))
        })?;
        for row in rows {
            let (prompt_id, attachment) = row?;
            if let Some((turn_index, followup_index)) = locations.get(&prompt_id).copied() {
                turns[turn_index].followups[followup_index]
                    .uploaded_attachments
                    .push(attachment);
            }
        }
    }
    Ok(())
}

/// 把 `turn_tool_reports` 里的报告接到各回合尾部（v25）。
///
/// **接在尾部，不是覆盖**：老回合的报告还在 `turns.tool_reports` 那个 JSON 列
/// 里（v25 不回填、不删列），`map_turn_row` 已经把它读进来了。跨升级的那种
/// 回合两边都有内容，先列后子表正是它们真实发生的顺序。
///
/// 排序用 `report_id`——自增主键，插入顺序即报告顺序，所以写入端连
/// `MAX(seq)` 都不用查。
///
/// 900 一批是跟着同文件其它 `attach_*` 走的：SQLite 默认变量上限 999。
pub(crate) fn attach_tool_reports_locked(conn: &Connection, turns: &mut [Turn]) -> Result<()> {
    if turns.is_empty() {
        return Ok(());
    }
    let indexes = turns
        .iter()
        .enumerate()
        .map(|(index, turn)| (turn.turn_id.clone(), index))
        .collect::<std::collections::HashMap<_, _>>();
    let turn_ids = indexes.keys().collect::<Vec<_>>();
    for chunk in turn_ids.chunks(900) {
        let placeholders = std::iter::repeat_n("?", chunk.len())
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT turn_id, report FROM turn_tool_reports
             WHERE turn_id IN ({placeholders})
             ORDER BY report_id ASC"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(chunk.iter()), |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for (turn_id, report) in rows {
            if let Some(&index) = indexes.get(&turn_id) {
                turns[index].tool_reports.push(report);
            }
        }
    }
    Ok(())
}
