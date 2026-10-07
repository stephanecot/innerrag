mod api;
mod cache;
mod chunk;
mod config;
mod context;
mod db;
mod embed;
mod eval;
mod explore;
mod extract;
mod feedback;
mod history;
mod ingest;
mod jobs;
mod keywords;
mod mcp;
mod ner;
mod projects;
mod rerank;
mod search;
mod watch;

use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use serde_json::json;
use tracing_subscriber::EnvFilter;

use crate::config::Config;
use crate::db::Graph;
use crate::embed::Embedder;
use crate::history::History;
use crate::jobs::Jobs;
use crate::ner::Ner;
use crate::projects::Projects;

pub struct AppState {
    pub config: Config,
    pub projects: Projects,
    pub history: History,
    pub jobs: Arc<Jobs>,
    pub embedder: Embedder,
    pub ner: Ner,
    /// Optional cross-encoder (present when `<models>/rerank` exists and is not disabled).
    pub reranker: Option<rerank::Reranker>,
    pub models: serde_json::Value,
    pub started: Instant,
}

macro_rules! client_error {
    ($($(#[$doc:meta])* $name:ident),*) => {$(
        $(#[$doc])*
        #[derive(Debug)]
        pub struct $name(pub String);

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl std::error::Error for $name {}
    )*};
}

client_error!(
    /// Bad input: HTTP 400.
    Invalid,
    /// Unknown project, document or entity: HTTP 404.
    NotFound,
    /// Already exists: HTTP 409.
    Conflict
);

const USAGE: &str = "usage: innerrag [serve | install-extensions]

  serve               start the HTTP server (default)
  install-extensions  download LadybugDB extensions into $HOME/.lbdb (needs network once)
  convert <file>      print the markdown extracted from a PDF, Word, PowerPoint… file (diagnostics)
  extract <text>      print the entities the NER model finds in <text> (diagnostics)

Configuration is read from INNERRAG_* environment variables (see README).";

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,ort=warn")))
        .init();

    match std::env::args().nth(1).as_deref() {
        None | Some("serve") => serve(),
        Some("install-extensions") => Graph::install_extensions(),
        Some("convert") => convert(std::env::args().nth(2).as_deref()),
        Some("extract") => extract_entities(&std::env::args().skip(2).collect::<Vec<_>>().join(" ")),
        Some("-h" | "--help" | "help") => {
            println!("{USAGE}");
            Ok(())
        }
        Some(other) => {
            eprintln!("unknown command `{other}`\n\n{USAGE}");
            std::process::exit(2);
        }
    }
}

fn convert(path: Option<&str>) -> Result<()> {
    let path = path.ok_or_else(|| anyhow::anyhow!("usage: innerrag convert <file>"))?;
    let bytes = std::fs::read(path).with_context(|| format!("reading {path}"))?;
    let (format, extracted) = extract::extract(path, &bytes)?;
    eprintln!(
        "format: {}, title: {:?}, pages: {:?}, structured: {}, {} characters",
        format.label(), extracted.title, extracted.pages, extracted.structured, extracted.text.len()
    );
    print!("{}", extracted.text);
    Ok(())
}

fn extract_entities(text: &str) -> Result<()> {
    let config = Config::from_env();
    let ner = Ner::load(&config.models_dir.join("ner"), config.threads, config.ner_threshold, false, &config.entity_stopwords)?;
    for m in ner.extract(&[text.to_string()], &config.ner_labels)?.remove(0) {
        println!("{:.3}  {:<14} {}", m.score, m.label, m.name);
    }
    Ok(())
}

/// Loads a model on the GPU when asked or possible, else on the CPU. Returns the device used.
fn on_device<T>(config: &Config, what: &str, load: impl Fn(bool) -> Result<T>) -> Result<(T, &'static str)> {
    match config.device.as_str() {
        "cpu" => Ok((load(false)?, "cpu")),
        "cuda" | "gpu" => Ok((load(true).with_context(|| format!("{what}: INNERRAG_DEVICE=cuda but CUDA is not usable"))?, "cuda")),
        _ => match load(true) {
            Ok(model) => Ok((model, "cuda")),
            Err(e) => {
                tracing::debug!("{what}: no usable GPU ({e:#}), using the CPU");
                Ok((load(false)?, "cpu"))
            }
        },
    }
}

fn load_state(config: Config) -> Result<AppState> {
    let t = Instant::now();
    let embed_dir = config.models_dir.join("embed");
    let (embedder, embed_device) = on_device(&config, "embedding model", |cuda| {
        Embedder::load(&embed_dir, config.threads, cuda)
            .with_context(|| format!("loading embedding model from {}", embed_dir.display()))
    })?;
    tracing::info!(dim = embedder.dim(), device = embed_device, elapsed_ms = t.elapsed().as_millis(), "embedding model loaded");

    let t = Instant::now();
    let ner_dir = config.models_dir.join("ner");
    let (ner, ner_device) = on_device(&config, "NER model", |cuda| {
        Ner::load(&ner_dir, config.threads, config.ner_threshold, cuda, &config.entity_stopwords)
            .with_context(|| format!("loading NER model from {}", ner_dir.display()))
    })?;
    tracing::info!(device = ner_device, elapsed_ms = t.elapsed().as_millis(), labels = ?config.ner_labels, "NER model loaded");

    let rerank_dir = config.models_dir.join("rerank");
    let (reranker, rerank_device) = if config.rerank && rerank_dir.join("model.onnx").exists() {
        let t = Instant::now();
        let (r, device) = on_device(&config, "reranker", |cuda| rerank::Reranker::load(&rerank_dir, config.threads, cuda))?;
        tracing::info!(device, elapsed_ms = t.elapsed().as_millis(), "reranker loaded");
        (Some(r), device)
    } else {
        (None, "none")
    };
    let embedding_model =
        std::env::var("INNERRAG_EMBED_MODEL").unwrap_or_else(|_| embed_dir.display().to_string());
    let projects = Projects::new(
        config.data_dir.join("projects"),
        config.buffer_pool_mb,
        config.threads,
        embedding_model.clone(),
        embedder.dim(),
    )?;
    if projects.list()?.is_empty() {
        projects.create(&config.default_project, None, Some("Projet créé au premier démarrage".into()))?;
        tracing::info!(project = %config.default_project, "default project created");
    }
    tracing::info!(data = %config.data_dir.display(), projects = projects.list()?.len(), "projects ready");

    let models = json!({
        "device": if embed_device == "cuda" || ner_device == "cuda" { "cuda" } else { "cpu" },
        "embedding_device": embed_device,
        "ner_device": ner_device,
        "reranker": reranker.as_ref().map(|_| std::env::var("INNERRAG_RERANK_MODEL").unwrap_or_else(|_| rerank_dir.display().to_string())),
        "reranker_device": rerank_device,
        "embedding": embedding_model,
        "ner": std::env::var("INNERRAG_NER_MODEL").unwrap_or_else(|_| ner_dir.display().to_string()),
    });
    let history = History::open(config.data_dir.join("history"))?;
    Ok(AppState { config, projects, history, jobs: Jobs::start(), embedder, ner, reranker, models, started: Instant::now() })
}

fn serve() -> Result<()> {
    let config = Config::from_env();
    let bind = config.bind.clone();
    let state = Arc::new(load_state(config)?);

    let resumed = ingest::resume_pending(&state);
    if resumed > 0 {
        tracing::info!(resumed, "interrupted ingestions queued again");
    }
    watch::start(state.clone());

    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    runtime.block_on(async {
        let listener = tokio::net::TcpListener::bind(&bind).await.with_context(|| format!("binding {bind}"))?;
        tracing::info!("listening on http://{bind}");
        axum::serve(listener, api::router(state.clone()))
            .with_graceful_shutdown(shutdown_signal())
            .await?;
        anyhow::Ok(())
    })?;

    // Flush every WAL and close the databases.
    state.projects.close_all();
    tracing::info!("bye");
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
    tracing::info!("shutting down");
}

#[cfg(test)]
mod repro_full {
    use super::*;

    /// Regression: with LadybugDB's full-text index, the second ingestion creating mentions and
    /// entity relations lost its Document row at commit. Needs the models (/models) and the
    /// LadybugDB extensions: `cargo test -- --ignored` inside the dev container.
    #[test]
    #[ignore]
    fn full_ingest_keeps_document() {
        let mut config = Config::from_env();
        config.data_dir = std::env::temp_dir().join(format!("innerrag-full-{}", uuid::Uuid::new_v4()));
        config.models_dir = "/models".into();
        config.threads = 4;
        let state = Arc::new(load_state(config).unwrap());
        let graph = state.projects.get("default").unwrap();
        let count = || {
            db::rows(&graph.reader().unwrap(), "MATCH (d:Document) RETURN collect(d.id)", vec![]).unwrap()[0][0].to_string()
        };
        for (id, text) in [("a", "Grace Hopper travaille à Arlington."), ("b", "Ada Lovelace vit à Londres."), ("c", "bonjour.")] {
            let req = ingest::IngestRequest {
                id: Some(id.into()),
                title: id.into(),
                text: text.into(),
                source: None,
                metadata: None,
                tags: None,
                status: None,
                creator: None,
                labels: None,
                plain: false,
                original: None,
            };
            ingest::ingest(&state, &graph, req, ingest::Mode::Create, None).unwrap();
        }
        assert_eq!(count(), "[a,b,c]");
        // The keyword index follows the base.
        assert_eq!(graph.search_keywords("Lovelace", 3)[0].0, "b#0");
    }
}
