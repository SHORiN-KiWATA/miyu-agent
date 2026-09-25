//! 渲染层的测试，按被测部件分文件。
//!
//! 原本是一个两千多行的 `mod tests`。这里的断言大多是「终端里长什么样」，
//! 所以分组按部件走：命令块、Markdown、表格、工具摘要、推理计时。

mod command;
mod command_step;
mod cross_session;
mod diagram_style;
mod event_clock;
mod golden;
mod markdown;
mod math;
mod patch;
mod question_panel;
mod reasoning;
mod reasoning_stall;
mod reply_preview;
mod shared;
mod surface;
mod table;
mod thought_rows;
mod timeline;
mod timeline_panels;
mod todo;
mod tool_summary;
mod usage;
