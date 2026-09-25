//! 用量统计:把明细汇总成统计页、`usage` 工具要的那几张表。纯计算,不碰存储
//! (09-24 从 `usage.rs` 搬来;明细改由 `UsageDb` 从库里取)。

use super::*;
use chrono::{Duration as ChronoDuration, Local, TimeZone};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default, Serialize)]
pub struct UsageAggregate {
    pub requests: u64,
    pub prompt: u64,
    pub completion: u64,
    pub cache_read: u64,
    pub total: u64,
    /// 计费估算(USD),按 models.dev 单价 × 用量;只累计查得到价的请求。
    pub cost: f64,
    /// 参与计费估算的请求数。< requests 说明部分记录没有价格数据
    /// (自定义中转、目录未收录的模型),前端据此标注估算覆盖率。
    pub costed_requests: u64,
}

impl UsageAggregate {
    fn absorb(&mut self, record: &UsageRecord, cost: Option<f64>) {
        self.requests += 1;
        self.prompt += record.prompt;
        self.completion += record.completion;
        self.cache_read += record.cache_read;
        self.total += record.total;
        if let Some(cost) = cost {
            self.cost += cost;
            self.costed_requests += 1;
        }
    }
}

/// 一个本地自然日的汇总(热力图与柱状图共用)。
#[derive(Debug, Clone, Serialize)]
pub struct DailyUsage {
    pub date: String,
    pub requests: u64,
    pub prompt: u64,
    pub completion: u64,
    pub cache_read: u64,
    pub total: u64,
    /// 计费估算(USD),只含查得到价的请求。
    pub cost: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelUsage {
    pub provider: String,
    pub model: String,
    #[serde(flatten)]
    pub aggregate: UsageAggregate,
}

/// 来源内的细项(如主动回复判断),已含在来源合计里,不重复计数。
/// `models` 让前端把细项摊进饼图与模型表——只给一个合计数字,细项就只能
/// 当页脚摆着(08-26 用户点名)。
#[derive(Debug, Clone, Serialize)]
pub struct KindUsage {
    pub kind: String,
    #[serde(flatten)]
    pub aggregate: UsageAggregate,
    pub models: Vec<ModelUsage>,
}

/// 一个来源(智能体/某平台)在选定范围内的汇总与模型构成。
#[derive(Debug, Clone, Serialize)]
pub struct SourceUsage {
    pub src: String,
    #[serde(flatten)]
    pub aggregate: UsageAggregate,
    pub models: Vec<ModelUsage>,
    /// 细项拆解(见 [`KindUsage`]);无标签记录不产生条目。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kinds: Vec<KindUsage>,
}

/// 一个账号在选定范围内的汇总(阶段 5,按人拆总表)。`acct` 空串 = 管理员/遗留/平台。
#[derive(Debug, Clone, Serialize)]
pub struct AccountUsage {
    pub acct: String,
    #[serde(flatten)]
    pub aggregate: UsageAggregate,
}

#[derive(Debug, Clone, Serialize)]
pub struct UsageStats {
    pub range: String,
    pub totals: UsageAggregate,
    /// 上一个等长窗口(环比基线);"至今" 无基线为 None。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prev_totals: Option<UsageAggregate>,
    /// 最近 364 个本地自然日(含今天),供热力图与柱状图切片。
    pub daily: Vec<DailyUsage>,
    pub sources: Vec<SourceUsage>,
    /// 范围内按账号拆分;只按某个账号过滤时为空(拆分没有意义)。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub accounts: Vec<AccountUsage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_ts: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageRange {
    /// 滚动近 24 小时(非日历日)。
    LastDay,
    Days(u32),
    All,
}

impl UsageRange {
    pub fn parse(value: &str) -> Self {
        match value {
            "1d" | "24h" | "today" => Self::LastDay,
            "7d" => Self::Days(7),
            "30d" => Self::Days(30),
            _ => Self::All,
        }
    }

    fn label(&self) -> String {
        match self {
            Self::LastDay => "1d".to_string(),
            Self::Days(n) => format!("{n}d"),
            Self::All => "all".to_string(),
        }
    }
}

fn local_day_start(ts: i64) -> Option<chrono::DateTime<Local>> {
    let time = Local.timestamp_opt(ts, 0).single()?;
    time.date_naive()
        .and_hms_opt(0, 0, 0)?
        .and_local_timezone(Local)
        .single()
}

/// 计价器:按 (provider, model) 给出单价;None = 无价格数据。
pub type PriceFn<'a> = &'a dyn Fn(&str, &str) -> Option<miyu_base::models_cache::ApiCost>;

/// 把明细汇总成统计页要的那几张表。`records` 已经按账号筛过(`account` 为 Some
/// 时只有该账号的记录,空串 = 管理员/遗留);None = 全部,并按人拆分。
pub(super) fn aggregate(
    records: Vec<UsageRecord>,
    range: UsageRange,
    price: PriceFn<'_>,
    account: Option<&str>,
) -> UsageStats {
    // 单价按 (provider, model) 记忆化:每条记录都查一次目录锁太浪费。
    let mut price_cache = std::collections::HashMap::<
        (String, String),
        Option<miyu_base::models_cache::ApiCost>,
    >::new();
    let mut record_cost = |record: &UsageRecord| -> Option<f64> {
        price_cache
            .entry((record.provider.clone(), record.model.clone()))
            .or_insert_with(|| price(&record.provider, &record.model))
            .map(|c| {
                c.estimate(
                    record.prompt,
                    record.completion,
                    record.cache_read,
                    record.cache_write,
                )
            })
    };
    let now = chrono::Utc::now().timestamp();
    // 范围窗口按本地自然日对齐:今天=本地零点起;7d/30d=含今天往前 n 天。
    let today_start = local_day_start(now).map(|t| t.timestamp()).unwrap_or(now);
    let (start, prev_start) = match range {
        UsageRange::LastDay => (Some(now - 86_400), Some(now - 2 * 86_400)),
        UsageRange::Days(n) => {
            let start = today_start - i64::from(n - 1) * 86_400;
            (Some(start), Some(start - i64::from(n) * 86_400))
        }
        UsageRange::All => (None, None),
    };

    let mut totals = UsageAggregate::default();
    let mut prev_totals = UsageAggregate::default();
    let mut daily = BTreeMap::<String, DailyUsage>::new();
    #[allow(clippy::type_complexity)]
    let mut sources = BTreeMap::<
        String,
        (
            UsageAggregate,
            BTreeMap<(String, String), UsageAggregate>,
            BTreeMap<String, (UsageAggregate, BTreeMap<(String, String), UsageAggregate>)>,
        ),
    >::new();
    let daily_floor = today_start - 363 * 86_400;
    let mut first_ts: Option<i64> = None;
    let mut accounts = BTreeMap::<String, UsageAggregate>::new();

    for record in &records {
        first_ts = Some(first_ts.map_or(record.ts, |t| t.min(record.ts)));
        let cost = record_cost(record);
        let in_range = start.map_or(true, |s| record.ts >= s);
        if in_range {
            totals.absorb(record, cost);
            if account.is_none() {
                accounts
                    .entry(record.acct.clone())
                    .or_default()
                    .absorb(record, cost);
            }
            let src = if record.src.is_empty() {
                "agent"
            } else {
                record.src.as_str()
            };
            let (agg, models, kinds) = sources.entry(src.to_string()).or_default();
            agg.absorb(record, cost);
            models
                .entry((record.provider.clone(), record.model.clone()))
                .or_default()
                .absorb(record, cost);
            if !record.kind.is_empty() {
                let (kind_agg, kind_models) = kinds.entry(record.kind.clone()).or_default();
                kind_agg.absorb(record, cost);
                kind_models
                    .entry((record.provider.clone(), record.model.clone()))
                    .or_default()
                    .absorb(record, cost);
            }
        } else if let (Some(s), Some(p)) = (start, prev_start) {
            if record.ts >= p && record.ts < s {
                prev_totals.absorb(record, cost);
            }
        }
        if record.ts >= daily_floor {
            if let Some(day) = local_day_start(record.ts) {
                let key = day.format("%Y-%m-%d").to_string();
                let entry = daily.entry(key.clone()).or_insert_with(|| DailyUsage {
                    date: key,
                    requests: 0,
                    prompt: 0,
                    completion: 0,
                    cache_read: 0,
                    total: 0,
                    cost: 0.0,
                });
                entry.requests += 1;
                entry.prompt += record.prompt;
                entry.completion += record.completion;
                entry.cache_read += record.cache_read;
                entry.total += record.total;
                entry.cost += cost.unwrap_or(0.0);
            }
        }
    }

    // 补齐 364 天里没有记录的日子(前端网格要连续)。
    if let Some(today) = local_day_start(now) {
        for offset in 0i64..364 {
            let day = today - ChronoDuration::days(363 - offset);
            let key = day.format("%Y-%m-%d").to_string();
            daily.entry(key.clone()).or_insert(DailyUsage {
                date: key,
                requests: 0,
                prompt: 0,
                completion: 0,
                cache_read: 0,
                total: 0,
                cost: 0.0,
            });
        }
    }
    let daily: Vec<DailyUsage> = daily.into_values().collect();
    let daily = daily
        .into_iter()
        .rev()
        .take(364)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();

    let sources = sources
        .into_iter()
        .map(|(src, (aggregate, models, kinds))| {
            let mut models: Vec<ModelUsage> = models
                .into_iter()
                .map(|((provider, model), aggregate)| ModelUsage {
                    provider,
                    model,
                    aggregate,
                })
                .collect();
            models.sort_by(|a, b| b.aggregate.total.cmp(&a.aggregate.total));
            let mut kinds: Vec<KindUsage> = kinds
                .into_iter()
                .map(|(kind, (aggregate, kind_models))| {
                    let mut models: Vec<ModelUsage> = kind_models
                        .into_iter()
                        .map(|((provider, model), aggregate)| ModelUsage {
                            provider,
                            model,
                            aggregate,
                        })
                        .collect();
                    models.sort_by(|a, b| b.aggregate.total.cmp(&a.aggregate.total));
                    KindUsage {
                        kind,
                        aggregate,
                        models,
                    }
                })
                .collect();
            kinds.sort_by(|a, b| b.aggregate.total.cmp(&a.aggregate.total));
            SourceUsage {
                src,
                aggregate,
                models,
                kinds,
            }
        })
        .collect::<Vec<_>>();
    // 智能体排最前,平台按名称序。
    let mut sources = sources;
    sources.sort_by(|a, b| {
        let rank = |s: &SourceUsage| if s.src == "agent" { 0 } else { 1 };
        rank(a).cmp(&rank(b)).then_with(|| a.src.cmp(&b.src))
    });

    let mut accounts: Vec<AccountUsage> = accounts
        .into_iter()
        .map(|(acct, aggregate)| AccountUsage { acct, aggregate })
        .collect();
    accounts.sort_by(|a, b| b.aggregate.total.cmp(&a.aggregate.total));

    UsageStats {
        range: range.label(),
        totals,
        prev_totals: (range != UsageRange::All).then_some(prev_totals),
        daily,
        sources,
        accounts,
        first_ts,
    }
}

/// 给明细标上估算费用(读取时按当前价目算,不落盘)。单价按 (provider, model)
/// 记忆化:每条记录都查一次目录锁太浪费。
pub(super) fn price_records(records: &mut [UsageRecord], price: PriceFn<'_>) {
    let mut price_cache = std::collections::HashMap::<
        (String, String),
        Option<miyu_base::models_cache::ApiCost>,
    >::new();
    for record in records.iter_mut() {
        record.cost = price_cache
            .entry((record.provider.clone(), record.model.clone()))
            .or_insert_with(|| price(&record.provider, &record.model))
            .map(|c| {
                c.estimate(
                    record.prompt,
                    record.completion,
                    record.cache_read,
                    record.cache_write,
                )
            });
    }
}
