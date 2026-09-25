use super::{ToolRegistry, ToolSpec};
use anyhow::{Context, Result};
use miyu_base::config::AppConfig;
use miyu_base::paths::MiyuPaths;
use miyu_core::skills::{self, SkillEntry, SkillScope};
use serde_json::{json, Value};

/// 技能目录块的开头，也是指令源认「最近一份」的凭据（`agent::instruction_source`）。
pub(crate) const AVAILABLE_SKILLS_TAG: &str = "<available-skills";

/// 模型最近看到的目录里还有技能、现在一件都没了时补的那一句。
pub(crate) const NO_SKILLS_NOTICE: &str = "<available-skills/>";

pub fn register_skills(
    registry: &mut ToolRegistry,
    config: &AppConfig,
    paths: &MiyuPaths,
) -> Result<()> {
    let (entries, fingerprint) = stable_catalog(config, paths)?;
    register_load_skill(registry, config.clone(), paths.clone());
    registry.set_skill_catalog(fingerprint, catalog_block(&entries));
    Ok(())
}

pub(crate) struct SkillCatalogSnapshot {
    entries: Vec<SkillEntry>,
    fingerprint: [u8; 32],
}

pub(crate) fn prepare_skill_refresh(
    current: Option<[u8; 32]>,
    config: &AppConfig,
    paths: &MiyuPaths,
) -> Result<Option<SkillCatalogSnapshot>> {
    let fingerprint = skills::catalog_fingerprint(config, paths)?;
    if current == Some(fingerprint) {
        return Ok(None);
    }
    let (entries, fingerprint) = stable_catalog(config, paths)?;
    Ok(Some(SkillCatalogSnapshot {
        entries,
        fingerprint,
    }))
}

/// 目录换新。`load_skill` 的描述是常量（09-25），不用重新注册：它加载时读的是盘上
/// 此刻的技能。
pub(crate) fn apply_skill_refresh(registry: &mut ToolRegistry, snapshot: SkillCatalogSnapshot) {
    registry.set_skill_catalog(snapshot.fingerprint, catalog_block(&snapshot.entries));
}

fn stable_catalog(config: &AppConfig, paths: &MiyuPaths) -> Result<(Vec<SkillEntry>, [u8; 32])> {
    for _ in 0..3 {
        let before = skills::catalog_fingerprint(config, paths)?;
        let entries = skills::discover(config, paths)?;
        let after = skills::catalog_fingerprint(config, paths)?;
        if before == after {
            return Ok((entries, after));
        }
    }
    anyhow::bail!("skill catalog kept changing while it was being refreshed")
}

/// 五件 Skill 创作工具合并成 `manage_skill`(08-17):create/update/delete/
/// publish/list_drafts 是同一条创作流水线上的五个动作。
pub fn register_authoring(registry: &mut ToolRegistry, config: AppConfig, paths: MiyuPaths) {
    registry.register(
        ToolSpec::new(
            "manage_skill",
            "Author Miyu skills. action=create opens a hidden draft for a new skill; action=update copies an existing skill into an isolated draft; change only the returned draft with the edit tool, then action=publish validates and atomically publishes it (create drafts never overwrite; update drafts fail if the live skill changed meanwhile). action=delete permanently removes a user skill; action=list_drafts lists retained drafts (drafts untouched for 30 days are pruned first). Scripts inside a skill stay resources and are never registered as tools.",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["create", "update", "delete", "publish", "list_drafts"],
                        "description": "Which authoring step to run."
                    },
                    "name": {
                        "type": "string",
                        "pattern": "^[a-z0-9]+(-[a-z0-9]+)*$",
                        "description": "Skill name, required for create/update/delete. Must follow the Agent Skills naming rules."
                    },
                    "description": {
                        "type": "string",
                        "description": "action=create: what the skill does and when it should be used."
                    },
                    "scope": {
                        "type": "string",
                        "enum": ["global", "persona"],
                        "description": "global is available to every persona; persona belongs to the current persona. Required for update/delete; for create it defaults to persona (choose global only to share the skill with every persona)."
                    },
                    "draft_id": {
                        "type": "string",
                        "description": "action=publish: draft ID returned by create or update."
                    }
                },
                "required": ["action"],
                "additionalProperties": false
            }),
            move |args| {
                let config = config.clone();
                let paths = paths.clone();
                async move {
                    tokio::task::spawn_blocking(move || {
                        match args.get("action").and_then(Value::as_str).unwrap_or_default() {
                            "create" => create_skill(args, &config, &paths),
                            "update" => update_skill(args, &config, &paths),
                            "delete" => delete_skill(args, &config, &paths),
                            "publish" => publish_skill(args, &paths),
                            "list_drafts" => Ok(serde_json::to_string_pretty(&json!({
                                "ok": true,
                                "drafts": skills::list_drafts(&paths)?,
                            }))?),
                            other => anyhow::bail!(
                                "unknown action: {other}; expected create, update, delete, publish or list_drafts"
                            ),
                        }
                    })
                    .await
                    .context("skill authoring worker stopped")?
                }
            },
        )
        // 不进 tools 数组(09-21):写技能是一周用不了一次的动作,而 skill-creator
        // 技能本来就在目录里讲「怎么写技能」——工具是那份技能的执行手段,该跟
        // 它走。注册照旧,`miyu tool-call` 与工具桥不受影响。
        .with_exposed(false)
        .writes(),
    );
}

fn register_load_skill(registry: &mut ToolRegistry, config: AppConfig, paths: MiyuPaths) {
    // 描述是常量，真相源是 `descriptions/load_skill.json`（这里只是占位）。技能目录
    // 09-25 起不拼进来：目录一变 tools 的字节就变，所有在线会话下一轮整段缓存作废
    // （B13）。目录改由回合尾巴发，变了就再发一份。
    registry.register(ToolSpec::new(
        "load_skill",
        "Load a specialized skill's full instructions and resources into the conversation.",
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "The exact skill name from the available skills list."
                }
            },
            "required": ["name"],
            "additionalProperties": false
        }),
        move |args| {
            let config = config.clone();
            let paths = paths.clone();
            async move {
                tokio::task::spawn_blocking(move || load_skill(args, &config, &paths))
                    .await
                    .context("skill loader worker stopped")?
            }
        },
    ));
}

fn load_skill(args: Value, config: &AppConfig, paths: &MiyuPaths) -> Result<String> {
    let name = required_string(&args, "name")?;
    let loaded = skills::load(&name, config, paths)?;
    let base_dir = loaded
        .base_dir
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "built-in".to_string());
    let files = if loaded.files.is_empty() {
        String::new()
    } else {
        format!(
            "\n<skill_files>\n{}\n</skill_files>",
            loaded
                .files
                .iter()
                .map(|path| format!("  <file>{}</file>", xml_escape(&path.display().to_string())))
                .collect::<Vec<_>>()
                .join("\n")
        )
    };
    let metadata = skill_metadata_xml(&loaded.metadata);
    Ok(format!(
        "<skill_content name=\"{}\" source=\"{}\">\n{}\n<skill_instructions format=\"markdown\">\n{}\n</skill_instructions>\n\n<skill_base_dir>{}</skill_base_dir>{}\n</skill_content>",
        xml_escape(&loaded.metadata.name),
        loaded.source.as_str(),
        metadata,
        xml_escape(&loaded.body),
        xml_escape(&base_dir),
        files,
    ))
}

fn skill_metadata_xml(metadata: &miyu_core::skills::SkillMetadata) -> String {
    let mut fields = vec![format!(
        "  <description>{}</description>",
        xml_escape(&metadata.description)
    )];
    if let Some(license) = &metadata.license {
        fields.push(format!("  <license>{}</license>", xml_escape(license)));
    }
    if let Some(compatibility) = &metadata.compatibility {
        fields.push(format!(
            "  <compatibility>{}</compatibility>",
            xml_escape(compatibility)
        ));
    }
    if let Some(allowed_tools) = &metadata.allowed_tools {
        fields.push(format!(
            "  <allowed_tools grants_permissions=\"false\">{}</allowed_tools>",
            xml_escape(allowed_tools)
        ));
    }
    for (key, value) in &metadata.metadata {
        fields.push(format!(
            "  <entry key=\"{}\">{}</entry>",
            xml_escape(key),
            xml_escape(value)
        ));
    }
    format!("<skill_metadata>\n{}\n</skill_metadata>", fields.join("\n"))
}

fn create_skill(args: Value, config: &AppConfig, paths: &MiyuPaths) -> Result<String> {
    let name = required_string(&args, "name")?;
    let description = required_string(&args, "description")?;
    // 创建默认落当前人格,不再默认 global(09-01)。在某人格对话里学会/创作的
    // 技能默认属于那个人格,漏给所有人格要显式选 global——尤其自动学习的技能,
    // 默认 global 会让 QQ 线学的习惯出现在别人的自定义人格里。
    let scope = match args.get("scope").and_then(Value::as_str) {
        Some(value) if !value.trim().is_empty() => SkillScope::parse(Some(value))?,
        _ => SkillScope::Persona,
    };
    let draft = skills::create_draft(config, paths, &name, &description, scope)?;
    Ok(serde_json::to_string_pretty(&json!({
        "ok": true,
        "state": "draft",
        "draft": draft,
        "next": "Change only the returned draft with the edit tool, then call manage_skill with action=publish and this draft_id."
    }))?)
}

fn update_skill(args: Value, config: &AppConfig, paths: &MiyuPaths) -> Result<String> {
    let name = required_string(&args, "name")?;
    let scope_value = required_string(&args, "scope")?;
    let scope = SkillScope::parse(Some(&scope_value))?;
    let draft = skills::update_draft(config, paths, &name, scope)?;
    Ok(serde_json::to_string_pretty(&json!({
        "ok": true,
        "state": "draft",
        "draft": draft,
        "next": "Change only the returned draft with the edit tool, then call manage_skill with action=publish and this draft_id."
    }))?)
}

fn publish_skill(args: Value, paths: &MiyuPaths) -> Result<String> {
    let draft_id = required_string(&args, "draft_id")?;
    let published = skills::publish_draft(paths, &draft_id)?;
    Ok(serde_json::to_string_pretty(&json!({
        "ok": true,
        "state": "published",
        "skill": published,
        "catalog_refresh": "next turn",
    }))?)
}

fn delete_skill(args: Value, config: &AppConfig, paths: &MiyuPaths) -> Result<String> {
    let name = required_string(&args, "name")?;
    let scope_value = required_string(&args, "scope")?;
    let scope = SkillScope::parse(Some(&scope_value))?;
    let deleted = skills::delete_skill(config, paths, &name, scope)?;
    Ok(serde_json::to_string_pretty(&json!({
        "ok": true,
        "state": "deleted",
        "skill": deleted,
        "catalog_refresh": "next turn",
    }))?)
}

fn required_string(args: &Value, key: &str) -> Result<String> {
    let value = args
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    if value.is_empty() {
        anyhow::bail!("{key} is required");
    }
    Ok(value.to_string())
}

/// 清单里每条技能摘要的长度上限（字符）。
///
/// 这份清单随技能数线性增长（09-25 前常驻 tools 数组，现在随回合尾巴发），而摘要只需要
/// 够模型判断「该不该加载这一件」——完整说明本来就在 SKILL.md 里，加载之后才该看到。
/// 上限是兜底：技能作者自己就该把 frontmatter 的 description 写短（见
/// skill-creator）。截断只在超限时发生，同一份技能目录下字节恒定。
const SKILL_SUMMARY_LIMIT: usize = 120;

fn clip_skill_summary(summary: &str) -> String {
    let mut clipped = String::new();
    for (index, ch) in summary.chars().enumerate() {
        if index >= SKILL_SUMMARY_LIMIT {
            clipped.push('…');
            return clipped;
        }
        clipped.push(ch);
    }
    clipped
}

/// 回合尾巴里的技能目录块；没有技能是 `None`。同一份目录两次生成逐字节相等——
/// 「变了才发」靠逐字节比对上一份。
fn catalog_block(entries: &[SkillEntry]) -> Option<String> {
    if entries.is_empty() {
        return None;
    }
    let items = entries
        .iter()
        .map(|entry| {
            // 08-21 文风批:条目单行化——五行 XML 壳对每技能是纯结构开销。
            format!(
                "  <skill name=\"{}\" source=\"{}\">{}</skill>",
                xml_escape(&entry.metadata.name),
                entry.source.as_str(),
                xml_escape(&clip_skill_summary(&entry.metadata.description)),
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    Some(format!(
        "{AVAILABLE_SKILLS_TAG}>\n{items}\n</available-skills>"
    ))
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_paths(root: &std::path::Path) -> MiyuPaths {
        MiyuPaths {
            root_dir: root.to_path_buf(),
            config_dir: root.join("config"),
            config_file: root.join("config/config.jsonc"),
            skills_dir: root.join("data/skills"),
            data_dir: root.join("data"),
            cache_dir: root.join("cache"),
            state_dir: root.join("state"),
            pictures_dir: root.join("data/pictures"),
            fish_hook_file: root.join("fish/miyu.fish"),
            bash_hook_file: root.join("config/shell/bash-hook.sh"),
            zsh_hook_file: root.join("config/shell/zsh-hook.zsh"),
            scripts_dir: root.join("data/scripts"),
            // 内置资源根 = 父目录 = `<root>`;技能读盘后内置技能从这里进来。
            system_scripts_dir: root.join("system-scripts"),
        }
    }

    #[test]
    fn skill_catalog_includes_builtin_creator() {
        let temp = tempfile::tempdir().unwrap();
        crate::tools::tests::install_bundled_skills(temp.path());
        let paths = test_paths(temp.path());
        let config = AppConfig::default();
        let mut registry = ToolRegistry::new();
        register_skills(&mut registry, &config, &paths).unwrap();
        let catalog = registry.skill_catalog().unwrap();
        assert!(catalog.contains("name=\"skill-creator\""));
        assert!(catalog.contains("source=\"built_in\""));
    }

    #[test]
    fn an_empty_catalog_has_no_block() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let mut registry = ToolRegistry::new();
        register_skills(&mut registry, &AppConfig::default(), &paths).unwrap();
        assert!(registry.contains("load_skill"));
        assert_eq!(registry.skill_catalog(), None);
    }

    #[test]
    fn loaded_skill_exposes_standard_metadata_without_granting_permissions() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let directory = paths.skills_dir.join("sample-skill");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join("SKILL.md"),
            "---\nname: sample-skill\ndescription: Sample workflow\nlicense: MIT\ncompatibility: Miyu\nallowed-tools: run_command\nmetadata:\n  author: test\n---\n\nBody.",
        )
        .unwrap();

        let loaded = load_skill(
            json!({"name": "sample-skill"}),
            &AppConfig::default(),
            &paths,
        )
        .unwrap();
        assert!(loaded.contains("<license>MIT</license>"));
        assert!(loaded.contains("<compatibility>Miyu</compatibility>"));
        assert!(loaded
            .contains("<allowed_tools grants_permissions=\"false\">run_command</allowed_tools>"));
        assert!(loaded.contains("<entry key=\"author\">test</entry>"));
    }

    #[test]
    fn authoring_tools_have_write_permissions() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let mut registry = ToolRegistry::new();
        register_authoring(&mut registry, AppConfig::default(), paths);
        assert_eq!(
            registry.permission("manage_skill").unwrap(),
            super::super::ToolPermission::Writes
        );
    }

    /// 模型得看得到技能名。清单原来只在 `load_skill` 的描述里，这个工具一懒加载，
    /// stub 模式只留描述第一行，清单被整段砍掉，模型只能瞎猜：实测连试 gaming /
    /// linux-game-compat / linux-gaming 三个名字，真名是 linux-game-compatibility，
    /// 一轮烧掉 208k token 去找一个一直都在的内置技能。09-25 起清单在回合尾巴里
    /// （`agent::instruction_source::SkillsSource`，读的就是注册表里这一份），描述是常量；
    /// 工具照旧常驻，stub 模式下不必先绕一轮 load_tools 取契约。
    #[test]
    fn load_skill_is_always_loaded_and_the_catalog_names_every_skill() {
        let temp = tempfile::tempdir().unwrap();
        // 技能 09-23 起读盘:内置技能得先摆进隔离资源树,清单里才有 skill-creator。
        crate::tools::tests::install_bundled_skills(temp.path());
        let paths = test_paths(temp.path());
        let config = AppConfig::default();
        let mut registry = ToolRegistry::new();
        register_skills(&mut registry, &config, &paths).unwrap();
        let load_skill = registry.get("load_skill").unwrap();
        assert!(load_skill.always_loaded);
        let catalog = registry.skill_catalog().expect("有技能就有清单");
        assert!(catalog.starts_with(AVAILABLE_SKILLS_TAG), "{catalog}");
        assert!(catalog.contains("name=\"skill-creator\""), "{catalog}");
        // 描述在 full 与 stub 两档都是同一份常量,不带清单。
        for stub in [false, true] {
            let definition = registry
                .request_definitions(stub)
                .into_iter()
                .find(|definition| definition.function.name == "load_skill")
                .expect("load_skill 两档都在");
            assert_eq!(definition.function.description, load_skill.description);
            assert!(!definition.function.description.contains("skill-creator"));
        }
        assert_eq!(
            load_skill.load_policy,
            super::super::tool_descriptions::LoadPolicy::Summary
        );
        assert_eq!(load_skill.groups, vec!["skills"]);
    }

    #[test]
    fn update_skill_requires_an_explicit_scope_at_runtime() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let error = update_skill(
            json!({"name": "sample-skill"}),
            &AppConfig::default(),
            &paths,
        )
        .unwrap_err();
        assert!(error.to_string().contains("scope is required"));
    }

    #[test]
    fn delete_skill_requires_an_explicit_scope_at_runtime() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let error = delete_skill(
            json!({"name": "sample-skill"}),
            &AppConfig::default(),
            &paths,
        )
        .unwrap_err();
        assert!(error.to_string().contains("scope is required"));
    }
}
