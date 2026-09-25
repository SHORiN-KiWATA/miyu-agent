//! 摘要结构校验（09-25）：不照模板写的输出不落库。
//!
//! 修前 fork 和隔离两条路径都只判空。fork 请求带着完整的人格 system，模型顺着对话聊
//! 下去的那段正文会被当成摘要存进库，折叠掉的历史从此只剩一句闲聊。

use super::shared::*;
use crate::agent::compact_structure::*;
use crate::agent::*;
use miyu_base::config::AppConfig;
use miyu_base::prompts::COMPACT_SYSTEM_PROMPT;
use tokio::net::TcpListener;

/// 压缩模板第一句：认出「这是摘要请求」（fork 把模板嵌在追加的那条 user 消息里）。
const COMPACT_MARKER: &str = "context summarization assistant";
const PERSONA_CHAT: &str = "好呀～那我们接着聊刚才的事吧！";
const GOOD_SUMMARY: &str =
    "## Task Goal\nKeep the widget build green.\n\n## Current Work\nNothing pending.";

fn sse_text(text: &str) -> String {
    format!(
        "data: {{\"choices\":[{{\"delta\":{{\"content\":{}}}}}]}}\n\n\
         data: {{\"choices\":[{{\"finish_reason\":\"stop\",\"delta\":{{}}}}]}}\n\n\
         data: [DONE]\n\n",
        serde_json::to_string(text).unwrap()
    )
}

#[test]
fn a_summary_needs_two_template_headings() {
    let structure = SummaryStructure::from_template(COMPACT_SYSTEM_PROMPT);
    assert!(structure.accepts(GOOD_SUMMARY));
    assert!(structure.accepts("## **Task Goal**:\nx\n\n## current work\ny"));
    assert!(!structure.accepts(PERSONA_CHAT));
    assert!(!structure.accepts("## Task Goal\nonly one section"));
    // 标题得是模板里的：闲聊里自带的标题不算。
    assert!(!structure.accepts("## 结论\n先这样\n\n## 下一步\n再说"));
    // 同一个标题写两遍不算两个。
    assert!(!structure.accepts("## Task Goal\na\n\n## Task Goal\nb"));
}

/// `MIYU_COMPACT_PROMPT_FILE` 换成没有 `## ` 标题的底稿时不校验。
#[test]
fn a_template_without_headings_accepts_any_summary() {
    let structure = SummaryStructure::from_template("Summarize the conversation in one paragraph.");
    assert!(structure.accepts(PERSONA_CHAT));
}

fn compact_test_config(base_url: String, cache_reuse: bool) -> AppConfig {
    let mut config = queue_test_config(base_url);
    config.providers[0]
        .model_context_window
        .insert("test-model".to_string(), 100_000);
    // 尾巴预算压到很小，保证有旧轮可折。
    config.context.compact_tail_tokens = Some(64);
    config.context.compact_cache_reuse = cache_reuse;
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

/// 起桩端：摘要请求按 `replies` 的顺序作答（用完就一直回最后一条），收下的请求体按到达
/// 顺序返回。`expected` 条摘要请求之后收工。
async fn serve_summaries(
    listener: TcpListener,
    replies: Vec<&'static str>,
    expected: usize,
) -> Vec<String> {
    let mut bodies = Vec::new();
    while bodies.len() < expected {
        let (mut stream, _) = listener.accept().await.unwrap();
        let body = String::from_utf8_lossy(&read_test_http_request(&mut stream).await).to_string();
        assert!(
            body.contains(COMPACT_MARKER),
            "only summary requests expected"
        );
        let reply = replies[bodies.len().min(replies.len() - 1)];
        write_test_sse(&mut stream, &sse_text(reply)).await;
        bodies.push(body);
    }
    bodies
}

async fn agent_with_history(
    temp: &std::path::Path,
    base_url: String,
    cache_reuse: bool,
) -> (Agent, StateStore) {
    let paths = test_paths(temp);
    let config = compact_test_config(base_url, cache_reuse);
    let state = StateStore::new(&paths).unwrap();
    state.init_files().unwrap();
    seed_history(&state);
    let provider = config.provider(None).unwrap().clone();
    let client = OpenAiCompatibleClient::new(&provider, &config, &paths).unwrap();
    let agent = Agent::new(
        config,
        &paths,
        state.clone(),
        client,
        ToolRegistry::new(),
        PersonaLane::Active,
    )
    .unwrap();
    (agent, state)
}

async fn compact(agent: &Agent) -> Result<Option<ChatResult>> {
    tokio::time::timeout(
        std::time::Duration::from_secs(60),
        agent.compact_now(|_| Ok(())),
    )
    .await
    .expect("compaction hung")
}

/// fork 回了一段闲聊：同一前缀追加一句纠正再要一次，存下的是第二次那份摘要。
/// 修前闲聊直接落库。
#[tokio::test]
async fn a_fork_reply_that_ignores_the_template_gets_one_correction() {
    let temp = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(serve_summaries(
        listener,
        vec![PERSONA_CHAT, GOOD_SUMMARY],
        2,
    ));
    let (agent, state) = agent_with_history(temp.path(), base_url, true).await;

    compact(&agent)
        .await
        .expect("the corrected summary should land");

    let summary = state.load_last_summary().unwrap().expect("a summary row");
    assert!(summary.assistant_content.starts_with("## Task Goal"));
    assert!(!summary.assistant_content.contains(PERSONA_CHAT));
    let bodies = server.await.unwrap();
    // 第二次是同一个 fork 接着问：摘要指令还在，后面跟着第一次的回复和那句纠正。
    for body in &bodies {
        assert!(body.contains("Summarize the entire conversation above now"));
    }
    let reply = bodies[1]
        .find(&serde_json::to_string(PERSONA_CHAT).unwrap())
        .expect("the first reply rides along");
    let correction = bodies[1].find(SUMMARY_CORRECTION).expect("the correction");
    assert!(reply < correction);
}

/// 隔离路径（关了缓存复用）两次都不照模板写：手动压缩报错，库里不留那段闲聊。
/// 修前闲聊直接落库、压缩报成功。
#[tokio::test]
async fn an_isolated_summary_that_ignores_the_template_is_not_stored() {
    let temp = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(serve_summaries(listener, vec![PERSONA_CHAT], 2));
    let (agent, state) = agent_with_history(temp.path(), base_url, false).await;

    let error = compact(&agent)
        .await
        .expect_err("an unstructured summary must not land");
    assert!(format!("{error:#}").contains("template"), "{error:#}");
    assert!(state.load_last_summary().unwrap().is_none());
    assert_eq!(
        state.load_visible_turns().unwrap().len(),
        6,
        "nothing folded"
    );
    let bodies = server.await.unwrap();
    assert_eq!(bodies.len(), 2, "one attempt plus the existing retry");
}

/// 隔离摘要请求里看得到工具流：修前 `turns_to_text` 不带 tool_flow。
#[tokio::test]
async fn the_isolated_summary_request_sees_the_tool_output() {
    let temp = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(serve_summaries(listener, vec![GOOD_SUMMARY], 1));
    let (agent, state) = agent_with_history(temp.path(), base_url, false).await;
    state
        .set_turn_tool_flow(
            "turn-0",
            &[miyu_core::state::ToolFlowRound {
                assistant_content: "Running the build.".to_string(),
                calls: vec![miyu_core::state::ToolFlowCall {
                    id: "call-1".to_string(),
                    name: "run_command".to_string(),
                    arguments: r#"{"command":"cargo build --secret-flag"}"#.to_string(),
                    output: "error[E0425]: BUILD-FAILED-MARKER".to_string(),
                    started_ms: None,
                    finished_ms: None,
                    sub_trace: None,
                    child_session_id: None,
                }],
                ..Default::default()
            }],
        )
        .unwrap();

    compact(&agent).await.expect("compaction should succeed");

    let bodies = server.await.unwrap();
    assert!(
        bodies[0].contains("BUILD-FAILED-MARKER"),
        "tool output missing"
    );
    assert!(bodies[0].contains("run_command {command} (1 key)"));
    assert!(
        !bodies[0].contains("--secret-flag"),
        "argument values stay out"
    );
}
