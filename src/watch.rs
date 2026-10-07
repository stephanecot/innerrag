//! Watched folders: a project can follow a folder mounted under `/watch` (for instance the
//! `docs/` of a git repository). New or changed files are ingested (re-imports only recompute
//! changed passages), deleted files remove their document. Polling keeps it simple and works
//! on bind mounts from macOS and Windows, where file events do not cross into the container.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::ingest::{self, FileFields, Mode};
use crate::{AppState, Invalid};

const STATE_FILE: &str = "watch-state.json";
const EXTENSIONS: [&str; 11] = ["pdf", "docx", "pptx", "doc", "ppt", "md", "markdown", "txt", "html", "htm", "rst"];

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct WatchState {
    /// Relative path → (size, modification time in seconds).
    pub files: HashMap<String, (u64, u64)>,
    pub last_scan: Option<i64>,
    pub last_error: Option<String>,
}

#[derive(Debug, Default, Serialize)]
pub struct ScanReport {
    pub added: usize,
    pub changed: usize,
    pub removed: usize,
    pub unchanged: usize,
    pub errors: Vec<String>,
}

/// Resolves a project's watch folder, refusing anything outside the watch root.
pub fn resolve(root: &Path, dir: &str) -> Result<PathBuf> {
    let candidate = if Path::new(dir).is_absolute() { PathBuf::from(dir) } else { root.join(dir) };
    let canonical = candidate
        .canonicalize()
        .map_err(|_| Invalid(format!("folder `{}` not found (mount it under {})", candidate.display(), root.display())))?;
    let root = root.canonicalize().with_context(|| format!("watch root {} not found", root.display()))?;
    if !canonical.starts_with(&root) || !canonical.is_dir() {
        return Err(Invalid(format!("`{dir}` must be a folder inside {}", root.display())).into());
    }
    Ok(canonical)
}

fn walk(dir: &Path, base: &Path, out: &mut Vec<(String, PathBuf)>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            walk(&path, base, out);
        } else if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| EXTENSIONS.contains(&e.to_lowercase().as_str()))
        {
            if let Ok(rel) = path.strip_prefix(base) {
                out.push((rel.to_string_lossy().replace('\\', "/"), path.clone()));
            }
        }
    }
}

pub fn load_state(project_dir: &Path) -> WatchState {
    std::fs::read(project_dir.join(STATE_FILE)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn save_state(project_dir: &Path, state: &WatchState) {
    if let Ok(json) = serde_json::to_vec_pretty(state) {
        let _ = std::fs::write(project_dir.join(STATE_FILE), json);
    }
}

/// Document id of a watched file: its path in the folder, so it is stable across scans.
fn doc_id(rel: &str) -> String {
    format!("watch/{rel}")
}

/// One pass over a project's folder.
pub fn scan(state: &Arc<AppState>, project: &str) -> Result<ScanReport> {
    let info = state.projects.info(project)?;
    let Some(dir) = info.meta.watch_dir.clone().filter(|d| !d.trim().is_empty()) else {
        return Err(Invalid(format!("project `{project}` follows no folder")).into());
    };
    let project_dir = state.projects.dir(project);
    let mut ws = load_state(&project_dir);
    let mut report = ScanReport::default();
    let folder = match resolve(&state.config.watch_root, &dir) {
        Ok(f) => f,
        Err(e) => {
            ws.last_error = Some(format!("{e:#}"));
            ws.last_scan = Some(time::OffsetDateTime::now_utc().unix_timestamp());
            save_state(&project_dir, &ws);
            return Err(e);
        }
    };
    let mut files = Vec::new();
    walk(&folder, &folder, &mut files);
    // Files recorded as imported whose document is missing from the base are imported again.
    let present: std::collections::HashSet<String> = {
        let graph = state.projects.get(project)?;
        let conn = graph.reader()?;
        crate::db::rows(&conn, "MATCH (d:Document) WHERE d.id STARTS WITH 'watch/' RETURN d.id", vec![])?
            .iter()
            .map(|r| crate::db::as_str(&r[0]))
            .collect()
    };
    // Documents of this project still being ingested are left alone until they are done.
    let busy: Vec<String> = state.jobs.list(project).into_iter().filter(|j| j.finished_at.is_none()).map(|j| j.document_id).collect();
    let mut seen = HashMap::new();
    for (rel, path) in files {
        let Ok(meta) = std::fs::metadata(&path) else { continue };
        let stamp = (meta.len(), meta.modified().ok().and_then(|m| m.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs()));
        seen.insert(rel.clone(), stamp);
        let id = doc_id(&rel);
        match ws.files.get(&rel) {
            Some(old) if *old == stamp && present.contains(&id) => {
                report.unchanged += 1;
                continue;
            }
            _ if busy.contains(&id) => continue,
            Some(_) => report.changed += 1,
            None => report.added += 1,
        }
        let outcome = std::fs::read(&path).map_err(anyhow::Error::from).and_then(|bytes| {
            let filename = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| rel.clone());
            let req = ingest::file_request(
                &filename,
                bytes,
                FileFields { id: Some(id.clone()), source: Some(format!("{dir}/{rel}")), ..Default::default() },
            )?;
            ingest::submit(state, project, req, Mode::Upsert, "watch", &rel)
        });
        match outcome {
            Ok(_) => {
                ws.files.insert(rel, stamp);
            }
            Err(e) => {
                // Remembered as seen so a broken file is not retried every pass; editing it retries.
                ws.files.insert(rel.clone(), stamp);
                report.errors.push(format!("{rel}: {e:#}"));
            }
        }
    }
    // Files gone from the folder: their documents go too.
    let gone: Vec<String> = ws.files.keys().filter(|rel| !seen.contains_key(*rel)).cloned().collect();
    if !gone.is_empty() {
        let graph = state.projects.get(project)?;
        for rel in gone {
            match ingest::delete_document(&graph, &doc_id(&rel)) {
                Ok(()) => report.removed += 1,
                Err(e) if e.downcast_ref::<crate::NotFound>().is_some() => {}
                Err(e) => report.errors.push(format!("{rel}: {e:#}")),
            }
            ws.files.remove(&rel);
        }
    }
    ws.last_scan = Some(time::OffsetDateTime::now_utc().unix_timestamp());
    ws.last_error = report.errors.first().cloned();
    save_state(&project_dir, &ws);
    if report.added + report.changed + report.removed > 0 {
        tracing::info!(project, added = report.added, changed = report.changed, removed = report.removed, "watched folder synchronised");
    }
    Ok(report)
}

/// Background loop over every project that follows a folder.
pub fn start(state: Arc<AppState>) {
    let interval = Duration::from_secs(state.config.watch_interval_s.max(5));
    std::thread::Builder::new()
        .name("watch".into())
        .spawn(move || loop {
            if let Ok(projects) = state.projects.list() {
                for p in projects.into_iter().filter(|p| p.meta.watch_dir.as_deref().is_some_and(|d| !d.trim().is_empty())) {
                    if let Err(e) = scan(&state, &p.id) {
                        tracing::debug!(project = %p.id, "watch: {e:#}");
                    }
                }
            }
            std::thread::sleep(interval);
        })
        .expect("spawning the watch thread");
}
