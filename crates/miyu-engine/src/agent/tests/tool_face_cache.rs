//! 工具面字节在会话里保持不变（09-24 对照 opencode 查出的 B5、B13）。
//!
//! tools 排在前缀最前面：它一变，连 system 在内整段缓存都作废。
//! - B5：到了工具轮数上限，最后那一轮原来一个工具都不带——偏偏是上下文最大的
//!   时候整段重算。改成照带工具、`tool_choice: none`。
//! - B13：`load_skill` 的完整描述里拼着技能目录，技能一增删改，在线会话下一轮
//!   的 tools 就变了。09-25 起描述是常量，目录改由回合尾巴的指令源发。

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

fn write_skill(paths: &MiyuPaths, name: &str) {
    let dir = paths.skills_dir.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: The {name} skill.\n---\n\nBody.\n"),
    )
    .unwrap();
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

/// 请求里出现过的技能目录块，按先后。
fn catalog_blocks(request: &serde_json::Value) -> Vec<String> {
    request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|message| message["role"] == "user")
        .filter_map(|message| message["content"].as_str())
        .filter(|text| text.starts_with("<available-skills"))
        .map(str::to_string)
        .collect()
}

/// 退回 B13 的「按会话冻结在描述里」：第一条断言就红（描述里拼着目录），中途新增的技能
/// 也要等到压缩之后才看得到。
#[tokio::test]
async fn the_skill_catalog_rides_the_turn_tail_and_the_tools_never_change() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let mut config = queue_test_config(base_url);
    config.tools.enabled = true;
    config.skills.enabled = true;
    write_skill(&paths, "alpha");
    let server = tokio::spawn(serve(listener, vec![TEXT_SSE; 5]));

    let state = StateStore::new(&paths).unwrap();
    state.init_files().unwrap();
    let tools =
        crate::tools::build_tool_registry(&config, &paths, PersonaLane::Active, true).unwrap();
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
    // 会话进行中有人发布了一件新技能。
    write_skill(&paths, "beta");
    agent.chat_stream("second", |_| Ok(())).await.unwrap();
    agent.chat_stream("third", |_| Ok(())).await.unwrap();
    // 压缩把带着目录的几轮全折进摘要。
    let ids = state
        .load_visible_turns()
        .unwrap()
        .iter()
        .map(|turn| turn.turn_id.clone())
        .collect::<Vec<_>>();
    state
        .replace_visible_with_summary(
            &ids,
            &ids,
            "## Task Goal\nKeep chatting.",
            miyu_core::llm::TurnTokens::default(),
            true,
            None,
            None,
        )
        .unwrap();
    agent.chat_stream("fourth", |_| Ok(())).await.unwrap();
    // 技能一件都不剩了。
    std::fs::remove_dir_all(paths.skills_dir.join("alpha")).unwrap();
    std::fs::remove_dir_all(paths.skills_dir.join("beta")).unwrap();
    agent.chat_stream("fifth", |_| Ok(())).await.unwrap();

    let requests = server.await.unwrap();
    let description = load_skill_description(&requests[0]);
    assert!(
        description.contains("<available-skills>") && !description.contains("alpha"),
        "the load_skill description is a constant: {description}"
    );
    for request in &requests[1..] {
        assert_eq!(
            request["tools"], requests[0]["tools"],
            "the tools bytes never change"
        );
    }

    let first = catalog_blocks(&requests[0]);
    assert_eq!(first.len(), 1, "{first:?}");
    assert!(first[0].contains("name=\"alpha\""), "{first:?}");

    let second = catalog_blocks(&requests[1]);
    assert_eq!(
        second.len(),
        2,
        "a changed catalog is sent again: {second:?}"
    );
    assert_eq!(second[0], first[0], "the old copy replays as a fossil");
    assert!(
        second[1].contains("name=\"alpha\"") && second[1].contains("name=\"beta\""),
        "{second:?}"
    );

    assert_eq!(
        catalog_blocks(&requests[2]),
        second,
        "an unchanged catalog is not repeated"
    );

    let after_compaction = catalog_blocks(&requests[3]);
    assert_eq!(
        after_compaction,
        vec![second[1].clone()],
        "the folded copies are gone, so the current catalog is sent again"
    );

    let emptied = catalog_blocks(&requests[4]);
    assert_eq!(
        emptied.last().map(String::as_str),
        Some(crate::tools::NO_SKILLS_NOTICE),
        "{emptied:?}"
    );
}
