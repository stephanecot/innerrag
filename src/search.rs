//! Local Graph RAG retrieval: vector search on chunks, fused with chunks reached
//! through entities related to the question.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::time::Instant;

use anyhow::Result;
use lbug::LogicalType;
use serde::{Deserialize, Serialize};

use crate::db::{self, as_f64, as_floats, as_i64, as_str, as_strings, rows, s, Graph, Structs};
use crate::ingest::normalize_tags;
use crate::{AppState, Invalid};

const MAX_NEIGHBOURS: i64 = 30;
/// Largest bonus the graph adds to a passage's similarity (passages mentioning the
/// most relevant entities get all of it).
const GRAPH_BOOST: f64 = 0.05;
/// Passages closer than this to a better-ranked one say the same thing: only the first is kept.
const NEAR_DUPLICATE: f32 = 0.95;
/// Largest bonus for passages that contain the question's words (BM25, normalised per query).
/// Keyword evidence may also bring a passage just under the threshold over it.
const KEYWORD_BOOST: f64 = 0.04;

#[derive(Debug, Deserialize)]
pub struct SearchRequest {
    pub query: String,
    pub k: Option<usize>,
    /// Disable graph expansion to compare with plain vector search.
    pub use_graph: Option<bool>,
    /// Disable keyword (BM25) search.
    pub use_keywords: Option<bool>,
    /// Order the passages with the cross-encoder (default: when installed).
    pub rerank: Option<bool>,
    /// Also search DRAFT documents (only PUBLISHED ones by default).
    pub include_drafts: Option<bool>,
    /// Restrict to documents carrying at least one of these tags.
    pub tags: Option<Vec<String>>,
    /// Minimum cosine similarity between the question and a passage (0 disables the filter).
    /// Defaults to the server setting.
    pub min_score: Option<f64>,
    /// How `context` is written for an agent: "full" (default) or "map" (see context.rs).
    #[serde(default)]
    pub mode: Option<String>,
    /// Ceiling of `context` in tokens.
    #[serde(default)]
    pub budget: Option<usize>,
    /// Passages already sent in this session are not repeated in `context`.
    #[serde(default)]
    pub session_id: Option<String>,
    /// Reuse a recent identical search (default true; evaluation turns it off to time searches).
    #[serde(default)]
    pub cache: Option<bool>,
}

/// Document filter appended to queries that bind the document as `d`.
const DOC_FILTER: &str = "($drafts OR d.status = 'PUBLISHED') AND (size($tags) = 0 OR any(t IN $tags WHERE list_contains(d.tags, t)))";

#[derive(Debug, Clone, Serialize)]
pub struct ChunkHit {
    pub id: String,
    pub doc_id: String,
    pub doc_title: String,
    pub doc_status: String,
    pub idx: i64,
    /// PDF page the passage starts on.
    pub page: Option<i64>,
    pub text: String,
    /// Ranking score: similarity plus the graph bonus.
    pub score: f64,
    /// Cosine similarity between the question and the passage (0–1).
    pub similarity: f64,
    /// Raw weight of the question's entities in this passage (None: not reached through the graph).
    pub graph_score: Option<f64>,
    /// Part of `score` that comes from the graph (0 to 0.05).
    pub graph_boost: f64,
    /// Part of `score` that comes from matching the question's words (0 to 0.04).
    pub keyword_boost: f64,
    /// Found only thanks to the graph (not among the nearest passages by meaning).
    pub via_graph: bool,
    /// Found only thanks to its words (keyword search).
    pub via_keywords: bool,
    /// Cross-encoder relevance (0–1) when reranking ran; the passages are then ordered by it.
    pub rerank_score: Option<f64>,
    pub entities: Vec<String>,
}

#[derive(Debug, Serialize, Clone)]
pub struct EntityHit {
    pub id: String,
    pub name: String,
    pub label: String,
    pub score: f64,
    /// `query` (named in the question), `vector` (close to the question) or `neighbour`.
    pub via: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RelationHit {
    pub source: String,
    pub target: String,
    pub weight: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchResponse {
    pub query: String,
    pub chunks: Vec<ChunkHit>,
    pub entities: Vec<EntityHit>,
    pub relations: Vec<RelationHit>,
    /// Markdown context ready to be handed to an LLM.
    pub context: String,
    /// Similarity threshold applied.
    pub min_score: f64,
    /// Candidates dropped because they were below the threshold.
    pub below_threshold: usize,
    /// Best similarity among all candidates (tells how close the base came).
    pub best_similarity: Option<f64>,
    /// Passages dropped because a better-ranked one says nearly the same thing.
    pub near_duplicates: usize,
    /// Served from the cache of recent searches (same question and settings, unchanged data).
    pub cached: bool,
    pub millis: u128,
}

pub fn search(state: &AppState, graph: &Graph, req: SearchRequest) -> Result<SearchResponse> {
    let started = Instant::now();
    let query = req.query.trim().to_string();
    if query.is_empty() {
        return Err(Invalid("query is required".into()).into());
    }
    let k = req.k.unwrap_or(8).clamp(1, 50);
    let min_score = req.min_score.unwrap_or(state.config.min_score).clamp(0.0, 1.0);
    let use_graph = req.use_graph.unwrap_or(true);
    let drafts = lbug::Value::Bool(req.include_drafts.unwrap_or(false));
    let tag_list = normalize_tags(req.tags.as_deref().unwrap_or_default());
    let version = graph.version();
    let use_cache = req.cache.unwrap_or(true);
    let cache_key = {
        let mut sorted_tags = tag_list.clone();
        sorted_tags.sort();
        serde_json::json!([
            version,
            query.split_whitespace().collect::<Vec<_>>().join(" "),
            k,
            min_score,
            use_graph,
            req.use_keywords.unwrap_or(true),
            req.rerank.unwrap_or(true),
            req.include_drafts.unwrap_or(false),
            sorted_tags,
        ])
        .to_string()
    };
    if use_cache {
        if let Some(hit) = graph.cache.get(&cache_key) {
            let mut res = (*hit).clone();
            res.cached = true;
            res.millis = started.elapsed().as_millis();
            return Ok(res);
        }
    }
    let tags = db::strings(tag_list);
    let qv = state.embedder.embed_query(&query)?;
    let conn = graph.reader()?;

    // 1. Vector ranking of chunks.
    let vector_hits: Vec<(String, f64)> = rows(
        &conn,
        // Oversample, since filtered-out documents are removed after the k-NN search.
        &format!(
            "CALL QUERY_VECTOR_INDEX('Chunk', 'chunk_vec', $q, $k)
             WITH node, distance
             MATCH (d:Document)-[:HAS_CHUNK]->(node)
             WHERE {DOC_FILTER}
             RETURN node.id, distance ORDER BY distance LIMIT $limit"
        ),
        vec![
            ("q", db::floats(&qv)),
            ("k", db::i((k * 8).clamp(40, 400) as i64)),
            ("limit", db::i((k * 2).max(10) as i64)),
            ("drafts", drafts.clone()),
            ("tags", tags.clone()),
        ],
    )?
    .iter()
    .map(|r| (as_str(&r[0]), 1.0 - as_f64(&r[1])))
    .collect();

    // 2. Seed entities: named in the question, or semantically close to it.
    let mut entities: HashMap<String, EntityHit> = HashMap::new();
    if use_graph {
        let mentions = state.ner.extract_quick(&query, &state.config.ner_labels, std::time::Duration::from_millis(400));
        let names: Vec<String> = mentions.iter().map(|m| m.name.to_lowercase()).collect();
        if !names.is_empty() {
            for r in rows(
                &conn,
                "MATCH (e:Entity) WHERE lower(e.name) IN $names RETURN e.id, e.name, e.label",
                vec![("names", db::strings(names))],
            )? {
                let hit = EntityHit {
                    id: as_str(&r[0]),
                    name: as_str(&r[1]),
                    label: as_str(&r[2]),
                    score: 1.0,
                    via: "query".into(),
                };
                entities.insert(hit.id.clone(), hit);
            }
        }
        for r in rows(
            &conn,
            "CALL QUERY_VECTOR_INDEX('Entity', 'entity_vec', $q, 10)
             RETURN node.id, node.name, node.label, distance ORDER BY distance",
            vec![("q", db::floats(&qv))],
        )? {
            let distance = as_f64(&r[3]);
            if distance > state.config.entity_seed_distance {
                continue;
            }
            let id = as_str(&r[0]);
            entities.entry(id.clone()).or_insert(EntityHit {
                id,
                name: as_str(&r[1]),
                label: as_str(&r[2]),
                score: 1.0 - distance,
                via: "vector".into(),
            });
        }
    }

    // 3. One-hop expansion through co-occurrence relations.
    if !entities.is_empty() {
        let seed_ids: Vec<String> = entities.keys().cloned().collect();
        let neighbours = rows(
            &conn,
            "UNWIND $ids AS id
             MATCH (a:Entity {id: id})-[r:RELATED]-(b:Entity)
             WHERE NOT b.id IN $ids
             RETURN a.id, b.id, b.name, b.label, r.weight
             ORDER BY r.weight DESC LIMIT $limit",
            vec![("ids", db::strings(seed_ids)), ("limit", db::i(MAX_NEIGHBOURS))],
        )?;
        let max_weight = neighbours.iter().map(|r| as_i64(&r[4])).max().unwrap_or(1).max(1) as f64;
        for r in neighbours {
            let seed_score = entities.get(&as_str(&r[0])).map_or(0.0, |e| e.score);
            let score = 0.5 * seed_score * (as_i64(&r[4]) as f64 / max_weight);
            let id = as_str(&r[1]);
            let entry = entities.entry(id.clone()).or_insert(EntityHit {
                id,
                name: as_str(&r[2]),
                label: as_str(&r[3]),
                score: 0.0,
                via: "neighbour".into(),
            });
            entry.score = entry.score.max(score);
        }
    }

    // 4. Graph ranking: chunks scored by the relevant entities they mention.
    let mut graph_hits: Vec<(String, f64)> = Vec::new();
    if !entities.is_empty() {
        let mut weighted = Structs::new(&[("id", LogicalType::String), ("w", LogicalType::Double)]);
        for e in entities.values() {
            weighted.push(vec![s(&e.id), db::f(e.score)]);
        }
        graph_hits = rows(
            &conn,
            &format!(
                "UNWIND $rows AS r
                 MATCH (e:Entity {{id: r.id}})<-[:MENTIONS]-(c:Chunk)<-[:HAS_CHUNK]-(d:Document)
                 WHERE {DOC_FILTER}
                 RETURN c.id, sum(r.w) AS score ORDER BY score DESC LIMIT $limit"
            ),
            vec![
                ("rows", weighted.into_value()),
                ("limit", db::i((k * 2).max(10) as i64)),
                ("drafts", drafts.clone()),
                ("tags", tags.clone()),
            ],
        )?
        .iter()
        .map(|r| (as_str(&r[0]), as_f64(&r[1])))
        .collect();
    }

    // 4b. Keyword ranking (BM25 with English and French stems); documents are filtered below.
    let keyword_scores: HashMap<String, f64> = if req.use_keywords.unwrap_or(true) {
        graph.search_keywords(&query, (k * 4).clamp(20, 100)).into_iter().collect()
    } else {
        HashMap::new()
    };
    let max_keyword = keyword_scores.values().copied().fold(0.0, f64::max).max(f64::EPSILON);
    let mut keyword_hits: Vec<(String, f64)> = keyword_scores.iter().map(|(id, s)| (id.clone(), *s)).collect();
    keyword_hits.sort_by(|a, b| b.1.total_cmp(&a.1));
    keyword_hits.truncate((k * 2).max(10));

    // 5. Every candidate (by meaning, through the graph or by its words) gets its similarity to
    //    the question, then small bonuses for the question's entities and words.
    let vector_ids: std::collections::HashSet<&String> = vector_hits.iter().map(|(id, _)| id).collect();
    let graph_scores: HashMap<String, f64> = graph_hits.iter().cloned().collect();
    let max_graph = graph_hits.iter().map(|(_, s)| *s).fold(0.0, f64::max).max(f64::EPSILON);
    let mut candidates: Vec<String> = vector_hits.iter().map(|(id, _)| id.clone()).collect();
    candidates.extend(graph_hits.iter().map(|(id, _)| id.clone()).filter(|id| !vector_ids.contains(id)));
    let graph_ids: std::collections::HashSet<String> = graph_hits.iter().map(|(id, _)| id.clone()).collect();
    for (id, _) in &keyword_hits {
        if !vector_ids.contains(id) && !graph_ids.contains(id) {
            candidates.push(id.clone());
        }
    }
    let dim = state.embedder.dim();
    type Detail = (String, String, String, i64, String, Vec<String>, Option<i64>, f64, Vec<f32>);
    let details: HashMap<String, Detail> = rows(
        &conn,
        &format!(
            "UNWIND $ids AS id
             MATCH (d:Document)-[:HAS_CHUNK]->(c:Chunk {{id: id}})
             WHERE {DOC_FILTER}
             OPTIONAL MATCH (c)-[:MENTIONS]->(e:Entity)
             RETURN c.id, d.id, d.title, d.status, c.idx, c.text, collect(e.name), c.page,
                    array_cosine_similarity(c.embedding, CAST($q AS FLOAT[{dim}])), c.embedding"
        ),
        vec![("ids", db::strings(candidates.clone())), ("q", db::floats(&qv)), ("drafts", drafts.clone()), ("tags", tags.clone())],
    )?
    .iter()
    .map(|r| {
        let page = Some(as_i64(&r[7])).filter(|p| *p > 0);
        let detail = (
            as_str(&r[1]), as_str(&r[2]), as_str(&r[3]), as_i64(&r[4]), as_str(&r[5]),
            as_strings(&r[6]), page, as_f64(&r[8]), as_floats(&r[9]),
        );
        (as_str(&r[0]), detail)
    })
    .collect();
    let best_similarity = details.values().map(|d| d.7).reduce(f64::max);
    let mut below_threshold = 0;
    let mut chunks: Vec<ChunkHit> = candidates
        .into_iter()
        .filter_map(|id| {
            let (doc_id, doc_title, doc_status, idx, text, entities, page, similarity, _) = details.get(&id)?.clone();
            let keyword_boost = keyword_scores.get(&id).map_or(0.0, |s| KEYWORD_BOOST * s / max_keyword);
            if similarity + keyword_boost < min_score {
                below_threshold += 1;
                return None;
            }
            let graph_score = graph_scores.get(&id).copied();
            let graph_boost = graph_score.map_or(0.0, |g| GRAPH_BOOST * g / max_graph);
            let in_vector = vector_ids.contains(&id);
            let via_graph = !in_vector && graph_ids.contains(&id);
            let via_keywords = !in_vector && !via_graph;
            Some(ChunkHit {
                id, doc_id, doc_title, doc_status, idx, page, text, entities,
                score: similarity + graph_boost + keyword_boost,
                similarity, graph_score, graph_boost, keyword_boost, via_graph, via_keywords,
                rerank_score: None,
            })
        })
        .collect();
    chunks.sort_by(|a, b| b.score.total_cmp(&a.score));
    // The cross-encoder re-reads the best candidates with the question and reorders them.
    if let Some(reranker) = state.reranker.as_ref().filter(|_| req.rerank.unwrap_or(true)) {
        chunks.truncate((k * 2).max(16));
        // Scores already computed for this question (another k, mode or budget) are reused.
        let ids: Vec<String> = chunks.iter().map(|c| c.id.clone()).collect();
        let known = graph.cache.scores(version, &query, &ids);
        let todo: Vec<usize> = (0..chunks.len()).filter(|i| !known.contains_key(&chunks[*i].id)).collect();
        let texts: Vec<&str> = todo.iter().map(|i| chunks[*i].text.as_str()).collect();
        let fresh = if texts.is_empty() { Vec::new() } else { reranker.score(&query, &texts)? };
        graph.cache.put_scores(version, &query, todo.iter().zip(&fresh).map(|(i, s)| (chunks[*i].id.clone(), *s)));
        for c in chunks.iter_mut() {
            c.rerank_score = known.get(&c.id).copied();
        }
        for (i, s) in todo.iter().zip(fresh) {
            chunks[*i].rerank_score = Some(s);
        }
        chunks.sort_by(|a, b| b.rerank_score.unwrap_or(0.0).total_cmp(&a.rerank_score.unwrap_or(0.0)));
    }
    // Near-identical passages (a listing printed twice, a repeated paragraph) waste the agent's tokens.
    let mut kept: Vec<&Vec<f32>> = Vec::new();
    let mut near_duplicates = 0;
    chunks.retain(|c| {
        let Some(v) = details.get(&c.id).map(|d| &d.8).filter(|v| !v.is_empty()) else { return true };
        if kept.len() >= k {
            return true; // truncated below anyway
        }
        if kept.iter().any(|w| v.iter().zip(w.iter()).map(|(a, b)| a * b).sum::<f32>() >= NEAR_DUPLICATE) {
            near_duplicates += 1;
            return false;
        }
        kept.push(v);
        true
    });
    chunks.truncate(k);

    // 7. Relations among the selected entities.
    let mut entity_list: Vec<EntityHit> = entities.into_values().collect();
    entity_list.sort_by(|a, b| b.score.total_cmp(&a.score));
    let entity_ids: Vec<String> = entity_list.iter().map(|e| e.id.clone()).collect();
    let relations: Vec<RelationHit> = if entity_ids.len() > 1 {
        rows(
            &conn,
            "MATCH (a:Entity)-[r:RELATED]->(b:Entity)
             WHERE a.id IN $ids AND b.id IN $ids
             RETURN a.id, b.id, r.weight ORDER BY r.weight DESC",
            vec![("ids", db::strings(entity_ids))],
        )?
        .iter()
        .map(|r| RelationHit { source: as_str(&r[0]), target: as_str(&r[1]), weight: as_i64(&r[2]) })
        .collect()
    } else {
        Vec::new()
    };

    // Without any relevant passage, related entities would only mislead.
    if chunks.is_empty() {
        entity_list.clear();
    }
    let relations = if chunks.is_empty() { Vec::new() } else { relations };
    let mut context = build_context(&chunks, &entity_list, &relations);
    if chunks.is_empty() {
        context = match best_similarity {
            Some(best) => format!(
                "No passage is relevant enough for this question (best similarity {best:.2}, threshold {min_score:.2}). \
                 The knowledge base probably does not cover it.\n"
            ),
            None => "The knowledge base has no published passage to search.\n".to_string(),
        };
    }
    let response = SearchResponse {
        query,
        chunks,
        entities: entity_list,
        relations,
        context,
        min_score,
        below_threshold,
        best_similarity,
        near_duplicates,
        cached: false,
        millis: started.elapsed().as_millis(),
    };
    if use_cache {
        graph.cache.put(cache_key, std::sync::Arc::new(response.clone()));
    }
    Ok(response)
}

fn build_context(chunks: &[ChunkHit], entities: &[EntityHit], relations: &[RelationHit]) -> String {
    let mut out = String::new();
    if !entities.is_empty() {
        let names: HashMap<&str, &str> = entities.iter().map(|e| (e.id.as_str(), e.name.as_str())).collect();
        out.push_str("## Related entities\n");
        for e in entities.iter().take(15) {
            let mut linked: Vec<(&str, i64)> = relations
                .iter()
                .filter_map(|r| {
                    if r.source == e.id {
                        names.get(r.target.as_str()).map(|n| (*n, r.weight))
                    } else if r.target == e.id {
                        names.get(r.source.as_str()).map(|n| (*n, r.weight))
                    } else {
                        None
                    }
                })
                .collect();
            linked.sort_by(|a, b| b.1.cmp(&a.1));
            let _ = write!(out, "- {} ({})", e.name, e.label);
            if !linked.is_empty() {
                let list: Vec<String> = linked.iter().take(6).map(|(n, w)| format!("{n} ×{w}")).collect();
                let _ = write!(out, " — co-occurs with: {}", list.join(", "));
            }
            out.push('\n');
        }
        out.push('\n');
    }
    out.push_str("## Passages\n");
    for (n, c) in chunks.iter().enumerate() {
        let location = c.page.map_or_else(|| format!("passage {}", c.idx + 1), |p| format!("page {p}"));
        let rerank = c.rerank_score.map(|r| format!(", rerank {r:.2}")).unwrap_or_default();
        let _ = write!(out, "\n[{}] {} ({location}, score {:.2}{rerank})\n{}\n", n + 1, c.doc_title, c.score, c.text);
    }
    if chunks.is_empty() {
        out.push_str("\nNo relevant passage found.\n");
    }
    out
}
