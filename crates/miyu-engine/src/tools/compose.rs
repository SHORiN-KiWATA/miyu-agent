use super::*;

/// 一条流水线装出任何场所、任何 persona 的工具面(09-10 分层架构阶段 4,取代
/// builtin_registry / dev_registry / restricted_platform_registry 三张各写一遍):
///
/// 1. **core**:今天的 dev 那套——命令与后台任务、补丁编辑、todo、goal、web
///    抓取/搜索、看图、MCP、subagent 子代理、load_tools。不看 persona。
/// 2. **扩展**:按 persona 清单启用。子系统(记忆、技能、语音)与插件(其余
///    注册单元,id 见 config::PLUGIN_IDS)各自受 config 的机器级开关约束——
///    persona 只能在「本机装了的」里挑。
/// 3. **场所**:External 只留 `trust == External` 的工具,再做平台专属的描述
///    修饰;`interactive_questions` 决定给不给 ask_question。
///
/// 注册顺序沿用旧表(subagent 的快照点、cross_hints 收尾都在原位),定义按名排序,
/// 所以三个面的 tools 数组与合并前逐字节相同(`shape_tests` 钉着)。
pub fn compose_registry(
    config: &AppConfig,
    paths: &MiyuPaths,
    manifest: &PersonaManifest,
    surface: Surface,
) -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.set_default_timeout_secs(config.tools.default_timeout_secs);
    install_builtin_guards(&mut registry, config);
    let external = surface.trust == SurfaceTrust::External;
    // 子系统按「人格意愿 × 机器配置」的快照裁决(config::subsystems 是唯一真相源)。
    let subsystems = manifest.enabled_subsystems(config);

    compose_core::register(&mut registry, config, paths, manifest, &subsystems);
    // 子代理拿的是这一刻的快照,指路句也得按它自己的工具面补。
    let mut subagent_tools = registry.clone();
    cross_hints::apply(&mut subagent_tools);
    subagent::register(&mut registry, config.clone(), paths.clone(), subagent_tools);
    // 子代理快照之后才挂的内置插件(记账:注册位置就是权限边界)。
    builtin_plugins::register_enabled(&mut registry, config, paths, manifest, true);
    // 跨会话消息(09-23)同理:子代理的回话对象是父回合,不给它往别的会话发话。
    cross_session::register(&mut registry);
    compose_providers::register(&mut registry, config, paths, manifest, external);
    if surface.interactive_questions {
        ask_question::register(&mut registry);
    }
    // load_tools 常驻注册(09-01):full 模式下调用它无害(返回契约文本),
    // 而会话中途从需加载模型切到完整模型时,历史里的 load_tools 调用记录
    // 必须仍然可执行,否则模型模仿历史会撞未知工具。
    load_tools::register(&mut registry);

    apply_surface_policy(&mut registry, surface);
    registry
}

/// Apply surface exposure after provider registration.
fn apply_surface_policy(registry: &mut ToolRegistry, surface: Surface) {
    if surface.trust == SurfaceTrust::External {
        registry.retain_trust(ToolTrust::External);
        if registry.contains("generate_image") {
            // Static English keeps platform tool descriptions byte-stable.
            registry.amend_description(
                "generate_image",
                " In messaging-platform conversations at most one image is generated per user request; the limit is enforced automatically.",
            );
        }
    }
    cross_hints::apply(registry);
}
