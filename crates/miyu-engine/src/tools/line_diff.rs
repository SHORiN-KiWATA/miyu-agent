//! 两份文本逐行比对：编辑工具的补丁预览（点开的 diff、时间线抬头的 `+N -M`）用它。
//!
//! 先剥掉首尾相同的行，中间用 Myers 差分：耗时随改了多少行涨，和文件多大无关。原来是一整张
//! LCS 表，两边行数一乘过了 25 万（各五百来行）就放弃、整份算成删了再加——一千多行的文件改
//! 一处，抬头也写 `+1269 -1285`，点开是整个文件（用户 09-26）。改动大到超过
//! `MAX_EDIT_DISTANCE` 才退回把中间那段整段替换。

/// diff 的一行：没动的、删掉的、加上的。
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum EditLine<'a> {
    Context(&'a str),
    Delete(&'a str),
    Insert(&'a str),
}

impl<'a> EditLine<'a> {
    pub(crate) fn marker(&self) -> char {
        match self {
            Self::Context(_) => ' ',
            Self::Delete(_) => '-',
            Self::Insert(_) => '+',
        }
    }

    pub(crate) fn line(&self) -> &'a str {
        match self {
            Self::Context(line) | Self::Delete(line) | Self::Insert(line) => line,
        }
    }
}

/// 中间那段最多比到改了这么多行。Myers 的回溯表随改动行数平方长，两千行的改动已经是整段
/// 重写了，再往上就整段替换。
const MAX_EDIT_DISTANCE: usize = 2_000;

/// 一次比对最多走多少步（大约是「两边行数之和 × 改动行数」）。几万行的大文件被大段改写时，
/// 改动上限跟着往下收，编辑工具不会卡在算 diff 上（09-26 审查）。
const MAX_WORK: usize = 20_000_000;

/// `before` 到 `after` 的逐行改动，按行序排好（没动的行也在里面，拼 hunk 用）。
pub(crate) fn diff_lines<'a>(before: &'a [String], after: &'a [String]) -> Vec<EditLine<'a>> {
    let prefix = before
        .iter()
        .zip(after)
        .take_while(|(old, new)| old == new)
        .count();
    let suffix = before[prefix..]
        .iter()
        .rev()
        .zip(after[prefix..].iter().rev())
        .take_while(|(old, new)| old == new)
        .count();
    let old = &before[prefix..before.len() - suffix];
    let new = &after[prefix..after.len() - suffix];
    let mut edits: Vec<EditLine<'a>> = before[..prefix]
        .iter()
        .map(|line| EditLine::Context(line))
        .collect();
    edits.extend(myers(old, new).unwrap_or_else(|| replace_all(old, new)));
    edits.extend(
        before[before.len() - suffix..]
            .iter()
            .map(|line| EditLine::Context(line)),
    );
    edits
}

fn replace_all<'a>(old: &'a [String], new: &'a [String]) -> Vec<EditLine<'a>> {
    old.iter()
        .map(|line| EditLine::Delete(line))
        .chain(new.iter().map(|line| EditLine::Insert(line)))
        .collect()
}

/// Myers 贪心差分（最短编辑脚本）。改动超过 `MAX_EDIT_DISTANCE` 行时返回 `None`。
///
/// `trace[d]` 存第 d 步开始时对角线 `-d-1 ..= d+1` 上走到的最远 x，回溯时按它判断每一步是
/// 删一行（向右）还是加一行（向下）。
fn myers<'a>(old: &'a [String], new: &'a [String]) -> Option<Vec<EditLine<'a>>> {
    let (n, m) = (old.len() as isize, new.len() as isize);
    let total = old.len() + new.len();
    let limit = total.min(MAX_EDIT_DISTANCE).min(MAX_WORK / total.max(1)) as isize;
    let offset = limit + 1;
    let mut frontier = vec![0isize; (2 * offset + 1) as usize];
    let at = |k: isize| (offset + k) as usize;
    // 回溯表存 u32：x 不会是负的，也不会超过行数；整段重写到上限时这张表约 16 MB。
    let mut trace: Vec<Vec<u32>> = Vec::new();
    for d in 0..=limit {
        trace.push(
            frontier[at(-d - 1)..=at(d + 1)]
                .iter()
                .map(|&x| x as u32)
                .collect(),
        );
        for k in (-d..=d).step_by(2) {
            let down = k == -d || (k != d && frontier[at(k - 1)] < frontier[at(k + 1)]);
            let mut x = if down {
                frontier[at(k + 1)]
            } else {
                frontier[at(k - 1)] + 1
            };
            let mut y = x - k;
            while x < n && y < m && old[x as usize] == new[y as usize] {
                x += 1;
                y += 1;
            }
            frontier[at(k)] = x;
            if x >= n && y >= m {
                return Some(backtrack(old, new, &trace));
            }
        }
    }
    None
}

fn backtrack<'a>(old: &'a [String], new: &'a [String], trace: &[Vec<u32>]) -> Vec<EditLine<'a>> {
    let mut edits = Vec::new();
    let (mut x, mut y) = (old.len() as isize, new.len() as isize);
    for (d, snapshot) in trace.iter().enumerate().rev() {
        let d = d as isize;
        // 快照从对角线 -d-1 起存。
        let reached = |k: isize| snapshot[(k + d + 1) as usize] as isize;
        let k = x - y;
        let down = k == -d || (k != d && reached(k - 1) < reached(k + 1));
        let previous_k = if down { k + 1 } else { k - 1 };
        let previous_x = reached(previous_k);
        let previous_y = previous_x - previous_k;
        while x > previous_x && y > previous_y {
            edits.push(EditLine::Context(&old[x as usize - 1]));
            x -= 1;
            y -= 1;
        }
        if d > 0 {
            if down {
                edits.push(EditLine::Insert(&new[y as usize - 1]));
            } else {
                edits.push(EditLine::Delete(&old[x as usize - 1]));
            }
        }
        x = previous_x;
        y = previous_y;
    }
    edits.reverse();
    edits
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<String> {
        text.lines().map(str::to_string).collect()
    }

    fn counts(edits: &[EditLine<'_>]) -> (usize, usize) {
        let removed = edits
            .iter()
            .filter(|edit| matches!(edit, EditLine::Delete(_)))
            .count();
        let added = edits
            .iter()
            .filter(|edit| matches!(edit, EditLine::Insert(_)))
            .count();
        (added, removed)
    }

    /// 按改动还原得回改后那份：没动的和加上的连起来就是 `after`，没动的和删掉的连起来就是
    /// `before`。
    fn assert_replays(before: &[String], after: &[String], edits: &[EditLine<'_>]) {
        let old: Vec<&str> = edits
            .iter()
            .filter(|edit| !matches!(edit, EditLine::Insert(_)))
            .map(EditLine::line)
            .collect();
        let new: Vec<&str> = edits
            .iter()
            .filter(|edit| !matches!(edit, EditLine::Delete(_)))
            .map(EditLine::line)
            .collect();
        assert_eq!(old, before.iter().map(String::as_str).collect::<Vec<_>>());
        assert_eq!(new, after.iter().map(String::as_str).collect::<Vec<_>>());
    }

    fn numbered(count: usize) -> Vec<String> {
        (0..count).map(|index| format!("line {index}")).collect()
    }

    /// 一千三百行的文件改一行：就是 `+1 -1`（用户 09-26：原来行数一乘过 25 万就算成整份
    /// 删了再加，抬头写 `+1269 -1285`）。
    #[test]
    fn one_changed_line_in_a_big_file_is_one_line() {
        let before = numbered(1_300);
        let mut after = before.clone();
        after[650] = "changed".to_string();
        let edits = diff_lines(&before, &after);
        assert_eq!(counts(&edits), (1, 1));
        assert_replays(&before, &after, &edits);
    }

    /// 大文件里隔得很远的几处改动：只数真正改的行，中间那一大段是没动的。
    #[test]
    fn scattered_edits_in_a_big_file_count_only_what_changed() {
        let before = numbered(3_000);
        let mut after = before.clone();
        after[10] = "first".to_string();
        after.insert(1_500, "inserted".to_string());
        after.remove(2_900);
        let edits = diff_lines(&before, &after);
        assert_eq!(counts(&edits), (2, 2));
        assert_replays(&before, &after, &edits);
    }

    #[test]
    fn small_diffs_stay_minimal() {
        let before = lines("a\nb\nc\nd\ne");
        let after = lines("a\nc\nd\nx\ne\nf");
        let edits = diff_lines(&before, &after);
        assert_eq!(counts(&edits), (2, 1));
        assert_replays(&before, &after, &edits);
        assert_eq!(counts(&diff_lines(&[], &after)), (6, 0));
        assert_eq!(counts(&diff_lines(&before, &[])), (0, 5));
        assert_eq!(counts(&diff_lines(&before, &before)), (0, 0));
    }

    /// 整段重写（改动超过上限）照样给得出一份对的 diff：中间那段整段替换。
    #[test]
    fn a_full_rewrite_past_the_limit_replaces_the_middle() {
        let before = numbered(MAX_EDIT_DISTANCE + 10);
        let after: Vec<String> = (0..MAX_EDIT_DISTANCE + 10)
            .map(|index| format!("other {index}"))
            .collect();
        let edits = diff_lines(&before, &after);
        assert_eq!(
            counts(&edits),
            (MAX_EDIT_DISTANCE + 10, MAX_EDIT_DISTANCE + 10)
        );
        assert_replays(&before, &after, &edits);
    }
}
