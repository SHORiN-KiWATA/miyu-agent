mod api_search;
mod html_search;
mod scheduler;
use api_search::*;
use html_search::*;
use scheduler::*;

use super::{html_conversion, http_response, ToolRegistry, ToolSpec};
use anyhow::{bail, Result};
use miyu_base::config::WebPluginConfig;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};
use urlencoding::decode as url_decode;

const MAX_RESPONSE_SIZE: usize = 5 * 1024 * 1024;
const DEFAULT_FETCH_MAX_CHARS: usize = 40_000;
const MAX_FETCH_CHARS: usize = 200_000;

const CRAWLER_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/143.0.0.0 Safari/537.36";
const CRAWLER_TIMEOUT: Duration = Duration::from_secs(15);

static DDG_BLOCKED_UNTIL: Mutex<Option<Instant>> = Mutex::new(None);
static SOGOU_BLOCKED_UNTIL: Mutex<Option<Instant>> = Mutex::new(None);
static SEARCH_SCHEDULER: LazyLock<Mutex<SearchScheduler>> =
    LazyLock::new(|| Mutex::new(SearchScheduler::default()));

struct CrawlerResult {
    title: String,
    url: String,
    snippet: String,
    source: String,
}

pub fn register(registry: &mut ToolRegistry, config: WebPluginConfig) {
    register_search_tool(registry, "web_search", config.clone());
}

/// WebUI 选中文字右键「搜索」用(`web/selection_menu.rs`):同一套搜索实现、
/// 同一份提供方回退顺序,返回与 `web_search` 工具相同的文本。
pub async fn search_for_webui(query: &str, config: WebPluginConfig) -> Result<String> {
    let max_results = config.max_results.min(8);
    web_search(
        json!({ "query": query, "max_results": max_results }),
        config,
    )
    .await
}

pub fn register_fetch(registry: &mut ToolRegistry) {
    registry.register(ToolSpec::new(
        "web_fetch",
        "Fetch a URL and return markdown, text, or html. Prefer this for opening a known URL. Does not search the web.",
        json!({
            "type": "object",
            "properties": {
                "url": { "type": "string", "description": "Fully-qualified http or https URL." },
                "format": { "type": "string", "enum": ["markdown", "text", "html"], "description": "Output format. Defaults to markdown." },
                "timeout": { "type": "integer", "description": "Timeout seconds, max 120." },
                "max_chars": { "type": "integer", "description": "Maximum characters to return. Defaults to 40000, max 200000." }
            },
            "required": ["url"],
            "additionalProperties": false
        }),
        |args| async move { web_fetch(args).await },
    )
    .concurrent());
}

fn register_search_tool(registry: &mut ToolRegistry, name: &'static str, config: WebPluginConfig) {
    registry.register(ToolSpec::new(
        name,
        "Search the web. Prefer configured Tavily, Firecrawl, AnySearch, or Exa API keys; fallback to SearXNG, then Exa's keyless free quota, then built-in DuckDuckGo HTML search (with Yahoo/360/Sogou fallback) when providers fail.",
        json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Search query." },
                "max_results": { "type": "integer", "description": "Maximum results; defaults to plugins.web.max_results." },
                "provider": { "type": "string", "enum": ["auto", "tavily", "firecrawl", "anysearch", "exa", "searxng", "script"], "description": "Search provider." }
            },
            "required": ["query"],
            "additionalProperties": false
        }),
        move |args| {
            let config = config.clone();
            async move { web_search(args, config).await }
        },
    )
    .concurrent());
}

async fn web_search(args: Value, config: WebPluginConfig) -> Result<String> {
    let query = args
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    if query.is_empty() {
        bail!("query is required");
    }
    let max_results = args
        .get("max_results")
        .and_then(Value::as_u64)
        .unwrap_or(config.max_results as u64)
        .clamp(1, 10) as usize;
    let provider = args
        .get("provider")
        .and_then(Value::as_str)
        .unwrap_or("auto");
    let client = reqwest::Client::builder()
        .timeout(CRAWLER_TIMEOUT)
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()?;
    let order = search_provider_order(provider, &config)?;
    let mut errors = Vec::new();
    for item in order {
        let provider_id = item.id();
        let result = search_with_provider(&client, query, max_results, &config, item).await;
        match result {
            Ok(output) => {
                mark_provider_success(provider_id);
                return Ok(output);
            }
            Err(err) => {
                let message = err.to_string();
                mark_provider_failure(provider_id, &message);
                errors.push(format!("{provider_id}: {message}"));
            }
        }
    }
    bail!(
        "no web search provider succeeded:\n- {}",
        errors.join("\n- ")
    )
}

// ── Crawler helper functions ───────────────────────────────────

fn is_result_url_allowed(url: &str) -> bool {
    let lower = url.to_lowercase();
    let host = lower
        .strip_prefix("https://")
        .or_else(|| lower.strip_prefix("http://"))
        .unwrap_or(&lower);
    let host = host.split('/').next().unwrap_or(host);
    if host.ends_with("duckduckgo.com") {
        return false;
    }
    if host == "search.yahoo.com" || host == "r.search.yahoo.com" {
        return false;
    }
    if host.ends_with("sogou.com") && lower.contains("/link") {
        return false;
    }
    if host.ends_with("so.com") && lower.contains("/link") {
        return false;
    }
    if host == "www.googleadservices.com"
        || host == "googleads.g.doubleclick.net"
        || host == "ad.doubleclick.net"
    {
        return false;
    }
    if host.ends_with("bing.com") && (lower.contains("/aclick") || lower.contains("/alink")) {
        return false;
    }
    true
}

fn dedupe_key(url: &str) -> String {
    let lower = url.to_lowercase();
    let stripped = lower.trim_end_matches('/');
    let no_scheme = stripped
        .strip_prefix("https://")
        .or_else(|| stripped.strip_prefix("http://"))
        .unwrap_or(stripped);
    if let Some(query_pos) = no_scheme.find('?') {
        no_scheme[..query_pos].to_string()
    } else {
        no_scheme.to_string()
    }
}

fn format_crawler_results(query: &str, provider: &str, results: Vec<CrawlerResult>) -> String {
    let mut lines = vec![
        format!("## Search results for: {query}"),
        format!("**Provider**: {provider}\n"),
    ];
    for (index, r) in results.into_iter().enumerate() {
        lines.push(format!("### {}. {}", index + 1, r.title));
        lines.push(format!("**URL**: {}", r.url));
        lines.push(format!("**Source**: {}", r.source));
        let snippet = strip_nav_link_runs(&r.snippet);
        if !snippet.trim().is_empty() {
            lines.push(format!("**Snippet**: {}", clip(snippet.trim(), 400)));
        }
        lines.push(String::new());
    }
    lines.join("\n")
}

// ── DuckDuckGo HTML search ─────────────────────────────────────

// ── Yahoo HTML search ──────────────────────────────────────────

// ── 360 (so.com) HTML search ───────────────────────────────────

// ── Sogou HTML search ──────────────────────────────────────────

// ── Multi-engine fallback dispatcher ────────────────────────────

fn format_search_results(
    query: &str,
    provider: &str,
    results: Vec<Value>,
    max_results: usize,
) -> Result<String> {
    let mut lines = vec![
        format!("## Search results for: {query}"),
        format!("**Provider**: {provider}\n"),
    ];
    let mut rendered = 0usize;
    for item in results.into_iter() {
        let title = item
            .get("title")
            .or_else(|| item.pointer("/metadata/title"))
            .and_then(Value::as_str)
            .unwrap_or("Untitled");
        let url = item
            .get("url")
            .or_else(|| item.pointer("/metadata/sourceURL"))
            .or_else(|| item.pointer("/metadata/url"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let snippet = item
            .get("content")
            .or_else(|| item.get("snippet"))
            .or_else(|| item.get("description"))
            .or_else(|| item.get("summary"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let raw = item
            .get("raw_content")
            .or_else(|| item.get("markdown"))
            .or_else(|| item.get("contentMarkdown"))
            .or_else(|| item.get("text"))
            .and_then(Value::as_str)
            .unwrap_or("");
        if title == "Untitled" && url.is_empty() && snippet.is_empty() && raw.is_empty() {
            continue;
        }
        if rendered >= max_results {
            // 上限由这里兜底:provider 常常多返(实测 max_results=10 时回过
            // 11 条),之前没人再收一次口。
            break;
        }
        rendered += 1;
        lines.push(format!("### {}. {title}", rendered));
        if !url.is_empty() {
            lines.push(format!("**URL**: {url}"));
        }
        if !snippet.is_empty() {
            let snippet = strip_nav_link_runs(snippet);
            if !snippet.trim().is_empty() {
                lines.push(format!("**Snippet**: {}", clip(snippet.trim(), 500)));
            }
        }
        if !raw.is_empty() {
            lines.push(format!("**Content**: {}", clip(raw, 800)));
        }
        lines.push(String::new());
    }
    if rendered == 0 {
        bail!("{provider} returned no usable results")
    }
    Ok(lines.join("\n"))
}

fn clip(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        value.to_string()
    } else {
        format!("{}...", value.chars().take(max_chars).collect::<String>())
    }
}

async fn web_fetch(args: Value) -> Result<String> {
    let url = args
        .get("url")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    if !url.starts_with("http://") && !url.starts_with("https://") {
        bail!("URL must start with http:// or https://");
    }
    let format = args
        .get("format")
        .and_then(Value::as_str)
        .unwrap_or("markdown");
    let timeout = args
        .get("timeout")
        .and_then(Value::as_u64)
        .unwrap_or(30)
        .min(120);
    let max_chars = args
        .get("max_chars")
        .and_then(Value::as_u64)
        .map(|value| value.clamp(1, MAX_FETCH_CHARS as u64) as usize)
        .unwrap_or(DEFAULT_FETCH_MAX_CHARS);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout))
        .build()?;
    let accept = match format {
        "text" => "text/plain;q=1.0, text/markdown;q=0.9, text/html;q=0.8, */*;q=0.1",
        "html" => "text/html;q=1.0, application/xhtml+xml;q=0.9, text/plain;q=0.8, */*;q=0.1",
        _ => "text/markdown;q=1.0, text/x-markdown;q=0.9, text/plain;q=0.8, text/html;q=0.7, */*;q=0.1",
    };
    let response = client
        .get(url)
        .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/143.0.0.0 Safari/537.36")
        .header("Accept", accept)
        .header("Accept-Language", "en-US,en;q=0.9")
        .send()
        .await?
        .error_for_status()?;
    if response.content_length().unwrap_or(0) > MAX_RESPONSE_SIZE as u64 {
        bail!("response too large (exceeds 5MB limit)");
    }
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let content = http_response::read_text(response, MAX_RESPONSE_SIZE).await?;
    let output = if content_type.contains("text/html") {
        match format {
            "html" => content,
            "text" => html_conversion::to_text_async(content, 120).await?,
            _ => html_conversion::to_markdown(content).await?,
        }
    } else {
        content
    };
    Ok(clip_fetch_output(&output, max_chars))
}

fn clip_fetch_output(value: &str, max_chars: usize) -> String {
    let total = value.chars().count();
    if total <= max_chars {
        return value.to_string();
    }
    let clipped = value.chars().take(max_chars).collect::<String>();
    format!("{clipped}\n\n[content truncated from {total} chars to {max_chars} chars]")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 压缩响应必须透明解压(08-24 bilibili 乱码取证:服务器对 Chrome UA
    /// 无视 Accept-Encoding 缺席强发 gzip)。退回 reqwest 的 gzip 特性,
    /// 这里读到的是 gzip 原始字节,断言报红。
    #[tokio::test]
    async fn fetch_transparently_decompresses_gzip_bodies() {
        use std::io::Write as _;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buffer = [0u8; 4096];
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let _ = sock.read(&mut buffer).await.unwrap();
            let mut encoder =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            encoder
                .write_all("你好，这是明文正文。".as_bytes())
                .unwrap();
            let gz = encoder.finish().unwrap();
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                gz.len()
            );
            sock.write_all(head.as_bytes()).await.unwrap();
            sock.write_all(&gz).await.unwrap();
        });
        let output = web_fetch(serde_json::json!({
            "url": format!("http://{addr}/page"),
            "format": "text"
        }))
        .await
        .unwrap();
        assert!(output.contains("你好，这是明文正文。"), "got: {output:?}");
        server.await.unwrap();
    }

    #[test]
    fn clips_fetch_output_with_notice() {
        let output = clip_fetch_output("abcdef", 3);

        assert_eq!(output, "abc\n\n[content truncated from 6 chars to 3 chars]");
    }

    #[test]
    fn keeps_short_fetch_output_unchanged() {
        assert_eq!(clip_fetch_output("abc", 3), "abc");
    }

    #[test]
    fn parses_exa_public_text_blocks() {
        let text = "Title: 第一条结果\nURL: https://example.com/a\nPublished: 2025-09-28T00:00:00.000Z\nAuthor: torvalds\nHighlights:\n第一段\n第二段\n\n---\n\nTitle: 第二条\nURL: https://example.com/b\nAuthor: N/A\nHighlights:\n内容\n";
        let results = exa_public_results(text, 10);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0]["title"], "第一条结果");
        assert_eq!(results[0]["url"], "https://example.com/a");
        assert!(results[0]["snippet"]
            .as_str()
            .unwrap()
            .contains("Published: 2025-09-28"));
        assert!(results[0]["snippet"].as_str().unwrap().contains("torvalds"));
        assert!(results[0]["raw_content"]
            .as_str()
            .unwrap()
            .contains("第二段"));
        // N/A 作者不进 snippet
        assert!(!results[1]["snippet"].as_str().unwrap().contains("N/A"));

        let formatted = format_search_results("测试", "Exa (free quota)", results, 10).unwrap();
        assert!(formatted.contains("### 1. 第一条结果"));
        assert!(formatted.contains("**URL**: https://example.com/b"));
    }

    #[test]
    fn exa_joins_auto_order_without_key() {
        let config = WebPluginConfig::default();
        let order = search_provider_order("auto", &config).unwrap();
        let ids = order.iter().map(|p| p.id()).collect::<Vec<_>>();
        // 无任何配置时：免 key Exa 优先，爬虫兜底
        assert_eq!(ids, vec!["exa", "duckduckgo"]);
    }

    #[test]
    fn exa_with_key_is_a_primary_provider() {
        let config = WebPluginConfig {
            exa_api_keys: vec!["k".to_string()],
            ..WebPluginConfig::default()
        };
        let providers = configured_primary_providers(&config);
        assert!(providers.iter().any(|p| matches!(p, SearchProvider::Exa)));
        assert!(search_provider_order("exa", &config).is_ok());
    }

    /// 真实网络实测：cargo test --bin miyu -- --ignored exa_free_quota
    #[tokio::test]
    #[ignore = "hits the real Exa MCP endpoint"]
    async fn exa_free_quota_live_search() {
        let client = reqwest::Client::builder()
            .timeout(CRAWLER_TIMEOUT)
            .build()
            .unwrap();
        let output = search_exa_public(&client, "Arch Linux kernel release", 2)
            .await
            .unwrap();
        assert!(output.contains("**Provider**: Exa (free quota)"));
        assert!(output.contains("**URL**:"));
    }
}
