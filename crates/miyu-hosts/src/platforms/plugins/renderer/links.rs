//! 纯文本里的地址。
//!
//! pulldown-cmark 只把 `[标题](地址)` 与 `<地址>` 当链接，而提示词要她给来源
//! 写的是 `title (url)` 这种纯文本（09-09 起），正文里也常直接贴地址——图里
//! 这些于是和正文同色，读者分不出哪儿是链接（09-23 用户反馈「链接颜色又没了」）。
//! 只给地址本身上色，标题不染（用户 09-23）。认地址的规则跟终端/WebUI 是同一份
//! （`crate::render::bare_url_at`：句尾标点不算、配不上对的右括号不算）。

use crate::platforms::plugins::renderer::*;
use crate::render::bare_url_at;

/// 给一串片段里的裸地址打上 `link` 样式。行内代码里的地址是给人抄的字面量，
/// 不碰；已经是链接的（Markdown 链接补出来的地址）也不重复处理。
pub(in crate::platforms::plugins::renderer) fn mark_links(spans: &mut Vec<RichSpan>) {
    if !spans.iter().any(|span| span.text.contains("://")) {
        return;
    }
    let mut chars = Vec::new();
    let mut styles = Vec::new();
    for span in spans.iter() {
        for ch in span.text.chars() {
            chars.push(ch);
            styles.push(span.style);
        }
    }
    let mut index = 0;
    while index < chars.len() {
        if styles[index].code || styles[index].link {
            index += 1;
            continue;
        }
        let Some(url) = bare_url_at(&chars, index) else {
            index += 1;
            continue;
        };
        let end = index + url.chars().count();
        if styles[index..end].iter().any(|style| style.code) {
            index += 1;
            continue;
        }
        for style in &mut styles[index..end] {
            style.link = true;
        }
        index = end;
    }
    *spans = regroup(&chars, &styles);
}

fn regroup(chars: &[char], styles: &[InlineStyle]) -> Vec<RichSpan> {
    let mut spans: Vec<RichSpan> = Vec::new();
    for (ch, style) in chars.iter().zip(styles) {
        match spans.last_mut() {
            Some(last) if last.style == *style => last.text.push(*ch),
            _ => spans.push(RichSpan {
                text: ch.to_string(),
                style: *style,
            }),
        }
    }
    spans
}
