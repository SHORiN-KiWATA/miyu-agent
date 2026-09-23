//! CodeBuddy CLI 中转协议(`protocol = "codebuddy"`)。
//!
//! 传输层不是 HTTP,是本机 `codebuddy` 子进程的 stream-json 双向流:CLI 用用户
//! 既有的腾讯登录态(`apiKeySource: copilot.tencent.com`),Miyu 不经手任何凭据。
//! 工具循环的所有权在 CLI 侧,Miyu 的工具经 `miyu mcp-serve` 桥挂进去,所以这条
//! 线对 Miyu 的回合循环呈现为「一次请求、纯文本(+思考)回来、永远没有
//! tool_calls」——与 claude-code 完全同构。
//!
//! **事件解析整条复用 `claude_code::stream`**。09-20 对着真 CLI 实测,
//! CodeBuddy 是 Claude Code 的分叉,stream-json 逐字段一致:
//!
//! ```text
//! {"type":"system","subtype":"init","session_id":…,"tools":[…],"model":…}
//! {"type":"stream_event","event":{"type":"content_block_delta",
//!                                 "delta":{"type":"thinking_delta",…}}}
//! {"type":"assistant","message":{"content":[{"type":"text",…}],"usage":{…}}}
//! {"type":"result","subtype":"success","result":…,"usage":{…}}
//! ```
//!
//! 连 `usage` 的字段名(`cache_creation_input_tokens` 等)都一样。抄一份 560 行
//! 的解析器只会让两边漂,所以 `run_claude_turn` 改成收一个 [`RelayLaunch`]
//! (二进制/空闲超时/标签/找不到时那句话),两条线各传各的。
//!
//! 独立成一档而不是给 claude-code 加个开关(用户 09-20 拍板):二进制、配置块、
//! 模型清单各是各的,而且下面这两处命令行差异是真实存在的——
//!
//! - **没有 `--effort`**:CodeBuddy 的 `-h` 里没有这个参数,思考档无处可落,
//!   所以 `reasoning_variant_supported_for_protocol` 对这条线一律返回 false。
//! - **没有 `--no-session-persistence`**:一次性回合(辅助请求)只能任由它落盘
//!   转录,清空联动那边按 `--resume` 的会话 id 处理。

use crate::llm::openai_compatible::claude_code::stream::{self, RelayLaunch};
use crate::llm::openai_compatible::cli_relay::{
    self, payload, RelayOutcome, ResumePlan, ToolScopes,
};
use crate::llm::openai_compatible::*;

/// 客户端构造期解析好的运行时参数,端点间共享。
#[derive(Clone)]
pub(in crate::llm::openai_compatible) struct CodeBuddyRuntime {
    pub(in crate::llm::openai_compatible) binary: PathBuf,
    /// CodeBuddy 原生工具(Bash/Edit/Read…)的模式作用域:off/dev/normal/all。
    pub(in crate::llm::openai_compatible) native_tools: String,
    /// Miyu 工具经 MCP 桥挂进去的模式作用域:off/dev/normal/all。
    pub(in crate::llm::openai_compatible) miyu_tools: String,
    /// 原生工具开启时的 --permission-mode(无头模式没有交互审批)。
    pub(in crate::llm::openai_compatible) permission_mode: String,
    pub(in crate::llm::openai_compatible) idle_timeout: Duration,
}

impl CodeBuddyRuntime {
    pub(in crate::llm::openai_compatible) fn from_config(config: &AppConfig) -> Self {
        let plugin = &config.plugins.codebuddy;
        // PATH 上没有时去常见安装目录找(09-23 macOS:claude 在 ~/.local/bin、codex 在 /opt/homebrew/bin)。
        let binary = miyu_base::paths::configured_program(&plugin.binary, "codebuddy");
        Self {
            binary,
            native_tools: plugin.native_tools.clone(),
            miyu_tools: plugin.miyu_tools.clone(),
            permission_mode: plugin.permission_mode.clone(),
            idle_timeout: Duration::from_secs(plugin.idle_timeout_seconds.max(30)),
        }
    }
}

impl OpenAiCompatibleClient {
    pub(crate) async fn chat_codebuddy_stream<F>(
        &self,
        messages: Vec<ChatMessage>,
        _tools: Vec<ToolDefinition>,
        request_id: &str,
        on_chunk: &mut F,
    ) -> Result<ChatResult>
    where
        F: FnMut(ChatStreamChunk) -> Result<()>,
    {
        let runtime = self
            .codebuddy
            .clone()
            .context("codebuddy runtime was not initialized for this client")?;
        let model = self.provider.default_model.clone();
        let (system_prompt, conversation) = payload::split_system(messages);
        let workdir = miyu_base::workspace::effective_workdir();
        let miyu_session = miyu_base::workspace::try_session();
        let miyu_session = miyu_session.as_deref();
        // 续传按工具面档位隔离:桥每轮按触发者身份重算工具面,两档共用一条
        // CLI 会话会让清单逐轮增删,模型读成「工具掉线」(见 session 模块头)。
        let host_tools = cli_relay::host_tools_face(miyu_session);
        let restrictions = cli_relay::turn_restrictions(miyu_session);
        let scopes = cli_relay::tool_scopes(
            self.request_scope,
            &runtime.native_tools,
            &runtime.miyu_tools,
            self.claude_code_dev_mode,
            &restrictions,
        );
        let prompt = cli_relay::compose_prompt(
            &system_prompt,
            scopes,
            RELAY_ENVIRONMENT_NOTE,
            RELAY_MIYU_TOOLS_NOTE,
        );
        let mut plan = ResumePlan::new(
            &self.provider.id,
            &model,
            &prompt,
            conversation,
            self.request_scope,
            miyu_session,
            host_tools,
            &restrictions,
        );
        let mut outcome = self
            .codebuddy_turn(
                &runtime, &model, &workdir, &prompt, scopes, &plan, request_id, on_chunk,
            )
            .await;
        if let Err(error) = &outcome {
            if plan.resume_id().is_some() && stream::resume_session_lost(error) {
                plan.resume_lost("codebuddy", request_id, error);
                outcome = self
                    .codebuddy_turn(
                        &runtime, &model, &workdir, &prompt, scopes, &plan, request_id, on_chunk,
                    )
                    .await;
            }
        }
        let outcome = outcome?;
        plan.record(&outcome);
        Ok(outcome.result)
    }

    #[allow(clippy::too_many_arguments)]
    async fn codebuddy_turn<F>(
        &self,
        runtime: &CodeBuddyRuntime,
        model: &str,
        workdir: &std::path::Path,
        prompt: &str,
        scopes: ToolScopes,
        plan: &ResumePlan,
        request_id: &str,
        on_chunk: &mut F,
    ) -> Result<RelayOutcome>
    where
        F: FnMut(ChatStreamChunk) -> Result<()>,
    {
        let payload = payload::render_user_payload(plan.delta(), STDIN_BYTE_BUDGET);
        let args = self.codebuddy_args(runtime, model, prompt, scopes, plan.resume_id());
        crate::llm::request_log::record(
            &self.provider.id,
            model,
            "codebuddy",
            self.request_scope,
            &runtime.binary.display().to_string(),
            &json!({ "args": args, "stdin": payload, "conversation": plan.conversation() }),
        );
        let launch = RelayLaunch {
            binary: &runtime.binary,
            idle_timeout: runtime.idle_timeout,
            // CodeBuddy 走腾讯登录态,与 ANTHROPIC_* 无关,不动用户环境。
            strip_anthropic_keys: false,
            label: "codebuddy",
            missing_hint: || {
                t(
                    "CodeBuddy CLI not found; install it or set plugins.codebuddy.binary",
                    "找不到 CodeBuddy CLI;请安装它或配置 plugins.codebuddy.binary",
                )
                .to_string()
            },
        };
        stream::run_claude_turn(&launch, workdir, &args, &payload, request_id, on_chunk).await
    }

    fn codebuddy_args(
        &self,
        runtime: &CodeBuddyRuntime,
        model: &str,
        prompt: &str,
        scopes: ToolScopes,
        resume: Option<&str>,
    ) -> Vec<String> {
        let mut args: Vec<String> = [
            "-p",
            "--verbose",
            "--output-format",
            "stream-json",
            "--input-format",
            "stream-json",
            "--include-partial-messages",
        ]
        .into_iter()
        .map(str::to_string)
        .collect();
        args.push("--model".into());
        args.push(model.to_string());
        // 没有 `--effort`:CodeBuddy 的 CLI 不认这个参数(09-20 核过 `-h`),
        // 思考档在 `reasoning_variant_supported_for_protocol` 那里就被挡住了。
        if !prompt.trim().is_empty() {
            // 整体替换默认系统提示词:人格/开发提示词原样过去,同时甩掉 CLI
            // 自带的身份与仓库规则注入。
            args.push("--system-prompt".into());
            args.push(prompt.to_string());
        }
        if scopes.native_on {
            args.push("--permission-mode".into());
            args.push(runtime.permission_mode.clone());
        } else {
            args.push("--tools".into());
            args.push(String::new());
        }
        args.push("--strict-mcp-config".into());
        if scopes.miyu_on {
            if let Some(mcp_config) =
                crate::llm::openai_compatible::claude_code::mcp_bridge_config(scopes.native_on)
            {
                args.push("--mcp-config".into());
                args.push(mcp_config);
                args.push("--allowedTools".into());
                args.push("mcp__miyu".into());
            }
        }
        if let Some(resume) = resume {
            args.push("--resume".into());
            args.push(resume.to_string());
        }
        // 没有 `--no-session-persistence`:一次性回合也只能让它落盘转录。
        args
    }
}

/// CodeBuddy 单条 stream-json user 输入的静默截断上限**未实测**,与 claude-code
/// 同样不给预算:量出来之前不改这条线的行为。
const STDIN_BYTE_BUDGET: Option<usize> = None;

const RELAY_ENVIRONMENT_NOTE: &str = "\n\n<relay-environment>\nThis session runs inside Miyu's relay: each turn is a fresh CLI process that exits when the turn ends. Work backgrounded through the built-in tools dies with the process, and its completion notifications never arrive.\n</relay-environment>";

const RELAY_MIYU_TOOLS_NOTE: &str = "\n<relay-environment-tools>\nThe mcp__miyu__ tools live in the persistent Miyu daemon and survive across turns: mcp__miyu__subagent runs a background subagent that wakes a follow-up turn when it finishes, mcp__miyu__job inspects or stops those, and mcp__miyu__alarm schedules timed reminders.\n</relay-environment-tools>";
