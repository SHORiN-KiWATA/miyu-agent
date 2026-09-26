//! 补丁与 diff 的渲染。
//!
//! 只认 unified diff 的 hunk 头（`parse_diff_hunk_header`），解析不出来就原样
//! 打——工具产出的 diff 格式不总是规范的，猜错不如不猜。

use crate::render::*;

pub(crate) fn write_tool_payload(
    stdout: &mut impl Write,
    label: &str,
    payload: &str,
) -> Result<()> {
    let formatted = format_tool_payload(payload);
    writeln!(stdout, "\x1b[2m{label}:\x1b[0m")?;
    for line in formatted.lines() {
        writeln!(stdout, "\x1b[2m  {line}\x1b[0m")?;
    }
    Ok(())
}

pub(crate) fn write_patch_result(stdout: &mut impl Write, output: &str) -> Result<bool> {
    let Ok(value) = serde_json::from_str::<Value>(output.trim()) else {
        return Ok(false);
    };
    let path = value.get("path").and_then(Value::as_str).unwrap_or("file");
    let diff = value.get("diff").and_then(Value::as_str).unwrap_or("");
    if diff.trim().is_empty() {
        return Ok(false);
    }
    write!(stdout, "{}", render_patch_diff(path, diff))?;
    Ok(true)
}

/// 这次编辑加了几行、删了几行。
///
/// 抬头那一行光有路径,看不出改动大小;`+3 -1` 一眼就知道是顺手一改还是大手术。
/// 数的是 diff 正文里的 `+`/`-` 行,`+++`/`---` 那两行文件头不算。一个补丁改好
/// 几个文件时**合计**(抬头上的路径已经是「第一个 +2 项」的合计口径,统计跟着
/// 合计才自洽;用户 09-17 裁定)。
pub(crate) fn diff_stat(diff: &str) -> Option<(usize, usize)> {
    miyu_engine::tools::diff_stat(diff)
}

/// 补丁预览 JSON(`{path, diff}`)里的加减行数。
pub(crate) fn preview_diff_stat(output: &str) -> Option<(usize, usize)> {
    let value = serde_json::from_str::<Value>(output.trim()).ok()?;
    diff_stat(value.get("diff").and_then(Value::as_str)?)
}

/// `+3 -1`,加绿减红。
///
/// 收色用 SGR 39(恢复默认前景)而不是 `0`:抬头整行被包在 `\x1b[2m…\x1b[0m` 里,
/// 用 0 会把后面的 dim 一起关掉,半行亮半行暗。
pub fn diff_stat_label(added: usize, removed: usize) -> String {
    format!("\x1b[32m+{added}\x1b[39m \x1b[31m-{removed}\x1b[39m")
}

/// 补丁预览切成可展开的行。解析不出 diff 就返回 `None`（没什么可展开的）。
pub(crate) fn patch_preview_lines(output: &str, width: usize) -> Option<Vec<String>> {
    let value = serde_json::from_str::<Value>(output.trim()).ok()?;
    let path = value.get("path").and_then(Value::as_str).unwrap_or("file");
    let diff = value.get("diff").and_then(Value::as_str).unwrap_or("");
    if diff.trim().is_empty() {
        return None;
    }
    // 宽度得**自己传**：这段 diff 之后还要整体缩进，按整屏宽折的话每一行都会
    // 多出几列，落到缓冲里被硬折一次，续行从第 0 列开始——就是那种"左边冒出
    // 半个字"的样子。表头也不要：时间线那一行已经把路径说过了。
    let lines = trim_blank_edges(render_patch_diff_at(path, diff, width, false));
    (!lines.is_empty()).then_some(lines)
}

/// 掐掉首尾空行。`render_patch_diff_at` 为了在整段输出里留呼吸空间自带首尾空行，
/// 而时间线那边 `step_detail` 也会在上下各留一行——两份加起来就是两行空白。
fn trim_blank_edges(rendered: String) -> Vec<String> {
    let mut lines: Vec<String> = rendered.lines().map(str::to_string).collect();
    while lines.first().is_some_and(|line| line.trim().is_empty()) {
        lines.remove(0);
    }
    while lines.last().is_some_and(|line| line.trim().is_empty()) {
        lines.pop();
    }
    lines
}

/// apply_patch 的**信封**（`*** Begin Patch … *** End Patch`）渲染成 diff。
///
/// 工具自己跑的时候会用改前改后算出真 diff 走 `__patch_preview__`，但那条路只
/// 到发起它的那个渲染器。子代理内层的编辑没有这条路——面板那边手上只有调用参数
/// 里的这份信封（用户实测：编辑文件工具展开后是一团原始 JSON）。信封本身就是
/// `+`/`-`/上下文 的形状，按同一套着色规则画出来就是能读的 diff。
///
/// 一个信封可以改好几个文件，逐段渲染。
/// 调用参数里带着 apply_patch 信封吗——带就画成 diff。
pub fn patch_envelope_lines_from_args(
    tool: &str,
    arguments: &str,
    width: usize,
) -> Option<Vec<String>> {
    if !matches!(
        crate::render::tool_event_base_name(tool),
        "edit" | "kb" | "artifact" | "apply_patch" | "apply_artifact_patch"
    ) {
        return None;
    }
    let args = serde_json::from_str::<Value>(arguments.trim()).ok()?;
    let patch = args
        .get("patchText")
        .or_else(|| args.get("patch_text"))
        .and_then(Value::as_str)?;
    patch_envelope_lines(patch, width)
}

/// 调用参数里补丁信封的加减行数（`+3 -1` 那个量），和真 diff 同一个数法。
pub(crate) fn patch_envelope_stat_from_args(tool: &str, arguments: &str) -> Option<(usize, usize)> {
    if !matches!(
        crate::render::tool_event_base_name(tool),
        "edit" | "kb" | "artifact" | "apply_patch" | "apply_artifact_patch"
    ) {
        return None;
    }
    let args = serde_json::from_str::<Value>(arguments.trim()).ok()?;
    let patch = args
        .get("patchText")
        .or_else(|| args.get("patch_text"))
        .and_then(Value::as_str)?;
    diff_stat(patch)
}

/// 新建文件那一段补的 hunk 头：只为让行号从 1 起，渲染时本身不出字。
const NEW_FILE_HUNK: &str = "@@ -0,0 +1 @@";

pub(crate) fn patch_envelope_lines(patch: &str, width: usize) -> Option<Vec<String>> {
    let mut sections: Vec<(String, Vec<String>)> = Vec::new();
    for line in patch.lines() {
        if let Some(path) = line.strip_prefix("*** Add File: ") {
            // 新建的文件从第 1 行起：补一个 hunk 头，行号才对得上（信封里没有行号）。
            sections.push((path.trim().to_string(), vec![NEW_FILE_HUNK.to_string()]));
            continue;
        }
        if let Some(path) = line
            .strip_prefix("*** Update File: ")
            .or_else(|| line.strip_prefix("*** Delete File: "))
        {
            sections.push((path.trim().to_string(), Vec::new()));
            continue;
        }
        // 信封自己的指令行不是内容。
        if line.starts_with("*** ") {
            continue;
        }
        if let Some((_, body)) = sections.last_mut() {
            body.push(line.to_string());
        }
    }
    // 只改一个文件时不带文件头：时间线那一行已经写过路径了（和 `patch_preview_lines` 一个口径）。
    // 改好几个文件时逐个带上，不然分不清哪段是哪个。
    let heading = sections.len() > 1;
    let mut out: Vec<String> = Vec::new();
    for (path, body) in sections {
        // 「删除文件」那种信封里一行内容都没有——那也是一条信息，得说出来。
        let diff = if body
            .iter()
            .all(|line| line.trim().is_empty() || line == NEW_FILE_HUNK)
        {
            String::new()
        } else {
            body.join("\n")
        };
        if !out.is_empty() {
            out.push(String::new());
        }
        if diff.is_empty() {
            out.push(format!(
                "\x1b[2m{}  \x1b[38;5;250m{path}\x1b[0m",
                t("Deleted", "已删除")
            ));
            continue;
        }
        out.extend(trim_blank_edges(render_patch_diff_at(
            &path, &diff, width, heading,
        )));
    }
    (!out.is_empty()).then_some(out)
}

pub fn render_patch_diff(path: &str, diff: &str) -> String {
    let terminal_width = crate::render::content_cols(100);
    render_patch_diff_at(path, diff, terminal_width, true)
}

pub(crate) fn render_patch_diff_at(
    path: &str,
    diff: &str,
    terminal_width: usize,
    heading: bool,
) -> String {
    let mut output = String::new();
    // apply_patch 是唯一编辑器(增/改/删同一语义),标签按 diff 形态区分:
    // 纯 + 无上下文=新建,纯 - 无上下文=删除,其余=修改。
    let mut plus = false;
    let mut minus = false;
    let mut context = false;
    for line in diff.lines() {
        if line.starts_with("--- ") || line.starts_with("+++ ") || line.starts_with("@@") {
            continue;
        }
        match line.as_bytes().first() {
            Some(b'+') => plus = true,
            Some(b'-') => minus = true,
            Some(_) => context = true,
            None => {}
        }
    }
    let label = if plus && !minus && !context {
        t("Created", "已新建")
    } else if minus && !plus && !context {
        t("Deleted", "已删除")
    } else {
        t("Modified", "已修改")
    };
    if heading {
        output.push_str(&format!("\x1b[2m{label}  \x1b[38;5;250m{path}\x1b[0m\n\n"));
    }

    // 先把每一行算出来，再决定行号栏多宽。
    //
    // 行号栏原来固定四格右对齐：一位数的行号前面空三格，再叠上展开区自己的
    // 缩进，整段 diff 比旁边的思考正文、工具输出都往右缩了半截（用户实测：
    // 「diff 缩进和别的不太对，有点靠右」）。栏宽按**这一段里最大的行号**算，
    // 改三行的小文件就是一格，大文件才撑到三四格。
    enum Row<'a> {
        Gap,
        Line(usize, char, &'a str, &'static str),
    }
    let mut rows: Vec<Row<'_>> = Vec::new();
    let mut old_line = 0usize;
    let mut new_line = 0usize;
    let mut widest = 0usize;
    for raw_line in diff.lines() {
        if raw_line.starts_with("--- ") || raw_line.starts_with("+++ ") {
            continue;
        }
        if raw_line.starts_with("@@") {
            if let Some((old_start, new_start)) = parse_diff_hunk_header(raw_line) {
                old_line = old_start;
                new_line = new_start;
            }
            rows.push(Row::Gap);
            continue;
        }

        let (line_no, sign, body, style) = if let Some(body) = raw_line.strip_prefix('-') {
            let line_no = old_line;
            old_line += 1;
            (line_no, '-', body, PATCH_DELETE_STYLE)
        } else if let Some(body) = raw_line.strip_prefix('+') {
            let line_no = new_line;
            new_line += 1;
            (line_no, '+', body, PATCH_INSERT_STYLE)
        } else if let Some(body) = raw_line.strip_prefix(' ') {
            let line_no = new_line;
            old_line += 1;
            new_line += 1;
            (line_no, ' ', body, "\x1b[38;5;245m")
        } else {
            (new_line, ' ', raw_line, "\x1b[38;5;245m")
        };
        widest = widest.max(line_no);
        rows.push(Row::Line(line_no, sign, body, style));
    }
    let gutter = line_number_gutter(widest);
    for row in rows {
        match row {
            Row::Gap => {
                if !output.ends_with("\n\n") {
                    output.push('\n');
                }
            }
            Row::Line(line_no, sign, body, style) => {
                push_patch_diff_line(
                    &mut output,
                    line_no,
                    sign,
                    body,
                    style,
                    terminal_width,
                    gutter,
                );
            }
        }
    }
    output.push('\n');
    output
}

/// 行号栏宽：放得下这一段里最大的行号就行。最少两格——一格的行号贴着符号
/// 显得挤，而两格也只比正文多退一个字。
fn line_number_gutter(widest_line_no: usize) -> usize {
    widest_line_no.max(1).to_string().len().max(2)
}

pub(crate) fn push_patch_diff_line(
    output: &mut String,
    line_no: usize,
    sign: char,
    body: &str,
    style: &str,
    terminal_width: usize,
    gutter: usize,
) {
    // 行号 + 符号 + 正文，**没有竖线**：符号那一列已经把增删说清楚了，再加一根
    // 分隔线只是把正文往右推两格、和别的展开内容对不上（用户拍板：不需要左侧竖线）。
    let first_prefix = format!("\x1b[38;5;102m{line_no:>gutter$}\x1b[0m {style}{sign} ");
    let continuation_prefix = format!("\x1b[38;5;102m{:gutter$}\x1b[0m {style}  ", "");
    let prefix_width = visible_width(&first_prefix);
    let body_width = terminal_width.saturating_sub(prefix_width + 1).max(1);
    let wrapped = wrap_ansi_text(body, body_width);

    for (index, segment) in wrapped.iter().enumerate() {
        if index == 0 {
            output.push_str(&first_prefix);
        } else {
            output.push_str(&continuation_prefix);
        }
        output.push_str(segment);
        output.push_str("\x1b[0m\n");
    }
}

pub(crate) fn parse_diff_hunk_header(header: &str) -> Option<(usize, usize)> {
    let mut parts = header.split_whitespace();
    parts.next()?;
    let old_part = parts.next()?.trim_start_matches('-');
    let new_part = parts.next()?.trim_start_matches('+');
    Some((
        parse_diff_range_start(old_part)?,
        parse_diff_range_start(new_part)?,
    ))
}

pub(crate) fn parse_diff_range_start(value: &str) -> Option<usize> {
    value.split(',').next()?.parse().ok()
}

pub(crate) fn format_tool_payload(payload: &str) -> String {
    let text = payload.trim();
    let formatted = serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|value| serde_json::to_string_pretty(&value).ok())
        .unwrap_or_else(|| text.to_string());
    truncate_chars(&formatted, 2400)
}
