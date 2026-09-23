//! shell 启动文件：那段 `source` 写进哪个文件，检测 / 卸载时又去哪些文件里找。
//!
//! Linux 上交互 bash 读 `~/.bashrc`。**macOS 的终端和 SSH 开的都是登录 shell**，
//! 只读 `~/.bash_profile`（没有就 `~/.bash_login`，再没有才 `~/.profile`），根本
//! 不碰 `.bashrc`——写进 `.bashrc` 在那儿等于没装（09-23 macOS 真机）。zsh 的启动
//! 文件跟着 `ZDOTDIR` 走，没设才是家目录。

use anyhow::Result;
use std::path::{Path, PathBuf};

/// 用户家目录。取不到时（极少见）返回 `None`，调用方原样退回相对路径的老行为。
pub(super) fn home() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf())
}

fn zdotdir() -> Option<PathBuf> {
    std::env::var_os("ZDOTDIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// 装的时候那段 `source` 写进哪个文件。
pub fn install_target(shell: &str, home: &Path) -> PathBuf {
    install_target_for(shell, home, cfg!(target_os = "macos"), zdotdir())
}

pub(super) fn install_target_for(
    shell: &str,
    home: &Path,
    macos: bool,
    zdotdir: Option<PathBuf>,
) -> PathBuf {
    match shell {
        "zsh" => zdotdir.unwrap_or_else(|| home.to_path_buf()).join(".zshrc"),
        _ if macos => [".bash_profile", ".bash_login"]
            .iter()
            .map(|name| home.join(name))
            .find(|path| path.exists())
            .unwrap_or_else(|| home.join(".bash_profile")),
        _ => home.join(".bashrc"),
    }
}

/// 可能放着那段 `source` 的全部文件。检测、卸载、迁移都得看全：换过平台规则、
/// 手挪过、或者老版本写在 `.bashrc` 里的，都要认得、都要清得掉。
pub fn candidates(shell: &str, home: &Path) -> Vec<PathBuf> {
    candidates_for(shell, home, zdotdir())
}

pub(super) fn candidates_for(shell: &str, home: &Path, zdotdir: Option<PathBuf>) -> Vec<PathBuf> {
    let mut files = match shell {
        "zsh" => vec![
            zdotdir.unwrap_or_else(|| home.to_path_buf()).join(".zshrc"),
            home.join(".zshrc"),
        ],
        _ => [".bashrc", ".bash_profile", ".bash_login", ".profile"]
            .iter()
            .map(|name| home.join(name))
            .collect(),
    };
    files.dedup();
    files
}

/// 要新建 macOS 的 `.bash_profile` 时，先把 `~/.profile` 接上。
///
/// bash 登录时只读第一个找得到的：`.bash_profile` → `.bash_login` → `.profile`。
/// 原来只有 `.profile` 的人，我们一建 `.bash_profile`，他放在 `.profile` 里的东西
/// （PATH、别名）就再也不会被读——得在新文件里先 source 它。
pub(super) fn prepare_install_target(target: &Path, home: &Path) -> Result<()> {
    let profile = home.join(".profile");
    let creating_bash_profile =
        !target.exists() && target.file_name() == Some(std::ffi::OsStr::new(".bash_profile"));
    if creating_bash_profile && profile.exists() {
        std::fs::write(
            target,
            "# Keep loading ~/.profile: bash skips it once this file exists (added by Miyu).\n[ -r ~/.profile ] && . ~/.profile\n\n",
        )?;
    }
    Ok(())
}

pub(super) fn remove_source_block(rc_path: &Path, begin: &str, end: &str) -> Result<bool> {
    let Ok(existing) = std::fs::read_to_string(rc_path) else {
        return Ok(false);
    };
    let Some(begin_index) = existing.find(begin) else {
        return Ok(false);
    };
    let Some(end_relative) = existing[begin_index..].find(end) else {
        return Ok(false);
    };
    let mut end_index = begin_index + end_relative + end.len();
    if existing.as_bytes().get(end_index) == Some(&b'\r') {
        end_index += 1;
    }
    if existing.as_bytes().get(end_index) == Some(&b'\n') {
        end_index += 1;
    }
    let mut updated = String::new();
    updated.push_str(&existing[..begin_index]);
    updated.push_str(&existing[end_index..]);
    super::write_rc_atomic(rc_path, &updated)?;
    Ok(true)
}

pub(super) fn remove_file_if_exists(path: &Path) -> Result<bool> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bash_goes_to_the_login_file_on_macos_and_bashrc_elsewhere() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        assert_eq!(
            install_target_for("bash", home, false, None),
            home.join(".bashrc")
        );
        assert_eq!(
            install_target_for("bash", home, true, None),
            home.join(".bash_profile")
        );
        // 已经有 `.bash_login`、没有 `.bash_profile`:bash 读的是它,就写它。
        std::fs::write(home.join(".bash_login"), "").unwrap();
        assert_eq!(
            install_target_for("bash", home, true, None),
            home.join(".bash_login")
        );
        std::fs::write(home.join(".bash_profile"), "").unwrap();
        assert_eq!(
            install_target_for("bash", home, true, None),
            home.join(".bash_profile")
        );
    }

    #[test]
    fn zsh_follows_zdotdir() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        let zdot = home.join("zdot");
        assert_eq!(
            install_target_for("zsh", home, true, None),
            home.join(".zshrc")
        );
        assert_eq!(
            install_target_for("zsh", home, false, Some(zdot.clone())),
            zdot.join(".zshrc")
        );
        assert_eq!(
            candidates_for("zsh", home, Some(zdot.clone())),
            vec![zdot.join(".zshrc"), home.join(".zshrc")]
        );
        assert_eq!(candidates_for("zsh", home, None), vec![home.join(".zshrc")]);
    }

    #[test]
    fn a_new_bash_profile_keeps_loading_the_old_profile() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        let target = home.join(".bash_profile");
        // 没有 `.profile`:不预写任何东西。
        prepare_install_target(&target, home).unwrap();
        assert!(!target.exists());
        // 有 `.profile`:新建的 `.bash_profile` 先接上它。
        std::fs::write(home.join(".profile"), "export A=1\n").unwrap();
        prepare_install_target(&target, home).unwrap();
        assert!(std::fs::read_to_string(&target)
            .unwrap()
            .contains(". ~/.profile"));
        // 已经存在的不动。
        std::fs::write(&target, "mine\n").unwrap();
        prepare_install_target(&target, home).unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "mine\n");
    }

    #[test]
    fn remove_file_if_exists_reports_whether_file_was_removed() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("hook.sh");
        assert!(!remove_file_if_exists(&path).unwrap());
        std::fs::write(&path, "x").unwrap();
        assert!(remove_file_if_exists(&path).unwrap());
        assert!(!remove_file_if_exists(&path).unwrap());
    }
}
