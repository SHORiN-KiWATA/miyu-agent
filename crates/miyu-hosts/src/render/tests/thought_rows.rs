//! 正在想的正文折好的行：只往后补的缓存，和整段重折一行不差。

use super::timeline::timeline_renderer;
use crate::render::stream::timeline::wrap_detail;

const TEXT: &str = "先把这一段想清楚再动手：setLeg(true, theta); setLeg(false, theta + Math.PI); \
车头轻微摆动，整架上下微颠加左右微倾，这一行故意写得很长很长，好让它在窄一点的宽度里折成好几行。\n\
\n\
// 双腿 + 曲柄\n\
E.steer.setAttribute('transform', `rotate(${(1.1 * Math.sin(TAU * t / 3.9)).toFixed(3)} 128 -175)`);\n\
最后这一段还没写完，没有换行结尾，mixed English words and 中文 to wrap";

/// 一截一截往后追加（截在行中间、截在换行上都有），每追加一次都跟整段重折比。
/// 用户 09-24：思考到三万词元时每一拍整段重折，TUI 吃满一个核。
#[test]
fn appended_thought_rows_match_a_full_wrap() {
    let renderer = timeline_renderer();
    let chars: Vec<char> = TEXT.chars().collect();
    let mut renderer = renderer;
    let mut start = 0;
    let mut step = 1;
    while start < chars.len() {
        let end = (start + step).min(chars.len());
        renderer.reasoning_text.extend(chars[start..end].iter());
        assert_eq!(
            renderer.thought_rows_all(),
            wrap_detail(&renderer.reasoning_text),
            "追加到第 {end} 个字时和整段重折对不上"
        );
        assert_eq!(
            renderer.thought_row_count(),
            wrap_detail(&renderer.reasoning_text).len()
        );
        start = end;
        step = step % 7 + 3;
    }
    let full = wrap_detail(TEXT);
    assert_eq!(
        renderer.thought_rows_last(3),
        full[full.len() - 3..].to_vec()
    );
    assert_eq!(renderer.thought_rows_range(2, 2), full[2..4].to_vec());
}

/// 换下一段思考：正文清空再写，缓存跟着从头来（显式清掉，或者正文变短了自己发现）。
#[test]
fn a_new_thought_starts_the_rows_over() {
    let mut renderer = timeline_renderer();
    renderer.reasoning_text.push_str(TEXT);
    assert!(!renderer.thought_rows_all().is_empty());
    renderer.reasoning_text.clear();
    renderer.thought_rows.borrow_mut().clear();
    renderer.reasoning_text.push_str("新的一段。\n第二行");
    assert_eq!(
        renderer.thought_rows_all(),
        wrap_detail("新的一段。\n第二行")
    );
    // 没人清缓存、正文却变短了：也得从头来，不能接着旧的往后补。
    renderer.reasoning_text = "短\n".to_string();
    assert_eq!(renderer.thought_rows_all(), wrap_detail("短\n"));
}

/// 一大段不换行的思考：最后那半行里前面的物理行也只折一次，结果照样和整段重折一样
///（截在词中间、截在空格上都有）。
#[test]
fn a_long_paragraph_without_newlines_matches_a_full_wrap() {
    let mut renderer = timeline_renderer();
    let paragraph =
        "one two three four five six seven eight nine ten 一二三四五六七八九十 ".repeat(40);
    let chars: Vec<char> = paragraph.chars().collect();
    let mut start = 0;
    let mut step = 2;
    while start < chars.len() {
        let end = (start + step).min(chars.len());
        renderer.reasoning_text.extend(chars[start..end].iter());
        assert_eq!(
            renderer.thought_rows_all(),
            wrap_detail(&renderer.reasoning_text),
            "追加到第 {end} 个字时和整段重折对不上"
        );
        start = end;
        step = step % 11 + 2;
    }
    // 段落写完再来一个换行、接着下一段：前面记住的那些要并进完整的行里。
    renderer.reasoning_text.push_str("\n下一段开头");
    assert_eq!(
        renderer.thought_rows_all(),
        wrap_detail(&renderer.reasoning_text)
    );
}
