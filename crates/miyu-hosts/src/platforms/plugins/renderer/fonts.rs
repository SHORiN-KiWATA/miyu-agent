//! 字体的发现与加载。
//!
//! 中文和 emoji 各要一份回退字体，缺了就是满屏豆腐块。`MAX_CUSTOM_FONT_FILES`
//! 限制用户目录里扫多少个文件——字体目录可能很大，全加载会把内存吃光。

use crate::platforms::plugins::renderer::*;

pub(in crate::platforms::plugins::renderer) const MAX_CACHED_GLYPHS: usize = 2048;

pub(in crate::platforms::plugins::renderer) const MAX_CUSTOM_FONT_FILES: usize = 8;

// 正文和图里的字是同一套打包字体，名字只写在 `diagram_style` 一处。
pub(in crate::platforms::plugins::renderer) const DEFAULT_BODY_FONT: &str =
    crate::render::diagram_style::BUNDLED_CJK_FAMILY;

// 代码字体:打包的 JetBrains Mono(拉丁等宽,OFL);CJK 字形经 FontSystem
// 回退落到 NotoSansCJK。此前指向 "Noto Sans Mono CJK SC"——该字体从未被
// 加载进 fontdb,resolve 静默回落 Sans,代码块整体非等宽(用户 08-20 点名)。
pub(in crate::platforms::plugins::renderer) const DEFAULT_CODE_FONT: &str = "JetBrains Mono";

pub(in crate::platforms::plugins::renderer) const DEFAULT_EMOJI_FONT: &str = "Noto Color Emoji";

pub(in crate::platforms::plugins::renderer) const CJK_FONT_FILE: &str =
    crate::render::diagram_style::BUNDLED_CJK_FILE;

pub(in crate::platforms::plugins::renderer) const CODE_FONT_FILE: &str =
    "JetBrainsMono-Regular.ttf";

pub(in crate::platforms::plugins::renderer) const EMOJI_FONT_FILE: &str = "NotoColorEmoji.ttf";

pub(in crate::platforms::plugins::renderer) const RENDERER_FONTS_ENV: &str =
    "MIYU_RENDERER_FONTS_DIR";

pub(in crate::platforms::plugins::renderer) fn renderer_fonts_dir() -> Result<PathBuf> {
    let candidates =
        miyu_base::paths::resources::candidates(miyu_base::paths::resources::ResourceKind::Fonts);
    renderer_fonts_dir_from(&candidates)
}

fn renderer_fonts_dir_from(candidates: &[PathBuf]) -> Result<PathBuf> {
    for candidate in candidates {
        if candidate.join(CJK_FONT_FILE).is_file() {
            return Ok(candidate.clone());
        }
    }
    let searched = candidates
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    bail!(
        "renderer font is missing; install {CJK_FONT_FILE} in /usr/share/miyu/fonts or set {RENDERER_FONTS_ENV} (searched: {searched})"
    )
}

#[derive(Clone)]
pub(in crate::platforms::plugins::renderer) struct NormalizedConfig {
    pub(in crate::platforms::plugins::renderer) theme: String,
    pub(in crate::platforms::plugins::renderer) font_size: u32,
    pub(in crate::platforms::plugins::renderer) code_font_size: u32,
    pub(in crate::platforms::plugins::renderer) padding: u32,
    pub(in crate::platforms::plugins::renderer) font: String,
    pub(in crate::platforms::plugins::renderer) title_font: String,
    pub(in crate::platforms::plugins::renderer) code_font: String,
    pub(in crate::platforms::plugins::renderer) emoji_font: String,
}

impl NormalizedConfig {
    pub fn new(config: &RenderConfig) -> Self {
        Self {
            theme: config.theme.trim().to_ascii_lowercase(),
            font_size: config.font_size.clamp(14, 56),
            code_font_size: config.code_font_size.clamp(12, 52),
            padding: config.padding.clamp(24, 160),
            font: config.font.clone(),
            title_font: config.title_font.clone(),
            code_font: config.code_font.clone(),
            emoji_font: config.emoji_font.clone(),
        }
    }
}

#[derive(Clone)]
pub(in crate::platforms::plugins::renderer) struct ResolvedFonts {
    pub(in crate::platforms::plugins::renderer) body: Option<String>,
    pub(in crate::platforms::plugins::renderer) title: Option<String>,
    pub(in crate::platforms::plugins::renderer) code: Option<String>,
    pub(in crate::platforms::plugins::renderer) emoji: Option<String>,
}

#[cfg(test)]
mod distribution_resources {
    use super::*;

    #[test]
    fn font_override_missing_file_falls_back_and_absence_is_an_error() {
        let temp = tempfile::tempdir().unwrap();
        let absent = temp.path().join("override");
        let installed = temp.path().join("installed");
        std::fs::create_dir_all(&installed).unwrap();
        assert!(renderer_fonts_dir_from(&[absent.clone(), installed.clone()]).is_err());
        std::fs::write(installed.join(CJK_FONT_FILE), b"font fixture").unwrap();
        assert_eq!(
            renderer_fonts_dir_from(&[absent.clone(), installed.clone()]).unwrap(),
            installed
        );
        std::fs::create_dir_all(&absent).unwrap();
        std::fs::write(absent.join(CJK_FONT_FILE), b"override fixture").unwrap();
        assert_eq!(
            renderer_fonts_dir_from(&[absent.clone(), installed]).unwrap(),
            absent
        );
    }
}
