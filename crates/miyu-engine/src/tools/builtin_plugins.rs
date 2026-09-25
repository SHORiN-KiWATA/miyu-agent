//! 内置插件的注册函数表:与 `config::BUILTIN_PLUGINS` 按 id 对齐。
//!
//! 描述符(id、中文名、可勾选、机器开关)住在 config,注册函数只能住在这里
//! (config 是底座,不能反向依赖 tools)。`register_enabled` 按人格清单 × 机器开关
//! 把开着的插件挂上;`after_subagent_snapshot` 的那些在子代理快照之后再挂——子代理
//! 面不带它们(记账:注册位置就是权限边界)。
//!
//! 新增一个内置插件:实现文件 + 描述 JSON + `tool_descriptions.rs` 的 `include_str!`
//! + config 表一行 + 这里一行;少了这里那一行,`registrars_cover_every_builtin` 报红。

use super::*;
use miyu_base::config::{PluginKind, BUILTIN_PLUGINS};

type Registrar = fn(&mut ToolRegistry, &AppConfig, &MiyuPaths);

struct BuiltinRegistrar {
    id: &'static str,
    register: Registrar,
    /// 在子代理快照之后注册:子代理工具面不带它。
    after_subagent_snapshot: bool,
}

fn reg_usage_query(registry: &mut ToolRegistry, config: &AppConfig, paths: &MiyuPaths) {
    usage_query::register(registry, paths.state_dir.clone(), config.clone());
}

fn reg_alarm(registry: &mut ToolRegistry, _config: &AppConfig, paths: &MiyuPaths) {
    alarm::register(registry, paths.clone());
}

fn reg_exchange_rate(registry: &mut ToolRegistry, config: &AppConfig, _paths: &MiyuPaths) {
    exchange_rate::register(registry, config.plugins.exchange_rate.clone());
}

fn reg_archlinux(registry: &mut ToolRegistry, _config: &AppConfig, paths: &MiyuPaths) {
    archlinux::register(registry, paths);
}

fn reg_print_image(registry: &mut ToolRegistry, config: &AppConfig, _paths: &MiyuPaths) {
    vision::register_print(registry, config.clone());
}

fn reg_memes(registry: &mut ToolRegistry, config: &AppConfig, paths: &MiyuPaths) {
    memes::register(registry, config.clone(), paths.clone());
}

/// 本地会话专属:平台会话有 send_message_to_user。配置里开了就恒在,不看连没连上——
/// 掉线在调用时拦,工具表不随连接变(09-25,见 `platform_outreach` 模块头)。
fn reg_platform_outreach(registry: &mut ToolRegistry, config: &AppConfig, _paths: &MiyuPaths) {
    platform_outreach::register(registry, config);
}

fn reg_web_images(registry: &mut ToolRegistry, config: &AppConfig, paths: &MiyuPaths) {
    web_images::register(registry, config.clone(), paths.clone(), true);
}

fn reg_image_generation(registry: &mut ToolRegistry, config: &AppConfig, paths: &MiyuPaths) {
    image_generation::register(registry, config.clone(), paths.clone());
}

fn reg_knowledge_base(registry: &mut ToolRegistry, config: &AppConfig, paths: &MiyuPaths) {
    knowledge_base::register(registry, config.clone(), paths.clone());
}

fn reg_ledger(registry: &mut ToolRegistry, config: &AppConfig, paths: &MiyuPaths) {
    ledger::register(registry, config.clone(), paths.clone());
}

const REGISTRARS: &[BuiltinRegistrar] = &[
    BuiltinRegistrar {
        id: "usage_query",
        register: reg_usage_query,
        after_subagent_snapshot: false,
    },
    BuiltinRegistrar {
        id: "alarm",
        register: reg_alarm,
        after_subagent_snapshot: false,
    },
    BuiltinRegistrar {
        id: "exchange_rate",
        register: reg_exchange_rate,
        after_subagent_snapshot: false,
    },
    BuiltinRegistrar {
        id: "archlinux",
        register: reg_archlinux,
        after_subagent_snapshot: false,
    },
    BuiltinRegistrar {
        id: "print_image",
        register: reg_print_image,
        after_subagent_snapshot: false,
    },
    BuiltinRegistrar {
        id: "memes",
        register: reg_memes,
        after_subagent_snapshot: false,
    },
    BuiltinRegistrar {
        id: "platform_outreach",
        register: reg_platform_outreach,
        after_subagent_snapshot: false,
    },
    BuiltinRegistrar {
        id: "web_images",
        register: reg_web_images,
        after_subagent_snapshot: false,
    },
    BuiltinRegistrar {
        id: "image_generation",
        register: reg_image_generation,
        after_subagent_snapshot: false,
    },
    BuiltinRegistrar {
        id: "knowledge_base",
        register: reg_knowledge_base,
        after_subagent_snapshot: false,
    },
    // 记账:注册位置就是权限边界——子代理快照之后才挂,不可信场所连工具名都不存在。
    BuiltinRegistrar {
        id: "ledger",
        register: reg_ledger,
        after_subagent_snapshot: true,
    },
];

/// 按人格清单 × 机器开关挂上开着的内置插件。`after_subagent_snapshot` 选挂哪一批。
pub(super) fn register_enabled(
    registry: &mut ToolRegistry,
    config: &AppConfig,
    paths: &MiyuPaths,
    manifest: &PersonaManifest,
    after_subagent_snapshot: bool,
) {
    for plugin in BUILTIN_PLUGINS
        .iter()
        .filter(|plugin| plugin.kind == PluginKind::Builtin)
    {
        // 关掉的插件一件不注册:关着仍常驻一份完整契约,是三个面都白背的纯浪费
        // (08-17 实测 get_exchange_rate 311 字符)。
        if !manifest.plugin_enabled(plugin.id) || !(plugin.installed)(config) {
            continue;
        }
        let Some(registrar) = REGISTRARS.iter().find(|entry| entry.id == plugin.id) else {
            // `registrars_cover_every_builtin` 保证到不了这里。
            continue;
        };
        if registrar.after_subagent_snapshot == after_subagent_snapshot {
            (registrar.register)(registry, config, paths);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// config 表里每个 Builtin 都有注册函数,注册函数表里没有多余的 id。
    #[test]
    fn registrars_cover_every_builtin() {
        let builtin: Vec<&str> = BUILTIN_PLUGINS
            .iter()
            .filter(|plugin| plugin.kind == PluginKind::Builtin)
            .map(|plugin| plugin.id)
            .collect();
        let registrars: Vec<&str> = REGISTRARS.iter().map(|entry| entry.id).collect();
        assert_eq!(
            builtin, registrars,
            "config::BUILTIN_PLUGINS 与 tools::builtin_plugins::REGISTRARS 必须按 id 逐一对齐"
        );
    }
}
