//! Cross-encoder reranking: reads the question and each candidate passage together and scores
//! their relevance, more precisely than comparing two separate embeddings.

use std::path::Path;

use anyhow::{anyhow, Context, Result};
use ndarray::Array2;
use ort::execution_providers::CUDAExecutionProvider;
use ort::session::{builder::GraphOptimizationLevel, Session};
use tokenizers::{PaddingParams, Tokenizer, TruncationParams};

const MAX_TOKENS: usize = 384;
const BATCH: usize = 8;

pub struct Reranker {
    session: Session,
    tokenizer: Tokenizer,
    output_name: String,
    with_token_types: bool,
}

impl Reranker {
    pub fn load(dir: &Path, threads: usize, cuda: bool) -> Result<Self> {
        let mut tokenizer = Tokenizer::from_file(dir.join("tokenizer.json"))
            .map_err(|e| anyhow!("loading reranker tokenizer: {e}"))?;
        let (pad_token, pad_id) = ["<pad>", "[PAD]"]
            .iter()
            .find_map(|t| tokenizer.token_to_id(t).map(|id| (t.to_string(), id)))
            .unwrap_or(("<pad>".into(), 1));
        tokenizer.with_padding(Some(PaddingParams { pad_id, pad_token, ..Default::default() }));
        tokenizer
            .with_truncation(Some(TruncationParams { max_length: MAX_TOKENS, ..Default::default() }))
            .map_err(|e| anyhow!("configuring truncation: {e}"))?;
        let mut builder = Session::builder()?
            .with_optimization_level(GraphOptimizationLevel::Level3)?
            .with_intra_threads(threads)?;
        if cuda {
            builder = builder.with_execution_providers([CUDAExecutionProvider::default().build().error_on_failure()])?;
        }
        let session = builder.commit_from_file(dir.join("model.onnx")).context("loading reranker model")?;
        let with_token_types = session.inputs.iter().any(|i| i.name == "token_type_ids");
        let output_name = session
            .outputs
            .first()
            .map(|o| o.name.clone())
            .ok_or_else(|| anyhow!("reranker model has no output"))?;
        Ok(Self { session, tokenizer, output_name, with_token_types })
    }

    /// Relevance of each passage to the question, between 0 and 1.
    pub fn score(&self, question: &str, passages: &[&str]) -> Result<Vec<f64>> {
        let mut scores = Vec::with_capacity(passages.len());
        for batch in passages.chunks(BATCH) {
            let pairs: Vec<(&str, &str)> = batch.iter().map(|p| (question, *p)).collect();
            let encodings = self.tokenizer.encode_batch(pairs, true).map_err(|e| anyhow!("tokenizing: {e}"))?;
            let rows = encodings.len();
            let cols = encodings.iter().map(|e| e.get_ids().len()).max().unwrap_or(0);
            let mut ids = Array2::<i64>::zeros((rows, cols));
            let mut mask = Array2::<i64>::zeros((rows, cols));
            let mut types = Array2::<i64>::zeros((rows, cols));
            for (r, enc) in encodings.iter().enumerate() {
                for (c, ((&id, &m), &t)) in enc.get_ids().iter().zip(enc.get_attention_mask()).zip(enc.get_type_ids()).enumerate() {
                    ids[[r, c]] = i64::from(id);
                    mask[[r, c]] = i64::from(m);
                    types[[r, c]] = i64::from(t);
                }
            }
            let outputs = if self.with_token_types {
                self.session.run(ort::inputs! { "input_ids" => ids, "attention_mask" => mask, "token_type_ids" => types }?)?
            } else {
                self.session.run(ort::inputs! { "input_ids" => ids, "attention_mask" => mask }?)?
            };
            let logits = outputs[self.output_name.as_str()].try_extract_tensor::<f32>()?;
            for r in 0..rows {
                let logit = f64::from(logits.as_slice().map_or(0.0, |s| s[r * (s.len() / rows)]));
                scores.push(1.0 / (1.0 + (-logit).exp()));
            }
        }
        Ok(scores)
    }
}
