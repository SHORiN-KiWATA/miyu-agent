//! 状态行上的前台子代理（会话项目第 4 段之二）。
//!
//! 子代理 09-18 起是一条会话，它做了什么在它自己那儿，点状态行那一行（跑完之后是时间线上
//! 那一步）切进去看。父会话这边只画那一行：它这会儿在干什么、烧了多少词元（引擎按中继的
//! 标记收好、`subagent.progress` 送来的），再把那一行链到它的会话。
//!
//! 原来这里按标记流攒一份子代理的内层时间线，浮层把它在父会话里再画一遍——09-25 随老
//! 标记中继一起退役。

use super::*;
use miyu_engine::tools::subagent::status::SubagentStatus;

impl StreamRenderer {
    /// 前台子代理报了一次样子：窥视、词元、子会话。
    pub fn write_subagent_status(&mut self, name: &str, status: SubagentStatus) {
        self.subagent_tokens.insert(name.to_string(), status.tokens);
        let session = status.session_id.clone();
        self.subagent_status.insert(name.to_string(), status);
        if let Some(session) = session {
            self.subagent_session(name, &session);
        }
    }

    /// 这个子代理的会话到了（实时是 `subagent.progress`，回放是结果里带的）：它那一行
    /// 点下去切进这条会话。
    pub(crate) fn subagent_session(&mut self, name: &str, session_id: &str) {
        let session_id = session_id.trim();
        if session_id.is_empty() {
            return;
        }
        self.subagent_status
            .entry(name.to_string())
            .or_default()
            .session_id = Some(session_id.to_string());
        if let Some(id) = self.live_tool_blocks.get(name).copied() {
            blocks::link_session(id, session_id);
        }
    }

    /// 这个子代理的会话（它那一行点下去要切进的）。
    pub(crate) fn subagent_session_of(&self, name: &str) -> Option<&str> {
        self.subagent_status.get(name)?.session_id.as_deref()
    }

    /// 跑着的那个子代理此刻在干什么，压成一行：正在想就露想到哪儿了，正在跑工具就露
    /// 那个工具，正在说话就露说到哪儿了。每帧都在变，一眼看得出它还活着。
    pub(super) fn subagent_peek(&self, name: &str) -> Option<String> {
        let peek = self.subagent_status.get(name)?.peek.trim();
        if peek.is_empty() {
            return None;
        }
        let width = crate::render::command_terminal_width()
            .saturating_sub(48)
            .max(16);
        Some(peek_tail(peek, width))
    }

    /// 这个子代理至此烧了多少（短标，给时间线那一行用）。
    pub(crate) fn subagent_tokens_label(&self, name: &str) -> Option<String> {
        let label = &self.subagent_status.get(name)?.tokens_label;
        (!label.is_empty()).then(|| label.clone())
    }

    /// 这一刻跑着的子代理**一共**烧了多少词元。
    ///
    /// 会话累计（footer 上的 Σ）要等子代理跑完、它的会话落盘才动；而一个子代理能跑
    /// 好几分钟，那几分钟里 Σ 纹丝不动（用户问：这个 token 消耗记录有每步刷新到会话
    /// 累计吗）。跑着的时候先把这份加上去，回合收尾时 Σ 从库里重读、这份清零，不会
    /// 算两遍。
    pub fn running_subagent_tokens(&self) -> u64 {
        self.subagent_tokens.values().copied().sum()
    }
}
