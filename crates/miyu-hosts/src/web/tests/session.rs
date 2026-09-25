//! 会话切换、重置、人格与模型覆盖。

use super::shared::*;
use crate::runtime::{LOGIN_ATTEMPT_LIMIT, MAX_TRACKED_LOGIN_PEERS};
use crate::web::*;
use miyu_core::state::PlatformSessionBindingKey;

#[test]
fn managed_persona_assets_use_the_resource_directory_and_reject_escape() {
    let temp = tempfile::tempdir().unwrap();
    let mut paths = test_paths(temp.path());
    paths.skills_dir = paths.data_dir.join("skills");
    paths.scripts_dir = paths.data_dir.join("scripts");

    assert_eq!(
        managed_persona_asset_path(&paths, "persona-avatars/avatar.png"),
        Some(paths.data_dir.join("persona-avatars/avatar.png"))
    );
    assert!(managed_persona_asset_path(&paths, "/etc/passwd").is_none());
    assert!(managed_persona_asset_path(&paths, "persona-avatars/../secret").is_none());
    assert_eq!(
        managed_persona_asset_path(&paths, "persona-avatars/nested/file.png"),
        Some(paths.data_dir.join("persona-avatars/nested/file.png"))
    );
    assert_eq!(
        resolve_persona_asset_path(&paths, "./persona-avatars/avatar.png"),
        Some(paths.data_dir.join("persona-avatars/avatar.png"))
    );
    assert!(resolve_persona_asset_path(&paths, "persona-avatars/../../secret").is_none());
    assert_eq!(
        resolve_persona_asset_path(&paths, "avatars/custom.png"),
        Some(paths.config_dir.join("avatars/custom.png"))
    );
    assert_eq!(
        resolve_persona_asset_path(&paths, "scripts/images/custom.png"),
        Some(paths.data_dir.join("scripts/images/custom.png"))
    );
    assert_eq!(
        resolve_persona_asset_path(
            &paths,
            &paths
                .config_dir
                .join("persona-avatars/absolute.png")
                .display()
                .to_string(),
        ),
        Some(paths.data_dir.join("persona-avatars/absolute.png"))
    );
}

#[test]
fn persona_asset_cleanup_normalizes_managed_reference_paths() {
    fn prompts(path: String) -> PromptDocuments {
        PromptDocuments {
            personas: vec![PromptDocument {
                name: "Persona.md".to_string(),
                content: String::new(),
                avatar_path: Some(path),
                board_image_path: None,
                board_title: None,
                board_subtitle: None,
                composer_placeholder: None,
                starter_prompts: None,
                original_name: None,
            }],
            identities: Vec::new(),
        }
    }

    let temp = tempfile::tempdir().unwrap();
    let mut paths = test_paths(temp.path());
    paths.skills_dir = paths.data_dir.join("skills");
    let directory = paths.persona_avatars_dir();
    std::fs::create_dir_all(&directory).unwrap();
    let name = format!("{}.png", "a".repeat(64));
    let asset = directory.join(&name);
    std::fs::write(&asset, "image").unwrap();

    cleanup_persona_assets(
        &paths,
        &prompts(format!("persona-avatars/{name}")),
        &prompts(format!("./persona-avatars/{name}")),
    );
    assert!(asset.is_file());
}

#[cfg(unix)]
#[test]
fn managed_persona_asset_validation_rejects_symlink_escape() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().unwrap();
    let mut paths = test_paths(temp.path());
    paths.skills_dir = paths.data_dir.join("skills");
    let directory = paths.persona_avatars_dir();
    std::fs::create_dir_all(&directory).unwrap();
    let outside = temp.path().join("outside.png");
    std::fs::write(&outside, "image").unwrap();
    let managed = directory.join("avatar.png");
    symlink(&outside, &managed).unwrap();

    assert!(validate_managed_persona_asset_file(&paths, &managed).is_err());
}

#[test]
fn target_session_state_does_not_move_the_default_session() {
    let temp = tempfile::tempdir().unwrap();
    let state = DaemonState::for_test(test_paths(temp.path()), 8300).unwrap();
    let persona = active_persona_scope(&state);
    state
        .state_store
        .adopt_sessions_for_persona(&persona)
        .unwrap();
    let default_session_id = state.state_store.session_id();
    let local = state
        .state_store
        .create_session(&persona, "repl local", "user", None)
        .unwrap();

    let snapshot = session_state_for(&state, &local.session_id).unwrap();

    assert_eq!(snapshot.session_id, local.session_id);
    assert_eq!(&*state.state_store.session_id(), &*default_session_id);
}

/// daemon 冷启动后，当前会话的 footer 上下文不能是 0。
///
/// `cold_context` 曾经把 `tokens` 硬编码成 0，而 `session_state_for` 对「当前
/// 会话」直接读 `manager.context` 这份内存快照、不重算——于是退出 REPL 再进去，
/// 首帧显示 `0/168k`，要对话一次才恢复。会话里明明躺着几万 token 的历史。
#[test]
fn cold_started_daemon_reports_a_nonzero_context_for_the_current_session() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let store = StateStore::new(&paths).unwrap();
    store.init_files().unwrap();
    // 先攒出一段够长的历史，让它明显盖过系统提示词本身的量。
    for index in 0..8 {
        let turn_id = format!("turn-{index}");
        store
            .start_turn(
                &turn_id,
                &"用户说了很长一段话。".repeat(40),
                std::process::id(),
            )
            .unwrap();
        store
            .complete_turn(&turn_id, &"助手也回了很长一段。".repeat(40), None)
            .unwrap();
    }

    // 这一步就是「daemon 冷启动」：没有活的 Agent，只有配置和状态库。
    let config = miyu_base::config::AppConfig::default();
    let cold = cold_context(&config, &paths, &store).unwrap();
    assert!(
        cold.tokens > 0,
        "冷启动上下文是 {}，footer 会显示 0",
        cold.tokens
    );

    // 而且要和「有 Agent 时」算出来的是同一个数——两条路口径不一致的话，
    // 切个会话数字就跳。
    let live = build_session_agent(&config, &paths, &store, PersonaLane::Active).unwrap();
    assert_eq!(
        cold.tokens,
        live.effective_context_tokens().unwrap(),
        "冷路径与热路径口径不一致"
    );
}

#[test]
fn local_session_resolution_rejects_platform_ids_and_prefers_local_names() {
    let temp = tempfile::tempdir().unwrap();
    let state = DaemonState::for_test(test_paths(temp.path()), 8300).unwrap();
    let persona = active_persona_scope(&state);
    state
        .state_store
        .adopt_sessions_for_persona(&persona)
        .unwrap();
    let local = state
        .state_store
        .create_session(&persona, "shared", "user", None)
        .unwrap();
    let platform = state
        .state_store
        .create_session(&persona, "shared", "user", None)
        .unwrap();
    state
        .state_store
        .bind_platform_session(
            &PlatformSessionBindingKey {
                platform: "onebot".to_string(),
                account_id: "10000".to_string(),
                conversation_kind: "private".to_string(),
                conversation_id: "20000".to_string(),
                participant_id: Some("20000".to_string()),
                persona,
            },
            &platform.session_id,
        )
        .unwrap();

    let resolved = resolve_local_session_ref(
        &state,
        &ipc::SessionRef::Name {
            name: "SHARED".to_string(),
        },
    )
    .unwrap();
    assert_eq!(resolved.session_id, local.session_id);
    assert!(resolve_local_session_ref(
        &state,
        &ipc::SessionRef::Id {
            id: platform.session_id,
        },
    )
    .is_err());
}

#[test]
fn startup_repairs_a_platform_owned_current_session() {
    let temp = tempfile::tempdir().unwrap();
    let store = StateStore::new(&test_paths(temp.path())).unwrap();
    store.adopt_sessions_for_persona("miyu").unwrap();
    let qq_session = store
        .create_session("miyu", "QQ group 20000", "user", None)
        .unwrap();
    store
        .bind_platform_session(
            &PlatformSessionBindingKey {
                platform: "onebot".to_string(),
                account_id: "10000".to_string(),
                conversation_kind: "group".to_string(),
                conversation_id: "20000".to_string(),
                participant_id: None,
                persona: "miyu".to_string(),
            },
            &qq_session.session_id,
        )
        .unwrap();
    store.switch_session(&qq_session.session_id).unwrap();

    ensure_local_current_session(&store, "miyu").unwrap();

    let repaired = store.session_id();
    assert_ne!(&*repaired, qq_session.session_id);
    assert!(!store.is_platform_session(&repaired).unwrap());
    assert_eq!(
        store.session_record(&repaired).unwrap().unwrap().persona,
        "miyu"
    );
}

#[test]
fn persona_file_mutations_include_avatar_sidecar() {
    let temp = tempfile::tempdir().unwrap();
    let mut mutations = HashMap::new();
    let documents = vec![PromptDocument {
        name: "Alice.md".to_string(),
        content: "prompt".to_string(),
        avatar_path: Some("avatars/alice.png".to_string()),
        board_image_path: None,
        board_title: None,
        board_subtitle: None,
        composer_placeholder: None,
        starter_prompts: None,
        original_name: None,
    }];
    collect_prompt_file_mutations(
        &[],
        &documents,
        temp.path(),
        temp.path(),
        &mut mutations,
        true,
    );

    let metadata = mutations
        .get(&temp.path().join("Alice.json"))
        .and_then(Option::as_deref)
        .unwrap();
    let metadata: Value = serde_json::from_slice(metadata).unwrap();
    assert_eq!(metadata["avatar_path"], "avatars/alice.png");
}

/// 看板/输入框默认文案现在跟着界面语言走(`dto.rs` 的 `t(...)`),而界面语言是
/// 进程级(测试里没有请求作用域)——CI 的 LANG 一换,原来只认中文的断言就红。
/// 中文那份照旧逐字钉;英文那份只要求命中同一句的英文版,精确措辞由
/// `tests/http.rs::server_text_follows_request_locale_scope` 与
/// `testkit/webui-i18n` 走查钉住(2026-09-23)。
fn assert_placeholder(actual: &str, zh: &str, en: &str) {
    assert!(
        actual == zh || actual == en,
        "输入框默认文案应为 {zh:?} 或 {en:?},实际 {actual:?}"
    );
}

#[test]
fn persona_identity_uses_default_and_custom_values() {
    let mut config = AppConfig::default();
    let prompts = PromptDocuments::default();
    let default = persona_identity(&config, &prompts);
    assert_eq!(default.name, "Miyu");
    assert_eq!(default.avatar_url.as_deref(), Some("/assets/miyu-logo.png"));
    assert_placeholder(
        &default.composer_placeholder,
        "给 Miyu 发消息",
        "Message Miyu",
    );

    config.prompt.active_persona = "Alice.md".to_string();
    let prompts = PromptDocuments {
        personas: vec![PromptDocument {
            name: "Alice.md".to_string(),
            content: "prompt".to_string(),
            avatar_path: Some("avatars/alice.png".to_string()),
            board_image_path: None,
            board_title: None,
            board_subtitle: None,
            composer_placeholder: None,
            starter_prompts: None,
            original_name: None,
        }],
        identities: Vec::new(),
    };
    let custom = persona_identity(&config, &prompts);
    assert_eq!(custom.name, "Alice");
    assert_eq!(custom.avatar_url.as_deref(), Some("/api/persona/avatar"));
    // 没配就跟着人格名走——此前这句写死在 index.html 里,改了人格名输入框
    // 还留着 "给 Miyu 发消息"。
    assert_placeholder(
        &custom.composer_placeholder,
        "给 Alice 发消息",
        "Message Alice",
    );

    // 配了就用配的;空白串不算配置(与看板文案同一把尺)。
    let with_placeholder = |value: Option<&str>| {
        let prompts = PromptDocuments {
            personas: vec![PromptDocument {
                name: "Alice.md".to_string(),
                content: "prompt".to_string(),
                avatar_path: None,
                board_image_path: None,
                board_title: None,
                board_subtitle: None,
                composer_placeholder: value.map(str::to_string),
                starter_prompts: None,
                original_name: None,
            }],
            identities: Vec::new(),
        };
        persona_identity(&config, &prompts).composer_placeholder
    };
    assert_eq!(with_placeholder(Some("说点什么…")), "说点什么…");
    assert_placeholder(
        &with_placeholder(Some("   ")),
        "给 Alice 发消息",
        "Message Alice",
    );
    assert_placeholder(&with_placeholder(None), "给 Alice 发消息", "Message Alice");
}

#[test]
fn sanitize_session_title_cleans_llm_output() {
    assert_eq!(sanitize_session_title("「东京天气查询」"), "东京天气查询");
    assert_eq!(
        sanitize_session_title("\"Arch Linux 新闻\"\n第二行忽略"),
        "Arch Linux 新闻"
    );
    assert_eq!(sanitize_session_title("  标题。  "), "标题");
    assert_eq!(sanitize_session_title(""), "");
    // Overlong output clips to 20 chars.
    let long = "很长的标题".repeat(10);
    assert_eq!(sanitize_session_title(&long).chars().count(), 20);
}

#[test]
fn optional_password_auth_issues_server_side_sessions_and_limits_failures() {
    let disabled = WebAuth::new(None);
    assert!(disabled.is_authenticated(None));

    let auth = WebAuth::new(Some("correct horse"));
    let peer = IpAddr::V4(Ipv4Addr::LOCALHOST);
    assert!(!auth.is_authenticated(None));
    assert!(matches!(
        auth.login(peer, "wrong"),
        Err(LoginFailure::Invalid)
    ));
    let token = auth.login(peer, "correct horse").unwrap();
    assert!(auth.is_authenticated(Some(&token)));

    let limited = WebAuth::new(Some("secret"));
    for _ in 0..LOGIN_ATTEMPT_LIMIT {
        assert!(matches!(
            limited.login(peer, "wrong"),
            Err(LoginFailure::Invalid)
        ));
    }
    assert!(matches!(
        limited.login(peer, "secret"),
        Err(LoginFailure::RateLimited)
    ));
}

#[test]
fn model_selection_rejects_empty_and_duplicate_pools() {
    assert!(validate_model_selection(Vec::new()).is_err());
    let model = ActiveProviderModelConfig {
        provider_id: "provider".to_string(),
        model: "model".to_string(),
    };
    assert!(validate_model_selection(vec![model.clone()]).is_ok());
    assert!(validate_model_selection(vec![model.clone(), model]).is_err());
}

#[test]
fn thinking_variant_validation_distinguishes_model_default_and_named_default() {
    let updates = validate_thinking_variant_updates(vec![
        ThinkingVariantUpdate {
            provider_id: " provider ".to_string(),
            model: "model-one".to_string(),
            selected: None,
        },
        ThinkingVariantUpdate {
            provider_id: "provider".to_string(),
            model: "model-two".to_string(),
            selected: Some(" default ".to_string()),
        },
    ])
    .unwrap();
    assert_eq!(updates[0].provider_id, "provider");
    assert_eq!(updates[0].selected, None);
    assert_eq!(updates[1].selected.as_deref(), Some("default"));

    assert!(validate_thinking_variant_updates(vec![
        ThinkingVariantUpdate {
            provider_id: "provider".to_string(),
            model: "model".to_string(),
            selected: None,
        },
        ThinkingVariantUpdate {
            provider_id: " provider ".to_string(),
            model: " model ".to_string(),
            selected: Some("high".to_string()),
        },
    ])
    .is_err());
}

#[test]
fn thinking_variant_updates_validate_before_persisting_and_can_clear_a_selection() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let config = AppConfig::default();
    let choice = config
        .active_provider_model_choices()
        .into_iter()
        .next()
        .unwrap();
    let mut preferences = ThinkingVariantPreferences::load(&paths);
    preferences.set(
        &choice.provider_id,
        &choice.model,
        Some("previous-selection".to_string()),
    );
    preferences.save(&paths).unwrap();

    let mut agent = None;
    let invalid = ThinkingVariantUpdate {
        provider_id: choice.provider_id.clone(),
        model: choice.model.clone(),
        selected: Some("definitely-not-a-real-variant".to_string()),
    };
    assert!(matches!(
        apply_thinking_variant_updates(&mut agent, &config, &paths, &[invalid]),
        Err(AdminFailure::Invalid(_))
    ));
    assert_eq!(
        ThinkingVariantPreferences::load(&paths).selected(&choice.provider_id, &choice.model),
        Some("previous-selection")
    );

    let clear = ThinkingVariantUpdate {
        provider_id: choice.provider_id.clone(),
        model: choice.model.clone(),
        selected: None,
    };
    apply_thinking_variant_updates(&mut agent, &config, &paths, &[clear]).unwrap();
    assert_eq!(
        ThinkingVariantPreferences::load(&paths).selected(&choice.provider_id, &choice.model),
        None
    );
}

#[test]
fn web_persona_rename_updates_qq_routes_and_deletion_is_rejected() {
    let mut config = AppConfig::default();
    config
        .platforms
        .qq
        .conversations
        .push(miyu_base::config::PlatformModelRoute {
            conversation: miyu_base::config::PlatformConversationConfig {
                kind: miyu_base::config::PlatformConversationKind::Group,
                id: "42".to_string(),
            },
            persona: miyu_base::config::PlatformPersonaOverride::Custom {
                name: "Old.md".to_string(),
            },
            text_models_inheritance: miyu_base::config::PlatformModelPoolInheritance::Platform,
            text_models: None,
            multimodal_models_inheritance:
                miyu_base::config::PlatformModelPoolInheritance::Platform,
            multimodal_models: None,
            extra_prompt: String::new(),
            session_limits: None,
            probability_reply: None,
            probability_reply_rate: None,
            ignore_sleep_hours: None,
            rate_limit: None,
        });
    let renamed: PromptDocuments = serde_json::from_value(json!({
        "personas": [{
            "name": "New.md",
            "content": "persona",
            "original_name": "Old.md"
        }],
        "identities": []
    }))
    .unwrap();

    reconcile_qq_persona_references(&mut config, &renamed);
    assert_eq!(
        config.platforms.qq.conversations[0].persona.custom_name(),
        Some("New.md")
    );
    assert!(validate_prompt_documents(&config, &renamed).is_ok());
    assert!(validate_prompt_documents(&config, &PromptDocuments::default()).is_err());
}

#[test]
fn web_persona_renames_use_the_original_reference_snapshot() {
    let route = |id: &str, persona: &str| miyu_base::config::PlatformModelRoute {
        conversation: miyu_base::config::PlatformConversationConfig {
            kind: miyu_base::config::PlatformConversationKind::Group,
            id: id.to_string(),
        },
        persona: miyu_base::config::PlatformPersonaOverride::Custom {
            name: persona.to_string(),
        },
        text_models_inheritance: miyu_base::config::PlatformModelPoolInheritance::Platform,
        text_models: None,
        multimodal_models_inheritance: miyu_base::config::PlatformModelPoolInheritance::Platform,
        multimodal_models: None,
        extra_prompt: String::new(),
        session_limits: None,
        probability_reply: None,
        probability_reply_rate: None,
        ignore_sleep_hours: None,
        rate_limit: None,
    };
    let mut config = AppConfig::default();
    config.platforms.qq.conversations = vec![route("1", "A.md"), route("2", "B.md")];
    let prompts: PromptDocuments = serde_json::from_value(json!({
        "personas": [
            {"name": "B.md", "content": "A", "original_name": "A.md"},
            {"name": "C.md", "content": "B", "original_name": "B.md"}
        ],
        "identities": []
    }))
    .unwrap();

    reconcile_qq_persona_references(&mut config, &prompts);

    assert_eq!(
        config.platforms.qq.conversations[0].persona.custom_name(),
        Some("B.md")
    );
    assert_eq!(
        config.platforms.qq.conversations[1].persona.custom_name(),
        Some("C.md")
    );
}

#[test]
fn web_rejects_persona_names_with_colliding_persistent_scopes() {
    let prompts: PromptDocuments = serde_json::from_value(json!({
        "personas": [
            {"name": "A B.md", "content": "first"},
            {"name": "A@B.md", "content": "second"}
        ],
        "identities": []
    }))
    .unwrap();

    assert!(validate_prompt_documents(&AppConfig::default(), &prompts).is_err());
}

#[test]
fn web_persona_scope_batch_migration_supports_swaps() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let store = StateStore::new(&paths).unwrap();
    let first = store.create_session("a", "first", "user", None).unwrap();
    let second = store.create_session("b", "second", "user", None).unwrap();

    migrate_persona_db_scopes(
        &store,
        &[
            ("a".to_string(), "b".to_string()),
            ("b".to_string(), "a".to_string()),
        ],
    )
    .unwrap();

    assert_eq!(
        store
            .session_record(&first.session_id)
            .unwrap()
            .unwrap()
            .persona,
        "b"
    );
    assert_eq!(
        store
            .session_record(&second.session_id)
            .unwrap()
            .unwrap()
            .persona,
        "a"
    );
}

/// 限流表原来只增不删：IPv6 一个 /64 有 2⁶⁴ 个地址，轮换着打就能把它撑爆。
/// 这是暴露在网络侧的内存耗尽入口。
///
/// 归并到 /64 之后，轮换地址不再产生新条目——同一个 /64 打一万次也只占一格，
/// 而且限流本身更严：它们共用一个计数桶，第 6 次就被挡。
#[test]
fn ipv6_rotation_cannot_grow_the_rate_limit_table() {
    let auth = WebAuth::new(Some("correct horse"));
    for index in 0..10_000u16 {
        // 同一个 /64，只有低 64 位在变
        let peer = IpAddr::V6(std::net::Ipv6Addr::new(
            0x2001,
            0xdb8,
            0,
            0,
            0,
            0,
            index >> 8,
            index & 0xff,
        ));
        let _ = auth.login(peer, "wrong");
    }
    assert_eq!(
        auth.tracked_login_peers(),
        1,
        "同一个 /64 的一万次尝试应当只占一格"
    );
}

/// 不同的 /64 仍然各记各的——归并不能把限流粒度粗到没用。
#[test]
fn different_ipv6_prefixes_are_tracked_separately() {
    let auth = WebAuth::new(Some("correct horse"));
    for prefix in 0..8u16 {
        let peer = IpAddr::V6(std::net::Ipv6Addr::new(
            0x2001, 0xdb8, prefix, 0, 0, 0, 0, 1,
        ));
        let _ = auth.login(peer, "wrong");
    }
    assert_eq!(auth.tracked_login_peers(), 8);
}

/// 表满了之后不能把「正在被限的记录」挤掉——那等于给攻击者一条重置计数的路。
#[test]
fn a_flood_of_new_peers_cannot_reset_an_active_block() {
    let auth = WebAuth::new(Some("correct horse"));
    let victim = IpAddr::V4(std::net::Ipv4Addr::new(203, 0, 113, 7));
    for _ in 0..LOGIN_ATTEMPT_LIMIT {
        let _ = auth.login(victim, "wrong");
    }
    assert!(matches!(
        auth.login(victim, "wrong"),
        Err(LoginFailure::RateLimited)
    ));

    // 拿远超容量的新来源猛灌
    for index in 0..(MAX_TRACKED_LOGIN_PEERS as u32 + 500u32) {
        let peer = IpAddr::V4(std::net::Ipv4Addr::from(index.to_be_bytes()));
        let _ = auth.login(peer, "wrong");
    }
    assert!(auth.tracked_login_peers() <= MAX_TRACKED_LOGIN_PEERS);
    // 被限的那个仍然被限，密码对了也不放行（窗口内）
    assert!(matches!(
        auth.login(victim, "correct horse"),
        Err(LoginFailure::RateLimited)
    ));
}

/// 重置对话要连待办一起清。
///
/// 待办不在 `clear_session_content` 清的那几张表里，加清理项时天然会漏掉它，
/// 于是「对话重来了，上一轮的待办还挂在侧边面板上，模型下一次读 todo 也还是
/// 旧的」。清空会话内容（平台会话与 WebUI 的「清空」）同理，两条都钉住。
/// 清单从老文件（`todos/{session}.json`，09-24 入库前的存法）导进来，顺带钉住
/// 老文件的导入与清除。
#[test]
fn resetting_a_conversation_also_clears_its_todo_list() {
    for clear_only in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let state = DaemonState::for_test(paths.clone(), 8330).unwrap();
        let config = state.manager.lock().unwrap().config.clone();
        let session_id = state.state_store.session_id().to_string();

        let todos = paths.state_dir.join("todos");
        std::fs::create_dir_all(&todos).unwrap();
        std::fs::write(
            todos.join(format!("{session_id}.json")),
            r#"[{"content":"上一轮的活","status":"pending","priority":"high"}]"#,
        )
        .unwrap();
        assert!(
            !miyu_engine::tools::session_todos(&state.state_store, &session_id).is_empty(),
            "前置条件不成立：清单没写进去"
        );

        let mut agent = None;
        let result = if clear_only {
            clear_actor_session_content(
                &mut agent,
                &config,
                &paths,
                &state.state_store,
                &state.manager,
                &session_id,
            )
        } else {
            reset_actor_conversation(
                &mut agent,
                &config,
                &paths,
                &state.state_store,
                &state.manager,
                &state.events,
                &session_id,
            )
        };
        result.unwrap();

        assert!(
            miyu_engine::tools::session_todos(&state.state_store, &session_id).is_empty(),
            "{}后待办还在——面板和模型看到的都是上一轮的清单",
            if clear_only {
                "清空会话"
            } else {
                "重置对话"
            }
        );
    }
}

/// `session.created` 事件必须带上 mode。
///
/// 会话模式有两个发布口：REST 的会话对象和这个事件。前端收到事件就把会话插
/// 进列表了，此后 HTTP 响应会因为「已存在」被跳过——事件里少一个字段，新建的
/// dev 会话就一直挂在「普通模式」组下，直到刷新走 `/api/sessions` 才纠正。
/// 两个口同源于 `session_mode_label`，这里钉住事件那一路。
#[tokio::test]
async fn session_created_event_carries_the_mode() {
    let temp = tempfile::tempdir().unwrap();
    let state = DaemonState::for_test(test_paths(temp.path()), 8331).unwrap();
    // 从当前位置起订：`subscribe_after` 会把此后发布的事件重放出来，不会
    // 因为「发布发生在 recv 之前」而漏掉。
    let after = state.events.latest_id();

    let payload = handle_session_command(
        &state,
        IpcCommand::CreateSession {
            name: Some("dev 会话".to_string()),
            switch: false,
            kind: None,
            mode: Some("dev".to_string()),
        },
    )
    .await
    .unwrap();
    assert_eq!(payload["session"]["mode"], "dev", "REST 那一路就没带 mode");

    let record = state
        .events
        .replay_after(after)
        .into_iter()
        .find(|event| event.kind == "session.created")
        .map(|event| serde_json::from_str::<serde_json::Value>(&event.data).unwrap())
        .expect("没有发出 session.created 事件");
    assert_eq!(
        record["mode"], "dev",
        "事件里没有 mode——前端会把这个 dev 会话分到普通模式组"
    );
}

/// 打断一个自主轮之后，目标不该自己接着跑。
///
/// 实测：Ctrl+C 掐掉一轮，退出 REPL 再进来，它又在跑了——取消只停了那一个
/// run，武装标记还在，驱动器转头就认领了下一轮。人按下停止就是明确说停，
/// 要接着跑得 `/goal resume`。
///
/// REPL 走 IPC、WebUI 走 HTTP，两条路共用 `cancel_run_and_disarm_goal`，
/// 这里钉的是那个函数。
#[tokio::test]
async fn cancelling_an_autonomous_round_disarms_the_goal() {
    let temp = tempfile::tempdir().unwrap();
    let state = DaemonState::for_test(test_paths(temp.path()), 8332).unwrap();
    let session_id = state.state_store.session_id().to_string();
    let goal = state
        .state_store
        .create_goal(&session_id, "长任务", Some(9))
        .unwrap();
    miyu_engine::tools::goal::set_armed(&session_id, true);

    // 登记一个「正在跑的自主轮」。
    let run_id = "run_test_goal_round".to_string();
    let (cancel_tx, _cancel_rx) = tokio::sync::watch::channel(false);
    state.manager.lock().unwrap().active_runs.insert(
        run_id.clone(),
        RunInfo {
            session_id: session_id.clone().into(),
            mode: PersonaLane::Active,
            audience: PromptAudience::Owner,
            cancel: cancel_tx,
            turn_id: None,
            queue_target: None,
            supersede: std::sync::Arc::new(miyu_engine::agent::TurnSupersedeSignal::default()),
            platform_followup: None,
            operation: RunOperation::Create,
            job_wake: true,
            turn_origin: miyu_base::workspace::TurnOrigin::GoalRound {
                goal_id: goal.goal_id.clone(),
                revision: goal.revision,
                round: 1,
            },
            job_wake_label: None,
            first_event_id: None,
        },
    );

    assert!(
        cancel_run_and_disarm_goal(&state, &run_id),
        "没找到那个 run"
    );
    assert!(
        !miyu_engine::tools::goal::is_armed(&session_id),
        "取消之后还武装着——驱动器会转头开下一轮"
    );
    // 目标本身还在，阶段不动：人可以 /goal resume 接着跑。
    let after = state.state_store.goal(&session_id).unwrap().unwrap();
    assert_eq!(after.phase, miyu_core::state::GoalPhase::Active);
}

/// 工具桥寻址平台会话(08-26):没有活回合时照旧拒绝(桥不该在回合外碰平台
/// 会话),回合在跑时放行——这是 claude-code 供应商在群聊里拿到平台工具的
/// 唯一入口,实测此前 `tool-call --list` 一律报"找不到该会话"。
#[test]
fn tool_bridge_addresses_platform_sessions_only_during_a_live_turn() {
    let temp = tempfile::tempdir().unwrap();
    let state = DaemonState::for_test(test_paths(temp.path()), 8300).unwrap();
    let persona = active_persona_scope(&state);
    let platform = state
        .state_store
        .create_session(&persona, "群会话", "user", None)
        .unwrap();
    state
        .state_store
        .bind_platform_session(
            &PlatformSessionBindingKey {
                platform: "onebot".to_string(),
                account_id: "10000".to_string(),
                conversation_kind: "group".to_string(),
                conversation_id: "130515298".to_string(),
                participant_id: None,
                persona,
            },
            &platform.session_id,
        )
        .unwrap();
    let target = ipc::SessionRef::Id {
        id: platform.session_id.clone(),
    };

    assert!(
        resolve_tool_bridge_session_ref(&state, &target).is_err(),
        "回合外不应放行平台会话"
    );

    let (_temp2, context, _adapter) = crate::platforms::tests::shared::test_turn_context(true);
    let context = std::sync::Arc::new(context);
    let _live = crate::platforms::LiveTurnGuard::register(&platform.session_id, &context);
    let resolved = resolve_tool_bridge_session_ref(&state, &target).expect("活回合应放行");
    assert_eq!(resolved.session_id, platform.session_id);
}

/// 桥给平台会话的工具面必须与平台回合同源(08-26 审查抓到:桥原来发的是
/// owner 面全量 registry,非管理员群友经它就能调 run_command / claude_code)。
/// 退回 apply_platform_turn_scope 之前,这两条断言会拿到宿主工具而报红。
#[test]
fn tool_bridge_scopes_platform_tools_like_a_real_turn() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let mut config = AppConfig::default();
    config.tools.enabled = true;
    // 非管理员群友:受限底座,宿主工具一律没有。
    let (_temp2, guest, _adapter) = crate::platforms::tests::shared::test_turn_context(false);
    let guest = std::sync::Arc::new(guest);
    assert!(!guest.host_tools_allowed());
    let mut registry =
        miyu_engine::tools::build_tool_registry(&config, &paths, PersonaLane::Active, false)
            .unwrap();
    assert!(registry.contains("run_command"), "底座本应有宿主工具");
    crate::platforms::apply_platform_turn_scope(&mut registry, &config, &paths, &guest, None);
    assert!(
        !registry.contains("run_command"),
        "非管理员会话不得有 run_command"
    );
    assert!(!registry.contains("edit"), "非管理员会话不得有写盘工具");

    // 管理员会话:保留底座(claude_code 委托工具 08-21 已删除,收口函数里
    // 那条 unregister 是它万一回归时的常备闸,当下无从断言)。
    // 注意 test_turn_context 的入参是适配器的 fail_first,不是管理员标志——
    // 管理员身份必须显式设(08-26:我第一版把它当管理员用,断言等于没测)。
    let (_temp3, mut admin, _adapter2) = crate::platforms::tests::shared::test_turn_context(true);
    admin.is_admin = true;
    let admin = std::sync::Arc::new(admin);
    assert!(admin.host_tools_allowed(), "管理员应放行宿主工具");
    let mut registry =
        miyu_engine::tools::build_tool_registry(&config, &paths, PersonaLane::Active, false)
            .unwrap();
    crate::platforms::apply_platform_turn_scope(&mut registry, &config, &paths, &admin, None);
    assert!(registry.contains("run_command"), "管理员会话保留底座");

    // 受限底座可由调用方复用(回合路径传缓存,不每轮重建):传进来的那份就是
    // 最终工具面——两条路共用同一个收口函数,口径不会再各写各的。
    let mut cached = miyu_engine::tools::restricted_platform_registry(&config, &paths);
    cached.unregister("read_file");
    let mut registry =
        miyu_engine::tools::build_tool_registry(&config, &paths, PersonaLane::Active, false)
            .unwrap();
    crate::platforms::apply_platform_turn_scope(
        &mut registry,
        &config,
        &paths,
        &guest,
        Some(&cached),
    );
    assert!(!registry.contains("read_file"), "应采用调用方给的受限底座");
}

/// 桥的工具面必须带上作用域看图(08-26 用户"图我看不了")。真实回合是在
/// `agent/input.rs` 准备输入时注册 `vision_analyze` 的,桥另建工具面走不到
/// 那里——于是 claude-code 供应商拿到的面里根本没有它,群上下文里的图只给了
/// id 却没有任何手段去看。顺带钉住同一处的越权口子:受限底座注册的
/// `generate_image` 参考图解析器不受限,作用域版本必须把它换掉。
#[test]
fn platform_tool_face_carries_scoped_vision() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let mut config = AppConfig::default();
    config.tools.enabled = true;
    config.plugins.vision.enabled = true;

    let (_temp2, guest, _adapter) = crate::platforms::tests::shared::test_turn_context(false);
    let guest = std::sync::Arc::new(guest);
    assert!(!guest.host_tools_allowed());
    // 本轮群里有一张可看的图。
    guest.set_context_images(vec![crate::platforms::PlatformContextImageRef {
        id: "context_image_1".to_string(),
        message_id: "m1".to_string(),
        image_index: 1,
    }]);

    let mut registry = miyu_engine::tools::restricted_platform_registry(&config, &paths);
    assert!(
        !registry.contains("vision_analyze"),
        "受限底座本来就没有看图工具——这正是病灶"
    );
    miyu_engine::tools::vision::register_scoped_platform(
        &mut registry,
        config.clone(),
        paths.clone(),
        Vec::new(),
        guest.context_images(),
        guest.context_files(),
        guest.clone(),
    );
    assert!(registry.contains("vision_analyze"), "桥的工具面必须能看图");
    // 非管理员只认已入库的 context_image_N,拿不到宿主任意路径。
    assert!(!guest.host_tools_allowed());
}

#[test]
fn session_model_override_is_applied_to_the_target_session_config() {
    let temp = tempfile::tempdir().unwrap();
    let state = DaemonState::for_test(test_paths(temp.path()), 8300).unwrap();
    let persona = active_persona_scope(&state);
    let record = state
        .state_store
        .create_session(&persona, "", "user", None)
        .unwrap();
    let session_id = record.session_id.clone();
    // 钉的得是配置里真有的那一对：钉一个不存在的现在会被当成失效覆盖清掉
    // （见 `a_stale_session_model_override_falls_back_to_the_global_pool`）。
    let pinned = usable_pin(&state.manager.lock().unwrap().config.clone());
    state
        .state_store
        .set_session_model_override(&session_id, Some(std::slice::from_ref(&pinned)))
        .unwrap();

    let mut config = state.manager.lock().unwrap().config.clone();
    apply_session_model_override_to(&mut config, &state.state_store, &session_id);
    let applied = config
        .active_provider_models
        .as_ref()
        .expect("the override is applied to the config");
    assert_eq!(applied.len(), 1);
    assert_eq!(applied[0].provider_id, pinned.provider_id);
    assert_eq!(applied[0].model, pinned.model);

    // 没有覆盖时不改这份 config（调用方每次都是从全局配置克隆出来的）。
    state
        .state_store
        .set_session_model_override(&session_id, None)
        .unwrap();
    let global = state.manager.lock().unwrap().config.clone();
    let mut fresh = global.clone();
    apply_session_model_override_to(&mut fresh, &state.state_store, &session_id);
    assert_eq!(fresh.active_provider_models, global.active_provider_models);
}

/// `--allow-read`:只读侧塌成一条 `/`(Landlock 是 allow-list,这一条就是全盘),
/// 可写侧一个字不动;不给这个开关时读侧仍是逐条放行的清单。
#[test]
fn allow_read_opens_reads_without_touching_the_write_side() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let root = temp.path().join("proj");
    std::fs::create_dir_all(&root).unwrap();
    let config = AppConfig::default();

    let locked = admin_scope(&paths, &config, root.clone(), false)
        .policy
        .expect("bound sessions always carry a policy");
    assert!(!locked.read_only.contains(&PathBuf::from("/")));
    assert!(locked.read_only.contains(&PathBuf::from("/usr")));
    assert!(locked
        .readable_summary
        .iter()
        .any(|entry| entry == "system dirs"));

    let open = admin_scope(&paths, &config, root.clone(), true)
        .policy
        .expect("bound sessions always carry a policy");
    assert_eq!(open.read_only, vec![PathBuf::from("/")]);
    assert_eq!(open.readable_summary, vec!["everything (read-only)"]);
    // 写侧与锁读时逐字节相同:这个开关只碰读。
    assert_eq!(open.read_write, locked.read_write);
    assert_eq!(open.writable_summary, locked.writable_summary);
    assert_eq!(open.root, root);
    assert_eq!(open.home, Some(root));
}

/// 「终端集成会话默认模式」：终端车道 = `current_session` 指针。dev 就把「终端集成
/// 会话」换到 dev 人格并把指针指过去（回合模式跟着变成开发），normal 换回来；指针
/// 停在别的普通会话上也要被拉回来；归了别的人格的终端会话不抢，退到自举一条。
#[test]
fn terminal_session_mode_flips_the_terminal_session_and_points_the_lane_at_it() {
    use crate::web::sessions::{apply_terminal_session_mode, turn_mode_for_session};
    use miyu_base::config::{AppConfig, PersonaLane};
    use miyu_core::state::{DEFAULT_SESSION_ID as TERMINAL, DEV_PERSONA};
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let store = miyu_core::state::StateStore::new(&paths).unwrap();
    store.init_files().unwrap();
    let mut config = AppConfig::default();
    let persona = config.active_persona_scope();
    store.adopt_sessions_for_persona(&persona).unwrap();
    let persona_of = |id: &str| store.session_record(id).unwrap().unwrap().persona;

    // 默认 normal：指针在终端集成会话上，普通模式，什么都不动。
    apply_terminal_session_mode(&config, &store).unwrap();
    assert_eq!(&*store.session_id(), TERMINAL);
    assert_eq!(
        turn_mode_for_session(&store, TERMINAL, PersonaLane::Dev),
        PersonaLane::Active,
        "默认是普通模式（客户端要 dev 也不给）"
    );

    // 指针停在另一条普通会话上（换人格会这样），配置改 dev：终端集成会话换到
    // dev 人格，指针拉回来。
    let side = store
        .create_session(&persona, "side", "user", None)
        .unwrap();
    store.switch_session(&side.session_id).unwrap();
    config.terminal_session_mode = "dev".into();
    apply_terminal_session_mode(&config, &store).unwrap();
    assert_eq!(persona_of(TERMINAL), DEV_PERSONA);
    assert_eq!(&*store.session_id(), TERMINAL, "车道指针指回终端集成会话");
    assert_eq!(persona_of(&side.session_id), persona, "别的会话不动");
    assert_eq!(
        turn_mode_for_session(&store, TERMINAL, PersonaLane::Active),
        PersonaLane::Dev
    );
    // 再对一次（daemon 重启）：dev 人格的终端会话不能被当「不可用」撵走。
    apply_terminal_session_mode(&config, &store).unwrap();
    assert_eq!(&*store.session_id(), TERMINAL);
    assert_eq!(persona_of(TERMINAL), DEV_PERSONA);

    // 改回 normal：换回激活人格，指针还在它上面。
    config.terminal_session_mode = "normal".into();
    apply_terminal_session_mode(&config, &store).unwrap();
    assert_eq!(persona_of(TERMINAL), persona);
    assert_eq!(&*store.session_id(), TERMINAL);

    // 终端集成会话归了别的人格：不抢，dev 车道自举一条 dev 人格的会话。
    store.set_session_persona(TERMINAL, "someone-else").unwrap();
    config.terminal_session_mode = "dev".into();
    apply_terminal_session_mode(&config, &store).unwrap();
    assert_eq!(persona_of(TERMINAL), "someone-else");
    let lane = store.session_id();
    assert_ne!(&*lane, TERMINAL);
    assert_eq!(persona_of(&lane), DEV_PERSONA);
}

/// BUG-14(09-18):QQ 里要能「发到别的群/好友」——平台版 `send_qq_message` +
/// `qq_contacts` 装给所有触发者(群友也有,用户裁定;两张脸一致也不掰缓存);开发模式只有
/// `send_qq_message` 且只发管理员;终端版在收口函数里照旧摘掉(它看终端开关)。
/// 09-25 起连没连上都装(掉线在调用时拦):工具表随连接变,会作废所有会话的缓存前缀。
#[test]
fn platform_outreach_tools_go_to_everyone_whether_or_not_qq_is_connected() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let mut config = AppConfig::default();
    config.tools.enabled = true;
    let outreach = miyu_engine::tools::platform_outreach::TOOL_NAME;
    let contacts = miyu_engine::tools::platform_outreach::CONTACTS_TOOL_NAME;

    let (_temp2, guest, _adapter) = crate::platforms::tests::shared::test_turn_context(false);
    let mut registry =
        miyu_engine::tools::build_tool_registry(&config, &paths, PersonaLane::Active, false)
            .unwrap();
    // 终端版先装上(模拟 terminal_outreach 且 QQ 连着的 owner 面)再收口。
    miyu_engine::tools::platform_outreach::register(&mut registry, &config);
    assert!(registry.contains(outreach));
    crate::platforms::apply_platform_turn_scope(&mut registry, &config, &paths, &guest, None);
    assert!(!registry.contains(outreach), "终端版在平台会话里要摘掉");
    crate::platforms::tool::register_outreach(&mut registry, &guest, PersonaLane::Active);
    assert!(registry.contains(outreach), "群友也有平台版，QQ 没连着也在");
    assert!(registry.contains(contacts));
    // 开发模式:只有发 QQ,且收件人是管理员枚举;没有地址簿。
    let mut dev = miyu_engine::tools::ToolRegistry::new();
    crate::platforms::tool::register_outreach(&mut dev, &guest, PersonaLane::Dev);
    assert!(dev.contains(outreach));
    assert!(!dev.contains(contacts), "开发模式没有地址簿工具");
    // 夹具配置里没配管理员,枚举列不出来;认「没有 kind」这个开发版独有的形状。
    assert!(dev.get(outreach).unwrap().parameters["properties"]
        .get("kind")
        .is_none());
    assert!(registry.get(outreach).unwrap().parameters["properties"]
        .get("kind")
        .is_some());

    let (_temp3, mut admin, _adapter2) = crate::platforms::tests::shared::test_turn_context(true);
    admin.is_admin = true;
    let mut registry =
        miyu_engine::tools::build_tool_registry(&config, &paths, PersonaLane::Active, false)
            .unwrap();
    miyu_engine::tools::platform_outreach::register(&mut registry, &config);
    crate::platforms::apply_platform_turn_scope(&mut registry, &config, &paths, &admin, None);
    crate::platforms::tool::register_outreach(&mut registry, &admin, PersonaLane::Active);
    assert!(registry.contains(outreach), "管理员同样有平台版");
    assert!(registry.contains(contacts));
}

/// 终端这一侧：配置里开了「从终端发到 QQ」和 QQ，就算 NapCat 没连上，本地会话的工具表里也
/// 有 `send_qq_message`（09-25：原来连上才装，daemon 重启、QQ 重连一次，所有开着的会话下一次
/// 请求整条缓存前缀作废）。掉线时调用会明说没连上，见 `platform_outreach` 的用例。
#[test]
fn terminal_outreach_stays_on_the_tool_face_while_qq_is_offline() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let mut config = AppConfig::default();
    config.tools.enabled = true;
    config.platforms.terminal_outreach = true;
    config.platforms.qq.enabled = true;
    let registry =
        miyu_engine::tools::build_tool_registry(&config, &paths, PersonaLane::Active, true)
            .unwrap();
    assert!(registry.contains(miyu_engine::tools::platform_outreach::TOOL_NAME));
    assert!(registry.contains(miyu_engine::tools::platform_outreach::CONTACTS_TOOL_NAME));
}

/// 会话钉的模型被供应商下架/改名之后：退回全局池，并把这份失效的覆盖清掉。
///
/// 留着它的话 `from_config` 直接报「没有可用端点」，这条会话连打开都打不开
/// （09-18 真机：某会话钉着 `opencodego / union-alpha`，`miyu` 整个进不去）。
#[test]
fn a_stale_session_model_override_falls_back_to_the_global_pool() {
    let temp = tempfile::tempdir().unwrap();
    let state = DaemonState::for_test(test_paths(temp.path()), 8300).unwrap();
    let persona = active_persona_scope(&state);
    let session_id = state
        .state_store
        .create_session(&persona, "", "user", None)
        .unwrap()
        .session_id;
    let global = state.manager.lock().unwrap().config.clone();
    let real = usable_pin(&global);
    let ghost = miyu_base::config::ActiveProviderModelConfig {
        provider_id: real.provider_id.clone(),
        model: "miyu-model-the-provider-removed".to_string(),
    };

    // 一条都对不上：退回全局池，覆盖被清掉，返回值点名是哪一条没了。
    state
        .state_store
        .set_session_model_override(&session_id, Some(std::slice::from_ref(&ghost)))
        .unwrap();
    let mut config = global.clone();
    let stale = apply_session_model_override_to(&mut config, &state.state_store, &session_id);
    assert_eq!(
        stale,
        vec![format!("{} / {}", ghost.provider_id, ghost.model)],
        "得说清是哪一条失效了"
    );
    assert_eq!(
        config.active_provider_models, global.active_provider_models,
        "退回全局池"
    );
    assert_eq!(
        state
            .state_store
            .session_model_override(&session_id)
            .unwrap(),
        None,
        "失效的覆盖要清掉,否则每一轮都重踩一次、`/models` 里还显示着它"
    );

    // 一半失效：把失效那条筛掉，剩下的照钉，覆盖留着。
    state
        .state_store
        .set_session_model_override(&session_id, Some(&[ghost.clone(), real.clone()]))
        .unwrap();
    let mut config = global.clone();
    let stale = apply_session_model_override_to(&mut config, &state.state_store, &session_id);
    assert!(stale.is_empty(), "还有能用的就不算失效");
    let applied = config.active_provider_models.as_ref().unwrap();
    assert_eq!(applied.len(), 1);
    assert_eq!(applied[0].model, real.model);
    assert!(state
        .state_store
        .session_model_override(&session_id)
        .unwrap()
        .is_some());
}

/// 默认配置里随便挑一个**真实存在**的供应商/模型，用来当「钉得住」的那一半。
fn usable_pin(config: &AppConfig) -> miyu_base::config::ActiveProviderModelConfig {
    let provider = config
        .providers
        .iter()
        .find(|provider| !provider.models.is_empty())
        .expect("默认配置里总该有一个带模型清单的供应商");
    miyu_base::config::ActiveProviderModelConfig {
        provider_id: provider.id.clone(),
        model: provider.models[0].clone(),
    }
}

/// 沙盒随开随关(09-23)在回合作用域上的样子:
/// - 全局「默认开启」:没对会话说过要不要沙盒 → 读全盘、只能写默认根——客户端在
///   项目目录里就是那个目录(自动检测,细则见 `pick_default_root` 的单测);
/// - `/sandbox clear` 过的会话不跟默认走;
/// - 只读压过一切:读全盘、哪儿都不许写,`/tmp` 也不例外(用户 09-23「彻底只读」);
/// - `/sandbox` 查看照实报「默认」与「只读」;不带目录的查询用上一回合的目录。
#[test]
fn session_scope_follows_the_default_and_the_readonly_switch() {
    if miyu_base::sandbox::probe().is_none() {
        eprintln!("SKIP: no sandbox backend on this machine");
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let state = DaemonState::for_test(test_paths(temp.path()), 8300).unwrap();
    let persona = active_persona_scope(&state);
    let session = state
        .state_store
        .create_session(&persona, "s", "user", None)
        .unwrap();
    let id = session.session_id.as_str();
    let mut config = state.manager.lock().unwrap().config.clone();
    // 项目目录得在 Miyu 家外面:家里面(`~/.miyu` 下)按规则不当项目用。
    let outside = tempfile::tempdir().unwrap();
    let client = outside.path().join("project");
    std::fs::create_dir_all(&client).unwrap();
    let client = client.canonicalize().unwrap();
    let scope = |config: &AppConfig, cwd: Option<PathBuf>| {
        session_scope(
            &state.paths,
            &state.state_store,
            &state.stores,
            config,
            id,
            cwd,
        )
    };

    config.tools.sandbox.default_enabled = false;
    let off = scope(&config, Some(client.clone()));
    assert!(off.policy.is_none(), "默认关着就不套");
    assert_eq!(off.workspace, client);

    config.tools.sandbox.default_enabled = true;
    let on = scope(&config, Some(client.clone()));
    let policy = on.policy.expect("默认开着就套");
    assert_eq!(on.workspace, client, "在项目目录里打开,写的就是它");
    assert_eq!(policy.read_only, vec![PathBuf::from("/")], "读全盘");
    assert!(policy.read_write.contains(&client));
    assert!(!policy.read_only_mode);

    // 回合开始时记下的目录:不带目录的查询(工具桥、`/sandbox` 查看)用的是同一个根。
    remember_client_cwd(id, &client);
    let snapshot = session_state_for(&state, id).unwrap();
    assert!(snapshot.sandbox_default && !snapshot.sandbox_readonly);
    assert_eq!(
        snapshot.sandbox.as_deref(),
        Some(client.to_string_lossy().as_ref())
    );
    assert_eq!(scope(&config, None).workspace, client);

    state
        .state_store
        .set_session_sandbox_readonly(id, true)
        .unwrap();
    let readonly = scope(&config, Some(client.clone()));
    let policy = readonly.policy.expect("只读也是一套策略");
    assert!(policy.read_only_mode);
    assert_eq!(readonly.workspace, client, "工作目录照旧是默认根");
    assert!(!policy.read_write.contains(&client), "根不许写");
    assert!(
        !policy.read_write.contains(&PathBuf::from("/tmp")),
        "彻底只读:/tmp 也不许写"
    );
    assert!(session_state_for(&state, id).unwrap().sandbox_readonly);

    // `/sandbox clear`:明确不要沙盒,之后不跟默认走,只读一并清掉。
    state
        .state_store
        .set_session_sandbox(id, None, false)
        .unwrap();
    assert!(
        scope(&config, Some(client.clone())).policy.is_none(),
        "说过不要就不跟默认走"
    );

    // 没绑、不跟默认,照样能开只读:工作目录是客户端 cwd,那里也不许写。
    state
        .state_store
        .set_session_sandbox_readonly(id, true)
        .unwrap();
    let bare = scope(&config, Some(client.clone()));
    let policy = bare.policy.expect("只读不依赖绑定");
    assert_eq!(bare.workspace, client);
    assert!(!policy.read_write.contains(&client));
}
