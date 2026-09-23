mod draft;
pub mod manifest;
pub use draft::*;
pub use manifest::*;

use anyhow::{bail, Context, Result};
use miyu_base::config::{persona_scope_name, AppConfig};
use miyu_base::paths::MiyuPaths;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use yaml_rust2::scanner::{Scanner, Token, TokenType};
use yaml_rust2::{Yaml, YamlLoader};

/// 平台级内置技能:任何人格(包括从白板捏起的)都得拿到的元能力。
///
/// skill-creator 是「如何扩展自己」:自定义人格想给自己加技能就得用它,归人格
/// 等于锁死自定义角色的自我扩展入口。script-creator 是脚本接口契约(头部/
/// stdin JSON/退出码),同样是元能力,缺了自定义人格写出的脚本注册不上(09-05)。
///
/// 09-23 起技能不再 `include_str!` 编进二进制,改与脚本同一条路线:从资源树的
/// `<资源根>/personas/<人格>/skills/<技能名>/SKILL.md` 读盘(见 [`skill_roots`])。
/// 目录决定**属于谁**,这份名单决定**谁能跨人格用**——门控语义与从前一致:
/// 平台级永远放行,其余内置技能只在出厂人格下默认可见,自定义人格按人格清单的
/// `plugins.skills` 白名单逐个勾回来(09-01 / 09-13)。
const PLATFORM_WIDE_SKILLS: &[&str] = &["skill-creator", "script-creator"];

/// 内置资源(技能/脚本)默认只属于 Miyu 出厂人格。判据:`active_persona` 去空
/// 白后为空 = scope "default" = Miyu 本人。非默认人格下,非平台级的内置资源
/// 一律隐藏,换上自定义人格即得纯净状态。
pub fn is_default_persona(config: &AppConfig) -> bool {
    config.prompt.active_persona.trim().is_empty()
}
const MAX_SKILL_CATALOG_ENTRIES: usize = 256;
const MAX_SKILL_ROOT_DIRECTORIES: usize = 1_024;
const MAX_SKILL_RESOURCE_ENTRIES: usize = 256;

/// 人格清单里的技能白名单(`plugins.skills`);None = 全部。
fn skill_allowlist(config: &AppConfig, paths: &MiyuPaths) -> Option<Vec<String>> {
    miyu_base::config::PersonaManifest::load(config, paths, &config.active_persona_scope())
        .plugins
        .skills
}

/// 平台级内置技能(skill-creator / script-creator):任何人格、任何白名单都放行。
pub(crate) fn is_platform_wide_builtin(name: &str) -> bool {
    PLATFORM_WIDE_SKILLS.contains(&name)
}

/// 这个技能在本人格下能不能用。
///
/// - 平台级内置技能永远能用。
/// - 非平台级内置技能:默认人格全开(再看白名单);自定义人格只有清单里
///   点了名的才开——没写清单 = 一件不挂,换上自定义人格还是纯净状态(09-01),
///   但引导里能逐个勾回来(09-13)。
/// - 其余(目录里的)技能:看白名单,None = 全部。
fn allowed_by(
    default_persona: bool,
    allowlist: &Option<Vec<String>>,
    name: &str,
    source: SkillSource,
) -> bool {
    // 人格自己那一层永远算数:那是它自己写的、或专门给它放的,白名单是给
    // 全局层与内置层用的(与脚本同一规则)。
    if source == SkillSource::Persona || is_platform_wide_builtin(name) {
        return true;
    }
    let listed = allowlist
        .as_ref()
        .is_some_and(|list| list.iter().any(|item| item == name));
    // 非平台级的内置技能(travel-planner、bilibili-live 这类 Miyu 配件)。
    if source == SkillSource::BuiltIn && !default_persona {
        return listed;
    }
    allowlist.is_none() || listed
}

/// 一个内置技能在本人格下开没开。技能带路的脚本用它当门:技能关掉,它
/// `skills/<技能名>/scripts/` 下的脚本也一并不可用(09-23)。判据与目录技能的
/// [`allowed_by`] 一致——非平台级内置技能在自定义人格下只有清单点了名才开。
pub fn builtin_skill_enabled(
    default_persona: bool,
    allowlist: &Option<Vec<String>>,
    name: &str,
) -> bool {
    if is_platform_wide_builtin(name) {
        return true;
    }
    let listed = allowlist
        .as_ref()
        .is_some_and(|list| list.iter().any(|item| item == name));
    if default_persona {
        allowlist.is_none() || listed
    } else {
        listed
    }
}

/// 引导里可以逐个勾的技能:目录里的 + 非平台级内置的,(id, 界面名, 界面说明, 是否内置)。
/// **不看白名单**——表要摆全,勾选状态由调用方按清单填。
///
/// 界面名与界面说明走人槽(`display_name` / `summary`),没写才回退到模型槽
/// (`name` / `description`)。模型槽是英文触发词,直接摆进设置页会中英混杂
/// (AGENTS §1.5.1)。id 仍是 `name`——清单白名单按它存。
pub fn persona_skill_options(
    config: &AppConfig,
    paths: &MiyuPaths,
) -> Vec<(String, String, String, bool)> {
    discover_visible(config, paths)
        .unwrap_or_default()
        .into_iter()
        .filter(|entry| !is_platform_wide_builtin(&entry.metadata.name))
        .map(|entry| {
            (
                entry.metadata.name.clone(),
                entry.metadata.ui_name().to_string(),
                entry.metadata.ui_summary().to_string(),
                entry.source == SkillSource::BuiltIn,
            )
        })
        .collect()
}

/// 本人格看得见的技能,再过一道清单白名单——这才是模型面上的目录。
pub fn discover(config: &AppConfig, paths: &MiyuPaths) -> Result<Vec<SkillEntry>> {
    let allowlist = skill_allowlist(config, paths);
    let default_persona = is_default_persona(config);
    Ok(discover_visible(config, paths)?
        .into_iter()
        .filter(|entry| {
            allowed_by(
                default_persona,
                &allowlist,
                &entry.metadata.name,
                entry.source,
            )
        })
        .collect())
}

/// 目录扫描 + 全部内置技能(含非平台级的),不含人格门与白名单——门在 `allowed_by`。
fn discover_visible(config: &AppConfig, paths: &MiyuPaths) -> Result<Vec<SkillEntry>> {
    let mut entries = Vec::new();
    let mut seen = BTreeSet::new();
    for (root, source) in skill_roots(config, paths) {
        for directory in sorted_skill_directories(&root)? {
            if directory.join(".disabled").exists() {
                continue;
            }
            let skill_file = directory.join("SKILL.md");
            if !skill_file.is_file() {
                continue;
            }
            let raw = match read_skill_file(&skill_file) {
                Ok(raw) => raw,
                Err(error) => {
                    tracing::warn!(path = %skill_file.display(), error = %error, "skipping unreadable skill");
                    continue;
                }
            };
            let directory_name = directory
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            let metadata = match parse_skill_metadata(&raw, Some(directory_name)) {
                Ok(metadata) => metadata,
                Err(error) => {
                    tracing::warn!(path = %skill_file.display(), error = %error, "skipping invalid skill");
                    continue;
                }
            };
            if seen.insert(metadata.name.clone()) {
                // 内置技能 09-23 起也走这条扫盘路,不再在循环外补一批,
                // 所以这里不需要给它留一个空位。
                if entries.len() >= MAX_SKILL_CATALOG_ENTRIES {
                    bail!("skill catalog exceeds the {MAX_SKILL_CATALOG_ENTRIES} entry limit");
                }
                entries.push(SkillEntry {
                    metadata,
                    source,
                    directory: Some(directory),
                });
            }
        }
    }
    Ok(entries)
}

pub fn catalog_fingerprint(config: &AppConfig, paths: &MiyuPaths) -> Result<[u8; 32]> {
    let mut hasher = blake3::Hasher::new();
    for (root, source) in skill_roots(config, paths) {
        hasher.update(source.as_str().as_bytes());
        hasher.update(root.as_os_str().as_encoded_bytes());
        for directory in sorted_skill_directories(&root)? {
            hasher.update(directory.as_os_str().as_encoded_bytes());
            hash_metadata(&mut hasher, &directory.join(".disabled"))?;
            hash_metadata(&mut hasher, &directory.join("SKILL.md"))?;
        }
    }
    // 指纹要把「本人格能看见哪些内置技能」也算进去:否则切换人格后目录没变、
    // 指纹不变,而可见的内置集合已经变了,催化缓存会拿旧目录充数。内置技能的
    // 内容现在也在根目录里(上面哈希过了),这里补上「出厂人格与否」这一个
    // 判据——非默认人格会隐掉非平台级内置件。
    hasher.update(&[is_default_persona(config) as u8]);
    // 白名单同理:清单一改,可见集合就变了(自定义人格靠它把内置技能勾回来)。
    if let Some(allowlist) = skill_allowlist(config, paths) {
        hasher.update(b"allowlist");
        for name in allowlist {
            hasher.update(name.as_bytes());
            hasher.update(b"\0");
        }
    }
    Ok(*hasher.finalize().as_bytes())
}

pub fn load(name: &str, config: &AppConfig, paths: &MiyuPaths) -> Result<LoadedSkill> {
    let name = name.trim();
    // 白名单外的技能加载不了:下面按 `discover` 找,它已经把可见性裁过了——
    // 模型照着历史 load 也捞不回关掉的技能。
    if name.is_empty() {
        bail!("skill name is required");
    }
    let entry = discover(config, paths)?
        .into_iter()
        .find(|entry| entry.metadata.name == name)
        .ok_or_else(|| anyhow::anyhow!("skill not found: {name}"))?;
    // 09-23 起内置技能也住磁盘,`discover` 找得到的技能一定有目录;缺失/坏掉的
    // SKILL.md 在扫描时就被跳过,于是这里只会落到「找不到」——与隐藏技能的
    // 报错同一条路,模型照着历史 load 也捞不回来。
    let directory = entry
        .directory
        .with_context(|| format!("skill not found: {name}"))?;
    let raw = read_skill_file(&directory.join("SKILL.md"))?;
    let (metadata, body) = parse_skill_document(&raw, Some(name))?;
    let mut files = Vec::new();
    for entry in fs::read_dir(&directory)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == "SKILL.md" || name.to_string_lossy().starts_with('.') {
            continue;
        }
        if files.len() >= MAX_SKILL_RESOURCE_ENTRIES {
            bail!("skill resource manifest exceeds the {MAX_SKILL_RESOURCE_ENTRIES} entry limit");
        }
        files.push(entry.path());
    }
    files.sort();
    Ok(LoadedSkill {
        metadata,
        body,
        source: entry.source,
        base_dir: Some(directory),
        files,
    })
}

pub fn is_generated_skill(raw: &str) -> bool {
    parse_skill_metadata(raw, None)
        .ok()
        .and_then(|metadata| metadata.metadata.get("miyu.generated").cloned())
        .is_some_and(|value| value.eq_ignore_ascii_case("true"))
        || raw.contains("generated_by: miyu")
        || raw.contains("Auto-learned method from assistant conversation")
        || raw.contains("Auto-learned method from Miyu conversation")
}

fn skill_roots(config: &AppConfig, paths: &MiyuPaths) -> Vec<(PathBuf, SkillSource)> {
    // 优先级从高到低:人格自己的(data 层)> 全局(data 层)> 资源树里的内置。
    let mut roots = vec![
        (
            config.active_persona_skills_dir(paths),
            SkillSource::Persona,
        ),
        (paths.skills_dir.clone(), SkillSource::Global),
    ];
    // 内置技能(09-23):`<资源根>/personas/<人格>/skills/<技能名>/SKILL.md`。
    // 候选链按优先级排;人格内部先当前人格再出厂人格——资源树里真给了这个人格
    // 一份技能,它就压过出厂那份。出厂人格那层对谁都扫:平台级技能任何人格都要
    // 拿得到,非平台级内置件由白名单决定勾不勾(09-01 / 09-13)。
    let factory = miyu_base::config::persona_scope_name("");
    let active = config.active_persona_scope();
    for root in paths.system_personas_dirs() {
        let mut scopes = vec![active.clone()];
        if !scopes.contains(&factory) {
            scopes.push(factory.clone());
        }
        for scope in scopes {
            roots.push((root.join(scope).join("skills"), SkillSource::BuiltIn));
        }
    }
    roots
}

fn sorted_skill_directories(root: &Path) -> Result<Vec<PathBuf>> {
    let metadata = match fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_symlink() {
        bail!("skill root must not be a symbolic link: {}", root.display());
    }
    if !metadata.is_dir() {
        bail!("skill root is not a directory: {}", root.display());
    }
    let mut directories = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() && !entry.file_name().to_string_lossy().starts_with('.') {
            if directories.len() >= MAX_SKILL_ROOT_DIRECTORIES {
                bail!("skill root exceeds the {MAX_SKILL_ROOT_DIRECTORIES} directory-entry limit");
            }
            directories.push(entry.path());
        }
    }
    directories.sort();
    Ok(directories)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 仓库里的内置技能必须按新布局摆在资源树里,而且 `SKILL.md` 的 `name`
    /// 与目录名一致——09-23 起技能是**读盘**的,目录形态就是契约,摆错位置或
    /// 名字对不上等于这份技能根本加载不到。09-20 加的 `travel-planner` 曾因
    /// 忘了登记静默躺了一天(09-21 实测发现),与 AGENTS §2.1 说 descriptions
    /// 宏行的是同一个坑。
    #[test]
    fn bundled_skills_follow_the_persona_resource_layout() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../src/personas");
        let mut personas: Vec<_> = std::fs::read_dir(&root)
            .unwrap()
            .collect::<std::io::Result<Vec<_>>>()
            .unwrap();
        personas.sort_by_key(|entry| entry.path());
        let mut found = BTreeSet::new();
        for persona in personas {
            let skills = persona.path().join("skills");
            if !skills.is_dir() {
                continue;
            }
            for skill in std::fs::read_dir(&skills).unwrap() {
                let directory = skill.unwrap().path();
                if !directory.is_dir() {
                    continue;
                }
                let name = directory.file_name().unwrap().to_string_lossy().to_string();
                let raw = read_skill_file(&directory.join("SKILL.md")).unwrap_or_else(|error| {
                    panic!("skill {name} has no readable SKILL.md: {error}")
                });
                parse_skill_metadata(&raw, Some(&name)).unwrap_or_else(|error| {
                    panic!("skill {name} has an invalid SKILL.md: {error}")
                });
                found.insert(name);
            }
        }
        // 出厂人格必须带齐平台级技能(任何人格都要拿得到的两件元能力)。
        for name in PLATFORM_WIDE_SKILLS {
            assert!(found.contains(*name), "factory persona is missing {name}");
        }
    }

    fn test_paths(root: &Path) -> MiyuPaths {
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
            // 内置资源根 = 它的父目录 = `<root>/system`,新布局的 personas/ 就在
            // 那下面(与生产里 `system_scripts_dir=<前缀>/scripts` 同构)。
            system_scripts_dir: root.join("system/scripts"),
        }
    }

    /// 按资源树布局写一份技能:`<root>/system/personas/<人格>/skills/<名>/SKILL.md`。
    fn write_persona_skill(root: &Path, persona: &str, name: &str, description: &str) -> PathBuf {
        let directory = root
            .join("system/personas")
            .join(persona)
            .join("skills")
            .join(name);
        fs::create_dir_all(&directory).unwrap();
        fs::write(
            directory.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {description}\n---\n\nBody of {name}."),
        )
        .unwrap();
        directory
    }

    #[test]
    fn parses_standard_frontmatter_fields() {
        let raw = "---\nname: sample-skill\ndescription: Sample workflow\nlicense: MIT\ncompatibility: Miyu\nallowed-tools: read_file\nmetadata:\n  author: test\n---\n\nBody.";
        let metadata = parse_skill_metadata(raw, Some("sample-skill")).unwrap();
        assert_eq!(metadata.license.as_deref(), Some("MIT"));
        assert_eq!(metadata.compatibility.as_deref(), Some("Miyu"));
        assert_eq!(metadata.allowed_tools.as_deref(), Some("read_file"));
        assert_eq!(
            metadata.metadata.get("author").map(String::as_str),
            Some("test")
        );
    }

    #[test]
    fn rejects_yaml_anchors_before_loading_frontmatter() {
        let raw = "---\nname: sample-skill\ndescription: &description Sample workflow\nmetadata:\n  copied: *description\n---\n";
        let error = parse_skill_metadata(raw, Some("sample-skill")).unwrap_err();
        assert!(error.to_string().contains("anchors or aliases"));
    }

    #[test]
    fn persona_skill_overrides_global_and_builtin() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let config = AppConfig::default();
        // 三层同名:资源树里的出厂件 < 全局层 < 人格自己。
        write_persona_skill(temp.path(), "default", "sample-skill", "builtin");
        let global = paths.skills_dir.join("sample-skill");
        let persona = config
            .active_persona_skills_dir(&paths)
            .join("sample-skill");
        for (directory, description) in [(&global, "global"), (&persona, "persona")] {
            fs::create_dir_all(directory).unwrap();
            fs::write(
                directory.join("SKILL.md"),
                format!("---\nname: sample-skill\ndescription: {description}\n---\n"),
            )
            .unwrap();
        }
        let source_of = |config: &AppConfig| {
            discover(config, &paths)
                .unwrap()
                .into_iter()
                .find(|entry| entry.metadata.name == "sample-skill")
                .map(|entry| (entry.source, entry.metadata.description))
                .unwrap()
        };
        assert_eq!(source_of(&config), (SkillSource::Persona, "persona".into()));
        fs::remove_dir_all(&persona).unwrap();
        assert_eq!(source_of(&config), (SkillSource::Global, "global".into()));
        fs::remove_dir_all(&global).unwrap();
        assert_eq!(source_of(&config), (SkillSource::BuiltIn, "builtin".into()));
    }

    /// 内置技能默认属于 Miyu 出厂人格:默认人格看得见非平台级内置技能,
    /// 自定义人格只剩平台级(skill-creator、script-creator)。
    #[test]
    fn builtin_skills_are_persona_gated_except_platform_wide() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        // 出厂人格的技能现在住磁盘:`<root>/system/personas/default/skills/<名>/SKILL.md`。
        for (name, description) in [
            ("skill-creator", "Author skills"),
            ("script-creator", "Author scripts"),
            ("travel-planner", "Plan travel"),
            ("bilibili-live", "Control a live room"),
        ] {
            write_persona_skill(temp.path(), "default", name, description);
        }

        let default_config = AppConfig::default();
        let names: BTreeSet<String> = discover(&default_config, &paths)
            .unwrap()
            .into_iter()
            .filter(|entry| entry.source == SkillSource::BuiltIn)
            .map(|entry| entry.metadata.name)
            .collect();
        assert!(names.contains("skill-creator"));
        assert!(names.contains("travel-planner"));

        let mut custom = AppConfig::default();
        custom.prompt.active_persona = "alter".to_string();
        let custom_names: BTreeSet<String> = discover(&custom, &paths)
            .unwrap()
            .into_iter()
            .filter(|entry| entry.source == SkillSource::BuiltIn)
            .map(|entry| entry.metadata.name)
            .collect();
        assert_eq!(
            custom_names,
            BTreeSet::from(["skill-creator".to_string(), "script-creator".to_string()]),
            "自定义人格只应看到平台级内置技能"
        );

        // 隐藏的内置技能连 load 都拒绝,不让模型照历史捞回。
        assert!(load("travel-planner", &custom, &paths).is_err());
        assert!(load("skill-creator", &custom, &paths).is_ok());

        // 指纹随可见集合变化:换人格后目录没动,指纹也必须不同。
        assert_ne!(
            catalog_fingerprint(&default_config, &paths).unwrap(),
            catalog_fingerprint(&custom, &paths).unwrap(),
        );
    }

    /// 新布局的技能读盘:成功、`SKILL.md` 缺失、`SKILL.md` 坏掉三条路。
    /// 坏掉的那份不该把好技能一起带走(扫描逐目录跳过,不是整层 bail)。
    #[test]
    fn skills_load_from_the_persona_disk_layout() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let config = AppConfig::default();

        let directory =
            write_persona_skill(temp.path(), "default", "sample-skill", "Use for samples");
        // 技能带路的资源(脚本)也要被 load 收进 files,第二批据此做开关连动。
        fs::create_dir_all(directory.join("scripts")).unwrap();
        fs::write(directory.join("scripts/tool.sh"), "#!/bin/sh\necho hi\n").unwrap();

        let entry = discover(&config, &paths)
            .unwrap()
            .into_iter()
            .find(|entry| entry.metadata.name == "sample-skill")
            .unwrap();
        assert_eq!(entry.source, SkillSource::BuiltIn);
        assert_eq!(entry.directory.as_deref(), Some(directory.as_path()));

        let loaded = load("sample-skill", &config, &paths).unwrap();
        assert!(loaded.body.contains("Body of sample-skill."));
        assert_eq!(loaded.base_dir.as_deref(), Some(directory.as_path()));
        // files 是技能根下的直接子项(第二批据此做技能带路脚本的开关连动)。
        assert!(loaded.files.iter().any(|file| file.ends_with("scripts")));

        // SKILL.md 缺失:扫描跳过,load 报「找不到」而不是崩。
        let missing = temp
            .path()
            .join("system/personas/default/skills/no-skill-md");
        fs::create_dir_all(&missing).unwrap();
        assert!(!discover(&config, &paths)
            .unwrap()
            .iter()
            .any(|entry| entry.metadata.name == "no-skill-md"));
        let error = load("no-skill-md", &config, &paths)
            .unwrap_err()
            .to_string();
        assert!(error.contains("skill not found"), "{error}");

        // SKILL.md 坏掉(frontmatter 的 name 与目录对不上):同样跳过并报错。
        let broken = write_persona_skill(temp.path(), "default", "broken-skill", "Broken");
        fs::write(
            broken.join("SKILL.md"),
            "---\nname: other-name\ndescription: nope\n---\n",
        )
        .unwrap();
        assert!(!discover(&config, &paths)
            .unwrap()
            .iter()
            .any(|entry| entry.metadata.name == "broken-skill"));
        let error = load("broken-skill", &config, &paths)
            .unwrap_err()
            .to_string();
        assert!(error.contains("skill not found"), "{error}");

        // 坏掉的那份不该把好技能一起带走。
        assert!(load("sample-skill", &config, &paths).is_ok());
    }

    /// 开发态(debug)的资源解析必须把仓库源码树算进来:内置技能 09-23 起读盘,
    /// `<资源根>` 若只认安装目录,从源码树跑起来的 Miyu 会一件内置技能都没有。
    /// 走真实 [`MiyuPaths::new`],按 `discover_visible` 判(绕开白名单/人格门)。
    #[test]
    #[cfg(debug_assertions)]
    fn source_tree_skills_are_reachable_through_the_real_resource_paths() {
        let paths = MiyuPaths::new().unwrap();
        let config = AppConfig::default();
        let names: BTreeSet<String> = discover_visible(&config, &paths)
            .unwrap()
            .into_iter()
            .filter(|entry| entry.source == SkillSource::BuiltIn)
            .map(|entry| entry.metadata.name)
            .collect();
        for name in PLATFORM_WIDE_SKILLS {
            assert!(
                names.contains(*name),
                "内置技能 {name} 没从资源树里扫到,资源根候选: {:?};扫到:{names:?}",
                paths.system_personas_dirs()
            );
        }
    }

    #[test]
    fn create_and_publish_draft_never_overwrites() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let config = AppConfig::default();
        let draft = create_draft(
            &config,
            &paths,
            "sample-skill",
            "Use for sample tasks",
            SkillScope::Global,
        )
        .unwrap();
        let published = publish_draft(&paths, &draft.id).unwrap();
        assert!(Path::new(&published.path).join("SKILL.md").is_file());
        assert!(create_draft(
            &config,
            &paths,
            "sample-skill",
            "Duplicate",
            SkillScope::Global,
        )
        .is_err());
    }

    #[test]
    fn deletes_global_and_current_persona_skills() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let config = AppConfig::default();
        for scope in [SkillScope::Global, SkillScope::Persona] {
            let draft = create_draft(
                &config,
                &paths,
                "sample-skill",
                "Use for sample tasks",
                scope,
            )
            .unwrap();
            publish_draft(&paths, &draft.id).unwrap();
        }

        let global = delete_skill(&config, &paths, "sample-skill", SkillScope::Global).unwrap();
        assert_eq!(global.scope, "global");
        assert!(!paths.skills_dir.join("sample-skill").exists());
        assert!(config
            .active_persona_skills_dir(&paths)
            .join("sample-skill")
            .is_dir());

        let persona = delete_skill(&config, &paths, "sample-skill", SkillScope::Persona).unwrap();
        assert_eq!(persona.scope, "persona");
        assert!(!config
            .active_persona_skills_dir(&paths)
            .join("sample-skill")
            .exists());
        assert!(delete_skill(&config, &paths, "sample-skill", SkillScope::Global).is_err());
    }

    #[test]
    fn update_draft_detects_concurrent_edits() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let config = AppConfig::default();
        let created = create_draft(
            &config,
            &paths,
            "sample-skill",
            "Use for sample tasks",
            SkillScope::Global,
        )
        .unwrap();
        publish_draft(&paths, &created.id).unwrap();
        let update = update_draft(&config, &paths, "sample-skill", SkillScope::Global).unwrap();
        fs::write(
            paths.skills_dir.join("sample-skill/SKILL.md"),
            "---\nname: sample-skill\ndescription: Changed elsewhere\n---\n",
        )
        .unwrap();
        assert!(publish_draft(&paths, &update.id).is_err());
    }

    #[test]
    fn two_update_drafts_from_the_same_revision_cannot_both_publish() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let config = AppConfig::default();
        let created = create_draft(
            &config,
            &paths,
            "sample-skill",
            "Use for sample tasks",
            SkillScope::Global,
        )
        .unwrap();
        publish_draft(&paths, &created.id).unwrap();
        let first = update_draft(&config, &paths, "sample-skill", SkillScope::Global).unwrap();
        let second = update_draft(&config, &paths, "sample-skill", SkillScope::Global).unwrap();
        fs::write(
            &first.skill_file,
            "---\nname: sample-skill\ndescription: First update\n---\n",
        )
        .unwrap();
        fs::write(
            &second.skill_file,
            "---\nname: sample-skill\ndescription: Second update\n---\n",
        )
        .unwrap();

        publish_draft(&paths, &first.id).unwrap();
        assert!(publish_draft(&paths, &second.id).is_err());
        assert!(
            fs::read_to_string(paths.skills_dir.join("sample-skill/SKILL.md"))
                .unwrap()
                .contains("First update")
        );
    }

    #[test]
    fn live_edit_detected_after_exchange_is_atomically_restored() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("sample-skill");
        let staged = temp.path().join(".stage");
        for (directory, description) in [(&target, "Original"), (&staged, "Updated")] {
            fs::create_dir(directory).unwrap();
            fs::write(
                directory.join("SKILL.md"),
                format!("---\nname: sample-skill\ndescription: {description}\n---\n"),
            )
            .unwrap();
        }
        let expected = skill_revision(&target).unwrap();
        fs::write(
            target.join("SKILL.md"),
            "---\nname: sample-skill\ndescription: Manual edit\n---\n",
        )
        .unwrap();

        let mut guard = StagedDirectory::new(staged.clone());
        assert!(install_updated_skill(&staged, &target, &expected, &mut guard).is_err());
        assert!(fs::read_to_string(target.join("SKILL.md"))
            .unwrap()
            .contains("Manual edit"));
        assert!(fs::read_to_string(staged.join("SKILL.md"))
            .unwrap()
            .contains("Updated"));
    }

    #[test]
    fn tampered_persona_scope_cannot_escape_the_skill_root() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let config = AppConfig::default();
        let draft = create_draft(
            &config,
            &paths,
            "sample-skill",
            "Use for sample tasks",
            SkillScope::Persona,
        )
        .unwrap();
        let manifest_path = paths
            .skill_drafts_dir()
            .join(&draft.id)
            .join(DRAFT_MANIFEST);
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        manifest["persona_scope"] = serde_json::Value::String("../../outside".to_string());
        fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();

        assert!(publish_draft(&paths, &draft.id).is_err());
        assert!(!paths.data_dir.join("outside/sample-skill").exists());
    }

    #[cfg(unix)]
    #[test]
    fn publish_rejects_a_symlinked_draft_package() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let config = AppConfig::default();
        let draft = create_draft(
            &config,
            &paths,
            "sample-skill",
            "Use for sample tasks",
            SkillScope::Global,
        )
        .unwrap();
        let draft_root = paths.skill_drafts_dir().join(&draft.id);
        let package = draft_root.join(DRAFT_PACKAGE_DIR);
        let outside = temp.path().join("outside-package");
        fs::create_dir_all(outside.join("sample-skill")).unwrap();
        fs::write(
            outside.join("sample-skill/SKILL.md"),
            "---\nname: sample-skill\ndescription: Outside\n---\n",
        )
        .unwrap();
        fs::remove_dir_all(&package).unwrap();
        symlink(&outside, &package).unwrap();

        assert!(publish_draft(&paths, &draft.id).is_err());
        assert!(!paths.skills_dir.join("sample-skill").exists());
    }

    #[test]
    fn expired_draft_cannot_be_published_directly() {
        fn set_modified_recursive(path: &Path, modified: SystemTime) {
            if path.is_dir() {
                for entry in fs::read_dir(path).unwrap() {
                    set_modified_recursive(&entry.unwrap().path(), modified);
                }
            }
            File::open(path)
                .unwrap()
                .set_times(std::fs::FileTimes::new().set_modified(modified))
                .unwrap();
        }

        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let config = AppConfig::default();
        let draft = create_draft(
            &config,
            &paths,
            "sample-skill",
            "Use for sample tasks",
            SkillScope::Global,
        )
        .unwrap();
        let draft_root = paths.skill_drafts_dir().join(&draft.id);
        let expired = SystemTime::now() - DRAFT_RETENTION - Duration::from_secs(60);
        set_modified_recursive(&draft_root, expired);

        assert!(publish_draft(&paths, &draft.id).is_err());
        assert!(!draft_root.exists());
        assert!(!paths.skills_dir.join("sample-skill").exists());
    }

    #[test]
    fn malformed_over_limit_draft_is_removed_during_pruning() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let draft = create_draft(
            &AppConfig::default(),
            &paths,
            "sample-skill",
            "Use for sample tasks",
            SkillScope::Global,
        )
        .unwrap();
        let mut directory = PathBuf::from(&draft.skill_dir);
        for index in 0..=(MAX_SKILL_PACKAGE_DEPTH + 5) {
            directory.push(format!("level-{index}"));
        }
        fs::create_dir_all(directory).unwrap();

        assert_eq!(prune_expired_drafts(&paths).unwrap(), 1);
        assert!(!paths.skill_drafts_dir().join(&draft.id).exists());
    }

    #[test]
    fn future_draft_timestamps_are_not_treated_as_expired() {
        fn set_modified_recursive(path: &Path, modified: SystemTime) {
            if path.is_dir() {
                for entry in fs::read_dir(path).unwrap() {
                    set_modified_recursive(&entry.unwrap().path(), modified);
                }
            }
            File::open(path)
                .unwrap()
                .set_times(std::fs::FileTimes::new().set_modified(modified))
                .unwrap();
        }

        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let draft = create_draft(
            &AppConfig::default(),
            &paths,
            "sample-skill",
            "Use for sample tasks",
            SkillScope::Global,
        )
        .unwrap();
        let draft_root = paths.skill_drafts_dir().join(&draft.id);
        set_modified_recursive(
            &draft_root,
            SystemTime::now() + Duration::from_secs(24 * 60 * 60),
        );

        assert_eq!(prune_expired_drafts(&paths).unwrap(), 0);
        assert!(draft_root.is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn revision_tracks_empty_directories_and_executable_bits() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("sample-skill");
        fs::create_dir_all(&root).unwrap();
        let skill_file = root.join("SKILL.md");
        fs::write(
            &skill_file,
            "---\nname: sample-skill\ndescription: Sample\n---\n",
        )
        .unwrap();
        let initial = skill_revision(&root).unwrap();
        fs::create_dir(root.join("empty")).unwrap();
        let with_directory = skill_revision(&root).unwrap();
        assert_ne!(initial, with_directory);
        let mut permissions = fs::metadata(&skill_file).unwrap().permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&skill_file, permissions).unwrap();
        assert_ne!(with_directory, skill_revision(&root).unwrap());
    }

    #[test]
    fn publish_rejects_excessive_directory_depth() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let config = AppConfig::default();
        let draft = create_draft(
            &config,
            &paths,
            "sample-skill",
            "Use for sample tasks",
            SkillScope::Global,
        )
        .unwrap();
        let mut directory = PathBuf::from(&draft.skill_dir);
        for index in 0..=MAX_SKILL_PACKAGE_DEPTH {
            directory.push(format!("level-{index}"));
        }
        fs::create_dir_all(directory).unwrap();

        assert!(publish_draft(&paths, &draft.id).is_err());
        assert!(!paths.skills_dir.join("sample-skill").exists());
    }
}

/// 删掉技能目录里模型自动生成的技能(`miyu reset --include-skills`):只认
/// [`is_generated_skill`] 认得出的,手写技能不动。以前住在记忆库的 `reset_all`
/// 里(memory → skills 的跨子系统直接引用),09-16 挪回技能自己这边,调用方
/// 在上层把两件事接起来。
pub fn purge_generated_skills(skills_dir: &Path) -> Result<usize> {
    if !skills_dir.exists() {
        return Ok(0);
    }
    let mut removed = 0;
    for entry in std::fs::read_dir(skills_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let raw = std::fs::read_to_string(entry.path().join("SKILL.md")).unwrap_or_default();
        if is_generated_skill(&raw) {
            std::fs::remove_dir_all(entry.path())?;
            removed += 1;
        }
    }
    Ok(removed)
}
