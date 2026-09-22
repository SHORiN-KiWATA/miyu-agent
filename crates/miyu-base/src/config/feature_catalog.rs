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
    /// 英文界面用这一份（理由同 `BuiltinPluginDescriptor`）。
    pub name_en: &'static str,
    pub hint_en: &'static str,
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
        name_zh: "语音",
        hint_zh: "唤醒对话、听写、朗读",
        name_en: "Voice",
        hint_en: "Wake word, dictation, speech",
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
        name_en: "Persona reminder",
        hint_en: "Remind the model to stay in character",
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
        name_en: "Mood and affection",
        hint_en: "Mood and affection on messaging platforms",
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
        name_en: "Memory",
        hint_en: "Long-term memory",
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
        name_en: "Skills",
        hint_en: "Loadable skill packs",
        in_onboarding: false,
        get: |subsystems| subsystems.skills,
        set: |subsystems, on| subsystems.skills = on,
        available: |_| true,
        settings: false,
    },
];

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
    pub scripts: Vec<(String, String, String, bool)>,
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
                name: crate::i18n::text(feature.name_en, feature.name_zh).into(),
                hint: crate::i18n::text(feature.hint_en, feature.hint_zh).into(),
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
        if !(descriptor.available)(sources) {
            continue;
        }
        items.push(FeatureItem {
            kind: FeatureKind::Subsystem,
            id: descriptor.id.into(),
            name: crate::i18n::text(descriptor.name_en, descriptor.name_zh).into(),
            hint: crate::i18n::text(descriptor.hint_en, descriptor.hint_zh).into(),
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
        items.push(FeatureItem {
            kind: FeatureKind::Plugin,
            id: plugin.id.into(),
            name: crate::i18n::text(plugin.name_en, plugin.name_zh).into(),
            hint: crate::i18n::text(plugin.hint_en, plugin.hint_zh).into(),
            on: manifest.plugin_enabled(plugin.id),
            builtin: false,
            settings: plugin.settings,
            machine_on: config.map(|config| (plugin.installed)(config)),
        });
    }

    for (id, name, hint, builtin) in &sources.scripts {
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
    manifest.plugins.scripts = allowlist(items, FeatureKind::Script, default_persona);
    manifest.plugins.skills = allowlist(items, FeatureKind::Skill, default_persona);
    // 表上没有 MCP 一格(机器级关着)不等于用户决定全连:手写的白名单原样保留。
    if items.iter().any(|item| item.kind == FeatureKind::Mcp) {
        manifest.plugins.mcp = allowlist(items, FeatureKind::Mcp, default_persona);
    }
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
                ("s1".into(), "脚本一".into(), String::new(), false),
                ("b1".into(), "内置一".into(), String::new(), true),
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
        // 常开的内置插件（文件、脚本）也只在设置界面。
        assert!(!ids(&guided, FeatureKind::Plugin).contains(&"files".to_string()));
        assert!(ids(&full, FeatureKind::Plugin).contains(&"files".to_string()));
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
        apply_selection(&mut manifest, &items, true);
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
        apply_selection(&mut manifest, &items, true);
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
        apply_selection(&mut manifest, &items, false);
        assert_eq!(manifest.plugins.scripts, None);
        assert_eq!(manifest.plugins.skills, None);
        // 勾一个内置脚本:必须写明细,且把目录里的也一起点名。
        let mut items = items;
        items.iter_mut().find(|item| item.id == "b1").unwrap().on = true;
        apply_selection(&mut manifest, &items, false);
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
        apply_selection(&mut manifest, &items, true);
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
        apply_selection(&mut manifest, &items, true);
        assert_eq!(manifest.plugins.mcp, Some(vec!["m2".to_string()]));
    }

    /// 人格提醒与情绪两个开关从此有 UI 入口:摆表能看见、关掉能写回清单、机器没装就不摆。
    #[test]
    fn persona_reminder_and_emotion_toggles_round_trip() {
        let mut manifest = PersonaManifest::all();
        let mut items = catalog(&manifest, &sources(), true, CatalogScope::Onboarding, None);
        let ids: Vec<&str> = items
            .iter()
            .filter(|item| item.kind == FeatureKind::Subsystem)
            .map(|item| item.id.as_str())
            .collect();
        assert_eq!(ids, ["voice", "persona_reminder", "emotion"]);
        for item in &mut items {
            if item.id == "persona_reminder" || item.id == "emotion" {
                item.on = false;
            }
        }
        apply_selection(&mut manifest, &items, true);
        assert!(!manifest.subsystems.persona_reminder && !manifest.subsystems.emotion);
        assert!(manifest.subsystems.voice, "没动的开关不受影响");
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
}

#[cfg(test)]
mod bilingual_tests {
    use super::*;

    /// 三张表每一条都得有英文名与英文说明，而且不能照抄中文。
    ///
    /// 09-23 之前只有中文：英文 locale 下脚本那一栏按 locale 变英文，而内置功能、
    /// 子系统、网络搜索/识图这三类没得选只能留中文，**同一页混两种语言**
    /// （用户截图）。以后往表里加条目漏了英文名，这条会当场红。
    #[test]
    fn every_catalog_entry_carries_both_languages() {
        let mut missing = Vec::new();
        for plugin in crate::config::builtin_plugins::BUILTIN_PLUGINS {
            if plugin.name_en.trim().is_empty() || plugin.hint_en.trim().is_empty() {
                missing.push(format!("plugin {}", plugin.id));
            }
        }
        for feature in crate::config::builtin_plugins::MACHINE_FEATURES {
            if feature.name_en.trim().is_empty() || feature.hint_en.trim().is_empty() {
                missing.push(format!("machine feature {}", feature.id));
            }
        }
        for descriptor in SUBSYSTEMS {
            if descriptor.name_en.trim().is_empty() || descriptor.hint_en.trim().is_empty() {
                missing.push(format!("subsystem {}", descriptor.id));
            }
        }
        assert!(missing.is_empty(), "这些条目缺英文名/英文说明: {missing:?}");
    }

    /// 英文名不能是中文——照抄一份中文进去等于没加。
    #[test]
    fn the_english_names_are_not_chinese() {
        let chinese = |value: &str| {
            value
                .chars()
                .any(|ch| ('\u{4e00}'..='\u{9fff}').contains(&ch))
        };
        for plugin in crate::config::builtin_plugins::BUILTIN_PLUGINS {
            assert!(
                !chinese(plugin.name_en),
                "{} 的英文名里有中文: {}",
                plugin.id,
                plugin.name_en
            );
        }
        for descriptor in SUBSYSTEMS {
            assert!(
                !chinese(descriptor.name_en),
                "{} 的英文名里有中文: {}",
                descriptor.id,
                descriptor.name_en
            );
        }
    }
}

#[cfg(test)]
mod locale_switch_tests {
    use super::*;

    /// 同一张表在两种 locale 下给出不同语言——这是用户看到的那个现象的直接判据。
    #[test]
    fn the_same_entry_renders_in_the_requested_language() {
        let plugin = crate::config::builtin_plugins::BUILTIN_PLUGINS
            .iter()
            .find(|item| item.id == "alarm")
            .expect("alarm is a built-in plugin");
        assert_eq!(
            crate::i18n::text_for(crate::i18n::Locale::Zh, plugin.name_en, plugin.name_zh),
            "闹钟"
        );
        assert_eq!(
            crate::i18n::text_for(crate::i18n::Locale::En, plugin.name_en, plugin.name_zh),
            "Alarm"
        );
    }
}
