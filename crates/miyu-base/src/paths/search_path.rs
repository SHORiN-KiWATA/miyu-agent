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

/// PATH 上找不到程序时依次去找的常见安装目录：Apple Silicon / Intel 的 Homebrew、
/// Linuxbrew、官方安装脚本和 `pip` 常用的 `~/.local/bin`、`cargo install`。
///
/// shell hook 找 `miyu`、daemon 找 `claude` / `codex` 这些 CLI 用的是同一张表（09-23）：
/// 她那台 Mac 上 `claude` 在 `~/.local/bin`（官方安装脚本）、`codex` 在
/// `/opt/homebrew/bin`（Homebrew），拉起 Miyu 的终端 PATH 里不一定有它们。
/// **改顺序或增删会改变 hook 的字节**（已装的 hook 会被整份重写一次）。
pub const COMMON_BIN_DIRS: &[&str] = &[
    "/opt/homebrew/bin",
    "/usr/local/bin",
    "/home/linuxbrew/.linuxbrew/bin",
    "~/.local/bin",
    "~/.cargo/bin",
];

/// [`COMMON_BIN_DIRS`] 展开 `~/` 之后的目录；没有家目录时跳过那几项。
pub fn common_bin_dirs(home: Option<&Path>) -> Vec<PathBuf> {
    COMMON_BIN_DIRS
        .iter()
        .filter_map(|dir| match dir.strip_prefix("~/") {
            Some(rest) => home.map(|home| home.join(rest)),
            None => Some(PathBuf::from(dir)),
        })
        .collect()
}

/// 外部程序该怎么启动：设置里填了路径就用填的；没填时 PATH 上有就按名字启动，PATH 上
/// 没有就去常见安装目录找，找到给绝对路径；哪儿都没有照原样返回名字，由启动处报
/// 「不在 PATH 上」。
pub fn configured_program(configured: &str, name: &str) -> PathBuf {
    let configured = configured.trim();
    if !configured.is_empty() {
        return PathBuf::from(configured);
    }
    let home = directories::BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf());
    resolve_program_in(
        name,
        std::env::var_os("PATH").as_deref(),
        &common_bin_dirs(home.as_deref()),
    )
}

/// 按名字能不能启动得起来（PATH 上，或常见安装目录里）。
pub fn program_available(name: &str) -> bool {
    let program = configured_program("", name);
    program.is_absolute() || on_path(name, std::env::var_os("PATH").as_deref())
}

pub(crate) fn resolve_program_in(name: &str, path: Option<&OsStr>, dirs: &[PathBuf]) -> PathBuf {
    if on_path(name, path) {
        return PathBuf::from(name);
    }
    dirs.iter()
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable(candidate))
        .unwrap_or_else(|| PathBuf::from(name))
}

fn on_path(name: &str, path: Option<&OsStr>) -> bool {
    path.is_some_and(|path| std::env::split_paths(path).any(|dir| is_executable(&dir.join(name))))
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn executable(dir: &Path, name: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn programs_off_path_are_found_in_common_install_dirs() {
        let temp = tempfile::tempdir().unwrap();
        let on_path_dir = temp.path().join("on-path");
        let brew = temp.path().join("brew-bin");
        let local = temp.path().join("home/.local/bin");
        executable(&on_path_dir, "codex");
        let claude = executable(&local, "claude");
        let dirs = vec![brew.clone(), local.clone()];
        let path = std::env::join_paths([on_path_dir.as_path()]).unwrap();
        // PATH 上有:照原样按名字启动,不去兜底目录抢。
        assert_eq!(
            resolve_program_in("codex", Some(&path), &dirs),
            PathBuf::from("codex")
        );
        // PATH 上没有:去常见目录找,给绝对路径(官方安装脚本把 claude 放在 ~/.local/bin)。
        assert_eq!(resolve_program_in("claude", Some(&path), &dirs), claude);
        // 哪儿都没有:照原样,由启动处报「不在 PATH 上」。
        assert_eq!(
            resolve_program_in("agy", Some(&path), &dirs),
            PathBuf::from("agy")
        );
        // 同名的非可执行文件不算。
        std::fs::create_dir_all(&brew).unwrap();
        std::fs::write(brew.join("agy"), "not executable").unwrap();
        assert_eq!(resolve_program_in("agy", None, &dirs), PathBuf::from("agy"));
    }

    #[test]
    fn configured_path_wins_and_home_dirs_expand() {
        assert_eq!(
            configured_program("  /custom/claude ", "claude"),
            PathBuf::from("/custom/claude")
        );
        let dirs = common_bin_dirs(Some(Path::new("/home/someone")));
        assert!(dirs.contains(&PathBuf::from("/home/someone/.local/bin")));
        assert!(dirs.contains(&PathBuf::from("/opt/homebrew/bin")));
        assert!(common_bin_dirs(None)
            .iter()
            .all(|dir| !dir.starts_with("~")));
    }

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
