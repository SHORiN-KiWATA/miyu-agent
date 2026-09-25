//! 失败分类驱动的重试与换端点（09-24 对照 opencode 查出的 B6）。
//!
//! - 429 带了 Retry-After：服务端明说等几秒就行，短的就原地等完再试一次，
//!   别让一次几秒的节流把整轮打成失败（单端点）或把端点关进 600 秒冷却（池里）。
//! - 上下文超长：同一端点重打必然还是超长，补齐的三次纯属白打，还拖住被动压缩。
//! - 402（余额/额度用完）：是对这个账号的裁定，换一家就能答；按「请求本身被拒」
//!   处理会连换端点都不试。

use super::shared::*;
use crate::llm::openai_compatible::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;

const OK_SSE: &str = concat!(
    "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\n",
    "data: {\"choices\":[{\"finish_reason\":\"stop\",\"delta\":{}}]}\n\n",
    "data: [DONE]\n\n"
);

fn http_response(status_line: &str, extra_headers: &str, content_type: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status_line}\r\nContent-Type: {content_type}\r\n{extra_headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn ok_response() -> String {
    http_response("200 OK", "", "text/event-stream", OK_SSE)
}

/// 按顺序回放一串原样 HTTP 响应，用完之后一直重复最后一条；数着被打了几次。
async fn spawn_scripted_endpoint(
    responses: Vec<String>,
) -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    let server = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            read_http_headers(&mut stream).await;
            let index = counter.fetch_add(1, Ordering::SeqCst);
            let response = responses
                .get(index)
                .or_else(|| responses.last())
                .cloned()
                .unwrap();
            stream.write_all(response.as_bytes()).await.unwrap();
        }
    });
    (url, hits, server)
}

fn rate_limited(retry_header: &str) -> String {
    http_response(
        "429 Too Many Requests",
        &format!("{retry_header}\r\n"),
        "application/json",
        r#"{"error":{"message":"Rate limit reached, please try again later.","type":"rate_limit_error"}}"#,
    )
}

async fn ask(client: &OpenAiCompatibleClient) -> Result<ChatResult> {
    client
        .chat_stream(vec![ChatMessage::plain("user", "hi")], Vec::new(), |_| {
            Ok(())
        })
        .await
}

#[tokio::test]
async fn a_short_retry_after_is_waited_out_on_the_same_endpoint() {
    let (url, hits, server) =
        spawn_scripted_endpoint(vec![rate_limited("retry-after-ms: 50"), ok_response()]).await;
    let client = client_over(vec![rate_limit_test_endpoint(
        "retry-after-short-test",
        &url,
    )]);

    let result = ask(&client)
        .await
        .expect("a 50 ms Retry-After should be waited out, not surfaced");

    assert_eq!(result.content, "ok");
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    server.abort();
}

#[tokio::test]
async fn a_long_retry_after_is_not_waited_out() {
    let (url, hits, server) =
        spawn_scripted_endpoint(vec![rate_limited("retry-after: 3600"), ok_response()]).await;
    let client = client_over(vec![rate_limit_test_endpoint(
        "retry-after-long-test",
        &url,
    )]);

    let error = ask(&client).await.unwrap_err();

    assert!(format!("{error:#}").contains("429"), "{error:#}");
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    server.abort();
}

#[tokio::test]
async fn a_context_overflow_is_not_retried_on_the_same_endpoint() {
    let overflow = http_response(
        "400 Bad Request",
        "",
        "application/json",
        r#"{"error":{"message":"This model's maximum context length is 8192 tokens. However, your messages resulted in 9000 tokens.","type":"invalid_request_error","code":"context_length_exceeded"}}"#,
    );
    let (url, hits, server) = spawn_scripted_endpoint(vec![overflow]).await;
    let client = client_over(vec![rate_limit_test_endpoint(
        "context-overflow-single-test",
        &url,
    )]);

    let error = ask(&client).await.unwrap_err();

    assert!(
        crate::llm::is_context_overflow_error(&error),
        "overflow must stay recognizable for compact-and-retry: {error:#}"
    );
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "an over-long conversation is over-long on every retry"
    );
    server.abort();
}

#[tokio::test]
async fn an_out_of_balance_endpoint_fails_over_instead_of_stopping() {
    // 402 的报文措辞各家不一样，有的还把 code 写成 invalid_request_error；
    // 状态码本身已经说清楚了：这个账号没钱了。
    let out_of_balance = http_response(
        "402 Payment Required",
        "",
        "application/json",
        r#"{"error":{"message":"Insufficient Balance","type":"unknown_error","param":null,"code":"invalid_request_error"}}"#,
    );
    let (broke_url, broke_hits, broke_server) = spawn_scripted_endpoint(vec![out_of_balance]).await;
    let (ok_url, ok_hits, ok_server) = spawn_scripted_endpoint(vec![ok_response()]).await;
    let client = client_over(vec![
        rate_limit_test_endpoint("out-of-balance-first-test", &broke_url),
        rate_limit_test_endpoint("out-of-balance-second-test", &ok_url),
    ]);

    let result = ask(&client)
        .await
        .expect("another endpoint with credit should answer");

    assert_eq!(result.content, "ok");
    assert_eq!(broke_hits.load(Ordering::SeqCst), 1);
    assert_eq!(ok_hits.load(Ordering::SeqCst), 1);
    broke_server.abort();
    ok_server.abort();
}

#[test]
fn retry_after_is_read_in_all_three_forms() {
    use reqwest::header::{HeaderMap, HeaderValue};
    let with = |name: &'static str, value: &str| {
        let mut headers = HeaderMap::new();
        headers.insert(name, HeaderValue::from_str(value).unwrap());
        parse_retry_after(&headers)
    };
    assert_eq!(
        with("retry-after-ms", "1500"),
        Some(Duration::from_millis(1500))
    );
    assert_eq!(with("retry-after", "7"), Some(Duration::from_secs(7)));
    // HTTP 日期已经过去：不用等。
    assert_eq!(
        with("retry-after", "Wed, 21 Oct 2015 07:28:00 GMT"),
        Some(Duration::ZERO)
    );
    let at = (chrono::Utc::now() + chrono::Duration::seconds(30)).to_rfc2822();
    let wait = with("retry-after", &at).unwrap();
    assert!(
        wait > Duration::from_secs(20) && wait <= Duration::from_secs(30),
        "{wait:?}"
    );
    assert_eq!(with("retry-after", "soon"), None);
    assert_eq!(parse_retry_after(&HeaderMap::new()), None);
}

#[test]
fn quota_and_overflow_get_their_own_classes() {
    let kind = |status, body| HttpStatusFailure::classify(status, body).kind;
    assert_eq!(kind(402, "{}"), HttpFailureKind::Quota);
    assert_eq!(
        kind(429, r#"{"error":{"code":"insufficient_quota"}}"#),
        HttpFailureKind::Quota
    );
    assert_eq!(
        kind(429, r#"{"error":{"message":"Rate limit reached"}}"#),
        HttpFailureKind::RateLimit
    );
    assert_eq!(
        kind(400, r#"{"error":{"code":"context_length_exceeded"}}"#),
        HttpFailureKind::ContextOverflow
    );
    // 限流的措辞里也有「too many tokens」：不能被当成超长去压缩。
    assert_eq!(
        kind(
            429,
            r#"{"error":{"message":"Too many tokens, please wait before trying again."}}"#
        ),
        HttpFailureKind::RateLimit
    );
}
