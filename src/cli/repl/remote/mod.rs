//! 终端连 daemon 的回合驱动。
//!
//! 日常路径：回合跑在 daemon 里，这边通过 IPC 收事件流并渲染。
//! 单次调用与交互式 REPL 生命周期不同，分两个文件。
mod interactive;
mod lobby_lane;
mod one_shot;
mod slash_config;
mod slash_context;
mod slash_session;
/// 一次性命令等子代理：报告叫醒的那几轮接着画（09-26）。
mod subagent_follow;
mod submit;
/// 切进子代理会话、回去（会话项目第 3 段）。
mod visit;
pub(in crate::cli) use interactive::*;
pub(in crate::cli) use one_shot::*;
pub(in crate::cli) use subagent_follow::follow_subagent_reports;
