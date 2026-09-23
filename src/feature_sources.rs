//! 功能表的数据源：扫脚本、列技能、列 MCP 服务器、探语音。
//!
//! 引导（OOBE）与设置界面的「启用的功能」是同一张表（[`miyu_base::config::
//! feature_catalog`]），这里是它的外装件那一半——配置层不扫目录也不探二进制，
//! 谁调谁给。2026-09-20 从引导里抽出来给设置界面复用。

use miyu_base::config::feature_catalog::FeatureSources;
use miyu_base::config::AppConfig;
use miyu_base::paths::MiyuPaths;

/// PATH 里有没有这个可执行文件。
pub fn on_path(binary: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|dir| {
            let candidate = dir.join(binary);
            candidate.is_file() || candidate.is_symlink()
        })
    })
}

/// `miyu-voice` 也可能和主程序放在一起而不在 PATH 里。
pub fn voice_available() -> bool {
    on_path("miyu-voice")
        || miyu_base::paths::miyu_executable()
            .ok()
            .and_then(|exe| exe.parent().map(|dir| dir.join("miyu-voice").is_file()))
            .unwrap_or(false)
}

/// 功能表里的 MCP 一格：机器级 `mcp.enabled` 开着才列，只列 `servers[].enabled`
/// 的；说明行是命令本身，显示名空的由目录退回 id。
pub fn mcp_server_options(config: &AppConfig) -> Vec<(String, String, String)> {
    if !config.mcp.enabled {
        return Vec::new();
    }
    config
        .mcp
        .servers
        .iter()
        .filter(|server| server.enabled && !server.id.trim().is_empty())
        .map(|server| {
            let mut hint = server.command.clone();
            for arg in &server.args {
                hint.push(' ');
                hint.push_str(arg);
            }
            (server.id.clone(), server.display_name.clone(), hint)
        })
        .collect()
}

/// 把这台机器上装着什么收齐。
pub fn collect(config: &AppConfig, paths: &MiyuPaths) -> FeatureSources {
    // 内置脚本对每个人格都列出来：默认人格默认全勾，自定义人格默认不勾、勾了才挂。
    // 技能带路的脚本(住在 `skills/<技能名>/scripts/`)也收进来，但不单独成行——
    // 它的开关是那份技能那一行，`apply_selection` 按带路技能连动脚本白名单。
    FeatureSources {
        voice_available: voice_available(),
        persona_reminder_available: config.prompt.persona_reminder,
        emotion_available: config.platforms.qq.enabled,
        scripts: miyu_engine::tools::list_scripts_for_features(paths),
        skills: miyu_core::skills::persona_skill_options(config, paths),
        mcp_servers: mcp_server_options(config),
    }
}
