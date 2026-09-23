use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Locale {
    En,
    Zh,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UiLanguage {
    Auto,
    En,
    Zh,
}

impl UiLanguage {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "en" => Some(Self::En),
            "zh" => Some(Self::Zh),
            _ => None,
        }
    }

    fn locale(self) -> Option<Locale> {
        match self {
            Self::Auto => None,
            Self::En => Some(Locale::En),
            Self::Zh => Some(Locale::Zh),
        }
    }
}

impl Locale {
    fn from_env_value(value: &str) -> Option<Self> {
        let value = value.trim().to_ascii_lowercase();
        if value.is_empty() || value == "auto" || value == "c" || value == "posix" {
            return None;
        }
        if value.starts_with("zh") {
            Some(Self::Zh)
        } else {
            Some(Self::En)
        }
    }

    fn resolve_with<F>(configured: &str, mut env: F) -> Self
    where
        F: FnMut(&str) -> Option<String>,
    {
        if let Some(value) = env("MIYU_LANG") {
            if let Some(language) = UiLanguage::parse(&value) {
                return language
                    .locale()
                    .unwrap_or_else(|| Self::system_with(&mut env));
            }
            if let Some(locale) = Self::from_env_value(&value) {
                return locale;
            }
        }
        if let Some(locale) = UiLanguage::parse(configured).and_then(UiLanguage::locale) {
            return locale;
        }
        Self::system_with(&mut env)
    }

    fn system_with<F>(env: &mut F) -> Self
    where
        F: FnMut(&str) -> Option<String>,
    {
        for key in ["LC_ALL", "LC_MESSAGES", "LANG"] {
            if let Some(locale) = env(key).and_then(|value| Self::from_env_value(&value)) {
                return locale;
            }
        }
        Self::En
    }
}

static UI_LOCALE: OnceLock<Locale> = OnceLock::new();

// WebUI 请求级语言(2026-09-23 用户拍板):一个 daemon 会被多台设备的浏览器
// 访问,语言各不同,进程级 `UI_LOCALE` 只够 CLI/TUI 用。web 中间件在请求入口
// scope 一次,请求内所有 `text`/`is_zh` 跟随它;没有 scope 的调用(CLI、平台
// 后台任务)回退全局值。
tokio::task_local! {
    static REQUEST_LOCALE: Locale;
}

/// 在 `locale` 作用域内执行 future,离开即恢复。请求级语言的唯一入口。
pub async fn scoped<F: std::future::Future>(locale: Locale, future: F) -> F::Output {
    REQUEST_LOCALE.scope(locale, future).await
}

/// 当前是否在请求级语言作用域内(不在则返回 None)。
pub fn request_locale() -> Option<Locale> {
    REQUEST_LOCALE.try_with(|locale| *locale).ok()
}

pub fn init(configured: &str) -> Locale {
    *UI_LOCALE.get_or_init(|| Locale::resolve_with(configured, |key| std::env::var(key).ok()))
}

pub fn locale() -> Locale {
    request_locale().unwrap_or_else(global_locale)
}

fn global_locale() -> Locale {
    UI_LOCALE
        .get()
        .copied()
        .unwrap_or_else(|| Locale::resolve_with("auto", |key| std::env::var(key).ok()))
}

pub fn is_zh() -> bool {
    locale() == Locale::Zh
}

pub fn text(en: &'static str, zh: &'static str) -> &'static str {
    text_for(locale(), en, zh)
}

pub fn text_for(locale: Locale, en: &'static str, zh: &'static str) -> &'static str {
    match locale {
        Locale::En => en,
        Locale::Zh => zh,
    }
}

// 模型可见面恒为英文，与系统 locale 无关：英文思维链质量更高、更贴近工具
// 调用的训练分布，token 也不更贵。曾经的 `agent_text(en, zh)` 双语开关已
// 整体移除——模型面文案直接写英文字面量，不留死掉的中文参数；UI 文案
// （`text`/`localize`）不受影响，仍跟随 locale。

#[cfg(test)]
mod tests {
    use super::*;

    /// UI 面按 locale 切换；模型面没有开关可测——双语形态已整体移除。
    #[test]
    fn ui_text_follows_locale() {
        assert_eq!(text_for(Locale::Zh, "english", "中文"), "中文");
        assert_eq!(text_for(Locale::En, "english", "中文"), "english");
    }

    #[test]
    fn detects_chinese_locale_values() {
        assert_eq!(Locale::from_env_value("zh_CN.UTF-8"), Some(Locale::Zh));
        assert_eq!(Locale::from_env_value("zh_TW"), Some(Locale::Zh));
    }

    #[test]
    fn detects_english_locale_values() {
        assert_eq!(Locale::from_env_value("en_US.UTF-8"), Some(Locale::En));
        assert_eq!(Locale::from_env_value("ja_JP.UTF-8"), Some(Locale::En));
        assert_eq!(Locale::from_env_value("C"), None);
    }

    #[test]
    fn parses_ui_language_values() {
        assert_eq!(UiLanguage::parse("auto"), Some(UiLanguage::Auto));
        assert_eq!(UiLanguage::parse("en"), Some(UiLanguage::En));
        assert_eq!(UiLanguage::parse("zh"), Some(UiLanguage::Zh));
        assert_eq!(UiLanguage::parse(""), None);
        assert_eq!(UiLanguage::parse("fr"), None);
    }

    #[test]
    fn explicit_locale_text_does_not_depend_on_global_state() {
        assert_eq!(text_for(Locale::En, "English", "中文"), "English");
        assert_eq!(text_for(Locale::Zh, "English", "中文"), "中文");
    }

    #[test]
    fn resolves_environment_then_config_then_system_locale() {
        let locale = Locale::resolve_with("zh", |key| match key {
            "MIYU_LANG" => Some("en_US.UTF-8".to_string()),
            "LANG" => Some("zh_CN.UTF-8".to_string()),
            _ => None,
        });
        assert_eq!(locale, Locale::En);

        let locale = Locale::resolve_with("zh", |key| match key {
            "LANG" => Some("en_US.UTF-8".to_string()),
            _ => None,
        });
        assert_eq!(locale, Locale::Zh);

        let locale = Locale::resolve_with("auto", |key| match key {
            "LANG" => Some("zh_CN.UTF-8".to_string()),
            _ => None,
        });
        assert_eq!(locale, Locale::Zh);

        let locale = Locale::resolve_with("zh", |key| match key {
            "MIYU_LANG" => Some("auto".to_string()),
            "LANG" => Some("en_US.UTF-8".to_string()),
            _ => None,
        });
        assert_eq!(locale, Locale::En);
    }

    #[test]
    fn system_locale_detection_ignores_ui_override() {
        let mut env = |key: &str| match key {
            "MIYU_LANG" => Some("en".to_string()),
            "LANG" => Some("zh_CN.UTF-8".to_string()),
            _ => None,
        };
        assert_eq!(Locale::system_with(&mut env), Locale::Zh);
    }

    #[tokio::test]
    async fn scoped_locale_overrides_global_and_unwinds() {
        // 全局值随测试机的 env 变,不能假设,只能相对断言。
        let global = locale();
        let other = if global == Locale::Zh {
            Locale::En
        } else {
            Locale::Zh
        };
        assert_eq!(request_locale(), None);
        scoped(other, async {
            assert_eq!(request_locale(), Some(other));
            assert_eq!(text("english", "中文"), text_for(other, "english", "中文"));
            // 嵌套:内层盖外层,出来逐层恢复
            scoped(global, async {
                assert_eq!(locale(), global);
            })
            .await;
            assert_eq!(locale(), other);
        })
        .await;
        assert_eq!(locale(), global);
        assert_eq!(request_locale(), None);
    }
}
