//! 工具面字节在会话里保持不变（09-24 对照 opencode 查出的 B5、B13）。
//!
//! tools 排在前缀最前面：它一变，连 system 在内整段缓存都作废。
//! - B5：到了工具轮数上限，最后那一轮原来一个工具都不带——偏偏是上下文最大的
//!   时候整段重算。改成照带工具、`tool_choice: none`。
//! - B13：`load_skill` 的完整描述里拼着技能目录，技能一增删改，在线会话下一轮
//!   的 tools 就变了。改成按会话冻结，压缩之后才换成当时的目录。

use super::shared::*;
use crate::agent::*;
use crate::tools::{empty_parameters, ToolSpec};
use tokio::net::TcpListener;

const TEXT_SSE: &str = concat!(
    "data: {\"choices\":[{\"delta\":{\"content\":\"done\"}}]}\n\n",
    "data: {\"choices\":[{\"finish_reason\":\"stop\",\"delta\":{}}]}\n\n",
    "data: [DONE]\n\n"
);

const NOOP_CALL_SSE: &str = concat!(
    "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"noop\",\"arguments\":\"{}\"}}]}}]}\n\n",
    "data: {\"choices\":[{\"finish_reason\":\"tool_calls\",\"delta\":{}}]}\n\n",
    "data: [DONE]\n\n"
);

/// 依次回放 `replies`，把每条请求体解析好交回来。
async fn serve(listener: TcpListener, replies: Vec<&'static str>) -> Vec<serde_json::Value> {
    let mut requests = Vec::new();
    for reply in replies {
        let (mut stream, _) = listener.accept().await.unwrap();
        let body = read_test_http_request(&mut stream).await;
        requests.push(serde_json::from_slice(&body).unwrap());
        write_test_sse(&mut stream, reply).await;
    }
    requests
}

fn noop_tool() -> ToolSpec {
    ToolSpec::new("noop", "does nothing", empty_parameters(), |_| async {
        Ok("ok".to_string())
    })
}

fn tool_names(request: &serde_json::Value) -> Vec<String> {
    request["tools"]
        .as_array()
        .map(|tools| {
            tools
                .iter()
                .filter_map(|tool| tool["function"]["name"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn the_last_round_at_the_tool_limit_keeps_the_tool_list() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let mut config = queue_test_config(base_url);
    config.tools.enabled = true;
    config.tools.max_rounds = 1;
    let server = tokio::spawn(serve(listener, vec![NOOP_CALL_SSE, TEXT_SSE]));

    let state = StateStore::new(&paths).unwrap();
    state.init_files().unwrap();
    let mut tools = ToolRegistry::new();
    tools.register(noop_tool());
    let provider = config.provider(None).unwrap().clone();
    let client = OpenAiCompatibleClient::new(&provider, &config, &paths).unwrap();
    let mut agent = Agent::new(config, &paths, state, client, tools, PersonaLane::Active).unwrap();

    agent.chat_stream("go", |_| Ok(())).await.unwrap();
    let requests = server.await.unwrap();

    assert!(tool_names(&requests[0]).contains(&"noop".to_string()));
    assert_eq!(
        requests[1]["tools"], requests[0]["tools"],
        "the last round must send the same tool list so the cached prefix survives"
    );
    assert_eq!(requests[1]["tool_choice"], "none");
}

fn load_skill_with(catalog: &str) -> ToolSpec {
    ToolSpec::new(
        "load_skill",
        format!("Load a specialized skill.\n\n<available_skills>{catalog}</available_skills>"),
        empty_parameters(),
        |_| async { Ok("loaded".to_string()) },
    )
}

fn load_skill_description(request: &serde_json::Value) -> String {
    request["tools"]
        .as_array()
        .and_then(|tools| {
            tools
                .iter()
                .find(|tool| tool["function"]["name"] == "load_skill")
        })
        .and_then(|tool| tool["function"]["description"].as_str())
        .unwrap_or_default()
        .to_string()
}

#[tokio::test]
async fn a_skill_catalog_change_waits_for_the_next_compaction() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let mut config = queue_test_config(base_url);
    config.tools.enabled = true;
    // 技能热刷新读的是盘上的技能目录；这里自己挂一个假的 load_skill，再手动换
    // 它的描述，模拟「会话进行中有人发布了新技能」。
    config.skills.enabled = false;
    let server = tokio::spawn(serve(listener, vec![TEXT_SSE, TEXT_SSE, TEXT_SSE]));

    let state = StateStore::new(&paths).unwrap();
    state.init_files().unwrap();
    let mut tools = ToolRegistry::new();
    tools.register(load_skill_with("alpha"));
    let provider = config.provider(None).unwrap().clone();
    let client = OpenAiCompatibleClient::new(&provider, &config, &paths).unwrap();
    let mut agent = Agent::new(
        config,
        &paths,
        state.clone(),
        client,
        tools,
        PersonaLane::Active,
    )
    .unwrap();

    agent.chat_stream("first", |_| Ok(())).await.unwrap();
    agent
        .tools
        .lock()
        .unwrap()
        .register(load_skill_with("alpha, beta"));
    agent.chat_stream("second", |_| Ok(())).await.unwrap();

    // 压缩之后前缀本来就断了一次，这时换成当时的目录。
    let visible = state.load_visible_turns().unwrap();
    let ids = visible
        .iter()
        .map(|turn| turn.turn_id.clone())
        .collect::<Vec<_>>();
    state
        .replace_visible_with_summary(
            &ids[..1],
            &ids,
            "## Task Goal\nKeep chatting.",
            miyu_core::llm::TurnTokens::default(),
            true,
            None,
            None,
        )
        .unwrap();
    agent.chat_stream("third", |_| Ok(())).await.unwrap();

    let requests = server.await.unwrap();
    let first = load_skill_description(&requests[0]);
    assert!(first.contains("alpha"), "{first}");
    assert_eq!(
        load_skill_description(&requests[1]),
        first,
        "a catalog change mid-session must not change the tools bytes"
    );
    assert!(
        load_skill_description(&requests[2]).contains("beta"),
        "after a compaction the session sees the current catalog"
    );
}
