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
}

impl Ner {
    /// `cuda`: run on an NVIDIA GPU, failing when it is not usable.
    pub fn load(dir: &Path, threads: usize, threshold: f32, cuda: bool) -> Result<Self> {
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
        Ok(Self { model: Mutex::new(model) })
    }

    /// Returns, for each text, its distinct entity mentions.
    pub fn extract(&self, texts: &[String], labels: &[String]) -> Result<Vec<Vec<Mention>>> {
        if labels.is_empty() {
            return Ok(vec![Vec::new(); texts.len()]);
        }
        let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
        let model = self.model.lock().map_err(|_| anyhow!("NER model lock poisoned"))?;
        let mut out = Vec::with_capacity(texts.len());
        for batch in texts.chunks(BATCH) {
            let refs: Vec<&str> = batch.iter().map(String::as_str).collect();
            let input = TextInput::from_str(&refs, &labels).map_err(|e| anyhow!("NER input: {e}"))?;
            let output = model.inference(input).map_err(|e| anyhow!("NER inference: {e}"))?;
            let mut per_text: Vec<HashMap<String, Mention>> = vec![HashMap::new(); batch.len()];
            for span in output.spans.iter().flatten() {
                let Some(name) = clean_name(span.text()) else { continue };
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
    fn ids_are_case_insensitive() {
        assert_eq!(entity_id("Location", "PARIS"), entity_id("location", "Paris"));
    }
}
