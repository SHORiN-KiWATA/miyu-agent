//! 终端输出原语。
//!
//! 这里放的是"怎么把东西画到终端上"的底层能力，既不属于工具层也不属于渲染
//! 层——两边都要用：
//!
//! - `kitty` 的图形协议既服务于 `print_image` 这类工具，也服务于公式渲染；
//! - `chafa` 的调用约定同理——两条路此前各写一套参数，于是同一个版本兼容坑
//!   踩了两遍；
//! - `CommandOutputStream` 由命令执行产出、由渲染层消费。
//!
//! 放在基础层，两边都往下依赖，方向一致。
pub mod chafa;
/// 全屏 TUI 的共享版面（引导与设置界面同一张脸）。
pub mod chrome;
/// herdr 注入 pane 的环境变量：谁该带着坐标、谁得忘掉。
pub mod herdr;
pub mod kitty;
mod text;
pub use text::*;

/// 把输出端的换行翻译（`OPOST | ONLCR`）打开。
///
/// REPL 要 raw **输入**才能拿到按键，但渲染层的输出仍旧依赖 `\n` → `\r\n` 的
/// 翻译，而 `cfmakeraw` 把 OPOST 一并关了。不补回来的话每一行都比上一行右移
/// 一截，整块输出沿对角线滑出屏幕。
///
/// 两个地方要补：`enable_live_raw_mode` 进入 raw 时补一次；**chafa 这类会自己
/// 动 termios 的子进程跑完也要补一次**——它探测终端能力时会改控制终端的
/// termios，被 `kill_on_drop` 杀在中途就不会恢复现场，而 ONLCR 一旦丢了，往后
/// 整个 REPL 的输出都是错位的。
#[cfg(unix)]
pub fn restore_output_processing() -> anyhow::Result<()> {
    let mut attributes = std::mem::MaybeUninit::<libc::termios>::uninit();
    unsafe {
        if libc::tcgetattr(libc::STDOUT_FILENO, attributes.as_mut_ptr()) != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let mut attributes = attributes.assume_init();
        attributes.c_oflag |= libc::OPOST | libc::ONLCR;
        if libc::tcsetattr(libc::STDOUT_FILENO, libc::TCSANOW, &attributes) != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    Ok(())
}

#[cfg(not(unix))]
pub fn restore_output_processing() -> anyhow::Result<()> {
    Ok(())
}

/// 命令输出来自哪条流。
///
/// 渲染层据此决定颜色与前缀（stderr 要显眼），工具层据此把两条流分开收集。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandOutputStream {
    Stdout,
    Stderr,
}

/// 备用屏交接：引导结束后紧接着进全屏 REPL，备用屏不退不进，中间不闪、不清屏。
///
/// 引导退出时 `hold_alt_screen()`；全屏后端进屏时 `take_held_alt_screen()` 为真就
/// 跳过 `EnterAlternateScreen`（终端已经在备用屏上，再进一次会把上一帧清掉）；
/// 没人接手（REPL 启动失败）就 `release_alt_screen_if_held()` 退回主屏。
static ALT_SCREEN_HELD: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn hold_alt_screen() {
    ALT_SCREEN_HELD.store(true, std::sync::atomic::Ordering::Relaxed);
}

pub fn take_held_alt_screen() -> bool {
    ALT_SCREEN_HELD.swap(false, std::sync::atomic::Ordering::Relaxed)
}

pub fn release_alt_screen_if_held() {
    if take_held_alt_screen() {
        let _ = crossterm::execute!(
            std::io::stdout(),
            crossterm::cursor::Show,
            crossterm::terminal::LeaveAlternateScreen
        );
    }
}

/// 转轮帧周期(40ms):渲染层驱动动画,Agent 的 SpinnerTick 也按它发。住在
/// terminal 是因为两边都要,而 agent 不能反向 use render(09-16)。
pub const SPINNER_INTERVAL: std::time::Duration = std::time::Duration::from_millis(40);

static CONTENT_VIEWPORT: std::sync::OnceLock<std::sync::Mutex<Option<(u16, u16)>>> =
    std::sync::OnceLock::new();

pub fn set_content_viewport(viewport: Option<(u16, u16)>) {
    *CONTENT_VIEWPORT
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .unwrap() = viewport;
}

pub fn content_viewport() -> Option<(u16, u16)> {
    CONTENT_VIEWPORT
        .get()
        .and_then(|value| value.lock().ok().and_then(|guard| *guard))
}

#[cfg(test)]
mod viewport_tests {
    use super::{content_viewport, set_content_viewport};

    #[test]
    fn viewport_can_be_cleared_when_fullscreen_ends() {
        set_content_viewport(Some((120, 30)));
        assert_eq!(content_viewport(), Some((120, 30)));
        set_content_viewport(None);
        assert_eq!(content_viewport(), None);
    }
}

/// 色板与色深降级（引导、空会话 banner 共用）。
pub mod palette;
/// 星空、渐变艺术字、扫光。
pub mod starfield;
