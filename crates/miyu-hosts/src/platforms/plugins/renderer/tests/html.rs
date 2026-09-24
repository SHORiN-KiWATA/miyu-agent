//! Markdown 里夹着的 HTML（用户 09-24）：`<br>` 换行、格式标签转样式、其余只留字。

use crate::platforms::plugins::renderer::*;

fn text(spans: &[RichSpan]) -> String {
    spans.iter().map(|span| span.text.as_str()).collect()
}

/// 截图里满格的 `S 级<br>(天花板)`：表格格子里的 `<br>` 画成换行，各种写法都认，
/// 格子末尾的那个不多出空行。
#[test]
fn br_inside_a_table_cell_becomes_a_line_break() {
    let blocks = collect_blocks(
        "| 梯队 | 说明 |\n| --- | --- |\n| S 级<br>(天花板) | • 术士<br/>• 猎人<BR />• 萨满</br> |\n",
    );
    let table = blocks
        .iter()
        .find_map(|block| block.table.as_ref())
        .unwrap();
    assert_eq!(text(&table.rows[0][0]), "S 级\n(天花板)");
    assert_eq!(text(&table.rows[0][1]), "• 术士\n• 猎人\n• 萨满");
}

#[test]
fn br_in_a_paragraph_does_not_double_the_following_newline() {
    let blocks = collect_blocks("甲<br>\n乙<br><br>丙");
    assert_eq!(text(&blocks[0].spans), "甲\n乙\n\n丙");
}

#[test]
fn common_format_tags_become_styles_and_others_keep_only_their_text() {
    let blocks = collect_blocks(
        "前<b>粗</b><i>斜</i><s>删</s><code>码</code><span style=\"color:red\">留字</span><!-- 注释 -->后",
    );
    let spans = &blocks[0].spans;
    let style = |needle: &str| {
        spans
            .iter()
            .find(|span| span.text.contains(needle))
            .unwrap_or_else(|| panic!("{needle} missing from {spans:?}"))
            .style
    };
    assert!(style("粗").bold);
    assert!(style("斜").italic);
    assert!(style("删").muted);
    assert!(style("码").code);
    assert_eq!(style("留字"), InlineStyle::default());
    assert_eq!(text(spans), "前粗斜删码留字后");
}

#[test]
fn html_in_code_stays_literal_and_unclosed_tags_stop_at_the_block_end() {
    let blocks = collect_blocks("`<br>` 和 <b>没闭合\n\n下一段 a < b");
    assert!(text(&blocks[0].spans).contains("<br>"));
    assert!(blocks[1].spans.iter().all(|span| !span.style.bold));
    assert_eq!(text(&blocks[1].spans), "下一段 a < b");
}

#[test]
fn html_blocks_keep_their_lines_skip_comments_and_decode_entities() {
    let blocks = collect_blocks(
        "<div>\n第一行 &amp; 符号\n<!-- 跨行\n的注释 -->\n第二行&nbsp;x&#33;\n</div>\n",
    );
    let all: String = blocks.iter().map(|block| text(&block.spans)).collect();
    assert_eq!(all, "第一行 & 符号\n第二行\u{a0}x!");
}
