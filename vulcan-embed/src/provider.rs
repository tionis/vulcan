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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbeddingError {
    pub message: String,
    pub retryable: bool,
    pub status_code: Option<u16>,
}

impl EmbeddingError {
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: false,
            status_code: None,
        }
    }

    #[must_use]
    pub fn retryable(message: impl Into<String>, status_code: Option<u16>) -> Self {
        Self {
            message: message.into(),
            retryable: true,
            status_code,
        }
    }
}

pub type EmbeddingResult = Result<Vec<f32>, EmbeddingError>;

pub trait EmbeddingProvider: Send + Sync {
    fn metadata(&self) -> ModelMetadata;

    fn embed_batch(&self, inputs: &[EmbeddingInput]) -> Vec<EmbeddingResult>;
}
