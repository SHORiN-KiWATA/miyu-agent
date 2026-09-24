use super::*;

#[derive(PartialEq)]
pub(super) enum PatchNamespace {
    Filesystem,
    Artifact,
    KnowledgeBase,
}

/// 扫描补丁头分类命名空间;混用直接拒绝,错误比静默猜测便宜。
pub(super) fn detect_patch_namespace(patch_text: &str) -> Result<PatchNamespace> {
    let mut namespace: Option<PatchNamespace> = None;
    for line in patch_text.lines() {
        let value = header_value(line, "*** Add File: ")
            .or_else(|| header_value(line, "*** Update File: "))
            .or_else(|| header_value(line, "*** Delete File: "));
        let Some(value) = value else { continue };
        let current = if value.starts_with("artifact:") {
            PatchNamespace::Artifact
        } else if value.starts_with("kb:") {
            PatchNamespace::KnowledgeBase
        } else {
            PatchNamespace::Filesystem
        };
        match &namespace {
            None => namespace = Some(current),
            Some(existing) if *existing == current => {}
            Some(_) => bail!(
                "patch rejected: one patch must target a single namespace (filesystem, artifact:, or kb:)"
            ),
        }
    }
    Ok(namespace.unwrap_or(PatchNamespace::Filesystem))
}
pub(super) fn parse_patch_with<F>(raw: &str, resolve_path: &F) -> Result<Vec<Operation>>
where
    F: Fn(&str) -> Result<PathBuf>,
{
    let normalized = strip_wrappers(raw)
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let lines = normalized.lines().collect::<Vec<_>>();
    let begin = lines
        .iter()
        .position(|line| line.trim() == "*** Begin Patch")
        .ok_or_else(|| anyhow::anyhow!("patch verification failed: missing *** Begin Patch"))?;
    let end = lines
        .iter()
        .rposition(|line| line.trim() == "*** End Patch")
        .ok_or_else(|| anyhow::anyhow!("patch verification failed: missing *** End Patch"))?;
    if begin >= end {
        bail!("patch verification failed: missing *** Begin Patch")
    }

    let mut operations = Vec::new();
    let mut index = begin + 1;
    while index < end {
        let line = lines[index];
        if line.trim().is_empty() {
            index += 1;
            continue;
        }
        if let Some(path) = header_value(line, "*** Add File:") {
            index += 1;
            let mut content = Vec::new();
            while index < end && !is_patch_header(lines[index]) {
                let Some(rest) = lines[index].strip_prefix('+') else {
                    bail!("patch verification failed: Add File lines must start with +")
                };
                content.push(rest.to_string());
                index += 1;
            }
            operations.push(Operation::Add {
                path: resolve_path(path)?,
                lines: content,
            });
        } else if let Some(path) = header_value(line, "*** Delete File:") {
            operations.push(Operation::Delete {
                path: resolve_path(path)?,
            });
            index += 1;
        } else if let Some(path) = header_value(line, "*** Update File:") {
            index += 1;
            let mut move_to = None;
            if index < end {
                if let Some(target) = header_value(lines[index], "*** Move to:") {
                    move_to = Some(resolve_path(target)?);
                    index += 1;
                }
            }
            let mut hunks = Vec::new();
            while index < end && !is_patch_header(lines[index]) {
                if lines[index].starts_with("--- ") || lines[index].starts_with("+++ ") {
                    index += 1;
                    continue;
                }
                if !lines[index].starts_with("@@") {
                    // hunk 内容样的行(以 ' '/'-'/'+' 开头)出现在 @@ 之外,
                    // 多半是漏写了 @@ 头:静默跳过等于把整块修改丢掉。
                    let stray = lines[index];
                    if stray.starts_with(' ') || stray.starts_with('-') || stray.starts_with('+') {
                        bail!(
                            "patch verification failed: hunk line outside a @@ hunk (missing @@ header?): {stray}"
                        )
                    }
                    index += 1;
                    continue;
                }
                let context = lines[index]
                    .strip_prefix("@@")
                    .unwrap_or_default()
                    .trim()
                    .trim_matches('@')
                    .trim();
                let context =
                    (!context.is_empty() && !context.starts_with('-') && !context.starts_with('+'))
                        .then(|| context.to_string());
                index += 1;
                let mut hunk_lines = Vec::new();
                let mut end_of_file = false;
                while index < end
                    && !lines[index].starts_with("@@")
                    && !is_patch_header(lines[index])
                {
                    let line = lines[index];
                    if let Some(rest) = line.strip_prefix(' ') {
                        hunk_lines.push(HunkLine::Context(rest.to_string()));
                    } else if let Some(rest) = line.strip_prefix('-') {
                        hunk_lines.push(HunkLine::Delete(rest.to_string()));
                    } else if let Some(rest) = line.strip_prefix('+') {
                        hunk_lines.push(HunkLine::Insert(rest.to_string()));
                    } else if line == "\\ No newline at end of file" {
                    } else if line == "*** End of File" {
                        end_of_file = true;
                        index += 1;
                        break;
                    } else {
                        bail!("patch verification failed: invalid hunk line: {line}")
                    }
                    index += 1;
                }
                if hunk_lines.is_empty() {
                    bail!("patch verification failed: empty hunk")
                }
                hunks.push(Hunk {
                    context,
                    end_of_file,
                    lines: hunk_lines,
                });
            }
            if hunks.is_empty() {
                bail!("patch verification failed: Update File requires at least one hunk")
            }
            operations.push(Operation::Update {
                path: resolve_path(path)?,
                move_to,
                hunks,
            });
        } else {
            bail!("patch verification failed: unknown patch header: {line}")
        }
    }
    Ok(operations)
}

fn strip_wrappers(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.starts_with("```") && trimmed.ends_with("```") {
        let mut lines = trimmed.lines().collect::<Vec<_>>();
        lines.remove(0);
        lines.pop();
        return lines.join("\n");
    }
    if let Some(captures) = strip_simple_heredoc(trimmed) {
        return captures;
    }
    trimmed.to_string()
}

fn strip_simple_heredoc(raw: &str) -> Option<String> {
    let first = raw.lines().next()?.trim();
    let marker = first
        .strip_prefix("cat <<")
        .or_else(|| first.strip_prefix("<<"))?
        .trim()
        .trim_matches('\'')
        .trim_matches('"');
    if marker.is_empty() {
        return None;
    }
    let mut body = raw.lines().skip(1).collect::<Vec<_>>();
    if body.last().map(|line| line.trim()) == Some(marker) {
        body.pop();
        return Some(body.join("\n"));
    }
    None
}

fn header_value<'a>(line: &'a str, prefix: &str) -> Option<&'a str> {
    line.strip_prefix(prefix)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn is_patch_header(line: &str) -> bool {
    line.starts_with("*** Add File:")
        || line.starts_with("*** Delete File:")
        || line.starts_with("*** Update File:")
        || line.starts_with("*** End Patch")
}
