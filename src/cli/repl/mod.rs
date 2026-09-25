//! 交互式 REPL。
//!
//! 按职责分文件：宽度计算、输入编辑、活动区渲染、远端与直连两条回合驱动。
pub(in crate::cli) mod banner;
pub mod commands;
pub(in crate::cli) mod dictation;
/// herdr 的状态上报（不在 herdr 里是 no-op）。
pub(in crate::cli) mod herdr;
pub(in crate::cli) mod input_layout;
pub(in crate::cli) mod job_stop;
pub(in crate::cli) mod jobs;
pub(in crate::cli) mod layout;
pub(in crate::cli) mod midturn_panel;
pub(in crate::cli) mod panel;
pub(in crate::cli) mod pickers;
pub(in crate::cli) mod placeholder;
pub(in crate::cli) mod question_flow;
/// `/sandbox` 查看与绑定回执的表格。
pub(in crate::cli) mod sandbox_view;
pub(in crate::cli) mod session;
pub(in crate::cli) mod session_picker;
/// 任务条：会话行（切进子代理会话、回主会话）与后台任务行。
pub(in crate::cli) mod strip;
/// 任务条上列哪些行、按什么顺序、挂在哪一层。
pub(in crate::cli) mod strip_tree;

pub(super) mod direct;
pub(super) mod editor;
pub(super) mod input;
pub(in crate::cli) mod live_turn;
pub(super) mod remote;
pub(super) mod tail;
pub(super) mod wake;
pub(super) mod width;
