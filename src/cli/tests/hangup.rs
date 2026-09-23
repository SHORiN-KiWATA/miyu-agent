//! 挂断看门狗盯的是哪个 fd。
//!
//! 09-10 报：shellhook（`printf '%s' "$buffer" | miyu --shell-intercept
//! --stdin`）里 ask_question 的面板一打开，什么都不做、几秒后整个回合被取消。
//! 根因是看门狗裸 poll stdin——管道写端退出后 stdin 常驻 POLLHUP，被当成
//! 「终端没了」，5 秒后 `exit(1)`，daemon 又把一次性客户端的断线当取消。

use crate::cli::hangup_watch_fd;
#[cfg(target_os = "linux")]
use crate::cli::{dead_terminal, fd_hung_up};

/// stdin 是终端时盯 stdin；stdin 被管道占用时盯控制终端。
#[test]
fn a_piped_stdin_moves_the_watch_to_the_controlling_terminal() {
    assert_eq!(
        hangup_watch_fd(true, Some(7)),
        Some(libc::STDIN_FILENO),
        "stdin 是终端:沿用原来的判据"
    );
    assert_eq!(
        hangup_watch_fd(false, Some(7)),
        Some(7),
        "stdin 是管道:管道 EOF 不是挂断,盯控制终端"
    );
    assert_eq!(
        hangup_watch_fd(false, None),
        None,
        "没有控制终端就永远不判挂断,宁可留着进程也不误杀"
    );
}

/// 记下真实形态：写端关掉的管道在 poll 里就是 POLLHUP。
/// 这是 Linux 内核的形态;macOS 的管道 EOF 只报可读、不置 POLLHUP。
#[cfg(target_os = "linux")]
#[test]
fn a_pipe_with_a_closed_writer_reports_hangup() {
    let mut fds = [0; 2];
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
    let (read_fd, write_fd) = (fds[0], fds[1]);
    assert!(!fd_hung_up(read_fd), "写端还开着:不算挂断");
    assert_eq!(unsafe { libc::close(write_fd) }, 0);
    assert!(
        fd_hung_up(read_fd),
        "写端一关就是 POLLHUP——所以不能拿它判终端"
    );
    assert_eq!(unsafe { libc::close(read_fd) }, 0);
}

/// 09-23：对端关掉的伪终端要认得出来。
///
/// 这时 `isatty` 返回假（tcgetattr 报 EIO），老判据把它当管道、改盯控制终端；进程
/// 的控制终端不是它的话就永远不退——一个 `miyu config` 这样全速空转了 8 小时。
#[cfg(target_os = "linux")]
#[test]
fn a_pty_whose_master_closed_is_a_dead_terminal() {
    let (mut master, mut slave) = (0, 0);
    let opened = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    assert_eq!(opened, 0);
    assert!(!dead_terminal(slave), "对端还开着：活终端");
    assert_eq!(unsafe { libc::close(master) }, 0);
    assert_eq!(
        unsafe { libc::isatty(slave) },
        0,
        "对端关了之后 isatty 返回假——老判据就是在这儿走岔的"
    );
    assert!(dead_terminal(slave), "字符设备 + 挂断 = 死终端");
    assert_eq!(unsafe { libc::close(slave) }, 0);

    // 写端关掉的管道也报挂断，但它不是终端。
    let mut fds = [0; 2];
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
    assert_eq!(unsafe { libc::close(fds[1]) }, 0);
    assert!(!dead_terminal(fds[0]), "管道不是终端");
    assert_eq!(unsafe { libc::close(fds[0]) }, 0);

    // /dev/null 是字符设备，但从不报挂断。
    let null = std::fs::File::open("/dev/null").unwrap();
    assert!(!dead_terminal(std::os::unix::io::AsRawFd::as_raw_fd(&null)));
}
