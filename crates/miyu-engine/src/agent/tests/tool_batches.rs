//! 一批工具调用的执行顺序与结果（09-24 对照 opencode 查出的 B9、图片插队，以及同轮并发）。

use super::shared::*;
use crate::agent::*;
use crate::tools::{empty_parameters, ToolSpec};
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;

const TEXT_SSE: &str = concat!(
    "data: {\"choices\":[{\"delta\":{\"content\":\"done\"}}]}\n\n",
    "data: {\"choices\":[{\"finish_reason\":\"stop\",\"delta\":{}}]}\n\n",
    "data: [DONE]\n\n"
);

/// 一条回复里发出好几个工具调用（参数都是 `{}`）。
fn calls_sse(names: &[&str]) -> String {
    let calls = names
        .iter()
        .enumerate()
        .map(|(index, name)| {
            format!(
                "{{\"index\":{index},\"id\":\"call_{index}\",\"type\":\"function\",\"function\":{{\"name\":\"{name}\",\"arguments\":\"{{}}\"}}}}"
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "data: {{\"choices\":[{{\"delta\":{{\"tool_calls\":[{calls}]}}}}]}}\n\n\
         data: {{\"choices\":[{{\"finish_reason\":\"tool_calls\",\"delta\":{{}}}}]}}\n\n\
         data: [DONE]\n\n"
    )
}

async fn serve(listener: TcpListener, replies: Vec<String>) -> Vec<serde_json::Value> {
    let mut requests = Vec::new();
    for reply in replies {
        let (mut stream, _) = listener.accept().await.unwrap();
        let body = read_test_http_request(&mut stream).await;
        requests.push(serde_json::from_slice(&body).unwrap());
        write_test_sse(&mut stream, &reply).await;
    }
    requests
}

/// 记下自己被执行了的工具。名叫 subagent 的照生产注册标成可并发。
fn recording_tool(
    name: &'static str,
    log: Arc<Mutex<Vec<String>>>,
    output: &'static str,
) -> ToolSpec {
    let spec = ToolSpec::new(name, "test tool", empty_parameters(), move |_| {
        let log = log.clone();
        async move {
            log.lock().unwrap().push(name.to_string());
            Ok(output.to_string())
        }
    });
    if name == "subagent" {
        spec.concurrent()
    } else {
        spec
    }
}

async fn run_batch(
    names: &[&str],
    tools: ToolRegistry,
    configure: impl FnOnce(&mut miyu_base::config::AppConfig),
) -> (Vec<serde_json::Value>, Vec<(String, bool)>) {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let mut config = queue_test_config(base_url);
    config.tools.enabled = true;
    configure(&mut config);
    let server = tokio::spawn(serve(
        listener,
        vec![calls_sse(names), TEXT_SSE.to_string()],
    ));
    let state = StateStore::new(&paths).unwrap();
    state.init_files().unwrap();
    let provider = config.provider(None).unwrap().clone();
    let client = OpenAiCompatibleClient::new(&provider, &config, &paths).unwrap();
    let mut agent = Agent::new(config, &paths, state, client, tools, PersonaLane::Active).unwrap();
    let mut results = Vec::new();
    agent
        .chat_stream("go", |event| {
            if let AgentEvent::ToolResult { name, ok, .. } = event {
                results.push((name, ok));
            }
            Ok(())
        })
        .await
        .unwrap();
    (server.await.unwrap(), results)
}

/// 09-24 B9：子代理组原来在整批循环之前先跑，越过排在它前面的调用。
#[tokio::test]
async fn a_serial_call_before_subagents_runs_first() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut tools = ToolRegistry::new();
    tools.register(recording_tool("first", log.clone(), "ok"));
    tools.register(recording_tool(
        "subagent",
        log.clone(),
        r#"{"ok":true,"result":"done"}"#,
    ));

    run_batch(&["first", "subagent", "subagent"], tools, |_| {}).await;

    let order = log.lock().unwrap().clone();
    assert_eq!(
        order.first().map(String::as_str),
        Some("first"),
        "{order:?}"
    );
}

/// 09-24 B9：并行组的结果原来一律报成功，子代理失败返回的 `ok:false` 也画成成功。
#[tokio::test]
async fn a_failed_subagent_in_a_parallel_group_is_reported_as_failed() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut tools = ToolRegistry::new();
    tools.register(recording_tool(
        "subagent",
        log,
        r#"{"ok":false,"error":"boom"}"#,
    ));

    let (_, results) = run_batch(&["subagent", "subagent"], tools, |_| {}).await;

    assert_eq!(results.len(), 2, "{results:?}");
    assert!(results.iter().all(|(_, ok)| !ok), "{results:?}");
}

/// 09-24：模型认图、但工具结果里不能放图时，图片补成一条用户消息。一批多个调用时它
/// 原来紧跟在自己那条工具结果后面，插在别的工具结果中间——严格的网关会 400
/// （assistant 的 tool_calls 之后必须先把每个调用的 tool 结果放完）。
#[tokio::test]
async fn an_image_companion_waits_until_every_tool_result_is_in() {
    let mut tools = ToolRegistry::new();
    tools.register(ToolSpec::new(
        "look",
        "returns an image",
        empty_parameters(),
        |_| async {
            Ok(crate::tools::vision::inline::deposit(vec![
                miyu_core::state::TurnInlineMedia {
                    call_id: String::new(),
                    seq: 0,
                    kind: miyu_core::state::INLINE_MEDIA_KIND_IMAGE.to_string(),
                    mime: "image/png".to_string(),
                    source: "shot.png".to_string(),
                    data: Some(b"fake png bytes".to_vec()),
                },
            ]))
        },
    ));
    tools.register(ToolSpec::new(
        "noop",
        "does nothing",
        empty_parameters(),
        |_| async { Ok("ok".to_string()) },
    ));

    let (requests, _) = run_batch(&["look", "noop"], tools, |config| {
        let provider = &mut config.providers[0];
        provider.model_modalities.insert(
            "test-model".to_string(),
            vec!["text".to_string(), "image".to_string()],
        );
        provider.tool_result_media = Some(false);
    })
    .await;

    let messages = requests[1]["messages"].as_array().unwrap();
    let assistant = messages
        .iter()
        .rposition(|message| message["tool_calls"].is_array())
        .unwrap();
    let tail = messages[assistant + 1..]
        .iter()
        .map(|message| {
            format!(
                "{}:{}",
                message["role"].as_str().unwrap_or_default(),
                message["tool_call_id"].as_str().unwrap_or("-")
            )
        })
        .collect::<Vec<_>>();
    // 两条工具结果各一条、紧挨着，图片消息在它们后面。
    assert_eq!(
        &tail[..3],
        &["tool:call_0", "tool:call_1", "user:-"],
        "{tail:?}"
    );
    assert_eq!(
        tail.iter()
            .filter(|entry| entry.as_str() == "tool:call_1")
            .count(),
        1,
        "each call gets exactly one result: {tail:?}"
    );
}
