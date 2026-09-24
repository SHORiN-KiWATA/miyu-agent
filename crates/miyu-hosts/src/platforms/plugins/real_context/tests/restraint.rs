//! 冷静机制(09-24 重做):近期发言量按半衰期衰减,门槛按内置 S 形曲线抬。

use crate::platforms::plugins::real_context::*;

const MINUTE: Duration = Duration::from_secs(60);

fn close(actual: f64, expected: f64) -> bool {
    (actual - expected).abs() < 1e-9
}

#[test]
fn each_reply_fades_by_half_every_three_minutes() {
    let start = Instant::now();
    let mut pressure = ReplyPressure::new(start);
    pressure.record(start);

    assert!(close(pressure.level(start), 1.0));
    assert!(close(pressure.level(start + 3 * MINUTE), 0.5));
    assert!(close(pressure.level(start + 6 * MINUTE), 0.25));

    pressure.record(start + 3 * MINUTE);
    assert!(close(pressure.level(start + 3 * MINUTE), 1.5));
}

/// 旧热度的病根:一下午每分钟回一次,线性回落跟不上,热度一路涨到上百(09-21
/// 主群实测 223),忙完要十几个小时才降回来。现在同样的话速会停在一个平台上,
/// 停嘴十分钟就只剩一成。
#[test]
fn a_busy_afternoon_neither_piles_up_nor_lingers() {
    let start = Instant::now();
    let settings = RealContextPluginSettings::default();
    let mut session = SessionRuntime::new(start);
    let mut now = start;
    for _ in 0..180 {
        now += MINUTE;
        session.record_reply(now, &settings);
    }
    let plateau = session.reply_pressure(now);
    // 半衰期 3 分钟、每分钟一笔:稳态 1 / (1 - 0.5^(1/3)) ≈ 4.85。
    assert!((4.8..4.9).contains(&plateau), "稳态发言量 {plateau}");
    let later = session.reply_pressure(now + 10 * MINUTE);
    assert!(later < plateau * 0.1, "停嘴十分钟后仍有 {later}");
}

#[test]
fn the_switch_stops_the_count() {
    let now = Instant::now();
    let mut session = SessionRuntime::new(now);
    let off = RealContextPluginSettings {
        reply_restraint_enable: false,
        ..RealContextPluginSettings::default()
    };
    session.record_reply(now, &off);
    assert!(close(session.reply_pressure(now), 0.0));
    assert!(close(restraint_threshold(false, 10.0), 0.0));
}

/// 曲线的形状就是这次改动的全部意图(用户 09-24:不分档,做一条合理的曲线):
/// 说一句几乎不压,连说到三四句明显收住,再多也封顶在 0.35 以下。
#[test]
fn one_line_is_nearly_free_and_a_run_of_lines_holds_her_back() {
    let at = |pressure: f64| restraint_threshold(true, pressure);
    assert!(close(at(0.0), 0.0));
    assert!(at(1.0) < 0.03, "一句 {}", at(1.0));
    assert!(close(at(2.5), 0.175), "半程点应是上限的一半");
    assert!((0.22..0.23).contains(&at(3.0)), "三句 {}", at(3.0));
    assert!((0.28..0.29).contains(&at(4.0)), "四句 {}", at(4.0));
    assert!(at(100.0) < 0.35 && at(100.0) > 0.34, "上限 {}", at(100.0));
    let steps = (0..=80).map(|tenth| at(f64::from(tenth) / 10.0));
    let values = steps.collect::<Vec<_>>();
    assert!(
        values.windows(2).all(|pair| pair[0] <= pair[1]),
        "曲线必须单调"
    );
}

/// 被 @(或顶替了一条被 @ 的)在平台层面就是冲她来的,冷静不压(用户 09-24 拍板);
/// 续聊、刚说过话、抽样要看判官的 to_bot。
#[test]
fn only_platform_mentions_are_exempt_before_the_judge_weighs_in() {
    let conditions = |edit: fn(&mut TriggerConditions)| {
        let mut conditions = TriggerConditions::default();
        edit(&mut conditions);
        conditions
    };
    assert!(conditions(|c| c.direct = true).addressed());
    assert!(conditions(|c| c.inherited = Some(TriggerKind::Direct)).addressed());
    assert!(conditions(|c| c.inherited = Some(TriggerKind::Supersede)).addressed());
    assert!(!conditions(|c| c.inherited = Some(TriggerKind::Probability)).addressed());
    assert!(!conditions(|c| c.continuation = true).addressed());
    assert!(!conditions(|c| c.after_speaking = true).addressed());
    assert!(!conditions(|c| c.probability = true).addressed());
}
