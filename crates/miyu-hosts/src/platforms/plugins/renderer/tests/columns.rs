//! 断栏（用户 09-24）：能拆就拆进这一栏剩下的地方，两截都得像样，标题跟着下文。

use super::shared::*;
use crate::platforms::plugins::renderer::*;

fn lines_in(block: &LayoutBlock, placement: &Placement) -> usize {
    block
        .boundaries
        .iter()
        .filter(|boundary| {
            **boundary > placement.source_start && **boundary <= placement.source_end
        })
        .count()
}

/// 用户 09-24 的截图：第一栏只排了标题和一段说明，下面那张装得进一整栏的大表
/// 整张挪到了下一栏，第一栏空了七成。现在表格先拆进这一栏剩下的地方。
#[test]
fn a_table_that_fits_a_column_starts_in_the_space_left() {
    let mut markdown = String::from("# 标题\n\n一段说明文字。\n\n| 梯队 | 说明 |\n| --- | --- |\n");
    for row in 0..14 {
        markdown.push_str(&format!("| T{row} | 第 {row} 行的说明 |\n"));
    }
    let layouts = layouts_for(&markdown);
    let table_index = layouts
        .iter()
        .position(|block| block.table.is_some())
        .unwrap();
    // 栏高：整张表装得下一栏，接在标题和说明后面就装不下。
    let usable = layouts[table_index].total_height + 60;
    let columns = plan_columns_with_height(&layouts, usable).unwrap();
    let first = &columns[0];
    assert!(
        first
            .placements
            .iter()
            .any(|placement| placement.block_index == table_index),
        "the table should begin in the first column"
    );
    assert!(
        first.used_height * 10 >= usable * 8,
        "first column left {} of {usable} empty",
        usable - first.used_height
    );
}

/// 标题落在栏底、正文在下一栏的那种（截图里「1. 种族梯度总览」）：标题连同下文
/// 开头放不下就一起换栏。
#[test]
fn a_heading_moves_with_the_start_of_its_section() {
    let layouts = layouts_for(
        "第一段第一行\n第一段第二行\n第一段第三行\n第一段第四行\n\n## 小标题\n\n第二段第一行\n第二段第二行\n第二段第三行\n",
    );
    assert!(matches!(layouts[1].kind, BlockKind::Heading(_)));
    let (first, heading, next) = (&layouts[0], &layouts[1], &layouts[2]);
    // 栏高：标题连同下文第一行放得下，第二行放不下。
    let usable = first.total_height
        + first.margin_after
        + heading.margin_before
        + heading.total_height
        + heading.margin_after
        + next.margin_before
        + next.boundaries[0];
    assert!(
        next.total_height <= usable,
        "test premise: the section fits a column"
    );
    let columns = plan_columns_with_height(&layouts, usable).unwrap();
    for column in &columns {
        let last = column.placements.last().unwrap();
        assert!(
            !matches!(layouts[last.block_index].kind, BlockKind::Heading(_)),
            "a heading was left at the bottom of a column"
        );
    }
    let with_heading = columns
        .iter()
        .find(|column| {
            column
                .placements
                .iter()
                .any(|placement| placement.block_index == 1)
        })
        .unwrap();
    assert!(with_heading
        .placements
        .iter()
        .any(|placement| placement.block_index == 2 && placement.source_start == 0));
}

/// 拆开的文字块两截各至少两行：栏底只剩一行地方时整段换栏，不挂一行在那儿。
#[test]
fn split_blocks_keep_at_least_two_lines_on_each_side() {
    let mut markdown = String::from("开头一行\n开头二行\n开头三行\n\n");
    for line in 0..30 {
        markdown.push_str(&format!("长段落第 {line} 行\n"));
    }
    let layouts = layouts_for(&markdown);
    let (first, long) = (&layouts[0], &layouts[1]);
    // 栏高：第一段之后刚好只容得下长段落的一行；长段落比一栏还高，必须拆。
    let usable =
        first.total_height + first.margin_after + long.margin_before + long.boundaries[0] + 2;
    assert!(
        long.total_height > usable,
        "test premise: the long paragraph must split"
    );
    let columns = plan_columns_with_height(&layouts, usable).unwrap();
    let fragments: Vec<&Placement> = columns
        .iter()
        .flat_map(|column| &column.placements)
        .filter(|placement| placement.block_index == 1)
        .collect();
    assert!(fragments.len() > 1);
    for fragment in fragments {
        let lines = lines_in(long, fragment);
        // 写死 2 而不是引用常量：常量被改小时这条要能报红。
        assert!(
            lines >= 2,
            "a fragment of {lines} line(s) at {}..{}",
            fragment.source_start,
            fragment.source_end
        );
    }
}
