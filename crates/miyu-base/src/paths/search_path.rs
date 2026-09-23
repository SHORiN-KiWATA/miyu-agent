//! 子进程按 PATH 找程序，而 Miyu 自己不一定是经 PATH 找到的。
//!
//! 09-23 macOS 真机：没配 `brew shellenv` 的 shell（她那台的 fish 就没配）里，hook
//! 兜底找到了 `/opt/homebrew/bin/miyu`，可 brew 随 formula 一起装的 `rg`、`chafa`
//! 也在那个目录里——daemon 和工具继承的 PATH 里没有它，搜索直接报「`rg` 不在 PATH
//! 上，请 brew install ripgrep」，而 ripgrep 明明装着。

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// 从父进程继承来的 PATH，补程序目录之前记下。
static INHERITED_PATH: OnceLock<Option<OsString>> = OnceLock::new();

/// 入口处调一次（还没有别的线程时）：记下继承来的 PATH，再把程序所在目录补到末尾，
/// 之后起的子进程——daemon、rg、chafa、脚本——都用补过的这份。
pub fn extend_path_with_executable_dir(executable: &Path) {
    let inherited = std::env::var_os("PATH");
    let _ = INHERITED_PATH.set(inherited.clone());
    if let Some(path) = path_with_executable_dir(inherited.as_deref(), executable) {
        std::env::set_var("PATH", path);
    }
}

/// 用户的 shell 看到的那份 PATH（没补过程序目录）。替用户判断「shell 里敲这个命令
/// 找不找得到」时看它：装 hook 时提醒「PATH 上找不到 miyu」、拦截时认「这行是不是
/// 命令」都是这个意思——看补过的 PATH，前者永远不会提醒，后者会把 shell 根本跑不了的
/// 命令当成命令。
pub fn inherited_path() -> Option<OsString> {
    match INHERITED_PATH.get() {
        Some(path) => path.clone(),
        None => std::env::var_os("PATH"),
    }
}

/// 程序所在目录不在 PATH 上时，返回把它补在**末尾**的新 PATH；不用改时返回 `None`。
///
/// 补在末尾：用户自己排的顺序一个不动，只在别处都找不到时才轮到它。PATH 根本没设时
/// 也不动——那时子进程走系统默认搜索路径，写一个只含这个目录的 PATH 反而把默认路径
/// 挡掉了。
pub fn path_with_executable_dir(path: Option<&OsStr>, executable: &Path) -> Option<OsString> {
    let path = path?;
    let dir = executable.parent()?;
    if !dir.is_absolute() || !dir.is_dir() {
        return None;
    }
    let mut entries: Vec<PathBuf> = std::env::split_paths(path).collect();
    if entries.iter().any(|entry| entry == dir) {
        return None;
    }
    entries.push(dir.to_path_buf());
    std::env::join_paths(entries).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_the_executable_directory_when_path_lacks_it() {
        let temp = tempfile::tempdir().unwrap();
        let bin = temp.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let path = path_with_executable_dir(Some(OsStr::new("/usr/bin:/bin")), &bin.join("miyu"))
            .expect("PATH 里没有程序所在目录时要补上");
        assert_eq!(
            std::env::split_paths(&path).collect::<Vec<_>>(),
            vec![PathBuf::from("/usr/bin"), PathBuf::from("/bin"), bin],
            "补在末尾,原有顺序不动"
        );
    }

    #[test]
    fn leaves_path_alone_when_already_present_unset_or_bogus() {
        let temp = tempfile::tempdir().unwrap();
        let bin = temp.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let listed = std::env::join_paths([Path::new("/usr/bin"), bin.as_path()]).unwrap();
        assert_eq!(
            path_with_executable_dir(Some(&listed), &bin.join("miyu")),
            None
        );
        assert_eq!(path_with_executable_dir(None, &bin.join("miyu")), None);
        // 测试构建里 miyu_executable() 给的是一个必然不存在的路径,不能把它塞进 PATH。
        let missing = Path::new("/nonexistent/miyu-test-harness");
        assert_eq!(
            path_with_executable_dir(Some(OsStr::new("/usr/bin")), missing),
            None
        );
        assert_eq!(
            path_with_executable_dir(Some(OsStr::new("/usr/bin")), Path::new("miyu")),
            None
        );
    }
}
