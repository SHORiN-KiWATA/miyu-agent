//! 提问代理：把模型的提问送到前端，等回答再送回来。
// 兄弟模块的类型互相引用（DaemonState 持有 EventHub、run 记录引用
// ManagerState 等），统一从 mod.rs 的再导出取，免得每个文件维护一份
// 交叉导入清单。
use super::*;
use miyu_base::question::{QuestionAnswers, QuestionRequest, QuestionResponse};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;

/// 记多少道最近了结的题。补发只发生在回合还活着的时候，一轮里问不了几道，
/// 留一批足够；再早的查不到就当「已经不在等了」。
const SETTLED_LOG_LIMIT: usize = 256;

// ── QuestionBroker 提问代理 ──
#[derive(Clone)]
pub(crate) struct QuestionBroker {
    pub(crate) pending: Arc<Mutex<HashMap<String, PendingQuestion>>>,
    /// 最近了结的题各自怎么了结的（09-24）。
    ///
    /// 同一个会话后挂上来的终端要把这一轮从头补一遍，`question.requested` 也
    /// 原样再发一次——终端就把一道早就答完的题当新题弹面板，卡在那儿不往下画，
    /// 按两下 Esc 还会把正跑着的回合掐掉（用户 09-24：答完退出 TUI 再进同一个
    /// 会话，又进了同一个提问界面）。补发那头查这里，把结果附在事件上。
    ///
    /// 锁序：先 `pending` 后 `settled`，两处都这么拿。
    settled: Arc<Mutex<SettledLog>>,
}

#[derive(Default)]
struct SettledLog {
    order: VecDeque<String>,
    outcomes: HashMap<String, QuestionResponse>,
}

impl SettledLog {
    fn record(&mut self, question_id: &str, outcome: QuestionResponse) {
        if self
            .outcomes
            .insert(question_id.to_string(), outcome)
            .is_none()
        {
            self.order.push_back(question_id.to_string());
        }
        while self.order.len() > SETTLED_LOG_LIMIT {
            if let Some(oldest) = self.order.pop_front() {
                self.outcomes.remove(&oldest);
            }
        }
    }
}

pub(crate) struct PendingQuestion {
    pub(crate) run_id: String,
    pub(crate) request: QuestionRequest,
    pub(crate) responder: oneshot::Sender<QuestionResponse>,
}

#[derive(Debug)]
pub(crate) enum AnswerFailure {
    NotFound,
    Invalid(String),
    Gone,
}

impl QuestionBroker {
    pub fn new() -> Self {
        Self {
            pending: Arc::new(Mutex::new(HashMap::new())),
            settled: Arc::new(Mutex::new(SettledLog::default())),
        }
    }

    /// 这道题已经了结了吗？了结了就给出结果；还在等人回答给 `None`。
    ///
    /// 超时那条路不会把题从表里摘掉，只是等的那一头走了（应答端已关闭）：
    /// 那也算了结，结果照回合循环超时时给模型的那样写。查不到的题（记录早被
    /// 挤掉了）同样当作不在等——弹一个答不了的面板比什么都不弹更糟。
    pub(crate) fn settled(&self, question_id: &str) -> Option<QuestionResponse> {
        let pending = self.pending.lock().unwrap();
        if let Some(question) = pending.get(question_id) {
            if !question.responder.is_closed() {
                return None;
            }
            return Some(gone());
        }
        let settled = self.settled.lock().unwrap();
        Some(
            settled
                .outcomes
                .get(question_id)
                .cloned()
                .unwrap_or_else(|| {
                    QuestionResponse::Unavailable("the question is no longer open".to_string())
                }),
        )
    }

    fn record_settled(&self, question_id: &str, outcome: QuestionResponse) {
        self.settled.lock().unwrap().record(question_id, outcome);
    }

    pub(crate) fn insert(
        &self,
        run_id: &str,
        request: QuestionRequest,
        responder: oneshot::Sender<QuestionResponse>,
    ) -> String {
        let mut pending = self.pending.lock().unwrap();
        loop {
            let question_id = random_id("question", 18);
            if !pending.contains_key(&question_id) {
                pending.insert(
                    question_id.clone(),
                    PendingQuestion {
                        run_id: run_id.to_string(),
                        request,
                        responder,
                    },
                );
                return question_id;
            }
        }
    }

    pub(crate) fn answer<F>(
        &self,
        question_id: &str,
        answers: QuestionAnswers,
        before_resume: F,
    ) -> std::result::Result<(), AnswerFailure>
    where
        F: FnOnce(&str, &QuestionAnswers),
    {
        let mut all_pending = self.pending.lock().unwrap();
        let request = all_pending
            .get(question_id)
            .map(|pending| pending.request.clone())
            .ok_or(AnswerFailure::NotFound)?;
        let answers = normalize_answers(&request, answers).map_err(AnswerFailure::Invalid)?;
        let pending = all_pending
            .remove(question_id)
            .ok_or(AnswerFailure::NotFound)?;
        let run_id = pending.run_id;
        if pending.responder.is_closed() {
            self.record_settled(question_id, gone());
            return Err(AnswerFailure::Gone);
        }
        before_resume(&run_id, &answers);
        // 记在 `pending` 锁里：补发那头拿同一把锁查，查不到待答就一定查得到结果。
        self.record_settled(question_id, QuestionResponse::Answered(answers.clone()));
        pending
            .responder
            .send(QuestionResponse::Answered(answers.clone()))
            .map_err(|_| AnswerFailure::Gone)?;
        Ok(())
    }

    pub fn close<F>(
        &self,
        question_id: &str,
        before_resume: F,
    ) -> std::result::Result<(), AnswerFailure>
    where
        F: FnOnce(&str),
    {
        let mut all_pending = self.pending.lock().unwrap();
        let pending = all_pending
            .remove(question_id)
            .ok_or(AnswerFailure::NotFound)?;
        let run_id = pending.run_id;
        if pending.responder.is_closed() {
            self.record_settled(question_id, gone());
            return Err(AnswerFailure::Gone);
        }
        before_resume(&run_id);
        self.record_settled(question_id, QuestionResponse::Closed);
        pending
            .responder
            .send(QuestionResponse::Closed)
            .map_err(|_| AnswerFailure::Gone)?;
        Ok(())
    }

    pub(crate) fn cancel_run(&self, run_id: &str) {
        let cancelled = {
            let mut pending = self.pending.lock().unwrap();
            let ids = pending
                .iter()
                .filter(|(_, question)| question.run_id == run_id)
                .map(|(id, _)| id.clone())
                .collect::<Vec<_>>();
            let mut settled = self.settled.lock().unwrap();
            ids.into_iter()
                .filter_map(|id| {
                    let question = pending.remove(&id)?;
                    settled.record(&id, QuestionResponse::Cancelled);
                    Some(question)
                })
                .collect::<Vec<_>>()
        };
        for pending in cancelled {
            let _ = pending.responder.send(QuestionResponse::Cancelled);
        }
    }
}

/// 等答案的那一头已经走了（回合循环超时放弃）。
fn gone() -> QuestionResponse {
    QuestionResponse::Unavailable("nobody answered within the time limit".to_string())
}

// ── normalize_answers ──
pub(crate) fn normalize_answers(
    request: &QuestionRequest,
    mut answers: QuestionAnswers,
) -> std::result::Result<QuestionAnswers, String> {
    for answer in &mut answers {
        for value in answer {
            *value = value.trim().to_string();
            if value.chars().any(char::is_control) {
                return Err("answers cannot contain control characters".to_string());
            }
        }
    }
    miyu_base::question::validate_answers(request, &answers)
        .map_err(|error| safe_error_message(&error))?;
    Ok(answers)
}

// ── constant_time_eq ──
pub(crate) fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let mut difference = left.len() ^ right.len();
    let length = left.len().max(right.len());
    for index in 0..length {
        let left = left.get(index).copied().unwrap_or(0);
        let right = right.get(index).copied().unwrap_or(0);
        difference |= usize::from(left ^ right);
    }
    difference == 0
}
