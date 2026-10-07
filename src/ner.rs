//! Zero-shot named entity recognition with a GLiNER ONNX model (span mode).

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use anyhow::{anyhow, Result};
use gliner::model::input::text::TextInput;
use gliner::model::params::Parameters;
use gliner::model::pipeline::span::SpanMode;
use gliner::model::GLiNER;
use ort::execution_providers::CUDAExecutionProvider;
use orp::params::RuntimeParameters;

const BATCH: usize = 8;

/// One entity found in one text, deduplicated by entity id.
#[derive(Debug, Clone)]
pub struct Mention {
    pub entity_id: String,
    pub name: String,
    pub label: String,
    pub score: f32,
}

pub struct Ner {
    model: Mutex<GLiNER<SpanMode>>,
    stopwords: std::collections::HashSet<String>,
}

/// Words GLiNER tends to tag although they name nothing: pronouns, generic roles and objects.
const STOPWORDS: &[&str] = &[
    // English
    "i", "me", "you", "he", "she", "it", "we", "they", "them", "us", "your", "our", "their", "this", "that",
    "user", "users", "app", "apps", "application", "applications", "developer", "developers", "people",
    "customer", "customers", "client", "clients", "team", "company", "system", "systems", "file", "files",
    "data", "code", "example", "examples", "device", "devices", "server", "computer", "world", "home",
    // French
    "je", "moi", "tu", "toi", "il", "elle", "on", "nous", "vous", "ils", "elles", "eux", "ce", "cela", "ça",
    "utilisateur", "utilisateurs", "utilisatrice", "développeur", "développeurs", "équipe", "équipes",
    "entreprise", "société", "salarié", "salariés", "personne", "personnes", "système", "fichier", "fichiers",
    "données", "exemple", "serveur", "ordinateur", "monde",
];

/// A single lowercase word without digits or inner capitals ("device", "contacts") is a common
/// noun, not a named entity; "onCreate", "iOS", "ext4" or "AndroidManifest" are kept.
fn is_noise(name: &str, stopwords: &std::collections::HashSet<String>) -> bool {
    let lower = name.to_lowercase();
    if stopwords.contains(&lower) {
        return true;
    }
    let single_word = !name.contains(char::is_whitespace);
    single_word && name.chars().all(|c| !c.is_uppercase() && !c.is_ascii_digit())
}

impl Ner {
    /// `cuda`: run on an NVIDIA GPU, failing when it is not usable.
    pub fn load(dir: &Path, threads: usize, threshold: f32, cuda: bool, extra_stopwords: &[String]) -> Result<Self> {
        let stopwords = STOPWORDS
            .iter()
            .map(|s| (*s).to_string())
            .chain(extra_stopwords.iter().map(|s| s.trim().to_lowercase()))
            .filter(|s| !s.is_empty())
            .collect();
        let params = Parameters::default().with_threshold(threshold).with_max_length(Some(512));
        let mut runtime = RuntimeParameters::default().with_threads(threads);
        if cuda {
            runtime = runtime.with_execution_providers([CUDAExecutionProvider::default().build().error_on_failure()]);
        }
        let model = GLiNER::<SpanMode>::new(
            params,
            runtime,
            dir.join("tokenizer.json"),
            dir.join("model.onnx"),
        )
        .map_err(|e| anyhow!("loading NER model: {e}"))?;
        Ok(Self { model: Mutex::new(model), stopwords })
    }

    /// True for names that are not real entities (stopwords, lowercase common nouns).
    pub fn is_noise(&self, name: &str) -> bool {
        is_noise(name, &self.stopwords)
    }

    /// Returns, for each text, its distinct entity mentions.
    pub fn extract(&self, texts: &[String], labels: &[String]) -> Result<Vec<Vec<Mention>>> {
        let model = self.model.lock().map_err(|_| anyhow!("NER model lock poisoned"))?;
        self.run(&model, texts, labels)
    }

    /// For search questions: waits at most `wait` for the model (busy with an ingestion), then
    /// gives up and returns no entity, so a search is never stuck behind a long document.
    pub fn extract_quick(&self, text: &str, labels: &[String], wait: std::time::Duration) -> Vec<Mention> {
        let started = std::time::Instant::now();
        loop {
            match self.model.try_lock() {
                Ok(model) => {
                    return self.run(&model, &[text.to_string()], labels).ok().and_then(|mut v| v.pop()).unwrap_or_default();
                }
                Err(std::sync::TryLockError::WouldBlock) if started.elapsed() < wait => {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                Err(_) => {
                    tracing::debug!("NER busy: searching without question entities");
                    return Vec::new();
                }
            }
        }
    }

    fn run(&self, model: &GLiNER<SpanMode>, texts: &[String], labels: &[String]) -> Result<Vec<Vec<Mention>>> {
        if labels.is_empty() {
            return Ok(vec![Vec::new(); texts.len()]);
        }
        // Empty texts (passages made only of code) are not sent to the model.
        let wanted: Vec<usize> = (0..texts.len()).filter(|&i| !texts[i].trim().is_empty()).collect();
        if wanted.len() < texts.len() {
            let kept: Vec<String> = wanted.iter().map(|&i| texts[i].clone()).collect();
            let mut found = self.run(model, &kept, labels)?.into_iter();
            let mut out = vec![Vec::new(); texts.len()];
            for &i in &wanted {
                out[i] = found.next().unwrap_or_default();
            }
            return Ok(out);
        }
        let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
        let mut out = Vec::with_capacity(texts.len());
        for batch in texts.chunks(BATCH) {
            let refs: Vec<&str> = batch.iter().map(String::as_str).collect();
            let input = TextInput::from_str(&refs, &labels).map_err(|e| anyhow!("NER input: {e}"))?;
            let output = model.inference(input).map_err(|e| anyhow!("NER inference: {e}"))?;
            let mut per_text: Vec<HashMap<String, Mention>> = vec![HashMap::new(); batch.len()];
            for span in output.spans.iter().flatten() {
                let Some(name) = clean_name(span.text()) else { continue };
                if is_noise(&name, &self.stopwords) {
                    continue;
                }
                let label = span.class().to_string();
                let entity_id = entity_id(&label, &name);
                let slot = &mut per_text[span.sequence()];
                let score = span.probability();
                slot.entry(entity_id.clone())
                    .and_modify(|m| m.score = m.score.max(score))
                    .or_insert(Mention { entity_id, name, label, score });
            }
            out.extend(per_text.into_iter().map(|m| m.into_values().collect()));
        }
        Ok(out)
    }
}

/// Trims whitespace and edge punctuation; rejects names that are too short to be useful.
pub fn clean_name(raw: &str) -> Option<String> {
    let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut trimmed = collapsed.trim_matches(|c: char| !c.is_alphanumeric()).to_string();
    if trimmed.contains('(') && !trimmed.contains(')') {
        trimmed.push(')');
    }
    (trimmed.chars().filter(|c| c.is_alphanumeric()).count() >= 2).then_some(trimmed)
}

/// Stable id: `label:lowercased name`.
pub fn entity_id(label: &str, name: &str) -> String {
    format!("{}:{}", label.to_lowercase(), name.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleans_names() {
        assert_eq!(clean_name("  « Paris »,").as_deref(), Some("Paris"));
        assert_eq!(clean_name("Jean\n  Dupont").as_deref(), Some("Jean Dupont"));
        assert_eq!(clean_name("BNP (France)").as_deref(), Some("BNP (France)"));
        assert_eq!(clean_name("-a-"), None);
    }

    #[test]
    fn noise_is_filtered() {
        let stop = STOPWORDS.iter().map(|s| (*s).to_string()).collect();
        for noisy in ["you", "Users", "device", "tablets", "vous"] {
            assert!(is_noise(noisy, &stop), "{noisy}");
        }
        for real in ["Android", "onCreate", "ext4", "Google Play Store", "Marie Curie", "iOS"] {
            assert!(!is_noise(real, &stop), "{real}");
        }
    }

    #[test]
    fn ids_are_case_insensitive() {
        assert_eq!(entity_id("Location", "PARIS"), entity_id("location", "Paris"));
    }
}
