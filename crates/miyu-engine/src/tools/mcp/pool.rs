//! 常驻连接池（09-25，用户拍板：按「服务器 + 会话 + 沙盒」各一份）。
//!
//! 原来每调一次 MCP 工具就新起一个服务器进程、用完就杀：浏览器、数据库这类有状态的服务器
//! 什么都记不住。规矩：
//! - 只在 daemon 里开（[`enable`]）；单次 CLI、直连模式照旧每次新起、用完就收。
//! - 钥匙 = 服务器配置 + 调用方这一侧（会话、装的沙盒策略、工作目录，见
//!   `SpawnScope::fingerprint`）。同一会话里连续调用复用一个进程；不同会话、沙盒不同各起
//!   各的，状态和目录不串，成员也借不到属主起的进程。
//! - 闲置 10 分钟回收（30 秒巡检一次）；全机最多 8 个，满了这一次退回新起、用完就收。
//! - 进程死了：下一次调用重起，结果里注明之前的状态没了。一分钟里崩三次就停一分钟，免得
//!   一个起不来的服务器每次调用都白起一遍。
//! - 连着两次超时：进程收掉，下一次重起（多半卡死了）。工具调用本身绝不自动重试——工具
//!   可能有副作用。
//! - 清空 / 删除会话收掉它名下的；改配置收掉配置变了的；daemon 关停全部收掉。

use super::connection::{request_timeout, McpConnection};
use super::protocol::with_notice;
use super::runtime;
use super::scope::SpawnScope;
use anyhow::{bail, Result};
use miyu_base::config::{AppConfig, McpServerConfig};
use serde_json::Value;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

const IDLE_AFTER: Duration = Duration::from_secs(600);
const CAP: usize = 8;
const SWEEP_EVERY: Duration = Duration::from_secs(30);
const CRASH_WINDOW: Duration = Duration::from_secs(60);
const CRASH_LIMIT: usize = 3;
const CRASH_PAUSE: Duration = Duration::from_secs(60);
const TIMEOUT_LIMIT: u32 = 2;

static ENABLED: AtomicBool = AtomicBool::new(false);

struct Entry {
    key: String,
    server: String,
    session: Arc<str>,
    connection: Arc<McpConnection>,
    last_used: Instant,
}

#[derive(Default)]
struct Pool {
    entries: Vec<Entry>,
    /// 收掉过的钥匙 → 哪个会话、为什么。下一次在这把钥匙上重起时告诉模型之前的状态没了。
    retired: HashMap<String, (Arc<str>, &'static str)>,
    /// 每个服务器最近几次崩的时刻。
    crashes: HashMap<String, VecDeque<Instant>>,
    /// 崩得太多、停到什么时候。
    paused: HashMap<String, Instant>,
}

fn pool() -> &'static Mutex<Pool> {
    static POOL: OnceLock<Mutex<Pool>> = OnceLock::new();
    POOL.get_or_init(|| Mutex::new(Pool::default()))
}

/// 同一把钥匙两个调用同时进来时只起一个进程。
fn starting_lock(key: &str) -> Arc<tokio::sync::Mutex<()>> {
    static STARTING: OnceLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> =
        OnceLock::new();
    STARTING
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap()
        .entry(key.to_string())
        .or_default()
        .clone()
}

/// 打开常驻（daemon 启动时）。
pub fn enable() {
    if ENABLED.swap(true, Ordering::AcqRel) {
        return;
    }
    runtime::handle().spawn(async {
        loop {
            tokio::time::sleep(SWEEP_EVERY).await;
            sweep(Instant::now());
        }
    });
}

/// 服务器配置的指纹：它变了，老进程就不能再用。
fn server_fingerprint(server: &McpServerConfig) -> String {
    let mut env = server.env.iter().collect::<Vec<_>>();
    env.sort();
    let mut hasher = blake3::Hasher::new();
    for part in [
        server.id.as_str(),
        server.command.as_str(),
        &server.args.join("\0"),
        &format!("{env:?}"),
        &server.timeout_seconds.to_string(),
        &server.capabilities.join("\0"),
        &format!("{:?}", server.sandbox),
        &server.sandbox_writable.join("\0"),
    ] {
        hasher.update(part.as_bytes());
        hasher.update(b"\x1f");
    }
    hasher.finalize().to_hex().to_string()
}

/// 调一件工具。在 MCP 线程上跑。
pub(super) async fn call(
    server: McpServerConfig,
    scope: SpawnScope,
    tool: String,
    args: Value,
) -> Result<String> {
    let session = scope
        .session
        .clone()
        .filter(|_| ENABLED.load(Ordering::Acquire) && server.persistent);
    let Some(session) = session else {
        return one_off(&server, &scope, &tool, args, None).await;
    };
    let server_key = server_fingerprint(&server);
    let key = format!("{server_key}:{}", scope.fingerprint(&server));
    check_paused(&server, &server_key)?;
    let starting = starting_lock(&key);
    let started = starting.lock().await;
    let (connection, notice) = match live_connection(&key) {
        Some(connection) => (connection, None),
        None => {
            let notice = take_retired(&key).map(|reason| {
                format!(
                    "MCP server {} was restarted ({reason}); state from earlier calls is gone.",
                    server.id
                )
            });
            if !has_room(Instant::now()) {
                drop(started);
                let full = "MCP connection pool is full; this call used a fresh server process, so earlier state is not available.";
                let notice = match notice {
                    Some(notice) => format!("{notice} {full}"),
                    None => full.to_string(),
                };
                return one_off(&server, &scope, &tool, args, Some(notice)).await;
            }
            match McpConnection::open(&server, &scope).await {
                Ok(connection) => {
                    let connection = Arc::new(connection);
                    tracing::info!(
                        server = %server.id,
                        session = %session,
                        pid = connection.pid(),
                        "started a persistent MCP server process"
                    );
                    pool().lock().unwrap().entries.push(Entry {
                        key: key.clone(),
                        server: server_key.clone(),
                        session,
                        connection: connection.clone(),
                        last_used: Instant::now(),
                    });
                    (connection, notice)
                }
                Err(error) => {
                    record_crash(&server_key, Instant::now());
                    return Err(error);
                }
            }
        }
    };
    drop(started);
    let outcome = connection
        .call_tool(&tool, args, request_timeout(&server))
        .await;
    settle(&key, &server_key, &connection);
    let output = outcome?;
    Ok(match notice {
        Some(notice) => with_notice(output, &notice),
        None => output,
    })
}

/// 不进池：起一个、调一次、收掉。
async fn one_off(
    server: &McpServerConfig,
    scope: &SpawnScope,
    tool: &str,
    args: Value,
    notice: Option<String>,
) -> Result<String> {
    let connection = McpConnection::open(server, scope).await?;
    let outcome = connection
        .call_tool(tool, args, request_timeout(server))
        .await;
    // 一次性的进程没有状态要留：整组直接收掉（连接丢掉时 SIGKILL），不等它自己退。
    drop(connection);
    let output = outcome?;
    Ok(match notice {
        Some(notice) => with_notice(output, &notice),
        None => output,
    })
}

fn live_connection(key: &str) -> Option<Arc<McpConnection>> {
    let mut pool = pool().lock().unwrap();
    let entry = pool.entries.iter_mut().find(|entry| entry.key == key)?;
    if !entry.connection.is_alive() {
        return None;
    }
    entry.last_used = Instant::now();
    Some(entry.connection.clone())
}

fn take_retired(key: &str) -> Option<&'static str> {
    pool()
        .lock()
        .unwrap()
        .retired
        .remove(key)
        .map(|(_, reason)| reason)
}

/// 一次调用之后：死了的记一次崩、收掉；连着超时的收掉；好好的记下用过的时刻。
fn settle(key: &str, server_key: &str, connection: &Arc<McpConnection>) {
    let now = Instant::now();
    let reason = if !connection.is_alive() {
        record_crash(server_key, now);
        Some("it crashed")
    } else if connection.consecutive_timeouts() >= TIMEOUT_LIMIT {
        Some("it stopped answering")
    } else {
        None
    };
    let mut pool = pool().lock().unwrap();
    let Some(index) = pool
        .entries
        .iter()
        .position(|entry| entry.key == key && Arc::ptr_eq(&entry.connection, connection))
    else {
        return;
    };
    match reason {
        None => pool.entries[index].last_used = now,
        Some(reason) => {
            let entry = pool.entries.remove(index);
            pool.retired
                .insert(entry.key.clone(), (entry.session.clone(), reason));
            drop(pool);
            retire(entry, reason);
        }
    }
}

fn record_crash(server_key: &str, now: Instant) {
    let mut pool = pool().lock().unwrap();
    let crashes = pool.crashes.entry(server_key.to_string()).or_default();
    crashes.push_back(now);
    while crashes
        .front()
        .is_some_and(|at| now.saturating_duration_since(*at) > CRASH_WINDOW)
    {
        crashes.pop_front();
    }
    if crashes.len() >= CRASH_LIMIT {
        crashes.clear();
        pool.paused
            .insert(server_key.to_string(), now + CRASH_PAUSE);
    }
}

fn check_paused(server: &McpServerConfig, server_key: &str) -> Result<()> {
    let mut pool = pool().lock().unwrap();
    let Some(until) = pool.paused.get(server_key).copied() else {
        return Ok(());
    };
    let now = Instant::now();
    if now >= until {
        pool.paused.remove(server_key);
        return Ok(());
    }
    bail!(
        "MCP server {} crashed {CRASH_LIMIT} times within a minute; it is paused for another {}s",
        server.id,
        until.saturating_duration_since(now).as_secs().max(1)
    )
}

/// 池里还有没有空位（先把闲置太久的收掉）。
fn has_room(now: Instant) -> bool {
    sweep(now);
    pool().lock().unwrap().entries.len() < CAP
}

/// 巡检：闲置太久的、闲着的时候死掉的，收掉。
fn sweep(now: Instant) {
    let mut expired = Vec::new();
    {
        let mut pool = pool().lock().unwrap();
        let mut kept = Vec::with_capacity(pool.entries.len());
        for entry in pool.entries.drain(..) {
            let reason = if !entry.connection.is_alive() {
                Some("it crashed")
            } else if now.saturating_duration_since(entry.last_used) >= IDLE_AFTER {
                Some("it was idle for 10 minutes")
            } else {
                None
            };
            match reason {
                Some(reason) => expired.push((entry, reason)),
                None => kept.push(entry),
            }
        }
        pool.entries = kept;
        for (entry, reason) in &expired {
            pool.retired
                .insert(entry.key.clone(), (entry.session.clone(), reason));
        }
    }
    for (entry, reason) in expired {
        retire(entry, reason);
    }
}

fn retire(entry: Entry, reason: &str) {
    tracing::info!(
        pid = entry.connection.pid(),
        session = %entry.session,
        "retiring MCP server process: {reason}"
    );
    runtime::handle().spawn(async move { entry.connection.shutdown().await });
}

/// 清空 / 删除会话：它名下的进程收掉，「之前的状态没了」的账也清掉（会话都重来了）。
pub fn forget_session(session_id: &str) {
    let removed = {
        let mut pool = pool().lock().unwrap();
        pool.retired
            .retain(|_, (session, _)| session.as_ref() != session_id);
        let (removed, kept): (Vec<_>, Vec<_>) = pool
            .entries
            .drain(..)
            .partition(|entry| entry.session.as_ref() == session_id);
        pool.entries = kept;
        removed
    };
    for entry in removed {
        retire(entry, "its session was reset or deleted");
    }
}

/// 改配置之后：配置变了、删掉的、关掉的服务器的进程收掉。
pub fn retire_changed(config: &AppConfig) {
    let current = config
        .mcp
        .servers
        .iter()
        .filter(|server| config.mcp.enabled && server.enabled)
        .map(server_fingerprint)
        .collect::<HashSet<_>>();
    let removed = {
        let mut pool = pool().lock().unwrap();
        let (removed, kept): (Vec<_>, Vec<_>) = pool
            .entries
            .drain(..)
            .partition(|entry| !current.contains(&entry.server));
        pool.entries = kept;
        removed
    };
    for entry in removed {
        retire(entry, "its configuration changed");
    }
}

/// daemon 关停：全部收掉，等它们退完。
pub async fn shutdown_all() {
    ENABLED.store(false, Ordering::Release);
    let entries = std::mem::take(&mut pool().lock().unwrap().entries);
    if entries.is_empty() {
        return;
    }
    let tasks = entries
        .into_iter()
        .map(|entry| runtime::handle().spawn(async move { entry.connection.shutdown().await }))
        .collect::<Vec<_>>();
    for task in tasks {
        let _ = task.await;
    }
}

#[cfg(test)]
mod test_support;
#[cfg(test)]
pub(super) use test_support::*;
