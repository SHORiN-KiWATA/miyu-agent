//! Background command jobs: spawn-and-forget shell processes with status
//! polling, bounded lifetimes, and orphan hygiene across restarts.
//!
//! Jobs live in the current process (daemon or direct REPL). A restart
//! terminates them — the ledger under the runtime dir lets the next
//! instance kill anything a crashed predecessor leaked. Completion invokes
//! an optional host hook (the daemon uses it to wake the model).

mod ledger;
mod output;
pub(crate) use ledger::*;
pub use output::*;

use super::{CommandOutputStream, ToolProgress, ToolRegistry, ToolSpec};
use anyhow::{bail, Context, Result};
use miyu_base::paths::MiyuPaths;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::process::Command;

const STOP_GRACE: Duration = Duration::from_secs(5);
const STATUS_POLL: Duration = Duration::from_millis(250);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JobState {
    Running,
    Exited { code: Option<i32> },
    TimedOut,
    Stopped,
}

impl JobState {
    fn label(&self) -> String {
        match self {
            JobState::Running => "running".to_string(),
            JobState::Exited { code: Some(code) } => format!("exited({code})"),
            JobState::Exited { code: None } => "exited(signal)".to_string(),
            JobState::TimedOut => "timed_out".to_string(),
            JobState::Stopped => "stopped".to_string(),
        }
    }

    fn is_terminal(&self) -> bool {
        !matches!(self, JobState::Running)
    }
}

/// What a background job actually is: an OS process group, or an in-process
/// detached subagent future.
#[derive(Clone)]
pub enum JobKind {
    Command {
        pid: u32,
    },
    Subagent {
        abort: tokio::task::AbortHandle,
        /// 开发模式子代理(dev=true):UI 的任务条据此把标签写成「开发中」。
        dev: bool,
    },
}

#[derive(Clone)]
struct JobEntry {
    job_id: String,
    title: String,
    command: String,
    workspace: PathBuf,
    session_id: Option<Arc<str>>,
    /// 触发这轮回合的终端(shellhook/单次 CLI)。任务完成后 daemon 凭它把
    /// 跟进回复写回那个终端。
    origin_tty: Option<miyu_core::ipc::OriginTty>,
    /// 平台回合里真实发起者的 user_id(如 QQ 号)。完成唤醒的合成回合凭它
    /// 继承发起者权限,而不是伪装成机器人自己(issue #29)。
    platform_sender: Option<String>,
    kind: JobKind,
    started_wall: SystemTime,
    started: Instant,
    finished: Option<Instant>,
    log_path: PathBuf,
    state: JobState,
    /// 子代理的原始进度标记流(`__subagent_reasoning__`/`__subtool_call__`…)。网页端
    /// 刷新后据它回放子过程时间线(#9:刷新丢内容)。封顶保存最近若干条,进程内、
    /// daemon 重启即清(那时任务多半也没了)。命令任务用日志文件回看,不走这。
    trace: Vec<String>,
    /// 环形缓冲从头挤掉过多少条。`trace[i]` 的**绝对序号**是 `trace_dropped + i`
    /// ——订阅方拿它判断「我上次读到的位置还在不在缓冲里」：不在就得整份重来，
    /// 不然中间那几条会静静消失、面板少一段过程。
    trace_dropped: u64,
    /// 状态行上那串量（子代理烧了多少词元）。命令类任务没有这个概念。
    metric: Option<String>,
    /// 同一个量的**数字**形态，给会话累计用。
    metric_tokens: Option<u64>,
}

/// trace 环形缓冲上限:子代理一步就几十条标记,4000 条够回放好几十步的展开区。
const MAX_TRACE: usize = 4000;

/// Completion details handed to the host hook (daemon: model wake-up).
#[derive(Clone, Debug)]
pub struct JobCompletion {
    pub job_id: String,
    pub title: String,
    /// False when the model itself stopped the command — the host should
    /// clean up UI strips but not wake the model about it.
    pub wake_requested: bool,
    /// True for detached subagents (wording of the wake prompt differs).
    pub is_subagent: bool,
    pub command: String,
    pub workspace: PathBuf,
    pub session_id: Option<Arc<str>>,
    /// 触发终端指纹,见 [`miyu_core::ipc::OriginTty`]。
    pub origin_tty: Option<miyu_core::ipc::OriginTty>,
    /// 平台回合发起者的 user_id,唤醒合成事件用它还原身份(issue #29)。
    pub platform_sender: Option<String>,
    pub state_label: String,
    pub exit_code: Option<i32>,
    pub runtime_seconds: u64,
    pub log_path: PathBuf,
}

pub type CompletionHook = Arc<dyn Fn(JobCompletion) + Send + Sync>;
pub type StartedHook = Arc<dyn Fn(JobOverview) + Send + Sync>;
/// (job_id, raw progress message) → 推给 UI 的实时进度。后台子代理的每条进度
/// 除了落任务日志,也经此上 SSE,网页端据 job_id 把它渲进那个任务的子过程流。
pub type ProgressHook = Arc<dyn Fn(&str, &str) + Send + Sync>;

/// UI-facing snapshot of one job, for status strips and IPC polling.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JobOverview {
    pub job_id: String,
    pub title: String,
    /// 跑的是哪条命令。title 是给任务条用的短标签(模型给的话截到 16 字符,
    /// 没给才拿命令前 20 字符兜底),看不出真正在跑什么;浮层要的是这一份原文。
    #[serde(default)]
    pub command: String,
    /// "command" or "subagent" — UIs word their labels by this.
    #[serde(default)]
    pub kind: String,
    /// 开发模式子代理标记;UI 据此把标签写成「开发中」。
    #[serde(default)]
    pub dev: bool,
    /// Owning turn session; UIs only strip-display jobs of their own session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// 归属会话所在子代理树的根(主会话),09-18 会话化:子代理/孙代理开的后台任务
    /// 归它们自己的会话,主会话的任务条按这个根把整棵树的任务都列出来。daemon 侧
    /// 按会话记录回填(`web::annotate_job_roots`);这里不知道树,一律 None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_session_id: Option<String>,
    pub status: String,
    pub running: bool,
    pub runtime_seconds: u64,
    /// 日志文件。全屏 TUI 点开状态行时直接读它，省掉一个专门的 IPC 往返——
    /// 日志本来就落在盘上，再造一条分页通道没意义。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_path: Option<String>,
    /// 状态行上时间左边那串量：子代理是词元数，命令没有。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metric: Option<String>,
    /// 同一个量的数字形态。跑着的时候先记在会话累计上，跑完由审计会话接手。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metric_tokens: Option<u64>,
    /// 后台子代理的子会话（子会话一建好就有）。任务条上点它打开那条会话（会话项目
    /// 第 4 段之二），刷新之后也点得进去。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_session_id: Option<String>,
}

struct JobHost {
    paths: MiyuPaths,
}

fn jobs() -> &'static Mutex<HashMap<String, JobEntry>> {
    static JOBS: OnceLock<Mutex<HashMap<String, JobEntry>>> = OnceLock::new();
    JOBS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn host() -> &'static OnceLock<JobHost> {
    static HOST: OnceLock<JobHost> = OnceLock::new();
    &HOST
}

fn completion_hook() -> &'static Mutex<Option<CompletionHook>> {
    static HOOK: OnceLock<Mutex<Option<CompletionHook>>> = OnceLock::new();
    HOOK.get_or_init(|| Mutex::new(None))
}

fn started_hook() -> &'static Mutex<Option<StartedHook>> {
    static HOOK: OnceLock<Mutex<Option<StartedHook>>> = OnceLock::new();
    HOOK.get_or_init(|| Mutex::new(None))
}

fn progress_hook() -> &'static Mutex<Option<ProgressHook>> {
    static HOOK: OnceLock<Mutex<Option<ProgressHook>>> = OnceLock::new();
    HOOK.get_or_init(|| Mutex::new(None))
}

/// Install the host started hook (daemon: publish job.started to UIs).
pub fn set_started_hook(hook: StartedHook) {
    *started_hook().lock().unwrap() = Some(hook);
}

/// Install the host progress hook (daemon: publish job.progress to UIs).
pub fn set_progress_hook(hook: ProgressHook) {
    *progress_hook().lock().unwrap() = Some(hook);
}

/// 后台任务的一条实时进度上 SSE(如已安装 hook)。子代理进度桥调用它。
pub fn publish_job_progress(job_id: &str, message: &str) {
    // 顺手把这条标记留进任务的 trace 缓冲,网页端刷新后据它回放(#9)。
    {
        let mut jobs = jobs().lock().unwrap();
        if let Some(job) = jobs.get_mut(job_id) {
            job.trace.push(message.to_string());
            if job.trace.len() > MAX_TRACE {
                let overflow = job.trace.len() - MAX_TRACE;
                job.trace.drain(0..overflow);
                job.trace_dropped += overflow as u64;
            }
        }
    }
    if let Some(hook) = progress_hook().lock().unwrap().clone() {
        hook(job_id, message);
    }
}

/// 某后台子代理任务到目前为止的原始进度标记流,给网页端刷新后回放(#9)。
pub fn job_trace(job_id: &str) -> Vec<String> {
    jobs()
        .lock()
        .unwrap()
        .get(job_id)
        .map(|job| job.trace.clone())
        .unwrap_or_default()
}

/// 只给测试用：立一个空任务，好往它的 trace 里塞标记。
#[cfg(test)]
pub(crate) fn register_test_job(label: &str) -> String {
    let job_id = next_job_id();
    let entry = JobEntry {
        job_id: job_id.clone(),
        title: label.to_string(),
        command: String::new(),
        workspace: PathBuf::new(),
        session_id: None,
        origin_tty: None,
        platform_sender: None,
        // 种类和游标算术无关；`Subagent` 那一支要一个 `AbortHandle`，测试里犯不上造。
        kind: JobKind::Command { pid: 0 },
        started_wall: SystemTime::now(),
        started: Instant::now(),
        // **立成「已完成」**：`shutdown_all` 只管 `Running` 的，而它对 Command 类
        // 任务发 `killpg(pid, …)`——pid 0 是「我自己那一组」，一个假任务就能让
        // 随后任何调用 `shutdown_all` 的测试把整个测试进程杀掉（`exit=137`，还
        // 随测试顺序时好时坏）。`signal_process_group` 那边也加了 0 的守卫。
        finished: Some(Instant::now()),
        log_path: PathBuf::new(),
        state: JobState::Exited { code: Some(0) },
        trace: Vec::new(),
        trace_dropped: 0,
        metric: None,
        metric_tokens: None,
    };
    jobs().lock().unwrap().insert(job_id.clone(), entry);
    job_id
}

/// 只给测试用：假装环形缓冲已经从头挤掉过这么多条。
#[cfg(test)]
pub(crate) fn force_trace_dropped_for_test(job_id: &str, dropped: u64) {
    if let Some(job) = jobs().lock().unwrap().get_mut(job_id) {
        job.trace_dropped = dropped;
    }
}

/// 从绝对序号 `after` 之后的那几条标记，外加**新的游标**。
///
/// 终端的后台子代理面板据它逐条跟（`Command::JobTrace`），不再 150ms 重读整份
/// 日志。返回的 `reset` 为真表示 `after` 已经被环形缓冲挤掉了——订阅方得从这份
/// （已经是缓冲里最早的那一段）重新攒，而不是接在旧的后面。
pub fn job_trace_after(job_id: &str, after: u64) -> Option<(Vec<String>, u64, bool)> {
    let jobs = jobs().lock().unwrap();
    let job = jobs.get(job_id)?;
    let base = job.trace_dropped;
    let end = base + job.trace.len() as u64;
    // 要的位置比缓冲最早那条还靠前：中间丢过东西，只能整份重来。
    let reset = after < base;
    let from = if reset { 0 } else { (after - base) as usize };
    let from = from.min(job.trace.len());
    Some((job.trace[from..].to_vec(), end, reset))
}

/// 某会话名下还在跑的后台任务数(命令 + 后台子代理的镜像任务)。子代理会话
/// 「任务完成」的判据之一(09-18):一轮结束时名下还有任务就是 waiting,不是 done。
pub fn running_job_count_for_session(session_id: &str) -> usize {
    jobs()
        .lock()
        .unwrap()
        .values()
        .filter(|job| {
            job.state == JobState::Running && job.session_id.as_deref() == Some(session_id)
        })
        .count()
}

/// 某后台任务归属的会话 id(事件按它做归属过滤:成员只收到自己那份)。
pub fn job_session_id(job_id: &str) -> Option<String> {
    jobs()
        .lock()
        .unwrap()
        .get(job_id)
        .and_then(|job| job.session_id.as_deref().map(str::to_string))
}

impl JobEntry {
    /// 命令还是子代理。UI 的任务条与工具返回值共用同一份判定,免得两处措辞跑偏。
    fn kind_label(&self) -> &'static str {
        match self.kind {
            JobKind::Command { .. } => "command",
            // 开发模式的子代理单列一类：状态行上「开发中」比「子代理」更说明
            // 它在干嘛——那一条是去写代码的，不是去查资料的。
            JobKind::Subagent { dev: true, .. } => "dev",
            JobKind::Subagent { .. } => "subagent",
        }
    }
}

fn overview_of(job: &JobEntry) -> JobOverview {
    JobOverview {
        job_id: job.job_id.clone(),
        title: job.title.clone(),
        command: job.command.clone(),
        kind: job.kind_label().to_string(),
        dev: matches!(job.kind, JobKind::Subagent { dev: true, .. }),
        log_path: Some(job.log_path.display().to_string()),
        session_id: job.session_id.as_deref().map(str::to_string),
        root_session_id: None,
        status: job.state.label(),
        metric: job.metric.clone(),
        metric_tokens: job.metric_tokens,
        child_session_id: matches!(job.kind, JobKind::Subagent { .. })
            .then(|| crate::tools::subagent::child_session_of_job(&job.job_id))
            .flatten(),
        running: !job.state.is_terminal(),
        runtime_seconds: job
            .finished
            .unwrap_or_else(Instant::now)
            .duration_since(job.started)
            .as_secs(),
    }
}

/// Jobs the UI status strip should show: running only — finished commands
/// are reported by the wake follow-up, so a terminal chip carries no
/// information.
pub fn overview() -> Vec<JobOverview> {
    let mut jobs = jobs().lock().unwrap();
    prune_expired_terminal(&mut jobs);
    let mut rows = jobs
        .values()
        .filter(|job| !job.state.is_terminal())
        .collect::<Vec<_>>();
    rows.sort_by_key(|job| job.started_wall);
    rows.into_iter().map(overview_of).collect()
}

/// 后台任务日志的尾部(UI 展开后台命令那行看输出用,09-12)。命令没有
/// job.progress 流,输出只落在日志文件里;点开时前端拉这个尾巴、运行中轮询。
/// 返回 (文本, 是否还在运行)。
pub fn job_log_tail(job_id: &str, max_bytes: usize) -> Option<(String, bool)> {
    let job = job_snapshot(job_id)?;
    let running = !job.state.is_terminal();
    let bytes = std::fs::read(&job.log_path).unwrap_or_default();
    let start = bytes.len().saturating_sub(max_bytes);
    // 从 max_bytes 边界起可能切进多字节字符中间,from_utf8_lossy 兜底。
    let text = String::from_utf8_lossy(&bytes[start..]).into_owned();
    Some((text, running))
}

/// 记下这个任务当前烧了多少——状态行上时间左边那串就是它。
///
/// 子代理跑着的时候每调一次工具报一次（见 `SubagentRunner::report_metric`）。
/// 不落日志：那是给人翻的，不是给数字刷屏的。
pub fn set_metric(job_id: &str, metric: &str, tokens: Option<u64>) {
    let mut jobs = jobs().lock().unwrap();
    if let Some(job) = jobs.get_mut(job_id) {
        job.metric = if metric.trim().is_empty() {
            None
        } else {
            Some(metric.trim().to_string())
        };
        job.metric_tokens = tokens;
    }
}

/// 完成且已报告的任务直接从注册表移除(验收 08-16 用户反馈:"做完了
/// 也不删除,一直留着占用后台"——此前只打标记,条目终身堆积)。日志
/// 文件留在磁盘,唤醒消息里带着 log_path,要翻旧账用 read。
pub fn acknowledge(job_id: &str) {
    let mut jobs = jobs().lock().unwrap();
    let terminal = jobs.get(job_id).is_some_and(|job| job.state.is_terminal());
    if terminal {
        jobs.remove(job_id);
    }
}

/// 兜底:唤醒没送达(宿主死亡/失败路径)的终态条目,完成满一小时后
/// 清出注册表——注册表只该装"还需要被看见"的任务。
const TERMINAL_RETENTION: Duration = Duration::from_secs(3600);

fn prune_expired_terminal(jobs: &mut HashMap<String, JobEntry>) {
    jobs.retain(|_, job| {
        !(job.state.is_terminal()
            && job
                .finished
                .is_some_and(|finished| finished.elapsed() > TERMINAL_RETENTION))
    });
}

/// Install the host completion hook (daemon: wake the model). Replaces any
/// previous hook; pass-through for the direct REPL which sets none.
pub fn set_completion_hook(hook: CompletionHook) {
    *completion_hook().lock().unwrap() = Some(hook);
}

/// One-time host init: remembers paths and sweeps ledger entries
/// left behind by dead predecessor processes.
pub fn init(paths: &MiyuPaths) {
    let _ = host().set(JobHost {
        paths: paths.clone(),
    });
    sweep_stale_jobs(paths);
    cleanup_old_logs(paths);
}

fn require_host() -> Result<&'static JobHost> {
    host()
        .get()
        .context("background jobs are not initialized in this process")
}

/// Terminate every job owned by this process; called on daemon shutdown
/// and direct-REPL exit so setsid'd children never outlive their host.
pub fn shutdown_all() {
    let running = jobs()
        .lock()
        .unwrap()
        .values()
        .filter(|job| job.state == JobState::Running)
        .map(|job| job.kind.clone())
        .collect::<Vec<_>>();
    let mut pids = Vec::new();
    for kind in &running {
        match kind {
            JobKind::Command { pid } => {
                signal_process_group(*pid, libc::SIGTERM);
                pids.push(*pid);
            }
            JobKind::Subagent { abort, .. } => abort.abort(),
        }
    }
    if !pids.is_empty() {
        std::thread::sleep(Duration::from_millis(300));
        for pid in pids {
            if process_alive(pid) {
                signal_process_group(pid, libc::SIGKILL);
            }
        }
    }
    if let Some(host) = host().get() {
        sync_ledger(&host.paths);
    }
}

/// Spawn `command` detached in its own process group; stdout+stderr stream
/// into a log file. Returns the tool JSON for run_command.
pub async fn spawn_background(
    command: &str,
    title: Option<&str>,
    progress: &ToolProgress,
) -> Result<String> {
    let host = require_host()?;
    let title = title
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.chars().take(16).collect::<String>())
        .unwrap_or_else(|| {
            let mut fallback = command.chars().take(20).collect::<String>();
            if fallback.len() < command.len() {
                fallback.push('…');
            }
            fallback
        });
    let job_id = next_job_id();
    let dir = logs_dir(&host.paths);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create job log dir {}", dir.display()))?;
    let log_path = dir.join(format!("{job_id}.log"));
    let log = std::fs::File::create(&log_path)
        .with_context(|| format!("failed to create job log {}", log_path.display()))?;
    let workspace = miyu_base::workspace::effective_workdir();
    let mut process = Command::new("sh");
    process
        .arg("-lc")
        .arg(command)
        .current_dir(&workspace)
        // 工具桥环境:后台脚本里的 `miyu tool-call` 也能以本会话身份执行。
        .envs(
            miyu_base::workspace::try_session()
                .map(|session| ("MIYU_SESSION".to_string(), session.to_string())),
        )
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(log.try_clone()?))
        .stderr(std::process::Stdio::from(log));
    process.process_group(0);
    miyu_base::sandbox::confine(&mut process);
    let mut child = process
        .spawn()
        .context("failed to spawn the background job")?;
    let pid = child.id().context("background job has no pid")?;
    let entry = JobEntry {
        job_id: job_id.clone(),
        title,
        command: command.to_string(),
        workspace: workspace.clone(),
        session_id: miyu_base::workspace::try_session(),
        origin_tty: miyu_base::workspace::current_origin_tty(),
        platform_sender: miyu_base::workspace::current_platform_sender(),
        kind: JobKind::Command { pid },
        started_wall: SystemTime::now(),
        started: Instant::now(),
        finished: None,
        log_path: log_path.clone(),
        state: JobState::Running,
        trace: Vec::new(),
        trace_dropped: 0,
        metric: None,
        metric_tokens: None,
    };
    let started = overview_of(&entry);
    jobs().lock().unwrap().insert(job_id.clone(), entry);
    sync_ledger(&host.paths);
    if let Some(hook) = started_hook().lock().unwrap().clone() {
        hook(started);
    }

    let reaper_job_id = job_id.clone();
    tokio::spawn(async move {
        // 后台任务不设运行时长上限:自然退出为准。泄漏保护由
        // sweep_stale_jobs(死进程清扫)与 job_stop 显式停止承担;
        // JobState::TimedOut 仅为兼容旧账本记录保留。
        let state = match child.wait().await {
            Ok(status) => match status.code() {
                Some(code) => JobState::Exited { code: Some(code) },
                None => JobState::Exited { code: None },
            },
            Err(_) => JobState::Exited { code: None },
        };
        finalize_job(&reaper_job_id, state, true);
    });

    // Surface the job id in the tool's visible output stream so the user
    // can see at a glance which job this call started.
    progress.report_command_output(
        CommandOutputStream::Stdout,
        format!(
            "{} {job_id}\n",
            miyu_base::i18n::text("Running in background:", "已后台运行")
        )
        .into_bytes(),
    );

    Ok(serde_json::to_string_pretty(&json!({
        "ok": true,
        "job_id": job_id,
        "pid": pid,
        "log": log_path.display().to_string(),
        "note": "Background command running. You will be woken automatically when it finishes — do not poll job(action=status) to wait; query it only when you need interim logs. Never assume the result before completion."
    }))?)
}

/// Detach a subagent as a background job: allocate an id and log file,
/// register the entry, spawn the provided future, and finalize through the
/// same completion hook as background commands (same wake, strip, stop).
/// The builder receives (job_id, log_path) so the future can stream its
/// progress into the log that `job_status` reads.
pub async fn spawn_background_subagent<F>(
    title: Option<&str>,
    description: &str,
    dev: bool,
    progress: &ToolProgress,
    build: impl FnOnce(String, PathBuf) -> F,
) -> Result<String>
where
    F: std::future::Future<Output = JobState> + Send + 'static,
{
    let (job_id, log_path) = register_background_subagent(title, description, dev, build)?;
    // Subagent detach note rides its own progress channel so it lands as the
    // block's ↳ subject line; CommandOutput is dropped for non-run_command.
    progress.report(format!(
        "__subagent_detach__{} {job_id}",
        miyu_base::i18n::text("Running in background:", "已后台运行")
    ));
    Ok(serde_json::to_string_pretty(&json!({
        "ok": true,
        "kind": "background_subagent",
        "job_id": job_id,
        "log": log_path.display().to_string(),
        "note": "Subagent detached to the background. Query with job(action=status) (the log holds its progress); never assume its result before it finishes — you will be woken automatically when it completes."
    }))?)
}

/// 只登记、不出工具回执的那一半，返回 (job_id, 日志路径)。daemon 重启后把子代理挂回
/// 父会话名下（09-24 断点续跑）走这里：那时没有正在跑的工具调用可以回话。任务归属
/// 的会话、工作目录取调用处的 task-local，调用方要先套好。
pub fn register_background_subagent<F>(
    title: Option<&str>,
    description: &str,
    dev: bool,
    build: impl FnOnce(String, PathBuf) -> F,
) -> Result<(String, PathBuf)>
where
    F: std::future::Future<Output = JobState> + Send + 'static,
{
    let host = require_host()?;
    let job_id = next_job_id();
    let dir = logs_dir(&host.paths);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create job log dir {}", dir.display()))?;
    let log_path = dir.join(format!("{job_id}.log"));
    std::fs::write(&log_path, b"")
        .with_context(|| format!("failed to create job log {}", log_path.display()))?;
    let title = title
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.chars().take(16).collect::<String>())
        .unwrap_or_else(|| {
            let mut fallback = description.chars().take(16).collect::<String>();
            if fallback.chars().count() < description.chars().count() {
                fallback.push('…');
            }
            fallback
        });
    let fut = build(job_id.clone(), log_path.clone());
    let reaper_job_id = job_id.clone();
    let handle = tokio::spawn(async move {
        let state = fut.await;
        finalize_job(&reaper_job_id, state, true);
    });
    let entry = JobEntry {
        job_id: job_id.clone(),
        title,
        command: description.to_string(),
        workspace: miyu_base::workspace::effective_workdir(),
        session_id: miyu_base::workspace::try_session(),
        origin_tty: miyu_base::workspace::current_origin_tty(),
        platform_sender: miyu_base::workspace::current_platform_sender(),
        kind: JobKind::Subagent {
            dev,
            abort: handle.abort_handle(),
        },
        started_wall: SystemTime::now(),
        started: Instant::now(),
        finished: None,
        log_path: log_path.clone(),
        state: JobState::Running,
        trace: Vec::new(),
        trace_dropped: 0,
        metric: None,
        metric_tokens: None,
    };
    let started = overview_of(&entry);
    jobs().lock().unwrap().insert(job_id.clone(), entry);
    if let Some(hook) = started_hook().lock().unwrap().clone() {
        hook(started);
    }
    Ok((job_id, log_path))
}

fn finalize_job(job_id: &str, state: JobState, wake_requested: bool) {
    let completion = {
        let mut jobs = jobs().lock().unwrap();
        let Some(job) = jobs.get_mut(job_id) else {
            return;
        };
        if job.state.is_terminal() {
            return;
        }
        job.state = state.clone();
        job.finished = Some(Instant::now());
        JobCompletion {
            job_id: job.job_id.clone(),
            title: job.title.clone(),
            wake_requested,
            is_subagent: matches!(job.kind, JobKind::Subagent { .. }),
            command: job.command.clone(),
            workspace: job.workspace.clone(),
            session_id: job.session_id.clone(),
            origin_tty: job.origin_tty.clone(),
            platform_sender: job.platform_sender.clone(),
            state_label: state.label(),
            exit_code: match state {
                JobState::Exited { code } => code,
                _ => None,
            },
            runtime_seconds: job.started.elapsed().as_secs(),
            log_path: job.log_path.clone(),
        }
    };
    if let Some(host) = host().get() {
        sync_ledger(&host.paths);
    }
    let hook = completion_hook().lock().unwrap().clone();
    if let Some(hook) = hook {
        hook(completion);
    }
}

impl JobEntry {
    fn pid(&self) -> Option<u32> {
        match &self.kind {
            JobKind::Command { pid } => Some(*pid),
            JobKind::Subagent { .. } => None,
        }
    }
}

fn job_snapshot(job_id: &str) -> Option<JobEntry> {
    jobs().lock().unwrap().get(job_id).cloned()
}

/// Job ids a call is asking about: the `job_ids` array first, then a scalar
/// `job_id`, de-duplicated while keeping the caller's order. Shared by
/// `job_status` and `job_stop` — a plain `dedup()` only drops *adjacent*
/// repeats, so `["a","b","a"]` used to slip two `a`s through.
fn requested_job_ids(args: &Value) -> Vec<String> {
    let mut ids: Vec<String> = super::string_list(args.get("job_ids"));
    if let Some(single) = args
        .get("job_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        ids.push(single.to_string());
    }
    let mut seen = std::collections::HashSet::new();
    ids.retain(|id| seen.insert(id.clone()));
    ids
}

/// Rejects ids owned by another session, mirroring the `all=true` escape
/// hatch both job tools offer.
fn ensure_jobs_visible(ids: &[String], current: Option<&str>, all: bool, verb: &str) -> Result<()> {
    let jobs = jobs().lock().unwrap();
    for id in ids {
        if let Some(job) = jobs.get(id) {
            if !job_visible(job.session_id.as_deref(), current, all) {
                bail!("background job {id} belongs to another session; pass all=true if you really need to {verb} it");
            }
        }
    }
    Ok(())
}

/// Session-scoped visibility: a tool call only sees jobs of its own turn
/// session unless it passes `all=true`.
///
/// **调用方在会话里、任务却没有会话** → 不可见（fail-closed）。以前这条走
/// `_ => true`，于是任何没带 session 的任务（台账恢复的旧任务、异常路径漏抓
/// 的）对**所有**会话可见——一个会话能 stop 掉另一个会话的东西。逃生门是
/// `all=true`，它本来就是给「我知道我在干什么」准备的。
///
/// 调用方**不在**会话里（测试、turn scope 外的直接调用）仍然全局可见：那里
/// 没有「本会话」可言，收紧只会让工具桥不可用。
fn job_visible(job_session: Option<&str>, current: Option<&str>, all: bool) -> bool {
    if all {
        return true;
    }
    match (current, job_session) {
        (Some(current), Some(session)) => current == session,
        (Some(_), None) => false,
        (None, _) => true,
    }
}

#[cfg(test)]
mod visibility_tests {
    use super::job_visible;

    /// 可见性是权限判定,不是显示偏好——`ensure_jobs_visible` 用它决定能不能
    /// stop 掉一个任务。所以「拿不准」必须是不可见。
    #[test]
    fn a_sessionless_job_is_invisible_from_inside_a_session() {
        // 同会话:看得见。
        assert!(job_visible(Some("s1"), Some("s1"), false));
        // 别的会话:看不见。
        assert!(!job_visible(Some("s2"), Some("s1"), false));
        // 任务没有会话,而我在某个会话里 —— 以前这里是 true,于是台账恢复的
        // 旧任务、异常路径漏抓 session 的任务对**所有**会话可见,一个会话能
        // stop 掉另一个会话的东西。
        assert!(!job_visible(None, Some("s1"), false));
        // all=true 是逃生门,上面三种一律放行。
        for job_session in [Some("s1"), Some("s2"), None] {
            assert!(job_visible(job_session, Some("s1"), true));
        }
        // 调用方不在会话里(测试、turn scope 外的工具桥):没有「本会话」可言,
        // 收紧只会让它不可用,保持全局可见。
        assert!(job_visible(Some("s1"), None, false));
        assert!(job_visible(None, None, false));
    }
}

/// Stop every running job bound to `session_id`; returns how many were
/// stopped. Used when the owning REPL exits — background commands follow
/// their conversation's lifecycle.
pub async fn stop_session_jobs(session_id: &str) -> usize {
    let targets = jobs()
        .lock()
        .unwrap()
        .values()
        .filter(|job| {
            job.state == JobState::Running && job.session_id.as_deref() == Some(session_id)
        })
        .map(|job| job.job_id.clone())
        .collect::<Vec<_>>();
    // Concurrent: serial stops used to add up, and with a stubborn child each
    // one held the caller for the whole grace period.
    let outcomes =
        futures_util::future::join_all(targets.iter().map(|job_id| stop_job(job_id))).await;
    outcomes.into_iter().filter(Result::is_ok).count()
}

/// Host-initiated stop (WebUI strip ✕ button); same semantics as job_stop.
pub async fn stop_job(job_id: &str) -> Result<()> {
    job_stop(json!({ "job_id": job_id })).await.map(|_| ())
}

async fn job_stop(args: Value) -> Result<String> {
    let ids = requested_job_ids(&args);
    if ids.is_empty() {
        bail!("provide at least one of job_id or job_ids; usage: job({{\"action\":\"stop\",\"job_ids\":[\"abc123\"]}})");
    }
    let all = args.get("all").and_then(Value::as_bool).unwrap_or(false);
    let current = miyu_base::workspace::try_session();
    ensure_jobs_visible(&ids, current.as_deref(), all, "stop")?;
    if ids.len() > 1 {
        // Concurrent for the same reason as `stop_session_jobs`; per-id errors
        // still stay per-id rather than aborting the batch.
        let outcomes = futures_util::future::join_all(ids.iter().map(|id| stop_one(id))).await;
        let results = ids
            .iter()
            .zip(outcomes)
            .map(|(id, outcome)| match outcome {
                Ok(status) => json!({ "job_id": id, "ok": true, "status": status }),
                Err(error) => json!({ "job_id": id, "ok": false, "error": error.to_string() }),
            })
            .collect::<Vec<_>>();
        return Ok(serde_json::to_string_pretty(&json!({
            "ok": true,
            "results": results,
        }))?);
    }
    let job_id = &ids[0];
    let job =
        job_snapshot(job_id).with_context(|| format!("background job {job_id} does not exist"))?;
    if job.state.is_terminal() {
        return Ok(serde_json::to_string_pretty(&json!({
            "ok": true,
            "job_id": job_id,
            "status": job.state.label(),
            "note": "the background task had already finished",
        }))?);
    }
    let status = stop_one(job_id).await?;
    Ok(serde_json::to_string_pretty(&json!({
        "ok": true,
        "job_id": job_id,
        "status": status,
    }))?)
}

/// Stop a single job; returns its resulting status label.
async fn stop_one(job_id: &str) -> Result<String> {
    let job =
        job_snapshot(job_id).with_context(|| format!("background job {job_id} does not exist"))?;
    if job.state.is_terminal() {
        return Ok(job.state.label());
    }
    // Mark terminal first so the reaper's own finalize becomes a no-op;
    // wake_requested=false tells the host to clean up without waking.
    finalize_job(job_id, JobState::Stopped, false);
    acknowledge(job_id);
    match &job.kind {
        JobKind::Command { pid } => {
            let pid = *pid;
            // SIGTERM goes out synchronously so a well-behaved child is
            // already dying when this returns. Only the grace period and the
            // SIGKILL escalation are detached — waiting them out inline is
            // what made Ctrl+C feel frozen, and it bought nothing: the job was
            // marked terminal above and has already left `overview()`.
            signal_process_group(pid, libc::SIGTERM);
            tokio::spawn(async move {
                let deadline = Instant::now() + STOP_GRACE;
                while process_alive(pid) && Instant::now() < deadline {
                    tokio::time::sleep(STATUS_POLL).await;
                }
                if process_alive(pid) {
                    signal_process_group(pid, libc::SIGKILL);
                }
            });
        }
        JobKind::Subagent { abort, .. } => abort.abort(),
    }
    Ok("stopped".to_string())
}

/// 后台任务查询与停止合并成一件 `job`(08-17):同一个对象的两种操作。
///
/// 曾经还有一个只注册 status 的只读变体,注释说是给聊天/平台面用的——但从
/// 来没有任何地方调用它,受限平台注册表压根不含任何后台任务工具。一并删掉,
/// 别留着一段撒谎的死代码。
pub fn register_management(registry: &mut ToolRegistry) {
    registry.register(job_spec().writes().with_always_loaded(false));
}

fn job_spec() -> ToolSpec {
    let actions = json!(["status", "stop"]);
    let action_hint = "status inspects, stop terminates. Defaults to status.";
    let description = "Inspect background jobs. action=status with no other argument lists this session's jobs, each with recent_output (log tail) and log_size. For one job's incremental output pass job_id plus offset; for several pass job_ids (the log budget is split between them). action=stop terminates jobs (commands get SIGTERM then SIGKILL; subagents are aborted), single or by job_ids. Add all=true to reach other sessions.";
    ToolSpec::new(
        "job",
        description,
        json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": actions, "description": action_hint },
                "all": { "type": "boolean", "description": "true reaches other sessions' jobs." },
                "job_id": { "type": "string", "description": "A single job id." },
                "job_ids": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Several job ids at once; for status the log budget is split between them."
                },
                "offset": { "type": "integer", "minimum": 0, "description": "status only: byte offset to resume reading the log from (use the previous next_offset)." }
            },
            "additionalProperties": false
        }),
        move |args| async move {
            match args.get("action").and_then(Value::as_str).unwrap_or("status") {
                "status" => job_status(args).await,
                "stop" => job_stop(args).await,
                other => bail!("unknown action: {other}; expected status or stop"),
            }
        },
    )
    .with_display_name(miyu_base::i18n::text("Background jobs", "后台任务"))
}

#[cfg(test)]
mod tests;
