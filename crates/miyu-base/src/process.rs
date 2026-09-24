//! 进程级的小工具(09-16 从 runtime 下沉:配置目录缓存这种底层模块也要用,不能反向认识 runtime)。

use std::sync::atomic::{AtomicBool, Ordering};

static DAEMON_SHUTTING_DOWN: AtomicBool = AtomicBool::new(false);

/// daemon 开始有序关停（重启、换二进制、systemd stop）。只有 daemon 的主流程调。
///
/// 关停要让正在跑的回合停下，但不能按「用户取消」收尾：那样库里它就是一条普通的
/// 「已中断」，下一个 daemon 分不出是人停的还是重启打断的，也就没法接着跑（09-24
/// 断点续跑）。回合的收尾守卫看到这个标记，只记用量、保留「执行中」。
pub fn begin_daemon_shutdown() {
    DAEMON_SHUTTING_DOWN.store(true, Ordering::SeqCst);
}

pub fn daemon_shutting_down() -> bool {
    DAEMON_SHUTTING_DOWN.load(Ordering::SeqCst)
}

/// 把 glibc arena 里已释放的整段内存还给操作系统。大块临时解析（models.dev
/// 目录、整回合请求体）释放后 glibc 默认把页留在 arena 里，RSS 不降。
/// 非 glibc 分配器下是无害空转。
#[cfg(target_os = "linux")]
pub fn trim_process_memory() {
    unsafe {
        libc::malloc_trim(0);
    }
}

/// 非 Linux 上没有 `malloc_trim`（那是 glibc 的扩展），空转即可。
///
/// 可见性必须和上面那支一致：`miyu-hosts` 的 `runtime/mod.rs` 把它再导出一层，
/// 写成 `pub(crate)` 的话在 macOS 上就是 E0603——而 Linux 侧编得过，所以这个
/// 错一直藏到 2026-09-22 加上 macOS CI 的第一次运行才现形。
#[cfg(not(target_os = "linux"))]
pub fn trim_process_memory() {}

/// 外部程序不在 `PATH` 上时，把 `os error 2` 换成一句能照着做的话。
///
/// 09-22 在 macOS 上实测：那台机器没有 ripgrep，模型调 `grep` / `glob` 收到的
/// 整句话就是 `No such file or directory (os error 2)`——连缺的是哪个程序都没说。
/// 模型拿到这种错只会去猜路径、换参数，白烧几轮。Linux 上看不见是因为 Arch 包
/// 把 `ripgrep` 写进了依赖，装 Miyu 就一起装上了。
///
/// 只认 `NotFound`：别的 io 错误（权限、管道断）原样往上抛，不要拿安装提示去盖
/// 一个不相干的故障。
pub fn missing_program(program: &str, error: &std::io::Error) -> Option<String> {
    if error.kind() != std::io::ErrorKind::NotFound {
        return None;
    }
    let install = match program {
        "rg" => Some(
            "brew install ripgrep (macOS), apt install ripgrep (Debian/Ubuntu), \
             dnf install ripgrep (Fedora), pacman -S ripgrep (Arch)",
        ),
        "chafa" => Some(
            "brew install chafa (macOS), apt install chafa (Debian/Ubuntu), \
             dnf install chafa (Fedora), pacman -S chafa (Arch)",
        ),
        _ => None,
    };
    Some(match install {
        Some(install) => format!("`{program}` is not on PATH. Install it: {install}."),
        None => format!("`{program}` is not on PATH."),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_program_is_named_with_a_way_to_install_it() {
        let error = std::io::Error::from(std::io::ErrorKind::NotFound);
        let hint = missing_program("rg", &error).expect("NotFound should produce a hint");
        assert!(hint.contains("`rg` is not on PATH"), "{hint}");
        assert!(hint.contains("brew install ripgrep"), "{hint}");
    }

    #[test]
    fn other_io_failures_keep_their_own_error() {
        // 权限被拒不是「没装」,拿安装提示去盖它会把人带偏。
        let error = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        assert!(missing_program("rg", &error).is_none());
    }

    #[test]
    fn an_unknown_program_still_says_which_one_is_missing() {
        let error = std::io::Error::from(std::io::ErrorKind::NotFound);
        let hint = missing_program("totally-unknown", &error).expect("still a hint");
        assert_eq!(hint, "`totally-unknown` is not on PATH.");
    }
}
