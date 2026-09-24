//! 冷静机制(09-24 重做):近期发言量按半衰期衰减,只抬门槛。

use crate::platforms::plugins::real_context::*;

const MINUTE: Duration = Duration::from_secs(60);

fn close(actual: f64, expected: f64) -> bool {
    (actual - expected).abs() < 1e-9
}

#[test]
fn each_reply_fades_by_half_per_half_life() {
    let start = Instant::now();
    let half_life = 3 * MINUTE;
    let mut pressure = ReplyPressure::new(start);
    pressure.record(start, half_life, 1.0);

    assert!(close(pressure.level(start, half_life), 1.0));
    assert!(close(pressure.level(start + 3 * MINUTE, half_life), 0.5));
    assert!(close(pressure.level(start + 6 * MINUTE, half_life), 0.25));

    pressure.record(start + 3 * MINUTE, half_life, 1.0);
    assert!(close(pressure.level(start + 3 * MINUTE, half_life), 1.5));
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
    let plateau = session.reply_pressure(now, &settings);
    // 半衰期 3 分钟、每分钟一笔:稳态 1 / (1 - 0.5^(1/3)) ≈ 4.85。
    assert!((4.8..4.9).contains(&plateau), "稳态发言量 {plateau}");
    let later = session.reply_pressure(now + 10 * MINUTE, &settings);
    assert!(later < plateau * 0.1, "停嘴十分钟后仍有 {later}");
}

#[test]
fn the_multiplier_is_the_weight_of_one_reply_and_the_switch_stops_the_count() {
    let now = Instant::now();
    let mut session = SessionRuntime::new(now);
    let settings = RealContextPluginSettings {
        reply_restraint_multiplier: 2.0,
        ..RealContextPluginSettings::default()
    };
    session.record_reply(now, &settings);
    assert!(close(session.reply_pressure(now, &settings), 2.0));

    let off = RealContextPluginSettings {
        reply_restraint_enable: false,
        ..RealContextPluginSettings::default()
    };
    session.record_reply(now, &off);
    assert!(close(session.reply_pressure(now, &settings), 2.0));
}

/// 每笔与封顶 = 旧版「扣分 + 抬门槛」合计 × 1.2(用户 09-24 要求整体 +20%)。
#[test]
fn restraint_is_a_fifth_stronger_than_the_old_table() {
    assert!(close(restraint_threshold(true, "medium", 1.0), 0.09));
    assert!(close(restraint_threshold(true, "medium", 100.0), 0.40));
    assert!(close(restraint_threshold(true, "light", 1.0), 0.03));
    assert!(close(restraint_threshold(true, "light", 100.0), 0.22));
    assert!(close(restraint_threshold(true, "strong", 1.0), 0.18));
    assert!(close(restraint_threshold(true, "strong", 100.0), 0.62));
    assert!(close(restraint_threshold(false, "strong", 100.0), 0.0));
}

/// 「克制恢复时间」现在是半衰期;0 也不能让它除零。
#[test]
fn recovery_minutes_are_the_half_life() {
    let settings = RealContextPluginSettings {
        reply_restraint_recover_minutes: 5,
        ..RealContextPluginSettings::default()
    };
    assert_eq!(restraint_half_life(&settings), 5 * MINUTE);
    let zero = RealContextPluginSettings {
        reply_restraint_recover_minutes: 0,
        ..RealContextPluginSettings::default()
    };
    assert_eq!(restraint_half_life(&zero), MINUTE);
}
