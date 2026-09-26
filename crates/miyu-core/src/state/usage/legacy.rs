//! 把老账本文件导进 `usage.db`。
//!
//! - 累计（`usage.json`）只导一次；
//! - 明细（`usage-history.jsonl`）按字节偏移续导：回退到老版本后又追加的那几行，
//!   再升级时也补得上。
//!
//! 老文件本版保留，回退到上一版还能读到。清空明细时连老文件一起删。

use super::db::insert_record;
use super::*;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use std::io::{BufRead, BufReader, Seek, SeekFrom};

const TOTALS_FILE: &str = "usage.json";
const HISTORY_FILE: &str = "usage-history.jsonl";
const TOTALS_IMPORTED: &str = "legacy_totals_imported";
const HISTORY_OFFSET: &str = "legacy_history_offset";

fn meta(conn: &Connection, key: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT value FROM usage_meta WHERE key = ?1",
            params![key],
            |row| row.get(0),
        )
        .optional()?)
}

fn set_meta(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO usage_meta (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

fn history_offset(conn: &Connection) -> Result<u64> {
    Ok(meta(conn, HISTORY_OFFSET)?
        .and_then(|value| value.parse().ok())
        .unwrap_or(0))
}

pub(super) fn import(db: &UsageDb, state_dir: &Path) -> Result<()> {
    let mut conn = db.conn.lock().unwrap();
    import_totals(&mut conn, &state_dir.join(TOTALS_FILE))?;
    import_history(&mut conn, &state_dir.join(HISTORY_FILE))
}

fn import_totals(conn: &mut Connection, path: &Path) -> Result<()> {
    if meta(conn, TOTALS_IMPORTED)?.is_some() {
        return Ok(());
    }
    let state = std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str::<UsageState>(&raw).ok());
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    // 别的进程可能刚刚抢先导完：拿到写锁以后再看一眼。
    if meta(&tx, TOTALS_IMPORTED)?.is_none() {
        if let Some(state) = state {
            tx.execute(
                "INSERT OR IGNORE INTO usage_totals (id, state) VALUES (1, ?1)",
                params![serde_json::to_string(&state)?],
            )?;
        }
        set_meta(&tx, TOTALS_IMPORTED, "1")?;
    }
    tx.commit()?;
    Ok(())
}

fn import_history(conn: &mut Connection, path: &Path) -> Result<()> {
    let Ok(file_len) = std::fs::metadata(path).map(|meta| meta.len()) else {
        return Ok(());
    };
    if history_offset(conn)? == file_len {
        return Ok(());
    }
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let offset = history_offset(&tx)?;
    if file_len < offset {
        // 老文件变短了：老版本改供应商名时整个重写过，或者清空过。偏移已经对不上，
        // 只能从这里接着记。
        set_meta(&tx, HISTORY_OFFSET, &file_len.to_string())?;
        tx.commit()?;
        return Ok(());
    }
    let mut file = std::fs::File::open(path)?;
    file.seek(SeekFrom::Start(offset))?;
    let mut reader = BufReader::new(file);
    let mut consumed = offset;
    let mut line = String::new();
    loop {
        line.clear();
        let read = reader.read_line(&mut line)?;
        // 只导写完的行：老版本可能正写到一半，没收尾的那行留到下次。
        if read == 0 || !line.ends_with('\n') {
            break;
        }
        consumed += read as u64;
        // 坏行跳过，不让一条脏数据废掉整个统计。
        if let Ok(record) = serde_json::from_str::<UsageRecord>(line.trim()) {
            insert_record(&tx, &record)?;
        }
    }
    set_meta(&tx, HISTORY_OFFSET, &consumed.to_string())?;
    tx.commit()?;
    Ok(())
}

/// 清空明细时连老文件一起删，偏移归零：之后老版本再建的新文件从头导。
pub(super) fn forget_history(conn: &Connection, state_dir: &Path) -> Result<()> {
    match std::fs::remove_file(state_dir.join(HISTORY_FILE)) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    set_meta(conn, HISTORY_OFFSET, "0")
}
