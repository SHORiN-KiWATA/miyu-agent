//! 代码块的框线与语法着色。
//!
//! 着色是**词法级**的近似：认关键字、字符串、数字、注释，不做语法分析。对终端
//! 里瞄一眼的场景够用，而且不用为每种语言引一个解析器。

use crate::render::*;
use unicode_width::UnicodeWidthStr;

pub fn highlight_code_line(lang: &str, line: &str) -> String {
    let lang = lang.trim().to_ascii_lowercase();
    if lang.is_empty() {
        return line.to_string();
    }
    let comment_marker = match lang.as_str() {
        "py" | "python" | "sh" | "bash" | "zsh" | "fish" | "toml" | "yaml" | "yml" => Some('#'),
        "rs" | "rust" | "js" | "ts" | "tsx" | "jsx" | "c" | "cpp" | "java" | "go" => None,
        _ => None,
    };
    let mut output = String::new();
    let chars = line.chars().collect::<Vec<_>>();
    let mut index = 0;
    while index < chars.len() {
        if let Some(marker) = comment_marker {
            if chars[index] == marker {
                output.push_str(CODE_COMMENT_STYLE);
                output.extend(chars[index..].iter());
                output.push_str(CODE_TOKEN_RESET);
                return output;
            }
        }
        if index + 1 < chars.len() && chars[index] == '/' && chars[index + 1] == '/' {
            output.push_str(CODE_COMMENT_STYLE);
            output.extend(chars[index..].iter());
            output.push_str(CODE_TOKEN_RESET);
            return output;
        }
        if chars[index] == '"'
            || chars[index] == '\''
            || (chars[index] == '`'
                && matches!(lang.as_str(), "js" | "ts" | "tsx" | "jsx" | "sh" | "bash"))
        {
            let quote = chars[index];
            let start = index;
            index += 1;
            let mut escaped = false;
            while index < chars.len() {
                if escaped {
                    escaped = false;
                } else if chars[index] == '\\' {
                    escaped = true;
                } else if chars[index] == quote {
                    index += 1;
                    break;
                }
                index += 1;
            }
            output.push_str(CODE_STRING_STYLE);
            output.extend(chars[start..index].iter());
            output.push_str(CODE_TOKEN_RESET);
            continue;
        }
        if chars[index].is_ascii_digit() {
            let start = index;
            index += 1;
            while index < chars.len()
                && (chars[index].is_ascii_alphanumeric() || matches!(chars[index], '_' | '.'))
            {
                index += 1;
            }
            output.push_str(CODE_NUMBER_STYLE);
            output.extend(chars[start..index].iter());
            output.push_str(CODE_TOKEN_RESET);
            continue;
        }
        if is_code_word_start(chars[index]) {
            let start = index;
            index += 1;
            while index < chars.len() && is_code_word_char(chars[index]) {
                index += 1;
            }
            let token = chars[start..index].iter().collect::<String>();
            let style = if code_keywords(&lang).contains(&token.as_str()) {
                Some(CODE_KEYWORD_STYLE)
            } else if matches!(
                token.as_str(),
                // Python 的 True/False 是大写的——名单里原来只有小写那对,
                // 于是 `while True:` 里的 True 和普通标识符一个色(09-19)。
                "true" | "false" | "null" | "None" | "True" | "False" | "Some" | "Ok" | "Err"
            ) {
                Some(CODE_NUMBER_STYLE)
            } else if next_non_space_is_open_paren(&chars, index) {
                Some(CODE_FUNCTION_STYLE)
            } else {
                None
            };
            if let Some(style) = style {
                output.push_str(style);
                output.push_str(&token);
                output.push_str(CODE_TOKEN_RESET);
            } else {
                output.push_str(PRIMARY_STYLE);
                output.push_str(&token);
                output.push_str(CODE_TOKEN_RESET);
            }
            continue;
        }
        output.push(chars[index]);
        index += 1;
    }
    output
}

const CODE_BLOCK_FOOTER: &str = "--";

/// 代码块的抬头与框宽。整块和活尾巴（`render_code_block_tail`）共用：同一份代码两处
/// 算出来的框必须一样宽，闭合那一刻样子才不跳。
fn code_block_layout<'a>(lang: &str, lines: impl Iterator<Item = &'a str>) -> (String, usize) {
    let label = if lang.is_empty() {
        "code".to_string()
    } else {
        format!("code {lang}")
    };
    let header = format!("-- {label}");
    // 框宽封顶在内容宽度：一行代码比屏（或面板）还宽的话，框按最长那行画出来
    // 就被终端硬折成碎片——底色断成两截、下一行是一片空白（子代理面板里
    // 实测）。超长的行在框里折行，一个字不丢。
    let cap = crate::render::content_cols(120).saturating_sub(2).max(24);
    let width = lines
        .map(UnicodeWidthStr::width)
        .chain([header.chars().count(), CODE_BLOCK_FOOTER.chars().count()])
        .max()
        .unwrap_or(CODE_BLOCK_FOOTER.len())
        .clamp(24, cap);
    (header, width)
}

pub(crate) fn render_code_block(lang: &str, lines: &[String]) -> String {
    let (header, width) = code_block_layout(lang, lines.iter().map(String::as_str));
    let mut output = String::new();
    output.push_str(&render_code_block_frame(&header, width));
    output.push('\n');
    for line in lines {
        for piece in crate::render::wrap_display_text(line, width) {
            output.push_str(&render_code_block_line_with_width(lang, &piece, width));
            output.push('\n');
        }
    }
    output.push_str(&render_code_block_frame(CODE_BLOCK_FOOTER, width));
    output.push('\n');
    output
}

/// 还没闭合的代码块露出来的最后 `max_rows` 行（正文的活尾巴，09-25）。`partial` 是还没
/// 收到换行的最后那半行。
///
/// 和 [`render_code_block`] 一个样子（框宽照整块算），只是不渲露不出来的那些行：一段
/// 几千行的代码每一拍都整块重渲，会把一个核吃满（思考滚动窗的 `ThoughtRows` 就是这么
/// 来的）。整块都露得出来时才带抬头。
pub(crate) fn render_code_block_tail(
    lang: &str,
    lines: &[String],
    partial: Option<&str>,
    max_rows: usize,
) -> Vec<String> {
    if max_rows == 0 {
        return Vec::new();
    }
    let all = || lines.iter().map(String::as_str).chain(partial);
    let (header, width) = code_block_layout(lang, all());
    let mut rows = vec![render_code_block_frame(CODE_BLOCK_FOOTER, width)];
    let mut complete = true;
    let newest_first = partial
        .into_iter()
        .chain(lines.iter().rev().map(String::as_str));
    'lines: for line in newest_first {
        let line = crate::render::markdown::preview_tail(line);
        for piece in crate::render::wrap_display_text(line, width).iter().rev() {
            if rows.len() >= max_rows {
                complete = false;
                break 'lines;
            }
            rows.push(render_code_block_line_with_width(lang, piece, width));
        }
    }
    if complete && rows.len() < max_rows {
        rows.push(render_code_block_frame(&header, width));
    }
    rows.reverse();
    rows
}

pub(crate) fn render_code_block_frame(text: &str, width: usize) -> String {
    if text == "--" {
        return format!("{CODE_BLOCK_FRAME_STYLE}{}{RESET}", "─".repeat(width));
    }
    let label = text.strip_prefix("-- ").unwrap_or(text);
    let prefix = format!("╭─ {label} ");
    format!(
        "{CODE_BLOCK_FRAME_STYLE}{prefix}{}{RESET}",
        "─".repeat(width.saturating_sub(prefix.chars().count()))
    )
}

pub(crate) fn render_code_block_line_with_width(lang: &str, line: &str, width: usize) -> String {
    let line_width = UnicodeWidthStr::width(line);
    let padding = " ".repeat(width.saturating_sub(line_width));
    let highlighted = highlight_code_line(lang, line);
    if highlighted.is_empty() {
        format!("{CODE_BLOCK_BG}{}{RESET}", " ".repeat(width.max(1)))
    } else {
        format!("{CODE_BLOCK_BG}{highlighted}{padding}{RESET}")
    }
}

pub(crate) fn code_keywords(lang: &str) -> &'static [&'static str] {
    match lang {
        "rs" | "rust" => &[
            "as", "async", "await", "break", "const", "continue", "crate", "else", "enum", "fn",
            "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref",
            "return", "self", "Self", "static", "struct", "trait", "type", "unsafe", "use",
            "where", "while",
        ],
        "py" | "python" => &[
            "and", "as", "async", "await", "break", "class", "continue", "def", "elif", "else",
            "except", "finally", "for", "from", "if", "import", "in", "is", "lambda", "not", "or",
            "pass", "raise", "return", "try", "while", "with", "yield",
        ],
        "js" | "ts" | "tsx" | "jsx" => &[
            "async", "await", "break", "case", "catch", "class", "const", "continue", "default",
            "else", "export", "extends", "finally", "for", "from", "function", "if", "import",
            "let", "new", "return", "switch", "throw", "try", "typeof", "var", "while",
        ],
        "sh" | "bash" | "zsh" | "fish" => &[
            "case", "do", "done", "elif", "else", "esac", "fi", "for", "function", "if", "in",
            "then", "while",
        ],
        "json" | "toml" | "yaml" | "yml" => &["true", "false", "null"],
        _ => &[],
    }
}

pub(crate) fn is_code_word_start(ch: char) -> bool {
    ch.is_ascii_alphabetic() || ch == '_'
}

pub(crate) fn is_code_word_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}

pub(crate) fn next_non_space_is_open_paren(chars: &[char], mut index: usize) -> bool {
    while index < chars.len() && chars[index].is_whitespace() {
        index += 1;
    }
    chars.get(index) == Some(&'(')
}

/// 一条命令的每一行该按什么语言着色。
///
/// 命令本身是 shell，但 `<<'PY'` 这种 heredoc 里装的往往是**别的语言的源码**
/// ——AI 自己写脚本时最常见的形状，也正是命令长到需要看清楚的那种时候
///（用户 09-19 的截图就是 `python3 - <<'PY'` 塞了五十行 Python）。
///
/// 语言从**起这个 heredoc 的那一行里的解释器词**认。认不出来就返回空串（不着
/// 色）：`cat > 文件 <<'EOF'` 那种 heredoc 体是数据不是代码，猜错了比不上色
/// 更难看。
pub fn command_line_languages(command: &str) -> Vec<&'static str> {
    let mut languages = Vec::new();
    // 开着的 heredoc：`(结束标记, 体内语言)`。
    let mut open: Option<(String, &'static str)> = None;
    for line in command.lines() {
        if let Some((delimiter, language)) = open.as_ref() {
            if line.trim() == delimiter.as_str() {
                // 结束标记那行是 shell 的词，不是体内容。
                languages.push("bash");
                open = None;
            } else {
                languages.push(language);
            }
            continue;
        }
        languages.push("bash");
        if let Some((delimiter, language)) = heredoc_opened_by(line) {
            open = Some((delimiter, language));
        }
    }
    languages
}

/// 这一行有没有起一个 heredoc；起了的话结束标记是什么、体内是什么语言。
fn heredoc_opened_by(line: &str) -> Option<(String, &'static str)> {
    let chars = line.chars().collect::<Vec<_>>();
    let mut index = 0;
    while index + 1 < chars.len() {
        if chars[index] != '<' || chars[index + 1] != '<' {
            index += 1;
            continue;
        }
        // `<<<` 是 here-string，一行就完了，不开体。
        if chars.get(index + 2) == Some(&'<') {
            index += 3;
            continue;
        }
        let mut cursor = index + 2;
        // `<<-` 允许结束标记前有制表符，对认标记本身没影响。
        if chars.get(cursor) == Some(&'-') {
            cursor += 1;
        }
        while chars.get(cursor).is_some_and(|ch| *ch == ' ') {
            cursor += 1;
        }
        let quote = match chars.get(cursor) {
            Some('\'') => Some('\''),
            Some('"') => Some('"'),
            _ => None,
        };
        if quote.is_some() {
            cursor += 1;
        }
        let start = cursor;
        while let Some(ch) = chars.get(cursor) {
            let ends = match quote {
                Some(quote) => *ch == quote,
                None => !(ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '-')),
            };
            if ends {
                break;
            }
            cursor += 1;
        }
        let delimiter = chars[start..cursor].iter().collect::<String>();
        if delimiter.is_empty() {
            index = cursor.max(index + 2);
            continue;
        }
        return Some((delimiter, heredoc_body_language(line)));
    }
    None
}

/// heredoc 体按什么语言着色：看这一行里的解释器。
fn heredoc_body_language(line: &str) -> &'static str {
    for word in line.split_whitespace() {
        // `/usr/bin/python3` 也算；参数（`-u`、`-`）跳过。
        let name = word.rsplit('/').next().unwrap_or(word);
        let name = name.trim_end_matches(|ch: char| !ch.is_ascii_alphanumeric());
        match name {
            "python" | "python2" | "python3" | "py" => return "python",
            "node" | "nodejs" | "bun" | "deno" => return "js",
            "sh" | "bash" | "zsh" | "fish" => return "bash",
            _ => {}
        }
    }
    // 认不出来就不着色（`cat > 文件 <<'EOF'` 里装的是数据）。
    ""
}
