use super::line_diff::{diff_lines, EditLine};
use super::ToolProgress;
use anyhow::Result;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

pub(crate) fn write_with_patch_preview(
    path: &Path,
    before: &str,
    after: &str,
    progress: &ToolProgress,
    mut result: Map<String, Value>,
) -> Result<String> {
    let target = write_target(path)?;
    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let temp = new_temp_file(parent)?;
    std::fs::write(temp.path(), after.as_bytes())?;
    // 已有的文件沿用原来的权限位（09-24 B2）：tempfile 在 Unix 上按 0600 建临时
    // 文件，rename 之后目标就成了 0600，改一次脚本就丢执行位。
    if let Ok(metadata) = std::fs::metadata(&target) {
        std::fs::set_permissions(temp.path(), metadata.permissions())?;
    }
    temp.persist(&target)?;
    report_patch_preview(progress, path, &patch_result_json(path, before, after));
    result.insert("ok".to_string(), Value::Bool(true));
    result.insert("path".to_string(), Value::String(display_path(path)));
    Ok(serde_json::to_string_pretty(&Value::Object(result))?)
}

/// 真正要写的那个文件。
///
/// 路径是软链时写到它指向的文件（09-24 B2）：rename 会把软链本身换成普通文件，
/// dotfiles 这类软链就此断开。解析出来的目标要再过一遍沙盒写检查——项目里一个
/// 指向沙盒外的软链，不能成为往外写的口子。指向不存在的目标时按原路径写。
fn write_target(path: &Path) -> Result<PathBuf> {
    let is_link = std::fs::symlink_metadata(path)
        .map(|metadata| metadata.file_type().is_symlink())
        .unwrap_or(false);
    if !is_link {
        return Ok(path.to_path_buf());
    }
    match std::fs::canonicalize(path) {
        Ok(target) => {
            miyu_base::sandbox::guard_write(&target)?;
            Ok(target)
        }
        Err(_) => Ok(path.to_path_buf()),
    }
}

/// 新文件按 0666 建、交给 umask 收窄，和 shell 重定向建出来的权限一样；tempfile
/// 默认的 0600 只适合临时文件。已有文件随后改回它原来的权限。
fn new_temp_file(parent: &Path) -> Result<tempfile::NamedTempFile> {
    let mut builder = tempfile::Builder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(std::fs::Permissions::from_mode(0o666));
    }
    Ok(builder.tempfile_in(parent)?)
}

pub(crate) fn patch_result_json(path: &Path, before: &str, after: &str) -> String {
    unified_diff(&display_path(path), before, after)
}

fn report_patch_preview(progress: &ToolProgress, path: &Path, diff: &str) {
    let Ok(payload) = serde_json::to_string(&json!({
        "path": display_path(path),
        "diff": diff,
    })) else {
        return;
    };
    progress.report(format!("__patch_preview__{payload}"));
}

pub(crate) fn display_path(path: &Path) -> String {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        miyu_base::workspace::effective_workdir().join(path)
    };
    if let Ok(stripped) = absolute.strip_prefix(miyu_base::workspace::effective_workdir()) {
        return stripped.display().to_string();
    }
    if let Ok(home) = std::env::var("HOME") {
        let home = PathBuf::from(home);
        if let Ok(stripped) = absolute.strip_prefix(home) {
            return format!("~/{}", stripped.display());
        }
    }
    path.display().to_string()
}

fn unified_diff(path: &str, before: &str, after: &str) -> String {
    let before_lines = split_lines(before);
    let after_lines = split_lines(after);
    let edits = diff_lines(&before_lines, &after_lines);
    let mut output = String::new();
    output.push_str(&format!("--- a/{path}\n"));
    output.push_str(&format!("+++ b/{path}\n"));
    for hunk in diff_hunks(&edits) {
        output.push_str(&format!(
            "@@ -{},{} +{},{} @@\n",
            hunk.old_start, hunk.old_count, hunk.new_start, hunk.new_count
        ));
        for edit in &edits[hunk.start..hunk.end] {
            output.push(edit.marker());
            output.push_str(edit.line());
            output.push('\n');
        }
    }
    output
}

#[derive(Debug, Eq, PartialEq)]
struct DiffHunk {
    start: usize,
    end: usize,
    old_start: usize,
    old_count: usize,
    new_start: usize,
    new_count: usize,
}

fn diff_hunks(edits: &[EditLine<'_>]) -> Vec<DiffHunk> {
    const CONTEXT: usize = 3;
    let mut changed = Vec::new();
    for (index, edit) in edits.iter().enumerate() {
        match edit {
            EditLine::Context(_) => {}
            EditLine::Delete(_) | EditLine::Insert(_) => changed.push(index),
        }
    }
    if changed.is_empty() {
        return Vec::new();
    }

    let mut ranges = Vec::<(usize, usize)>::new();
    for index in changed {
        let start = index.saturating_sub(CONTEXT);
        let end = (index + CONTEXT + 1).min(edits.len());
        if let Some((_, last_end)) = ranges.last_mut() {
            if start <= *last_end {
                *last_end = (*last_end).max(end);
                continue;
            }
        }
        ranges.push((start, end));
    }

    ranges
        .into_iter()
        .map(|(start, end)| {
            let (old_start, new_start) = line_numbers_at(edits, start);
            let (old_count, new_count) = line_counts(&edits[start..end]);
            DiffHunk {
                start,
                end,
                old_start,
                old_count,
                new_start,
                new_count,
            }
        })
        .collect()
}

fn line_numbers_at(edits: &[EditLine<'_>], index: usize) -> (usize, usize) {
    let mut old_line = 1usize;
    let mut new_line = 1usize;
    for edit in &edits[..index] {
        match edit {
            EditLine::Context(_) => {
                old_line += 1;
                new_line += 1;
            }
            EditLine::Delete(_) => old_line += 1,
            EditLine::Insert(_) => new_line += 1,
        }
    }
    (old_line, new_line)
}

fn line_counts(edits: &[EditLine<'_>]) -> (usize, usize) {
    let mut old_count = 0usize;
    let mut new_count = 0usize;
    for edit in edits {
        match edit {
            EditLine::Context(_) => {
                old_count += 1;
                new_count += 1;
            }
            EditLine::Delete(_) => old_count += 1,
            EditLine::Insert(_) => new_count += 1,
        }
    }
    (old_count, new_count)
}

fn split_lines(value: &str) -> Vec<String> {
    if value.is_empty() {
        return Vec::new();
    }
    value
        .lines()
        .map(str::to_string)
        .chain(if value.ends_with('\n') {
            Vec::new()
        } else {
            vec!["\\ No newline at end of file".to_string()]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_unified_diff() {
        let diff = unified_diff("demo.txt", "one\ntwo\n", "one\nTWO\nthree\n");
        assert!(diff.contains("--- a/demo.txt"));
        assert!(diff.contains("+++ b/demo.txt"));
        assert!(diff.contains("-two"));
        assert!(diff.contains("+TWO"));
        assert!(diff.contains("+three"));
    }

    /// 一千多行的文件改一行：diff 只有那一行一块（用户 09-26：原来两边行数一乘过 25 万就算成
    /// 整份删了再加，抬头写 `+1269 -1285`，点开是整个文件）。
    #[test]
    fn a_one_line_edit_in_a_big_file_is_a_one_line_diff() {
        let before: String = (0..1_300).map(|index| format!("line {index}\n")).collect();
        let after = before.replace("line 650\n", "changed\n");
        let diff = unified_diff("big.html", &before, &after);
        assert_eq!(crate::tools::diff_stat(&diff), Some((1, 1)), "{diff}");
        assert_eq!(diff.matches("@@ ").count(), 1, "{diff}");
    }

    #[test]
    fn new_file_diff_contains_insertions() {
        let diff = unified_diff("new.txt", "", "alpha\nbeta\n");
        assert!(diff.contains("@@ -1,0 +1,2 @@"));
        assert!(diff.contains("+alpha"));
        assert!(diff.contains("+beta"));
        assert!(!diff.contains("-alpha"));
    }

    #[test]
    fn emptied_file_diff_contains_deletions() {
        let diff = unified_diff("old.txt", "alpha\nbeta\n", "");
        assert!(diff.contains("@@ -1,2 +1,0 @@"));
        assert!(diff.contains("-alpha"));
        assert!(diff.contains("-beta"));
        assert!(!diff.contains("+alpha"));
    }
}
