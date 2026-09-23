//! 宿主能力端口:trait 在这里,实现由**拥有那份能力的层**在 daemon 启动时装入。
//!
//! 工具层、中转线桥这些下层要用上层才有的能力(语音桥、QQ 直发、宿主只读查询)
//! 时,不再反向 `use miyu_hosts::web` / `use miyu_hosts::platforms`,而是调这里的窄接口;
//! `web::server` / `platforms::onebot::proactive` 在启动时把实现装进来。所以这几个
//! 文件本身只认 config / paths / platform_types,住在工具层之下;`runtime` 剩下的
//! `DaemonState` / actor / run 那一半才是场所层。
//!
//! - [`ports`]:`VoicePort` / `QqOutreachPort` 两条端口与装入点;
//! - [`host_grants`]:进程外扩展的一次性能力令牌;
//! - [`host_query`]:凭令牌问宿主的只读方法(脱敏 DTO);
//! - [`live_turn`]:平台回合登记的宿主工具位,给中转线桥读;
//! - [`turn_restrictions`]:回合登记的工具白名单与「不写记忆」,给桥和中转线读(09-23);
//! - [`subagent`]:子代理会话化(09-18):工具层请 daemon 建子会话、起回合、等任务终态;
//! - [`cross_session`]:跨会话消息(09-23):列同一个人开着的会话、往里投话。
//!
//! `runtime` 里一行 `pub use crate::host_ports::*;` 保留了老路径,web / pm
//! 这些同层或更高层的调用方写 `miyu_hosts::runtime::…` 照旧能编译。

mod cross_session;
mod host_grants;
mod host_query;
mod live_turn;
mod ports;
mod subagent;
mod turn_restrictions;

pub use cross_session::*;
pub use host_grants::*;
pub use host_query::*;
pub use live_turn::*;
pub use ports::*;
pub use subagent::*;
pub use turn_restrictions::*;

// `host_grants` 签令牌用的 `random_token` 已归位到基础层 `crate::random_id`
// (它与宿主端口无关,只是 id 生成器)。这条再导出让 `host_grants.rs` 里的
// `super::random_token` 一字未改(09-16)。
pub use crate::random_id::random_token;
