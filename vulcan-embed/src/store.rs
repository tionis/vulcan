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

    /// Return `(pending, stale)` chunk IDs by comparing `current` against the stored index.
    ///
    /// - `pending`: chunk IDs whose hash does not match what is stored (or which are absent).
    /// - `stale`: chunk IDs present in the store but absent from `current`.
    ///
    /// `current` is a slice of `(chunk_id, content_hash)` pairs representing the chunks that
    /// should be indexed.
    ///
    /// The default implementation falls back to `load_hashes()`.
    fn pending_and_stale_chunks(
        &self,
        current: &[(String, String)],
    ) -> Result<(Vec<String>, Vec<String>), String> {
        let stored = self.load_hashes()?;
        let current_set: HashMap<&str, &str> = current
            .iter()
            .map(|(id, hash)| (id.as_str(), hash.as_str()))
            .collect();
        let pending = current
            .iter()
            .filter(|(id, hash)| stored.get(id.as_str()).map(String::as_str) != Some(hash))
            .map(|(id, _)| id.clone())
            .collect();
        let stale = stored
            .keys()
            .filter(|id| !current_set.contains_key(id.as_str()))
            .cloned()
            .collect();
        Ok((pending, stale))
    }

    fn upsert(&mut self, vectors: &[StoredVector]) -> Result<(), String>;

    fn delete_chunks(&mut self, chunk_ids: &[String]) -> Result<(), String>;

    fn query(&self, query: &VectorQuery) -> Result<Vec<VectorSearchResult>, String>;

    fn list_models(&self) -> Result<Vec<StoredModelInfo>, String>;

    fn drop_model(&mut self, cache_key: &str) -> Result<bool, String>;

    fn delete_chunks_all_models(&mut self, chunk_ids: &[String]) -> Result<(), String>;

    fn set_active_model(&mut self, cache_key: &str) -> Result<(), String>;
}
