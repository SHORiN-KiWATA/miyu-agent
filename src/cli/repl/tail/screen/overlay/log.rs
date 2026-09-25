//! 后台任务的日志：读文件末尾那一截。
//!
//! 原来这里还把子代理的任务日志（流水账）解析成一条时间线，在浮层里再画一遍；子代理
//! 09-18 起是一条会话，点它切进去看，09-25 那一半随老标记中继退役（会话项目第 4 段之二）。

/// 读文件末尾 `budget` 字节。从中间切开的第一行丢掉，免得开头是半个字符。
pub(super) fn read_tail(path: &std::path::Path, budget: u64) -> String {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let Ok(mut file) = std::fs::File::open(path) else {
        return String::new();
    };
    let size = file.metadata().map(|meta| meta.len()).unwrap_or(0);
    let from = size.saturating_sub(budget);
    if from > 0 && file.seek(SeekFrom::Start(from)).is_err() {
        return String::new();
    }
    let mut buffer = Vec::new();
    if file.read_to_end(&mut buffer).is_err() {
        return String::new();
    }
    let text = String::from_utf8_lossy(&buffer).into_owned();
    if from > 0 {
        match text.find('\n') {
            Some(index) => text[index + 1..].to_string(),
            None => text,
        }
    } else {
        text
    }
}
