//! agy 进程复用:同一条 Miyu 会话连续几轮共用一个 agy 进程(09-18)。
//!
//! 09-16 真机统计(147 轮):冷启动中位 8.8s,只调一次模型的短回合冷启动占整轮
//! 95%;其中「起进程→登录完成 2.9s、登录→挂上会话 2.1s」与工具表完全无关。agy 的
//! stream-json 本来就支持一个进程连打多轮(不关 stdin 即可),实测同进程三轮
//! 10.69s / 2.41s / 6.03s。原来 `process.rs` 写完载荷就关 stdin,每轮一个新进程。
//!
//! 规矩:
//! - 只在**同一条会话、工具面没变**时复用:键 = 二进制 + 启动参数(去掉
//!   `--conversation`)+ 环境 + 工作目录 + 工具面档位 + 桥的 eager 名单——这些
//!   任何一样变了(人格文件改了 → 代理名变;MCP 注册变;换模型/档位;管理员与群友
//!   交替发言换脸)就是另一把钥匙,旧进程留到闲置回收。一条会话最多挂一个进程。
//! - 借出去就从池里拿走(同一进程不会被两轮同时用);还回来才再进池。
//! - 闲置超过 `plugins.antigravity.reuse_idle_seconds` 回收(拿/还的时候顺手扫,
//!   另有一根 30 秒的巡检);池最多 [`CAP`] 个,超了先收最久没用的。
//! - 会话级粘性 ERROR 的进程不进池(那条 agy 会话已经退休);清空/删除 Miyu 会话时
//!   它名下的进程一并收掉;配置重载(人格/MCP 可能变了)与 daemon 关停全部收掉。
//! - 借来的进程死了(写不进 stdin)就当没借到,照旧起新进程带 `--conversation`
//!   续传——复用只省时间,不改正确性。

use crate::llm::openai_compatible::cli_relay::process::RelayProcess;
use crate::llm::openai_compatible::*;

/// 池里最多挂几个进程。每个 agy 常驻几百 MB,不能按会话数无限挂。
const CAP: usize = 4;

/// 巡检间隔。
const SWEEP_INTERVAL: Duration = Duration::from_secs(30);

struct Parked {
    fingerprint: String,
    miyu_session: String,
    conversation_id: String,
    process: RelayProcess,
    expires_at: Instant,
    turns: u32,
}

static POOL: Mutex<Vec<Parked>> = Mutex::new(Vec::new());

/// 一个进程「能不能接着用」的指纹:启动参数(不含 `--conversation`)、环境、
/// 二进制、工作目录、工具面档位、桥的 eager 名单、沙盒策略。
///
/// 沙盒是起进程时装上的、装上就撤不掉。09-23 之前策略变了总会连带改掉提示词
/// (进 `--agent`)或工作目录,指纹间接跟着变;沙盒说明挪出系统提示词、又能按 Tab
/// 在同一个目录下切只读之后,不直接算进来就会把没受限或规则过期的进程借出去。
pub(super) fn fingerprint(
    binary: &std::path::Path,
    base_args: &[String],
    env: &[(String, Option<String>)],
    workdir: &std::path::Path,
    host_tools: bool,
    eager_tools: &[String],
    sandbox: Option<&miyu_base::sandbox::SandboxPolicy>,
) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(binary.display().to_string().as_bytes());
    hasher.update(b"\0args\0");
    for arg in base_args {
        hasher.update(arg.as_bytes());
        hasher.update(b"\0");
    }
    hasher.update(b"\0env\0");
    for (key, value) in env {
        hasher.update(key.as_bytes());
        hasher.update(b"=");
        hasher.update(value.as_deref().unwrap_or("\u{1}unset").as_bytes());
        hasher.update(b"\0");
    }
    hasher.update(b"\0workdir\0");
    hasher.update(workdir.display().to_string().as_bytes());
    hasher.update(if host_tools { b"\0host=1" } else { b"\0host=0" });
    hasher.update(b"\0eager\0");
    for tool in eager_tools {
        hasher.update(tool.as_bytes());
        hasher.update(b"\0");
    }
    hasher.update(b"\0sandbox\0");
    if let Some(policy) = sandbox {
        hasher.update(format!("{policy:?}").as_bytes());
    }
    hasher.finalize().to_hex().to_string()
}

fn retire_parked(parked: Parked, reason: &str) {
    tracing::info!(
        target: "miyu::relay",
        pid = parked.process.pid(),
        session = %parked.miyu_session,
        conversation = %parked.conversation_id,
        turns = parked.turns,
        "retiring pooled agy process: {reason}"
    );
    parked.process.retire();
}

/// 扫掉过期的;`now` 传进来是为了测试。
fn sweep_locked(pool: &mut Vec<Parked>, now: Instant) {
    let mut kept = Vec::with_capacity(pool.len());
    for parked in pool.drain(..) {
        if parked.expires_at <= now {
            retire_parked(parked, "idle timeout");
        } else {
            kept.push(parked);
        }
    }
    *pool = kept;
}

/// 借一个能接着用的进程:键、会话、agy 会话 id 三样都对上。顺手把这条会话名下
/// **别的**钥匙的进程收掉(工具面/人格变了,它不会再被用到),过期的也收掉。
pub(super) fn take(
    fingerprint: &str,
    miyu_session: &str,
    conversation_id: &str,
) -> Option<(RelayProcess, u32)> {
    let mut pool = POOL.lock().ok()?;
    sweep_locked(&mut pool, Instant::now());
    let mut hit = None;
    let mut kept = Vec::with_capacity(pool.len());
    for parked in pool.drain(..) {
        if parked.miyu_session != miyu_session {
            kept.push(parked);
        } else if hit.is_none()
            && parked.fingerprint == fingerprint
            && parked.conversation_id == conversation_id
        {
            hit = Some(parked);
        } else {
            retire_parked(
                parked,
                "the session moved to a different tool face or conversation",
            );
        }
    }
    *pool = kept;
    let parked = hit?;
    tracing::info!(
        target: "miyu::relay",
        pid = parked.process.pid(),
        session = %miyu_session,
        conversation = %conversation_id,
        turns = parked.turns,
        "reusing pooled agy process"
    );
    Some((parked.process, parked.turns))
}

/// 本轮跑完还回来。超容量先收最久没用的。
pub(super) fn park(
    fingerprint: &str,
    miyu_session: &str,
    conversation_id: &str,
    process: RelayProcess,
    idle: Duration,
    turns: u32,
) {
    ensure_sweeper();
    let Ok(mut pool) = POOL.lock() else {
        process.retire();
        return;
    };
    let now = Instant::now();
    sweep_locked(&mut pool, now);
    // 同一条会话只挂一个。
    let mut kept = Vec::with_capacity(pool.len() + 1);
    for parked in pool.drain(..) {
        if parked.miyu_session == miyu_session {
            retire_parked(parked, "replaced by a newer process of the same session");
        } else {
            kept.push(parked);
        }
    }
    *pool = kept;
    while pool.len() >= CAP {
        // 最早过期的就是最久没用的。
        let Some(oldest) = pool
            .iter()
            .enumerate()
            .min_by_key(|(_, parked)| parked.expires_at)
            .map(|(index, _)| index)
        else {
            break;
        };
        retire_parked(pool.remove(oldest), "pool is full");
    }
    tracing::debug!(
        target: "miyu::relay",
        pid = process.pid(),
        session = %miyu_session,
        idle_seconds = idle.as_secs(),
        "parking agy process for reuse"
    );
    pool.push(Parked {
        fingerprint: fingerprint.to_string(),
        miyu_session: miyu_session.to_string(),
        conversation_id: conversation_id.to_string(),
        process,
        expires_at: now + idle,
        turns,
    });
}

/// 清空/删除 Miyu 会话:它名下的进程收掉。
pub(in crate::llm::openai_compatible) fn forget_session(miyu_session: &str) {
    let Ok(mut pool) = POOL.lock() else {
        return;
    };
    let mut kept = Vec::with_capacity(pool.len());
    for parked in pool.drain(..) {
        if parked.miyu_session == miyu_session {
            retire_parked(parked, "the Miyu session was cleared");
        } else {
            kept.push(parked);
        }
    }
    *pool = kept;
}

/// 全部收掉(同步):配置重载——人格文件、MCP 注册、模型都可能变了,常驻进程
/// 手里的是旧世界。
pub fn retire_all(reason: &str) {
    let Ok(mut pool) = POOL.lock() else {
        return;
    };
    for parked in pool.drain(..) {
        retire_parked(parked, reason);
    }
}

/// 全部收掉并等它们退出(异步):daemon 关停。子进程各在自己的进程组里,不等
/// 就会变成孤儿继续跑。
pub async fn shutdown_all() {
    let drained: Vec<Parked> = match POOL.lock() {
        Ok(mut pool) => pool.drain(..).collect(),
        Err(_) => return,
    };
    for mut parked in drained {
        tracing::info!(
            target: "miyu::relay",
            pid = parked.process.pid(),
            session = %parked.miyu_session,
            "terminating pooled agy process: daemon shutdown"
        );
        parked.process.terminate().await;
    }
}

/// 池里当前挂着的会话(测试用)。
#[cfg(test)]
pub(super) fn parked_sessions() -> Vec<(String, u32)> {
    POOL.lock()
        .map(|pool| {
            pool.iter()
                .map(|parked| (parked.miyu_session.clone(), parked.process.pid()))
                .collect()
        })
        .unwrap_or_default()
}

/// 巡检任务只起一次:没人来拿/还的时候闲置进程也要按时收。没有运行时
/// (同步测试)就不起,靠拿/还时的顺手扫。
fn ensure_sweeper() {
    static STARTED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    STARTED.get_or_init(|| {
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async {
                loop {
                    tokio::time::sleep(SWEEP_INTERVAL).await;
                    if let Ok(mut pool) = POOL.lock() {
                        sweep_locked(&mut pool, Instant::now());
                    }
                }
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn fake_process() -> RelayProcess {
        RelayProcess::spawn_persistent(
            std::path::Path::new("sh"),
            &["-c".to_string(), "cat >/dev/null".to_string()],
            std::path::Path::new("/"),
            &[],
            "",
            Duration::from_secs(30),
            "test",
            "fake",
            || "missing".to_string(),
        )
        .await
        .unwrap()
    }

    fn alive(pid: u32) -> bool {
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }

    async fn gone_soon(pid: u32) -> bool {
        for _ in 0..60 {
            if !alive(pid) {
                return true;
            }
            // 僵尸也算「没了」:父进程还没 wait,但它已经不跑了。
            if std::fs::read_to_string(format!("/proc/{pid}/stat"))
                .map(|stat| stat.contains(") Z"))
                .unwrap_or(true)
            {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        false
    }

    /// 指纹:参数/环境/档位/eager 名单任一变都是另一把钥匙。
    #[test]
    fn fingerprint_changes_with_any_ingredient() {
        let bin = std::path::Path::new("agy");
        let wd = std::path::Path::new("/w");
        let base = fingerprint(bin, &["--a".into()], &[], wd, true, &[], None);
        assert_eq!(
            base,
            fingerprint(bin, &["--a".into()], &[], wd, true, &[], None)
        );
        assert_ne!(
            base,
            fingerprint(bin, &["--b".into()], &[], wd, true, &[], None)
        );
        assert_ne!(
            base,
            fingerprint(
                bin,
                &["--a".into()],
                &[("K".into(), Some("v".into()))],
                wd,
                true,
                &[],
                None
            )
        );
        assert_ne!(
            base,
            fingerprint(bin, &["--a".into()], &[], wd, false, &[], None)
        );
        assert_ne!(
            base,
            fingerprint(bin, &["--a".into()], &[], wd, true, &["t".into()], None)
        );
        assert_ne!(
            base,
            fingerprint(
                bin,
                &["--a".into()],
                &[],
                std::path::Path::new("/x"),
                true,
                &[],
                None
            )
        );
        // 同一目录下切只读(09-23):沙盒不同就不能复用同一个进程。
        let readonly = miyu_base::sandbox::SandboxPolicy {
            read_only_mode: true,
            ..Default::default()
        };
        assert_ne!(
            base,
            fingerprint(bin, &["--a".into()], &[], wd, true, &[], Some(&readonly))
        );
    }

    /// 借还:键+会话+agy 会话 id 都对才借得到;同会话换钥匙旧的收掉;过期的收掉;
    /// 清会话收掉;retire_all 收光。全程用 `sh -c cat` 当假 agy。
    #[tokio::test]
    async fn take_park_expire_and_forget() {
        retire_all("test reset");
        let idle = Duration::from_secs(60);
        let p1 = fake_process().await;
        let pid1 = p1.pid();
        park("fp-a", "sess-1", "conv-1", p1, idle, 1);
        assert!(
            take("fp-a", "sess-1", "conv-other").is_none(),
            "会话 id 不对借不到"
        );
        // 上一行的 take 判定「同会话别的会话 id」→ 收掉了 p1。
        assert!(gone_soon(pid1).await, "换了会话 id 的旧进程要收掉");

        let p2 = fake_process().await;
        let pid2 = p2.pid();
        park("fp-a", "sess-1", "conv-1", p2, idle, 1);
        let (borrowed, turns) = take("fp-a", "sess-1", "conv-1").expect("三样都对能借到");
        assert_eq!(borrowed.pid(), pid2);
        assert_eq!(turns, 1);
        assert!(
            take("fp-a", "sess-1", "conv-1").is_none(),
            "借走了池里就没有了"
        );
        park("fp-a", "sess-1", "conv-1", borrowed, idle, 2);

        // 同会话换钥匙:旧的收掉,新的挂上。
        let p3 = fake_process().await;
        let pid3 = p3.pid();
        park("fp-b", "sess-1", "conv-1", p3, idle, 1);
        assert!(gone_soon(pid2).await);
        assert!(alive(pid3));

        // 过期:expires_at 已过的在下一次拿/还时收掉。
        let p4 = fake_process().await;
        let pid4 = p4.pid();
        park("fp-c", "sess-2", "conv-2", p4, Duration::from_millis(1), 1);
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(take("fp-c", "sess-2", "conv-2").is_none(), "过期的借不到");
        assert!(gone_soon(pid4).await);

        // 清会话。
        forget_session("sess-1");
        assert!(gone_soon(pid3).await);
        assert!(parked_sessions().is_empty());

        let p5 = fake_process().await;
        let pid5 = p5.pid();
        park("fp-a", "sess-3", "conv-3", p5, idle, 1);
        retire_all("test");
        assert!(gone_soon(pid5).await);
        assert!(parked_sessions().is_empty());
    }
}
