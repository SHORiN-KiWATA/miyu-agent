//! mermaid 出位图的字体与长图版式（用户 09-24）。

use crate::render::diagram_style::*;
use crate::render::mermaid::{recolour_backdrop, render_png_in_box, render_svg_styled, SvgStyle};

const MINDMAP: &str = "mindmap
  root((魔兽世界：无限))
    中立新种族
      天裔 Skyborne
        联盟高阶阵营: 法力回涌/法师解禁
        部落御风者: 移速增益/萨满解禁
    全新职业生态
      混合职业救赎
        防骑原生嘲讽与群拉
        恢复萨实装激流与图腾优化
      经典重制
        狂暴战怒气循环优化
        痛苦术Dot解除限制
    环境划分
      PVE 团本与5人本: 重视减伤链与辅助协同
      PVP 野外与战场: 节奏更快、重视强解与爆发
";

const PAPER: [u8; 4] = [0xF4, 0xEF, 0xE5, 0xFF];

/// SVG 的原始宽高（根元素写的是 `width="100%"`，真尺寸在 viewBox 里）。
fn svg_size(svg: &str) -> (f32, f32) {
    let start = svg.find("viewBox=\"").unwrap() + "viewBox=\"".len();
    let values: Vec<f32> = svg[start..start + svg[start..].find('"').unwrap()]
        .split_whitespace()
        .map(|value| value.parse().unwrap())
        .collect();
    (values[2], values[3])
}

fn png_size(png: &[u8]) -> (u32, u32) {
    let image = image::load_from_memory(png).unwrap();
    (image.width(), image.height())
}

/// 用户机器上中文被补成了等宽编程字体、一字一隔。出位图的两条路都换成打包的
/// 中文字体打头；网页交给浏览器，保持渲染器原样。
#[test]
fn raster_diagrams_lead_with_the_bundled_cjk_font() {
    let source = "graph TD\n    A[敏锐贼] --> B[冰法]";
    assert!(!render_svg_styled(source, SvgStyle::Web)
        .unwrap()
        .contains(BUNDLED_CJK_FAMILY));
    for style in [SvgStyle::Terminal, SvgStyle::Image] {
        assert!(render_svg_styled(source, style)
            .unwrap()
            .contains(BUNDLED_CJK_FAMILY));
    }
}

/// 字体库里要有打包的那份文件本身：系统里恰好也装了同名字体时，只看字族名就
/// 分不出来。
#[test]
fn the_bundled_cjk_font_file_is_in_the_raster_font_database() {
    let bundled = bundled_cjk_font().expect("assets/fonts ships the CJK font");
    let db = raster_fonts();
    assert!(db.faces().any(|face| matches!(
        &face.source,
        resvg::usvg::fontdb::Source::File(path) | resvg::usvg::fontdb::Source::SharedFile(path, _)
            if path == &bundled
    )));
}

/// 截图里的思维导图：放射状铺开 1300 宽，塞进一栏字只剩 10px。长图改用树状，
/// 宽高比小得多，一栏放得下还能把字放大。
#[test]
fn image_mindmaps_use_the_narrow_tree_layout() {
    let (web_w, web_h) = svg_size(&render_svg_styled(MINDMAP, SvgStyle::Web).unwrap());
    let (image_w, image_h) = svg_size(&render_svg_styled(MINDMAP, SvgStyle::Image).unwrap());
    assert!(
        image_w / image_h * 2.0 < web_w / web_h,
        "tree {image_w}x{image_h} vs radial {web_w}x{web_h}"
    );
}

/// 小图按目标字号放大（矢量放大不糊），宽图照样收进一栏。
#[test]
fn image_diagrams_scale_text_up_to_a_readable_size() {
    let small = "graph TD\n    A[开始] --> B[结束]";
    let (natural_w, _) = svg_size(&render_svg_styled(small, SvgStyle::Image).unwrap());
    let (width, _) = png_size(&render_png_in_box(small, 960, 2200, PAPER).unwrap());
    let expected = natural_w * IMAGE_TEXT_PX / raster_theme().font_size;
    assert!(
        (width as f32 - expected).abs() <= 2.0,
        "{width} should be the natural {natural_w} scaled to {IMAGE_TEXT_PX}px text"
    );

    let mut wide = String::from("graph LR\n");
    for index in 0..14 {
        wide.push_str(&format!(
            "    N{index}[第 {index} 步：做一件事] --> N{}[下一步]\n",
            index + 1
        ));
    }
    let (width, _) = png_size(&render_png_in_box(&wide, 960, 2200, PAPER).unwrap());
    assert_eq!(width, 960);
}

/// 导图那块底色矩形带着浮点零头（`x="0.000027656555"`），以前认不出，长图里的
/// 导图一直是纸面上一块白板（用户 09-24 截图）。偏得明显的矩形照旧不动。
#[test]
fn a_backdrop_with_float_noise_at_the_origin_takes_the_page_colour() {
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100%" viewBox="0 0 10 10"><rect x="0.000027656555" y="-0.000002861023" width="10" height="10" fill="#FFFFFF"/></svg>"##;
    assert!(recolour_backdrop(svg, "#F4EFE5").contains("fill=\"#F4EFE5\""));
    let shifted = svg.replace("x=\"0.000027656555\"", "x=\"5\"");
    assert_eq!(recolour_backdrop(&shifted, "#F4EFE5"), shifted);

    let mindmap = render_svg_styled(MINDMAP, SvgStyle::Image).unwrap();
    assert_eq!(
        recolour_backdrop(&mindmap, "#F4EFE5")
            .matches("#F4EFE5")
            .count(),
        1
    );
}
