//! 终端状态的兜底还原，以及「对端已经关掉了」的挂断守卫。从 `src/cli/mod.rs` 搬来（09-16 拆分），逻辑未改。

use crate::cli::*;

pub(super) struct ReplCursorRestore;

impl Drop for ReplCursorRestore {
    fn drop(&mut self) {
        // 1. 会话级兜底：恢复括号粘贴与光标
        // 2. 再关闭 raw mode；键盘增强由 LiveRawMode / 局部输入作用域负责 Pop
        let _ = execute!(
            io::stdout(),
            DisableBracketedPaste,
            DisableFocusChange,
            Show
        );
        let _ = terminal::disable_raw_mode();
    }
}

/// Raw input is required for key events, but renderer output still relies on
/// newline translation. 实现挪到了 `terminal::restore_output_processing`：
/// chafa 跑完也要补一次，那边是两个调用方的公共位置。
pub(super) fn restore_live_output_processing() -> Result<()> {
    miyu_base::terminal::restore_output_processing()
}

/// 终端已死(PTY 对端关闭):POLLHUP/POLLERR/POLLNVAL 任一命中。
/// 不发 SIGHUP 的断开路径(tmux kill-pane、终端崩溃、SSH 掉线)只能靠它
/// 兜底——否则 crossterm 的 poll 对 EOF fd 永远立即就绪、read 又读不出
/// 事件,REPL 主循环全速空转,留下一个 98% CPU 的残留进程。
/// 挂断看门狗:独立线程每 500ms 裸 poll 探测 stdin 挂断,确认后给优雅
/// 退出路径 5 秒宽限——主线程若卡死在 crossterm 对 HUP fd 的任何内部
/// 自旋(事件 poll、CPR 应答等待,均为实测形态),由这里强制收尾,
/// 保证关终端后绝不留下吃 CPU 的残留进程。
/// REPL 是不是正跑在全屏（备用屏）里。
///
/// 提问面板、选择器这类"自己占一块屏"的组件要据此改行为：备用屏没有
/// scrollback，靠打换行腾地方会把正文顶没。
pub(crate) fn in_fullscreen() -> bool {
    repl::tail::screen::in_fullscreen()
}

/// 全屏下正文区的尺寸（列, 行）。别的地方拿它替代 `terminal::size()`。
pub(crate) fn content_viewport() -> Option<(u16, u16)> {
    repl::tail::screen::content_viewport()
}

pub(crate) fn spawn_hangup_watchdog() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        std::thread::spawn(|| loop {
            std::thread::sleep(Duration::from_millis(500));
            if terminal_hangup() {
                std::thread::sleep(Duration::from_secs(5));
                if terminal_hangup() {
                    exit_after_terminal_gone(1);
                }
            }
        });
    });
}

pub(super) fn terminal_hangup() -> bool {
    let stdin_is_tty = unsafe { libc::isatty(libc::STDIN_FILENO) } == 1;
    // 对端已经关掉的伪终端：tcgetattr 报 EIO，`isatty` 因此返回假——下面会把它当
    // 管道、改盯控制终端。进程的控制终端要不是这一个（测具起进程没设、或别的会话
    // 起的），就永远判不出挂断，crossterm 对死 fd 全速自旋（09-23 实测一个
    // `miyu config` 这样空转了 8 小时）。它不是管道，是一个死掉的终端。
    if !stdin_is_tty && dead_terminal(libc::STDIN_FILENO) {
        return true;
    }
    match hangup_watch_fd(stdin_is_tty, controlling_tty_fd()) {
        Some(fd) => fd_hung_up(fd),
        None => false,
    }
}

/// 盯哪个 fd 判挂断:stdin 是终端就盯 stdin;stdin 被管道/重定向占用时盯
/// **控制终端**——管道读到 EOF 是正常收尾,不是挂断。
///
/// shellhook 的 `printf '%s' "$buffer" | miyu --shell-intercept --stdin` 就是
/// 这个形态:写端 printf 一退出,stdin 立刻常驻 POLLHUP。原先一律裸 poll
/// stdin,于是问题面板一打开(它是 `spawn_hangup_watchdog` 的第一个调用点)
/// 就按下 5 秒倒计时,到点 `exit(1)`,daemon 看到一次性客户端断线又把回合
/// 取消——用户看到的是「面板开着没动,几秒后自己没了」(09-10 报)。
///
/// 拿不到控制终端(纯后台、cron)时返回 None:宁可不判挂断,也不误杀。
pub(super) fn hangup_watch_fd(
    stdin_is_tty: bool,
    controlling_tty: Option<libc::c_int>,
) -> Option<libc::c_int> {
    if stdin_is_tty {
        return Some(libc::STDIN_FILENO);
    }
    controlling_tty
}

/// 控制终端 fd,进程内只开一次。它随进程存活,不关——看门狗每 500ms 用一次。
fn controlling_tty_fd() -> Option<libc::c_int> {
    use std::os::unix::io::IntoRawFd;
    static FD: std::sync::OnceLock<Option<libc::c_int>> = std::sync::OnceLock::new();
    *FD.get_or_init(|| {
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/tty")
            .ok()
            .map(IntoRawFd::into_raw_fd)
    })
}

/// 这个 fd 是字符设备、而且报挂断：一个对端已经没了的终端。
///
/// 管道（写端关了也报 POLLHUP）不是字符设备；`/dev/null` 是字符设备但从不报挂断。
pub(super) fn dead_terminal(fd: libc::c_int) -> bool {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: fstat 只往我们给的那块内存里写；返回 0 才读它。
    if unsafe { libc::fstat(fd, stat.as_mut_ptr()) } != 0 {
        return false;
    }
    let mode = unsafe { stat.assume_init() }.st_mode;
    (mode & libc::S_IFMT) == libc::S_IFCHR && fd_hung_up(fd)
}

pub(super) fn fd_hung_up(fd: libc::c_int) -> bool {
    let mut pollfd = libc::pollfd {
        fd,
        events: 0,
        revents: 0,
    };
    let ready = unsafe { libc::poll(&mut pollfd, 1, 0) };
    ready == 1 && (pollfd.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL)) != 0
}

/// 终端没了（挂断）或者收到终止信号：先把 herdr 那个 pane 的权威还回去，再退。
///
/// `process::exit` 绕过所有 Drop——回合守卫收尾时报 idle / release 的那条路走
/// 不到，侧栏上就一直挂着一个已经不在了的 miyu（09-20 记下、09-23 补上）。
/// 不在 herdr 里时 `release_blocking` 什么都不做。
pub(crate) fn exit_after_terminal_gone(code: i32) -> ! {
    crate::cli::repl::herdr::release_blocking();
    std::process::exit(code)
}

/// 收到 SIGHUP / SIGTERM 就收尾退出。常驻 REPL 起来时调一次。
///
/// 原来这是 `tokio::spawn` 出去的一个异步任务——而运行时是 `current_thread`，
/// REPL 空闲时同步阻塞在读键盘上，任务根本轮不到：一轮都没跑过时信号还没注册，
/// 进程被默认动作杀掉，退出码 -15（09-20 记下）；跑过一轮之后信号注册上了却
/// 没人处理，SIGTERM 被一直憋着，进程杀不掉（09-23 真 herdr 实测）。
/// 改成当场同步注册、另起一根线程等：两件事都不再依赖主循环让不让出。
pub(crate) fn exit_on_termination_signals() {
    use signal_hook::consts::{SIGHUP, SIGTERM};
    let Ok(mut signals) = signal_hook::iterator::Signals::new([SIGHUP, SIGTERM]) else {
        return;
    };
    let _ = std::thread::Builder::new()
        .name("miyu-signals".to_string())
        .spawn(move || {
            if signals.forever().next().is_none() {
                return;
            }
            // 后台任务归 daemon 管：前端死了任务照跑，完成后有唤醒（dsh 语义）。
            // SIGTERM 时终端往往还活着：先尽力恢复 raw mode，否则用户的 shell
            // 停在原始模式里。
            let _ = crossterm::terminal::disable_raw_mode();
            exit_after_terminal_gone(0);
        });
}
