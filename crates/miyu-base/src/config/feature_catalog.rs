//! 引导里「自选功能」那一屏的真相源。终端引导与 WebUI 成员引导共用一份：
//! 哪些插件给开关、哪些永远开着不摆出来、显示名和一句话说明都在这里。
//!
//! 分三档：
//!
//! - **core 与必开项**不出现在引导里：文件读写、看图、搜图、用量、脚本插件
//!   本身、知识库、MCP、记忆、技能。它们是「能用」的底线，关掉只会让人以为坏了。
//! - **可开关的内置插件**（[`TOGGLE_PLUGINS`]）：闹钟、汇率、Arch、API 额度、
//!   表情包、生图、记账——生活助理的配件，不是每个人都要。
//! - **逐个勾的外装件**：每个内置/全局脚本、每个非平台级技能、每台配置里开着的
//!   MCP 服务器（机器级 `mcp.enabled` 关着就整格不摆）；语音只在本机装了
//!   `miyu-voice` 时才给开关。
//!
//! 内置脚本与内置技能对**自定义人格**是可选件：默认不勾（换上自定义人格仍是
//! 纯净状态，09-01），勾了就写进白名单。默认人格（Miyu 本人）默认全勾。
//!
//! 选择最终落到 [`PersonaManifest`]：`plugins.enabled` / `plugins.scripts` /
//! `plugins.skills` / `plugins.mcp` 四个白名单与 `subsystems.*`。全开时白名单写 `None`
//! （= 以后装进来的也自动可见），只有关过东西、或自定义人格勾了内置件才写明细。

use super::builtin_plugins::{MachineSwitch, BUILTIN_PLUGINS, MACHINE_FEATURES};
use super::persona_manifest::{PersonaManifest, Subsystems, PLUGIN_IDS};
use super::AppConfig;

pub use super::builtin_plugins::{plugin_label, TOGGLE_PLUGINS};

/// 这张表是摆给谁看的。
///
/// 引导只摆「不是底线」的那些：文件读写、看图、记忆、技能关掉只会让人以为
/// 坏了。设置界面摆全——dev 人格关掉的正是记忆与技能，要改就得看得见
/// （2026-09-20：引导之后这张表再也没有入口，就是这次要补的）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CatalogScope {
    Onboarding,
    Settings,
}

impl CatalogScope {
    fn everything(self) -> bool {
        self == CatalogScope::Settings
    }
}

/// 子系统在功能表上的一行。子系统是「挂进回合流水线多个点」的东西，开关写在
/// 人格清单的 `subsystems.*` 上。
pub struct SubsystemDescriptor {
    pub id: &'static str,
    pub name_zh: &'static str,
    pub hint_zh: &'static str,
    /// 引导里摆不摆。
    pub in_onboarding: bool,
    pub get: fn(&Subsystems) -> bool,
    pub set: fn(&mut Subsystems, bool),
    /// 这台机器上有没有它（语音要装 miyu-voice、情绪要连着 QQ、人格提醒要
    /// 机器侧开着）。不可用就整行不摆——人格只能在装了的里挑。
    pub available: fn(&FeatureSources) -> bool,
    pub settings: bool,
}

/// 顺序即功能表上「子系统」那一节的顺序；前三项同时也是引导里的顺序。
pub const SUBSYSTEMS: &[SubsystemDescriptor] = &[
    SubsystemDescriptor {
        id: "voice",
        name_zh: "语音功能",
        hint_zh: "唤醒对话、听写、朗读",
        in_onboarding: true,
        get: |subsystems| subsystems.voice,
        set: |subsystems, on| subsystems.voice = on,
        available: |sources| sources.voice_available,
        settings: true,
    },
    SubsystemDescriptor {
        id: "persona_reminder",
        name_zh: "人格提醒",
        hint_zh: "隔几轮提醒模型保持人设",
        in_onboarding: true,
        get: |subsystems| subsystems.persona_reminder,
        set: |subsystems, on| subsystems.persona_reminder = on,
        available: |sources| sources.persona_reminder_available,
        settings: false,
    },
    SubsystemDescriptor {
        id: "emotion",
        name_zh: "情绪与好感度",
        hint_zh: "通讯平台里的情绪状态与好感度",
        in_onboarding: true,
        get: |subsystems| subsystems.emotion,
        set: |subsystems, on| subsystems.emotion = on,
        available: |sources| sources.emotion_available,
        settings: true,
    },
    SubsystemDescriptor {
        id: "memory",
        name_zh: "长期记忆",
        hint_zh: "记忆、联想、日记",
        in_onboarding: false,
        get: |subsystems| subsystems.memory,
        set: |subsystems, on| subsystems.memory = on,
        available: |_| true,
        settings: true,
    },
    SubsystemDescriptor {
        id: "skills",
        name_zh: "技能",
        hint_zh: "技能目录与 load_skill",
        in_onboarding: false,
        get: |subsystems| subsystems.skills,
        set: |subsystems, on| subsystems.skills = on,
        available: |_| true,
        settings: false,
    },
];

/// 功能表上不摆的几项(用户 09-23 拍板):读写文件、外发是「能用」的底线,
/// 人格提醒与情绪好感度不常动。**开关本身照旧生效**,只是不占表上的位置——
/// 引导与设置两处都按这张表跳过(`catalog` 里统一拦)。
pub const UNLISTED_FEATURES: &[&str] =
    &["files", "platform_outreach", "persona_reminder", "emotion"];

pub fn unlisted(id: &str) -> bool {
    UNLISTED_FEATURES.contains(&id)
}

pub fn subsystem(id: &str) -> Option<&'static SubsystemDescriptor> {
    SUBSYSTEMS.iter().find(|item| item.id == id)
}

/// 引导里不摆开关、永远开着的插件 id。
pub fn always_on_plugins() -> impl Iterator<Item = &'static str> {
    PLUGIN_IDS
        .iter()
        .copied()
        .filter(|id| !TOGGLE_PLUGINS.contains(id))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeatureKind {
    Subsystem,
    Plugin,
    Script,
    Skill,
    /// 配置里的一台 MCP 服务器(`mcp.servers[].id`),写回 `plugins.mcp`。
    Mcp,
    /// 只有机器层的能力（网络搜索、识图）：工具面上属于 core，persona.toml
    /// 管不着，勾选直接写 config。只在设置界面摆。
    Machine,
}

/// 引导表里的一行。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeatureItem {
    pub kind: FeatureKind,
    pub id: String,
    pub name: String,
    pub hint: String,
    pub on: bool,
    /// 内置件（Miyu 出厂脚本/技能）。自定义人格下默认不勾，勾了要写进白名单。
    pub builtin: bool,
    /// 有没有「怎么配」的设置页。设置界面据此在行尾摆齿轮。
    pub settings: bool,
    /// 机器层此刻开没开。`None` = 它没有机器层开关（常在）。勾着但机器层关着
    /// 的行要标出来——那是「勾了不生效」。
    pub machine_on: Option<bool>,
}

/// 调用方探到的外装件：脚本 (id, 显示名, 描述, 是否内置)、技能 (名字, 描述, 是否内置)、
/// 语音装没装。配置层不扫目录也不探二进制，谁调谁给。
#[derive(Clone, Debug, Default)]
pub struct FeatureSources {
    pub voice_available: bool,
    /// 机器侧 `prompt.persona_reminder` 开着才摆开关(人格只能在装了的里挑)。
    pub persona_reminder_available: bool,
    /// 情绪与好感度只在通讯平台层生效,QQ 没开就不摆。
    pub emotion_available: bool,
    /// 脚本 (id, 界面名, 界面说明, 是否内置, 带路技能)。`带路技能 = Some(名)`
    /// 的是住在 `skills/<名>/scripts/` 里的脚本:功能表上不单独成行,开关跟着
    /// 那份技能那一行走。
    pub scripts: Vec<(String, String, String, bool, Option<String>)>,
    /// 技能 (id, 界面名, 界面说明, 是否内置)。界面名/说明走人槽,模型槽
    /// (`description`)是英文触发词,摆进设置页会中英混杂(AGENTS §1.5.1)。
    pub skills: Vec<(String, String, String, bool)>,
    /// MCP 服务器 (id, 显示名, 一句话说明):调用方只列机器级 `mcp.enabled` 开着、
    /// 且 `servers[].enabled` 的;机器级关着就传空,整格不摆、清单里的白名单也不动。
    pub mcp_servers: Vec<(String, String, String)>,
}

/// 白名单里点没点名。
fn listed(list: &Option<Vec<String>>, id: &str) -> bool {
    list.as_ref()
        .is_some_and(|list| list.iter().any(|item| item == id))
}

/// 一件外装件此刻开没开：内置件在自定义人格下只看白名单点名，其余 None = 全开。
fn extension_on(
    list: &Option<Vec<String>>,
    id: &str,
    builtin: bool,
    default_persona: bool,
) -> bool {
    if builtin && !default_persona {
        listed(list, id)
    } else {
        list.is_none() || listed(list, id)
    }
}

/// 按人格清单当前的状态摆出整张表。
///
/// 顺序：机器级能力（只在设置界面）→ 子系统 → 内置插件 → 脚本 → 技能 → MCP。
pub fn catalog(
    manifest: &PersonaManifest,
    sources: &FeatureSources,
    default_persona: bool,
    scope: CatalogScope,
    config: Option<&AppConfig>,
) -> Vec<FeatureItem> {
    let mut items = Vec::new();

    // 只有机器层的那几件（网络搜索、识图）。引导里不摆：它们是「能用」的底线。
    if scope.everything() {
        for feature in MACHINE_FEATURES {
            let on = config.is_some_and(|config| (feature.switch.get)(config));
            items.push(FeatureItem {
                kind: FeatureKind::Machine,
                id: feature.id.into(),
                name: feature.name_zh.into(),
                hint: feature.hint_zh.into(),
                on,
                builtin: false,
                settings: feature.settings,
                machine_on: Some(on),
            });
        }
    }

    for descriptor in SUBSYSTEMS {
        if !scope.everything() && !descriptor.in_onboarding {
            continue;
        }
        if unlisted(descriptor.id) {
            continue;
        }
        if !(descriptor.available)(sources) {
            continue;
        }
        items.push(FeatureItem {
            kind: FeatureKind::Subsystem,
            id: descriptor.id.into(),
            name: descriptor.name_zh.into(),
            hint: descriptor.hint_zh.into(),
            on: (descriptor.get)(&manifest.subsystems),
            builtin: false,
            settings: descriptor.settings,
            machine_on: None,
        });
    }

    // 引导只摆可勾的那几件；设置界面连常开件也摆——dev 人格连 files 都关着,
    // 想照着调就得看得见。
    for plugin in BUILTIN_PLUGINS {
        if !scope.everything() && !plugin.toggleable {
            continue;
        }
        if unlisted(plugin.id) {
            continue;
        }
        items.push(FeatureItem {
            kind: FeatureKind::Plugin,
            id: plugin.id.into(),
            name: plugin.name_zh.into(),
            hint: plugin.hint_zh.into(),
            on: manifest.plugin_enabled(plugin.id),
            builtin: false,
            settings: plugin.settings,
            machine_on: config.map(|config| (plugin.installed)(config)),
        });
    }

    for (id, name, hint, builtin, skill) in &sources.scripts {
        // 技能带路的脚本不单独成行:它的开关就是那份技能那一行。
        if skill.is_some() {
            continue;
        }
        if unlisted(id) {
            continue;
        }
        items.push(FeatureItem {
            kind: FeatureKind::Script,
            id: id.clone(),
            name: if name.trim().is_empty() {
                id.clone()
            } else {
                name.clone()
            },
            hint: hint.clone(),
            on: extension_on(&manifest.plugins.scripts, id, *builtin, default_persona),
            builtin: *builtin,
            settings: false,
            machine_on: None,
        });
    }
    for (id, name, hint, builtin) in &sources.skills {
        items.push(FeatureItem {
            kind: FeatureKind::Skill,
            id: id.clone(),
            name: name.clone(),
            hint: hint.clone(),
            on: extension_on(&manifest.plugins.skills, id, *builtin, default_persona),
            builtin: *builtin,
            settings: false,
            machine_on: None,
        });
    }
    // MCP 服务器没有「内置件」一说:None = 全连,写了名单就只连名单上的
    // (与 `tools/mcp.rs::register` 同一判据)。
    for (id, name, hint) in &sources.mcp_servers {
        items.push(FeatureItem {
            kind: FeatureKind::Mcp,
            id: id.clone(),
            name: if name.trim().is_empty() {
                id.clone()
            } else {
                name.clone()
            },
            hint: hint.clone(),
            on: extension_on(&manifest.plugins.mcp, id, false, default_persona),
            builtin: false,
            settings: false,
            machine_on: None,
        });
    }
    items
}

/// 勾上的那些，把机器层的开关也打开（用户 2026-09-20 拍板：勾 = 两层一起开）。
///
/// 取消勾选**不**关机器层：那里存着密钥、尺寸、账号，关掉再勾回来就得重填；
/// 而人格白名单已经把它挡在外面了，留着不生效也不碍事。
pub fn apply_machine_switches(config: &mut AppConfig, items: &[FeatureItem]) {
    for item in items.iter().filter(|item| item.on) {
        let switch: Option<&MachineSwitch> = match item.kind {
            FeatureKind::Machine => {
                super::builtin_plugins::machine_feature(&item.id).map(|feature| &feature.switch)
            }
            FeatureKind::Plugin => super::builtin_plugins::descriptor(&item.id)
                .and_then(|plugin| plugin.switch.as_ref()),
            _ => None,
        };
        if let Some(switch) = switch {
            (switch.set)(config, true);
        }
    }
}

/// 把表上的勾选写回清单。全开 = 白名单留空（`None`），关过才写明细；自定义人格
/// 勾了内置件也得写明细（None 对它意味着「内置一件不挂」）。
///
/// 表里没出现的内置插件（core 与必开项）一律算开——它们本来就不给关。
pub fn apply_selection(
    manifest: &mut PersonaManifest,
    items: &[FeatureItem],
    sources: &FeatureSources,
    default_persona: bool,
) {
    for item in items {
        if item.kind == FeatureKind::Subsystem {
            if let Some(descriptor) = subsystem(&item.id) {
                (descriptor.set)(&mut manifest.subsystems, item.on);
            }
        }
    }
    let plugins_off = items
        .iter()
        .any(|item| item.kind == FeatureKind::Plugin && !item.on);
    manifest.plugins.enabled = plugins_off.then(|| {
        PLUGIN_IDS
            .iter()
            .copied()
            .filter(|id| {
                items
                    .iter()
                    .find(|item| item.kind == FeatureKind::Plugin && item.id == *id)
                    .is_none_or(|item| item.on)
            })
            .map(str::to_string)
            .collect()
    });
    manifest.plugins.scripts = script_allowlist(items, sources, default_persona);
    manifest.plugins.skills = allowlist(items, FeatureKind::Skill, default_persona);
    // 表上没有 MCP 一格(机器级关着)不等于用户决定全连:手写的白名单原样保留。
    if items.iter().any(|item| item.kind == FeatureKind::Mcp) {
        manifest.plugins.mcp = allowlist(items, FeatureKind::Mcp, default_persona);
    }
}

/// 脚本白名单。技能带路的脚本没有自己的行,开关状态跟着带路技能走——技能关掉
/// 它的脚本也视为不可用,打开则可用(09-23)。技能与脚本两边因此不会各说各话:
/// 自定义人格下不会出现「技能开着、脚本没开」或反过来的断裂。
fn script_allowlist(
    items: &[FeatureItem],
    sources: &FeatureSources,
    default_persona: bool,
) -> Option<Vec<String>> {
    let skill_on = |id: &str| {
        items
            .iter()
            .find(|item| item.kind == FeatureKind::Skill && item.id == id)
            .map(|item| item.on)
            // 表上没有的那份技能(平台级技能不进表):它不受开关管,按可用算。
            .unwrap_or(true)
    };
    let mut listed: Vec<(String, bool, bool)> = items
        .iter()
        .filter(|item| item.kind == FeatureKind::Script)
        .map(|item| (item.id.clone(), item.on, item.builtin))
        .collect();
    listed.extend(
        sources
            .scripts
            .iter()
            .filter_map(|(id, _name, _hint, builtin, skill)| {
                skill
                    .as_deref()
                    .map(|skill| (id.clone(), skill_on(skill), *builtin))
            }),
    );
    if listed.is_empty() {
        return None;
    }
    // 默认人格:全开才留空。自定义人格:目录里的全开**且**内置一件没勾才留空。
    let all_on = listed.iter().all(|(_, on, _)| *on);
    let regular_all_on = listed
        .iter()
        .filter(|(_, _, builtin)| !builtin)
        .all(|(_, on, _)| *on);
    let builtin_any_on = listed.iter().any(|(_, on, builtin)| *builtin && *on);
    let keep_none = if default_persona {
        all_on
    } else {
        regular_all_on && !builtin_any_on
    };
    if keep_none {
        return None;
    }
    Some(
        listed
            .into_iter()
            .filter(|(_, on, _)| *on)
            .map(|(id, _, _)| id)
            .collect(),
    )
}

fn allowlist(
    items: &[FeatureItem],
    kind: FeatureKind,
    default_persona: bool,
) -> Option<Vec<String>> {
    let listed: Vec<&FeatureItem> = items.iter().filter(|item| item.kind == kind).collect();
    if listed.is_empty() {
        return None;
    }
    // 默认人格:全开才留空。自定义人格:目录里的全开**且**内置一件没勾才留空
    // (None 对它意味着「内置一件不挂」,勾了内置件就必须写明细)。
    let all_on = listed.iter().all(|item| item.on);
    let regular_all_on = listed
        .iter()
        .filter(|item| !item.builtin)
        .all(|item| item.on);
    let builtin_any_on = listed.iter().any(|item| item.builtin && item.on);
    let keep_none = if default_persona {
        all_on
    } else {
        regular_all_on && !builtin_any_on
    };
    if keep_none {
        return None;
    }
    Some(
        listed
            .iter()
            .filter(|item| item.on)
            .map(|item| item.id.clone())
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sources() -> FeatureSources {
        FeatureSources {
            voice_available: true,
            persona_reminder_available: true,
            emotion_available: true,
            scripts: vec![
                ("s1".into(), "脚本一".into(), String::new(), false, None),
                ("b1".into(), "内置一".into(), String::new(), true, None),
            ],
            skills: vec![
                ("k1".into(), "技能一".into(), "说明一".into(), false),
                ("bk".into(), "内置技能".into(), "内置说明".into(), true),
            ],
            mcp_servers: vec![
                ("m1".into(), "服务器一".into(), "npx server-one".into()),
                ("m2".into(), String::new(), "uvx server-two".into()),
            ],
        }
    }

    /// 设置界面那档比引导多摆什么：机器级能力、记忆与技能、常开的内置插件。
    #[test]
    fn the_settings_scope_shows_what_onboarding_hides() {
        let manifest = PersonaManifest::all();
        let config = AppConfig::default();
        let guided = catalog(&manifest, &sources(), true, CatalogScope::Onboarding, None);
        let full = catalog(
            &manifest,
            &sources(),
            true,
            CatalogScope::Settings,
            Some(&config),
        );
        let ids = |items: &[FeatureItem], kind: FeatureKind| -> Vec<String> {
            items
                .iter()
                .filter(|item| item.kind == kind)
                .map(|item| item.id.clone())
                .collect()
        };
        // 引导里一件机器级能力都不摆。
        assert!(ids(&guided, FeatureKind::Machine).is_empty());
        assert_eq!(ids(&full, FeatureKind::Machine), ["web", "vision"]);
        // 记忆与技能只在设置界面。
        let guided_subs = ids(&guided, FeatureKind::Subsystem);
        assert!(!guided_subs
            .iter()
            .any(|id| id == "memory" || id == "skills"));
        assert!(ids(&full, FeatureKind::Subsystem).contains(&"memory".to_string()));
        // 常开的内置插件（用量查询）也只在设置界面。
        assert!(!ids(&guided, FeatureKind::Plugin).contains(&"usage_query".to_string()));
        assert!(ids(&full, FeatureKind::Plugin).contains(&"usage_query".to_string()));
        // 09-23 起这四件两个 scope 都不摆(UNLISTED_FEATURES):开关照旧生效,
        // 只是不在表上占位置。
        for id in ["files", "platform_outreach", "persona_reminder", "emotion"] {
            assert!(
                !ids(&full, FeatureKind::Plugin).contains(&id.to_string())
                    && !ids(&full, FeatureKind::Subsystem).contains(&id.to_string())
                    && !ids(&guided, FeatureKind::Plugin).contains(&id.to_string())
                    && !ids(&guided, FeatureKind::Subsystem).contains(&id.to_string()),
                "{id} 不该出现在功能表上"
            );
        }
        // 有设置页的那些带着齿轮标记。
        let memes = full
            .iter()
            .find(|item| item.id == "memes")
            .expect("表情包在表上");
        assert!(memes.settings);
        assert_eq!(memes.machine_on, Some(config.plugins.memes.enabled));
    }

    /// 勾上 = 人格层与机器层一起开；取消勾选不碰机器层。
    #[test]
    fn ticking_a_row_also_opens_the_machine_switch() {
        let mut config = AppConfig::default();
        config.plugins.image_generation.enabled = false;
        config.plugins.memes.enabled = false;
        let manifest = PersonaManifest::all();
        let mut items = catalog(
            &manifest,
            &sources(),
            true,
            CatalogScope::Settings,
            Some(&config),
        );
        for item in &mut items {
            item.on = item.id == "image_generation";
        }
        apply_machine_switches(&mut config, &items);
        assert!(config.plugins.image_generation.enabled, "勾上的要打开");
        assert!(
            !config.plugins.memes.enabled,
            "没勾的不动——那儿存着密钥和尺寸"
        );
    }

    #[test]
    fn toggle_plugins_are_all_known_ids() {
        for id in TOGGLE_PLUGINS {
            assert!(PLUGIN_IDS.contains(id), "{id} is not a plugin id");
            assert!(!plugin_label(id).0.is_empty(), "{id} has no label");
        }
        let always: Vec<&str> = always_on_plugins().collect();
        assert_eq!(always.len() + TOGGLE_PLUGINS.len(), PLUGIN_IDS.len());
        assert!(always.contains(&"mcp"));
        assert!(always.contains(&"knowledge_base"));
    }

    #[test]
    fn default_persona_all_on_leaves_allowlists_empty() {
        let mut manifest = PersonaManifest::all();
        let items = catalog(&manifest, &sources(), true, CatalogScope::Onboarding, None);
        assert!(items.iter().all(|item| item.on));
        apply_selection(&mut manifest, &items, &sources(), true);
        assert_eq!(manifest, PersonaManifest::all());
    }

    #[test]
    fn default_persona_turning_things_off_writes_explicit_lists() {
        let mut manifest = PersonaManifest::all();
        let mut items = catalog(&manifest, &sources(), true, CatalogScope::Onboarding, None);
        for item in &mut items {
            if ["voice", "memes", "b1", "k1"].contains(&item.id.as_str()) {
                item.on = false;
            }
        }
        apply_selection(&mut manifest, &items, &sources(), true);
        assert!(!manifest.subsystems.voice);
        let enabled = manifest.plugins.enabled.clone().unwrap();
        assert!(!enabled.contains(&"memes".to_string()));
        assert!(enabled.contains(&"files".to_string()));
        assert!(enabled.contains(&"mcp".to_string()));
        assert_eq!(manifest.plugins.scripts, Some(vec!["s1".to_string()]));
        assert_eq!(manifest.plugins.skills, Some(vec!["bk".to_string()]));
        // 再摆一遍表,勾选状态回得来。
        assert_eq!(
            catalog(&manifest, &sources(), true, CatalogScope::Onboarding, None),
            items
        );
    }

    #[test]
    fn custom_persona_builtins_default_off_and_opt_in() {
        let mut manifest = PersonaManifest::all();
        let items = catalog(&manifest, &sources(), false, CatalogScope::Onboarding, None);
        let by_id = |id: &str| items.iter().find(|item| item.id == id).unwrap().on;
        assert!(by_id("s1") && !by_id("b1") && by_id("k1") && !by_id("bk"));
        // 什么都不动:清单不落盘,内置照样不挂。
        apply_selection(&mut manifest, &items, &sources(), false);
        assert_eq!(manifest.plugins.scripts, None);
        assert_eq!(manifest.plugins.skills, None);
        // 勾一个内置脚本:必须写明细,且把目录里的也一起点名。
        let mut items = items;
        items.iter_mut().find(|item| item.id == "b1").unwrap().on = true;
        apply_selection(&mut manifest, &items, &sources(), false);
        assert_eq!(
            manifest.plugins.scripts,
            Some(vec!["s1".to_string(), "b1".to_string()])
        );
        assert_eq!(manifest.plugins.skills, None);
        assert_eq!(
            catalog(&manifest, &sources(), false, CatalogScope::Onboarding, None),
            items
        );
    }

    /// MCP 逐服务器勾选:None 全勾;关一台就写明细;显示名空的用 id;机器级关着整格不摆、
    /// 手写的白名单原样保留。
    #[test]
    fn mcp_servers_get_per_server_toggles_that_write_the_allowlist() {
        let mut manifest = PersonaManifest::all();
        let mut items = catalog(&manifest, &sources(), true, CatalogScope::Onboarding, None);
        let mcp: Vec<(&str, &str, bool)> = items
            .iter()
            .filter(|item| item.kind == FeatureKind::Mcp)
            .map(|item| (item.id.as_str(), item.name.as_str(), item.on))
            .collect();
        assert_eq!(mcp, [("m1", "服务器一", true), ("m2", "m2", true)]);
        assert_eq!(
            items.last().unwrap().kind,
            FeatureKind::Mcp,
            "MCP 排在最后一格"
        );

        items.iter_mut().find(|item| item.id == "m1").unwrap().on = false;
        apply_selection(&mut manifest, &items, &sources(), true);
        assert_eq!(manifest.plugins.mcp, Some(vec!["m2".to_string()]));
        assert_eq!(manifest.plugins.scripts, None, "别的白名单不受影响");
        assert_eq!(
            catalog(&manifest, &sources(), true, CatalogScope::Onboarding, None),
            items
        );

        // 自定义人格同一套判据(MCP 没有内置件):None 照样全勾。
        let custom = catalog(
            &PersonaManifest::all(),
            &sources(),
            false,
            CatalogScope::Onboarding,
            None,
        );
        assert!(custom
            .iter()
            .filter(|item| item.kind == FeatureKind::Mcp)
            .all(|item| item.on));

        // 机器级 MCP 关着:不摆,也不碰手写的名单。
        let mut hidden = sources();
        hidden.mcp_servers.clear();
        let items = catalog(&manifest, &hidden, true, CatalogScope::Onboarding, None);
        assert!(items.iter().all(|item| item.kind != FeatureKind::Mcp));
        apply_selection(&mut manifest, &items, &hidden, true);
        assert_eq!(manifest.plugins.mcp, Some(vec!["m2".to_string()]));
    }

    /// 09-23 起人格提醒与情绪不在功能表上(用户拍板),但开关本身还在清单里:
    /// 摆表看不见它们,`apply_selection` 也不许碰它们。
    #[test]
    fn unlisted_switches_stay_off_the_table_and_untouched() {
        let mut manifest = PersonaManifest::all();
        let mut items = catalog(&manifest, &sources(), true, CatalogScope::Onboarding, None);
        let ids: Vec<&str> = items
            .iter()
            .filter(|item| item.kind == FeatureKind::Subsystem)
            .map(|item| item.id.as_str())
            .collect();
        assert_eq!(ids, ["voice"]);
        // 表上没有它们,勾选写回也不该动到这两个开关。
        for item in &mut items {
            item.on = false;
        }
        let before = (
            manifest.subsystems.persona_reminder,
            manifest.subsystems.emotion,
        );
        apply_selection(&mut manifest, &items, &sources(), true);
        assert_eq!(
            (
                manifest.subsystems.persona_reminder,
                manifest.subsystems.emotion
            ),
            before,
            "不在表上的开关不该被写回改掉"
        );
        assert!(!manifest.subsystems.voice, "表上的开关照旧能关");
        assert_eq!(
            catalog(&manifest, &sources(), true, CatalogScope::Onboarding, None),
            items
        );

        let mut hidden = sources();
        hidden.persona_reminder_available = false;
        hidden.emotion_available = false;
        let items = catalog(&manifest, &hidden, true, CatalogScope::Onboarding, None);
        assert!(items
            .iter()
            .all(|item| item.kind != FeatureKind::Subsystem || item.id == "voice"));
    }

    /// 09-23:技能带路的脚本不单独成行,它的开关跟着那份技能走——技能关掉,
    /// 脚本白名单里也不再有它;技能打开就回来。技能与脚本两边不脱节。
    #[test]
    fn skill_carried_scripts_ride_the_skill_toggle() {
        let mut sources = sources();
        // bk 是内置技能(true),带一件脚本 g1(也内置)。
        sources.scripts.push((
            "g1".into(),
            "带路脚本".into(),
            String::new(),
            true,
            Some("bk".into()),
        ));
        // 默认人格:全开 → 脚本白名单留空(=全开),技能行那一份就是 g1 的开关。
        let mut manifest = PersonaManifest::all();
        let mut items = catalog(&manifest, &sources, true, CatalogScope::Onboarding, None);
        assert!(
            items
                .iter()
                .all(|item| !(item.kind == FeatureKind::Script && item.id == "g1")),
            "技能带路的脚本不该在表上单独成行"
        );
        apply_selection(&mut manifest, &items, &sources, true);
        assert_eq!(manifest.plugins.scripts, None);
        assert_eq!(manifest.plugins.skills, None);
        // 关掉带路技能 bk:g1 一并从脚本白名单里退出,其余照旧。
        for item in &mut items {
            if item.id == "bk" {
                item.on = false;
            }
        }
        apply_selection(&mut manifest, &items, &sources, true);
        assert_eq!(manifest.plugins.skills, Some(vec!["k1".to_string()]));
        let scripts = manifest.plugins.scripts.clone().unwrap();
        assert!(
            !scripts.contains(&"g1".to_string()),
            "技能关掉,它带路的脚本也得不可用"
        );
        assert!(scripts.contains(&"s1".to_string()) && scripts.contains(&"b1".to_string()));
        // 自定义人格:内置技能默认不挂,g1 也不挂;勾上技能,技能与脚本一起回来。
        let mut custom = PersonaManifest::all();
        let mut items = catalog(&custom, &sources, false, CatalogScope::Onboarding, None);
        apply_selection(&mut custom, &items, &sources, false);
        assert_eq!(custom.plugins.scripts, None, "内置不挂、目录全开 → 留空");
        assert_eq!(custom.plugins.skills, None);
        for item in &mut items {
            if item.id == "bk" {
                item.on = true;
            }
        }
        apply_selection(&mut custom, &items, &sources, false);
        // 目录技能 k1 全开、内置技能 bk 点了名 → 白名单写明细,两件都在。
        assert_eq!(
            custom.plugins.skills,
            Some(vec!["k1".to_string(), "bk".to_string()])
        );
        let scripts = custom.plugins.scripts.clone().unwrap();
        assert!(
            scripts.contains(&"g1".to_string()),
            "技能点开,脚本不该缺席(技能开着、脚本没开是断裂)"
        );
    }
}
