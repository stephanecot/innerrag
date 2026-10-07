//! Document lifecycle: add, replace (chunk → embed → NER → graph write), edit
//! metadata, delete.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use lbug::{Connection, LogicalType};
use serde::{Deserialize, Serialize};

use crate::chunk::{chunk_document, chunk_plain, prose};
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
    // Kept on disk until done, so a restart resumes the queue instead of losing it.
    let pending = persist(state, project, mode, channel, &label, &req)?;
    let st = state.clone();
    let project_id = project.to_string();
    let detail = label.clone();
    Ok(state.jobs.submit(project, &label, &id, move |progress| {
        let operation = if mode == Mode::Create { "ingest" } else { "replace" };
        let call = Call::start(channel, &project_id, operation, detail);
        let outcome = ingest(&st, &graph, req, mode, Some(progress));
        call.finish(&st.history, &outcome, summary);
        let _ = std::fs::remove_dir_all(&pending);
        outcome
    }))
}

#[derive(Serialize, Deserialize)]
struct PendingJob {
    project: String,
    mode: Mode,
    channel: String,
    label: String,
    original_name: Option<String>,
    request: IngestRequest,
}

fn pending_root(state: &AppState) -> std::path::PathBuf {
    state.config.data_dir.join("jobs")
}

fn persist(state: &AppState, project: &str, mode: Mode, channel: &str, label: &str, req: &IngestRequest) -> Result<std::path::PathBuf> {
    let dir = pending_root(state).join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&dir)?;
    if let Some(original) = &req.original {
        std::fs::write(dir.join("original.bin"), &original.bytes)?;
    }
    let job = serde_json::json!({
        "project": project,
        "mode": mode,
        "channel": channel,
        "label": label,
        "original_name": req.original.as_ref().map(|o| o.filename.clone()),
        "request": req,
    });
    std::fs::write(dir.join("job.json"), serde_json::to_vec(&job)?)?;
    Ok(dir)
}

/// Queues again the ingestions a restart interrupted. Returns how many were resumed.
pub fn resume_pending(state: &Arc<AppState>) -> usize {
    let Ok(entries) = std::fs::read_dir(pending_root(state)) else { return 0 };
    let mut resumed = 0;
    for entry in entries.flatten() {
        let dir = entry.path();
        let job: Option<PendingJob> = std::fs::read(dir.join("job.json")).ok().and_then(|b| serde_json::from_slice(&b).ok());
        let original = std::fs::read(dir.join("original.bin")).ok();
        let _ = std::fs::remove_dir_all(&dir);
        let Some(mut job) = job else { continue };
        if let (Some(filename), Some(bytes)) = (job.original_name.clone(), original) {
            job.request.original = Some(Original { filename, bytes });
        }
        let channel = match job.channel.as_str() {
            "mcp" => "mcp",
            "ui" => "ui",
            "watch" => "watch",
            _ => "rest",
        };
        // A resumed creation may find its own document half-written by nobody: upsert is safe.
        let mode = if job.mode == Mode::Create { Mode::Upsert } else { job.mode };
        match submit(state, &job.project, job.request, mode, channel, &job.label) {
            Ok(_) => resumed += 1,
            Err(e) => tracing::warn!(project = %job.project, "cannot resume ingestion of {}: {e:#}", job.label),
        }
    }
    resumed
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

#[derive(Debug, Serialize, Deserialize)]
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
    #[serde(default)]
    pub plain: bool,
    /// The uploaded file, kept in the project's `files/` folder for reading as is.
    #[serde(skip)]
    pub original: Option<Original>,
}

/// What a client may set alongside an uploaded file (each overrides the file's own value).
#[derive(Debug, Default)]
pub struct FileFields {
    pub id: Option<String>,
    pub title: Option<String>,
    pub source: Option<String>,
    pub tags: Option<Vec<String>>,
    pub status: Option<String>,
    pub creator: Option<String>,
    pub metadata: Option<serde_json::Value>,
}

/// Extracts a file (PDF, Word, PowerPoint, Markdown…) into an ingestion request that keeps the
/// original. Shared by uploads, the MCP `ingest_file` tool and watched folders.
pub fn file_request(filename: &str, bytes: Vec<u8>, fields: FileFields) -> Result<IngestRequest> {
    let (format, extracted) = crate::extract::extract(filename, &bytes)?;
    let status = match fields.status.or(extracted.status) {
        Some(s) => Some(Status::parse(&s)?),
        None => None,
    };
    let stem = std::path::Path::new(filename).file_stem().and_then(|s| s.to_str()).unwrap_or("document").to_string();
    let mut metadata = fields.metadata.filter(serde_json::Value::is_object).unwrap_or_else(|| serde_json::json!({}));
    metadata["file"] = serde_json::json!({
        "name": filename,
        "format": format,
        "format_label": format.label(),
        "size": bytes.len(),
        "pages": extracted.pages,
    });
    Ok(IngestRequest {
        id: fields.id,
        title: fields.title.or(extracted.title).unwrap_or(stem),
        text: extracted.text,
        source: fields.source.or_else(|| Some(filename.to_string())),
        metadata: Some(metadata),
        tags: fields.tags.or_else(|| (!extracted.tags.is_empty()).then_some(extracted.tags)),
        status,
        creator: fields.creator,
        labels: None,
        plain: !extracted.structured,
        original: Some(Original { filename: filename.to_string(), bytes }),
    })
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
    /// Passages unchanged since the previous version: their vectors and entities were reused.
    pub reused: usize,
    pub millis: u128,
}

/// The current passages of a document: text → (vector, entities), to skip unchanged ones.
fn previous_chunks(graph: &Graph, doc_id: &str) -> Result<HashMap<String, (Vec<f32>, Vec<Mention>)>> {
    let conn = graph.reader()?;
    let mut out: HashMap<String, (Vec<f32>, Vec<Mention>)> = HashMap::new();
    let mut by_id: HashMap<String, String> = HashMap::new();
    for r in rows(
        &conn,
        "MATCH (:Document {id: $id})-[:HAS_CHUNK]->(c:Chunk) RETURN c.id, c.text, c.embedding",
        vec![("id", s(doc_id))],
    )? {
        let text = as_str(&r[1]);
        by_id.insert(as_str(&r[0]), text.clone());
        out.insert(text, (db::as_floats(&r[2]), Vec::new()));
    }
    if out.is_empty() {
        return Ok(out);
    }
    for r in rows(
        &conn,
        "MATCH (:Document {id: $id})-[:HAS_CHUNK]->(c:Chunk)-[m:MENTIONS]->(e:Entity)
         RETURN c.id, e.id, e.name, e.label, m.score",
        vec![("id", s(doc_id))],
    )? {
        if let Some(entry) = by_id.get(&as_str(&r[0])).and_then(|text| out.get_mut(text)) {
            entry.1.push(Mention {
                entity_id: as_str(&r[1]),
                name: as_str(&r[2]),
                label: as_str(&r[3]),
                score: db::as_f64(&r[4]) as f32,
            });
        }
    }
    Ok(out)
}

/// Entities already in the base, by lowercase name: (id, label) of the most mentioned one.
fn existing_entities(graph: &Graph, mut names: Vec<String>) -> Result<HashMap<String, (String, String)>> {
    names.sort_unstable();
    names.dedup();
    let conn = graph.reader()?;
    let mut out = HashMap::new();
    for r in rows(
        &conn,
        "MATCH (e:Entity) WHERE lower(e.name) IN $names
         OPTIONAL MATCH (e)<-[m:MENTIONS]-(:Chunk)
         RETURN lower(e.name), e.id, e.label, count(m) AS n ORDER BY n DESC",
        vec![("names", db::strings(names))],
    )? {
        out.entry(as_str(&r[0])).or_insert((as_str(&r[1]), as_str(&r[2])));
    }
    Ok(out)
}

/// One entity per name. GLiNER may tag "Android" as product here and technology there: the
/// entity already in the base wins, else the label the document gives it most often.
fn canonicalize(mentions: &mut [Vec<Mention>], existing: &HashMap<String, (String, String)>) {
    let mut votes: HashMap<String, HashMap<String, (usize, f32)>> = HashMap::new();
    for m in mentions.iter().flatten() {
        let v = votes.entry(m.name.to_lowercase()).or_default().entry(m.label.clone()).or_default();
        v.0 += 1;
        v.1 += m.score;
    }
    let chosen: HashMap<String, String> = votes
        .into_iter()
        .filter_map(|(name, labels)| {
            let best = labels.into_iter().max_by(|a, b| a.1 .0.cmp(&b.1 .0).then(a.1 .1.total_cmp(&b.1 .1)))?;
            Some((name, best.0))
        })
        .collect();
    for chunk in mentions.iter_mut() {
        let mut out: Vec<Mention> = Vec::with_capacity(chunk.len());
        for mut m in chunk.drain(..) {
            let lower = m.name.to_lowercase();
            if let Some((id, label)) = existing.get(&lower) {
                m.entity_id.clone_from(id);
                m.label.clone_from(label);
            } else if let Some(label) = chosen.get(&lower) {
                m.label.clone_from(label);
                m.entity_id = crate::ner::entity_id(label, &m.name);
            }
            match out.iter_mut().find(|o| o.entity_id == m.entity_id) {
                Some(o) => o.score = o.score.max(m.score),
                None => out.push(m),
            }
        }
        *chunk = out;
    }
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
    let custom_labels = req.labels.as_ref().is_some_and(|l| !l.is_empty());
    let labels = req.labels.clone().filter(|l| !l.is_empty()).unwrap_or_else(|| state.config.ner_labels.clone());

    // CPU-heavy work happens before taking the writer lock.
    let (size, overlap) = (state.config.chunk_size, state.config.chunk_overlap);
    let pieces = if req.plain { chunk_plain(&req.text, size, overlap) } else { chunk_document(&req.text, size, overlap) };
    let chunks: Vec<String> = pieces.iter().map(|c| c.text.clone()).collect();
    let total = chunks.len();
    // Re-import: passages whose text did not change keep their vectors and entities.

    let previous = if custom_labels { HashMap::new() } else { previous_chunks(graph, &doc_id)? };
    let todo: Vec<usize> = (0..total).filter(|&i| !previous.contains_key(&chunks[i])).collect();
    let reused = total - todo.len();
    let todo_texts: Vec<String> = todo.iter().map(|&i| chunks[i].clone()).collect();
    let mut new_embeddings = Vec::with_capacity(todo.len());
    for batch in todo_texts.chunks(EMBED_STEP) {
        report_progress(Stage::Embedding, reused + new_embeddings.len(), total)?;
        new_embeddings.extend(state.embedder.embed_passages(batch)?);
    }
    let mut new_mentions = Vec::with_capacity(todo.len());
    for batch in todo_texts.chunks(NER_STEP) {
        report_progress(Stage::Entities, reused + new_mentions.len(), total)?;
        let prose_batch: Vec<String> = batch.iter().map(|t| prose(t)).collect();
        new_mentions.extend(state.ner.extract(&prose_batch, &labels)?);
    }
    let (mut embeddings, mut mentions) = (Vec::with_capacity(total), Vec::with_capacity(total));
    let (mut new_embeddings, mut new_mentions) = (new_embeddings.into_iter(), new_mentions.into_iter());
    for text in &chunks {
        match previous.get(text) {
            Some((emb, found)) => {
                embeddings.push(emb.clone());
                // Entities kept from an earlier import go through today's noise filter too.
                mentions.push(found.iter().filter(|m| !state.ner.is_noise(&m.name)).cloned().collect());
            }
            None => {
                embeddings.push(new_embeddings.next().unwrap_or_default());
                mentions.push(new_mentions.next().unwrap_or_default());
            }
        }
    }
    report_progress(Stage::Writing, 0, 1)?;
    let names: Vec<String> = mentions.iter().flatten().map(|m| m.name.to_lowercase()).collect();
    let known = existing_entities(graph, names)?;
    canonicalize(&mut mentions, &known);
    // Vectors of the entities that look new are computed now, outside the write transaction.
    let mut unseen: Vec<(String, String)> = mentions
        .iter()
        .flatten()
        .filter(|m| !known.contains_key(&m.name.to_lowercase()))
        .map(|m| (m.entity_id.clone(), m.name.clone()))
        .collect();
    unseen.sort();
    unseen.dedup_by(|a, b| a.0 == b.0);
    let unseen_vectors = state.embedder.embed_queries(&unseen.iter().map(|(_, n)| n.clone()).collect::<Vec<_>>())?;
    let mut entity_vectors: HashMap<String, Vec<f32>> =
        unseen.into_iter().map(|(id, _)| id).zip(unseen_vectors).collect();

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
        // Normally all computed before the transaction; a concurrent ingestion may have added some.
        let missing: Vec<String> =
            new_entities.iter().filter(|m| !entity_vectors.contains_key(&m.entity_id)).map(|m| m.name.clone()).collect();
        if !missing.is_empty() {
            let ids: Vec<String> = new_entities.iter().filter(|m| !entity_vectors.contains_key(&m.entity_id)).map(|m| m.entity_id.clone()).collect();
            entity_vectors.extend(ids.into_iter().zip(state.embedder.embed_queries(&missing)?));
        }
        let entity_embeddings: Vec<Vec<f32>> =
            new_entities.iter().map(|m| entity_vectors.get(&m.entity_id).cloned().unwrap_or_default()).collect();

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
            reused,
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
    graph.index_keywords(&doc_id, &chunks.iter().enumerate().map(|(i, text)| (chunk_id(i), text.clone())).collect::<Vec<_>>());
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
    graph.index_keywords(doc_id, &[]);
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
    fn labels_are_unified_per_name() {
        let m = |name: &str, label: &str, score: f32| Mention {
            entity_id: crate::ner::entity_id(label, name),
            name: name.into(),
            label: label.into(),
            score,
        };
        let mut mentions = vec![
            vec![m("Android", "product", 0.9), m("Java", "technology", 0.8)],
            vec![m("Android", "technology", 0.7), m("android", "product", 0.6)],
            vec![m("Google", "organization", 0.9)],
        ];
        let existing = HashMap::from([("google".to_string(), ("organization:google llc".to_string(), "organization".to_string()))]);
        canonicalize(&mut mentions, &existing);
        assert!(mentions.iter().flatten().filter(|x| x.name.eq_ignore_ascii_case("android")).all(|x| x.entity_id == "product:android"));
        assert_eq!(mentions[1].len(), 1, "same entity twice in one chunk is merged");
        assert_eq!(mentions[2][0].entity_id, "organization:google llc");
    }

    #[test]
    fn status_parsing() {
        assert_eq!(Status::parse("draft").unwrap(), Status::Draft);
        assert!(Status::parse("archived").is_err());
    }
}
