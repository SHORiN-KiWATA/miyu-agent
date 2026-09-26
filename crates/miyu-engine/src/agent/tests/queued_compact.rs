//! 回合里排队的 `/compact`（09-25，用户：「像 followup 消息一样排队」）。
//!
//! 回合在接插话的两处检查点（一批工具跑完、模型要收尾时）取走请求，压掉本轮之前的历史：
//! 工具之后那一处把新前缀拼回本轮活尾巴前面接着跑，收尾那一处只压库。

use super::shared::*;
use crate::agent::*;
use crate::tools::{empty_parameters, ToolSpec};
use miyu_base::config::AppConfig;
use tokio::net::TcpListener;

/// 压缩模板第一句：认出「这是摘要请求」（fork 式、隔离式都带它）。
const COMPACT_MARKER: &str = "context summarization assistant";
const SUMMARY: &str = "## Task Goal\nKeep chatting.\n\n## Current Work\nNothing pending.";

fn sse_text(text: &str) -> String {
    format!(
        "data: {{\"choices\":[{{\"delta\":{{\"content\":{}}}}}]}}\n\n\
         data: {{\"choices\":[{{\"finish_reason\":\"stop\",\"delta\":{{}}}}]}}\n\n\
         data: [DONE]\n\n",
        serde_json::to_string(text).unwrap()
    )
}

fn sse_tool_call(name: &str) -> String {
    format!(
        "data: {{\"choices\":[{{\"delta\":{{\"tool_calls\":[{{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{{\"name\":\"{name}\",\"arguments\":\"{{}}\"}}}}]}}}}]}}\n\n\
         data: {{\"choices\":[{{\"finish_reason\":\"tool_calls\",\"delta\":{{}}}}]}}\n\n\
         data: [DONE]\n\n"
    )
}

fn compact_test_config(base_url: String) -> AppConfig {
    let mut config = queue_test_config(base_url);
    config.tools.enabled = true;
    config.tools.loading_mode = "full".to_string();
    config.skills.enabled = false;
    config.memory.enabled = false;
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

/// 桩端点：摘要请求回摘要；其余请求按 `answers` 依次作答。答案发完、也收到过一次摘要请求
/// 才收工（收尾前的那次压缩排在最后一个答案之后）。返回每条请求（种类、正文）。
async fn serve(listener: TcpListener, answers: Vec<String>) -> Vec<(&'static str, String)> {
    let mut seen = Vec::new();
    let mut answers = answers.into_iter().peekable();
    let mut summarized = false;
    for _ in 0..8 {
        if summarized && answers.peek().is_none() {
            break;
        }
        let (mut stream, _) = listener.accept().await.unwrap();
        let body = String::from_utf8_lossy(&read_test_http_request(&mut stream).await).to_string();
        if body.contains(COMPACT_MARKER) {
            write_test_sse(&mut stream, &sse_text(SUMMARY)).await;
            seen.push(("summary", body));
            summarized = true;
            continue;
        }
        let Some(answer) = answers.next() else {
            break;
        };
        write_test_sse(&mut stream, &answer).await;
        seen.push(("model", body));
    }
    seen
}

/// 等桩收工；实现坏了、摘要请求一直不来时别让测试挂死。
async fn requests_seen(
    server: tokio::task::JoinHandle<Vec<(&'static str, String)>>,
) -> Vec<(&'static str, String)> {
    tokio::time::timeout(std::time::Duration::from_secs(10), server)
        .await
        .expect("the queued compact never sent its summary request")
        .unwrap()
}

struct Setup {
    agent: Agent,
    state: StateStore,
    control: AgentTurnControl,
    request: Arc<TurnCompactRequest>,
    _temp: tempfile::TempDir,
}

fn setup(base_url: String) -> Setup {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let config = compact_test_config(base_url);
    let state = StateStore::new(&paths).unwrap();
    state.init_files().unwrap();
    seed_history(&state);
    let mut tools = ToolRegistry::new();
    tools.register(ToolSpec::new(
        "probe_tool",
        "returns a fixed result",
        empty_parameters(),
        |_| async { Ok("probe finished".to_string()) },
    ));
    let provider = config.provider(None).unwrap().clone();
    let client = OpenAiCompatibleClient::new(&provider, &config, &paths).unwrap();
    let agent = Agent::new(
        config,
        &paths,
        state.clone(),
        client,
        tools.clone(),
        PersonaLane::Active,
    )
    .unwrap();
    let mut control = AgentTurnControl::new(PersonaLane::Active, tools.clone(), tools);
    let request = Arc::new(TurnCompactRequest::default());
    control.set_compact_request(request.clone());
    // 回合开始前就排好：等价于「回合跑着时有人敲了 /compact」。
    request.request();
    Setup {
        agent,
        state,
        control,
        request,
        _temp: temp,
    }
}

fn has_summary_row(state: &StateStore) -> bool {
    state
        .load_visible_turns()
        .unwrap()
        .iter()
        .any(|turn| turn.is_summary)
}

#[tokio::test]
async fn a_queued_compact_runs_after_the_tool_round_and_the_turn_goes_on() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(serve(
        listener,
        vec![sse_tool_call("probe_tool"), sse_text("done after compact")],
    ));
    let mut s = setup(base_url);

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        s.agent
            .chat_stream_with_control("new question", &[], &s.control, |_| Ok(())),
    )
    .await
    .expect("turn hung")
    .expect("turn failed");

    assert_eq!(result.content, "done after compact");
    let seen = requests_seen(server).await;
    let kinds = seen.iter().map(|(kind, _)| *kind).collect::<Vec<_>>();
    assert_eq!(kinds, ["model", "summary", "model"], "requests: {kinds:?}");
    let before = seen[0].1.matches("earlier discussion").count();
    let after = &seen[2].1;
    assert!(
        after.contains("Keep chatting."),
        "the next request lacks the summary"
    );
    assert!(
        after.matches("earlier discussion").count() < before,
        "the folded history is still in the next request"
    );
    assert!(
        after.contains("probe finished"),
        "the live tool exchange was lost"
    );
    assert!(!s.request.is_pending());
    assert!(has_summary_row(&s.state));
}

#[tokio::test]
async fn a_queued_compact_runs_before_a_turn_without_tools_finishes() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(serve(listener, vec![sse_text("plain answer")]));
    let mut s = setup(base_url);

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        s.agent
            .chat_stream_with_control("new question", &[], &s.control, |_| Ok(())),
    )
    .await
    .expect("turn hung")
    .expect("turn failed");

    assert_eq!(result.content, "plain answer");
    let seen = requests_seen(server).await;
    let kinds = seen.iter().map(|(kind, _)| *kind).collect::<Vec<_>>();
    assert_eq!(kinds, ["model", "summary"], "requests: {kinds:?}");
    assert!(!s.request.is_pending());
    assert!(has_summary_row(&s.state));
    assert!(s
        .state
        .load_visible_turns()
        .unwrap()
        .iter()
        .all(|turn| turn.status != miyu_core::state::TurnStatus::Running));
}
