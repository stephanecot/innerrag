//! Read-only views over the graph, shared by the REST API, MCP tools and the UI.

use std::collections::HashMap;

use anyhow::Result;
use serde::Serialize;
use serde_json::Value as Json;

use crate::db::{self, as_i64, as_str, as_strings, rows, s, Graph};
use crate::Invalid;

const MAX_CYPHER_ROWS: usize = 1000;

#[derive(Serialize)]
pub struct LabelCount {
    pub label: String,
    pub count: i64,
}

#[derive(Serialize)]
pub struct Stats {
    pub documents: i64,
    pub published: i64,
    pub drafts: i64,
    pub chunks: i64,
    pub entities: i64,
    pub mentions: i64,
    pub relations: i64,
    pub labels: Vec<LabelCount>,
}

fn count(conn: &lbug::Connection, query: &str) -> Result<i64> {
    Ok(rows(conn, query, vec![])?.first().map_or(0, |r| as_i64(&r[0])))
}

pub fn stats(graph: &Graph) -> Result<Stats> {
    let conn = graph.reader()?;
    Ok(Stats {
        documents: count(&conn, "MATCH (d:Document) RETURN count(d)")?,
        published: count(&conn, "MATCH (d:Document) WHERE d.status = 'PUBLISHED' RETURN count(d)")?,
        drafts: count(&conn, "MATCH (d:Document) WHERE d.status <> 'PUBLISHED' RETURN count(d)")?,
        chunks: count(&conn, "MATCH (c:Chunk) RETURN count(c)")?,
        entities: count(&conn, "MATCH (e:Entity) RETURN count(e)")?,
        mentions: count(&conn, "MATCH ()-[m:MENTIONS]->() RETURN count(m)")?,
        relations: count(&conn, "MATCH ()-[r:RELATED]->() RETURN count(r)")?,
        labels: rows(&conn, "MATCH (e:Entity) RETURN e.label, count(e) AS n ORDER BY n DESC", vec![])?
            .iter()
            .map(|r| LabelCount { label: as_str(&r[0]), count: as_i64(&r[1]) })
            .collect(),
    })
}

#[derive(Serialize)]
pub struct DocumentSummary {
    pub id: String,
    pub title: String,
    pub source: String,
    pub status: String,
    pub creator: String,
    pub tags: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
    pub chunks: i64,
    pub entities: i64,
}

const DOC_FIELDS: &str = "d.id, d.title, d.source, d.status, d.creator, d.tags, \
    CAST(d.created_at AS STRING) AS created, CAST(d.updated_at AS STRING) AS updated";

fn doc_summary(r: &[lbug::Value], chunks: i64, entities: i64) -> DocumentSummary {
    DocumentSummary {
        id: as_str(&r[0]),
        title: as_str(&r[1]),
        source: as_str(&r[2]),
        status: as_str(&r[3]),
        creator: as_str(&r[4]),
        tags: as_strings(&r[5]),
        created_at: as_str(&r[6]),
        updated_at: as_str(&r[7]),
        chunks,
        entities,
    }
}

#[derive(Debug, Default, serde::Deserialize)]
pub struct DocumentFilter {
    #[serde(default)]
    pub q: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub tag: String,
}

pub fn list_documents(graph: &Graph, filter: &DocumentFilter) -> Result<Vec<DocumentSummary>> {
    let conn = graph.reader()?;
    let entity_counts: HashMap<String, i64> = rows(
        &conn,
        "MATCH (d:Document)-[:HAS_CHUNK]->(:Chunk)-[:MENTIONS]->(e:Entity) RETURN d.id, count(DISTINCT e)",
        vec![],
    )?
    .iter()
    .map(|r| (as_str(&r[0]), as_i64(&r[1])))
    .collect();
    Ok(rows(
        &conn,
        &format!(
            "MATCH (d:Document)
             WHERE ($q = '' OR lower(d.title) CONTAINS lower($q))
               AND ($status = '' OR d.status = upper($status))
               AND ($tag = '' OR list_contains(d.tags, $tag))
             OPTIONAL MATCH (d)-[:HAS_CHUNK]->(c:Chunk)
             RETURN {DOC_FIELDS}, count(c) ORDER BY updated DESC"
        ),
        vec![("q", s(filter.q.trim())), ("status", s(filter.status.trim())), ("tag", s(filter.tag.trim()))],
    )?
    .iter()
    .map(|r| doc_summary(r, as_i64(&r[8]), entity_counts.get(&as_str(&r[0])).copied().unwrap_or(0)))
    .collect())
}

#[derive(Serialize)]
pub struct TagCount {
    pub tag: String,
    pub count: i64,
}

pub fn tags(graph: &Graph) -> Result<Vec<TagCount>> {
    let conn = graph.reader()?;
    Ok(rows(&conn, "MATCH (d:Document) UNWIND d.tags AS t RETURN t, count(*) AS n ORDER BY n DESC, t", vec![])?
        .iter()
        .map(|r| TagCount { tag: as_str(&r[0]), count: as_i64(&r[1]) })
        .collect())
}

#[derive(Serialize)]
pub struct ChunkView {
    pub id: String,
    pub idx: i64,
    pub text: String,
    pub entities: Vec<String>,
    /// PDF page the passage starts on.
    pub page: Option<i64>,
}

fn page_of(v: &lbug::Value) -> Option<i64> {
    Some(as_i64(v)).filter(|p| *p > 0)
}

#[derive(Serialize)]
pub struct DocumentDetail {
    #[serde(flatten)]
    pub summary: DocumentSummary,
    pub metadata: Json,
    /// The first passages only; `/passages` pages through all of them.
    #[serde(rename = "passages")]
    pub chunks: Vec<ChunkView>,
    pub passage_count: i64,
}

const PASSAGE_QUERY: &str = "MATCH (:Document {id: $id})-[:HAS_CHUNK]->(c:Chunk)
     WITH c ORDER BY c.idx SKIP $skip LIMIT $limit
     OPTIONAL MATCH (c)-[:MENTIONS]->(e:Entity)
     RETURN c.id, c.idx, c.text, collect(e.name), c.page ORDER BY c.idx";

fn passages(conn: &lbug::Connection, id: &str, skip: usize, limit: usize) -> Result<Vec<ChunkView>> {
    Ok(rows(
        conn,
        PASSAGE_QUERY,
        vec![("id", s(id)), ("skip", db::i(skip as i64)), ("limit", db::i(limit as i64))],
    )?
    .iter()
    .map(|r| ChunkView {
        id: as_str(&r[0]),
        idx: as_i64(&r[1]),
        text: as_str(&r[2]),
        entities: as_strings(&r[3]),
        page: page_of(&r[4]),
    })
    .collect())
}

pub fn get_document(graph: &Graph, id: &str) -> Result<Option<DocumentDetail>> {
    let conn = graph.reader()?;
    let Some(doc) = rows(
        &conn,
        &format!("MATCH (d:Document {{id: $id}}) RETURN {DOC_FIELDS}, d.metadata"),
        vec![("id", s(id))],
    )?
    .into_iter()
    .next() else {
        return Ok(None);
    };
    let counts = rows(
        &conn,
        "MATCH (:Document {id: $id})-[:HAS_CHUNK]->(c:Chunk)
         OPTIONAL MATCH (c)-[:MENTIONS]->(e:Entity)
         RETURN count(DISTINCT c), count(DISTINCT e)",
        vec![("id", s(id))],
    )?;
    let (passage_count, entities) = counts.first().map_or((0, 0), |r| (as_i64(&r[0]), as_i64(&r[1])));
    let chunks = passages(&conn, id, 0, 20)?;
    let metadata = serde_json::from_str(&as_str(&doc[8])).unwrap_or(Json::Null);
    Ok(Some(DocumentDetail { summary: doc_summary(&doc, passage_count, entities), metadata, chunks, passage_count }))
}

#[derive(Serialize)]
pub struct PassagePage {
    pub total: i64,
    pub offset: usize,
    pub passages: Vec<ChunkView>,
}

pub fn document_passages(graph: &Graph, id: &str, offset: usize, limit: usize) -> Result<PassagePage> {
    let conn = graph.reader()?;
    let total = rows(&conn, "MATCH (:Document {id: $id})-[:HAS_CHUNK]->(c:Chunk) RETURN count(c)", vec![("id", s(id))])?
        .first()
        .map_or(0, |r| as_i64(&r[0]));
    Ok(PassagePage { total, offset, passages: passages(&conn, id, offset, limit.clamp(1, 200))? })
}

#[derive(Serialize)]
pub struct DocumentContent {
    pub id: String,
    pub title: String,
    /// Markdown of the whole document (PDFs carry `<!-- page N -->` markers).
    pub content: String,
    pub metadata: Json,
}

pub fn document_content(graph: &Graph, id: &str) -> Result<Option<DocumentContent>> {
    let conn = graph.reader()?;
    Ok(rows(&conn, "MATCH (d:Document {id: $id}) RETURN d.id, d.title, d.content, d.metadata", vec![("id", s(id))])?
        .first()
        .map(|r| DocumentContent {
            id: as_str(&r[0]),
            title: as_str(&r[1]),
            content: as_str(&r[2]),
            metadata: serde_json::from_str(&as_str(&r[3])).unwrap_or(Json::Null),
        }))
}

/// The stored original of a document: (path on disk, file name to show).
pub fn document_file(graph: &Graph, id: &str) -> Result<Option<(std::path::PathBuf, String)>> {
    let conn = graph.reader()?;
    let Some(meta) = rows(&conn, "MATCH (d:Document {id: $id}) RETURN d.metadata", vec![("id", s(id))])?
        .first()
        .map(|r| as_str(&r[0]))
    else {
        return Ok(None);
    };
    let Some(rel) = crate::ingest::stored_file(&meta) else { return Ok(None) };
    let name = serde_json::from_str::<Json>(&meta)
        .ok()
        .and_then(|m| m.get("file")?.get("name")?.as_str().map(str::to_string))
        .unwrap_or_else(|| rel.clone());
    let path = graph.files_dir().join(rel.trim_start_matches("files/"));
    Ok(path.exists().then_some((path, name)))
}

#[derive(Serialize, Clone)]
pub struct EntitySummary {
    pub id: String,
    pub name: String,
    pub label: String,
    pub mentions: i64,
    /// Mentions coming from PUBLISHED documents; 0 means the entity only exists in drafts.
    pub published_mentions: i64,
}

fn entity_rows(rows: Vec<db::Row>) -> Vec<EntitySummary> {
    rows.iter()
        .map(|r| EntitySummary {
            id: as_str(&r[0]),
            name: as_str(&r[1]),
            label: as_str(&r[2]),
            mentions: as_i64(&r[3]),
            published_mentions: r.get(4).map_or(0, as_i64),
        })
        .collect()
}

/// Mention counts of `e`, split by document status (expects `e` bound).
const MENTION_COUNTS: &str = "OPTIONAL MATCH (e)<-[m:MENTIONS]-(:Chunk)<-[:HAS_CHUNK]-(md:Document)
     WITH e, count(m) AS n, sum(CASE WHEN md.status = 'PUBLISHED' THEN 1 ELSE 0 END) AS p";

pub fn list_entities(
    graph: &Graph,
    q: &str,
    label: &str,
    include_drafts: bool,
    limit: usize,
) -> Result<Vec<EntitySummary>> {
    let conn = graph.reader()?;
    Ok(entity_rows(rows(
        &conn,
        &format!(
            "MATCH (e:Entity)
             WHERE ($q = '' OR lower(e.name) CONTAINS lower($q)) AND ($label = '' OR e.label = $label)
             {MENTION_COUNTS}
             WHERE $drafts OR p > 0
             RETURN e.id, e.name, e.label, n, p ORDER BY n DESC, e.name LIMIT $limit"
        ),
        vec![
            ("q", s(q.trim())),
            ("label", s(label.trim())),
            ("drafts", lbug::Value::Bool(include_drafts)),
            ("limit", db::i(limit.clamp(1, 1000) as i64)),
        ],
    )?))
}

#[derive(Serialize)]
pub struct Neighbour {
    #[serde(flatten)]
    pub entity: EntitySummary,
    pub weight: i64,
    pub strength: f64,
}

#[derive(Serialize)]
pub struct Passage {
    pub chunk_id: String,
    pub doc_id: String,
    pub doc_title: String,
    pub doc_status: String,
    pub page: Option<i64>,
    pub idx: i64,
    pub text: String,
}

#[derive(Serialize)]
pub struct EntityDetail {
    #[serde(flatten)]
    pub entity: EntitySummary,
    pub neighbours: Vec<Neighbour>,
    pub passages: Vec<Passage>,
}

pub fn get_entity(graph: &Graph, id: &str) -> Result<Option<EntityDetail>> {
    let conn = graph.reader()?;
    let Some(entity) = entity_rows(rows(
        &conn,
        &format!("MATCH (e:Entity {{id: $id}}) {MENTION_COUNTS} RETURN e.id, e.name, e.label, n, p"),
        vec![("id", s(id))],
    )?)
    .into_iter()
    .next() else {
        return Ok(None);
    };
    let neighbours = rows(
        &conn,
        "MATCH (a:Entity {id: $id})-[r:RELATED]-(b:Entity)
         RETURN b.id, b.name, b.label, r.weight,
                COUNT { MATCH (a)<-[:MENTIONS]-(:Chunk) }, COUNT { MATCH (b)<-[:MENTIONS]-(:Chunk) } AS nb
         ORDER BY r.weight DESC, b.name LIMIT 50",
        vec![("id", s(id))],
    )?
    .iter()
    .map(|r| {
        let weight = as_i64(&r[3]);
        Neighbour {
            entity: EntitySummary {
                id: as_str(&r[0]),
                name: as_str(&r[1]),
                label: as_str(&r[2]),
                mentions: as_i64(&r[5]),
                published_mentions: 0,
            },
            weight,
            strength: strength(weight, as_i64(&r[4]), as_i64(&r[5])),
        }
    })
    .collect();
    let passages = rows(
        &conn,
        "MATCH (e:Entity {id: $id})<-[m:MENTIONS]-(c:Chunk)<-[:HAS_CHUNK]-(d:Document)
         RETURN c.id, d.id, d.title, d.status, c.idx, c.text, c.page ORDER BY m.score DESC LIMIT 20",
        vec![("id", s(id))],
    )?
    .iter()
    .map(|r| Passage {
        chunk_id: as_str(&r[0]),
        doc_id: as_str(&r[1]),
        doc_title: as_str(&r[2]),
        doc_status: as_str(&r[3]),
        page: page_of(&r[6]),
        idx: as_i64(&r[4]),
        text: as_str(&r[5]),
    })
    .collect();
    Ok(Some(EntityDetail { entity, neighbours, passages }))
}

#[derive(Serialize)]
pub struct RelationDetail {
    pub a: EntitySummary,
    pub b: EntitySummary,
    pub weight: i64,
    pub strength: f64,
    /// Passages where both appear.
    pub passages: Vec<Passage>,
}

/// Why two entities are linked: the passages that cite both.
pub fn relation(graph: &Graph, a: &str, b: &str) -> Result<Option<RelationDetail>> {
    let conn = graph.reader()?;
    let ends = entity_rows(rows(
        &conn,
        &format!("UNWIND [$a, $b] AS id MATCH (e:Entity {{id: id}}) {MENTION_COUNTS} RETURN e.id, e.name, e.label, n, p"),
        vec![("a", s(a)), ("b", s(b))],
    )?);
    let (Some(ea), Some(eb)) = (ends.iter().find(|e| e.id == a).cloned(), ends.iter().find(|e| e.id == b).cloned()) else {
        return Ok(None);
    };
    let passages: Vec<Passage> = rows(
        &conn,
        "MATCH (x:Entity {id: $a})<-[:MENTIONS]-(c:Chunk)-[:MENTIONS]->(y:Entity {id: $b}), (d:Document)-[:HAS_CHUNK]->(c)
         RETURN c.id, d.id, d.title, d.status, c.idx, c.text, c.page ORDER BY d.title, c.idx",
        vec![("a", s(a)), ("b", s(b))],
    )?
    .iter()
    .map(|r| Passage {
        chunk_id: as_str(&r[0]),
        doc_id: as_str(&r[1]),
        doc_title: as_str(&r[2]),
        doc_status: as_str(&r[3]),
        page: page_of(&r[6]),
        idx: as_i64(&r[4]),
        text: as_str(&r[5]),
    })
    .collect();
    let weight = passages.len() as i64;
    let strength = strength(weight, ea.mentions, eb.mentions);
    Ok(Some(RelationDetail { a: ea, b: eb, weight, strength, passages: passages.into_iter().take(30).collect() }))
}

/// Resolves a free-text entity name (exact match first, then substring).
pub fn find_entity(graph: &Graph, name: &str) -> Result<Option<String>> {
    let conn = graph.reader()?;
    let found = rows(
        &conn,
        "MATCH (e:Entity) WHERE lower(e.name) = lower($name) OR e.id = $name
         OPTIONAL MATCH (e)<-[m:MENTIONS]-(:Chunk)
         RETURN e.id, count(m) AS n ORDER BY n DESC LIMIT 1",
        vec![("name", s(name.trim()))],
    )?;
    if let Some(r) = found.first() {
        return Ok(Some(as_str(&r[0])));
    }
    // Same name written differently: "ContentProvider", "content provider", "Content-Providers".
    let wanted = compact(name);
    if !wanted.is_empty() {
        let all = rows(
            &conn,
            "MATCH (e:Entity) RETURN e.id, e.name, COUNT { MATCH (e)<-[:MENTIONS]-(:Chunk) } AS n",
            vec![],
        )?;
        let best = all
            .iter()
            .filter(|r| compact(&as_str(&r[1])) == wanted)
            .max_by_key(|r| as_i64(&r[2]));
        if let Some(r) = best {
            return Ok(Some(as_str(&r[0])));
        }
    }
    Ok(list_entities(graph, name, "", true, 1)?.into_iter().next().map(|e| e.id))
}

/// Lowercased letters and digits only, without a final plural "s".
fn compact(name: &str) -> String {
    let mut out: String = name.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect();
    if out.len() > 3 && out.ends_with('s') && !out.ends_with("ss") {
        out.pop();
    }
    out
}

#[derive(Serialize)]
pub struct Edge {
    pub source: String,
    pub target: String,
    /// Passages where both entities appear.
    pub weight: i64,
    /// Share of the passages citing either entity that cite both (0–1, Jaccard): high when the
    /// link is specific, low when one side is cited everywhere.
    pub strength: f64,
}

/// Jaccard strength of a link from its weight and the passage counts of both ends.
fn strength(weight: i64, a: i64, b: i64) -> f64 {
    let union = (a + b - weight).max(weight).max(1);
    weight as f64 / union as f64
}

const LINK_COUNTS: &str = "COUNT { MATCH (a)<-[:MENTIONS]-(:Chunk) }, COUNT { MATCH (b)<-[:MENTIONS]-(:Chunk) }";

#[derive(Serialize)]
pub struct GraphView {
    pub nodes: Vec<EntitySummary>,
    pub edges: Vec<Edge>,
}

fn edges_between(conn: &lbug::Connection, ids: Vec<String>, min_weight: i64) -> Result<Vec<Edge>> {
    Ok(rows(
        conn,
        &format!(
            "MATCH (a:Entity)-[r:RELATED]->(b:Entity)
             WHERE a.id IN $ids AND b.id IN $ids AND r.weight >= $w
             RETURN a.id, b.id, r.weight, {LINK_COUNTS}"
        ),
        vec![("ids", db::strings(ids)), ("w", db::i(min_weight))],
    )?
    .iter()
    .map(|r| {
        let weight = as_i64(&r[2]);
        Edge { source: as_str(&r[0]), target: as_str(&r[1]), weight, strength: strength(weight, as_i64(&r[3]), as_i64(&r[4])) }
    })
    .collect())
}

/// The most mentioned entities and the relations among them.
pub fn graph(graph: &Graph, limit: usize, min_weight: i64, label: &str, include_drafts: bool) -> Result<GraphView> {
    let nodes = list_entities(graph, "", label, include_drafts, limit)?;
    let conn = graph.reader()?;
    let edges = edges_between(&conn, nodes.iter().map(|n| n.id.clone()).collect(), min_weight)?;
    Ok(GraphView { nodes, edges })
}

/// An entity, its strongest neighbours, and the relations among all of them.
pub fn neighbourhood(graph: &Graph, id: &str, limit: usize) -> Result<GraphView> {
    let conn = graph.reader()?;
    let mut nodes = entity_rows(rows(
        &conn,
        &format!("MATCH (e:Entity {{id: $id}}) {MENTION_COUNTS} RETURN e.id, e.name, e.label, n, p"),
        vec![("id", s(id))],
    )?);
    nodes.extend(entity_rows(rows(
        &conn,
        &format!(
            "MATCH (c:Entity {{id: $id}})-[r:RELATED]-(e:Entity)
             WITH e, r.weight AS w ORDER BY w DESC LIMIT $limit
             {MENTION_COUNTS}
             RETURN e.id, e.name, e.label, n, p"
        ),
        vec![("id", s(id)), ("limit", db::i(limit.clamp(1, 500) as i64))],
    )?));
    let edges = edges_between(&conn, nodes.iter().map(|n| n.id.clone()).collect(), 1)?;
    Ok(GraphView { nodes, edges })
}

#[derive(Serialize)]
pub struct CypherResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Json>>,
    pub truncated: bool,
}

/// Runs a user query, refusing anything that writes.
pub fn cypher(graph: &Graph, query: &str) -> Result<CypherResult> {
    // `is_read_only` does not cover statements that touch the filesystem or extensions.
    const FORBIDDEN: [&str; 7] = ["COPY", "LOAD", "INSTALL", "ATTACH", "DETACH", "EXPORT", "IMPORT"];
    let forbidden = query
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .map(str::to_uppercase)
        .find(|w| FORBIDDEN.contains(&w.as_str()));
    if let Some(word) = forbidden {
        return Err(Invalid(format!("{word} is not allowed in the console")).into());
    }
    let conn = graph.reader()?;
    let mut stmt = conn.prepare(query).map_err(|e| Invalid(e.to_string()))?;
    if !stmt.is_read_only() {
        return Err(Invalid("only read-only queries are allowed".into()).into());
    }
    let mut result = conn.execute(&mut stmt, vec![]).map_err(|e| Invalid(e.to_string()))?;
    let columns = result.get_column_names();
    let mut out = Vec::new();
    let mut truncated = false;
    for row in result.by_ref() {
        if out.len() == MAX_CYPHER_ROWS {
            truncated = true;
            break;
        }
        out.push(row.iter().map(db::to_json).collect());
    }
    Ok(CypherResult { columns, rows: out, truncated })
}
