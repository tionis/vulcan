use serde::{Deserialize, Serialize};
use ulid::Ulid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelMetadata {
    pub provider_name: String,
    pub model_name: String,
    pub dimensions: usize,
    pub normalized: bool,
    pub max_batch_size: usize,
    pub max_input_tokens: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbeddingInput {
    pub id: Ulid,
    pub text: String,
}

pub type EmbeddingResult = Result<Vec<f32>, String>;

pub trait EmbeddingProvider {
    fn metadata(&self) -> &ModelMetadata;

    fn embed_batch(&self, inputs: &[EmbeddingInput]) -> Vec<EmbeddingResult>;
}
