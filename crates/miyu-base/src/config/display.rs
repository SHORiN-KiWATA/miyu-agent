//! 终端/WebUI 显示相关配置。
//!
//! `DisplayConfig` 的反序列化是手写的（`RawDisplayConfig` 先收旧键名再折算），
//! 所以定义、原始形态与 `Default` 三件放在一起。

use crate::config::*;

#[derive(Debug, Clone, Serialize)]
pub struct DisplayConfig {
    #[serde(default = "default_display_language")]
    pub language: String,
    /// 思考那一步**出来就是展开的**吗。
    ///
    /// 09-17 之前这儿是 `reasoning: "hidden" | "summary" | "full"` 三档。三值
    /// 枚举装着两件不相干的事（出不出现、展不展开），而且「完整」那一档在
    /// 非全屏还顺手换了整条渲染路径——用户 09-17 拍板拆成布尔、隐藏档删掉：
    /// 「是否展开思考内容 / 是否展开工具内容 / 是否缩起成 Worked for，这样更
    /// 简洁」。思考以后要从时间线里搬出去，这三位之间不能有耦合。
    #[serde(default)]
    pub expand_reasoning: bool,
    /// 工具那一步**出来就是展开的**吗。见 [`DisplayConfig::expand_reasoning`]。
    #[serde(default)]
    pub expand_tool_calls: bool,
    #[serde(default = "default_true")]
    pub readable_tool_names: bool,
    #[serde(default)]
    pub show_token_usage: bool,
    #[serde(default = "default_mixed_model_endpoint_display")]
    pub mixed_model_endpoint_display: String,
    /// 命令那一步抬头底下露几行**命令**。09-17 之前露的是命令输出,现在输出
    /// 退到点开里;键名不改,改了用户设过的值会掉回默认。
    #[serde(default = "default_command_output_lines")]
    pub command_output_lines: usize,
    /// 思考进行中「思考中」抬头底下滚着露最近几行；想完收成一行
    /// `已思考 · N 词元 · Xs`。0 = 只留抬头不露正文。`expand_reasoning` 开着
    /// 时不走这个窗：全屏 TUI 直接把正文展开、shellhook 边想边往下流。
    #[serde(default = "default_thinking_scroll_lines")]
    pub thinking_scroll_lines: usize,
    /// 跨会话 AI 消息（09-23，收到的那条与发出去的那次工具调用）先露几行正文，
    /// 其余点开看。0 = 只留抬头。
    #[serde(default = "default_cross_session_preview_lines")]
    pub cross_session_preview_lines: usize,
    /// 一段过程跑完收成一行 `Worked for …` 吗。关掉就每一步就地留着——和 shell
    /// 无缝对话那条路一个样子（用户 todolist:21）。
    ///
    /// 它**只管收不收段**：步骤照样能点开、详情照样收在块里。09-17 之前它还
    /// 顺手把那些步变成点不开、正文铺一地，那是 `commit_immediately` 一位管了
    /// 三件事的副产品（用户：「即使不自动收起过程为 true，也不应该以 tag 行下
    /// 预览的形式出现 tag 行的内容」）。
    #[serde(default = "default_true")]
    pub fold_timeline: bool,
    /// 空会话时在输入框上方画 MIYU banner（渐变艺术字 + 星空 + 模式行）。
    /// 关掉就只剩输入框。艺术字可用 `config/banner.txt` 替换。
    #[serde(default = "default_true")]
    pub banner: bool,
    /// 这个版本不认识的显示项，原样留着写回。见 [`AppConfig::extra`]。
    #[serde(flatten, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
struct RawDisplayConfig {
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    expand_reasoning: Option<bool>,
    #[serde(default)]
    expand_tool_calls: Option<bool>,
    #[serde(default)]
    fold_timeline: Option<bool>,
    // —— 以下都是旧键，只读不写：读到就折算成上面那三位。——
    #[serde(default)]
    reasoning: Option<String>,
    #[serde(default)]
    tool_calls: Option<String>,
    #[serde(default)]
    show_reasoning: Option<bool>,
    #[serde(default)]
    reasoning_mode: Option<String>,
    #[serde(default)]
    show_tool_details: Option<bool>,
    #[serde(default)]
    readable_tool_names: Option<bool>,
    #[serde(default)]
    show_token_usage: Option<bool>,
    #[serde(default)]
    show_mixed_model_endpoint: Option<bool>,
    #[serde(default)]
    mixed_model_endpoint_display: Option<String>,
    #[serde(default)]
    command_output_lines: Option<usize>,
    #[serde(default)]
    thinking_scroll_lines: Option<usize>,
    #[serde(default)]
    cross_session_preview_lines: Option<usize>,
    #[serde(default)]
    keep_timeline_open: Option<bool>,
    #[serde(default)]
    banner: Option<bool>,
    #[serde(flatten, default)]
    extra: BTreeMap<String, serde_json::Value>,
}

impl<'de> Deserialize<'de> for DisplayConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = RawDisplayConfig::deserialize(deserializer)?;
        // 旧的三值档折成「展不展开」：`full` = 展开，别的（含已删掉的 `hidden`）
        // = 收起。用户 09-17 拍板删隐藏档，配过 hidden 的会被当成「显示 + 收起」。
        let expand_reasoning = raw.expand_reasoning.unwrap_or_else(|| {
            // 更老的 `show_reasoning = false` 说的是「隐藏」。隐藏档已删，
            // 落到「显示 + 收起」。
            if raw.show_reasoning == Some(false) {
                return false;
            }
            raw.reasoning
                .or(raw.reasoning_mode)
                .is_some_and(|legacy| legacy.trim().eq_ignore_ascii_case("full"))
        });
        let expand_tool_calls = raw.expand_tool_calls.unwrap_or_else(|| {
            match raw.tool_calls {
                Some(legacy) => legacy.trim().eq_ignore_ascii_case("full"),
                // 更老的那一版只有一个「要不要详细」的开关。
                None => raw.show_tool_details == Some(true),
            }
        });
        // `keep_timeline_open` 是反过来说的同一件事。
        let fold_timeline = raw
            .fold_timeline
            .or_else(|| raw.keep_timeline_open.map(|keep| !keep))
            .unwrap_or(true);
        Ok(Self {
            language: raw.language.unwrap_or_else(default_display_language),
            expand_reasoning,
            expand_tool_calls,
            readable_tool_names: raw.readable_tool_names.unwrap_or_else(default_true),
            show_token_usage: raw.show_token_usage.unwrap_or(false),
            mixed_model_endpoint_display: raw.mixed_model_endpoint_display.unwrap_or_else(|| {
                match raw.show_mixed_model_endpoint {
                    Some(true) => "all".to_string(),
                    Some(false) => "off".to_string(),
                    None => default_mixed_model_endpoint_display(),
                }
            }),
            command_output_lines: raw
                .command_output_lines
                .unwrap_or_else(default_command_output_lines),
            thinking_scroll_lines: raw
                .thinking_scroll_lines
                .unwrap_or_else(default_thinking_scroll_lines),
            cross_session_preview_lines: raw
                .cross_session_preview_lines
                .unwrap_or_else(default_cross_session_preview_lines),
            fold_timeline,
            banner: raw.banner.unwrap_or(true),
            extra: raw.extra,
        })
    }
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            language: default_display_language(),
            expand_reasoning: false,
            expand_tool_calls: false,
            readable_tool_names: default_true(),
            show_token_usage: false,
            mixed_model_endpoint_display: default_mixed_model_endpoint_display(),
            command_output_lines: default_command_output_lines(),
            thinking_scroll_lines: default_thinking_scroll_lines(),
            cross_session_preview_lines: default_cross_session_preview_lines(),
            fold_timeline: true,
            banner: true,
            extra: BTreeMap::new(),
        }
    }
}
