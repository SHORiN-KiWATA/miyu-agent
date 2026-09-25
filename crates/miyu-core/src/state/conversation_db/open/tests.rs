//! 开库路径的测试:回收空闲页、损坏库的报错与备份(09-24 从 `mod.rs` 搬来)。

use super::*;

#[cfg(test)]
mod reclaim_tests_support {
    use super::*;

    pub(super) fn page_stats(path: &Path) -> (i64, i64, i64) {
        let conn = Connection::open(path).unwrap();
        let get = |pragma: &str| -> i64 {
            conn.query_row(&format!("PRAGMA {pragma}"), [], |row| row.get(0))
                .unwrap()
        };
        (get("auto_vacuum"), get("page_count"), get("freelist_count"))
    }
}

#[cfg(test)]
mod reclaim_tests {
    use super::reclaim_tests_support::*;
    use super::*;

    /// 造一个「老库」：`auto_vacuum = 0`（SQLite 的默认），塞一堆数据再删掉，
    /// 留下一屁股空闲页。这正是本机那个 76 MB / 90% 空闲的库的来历。
    fn write_legacy_database(path: &Path) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "PRAGMA auto_vacuum = NONE;
             PRAGMA journal_mode = DELETE;
             CREATE TABLE junk(id INTEGER PRIMARY KEY, body TEXT);",
        )
        .unwrap();
        let mut insert = conn.prepare("INSERT INTO junk(body) VALUES(?1)").unwrap();
        let filler = "x".repeat(2048);
        for _ in 0..2_000 {
            insert.execute(params![filler]).unwrap();
        }
        drop(insert);
        conn.execute_batch("DELETE FROM junk;").unwrap();
    }

    /// 老库打开时要被转换并回收：文件真的变小，`auto_vacuum` 从 0 变 2。
    #[test]
    fn opening_a_legacy_database_reclaims_free_pages() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("conversation.db");
        write_legacy_database(&path);

        let (mode, pages, free) = page_stats(&path);
        assert_eq!(mode, 0, "构造出来的应该是老库");
        assert!(free * 2 > pages, "构造出来的库应该大半是空闲页");
        let before = std::fs::metadata(&path).unwrap().len();

        let db = ConversationDb::open(dir.path()).unwrap();
        drop(db);

        let (mode, pages, free) = page_stats(&path);
        let after = std::fs::metadata(&path).unwrap().len();
        assert_eq!(mode, 2, "应该已经转成 INCREMENTAL");
        assert!(after < before / 2, "文件没缩水：{before} → {after} 字节");
        assert!(
            free * 10 < pages,
            "回收后不该还剩大量空闲页：{free}/{pages}"
        );
    }

    /// 新库开箱就是 INCREMENTAL——`auto_vacuum` 只有在建表**之前**设才管用，
    /// 这条防的是有人把那句 PRAGMA 挪到 `run_migrations` 后面。
    #[test]
    fn a_fresh_database_is_created_with_incremental_vacuum() {
        let dir = tempfile::tempdir().unwrap();
        let db = ConversationDb::open(dir.path()).unwrap();
        drop(db);
        let (mode, _, _) = page_stats(&dir.path().join("conversation.db"));
        assert_eq!(mode, 2);
    }

    /// 转换只发生一次：第二次打开不该再跑完整 VACUUM。
    #[test]
    fn the_conversion_does_not_repeat() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("conversation.db");
        write_legacy_database(&path);
        drop(ConversationDb::open(dir.path()).unwrap());
        let (mode, _, _) = page_stats(&path);
        assert_eq!(mode, 2);
        drop(ConversationDb::open(dir.path()).unwrap());
        let (mode, _, _) = page_stats(&path);
        assert_eq!(mode, 2);
    }
}

#[cfg(test)]
mod reclaim_probe {
    use super::reclaim_tests_support::*;

    /// 量尺：把一份**真实**的 conversation.db 复制到临时目录，用真实的 `open`
    /// 路径跑一遍，看能还回去多少磁盘。
    ///
    /// ```
    /// cp ~/.miyu/state/conversation.db /tmp/probe/
    /// MIYU_RECLAIM_PROBE_DIR=/tmp/probe \
    ///   cargo test --lib reclaim_probe -- --ignored --nocapture
    /// ```
    ///
    /// 不读默认路径、只认显式给的目录：量尺不该顺手打开用户正在用的库。
    #[test]
    #[ignore]
    fn reclaim_on_a_real_database() {
        let Some(dir) = std::env::var_os("MIYU_RECLAIM_PROBE_DIR") else {
            println!("\n  跳过：没给 MIYU_RECLAIM_PROBE_DIR");
            return;
        };
        let dir = std::path::PathBuf::from(dir);
        let path = dir.join("conversation.db");
        let (mode, pages, free) = page_stats(&path);
        let before = std::fs::metadata(&path).unwrap().len();
        println!(
            "\n  改前  {:.1} MB  auto_vacuum={mode}  页 {pages}（空闲 {free}，{:.0}%）",
            before as f64 / 1048576.0,
            100.0 * free as f64 / pages as f64
        );

        let started = std::time::Instant::now();
        drop(super::ConversationDb::open(&dir).unwrap());
        let elapsed = started.elapsed();

        let (mode, pages, free) = page_stats(&path);
        let after = std::fs::metadata(&path).unwrap().len();
        println!(
            "  改后  {:.1} MB  auto_vacuum={mode}  页 {pages}（空闲 {free}）\n  \
             还回 {:.1} MB（{:.0}%），open 用时 {:.0} ms",
            after as f64 / 1048576.0,
            (before - after) as f64 / 1048576.0,
            100.0 * (before - after) as f64 / before as f64,
            elapsed.as_secs_f64() * 1000.0,
        );
    }
}

#[cfg(test)]
mod corruption_tests {
    use super::*;

    /// 造一个真正会让 SQLite 报损坏的库：先建一个合法库，再把中间的页面
    /// 用垃圾覆盖掉。文件头还在，所以它过得了"是不是 SQLite 文件"这关，
    /// 坏在页面结构上——正是用户 08-29 反馈的那种 `SQLITE_CORRUPT`。
    fn write_corrupt_database(path: &Path) {
        {
            let conn = Connection::open(path).unwrap();
            conn.execute_batch(
                "PRAGMA journal_mode = DELETE;
                 CREATE TABLE turns(turn_id TEXT PRIMARY KEY, body TEXT);",
            )
            .unwrap();
            let mut insert = conn.prepare("INSERT INTO turns VALUES(?1, ?2)").unwrap();
            let filler = "x".repeat(4096);
            for index in 0..200 {
                insert.execute(params![index.to_string(), filler]).unwrap();
            }
        }
        // 保留 100 字节文件头（否则 SQLite 只会说"这不是数据库"），砸掉
        // 第一页剩下的部分——sqlite_master 的 b-tree 就在那儿，读不出 schema
        // 才是 SQLITE_CORRUPT。砸文件尾部没用：第一页完好时 schema 照样读得
        // 出来，报的会是"no such column"之类的普通错误。
        let mut bytes = std::fs::read(path).unwrap();
        let page_size = 4096.min(bytes.len());
        for byte in &mut bytes[100..page_size] {
            *byte = 0x5a;
        }
        std::fs::write(path, bytes).unwrap();
    }

    /// 坏库的报错要说清楚是哪个文件、怎么办。原来只有 rusqlite 的裸串
    /// 「database disk image is malformed」,用户拿到手什么也做不了。
    #[test]
    fn a_corrupt_database_reports_the_file_and_what_to_do() {
        let dir = tempfile::tempdir().unwrap();
        write_corrupt_database(&dir.path().join("conversation.db"));

        let error = ConversationDb::open(dir.path()).unwrap_err();
        let rendered = format!("{error:#}");

        assert!(is_database_corrupt(&error), "没认出损坏: {rendered}");
        assert!(rendered.contains("会话数据库已损坏"), "{rendered}");
        assert!(rendered.contains("conversation.db"), "{rendered}");
        assert!(rendered.contains("conversation.db-wal"), "{rendered}");
    }

    /// 备份必须先导到暂存文件、成功了再顶上。原来是「先删旧备份、再
    /// VACUUM INTO、失败只 log」——导出一失败,用户最需要备份的那一刻,上一
    /// 份好备份刚被自己删掉。
    ///
    /// 这里用「把暂存路径先占成目录」来逼 VACUUM INTO 失败:坏库测不出这条
    /// (库一坏,`current_version` 先炸,根本走不到备份这段——第一版测试就是
    /// 这么假绿的)。
    #[test]
    fn a_failed_backup_never_destroys_the_existing_one() {
        let dir = tempfile::tempdir().unwrap();
        {
            let conn = Connection::open(dir.path().join("conversation.db")).unwrap();
            conn.execute_batch(
                "CREATE TABLE turns(turn_id TEXT PRIMARY KEY, body TEXT);
                 INSERT INTO turns VALUES('t1', 'body');",
            )
            .unwrap();
        }
        let backup = dir.path().join("conversation.db.bak");
        std::fs::write(&backup, b"previous good backup").unwrap();
        // 暂存路径被目录占住 → VACUUM INTO 必失败。
        std::fs::create_dir(dir.path().join("conversation.db.bak.new")).unwrap();

        let _ = ConversationDb::open(dir.path());

        assert_eq!(
            std::fs::read(&backup).unwrap(),
            b"previous good backup",
            "导出失败时旧备份被删了"
        );
    }
}

#[cfg(test)]
mod shared_connection_tests {
    use super::reclaim_tests_support::page_stats;
    use super::*;

    /// 在一个当前版本的库里造出一大片空闲页:建张临时表塞满再删光。
    fn current_database_with_free_pages(dir: &Path) -> i64 {
        drop(ConversationDb::open(dir).unwrap());
        let conn = Connection::open(dir.join("conversation.db")).unwrap();
        conn.execute_batch("CREATE TABLE junk(id INTEGER PRIMARY KEY, body TEXT);")
            .unwrap();
        let filler = "x".repeat(2048);
        for _ in 0..2_000 {
            conn.execute("INSERT INTO junk(body) VALUES(?1)", params![filler])
                .unwrap();
        }
        conn.execute_batch("DELETE FROM junk; PRAGMA wal_checkpoint(TRUNCATE);")
            .unwrap();
        drop(conn);
        let (_, _, free) = page_stats(&dir.join("conversation.db"));
        assert!(free >= 64, "测具没造出足够的空闲页:{free}");
        free
    }

    /// 客户端开一个当前版本的库不做维护:空闲页原样留着,等守护进程来回收。
    /// 守护进程开同一个库照旧回收(09-24 会话项目第 1 段)。
    #[test]
    fn a_client_open_leaves_maintenance_to_the_daemon() {
        let dir = tempfile::tempdir().unwrap();
        let free = current_database_with_free_pages(dir.path());

        let client = ConversationDb::shared(dir.path(), dir.path(), OpenRole::Client).unwrap();
        let (_, _, after_client) = page_stats(&dir.path().join("conversation.db"));
        assert_eq!(after_client, free, "客户端开库回收了空闲页");
        drop(client);

        let daemon = ConversationDb::shared(dir.path(), dir.path(), OpenRole::Maintain).unwrap();
        let (_, _, after_daemon) = page_stats(&dir.path().join("conversation.db"));
        assert!(
            after_daemon < free,
            "守护进程开库没回收:{free} → {after_daemon}"
        );
        drop(daemon);
    }

    /// 同一个进程里再开同一个库,拿到的是同一个连接。
    #[test]
    fn one_process_shares_one_connection_per_database_file() {
        let dir = tempfile::tempdir().unwrap();
        let first = ConversationDb::shared(dir.path(), dir.path(), OpenRole::Client).unwrap();
        let second = ConversationDb::shared(dir.path(), dir.path(), OpenRole::Client).unwrap();
        assert!(std::sync::Arc::ptr_eq(&first, &second));
    }

    /// 库文件整个换掉(删掉重建、导入顶替)之后,不能再递出指着旧文件的连接。
    #[test]
    fn a_replaced_database_file_gets_a_fresh_connection() {
        let dir = tempfile::tempdir().unwrap();
        let stale = ConversationDb::shared(dir.path(), dir.path(), OpenRole::Client).unwrap();
        for name in [
            "conversation.db",
            "conversation.db-wal",
            "conversation.db-shm",
        ] {
            let _ = std::fs::remove_file(dir.path().join(name));
        }
        let fresh = ConversationDb::shared(dir.path(), dir.path(), OpenRole::Client).unwrap();
        assert!(!std::sync::Arc::ptr_eq(&stale, &fresh));
        assert!(dir.path().join("conversation.db").exists());
    }
}

#[cfg(test)]
mod open_cost_probe {
    use super::*;

    /// 量尺：终端启动要开 6 次库（09-24 调研）。改前每次都是整套维护（体检扫全文件、
    /// 查迁移、够多空闲页就回收）；改后第一次是客户端开库，后面 5 次拿进程里那个。
    ///
    /// ```
    /// python3 -c "import sqlite3; s=sqlite3.connect('file:<真库>?mode=ro', uri=True); \
    ///   d=sqlite3.connect('<目录>/conversation.db'); s.backup(d)"
    /// MIYU_OPEN_PROBE_DIR=<目录> cargo test --lib --release open_cost_probe -- --ignored --nocapture
    /// ```
    ///
    /// 只认显式给的目录：量尺不该顺手打开用户正在用的库。拷副本要用只读连接的
    /// backup（页原样照搬，空闲页也在，体检的代价才对得上），别 `cp` 活库。
    #[test]
    #[ignore]
    fn six_opens_at_startup() {
        let Some(dir) = std::env::var_os("MIYU_OPEN_PROBE_DIR") else {
            println!("\n  跳过：没给 MIYU_OPEN_PROBE_DIR");
            return;
        };
        let dir = std::path::PathBuf::from(dir);
        // 先按守护进程的样子开一次：老库要回收的空闲页先回收掉，两边量的是同一个库。
        drop(ConversationDb::open(&dir).unwrap());
        let bytes = std::fs::metadata(dir.join("conversation.db"))
            .unwrap()
            .len();

        let started = std::time::Instant::now();
        for _ in 0..6 {
            drop(ConversationDb::open(&dir).unwrap());
        }
        let before = started.elapsed();

        let started = std::time::Instant::now();
        let first = ConversationDb::shared(&dir, &dir, OpenRole::Client).unwrap();
        for _ in 0..5 {
            drop(ConversationDb::shared(&dir, &dir, OpenRole::Client).unwrap());
        }
        let after = started.elapsed();
        drop(first);
        println!(
            "\n  库 {:.1} MB，开 6 次：改前 {:.1} ms，改后 {:.1} ms",
            bytes as f64 / 1048576.0,
            before.as_secs_f64() * 1000.0,
            after.as_secs_f64() * 1000.0
        );
    }
}
