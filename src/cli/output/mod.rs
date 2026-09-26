//! 程序驱动形态的输出层:对外事件 schema、JSON 回合客户端、一次性 JSON 运行器。

pub mod conclusion;
pub mod event;
pub mod turn_client;

use crate::cli::args::OutputFormat;
use crate::cli::exit_code::exit_with;
use anyhow::Result;
use conclusion::run_turn_to_conclusion;
use event::PublicEvent;
use miyu_base::paths::MiyuPaths;
use std::io::Write;
use turn_client::{QuestionPolicy, TurnOutcome, TurnRequest};

/// `miyu ask --output-format json|stream-json` 的一次性回合:stream-json 逐
/// 事件一行,json 只打终态一行;两种都只往 stdout 写 JSON,别的一个字节
/// 不漏。失败按 [`event::ErrorKind`] 映射退出码,错误行也走 stdout(宿主
/// 只解析一个流)。派了子代理就等它们的报告回完，终态带的是结论（09-26）。
pub async fn run_json_one_shot(
    paths: &MiyuPaths,
    request: TurnRequest,
    format: OutputFormat,
) -> Result<()> {
    let stdout = std::io::stdout();
    let streaming = format == OutputFormat::StreamJson;
    let emit = |event: PublicEvent| {
        if streaming {
            let mut out = stdout.lock();
            let _ = writeln!(out, "{}", event.to_line(None));
            let _ = out.flush();
        }
    };
    let outcome =
        run_turn_to_conclusion(paths, request, QuestionPolicy::AutoClose, None, emit).await?;
    let mut out = stdout.lock();
    match outcome {
        TurnOutcome::Completed(done) => {
            writeln!(out, "{}", done.to_line(None))?;
            out.flush()?;
            Ok(())
        }
        TurnOutcome::Failed {
            kind,
            message,
            session_id,
        } => {
            let event = PublicEvent::Error {
                kind,
                message: message.clone(),
                session_id,
            };
            writeln!(out, "{}", event.to_line(None))?;
            out.flush()?;
            // 错误已经以 JSON 打给宿主了,main 那行文字提示留空,只留退出码。
            Err(exit_with(kind.exit_code(), String::new()))
        }
    }
}
