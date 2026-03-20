mod provider;
mod store;

pub use provider::{EmbeddingInput, EmbeddingProvider, EmbeddingResult, ModelMetadata};
pub use store::{StoredVector, VectorQuery, VectorSearchResult, VectorStore};

#[cfg(test)]
mod tests {
    use super::*;
    use ulid::Ulid;

    struct FixedProvider {
        metadata: ModelMetadata,
    }

    impl EmbeddingProvider for FixedProvider {
        fn metadata(&self) -> &ModelMetadata {
            &self.metadata
        }

        fn embed_batch(&self, inputs: &[EmbeddingInput]) -> Vec<EmbeddingResult> {
            inputs.iter().map(|_| Ok(vec![1.0])).collect()
        }
    }

    struct InMemoryVectorStore {
        rows: Vec<StoredVector>,
    }

    impl VectorStore for InMemoryVectorStore {
        fn upsert(&mut self, vectors: &[StoredVector]) -> Result<(), String> {
            self.rows.extend_from_slice(vectors);
            Ok(())
        }

        fn query(&self, query: &VectorQuery) -> Result<Vec<VectorSearchResult>, String> {
            Ok(self
                .rows
                .iter()
                .take(query.limit)
                .map(|row| VectorSearchResult {
                    chunk_id: row.chunk_id,
                    score: row.embedding.iter().sum(),
                })
                .collect())
        }
    }

    #[test]
    fn provider_and_store_traits_support_simple_roundtrip() {
        let provider = FixedProvider {
            metadata: ModelMetadata {
                provider_name: "test".to_string(),
                model_name: "fixture".to_string(),
                dimensions: 1,
                normalized: false,
                max_batch_size: 32,
                max_input_tokens: 1024,
            },
        };
        let inputs = vec![EmbeddingInput {
            id: Ulid::new(),
            text: "hello world".to_string(),
        }];
        let embeddings = provider.embed_batch(&inputs);
        let embedding = embeddings[0].clone().expect("embedding should succeed");
        let chunk_id = Ulid::new();
        let mut store = InMemoryVectorStore { rows: Vec::new() };

        store
            .upsert(&[StoredVector {
                chunk_id,
                provider_name: provider.metadata().provider_name.clone(),
                model_name: provider.metadata().model_name.clone(),
                dimensions: 1,
                embedding,
            }])
            .expect("store insert should succeed");

        let results = store
            .query(&VectorQuery {
                embedding: vec![1.0],
                limit: 1,
                provider_name: "test".to_string(),
                model_name: "fixture".to_string(),
            })
            .expect("query should succeed");

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].chunk_id, chunk_id);
    }
}
