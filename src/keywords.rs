//! Keyword search (BM25) over passages, kept in memory per project.
//!
//! LadybugDB's own full-text extension loses Document rows at commit when its index is present
//! and a transaction creates mentions and entity relations (reproduced in
//! `repro_full::full_ingest_keeps_document`), so the index lives here instead: built from the
//! passages when a project opens, updated after each ingestion or deletion.

use std::collections::{HashMap, HashSet};

use rust_stemmers::{Algorithm, Stemmer};

const K1: f64 = 1.2;
const B: f64 = 0.75;

/// Words too common to carry meaning (English and French).
const STOPWORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "by", "for", "from", "has", "have", "how", "i", "in", "is", "it",
    "of", "on", "or", "that", "the", "this", "to", "was", "what", "when", "where", "which", "who", "why", "will",
    "with", "you", "do", "does", "can", "au", "aux", "avec", "ce", "ces", "comment", "dans", "de", "des", "du",
    "elle", "en", "est", "et", "il", "je", "la", "le", "les", "leur", "lui", "mais", "ne", "nous", "on", "ou",
    "où", "par", "pas", "pour", "qu", "que", "quel", "quelle", "qui", "quoi", "sa", "se", "ses", "son", "sont",
    "sur", "ta", "te", "un", "une", "vous", "y", "l", "d", "c", "s", "n", "j", "m", "t",
];

pub struct KeywordIndex {
    english: Stemmer,
    french: Stemmer,
    stopwords: HashSet<&'static str>,
    /// term → (passage → term frequency)
    postings: HashMap<String, HashMap<String, u32>>,
    /// passage → (length in terms, its distinct terms)
    passages: HashMap<String, (u32, Vec<String>)>,
    total_len: u64,
}

impl Default for KeywordIndex {
    fn default() -> Self {
        Self {
            english: Stemmer::create(Algorithm::English),
            french: Stemmer::create(Algorithm::French),
            stopwords: STOPWORDS.iter().copied().collect(),
            postings: HashMap::new(),
            passages: HashMap::new(),
            total_len: 0,
        }
    }
}

impl KeywordIndex {
    /// Lowercased words; identifiers keep `_` and digits ("input_type", "ext4"). Each word gives
    /// its English and French stems, so either language matches its inflections.
    fn terms(&self, text: &str) -> Vec<String> {
        let mut out = Vec::new();
        for word in text.split(|c: char| !(c.is_alphanumeric() || c == '_')).filter(|w| !w.is_empty()) {
            let lower = word.to_lowercase();
            if self.stopwords.contains(lower.as_str()) || lower.chars().count() < 2 {
                continue;
            }
            let en = self.english.stem(&lower).into_owned();
            let fr = self.french.stem(&lower).into_owned();
            if fr != en {
                out.push(fr);
            }
            out.push(en);
        }
        out
    }

    pub fn len(&self) -> usize {
        self.passages.len()
    }

    pub fn insert(&mut self, id: &str, text: &str) {
        self.remove(id);
        let terms = self.terms(text);
        let mut tf: HashMap<String, u32> = HashMap::new();
        for t in &terms {
            *tf.entry(t.clone()).or_default() += 1;
        }
        let len = terms.len() as u32;
        for (term, n) in &tf {
            self.postings.entry(term.clone()).or_default().insert(id.to_string(), *n);
        }
        self.total_len += u64::from(len);
        self.passages.insert(id.to_string(), (len, tf.into_keys().collect()));
    }

    pub fn remove(&mut self, id: &str) {
        let Some((len, terms)) = self.passages.remove(id) else { return };
        self.total_len -= u64::from(len);
        for term in terms {
            if let Some(p) = self.postings.get_mut(&term) {
                p.remove(id);
                if p.is_empty() {
                    self.postings.remove(&term);
                }
            }
        }
    }

    /// Removes every passage of a document (ids are `<doc id>#<n>`).
    pub fn remove_document(&mut self, doc_id: &str) {
        let prefix = format!("{doc_id}#");
        let ids: Vec<String> = self
            .passages
            .keys()
            .filter(|id| id.strip_prefix(&prefix).is_some_and(|n| n.chars().all(|c| c.is_ascii_digit())))
            .cloned()
            .collect();
        for id in ids {
            self.remove(&id);
        }
    }

    /// BM25 scores of the best `top` passages for the query.
    pub fn search(&self, query: &str, top: usize) -> Vec<(String, f64)> {
        let n = self.passages.len() as f64;
        if n == 0.0 {
            return Vec::new();
        }
        let avg = self.total_len as f64 / n;
        let mut terms = self.terms(query);
        terms.sort();
        terms.dedup();
        let mut scores: HashMap<&str, f64> = HashMap::new();
        for term in &terms {
            let Some(posting) = self.postings.get(term) else { continue };
            let df = posting.len() as f64;
            let idf = ((n - df + 0.5) / (df + 0.5) + 1.0).ln();
            for (id, tf) in posting {
                let len = self.passages.get(id).map_or(avg, |p| f64::from(p.0));
                let tf = f64::from(*tf);
                *scores.entry(id.as_str()).or_default() += idf * tf * (K1 + 1.0) / (tf + K1 * (1.0 - B + B * len / avg));
            }
        }
        let mut ranked: Vec<(String, f64)> = scores.into_iter().map(|(id, s)| (id.to_string(), s)).collect();
        ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
        ranked.truncate(top);
        ranked
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bm25_with_french_and_english_stems() {
        let mut idx = KeywordIndex::default();
        idx.insert("doc#0", "The EditText control has an attribute inputType for validation.");
        idx.insert("doc#1", "Les congés payés sont validés par Claire Martin.");
        idx.insert("other#0", "Nothing relevant here at all.");
        assert_eq!(idx.search("inputType attribute", 5)[0].0, "doc#0");
        assert_eq!(idx.search("congé validé", 5)[0].0, "doc#1");
        assert!(idx.search("tarte tatin", 5).is_empty());
        idx.remove_document("doc");
        assert_eq!(idx.len(), 1);
        assert!(idx.search("inputType", 5).is_empty());
    }
}
