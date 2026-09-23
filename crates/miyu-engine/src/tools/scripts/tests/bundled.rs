//! 内置脚本头部 = 迁移前 index.json 的契约(09-05 迁移)。
//!
//! 内置脚本的描述/参数/超时/分组从 index.json 搬进了各脚本头部,index.json 删除。
//! 夹具是迁移前那份 index,这里逐条比对:参数 schema(去掉 description 文案后)
//! 逐字节相同,超时、分组、显示名不变;描述换成英文后首句 ≤60 字符。

use crate::tools::scripts::*;

const LEGACY_INDEX: &str = include_str!(
    "../../../../../../src/tools/scripts/tests/fixtures/bundled-index-2026-09-05.json"
);

/// 出厂人格的资源根:`src/personas/default/`。出厂脚本住 `scripts/`(09-23 从
/// 老的 `src/scripts/personas/default/` 整批搬来),技能带路的脚本住
/// `skills/<技能名>/scripts/`。
fn bundled_persona_dir() -> PathBuf {
    Path::new(miyu_base::WORKSPACE_ROOT).join("src/personas/default")
}

fn bundled_dir() -> PathBuf {
    bundled_persona_dir().join("scripts")
}

/// 出厂脚本的两处家:`scripts/` 与技能树里的 `skills/<技能名>/scripts/`。头部契约
/// (描述/参数/超时/分组/显示名)两边同一套。
///
/// 两处都必须读得到:原来读不到就 `continue`,目录一搬走,依赖它的检查全都
/// 悄悄少查一大半还照样绿。
fn bundled_script_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for dir in [bundled_dir(), bundled_persona_dir().join("skills")] {
        let read_dir =
            std::fs::read_dir(&dir).unwrap_or_else(|error| panic!("{}: {error}", dir.display()));
        let mut entries: Vec<PathBuf> = read_dir
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .collect();
        entries.sort();
        for entry in entries {
            if entry.is_file() {
                paths.push(entry);
            } else if entry.is_dir() {
                let scripts = entry.join("scripts");
                if scripts.is_dir() {
                    for file in std::fs::read_dir(&scripts).unwrap() {
                        let path = file.unwrap().path();
                        if path.is_file() {
                            paths.push(path);
                        }
                    }
                }
            }
        }
    }
    paths.sort();
    paths
}

fn strip_descriptions(value: &mut Value) {
    match value {
        Value::Object(map) => {
            map.remove("description");
            for nested in map.values_mut() {
                strip_descriptions(nested);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(strip_descriptions),
        _ => {}
    }
}

fn first_sentence_chars(text: &str) -> usize {
    text.split_inclusive(['.', '!', '?'])
        .next()
        .unwrap_or(text)
        .trim()
        .chars()
        .count()
}

#[test]
fn bundled_headers_match_the_legacy_index_contracts() {
    let dir = bundled_dir();
    let scan = scan_scripts(&[dir.as_path()]).unwrap();
    assert!(scan.unregistered.is_empty(), "{:?}", scan.unregistered);
    assert!(
        !dir.join("index.json").exists(),
        "内置目录不该再有 index.json"
    );

    let legacy: ScriptIndex = serde_json::from_str(LEGACY_INDEX).unwrap();
    assert_eq!(legacy.scripts.len(), 8);
    for old in legacy.scripts {
        // 09-23 起三件试点(机票/酒店/直播)搬进了技能树,不再在这份目录扫描里;
        // 它们的头部契约由 `bundled_skill_carried_scripts_keep_their_contracts` 管。
        let Some(new) = scan.entries.iter().find(|entry| entry.id == old.id) else {
            continue;
        };
        let mut old_params = old.parameters.clone();
        strip_descriptions(&mut old_params);
        let mut new_params = new.parameters.clone();
        strip_descriptions(&mut new_params);
        assert_eq!(old_params, new_params, "{}: parameters drifted", old.id);
        assert_eq!(old.timeout_seconds, new.timeout_seconds, "{}", old.id);
        assert_eq!(old.groups, new.groups, "{}", old.id);
        assert!(matches!(new.load_policy, LoadPolicy::Group), "{}", old.id);
        assert_eq!(new.always_loaded, None, "{}", old.id);
        // 迁移前的 index 只有中文名,所以比中文槽而不是 `entry.display_name`:
        // 后者按运行时 locale 选,测试在英文 locale 下跑会挑出英文名,拿它跟
        // 中文契约比是假挂。
        let zh_name = metadata_from_script(Path::new(&new.path)).display_names.zh;
        assert_eq!(
            zh_name.as_deref(),
            Some(old.display_name.as_str()),
            "{}",
            old.id
        );
    }
}

/// 搬进技能树的脚本(机票/酒店/直播)头部契约不变:参数 schema(去掉 description
/// 文案后)逐字节相同,超时、分组、显示名不变。进不进常驻面从此只看它住在哪,
/// 头部不再需要任何声明。
#[test]
fn bundled_skill_carried_scripts_keep_their_contracts() {
    let legacy: ScriptIndex = serde_json::from_str(LEGACY_INDEX).unwrap();
    let mut checked = 0;
    for old in legacy.scripts {
        let Some(path) = bundled_script_paths().into_iter().find(|path| {
            path.file_name().and_then(|name| name.to_str()) == Some(old.path.as_str())
        }) else {
            continue;
        };
        // 只在技能树里的那一份上比——老布局的副本已经搬走。
        if !path.starts_with(bundled_persona_dir().join("skills")) {
            continue;
        }
        let metadata = metadata_from_script(&path);
        let mut old_params = old.parameters.clone();
        strip_descriptions(&mut old_params);
        let mut new_params = metadata.parameters.clone().unwrap_or(Value::Null);
        strip_descriptions(&mut new_params);
        assert_eq!(old_params, new_params, "{}: parameters drifted", old.id);
        assert_eq!(old.timeout_seconds, metadata.timeout_seconds, "{}", old.id);
        assert_eq!(old.groups, metadata.groups, "{}", old.id);
        assert_eq!(
            metadata.display_names.zh.as_deref(),
            Some(old.display_name.as_str()),
            "{}",
            old.id
        );
        checked += 1;
    }
    assert_eq!(checked, 3, "三件试点都该在技能树里找到");
}

/// 出厂脚本必须有中文显示名;英文名可选。显示名是给人看的,而工具 id 本来就是
/// 英文,英文界面按 id 兜一个就够(`xhs_search` → `Xhs search`),不值得再维护一份
/// 英文文案。中文名反过来是必填的:没有它,中文界面只能端出 id。
/// 这条闸顺带挡住把中文写进 `Display name:` 的老毛病——两份文档的示例曾经
/// 就是 `# Display name: 番组日历`,照抄的人把中文塞进英文槽,中文界面靠 en→zh
/// 回退才显示对,英文界面反倒露出中文。
#[test]
fn bundled_scripts_carry_a_chinese_display_name() {
    let paths = bundled_script_paths();
    assert!(!paths.is_empty());
    for path in &paths {
        let names = metadata_from_script(path).display_names;
        let english = names.en.unwrap_or_default();
        let chinese = names.zh.unwrap_or_default();
        let id = path.file_name().unwrap().to_string_lossy();
        assert!(
            !chinese.is_empty(),
            "{}: 缺中文显示名,给头部加一行 `# 显示名称：...`",
            id
        );
        assert!(
            english.is_ascii(),
            "{}: `Display name:` 是英文槽,中文名要写在 `显示名称:` 上: {english}",
            id
        );
        assert!(
            chinese
                .chars()
                .any(|character| ('\u{4e00}'..='\u{9fff}').contains(&character)),
            "{}: `显示名称:` 该写中文: {chinese}",
            id
        );
    }
}

#[test]
fn bundled_descriptions_follow_the_header_style_rules() {
    let scan = scan_scripts(&[bundled_dir().as_path()]).unwrap();
    let ids: Vec<&str> = scan.entries.iter().map(|entry| entry.id.as_str()).collect();
    // 09-23 起机票/酒店/直播三件搬进技能树(`skills/<技能名>/scripts/`),
    // 不再出现在这层目录扫描里。
    assert_eq!(
        ids,
        vec![
            "bangumi",
            "battery_care",
            "codec",
            "crack_search",
            "divine",
            "game_compat",
            "get_weather",
            "goofish_search",
            "online_man",
            "query_deepseek_status",
            "query_moegirl",
            "read_clipboard",
            "reddit_search",
            "scientific_calculator",
            "xhs_search",
            "zhihu_search",
        ]
    );
    // 文风规则对两处家一视同仁:老布局目录 + 技能树里的脚本。
    for path in bundled_script_paths() {
        let metadata = metadata_from_script(&path);
        let id = path.file_name().unwrap().to_string_lossy().to_string();
        let description = metadata.descriptions.en.unwrap_or_default();
        assert!(
            description.starts_with(|character: char| character.is_ascii_alphabetic()),
            "{id}: description must be English: {description}"
        );
        assert!(
            first_sentence_chars(&description) <= 60,
            "{id}: first sentence over 60 chars: {description}"
        );
        if let Some(properties) = metadata
            .parameters
            .as_ref()
            .and_then(|parameters| parameters.get("properties"))
            .and_then(Value::as_object)
        {
            for (name, property) in properties {
                // 没有说明是允许的,而且往往是对的:参数名加 enum 已经说清的
                // (`format: md|json`)再写一句是每回合常驻的纯开销(09-21 瘦身)。
                // 这条规则管的是文风——写了就必须是英文(AGENTS §1.5)。
                let Some(description) = property.get("description").and_then(Value::as_str) else {
                    continue;
                };
                assert!(
                    description.starts_with(|character: char| character.is_ascii_alphabetic()),
                    "{id}.{name}: parameter description must be English: {description}"
                );
            }
        }
    }
}

/// 脚本是直接 exec 的(`Command::new(&script_path)`),没有可执行位就是
/// Permission denied。09-23 搬三件试点时 git 把 755 丢成了 644,源码树里
/// 当场全坏——安装包那边 assets.json 给整棵树补 0755,掩盖了这件事。
#[cfg(unix)]
#[test]
fn bundled_scripts_are_executable() {
    use std::os::unix::fs::PermissionsExt as _;
    let paths = bundled_script_paths();
    assert!(!paths.is_empty(), "一件出厂脚本都没扫到,路径是不是变了");
    let missing: Vec<String> = paths
        .iter()
        .filter(|path| {
            std::fs::metadata(path)
                .map(|metadata| metadata.permissions().mode() & 0o111 == 0)
                .unwrap_or(true)
        })
        .map(|path| path.display().to_string())
        .collect();
    assert!(missing.is_empty(), "这些出厂脚本没有可执行位: {missing:?}");
}
