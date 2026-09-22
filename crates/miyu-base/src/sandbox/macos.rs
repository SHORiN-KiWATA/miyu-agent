//! macOS 的沙盒：exec 到 `/usr/bin/sandbox-exec`，规则用 SBPL 字符串传。
//!
//! # 为什么不照搬 Linux 那条路
//!
//! Linux 走的是「fork 之后、exec 之前，在子进程里给自己装 Landlock 规则」，所以
//! [`Rules::apply`] 的铁律是**不分配、不拿锁**——post-fork 的多线程进程里 malloc
//! 可能死锁。macOS 的对应物 `sandbox_init(3)` 要现场编译 SBPL 策略，**必然分配**，
//! 直接搬过去等于埋一个偶发死锁。
//!
//! 所以这里换一个动作：`apply` 里 `execv` 到 `sandbox-exec`。argv 在 [`Rules::prepare`]
//! 里就备好（那时还没 fork，随便分配），`apply` 只剩一个 `execv`——async-signal-safe、
//! 零分配，正好满足那条铁律。exec 成功之后原来那次 exec 就不会发生了；失败返回
//! errno，spawn 随之失败，和 Linux 一样是失败关闭。
//!
//! # 这一版比 Linux 弱一档，别当成等价
//!
//! Landlock 是白名单：没列出的路径**读写都拒**。macOS 这一版只收**写**：目录外
//! 写不进去，但读得到。
//!
//! 2026-09-22 在真 Mac（macOS 26.6）上逐条试过：
//!
//! - `(deny default)` 全拒白名单 —— 连 `/bin/sh` 都 exec 不起来，加
//!   `(import "system.sb")` 也不行；
//! - `(allow default)` + 禁写白名单 —— 可用，目录外写被拒、**孙进程也被挡住**、
//!   14.8 KB 的长策略照跑；
//! - `(allow default)` + 禁读白名单 —— **进程直接 abort**：`Abort trap: 6`
//!   （SIGABRT，退出码 134），无 stdout 无 stderr。09-23 又专门二分过一次：
//!   逐步加上 `/usr` `/bin` `/sbin` `/System` `/Library` `/private/var/db/dyld`
//!   `/dev` `/private/etc` 全套放行，仍然 134；连不经过 shell 的
//!   `/bin/echo` 也一样。**动态链接那一层在读被拒时直接 abort，不是缺某一条
//!   路径的事。**
//!
//! 所以读侧收紧在 `sandbox-exec -p` 这条路上**不可行**，不是「还没调出来」。
//! 要做只剩重写成 `(deny default)` 的完整白名单，而那一条上面已经证否。
//! **因此环境块里必须如实写明 macOS 上读没有收**，不能让模型以为和 Linux 一样。

use super::SandboxPolicy;
use std::ffi::{CString, OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

pub(super) struct Rules {
    /// argv 的实际存储：必须活到 `execv` 之后。
    _storage: Vec<CString>,
    /// 指向 `_storage` 的裸指针数组，NULL 结尾。fork 之后不能再分配，所以预先备好。
    argv: Vec<*const libc::c_char>,
    path: CString,
}

// SAFETY: 裸指针全部指向自己 `_storage` 里的堆缓冲；CString 移动时移动的是指针值，
// 缓冲本身不搬家，所以这些指针跨 move 依旧有效。构造之后不再改动任一方。
unsafe impl Send for Rules {}
unsafe impl Sync for Rules {}

impl Rules {
    pub(super) fn prepare(policy: &SandboxPolicy, program: &OsStr, args: &[OsString]) -> Self {
        let mut storage = Vec::with_capacity(args.len() + 5);
        for piece in [
            CString::new("sandbox-exec").ok(),
            CString::new("-p").ok(),
            CString::new(profile(policy)).ok(),
            CString::new("--").ok(),
            CString::new(program.as_bytes()).ok(),
        ] {
            if let Some(piece) = piece {
                storage.push(piece);
            }
        }
        for arg in args {
            if let Ok(value) = CString::new(arg.as_bytes()) {
                storage.push(value);
            }
        }
        let argv = storage
            .iter()
            .map(|item| item.as_ptr())
            .chain(std::iter::once(std::ptr::null()))
            .collect();
        Self {
            path: CString::new(SANDBOX_EXEC).unwrap_or_default(),
            _storage: storage,
            argv,
        }
    }

    /// 子进程里跑：换成 `sandbox-exec` 的镜像。成功就不返回。
    pub(super) fn apply(&self) -> std::io::Result<()> {
        // SAFETY: `execv` 是 async-signal-safe，不分配也不拿锁；argv 已经在 fork
        // 之前备好，这里只是把指针递进去。
        unsafe { libc::execv(self.path.as_ptr(), self.argv.as_ptr()) };
        Err(std::io::Error::last_os_error())
    }
}

/// 这台机器上沙盒能不能用：就看 `sandbox-exec` 在不在。
///
/// 漏了这一支的话 `probe()` 会走 `unsupported`、返回 `None`，于是**沙盒明明能用，
/// 启动日志却报「不可用，成员命令会被拒」**——比没有更糟，因为它会让人对着一个
/// 假故障查半天。返回值只用来表示「有」，所以给 1（Linux 那边返回的是 Landlock
/// 的 ABI 版本号，这里没有对应的东西）。
pub(super) fn probe() -> Option<i64> {
    std::path::Path::new(SANDBOX_EXEC).is_file().then_some(1)
}

/// SBPL 的字符串字面量里 `"` 和 `\` 要转义，否则一个带引号的路径就能把策略截断。
fn quote(path: &Path) -> String {
    let mut out = String::with_capacity(path.as_os_str().len() + 2);
    for ch in path.to_string_lossy().chars() {
        if ch == '"' || ch == '\\' {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// 路径要先解析成真身再写进策略。
///
/// `sandbox-exec` 的 `subpath` 拿**解析后**的路径匹配，而 macOS 上 `/var` 是指向
/// `/private/var` 的符号链接、`/tmp` 指向 `/private/tmp`——`tempfile::tempdir()`
/// 给的正是 `/var/folders/…`。照原样写进策略的话，规则永远匹配不上真实路径，
/// 结果是**盒内也写不进去**，看起来像沙盒太狠，其实是规则没命中（2026-09-23
/// 实测；同一个符号链接坑当天还在打包测试里踩过一次）。
///
/// 解析不了（路径还不存在）就退回原样：宁可规则宽一点，也不要把一条规则整个丢掉。
fn resolved(path: &Path) -> std::path::PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// 默认放行、禁写、再逐条放开可写目录。见模块头：这只收写，不收读。
fn profile(policy: &SandboxPolicy) -> String {
    let mut out = String::from("(version 1)\n(allow default)\n(deny file-write*)\n");
    let mut writable: Vec<&Path> = Vec::new();
    if !policy.root.as_os_str().is_empty() {
        writable.push(policy.root.as_path());
    }
    for path in &policy.read_write {
        writable.push(path.as_path());
    }
    for path in writable {
        out.push_str("(allow file-write* (subpath \"");
        out.push_str(&quote(&resolved(path)));
        out.push_str("\"))\n");
    }
    // 标准输出/错误、临时设备节点：不放行的话连报错都打不出来。
    out.push_str(
        "(allow file-write* (literal \"/dev/null\") (literal \"/dev/stdout\") \
         (literal \"/dev/stderr\") (literal \"/dev/tty\"))\n",
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn policy(root: &str, rw: &[&str]) -> SandboxPolicy {
        SandboxPolicy {
            root: PathBuf::from(root),
            read_write: rw.iter().map(PathBuf::from).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn the_profile_denies_writes_and_then_opens_the_allowed_roots() {
        let text = profile(&policy("/tmp/box", &["/tmp/box/work"]));
        assert!(text.contains("(deny file-write*)"), "{text}");
        assert!(text.contains("(subpath \"/tmp/box\")"), "{text}");
        assert!(text.contains("(subpath \"/tmp/box/work\")"), "{text}");
        // 顺序要紧：先 deny 再 allow，反过来写就全放开了。
        let deny = text.find("(deny file-write*)").expect("deny present");
        let allow = text
            .find("(allow file-write* (subpath")
            .expect("allow present");
        assert!(deny < allow, "deny 必须排在 allow 前面\n{text}");
    }

    #[test]
    fn a_path_with_a_quote_cannot_break_out_of_the_profile() {
        // 带引号的目录名不转义的话，一条规则就能把后面的策略整段截断。
        let text = profile(&policy("/tmp/a\"b", &[]));
        assert!(text.contains("/tmp/a\\\"b"), "{text}");
    }

    #[test]
    fn stdio_stays_writable_or_nothing_can_even_report_its_own_failure() {
        let text = profile(&policy("/tmp/box", &[]));
        assert!(text.contains("/dev/stderr"), "{text}");
    }

    #[test]
    fn the_argv_is_sandbox_exec_then_the_original_command() {
        let rules = Rules::prepare(
            &policy("/tmp/box", &[]),
            OsStr::new("/bin/echo"),
            &[OsString::from("hi")],
        );
        let argv: Vec<String> = rules
            ._storage
            .iter()
            .map(|item| item.to_string_lossy().into_owned())
            .collect();
        assert_eq!(argv[0], "sandbox-exec");
        assert_eq!(argv[1], "-p");
        assert!(argv[2].starts_with("(version 1)"), "{}", argv[2]);
        assert_eq!(argv[3], "--");
        assert_eq!(argv[4], "/bin/echo");
        assert_eq!(argv[5], "hi");
        // NULL 结尾，不然 execv 会读过头。
        assert!(rules.argv.last().expect("argv not empty").is_null());
    }
}
