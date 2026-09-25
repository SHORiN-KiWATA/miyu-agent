//! 本机 CLI 中转线(claude-code / antigravity / codex)的共用骨架。
//!
//! 三条线的传输都是「拉起一个 CLI 子进程,喂一段 stdin,按行读结构化事件」,
//! 工具循环都在 CLI 侧闭环,Miyu 的工具都经 `miyu mcp-serve` 桥挂进去。各线
//! 只差三样:命令行怎么拼、stdin 长什么样、事件怎么解析。其余——工具作用域
//! 裁决、逐消息哈希链续传、载荷转写、子进程泵、清空联动——都在这里。
//!
//! 续传驱动([`ResumePlan`])刻意做成两步 API 而不是回调:各线的 run 闭包
//! 要可变借用 on_chunk 跨 await,塞进 FnMut→Future 的签名里表达不了;两步
//! 之后每条线只剩十行样板,也更好读。

pub(in crate::llm::openai_compatible) mod payload;
pub(in crate::llm::openai_compatible) mod process;
pub(in crate::llm::openai_compatible) mod session;

use crate::llm::openai_compatible::*;

/// 模式作用域判定:会话是 dev 还是 normal 由 Agent 构造时经
/// `with_claude_code_dev_mode` 打到客户端上。未知值按 off 兜底。
pub(in crate::llm::openai_compatible) fn scope_allows(scope: &str, dev_mode: bool) -> bool {
    miyu_base::config::relay_scope_allows(scope, dev_mode)
}

/// 本轮的双四档裁决结果。
#[derive(Clone, Copy, Debug)]
pub(in crate::llm::openai_compatible) struct ToolScopes {
    pub(in crate::llm::openai_compatible) native_on: bool,
    pub(in crate::llm::openai_compatible) miyu_on: bool,
}

/// 工具面按双四档作用域装配。subagent 作用域也给:中转不会把工具循环交还
/// 给外层(SubagentRunner 收到的永远是最终文本),子代理的干活能力全靠内层
/// CLI 自己的原生工具 + MCP 桥闭环;真正的纯文本辅助请求(摘要/标题/judge)
/// 仍然无工具。
pub(in crate::llm::openai_compatible) fn tool_scopes(
    request_scope: &str,
    native_scope: &str,
    miyu_scope: &str,
    dev_mode: bool,
    restrictions: &miyu_base::host_ports::TurnToolRestrictions,
) -> ToolScopes {
    let tool_capable = matches!(request_scope, "chat" | "subagent");
    // 这一轮带了工具白名单(含 `--no-tools`):CLI 自带的原生工具一律关。调用方要的
    // 是「精确就这几样」,留着 Bash 等于绕过(Bash 能做 run_command 能做的一切,用户
    // 09-23 拍板)。白名单是空的就连桥也不挂:挂上去也是一张空表,还白起一个 MCP
    // 子进程、往提示词里写一段 Miyu 工具说明。只带 `--no-memory` 不碰原生工具。
    let allowlisted = restrictions.allowlist.is_some();
    let no_tools = restrictions.allowlist.as_ref().is_some_and(Vec::is_empty);
    // 沙盒回合(成员)里 CLI 自带的工具照开:整个 CLI 进程关在 Landlock 里
    // (`RelayProcess::spawn` → `sandbox::confine_relay`),它起的 Bash/Edit 子进程
    // 继承规则。09-11 用户拍板:关进沙盒,不是关掉工具。
    ToolScopes {
        native_on: tool_capable && !allowlisted && scope_allows(native_scope, dev_mode),
        miyu_on: tool_capable && !no_tools && scope_allows(miyu_scope, dev_mode),
    }
}

/// 这一轮的单轮覆盖项限制(`miyu ask --tools / --no-tools / --no-memory`),回合装配时
/// 按会话登记。每轮只读一次、交给 [`tool_scopes`] 与 [`ResumePlan`] 共用:分两次读,
/// 中间有别的回合登记或撤掉,两边就对不上。回合作用域外(没有会话)= 不限制。
pub(in crate::llm::openai_compatible) fn turn_restrictions(
    miyu_session: Option<&str>,
) -> miyu_base::host_ports::TurnToolRestrictions {
    miyu_session
        .map(miyu_base::host_ports::live_turn_tool_restrictions)
        .unwrap_or_default()
}

/// 本轮经 MCP 桥暴露的工具面档位。判据与桥完全同源:桥问工具时走
/// `attach_owner_turn_tools` → `apply_platform_turn_scope`,取的就是这个活体
/// 平台上下文的 `host_tools_allowed()`。非平台会话(REPL/WebUI/回合外)没有
/// 登记,按全量底座记——那些路径本来就只有 owner 一档。
pub(in crate::llm::openai_compatible) fn host_tools_face(miyu_session: Option<&str>) -> bool {
    miyu_session
        .and_then(miyu_base::host_ports::live_turn_host_tools_allowed)
        .unwrap_or(true)
}

/// 中转侧工具活动里**不**翻成卡片事件的工具。桥问答(`ask_question`)有自己
/// 的事件流(question.requested/answered,由 bridge_question 直发 EventHub),
/// 再发一份 tool.started 只会捣乱:CLI 的工具步开始与桥的 question.requested
/// 并发到达,前者晚到时终端的「准备问题」黏性态在面板关掉之后才被置上,
/// 此后每个工具前都挂着「准备问题」(09-03 用户实录)。
pub(in crate::llm::openai_compatible) fn hidden_remote_tool(name: &str) -> bool {
    name == "ask_question"
}

/// 折叠空白并按字符截断,给思考通道的一行摘要用。
pub(in crate::llm::openai_compatible) fn compact_line(text: &str, limit: usize) -> String {
    let mut collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() > limit {
        collapsed = collapsed.chars().take(limit).collect::<String>() + "…";
    }
    collapsed
}

/// 按字符截断但保留换行结构:命令输出走命令输出块渲染,折叠空白会把
/// 多行日志压成一行。
pub(in crate::llm::openai_compatible) fn truncate_block(text: &str, limit: usize) -> String {
    let trimmed = text.trim_end();
    if trimmed.chars().count() > limit {
        trimmed.chars().take(limit).collect::<String>() + "…"
    } else {
        trimmed.to_string()
    }
}

/// 中转工具结果的展示整形:命令家族保换行,其余折成一行;`\r` 一律去掉。
pub(in crate::llm::openai_compatible) fn shape_remote_output(name: &str, output: &str) -> String {
    let output = output.replace('\r', "");
    if miyu_base::tool_names::is_command_tool(name) {
        truncate_block(&output, 4000)
    } else {
        compact_line(&output, 4000)
    }
}

/// 系统提示词 + 中转环境事实(常量字节,前缀稳定):每轮一进程,自带后台/
/// 通知活不过本轮——这是模型光靠自我认知猜不到的宿主事实。各线只差措辞。
pub(in crate::llm::openai_compatible) fn compose_prompt(
    system_prompt: &str,
    scopes: ToolScopes,
    environment_note: &str,
    tools_note: &str,
) -> String {
    let mut prompt = system_prompt.to_string();
    if scopes.native_on || scopes.miyu_on {
        prompt.push_str(environment_note);
        if scopes.miyu_on {
            prompt.push_str(tools_note);
        }
    }
    prompt
}

/// 桥进程要认得 daemon 的 home/runtime 目录,但必须**如实透传**(daemon 自己
/// 有什么才给什么):runtime 目录推导对「显式设了 MIYU_HOME」与「没设」给出
/// 不同路径(默认 home 显式设也会变成哈希子目录),无条件塞 MIYU_HOME 会让
/// mcp-serve 连不上正常启动的 daemon,静默滑进直连兜底(claude 线第六轮实录)。
pub(in crate::llm::openai_compatible) fn bridge_env_passthrough() -> Vec<(String, String)> {
    ["MIYU_HOME", "XDG_RUNTIME_DIR"]
        .into_iter()
        .filter_map(|key| {
            std::env::var_os(key)
                .map(|value| (key.to_string(), value.to_string_lossy().to_string()))
        })
        .collect()
}

/// CLI 侧这条会话已经坏到再续传也只会重复失败:上层作废续传条目、开一条
/// 干净会话重试一次。与 [`ResumeTargetLost`](super::antigravity::ResumeTargetLost)
/// 的区别只在于会话还在、但状态已经废了。
#[derive(Debug)]
pub(in crate::llm::openai_compatible) struct SessionPoisoned;

impl std::fmt::Display for SessionPoisoned {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the CLI-side conversation is no longer usable")
    }
}

impl std::error::Error for SessionPoisoned {}

/// 标记是用 `.context(SessionPoisoned)` 挂上去的,`chain()` 迭代出来的是
/// anyhow 的包装类型、对它 downcast 认不出来;得走 anyhow 自己的 downcast。
pub(in crate::llm::openai_compatible) fn session_poisoned(error: &anyhow::Error) -> bool {
    error.downcast_ref::<SessionPoisoned>().is_some()
}

/// 一轮中转的结果:正文结果 + CLI 侧会话 id(续传映射用)。
pub(in crate::llm::openai_compatible) struct RelayOutcome {
    pub(in crate::llm::openai_compatible) result: ChatResult,
    pub(in crate::llm::openai_compatible) session_id: Option<String>,
    /// 本轮跑成了,但 CLI 侧这条会话已经带着上一次失败的粘性状态(见
    /// `antigravity::stream` 对 `result.status` 的注释)。结果照常交付,但这条
    /// 会话不再留作续传目标:下一轮重开一条干净的。
    pub(in crate::llm::openai_compatible) session_poisoned: bool,
}

/// 一轮中转的续传计划:哈希链、命中的 CLI 会话、要发的增量。
pub(in crate::llm::openai_compatible) struct ResumePlan {
    provider_id: String,
    model: String,
    miyu_session: Option<String>,
    host_tools: bool,
    /// 本轮单轮限制的签名:和 `host_tools` 一样是工具面档位的一维(续传、进程复用)。
    restrictions: String,
    ephemeral: bool,
    conversation: Vec<ChatMessage>,
    chain: Vec<u64>,
    resumable: Option<(String, usize)>,
}

impl ResumePlan {
    /// `prompt_seed` 是进哈希链种子的系统提示词(含环境事实):它一变,整条
    /// 链失配,下一轮自然全量重放。辅助请求(scope≠chat)一次一个 CLI 会话,
    /// 不参与续传匹配,免得污染主对话的会话映射。
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        provider_id: &str,
        model: &str,
        prompt_seed: &str,
        conversation: Vec<ChatMessage>,
        request_scope: &str,
        miyu_session: Option<&str>,
        host_tools: bool,
        restrictions: &miyu_base::host_ports::TurnToolRestrictions,
    ) -> Self {
        let ephemeral = request_scope != "chat";
        let restrictions = restrictions.signature();
        let chain = session::prefix_chain(provider_id, model, prompt_seed, &conversation);
        let resumable = if ephemeral {
            None
        } else {
            match session::find_resumable(
                provider_id,
                model,
                miyu_session,
                host_tools,
                &restrictions,
                &chain,
                conversation.len(),
            ) {
                Ok(hit) => Some(hit),
                Err(miss) => {
                    // 全量重放的原因留痕(09-04 案卷 B′):不写出来,每次重放都
                    // 得靠猜。首轮 NoEntry 是正常的,其余都值得看一眼。
                    tracing::info!(
                        provider = provider_id,
                        host_tools,
                        restrictions = %restrictions,
                        messages = conversation.len(),
                        reason = ?miss,
                        "relay resume miss; replaying the full conversation in a fresh session"
                    );
                    None
                }
            }
        };
        Self {
            provider_id: provider_id.to_string(),
            model: model.to_string(),
            miyu_session: miyu_session.map(str::to_string),
            host_tools,
            restrictions,
            ephemeral,
            conversation,
            chain,
            resumable,
        }
    }

    pub(in crate::llm::openai_compatible) fn ephemeral(&self) -> bool {
        self.ephemeral
    }

    /// 本轮的工具面档位(进程复用的钥匙要带它:换脸就换进程)。
    pub(in crate::llm::openai_compatible) fn host_tools(&self) -> bool {
        self.host_tools
    }

    /// 本轮单轮限制的签名(进程复用的钥匙同样要带:限制变了工具面就变了)。
    pub(in crate::llm::openai_compatible) fn restrictions(&self) -> &str {
        &self.restrictions
    }

    /// 命中的 CLI 会话 id(要 `--resume`/`--conversation`/`resume` 的目标)。
    pub(in crate::llm::openai_compatible) fn resume_id(&self) -> Option<&str> {
        self.resumable.as_ref().map(|(id, _)| id.as_str())
    }

    /// 本轮要发给 CLI 的增量:命中续传就只发未覆盖的尾巴,否则整段。
    pub(in crate::llm::openai_compatible) fn delta(&self) -> &[ChatMessage] {
        let covered = self.resumable.as_ref().map(|(_, len)| *len).unwrap_or(0);
        &self.conversation[covered..]
    }

    pub(in crate::llm::openai_compatible) fn conversation(&self) -> &[ChatMessage] {
        &self.conversation
    }

    /// CLI 侧会话没了(过期/被清理/静默新开):忘掉映射,改成整段全量重放。
    /// 只对「会话找不到」类错误自愈,限流/登录错误照常上抛——调用方判定。
    pub(in crate::llm::openai_compatible) fn resume_lost(
        &mut self,
        kind: &str,
        request_id: &str,
        error: &anyhow::Error,
    ) {
        tracing::warn!(
            request_id,
            error = %format!("{error:#}"),
            "{kind} resume target is gone; replaying the full conversation in a fresh session"
        );
        if let Some((id, _)) = self.resumable.take() {
            session::forget_session(&id);
        }
    }

    /// 本轮交付了结果,但 CLI 侧这条会话已经废了([`RelayOutcome::session_poisoned`]):
    /// **不**把它记成下一轮的续传目标,并把可能已存在的旧映射一并抹掉。下一轮
    /// 匹配不到条目,自然走全量重放开一条干净会话。
    pub(in crate::llm::openai_compatible) fn retire_session(&self, outcome: &RelayOutcome) {
        if self.ephemeral {
            return;
        }
        if let Some(session_id) = &outcome.session_id {
            session::forget_session(session_id);
        }
    }

    /// 回合结束:预测下一轮的前缀(已发送的会话消息 + 一条 assistant 正文)
    /// 并记下 CLI 会话 id。预测若与实际化石有分歧,下一轮匹配不上,自动退化为
    /// 全量重放——只损失效率,不损失正确性。辅助请求不记。
    pub(in crate::llm::openai_compatible) fn record(&self, outcome: &RelayOutcome) {
        if self.ephemeral {
            return;
        }
        let Some(session_id) = &outcome.session_id else {
            return;
        };
        let content = outcome.result.content.clone();
        if content.trim().is_empty() {
            return;
        }
        let predicted = ChatMessage::assistant(content, None);
        let next_hash = session::extend_chain(self.chain[self.conversation.len()], &predicted);
        session::record_session(
            &self.provider_id,
            &self.model,
            self.miyu_session.as_deref(),
            self.host_tools,
            &self.restrictions,
            self.conversation.len() + 1,
            next_hash,
            session_id.clone(),
        );
    }
}

/// 清空 Miyu 会话时的联动(三条 CLI 中转线共用):丢弃它名下的续传映射,
/// 并尽力删除各家 CLI 侧的会话转录。存储布局是各家 CLI 的内部实现,删不到
/// 只记日志不报错——映射已丢弃,该会话无论如何不会再被续传。会话 id 都是
/// 全局唯一,每家都试一遍不会误删。
pub fn forget_relay_sessions(miyu_session: &str) {
    // 常驻的 agy 进程手里就是这条会话,一并收掉。
    super::antigravity::pool::forget_session(miyu_session);
    // 前缀指纹的上一条链、断缓存的上一请求也跟着这条会话走。
    crate::llm::cache_prefix::forget_session(miyu_session);
    crate::llm::cache_break::forget_session(miyu_session);
    let removed = session::forget_miyu_session(miyu_session);
    if removed.is_empty() {
        return;
    }
    for relay_session in &removed {
        super::claude_code::remove_transcript(miyu_session, relay_session);
        super::antigravity::remove_conversation_files(relay_session);
        super::codex::remove_rollout(relay_session);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scopes_follow_request_scope_and_mode() {
        let free = miyu_base::host_ports::TurnToolRestrictions::default();
        let scopes = tool_scopes("chat", "all", "dev", false, &free);
        assert!(scopes.native_on && !scopes.miyu_on);
        let scopes = tool_scopes("subagent", "normal", "all", true, &free);
        assert!(!scopes.native_on && scopes.miyu_on);
        let scopes = tool_scopes("compact", "all", "all", false, &free);
        assert!(!scopes.native_on && !scopes.miyu_on);
    }

    /// 单轮覆盖项(09-23 用户拍板):带了白名单就关原生工具;白名单为空连桥也不挂;
    /// 只带 `--no-memory` 两样都照开(remember_fact 由桥那头摘)。
    #[test]
    fn a_turn_allowlist_switches_native_tools_off() {
        use miyu_base::host_ports::TurnToolRestrictions;
        let only_read = TurnToolRestrictions {
            allowlist: Some(vec!["read".into()]),
            no_memory_writes: false,
        };
        let scopes = tool_scopes("chat", "all", "all", false, &only_read);
        assert!(!scopes.native_on && scopes.miyu_on);
        let nothing = TurnToolRestrictions {
            allowlist: Some(Vec::new()),
            no_memory_writes: false,
        };
        let scopes = tool_scopes("chat", "all", "all", false, &nothing);
        assert!(!scopes.native_on && !scopes.miyu_on);
        let no_memory = TurnToolRestrictions {
            allowlist: None,
            no_memory_writes: true,
        };
        let scopes = tool_scopes("chat", "all", "all", false, &no_memory);
        assert!(scopes.native_on && scopes.miyu_on);
    }

    #[test]
    fn remote_output_shaping_keeps_command_newlines() {
        assert_eq!(shape_remote_output("run_command", "a\r\nb\r\n"), "a\nb");
        assert_eq!(shape_remote_output("Bash", "a\nb\n"), "a\nb");
        assert_eq!(shape_remote_output("use_meme", "a\nb"), "a b");
        let long = "x".repeat(4001);
        assert!(truncate_block(&long, 4000).ends_with('…'));
    }

    #[test]
    fn plan_prefers_full_replay_for_auxiliary_scopes() {
        let conversation = vec![ChatMessage::plain("user", "hi")];
        let plan = ResumePlan::new(
            "p",
            "m",
            "seed",
            conversation,
            "compact",
            None,
            true,
            &miyu_base::host_ports::TurnToolRestrictions::default(),
        );
        assert!(plan.ephemeral());
        assert!(plan.resume_id().is_none());
        assert_eq!(plan.delta().len(), 1);
    }
}
