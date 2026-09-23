//! 引导每一屏的落盘。每屏「下一步」即写盘：接模型在最后，用户在那里失败或
//! 退出时，前面填的东西不能白填（`docs/plan/2026-09-12-tui-oobe.md` §4.3）。
//!
//! | 屏 | 写什么 | 写哪 |
//! |---|---|---|
//! | 人格 | 人格提示词 + 激活 | `<prompts_dir>/<名字>.md`、`config.prompt.active_persona` |
//! | 功能 | 启用集 | `<persona_memory_data_dir>/persona.toml` |
//! | 认识你 | 用户自述 | `profile.md`（`config.user_identity_path`） |
//! | 终端 | shell hook | shell rc（**仅当用户选「装上」**） |
//! | 模型 | 供应商 | `config.providers` / `active_provider` / `active_provider_models` |
//! | → | 完成标志 | `config.oobe_done` |

use anyhow::{bail, Context, Result};
use miyu_base::config::feature_catalog::{self, FeatureItem, FeatureSources};
use miyu_base::config::{AppConfig, PersonaManifest, ProviderConfig};
use miyu_base::paths::MiyuPaths;

/// 人格屏的结果。
pub(super) enum PersonaPick {
    Builtin,
    Custom { name: String, prompt: String },
}

/// 名字里不能带路径分隔符，也不能撞上保留名。
pub(super) fn persona_file_name(name: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() {
        bail!("名字不能为空");
    }
    if name.chars().count() > 40 {
        bail!("名字太长了（最多 40 个字符）");
    }
    if name.contains(['/', '\\', '\0']) || name == "." || name == ".." {
        bail!("名字里不能有斜杠");
    }
    let file = format!("{name}.md");
    if file.eq_ignore_ascii_case("system-prompt.md") {
        bail!("这个名字被保留了，换一个");
    }
    Ok(file)
}

/// 写人格文件并激活。返回这个人格的 scope（persona.toml 落在它名下）。
pub(super) fn save_persona(
    config: &mut AppConfig,
    paths: &MiyuPaths,
    pick: &PersonaPick,
) -> Result<String> {
    match pick {
        PersonaPick::Builtin => {
            config.prompt.active_persona = String::new();
        }
        PersonaPick::Custom { name, prompt } => {
            let file = persona_file_name(name)?;
            let dir = config.prompts_dir_path(paths);
            std::fs::create_dir_all(&dir)
                .with_context(|| format!("创建人格目录失败: {}", dir.display()))?;
            let body = if prompt.trim().is_empty() {
                // 设定可以留空：只有名字是这个人格的，其余按默认人格走。
                format!("你是{}。\n", name.trim())
            } else {
                format!("{}\n", prompt.trim_end())
            };
            std::fs::write(dir.join(&file), body)
                .with_context(|| format!("写人格文件失败: {}", dir.join(&file).display()))?;
            config.prompt.active_persona = file;
        }
    }
    config.save(paths).context("保存配置失败")?;
    Ok(config.active_persona_scope())
}

/// 功能屏的勾选写进人格清单；勾上的那些顺手把机器级开关也打开。
///
/// 机器层那一半是 2026-09-20 补的：勾了却因为机器开关关着而不生效，在引导里
/// 一样说不通（设置界面的功能表同一个口径）。默认配置本来就全开，所以这条只
/// 在「用户之前关过、又重跑引导」时才起作用。
pub(super) fn save_features(
    config: &mut AppConfig,
    paths: &MiyuPaths,
    scope: &str,
    items: &[FeatureItem],
    sources: &FeatureSources,
    default_persona: bool,
) -> Result<()> {
    let before = serde_json::to_string(&config.plugins).ok();
    let mcp_before = config.mcp.enabled;
    feature_catalog::apply_machine_switches(config, items);
    let mut manifest = PersonaManifest::load(config, paths, scope);
    feature_catalog::apply_selection(&mut manifest, items, sources, default_persona);
    let path = PersonaManifest::manifest_path(config, paths, scope);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("创建人格数据目录失败: {}", parent.display()))?;
    }
    std::fs::write(&path, manifest.to_toml())
        .with_context(|| format!("写 persona.toml 失败: {}", path.display()))?;
    if serde_json::to_string(&config.plugins).ok() != before || config.mcp.enabled != mcp_before {
        config.save(paths).context("保存配置失败")?;
    }
    Ok(())
}

/// 当前人格清单，给功能屏预填。
pub(super) fn current_manifest(config: &AppConfig, paths: &MiyuPaths) -> PersonaManifest {
    PersonaManifest::load(config, paths, &config.active_persona_scope())
}

/// 用户自述。空的就不写（也不删已有的）。
pub(super) fn save_identity(config: &AppConfig, paths: &MiyuPaths, text: &str) -> Result<()> {
    if text.trim().is_empty() {
        return Ok(());
    }
    let path = config.user_identity_path(paths);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("创建目录失败: {}", parent.display()))?;
    }
    std::fs::write(&path, format!("{}\n", text.trim_end()))
        .with_context(|| format!("写自述失败: {}", path.display()))?;
    Ok(())
}

pub(super) fn current_identity(config: &AppConfig, paths: &MiyuPaths) -> String {
    std::fs::read_to_string(config.user_identity_path(paths))
        .map(|text| text.trim_end().to_string())
        .unwrap_or_default()
}

/// 当前激活的人格名（自定义时是文件名去掉 .md）及其提示词，给人格屏预填。
pub(super) fn current_persona(config: &AppConfig, paths: &MiyuPaths) -> Option<(String, String)> {
    let file = config.prompt.active_persona.trim();
    if file.is_empty() {
        return None;
    }
    let name = file.strip_suffix(".md").unwrap_or(file).to_string();
    let prompt = std::fs::read_to_string(config.persona_path(paths, file))
        .map(|text| text.trim_end().to_string())
        .unwrap_or_default();
    Some((name, prompt))
}

/// 这个 shell 的 hook 装没装。fish 看 conf.d 里的文件；bash/zsh 还要 rc 里有
/// 那段标记块——只有文件没有 source 等于没装。
///
/// 标记块可能在任何一个候选启动文件里(macOS 上 bash 写在 `.bash_profile`),
/// 与安装、卸载用同一张候选表。
pub(super) fn hook_installed(paths: &MiyuPaths, shell: &str) -> bool {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let rc_has = |marker: &str| {
        home.as_ref().is_some_and(|home| {
            miyu_base::shell::startup::candidates(shell, home)
                .iter()
                .filter_map(|file| std::fs::read_to_string(file).ok())
                .any(|text| text.contains(marker))
        })
    };
    match shell {
        "fish" => paths.fish_hook_file.is_file(),
        "bash" => paths.bash_hook_file.is_file() && rc_has("miyu bash hook"),
        "zsh" => paths.zsh_hook_file.is_file() && rc_has("miyu zsh hook"),
        _ => false,
    }
}

/// 装 shell hook。安装函数自己会往 stdout 打几行提示——引导在备用屏上，
/// 打完整屏重画一次就盖掉了。
pub(super) fn install_hook(paths: &MiyuPaths, shell: &str) -> Result<()> {
    match shell {
        "fish" => miyu_base::shell::fish::install(paths),
        "bash" => miyu_base::shell::bash::install(paths),
        "zsh" => miyu_base::shell::zsh::install(paths),
        other => bail!("不认识的 shell: {other}"),
    }
}

pub(super) fn save_provider(
    config: &mut AppConfig,
    paths: &MiyuPaths,
    provider: ProviderConfig,
    model: &str,
) -> Result<()> {
    super::providers::apply(config, paths, provider, model);
    config.save(paths).context("保存配置失败")
}

pub(super) fn mark_done(config: &mut AppConfig, paths: &MiyuPaths) -> Result<()> {
    config.oobe_done = true;
    config.save(paths).context("保存配置失败")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persona_file_names_are_checked() {
        assert_eq!(persona_file_name("Eris").unwrap(), "Eris.md");
        assert!(persona_file_name("").is_err());
        assert!(persona_file_name("a/b").is_err());
        assert!(persona_file_name("system-prompt").is_err());
        // 纯中文名可以用:目录名由 `persona_scope_name` 按名字哈希。
        assert_eq!(persona_file_name("小满").unwrap(), "小满.md");
        assert!(miyu_base::config::persona_scope_name("小满.md").starts_with("persona-"));
    }
}
