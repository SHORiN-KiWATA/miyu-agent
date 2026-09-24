//! 上下文溢出后「压缩一次再重试」的兜底（09-24 对照 opencode 查出的 B1）。
//!
//! 回合一开始就在库里写了一行 running；兜底调的压缩读全部可见回合，见到 running
//! 就报「cannot compact while another conversation turn is running」退出。于是兜底
//! 从 08-06 加进来起一次都没成功过：模型一报超长，错误就原样抛给用户（QQ 里是不回话）。

use super::shared::*;
use crate::agent::*;
use miyu_base::config::AppConfig;
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};

/// OpenAI 兼容网关的超长报错原样（`is_context_overflow_message` 认的就是这几句）。
const OVERFLOW_BODY: &str = r#"{"error":{"message":"This model's maximum context length is 8192 tokens. However, your messages resulted in 9000 tokens.","type":"invalid_request_error","code":"context_length_exceeded"}}"#;

/// 压缩模板第一句：认出「这是摘要请求」。
const COMPACT_MARKER: &str = "context summarization assistant";

async fn write_test_error(stream: &mut TcpStream, status: u16, body: &str) {
    let response = format!(
        "HTTP/1.1 {status} Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len(),
    );
    stream.write_all(response.as_bytes()).await.unwrap();
}

fn sse_text(text: &str) -> String {
    format!(
        "data: {{\"choices\":[{{\"delta\":{{\"content\":{}}}}}]}}\n\n\
         data: {{\"choices\":[{{\"finish_reason\":\"stop\",\"delta\":{{}}}}]}}\n\n\
         data: [DONE]\n\n",
        serde_json::to_string(text).unwrap()
    )
}

fn overflow_test_config(base_url: String) -> AppConfig {
    let mut config = queue_test_config(base_url);
    // 窗口给足，回合前不会主动压；超长由服务端报出来，走的正是被动兜底。
    config.providers[0]
        .model_context_window
        .insert("test-model".to_string(), 100_000);
    // 尾巴预算压到很小，保证有旧轮可折。
    config.context.compact_tail_tokens = Some(64);
    config
}

fn seed_history(state: &StateStore) {
    let bulk = "earlier discussion ".repeat(120);
    for index in 0..6 {
        let turn_id = format!("turn-{index}");
        state.start_turn(&turn_id, &bulk, 999_999).unwrap();
        state.complete_turn(&turn_id, &bulk, None).unwrap();
    }
}

/// 桩端点：摘要请求回摘要；摘要之前的普通请求一律回超长；摘要之后回正常答案。
/// 返回每条请求的种类，按到达顺序。
async fn serve_overflow_then_recover(listener: TcpListener) -> Vec<&'static str> {
    let mut seen = Vec::new();
    let mut summarized = false;
    // 兜底失效时请求会无限重来吗？不会，但给个上限免得测试挂住。
    for _ in 0..12 {
        let (mut stream, _) = listener.accept().await.unwrap();
        let body = String::from_utf8_lossy(&read_test_http_request(&mut stream).await).to_string();
        if body.contains(COMPACT_MARKER) {
            seen.push("summary");
            summarized = true;
            write_test_sse(
                &mut stream,
                &sse_text("## Task Goal\nKeep chatting.\n\n## Current Work\nNothing pending."),
            )
            .await;
        } else if !summarized {
            seen.push("overflow");
            write_test_error(&mut stream, 400, OVERFLOW_BODY).await;
        } else {
            seen.push("answer");
            write_test_sse(&mut stream, &sse_text("recovered answer")).await;
            break;
        }
    }
    seen
}

#[tokio::test]
async fn context_overflow_is_recovered_by_compacting_and_retrying() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let config = overflow_test_config(base_url);
    let state = StateStore::new(&paths).unwrap();
    state.init_files().unwrap();
    seed_history(&state);

    let server = tokio::spawn(serve_overflow_then_recover(listener));
    let provider = config.provider(None).unwrap().clone();
    let client = OpenAiCompatibleClient::new(&provider, &config, &paths).unwrap();
    let mut agent = Agent::new(
        config,
        &paths,
        state.clone(),
        client,
        ToolRegistry::new(),
        PersonaLane::Active,
    )
    .unwrap();

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        agent.chat_stream("new question", |_| Ok(())),
    )
    .await
    .expect("turn hung")
    .expect("the overflow should have been recovered by compact-and-retry");

    assert_eq!(result.content, "recovered answer");
    let seen = server.await.unwrap();
    assert!(seen.contains(&"summary"), "no summary request: {seen:?}");
    let visible = state.load_visible_turns().unwrap();
    assert!(
        visible.iter().any(|turn| turn.is_summary),
        "the folded history should now be a summary row"
    );
    assert!(
        visible
            .iter()
            .all(|turn| turn.status != miyu_core::state::TurnStatus::Running),
        "the recovered turn must finish"
    );
}
