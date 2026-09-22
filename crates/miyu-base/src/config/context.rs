//! 上下文窗口、裁剪与压缩的配置项。

use crate::config::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextConfig {
    /// 工具输出的模型侧内联上限(UTF-8 字节)。超限的纯文本输出全文外溢到
    /// 会话级 spill 文件,模型只看头尾预览+取回提示(read_file/rg 按需读回)。
    /// 0 = 关闭外溢。照抄 dsh 默认 50KB。
    #[serde(default = "default_tool_output_spill_bytes")]
    pub tool_output_spill_bytes: usize,
    /// 压缩的触发水位。留空就继承 `trim_at_ratio`。
    ///
    /// 09-23 实测：把它单独调低（0.8）会让压缩触发次数翻倍，而**每压缩一次
    /// 前缀就断一次**，整体命中率反而从 86.3% 掉到 84.0%。要提高命中率是让
    /// 压缩更少更晚，不是更早。所以默认不与 `trim_at_ratio` 分家；分开只是
    /// 为了留一个能单独调的旋钮。
    #[serde(default)]
    pub compact_at_ratio: Option<f32>,
    /// 裁剪(直接删最老的轮)的水位。**只对 `on_overflow = "pop"` 有意义**：
    /// 走压缩的会话根本不跑裁剪（09-23 用户裁定，理由是裁剪会掰断缓存前缀）。
    #[serde(default = "default_trim_at_ratio")]
    pub trim_at_ratio: f32,
    #[serde(default = "default_trim_batch_ratio")]
    pub trim_batch_ratio: f32,
    #[serde(default = "default_on_overflow")]
    pub on_overflow: String,
    #[serde(default = "default_context_window")]
    pub default_context_window: usize,
    /// Watermark that forces a compaction even when the fold-economics gate
    /// would skip it. Must be >= `compact_at_ratio`: between the two, a
    /// compaction still runs but bails out when there is too little to fold
    /// (`MIN_FOLD_TOKENS`); at or above this line it folds regardless.
    #[serde(default = "default_compact_force_ratio")]
    pub compact_force_ratio: f32,
    /// Verbatim tail budget kept outside the summary, in tokens. None derives
    /// min(16384, window/4) for task modes and 8192 for chat mode; the value
    /// is always capped at window/2 so a small window still lands below the
    /// trigger after compaction (re-compaction loop guard).
    #[serde(default)]
    pub compact_tail_tokens: Option<usize>,
    /// 历史工具结果分级剪枝（字符）：落库时超过 chars 的输出改写成
    /// 「头 head + 省略标记 + 尾 tail」。0 = 关闭。默认值抄 dsh 的
    /// compaction-tool-result-pruner（8192 / 4096 / 1024）。
    #[serde(default = "default_tool_result_prune_chars")]
    pub tool_result_prune_chars: usize,
    #[serde(default = "default_tool_result_prune_head_chars")]
    pub tool_result_prune_head_chars: usize,
    #[serde(default = "default_tool_result_prune_tail_chars")]
    pub tool_result_prune_tail_chars: usize,
    /// Summarization requests fork the live conversation (same byte prefix,
    /// same tools + one appended instruction) so the provider prefix cache
    /// pays for re-reading the history — roughly a 10x input-cost saving on
    /// prefix-cached providers (DeepSeek/OpenAI-compatible/Anthropic). Turn
    /// OFF on per-request-billed gateways where cache hits save nothing: the
    /// isolated fallback path sends the history as plain text instead.
    #[serde(default = "default_true")]
    pub compact_cache_reuse: bool,
    /// Files re-read from disk after a compaction, most recently touched
    /// first, inlined behind the checkpoint so the working set survives the
    /// fold. 0 = off.
    #[serde(default = "default_compact_restore_files")]
    pub compact_restore_files: usize,
    /// Per-file token cap; a file over it keeps only its path.
    #[serde(default = "default_compact_restore_file_tokens")]
    pub compact_restore_file_tokens: usize,
    /// Total token budget for one restore pass. Also capped at window/8.
    #[serde(default = "default_compact_restore_total_tokens")]
    pub compact_restore_total_tokens: usize,
    /// Folded turns are written to a markdown transcript under
    /// `state/compact/<session>/` that the model can read back when the
    /// summary lacks a detail.
    #[serde(default = "default_true")]
    pub compact_transcript_export: bool,
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            tool_output_spill_bytes: default_tool_output_spill_bytes(),
            compact_at_ratio: None,
            trim_at_ratio: default_trim_at_ratio(),
            trim_batch_ratio: default_trim_batch_ratio(),
            on_overflow: default_on_overflow(),
            default_context_window: default_context_window(),
            compact_force_ratio: default_compact_force_ratio(),
            compact_tail_tokens: None,
            tool_result_prune_chars: default_tool_result_prune_chars(),
            tool_result_prune_head_chars: default_tool_result_prune_head_chars(),
            tool_result_prune_tail_chars: default_tool_result_prune_tail_chars(),
            compact_cache_reuse: true,
            compact_restore_files: default_compact_restore_files(),
            compact_restore_file_tokens: default_compact_restore_file_tokens(),
            compact_restore_total_tokens: default_compact_restore_total_tokens(),
            compact_transcript_export: true,
        }
    }
}

impl ContextConfig {
    /// 压缩的触发水位。`compact_at_ratio` 留空就继承 `trim_at_ratio`——两者
    /// 分家实测是负收益（09-23：压缩次数翻倍、命中率 86.3% → 84.0%），
    /// 所以默认不分。
    pub fn effective_compact_at_ratio(&self) -> f32 {
        self.compact_at_ratio.unwrap_or(self.trim_at_ratio)
    }
}
