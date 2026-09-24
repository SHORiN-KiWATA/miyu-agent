//! 冷静机制:她最近在这个群说了多少话,主动回复的门槛就抬多高。
//!
//! 09-24 重做(用户拍板)。旧版是「每回一次 +1、每 N 分钟线性回落 1」的热度,
//! 外加扣分与抬门槛两套系数。主群一天回几百次,热度常年几十上百(7 天日志实测
//! 峰值 223),而效果在 3–5 点就封顶——于是全天恒定抬 0.33,分不出「刚连说三句」
//! 和「下午忙过一阵」,忙完要十几个小时才回落。扣分与抬门槛在数学上是同一件事
//! (`分数 - 扣分 >= 门槛 + 抬高` 等价于 `分数 >= 门槛 + 扣分 + 抬高`)。
//!
//! 现在:每回一轮记一笔,每笔 3 分钟衰减一半,衰减后的总和就是「近期发言量」。
//! 它自带上限(话速 × 平均寿命),停下来十来分钟就消退。门槛按一条 S 形曲线抬:
//! 说一句几乎不压,连说到三四句明显收住,再多也最多抬 0.35。
//!
//! 冷静只管插嘴(09-24 从真人聊天的角度定的):真人被搭话会接着回,只有往别人的
//! 对话或开放话题里插嘴才讲分寸。所以被 @ 的(`TriggerConditions::addressed`)和
//! 判官认定冲她来的(`to_bot`)不压。账照样记所有回复,回 @ 的也算——刚被一堆人
//! @ 着忙,就更不会去插别人的嘴。
//!
//! 不给档位、倍率、半衰期这些旋钮(用户 09-24:「没有人回去调挡位的」),只留总
//! 开关 `reply_restraint_enable`;出厂这条曲线就得是对的。

use crate::platforms::plugins::real_context::*;

/// 一笔衰减到一半要多久。
pub(in crate::platforms::plugins::real_context) const RESTRAINT_HALF_LIFE: Duration =
    Duration::from_secs(3 * 60);

/// 曲线的渐近上限:说得再多,门槛最多抬这么高。
///
/// 09-24 用 7 天日志复核:判官放行的插嘴(不冲她来的)余量最大 0.38、中位 0.22。
/// 上限若高过这个尾巴(原先 0.45),她连说三四句之后插嘴一条都过不去,曲线就成了
/// 一堵墙;0.35 让最合适的那 3% 在她正忙时仍能搭一句。回放:插嘴 47 → 42。
const RESTRAINT_CEILING: f64 = 0.35;
/// 近期发言量到多少时,门槛抬到上限的一半。
const RESTRAINT_MIDPOINT: f64 = 2.5;
/// 曲线的陡度。3 让开头很平(一句只抬 0.02)、中段很陡。
const RESTRAINT_STEEPNESS: i32 = 3;

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

    pub(in crate::platforms::plugins::real_context) fn level(&self, now: Instant) -> f64 {
        let elapsed = now.saturating_duration_since(self.settled_at).as_secs_f64();
        self.value * 0.5_f64.powf(elapsed / RESTRAINT_HALF_LIFE.as_secs_f64())
    }

    /// 真发出去一轮回复:记一笔。
    pub(in crate::platforms::plugins::real_context) fn record(&mut self, now: Instant) {
        self.value = self.level(now) + 1.0;
        self.settled_at = now;
    }
}

/// 近期发言量折算成的门槛抬高量:`上限 · pⁿ / (pⁿ + 半程点ⁿ)`。
///
/// 曲线形状 09-24 用 7 天日志回放选定:与此前验收过的「每笔 0.09、封顶 0.40」
/// 直线相比,各触发的回复总量几乎一样(用户要的整体 +20% 保持住),只是把压力
/// 从第一句挪到了连说的第三四句上。
pub(in crate::platforms::plugins::real_context) fn restraint_threshold(
    enabled: bool,
    pressure: f64,
) -> f64 {
    if !enabled || pressure <= 0.0 {
        return 0.0;
    }
    let rising = pressure.powi(RESTRAINT_STEEPNESS);
    RESTRAINT_CEILING * rising / (rising + RESTRAINT_MIDPOINT.powi(RESTRAINT_STEEPNESS))
}
