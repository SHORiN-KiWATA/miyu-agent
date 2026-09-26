//! 用量账本的库：`state/usage.db`。
//!
//! 单连接加 Mutex（同 `LedgerDb`），进程内共用一个连接：每轮、每次辅助调用都要
//! 记账，不能每次都重新开库。

use super::*;
use crate::state::conversation_db::file_identity;
use anyhow::Context;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock, Weak};

/// 本进程开着的账本，按库文件路径记。只存 `Weak`：谁都不拿着了，连接就关。
static OPEN_LEDGERS: OnceLock<Mutex<HashMap<PathBuf, Weak<UsageDb>>>> = OnceLock::new();

/// 明细表的列。读写共用这一份，顺序要和 `record_from_row` 对上。
const RECORD_COLUMNS: &str =
    "ts, src, provider, model, prompt, completion, total, cache_read, cache_write, aux, kind, acct";

pub struct UsageDb {
    pub(super) conn: Mutex<Connection>,
    state_dir: PathBuf,
    identity: Option<(u64, u64)>,
}

pub(super) fn insert_record(conn: &Connection, record: &UsageRecord) -> Result<()> {
    conn.execute(
        &format!(
            "INSERT INTO usage_records ({RECORD_COLUMNS})
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)"
        ),
        params![
            record.ts,
            record.src,
            record.provider,
            record.model,
            record.prompt as i64,
            record.completion as i64,
            record.total as i64,
            record.cache_read as i64,
            record.cache_write as i64,
            record.aux,
            record.kind,
            record.acct,
        ],
    )?;
    Ok(())
}

fn record_from_row(row: &rusqlite::Row) -> rusqlite::Result<UsageRecord> {
    let count = |index: usize| row.get::<_, i64>(index).map(|value| value.max(0) as u64);
    Ok(UsageRecord {
        ts: row.get(0)?,
        src: row.get(1)?,
        provider: row.get(2)?,
        model: row.get(3)?,
        prompt: count(4)?,
        completion: count(5)?,
        total: count(6)?,
        cache_read: count(7)?,
        cache_write: count(8)?,
        aux: row.get(9)?,
        kind: row.get(10)?,
        acct: row.get(11)?,
        cost: None,
    })
}

fn totals_locked(conn: &Connection) -> Result<UsageState> {
    let raw: Option<String> = conn
        .query_row("SELECT state FROM usage_totals WHERE id = 1", [], |row| {
            row.get(0)
        })
        .optional()?;
    Ok(raw
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default())
}

impl UsageDb {
    pub(super) fn shared(state_dir: &Path) -> Result<Arc<Self>> {
        let path = state_dir.join("usage.db");
        let mut open = OPEN_LEDGERS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap();
        let live = open
            .get(&path)
            .and_then(Weak::upgrade)
            .filter(|db| db.identity.is_some() && db.identity == file_identity(&path));
        if let Some(db) = live {
            return Ok(db);
        }
        let db = Arc::new(Self::open(state_dir, &path)?);
        open.retain(|_, previous| previous.strong_count() > 0);
        open.insert(path, Arc::downgrade(&db));
        Ok(db)
    }

    fn open(state_dir: &Path, path: &Path) -> Result<Self> {
        std::fs::create_dir_all(state_dir)?;
        let conn = Connection::open(path)
            .with_context(|| format!("failed to open usage ledger: {}", path.display()))?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA busy_timeout = 5000;
             CREATE TABLE IF NOT EXISTS usage_records (
                 id          INTEGER PRIMARY KEY AUTOINCREMENT,
                 ts          INTEGER NOT NULL,
                 src         TEXT NOT NULL,
                 provider    TEXT NOT NULL,
                 model       TEXT NOT NULL,
                 prompt      INTEGER NOT NULL,
                 completion  INTEGER NOT NULL,
                 total       INTEGER NOT NULL,
                 cache_read  INTEGER NOT NULL,
                 cache_write INTEGER NOT NULL,
                 aux         INTEGER NOT NULL,
                 kind        TEXT NOT NULL,
                 acct        TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_usage_records_ts ON usage_records(ts);
             CREATE TABLE IF NOT EXISTS usage_totals (
                 id    INTEGER PRIMARY KEY CHECK (id = 1),
                 state TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS usage_meta (
                 key   TEXT PRIMARY KEY,
                 value TEXT NOT NULL
             );",
        )?;
        let db = Self {
            conn: Mutex::new(conn),
            state_dir: state_dir.to_path_buf(),
            identity: file_identity(path),
        };
        // 老账本导不进来不该让人记不了账：告警，下次开库再试。
        if let Err(error) = legacy::import(&db, state_dir) {
            tracing::warn!(error = %error, "importing the old usage files failed");
        }
        Ok(db)
    }

    /// 改累计：读、改、写在一个事务里，别的进程同时记账也不会互相覆盖。
    fn update_totals(&self, change: impl FnOnce(&mut UsageState)) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut state = totals_locked(&tx)?;
        change(&mut state);
        tx.execute(
            "INSERT INTO usage_totals (id, state) VALUES (1, ?1)
             ON CONFLICT(id) DO UPDATE SET state = excluded.state",
            params![serde_json::to_string(&state)?],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// 记一次调用的累计。`is_conversation` 为 false 是辅助调用（压缩、判官……），
    /// 不算进「这次对话」，也不顶掉上一次对话调用的用量。
    pub fn add(&self, usage: &Usage, is_conversation: bool) -> Result<()> {
        self.update_totals(|state| {
            state.requests += 1;
            state.prompt_tokens += usage.prompt_tokens;
            state.completion_tokens += usage.completion_tokens;
            state.total_tokens += usage.effective_total_tokens();
            state.cache_read_tokens += usage.cache_read_tokens;
            state.cache_write_tokens += usage.cache_write_tokens;
            state.reasoning_tokens += usage.reasoning_tokens;
            if is_conversation {
                state.conversation_tokens += usage.effective_total_tokens();
            }
            state.last_usage = Some(usage.clone());
            if is_conversation {
                state.last_conversation_usage = Some(usage.clone());
            }
        })
    }

    pub fn snapshot(&self) -> Result<UsageSnapshot> {
        let conn = self.conn.lock().unwrap();
        Ok(totals_locked(&conn)?.into())
    }

    pub fn clear_last_usage(&self) -> Result<()> {
        self.update_totals(|state| {
            state.last_usage = None;
            state.last_conversation_usage = None;
        })
    }

    pub fn reset_conversation(&self) -> Result<()> {
        self.update_totals(|state| {
            state.conversation_tokens = 0;
            state.last_usage = None;
            state.last_conversation_usage = None;
        })
    }

    /// 记一条明细，归到某个账号名下（空串 = 管理员/遗留/平台）。
    pub fn record(
        &self,
        usage: &Usage,
        meta: UsageMeta<'_>,
        aux: bool,
        account: &str,
    ) -> Result<()> {
        self.record_at(usage, meta, aux, account, chrono::Utc::now().timestamp())
    }

    pub(super) fn record_at(
        &self,
        usage: &Usage,
        meta: UsageMeta<'_>,
        aux: bool,
        account: &str,
        ts: i64,
    ) -> Result<()> {
        let record = UsageRecord {
            ts,
            src: if meta.source.is_empty() {
                "agent".to_string()
            } else {
                meta.source.to_string()
            },
            provider: meta.provider.unwrap_or_default().to_string(),
            model: meta.model.unwrap_or_default().to_string(),
            prompt: usage.prompt_tokens,
            completion: usage.completion_tokens,
            total: usage.effective_total_tokens(),
            cache_read: usage.cache_read_tokens,
            cache_write: usage.cache_write_tokens,
            aux,
            kind: meta.kind.unwrap_or_default().to_string(),
            acct: account.to_string(),
            cost: None,
        };
        let conn = self.conn.lock().unwrap();
        insert_record(&conn, &record)
    }

    /// 供应商改名：把明细里 `provider == old` 的行改成 `new`，返回改了几行。
    ///
    /// 明细存的是当时配置里的供应商 id。改了 id 而账本不动，统计页会把同一个
    /// 供应商拆成新旧两行，旧行还因为查不到 base_url 而算不出费用。
    pub fn rename_provider(&self, old: &str, new: &str) -> Result<usize> {
        if old == new || old.is_empty() {
            return Ok(0);
        }
        let conn = self.conn.lock().unwrap();
        Ok(conn.execute(
            "UPDATE usage_records SET provider = ?2 WHERE provider = ?1",
            params![old, new],
        )?)
    }

    /// 清空逐次调用明细。累计不动：那是「一生用了多少」的唯一来源，统计页要的
    /// 只是明细派生的图表与记录。
    pub fn clear_history(&self) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM usage_records", [])?;
        legacy::forget_history(&conn, &self.state_dir)
    }

    /// 全部明细，按写入先后；`account` 为 Some 时只要这个账号的。
    pub fn records(&self, account: Option<&str>) -> Result<Vec<UsageRecord>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&format!(
            "SELECT {RECORD_COLUMNS} FROM usage_records
              WHERE ?1 IS NULL OR acct = ?1
              ORDER BY id"
        ))?;
        let records = stmt
            .query_map(params![account], record_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(records)
    }

    /// 统计页、`usage` 工具要的汇总；`account` 为 Some 时只看该账号（空串 = 管理员/
    /// 遗留），None = 全部并按人拆分。
    pub fn stats(
        &self,
        range: UsageRange,
        price: PriceFn<'_>,
        account: Option<&str>,
    ) -> Result<UsageStats> {
        Ok(stats::aggregate(
            self.records(account)?,
            range,
            price,
            account,
        ))
    }

    /// 最近 `limit` 条调用记录，新的在前；可按来源、模型过滤。按 ts 排序而不是
    /// 写入顺序：写入顺序通常就是时间顺序，但时钟回拨或手工并档后，也要给出
    /// 正确的「最近」。同一秒的几条按写入先后，和以前的稳定排序一致。
    pub fn details(
        &self,
        limit: usize,
        src: Option<&str>,
        model: Option<&str>,
        price: PriceFn<'_>,
        account: Option<&str>,
    ) -> Result<Vec<UsageRecord>> {
        let src = src.filter(|value| !value.is_empty());
        let model = model.filter(|value| !value.is_empty());
        let mut records = {
            let conn = self.conn.lock().unwrap();
            let mut stmt = conn.prepare(&format!(
                "SELECT {RECORD_COLUMNS} FROM usage_records
                  WHERE (?1 IS NULL OR acct = ?1)
                    AND (?2 IS NULL OR (CASE WHEN src = '' THEN 'agent' ELSE src END) = ?2)
                    AND (?3 IS NULL OR model = ?3)
                  ORDER BY ts DESC, id ASC
                  LIMIT ?4"
            ))?;
            let records = stmt
                .query_map(params![account, src, model, limit as i64], record_from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            records
        };
        stats::price_records(&mut records, price);
        Ok(records)
    }
}
