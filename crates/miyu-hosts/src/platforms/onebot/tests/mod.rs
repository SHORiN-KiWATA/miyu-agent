//! OneBot 平台的测试，按被测主题分文件。
//!
//! 原本是一个近四千行的 `mod tests`，里面混着连接、解析、准入、投递、
//! 文件、入群审核六件互不相干的事。

mod admission;
mod connection;
mod delivery;
mod files;
mod forward;
mod identity;
mod notices;
mod parsing;
mod receipt_recall;
mod requests;
mod shared;
mod turn;
