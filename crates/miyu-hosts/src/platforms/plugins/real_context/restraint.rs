//! 冷静机制:她最近在这个群说了多少话,主动回复的门槛就抬多高。
//!
//! 09-24 重做(用户拍板)。旧版是「每回一次 +1、每 N 分钟线性回落 1」的热度,
//! 外加扣分与抬门槛两套系数。主群一天回几百次,热度常年几十上百(7 天日志实测
//! 峰值 223),而效果在 3–5 点就封顶——于是全天恒定抬 0.33,分不出「刚连说三句」
//! 和「下午忙过一阵」,忙完要十几个小时才回落。扣分与抬门槛在数学上是同一件事
//! (`分数 - 扣分 >= 门槛 + 抬高` 等价于 `分数 >= 门槛 + 扣分 + 抬高`)。
//!
//! 现在:每回一轮记一笔(权重 = 克制倍率),每笔按半衰期指数衰减(半衰期 = 克制
//! 恢复时间),衰减后的总和就是「近期发言量」。它自带上限(话速 × 平均寿命),
//! 停下来几个半衰期就消退。效果只有一个:门槛抬高 min(发言量 × 每笔, 封顶)。
//! 被 @ 的消息照样受压,回 @ 的那几轮也照样记账——真人不会因为被艾特就不累
//! (用户 09-24 原话的意思)。

use crate::platforms::plugins::real_context::*;

/// 一个群的近期发言量。只存「上次结算时的值」和结算时刻,读的时候现算衰减。
#[derive(Clone, Copy, Debug)]
pub(in crate::platforms::plugins::real_context) struct ReplyPressure {
    value: f64,
    settled_at: Instant,
}

impl ReplyPressure {
    pub(in crate::platforms::plugins::real_context) fn new(now: Instant) -> Self {
        Self {
            value: 0.0,
            settled_at: now,
        }
    }

    pub(in crate::platforms::plugins::real_context) fn level(
        &self,
        now: Instant,
        half_life: Duration,
    ) -> f64 {
        let elapsed = now.saturating_duration_since(self.settled_at).as_secs_f64();
        let half_life = half_life.as_secs_f64().max(1.0);
        self.value * 0.5_f64.powf(elapsed / half_life)
    }

    pub(in crate::platforms::plugins::real_context) fn record(
        &mut self,
        now: Instant,
        half_life: Duration,
        weight: f64,
    ) {
        self.value = self.level(now, half_life) + weight.max(0.0);
        self.settled_at = now;
    }
}

/// 「克制恢复时间」现在的含义:一笔衰减到一半要多久。
pub(in crate::platforms::plugins::real_context) fn restraint_half_life(
    settings: &RealContextPluginSettings,
) -> Duration {
    Duration::from_secs(settings.reply_restraint_recover_minutes.max(1) * 60)
}

/// 近期发言量折算成的门槛抬高量。
///
/// 每笔与封顶是旧版「扣分 + 抬门槛」合计的 1.2 倍(用户 09-24 要求整体 +20%),
/// 取两位小数:旧中档合计每点 0.075、封顶 0.33,轻档 0.025 / 0.18,强档
/// 0.15 / 0.52。
pub(in crate::platforms::plugins::real_context) fn restraint_threshold(
    enabled: bool,
    strength: &str,
    pressure: f64,
) -> f64 {
    if !enabled {
        return 0.0;
    }
    let (per_reply, maximum) = match strength {
        "light" => (0.03, 0.22),
        "strong" => (0.18, 0.62),
        _ => (0.09, 0.40),
    };
    (pressure.max(0.0) * per_reply).min(maximum)
}
