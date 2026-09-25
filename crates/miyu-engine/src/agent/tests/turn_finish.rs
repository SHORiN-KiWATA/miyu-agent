//! 回合收尾合成一个事务（09-25，opencode 调研 3.5 第 4 条）。
//!
//! 修前收尾分四五笔写：完成那一笔同时删掉流水，上下文锚点、输出速度、工具流各写一笔。
//! 后几笔失败（或进程在两笔之间死掉），库里就留下「已完成、流水已删、工具流还是上一个
//! 检查点」的轮——而且回合照样报错，人看到的是失败，库里记的却是完成。

use super::shared::*;
use crate::agent::*;
use crate::tools::{empty_parameters, ToolSpec};
use tokio::net::TcpListener;

const PROBE_CALL_SSE: &str = concat!(
    "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_0\",\"type\":\"function\",\"function\":{\"name\":\"probe\",\"arguments\":\"{}\"}}]}}]}\n\n",
    "data: {\"choices\":[{\"finish_reason\":\"tool_calls\",\"delta\":{}}]}\n\n",
    "data: [DONE]\n\n"
);
const TEXT_SSE: &str = concat!(
    "data: {\"choices\":[{\"delta\":{\"content\":\"done\"}}]}\n\n",
    "data: {\"choices\":[{\"finish_reason\":\"stop\",\"delta\":{}}]}\n\n",
    "data: [DONE]\n\n"
);

/// 收尾那笔写失败：回合报错，这一轮不能停在「已完成」。修前完成标记已经单独提交了。
#[tokio::test]
async fn a_failed_finish_does_not_leave_a_completed_turn_behind() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let mut config = queue_test_config(base_url);
    config.tools.enabled = true;
    let server = tokio::spawn(async move {
        for reply in [PROBE_CALL_SSE, TEXT_SSE] {
            let (mut stream, _) = listener.accept().await.unwrap();
            read_test_http_request(&mut stream).await;
            write_test_sse(&mut stream, reply).await;
        }
    });
    let state = StateStore::new(&paths).unwrap();
    state.init_files().unwrap();
    // 回合跑着时的检查点照写（那时还是 running）；完成之后再改工具流就失败。
    let conn =
        rusqlite::Connection::open(paths.conversation_db_dir().join("conversation.db")).unwrap();
    conn.execute_batch(
        "CREATE TRIGGER fail_final_flow
         BEFORE UPDATE OF tool_flow ON turns WHEN OLD.status = 'completed'
         BEGIN SELECT RAISE(ABORT, 'injected finish failure'); END;",
    )
    .unwrap();
    let mut tools = ToolRegistry::new();
    tools.register(ToolSpec::new(
        "probe",
        "test tool",
        empty_parameters(),
        |_| async { Ok("probe output".to_string()) },
    ));
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

    let error = agent
        .chat_stream("go", |_| Ok(()))
        .await
        .expect_err("the injected failure must surface");
    assert!(
        format!("{error:#}").contains("injected finish failure"),
        "{error:#}"
    );
    server.await.unwrap();

    let turn = state.load_turns().unwrap().pop().unwrap();
    assert_ne!(
        turn.status,
        miyu_core::state::TurnStatus::Completed,
        "a failed finish must not leave the turn marked completed"
    );
    // 检查点那份还在：下一轮回放照样看得到这一步工具。
    assert_eq!(turn.tool_flow[0].calls[0].output, "probe output");
}
