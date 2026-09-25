//! 指令源（09-25，opencode 调研 3.2 第 3 条）：模型得知道「此刻是什么样」的事，每件一个源
//! ——时间与工作目录（`<runtime>`）、沙盒（`<sandbox>`）、场所递进来的状态快照
//! （网页会话的 `<artifact-workspace>`）。
//!
//! 规矩只写一遍（[`project`]）：跟请求里最近一份逐字节比，一样就不发，变了发新的整份；
//! 此刻没有了、模型最近看到的还是有，由源自己给那一句「没了」。发出去的块跟着这一轮的
//! 尾巴化石化（v7 append-only），「模型最近看到的是什么」直接从请求里读，不另记状态：
//! 带着它的那一轮被压缩折进摘要、或被裁剪掉以后，请求里就看不到了，下一轮自然整份重发。
//!
//! 这里只放事实。要模型照着做的话放系统提示词那一侧：化石原样重放，写在尾巴里的指令
//! 过几轮就成了过期命令（AGENTS §1.4）。

use crate::agent::*;

/// 一件会在会话中途变化的事。
pub(in crate::agent) trait InstructionSource {
    /// 块的开头。
    fn tag(&self) -> &'static str;

    /// 请求里模型最近看到的那一份。默认：以 [`Self::tag`] 开头的最后一条 user 消息
    /// （块单独占一条消息；用户输入不会以这样的标签开头）。
    fn latest<'a>(&self, messages: &'a [ChatMessage]) -> Option<&'a str> {
        last_fossil_with_prefix(messages, self.tag())
    }

    /// 此刻的完整块；`None` = 此刻没有这件事。
    fn current(&self) -> Option<String>;

    /// 此刻没有了、模型最近看到的却还是有：补的那一句（最近一份已经是它就不再补）。
    /// 默认不补。
    fn gone(&self) -> Option<String> {
        None
    }
}

/// 这一轮这件事该发什么；`None` = 不发。
pub(in crate::agent) fn project(
    source: &dyn InstructionSource,
    messages: &[ChatMessage],
) -> Option<String> {
    let last = source.latest(messages);
    match source.current() {
        Some(block) => (last != Some(block.as_str())).then_some(block),
        None => {
            let last = last?;
            source.gone().filter(|notice| notice != last)
        }
    }
}

/// 按顺序投影，要发的块逐条追加到 `messages` 末尾。顺序就是缓存前缀：新源只往后加。
pub(in crate::agent) fn push_projected(
    sources: &[Box<dyn InstructionSource>],
    messages: &mut Vec<ChatMessage>,
) {
    for source in sources {
        if let Some(block) = project(source.as_ref(), messages) {
            messages.push(ChatMessage::turn_context(block));
        }
    }
}

/// `<runtime now=… [cwd=…]/>`（`prompt::runtime_context`）。
///
/// dsh 式投影（08-16 缓存调研）：终端面时间降到小时级，同一小时内 cwd 不变就和历史里
/// 最近一份逐字节相同，这一轮零新增；平台面保留分钟级，人格报时靠它。
pub(in crate::agent) struct RuntimeSource {
    pub platform: bool,
}

impl InstructionSource for RuntimeSource {
    fn tag(&self) -> &'static str {
        "<runtime "
    }

    fn current(&self) -> Option<String> {
        Some(runtime_context(self.platform))
    }
}

/// `<sandbox …/>`：这一轮关在哪、能碰什么。
///
/// 09-23 起不在系统提示词里：按 Tab 随开随关，写在前缀里每切一次就掰断整段缓存。
/// 说过而现在关了，补一条「关了」；从没说过而且现在没沙盒，什么都不发。
pub(in crate::agent) struct SandboxSource;

impl SandboxSource {
    /// 谁看得到：跟主机环境块同一判据（属主回合、WebUI 回合；QQ 等平台回合不带）
    /// ——沙盒说明 09-23 之前就写在那个块里，挪出来不改谁看得到。
    pub(in crate::agent) fn applies(audience: PromptAudience, platform_turn: bool) -> bool {
        audience == PromptAudience::Owner
            || (audience == PromptAudience::External && !platform_turn)
    }
}

impl InstructionSource for SandboxSource {
    fn tag(&self) -> &'static str {
        "<sandbox "
    }

    fn current(&self) -> Option<String> {
        miyu_base::sandbox::current_sandbox()
            .map(|policy| miyu_base::host_info::sandbox_notice(&policy))
    }

    fn gone(&self) -> Option<String> {
        Some(miyu_base::host_info::SANDBOX_OFF_NOTICE.to_string())
    }
}

/// 状态快照类的回合尾巴块:内容是「此刻的状态」(网页会话的 artifact 清单)。和对话里
/// **最近一份**同名快照逐字节相同就不再重发——模型眼前最近那份就是现状。比最近一份而不是
/// 任意一份:清单 A → B → A 时历史里确实有 A,但模型最近看到的是 B,A 必须重发。
/// 压缩把旧快照折进摘要后看不见了,自然会再发一次。
pub(in crate::agent) const STATE_SNAPSHOT_TAGS: &[&str] = &[crate::tools::ARTIFACT_WORKSPACE_TAG];

/// 请求里最近一份以 `tag` 开头的快照块(整块,到收尾标签为止)。块可能和别的尾巴块拼在同一条
/// user 消息里,所以在正文里按标签找,不要求它在消息开头。
pub(in crate::agent) fn latest_visible_snapshot<'a>(
    messages: &'a [ChatMessage],
    tag: &str,
) -> Option<&'a str> {
    let close = format!("</{}", tag.trim_start_matches('<'));
    messages
        .iter()
        .rev()
        .filter(|message| message.role == "user")
        .find_map(|message| {
            let Some(ChatContent::Text(text)) = message.content.as_ref() else {
                return None;
            };
            let start = text.rfind(tag)?;
            let end = start + text[start..].find(close.as_str())? + close.len();
            Some(&text[start..end])
        })
}

/// 场所递进来的一份状态快照（[`STATE_SNAPSHOT_TAGS`]）。它和别的场所块拼在同一条消息里
/// 发（`assemble_turn_tail`），所以最近一份按标签在正文里找；场所不再递它时什么都不说。
pub(in crate::agent) struct HostSnapshot<'b> {
    tag: &'static str,
    block: &'b str,
}

impl<'b> HostSnapshot<'b> {
    /// `block` 是状态快照就给它一个源。
    pub(in crate::agent) fn of(block: &'b str) -> Option<Self> {
        STATE_SNAPSHOT_TAGS
            .iter()
            .find(|tag| block.starts_with(**tag))
            .map(|tag| HostSnapshot { tag, block })
    }
}

impl InstructionSource for HostSnapshot<'_> {
    fn tag(&self) -> &'static str {
        self.tag
    }

    fn latest<'a>(&self, messages: &'a [ChatMessage]) -> Option<&'a str> {
        latest_visible_snapshot(messages, self.tag)
    }

    fn current(&self) -> Option<String> {
        Some(self.block.to_string())
    }
}

impl Agent {
    /// 这一轮回合尾巴上的指令源，按发送顺序。
    pub(in crate::agent) fn instruction_sources(&self) -> Vec<Box<dyn InstructionSource>> {
        let platform = self.input.platform_context.is_some();
        let mut sources: Vec<Box<dyn InstructionSource>> =
            vec![Box::new(RuntimeSource { platform })];
        // 整体替换了系统提示词的会话连主机环境块都没有，沙盒也不说。
        if self.input.system_prompt_override.is_none()
            && SandboxSource::applies(self.core.prompt_audience, platform)
        {
            sources.push(Box::new(SandboxSource));
        }
        sources
    }
}
