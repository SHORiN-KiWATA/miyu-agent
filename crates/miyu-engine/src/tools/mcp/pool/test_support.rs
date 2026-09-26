//! 测试夹具：只在 cfg(test) 编译。连接池是进程级的，用例之间要清干净。

use super::*;

pub(in crate::tools::mcp) fn live_count() -> usize {
    pool().lock().unwrap().entries.len()
}

pub(in crate::tools::mcp) fn reset_for_test() {
    let mut pool = pool().lock().unwrap();
    pool.entries.clear();
    pool.retired.clear();
    pool.crashes.clear();
    pool.paused.clear();
}

pub(in crate::tools::mcp) fn force_enable_for_test() {
    ENABLED.store(true, Ordering::Release);
}
