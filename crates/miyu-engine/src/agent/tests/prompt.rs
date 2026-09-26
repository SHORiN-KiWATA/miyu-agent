//! 系统提示词的组装与字节稳定性。

use super::shared::*;
use crate::agent::*;
use miyu_base::config::AppConfig;
use tokio::net::TcpListener;

#[test]
fn runtime_context_contains_dynamic_runtime_only() {
    let context = runtime_context(false);
    assert!(context.starts_with("<runtime "));
    assert!(context.contains("now=\""));
    assert!(context.contains("cwd=\""));
    for noise in ["env=", "shell=", "terminal=", "note="] {
        assert!(!context.contains(noise), "{noise} in {context}");
    }
    // ISO 日期 + 三字母星期,不是中文长日期。
    assert!(!context.contains('年'), "{context}");
}

#[test]
fn a_platform_runtime_stamp_carries_nothing_a_chat_message_cannot_use() {
    // A QQ turn has no working directory, no shell and no terminal. Those
    // attributes were re-sent at full price on every single turn — 285
    // chars where a timestamp needs about 45.
    let platform = runtime_context(true);
    assert!(platform.contains("now=\""), "{platform}");
    for noise in ["cwd=", "shell=", "terminal=", "env=", "note="] {
        assert!(!platform.contains(noise), "{noise} in {platform}");
    }
    // 平台面到分钟,终端面到小时:同粒度内整块字节不变。
    assert!(platform.contains(':'), "{platform}");
    let terminal = runtime_context(false);
    let stamp = terminal
        .split("now=\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .unwrap();
    // 终端面到小时:分钟位恒为 00,同一小时内整块字节不变。
    assert!(stamp.ends_with(":00"), "{stamp}");
}

#[test]
fn host_environment_rides_the_system_prompt_for_owners_only() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());

    let owner = with_host_environment(
        "base".to_string(),
        PromptAudience::Owner,
        &paths,
        &AppConfig::default(),
        false,
        true,
        false,
    );
    assert!(owner.starts_with("base\n\n<host-environment os=\""));
    assert!(owner.contains("/>"));
    assert!(owner.contains("LaTeX"), "渲染能力说明应跟随 owner 提示词");
    assert!(owner.contains(&format!(" miyu_home=\"{}\"", paths.root_dir.display())));
    // The static block must not be mistaken for the per-turn stamp: the
    // system prompt never carries a `<runtime` tag.
    assert!(!owner.contains("<runtime"));
    // 语音协议跟语音子系统快照走(09-16):开着就在属主分支末尾,关着整段不发,
    // 其余字节逐字相同——翻转开关=恰好那一段的差。
    assert!(owner.ends_with(&format!("\n\n{VOICE_PROTOCOL}")), "{owner}");
    let owner_no_voice = with_host_environment(
        "base".to_string(),
        PromptAudience::Owner,
        &paths,
        &AppConfig::default(),
        false,
        false,
        false,
    );
    assert!(
        !owner_no_voice.contains("<voice-protocol"),
        "{owner_no_voice}"
    );
    assert_eq!(
        format!("{owner_no_voice}\n\n{VOICE_PROTOCOL}"),
        owner,
        "语音开关只该差那一段"
    );

    // 判官/子代理(Internal)一字不加;平台会话(External)只多一段风格锁——
    // 它与受众无关,主机路径、LaTeX、语音协议仍旧只给属主。
    assert_eq!(
        with_host_environment(
            "base".to_string(),
            PromptAudience::Internal,
            &paths,
            &AppConfig::default(),
            false,
            true,
            false,
        ),
        "base"
    );
    let external = with_host_environment(
        "base".to_string(),
        PromptAudience::External,
        &paths,
        &AppConfig::default(),
        false,
        true,
        true,
    );
    assert_eq!(external, format!("base{STYLE_LOCK}"));
    assert!(!external.contains("<host-environment"));
    assert!(!external.contains("LaTeX"));
    assert!(!external.contains("<voice-protocol"));
    // WebUI 回合(External 但不是平台回合):带主机环境块,但 LaTeX/语音协议仍只给属主
    let webui = with_host_environment(
        "base".to_string(),
        PromptAudience::External,
        &paths,
        &AppConfig::default(),
        false,
        true,
        false,
    );
    assert!(webui.starts_with("base\n\n<host-environment os=\""));
    assert!(webui.ends_with(STYLE_LOCK));
    assert!(!webui.contains("LaTeX"));
    // dev 提示词极简,外部受众也不带风格锁。
    assert_eq!(
        with_host_environment(
            "base".to_string(),
            PromptAudience::External,
            &paths,
            &AppConfig::default(),
            true,
            true,
            true,
        ),
        "base"
    );
    // dev 的属主分支:主机环境块照带,风格锁/LaTeX/语音协议一概不带(语音开着也不带)。
    let dev_owner = with_host_environment(
        "base".to_string(),
        PromptAudience::Owner,
        &paths,
        &AppConfig::default(),
        true,
        true,
        false,
    );
    assert!(dev_owner.starts_with("base\n\n<host-environment os=\""));
    for absent in ["<style-lock>", "LaTeX", "<voice-protocol"] {
        assert!(!dev_owner.contains(absent), "{absent} in {dev_owner}");
    }
    // 属主提示词的字节顺序不变:风格锁仍在主机环境之后。
    let host_at = owner.find("<host-environment").unwrap();
    let lock_at = owner.find("<style-lock>").unwrap();
    assert!(host_at < lock_at);
}

/// `<sandbox>` 尾巴:受众不对就不发,否则跟最近一份比(09-25 起走指令源)。
fn sandbox_tail(
    audience: PromptAudience,
    platform_turn: bool,
    last: Option<&str>,
) -> Option<String> {
    if !SandboxSource::applies(audience, platform_turn) {
        return None;
    }
    let messages = last
        .map(|last| ChatMessage::turn_context(last.to_string()))
        .into_iter()
        .collect::<Vec<_>>();
    project(&SandboxSource, &messages)
}

/// 沙盒不在环境块里(09-23 起:按 Tab 随开随关,写在系统提示词里每切一次就掰断
/// 整段前缀),改走「变了才追加」的 `<sandbox>` 尾巴。退回修复前第一条断言报红。
#[tokio::test]
async fn sandbox_lives_in_the_tail_not_the_host_environment() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let build = || {
        with_host_environment(
            "base".to_string(),
            PromptAudience::Owner,
            &paths,
            &AppConfig::default(),
            false,
            true,
            false,
        )
    };
    let policy = std::sync::Arc::new(miyu_base::sandbox::SandboxPolicy {
        root: temp.path().join("root"),
        writable_summary: vec!["root".into(), "/tmp".into()],
        readable_summary: vec!["root".into(), "/tmp".into(), "system dirs".into()],
        ..Default::default()
    });
    let outside = build();
    let (inside, first, repeat, platform) =
        miyu_base::sandbox::with_sandbox(Some(policy.clone()), async {
            let first = sandbox_tail(PromptAudience::Owner, false, None);
            let repeat = sandbox_tail(PromptAudience::Owner, false, first.as_deref());
            let platform = sandbox_tail(PromptAudience::External, true, None);
            (build(), first, repeat, platform)
        })
        .await;
    assert_eq!(inside, outside, "环境块不随沙盒变");
    assert!(!inside.contains("sandbox"), "{inside}");

    let first = first.expect("第一次要说");
    assert_eq!(first, miyu_base::host_info::sandbox_notice(&policy));
    assert_eq!(repeat, None, "跟上一份逐字节相同就不再发");
    assert_eq!(platform, None, "平台回合跟环境块一样不带");

    // 作用域外 = 没沙盒:说过就补一条「关了」,从没说过就什么都不发。
    assert_eq!(
        sandbox_tail(PromptAudience::Owner, false, Some(&first)).as_deref(),
        Some(miyu_base::host_info::SANDBOX_OFF_NOTICE)
    );
    assert_eq!(sandbox_tail(PromptAudience::Owner, false, None), None);
    assert_eq!(
        sandbox_tail(
            PromptAudience::Owner,
            false,
            Some(miyu_base::host_info::SANDBOX_OFF_NOTICE)
        ),
        None
    );
}

#[test]
fn host_environment_is_byte_stable_across_prompt_rebuilds() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    // Rebuilt on every turn by `prepare_for_turn`; a value that drifted
    // between rebuilds would move the prefix and cost a cache miss a turn.
    let first = with_host_environment(
        String::new(),
        PromptAudience::Owner,
        &paths,
        &AppConfig::default(),
        false,
        true,
        false,
    );
    let second = with_host_environment(
        String::new(),
        PromptAudience::Owner,
        &paths,
        &AppConfig::default(),
        false,
        true,
        false,
    );
    assert_eq!(first, second);
}

#[test]
fn user_identity_is_limited_to_owner_prompts() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    let mut config = AppConfig::default();
    std::fs::create_dir_all(config.identities_dir_path(&paths)).unwrap();
    std::fs::write(config.user_identity_path(&paths), "legacy-owner-marker").unwrap();

    let owner = config
        .system_prompt_for(&paths, PromptAudience::Owner)
        .unwrap();
    let external = config
        .system_prompt_for(&paths, PromptAudience::External)
        .unwrap();
    let internal = config
        .system_prompt_for(&paths, PromptAudience::Internal)
        .unwrap();
    assert!(owner.contains("legacy-owner-marker"));
    assert!(!external.contains("legacy-owner-marker"));
    assert!(!internal.contains("legacy-owner-marker"));

    config.prompt.active_identity = "owner.md".to_string();
    std::fs::write(
        config.identity_path(&paths, "owner.md"),
        "active-owner-marker",
    )
    .unwrap();
    assert!(config
        .system_prompt_for(&paths, PromptAudience::Owner)
        .unwrap()
        .contains("active-owner-marker"));
    assert!(!config
        .system_prompt_for(&paths, PromptAudience::External)
        .unwrap()
        .contains("active-owner-marker"));
}

#[test]
fn runtime_system_context_refreshes_the_effective_prompt_immediately() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let config = AppConfig::default();
    let state = StateStore::new(&paths).unwrap();
    let client =
        OpenAiCompatibleClient::new(config.provider(None).unwrap(), &config, &paths).unwrap();
    let mut agent = Agent::new(
        config,
        &paths,
        state,
        client,
        ToolRegistry::new(),
        PersonaLane::Active,
    )
    .unwrap();

    agent
        .set_runtime_system_context(vec!["  platform-only notice  ".to_string()])
        .unwrap();
    assert!(agent.system_prompt.contains("platform-only notice"));
    assert_eq!(
        agent.input.runtime_system_context,
        vec!["platform-only notice".to_string()]
    );
}

#[test]
fn nothing_after_the_leading_prompt_may_carry_the_system_role() {
    // Provider chat templates gather every `system` message to the front of
    // the rendered prompt, so one appearing mid-conversation shifts that
    // block and drops the prefix cache to zero. Measured on DeepSeek with a
    // byte-identical prefix: appending `assistant + user` hit 99%, the same
    // append with one `system` in front of it hit 0%, and moving that
    // `system` to the very end still hit 0%.
    let messages = vec![
        ChatMessage::system("persona"),
        ChatMessage::plain("user", "问题"),
        ChatMessage::turn_context("<runtime now=\"x\"/>"),
        ChatMessage::turn_context("<associative-memory>x</associative-memory>"),
        ChatMessage::assistant("答案", None),
    ];
    let stray: Vec<usize> = messages
        .iter()
        .enumerate()
        .skip(1)
        .filter(|(_, message)| message.role == "system")
        .map(|(index, _)| index)
        .collect();
    assert!(
        stray.is_empty(),
        "system role at {stray:?} would reset the prefix cache"
    );
}

/// 防失忆提醒(08-16 版):首回合蒸馏后以化石身份进历史;间隔轮数内
/// 的第二回合不再注入新份——请求里只有回放的那一份,且当前轮尾部
/// 干净(runtime 投影同小时也跳注入),前缀纯追加。
#[tokio::test]
async fn persona_reminder_fossilizes_on_interval_and_replays() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let mut config = queue_test_config(base_url);
    config.tools.enabled = false;
    config.system_prompt = Some("测试人格：说话简短。".to_string());
    config.prompt.persona_reminder = true;

    let (first_chat_tx, first_chat_rx) = oneshot::channel();
    let (second_chat_tx, second_chat_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let reply = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"哦\"}}]}\n\n",
            "data: {\"choices\":[{\"finish_reason\":\"stop\",\"delta\":{}}]}\n\n",
            "data: [DONE]\n\n"
        );
        // 回合1请求①:蒸馏调用(产物首行名字,次行正文)。
        let (mut distill, _) = listener.accept().await.unwrap();
        let request = read_test_http_request(&mut distill).await;
        let body: serde_json::Value = serde_json::from_slice(&request).unwrap();
        assert!(body["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("persona definition file"));
        write_test_sse(
            &mut distill,
            concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"短\\n回复很短，从不用Emoji。\"}}]}\n\n",
                "data: {\"choices\":[{\"finish_reason\":\"stop\",\"delta\":{}}]}\n\n",
                "data: [DONE]\n\n"
            ),
        )
        .await;
        // 回合1请求②:正式对话。
        let (mut chat, _) = listener.accept().await.unwrap();
        let _ = first_chat_tx.send(read_test_http_request(&mut chat).await);
        write_test_sse(&mut chat, reply).await;
        // 回合2请求①:缓存命中,直接就是对话请求(若再蒸馏一次,
        // 这里读到的请求不含新消息,下方断言会失败)。
        let (mut chat2, _) = listener.accept().await.unwrap();
        let _ = second_chat_tx.send(read_test_http_request(&mut chat2).await);
        write_test_sse(&mut chat2, reply).await;
    });

    let state = StateStore::new(&paths).unwrap();
    state.init_files().unwrap();
    let provider = config.provider(None).unwrap().clone();
    let client = OpenAiCompatibleClient::new(&provider, &config, &paths).unwrap();
    let mut agent = Agent::new(
        config.clone(),
        &paths,
        state.clone(),
        client,
        ToolRegistry::new(),
        PersonaLane::Active,
    )
    .unwrap();
    let context = Arc::new(FakePlatformTurn::onebot());
    agent.set_platform_context_images(context.clone(), Vec::new());
    agent.chat_stream("第一条消息", |_| Ok(())).await.unwrap();

    let expected_reminder = "<persona-reminder>回复很短，从不用Emoji。\
         就算是讲解答疑，也只说最关键的两三步，整条不超过一百字，\
         一次说不完就等对方追问。</persona-reminder>";
    let request: serde_json::Value = serde_json::from_slice(&first_chat_rx.await.unwrap()).unwrap();
    let messages = request["messages"].as_array().unwrap();
    // 提醒以化石身份入列(位置在 runtime 之后、随机注入的表情包
    // 提醒之前),不再断言绝对末尾——只断言恰好一份。
    assert_eq!(
        messages
            .iter()
            .filter(|message| message["content"] == expected_reminder)
            .count(),
        1
    );
    let turns = state.load_turns().unwrap();
    // 新语义:提醒就是化石,回放历史自带。
    assert!(format!("{:?}", turns[0].context_messages).contains("persona-reminder"));
    assert!(paths
        .state_dir
        .join("persona-hints")
        .read_dir()
        .unwrap()
        .next()
        .is_some());

    agent.set_platform_context_images(context, Vec::new());
    agent.chat_stream("第二条消息", |_| Ok(())).await.unwrap();
    let request: serde_json::Value =
        serde_json::from_slice(&second_chat_rx.await.unwrap()).unwrap();
    let messages = request["messages"].as_array().unwrap();
    assert!(messages.iter().any(|message| {
        message["content"]
            .as_str()
            .is_some_and(|content| content.contains("第二条消息"))
    }));
    let reminder_count = messages
        .iter()
        .filter(|message| {
            message["content"]
                .as_str()
                .is_some_and(|content| content.contains("persona-reminder"))
        })
        .count();
    // 间隔(默认3)未到:仅回放化石那一份,不再追加新份;绝对末尾
    // 不再是漂浮提醒(可能是用户消息或跨分钟的新 runtime,都合法)。
    assert_eq!(reminder_count, 1);
    assert!(messages
        .iter()
        .any(|message| message["content"] == expected_reminder));
    assert_ne!(messages.last().unwrap()["content"], expected_reminder);
    server.await.unwrap();
}

/// 手写防失忆提示(hints/<scope>.md)优先于自动蒸馏:存在时整回合
/// 不发蒸馏请求(服务端只应答一次对话),尾部原样携带手写内容,
/// 不拼场景句。
#[tokio::test]
async fn manual_persona_reminder_overrides_distillation() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let mut config = queue_test_config(base_url);
    config.tools.enabled = false;
    config.system_prompt = Some("测试人格：说话简短。".to_string());
    config.prompt.persona_reminder = true;
    let hint_path = miyu_core::persona_hint::manual_hint_path(&config, &paths, "default");
    std::fs::create_dir_all(hint_path.parent().unwrap()).unwrap();
    std::fs::write(&hint_path, "未有在群里潜水。手写版提醒。\n").unwrap();

    let (chat_tx, chat_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut chat, _) = listener.accept().await.unwrap();
        let _ = chat_tx.send(read_test_http_request(&mut chat).await);
        write_test_sse(
            &mut chat,
            concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"哦\"}}]}\n\n",
                "data: {\"choices\":[{\"finish_reason\":\"stop\",\"delta\":{}}]}\n\n",
                "data: [DONE]\n\n"
            ),
        )
        .await;
    });

    let state = StateStore::new(&paths).unwrap();
    state.init_files().unwrap();
    let provider = config.provider(None).unwrap().clone();
    let client = OpenAiCompatibleClient::new(&provider, &config, &paths).unwrap();
    let mut agent = Agent::new(
        config.clone(),
        &paths,
        state.clone(),
        client,
        ToolRegistry::new(),
        PersonaLane::Active,
    )
    .unwrap();
    let context = Arc::new(FakePlatformTurn::onebot());
    agent.set_platform_context_images(context, Vec::new());
    agent.chat_stream("第一条消息", |_| Ok(())).await.unwrap();

    let request: serde_json::Value = serde_json::from_slice(&chat_rx.await.unwrap()).unwrap();
    let last = request["messages"].as_array().unwrap().last().unwrap();
    assert_eq!(last["role"], "user");
    assert_eq!(
        last["content"],
        "<persona-reminder>未有在群里潜水。手写版提醒。</persona-reminder>"
    );
    server.await.unwrap();
}

/// 预设对话(begin_dialogs):system 之后、真实历史之前注入 Q/A 对,
/// 每请求从 dialogs/<scope>.md 重建、永不落库。
#[test]
fn preset_dialogs_ride_after_system_before_history() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let config = AppConfig::default();
    let dialogs = miyu_core::persona_hint::dialogs_path(&config, &paths, "default");
    std::fs::create_dir_all(dialogs.parent().unwrap()).unwrap();
    std::fs::write(&dialogs, "user: 你好\nassistant: 哼，又来一个。\n").unwrap();
    let state = StateStore::new(&paths).unwrap();
    state.init_files().unwrap();
    state.start_turn("turn_h", "历史问题", 999999).unwrap();
    state.complete_turn("turn_h", "历史回答", None).unwrap();
    let client =
        OpenAiCompatibleClient::new(config.provider(None).unwrap(), &config, &paths).unwrap();
    let agent = Agent::new(
        config,
        &paths,
        state,
        client,
        ToolRegistry::new(),
        PersonaLane::Active,
    )
    .unwrap();
    let messages = agent.chat_messages("current", "新消息").unwrap().0;
    assert_eq!(messages[0].role, "system");
    assert_eq!(messages[1].role, "user");
    assert_eq!(chat_message_text(&messages[1]).unwrap(), "你好");
    assert_eq!(messages[2].role, "assistant");
    assert_eq!(chat_message_text(&messages[2]).unwrap(), "哼，又来一个。");
    assert_eq!(chat_message_text(&messages[3]).unwrap(), "历史问题");
    // 预设对话只活在请求里:历史存储不含它。
    let turns = agent.state.load_turns().unwrap();
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].user_content, "历史问题");
}

/// Dev 模式极简组装:系统提示词是 dev-prompt.md 的一行(缺省内置默认),
/// 人格全家(预设对话/用户档案)整套绕开——即使 dialogs 文件存在。
#[test]
fn dev_mode_uses_one_line_prompt_and_skips_persona_family() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let config = AppConfig::default();
    // 人格侧的预设对话文件在场,dev 也必须无视。
    let dialogs = miyu_core::persona_hint::dialogs_path(&config, &paths, "default");
    std::fs::create_dir_all(dialogs.parent().unwrap()).unwrap();
    std::fs::write(&dialogs, "user: 你好\nassistant: 哼，又来一个。\n").unwrap();
    let state = StateStore::new(&paths).unwrap();
    state.init_files().unwrap();
    state.start_turn("turn_h", "历史问题", 999999).unwrap();
    state.complete_turn("turn_h", "历史回答", None).unwrap();
    let client =
        OpenAiCompatibleClient::new(config.provider(None).unwrap(), &config, &paths).unwrap();
    let agent = Agent::new(
        config,
        &paths,
        state,
        client,
        ToolRegistry::new(),
        PersonaLane::Dev,
    )
    .unwrap();
    let messages = agent.chat_messages("current", "新消息").unwrap().0;
    assert_eq!(messages[0].role, "system");
    let system = chat_message_text(&messages[0]).unwrap();
    // 09-24 起开发模式没有内置角色句:没写 dev-prompt.md 就从环境块开始,开头不空行。
    assert!(
        system.starts_with("<host-environment")
            && !system.contains(miyu_base::config::LEGACY_DEV_SYSTEM_PROMPT),
        "dev 系统提示词不该带内置角色句: {system}"
    );
    assert!(!system.contains("<current-user-profile>"), "dev 无用户身份");
    // 09-09:记忆整套退场,连 `<associative-memory>` 前言都不该出现。
    assert!(
        !system.contains("<associative-memory>"),
        "dev 不带记忆,前言不该进 system: {system}"
    );
    // 第一条对话消息直接是历史,没有预设对话对。
    assert_eq!(messages[1].role, "user");
    assert_eq!(chat_message_text(&messages[1]).unwrap(), "历史问题");
}

/// `load_tools` 在 full 档里是死重量:模型看不见它就不会调,而它每轮都占
/// 着目录。留它的唯一理由是「历史里已有调用记录时不能变成未知工具」——
/// 所以判据是本会话到底调没调过,而不是档位本身。
#[test]
fn dev_load_tools_registers_only_after_the_session_used_it() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let config = AppConfig::default();
    let state = StateStore::new(&paths).unwrap();
    state.init_files().unwrap();
    let client =
        OpenAiCompatibleClient::new(config.provider(None).unwrap(), &config, &paths).unwrap();
    let tools =
        crate::tools::build_tool_registry(&config, &paths, PersonaLane::Dev, false).unwrap();
    assert!(tools.contains("load_tools"), "底座表里本来就有 load_tools");
    let mut agent = Agent::new(
        config,
        &paths,
        state.clone(),
        client,
        tools,
        PersonaLane::Dev,
    )
    .unwrap();

    agent.prepare_for_turn().unwrap();
    assert!(
        !agent.tools.lock().unwrap().contains("load_tools"),
        "full 档 + 全新会话:不该带 load_tools"
    );

    // 会话里出现过加载记录(从需加载档切过来的会话就是这个形状)。
    state
        .add_session_loaded_tools(&["web_search".to_string()], None)
        .unwrap();
    agent.prepare_for_turn().unwrap();
    assert!(
        agent.tools.lock().unwrap().contains("load_tools"),
        "历史里调用过就必须放回来,否则模型照着历史撞未知工具"
    );
}

/// 被内容策略拦下、已经踢出上下文的那一轮，**不许再发给模型**。
///
/// 这条是整件事的落点：用户 09-20 报 agy 拦下一条提示词之后，那一轮留在上下文里
/// 每轮重发、每轮被拦，整条会话哑掉。上面两层单测只证明「标记打得对」「标记打在
/// 对的轮上」，真正要钉住的是**标记之后请求里没有它**。
#[test]
fn a_turn_dropped_from_context_never_reaches_the_model_again() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let config = AppConfig::default();
    let state = StateStore::new(&paths).unwrap();
    state.init_files().unwrap();
    state.start_turn("turn_ok", "正常的一句", 999999).unwrap();
    state.complete_turn("turn_ok", "好的", None).unwrap();
    state
        .start_turn("turn_blocked", "触发拦截的那句", 999999)
        .unwrap();
    state.complete_turn("turn_blocked", "", None).unwrap();

    let messages_before = {
        let client =
            OpenAiCompatibleClient::new(config.provider(None).unwrap(), &config, &paths).unwrap();
        let agent = Agent::new(
            config.clone(),
            &paths,
            state.clone(),
            client,
            ToolRegistry::new(),
            PersonaLane::Active,
        )
        .unwrap();
        agent.chat_messages("current", "下一句").unwrap().0
    };
    let joined_before = messages_before
        .iter()
        .map(|message| chat_message_text(message).unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        joined_before.contains("触发拦截的那句"),
        "前提不成立：踢出去之前它本来就该在请求里"
    );

    assert_eq!(
        state.hide_last_turn().unwrap().as_deref(),
        Some("turn_blocked")
    );

    let client =
        OpenAiCompatibleClient::new(config.provider(None).unwrap(), &config, &paths).unwrap();
    let agent = Agent::new(
        config,
        &paths,
        state,
        client,
        ToolRegistry::new(),
        PersonaLane::Active,
    )
    .unwrap();
    let joined_after = agent
        .chat_messages("current", "下一句")
        .unwrap()
        .0
        .iter()
        .map(|message| chat_message_text(message).unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !joined_after.contains("触发拦截的那句"),
        "被拦的那一轮还在请求里，下一轮照样会被拦：\n{joined_after}"
    );
    assert!(
        joined_after.contains("正常的一句"),
        "只该踢掉被拦的那一轮，别的历史要留着：\n{joined_after}"
    );
}
