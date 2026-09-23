/// Regression: the built-in description overlay
/// (`descriptions/subagent.json`) wholesale replaces the subagent schema at
/// register time — a param added only in code silently vanishes from
/// what the LLM sees.
#[test]
fn subagent_definition_includes_tier() {
    let config = miyu_base::config::AppConfig::default();
    let paths = miyu_base::paths::MiyuPaths::new().unwrap();
    let registry = super::builtin_registry(&config, &paths);
    let defs = registry.definitions();
    let subagent = defs
        .iter()
        .find(|d| d.function.name == "subagent")
        .expect("subagent registered");
    let props = subagent.function.parameters.get("properties").unwrap();
    assert!(
        props.get("tier").is_some(),
        "tier missing: {}",
        subagent.function.parameters
    );
    // 全未配置=零追加(08-16 tools 瘦身:三行"未配置"是零信息,还把
    // 动态文本焊进 tools 数组);配置了档位才出现状态。
    assert!(!subagent.function.description.contains("cheap=["));
}

/// The description is constant bytes: configuring tier pools must not
/// change it (a config-derived suffix would re-key the prompt cache on
/// every pool edit), and the tier enum carries the four current names.
#[test]
fn subagent_description_is_constant_and_lists_the_four_tiers() {
    let paths = miyu_base::paths::MiyuPaths::new().unwrap();
    let bare = miyu_base::config::AppConfig::default();
    let bare_subagent = super::builtin_registry(&bare, &paths)
        .definitions()
        .into_iter()
        .find(|d| d.function.name == "subagent")
        .unwrap();

    let mut config = miyu_base::config::AppConfig::default();
    let provider_id = config.active_provider.clone();
    let provider = config
        .providers
        .iter_mut()
        .find(|provider| provider.id == provider_id)
        .unwrap();
    provider.models.push("mini-a".to_string());
    config
        .toggle_tier_model(miyu_base::config::ModelTier::Cheap, &provider_id, "mini-a")
        .unwrap();
    let subagent = super::builtin_registry(&config, &paths)
        .definitions()
        .into_iter()
        .find(|d| d.function.name == "subagent")
        .unwrap();
    assert_eq!(
        subagent.function.description,
        bare_subagent.function.description
    );
    assert!(!subagent.function.description.contains("cheap=["));
    let schema = serde_json::to_string(&subagent.function.parameters).unwrap();
    for tier in ["lite", "cheap", "standard", "flagship"] {
        assert!(schema.contains(&format!("\"{tier}\"")), "{schema}");
    }
    assert!(
        !schema.contains("balanced") && !schema.contains("strong"),
        "{schema}"
    );
}

/// 单件工具契约的 token 上限（发给模型的 ToolDefinition 全文，o200k）。
///
/// 超了不是"写得细"，是把"调用之后才用得上的知识"塞进了每回合常驻的
/// 那一份里——那类内容该写进工具自己的输出，或者写进技能正文。
const TOOL_TOKEN_BUDGET: usize = 550;

/// 仓库自带的整个工具面（`descriptions/*.json` + 内置脚本头）的 token 上限。
///
/// 09-21 瘦身后实测 9994（60 件），留约 5% 余量。加一件工具就得有人从别处
/// 腾出来，这正是这道闸的意思：工具面是一份公共预算，不是可以各自无限追加
/// 的地方。
const TOOL_FACE_TOKEN_BUDGET: usize = 10_500;

fn definition_tokens(name: &str, description: &str, parameters: &serde_json::Value) -> usize {
    let definition = miyu_core::llm::ToolDefinition {
        kind: "function",
        function: miyu_core::llm::FunctionDefinition {
            name: name.to_string(),
            description: description.to_string(),
            parameters: parameters.clone(),
        },
    };
    miyu_base::token_counter::count(&serde_json::to_string(&definition).unwrap())
}

/// 仓库自带工具面的 token 预算闸（量尺见 `token_diet_baseline_probe`）。
///
/// 读的是两处真相源本身（`src/tools/descriptions/*.json` 与内置脚本头），
/// 不建注册表——注册表要扫本机的脚本目录，装机环境会把结果搅乱。
/// 技能带路的脚本住在 `personas/<人格>/skills/<技能名>/scripts/`，不在本探针扫的
/// 目录里，自然不占预算。
#[test]
fn bundled_tool_face_stays_within_its_token_budget() {
    // 注册着、但不进 tools 数组的内置工具(`ToolSpec::with_exposed(false)`):
    // 技能带路的配置类动作。它们不占常驻预算,所以不进这本账。
    const SKILL_ONLY_BUILTINS: &[&str] = &["manage_script", "manage_skill"];
    let mut rows: Vec<(String, usize)> = crate::tools::tool_descriptions::all()
        .values()
        .filter(|description| !SKILL_ONLY_BUILTINS.contains(&description.name.as_str()))
        .map(|description| {
            (
                description.name.clone(),
                definition_tokens(
                    &description.name,
                    &description.description,
                    &description.parameters,
                ),
            )
        })
        .collect();

    let scripts_dir =
        std::path::Path::new(miyu_base::WORKSPACE_ROOT).join("src/personas/default/scripts");
    let entries = std::fs::read_dir(&scripts_dir)
        .unwrap_or_else(|error| panic!("{}: {error}", scripts_dir.display()));
    for entry in entries {
        let path = entry.unwrap().path();
        if !path.is_file() {
            continue;
        }
        let Some(raw) = super::scripts::header::read_header(&path) else {
            continue;
        };
        let metadata = super::scripts::header::extract_metadata(&raw);
        let Some(description) = metadata.descriptions.en.as_deref() else {
            continue;
        };
        let name = metadata.id.clone().unwrap_or_else(|| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .replace(|c: char| !c.is_ascii_alphanumeric(), "_")
        });
        let parameters = metadata
            .parameters
            .clone()
            .unwrap_or_else(|| serde_json::json!({"type": "object", "properties": {}}));
        let tokens = definition_tokens(&name, description, &parameters);
        rows.push((name, tokens));
    }

    rows.sort_by_key(|(_, tokens)| std::cmp::Reverse(*tokens));
    let over: Vec<&(String, usize)> = rows
        .iter()
        .filter(|(_, tokens)| *tokens > TOOL_TOKEN_BUDGET)
        .collect();
    assert!(
        over.is_empty(),
        "these tool contracts are over the {TOOL_TOKEN_BUDGET} token budget: {over:?}\n\
         move call-time knowledge into the tool's own output, or into a skill"
    );
    let total: usize = rows.iter().map(|(_, tokens)| tokens).sum();
    assert!(
        total <= TOOL_FACE_TOKEN_BUDGET,
        "the bundled tool face costs {total} tokens, over the {TOOL_FACE_TOKEN_BUDGET} budget \
         ({} tools). Heaviest: {:?}",
        rows.len(),
        &rows[..rows.len().min(5)]
    );
}

/// 量尺：`cargo test --lib token_diet_baseline -- --ignored --nocapture`
///
/// token 瘦身专项的基线：三套 registry 在 stub（默认发送形态）与 full
/// （懒加载展开上限）两种形态下，发给 LLM 的 tools 数组的真实 o200k
/// token 数，附逐工具排行。默认 AppConfig，不含平台插件回合注册的工具。
#[test]
#[ignore]
fn token_diet_baseline_probe() {
    use crate::tools::tests::test_paths;
    use crate::tools::{builtin_registry, dev_registry, restricted_platform_registry, AppConfig};
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let config = AppConfig::default();
    for (label, registry) in [
        ("normal", builtin_registry(&config, &paths)),
        ("dev", dev_registry(&config, &paths)),
        ("restricted", restricted_platform_registry(&config, &paths)),
    ] {
        // 两档都走 `request_definitions`：量尺要量真发出去的那一份，不是
        // 注册表里有什么(full 档 load_tools 注册着但不再发送)。
        for (variant, defs) in [
            ("stub", registry.request_definitions(true)),
            ("full", registry.request_definitions(false)),
        ] {
            let whole = serde_json::to_string(&defs).unwrap();
            let tokens = miyu_base::token_counter::count(&whole);
            eprintln!(
                "[{label}/{variant}] tools={} bytes={} tokens={}",
                defs.len(),
                whole.len(),
                tokens
            );
            let mut rows: Vec<(String, usize, usize)> = defs
                .iter()
                .map(|d| {
                    let s = serde_json::to_string(d).unwrap();
                    (
                        d.function.name.clone(),
                        s.len(),
                        miyu_base::token_counter::count(&s),
                    )
                })
                .collect();
            rows.sort_by_key(|r| std::cmp::Reverse(r.2));
            for (name, bytes, toks) in rows {
                eprintln!("  {toks:>6} tok {bytes:>6} B  {name}");
            }
        }
    }
}
