//! 会话级零碎状态入库：待办、指纹、档位钉、上键历史（09-24 会话项目第 1 段）。

use super::shared::*;
use crate::state::*;

fn state_dir(temp: &tempfile::TempDir) -> PathBuf {
    temp.path().join("state")
}

/// 存得进、读得回，删会话时跟着走——以前删会话留下孤儿文件（实测 todos 11/11）。
#[test]
fn a_session_value_round_trips_and_goes_away_with_its_session() {
    let (_temp, store) = test_store();
    let doomed = store
        .create_session("miyu", "doomed", "user", None)
        .unwrap()
        .session_id;
    store
        .set_session_value(&doomed, SessionValueKind::Todos, "[\"write tests\"]")
        .unwrap();
    assert_eq!(
        store
            .session_value(&doomed, SessionValueKind::Todos)
            .unwrap()
            .as_deref(),
        Some("[\"write tests\"]")
    );

    store.delete_session(&doomed).unwrap();

    assert_eq!(
        store
            .conv_db()
            .session_value(&doomed, SessionValueKind::Todos)
            .unwrap(),
        None
    );
}

/// 老文件在第一次读写之前导进来，只导一次；老文件本版留着，好回退。
#[test]
fn a_legacy_file_is_imported_once_before_the_first_write() {
    let (temp, store) = test_store();
    let session = store.session();
    let legacy = state_dir(&temp)
        .join("todos")
        .join(format!("{session}.json"));
    std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    std::fs::write(&legacy, "[\"old\"]").unwrap();

    assert_eq!(
        store
            .session_value(&session, SessionValueKind::Todos)
            .unwrap()
            .as_deref(),
        Some("[\"old\"]")
    );
    store
        .set_session_value(&session, SessionValueKind::Todos, "[\"new\"]")
        .unwrap();
    assert_eq!(
        store
            .session_value(&session, SessionValueKind::Todos)
            .unwrap()
            .as_deref(),
        Some("[\"new\"]")
    );
    assert!(legacy.exists(), "老文件本版要留着");
}

/// 上键历史：老文件里的排在前面，新记的接在后面，只留最近 200 条。
#[test]
fn repl_history_keeps_legacy_lines_first_and_only_the_newest() {
    let (temp, store) = test_store();
    let session = store.session();
    let legacy = state_dir(&temp)
        .join("repl-history")
        .join(format!("{session}.jsonl"));
    std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    std::fs::write(&legacy, "\"one\"\n\"two\"\n\n").unwrap();

    store.append_repl_history(&session, "\"three\"").unwrap();
    assert_eq!(
        store.repl_history(&session).unwrap(),
        ["\"one\"", "\"two\"", "\"three\""]
    );

    for index in 0..REPL_HISTORY_KEEP {
        store
            .append_repl_history(&session, &format!("\"n{index}\""))
            .unwrap();
    }
    let history = store.repl_history(&session).unwrap();
    assert_eq!(history.len(), REPL_HISTORY_KEEP);
    assert_eq!(history.first().unwrap(), "\"n0\"");
    assert_eq!(
        history.last().unwrap(),
        &format!("\"n{}\"", REPL_HISTORY_KEEP - 1)
    );
}

/// 清空之后老文件一起删，也不会在下次读的时候被当成新内容导回来。
#[test]
fn clearing_a_value_removes_the_legacy_file_for_good() {
    let (temp, store) = test_store();
    let session = store.session();
    let legacy = state_dir(&temp)
        .join("session-thinking-variants")
        .join(format!("{session}.json"));
    std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    std::fs::write(&legacy, "{\"selected\":{}}").unwrap();

    store
        .clear_session_value(&session, SessionValueKind::ThinkingPins)
        .unwrap();

    assert!(!legacy.exists());
    assert_eq!(
        store
            .session_value(&session, SessionValueKind::ThinkingPins)
            .unwrap(),
        None
    );
}

/// 成员的档位钉以前在他自己家里，不在 state 目录。
#[test]
fn a_member_thinking_pin_comes_from_the_member_home() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let member = StateStore::open_member(&paths, "alice").unwrap();
    let session = member.session();
    let legacy = paths
        .user_home_dir("alice")
        .join("session-thinking-variants")
        .join(format!("{session}.json"));
    std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    std::fs::write(&legacy, "{\"selected\":{\"p\\tm\":\"high\"}}").unwrap();

    assert_eq!(
        member
            .session_value(&session, SessionValueKind::ThinkingPins)
            .unwrap()
            .as_deref(),
        Some("{\"selected\":{\"p\\tm\":\"high\"}}")
    );
}

/// 删会话把它散在库外的东西一起带走：老文件、spill、压缩转录。别的会话的不动。
#[test]
fn deleting_a_session_removes_its_files_outside_the_database() {
    let (temp, store) = test_store();
    let doomed = store
        .create_session("miyu", "doomed", "user", None)
        .unwrap()
        .session_id;
    let kept = store.session();
    let state = state_dir(&temp);
    let files = |session: &str| {
        vec![
            state.join("todos").join(format!("{session}.json")),
            state.join("repl-history").join(format!("{session}.jsonl")),
            state.join("spill").join(session).join("t1-c1-run.txt"),
            state.join("compact").join(session).join("fold-1.md"),
        ]
    };
    for path in files(&doomed).into_iter().chain(files(&kept)) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "x").unwrap();
    }

    store.delete_session(&doomed).unwrap();

    for path in files(&doomed) {
        assert!(!path.exists(), "没删掉:{}", path.display());
    }
    for path in files(&kept) {
        assert!(path.exists(), "误删了别的会话:{}", path.display());
    }
}
