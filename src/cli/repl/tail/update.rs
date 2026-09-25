//! Synchronized terminal updates, including nested redraws.
//!
//! DEC mode 2026 is a boolean, not a stack. Only the outer update may end it.
//! A hidden cursor still exposes its position to multiplexers and cursor trails,
//! so fullscreen redraws must keep the whole frame inside this boundary.
//!
//! In fullscreen the outer update also collects the frame in memory and hands it
//! to the terminal in a single write (see [`TermOut`]).

use crate::cli::repl::layout::CursorAfterUpdate;
use anyhow::Result;
use crossterm::cursor::{Hide, Show};
use crossterm::terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate};
use crossterm::{execute, queue, Command};
use std::cell::{Cell, RefCell};
use std::io::{self, Write};

thread_local! {
    static UPDATE_DEPTH: Cell<usize> = const { Cell::new(0) };
    static BLOCK_STARTED: Cell<Option<std::time::Instant>> = const { Cell::new(None) };
    /// 全屏下最外层同步块正在攒的这一帧。`None` = 没在攒（块外，或 inline）。
    static FRAME: RefCell<Option<Vec<u8>>> = const { RefCell::new(None) };
}

/// 一帧的初始容量。大厅一帧十几 KB（09-23 实测），先留够免得边攒边扩。
const FRAME_CAPACITY: usize = 16 * 1024;

thread_local! {
    /// 攒帧（09-25）：切会话那一阵分好几步画（回合收尾、清屏回放、footer、挂上正在跑的
    /// 那一轮），每一步原来各出一帧，终端上先是空屏、再一截截长出来（用户：「切换会话一点
    /// 都不顺畅」）。开着的时候最外层同步块收尾不写出去，续在这里，放行时整段作一帧。
    /// 只在全屏下开；REPL 跑在单线程运行时上，跨 await 也还在同一个线程。
    static HOLD: RefCell<Option<Held>> = const { RefCell::new(None) };
}

struct Held {
    bytes: Vec<u8>,
    since: std::time::Instant,
}

/// 挂上一轮之后，多久没有新帧就算补发的那一阵画完了：攒帧在这时放行。
pub(in crate::cli) const CATCH_UP_QUIET: std::time::Duration = std::time::Duration::from_millis(40);

/// 攒帧最多攒多久。过了就连同手上这一帧一起放出去，免得哪一步出了岔子画面一直不动。
const HOLD_LIMIT: std::time::Duration = std::time::Duration::from_millis(500);
const SYNC_BEGIN: &[u8] = b"\x1b[?2026h";
const SYNC_END: &[u8] = b"\x1b[?2026l";

/// 开始攒帧（已经在攒就不动）。
pub(in crate::cli) fn begin_frame_hold() {
    if !crate::cli::repl::tail::screen::in_fullscreen() {
        return;
    }
    HOLD.with(|hold| {
        hold.borrow_mut().get_or_insert_with(|| Held {
            bytes: Vec::new(),
            since: std::time::Instant::now(),
        });
    });
}

pub(in crate::cli) fn frame_hold_active() -> bool {
    HOLD.with(|hold| hold.borrow().is_some())
}

/// 放行：攒着的几步画面整段作一帧写出去。
pub(in crate::cli) fn release_frame_hold() -> Result<()> {
    let Some(held) = HOLD.with(|hold| hold.borrow_mut().take()) else {
        return Ok(());
    };
    if held.bytes.is_empty() {
        return Ok(());
    }
    let mut frame = Vec::with_capacity(held.bytes.len() + SYNC_BEGIN.len() + SYNC_END.len());
    frame.extend_from_slice(SYNC_BEGIN);
    frame.extend_from_slice(&held.bytes);
    frame.extend_from_slice(SYNC_END);
    let mut door = FrameDoor(io::stdout().lock());
    door.write_all(&frame)?;
    door.flush()?;
    Ok(())
}

/// 最外层同步块攒好的一帧：攒帧开着就续进去（去掉它自己的开始/结束标记），返回是否已经
/// 接手。攒过了时限就连这一帧一起放行。
fn hold_frame(frame: &[u8]) -> io::Result<bool> {
    let expired = HOLD.with(|hold| {
        let mut hold = hold.borrow_mut();
        let Some(held) = hold.as_mut() else {
            return None;
        };
        append_without_sync_marks(&mut held.bytes, frame);
        Some(held.since.elapsed() >= HOLD_LIMIT)
    });
    match expired {
        None => Ok(false),
        Some(false) => Ok(true),
        Some(true) => release_frame_hold()
            .map(|()| true)
            .map_err(|error| io::Error::other(error.to_string())),
    }
}

fn append_without_sync_marks(out: &mut Vec<u8>, frame: &[u8]) {
    let mut rest = frame;
    while !rest.is_empty() {
        let next = [SYNC_BEGIN, SYNC_END]
            .iter()
            .filter_map(|mark| {
                rest.windows(mark.len())
                    .position(|window| window == *mark)
                    .map(|at| (at, mark.len()))
            })
            .min();
        match next {
            Some((at, len)) => {
                out.extend_from_slice(&rest[..at]);
                rest = &rest[at + len..];
            }
            None => {
                out.extend_from_slice(rest);
                break;
            }
        }
    }
}

/// `MIYU_SYNC_TRACE=1`：每个最外层同步块的时长（微秒）与这一帧的字节数追加到
/// `/tmp/miyu-sync-trace.log`。kitty 在块开着的时间里按活光标给输入法定位，块越
/// 短越不容易撞上（09-17），这把尺子量的就是那个窗口。
static SYNC_TRACE: std::sync::LazyLock<bool> =
    std::sync::LazyLock::new(|| std::env::var_os("MIYU_SYNC_TRACE").is_some());

fn trace_block(elapsed: std::time::Duration, bytes: usize) {
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("/tmp/miyu-sync-trace.log")
    {
        let _ = writeln!(file, "{} {bytes}", elapsed.as_micros());
    }
}

/// 画面的写出口：全屏同步块里写进帧缓冲，其余时候直通 stdout。
///
/// 一帧原来经 std 的 1KB 行缓冲和中途好几次 flush 拆成 7–23 次 write（09-23
/// strace 实测）。kitty 收到首字节 3ms 后解析**已经到了的**字节，帧拖过这个窗口
/// 就只解析到半帧——这时来的输入法预编辑更新按活光标定位，而活光标正停在别处
/// （大厅的星星格子上），输入法框和光标先跳过去、帧尾再跳回输入框（用户 09-23：
/// 「光标一直从输入框外跳到输入框」）。整帧一次写出去，终端就解析不到半帧。
///
/// inline 不攒：那边重画要问终端光标在哪（`ESC[6n`），前面的字节必须先到终端。
pub(in crate::cli) struct TermOut;

pub(in crate::cli) fn term_out() -> TermOut {
    TermOut
}

impl Write for TermOut {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let collected = FRAME.with(|frame| match frame.borrow_mut().as_mut() {
            Some(buffer) => {
                buffer.extend_from_slice(bytes);
                true
            }
            None => false,
        });
        if collected {
            return Ok(bytes.len());
        }
        io::stdout().write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        // 攒着的时候什么都不做：一帧的字节只在最外层收尾时一次出门。
        if collecting() {
            return Ok(());
        }
        io::stdout().flush()
    }
}

fn collecting() -> bool {
    FRAME.with(|frame| frame.borrow().is_some())
}

pub(in crate::cli) fn synchronized_terminal_update<T>(
    cursor_after: CursorAfterUpdate,
    update: impl FnOnce() -> Result<T>,
) -> Result<T> {
    // Stdout's lock is reentrant: drawing and nested updates on this thread
    // can still write, while other threads cannot interleave with the frame.
    let stdout = io::stdout().lock();
    if crate::cli::repl::tail::screen::in_fullscreen() {
        return update_to(FrameDoor(stdout), true, cursor_after, update);
    }
    update_to(stdout, false, cursor_after, update)
}

/// 攒好的一帧从这儿出门：先把 std 行缓冲里更早的字节冲掉（保住先后），再整帧
/// 一次写给 fd 1——不经 std 的行缓冲，它碰到换行会把一帧拆成两次写。
struct FrameDoor<'a>(io::StdoutLock<'a>);

impl Write for FrameDoor<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.flush()?;
        loop {
            // SAFETY: fd 1 在进程生命期内一直开着；缓冲区指针与长度来自同一个切片。
            let written =
                unsafe { libc::write(libc::STDOUT_FILENO, bytes.as_ptr().cast(), bytes.len()) };
            if written >= 0 {
                return Ok(written as usize);
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

fn update_to<T>(
    writer: impl Write,
    collect: bool,
    cursor_after: CursorAfterUpdate,
    update: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let guard = UpdateGuard::begin(writer, collect, cursor_after)?;
    let result = update();
    let end = guard.finish();
    match result {
        Ok(value) => {
            end?;
            Ok(value)
        }
        Err(error) => Err(error),
    }
}

struct UpdateGuard<W: Write> {
    writer: W,
    cursor_after: CursorAfterUpdate,
    outermost: bool,
    /// 这一层开了帧缓冲：只有最外层、且在全屏下。收尾时由它把整帧写出去。
    collecting: bool,
    active: bool,
}

impl<W: Write> UpdateGuard<W> {
    fn begin(writer: W, collect: bool, cursor_after: CursorAfterUpdate) -> io::Result<Self> {
        let outermost = UPDATE_DEPTH.get() == 0;
        let collecting = outermost && collect;
        if outermost && *SYNC_TRACE {
            BLOCK_STARTED.set(Some(std::time::Instant::now()));
        }
        if collecting {
            FRAME.with(|frame| *frame.borrow_mut() = Some(Vec::with_capacity(FRAME_CAPACITY)));
        }
        let mut guard = Self {
            writer,
            cursor_after,
            outermost,
            collecting,
            active: false,
        };
        if matches!(
            cursor_after,
            CursorAfterUpdate::Shown | CursorAfterUpdate::Hidden
        ) {
            guard.emit(Hide)?;
        }
        if outermost {
            guard.emit(BeginSynchronizedUpdate)?;
        }
        UPDATE_DEPTH.set(UPDATE_DEPTH.get() + 1);
        guard.active = true;
        Ok(guard)
    }

    /// 写一条控制序列：帧在攒就跟着帧走，否则直接给 writer。
    fn emit(&mut self, command: impl Command) -> io::Result<()> {
        if collecting() {
            return queue!(TermOut, command);
        }
        execute!(self.writer, command)
    }

    fn finish(mut self) -> io::Result<()> {
        self.close()
    }

    fn close(&mut self) -> io::Result<()> {
        if !self.active {
            // `begin` 半路出错：帧缓冲已经开了就收掉，别让下一帧接着往里攒。
            if self.collecting {
                FRAME.with(|frame| frame.borrow_mut().take());
            }
            return Ok(());
        }
        self.active = false;
        UPDATE_DEPTH.set(UPDATE_DEPTH.get() - 1);
        let mut result = Ok(());
        if self.outermost {
            result = self.emit(EndSynchronizedUpdate);
        }
        let cursor = match self.cursor_after {
            CursorAfterUpdate::Shown => self.emit(Show),
            CursorAfterUpdate::Hidden => self.emit(Hide),
            CursorAfterUpdate::Preserve => Ok(()),
        };
        result = result.and(cursor);
        let mut bytes = 0;
        if self.collecting {
            let frame = FRAME
                .with(|frame| frame.borrow_mut().take())
                .unwrap_or_default();
            bytes = frame.len();
            let written = match hold_frame(&frame) {
                Ok(true) => Ok(()),
                Ok(false) => self
                    .writer
                    .write_all(&frame)
                    .and_then(|()| self.writer.flush()),
                Err(error) => Err(error),
            };
            result = result.and(written);
        }
        if self.outermost {
            if let Some(started) = BLOCK_STARTED.take() {
                trace_block(started.elapsed(), bytes);
            }
        }
        result
    }
}

impl<W: Write> Drop for UpdateGuard<W> {
    fn drop(&mut self) {
        // Also release the terminal if a draw unwinds instead of returning Err.
        let _ = self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;

    /// 记下每一次 `write` 调用：「一帧一次写」要数的就是调用次数。
    #[derive(Clone, Default)]
    struct Sink {
        bytes: Rc<RefCell<Vec<u8>>>,
        writes: Rc<Cell<usize>>,
    }

    impl Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.bytes.borrow_mut().extend_from_slice(bytes);
            self.writes.set(self.writes.get() + 1);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Sink {
        fn text(&self) -> String {
            String::from_utf8(self.bytes.borrow().clone()).unwrap()
        }
    }

    #[test]
    fn nested_redraw_cannot_end_the_outer_frame() {
        let mut sink = Sink::default();
        update_to(sink.clone(), false, CursorAfterUpdate::Shown, || {
            update_to(sink.clone(), false, CursorAfterUpdate::Preserve, || {
                sink.write_all(b"body")?;
                Ok(())
            })?;
            assert!(!sink.text().contains("\x1b[?2026l"));
            sink.write_all(b"input and cursor")?;
            Ok(())
        })
        .unwrap();
        let text = sink.text();
        assert_eq!(text.matches("\x1b[?2026h").count(), 1);
        assert_eq!(text.matches("\x1b[?2026l").count(), 1);
        assert!(text.ends_with("input and cursor\x1b[?2026l\x1b[?25h"));
        assert_eq!(UPDATE_DEPTH.get(), 0);
    }

    #[test]
    fn failed_draw_closes_the_frame_and_preserves_its_error() {
        let sink = Sink::default();
        let result: Result<()> = update_to(sink.clone(), false, CursorAfterUpdate::Shown, || {
            anyhow::bail!("draw failed")
        });
        assert_eq!(result.unwrap_err().to_string(), "draw failed");
        assert!(sink.text().ends_with("\x1b[?2026l\x1b[?25h"));
        assert_eq!(UPDATE_DEPTH.get(), 0);
    }

    #[test]
    fn unwinding_nested_draw_releases_synchronization() {
        let sink = Sink::default();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _: Result<()> = update_to(sink.clone(), false, CursorAfterUpdate::Preserve, || {
                update_to(sink.clone(), false, CursorAfterUpdate::Preserve, || {
                    panic!("draw panic");
                })
            });
        }));
        assert!(result.is_err());
        assert_eq!(sink.text().matches("\x1b[?2026l").count(), 1);
        assert_eq!(UPDATE_DEPTH.get(), 0);
    }

    /// 09-23：全屏的一帧——含嵌套重画、中途 flush、光标收尾——只调一次 write，
    /// 字节顺序与原来逐段写出去的一模一样。
    #[test]
    fn a_collected_frame_leaves_in_one_write() {
        let sink = Sink::default();
        update_to(sink.clone(), true, CursorAfterUpdate::Shown, || {
            let mut out = term_out();
            out.write_all(b"body")?;
            out.flush()?;
            update_to(sink.clone(), true, CursorAfterUpdate::Shown, || {
                term_out().write_all(b"panel")?;
                Ok(())
            })?;
            assert_eq!(sink.writes.get(), 0, "帧没收尾之前一个字节都不该出门");
            out.write_all(b"input")?;
            Ok(())
        })
        .unwrap();
        assert_eq!(sink.writes.get(), 1);
        assert_eq!(
            sink.text(),
            "\x1b[?25l\x1b[?2026hbody\x1b[?25lpanel\x1b[?25hinput\x1b[?2026l\x1b[?25h"
        );
        assert!(!collecting());
        assert_eq!(UPDATE_DEPTH.get(), 0);
    }

    #[test]
    fn a_failed_collected_frame_still_leaves_and_stops_collecting() {
        let sink = Sink::default();
        let result: Result<()> = update_to(sink.clone(), true, CursorAfterUpdate::Preserve, || {
            term_out().write_all(b"half")?;
            anyhow::bail!("draw failed")
        });
        assert!(result.is_err());
        assert_eq!(sink.writes.get(), 1);
        assert_eq!(sink.text(), "\x1b[?2026hhalf\x1b[?2026l");
        assert!(!collecting());
    }
}
