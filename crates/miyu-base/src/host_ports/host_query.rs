//! 宿主只读查询(09-16 接口治理):扩展凭令牌问宿主几件事,拿到的是脱敏 DTO,
//! 绝不返回 `AppConfig`、api key、base_url、登录路径。方法、能力与错误码是契约,
//! 记在 `docs/interfaces/host-capabilities.md`;加方法先加文档。
//!
//! | 方法 | 需要的能力 | 返回 |
//! |---|---|---|
//! | `host.info` | `host.info` | 版本、支持的契约版本、本次授权集、当前人格 |
//! | `providers.list` | `providers.read` | 供应商摘要列表 + 当前激活选择 |
//! | `providers.get` | `providers.read` | 一家供应商的摘要 + 模型上下文窗口 / 模态 |
//! | `subsystems.enabled` | `subsystems.read` | 当前人格的子系统启用快照 |
//!
//! 错误码:`permission_denied` / `unknown_method` / `invalid_argument` / `not_found`。
//! 以 daemon 当前配置为准(与脚本所在回合同一份全局配置)。

use crate::config::{AppConfig, PersonaManifest, ProviderConfig, SUPPORTED_CONTRACTS};
use crate::paths::MiyuPaths;
use serde_json::{json, Value};
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostQueryError {
    pub code: &'static str,
    pub message: String,
}

impl HostQueryError {
    fn denied(capability: &str) -> Self {
        Self {
            code: "permission_denied",
            message: format!("this token does not carry the {capability} capability"),
        }
    }
}

fn require(granted: &[String], capability: &str) -> Result<(), HostQueryError> {
    if granted.iter().any(|item| item == capability) {
        Ok(())
    } else {
        Err(HostQueryError::denied(capability))
    }
}

/// 当前激活的 (provider_id, model) 选择:池模式列全部,否则激活供应商的默认模型。
fn active_selection(config: &AppConfig) -> Vec<(String, String)> {
    if let Some(pool) = &config.active_provider_models {
        if !pool.is_empty() {
            return pool
                .iter()
                .map(|item| (item.provider_id.clone(), item.model.clone()))
                .collect();
        }
    }
    config
        .provider(None)
        .ok()
        .map(|provider| vec![(provider.id.clone(), provider.default_model.clone())])
        .unwrap_or_default()
}

/// 脱敏摘要:没有 api_key、base_url、超时、额外请求体。
fn provider_summary(provider: &ProviderConfig, active_ids: &BTreeSet<&str>) -> Value {
    let models: BTreeSet<&str> = provider
        .models
        .iter()
        .chain(provider.custom_models.iter())
        .map(String::as_str)
        .filter(|model| !model.trim().is_empty())
        .collect();
    json!({
        "id": provider.id,
        "display_name": provider.display_name,
        "protocol": provider.protocol,
        "enabled": provider.enabled,
        "builtin_cli": provider.is_builtin_cli_provider(),
        "default_model": provider.default_model,
        "models": models,
        "active": active_ids.contains(provider.id.as_str()),
    })
}

pub fn answer_host_query(
    granted: &[String],
    config: &AppConfig,
    paths: &MiyuPaths,
    method: &str,
    params: &Value,
) -> Result<Value, HostQueryError> {
    match method {
        "host.info" => {
            require(granted, "host.info")?;
            Ok(json!({
                "version": env!("CARGO_PKG_VERSION"),
                "contracts": SUPPORTED_CONTRACTS
                    .iter()
                    .map(|(id, version)| json!({ "id": id, "version": version }))
                    .collect::<Vec<_>>(),
                "capabilities": granted,
                "persona": config.active_persona_scope(),
            }))
        }
        "providers.list" => {
            require(granted, "providers.read")?;
            let selection = active_selection(config);
            let active_ids: BTreeSet<&str> =
                selection.iter().map(|(id, _)| id.as_str()).collect();
            Ok(json!({
                "providers": config
                    .providers
                    .iter()
                    .map(|provider| provider_summary(provider, &active_ids))
                    .collect::<Vec<_>>(),
                "active": selection
                    .iter()
                    .map(|(provider_id, model)| json!({ "provider_id": provider_id, "model": model }))
                    .collect::<Vec<_>>(),
            }))
        }
        "providers.get" => {
            require(granted, "providers.read")?;
            let id = params
                .get("provider_id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .ok_or_else(|| HostQueryError {
                    code: "invalid_argument",
                    message: "params.provider_id is required".to_string(),
                })?;
            let provider = config
                .providers
                .iter()
                .find(|provider| provider.id == id)
                .ok_or_else(|| HostQueryError {
                    code: "not_found",
                    message: format!("no provider with id {id:?}"),
                })?;
            let selection = active_selection(config);
            let active_ids: BTreeSet<&str> =
                selection.iter().map(|(id, _)| id.as_str()).collect();
            let mut summary = provider_summary(provider, &active_ids);
            summary["context_windows"] = json!(provider.model_context_window);
            summary["modalities"] = json!(provider.model_modalities);
            Ok(summary)
        }
        "subsystems.enabled" => {
            require(granted, "subsystems.read")?;
            let scope = config.active_persona_scope();
            let manifest = PersonaManifest::load(config, paths, &scope);
            let enabled = manifest.enabled_subsystems(config);
            Ok(json!({
                "persona": scope,
                "memory": enabled.memory,
                // 技能 09-24 从子系统转成插件:键留着(老消费方),值按插件闸算,
                // 与迁移前 `subsystems.skills × skills.enabled` 逐字等价。
                "skills": manifest.skills_enabled(config),
                "persona_reminder": enabled.persona_reminder,
                "voice": enabled.voice,
                "emotion": enabled.emotion,
            }))
        }
        other => Err(HostQueryError {
            code: "unknown_method",
            message: format!(
                "unknown host method {other:?}; known: host.info, providers.list, providers.get, subsystems.enabled"
            ),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(root: &std::path::Path) -> MiyuPaths {
        MiyuPaths {
            root_dir: root.to_path_buf(),
            config_dir: root.join("config"),
            config_file: root.join("config/config.jsonc"),
            skills_dir: root.join("config/skills"),
            data_dir: root.join("data"),
            cache_dir: root.join("cache"),
            state_dir: root.join("state"),
            pictures_dir: root.join("pictures"),
            fish_hook_file: root.join("fish"),
            bash_hook_file: root.join("bash"),
            zsh_hook_file: root.join("zsh"),
            scripts_dir: root.join("scripts"),
            system_scripts_dir: root.join("system-scripts"),
        }
    }

    fn all() -> Vec<String> {
        super::super::HOST_CAPABILITIES
            .iter()
            .map(|id| id.to_string())
            .collect()
    }

    /// 摘要里没有密钥、地址一类字段;激活位跟当前选择走。
    #[test]
    fn provider_summaries_carry_no_credentials() {
        let temp = tempfile::tempdir().unwrap();
        let mut config = AppConfig::default();
        for provider in &mut config.providers {
            provider.api_key = Some("sk-secret".to_string());
        }
        let data = answer_host_query(
            &all(),
            &config,
            &paths(temp.path()),
            "providers.list",
            &json!({}),
        )
        .unwrap();
        let text = data.to_string();
        assert!(!text.contains("sk-secret"), "{text}");
        assert!(
            !text.contains("api_key") && !text.contains("base_url"),
            "{text}"
        );
        let providers = data["providers"].as_array().unwrap();
        assert!(!providers.is_empty());
        let active_id = config.provider(None).unwrap().id.clone();
        let active = providers
            .iter()
            .find(|provider| provider["id"] == active_id)
            .unwrap();
        assert_eq!(active["active"], true);
        assert_eq!(data["active"][0]["provider_id"], active_id);

        let one = answer_host_query(
            &all(),
            &config,
            &paths(temp.path()),
            "providers.get",
            &json!({ "provider_id": active_id }),
        )
        .unwrap();
        assert!(one.get("context_windows").is_some() && one.get("modalities").is_some());
        assert!(!one.to_string().contains("sk-secret"));
    }

    #[test]
    fn missing_capability_unknown_method_and_bad_params_are_named_errors() {
        let temp = tempfile::tempdir().unwrap();
        let config = AppConfig::default();
        let paths = paths(temp.path());
        let only_info = vec!["host.info".to_string()];
        assert_eq!(
            answer_host_query(&only_info, &config, &paths, "providers.list", &json!({}))
                .unwrap_err()
                .code,
            "permission_denied"
        );
        assert_eq!(
            answer_host_query(&all(), &config, &paths, "providers.get", &json!({}))
                .unwrap_err()
                .code,
            "invalid_argument"
        );
        assert_eq!(
            answer_host_query(
                &all(),
                &config,
                &paths,
                "providers.get",
                &json!({ "provider_id": "nope" })
            )
            .unwrap_err()
            .code,
            "not_found"
        );
        assert_eq!(
            answer_host_query(&all(), &config, &paths, "banana", &json!({}))
                .unwrap_err()
                .code,
            "unknown_method"
        );
        let info = answer_host_query(&only_info, &config, &paths, "host.info", &json!({})).unwrap();
        assert_eq!(info["capabilities"], json!(["host.info"]));
        assert_eq!(info["contracts"][0]["id"], "scripts");
        let subsystems =
            answer_host_query(&all(), &config, &paths, "subsystems.enabled", &json!({})).unwrap();
        assert_eq!(subsystems["memory"], config.memory_config().enabled);
    }
}
