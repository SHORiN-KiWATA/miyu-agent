//! token 用量查询工具:读 usage-history.jsonl 聚合出中文摘要。
//! 智能体主工具集(终端/WebUI/shell hook)与 QQ 平台工具集共用这套
//! 实现,两边只是 usage-history 路径的来源不同。

use super::{ToolRegistry, ToolSpec};
use anyhow::{Context, Result};
use miyu_core::state::usage;
use serde_json::{json, Value};
use std::path::PathBuf;

/// 描述与 schema 在智能体侧与平台侧(src/platforms/tool.rs)两处注册共用,
/// 收敛成一份防止漂移。
/// 全局那件：整个 Miyu 的用量台账。本会话那件是 [`SESSION_DESCRIPTION`]。
pub const DESCRIPTION: &str = "Token usage across ALL of Miyu: this agent, messaging platforms, background jobs and subagents added up. Totals, request count, cache hit rate, per-source model breakdown. For THIS conversation's own usage use query_session_token_usage instead.";

pub fn parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "range": {
                "type": "string",
                "enum": ["1d", "7d", "30d", "all"],
                "description": "Time range, defaults to 1d (rolling 24h)."
            }
        },
        "additionalProperties": false
    })
}

/// 「这个会话烧了多少」的取值器。
///
/// 工具在装配层注册时还不知道自己会挂在哪个会话上（`compose_registry` 只认
/// 人格与工具面）。会话是回合开始时才定的，所以由 Agent 侧把这个闭包塞进来
/// ——`Agent` 每轮新建，闭包捕获的就是这一轮的会话（用户 09-22 提议：不往
/// 提示词里加常驻字段，改成给这件现成的工具加一个 scope）。
pub type SessionUsageFn = std::sync::Arc<dyn Fn() -> Option<SessionUsage> + Send + Sync>;

/// 一个会话此刻的用量。
pub struct SessionUsage {
    /// 供应商最近一次报回来的上下文占用（含正在跑的这一轮）。
    pub context_tokens: Option<u64>,
    /// 这个模型的上下文窗口。
    pub context_window: Option<usize>,
    /// 这个会话（含它派出去的子代理）累计烧掉的。
    pub spent: miyu_core::llm::TurnTokens,
    /// 这个会话里可见的轮数。
    pub turns: usize,
}

/// 本会话那一段的中文摘要。
pub fn format_session_usage(usage: &SessionUsage) -> String {
    let fmt = format_tokens;
    let mut lines = vec!["**Token 消耗 · 本会话**".to_string(), String::new()];
    match usage.context_tokens {
        Some(tokens) => {
            let window = usage.context_window.filter(|window| *window > 0);
            let share = window
                .map(|window| {
                    format!(
                        "，占上下文窗口 **{:.0}%**",
                        tokens as f64 / window as f64 * 100.0
                    )
                })
                .unwrap_or_default();
            let of = window
                .map(|window| format!(" / {}", fmt(window as u64)))
                .unwrap_or_default();
            lines.push(format!("- 当前上下文 **{}**{of}{share}", fmt(tokens)));
        }
        // 刚开新会话、刚压缩完、上一轮被打断：供应商还没报过占用。
        None => lines.push("- 当前上下文：还没有实测数".to_string()),
    }
    if usage.spent.total > 0 {
        let hit = (usage.spent.prompt > 0)
            .then(|| usage.spent.cache_read as f64 / usage.spent.prompt as f64 * 100.0)
            .unwrap_or(0.0);
        lines.push(format!(
            "- 这个会话累计 **{}**（含这一轮到目前为止、含派出去的子代理）· 缓存命中率 **{hit:.0}%**",
            fmt(usage.spent.total)
        ));
    } else {
        lines.push("- 这个会话累计：还没有用量".to_string());
    }
    lines.push(format!("- 已经聊了 **{}** 轮", usage.turns));
    lines.join("\n")
}

/// 本会话那件：这条对话自己的账。全局那件是 [`DESCRIPTION`]。
///
/// 独立成一件工具而不是给全局那件加一个 scope 参数（用户 09-22 裁定）：两者
/// 问的根本不是同一件事——一个是「Miyu 这台机器烧了多少」，一个是「我们这段
/// 对话吃了多少」。挤在一个参数里既容易和 `range` 的 `all` 混，模型也难分。
pub const SESSION_DESCRIPTION: &str = "Token usage of THIS conversation: how much of the context window it currently fills, what it has spent so far (including its subagents), and how many turns it has run. For Miyu's overall usage across all sources use query_system_token_usage instead.";

pub fn session_parameters() -> Value {
    json!({ "type": "object", "properties": {}, "additionalProperties": false })
}

/// 注册「本会话用量」。`session` 给不出来（平台工具集、还没绑会话）就不注册
/// ——凭空多一件答不上来的工具比没有更糟。
pub fn register_session(registry: &mut ToolRegistry, session: SessionUsageFn) {
    registry.register(
        ToolSpec::new(
            "query_session_token_usage",
            SESSION_DESCRIPTION,
            session_parameters(),
            move |_arguments| {
                let session = session.clone();
                async move {
                    Ok(match session() {
                        Some(usage) => format_session_usage(&usage),
                        None => "**Token 消耗 · 本会话**\n\n这条线上读不到会话级用量。".to_string(),
                    })
                }
            },
        )
        .with_display_name(miyu_base::i18n::text("Session usage", "本会话用量")),
    );
}

pub fn register(
    registry: &mut ToolRegistry,
    state_dir: PathBuf,
    config: miyu_base::config::AppConfig,
) {
    registry.register(
        ToolSpec::new(
            "query_system_token_usage",
            DESCRIPTION,
            parameters(),
            move |arguments| {
                let state_dir = state_dir.clone();
                let config = config.clone();
                async move { query(arguments, state_dir, config).await }
            },
        )
        .with_display_name(miyu_base::i18n::text("Token usage", "词元用量")),
    );
}

async fn query(
    arguments: Value,
    state_dir: PathBuf,
    config: miyu_base::config::AppConfig,
) -> Result<String> {
    let range_key = arguments
        .get("range")
        .and_then(Value::as_str)
        .unwrap_or("1d")
        .to_string();
    let range = miyu_core::state::UsageRange::parse(&range_key);
    let stats = tokio::task::spawn_blocking(move || {
        let price = miyu_base::models_cache::pricing_resolver(&config);
        usage::ledger(&state_dir)?.stats(range, &price, None)
    })
    .await
    .context("usage stats task panicked")??;
    Ok(format_usage_summary(&stats, &range_key))
}

/// Markdown 输出(08-26):平台长文会转图渲染,QQ 与 WebUI 都按 markdown
/// 显示——原来的 `▸` 自造符号既不是 markdown 也不好读。细项(主动回复
/// 判断等)挂在所属来源下面,零记录不出现。
pub fn format_usage_summary(stats: &miyu_core::state::UsageStats, range_key: &str) -> String {
    let label = match range_key {
        "1d" | "24h" | "today" => "近一天",
        "7d" => "近 7 天",
        "30d" => "近 30 天",
        _ => "至今",
    };
    if stats.totals.requests == 0 {
        return format!("**Token 消耗 · 全局 · {label}**\n\n{label}没有任何 LLM 调用记录。");
    }
    let fmt = format_tokens;
    let hit = |cache_read: u64, prompt: u64| {
        (prompt > 0).then(|| (cache_read as f64 / prompt as f64 * 100.0).round())
    };
    let total_hit = hit(stats.totals.cache_read, stats.totals.prompt).unwrap_or(0.0);
    let mut lines = vec![
        format!("**Token 消耗 · 全局 · {label}**"),
        String::new(),
        format!(
            "- 总消耗 **{}**(输入 {} · 输出 {})",
            fmt(stats.totals.total),
            fmt(stats.totals.prompt),
            fmt(stats.totals.completion)
        ),
        format!(
            "- 请求 **{}** 次 · 缓存命中率 **{total_hit:.0}%**",
            stats.totals.requests
        ),
    ];
    // 金额估算不进工具输出(用户 08-20 裁定:models.dev 价目对不齐实际计费,
    // 数字不准还容易被模型当真话复述)。WebUI 控制台的统计图表照旧。
    for source in &stats.sources {
        let name = usage_source_name(&source.src);
        let source_hit = hit(source.aggregate.cache_read, source.aggregate.prompt)
            .map(|value| format!(" · 命中 {value:.0}%"))
            .unwrap_or_default();
        lines.push(String::new());
        lines.push(format!(
            "**{name}** · {} 次 · {}{source_hit}",
            source.aggregate.requests,
            fmt(source.aggregate.total)
        ));
        let mut parts = Vec::new();
        for model in source.models.iter().take(3) {
            let share = if source.aggregate.total > 0 {
                (model.aggregate.total as f64 / source.aggregate.total as f64 * 100.0).round()
            } else {
                0.0
            };
            let display = if model.model.is_empty() {
                "(未标模型)"
            } else {
                model.model.as_str()
            };
            parts.push(format!("{display} {share:.0}%"));
        }
        if !parts.is_empty() {
            lines.push(format!("- 模型构成:{}", parts.join(" · ")));
        }
        for kind in &source.kinds {
            let share = if source.aggregate.total > 0 {
                (kind.aggregate.total as f64 / source.aggregate.total as f64 * 100.0).round()
            } else {
                0.0
            };
            lines.push(format!(
                "- 其中 {} {} 次 · {} · 占本来源 {share:.0}%",
                usage_kind_name(&kind.kind),
                kind.aggregate.requests,
                fmt(kind.aggregate.total)
            ));
        }
    }
    lines.join("\n")
}

pub(crate) fn usage_source_name(src: &str) -> String {
    match src {
        "agent" => "智能体".to_string(),
        "qq" | "onebot" => "QQ".to_string(),
        other => other.to_string(),
    }
}

pub(crate) fn usage_kind_name(kind: &str) -> String {
    match kind {
        miyu_core::state::USAGE_KIND_JUDGE => "主动回复判断".to_string(),
        miyu_core::state::USAGE_KIND_AFFECTION => "好感度更新".to_string(),
        miyu_core::state::USAGE_KIND_GROUP_JOIN => "入群审批".to_string(),
        other => other.to_string(),
    }
}

fn format_tokens(value: u64) -> String {
    // 与 WebUI 的 usageFmt 同档:全量范围下总量早就过十亿,只有 M 会印出
    // "1234.56M"(08-26 用户点名)。
    if value >= 1_000_000_000 {
        format!("{:.2}B", value as f64 / 1_000_000_000.0)
    } else if value >= 1_000_000 {
        format!("{:.2}M", value as f64 / 1_000_000.0)
    } else if value >= 1_000 {
        format!("{:.1}k", value as f64 / 1_000.0)
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use miyu_core::llm::Usage;

    #[tokio::test]
    async fn agent_registry_tool_reports_usage() {
        let temp = tempfile::tempdir().unwrap();
        let history = temp.path().to_path_buf();
        usage::record_usage(
            &history,
            &Usage {
                prompt_tokens: 2000,
                completion_tokens: 300,
                total_tokens: 2300,
                cache_read_tokens: 900,
                ..Usage::default()
            },
            usage::UsageMeta {
                source: "agent",
                provider: Some("prov"),
                model: Some("m-x"),
                kind: None,
            },
            false,
        )
        .unwrap();
        let mut registry = ToolRegistry::new();
        register(
            &mut registry,
            history,
            miyu_base::config::AppConfig::default(),
        );
        let output = registry
            .call("query_system_token_usage", r#"{"range":"1d"}"#)
            .await
            .unwrap();
        assert!(
            output.contains("**Token 消耗 · 全局 · 近一天**"),
            "{output}"
        );
        assert!(output.contains("**智能体**"), "{output}");
        assert!(output.contains("- 模型构成:m-x"), "{output}");
        assert!(output.contains("缓存命中率 **45%**"), "{output}");
        // 没有细项标签的来源不长出"其中"行。
        assert!(!output.contains("其中"), "{output}");
    }

    /// 主动回复判断作为来源下的细项(08-26):有记录才出现,数字不重复计进
    /// 来源合计之外。退回 kind 聚合前,这条断言拿不到"其中 主动回复判断"。
    #[tokio::test]
    async fn judge_usage_renders_as_a_platform_sub_item() {
        let temp = tempfile::tempdir().unwrap();
        let history = temp.path().to_path_buf();
        let record = |kind: Option<&str>, prompt: u64| {
            usage::record_usage(
                &history,
                &Usage {
                    prompt_tokens: prompt,
                    completion_tokens: 100,
                    total_tokens: prompt + 100,
                    ..Usage::default()
                },
                usage::UsageMeta {
                    source: "onebot",
                    provider: Some("prov"),
                    model: Some("m-q"),
                    kind,
                },
                kind.is_some(),
            )
            .unwrap();
        };
        record(None, 9_000);
        record(Some(miyu_core::state::USAGE_KIND_JUDGE), 1_000);
        let mut registry = ToolRegistry::new();
        register(
            &mut registry,
            history,
            miyu_base::config::AppConfig::default(),
        );
        let output = registry
            .call("query_system_token_usage", r#"{"range":"1d"}"#)
            .await
            .unwrap();
        assert!(output.contains("**QQ** · 2 次"), "{output}");
        assert!(output.contains("- 其中 主动回复判断 1 次"), "{output}");
        // 合计仍是两条之和,细项不额外加总。
        assert!(output.contains("总消耗 **10.2k**"), "{output}");
    }

    /// `query_session_token_usage` 报的是**这条会话**：上下文占了窗口多少、累计烧了
    /// 多少、聊了几轮（用户 09-22：别往提示词里塞常驻字段，做成工具；而且两
    /// 件事要分成两件工具，别挤在一个 scope 参数里）。
    #[tokio::test]
    async fn the_session_tool_reports_this_conversation() {
        let temp = tempfile::tempdir().unwrap();
        let history = temp.path().to_path_buf();
        let mut registry = ToolRegistry::new();
        let session: SessionUsageFn = std::sync::Arc::new(|| {
            Some(SessionUsage {
                context_tokens: Some(47_000),
                context_window: Some(128_000),
                spent: miyu_core::llm::TurnTokens {
                    total: 1_200_000,
                    prompt: 1_000_000,
                    cache_read: 900_000,
                },
                turns: 12,
            })
        });
        let _ = history;
        register_session(&mut registry, session);
        assert_eq!(
            registry.tool_names(),
            vec!["query_session_token_usage".to_string()]
        );

        let out = registry
            .call("query_session_token_usage", r#"{}"#)
            .await
            .unwrap();
        assert!(out.contains("本会话"), "{out}");
        assert!(out.contains("47.0k"), "{out}");
        assert!(out.contains("37%"), "占窗口的比例没报: {out}");
        assert!(out.contains("1.20M"), "{out}");
        assert!(out.contains("90%"), "缓存命中率没报: {out}");
        assert!(out.contains("12"), "轮数没报: {out}");
    }

    /// 全局那件和本会话那件是两件独立的工具，名字各自说清楚问的是什么
    /// （用户 09-22：「不要混起来」）。
    #[tokio::test]
    async fn the_two_scopes_are_two_separate_tools() {
        let temp = tempfile::tempdir().unwrap();
        let history = temp.path().to_path_buf();
        let mut registry = ToolRegistry::new();
        register(
            &mut registry,
            history,
            miyu_base::config::AppConfig::default(),
        );
        // 没绑会话时只有全局那件——凭空多一件答不上来的工具比没有更糟。
        assert_eq!(
            registry.tool_names(),
            vec!["query_system_token_usage".to_string()]
        );
    }
}
