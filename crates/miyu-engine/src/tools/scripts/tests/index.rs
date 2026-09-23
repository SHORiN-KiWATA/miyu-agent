//! 索引扫描、ID 校验与头部/索引合并。

use crate::tools::scripts::*;

#[test]
fn migrated_script_index_absolute_paths_follow_the_data_directory() {
    let temp = tempfile::tempdir().unwrap();
    let scripts_dir = temp.path().join("data/scripts");
    let legacy = temp.path().join("config/scripts/tool.sh");
    assert_eq!(
        resolve_script_path(&legacy.display().to_string(), &scripts_dir),
        scripts_dir.join("tool.sh")
    );
}

#[test]
fn auto_detects_executable_script() {
    let temp = tempfile::tempdir().unwrap();
    let script_path = temp.path().join("hello.sh");
    std::fs::write(
        &script_path,
        "#!/bin/bash\n# description: Say hello\n\necho hello",
    )
    .unwrap();
    let entry = auto_detect_script(&script_path).unwrap();
    assert_eq!(entry.id, "hello");
    // 一个显示名都没写:按 id 兜一个人话名,不再端出裸 id。
    assert_eq!(entry.display_name, "Hello");
    assert_eq!(entry.description, "Say hello");
    assert_eq!(entry.path, "hello.sh");
    assert!(entry.parameters.is_null());
}

#[test]
fn extracts_script_display_name_metadata() {
    let raw = "#!/bin/bash\n# 显示名称：电池护理\n# 描述：管理电池充电阈值\n\necho ok";
    let metadata = extract_metadata(raw);
    assert_eq!(metadata.display_names.zh, Some("电池护理".to_string()));
    assert_eq!(
        metadata.descriptions.zh,
        Some("管理电池充电阈值".to_string())
    );
}

/// 文件名里的连字符折成下划线:工具名两套规则(自动检测 vs manage_script)统一。
#[test]
fn auto_detect_uses_script_display_name_and_normalized_id() {
    let temp = tempfile::tempdir().unwrap();
    let script_path = temp.path().join("battery-care.sh");
    std::fs::write(
        &script_path,
        "#!/bin/bash\n# 显示名称：电池护理\n# 描述：管理电池充电阈值\n\necho ok",
    )
    .unwrap();
    let entry = auto_detect_script(&script_path).unwrap();
    assert_eq!(entry.id, "battery_care");
    // 只写了中文名:中文界面拿中文名,英文界面不回退中文而是按 id 兜底。
    // 断言跟着 locale 走,测试才能在两种语言环境下都成立。
    let expected = match miyu_base::i18n::locale() {
        miyu_base::i18n::Locale::Zh => "电池护理",
        miyu_base::i18n::Locale::En => "Battery care",
    };
    assert_eq!(entry.display_name, expected);
    assert_eq!(entry.description, "管理电池充电阈值");
}

/// 展示边界的兜底:`ScriptEntry.display_name` 空着就按 id 兜一个,但空值本身
/// 不被改写——register 落盘时不该把某次运行时 locale 挑出来的名字固化进
/// index.json。
#[test]
fn display_name_falls_back_to_a_humanized_id_at_the_display_boundary() {
    let mut entry = ScriptEntry::overlay("xhs_search".to_string(), "xhs-search".to_string());
    assert_eq!(entry_display_name(&entry), "Xhs search");
    entry.display_name = "小红书搜索".to_string();
    assert_eq!(entry_display_name(&entry), "小红书搜索");
}

#[test]
fn scan_finds_auto_detected_scripts() {
    let temp = tempfile::tempdir().unwrap();
    let scripts_dir = temp.path();
    std::fs::write(
        scripts_dir.join("greet.sh"),
        "#!/bin/bash\n# description: Greet user\n\necho hi",
    )
    .unwrap();
    let scan = scan_scripts(&[scripts_dir]).unwrap();
    assert_eq!(scan.entries.len(), 1);
    assert_eq!(scan.entries[0].id, "greet");
    assert!(scan.unregistered.is_empty());
}

#[test]
fn scan_merges_index_and_auto_detected() {
    let temp = tempfile::tempdir().unwrap();
    let scripts_dir = temp.path();
    std::fs::write(
        scripts_dir.join("index.json"),
        r#"{"scripts":[{"id":"custom","display_name":"自定义","description":"Custom tool","path":"custom.sh"}]}"#,
    )
    .unwrap();
    std::fs::write(scripts_dir.join("custom.sh"), "#!/bin/bash\necho custom").unwrap();
    std::fs::write(
        scripts_dir.join("auto.sh"),
        "#!/bin/bash\n# description: Auto detected\n\necho auto",
    )
    .unwrap();
    let scan = scan_scripts(&[scripts_dir]).unwrap();
    assert_eq!(scan.entries.len(), 2);
    let ids: Vec<&str> = scan.entries.iter().map(|e| e.id.as_str()).collect();
    assert!(ids.contains(&"custom"));
    assert!(ids.contains(&"auto"));
}

#[test]
fn scan_fills_empty_index_description_from_script_header() {
    let temp = tempfile::tempdir().unwrap();
    let scripts_dir = temp.path();
    std::fs::write(
        scripts_dir.join("index.json"),
        r#"{"scripts":[{"id":"custom","display_name":"自定义","description":"","path":"custom.sh"}]}"#,
    )
    .unwrap();
    std::fs::write(
        scripts_dir.join("custom.sh"),
        "#!/bin/bash\n# Description: Custom header description\n\necho custom",
    )
    .unwrap();
    let scan = scan_scripts(&[scripts_dir]).unwrap();
    assert_eq!(scan.entries.len(), 1);
    assert_eq!(scan.entries[0].description, "Custom header description");
}

/// index 是覆盖层:写了的字段压住头部,没写的从头部补。
#[test]
fn index_overrides_header_fields_and_header_fills_the_rest() {
    let temp = tempfile::tempdir().unwrap();
    let scripts_dir = temp.path();
    std::fs::write(
        scripts_dir.join("lookup.py"),
        "#!/usr/bin/env python3\n\
# Display name: 头部名\n\
# Description: Header description\n\
# Timeout: 60\n\
# Group: research\n\
# Argv: flags\n\
# Parameters: {\"type\":\"object\",\"properties\":{\"q\":{\"type\":\"string\"}}}\n\
print(1)\n",
    )
    .unwrap();
    std::fs::write(
        scripts_dir.join("index.json"),
        serde_json::to_string(&json!({
            "scripts": [{
                "id": "lookup",
                "path": "lookup.py",
                "description": "Index description",
                "timeout_seconds": 10
            }]
        }))
        .unwrap(),
    )
    .unwrap();

    let scan = scan_scripts(&[scripts_dir]).unwrap();
    assert_eq!(scan.entries.len(), 1);
    let entry = &scan.entries[0];
    assert_eq!(entry.description, "Index description");
    assert_eq!(entry.timeout_seconds, Some(10));
    assert_eq!(entry.display_name, "头部名");
    assert_eq!(entry.parameters["properties"]["q"]["type"], "string");
    assert_eq!(entry.groups, vec!["research"]);
    assert_eq!(entry.argv, ArgvMode::Flags);

    let spec = entry_to_spec(entry, scripts_dir, scripts_dir).unwrap();
    assert!(!spec.always_loaded, "有 schema 的脚本默认懒加载");
    assert!(
        matches!(spec.load_policy, LoadPolicy::Group),
        "头部给了分组就走 group 目录"
    );
    assert_eq!(spec.groups, vec!["research"]);
}

#[test]
fn header_parameters_make_auto_detected_script_carry_a_schema() {
    let temp = tempfile::tempdir().unwrap();
    let scripts_dir = temp.path();
    std::fs::write(
        scripts_dir.join("tool.sh"),
        "#!/bin/sh\n# Description: Tool\n# Parameters:\n# {\"type\":\"object\",\n#  \"properties\":{\"n\":{\"type\":\"integer\"}}}\necho\n",
    )
    .unwrap();
    let scan = scan_scripts(&[scripts_dir]).unwrap();
    assert_eq!(scan.entries.len(), 1);
    assert_eq!(
        scan.entries[0].parameters["properties"]["n"]["type"],
        "integer"
    );
}

#[test]
fn pure_non_ascii_file_name_is_listed_as_unregistered() {
    let temp = tempfile::tempdir().unwrap();
    let scripts_dir = temp.path();
    std::fs::write(
        scripts_dir.join("查天气"),
        "#!/bin/sh\n# Description: weather\necho\n",
    )
    .unwrap();
    let scan = scan_scripts(&[scripts_dir]).unwrap();
    assert!(scan.entries.is_empty());
    assert_eq!(scan.unregistered.len(), 1);
    assert_eq!(scan.unregistered[0].name, "查天气");
}

#[test]
fn scan_deduplicates_by_path() {
    let temp = tempfile::tempdir().unwrap();
    let scripts_dir = temp.path();
    let script = scripts_dir.join("dup.sh");
    std::fs::write(&script, "#!/bin/bash\n# description: Dup\n\necho dup").unwrap();
    std::fs::write(
        scripts_dir.join("index.json"),
        r#"{"scripts":[{"id":"alias1","display_name":"A1","description":"alias","path":"dup.sh"}]}"#,
    )
    .unwrap();
    let scan = scan_scripts(&[scripts_dir]).unwrap();
    assert_eq!(scan.entries.len(), 1);
}

#[test]
fn scan_user_dir_overrides_system_dir() {
    let sys_temp = tempfile::tempdir().unwrap();
    let user_temp = tempfile::tempdir().unwrap();
    std::fs::write(
        sys_temp.path().join("tool.sh"),
        "#!/bin/bash\n# description: System version\n\necho sys",
    )
    .unwrap();
    std::fs::write(
        user_temp.path().join("tool.sh"),
        "#!/bin/bash\n# description: User version\n\necho user",
    )
    .unwrap();
    let scan = scan_scripts(&[sys_temp.path(), user_temp.path()]).unwrap();
    assert_eq!(scan.entries.len(), 1);
    assert_eq!(scan.entries[0].description, "User version");
}

#[test]
fn scan_lists_scripts_without_descriptions_as_unregistered() {
    let temp = tempfile::tempdir().unwrap();
    let scripts_dir = temp.path();
    std::fs::write(scripts_dir.join("unknown.sh"), "#!/bin/bash\necho unknown").unwrap();

    let scan = scan_scripts(&[scripts_dir]).unwrap();
    assert!(scan.entries.is_empty());
    assert_eq!(scan.unregistered.len(), 1);
    assert_eq!(scan.unregistered[0].name, "unknown");
    assert_eq!(
        scan.unregistered[0].path,
        scripts_dir.join("unknown.sh").to_string_lossy()
    );
}

/// 09-05 起脚本一律默认懒加载;index 里显式 always_loaded:true 才进顶层。
#[test]
fn scripts_default_to_lazy_and_index_always_loaded_forces_top_level() {
    let temp = tempfile::tempdir().unwrap();
    let scripts_dir = temp.path();
    std::fs::write(
        scripts_dir.join("generic.sh"),
        "#!/bin/bash\n# Description: Generic script\n\necho generic",
    )
    .unwrap();
    std::fs::write(
        scripts_dir.join("pinned.sh"),
        "#!/bin/bash\n# Description: Pinned script\n\necho pinned",
    )
    .unwrap();
    std::fs::write(
        scripts_dir.join("index.json"),
        serde_json::to_string(&json!({
            "scripts": [
                { "id": "generic_script", "path": "generic.sh" },
                { "id": "pinned_script", "path": "pinned.sh", "always_loaded": true }
            ]
        }))
        .unwrap(),
    )
    .unwrap();

    let scan = scan_scripts(&[scripts_dir]).unwrap();
    let specs = script_specs(&scan.entries, scripts_dir, scripts_dir);
    let mut registry = ToolRegistry::new();
    crate::tools::load_tools::register(&mut registry);
    registry
        .replace_script_tools(specs, scan.unregistered)
        .unwrap();

    let definitions = registry.lazy_definitions(&BTreeSet::new());
    let names = definitions
        .iter()
        .map(|definition| definition.function.name.as_str())
        .collect::<BTreeSet<_>>();
    assert!(!names.contains("generic_script"));
    assert!(names.contains("pinned_script"));
    // 生产走 stub 档:没被钉住的脚本以桩的形式留在目录里,按需经 load_tools 取全文;
    // 钉住的照样在。(旧 lazy 档把目标清单写进 load_tools 描述,随 lazy 档退役。)
    let stubs = registry
        .stub_definitions()
        .into_iter()
        .map(|definition| definition.function.name)
        .collect::<BTreeSet<_>>();
    assert!(stubs.contains("generic_script"));
    assert!(stubs.contains("pinned_script"));
}

#[test]
fn invalid_external_index_entry_does_not_hide_valid_local_scripts() {
    let scripts_temp = tempfile::tempdir().unwrap();
    let external_temp = tempfile::tempdir().unwrap();
    let scripts_dir = scripts_temp.path();
    let external_script = external_temp.path().join("external.sh");
    std::fs::write(
        &external_script,
        "#!/bin/bash\n# Description: External\n\necho external",
    )
    .unwrap();
    std::fs::write(
        scripts_dir.join("local.sh"),
        "#!/bin/bash\n# Description: Local\n\necho local",
    )
    .unwrap();
    std::fs::write(
        scripts_dir.join("index.json"),
        serde_json::to_string(&json!({
            "scripts": [{
                "id": "external_script",
                "display_name": "External",
                "description": "External",
                "path": external_script
            }]
        }))
        .unwrap(),
    )
    .unwrap();

    let scan = scan_scripts(&[scripts_dir]).unwrap();
    assert_eq!(scan.entries.len(), 1);
    assert_eq!(scan.entries[0].id, "local");
}

#[test]
fn malformed_index_entries_do_not_hide_valid_scripts() {
    let temp = tempfile::tempdir().unwrap();
    let scripts_dir = temp.path();
    std::fs::write(
        scripts_dir.join("valid.sh"),
        "#!/bin/bash\n# Description: Valid\n\necho valid",
    )
    .unwrap();
    std::fs::write(scripts_dir.join("invalid.sh"), "not a script").unwrap();
    std::fs::write(
        scripts_dir.join("index.json"),
        serde_json::to_string(&json!({
            "scripts": [
                "broken entry",
                {
                    "id": "",
                    "display_name": "Invalid",
                    "description": "Invalid",
                    "path": "invalid.sh"
                },
                {
                    "id": "valid_script",
                    "display_name": "Valid",
                    "description": "Valid",
                    "path": "valid.sh"
                }
            ]
        }))
        .unwrap(),
    )
    .unwrap();

    let scan = scan_scripts(&[scripts_dir]).unwrap();
    assert_eq!(scan.entries.len(), 1);
    assert_eq!(scan.entries[0].id, "valid_script");
}

/// 扫描根与来源层:老布局(内置 `<system>/personas/<人格>/`)与新布局
/// (`<资源根>/personas/<人格>/{scripts,skills/<技能>/scripts}`)并存,
/// 用户层最后(优先级最高)。内置脚本装在 `personas/` 下,自定义人格在资源树里
/// 没有自己的目录=天然拿不到;09-13 起出厂人格那层照扫,自定义人格能在引导里
/// 逐个勾内置脚本,是否真挂由 `prepare_script_refresh` 按清单白名单裁决。
/// 覆盖顺序低→高:内置平台 < 内置默认 < 内置人格 < 新布局 < 全局 < 全局人格。
#[test]
fn script_scan_roots_resolve_persona_substructure_per_layer() {
    let temp = tempfile::tempdir().unwrap();
    let mut paths = miyu_base::paths::MiyuPaths::new().unwrap();
    paths.system_scripts_dir = temp.path().join("system");
    paths.scripts_dir = temp.path().join("data/scripts");

    // 新布局的资源根 = 内置脚本目录的父目录 = temp 本身。
    let personas = temp.path().join("personas");
    let layers = |config: &miyu_base::config::AppConfig| {
        script_scan_root_layers(config, &paths)
            .into_iter()
            .map(|root| (root.path, root.origin.layer))
            .collect::<Vec<_>>()
    };
    let default_config = miyu_base::config::AppConfig::default();
    let roots = layers(&default_config);
    assert_eq!(
        roots,
        vec![
            (paths.system_scripts_dir.clone(), ScriptLayerKind::Builtin),
            (
                paths.system_scripts_dir.join("personas/default"),
                ScriptLayerKind::BuiltinPersona
            ),
            (
                personas.join("default/scripts"),
                ScriptLayerKind::BuiltinPersona
            ),
            (paths.scripts_dir.clone(), ScriptLayerKind::Global),
            (
                paths.scripts_dir.join("personas/default"),
                ScriptLayerKind::Persona
            ),
        ],
        "默认人格:老布局内置两层 + 新布局内置一层 + 用户两层"
    );

    let mut custom = miyu_base::config::AppConfig::default();
    custom.prompt.active_persona = "alter".to_string();
    let custom_roots = layers(&custom);
    assert_eq!(
        custom_roots,
        vec![
            (paths.system_scripts_dir.clone(), ScriptLayerKind::Builtin),
            (
                paths.system_scripts_dir.join("personas/default"),
                ScriptLayerKind::BuiltinPersona
            ),
            (
                paths.system_scripts_dir.join("personas/alter"),
                ScriptLayerKind::BuiltinPersona
            ),
            (
                personas.join("default/scripts"),
                ScriptLayerKind::BuiltinPersona
            ),
            (
                personas.join("alter/scripts"),
                ScriptLayerKind::BuiltinPersona
            ),
            (paths.scripts_dir.clone(), ScriptLayerKind::Global),
            (
                paths.scripts_dir.join("personas/alter"),
                ScriptLayerKind::Persona
            ),
        ],
        "自定义人格:出厂层照扫(可选件),再各多一层不存在的 personas/alter"
    );
    // 顶层(平台)与出厂层无论人格都在;差异只在 personas/<人格> 这一维。
    assert_eq!(roots[0], custom_roots[0]);
    assert_eq!(roots[1], custom_roots[1]);
    assert_eq!(roots[3], custom_roots[5]);
}

/// 用户机器实查(09-05):`gpustoggle.bak`(无描述头)与 index 里的 gpustoggle
/// 同 stem,旧扫描把正主从 entries 里删掉、塞进未注册清单。
#[test]
fn backup_sibling_does_not_hide_the_indexed_script() {
    let temp = tempfile::tempdir().unwrap();
    let scripts_dir = temp.path();
    std::fs::write(scripts_dir.join("gpustoggle"), "#!/bin/bash\necho real\n").unwrap();
    std::fs::write(
        scripts_dir.join("gpustoggle.bak"),
        "#!/bin/bash\necho old\n",
    )
    .unwrap();
    std::fs::write(
        scripts_dir.join("gpustoggle.orig"),
        "#!/bin/bash\n# Description: stale copy\necho older\n",
    )
    .unwrap();
    std::fs::write(
        scripts_dir.join("index.json"),
        serde_json::to_string(&json!({
            "scripts": [{
                "id": "gpustoggle",
                "description": "Toggle the GPU",
                "path": "gpustoggle",
                "groups": ["vfio"],
                "load_policy": "summary"
            }]
        }))
        .unwrap(),
    )
    .unwrap();

    let scan = scan_scripts(&[scripts_dir]).unwrap();
    assert_eq!(scan.entries.len(), 1);
    assert_eq!(scan.entries[0].description, "Toggle the GPU");
    assert!(scan.unregistered.is_empty(), "{:?}", scan.unregistered);
    // index 自己写的 groups+summary 组合不被改成 group。
    let spec = entry_to_spec(&scan.entries[0], scripts_dir, scripts_dir).unwrap();
    assert!(matches!(spec.load_policy, LoadPolicy::Summary));
}

/// 同目录里一个同 stem 的别名文件有描述头时,同样不能顶掉 index 正主。
#[test]
fn same_stem_sibling_with_header_does_not_replace_indexed_entry() {
    let temp = tempfile::tempdir().unwrap();
    let scripts_dir = temp.path();
    std::fs::write(
        scripts_dir.join("tool.py"),
        "#!/usr/bin/env python3\nprint(1)\n",
    )
    .unwrap();
    std::fs::write(
        scripts_dir.join("tool.sh"),
        "#!/bin/bash\n# Description: shell twin\necho\n",
    )
    .unwrap();
    std::fs::write(
        scripts_dir.join("index.json"),
        r#"{"scripts":[{"id":"tool","description":"Python one","path":"tool.py"}]}"#,
    )
    .unwrap();
    let scan = scan_scripts(&[scripts_dir]).unwrap();
    assert_eq!(scan.entries.len(), 1);
    assert_eq!(scan.entries[0].description, "Python one");
    assert!(scan.entries[0].path.ends_with("tool.py"));
}

/// 头部的五个清单字段一路落到 ToolSpec:信任位、权限、桩示例、指路句、前置工具;
/// index 里显式写的仍是覆盖层。
#[test]
fn manifest_fields_reach_the_tool_spec() {
    let raw = "#!/bin/sh\n\
# Description: Weather lookup\n\
# Trust: external\n\
# Permission: read-only\n\
# Example: {\"city\":\"Tokyo\"}\n\
# Hint: web_fetch: Fetch the page with web_fetch.\n\
# Requires: check_issue\n\
echo ok";
    let metadata = extract_metadata(raw);
    let mut entry = ScriptEntry::overlay("weather".to_string(), "weather".to_string());
    merge_header_defaults(&mut entry, &metadata);
    let spec = entry_to_spec(&entry, Path::new("."), Path::new(".")).unwrap();
    assert_eq!(spec.trust, crate::tools::ToolTrust::External);
    assert_eq!(spec.permission, crate::tools::ToolPermission::ReadOnly);
    assert_eq!(spec.stub_example.as_deref(), Some("{\"city\":\"Tokyo\"}"));
    assert_eq!(
        spec.cross_hints,
        vec![(
            "web_fetch".to_string(),
            " Fetch the page with web_fetch.".to_string()
        )]
    );
    assert_eq!(spec.requires_prior, vec!["check_issue".to_string()]);

    // 缺省:只给属主、writes、无示例。
    let mut plain = ScriptEntry::overlay("plain".to_string(), "plain".to_string());
    plain.description = "Plain".to_string();
    let spec = entry_to_spec(&plain, Path::new("."), Path::new(".")).unwrap();
    assert_eq!(spec.trust, crate::tools::ToolTrust::Owner);
    assert_eq!(spec.permission, crate::tools::ToolPermission::Writes);
    assert!(spec.stub_example.is_none());

    // index 覆盖层写了 trust 就以 index 为准。
    let mut pinned = ScriptEntry::overlay("pinned".to_string(), "pinned".to_string());
    pinned.trust = crate::tools::ToolTrust::External;
    merge_header_defaults(
        &mut pinned,
        &extract_metadata("#!/bin/sh\n# Description: x\n# Trust: owner\necho"),
    );
    assert_eq!(pinned.trust, crate::tools::ToolTrust::External);
}

/// 新布局(09-23)的隔离资源树:`<root>/share/scripts` 是内置脚本目录
/// (`system_scripts_dir`),它旁边的 `<root>/share/personas/` 就是新布局的资源根。
fn resource_paths(root: &std::path::Path) -> miyu_base::paths::MiyuPaths {
    let mut paths = miyu_base::paths::MiyuPaths::new().unwrap();
    paths.root_dir = root.join("home");
    paths.data_dir = root.join("home/data");
    paths.state_dir = root.join("home/state");
    paths.cache_dir = root.join("cache");
    paths.scripts_dir = root.join("home/extensions/scripts");
    paths.system_scripts_dir = root.join("share/scripts");
    paths
}

fn write_test_script(dir: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, body).unwrap();
    path
}

/// 新布局:人格自己的脚本 `<前缀>/personas/<人格>/scripts/` 与技能带路的脚本
/// `<前缀>/personas/<人格>/skills/<技能名>/scripts/` 都要被扫到,并标出来源层;
/// 技能带路那条还要带技能名——第二批据此做「不单独列行 + 开关连动」。
#[test]
fn new_persona_layout_scans_scripts_and_marks_skill_carried_ones() {
    let temp = tempfile::tempdir().unwrap();
    let paths = resource_paths(temp.path());
    let config = miyu_base::config::AppConfig::default();

    // 人格自己的脚本。
    write_test_script(
        &temp.path().join("share/personas/default/scripts"),
        "persona_tool",
        "#!/bin/sh\n# Description: A persona-owned tool\n",
    );
    // 技能带路的脚本 + 同一棵技能树里的 SKILL.md。
    let skill_dir = temp.path().join("share/personas/default/skills/travel");
    write_test_script(
        &skill_dir.join("scripts"),
        "flight",
        "#!/bin/sh\n# Description: A skill-carried tool\n",
    );
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: travel\ndescription: Plan a trip\n---\n\nBody.",
    )
    .unwrap();

    let roots = script_scan_root_layers(&config, &paths);
    let scan = scan_scripts_at(&roots).unwrap();
    let by_id = |id: &str| scan.entries.iter().find(|entry| entry.id == id).unwrap();

    let persona_tool = by_id("persona_tool");
    assert_eq!(persona_tool.origin.layer, ScriptLayerKind::BuiltinPersona);
    assert!(persona_tool.origin.is_builtin());
    assert_eq!(persona_tool.origin.skill, None);
    assert!(is_builtin_script(&paths, persona_tool));

    let flight = by_id("flight");
    assert_eq!(flight.origin.layer, ScriptLayerKind::BuiltinSkill);
    assert_eq!(flight.origin.skill.as_deref(), Some("travel"));
    assert!(is_builtin_script(&paths, flight));

    // 同一棵树里的技能也走读盘,而且认得带路脚本属于哪个技能。
    let loaded = miyu_core::skills::load("travel", &config, &paths).unwrap();
    assert_eq!(loaded.source, miyu_core::skills::SkillSource::BuiltIn);
    assert!(loaded.files.iter().any(|file| file.ends_with("scripts")));
}

/// 老布局 `<前缀>/scripts/personas/<人格>/` 必须继续被扫到:升级后老用户装在
/// 旧路径下的脚本不能凭空消失,而且仍算内置层(人格白名单照旧管得着)。
#[test]
fn legacy_persona_script_layout_still_scans() {
    let temp = tempfile::tempdir().unwrap();
    let paths = resource_paths(temp.path());
    let config = miyu_base::config::AppConfig::default();
    let legacy = temp.path().join("share/scripts/personas/default");
    write_test_script(
        &legacy,
        "legacy_tool",
        "#!/bin/sh\n# Description: Legacy layout tool\n",
    );

    let roots = script_scan_root_layers(&config, &paths);
    assert!(roots.iter().any(|root| root.path == legacy));
    let scan = scan_scripts_at(&roots).unwrap();
    let entry = scan
        .entries
        .iter()
        .find(|entry| entry.id == "legacy_tool")
        .unwrap();
    assert_eq!(entry.origin.layer, ScriptLayerKind::BuiltinPersona);
    assert!(is_builtin_script(&paths, entry));
}

/// 09-23:一个脚本进不进模型的常驻 tools 数组,只看它住在哪——
/// `skills/<技能名>/scripts/` 里的由技能带路(不进面,可经工具桥调用),
/// 其余照旧进面。判据是 layout,不再看脚本头部的一行声明。
#[test]
fn skill_carried_scripts_stay_off_the_tool_face_by_layout() {
    let temp = tempfile::tempdir().unwrap();
    let paths = resource_paths(temp.path());
    let config = miyu_base::config::AppConfig::default();

    // 人格自己的脚本(住在 `scripts/`):进面。
    write_test_script(
        &temp.path().join("share/personas/default/scripts"),
        "persona_tool",
        "#!/bin/sh\n# Description: A persona-owned tool\n",
    );
    // 技能带路的脚本(住在 `skills/<技能名>/scripts/`):不进面。
    write_test_script(
        &temp
            .path()
            .join("share/personas/default/skills/travel/scripts"),
        "flight",
        "#!/bin/sh\n# Description: A skill-carried tool\n",
    );

    let roots = script_scan_root_layers(&config, &paths);
    let scan = scan_scripts_at(&roots).unwrap();
    let specs = script_specs(&scan.entries, &paths.scripts_dir, &paths.cache_dir);
    let exposed = |id: &str| {
        specs
            .iter()
            .find(|spec| spec.name == id)
            .unwrap_or_else(|| panic!("{id} missing from specs"))
            .exposed
    };
    assert!(exposed("persona_tool"), "住在 scripts/ 的照旧进常驻面");
    assert!(!exposed("flight"), "技能带路的不进常驻面");

    // 头部写没写声明都不影响:同一份脚本,住哪儿就是哪儿。
    let mut registry = ToolRegistry::new();
    crate::tools::load_tools::register(&mut registry);
    registry
        .replace_script_tools(specs, scan.unregistered)
        .unwrap();
    let names: Vec<String> = registry
        .stub_definitions()
        .into_iter()
        .map(|definition| definition.function.name)
        .collect();
    assert!(names.contains(&"persona_tool".to_string()));
    assert!(
        !names.contains(&"flight".to_string()),
        "技能带路的脚本连桩都不该进常驻面"
    );
}
