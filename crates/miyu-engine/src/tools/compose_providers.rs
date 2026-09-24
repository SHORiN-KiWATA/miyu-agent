use super::{mcp, scripts, skills, AppConfig, MiyuPaths, PersonaManifest, ToolRegistry};

/// 外装件的接入机制:脚本、MCP、技能(09-24 技能从子系统并进插件,与前两者同级)。
/// 挂的先后就是工具面的先后,技能仍排最后——挪位置会改掉工具面字节。
pub fn register(
    registry: &mut ToolRegistry,
    config: &AppConfig,
    paths: &MiyuPaths,
    manifest: &PersonaManifest,
    external: bool,
) {
    let plugin = |id: &str| manifest.plugin_enabled(id);
    if plugin("scripts") {
        if external {
            scripts::register_external(registry, config, paths);
        } else {
            scripts::register(registry, config, paths);
        }
    }
    if plugin("mcp") && config.mcp.enabled {
        mcp::register(registry, config.clone(), manifest.plugins.mcp.as_deref());
    }
    if manifest.skills_enabled(config) {
        if let Err(error) = skills::register_skills(registry, config, paths) {
            tracing::warn!(error = %error, "failed to register skills");
        }
        skills::register_authoring(registry, config.clone(), paths.clone());
    }
}
