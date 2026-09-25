//! 唤醒模型的那条后台任务报告：场所层拼出来的，终端拆得回结果段（09-26：铃铛那一行点开看
//! 返回的内容）。拼和拆分在两个 crate，这里把两头接起来。

use crate::web::*;

fn completion(log_path: std::path::PathBuf, is_subagent: bool) -> tools::jobs::JobCompletion {
    tools::jobs::JobCompletion {
        job_id: "79ea0b".to_string(),
        title: "查资料".to_string(),
        wake_requested: true,
        is_subagent,
        command: "sleep 1\n- 输出结尾:\n不是结果".to_string(),
        workspace: std::env::temp_dir(),
        session_id: None,
        origin_tty: None,
        platform_sender: None,
        state_label: "已完成".to_string(),
        exit_code: Some(0),
        runtime_seconds: 3,
        log_path,
    }
}

#[test]
fn the_terminal_reads_back_what_the_wake_reported() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("79ea0b.log");
    std::fs::write(&log, "第一行\nBGOUT\n").unwrap();
    let done = completion(log, false);
    for with_log in [true, false] {
        let content = crate::web::actor::job_report_content(&done, &done.command, with_log);
        let result = miyu_core::state::job_report_result(&content).expect("result section");
        assert_eq!(result.kind, miyu_core::state::JobReportKind::OutputTail);
        assert_eq!(result.body, "第一行\nBGOUT", "{content}");
    }
    let empty = dir.path().join("empty.log");
    std::fs::write(&empty, "").unwrap();
    let silent = completion(empty, false);
    let content = crate::web::actor::job_report_content(&silent, "sleep 1", true);
    assert_eq!(miyu_core::state::job_report_result(&content), None);
}
