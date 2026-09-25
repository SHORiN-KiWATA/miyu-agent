//! Markdown 的流式渲染。
//!
//! 难点是**流式**：内容一个 chunk 一个 chunk 地来，但 Markdown 的很多结构要看
//! 到行尾甚至下一行才能确定（表格要看分隔行，代码块要看闭合围栏）。所以按行
//! 缓冲、行完整了才渲染，`MarkdownLineRenderer` 持有跨行的状态。
//!
//! 行内解析（`render_inline`）是手写的扫描而不是通用 Markdown 库：这里只需要
//! 支持实际会出现的一小撮语法，而且要能在**任意位置被截断**后继续。

use crate::render::*;

pub fn print_assistant_response(response: &ChatResult, show_reasoning: bool) -> Result<()> {
    if show_reasoning {
        if let Some(reasoning) = response
            .reasoning
            .as_deref()
            .filter(|text| !text.trim().is_empty())
        {
            print_reasoning(reasoning)?;
        }
    }
    print_markdown(&response.content);
    Ok(())
}

pub fn print_markdown(markdown: &str) {
    let skin = termimad::MadSkin::default();
    println!("{}", skin.term_text(markdown.trim_end()));
}

pub(crate) struct MarkdownStreamRenderer {
    pub(crate) buffer: String,
    pub(crate) line_renderer: MarkdownLineRenderer,
}

impl MarkdownStreamRenderer {
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
            line_renderer: MarkdownLineRenderer::new(),
        }
    }

    pub fn push(&mut self, delta: &str) -> String {
        self.buffer.push_str(delta);
        let mut output = String::new();
        while let Some(index) = self.buffer.find('\n') {
            let line = self.buffer[..index].to_string();
            self.buffer = self.buffer[index + 1..].to_string();
            output.push_str(&self.line_renderer.render_line(&line));
        }
        output
    }

    pub(crate) fn flush(&mut self) -> String {
        let mut output = String::new();
        if !self.buffer.is_empty() {
            let line = std::mem::take(&mut self.buffer);
            output.push_str(&self.line_renderer.render_line(&line));
        }
        output.push_str(&self.line_renderer.flush());
        output
    }

    /// 还没落下的那一截此刻的样子，一项一行（没折行）。正文的活尾巴用（09-25）。
    ///
    /// 没有副作用：不画 mermaid 图、不出公式图片、不动任何状态——那几样只在真落下时做。
    /// 没闭合的代码块按最终那套渲（框宽照整块算，只渲最后 `max_rows` 行），mermaid 也先
    /// 露源码；没闭合的公式照原样；进行中的表格那一行按表格行渲；还没定下来是不是表格的
    /// 几行和半行都按普通正文渲。
    pub(crate) fn pending_preview(&self, max_rows: usize) -> Vec<String> {
        let state = &self.line_renderer;
        let partial = Some(preview_tail(&self.buffer)).filter(|line| !line.is_empty());
        if state.in_code_block {
            return render_code_block_tail(&state.code_lang, &state.code_buffer, partial, max_rows);
        }
        let mut rows = Vec::new();
        if state.in_math_block {
            let opener = if state.math_closer == "$$" {
                "$$"
            } else {
                "\\["
            };
            rows.push(format!("\x1b[36m{opener}\x1b[0m"));
            for line in state.math_buffer.iter().map(String::as_str).chain(partial) {
                rows.push(format!("\x1b[36m{line}\x1b[0m"));
            }
            return rows;
        }
        rows.extend(
            state
                .table_buffer
                .iter()
                .map(|line| render_markdown_line(line)),
        );
        if let Some(line) = partial {
            match &state.active_table {
                Some(table) if looks_like_table_row(line) => {
                    rows.push(middle_table_border(&table.widths).trim_end().to_string());
                    let row = parse_table_row(line);
                    rows.extend(
                        render_table_row(&row, &table.widths, &table.alignments, false)
                            .lines()
                            .map(str::to_string),
                    );
                }
                _ => rows.push(render_markdown_line(line)),
            }
        }
        rows
    }

    /// 还没落下的那一截的指纹：它没变，活尾巴就不用重算。
    ///
    /// 各缓冲只在这一截里往后长，落下一段时渲染器自己把尾巴重画一遍，所以长度加半行的
    /// 内容就分得清。
    pub(crate) fn pending_fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let state = &self.line_renderer;
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.buffer.hash(&mut hasher);
        (
            state.in_code_block,
            state.in_math_block,
            state.code_buffer.len(),
            state.table_buffer.len(),
            state.math_buffer.len(),
            state.active_table.is_some(),
        )
            .hash(&mut hasher);
        hasher.finish()
    }
}

/// 预览只看一行的最后这么多字。模型要是吐一整段不带换行的超长文字（压缩过的代码、
/// 一大段 JSON），每一拍整段重渲会把一个核吃满——思考滚动窗的 `ThoughtRows` 就是这么
/// 来的。露出来的只有最后几行，截掉的部分落下时照样整段渲。
pub(crate) const PREVIEW_LINE_CHARS: usize = 8192;

/// 一行的最后 [`PREVIEW_LINE_CHARS`] 个字（落在字符边界上）。
pub(crate) fn preview_tail(line: &str) -> &str {
    match line.char_indices().rev().nth(PREVIEW_LINE_CHARS - 1) {
        Some((start, _)) if start > 0 => &line[start..],
        _ => line,
    }
}

pub(crate) struct MarkdownLineRenderer {
    pub(crate) in_code_block: bool,
    pub(crate) in_math_block: bool,
    pub(crate) code_lang: String,
    pub(crate) code_buffer: Vec<String>,
    pub(crate) table_buffer: Vec<String>,
    pub(crate) active_table: Option<ActiveTable>,
    pub(crate) math_buffer: Vec<String>,
    /// 当前块级公式的闭合定界符("$$" 或 "\\]")。
    pub(crate) math_closer: &'static str,
}

impl MarkdownLineRenderer {
    pub fn new() -> Self {
        Self {
            in_code_block: false,
            in_math_block: false,
            code_lang: String::new(),
            code_buffer: Vec::new(),
            table_buffer: Vec::new(),
            active_table: None,
            math_buffer: Vec::new(),
            math_closer: "$$",
        }
    }

    /// 围栏闭合时这一段怎么出。
    ///
    /// mermaid 围栏先试着画成图(终端图片协议);画不出来——语法不对、图型不支持、
    /// 终端不认图片——就照常打那个带语法高亮的代码块。与数学公式同规矩:
    /// 渲染失败永不阻断输出。
    fn close_code_block(&mut self) -> String {
        let lang = std::mem::take(&mut self.code_lang);
        let body = std::mem::take(&mut self.code_buffer);
        if super::mermaid::is_mermaid_lang(&lang) {
            if let Some(block) = super::mermaid::render_terminal(&body.join("\n")) {
                return block;
            }
        }
        render_code_block(&lang, &body)
    }

    pub(crate) fn render_line(&mut self, line: &str) -> String {
        if line.trim_start().starts_with("```") {
            if self.in_code_block {
                self.in_code_block = false;
                let code = self.close_code_block();
                return code;
            }
            let pending = self.flush();
            self.in_code_block = true;
            self.code_lang = line
                .trim_start()
                .trim_start_matches('`')
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .to_string();
            self.code_buffer.clear();
            return pending;
        }
        if self.in_code_block {
            self.code_buffer.push(line.to_string());
            return String::new();
        }
        if self.in_math_block {
            let trimmed = line.trim();
            if trimmed == self.math_closer || trimmed.ends_with(self.math_closer) {
                if trimmed != self.math_closer {
                    self.math_buffer
                        .push(trimmed[..trimmed.len() - self.math_closer.len()].to_string());
                }
                self.in_math_block = false;
                let tex = std::mem::take(&mut self.math_buffer).join("\n");
                return render_display_math(&tex, self.math_closer);
            }
            self.math_buffer.push(line.to_string());
            return String::new();
        }
        {
            let trimmed = line.trim();
            let opener = if trimmed.starts_with("$$") {
                Some(("$$", "$$"))
            } else if trimmed.starts_with("\\[") {
                Some(("\\[", "\\]"))
            } else {
                None
            };
            if let Some((open, close)) = opener {
                let pending = self.flush();
                let inner = &trimmed[open.len()..];
                // 单行闭合:$$E=mc^2$$ / \[x\]
                if let Some(tex) = inner.strip_suffix(close) {
                    if !tex.trim().is_empty() {
                        return format!("{pending}{}", render_display_math(tex, close));
                    }
                }
                self.in_math_block = true;
                self.math_closer = close;
                self.math_buffer.clear();
                if !inner.trim().is_empty() {
                    self.math_buffer.push(inner.to_string());
                }
                return pending;
            }
        }
        if let Some(table) = &self.active_table {
            if looks_like_table_row(line) {
                let row = parse_table_row(line);
                let mut output = middle_table_border(&table.widths);
                output.push_str(&render_table_row(
                    &row,
                    &table.widths,
                    &table.alignments,
                    false,
                ));
                return output;
            }
            let mut output = bottom_table_border(&table.widths);
            self.active_table = None;
            output.push_str(&self.render_line(line));
            return output;
        }
        if looks_like_table_row(line) {
            self.table_buffer.push(line.to_string());
            if self.table_buffer.len() < 3 {
                return String::new();
            }
            let second = self.table_buffer.get(1).cloned().unwrap_or_default();
            if is_table_separator(&second) {
                let header =
                    parse_table_row(self.table_buffer.first().map(String::as_str).unwrap_or(""));
                let alignments = parse_table_alignments(&second);
                let first_row =
                    parse_table_row(self.table_buffer.get(2).map(String::as_str).unwrap_or(""));
                let widths = table_widths_for_rows(&[header.clone(), first_row.clone()]);
                self.table_buffer.clear();
                self.active_table = Some(ActiveTable {
                    widths: widths.clone(),
                    alignments: alignments.clone(),
                });
                let mut output = top_table_border(&widths);
                output.push_str(&render_table_row(&header, &widths, &alignments, true));
                output.push_str(&middle_table_border(&widths));
                output.push_str(&render_table_row(&first_row, &widths, &alignments, false));
                return output;
            }
            return self.flush();
        }
        let mut output = self.flush();
        output.push_str(&render_markdown_line(line));
        output.push('\n');
        output
    }

    pub(crate) fn flush(&mut self) -> String {
        if self.in_math_block {
            // 流结束仍未闭合:按原样回放,不吞内容。
            self.in_math_block = false;
            let opener = if self.math_closer == "$$" {
                "$$"
            } else {
                "\\["
            };
            let mut output = format!("\x1b[36m{opener}\x1b[0m\n");
            for line in std::mem::take(&mut self.math_buffer) {
                output.push_str(&format!("\x1b[36m{line}\x1b[0m\n"));
            }
            return output;
        }
        if self.in_code_block {
            self.in_code_block = false;
            return self.close_code_block();
        }
        if let Some(table) = self.active_table.take() {
            return bottom_table_border(&table.widths);
        }
        if self.table_buffer.is_empty() {
            return String::new();
        }
        let lines = std::mem::take(&mut self.table_buffer);
        if lines.len() >= 2 && is_table_separator(lines.get(1).map(String::as_str).unwrap_or("")) {
            render_table(&lines)
        } else {
            let mut output = String::new();
            for line in lines {
                output.push_str(&render_markdown_line(&line));
                output.push('\n');
            }
            output
        }
    }
}

/// 块级公式:kitty 家族终端走图形协议(高清,复用 print_image 管线),
/// 其余终端半块画;渲染失败原样回放(青色+定界符)。
pub(crate) fn render_display_math(tex: &str, closer: &str) -> String {
    let terminal_rows = miyu_base::terminal::content_viewport()
        .map(|(_, rows)| rows)
        .unwrap_or_else(|| terminal::size().map(|(_, rows)| rows).unwrap_or(24));
    let terminal_cols = crate::render::content_cols(100);
    let max_cols = terminal_cols.saturating_sub(6).clamp(24, 110);
    // 垂直方向此前没有任何上限——只约束宽度，行数由调用方写死为 9。
    // 上限取 8 与 kitty 那条路对齐，再按终端高度收一道，矮窗口里一条公式
    // 不该占掉半屏。
    let max_rows = (terminal_rows as usize / 4).clamp(2, 8);
    if math::kitty_graphics_supported() {
        if let Some(kitty) = math::render_math_kitty(tex, max_cols) {
            // 占位行自带换行,逐行加两格缩进(图形转义段无换行,不受影响);
            // 首尾补空行,与正文拉开呼吸感。
            let mut output = String::from("\n");
            for line in kitty.sequence.split_inclusive('\n') {
                output.push_str("  ");
                output.push_str(line);
            }
            output.push('\n');
            return output;
        }
    }
    // 非 kitty 终端交给 chafa:它认得 Konsole/WezTerm/foot/iTerm2 之流,能
    // 出真图。图片工具一直走这条路,公式此前却只有半块——同一个终端里图片
    // 是图、公式是像素块,就是这么来的。chafa 缺失或失败才退半块。
    if let Some(art) = math::render_math_chafa(tex, max_cols, max_rows) {
        let mut output = String::from("\n");
        for line in &art.lines {
            output.push_str("  ");
            output.push_str(line);
            output.push('\n');
        }
        // 与半块分支一致:首尾各留一个空行,和正文拉开呼吸感。
        output.push('\n');
        return output;
    }
    if let Some(art) = math::render_block_math(tex, max_cols, max_rows) {
        let mut output = String::from("\n");
        for line in art.lines {
            output.push_str("  ");
            output.push_str(&line);
            output.push('\n');
        }
        output.push('\n');
        return output;
    }
    let opener = if closer == "$$" { "$$" } else { "\\[" };
    let closing = if closer == "$$" { "$$" } else { "\\]" };
    let mut output = format!("\x1b[36m{opener}\x1b[0m\n");
    for line in tex.lines() {
        output.push_str(&format!("\x1b[36m{line}\x1b[0m\n"));
    }
    output.push_str(&format!("\x1b[36m{closing}\x1b[0m\n"));
    output
}

pub fn render_markdown_line(line: &str) -> String {
    let trimmed = line.trim_start();
    let indent = &line[..line.len() - trimmed.len()];
    if let Some(header) = render_header(trimmed) {
        return header;
    }
    if let Some((depth, rest)) = parse_blockquote(trimmed) {
        let bars = "\x1b[32m| \x1b[0m".repeat(depth);
        return format!("{indent}{bars}\x1b[32m{}\x1b[0m", render_inline(rest));
    }
    if let Some(rest) = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
        .or_else(|| trimmed.strip_prefix("+ "))
    {
        return format!(
            "{indent}{TERTIARY_STYLE}-{RESET} {}",
            render_line_body(rest)
        );
    }
    let digits = trimmed.chars().take_while(|ch| ch.is_ascii_digit()).count();
    if digits > 0
        && trimmed.as_bytes().get(digits) == Some(&b'.')
        && trimmed.as_bytes().get(digits + 1) == Some(&b' ')
    {
        let marker = &trimmed[..=digits];
        let rest = &trimmed[digits + 2..];
        return format!(
            "{indent}{TERTIARY_STYLE}{marker}{RESET} {}",
            render_line_body(rest)
        );
    }
    if is_horizontal_rule(trimmed) {
        return horizontal_rule();
    }
    render_line_body(line)
}

/// 行的正文部分。先看整行是不是「标题 (地址)」——命中就整行成链，
/// 否则退回逐字符的行内解析。
pub(crate) fn render_line_body(text: &str) -> String {
    render_title_url_line(text).unwrap_or_else(|| render_inline(text))
}

pub(crate) fn parse_blockquote(line: &str) -> Option<(usize, &str)> {
    let mut depth = 0;
    let mut rest = line;
    while let Some(stripped) = rest.strip_prefix('>') {
        depth += 1;
        rest = stripped.strip_prefix(' ').unwrap_or(stripped);
    }
    (depth > 0).then_some((depth, rest))
}

pub(crate) fn render_header(line: &str) -> Option<String> {
    let level = line.chars().take_while(|ch| *ch == '#').count();
    if level == 0 || level > 6 || line.as_bytes().get(level) != Some(&b' ') {
        return None;
    }
    let prefix = "#".repeat(level);
    Some(format!(
        "{HEADER_STYLE}{prefix} {}{RESET}",
        render_inline(&line[level + 1..])
    ))
}

pub(crate) fn render_inline(text: &str) -> String {
    let mut output = String::new();
    let chars = text.chars().collect::<Vec<_>>();
    let mut index = 0;
    while index < chars.len() {
        // 行内公式 $…$ / $$…$$:Unicode 转写(xₙ₊₁、√π、α∈(0,1))。
        // 单 $ 启发式同 WebUI:内容非空、两端非空格、右侧不接数字(放过价格)。
        if chars[index] == '$' {
            let double = chars.get(index + 1) == Some(&'$');
            let open = if double { index + 2 } else { index + 1 };
            let close = if double {
                find_double_dollar(&chars, open)
            } else {
                find_marker(&chars, open, '$')
            };
            if let Some(end) = close {
                let tex: String = chars[open..end].iter().collect();
                let accept = !tex.trim().is_empty()
                    && (double
                        || (!tex.starts_with(' ')
                            && !tex.ends_with(' ')
                            && !chars.get(end + 1).is_some_and(|next| next.is_ascii_digit())));
                if accept {
                    output.push_str(PRIMARY_STYLE);
                    output.push_str(&math::unicode_math(&tex));
                    output.push_str(RESET);
                    index = end + if double { 2 } else { 1 };
                    continue;
                }
            }
        }
        if chars[index] == '\\' && chars.get(index + 1) == Some(&'(') {
            let mut probe = index + 2;
            let mut closing = None;
            while probe + 1 < chars.len() {
                if chars[probe] == '\\' && chars[probe + 1] == ')' {
                    closing = Some(probe);
                    break;
                }
                probe += 1;
            }
            if let Some(end) = closing {
                let tex: String = chars[index + 2..end].iter().collect();
                output.push_str(PRIMARY_STYLE);
                output.push_str(&math::unicode_math(&tex));
                output.push_str(RESET);
                index = end + 2;
                continue;
            }
        }
        if index + 1 < chars.len() && chars[index] == '!' && chars[index + 1] == '[' {
            if let Some(label_end) = find_marker(&chars, index + 2, ']') {
                if chars.get(label_end + 1) == Some(&'(') {
                    if let Some(url_end) = find_marker(&chars, label_end + 2, ')') {
                        let alt = chars[index + 2..label_end].iter().collect::<String>();
                        output.push_str(IMAGE_STYLE);
                        output.push_str("[image");
                        if !alt.is_empty() {
                            output.push_str(": ");
                            output.push_str(&alt);
                        }
                        output.push_str("]");
                        output.push_str(RESET);
                        output.push('(');
                        output.push_str(&render_url(
                            &chars[label_end + 2..url_end].iter().collect::<String>(),
                        ));
                        output.push(')');
                        index = url_end + 1;
                        continue;
                    }
                }
            }
        }
        if chars[index] == '`' {
            if let Some(end) = find_marker(&chars, index + 1, '`') {
                output.push_str(INLINE_CODE_STYLE);
                output.extend(chars[index + 1..end].iter());
                output.push_str(RESET);
                index = end + 1;
                continue;
            }
        }
        if index + 1 < chars.len() && chars[index] == '~' && chars[index + 1] == '~' {
            if let Some(end) = find_double_marker(&chars, index + 2, '~') {
                output.push_str(STRIKE_STYLE);
                output.extend(chars[index + 2..end].iter());
                output.push_str(RESET);
                index = end + 2;
                continue;
            }
        }
        if index + 1 < chars.len() && chars[index] == '*' && chars[index + 1] == '*' {
            if let Some(end) = find_double_marker(&chars, index + 2, '*') {
                output.push_str(BOLD_STYLE);
                output.extend(chars[index + 2..end].iter());
                output.push_str(RESET);
                index = end + 2;
                continue;
            }
        }
        if chars[index] == '*' {
            if let Some(end) = find_marker(&chars, index + 1, '*') {
                output.push_str(ITALIC_STYLE);
                output.extend(chars[index + 1..end].iter());
                output.push_str(RESET);
                index = end + 1;
                continue;
            }
        }
        if chars[index] == '_' {
            if is_emphasis_start(&chars, index) {
                if let Some(end) = find_emphasis_end(&chars, index + 1, '_') {
                    output.push_str(ITALIC_STYLE);
                    output.extend(chars[index + 1..end].iter());
                    output.push_str(RESET);
                    index = end + 1;
                    continue;
                }
            }
        }
        if chars[index] == '[' {
            if let Some(label_end) = find_marker(&chars, index + 1, ']') {
                if chars.get(label_end + 1) == Some(&'(') {
                    if let Some(url_end) = find_marker(&chars, label_end + 2, ')') {
                        let label = chars[index + 1..label_end].iter().collect::<String>();
                        let url = chars[label_end + 2..url_end].iter().collect::<String>();
                        // 标签不再往下解析：整段要挂同一个 OSC 8，里面再嵌一层
                        // 链接转义只会互相打架，而标签里出现 Markdown 的情况罕见。
                        let inner = format!(
                            "{LINK_LABEL_STYLE}{label}{RESET} {}",
                            render_url_wrapped(&url)
                        );
                        output.push_str(&hyperlink(url.trim(), &inner));
                        index = url_end + 1;
                        continue;
                    }
                }
            }
        }
        if chars[index] == '<' {
            if let Some(end) = find_marker(&chars, index + 1, '>') {
                let value = chars[index + 1..end].iter().collect::<String>();
                if scheme_len(&value).is_some() {
                    let inner = format!("\x1b[4m{}{RESET}", render_url_wrapped(&value));
                    output.push_str(&hyperlink(&value, &inner));
                    index = end + 1;
                    continue;
                }
                if let Some(rendered) = render_html_tag(&value) {
                    output.push_str(&rendered);
                    index = end + 1;
                    continue;
                }
            }
        }
        // 裸地址。放在行内代码与 [](…) 之后：那两样先被吃掉，这里看不到它们的内容。
        if matches!(chars[index], 'h' | 'H' | 'f' | 'F') {
            if let Some(url) = bare_url_at(&chars, index) {
                output.push_str(&render_bare_url(&url));
                index += url.chars().count();
                continue;
            }
        }
        output.push(chars[index]);
        index += 1;
    }
    output
}

pub(crate) fn render_url(url: &str) -> String {
    format!("{URL_STYLE}{url}{RESET}")
}

pub(crate) fn render_url_wrapped(url: &str) -> String {
    format!("<{}>", render_url(url))
}

pub(crate) fn render_html_tag(tag: &str) -> Option<String> {
    match tag.trim().to_ascii_lowercase().as_str() {
        "u" => Some("\x1b[4m".to_string()),
        "/u" => Some("\x1b[0m".to_string()),
        "sub" => Some("\x1b[2m".to_string()),
        "/sub" => Some("\x1b[0m".to_string()),
        "sup" => Some("\x1b[1m".to_string()),
        "/sup" => Some("\x1b[0m".to_string()),
        "br" | "br/" | "br /" => Some("\n".to_string()),
        _ => None,
    }
}

pub(crate) fn horizontal_rule() -> String {
    let width = (crate::render::content_cols(72) / 3).clamp(16, 40);
    format!("\x1b[2m{}\x1b[0m", "─".repeat(width))
}

pub(crate) fn is_horizontal_rule(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.len() >= 3 && trimmed.chars().all(|ch| ch == '-')
}

pub(crate) fn find_marker(chars: &[char], start: usize, marker: char) -> Option<usize> {
    (start..chars.len()).find(|index| chars[*index] == marker)
}

pub(crate) fn find_double_dollar(chars: &[char], start: usize) -> Option<usize> {
    (start..chars.len().saturating_sub(1))
        .find(|index| chars[*index] == '$' && chars[*index + 1] == '$')
}

pub(crate) fn find_emphasis_end(chars: &[char], start: usize, marker: char) -> Option<usize> {
    (start..chars.len()).find(|index| chars[*index] == marker && is_emphasis_end(chars, *index))
}

pub(crate) fn is_emphasis_start(chars: &[char], index: usize) -> bool {
    !chars
        .get(index.wrapping_sub(1))
        .is_some_and(|ch| is_word_char(*ch))
        && chars
            .get(index + 1)
            .is_some_and(|ch| !ch.is_whitespace() && *ch != '_')
}

pub(crate) fn is_emphasis_end(chars: &[char], index: usize) -> bool {
    chars
        .get(index.wrapping_sub(1))
        .is_some_and(|ch| !ch.is_whitespace() && *ch != '_')
        && !chars.get(index + 1).is_some_and(|ch| is_word_char(*ch))
}

pub(crate) fn is_word_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric()
}

pub(crate) fn find_double_marker(chars: &[char], start: usize, marker: char) -> Option<usize> {
    (start..chars.len().saturating_sub(1))
        .find(|index| chars[*index] == marker && chars[index + 1] == marker)
}

/// 一段文本占多少显示列。转义序列不算。
///
/// 以前只认「ESC 到 `m` 为止」，于是 OSC（以 BEL 结尾）会把它之后的整行都
/// 当成转义吞掉，宽度算成 0。全屏 TUI 把块标记（OSC）放进了行里，这条必须对。
pub fn visible_width(text: &str) -> usize {
    let mut width = 0;
    let mut rest = text;
    while !rest.is_empty() {
        if let Some(len) = escape_len(rest) {
            rest = &rest[len..];
            continue;
        }
        let ch = rest.chars().next().unwrap_or('\0');
        width += char_display_width(ch);
        rest = &rest[ch.len_utf8()..];
    }
    width
}

/// 去转义序列、认转义段长度:文本原语已下沉到 `miyu_base::terminal::text`,
/// 这里保留老路径 `crate::render::{strip_ansi_text, escape_len}`(09-16)。
pub use miyu_base::terminal::{escape_len, strip_ansi_text};
