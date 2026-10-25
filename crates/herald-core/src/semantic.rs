use std::path::{Path, PathBuf};

use fastembed::{
    InitOptionsUserDefined, Pooling, TextEmbedding, TokenizerFiles, UserDefinedEmbeddingModel,
};

use crate::config::Config;
use crate::error::{HeraldError, Result};
use crate::model::SearchResult;
use crate::store::{Store, EMBEDDING_DIM};
use crate::sync::EmbeddingJob;

pub const MODEL_FILE: &str = "model.onnx";
pub const TOKENIZER_FILES: [&str; 4] = [
    "tokenizer.json",
    "config.json",
    "special_tokens_map.json",
    "tokenizer_config.json",
];

pub struct SemanticService {
    enabled: bool,
    model_dir: PathBuf,
    tokenizer_dir: PathBuf,
    embedder: Option<TextEmbedding>,
    warned: bool,
}

impl SemanticService {
    pub fn new(config: &Config, model_dir: PathBuf, tokenizer_dir: PathBuf) -> Self {
        Self {
            enabled: config.semantic.enabled,
            model_dir,
            tokenizer_dir,
            embedder: None,
            warned: false,
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn model_dir(&self) -> &Path {
        &self.model_dir
    }

    pub fn tokenizer_dir(&self) -> &Path {
        &self.tokenizer_dir
    }

    pub fn model_files_present(&self) -> bool {
        file_is_present(&self.model_dir.join(MODEL_FILE))
            && TOKENIZER_FILES
                .iter()
                .all(|name| file_is_present(&self.tokenizer_dir.join(name)))
    }

    fn embedder(&mut self) -> Result<&mut TextEmbedding> {
        if !self.enabled {
            return Err(HeraldError::Model(
                "语义检索已在配置中关闭（semantic.enabled = false）".to_string(),
            ));
        }
        if self.embedder.is_none() {
            self.embedder = Some(load_model(&self.model_dir, &self.tokenizer_dir)?);
        }
        Ok(self.embedder.as_mut().expect("embedder was just loaded"))
    }

    pub fn embed(&mut self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let embedder = self.embedder()?;
        let embeddings = embedder
            .embed(texts, Some(8))
            .map_err(|err| HeraldError::Model(format!("生成 embedding 失败：{err}")))?;
        let mut result = Vec::with_capacity(embeddings.len());
        for embedding in embeddings {
            if embedding.len() != EMBEDDING_DIM {
                return Err(HeraldError::Model(format!(
                    "模型输出维度为 {}，期望 {EMBEDDING_DIM}",
                    embedding.len()
                )));
            }
            result.push(normalize(&embedding));
        }
        Ok(result)
    }

    pub fn embed_one(&mut self, text: &str) -> Result<Vec<f32>> {
        let mut vectors = self.embed(&[text.to_string()])?;
        vectors
            .pop()
            .ok_or_else(|| HeraldError::Model("模型返回空向量".to_string()))
    }

    /// Best-effort embedding: a missing/broken model must never break sync.
    pub fn apply_jobs(&mut self, store: &Store, jobs: &[EmbeddingJob]) -> usize {
        if !self.enabled || jobs.is_empty() {
            return 0;
        }
        if !store.vec_enabled() {
            self.warn_once("sqlite-vec 不可用，跳过 embedding 写入");
            return 0;
        }
        let jobs: Vec<&EmbeddingJob> = jobs
            .iter()
            .filter(|job| !job.text.trim().is_empty())
            .collect();
        let mut stored = 0usize;
        for chunk in jobs.chunks(16) {
            let texts: Vec<String> = chunk.iter().map(|job| job.text.clone()).collect();
            match self.embed(&texts) {
                Ok(vectors) => {
                    for (job, vector) in chunk.iter().zip(vectors.iter()) {
                        match store.upsert_embedding(job.message_id, vector) {
                            Ok(()) => stored += 1,
                            Err(err) => {
                                self.warn_once(&format!("写入 embedding 失败：{err}"));
                                return stored;
                            }
                        }
                    }
                }
                Err(err) => {
                    self.warn_once(&format!("{err}（可运行 herald models download）"));
                    return stored;
                }
            }
        }
        stored
    }

    pub fn search(
        &mut self,
        store: &Store,
        account_id: i64,
        query: &str,
        limit: usize,
    ) -> Result<SearchResult> {
        let vector = self.embed_one(query)?;
        store.semantic_search(account_id, &vector, limit)
    }

    fn warn_once(&mut self, message: &str) {
        if !self.warned {
            tracing::warn!("{message}");
            self.warned = true;
        }
    }
}

fn load_model(model_dir: &Path, tokenizer_dir: &Path) -> Result<TextEmbedding> {
    let model_path = model_dir.join(MODEL_FILE);
    if !file_is_present(&model_path) {
        return Err(HeraldError::Model(format!(
            "模型文件缺失：{}（运行 herald models download）",
            model_path.display()
        )));
    }
    for name in TOKENIZER_FILES {
        let path = tokenizer_dir.join(name);
        if !file_is_present(&path) {
            return Err(HeraldError::Model(format!(
                "Tokenizer 文件缺失：{}（运行 herald models download）",
                path.display()
            )));
        }
    }
    let onnx = std::fs::read(&model_path)?;
    let tokenizer_files = TokenizerFiles {
        tokenizer_file: std::fs::read(tokenizer_dir.join("tokenizer.json"))?,
        config_file: std::fs::read(tokenizer_dir.join("config.json"))?,
        special_tokens_map_file: std::fs::read(tokenizer_dir.join("special_tokens_map.json"))?,
        tokenizer_config_file: std::fs::read(tokenizer_dir.join("tokenizer_config.json"))?,
    };
    let model = UserDefinedEmbeddingModel::new(onnx, tokenizer_files).with_pooling(Pooling::Cls);
    let options = InitOptionsUserDefined::new().with_max_length(512);
    TextEmbedding::try_new_from_user_defined(model, options)
        .map_err(|err| HeraldError::Model(format!("加载本地 ONNX 模型失败：{err}")))
}

fn file_is_present(path: &Path) -> bool {
    path.metadata()
        .map(|meta| meta.is_file() && meta.len() > 0)
        .unwrap_or(false)
}

fn normalize(vector: &[f32]) -> Vec<f32> {
    let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
    if norm <= f32::EPSILON {
        return vector.to_vec();
    }
    vector.iter().map(|value| value / norm).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_vectors() {
        let vector = normalize(&[3.0, 4.0]);
        assert!((vector[0] - 0.6).abs() < 1e-6);
        assert!((vector[1] - 0.8).abs() < 1e-6);
    }

    #[test]
    fn missing_model_is_reported() {
        let temp = tempfile::tempdir().unwrap();
        let err = match load_model(temp.path(), temp.path()) {
            Err(err) => err,
            Ok(_) => panic!("expected a missing model error"),
        };
        assert!(matches!(err, HeraldError::Model(_)));
    }

    #[test]
    fn model_and_tokenizer_can_live_in_separate_directories() {
        let temp = tempfile::tempdir().unwrap();
        let model_dir = temp.path().join("model");
        let tokenizer_dir = temp.path().join("tokenizer");
        std::fs::create_dir_all(&model_dir).unwrap();
        std::fs::create_dir_all(&tokenizer_dir).unwrap();
        std::fs::write(model_dir.join(MODEL_FILE), b"x").unwrap();
        for name in TOKENIZER_FILES {
            std::fs::write(tokenizer_dir.join(name), b"x").unwrap();
        }

        let service =
            SemanticService::new(&Config::default(), model_dir.clone(), tokenizer_dir.clone());
        assert!(service.model_files_present());
        assert_eq!(service.model_dir(), model_dir);
        assert_eq!(service.tokenizer_dir(), tokenizer_dir);

        std::fs::remove_file(tokenizer_dir.join("tokenizer.json")).unwrap();
        assert!(!service.model_files_present());
    }
}
