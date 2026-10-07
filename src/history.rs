//! Call history: every consuming operation (search, ingestion, exploration, Cypher…)
//! from REST, MCP or the UI, with its duration and the size of the context it returned.
//! Stored as JSON lines in `<data>/history/`, outside the projects so it never ends up in git.

use std::collections::VecDeque;
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Instant;

use anyhow::Result;
use serde::{Deserialize, Serialize};

const KEEP_IN_MEMORY: usize = 20_000;
const ROTATE_BYTES: u64 = 20 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallRecord {
    /// Unix time in milliseconds.
    pub at: i64,
    /// `mcp`, `rest` or `ui`.
    pub channel: String,
    pub project: String,
    pub operation: String,
    pub detail: String,
    /// Short human summary of the result ("3 passages", "9 entités"…).
    pub result: String,
    /// Characters of context handed back to the caller (what an agent will read).
    pub context_chars: usize,
    /// Rough token estimate of that context (chars / 4).
    pub context_tokens: usize,
    pub duration_ms: u64,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub struct History {
    path: PathBuf,
    recent: Mutex<VecDeque<CallRecord>>,
}

fn now_ms() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp_nanos() as i64 / 1_000_000
}

/// "1 passage", "3 passages".
pub fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n > 1 { many } else { one })
}

pub fn estimate_tokens(chars: usize) -> usize {
    chars.div_ceil(4)
}

impl History {
    pub fn open(dir: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("calls.jsonl");
        let mut recent = VecDeque::new();
        if let Ok(file) = std::fs::File::open(&path) {
            for line in BufReader::new(file).lines().map_while(Result::ok) {
                if let Ok(rec) = serde_json::from_str::<CallRecord>(&line) {
                    if recent.len() == KEEP_IN_MEMORY {
                        recent.pop_front();
                    }
                    recent.push_back(rec);
                }
            }
        }
        Ok(Self { path, recent: Mutex::new(recent) })
    }

    pub fn record(&self, rec: CallRecord) {
        if let Err(e) = self.append(&rec) {
            tracing::warn!("cannot write call history: {e:#}");
        }
        if let Ok(mut recent) = self.recent.lock() {
            if recent.len() == KEEP_IN_MEMORY {
                recent.pop_front();
            }
            recent.push_back(rec);
        }
    }

    fn append(&self, rec: &CallRecord) -> Result<()> {
        if std::fs::metadata(&self.path).is_ok_and(|m| m.len() > ROTATE_BYTES) {
            let rotated = self.path.with_file_name(format!("calls-{}.jsonl", now_ms()));
            std::fs::rename(&self.path, rotated)?;
        }
        let mut file = OpenOptions::new().create(true).append(true).open(&self.path)?;
        writeln!(file, "{}", serde_json::to_string(rec)?)?;
        Ok(())
    }

    pub fn query(&self, filter: &HistoryFilter) -> HistoryView {
        let since = now_ms() - (filter.hours.unwrap_or(24).clamp(1, 24 * 90) as i64) * 3_600_000;
        let recent = self.recent.lock().map(|r| r.clone()).unwrap_or_default();
        let matching: Vec<CallRecord> = recent
            .into_iter()
            .rev()
            .filter(|r| r.at >= since)
            .filter(|r| filter.channel.is_empty() || r.channel == filter.channel)
            .filter(|r| filter.operation.is_empty() || r.operation == filter.operation)
            .filter(|r| filter.project.is_empty() || r.project == filter.project)
            .filter(|r| !filter.errors || !r.ok)
            .collect();

        let mut durations: Vec<u64> = matching.iter().map(|r| r.duration_ms).collect();
        durations.sort_unstable();
        let totals = Totals {
            calls: matching.len(),
            context_tokens: matching.iter().map(|r| r.context_tokens).sum(),
            median_ms: durations.get(durations.len() / 2).copied().unwrap_or(0),
            errors: matching.iter().filter(|r| !r.ok).count(),
        };

        // Hourly buckets from `since` to now, oldest first.
        let hours = filter.hours.unwrap_or(24).clamp(1, 24 * 90) as usize;
        let bucket_ms = if hours <= 48 { 3_600_000 } else { 86_400_000 };
        let buckets = (hours as i64 * 3_600_000 / bucket_ms) as usize;
        let start = now_ms() - buckets as i64 * bucket_ms;
        let mut series: Vec<Bucket> =
            (0..buckets).map(|i| Bucket { at: start + i as i64 * bucket_ms, ..Bucket::default() }).collect();
        for r in &matching {
            let idx = ((r.at - start) / bucket_ms).clamp(0, buckets as i64 - 1) as usize;
            let b = &mut series[idx];
            b.calls += 1;
            if r.channel == "mcp" {
                b.mcp_tokens += r.context_tokens;
            } else {
                b.other_tokens += r.context_tokens;
            }
        }

        let limit = filter.limit.unwrap_or(200).clamp(1, 2000);
        HistoryView { totals, series, calls: matching.into_iter().take(limit).collect() }
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct HistoryFilter {
    pub hours: Option<u32>,
    #[serde(default)]
    pub channel: String,
    #[serde(default)]
    pub operation: String,
    #[serde(default)]
    pub project: String,
    #[serde(default)]
    pub errors: bool,
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct Totals {
    pub calls: usize,
    pub context_tokens: usize,
    pub median_ms: u64,
    pub errors: usize,
}

#[derive(Debug, Default, Serialize)]
pub struct Bucket {
    pub at: i64,
    pub calls: usize,
    pub mcp_tokens: usize,
    pub other_tokens: usize,
}

#[derive(Debug, Serialize)]
pub struct HistoryView {
    pub totals: Totals,
    pub series: Vec<Bucket>,
    pub calls: Vec<CallRecord>,
}

/// Measures one call; `finish` records it.
pub struct Call {
    started: Instant,
    channel: String,
    project: String,
    operation: &'static str,
    detail: String,
}

impl Call {
    pub fn start(channel: &str, project: &str, operation: &'static str, detail: impl Into<String>) -> Self {
        let mut detail: String = detail.into();
        if detail.len() > 300 {
            detail.truncate(detail.floor_char_boundary(300));
            detail.push('…');
        }
        Self { started: Instant::now(), channel: channel.into(), project: project.into(), operation, detail }
    }

    /// Records the outcome; `summary` gives the result line and the context size in characters.
    pub fn finish<T>(self, history: &History, outcome: &Result<T>, summary: impl FnOnce(&T) -> (String, usize)) {
        let (result, context_chars, error) = match outcome {
            Ok(v) => {
                let (result, chars) = summary(v);
                (result, chars, None)
            }
            Err(e) => ("erreur".to_string(), 0, Some(format!("{e:#}"))),
        };
        history.record(CallRecord {
            at: now_ms(),
            channel: self.channel,
            project: self.project,
            operation: self.operation.to_string(),
            detail: self.detail,
            result,
            context_chars,
            context_tokens: estimate_tokens(context_chars),
            duration_ms: self.started.elapsed().as_millis() as u64,
            ok: error.is_none(),
            error,
        });
    }
}
