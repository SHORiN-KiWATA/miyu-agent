//! 带缓冲的收帧器：异步照常一帧一帧收，另外能**不等待、不取走**地往前看一眼
//! 已经到了的帧。
//!
//! 为什么要它（09-24）：终端的提问面板是同步循环，CLI 又跑在单线程运行时上，
//! 面板开着的时候整个运行时都停着，没人读 socket。同一个会话的另一个终端（或
//! 网页）把题答了、回合在别处被取消了，这边一概不知，面板一直挂着；按两下 Esc
//! 关掉，还会把人家正跑着的回合掐掉。面板每一拍调 [`FrameReader::look_ahead`]
//! 看一眼新到的帧就知道了——只看不取，面板关掉后泵照常 [`FrameReader::receive`]，
//! 一帧不少、顺序不乱。
//!
//! 同步那一眼不走 tokio 的 `try_read`：单线程运行时被同步代码占着的时候 reactor
//! 不转，它缓存的就绪状态是旧的，不会真去读就回 `WouldBlock`。这里复制一份文件
//! 描述符交给标准库读——tokio 早把它设成了非阻塞，复制出来的描述符共享同一个
//! 打开的文件，读不到就立刻返回。背着 tokio 读走字节不碍事：它下次读到空会清掉
//! 就绪位、等下一次边沿，没读完的数据照样会再报一次可读。

use super::MAX_FRAME_BYTES;
use anyhow::{bail, Result};
use serde::de::DeserializeOwned;
use std::io::{ErrorKind, Read};
use std::os::fd::AsFd;
use tokio::io::AsyncReadExt;
use tokio::net::UnixStream;

/// 帧头：4 字节大端长度。和 [`super::send`] 写的是同一种帧。
const HEADER_BYTES: usize = 4;
/// 取走的字节攒到这么多、又占了缓冲一半以上时才挪一次，免得每取一帧都搬一遍。
const COMPACT_AFTER_BYTES: usize = 64 * 1024;

pub struct FrameReader {
    stream: UnixStream,
    /// 标准库那一份（同一个 socket 的复制描述符），第一次往前看时才开。
    peek_handle: Option<std::os::unix::net::UnixStream>,
    buffer: Vec<u8>,
    /// `buffer[..consumed]` 已经被 `receive` 取走。
    consumed: usize,
    /// `buffer[..scanned]` 里的帧 `look_ahead` 已经看过，下次从这儿接着看。
    scanned: usize,
    eof: bool,
}

impl FrameReader {
    pub fn new(stream: UnixStream) -> Self {
        Self {
            stream,
            peek_handle: None,
            buffer: Vec::new(),
            consumed: 0,
            scanned: 0,
            eof: false,
        }
    }

    /// 收下一帧；对端在帧边界上关掉连接时给 `None`。
    ///
    /// 可以安全地放进 `select!` 里被取消：读进来的字节先落缓冲，取帧是同步的。
    pub async fn receive<T: DeserializeOwned>(&mut self) -> Result<Option<T>> {
        loop {
            if let Some(frame) = self.take_frame()? {
                return Ok(Some(serde_json::from_slice(&frame)?));
            }
            if self.eof {
                if self.consumed == self.buffer.len() {
                    return Ok(None);
                }
                bail!("IPC stream closed in the middle of a frame");
            }
            self.compact();
            self.buffer.reserve(8 * 1024);
            if self.stream.read_buf(&mut self.buffer).await? == 0 {
                self.eof = true;
            }
        }
    }

    /// 不等待：把 socket 里已经到了的字节收进缓冲，再按顺序把**还没看过**的
    /// 完整帧交给 `visit`，一帧都不取走。`visit` 返回 `true` 就停在这一帧之后。
    ///
    /// 解不成 `T` 的帧跳过（它照样留给 `receive`）。
    pub fn look_ahead<T: DeserializeOwned>(
        &mut self,
        mut visit: impl FnMut(T) -> bool,
    ) -> Result<()> {
        self.fill_now()?;
        let mut cursor = self.scanned.max(self.consumed);
        while let Some((body, end)) = frame_at(&self.buffer, cursor)? {
            cursor = end;
            self.scanned = end;
            if let Ok(frame) = serde_json::from_slice::<T>(&self.buffer[body..end]) {
                if visit(frame) {
                    break;
                }
            }
        }
        Ok(())
    }

    fn take_frame(&mut self) -> Result<Option<Vec<u8>>> {
        let Some((body, end)) = frame_at(&self.buffer, self.consumed)? else {
            return Ok(None);
        };
        let frame = self.buffer[body..end].to_vec();
        self.consumed = end;
        Ok(Some(frame))
    }

    fn compact(&mut self) {
        if self.consumed == self.buffer.len() {
            self.buffer.clear();
        } else if self.consumed >= COMPACT_AFTER_BYTES && self.consumed * 2 >= self.buffer.len() {
            self.buffer.drain(..self.consumed);
        } else {
            return;
        }
        self.scanned = self.scanned.saturating_sub(self.consumed);
        self.consumed = 0;
    }

    /// 非阻塞地把已经到了的字节全收进来。
    fn fill_now(&mut self) -> Result<()> {
        if self.eof {
            return Ok(());
        }
        self.compact();
        if self.peek_handle.is_none() {
            let fd = self.stream.as_fd().try_clone_to_owned()?;
            let handle = std::os::unix::net::UnixStream::from(fd);
            // tokio 本来就要它非阻塞；再设一次是保险：万一拿到的是阻塞的描述符，
            // 面板会卡死在这一读上。
            handle.set_nonblocking(true)?;
            self.peek_handle = Some(handle);
        }
        let Some(handle) = self.peek_handle.as_mut() else {
            return Ok(());
        };
        let mut chunk = [0u8; 16 * 1024];
        loop {
            match handle.read(&mut chunk) {
                Ok(0) => {
                    self.eof = true;
                    return Ok(());
                }
                Ok(read) => self.buffer.extend_from_slice(&chunk[..read]),
                Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == ErrorKind::WouldBlock => return Ok(()),
                Err(error) => return Err(error.into()),
            }
        }
    }
}

/// `buffer[at..]` 开头若是一整帧，给出它正文的起止（`[body, end)`）。
fn frame_at(buffer: &[u8], at: usize) -> Result<Option<(usize, usize)>> {
    let Some(header) = buffer.get(at..at + HEADER_BYTES) else {
        return Ok(None);
    };
    let length = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as usize;
    if length == 0 || length > MAX_FRAME_BYTES {
        bail!("invalid IPC frame length: {length}");
    }
    let body = at + HEADER_BYTES;
    let end = body + length;
    Ok((buffer.len() >= end).then_some((body, end)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    fn frame(value: &serde_json::Value) -> Vec<u8> {
        let body = serde_json::to_vec(value).unwrap();
        let mut bytes = (body.len() as u32).to_be_bytes().to_vec();
        bytes.extend_from_slice(&body);
        bytes
    }

    /// 对端写的字节要真的落进本端的内核缓冲，同步那一眼才看得到。
    async fn settle() {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn look_ahead_sees_arrived_frames_without_consuming_them() {
        let (ours, mut theirs) = UnixStream::pair().unwrap();
        let mut reader = FrameReader::new(ours);
        for index in 0..3 {
            theirs
                .write_all(&frame(&serde_json::json!({ "n": index })))
                .await
                .unwrap();
        }
        settle().await;

        let mut seen = Vec::new();
        reader
            .look_ahead::<serde_json::Value>(|value| {
                seen.push(value["n"].as_i64().unwrap());
                false
            })
            .unwrap();
        assert_eq!(seen, vec![0, 1, 2]);

        // 看过的不再看第二遍。
        let mut again = 0;
        reader
            .look_ahead::<serde_json::Value>(|_| {
                again += 1;
                false
            })
            .unwrap();
        assert_eq!(again, 0);

        // 一帧没少、顺序没乱。
        for index in 0..3 {
            let value = reader
                .receive::<serde_json::Value>()
                .await
                .unwrap()
                .unwrap();
            assert_eq!(value["n"], index);
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn look_ahead_stops_where_visit_says_and_resumes_after_it() {
        let (ours, mut theirs) = UnixStream::pair().unwrap();
        let mut reader = FrameReader::new(ours);
        for index in 0..4 {
            theirs
                .write_all(&frame(&serde_json::json!({ "n": index })))
                .await
                .unwrap();
        }
        settle().await;
        let mut seen = Vec::new();
        reader
            .look_ahead::<serde_json::Value>(|value| {
                let n = value["n"].as_i64().unwrap();
                seen.push(n);
                n == 1
            })
            .unwrap();
        assert_eq!(seen, vec![0, 1]);
        reader
            .look_ahead::<serde_json::Value>(|value| {
                seen.push(value["n"].as_i64().unwrap());
                false
            })
            .unwrap();
        assert_eq!(seen, vec![0, 1, 2, 3]);
    }

    /// 半截帧留在缓冲里：往前看时跳过它，写完了再看得到；异步收帧从缓冲里接上。
    #[tokio::test(flavor = "current_thread")]
    async fn a_half_arrived_frame_is_neither_seen_nor_lost() {
        let (ours, mut theirs) = UnixStream::pair().unwrap();
        let mut reader = FrameReader::new(ours);
        let whole = frame(&serde_json::json!({ "kind": "question.answered" }));
        let (head, tail) = whole.split_at(whole.len() / 2);
        theirs.write_all(head).await.unwrap();
        settle().await;
        let mut seen = 0;
        reader
            .look_ahead::<serde_json::Value>(|_| {
                seen += 1;
                false
            })
            .unwrap();
        assert_eq!(seen, 0);

        theirs.write_all(tail).await.unwrap();
        settle().await;
        reader
            .look_ahead::<serde_json::Value>(|value| {
                assert_eq!(value["kind"], "question.answered");
                seen += 1;
                false
            })
            .unwrap();
        assert_eq!(seen, 1);
        let value = reader
            .receive::<serde_json::Value>()
            .await
            .unwrap()
            .unwrap();
        assert_eq!(value["kind"], "question.answered");
    }

    /// 同步那一眼要在运行时被占着（没有 await）的时候也读得到新数据——面板就是
    /// 这么占着的。先让 tokio 读空一次（它会清掉缓存的就绪位），再在不让出的情况下
    /// 看新写进来的帧。
    #[tokio::test(flavor = "current_thread")]
    async fn look_ahead_reads_while_the_runtime_is_blocked() {
        let (ours, theirs) = UnixStream::pair().unwrap();
        let mut reader = FrameReader::new(ours);
        let mut theirs = theirs.into_std().unwrap();
        theirs.set_nonblocking(false).unwrap();
        use std::io::Write as _;
        theirs
            .write_all(&frame(&serde_json::json!({ "n": 0 })))
            .unwrap();
        assert_eq!(
            reader
                .receive::<serde_json::Value>()
                .await
                .unwrap()
                .unwrap()["n"],
            0
        );
        // 以下不再 await：运行时这段时间里一直被占着。
        theirs
            .write_all(&frame(&serde_json::json!({ "n": 1 })))
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut seen = None;
        while seen.is_none() && std::time::Instant::now() < deadline {
            reader
                .look_ahead::<serde_json::Value>(|value| {
                    seen = value["n"].as_i64();
                    true
                })
                .unwrap();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(seen, Some(1));
        assert_eq!(
            reader
                .receive::<serde_json::Value>()
                .await
                .unwrap()
                .unwrap()["n"],
            1
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn closing_on_a_frame_boundary_ends_the_stream_cleanly() {
        let (ours, mut theirs) = UnixStream::pair().unwrap();
        let mut reader = FrameReader::new(ours);
        theirs
            .write_all(&frame(&serde_json::json!({ "n": 0 })))
            .await
            .unwrap();
        drop(theirs);
        settle().await;
        reader.look_ahead::<serde_json::Value>(|_| false).unwrap();
        assert!(reader
            .receive::<serde_json::Value>()
            .await
            .unwrap()
            .is_some());
        assert!(reader
            .receive::<serde_json::Value>()
            .await
            .unwrap()
            .is_none());
    }

    /// 和 `super::send` / `super::receive` 是同一种帧。
    #[tokio::test(flavor = "current_thread")]
    async fn reads_frames_written_by_the_protocol_sender() {
        let (ours, mut theirs) = UnixStream::pair().unwrap();
        let mut reader = FrameReader::new(ours);
        super::super::send(&mut theirs, &serde_json::json!({ "hello": "world" }))
            .await
            .unwrap();
        let value = reader
            .receive::<serde_json::Value>()
            .await
            .unwrap()
            .unwrap();
        assert_eq!(value["hello"], "world");
    }
}
