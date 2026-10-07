//! Sentence embeddings with an E5-style ONNX encoder (mean pooling + L2 normalisation).

use std::path::Path;

use anyhow::{anyhow, Context, Result};
use ndarray::Array2;
use ort::execution_providers::CUDAExecutionProvider;
use ort::session::{builder::GraphOptimizationLevel, Session};
use tokenizers::{PaddingParams, Tokenizer, TruncationParams};

const BATCH: usize = 16;
const MAX_TOKENS: usize = 512;

pub struct Embedder {
    session: Session,
    tokenizer: Tokenizer,
    output_name: String,
    with_token_types: bool,
    dim: usize,
}

impl Embedder {
    /// `cuda`: run on an NVIDIA GPU, failing (instead of silently using the CPU) when it is not usable.
    pub fn load(dir: &Path, threads: usize, cuda: bool) -> Result<Self> {
        let mut tokenizer = Tokenizer::from_file(dir.join("tokenizer.json"))
            .map_err(|e| anyhow!("loading embedding tokenizer: {e}"))?;
        let (pad_token, pad_id) = ["<pad>", "[PAD]"]
            .iter()
            .find_map(|t| tokenizer.token_to_id(t).map(|id| (t.to_string(), id)))
            .unwrap_or(("<pad>".into(), 0));
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
        let session = builder.commit_from_file(dir.join("model.onnx")).context("loading embedding model")?;
        let with_token_types = session.inputs.iter().any(|i| i.name == "token_type_ids");
        let output_name = session
            .outputs
            .iter()
            .map(|o| o.name.clone())
            .find(|n| n == "last_hidden_state")
            .or_else(|| session.outputs.first().map(|o| o.name.clone()))
            .ok_or_else(|| anyhow!("embedding model has no output"))?;

        let mut embedder = Self { session, tokenizer, output_name, with_token_types, dim: 0 };
        embedder.dim = embedder.run(&["query: probe".to_string()])?[0].len();
        Ok(embedder)
    }

    pub fn dim(&self) -> usize {
        self.dim
    }

    /// Embeds a search query (E5 `query:` prefix). Also used for entity names.
    pub fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        Ok(self.run(&[format!("query: {text}")])?.remove(0))
    }

    pub fn embed_queries(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.embed_prefixed("query: ", texts)
    }

    /// Embeds document passages (E5 `passage:` prefix).
    pub fn embed_passages(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.embed_prefixed("passage: ", texts)
    }

    fn embed_prefixed(&self, prefix: &str, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let mut out = Vec::with_capacity(texts.len());
        for batch in texts.chunks(BATCH) {
            let prefixed: Vec<String> = batch.iter().map(|t| format!("{prefix}{t}")).collect();
            out.extend(self.run(&prefixed)?);
        }
        Ok(out)
    }

    fn run(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let encodings = self
            .tokenizer
            .encode_batch(texts.iter().map(String::as_str).collect::<Vec<_>>(), true)
            .map_err(|e| anyhow!("tokenizing: {e}"))?;
        let rows = encodings.len();
        let cols = encodings.iter().map(|e| e.get_ids().len()).max().unwrap_or(0);
        let mut ids = Array2::<i64>::zeros((rows, cols));
        let mut mask = Array2::<i64>::zeros((rows, cols));
        let mut types = Array2::<i64>::zeros((rows, cols));
        for (r, enc) in encodings.iter().enumerate() {
            for (c, ((&id, &m), &t)) in enc
                .get_ids()
                .iter()
                .zip(enc.get_attention_mask())
                .zip(enc.get_type_ids())
                .enumerate()
            {
                ids[[r, c]] = i64::from(id);
                mask[[r, c]] = i64::from(m);
                types[[r, c]] = i64::from(t);
            }
        }

        let outputs = if self.with_token_types {
            self.session.run(ort::inputs! {
                "input_ids" => ids,
                "attention_mask" => mask.clone(),
                "token_type_ids" => types,
            }?)?
        } else {
            self.session.run(ort::inputs! {
                "input_ids" => ids,
                "attention_mask" => mask.clone(),
            }?)?
        };
        let hidden = outputs[self.output_name.as_str()].try_extract_tensor::<f32>()?;
        let shape = hidden.shape().to_vec();
        if shape.len() != 3 {
            return Err(anyhow!("unexpected embedding output shape {shape:?}"));
        }
        let dim = shape[2];

        let mut result = Vec::with_capacity(rows);
        for r in 0..rows {
            let mut pooled = vec![0f32; dim];
            let mut count = 0f32;
            for c in 0..cols {
                if mask[[r, c]] == 0 {
                    continue;
                }
                count += 1.0;
                for (d, p) in pooled.iter_mut().enumerate() {
                    *p += hidden[[r, c, d]];
                }
            }
            let count = count.max(1.0);
            pooled.iter_mut().for_each(|p| *p /= count);
            let norm = pooled.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-12);
            pooled.iter_mut().for_each(|p| *p /= norm);
            result.push(pooled);
        }
        Ok(result)
    }
}
