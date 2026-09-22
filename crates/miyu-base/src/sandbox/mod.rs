//! 子进程沙盒(09-11 成员,09-13 起管理员会话也可绑;Landlock)。
//!
//! 照搬 dsh 的 `landlock-run` 思路:fork 之后、exec 之前给**子进程自己**装一套
//! Landlock 规则集(允许列表),规则随 `execve` 继承,命令和它再起的一切子进程
//! 都受限,daemon 本身不受影响。没有任何依赖——三个裸 syscall 加一次 `prctl`,
//! 内核 5.13+ 自带。
//!
//! 策略由调用方给(回合层按「会话归谁、绑没绑沙盒」算,见 `web::sandbox_scope`):
//! 成员的回合与工具桥里,run_command、后台 job、脚本工具起的进程只能写自己家里
//! 的工作区、`/tmp` 与脚本缓存,其余只读;管理员默认不套,`/sandbox <路径>` 绑定
//! 后同样读写都锁在那个根下。内核不支持(没编 Landlock / 被禁)就**失败关闭**:
//! 沙盒回合的命令一个都不跑,而不是裸奔。
//!
//! 只管文件系统。网络(ABI 4 的 TCP bind/connect)不在 handled 集合里,不受限。
//! Linux 使用 Landlock。macOS 等尚无后端的平台在存在策略时拒绝创建子进程,
//! 不执行任何 Linux syscall。没有策略的命令保持正常执行。

use std::path::PathBuf;
use std::sync::Arc;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(any(not(target_os = "linux"), test))]
mod unsupported;

mod backend;
use backend::Rules;

/// Available filesystem sandbox ABI. None means no supported kernel backend.
pub fn probe() -> Option<i64> {
    #[cfg(target_os = "linux")]
    {
        linux::probe()
    }
    #[cfg(target_os = "macos")]
    {
        macos::probe()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        unsupported::probe()
    }
}

#[derive(Debug, Clone, Default)]
pub struct SandboxPolicy {
    /// 沙盒根:成员是 `home/<用户>/workspace`,管理员是 `/sandbox` 绑的目录。
    /// 进环境块告诉模型自己关在哪;`/sandbox` 查看也用它。
    pub root: PathBuf,
    /// 只读 + 可执行(目录下的一切)。
    pub read_only: Vec<PathBuf>,
    /// 内核能管的全部文件系统权限。
    pub read_write: Vec<PathBuf>,
    /// 子进程的 HOME(沙盒根):登录 shell 读 ~/.profile、程序写 ~/.cache
    /// 都落在这里,而不是撞在真家门口的 Permission denied 上。
    pub home: Option<PathBuf>,
    /// 额外环境变量(工具链直通:CARGO_HOME 这类指回真家里放行了的目录)。
    pub env: Vec<(String, String)>,
    /// 插到子进程 PATH 头部的目录(`~/.cargo/bin` 这类,存在且放行了才进)。
    pub path_prepend: Vec<PathBuf>,
    /// 给模型看的可写/可读摘要(环境块与 `/sandbox` 查看共用),路径以 `~` 缩写;
    /// 系统目录、Miyu 内部目录不逐条列,只写 `system dirs`。
    pub writable_summary: Vec<String>,
    pub readable_summary: Vec<String>,
}

/// 进程内工具(read/edit/glob/grep/print_image/看图……)读路径前过一遍:
/// 有沙盒策略时,路径必须落在只读或可写根之下;没有策略原样放行。
pub fn guard_read(path: &std::path::Path) -> anyhow::Result<()> {
    guard(path, false)
}

/// 同上,写路径:只认可写根。
pub fn guard_write(path: &std::path::Path) -> anyhow::Result<()> {
    guard(path, true)
}

fn guard(path: &std::path::Path, write: bool) -> anyhow::Result<()> {
    let Some(policy) = current_sandbox() else {
        return Ok(());
    };
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        crate::workspace::effective_workdir().join(path)
    };
    let resolved = resolve_existing_prefix(&absolute);
    let allowed = policy
        .read_write
        .iter()
        .chain(if write {
            [].iter()
        } else {
            policy.read_only.iter()
        })
        .any(|root| {
            let root = root.canonicalize().unwrap_or_else(|_| root.clone());
            resolved.starts_with(&root)
        });
    if allowed {
        Ok(())
    } else {
        anyhow::bail!(
            "sandbox: {} is outside your workspace ({} not allowed there)",
            path.display(),
            if write { "writing" } else { "reading" }
        )
    }
}

/// 把路径里已存在的最长前缀 canonicalize(跟符号链接走),剩下的原样接回去——
/// 还不存在的文件也能判在哪个根下,`..` 与软链绕不出去。
fn resolve_existing_prefix(path: &std::path::Path) -> PathBuf {
    let mut existing = path.to_path_buf();
    let mut rest = Vec::new();
    while !existing.exists() {
        match (existing.file_name(), existing.parent()) {
            (Some(name), Some(parent)) => {
                rest.push(name.to_os_string());
                existing = parent.to_path_buf();
            }
            _ => break,
        }
    }
    let mut resolved = existing.canonicalize().unwrap_or(existing);
    for name in rest.into_iter().rev() {
        if name == ".." {
            resolved.pop();
        } else if name != "." {
            resolved.push(name);
        }
    }
    resolved
}

tokio::task_local! {
    static SANDBOX: Option<Arc<SandboxPolicy>>;
}

/// 在这个 future 里起的子进程(经 [`confine`] / [`confine_std`])都套这套策略;
/// `None` = 不套(管理员、终端、平台回合的老路)。
pub async fn with_sandbox<F: std::future::Future>(
    policy: Option<Arc<SandboxPolicy>>,
    future: F,
) -> F::Output {
    SANDBOX.scope(policy, future).await
}

pub fn current_sandbox() -> Option<Arc<SandboxPolicy>> {
    SANDBOX.try_with(|policy| policy.clone()).ok().flatten()
}

/// 子进程环境:HOME 换成沙盒根(`keep_home` 时不换——中转线 CLI 得按真家找
/// `~/.claude`)、工具链变量、PATH 头部补放行了的 bin 目录。
fn child_env(policy: &SandboxPolicy, keep_home: bool) -> Vec<(String, std::ffi::OsString)> {
    let mut env: Vec<(String, std::ffi::OsString)> = Vec::new();
    if !keep_home {
        if let Some(home) = &policy.home {
            env.push(("HOME".to_string(), home.clone().into_os_string()));
        }
    }
    for (key, value) in &policy.env {
        env.push((key.clone(), value.clone().into()));
    }
    if !policy.path_prepend.is_empty() {
        let mut parts: Vec<PathBuf> = policy.path_prepend.clone();
        if let Some(existing) = std::env::var_os("PATH") {
            parts.extend(std::env::split_paths(&existing));
        }
        if let Ok(joined) = std::env::join_paths(parts) {
            env.push(("PATH".to_string(), joined));
        }
    }
    env
}

/// 有策略在身就给 Command 挂 `pre_exec`(在子进程里装规则再 exec);没有就原样。
/// 把命令本身抄出来（macOS 那一支要把它接在 `sandbox-exec` 后面）。
///
/// Linux 用不到，但两边共用一个 `prepare` 签名比按平台分叉两套调用点干净。
/// 读是安全的：`std::process::Command` 的 program/args 都可读，stdio 与 env
/// 留在原 `Command` 上，跨 `execv` 照常继承。
fn program_and_args(
    command: &tokio::process::Command,
) -> (std::ffi::OsString, Vec<std::ffi::OsString>) {
    let std_command = command.as_std();
    (
        std_command.get_program().to_os_string(),
        std_command
            .get_args()
            .map(|arg| arg.to_os_string())
            .collect(),
    )
}

pub fn confine(command: &mut tokio::process::Command) {
    if let Some(policy) = current_sandbox() {
        for (key, value) in child_env(&policy, false) {
            command.env(key, value);
        }
        let rules = Rules::prepare(&policy, &program_and_args(command));
        // SAFETY: 闭包只做裸 syscall / open / close,不碰锁、不分配。
        // macOS 那一支是 execv,同样 async-signal-safe(见 `macos::Rules::apply`)。
        unsafe {
            command.pre_exec(move || rules.apply());
        }
    }
}

/// 中转线的 CLI 进程(claude / codex / agy)本身关进沙盒(09-11 用户拍板:不是关掉
/// 它们自带的工具,而是让整个进程跑在沙盒里,它起的 Bash/Edit 子进程照样继承规则)。
/// 在成员策略之上再放行 CLI 自己的配置目录(登录态、会话文件都在里面),HOME 不换
/// ——CLI 得按真 HOME 找 ~/.claude。没有策略(管理员)原样。
pub fn confine_relay(command: &mut tokio::process::Command, extra_rw: &[PathBuf]) {
    if let Some(policy) = current_sandbox() {
        let mut extended = (*policy).clone();
        extended.home = None;
        for path in extra_rw {
            if path.exists() && !extended.read_write.iter().any(|p| p == path) {
                extended.read_write.push(path.clone());
            }
        }
        for (key, value) in child_env(&extended, true) {
            command.env(key, value);
        }
        let rules = Rules::prepare(&extended, &program_and_args(command));
        // SAFETY: 同 confine。
        unsafe {
            command.pre_exec(move || rules.apply());
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(any(test, feature = "testkit"))]
mod test_support;
#[cfg(any(test, feature = "testkit"))]
#[allow(unused_imports)]
pub use test_support::*;
