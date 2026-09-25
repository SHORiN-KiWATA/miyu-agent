//! 后台任务唤醒那条「用户消息」的外壳：场所层 `job_wake` 拼，这里拆（09-26 收拢）。
//!
//! ```text
//! <background-job-report>后台子代理「标题」已执行完毕：
//! - job_id: 79ea0b
//! - 任务: …
//! - 状态: 已完成（运行 12 秒）
//! - 日志: /…/79ea0b.log
//! - 子代理结论:
//! 结论正文
//! 这是系统自动触发的跟进，不是用户消息。</background-job-report>
//! ```
//!
//! 终端上那一行铃铛（「子代理完成 79ea0b · 标题」）点开看的就是最后那一段（用户 09-26）：
//! 子代理的结论、失败原因，或者命令输出的结尾。拼和拆用同一份标签与结尾句，改一处两边一起变。

use super::BACKGROUND_JOB_REPORT_TAG;
use serde::{Deserialize, Serialize};

pub const BACKGROUND_JOB_REPORT_CLOSE_TAG: &str = "</background-job-report>";

/// 结果段后面那一句（写给模型的）。拆的时候结果段到它为止。
pub const JOB_REPORT_TRAILER: &str = "这是系统自动触发的跟进，不是用户消息。";

/// 结果段是哪一种，对应唤醒里 `- <标签>:` 那一行。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum JobReportKind {
    /// 子代理交上来的结论。
    Conclusion,
    /// 子代理失败的原因。
    Failure,
    /// 命令输出的最后几行。
    OutputTail,
}

impl JobReportKind {
    const ALL: [Self; 3] = [Self::Conclusion, Self::Failure, Self::OutputTail];

    /// 唤醒原文里的标签。那是模型收到的字，不跟界面语言走。
    pub fn label(self) -> &'static str {
        match self {
            Self::Conclusion => "子代理结论",
            Self::Failure => "子代理失败",
            Self::OutputTail => "输出结尾",
        }
    }
}

/// 从唤醒里拆出来的结果段。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobReportResult {
    pub kind: JobReportKind,
    pub body: String,
}

/// 拆出结果段。不是后台任务报告，或者没有结果段（子代理没交结论、命令没有输出），返回 `None`。
pub fn job_report_result(content: &str) -> Option<JobReportResult> {
    let rest = content
        .trim_start()
        .strip_prefix(BACKGROUND_JOB_REPORT_TAG)?;
    // 「任务」那一行是命令的开头一截，自己可能带换行：从「状态」那一行往后找标签。
    let rest = rest.find("\n- 状态: ").map_or(rest, |at| &rest[at + 1..]);
    let (kind, start) = JobReportKind::ALL
        .into_iter()
        .filter_map(|kind| {
            let line = format!("\n- {}:\n", kind.label());
            rest.find(&line).map(|at| (kind, at + line.len()))
        })
        .min_by_key(|(_, start)| *start)?;
    let body = &rest[start..];
    let body = body
        .rfind(JOB_REPORT_TRAILER)
        .map(|at| &body[..at])
        .or_else(|| body.strip_suffix(BACKGROUND_JOB_REPORT_CLOSE_TAG))
        .unwrap_or(body)
        .trim_end();
    (!body.trim().is_empty()).then(|| JobReportResult {
        kind,
        body: body.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(result: &str) -> String {
        format!(
            "{BACKGROUND_JOB_REPORT_TAG}后台子代理「查资料」已执行完毕：\n- job_id: 79ea0b\n\
             - 任务: 查一下\n- 子代理结论:\n假的\n- 状态: 已完成（运行 3 秒）\n- 日志: /tmp/x.log\n\
             {result}{JOB_REPORT_TRAILER}{BACKGROUND_JOB_REPORT_CLOSE_TAG}"
        )
    }

    #[test]
    fn takes_the_section_after_the_status_line() {
        let conclusion = report("- 子代理结论:\n第一行\n\n第二行\n");
        assert_eq!(
            job_report_result(&conclusion),
            Some(JobReportResult {
                kind: JobReportKind::Conclusion,
                body: "第一行\n\n第二行".to_string(),
            }),
            "任务那一截里长得像标签的不算"
        );
        let tail = report("- 输出结尾:\nBGOUT\n");
        assert_eq!(
            job_report_result(&tail).map(|result| (result.kind, result.body)),
            Some((JobReportKind::OutputTail, "BGOUT".to_string()))
        );
        let failed = report("- 子代理失败:\nmodel refused\n");
        assert_eq!(
            job_report_result(&failed).map(|result| result.kind),
            Some(JobReportKind::Failure)
        );
    }

    #[test]
    fn nothing_to_show_is_none() {
        assert_eq!(job_report_result(&report("")), None);
        assert_eq!(job_report_result(&report("- 输出结尾:\n  \n")), None);
        assert_eq!(job_report_result("- 子代理结论:\n不是唤醒\n"), None);
    }
}
