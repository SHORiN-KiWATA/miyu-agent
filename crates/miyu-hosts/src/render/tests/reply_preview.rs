//! 正文活尾巴的预览（`MarkdownStreamRenderer::pending_preview`，09-25）：还没落下的那一截
//! 露出来时，长相得和落下之后一样，而且看一眼不能改变任何东西。

use crate::render::*;

fn lines(text: &str) -> Vec<String> {
    text.lines().map(str::to_string).collect()
}

#[test]
fn a_half_line_previews_as_the_line_it_will_become() {
    let mut renderer = MarkdownStreamRenderer::new();
    assert_eq!(renderer.push("## Half a hea"), "");
    assert_eq!(
        renderer.pending_preview(10),
        vec![render_markdown_line("## Half a hea")]
    );
    // 换行一到整行落下，预览就空了。
    assert_eq!(
        renderer.push("\n"),
        format!("{}\n", render_markdown_line("## Half a hea"))
    );
    assert!(renderer.pending_preview(10).is_empty());
}

/// 没闭合的代码块和闭合后落下的那一块一个样子：框宽照整块算，只露最后几行。
#[test]
fn an_open_code_block_previews_in_its_final_frame() {
    let code = ["fn main() {", "    let long_name_for_width = 42;", "}"];
    let mut renderer = MarkdownStreamRenderer::new();
    renderer.push("```rust\n");
    for line in code {
        renderer.push(&format!("{line}\n"));
    }
    let owned = code.iter().map(|line| line.to_string()).collect::<Vec<_>>();
    let whole = lines(&render_code_block("rust", &owned));
    assert_eq!(renderer.pending_preview(20), whole, "露得下就整块照搬");
    assert_eq!(
        renderer.pending_preview(2),
        whole[whole.len() - 2..].to_vec(),
        "露不下就是整块的最后几行"
    );
    // 还没收到换行的那半行也算进块里。
    renderer.push("// tail");
    let mut with_partial = owned.clone();
    with_partial.push("// tail".to_string());
    assert_eq!(
        renderer.pending_preview(20),
        lines(&render_code_block("rust", &with_partial))
    );
}

/// 看一眼不改变任何东西：mermaid 在预览里只露源码，真落下时照样出图那一套。
#[test]
fn previewing_never_changes_what_lands() {
    let feed = |peek: bool| {
        let mut renderer = MarkdownStreamRenderer::new();
        let mut out = String::new();
        for piece in [
            "Intro **bo",
            "ld** text\n```",
            "mermaid\ngraph TD\n",
            "A-->B\n```\n",
            "tail",
        ] {
            out.push_str(&renderer.push(piece));
            if peek {
                let _ = renderer.pending_preview(5);
                let _ = renderer.pending_fingerprint();
            }
        }
        out.push_str(&renderer.flush());
        out
    };
    assert_eq!(feed(true), feed(false));
}

#[test]
fn an_open_display_formula_previews_as_raw_tex() {
    let mut renderer = MarkdownStreamRenderer::new();
    renderer.push("$$\n");
    renderer.push("E = mc^2\n");
    renderer.push("x +");
    let preview = renderer
        .pending_preview(10)
        .iter()
        .map(|row| strip_ansi_text(row))
        .collect::<Vec<_>>();
    assert_eq!(preview, vec!["$$", "E = mc^2", "x +"]);
}

/// 表格已经开了头：进行中的那一行按表格行渲（和落下时同一套列宽）。
#[test]
fn a_row_of_an_open_table_previews_as_a_table_row() {
    let mut renderer = MarkdownStreamRenderer::new();
    renderer.push("| a | b |\n|---|---|\n| 1 | 2 |\n");
    renderer.push("| 3 | 4");
    let preview = renderer.pending_preview(10);
    let text = preview
        .iter()
        .map(|row| strip_ansi_text(row))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains('3') && text.contains('4'), "{text}");
    assert!(text.contains('│') || text.contains('|'), "{text}");
}

#[test]
fn the_fingerprint_moves_whenever_the_pending_text_does() {
    let mut renderer = MarkdownStreamRenderer::new();
    let empty = renderer.pending_fingerprint();
    renderer.push("abc");
    let partial = renderer.pending_fingerprint();
    assert_ne!(empty, partial);
    renderer.push("```\n");
    assert_ne!(renderer.pending_fingerprint(), partial);
}

/// 一整段不带换行的超长文字：预览只看最后那一截，每一拍的活是有上限的。
#[test]
fn a_huge_half_line_previews_only_its_end() {
    let mut renderer = MarkdownStreamRenderer::new();
    renderer.push(&format!("{}END", "a".repeat(20_000)));
    let text = strip_ansi_text(&renderer.pending_preview(10).concat());
    assert!(text.ends_with("END"), "{}", &text[text.len() - 20..]);
    assert_eq!(
        text.chars().count(),
        crate::render::markdown::PREVIEW_LINE_CHARS
    );
}
