//! tools/list 缓存（09-04 issue #36）与服务器说明（09-25）。
//!
//! 注册表每次重建（cold_context、TurnResources 的 normal/dev 两套、配置保存……）原来都对
//! 每个 MCP server 现 spawn 一次子进程做 tools/list，并且是串行的：一个不可达的 server
//! 就把每次重建拖到 timeout_seconds + 5s，daemon 启动路径上这条链跑在 bind 之前。三刀：
//! (1) 列举结果按 server 配置缓存在进程里，成功永久（配置一变键就变），失败带 TTL；
//! (2) 未命中的 server 并行列举；(3) 列举预算与 `timeout_seconds` 解耦、单独封顶。
//!
//! 09-25：握手时服务器给的使用说明跟工具清单一起记下（系统提示词里那一段读它，见
//! `instructions`）；daemon 在回合开始前先异步列好缺的（[`prefetch`]），注册表同步重建时
//! 就全是命中，不再让 actor 线程干等。

use super::connection::McpConnection;
use super::protocol::McpToolInfo;
use super::runtime;
use super::scope::SpawnScope;
use anyhow::Result;
use miyu_base::config::{AppConfig, McpServerConfig};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// 失败的列举在这段时间内不再重试：死 server 让每次注册表重建都白等一轮太浪费，但也
/// 不能永久判死——server 修好了得能自动回来。
pub(super) const FAILED_LISTING_RETRY_AFTER: Duration = Duration::from_secs(60);

/// tools/list 外层预算的封顶（秒），不含 5s 余量。
pub(super) const LIST_TIMEOUT_CAP_SECS: u64 = 15;

/// 一个服务器列出来的东西。
#[derive(Debug)]
pub(super) struct Listing {
    pub(super) tools: Vec<McpToolInfo>,
    pub(super) instructions: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ListingKey {
    id: String,
    command: String,
    args: Vec<String>,
    env: Vec<(String, String)>,
    timeout_seconds: u64,
    capabilities: Vec<String>,
}

impl ListingKey {
    fn of(server: &McpServerConfig) -> Self {
        let mut env: Vec<(String, String)> = server
            .env
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        env.sort();
        Self {
            id: server.id.clone(),
            command: server.command.clone(),
            args: server.args.clone(),
            env,
            timeout_seconds: server.timeout_seconds,
            capabilities: server.capabilities.clone(),
        }
    }
}

#[derive(Debug, Clone)]
enum CachedListing {
    Listed(Arc<Listing>),
    Failed { at: Instant, error: String },
}

fn cache() -> &'static Mutex<HashMap<ListingKey, CachedListing>> {
    static CACHE: OnceLock<Mutex<HashMap<ListingKey, CachedListing>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 缓存里可用的条目：成功的永远可用，失败的在 TTL 内可用（返回 Err 让调用方跳过、
/// 不重试），过期的当未命中。
fn cached(key: &ListingKey) -> Option<std::result::Result<Arc<Listing>, String>> {
    let cache = cache().lock().unwrap();
    match cache.get(key)? {
        CachedListing::Listed(listing) => Some(Ok(listing.clone())),
        CachedListing::Failed { at, error } => {
            (at.elapsed() < FAILED_LISTING_RETRY_AFTER).then(|| Err(error.clone()))
        }
    }
}

fn store(
    server: &McpServerConfig,
    key: ListingKey,
    outcome: Result<Listing>,
) -> Option<Arc<Listing>> {
    match outcome {
        Ok(listing) => {
            tracing::info!(
                server = %server.id,
                tools = listing.tools.len(),
                instructions = listing.instructions.is_some(),
                "MCP server tools listed"
            );
            let listing = Arc::new(listing);
            cache()
                .lock()
                .unwrap()
                .insert(key, CachedListing::Listed(listing.clone()));
            Some(listing)
        }
        Err(error) => {
            // 失败必须留日志，否则用户无从排查工具为何消失。daemon 默认只记 error 级，
            // warn 用户看不到。
            let error = format!("{error:#}");
            tracing::error!(
                server = %server.id,
                error = %error,
                retry_after_secs = FAILED_LISTING_RETRY_AFTER.as_secs(),
                "MCP server failed to start or list tools; its tools are skipped"
            );
            cache().lock().unwrap().insert(
                key,
                CachedListing::Failed {
                    at: Instant::now(),
                    error,
                },
            );
            None
        }
    }
}

/// tools/list 的外层预算：`timeout_seconds` 是给工具调用的，列举单独封顶。
pub(super) fn list_timeout(server: &McpServerConfig) -> Duration {
    Duration::from_secs(server.timeout_seconds.clamp(1, LIST_TIMEOUT_CAP_SECS))
        + Duration::from_secs(5)
}

/// 起一个一次性的进程，握手、问工具清单、收掉。在 MCP 线程上跑。
async fn list_server(server: McpServerConfig) -> Result<Listing> {
    let budget = list_timeout(&server);
    let listed = tokio::time::timeout(budget, async {
        let connection = McpConnection::open(&server, &SpawnScope::listing()).await?;
        let tools = connection.list_tools(budget).await;
        let instructions = connection.instructions().map(str::to_string);
        // 一次性的进程：整组直接收掉（连接丢掉时 SIGKILL），不等它自己退。
        drop(connection);
        Ok(Listing {
            tools: tools?,
            instructions,
        })
    })
    .await;
    match listed {
        Ok(outcome) => outcome,
        Err(_) => anyhow::bail!(
            "MCP server {} did not answer tools/list within {}s",
            server.id,
            budget.as_secs()
        ),
    }
}

/// 把一批 server 的清单拿到手：命中缓存的直接用，未命中的在 MCP 线程上并行列举后回填。
/// 返回顺序与入参一致；列举失败的 server 值为 None。同步等（注册表是同步建的）。
pub(super) fn resolve(servers: &[&McpServerConfig]) -> Vec<Option<Arc<Listing>>> {
    let mut resolved: Vec<Option<Arc<Listing>>> = vec![None; servers.len()];
    let mut pending = Vec::new();
    for (index, server) in servers.iter().enumerate() {
        let key = ListingKey::of(server);
        match cached(&key) {
            Some(Ok(listing)) => resolved[index] = Some(listing),
            Some(Err(error)) => tracing::debug!(
                server = %server.id,
                error = %error,
                "MCP server listing recently failed; skipping until retry window passes"
            ),
            None => pending.push((index, key)),
        }
    }
    if pending.is_empty() {
        return resolved;
    }
    // 结果按完成顺序收：秒退的 server 立刻落日志、进缓存，不用排在最慢那个后面等。
    let (outcome_tx, outcome_rx) = std::sync::mpsc::channel();
    let expected = pending.len();
    let mut deadline = Duration::ZERO;
    for (index, key) in pending {
        let server = servers[index].clone();
        deadline = deadline.max(list_timeout(&server));
        let outcome_tx = outcome_tx.clone();
        tracing::info!(server = %server.id, "listing MCP server tools");
        runtime::handle().spawn(async move {
            let outcome = list_server(server).await;
            let _ = outcome_tx.send((index, key, outcome));
        });
    }
    drop(outcome_tx);
    // 每个列举自己带期限；这里再多等一秒兜住调度。
    let give_up = Instant::now() + deadline + Duration::from_secs(1);
    for _ in 0..expected {
        let left = give_up.saturating_duration_since(Instant::now());
        let Ok((index, key, outcome)) = outcome_rx.recv_timeout(left) else {
            break;
        };
        resolved[index] = store(servers[index], key, outcome);
    }
    resolved
}

/// 把配置里启用的 server 中还没列过的先列好（daemon 在回合开始前调，见 `turns/task.rs`）。
/// 在调用方的 runtime 里 await，不占线程：actor 等的时候照样处理别的会话。
pub async fn prefetch(config: &AppConfig) {
    if !config.mcp.enabled {
        return;
    }
    let missing = config
        .mcp
        .servers
        .iter()
        .filter(|server| server.enabled && !server.id.trim().is_empty())
        .filter(|server| cached(&ListingKey::of(server)).is_none())
        .cloned()
        .collect::<Vec<_>>();
    // 一起起、在 MCP 线程上就落缓存：调用方中途撤了（回合取消），列好的也不白列。
    let tasks = missing
        .into_iter()
        .map(|server| {
            tracing::info!(server = %server.id, "listing MCP server tools");
            runtime::handle().spawn(async move {
                let key = ListingKey::of(&server);
                let outcome = list_server(server.clone()).await;
                store(&server, key, outcome);
            })
        })
        .collect::<Vec<_>>();
    for task in tasks {
        let _ = task.await;
    }
}
