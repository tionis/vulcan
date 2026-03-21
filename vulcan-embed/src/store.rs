use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredModel {
    pub cache_key: String,
    pub provider_name: String,
    pub model_name: String,
    pub dimensions: usize,
    pub normalized: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredVector {
    pub chunk_id: String,
    pub provider_name: String,
    pub model_name: String,
    pub dimensions: usize,
    pub normalized: bool,
    pub content_hash: String,
    pub embedding: Vec<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VectorQuery {
    pub embedding: Vec<f32>,
    pub limit: usize,
    pub provider_name: String,
    pub model_name: String,
    pub dimensions: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VectorSearchResult {
    pub chunk_id: String,
    pub distance: f32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredModelInfo {
    pub cache_key: String,
    pub table_name: String,
    pub provider_name: String,
    pub model_name: String,
    pub dimensions: usize,
    pub normalized: bool,
    pub chunk_count: usize,
    pub is_active: bool,
}

pub trait VectorStore {
    fn current_model(&self) -> Result<Option<StoredModel>, String>;

    fn replace_model(&mut self, model: &StoredModel) -> Result<(), String>;

    fn load_vectors(&self) -> Result<Vec<StoredVector>, String>;

    fn load_hashes(&self) -> Result<HashMap<String, String>, String>;

    fn upsert(&mut self, vectors: &[StoredVector]) -> Result<(), String>;

    fn delete_chunks(&mut self, chunk_ids: &[String]) -> Result<(), String>;

    fn query(&self, query: &VectorQuery) -> Result<Vec<VectorSearchResult>, String>;

    fn list_models(&self) -> Result<Vec<StoredModelInfo>, String>;

    fn drop_model(&mut self, cache_key: &str) -> Result<bool, String>;

    fn delete_chunks_all_models(&mut self, chunk_ids: &[String]) -> Result<(), String>;

    fn set_active_model(&mut self, cache_key: &str) -> Result<(), String>;
}
