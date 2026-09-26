//! 摘要结构校验（09-25，opencode 调研 3.2 第 1 条）。
//!
//! fork 摘要带着完整的人格 system 发出去，模型偶尔会顺着对话往下聊。09-25 前两条摘要
//! 路径都只判空，那段闲聊会被当成摘要落库，折叠掉的历史从此只剩一句「好的」。
//!
//! 判据取模板自己的 `## ` 标题，至少命中两个才算摘要。标题从当前生效的模板里读，
//! `MIYU_COMPACT_PROMPT_FILE` 换了底稿也跟着换；底稿里不足两个标题就不校验。
//! 不合格时逐级降：fork 同前缀追加一句纠正再要一次 → 隔离路径（自带一次重试）→
//! 机械兜底（手动 `/compact` 报错）。模板本身不动。

/// 至少命中几个模板标题才算摘要。只要一个的话，闲聊里碰巧写了个 `## 结论` 式的标题
/// 也可能撞上；九节模板里两个是「确实照着结构写了」的最低信号。
const MIN_MATCHED_HEADINGS: usize = 2;

/// fork 摘要第一次不合格时追加的那句。模型可见的机械文本一律英文短句（AGENTS §1.5）。
pub(in crate::agent) const SUMMARY_CORRECTION: &str = "That reply does not follow the summary structure. Write the summary now, using the section headings exactly as given in the summarization instructions.";

/// 当前模板的二级标题（小写、去掉首尾空白）。
pub(in crate::agent) struct SummaryStructure {
    headings: Vec<String>,
}

impl SummaryStructure {
    pub fn from_template(system_prompt: &str) -> Self {
        let headings = system_prompt
            .lines()
            .filter_map(heading_text)
            .collect::<Vec<_>>();
        Self { headings }
    }

    /// 这段输出是不是照模板写的摘要。
    pub fn accepts(&self, summary: &str) -> bool {
        if self.headings.len() < MIN_MATCHED_HEADINGS {
            return true;
        }
        let mut matched = summary
            .lines()
            .filter_map(heading_text)
            .filter(|heading| self.headings.contains(heading))
            .collect::<Vec<_>>();
        matched.sort();
        matched.dedup();
        matched.len() >= MIN_MATCHED_HEADINGS
    }
}

/// `## 标题` 那一行的标题文字，归一成小写。模型偶尔给标题加粗或补冒号，照样认。
fn heading_text(line: &str) -> Option<String> {
    let text = line.trim_start().strip_prefix("## ")?;
    let text = text.trim_matches(|c: char| c == '*' || c == ':' || c.is_whitespace());
    (!text.is_empty()).then(|| text.to_lowercase())
}
