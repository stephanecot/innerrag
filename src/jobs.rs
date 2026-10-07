//! Background ingestion jobs. Large files (a 700-page PDF is ~2 000 chunks) take minutes
//! on CPU, so uploads are queued and processed one at a time by a worker thread, while
//! clients poll the job for its stage and progress.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};

use serde::Serialize;

use crate::ingest::IngestReport;

const KEEP: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Stage {
    Queued,
    Extracting,
    Embedding,
    Entities,
    Writing,
    Done,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize)]
pub struct Job {
    pub id: String,
    pub project: String,
    /// File name or document title.
    pub filename: String,
    /// Id the document will have once ingested.
    pub document_id: String,
    pub stage: Stage,
    /// Progress within the current stage.
    pub done: usize,
    pub total: usize,
    pub created_at: i64,
    pub finished_at: Option<i64>,
    pub report: Option<IngestReport>,
    pub error: Option<String>,
}

type Work = Box<dyn FnOnce() + Send>;

pub struct Jobs {
    jobs: Mutex<(HashMap<String, Job>, VecDeque<String>)>,
    queue: Mutex<Sender<Work>>,
    cancelled: Mutex<HashSet<String>>,
}

/// Error returned by the ingestion when its job is cancelled.
#[derive(Debug)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ingestion cancelled")
    }
}

impl std::error::Error for Cancelled {}

fn now_ms() -> i64 {
    (time::OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as i64
}

impl Jobs {
    pub fn start() -> Arc<Self> {
        let (tx, rx) = channel::<Work>();
        std::thread::Builder::new()
            .name("ingestion".into())
            .spawn(move || {
                for work in rx {
                    work();
                }
            })
            .expect("spawning the ingestion worker");
        Arc::new(Self {
            jobs: Mutex::new((HashMap::new(), VecDeque::new())),
            queue: Mutex::new(tx),
            cancelled: Mutex::new(HashSet::new()),
        })
    }

    /// Registers a job and queues `work`, which receives a progress handle.
    pub fn submit(
        self: &Arc<Self>,
        project: &str,
        filename: &str,
        document_id: &str,
        work: impl FnOnce(&Progress) -> anyhow::Result<IngestReport> + Send + 'static,
    ) -> Job {
        let job = Job {
            id: uuid::Uuid::new_v4().to_string(),
            project: project.to_string(),
            filename: filename.to_string(),
            document_id: document_id.to_string(),
            stage: Stage::Queued,
            done: 0,
            total: 0,
            created_at: now_ms(),
            finished_at: None,
            report: None,
            error: None,
        };
        if let Ok(mut guard) = self.jobs.lock() {
            let (map, order) = &mut *guard;
            map.insert(job.id.clone(), job.clone());
            order.push_back(job.id.clone());
            while order.len() > KEEP {
                // Never forget a job that is still queued or running.
                let Some(oldest) = order.front().cloned() else { break };
                if map.get(&oldest).is_some_and(|j| j.finished_at.is_none()) {
                    break;
                }
                order.pop_front();
                map.remove(&oldest);
            }
        }
        let progress = Progress { jobs: self.clone(), id: job.id.clone() };
        let task: Work = Box::new(move || {
            // A job cancelled while queued never starts.
            let outcome = if progress.is_cancelled() { Err(Cancelled.into()) } else { work(&progress) };
            progress.finish(outcome);
        });
        if let Ok(tx) = self.queue.lock() {
            if tx.send(task).is_err() {
                tracing::error!("ingestion worker is gone");
            }
        }
        job
    }

    /// Asks a queued or running job to stop; it ends at its next progress step.
    /// Returns false when the job is unknown or already finished.
    pub fn cancel(&self, id: &str) -> bool {
        if self.get(id).is_none_or(|j| j.finished_at.is_some()) {
            return false;
        }
        if let Ok(mut set) = self.cancelled.lock() {
            set.insert(id.to_string());
        }
        true
    }

    pub fn get(&self, id: &str) -> Option<Job> {
        self.jobs.lock().ok()?.0.get(id).cloned()
    }

    /// Most recent first.
    pub fn list(&self, project: &str) -> Vec<Job> {
        let Ok(guard) = self.jobs.lock() else { return Vec::new() };
        let (map, order) = &*guard;
        order
            .iter()
            .rev()
            .filter_map(|id| map.get(id))
            .filter(|j| project.is_empty() || j.project == project)
            .cloned()
            .collect()
    }

    fn update(&self, id: &str, f: impl FnOnce(&mut Job)) {
        if let Ok(mut guard) = self.jobs.lock() {
            if let Some(job) = guard.0.get_mut(id) {
                f(job);
            }
        }
    }
}

/// Handle given to the ingestion code to report its progress.
pub struct Progress {
    jobs: Arc<Jobs>,
    id: String,
}

impl Progress {
    pub fn is_cancelled(&self) -> bool {
        self.jobs.cancelled.lock().is_ok_and(|s| s.contains(&self.id))
    }

    pub fn set(&self, stage: Stage, done: usize, total: usize) {
        self.jobs.update(&self.id, |j| {
            j.stage = stage;
            j.done = done;
            j.total = total;
        });
    }

    fn finish(&self, outcome: anyhow::Result<IngestReport>) {
        if let Ok(mut set) = self.jobs.cancelled.lock() {
            set.remove(&self.id);
        }
        self.jobs.update(&self.id, |j| {
            j.finished_at = Some(now_ms());
            match outcome {
                Ok(report) => {
                    j.stage = Stage::Done;
                    j.report = Some(report);
                }
                Err(e) if e.downcast_ref::<Cancelled>().is_some() => j.stage = Stage::Cancelled,
                Err(e) => {
                    j.stage = Stage::Failed;
                    j.error = Some(format!("{e:#}"));
                }
            }
        });
    }
}
