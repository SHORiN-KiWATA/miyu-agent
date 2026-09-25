//! 目录缓存按配置裁剪:这个进程要过哪些模型的元数据、装载的那份够不够用。
//!
//! models.dev 的整份目录解析出来很占内存,守护进程只留配置里用得到的模型
//! (08-21 低占用专项)。裁剪以前只看「装载那一刻的配置」,而且装过一次就不再
//! 重读:运行中在设置页新加的模型,上下文窗口、思考档位、价格全查不到,直到
//! 重启(B12,09-24)。现在按「这个进程至今要过的全部模型」裁剪,选集只增不减。

use crate::models_cache::*;

/// 要留元数据的模型。按「供应商 → 模型」记一份,再记一份只认模型名的——自定义
/// 供应商对不上 models.dev 的键时,靠模型名从别家借元数据。
#[derive(Debug, Clone, Default)]
pub(in crate::models_cache) struct Selection {
    by_provider: HashMap<String, HashSet<String>>,
    model_ids: HashSet<String>,
}

impl Selection {
    /// 一份配置用得到的全部模型:各供应商默认模型、主对话池、QQ 各池与会话
    /// 路由、真实上下文插件的两个池。
    pub(in crate::models_cache) fn of(config: &crate::config::AppConfig) -> Self {
        let mut selection = Self::default();
        for provider in &config.providers {
            selection
                .by_provider
                .entry(provider.id.clone())
                .or_default()
                .insert(provider.default_model.clone());
            if !provider.default_model.trim().is_empty() {
                selection.model_ids.insert(provider.default_model.clone());
            }
        }
        let conversation_models = config.platforms.qq.conversations.iter().flat_map(|route| {
            route
                .text_models
                .iter()
                .flatten()
                .chain(route.multimodal_models.iter().flatten())
        });
        let real_context_models: Vec<crate::config::ActiveProviderModelConfig> = config
            .platforms
            .qq
            .plugins
            .get(crate::config::REAL_CONTEXT_PLUGIN_ID)
            .and_then(|instance| {
                crate::config::RealContextPluginSettings::from_instance(instance).ok()
            })
            .map(|settings| {
                settings
                    .text_models
                    .explicit_entries()
                    .iter()
                    .chain(settings.affection_text_models.explicit_entries())
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        for choice in config
            .active_provider_models
            .iter()
            .flatten()
            .chain(config.active_multimodal_provider_models.iter().flatten())
            .chain(config.platforms.qq.text_models.explicit_entries())
            .chain(config.platforms.qq.multimodal_models.explicit_entries())
            .chain(
                config
                    .platforms
                    .qq
                    .non_whitelist_text_models
                    .explicit_entries(),
            )
            .chain(conversation_models)
            .chain(real_context_models.iter())
        {
            selection
                .by_provider
                .entry(choice.provider_id.clone())
                .or_default()
                .insert(choice.model.clone());
            selection.model_ids.insert(choice.model.clone());
        }
        selection
    }

    /// `other` 要的每个模型这里都要了。
    fn covers(&self, other: &Selection) -> bool {
        other.model_ids.is_subset(&self.model_ids)
            && other.by_provider.iter().all(|(provider, models)| {
                self.by_provider
                    .get(provider)
                    .is_some_and(|mine| models.is_subset(mine))
            })
    }

    fn merge(&mut self, other: Selection) {
        for (provider, models) in other.by_provider {
            self.by_provider.entry(provider).or_default().extend(models);
        }
        self.model_ids.extend(other.model_ids);
    }

    pub(in crate::models_cache) fn retain(
        &self,
        data: &mut HashMap<String, HashMap<String, ModelInfo>>,
    ) {
        data.retain(|provider_id, models| {
            let provider_models = self.by_provider.get(provider_id);
            models.retain(|model_id, _| {
                provider_models.is_some_and(|ids| ids.contains(model_id))
                    || self.model_ids.contains(model_id)
            });
            !models.is_empty()
        });
    }
}

/// 进程里的那份目录,连同「至今要过哪些模型」。三样放在同一把锁下:判断够不够
/// 用和装上新的一份必须看到同一个选集,否则晚到的刷新会按旧选集把新模型裁掉。
#[derive(Default)]
pub(in crate::models_cache) struct Catalogue {
    pub(in crate::models_cache) loaded: Option<Cache>,
    /// 装载的那份按哪个选集裁过;None 是整份目录(或者还没装载)。
    pruned_to: Option<Selection>,
    /// 这个进程至今要过的全部模型,只增不减。多账户各自的配置都算进来,
    /// 所以轮流用不同配置时不会来回重读。
    wanted: Selection,
}

impl Catalogue {
    /// 记下这份配置要的模型;装载的那份不够用(还没装载,或者是按更小的选集裁
    /// 的)就返回 true,调用方去盘上重读一份。整份目录什么都有,不用重读。
    pub(in crate::models_cache) fn want(&mut self, config: &crate::config::AppConfig) -> bool {
        self.wanted.merge(Selection::of(config));
        match (&self.loaded, &self.pruned_to) {
            (None, _) => true,
            (Some(_), None) => false,
            (Some(_), Some(pruned_to)) => !pruned_to.covers(&self.wanted),
        }
    }

    /// 装上一份新解析的目录,按至今要过的全部模型裁剪(包括 `config` 要的)。
    pub(in crate::models_cache) fn install_pruned(
        &mut self,
        mut fresh: Cache,
        config: &crate::config::AppConfig,
    ) {
        self.wanted.merge(Selection::of(config));
        self.wanted.retain(&mut fresh.data);
        self.pruned_to = Some(self.wanted.clone());
        self.loaded = Some(fresh);
    }

    /// 装上整份目录:CLI、配置界面要列全部模型,`AppConfig::load` 校验前也装整份。
    pub(in crate::models_cache) fn install_full(&mut self, fresh: Cache) {
        self.pruned_to = None;
        self.loaded = Some(fresh);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(window: u64) -> ModelInfo {
        ModelInfo {
            input_modalities: Vec::new(),
            context_window: Some(window),
            reasoning: None,
            cost: None,
        }
    }

    /// 目录里 p 家有 a、b 两个模型;配置按给的顺序把它们放进主对话池。
    const CATALOGUE: &str = r#"{"p":{"models":{
        "a":{"limit":{"context":111000}},
        "b":{"limit":{"context":222000}}
    }}}"#;

    fn catalogue() -> Cache {
        parse_api_response(CATALOGUE).unwrap()
    }

    fn pool(models: &[&str]) -> crate::config::AppConfig {
        let mut config = crate::config::AppConfig::default();
        config.active_provider_models = Some(
            models
                .iter()
                .map(|model| crate::config::ActiveProviderModelConfig {
                    provider_id: "p".to_string(),
                    model: model.to_string(),
                })
                .collect(),
        );
        config
    }

    fn window(catalogue: &Catalogue, model: &str) -> Option<u64> {
        lookup_context_window(&catalogue.loaded.as_ref()?.data, "p", model)
    }

    /// B12(09-24):设置页运行中新加的模型,上下文窗口、思考档位、价格全查不到,
    /// 直到重启——目录只在还没装载时读一次,装载后按当时的配置裁掉了它。
    #[test]
    fn a_model_added_while_running_makes_the_catalogue_reload() {
        let mut state = Catalogue::default();
        assert!(state.want(&pool(&["a"])), "nothing is loaded yet");
        state.install_pruned(catalogue(), &pool(&["a"]));
        assert_eq!(window(&state, "a"), Some(111_000));
        assert_eq!(window(&state, "b"), None, "b is not configured yet");

        assert!(
            state.want(&pool(&["a", "b"])),
            "the loaded catalogue was pruned before b was added"
        );
        state.install_pruned(catalogue(), &pool(&["a", "b"]));
        assert_eq!(window(&state, "b"), Some(222_000));
        assert!(
            !state.want(&pool(&["a", "b"])),
            "once covered, the same configuration must not reload again"
        );
    }

    /// 启动时按当时配置发出去的那次联网刷新可能晚到:它装上的新目录不能把期间
    /// 新加的模型又裁掉。多账户各自的配置也一样——谁要过的都留着,不来回重读。
    #[test]
    fn a_late_refresh_keeps_models_wanted_since_it_started() {
        let mut state = Catalogue::default();
        state.install_pruned(catalogue(), &pool(&["a"]));
        state.want(&pool(&["b"]));
        state.install_pruned(catalogue(), &pool(&["b"]));

        state.install_pruned(catalogue(), &pool(&["a"]));

        assert_eq!(window(&state, "a"), Some(111_000));
        assert_eq!(window(&state, "b"), Some(222_000));
        assert!(!state.want(&pool(&["a"])));
        assert!(!state.want(&pool(&["b"])));
    }

    /// 整份目录什么都有:CLI 和配置加载装的就是整份,不必为哪份配置重读。
    #[test]
    fn a_full_catalogue_covers_any_configuration() {
        let mut state = Catalogue::default();
        state.install_full(catalogue());
        assert!(!state.want(&pool(&["a", "b", "not-in-the-catalogue"])));
        assert_eq!(window(&state, "b"), Some(222_000));
    }

    #[test]
    fn compact_cache_retains_only_configured_models() {
        let config = crate::config::AppConfig::default();
        let provider = &config.providers[0];
        let mut data = HashMap::from([
            (
                provider.id.clone(),
                HashMap::from([
                    (provider.default_model.clone(), model(128_000)),
                    ("unused-model".to_string(), model(64_000)),
                ]),
            ),
            (
                "unused-provider".to_string(),
                HashMap::from([("unused-model".to_string(), model(32_000))]),
            ),
        ]);

        Selection::of(&config).retain(&mut data);

        assert!(!data.contains_key("unused-provider"));
        assert!(data[&provider.id].contains_key(&provider.default_model));
        assert!(!data[&provider.id].contains_key("unused-model"));
    }

    #[test]
    fn compact_cache_retains_models_used_only_by_platform_routes() {
        let mut config = crate::config::AppConfig::default();
        let provider_id = config.providers[0].id.clone();
        config.providers[0].models.extend([
            "route-text".to_string(),
            "route-vision".to_string(),
            "platform-text".to_string(),
            "non-whitelist-text".to_string(),
            "context-text".to_string(),
        ]);
        config.platforms.qq.text_models =
            crate::config::ModelPoolRef::models(vec![crate::config::ActiveProviderModelConfig {
                provider_id: provider_id.clone(),
                model: "platform-text".to_string(),
            }]);
        config.platforms.qq.non_whitelist_text_models =
            crate::config::ModelPoolRef::models(vec![crate::config::ActiveProviderModelConfig {
                provider_id: provider_id.clone(),
                model: "non-whitelist-text".to_string(),
            }]);
        let mut real_context = crate::config::PlatformPluginInstanceConfig::default();
        crate::config::merge_real_context_settings(
            &mut real_context,
            &crate::config::RealContextPluginSettings {
                text_models: crate::config::ModelPoolRef::models(vec![
                    crate::config::ActiveProviderModelConfig {
                        provider_id: provider_id.clone(),
                        model: "context-text".to_string(),
                    },
                ]),
                ..Default::default()
            },
        );
        config.platforms.qq.plugins.insert(
            crate::config::REAL_CONTEXT_PLUGIN_ID.to_string(),
            real_context,
        );
        config
            .platforms
            .qq
            .conversations
            .push(crate::config::PlatformModelRoute {
                conversation: crate::config::PlatformConversationConfig {
                    kind: crate::config::PlatformConversationKind::Group,
                    id: "20000".to_string(),
                },
                persona: crate::config::PlatformPersonaOverride::Inherit,
                text_models_inheritance: crate::config::PlatformModelPoolInheritance::Platform,
                text_models: Some(vec![crate::config::ActiveProviderModelConfig {
                    provider_id: provider_id.clone(),
                    model: "route-text".to_string(),
                }]),
                multimodal_models_inheritance:
                    crate::config::PlatformModelPoolInheritance::Platform,
                multimodal_models: Some(vec![crate::config::ActiveProviderModelConfig {
                    provider_id: provider_id.clone(),
                    model: "route-vision".to_string(),
                }]),
                extra_prompt: String::new(),
                session_limits: None,
                probability_reply: None,
                probability_reply_rate: None,
                ignore_sleep_hours: None,
                rate_limit: None,
            });
        let mut data = HashMap::from([(
            provider_id.clone(),
            HashMap::from([
                (config.providers[0].default_model.clone(), model(128_000)),
                ("route-text".to_string(), model(64_000)),
                ("route-vision".to_string(), model(96_000)),
                ("platform-text".to_string(), model(64_000)),
                ("non-whitelist-text".to_string(), model(64_000)),
                ("context-text".to_string(), model(64_000)),
                ("unused-model".to_string(), model(32_000)),
            ]),
        )]);

        Selection::of(&config).retain(&mut data);

        let retained = &data[&provider_id];
        assert!(retained.contains_key("route-text"));
        assert!(retained.contains_key("route-vision"));
        assert!(retained.contains_key("platform-text"));
        assert!(retained.contains_key("non-whitelist-text"));
        assert!(retained.contains_key("context-text"));
        assert!(!retained.contains_key("unused-model"));
    }

    #[test]
    fn compact_cache_retains_same_model_metadata_from_other_providers() {
        let mut config = crate::config::AppConfig::default();
        let provider = &mut config.providers[0];
        provider.models = vec!["custom-model".to_string()];
        provider.default_model = "custom-model".to_string();
        let mut data = HashMap::from([
            (
                provider.id.clone(),
                HashMap::from([("custom-model".to_string(), model(64_000))]),
            ),
            (
                "catalog-provider".to_string(),
                HashMap::from([("custom-model".to_string(), model(128_000))]),
            ),
        ]);

        Selection::of(&config).retain(&mut data);

        assert!(data.contains_key("catalog-provider"));
        assert_eq!(
            lookup_context_window(&data, "custom-provider", "custom-model"),
            Some(64_000)
        );
    }
}
