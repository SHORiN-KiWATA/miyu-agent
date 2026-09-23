use super::startup::{remove_file_if_exists, remove_source_block};
use crate::i18n::text as t;
use crate::paths::MiyuPaths;
use anyhow::Result;

const BEGIN_MARKER: &str = "# >>> miyu zsh hook >>>";
const END_MARKER: &str = "# <<< miyu zsh hook <<<";

pub fn hook() -> String {
    super::stamp_hook(
        "zsh-init",
        &format!("{}{}", super::locate::posix_prelude(), body()),
    )
}

fn body() -> &'static str {
    r#"# zsh 是先展开再找命令:一句自然语言里带个没匹配上的通配符,它在展开阶段就报
# 「no matches found」,命令根本没开始找,command_not_found_handler 也就永远不
# 触发,整句话掉在地上(fish 有同一个坑,已同样修法)。所以回车时先看首词。

# 只看首词长什么样,所以用 ${(z)} 做纯词法切分:它不跑命令替换、不展开通配符,
# 放在每次回车上按得住。
__miyu_first_token() {
    local -a words
    words=(${(z)1})
    local token
    for token in $words; do
        # 跳过 FOO=1 这样的环境变量前缀。
        [[ $token == [A-Za-z_]*=* ]] && continue
        print -r -- $token
        return 0
    done
    return 1
}

# 首词是不是一个「展开后还是它自己」的普通词。$ ( ) ~ { } % 引号反斜杠会把首词
# 换成别的东西,判不出来就交回 zsh 自己展开,这里不猜;; & | < > # ^ ! 和空白同理。
# 通配符 * ? [ ] 故意不在名单里:命令位上出现通配符,本来就说明这不是命令名。
__miyu_head_is_plain_word() {
    [[ -n $1 ]] || return 1
    [[ $1 == *[\$\(\)~\{\}%\;\&\|\<\>\#\^\!\'\"\\[:space:]]* ]] && return 1
    return 0
}

__miyu_accept_line() {
    if [[ -o interactive && -n $BUFFER && $BUFFER != *$'\n'* ]]; then
        local head
        head=$(__miyu_first_token "$BUFFER")
        if [[ -n $head ]] && __miyu_head_is_plain_word "$head" \
            && ! whence -- "$head" >/dev/null 2>&1; then
            # 交给 Miyu:清空命令行让 zsh 正常收尾,真正发送放在 precmd 里——
            # 在 zle 小部件里跑一个交互式长命令是不行的。
            __miyu_pending_buffer=$BUFFER
            print -s -- "$BUFFER"
            BUFFER=""
        fi
    fi
    zle .accept-line
}
zle -N accept-line __miyu_accept_line

__miyu_on_prompt() {
    [[ -n ${__miyu_pending_buffer-} ]] || return
    local buffer=$__miyu_pending_buffer
    unset __miyu_pending_buffer
    print -r -- "$buffer"
    print -r -- "$buffer" | miyu --shell-intercept --shell zsh --stdin 2>/dev/null
}
autoload -Uz add-zsh-hook
add-zsh-hook precmd __miyu_on_prompt

# 兜底照旧留着:首词判不出来(带展开符号)而 zsh 展开后确实找不到命令时走这条。
command_not_found_handler() {
    [[ -o interactive ]] || return 127

    local text="$*"
    [[ -n "$text" ]] || return 127
    [[ "$text" != *$'\n'* && "$text" != *$'\r'* ]] || return 127

    miyu --shell-intercept --shell zsh -- "$@" 2>/dev/null
    return 127
}
"#
}

pub fn install(paths: &MiyuPaths) -> Result<()> {
    if let Some(parent) = paths.zsh_hook_file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&paths.zsh_hook_file, hook())?;
    let home = super::startup::home().unwrap_or_default();
    let rc_path = super::startup::install_target("zsh", &home);
    super::startup::prepare_install_target(&rc_path, &home)?;
    super::upsert_source_block(&rc_path, BEGIN_MARKER, END_MARKER, &paths.zsh_hook_file)?;
    println!(
        "{}: {}",
        t("installed zsh hook", "已安装 zsh hook"),
        paths.zsh_hook_file.display()
    );
    println!("{}: {}", t("updated", "已更新"), rc_path.display());
    super::print_reload_hint("zsh", &paths.zsh_hook_file);
    super::locate::warn_if_unreachable("zsh", Some(&rc_path));
    Ok(())
}

pub fn uninstall(paths: &MiyuPaths) -> Result<bool> {
    let removed_file = remove_file_if_exists(&paths.zsh_hook_file)?;
    // 所有候选启动文件都清一遍:老版本写在 `.bashrc`、macOS 上写在
    // `.bash_profile`,卸载得都认得。
    let home = super::startup::home().unwrap_or_default();
    let mut removed_block = false;
    for rc_path in super::startup::candidates("zsh", &home) {
        removed_block |= remove_source_block(&rc_path, BEGIN_MARKER, END_MARKER)?;
    }
    let removed = removed_file || removed_block;
    if removed {
        println!(
            "{}: zsh",
            t("removed Miyu shell hook", "已移除 Miyu shell hook")
        );
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// zsh 和 fish 一样是先展开再找命令:句子里带个没匹配上的通配符,它在展开
    /// 阶段就报 no matches found,command_not_found_handler 永远不触发。所以
    /// 回车时先看首词。判定矩阵在 testkit/zsh-accept-line/run.py(真 zsh 跑)。
    #[test]
    fn zsh_hook_routes_single_line_prose_before_zsh_expands_globs() {
        let hook = hook();
        // 纯词法切分,不跑命令替换、不展开通配符。
        assert!(hook.contains("words=(${(z)1})"));
        assert!(!hook.contains("${(Z)1}"));
        // 通配符不在首词黑名单里:命令位上出现 * ? [ ],本来就说明这不是命令名。
        let class = r##"*[\$\(\)~\{\}%\;\&\|\<\>\#\^\!\'\"\\[:space:]]*"##;
        assert!(hook.contains(class), "首词黑名单变了:\n{hook}");
        for glob in ['*', '?'] {
            assert!(
                !class.trim_matches('*').contains(glob),
                "{glob} 不该进首词黑名单"
            );
        }
        // 小部件只负责判路;真正发送放 precmd,zle 里跑交互式长命令是不行的。
        assert!(hook.contains("zle -N accept-line __miyu_accept_line"));
        assert!(hook.contains("add-zsh-hook precmd __miyu_on_prompt"));
        assert!(hook.contains("BUFFER=\"\""));
        assert!(hook.contains("zle .accept-line"));
        assert!(hook.contains("--shell-intercept --shell zsh --stdin"));
        // 兜底留着:首词判不出来时仍走它。
        assert!(hook.contains("command_not_found_handler()"));
    }

    #[test]
    fn zsh_hook_defines_command_not_found_handler() {
        let hook = hook();
        assert!(hook.contains("command_not_found_handler"));
        assert!(hook.contains("--shell zsh"));
        assert!(hook.contains("return 127"));
    }

    #[test]
    fn zsh_hook_does_not_filter_natural_language_symbols() {
        let hook = hook();
        assert!(!hook.contains("${#text} <= 120"));
        assert!(!hook.contains("miyu_shell_syntax_pattern"));
        assert!(!hook.contains("miyu_leading_pattern"));
    }

    #[test]
    fn remove_source_block_reports_whether_block_was_removed() {
        let temp = tempfile::tempdir().unwrap();
        let rc_path = temp.path().join(".zshrc");
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
        let rc_path = temp.path().join(".zshrc");
        std::fs::write(
            &rc_path,
            format!("before\n{BEGIN_MARKER}\nsource '/old/miyu-hook.zsh'\n{END_MARKER}\nafter\n"),
        )
        .unwrap();
        let hook = temp.path().join("new miyu-hook.zsh");

        crate::shell::upsert_source_block(&rc_path, BEGIN_MARKER, END_MARKER, &hook).unwrap();

        let updated = std::fs::read_to_string(rc_path).unwrap();
        assert!(updated.contains("new miyu-hook.zsh"));
        assert!(!updated.contains("/old/miyu-hook.zsh"));
        assert_eq!(updated.matches(BEGIN_MARKER).count(), 1);
        assert!(updated.starts_with("before\n"));
        assert!(updated.ends_with("after\n"));
    }
}
