//! Versioned schema migrations for conversation.db.
//!
//! Uses `PRAGMA user_version` to track the applied schema version. Each
//! migration runs inside an immediate transaction; the version is bumped in
//! the same transaction so a crash mid-migration leaves the database at the
//! previous version and the migration re-runs on next open.
//!
//! Version 1 is the idempotent baseline: it absorbs the historical
//! `CREATE TABLE IF NOT EXISTS` + `add_column_if_missing` logic so that any
//! legacy database (at any historical column state) converges to the same
//! schema. Later migrations may assume the baseline and use destructive
//! operations such as table rebuilds.

mod baseline;
mod columns;
mod named;
pub use baseline::DEFAULT_SESSION_ID;
use baseline::*;
use columns::*;
pub use named::run_named_migrations;

use anyhow::{bail, Context, Result};
use rusqlite::{Connection, TransactionBehavior};

struct Migration {
    version: i64,
    name: &'static str,
    apply: fn(&Connection) -> Result<()>,
}

const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "baseline",
        apply: apply_v1_baseline,
    },
    Migration {
        version: 2,
        name: "sessions",
        apply: apply_v2_sessions,
    },
    Migration {
        version: 3,
        name: "platform_sessions_and_plugin_state",
        apply: apply_v3_platform_sessions_and_plugin_state,
    },
    Migration {
        version: 4,
        name: "platform_meme_refs",
        apply: apply_v4_platform_meme_refs,
    },
    Migration {
        version: 5,
        name: "user_attachments",
        apply: apply_v5_user_attachments,
    },
    Migration {
        version: 6,
        name: "turn_redo_checkpoints",
        apply: apply_v6_turn_redo_checkpoints,
    },
    Migration {
        version: 7,
        name: "turn_redo_backups",
        apply: apply_v7_turn_redo_backups,
    },
    Migration {
        version: 8,
        name: "artifact_assets",
        apply: apply_v8_artifact_assets,
    },
    Migration {
        version: 9,
        name: "platform_access_control",
        apply: apply_v9_platform_access_control,
    },
    Migration {
        version: 10,
        name: "turn_generation_journal",
        apply: apply_v10_turn_generation_journal,
    },
    Migration {
        version: 11,
        name: "session_model_override",
        apply: apply_v11_session_model_override,
    },
    Migration {
        version: 12,
        name: "turn_context_messages",
        apply: apply_v12_turn_context_messages,
    },
    Migration {
        version: 13,
        name: "compact_hidden_turns",
        apply: apply_v13_compact_hidden_turns,
    },
    Migration {
        version: 14,
        name: "tool_reports_archive",
        apply: apply_v14_tool_reports_archive,
    },
    Migration {
        version: 15,
        name: "session_last_request_at",
        apply: apply_v15_session_last_request_at,
    },
    Migration {
        version: 16,
        name: "turn_tool_footprint",
        apply: apply_v16_turn_tool_footprint,
    },
    Migration {
        version: 17,
        name: "turn_replay_journal",
        apply: apply_v17_turn_replay_journal,
    },
    Migration {
        version: 18,
        name: "turn_cache_tokens",
        apply: apply_v18_turn_cache_tokens,
    },
    Migration {
        version: 19,
        name: "session_cache_tokens",
        apply: apply_v19_session_cache_tokens,
    },
    Migration {
        version: 20,
        name: "turn_tool_flow",
        apply: apply_v20_turn_tool_flow,
    },
    Migration {
        version: 21,
        name: "rename_default_session",
        apply: apply_v21_rename_default_session,
    },
    Migration {
        version: 22,
        name: "session_goals",
        apply: apply_v22_session_goals,
    },
    Migration {
        version: 23,
        name: "retire_session_archiving",
        apply: apply_v23_retire_session_archiving,
    },
    Migration {
        version: 24,
        name: "retire_session_goals",
        apply: apply_v24_retire_session_goals,
    },
    Migration {
        version: 25,
        name: "tool_reports_child_table",
        apply: apply_v25_tool_reports_child_table,
    },
    Migration {
        version: 26,
        name: "session_goals",
        apply: apply_v26_session_goals,
    },
    Migration {
        version: 27,
        name: "shared_files",
        apply: apply_v27_shared_files,
    },
    Migration {
        version: 28,
        name: "session_sort_key",
        apply: apply_v28_session_sort_key,
    },
    Migration {
        version: 29,
        name: "attachment_files",
        apply: apply_v29_attachment_files,
    },
    Migration {
        version: 30,
        name: "turn_inline_media",
        apply: apply_v30_turn_inline_media,
    },
    Migration {
        version: 31,
        name: "queued_prompt_context_messages",
        apply: apply_v31_queued_prompt_context_messages,
    },
    Migration {
        version: 32,
        name: "sponsor_records",
        apply: apply_v32_sponsor_records,
    },
    Migration {
        version: 33,
        name: "compact_v3",
        apply: apply_v33_compact_v3,
    },
    Migration {
        version: 34,
        name: "accounts",
        apply: apply_v34_accounts,
    },
    Migration {
        version: 35,
        name: "generation_speed",
        apply: apply_v35_generation_speed,
    },
    Migration {
        version: 36,
        name: "sandbox_root",
        apply: apply_v36_sandbox_root,
    },
    Migration {
        version: 37,
        name: "sandbox_read_all",
        apply: apply_v37_sandbox_read_all,
    },
    Migration {
        version: 38,
        name: "session_context_tokens",
        apply: apply_v38_session_context_tokens,
    },
    Migration {
        version: 39,
        name: "subagent_sessions",
        apply: apply_v39_subagent_sessions,
    },
    Migration {
        version: 40,
        name: "sandbox_toggle",
        apply: apply_v40_sandbox_toggle,
    },
];

/// Latest schema version this build produces.
pub const LATEST_VERSION: i64 = 40;

/// Returns the schema version currently recorded in the database.
pub fn current_version(conn: &Connection) -> Result<i64> {
    user_version(conn)
}

/// Runs all pending migrations. Called from `ConversationDb::open` while the
/// connection is still exclusively owned by the caller.
///
/// Foreign-key enforcement is disabled for the duration: table rebuilds drop
/// and recreate parent tables, and with enforcement on the implicit
/// `DELETE FROM` of `DROP TABLE` would cascade into child tables. Integrity is
/// re-checked with `foreign_key_check` inside each migration's transaction.
pub fn run_migrations(conn: &mut Connection) -> Result<()> {
    let current = user_version(conn)?;
    let latest = MIGRATIONS.last().map(|m| m.version).unwrap_or(0);
    if current > latest {
        bail!(
            "conversation.db schema version {current} is newer than this build supports ({latest}); refusing to open"
        );
    }
    if current == latest {
        return Ok(());
    }
    conn.pragma_update(None, "foreign_keys", false)?;
    let result = apply_pending(conn, current);
    let restore = conn.pragma_update(None, "foreign_keys", true);
    result?;
    restore?;
    Ok(())
}

fn apply_pending(conn: &mut Connection, current: i64) -> Result<()> {
    apply_migrations(conn, current, MIGRATIONS)
}

/// 迁移列表作为参数传进来，测试才能塞一个故意写坏的迁移进去验回滚。
fn apply_migrations(conn: &mut Connection, current: i64, migrations: &[Migration]) -> Result<()> {
    for migration in migrations.iter().filter(|m| m.version > current) {
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .with_context(|| format!("failed to begin migration '{}'", migration.name))?;
        // 迁移**动手之前**的违规数。库里可能本来就有孤儿行（崩溃、外部工具、
        // 一次 `.recover` 之后都可能留下），那不是这个迁移的责任。
        let before = foreign_key_violations(&tx)?;
        (migration.apply)(&tx)
            .with_context(|| format!("schema migration '{}' failed", migration.name))?;
        let after = foreign_key_violations(&tx)?;
        // 只看**新增**的。以前是「跑完还有违规就回滚」，于是库里但凡有一条
        // 历史脏数据，以后所有升级都会失败、daemon 直接起不来，而且报错指向
        // 一个完全无辜的迁移——它只是碰巧排在最前面。
        if after > before {
            bail!(
                "schema migration '{}' introduced {} foreign-key violations \
                 (before {before}, after {after}); rolling back",
                migration.name,
                after - before
            );
        }
        if before > 0 {
            // 放行但要留痕：脏数据仍然存在，只是不该由升级路径来当门卫。
            tracing::warn!(
                migration = migration.name,
                violations = before,
                "database has pre-existing foreign-key violations; migration applied anyway"
            );
        }
        tx.pragma_update(None, "user_version", migration.version)?;
        tx.commit()
            .with_context(|| format!("failed to commit migration '{}'", migration.name))?;
    }
    Ok(())
}

fn foreign_key_violations(conn: &Connection) -> Result<i64> {
    Ok(
        conn.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })?,
    )
}

fn user_version(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row("PRAGMA user_version", [], |row| row.get(0))?)
}

fn add_column_if_missing(
    conn: &Connection,
    table: &str,
    column: &str,
    definition: &str,
) -> Result<()> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
    for row in rows {
        if row? == column {
            return Ok(());
        }
    }
    conn.execute(
        &format!("ALTER TABLE {table} ADD COLUMN {column} {definition}"),
        [],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests;
