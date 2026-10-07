//! Projects: one self-contained directory per project, holding its own LadybugDB
//! database and a readable `project.json`. A project directory can be committed to
//! git and mounted into the container.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

use crate::db::Graph;
use crate::{Conflict, Invalid, NotFound};

pub const DB_FILE: &str = "innerrag.lbdb";
const META_FILE: &str = "project.json";
const GITIGNORE: &str = "# LadybugDB transient files (the server checkpoints after every write)\n*.wal\n*.lock\n*.tmp\n";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectMeta {
    pub title: String,
    #[serde(default)]
    pub description: String,
    pub created_at: String,
    /// Model used to compute the stored embeddings; the index is only valid for that model.
    pub embedding_model: String,
    pub embedding_dim: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectInfo {
    pub id: String,
    #[serde(flatten)]
    pub meta: ProjectMeta,
    pub size_bytes: u64,
}

pub struct Projects {
    root: PathBuf,
    open: RwLock<HashMap<String, Arc<Graph>>>,
    buffer_pool_mb: u64,
    threads: usize,
    embedding_model: String,
    dim: usize,
}

pub fn validate_id(id: &str) -> Result<()> {
    let ok = !id.is_empty()
        && id.len() <= 64
        && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
        && id.chars().next().is_some_and(|c| c.is_ascii_alphanumeric());
    if ok {
        Ok(())
    } else {
        Err(Invalid(format!(
            "invalid project id `{id}`: use 1-64 lowercase letters, digits, `-` or `_`, starting with a letter or digit"
        ))
        .into())
    }
}

fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

fn dir_size(path: &Path) -> u64 {
    std::fs::read_dir(path)
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|e| e.metadata().ok())
                .filter(|m| m.is_file())
                .map(|m| m.len())
                .sum()
        })
        .unwrap_or(0)
}

impl Projects {
    pub fn new(root: PathBuf, buffer_pool_mb: u64, threads: usize, embedding_model: String, dim: usize) -> Result<Self> {
        std::fs::create_dir_all(&root).with_context(|| format!("creating {}", root.display()))?;
        Ok(Self { root, open: RwLock::new(HashMap::new()), buffer_pool_mb, threads, embedding_model, dim })
    }

    fn dir(&self, id: &str) -> PathBuf {
        self.root.join(id)
    }

    fn read_meta(&self, id: &str) -> Result<ProjectMeta> {
        let dir = self.dir(id);
        let path = dir.join(META_FILE);
        if path.exists() {
            let raw = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
            return serde_json::from_str(&raw).with_context(|| format!("parsing {}", path.display()));
        }
        if dir.join(DB_FILE).exists() {
            // A database mounted without its project.json: adopt it with the current model.
            let meta = ProjectMeta {
                title: id.to_string(),
                description: String::new(),
                created_at: now_rfc3339(),
                embedding_model: self.embedding_model.clone(),
                embedding_dim: self.dim,
            };
            self.write_meta(id, &meta)?;
            return Ok(meta);
        }
        Err(NotFound(format!("project `{id}` not found")).into())
    }

    fn write_meta(&self, id: &str, meta: &ProjectMeta) -> Result<()> {
        let path = self.dir(id).join(META_FILE);
        std::fs::write(&path, serde_json::to_string_pretty(meta)? + "\n")
            .with_context(|| format!("writing {}", path.display()))
    }

    pub fn info(&self, id: &str) -> Result<ProjectInfo> {
        validate_id(id)?;
        Ok(ProjectInfo { id: id.to_string(), meta: self.read_meta(id)?, size_bytes: dir_size(&self.dir(id)) })
    }

    pub fn list(&self) -> Result<Vec<ProjectInfo>> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(&self.root)?.flatten() {
            let Some(id) = entry.file_name().to_str().map(str::to_string) else { continue };
            if !entry.path().is_dir() || validate_id(&id).is_err() {
                continue;
            }
            match self.info(&id) {
                Ok(info) => out.push(info),
                Err(e) if e.downcast_ref::<NotFound>().is_some() => {}
                Err(e) => tracing::warn!("skipping project {id}: {e:#}"),
            }
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(out)
    }

    pub fn create(&self, id: &str, title: Option<String>, description: Option<String>) -> Result<ProjectInfo> {
        validate_id(id)?;
        let dir = self.dir(id);
        if dir.join(META_FILE).exists() || dir.join(DB_FILE).exists() {
            return Err(Conflict(format!("project `{id}` already exists")).into());
        }
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        let meta = ProjectMeta {
            title: title.filter(|t| !t.trim().is_empty()).unwrap_or_else(|| id.to_string()),
            description: description.unwrap_or_default(),
            created_at: now_rfc3339(),
            embedding_model: self.embedding_model.clone(),
            embedding_dim: self.dim,
        };
        self.write_meta(id, &meta)?;
        std::fs::write(dir.join(".gitignore"), GITIGNORE)?;
        self.get(id)?;
        self.info(id)
    }

    pub fn update(&self, id: &str, title: Option<String>, description: Option<String>) -> Result<ProjectInfo> {
        let mut meta = self.info(id)?.meta;
        if let Some(t) = title.filter(|t| !t.trim().is_empty()) {
            meta.title = t;
        }
        if let Some(d) = description {
            meta.description = d;
        }
        self.write_meta(id, &meta)?;
        self.info(id)
    }

    /// Opens the project database on first use and keeps it open.
    pub fn get(&self, id: &str) -> Result<Arc<Graph>> {
        validate_id(id)?;
        if let Some(g) = self.open.read().map_err(|_| anyhow!("projects lock poisoned"))?.get(id) {
            return Ok(g.clone());
        }
        let mut open = self.open.write().map_err(|_| anyhow!("projects lock poisoned"))?;
        if let Some(g) = open.get(id) {
            return Ok(g.clone());
        }
        let meta = self.read_meta(id)?;
        if meta.embedding_dim != self.dim || meta.embedding_model != self.embedding_model {
            return Err(Invalid(format!(
                "project `{id}` was indexed with {} ({} dims) but this server embeds with {} ({} dims); \
                 re-ingest its documents with a matching image",
                meta.embedding_model, meta.embedding_dim, self.embedding_model, self.dim
            ))
            .into());
        }
        let graph = Arc::new(Graph::open(&self.dir(id).join(DB_FILE), self.buffer_pool_mb, self.threads, self.dim)?);
        tracing::info!(project = id, "project database opened");
        open.insert(id.to_string(), graph.clone());
        Ok(graph)
    }

    /// Closes the database and removes the project directory.
    pub fn delete(&self, id: &str) -> Result<()> {
        self.info(id)?;
        self.open.write().map_err(|_| anyhow!("projects lock poisoned"))?.remove(id);
        std::fs::remove_dir_all(self.dir(id)).with_context(|| format!("removing project {id}"))?;
        tracing::info!(project = id, "project deleted");
        Ok(())
    }

    pub fn close_all(&self) {
        if let Ok(mut open) = self.open.write() {
            for (id, graph) in open.drain() {
                if let Err(e) = graph.checkpoint() {
                    tracing::warn!(project = id, "checkpoint on shutdown failed: {e:#}");
                }
            }
        }
    }
}
