use super::{readable_tool_name, ToolProgress, ToolRegistry};
use anyhow::{bail, Result};
use miyu_base::i18n::is_zh;
use miyu_core::llm::{
    ChatMessage, ChatResult, ChatStreamChunk, ChatStreamKind, OpenAiCompatibleClient, Usage,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Checkpoint of an interrupted subagent run: the full live message history
/// (including every completed tool round) plus the consumed step budget, so a
/// follow-up `task` call with `resume_id` continues instead of starting over.
/// 内存一份(快)+ 磁盘一份(跨 daemon 重启也能续,09-12 用户报「不落盘导致
/// resume 不可靠」)。磁盘落 `<state>/subagent-checkpoints/<id>.json`。
struct SubagentCheckpoint {
    messages: Vec<ChatMessage>,
    steps: usize,
    created: Instant,
}

/// 磁盘上的检查点(Instant 不可序列化,单独存一个 unix 秒的创建时间做 TTL)。
#[derive(serde::Serialize, serde::Deserialize)]
struct DiskCheckpoint {
    messages: Vec<ChatMessage>,
    steps: usize,
    created_unix: u64,
}

const CHECKPOINT_TTL: Duration = Duration::from_secs(2 * 60 * 60);
const CHECKPOINT_CAP: usize = 32;
const STREAM_ATTEMPTS: usize = 3;

fn checkpoints() -> &'static Mutex<HashMap<String, SubagentCheckpoint>> {
    static STORE: OnceLock<Mutex<HashMap<String, SubagentCheckpoint>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

static CHECKPOINT_DIR: OnceLock<std::path::PathBuf> = OnceLock::new();

/// daemon 启动时定一次落盘目录并清掉过期文件(server::run 调,紧挨 jobs::init)。
pub fn init_checkpoint_dir(paths: &miyu_base::paths::MiyuPaths) {
    let dir = paths.state_dir.join("subagent-checkpoints");
    let _ = std::fs::create_dir_all(&dir);
    let _ = CHECKPOINT_DIR.set(dir);
    // 落盘目录定了再清过期文件(prune 读 CHECKPOINT_DIR)。
    prune_disk_checkpoints();
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn checkpoint_path(id: &str) -> Option<std::path::PathBuf> {
    CHECKPOINT_DIR
        .get()
        .map(|dir| dir.join(format!("{id}.json")))
}

fn store_checkpoint(messages: Vec<ChatMessage>, steps: usize) -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    // id 带上 pid:重启后计数器归 1,不含 pid 会撞上盘里旧进程的 subagent-ckpt-1。
    let id = format!(
        "subagent-ckpt-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    // 先落盘(跨重启),再进内存(同进程快取)。
    if let Some(path) = checkpoint_path(&id) {
        let disk = DiskCheckpoint {
            messages: messages.clone(),
            steps,
            created_unix: now_unix(),
        };
        if let Ok(bytes) = serde_json::to_vec(&disk) {
            let _ = std::fs::write(&path, bytes);
        }
        // 落盘的容量/TTL 清理:超龄或超量都删最旧的。
        prune_disk_checkpoints();
    }
    let mut store = checkpoints().lock().unwrap();
    store.retain(|_, ckpt| ckpt.created.elapsed() < CHECKPOINT_TTL);
    if store.len() >= CHECKPOINT_CAP {
        let oldest = store
            .iter()
            .min_by_key(|(_, ckpt)| ckpt.created)
            .map(|(key, _)| key.clone());
        if let Some(oldest) = oldest {
            store.remove(&oldest);
        }
    }
    store.insert(
        id.clone(),
        SubagentCheckpoint {
            messages,
            steps,
            created: Instant::now(),
        },
    );
    id
}

fn prune_disk_checkpoints() {
    let Some(dir) = CHECKPOINT_DIR.get() else {
        return;
    };
    let now = now_unix();
    let mut files: Vec<(u64, std::path::PathBuf)> = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let created = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<DiskCheckpoint>(&bytes).ok())
            .map(|ckpt| ckpt.created_unix)
            .unwrap_or(0);
        if now.saturating_sub(created) >= CHECKPOINT_TTL.as_secs() {
            let _ = std::fs::remove_file(&path);
            continue;
        }
        files.push((created, path));
    }
    if files.len() > CHECKPOINT_CAP {
        files.sort_by_key(|(created, _)| *created);
        for (_, path) in files.iter().take(files.len() - CHECKPOINT_CAP) {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn take_checkpoint(id: &str) -> Option<(Vec<ChatMessage>, usize)> {
    {
        let mut store = checkpoints().lock().unwrap();
        store.retain(|_, ckpt| ckpt.created.elapsed() < CHECKPOINT_TTL);
        if let Some(ckpt) = store.remove(id) {
            if let Some(path) = checkpoint_path(id) {
                let _ = std::fs::remove_file(path);
            }
            return Some((ckpt.messages, ckpt.steps));
        }
    }
    // 内存没有(通常是 daemon 重启过):从盘里捞回来。
    let path = checkpoint_path(id)?;
    let bytes = std::fs::read(&path).ok()?;
    let disk: DiskCheckpoint = serde_json::from_slice(&bytes).ok()?;
    let _ = std::fs::remove_file(&path);
    if now_unix().saturating_sub(disk.created_unix) >= CHECKPOINT_TTL.as_secs() {
        return None;
    }
    Some((disk.messages, disk.steps))
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum ProgressMode {
    Hidden,
    Summary,
    Full,
}

impl ProgressMode {
    pub fn from_config(config: &miyu_base::config::AppConfig) -> Self {
        // 配置里只剩「工具内容展不展开」一位（用户 09-17 删掉隐藏档）。
        // `Hidden` 还留着是给网页端那条默认收起的路用的。
        if config.display.expand_tool_calls {
            Self::Full
        } else {
            Self::Summary
        }
    }
}

use crate::tools::subagent::protocol::REASONING_DONE_MARKER;

#[derive(Clone)]
pub struct SubagentProgress {
    progress: ToolProgress,
    tool_mode: ProgressMode,
    enabled: bool,
    /// 这一段思考从什么时候开始的。段结束时报一条耗时。
    ///
    /// **标记流里原来没有任何时间信息。** 写日志那一侧自己掐表、把耗时写进
    /// `[思考] 1.2s\t…`；而后台面板 09-17 起优先订标记流（阶段 4），于是面板上
    /// 的思考全都成了光秃秃的「已思考」——同一件事，换条路看就没有耗时了。
    /// 掐表的地方只能在**发的这一侧**：它是唯一知道"这一段什么时候开始、什么
    /// 时候结束"的人。
    reasoning_since: std::sync::Arc<std::sync::Mutex<Option<std::time::Instant>>>,
    /// 这次内层工具调用是什么时候发出去的。结果回来时算耗时写进那条标记里。
    ///
    /// 同 [`Self::reasoning_since`]：标记流里没有时间戳。写日志那一侧原来是自己
    /// 掐表的（`log.rs` 的 `last_call`），订标记流的面板于是一个工具的耗时都
    /// 看不到——抬头上只剩 `运行命令 · 看看输出`，没有 `· 1.2s`。
    tool_since: std::sync::Arc<std::sync::Mutex<Option<std::time::Instant>>>,
}

impl SubagentProgress {
    pub fn new(progress: ToolProgress, tool_mode: ProgressMode, enabled: bool) -> Self {
        Self {
            progress,
            tool_mode,
            enabled,
            reasoning_since: std::sync::Arc::new(std::sync::Mutex::new(None)),
            tool_since: std::sync::Arc::new(std::sync::Mutex::new(None)),
        }
    }

    /// 这一段思考到此为止：报一条耗时，读的那一侧把它盖到刚才那一步上。
    ///
    /// 别的事件一来就调它（正文、准备、工具）——那正是"想完了"的信号。
    fn seal_reasoning(&self) {
        let Ok(mut since) = self.reasoning_since.lock() else {
            return;
        };
        let Some(started) = since.take() else {
            return;
        };
        if !self.enabled || self.tool_mode == ProgressMode::Hidden {
            return;
        }
        self.progress.report(format!(
            "{REASONING_DONE_MARKER}{}",
            started.elapsed().as_millis()
        ));
    }

    pub fn phase(&self, message: impl Into<String>) {
        if self.enabled && self.tool_mode != ProgressMode::Hidden {
            self.progress.report(message.into());
        }
    }

    /// 子代理的思考。**Summary 档也发**：全屏 TUI 点开子代理看到的是它自己的
    /// 时间线，那条时间线就靠这些事件拼出来。不认识这个前缀的客户端一直是
    /// 直接丢掉的，多发不影响它们。
    pub fn reasoning(&self, text: &str) {
        // 空 delta 不发:模型常在步末尾吐一个空 reasoning/content 块,UI 据此会造一个
        // 空的思考/正文块并切断时间线(用户报的「串」:空块把时间线切得七零八落)。
        if text.is_empty() {
            return;
        }
        if self.enabled && self.tool_mode != ProgressMode::Hidden {
            if let Ok(mut since) = self.reasoning_since.lock() {
                since.get_or_insert_with(std::time::Instant::now);
            }
            self.progress
                .report(format!("__subagent_reasoning__{}", text));
        }
    }

    /// 子代理的正文(assistant content)增量:模型在工具轮之间/收尾时说的话。
    /// 只在 Full 档流,和 reasoning 一样;UI 把它渲成子过程时间线里的一段正文,
    /// 中间输出与最终输出都能实时看到(#6:光有 timeline、正文没流出来)。
    pub fn content(&self, text: &str) {
        if text.is_empty() {
            return;
        }
        // 同 `reasoning`：**Summary 档也发**。前台子代理的面板要靠它把"它开始
        // 说话了"这件事表达出来——说话之前那几步该收成一行 `Worked for …`。
        if self.enabled && self.tool_mode != ProgressMode::Hidden {
            self.seal_reasoning();
            self.progress
                .report(format!("__subagent_content__{}", text));
        }
    }

    /// 内层工具的参数开始流了。只报主线也会报的那些（`preparing_phase` 认得的
    /// 慢参数工具），别的一闪就过，报了只是闪一下。
    pub fn tool_preparing(&self, name: &str) {
        if !self.enabled || self.tool_mode == ProgressMode::Hidden {
            return;
        }
        if crate::tools::preparing_phase(name).is_none() {
            return;
        }
        self.seal_reasoning();
        self.progress.report(format!("__subtool_preparing__{name}"));
    }

    pub fn tool_start(&self, step: usize, name: &str, args: &str) {
        if !self.enabled || self.tool_mode == ProgressMode::Hidden {
            return;
        }
        if self.tool_mode == ProgressMode::Summary {
            let subject = crate::tools::tool_subject(name, args)
                .map(|subject| format!(" · {subject}"))
                .unwrap_or_default();
            self.progress.report(if is_zh() {
                format!("工具 #{step}：{}{subject} 运行中", readable_tool_name(name))
            } else {
                format!(
                    "tool #{step}: {}{subject} running",
                    readable_tool_name(name)
                )
            });
        }
    }

    /// 子代理开始调一个工具。**每个档都发、每个工具都发**：面板要靠它给每一步
    /// 掐表、露出「正在跑」那一行，内层事件本身不带耗时。
    ///
    /// 原来 Full 档下 `run_command` 不发（inline 那边由 `tool_end` 整块画，先发
    /// 一次会画两遍）。可是 REPL 常驻连接不带 origin tty，daemon 把它当网页回合
    /// 一律用 Full 档——于是全屏面板里最常见的那个工具从来没有「运行中」，只剩
    /// 「准备执行」一直挂到结果回来（用户实测截图）。画两遍的事让渲染那边自己
    /// 躲：Full 档 inline 收到这条时命令块不画，等结果整块画。
    pub fn tool_call_detail(&self, name: &str, args: &str) {
        if !self.enabled || self.tool_mode == ProgressMode::Hidden {
            return;
        }
        self.seal_reasoning();
        if let Ok(mut since) = self.tool_since.lock() {
            *since = Some(std::time::Instant::now());
        }
        self.progress.report(format!(
            "__subtool_call__{}",
            json!({
                "name": name,
                "display": readable_tool_name(name),
                "args": clip_detail(args),
            })
        ));
    }

    pub fn tool_end(&self, step: usize, name: &str, args: &str, ok: bool, output: &str) {
        if !self.enabled || self.tool_mode == ProgressMode::Hidden {
            return;
        }
        if self.tool_mode == ProgressMode::Summary {
            let subject = crate::tools::tool_subject(name, args)
                .map(|subject| format!(" · {subject}"))
                .unwrap_or_default();
            let status = if ok { "ok" } else { "err" };
            self.progress.report(if is_zh() {
                format!(
                    "工具 #{step}：{}{subject} {status}",
                    readable_tool_name(name)
                )
            } else {
                format!(
                    "tool #{step}: {}{subject} {status}",
                    readable_tool_name(name)
                )
            });
        }
        // 同 `reasoning`：Summary 档也发，全屏 TUI 的子代理面板要靠它。
        // 输出截断——这是给人看一眼的，不是把 IPC 当日志管道。
        // 耗时跟着结果一起报：标记流里没有时间戳，读那侧只能靠这个数。
        let millis = self
            .tool_since
            .lock()
            .ok()
            .and_then(|mut since| since.take())
            .map(|at| at.elapsed().as_millis());
        self.progress.report(format!(
            "__subtool_result__{}",
            json!({
                "name": name,
                "display": readable_tool_name(name),
                "args": args,
                "ok": ok,
                "ms": millis,
                "output": clip_detail(output),
            })
        ));
    }
}

/// 内层事件里单条输出最多带多少字节。
const MAX_DETAIL_BYTES: usize = 8 * 1024;

fn clip_detail(text: &str) -> String {
    if text.len() <= MAX_DETAIL_BYTES {
        return text.to_string();
    }
    let mut end = MAX_DETAIL_BYTES;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n…", &text[..end])
}

#[derive(Default)]
pub struct SubagentStats {
    pub tool_calls: usize,
    pub tool_ok: usize,
    pub tool_errors: usize,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    /// How much of `prompt_tokens` the provider served from cache. Tracked so
    /// subagent usage can join the session's cumulative cache rate without
    /// diluting it: folding the prompt into the denominator while dropping the
    /// hits would make a healthy cache look broken.
    pub cache_read_tokens: u64,
    pub token_estimate: u64,
    pub token_estimate_method: TokenEstimateMethod,
    pub budget_reached: bool,
}

#[derive(Clone, Copy, Default, Eq, PartialEq)]
pub enum TokenEstimateMethod {
    #[default]
    None,
    ProviderUsage,
    ProviderUsagePlusEstimate,
    RoughCharEstimate,
}

impl SubagentStats {
    pub fn add_usage_or_estimate(&mut self, usage: Option<&Usage>, texts: &[&str]) {
        if let Some(usage) = usage {
            let total_tokens = usage.effective_total_tokens();
            if total_tokens > 0 {
                self.prompt_tokens += usage.prompt_tokens;
                self.completion_tokens += usage.completion_tokens;
                self.cache_read_tokens += usage.cache_read_tokens;
                self.total_tokens += total_tokens;
                self.token_estimate += total_tokens;
                self.token_estimate_method = match self.token_estimate_method {
                    TokenEstimateMethod::None | TokenEstimateMethod::ProviderUsage => {
                        TokenEstimateMethod::ProviderUsage
                    }
                    _ => TokenEstimateMethod::ProviderUsagePlusEstimate,
                };
                return;
            }
        }
        let estimate = estimate_tokens(texts);
        self.token_estimate += estimate;
        self.token_estimate_method = match self.token_estimate_method {
            TokenEstimateMethod::None | TokenEstimateMethod::RoughCharEstimate => {
                TokenEstimateMethod::RoughCharEstimate
            }
            _ => TokenEstimateMethod::ProviderUsagePlusEstimate,
        };
    }

    pub fn public(&self) -> Value {
        json!({
            "tool_calls": self.tool_calls,
            "tool_ok": self.tool_ok,
            "tool_errors": self.tool_errors,
            "prompt_tokens": self.prompt_tokens,
            "completion_tokens": self.completion_tokens,
            "total_tokens": self.total_tokens,
            "cache_read_tokens": self.cache_read_tokens,
            "token_estimate": self.token_estimate,
            "token_estimate_method": token_estimate_method_label(self.token_estimate_method),
            "token_estimate_is_actual": self.token_estimate_method == TokenEstimateMethod::ProviderUsage,
        })
    }
}

pub fn token_estimate_method_label(method: TokenEstimateMethod) -> &'static str {
    match method {
        TokenEstimateMethod::ProviderUsage => "provider_usage",
        TokenEstimateMethod::ProviderUsagePlusEstimate => "provider_usage_plus_estimate",
        TokenEstimateMethod::RoughCharEstimate | TokenEstimateMethod::None => "rough_char_estimate",
    }
}

pub fn estimate_tokens(texts: &[&str]) -> u64 {
    let combined: String = texts.iter().copied().collect();
    if combined.is_empty() {
        0
    } else {
        crate::agent::overflow::estimate_tokens(&combined) as u64
    }
}

/// 一次子代理跑到此刻的量：`(词元数, 人话)`。
fn stats_text(stats: &SubagentStats) -> (String, String) {
    // 与 stats JSON 的 token_estimate_is_actual 同口径:估算值必须带
    // `≈` 前缀,硬编码 false 会把估算按精确值展示。
    let estimated = stats.token_estimate_method != TokenEstimateMethod::ProviderUsage;
    let tokens = format_token_count(stats.token_estimate, estimated);
    let text = if is_zh() {
        format!("工具调用 {} 次　消耗词元 {tokens}", stats.tool_calls)
    } else {
        format!("tool calls: {}　token cost: {tokens}", stats.tool_calls)
    };
    (tokens, text)
}

pub fn format_token_count(tokens: u64, estimated: bool) -> String {
    let prefix = if estimated { "≈" } else { "" };
    if tokens >= 1_000_000 {
        format!("{prefix}{:.2}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{prefix}{:.1}K", tokens as f64 / 1_000.0)
    } else {
        format!("{prefix}{tokens}")
    }
}

pub fn finalization_prompt() -> &'static str {
    "<tool_budget_reached>The tool budget is exhausted. Do not request any more tools. Produce the final result based only on the task description above and the tool results already executed; state explicitly where information is missing.</tool_budget_reached>"
}

pub struct SubagentRunner {
    client: OpenAiCompatibleClient,
    system_prompt: String,
    tools: ToolRegistry,
    excluded_tools: Vec<String>,
    max_steps: usize,
    timeout_seconds: u64,
    progress: SubagentProgress,
    /// 每报一次量就把账记到审计会话上。
    ///
    /// 审计会话原来是**跑完才写**的：中途被打断（Ctrl+C、超时、daemon 重启）
    /// 这一趟烧掉的词元就彻底没了，会话累计里查无此事（用户问到的正是这个）。
    /// 落盘是一条按 session_id 的 UPDATE，写的是**累计值**不是增量，重复写
    /// 不会算两遍。
    usage_sink: Option<std::sync::Arc<dyn Fn(&SubagentStats) + Send + Sync>>,
}

impl SubagentRunner {
    pub fn new(
        client: OpenAiCompatibleClient,
        system_prompt: impl Into<String>,
        tools: ToolRegistry,
        progress: SubagentProgress,
    ) -> Self {
        Self {
            client,
            system_prompt: system_prompt.into(),
            tools,
            excluded_tools: Vec::new(),
            max_steps: 0,
            timeout_seconds: 60,
            progress,
            usage_sink: None,
        }
    }

    pub fn max_steps(mut self, n: usize) -> Self {
        self.max_steps = n;
        self
    }

    pub fn timeout_seconds(mut self, s: u64) -> Self {
        self.timeout_seconds = s;
        self
    }

    pub fn excluded_tools(mut self, names: &[&str]) -> Self {
        self.excluded_tools = names.iter().map(|s| s.to_string()).collect();
        self
    }

    pub async fn run_with_resume(
        &self,
        prompt: &str,
        resume_id: Option<&str>,
    ) -> Result<(ChatResult, SubagentStats)> {
        let mut stats = SubagentStats::default();
        let (messages, initial_steps) = match resume_id.map(str::trim).filter(|id| !id.is_empty()) {
            Some(id) => match take_checkpoint(id) {
                Some((mut messages, steps)) => {
                    messages.push(ChatMessage::plain(
                        "user",
                        "The previous connection dropped here. All completed tool results are preserved above; continue the original task from where it stopped without repeating finished steps.",
                    ));
                    (messages, steps)
                }
                None => bail!(
                    "resume_id '{id}' not found or expired (checkpoints persist on disk for 2h; a much older one is gone); re-issue the task without resume_id"
                ),
            },
            None => (
                vec![
                    ChatMessage::system(self.system_prompt.clone()),
                    ChatMessage::plain("user", prompt.to_string()),
                ],
                0,
            ),
        };

        let result = self
            .chat_with_tools(messages, &mut stats, initial_steps)
            .await?;

        // **不要**在这儿再加一次最后一轮的用量：`chat_with_tools` 的循环里每一轮
        // 都已经加过了（包括交卷那一轮），再加就是把它算两遍——一个跑了三十来步
        // 的子代理，末轮的 prompt 里装着整段对话，重复计一次能把总数抬高一大截
        //（用户实测：token 处理有些问题）。
        //
        // 只在**一次都没记上**时用估算兜底：那是供应商压根不报用量的情形。
        if stats.token_estimate_method == TokenEstimateMethod::None {
            stats.add_usage_or_estimate(None, &[&self.system_prompt, prompt, &result.content]);
        }

        self.report_stats(&stats);

        Ok((result, stats))
    }

    fn report_stats(&self, stats: &SubagentStats) {
        let (_, text) = stats_text(stats);
        self.progress.phase(format!("__subagent_stats__{text}"));
    }

    /// 中途报一次量。面板标题和后台任务状态行都靠它。
    ///
    /// 原来只在**跑完**报——而面板恰恰是它跑着的时候才开着的，于是标题上那两个
    /// 数字一路停在开跑时的样子（用户实测：工具调用次数和消耗词元都没正常涨）。
    /// 走单独的前缀是因为这条一秒能来好几次：`__subagent_stats__` 是要落进流水账
    /// 留底的，中途的量报落进去只会把时间线撑满。
    /// 每报一次量就落一次账。见 `usage_sink`。
    pub fn usage_sink(
        mut self,
        sink: std::sync::Arc<dyn Fn(&SubagentStats) + Send + Sync>,
    ) -> Self {
        self.usage_sink = Some(sink);
        self
    }

    fn report_metric(&self, stats: &SubagentStats) {
        if let Some(sink) = &self.usage_sink {
            sink(stats);
        }
        let (tokens, text) = stats_text(stats);
        // 机器读的在前，制表符分隔，人话在后——和内层工具事件一个写法。
        // 三段：给状态行看的那串（带 `≈`）、给会话累计加的那个**数**、人话。
        let raw = stats.token_estimate.max(stats.total_tokens);
        self.progress
            .phase(format!("__subagent_metric__{tokens}\t{raw}\t{text}"));
    }

    /// chat_stream with bounded retries: a mid-stream disconnect re-sends the
    /// same request (cheap thanks to provider prefix caching) instead of
    /// killing the whole subagent run.
    async fn chat_stream_with_retry(
        &self,
        messages: &[ChatMessage],
        definitions: &[miyu_core::llm::ToolDefinition],
        steps: usize,
    ) -> Result<ChatResult> {
        let mut last_err = None;
        for attempt in 0..STREAM_ATTEMPTS {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_secs(2 * attempt as u64)).await;
                self.progress.phase(format!(
                    "__subagent_stats__{}",
                    if is_zh() {
                        format!("连接中断，重试第 {attempt} 次…")
                    } else {
                        format!("stream dropped, retry {attempt}…")
                    }
                ));
            }
            match self
                .client
                .chat_stream(
                    messages.to_vec(),
                    definitions.to_vec(),
                    |chunk: ChatStreamChunk| {
                        match chunk.kind {
                            ChatStreamKind::Reasoning => self.progress.reasoning(&chunk.text),
                            ChatStreamKind::Content => self.progress.content(&chunk.text),
                            // 工具名已解码、参数还在流：主线在这个窗口报「准备xx」，
                            // 面板里也该有（用户实测：浮层中没有「准备xx」）。
                            ChatStreamKind::ToolCall => self.progress.tool_preparing(&chunk.text),
                            _ => {}
                        }
                        Ok(())
                    },
                )
                .await
            {
                Ok(result) => return Ok(result),
                Err(err) => last_err = Some(err),
            }
        }
        let err = last_err.expect("at least one attempt");
        // All retries failed: freeze the live history so the model can resume
        // this subagent instead of restarting it from scratch.
        let resume_id = store_checkpoint(messages.to_vec(), steps);
        bail!(
            "subagent stream failed after {STREAM_ATTEMPTS} attempts: {err}; resume_id=\"{resume_id}\" — call the subagent tool again with this resume_id to continue from the last completed tool round (persisted on disk, survives a daemon restart, kept 2h)"
        );
    }

    async fn chat_with_tools(
        &self,
        mut messages: Vec<ChatMessage>,
        stats: &mut SubagentStats,
        initial_steps: usize,
    ) -> Result<ChatResult> {
        let excluded: Vec<&str> = self.excluded_tools.iter().map(String::as_str).collect();
        let definitions = self.tools.definitions_except(&excluded);
        let mut steps = initial_steps;

        loop {
            if self.max_steps > 0 && steps >= self.max_steps {
                stats.budget_reached = true;
                messages.push(ChatMessage::plain("user", finalization_prompt()));
                let result = self.chat_stream_with_retry(&messages, &[], steps).await?;
                stats.add_usage_or_estimate(result.usage.as_ref(), &[&result.content]);
                return Ok(result);
            }

            let result = self
                .chat_stream_with_retry(&messages, &definitions, steps)
                .await?;
            stats.add_usage_or_estimate(result.usage.as_ref(), &[]);
            // 每步更新一次量:后台子代理任务条那行的 token 消耗据此逐步刷新
            // (09-12 用户要「时间左侧的 token 每步更新」),不再只在收尾时报一次。
            // 走 `__subagent_metric__` 而不是 `__subagent_stats__`：后者是要落进
            // 流水账留底的，中途的量报一秒来好几次，落进去会把时间线撑满。
            self.report_metric(stats);

            if result.tool_calls.is_empty() {
                return Ok(result);
            }

            messages.push(ChatMessage::assistant(
                result.content.clone(),
                Some(result.tool_calls.clone()),
            ));

            for call in result.tool_calls {
                if self.max_steps > 0 && steps >= self.max_steps {
                    messages.push(ChatMessage::tool(
                        call.id,
                        "tool budget reached for this subagent session",
                    ));
                    continue;
                }
                steps += 1;
                stats.tool_calls += 1;

                self.progress
                    .tool_start(steps, &call.function.name, &call.function.arguments);
                self.progress
                    .tool_call_detail(&call.function.name, &call.function.arguments);

                let (output, ok) = match tokio::time::timeout(
                    Duration::from_secs(self.timeout_seconds.max(5)),
                    self.tools
                        .call(&call.function.name, &call.function.arguments),
                )
                .await
                {
                    Ok(Ok(output)) => (output, true),
                    Ok(Err(err)) => (format!("tool error: {err}"), false),
                    Err(_) => (
                        format!(
                            "tool error: {} timed out after {}s",
                            call.function.name, self.timeout_seconds
                        ),
                        false,
                    ),
                };

                if ok {
                    stats.tool_ok += 1;
                } else {
                    stats.tool_errors += 1;
                }
                // 每调完一个就把次数推出去，别等这一轮结束——一轮里并排调五个
                // 工具是常事，那五个跑完之前标题上一直是上一轮的数。
                self.report_metric(stats);

                self.progress.tool_end(
                    steps,
                    &call.function.name,
                    &call.function.arguments,
                    ok,
                    &output,
                );
                messages.push(ChatMessage::tool(call.id, output));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ToolProgressEvent;
    use tokio::sync::mpsc;

    /// Summary 档也要发内层细节：全屏 TUI 点开子代理看到的是它自己的时间线，
    /// 那条线就是用这些事件拼的。不认识这个前缀的客户端一直是直接丢掉的。
    /// （本条曾经断言的是相反的行为，随全屏面板一起改的。）
    #[test]
    fn tool_summary_still_emits_inner_detail_for_panels() {
        let (sender, mut receiver) = mpsc::unbounded_channel();
        let progress =
            SubagentProgress::new(ToolProgress::new(sender), ProgressMode::Summary, true);

        progress.reasoning("detailed reasoning");
        let ToolProgressEvent::Message(message) = receiver.try_recv().unwrap() else {
            panic!("expected reasoning progress message");
        };
        assert_eq!(message, "__subagent_reasoning__detailed reasoning");

        progress.tool_end(1, "grep", r#"{"pattern":"x"}"#, true, "hit");
        // Summary 档的粗粒度状态行照旧
        let ToolProgressEvent::Message(status) = receiver.try_recv().unwrap() else {
            panic!("expected coarse status line");
        };
        assert!(
            !status.starts_with("__"),
            "粗粒度那行不该带内部前缀: {status}"
        );
        // 细节紧跟其后
        let ToolProgressEvent::Message(detail) = receiver.try_recv().unwrap() else {
            panic!("expected inner detail");
        };
        assert!(detail.starts_with("__subtool_result__"), "{detail}");
        assert!(detail.contains("hit"));
    }

    #[test]
    fn hidden_mode_emits_nothing() {
        let (sender, mut receiver) = mpsc::unbounded_channel();
        let progress = SubagentProgress::new(ToolProgress::new(sender), ProgressMode::Hidden, true);
        progress.reasoning("detailed reasoning");
        progress.tool_end(1, "grep", "{}", true, "hit");
        assert!(receiver.try_recv().is_err());
    }

    /// 单条输出不能把 IPC 当日志管道使。
    #[test]
    fn inner_detail_output_is_clipped() {
        let long = "字".repeat(20_000);
        let clipped = clip_detail(&long);
        assert!(clipped.len() <= MAX_DETAIL_BYTES + 8, "{}", clipped.len());
        assert!(clipped.ends_with('…'));
        // 截在字符边界上,不能切出半个字
        assert!(clipped
            .chars()
            .all(|ch| ch == '字' || ch == '…' || ch == '\n'));
    }

    #[test]
    fn tool_full_emits_subagent_reasoning_and_tool_details() {
        let (sender, mut receiver) = mpsc::unbounded_channel();
        let progress = SubagentProgress::new(ToolProgress::new(sender), ProgressMode::Full, true);

        progress.reasoning("detailed reasoning");
        progress.tool_call_detail("grep", r#"{"pattern":"x"}"#);

        let ToolProgressEvent::Message(message) = receiver.try_recv().unwrap() else {
            panic!("expected reasoning progress message");
        };
        assert_eq!(message, "__subagent_reasoning__detailed reasoning");
        // 工具一来就说明刚才那一段想完了：先报一条耗时，读的那一侧把它盖到上一步
        // 思考上（标记流里原来一点时间信息都没有）。
        let ToolProgressEvent::Message(message) = receiver.try_recv().unwrap() else {
            panic!("expected the reasoning-done marker");
        };
        assert!(
            message.starts_with(REASONING_DONE_MARKER),
            "想完了没报耗时: {message:?}"
        );
        let ToolProgressEvent::Message(message) = receiver.try_recv().unwrap() else {
            panic!("expected tool detail progress message");
        };
        assert!(message.starts_with("__subtool_call__"));
        assert!(receiver.try_recv().is_err());
    }

    /// 一段思考只报一次耗时：段收掉之后再来别的事件不该又报一条。
    #[test]
    fn the_reasoning_timer_is_sealed_once() {
        let (sender, mut receiver) = mpsc::unbounded_channel();
        let progress = SubagentProgress::new(ToolProgress::new(sender), ProgressMode::Full, true);

        progress.reasoning("想一句");
        progress.content("说一句");
        progress.content("再说一句");

        let mut markers = Vec::new();
        while let Ok(ToolProgressEvent::Message(message)) = receiver.try_recv() {
            markers.push(message);
        }
        assert_eq!(
            markers
                .iter()
                .filter(|message| message.starts_with(REASONING_DONE_MARKER))
                .count(),
            1,
            "耗时报了不止一次: {markers:?}"
        );
    }
}
