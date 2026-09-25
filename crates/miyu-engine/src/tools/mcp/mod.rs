//! MCP 客户端：把配置里的服务器的工具挂进工具面，调用时经常驻连接池（daemon）或一次性
//! 进程（单次 CLI、直连模式）。09-25 从单文件拆成目录：
//! - `protocol`：JSON-RPC 报文与结果格式；
//! - `runtime`：MCP 子进程专用的线程；
//! - `scope`：调用方的沙盒、工作目录、会话；
//! - `connection`：一个服务器进程的 stdio 连接；
//! - `listing`：tools/list 缓存与服务器说明；
//! - `pool`：常驻连接池；
//! - `instructions`：服务器说明进系统提示词。

mod connection;
mod instructions;
mod listing;
mod pool;
mod protocol;
mod runtime;
mod scope;

pub(crate) use instructions::{section as instructions_section, ServerInstructions};
pub use listing::prefetch;
pub use pool::{
    enable as enable_pool, forget_session, retire_changed, shutdown_all as shutdown_pool,
};

use super::{ToolRegistry, ToolSpec};
use anyhow::Result;
use miyu_base::config::{AppConfig, McpServerConfig};
use protocol::{mcp_tool_id, normalize_schema};
use serde_json::Value;

#[cfg(test)]
mod result_tests;
#[cfg(test)]
mod tests;

#[derive(Debug, Clone)]
struct McpToolBinding {
    server: McpServerConfig,
    tool_name: String,
}

/// `allowlist`：人格清单里 `plugins.mcp` 的服务器 id 白名单；None = 全部。在拉 tools/list
/// **之前**过滤，关掉的服务器不会被拉起。
pub fn register(registry: &mut ToolRegistry, config: AppConfig, allowlist: Option<&[String]>) {
    let servers: Vec<&McpServerConfig> = config
        .mcp
        .servers
        .iter()
        .filter(|server| server.enabled && !server.id.trim().is_empty())
        .filter(|server| allowlist.is_none_or(|list| list.iter().any(|item| item == &server.id)))
        .collect();
    let listings = listing::resolve(&servers);
    for (server, listing) in servers.into_iter().zip(listings) {
        let Some(listing) = listing else {
            continue;
        };
        let mut tool_ids = Vec::with_capacity(listing.tools.len());
        for tool in listing.tools.iter().cloned() {
            let tool_id = mcp_tool_id(&server.id, &tool.name);
            let display_name = if server.display_name.trim().is_empty() {
                format!("MCP {} / {}", server.id, tool.name)
            } else {
                format!("MCP {} / {}", server.display_name, tool.name)
            };
            let binding = McpToolBinding {
                server: server.clone(),
                tool_name: tool.name.clone(),
            };
            let description = if tool.description.trim().is_empty() {
                format!("Call MCP tool {} from server {}.", tool.name, server.id)
            } else {
                tool.description.clone()
            };
            let mut spec = ToolSpec::new(
                tool_id.clone(),
                description,
                normalize_schema(tool.input_schema),
                move |args| {
                    let binding = binding.clone();
                    async move { call_tool(binding, args).await }
                },
            )
            .with_display_name(display_name)
            .with_always_loaded(false);
            // 超时自己管：每个请求按服务器配置的秒数等（设置页最长 600 秒），注册表那道
            // 180 秒的兜底会把长调用从中间截断。
            spec.timeout_seconds = Some(0);
            registry.register(spec);
            tool_ids.push(tool_id);
        }
        if let Some(text) = &listing.instructions {
            registry.add_mcp_instructions(ServerInstructions {
                server_id: server.id.clone(),
                tool_ids,
                text: text.clone(),
            });
        }
    }
}

/// 调一件 MCP 工具：调用方的沙盒、工作目录、会话在这里取下（只在调用方的任务上看得见），
/// 带到 MCP 线程上。调用方不等了（回合取消），那边跟着撤：请求发取消通知，一次性的
/// 进程整组收掉。
async fn call_tool(binding: McpToolBinding, args: Value) -> Result<String> {
    let scope = scope::SpawnScope::capture();
    runtime::run(pool::call(binding.server, scope, binding.tool_name, args)).await
}
