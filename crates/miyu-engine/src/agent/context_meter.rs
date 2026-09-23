//! 上下文表：优先用供应商报的真实占用，拿不到才退回本地估算。
//!
//! 本地估算走 o200k BPE，中文按 2 字/token 退化——不同供应商的分词器差得远，
//! 一换池子触发线就系统性偏。真实计数只在回合结束时拿得到，所以「锚点」记在
//! 回合行上，下一次问上下文占用时直接读它。

use crate::agent::*;

/// 一个已完成回合结束时的上下文占用：它最后一次请求的 prompt + completion。
/// `result.usage` 是整轮累计（多轮工具时是各请求之和），拿它当上下文占用会
/// 把同一段前缀数很多遍——这里只认 `last_request_usage`。
pub(in crate::agent) fn context_end_tokens(result: &ChatResult) -> Option<u64> {
    if result.usage_estimated {
        return None;
    }
    let usage = result.last_request_usage.as_ref()?;
    let total = usage.prompt_tokens.saturating_add(usage.completion_tokens);
    (total > 0).then_some(total)
}

impl Agent {
    /// 本地 o200k 估算：渲染整份请求再数 token（v3 之前的唯一口径，现为兜底）。
    pub fn context_tokens_estimate(&self) -> Result<u64> {
        let (messages, _) = self.chat_messages("", "")?;
        let mut tokens = overflow::estimate_messages_tokens(&messages) as u64;
        if self.core.tools_enabled {
            tokens = tokens.saturating_add(self.tool_definition_tokens() as u64);
        }
        Ok(tokens)
    }

    /// 触发线与 footer 用的上下文占用。
    ///
    /// 锚点是「上一次请求结束时」的占用，下一次请求 ≈ 锚点 + 新用户消息 +
    /// 瞬态尾巴；而估算路径本来就是拿空输入渲染的，两者口径一致，所以有锚点
    /// 时直接返回锚点，不做锚点后的增量估算。completion 里含推理 token（多数
    /// 供应商不回放推理），锚点因此略偏高——偏保守的方向，可接受。
    ///
    /// 刻意不校验供应商是否与当前池一致：池按请求轮换，换了端点也照样是一份
    /// 真实计数，比 o200k 硬数中文更接近真值。
    pub fn effective_context_tokens(&self) -> Result<u64> {
        let tokens = self.effective_context_tokens_uncached()?;
        // 落一份到会话记录上(v38):`/session` 列表每行的「当前上下文」直接读它,
        // 不用逐个会话重建 agent 去估。算出来就写,回合收尾/压缩/撤销/快照全覆盖。
        if let Err(error) = self
            .state
            .set_session_context_tokens(&self.state.session_id(), tokens)
        {
            tracing::debug!(%error, "session context tokens not persisted");
        }
        Ok(tokens)
    }

    /// 三种情形：
    ///
    /// - 最新一轮自己就是实测的：直接用它。
    /// - 最新几轮已经结束却没有实测（被打断、失败、用量是估的）：往前找最近一条
    ///   实测，加上它之后那几轮按回放形态估出来的增量。原来这里退回整段估算——
    ///   把整段历史重新拼、重新数，长会话 debug 下每遍一两秒，打断一次要数两遍
    ///   （09-23 用户：「AI 运行过程中打断都挺慢」）；而且 o200k 和供应商的分词器
    ///   差得远，打断后 footer 从实测的 710k 掉回估算的 400k（todolist #88）。
    /// - 锚点之后有一轮还在跑、压缩过、或一条实测都没有：整段估算。回合跑着时
    ///   退回估算是压缩触发线要的语义（见 `ConversationDb::load_context_anchor`）。
    fn effective_context_tokens_uncached(&self) -> Result<u64> {
        let Some((anchor, tail)) = self.state.load_context_anchor_and_tail()? else {
            return self.context_tokens_estimate();
        };
        if tail
            .iter()
            .any(|turn| turn.status == miyu_core::state::TurnStatus::Running)
        {
            return self.context_tokens_estimate();
        }
        if tail.is_empty() {
            // 量尺：测具（compact-quality）按 "context meter" 抓行算估算与真值的
            // 相对误差。整段估算很贵，只在这一行真会写出去时才算——daemon 默认
            // 只记 error，原来每次都白算一遍。
            if tracing::enabled!(tracing::Level::INFO) {
                let estimate = self.context_tokens_estimate()?;
                tracing::info!(estimate, anchor = anchor.tokens, "context meter");
            }
            return Ok(anchor.tokens);
        }
        Ok(anchor
            .tokens
            .saturating_add(self.replay_tokens(&tail) as u64))
    }

    /// 几轮历史按回放形态渲染后的 token 数——和请求里那几轮逐字节一致
    /// （`push_history_turn` 是回放的唯一出口）。
    fn replay_tokens(&self, turns: &[miyu_core::state::Turn]) -> usize {
        let mut messages = Vec::new();
        for turn in turns.iter().filter(|turn| !turn.is_summary) {
            self.push_history_turn(&mut messages, turn);
        }
        if messages.is_empty() {
            return 0;
        }
        overflow::estimate_messages_tokens(&messages)
    }
}
