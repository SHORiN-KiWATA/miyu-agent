//! 执行 shell 命令。
//!
//! `CommandProcessGroup` 把子进程放进独立进程组，`Drop` 时整组杀掉——只杀直接
//! 子进程的话，它拉起的孙子进程会活下来占着终端。
//!
//! 输出边读边截（`read_command_output`），不是先收完再截：一个 `yes` 就能把内存
//! 吃光。

use crate::tools::default_tools::*;

pub(in crate::tools) const MAX_COMMAND_OUTPUT_CHARS: usize = 20_000;

/// 前台不传 `timeout_seconds` 时的预算。对齐 Claude Code / opencode 的 2 分钟：
/// 30 秒会把 `cargo build`、`npm test` 这类常见活儿一律打断。
pub(in crate::tools) const FOREGROUND_DEFAULT_TIMEOUT_SECS: u64 = 120;

/// 前台能请求的上限。对齐 Claude Code 的 10 分钟——再长的活儿本该走
/// `background=true`：前台只有一个回合的位置，不该被单个命令占满。
pub(in crate::tools) const FOREGROUND_MAX_TIMEOUT_SECS: u64 = 600;

/// 前台命令的超时预算。
///
/// `requested` 只用来报错。上一版是**静默** clamp：写 300 秒实际生效 120 秒，
/// 报错却是 tokio 原生的 `deadline has elapsed`——不说工具、不说生效秒数、
/// 不说长活走哪条路。拿到那种错误只能换着数字乱试（用户实测就是这么撞的）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::tools) struct CommandTimeout {
    pub(in crate::tools) requested: u64,
    pub(in crate::tools) effective: u64,
}

impl CommandTimeout {
    /// 缺省吃默认，越界收敛到 `[1, FOREGROUND_MAX_TIMEOUT_SECS]`。下限取 1：
    /// `timeout_seconds=0` 会让命令连启动的机会都没有。
    pub(in crate::tools) fn from_args(args: &Value) -> Self {
        let requested = args
            .get("timeout_seconds")
            .and_then(Value::as_u64)
            .unwrap_or(FOREGROUND_DEFAULT_TIMEOUT_SECS);
        Self {
            requested,
            effective: requested.clamp(1, FOREGROUND_MAX_TIMEOUT_SECS),
        }
    }

    /// 超时错误要自带出路：生效多少秒、请求值有没有被压掉、长活该怎么跑。
    pub(in crate::tools) fn error(&self) -> anyhow::Error {
        let mut message = format!("command timed out after {}s", self.effective);
        if self.requested > self.effective {
            message.push_str(&format!(
                " (requested {}s, foreground cap {}s)",
                self.requested, FOREGROUND_MAX_TIMEOUT_SECS
            ));
        }
        message.push_str(". Use background=true for longer work.");
        anyhow::anyhow!(message)
    }
}

pub(in crate::tools) async fn run_command(
    args: Value,
    allowed: bool,
    progress: ToolProgress,
    full_output_dir: &Path,
) -> Result<String> {
    if !allowed {
        bail!("{}", "command execution is disabled; set skills.allow_command_execution=true in config.jsonc to enable run_command");
    }
    let command = required(&args, "command")?;
    if args
        .get("background")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        let title = args.get("title").and_then(Value::as_str);
        return crate::tools::jobs::spawn_background(&command, title, &progress).await;
    }
    execute_command(
        &command,
        CommandTimeout::from_args(&args),
        progress,
        Some(full_output_dir),
    )
    .await
}

/// `full_output_dir`：输出被截断时把全文存到这里、在结果里给出路径（09-24 B8）。
/// 模型只看得到末尾两万字，编译日志这类长输出的开头往往才是第一个报错。
pub(in crate::tools) async fn execute_command(
    command: &str,
    timeout: CommandTimeout,
    progress: ToolProgress,
    full_output_dir: Option<&Path>,
) -> Result<String> {
    let mut command_process = Command::new("sh");
    command_process
        .arg("-lc")
        .arg(command)
        // Explicit cwd: shell commands must run in the turn workspace, not
        // whatever the daemon process cwd happens to be.
        .current_dir(miyu_base::workspace::effective_workdir());
    // 工具桥环境(任务#12):脚本里 `miyu tool-call` 凭这些以本回合的
    // 会话身份/来源打回 daemon 执行结构化工具,内层调用照走 guard 管线。
    if let Some(session) = miyu_base::workspace::try_session() {
        command_process.env("MIYU_SESSION", &*session);
    }
    if let Ok(origin) = serde_json::to_string(&miyu_base::workspace::current_turn_origin()) {
        command_process.env("MIYU_TURN_ORIGIN", origin);
    }
    command_process.env(
        "MIYU_BRIDGE_DEPTH",
        miyu_base::workspace::current_bridge_depth().to_string(),
    );
    command_process
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command_process.process_group(0);
    // 成员回合:子进程套 Landlock(策略在回合的 task-local 上;管理员没有)。
    miyu_base::sandbox::confine(&mut command_process);
    let mut child = command_process.spawn()?;
    let mut process_group = CommandProcessGroup::new(child.id());
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("failed to capture command stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("failed to capture command stderr"))?;

    // 输出边读边存进共享缓冲：超时时读输出的 future 连同它手里的缓冲一起被丢掉，
    // 原来已经打出来的输出也就跟着没了，模型只拿到一句「超时」（09-24 B8）。
    let stdout_sink = OutputSink::default();
    let stderr_sink = OutputSink::default();
    let execution =
        tokio::time::timeout(std::time::Duration::from_secs(timeout.effective), async {
            tokio::join!(
                child.wait(),
                read_command_output(
                    stdout,
                    progress.clone(),
                    |progress, chunk| {
                        progress.report_command_output(CommandOutputStream::Stdout, chunk);
                    },
                    stdout_sink.clone(),
                ),
                read_command_output(
                    stderr,
                    progress,
                    |progress, chunk| {
                        progress.report_command_output(CommandOutputStream::Stderr, chunk);
                    },
                    stderr_sink.clone(),
                ),
            )
        })
        .await;

    match execution {
        Ok((status, stdout, stderr)) => {
            process_group.disarm();
            let status = status?;
            stdout?;
            stderr?;
            let (stdout, stderr) = (stdout_sink.take(), stderr_sink.take());
            let mut text = command_text(status, stdout.clone(), stderr.clone());
            append_full_output_path(&mut text, command, &stdout, &stderr, full_output_dir);
            Ok(text)
        }
        Err(_) => {
            process_group.terminate();
            let _ = child.start_kill();
            let _ = child.wait().await;
            process_group.disarm();
            let (stdout, stderr) = (stdout_sink.take(), stderr_sink.take());
            let mut message = timeout.error().to_string();
            let (body, _) = command_body(&stdout, &stderr);
            if !body.is_empty() {
                message.push_str("\nOutput before the timeout:\n");
                message.push_str(&body);
            }
            append_full_output_path(&mut message, command, &stdout, &stderr, full_output_dir);
            Err(anyhow::anyhow!(message))
        }
    }
}

/// 读输出的缓冲放在 future 外面，超时丢掉 future 时它还在。
#[derive(Clone, Default)]
pub(in crate::tools) struct OutputSink(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl OutputSink {
    fn push(&self, chunk: &[u8]) {
        if let Ok(mut buffer) = self.0.lock() {
            buffer.extend_from_slice(chunk);
        }
    }

    fn len(&self) -> usize {
        self.0.lock().map(|buffer| buffer.len()).unwrap_or(0)
    }

    pub(in crate::tools) fn take(&self) -> Vec<u8> {
        self.0
            .lock()
            .map(|mut buffer| std::mem::take(&mut *buffer))
            .unwrap_or_default()
    }
}

/// 输出被截断时把全文存盘，并在结果末尾写明路径。存盘目录里超过 7 天的旧文件顺手清掉。
fn append_full_output_path(
    text: &mut String,
    command: &str,
    stdout: &[u8],
    stderr: &[u8],
    full_output_dir: Option<&Path>,
) {
    let Some(dir) = full_output_dir else {
        return;
    };
    if !output_clipped(stdout) && !output_clipped(stderr) {
        return;
    }
    if let Some(path) = save_full_output(dir, command, stdout, stderr) {
        text.push_str(&format!("\n[full output saved to {}]", path.display()));
    }
}

const FULL_OUTPUT_RETENTION: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 3600);

fn save_full_output(dir: &Path, command: &str, stdout: &[u8], stderr: &[u8]) -> Option<PathBuf> {
    std::fs::create_dir_all(dir).ok()?;
    prune_old_full_outputs(dir);
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or(0);
    let path = dir.join(format!("{millis}-{:04x}.log", rand::random::<u16>()));
    let mut content = format!("$ {command}\n\n");
    content.push_str(&String::from_utf8_lossy(stdout));
    if !stderr.is_empty() {
        content.push_str("\n[stderr]\n");
        content.push_str(&String::from_utf8_lossy(stderr));
    }
    std::fs::write(&path, content).ok()?;
    Some(path)
}

fn prune_old_full_outputs(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let stale = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age > FULL_OUTPUT_RETENTION);
        if stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

pub(in crate::tools) struct CommandProcessGroup {
    #[cfg(unix)]
    pub(in crate::tools) pgid: Option<i32>,
}

impl CommandProcessGroup {
    pub fn new(child_id: Option<u32>) -> Self {
        Self {
            #[cfg(unix)]
            pgid: child_id.and_then(|id| i32::try_from(id).ok()),
        }
    }

    pub(in crate::tools) fn terminate(&self) {
        #[cfg(unix)]
        if let Some(pgid) = self.pgid {
            unsafe {
                libc::kill(-pgid, libc::SIGKILL);
            }
        }
    }

    pub(in crate::tools) fn disarm(&mut self) {
        #[cfg(unix)]
        {
            self.pgid = None;
        }
    }
}

impl Drop for CommandProcessGroup {
    fn drop(&mut self) {
        self.terminate();
    }
}

/// Cumulative cap for collected command output. Beyond it the stream is
/// still drained (so the child never blocks on a full pipe) but no longer
/// buffered or forwarded — unbounded collection plus a clone per chunk
/// into the progress channel is a memory hazard on runaway commands.
pub(in crate::tools) const MAX_COMMAND_OUTPUT_BYTES: usize = 8 * 1024 * 1024;

pub(in crate::tools) async fn read_command_output(
    mut reader: impl tokio::io::AsyncRead + Unpin,
    progress: ToolProgress,
    report: impl Fn(&ToolProgress, Vec<u8>),
    output: OutputSink,
) -> std::io::Result<()> {
    let mut truncated = false;
    let mut buffer = [0; 8192];
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        let remaining = MAX_COMMAND_OUTPUT_BYTES.saturating_sub(output.len());
        if remaining == 0 {
            truncated = true;
            continue;
        }
        let take = read.min(remaining);
        if take < read {
            truncated = true;
        }
        let chunk = buffer[..take].to_vec();
        output.push(&chunk);
        report(&progress, chunk);
    }
    if truncated {
        // 截断标记进入工具返回体（模型上下文），走 agent_text 恒英文。
        output.push("\n[output truncated at the 8MB cap]".as_bytes());
    }
    Ok(())
}

/// dsh 式纯文本返回体(08-17)。此前每条结果都裹一层 pretty-print JSON,
/// 里面 6 个字段在说"什么都没发生"(`stderr:""`、`*_truncated:false`、
/// `*_omitted_chars:0`、`success:true`)——实测一次 `uname -r; pwd; ls`
/// 407 字符里信封占 217(53%)。
///
/// 新形态:正文就是 stdout;有 stderr 才追加 `[stderr]` 段;完全没输出时
/// 精确输出 `(no output)`;截断和非零退出码各自只在真发生时补一行标记。
/// 退出码是通用 Unix 词汇,不额外解释。
pub(in crate::tools) fn command_text(
    status: std::process::ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
) -> String {
    let (mut body, _) = command_body(&stdout, &stderr);
    if body.is_empty() {
        body.push_str("(no output)");
    }
    if let Some(code) = status.code() {
        if code != 0 {
            body.push_str(&format!("\n[exit code: {code}]"));
        }
    } else {
        // 没有退出码 = 被信号杀掉;不说一声模型会把它当成功。
        body.push_str("\n[killed by signal]");
    }
    body
}

/// 正文部分：stdout，有 stderr 才追加 `[stderr]` 段，各自截到末尾两万字。返回值第二项
/// 表示有没有截掉东西。超时报错也用它，所以不带退出码。
fn command_body(stdout: &[u8], stderr: &[u8]) -> (String, bool) {
    let stdout = clip_output_with_meta(&String::from_utf8_lossy(stdout));
    let stderr = clip_output_with_meta(&String::from_utf8_lossy(stderr));
    let mut body = stdout.text.trim_end().to_string();
    if !stderr.text.trim().is_empty() {
        if !body.is_empty() {
            body.push('\n');
        }
        body.push_str("[stderr]\n");
        body.push_str(stderr.text.trim_end());
    }
    (body, stdout.truncated || stderr.truncated)
}

fn output_clipped(bytes: &[u8]) -> bool {
    clip_output_with_meta(&String::from_utf8_lossy(bytes)).truncated
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(in crate::tools) struct ClippedOutput {
    pub(in crate::tools) text: String,
    pub(in crate::tools) truncated: bool,
    pub(in crate::tools) omitted_chars: usize,
}

#[cfg(any(test, feature = "testkit"))]
mod test_support;
