//! 开库:建连接、体检、迁移前备份、迁移、回收空闲页。
//!
//! 09-24 从 `mod.rs` 整段搬来(会话项目第 1 段),好在这里把「谁该做维护」
//! 和「进程内共用连接」加进来,不再撑大 `mod.rs`。

use super::*;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, Weak};

/// 本进程开着的库,按库文件路径记。只存 `Weak`:谁都不拿着了连接就关,测试里
/// 成千上万个临时库不会一直占着文件句柄。
static SHARED_CONNECTIONS: OnceLock<Mutex<HashMap<PathBuf, Weak<ConversationDb>>>> =
    OnceLock::new();

/// 库文件的 (设备号, inode)。进程内缓存靠它认出「库文件被整个换掉了」——删掉
/// 重建、导入顶替之后,还拿着旧文件的连接读到的是已经不存在的数据。
pub(super) fn file_identity(path: &Path) -> Option<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(path)
            .ok()
            .map(|meta| (meta.dev(), meta.ino()))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

/// 08-21 生图 bug 教训:损坏库(freelist 不一致)只在 ≥1MB blob 写入时失败,且
/// 失败只打 warn,静默丢了三天图片资产。维护时先体检,坏了就用默认级别可见的
/// error 亮出来;只报不拦——拦截会把用户整个锁在门外。
fn quick_check(conn: &Connection, db_path: &Path) -> String {
    let check = conn
        .query_row("PRAGMA quick_check(1)", [], |row| row.get::<_, String>(0))
        .unwrap_or_else(|error| format!("quick_check failed: {error}"));
    if check != "ok" {
        tracing::error!(
            db = %db_path.display(),
            %check,
            "conversation.db 未通过完整性体检;大对象写入可能失败,请备份后用 sqlite3 .recover 重建"
        );
    }
    check
}

/// 把删掉的行占的页真正还给磁盘。
///
/// SQLite 删行只是把页挂进 freelist，文件本身**不会变小**，那些页也不会还给
/// 操作系统。本机实测：conversation.db 76 MB，其中 **90%（67.9 MB）是空闲页**，
/// 存活数据只有 7.4 MB——清理过的旧会话、旧图片全躺在那儿占着地方。
///
/// 同仓库的 message_history 库一直是对的（`auto_vacuum = INCREMENTAL` +
/// 每次清理跑一段有界的 `incremental_vacuum`），只有这个库漏了。
///
/// 两条路：
///
/// - **老库**（`auto_vacuum` 读出来是 0）：`open` 里那句 PRAGMA 对已有数据的
///   库是空转——实测设完立刻读回来还是 0，必须**紧跟一次完整 VACUUM** 才落地。
///   VACUUM 的代价只随存活数据长（实测约 2.6 ms/MB：存活 5.6 MB→15 ms、
///   22.5 MB→58 ms、67.5 MB→175 ms），跟文件多大无关。而且只会发生一次：
///   转换完 `auto_vacuum` 就是 2 了。
/// - **已转换的库**：只回收有界的一小段，照抄 message_history 的 256 页
///   （4 KB 页面下是 1 MB）。不在启动路径上跑完整 VACUUM。
///
/// 全程 `let _ =`：回收空间失败不该让人打不开自己的会话库。
fn reclaim_free_pages(conn: &Connection) {
    let mode: i64 = conn
        .query_row("PRAGMA auto_vacuum", [], |row| row.get(0))
        .unwrap_or(0);
    if mode == 0 {
        let _ = conn.execute_batch("VACUUM;");
        return;
    }
    // 08-21 取证:incremental_vacuum 的截断提交对 wal/主文件失配零容忍,是把
    // 失配放大成截断损坏的放大器;子代理审计等瞬时连接也在每次 open 白跑它。
    // 攒到 64 空闲页(256 KB)再回收,把最脆的代码路径从"每次 open"降到"偶尔"。
    let free: i64 = conn
        .query_row("PRAGMA freelist_count", [], |row| row.get(0))
        .unwrap_or(0);
    if free < 64 {
        return;
    }
    let _ = conn.execute_batch("PRAGMA incremental_vacuum(256);");
}

/// 这条错误链里有没有 SQLite 的「库损坏」。损坏码有两个:`DatabaseCorrupt`
/// (SQLITE_CORRUPT,11) 是页面结构坏了,`NotADatabase` (SQLITE_NOTADB,26) 是
/// 文件头就不对——对用户是同一件事,恢复手段也一样。
fn is_database_corrupt(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        matches!(
            cause.downcast_ref::<rusqlite::Error>(),
            Some(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error {
                    code: rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase,
                    ..
                },
                _
            ))
        )
    })
}

/// 谁在开库。
///
/// 守护进程是库的看门人:体检(`quick_check` 要把整个文件扫一遍)、迁移前备份、
/// 回收空闲页都只在它那儿做。客户端——终端、一次性命令、守护进程里临时开库的
/// 工具——库已经是当前版本就直接用:终端启动要开 6 次库、换一次会话 3 次、每轮
/// 3 次,以前每次都扫一遍整个文件(09-24 调研)。库落后于本程序时谁开谁迁移,
/// 迁移前的体检与备份照旧。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OpenRole {
    Maintain,
    Client,
}

impl ConversationDb {
    /// 进程内共用的连接:同一个库文件在一个进程里只开一次。以前终端每次要读点
    /// 什么都新开一个连接,启动 6 次、换会话 3 次、每轮 3 次(09-24 调研)。
    pub(crate) fn shared(db_dir: &Path, state_dir: &Path, role: OpenRole) -> Result<Arc<Self>> {
        let db_path = db_dir.join("conversation.db");
        let mut open = SHARED_CONNECTIONS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap();
        let live = open
            .get(&db_path)
            .and_then(Weak::upgrade)
            .filter(|db| db.file_identity.is_some() && db.file_identity == file_identity(&db_path));
        let db = match live {
            Some(db) => db,
            None => {
                let db = Arc::new(Self::open_with_role(db_dir, state_dir, role)?);
                open.retain(|_, previous| previous.strong_count() > 0);
                open.insert(db_path, Arc::downgrade(&db));
                db
            }
        };
        drop(open);
        if role == OpenRole::Maintain {
            db.maintain_once();
        }
        Ok(db)
    }

    /// 守护进程拿到的是客户端先开好的连接时,补做一次体检与回收。
    fn maintain_once(&self) {
        if self.maintained.swap(true, Ordering::AcqRel) {
            return;
        }
        let conn = self.conn.lock().unwrap();
        if quick_check(&conn, &self.db_path) == "ok" {
            reclaim_free_pages(&conn);
        }
    }

    /// 库文件在 `db_dir`(家目录布局下是管理员家目录),附件仍在 state。
    pub fn open_at(db_dir: &Path, state_dir: &Path) -> Result<Self> {
        Self::open_with_role(db_dir, state_dir, OpenRole::Maintain)
    }

    fn open_with_role(db_dir: &Path, state_dir: &Path, role: OpenRole) -> Result<Self> {
        let db_path = db_dir.join("conversation.db");
        Self::open_inner(db_dir, state_dir, role).map_err(|error| {
            if !is_database_corrupt(&error) {
                return error;
            }
            let backup = db_dir.join("conversation.db.bak");
            let recovery = if backup.exists() {
                format!(
                    "\n可用的迁移前备份：{}（改名成 conversation.db 顶上，会丢掉最后一次版本升级之后的记录）",
                    backup.display()
                )
            } else {
                String::new()
            };
            error.context(format!(
                "会话数据库已损坏：{}\n先停掉所有 miyu 进程，再任选一条：\n\
                 1. 抢救数据：用 sqlite3 命令行对该文件跑 \".recover\" 导出后重建\n\
                 2. 放弃历史：把 conversation.db、conversation.db-wal、conversation.db-shm 一起挪走，Miyu 会重建空库{}",
                db_path.display(),
                recovery
            ))
        })
    }

    fn open_inner(db_dir: &Path, state_dir: &Path, role: OpenRole) -> Result<Self> {
        std::fs::create_dir_all(db_dir)?;
        std::fs::create_dir_all(state_dir)?;
        let db_path = db_dir.join("conversation.db");
        let mut conn = Connection::open(&db_path)
            .with_context(|| format!("failed to open conversation db: {}", db_path.display()))?;
        conn.execute_batch(
            // auto_vacuum 必须在建表**之前**设，新库才认。老库这句是空转，
            // 由下面的 `reclaim_free_pages` 补一次 VACUUM 来落地。
            // cache_size：本库存活数据只有个位数 MB、典型查询 1-2 ms，
            // 默认 2 MB 页缓存对它是浪费，1 MB 足够且无感。
            "PRAGMA auto_vacuum = INCREMENTAL;
             PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA busy_timeout = 5000;
             PRAGMA foreign_keys = ON;
             PRAGMA cache_size = -1024;",
        )?;
        let behind = crate::state::migrations::current_version(&conn)?
            < crate::state::migrations::LATEST_VERSION;
        // 客户端开当前版本的库什么维护也不做(见 `OpenRole`);库落后时谁开谁
        // 迁移,迁移前的体检与备份照旧。
        let maintained = role == OpenRole::Maintain || behind;
        let check = if maintained {
            quick_check(&conn, &db_path)
        } else {
            String::new()
        };
        // Back up the database file before applying schema migrations to a
        // database that already holds data.
        if behind {
            let has_turns: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='turns')",
                [],
                |row| row.get(0),
            )?;
            // 体检不过就不导:坏库上 VACUUM 会把失配放大成截断损坏(08-21
            // 取证,下面 reclaim_free_pages 同一个判据)。这里原来没挡,坏库
            // 照样往下走。
            if has_turns && check == "ok" {
                // 08-21 取证:std::fs::copy 打开再 close 主库文件,POSIX close
                // 语义会连带丢掉本进程在主文件上的常驻 WAL 锁,且拷出的副本
                // 可能撕裂。VACUUM INTO 在 SQLite 事务内导出自洽副本,两个坑
                // 一起填(transfer/export.rs 同范式)。
                //
                // 先导到暂存文件、成功了再改名顶上。原来是"先 remove 旧备份、
                // 再 VACUUM INTO、失败只 log"——在坏库上 VACUUM 几乎必失败,
                // 于是用户最需要备份的那一刻,上一份好备份刚被删掉,新的又没
                // 生成(08-29 用户反馈的坏库现场)。
                let bak = db_dir.join("conversation.db.bak");
                let staging = db_dir.join("conversation.db.bak.new");
                let _ = std::fs::remove_file(&staging);
                match conn.execute("VACUUM INTO ?1", [staging.to_string_lossy().as_ref()]) {
                    Ok(_) => {
                        if let Err(error) = std::fs::rename(&staging, &bak) {
                            let _ = std::fs::remove_file(&staging);
                            tracing::error!(%error, "conversation.db 迁移前备份改名失败(旧备份保留)");
                        }
                    }
                    Err(error) => {
                        let _ = std::fs::remove_file(&staging);
                        tracing::error!(%error, "conversation.db 迁移前备份失败(旧备份保留,继续迁移)");
                    }
                }
            } else if has_turns {
                tracing::error!(
                    %check,
                    "conversation.db 体检未通过,跳过迁移前备份以免放大损坏;已有的 conversation.db.bak 保持不动"
                );
            }
        }
        crate::state::migrations::run_migrations(&mut conn)?;
        crate::state::migrations::run_named_migrations(&mut conn)?;
        // 坏库上跑 vacuum 会把失配放大成截断损坏(08-21 取证),体检不过就跳过。
        if check == "ok" {
            reclaim_free_pages(&conn);
        }
        Ok(Self {
            conn: Mutex::new(conn),
            attachments_dir: state_dir.join("attachments"),
            file_identity: file_identity(&db_path),
            maintained: AtomicBool::new(maintained),
            db_path,
        })
    }
}

#[cfg(test)]
mod tests;
