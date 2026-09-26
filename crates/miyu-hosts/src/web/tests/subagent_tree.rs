//! 子代理树：任务条折叠行的「（+N）」和在子代理会话里按停止连它名下的一起停（09-26）。

use super::shared::*;
use crate::web::*;
use miyu_core::state::SubagentTaskState;

/// 后台任务表是进程级的（`init` 只认第一次）：整个测试进程共用一个漏掉的家目录，各测试
/// 按自己的会话号取自己的任务。
fn shared_jobs_home() {
    static INIT: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    INIT.get_or_init(|| {
        let temp = Box::leak(Box::new(tempfile::tempdir().unwrap()));
        tools::jobs::init(&test_paths(temp.path()));
    });
}

/// 在 `session_id` 名下起一条慢的后台命令，返回任务号。
async fn command_in(session_id: &str) -> String {
    let reply = miyu_base::workspace::with_session(
        session_id.into(),
        tools::jobs::spawn_background("sleep 30", Some("慢命令"), &Default::default()),
    )
    .await
    .unwrap();
    serde_json::from_str::<Value>(&reply).unwrap()["job_id"]
        .as_str()
        .unwrap()
        .to_string()
}

/// 在 `session_id` 名下登记一条后台子代理的镜像任务（它的会话是 `child`，数会话时已经数过）。
fn mirror_in(session_id: &str) -> String {
    let session = std::sync::Arc::<str>::from(session_id);
    let (job_id, _) = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(miyu_base::workspace::with_session(
            session,
            async {
                tools::jobs::register_background_subagent(
                    Some("孙代理"),
                    "孙代理",
                    false,
                    |_, _| std::future::pending::<tools::jobs::JobState>(),
                )
            },
        ))
    })
    .unwrap();
    job_id
}

fn running(job_id: &str) -> bool {
    tools::jobs::overview()
        .iter()
        .any(|job| job.job_id == job_id && job.running)
}

struct Tree {
    main: String,
    child: String,
    grandchild: String,
}

/// 主会话 → 子代理（在跑）→ 孙代理（在跑）+ 一个已经做完的孙代理。
fn tree(state: &DaemonState) -> Tree {
    let persona = active_persona_scope(state);
    let store = &state.state_store;
    store.adopt_sessions_for_persona(&persona).unwrap();
    let main = store
        .create_session(&persona, "主会话", "user", None)
        .unwrap()
        .session_id;
    let child = store
        .create_subagent_session(&persona, "写迁移", &main, "", 1, None, true)
        .unwrap()
        .session_id;
    let grandchild = store
        .create_subagent_session(&persona, "查表", &child, "", 2, None, true)
        .unwrap()
        .session_id;
    let finished = store
        .create_subagent_session(&persona, "早做完了", &child, "", 2, None, true)
        .unwrap()
        .session_id;
    store
        .set_session_task_state(&child, SubagentTaskState::Running)
        .unwrap();
    store
        .set_session_task_state(&grandchild, SubagentTaskState::Running)
        .unwrap();
    store
        .set_session_task_state(&finished, SubagentTaskState::Done)
        .unwrap();
    Tree {
        main,
        child,
        grandchild,
    }
}

fn fake_run(session_id: &str, cancel: tokio::sync::watch::Sender<bool>) -> RunInfo {
    RunInfo {
        session_id: session_id.into(),
        mode: PersonaLane::Active,
        audience: PromptAudience::External,
        cancel,
        turn_id: None,
        queue_target: None,
        supersede: Arc::new(miyu_engine::agent::TurnSupersedeSignal::default()),
        platform_followup: None,
        operation: RunOperation::Create,
        job_wake: false,
        turn_origin: miyu_base::workspace::TurnOrigin::Human,
        job_wake_label: None,
        first_event_id: None,
    }
}

/// 一轮在跑的回合：被要求停时自己退场（摘掉、通知），像真的回合那样。返回它有没有被要求停。
fn run_in(state: &DaemonState, run_id: &str, session_id: &str) -> tokio::task::JoinHandle<bool> {
    let (cancel, mut cancelled) = tokio::sync::watch::channel(false);
    state
        .manager
        .lock()
        .unwrap()
        .active_runs
        .insert(run_id.to_string(), fake_run(session_id, cancel));
    let state = state.clone();
    let run_id = run_id.to_string();
    tokio::spawn(async move {
        // 被人从活动表里摘掉（发令端跟着没了）不算「被要求停」。
        let stopped = tokio::time::timeout(Duration::from_secs(3), cancelled.changed())
            .await
            .is_ok_and(|changed| changed.is_ok() && *cancelled.borrow());
        let notify = {
            let mut manager = state.manager.lock().unwrap();
            manager.active_runs.remove(&run_id);
            manager.runs_changed.clone()
        };
        notify.notify_waiters();
        stopped
    })
}

fn task_state(state: &DaemonState, session_id: &str) -> Option<String> {
    state
        .state_store
        .session_record(session_id)
        .unwrap()
        .and_then(|record| record.task_state)
}

/// 停是另起一个任务收的：等它走到记状态那一步。
async fn settled_task_state(
    state: &DaemonState,
    session_id: &str,
    expected: &str,
) -> Option<String> {
    for _ in 0..150 {
        if task_state(state, session_id).as_deref() == Some(expected) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    task_state(state, session_id)
}

/// 主会话的任务条第一层只列子代理那一行，它名下的都收进「（+N）」：在跑的孙代理一个、它
/// 自己的后台命令一个、孙代理的后台命令一个。做完的孙代理、孙代理的镜像任务（和会话是同
/// 一件事）不数。
#[tokio::test(flavor = "multi_thread")]
async fn a_collapsed_row_counts_what_is_still_running_below_it() {
    shared_jobs_home();
    let temp = tempfile::tempdir().unwrap();
    let state = DaemonState::for_test(test_paths(temp.path()), 8300).unwrap();
    let tree = tree(&state);
    let own = command_in(&tree.child).await;
    let below = command_in(&tree.grandchild).await;
    let mirror = mirror_in(&tree.child);

    let listed = handle_session_command(
        &state,
        IpcCommand::ListSubagentSessions {
            session_id: tree.main.clone(),
        },
    )
    .await
    .unwrap();

    for job_id in [&own, &below, &mirror] {
        let _ = tools::jobs::stop_job(job_id).await;
    }
    let rows = listed["sessions"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "{listed}");
    assert_eq!(rows[0]["session_id"], tree.child);
    assert_eq!(rows[0]["running_descendants"], 3, "{listed}");
}

/// 在子代理会话里停它那一轮（终端 Ctrl+C、网页停止按钮）：它名下的孙代理（轮和命令）、它
/// 自己的后台命令一起停，孙代理记成被打断（用户 09-26：Ctrl+C 关掉了子代理，孙代理没停下）。
#[tokio::test(flavor = "multi_thread")]
async fn stopping_inside_a_subagent_takes_its_branch_down() {
    shared_jobs_home();
    let temp = tempfile::tempdir().unwrap();
    let state = DaemonState::for_test(test_paths(temp.path()), 8300).unwrap();
    let tree = tree(&state);
    let own = command_in(&tree.child).await;
    let below = command_in(&tree.grandchild).await;
    let elsewhere = command_in(&tree.main).await;
    let child_run = run_in(&state, "child-run", &tree.child);
    let grandchild_run = run_in(&state, "grandchild-run", &tree.grandchild);

    assert!(cancel_run_and_disarm_goal(&state, "child-run"));
    assert!(child_run.await.unwrap());
    let grandchild_stopped = grandchild_run.await.unwrap();
    for _ in 0..100 {
        if !running(&below) && !running(&own) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let main_command_survived = running(&elsewhere);
    let _ = tools::jobs::stop_job(&elsewhere).await;
    assert!(grandchild_stopped, "the grandchild kept running");
    assert!(!running(&below), "the grandchild's command kept running");
    assert!(!running(&own), "the subagent's own command kept running");
    assert!(
        main_command_survived,
        "the main session's command was stopped too"
    );
    assert_eq!(
        settled_task_state(&state, &tree.grandchild, "interrupted")
            .await
            .as_deref(),
        Some("interrupted")
    );
}

/// 主会话照旧：停主会话这一轮，后台子代理接着跑（它本来就是要自己跑下去的）。
#[tokio::test(flavor = "multi_thread")]
async fn stopping_the_main_session_leaves_background_subagents_running() {
    shared_jobs_home();
    let temp = tempfile::tempdir().unwrap();
    let state = DaemonState::for_test(test_paths(temp.path()), 8300).unwrap();
    let tree = tree(&state);
    let own = command_in(&tree.child).await;
    let main_run = run_in(&state, "main-run", &tree.main);
    let child_run = run_in(&state, "child-run-2", &tree.child);

    assert!(cancel_run_and_disarm_goal(&state, "main-run"));
    assert!(main_run.await.unwrap());
    tokio::time::sleep(Duration::from_millis(200)).await;

    let still_running = running(&own);
    let _ = tools::jobs::stop_job(&own).await;
    state
        .manager
        .lock()
        .unwrap()
        .active_runs
        .remove("child-run-2");
    assert!(
        !child_run.await.unwrap(),
        "the background subagent was stopped"
    );
    assert!(still_running, "the subagent's command was stopped");
    assert_eq!(task_state(&state, &tree.child).as_deref(), Some("running"));
}

/// 子代理这一轮已经说完、闲着等后台时按 Ctrl+C（有后台活的那一级，`StopSessionJobs`）：
/// 同样连孙代理一起停，它自己也记成被打断——等它的主会话照常收到汇报。
#[tokio::test(flavor = "multi_thread")]
async fn stopping_an_idle_subagent_takes_its_branch_down() {
    shared_jobs_home();
    let temp = tempfile::tempdir().unwrap();
    let state = DaemonState::for_test(test_paths(temp.path()), 8300).unwrap();
    let tree = tree(&state);
    state
        .state_store
        .set_session_task_state(&tree.child, SubagentTaskState::Waiting)
        .unwrap();
    let below = command_in(&tree.grandchild).await;
    let grandchild_run = run_in(&state, "grandchild-run-2", &tree.grandchild);

    let (mut client, server) = tokio::net::UnixStream::pair().unwrap();
    let server_state = state.clone();
    let server = tokio::spawn(async move { handle_ipc_connection(server_state, server).await });
    ipc::send(
        &mut client,
        &IpcRequest::new(IpcCommand::StopSessionJobs {
            session_id: tree.child.clone(),
        }),
    )
    .await
    .unwrap();
    let reply = ipc::receive::<IpcFrame>(&mut client).await.unwrap();
    let _ = server.await;

    let grandchild_stopped = grandchild_run.await.unwrap();
    let below_running = running(&below);
    let _ = tools::jobs::stop_job(&below).await;
    assert!(
        matches!(reply, Some(IpcFrame::AdminResult { .. })),
        "{reply:?}"
    );
    assert!(grandchild_stopped, "the grandchild kept running");
    assert!(!below_running, "the grandchild's command kept running");
    // 各层的轮在后台收（回话不等它们退场），状态稍后才记上。
    assert_eq!(
        settled_task_state(&state, &tree.grandchild, "interrupted")
            .await
            .as_deref(),
        Some("interrupted")
    );
    assert_eq!(
        settled_task_state(&state, &tree.child, "interrupted")
            .await
            .as_deref(),
        Some("interrupted")
    );
}

/// 主会话闲着按 Ctrl+C（`StopSessionJobs`）：名下的子代理一支也一起停——哪怕它已经没有镜像任务
/// （用户 09-26：切进子代理停过它一次、又在它会话里让它派了孙代理，回到主会话 Ctrl+C 一个都
/// 停不到）。孙代理的轮和命令停下、记成被打断，子代理也记成被打断。
#[tokio::test(flavor = "multi_thread")]
async fn stopping_an_idle_main_session_takes_branches_without_mirrors_down() {
    shared_jobs_home();
    let temp = tempfile::tempdir().unwrap();
    let state = DaemonState::for_test(test_paths(temp.path()), 8300).unwrap();
    let tree = tree(&state);
    state
        .state_store
        .set_session_task_state(&tree.child, SubagentTaskState::Waiting)
        .unwrap();
    let below = command_in(&tree.grandchild).await;
    let grandchild_run = run_in(&state, "grandchild-run-3", &tree.grandchild);

    let (mut client, server) = tokio::net::UnixStream::pair().unwrap();
    let server_state = state.clone();
    let server = tokio::spawn(async move { handle_ipc_connection(server_state, server).await });
    ipc::send(
        &mut client,
        &IpcRequest::new(IpcCommand::StopSessionJobs {
            session_id: tree.main.clone(),
        }),
    )
    .await
    .unwrap();
    let reply = ipc::receive::<IpcFrame>(&mut client).await.unwrap();
    let _ = server.await;

    // 没被要求停的话，假回合等满 3 秒自己退场、报「没人叫停」。
    let grandchild_stopped = grandchild_run.await.unwrap();
    let below_running = running(&below);
    let _ = tools::jobs::stop_job(&below).await;
    assert!(
        matches!(reply, Some(IpcFrame::AdminResult { .. })),
        "{reply:?}"
    );
    assert!(!below_running, "the grandchild's command kept running");
    assert!(grandchild_stopped, "the grandchild kept running");
    // 各层的轮在后台收（回话不等它们退场），状态稍后才记上。
    assert_eq!(
        settled_task_state(&state, &tree.grandchild, "interrupted")
            .await
            .as_deref(),
        Some("interrupted")
    );
    assert_eq!(
        settled_task_state(&state, &tree.child, "interrupted")
            .await
            .as_deref(),
        Some("interrupted")
    );
}

/// 子代理一多、轮退场又慢（卡在一次长工具调用里），主会话闲着按 Ctrl+C 也当场回话：后台任务当场
/// 停，各层的轮在后台一起收（用户 09-26：打断的时候会卡住，这段里按的键还被回显进输入框）。原来一层
/// 层停、每层等轮退场最多 5 秒，这里两层就要等十秒才回话。
#[tokio::test(flavor = "multi_thread")]
async fn stopping_a_busy_tree_answers_without_waiting_for_runs() {
    shared_jobs_home();
    let temp = tempfile::tempdir().unwrap();
    let state = DaemonState::for_test(test_paths(temp.path()), 8300).unwrap();
    let tree = tree(&state);
    // 叫停了也不退场的两轮：收着取消信号，但不理它。
    let mut asked = Vec::new();
    for (run_id, session) in [
        ("slow-child", &tree.child),
        ("slow-grandchild", &tree.grandchild),
    ] {
        let (cancel, cancelled) = tokio::sync::watch::channel(false);
        state
            .manager
            .lock()
            .unwrap()
            .active_runs
            .insert(run_id.to_string(), fake_run(session, cancel));
        asked.push(cancelled);
    }

    let (mut client, server) = tokio::net::UnixStream::pair().unwrap();
    let server_state = state.clone();
    let server = tokio::spawn(async move { handle_ipc_connection(server_state, server).await });
    let started = std::time::Instant::now();
    ipc::send(
        &mut client,
        &IpcRequest::new(IpcCommand::StopSessionJobs {
            session_id: tree.main.clone(),
        }),
    )
    .await
    .unwrap();
    let reply = ipc::receive::<IpcFrame>(&mut client).await.unwrap();
    let waited = started.elapsed();
    let _ = server.await;
    assert!(
        matches!(reply, Some(IpcFrame::AdminResult { .. })),
        "{reply:?}"
    );
    assert!(waited < Duration::from_secs(3), "回话等了 {waited:?}");
    // 两轮都被叫停了（在后台叫的，稍等一下）。
    for _ in 0..100 {
        if asked.iter().all(|cancelled| *cancelled.borrow()) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        asked.iter().all(|cancelled| *cancelled.borrow()),
        "有一轮没被叫停"
    );
    let mut manager = state.manager.lock().unwrap();
    manager.active_runs.remove("slow-child");
    manager.active_runs.remove("slow-grandchild");
}

/// 任务条那一行的窥视、量、用时不再只靠后台子代理的镜像任务（09-26 用户：停下之后接着聊的那条
/// 子代理，任务条上什么都没有）：子代理会话一起轮，daemon 就跟着事件流记它在干什么，列子代理时
/// 带上。这条会话没有镜像任务。
#[tokio::test(flavor = "multi_thread")]
async fn a_running_subagent_reports_what_it_is_doing_without_a_mirror_job() {
    let temp = tempfile::tempdir().unwrap();
    let state = DaemonState::for_test(test_paths(temp.path()), 8300).unwrap();
    let tree = tree(&state);
    spawn_subagent_activity_tracker(state.clone());
    tokio::time::sleep(Duration::from_millis(50)).await;

    state.events.publish(
        "run.started",
        json!({ "run_id": "live-run", "session_id": tree.child }),
    );
    state.events.publish(
        "tool.started",
        json!({
            "run_id": "live-run",
            "tool_id": "t1",
            "name": "run_command",
            "display_name": "运行命令",
            "arguments": "{\"command\":\"sleep 600\"}",
        }),
    );
    state.events.publish(
        "chat.round_usage",
        json!({ "run_id": "live-run", "cumulative_tokens": 12_345 }),
    );
    let mut row = Value::Null;
    for _ in 0..100 {
        let listed = subagent_sessions_json(&state, &tree.main).unwrap();
        row = listed["sessions"][0].clone();
        if row["peek"].as_str().is_some_and(|peek| !peek.is_empty())
            && row["tokens"].as_u64() == Some(12_345)
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    assert!(row["job_id"].is_null(), "这条没有镜像任务: {row}");
    assert!(
        row["peek"]
            .as_str()
            .is_some_and(|peek| peek.contains("sleep 600")),
        "{row}"
    );
    assert_eq!(row["tokens"], 12_345, "{row}");
    assert!(row["running_since_ms"].as_u64().is_some(), "{row}");

    state
        .events
        .publish("run.completed", json!({ "run_id": "live-run" }));
    for _ in 0..100 {
        if subagent_activity(&tree.child).is_none() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(subagent_activity(&tree.child).is_none(), "这一轮结束就撤");
}
