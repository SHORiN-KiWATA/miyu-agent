//! WebUI 请求级界面语言(2026-09-23 用户拍板:界面语言跟 config 的
//! `display.language`,`auto` = 浏览器语言)。
//!
//! 服务端是语言的唯一权威:根页面加载的 `/i18n.js` 由这里注入
//! `window.MIYU_LANG`,API 请求由中间件 scope 进 `miyu_base::i18n` 的
//! task-local —— 前端不自己猜语言,CLI/TUI 的进程级语言不受影响。
use crate::web::*;
use axum::extract::{Request, State};
use axum::http::header::ACCEPT_LANGUAGE;
use axum::middleware::Next;
use miyu_base::config::AppConfig;
use miyu_base::i18n::{Locale, UiLanguage};

/// 解析一次请求的界面语言:config 明确 en/zh 时最优先,`auto` 看
/// `Accept-Language`(浏览器会自动带上,顺序即优先级)。
pub(in crate::web) fn resolve(config: &AppConfig, headers: &HeaderMap) -> Locale {
    match UiLanguage::parse(&config.display.language) {
        Some(UiLanguage::En) => Locale::En,
        Some(UiLanguage::Zh) => Locale::Zh,
        _ => from_accept_language(headers),
    }
}

/// `Accept-Language` 的第一门具体语言定音:zh* 中文,其余英文。浏览器语言是
/// `zh-Hans-CN` 这种长标签,只认主语言前缀;没有头(脚本/curl)按英文。
fn from_accept_language(headers: &HeaderMap) -> Locale {
    let Some(raw) = headers
        .get(ACCEPT_LANGUAGE)
        .and_then(|value| value.to_str().ok())
    else {
        return Locale::En;
    };
    for item in raw.split(',') {
        let tag = item
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        if tag.is_empty() || tag == "*" {
            continue;
        }
        return if tag.starts_with("zh") {
            Locale::Zh
        } else {
            Locale::En
        };
    }
    Locale::En
}

/// axum 中间件:解析请求语言并 scope 进 i18n,让该请求内所有
/// `i18n::text`/`is_zh`(含 handler 调用的其它 crate)按浏览器语言输出。
pub(in crate::web) async fn middleware(
    State(state): State<DaemonState>,
    request: Request,
    next: Next,
) -> Response {
    let locale = {
        let manager = state.manager.lock().unwrap();
        resolve(&manager.config, request.headers())
    };
    miyu_base::i18n::scoped(locale, next.run(request)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers_with(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT_LANGUAGE, HeaderValue::from_str(value).unwrap());
        headers
    }

    #[test]
    fn accept_language_picks_first_concrete_tag() {
        assert_eq!(
            from_accept_language(&headers_with("zh-CN,zh;q=0.9,en;q=0.8")),
            Locale::Zh
        );
        assert_eq!(
            from_accept_language(&headers_with("en-US,en;q=0.9")),
            Locale::En
        );
        // 第一门具体语言不是中文就按英文,后面的 zh 只是备选
        assert_eq!(
            from_accept_language(&headers_with("ja-JP,zh;q=0.5")),
            Locale::En
        );
        assert_eq!(from_accept_language(&headers_with("*")), Locale::En);
        assert_eq!(from_accept_language(&HeaderMap::new()), Locale::En);
    }

    #[test]
    fn configured_language_beats_browser() {
        let mut config = AppConfig::default();
        config.display.language = "en".to_string();
        assert_eq!(
            resolve(&config, &headers_with("zh-CN,zh;q=0.9")),
            Locale::En
        );
        config.display.language = "zh".to_string();
        assert_eq!(
            resolve(&config, &headers_with("en-US,en;q=0.9")),
            Locale::Zh
        );
        config.display.language = "auto".to_string();
        assert_eq!(
            resolve(&config, &headers_with("zh-CN,zh;q=0.9")),
            Locale::Zh
        );
        assert_eq!(
            resolve(&config, &headers_with("en-US,en;q=0.9")),
            Locale::En
        );
    }
}
