//! 脚本目录的变更检测:指纹没变就不重扫。
//!
//! 此前每个工具回合都在持 registry 锁的状态下同步全量重扫四个目录——每个
//! 脚本 `read_to_string` 读全文(内置脚本合计 500KB)再 canonicalize。这里
//! 照 skills 的做法:目录清单 + 每个文件的元数据(长度/mtime/inode)做 blake3
//! 指纹,变了才扫;扫描本身放 spawn_blocking,读完再短暂持锁替换。三次稳定读
//! 防扫描途中目录还在变。

use crate::tools::scripts::*;

pub struct ScriptRefreshSnapshot {
    pub(crate) scan: ScriptScanResult,
    pub(crate) fingerprint: [u8; 32],
}

pub(crate) fn catalog_fingerprint(roots: &[PathBuf]) -> Result<[u8; 32]> {
    let mut hasher = blake3::Hasher::new();
    for root in roots {
        hasher.update(root.as_os_str().as_encoded_bytes());
        let Ok(read_dir) = std::fs::read_dir(root) else {
            hasher.update(&[0]);
            continue;
        };
        let mut paths: Vec<PathBuf> = read_dir
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .collect();
        paths.sort();
        for path in paths {
            hasher.update(path.as_os_str().as_encoded_bytes());
            miyu_core::skills::hash_metadata(&mut hasher, &path)?;
        }
    }
    Ok(*hasher.finalize().as_bytes())
}

/// 指纹与 `current` 一致 → None(什么都不用做);否则返回稳定态的扫描结果。
pub fn prepare_script_refresh(
    current: Option<[u8; 32]>,
    config: &miyu_base::config::AppConfig,
    paths: &MiyuPaths,
) -> Result<Option<ScriptRefreshSnapshot>> {
    let root_layers = script_scan_root_layers(config, paths);
    let roots: Vec<PathBuf> = root_layers.iter().map(|root| root.path.clone()).collect();
    for _ in 0..3 {
        let before = catalog_fingerprint(&roots)?;
        if Some(before) == current {
            return Ok(None);
        }
        let mut scan = scan_scripts_at(&root_layers)?;
        retain_persona_visible(config, paths, &mut scan.entries);
        let after = catalog_fingerprint(&roots)?;
        if before == after {
            return Ok(Some(ScriptRefreshSnapshot {
                scan,
                fingerprint: after,
            }));
        }
    }
    bail!("script directories kept changing while they were being scanned")
}

/// 本人格看得见的脚本:自定义人格下内置脚本只有清单里点了名的才算(没写清单
/// = 一件不挂,纯净状态);写了白名单就只留名单上的。注册与 `manage_script list`
/// 共用这一道门——模型看到的目录和真挂上的工具面必须是同一份。
pub(crate) fn retain_persona_visible(
    config: &miyu_base::config::AppConfig,
    paths: &MiyuPaths,
    entries: &mut Vec<super::ScriptEntry>,
) {
    let default_persona = miyu_core::skills::is_default_persona(config);
    let full_manifest =
        miyu_base::config::PersonaManifest::load(config, paths, &config.active_persona_scope());
    // 技能整个关掉时(插件位或机器位),技能一件不挂,带路脚本也跟着不可用。
    let skills_on = full_manifest.skills_enabled(config);
    let manifest = full_manifest.plugins;
    let allow = &manifest.scripts;
    // 人格自己那一层(`<scripts>/personas/<人格>/`)永远算数:那是它自己写的、
    // 或专门给它放的,白名单是给内置层和全局层用的——不然它刚 register 完的
    // 脚本自己都调不到(用户实测)。
    let own_layer = config.active_persona_scripts_dir(paths);
    entries.retain(|entry| {
        if Path::new(&entry.path).starts_with(&own_layer) {
            return true;
        }
        let listed = allow
            .as_ref()
            .is_some_and(|list| list.iter().any(|id| id == &entry.id));
        // 技能带路的脚本:开关跟着那份技能走——技能关掉,它带的脚本也一并不可用
        // (09-23)。功能表把技能开关连动写进了脚本白名单,这里再兜一道,手写的
        // persona.toml 也不会出现「技能开着、脚本没开」或反过来的断裂。
        if let Some(skill) = entry.origin.skill.as_deref() {
            return skills_on
                && miyu_core::skills::builtin_skill_enabled(
                    default_persona,
                    &manifest.skills,
                    skill,
                );
        }
        if super::is_builtin_script(paths, entry) && !default_persona {
            return listed;
        }
        allow.is_none() || listed
    });
}

#[cfg(test)]
mod persona_gate_tests {
    use super::*;

    fn entry(id: &str, path: &std::path::Path) -> super::super::ScriptEntry {
        // 走 serde 和 index.json 同一条路,省得给整个结构体凑 Default。
        serde_json::from_value(serde_json::json!({
            "id": id,
            "path": path.join(id).to_string_lossy(),
        }))
        .unwrap()
    }

    fn setup(
        persona: &str,
        allow: Option<&[&str]>,
    ) -> (miyu_base::config::AppConfig, MiyuPaths, tempfile::TempDir) {
        let temp = tempfile::tempdir().unwrap();
        let mut paths = MiyuPaths::new().unwrap();
        // 根目录也指到临时目录:人格清单按 root 下的布局标记找位置,不能碰真家。
        paths.root_dir = temp.path().to_path_buf();
        paths.system_scripts_dir = temp.path().join("system");
        paths.scripts_dir = temp.path().join("scripts");
        paths.data_dir = temp.path().join("data");
        paths.state_dir = temp.path().join("state");
        let mut config = miyu_base::config::AppConfig::default();
        config.prompt.active_persona = persona.to_string();
        if let Some(allow) = allow {
            let scope = config.active_persona_scope();
            let dir = miyu_base::config::PersonaManifest::manifest_path(&config, &paths, &scope);
            std::fs::create_dir_all(dir.parent().unwrap()).unwrap();
            let mut manifest = miyu_base::config::PersonaManifest::all();
            manifest.plugins.scripts = Some(allow.iter().map(|id| id.to_string()).collect());
            std::fs::write(&dir, manifest.to_toml()).unwrap();
        }
        (config, paths, temp)
    }

    /// 同上,但写的是**技能**白名单(`plugins.skills`)——技能带路的脚本那道门
    /// 看的是它,不是脚本白名单。
    fn setup_skills(
        persona: &str,
        allow: Option<&[&str]>,
    ) -> (miyu_base::config::AppConfig, MiyuPaths, tempfile::TempDir) {
        let temp = tempfile::tempdir().unwrap();
        let mut paths = MiyuPaths::new().unwrap();
        paths.root_dir = temp.path().to_path_buf();
        paths.system_scripts_dir = temp.path().join("system");
        paths.scripts_dir = temp.path().join("scripts");
        paths.data_dir = temp.path().join("data");
        paths.state_dir = temp.path().join("state");
        let mut config = miyu_base::config::AppConfig::default();
        config.prompt.active_persona = persona.to_string();
        if let Some(allow) = allow {
            let scope = config.active_persona_scope();
            let dir = miyu_base::config::PersonaManifest::manifest_path(&config, &paths, &scope);
            std::fs::create_dir_all(dir.parent().unwrap()).unwrap();
            let mut manifest = miyu_base::config::PersonaManifest::all();
            manifest.plugins.skills = Some(allow.iter().map(|name| name.to_string()).collect());
            std::fs::write(&dir, manifest.to_toml()).unwrap();
        }
        (config, paths, temp)
    }

    /// 技能插件整个关掉(启用名单里没有 `skills`)时,技能一件都不挂,
    /// 它们带路的脚本也不能再经工具桥调到——哪怕脚本白名单是「全开」。
    #[test]
    fn skill_carried_scripts_go_with_the_skills_plugin() {
        let (config, paths, _temp) = setup("", None);
        let scope = config.active_persona_scope();
        let manifest_path =
            miyu_base::config::PersonaManifest::manifest_path(&config, &paths, &scope);
        std::fs::create_dir_all(manifest_path.parent().unwrap()).unwrap();
        let mut manifest = miyu_base::config::PersonaManifest::all();
        manifest.plugins.enabled = Some(
            miyu_base::config::PLUGIN_IDS
                .iter()
                .filter(|id| **id != miyu_base::config::SKILLS_PLUGIN)
                .map(|id| id.to_string())
                .collect(),
        );
        std::fs::write(&manifest_path, manifest.to_toml()).unwrap();
        let carried = paths
            .system_personas_dir()
            .unwrap()
            .join("default/skills/travel/scripts");
        let mut entries = vec![
            entry("flight", &carried),
            entry("global_tool", &paths.scripts_dir),
        ];
        entries[0].origin = super::super::ScriptOrigin {
            layer: super::super::ScriptLayerKind::BuiltinSkill,
            skill: Some("travel".to_string()),
        };
        retain_persona_visible(&config, &paths, &mut entries);
        let ids: Vec<&str> = entries.iter().map(|entry| entry.id.as_str()).collect();
        assert_eq!(ids, vec!["global_tool"]);
    }

    #[test]
    fn own_layer_survives_the_allowlist_and_builtins_need_naming() {
        let (config, paths, _temp) = setup("alter.md", Some(&["bili"]));
        let builtin = super::super::builtin_scripts_dir(&paths);
        let own = config.active_persona_scripts_dir(&paths);
        let mut entries = vec![
            entry("bili", &builtin),
            entry("xhs", &builtin),
            entry("mine", &own),
            entry("global_tool", &paths.scripts_dir),
        ];
        retain_persona_visible(&config, &paths, &mut entries);
        let ids: Vec<&str> = entries.iter().map(|entry| entry.id.as_str()).collect();
        assert_eq!(ids, vec!["bili", "mine"]);
    }

    #[test]
    fn custom_persona_without_manifest_hides_builtins_but_keeps_its_own() {
        let (config, paths, _temp) = setup("alter.md", None);
        let builtin = super::super::builtin_scripts_dir(&paths);
        let own = config.active_persona_scripts_dir(&paths);
        let mut entries = vec![
            entry("xhs", &builtin),
            entry("mine", &own),
            entry("global_tool", &paths.scripts_dir),
        ];
        retain_persona_visible(&config, &paths, &mut entries);
        let ids: Vec<&str> = entries.iter().map(|entry| entry.id.as_str()).collect();
        assert_eq!(ids, vec!["mine", "global_tool"]);
    }

    /// 09-23:技能带路的脚本开关跟着那份技能走——技能关掉,它带的脚本也一并
    /// 不可用;技能打开就回来。自定义人格下不会出现「技能开着、脚本没开」。
    #[test]
    fn skill_carried_scripts_follow_the_skill_allowlist() {
        let (config, paths, _temp) = setup("alter.md", None);
        let skills_dir = paths.system_personas_dir().unwrap().join("default/skills");
        let carried = skills_dir.join("travel/scripts");
        let mut entries = vec![
            entry("flight", &carried),
            entry("hotel", &carried),
            entry("global_tool", &paths.scripts_dir),
        ];
        for entry in &mut entries {
            if Path::new(&entry.path).starts_with(&carried) {
                entry.origin = super::super::ScriptOrigin {
                    layer: super::super::ScriptLayerKind::BuiltinSkill,
                    skill: Some("travel".to_string()),
                };
            }
        }
        // 自定义人格、清单没点名:技能不挂,带路脚本也不挂;全局层照旧。
        retain_persona_visible(&config, &paths, &mut entries);
        let ids: Vec<&str> = entries.iter().map(|entry| entry.id.as_str()).collect();
        assert_eq!(ids, vec!["global_tool"]);

        // 技能点开(清单里点名 travel):带路脚本一起回来。清单里也点名脚本,
        // 免得被「内置脚本要逐个点名」那条门挡下——这里验的是技能这一道。
        let (config, paths, _temp) = setup_skills("alter.md", Some(&["travel"]));
        let skills_dir = paths.system_personas_dir().unwrap().join("default/skills");
        let carried = skills_dir.join("travel/scripts");
        let mut entries = vec![entry("flight", &carried), entry("hotel", &carried)];
        for entry in &mut entries {
            entry.origin = super::super::ScriptOrigin {
                layer: super::super::ScriptLayerKind::BuiltinSkill,
                skill: Some("travel".to_string()),
            };
        }
        // 技能开着:带路脚本放行(脚本白名单里也点了名)。
        retain_persona_visible(&config, &paths, &mut entries);
        let ids: Vec<&str> = entries.iter().map(|entry| entry.id.as_str()).collect();
        assert_eq!(ids, vec!["flight", "hotel"]);

        // 技能关掉(清单里没有 travel):脚本白名单点没点名都不算数。
        let (config, paths, _temp) = setup_skills("alter.md", None);
        let carried = paths
            .system_personas_dir()
            .unwrap()
            .join("default/skills/travel/scripts");
        let mut entries = vec![entry("flight", &carried), entry("hotel", &carried)];
        for entry in &mut entries {
            entry.origin = super::super::ScriptOrigin {
                layer: super::super::ScriptLayerKind::BuiltinSkill,
                skill: Some("travel".to_string()),
            };
        }
        retain_persona_visible(&config, &paths, &mut entries);
        assert!(
            entries.is_empty(),
            "技能关掉,它带路的脚本不该还在: {:?}",
            entries
                .iter()
                .map(|entry| entry.id.as_str())
                .collect::<Vec<_>>()
        );
    }
}

pub fn apply_script_refresh(
    registry: &mut ToolRegistry,
    paths: &MiyuPaths,
    snapshot: ScriptRefreshSnapshot,
) {
    let specs = script_specs(&snapshot.scan.entries, &paths.scripts_dir, &paths.cache_dir);
    if let Err(error) = registry.replace_script_tools(specs, snapshot.scan.unregistered) {
        tracing::warn!(error = %error, "failed to replace Miyu script tools");
    }
    // 失败也记指纹:同一份坏目录不值得每回合重扫一次,改了文件指纹自然变。
    registry.set_script_catalog_fingerprint(snapshot.fingerprint);
}
