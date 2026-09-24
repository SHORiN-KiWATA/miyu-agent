//! 排好版的块 → 分栏与成图尺寸。
//!
//! 两件事：一栏里在哪儿断（`plan_columns_with_height`），一共几栏、每栏多高
//! （`plan_balanced_columns`）。
//!
//! 断栏（用户 09-24）：放不下的块先拆进这一栏剩下的地方，再续到下一栏。以前能
//! 装进一整栏的块一律整块挪走，前一栏常空出大半截。拆开的两截都得像样：文字各
//! 至少 `MIN_SPLIT_LINES` 行，表格至少一行正文（续栏重复表头）；标题跟下文的开头
//! 待在同一栏。
//!
//! 尺寸（用户 09-24）：长宽都跟着内容量走。从放得下的最少栏数往上试，挑成图最
//! 接近 `TARGET_ASPECT_RATIO` 的一种；高度只受单页上限（`MAX_PAGE_HEIGHT`、
//! `MAX_PAGE_PIXELS`）约束，不再有可配的最大高度。以前高度被配置卡在 2600，
//! 长文只能一栏栏往横里加，成了又宽又扁的长条。

use crate::platforms::plugins::renderer::*;

/// 目标是接近方图（09-24 从 4:3 改来）：短回复一栏往下长，大约长到宽的两倍才分栏；
/// 4:3 会让四五百字的回复就摊成又矮又宽的两栏。方图在手机竖屏和电脑横屏上都不吃亏。
pub(in crate::platforms::plugins::renderer) const TARGET_ASPECT_RATIO: f32 = 1.0;

/// 比例几乎一样时偏向更少的栏。
pub(in crate::platforms::plugins::renderer) const ASPECT_TIE_EPSILON: f32 = 0.01;

/// 拆开的文字块每一截至少这么多行：再少就是一两行孤零零挂在栏底或栏顶。
pub(in crate::platforms::plugins::renderer) const MIN_SPLIT_LINES: usize = 2;

/// 从最少栏数往上最多再试这么多种。栏越多成图越宽越矮，过了目标比例就停，
/// 正常到不了这个数，只给退化输入兜底。
const MAX_EXTRA_COLUMNS: usize = 16;

#[derive(Default)]
pub(in crate::platforms::plugins::renderer) struct ColumnPlan {
    pub(in crate::platforms::plugins::renderer) placements: Vec<Placement>,
    pub(in crate::platforms::plugins::renderer) used_height: u32,
}

pub(in crate::platforms::plugins::renderer) struct Placement {
    pub(in crate::platforms::plugins::renderer) block_index: usize,
    pub(in crate::platforms::plugins::renderer) source_start: u32,
    pub(in crate::platforms::plugins::renderer) source_end: u32,
    pub(in crate::platforms::plugins::renderer) y: u32,
}

/// 按固定栏高从左到右排。
pub(in crate::platforms::plugins::renderer) fn plan_columns_with_height(
    layouts: &[LayoutBlock],
    usable_height: u32,
) -> Result<Vec<ColumnPlan>> {
    if usable_height < 128 {
        bail!("page height leaves too little room for rendered content");
    }
    let mut columns = vec![ColumnPlan::default()];
    for (block_index, block) in layouts.iter().enumerate() {
        check_table_fits(block, usable_height)?;
        if matches!(block.kind, BlockKind::Heading(_)) {
            keep_heading_with_next(&mut columns, layouts, block_index, usable_height)?;
        }
        place_block(&mut columns, block_index, block, usable_height)?;
    }
    Ok(columns)
}

fn check_table_fits(block: &LayoutBlock, usable_height: u32) -> Result<()> {
    let Some(table) = block.table.as_ref() else {
        return Ok(());
    };
    if table.header_height > usable_height {
        bail!("a Markdown table header exceeds the usable image height");
    }
    for row in table.rows.iter().filter(|row| !row.header) {
        let row_height = row.source_end.saturating_sub(row.source_start);
        if table.header_height.saturating_add(row_height) > usable_height {
            bail!("a Markdown table row exceeds the usable image height");
        }
    }
    Ok(())
}

/// 标题落在栏底、正文却在下一栏，读的人得跨栏去找。这一栏放不下「标题（连同紧跟
/// 着的下级标题）+ 下文开头」就整组换栏；整组比一栏还高就不管了。
fn keep_heading_with_next(
    columns: &mut Vec<ColumnPlan>,
    layouts: &[LayoutBlock],
    heading_index: usize,
    usable_height: u32,
) -> Result<()> {
    let used = active(columns)?.used_height;
    if used == 0 {
        return Ok(());
    }
    let mut needed = 0_u32;
    for block in &layouts[heading_index..] {
        needed = needed.saturating_add(block.margin_before);
        if matches!(block.kind, BlockKind::Heading(_)) {
            needed = needed
                .saturating_add(block.total_height)
                .saturating_add(block.margin_after);
            continue;
        }
        needed = needed.saturating_add(min_first_fragment(block));
        break;
    }
    if needed > usable_height.saturating_sub(used) && needed <= usable_height {
        push_column(columns)?;
    }
    Ok(())
}

/// 这块开头在一栏里至少要占多高：表格是表头加第一行正文，文字是前
/// `MIN_SPLIT_LINES` 行，拆不开的（图、分隔线）是整块。
fn min_first_fragment(block: &LayoutBlock) -> u32 {
    if let Some(table) = block.table.as_ref() {
        return table
            .rows
            .iter()
            .find(|row| !row.header)
            .map_or(block.total_height, |row| row.source_end);
    }
    block
        .boundaries
        .get(MIN_SPLIT_LINES - 1)
        .copied()
        .unwrap_or(block.total_height)
        .min(block.total_height)
}

fn place_block(
    columns: &mut Vec<ColumnPlan>,
    block_index: usize,
    block: &LayoutBlock,
    usable_height: u32,
) -> Result<()> {
    let mut source_start = 0;
    let mut first_fragment = true;
    while source_start < block.total_height {
        if source_start > 0 {
            repeat_table_header(active(columns)?, block_index, block);
        }
        let column = active(columns)?;
        let margin = if first_fragment && column.used_height > 0 {
            block.margin_before
        } else {
            0
        };
        let remaining = block.total_height.saturating_sub(source_start);
        let available = usable_height
            .saturating_sub(column.used_height)
            .saturating_sub(margin);
        let limit = source_start.saturating_add(available);
        let source_end = if remaining <= available {
            block.total_height
        } else if let Some(end) = split_point(block, source_start, limit) {
            end
        } else if column_has_other_content(column, block_index, source_start) {
            push_column(columns)?;
            continue;
        } else {
            // 空栏也凑不出像样的两截（栏很矮时会遇到）：能放多少放多少。
            let end = last_boundary_within(block, source_start, limit);
            if end <= source_start {
                bail!("a rendered text line exceeds the usable page height");
            }
            end
        };

        let y = column.used_height.saturating_add(margin);
        column.placements.push(Placement {
            block_index,
            source_start,
            source_end,
            y,
        });
        column.used_height = y.saturating_add(source_end.saturating_sub(source_start));
        source_start = source_end;
        first_fragment = false;
        if source_start < block.total_height {
            push_column(columns)?;
        } else {
            column.used_height = column
                .used_height
                .saturating_add(block.margin_after)
                .min(usable_height);
        }
    }
    Ok(())
}

/// 表格续到新的一栏时先重复表头。
fn repeat_table_header(column: &mut ColumnPlan, block_index: usize, block: &LayoutBlock) {
    let Some(table) = block.table.as_ref() else {
        return;
    };
    if table.header_height == 0 || column.used_height > 0 {
        return;
    }
    column.placements.push(Placement {
        block_index,
        source_start: 0,
        source_end: table.header_height,
        y: 0,
    });
    column.used_height = table.header_height;
}

/// 这一栏里除了这块自己续栏时补的表头，还有没有别的东西。只有表头的话换栏也
/// 没用，新栏一样只能先放表头。
fn column_has_other_content(column: &ColumnPlan, block_index: usize, source_start: u32) -> bool {
    column.placements.iter().any(|placement| {
        !(source_start > 0 && placement.block_index == block_index && placement.source_start == 0)
    })
}

/// 在 `(source_start, limit]` 里找最靠下、两截都像样的断点：表格这一截至少一行
/// 正文（第一截还得连着表头），剩下的也至少一行；文字两截各至少
/// `MIN_SPLIT_LINES` 行。拆不开的块（图、分隔线）没有断点。
fn split_point(block: &LayoutBlock, source_start: u32, limit: u32) -> Option<u32> {
    let boundaries = &block.boundaries;
    if boundaries.len() < 2 {
        return None;
    }
    let min_lines = if block.table.is_some() {
        1
    } else {
        MIN_SPLIT_LINES
    };
    let floor = if source_start == 0 {
        min_first_fragment(block)
    } else {
        0
    };
    let first_after_start = boundaries.partition_point(|boundary| *boundary <= source_start);
    (first_after_start..boundaries.len())
        .rev()
        .map(|index| (index, boundaries[index]))
        .filter(|(_, end)| *end <= limit && *end >= floor && *end < block.total_height)
        .find(|(index, _)| {
            let before = index + 1 - first_after_start;
            let after = boundaries.len() - index - 1;
            before >= min_lines && after >= min_lines
        })
        .map(|(_, end)| end)
}

fn last_boundary_within(block: &LayoutBlock, source_start: u32, limit: u32) -> u32 {
    block
        .boundaries
        .iter()
        .copied()
        .filter(|boundary| *boundary > source_start && *boundary <= limit)
        .last()
        .unwrap_or(source_start)
}

fn active(columns: &mut [ColumnPlan]) -> Result<&mut ColumnPlan> {
    columns
        .last_mut()
        .ok_or_else(|| anyhow!("renderer column planner lost its active column"))
}

pub(in crate::platforms::plugins::renderer) fn push_column(
    columns: &mut Vec<ColumnPlan>,
) -> Result<()> {
    columns
        .len()
        .checked_add(1)
        .context("rendered Markdown column count overflowed")?;
    columns.push(ColumnPlan::default());
    Ok(())
}

/// 定栏数和栏高：从放得下的最少栏数往上试，每种栏数二分出能装下的最矮栏高（各栏
/// 自然就匀了），挑成图最接近 `TARGET_ASPECT_RATIO` 的（比例几乎相等偏向少栏）。
/// 栏数再加只会更宽更矮，一过目标比例就停；超了单页像素上限的不要。
pub(in crate::platforms::plugins::renderer) fn plan_balanced_columns(
    layouts: &[LayoutBlock],
    config: &NormalizedConfig,
) -> Result<Vec<ColumnPlan>> {
    let max_usable = MAX_PAGE_HEIGHT.saturating_sub(config.padding.saturating_mul(2));
    let fewest_plan = plan_columns_with_height(layouts, max_usable)?;
    let fewest = fewest_plan.len();

    let total_content: u64 = layouts
        .iter()
        .map(|block| u64::from(block.total_height))
        .sum();
    let height_floor = u64::from(
        MIN_RENDERED_HEIGHT
            .saturating_sub(config.padding.saturating_mul(2))
            .max(128),
    );
    let mut best: Option<(Vec<ColumnPlan>, f32)> = None;
    for candidate in fewest..=fewest.saturating_add(MAX_EXTRA_COLUMNS) {
        let low = total_content
            .div_ceil(candidate as u64)
            .max(height_floor)
            .min(u64::from(max_usable)) as u32;
        let Some(plan) = balanced_plan_for_count(layouts, candidate, low, max_usable) else {
            continue;
        };
        let (width, height) = page_size(&plan, config);
        if u64::from(width) * u64::from(height) > MAX_PAGE_PIXELS {
            break;
        }
        let distance = aspect_distance(&plan, config);
        let improves = best
            .as_ref()
            .map(|(_, best_distance)| distance + ASPECT_TIE_EPSILON < *best_distance)
            .unwrap_or(true);
        if improves {
            best = Some((plan, distance));
        }
        if width as f32 / height as f32 >= TARGET_ASPECT_RATIO {
            break;
        }
    }
    Ok(best.map(|(plan, _)| plan).unwrap_or(fewest_plan))
}

/// Binary-searches the smallest usable height in `[low, high]` whose plan fits
/// in at most `target_columns` columns. Returns `None` when even the full
/// height `high` cannot satisfy the target.
pub(in crate::platforms::plugins::renderer) fn balanced_plan_for_count(
    layouts: &[LayoutBlock],
    target_columns: usize,
    low: u32,
    high: u32,
) -> Option<Vec<ColumnPlan>> {
    let mut best = match plan_columns_with_height(layouts, high) {
        Ok(plan) if plan.len() <= target_columns => plan,
        _ => return None,
    };
    let mut low = low.min(high);
    let mut high = high;
    while low < high {
        let mid = low + (high - low) / 2;
        match plan_columns_with_height(layouts, mid) {
            Ok(plan) if plan.len() <= target_columns => {
                best = plan;
                high = mid;
            }
            _ => low = mid.saturating_add(1),
        }
    }
    Some(best)
}

/// 成图的宽高，与 `render_pages` 同一套算法。
pub(in crate::platforms::plugins::renderer) fn page_size(
    columns: &[ColumnPlan],
    config: &NormalizedConfig,
) -> (u32, u32) {
    let count = u32::try_from(columns.len()).unwrap_or(u32::MAX);
    let width = config
        .padding
        .saturating_mul(2)
        .saturating_add(COLUMN_WIDTH.saturating_mul(count))
        .saturating_add(COLUMN_GAP.saturating_mul(count.saturating_sub(1)));
    let content_height = columns
        .iter()
        .map(|column| column.used_height)
        .max()
        .unwrap_or(0);
    let height = content_height
        .saturating_add(config.padding.saturating_mul(2))
        .clamp(MIN_RENDERED_HEIGHT, MAX_PAGE_HEIGHT);
    (width, height)
}

/// Log-space distance between the finished image's aspect ratio and
/// `TARGET_ASPECT_RATIO`.
pub(in crate::platforms::plugins::renderer) fn aspect_distance(
    columns: &[ColumnPlan],
    config: &NormalizedConfig,
) -> f32 {
    let (width, height) = page_size(columns, config);
    ((width as f32 / height as f32).ln() - TARGET_ASPECT_RATIO.ln()).abs()
}
