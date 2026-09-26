use super::*;

pub(super) fn preflight_operations(operations: Vec<Operation>) -> Result<Vec<FileChange>> {
    let mut staged: HashMap<PathBuf, Option<String>> = HashMap::new();
    let mut changes = Vec::new();

    for operation in operations {
        match operation {
            Operation::Add { path, lines } => {
                if staged
                    .get(&path)
                    .and_then(|content| content.as_ref())
                    .is_some()
                    || (!staged.contains_key(&path) && path.exists())
                {
                    bail!(
                        "patch verification failed: file already exists: {}",
                        path.display()
                    )
                }
                let after = ensure_trailing_newline(lines.join("\n"));
                staged.insert(path.clone(), Some(after.clone()));
                changes.push(FileChange {
                    path,
                    before: String::new(),
                    after,
                    kind: ChangeKind::Add,
                });
            }
            Operation::Delete { path } => {
                let before = staged_content(&path, &staged)?;
                staged.insert(path.clone(), None);
                changes.push(FileChange {
                    path,
                    before,
                    after: String::new(),
                    kind: ChangeKind::Delete,
                });
            }
            Operation::Update {
                path,
                move_to,
                hunks,
            } => {
                if move_to.is_some() {
                    bail!("patch verification failed: Move to is not supported yet")
                }
                let before = staged_content(&path, &staged)?;
                // CRLF 文件的匹配靠 TrimEnd 模式成功,但替换进来的新行是
                // LF:统一按 LF 应用,写回前还原原文件的行尾风格,避免混合。
                let crlf = before.contains("\r\n");
                let mut after = if crlf {
                    before.replace("\r\n", "\n")
                } else {
                    before.clone()
                };
                let total = hunks.len();
                let mut cursor = 0;
                for (index, hunk) in hunks.iter().enumerate() {
                    let (next, next_cursor) = apply_hunk(&path, &after, hunk, cursor)
                        .map_err(|err| describe_hunk_failure(err, index, total, hunk))?;
                    after = next;
                    cursor = next_cursor;
                }
                if crlf {
                    after = after.replace('\n', "\r\n");
                }
                staged.insert(path.clone(), Some(after.clone()));
                changes.push(FileChange {
                    path,
                    before,
                    after,
                    kind: ChangeKind::Update,
                });
            }
        }
    }

    Ok(changes)
}

fn staged_content(path: &Path, staged: &HashMap<PathBuf, Option<String>>) -> Result<String> {
    if let Some(content) = staged.get(path) {
        return content.clone().ok_or_else(|| {
            anyhow::anyhow!(
                "patch verification failed: file was deleted earlier in patch: {}",
                path.display()
            )
        });
    }
    std::fs::read_to_string(path).map_err(|err| {
        anyhow::anyhow!(
            "patch verification failed: failed to read file to update {}: {err}",
            path.display()
        )
    })
}

/// 把一块补丁打进 `content`，返回新内容和下一块的游标（这一块改完之后的那一行）。
///
/// `cursor` 只在从头找会歧义时用：补丁里的块按文件顺序写，所以取上一块之后唯一的
/// 那一处（09-24 B10）。匹配唯一时照旧从头找，乱序写的补丁不受影响。
pub(super) fn apply_hunk(
    path: &Path,
    content: &str,
    hunk: &Hunk,
    cursor: usize,
) -> Result<(String, usize)> {
    let old = hunk_text(&hunk.lines, false);
    let new = hunk_text(&hunk.lines, true);
    if old.is_empty() {
        return apply_insertion_hunk(path, content, hunk, &new);
    }
    let current_lines = content_lines(content);
    let mut pattern = content_lines(&old);
    let new_lines = content_lines(&new);
    let start_index = hunk
        .context
        .as_ref()
        .and_then(|context| seek_sequence(&current_lines, &[context.to_string()], 0, false))
        .map(|index| index + 1)
        .unwrap_or(0);
    let anchored = hunk.context.is_some() || hunk.end_of_file;
    for trim_blank_tail in [false, true] {
        if trim_blank_tail {
            if !pattern.last().is_some_and(|line| line.is_empty()) {
                break;
            }
            pattern.pop();
        }
        let Some((found, mode)) =
            seek_sequence_with_mode(&current_lines, &pattern, start_index, hunk.end_of_file)
        else {
            continue;
        };
        // 无 @@ 锚点的 hunk 在文件中多处匹配时,首命中会落到错误位置还返回
        // ok:与 fallback 子串路径的 count>1 拒绝对齐。带锚点/EOF 的视为已消歧。
        // 只在命中的那一档里数（09-24 B10）：精确匹配唯一时，后面一处只差缩进
        // 的同文不算歧义——原来四档一起数，误报。
        let unique_after = |from: usize| {
            seek_in_mode(&current_lines, &pattern, from, mode)
                .filter(|&index| seek_in_mode(&current_lines, &pattern, index + 1, mode).is_none())
        };
        let target = if anchored || unique_after(found) == Some(found) {
            found
        } else if let Some(after_previous) =
            (cursor > found).then(|| unique_after(cursor)).flatten()
        {
            after_previous
        } else {
            bail!(
                "patch verification failed: hunk matches multiple places in {}; add a @@ context line to pick one",
                path.display()
            )
        };
        let next_cursor = target + new_lines.len();
        let mut result = current_lines.clone();
        result.splice(target..target + pattern.len(), new_lines);
        return Ok((join_lines(result), next_cursor));
    }

    // Fallback for legacy simple hunks: keep exact substring replacement.
    if old.is_empty() {
        bail!(
            "patch verification failed: empty update context for {}",
            path.display()
        )
    }
    let count = count_occurrences(content, &old);
    if count == 0 {
        bail!(
            "patch verification failed: hunk does not match {}",
            path.display()
        )
    }
    if count > 1 {
        bail!(
            "patch verification failed: hunk matches multiple places in {}; add a @@ context line to pick one",
            path.display()
        )
    }
    let start_line = content[..content.find(&old).unwrap_or(0)]
        .matches('\n')
        .count();
    Ok((
        content.replacen(&old, &new, 1),
        start_line + content_lines(&new).len(),
    ))
}

/// 失败的那一块是第几块、从哪一行开始：模型据此只重写这一块，不必整份重来。
fn describe_hunk_failure(
    error: anyhow::Error,
    index: usize,
    total: usize,
    hunk: &Hunk,
) -> anyhow::Error {
    let first = hunk
        .lines
        .iter()
        .find_map(|line| match line {
            HunkLine::Context(text) | HunkLine::Delete(text) => Some(text.trim()),
            HunkLine::Insert(_) => None,
        })
        .filter(|text| !text.is_empty())
        .map(|text| text.chars().take(80).collect::<String>());
    let mut message = format!("{error} (hunk {} of {total}", index + 1);
    if let Some(first) = first {
        message.push_str(&format!(", starting at `{first}`"));
    }
    message.push_str("). Re-read the file and copy those lines exactly.");
    anyhow::anyhow!(message)
}

fn apply_insertion_hunk(
    path: &Path,
    content: &str,
    hunk: &Hunk,
    new: &str,
) -> Result<(String, usize)> {
    let mut lines = content_lines(content);
    let insert = content_lines(new);
    let index = if hunk.end_of_file {
        lines.len()
    } else if let Some(context) = &hunk.context {
        seek_sequence(&lines, &[context.to_string()], 0, false)
            .map(|index| index + 1)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "patch verification failed: failed to find context '{}' in {}",
                    context,
                    path.display()
                )
            })?
    } else {
        lines.len()
    };
    let next_cursor = index + insert.len();
    lines.splice(index..index, insert);
    Ok((join_lines(lines), next_cursor))
}

fn hunk_text(lines: &[HunkLine], new_side: bool) -> String {
    let selected = lines
        .iter()
        .filter_map(|line| match (new_side, line) {
            (_, HunkLine::Context(text)) => Some(text.as_str()),
            (false, HunkLine::Delete(text)) => Some(text.as_str()),
            (true, HunkLine::Insert(text)) => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    ensure_trailing_newline(selected.join("\n"))
}

fn ensure_trailing_newline(mut value: String) -> String {
    if !value.is_empty() && !value.ends_with('\n') {
        value.push('\n');
    }
    value
}

fn content_lines(content: &str) -> Vec<String> {
    let mut lines = content.split('\n').map(str::to_string).collect::<Vec<_>>();
    if lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    lines
}

fn join_lines(mut lines: Vec<String>) -> String {
    if lines.is_empty() {
        return String::new();
    }
    lines.push(String::new());
    lines.join("\n")
}

fn seek_sequence(
    lines: &[String],
    pattern: &[String],
    start_index: usize,
    eof: bool,
) -> Option<usize> {
    seek_sequence_with_mode(lines, pattern, start_index, eof).map(|(index, _)| index)
}

/// 从宽到严逐档找，返回第一处命中和它是在哪一档命中的。
fn seek_sequence_with_mode(
    lines: &[String],
    pattern: &[String],
    start_index: usize,
    eof: bool,
) -> Option<(usize, CompareMode)> {
    if pattern.is_empty() {
        return None;
    }
    if eof {
        let from_end = lines.len().checked_sub(pattern.len())?;
        if from_end >= start_index && lines_match_at(lines, pattern, from_end, CompareMode::Exact) {
            return Some((from_end, CompareMode::Exact));
        }
    }
    [
        CompareMode::Exact,
        CompareMode::TrimEnd,
        CompareMode::Trim,
        CompareMode::Normalize,
    ]
    .into_iter()
    .find_map(|mode| seek_in_mode(lines, pattern, start_index, mode).map(|index| (index, mode)))
}

fn seek_in_mode(
    lines: &[String],
    pattern: &[String],
    start_index: usize,
    mode: CompareMode,
) -> Option<usize> {
    if pattern.is_empty() {
        return None;
    }
    (start_index..=lines.len().saturating_sub(pattern.len()))
        .find(|&index| lines_match_at(lines, pattern, index, mode))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CompareMode {
    Exact,
    TrimEnd,
    Trim,
    Normalize,
}

fn lines_match_at(lines: &[String], pattern: &[String], index: usize, mode: CompareMode) -> bool {
    pattern.iter().enumerate().all(|(offset, expected)| {
        let Some(actual) = lines.get(index + offset) else {
            return false;
        };
        match mode {
            CompareMode::Exact => actual == expected,
            CompareMode::TrimEnd => actual.trim_end() == expected.trim_end(),
            CompareMode::Trim => actual.trim() == expected.trim(),
            CompareMode::Normalize => normalize_match(actual) == normalize_match(expected),
        }
    })
}

fn normalize_match(value: &str) -> String {
    value
        .trim()
        .replace(['‘', '’', '‚', '‛'], "'")
        .replace(['“', '”', '„', '‟'], "\"")
        .replace(['‐', '‑', '‒', '–', '—', '―', '\u{2212}'], "-")
        .replace('…', "...")
        .replace(
            [
                '\u{00a0}', '\u{2002}', '\u{2003}', '\u{2004}', '\u{2005}', '\u{2006}', '\u{2007}',
                '\u{2008}', '\u{2009}', '\u{200a}', '\u{202f}', '\u{3000}',
            ],
            " ",
        )
}

fn count_occurrences(content: &str, search: &str) -> usize {
    if search.is_empty() {
        return 0;
    }
    let mut count = 0;
    let mut offset = 0;
    while let Some(pos) = content[offset..].find(search) {
        count += 1;
        offset += pos + search.len();
    }
    count
}
