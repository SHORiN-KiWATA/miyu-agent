//! 实时那一轮说完（或被打断）时，正文末尾那行 `✻ 模型 · 处理了多久 · 几点完成`（用户 09-26）。
//!
//! 长相在 `render::timeline::turn_end_frame`，回放那条路在 `history_replay.rs`。只有常驻 REPL
//! （全屏、inline）画它：一次性 / shellhook 没有活动区，走不到这里。

use crate::cli::repl::tail::*;
use crate::cli::*;

/// 落一行收尾，返回画没画。用时从库里这一轮开始的时刻算起——回放也是从它算的，两边才对得上
/// （转轮起转比它晚零点几秒，短回合会差出一整秒）；库读不到才用活动区的回合计时（熄转轮之前
/// 取）。轮号不知道就不画：动词按轮号挑，回放时才对得上。
pub(in crate::cli) fn show_turn_end(
    paths: &MiyuPaths,
    live: &mut LiveReplTail,
    turn_id: Option<&str>,
    model: Option<&str>,
    elapsed: Option<std::time::Duration>,
    interrupted: bool,
) -> Result<bool> {
    let Some(turn_id) = turn_id.filter(|turn_id| !turn_id.is_empty()) else {
        return Ok(false);
    };
    let Some(elapsed) = turn_started_instant(paths, turn_id)
        .map(|started| started.elapsed())
        .or(elapsed)
    else {
        return Ok(false);
    };
    let end = render::timeline::TurnEnd {
        turn_id,
        model,
        elapsed,
        finished_at: chrono::Local::now(),
        interrupted,
    };
    live.apply_output_frame(render::timeline::turn_end_frame(&end).as_bytes())?;
    Ok(true)
}

/// 收尾那行的模型位置写什么。混合模型池开着「本次供应商 / 模型」时写「供应商 / 模型」，原来单独
/// 那一行就不画了（用户 09-26：同一个模型名写了两遍）；否则只写模型。供应商不知道时也只写模型。
pub(in crate::cli) fn turn_end_model(
    paths: &MiyuPaths,
    config: &AppConfig,
    session_id: &str,
    provider: Option<&str>,
    model: Option<&str>,
) -> Option<String> {
    let model = model.map(str::trim).filter(|model| !model.is_empty())?;
    let provider = provider
        .map(str::trim)
        .filter(|provider| !provider.is_empty());
    let session_config = crate::cli::footer::footer_config_for_session(paths, config, session_id);
    match provider {
        Some(provider)
            if crate::cli::model_cmds::show_mixed_model_endpoint(&session_config, true) =>
        {
            Some(crate::cli::model_cmds::mixed_model_endpoint_label(
                provider, model, None,
            ))
        }
        _ => Some(model.to_string()),
    }
}

/// 库里这一轮开始的时刻，换算成本进程的 `Instant`（计时要的是单调钟）。
pub(in crate::cli) fn turn_started_instant(
    paths: &MiyuPaths,
    turn_id: &str,
) -> Option<std::time::Instant> {
    let started = StateStore::new(paths)
        .ok()?
        .turn_started_at(turn_id)
        .ok()
        .flatten()?;
    let started = chrono::DateTime::parse_from_rfc3339(&started).ok()?;
    let ago = (chrono::Utc::now() - started.with_timezone(&chrono::Utc))
        .to_std()
        .unwrap_or_default();
    std::time::Instant::now().checked_sub(ago)
}
