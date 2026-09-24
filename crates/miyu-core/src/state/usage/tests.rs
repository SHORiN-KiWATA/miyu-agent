//! 用量账本的测试。09-24 起账本在库里（`state/usage.db`），后半段是老文件的导入。

use super::*;
use std::io::Write;

fn tokens(prompt: u64, completion: u64, total: u64) -> Usage {
    Usage {
        prompt_tokens: prompt,
        completion_tokens: completion,
        total_tokens: total,
        ..Usage::default()
    }
}

fn meta<'a>(source: &'a str, provider: &'a str, model: &'a str) -> UsageMeta<'a> {
    UsageMeta {
        source,
        provider: Some(provider),
        model: Some(model),
        kind: None,
    }
}

/// 计费估算:有价的记录累计 cost/costed_requests,无价的只计用量。
#[test]
fn usage_stats_estimates_cost_with_resolver() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path();
    let usage = Usage {
        cache_read_tokens: 1_000_000,
        ..tokens(2_000_000, 1_000_000, 3_000_000)
    };
    record_usage(dir, &usage, meta("agent", "priced", "m"), false).unwrap();
    record_usage(dir, &usage, meta("agent", "unknown", "m"), false).unwrap();
    let price = |provider: &str, _model: &str| {
        (provider == "priced").then_some(miyu_base::models_cache::ApiCost {
            input: 1.0,
            output: 2.0,
            cache_read: Some(0.1),
            cache_write: None,
        })
    };
    let stats = ledger(dir)
        .unwrap()
        .stats(UsageRange::All, &price, None)
        .unwrap();
    assert_eq!(stats.totals.requests, 2);
    assert_eq!(stats.totals.costed_requests, 1);
    // 未命中 100 万×1 + 命中 100 万×0.1 + 输出 100 万×2 = 3.1
    assert!(
        (stats.totals.cost - 3.1).abs() < 1e-9,
        "{}",
        stats.totals.cost
    );
    let day_cost: f64 = stats.daily.iter().map(|d| d.cost).sum();
    assert!((day_cost - 3.1).abs() < 1e-9);
    let details = usage_details(dir, 10, None, None, &price).unwrap();
    let priced: Vec<_> = details.iter().filter(|r| r.cost.is_some()).collect();
    assert_eq!(priced.len(), 1);
    assert!((priced[0].cost.unwrap() - 3.1).abs() < 1e-9);
}

#[test]
fn records_and_clears_last_usage() {
    let temp = tempfile::tempdir().unwrap();
    let ledger = ledger(temp.path()).unwrap();

    ledger.add(&tokens(10, 5, 15), true).unwrap();
    let usage_snapshot = ledger.snapshot().unwrap();
    assert_eq!(usage_snapshot.last_usage.unwrap().total_tokens, 15);
    assert_eq!(
        usage_snapshot
            .last_conversation_usage
            .unwrap()
            .prompt_tokens,
        10
    );

    ledger.clear_last_usage().unwrap();
    let usage_snapshot = ledger.snapshot().unwrap();
    assert_eq!(usage_snapshot.total_tokens, 15);
    assert!(usage_snapshot.last_usage.is_none());
    assert!(usage_snapshot.last_conversation_usage.is_none());
}

#[test]
fn auxiliary_usage_does_not_replace_conversation_usage() {
    let temp = tempfile::tempdir().unwrap();
    let ledger = ledger(temp.path()).unwrap();

    ledger.add(&tokens(100, 20, 120), true).unwrap();
    ledger.add(&tokens(5, 2, 7), false).unwrap();

    let snapshot = ledger.snapshot().unwrap();
    assert_eq!(snapshot.total_tokens, 127);
    assert_eq!(snapshot.last_usage.unwrap().prompt_tokens, 5);
    assert_eq!(snapshot.last_conversation_usage.unwrap().prompt_tokens, 100);
}

#[test]
fn total_tokens_falls_back_to_prompt_plus_completion() {
    let temp = tempfile::tempdir().unwrap();
    let ledger = ledger(temp.path()).unwrap();

    ledger.add(&tokens(7, 3, 0), true).unwrap();

    let snapshot = ledger.snapshot().unwrap();
    assert_eq!(snapshot.total_tokens, 10);
    assert_eq!(snapshot.conversation_tokens, 10);
}

#[test]
fn usage_history_records_and_aggregates_by_source() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path();
    let now = chrono::Utc::now().timestamp();
    let usage = |prompt: u64, completion: u64, cache: u64| Usage {
        cache_read_tokens: cache,
        ..tokens(prompt, completion, prompt + completion)
    };

    record_usage_at(
        dir,
        &usage(100, 20, 60),
        meta("agent", "prov", "m-a"),
        false,
        now,
    )
    .unwrap();
    record_usage_at(
        dir,
        &usage(50, 10, 0),
        meta("qq", "prov", "m-b"),
        false,
        now - 3_600,
    )
    .unwrap();
    record_usage_at(
        dir,
        &usage(30, 5, 10),
        meta("qq", "prov", "m-b"),
        true,
        now - 40 * 86_400,
    )
    .unwrap();

    let ledger = ledger(dir).unwrap();
    let all = ledger.stats(UsageRange::All, &|_, _| None, None).unwrap();
    assert_eq!(all.totals.requests, 3);
    assert_eq!(all.totals.prompt, 180);
    assert_eq!(all.totals.cache_read, 70);
    assert!(all.prev_totals.is_none());
    assert_eq!(all.daily.len(), 364);
    assert_eq!(all.sources.len(), 2);
    assert_eq!(all.sources[0].src, "agent"); // 智能体排最前
    assert_eq!(all.sources[1].src, "qq");
    assert_eq!(all.sources[1].aggregate.requests, 2);
    assert_eq!(all.sources[1].models[0].model, "m-b");

    let week = ledger
        .stats(UsageRange::Days(7), &|_, _| None, None)
        .unwrap();
    assert_eq!(week.totals.requests, 2); // 40 天前的那条不在窗口
    assert!(week.prev_totals.is_some());

    let details = usage_details(dir, 2, None, None, &|_, _| None).unwrap();
    assert_eq!(details.len(), 2);
    assert_eq!(details[0].model, "m-a"); // 新的在前
    assert!(!details[0].aux);

    let only_qq = usage_details(dir, 10, Some("qq"), None, &|_, _| None).unwrap();
    assert_eq!(only_qq.len(), 2);
    assert!(only_qq.iter().all(|record| record.src == "qq"));
    let only_model = usage_details(dir, 10, None, Some("m-b"), &|_, _| None).unwrap();
    assert_eq!(only_model.len(), 2);
}

#[test]
fn reset_conversation_preserves_global_total() {
    let temp = tempfile::tempdir().unwrap();
    let ledger = ledger(temp.path()).unwrap();
    ledger.add(&tokens(7, 3, 10), true).unwrap();

    ledger.reset_conversation().unwrap();
    let snapshot = ledger.snapshot().unwrap();
    assert_eq!(snapshot.total_tokens, 10);
    assert_eq!(snapshot.conversation_tokens, 0);
    assert!(snapshot.last_conversation_usage.is_none());
}

/// 改名只动 provider 对得上的行,别的行一个字节不碰。
#[test]
fn rename_provider_rewrites_matching_rows_only() {
    let temp = tempfile::tempdir().unwrap();
    for provider in ["old", "old", "other"] {
        record_usage(
            temp.path(),
            &tokens(10, 5, 15),
            meta("agent", provider, "m"),
            false,
        )
        .unwrap();
    }
    let ledger = ledger(temp.path()).unwrap();

    assert_eq!(ledger.rename_provider("old", "new").unwrap(), 2);

    let records = ledger.records(None).unwrap();
    let providers: Vec<&str> = records.iter().map(|item| item.provider.as_str()).collect();
    assert_eq!(providers, ["new", "new", "other"]);
    assert!(records.iter().all(|item| item.total == 15));
}

/// 同名、空旧名、没有对得上的行都返回 0。
#[test]
fn rename_provider_without_matches_changes_nothing() {
    let temp = tempfile::tempdir().unwrap();
    record_usage(
        temp.path(),
        &tokens(10, 5, 15),
        meta("agent", "other", "m"),
        false,
    )
    .unwrap();
    let ledger = ledger(temp.path()).unwrap();

    assert_eq!(ledger.rename_provider("missing", "new").unwrap(), 0);
    assert_eq!(ledger.rename_provider("other", "other").unwrap(), 0);
    assert_eq!(ledger.rename_provider("", "new").unwrap(), 0);
    assert_eq!(ledger.records(None).unwrap()[0].provider, "other");
}

fn history_line(ts: i64, provider: &str) -> String {
    format!(
        r#"{{"ts":{ts},"provider":"{provider}","model":"m","prompt":10,"completion":2,"total":12}}"#
    )
}

/// 老账本第一次开库时导进来：累计照搬；明细的坏行跳过，没写完的那行留着；
/// 缺来源的老记录照旧归智能体。老文件本版保留，回退到上一版还读得到。
#[test]
fn old_ledger_files_are_imported_and_kept() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path();
    let now = chrono::Utc::now().timestamp();
    std::fs::write(
        dir.join("usage.json"),
        r#"{"requests":3,"prompt_tokens":20,"completion_tokens":10,"total_tokens":30,"conversation_tokens":30}"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("usage-history.jsonl"),
        format!(
            "{}\nnot-json\n{}\n{{\"ts\":{now},\"prov",
            history_line(now, "a"),
            history_line(now, "b")
        ),
    )
    .unwrap();

    let ledger = ledger(dir).unwrap();

    let snapshot = ledger.snapshot().unwrap();
    assert_eq!(snapshot.requests, 3);
    assert_eq!(snapshot.total_tokens, 30);
    let records = ledger.records(None).unwrap();
    assert_eq!(records.len(), 2, "坏行跳过,没写完的那行不导");
    let stats = ledger.stats(UsageRange::All, &|_, _| None, None).unwrap();
    assert_eq!(stats.sources[0].src, "agent"); // 旧记录缺 src 归智能体
    assert!(dir.join("usage.json").exists());
    assert!(dir.join("usage-history.jsonl").exists());
}

/// 回退到老版本后它又往老文件里追加了几行：再开库时接着导，累计不重导。
#[test]
fn lines_appended_by_an_older_build_are_picked_up_on_the_next_open() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path();
    let now = chrono::Utc::now().timestamp();
    let history = dir.join("usage-history.jsonl");
    let totals = |requests: u64| {
        format!(
            r#"{{"requests":{requests},"prompt_tokens":0,"completion_tokens":0,"total_tokens":0}}"#
        )
    };
    std::fs::write(dir.join("usage.json"), totals(3)).unwrap();
    std::fs::write(&history, format!("{}\n", history_line(now, "a"))).unwrap();
    let first = ledger(dir).unwrap();
    first.add(&tokens(1, 1, 2), true).unwrap();
    assert_eq!(first.records(None).unwrap().len(), 1);
    drop(first);

    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&history)
        .unwrap();
    writeln!(file, "{}", history_line(now, "b")).unwrap();
    drop(file);
    std::fs::write(dir.join("usage.json"), totals(99)).unwrap();

    let reopened = ledger(dir).unwrap();
    let providers = reopened
        .records(None)
        .unwrap()
        .into_iter()
        .map(|record| record.provider)
        .collect::<Vec<_>>();
    assert_eq!(providers, ["a", "b"]);
    assert_eq!(reopened.snapshot().unwrap().requests, 4, "累计只导一次");
}

/// 清空明细连老文件一起删：不然回退到上一版，清掉的明细又冒出来。累计不动。
#[test]
fn clearing_the_history_removes_the_old_file_too() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path();
    let now = chrono::Utc::now().timestamp();
    std::fs::write(
        dir.join("usage-history.jsonl"),
        format!("{}\n", history_line(now, "a")),
    )
    .unwrap();
    let ledger = ledger(dir).unwrap();
    ledger.add(&tokens(1, 1, 2), true).unwrap();

    ledger.clear_history().unwrap();

    assert!(ledger.records(None).unwrap().is_empty());
    assert!(!dir.join("usage-history.jsonl").exists());
    assert_eq!(ledger.snapshot().unwrap().total_tokens, 2);
}
