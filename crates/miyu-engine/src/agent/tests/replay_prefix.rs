//! 回放字节守卫：各种形态的回合跑完之后，下一轮第一条请求必须是上一条请求的
//! 纯追加——上一条请求里的每条消息原样还在、一个字节不差。
//!
//! 09-24 排查缓存时坐实：回放不是把活体发过的消息原样重放，而是拿 tool_flow
//! （只记工具轮）+ followups（不记位置）+ 流水账拼回来的。轮里只要有工具轮
//! 以外的东西（轮中插话、goal 收尾通知、被打断），拼出来的顺序就和活体不一样，
//! 下一轮的前缀缓存在这一轮的起点整段断掉。生产上一次最多丢过 57 万 token。

use super::shared::*;
use crate::agent::*;
use crate::tools::ToolSpec;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::TcpListener;

/// 桩端点的一步：回什么，以及收到这条请求时顺手往队列里塞一条排队消息
/// （模拟「模型正在跑，用户又发了一句」）。
struct Step {
    reply: Reply,
    enqueue: Option<&'static str>,
}

enum Reply {
    Sse(String),
    /// 收下请求、永不回复：这条请求在飞的时候回合被打断。
    Hang,
}

fn sse(chunks: &[Value]) -> String {
    let mut out = String::new();
    for chunk in chunks {
        out.push_str("data: ");
        out.push_str(&chunk.to_string());
        out.push_str("\n\n");
    }
    out.push_str("data: [DONE]\n\n");
    out
}

fn tool_step(call_id: &str, step: u32) -> Step {
    let arguments = json!({ "step": step }).to_string();
    Step {
        reply: Reply::Sse(sse(&[
            json!({"choices":[{"delta":{"reasoning_content": format!("plan {call_id}")}}]}),
            json!({"choices":[{"delta":{"content": format!("checking {step}")}}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":call_id,"type":"function",
                "function":{"name":"probe_tool","arguments":arguments}}]}}]}),
            json!({"choices":[{"finish_reason":"tool_calls","delta":{}}]}),
        ])),
        enqueue: None,
    }
}

fn final_step(text: &str) -> Step {
    Step {
        reply: Reply::Sse(sse(&[
            json!({"choices":[{"delta":{"reasoning_content":"wrap up"}}]}),
            json!({"choices":[{"delta":{"content": text}}]}),
            json!({"choices":[{"finish_reason":"stop","delta":{}}]}),
        ])),
        enqueue: None,
    }
}

fn enqueue_on_receipt(mut step: Step, text: &'static str) -> Step {
    step.enqueue = Some(text);
    step
}

struct Harness {
    _temp: tempfile::TempDir,
    paths: MiyuPaths,
    config: AppConfig,
    state: StateStore,
    tools: ToolRegistry,
    steps: Arc<Mutex<VecDeque<Step>>>,
    bodies: Arc<Mutex<Vec<Value>>>,
    /// `step: HANGING_STEP` 的工具调用开跑时响一下,然后永不返回。
    tool_started: Arc<tokio::sync::Notify>,
    server: tokio::task::JoinHandle<()>,
}

/// 调到这一步的 probe_tool 卡住不返回:模拟打断时工具还在跑。
const HANGING_STEP: u32 = 99;

async fn harness() -> Harness {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let mut config = queue_test_config(base_url);
    config.tools.enabled = true;
    config.tools.loading_mode = "full".to_string();
    config.system_prompt = Some("replay prefix guard persona".to_string());
    let state = StateStore::new(&paths).unwrap();
    state.init_files().unwrap();
    let steps = Arc::new(Mutex::new(VecDeque::<Step>::new()));
    let bodies = Arc::new(Mutex::new(Vec::<Value>::new()));
    let server = {
        let steps = steps.clone();
        let bodies = bodies.clone();
        let state = state.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                let steps = steps.clone();
                let bodies = bodies.clone();
                let state = state.clone();
                tokio::spawn(async move {
                    let body = read_test_http_request(&mut stream).await;
                    let step = {
                        let mut bodies = bodies.lock().unwrap();
                        bodies.push(serde_json::from_slice(&body).unwrap_or(Value::Null));
                        steps.lock().unwrap().pop_front()
                    };
                    let Some(step) = step else {
                        return;
                    };
                    if let Some(text) = step.enqueue {
                        state.enqueue_prompt(text, text, text, &[]).unwrap();
                    }
                    match step.reply {
                        Reply::Sse(reply) => write_test_sse(&mut stream, &reply).await,
                        Reply::Hang => tokio::time::sleep(Duration::from_secs(3600)).await,
                    }
                });
            }
        })
    };
    let tool_started = Arc::new(tokio::sync::Notify::new());
    let mut tools = ToolRegistry::new();
    {
        let tool_started = tool_started.clone();
        tools.register(ToolSpec::new(
            "probe_tool",
            "returns a fixed result",
            json!({"type":"object","properties":{"step":{"type":"integer"}}}),
            move |args: Value| {
                let tool_started = tool_started.clone();
                async move {
                    if args["step"] == HANGING_STEP {
                        tool_started.notify_one();
                        tokio::time::sleep(Duration::from_secs(3600)).await;
                    }
                    Ok("probe result".to_string())
                }
            },
        ));
    }
    Harness {
        _temp: temp,
        paths,
        config,
        state,
        tools,
        steps,
        bodies,
        tool_started,
        server,
    }
}

impl Harness {
    fn agent(&self) -> Agent {
        let client = OpenAiCompatibleClient::new(
            self.config.provider(None).unwrap(),
            &self.config,
            &self.paths,
        )
        .unwrap();
        Agent::new(
            self.config.clone(),
            &self.paths,
            self.state.clone(),
            client,
            self.tools.clone(),
            PersonaLane::Active,
        )
        .unwrap()
    }

    fn script(&self, steps: Vec<Step>) {
        self.steps.lock().unwrap().extend(steps);
    }

    fn bodies(&self) -> Vec<Value> {
        self.bodies.lock().unwrap().clone()
    }

    fn control(&self) -> AgentTurnControl {
        AgentTurnControl::new(PersonaLane::Active, self.tools.clone(), ToolRegistry::new())
    }

    async fn turn(&self, agent: &mut Agent, input: &str) {
        // 回合 future 很大(cli_relay 那次的 Box::pin 栈坑),测试线程栈装不下。
        let control = self.control();
        Box::pin(agent.chat_stream_with_control(input, &[], &control, |_| Ok(())))
            .await
            .unwrap();
    }

    /// 等桩端点收到第 `requests` 条请求(那条在飞的请求永不回复)。
    async fn request_arrived(&self, requests: usize) {
        while self.bodies.lock().unwrap().len() < requests {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// 跑一轮,`cut` 一到就把回合掐掉,再像宿主那样把回合记成被打断。
    async fn interrupted_turn(
        &self,
        agent: &mut Agent,
        input: &str,
        cut: impl std::future::Future<Output = ()>,
    ) {
        let control = self.control();
        let turn = Box::pin(agent.chat_stream_with_control(input, &[], &control, |_| Ok(())));
        tokio::select! {
            _ = turn => panic!("the turn must still be running when it is cut"),
            _ = cut => {}
        }
        let turns = self.state.load_turns().unwrap();
        let cut = turns.last().expect("the cut turn was recorded");
        // 宿主收到打断时做的就是这一步;掐掉 future 本身不改回合状态。
        if cut.status == miyu_core::state::TurnStatus::Running {
            self.state.interrupt_turn(&cut.turn_id).unwrap();
        }
        let cut = self.state.load_turn(&cut.turn_id).unwrap().unwrap();
        assert_eq!(
            cut.status,
            miyu_core::state::TurnStatus::Interrupted,
            "{:?}",
            turns
                .iter()
                .map(|turn| (&turn.turn_id, turn.status))
                .collect::<Vec<_>>()
        );
        assert!(
            !cut.journal_events.is_empty(),
            "the cut turn left a journal"
        );
    }
}

fn brief(message: Option<&Value>) -> String {
    let Some(message) = message else {
        return "-".to_string();
    };
    let role = message["role"].as_str().unwrap_or("?");
    let calls = message["tool_calls"].as_array().map_or(0, Vec::len);
    let text: String = match message["content"].as_str() {
        Some(text) => text.chars().take(90).collect(),
        None => message["content"].to_string().chars().take(90).collect(),
    };
    format!("{role} calls={calls} {text:?}")
}

/// 第一条请求 `after` 必须原样包含 `before` 的全部消息与工具表。
fn assert_pure_append(label: &str, before: &Value, after: &Value) {
    let prev = before["messages"].as_array().unwrap();
    let next = after["messages"].as_array().unwrap();
    let shared = prev
        .iter()
        .zip(next.iter())
        .take_while(|(a, b)| a == b)
        .count();
    assert_eq!(
        before["tools"], after["tools"],
        "[{label}] the tool table changed between the two requests"
    );
    assert!(
        shared == prev.len() && next.len() > prev.len(),
        "[{label}] replay diverged at message {shared} of {}:\n  live   {}\n  replay {}",
        prev.len(),
        brief(prev.get(shared)),
        brief(next.get(shared)),
    );
}

/// 第一轮跑完（最后一条请求的下标是 `live_last`），第二轮第一条请求就是它的下一条。
async fn second_turn_extends(h: &Harness, label: &str, live_last: usize) {
    h.script(vec![final_step("done two")]);
    let mut agent = h.agent();
    Box::pin(h.turn(&mut agent, "second question")).await;
    let bodies = h.bodies();
    assert_pure_append(label, &bodies[live_last], &bodies[live_last + 1]);
}

#[tokio::test]
async fn a_completed_tool_turn_replays_as_a_pure_append() {
    let h = harness().await;
    miyu_base::workspace::with_workspace(h.paths.root_dir.clone(), async {
        h.script(vec![
            tool_step("call_a1", 1),
            tool_step("call_a2", 2),
            final_step("done one"),
        ]);
        h.turn(&mut h.agent(), "first question").await;
        second_turn_extends(&h, "completed", 2).await;
    })
    .await;
    h.server.abort();
}

#[tokio::test]
async fn a_followup_merged_after_a_tool_round_replays_where_it_was_sent() {
    let h = harness().await;
    miyu_base::workspace::with_workspace(h.paths.root_dir.clone(), async {
        // 第一条请求到达时塞进一句插话：第一轮工具跑完它就并入对话。
        h.script(vec![
            enqueue_on_receipt(tool_step("call_a1", 1), "also check the other file"),
            tool_step("call_a2", 2),
            final_step("done one"),
        ]);
        h.turn(&mut h.agent(), "first question").await;
        let live = h.bodies();
        assert!(live[1]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| message["content"] == "also check the other file"));
        second_turn_extends(&h, "followup after tool round", 2).await;
    })
    .await;
    h.server.abort();
}

#[tokio::test]
async fn a_followup_merged_after_an_interim_answer_replays_where_it_was_sent() {
    let h = harness().await;
    miyu_base::workspace::with_workspace(h.paths.root_dir.clone(), async {
        // 插话在模型已经给出正文之后才到：正文先落进对话，插话接在它后面，
        // 模型接着答。
        h.script(vec![
            tool_step("call_a1", 1),
            enqueue_on_receipt(final_step("interim answer"), "one more thing"),
            final_step("done one"),
        ]);
        h.turn(&mut h.agent(), "first question").await;
        let live = h.bodies();
        assert!(live[2]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| message["content"] == "one more thing"));
        second_turn_extends(&h, "followup after interim answer", 2).await;
    })
    .await;
    h.server.abort();
}

#[tokio::test]
async fn repeated_identical_rounds_replay_verbatim() {
    let h = harness().await;
    miyu_base::workspace::with_workspace(h.paths.root_dir.clone(), async {
        h.script(vec![
            tool_step("call_a1", 1),
            tool_step("call_a2", 1),
            final_step("done one"),
        ]);
        h.turn(&mut h.agent(), "first question").await;
        second_turn_extends(&h, "identical rounds", 2).await;
    })
    .await;
    h.server.abort();
}

#[tokio::test]
async fn a_goal_notice_injected_mid_turn_replays_where_it_was_sent() {
    let h = harness().await;
    // 回合自己按 StateStore 的会话 id 开会话作用域,goal 的待注入指令也按它挂。
    // goal 运行时是进程级的表:所有测试的库都在默认会话上,不换一条会话,
    // 这条指令会被并行跑的别的测试取走。
    let session = h.state.new_repl_session("default").unwrap();
    h.state.switch_session(&session).unwrap();
    miyu_base::workspace::with_workspace(h.paths.root_dir.clone(), async {
        {
            crate::tools::goal::push_turn_notice(
                &session,
                "<goal-note>wrap up now</goal-note>".into(),
            );
            h.script(vec![tool_step("call_a1", 1), final_step("done one")]);
            h.turn(&mut h.agent(), "first question").await;
            let live = h.bodies();
            let sent = live[1]["messages"].as_array().unwrap();
            assert!(
                sent.iter()
                    .any(|message| message["content"] == "<goal-note>wrap up now</goal-note>"),
                "the notice must ride the second request: {:?}",
                sent.iter()
                    .map(|message| brief(Some(message)))
                    .collect::<Vec<_>>()
            );
            second_turn_extends(&h, "goal notice", 1).await;
        }
    })
    .await;
    h.server.abort();
}

#[tokio::test]
async fn an_interrupted_turn_replays_its_finished_rounds_where_they_were_sent() {
    let h = harness().await;
    miyu_base::workspace::with_workspace(h.paths.root_dir.clone(), async {
        h.script(vec![
            tool_step("call_a1", 1),
            tool_step("call_a2", 2),
            Step {
                reply: Reply::Hang,
                enqueue: None,
            },
        ]);
        h.interrupted_turn(&mut h.agent(), "first question", h.request_arrived(3))
            .await;
        second_turn_extends(&h, "interrupted after two rounds", 2).await;
    })
    .await;
    h.server.abort();
}

#[tokio::test]
async fn a_turn_interrupted_during_its_first_request_replays_as_a_pure_append() {
    let h = harness().await;
    miyu_base::workspace::with_workspace(h.paths.root_dir.clone(), async {
        h.script(vec![Step {
            reply: Reply::Hang,
            enqueue: None,
        }]);
        h.interrupted_turn(&mut h.agent(), "first question", h.request_arrived(1))
            .await;
        second_turn_extends(&h, "interrupted before any round", 0).await;
    })
    .await;
    h.server.abort();
}

#[tokio::test]
async fn a_turn_interrupted_while_a_tool_runs_replays_its_finished_rounds_first() {
    let h = harness().await;
    miyu_base::workspace::with_workspace(h.paths.root_dir.clone(), async {
        h.script(vec![
            tool_step("call_a1", 1),
            tool_step("call_a2", HANGING_STEP),
        ]);
        h.interrupted_turn(&mut h.agent(), "first question", h.tool_started.notified())
            .await;
        // 最后发出去的是第二条请求(带着第一轮的结果);第二轮的调用只在流水账里。
        second_turn_extends(&h, "interrupted while a tool runs", 1).await;
        let replayed = h.bodies()[2]["messages"].as_array().unwrap().clone();
        let tail = replayed
            .iter()
            .rev()
            .take(4)
            .map(|message| brief(Some(message)))
            .collect::<Vec<_>>()
            .join(" | ");
        assert!(
            tail.contains("interrupted"),
            "the cut call is marked: {tail}"
        );
        assert!(tail.contains("interrupted-turn-recovery"), "{tail}");
    })
    .await;
    h.server.abort();
}

/// 每条请求里某个块出现了几份(只数 user 侧正文)。
fn copies(body: &Value, block: &str) -> usize {
    body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|message| message["role"] == "user")
        .filter_map(|message| message["content"].as_str())
        .map(|text| text.matches(block).count())
        .sum()
}

/// 回合尾巴非空(网页 artifact 清单、联想记忆、平台上下文……)的工具轮。尾巴随化石回放
/// 一次;工具流从尾巴追加之前扫起,曾把它当成「第一轮之前并进来的消息」再记一份,回放
/// 出两份,下一轮前缀断在这里(09-24 C3 黑盒实测抓到:工具轮之后那一轮纯追加为假)。
const TURN_TAIL: &str = "<transport-context>trusted per-turn context</transport-context>";

fn with_tail(mut agent: Agent) -> Agent {
    agent.set_turn_system_context(vec![TURN_TAIL.to_string()]);
    agent
}

#[tokio::test]
async fn a_tool_turn_with_a_turn_tail_replays_the_tail_once() {
    let h = harness().await;
    miyu_base::workspace::with_workspace(h.paths.root_dir.clone(), async {
        h.script(vec![
            tool_step("call_a1", 1),
            tool_step("call_a2", 2),
            final_step("done one"),
        ]);
        h.turn(&mut with_tail(h.agent()), "first question").await;
        second_turn_extends(&h, "tool turn with a turn tail", 2).await;
        assert_eq!(copies(&h.bodies()[3], TURN_TAIL), 1);
    })
    .await;
    h.server.abort();
}

#[tokio::test]
async fn a_followup_turn_with_a_turn_tail_replays_the_tail_once() {
    let h = harness().await;
    miyu_base::workspace::with_workspace(h.paths.root_dir.clone(), async {
        h.script(vec![
            enqueue_on_receipt(tool_step("call_a1", 1), "also check the other file"),
            tool_step("call_a2", 2),
            final_step("done one"),
        ]);
        h.turn(&mut with_tail(h.agent()), "first question").await;
        second_turn_extends(&h, "followup turn with a turn tail", 2).await;
        assert_eq!(copies(&h.bodies()[3], TURN_TAIL), 1);
    })
    .await;
    h.server.abort();
}

#[tokio::test]
async fn an_interrupted_turn_with_a_turn_tail_replays_the_tail_once() {
    let h = harness().await;
    miyu_base::workspace::with_workspace(h.paths.root_dir.clone(), async {
        h.script(vec![
            tool_step("call_a1", 1),
            tool_step("call_a2", 2),
            Step {
                reply: Reply::Hang,
                enqueue: None,
            },
        ]);
        h.interrupted_turn(
            &mut with_tail(h.agent()),
            "first question",
            h.request_arrived(3),
        )
        .await;
        second_turn_extends(&h, "interrupted turn with a turn tail", 2).await;
        assert_eq!(copies(&h.bodies()[3], TURN_TAIL), 1);
    })
    .await;
    h.server.abort();
}

/// 轮间消息怎么落 flow:纯文本插话与尾巴原样存字节;带图的插话只记排队消息 id
/// (图的 base64 不抄第二份);工具结果后面那条媒体伴随消息不抄(回放按库里的媒体重建)。
#[test]
fn between_round_messages_are_kept_verbatim_except_media() {
    let call = ToolCall {
        id: "call_1".to_string(),
        kind: "function".to_string(),
        function: ToolCallFunction {
            name: "probe_tool".to_string(),
            arguments: "{}".to_string(),
        },
    };
    let mut companion = ChatMessage::user_parts(vec![ChatContentPart::ImageUrl {
        image_url: ImageUrlContent {
            url: "data:image/png;base64,AAAA".to_string(),
        },
    }]);
    companion.media_companion = true;
    let mut text_followup = ChatMessage::plain("user", "also this");
    text_followup.followup_prompt = Some("q-text".to_string());
    let mut image_followup = ChatMessage::user_parts(vec![
        ChatContentPart::Text {
            text: "look at this".to_string(),
        },
        ChatContentPart::ImageUrl {
            image_url: ImageUrlContent {
                url: "data:image/png;base64,BBBB".to_string(),
            },
        },
    ]);
    image_followup.followup_prompt = Some("q-image".to_string());
    let messages = vec![
        ChatMessage::system("s"),
        ChatMessage::plain("user", "question"),
        ChatMessage::assistant("", Some(vec![call])),
        ChatMessage::tool("call_1", "result"),
        companion,
        text_followup,
        ChatMessage::turn_context("<runtime now=\"x\"/>"),
        image_followup,
    ];
    let flow = derive_tool_flow(&messages, 2, false);
    assert_eq!(flow.len(), 1);
    assert!(flow[0].interleaved);
    let after = serde_json::to_value(&flow[0].after).unwrap();
    assert_eq!(
        after,
        json!([
            {"role": "user", "content": "also this"},
            {"role": "user", "content": "<runtime now=\"x\"/>"},
            {"followup": "q-image"},
        ])
    );
    // 老版本读得回来、新字段缺省时是空的:老记录照旧能解析。
    let legacy: miyu_core::state::ToolFlowRound =
        serde_json::from_value(json!({"assistant_content": "", "calls": []})).unwrap();
    assert!(legacy.after.is_empty() && !legacy.interleaved);
}

fn artifact_block(manifest: &str) -> String {
    crate::tools::webui_artifact_workspace_block(manifest)
}

/// 网页会话每一轮都在回合尾巴里附一份 artifact 清单。清单没变、对话里最近一份就是它时不再
/// 重发——不然每轮都往上下文里多塞一份一模一样的清单,全是没命中的新内容,还一份份化石下去。
/// 比的是「最近一份」:清单 A → B → A 时,模型眼前最近的是 B,A 必须重发。
#[tokio::test]
async fn an_unchanged_artifact_manifest_is_not_sent_again() {
    let h = harness().await;
    let a = artifact_block("(no managed artifacts yet)");
    let b = artifact_block("Managed artifact files in this session:\n- report.md (120 bytes)");
    miyu_base::workspace::with_workspace(h.paths.root_dir.clone(), async {
        for (index, block) in [&a, &a, &b, &a].into_iter().enumerate() {
            h.script(vec![final_step("done")]);
            let mut agent = h.agent();
            agent.set_turn_system_context(vec![block.clone()]);
            Box::pin(h.turn(&mut agent, &format!("question {index}"))).await;
        }
    })
    .await;
    let seen = h
        .bodies()
        .iter()
        .map(|body| (copies(body, &a), copies(body, &b)))
        .collect::<Vec<_>>();
    assert_eq!(
        seen,
        [(1, 0), (1, 0), (1, 1), (2, 1)],
        "(A 的份数, B 的份数) 逐条请求"
    );
    h.server.abort();
}
