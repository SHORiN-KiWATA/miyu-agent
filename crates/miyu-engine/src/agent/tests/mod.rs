//! agent 层的测试，按被测主题分文件。
//!
//! 原本是一个三千多行的 `mod tests`。分组按测试实际在测什么，不按代码位置。

mod artifacts;
mod compact_analysis;
mod compact_extras;
mod context;
mod context_meter;
mod input;
mod overflow_recovery;
mod prompt;
mod queue_journal;
mod reasoning;
mod remote_tools;
mod replay_prefix;
mod request_shape;
mod restart;
mod session_usage;
mod shared;
mod stream;
mod subsystems;
mod vision;
