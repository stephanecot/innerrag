//! Document lifecycle: add, replace (chunk → embed → NER → graph write), edit
//! metadata, delete.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use lbug::{Connection, LogicalType};
use serde::{Deserialize, Serialize};

use crate::chunk::{chunk_document, chunk_plain};
use crate::db::{self, as_str, exec, rows, s, Graph, Structs};
use crate::extract::{first_heading, front_matter};
use crate::history::{count, Call};
use crate::jobs::{Job, Progress, Stage};
use crate::ner::Mention;
use crate::{AppState, Conflict, Invalid, NotFound};

/// Normalises a request before it is queued, so bad input fails immediately:
/// front matter is applied, title and text are checked.
pub fn prepare(mut req: IngestRequest) -> Result<IngestRequest> {
    let fm = if req.plain { Default::default() } else { front_matter(&req.text) };
    if fm.title.is_some() || !fm.tags.is_empty() || fm.status.is_some() {
        req.text = fm.body;
        if req.title.trim().is_empty() {
            req.title = fm.title.unwrap_or_default();
        }
        if req.tags.as_ref().is_none_or(Vec::is_empty) && !fm.tags.is_empty() {
            req.tags = Some(fm.tags);
        }
        if req.status.is_none() {
            req.status = fm.status.as_deref().map(Status::parse).transpose()?;
        }
    }
    if req.text.trim().is_empty() {
        return Err(Invalid("text is required".into()).into());
    }
    if req.title.trim().is_empty() {
        let heading = if req.plain { None } else { first_heading(&req.text) };
        req.title = heading.ok_or_else(|| Invalid("title is required".into()))?;
    }
    if let Some(id) = &req.id {
        if id.trim().is_empty() {
            req.id = None;
        }
    }
    Ok(req)
}

fn summary(r: &IngestReport) -> (String, usize) {
    (format!("{}, {}", count(r.chunks, "passage", "passages"), count(r.entities, "entité", "entités")), 0)
}

/// Validates and queues an ingestion; the work runs on the ingestion worker.
pub fn submit(
    state: &Arc<AppState>,
    project: &str,
    req: IngestRequest,
    mode: Mode,
    channel: &'static str,
    label: &str,
) -> Result<Job> {
    let graph = state.projects.get(project)?;
    let mut req = prepare(req)?;
    if mode == Mode::Create {
        if let Some(id) = &req.id {
            let conn = graph.reader()?;
            if !rows(&conn, "MATCH (d:Document {id: $id}) RETURN d.id", vec![("id", s(id))])?.is_empty() {
                return Err(Conflict(format!("document `{id}` already exists; use PUT to replace it")).into());
            }
        }
    }
    // The id is fixed now, so clients can follow the document before the job ends.
    let id = req.id.get_or_insert_with(|| uuid::Uuid::new_v4().to_string()).clone();
    let label = if label.is_empty() { req.title.clone() } else { label.to_string() };
    let st = state.clone();
    let project_id = project.to_string();
    let detail = label.clone();
    Ok(state.jobs.submit(project, &label, &id, move |progress| {
        let operation = if mode == Mode::Create { "ingest" } else { "replace" };
        let call = Call::start(channel, &project_id, operation, detail);
        let outcome = ingest(&st, &graph, req, mode, Some(progress));
        call.finish(&st.history, &outcome, summary);
        outcome
    }))
}

/// Waits for a job to finish (polling), at most `timeout` when given.
pub async fn wait(state: &Arc<AppState>, id: &str, timeout: Option<std::time::Duration>) -> Option<Job> {
    let started = Instant::now();
    loop {
        let job = state.jobs.get(id)?;
        if job.finished_at.is_some() || timeout.is_some_and(|t| started.elapsed() >= t) {
            return Some(job);
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Status {
    Draft,
    Published,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Draft => "DRAFT",
            Status::Published => "PUBLISHED",
        }
    }

    pub fn parse(raw: &str) -> Result<Self> {
        match raw.trim().to_uppercase().as_str() {
            "DRAFT" => Ok(Status::Draft),
            "PUBLISHED" => Ok(Status::Published),
            other => Err(Invalid(format!("unknown status `{other}` (expected DRAFT or PUBLISHED)")).into()),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct IngestRequest {
    /// Stable id; generated when absent.
    pub id: Option<String>,
    /// Defaults to the markdown front matter `title`, then the first `# heading`.
    #[serde(default)]
    pub title: String,
    pub text: String,
    pub source: Option<String>,
    pub metadata: Option<serde_json::Value>,
    pub tags: Option<Vec<String>>,
    pub status: Option<Status>,
    /// Defaults to the configured user (single-user mode).
    pub creator: Option<String>,
    /// Overrides the configured NER labels for this document.
    pub labels: Option<Vec<String>>,
    /// Plain text (PDF, legacy Office, .txt): `#` lines are not headings and there is no
    /// front matter. JSON requests are markdown.
    #[serde(skip)]
    pub plain: bool,
    /// The uploaded file, kept in the project's `files/` folder for reading as is.
    #[serde(skip)]
    pub original: Option<Original>,
}

pub struct Original {
    pub filename: String,
    pub bytes: Vec<u8>,
}

impl std::fmt::Debug for Original {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Original({}, {} bytes)", self.filename, self.bytes.len())
    }
}

/// `files/<hash>.<ext>`: document ids may contain `/` or spaces, file names must not.
fn stored_name(doc_id: &str, filename: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in doc_id.bytes() {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    let ext = std::path::Path::new(filename)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| format!(".{}", e.to_lowercase()))
        .unwrap_or_default();
    format!("files/{hash:016x}{ext}")
}

/// The stored original of a document, from its metadata.
pub fn stored_file(metadata: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(metadata).ok()?;
    value.get("file")?.get("stored")?.as_str().map(str::to_string)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Fails if the id already exists.
    Create,
    /// Replaces the document if it exists (keeping its creator and creation date), creates it otherwise.
    Upsert,
}

#[derive(Debug, Clone, Serialize)]
pub struct IngestReport {
    pub id: String,
    pub title: String,
    pub status: Status,
    pub tags: Vec<String>,
    pub replaced: bool,
    pub chunks: usize,
    pub entities: usize,
    pub new_entities: usize,
    pub relations: usize,
    pub millis: u128,
}

pub fn normalize_tags(tags: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    tags.iter()
        .map(|t| t.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|t| !t.is_empty() && seen.insert(t.to_lowercase()))
        .collect()
}

fn default_status(state: &AppState) -> Status {
    Status::parse(&state.config.default_status).unwrap_or(Status::Published)
}

const EMBED_STEP: usize = 64;
const NER_STEP: usize = 16;

pub fn ingest(
    state: &AppState,
    graph: &Graph,
    req: IngestRequest,
    mode: Mode,
    progress: Option<&Progress>,
) -> Result<IngestReport> {
    let started = Instant::now();
    // Reports progress and stops here when the job was cancelled (nothing is written yet).
    let report_progress = |stage, done, total| -> Result<()> {
        if let Some(p) = progress {
            if p.is_cancelled() {
                return Err(crate::jobs::Cancelled.into());
            }
            p.set(stage, done, total);
        }
        Ok(())
    };
    let req = prepare(req)?;
    let title = req.title.trim().to_string();
    let doc_id = match req.id.as_deref().map(str::trim) {
        Some(id) if !id.is_empty() => id.to_string(),
        _ => uuid::Uuid::new_v4().to_string(),
    };
    let status = req.status.unwrap_or_else(|| default_status(state));
    let tags = normalize_tags(req.tags.as_deref().unwrap_or_default());
    let labels = req.labels.filter(|l| !l.is_empty()).unwrap_or_else(|| state.config.ner_labels.clone());

    // CPU-heavy work happens before taking the writer lock.
    let (size, overlap) = (state.config.chunk_size, state.config.chunk_overlap);
    let pieces = if req.plain { chunk_plain(&req.text, size, overlap) } else { chunk_document(&req.text, size, overlap) };
    let chunks: Vec<String> = pieces.iter().map(|c| c.text.clone()).collect();
    let total = chunks.len();
    let mut embeddings = Vec::with_capacity(total);
    for batch in chunks.chunks(EMBED_STEP) {
        report_progress(Stage::Embedding, embeddings.len(), total)?;
        embeddings.extend(state.embedder.embed_passages(batch)?);
    }
    let mut mentions = Vec::with_capacity(total);
    for batch in chunks.chunks(NER_STEP) {
        report_progress(Stage::Entities, mentions.len(), total)?;
        mentions.extend(state.ner.extract(batch, &labels)?);
    }
    report_progress(Stage::Writing, 0, 1)?;

    // Distinct entities of the document (first surface form wins) and co-occurrence pairs.
    let mut entities: BTreeMap<String, &Mention> = BTreeMap::new();
    let mut pairs: HashMap<(String, String), i64> = HashMap::new();
    for chunk_mentions in &mentions {
        for m in chunk_mentions {
            entities.entry(m.entity_id.clone()).or_insert(m);
        }
        let mut ids: Vec<&str> = chunk_mentions.iter().map(|m| m.entity_id.as_str()).collect();
        ids.sort_unstable();
        for (n, a) in ids.iter().enumerate() {
            for b in &ids[n + 1..] {
                *pairs.entry(((*a).to_string(), (*b).to_string())).or_default() += 1;
            }
        }
    }

    let chunk_id = |idx: usize| format!("{doc_id}#{idx}");
    let stored = req
        .original
        .as_ref()
        .filter(|_| state.config.keep_originals)
        .map(|o| stored_name(&doc_id, &o.filename));
    let conn = graph.writer()?;
    let report = db::transaction(&conn, |conn| {
        // Creator and creation date survive a replacement.
        let previous = rows(
            conn,
            "MATCH (d:Document {id: $id}) RETURN d.creator, CAST(d.created_at AS STRING), d.metadata",
            vec![("id", s(&doc_id))],
        )?;
        let (creator, created_at) = match previous.first() {
            Some(_) if mode == Mode::Create => {
                return Err(Conflict(format!("document `{doc_id}` already exists; use PUT to replace it")).into());
            }
            Some(r) => (as_str(&r[0]), as_str(&r[1])),
            None => (String::new(), String::new()),
        };
        let previous_file = previous.first().and_then(|r| stored_file(&as_str(&r[2])));
        let creator = req
            .creator
            .clone()
            .filter(|c| !c.trim().is_empty())
            .or_else(|| (!creator.is_empty()).then_some(creator))
            .unwrap_or_else(|| state.config.user.clone());
        let replaced = delete_document_tx(conn, &doc_id)?;

        let existing: HashSet<String> = rows(
            conn,
            "UNWIND $ids AS id MATCH (e:Entity {id: id}) RETURN e.id",
            vec![("ids", db::strings(entities.keys().cloned()))],
        )?
        .iter()
        .map(|r| as_str(&r[0]))
        .collect();
        let new_entities: Vec<&Mention> =
            entities.values().filter(|m| !existing.contains(&m.entity_id)).copied().collect();
        let names: Vec<String> = new_entities.iter().map(|m| m.name.clone()).collect();
        let entity_embeddings = state.embedder.embed_queries(&names)?;

        let mut metadata = req.metadata.clone().unwrap_or_else(|| serde_json::json!({}));
        if let (Some(file), true) = (stored.as_ref(), metadata.is_object()) {
            if metadata.get("file").is_none_or(|f| !f.is_object()) {
                metadata["file"] = serde_json::json!({});
            }
            metadata["file"]["stored"] = serde_json::json!(file);
        }
        let metadata = if metadata.as_object().is_some_and(|m| m.is_empty()) { String::new() } else { metadata.to_string() };
        exec(
            conn,
            "CREATE (:Document {id: $id, title: $title, source: $source, metadata: $metadata,
                     status: $status, creator: $creator, tags: $tags, content: $content,
                     created_at: CASE WHEN $created = '' THEN current_timestamp() ELSE timestamp($created) END,
                     updated_at: current_timestamp()})",
            vec![
                ("id", s(&doc_id)),
                ("title", s(&title)),
                ("source", s(req.source.clone().unwrap_or_default())),
                ("metadata", s(metadata)),
                ("status", s(status.as_str())),
                ("creator", s(creator)),
                ("tags", db::strings(tags.clone())),
                ("content", s(&req.text)),
                ("created", s(created_at)),
            ],
        )?;

        let dim = graph.dim();
        let mut chunk_rows = Structs::new(&[
            ("id", LogicalType::String),
            ("idx", LogicalType::Int64),
            ("text", LogicalType::String),
            ("emb", db::float_list_type()),
            ("page", LogicalType::Int64),
        ]);
        for (idx, ((text, emb), piece)) in chunks.iter().zip(&embeddings).zip(&pieces).enumerate() {
            chunk_rows.push(vec![
                s(chunk_id(idx)),
                db::i(idx as i64),
                s(text),
                db::floats(emb),
                db::i(piece.page.unwrap_or(0)),
            ]);
        }
        exec(
            conn,
            &format!(
                "MATCH (d:Document {{id: $doc}})
                 UNWIND $rows AS r
                 CREATE (d)-[:HAS_CHUNK]->(:Chunk {{id: r.id, doc_id: $doc, idx: r.idx, text: r.text, embedding: CAST(r.emb AS FLOAT[{dim}]), page: r.page}})"
            ),
            vec![("doc", s(&doc_id)), ("rows", chunk_rows.into_value())],
        )?;
        if chunks.len() > 1 {
            exec(
                conn,
                "UNWIND range(0, $last - 1) AS n
                 MATCH (a:Chunk {id: $doc + '#' + CAST(n AS STRING)}), (b:Chunk {id: $doc + '#' + CAST(n + 1 AS STRING)})
                 CREATE (a)-[:NEXT]->(b)",
                vec![("doc", s(&doc_id)), ("last", db::i(chunks.len() as i64 - 1))],
            )?;
        }

        let mut entity_rows = Structs::new(&[
            ("id", LogicalType::String),
            ("name", LogicalType::String),
            ("label", LogicalType::String),
            ("emb", db::float_list_type()),
        ]);
        for (m, emb) in new_entities.iter().zip(&entity_embeddings) {
            entity_rows.push(vec![s(&m.entity_id), s(&m.name), s(&m.label), db::floats(emb)]);
        }
        if !entity_rows.is_empty() {
            exec(
                conn,
                &format!(
                    "UNWIND $rows AS r
                     CREATE (:Entity {{id: r.id, name: r.name, label: r.label, embedding: CAST(r.emb AS FLOAT[{dim}])}})"
                ),
                vec![("rows", entity_rows.into_value())],
            )?;
        }

        let mut mention_rows = Structs::new(&[
            ("chunk", LogicalType::String),
            ("entity", LogicalType::String),
            ("score", LogicalType::Double),
            ("surface", LogicalType::String),
        ]);
        for (idx, chunk_mentions) in mentions.iter().enumerate() {
            for m in chunk_mentions {
                mention_rows.push(vec![
                    s(chunk_id(idx)),
                    s(&m.entity_id),
                    db::f(f64::from(m.score)),
                    s(&m.name),
                ]);
            }
        }
        if !mention_rows.is_empty() {
            exec(
                conn,
                "UNWIND $rows AS r
                 MATCH (c:Chunk {id: r.chunk}), (e:Entity {id: r.entity})
                 CREATE (c)-[:MENTIONS {score: r.score, surface: r.surface}]->(e)",
                vec![("rows", mention_rows.into_value())],
            )?;
        }

        let mut pair_rows = Structs::new(&[
            ("a", LogicalType::String),
            ("b", LogicalType::String),
            ("w", LogicalType::Int64),
        ]);
        for ((a, b), w) in &pairs {
            pair_rows.push(vec![s(a), s(b), db::i(*w)]);
        }
        if !pair_rows.is_empty() {
            exec(
                conn,
                "UNWIND $rows AS r
                 MATCH (a:Entity {id: r.a}), (b:Entity {id: r.b})
                 MERGE (a)-[x:RELATED]->(b)
                 ON CREATE SET x.weight = r.w
                 ON MATCH SET x.weight = x.weight + r.w",
                vec![("rows", pair_rows.into_value())],
            )?;
        }

        Ok(IngestReport {
            id: doc_id.clone(),
            title: title.clone(),
            status,
            tags: tags.clone(),
            replaced,
            chunks: chunks.len(),
            entities: entities.len(),
            new_entities: new_entities.len(),
            relations: pairs.len(),
            millis: 0,
        })
        .map(|report| (report, previous_file))
    })
    .map(|(report, previous_file)| {
        // Originals: write the new one, drop the one it replaces.
        if let (Some(rel), Some(original)) = (stored.as_ref(), req.original.as_ref()) {
            let path = graph.files_dir().join(rel.trim_start_matches("files/"));
            if let Err(e) = std::fs::create_dir_all(graph.files_dir()).and_then(|()| std::fs::write(&path, &original.bytes)) {
                tracing::warn!("cannot keep the original of {doc_id}: {e}");
            }
        }
        if let Some(old) = previous_file.filter(|old| Some(old) != stored.as_ref()) {
            let _ = std::fs::remove_file(graph.files_dir().join(old.trim_start_matches("files/")));
        }
        report
    })?;
    drop(conn);
    graph.checkpoint()?;

    tracing::info!(
        doc = %report.id, chunks = report.chunks, entities = report.entities,
        elapsed_ms = started.elapsed().as_millis(), "document ingested"
    );
    Ok(IngestReport { millis: started.elapsed().as_millis(), ..report })
}

/// Metadata changes that do not require re-indexing the text.
#[derive(Debug, Default, Deserialize)]
pub struct DocumentPatch {
    pub title: Option<String>,
    pub status: Option<Status>,
    pub tags: Option<Vec<String>>,
    pub source: Option<String>,
    pub metadata: Option<serde_json::Value>,
}

pub fn update_document(graph: &Graph, doc_id: &str, patch: DocumentPatch) -> Result<()> {
    if patch.title.as_deref().is_some_and(|t| t.trim().is_empty()) {
        return Err(Invalid("title cannot be empty".into()).into());
    }
    let conn = graph.writer()?;
    let found = rows(
        &conn,
        "MATCH (d:Document {id: $id})
         SET d.title = coalesce($title, d.title),
             d.status = coalesce($status, d.status),
             d.tags = CASE WHEN $set_tags THEN $tags ELSE d.tags END,
             d.source = coalesce($source, d.source),
             d.metadata = coalesce($metadata, d.metadata),
             d.updated_at = current_timestamp()
         RETURN d.id",
        vec![
            ("id", s(doc_id)),
            ("title", opt(patch.title.map(|t| t.trim().to_string()))),
            ("status", opt(patch.status.map(|st| st.as_str().to_string()))),
            ("set_tags", lbug::Value::Bool(patch.tags.is_some())),
            ("tags", db::strings(normalize_tags(patch.tags.as_deref().unwrap_or_default()))),
            ("source", opt(patch.source)),
            ("metadata", opt(patch.metadata.map(|m| m.to_string()))),
        ],
    )?;
    drop(conn);
    if found.is_empty() {
        return Err(NotFound(format!("document `{doc_id}` not found")).into());
    }
    graph.checkpoint()
}

fn opt(v: Option<String>) -> lbug::Value {
    v.map_or(lbug::Value::Null(LogicalType::String), s)
}

pub fn delete_document(graph: &Graph, doc_id: &str) -> Result<()> {
    let conn = graph.writer()?;
    let file = rows(&conn, "MATCH (d:Document {id: $id}) RETURN d.metadata", vec![("id", s(doc_id))])?
        .first()
        .and_then(|r| stored_file(&as_str(&r[0])));
    let deleted = db::transaction(&conn, |conn| delete_document_tx(conn, doc_id))?;
    drop(conn);
    if !deleted {
        return Err(NotFound(format!("document `{doc_id}` not found")).into());
    }
    if let Some(rel) = file {
        let _ = std::fs::remove_file(graph.files_dir().join(rel.trim_start_matches("files/")));
    }
    graph.checkpoint()
}

/// Removes a document, its chunks, the co-occurrence weight it contributed and orphan entities.
fn delete_document_tx(conn: &Connection, doc_id: &str) -> Result<bool> {
    let exists = !rows(conn, "MATCH (d:Document {id: $id}) RETURN d.id", vec![("id", s(doc_id))])?.is_empty();
    if !exists {
        return Ok(false);
    }
    exec(
        conn,
        "MATCH (:Document {id: $id})-[:HAS_CHUNK]->(c:Chunk)-[:MENTIONS]->(a:Entity), (c)-[:MENTIONS]->(b:Entity)
         WHERE a.id < b.id
         WITH a, b, count(c) AS n
         MATCH (a)-[x:RELATED]->(b)
         SET x.weight = x.weight - n",
        vec![("id", s(doc_id))],
    )?;
    exec(conn, "MATCH ()-[x:RELATED]->() WHERE x.weight <= 0 DELETE x", vec![])?;
    exec(
        conn,
        "MATCH (:Document {id: $id})-[:HAS_CHUNK]->(c:Chunk) DETACH DELETE c",
        vec![("id", s(doc_id))],
    )?;
    exec(conn, "MATCH (d:Document {id: $id}) DETACH DELETE d", vec![("id", s(doc_id))])?;
    exec(
        conn,
        "MATCH (e:Entity) WHERE NOT EXISTS { MATCH (e)<-[:MENTIONS]-(:Chunk) } DETACH DELETE e",
        vec![],
    )?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_are_trimmed_and_deduplicated() {
        let tags = normalize_tags(&["  RH ".into(), "rh".into(), "".into(), "paie  mensuelle".into()]);
        assert_eq!(tags, vec!["RH", "paie mensuelle"]);
    }

    #[test]
    fn status_parsing() {
        assert_eq!(Status::parse("draft").unwrap(), Status::Draft);
        assert!(Status::parse("archived").is_err());
    }
}
