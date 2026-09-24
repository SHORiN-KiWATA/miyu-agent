//! 失败的分类。
//!
//! 「这次失败该不该重试、该不该换端点」全靠这里的分类。分成两层：传输层
//! （连不上、超时）和 HTTP 层（状态码 + 响应体）。
//!
//! `classify_provider_error_body` 存在的原因是**供应商的状态码经常不可信**：
//! 配额耗尽返回 200 带一个 error 字段、参数错误返回 500 的都有。所以除了看状
//! 态码，还要读响应体里的信号。这也是「报错信息是嫌疑人」在代码里的样子。

use crate::llm::openai_compatible::*;

pub(in crate::llm::openai_compatible) const TRANSPORT_RETRY_DELAY: Duration =
    Duration::from_millis(250);

pub(in crate::llm::openai_compatible) const MAX_SEND_ATTEMPTS: usize = 3;

/// Attempts a request gets before giving up, however few endpoints exist. With
/// several endpoints these are failovers; with one they are plain retries.
pub(in crate::llm::openai_compatible) const MIN_ENDPOINT_ATTEMPTS: usize = 3;

#[cfg(not(any(test, feature = "testkit")))]
pub(in crate::llm::openai_compatible) const HTTP_STATUS_RETRY_INITIAL_DELAY: Duration =
    Duration::from_secs(2);

#[cfg(not(any(test, feature = "testkit")))]
pub(in crate::llm::openai_compatible) const HTTP_STATUS_RETRY_MAX_DELAY: Duration =
    Duration::from_secs(120);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::llm::openai_compatible) enum TransportFailureKind {
    Connect,
    Timeout,
    Other,
}

impl std::fmt::Display for TransportFailureKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Connect => "connect",
            Self::Timeout => "timeout",
            Self::Other => "request",
        })
    }
}

pub(in crate::llm::openai_compatible) fn retryable_transport_failure(
    kind: TransportFailureKind,
) -> bool {
    kind == TransportFailureKind::Connect
}

pub(in crate::llm::openai_compatible) fn retryable_http_status(status: u16) -> bool {
    (500..=599).contains(&status)
}

pub(in crate::llm::openai_compatible) fn http_status_retry_delay(attempt: usize) -> Duration {
    HTTP_STATUS_RETRY_INITIAL_DELAY
        .saturating_mul(1 << attempt.saturating_sub(1).min(6))
        .min(HTTP_STATUS_RETRY_MAX_DELAY)
}

#[derive(Debug)]
pub(in crate::llm::openai_compatible) struct TransportFailure {
    pub(in crate::llm::openai_compatible) stage: &'static str,
    pub(in crate::llm::openai_compatible) kind: TransportFailureKind,
}

impl std::fmt::Display for TransportFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} transport failed ({})", self.stage, self.kind)
    }
}

impl std::error::Error for TransportFailure {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::llm::openai_compatible) enum HttpFailureKind {
    Status,
    Authentication,
    RateLimit,
    EndpointUnavailable,
    EndpointIncompatible,
    InvalidRequest,
    /// 供应商的内容策略拦下了这条提示词(Google「Prohibited Use policy」
    /// 那种):是对这条内容的裁定,不是端点故障——不冷却、可切别的端点、
    /// 同端点重打必然再撞。
    ContentPolicy,
    /// 对话超出了这个模型的上下文窗口。同端点重打必然还是超长(09-24 B6:
    /// 原来归「端点不兼容」,单端点会被补齐白打三次才轮到被动压缩);换一个
    /// 窗口更大的端点可能答得了,所以照样可以换端点。不冷却:端点没坏。
    ContextOverflow,
    /// 余额或额度用完(402、insufficient_quota 一类)。是对这个账号的裁定:
    /// 同端点不重打、长冷却;换一家就能答,所以照样换端点(09-24 B6:402 的
    /// 报文常把 code 写成 invalid_request_error,原来因此连换端点都不试)。
    Quota,
}

impl std::fmt::Display for HttpFailureKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Status => "status",
            Self::Authentication => "authentication",
            Self::RateLimit => "rate_limit",
            Self::EndpointUnavailable => "endpoint_unavailable",
            Self::EndpointIncompatible => "endpoint_incompatible",
            Self::InvalidRequest => "invalid_request",
            Self::ContentPolicy => "content_policy",
            Self::ContextOverflow => "context_overflow",
            Self::Quota => "quota",
        })
    }
}

impl HttpFailureKind {
    /// 给人看的那句：出了什么事。
    ///
    /// 分类本来只喂调度器（冷却多久、换不换端点），一个字都没到过用户眼前——
    /// 429 和别的状态码打出来长得一模一样，谁也看不出是额度问题（BUG-16）。
    pub(in crate::llm::openai_compatible) fn label(self, relay: bool) -> &'static str {
        match self {
            Self::RateLimit if relay => t("usage limit reached", "额度用完或被限流"),
            Self::RateLimit => t("rate limited or out of quota", "被限流或额度用完"),
            Self::Authentication if relay => t("not signed in", "没登录或登录态失效"),
            Self::Authentication => t("authentication failed", "认证失败"),
            Self::EndpointUnavailable => t(
                "the endpoint does not serve this model",
                "这个端点没有这个模型",
            ),
            Self::EndpointIncompatible => t(
                "the endpoint does not accept this request shape",
                "这个端点不认这种请求",
            ),
            Self::InvalidRequest => t("the request was rejected", "请求被拒"),
            Self::ContentPolicy => t(
                "the provider's content policy blocked this prompt",
                "供应商的内容策略拦下了这条提示词",
            ),
            Self::ContextOverflow => t(
                "the conversation is longer than the model's context window",
                "对话超出了模型的上下文窗口",
            ),
            Self::Quota => t("out of balance or quota", "余额或额度用完"),
            Self::Status => t("the provider returned an error", "供应商报错"),
        }
    }
}

#[derive(Debug)]
pub(in crate::llm::openai_compatible) struct HttpStatusFailure {
    pub(in crate::llm::openai_compatible) status: u16,
    pub(in crate::llm::openai_compatible) kind: HttpFailureKind,
    /// 这个状态码是**编的**吗。
    ///
    /// CLI 中转线（codex / claude-code / antigravity）根本不发 HTTP 请求，是拉起
    /// 子进程读 stdout，再按措辞翻成状态码。翻完还打印「HTTP 429」的话，人会
    /// 照着网络问题去查 base_url 和代理，而真正要做的是等订阅额度刷新
    /// （BUG-16）。标上它，文本里就不提那个数字。
    pub(in crate::llm::openai_compatible) relay: bool,
    /// 供应商自己那句人话（报文里的 `error.message`），已经裁短。
    ///
    /// 原来给用户的那行是把整条错误链倒出来，里头是一整坨原始 JSON；而真正有用
    /// 的只有这一句（「Please try again in 1.024s」这种）。留下它，别的丢掉。
    pub(in crate::llm::openai_compatible) detail: Option<String>,
    /// 服务端在 `retry-after-ms` / `retry-after` 头里说的「多久之后再来」。
    pub(in crate::llm::openai_compatible) retry_after: Option<Duration>,
}

impl HttpStatusFailure {
    /// 子进程输出的措辞翻出来的失败：没有真的 HTTP 请求，别提状态码。
    pub(in crate::llm::openai_compatible) fn relay(status: u16, kind: HttpFailureKind) -> Self {
        Self {
            status,
            kind,
            relay: true,
            detail: None,
            retry_after: None,
        }
    }

    /// 带上响应头里的 Retry-After。只有真 HTTP 响应才有头，所以单独挂。
    pub(in crate::llm::openai_compatible) fn with_retry_after(
        mut self,
        retry_after: Option<Duration>,
    ) -> Self {
        self.retry_after = retry_after;
        self
    }

    pub(in crate::llm::openai_compatible) fn classify(status: u16, body: &str) -> Self {
        let kind = match status {
            401 | 403 => HttpFailureKind::Authentication,
            402 => HttpFailureKind::Quota,
            429 if quota_exhausted(body) => HttpFailureKind::Quota,
            429 => HttpFailureKind::RateLimit,
            // 404 从 LLM 端点回来只有一个意思:这儿没有这个模型/这条路径。
            // 一定是端点自己的问题,换一个端点有意义——不该按「服务端一时
            // 抖动」给冷却。不靠报文措辞判,因为措辞五花八门(08-18 实测:
            // LM Studio 回的是 `not_found_error` + "Model 'X' not found",
            // 模型名夹在中间,拼不出 `model_not_found` 这个关键词)。
            404 => HttpFailureKind::EndpointUnavailable,
            408 | 500..=599 => HttpFailureKind::Status,
            // 超长的措辞各家不一，全文匹配复用被动压缩那一份判据
            // (`is_context_overflow_message`,带限流排除)，两边永远认同一批。
            400..=499 if crate::llm::is_context_overflow_message(body) => {
                HttpFailureKind::ContextOverflow
            }
            _ => classify_provider_error_body(body).unwrap_or(HttpFailureKind::Status),
        };
        Self {
            status,
            kind,
            relay: false,
            detail: provider_message(body),
            retry_after: None,
        }
    }
}

/// 余额 / 额度用完的报文信号(与「一时限流」区分:额度用完等多久都没用)。
const QUOTA_SIGNALS: &[&str] = &[
    "insufficient_quota",
    "insufficient_balance",
    "exceeded_current_quota",
    "billing_hard_limit",
    "payment_required",
    "insufficient_credit",
    "out_of_credit",
];

fn quota_exhausted(body: &str) -> bool {
    let signal = normalize_error_signal(body);
    contains_any(&signal, QUOTA_SIGNALS)
}

/// 服务端让等多久以内，就在原地等完、同一端点再试一次（用户 09-24 拍板 20 秒）。
/// 更长的等待交给冷却：一轮对话不该卡在那里干等。
pub(in crate::llm::openai_compatible) const HONORED_RETRY_AFTER_MAX: Duration =
    Duration::from_secs(20);

/// 读 `retry-after-ms`(毫秒,OpenAI / Anthropic 网关)与 `retry-after`
/// (秒数或 HTTP 日期,RFC 9110)。读不出来就是 None。
pub(in crate::llm::openai_compatible) fn parse_retry_after(
    headers: &reqwest::header::HeaderMap,
) -> Option<Duration> {
    let header = |name: &str| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
    };
    if let Some(millis) = header("retry-after-ms").and_then(|value| value.parse::<f64>().ok()) {
        if millis.is_finite() && millis >= 0.0 {
            return Some(Duration::from_secs_f64(millis / 1000.0));
        }
    }
    let value = header("retry-after")?;
    if let Ok(seconds) = value.parse::<f64>() {
        return (seconds.is_finite() && seconds >= 0.0).then(|| Duration::from_secs_f64(seconds));
    }
    let at = chrono::DateTime::parse_from_rfc2822(value).ok()?;
    let wait = at.with_timezone(&chrono::Utc) - chrono::Utc::now();
    Some(wait.to_std().unwrap_or(Duration::ZERO))
}

/// 这次失败要不要原地等一会儿、再打同一个端点：只认服务端明说的短等待。
/// 额度用完不算——那不是等一等就会好的事。
pub(in crate::llm::openai_compatible) fn retry_after_to_honor(
    error: &anyhow::Error,
) -> Option<Duration> {
    let failure = error.downcast_ref::<HttpStatusFailure>()?;
    if failure.kind != HttpFailureKind::RateLimit {
        return None;
    }
    failure
        .retry_after
        .filter(|wait| *wait <= HONORED_RETRY_AFTER_MAX)
}

/// 报文里供应商自己那句话。JSON 认 `error.message` 与顶层 `message`；不是 JSON
/// 的就整段当那句话。
pub(in crate::llm::openai_compatible) fn provider_message(body: &str) -> Option<String> {
    let structured = serde_json::from_str::<Value>(body).ok();
    let message = structured
        .as_ref()
        .and_then(|value| value.get("error").unwrap_or(value).get("message"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| body.to_string());
    clip_detail(&message)
}

/// 裁到一行能读的长度。
fn clip_detail(detail: &str) -> Option<String> {
    const MAX_CHARS: usize = 120;
    let single_line = detail.split_whitespace().collect::<Vec<_>>().join(" ");
    if single_line.is_empty() {
        return None;
    }
    if single_line.chars().count() <= MAX_CHARS {
        return Some(single_line);
    }
    Some(single_line.chars().take(MAX_CHARS).collect::<String>() + "…")
}

pub(in crate::llm::openai_compatible) fn classify_provider_error_body(
    body: &str,
) -> Option<HttpFailureKind> {
    let structured = serde_json::from_str::<Value>(body).ok();
    let error = structured
        .as_ref()
        .and_then(|value| value.get("error"))
        .or(structured.as_ref());
    let mut signals = Vec::with_capacity(3);
    if let Some(error) = error {
        for field in ["code", "type", "status", "message"] {
            if let Some(value) = error.get(field).and_then(Value::as_str) {
                signals.push(normalize_error_signal(value));
            }
        }
    }
    if signals.is_empty() {
        signals.push(normalize_error_signal(body));
    }

    for signal in &signals {
        if contains_any(
            signal,
            &[
                "invalid_api_key",
                "incorrect_api_key",
                "authentication",
                "unauthorized",
                "forbidden",
                "permission_denied",
            ],
        ) {
            return Some(HttpFailureKind::Authentication);
        }
    }
    for signal in &signals {
        if contains_any(signal, QUOTA_SIGNALS) {
            return Some(HttpFailureKind::Quota);
        }
    }
    for signal in &signals {
        if contains_any(
            signal,
            &["rate_limit", "ratelimit", "quota", "too_many_requests"],
        ) {
            return Some(HttpFailureKind::RateLimit);
        }
    }
    for signal in &signals {
        if contains_any(
            signal,
            &[
                "model_not_found",
                "model_not_available",
                "model_unavailable",
                "unsupported_model",
                "deployment_not_found",
                "model_access_denied",
                "no_available_provider",
                "not_found",
                "provider_unavailable",
                "upstream_request_failed",
                "service_unavailable",
                "overloaded",
            ],
        ) {
            return Some(HttpFailureKind::EndpointUnavailable);
        }
    }
    for signal in &signals {
        if contains_any(
            signal,
            &[
                "context_length_exceeded",
                "context_window_exceeded",
                "maximum_context_length",
                "model_context_window_exceeded",
            ],
        ) {
            return Some(HttpFailureKind::ContextOverflow);
        }
    }
    for signal in &signals {
        if contains_any(
            signal,
            &[
                "context_length",
                "context_window",
                "max_tokens",
                "unsupported_parameter",
                "unknown_parameter",
                "unsupported_feature",
                "not_supported",
            ],
        ) {
            return Some(HttpFailureKind::EndpointIncompatible);
        }
    }
    for signal in &signals {
        if contains_any(
            signal,
            &[
                "invalid_request",
                "invalid_argument",
                "malformed",
                "validation_error",
            ],
        ) {
            return Some(HttpFailureKind::InvalidRequest);
        }
    }
    None
}

pub(in crate::llm::openai_compatible) fn normalize_error_signal(value: &str) -> String {
    let mut normalized = String::with_capacity(value.len());
    let mut separator = false;
    let bytes = value.as_bytes();
    for (index, byte) in bytes.iter().copied().enumerate() {
        if byte.is_ascii_alphanumeric() {
            let previous = index
                .checked_sub(1)
                .and_then(|index| bytes.get(index))
                .copied();
            let next = bytes.get(index + 1).copied();
            let camel_case_boundary = byte.is_ascii_uppercase()
                && previous.is_some_and(|previous| {
                    previous.is_ascii_lowercase()
                        || previous.is_ascii_digit()
                        || (previous.is_ascii_uppercase()
                            && next.is_some_and(|next_byte| next_byte.is_ascii_lowercase()))
                });
            if camel_case_boundary && !separator && !normalized.is_empty() {
                normalized.push('_');
            }
            normalized.push((byte as char).to_ascii_lowercase());
            separator = false;
        } else if !separator && !normalized.is_empty() {
            normalized.push('_');
            separator = true;
        }
    }
    if normalized.ends_with('_') {
        normalized.pop();
    }
    normalized
}

pub(in crate::llm::openai_compatible) fn contains_any(value: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| value.contains(needle))
}

impl std::fmt::Display for HttpStatusFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 分类要打在最前面：这一行是用户唯一看得到的东西，而「HTTP 429」这个
        // 数字对人几乎没有信息量（BUG-16）。中转线连数字都不提——那是编的。
        let label = self.kind.label(self.relay);
        if self.relay {
            write!(f, "{label}")?;
        } else {
            write!(f, "{label}（HTTP {}）", self.status)?;
        }
        match &self.detail {
            Some(detail) => write!(f, "：{detail}"),
            None => Ok(()),
        }
    }
}

impl std::error::Error for HttpStatusFailure {}

pub(in crate::llm::openai_compatible) fn format_error_chain(
    error: &(dyn std::error::Error + 'static),
) -> String {
    let mut message = error.to_string();
    let mut source = error.source();
    while let Some(error) = source {
        message.push_str(": ");
        message.push_str(&error.to_string());
        source = error.source();
    }
    message
}

pub(in crate::llm::openai_compatible) fn anthropic_thinking_unsupported(
    status: u16,
    body: &str,
) -> bool {
    if status != 400 && status != 422 {
        return false;
    }
    let body = body.to_ascii_lowercase();
    body.contains("thinking")
        && (body.contains("unsupported")
            || body.contains("not supported")
            || body.contains("unknown")
            || body.contains("invalid")
            || body.contains("unrecognized"))
}

pub(in crate::llm::openai_compatible) fn responses_unsupported(status: u16, body: &str) -> bool {
    if status == 404 || status == 405 {
        return true;
    }
    if status != 400 {
        return false;
    }
    let body = body.to_ascii_lowercase();
    body.contains("unsupported")
        || body.contains("not supported")
        || body.contains("unknown parameter")
        || body.contains("invalid endpoint")
        || body.contains("not found")
}

pub(in crate::llm::openai_compatible) fn stream_options_unsupported(
    status: u16,
    body: &str,
) -> bool {
    if status != 400 && status != 422 {
        return false;
    }
    let body = body.to_ascii_lowercase();
    body.contains("stream_options")
        && (body.contains("unsupported")
            || body.contains("not supported")
            || body.contains("unknown")
            || body.contains("unrecognized")
            || body.contains("invalid")
            || body.contains("extra"))
}

pub(in crate::llm::openai_compatible) fn non_stream_quota_fallback_candidate(
    status: u16,
    body: &str,
) -> bool {
    status == 429 && body.to_ascii_lowercase().contains("insufficient_quota")
}

pub(in crate::llm::openai_compatible) fn zen_upstream_failed(
    provider: &ProviderConfig,
    status: u16,
    body: &str,
) -> bool {
    status == 400
        && super::zen_headers::is_zen_endpoint(provider)
        && body
            .to_ascii_lowercase()
            .contains("upstream request failed")
}

pub(in crate::llm::openai_compatible) fn is_empty_error(value: &Value) -> bool {
    match value {
        Value::String(text) => text.trim().is_empty(),
        Value::Object(fields) => fields.is_empty(),
        Value::Null => true,
        _ => false,
    }
}

pub(in crate::llm::openai_compatible) fn provider_error_text(value: &Value) -> String {
    value
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| {
            value
                .get("error")
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str)
        })
        .map(|message| clean_plain_text(message.to_string()))
        .unwrap_or_else(|| clean_plain_text(value.to_string()))
}

#[cfg(test)]
mod classify_tests {
    use super::*;

    /// 08-18 实测：LM Studio 对着一个已经删掉的模型名回 404，报文是
    /// `not_found_error`，而分类器只认 `model_not_found`——名字夹在中间就拼不
    /// 出关键词，于是落到 `Status`，冷却时长按「一时的服务端抖动」给，而不是
    /// 按「这个端点没有这个模型」给。
    #[test]
    fn a_404_from_an_endpoint_is_endpoint_unavailable() {
        let body = r#"{"error":{"message":"Model 'Qwen3.5-2B-MLX-8bit' not found. Available models: bge-m3-mlx-8bit, Qwen3.6-35B-A3B-DFlash-MLX-6bit, Qwen3.6-35B-A3B-4bit","type":"not_found_error","param":null,"code":null}}"#;
        assert_eq!(
            HttpStatusFailure::classify(404, body).kind,
            HttpFailureKind::EndpointUnavailable
        );
    }

    /// 报文自己说 not_found 时，即便状态码不是 404 也该按端点不可用处理。
    #[test]
    fn a_not_found_body_is_endpoint_unavailable_whatever_the_status() {
        let body = r#"{"error":{"type":"not_found_error","message":"no such deployment"}}"#;
        assert_eq!(
            classify_provider_error_body(body),
            Some(HttpFailureKind::EndpointUnavailable)
        );
    }

    /// 别把真正的请求错误也吞成端点问题——那类不该换端点重试。
    #[test]
    fn a_malformed_request_still_stops_the_failover() {
        let body =
            r#"{"error":{"type":"invalid_request_error","message":"messages must be an array"}}"#;
        assert_eq!(
            classify_provider_error_body(body),
            Some(HttpFailureKind::InvalidRequest)
        );
        assert!(!endpoint_failover_allowed(&anyhow::Error::new(
            HttpStatusFailure::classify(400, body)
        )));
    }
}

#[cfg(any(test, feature = "testkit"))]
mod test_support;
#[cfg(any(test, feature = "testkit"))]
#[allow(unused_imports)]
pub use test_support::*;
