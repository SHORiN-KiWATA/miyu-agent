//! 配置测试共用的 fixture。

use crate::config::*;

pub(super) fn route_test_config() -> AppConfig {
    let mut config = AppConfig::default();
    // 按「第一个非内置 CLI 的供应商」取,不按下标:内置 CLI 中转恒占列表最前
    // (09-20 起还按字母序排),下标 0 是禁用的 CLI 条目,而且它们看图走原生
    // 文件工具、消息里不内联,拿来当视觉模型会被 validate 拦下。
    let provider = config
        .providers
        .iter_mut()
        .find(|provider| !provider.is_builtin_cli_provider())
        .expect("默认模板里应当有普通 HTTP 供应商");
    provider.models = vec!["text-only".to_string(), "vision".to_string()];
    provider.default_model = "text-only".to_string();
    provider
        .model_modalities
        .insert("text-only".to_string(), vec!["text".to_string()]);
    provider.model_modalities.insert(
        "vision".to_string(),
        vec!["text".to_string(), "image".to_string()],
    );
    config
}

/// `route_test_config` 配好 text-only/vision 的那条供应商的 id。
///
/// 别再写 `config.providers[0].id`:内置 CLI 中转恒占列表最前(09-20 起还按
/// 字母序排),下标 0 是禁用的 CLI 条目,不是这里要的那条。
pub(super) fn route_test_provider_id(config: &AppConfig) -> String {
    config
        .providers
        .iter()
        .find(|provider| !provider.is_builtin_cli_provider())
        .expect("默认模板里应当有普通 HTTP 供应商")
        .id
        .clone()
}

pub(super) fn test_route(config: &AppConfig) -> PlatformModelRoute {
    PlatformModelRoute {
        conversation: PlatformConversationConfig {
            kind: PlatformConversationKind::Group,
            id: "20002".to_string(),
        },
        persona: PlatformPersonaOverride::Inherit,
        text_models_inheritance: PlatformModelPoolInheritance::Platform,
        text_models: Some(vec![ActiveProviderModelConfig {
            provider_id: route_test_provider_id(config),
            model: "text-only".to_string(),
        }]),
        multimodal_models_inheritance: PlatformModelPoolInheritance::Platform,
        multimodal_models: Some(vec![ActiveProviderModelConfig {
            provider_id: route_test_provider_id(config),
            model: "vision".to_string(),
        }]),
        extra_prompt: "Reply naturally in this group.".to_string(),
        session_limits: None,
        probability_reply: None,
        probability_reply_rate: None,
        ignore_sleep_hours: None,
        rate_limit: None,
    }
}
