use miyu_core::llm::{ChatContent, ChatMessage};

const IMAGE_TOKEN_ESTIMATE: usize = 765;
const RESERVED_RATIO: f32 = 0.1;
const MIN_RESERVED_TOKENS: usize = 4096;

pub struct OverflowCheck {
    pub context_window: Option<usize>,
    pub reserved_tokens: usize,
    /// 触发水位。裁剪与压缩各传各的——两者同水位时裁剪永远先跑，压缩就
    /// 等不到触发（09-22 实测：三天 compact 0 次、trim 44 次）。
    pub trigger_ratio: f32,
}

impl OverflowCheck {
    pub fn new(
        context_window: Option<usize>,
        trigger_ratio: f32,
        reserved_tokens: Option<usize>,
    ) -> Self {
        let reserved_tokens = reserved_tokens.unwrap_or_else(|| {
            context_window
                .map(|w| ((w as f32 * RESERVED_RATIO) as usize).max(MIN_RESERVED_TOKENS))
                .unwrap_or(MIN_RESERVED_TOKENS)
        });
        Self {
            context_window,
            reserved_tokens,
            trigger_ratio,
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.context_window.is_some()
    }

    #[allow(dead_code)]
    pub fn usable_tokens(&self) -> Option<usize> {
        self.context_window
            .map(|w| w.saturating_sub(self.reserved_tokens))
    }

    pub fn threshold(&self) -> Option<usize> {
        self.context_window
            .map(|w| (w as f32 * self.trigger_ratio).max(1.0) as usize)
    }

    pub fn check_tokens(&self, tokens: usize) -> bool {
        let Some(threshold) = self.threshold() else {
            return false;
        };
        tokens >= threshold
    }

    #[allow(dead_code)]
    pub fn check_estimate(&self, messages: &[ChatMessage]) -> bool {
        let Some(threshold) = self.threshold() else {
            return false;
        };
        estimate_messages_tokens(messages) >= threshold
    }
}

#[allow(dead_code)]
pub fn estimate_messages_tokens(messages: &[ChatMessage]) -> usize {
    let tokens: usize = messages.iter().map(message_tokens).sum();
    tokens.max(1)
}

pub fn estimate_tokens(text: &str) -> usize {
    miyu_base::token_estimate::estimate_tokens(text).max(1)
}

fn text_tokens(text: &str) -> usize {
    miyu_base::token_estimate::estimate_tokens(text)
}

#[allow(dead_code)]
fn message_tokens(msg: &ChatMessage) -> usize {
    let role_tokens = text_tokens(&msg.role);
    let content_tokens = match &msg.content {
        Some(ChatContent::Text(s)) => text_tokens(s),
        Some(ChatContent::Parts(parts)) => parts
            .iter()
            .map(|p| match p {
                miyu_core::llm::ChatContentPart::Text { text } => text_tokens(text),
                miyu_core::llm::ChatContentPart::ImageUrl { .. }
                | miyu_core::llm::ChatContentPart::VideoUrl { .. }
                // PDF 按页计费,一页比一张图贵不少;但这个估算只用来判溢出,
                // 而 PDF 块和图片一样只进本轮请求、不进历史,拿同一个常数
                // 兜着即可——真实数字下一轮就由供应商的 usage 覆盖。
                | miyu_core::llm::ChatContentPart::File { .. } => IMAGE_TOKEN_ESTIMATE,
            })
            .sum(),
        None => 0,
    };
    let tool_tokens = msg
        .tool_calls
        .as_ref()
        .map(|calls| {
            calls
                .iter()
                .map(|c| text_tokens(&c.function.name) + text_tokens(&c.function.arguments))
                .sum::<usize>()
        })
        .unwrap_or(0);
    role_tokens + content_tokens + tool_tokens
}
