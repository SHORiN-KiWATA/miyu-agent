//! Markdown 里夹着的 HTML 标签。
//!
//! 模型爱在表格格子里写 `<br>` 换行（Markdown 表格没有别的换行办法），原先整段
//! HTML 被当成正文原样画了出来。图渲染器不是浏览器，只认几样：`<br>` 画成换行，
//! 常见的格式标签换成对应样式，其余标签只去掉记号、留下里面的字，注释整段不要
//! （用户 09-24）。代码里的尖括号不走这里：pulldown-cmark 把行内代码和代码块的
//! 内容交成 `Code`/`Text`，不会拆成 HTML 事件。

use std::borrow::Cow;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::platforms::plugins::renderer) enum HtmlFormat {
    Bold,
    Italic,
    Strike,
    Code,
}

#[derive(Debug, PartialEq, Eq)]
pub(in crate::platforms::plugins::renderer) enum HtmlPiece<'a> {
    Text(Cow<'a, str>),
    LineBreak,
    Open(HtmlFormat),
    Close(HtmlFormat),
}

/// HTML 标签带来的样式层数。与 Markdown 的 `**`/`*` 分开记：没闭合的 `<b>`
/// 只该染到块尾，每个块、每个表格格子收尾时整个清零。
#[derive(Clone, Copy, Debug, Default)]
pub(in crate::platforms::plugins::renderer) struct HtmlStyle {
    pub(in crate::platforms::plugins::renderer) bold: usize,
    pub(in crate::platforms::plugins::renderer) italic: usize,
    pub(in crate::platforms::plugins::renderer) strike: usize,
    pub(in crate::platforms::plugins::renderer) code: usize,
}

impl HtmlStyle {
    pub(in crate::platforms::plugins::renderer) fn apply(
        &mut self,
        format: HtmlFormat,
        open: bool,
    ) {
        let depth = match format {
            HtmlFormat::Bold => &mut self.bold,
            HtmlFormat::Italic => &mut self.italic,
            HtmlFormat::Strike => &mut self.strike,
            HtmlFormat::Code => &mut self.code,
        };
        *depth = if open {
            depth.saturating_add(1)
        } else {
            depth.saturating_sub(1)
        };
    }
}

/// 把一段 HTML 原文切成字和标签。`in_comment` 由调用方跨事件保存：HTML 块里的
/// 注释可以跨好几行，pulldown-cmark 按行一段段交过来。
pub(in crate::platforms::plugins::renderer) fn scan<'a>(
    html: &'a str,
    in_comment: &mut bool,
) -> Vec<HtmlPiece<'a>> {
    let mut pieces = Vec::new();
    let mut rest = html;
    loop {
        if *in_comment {
            let Some(end) = rest.find("-->") else {
                return pieces;
            };
            rest = &rest[end + 3..];
            *in_comment = false;
        }
        let Some(open) = rest.find('<') else {
            push_text(&mut pieces, rest);
            return pieces;
        };
        push_text(&mut pieces, &rest[..open]);
        let tail = &rest[open..];
        if let Some(comment) = tail.strip_prefix("<!--") {
            *in_comment = true;
            rest = comment;
            continue;
        }
        match tail
            .find('>')
            .and_then(|close| tag_at(&tail[1..close]).map(|tag| (close, tag)))
        {
            Some((close, tag)) => {
                if let Some(piece) = tag {
                    pieces.push(piece);
                }
                rest = &tail[close + 1..];
            }
            // 不像标签（`a < b`、没收口的 `<`）就当普通字符。
            None => {
                push_text(&mut pieces, "<");
                rest = &tail[1..];
            }
        }
    }
}

/// `<` 与 `>` 之间的内容。不是标签返回 `None`；是标签但不认识返回
/// `Some(None)`（去掉记号，什么也不画）。
fn tag_at(inner: &str) -> Option<Option<HtmlPiece<'static>>> {
    let (closing, body) = match inner.strip_prefix('/') {
        Some(body) => (true, body),
        None => (false, inner),
    };
    let name_end = body
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
        .unwrap_or(body.len());
    let name = &body[..name_end];
    if !name.starts_with(|c: char| c.is_ascii_alphabetic()) {
        return None;
    }
    let after = body[name_end..].trim_end_matches('/');
    if !(after.is_empty() || after.starts_with(char::is_whitespace)) {
        return None;
    }
    let format = match name.to_ascii_lowercase().as_str() {
        // `</br>` 不合规范，可模型真会这么写，浏览器也当换行。
        "br" => return Some(Some(HtmlPiece::LineBreak)),
        "b" | "strong" => HtmlFormat::Bold,
        "i" | "em" => HtmlFormat::Italic,
        "s" | "del" | "strike" => HtmlFormat::Strike,
        "code" => HtmlFormat::Code,
        _ => return Some(None),
    };
    Some(Some(if closing {
        HtmlPiece::Close(format)
    } else {
        HtmlPiece::Open(format)
    }))
}

fn push_text<'a>(pieces: &mut Vec<HtmlPiece<'a>>, text: &'a str) {
    if !text.is_empty() {
        pieces.push(HtmlPiece::Text(decode_entities(text)));
    }
}

/// HTML 块里的字 pulldown-cmark 不解码实体，这里补上常见的几种；认不出的原样留着。
fn decode_entities(text: &str) -> Cow<'_, str> {
    if !text.contains('&') {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let tail = &rest[amp..];
        let decoded = tail.find(';').filter(|end| *end <= 10).and_then(|end| {
            let entity = &tail[1..end];
            let ch = match entity {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some('\u{a0}'),
                _ => entity
                    .strip_prefix("#x")
                    .or_else(|| entity.strip_prefix("#X"))
                    .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                    .or_else(|| entity.strip_prefix('#').and_then(|dec| dec.parse().ok()))
                    .and_then(char::from_u32),
            };
            ch.map(|ch| (ch, end))
        });
        match decoded {
            Some((ch, end)) => {
                out.push(ch);
                rest = &tail[end + 1..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    Cow::Owned(out)
}
