//! Antigravity CLI 中转协议(`protocol = "antigravity"`)。
//!
//! 传输层是本机 `agy` 子进程的 stream-json 流,与 claude-code 线同构:CLI 用
//! 用户既有的 Google 登录态,Miyu 不经手凭据;工具循环的所有权在 agy 侧,Miyu
//! 工具经 `miyu mcp-serve` 桥挂进去,回合循环看到的永远是「一次请求、纯文本回
//! 来、没有 tool_calls」。
//!
//! 与 claude-code 的三处实质差异(09-03 实测):
//! ①没有 `--system-prompt`——人格写成全局自定义代理
//! `~/.gemini/config/agents/miyu/agent.md`,正文整体替换 agy 默认指令,
//! `tools:` 白名单同时决定原生工具面(不写只得缩水集;列全 57 件会整轮出错);
//! ②没有 `--mcp-config`——桥只能全局注册在 `~/.gemini/config/mcp_config.json`,
//! 靠 agy 把自己的环境(MIYU_SESSION 等)原样继承给 MCP 子进程按会话分流;
//! ③续传目标丢失**不报错**而是静默新开会话,判据是 init 首行的 id 与请求不符。
//!
//! 作用域裁决、哈希链续传、载荷转写、子进程泵都在 [`cli_relay`]:键本就带
//! provider 维度,种子含系统提示词,「提示词变=新会话全量重放」三线一致。

pub(in crate::llm::openai_compatible) mod pool;
mod setup;
mod stream;

use crate::llm::openai_compatible::cli_relay::{
    self, payload, process::RelayProcess, RelayOutcome, ResumePlan, ToolScopes,
};
use crate::llm::openai_compatible::*;

pub(in crate::llm::openai_compatible) use setup::remove_conversation_files;

/// 供应商在表单里被关掉时的清理:代理目录与全局 mcp_config 里的桥条目。
pub fn remove_relay_files_now() {
    setup::remove_relay_files(&setup::default_config_dir());
}

/// 客户端构造期解析好的运行时参数,端点间共享。
pub(in crate::llm::openai_compatible) struct AntigravityRuntime {
    pub(in crate::llm::openai_compatible) binary: PathBuf,
    /// agy 原生工具的模式作用域:off/dev/normal/all。
    pub(in crate::llm::openai_compatible) native_tools: String,
    /// Miyu 工具经 MCP 桥挂给 agy 的模式作用域:off/dev/normal/all。
    pub(in crate::llm::openai_compatible) miyu_tools: String,
    /// 桥工具按 eager 注册(原生名直调)还是走 agy 的懒加载。
    pub(in crate::llm::openai_compatible) miyu_tools_eager: bool,
    pub(in crate::llm::openai_compatible) idle_timeout: Duration,
    pub(in crate::llm::openai_compatible) print_timeout: Duration,
    /// agy 的用户配置根(`~/.gemini/config`):代理文件与 MCP 注册都落这里。
    /// 测试经 `MIYU_AGY_CONFIG_DIR` 改道,免得碰真实配置。
    pub(in crate::llm::openai_compatible) config_dir: PathBuf,
    /// 同会话连续轮复用进程(见 [`pool`])。
    pub(in crate::llm::openai_compatible) reuse_process: bool,
    /// 常驻进程闲置多久回收。
    pub(in crate::llm::openai_compatible) reuse_idle: Duration,
}

impl AntigravityRuntime {
    pub(in crate::llm::openai_compatible) fn from_config(config: &AppConfig) -> Self {
        let plugin = &config.plugins.antigravity;
        // PATH 上没有时去常见安装目录找(09-23 macOS:claude 在 ~/.local/bin、codex 在 /opt/homebrew/bin)。
        let binary = miyu_base::paths::configured_program(&plugin.binary, "agy");
        Self {
            binary,
            native_tools: plugin.native_tools.clone(),
            miyu_tools: plugin.miyu_tools.clone(),
            miyu_tools_eager: plugin.miyu_tools_eager,
            idle_timeout: Duration::from_secs(plugin.idle_timeout_seconds.max(30)),
            print_timeout: Duration::from_secs(plugin.print_timeout_seconds.max(60)),
            config_dir: setup::default_config_dir(),
            reuse_process: plugin.reuse_process,
            reuse_idle: Duration::from_secs(plugin.reuse_idle_seconds.max(5)),
        }
    }
}

/// 人格代理在 agy 侧的名字前缀:`miyu-<内容哈希>`。一个固定名字不够——代理
/// 文件是全局的,别的会话/辅助请求(不同人格、不带环境事实、`tools: []`)
/// 会在本轮 agy 还没拉起时把它改写掉,agy 启动时读到的就是别人的人格
/// (评审 09-03)。按内容哈希各占一目录,互不相扰;旧目录按 mtime 过期回收。
pub(in crate::llm::openai_compatible) const AGENT_PREFIX: &str = "miyu-";

/// 全局 mcp_config.json 里桥条目的键。
pub(in crate::llm::openai_compatible) const MCP_SERVER_NAME: &str = "miyu";

/// `tools:` 白名单——「原生全开」的实际内容。取自默认代理自报的工具集里
/// **注册表实测认识**的名字(09-03:command_status/wait_5_seconds 不在注册表,
/// 列了会让整轮静默失败;browser_* 系需要浏览器上下文,列了整轮报错)。
/// 故意不列的两件:`ask_question`(无头下必被跳过,不列它模型就只剩桥版
/// `mcp_miyu_ask_question`)与 `generate_image`(原生生图落在 agy 自己的产物
/// 目录,不进 Miyu 的 tool.image 通道,用户看不到)。
pub(in crate::llm::openai_compatible) const NATIVE_TOOLS: &[&str] = &[
    "run_command",
    "view_file",
    "write_to_file",
    "replace_file_content",
    "find_by_name",
    "grep_search",
    "list_dir",
    "read_url_content",
    "search_web",
];

/// 两套工具同开时从桥里剔除的 Miyu 工具(与 agy 原生功能重复,原生在训练
/// 分布内且吃订阅额度,优先)。与 claude 线的差异:agy 没有 todowrite 对应物
/// (`manage_task` 管的是后台任务),所以不剔;`read`/`edit`/`task`/`job`/`alarm`
/// 不剔的理由同 claude 线(kb:/artifact: 域、daemon 常驻后台)。
pub const BRIDGE_DUPLICATE_TOOLS: &[&str] =
    &["run_command", "web_search", "web_fetch", "glob", "grep"];

/// 中转环境事实(声明式,不写指令;常量字节保证提示词哈希稳定)。09-18 起进程
/// 可能跨轮常驻(同会话复用),但随时会被换掉——措辞改成「不保证活过本轮」。
/// 这句一改,提示词哈希变,所有 agy 会话下一轮全量重放一次(只损失效率)。
const RELAY_ENVIRONMENT_NOTE: &str = "\n\n<relay-environment>\nThis session runs inside Miyu's relay. The agy process may be kept alive across consecutive turns of one conversation, but it can be replaced at any time (idle timeout, configuration reload, tool set change), so work backgrounded through the built-in tools (run_command background runs, manage_task, schedule, subagents) is not guaranteed to survive a turn, and its completion notifications may never arrive. The built-in ask_question and generate_image tools are not wired to the user here. Messages reach you as text only: images, videos, audio and documents the user sends are saved to local files and the message carries their absolute paths. Open such a path with view_file to see or hear the media itself.\n</relay-environment>";

/// miyu 工具桥在场时的补充事实。
const RELAY_MIYU_TOOLS_NOTE: &str = "\n<relay-environment-tools>\nThe mcp_miyu_ tools live in the persistent Miyu daemon and survive across turns: mcp_miyu_subagent runs a background subagent that wakes a follow-up turn when it finishes, mcp_miyu_job inspects or stops those, mcp_miyu_alarm schedules timed reminders, mcp_miyu_ask_question actually reaches the user and waits for the answer, and mcp_miyu_generate_image delivers the picture to the user.\n</relay-environment-tools>";

/// 续传目标在 agy 侧已不存在:agy 不报错,静默新开了别的会话。
#[derive(Debug)]
pub(super) struct ResumeTargetLost {
    pub(super) requested: String,
    pub(super) actual: String,
}

impl std::fmt::Display for ResumeTargetLost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "agy did not resume conversation {} (it started {} instead)",
            self.requested, self.actual
        )
    }
}

impl std::error::Error for ResumeTargetLost {}

pub(super) fn resume_target_lost(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| cause.downcast_ref::<ResumeTargetLost>().is_some())
}

impl OpenAiCompatibleClient {
    pub(crate) async fn chat_antigravity_stream<F>(
        &self,
        messages: Vec<ChatMessage>,
        tools: Vec<ToolDefinition>,
        request_id: &str,
        on_chunk: &mut F,
    ) -> Result<ChatResult>
    where
        F: FnMut(ChatStreamChunk) -> Result<()>,
    {
        let runtime = self
            .antigravity
            .clone()
            .context("antigravity runtime was not initialized for this client")?;
        let model = self.provider.default_model.clone();
        let (system_prompt, conversation) = payload::split_system(messages);
        let workdir = miyu_base::workspace::effective_workdir();
        let miyu_session = miyu_base::workspace::try_session();
        let miyu_session = miyu_session.as_deref();
        let host_tools = cli_relay::host_tools_face(miyu_session);
        let restrictions = cli_relay::turn_restrictions(miyu_session);
        let scopes = cli_relay::tool_scopes(
            self.request_scope,
            &runtime.native_tools,
            &runtime.miyu_tools,
            self.claude_code_dev_mode,
            &restrictions,
        );
        let agent_prompt = cli_relay::compose_prompt(
            &system_prompt,
            scopes,
            RELAY_ENVIRONMENT_NOTE,
            RELAY_MIYU_TOOLS_NOTE,
        );
        let mut plan = ResumePlan::new(
            &self.provider.id,
            &model,
            &agent_prompt,
            conversation,
            self.request_scope,
            miyu_session,
            host_tools,
            &restrictions,
        );
        // 人格代理落盘(按内容哈希,内容不变就不写)。桥只在「作用域开着且有
        // 会话身份」时才注册:没有会话(回合作用域外/后台子代理)时桥本就应答空
        // 表,写一份空 eager 名单只会覆盖别的会话正在用的那份。
        let agent_name =
            setup::ensure_agent_file(&runtime.config_dir, &agent_prompt, scopes.native_on)?;
        let bridge_on = scopes.miyu_on && miyu_session.is_some();
        let mut eager_tools: Vec<String> = Vec::new();
        if bridge_on {
            if runtime.miyu_tools_eager {
                eager_tools = tools
                    .iter()
                    .map(|tool| tool.function.name.clone())
                    .filter(|name| {
                        !scopes.native_on || !BRIDGE_DUPLICATE_TOOLS.contains(&name.as_str())
                    })
                    .collect();
            }
            setup::ensure_mcp_entry(&runtime.config_dir, &eager_tools)?;
        }
        let env = relay_env(scopes, miyu_session);
        let mut outcome = self
            .agy_turn(
                &runtime,
                &model,
                &workdir,
                &env,
                &agent_name,
                &plan,
                miyu_session,
                &eager_tools,
                request_id,
                on_chunk,
            )
            .await;
        if let Err(error) = &outcome {
            // 两种都是「这条 agy 会话不能再用了」,处理一样:作废续传条目、
            // 全量重放开一条新的,重试一次。
            // - ResumeTargetLost:会话没了(被清理/过期)。init 是流的首行、先于
            //   模型调用,所以上面那次几乎没花额度。
            // - SessionPoisoned:会话还在,但转录已经废了(取消态步/压缩炸掉),
            //   再续传多少次都是同一句报错——09-16 群聊非管理员档连挂两小时。
            if plan.resume_id().is_some()
                && (resume_target_lost(error) || cli_relay::session_poisoned(error))
            {
                plan.resume_lost("antigravity", request_id, error);
                outcome = self
                    .agy_turn(
                        &runtime,
                        &model,
                        &workdir,
                        &env,
                        &agent_name,
                        &plan,
                        miyu_session,
                        &eager_tools,
                        request_id,
                        on_chunk,
                    )
                    .await;
            }
        }
        let outcome = outcome?;
        if plan.ephemeral() {
            // 辅助请求用完顺手删掉 agy 侧转录,免得把用户的会话列表刷满。
            if let Some(conversation_id) = &outcome.session_id {
                setup::remove_conversation_files(conversation_id);
            }
        }
        if outcome.session_poisoned {
            // 本轮靠「有正文就交付」救回来了,但这条会话的粘性 ERROR 不会自己
            // 好:记下它只会让下一轮再撞一次。退休掉,下一轮全量重放开新的。
            if let Some(conversation_id) = &outcome.session_id {
                tracing::warn!(
                    request_id,
                    conversation = %conversation_id,
                    "retiring the poisoned agy conversation; the next turn replays into a fresh one"
                );
            }
            plan.retire_session(&outcome);
        } else {
            plan.record(&outcome);
        }
        Ok(outcome.result)
    }

    /// 一轮 agy:能借到常驻进程(同会话、同钥匙、同 agy 会话)就往它 stdin 再写一段,
    /// 否则起新进程(复用开着时起常驻的)。跑完没坏就还回池里。
    #[allow(clippy::too_many_arguments)]
    async fn agy_turn<F>(
        &self,
        runtime: &AntigravityRuntime,
        model: &str,
        workdir: &std::path::Path,
        env: &[(String, Option<String>)],
        agent_name: &str,
        plan: &ResumePlan,
        miyu_session: Option<&str>,
        eager_tools: &[String],
        request_id: &str,
        on_chunk: &mut F,
    ) -> Result<RelayOutcome>
    where
        F: FnMut(ChatStreamChunk) -> Result<()>,
    {
        let payload = render_stdin_line(plan.delta());
        let args = self.antigravity_args(runtime, model, workdir, agent_name, plan.resume_id());
        // 辅助请求(scope≠chat)一次一个 agy 会话,不复用;回合作用域外没有会话身份也不。
        let reuse = runtime.reuse_process && !plan.ephemeral() && miyu_session.is_some();
        let fingerprint = reuse.then(|| {
            let base_args = self.antigravity_args(runtime, model, workdir, agent_name, None);
            pool::fingerprint(
                &runtime.binary,
                &base_args,
                env,
                workdir,
                plan.host_tools(),
                plan.restrictions(),
                eager_tools,
                miyu_base::sandbox::current_sandbox().as_deref(),
            )
        });
        let mut pooled: Option<(RelayProcess, u32)> = None;
        if let (Some(fingerprint), Some(session), Some(conversation)) =
            (fingerprint.as_deref(), miyu_session, plan.resume_id())
        {
            if let Some((mut process, turns)) = pool::take(fingerprint, session, conversation) {
                if process.is_alive() && process.write_payload(&payload).await.is_ok() {
                    pooled = Some((process, turns));
                } else {
                    tracing::info!(
                        target: "miyu::relay",
                        request_id,
                        pid = process.pid(),
                        "pooled agy process is gone; starting a fresh one"
                    );
                    process.retire();
                }
            }
        }
        let reused = pooled.is_some();
        crate::llm::request_log::record(
            &self.provider.id,
            model,
            "antigravity",
            self.request_scope,
            &runtime.binary.display().to_string(),
            &json!({
                "args": args,
                "stdin": payload,
                "conversation": plan.conversation(),
                "reused_process": reused,
            }),
        );
        let not_found = || {
            t(
                "Antigravity CLI (agy) not found; install it or set plugins.antigravity.binary",
                "找不到 Antigravity CLI(agy);请安装它或配置 plugins.antigravity.binary",
            )
            .to_string()
        };
        let (process, turns) = match pooled {
            Some(pooled) => pooled,
            None if reuse => (
                RelayProcess::spawn_persistent(
                    &runtime.binary,
                    &args,
                    workdir,
                    env,
                    &payload,
                    runtime.idle_timeout,
                    "antigravity.stream",
                    "agy",
                    not_found,
                )
                .await?,
                0,
            ),
            None => (
                RelayProcess::spawn(
                    &runtime.binary,
                    &args,
                    workdir,
                    env,
                    &payload,
                    runtime.idle_timeout,
                    "antigravity.stream",
                    "agy",
                    not_found,
                )
                .await?,
                0,
            ),
        };
        let (outcome, live) = stream::run_agy_turn(
            process,
            agent_name,
            plan.resume_id(),
            reused,
            reuse,
            request_id,
            on_chunk,
        )
        .await?;
        if let Some(process) = live {
            match (fingerprint.as_deref(), miyu_session, &outcome.session_id) {
                (Some(fingerprint), Some(session), Some(conversation))
                    if !outcome.session_poisoned =>
                {
                    pool::park(
                        fingerprint,
                        session,
                        conversation,
                        process,
                        runtime.reuse_idle,
                        turns + 1,
                    );
                }
                _ => process.retire(),
            }
        }
        Ok(outcome)
    }

    fn antigravity_args(
        &self,
        runtime: &AntigravityRuntime,
        model: &str,
        workdir: &std::path::Path,
        agent_name: &str,
        resume: Option<&str>,
    ) -> Vec<String> {
        // `--print=`:print 旗标必须带参数(空串即可),否则它把下一个旗标吃成
        // 提示词。
        let mut args: Vec<String> = [
            "--print=",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--dangerously-skip-permissions",
        ]
        .into_iter()
        .map(str::to_string)
        .collect();
        args.push("--model".into());
        args.push(model.to_string());
        if let Some((_, variant)) = self.selected_reasoning_variant() {
            if let miyu_base::models_cache::ReasoningSetting::Effort(effort) = variant.setting {
                args.push("--effort".into());
                args.push(effort);
            }
        }
        // 人格代理:恒挂。流侧校验 init.agent,没挂上视为错误(否则静默跑在
        // agy 自己 13.9k tok 的默认提示词上)。
        args.push("--agent".into());
        args.push(agent_name.to_string());
        // 原生 run_command 默认跑在 agy 的 scratch 目录,不是进程 cwd;只有
        // --add-dir 过的目录才是它的工作区。只加一个:加两个时 cwd 在两者间随机。
        args.push("--add-dir".into());
        args.push(workdir.display().to_string());
        args.push("--print-timeout".into());
        args.push(format!("{}s", runtime.print_timeout.as_secs()));
        if let Some(resume) = resume {
            args.push("--conversation".into());
            args.push(resume.to_string());
        }
        args
    }
}

/// 给 agy 进程的环境:它会原样继承给 MCP 子进程(实测),所以桥的会话身份
/// 走这里而不是 mcp_config 的静态 env。MIYU_HOME/XDG_RUNTIME_DIR 本来就在
/// 我们自己的环境里,自然继承,不再显式塞(claude 线第六轮的「如实透传」教训
/// 在这里天然满足)。
fn relay_env(scopes: ToolScopes, miyu_session: Option<&str>) -> Vec<(String, Option<String>)> {
    let mut env: Vec<(String, Option<String>)> = Vec::new();
    match (scopes.miyu_on, miyu_session) {
        (true, Some(session)) => {
            env.push(("MIYU_SESSION".into(), Some(session.to_string())));
            let origin = serde_json::to_string(&miyu_base::workspace::current_turn_origin())
                .unwrap_or_default();
            env.push(("MIYU_TURN_ORIGIN".into(), Some(origin)));
            // 桥吐的 schema 按 Gemini 方言整形(空 enum/联合类型/多余键会被 400)。
            env.push(("MIYU_MCP_SCHEMA_DIALECT".into(), Some("gemini".into())));
            env.push((
                "MIYU_MCP_EXCLUDE".into(),
                Some(if scopes.native_on {
                    BRIDGE_DUPLICATE_TOOLS.join(",")
                } else {
                    String::new()
                }),
            ));
        }
        _ => {
            // 桥关着:抹掉会话身份,守卫让 mcp-serve 只应答空工具表。
            env.push(("MIYU_SESSION".into(), None));
            env.push(("MIYU_TURN_ORIGIN".into(), None));
            env.push(("MIYU_MCP_EXCLUDE".into(), None));
            env.push(("MIYU_MCP_SCHEMA_DIALECT".into(), None));
        }
    }
    env
}

/// agy 对单条 user 输入的硬上限是 **192,000 字节**(09-04 案卷五次采样钉死:
/// 上限是字节,不是字符也不是 token),超过就从尾部静默截断、照样返回 SUCCESS
/// ——而活跃尾巴(本轮真实消息)排在载荷末尾,先死的永远是它。这里取九折,
/// 余量留给 agy 自己追加的通知与 `<ADDITIONAL_METADATA>`。
pub(in crate::llm::openai_compatible) const STDIN_BYTE_BUDGET: usize = 172_800;

/// stdin 的单行载荷:agy 的 `{"event":"user","message":{"content":[…]}}`。
/// 内容块复用 claude 线的翻译(历史转写 + 活跃尾巴),agy 只收 text 块,
/// 图片块降级成占位文本。载荷按 [`STDIN_BYTE_BUDGET`] 收口:尾巴整条保留,
/// 历史从最老的回合丢。
fn render_stdin_line(delta: &[ChatMessage]) -> String {
    let blocks: Vec<Value> = payload::render_user_blocks(delta, Some(STDIN_BYTE_BUDGET))
        .into_iter()
        .map(|block| {
            if block.get("type").and_then(Value::as_str) == Some("image") {
                json!({
                    "type": "text",
                    "text": "[image omitted: the antigravity relay accepts text only]"
                })
            } else {
                block
            }
        })
        .collect();
    let mut line = json!({
        "event": "user",
        "message": { "content": blocks }
    })
    .to_string();
    line.push('\n');
    line
}

#[cfg(test)]
mod bridge_dedup_tests {
    use super::{BRIDGE_DUPLICATE_TOOLS, NATIVE_TOOLS};

    /// 白名单是给 agy 的原生名,与 Miyu 工具名撞上的只能是同名剔除项:同名
    /// 两源同时出现会让卡片没法区分。
    #[test]
    fn native_allowlist_does_not_collide_with_bridged_miyu_names() {
        for name in NATIVE_TOOLS {
            let miyu_has_it = name == &"run_command";
            assert!(
                !miyu_has_it || BRIDGE_DUPLICATE_TOOLS.contains(name),
                "{name} 同时是 agy 原生名与 Miyu 工具名,必须进去重名单"
            );
        }
    }
}
