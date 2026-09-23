//! 上下文用量表（`context_meter`）：有供应商实测锚点时，在锚点上加尾巴，不把整段
//! 历史重拼重数。

use super::shared::*;
use crate::agent::*;
use tokio::net::TcpListener;

/// 最新一轮被打断：取上一轮的实测锚点，加上被打断这一轮按回放形态估出的增量，
/// 不把整段历史重拼重数（09-23：长会话打断一次要数两遍，debug 下每遍一两秒）。
/// 那一轮还在跑时照旧整段估算——压缩触发线要的就是这个。
#[tokio::test]
async fn an_interrupted_tail_is_added_on_top_of_the_last_anchor() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let mut config = queue_test_config(base_url);
    config.tools.enabled = false;
    config.providers[0]
        .model_context_window
        .insert("test-model".to_string(), 8000);
    config.system_prompt = Some("tail fixture persona".to_string());

    let server = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let _ = read_test_http_request(&mut stream).await;
            write_test_sse(
                &mut stream,
                concat!(
                    "data: {\"choices\":[{\"delta\":{\"content\":\"answer\"}}]}\n\n",
                    "data: {\"choices\":[{\"finish_reason\":\"stop\",\"delta\":{}}]}\n\n",
                    "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":900,\"completion_tokens\":100,\"total_tokens\":1000}}\n\n",
                    "data: [DONE]\n\n"
                ),
            )
            .await;
        }
    });

    let state = StateStore::new(&paths).unwrap();
    state.init_files().unwrap();
    let client =
        OpenAiCompatibleClient::new(config.provider(None).unwrap(), &config, &paths).unwrap();
    let mut agent = Agent::new(
        config,
        &paths,
        state,
        client,
        ToolRegistry::new(),
        PersonaLane::Active,
    )
    .unwrap();
    for i in 0..3 {
        agent
            .chat_stream(&format!("message {i}"), |_| Ok(()))
            .await
            .unwrap();
    }
    assert_eq!(agent.effective_context_tokens().unwrap(), 1000);

    // 还在跑：整段估算（和改之前一样）。
    let interrupted = "打断的这一轮：".to_string() + &"一段足够长的用户输入。".repeat(40);
    agent
        .state
        .start_turn("t-cut", &interrupted, 999999)
        .unwrap();
    assert_eq!(
        agent.effective_context_tokens().unwrap(),
        agent.context_tokens_estimate().unwrap(),
        "a running tail keeps the full estimate the compaction trigger relies on",
    );

    // 打断了：锚点 + 这一轮回放出来的 token。
    agent.state.interrupt_turn("t-cut").unwrap();
    let turns = agent.state.load_visible_turns().unwrap();
    let tail = turns.iter().find(|turn| turn.turn_id == "t-cut").unwrap();
    let mut replay = Vec::new();
    agent.push_history_turn(&mut replay, tail);
    let delta = overflow::estimate_messages_tokens(&replay) as u64;
    assert!(
        delta > 100,
        "the interrupted turn must weigh something: {delta}"
    );
    assert_eq!(agent.effective_context_tokens().unwrap(), 1000 + delta);
    assert_ne!(
        agent.effective_context_tokens().unwrap(),
        agent.context_tokens_estimate().unwrap(),
        "must not fall back to re-estimating the whole history",
    );
    server.abort();
}
