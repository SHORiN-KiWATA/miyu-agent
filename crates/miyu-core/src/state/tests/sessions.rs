//! 会话增删、人格归属与一次性会话。

use super::shared::*;
use crate::state::*;

#[test]
fn session_crud_switching_and_persona_adoption() {
    let temp = tempfile::tempdir().unwrap();
    let store = StateStore::new(&test_paths(temp.path())).unwrap();
    // Migrated/default rows start persona-less and are claimed on adoption.
    store.adopt_sessions_for_persona("miyu").unwrap();
    let default_id = store.session_id();
    let default = store.session_record(&default_id).unwrap().unwrap();
    assert_eq!(default.persona, "miyu");

    store.start_turn("t1", "hello", std::process::id()).unwrap();
    store.complete_turn("t1", "hi", None).unwrap();

    let created = store
        .create_session("miyu", "旅行计划", "user", None)
        .unwrap();
    store.switch_session(&created.session_id).unwrap();
    assert_eq!(&*store.session_id(), created.session_id.as_str());
    // The new session starts empty; history stays in the old session.
    assert!(store.load_visible_turns().unwrap().is_empty());

    // The pointer is persisted: an independent store resolves to it.
    let reopened = StateStore::new(&test_paths(temp.path())).unwrap();
    assert_eq!(&*reopened.session_id(), created.session_id.as_str());

    let listed = store.list_sessions("miyu").unwrap();
    assert_eq!(listed.len(), 2);
    let default_overview = listed
        .iter()
        .find(|overview| overview.record.session_id == &*default_id)
        .unwrap();
    assert_eq!(default_overview.turn_count, 1);
    assert_eq!(default_overview.last_user_content.as_deref(), Some("hello"));

    assert!(store
        .find_session_by_name("miyu", "旅行计划")
        .unwrap()
        .is_some());
    store.rename_session(&created.session_id, "新名字").unwrap();
    assert!(store
        .find_session_by_name("miyu", "旅行计划")
        .unwrap()
        .is_none());

    // Deleting a session cascades its turns away.
    store.delete_session(&default_id).unwrap();
    assert!(store.session_record(&default_id).unwrap().is_none());
    assert_eq!(store.list_sessions("miyu").unwrap().len(), 1);

    // A dangling pointer self-heals back to a default session.
    store.delete_session(&created.session_id).unwrap();
    let healed = StateStore::new(&test_paths(temp.path())).unwrap();
    assert!(healed
        .session_record(&healed.session_id())
        .unwrap()
        .is_some());
}

#[test]
fn persona_reset_clears_active_local_and_onebot_contexts_only() {
    let (_temp, store) = test_store();
    store.adopt_sessions_for_persona("miyu").unwrap();
    let current = store.session_id().to_string();
    let local = store.create_session("miyu", "local", "user", None).unwrap();
    let second = store
        .create_session("miyu", "second", "user", None)
        .unwrap();
    let other_persona = store
        .create_session("other", "other", "user", None)
        .unwrap();
    let qq = store.create_session("miyu", "qq", "user", None).unwrap();
    store
        .bind_platform_session(
            &PlatformSessionBindingKey {
                platform: "onebot".to_string(),
                account_id: "10000".to_string(),
                conversation_kind: "group".to_string(),
                conversation_id: "42".to_string(),
                participant_id: None,
                persona: "miyu".to_string(),
            },
            &qq.session_id,
        )
        .unwrap();
    let subagent = store
        .create_session("miyu", "child", "subagent", Some(&local.session_id))
        .unwrap();
    let second_child = store
        .create_session("miyu", "second-child", "subagent", Some(&second.session_id))
        .unwrap();

    let sessions = [
        current.clone(),
        local.session_id.clone(),
        second.session_id.clone(),
        other_persona.session_id.clone(),
        qq.session_id.clone(),
        subagent.session_id.clone(),
        second_child.session_id.clone(),
    ];
    for (index, session_id) in sessions.iter().enumerate() {
        let pinned = store.pinned(session_id);
        let turn_id = format!("reset-scope-{index}");
        pinned
            .start_turn(&turn_id, "before", std::process::id())
            .unwrap();
        pinned.complete_turn(&turn_id, "after", None).unwrap();
    }

    let targets = store.persona_reset_session_ids("miyu", "onebot").unwrap();
    assert!(targets.contains(&current));
    assert!(targets.contains(&local.session_id));
    assert!(targets.contains(&qq.session_id));
    assert!(targets.contains(&subagent.session_id));
    // 归档豁免已随功能移除:普通本地会话及其子代理一并进重置范围。
    assert!(targets.contains(&second.session_id));
    assert!(targets.contains(&second_child.session_id));
    assert!(!targets.contains(&other_persona.session_id));

    let cleared = store.reset_persona_contexts("miyu", "onebot").unwrap();
    assert_eq!(cleared, targets);
    for session_id in [
        &current,
        &local.session_id,
        &qq.session_id,
        &subagent.session_id,
        &second.session_id,
        &second_child.session_id,
    ] {
        assert!(store.pinned(session_id).load_turns().unwrap().is_empty());
    }
    for session_id in [&other_persona.session_id] {
        assert_eq!(store.pinned(session_id).load_turns().unwrap().len(), 1);
    }
    assert_eq!(
        store.platform_session_bindings("miyu", "onebot").unwrap()[0].session_id,
        qq.session_id
    );
}

#[test]
fn persona_scope_rename_migrates_sessions_bindings_and_affection() {
    let (_temp, store) = test_store();
    let session = store
        .create_session("old", "QQ group", "user", None)
        .unwrap();
    let old_binding = platform_binding_key("20000", None, "old");
    store
        .bind_platform_session(&old_binding, &session.session_id)
        .unwrap();
    store
        .set_persona_current_session("old", &session.session_id)
        .unwrap();
    let scope = PlatformPluginScopeKey {
        plugin_id: "real_context".to_string(),
        ..plugin_scope("20000")
    };
    store
        .plugin_put_json(
            &scope,
            "affection_profile:old",
            &serde_json::json!({"score": 42}),
        )
        .unwrap();

    store.rename_persona_scope("old", "new").unwrap();

    assert_eq!(
        store
            .session_record(&session.session_id)
            .unwrap()
            .unwrap()
            .persona,
        "new"
    );
    assert!(store
        .find_platform_session_binding(&old_binding)
        .unwrap()
        .is_none());
    let new_binding = platform_binding_key("20000", None, "new");
    assert_eq!(
        store.find_platform_session_binding(&new_binding).unwrap(),
        Some(session.session_id.clone())
    );
    assert_eq!(
        store.persona_current_session("new").unwrap(),
        Some(session.session_id)
    );
    assert!(store
        .plugin_get_json::<serde_json::Value>(&scope, "affection_profile:old")
        .unwrap()
        .is_none());
    assert_eq!(
        store
            .plugin_get_json::<serde_json::Value>(&scope, "affection_profile:new")
            .unwrap()
            .unwrap()["score"],
        42
    );
}

#[test]
fn local_session_listing_excludes_platform_owned_history() {
    let (_temp, store) = test_store();
    let local = store
        .create_session("miyu", "shared name", "user", None)
        .unwrap();
    let platform = store
        .create_session("miyu", "shared name", "user", None)
        .unwrap();
    let key = platform_binding_key("20000", None, "miyu");
    store
        .bind_platform_session(&key, &platform.session_id)
        .unwrap();

    let all_ids = store
        .list_sessions("miyu")
        .unwrap()
        .into_iter()
        .map(|overview| overview.record.session_id)
        .collect::<Vec<_>>();
    assert!(all_ids.contains(&local.session_id));
    assert!(all_ids.contains(&platform.session_id));

    let local_ids = store
        .list_local_sessions("miyu")
        .unwrap()
        .into_iter()
        .map(|overview| overview.record.session_id)
        .collect::<Vec<_>>();
    assert!(local_ids.contains(&local.session_id));
    assert!(!local_ids.contains(&platform.session_id));
    assert!(!store.is_platform_session(&local.session_id).unwrap());
    assert!(store.is_platform_session(&platform.session_id).unwrap());
    assert_eq!(
        store
            .find_local_session_by_name("miyu", "SHARED NAME")
            .unwrap()
            .unwrap()
            .session_id,
        local.session_id
    );
}

#[test]
fn wiping_the_persona_takes_the_subagent_rows_with_it() {
    let temp = tempfile::tempdir().unwrap();
    let store = StateStore::new(&test_paths(temp.path())).unwrap();
    store.adopt_sessions_for_persona("miyu").unwrap();
    let parent = store.session_id();
    let audit = store
        .create_session("miyu", "深挖", "subagent", Some(&parent))
        .unwrap();
    store
        .record_subagent_usage(&audit.session_id, None, None, None, 400, 100, 500, 200)
        .unwrap();
    assert_eq!(store.session_cumulative_token_totals().unwrap().total, 500);

    // Subagent usage lives on the session row, not in `turns` — clearing
    // the turns alone left every Σ still carrying it.
    store.reset_persona_contexts("miyu", "onebot").unwrap();
    assert_eq!(
        store.session_cumulative_token_totals().unwrap(),
        TurnTokens::default()
    );
}

#[test]
fn a_subagents_tokens_land_in_the_launching_sessions_total() {
    let temp = tempfile::tempdir().unwrap();
    let store = StateStore::new(&test_paths(temp.path())).unwrap();
    store.adopt_sessions_for_persona("miyu").unwrap();
    let parent = store.session_id();

    let turn_id = "turn_parent_1";
    store
        .start_turn(turn_id, "问题", std::process::id())
        .unwrap();
    store
        .complete_turn_with_usage_and_model(
            turn_id,
            "答案",
            None,
            None,
            None,
            TurnTokens {
                total: 1_000,
                prompt: 900,
                cache_read: 300,
            },
            false,
        )
        .unwrap();
    assert_eq!(
        store.session_cumulative_token_totals().unwrap(),
        TurnTokens {
            total: 1_000,
            prompt: 900,
            cache_read: 300
        }
    );

    let audit = store
        .create_session("miyu", "深挖", "subagent", Some(&parent))
        .unwrap();
    store
        .record_subagent_usage(&audit.session_id, None, None, None, 400, 100, 500, 200)
        .unwrap();

    // A subagent bills to the session that launched it, cache hits and all
    // — otherwise the most expensive thing a turn can do is invisible.
    assert_eq!(
        store.session_cumulative_token_totals().unwrap(),
        TurnTokens {
            total: 1_500,
            prompt: 1_300,
            cache_read: 500
        }
    );

    // A reset that left the audit sessions behind would zero the history
    // and still report a running total.
    store.reset_conversation().unwrap();
    assert_eq!(
        store.session_cumulative_token_totals().unwrap(),
        TurnTokens::default()
    );
}

#[test]
fn a_subagent_run_recorded_before_the_cache_column_stays_out_of_the_rate() {
    let temp = tempfile::tempdir().unwrap();
    let store = StateStore::new(&test_paths(temp.path())).unwrap();
    store.adopt_sessions_for_persona("miyu").unwrap();
    let parent = store.session_id();
    let audit = store
        .create_session("miyu", "升级前的一次", "subagent", Some(&parent))
        .unwrap();
    // Exactly what the v19 migration leaves behind: usage recorded, cache
    // unknown (NULL). Counting its prompt with no hits to match turned a
    // measured 24% into 1% on the real database.
    store
        .conv_db()
        .record_legacy_subagent_usage_for_test(&audit.session_id, 1_111_360, 1_222_121)
        .unwrap();
    let totals = store.session_cumulative_token_totals().unwrap();
    assert_eq!(totals.total, 1_222_121);
    assert_eq!(
        totals.prompt, 0,
        "unknown cache must not claim a denominator"
    );
    assert_eq!(totals.cache_read, 0);
}

#[test]
fn an_estimated_subagent_run_never_reaches_the_cache_denominator() {
    let temp = tempfile::tempdir().unwrap();
    let store = StateStore::new(&test_paths(temp.path())).unwrap();
    store.adopt_sessions_for_persona("miyu").unwrap();
    let parent = store.session_id();
    let audit = store
        .create_session("miyu", "估算的一次", "subagent", Some(&parent))
        .unwrap();
    // The provider reported nothing, so only the char estimate is known:
    // it inflates the total but must not pretend to be measured prompt.
    store
        .record_subagent_usage(&audit.session_id, None, None, None, 0, 0, 9_000, 0)
        .unwrap();
    let totals = store.session_cumulative_token_totals().unwrap();
    assert_eq!(totals.total, 9_000);
    assert_eq!(totals.prompt, 0);
    assert_eq!(totals.cache_read, 0);
}

#[test]
fn subagent_audit_sessions_are_hidden_and_expire() {
    let temp = tempfile::tempdir().unwrap();
    let store = StateStore::new(&test_paths(temp.path())).unwrap();
    store.adopt_sessions_for_persona("miyu").unwrap();
    let parent = store.session_id();
    let audit = store
        .create_session("miyu", "探索代码库", "subagent", Some(&parent))
        .unwrap();
    let pinned = store.pinned(&audit.session_id);
    pinned
        .start_turn("sat_1", "task prompt", std::process::id())
        .unwrap();
    pinned
        .complete_turn("sat_1", "{\"ok\":true}", None)
        .unwrap();
    store
        .record_subagent_usage(
            &audit.session_id,
            Some("opencode"),
            Some("big-pickle"),
            Some(168000),
            100,
            50,
            150,
            40,
        )
        .unwrap();

    // Hidden from the user-facing session list.
    assert!(store
        .list_sessions("miyu")
        .unwrap()
        .iter()
        .all(|overview| overview.record.session_id != audit.session_id));
    let record = store.session_record(&audit.session_id).unwrap().unwrap();
    assert_eq!(record.kind, "subagent");
    assert_eq!(record.parent_session_id.as_deref(), Some(&*parent));

    // Fresh audit survives cleanup; a backdated one is removed with its
    // turns (FK cascade).
    assert_eq!(store.delete_subagent_sessions_older_than(7).unwrap(), 0);
    store
        .conv_db()
        .record_subagent_usage(&audit.session_id, None, None, None, 0, 0, 0, 0)
        .unwrap();
    // Backdate updated_at directly.
    store.conv_db().touch_session(&audit.session_id).unwrap();
    let backdated = (chrono::Utc::now() - chrono::Duration::days(10)).to_rfc3339();
    // No public API to backdate; use a raw update via the test-only conv_db handle.
    {
        use rusqlite::params;
        let db_path = temp.path().join("state").join("conversation.db");
        let conn = rusqlite::Connection::open(db_path).unwrap();
        conn.execute(
            "UPDATE sessions SET updated_at = ?1 WHERE session_id = ?2",
            params![backdated, audit.session_id],
        )
        .unwrap();
    }
    assert_eq!(store.delete_subagent_sessions_older_than(7).unwrap(), 1);
    assert!(store.session_record(&audit.session_id).unwrap().is_none());
}

#[test]
fn one_shot_sessions_stay_invisible_and_stale_ones_are_swept() {
    let (temp, store) = test_store();
    store.init_files().unwrap();
    let user = store
        .create_session("miyu", "real", USER_SESSION_KIND, None)
        .unwrap();
    let ask = store
        .create_session("miyu", "一次性对话", ASK_SESSION_KIND, None)
        .unwrap();

    // Never listed, never findable by name — only the client holding the
    // freshly minted id can address it.
    let listed = store.list_sessions("miyu").unwrap();
    assert!(listed
        .iter()
        .any(|overview| overview.record.session_id == user.session_id));
    assert!(listed
        .iter()
        .all(|overview| overview.record.session_id != ask.session_id));
    assert!(store
        .find_local_session_by_name("miyu", "一次性对话")
        .unwrap()
        .is_none());

    // Fresh one-shot survives the sweep; an hour-old orphan does not.
    assert_eq!(store.delete_ask_sessions_older_than(1).unwrap(), 0);
    {
        use rusqlite::params;
        let backdated = (chrono::Utc::now() - chrono::Duration::hours(4)).to_rfc3339();
        let db_path = temp.path().join("state").join("conversation.db");
        let conn = rusqlite::Connection::open(db_path).unwrap();
        conn.execute("UPDATE sessions SET updated_at = ?1", params![backdated])
            .unwrap();
    }
    assert_eq!(store.delete_ask_sessions_older_than(1).unwrap(), 1);
    assert!(store.session_record(&ask.session_id).unwrap().is_none());
    // The equally backdated user session is untouched.
    assert!(store.session_record(&user.session_id).unwrap().is_some());
}

/// normal 车道永不落进终端集成会话(08-25 用户裁定):指针缺失自举新会话;
/// 指针被(历史 /session 切换或老回落语义)钉在终端会话上时同样自愈成新
/// 会话并改钉。退回 ensure_repl_session 的守卫前,第二段断言会拿到
/// DEFAULT_SESSION_ID 报红。
#[test]
fn ensure_repl_session_never_lands_on_the_terminal_session() {
    let (_temp, store) = test_store();
    store.init_files().unwrap();
    let terminal = store.session_id().to_string();

    // 库里只有终端会话:自举新会话而不是借用终端车道。
    let bootstrapped = store.ensure_repl_session("miyu").unwrap();
    assert_ne!(bootstrapped, terminal);

    // 指针被钉到终端会话:视同缺失,自愈成另一条新会话。生产形态里终端
    // 会话已被 daemon 启动时的 adopt 认领(persona 匹配、kind=user),指针
    // 校验放行——生产实锤 repl_session_persona:default=default 正是这么
    // 进的终端车道,测试必须先复刻认领。
    store.adopt_sessions_for_persona("miyu").unwrap();
    store.set_repl_session("miyu", &terminal).unwrap();
    assert_eq!(
        store.repl_session("miyu").unwrap().as_deref(),
        Some(terminal.as_str()),
        "认领后指针校验应放行终端会话,否则本用例测不到守卫"
    );
    let healed = store.ensure_repl_session("miyu").unwrap();
    assert_ne!(healed, terminal);
    assert_ne!(healed, bootstrapped);
    assert_eq!(
        store.repl_session("miyu").unwrap().as_deref(),
        Some(healed.as_str())
    );
}

#[test]
fn repl_session_pointer_is_separate_and_drops_when_stale() {
    let (_temp, store) = test_store();
    store.init_files().unwrap();
    let terminal = store.session_id().to_string();
    let repl = store
        .create_session("miyu", "repl lane", USER_SESSION_KIND, None)
        .unwrap();

    assert!(store.repl_session("miyu").unwrap().is_none());
    store.set_repl_session("miyu", &repl.session_id).unwrap();
    assert_eq!(
        store.repl_session("miyu").unwrap().as_deref(),
        Some(repl.session_id.as_str())
    );
    // Moving the REPL lane must not drag the terminal lane along.
    assert_eq!(&*store.session_id(), terminal.as_str());

    // Deleted: the pointer goes stale rather than returning a session
    // the REPL must not land on.
    store.delete_session(&repl.session_id).unwrap();
    assert!(store.repl_session("miyu").unwrap().is_none());
}

#[test]
fn clearing_pinned_session_content_is_isolated_and_preserves_usage_and_binding() {
    let (_temp, store) = test_store();
    let current_session = store.session_id();
    store
        .start_turn("local_turn", "local prompt", std::process::id())
        .unwrap();
    store
        .complete_turn("local_turn", "local answer", None)
        .unwrap();

    let target_record = store
        .create_session("miyu", "qq:10000:private:42", "user", None)
        .unwrap();
    let target = store.pinned(&target_record.session_id);
    target
        .start_turn("qq_turn", "QQ prompt", std::process::id())
        .unwrap();
    target.complete_turn("qq_turn", "QQ answer", None).unwrap();
    target
        .enqueue_prompt("qq_queue", "queued", "queued", &[])
        .unwrap();
    let binding = platform_binding_key("42", None, "miyu");
    store
        .bind_platform_session(&binding, &target_record.session_id)
        .unwrap();

    store
        .add_usage(
            &Usage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
                ..Usage::default()
            },
            UsageMeta {
                source: "agent",
                provider: Some("prov"),
                model: Some("model"),
                kind: None,
            },
        )
        .unwrap();
    let usage_before = store.usage_snapshot().unwrap();

    target.clear_session_content().unwrap();

    assert!(target.load_turns().unwrap().is_empty());
    assert!(target.load_queued_prompts().unwrap().is_empty());
    assert_eq!(store.load_turns().unwrap().len(), 1);
    assert_eq!(store.session_id(), current_session);
    assert!(store
        .session_record(&target_record.session_id)
        .unwrap()
        .is_some());
    assert_eq!(
        store.find_platform_session_binding(&binding).unwrap(),
        Some(target_record.session_id)
    );
    let usage_after = store.usage_snapshot().unwrap();
    assert_eq!(usage_after.total_tokens, usage_before.total_tokens);
    assert_eq!(
        usage_after.conversation_tokens,
        usage_before.conversation_tokens
    );
}

/// 沙盒随开随关(09-23):解绑 = 明确不要沙盒(之后不跟全局默认走),绑定撤回这个
/// 声明;只读两种情况都清掉——绑根是「要在这里写」,解绑是「不要任何限制」。
/// 子代理把整套设置照抄过去,父会话按了只读,子代理不能反倒写得了。
#[test]
fn sandbox_bind_clear_and_readonly_reset_each_other() {
    let temp = tempfile::tempdir().unwrap();
    let store = StateStore::new(&test_paths(temp.path())).unwrap();
    let session = store.create_session("miyu", "s", "user", None).unwrap();
    let id = session.session_id.as_str();
    let record = |store: &StateStore| store.session_record(id).unwrap().unwrap();
    assert!(!record(&store).sandbox_opt_out && !record(&store).sandbox_readonly);

    store.set_session_sandbox_readonly(id, true).unwrap();
    assert!(record(&store).sandbox_readonly);

    store.set_session_sandbox(id, None, false).unwrap();
    let cleared = record(&store);
    assert!(cleared.sandbox_opt_out, "解绑 = 明确不要沙盒");
    assert!(!cleared.sandbox_readonly, "解绑把只读一起清掉");

    store.set_session_sandbox_readonly(id, true).unwrap();
    store.set_session_sandbox(id, Some("/tmp"), true).unwrap();
    let bound = record(&store);
    assert_eq!(bound.sandbox.as_deref(), Some("/tmp"));
    assert!(bound.sandbox_read_all);
    assert!(!bound.sandbox_opt_out, "绑定撤回「不要沙盒」");
    assert!(!bound.sandbox_readonly, "绑定把只读清掉");

    store.set_session_sandbox_readonly(id, true).unwrap();
    let child = store.create_session("miyu", "child", "user", None).unwrap();
    store
        .copy_session_sandbox(&record(&store), &child.session_id)
        .unwrap();
    let copied = store.session_record(&child.session_id).unwrap().unwrap();
    assert_eq!(copied.sandbox.as_deref(), Some("/tmp"));
    assert!(copied.sandbox_read_all && copied.sandbox_readonly);
}
