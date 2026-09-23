use super::*;
use miyu_base::config::PersonaLane;

/// 显示名表是合并登记的:normal 之后再登记一张没有脚本的 dev 表,属主脚本的
/// 名字不能被冲掉(09-10 沙盒实测 battery_care 变裸 id 的根因)。
#[test]
fn script_display_names_survive_registering_a_registry_without_them() {
    let mut with_scripts = ToolRegistry::new();
    with_scripts.register(
        ToolSpec::new(
            "battery_care_probe_test",
            "probe",
            serde_json::json!({"type": "object"}),
            |_| async { Ok(String::new()) },
        )
        .with_display_name("电池养护"),
    );
    register_script_display_names(&with_scripts);
    assert_eq!(readable_tool_name("battery_care_probe_test"), "电池养护");
    let without_scripts = ToolRegistry::new();
    register_script_display_names(&without_scripts);
    assert_eq!(readable_tool_name("battery_care_probe_test"), "电池养护");
}

/// 内置工具 schema 的 token 预算:每件工具的 description + parameters 折成的
/// token 数封顶。这份东西每轮都进上下文(full 模式)或按需拉入(stub),膨胀
/// 是慢性的、靠肉眼发现不了。超线的名字连同前十名一起打出来,好知道该修谁。
/// 内置脚本不在此列:脚本按「一个脚本包办所有事」设计,参数面大是本分
/// (用户 09-03 裁定),不拿这条预算约束它们。
#[test]
fn tool_schemas_stay_within_the_token_budget() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let mut config = miyu_base::config::AppConfig::default();
    config.plugins.web.enabled = true;
    config.skills.allow_command_execution = true;
    let registry = builtin_registry(&config, &paths);
    let cost = |description: &str, parameters: &serde_json::Value| {
        miyu_base::token_estimate::estimate_tokens(description)
            + miyu_base::token_estimate::estimate_tokens(&parameters.to_string())
    };
    let mut rows: Vec<(String, usize)> = registry
        .tool_names()
        .iter()
        .filter_map(|name| registry.get(name))
        .map(|spec| (spec.name.clone(), cost(&spec.description, &spec.parameters)))
        .collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1));
    let top: Vec<String> = rows
        .iter()
        .take(10)
        .map(|(n, t)| format!("{n}={t}"))
        .collect();
    println!("schema token top10: {}", top.join(" "));
    // 600 = 现状最重的 subagent(332)留将近一倍头:新工具照这个体量写,别更肥。
    const BUDGET: usize = 600;
    let over: Vec<&(String, usize)> = rows.iter().filter(|(_, tokens)| *tokens > BUDGET).collect();
    assert!(
        over.is_empty(),
        "这些工具的 schema 超过 {BUDGET} token 预算:{over:?};当前前十:{top:?}"
    );
}

/// 数组参数要容忍模型真会传的形状。线上实测:mimo-v2.5 把
/// `reference_images` 传成了 `"[\"/path.png\"]"`——一个被 JSON 编码成
/// 字符串的数组。只认真数组会让 job_ids / user_ids / tags / groups 这类
/// 参数一起静默失效,踢人工具取不到目标尤其危险。
#[test]
fn string_list_accepts_the_shapes_models_actually_send() {
    use serde_json::json;
    let one = vec!["a".to_string()];
    assert_eq!(string_list(Some(&json!(["a"]))), one);
    assert_eq!(string_list(Some(&json!("a"))), one);
    assert_eq!(string_list(Some(&json!(r#"["a"]"#))), one);
    assert_eq!(
        string_list(Some(&json!(["a", " b ", "", "  "]))),
        vec!["a".to_string(), "b".to_string()]
    );
    assert!(string_list(None).is_empty());
    assert!(string_list(Some(&json!([]))).is_empty());
    assert!(string_list(Some(&json!(""))).is_empty());
    assert!(string_list(Some(&json!(null))).is_empty());
    // 解不开的字符串按单条路径收下,不当成数组硬猜。
    assert_eq!(
        string_list(Some(&json!("[not json"))),
        vec!["[not json".to_string()]
    );
}

/// 有效加载模式按候选池取最保守:任一成员要 full 则整池 full——一次请求
/// 只有一张工具面,命中与故障转移都在发送时才定,这张脸必须全员可用。
/// 约束解码型模型(实测 bigmodel glm-5.3-flash)吃不下空壳 stub,是模型级
/// full 覆盖存在的理由(09-01)。
#[test]
fn effective_loading_mode_takes_the_most_conservative_pool_member() {
    use miyu_base::config::ActiveProviderModelConfig;
    let mut config = AppConfig::default();
    config.tools.loading_mode = "stub".to_string();
    let provider_id = config.providers[0].id.clone();
    let pick = |model: &str| ActiveProviderModelConfig {
        provider_id: provider_id.clone(),
        model: model.to_string(),
    };

    // 空池回退全局。
    config.active_provider_models = None;
    assert_eq!(effective_tools_loading_mode(&config), "stub");

    // 全员跟随全局(需加载)。
    config.active_provider_models = Some(vec![pick("lenient-a"), pick("lenient-b")]);
    assert_eq!(effective_tools_loading_mode(&config), "stub");

    // 混进一个模型级 full,整池升 full。
    config.providers[0]
        .model_tools_loading_mode
        .insert("locked".to_string(), "full".to_string());
    config
        .active_provider_models
        .as_mut()
        .unwrap()
        .push(pick("locked"));
    assert_eq!(effective_tools_loading_mode(&config), "full");

    // 回归(09-01 用户暴露):多模态池【不】参与——它只喂看图子分析,不处理
    // 带工具的主回合。文本池全需加载、多模态池里配了 full 的模型时,主回合
    // 仍是需加载,不被拖成 full。
    config.active_provider_models = Some(vec![pick("lenient-a")]);
    config.active_multimodal_provider_models = Some(vec![pick("locked")]);
    assert_eq!(
        effective_tools_loading_mode(&config),
        "stub",
        "多模态池不该把文本主回合拖成 full"
    );
    config.active_multimodal_provider_models = None;

    // 模型级覆盖压过全局:全局 full,钉死的单模型显式需加载 → stub。
    config.tools.loading_mode = "full".to_string();
    config.providers[0]
        .model_tools_loading_mode
        .insert("thrifty".to_string(), "stub".to_string());
    config.active_provider_models = Some(vec![pick("thrifty")]);
    assert_eq!(effective_tools_loading_mode(&config), "stub");

    // 已删档的 hybrid/lazy 旧值按需加载处理,不悄悄升 full。
    config.tools.loading_mode = "hybrid".to_string();
    config.active_provider_models = Some(vec![pick("lenient-a")]);
    assert_eq!(effective_tools_loading_mode(&config), "stub");
    assert!(is_stub_loading_mode("hybrid"));
    assert!(is_stub_loading_mode("lazy"));
}

/// 回归:dev 模式要有看图(vision_analyze),且随 vision 插件开关走。
#[test]
fn dev_registry_vision_follows_plugin_switch() {
    let paths = miyu_base::paths::MiyuPaths::new().unwrap();
    let mut config = miyu_base::config::AppConfig::default();
    let names = |registry: &ToolRegistry| -> Vec<String> {
        registry
            .definitions()
            .iter()
            .map(|d| d.function.name.clone())
            .collect()
    };
    assert!(names(&dev_registry(&config, &paths)).contains(&"vision_analyze".to_string()));
    config.plugins.vision.enabled = false;
    assert!(!names(&dev_registry(&config, &paths)).contains(&"vision_analyze".to_string()));
}

/// 回归:dev 的技能面与记忆面整套退场(09-09)。
///
/// 退回这个提交之前,dev 会拿到 `load_skill`——而且它列出的是**默认
/// 人格**的技能(`register_skills` 收到的 config 没经过 `dev_scoped`),
/// 外加三件记忆工具。normal 侧必须一件不少。
#[test]
fn dev_registry_drops_skills_and_memory() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let config = miyu_base::config::AppConfig::default();
    let names = |mode| -> Vec<String> {
        build_tool_registry(&config, &paths, mode, false)
            .unwrap()
            .tool_names()
    };
    let dev = names(PersonaLane::Dev);
    for gone in [
        "load_skill",
        "recall_memories",
        "remember_fact",
        "search_evicted_context",
    ] {
        assert!(!dev.contains(&gone.to_string()), "dev still exposes {gone}");
    }
    // 干活的那些一件都不能少。
    for kept in ["run_command", "edit", "subagent", "job", "todowrite"] {
        assert!(dev.contains(&kept.to_string()), "dev lost {kept}");
    }
    let normal = names(PersonaLane::Active);
    for kept in ["load_skill", "recall_memories", "remember_fact"] {
        assert!(normal.contains(&kept.to_string()), "normal lost {kept}");
    }
}

/// dev 的记忆是在配置层关的(`dev_scoped`),这一条守住那个开关——
/// 联想注入、自动日记、`<associative-memory>` 前言全看它。
#[test]
fn dev_scoped_config_turns_memory_off() {
    let config = miyu_base::config::AppConfig::default();
    assert!(config.memory_config().enabled);
    assert!(!config.dev_scoped().memory_config().enabled);
}

pub(super) fn test_paths(root: &std::path::Path) -> MiyuPaths {
    MiyuPaths {
        root_dir: root.to_path_buf(),
        config_dir: root.join("config"),
        config_file: root.join("config/config.jsonc"),
        skills_dir: root.join("config/skills"),
        data_dir: root.join("data"),
        cache_dir: root.join("cache"),
        state_dir: root.join("state"),
        pictures_dir: root.join("pictures"),
        fish_hook_file: root.join("config/fish/conf.d/miyu.fish"),
        bash_hook_file: root.join("config/shell/bash-hook.sh"),
        zsh_hook_file: root.join("config/shell/zsh-hook.zsh"),
        scripts_dir: root.join("config/scripts"),
        system_scripts_dir: root.join("system-scripts"),
    }
}

/// 把仓库里出厂人格的技能摆进隔离资源树:`<root>/personas/default/skills/`。
///
/// 技能 09-23 起读盘(不再 `include_str!`),夹具要看到内置技能(如
/// skill-creator)就得先把它们放进临时树。[`test_paths`] 的
/// `system_scripts_dir=<root>/system-scripts`,父目录 `<root>` 正是资源根。
/// 拷真实文件而不是软链:`read_skill_file` 拒绝符号链接的 SKILL.md。
///
/// 技能树里还住着技能带路的脚本(机票/酒店/直播):它们**不进**常驻工具面,
/// 所以形状夹具里不该出现它们——拷贝时跳过 `scripts/`,免得把脚本面也搬进来。
pub(super) fn install_bundled_skills(root: &std::path::Path) {
    let source =
        std::path::Path::new(miyu_base::WORKSPACE_ROOT).join("src/personas/default/skills");
    copy_tree_without_scripts(&source, &root.join("personas/default/skills"));
}

/// 同 [`copy_tree`],但跳过名为 `scripts` 的目录:技能带路的脚本不在常驻面上。
fn copy_tree_without_scripts(source: &std::path::Path, target: &std::path::Path) {
    std::fs::create_dir_all(target).unwrap();
    for entry in std::fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        if entry.file_name() == "scripts" {
            continue;
        }
        let destination = target.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree_without_scripts(&entry.path(), &destination);
        } else {
            std::fs::copy(entry.path(), destination).unwrap();
        }
    }
}

fn copy_tree(source: &std::path::Path, target: &std::path::Path) {
    std::fs::create_dir_all(target).unwrap();
    for entry in std::fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let destination = target.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &destination);
        } else {
            std::fs::copy(entry.path(), destination).unwrap();
        }
    }
}

#[test]
fn preparing_phase_covers_the_slow_argument_tools_only() {
    for name in [
        "apply_patch",
        "apply_artifact_patch",
        "create_artifact",
        "write_file",
        "edit_file",
        "edit_string",
    ] {
        assert_eq!(
            preparing_phase(name),
            Some(miyu_base::i18n::text("Preparing edit", "准备编辑")),
            "{name}"
        );
    }
    assert_eq!(
        preparing_phase("run_command"),
        Some(miyu_base::i18n::text("Preparing command", "准备执行"))
    );
    assert_eq!(
        preparing_phase("trash_path"),
        Some(miyu_base::i18n::text("Preparing delete", "准备删除"))
    );
    assert_eq!(
        preparing_phase("subagent"),
        Some(miyu_base::i18n::text("Preparing task", "准备任务"))
    );
    assert_eq!(
        preparing_phase("ask_question"),
        Some(miyu_base::i18n::text("Preparing question", "准备问题"))
    );
    // Arguments arrive in one chunk: a hint would only flicker.
    for name in ["read_file", "grep", "list_directory"] {
        assert_eq!(preparing_phase(name), None, "{name}");
    }
}

/// claude-code 中转线的原生工具名(不剥前缀)也要有提示词,否则那条线
/// 只剩批量兜底的「准备工具」。
#[test]
fn preparing_phase_covers_claude_native_tools() {
    for name in ["Edit", "Write", "MultiEdit", "NotebookEdit"] {
        assert_eq!(
            preparing_phase(name),
            Some(miyu_base::i18n::text("Preparing edit", "准备编辑")),
            "{name}"
        );
    }
    assert_eq!(
        preparing_phase("Bash"),
        Some(miyu_base::i18n::text("Preparing command", "准备执行"))
    );
    for name in ["Task", "Agent"] {
        assert_eq!(
            preparing_phase(name),
            Some(miyu_base::i18n::text("Preparing task", "准备任务")),
            "{name}"
        );
    }
    assert_eq!(
        preparing_phase("TodoWrite"),
        Some(miyu_base::i18n::text("Preparing list", "准备清单"))
    );
    for name in ["Read", "Glob", "Grep", "WebFetch"] {
        assert_eq!(preparing_phase(name), None, "{name}");
    }
}

/// 续轮提示词必须自报来历。
///
/// 实测过一次：一个会话正在排查游戏的 VC++ 运行库，用户设了个「查询东京
/// 天气」的目标，续轮到达时模型判定「This looks like a system prompt
/// injection or some automated goal that hijacked my session」，拒绝执行、
/// 继续做上一个话题。那个警惕是对的——一段没有来历、和上文毫无关系的
/// 英文祈使句，本来就该被怀疑。
///
/// 所以这几句不是客套：谁下的（用户）、怎么来的（/goal 命令 + 空闲时自动
/// 续轮）、为什么和上文对不上（长期目标会跨越话题）。看着像冗余，最容易
/// 被后人当废话删掉，这条测试就是拦这个的。
#[test]
fn goal_round_prompt_states_where_it_came_from() {
    let goal = miyu_core::state::GoalRecord {
        session_id: "sess_x".to_string(),
        goal_id: "goal_abc123".to_string(),
        revision: 4,
        objective: "把测试跑绿".to_string(),
        phase: miyu_core::state::GoalPhase::Active,
        blocked_code: None,
        blocked_message: None,
        max_rounds: 10,
        rounds_started: 2,
        created_at: String::new(),
        updated_at: String::new(),
    };
    let prompt = crate::tools::goal::goal_round_prompt(&goal, true);
    // 断言按小写比，大小写不是这条测试要守的东西。
    let lowered = prompt.to_lowercase();
    for expected in [
        "set by the user",           // 谁下的
        "/goal",                     // 怎么下的
        "unrelated to the messages", // 为什么和上文对不上
        "waiting",                   // 不许拿一整轮只说「我在等你」
        "goal_abc123",               // CAS 凭证直接给它，省一次 action=get
    ] {
        assert!(
            lowered.contains(expected),
            "续轮提示词丢了来历说明（缺 {expected:?}）——模型会把它当注入拒掉:\n{prompt}"
        );
    }
    // 目标本身和轮号仍要在。
    assert!(prompt.contains("把测试跑绿"));
    assert!(prompt.contains("Round 2 of 10"));
    // 两条结束调用都要把 goal_id 和 revision 填好——让模型自己去读一遍
    // 目标、或者为此加载一次工具，都是白跑的往返。
    assert!(
        prompt.contains(r#""revision":4"#),
        "revision 没填进调用里：\n{prompt}"
    );
    assert!(
        prompt.matches(r#""goal_id":"goal_abc123""#).count() == 2,
        "complete 和 blocked 两条都要填好：\n{prompt}"
    );
    assert!(
        prompt.contains("do not read the goal or load tools first"),
        "要明说别为它加载工具，否则模型会先失败一次再去加载：\n{prompt}"
    );

    // 第二轮起发短版，但短版必须**自包含**：一行来历 + 目标全文 + 两条
    // 填好的调用。早先短版只说「same objective and rules as above」，赌
    // 完整版还躺在上下文里——压缩会把这个赌注折掉，目标被人改过它又指向
    // 旧文案，为此还得维护一套「下轮重发完整版」的脏标记。
    let short = crate::tools::goal::goal_round_prompt(&goal, false);
    assert!(short.contains("Round 2 of 10"));
    assert!(
        short.contains("set by the user") && short.contains("把测试跑绿"),
        "短版丢了来历或目标全文——压缩/编辑之后它就指向空气：\n{short}"
    );
    assert!(
        short.contains(r#""revision":4"#) && short.matches("goal {").count() == 2,
        "短版仍要带两条填好的调用——revision 每轮可能变，不该让模型去回忆：\n{short}"
    );
    // 仍要比完整版短：短版逐轮追加，长散文只该在第一轮出现一次。
    assert!(
        short.len() < prompt.len(),
        "短版没短下来（{} vs {}）：\n{short}",
        short.len(),
        prompt.len()
    );
}

#[test]
fn readable_names_cover_all_built_in_tools_and_groups() {
    let mut missing_tools = tool_descriptions::all()
        .keys()
        .filter(|name| builtin_readable_tool_name(name).is_none())
        .cloned()
        .collect::<Vec<_>>();
    missing_tools.sort();
    assert!(
        missing_tools.is_empty(),
        "missing tool names: {missing_tools:?}"
    );

    let mut missing_groups = tool_descriptions::group_names()
        .into_iter()
        .filter(|group| builtin_readable_group_name(group).is_none())
        .collect::<Vec<_>>();
    missing_groups.sort();
    assert!(
        missing_groups.is_empty(),
        "missing tool group names: {missing_groups:?}"
    );
}

#[test]
fn ui_language_does_not_change_agent_tool_definitions() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let mut english = AppConfig::default();
    english.display.language = "en".to_string();
    let mut chinese = english.clone();
    chinese.display.language = "zh".to_string();

    let english = serde_json::to_value(builtin_registry(&english, &paths).definitions()).unwrap();
    let chinese = serde_json::to_value(builtin_registry(&chinese, &paths).definitions()).unwrap();

    assert_eq!(english, chinese);
}

#[test]
fn restricted_platform_registry_has_no_host_or_write_tools() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let registry = restricted_platform_registry(&AppConfig::default(), &paths);
    let names = registry.tool_names();

    for forbidden in [
        "run_command",
        "read_file",
        "write_file",
        "apply_patch",
        "vision_analyze",
        "subagent",
    ] {
        assert!(!names.iter().any(|name| name == forbidden), "{forbidden}");
    }
    for name in names {
        // 两个明示的 Writes 例外：都只写自己插件的目录，碰不到主机文件。
        // generate_image 写图片输出目录；manage_meme 写人格的表情库。
        if name == "generate_image" || name == "manage_meme" {
            continue;
        }
        assert_eq!(
            registry.permission(&name).unwrap(),
            ToolPermission::ReadOnly
        );
    }
    assert!(registry.contains("load_skill"));
    assert!(registry.contains("load_tools"));
    // With the plugin enabled, image generation is exposed to platforms.
    let mut config = AppConfig::default();
    config.plugins.image_generation.enabled = true;
    let registry = restricted_platform_registry(&config, &paths);
    assert!(registry.contains("generate_image"));
    let visible = registry.lazy_definitions(&Default::default());
    assert!(visible
        .iter()
        .any(|definition| definition.function.name == "load_tools"));
}

/// 09-09 起技能面整体只给 normal:dev 连 `load_skill` 都没有(它在 dev
/// 里列的还是默认人格的技能,而 dev 又没有创作工具)。
#[test]
fn skill_tools_are_normal_mode_only() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let config = AppConfig::default();
    let normal = build_tool_registry(&config, &paths, PersonaLane::Active, false).unwrap();
    let dev = build_tool_registry(&config, &paths, PersonaLane::Dev, false).unwrap();

    assert!(normal.contains("manage_skill"));
    assert!(!dev.contains("manage_skill"));
    assert!(normal.contains("load_skill"));
    assert!(!dev.contains("load_skill"));
}

#[test]
fn artifact_tools_are_only_added_by_the_webui_registration_step() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let config = AppConfig::default();
    let mut registry = builtin_registry(&config, &paths);
    assert!(!registry.contains("present_artifact"));

    // Edit/Read 统一后 WebUI 附加的只剩发布动作;创建/读取/打补丁走
    // edit/read 的 artifact: 命名空间。
    register_webui_artifact_tools(&mut registry, &config, &paths, "sess_webui");
    assert_eq!(
        registry.permission("present_artifact").unwrap(),
        ToolPermission::Presentation,
    );
    let definitions = registry.definitions();
    assert!(definitions
        .iter()
        .any(|definition| definition.function.name == "present_artifact"));
    assert!(!definitions
        .iter()
        .any(|definition| definition.function.name == "create_artifact"));
}

/// 内置脚本按头部的 Trust 位进受限注册表:divine(Trust: external)在,
/// read_clipboard(只给属主)不在;懒加载的分组照常能 load。
#[tokio::test]
async fn restricted_platform_can_load_the_divination_group() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let bundled =
        std::path::Path::new(miyu_base::WORKSPACE_ROOT).join("src/scripts/personas/default");
    let system = paths.system_scripts_dir.join("personas/default");
    std::fs::create_dir_all(&system).unwrap();
    for name in ["divine", "read_clipboard"] {
        std::fs::copy(bundled.join(name), system.join(name)).unwrap();
    }
    let registry = restricted_platform_registry(&AppConfig::default(), &paths);
    assert!(
        registry.contains("divine"),
        "Trust: external 的脚本应进受限注册表"
    );
    assert!(
        !registry.contains("read_clipboard"),
        "没写 Trust 的脚本只给属主"
    );
    let visible = registry.lazy_definitions(&Default::default());
    assert!(!visible
        .iter()
        .any(|definition| definition.function.name == "divine"));

    let output = registry
        .call("load_tools", r#"{"names":["group:divination"]}"#)
        .await
        .unwrap();
    let loaded = output
        .lines()
        .find_map(|line| line.strip_prefix("loaded_tools:"))
        .expect("loaded_tools line");
    assert!(loaded.split(',').any(|name| name.trim() == "divine"));
}
