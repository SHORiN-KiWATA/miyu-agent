//! 各家图片搜索引擎的抓取与解析。
//!
//! 每家一个 `search_*`，输出统一的 `ImageCandidate`。引擎会挂、会改页面、会弹
//! 验证码（`looks_like_search_challenge`），所以**一家失败就换下一家**：
//! `mark_provider_failure` 给失败的引擎记冷却，别每次都从同一家开始试。

use crate::tools::web_images::*;

pub(in crate::tools::web_images) const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/143.0.0.0 Safari/537.36";

pub(in crate::tools::web_images) const MAX_SEARCH_RESPONSE_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Copy)]
pub(in crate::tools::web_images) enum ImageSearchProvider {
    SearXng,
    DuckDuckGo,
    BingCn,
    Baidu,
    So360,
}

impl ImageSearchProvider {
    pub(in crate::tools::web_images) fn id(self) -> &'static str {
        match self {
            Self::SearXng => "searxng",
            Self::DuckDuckGo => "duckduckgo",
            Self::BingCn => "bing_cn",
            Self::Baidu => "baidu",
            Self::So360 => "so360",
        }
    }
}

pub(in crate::tools::web_images) fn image_search_providers(
    config: &AppConfig,
    query: &str,
    safe_search: bool,
) -> Vec<ImageSearchProvider> {
    let mode = config.plugins.web_images.source_mode.trim();
    // 百度与 360 没有安全搜索参数,只在用户关掉安全搜索时才用。
    let allow_best_effort_domestic = !safe_search;
    let mut providers = Vec::new();
    if !config.plugins.web.searxng_base_url.trim().is_empty() {
        providers.push(ImageSearchProvider::SearXng);
    }
    match mode {
        "mainland" => {
            providers.push(ImageSearchProvider::BingCn);
            if allow_best_effort_domestic {
                providers.extend([ImageSearchProvider::Baidu, ImageSearchProvider::So360]);
            }
        }
        "global" => {
            providers.extend([ImageSearchProvider::DuckDuckGo, ImageSearchProvider::BingCn])
        }
        _ if query.chars().any(is_cjk) => {
            providers.extend([ImageSearchProvider::DuckDuckGo, ImageSearchProvider::BingCn]);
            if allow_best_effort_domestic {
                providers.extend([ImageSearchProvider::Baidu, ImageSearchProvider::So360]);
            }
        }
        _ => providers.extend([ImageSearchProvider::DuckDuckGo, ImageSearchProvider::BingCn]),
    }
    providers
}

pub(in crate::tools::web_images) async fn search_with_provider(
    client: &Client,
    provider: ImageSearchProvider,
    searxng_base_url: &str,
    query: &str,
    limit: usize,
    safe_search: bool,
) -> Result<Vec<ImageCandidate>> {
    match provider {
        ImageSearchProvider::SearXng => {
            search_searxng_images(client, searxng_base_url, query, limit, safe_search).await
        }
        ImageSearchProvider::DuckDuckGo => {
            search_ddg_images(client, query, limit, safe_search).await
        }
        ImageSearchProvider::BingCn => search_bing_images(client, query, limit, safe_search).await,
        ImageSearchProvider::Baidu => search_baidu_images(client, query, limit).await,
        ImageSearchProvider::So360 => search_so360_images(client, query, limit).await,
    }
}

pub(in crate::tools::web_images) fn provider_ready(provider: &ImageSearchProvider) -> bool {
    let Ok(mut cooldowns) = PROVIDER_COOLDOWNS.lock() else {
        return true;
    };
    match cooldowns.get(provider.id()).copied() {
        Some(until) if until > Instant::now() => false,
        Some(_) => {
            cooldowns.remove(provider.id());
            true
        }
        None => true,
    }
}

pub(in crate::tools::web_images) fn provider_probe_candidate(
    providers: &[ImageSearchProvider],
) -> Option<ImageSearchProvider> {
    let cooldowns = PROVIDER_COOLDOWNS.lock().ok()?;
    providers.iter().copied().min_by_key(|provider| {
        cooldowns
            .get(provider.id())
            .copied()
            .unwrap_or(Instant::now())
    })
}

pub(in crate::tools::web_images) fn mark_provider_success(provider: ImageSearchProvider) {
    if let Ok(mut cooldowns) = PROVIDER_COOLDOWNS.lock() {
        cooldowns.remove(provider.id());
    }
}

pub(in crate::tools::web_images) fn mark_provider_failure(
    provider: ImageSearchProvider,
    error: &str,
) {
    let lower = error.to_ascii_lowercase();
    let duration = if lower.contains("403")
        || lower.contains("429")
        || lower.contains("forbid")
        || lower.contains("anti-bot")
        || lower.contains("captcha")
        || lower.contains("challenge")
    {
        Some(Duration::from_secs(600))
    } else if lower.contains("timeout")
        || lower.contains("timed out")
        || lower.contains("connection")
        || lower.contains("dns")
        || lower.contains("http 5")
    {
        Some(Duration::from_secs(120))
    } else {
        None
    };
    if let (Some(duration), Ok(mut cooldowns)) = (duration, PROVIDER_COOLDOWNS.lock()) {
        cooldowns.insert(provider.id(), Instant::now() + duration);
    }
}

pub(in crate::tools::web_images) async fn response_bytes_limited(
    response: reqwest::Response,
) -> Result<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_SEARCH_RESPONSE_BYTES as u64)
    {
        bail!("image search response exceeds the 8 MiB limit")
    }
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if bytes.len().saturating_add(chunk.len()) > MAX_SEARCH_RESPONSE_BYTES {
            bail!("image search response exceeds the 8 MiB limit")
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

pub(in crate::tools::web_images) async fn response_json_limited(
    response: reqwest::Response,
) -> Result<Value> {
    Ok(serde_json::from_slice(
        &response_bytes_limited(response).await?,
    )?)
}

pub(in crate::tools::web_images) async fn response_text_limited(
    response: reqwest::Response,
) -> Result<String> {
    Ok(String::from_utf8_lossy(&response_bytes_limited(response).await?).into_owned())
}

pub(in crate::tools::web_images) async fn search_searxng_images(
    client: &Client,
    base_url: &str,
    query: &str,
    limit: usize,
    safe_search: bool,
) -> Result<Vec<ImageCandidate>> {
    let base_url = base_url.trim().trim_end_matches('/');
    if base_url.is_empty() {
        bail!("missing SearXNG base URL")
    }
    let response = client
        .get(format!("{base_url}/search"))
        .query(&[
            ("q", query),
            ("categories", "images"),
            ("format", "json"),
            ("language", "auto"),
            ("safesearch", if safe_search { "2" } else { "0" }),
        ])
        .headers(image_headers(base_url))
        .send()
        .await?
        .error_for_status()?;
    let data = response_json_limited(response).await?;
    let mut candidates = Vec::new();
    for item in data
        .get("results")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(limit)
    {
        let (width, height) = parse_resolution(
            item.get("resolution")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        );
        if let Some(candidate) = build_candidate(
            item.get("title")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            item.get("url").and_then(Value::as_str).unwrap_or_default(),
            item.get("img_src")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            item.get("thumbnail_src")
                .or_else(|| item.get("thumbnail"))
                .and_then(Value::as_str)
                .unwrap_or_default(),
            "SearXNG Images",
            width as u64,
            height as u64,
            item.get("content")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        ) {
            candidates.push(candidate);
        }
    }
    if candidates.is_empty() {
        bail!("SearXNG returned no image results")
    }
    Ok(candidates)
}

pub(in crate::tools::web_images) async fn search_baidu_images(
    client: &Client,
    query: &str,
    limit: usize,
) -> Result<Vec<ImageCandidate>> {
    let response = client
        .get("https://image.baidu.com/search/acjson")
        .query(&[
            ("tn", "resultjson_com"),
            ("ipn", "rj"),
            ("ct", "201326592"),
            ("fp", "result"),
            ("word", query),
            ("queryWord", query),
            ("cl", "2"),
            ("lm", "-1"),
            ("ie", "utf-8"),
            ("oe", "utf-8"),
            ("st", "-1"),
            ("face", "0"),
            ("istype", "2"),
            ("nc", "1"),
            ("pn", "0"),
            ("rn", &limit.min(60).to_string()),
        ])
        .headers(image_headers("https://image.baidu.com/"))
        .send()
        .await?
        .error_for_status()?;
    let data = response_json_limited(response).await?;
    if data.get("antiFlag").is_some() {
        bail!("Baidu Images anti-bot response")
    }
    let mut candidates = Vec::new();
    for item in data
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(limit)
    {
        let replacement = item
            .get("replaceUrl")
            .and_then(Value::as_array)
            .and_then(|items| items.first());
        let image_url = replacement
            .and_then(|value| value.get("ObjURL").or_else(|| value.get("ObjUrl")))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .or_else(|| item.get("middleURL").and_then(Value::as_str))
            .unwrap_or_default();
        let page_url = replacement
            .and_then(|value| value.get("FromURL").or_else(|| value.get("FromUrl")))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .or_else(|| item.get("fromJumpUrl").and_then(Value::as_str))
            .unwrap_or_default();
        if let Some(candidate) = build_candidate(
            item.get("fromPageTitleEnc")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            page_url,
            image_url,
            item.get("thumbURL")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            "Baidu Images",
            item.get("width").and_then(Value::as_u64).unwrap_or(0),
            item.get("height").and_then(Value::as_u64).unwrap_or(0),
            "",
        ) {
            candidates.push(candidate);
        }
    }
    if candidates.is_empty() {
        bail!("Baidu Images returned no results")
    }
    Ok(candidates)
}

pub(in crate::tools::web_images) async fn search_so360_images(
    client: &Client,
    query: &str,
    limit: usize,
) -> Result<Vec<ImageCandidate>> {
    let response = client
        .get("https://image.so.com/j")
        .query(&[
            ("q", query),
            ("src", "srp"),
            ("sn", "0"),
            ("pn", &limit.min(60).to_string()),
        ])
        .headers(image_headers("https://image.so.com/"))
        .send()
        .await?
        .error_for_status()?;
    let data = response_json_limited(response).await?;
    let mut candidates = Vec::new();
    for item in data
        .get("list")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(limit)
    {
        if let Some(candidate) = build_candidate(
            item.get("title")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            item.get("link").and_then(Value::as_str).unwrap_or_default(),
            item.get("img").and_then(Value::as_str).unwrap_or_default(),
            item.get("thumb")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            "360 Images",
            parse_u64ish(item.get("width")),
            parse_u64ish(item.get("height")),
            item.get("dspurl")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        ) {
            candidates.push(candidate);
        }
    }
    if candidates.is_empty() {
        bail!("360 Images returned no results")
    }
    Ok(candidates)
}

pub(in crate::tools::web_images) fn parse_u64ish(value: Option<&Value>) -> u64 {
    value
        .and_then(|value| {
            value
                .as_u64()
                .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
        })
        .unwrap_or(0)
}

pub(in crate::tools::web_images) fn parse_resolution(value: &str) -> (u32, u32) {
    let values = value
        .split(|ch: char| !ch.is_ascii_digit())
        .filter(|value| !value.is_empty())
        .filter_map(|value| value.parse::<u32>().ok())
        .take(2)
        .collect::<Vec<_>>();
    match values.as_slice() {
        [width, height] => (*width, *height),
        _ => (0, 0),
    }
}

pub(in crate::tools::web_images) async fn search_ddg_images(
    client: &Client,
    query: &str,
    limit: usize,
    safe_search: bool,
) -> Result<Vec<ImageCandidate>> {
    let page_url = format!(
        "https://duckduckgo.com/?q={}&iax=images&ia=images",
        urlencoding::encode(query)
    );
    let page_response = client
        .get("https://duckduckgo.com/")
        .query(&[("q", query), ("iax", "images"), ("ia", "images")])
        .headers(image_headers(""))
        .send()
        .await?;
    let page_status = page_response.status();
    let html = response_text_limited(page_response).await?;
    if page_status.as_u16() != 200 || looks_like_search_challenge(&html) {
        bail!("DuckDuckGo image challenge or HTTP {page_status}")
    }
    let vqd = extract_ddg_vqd(&html).context("DuckDuckGo image page did not return vqd")?;
    let api_response = client
        .get("https://duckduckgo.com/i.js")
        .query(&[
            ("q", query),
            ("o", "json"),
            ("p", if safe_search { "1" } else { "-1" }),
            ("s", "0"),
            ("u", "bing"),
            ("f", ",,,"),
            (
                "l",
                if query.chars().any(is_cjk) {
                    "cn-zh"
                } else {
                    "wt-wt"
                },
            ),
            ("vqd", vqd.as_str()),
        ])
        .headers(image_headers(&page_url))
        .send()
        .await?;
    let api_status = api_response.status();
    let response = response_text_limited(api_response).await?;
    if api_status.as_u16() != 200 || looks_like_search_challenge(&response) {
        bail!("DuckDuckGo image API challenge or HTTP {api_status}")
    }
    parse_ddg_results(&response, limit)
}

pub(in crate::tools::web_images) fn extract_ddg_vqd(html: &str) -> Option<String> {
    for marker in ["vqd=\"", "vqd='", "vqd:\"", "vqd: '"] {
        if let Some(start) = html.find(marker) {
            let rest = &html[start + marker.len()..];
            let value: String = rest
                .chars()
                .take_while(|ch| ch.is_ascii_digit() || *ch == '-')
                .collect();
            if !value.is_empty() {
                return Some(value);
            }
        }
    }
    if let Some(start) = html.find("\"vqd\":\"") {
        let rest = &html[start + "\"vqd\":\"".len()..];
        let value: String = rest
            .chars()
            .take_while(|ch| ch.is_ascii_digit() || *ch == '-')
            .collect();
        if !value.is_empty() {
            return Some(value);
        }
    }
    None
}

pub(in crate::tools::web_images) fn parse_ddg_results(
    text: &str,
    limit: usize,
) -> Result<Vec<ImageCandidate>> {
    let data: Value = serde_json::from_str(text)?;
    let results = data
        .get("results")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut candidates = Vec::new();
    for item in results.into_iter().take(limit) {
        if let Some(candidate) = build_candidate(
            item.get("title")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            item.get("url").and_then(Value::as_str).unwrap_or_default(),
            item.get("image")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            item.get("thumbnail")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            "DuckDuckGo Images",
            item.get("width").and_then(Value::as_u64).unwrap_or(0),
            item.get("height").and_then(Value::as_u64).unwrap_or(0),
            "",
        ) {
            candidates.push(candidate);
        }
    }
    Ok(candidates)
}

pub(in crate::tools::web_images) async fn search_bing_images(
    client: &Client,
    query: &str,
    limit: usize,
    safe_search: bool,
) -> Result<Vec<ImageCandidate>> {
    let mut request = client
        .get("https://cn.bing.com/images/search")
        .query(&[("q", query), ("first", "1"), ("mkt", "zh-CN")])
        .headers(image_headers(""));
    if safe_search {
        request = request.query(&[("safeSearch", "Strict")]);
    }
    let html = response_text_limited(request.send().await?.error_for_status()?).await?;
    let candidates = parse_bing_results(&html, limit);
    if candidates.is_empty() {
        bail!("Bing CN Images returned no parseable results")
    }
    Ok(candidates)
}

pub(in crate::tools::web_images) fn parse_bing_results(
    html: &str,
    limit: usize,
) -> Vec<ImageCandidate> {
    let mut candidates = Vec::new();
    let mut rest = html;
    while let Some(pos) = rest.find("<a") {
        rest = &rest[pos..];
        let Some(iusc_pos) = rest.find("class=\"iusc\"") else {
            if rest.len() <= 2 {
                break;
            }
            rest = &rest[2..];
            continue;
        };
        rest = &rest[iusc_pos..];
        let Some(m_pos) = rest.find("m=\"") else {
            rest = &rest[1..];
            continue;
        };
        let start = m_pos + 3;
        let Some(end) = rest[start..].find('"') else {
            break;
        };
        let raw = html_unescape(&rest[start..start + end]);
        if let Ok(data) = serde_json::from_str::<Value>(&raw) {
            if let Some(candidate) = build_candidate(
                data.get("t")
                    .or_else(|| data.get("desc"))
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
                data.get("purl").and_then(Value::as_str).unwrap_or_default(),
                data.get("murl").and_then(Value::as_str).unwrap_or_default(),
                data.get("turl").and_then(Value::as_str).unwrap_or_default(),
                "Bing CN Images",
                data.get("w")
                    .or_else(|| data.get("expw"))
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                data.get("h")
                    .or_else(|| data.get("exph"))
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                data.get("desc").and_then(Value::as_str).unwrap_or_default(),
            ) {
                candidates.push(candidate);
            }
        }
        if candidates.len() >= limit {
            break;
        }
        rest = &rest[start + end..];
    }
    candidates
}

pub(in crate::tools::web_images) fn build_candidate(
    title: &str,
    page_url: &str,
    image_url: &str,
    thumbnail_url: &str,
    source: &str,
    width: u64,
    height: u64,
    extra_description: &str,
) -> Option<ImageCandidate> {
    let image_url = clean_url(image_url);
    if !image_url.starts_with("http://") && !image_url.starts_with("https://") {
        return None;
    }
    let title = clean_text(title, 180);
    let page_url = clean_url(page_url);
    let thumbnail_url = clean_url(thumbnail_url);
    let mut description_parts = vec![title.clone(), clean_text(extra_description, 180)];
    if let Some(host) = host_from_url(&page_url) {
        description_parts.push(format!("来源页面: {host}"));
    }
    let search_description = clean_text(
        &description_parts
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("；"),
        420,
    );
    Some(ImageCandidate {
        title,
        page_url,
        image_url,
        thumbnail_url,
        source: source.to_string(),
        width: width.min(u32::MAX as u64) as u32,
        height: height.min(u32::MAX as u64) as u32,
        search_description,
        provider_rank: 0,
    })
}

pub(in crate::tools::web_images) fn looks_like_search_challenge(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("anomaly-modal")
        || lower.contains("captcha")
        || lower.contains("challenge-form")
        || lower.contains("robot check")
}

pub(in crate::tools::web_images) fn clean_url(value: &str) -> String {
    html_unescape(value.trim())
}

pub(in crate::tools::web_images) fn clean_text(value: &str, max_chars: usize) -> String {
    let text = html_unescape(value)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if text.chars().count() <= max_chars {
        text
    } else {
        format!("{}...", text.chars().take(max_chars).collect::<String>())
    }
}

pub(in crate::tools::web_images) fn html_unescape(value: &str) -> String {
    value
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
}

pub(in crate::tools::web_images) fn host_from_url(url: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    Some(rest.split('/').next()?.to_ascii_lowercase())
}
