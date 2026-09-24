//! 思考停了、别的也还没来：live 区只留转轮。
//!
//! 有的线路把整段工具调用扣在手里，模型写完才一口气放出来（09-24 实测：
//! opencodego 的 deepseek-v4.1-flash 思考停后 1.5–3.9s 一个字节都没有，参数随后
//! 0.15s 倒完；bigmodel 的 glm-5.3-flash 静默 12s，参数只有一个分片）。这段时间
//! 模型其实在写参数，屏上却一直是「思考中」在走秒，「准备编辑」只闪几十毫秒，
//! 用户看不见（todolist：「有的时候 AI 在准备编辑文件，但是我看不见准备编辑这个
//! tag 行」）。边写边发的线路（官方 DeepSeek、ririxin 的 glm-5.2）不受影响：
//! 思考一停工具名就到，准备态照常接上。
//!
//! 于是思考超过 [`REASONING_STALL`] 没动静，就把它收成「已思考」、秒数停在最后
//! 一片思考上，live 区只剩转轮，和刚发出去还没回音时一个样（用户 09-24：「提示
//! 要为空，就是只有 spinner」）。

use super::*;
use miyu_core::llm::ChatStreamKind;

/// 思考停多久算「想完了」。
///
/// 09-24 实测思考中途最长只停过 0.17s；把工具调用扣着的线路静默 1.5–13.7s。
/// 取 1.5s，两边都隔得开。真有线路在想到一半时卡这么久，代价是那段思考被收成
/// 两步「已思考」。
pub(crate) const REASONING_STALL: Duration = Duration::from_millis(1500);

impl StreamRenderer {
    /// 转轮每一帧先问一句：思考是不是停住了。停住了就收进时间线。
    ///
    /// 只管时间线那几面（全屏、shellhook、单次 `miyu "…"`）：没有时间线的面收了
    /// 思考转轮就停了，那儿也没有「准备编辑」那一行。
    pub(crate) fn settle_stalled_reasoning(&mut self, now: Instant) -> anyhow::Result<()> {
        let Some(last) = self.reasoning_last_delta_at else {
            return Ok(());
        };
        if now.saturating_duration_since(last) < REASONING_STALL
            || !self.reasoning_is_waiting_alone()
        {
            return Ok(());
        }
        self.reasoning_last_delta_at = None;
        // 起点照这段思考真正开始的时刻记：收进来的这一刻比它结束晚了一截空档，
        // 从此刻倒推会把起点算晚，`Worked for` 就少了这截。
        if let Some(started) = self.reasoning_started_at {
            self.timeline.note_started_at(started);
        }
        self.freeze_reasoning_elapsed_at(last);
        self.finalize_reasoning_summary()
    }

    /// 这一刻屏上正在转的只有「思考中」这一行。
    ///
    /// 准备态、跑着的工具、钉死的转轮文案（压缩上下文）都各自占着那一行；正文
    /// 已经开始流的话思考早就收了。
    fn reasoning_is_waiting_alone(&self) -> bool {
        self.timeline_enabled()
            && self.captures_reasoning()
            && self.reasoning_mode != ReasoningDisplayMode::Hidden
            && self.mode == Some(ChatStreamKind::Reasoning)
            && (self.reasoning_title.is_some() || !self.reasoning_text.is_empty())
            && self.wait_spinner.is_some()
            && self.custom_waiting_phase.is_none()
            && self.tool_preparing.is_none()
            && self.preparing_question_started_at.is_none()
            && self.tool_stats.is_empty()
    }
}
