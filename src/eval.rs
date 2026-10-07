//! Retrieval evaluation: a set of reference questions stored with the project (`eval.json`,
//! versioned with it) and metrics to tune k, the threshold and the graph/keyword boosts.

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::db::Graph;
use crate::search::{self, ChunkHit, SearchRequest};
use crate::AppState;

const FILE: &str = "eval.json";

/// What counts as a right answer. Every field given must match; `none` expects no passage.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Expect {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    /// Acceptable pages (±1, a passage may start on the previous page).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pages: Vec<i64>,
    /// Text the passage must contain (case-insensitive).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contains: Vec<String>,
    /// The question is out of scope: the right answer is no passage at all.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub none: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Question {
    pub id: String,
    pub question: String,
    pub expect: Expect,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct EvalSet {
    pub questions: Vec<Question>,
}

fn path(graph: &Graph) -> PathBuf {
    graph.files_dir().parent().map_or_else(|| PathBuf::from(FILE), |dir| dir.join(FILE))
}

pub fn load(graph: &Graph) -> Result<EvalSet> {
    let p = path(graph);
    if !p.exists() {
        return Ok(EvalSet::default());
    }
    let raw = std::fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?;
    serde_json::from_str(&raw).with_context(|| format!("parsing {}", p.display()))
}

pub fn save(graph: &Graph, mut set: EvalSet) -> Result<EvalSet> {
    for q in &mut set.questions {
        if q.id.trim().is_empty() {
            q.id = uuid::Uuid::new_v4().to_string()[..8].to_string();
        }
        q.question = q.question.trim().to_string();
    }
    set.questions.retain(|q| !q.question.is_empty());
    write(&path(graph), &set)?;
    Ok(set)
}

fn write(p: &Path, set: &EvalSet) -> Result<()> {
    std::fs::write(p, serde_json::to_string_pretty(set)? + "\n").with_context(|| format!("writing {}", p.display()))
}

fn matches(hit: &ChunkHit, expect: &Expect) -> bool {
    if expect.doc.as_ref().is_some_and(|d| d != &hit.doc_id) {
        return false;
    }
    if !expect.pages.is_empty() {
        let Some(page) = hit.page else { return false };
        if !expect.pages.iter().any(|p| (page - p).abs() <= 1) {
            return false;
        }
    }
    let text = hit.text.to_lowercase();
    expect.contains.iter().all(|c| text.contains(&c.to_lowercase()))
}

#[derive(Debug, Clone, Deserialize)]
pub struct RunRequest {
    pub k: Option<usize>,
    pub use_graph: Option<bool>,
    pub use_keywords: Option<bool>,
    pub rerank: Option<bool>,
    /// Thresholds to compare; the server default when empty.
    #[serde(default)]
    pub min_scores: Vec<f64>,
}

#[derive(Debug, Serialize)]
pub struct QuestionResult {
    pub id: String,
    pub question: String,
    pub expects_none: bool,
    /// 1-based rank of the first right passage.
    pub rank: Option<usize>,
    pub correct: bool,
    pub returned: usize,
    pub best_similarity: Option<f64>,
    pub top: Vec<TopHit>,
    pub millis: u128,
}

#[derive(Debug, Serialize)]
pub struct TopHit {
    pub doc_title: String,
    pub page: Option<i64>,
    pub score: f64,
    pub similarity: f64,
    pub right: bool,
}

#[derive(Debug, Serialize)]
pub struct Summary {
    pub min_score: f64,
    pub questions: usize,
    /// Answerable questions with the right passage first, in the top 3, in the top k.
    pub hit_at_1: f64,
    pub hit_at_3: f64,
    pub hit_at_k: f64,
    /// Mean reciprocal rank over answerable questions.
    pub mrr: f64,
    /// Out-of-scope questions correctly answered with nothing.
    pub rejected_out_of_scope: Option<f64>,
    /// Single figure to compare settings: mean of hit@k and out-of-scope rejection.
    pub overall: f64,
    pub avg_millis: f64,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub k: usize,
    pub runs: Vec<Summary>,
    /// Detail for the best threshold.
    pub best: usize,
    pub results: Vec<QuestionResult>,
}

fn run_once(state: &AppState, graph: &Graph, set: &EvalSet, req: &RunRequest, k: usize, min_score: f64) -> Result<(Summary, Vec<QuestionResult>)> {
    let mut results = Vec::new();
    for q in &set.questions {
        let started = Instant::now();
        let res = search::search(
            state,
            graph,
            SearchRequest {
                query: q.question.clone(),
                k: Some(k),
                use_graph: req.use_graph,
                use_keywords: req.use_keywords,
                rerank: req.rerank,
                include_drafts: Some(false),
                tags: None,
                min_score: Some(min_score),
                mode: None,
                budget: None,
                session_id: None,
                cache: Some(false),
            },
        )?;
        let rank = res.chunks.iter().position(|c| matches(c, &q.expect)).map(|i| i + 1);
        let correct = if q.expect.none { res.chunks.is_empty() } else { rank.is_some() };
        results.push(QuestionResult {
            id: q.id.clone(),
            question: q.question.clone(),
            expects_none: q.expect.none,
            rank,
            correct,
            returned: res.chunks.len(),
            best_similarity: res.best_similarity,
            top: res
                .chunks
                .iter()
                .take(3)
                .map(|c| TopHit {
                    doc_title: c.doc_title.clone(),
                    page: c.page,
                    score: c.score,
                    similarity: c.similarity,
                    right: matches(c, &q.expect),
                })
                .collect(),
            millis: started.elapsed().as_millis(),
        });
    }
    let answerable: Vec<&QuestionResult> = results.iter().filter(|r| !r.expects_none).collect();
    let none: Vec<&QuestionResult> = results.iter().filter(|r| r.expects_none).collect();
    let ratio = |n: usize, d: usize| if d == 0 { 0.0 } else { n as f64 / d as f64 };
    let hit = |limit: usize| ratio(answerable.iter().filter(|r| r.rank.is_some_and(|x| x <= limit)).count(), answerable.len());
    let mrr = if answerable.is_empty() {
        0.0
    } else {
        answerable.iter().map(|r| r.rank.map_or(0.0, |x| 1.0 / x as f64)).sum::<f64>() / answerable.len() as f64
    };
    let rejected = (!none.is_empty()).then(|| ratio(none.iter().filter(|r| r.correct).count(), none.len()));
    let hit_at_k = hit(k);
    let overall = match rejected {
        Some(r) if !answerable.is_empty() => (hit_at_k + r) / 2.0,
        Some(r) => r,
        None => hit_at_k,
    };
    let summary = Summary {
        min_score,
        questions: results.len(),
        hit_at_1: hit(1),
        hit_at_3: hit(3),
        hit_at_k,
        mrr,
        rejected_out_of_scope: rejected,
        overall,
        avg_millis: if results.is_empty() { 0.0 } else { results.iter().map(|r| r.millis as f64).sum::<f64>() / results.len() as f64 },
    };
    Ok((summary, results))
}

pub fn run(state: &AppState, graph: &Graph, req: RunRequest) -> Result<Report> {
    let set = load(graph)?;
    if set.questions.is_empty() {
        return Err(crate::Invalid("no reference question yet: add some first".into()).into());
    }
    let k = req.k.unwrap_or(8).clamp(1, 50);
    let mut thresholds = req.min_scores.clone();
    if thresholds.is_empty() {
        thresholds.push(state.config.min_score);
    }
    let mut runs = Vec::new();
    let mut details = Vec::new();
    for t in thresholds {
        let (summary, results) = run_once(state, graph, &set, &req, k, t.clamp(0.0, 1.0))?;
        runs.push(summary);
        details.push(results);
    }
    // Best: highest overall, then MRR, then the stricter threshold.
    let best = (0..runs.len())
        .max_by(|&a, &b| {
            runs[a]
                .overall
                .total_cmp(&runs[b].overall)
                .then(runs[a].mrr.total_cmp(&runs[b].mrr))
                .then(runs[a].min_score.total_cmp(&runs[b].min_score))
        })
        .unwrap_or(0);
    let results = details.swap_remove(best);
    Ok(Report { k, runs, best, results })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(doc: &str, page: Option<i64>, text: &str) -> ChunkHit {
        ChunkHit {
            id: "c".into(),
            doc_id: doc.into(),
            doc_title: "T".into(),
            doc_status: "PUBLISHED".into(),
            idx: 0,
            page,
            text: text.into(),
            score: 0.9,
            similarity: 0.9,
            graph_score: None,
            graph_boost: 0.0,
            keyword_boost: 0.0,
            via_graph: false,
            via_keywords: false,
            rerank_score: None,
            entities: vec![],
            images: Vec::new(),
        }
    }

    #[test]
    fn expectations() {
        let e = Expect { doc: Some("book".into()), pages: vec![41], contains: vec!["inputType".into()], none: false };
        assert!(matches(&hit("book", Some(42), "the INPUTTYPE attribute"), &e));
        assert!(!matches(&hit("book", Some(44), "inputType"), &e));
        assert!(!matches(&hit("other", Some(41), "inputType"), &e));
        assert!(!matches(&hit("book", Some(41), "nothing"), &e));
    }
}
