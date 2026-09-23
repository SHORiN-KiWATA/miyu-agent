//! `miyu-base`:Miyu 的第 基础/配置 层(09-16 拆 crate)。模块归属见 `test_scripts/arch_dep_check.py` 的 `TIERS`。
// 09-16 接口治理收口:死代码在生产构建里是警告,不再全局豁免;测试构建里 cfg(test) 的辅助函数照旧豁免。
#![cfg_attr(test, allow(dead_code))]

pub mod clipboard;
pub mod config;
pub mod default_models;
pub mod durations;
pub mod embedding;
pub mod host_info;
pub mod host_ports;
pub mod http_response;
pub mod i18n;
pub mod json_extract;
pub mod logging;
pub mod media_mime;
pub mod memory_types;
pub mod models_cache;
pub mod notify;
pub mod orphan_guard;
pub mod paths;
pub mod persona_lane;
pub mod platform_types;
pub mod process;
pub mod prompts;
pub mod provider_catalog;
pub mod provider_url;
pub mod question;
pub mod random_id;
pub mod sandbox;
pub mod shell;
pub mod terminal;
pub mod token_counter;
pub mod token_estimate;
pub mod tool_names;
pub mod workspace;

/// 本次构建的唯一 id:CLI 据它判断后台 daemon 是不是老版本,WebUI 静态资源的版本号也用它。
///
/// 真相源在根包 `build.rs`(只有它对整棵源码树 `rerun-if-changed`),可执行入口
/// `miyu::run()` 一进来就 [`install_build_id`] 装进这个槽;下层 crate 运行时读
/// [`build_id`],而不是编译期嵌一个常量——否则改上层一行都得从 base 起全量重编。
/// 没装(库测试、夹具进程)时读到 `"dev"`,同一进程两边一致。
static BUILD_ID_SLOT: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();

/// 装入本次构建的 id;只认第一次,之后的调用无效。
pub fn install_build_id(id: &'static str) {
    let _ = BUILD_ID_SLOT.set(id);
}

/// 本次构建的 id;入口没装时是 `"dev"`。
pub fn build_id() -> &'static str {
    BUILD_ID_SLOT.get().copied().unwrap_or("dev")
}

/// 仓库根(编译期由 base 的 `build.rs` 从自己的 manifest 目录算出):测试与开发态
/// 资源查找用它,别用 `CARGO_MANIFEST_DIR`——拆 crate 之后那是 `crates/<crate>`,
/// 而资源(`src/personas`、`assets/`、测试夹具)都留在仓库根。
pub const WORKSPACE_ROOT: &str = env!("MIYU_WORKSPACE_ROOT");

/// 记忆分词用的紧凑 Jieba 词典(`build.rs` 从 assets/jieba/dict.txt 编成 FST)。
pub const JIEBA_INDEX: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/jieba.fst"));
