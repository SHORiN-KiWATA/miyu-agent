//! 一轮说完之后的收尾行（用户 09-26 照 Claude Code 定）：
//!
//! ```text
//! ✻ deepseek-v4.1-flash · 处理了 3 分 14 秒 · 1:53 完成
//! ```
//!
//! - 动词按这一轮固定（轮号的哈希挑），重画、回放都是同一个词。词表里不放「运行了」「执行了」：
//!   收缩行已经有「运行了 N 次命令」，撞词就分不清哪句说的是什么；
//! - 被打断的轮末尾写「中断」，不是今天的轮在时刻前面带上日期；
//! - 全屏和常驻 REPL 画它（实时与回放），一次性 / shellhook 不画；
//! - 网页的过程时间线是同一套词表与哈希（`web/app.js` 的 `turnEndText`），两边对得上。

use chrono::{DateTime, Datelike, Local};
use std::time::Duration;

/// 收尾行要知道的。
pub struct TurnEnd<'a> {
    /// 挑动词用。
    pub turn_id: &'a str,
    /// 这一轮是哪个模型答的。不知道（被打断的轮库里不记）就不写。
    pub model: Option<&'a str>,
    pub elapsed: Duration,
    pub finished_at: DateTime<Local>,
    pub interrupted: bool,
}

const VERBS_ZH: [&str; 6] = ["处理了", "忙活了", "琢磨了", "推敲了", "折腾了", "消耗了"];
const VERBS_EN: [&str; 6] = [
    "Churned for",
    "Pondered for",
    "Mulled for",
    "Tinkered for",
    "Crunched for",
    "Spent",
];

/// 轮号的 FNV-1a（32 位）挑一个词：换进程、换版本都是同一个，网页按同样的算法挑。
fn verb_index(turn_id: &str) -> usize {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in turn_id.bytes() {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash as usize % VERBS_ZH.len()
}

/// `✻ 模型 · 处理了 3 分 14 秒 · 1:53 完成`（不带颜色）。`now` 判「是不是今天」。
pub fn turn_end_text(end: &TurnEnd<'_>, now: DateTime<Local>) -> String {
    let zh = miyu_base::i18n::is_zh();
    let index = verb_index(end.turn_id);
    let spent = match (end.elapsed < Duration::from_secs(1), zh) {
        (true, true) => "不到 1 秒".to_string(),
        (true, false) => "<1s".to_string(),
        (false, true) => miyu_base::durations::format_hms_zh(end.elapsed),
        (false, false) => miyu_base::durations::format_hms(end.elapsed),
    };
    let at = clock_text(end.finished_at, now, zh);
    let when = match (zh, end.interrupted) {
        (true, false) => format!("{at} 完成"),
        (true, true) => format!("{at} 中断"),
        (false, false) => format!("done at {at}"),
        (false, true) => format!("stopped at {at}"),
    };
    let verb = if zh { VERBS_ZH[index] } else { VERBS_EN[index] };
    let mut parts: Vec<String> = end
        .model
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(str::to_string)
        .into_iter()
        .collect();
    parts.push(spell(verb, &spent));
    parts.push(when);
    format!("✻ {}", parts.join(" · "))
}

/// 动词接用时：数字前留一个空格（`处理了 3 秒`），`不到 1 秒` 直接接上（`处理了不到 1 秒`）。
pub(crate) fn spell(verb: &str, spent: &str) -> String {
    if spent.starts_with(|c: char| c.is_ascii_digit() || c == '<') {
        format!("{verb} {spent}")
    } else {
        format!("{verb}{spent}")
    }
}

/// 几点：今天的只写时刻，今年的带月日，更早的带年份。
fn clock_text(at: DateTime<Local>, now: DateTime<Local>, zh: bool) -> String {
    let clock = at.format("%-H:%M").to_string();
    if at.date_naive() == now.date_naive() {
        return clock;
    }
    match (zh, at.year() == now.year()) {
        (true, true) => format!("{}月{}日 {clock}", at.month(), at.day()),
        (true, false) => format!("{}年{}月{}日 {clock}", at.year(), at.month(), at.day()),
        (false, true) => format!("{} {clock}", at.format("%b %-d")),
        (false, false) => format!("{} {clock}", at.format("%b %-d %Y")),
    }
}

/// 上了色的那一行（不带换行、不缩进）：暗色，和「本次供应商 / 模型」那行同一个灰。
pub fn turn_end_styled(end: &TurnEnd<'_>) -> String {
    format!(
        "\x1b[2m\x1b[38;5;245m{}\x1b[0m",
        turn_end_text(end, Local::now())
    )
}

/// 落进正文的那一帧：末尾留一个空行。上面和回复之间的空行是渲染器收尾本来就留的。全屏下
/// 按正文的缩进对齐。
pub fn turn_end_frame(end: &TurnEnd<'_>) -> String {
    let line = format!("{}\n\n", turn_end_styled(end));
    if crate::render::blocks::enabled() {
        super::indent_body(&line)
    } else {
        line
    }
}

/// 库里记的两个时刻（RFC 3339）拼成收尾行要的那几样；缺一个就画不了（还在跑、老数据）。
pub fn turn_end_span(
    started_at: Option<&str>,
    finished_at: Option<&str>,
) -> Option<(Duration, DateTime<Local>)> {
    let parse = |text: &str| DateTime::parse_from_rfc3339(text).ok();
    let started = parse(started_at?)?;
    let finished = parse(finished_at?)?;
    let elapsed = (finished - started).to_std().unwrap_or_default();
    Some((elapsed, finished.with_timezone(&Local)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::t;
    use chrono::TimeZone;

    fn end(interrupted: bool, finished_at: DateTime<Local>) -> TurnEnd<'static> {
        TurnEnd {
            turn_id: "turn_1790180210926_acd86d38",
            model: Some("deepseek-v4.1-flash"),
            elapsed: Duration::from_secs(194),
            finished_at,
            interrupted,
        }
    }

    /// 同一轮每次画都是同一个动词（用户 09-26：按这一轮固定），而且不同的轮会挑到不同的词。
    #[test]
    fn the_verb_is_fixed_per_turn() {
        let first = verb_index("turn_1790180210926_acd86d38");
        assert_eq!(first, verb_index("turn_1790180210926_acd86d38"));
        let spread: std::collections::HashSet<usize> =
            (0..64).map(|n| verb_index(&format!("turn_{n}"))).collect();
        assert!(spread.len() > 3, "{spread:?}");
        // 网页 `turnEndText` 用同一个哈希：这个值两边都钉着。
        assert_eq!(verb_index("turn_1790180210926_acd86d38"), 5);
        assert_eq!(verb_index("turn_0"), 3);
    }

    #[test]
    fn today_reads_time_and_state() {
        let now = Local.with_ymd_and_hms(2026, 9, 26, 2, 0, 0).unwrap();
        let at = Local.with_ymd_and_hms(2026, 9, 26, 1, 53, 7).unwrap();
        let verb = t(VERBS_EN[5], VERBS_ZH[5]);
        let expected = if miyu_base::i18n::is_zh() {
            format!("✻ deepseek-v4.1-flash · {verb} 3 分 14 秒 · 1:53 完成")
        } else {
            format!("✻ deepseek-v4.1-flash · {verb} 3m 14s · done at 1:53")
        };
        assert_eq!(turn_end_text(&end(false, at), now), expected);
        assert!(turn_end_text(&end(true, at), now).ends_with(t("stopped at 1:53", "1:53 中断")));
        let mut unknown = end(true, at);
        unknown.model = None;
        assert!(turn_end_text(&unknown, now).starts_with(&format!("✻ {verb} ")));
    }

    #[test]
    fn older_turns_carry_the_date() {
        let now = Local.with_ymd_and_hms(2026, 9, 26, 2, 0, 0).unwrap();
        let yesterday = Local.with_ymd_and_hms(2026, 9, 25, 23, 5, 0).unwrap();
        assert!(turn_end_text(&end(false, yesterday), now)
            .ends_with(t("done at Sep 25 23:05", "9月25日 23:05 完成")));
        let last_year = Local.with_ymd_and_hms(2025, 12, 31, 9, 0, 0).unwrap();
        assert!(turn_end_text(&end(false, last_year), now)
            .ends_with(t("done at Dec 31 2025 9:00", "2025年12月31日 9:00 完成")));
    }

    #[test]
    fn the_span_comes_from_the_two_stored_times() {
        let (elapsed, _) = turn_end_span(
            Some("2026-09-26T01:49:53+00:00"),
            Some("2026-09-26T01:53:07+00:00"),
        )
        .unwrap();
        assert_eq!(elapsed, Duration::from_secs(194));
        assert!(turn_end_span(Some("2026-09-26T01:49:53+00:00"), None).is_none());
    }
}
