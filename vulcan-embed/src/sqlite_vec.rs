use crate::store::{StoredModel, StoredVector, VectorQuery, VectorSearchResult, VectorStore};
use rusqlite::ffi::{sqlite3, sqlite3_api_routines, sqlite3_auto_extension};
use rusqlite::{params, Connection, OptionalExtension};
use sqlite_vec::sqlite3_vec_init;
use std::collections::HashMap;
use std::os::raw::{c_char, c_int};
use std::sync::Once;

const VECTOR_TABLE: &str = "vectors";
const VECTOR_STATE_ID: i64 = 1;

static REGISTER_EXTENSION: Once = Once::new();

pub fn register_sqlite_vec_extension() {
    REGISTER_EXTENSION.call_once(|| unsafe {
        type SqliteExtensionInit = unsafe extern "C" fn(
            *mut sqlite3,
            *mut *mut c_char,
            *const sqlite3_api_routines,
        ) -> c_int;

        sqlite3_auto_extension(Some(std::mem::transmute::<
            unsafe extern "C" fn(),
            SqliteExtensionInit,
        >(sqlite3_vec_init)));
    });
}

#[derive(Debug)]
pub struct SqliteVecStore<'connection> {
    connection: &'connection Connection,
}

impl<'connection> SqliteVecStore<'connection> {
    pub fn new(connection: &'connection Connection) -> Result<Self, String> {
        register_sqlite_vec_extension();
        ensure_state_table(connection)?;

        Ok(Self { connection })
    }
}

impl VectorStore for SqliteVecStore<'_> {
    fn current_model(&self) -> Result<Option<StoredModel>, String> {
        self.connection
            .query_row(
                "
                SELECT provider_name, model_name, dimensions, normalized
                FROM vector_index_state
                WHERE id = ?1
                ",
                [VECTOR_STATE_ID],
                |row| {
                    Ok(StoredModel {
                        provider_name: row.get(0)?,
                        model_name: row.get(1)?,
                        dimensions: usize::try_from(row.get::<_, i64>(2)?).map_err(|error| {
                            rusqlite::Error::FromSqlConversionFailure(
                                2,
                                rusqlite::types::Type::Integer,
                                Box::new(error),
                            )
                        })?,
                        normalized: row.get::<_, i64>(3)? != 0,
                    })
                },
            )
            .optional()
            .map_err(|error| format!("failed to load vector index state: {error}"))
    }

    fn replace_model(&mut self, model: &StoredModel) -> Result<(), String> {
        if self.current_model()?.as_ref() == Some(model) && vector_table_exists(self.connection)? {
            return Ok(());
        }

        rebuild_model_index(self.connection, model)
    }

    fn load_hashes(&self) -> Result<HashMap<String, String>, String> {
        if !vector_table_exists(self.connection)? {
            return Ok(HashMap::new());
        }

        let mut statement = self
            .connection
            .prepare("SELECT chunk_id, content_hash FROM vectors")
            .map_err(|error| format!("failed to prepare vector hash query: {error}"))?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|error| format!("failed to query vector hashes: {error}"))?;

        rows.collect::<Result<HashMap<_, _>, _>>()
            .map_err(|error| format!("failed to collect vector hashes: {error}"))
    }

    fn load_vectors(&self) -> Result<Vec<StoredVector>, String> {
        if !vector_table_exists(self.connection)? {
            return Ok(Vec::new());
        }

        load_stored_vectors(self.connection)
    }

    fn upsert(&mut self, vectors: &[StoredVector]) -> Result<(), String> {
        if vectors.is_empty() {
            return Ok(());
        }

        let current_model = self
            .current_model()?
            .ok_or_else(|| "vector store has no active model".to_string())?;
        let transaction = self
            .connection
            .unchecked_transaction()
            .map_err(|error| format!("failed to start vector transaction: {error}"))?;
        let mut insert_statement = transaction
            .prepare(
                "
                INSERT OR REPLACE INTO vectors (
                    chunk_id,
                    provider_name,
                    model_name,
                    dimensions,
                    normalized,
                    content_hash,
                    embedding
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                ",
            )
            .map_err(|error| format!("failed to prepare vector insert statement: {error}"))?;

        for vector in vectors {
            validate_vector(vector, &current_model)?;
            insert_statement
                .execute(params![
                    &vector.chunk_id,
                    &vector.provider_name,
                    &vector.model_name,
                    i64::try_from(vector.dimensions)
                        .map_err(|error| format!("dimensions overflow i64: {error}"))?,
                    i64::from(vector.normalized),
                    &vector.content_hash,
                    vector_to_blob(&vector.embedding),
                ])
                .map_err(|error| format!("failed to insert vector row: {error}"))?;
        }

        drop(insert_statement);
        transaction
            .commit()
            .map_err(|error| format!("failed to commit vector transaction: {error}"))?;

        Ok(())
    }

    fn delete_chunks(&mut self, chunk_ids: &[String]) -> Result<(), String> {
        if chunk_ids.is_empty() || !vector_table_exists(self.connection)? {
            return Ok(());
        }

        let current_model = self
            .current_model()?
            .ok_or_else(|| "vector store has no active model".to_string())?;
        let skip_chunk_ids = chunk_ids
            .iter()
            .cloned()
            .collect::<std::collections::HashSet<_>>();
        let remaining_vectors = load_stored_vectors(self.connection)?
            .into_iter()
            .filter(|vector| !skip_chunk_ids.contains(&vector.chunk_id))
            .collect::<Vec<_>>();

        rebuild_model_index(self.connection, &current_model)?;
        self.upsert(&remaining_vectors)?;
        Ok(())
    }

    fn query(&self, query: &VectorQuery) -> Result<Vec<VectorSearchResult>, String> {
        let current_model = self
            .current_model()?
            .ok_or_else(|| "vector store has no active model".to_string())?;
        if !vector_table_exists(self.connection)? {
            return Ok(Vec::new());
        }

        if current_model.provider_name != query.provider_name
            || current_model.model_name != query.model_name
            || current_model.dimensions != query.dimensions
        {
            return Err("vector query model does not match the active vector index".to_string());
        }
        if query.embedding.len() != query.dimensions {
            return Err(format!(
                "query embedding dimensions {} do not match expected {}",
                query.embedding.len(),
                query.dimensions
            ));
        }

        let mut statement = self
            .connection
            .prepare(
                "
                SELECT chunk_id, distance
                FROM vectors
                WHERE embedding MATCH ?1
                  AND k = ?2
                ORDER BY distance ASC
                ",
            )
            .map_err(|error| format!("failed to prepare vector query: {error}"))?;
        let rows = statement
            .query_map(
                params![
                    vector_to_blob(&query.embedding),
                    i64::try_from(query.limit)
                        .map_err(|error| format!("query limit overflow i64: {error}"))?,
                ],
                |row| {
                    Ok(VectorSearchResult {
                        chunk_id: row.get(0)?,
                        distance: row.get(1)?,
                    })
                },
            )
            .map_err(|error| format!("failed to execute vector query: {error}"))?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("failed to collect vector results: {error}"))
    }
}

fn ensure_state_table(connection: &Connection) -> Result<(), String> {
    connection
        .execute_batch(
            "
            CREATE TABLE IF NOT EXISTS vector_index_state (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                provider_name TEXT NOT NULL,
                model_name TEXT NOT NULL,
                dimensions INTEGER NOT NULL,
                normalized INTEGER NOT NULL
            );
            ",
        )
        .map_err(|error| format!("failed to create vector state table: {error}"))
}

fn rebuild_model_index(connection: &Connection, model: &StoredModel) -> Result<(), String> {
    connection
        .execute_batch(&format!("DROP TABLE IF EXISTS {VECTOR_TABLE}"))
        .map_err(|error| format!("failed to drop vector table: {error}"))?;
    connection
        .execute_batch(&create_vector_table_sql(model))
        .map_err(|error| format!("failed to create vector table: {error}"))?;
    connection
        .execute(
            "
            INSERT INTO vector_index_state (
                id,
                provider_name,
                model_name,
                dimensions,
                normalized
            )
            VALUES (?1, ?2, ?3, ?4, ?5)
            ON CONFLICT(id) DO UPDATE SET
                provider_name = excluded.provider_name,
                model_name = excluded.model_name,
                dimensions = excluded.dimensions,
                normalized = excluded.normalized
            ",
            params![
                VECTOR_STATE_ID,
                &model.provider_name,
                &model.model_name,
                i64::try_from(model.dimensions)
                    .map_err(|error| format!("dimensions overflow i64: {error}"))?,
                i64::from(model.normalized),
            ],
        )
        .map_err(|error| format!("failed to persist vector index state: {error}"))?;

    Ok(())
}

fn vector_table_exists(connection: &Connection) -> Result<bool, String> {
    connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name = ?1",
            [VECTOR_TABLE],
            |row| row.get::<_, i64>(0),
        )
        .map(|count| count > 0)
        .map_err(|error| format!("failed to inspect vector table: {error}"))
}

fn load_stored_vectors(connection: &Connection) -> Result<Vec<StoredVector>, String> {
    let mut statement = connection
        .prepare(
            "
            SELECT
                chunk_id,
                provider_name,
                model_name,
                dimensions,
                normalized,
                content_hash,
                embedding
            FROM vectors
            ",
        )
        .map_err(|error| format!("failed to prepare stored vector query: {error}"))?;
    let rows = statement
        .query_map([], |row| {
            let dimensions = usize::try_from(row.get::<_, i64>(3)?).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    3,
                    rusqlite::types::Type::Integer,
                    Box::new(error),
                )
            })?;
            let embedding_blob = row.get::<_, Vec<u8>>(6)?;
            Ok(StoredVector {
                chunk_id: row.get(0)?,
                provider_name: row.get(1)?,
                model_name: row.get(2)?,
                dimensions,
                normalized: row.get::<_, i64>(4)? != 0,
                content_hash: row.get(5)?,
                embedding: blob_to_vector(&embedding_blob).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        6,
                        rusqlite::types::Type::Blob,
                        Box::new(std::io::Error::other(error)),
                    )
                })?,
            })
        })
        .map_err(|error| format!("failed to query stored vectors: {error}"))?;

    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("failed to collect stored vectors: {error}"))
}

fn create_vector_table_sql(model: &StoredModel) -> String {
    let distance_metric = if model.normalized { "cosine" } else { "l2" };
    format!(
        "
        CREATE VIRTUAL TABLE {VECTOR_TABLE} USING vec0(
            chunk_id text primary key,
            provider_name text,
            model_name text,
            dimensions integer,
            normalized integer,
            content_hash text,
            embedding float[{dimensions}] distance_metric={distance_metric}
        );
        ",
        dimensions = model.dimensions
    )
}

fn validate_vector(vector: &StoredVector, model: &StoredModel) -> Result<(), String> {
    if vector.provider_name != model.provider_name
        || vector.model_name != model.model_name
        || vector.dimensions != model.dimensions
        || vector.normalized != model.normalized
    {
        return Err("vector metadata does not match the active vector index".to_string());
    }
    if vector.embedding.len() != vector.dimensions {
        return Err(format!(
            "vector dimensions {} do not match embedding length {}",
            vector.dimensions,
            vector.embedding.len()
        ));
    }

    Ok(())
}

fn vector_to_blob(vector: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(std::mem::size_of_val(vector));
    for value in vector {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

fn blob_to_vector(bytes: &[u8]) -> Result<Vec<f32>, String> {
    if bytes.len() % std::mem::size_of::<f32>() != 0 {
        return Err(format!(
            "vector blob length {} is not divisible by {}",
            bytes.len(),
            std::mem::size_of::<f32>()
        ));
    }

    Ok(bytes
        .chunks_exact(std::mem::size_of::<f32>())
        .map(|chunk| {
            f32::from_le_bytes(
                chunk
                    .try_into()
                    .expect("chunks_exact should always yield 4-byte chunks"),
            )
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    #[test]
    fn sqlite_vec_store_roundtrips_vectors() {
        register_sqlite_vec_extension();
        let connection = Connection::open_in_memory().expect("in-memory db should open");
        let mut store = SqliteVecStore::new(&connection).expect("store should initialize");
        let model = StoredModel {
            provider_name: "openai-compatible".to_string(),
            model_name: "fixture".to_string(),
            dimensions: 2,
            normalized: true,
        };

        store.replace_model(&model).expect("model should be stored");
        store
            .upsert(&[
                StoredVector {
                    chunk_id: "chunk-1".to_string(),
                    provider_name: model.provider_name.clone(),
                    model_name: model.model_name.clone(),
                    dimensions: model.dimensions,
                    normalized: model.normalized,
                    content_hash: "hash-1".to_string(),
                    embedding: vec![1.0, 0.0],
                },
                StoredVector {
                    chunk_id: "chunk-2".to_string(),
                    provider_name: model.provider_name.clone(),
                    model_name: model.model_name.clone(),
                    dimensions: model.dimensions,
                    normalized: model.normalized,
                    content_hash: "hash-2".to_string(),
                    embedding: vec![0.0, 1.0],
                },
            ])
            .expect("vectors should insert");

        let hashes = store.load_hashes().expect("hashes should load");
        assert_eq!(hashes.get("chunk-1"), Some(&"hash-1".to_string()));

        let results = store
            .query(&VectorQuery {
                embedding: vec![1.0, 0.0],
                limit: 2,
                provider_name: model.provider_name.clone(),
                model_name: model.model_name.clone(),
                dimensions: model.dimensions,
            })
            .expect("query should succeed");
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].chunk_id, "chunk-1");
        assert!(results[0].distance <= results[1].distance);

        store
            .delete_chunks(&["chunk-1".to_string()])
            .expect("chunk delete should succeed");
        let remaining_hashes = store.load_hashes().expect("hashes should reload");
        assert!(!remaining_hashes.contains_key("chunk-1"));
    }

    #[test]
    fn replacing_model_recreates_the_underlying_index() {
        register_sqlite_vec_extension();
        let connection = Connection::open_in_memory().expect("in-memory db should open");
        let mut store = SqliteVecStore::new(&connection).expect("store should initialize");
        let first_model = StoredModel {
            provider_name: "openai-compatible".to_string(),
            model_name: "first".to_string(),
            dimensions: 2,
            normalized: true,
        };
        let second_model = StoredModel {
            provider_name: "openai-compatible".to_string(),
            model_name: "second".to_string(),
            dimensions: 3,
            normalized: true,
        };

        store
            .replace_model(&first_model)
            .expect("first model should initialize");
        store
            .upsert(&[StoredVector {
                chunk_id: "chunk-1".to_string(),
                provider_name: first_model.provider_name.clone(),
                model_name: first_model.model_name.clone(),
                dimensions: first_model.dimensions,
                normalized: first_model.normalized,
                content_hash: "hash-1".to_string(),
                embedding: vec![1.0, 0.0],
            }])
            .expect("first model vector should insert");
        store
            .replace_model(&second_model)
            .expect("second model should replace the index");

        assert_eq!(
            store.current_model().expect("model should load"),
            Some(second_model)
        );
        assert!(store.load_hashes().expect("hashes should load").is_empty());
    }
}
