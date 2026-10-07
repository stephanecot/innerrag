//! Usage feedback: what was searched, which passages the agents actually cited, and which
//! questions the base could not answer. Stored with the project (`feedback.jsonl`), it feeds
//! two lists: the documentation gaps (unanswered questions, grouped) and proposed reference
//! questions for the evaluation set (question → cited passages).

use std::collections::{HashMap, HashSet};
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::db::{self, as_i64, as_str, rows, Graph};
use crate::eval::{self, Expect, Question};
use crate::{AppState, Invalid};

const FILE: &str = "feedback.jsonl";
/// Two questions closer than this (cosine of their embeddings) are the same gap.
const SAME_QUESTION: f32 = 0.9;
/// Only the most recent records are analysed.
const MAX_RECORDS: usize = 20_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// The cited passages answer the question.
    Answered,
    /// They answer part of it.
    Partial,
    /// The base does not answer it.
    NotFound,
}

impl Outcome {
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "answered" => Ok(Self::Answered),
            "partial" => Ok(Self::Partial),
            "not_found" => Ok(Self::NotFound),
            other => Err(Invalid(format!("unknown outcome `{other}`: use answered, partial or not_found")).into()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Record {
    /// A search and what it returned.
    Search { at: i64, query: String, channel: String, chunks: Vec<String>, best: Option<f64> },
    /// An agent reporting the passages its answer relied on.
    Cite { at: i64, question: String, chunks: Vec<String>, outcome: Outcome, channel: String },
    /// A question set aside from the gaps and proposals.
    Dismiss { at: i64, key: String },
}

fn now_ms() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp_nanos() as i64 / 1_000_000
}

fn path(graph: &Graph) -> PathBuf {
    graph.files_dir().parent().map_or_else(|| PathBuf::from(FILE), |dir| dir.join(FILE))
}

/// Same question despite case, spacing and final punctuation.
pub fn key(question: &str) -> String {
    let lower = question.to_lowercase();
    let words: Vec<&str> = lower.split_whitespace().collect();
    words.join(" ").trim_end_matches(['?', '!', '.', ' ', '…']).to_string()
}

fn append(graph: &Graph, rec: &Record) -> Result<()> {
    let p = path(graph);
    let mut file = OpenOptions::new().create(true).append(true).open(&p).with_context(|| format!("opening {}", p.display()))?;
    writeln!(file, "{}", serde_json::to_string(rec)?)?;
    Ok(())
}

fn load(graph: &Graph) -> Vec<Record> {
    let Ok(file) = std::fs::File::open(path(graph)) else { return Vec::new() };
    let mut out: Vec<Record> = BufReader::new(file)
        .lines()
        .map_while(std::result::Result::ok)
        .filter_map(|l| serde_json::from_str(&l).ok())
        .collect();
    if out.len() > MAX_RECORDS {
        out.drain(..out.len() - MAX_RECORDS);
    }
    out
}

/// Logs a search; failures only warn, a search must never fail because of its log.
pub fn record_search(graph: &Graph, query: &str, channel: &str, chunks: Vec<String>, best: Option<f64>) {
    let rec = Record::Search { at: now_ms(), query: query.trim().to_string(), channel: channel.into(), chunks, best };
    if let Err(e) = append(graph, &rec) {
        tracing::warn!("cannot write feedback: {e:#}");
    }
}

pub fn cite(graph: &Graph, question: &str, chunks: Vec<String>, outcome: Outcome, channel: &str) -> Result<()> {
    let question = question.trim();
    if question.is_empty() {
        return Err(Invalid("question is required".into()).into());
    }
    if outcome != Outcome::NotFound && chunks.is_empty() {
        return Err(Invalid("give the ids of the passages used (chunk ids from search_knowledge), or outcome not_found".into()).into());
    }
    // Only passages that exist: an agent may garble an id.
    let known: HashSet<String> = if chunks.is_empty() {
        HashSet::new()
    } else {
        rows(&graph.reader()?, "MATCH (c:Chunk) WHERE c.id IN $ids RETURN c.id", vec![("ids", db::strings(chunks.clone()))])?
            .iter()
            .map(|r| as_str(&r[0]))
            .collect()
    };
    let unknown: Vec<&String> = chunks.iter().filter(|c| !known.contains(*c)).collect();
    if !unknown.is_empty() {
        return Err(Invalid(format!("unknown passage ids: {}", unknown.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", "))).into());
    }
    append(graph, &Record::Cite { at: now_ms(), question: question.into(), chunks, outcome, channel: channel.into() })
}

pub fn dismiss(graph: &Graph, question_key: &str) -> Result<()> {
    append(graph, &Record::Dismiss { at: now_ms(), key: key(question_key) })
}

#[derive(Debug, Serialize)]
pub struct Gap {
    pub key: String,
    /// The most frequent wording.
    pub question: String,
    /// Other wordings grouped with it.
    pub variants: Vec<String>,
    pub count: usize,
    /// Searches that found nothing above the threshold.
    pub unanswered_searches: usize,
    /// Agents that reported the base did not (fully) answer.
    pub reported: usize,
    pub last_at: i64,
    /// Closest the base came (best similarity of a search).
    pub best_similarity: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct CitedPassage {
    pub id: String,
    pub doc_id: String,
    pub doc_title: String,
    pub page: Option<i64>,
    pub excerpt: String,
}

#[derive(Debug, Serialize)]
pub struct Proposal {
    pub key: String,
    pub question: String,
    pub count: usize,
    pub last_at: i64,
    pub partial: bool,
    pub passages: Vec<CitedPassage>,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub searches: usize,
    pub unanswered_searches: usize,
    pub citations: usize,
    pub gaps: Vec<Gap>,
    pub proposals: Vec<Proposal>,
}

/// Embeddings of questions, kept between reports.
fn embedding_cache() -> &'static Mutex<HashMap<String, Vec<f32>>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Vec<f32>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn embed_all(state: &AppState, questions: &[String]) -> Result<Vec<Vec<f32>>> {
    let mut cache = embedding_cache().lock().map_err(|_| anyhow::anyhow!("cache lock poisoned"))?;
    let mut out = Vec::with_capacity(questions.len());
    for q in questions {
        if let Some(v) = cache.get(q) {
            out.push(v.clone());
            continue;
        }
        let v = state.embedder.embed_query(q)?;
        cache.insert(q.clone(), v.clone());
        out.push(v);
    }
    Ok(out)
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    // Embeddings are L2-normalised.
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

pub fn report(state: &AppState, graph: &Graph) -> Result<Report> {
    let records = load(graph);
    let dismissed: HashSet<String> = records
        .iter()
        .filter_map(|r| if let Record::Dismiss { key, .. } = r { Some(key.clone()) } else { None })
        .collect();
    let in_eval: HashSet<String> = eval::load(graph)?.questions.iter().map(|q| key(&q.question)).collect();

    let mut searches = 0;
    let mut unanswered = 0;
    let mut citations = 0;
    // Gap evidence per question key: (wording, unanswered search?, reported?, at, best).
    let mut evidence: Vec<(String, bool, bool, i64, Option<f64>)> = Vec::new();
    // Latest citation per question key.
    let mut cited: HashMap<String, (String, Vec<String>, bool, i64, usize)> = HashMap::new();
    for r in &records {
        match r {
            Record::Search { at, query, chunks, best, .. } => {
                searches += 1;
                if chunks.is_empty() {
                    unanswered += 1;
                    evidence.push((query.clone(), true, false, *at, *best));
                }
            }
            Record::Cite { at, question, chunks, outcome, .. } => {
                citations += 1;
                if *outcome != Outcome::Answered {
                    evidence.push((question.clone(), false, true, *at, None));
                }
                if !chunks.is_empty() {
                    let entry = cited.entry(key(question)).or_insert_with(|| (question.clone(), Vec::new(), false, 0, 0));
                    entry.4 += 1;
                    if *at >= entry.3 {
                        *entry = (question.clone(), chunks.clone(), *outcome == Outcome::Partial, *at, entry.4);
                    }
                }
            }
            Record::Dismiss { .. } => {}
        }
    }
    evidence.retain(|e| !dismissed.contains(&key(&e.0)));

    // Group the unanswered questions by meaning.
    let wordings: Vec<String> = {
        let mut seen = HashSet::new();
        evidence.iter().map(|e| e.0.clone()).filter(|q| seen.insert(key(q))).collect()
    };
    let vectors = embed_all(state, &wordings)?;
    let mut group_of: HashMap<String, usize> = HashMap::new();
    let mut leaders: Vec<usize> = Vec::new();
    for (i, w) in wordings.iter().enumerate() {
        let group = leaders.iter().position(|&l| cosine(&vectors[l], &vectors[i]) >= SAME_QUESTION).unwrap_or_else(|| {
            leaders.push(i);
            leaders.len() - 1
        });
        group_of.insert(key(w), group);
    }
    let mut gaps: Vec<Gap> = Vec::new();
    let mut by_group: HashMap<usize, Vec<&(String, bool, bool, i64, Option<f64>)>> = HashMap::new();
    for e in &evidence {
        if let Some(g) = group_of.get(&key(&e.0)) {
            by_group.entry(*g).or_default().push(e);
        }
    }
    for items in by_group.values() {
        let mut freq: HashMap<String, (String, usize)> = HashMap::new();
        for e in items {
            freq.entry(key(&e.0)).or_insert_with(|| (e.0.clone(), 0)).1 += 1;
        }
        let mut wordings: Vec<(String, usize)> = freq.into_values().collect();
        wordings.sort_by(|a, b| b.1.cmp(&a.1));
        let question = wordings[0].0.clone();
        gaps.push(Gap {
            key: key(&question),
            variants: wordings.iter().skip(1).map(|w| w.0.clone()).collect(),
            question,
            count: items.len(),
            unanswered_searches: items.iter().filter(|e| e.1).count(),
            reported: items.iter().filter(|e| e.2).count(),
            last_at: items.iter().map(|e| e.3).max().unwrap_or(0),
            best_similarity: items.iter().filter_map(|e| e.4).reduce(f64::max),
        });
    }
    gaps.sort_by(|a, b| b.count.cmp(&a.count).then(b.last_at.cmp(&a.last_at)));

    // Proposed reference questions: cited answers not yet in the evaluation set.
    let mut proposals: Vec<Proposal> = Vec::new();
    let wanted: Vec<(String, (String, Vec<String>, bool, i64, usize))> =
        cited.into_iter().filter(|(k, _)| !in_eval.contains(k) && !dismissed.contains(k)).collect();
    let ids: Vec<String> = wanted.iter().flat_map(|(_, c)| c.1.clone()).collect();
    let passages: HashMap<String, CitedPassage> = if ids.is_empty() {
        HashMap::new()
    } else {
        rows(
            &graph.reader()?,
            "MATCH (d:Document)-[:HAS_CHUNK]->(c:Chunk) WHERE c.id IN $ids RETURN c.id, d.id, d.title, c.page, c.text",
            vec![("ids", db::strings(ids))],
        )?
        .iter()
        .map(|r| {
            let id = as_str(&r[0]);
            let text = as_str(&r[4]);
            let body = text.lines().skip_while(|l| l.contains(" › ")).collect::<Vec<_>>().join(" ");
            let excerpt: String = body.chars().take(220).collect();
            (
                id.clone(),
                CitedPassage { id, doc_id: as_str(&r[1]), doc_title: as_str(&r[2]), page: Some(as_i64(&r[3])).filter(|p| *p > 0), excerpt },
            )
        })
        .collect()
    };
    for (k, (question, chunks, partial, at, count)) in wanted {
        let found: Vec<CitedPassage> = chunks
            .iter()
            .filter_map(|c| passages.get(c))
            .map(|p| CitedPassage { id: p.id.clone(), doc_id: p.doc_id.clone(), doc_title: p.doc_title.clone(), page: p.page, excerpt: p.excerpt.clone() })
            .collect();
        if found.is_empty() {
            continue; // the cited passages were deleted since
        }
        proposals.push(Proposal { key: k, question, count, last_at: at, partial, passages: found });
    }
    proposals.sort_by(|a, b| b.last_at.cmp(&a.last_at));

    Ok(Report { searches, unanswered_searches: unanswered, citations, gaps, proposals })
}

/// Adds a question to the evaluation set: from its cited passages, or as out of scope.
pub fn accept(state: &AppState, graph: &Graph, question_key: &str, out_of_scope: bool) -> Result<eval::EvalSet> {
    let wanted = key(question_key);
    let mut set = eval::load(graph)?;
    if set.questions.iter().any(|q| key(&q.question) == wanted) {
        return Ok(set);
    }
    let expect = if out_of_scope {
        Expect { none: true, ..Expect::default() }
    } else {
        let report = report(state, graph)?;
        let proposal = report
            .proposals
            .into_iter()
            .find(|p| p.key == wanted)
            .ok_or_else(|| Invalid(format!("no cited answer for \"{question_key}\"")))?;
        // The first cited passage defines the expected document.
        let first = &proposal.passages[0];
        let pages: Vec<i64> = proposal.passages.iter().filter(|p| p.doc_id == first.doc_id).filter_map(|p| p.page).collect();
        // Without pages, a few words of the passage identify it.
        let contains = if pages.is_empty() {
            vec![first.excerpt.split_whitespace().take(6).collect::<Vec<_>>().join(" ")]
        } else {
            Vec::new()
        };
        Expect { doc: Some(first.doc_id.clone()), pages, contains, none: false }
    };
    let question = load(graph)
        .iter()
        .rev()
        .find_map(|r| match r {
            Record::Cite { question, .. } | Record::Search { query: question, .. } if key(question) == wanted => Some(question.clone()),
            _ => None,
        })
        .unwrap_or_else(|| question_key.trim().to_string());
    set.questions.push(Question { id: String::new(), question, expect });
    eval::save(graph, set)
}
