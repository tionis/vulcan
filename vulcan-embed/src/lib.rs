mod openai_compat;
mod provider;
mod sqlite_vec;
mod store;

pub use openai_compat::{OpenAICompatibleConfig, OpenAICompatibleProvider};
pub use provider::{
    EmbeddingError, EmbeddingInput, EmbeddingProvider, EmbeddingResult, ModelMetadata,
};
pub use sqlite_vec::{register_sqlite_vec_extension, SqliteVecStore};
pub use store::{StoredModel, StoredVector, VectorQuery, VectorSearchResult, VectorStore};
