//! 测试夹具:只在 cfg(test) 编译,生产二进制零字节。从 `src/state/usage.rs` 搬来(09-16 夹具搬家)。
#![allow(dead_code)]
use super::*;

/// 往 `state_dir` 的账本记一条明细,管理员名下、此刻的时间。
pub fn record_usage(state_dir: &Path, usage: &Usage, meta: UsageMeta<'_>, aux: bool) -> Result<()> {
    ledger(state_dir)?.record(usage, meta, aux, "")
}

/// 同 [`record_usage`],时间自己给。
pub fn record_usage_at(
    state_dir: &Path,
    usage: &Usage,
    meta: UsageMeta<'_>,
    aux: bool,
    ts: i64,
) -> Result<()> {
    ledger(state_dir)?.record_at(usage, meta, aux, "", ts)
}

/// 最近 `limit` 条调用记录,新的在前;可按来源/模型过滤。
pub fn usage_details(
    state_dir: &Path,
    limit: usize,
    src: Option<&str>,
    model: Option<&str>,
    price: PriceFn<'_>,
) -> Result<Vec<UsageRecord>> {
    ledger(state_dir)?.details(limit, src, model, price, None)
}
