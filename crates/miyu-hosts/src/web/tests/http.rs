//! HTTP 接口：鉴权、附件、静态资源、SSE。

use super::shared::*;
use crate::runtime::{clear_platform_session_content, PlatformSessionResetError};
use crate::web::*;

#[test]
fn artifact_tools_are_scoped_to_local_webui_requests() {
    assert!(is_local_webui_request(PromptAudience::External, false));
    assert!(!is_local_webui_request(PromptAudience::Owner, false));
    assert!(!is_local_webui_request(PromptAudience::External, true));
}

/// 服务端文案跟请求语言(2026-09-23 WebUI 双语):同一次进程里,中文与英文两个
/// 请求作用域各自拿到自己那份看板/输入框文案。中间件就是按这个作用域包的
/// (`ui_locale::middleware` → `i18n::scoped`),这里是它最小可复现的落点。
#[tokio::test]
async fn server_text_follows_request_locale_scope() {
    use miyu_base::i18n::Locale;

    let zh = miyu_base::i18n::scoped(Locale::Zh, async {
        (
            default_board_title(),
            default_board_subtitle(),
            default_composer_placeholder("Miyu"),
            default_starter_prompts()[0],
        )
    })
    .await;
    let en = miyu_base::i18n::scoped(Locale::En, async {
        (
            default_board_title(),
            default_board_subtitle(),
            default_composer_placeholder("Miyu"),
            default_starter_prompts()[0],
        )
    })
    .await;

    assert_eq!(zh.0, "今天想聊些什么？");
    assert_eq!(en.0, "What shall we talk about today?");
    assert_ne!(zh.1, en.1);
    assert_eq!(zh.2, "给 Miyu 发消息");
    assert_eq!(en.2, "Message Miyu");
    assert_ne!(zh.3, en.3);
}

/// 清空 token 统计明细:删的是 usage-history.jsonl,累计正账 usage.json
/// 必须留着(它是"一生用了多少"的唯一记录)。08-26 新增按钮的后端契约。
#[test]
fn clearing_usage_history_keeps_the_cumulative_ledger() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let store = miyu_core::state::StateStore::new(&paths).unwrap();
    store.init_files().unwrap();
    let usage = miyu_core::llm::Usage {
        prompt_tokens: 1_000,
        completion_tokens: 100,
        total_tokens: 1_100,
        ..miyu_core::llm::Usage::default()
    };
    store
        .add_auxiliary_usage(
            &usage,
            miyu_core::state::UsageMeta {
                source: "agent",
                provider: Some("p"),
                model: Some("m"),
                kind: None,
            },
        )
        .unwrap();
    let details = |store: &StateStore| {
        store
            .usage_details_for_account(10, None, None, None, None)
            .unwrap()
            .len()
    };
    assert_eq!(details(&store), 1);
    let before = store.usage_snapshot().unwrap().total_tokens;
    assert!(before > 0, "累计账应已记上");

    store.clear_usage_history().unwrap();
    assert_eq!(details(&store), 0, "明细应被清空");
    assert_eq!(
        store.usage_snapshot().unwrap().total_tokens,
        before,
        "累计正账不得被清"
    );
    let stats = store
        .usage_stats(miyu_core::state::UsageRange::All, None)
        .unwrap();
    assert_eq!(stats.totals.requests, 0, "统计页数据应归零");
}

#[tokio::test]
async fn persona_asset_store_is_atomic_and_rejects_corrupt_cache_entries() {
    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path().join("persona-avatars");
    std::fs::create_dir_all(&directory).unwrap();
    let body = b"persona asset";
    let hash = format!("{:x}", Sha256::digest(body));
    let destination = directory.join(format!("{hash}.png"));

    store_persona_asset(&directory, &destination, &hash, body)
        .await
        .unwrap();
    store_persona_asset(&directory, &destination, &hash, body)
        .await
        .unwrap();
    assert_eq!(std::fs::read(&destination).unwrap(), body);

    std::fs::write(&destination, b"corrupt").unwrap();
    store_persona_asset(&directory, &destination, &hash, body)
        .await
        .unwrap();
    assert_eq!(std::fs::read(&destination).unwrap(), body);
}

#[test]
fn attachment_validation_classifies_text_and_falls_back_to_file() {
    let (kind, mime, width, height) =
        inspect_user_attachment("main.rs", b"fn main() {}\n").unwrap();
    assert_eq!(kind, "text");
    assert_eq!(mime, "text/plain");
    assert_eq!((width, height), (0, 0));
    // 09-03 起不再拒绝未知二进制:一律按 file 落盘,只把路径交给模型。
    let (kind, mime, _, _) = inspect_user_attachment("payload.bin", &[0xff, 0xfe, 0xfd]).unwrap();
    assert_eq!(
        (kind.as_str(), mime.as_str()),
        ("file", "application/octet-stream")
    );
    let (kind, mime, _, _) = inspect_user_attachment("clip.mp4", &[0, 0, 0, 0x18]).unwrap();
    assert_eq!((kind.as_str(), mime.as_str()), ("file", "video/mp4"));
    // 白名单外的扩展名即使内容是文本也走 file,不进提示词。
    let (kind, _, _, _) = inspect_user_attachment("notes.exe", b"plain text").unwrap();
    assert_eq!(kind, "file");
    // 白名单内但不是 UTF-8 的也走 file。
    let (kind, _, _, _) = inspect_user_attachment("broken.txt", &[0xff, 0xfe]).unwrap();
    assert_eq!(kind, "file");
}

#[test]
fn file_attachment_is_injected_as_a_path_reference_only() {
    let attachment = miyu_core::state::UserAttachmentData {
        attachment: UserAttachment {
            attachment_id: "att_video".to_string(),
            file_name: "clip.mp4".to_string(),
            mime: "video/mp4".to_string(),
            kind: "file".to_string(),
            size_bytes: 4,
            width: 0,
            height: 0,
            created_at: chrono::Utc::now().to_rfc3339(),
        },
        bytes: Vec::new(),
        path: Some(std::path::PathBuf::from("/tmp/att_video/clip.mp4")),
    };
    let prepared = prepare_web_attachment_data("cut it", vec![attachment]).unwrap();
    assert!(prepared.images.is_empty());
    assert_eq!(
        prepared.content,
        "cut it\n\n<user-attachment name=\"clip.mp4\" mime=\"video/mp4\" size=\"4\" path=\"/tmp/att_video/clip.mp4\" />"
    );
}

#[test]
fn attachment_download_header_preserves_utf8_filename() {
    let value = attachment_content_disposition("报告 1.md", false)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(value.starts_with("attachment;"));
    assert!(value.contains("filename*=UTF-8''%E6%8A%A5%E5%91%8A%201.md"));
}

#[test]
fn qq_group_history_scope_and_offender_deletion_are_isolated() {
    let temp = tempfile::tempdir().unwrap();
    let store = StateStore::new(&test_paths(temp.path())).unwrap();
    let scope = qq_group_scope("123456", "234567").unwrap();
    store
        .plugin_put_json(
            &scope,
            "offender_history",
            &json!({
                "345678": { "user_id": "345678", "ban_count": 2 },
                "456789": { "user_id": "456789", "ban_count": 1 }
            }),
        )
        .unwrap();
    store
        .plugin_update_json::<HashMap<String, Value>, _>(&scope, "offender_history", |current| {
            let mut records = current.unwrap_or_default();
            records.remove("345678");
            Ok(Some(records))
        })
        .unwrap();
    let remaining = store
        .plugin_get_json::<HashMap<String, Value>>(&scope, "offender_history")
        .unwrap()
        .unwrap();
    assert!(!remaining.contains_key("345678"));
    assert!(remaining.contains_key("456789"));
    assert_eq!(scope.platform, "onebot");
    assert_eq!(scope.conversation_kind, "group");
}

#[tokio::test]
async fn platform_session_reset_is_serialized_per_target_session() {
    let temp = tempfile::tempdir().unwrap();
    let (state, actor_join) = test_daemon_with_actor(temp.path());
    let target = state
        .state_store
        .create_session("miyu", "qq target", "user", None)
        .unwrap();
    let other = state
        .state_store
        .create_session("miyu", "other", "user", None)
        .unwrap();
    let target_store = state.state_store.pinned(&target.session_id);
    target_store
        .start_turn("before_reset", "hello", std::process::id())
        .unwrap();
    target_store
        .complete_turn("before_reset", "world", None)
        .unwrap();

    let (other_cancel, _other_cancel_rx) = tokio::sync::watch::channel(false);
    state.manager.lock().unwrap().active_runs.insert(
        "other_run".to_string(),
        RunInfo {
            session_id: other.session_id.clone().into(),
            mode: PersonaLane::Active,
            audience: PromptAudience::Internal,
            cancel: other_cancel,
            turn_id: None,
            queue_target: None,
            supersede: Arc::new(miyu_engine::agent::TurnSupersedeSignal::default()),
            platform_followup: None,
            operation: RunOperation::Create,
            job_wake: false,
            turn_origin: miyu_base::workspace::TurnOrigin::Human,
            job_wake_label: None,
            first_event_id: None,
        },
    );
    assert!(
        clear_platform_session_content(&state, target.session_id.clone().into())
            .await
            .is_ok()
    );
    assert!(target_store.load_turns().unwrap().is_empty());
    assert!(!state.manager.lock().unwrap().admin_busy);

    target_store
        .start_turn("must_survive", "still here", std::process::id())
        .unwrap();
    target_store
        .complete_turn("must_survive", "answer", None)
        .unwrap();
    let (target_cancel, _target_cancel_rx) = tokio::sync::watch::channel(false);
    state.manager.lock().unwrap().active_runs.insert(
        "target_run".to_string(),
        RunInfo {
            session_id: target.session_id.clone().into(),
            mode: PersonaLane::Active,
            audience: PromptAudience::External,
            cancel: target_cancel,
            turn_id: None,
            queue_target: None,
            supersede: Arc::new(miyu_engine::agent::TurnSupersedeSignal::default()),
            platform_followup: None,
            operation: RunOperation::Create,
            job_wake: false,
            turn_origin: miyu_base::workspace::TurnOrigin::Human,
            job_wake_label: None,
            first_event_id: None,
        },
    );
    assert!(matches!(
        clear_platform_session_content(&state, target.session_id.clone().into()).await,
        Err(PlatformSessionResetError::Busy)
    ));
    assert_eq!(target_store.load_turns().unwrap().len(), 1);
    assert!(!state.manager.lock().unwrap().admin_busy);

    state.manager.lock().unwrap().active_runs.clear();
    target_store
        .start_turn("database_running", "working", std::process::id())
        .unwrap();
    assert!(matches!(
        clear_platform_session_content(&state, target.session_id.clone().into()).await,
        Err(PlatformSessionResetError::Busy)
    ));
    assert!(!state.manager.lock().unwrap().admin_busy);
    target_store.interrupt_turn("database_running").unwrap();

    state.actor_tx.send(ActorCommand::Shutdown).unwrap();
    actor_join.join().unwrap().unwrap();
    assert!(matches!(
        clear_platform_session_content(&state, target.session_id.into()).await,
        Err(PlatformSessionResetError::Unavailable)
    ));
    assert!(!state.manager.lock().unwrap().admin_busy);
}

#[test]
fn assistant_sentinels_are_never_exposed() {
    assert_eq!(
        redact_internal_assistant_text(miyu_core::state::pending_placeholder()),
        ""
    );
    assert_eq!(
        redact_internal_assistant_text(miyu_core::state::interrupted_text()),
        ""
    );
    let combined = format!("before {} after", miyu_core::state::interrupted_text());
    let redacted = redact_internal_assistant_text(&combined);
    assert_eq!(redacted, "before  after");
    assert!(!redacted.contains("system-reminder"));
}

#[test]
fn persisted_meme_assets_hide_their_descriptive_caption() {
    let asset = ImageAsset {
        asset_id: "img_test".to_string(),
        turn_id: "turn_test".to_string(),
        tool_id: Some("tool_test".to_string()),
        mime: "image/png".to_string(),
        width: 64,
        height: 64,
        alt: "猫猫 开心 & <得意>".to_string(),
        created_at: "2026-01-01T00:00:00Z".to_string(),
    };
    let reports = vec![
        "<sent_meme>发送了一个表情包：id=sha256:test；description=猫猫 开心 &amp; &lt;得意&gt;</sent_meme>"
            .to_string(),
    ];

    assert!(meme_asset_caption_hidden(&asset, &reports));
    assert!(!meme_asset_caption_hidden(
        &asset,
        &["normal tool output".to_string()]
    ));
}

/// comm 字段可以合法地包含空格和右括号(如进程改名成 "a) b"),解析必须
/// 锚定在最后一个 ')' 之后,否则字段错位会把别的数字当成 tpgid。
#[test]
fn stat_parse_survives_hostile_comm() {
    // 正常 fish:pgrp==tpgid(停在提示符)
    let stat = "1234 (fish) S 1000 1234 1234 34816 1234 4194304 1 0 0 0";
    assert_eq!(parse_stat_pgrp_tpgid(stat), Some((1234, 1234)));
    // comm 里嵌了 ") S 9 9 9 9":只有从最后一个 ')' 起切才对
    let stat = "1234 (a) S 9 9 9 9 (b) R 1000 1234 1234 34816 5678 4194304";
    assert_eq!(parse_stat_pgrp_tpgid(stat), Some((1234, 5678)));
    // 前台在跑别的程序:pgrp != tpgid
    let stat = "1234 (zsh) S 1000 1234 1234 34816 9999 4194304";
    assert_eq!(parse_stat_pgrp_tpgid(stat), Some((1234, 9999)));
    assert_eq!(parse_stat_pgrp_tpgid("no paren here"), None);
}

/// 真 PTY 全链路:python pty.fork 造出「会话首进程挂在 pts 上且是前台」的
/// 假 shell(exec sleep),验证 ① 在提示符判定为真 ② 写回的字节真从 master
/// 端读出来 ③ 进程死后判定翻假。覆盖 /proc 探测和 tty 写入两段真实内核路径。
/// 依赖 /proc 与 /dev/pts,只在 Linux 上成立。
#[cfg(target_os = "linux")]
#[test]
fn origin_tty_gates_and_writeback_against_real_pty() {
    // 这段 Python 的缩进是语义的一部分。拆分模块时被重排过一次(缩进全被
    // 抹平),脚本变成 IndentationError 秒死、无 stdout,下面 `lines.next()`
    // 拿到 None 就 panic —— 报错指向 Rust 侧,真凶却在字符串里。改这里之后
    // 务必单独跑一遍本用例。
    let script = r#"
import os, pty, signal, sys, time
pid, master = pty.fork()
if pid == 0:
    os.execvp("sleep", ["sleep", "60"])
# 子进程是会话首进程,ctty=slave,前台进程组=自己 —— 正是 shell 停在提示符的形状。
# slave 路径从 /proc/child/fd/0 反查,不依赖 ptsname。
# 竞态(08-21 验收实测):fork 返回时子进程可能还没做完 login_tty 的 dup,
# 这一瞬 fd/0 还是继承的管道——读早了就把错误路径一次性交给 Rust,后面
# 怎么重试都救不回。等到它真变成 pts 再上报。
slave = ""
for _ in range(500):
    try:
        slave = os.readlink(f"/proc/{pid}/fd/0")
    except OSError:
        slave = ""
    if slave.startswith("/dev/pts/"):
        break
    time.sleep(0.01)
print(pid, slave, flush=True)
sys.stdin.readline()  # 等 Rust 侧写完
data = b""
try:
    while b"MIYU-E2E-END" not in data:
        data += os.read(master, 4096)
except OSError:
    pass
print("DATA:" + data.hex(), flush=True)
os.kill(pid, signal.SIGKILL)
os.waitpid(pid, 0)
print("GONE", flush=True)
sys.stdin.readline()  # 等 Rust 侧完成死后判定
"#;
    use std::io::{BufRead, BufReader, Write};
    let Ok(mut child) = std::process::Command::new("python3")
        .arg("-c")
        .arg(script)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
    else {
        eprintln!("python3 unavailable; skipping pty gate test");
        return;
    };
    let mut stdin = child.stdin.take().unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    // 「一行都没读到」几乎只有一个成因:上面那段 Python 没跑起来(语法/缩进坏
    // 了,解释器在 stderr 上报错后立刻退出)。裸 unwrap 只会说 `None`,把人引到
    // Rust 侧去查 —— 这里直接把真正的怀疑对象说出来。
    let head = lines
        .next()
        .expect(
            "python3 produced no stdout: the embedded pty script failed to start \
             (check its indentation — it is a raw string and reformatting has broken it before)",
        )
        .unwrap();
    let (pid, slave) = head.split_once(' ').unwrap();
    let origin = miyu_core::ipc::OriginTty {
        path: std::path::PathBuf::from(slave),
        shell_pid: pid.parse().unwrap(),
        follower_pid: None,
    };

    // pty.fork 的父进程拿到 pid 时,子进程的 login_tty(setsid+TIOCSCTTY+dup)
    // 可能还没执行——那一瞬 /proc 里 pgrp≠tpgid、fd/0 也不是 slave,判定为假
    // 是正确行为。重负载下这个窗口放大成闪失败(08-21 验收实测),轮询等子进
    // 程就位再断言;判定函数本身零改动。
    let mut at_prompt = false;
    for _ in 0..200 {
        if origin_shell_at_prompt(&origin) {
            at_prompt = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(at_prompt, "pty.fork 出的会话首进程应判定为「在提示符」");
    // 发起的一次性命令还在前台跟着后面的轮（09-26）：pty 里那个进程占着前台、三个标准流
    // 都在 slave 上，拿它当跟随者判定为真；拿测试进程自己（不在那个终端上）、不带跟随者
    // 都判定为假。
    let followed = miyu_core::ipc::OriginTty {
        follower_pid: Some(origin.shell_pid),
        ..origin.clone()
    };
    assert!(origin_follower_in_foreground(&followed));
    let elsewhere = miyu_core::ipc::OriginTty {
        follower_pid: Some(std::process::id()),
        ..origin.clone()
    };
    assert!(!origin_follower_in_foreground(&elsewhere));
    assert!(!origin_follower_in_foreground(&origin));
    // 走生产写线程:Write 分片 + Finish(flush + SIGWINCH),与流式回写同路。
    {
        use std::os::unix::fs::OpenOptionsExt;
        let tty = std::fs::OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NOCTTY)
            .open(&origin.path)
            .unwrap();
        let (ops_tx, ops_rx) = std::sync::mpsc::channel::<TtyWriteOp>();
        let shell_pid = origin.shell_pid;
        let setup = TtyRenderSetup {
            reasoning_mode: crate::render::ReasoningDisplayMode::Summary,
            tool_call_mode: crate::render::ToolCallDisplayMode::Summary,
            readable_tool_names: true,
            command_output_lines: 10,
            thinking_scroll_lines: 10,
            cross_session_preview_lines: 10,
            cols: 80,
            title: "走查".to_string(),
        };
        let writer = std::thread::spawn(move || origin_tty_writer(tty, shell_pid, ops_rx, setup));
        ops_tx
            .send(TtyWriteOp::Write(
                "\x1b[1m✦ Miyu 后台任务跟进\x1b[0m\r\n".to_string(),
            ))
            .unwrap();
        // 正文走事件路：写线程上的渲染器把 markdown 画出来（和 shellhook 同款）。
        ops_tx
            .send(TtyWriteOp::Event {
                kind: "assistant.delta".to_string(),
                data: serde_json::json!({"delta": "**粗体** 与 `代码` MIYU-E2E-END\n"}),
            })
            .unwrap();
        ops_tx
            .send(TtyWriteOp::Finish { interrupted: false })
            .unwrap();
        writer.join().unwrap();
    }
    stdin.write_all(b"written\n").unwrap();

    let data_line = loop {
        let line = lines.next().unwrap().unwrap();
        if let Some(rest) = line.strip_prefix("DATA:") {
            break rest.to_string();
        }
    };
    let bytes = (0..data_line.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&data_line[i..i + 2], 16).unwrap())
        .collect::<Vec<u8>>();
    let text = String::from_utf8_lossy(&bytes);
    assert!(
        text.contains("Miyu 后台任务跟进"),
        "master 端应读到标题,实际: {text:?}"
    );
    assert!(text.contains("MIYU-E2E-END"), "正文应完整到达");
    assert!(text.contains("\u{1b}["), "应带 SGR 样式");

    let gone = lines.next().unwrap().unwrap();
    assert_eq!(gone, "GONE");
    assert!(!origin_shell_at_prompt(&origin), "进程死后必须判定为不可写");
    assert!(
        !origin_follower_in_foreground(&followed),
        "跟随的命令退出之后,回写照旧"
    );
    stdin.write_all(b"done\n").unwrap();
    let _ = child.wait();
}

/// WebUI 换会话后「累计」要按这条会话的权威值(含子代理花销)重新起算,靠 /context 带
/// 回来。09-23 之前它只给上下文与窗口,换会话后信息行的「累计」一直是上一条会话的数。
#[tokio::test]
async fn session_context_reports_the_sessions_cumulative_including_subagents() {
    let temp = tempfile::tempdir().unwrap();
    let state = DaemonState::for_test(test_paths(temp.path()), 8300).unwrap();
    let persona = active_persona_scope(&state);
    state
        .state_store
        .adopt_sessions_for_persona(&persona)
        .unwrap();
    let other = state
        .state_store
        .create_session(&persona, "别的会话", "user", None)
        .unwrap();
    let child = state
        .state_store
        .create_session(&persona, "深挖", "subagent", Some(&other.session_id))
        .unwrap();
    state
        .state_store
        .record_subagent_usage(&child.session_id, None, None, None, 400, 100, 500, 200)
        .unwrap();

    let snapshot = session_state_for(&state, &other.session_id).unwrap();
    assert_eq!(
        snapshot.cumulative_tokens, 500,
        "子代理的花销要算进它的会话累计"
    );
    let axum::Json(payload) = session_context_http(
        axum::extract::State(state.clone()),
        HeaderMap::new(),
        axum::extract::Path(other.session_id.clone()),
    )
    .await
    .unwrap();
    assert_eq!(payload["cumulative_tokens"], 500, "{payload}");
    assert_eq!(
        payload["cumulative_prompt_tokens"],
        serde_json::json!(snapshot.cumulative_prompt_tokens),
        "{payload}"
    );
    assert_eq!(
        payload["cumulative_cache_read_tokens"],
        serde_json::json!(snapshot.cumulative_cache_read_tokens),
        "{payload}"
    );
}
