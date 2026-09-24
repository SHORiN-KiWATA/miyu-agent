//! mermaid 出位图的字体与版式。终端和 QQ 长图共用字体，长图另有自己的版式；
//! 网页是浏览器画 SVG，不走这里。
//!
//! 字体（用户 09-24）：渲染器主题的字体表里没有中文字体，中文由 resvg 从系统里
//! 随手找一款补上。用户机器上挑中的是等宽编程字体 JetBrains Maple Mono，中文一个
//! 字一个字隔开，换台机器又是另一款。现在打包的 Noto Sans CJK SC 打头，那份字体
//! 文件也加进 resvg 的字体库，不看系统装了什么，也和长图正文是同一套字。
//!
//! 长图版式（用户 09-24）：原先每张图都等比铺满一栏，小图的字被撑得比正文还大，
//! 宽图塞进 960 宽的一栏字只剩 10px。现在按 `IMAGE_TEXT_PX` 定比例，框只当上限；
//! 间距收紧；
//! 思维导图一律用树状排法（根在左、往右长），宽度只随层数长，一栏装得下。

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

/// 打包的中文正文字体。长图正文和图里的字是同一套。
pub(crate) const BUNDLED_CJK_FAMILY: &str = "Noto Sans CJK SC";

pub(crate) const BUNDLED_CJK_FILE: &str = "NotoSansCJK-Regular.ttc";

/// 长图里图上文字的目标字号，正文 36 号的三分之二左右。
pub(crate) const IMAGE_TEXT_PX: f32 = 24.0;

/// 出位图用的主题：打包的中文字体打头，其余照渲染器默认。
pub(crate) fn raster_theme() -> mermaid_rs_renderer::Theme {
    let mut theme = mermaid_rs_renderer::Theme::modern();
    theme.font_family = format!("\"{BUNDLED_CJK_FAMILY}\", {}", theme.font_family);
    theme
}

/// 长图的版式：间距收紧（默认都是 50），思维导图用树状。
pub(crate) fn image_layout() -> mermaid_rs_renderer::LayoutConfig {
    let mut layout = mermaid_rs_renderer::LayoutConfig::default();
    layout.node_spacing = 24.0;
    layout.rank_spacing = 36.0;
    layout.mindmap.layout_algorithm = "lr-tree".to_string();
    layout
}

/// resvg 用的字体库：系统字体加打包的中文字体，每进程只建一次（冷页缓存下扫一遍
/// 系统字体要一秒多，见 `mermaid::tests::timings`）。
pub(crate) fn raster_fonts() -> Arc<resvg::usvg::fontdb::Database> {
    static DB: OnceLock<Arc<resvg::usvg::fontdb::Database>> = OnceLock::new();
    DB.get_or_init(|| {
        let mut db = resvg::usvg::fontdb::Database::new();
        db.load_system_fonts();
        match bundled_cjk_font() {
            Some(path) => {
                if let Err(error) = db.load_font_file(&path) {
                    tracing::warn!(path = %path.display(), %error, "loading the bundled CJK font for diagrams failed");
                }
            }
            None => tracing::info!("bundled CJK font not found; diagrams fall back to system fonts"),
        }
        Arc::new(db)
    })
    .clone()
}

pub(crate) fn bundled_cjk_font() -> Option<PathBuf> {
    miyu_base::paths::resources::candidates(miyu_base::paths::resources::ResourceKind::Fonts)
        .into_iter()
        .map(|dir| dir.join(BUNDLED_CJK_FILE))
        .find(|path| path.is_file())
}
