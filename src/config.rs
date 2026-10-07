use std::env;
use std::path::PathBuf;
use std::str::FromStr;

/// Runtime configuration, read from `INNERRAG_*` environment variables.
#[derive(Debug, Clone)]
pub struct Config {
    pub bind: String,
    /// Root of the mounted data; projects live in `<data_dir>/projects/<id>/`.
    pub data_dir: PathBuf,
    /// Project used by `/mcp` and created at startup when no project exists.
    pub default_project: String,
    /// Creator recorded on documents (single-user for now).
    pub user: String,
    /// Status given to new documents when the request does not set one.
    pub default_status: String,
    pub models_dir: PathBuf,
    pub ui_dir: PathBuf,
    /// Buffer pool of each open project database.
    pub buffer_pool_mb: u64,
    pub threads: usize,
    pub ner_labels: Vec<String>,
    pub ner_threshold: f32,
    /// Extra words never kept as entities (added to the built-in list).
    pub entity_stopwords: Vec<String>,
    pub chunk_size: usize,
    pub chunk_overlap: usize,
    /// `auto` (GPU when usable, else CPU), `cpu` or `cuda` (GPU required).
    pub device: String,
    /// Keep uploaded files (PDF, Word…) in `<project>/files/` to read them as they are.
    pub keep_originals: bool,
    /// Largest accepted request (file uploads), in MB.
    pub max_upload_mb: usize,
    /// Folders a project may follow must live under this root (mount them there).
    pub watch_root: std::path::PathBuf,
    /// Seconds between two passes over watched folders.
    pub watch_interval_s: u64,
    /// Rerank search results with the cross-encoder when it is installed.
    pub rerank: bool,
    /// Minimum similarity (cosine, 0–1) for a passage to be returned by a search.
    pub min_score: f64,
    /// Maximum cosine distance for an entity to be used as a search seed.
    pub entity_seed_distance: f64,
}

fn var<T: FromStr>(name: &str, default: T) -> T {
    env::var(name)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(default)
}

fn string(name: &str, default: &str) -> String {
    env::var(name)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| default.to_string())
}

pub fn parse_labels(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

impl Config {
    pub fn from_env() -> Self {
        let default_threads = std::thread::available_parallelism().map_or(4, |n| n.get());
        Self {
            bind: string("INNERRAG_BIND", "0.0.0.0:8080"),
            data_dir: string("INNERRAG_DATA", "/data").into(),
            default_project: string("INNERRAG_DEFAULT_PROJECT", "default"),
            user: string("INNERRAG_USER", "local"),
            default_status: string("INNERRAG_DEFAULT_STATUS", "PUBLISHED").to_uppercase(),
            models_dir: string("INNERRAG_MODELS", "/models").into(),
            ui_dir: string("INNERRAG_UI", "/app/ui").into(),
            buffer_pool_mb: var("INNERRAG_BUFFER_POOL_MB", 256),
            threads: var("INNERRAG_THREADS", default_threads),
            ner_labels: parse_labels(&string(
                "INNERRAG_NER_LABELS",
                "person,organization,location,event,product,technology",
            )),
            ner_threshold: var("INNERRAG_NER_THRESHOLD", 0.5),
            entity_stopwords: parse_labels(&string("INNERRAG_ENTITY_STOPWORDS", "")),
            chunk_size: var("INNERRAG_CHUNK_SIZE", 1000),
            chunk_overlap: var("INNERRAG_CHUNK_OVERLAP", 150),
            entity_seed_distance: var("INNERRAG_ENTITY_SEED_DISTANCE", 0.18),
            min_score: var("INNERRAG_MIN_SCORE", 0.80),
            rerank: var("INNERRAG_RERANK", true),
            watch_root: string("INNERRAG_WATCH_ROOT", "/watch").into(),
            watch_interval_s: var("INNERRAG_WATCH_INTERVAL", 30),
            max_upload_mb: var("INNERRAG_MAX_UPLOAD_MB", 200),
            keep_originals: var("INNERRAG_KEEP_ORIGINALS", true),
            device: string("INNERRAG_DEVICE", "auto").to_lowercase(),
        }
    }
}
