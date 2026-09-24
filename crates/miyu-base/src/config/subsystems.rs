//! 子系统挂接表(09-16,core/normal 接口治理 Phase 3)。
//!
//! 四个 normal 子系统——记忆、人格提醒、语音、情绪——各自从哪儿进回合
//! 流水线、由谁决定开关,以前散在六七处(`tools::compose_core`、
//! `tools::compose_providers`、`agent/setup.rs` 两处、`turn_loop/parallel.rs`、
//! real_context 插件),每处自己读一遍 persona.toml、自己拼一遍「人格意愿 ×
//! 机器配置」:`emotion` 甚至没有任何运行时读者;`memory` 在 Agent 构造期看清单、
//! 在 `prepare_for_turn` 重组系统提示词时只看机器配置,清单关着的人格第一回合起
//! 前言又回来了。
//!
//! 现在只有这一张表:[`SUBSYSTEMS`] 记录 id、挂接阶段与启用判定;
//! [`EnabledSubsystems::resolve`] 把清单与配置折成一份快照——Agent 构造时取一次
//! (`reload_config` 重取)、组工具面时取一次、平台插件按回合取,各挂接点只看
//! 快照。中途改 persona.toml 只影响之后新建的 Agent(daemon 每回合新建)。
//!
//! 规则:persona 只能在「本机装了的」里挑——清单开着但机器关着仍是关;清单关着
//! 就整套不构造(不注册工具、不注入、不写库)。

use super::persona_manifest::PersonaManifest;
use super::AppConfig;

/// 子系统进回合流水线的阶段。一个子系统可以挂多个阶段;各阶段内的顺序见
/// `docs/interfaces/subsystems.md`。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubsystemPhase {
    /// 组工具面(`tools::compose_registry`)。
    ToolRegistration,
    /// 组系统提示词——进稳定前缀,字节随会话不变。
    SystemPrompt,
    /// 模型请求前的回合尾注入(联想记忆、人格提醒),不进稳定前缀。
    BeforeModel,
    /// 回合结束后的副作用(日记、逐出库归档)。
    AfterTurn,
    /// 只在通讯平台层经 `PlatformPlugin` hook 生效。
    PlatformHooks,
}

/// 一个子系统的描述符:`id` 与 [`super::persona_manifest::Subsystems`] 的字段同名,
/// `enabled` 是「人格意愿 × 机器配置」的唯一判定。
pub struct SubsystemDescriptor {
    pub id: &'static str,
    /// 机器可读的挂接阶段:今天只有测试与 `docs/interfaces/subsystems.md` 读它,
    /// 运行时不按它派发(各挂接点仍是显式调用)。
    #[allow(dead_code)]
    pub phases: &'static [SubsystemPhase],
    pub enabled: fn(&PersonaManifest, &AppConfig) -> bool,
}

fn memory_enabled(manifest: &PersonaManifest, config: &AppConfig) -> bool {
    manifest.memory_enabled(config)
}

/// 提醒还受「dev 无人格」与 `prompt.persona_reminder_interval` 两条规则约束——
/// 那是「怎么提醒」,不是「要不要这个子系统」。
fn persona_reminder_enabled(manifest: &PersonaManifest, config: &AppConfig) -> bool {
    manifest.subsystems.persona_reminder && config.prompt.persona_reminder
}

/// 唤醒对话(`voice.enabled`)与播报(`voice.tts`)任一激活就算装了;两件工具
/// 各自再看自己那一位。
fn voice_enabled(manifest: &PersonaManifest, config: &AppConfig) -> bool {
    manifest.subsystems.voice && (config.voice.enabled || config.voice.tts.is_active())
}

/// 机器侧的 `affection_enable` / `emotion_enable` 在 real_context 插件设置里
/// (QQ 专属,不在 `AppConfig` 的类型化字段上),这里只折人格意愿,插件把两者相乘。
fn emotion_enabled(manifest: &PersonaManifest, _config: &AppConfig) -> bool {
    manifest.subsystems.emotion
}

pub const SUBSYSTEMS: &[SubsystemDescriptor] = &[
    SubsystemDescriptor {
        id: "memory",
        phases: &[
            SubsystemPhase::ToolRegistration,
            SubsystemPhase::SystemPrompt,
            SubsystemPhase::BeforeModel,
            SubsystemPhase::AfterTurn,
        ],
        enabled: memory_enabled,
    },
    SubsystemDescriptor {
        id: "persona_reminder",
        phases: &[SubsystemPhase::BeforeModel],
        enabled: persona_reminder_enabled,
    },
    SubsystemDescriptor {
        id: "voice",
        phases: &[
            SubsystemPhase::ToolRegistration,
            SubsystemPhase::SystemPrompt,
        ],
        enabled: voice_enabled,
    },
    SubsystemDescriptor {
        id: "emotion",
        phases: &[SubsystemPhase::PlatformHooks],
        enabled: emotion_enabled,
    },
];

/// 清单 × 配置折出来的启用快照。字段与 [`SUBSYSTEMS`] 一一对应。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EnabledSubsystems {
    pub memory: bool,
    pub persona_reminder: bool,
    pub voice: bool,
    pub emotion: bool,
}

impl EnabledSubsystems {
    pub fn resolve(manifest: &PersonaManifest, config: &AppConfig) -> Self {
        let on = |id: &str| {
            SUBSYSTEMS
                .iter()
                .find(|descriptor| descriptor.id == id)
                .is_some_and(|descriptor| (descriptor.enabled)(manifest, config))
        };
        Self {
            memory: on("memory"),
            persona_reminder: on("persona_reminder"),
            voice: on("voice"),
            emotion: on("emotion"),
        }
    }
}

impl PersonaManifest {
    /// 这个 persona 在这台机器上实际构造哪些子系统。
    pub fn enabled_subsystems(&self, config: &AppConfig) -> EnabledSubsystems {
        EnabledSubsystems::resolve(self, config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 给 `Subsystems` 加一个字段就必须在表里登记一行:表是全仓唯一真相源。
    #[test]
    fn table_lists_every_manifest_switch_exactly_once() {
        let ids: Vec<&str> = SUBSYSTEMS.iter().map(|descriptor| descriptor.id).collect();
        assert_eq!(ids, ["memory", "persona_reminder", "voice", "emotion"]);
        let fields =
            serde_json::to_value(super::super::persona_manifest::Subsystems::default()).unwrap();
        let mut fields: Vec<&str> = fields
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        let mut sorted = ids.clone();
        fields.sort_unstable();
        sorted.sort_unstable();
        assert_eq!(
            fields, sorted,
            "persona.toml 的 [subsystems] 字段与挂接表不一致"
        );
        for descriptor in SUBSYSTEMS {
            assert!(
                !descriptor.phases.is_empty(),
                "{} 没登记挂接阶段",
                descriptor.id
            );
        }
    }

    /// 机器全装、人格全开:快照就是机器开关;dev 的 core_only 一件都不构造。
    #[test]
    fn core_only_constructs_nothing_even_with_everything_installed() {
        let mut config = AppConfig::default();
        config.voice.enabled = true;
        config.prompt.persona_reminder = true;
        assert!(config.memory_config().enabled);
        let all = PersonaManifest::all().enabled_subsystems(&config);
        assert_eq!(
            all.ids(),
            ["memory", "persona_reminder", "voice", "emotion"]
        );
        let dev = PersonaManifest::core_only().enabled_subsystems(&config);
        assert!(dev.is_empty(), "{dev:?}");
        assert!(dev.ids().is_empty());
    }

    /// 人格不能把机器没装的东西打开;情绪只折人格意愿(机器位在插件设置里)。
    #[test]
    fn persona_cannot_enable_what_the_machine_lacks() {
        let mut config = AppConfig::default();
        config.memory.enabled = false;
        config.voice.enabled = false;
        config.voice.tts.enabled = false;
        config.prompt.persona_reminder = false;
        let resolved = PersonaManifest::all().enabled_subsystems(&config);
        assert_eq!(
            resolved,
            EnabledSubsystems {
                memory: false,
                persona_reminder: false,
                voice: false,
                emotion: true,
            }
        );
    }

    /// 清单关一件就少一件,其余照机器。
    #[test]
    fn manifest_switches_turn_subsystems_off_individually() {
        let mut config = AppConfig::default();
        config.voice.enabled = true;
        config.prompt.persona_reminder = true;
        let manifest = PersonaManifest::parse(
            "[subsystems]\nmemory = false\npersona_reminder = false\nemotion = false\n",
        )
        .unwrap();
        let resolved = manifest.enabled_subsystems(&config);
        assert_eq!(resolved.ids(), ["voice"]);
        assert!(!resolved.is_empty());
    }
}

#[cfg(any(test, feature = "testkit"))]
mod test_support;
