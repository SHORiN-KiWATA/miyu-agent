//! apply_patch 测试夹具：假回收站。
//!
//! 补丁的 Delete File 走系统回收站（09-24 B11）；测试跑一遍就往开发者的真回收站里
//! 扔一堆临时文件不合适，所以测试构建改成记下路径、直接删掉。

use anyhow::Result;
use std::cell::RefCell;
use std::path::{Path, PathBuf};

thread_local! {
    static TRASHED: RefCell<Vec<PathBuf>> = const { RefCell::new(Vec::new()) };
}

pub(super) fn fake_trash(path: &Path) -> Result<()> {
    std::fs::remove_file(path)?;
    TRASHED.with(|trashed| trashed.borrow_mut().push(path.to_path_buf()));
    Ok(())
}

/// 本线程里被「扔进回收站」的路径。
#[cfg(test)]
pub(super) fn trashed() -> Vec<PathBuf> {
    TRASHED.with(|trashed| trashed.borrow().clone())
}
