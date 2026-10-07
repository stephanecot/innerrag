//! Minimal MCP server over Streamable HTTP: JSON-RPC requests on `POST /mcp`
//! (default project, tools accept a `project` argument) and `POST /mcp/{project}`
//! (bound to one project), answered with plain JSON (no server-initiated stream).

use std::fmt::Write as _;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use crate::api::{blocking, Shared};
use crate::history::{count, Call};
use crate::ingest::{IngestRequest, Mode, Status};
use crate::jobs::{Job, Stage};
use crate::search::SearchRequest;
use crate::{explore, ingest, search, AppState, Invalid};
use base64::Engine as _;

const PROTOCOL_VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

const INSTRUCTIONS: &str = "innerrag is a local knowledge graph (Graph RAG) organised in isolated projects. \
Use `search_knowledge` to retrieve passages and related entities before answering questions about the \
ingested documents, and cite passages by their [n] number. Use `explore_entity` to follow relations \
between entities. Only PUBLISHED documents are searched unless include_drafts is set.";

/// Which project a request works on.
enum Scope {
    /// `/mcp`: the default project, overridable per call with a `project` argument.
    Default,
    /// `/mcp/{project}`: fixed.
    Bound(String),
}

pub async fn reject_get() -> impl IntoResponse {
    (StatusCode::METHOD_NOT_ALLOWED, "this MCP server does not offer a server-sent event stream")
}

pub async fn handle_default(State(state): State<Shared>, body: Bytes) -> Response {
    handle(state, Scope::Default, body).await
}

pub async fn handle_project(State(state): State<Shared>, Path(project): Path<String>, body: Bytes) -> Response {
    handle(state, Scope::Bound(project), body).await
}

async fn handle(state: Shared, scope: Scope, body: Bytes) -> Response {
    let Ok(message) = serde_json::from_slice::<Value>(&body) else {
        return Json(error(Value::Null, -32700, "parse error")).into_response();
    };
    let messages = match message {
        Value::Array(batch) => batch,
        m => vec![m],
    };
    let is_batch = messages.len() > 1;
    let mut responses = Vec::new();
    for m in messages {
        if let Some(r) = dispatch(&state, &scope, m).await {
            responses.push(r);
        }
    }
    match (responses.len(), is_batch) {
        (0, _) => StatusCode::ACCEPTED.into_response(),
        (_, true) => Json(Value::Array(responses)).into_response(),
        _ => Json(responses.remove(0)).into_response(),
    }
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn ok(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

/// Returns `None` for notifications and responses, which get no reply.
async fn dispatch(state: &Shared, scope: &Scope, message: Value) -> Option<Value> {
    let id = message.get("id").cloned()?;
    let method = message.get("method").and_then(Value::as_str)?.to_string();
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    Some(match method.as_str() {
        "initialize" => {
            let requested = params.get("protocolVersion").and_then(Value::as_str).unwrap_or_default();
            let version = PROTOCOL_VERSIONS.iter().find(|v| **v == requested).unwrap_or(&PROTOCOL_VERSIONS[0]);
            let instructions = match scope {
                Scope::Bound(p) => format!("{INSTRUCTIONS} This endpoint is bound to the project `{p}`."),
                Scope::Default => format!(
                    "{INSTRUCTIONS} Calls use the project `{}` unless a `project` argument is given; \
                     `list_projects` shows the others.",
                    state.config.default_project
                ),
            };
            ok(
                id,
                json!({
                    "protocolVersion": version,
                    "capabilities": { "tools": { "listChanged": false } },
                    "serverInfo": { "name": "innerrag", "version": env!("CARGO_PKG_VERSION") },
                    "instructions": instructions,
                }),
            )
        }
        "ping" => ok(id, json!({})),
        "tools/list" => ok(id, json!({ "tools": tools(matches!(scope, Scope::Default)) })),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or_default().to_string();
            let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
            let project = match scope {
                Scope::Bound(p) => p.clone(),
                Scope::Default => args
                    .get("project")
                    .and_then(Value::as_str)
                    .filter(|p| !p.is_empty())
                    .map_or_else(|| state.config.default_project.clone(), str::to_string),
            };
            match blocking(state, move |s| call_tool(s, &project, &name, &args)).await {
                Ok(text) => ok(id, json!({ "content": [{ "type": "text", "text": text }], "isError": false })),
                Err(e) => ok(id, json!({ "content": [{ "type": "text", "text": e.message() }], "isError": true })),
            }
        }
        _ => error(id, -32601, &format!("method not found: {method}")),
    })
}

fn tools(with_project_arg: bool) -> Value {
    let mut tools = json!([
        {
            "name": "search_knowledge",
            "description": "Graph RAG retrieval over the project's PUBLISHED documents: returns the most relevant passages (vector search fused with entity-graph expansion) and the entities related to the question.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Question or keywords, in any language." },
                    "k": { "type": "integer", "minimum": 1, "maximum": 50, "description": "Number of passages (default 8)." },
                    "min_score": { "type": "number", "minimum": 0, "maximum": 1, "description": "Minimum similarity of a passage (default: server setting, 0.80). Lower it to cast a wider net, 0 disables the filter." },
                    "tags": { "type": "array", "items": { "type": "string" }, "description": "Only documents carrying one of these tags." },
                    "include_drafts": { "type": "boolean", "description": "Also search DRAFT documents (default false)." }
                },
                "required": ["query"]
            },
            "annotations": { "readOnlyHint": true }
        },
        {
            "name": "explore_entity",
            "description": "Shows an entity of the knowledge graph: the entities it co-occurs with (strongest first) and the passages that mention it.",
            "inputSchema": {
                "type": "object",
                "properties": { "name": { "type": "string", "description": "Entity name or id (label:name)." } },
                "required": ["name"]
            },
            "annotations": { "readOnlyHint": true }
        },
        {
            "name": "explore_relation",
            "description": "Explains the link between two entities: how many passages cite both, how specific the link is (strength 0–1) and those passages.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "a": { "type": "string", "description": "First entity (name or label:name id)." },
                    "b": { "type": "string", "description": "Second entity." }
                },
                "required": ["a", "b"]
            },
            "annotations": { "readOnlyHint": true }
        },
        {
            "name": "ingest_document",
            "description": "Adds a text document to the project (chunking, embeddings, entity extraction). Re-using an existing id replaces that document.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "title": { "type": "string" },
                    "text": { "type": "string" },
                    "id": { "type": "string", "description": "Optional stable id; an existing id is replaced." },
                    "source": { "type": "string", "description": "Optional origin (URL, file path...)." },
                    "tags": { "type": "array", "items": { "type": "string" } },
                    "status": { "type": "string", "enum": ["DRAFT", "PUBLISHED"], "description": "Defaults to the server setting." },
                    "wait": { "type": "boolean", "description": "Wait (up to ~2 min) for the ingestion to finish. Default false: returns a job to follow with ingestion_status." }
                },
                "required": ["text"]
            }
        },
        {
            "name": "ingest_file",
            "description": "Adds a file to the project: PDF, Word (.docx, .doc), PowerPoint (.pptx, .ppt), Markdown or text, sent base64-encoded. Text is extracted on the server. Ingestion runs in the background: follow it with ingestion_status.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "filename": { "type": "string", "description": "File name with its extension (used to detect the format)." },
                    "content_base64": { "type": "string" },
                    "title": { "type": "string", "description": "Defaults to the document's own title, then the file name." },
                    "id": { "type": "string", "description": "Optional stable id; an existing id is replaced." },
                    "tags": { "type": "array", "items": { "type": "string" } },
                    "status": { "type": "string", "enum": ["DRAFT", "PUBLISHED"] },
                    "wait": { "type": "boolean", "description": "Wait (up to ~2 min) for the result." }
                },
                "required": ["filename", "content_base64"]
            }
        },
        {
            "name": "ingestion_status",
            "description": "Progress of background ingestions: one job by id, or the recent jobs of the project.",
            "inputSchema": {
                "type": "object",
                "properties": { "job_id": { "type": "string" } }
            },
            "annotations": { "readOnlyHint": true }
        },
        {
            "name": "list_documents",
            "description": "Lists the project's documents with their status, tags and creator.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "status": { "type": "string", "enum": ["DRAFT", "PUBLISHED"] },
                    "tag": { "type": "string" }
                }
            },
            "annotations": { "readOnlyHint": true }
        },
        {
            "name": "graph_stats",
            "description": "Counts of documents (published / drafts), chunks, entities and relations, and entities per label.",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": { "readOnlyHint": true }
        },
        {
            "name": "run_cypher",
            "description": "Runs a read-only Cypher query on the project's LadybugDB graph. Schema: (:Document {id,title,source,metadata,status,creator,tags,created_at,updated_at})-[:HAS_CHUNK]->(:Chunk {id,doc_id,idx,text,embedding})-[:MENTIONS {score,surface}]->(:Entity {id,name,label,embedding}); (:Chunk)-[:NEXT]->(:Chunk); (:Entity)-[:RELATED {weight}]->(:Entity). Avoid returning embedding columns.",
            "inputSchema": {
                "type": "object",
                "properties": { "query": { "type": "string" } },
                "required": ["query"]
            },
            "annotations": { "readOnlyHint": true }
        }
    ]);
    if with_project_arg {
        let list = tools.as_array_mut().expect("tools is an array");
        for tool in list.iter_mut() {
            tool["inputSchema"]["properties"]["project"] =
                json!({ "type": "string", "description": "Project id (default project when omitted)." });
        }
        list.push(json!({
            "name": "list_projects",
            "description": "Lists the projects of this server.",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": { "readOnlyHint": true }
        }));
    }
    tools
}

fn arg_str(args: &Value, key: &str) -> anyhow::Result<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| Invalid(format!("missing argument `{key}`")).into())
}

fn arg_strings(args: &Value, key: &str) -> Option<Vec<String>> {
    args.get(key)
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
}

const MCP_WAIT: std::time::Duration = std::time::Duration::from_secs(110);

fn describe_job(job: &Job) -> String {
    match job.stage {
        Stage::Done => match &job.report {
            Some(r) => format!(
                "Ingested `{}` as document `{}` ({}): {}, {} ({} new), {} relations, in {} ms.",
                job.filename, r.id, r.status.as_str(),
                count(r.chunks, "chunk", "chunks"), count(r.entities, "entity", "entities"),
                r.new_entities, r.relations, r.millis
            ),
            None => format!("Job `{}` finished.", job.id),
        },
        Stage::Failed => format!("Ingestion of `{}` failed: {}", job.filename, job.error.clone().unwrap_or_default()),
        Stage::Cancelled => format!("Ingestion of `{}` was cancelled.", job.filename),
        stage => format!(
            "Ingestion of `{}` (job `{}`, document `{}`) is {}: {}/{}. Call ingestion_status with this job_id to follow it.",
            job.filename, job.id, job.document_id,
            serde_json::to_value(stage).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default(),
            job.done, job.total
        ),
    }
}

/// Queues an ingestion and optionally waits for it (blocking: we run on a blocking thread).
fn queue(state: &Shared, project: &str, req: IngestRequest, label: &str, wait: bool) -> anyhow::Result<String> {
    let mode = if req.id.is_some() { Mode::Upsert } else { Mode::Create };
    let mut job = ingest::submit(state, project, req, mode, "mcp", label)?;
    if wait {
        let started = std::time::Instant::now();
        while job.finished_at.is_none() && started.elapsed() < MCP_WAIT {
            std::thread::sleep(std::time::Duration::from_millis(300));
            job = state.jobs.get(&job.id).unwrap_or(job);
        }
    }
    Ok(describe_job(&job))
}

fn call_tool(state: &Shared, project: &str, name: &str, args: &Value) -> anyhow::Result<String> {
    let wait = args.get("wait").and_then(Value::as_bool).unwrap_or(false);
    match name {
        "ingest_document" => {
            let status = args.get("status").and_then(Value::as_str).map(Status::parse).transpose()?;
            let req = IngestRequest {
                id: args.get("id").and_then(Value::as_str).map(str::to_string),
                title: args.get("title").and_then(Value::as_str).unwrap_or_default().to_string(),
                text: arg_str(args, "text")?,
                source: args.get("source").and_then(Value::as_str).map(str::to_string),
                metadata: None,
                tags: arg_strings(args, "tags"),
                status,
                creator: None,
                labels: None,
                plain: false,
                original: None,
            };
            return queue(state, project, req, "", wait);
        }
        "ingest_file" => {
            let filename = arg_str(args, "filename")?;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(arg_str(args, "content_base64")?.trim())
                .map_err(|e| Invalid(format!("content_base64 is not valid base64: {e}")))?;
            let req = ingest::file_request(
                &filename,
                bytes,
                ingest::FileFields {
                    id: args.get("id").and_then(Value::as_str).map(str::to_string),
                    title: args.get("title").and_then(Value::as_str).map(str::to_string),
                    tags: arg_strings(args, "tags"),
                    status: args.get("status").and_then(Value::as_str).map(str::to_string),
                    ..Default::default()
                },
            )?;
            return queue(state, project, req, &filename, wait);
        }
        "ingestion_status" => {
            if let Some(id) = args.get("job_id").and_then(Value::as_str) {
                return state
                    .jobs
                    .get(id)
                    .map(|j| describe_job(&j))
                    .ok_or_else(|| Invalid(format!("unknown job `{id}`")).into());
            }
            let jobs = state.jobs.list(project);
            if jobs.is_empty() {
                return Ok("No recent ingestion in this project.".into());
            }
            return Ok(jobs.iter().take(20).map(|j| format!("- {}", describe_job(j))).collect::<Vec<_>>().join("\n"));
        }
        _ => {}
    }
    call_read_tool(state, project, name, args)
}

fn call_read_tool(state: &AppState, project: &str, name: &str, args: &Value) -> anyhow::Result<String> {
    if name == "list_projects" {
        let mut out = String::new();
        for p in state.projects.list()? {
            let _ = writeln!(out, "- `{}`: {} {}", p.id, p.meta.title, p.meta.description);
        }
        return Ok(out);
    }
    let graph = state.projects.get(project)?;
    let detail = match name {
        "search_knowledge" => args.get("query").and_then(Value::as_str).unwrap_or_default().to_string(),
        "explore_entity" => args.get("name").and_then(Value::as_str).unwrap_or_default().to_string(),
        "explore_relation" => format!(
            "{} — {}",
            args.get("a").and_then(Value::as_str).unwrap_or_default(),
            args.get("b").and_then(Value::as_str).unwrap_or_default()
        ),
        "run_cypher" => args.get("query").and_then(Value::as_str).unwrap_or_default().to_string(),
        _ => String::new(),
    };
    let operation = match name {
        "search_knowledge" => "search",
        "explore_entity" | "explore_relation" => "explore",
        "list_documents" => "list_documents",
        "graph_stats" => "stats",
        "run_cypher" => "cypher",
        _ => "unknown",
    };
    let call = Call::start("mcp", project, operation, detail);
    let mut result_line = String::new();
    let outcome = run_tool(state, &graph, name, args, &mut result_line);
    call.finish(&state.history, &outcome, |text| (result_line.clone(), text.len()));
    outcome
}

fn run_tool(
    state: &AppState,
    graph: &crate::db::Graph,
    name: &str,
    args: &Value,
    result_line: &mut String,
) -> anyhow::Result<String> {
    match name {
        "search_knowledge" => {
            let req = SearchRequest {
                query: arg_str(args, "query")?,
                k: args.get("k").and_then(Value::as_u64).map(|k| k as usize),
                use_graph: None,
                use_keywords: None,
                rerank: None,
                include_drafts: args.get("include_drafts").and_then(Value::as_bool),
                tags: arg_strings(args, "tags"),
                min_score: args.get("min_score").and_then(Value::as_f64),
            };
            let res = search::search(state, graph, req)?;
            *result_line = count(res.chunks.len(), "passage", "passages");
            let mut out = res.context;
            if !res.chunks.is_empty() {
                out.push_str("\n## Sources\n");
                for (n, c) in res.chunks.iter().enumerate() {
                    let _ = writeln!(out, "[{}] {} — doc `{}`, chunk `{}`", n + 1, c.doc_title, c.doc_id, c.id);
                }
            }
            Ok(out)
        }
        "explore_entity" => {
            let name = arg_str(args, "name")?;
            let Some(e) = explore::find_entity(graph, &name)?.map(|id| explore::get_entity(graph, &id)).transpose()?.flatten()
            else {
                *result_line = "aucune entité".into();
                return Ok(format!("No entity matches \"{name}\"."));
            };
            *result_line = count(e.neighbours.len(), "voisin", "voisins");
            let mut out = format!(
                "# {} ({})\nid: `{}`, mentioned in {} passages\n\n## Co-occurs with\n",
                e.entity.name, e.entity.label, e.entity.id, e.entity.mentions
            );
            for n in &e.neighbours {
                let _ = writeln!(out, "- {} ({}) ×{}", n.entity.name, n.entity.label, n.weight);
            }
            out.push_str("\n## Passages\n");
            for p in &e.passages {
                let _ = write!(out, "\n### {} (passage {}, {})\n{}\n", p.doc_title, p.idx + 1, p.doc_status, p.text);
            }
            Ok(out)
        }
        "explore_relation" => {
            let (a, b) = (arg_str(args, "a")?, arg_str(args, "b")?);
            let (Some(ia), Some(ib)) = (explore::find_entity(graph, &a)?, explore::find_entity(graph, &b)?) else {
                *result_line = "entité inconnue".into();
                return Ok(format!("No entity matches \"{a}\" or \"{b}\"."));
            };
            let Some(rel) = explore::relation(graph, &ia, &ib)? else {
                return Ok("No such entities.".into());
            };
            *result_line = count(rel.passages.len(), "passage", "passages");
            let mut out = format!(
                "# {} ({}) — {} ({})\n{} passages cite both; link strength {:.2} (share of the passages citing either that cite both).\n",
                rel.a.name, rel.a.label, rel.b.name, rel.b.label, rel.weight, rel.strength
            );
            for p in &rel.passages {
                let loc = p.page.map_or_else(|| format!("passage {}", p.idx + 1), |pg| format!("page {pg}"));
                let _ = write!(out, "\n### {} ({loc}, {})\n{}\n", p.doc_title, p.doc_status, p.text);
            }
            Ok(out)
        }
        "list_documents" => {
            let filter = explore::DocumentFilter {
                q: String::new(),
                status: args.get("status").and_then(Value::as_str).unwrap_or_default().to_string(),
                tag: args.get("tag").and_then(Value::as_str).unwrap_or_default().to_string(),
            };
            let docs = explore::list_documents(graph, &filter)?;
            *result_line = count(docs.len(), "document", "documents");
            if docs.is_empty() {
                return Ok("No document matches.".into());
            }
            let mut out = String::new();
            for d in docs {
                let tags = if d.tags.is_empty() { String::new() } else { format!(", tags: {}", d.tags.join(", ")) };
                let _ = writeln!(
                    out,
                    "- {} (id `{}`, {}, by {}, {} chunks, {} entities{tags}, updated {})",
                    d.title, d.id, d.status, d.creator, d.chunks, d.entities, d.updated_at
                );
            }
            Ok(out)
        }
        "graph_stats" => {
            *result_line = "statistiques".into();
            Ok(serde_json::to_string_pretty(&explore::stats(graph)?)?)
        }
        "run_cypher" => {
            let res = explore::cypher(graph, &arg_str(args, "query")?)?;
            *result_line = count(res.rows.len(), "ligne", "lignes");
            let mut text = serde_json::to_string(&res)?;
            if text.len() > 60_000 {
                text.truncate(text.floor_char_boundary(60_000));
                text.push_str("… [truncated]");
            }
            Ok(text)
        }
        other => Err(Invalid(format!("unknown tool `{other}`")).into()),
    }
}
