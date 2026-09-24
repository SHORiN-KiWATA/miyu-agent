//! persona 清单(09-10 分层架构阶段 4):一个 persona 目录里的 `persona.toml`,
//! 声明它启用哪些**子系统**(挂进回合流水线多个点的:记忆、人格提醒、语音、
//! 情绪)和哪些**插件**(只往工具面加东西的:内置插件与外装脚本/技能/MCP)。
//! 技能 09-24 从子系统并入插件(`plugins.enabled` 里的 `"skills"`),老文件的
//! `subsystems.skills` 读入时折算,见 [`PersonaManifestWire::into_manifest`]。
//!
//! 这是「模式」退场后唯一的配置单位:dev 是启用集为空的内置 persona,默认人格
//! 是「全部启用」。运行时按启用集**决定构造什么**,不是装了再关——记忆关着
//! 就不建库、不注入、不写日记(setup.rs / turn_loop 按这里裁决)。
//!
//! 文件缺失时按内置默认:persona 名为 `dev` → [`PersonaManifest::core_only`],
//! 其余 → [`PersonaManifest::all`]。解析失败记 warn 并退回默认,不让一个手写
//! 错误把 persona 整个弄哑。

use crate::config::AppConfig;
use crate::paths::MiyuPaths;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::PathBuf;

pub const PERSONA_MANIFEST_FILE: &str = "persona.toml";

pub use super::builtin_plugins::PLUGIN_IDS;

/// 已退役的插件 id:存量 persona.toml 里写着也当没写(09-13 删 deep_research /
/// diagnostics,package_advisor 并入 archlinux;09-21 删 api_quota——只会查
/// DeepSeek 与 OpenRouter 两家,一件脚本工具就能替代,不值一个内置插件加
/// 一份常驻工具契约)。
pub const RETIRED_PLUGIN_IDS: &[&str] = &[
    "deep_research",
    "diagnostics",
    "package_advisor",
    "api_quota",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Subsystems {
    /// 长期记忆:工具、每轮联想注入、回合后日记/经历、逐出库、系统提示前言。
    pub memory: bool,
    /// 人格提醒(化石注入,间隔仍在 config.prompt.persona_reminder_interval)。
    pub persona_reminder: bool,
    /// 语音:唤醒对话、听写、TTS 工具(speak / voice_chat)。
    pub voice: bool,
    /// 情绪与好感度(只在通讯平台层生效;这里是 persona 的意愿位)。
    pub emotion: bool,
}

impl Default for Subsystems {
    fn default() -> Self {
        Self {
            memory: true,
            persona_reminder: true,
            voice: true,
            emotion: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PluginSelection {
    /// 缺省(None)= 本机装了的、config 开着的全部;写了就是白名单。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<Vec<String>>,
    /// 脚本工具按 id 的白名单(阶段 8:成员人格逐个勾脚本);None = 全部。
    /// 只在 `scripts` 插件开着时有意义。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scripts: Option<Vec<String>>,
    /// 技能按名字的白名单;None = 全部。平台级内置技能(skill-creator /
    /// script-creator)不受它管——那是「如何扩展自己」的元能力。
    /// 只在 `skills` 插件开着时有意义。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skills: Option<Vec<String>>,
    /// MCP 服务器按 id 的白名单;None = 配置里开着的全部。关掉的服务器
    /// 连 tools/list 都不拉。只在 `mcp` 插件开着时有意义。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcp: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PersonaManifest {
    pub subsystems: Subsystems,
    pub plugins: PluginSelection,
}

/// `[subsystems]` 的读取态:比 [`Subsystems`] 多一个 09-24 之前的 `skills` 位。
/// 写出去永远走 [`Subsystems`](没有这一位),老字段不会复活。
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct SubsystemsWire {
    memory: Option<bool>,
    skills: Option<bool>,
    persona_reminder: Option<bool>,
    voice: Option<bool>,
    emotion: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct PersonaManifestWire {
    subsystems: SubsystemsWire,
    plugins: PluginSelection,
}

impl PersonaManifestWire {
    /// 把读取态折成运行时清单,顺带把老的 `subsystems.skills` 折进插件名单,
    /// 让技能开没开跟升级前一模一样:
    ///
    /// - 关着:技能不在启用名单里(名单原本是全开就补一份不含技能的明细);
    /// - 开着、名单是明细:当年名单里不可能有技能(它还不是插件),补上——
    ///   不补的话,写过明细名单的老人格升级后技能被悄悄关掉(09-23 那版半成品
    ///   漏的就是这条);
    /// - 开着、名单全开,或没写这个旧字段(新写法):名单说了算。
    ///
    /// 程序写出的老文件五个子系统位都写全,所以「没写」只会是新写法或手写文件。
    /// 幂等:折算后写出的文本不带 `subsystems.skills`,再读回来是同一份状态。
    fn into_manifest(self) -> PersonaManifest {
        let subsystems = Subsystems {
            memory: self.subsystems.memory.unwrap_or(true),
            persona_reminder: self.subsystems.persona_reminder.unwrap_or(true),
            voice: self.subsystems.voice.unwrap_or(true),
            emotion: self.subsystems.emotion.unwrap_or(true),
        };
        let mut plugins = self.plugins;
        match (self.subsystems.skills, plugins.enabled.as_mut()) {
            (Some(false), None) => {
                plugins.enabled = Some(
                    PLUGIN_IDS
                        .iter()
                        .filter(|id| **id != SKILLS_PLUGIN)
                        .map(|id| id.to_string())
                        .collect(),
                );
            }
            (Some(false), Some(list)) => list.retain(|id| id != SKILLS_PLUGIN),
            (Some(true), Some(list)) if !list.iter().any(|id| id == SKILLS_PLUGIN) => {
                list.push(SKILLS_PLUGIN.to_string());
            }
            _ => {}
        }
        PersonaManifest {
            subsystems,
            plugins,
        }
    }
}

impl<'de> Deserialize<'de> for PersonaManifest {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(PersonaManifestWire::deserialize(deserializer)?.into_manifest())
    }
}

/// 技能插件的 id。
pub const SKILLS_PLUGIN: &str = "skills";

impl Default for PersonaManifest {
    fn default() -> Self {
        Self::all()
    }
}

impl PersonaManifest {
    /// 默认人格:子系统全开,插件按 config 全上——今天的 normal。
    pub fn all() -> Self {
        Self {
            subsystems: Subsystems::default(),
            plugins: PluginSelection::default(),
        }
    }

    /// dev:core 之上一件都不挂。唯一例外是 `platform_outreach`(写代码时
    /// 「跑完发我手机」是真需求,09-05 用户拍板),它自己还受「终端外发开着且
    /// QQ 连着」的机器条件。MCP 也不挂(09-13 用户拍板:dev 只留核心工具)。
    pub fn core_only() -> Self {
        Self {
            subsystems: Subsystems {
                memory: false,
                persona_reminder: false,
                voice: false,
                emotion: false,
            },
            plugins: PluginSelection {
                enabled: Some(vec!["platform_outreach".to_string()]),
                scripts: None,
                skills: None,
                mcp: None,
            },
        }
    }

    pub fn builtin_for(persona: &str) -> Self {
        if persona.trim() == crate::config::DEV_PERSONA {
            Self::core_only()
        } else {
            Self::all()
        }
    }

    pub fn manifest_path(config: &AppConfig, paths: &MiyuPaths, persona: &str) -> PathBuf {
        config
            .persona_memory_data_dir(paths, persona)
            .join(PERSONA_MANIFEST_FILE)
    }

    /// 读 persona 目录里的清单;没有文件用内置默认,坏文件记 warn 退回默认。
    pub fn load(config: &AppConfig, paths: &MiyuPaths, persona: &str) -> Self {
        let path = Self::manifest_path(config, paths, persona);
        match std::fs::read_to_string(&path) {
            Ok(raw) => match Self::parse(&raw) {
                Ok(manifest) => manifest,
                Err(error) => {
                    tracing::warn!(
                        path = %path.display(),
                        %error,
                        "persona.toml did not parse; using the built-in defaults"
                    );
                    Self::builtin_for(persona)
                }
            },
            Err(_) => Self::builtin_for(persona),
        }
    }

    pub fn parse(raw: &str) -> anyhow::Result<Self> {
        let manifest: Self = toml::from_str(raw)?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn to_toml(&self) -> String {
        toml::to_string_pretty(self).unwrap_or_default()
    }

    /// 白名单里出现不认识的插件 id 是错误——静默忽略会让用户以为开了。
    /// 已退役的 id(见 [`RETIRED_PLUGIN_IDS`])除外:存量 persona.toml 还写着
    /// 它们,当成"没有这件"跳过,别让一次删插件把整个人格锁在门外。
    pub fn validate(&self) -> anyhow::Result<()> {
        if let Some(enabled) = &self.plugins.enabled {
            let known: BTreeSet<&str> = PLUGIN_IDS.iter().copied().collect();
            let unknown: Vec<&str> = enabled
                .iter()
                .map(String::as_str)
                .filter(|id| !known.contains(id) && !RETIRED_PLUGIN_IDS.contains(id))
                .collect();
            if !unknown.is_empty() {
                anyhow::bail!(
                    "unknown plugin id(s) in persona.toml: {}; known: {}",
                    unknown.join(", "),
                    PLUGIN_IDS.join(", ")
                );
            }
        }
        Ok(())
    }

    pub fn plugin_enabled(&self, id: &str) -> bool {
        match &self.plugins.enabled {
            None => true,
            Some(list) => list.iter().any(|item| item == id),
        }
    }

    /// 这个 persona 的记忆子系统是否构造:persona 意愿 × 机器配置。
    pub fn memory_enabled(&self, config: &AppConfig) -> bool {
        self.subsystems.memory && config.memory_config().enabled
    }

    /// 技能挂不挂:persona 的插件位 × 机器级 `skills.enabled`。注册、技能带路
    /// 脚本的可见性、`miyu host` 查询共用这一道判据(09-24 并入插件前是子系统
    /// 快照里的 `skills`,乘法不变)。
    pub fn skills_enabled(&self, config: &AppConfig) -> bool {
        self.plugin_enabled(SKILLS_PLUGIN) && config.skills.enabled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 三条白名单的裁决点各在装工具面处(scripts/refresh.rs、skills/mod.rs、
    /// tools/mcp.rs::register 直接读字段);这里只钉清单本身:解析、全开不落盘、dev 不带 MCP。
    #[test]
    fn allowlists_parse_and_stay_off_disk_when_all_on() {
        let all = PersonaManifest::all();
        assert!(all.plugin_enabled("mcp"));
        assert!(
            all.plugins.scripts.is_none()
                && all.plugins.skills.is_none()
                && all.plugins.mcp.is_none()
        );
        let parsed = PersonaManifest::parse(
            "[plugins]\nscripts = [\"a\"]\nskills = [\"k\"]\nmcp = [\"srv\"]\n",
        )
        .unwrap();
        assert_eq!(parsed.plugins.scripts, Some(vec!["a".to_string()]));
        assert_eq!(parsed.plugins.skills, Some(vec!["k".to_string()]));
        assert_eq!(parsed.plugins.mcp, Some(vec!["srv".to_string()]));
        // dev 不带 MCP(09-13 用户拍板)。
        assert!(!PersonaManifest::core_only().plugin_enabled("mcp"));
        // 全开时白名单不落盘。
        assert!(!all.to_toml().contains("skills = ["));
        assert!(!all.to_toml().contains("mcp = ["));
    }

    #[test]
    fn defaults_are_all_on_and_dev_is_core_only() {
        let all = PersonaManifest::all();
        assert!(all.subsystems.memory && all.subsystems.voice);
        assert!(
            all.plugin_enabled("memes")
                && all.plugin_enabled("scripts")
                && all.plugin_enabled(SKILLS_PLUGIN)
        );
        let dev = PersonaManifest::core_only();
        assert!(!dev.subsystems.memory && !dev.plugin_enabled(SKILLS_PLUGIN));
        assert!(dev.plugin_enabled("platform_outreach"));
        assert!(!dev.plugin_enabled("memes"));
        assert_eq!(PersonaManifest::builtin_for("dev"), dev);
        assert_eq!(PersonaManifest::builtin_for("default"), all);
    }

    #[test]
    fn parses_partial_toml_and_keeps_the_rest_default() {
        let manifest = PersonaManifest::parse(
            "[subsystems]\nmemory = false\n\n[plugins]\nenabled = [\"ledger\", \"memes\"]\n",
        )
        .unwrap();
        assert!(!manifest.subsystems.memory);
        assert!(manifest.subsystems.voice, "没写的子系统保持默认开");
        // 新写法(没有 subsystems.skills):技能是插件,名单没点名就是关。
        assert!(!manifest.plugin_enabled(SKILLS_PLUGIN));
        assert!(manifest.plugin_enabled("ledger"));
        assert!(!manifest.plugin_enabled("knowledge_base"));
        // 往返:写出来再读回来一致。
        let again = PersonaManifest::parse(&manifest.to_toml()).unwrap();
        assert_eq!(again, manifest);
    }

    /// 09-24 技能从子系统并入插件:老 `subsystems.skills` 读入即折进插件名单,
    /// 技能开没开跟升级前一样;写出去不再带老字段,再读回来不变。
    #[test]
    fn legacy_skills_switch_folds_into_the_plugin_list() {
        let read = |raw: &str| {
            let manifest = PersonaManifest::parse(raw).unwrap();
            let text = manifest.to_toml();
            let value: toml::Value = toml::from_str(&text).unwrap();
            assert!(
                value
                    .get("subsystems")
                    .and_then(|table| table.get("skills"))
                    .is_none(),
                "写出去不能再带 subsystems.skills: {text}"
            );
            assert_eq!(
                PersonaManifest::parse(&text).unwrap(),
                manifest,
                "折算要幂等"
            );
            manifest
        };

        // 关着、名单全开:补一份不含技能的明细,其余插件照旧开。
        let off = read("[subsystems]\nskills = false\n");
        assert!(!off.plugin_enabled(SKILLS_PLUGIN));
        assert!(off.plugin_enabled("memes") && off.plugin_enabled("scripts"));
        // 开着、名单全开:什么都不用写。
        let on = read("[subsystems]\nskills = true\n");
        assert!(on.plugin_enabled(SKILLS_PLUGIN));
        assert_eq!(on.plugins.enabled, None);
        // 开着、名单是明细:当年名单里不可能有技能,得补上,否则升级后技能被悄悄关掉。
        let listed = read("[subsystems]\nskills = true\n\n[plugins]\nenabled = [\"memes\"]\n");
        assert!(listed.plugin_enabled(SKILLS_PLUGIN) && listed.plugin_enabled("memes"));
        assert!(!listed.plugin_enabled("ledger"));
        // 关着、名单是明细:照旧关。
        let listed_off = read("[subsystems]\nskills = false\n\n[plugins]\nenabled = [\"memes\"]\n");
        assert!(!listed_off.plugin_enabled(SKILLS_PLUGIN));
        // 新写法:名单说了算。
        assert!(
            read("[plugins]\nenabled = [\"memes\", \"skills\"]\n").plugin_enabled(SKILLS_PLUGIN)
        );
        assert!(!read("[plugins]\nenabled = [\"memes\"]\n").plugin_enabled(SKILLS_PLUGIN));

        // 本机两份真实人格文件的形状(09-24):默认人格全开,另一份全关、名单为空。
        let default_persona = read(
            "[subsystems]\nmemory = true\nskills = true\npersona_reminder = true\nvoice = true\nemotion = true\n\n[plugins]\n",
        );
        assert_eq!(default_persona, PersonaManifest::all());
        let quiet = read(
            "[subsystems]\nmemory = false\nskills = false\npersona_reminder = false\nvoice = false\nemotion = false\n\n[plugins]\nenabled = []\nscripts = []\n",
        );
        assert!(!quiet.plugin_enabled(SKILLS_PLUGIN));
        assert_eq!(quiet.plugins.enabled, Some(Vec::new()));
    }

    #[test]
    fn unknown_plugin_ids_are_rejected() {
        let error = PersonaManifest::parse("[plugins]\nenabled = [\"weather\"]\n").unwrap_err();
        assert!(error.to_string().contains("unknown plugin id"), "{error}");
    }

    /// 09-13 退役的插件 id 还留在存量 persona.toml 里:当没写,不报错。
    #[test]
    fn retired_plugin_ids_are_tolerated() {
        let manifest = PersonaManifest::parse(
            "[plugins]\nenabled = [\"deep_research\", \"package_advisor\", \"diagnostics\", \"memes\"]\n",
        )
        .unwrap();
        assert!(manifest.plugin_enabled("memes"));
        assert!(!manifest.plugin_enabled("archlinux"));
        for id in RETIRED_PLUGIN_IDS {
            assert!(!PLUGIN_IDS.contains(id), "{id} 不该同时在两张表里");
        }
    }

    #[test]
    fn missing_or_broken_file_falls_back_to_builtin() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let paths = crate::paths::MiyuPaths {
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
        };
        let config = AppConfig::default();
        assert_eq!(
            PersonaManifest::load(&config, &paths, "dev"),
            PersonaManifest::core_only()
        );
        let path = PersonaManifest::manifest_path(&config, &paths, "default");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "this is = not toml [").unwrap();
        assert_eq!(
            PersonaManifest::load(&config, &paths, "default"),
            PersonaManifest::all()
        );
        std::fs::write(&path, "[subsystems]\nvoice = false\n").unwrap();
        assert!(
            !PersonaManifest::load(&config, &paths, "default")
                .subsystems
                .voice
        );
    }
}
