//! 量尺:账本从老文件导入、统计查询随明细条数怎么涨(09-24 起走 `usage.db`)。

mod history_scaling_probe {
    use super::super::*;
    use chrono::Local;

    /// 造一份 `n` 条的历史，形状照抄真实文件（151 字节/条）。
    pub(super) fn write_history_for_probe(path: &Path, records: usize) -> std::io::Result<()> {
        write_history(path, records)
    }

    fn write_history(path: &Path, records: usize) -> std::io::Result<()> {
        let mut body = String::with_capacity(records * 160);
        let base = Local::now().timestamp() - records as i64 * 30;
        for index in 0..records {
            let ts = base + index as i64 * 30;
            body.push_str(&format!(
                r#"{{"ts":{ts},"src":"agent","provider":"opencodego","model":"deepseek-v4-flash","prompt":7638,"completion":2842,"total":10480,"cache_read":7552,"aux":true}}"#
            ));
            body.push('\n');
        }
        std::fs::write(path, body)
    }

    /// 量尺：`cargo test --lib history_scaling_probe -- --ignored --nocapture`
    ///
    /// 明细只增不轮转。本机实测 14,511 条 / 2.2 MB 是 **5.7 天**攒出来的（约
    /// 387 KB/天），所以下面这几档分别对应 5.7 天 / 1 个月 / 半年 / 1 年。以前每次
    /// 查询都把整个 jsonl 读进来解析；现在老文件只在第一次开库时导一次，查询走库。
    #[test]
    #[ignore]
    fn usage_stats_scales_with_history_size() {
        println!(
            "\n  {:<16}{:>10}{:>14}{:>12}{:>10}{:>12}",
            "条数", "文件", "改前每次查询", "一次导入", "查询", "折合时长"
        );
        for (records, span) in [
            (14_511usize, "5.7 天（当前）"),
            (76_000, "1 个月"),
            (460_000, "半年"),
            (930_000, "1 年"),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("usage-history.jsonl");
            write_history(&path, records).unwrap();
            let bytes = std::fs::metadata(&path).unwrap().len();
            // 改前的路子原样照做：整个文件读进来、逐行解析，再做同一份汇总。
            let started = std::time::Instant::now();
            let raw = std::fs::read_to_string(&path).unwrap();
            let parsed = raw
                .lines()
                .filter(|line| !line.trim().is_empty())
                .filter_map(|line| serde_json::from_str::<UsageRecord>(line).ok())
                .collect::<Vec<_>>();
            let _ =
                super::super::stats::aggregate(parsed, UsageRange::parse("1d"), &|_, _| None, None);
            let before = started.elapsed();
            let started = std::time::Instant::now();
            let ledger = ledger(dir.path()).unwrap();
            let imported = started.elapsed();
            let started = std::time::Instant::now();
            let stats = ledger.stats(UsageRange::parse("1d"), &|_, _| None, None);
            let queried = started.elapsed();
            assert!(stats.is_ok());
            println!(
                "  {records:<16}{:>8.1}MB{:>12.1}ms{:>10.1}ms{:>8.1}ms{:>14}",
                bytes as f64 / 1048576.0,
                before.as_secs_f64() * 1000.0,
                imported.as_secs_f64() * 1000.0,
                queried.as_secs_f64() * 1000.0,
                span
            );
        }
    }
}

mod runtime_freeze_probe {
    use super::history_scaling_probe::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// 量尺：`cargo test --lib runtime_freeze_probe -- --ignored --nocapture`
    ///
    /// 要证明的不是「统计变快了」（活儿一样多），而是**统计期间别的异步任务还
    /// 转不转**。
    ///
    /// 关键坑（C17 那次差点栽在这上面）：被测的活儿必须用 `tokio::spawn` 丢到
    /// worker 上。写成 `runtime.block_on(...)` 的话它跑在**调用线程**，而 ticker
    /// 在 worker 上，两边根本不抢同一个线程，量出来两种写法一样快，结论正好
    /// 反过来。
    #[test]
    #[ignore]
    fn stats_query_does_not_stall_other_tasks() {
        let dir = tempfile::tempdir().unwrap();
        write_history_for_probe(&dir.path().join("usage-history.jsonl"), 76_000).unwrap();
        let ledger = super::super::ledger(dir.path()).unwrap();

        println!("\n  单 worker 运行时上，统计跑着的时候 ticker 还能跳几次");
        for (label, blocking) in [
            ("同步调用（改前）", false),
            ("spawn_blocking（改后）", true),
        ] {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .build()
                .unwrap();
            let ticks = Arc::new(AtomicUsize::new(0));
            let counter = ticks.clone();
            let ledger = ledger.clone();
            runtime.block_on(async move {
                let ticker = tokio::spawn(async move {
                    for _ in 0..200 {
                        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                        counter.fetch_add(1, Ordering::Relaxed);
                    }
                });
                let work = tokio::spawn(async move {
                    let range = crate::state::UsageRange::parse("1d");
                    if blocking {
                        tokio::task::spawn_blocking(move || {
                            ledger.stats(range, &|_, _| None, None)
                        })
                        .await
                        .unwrap()
                        .unwrap();
                    } else {
                        ledger.stats(range, &|_, _| None, None).unwrap();
                    }
                });
                work.await.unwrap();
                let during = ticks.load(Ordering::Relaxed);
                ticker.abort();
                println!("  {label:<26}{during:>4} 次");
            });
        }
    }
}

mod write_cost_probe {
    use super::super::*;

    /// 量尺：`cargo test --lib write_cost_probe -- --ignored --nocapture`
    ///
    /// 每次调模型（主对话、缓存保活、QQ 每条消息的主动回复判断……）都要记一笔累计。
    /// 改前是把 usage.json 整个读出来、改、写临时文件、fsync、改名；改前的路子
    /// 在这里原样照做，和记进库里的一次事务对比。
    #[test]
    #[ignore]
    fn recording_one_call() {
        const CALLS: u32 = 300;
        let usage = Usage {
            prompt_tokens: 7638,
            completion_tokens: 2842,
            total_tokens: 10480,
            ..Usage::default()
        };
        // 临时目录在 tmpfs 上时 fsync 是空操作，量不出真账本（在 ~/.miyu，真磁盘上）
        // 的代价：放到家目录的缓存里量。
        let home_cache = std::path::PathBuf::from(std::env::var_os("HOME").unwrap()).join(".cache");
        std::fs::create_dir_all(&home_cache).unwrap();
        let dir = tempfile::tempdir_in(&home_cache).unwrap();
        let path = dir.path().join("usage.json");
        std::fs::write(
            &path,
            "{\"requests\":0,\"prompt_tokens\":0,\"completion_tokens\":0,\"total_tokens\":0}",
        )
        .unwrap();
        let started = std::time::Instant::now();
        for _ in 0..CALLS {
            let raw = std::fs::read_to_string(&path).unwrap();
            let mut state: UsageState = serde_json::from_str(&raw).unwrap();
            state.requests += 1;
            state.total_tokens += usage.effective_total_tokens();
            state.last_usage = Some(usage.clone());
            let mut file = tempfile::NamedTempFile::new_in(dir.path()).unwrap();
            use std::io::Write as _;
            writeln!(file, "{}", serde_json::to_string_pretty(&state).unwrap()).unwrap();
            file.as_file().sync_all().unwrap();
            file.persist(&path).unwrap();
        }
        let before = started.elapsed() / CALLS;

        let ledger = ledger(dir.path()).unwrap();
        let started = std::time::Instant::now();
        for _ in 0..CALLS {
            ledger.add(&usage, true).unwrap();
        }
        let after = started.elapsed() / CALLS;
        println!(
            "\n  记一笔累计：改前 {:.2} ms（读整份 + fsync + 改名），入库后 {:.2} ms",
            before.as_secs_f64() * 1000.0,
            after.as_secs_f64() * 1000.0
        );
    }
}
