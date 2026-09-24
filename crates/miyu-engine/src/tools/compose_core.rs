use super::*;

pub fn register(
    registry: &mut ToolRegistry,
    config: &AppConfig,
    paths: &MiyuPaths,
    manifest: &PersonaManifest,
    subsystems: &miyu_base::config::EnabledSubsystems,
) {
    let plugin = |id: &str| manifest.plugin_enabled(id);
    // ── core ──
    if plugin("files") {
        // 读检索(read/glob/grep)、trash_path 一整套。
        default_tools::register(
            registry,
            config.skills.allow_command_execution,
            config,
            paths,
        );
    } else {
        // 只挂 run_command:coreutils 干得更好的都不注册(dev 验收三轮裁剪)。
        default_tools::register_run_command(registry, config.skills.allow_command_execution);
    }
    jobs::register_management(registry);
    // 编辑器只留 apply_patch(聚合增/改/删,diff 渲染载体)。
    apply_patch::register(registry);
    todowrite::register(registry, config.clone(), paths.clone());
    goal::register(registry, config.clone(), paths.clone());
    web::register_fetch(registry);
    if subsystems.voice {
        if config.voice.enabled {
            voice_chat::register(registry);
        }
        if config.voice.tts.is_active() {
            voice_speak::register(registry);
        }
    }
    if config.plugins.web.enabled {
        web::register(registry, config.plugins.web.clone());
    }
    if config.plugins.vision.enabled {
        // 看图对 coding 也是刚需(UI 截图排错、设计稿、测试产出的图表);
        // 聊天模型不带眼睛时由 vision 插件路由给专用视觉模型。
        vision::register(registry, config.clone(), paths.clone(), true);
    }
    // 记忆整套按 persona 清单构造:关着就一件工具都不注册(联想注入、日记、
    // 前言在 agent 侧同样按清单裁决)。
    if subsystems.memory {
        memory::register(registry, config.clone(), paths.clone());
    }
    // 内置插件按登记表挂(config::BUILTIN_PLUGINS × tools::builtin_plugins):
    // 人格清单 × 机器开关;发给模型的定义按名排序,注册先后不影响字节。
    builtin_plugins::register_enabled(registry, config, paths, manifest, false);
}
