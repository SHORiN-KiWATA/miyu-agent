//! 一个 MCP 服务器进程和它的 stdio 连接（09-25）。
//!
//! - 按请求 id 分发回话，能同时挂好几个请求（同一会话里并发调同一件服务器）。
//! - 服务器反过来问我们的（ping）要回，不回的话有的服务器会当我们掉线。
//! - 请求超时或调用方不等了（回合取消），发 `notifications/cancelled` 撤掉那一个请求，
//!   进程留着。
//! - stderr 留一截尾巴，服务器死了带进报错——原来整个丢掉，出事没法查。
//! - 进程单独成一个进程组：npx/uvx/浏览器会再起子进程，只杀直接子进程会留下孤儿。
//!   收的时候先关 stdin（MCP 约定：服务器读到 EOF 自己退），再 SIGTERM 整组，最后 SIGKILL；
//!   连接没收就被丢掉（任务被撤）时整组 SIGKILL。
//!
//! 只能在 MCP 线程上起（`runtime`）：读写管道的任务挂在那边的 runtime 上。

use super::protocol::{self, Incoming, InitializeResult, McpToolInfo, ToolsListResult};
use super::scope::{self, SpawnScope};
use anyhow::{anyhow, bail, Context, Result};
use miyu_base::config::McpServerConfig;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};

/// stderr 留几行。
const STDERR_TAIL_LINES: usize = 20;

/// 报错里带的 stderr 最多这么长（字符）。
const STDERR_IN_ERROR_CHARS: usize = 800;

/// 关 stdin 之后等它自己退、SIGTERM 之后再等，各这么久。
const EXIT_GRACE: Duration = Duration::from_secs(2);

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value>>>>>;

/// 写端任务收的东西。
enum Outgoing {
    Line(String),
    /// 关 stdin（收进程的第一步）。
    Close,
}

pub(super) struct McpConnection {
    server_id: String,
    pid: u32,
    writer: mpsc::UnboundedSender<Outgoing>,
    pending: Pending,
    next_id: AtomicU64,
    alive: Arc<AtomicBool>,
    stderr: Arc<Mutex<VecDeque<String>>>,
    /// 连着几次超时（回话到了就清零）。
    timeouts: AtomicU32,
    /// 握手时服务器给的使用说明。
    instructions: Option<String>,
    /// 还没收（`shutdown` 取走）。丢掉连接时还在就整组 SIGKILL。
    child: Mutex<Option<tokio::process::Child>>,
    /// 宿主查询令牌：进程活着就有效，连接丢掉就作废。
    _host_grant: Option<miyu_base::host_ports::HostGrantGuard>,
}

impl McpConnection {
    /// 起进程并握手。
    pub(super) async fn open(server: &McpServerConfig, scope: &SpawnScope) -> Result<Self> {
        let mut connection = Self::spawn(server, scope)?;
        let timeout = request_timeout(server);
        let result = connection
            .request("initialize", protocol::initialize_params(), timeout)
            .await?;
        let parsed: InitializeResult = serde_json::from_value(result).unwrap_or_default();
        connection.instructions = parsed.instructions.filter(|text| !text.trim().is_empty());
        connection.notify("notifications/initialized", json!({}));
        Ok(connection)
    }

    fn spawn(server: &McpServerConfig, scope: &SpawnScope) -> Result<Self> {
        if server.command.trim().is_empty() {
            bail!("MCP server {} has no command", server.id);
        }
        let mut command = tokio::process::Command::new(&server.command);
        command.args(&server.args);
        for (key, value) in &server.env {
            command.env(key, value);
        }
        if let Some(workdir) = scope.workdir.as_ref().filter(|dir| dir.is_dir()) {
            command.current_dir(workdir);
        }
        // 宿主查询令牌(09-16):声明了能力且在 daemon 里才有;服务器用
        // `$MIYU_HOST_BIN host <method>` 查(与脚本同一条路)。
        let capabilities = host_capabilities_for_server(server);
        let host_grant = miyu_base::host_ports::issue_host_grant(&capabilities);
        if let Some(grant) = &host_grant {
            command.env("MIYU_HOST_TOKEN", grant.token());
            command.env("MIYU_HOST_CAPABILITIES", capabilities.join(","));
            if let Ok(binary) = miyu_base::paths::miyu_executable() {
                command.env("MIYU_HOST_BIN", binary);
            }
        }
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .kill_on_drop(true);
        if let Some(policy) = scope.policy_for(server) {
            miyu_base::sandbox::confine_with(&mut command, &policy, &scope::writable_paths(server));
        }
        let mut child = command
            .spawn()
            .with_context(|| format!("failed to start MCP server {}", server.id))?;
        let pid = child
            .id()
            .with_context(|| format!("MCP server {} exited at once", server.id))?;
        let stdin = child.stdin.take().context("failed to open MCP stdin")?;
        let stdout = child.stdout.take().context("failed to open MCP stdout")?;
        let stderr_pipe = child.stderr.take().context("failed to open MCP stderr")?;

        let (writer, outgoing) = mpsc::unbounded_channel();
        tokio::spawn(write_loop(stdin, outgoing));

        let stderr = Arc::new(Mutex::new(VecDeque::new()));
        tokio::spawn(stderr_loop(stderr_pipe, stderr.clone(), server.id.clone()));

        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let alive = Arc::new(AtomicBool::new(true));
        tokio::spawn(read_loop(ReadLoop {
            stdout,
            // 弱引用：连接丢掉后写端要能关（读端还挂着一个强引用的话 stdin 永远不关，
            // 服务器等不到 EOF，读端也就永远等不到它退出）。
            answers: writer.downgrade(),
            pending: pending.clone(),
            alive: alive.clone(),
            stderr: stderr.clone(),
            server_id: server.id.clone(),
        }));

        Ok(Self {
            server_id: server.id.clone(),
            pid,
            writer,
            pending,
            next_id: AtomicU64::new(1),
            alive,
            stderr,
            timeouts: AtomicU32::new(0),
            instructions: None,
            child: Mutex::new(Some(child)),
            _host_grant: host_grant,
        })
    }

    pub(super) fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Acquire)
    }

    pub(super) fn pid(&self) -> u32 {
        self.pid
    }

    /// 连着几次超时。
    pub(super) fn consecutive_timeouts(&self) -> u32 {
        self.timeouts.load(Ordering::Acquire)
    }

    pub(super) fn instructions(&self) -> Option<&str> {
        self.instructions.as_deref()
    }

    pub(super) async fn list_tools(&self, timeout: Duration) -> Result<Vec<McpToolInfo>> {
        let result = self.request("tools/list", json!({}), timeout).await?;
        let parsed: ToolsListResult = serde_json::from_value(result)?;
        Ok(parsed.tools)
    }

    pub(super) async fn call_tool(
        &self,
        tool: &str,
        args: Value,
        timeout: Duration,
    ) -> Result<String> {
        let result = self
            .request(
                "tools/call",
                json!({"name": tool, "arguments": args}),
                timeout,
            )
            .await?;
        Ok(protocol::format_mcp_result(&result))
    }

    async fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        if !self.is_alive() {
            bail!("{}", self.stopped_error());
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, sender);
        let mut in_flight = InFlight {
            id,
            pending: self.pending.clone(),
            writer: self.writer.clone(),
            armed: true,
        };
        if self
            .writer
            .send(Outgoing::Line(protocol::request_line(id, method, params)))
            .is_err()
        {
            bail!("{}", self.stopped_error());
        }
        match tokio::time::timeout(timeout, receiver).await {
            Ok(Ok(outcome)) => {
                in_flight.armed = false;
                self.timeouts.store(0, Ordering::Release);
                outcome
            }
            // 读端退出时把挂着的请求一并回了错；这里是它连错都没来得及回。
            Ok(Err(_)) => {
                in_flight.armed = false;
                bail!("{}", self.stopped_error())
            }
            Err(_) => {
                self.timeouts.fetch_add(1, Ordering::AcqRel);
                bail!(
                    "MCP server {} did not answer {method} within {}s",
                    self.server_id,
                    timeout.as_secs()
                )
            }
        }
    }

    fn notify(&self, method: &str, params: Value) {
        let _ = self
            .writer
            .send(Outgoing::Line(protocol::notification_line(method, params)));
    }

    /// 服务器已经停了的报错，带上 stderr 的尾巴。
    pub(super) fn stopped_error(&self) -> String {
        let tail = stderr_tail(&self.stderr);
        if tail.is_empty() {
            format!("MCP server {} stopped", self.server_id)
        } else {
            format!("MCP server {} stopped; stderr: {tail}", self.server_id)
        }
    }

    /// 收进程：关 stdin 等它自己退，不退就 SIGTERM 整组，再不退 SIGKILL。服务器自己退了，
    /// 它起的子进程不一定跟着退（npx 下面的 node、浏览器）：最后整组再扫一遍。
    pub(super) async fn shutdown(&self) {
        self.alive.store(false, Ordering::Release);
        let Some(mut child) = self.child.lock().unwrap().take() else {
            return;
        };
        let _ = self.writer.send(Outgoing::Close);
        if tokio::time::timeout(EXIT_GRACE, child.wait())
            .await
            .is_err()
        {
            signal_group(self.pid, libc::SIGTERM);
            if tokio::time::timeout(EXIT_GRACE, child.wait())
                .await
                .is_err()
            {
                signal_group(self.pid, libc::SIGKILL);
                let _ = child.wait().await;
            }
        }
        signal_group(self.pid, libc::SIGKILL);
    }
}

impl Drop for McpConnection {
    fn drop(&mut self) {
        if self.child.lock().unwrap().is_some() {
            signal_group(self.pid, libc::SIGKILL);
        }
    }
}

/// 一个挂着的请求：没等到回话就被丢掉（超时、调用方撤了）时，从挂账里划掉并告诉服务器
/// 别做了。
struct InFlight {
    id: u64,
    pending: Pending,
    writer: mpsc::UnboundedSender<Outgoing>,
    armed: bool,
}

impl Drop for InFlight {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        self.pending.lock().unwrap().remove(&self.id);
        let _ = self.writer.send(Outgoing::Line(protocol::notification_line(
            "notifications/cancelled",
            json!({"requestId": self.id, "reason": "client stopped waiting"}),
        )));
    }
}

async fn write_loop(
    mut stdin: tokio::process::ChildStdin,
    mut outgoing: mpsc::UnboundedReceiver<Outgoing>,
) {
    while let Some(message) = outgoing.recv().await {
        let Outgoing::Line(line) = message else {
            break;
        };
        let written = async {
            stdin.write_all(line.as_bytes()).await?;
            stdin.write_all(b"\n").await?;
            stdin.flush().await
        }
        .await;
        if written.is_err() {
            break;
        }
    }
}

async fn stderr_loop(
    stderr: tokio::process::ChildStderr,
    tail: Arc<Mutex<VecDeque<String>>>,
    server_id: String,
) {
    let mut lines = BufReader::new(stderr).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        tracing::debug!(server = %server_id, "MCP stderr: {line}");
        let mut tail = tail.lock().unwrap();
        if tail.len() == STDERR_TAIL_LINES {
            tail.pop_front();
        }
        tail.push_back(line);
    }
}

struct ReadLoop {
    stdout: tokio::process::ChildStdout,
    answers: mpsc::WeakUnboundedSender<Outgoing>,
    pending: Pending,
    alive: Arc<AtomicBool>,
    stderr: Arc<Mutex<VecDeque<String>>>,
    server_id: String,
}

async fn read_loop(state: ReadLoop) {
    let mut lines = BufReader::new(state.stdout).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        match protocol::classify(&line) {
            Some(Incoming::Response { id, outcome }) => {
                if let Some(sender) = state.pending.lock().unwrap().remove(&id) {
                    let _ = sender.send(outcome.map_err(|error| anyhow!(error.describe())));
                }
            }
            Some(Incoming::Request { id, method }) => {
                if let Some(writer) = state.answers.upgrade() {
                    let _ = writer.send(Outgoing::Line(protocol::answer_line(&id, &method)));
                }
            }
            Some(Incoming::Notification) | None => {}
        }
    }
    state.alive.store(false, Ordering::Release);
    // stderr 可能比 stdout 晚一点读完：给它一眼时间，报错里才带得上最后几行。
    tokio::time::sleep(Duration::from_millis(50)).await;
    let tail = stderr_tail(&state.stderr);
    let message = if tail.is_empty() {
        format!("MCP server {} closed stdout", state.server_id)
    } else {
        format!(
            "MCP server {} closed stdout; stderr: {tail}",
            state.server_id
        )
    };
    for (_, sender) in state.pending.lock().unwrap().drain() {
        let _ = sender.send(Err(anyhow!(message.clone())));
    }
}

fn stderr_tail(tail: &Mutex<VecDeque<String>>) -> String {
    let joined = tail
        .lock()
        .unwrap()
        .iter()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" | ");
    let count = joined.chars().count();
    if count <= STDERR_IN_ERROR_CHARS {
        return joined;
    }
    joined.chars().skip(count - STDERR_IN_ERROR_CHARS).collect()
}

fn signal_group(pid: u32, signal: libc::c_int) {
    // SAFETY: 只发信号，负 pid = 整个进程组（起进程时 `process_group(0)`）。
    unsafe {
        libc::kill(-(pid as libc::pid_t), signal);
    }
}

/// 一次请求等多久：服务器配置里的秒数。
pub(super) fn request_timeout(server: &McpServerConfig) -> Duration {
    Duration::from_secs(server.timeout_seconds.max(1))
}

/// 服务器配置里声明的能力 ∩ 宿主词表;不认识的记 warn 丢掉(与脚本同一规则)。
pub(super) fn host_capabilities_for_server(server: &McpServerConfig) -> Vec<String> {
    server
        .capabilities
        .iter()
        .filter(|id| {
            let known = miyu_base::host_ports::is_known_capability(id);
            if !known {
                tracing::warn!(server = %server.id, capability = %id, "unknown host capability ignored");
            }
            known
        })
        .cloned()
        .collect()
}
