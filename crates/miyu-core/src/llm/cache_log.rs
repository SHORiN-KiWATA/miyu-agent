//! Per-request cache accounting JSONL (v7 Release 1.5 "JSONL 结构化日志先行").
//!
//! One line of absolute token numbers per LLM request — provider, model,
//! scope, prompt/cache_read/completion — and never any prompt text. The
//! in-process `tracing` line in `finalize_stream_result` is debug-level and
//! ephemeral; this file is what makes cache regressions diagnosable after the
//! fact (the 2026-08-10 "12% full-miss turns" hunt had to be reconstructed
//! from the turns table because nothing durable recorded per-request hits).
//!
//! Files rotate daily (`cache-usage.<YYYY-MM-DD>.jsonl`), are created 0600,
//! and files older than the configured retention are pruned on rotation.
//! Recording must never fail a request: all errors degrade to a debug log.

use crate::llm::cache_prefix::PrefixDiff;
use crate::llm::Usage;
use miyu_base::config::CacheConfig;
use miyu_base::paths::MiyuPaths;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

const FILE_PREFIX: &str = "cache-usage.";
const FILE_SUFFIX: &str = ".jsonl";

struct Sink {
    dir: PathBuf,
    enabled: bool,
    retention_days: u64,
    current: Option<(String, fs::File)>,
}

static SINK: OnceLock<Mutex<Sink>> = OnceLock::new();

/// Installs (or updates) the process-wide sink. Called from client
/// construction so every path that can issue LLM requests configures the
/// sink before its first request; later calls just refresh the settings.
pub(crate) fn configure(paths: &MiyuPaths, config: &CacheConfig) {
    let dir = paths.cache_dir.join("logs");
    let mutex = SINK.get_or_init(|| {
        Mutex::new(Sink {
            dir: dir.clone(),
            enabled: config.request_log,
            retention_days: config.request_log_retention_days,
            current: None,
        })
    });
    let mut sink = mutex.lock().unwrap();
    if sink.dir != dir {
        sink.current = None;
        sink.dir = dir;
    }
    sink.enabled = config.request_log;
    sink.retention_days = config.request_log_retention_days;
}

/// Appends one accounting line. `usage` may be `None` (provider reported no
/// usage); the request is still counted so per-scope request totals stay
/// honest.
pub(crate) fn record(
    scope: &str,
    provider: &str,
    model: &str,
    key_index: usize,
    request_id: &str,
    usage: Option<&Usage>,
) {
    record_with_context(
        scope,
        provider,
        model,
        key_index,
        request_id,
        usage,
        &RecordContext::default(),
    );
}

/// 记账归属与前缀比对结果。没有这两样,一行 `cache_read=0` 既可能是我们把
/// 前缀掰了、也可能是上游丢了缓存,分不出来(模块头有判定表)。
#[derive(Default)]
pub(crate) struct RecordContext<'a> {
    pub(crate) session: Option<&'a str>,
    pub(crate) turn: Option<&'a str>,
    pub(crate) prefix: Option<&'a PrefixDiff>,
}

pub(crate) fn record_with_context(
    scope: &str,
    provider: &str,
    model: &str,
    key_index: usize,
    request_id: &str,
    usage: Option<&Usage>,
    context: &RecordContext<'_>,
) {
    let Some(mutex) = SINK.get() else {
        return;
    };
    let Ok(mut sink) = mutex.lock() else {
        return;
    };
    if !sink.enabled {
        return;
    }
    let now = chrono::Local::now();
    let date = now.format("%Y-%m-%d").to_string();
    let line = format_line(
        &now.to_rfc3339(),
        scope,
        provider,
        model,
        key_index,
        request_id,
        usage,
        context,
    );
    if let Err(error) = sink.write_line(&date, &line) {
        tracing::debug!(error = %error, "cache usage log write failed");
        sink.current = None;
    }
}

impl Sink {
    fn write_line(&mut self, date: &str, line: &str) -> std::io::Result<()> {
        let rotated = match &self.current {
            Some((current_date, _)) => current_date != date,
            None => true,
        };
        if rotated {
            fs::create_dir_all(&self.dir)?;
            let path = self.dir.join(format!("{FILE_PREFIX}{date}{FILE_SUFFIX}"));
            let mut options = fs::OpenOptions::new();
            options.create(true).append(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let file = options.open(path)?;
            self.current = Some((date.to_string(), file));
            prune_old_files(&self.dir, date, self.retention_days);
        }
        let (_, file) = self.current.as_mut().expect("rotation just set current");
        file.write_all(line.as_bytes())?;
        file.write_all(b"\n")
    }
}

#[allow(clippy::too_many_arguments)]
fn format_line(
    ts: &str,
    scope: &str,
    provider: &str,
    model: &str,
    key_index: usize,
    request_id: &str,
    usage: Option<&Usage>,
    context: &RecordContext<'_>,
) -> String {
    let (prompt, cache_read, cache_write, completion, reasoning, reported) = match usage {
        Some(usage) => (
            usage.prompt_tokens,
            usage.cache_read_tokens,
            usage.cache_write_tokens,
            usage.completion_tokens,
            usage.reasoning_tokens,
            usage.cache_reported,
        ),
        None => (0, 0, 0, 0, 0, false),
    };
    let mut line = serde_json::json!({
        "ts": ts,
        "scope": scope,
        "provider": provider,
        "model": model,
        "key": key_index + 1,
        "req": request_id,
        "prompt": prompt,
        "cache_read": cache_read,
        "cache_write": cache_write,
        "completion": completion,
        "reasoning": reasoning,
        "reported": reported,
    });
    let object = line.as_object_mut().expect("json! built an object");
    if let Some(session) = context.session {
        object.insert("sess".into(), session.into());
    }
    if let Some(turn) = context.turn {
        object.insert("turn".into(), turn.into());
    }
    if let Some(prefix) = context.prefix {
        // `msgs`/`prev`/`same` 三个数就够判定:same==prev 是纯追加,
        // same==0 是开头就变了,中间停住的话 `at`/`role` 指出是哪一条。
        object.insert("msgs".into(), prefix.messages.into());
        if let Some(previous) = prefix.previous {
            object.insert("prev".into(), previous.into());
        }
        object.insert("same".into(), prefix.same.into());
        if let Some((at, role)) = prefix.rewritten_at {
            object.insert("at".into(), at.into());
            object.insert("role".into(), role.into());
        }
        if prefix.tools_changed {
            object.insert("tools_changed".into(), true.into());
        }
        if let Some(diff) = &prefix.tools_diff {
            object.insert("tools_diff".into(), diff.as_str().into());
        }
        // 跨进程可比的两个短指纹:重启后 `prev` 没了,拿日志前后两行比这两个。
        object.insert("sys".into(), short_hash(prefix.system).into());
        object.insert("tools_hash".into(), short_hash(prefix.tools).into());
    }
    line.to_string()
}

fn short_hash(value: u64) -> String {
    format!("{:012x}", value >> 16)
}

/// Deletes cache-usage files whose date suffix is more than `retention_days`
/// before `today`. Unparseable file names are left alone.
fn prune_old_files(dir: &Path, today: &str, retention_days: u64) {
    let Ok(today) = chrono::NaiveDate::parse_from_str(today, "%Y-%m-%d") else {
        return;
    };
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !stale_file_date(name, today, retention_days) {
            continue;
        }
        if let Err(error) = fs::remove_file(entry.path()) {
            tracing::debug!(file = name, error = %error, "cache usage log prune failed");
        }
    }
}

fn stale_file_date(name: &str, today: chrono::NaiveDate, retention_days: u64) -> bool {
    let Some(date) = name
        .strip_prefix(FILE_PREFIX)
        .and_then(|rest| rest.strip_suffix(FILE_SUFFIX))
    else {
        return false;
    };
    let Ok(date) = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d") else {
        return false;
    };
    today.signed_duration_since(date).num_days() > retention_days as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(text: &str) -> chrono::NaiveDate {
        chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn stale_detection_honours_retention_and_ignores_foreign_files() {
        let today = day("2026-08-10");
        assert!(stale_file_date("cache-usage.2026-07-01.jsonl", today, 14));
        assert!(!stale_file_date("cache-usage.2026-08-01.jsonl", today, 14));
        assert!(!stale_file_date("cache-usage.2026-08-10.jsonl", today, 14));
        // 边界:正好 retention 天不删,多一天才删
        assert!(!stale_file_date("cache-usage.2026-07-27.jsonl", today, 14));
        assert!(stale_file_date("cache-usage.2026-07-26.jsonl", today, 14));
        // 非本日志的文件一律不动
        assert!(!stale_file_date("miyu.2026-07-01.log", today, 14));
        assert!(!stale_file_date("cache-usage.not-a-date.jsonl", today, 14));
    }

    #[test]
    fn line_contains_numbers_only_and_flags_missing_usage() {
        let usage = Usage {
            prompt_tokens: 51910,
            cache_read_tokens: 32384,
            completion_tokens: 996,
            reasoning_tokens: 558,
            cache_reported: true,
            ..Usage::default()
        };
        let line = format_line(
            "2026-08-10T17:00:00+08:00",
            "qq-judge",
            "ririxin",
            "deepseek-v4-flash",
            0,
            "llm_1",
            Some(&usage),
            &RecordContext::default(),
        );
        let value: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["prompt"], 51910);
        assert_eq!(value["cache_read"], 32384);
        assert_eq!(value["scope"], "qq-judge");
        assert_eq!(value["key"], 1);
        assert_eq!(value["reported"], true);
        // 没有归属与指纹时不写这几个键:老行照旧能解析。
        assert!(value.get("sess").is_none());
        assert!(value.get("same").is_none());

        let empty = format_line(
            "ts",
            "chat",
            "p",
            "m",
            2,
            "llm_2",
            None,
            &RecordContext::default(),
        );
        let value: serde_json::Value = serde_json::from_str(&empty).unwrap();
        assert_eq!(value["prompt"], 0);
        assert_eq!(value["reported"], false);
        assert_eq!(value["key"], 3);
    }

    #[test]
    fn an_identified_line_carries_session_turn_and_the_prefix_verdict() {
        let line = format_line(
            "ts",
            "chat",
            "opencodego",
            "mimo-v2.6-flash",
            0,
            "llm_3",
            None,
            &RecordContext {
                session: Some("default"),
                turn: Some("turn_1"),
                prefix: Some(&PrefixDiff {
                    messages: 42,
                    previous: Some(40),
                    same: 37,
                    rewritten_at: Some((37, "tool")),
                    tools_changed: true,
                    tools_diff: Some("+read".to_string()),
                    system: 0xabcdef0123456789,
                    tools: 7,
                }),
            },
        );
        let value: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["sess"], "default");
        assert_eq!(value["turn"], "turn_1");
        assert_eq!(value["msgs"], 42);
        assert_eq!(value["prev"], 40);
        assert_eq!(value["same"], 37);
        assert_eq!(value["at"], 37);
        assert_eq!(value["role"], "tool");
        assert_eq!(value["tools_changed"], true);
        assert_eq!(value["tools_diff"], "+read");
        assert_eq!(value["sys"], "abcdef012345");
        assert_eq!(value["tools_hash"], "000000000000");
    }

    #[test]
    fn sink_rotates_by_date_and_prunes_stale_files() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().to_path_buf();
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("cache-usage.2026-07-01.jsonl"), "old\n").unwrap();
        fs::write(dir.join("miyu.2026-07-01.log"), "keep\n").unwrap();
        let mut sink = Sink {
            dir: dir.clone(),
            enabled: true,
            retention_days: 14,
            current: None,
        };
        sink.write_line("2026-08-10", "{\"a\":1}").unwrap();
        sink.write_line("2026-08-10", "{\"a\":2}").unwrap();
        sink.write_line("2026-08-11", "{\"a\":3}").unwrap();
        let first = fs::read_to_string(dir.join("cache-usage.2026-08-10.jsonl")).unwrap();
        assert_eq!(first.lines().count(), 2);
        let second = fs::read_to_string(dir.join("cache-usage.2026-08-11.jsonl")).unwrap();
        assert_eq!(second.lines().count(), 1);
        assert!(!dir.join("cache-usage.2026-07-01.jsonl").exists());
        assert!(dir.join("miyu.2026-07-01.log").exists());
    }
}
