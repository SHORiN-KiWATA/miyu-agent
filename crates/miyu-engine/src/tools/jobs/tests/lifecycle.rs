//! 起停与状态查询。

use super::shared::*;
use crate::tools::jobs::*;

#[tokio::test]
async fn background_job_lifecycle() {
    shared_init();
    let spawned: Value = serde_json::from_str(
        &spawn_background("echo hello; exit 3", Some("退出码测试"), &test_progress())
            .await
            .unwrap(),
    )
    .unwrap();
    let job_id = spawned["job_id"].as_str().unwrap().to_string();
    assert!(spawned["ok"].as_bool().unwrap());

    await_terminal(&job_id).await;
    let status: Value =
        serde_json::from_str(&job_status(json!({"job_id": job_id})).await.unwrap()).unwrap();
    assert_eq!(status["status"], "exited(3)");
    assert!(status["output"]["content"]
        .as_str()
        .unwrap()
        .contains("hello"));
}

#[test]
fn requested_ids_merge_both_forms_and_drop_repeats() {
    assert_eq!(
        requested_job_ids(&json!({"job_ids": [" a ", "b", "a", ""], "job_id": "b"})),
        vec!["a".to_string(), "b".to_string()]
    );
    assert!(requested_job_ids(&json!({})).is_empty());
    // Caller order survives — a plain sort would reshuffle the report.
    assert_eq!(
        requested_job_ids(&json!({"job_ids": ["z", "a"]})),
        vec!["z".to_string(), "a".to_string()]
    );
}

#[tokio::test]
async fn stopping_returns_without_waiting_out_the_grace_period() {
    shared_init();
    // `sh -c 'trap "" TERM; sleep 30'` ignores SIGTERM, so the old inline
    // grace wait would hold the caller for the full STOP_GRACE per job.
    let mut ids = Vec::new();
    for _ in 0..2 {
        let spawned: Value = serde_json::from_str(
            &spawn_background("trap '' TERM; sleep 30", Some("顽固任务"), &test_progress())
                .await
                .unwrap(),
        )
        .unwrap();
        ids.push(spawned["job_id"].as_str().unwrap().to_string());
    }

    let started = std::time::Instant::now();
    let stopped = stop_session_jobs_for_test(&ids).await;
    let elapsed = started.elapsed();

    assert_eq!(stopped, 2);
    // Two stubborn jobs used to cost 2 × STOP_GRACE; the escalation now
    // runs detached, so the caller is back essentially immediately.
    assert!(
        elapsed < STOP_GRACE,
        "stopping blocked for {elapsed:?}, expected well under {STOP_GRACE:?}"
    );
    for id in &ids {
        // 停止即报告:条目可能已出表;仍在表内则必为终态。
        assert!(job_snapshot(id).is_none_or(|job| job.state.is_terminal()));
    }
}

/// 列表模式必须自带日志尾部:否则模型看完列表还得逐个再查一轮,
/// 而子代理的提示语本来就写着「日志即其进度」。
#[tokio::test]
async fn job_status_list_carries_recent_output_and_log_path() {
    shared_init();
    let spawned: Value = serde_json::from_str(
        &spawn_background("echo listed-marker", Some("列表用"), &test_progress())
            .await
            .unwrap(),
    )
    .unwrap();
    let id = spawned["job_id"].as_str().unwrap().to_string();
    await_terminal(&id).await;

    let status: Value = serde_json::from_str(&job_status(json!({})).await.unwrap()).unwrap();
    let row = status["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["job_id"].as_str() == Some(id.as_str()))
        .expect("列表里应当有这条任务");

    assert!(row["recent_output"]
        .as_str()
        .unwrap()
        .contains("listed-marker"));
    assert!(row["log_size"].as_u64().unwrap() > 0);
    // 完整翻阅走 read_file 读它,所以路径必须给出来。
    assert!(row["log_path"].as_str().unwrap().ends_with(".log"));
    // 模型不必再去解析 "exited(0)" 这种字符串猜状态。
    assert_eq!(row["running"], false);
    assert_eq!(row["kind"], "command");
    assert!(row["title"].is_string());
    // 列表给的是尾部快照,不是续读起点——给了 next_offset 会被拿去续读并漏掉中间。
    assert!(row.get("next_offset").is_none());
}

#[tokio::test]
async fn job_status_reports_several_ids_at_once() {
    shared_init();
    let mut ids = Vec::new();
    for marker in ["alpha", "beta"] {
        let spawned: Value = serde_json::from_str(
            &spawn_background(&format!("echo {marker}"), Some(marker), &test_progress())
                .await
                .unwrap(),
        )
        .unwrap();
        ids.push(spawned["job_id"].as_str().unwrap().to_string());
    }

    for id in &ids {
        await_terminal(id).await;
    }

    let status: Value =
        serde_json::from_str(&job_status(json!({"job_ids": ids})).await.unwrap()).unwrap();
    let rows = status["jobs"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    // Rows come back in the order the ids were asked for.
    for (row, id) in rows.iter().zip(&ids) {
        assert_eq!(row["job_id"].as_str(), Some(id.as_str()));
    }
    for (row, marker) in rows.iter().zip(["alpha", "beta"]) {
        assert!(row["output"]["content"].as_str().unwrap().contains(marker));
    }

    // A single id keeps the flat shape callers already parse.
    let single: Value =
        serde_json::from_str(&job_status(json!({"job_id": ids[0]})).await.unwrap()).unwrap();
    assert_eq!(single["job_id"], ids[0].as_str());
    assert!(single["jobs"].is_null());
    assert!(single["output"]["content"]
        .as_str()
        .unwrap()
        .contains("alpha"));
}

#[tokio::test]
async fn background_subagent_lifecycle() {
    shared_init();
    let spawned: Value = serde_json::from_str(
        &spawn_background_subagent(
            Some("子代理测试"),
            "描述文本",
            false,
            Some("sess_child_1"),
            &test_progress(),
            |_job_id, log_path| async move {
                let _ = std::fs::write(&log_path, "工作中\n");
                JobState::Exited { code: Some(0) }
            },
        )
        .await
        .unwrap(),
    )
    .unwrap();
    let job_id = spawned["job_id"].as_str().unwrap().to_string();
    assert_eq!(spawned["kind"], "background_subagent");
    // 09-26 起子代理只在后台跑：子会话先建好，回执带着它的 id（续话、界面链接都认它）。
    assert_eq!(spawned["session_id"], "sess_child_1");
    await_terminal(&job_id).await;
    let status: Value =
        serde_json::from_str(&job_status(json!({"job_id": job_id})).await.unwrap()).unwrap();
    assert_eq!(status["status"], "exited(0)");
    assert!(status["output"]["content"]
        .as_str()
        .unwrap()
        .contains("工作中"));
}

#[tokio::test]
async fn job_stop_terminates_a_running_job() {
    shared_init();
    let spawned: Value = serde_json::from_str(
        &spawn_background("sleep 300", None, &test_progress())
            .await
            .unwrap(),
    )
    .unwrap();
    let job_id = spawned["job_id"].as_str().unwrap().to_string();
    let stopped: Value =
        serde_json::from_str(&job_stop(json!({"job_id": job_id})).await.unwrap()).unwrap();
    assert_eq!(stopped["status"], "stopped");
    // 新语义(08-16):停止即报告,条目当场出表;日志仍在磁盘。
    let error = job_status(json!({"job_id": job_id})).await.unwrap_err();
    assert!(format!("{error:#}").contains("does not exist"), "{error:#}");
}
