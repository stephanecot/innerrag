//! REST API, MCP endpoints and static UI.

use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, Multipart, Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;
use tower::ServiceBuilder;
use tower_http::cors::CorsLayer;
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;

use crate::db::Graph;
use crate::history::{count, Call, HistoryFilter, HistoryView};
use crate::ingest::{DocumentPatch, IngestRequest, Mode};
use crate::jobs::{Job, Stage};
use crate::search::SearchRequest;
use crate::{explore, extract, ingest, mcp, projects, search, AppState, Conflict, Invalid, NotFound};

pub type Shared = Arc<AppState>;

pub struct ApiError(StatusCode, String);

impl ApiError {
    fn not_found(what: &str) -> Self {
        Self(StatusCode::NOT_FOUND, format!("{what} not found"))
    }

    pub fn message(self) -> String {
        self.1
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        if let Some(x) = e.downcast_ref::<Invalid>() {
            return Self(StatusCode::BAD_REQUEST, x.0.clone());
        }
        if let Some(x) = e.downcast_ref::<NotFound>() {
            return Self(StatusCode::NOT_FOUND, x.0.clone());
        }
        if let Some(x) = e.downcast_ref::<Conflict>() {
            return Self(StatusCode::CONFLICT, x.0.clone());
        }
        tracing::error!("{e:#}");
        Self(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}"))
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

type ApiResult<T> = Result<Json<T>, ApiError>;

/// Runs blocking database / model work off the async runtime.
pub async fn blocking<T, F>(state: &Shared, work: F) -> Result<T, ApiError>
where
    T: Send + 'static,
    F: FnOnce(&Shared) -> anyhow::Result<T> + Send + 'static,
{
    let state = state.clone();
    tokio::task::spawn_blocking(move || work(&state))
        .await
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map_err(ApiError::from)
}

/// `ui` when the request comes from the bundled interface, `rest` otherwise.
fn channel(headers: &HeaderMap) -> &'static str {
    match headers.get("x-innerrag-client").and_then(|v| v.to_str().ok()) {
        Some("ui") => "ui",
        _ => "rest",
    }
}

/// Same as [`blocking`], with the project's database opened.
pub async fn in_project<T, F>(state: &Shared, project: String, work: F) -> Result<T, ApiError>
where
    T: Send + 'static,
    F: FnOnce(&Shared, &Graph) -> anyhow::Result<T> + Send + 'static,
{
    blocking(state, move |s| {
        let graph = s.projects.get(&project)?;
        work(s, &graph)
    })
    .await
}

pub fn router(state: Shared) -> Router {
    let ui_dir = state.config.ui_dir.clone();
    let project = Router::new()
        .route("/", get(get_project).patch(update_project).delete(delete_project))
        .route("/watch/scan", post(scan_watch))
        .route("/stats", get(stats))
        .route("/documents", get(list_documents).post(create_document))
        .route("/documents/upload", post(upload_document))
        .route(
            "/documents/{id}",
            get(get_document).put(replace_document).patch(patch_document).delete(delete_document),
        )
        .route("/documents/{id}/upload", axum::routing::put(upload_replace))
        .route("/documents/{id}/content", get(document_content))
        .route("/documents/{id}/passages", get(document_passages))
        .route("/documents/{id}/file", get(document_file))
        .route("/tags", get(tags))
        .route("/entities", get(list_entities))
        .route("/entities/{id}", get(get_entity))
        .route("/relation", get(get_relation))
        .route("/graph", get(graph))
        .route("/graph/neighbourhood/{id}", get(neighbourhood))
        .route("/search", post(search_handler))
        .route("/eval", get(get_eval).put(put_eval))
        .route("/eval/run", post(run_eval))
        .route("/passages", post(read_passages))
        .route("/feedback", get(get_feedback))
        .route("/feedback/cite", post(cite_feedback))
        .route("/feedback/dismiss", post(dismiss_feedback))
        .route("/feedback/accept", post(accept_feedback))
        .route("/cypher", post(cypher));
    let api = Router::new()
        .route("/config", get(config))
        .route("/history", get(history).delete(clear_history))
        .route("/jobs", get(list_jobs))
        .route("/jobs/{id}", get(get_job).delete(cancel_job))
        .route("/projects", get(list_projects).post(create_project))
        .nest("/projects/{project}", project)
        .fallback(|| async { ApiError::not_found("endpoint") });
    Router::new()
        .route("/api/health", get(health))
        .nest("/api", api)
        .route("/mcp", post(mcp::handle_default).get(mcp::reject_get).delete(mcp::reject_get))
        .route("/mcp/{project}", post(mcp::handle_project).get(mcp::reject_get).delete(mcp::reject_get))
        // Revalidate the UI on every load so a rebuilt image is picked up at once
        // (assets have hashed names, index.html does not).
        .fallback_service(
            ServiceBuilder::new()
                .layer(SetResponseHeaderLayer::overriding(header::CACHE_CONTROL, HeaderValue::from_static("no-cache")))
                .service(ServeDir::new(&ui_dir).fallback(ServeFile::new(ui_dir.join("index.html")))),
        )
        .layer(DefaultBodyLimit::max(state.config.max_upload_mb * 1024 * 1024))
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn health(State(state): State<Shared>) -> Json<serde_json::Value> {
    Json(json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
        "uptime_s": state.started.elapsed().as_secs(),
    }))
}

async fn config(State(state): State<Shared>) -> Json<serde_json::Value> {
    let c = &state.config;
    Json(json!({
        "user": c.user,
        "default_project": c.default_project,
        "default_status": c.default_status,
        "ner_labels": c.ner_labels,
        "ner_threshold": c.ner_threshold,
        "chunk_size": c.chunk_size,
        "chunk_overlap": c.chunk_overlap,
        "max_upload_mb": c.max_upload_mb,
        "formats": extract::SUPPORTED,
        "embedding_dim": state.embedder.dim(),
        "entity_seed_distance": c.entity_seed_distance,
        "min_score": c.min_score,
        "models": state.models,
        "data_dir": c.data_dir,
        "lbug_version": lbug::VERSION,
        "watch_root": c.watch_root,
    }))
}

async fn history(State(state): State<Shared>, Query(filter): Query<HistoryFilter>) -> Json<HistoryView> {
    Json(state.history.query(&filter))
}

#[derive(Deserialize)]
struct ClearHistory {
    /// Only this project's calls; every call when absent.
    #[serde(default)]
    project: String,
}

async fn clear_history(State(state): State<Shared>, Query(q): Query<ClearHistory>) -> ApiResult<serde_json::Value> {
    let removed = state.history.clear(&q.project)?;
    Ok(Json(serde_json::json!({ "removed": removed })))
}

// ---- Projects -------------------------------------------------------------------

#[derive(Deserialize)]
struct ProjectBody {
    id: Option<String>,
    title: Option<String>,
    description: Option<String>,
    /// Folder under the watch root to keep in sync; "" stops following.
    watch_dir: Option<String>,
}

async fn list_projects(State(state): State<Shared>) -> ApiResult<Vec<projects::ProjectInfo>> {
    Ok(Json(blocking(&state, |s| s.projects.list()).await?))
}

async fn create_project(
    State(state): State<Shared>,
    Json(body): Json<ProjectBody>,
) -> Result<(StatusCode, Json<projects::ProjectInfo>), ApiError> {
    let id = body.id.unwrap_or_default().trim().to_string();
    let info = blocking(&state, move |s| s.projects.create(&id, body.title, body.description)).await?;
    Ok((StatusCode::CREATED, Json(info)))
}

async fn get_project(State(state): State<Shared>, Path(project): Path<String>) -> ApiResult<projects::ProjectInfo> {
    Ok(Json(blocking(&state, move |s| s.projects.info(&project)).await?))
}

async fn update_project(
    State(state): State<Shared>,
    Path(project): Path<String>,
    Json(body): Json<ProjectBody>,
) -> ApiResult<projects::ProjectInfo> {
    Ok(Json(
        blocking(&state, move |s| {
            if let Some(dir) = body.watch_dir.as_deref().filter(|d| !d.trim().is_empty()) {
                crate::watch::resolve(&s.config.watch_root, dir.trim())?;
            }
            let rescan = body.watch_dir.is_some();
            let info = s.projects.update(&project, body.title, body.description, body.watch_dir)?;
            if rescan && info.meta.watch_dir.is_some() {
                crate::watch::scan(s, &project)?;
                return s.projects.info(&project);
            }
            Ok(info)
        })
        .await?,
    ))
}

async fn scan_watch(State(state): State<Shared>, Path(project): Path<String>) -> ApiResult<crate::watch::ScanReport> {
    Ok(Json(blocking(&state, move |s| crate::watch::scan(s, &project)).await?))
}

async fn delete_project(State(state): State<Shared>, Path(project): Path<String>) -> Result<StatusCode, ApiError> {
    blocking(&state, move |s| s.projects.delete(&project)).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn stats(State(state): State<Shared>, Path(project): Path<String>) -> ApiResult<explore::Stats> {
    Ok(Json(in_project(&state, project, |_, g| explore::stats(g)).await?))
}

// ---- Documents ------------------------------------------------------------------

async fn list_documents(
    State(state): State<Shared>,
    Path(project): Path<String>,
    Query(filter): Query<explore::DocumentFilter>,
) -> ApiResult<Vec<explore::DocumentSummary>> {
    Ok(Json(in_project(&state, project, move |_, g| explore::list_documents(g, &filter)).await?))
}

#[derive(Deserialize)]
struct WaitQuery {
    /// Answer once the ingestion is finished instead of returning the queued job.
    #[serde(default)]
    wait: bool,
}

/// 202 + job by default; with `?wait=true`, the ingestion report (or its error).
async fn job_response(state: &Shared, job: Job, wait: bool, created: bool) -> Result<Response, ApiError> {
    if !wait {
        return Ok((StatusCode::ACCEPTED, Json(job)).into_response());
    }
    let job = ingest::wait(state, &job.id, None).await.ok_or_else(|| ApiError::not_found("job"))?;
    match (job.stage, job.report) {
        (Stage::Done, Some(report)) => {
            let code = if created { StatusCode::CREATED } else { StatusCode::OK };
            Ok((code, Json(report)).into_response())
        }
        _ => Err(ApiError(StatusCode::INTERNAL_SERVER_ERROR, job.error.unwrap_or_else(|| "ingestion failed".into()))),
    }
}

async fn create_document(
    State(state): State<Shared>,
    headers: HeaderMap,
    Path(project): Path<String>,
    Query(w): Query<WaitQuery>,
    Json(req): Json<IngestRequest>,
) -> Result<Response, ApiError> {
    let ch = channel(&headers);
    let job = blocking(&state, move |s| ingest::submit(s, &project, req, Mode::Create, ch, "")).await?;
    job_response(&state, job, w.wait, true).await
}

async fn replace_document(
    State(state): State<Shared>,
    headers: HeaderMap,
    Path((project, id)): Path<(String, String)>,
    Query(w): Query<WaitQuery>,
    Json(mut req): Json<IngestRequest>,
) -> Result<Response, ApiError> {
    req.id = Some(id);
    let ch = channel(&headers);
    let job = blocking(&state, move |s| ingest::submit(s, &project, req, Mode::Upsert, ch, "")).await?;
    job_response(&state, job, w.wait, false).await
}

struct Upload {
    filename: String,
    bytes: Vec<u8>,
    fields: std::collections::HashMap<String, String>,
}

async fn read_upload(mut multipart: Multipart) -> Result<Upload, ApiError> {
    let bad = |e: axum::extract::multipart::MultipartError| ApiError(StatusCode::BAD_REQUEST, e.body_text());
    let mut upload = Upload { filename: String::new(), bytes: Vec::new(), fields: Default::default() };
    while let Some(field) = multipart.next_field().await.map_err(bad)? {
        let name = field.name().unwrap_or_default().to_string();
        if name == "file" {
            upload.filename = field.file_name().unwrap_or("document").to_string();
            upload.bytes = field.bytes().await.map_err(bad)?.to_vec();
        } else {
            upload.fields.insert(name, field.text().await.map_err(bad)?);
        }
    }
    if upload.bytes.is_empty() {
        return Err(ApiError(StatusCode::BAD_REQUEST, "missing or empty `file` field".into()));
    }
    Ok(upload)
}

/// Extracts the file and builds the ingestion request (form fields win over file metadata).
fn upload_request(upload: Upload, id: Option<String>) -> anyhow::Result<(IngestRequest, String)> {
    let field = |k: &str| upload.fields.get(k).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
    let fields = ingest::FileFields {
        id: id.or_else(|| field("id")),
        title: field("title"),
        source: field("source"),
        tags: field("tags").map(|raw| {
            serde_json::from_str::<Vec<String>>(&raw)
                .unwrap_or_else(|_| raw.split(',').map(|t| t.trim().to_string()).collect())
        }),
        status: field("status"),
        creator: field("creator"),
        metadata: field("metadata").and_then(|m| serde_json::from_str::<serde_json::Value>(&m).ok()),
    };
    let filename = upload.filename.clone();
    Ok((ingest::file_request(&upload.filename, upload.bytes, fields)?, filename))
}

async fn upload_document(
    State(state): State<Shared>,
    headers: HeaderMap,
    Path(project): Path<String>,
    Query(w): Query<WaitQuery>,
    multipart: Multipart,
) -> Result<Response, ApiError> {
    let upload = read_upload(multipart).await?;
    let ch = channel(&headers);
    let job = blocking(&state, move |s| {
        let (req, filename) = upload_request(upload, None)?;
        // A file uploaded again under the same id replaces it only through PUT.
        ingest::submit(s, &project, req, Mode::Create, ch, &filename)
    })
    .await?;
    job_response(&state, job, w.wait, true).await
}

async fn upload_replace(
    State(state): State<Shared>,
    headers: HeaderMap,
    Path((project, id)): Path<(String, String)>,
    Query(w): Query<WaitQuery>,
    multipart: Multipart,
) -> Result<Response, ApiError> {
    let upload = read_upload(multipart).await?;
    let ch = channel(&headers);
    let job = blocking(&state, move |s| {
        let (req, filename) = upload_request(upload, Some(id))?;
        ingest::submit(s, &project, req, Mode::Upsert, ch, &filename)
    })
    .await?;
    job_response(&state, job, w.wait, false).await
}

#[derive(Deserialize)]
struct JobsQuery {
    #[serde(default)]
    project: String,
}

async fn list_jobs(State(state): State<Shared>, Query(q): Query<JobsQuery>) -> Json<Vec<Job>> {
    Json(state.jobs.list(&q.project))
}

async fn cancel_job(State(state): State<Shared>, Path(id): Path<String>) -> Result<StatusCode, ApiError> {
    if state.jobs.cancel(&id) {
        Ok(StatusCode::ACCEPTED)
    } else {
        Err(ApiError(StatusCode::CONFLICT, "job unknown or already finished".into()))
    }
}

async fn get_job(State(state): State<Shared>, Path(id): Path<String>) -> ApiResult<Job> {
    state.jobs.get(&id).map(Json).ok_or_else(|| ApiError::not_found("job"))
}

async fn get_document(
    State(state): State<Shared>,
    Path((project, id)): Path<(String, String)>,
) -> ApiResult<explore::DocumentDetail> {
    in_project(&state, project, move |_, g| explore::get_document(g, &id))
        .await?
        .map(Json)
        .ok_or_else(|| ApiError::not_found("document"))
}

async fn document_content(
    State(state): State<Shared>,
    Path((project, id)): Path<(String, String)>,
) -> ApiResult<explore::DocumentContent> {
    in_project(&state, project, move |_, g| explore::document_content(g, &id))
        .await?
        .map(Json)
        .ok_or_else(|| ApiError::not_found("document"))
}

#[derive(Deserialize)]
struct PageQuery {
    #[serde(default)]
    offset: usize,
    limit: Option<usize>,
}

async fn document_passages(
    State(state): State<Shared>,
    Path((project, id)): Path<(String, String)>,
    Query(q): Query<PageQuery>,
) -> ApiResult<explore::PassagePage> {
    let limit = q.limit.unwrap_or(50);
    Ok(Json(in_project(&state, project, move |_, g| explore::document_passages(g, &id, q.offset, limit)).await?))
}

/// The original file, shown inline (the browser's PDF viewer honours `#page=N`).
async fn document_file(
    State(state): State<Shared>,
    Path((project, id)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let (path, name) = in_project(&state, project, move |_, g| explore::document_file(g, &id))
        .await?
        .ok_or_else(|| ApiError::not_found("original file"))?;
    let bytes = tokio::fs::read(&path).await.map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let mime = match path.extension().and_then(|e| e.to_str()).unwrap_or_default() {
        "pdf" => "application/pdf",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "doc" => "application/msword",
        "ppt" => "application/vnd.ms-powerpoint",
        "md" | "markdown" => "text/markdown; charset=utf-8",
        "html" | "htm" => "text/plain; charset=utf-8",
        _ => "text/plain; charset=utf-8",
    };
    let safe_name: String = name.chars().map(|c| if c.is_ascii_graphic() || c == ' ' { c } else { '_' }).collect();
    let disposition = format!("inline; filename=\"{}\"", safe_name.replace('"', ""));
    Ok((
        [
            (header::CONTENT_TYPE, HeaderValue::from_static(mime)),
            (header::CONTENT_DISPOSITION, HeaderValue::from_str(&disposition).unwrap_or(HeaderValue::from_static("inline"))),
        ],
        bytes,
    )
        .into_response())
}

async fn patch_document(
    State(state): State<Shared>,
    headers: HeaderMap,
    Path((project, id)): Path<(String, String)>,
    Json(patch): Json<DocumentPatch>,
) -> ApiResult<explore::DocumentDetail> {
    let call = Call::start(channel(&headers), &project, "update", id.clone());
    in_project(&state, project, move |s, g| {
        let outcome = ingest::update_document(g, &id, patch);
        call.finish(&s.history, &outcome, |()| ("métadonnées".into(), 0));
        outcome?;
        explore::get_document(g, &id)
    })
    .await?
    .map(Json)
    .ok_or_else(|| ApiError::not_found("document"))
}

async fn delete_document(
    State(state): State<Shared>,
    headers: HeaderMap,
    Path((project, id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let call = Call::start(channel(&headers), &project, "delete", id.clone());
    in_project(&state, project, move |s, g| {
        let outcome = ingest::delete_document(g, &id);
        call.finish(&s.history, &outcome, |()| ("supprimé".into(), 0));
        outcome
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn tags(State(state): State<Shared>, Path(project): Path<String>) -> ApiResult<Vec<explore::TagCount>> {
    Ok(Json(in_project(&state, project, |_, g| explore::tags(g)).await?))
}

// ---- Graph ----------------------------------------------------------------------

fn yes() -> bool {
    true
}

#[derive(Deserialize)]
struct EntityQuery {
    #[serde(default)]
    q: String,
    #[serde(default)]
    label: String,
    #[serde(default = "yes")]
    include_drafts: bool,
    limit: Option<usize>,
}

async fn list_entities(
    State(state): State<Shared>,
    Path(project): Path<String>,
    Query(p): Query<EntityQuery>,
) -> ApiResult<Vec<explore::EntitySummary>> {
    let limit = p.limit.unwrap_or(100);
    let entities =
        in_project(&state, project, move |_, g| explore::list_entities(g, &p.q, &p.label, p.include_drafts, limit))
            .await?;
    Ok(Json(entities))
}

#[derive(Deserialize)]
struct EntityDocQuery {
    /// Only the passages of this document.
    doc: Option<String>,
}

async fn get_entity(
    State(state): State<Shared>,
    headers: HeaderMap,
    Path((project, id)): Path<(String, String)>,
    Query(q): Query<EntityDocQuery>,
) -> ApiResult<explore::EntityDetail> {
    let call = Call::start(channel(&headers), &project, "explore", id.clone());
    in_project(&state, project, move |s, g| {
        let outcome = explore::get_entity(g, &id, q.doc.as_deref().filter(|d| !d.is_empty()));
        call.finish(&s.history, &outcome, |e| {
            let size = e.as_ref().map_or(0, |e| serde_json::to_string(e).map_or(0, |j| j.len()));
            let n = e.as_ref().map_or(0, |e| e.neighbours.len());
            (count(n, "voisin", "voisins"), size)
        });
        outcome
    })
        .await?
        .map(Json)
        .ok_or_else(|| ApiError::not_found("entity"))
}

#[derive(Deserialize)]
struct RelationQuery {
    a: String,
    b: String,
}

async fn get_relation(
    State(state): State<Shared>,
    Path(project): Path<String>,
    Query(q): Query<RelationQuery>,
) -> ApiResult<explore::RelationDetail> {
    in_project(&state, project, move |_, g| explore::relation(g, &q.a, &q.b))
        .await?
        .map(Json)
        .ok_or_else(|| ApiError::not_found("entity"))
}

#[derive(Deserialize)]
struct GraphQuery {
    limit: Option<usize>,
    min_weight: Option<i64>,
    #[serde(default)]
    label: String,
    #[serde(default = "yes")]
    include_drafts: bool,
    /// Only the entities of this document, linked within it.
    #[serde(default)]
    doc: String,
}

async fn graph(
    State(state): State<Shared>,
    Path(project): Path<String>,
    Query(p): Query<GraphQuery>,
) -> ApiResult<explore::GraphView> {
    let limit = p.limit.unwrap_or(150);
    let min_weight = p.min_weight.unwrap_or(1);
    let view = in_project(&state, project, move |_, g| {
        if p.doc.is_empty() {
            explore::graph(g, limit, min_weight, &p.label, p.include_drafts)
        } else {
            explore::document_graph(g, &p.doc, limit, min_weight, &p.label)
        }
    })
    .await?;
    Ok(Json(view))
}

#[derive(Deserialize)]
struct NeighbourhoodQuery {
    limit: Option<usize>,
}

async fn neighbourhood(
    State(state): State<Shared>,
    Path((project, id)): Path<(String, String)>,
    Query(p): Query<NeighbourhoodQuery>,
) -> ApiResult<explore::GraphView> {
    let limit = p.limit.unwrap_or(25);
    Ok(Json(in_project(&state, project, move |_, g| explore::neighbourhood(g, &id, limit)).await?))
}

async fn search_handler(
    State(state): State<Shared>,
    headers: HeaderMap,
    Path(project): Path<String>,
    Json(req): Json<SearchRequest>,
) -> ApiResult<search::SearchResponse> {
    let ch = channel(&headers);
    let call = Call::start(ch, &project, "search", req.query.clone());
    Ok(Json(
        in_project(&state, project.clone(), move |s, g| {
            let opts = (req.mode.is_some() || req.budget.is_some() || req.session_id.is_some()).then(|| -> anyhow::Result<_> {
                Ok(crate::context::Options {
                    mode: crate::context::Mode::parse(req.mode.as_deref())?,
                    budget: req.budget,
                    session: req.session_id.clone().filter(|s| !s.is_empty()),
                })
            });
            let opts = opts.transpose()?;
            let mut outcome = search::search(s, g, req);
            if let (Ok(r), Some(opts)) = (&mut outcome, &opts) {
                r.context = crate::context::render(s, &project, r, opts)?;
            }
            if let Ok(r) = &outcome {
                crate::feedback::record_search(g, &r.query, ch, r.chunks.iter().map(|c| c.id.clone()).collect(), r.best_similarity);
            }
            call.finish(&s.history, &outcome, |r| (count(r.chunks.len(), "passage", "passages"), r.context.len()));
            outcome
        })
        .await?,
    ))
}

#[derive(Deserialize)]
struct ReadBody {
    ids: Vec<String>,
    #[serde(default)]
    window: i64,
    session_id: Option<String>,
}

async fn read_passages(
    State(state): State<Shared>,
    headers: HeaderMap,
    Path(project): Path<String>,
    Json(body): Json<ReadBody>,
) -> ApiResult<serde_json::Value> {
    let call = Call::start(channel(&headers), &project, "read", body.ids.join(", "));
    let text = in_project(&state, project.clone(), move |s, g| {
        let outcome = crate::context::read_passages(g, &project, &body.ids, body.window, body.session_id.as_deref());
        call.finish(&s.history, &outcome, |t| (count(body.ids.len(), "passage", "passages"), t.len()));
        outcome
    })
    .await?;
    Ok(Json(json!({ "text": text })))
}

async fn get_feedback(State(state): State<Shared>, Path(project): Path<String>) -> ApiResult<crate::feedback::Report> {
    Ok(Json(in_project(&state, project, |s, g| crate::feedback::report(s, g)).await?))
}

#[derive(Deserialize)]
struct CiteBody {
    question: String,
    #[serde(default)]
    chunk_ids: Vec<String>,
    outcome: String,
}

async fn cite_feedback(
    State(state): State<Shared>,
    headers: HeaderMap,
    Path(project): Path<String>,
    Json(body): Json<CiteBody>,
) -> Result<StatusCode, ApiError> {
    let ch = channel(&headers);
    in_project(&state, project, move |_, g| {
        let outcome = crate::feedback::Outcome::parse(&body.outcome)?;
        crate::feedback::cite(g, &body.question, body.chunk_ids, outcome, ch)
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct FeedbackKey {
    key: String,
    /// accept: add as an out-of-scope question (no passage expected).
    #[serde(default)]
    out_of_scope: bool,
}

async fn dismiss_feedback(
    State(state): State<Shared>,
    Path(project): Path<String>,
    Json(body): Json<FeedbackKey>,
) -> Result<StatusCode, ApiError> {
    in_project(&state, project, move |_, g| crate::feedback::dismiss(g, &body.key)).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn accept_feedback(
    State(state): State<Shared>,
    Path(project): Path<String>,
    Json(body): Json<FeedbackKey>,
) -> ApiResult<crate::eval::EvalSet> {
    Ok(Json(in_project(&state, project, move |s, g| crate::feedback::accept(s, g, &body.key, body.out_of_scope)).await?))
}

async fn get_eval(State(state): State<Shared>, Path(project): Path<String>) -> ApiResult<crate::eval::EvalSet> {
    Ok(Json(in_project(&state, project, |_, g| crate::eval::load(g)).await?))
}

async fn put_eval(
    State(state): State<Shared>,
    Path(project): Path<String>,
    Json(set): Json<crate::eval::EvalSet>,
) -> ApiResult<crate::eval::EvalSet> {
    Ok(Json(in_project(&state, project, move |_, g| crate::eval::save(g, set)).await?))
}

async fn run_eval(
    State(state): State<Shared>,
    Path(project): Path<String>,
    Json(req): Json<crate::eval::RunRequest>,
) -> ApiResult<crate::eval::Report> {
    Ok(Json(in_project(&state, project, move |s, g| crate::eval::run(s, g, req)).await?))
}

#[derive(Deserialize)]
struct CypherRequest {
    query: String,
}

async fn cypher(
    State(state): State<Shared>,
    headers: HeaderMap,
    Path(project): Path<String>,
    Json(req): Json<CypherRequest>,
) -> ApiResult<explore::CypherResult> {
    let call = Call::start(channel(&headers), &project, "cypher", req.query.clone());
    Ok(Json(
        in_project(&state, project, move |s, g| {
            let outcome = explore::cypher(g, &req.query);
            call.finish(&s.history, &outcome, |r| {
                (count(r.rows.len(), "ligne", "lignes"), serde_json::to_string(&r.rows).map_or(0, |j| j.len()))
            });
            outcome
        })
        .await?,
    ))
}
