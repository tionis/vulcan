use serde::{Deserialize, Serialize};
use ulid::Ulid;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredVector {
    pub chunk_id: Ulid,
    pub provider_name: String,
    pub model_name: String,
    pub dimensions: usize,
    pub embedding: Vec<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VectorQuery {
    pub embedding: Vec<f32>,
    pub limit: usize,
    pub provider_name: String,
    pub model_name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VectorSearchResult {
    pub chunk_id: Ulid,
    pub score: f32,
}

pub trait VectorStore {
    fn upsert(&mut self, vectors: &[StoredVector]) -> Result<(), String>;

    fn query(&self, query: &VectorQuery) -> Result<Vec<VectorSearchResult>, String>;
}
