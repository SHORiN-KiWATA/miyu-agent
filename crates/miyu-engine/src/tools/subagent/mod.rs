use super::subagent_runner::{ProgressMode, SubagentProgress, SubagentRunner, SubagentStats};
use super::{ToolRegistry, ToolSpec};
use anyhow::{bail, Context as _, Result};
use miyu_base::config::PersonaLane;
use miyu_base::config::{AppConfig, ModelTier};
use miyu_base::host_ports::{
    ChildOutcome, ContinueChildRequest, SpawnChildRequest, SubagentHostPort, SubagentProgressSink,
};
use miyu_base::paths::MiyuPaths;
use miyu_core::llm::OpenAiCompatibleClient;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

mod audit;
mod background;
mod log;
/// 过程协议（标签清单、行解析、标记解析）。格式由**写的这一侧**定，读的那一侧
/// （后台面板）对着同一份。
pub mod protocol;
/// 前台子代理的进度收成状态行要的三样（会话项目第 4 段之二）。
pub mod status;

use self::audit::*;
use self::background::run_mirrored_child;
pub use self::background::{reattach_background_child, ReattachChild};
use self::log::*;

const SUBAGENT_SYSTEM_PROMPT: &str = include_str!("../../../../../src/prompts/subagent-general.md");

/// 会话化的子代理(09-18)在标记流开头报自己的子会话 id。`tool_report` 把它落进
/// `ToolFlowCall.child_session_id`,前端据此把状态行链到那条会话;老渲染层不认识
/// 这个前缀,照旧丢掉。
pub const SUBAGENT_SESSION_MARKER: &str = "__subagent_session__";

/// 子代理树深度上限(用户 09-18 拍板写死):0 主会话、1 子代理、2 孙代理;孙代理面上
/// 没有 subagent 工具,这里是第二道闸。
const MAX_SUBAGENT_DEPTH: u32 = 2;

/// 后台子代理的镜像任务 id → 子会话 id。模型从 `background=true` 的返回里拿到的是
/// job_id,给它追话(`send_subagent_message` / `subagent(session_id=…)`)时两种 id 都认。
fn background_children() -> &'static Mutex<HashMap<String, String>> {
    static MAP: std::sync::OnceLock<Mutex<HashMap<String, String>>> = std::sync::OnceLock::new();
    MAP.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 这个子会话是哪个后台任务的镜像（后台子代理才有）。任务条按它把子会话那一行和
/// 同一件事的后台任务行合成一行（会话项目第 3 段）。
pub fn background_job_of(session_id: &str) -> Option<String> {
    background_children()
        .lock()
        .unwrap()
        .iter()
        .find_map(|(job_id, child)| (child == session_id).then(|| job_id.clone()))
}

/// 这个后台任务镜像的是哪条子会话（子会话还没建好就没有）。
pub fn child_session_of_job(job_id: &str) -> Option<String> {
    background_children().lock().unwrap().get(job_id).cloned()
}

fn resolve_child_session(id: &str) -> String {
    background_children()
        .lock()
        .unwrap()
        .get(id)
        .cloned()
        .unwrap_or_else(|| id.to_string())
}

/// 前台子代理调用 → 它的子会话。回合收尾 `derive_tool_flow` 时取走、挂到那一步上
/// （`ToolFlowCall.child_session_id`），前端据它把状态行 / 卡片链到那条会话。
///
/// 原来这里暂存的是整条标记流（`sub_trace`），网页回看时在父会话里把子代理的过程再画
/// 一遍。子代理 09-18 起是一条会话，过程在它自己那里，这里只留会话 id（会话项目第 4 段
/// 之二）。进程内，取走即清。
fn subagent_sessions() -> &'static Mutex<HashMap<String, String>> {
    static SESSIONS: std::sync::OnceLock<Mutex<HashMap<String, String>>> =
        std::sync::OnceLock::new();
    SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 这次调用的子会话到了。
pub fn record_subagent_session(call_id: &str, session_id: &str) {
    if call_id.is_empty() || session_id.is_empty() {
        return;
    }
    subagent_sessions()
        .lock()
        .unwrap()
        .insert(call_id.to_string(), session_id.to_string());
}

/// 取走这次调用的子会话（回合最终落库时用，取完清掉）。
pub fn take_subagent_session(call_id: &str) -> Option<String> {
    subagent_sessions().lock().unwrap().remove(call_id)
}

/// 只读不清：回合中途的检查点（`checkpoint_tool_flow`）会跑好几次，用 `take` 的话等回合
/// 收尾真正落库时就没了。
pub fn peek_subagent_session(call_id: &str) -> Option<String> {
    subagent_sessions().lock().unwrap().get(call_id).cloned()
}

/// 一条进度是不是子代理子过程标记(据此决定要不要留进 trace)。
pub fn is_subagent_marker(message: &str) -> bool {
    message.starts_with("__subagent") || message.starts_with("__subtool")
}

/// dev 子代理的系统提示词由三段拼成:用户的 dev 提示词(与 dev 会话同一份
/// 真相源)、主机环境块、这一句交付约定。三段都是同一会话内的常量,拼出的
/// 前缀字节稳定,多次 dev 子代理之间照样命中供应商缓存。
///
/// 约定只留一句:主体布置任务时会把目标写进 prompt,但「回话对象是主 agent
/// 而不是用户、没有第二轮」这件事它自己看不出来——dev 提示词里也没有。
const SUBAGENT_DEV_CONTRACT: &str = "Your reply goes back to the agent that delegated this task, not to a user, and there is no second round: finish the work yourself and end with what you did, what the result was, and anything the caller must know.";

/// 子代理不再分类(08-17):任务由主体布置,工具就沿用主体的目录。
/// 原来的 explore 是一份硬白名单(read_file/glob/grep/check_os_info/
/// read_clipboard/web_fetch/web_search),而 dev 目录根本不注册前五个——
/// dev 下的 explore 只剩 web 两件套,描述却还在承诺 7 个工具。分类本身
/// 就是这类漂移的来源,连同 275 字符的 subagent_type 参数一起退场。
///
/// 递归防护保留:这份排除表继续把 subagent、技能创作、闹钟和
/// 娱乐类工具挡在子代理之外。
pub(in crate::tools) const SUBAGENT_EXCLUDED: &[&str] = &[
    "subagent",
    // 09-11 改名前的旧名,按名匹配的排除表留着不花钱。
    "task",
    "task_agent",
    "send_subagent_message",
    "load_skill",
    "manage_skill",
    "alarm",
    "use_meme",
    "manage_meme",
    "generate_image",
    "print_image",
    "search_web_images",
    "divine",
];

/// 会话化子代理(09-18)的工具面排除表:与 [`SUBAGENT_EXCLUDED`] 同一份口径,只是
/// 不摘 subagent 本身——子会话能再开一层,孙代理由场所按深度摘(`web/turns/task.rs`)。
pub const SUBAGENT_SESSION_EXCLUDED: &[&str] = &[
    // 快照之后才注册,老循环本来就拿不到;会话化的子代理走的是完整工具面,得在这摘。
    crate::tools::cross_session::TOOL_NAME,
    "load_skill",
    "manage_skill",
    "alarm",
    "use_meme",
    "manage_meme",
    "generate_image",
    "print_image",
    "search_web_images",
    "divine",
];

const SUBAGENT_TOOL_TIMEOUT: u64 = 120;

#[derive(Clone)]
struct SubagentContext {
    config: AppConfig,
    paths: MiyuPaths,
    tools: ToolRegistry,
}

pub fn register(
    registry: &mut ToolRegistry,
    config: AppConfig,
    paths: MiyuPaths,
    tools: ToolRegistry,
) {
    let context = SubagentContext {
        config,
        paths,
        tools,
    };
    registry.register(ToolSpec::new_with_progress(
        "subagent",
        "Launch a subagent to handle a complex task independently. The subagent has its own system prompt, tool set, and LLM loop, and returns its final text to the main agent. Set dev=true for coding work.",
        json!({
            "type": "object",
            "properties": {
                "description": {
                    "type": "string",
                    "description": "Short task description for progress display."
                },
                "prompt": {
                    "type": "string",
                    "description": "Detailed task prompt. Must include full context, goals, and output requirements since the subagent has no access to the main agent's conversation history."
                },
                "dev": {
                    "type": "boolean",
                    "description": "Run the subagent in development mode: the development system prompt plus a minimal coding tool set. Turn it on for every coding task."
                },
                "max_steps": {
                    "type": "integer",
                    "description": "Optional tool-call budget. Unlimited by default: the subagent ends when the task is done. Set a number only when you want a hard cap."
                },
                "background": {
                    "type": "boolean",
                    "description": "Run the subagent detached in the background: returns a job_id immediately; check with job(action=status) (its log holds live progress) and you are woken automatically on completion. Use for long research/tasks that should not block the conversation."
                },
                "session_id": {
                    "type": "string",
                    "description": "Optional. Continue an existing subagent session (the session id from a previous result, or the job_id of a background subagent): if it is still running your prompt is queued into it as a follow-up; otherwise it starts a new turn there with your prompt. Use it to steer a running subagent or to resume one that was interrupted."
                },
                "resume_id": {
                    "type": "string",
                    "description": "Optional. When a previous task failed with a resume_id in its error, pass it here to continue that subagent from its last completed tool round instead of starting over (checkpoints persist on disk and survive a daemon restart, kept 2h)."
                },
                "tier": {
                    "type": "string",
                    "enum": ["lite", "cheap", "standard", "flagship"],
                    "description": "Optional model tier by task difficulty: lite for trivial lookups and formatting, cheap for simple tool-using work, standard for regular multi-step work (default), flagship for hard reasoning. Every tier has the full tool set; an unconfigured tier falls back to the main model."
                }
            },
            "required": ["description", "prompt"],
            "additionalProperties": false
        }),
        move |args, progress| {
            let context = context.clone();
            async move { run_subagent(args, context, progress).await }
        },
    )
    .writes()
    .concurrent());
    // resume_id 只有进程内老循环读得到(`run_subagent`:端口在场就整个走
    // `run_via_host`,那条路从不碰它)。daemon 里它是模型看得见、却永远不会
    // 生效的一个参数——续接子代理在会话化之后走 session_id。端口在 daemon
    // 启动时一次装好、此后不变,所以这条分叉按进程形态定,同一进程内 tools
    // 数组字节恒定(AGENTS §1.1)。
    if miyu_base::host_ports::subagent_port().is_some() {
        registry.remove_parameter("subagent", "resume_id");
    }

    // 给开过的子代理追话:跑着就排进它这一轮(下一步开始前取走),跑完了就在它的
    // 子会话里另起一轮后台跑(09-18 会话化)。描述 09-23 按这个行为重写:原来还写着
    // 改名前的 task(background=true)、「只能发给还在跑的」。
    registry.register(ToolSpec::new_with_progress(
        "send_subagent_message",
        "Send a follow-up to a subagent you started, by its job_id or session id. A running subagent reads it before its next step. A finished one starts a new background turn with it, and you are woken when that turn ends.",
        json!({
            "type": "object",
            "properties": {
                "job_id": {
                    "type": "string",
                    "description": "The job_id or session id from the subagent result."
                },
                "message": {
                    "type": "string",
                    "description": "The follow-up message."
                }
            },
            "required": ["job_id", "message"],
            "additionalProperties": false
        }),
        move |args, progress| async move { send_subagent_message(args, progress).await },
    ));
}

async fn send_subagent_message(
    args: Value,
    progress: crate::tools::ToolProgress,
) -> Result<String> {
    let job_id = args
        .get("job_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    let message = args
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    if job_id.is_empty() {
        bail!("job_id is required (the background subagent's id from the task result)");
    }
    if message.is_empty() {
        bail!("message is required");
    }
    // 会话化(09-18):追话 = 往子会话排 follow-up(跑着)或起新一轮(闲着,后台等)。
    if let Some(port) = miyu_base::host_ports::subagent_port() {
        let params = SubagentParams {
            description: format!(
                "follow-up · {}",
                message.chars().take(24).collect::<String>()
            ),
            prompt: message,
            session_id: Some(resolve_child_session(&job_id)),
            resume_id: None,
            max_steps: 0,
            tier: ModelTier::Standard,
            dev: false,
        };
        return run_via_host(port, params, true, progress).await;
    }
    if crate::tools::subagent_runner::deliver_to_subagent(&job_id, &message) {
        Ok(serde_json::to_string_pretty(&json!({
            "ok": true,
            "job_id": job_id,
            "queued": message,
            "note": "The subagent will incorporate this before its next step."
        }))?)
    } else {
        let running = crate::tools::subagent_runner::running_subagent_ids();
        let hint = if running.is_empty() {
            "no background subagent is running right now".to_string()
        } else {
            format!("running background subagents: {}", running.join(", "))
        };
        bail!(
            "no running background subagent with job_id '{job_id}' (it may have already finished). {hint}"
        )
    }
}

#[derive(Clone)]
struct SubagentParams {
    description: String,
    prompt: String,
    /// 续接已有子会话(会话化,09-18);老循环不认。
    session_id: Option<String>,
    resume_id: Option<String>,
    max_steps: usize,
    tier: ModelTier,
    dev: bool,
}

/// Session linkage captured while still inside the turn scope — a detached
/// background subagent loses the task-locals, so the audit anchor must be
/// resolved before spawning.
#[derive(Clone)]
struct AuditAnchor {
    parent: Option<String>,
    persona: String,
}

fn parse_params(args: &Value) -> Result<SubagentParams> {
    let description = args
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    if description.is_empty() {
        bail!("description is required");
    }
    let prompt = args
        .get("prompt")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    if prompt.is_empty() {
        bail!("prompt is required");
    }
    let resume_id = args
        .get("resume_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string);
    let session_id = args
        .get("session_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string);
    // 0 = 不限步数(runner 语义):默认让子代理自然结束,预算仅在调用方
    // 显式给出 max_steps 时生效。
    let max_steps = args
        .get("max_steps")
        .and_then(Value::as_u64)
        .map(|v| v as usize)
        .unwrap_or(0);
    let tier = args
        .get("tier")
        .and_then(Value::as_str)
        .and_then(ModelTier::from_str)
        .unwrap_or(ModelTier::Standard);
    let dev = args.get("dev").and_then(Value::as_bool).unwrap_or(false);
    Ok(SubagentParams {
        description,
        prompt,
        session_id,
        resume_id,
        max_steps,
        tier,
        dev,
    })
}

async fn run_subagent(
    args: Value,
    context: SubagentContext,
    progress: crate::tools::ToolProgress,
) -> Result<String> {
    let params = parse_params(&args)?;
    let background = args
        .get("background")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    // 会话化(09-18):daemon 里子代理是一条真会话——建会话、起回合、等任务终态都在
    // 场所层(`web::subagent_host`),这里只剩把结果整理成工具输出。
    if let Some(port) = miyu_base::host_ports::subagent_port() {
        return run_via_host(port, params, background, progress).await;
    }
    // daemon 之外(REPL 直连、`miyu tool-call`)没人装端口:沿用进程内的老循环。
    if params.session_id.is_some() {
        bail!("session_id continuation needs the daemon; start a fresh subagent instead");
    }
    let anchor = AuditAnchor {
        parent: miyu_base::workspace::try_session().map(|session| session.to_string()),
        persona: context.config.active_persona_scope(),
    };
    if background {
        return spawn_background(context, params, anchor, progress).await;
    }
    // 前台子代理阻塞在本次调用里,主体无从中途插话,不开收件箱(None)。
    Ok(run_core(context, progress, params, anchor, None)
        .await?
        .output)
}

fn progress_sink(progress: crate::tools::ToolProgress) -> SubagentProgressSink {
    Arc::new(move |message| progress.report(message))
}

/// 会话化路径(09-18):前台就在本次调用里等子会话的任务终态;后台包进后台任务
/// 注册表的镜像任务里等——任务条、`job(action=stop)`、完成唤醒全走后台命令那一套,
/// 唤醒报告里带的是子会话最后一轮的正文。父回合被停时前台那条 future 被 drop,
/// 宿主端口的 Drop 守卫会顺手取消子会话的活动回合(级联到孙代理)。
async fn run_via_host(
    port: Arc<dyn SubagentHostPort>,
    params: SubagentParams,
    background: bool,
    progress: crate::tools::ToolProgress,
) -> Result<String> {
    let depth = miyu_base::workspace::current_subagent_depth();
    if depth >= MAX_SUBAGENT_DEPTH {
        bail!(
            "subagent depth limit reached (this is already a depth-{depth} subagent): \
             do the work yourself instead of delegating further"
        );
    }
    let parent = miyu_base::workspace::try_session()
        .map(|session| session.to_string())
        .context("subagent needs a session to attach to")?;
    let workdir = miyu_base::workspace::try_workspace();
    // 开发模式的会话开的子代理不管传没传 dev 都是开发模式(人格跟父):标签按实际来。
    let dev =
        params.dev || miyu_base::workspace::current_turn_lane().is_some_and(|lane| lane.is_dev());
    let child = params.session_id.as_deref().map(resolve_child_session);
    // 跑着的子会话:话排进它当前那一轮,立刻返回,不管前后台。
    if let Some(child) = child.as_deref().filter(|child| port.is_running(child)) {
        let outcome = port
            .continue_child(ContinueChildRequest {
                parent_session: parent,
                child_session: child.to_string(),
                message: params.prompt,
                workdir,
                progress: progress_sink(progress),
            })
            .await?;
        return format_child_outcome(&params.description, params.tier, outcome);
    }
    if background {
        let description = params.description.clone();
        let prompt = params.prompt.clone();
        return crate::tools::jobs::spawn_background_subagent(
            None,
            &description,
            dev,
            &progress,
            move |job_id, log_path| {
                run_mirrored_child(job_id, log_path, prompt, move |sink| match child {
                    Some(child) => port.continue_child(ContinueChildRequest {
                        parent_session: parent,
                        child_session: child,
                        message: params.prompt,
                        workdir,
                        progress: sink,
                    }),
                    None => port.spawn(SpawnChildRequest {
                        parent_session: parent,
                        description: params.description,
                        prompt: params.prompt,
                        dev,
                        tier: params.tier,
                        background: true,
                        max_steps: params.max_steps,
                        spawned_by_turn: None,
                        workdir,
                        progress: sink,
                    }),
                })
            },
        )
        .await;
    }
    let sink = progress_sink(progress);
    let outcome = match child {
        Some(child) => {
            port.continue_child(ContinueChildRequest {
                parent_session: parent,
                child_session: child,
                message: params.prompt.clone(),
                workdir,
                progress: sink,
            })
            .await?
        }
        None => {
            port.spawn(SpawnChildRequest {
                parent_session: parent,
                description: params.description.clone(),
                prompt: params.prompt.clone(),
                dev,
                tier: params.tier,
                background: false,
                max_steps: params.max_steps,
                spawned_by_turn: None,
                workdir,
                progress: sink,
            })
            .await?
        }
    };
    format_child_outcome(&params.description, params.tier, outcome)
}

/// 会话化子代理的工具输出。成功路径沿用 08-21 的文本形态(`result:` 之后是结论
/// 本体,`tool_report` 按它提取);终态不是 done 时多一句提示,告诉模型这条会话还在、
/// 拿 session_id 能续。
fn format_child_outcome(
    description: &str,
    tier: ModelTier,
    outcome: ChildOutcome,
) -> Result<String> {
    match outcome {
        ChildOutcome::Queued { session_id } => Ok(serde_json::to_string_pretty(&json!({
            "ok": true,
            "kind": "subagent_followup",
            "session_id": session_id,
            "note": "The message was queued into the running subagent; it picks it up before its next step and you are woken when it finishes.",
        }))?),
        ChildOutcome::Finished(result) => {
            let state = match result.state.as_str() {
                "done" => "completed",
                other => other,
            };
            let mut output = format!(
                "subagent {state} (tier {}, session {}): {description}\n",
                tier.label(),
                result.session_id
            );
            output.push_str(&format!(
                "stats: {}\n",
                json!({
                    "turns": result.turns,
                    "total_tokens": result.total_tokens,
                    "provider_id": result.provider_id,
                    "model": result.model,
                })
            ));
            if state != "completed" {
                output.push_str(&format!(
                    "note: the subagent ended in state '{state}'; its session is kept — call subagent again with session_id=\"{}\" to continue it.\n",
                    result.session_id
                ));
            }
            output.push_str("result:\n");
            output.push_str(result.final_text.trim());
            Ok(output)
        }
    }
}

/// 子代理工具结果里的子会话 id：前台跑完是 `subagent <状态> (tier …, session <id>): …`
/// 那一行，追话排进去了是 JSON 的 `session_id`。后台刚派出去时子会话还没建，结果里没有，
/// 返回 `None`。回放没有派出去时报的那条标记，界面靠它把时间线上那一行链到子会话
/// （会话项目第 3 段）。形状由 `format_child_outcome` 定，两边一起改。
pub fn subagent_session_of_output(output: &str) -> Option<String> {
    let output = output.trim_start();
    let id = if output.starts_with('{') {
        let value: Value = serde_json::from_str(output).ok()?;
        value.get("session_id")?.as_str()?.to_string()
    } else {
        let head = output.lines().next()?.strip_prefix("subagent ")?;
        let (_, rest) = head.split_once(", session ")?;
        rest.split(')').next()?.trim().to_string()
    };
    (!id.is_empty()).then_some(id)
}

/// 一次子代理运行的结果。
///
/// `state` 以前是后台路径把 `output` 当 JSON 反解出来的——而 08-21 的
/// token-diet 把成功路径改成了纯文本,那次反解从此永远失败、悄悄退化成
/// "completed",`budget_reached` 被当成正常完成上报。现在直接带出来。
struct SubagentRun {
    output: String,
    state: &'static str,
}

/// 回合作用域(沙盒策略、工作区、会话身份)不跟着 `tokio::spawn` 走:后台
/// 子代理起在一条新任务上,task-local 到那边全是空的。后果不是显示问题
/// ——成员回合的后台子代理会跑在 Landlock 之外,工具的工作目录也退回
/// daemon 的 cwd。在还看得见的地方抓下来,进了后台原样套回去。
async fn with_turn_scope<F>(
    sandbox: Option<std::sync::Arc<miyu_base::sandbox::SandboxPolicy>>,
    workspace: Option<std::path::PathBuf>,
    session: Option<std::sync::Arc<str>>,
    future: F,
) -> F::Output
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    let mut future: std::pin::Pin<Box<dyn std::future::Future<Output = F::Output> + Send>> =
        Box::pin(future);
    if let Some(session) = session {
        future = Box::pin(miyu_base::workspace::with_session(session, future));
    }
    if let Some(workspace) = workspace {
        future = Box::pin(miyu_base::workspace::with_workspace(workspace, future));
    }
    // `None` 也照样套:显式「这一段没有策略」与回合里的语义一致。
    miyu_base::sandbox::with_sandbox(sandbox, future).await
}

/// Detach the subagent run behind the shared background-job registry: its
/// progress streams into the job log, and completion goes through the same
/// wake path as background commands.
async fn spawn_background(
    context: SubagentContext,
    params: SubagentParams,
    anchor: AuditAnchor,
    progress: crate::tools::ToolProgress,
) -> Result<String> {
    let description = params.description.clone();
    let prompt = params.prompt.clone();
    // 后台子代理起在 tokio::spawn 的新任务上,回合的 task-local(工作区/会话/
    // 沙盒)到那儿全空了:相对路径退回 daemon 的 cwd、Landlock 失效、且
    // mcp_bridge_config 因 try_session()=None 返回 None(claude-code 拿不到 Miyu 桥)。
    // 在还处于父回合作用域的此刻抓下来,由 with_turn_scope 在 spawn 里套回去。
    let sandbox = miyu_base::sandbox::current_sandbox();
    let workspace = miyu_base::workspace::try_workspace();
    let session = miyu_base::workspace::try_session();
    crate::tools::jobs::spawn_background_subagent(
        None,
        &description,
        params.dev,
        &progress,
        move |job_id, log_path| async move {
            write_subagent_prompt_header(&log_path, &prompt);
            let bridge = spawn_subagent_log_bridge(job_id.clone(), log_path.clone());
            // 后台子代理:用后台任务 id 作收件箱键,主体可用 send_subagent_message
            // 中途投递 follow-up;主体从后台返回里拿到这个 job_id。工作区/会话/沙盒
            // 由 with_turn_scope 套回(见上)。
            let run = with_turn_scope(
                sandbox,
                workspace,
                session,
                run_core(context, bridge, params, anchor, Some(job_id.clone())),
            )
            .await;
            let state_label = match &run {
                Ok(run) => run.state,
                Err(_) => "error",
            };
            let tail = match &run {
                Ok(run) => format!(
                    "\n{}\n{}\n",
                    crate::tools::jobs::SUBAGENT_RESULT_MARKER,
                    run.output
                ),
                Err(error) => format!("\n{}\n{error}\n", crate::tools::jobs::SUBAGENT_ERROR_MARKER),
            };
            let _ = std::fs::OpenOptions::new()
                .append(true)
                .open(&log_path)
                .and_then(|mut file| {
                    use std::io::Write as _;
                    file.write_all(tail.as_bytes())
                });
            tracing::debug!(job_id = %job_id, state = %state_label, "background subagent finished");
            match state_label {
                "completed" | "budget_reached" => {
                    crate::tools::jobs::JobState::Exited { code: Some(0) }
                }
                "timeout" => crate::tools::jobs::JobState::TimedOut,
                _ => crate::tools::jobs::JobState::Exited { code: None },
            }
        },
    )
    .await
}

/// dev 子代理的系统提示词。
///
/// 第一段是用户自己的 dev 提示词(`dev-prompt.md`,与 dev 会话读同一份),
/// 改它对子代理同时生效;09-24 起默认为空,没写就从环境块开始。第二段是主体也在用的主机环境块——子代理没有
/// 每轮瞬态尾巴,工作目录只能从这里知道,否则第一步永远浪费在 `pwd` 上。
/// 沙盒说明同理(09-23 起不在环境块里了):子代理起跑时抓的那份策略整趟不变,
/// 放这里就是常量。末尾是那句交付约定。
///
/// 三段在一个会话里都是常量(工作目录跟着会话工作区走),多次 dev 子代理
/// 之间前缀缓存照样命中。
fn build_dev_system_prompt(config: &AppConfig, paths: &MiyuPaths) -> Result<String> {
    let mut prompt = config.dev_system_prompt(paths)?;
    crate::agent::prompt::push_block(
        &mut prompt,
        &crate::agent::prompt::host_environment_for(config, paths),
    );
    prompt.push_str(&format!(
        "\n<runtime cwd=\"{}\"/>",
        miyu_base::host_info::xml_attr_escape(
            &miyu_base::workspace::effective_workdir()
                .display()
                .to_string()
        )
    ));
    if let Some(policy) = miyu_base::sandbox::current_sandbox() {
        prompt.push('\n');
        prompt.push_str(&miyu_base::host_info::sandbox_notice(&policy));
    }
    prompt.push_str("\n\n");
    prompt.push_str(SUBAGENT_DEV_CONTRACT);
    Ok(prompt)
}

async fn run_core(
    context: SubagentContext,
    progress: crate::tools::ToolProgress,
    params: SubagentParams,
    anchor: AuditAnchor,
    inbox_id: Option<String>,
) -> Result<SubagentRun> {
    let SubagentParams {
        description,
        prompt,
        session_id: _,
        resume_id,
        max_steps,
        tier,
        dev,
    } = params;
    let tool_timeout = SUBAGENT_TOOL_TIMEOUT;

    // WebUI 回合(既非终端、也非平台:没有 origin tty、没有平台 sender)一律用
    // Full 档发子过程标记(思考 + 结构化工具调用/结果),网页端据此把展开后的
    // 子过程时间线画成「思考+工具流」——和主智能体过程区同款(09-11 用户要求)。
    // 网页端默认收起这些,静息态不吵;终端/平台仍按 display.tool_calls 配置,
    // 免得 Summary 档的终端用户突然被子代理的全量嵌套刷屏。
    let is_webui_turn = miyu_base::workspace::current_origin_tty().is_none()
        && miyu_base::workspace::current_platform_sender().is_none();
    let mode = if is_webui_turn {
        ProgressMode::Full
    } else {
        ProgressMode::from_config(&context.config)
    };
    // 过程回显曾借 deep_research 插件的 show_progress 开关;插件 09-13 删除后没有
    // 独立的子代理插件配置承接它,固定为开。
    let sa_progress = SubagentProgress::new(progress, mode, true);

    // 子过程展开区最上方的任务简介(09-12 #9:后台子代理展开后没有 prompt)。
    // 只在 Full 档(WebUI)发;前台子代理前端从工具参数直接建 brief、并置 sink.brief,
    // 收到这条 marker 会跳过不重复,后台没有参数就靠这条把 prompt 显示出来。
    if mode == ProgressMode::Full {
        sa_progress.phase(format!(
            "__subagent_brief__{}",
            serde_json::json!({ "description": &description, "prompt": &prompt })
        ));
    }

    // dev 子代理 = 开发模式的三件套,与 dev 会话同源:保留人格 "dev" 的
    // 作用域(记忆整套关)、那份 core_only 的工具面、以及中转线的 dev 工具
    // 作用域。少任何一件都会漂移成「名字叫 dev、其实是普通子代理」。
    let config = if dev {
        context.config.dev_scoped()
    } else {
        context.config.clone()
    };

    // Tier routing: the tier's pool gets its own load-balanced client;
    // an unconfigured pool silently uses the main model pool, and a
    // configured-but-unusable pool falls back with a notice returned to
    // the calling agent (not printed to the user). The fallback contract
    // lives in `from_tier` so auxiliary roles share it byte for byte.
    let routed = OpenAiCompatibleClient::from_tier(&config, &context.paths, tier)?;
    let tier_notice = routed.notice;
    let model_choice = routed.model_choice;
    let client = routed
        .client
        .with_request_scope("subagent")
        .with_claude_code_dev_mode(dev)
        .for_subagent_output(mode == ProgressMode::Full);
    // 普通子代理沿用主体目录:任务是主体布置的,分类只会让"承诺的工具"
    // 与"实际注册的工具"漂移(dev 下的旧 explore 就是这么坏掉的)。
    // dev 子代理反过来:它的任务与主体人格无关,拿的就是 dev 会话那张面,
    // 现造而不是注册时造——注册发生在 `compose_registry` 里,在那儿造 dev
    // 面会自己套自己。
    let tools = if dev {
        crate::tools::build_tool_registry(&config, &context.paths, PersonaLane::Dev, false)?
    } else {
        context.tools.clone()
    };

    let system_prompt = if dev {
        build_dev_system_prompt(&config, &context.paths)?
    } else {
        SUBAGENT_SYSTEM_PROMPT.to_string()
    };

    // 审计会话**开跑之前**就建好：它的用量行是会话累计里子代理那一份的来源，
    // 跑完才写的话，中途被打断这一趟烧的词元就彻底没了（用户问到的正是这个）。
    let audit = SubagentAudit::open(&context, &anchor, &description, &prompt);
    let mut runner = SubagentRunner::new(client, system_prompt, tools, sa_progress)
        .max_steps(max_steps)
        .timeout_seconds(tool_timeout)
        .excluded_tools(SUBAGENT_EXCLUDED)
        .inbox_id(inbox_id.clone());
    if let Some(audit) = &audit {
        runner = runner.usage_sink(audit.usage_sink());
    }

    // 后台子代理开收件箱:主体可在运行途中投递 follow-up(见 subagent_runner)。
    // 用 drop guard 关箱,覆盖所有退出路径(正常返回 / `?` 早退 / panic)。
    struct InboxGuard(Option<String>);
    impl Drop for InboxGuard {
        fn drop(&mut self) {
            if let Some(id) = &self.0 {
                crate::tools::subagent_runner::close_subagent_inbox(id);
            }
        }
    }
    if let Some(id) = &inbox_id {
        crate::tools::subagent_runner::open_subagent_inbox(id);
    }
    let _inbox_guard = InboxGuard(inbox_id.clone());

    // 子代理不设总时长上限:它自然结束于任务完成或步数预算;逐工具超时
    // (tool_timeout)仍然兜底单步挂死。
    // 标记「在子代理里」:vision_analyze 据此走旁路转写而非 inline 寄存
    // (子代理循环不接力 inline 媒体,见 workspace::in_subagent)。
    let (result, stats) = match miyu_base::workspace::with_subagent(
        runner.run_with_resume(&prompt, resume_id.as_deref()),
    )
    .await
    {
        Ok((result, stats)) => (result, stats),
        Err(err) => {
            let output = serde_json::to_string_pretty(&json!({
                "ok": false,
                "kind": "subagent",
                "tier": tier.label(),
                "tier_notice": tier_notice,
                "description": description,
                "state": "error",
                "error": err.to_string(),
                "stats": SubagentStats::default().public(),
            }))?;
            match &audit {
                Some(audit) => audit.finish(&context, &output, None, &model_choice),
                None => record_subagent_audit(
                    &context,
                    &anchor,
                    &description,
                    &prompt,
                    &output,
                    None,
                    &model_choice,
                ),
            }
            return Ok(SubagentRun {
                output,
                state: "error",
            });
        }
    };

    let state = if stats.budget_reached {
        "budget_reached"
    } else {
        "completed"
    };

    let final_text = result.content.trim().to_string();

    // 08-21 token-diet:成功路径改文本形态——子代理结论不再被 JSON 转义
    // (换行/引号转义在长结论上是实打实的浪费)。result: 之后到结尾都是
    // 结论本体,tool_report.rs 的持久化提取按此约定解析;错误路径保留
    // ok:false JSON(成败判定的结构即功能)。
    let mut output = format!("subagent {state} (tier {}): {description}\n", tier.label());
    if let Some(notice) = &tier_notice {
        output.push_str(notice);
        output.push('\n');
    }
    output.push_str(&format!(
        "stats: {}\n",
        serde_json::to_string(&stats.public())?
    ));
    output.push_str("result:\n");
    output.push_str(&final_text);
    // Prefer the endpoint that actually produced the final reply (pools
    // load-balance, so the representative pool entry may differ).
    let model_choice = match (&result.provider_id, &result.model) {
        (Some(provider_id), Some(model)) => Some((provider_id.clone(), model.clone())),
        _ => model_choice,
    };
    match &audit {
        Some(audit) => audit.finish(&context, &output, Some(&stats), &model_choice),
        None => record_subagent_audit(
            &context,
            &anchor,
            &description,
            &prompt,
            &output,
            Some(&stats),
            &model_choice,
        ),
    }
    Ok(SubagentRun { output, state })
}

#[cfg(test)]
mod tests;
