pub mod bash;
pub mod fish;
mod locate;
pub mod startup;
pub mod zsh;

use crate::i18n::text as t;
use crate::paths::MiyuPaths;
use anyhow::{bail, Context, Result};
use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

const BASH_BEGIN_MARKER: &str = "# >>> miyu bash hook >>>";
const BASH_END_MARKER: &str = "# <<< miyu bash hook <<<";
const ZSH_BEGIN_MARKER: &str = "# >>> miyu zsh hook >>>";
const ZSH_END_MARKER: &str = "# <<< miyu zsh hook <<<";

/// 原子写用户 shell 启动文件:写回瞬间崩溃不能把 .bashrc/.zshrc 留成
/// 截断的半个文件。保留原文件权限。
pub(super) fn write_rc_atomic(rc_path: &Path, content: &str) -> Result<()> {
    let parent = rc_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let temp = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("creating a temp file next to {}", rc_path.display()))?;
    fs::write(temp.path(), content)?;
    if let Ok(metadata) = fs::metadata(rc_path) {
        let _ = fs::set_permissions(temp.path(), metadata.permissions());
    }
    temp.persist(rc_path)
        .map(|_| ())
        .with_context(|| format!("updating shell startup file {}", rc_path.display()))
}

pub(super) fn upsert_source_block(
    rc_path: &Path,
    begin: &str,
    end: &str,
    hook_file: &Path,
) -> Result<()> {
    let existing = read_optional_text(rc_path)?;
    let block = source_block(begin, end, hook_file);
    if let Some(updated) = replace_marked_block(&existing, begin, end, &block)? {
        if updated != existing {
            write_rc_atomic(rc_path, &updated)?;
        }
        return Ok(());
    }
    if let Some(parent) = rc_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(rc_path)?;
    if !existing.ends_with('\n') && !existing.is_empty() {
        writeln!(file)?;
    }
    file.write_all(block.as_bytes())?;
    Ok(())
}

/// Refreshes only hook blocks installed by an older Miyu layout. It never
/// enables shell integration for a user who did not already have it enabled.
pub(crate) fn refresh_migrated_hook_sources(
    home: &Path,
    bash_hook: Option<&Path>,
    zsh_hook: Option<&Path>,
) -> Result<()> {
    // 那段 source 可能在任何一个候选启动文件里(macOS 上 bash 装在
    // `.bash_profile`),只动已经写着标记块的那些。
    if let Some(hook) = bash_hook {
        for rc in startup::candidates("bash", home) {
            refresh_source_block_if_present(&rc, BASH_BEGIN_MARKER, BASH_END_MARKER, hook)?;
        }
    }
    if let Some(hook) = zsh_hook {
        for rc in startup::candidates("zsh", home) {
            refresh_source_block_if_present(&rc, ZSH_BEGIN_MARKER, ZSH_END_MARKER, hook)?;
        }
    }
    Ok(())
}

fn refresh_source_block_if_present(
    rc_path: &Path,
    begin: &str,
    end: &str,
    hook_file: &Path,
) -> Result<()> {
    let existing = read_optional_text(rc_path)?;
    if existing.is_empty() {
        return Ok(());
    }
    let block = source_block(begin, end, hook_file);
    let Some(updated) = replace_marked_block(&existing, begin, end, &block)? else {
        return Ok(());
    };
    if updated != existing {
        write_text_atomically(rc_path, &updated)
            .with_context(|| format!("refreshing migrated shell hook in {}", rc_path.display()))?;
    }
    Ok(())
}

fn write_text_atomically(path: &Path, contents: &str) -> Result<()> {
    let parent = path
        .parent()
        .context("shell startup file has no parent directory")?;
    let mode = fs::symlink_metadata(path)
        .ok()
        .map(|metadata| metadata.permissions().mode() & 0o7777)
        .unwrap_or(0o600);
    let temporary = parent.join(format!(
        ".miyu-hook-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(mode)
        .open(&temporary)
        .with_context(|| {
            format!(
                "creating temporary shell startup file {}",
                temporary.display()
            )
        })?;
    if let Err(error) = file
        .write_all(contents.as_bytes())
        .and_then(|()| file.sync_all())
    {
        let _ = fs::remove_file(&temporary);
        return Err(anyhow::Error::new(error))
            .with_context(|| format!("writing temporary shell startup file {}", path.display()));
    }
    if let Err(error) = fs::rename(&temporary, path) {
        // 换不过去就把半成品收走:留在 conf.d 里是个不会被 source 的隐藏文件
        // (fish 只认 `*.fish`),但没必要攒垃圾。
        let _ = fs::remove_file(&temporary);
        return Err(anyhow::Error::new(error))
            .with_context(|| format!("installing updated shell startup file {}", path.display()));
    }
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}

fn read_optional_text(path: &Path) -> Result<String> {
    match fs::read_to_string(path) {
        Ok(value) => Ok(value),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(error.into()),
    }
}

fn source_block(begin: &str, end: &str, hook_file: &Path) -> String {
    let hook = shell_quote(hook_file);
    format!("{begin}\n[ -r {hook} ] && source {hook}\n{end}\n")
}

fn replace_marked_block(
    existing: &str,
    begin: &str,
    end: &str,
    replacement: &str,
) -> Result<Option<String>> {
    let Some(begin_index) = existing.find(begin) else {
        if existing.contains(end) {
            bail!("shell startup file contains a Miyu end marker without its begin marker");
        }
        return Ok(None);
    };
    let Some(end_relative) = existing[begin_index..].find(end) else {
        bail!("shell startup file contains an incomplete Miyu hook block");
    };
    let mut end_index = begin_index + end_relative + end.len();
    if existing.as_bytes().get(end_index) == Some(&b'\r') {
        end_index += 1;
    }
    if existing.as_bytes().get(end_index) == Some(&b'\n') {
        end_index += 1;
    }
    let mut updated = String::with_capacity(
        existing.len().saturating_sub(end_index - begin_index) + replacement.len(),
    );
    updated.push_str(&existing[..begin_index]);
    updated.push_str(replacement);
    updated.push_str(&existing[end_index..]);
    Ok(Some(updated))
}

pub fn print_reload_hint(shell: &str, hook_file: &Path) {
    let source = match shell {
        "fish" => format!("source {}", fish_quote(hook_file)),
        "bash" | "zsh" => format!("source {}", shell_quote(hook_file)),
        _ => return,
    };
    if current_parent_shell().as_deref() == Some(shell) {
        println!(
            "{}: {}",
            t(
                "run this in the current terminal to load it now",
                "在当前终端运行此命令可立即加载"
            ),
            source
        );
    } else {
        println!(
            "{}",
            t(
                "open a new matching shell session for the hook to take effect",
                "新开对应 shell 会话后 hook 将生效"
            )
        );
    }
}

/// 往上数几层父进程,看看是谁在跑我们——装 hook 之后那句「当前 shell 要不要
/// source 一下」靠它判断。
///
/// 走 `/proc`,**macOS 上没有这个文件系统**,于是每一层都拿不到、恒返回 `None`。
/// 拿不到就退回 `$SHELL`:它说的是登录 shell 而不是当前这一层,不如进程树准
/// (在 bash 里 `exec fish` 之后就会答错),但比什么都不知道强。
pub fn current_parent_shell() -> Option<String> {
    let mut pid = std::process::id();
    for _ in 0..8 {
        let Some(parent) = parent_pid(pid) else {
            break;
        };
        let Some(name) = process_name(parent) else {
            break;
        };
        if is_known_shell(&name) {
            return Some(name);
        }
        pid = parent;
    }
    shell_from_env(std::env::var_os("SHELL").as_deref().map(Path::new))
}

fn is_known_shell(name: &str) -> bool {
    matches!(name, "fish" | "bash" | "zsh")
}

/// `$SHELL` → 认得的 shell 名。纯函数,好测。
fn shell_from_env(shell: Option<&Path>) -> Option<String> {
    let name = shell?.file_name()?.to_str()?;
    is_known_shell(name).then(|| name.to_string())
}

fn parent_pid(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after_name = stat.rsplit_once(") ")?.1;
    after_name.split_whitespace().nth(1)?.parse().ok()
}

fn process_name(pid: u32) -> Option<String> {
    std::fs::read_to_string(format!("/proc/{pid}/comm"))
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub(super) fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

pub(super) fn fish_quote(path: &Path) -> String {
    format!(
        "'{}'",
        path.display()
            .to_string()
            .replace('\\', "\\\\")
            .replace('\'', "\\'")
    )
}

pub fn is_shell_command(input: &str, shell_name: &str) -> bool {
    let Some((command, rest)) = first_command_token_with_rest(input) else {
        return false;
    };
    if ambiguous_command_tail_looks_like_message(&command, rest) {
        return false;
    }
    is_shell_keyword_or_builtin(&command, shell_name)
        || is_explicit_command_path(&command)
        || command_exists_in_path(&command)
}

fn first_command_token_with_rest(input: &str) -> Option<(String, &str)> {
    let mut offset = 0;
    while let Some(token) = next_fish_like_token(input, &mut offset) {
        if is_env_assignment(&token) {
            continue;
        }
        return Some((token, input.get(offset..).unwrap_or("")));
    }
    None
}

fn ambiguous_command_tail_looks_like_message(command: &str, rest: &str) -> bool {
    if !matches!(
        command,
        "time" | "test" | "date" | "which" | "type" | "command" | "history"
    ) {
        return false;
    }
    let rest = rest.trim();
    !rest.is_empty()
        && rest
            .chars()
            .any(|ch| ch == '?' || ch == '？' || is_cjk_char(ch))
}

fn is_cjk_char(ch: char) -> bool {
    matches!(
        ch,
        '\u{3400}'..='\u{4DBF}'
            | '\u{4E00}'..='\u{9FFF}'
            | '\u{F900}'..='\u{FAFF}'
            | '\u{20000}'..='\u{2A6DF}'
            | '\u{2A700}'..='\u{2B73F}'
            | '\u{2B740}'..='\u{2B81F}'
            | '\u{2B820}'..='\u{2CEAF}'
    )
}

fn next_fish_like_token(input: &str, offset: &mut usize) -> Option<String> {
    let mut index = *offset;
    loop {
        let rest = input.get(index..)?;
        let Some(ch) = rest.chars().next() else {
            *offset = input.len();
            return None;
        };
        if ch.is_whitespace() {
            index += ch.len_utf8();
            continue;
        }
        if ch == '#' {
            index += ch.len_utf8();
            while let Some(next) = input.get(index..).and_then(|rest| rest.chars().next()) {
                index += next.len_utf8();
                if next == '\n' || next == '\r' {
                    break;
                }
            }
            continue;
        }
        break;
    }

    let mut token = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;
    let mut consumed = input.len();

    for (relative, ch) in input[index..].char_indices() {
        let absolute = index + relative;
        if escaped {
            token.push(ch);
            escaped = false;
            continue;
        }
        if ch == '\\' && !in_single {
            escaped = true;
            continue;
        }
        if ch == '\'' && !in_double {
            in_single = !in_single;
            continue;
        }
        if ch == '"' && !in_single {
            in_double = !in_double;
            continue;
        }
        if !in_single
            && !in_double
            && (ch.is_whitespace() || matches!(ch, ';' | '|' | '&' | '<' | '>'))
        {
            consumed = absolute + ch.len_utf8();
            if token.is_empty() {
                token.push(ch);
            }
            break;
        }
        token.push(ch);
    }

    *offset = consumed;
    if token.is_empty() {
        None
    } else {
        Some(token)
    }
}

fn is_env_assignment(token: &str) -> bool {
    let Some((name, _)) = token.split_once('=') else {
        return false;
    };
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || first.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

fn is_shell_keyword_or_builtin(command: &str, shell_name: &str) -> bool {
    let common = matches!(
        command,
        "alias"
            | "bg"
            | "break"
            | "builtin"
            | "case"
            | "cd"
            | "command"
            | "continue"
            | "else"
            | "end"
            | "exec"
            | "exit"
            | "false"
            | "fg"
            | "for"
            | "function"
            | "functions"
            | "history"
            | "if"
            | "jobs"
            | "not"
            | "or"
            | "and"
            | "read"
            | "return"
            | "set"
            | "source"
            | "status"
            | "switch"
            | "test"
            | "time"
            | "true"
            | "while"
    );
    common
        || (shell_name == "fish"
            && matches!(
                command,
                "abbr"
                    | "argparse"
                    | "begin"
                    | "bind"
                    | "block"
                    | "contains"
                    | "count"
                    | "disown"
                    | "emit"
                    | "eval"
                    | "math"
                    | "random"
                    | "string"
                    | "type"
                    | "ulimit"
            ))
}

/// 显式写出的路径要**真的可执行**才算命令。
///
/// 原先只看形状(以 `/`、`./`、`../`、`~/` 开头就算),而紧挨着的
/// `command_exists_in_path` 对光名字反倒要验 `is_executable_file`——裸名字
/// 严格、显式路径反而照单全收,这个不对称正是病灶:把
/// `/home/shorin/Downloads/1.png` 粘在多行输入的头一行,整段就被判成 shell
/// 命令交给 fish 逐行执行,图片路径各报一次"存在,但不是一个可执行文件",
/// 只有末尾那句中文漏进 Miyu,图全丢了(08-26 用户实测)。
///
/// 这条判定只在**多行**缓冲区上被问到(单行走 fish 自己的
/// `fish_command_not_found`),所以路径不存在时判成"给 Miyu"是安全的:多行且
/// 首个 token 是个不可执行的路径,基本只可能是粘进来的内容。
fn is_explicit_command_path(command: &str) -> bool {
    let shaped = command.starts_with('/')
        || command.starts_with("./")
        || command.starts_with("../")
        || command.starts_with("~/");
    if !shaped {
        return false;
    }
    let expanded = match command.strip_prefix("~/") {
        Some(rest) => match env::var_os("HOME") {
            Some(home) => PathBuf::from(home).join(rest),
            None => return false,
        },
        None => PathBuf::from(command),
    };
    is_executable_file(&expanded)
}

pub(super) fn command_exists_in_path(command: &str) -> bool {
    if command.is_empty() || command.contains('/') {
        return false;
    }
    let Some(paths) = env::var_os("PATH") else {
        return false;
    };
    env::split_paths(&paths).any(|dir| is_executable_file(&dir.join(command)))
}

pub(super) fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
}

/// hook 文件顶上那行标记的前缀。整行长这样：
///
/// ```text
/// # miyu shell hook · v0.6.0 · 7f3a1c2e
/// ```
///
/// 版本认发行版本，指纹认「同一个版本下这段脚本改没改」——开发版、手改过的
/// 都认得出来。外面的工具（打包脚本之类）读这一行就知道装着的是哪一份。
pub const HOOK_STAMP_PREFIX: &str = "# miyu shell hook · v";

/// 这段脚本的内容指纹。取 blake3 前 8 位十六进制：够分辨，又短到能一眼看完。
pub fn hook_fingerprint(body: &str) -> String {
    blake3::hash(body.as_bytes()).to_hex()[..8].to_string()
}

/// 给一段 hook 正文打上标记，返回该写进文件的全文。
///
/// `reinstall` 是重装用的子命令名（`fish-init` / `bash-init` / `zsh-init`），
/// 写在第二行里——用户看到这个文件时该知道去哪儿重新生成，而不是手改它。
pub fn stamp_hook(reinstall: &str, body: &str) -> String {
    format!(
        "{HOOK_STAMP_PREFIX}{version} · {fingerprint}\n# {note} miyu {reinstall}\n\n{body}",
        version = env!("CARGO_PKG_VERSION"),
        fingerprint = hook_fingerprint(body),
        note = t(
            "generated by Miyu, do not edit by hand; reinstall with",
            "由 Miyu 生成，勿手改；重装",
        ),
    )
}

/// 从一份 hook 全文里读出 `(版本, 指纹)`。没有标记（更老的版本装的）返回 `None`。
pub fn installed_hook_stamp(text: &str) -> Option<(&str, &str)> {
    let line = text.lines().next()?.strip_prefix(HOOK_STAMP_PREFIX)?;
    let (version, fingerprint) = line.split_once(" · ")?;
    Some((version.trim(), fingerprint.trim()))
}

/// 已经装着的 hook 和当前这个二进制对不上就重写，返回更新了哪几个 shell。
///
/// 两条规矩：
///
/// - **只动已经存在的文件**。没装过 shell 集成的用户不会被我们自作主张装上，
///   `.bashrc` / `.zshrc` 里那段 source 也一个字不碰（那是用户的启动文件，
///   只有 `miyu bash-init` 有资格写）。
/// - **整份换掉**。判据是全文不等，比只比版本和指纹更严，也把「同一版本下
///   脚本改过」和「手改过」一起盖住了。
pub fn sync_installed_hooks(paths: &MiyuPaths) -> Vec<&'static str> {
    let mut updated = Vec::new();
    // hook 文本按需生成：没装过的那几个连字符串都不用拼（三份加起来十几 KB，
    // 而这段代码在**每次** miyu 启动时都会走一遍）。
    let wanted: [(&'static str, &Path, fn() -> String); 3] = [
        (
            "fish",
            paths.fish_hook_file.as_path(),
            fish::hook as fn() -> String,
        ),
        (
            "bash",
            paths.bash_hook_file.as_path(),
            bash::hook as fn() -> String,
        ),
        (
            "zsh",
            paths.zsh_hook_file.as_path(),
            zsh::hook as fn() -> String,
        ),
    ];
    for (name, path, render) in wanted {
        match sync_installed_hook(name, path, render) {
            Ok(true) => updated.push(name),
            Ok(false) => {}
            Err(error) => {
                // 更新失败不该拦住这次调用:用户是来跑别的事的,hook 老一版
                // 还能用。留个日志就够了。
                tracing::warn!(
                    shell = name,
                    path = %path.display(),
                    error = %error,
                    "could not refresh the installed shell hook"
                );
            }
        }
    }
    updated
}

fn sync_installed_hook(shell: &str, path: &Path, render: fn() -> String) -> Result<bool> {
    let Ok(existing) = fs::read_to_string(path) else {
        // 没装过（或读不了、不是 UTF-8）——不是我们该管的。
        return Ok(false);
    };
    let current = &render();
    if existing == *current {
        return Ok(false);
    }
    // 我们**要写进去**的那份自己得像个 hook。空的、被截短的、少了拦截那一句的,
    // 语法上都合法(空文件解析得过),但装上去等于把 shell 集成悄悄关掉。宁可
    // 留着老的。
    if !current.contains("miyu --shell-intercept") {
        bail!("refusing to install a {shell} hook that does not intercept anything");
    }
    if !looks_generated(&existing) {
        bail!(
            "{} does not look like a Miyu-generated hook; leaving it alone",
            path.display()
        );
    }
    if installed_is_newer(&existing) {
        // 机器上放着两个版本的 miyu（包管理器一份、~/.local/bin 一份）时，
        // 老的那个每跑一次就把 hook 拽回旧版。只往前，不往后。
        return Ok(false);
    }
    // 语法闸放在**真要写的这一刻**：它要起一个 shell 进程，搁在比对之前就是
    // 每跑一条 miyu 命令都白起三个。走到这儿说明内容确实不一样，很少见。
    if !passes_syntax_check(shell, current) {
        bail!(
            "the {shell} hook this build would install does not parse; keeping the installed one"
        );
    }
    // 跟着符号链接写：有人把 hook 链进自己的 dotfiles 仓库，`rename` 换的是
    // 目录项，链接会被换成普通文件（还继承了链接的 0777 权限），那份 dotfiles
    // 就此失联。`fish-init` 走 `fs::write`，一直是写穿链接的，这里对齐。
    let target = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    write_text_atomically(&target, current)
        .with_context(|| format!("refreshing shell hook {}", target.display()))?;
    Ok(true)
}

/// 这个文件是不是**我们生成的**。
///
/// 路径是写死的（`conf.d/miyu.fish` 之类），万一有人自己在那个名字下放了别的
/// 东西，自动更新不该把它吃掉——`fish-init` 是用户明确要求的，照旧整份覆盖；
/// 自动那条路得保守。
///
/// 三种算：带我们的标记；含 `miyu --shell-intercept`（三个 hook 从第一版起都
/// 有这一句，老版本装的没标记但有它）；空文件（被截断的残骸，谁的内容都不是，
/// 正该被治好）。
fn looks_generated(existing: &str) -> bool {
    existing.trim().is_empty()
        || installed_hook_stamp(existing).is_some()
        || existing.contains("miyu --shell-intercept")
}

/// 装着的那份是不是比我们**新**。读不出版本号就当不是。
fn installed_is_newer(existing: &str) -> bool {
    let Some((installed, _)) = installed_hook_stamp(existing) else {
        return false;
    };
    match (
        version_triple(installed),
        version_triple(env!("CARGO_PKG_VERSION")),
    ) {
        (Some(installed), Some(ours)) => installed > ours,
        _ => false,
    }
}

fn version_triple(version: &str) -> Option<(u64, u64, u64)> {
    let core = version.split(['-', '+']).next()?;
    let mut parts = core.split('.');
    let mut next = || parts.next()?.parse::<u64>().ok();
    Some((next()?, next()?, next()?))
}

/// 写进去之前先让那个 shell 自己解析一遍。
///
/// 自动更新把「发了一份坏 hook」的后果放大了：以前只有主动跑 `fish-init` 的人
/// 中招，现在是所有人升级后第一次跑 miyu 就中招——而 `conf.d/miyu.fish` 是
/// **每个 fish 启动都 source** 的，坏掉就是开不了终端。所以换之前用
/// `fish -n` / `bash -n` / `zsh -n` 验一遍语法，过不了就不换：留着的那份老归老，
/// 至少是能跑的。
///
/// 那个 shell 没装就跳过——验不了不等于不该装（`fish-init` 一直也是直接写的）。
fn passes_syntax_check(shell: &str, contents: &str) -> bool {
    let Ok(file) = tempfile::Builder::new()
        .prefix("miyu-hook-check")
        .tempfile()
    else {
        return true;
    };
    if fs::write(file.path(), contents).is_err() {
        return true;
    }
    let status = std::process::Command::new(shell)
        .arg("-n")
        .arg(file.path())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    match status {
        Ok(status) => status.success(),
        // 没装那个 shell（或起不来）：验不了，按能写处理。
        Err(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn detects_safe_natural_language() {
        assert!(looks_like_natural_language("帮我查一下 niri 输入法"));
        assert!(looks_like_natural_language(
            "why is fcitx candidate window small"
        ));
    }

    #[test]
    fn accepts_command_not_found_text_without_syntax_filtering() {
        assert!(looks_like_natural_language(
            "这样写可以吗？假设我们输入一个字母`x`"
        ));
        assert!(looks_like_natural_language(
            "我好像在输入里加一个左斜杠就会导致输入不被传给miyu/对吗？"
        ));
        assert!(looks_like_natural_language(
            "软件需要适配 Wayland 的 `text-input` 协议，输入法要支持 $GTK_IM_MODULE 吗？"
        ));
        assert!(looks_like_natural_language(
            "GTK_IM_MODULE=fcitx 是什么意思？"
        ));
        assert!(looks_like_natural_language(
            "./target/release/miyu 查询为什么失败？"
        ));
    }

    #[test]
    fn rejects_empty_or_multiline_text() {
        assert!(!looks_like_natural_language(""));
        assert!(!looks_like_natural_language("   "));
        assert!(!looks_like_natural_language("第一行\n第二行"));
    }

    #[test]
    fn classifies_commands_as_shell() {
        assert!(is_shell_command("echo hi", "fish"));
        assert!(is_shell_command("cd /tmp", "fish"));
        assert!(is_shell_command("FOO=bar cargo check", "fish"));
        assert!(is_shell_command("# comment\nls", "fish"));
        // 真实存在且可执行的显式路径。原来写的是 ./target/release/miyu,
        // 依赖本机有没有 release 产物,换成必然存在的系统命令。
        assert!(is_shell_command("/bin/sh -c true", "fish"));
        assert!(is_shell_command("for item in a b", "fish"));
        assert!(is_shell_command("time cargo check", "fish"));
        assert!(is_shell_command(
            "sudo pacman -U --noconfirm \\\n  /tmp/package.pkg.tar.zst",
            "fish"
        ));
    }

    #[test]
    fn classifies_messages_as_miyu() {
        assert!(!is_shell_command("你觉得 a;b 是什么意思", "fish"));
        assert!(!is_shell_command("解释 <tag> 是什么", "fish"));
        assert!(!is_shell_command("第一行\n第二行", "fish"));
        assert!(!is_shell_command("# note\n解释一下这个问题", "fish"));
        assert!(!is_shell_command("time 是什么命令？", "fish"));
        assert!(!is_shell_command(
            "this-command-probably-does-not-exist",
            "fish"
        ));
        assert!(!is_shell_command(
            "GTK_IM_MODULE=fcitx 是什么意思？",
            "fish"
        ));
        assert!(!is_shell_command(r"A\=是真的\这个短语", "fish"));
    }

    /// 多行粘贴里首个 token 是不可执行的路径时必须交给 Miyu(08-26 实测:
    /// 粘两张图路径加一句中文,整段被 fish 逐行执行,图全丢了)。
    /// 红检:把可执行判定停用改成恒真,这条立刻报红。
    #[test]
    fn explicit_paths_must_be_executable_to_count_as_commands() {
        let temp = tempfile::tempdir().unwrap();
        let image = temp.path().join("1.png");
        std::fs::write(&image, b"not a program").unwrap();
        let script = temp.path().join("run.sh");
        std::fs::write(&script, b"#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let pasted = format!(
            "{}\n{}\n把这两张图按顺序上下拼接",
            image.display(),
            image.display()
        );
        assert!(
            !is_shell_command(&pasted, "fish"),
            "不可执行的路径不该被当成命令"
        );
        // 真能执行的显式路径仍然是命令。
        assert!(is_shell_command(
            &format!("{} --flag\nsecond line", script.display()),
            "fish"
        ));
        // 路径根本不存在时同样交给 Miyu。
        assert!(!is_shell_command(
            &format!("{}/nope --flag\nsecond line", temp.path().display()),
            "fish"
        ));
    }
}

#[cfg(any(test, feature = "testkit"))]
mod test_support;
#[cfg(any(test, feature = "testkit"))]
#[allow(unused_imports)]
pub use test_support::*;

#[cfg(test)]
mod stamp_tests {
    use super::*;

    fn paths_in(root: &Path) -> MiyuPaths {
        MiyuPaths {
            root_dir: root.to_path_buf(),
            config_dir: root.to_path_buf(),
            config_file: root.join("config.jsonc"),
            skills_dir: root.join("skills"),
            data_dir: root.join("data"),
            cache_dir: root.join("cache"),
            state_dir: root.join("state"),
            pictures_dir: root.join("pictures"),
            fish_hook_file: root.join("fish/conf.d/miyu.fish"),
            bash_hook_file: root.join("shell/bash-hook.sh"),
            zsh_hook_file: root.join("shell/zsh-hook.zsh"),
            scripts_dir: root.join("scripts"),
            system_scripts_dir: PathBuf::new(),
        }
    }

    /// 三个 hook 顶上都带 `# miyu shell hook · v<版本> · <指纹>`。
    ///
    /// 版本认发行版本、指纹认这一份脚本——所以版本三个一样、指纹三个不一样。
    #[test]
    fn every_hook_carries_a_version_and_a_content_fingerprint() {
        for hook in [fish::hook(), bash::hook(), zsh::hook()] {
            let (version, fingerprint) = installed_hook_stamp(&hook)
                .unwrap_or_else(|| panic!("hook 顶上没有标记:\n{}", &hook[..hook.len().min(200)]));
            assert_eq!(version, env!("CARGO_PKG_VERSION"));
            assert_eq!(fingerprint.len(), 8, "指纹长度变了: {fingerprint}");
            assert!(fingerprint.chars().all(|c| c.is_ascii_hexdigit()));
            // 第二行得告诉用户去哪儿重新生成,别让人手改这个文件。
            let second = hook.lines().nth(1).unwrap_or_default();
            assert!(second.contains("-init"), "第二行没写重装命令: {second}");
        }
        let fingerprints = [fish::hook(), bash::hook(), zsh::hook()]
            .map(|hook| installed_hook_stamp(&hook).unwrap().1.to_string());
        assert_ne!(fingerprints[0], fingerprints[1]);
        assert_ne!(fingerprints[1], fingerprints[2]);
        assert_ne!(fingerprints[0], fingerprints[2]);
    }

    /// 没有标记的（更老的版本装的）也认得出来是「不是当前这份」。
    #[test]
    fn text_without_a_stamp_has_no_version() {
        assert_eq!(installed_hook_stamp("complete -c miyu\n"), None);
        assert_eq!(installed_hook_stamp(""), None);
        // 前缀对了但少一半也不算。
        assert_eq!(installed_hook_stamp("# miyu shell hook · v0.6.0\n"), None);
    }

    /// 装着的对不上就换掉；**没装过的不碰**。
    #[test]
    fn stale_hooks_are_rewritten_and_missing_ones_are_left_alone() {
        let temp = tempfile::tempdir().unwrap();
        let paths = paths_in(temp.path());
        // 一个都没装:一个文件都不该冒出来。
        assert!(sync_installed_hooks(&paths).is_empty());
        assert!(!paths.fish_hook_file.exists());
        assert!(!paths.bash_hook_file.exists());
        assert!(!paths.zsh_hook_file.exists());

        // 只装 fish,而且是过期的那一份。
        fs::create_dir_all(paths.fish_hook_file.parent().unwrap()).unwrap();
        // 老版本装的那份没有标记,但有这一句——`looks_generated` 认的就是它。
        fs::write(
            &paths.fish_hook_file,
            "# 上一版装的\nprintf '%s' \"$buffer\" | miyu --shell-intercept\n",
        )
        .unwrap();
        assert_eq!(sync_installed_hooks(&paths), vec!["fish"]);
        assert_eq!(
            fs::read_to_string(&paths.fish_hook_file).unwrap(),
            fish::hook()
        );
        // bash / zsh 没装过,还是不该冒出来。
        assert!(!paths.bash_hook_file.exists());
        assert!(!paths.zsh_hook_file.exists());
        // 已经是当前这份了就别再写一遍。
        assert!(sync_installed_hooks(&paths).is_empty());
    }
}

#[cfg(test)]
mod hazard_tests {
    use super::*;

    /// 那个名字下放着**别人的**文件时，自动更新不许碰它。
    ///
    /// 路径是写死的，`fish-init` 是用户明确要求的（照旧覆盖），自动那条路不是。
    #[test]
    fn a_file_we_did_not_generate_is_never_overwritten() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("miyu.fish");
        let mine = "# 我自己写的\nalias m=miyu\n";
        fs::write(&path, mine).unwrap();
        assert!(sync_installed_hook("fish", &path, fish::hook).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), mine);

        // 反过来:老版本装的（没标记、有那句拦截）和被截断成空的，都该治。
        for stale in [
            "printf '%s' \"$buffer\" | miyu --shell-intercept\n",
            "",
            "   \n",
        ] {
            fs::write(&path, stale).unwrap();
            assert!(sync_installed_hook("fish", &path, fish::hook).unwrap());
            assert_eq!(fs::read_to_string(&path).unwrap(), fish::hook());
        }
    }

    /// 机器上还放着一个更新版本装的 hook 时，老二进制不许把它拽回去。
    #[test]
    fn an_older_build_does_not_downgrade_a_newer_hook() {
        let newer =
            format!("{HOOK_STAMP_PREFIX}999.0.0 · deadbeef\n# x\n\nmiyu --shell-intercept\n");
        assert!(installed_is_newer(&newer));
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("miyu.fish");
        fs::write(&path, &newer).unwrap();
        assert!(!sync_installed_hook("fish", &path, fish::hook).unwrap());
        assert_eq!(fs::read_to_string(&path).unwrap(), newer);

        // 更旧的、同版本的、读不出版本的,都照换。
        for older in ["0.0.1", env!("CARGO_PKG_VERSION"), "什么鬼"] {
            let text =
                format!("{HOOK_STAMP_PREFIX}{older} · deadbeef\n# x\n\nmiyu --shell-intercept\n");
            fs::write(&path, &text).unwrap();
            assert!(
                sync_installed_hook("fish", &path, fish::hook).unwrap(),
                "版本 {older} 该被换掉"
            );
        }
    }

    /// 语法闸：坏脚本过不了那个 shell 自己的 `-n`。
    ///
    /// 这是「发了一份坏 hook = 所有人开不了终端」那条路上的最后一道拦。
    #[test]
    fn the_syntax_gate_rejects_a_broken_script_and_accepts_the_real_one() {
        if std::process::Command::new("fish")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_err()
        {
            return; // 本机没装 fish,这条验不了
        }
        assert!(passes_syntax_check("fish", &fish::hook()));
        assert!(!passes_syntax_check("fish", "function 没收尾\n  echo\n"));
        // 没装的 shell:验不了就放行,不能因此把人卡住。
        assert!(passes_syntax_check(
            "miyu-no-such-shell-42",
            "function 没收尾\n  echo\n"
        ));
    }

    /// hook 文件被链进 dotfiles 仓库时，自动更新不能把**链接本身**换成普通文件。
    ///
    /// `fish-init` 走 `fs::write`，是写穿链接的；原子重写走 `rename`，换的是
    /// 目录项——链接就此断掉，dotfiles 里那份再也不生效，而且新文件还继承了
    /// 链接的 0777 权限。
    #[test]
    fn a_symlinked_hook_keeps_being_a_symlink() {
        let temp = tempfile::tempdir().unwrap();
        let real = temp.path().join("dotfiles/miyu.fish");
        fs::create_dir_all(real.parent().unwrap()).unwrap();
        fs::write(&real, "# 上一版装的\nmiyu --shell-intercept\n").unwrap();
        let link = temp.path().join("conf.d/miyu.fish");
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();

        assert!(sync_installed_hook("fish", &link, fish::hook).unwrap());
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink(),
            "链接被换成了普通文件"
        );
        assert_eq!(fs::read_to_string(&real).unwrap(), fish::hook());
    }

    /// `/proc` 拿不到时（macOS 上恒如此）退回 `$SHELL`；认不出的一律不猜。
    #[test]
    fn falls_back_to_the_shell_env_var() {
        use super::shell_from_env;
        use std::path::Path;

        assert_eq!(
            shell_from_env(Some(Path::new("/bin/zsh"))).as_deref(),
            Some("zsh")
        );
        assert_eq!(
            shell_from_env(Some(Path::new("/opt/homebrew/bin/fish"))).as_deref(),
            Some("fish")
        );
        assert_eq!(
            shell_from_env(Some(Path::new("/bin/bash"))).as_deref(),
            Some("bash")
        );
        // 认不出的别硬凑：宁可答「不知道」，也不要把 hook 装错 shell。
        assert_eq!(shell_from_env(Some(Path::new("/usr/bin/nu"))), None);
        assert_eq!(shell_from_env(Some(Path::new("/bin/"))), None);
        assert_eq!(shell_from_env(None), None);
    }
}
