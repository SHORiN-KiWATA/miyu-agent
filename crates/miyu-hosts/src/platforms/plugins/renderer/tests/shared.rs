//! 渲染测试共用的 fixture。

use crate::platforms::plugins::renderer::*;

pub(super) fn render(markdown: &str, raw_config: &RenderConfig) -> Result<Vec<RenderedImage>> {
    render_in_process_for_test(markdown, raw_config)
}

/// 断栏规则用固定栏高来测，不受成图尺寸的选择影响（以前 `max_height: 1000`
/// 时的栏高）。
pub(super) const SHORT_COLUMN: u32 = 1000 - 64 * 2;

/// 按默认配置把 Markdown 排成块。
pub(super) fn layouts_for(markdown: &str) -> Vec<LayoutBlock> {
    let config = NormalizedConfig::new(&RenderConfig::default());
    let mut renderer = RendererState::new().unwrap();
    let fonts = renderer.resolve_config_fonts(&config, false).unwrap();
    layout_blocks(
        &mut renderer.font_system,
        collect_blocks(markdown),
        &config,
        Palette::for_theme("paper"),
        &fonts,
    )
    .unwrap()
}

pub(super) fn code_markdown(lines: usize) -> String {
    let mut markdown = String::from("```text\n");
    for line in 0..lines {
        markdown.push_str(&format!("line {line:02}: rendered column content\n"));
    }
    markdown.push_str("```\n");
    markdown
}
