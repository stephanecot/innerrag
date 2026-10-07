//! What an agent reads back from a search, at the size it chooses.
//!
//! - `full`: the passages in full (the default).
//! - `map`: one line per passage (id, heading path, page, score and its sentence closest to the
//!   question); the agent then unfolds what it needs with `read_passages`.
//! - `budget`: a ceiling in tokens; lower-ranked passages are left out, and said so.
//! - `session_id`: passages already sent in the same session are replaced by a reminder.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::db::{self, as_i64, as_str, rows, Graph};
use crate::history::estimate_tokens;
use crate::search::{ChunkHit, SearchResponse};
use crate::{AppState, Invalid};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Full,
    Map,
}

impl Mode {
    pub fn parse(s: Option<&str>) -> Result<Self> {
        match s.unwrap_or("full") {
            "full" => Ok(Self::Full),
            "map" => Ok(Self::Map),
            other => Err(Invalid(format!("unknown mode `{other}`: use full or map")).into()),
        }
    }
}

pub struct Options {
    pub mode: Mode,
    pub budget: Option<usize>,
    pub session: Option<String>,
}

const SESSION_TTL: Duration = Duration::from_secs(4 * 3600);
const MAX_SESSIONS: usize = 500;
const SENTENCE_CHARS: usize = 240;

/// Passages already sent, per project and session.
fn sessions() -> &'static Mutex<HashMap<String, (Instant, HashSet<String>)>> {
    static SESSIONS: OnceLock<Mutex<HashMap<String, (Instant, HashSet<String>)>>> = OnceLock::new();
    SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn session_key(project: &str, session: &str) -> String {
    format!("{project}/{session}")
}

fn already_sent(project: &str, session: Option<&str>) -> HashSet<String> {
    let Some(session) = session else { return HashSet::new() };
    let Ok(mut map) = sessions().lock() else { return HashSet::new() };
    map.retain(|_, (at, _)| at.elapsed() < SESSION_TTL);
    map.get(&session_key(project, session)).map(|(_, ids)| ids.clone()).unwrap_or_default()
}

fn mark_sent(project: &str, session: Option<&str>, ids: impl IntoIterator<Item = String>) {
    let Some(session) = session else { return };
    let Ok(mut map) = sessions().lock() else { return };
    if map.len() >= MAX_SESSIONS {
        if let Some(oldest) = map.iter().min_by_key(|(_, (at, _))| *at).map(|(k, _)| k.clone()) {
            map.remove(&oldest);
        }
    }
    let entry = map.entry(session_key(project, session)).or_insert_with(|| (Instant::now(), HashSet::new()));
    entry.0 = Instant::now();
    entry.1.extend(ids);
}

/// Heading path ("A › B") and body of a passage.
pub fn split_heading(text: &str) -> (Option<&str>, &str) {
    match text.split_once('\n') {
        Some((first, rest)) if first.contains(" › ") => (Some(first), rest),
        _ => (None, text),
    }
}

fn sentences(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in body.lines() {
        let mut start = 0;
        let chars: Vec<(usize, char)> = line.char_indices().collect();
        for (n, (i, c)) in chars.iter().enumerate() {
            let next_is_space = chars.get(n + 1).is_some_and(|(_, c)| c.is_whitespace());
            if matches!(c, '.' | '!' | '?' | ';') && next_is_space {
                out.push(line[start..=*i].trim().to_string());
                start = i + c.len_utf8();
            }
        }
        out.push(line[start..].trim().to_string());
    }
    out.retain(|s| s.chars().filter(|c| c.is_alphabetic()).count() >= 15);
    // Headings and running titles carry no answer: keep real sentences when there are some.
    let real: Vec<String> = out.iter().filter(|s| s.ends_with(['.', '!', '?', ';', ':']) && s.contains(' ')).cloned().collect();
    if real.is_empty() { out } else { real }
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max).collect::<String>().trim_end())
    }
}

/// The sentence of each passage closest to the question (by embedding).
fn best_sentences(state: &AppState, query: &str, chunks: &[&ChunkHit]) -> Result<HashMap<String, String>> {
    let mut owners: Vec<(String, String)> = Vec::new();
    for c in chunks {
        let (_, body) = split_heading(&c.text);
        for s in sentences(body).into_iter().take(14) {
            owners.push((c.id.clone(), s));
        }
    }
    let mut best: HashMap<String, (f32, String)> = HashMap::new();
    if !owners.is_empty() {
        let q = state.embedder.embed_query(query)?;
        let texts: Vec<String> = owners.iter().map(|(_, s)| s.clone()).collect();
        let vectors = state.embedder.embed_passages(&texts)?;
        for ((id, sentence), v) in owners.into_iter().zip(vectors) {
            let score: f32 = q.iter().zip(&v).map(|(a, b)| a * b).sum();
            let entry = best.entry(id).or_insert((f32::MIN, String::new()));
            if score > entry.0 {
                *entry = (score, sentence);
            }
        }
    }
    Ok(chunks
        .iter()
        .map(|c| {
            let fallback = || clip(split_heading(&c.text).1.trim(), SENTENCE_CHARS);
            let s = best.remove(&c.id).map_or_else(fallback, |(_, s)| clip(&s, SENTENCE_CHARS));
            (c.id.clone(), s)
        })
        .collect())
}

fn location(c: &ChunkHit) -> String {
    c.page.map_or_else(|| format!("passage {}", c.idx + 1), |p| format!("page {p}"))
}

/// Renders a search for an agent, within the requested size.
pub fn render(state: &AppState, project: &str, res: &SearchResponse, opts: &Options) -> Result<String> {
    if res.chunks.is_empty() {
        return Ok(res.context.clone());
    }
    let session = opts.session.as_deref();
    let sent = already_sent(project, session);
    let budget = opts.budget.map(|b| b.max(100));
    let mut out = String::new();

    if !res.entities.is_empty() {
        let names: Vec<String> = res.entities.iter().take(if opts.mode == Mode::Map { 8 } else { 12 }).map(|e| format!("{} ({})", e.name, e.label)).collect();
        let _ = writeln!(out, "Related entities: {}\n", names.join(", "));
    }

    let fresh: Vec<&ChunkHit> = res.chunks.iter().filter(|c| !sent.contains(&c.id)).collect();
    let sentences = if opts.mode == Mode::Map { best_sentences(state, &res.query, &fresh)? } else { HashMap::new() };
    match opts.mode {
        Mode::Map => out.push_str("## Passage map\nOne line per passage, best first. Unfold the ones you need with read_passages(ids).\n\n"),
        Mode::Full => out.push_str("## Passages\n"),
    }

    let mut shown: Vec<String> = Vec::new();
    let mut left_out = 0;
    for (n, c) in res.chunks.iter().enumerate() {
        let rank = n + 1;
        let heading = split_heading(&c.text).0.map(|h| format!(" › {h}")).unwrap_or_default();
        let item = if sent.contains(&c.id) {
            format!("\n[{rank}] `{}` {}{heading} ({}): already provided earlier in this session.\n", c.id, c.doc_title, location(c))
        } else {
            match opts.mode {
                Mode::Map => format!(
                    "- [{rank}] `{}` {}{heading} ({}, score {:.2}): {}\n",
                    c.id, c.doc_title, location(c), c.score, sentences.get(&c.id).map_or("", String::as_str)
                ),
                Mode::Full => format!("\n[{rank}] `{}` {} ({}, score {:.2})\n{}\n", c.id, c.doc_title, location(c), c.score, c.text),
            }
        };
        if let Some(b) = budget {
            // The best passage is always given, even over budget.
            if !shown.is_empty() && estimate_tokens(out.len() + item.len()) > b {
                left_out += 1;
                continue;
            }
        }
        out.push_str(&item);
        if !sent.contains(&c.id) {
            shown.push(c.id.clone());
        }
    }
    if left_out > 0 {
        let _ = writeln!(
            out,
            "\n{left_out} more relevant passages left out to stay within {} tokens: raise budget, use mode \"map\", or read them with read_passages.",
            budget.unwrap_or_default()
        );
    }
    // In map mode only the lines were sent: the passages themselves are still to be read.
    if opts.mode == Mode::Full {
        mark_sent(project, session, shown);
    }
    Ok(out)
}

/// Full text of passages, with `window` neighbours on each side, grouped by document.
pub fn read_passages(graph: &Graph, project: &str, ids: &[String], window: i64, session: Option<&str>) -> Result<String> {
    if ids.is_empty() {
        return Err(Invalid("give at least one passage id".into()).into());
    }
    let window = window.clamp(0, 3);
    let conn = graph.reader()?;
    let anchors = rows(
        &conn,
        "MATCH (d:Document)-[:HAS_CHUNK]->(c:Chunk) WHERE c.id IN $ids RETURN c.id, d.id, d.title, c.idx",
        vec![("ids", db::strings(ids.to_vec()))],
    )?;
    let found: HashSet<String> = anchors.iter().map(|r| as_str(&r[0])).collect();
    let mut out = String::new();
    let mut sent = Vec::new();
    // Ranges per document, merged when they touch.
    let mut ranges: Vec<(String, String, i64, i64)> = anchors
        .iter()
        .map(|r| (as_str(&r[1]), as_str(&r[2]), as_i64(&r[3]) - window, as_i64(&r[3]) + window))
        .collect();
    ranges.sort_by(|a, b| a.0.cmp(&b.0).then(a.2.cmp(&b.2)));
    let mut merged: Vec<(String, String, i64, i64)> = Vec::new();
    for r in ranges {
        match merged.last_mut() {
            Some(last) if last.0 == r.0 && r.2 <= last.3 + 1 => last.3 = last.3.max(r.3),
            _ => merged.push(r),
        }
    }
    for (doc, title, from, to) in merged {
        let passages = rows(
            &conn,
            "MATCH (c:Chunk) WHERE c.doc_id = $doc AND c.idx >= $from AND c.idx <= $to RETURN c.id, c.idx, c.page, c.text ORDER BY c.idx",
            vec![("doc", db::s(&doc)), ("from", db::i(from)), ("to", db::i(to))],
        )?;
        let _ = writeln!(out, "## {title}");
        for p in passages {
            let id = as_str(&p[0]);
            let page = Some(as_i64(&p[2])).filter(|p| *p > 0).map(|p| format!(", page {p}")).unwrap_or_default();
            let marker = if ids.contains(&id) { "" } else { " (context)" };
            let _ = writeln!(out, "\n`{id}`{page}{marker}\n{}", as_str(&p[3]));
            sent.push(id);
        }
        out.push('\n');
    }
    let missing: Vec<&String> = ids.iter().filter(|i| !found.contains(*i)).collect();
    if !missing.is_empty() {
        let _ = writeln!(out, "Unknown passage ids: {}", missing.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", "));
    }
    mark_sent(project, session, sent);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_heading_and_sentences() {
        let (h, body) = split_heading("Guide › Install\nRun the installer first. Then restart the machine; it is required.");
        assert_eq!(h, Some("Guide › Install"));
        assert_eq!(sentences(body), vec!["Run the installer first.", "Then restart the machine;"]);
        assert_eq!(sentences("Advanced AsyncTask and Progress Dialogs\nCall cancel() to stop the running task."), vec!["Call cancel() to stop the running task."]);
        assert_eq!(split_heading("No heading here\nsecond line").0, None);
    }
}
