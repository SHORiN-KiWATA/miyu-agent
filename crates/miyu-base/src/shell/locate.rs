//! hook 怎么找到 `miyu` 这个程序。
//!
//! hook 里一律直接调 `miyu`，报错丢进 `/dev/null`——PATH 上找不到它，自然语言
//! 拦截就**静默失效**，看起来像「集成没生效」（09-23 macOS 真机）。Arch 上装在
//! `/usr/bin`，哪个 shell 都找得到；macOS 上 Homebrew 装在 `/opt/homebrew/bin`，
//! 只有配过 `brew shellenv` 的 shell 才找得到，fish 常常没配。
//!
//! 所以 hook 开头先兜一道：PATH 上没有，就去几个常见安装目录里找，找到了定义
//! 一个同名函数指过去（顺带让手敲 `miyu` 也能用）。
//!
//! **hook 里不写本机的绝对路径**：`sync_installed_hooks` 每次启动都会把内容不同
//! 的 hook 整份重写，而 hook 的位置跟着真家走——写进「当前这个二进制」的路径，
//! 在 worktree 里跑一次测试二进制就会把用户真正的 hook 改成指向它，机器上装着
//! 两份 miyu 时还会来回改。这里只放一张固定的目录表，所有机器逐字节相同。

use std::path::PathBuf;

/// PATH 上找不到时依次去找的目录（写进 hook 的就是这张表）。
/// Apple Silicon 的 Homebrew、Intel 的 Homebrew、Linuxbrew、`pip`/手装常用的
/// `~/.local/bin`、`cargo install`。
const FALLBACK_DIRS: &[&str] = &[
    "/opt/homebrew/bin",
    "/usr/local/bin",
    "/home/linuxbrew/.linuxbrew/bin",
    "~/.local/bin",
    "~/.cargo/bin",
];

/// fish hook 开头那段兜底。
pub(super) fn fish_prelude() -> String {
    let dirs = FALLBACK_DIRS.join(" ");
    format!(
        "# PATH 上没有 miyu 时去常见安装目录里找(Homebrew、~/.local、cargo)。\n\
if not type -q miyu\n    for __miyu_dir in {dirs}\n        if test -x $__miyu_dir/miyu\n            set -g __miyu_bin $__miyu_dir/miyu\n            function miyu\n                $__miyu_bin $argv\n            end\n            break\n        end\n    end\n    set -e __miyu_dir\nend\n\n"
    )
}

/// bash / zsh hook 开头那段兜底（两边语法在这几行上一致）。
pub(super) fn posix_prelude() -> String {
    let dirs = FALLBACK_DIRS
        .iter()
        .map(|dir| match dir.strip_prefix("~/") {
            Some(rest) => format!("\"$HOME/{rest}\""),
            None => (*dir).to_string(),
        })
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "# PATH 上没有 miyu 时去常见安装目录里找(Homebrew、~/.local、cargo)。\n\
if ! command -v miyu >/dev/null 2>&1; then\n    for __miyu_dir in {dirs}; do\n        if [ -x \"$__miyu_dir/miyu\" ]; then\n            __MIYU_BIN=\"$__miyu_dir/miyu\"\n            miyu() {{ \"$__MIYU_BIN\" \"$@\"; }}\n            break\n        fi\n    done\n    unset __miyu_dir\nfi\n\n"
    )
}

/// 装好的 hook 在这台机器上找不找得到 `miyu`：PATH 上有，或者在兜底目录里。
///
/// 看的是**本进程**的 PATH，和用户日后打开的那个 shell 不一定一样——所以只拿来
/// 决定要不要提醒，不拦安装。
pub(super) fn reachable() -> bool {
    super::command_exists_in_path("miyu")
        || FALLBACK_DIRS
            .iter()
            .filter_map(|dir| expand(dir))
            .any(|dir| super::is_executable_file(&dir.join("miyu")))
}

fn expand(dir: &str) -> Option<PathBuf> {
    match dir.strip_prefix("~/") {
        Some(rest) => super::startup::home().map(|home| home.join(rest)),
        None => Some(PathBuf::from(dir)),
    }
}

/// hook 找不到 `miyu` 时，装完给一句能照抄的修法。
pub(super) fn warn_if_unreachable(shell: &str, startup_file: Option<&std::path::Path>) {
    if reachable() {
        return;
    }
    let Some(dir) = crate::paths::miyu_executable()
        .ok()
        .and_then(|exe| exe.parent().map(PathBuf::from))
    else {
        return;
    };
    let fix = match (shell, startup_file) {
        ("fish", _) => format!("fish_add_path {}", super::fish_quote(&dir)),
        (_, Some(file)) => format!(
            "echo 'export PATH={}:$PATH' >> {}",
            super::shell_quote(&dir),
            super::shell_quote(file)
        ),
        _ => format!("export PATH={}:$PATH", super::shell_quote(&dir)),
    };
    println!(
        "{}\n  {fix}",
        crate::i18n::text(
            "warning: `miyu` is not on PATH, so the hook cannot reach it and natural-language input will be ignored. Add its directory to PATH:",
            "注意：PATH 上找不到 `miyu`，hook 调不到它，自然语言输入会被忽略。把它所在的目录加进 PATH：",
        )
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// hook 里的兜底段只能是一张固定目录表，不能带上本机路径（见模块文档）。
    #[test]
    fn preludes_carry_no_machine_specific_path() {
        let home = super::super::startup::home().unwrap_or_default();
        for prelude in [fish_prelude(), posix_prelude()] {
            if !home.as_os_str().is_empty() {
                assert!(
                    !prelude.contains(&*home.to_string_lossy()),
                    "兜底段里出现了本机家目录: {prelude}"
                );
            }
            assert!(prelude.contains("/opt/homebrew/bin"));
        }
    }
}
