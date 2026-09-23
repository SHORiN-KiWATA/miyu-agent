use super::startup::{remove_file_if_exists, remove_source_block};
use crate::i18n::text as t;
use crate::paths::MiyuPaths;
use anyhow::Result;
use std::path::PathBuf;
use std::process::{Command, Stdio};

const BEGIN_MARKER: &str = "# >>> miyu bash hook >>>";
const END_MARKER: &str = "# <<< miyu bash hook <<<";

pub fn hook() -> String {
    super::stamp_hook(
        "bash-init",
        &format!("{}{}", super::locate::posix_prelude(), body()),
    )
}

fn body() -> &'static str {
    r#"command_not_found_handle() {
    [[ $- == *i* ]] || return 127

    local text="$*"
    [[ -n "$text" ]] || return 127
    [[ "$text" != *$'\n'* && "$text" != *$'\r'* ]] || return 127

    miyu --shell-intercept --shell bash -- "$@" 2>/dev/null
    return 127
}
"#
}

pub fn install(paths: &MiyuPaths) -> Result<()> {
    if let Some(parent) = paths.bash_hook_file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&paths.bash_hook_file, hook())?;
    let home = super::startup::home().unwrap_or_default();
    let rc_path = super::startup::install_target("bash", &home);
    super::startup::prepare_install_target(&rc_path, &home)?;
    super::upsert_source_block(&rc_path, BEGIN_MARKER, END_MARKER, &paths.bash_hook_file)?;
    println!(
        "{}: {}",
        t("installed bash hook", "已安装 bash hook"),
        paths.bash_hook_file.display()
    );
    println!("{}: {}", t("updated", "已更新"), rc_path.display());
    super::print_reload_hint("bash", &paths.bash_hook_file);
    super::locate::warn_if_unreachable("bash", Some(&rc_path));
    warn_if_bash_lacks_handler();
    Ok(())
}

/// 自然语言靠 `command_not_found_handle` 接住，那是 bash 4.0 才有的；macOS 自带的
/// bash 3.2 根本不会调它，敲中文直接 `command not found`（09-23 真机）。hook 照装——
/// 兜底找 `miyu` 的那段在 3.2 上照样管用——但装的时候得把话说明白。
fn warn_if_bash_lacks_handler() {
    let Some((major, minor)) = user_bash_version() else {
        return;
    };
    if major >= 4 {
        return;
    }
    println!(
        "{}",
        t(
            "note: bash {version} cannot hand natural-language input to Miyu: that needs command_not_found_handle from bash 4 or newer, and macOS ships bash 3.2. Use zsh (the macOS default) or a newer bash (brew install bash). Calling `miyu` directly still works.",
            "注意：bash {version} 没法把自然语言交给 Miyu——这要靠 bash 4 起才有的 command_not_found_handle，macOS 自带的是 3.2。请改用 zsh（macOS 默认的 shell），或装新版 bash（brew install bash）。直接敲 `miyu` 照常可用。",
        )
        .replace("{version}", &format!("{major}.{minor}"))
    );
}

/// 用户会用的那个 bash：登录 shell 是 bash 就认它，否则认 PATH 上的 `bash`。
fn user_bash_version() -> Option<(u32, u32)> {
    let program = std::env::var_os("SHELL")
        .map(PathBuf::from)
        .filter(|shell| shell.file_name().is_some_and(|name| name == "bash"))
        .unwrap_or_else(|| PathBuf::from("bash"));
    let output = Command::new(program)
        .arg("--version")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    parse_bash_version(&String::from_utf8_lossy(&output.stdout))
}

/// `GNU bash, version 3.2.57(1)-release (arm64-apple-darwin25)` → `(3, 2)`。
fn parse_bash_version(text: &str) -> Option<(u32, u32)> {
    let rest = text.lines().next()?.split("version ").nth(1)?;
    let mut numbers = rest.split(|c: char| !c.is_ascii_digit());
    Some((numbers.next()?.parse().ok()?, numbers.next()?.parse().ok()?))
}

pub fn uninstall(paths: &MiyuPaths) -> Result<bool> {
    let removed_file = remove_file_if_exists(&paths.bash_hook_file)?;
    // 所有候选启动文件都清一遍:老版本写在 `.bashrc`、macOS 上写在
    // `.bash_profile`,卸载得都认得。
    let home = super::startup::home().unwrap_or_default();
    let mut removed_block = false;
    for rc_path in super::startup::candidates("bash", &home) {
        removed_block |= remove_source_block(&rc_path, BEGIN_MARKER, END_MARKER)?;
    }
    let removed = removed_file || removed_block;
    if removed {
        println!(
            "{}: bash",
            t("removed Miyu shell hook", "已移除 Miyu shell hook")
        );
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bash_hook_defines_command_not_found_handler() {
        let hook = hook();
        assert!(hook.contains("command_not_found_handle"));
        assert!(hook.contains("--shell bash"));
        assert!(hook.contains("return 127"));
    }

    #[test]
    fn bash_version_parses_the_first_line_of_version_output() {
        assert_eq!(
            parse_bash_version(
                "GNU bash, version 3.2.57(1)-release (arm64-apple-darwin25)\nCopyright"
            ),
            Some((3, 2))
        );
        assert_eq!(
            parse_bash_version("GNU bash, version 5.3.3(1)-release (x86_64-pc-linux-gnu)"),
            Some((5, 3))
        );
        assert_eq!(parse_bash_version("zsh 5.9 (arm64-apple-darwin25)"), None);
        assert_eq!(parse_bash_version(""), None);
    }

    #[test]
    fn bash_hook_does_not_filter_natural_language_symbols() {
        let hook = hook();
        assert!(!hook.contains("${#text} <= 120"));
        assert!(!hook.contains("miyu_shell_syntax_pattern"));
        assert!(!hook.contains("miyu_leading_pattern"));
    }

    #[test]
    fn remove_source_block_reports_whether_block_was_removed() {
        let temp = tempfile::tempdir().unwrap();
        let rc_path = temp.path().join(".bashrc");
        std::fs::write(
            &rc_path,
            format!("before\n{BEGIN_MARKER}\nsource hook\n{END_MARKER}\nafter\n"),
        )
        .unwrap();

        assert!(remove_source_block(&rc_path, BEGIN_MARKER, END_MARKER).unwrap());
        assert_eq!(
            std::fs::read_to_string(&rc_path).unwrap(),
            "before\nafter\n"
        );
        assert!(!remove_source_block(&rc_path, BEGIN_MARKER, END_MARKER).unwrap());
    }

    #[test]
    fn installing_again_refreshes_an_existing_hook_path() {
        let temp = tempfile::tempdir().unwrap();
        let rc_path = temp.path().join(".bashrc");
        std::fs::write(
            &rc_path,
            format!("before\n{BEGIN_MARKER}\nsource '/old/miyu-hook.sh'\n{END_MARKER}\nafter\n"),
        )
        .unwrap();
        let hook = temp.path().join("new miyu-hook.sh");

        crate::shell::upsert_source_block(&rc_path, BEGIN_MARKER, END_MARKER, &hook).unwrap();

        let updated = std::fs::read_to_string(rc_path).unwrap();
        assert!(updated.contains("new miyu-hook.sh"));
        assert!(!updated.contains("/old/miyu-hook.sh"));
        assert_eq!(updated.matches(BEGIN_MARKER).count(), 1);
        assert!(updated.starts_with("before\n"));
        assert!(updated.ends_with("after\n"));
    }
}
