//! 纯增量的命名迁移（09-24 起）。
//!
//! 版本号迁移（`user_version`）有两个老毛病：
//! - 库比程序新就拒开：分支构建把库迁上去，main 构建的 daemon 就起不来（09-09）；
//! - 两条分支各加一个「v41」，会静默撞号。
//!
//! 只加表、加索引的改动走这里，按 id 记在 `schema_migrations` 里：
//! - 缺哪条补哪条，库里多出来的陌生 id 不管；
//! - **不动 `user_version`**，老程序照样能开这个库，多出来的表它看不见，也用不着。
//!
//! 改老列、重建表这类老程序读不懂的改动，仍然走版本号迁移，好让老程序拒开。

use anyhow::Result;
use rusqlite::{params, Connection, TransactionBehavior};

struct NamedMigration {
    id: &'static str,
    apply: fn(&Connection) -> Result<()>,
}

const NAMED_MIGRATIONS: &[NamedMigration] = &[
    NamedMigration {
        id: "2026-09-24-session-scoped-state",
        apply: apply_session_scoped_state,
    },
    NamedMigration {
        id: "2026-09-25-cache-breaks",
        apply: apply_cache_breaks,
    },
    NamedMigration {
        id: "2026-09-26-held-job-reports",
        apply: apply_held_job_reports,
    },
];

/// 平台会话里还没交出去的后台任务汇报（09-26，见 `conversation_db/held_reports.rs`）：一份一行，
/// 按批（派它们的那一轮）取，挂会话级联删除。
fn apply_held_job_reports(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS held_job_reports (
             id         INTEGER PRIMARY KEY AUTOINCREMENT,
             session_id TEXT NOT NULL REFERENCES sessions(session_id) ON DELETE CASCADE,
             batch      TEXT NOT NULL,
             job_id     TEXT NOT NULL,
             initiator  TEXT,
             content    TEXT NOT NULL,
             created_at TEXT NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_held_job_reports_session ON held_job_reports(session_id, id);",
    )?;
    Ok(())
}

/// 断缓存记录（09-25，见 `llm::cache_break`）：一次一行，挂会话级联删除。
fn apply_cache_breaks(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS cache_breaks (
             id          INTEGER PRIMARY KEY AUTOINCREMENT,
             session_id  TEXT NOT NULL REFERENCES sessions(session_id) ON DELETE CASCADE,
             turn_id     TEXT,
             at          TEXT NOT NULL,
             lost_tokens INTEGER NOT NULL,
             cause       TEXT NOT NULL,
             idle_secs   INTEGER NOT NULL DEFAULT 0
         );
         CREATE INDEX IF NOT EXISTS idx_cache_breaks_session ON cache_breaks(session_id, id);",
    )?;
    Ok(())
}

/// 会话级零碎状态入库（会话项目第 1 段）。
///
/// 这些状态以前散在 `state/` 下按会话 id 命名的文件里，删会话时没人清（实测
/// repl-history 127/129、todos 11/11 是孤儿）。现在挂在 sessions 上级联删除，
/// 删会话自动带走：
/// - `session_values`：每个会话每类一份文本（待办、提示词指纹、思考档位钉），
///   格式归各自的主人；
/// - `repl_history`：终端的上键历史，一条一行；
/// - `legacy_file_imports`：哪个会话的哪类老文件已经导进来了，保证只导一次、
///   老内容排在新写入前面。
fn apply_session_scoped_state(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS session_values (
             session_id TEXT NOT NULL REFERENCES sessions(session_id) ON DELETE CASCADE,
             kind       TEXT NOT NULL,
             value      TEXT NOT NULL,
             updated_at TEXT NOT NULL,
             PRIMARY KEY (session_id, kind)
         );
         CREATE TABLE IF NOT EXISTS repl_history (
             id         INTEGER PRIMARY KEY AUTOINCREMENT,
             session_id TEXT NOT NULL REFERENCES sessions(session_id) ON DELETE CASCADE,
             entry      TEXT NOT NULL,
             created_at TEXT NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_repl_history_session ON repl_history(session_id, id);
         CREATE TABLE IF NOT EXISTS legacy_file_imports (
             session_id  TEXT NOT NULL REFERENCES sessions(session_id) ON DELETE CASCADE,
             kind        TEXT NOT NULL,
             imported_at TEXT NOT NULL,
             PRIMARY KEY (session_id, kind)
         );",
    )?;
    Ok(())
}

/// 补上还没做过的命名迁移。每个进程每次开库都跑：库已经齐了就只是一次查询。
pub fn run_named_migrations(conn: &mut Connection) -> Result<()> {
    let has_ledger: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'schema_migrations')",
        [],
        |row| row.get(0),
    )?;
    let applied = if has_ledger {
        let mut stmt = conn.prepare("SELECT id FROM schema_migrations")?;
        let ids = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<std::collections::HashSet<_>>>()?;
        ids
    } else {
        Default::default()
    };
    for migration in NAMED_MIGRATIONS
        .iter()
        .filter(|migration| !applied.contains(migration.id))
    {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_migrations (
                 id         TEXT PRIMARY KEY,
                 applied_at TEXT NOT NULL
             );",
        )?;
        // 别的进程可能刚刚抢先做完了：拿到写锁以后再看一眼。
        let done: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE id = ?1)",
            params![migration.id],
            |row| row.get(0),
        )?;
        if !done {
            (migration.apply)(&tx)?;
            tx.execute(
                "INSERT INTO schema_migrations (id, applied_at) VALUES (?1, ?2)",
                params![migration.id, chrono::Utc::now().to_rfc3339()],
            )?;
        }
        tx.commit()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn migrated() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::state::migrations::run_migrations(&mut conn).unwrap();
        run_named_migrations(&mut conn).unwrap();
        conn
    }

    /// 命名迁移不动版本号：老程序只认 `user_version`，照样能开这个库。
    #[test]
    fn named_migrations_leave_the_schema_version_alone() {
        let conn = migrated();
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, crate::state::migrations::LATEST_VERSION);
        for table in ["session_values", "repl_history", "legacy_file_imports"] {
            let exists: bool = conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
                    params![table],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(exists, "{table} 没建出来");
        }
    }

    /// 再跑一遍什么也不做；库里有本程序不认识的 id（更新的分支加的）也不拦。
    #[test]
    fn rerunning_is_a_no_op_and_unknown_ids_are_left_alone() {
        let mut conn = migrated();
        conn.execute(
            "INSERT INTO schema_migrations (id, applied_at) VALUES ('2099-01-01-from-a-newer-build', 'x')",
            [],
        )
        .unwrap();
        run_named_migrations(&mut conn).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, NAMED_MIGRATIONS.len() as i64 + 1);
    }
}
