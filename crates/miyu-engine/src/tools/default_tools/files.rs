//! 读文件、改文件、丢进回收站、按 glob 找、按内容搜。
//!
//! 读有三重上限（字节、行数、单行字符）：路径由模型给，一个日志文件就能把
//! 上下文吃光。
//!
//! 删除走系统回收站（`trash_one`）而不是 `rm`：模型判断错的代价必须可撤销。

use crate::tools::default_tools::*;

pub(in crate::tools) const MAX_READ_BYTES: u64 = 50 * 1024;

pub(in crate::tools) const MAX_READ_LINES: usize = 2_000;

pub(in crate::tools) const MAX_LINE_CHARS: usize = 2_000;

pub(in crate::tools) const SEARCH_TIMEOUT_SECONDS: u64 = 30;

pub(crate) fn read_file(args: Value) -> Result<String> {
    let path = path_arg(&args, "path")?;
    miyu_base::sandbox::guard_read(&path)?;
    let offset = args
        .get("offset")
        .and_then(Value::as_u64)
        .unwrap_or(1)
        .max(1) as usize;
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(MAX_READ_LINES as u64)
        .clamp(1, MAX_READ_LINES as u64) as usize;
    if path.is_dir() {
        let mut entries = Vec::new();
        for entry in std::fs::read_dir(&path)? {
            let entry = entry?;
            let suffix = if entry.file_type()?.is_dir() { "/" } else { "" };
            entries.push(format!("{}{}", entry.file_name().to_string_lossy(), suffix));
        }
        entries.sort();
        let start = offset.saturating_sub(1);
        let selected = entries
            .iter()
            .skip(start)
            .take(limit)
            .cloned()
            .collect::<Vec<_>>();
        let next = (start + selected.len() < entries.len()).then_some(offset + selected.len());
        let header = page_header(
            &format!("{}/", path.display().to_string().trim_end_matches('/')),
            "entries",
            offset,
            offset + selected.len().saturating_sub(1),
            next,
        );
        return Ok(format!("{header}\n{}", selected.join("\n")));
    }
    let metadata = std::fs::metadata(&path)?;
    if !metadata.is_file() {
        bail!("not a regular file or directory: {}", path.display())
    }
    ensure_not_binary_file(&path)?;
    let file = std::fs::File::open(&path)?;
    let reader = BufReader::new(file);
    let mut lines = Vec::new();
    let mut bytes = 0usize;
    let mut next = None;
    for (index, line) in reader.lines().enumerate() {
        let line_number = index + 1;
        if line_number < offset {
            continue;
        }
        if lines.len() >= limit || bytes >= MAX_READ_BYTES as usize {
            next = Some(line_number);
            break;
        }
        let mut line = line?;
        if line.chars().count() > MAX_LINE_CHARS {
            line = format!(
                "{}... (line truncated to {MAX_LINE_CHARS} chars)",
                line.chars().take(MAX_LINE_CHARS).collect::<String>()
            );
        }
        let rendered = format!("{line_number}: {line}");
        bytes += rendered.len() + 1;
        if bytes > MAX_READ_BYTES as usize {
            next = Some(line_number);
            break;
        }
        lines.push(rendered);
    }
    if lines.is_empty() && offset != 1 {
        bail!("offset {offset} is out of range")
    }
    // Pagination cursor before the bulky content: truncating consumers
    // (platform tool logs cap at 2400 chars) must still see truncated/next.
    // 纯文本（09-24，对照 opencode）：正文塞进 JSON 字符串会把每个换行和引号都转义，
    // 实测同样的内容多出一成多 token。旧回合里的 JSON 形态照旧逐字节回放。
    let header = page_header(
        &path.display().to_string(),
        "lines",
        offset,
        offset + lines.len().saturating_sub(1),
        next,
    );
    Ok(format!("{header}\n{}", lines.join("\n")))
}

/// 读取结果的第一行：读的是什么、这一页是哪一段、后面还有没有。
fn page_header(path: &str, unit: &str, first: usize, last: usize, next: Option<usize>) -> String {
    let mut header = format!("[{path} · {unit} {first}-{last}");
    match next {
        Some(next) => header.push_str(&format!(" · more below, continue with offset={next}]")),
        None => header.push_str(" · end]"),
    }
    header
}

pub(in crate::tools) fn trash_paths(args: Value, progress: ToolProgress) -> Result<String> {
    trash_paths_with(args, &progress, |path| {
        trash::delete(path).map_err(|err| anyhow::anyhow!("failed to move to trash: {err}"))
    })
}

/// 一次处理整批路径。
///
/// 逐条一次调用时,每条都要回一份带 `note`/`restore_hint` 的完整 JSON——删 12
/// 个就是 12 份几乎相同的文本刷屏,也白占模型上下文。改成整批之后,那两句提示
/// 整次只出现一次,终端上成功也只占一行。
///
/// 单条失败不中断后面的:删一批文件时,因为第 3 条没权限就把剩下 9 条也丢下,
/// 只会让模型再发一轮重试。失败项逐条收集,最后一并交代。
pub(in crate::tools) fn trash_paths_with(
    args: Value,
    progress: &ToolProgress,
    mut move_to_trash: impl FnMut(&Path) -> Result<()>,
) -> Result<String> {
    let inputs = paths_arg(&args)?;
    for input in &inputs {
        miyu_base::sandbox::guard_write(input)?;
    }
    let total = inputs.len();
    let mut moved_paths = Vec::new();
    let mut failures = Vec::new();
    // 不逐条报进度:同文件系统上移入回收站就是一次 rename,十几条也在毫秒内
    // 走完,那行进度还没被画出来就被下一条覆盖了,白发一串消息。真正慢的是
    // 模型吐这串路径的时间,那段由 `preparing_phase` 的「准备删除」盖住。
    for input in &inputs {
        match trash_one(input, &mut move_to_trash) {
            Ok(path) => moved_paths.push(path),
            Err(error) => failures.push(json!({
                "path": input.display().to_string(),
                "error": error.to_string(),
            })),
        }
    }
    // 失败清单走终端的最终摘要通道:成功时一行不多,失败时逐条列出来。
    if !failures.is_empty() {
        let lines = failures
            .iter()
            .map(|failure| {
                format!(
                    "✗ {}  {}",
                    failure["path"].as_str().unwrap_or_default(),
                    failure["error"].as_str().unwrap_or_default()
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        progress.report(format!("{TOOL_SUMMARY_PREFIX}{lines}"));
    }
    let moved = moved_paths.len();
    Ok(serde_json::to_string_pretty(&json!({
        "ok": moved > 0,
        "moved": moved,
        "failed": failures.len(),
        "total": total,
        "moved_paths": moved_paths,
        "failures": failures,
        "note": "The paths were moved to Trash, not permanently deleted.",
        "restore_hint": "Open the system Trash and restore an item if needed.",
    }))?)
}

/// 校验并移动单条路径,成功时返回它的原始绝对路径。
pub(in crate::tools) fn trash_one(
    input: &Path,
    move_to_trash: &mut impl FnMut(&Path) -> Result<()>,
) -> Result<String> {
    let resolved = resolve_existing_path_without_following_leaf(input)?;
    ensure_safe_trash_target(&resolved)?;
    std::fs::symlink_metadata(&resolved)?;
    let original_path = resolved.display().to_string();
    move_to_trash(&resolved)?;
    if std::fs::symlink_metadata(&resolved).is_ok() {
        bail!("{}", "the path is still present after the move");
    }
    Ok(original_path)
}

pub(in crate::tools) fn paths_arg(args: &Value) -> Result<Vec<PathBuf>> {
    let Some(values) = args.get("paths").and_then(Value::as_array) else {
        bail!("{}", "paths must be an array of path strings");
    };
    let paths = values
        .iter()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(expand_path)
        .collect::<Vec<_>>();
    if paths.is_empty() {
        bail!("{}", "paths must contain at least one path");
    }
    Ok(paths)
}

/// `rg` 跑不起来时，把 `os error 2` 换成说得清的话。
///
/// 09-22 在 macOS 上实测：那台机器没有 ripgrep，模型调 `grep` / `glob` 收到的
/// 整句话就是 `No such file or directory (os error 2)`——连缺的是哪个程序都没说。
/// Linux 上看不见是因为 Arch 包把 `ripgrep` 写进了依赖。
fn ripgrep_output(result: std::io::Result<std::process::Output>) -> Result<std::process::Output> {
    result.map_err(
        |error| match miyu_base::process::missing_program("rg", &error) {
            Some(hint) => anyhow::anyhow!("{hint}"),
            None => anyhow::Error::from(error),
        },
    )
}

pub(in crate::tools) async fn glob_files(args: Value) -> Result<String> {
    let path = optional_path(&args).unwrap_or_else(miyu_base::workspace::effective_workdir);
    miyu_base::sandbox::guard_read(&path)?;
    let search_path = prepare_search_path(&path)?;
    let pattern = required(&args, "pattern")?;
    let max_results = max_results(&args);
    let mut command = Command::new("rg");
    command
        .arg("--no-config")
        .arg("--files")
        .arg("--no-messages")
        .arg("--hidden")
        .arg(format!("--iglob={pattern}"))
        .args(search_exclude_args(&search_path))
        .arg(".")
        .current_dir(&search_path)
        .stdin(Stdio::null())
        // 超时丢弃 future 时同步回收 rg,否则孤儿进程继续扫整盘。
        .kill_on_drop(true);
    miyu_base::sandbox::confine(&mut command);
    let output = ripgrep_output(
        tokio::time::timeout(
            std::time::Duration::from_secs(SEARCH_TIMEOUT_SECONDS),
            command.output(),
        )
        .await?,
    )?;
    search_output_limited(output, max_results)
}

pub(in crate::tools) async fn grep_text(args: Value) -> Result<String> {
    let path = optional_path(&args).unwrap_or_else(miyu_base::workspace::effective_workdir);
    miyu_base::sandbox::guard_read(&path)?;
    let is_file = path.is_file();
    let search_root = if is_file {
        path.parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf()
    } else {
        path.clone()
    };
    let search_root = prepare_search_path(&search_root)?;
    let pattern = required(&args, "pattern")?;
    let max_results = max_results(&args);
    let mut command = Command::new("rg");
    miyu_base::sandbox::confine(&mut command);
    command
        .arg("--no-config")
        .arg("--line-number")
        .arg("--no-messages")
        .arg("--hidden")
        .args(search_exclude_args(&search_root))
        .arg(pattern);
    if let Some(include) = args
        .get("include")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
    {
        command.arg("--iglob").arg(include.trim());
    }
    if is_file {
        if let Some(name) = path.file_name() {
            command.arg(name);
        }
    } else {
        command.arg(".");
    }
    let output = ripgrep_output(
        tokio::time::timeout(
            std::time::Duration::from_secs(SEARCH_TIMEOUT_SECONDS),
            command
                .current_dir(search_root)
                .stdin(Stdio::null())
                .kill_on_drop(true)
                .output(),
        )
        .await?,
    )?;
    search_output_limited(output, max_results)
}
