//! Embedding-provider and vector-store abstractions for Vulcan.
//!
//! Most library consumers should not depend on this crate directly. Use
//! `vulcan-core` with the `vectors` feature when you want Vulcan's vector-backed
//! suggestions, related-note queries, duplicate detection, or vector cache
//! repair helpers. Depend on `vulcan-embed` directly only when implementing or
//! testing embedding providers and vector stores.
//!
//! The public surface is intentionally small:
//!
//! - [`EmbeddingProvider`] describes synchronous embedding generation.
//! - [`OpenAICompatibleProvider`] implements an OpenAI-compatible HTTP provider.
//! - [`VectorStore`] abstracts storage/search so `sqlite-vec` remains behind one
//!   boundary.
//! - [`SqliteVecStore`] is the bundled local vector-store implementation.

mod openai_compat;
mod provider;
mod sqlite_vec;
mod store;

pub use openai_compat::{OpenAICompatibleConfig, OpenAICompatibleProvider};
pub use provider::{
    EmbeddingError, EmbeddingInput, EmbeddingProvider, EmbeddingResult, ModelMetadata,
};
pub use sqlite_vec::{register_sqlite_vec_extension, SqliteVecStore};
pub use store::{
    StoredModel, StoredModelInfo, StoredVector, VectorQuery, VectorSearchResult, VectorStore,
};
