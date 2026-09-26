//! 版本号迁移的测试(09-24 从 `mod.rs` 搬来)。

use super::*;

fn open_migrated() -> Connection {
    let mut conn = Connection::open_in_memory().unwrap();
    run_migrations(&mut conn).unwrap();
    conn
}

#[test]
fn fresh_database_migrates_to_latest_version() {
    let conn = open_migrated();
    let version = user_version(&conn).unwrap();
    assert_eq!(version, MIGRATIONS.last().unwrap().version);
}

#[test]
fn migrations_are_idempotent_on_reopen() {
    let mut conn = open_migrated();
    // A second run must be a no-op.
    run_migrations(&mut conn).unwrap();
    assert_eq!(
        user_version(&conn).unwrap(),
        MIGRATIONS.last().unwrap().version
    );
}

/// 库里本来就有的外键孤儿，不该把以后所有升级都堵死。
///
/// 这条是真事：一次数据库损坏恢复在 `turn_journal_segments` 里留下 3 条
/// 指向已不存在回合的孤儿行。半个月后加了一个和它们毫无关系的迁移，
/// daemon 就再也起不来了，报错还指着那个无辜的迁移——它只是碰巧排在
/// 待跑队列的最前面。
#[test]
fn pre_existing_violations_do_not_block_unrelated_migrations() {
    let mut conn = open_migrated();
    // 造一条孤儿：外键此刻不生效（迁移期间本来就关着），直接插得进去。
    conn.execute_batch(
        "PRAGMA foreign_keys = OFF;
         INSERT INTO turn_journal_segments
             (turn_id, revision, segment_index, status, started_at)
         VALUES ('turn_does_not_exist', 0, 0, 'interrupted', '2026-08-18T00:00:00Z');
         PRAGMA user_version = 25;",
    )
    .unwrap();
    assert!(foreign_key_violations(&conn).unwrap() > 0, "前置条件不成立");

    run_migrations(&mut conn).expect("历史脏数据把升级路径堵死了");
    assert_eq!(
        user_version(&conn).unwrap(),
        MIGRATIONS.last().unwrap().version
    );
    // 脏数据仍在——迁移器不该顺手替别人打扫，只是不再当门卫。
    assert!(foreign_key_violations(&conn).unwrap() > 0);
}

/// 但迁移**自己**造出来的违规仍然必须回滚——放宽的是「不追究前人」，
/// 不是「不管了」。
#[test]
fn a_migration_that_breaks_referential_integrity_still_rolls_back() {
    let mut conn = open_migrated();
    // 真实迁移期间外键是关的（见 `run_migrations` 的注释：表重建会让
    // DROP TABLE 的隐式删除级联到子表）。这里照做，否则坏迁移在 INSERT
    // 那一步就被拦下，走不到我们要验的那道检查。
    conn.pragma_update(None, "foreign_keys", false).unwrap();
    let latest = MIGRATIONS.last().unwrap().version;
    let bad = [Migration {
        version: latest + 1,
        name: "deliberately_broken",
        apply: |conn| {
            conn.execute_batch(
                "INSERT INTO turn_journal_segments
                     (turn_id, revision, segment_index, status, started_at)
                 VALUES ('turn_missing', 0, 0, 'running', '2026-08-18T00:00:00Z');",
            )?;
            Ok(())
        },
    }];

    let error =
        apply_migrations(&mut conn, latest, &bad).expect_err("迁移把引用完整性搞坏了却提交了");
    assert!(
        format!("{error}").contains("introduced"),
        "报错要说清是这个迁移新增的：{error}"
    );
    // 事务回滚了：坏行没留下，版本号也没往前走。
    assert_eq!(foreign_key_violations(&conn).unwrap(), 0);
    assert_eq!(user_version(&conn).unwrap(), latest);
}

/// v24 的老库升到 v25：新表建出来，列里的老报告一个字节不动。
///
/// v25 是**纯增量**的——不回填、不删列。所以这条要证的是「什么都没被搬」，
/// 而不是「搬对了」。
#[test]
fn v25_adds_the_child_table_without_touching_existing_reports() {
    let mut conn = open_migrated();
    conn.execute_batch(
        "DROP TABLE turn_tool_reports;
         PRAGMA user_version = 24;",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO turns (turn_id, session_id, seq, user_content, user_timestamp,
                            assistant_content, assistant_timestamp, status, tool_reports)
         VALUES ('t1', ?1, 1, 'hi', 'now', 'yo', 'now', 'completed', ?2)",
        rusqlite::params![DEFAULT_SESSION_ID, r#"["老报告"]"#],
    )
    .unwrap();

    run_migrations(&mut conn).unwrap();

    assert_eq!(user_version(&conn).unwrap(), LATEST_VERSION);
    let reports: String = conn
        .query_row(
            "SELECT tool_reports FROM turns WHERE turn_id = 't1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(reports, r#"["老报告"]"#, "老报告被动过了");
    let child_rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM turn_tool_reports", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(child_rows, 0, "v25 不该回填");
    let violations: i64 = conn
        .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(violations, 0);
}

#[test]
fn v7_repairs_v6_database_missing_redo_backup_tables() {
    let mut conn = open_migrated();
    conn.execute_batch(
        "DROP TABLE turn_redo_image_backups;
         DROP TABLE turn_redo_question_backups;
         DROP TABLE turn_redo_backups;
         PRAGMA user_version = 6;",
    )
    .unwrap();

    run_migrations(&mut conn).unwrap();

    assert_eq!(user_version(&conn).unwrap(), LATEST_VERSION);
    for table in [
        "turn_redo_checkpoints",
        "turn_redo_backups",
        "turn_redo_question_backups",
        "turn_redo_image_backups",
    ] {
        let exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
                [table],
                |row| row.get(0),
            )
            .unwrap();
        assert!(exists, "missing repaired table: {table}");
    }
    let violations: i64 = conn
        .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(violations, 0);
}

#[test]
fn baseline_converges_legacy_database() {
    // Simulate a legacy pre-versioning database: base turns table without
    // the later ALTER-added columns and user_version 0.
    let mut conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE turns (
            turn_id          TEXT PRIMARY KEY,
            seq              INTEGER NOT NULL UNIQUE,
            user_content     TEXT NOT NULL,
            user_timestamp   TEXT NOT NULL,
            assistant_content TEXT NOT NULL,
            assistant_reasoning TEXT,
            assistant_timestamp TEXT,
            status           TEXT NOT NULL DEFAULT 'running',
            tool_reports     TEXT NOT NULL DEFAULT '[]'
        );",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO turns (turn_id, seq, user_content, user_timestamp, assistant_content)
         VALUES ('t1', 1, 'hi', 'now', 'hello')",
        [],
    )
    .unwrap();
    run_migrations(&mut conn).unwrap();
    // Legacy row survives and the ALTER-added columns exist with defaults.
    let (hidden, model): (i64, Option<String>) = conn
        .query_row(
            "SELECT hidden, assistant_model FROM turns WHERE turn_id = 't1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(hidden, 0);
    assert_eq!(model, None);
}

#[test]
fn newer_database_version_is_refused() {
    let mut conn = Connection::open_in_memory().unwrap();
    conn.pragma_update(None, "user_version", 9999).unwrap();
    let err = run_migrations(&mut conn).unwrap_err();
    assert!(err.to_string().contains("newer"));
}

#[test]
fn v2_moves_existing_history_into_the_default_session() {
    let mut conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
    // Build a v1 database with a turn and a dependent child row.
    conn.pragma_update(None, "user_version", 0).unwrap();
    apply_v1_baseline(&conn).unwrap();
    conn.pragma_update(None, "user_version", 1).unwrap();
    conn.execute(
        "INSERT INTO turns (turn_id, seq, user_content, user_timestamp, assistant_content)
         VALUES ('t1', 7, 'hi', 'now', 'hello')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO question_exchanges (turn_id, exchange_index, payload)
         VALUES ('t1', 0, '{}')",
        [],
    )
    .unwrap();
    run_migrations(&mut conn).unwrap();

    let (session_id, seq): (String, i64) = conn
        .query_row(
            "SELECT session_id, seq FROM turns WHERE turn_id = 't1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(session_id, DEFAULT_SESSION_ID);
    assert_eq!(seq, 7);
    // The FK-off rebuild must not cascade-delete child rows.
    let exchanges: i64 = conn
        .query_row("SELECT count(*) FROM question_exchanges", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(exchanges, 1);
    let current: String = conn
        .query_row(
            "SELECT value FROM app_state WHERE key = 'current_session'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(current, DEFAULT_SESSION_ID);
    // Per-session seq uniqueness: same seq in another session is fine.
    conn.execute(
        "INSERT INTO sessions (session_id, persona, name, created_at, updated_at)
         VALUES ('other', '', 'x', 'now', 'now')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO turns (turn_id, session_id, seq, user_content, user_timestamp, assistant_content)
         VALUES ('t2', 'other', 7, 'hi', 'now', '')",
        [],
    )
    .unwrap();
    // …but duplicated seq within one session is rejected.
    assert!(conn
        .execute(
            "INSERT INTO turns (turn_id, session_id, seq, user_content, user_timestamp, assistant_content)
             VALUES ('t3', 'other', 7, 'hi', 'now', '')",
            [],
        )
        .is_err());
}

#[test]
fn v3_platform_tables_enforce_uniqueness_and_session_cascade() {
    let conn = open_migrated();
    assert_eq!(user_version(&conn).unwrap(), LATEST_VERSION);
    conn.execute(
        "INSERT INTO sessions (session_id, persona, name, created_at, updated_at)
         VALUES ('platform-session', 'miyu', 'platform', 'now', 'now')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO platform_session_bindings (
            platform, account_id, conversation_kind, conversation_id,
            persona, session_id, created_at, updated_at
         ) VALUES ('onebot', '10000', 'private', '20000', 'miyu',
                   'platform-session', 'now', 'now')",
        [],
    )
    .unwrap();

    // A session cannot be attached to a second external identity.
    assert!(conn
        .execute(
            "INSERT INTO platform_session_bindings (
                platform, account_id, conversation_kind, conversation_id,
                persona, session_id, created_at, updated_at
             ) VALUES ('onebot', '10000', 'private', 'other', 'miyu',
                       'platform-session', 'now', 'now')",
            [],
        )
        .is_err());
    conn.execute(
        "INSERT INTO platform_plugin_kv (
            plugin_id, platform, account_id, conversation_kind,
            conversation_id, key, value_json, updated_at
         ) VALUES ('reply_processor', 'onebot', '10000', 'private',
                   '20000', 'recent_images', '[]', 'now')",
        [],
    )
    .unwrap();

    conn.execute(
        "DELETE FROM sessions WHERE session_id = 'platform-session'",
        [],
    )
    .unwrap();
    let binding_count: i64 = conn
        .query_row(
            "SELECT count(*) FROM platform_session_bindings",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let plugin_count: i64 = conn
        .query_row("SELECT count(*) FROM platform_plugin_kv", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(binding_count, 0);
    // Plugin state is scoped to the external conversation, not a session.
    assert_eq!(plugin_count, 1);
}

#[test]
fn v4_platform_meme_refs_enforce_identity_and_direction() {
    let conn = open_migrated();
    assert_eq!(user_version(&conn).unwrap(), LATEST_VERSION);
    conn.execute(
        "INSERT INTO platform_meme_refs (
            platform, account_id, conversation_kind, conversation_id,
            message_id, library, meme_id, direction, created_at
         ) VALUES ('onebot', '10000', 'group', '20000', 'message-1',
                   'default', 'meme-1', 'inbound', 'now')",
        [],
    )
    .unwrap();

    assert!(conn
        .execute(
            "INSERT INTO platform_meme_refs (
                platform, account_id, conversation_kind, conversation_id,
                message_id, library, meme_id, direction, created_at
             ) VALUES ('onebot', '10000', 'group', '20000', 'message-1',
                       'default', 'meme-1', 'inbound', 'later')",
            [],
        )
        .is_err());
    assert!(conn
        .execute(
            "INSERT INTO platform_meme_refs (
                platform, account_id, conversation_kind, conversation_id,
                message_id, library, meme_id, direction, created_at
             ) VALUES ('onebot', '10000', 'group', '20000', 'message-2',
                       'default', 'meme-1', 'sideways', 'now')",
            [],
        )
        .is_err());
}

#[test]
fn v4_migrates_an_existing_v3_database_without_losing_platform_state() {
    let mut conn = Connection::open_in_memory().unwrap();
    apply_v1_baseline(&conn).unwrap();
    apply_v2_sessions(&conn).unwrap();
    apply_v3_platform_sessions_and_plugin_state(&conn).unwrap();
    conn.pragma_update(None, "user_version", 3).unwrap();
    conn.execute(
        "INSERT INTO platform_plugin_kv (
            plugin_id, platform, account_id, conversation_kind,
            conversation_id, key, value_json, updated_at
         ) VALUES ('reply_processor', 'onebot', '10000', 'group',
                   '20000', 'recent_images', '[]', 'now')",
        [],
    )
    .unwrap();

    run_migrations(&mut conn).unwrap();

    assert_eq!(user_version(&conn).unwrap(), LATEST_VERSION);
    let plugin_rows: i64 = conn
        .query_row("SELECT count(*) FROM platform_plugin_kv", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(plugin_rows, 1);
    let meme_table_exists: bool = conn
        .query_row(
            "SELECT EXISTS(
                SELECT 1 FROM sqlite_master
                WHERE type = 'table' AND name = 'platform_meme_refs'
            )",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(meme_table_exists);
}

/// v36 把老 `/workspace` 的 cwd 绑定清掉:升级后没有会话会被暗中锁进旧工作区。
#[test]
fn v36_clears_legacy_workspace_bindings() {
    let mut conn = Connection::open_in_memory().unwrap();
    apply_migrations(&mut conn, 0, &MIGRATIONS[..35]).unwrap();
    assert_eq!(user_version(&conn).unwrap(), 35);
    conn.execute(
        "INSERT INTO sessions (session_id, persona, name, kind, workspace, created_at, updated_at) \
         VALUES ('s1', 'miyu', 'old', 'user', '/tmp/old-workspace', '2026-01-01', '2026-01-01')",
        [],
    )
    .unwrap();
    run_migrations(&mut conn).unwrap();
    assert_eq!(user_version(&conn).unwrap(), LATEST_VERSION);
    let bound: Option<String> = conn
        .query_row(
            "SELECT workspace FROM sessions WHERE session_id = 's1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(bound, None);
}

/// v37 给老会话补上读放开开关,默认关:升级不会把已绑沙盒的会话读权限放开。
#[test]
fn v37_defaults_sandbox_read_all_to_locked() {
    let mut conn = Connection::open_in_memory().unwrap();
    apply_migrations(&mut conn, 0, &MIGRATIONS[..36]).unwrap();
    assert_eq!(user_version(&conn).unwrap(), 36);
    conn.execute(
        "INSERT INTO sessions (session_id, persona, name, kind, workspace, created_at, updated_at) \
         VALUES ('s1', 'miyu', 'bound', 'user', '/tmp/root', '2026-01-01', '2026-01-01')",
        [],
    )
    .unwrap();
    run_migrations(&mut conn).unwrap();
    assert_eq!(user_version(&conn).unwrap(), LATEST_VERSION);
    let read_all: i64 = conn
        .query_row(
            "SELECT sandbox_read_all FROM sessions WHERE session_id = 's1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(read_all, 0);
}

/// v40 给老会话补上「明确不要沙盒」与只读两列,默认都关:升级后行为不变。
#[test]
fn v40_defaults_sandbox_toggles_to_off() {
    let mut conn = Connection::open_in_memory().unwrap();
    apply_migrations(&mut conn, 0, &MIGRATIONS[..39]).unwrap();
    assert_eq!(user_version(&conn).unwrap(), 39);
    conn.execute(
        "INSERT INTO sessions (session_id, persona, name, kind, created_at, updated_at) \
         VALUES ('s1', 'miyu', 'old', 'user', '2026-01-01', '2026-01-01')",
        [],
    )
    .unwrap();
    run_migrations(&mut conn).unwrap();
    assert_eq!(user_version(&conn).unwrap(), LATEST_VERSION);
    let (opt_out, readonly): (i64, i64) = conn
        .query_row(
            "SELECT sandbox_opt_out, sandbox_readonly FROM sessions WHERE session_id = 's1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((opt_out, readonly), (0, 0));
}

#[test]
fn v9_creates_platform_access_and_audit_tables() {
    let conn = open_migrated();
    assert_eq!(user_version(&conn).unwrap(), LATEST_VERSION);
    for table in ["platform_access_grants", "platform_access_audit"] {
        let exists: bool = conn
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM sqlite_master
                    WHERE type = 'table' AND name = ?1
                )",
                [table],
                |row| row.get(0),
            )
            .unwrap();
        assert!(exists, "missing access-control table: {table}");
    }
}
