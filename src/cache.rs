//! Per-project cache of recent searches.
//!
//! A search costs ~450 ms on CPU, ~80% of it in the cross-encoder, and agents often repeat one
//! (a retry, a map then a full read, the same question from several people). Entries are keyed
//! by the project version, bumped after every write, so a cached answer is never stale.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::search::SearchResponse;

const MAX_RESULTS: usize = 256;
const MAX_SCORES: usize = 20_000;

#[derive(Default)]
pub struct SearchCache {
    results: Mutex<Lru<Arc<SearchResponse>>>,
    /// Cross-encoder scores per (question, passage), valid for one version.
    scores: Mutex<(u64, HashMap<(String, String), f64>)>,
}

/// Small least-recently-used map: entries carry the tick of their last use.
struct Lru<V> {
    map: HashMap<String, (u64, V)>,
    tick: u64,
    cap: usize,
}

impl<V> Default for Lru<V> {
    fn default() -> Self {
        Self { map: HashMap::new(), tick: 0, cap: MAX_RESULTS }
    }
}

impl<V: Clone> Lru<V> {
    fn get(&mut self, key: &str) -> Option<V> {
        self.tick += 1;
        let tick = self.tick;
        self.map.get_mut(key).map(|(t, v)| {
            *t = tick;
            v.clone()
        })
    }

    fn put(&mut self, key: String, value: V) {
        self.tick += 1;
        if self.map.len() >= self.cap && !self.map.contains_key(&key) {
            if let Some(oldest) = self.map.iter().min_by_key(|(_, (t, _))| *t).map(|(k, _)| k.clone()) {
                self.map.remove(&oldest);
            }
        }
        self.map.insert(key, (self.tick, value));
    }
}

impl SearchCache {
    pub fn get(&self, key: &str) -> Option<Arc<SearchResponse>> {
        self.results.lock().ok()?.get(key)
    }

    pub fn put(&self, key: String, value: Arc<SearchResponse>) {
        if let Ok(mut lru) = self.results.lock() {
            lru.put(key, value);
        }
    }

    /// Known cross-encoder scores of these passages for this question (same version only).
    pub fn scores(&self, version: u64, query: &str, ids: &[String]) -> HashMap<String, f64> {
        let Ok(mut guard) = self.scores.lock() else { return HashMap::new() };
        if guard.0 != version {
            *guard = (version, HashMap::new());
        }
        ids.iter()
            .filter_map(|id| guard.1.get(&(query.to_string(), id.clone())).map(|s| (id.clone(), *s)))
            .collect()
    }

    pub fn put_scores(&self, version: u64, query: &str, scores: impl IntoIterator<Item = (String, f64)>) {
        let Ok(mut guard) = self.scores.lock() else { return };
        if guard.0 != version {
            *guard = (version, HashMap::new());
        }
        if guard.1.len() > MAX_SCORES {
            guard.1.clear();
        }
        for (id, s) in scores {
            guard.1.insert((query.to_string(), id), s);
        }
    }
}
