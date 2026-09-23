//! 某条车道开一条新会话时的上下文，不建会话就算出来。
//!
//! REPL 大厅里按 Tab 只换显示（用户 09-23 拍板），会话等发第一句话才建。换过去那条
//! 车道此刻没有会话，footer 上的上下文原来只能显示「—」；用户 09-24：「这个初始
//! 上下文数据不是可以事先计算好的吗？」——能：空会话的上下文就是系统提示词加工具表，
//! 和会话里有什么无关。

use crate::web::*;

/// 估算用的会话 id。库里没有它：没有回合、没有钉模型、没有沙盒，估出来的正好是
/// 「这条车道开一条新会话」那一刻的数。`Agent::new` 与估算只读不写，不会因此在
/// 库里留下什么。
const PROBE_SESSION_ID: &str = "__empty_session_context__";

/// 这条车道开一条新会话时的上下文词元数。
///
/// 和 `session_state_for` 对空会话算的是同一套（按车道装配 Agent，再
/// `current_context`），所以建完会话之后 footer 上的数不会跳。装一次 Agent 要把工具表
/// 建出来，按（车道, 配置）缓存：配置不变，同一条车道只算一次。
pub(in crate::web) fn empty_session_context(state: &DaemonState, dev: bool) -> Result<u64> {
    static CACHE: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<(bool, u64), u64>>,
    > = std::sync::OnceLock::new();
    let config = state.manager.lock().unwrap().config.clone();
    let key = (dev, config_fingerprint(&config));
    let cache = CACHE.get_or_init(Default::default);
    if let Some(tokens) = cache.lock().unwrap().get(&key).copied() {
        return Ok(tokens);
    }
    // dev 按 dev 装配：系统提示词、工具表、记忆钥匙都跟着车道走（同 `session_state_for`）。
    let (config, lane) = if dev {
        (config.dev_scoped(), PersonaLane::Dev)
    } else {
        (config, PersonaLane::Active)
    };
    let store = state.state_store.pinned(PROBE_SESSION_ID);
    let tokens =
        current_context(&build_session_agent(&config, &state.paths, &store, lane)?)?.tokens;
    cache.lock().unwrap().insert(key, tokens);
    Ok(tokens)
}

/// 配置的指纹：改了哪一项（模型、人格、工具开关……）缓存都该作废，逐项挑容易漏，
/// 整份序列化了算。
fn config_fingerprint(config: &AppConfig) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    serde_json::to_string(config)
        .unwrap_or_default()
        .hash(&mut hasher);
    hasher.finish()
}
