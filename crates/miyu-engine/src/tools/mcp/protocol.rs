//! MCP 的 JSON-RPC 报文：请求怎么拼、服务器发来的一行是什么、工具结果怎么变成给模型的字。

use serde::Deserialize;
use serde_json::{json, Value};

pub(super) const JSONRPC_VERSION: &str = "2.0";

/// 握手时报的协议版本。
pub(super) const PROTOCOL_VERSION: &str = "2025-03-26";

/// 方法不存在（JSON-RPC 标准码）：服务器反过来问我们不支持的东西时回它。
const METHOD_NOT_FOUND: i64 = -32601;

#[derive(Debug, Deserialize)]
pub(super) struct JsonRpcError {
    pub(super) code: i64,
    pub(super) message: String,
    #[serde(default)]
    pub(super) data: Option<Value>,
}

impl JsonRpcError {
    pub(super) fn describe(&self) -> String {
        format!(
            "MCP error {}: {}{}",
            self.code,
            self.message,
            format_error_data(&self.data)
        )
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct ToolsListResult {
    #[serde(default)]
    pub(super) tools: Vec<McpToolInfo>,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct McpToolInfo {
    pub(super) name: String,
    #[serde(default)]
    pub(super) description: String,
    #[serde(default, rename = "inputSchema")]
    pub(super) input_schema: Value,
}

/// 握手回话里我们要的那一件：服务器写给模型看的使用说明（可选）。
#[derive(Debug, Default, Deserialize)]
pub(super) struct InitializeResult {
    #[serde(default)]
    pub(super) instructions: Option<String>,
}

/// 服务器发来的一行。
#[derive(Debug)]
pub(super) enum Incoming {
    /// 回我们的请求。
    Response {
        id: u64,
        outcome: Result<Value, JsonRpcError>,
    },
    /// 服务器反过来问我们（ping 之类）。
    Request { id: Value, method: String },
    /// 通知，不用回。
    Notification,
}

/// 认一行。不是 JSON、不是 JSON-RPC 的（有的服务器往 stdout 打日志）返回 None。
pub(super) fn classify(line: &str) -> Option<Incoming> {
    let value: Value = serde_json::from_str(line.trim()).ok()?;
    let method = value.get("method").and_then(Value::as_str);
    match (value.get("id"), method) {
        (Some(id), Some(method)) => Some(Incoming::Request {
            id: id.clone(),
            method: method.to_string(),
        }),
        (None, Some(_)) => Some(Incoming::Notification),
        (Some(id), None) => {
            let id = id.as_u64()?;
            let outcome = match value.get("error") {
                Some(error) => Err(serde_json::from_value(error.clone()).ok()?),
                None => Ok(value.get("result").cloned().unwrap_or(Value::Null)),
            };
            Some(Incoming::Response { id, outcome })
        }
        (None, None) => None,
    }
}

pub(super) fn request_line(id: u64, method: &str, params: Value) -> String {
    json!({"jsonrpc": JSONRPC_VERSION, "id": id, "method": method, "params": params}).to_string()
}

pub(super) fn notification_line(method: &str, params: Value) -> String {
    json!({"jsonrpc": JSONRPC_VERSION, "method": method, "params": params}).to_string()
}

/// 回服务器反过来问的：ping 回空结果，别的一律「方法不存在」。
pub(super) fn answer_line(id: &Value, method: &str) -> String {
    if method == "ping" {
        return json!({"jsonrpc": JSONRPC_VERSION, "id": id, "result": {}}).to_string();
    }
    json!({
        "jsonrpc": JSONRPC_VERSION,
        "id": id,
        "error": {"code": METHOD_NOT_FOUND, "message": format!("method not supported: {method}")},
    })
    .to_string()
}

pub(super) fn initialize_params() -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": {},
        "clientInfo": {"name": "miyu", "version": env!("CARGO_PKG_VERSION")},
    })
}

pub(super) fn mcp_tool_id(server_id: &str, tool_name: &str) -> String {
    format!("mcp_{}_{}", sanitize_id(server_id), sanitize_id(tool_name))
}

pub(super) fn sanitize_id(value: &str) -> String {
    let mut out = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect::<String>();
    while out.contains("__") {
        out = out.replace("__", "_");
    }
    out.trim_matches('_').to_string()
}

pub(super) fn normalize_schema(schema: Value) -> Value {
    if schema.is_object() {
        schema
    } else {
        json!({"type":"object","properties":{},"additionalProperties":true})
    }
}

pub(super) fn format_mcp_result(result: &Value) -> String {
    let output = format_mcp_content(result);
    if result.get("isError").and_then(Value::as_bool) == Some(true) {
        // A successful JSON-RPC exchange can contain a failed tool call.
        // Keep its diagnostic content inside the existing tool failure format.
        json!({"ok": false, "error": output}).to_string()
    } else {
        output
    }
}

fn format_mcp_content(result: &Value) -> String {
    if let Some(content) = result.get("content").and_then(Value::as_array) {
        let parts = content
            .iter()
            .filter_map(format_content_part)
            .collect::<Vec<_>>();
        if !parts.is_empty() {
            return parts.join("\n\n");
        }
    }
    serde_json::to_string_pretty(result).unwrap_or_else(|_| result.to_string())
}

fn format_content_part(value: &Value) -> Option<String> {
    match value.get("type").and_then(Value::as_str) {
        Some("text") => value
            .get("text")
            .and_then(Value::as_str)
            .map(str::to_string),
        Some(kind) => {
            Some(serde_json::to_string_pretty(value).unwrap_or_else(|_| kind.to_string()))
        }
        None => Some(serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())),
    }
}

fn format_error_data(data: &Option<Value>) -> String {
    data.as_ref()
        .map(|data| format!(": {}", data))
        .unwrap_or_default()
}

/// 在一次工具结果前面挂一句说明（服务器重启过、这一次没复用进程……）。失败的结果是
/// `{"ok":false,…}` 这种 JSON，成败判定只认它：往 JSON 里加一个字段，不往前面拼字。
pub(super) fn with_notice(output: String, notice: &str) -> String {
    if let Ok(Value::Object(mut map)) = serde_json::from_str::<Value>(&output) {
        map.insert("notice".to_string(), Value::String(notice.to_string()));
        return Value::Object(map).to_string();
    }
    format!("[{notice}]\n{output}")
}
