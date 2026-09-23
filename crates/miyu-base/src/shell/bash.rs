use super::startup::{remove_file_if_exists, remove_source_block};
use crate::i18n::text as t;
use crate::paths::MiyuPaths;
use anyhow::Result;

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
    Ok(())
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
