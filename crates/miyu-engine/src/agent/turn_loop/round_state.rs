//! `chat_with_tools` 一次回合循环里跨轮存活的局部态。
//!
//! 09-17 从 1216 行的函数体里收出来:这些量原本是几十个 `let mut`,散在循环外面,
//! 循环里每一步都要碰其中几个,函数因此拆不动。收成一个结构之后,目录刷新 / 组请求 /
//! 模型往返 / 出错自愈 / 收尾 / 工具执行 / 轮后排队才能各自成为 `impl Agent` 的方法。
//! 字段语义与初值都照搬原来的 `let`,注释也一并搬来。

use super::repeat_gate::ToolRepeatGate;
use crate::agent::*;

pub(super) struct RoundState {
    pub(super) tool_round: usize,
    pub(super) question_rounds: usize,
    /// 活跃轮边界:`messages[replay_start..]` 是本回合自己压进去的部分。
    pub(super) replay_start: usize,
    /// Passive overflow recovery is a one-shot barrier per turn: the
    /// post-compaction retry must not recover another overflow (pi /
    /// opencode / Claude Code all converge on exactly one attempt).
    pub(super) overflow_recovery_attempted: bool,
    pub(super) loaded_tools: std::collections::BTreeSet<String>,
    /// 已经给过契约提示的桩工具:同一工具反复失败不必每次都重发一遍 schema。
    pub(super) contract_hinted: std::collections::BTreeSet<String>,
    pub(super) usage_accumulator: UsageAccumulator,
    /// v7 cache write-grace: provider prefix-cache writes are async, so a
    /// follow-up fired within ~2s can miss the prefix the previous round
    /// just computed (measured on DeepSeek). Track round completion time.
    pub(super) last_round_completed_at: Option<Instant>,
    pub(super) responses_continuation: Option<Box<miyu_core::llm::ResponsesContinuation>>,
    pub(super) continuation_input_start: usize,
    pub(super) continuation_context: Option<(usize, Vec<ChatMessage>)>,
    pub(super) repeat_gate: ToolRepeatGate,
    pub(super) repeat_fused: bool,
}

impl RoundState {
    pub(super) fn new(
        messages: &[ChatMessage],
        replay_start: usize,
        initial_tool_rounds: usize,
        initial_question_rounds: usize,
        loaded_tools: std::collections::BTreeSet<String>,
    ) -> Self {
        Self {
            tool_round: initial_tool_rounds,
            question_rounds: initial_question_rounds,
            replay_start,
            overflow_recovery_attempted: false,
            loaded_tools,
            contract_hinted: std::collections::BTreeSet::new(),
            usage_accumulator: UsageAccumulator::default(),
            last_round_completed_at: None,
            responses_continuation: None,
            continuation_input_start: messages.len(),
            continuation_context: None,
            repeat_gate: ToolRepeatGate::new(),
            repeat_fused: false,
        }
    }
}
