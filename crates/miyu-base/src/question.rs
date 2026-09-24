use anyhow::{bail, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;

const MAX_QUESTIONS: usize = 8;
const MAX_OPTIONS: usize = 8;
const MAX_HEADER_CHARS: usize = 30;
const MAX_QUESTION_CHARS: usize = 1_000;
const MAX_LABEL_CHARS: usize = 80;
const MAX_DESCRIPTION_CHARS: usize = 400;
pub const MAX_CUSTOM_ANSWER_CHARS: usize = 4_000;

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct QuestionOption {
    pub label: String,
    pub description: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct QuestionPrompt {
    pub header: String,
    pub question: String,
    pub options: Vec<QuestionOption>,
    #[serde(default)]
    pub multiple: bool,
    #[serde(default = "default_true")]
    pub custom: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct QuestionRequest {
    pub questions: Vec<QuestionPrompt>,
}

pub type QuestionAnswers = Vec<Vec<String>>;

// 可序列化是给「补发时附上结果」用的（09-24）：daemon 把一道早已了结的题连同
// 它的结果一起补发给后挂上来的终端，两头认同一份编码，免得各写一套字段名。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "status", content = "detail", rename_all = "snake_case")]
pub enum QuestionResponse {
    Answered(QuestionAnswers),
    Closed,
    Cancelled,
    Unavailable(String),
}

#[derive(Debug)]
pub struct QuestionCancelled;

impl fmt::Display for QuestionCancelled {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("question cancelled by user")
    }
}

impl Error for QuestionCancelled {}

pub fn is_question_cancelled(error: &anyhow::Error) -> bool {
    error.downcast_ref::<QuestionCancelled>().is_some()
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct QuestionExchange {
    pub questions: Vec<QuestionPrompt>,
    pub answers: QuestionAnswers,
    pub answered_at: String,
}

impl QuestionExchange {
    pub fn new(request: QuestionRequest, answers: QuestionAnswers) -> Result<Self> {
        request.validate()?;
        validate_answers(&request, &answers)?;
        Ok(Self {
            questions: request.questions,
            answers,
            answered_at: Utc::now().to_rfc3339(),
        })
    }
}

/// 把被写成「JSON 字符串」的数组还原成数组。
///
/// 模型经常把结构化参数序列化一次再传。别的工具由
/// `tools::registry::coerce_declared_shapes` 在注册表分发处统一还原，但
/// `ask_question` 要拿到交互通道，是**唯一一个完全绕过注册表**的工具（见
/// `agent::turn_loop` 里按名字的特判），那条修复到不了这里。
///
/// 保守处理：只有当值确实是以 `[` 开头、且能解析成数组的字符串时才换。
fn restore_array_in_place(object: &mut serde_json::Map<String, serde_json::Value>, key: &str) {
    let Some(serde_json::Value::String(text)) = object.get(key) else {
        return;
    };
    let text = text.trim();
    if !text.starts_with('[') {
        return;
    }
    if let Some(array) = serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .filter(serde_json::Value::is_array)
    {
        object.insert(key.to_string(), array);
    }
}

impl QuestionRequest {
    pub fn parse(arguments: &str) -> Result<Self> {
        let mut value: serde_json::Value = serde_json::from_str(arguments)?;
        if let Some(object) = value.as_object_mut() {
            restore_array_in_place(object, "questions");
            // 嵌套一层的 options 同样会被序列化成字符串
            if let Some(questions) = object.get_mut("questions").and_then(|q| q.as_array_mut()) {
                for question in questions {
                    if let Some(question) = question.as_object_mut() {
                        restore_array_in_place(question, "options");
                    }
                }
            }
        }
        let request: Self = serde_json::from_value(value)?;
        request.validate()?;
        Ok(request)
    }

    pub fn validate(&self) -> Result<()> {
        if self.questions.is_empty() {
            bail!("questions must contain at least one question");
        }
        if self.questions.len() > MAX_QUESTIONS {
            bail!("questions cannot contain more than {MAX_QUESTIONS} questions");
        }
        for (question_index, question) in self.questions.iter().enumerate() {
            validate_text(&question.header, "header", question_index, MAX_HEADER_CHARS)?;
            validate_text(
                &question.question,
                "question",
                question_index,
                MAX_QUESTION_CHARS,
            )?;
            if question.options.len() > MAX_OPTIONS {
                bail!(
                    "questions[{question_index}].options cannot contain more than {MAX_OPTIONS} options"
                );
            }
            if question.options.is_empty() && !question.custom {
                bail!(
                    "questions[{question_index}] must provide an option or allow a custom answer"
                );
            }
            let mut labels = BTreeSet::new();
            for (option_index, option) in question.options.iter().enumerate() {
                validate_text(
                    &option.label,
                    &format!("options[{option_index}].label"),
                    question_index,
                    MAX_LABEL_CHARS,
                )?;
                if option.description.chars().any(char::is_control) {
                    bail!(
                        "questions[{question_index}].options[{option_index}].description contains control characters"
                    );
                }
                if option.description.chars().count() > MAX_DESCRIPTION_CHARS {
                    bail!(
                        "questions[{question_index}].options[{option_index}].description is too long"
                    );
                }
                if !labels.insert(option.label.trim()) {
                    bail!("questions[{question_index}] contains duplicate option labels");
                }
            }
        }
        Ok(())
    }

    pub fn needs_review(&self) -> bool {
        self.questions.len() > 1 || self.questions.iter().any(|question| question.multiple)
    }
}

pub fn validate_answers(request: &QuestionRequest, answers: &QuestionAnswers) -> Result<()> {
    if answers.len() != request.questions.len() {
        bail!("answer count does not match question count");
    }
    for (index, (question, answer)) in request.questions.iter().zip(answers).enumerate() {
        if answer.is_empty() {
            bail!("question {index} is unanswered");
        }
        if !question.multiple && answer.len() != 1 {
            bail!("question {index} only accepts one answer");
        }
        let option_labels = question
            .options
            .iter()
            .map(|option| option.label.as_str())
            .collect::<BTreeSet<_>>();
        let mut unique = BTreeSet::new();
        for value in answer {
            let value = value.trim();
            if value.is_empty() {
                bail!("question {index} contains an empty answer");
            }
            if value.chars().count() > MAX_CUSTOM_ANSWER_CHARS {
                bail!("question {index} contains an answer that is too long");
            }
            if !option_labels.contains(value) && !question.custom {
                bail!("question {index} does not allow custom answers");
            }
            if !unique.insert(value) {
                bail!("question {index} contains duplicate answers");
            }
        }
    }
    Ok(())
}

pub fn answered_tool_output(exchange: &QuestionExchange) -> String {
    let answers = exchange
        .questions
        .iter()
        .zip(&exchange.answers)
        .map(|(question, selected)| {
            json!({
                "header": question.header,
                "question": question.question,
                "answers": selected,
            })
        })
        .collect::<Vec<_>>();
    serde_json::to_string(&json!({
        "status": "answered",
        "answers": answers,
        "instruction": "Continue using these user-provided answers. Do not ask the same questions again.",
    }))
    .unwrap_or_else(|_| "{\"status\":\"answered\"}".to_string())
}

pub fn unavailable_tool_output(reason: &str) -> String {
    serde_json::to_string(&json!({
        "status": "unavailable",
        "reason": reason,
        "instruction": "Interactive input is unavailable. Continue safely without assuming an answer.",
    }))
    .unwrap_or_else(|_| "{\"status\":\"unavailable\"}".to_string())
}

pub fn closed_tool_output() -> String {
    serde_json::to_string(&json!({
        "status": "closed",
        "instruction": "The user closed the answer interface without providing answers. Continue the current response without assuming an answer.",
    }))
    .unwrap_or_else(|_| "{\"status\":\"closed\"}".to_string())
}

pub fn assistant_exchange_text(exchange: &QuestionExchange) -> String {
    let mut output = String::from("Clarification questions:");
    for (index, question) in exchange.questions.iter().enumerate() {
        output.push_str(&format!(
            "\n{}. [{}] {}",
            index + 1,
            question.header,
            question.question.trim()
        ));
        for option in &question.options {
            output.push_str(&format!("\n   - {}", option.label));
            if !option.description.is_empty() {
                output.push_str(&format!(": {}", option.description));
            }
        }
        if question.custom {
            output.push_str("\n   - custom answer allowed");
        }
    }
    output
}

pub fn user_exchange_text(exchange: &QuestionExchange) -> String {
    let mut output = String::from("Clarification answers:");
    for (question, answers) in exchange.questions.iter().zip(&exchange.answers) {
        output.push_str(&format!("\n- {}: {}", question.header, answers.join(", ")));
    }
    output
}

fn validate_text(value: &str, field: &str, question_index: usize, max_chars: usize) -> Result<()> {
    if value.trim().is_empty() {
        bail!("questions[{question_index}].{field} cannot be empty");
    }
    if value.trim() != value {
        bail!("questions[{question_index}].{field} cannot have surrounding whitespace");
    }
    if value.chars().any(char::is_control) {
        bail!("questions[{question_index}].{field} contains control characters");
    }
    if value.chars().count() > max_chars {
        bail!("questions[{question_index}].{field} is too long");
    }
    Ok(())
}

fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> QuestionRequest {
        QuestionRequest {
            questions: vec![QuestionPrompt {
                header: "范围".to_string(),
                question: "修改哪些文件？".to_string(),
                options: vec![QuestionOption {
                    label: "全部".to_string(),
                    description: "修改全部相关文件".to_string(),
                }],
                multiple: false,
                custom: true,
            }],
        }
    }

    #[test]
    fn request_accepts_option_and_custom_answers() {
        let request = request();
        assert!(validate_answers(&request, &vec![vec!["全部".to_string()]]).is_ok());
        assert!(validate_answers(&request, &vec![vec!["仅配置".to_string()]]).is_ok());
    }

    #[test]
    fn single_question_rejects_multiple_answers() {
        let request = request();
        assert!(validate_answers(
            &request,
            &vec![vec!["全部".to_string(), "仅配置".to_string()]]
        )
        .is_err());
    }

    #[test]
    fn request_rejects_duplicate_labels() {
        let mut request = request();
        let duplicate = request.questions[0].options[0].clone();
        request.questions[0].options.push(duplicate);
        assert!(request.validate().is_err());
    }

    #[test]
    fn request_rejects_terminal_control_sequences() {
        let mut request = request();
        request.questions[0].question = "选择\u{1b}[2J范围".to_string();
        assert!(request.validate().is_err());
    }

    #[test]
    fn request_rejects_surrounding_label_whitespace() {
        let mut request = request();
        request.questions[0].options[0].label = " 全部 ".to_string();
        assert!(request.validate().is_err());
    }

    #[test]
    fn persisted_assistant_exchange_keeps_option_meaning() {
        let exchange = QuestionExchange::new(request(), vec![vec!["全部".to_string()]]).unwrap();
        let text = assistant_exchange_text(&exchange);
        assert!(text.contains("全部: 修改全部相关文件"));
        assert!(text.contains("custom answer allowed"));
    }

    #[test]
    fn closed_output_continues_without_an_answer() {
        let output: serde_json::Value = serde_json::from_str(&closed_tool_output()).unwrap();
        assert_eq!(output["status"], "closed");
        assert!(output["instruction"]
            .as_str()
            .is_some_and(|instruction| instruction.contains("without providing answers")));
    }
}

#[cfg(test)]
mod parse_tests {
    use super::*;

    /// 模型经常把结构化参数序列化成字符串再传（见
    /// `tools::registry::coerce_declared_shapes` 的文档注释）。`ask_question`
    /// 不走注册表分发，所以这条路上必须自己还原。
    #[test]
    fn questions_serialized_as_a_json_string_are_restored() {
        let arguments = r#"{"questions":"[{\"header\":\"选项\",\"question\":\"选哪个？\",\"options\":[{\"label\":\"A\",\"description\":\"甲\"}]}]"}"#;
        let request = QuestionRequest::parse(arguments).expect("字符串形态的 questions 应能解析");
        assert_eq!(request.questions.len(), 1);
        assert_eq!(request.questions[0].header, "选项");
    }

    /// 嵌套一层的 options 也会被序列化成字符串。
    #[test]
    fn nested_options_serialized_as_a_json_string_are_restored() {
        let arguments = r#"{"questions":[{"header":"选项","question":"选哪个？","options":"[{\"label\":\"A\",\"description\":\"甲\"}]"}]}"#;
        let request = QuestionRequest::parse(arguments).expect("字符串形态的 options 应能解析");
        assert_eq!(request.questions[0].options.len(), 1);
        assert_eq!(request.questions[0].options[0].label, "A");
    }

    /// 该报错的还是要报错：不能因为兼容就把乱七八糟的输入也吞下去。
    #[test]
    fn a_string_that_is_not_an_array_still_fails() {
        let arguments = r#"{"questions":"随便写的一句话"}"#;
        assert!(QuestionRequest::parse(arguments).is_err());
    }

    /// 正常形态不能因为兼容而走样。
    #[test]
    fn questions_as_a_real_array_still_parse() {
        let arguments = r#"{"questions":[{"header":"选项","question":"选哪个？","options":[{"label":"A","description":"甲"}]}]}"#;
        let request = QuestionRequest::parse(arguments).expect("数组形态的 questions 应能解析");
        assert_eq!(request.questions.len(), 1);
    }
}
