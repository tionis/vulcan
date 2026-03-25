use crate::store::{
    StoredModel, StoredModelInfo, StoredVector, VectorQuery, VectorSearchResult, VectorStore,
};
use rusqlite::ffi::{sqlite3, sqlite3_api_routines, sqlite3_auto_extension};
use rusqlite::{params, Connection, OptionalExtension};
use sqlite_vec::sqlite3_vec_init;
use std::collections::HashMap;
use std::os::raw::{c_char, c_int};
use std::sync::Once;

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
    active_cache_key: Option<String>,
    active_table: Option<String>,
}

impl<'connection> SqliteVecStore<'connection> {
    pub fn new(connection: &'connection Connection) -> Result<Self, String> {
        register_sqlite_vec_extension();
        ensure_registry_table(connection)?;
        migrate_legacy_state(connection)?;

        let (active_cache_key, active_table) = load_active_model_info(connection)?;

        Ok(Self {
            connection,
            active_cache_key,
            active_table,
        })
    }
}

impl VectorStore for SqliteVecStore<'_> {
    fn current_model(&self) -> Result<Option<StoredModel>, String> {
        let Some(cache_key) = &self.active_cache_key else {
            return Ok(None);
        };

        self.connection
            .query_row(
                "SELECT cache_key, provider_name, model_name, dimensions, normalized
                 FROM vector_model_registry
                 WHERE cache_key = ?1",
                [cache_key],
                |row| {
                    Ok(StoredModel {
                        cache_key: row.get(0)?,
                        provider_name: row.get(1)?,
                        model_name: row.get(2)?,
                        dimensions: usize::try_from(row.get::<_, i64>(3)?).map_err(|error| {
                            rusqlite::Error::FromSqlConversionFailure(
                                3,
                                rusqlite::types::Type::Integer,
                                Box::new(error),
                            )
                        })?,
                        normalized: row.get::<_, i64>(4)? != 0,
                    })
                },
            )
            .optional()
            .map_err(|error| format!("failed to load active model from registry: {error}"))
    }

    fn replace_model(&mut self, model: &StoredModel) -> Result<(), String> {
        let table_name = sanitized_table_name(&model.cache_key);

        // Check if this model already exists in the registry with the correct table.
        let existing: Option<(String, i64)> = self
            .connection
            .query_row(
                "SELECT table_name, dimensions FROM vector_model_registry WHERE cache_key = ?1",
                [&model.cache_key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|error| format!("failed to query registry: {error}"))?;

        let needs_recreate = match &existing {
            Some((existing_table, existing_dims)) => {
                let dims_i64 = i64::try_from(model.dimensions)
                    .map_err(|error| format!("dimensions overflow i64: {error}"))?;
                // Recreate if dimensions changed or table is missing.
                *existing_dims != dims_i64 || !table_exists(self.connection, existing_table)?
            }
            None => true,
        };

        if needs_recreate {
            // Drop old table for this cache_key if it exists.
            if let Some((old_table, _)) = &existing {
                self.connection
                    .execute_batch(&format!("DROP TABLE IF EXISTS [{old_table}]"))
                    .map_err(|error| format!("failed to drop old vector table: {error}"))?;
            }

            // Create the new vec0 table.
            self.connection
                .execute_batch(&create_vector_table_sql(&table_name, model))
                .map_err(|error| format!("failed to create vector table: {error}"))?;
        }

        // Upsert registry entry.
        self.connection
            .execute(
                "INSERT INTO vector_model_registry (
                    cache_key, table_name, provider_name, model_name,
                    dimensions, normalized, is_active, created_at
                 )
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, datetime('now'))
                 ON CONFLICT(cache_key) DO UPDATE SET
                    table_name = excluded.table_name,
                    provider_name = excluded.provider_name,
                    model_name = excluded.model_name,
                    dimensions = excluded.dimensions,
                    normalized = excluded.normalized",
                params![
                    &model.cache_key,
                    &table_name,
                    &model.provider_name,
                    &model.model_name,
                    i64::try_from(model.dimensions)
                        .map_err(|error| format!("dimensions overflow i64: {error}"))?,
                    i64::from(model.normalized),
                ],
            )
            .map_err(|error| format!("failed to upsert registry entry: {error}"))?;

        // Set this model as active.
        self.set_active_model(&model.cache_key)?;

        Ok(())
    }

    fn load_hashes(&self) -> Result<HashMap<String, String>, String> {
        let Some(table) = &self.active_table else {
            return Ok(HashMap::new());
        };
        if !table_exists(self.connection, table)? {
            return Ok(HashMap::new());
        }

        let mut statement = self
            .connection
            .prepare(&format!("SELECT chunk_id, content_hash FROM [{table}]"))
            .map_err(|error| format!("failed to prepare vector hash query: {error}"))?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|error| format!("failed to query vector hashes: {error}"))?;

        rows.collect::<Result<HashMap<_, _>, _>>()
            .map_err(|error| format!("failed to collect vector hashes: {error}"))
    }

    fn pending_and_stale_chunks(
        &self,
        current: &[(String, String)],
    ) -> Result<(Vec<String>, Vec<String>), String> {
        let Some(table) = &self.active_table else {
            // No active table: everything is pending, nothing is stale.
            let pending = current.iter().map(|(id, _)| id.clone()).collect();
            return Ok((pending, Vec::new()));
        };
        if !table_exists(self.connection, table)? {
            let pending = current.iter().map(|(id, _)| id.clone()).collect();
            return Ok((pending, Vec::new()));
        }

        // Use a uniquely named temp table to avoid conflicts.
        let temp_table = "_pending_check";

        self.connection
            .execute_batch(&format!(
                "CREATE TEMP TABLE IF NOT EXISTS [{temp_table}] (
                    chunk_id TEXT NOT NULL,
                    content_hash TEXT NOT NULL
                )"
            ))
            .map_err(|error| format!("failed to create temp table for hash comparison: {error}"))?;

        // Insert all current (chunk_id, content_hash) pairs.
        {
            let insert_sql =
                format!("INSERT INTO [{temp_table}] (chunk_id, content_hash) VALUES (?1, ?2)");
            let transaction = self
                .connection
                .unchecked_transaction()
                .map_err(|error| format!("failed to start temp insert transaction: {error}"))?;
            let mut stmt = transaction
                .prepare(&insert_sql)
                .map_err(|error| format!("failed to prepare temp insert: {error}"))?;
            for (chunk_id, content_hash) in current {
                stmt.execute(params![chunk_id, content_hash])
                    .map_err(|error| format!("failed to insert into temp table: {error}"))?;
            }
            drop(stmt);
            transaction
                .commit()
                .map_err(|error| format!("failed to commit temp inserts: {error}"))?;
        }

        // Query pending: current chunks whose hash doesn't match what is stored.
        let pending = {
            let sql = format!(
                "SELECT chunk_id FROM [{temp_table}]
                 WHERE NOT EXISTS (
                     SELECT 1 FROM [{table}]
                     WHERE [{table}].chunk_id = [{temp_table}].chunk_id
                       AND [{table}].content_hash = [{temp_table}].content_hash
                 )"
            );
            let mut stmt = self
                .connection
                .prepare(&sql)
                .map_err(|error| format!("failed to prepare pending query: {error}"))?;
            let rows = stmt
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(|error| format!("failed to execute pending query: {error}"))?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|error| format!("failed to collect pending chunk IDs: {error}"))?
        };

        // Query stale: stored chunks not present in current.
        let stale = {
            let sql = format!(
                "SELECT chunk_id FROM [{table}]
                 WHERE chunk_id NOT IN (SELECT chunk_id FROM [{temp_table}])"
            );
            let mut stmt = self
                .connection
                .prepare(&sql)
                .map_err(|error| format!("failed to prepare stale query: {error}"))?;
            let rows = stmt
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(|error| format!("failed to execute stale query: {error}"))?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|error| format!("failed to collect stale chunk IDs: {error}"))?
        };

        // Clean up temp table for reuse within the same connection.
        self.connection
            .execute_batch(&format!("DROP TABLE IF EXISTS [{temp_table}]"))
            .map_err(|error| format!("failed to drop temp table: {error}"))?;

        Ok((pending, stale))
    }

    fn load_vectors(&self) -> Result<Vec<StoredVector>, String> {
        let Some(table) = &self.active_table else {
            return Ok(Vec::new());
        };
        if !table_exists(self.connection, table)? {
            return Ok(Vec::new());
        }

        load_stored_vectors(self.connection, table)
    }

    fn upsert(&mut self, vectors: &[StoredVector]) -> Result<(), String> {
        if vectors.is_empty() {
            return Ok(());
        }

        let current_model = self
            .current_model()?
            .ok_or_else(|| "vector store has no active model".to_string())?;
        let table = self
            .active_table
            .as_deref()
            .ok_or_else(|| "no active table".to_string())?;

        let transaction = self
            .connection
            .unchecked_transaction()
            .map_err(|error| format!("failed to start vector transaction: {error}"))?;
        let sql = format!(
            "INSERT OR REPLACE INTO [{table}] (
                chunk_id, provider_name, model_name, dimensions,
                normalized, content_hash, embedding
             )
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)"
        );
        let mut insert_statement = transaction
            .prepare(&sql)
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
        let Some(table) = &self.active_table else {
            return Ok(());
        };
        if chunk_ids.is_empty() || !table_exists(self.connection, table)? {
            return Ok(());
        }

        let current_model = self
            .current_model()?
            .ok_or_else(|| "vector store has no active model".to_string())?;
        let skip_chunk_ids = chunk_ids
            .iter()
            .cloned()
            .collect::<std::collections::HashSet<_>>();
        let remaining_vectors = load_stored_vectors(self.connection, table)?
            .into_iter()
            .filter(|vector| !skip_chunk_ids.contains(&vector.chunk_id))
            .collect::<Vec<_>>();

        // Recreate this model's table.
        let table_name = table.clone();
        self.connection
            .execute_batch(&format!("DROP TABLE IF EXISTS [{table_name}]"))
            .map_err(|error| format!("failed to drop vector table for rebuild: {error}"))?;
        self.connection
            .execute_batch(&create_vector_table_sql(&table_name, &current_model))
            .map_err(|error| format!("failed to recreate vector table: {error}"))?;

        self.upsert(&remaining_vectors)?;
        Ok(())
    }

    fn query(&self, query: &VectorQuery) -> Result<Vec<VectorSearchResult>, String> {
        let current_model = self
            .current_model()?
            .ok_or_else(|| "vector store has no active model".to_string())?;
        let Some(table) = &self.active_table else {
            return Ok(Vec::new());
        };
        if !table_exists(self.connection, table)? {
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

        let sql = format!(
            "SELECT chunk_id, distance
             FROM [{table}]
             WHERE embedding MATCH ?1
               AND k = ?2
             ORDER BY distance ASC"
        );
        let mut statement = self
            .connection
            .prepare(&sql)
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

    fn list_models(&self) -> Result<Vec<StoredModelInfo>, String> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT cache_key, table_name, provider_name, model_name,
                        dimensions, normalized, is_active
                 FROM vector_model_registry
                 ORDER BY cache_key",
            )
            .map_err(|error| format!("failed to prepare model list query: {error}"))?;

        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                ))
            })
            .map_err(|error| format!("failed to query model list: {error}"))?;

        let mut models = Vec::new();
        for row in rows {
            let (
                cache_key,
                table_name,
                provider_name,
                model_name,
                dimensions,
                normalized,
                is_active,
            ) = row.map_err(|error| format!("failed to read model row: {error}"))?;

            let chunk_count = if table_exists(self.connection, &table_name)? {
                self.connection
                    .query_row(&format!("SELECT COUNT(*) FROM [{table_name}]"), [], |row| {
                        row.get::<_, i64>(0)
                    })
                    .map(|c| usize::try_from(c).unwrap_or(0))
                    .unwrap_or(0)
            } else {
                0
            };

            models.push(StoredModelInfo {
                cache_key,
                table_name,
                provider_name,
                model_name,
                dimensions: usize::try_from(dimensions).unwrap_or(0),
                normalized: normalized != 0,
                chunk_count,
                is_active: is_active != 0,
            });
        }

        Ok(models)
    }

    fn drop_model(&mut self, cache_key: &str) -> Result<bool, String> {
        let table_name: Option<String> = self
            .connection
            .query_row(
                "SELECT table_name FROM vector_model_registry WHERE cache_key = ?1",
                [cache_key],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| format!("failed to query registry for drop: {error}"))?;

        let Some(table_name) = table_name else {
            return Ok(false);
        };

        self.connection
            .execute_batch(&format!("DROP TABLE IF EXISTS [{table_name}]"))
            .map_err(|error| format!("failed to drop vector table: {error}"))?;
        self.connection
            .execute(
                "DELETE FROM vector_model_registry WHERE cache_key = ?1",
                [cache_key],
            )
            .map_err(|error| format!("failed to delete registry entry: {error}"))?;

        // If we dropped the active model, clear active state.
        if self.active_cache_key.as_deref() == Some(cache_key) {
            self.active_cache_key = None;
            self.active_table = None;
        }

        Ok(true)
    }

    fn delete_chunks_all_models(&mut self, chunk_ids: &[String]) -> Result<(), String> {
        if chunk_ids.is_empty() {
            return Ok(());
        }

        let models = self.list_models()?;
        for model_info in &models {
            if !table_exists(self.connection, &model_info.table_name)? {
                continue;
            }

            // vec0 virtual tables don't support DELETE WHERE, so we must
            // load remaining vectors, recreate the table, and re-insert.
            let skip_ids = chunk_ids
                .iter()
                .cloned()
                .collect::<std::collections::HashSet<_>>();
            let remaining = load_stored_vectors(self.connection, &model_info.table_name)?
                .into_iter()
                .filter(|v| !skip_ids.contains(&v.chunk_id))
                .collect::<Vec<_>>();

            let model = StoredModel {
                cache_key: model_info.cache_key.clone(),
                provider_name: model_info.provider_name.clone(),
                model_name: model_info.model_name.clone(),
                dimensions: model_info.dimensions,
                normalized: model_info.normalized,
            };

            let table = &model_info.table_name;
            self.connection
                .execute_batch(&format!("DROP TABLE IF EXISTS [{table}]"))
                .map_err(|error| {
                    format!("failed to drop table {table} for chunk cleanup: {error}")
                })?;
            self.connection
                .execute_batch(&create_vector_table_sql(table, &model))
                .map_err(|error| {
                    format!("failed to recreate table {table} for chunk cleanup: {error}")
                })?;

            // Re-insert remaining vectors directly (bypass the trait method which checks active model).
            if !remaining.is_empty() {
                let sql = format!(
                    "INSERT OR REPLACE INTO [{table}] (
                        chunk_id, provider_name, model_name, dimensions,
                        normalized, content_hash, embedding
                     )
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)"
                );
                let transaction = self
                    .connection
                    .unchecked_transaction()
                    .map_err(|error| format!("failed to start transaction: {error}"))?;
                let mut stmt = transaction
                    .prepare(&sql)
                    .map_err(|error| format!("failed to prepare insert: {error}"))?;
                for v in &remaining {
                    stmt.execute(params![
                        &v.chunk_id,
                        &v.provider_name,
                        &v.model_name,
                        i64::try_from(v.dimensions)
                            .map_err(|e| format!("dimensions overflow: {e}"))?,
                        i64::from(v.normalized),
                        &v.content_hash,
                        vector_to_blob(&v.embedding),
                    ])
                    .map_err(|error| format!("failed to re-insert vector: {error}"))?;
                }
                drop(stmt);
                transaction
                    .commit()
                    .map_err(|error| format!("failed to commit: {error}"))?;
            }
        }

        Ok(())
    }

    fn set_active_model(&mut self, cache_key: &str) -> Result<(), String> {
        self.connection
            .execute(
                "UPDATE vector_model_registry SET is_active = 0 WHERE is_active = 1",
                [],
            )
            .map_err(|error| format!("failed to clear active model: {error}"))?;
        let rows_updated = self
            .connection
            .execute(
                "UPDATE vector_model_registry SET is_active = 1 WHERE cache_key = ?1",
                [cache_key],
            )
            .map_err(|error| format!("failed to set active model: {error}"))?;

        if rows_updated == 0 {
            return Err(format!(
                "no model with cache key '{cache_key}' found in registry"
            ));
        }

        let (active_key, active_table) = load_active_model_info(self.connection)?;
        self.active_cache_key = active_key;
        self.active_table = active_table;

        Ok(())
    }
}

// --- Helper functions ---

fn ensure_registry_table(connection: &Connection) -> Result<(), String> {
    // Ensure the legacy state table exists (for migration check) and the new registry.
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS vector_index_state (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                provider_name TEXT NOT NULL,
                model_name TEXT NOT NULL,
                dimensions INTEGER NOT NULL,
                normalized INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS vector_model_registry (
                cache_key TEXT PRIMARY KEY,
                table_name TEXT NOT NULL UNIQUE,
                provider_name TEXT NOT NULL,
                model_name TEXT NOT NULL,
                dimensions INTEGER NOT NULL,
                normalized INTEGER NOT NULL,
                is_active INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );",
        )
        .map_err(|error| format!("failed to create registry tables: {error}"))
}

#[allow(clippy::too_many_lines)]
fn migrate_legacy_state(connection: &Connection) -> Result<(), String> {
    // Check if legacy vector_index_state has data AND registry is empty.
    let legacy_exists: bool = connection
        .query_row("SELECT COUNT(*) FROM vector_index_state", [], |row| {
            row.get::<_, i64>(0)
        })
        .map(|c| c > 0)
        .unwrap_or(false);

    if !legacy_exists {
        return Ok(());
    }

    let registry_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM vector_model_registry", [], |row| {
            row.get(0)
        })
        .map_err(|error| format!("failed to count registry entries: {error}"))?;

    if registry_count > 0 {
        return Ok(());
    }

    // Read legacy state.
    let legacy: Option<(String, String, i64, i64)> = connection
        .query_row(
            "SELECT provider_name, model_name, dimensions, normalized
             FROM vector_index_state WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()
        .map_err(|error| format!("failed to read legacy state: {error}"))?;

    let Some((provider_name, model_name, dimensions, normalized)) = legacy else {
        return Ok(());
    };

    let cache_key = format!("{provider_name}:{model_name}");
    let table_name = sanitized_table_name(&cache_key);

    // Check if the old "vectors" table exists.
    if !table_exists(connection, "vectors")? {
        // No vectors table — just register the model with an empty table.
        let dims_usize = usize::try_from(dimensions).unwrap_or(0);
        let model = StoredModel {
            cache_key: cache_key.clone(),
            provider_name: provider_name.clone(),
            model_name: model_name.clone(),
            dimensions: dims_usize,
            normalized: normalized != 0,
        };
        connection
            .execute_batch(&create_vector_table_sql(&table_name, &model))
            .map_err(|error| format!("failed to create vector table during migration: {error}"))?;
        connection
            .execute(
                "INSERT INTO vector_model_registry (
                    cache_key, table_name, provider_name, model_name,
                    dimensions, normalized, is_active
                 )
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1)",
                params![
                    &cache_key,
                    &table_name,
                    &provider_name,
                    &model_name,
                    dimensions,
                    normalized,
                ],
            )
            .map_err(|error| format!("failed to insert migrated registry entry: {error}"))?;
        return Ok(());
    }

    // Migrate: CREATE new table, INSERT INTO from old, DROP old.
    let dims_usize = usize::try_from(dimensions).unwrap_or(0);
    let model = StoredModel {
        cache_key: cache_key.clone(),
        provider_name: provider_name.clone(),
        model_name: model_name.clone(),
        dimensions: dims_usize,
        normalized: normalized != 0,
    };

    connection
        .execute_batch(&create_vector_table_sql(&table_name, &model))
        .map_err(|error| format!("failed to create new vector table during migration: {error}"))?;

    connection
        .execute_batch(&format!(
            "INSERT INTO [{table_name}] (
                chunk_id, provider_name, model_name, dimensions,
                normalized, content_hash, embedding
             )
             SELECT chunk_id, provider_name, model_name, dimensions,
                    normalized, content_hash, embedding
             FROM vectors"
        ))
        .map_err(|error| format!("failed to copy vectors during migration: {error}"))?;

    connection
        .execute_batch("DROP TABLE IF EXISTS vectors")
        .map_err(|error| format!("failed to drop legacy vectors table: {error}"))?;

    connection
        .execute(
            "INSERT INTO vector_model_registry (
                cache_key, table_name, provider_name, model_name,
                dimensions, normalized, is_active
             )
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1)",
            params![
                &cache_key,
                &table_name,
                &provider_name,
                &model_name,
                dimensions,
                normalized,
            ],
        )
        .map_err(|error| format!("failed to insert migrated registry entry: {error}"))?;

    Ok(())
}

fn load_active_model_info(
    connection: &Connection,
) -> Result<(Option<String>, Option<String>), String> {
    let result: Option<(String, String)> = connection
        .query_row(
            "SELECT cache_key, table_name FROM vector_model_registry WHERE is_active = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|error| format!("failed to load active model info: {error}"))?;

    match result {
        Some((key, table)) => Ok((Some(key), Some(table))),
        None => Ok((None, None)),
    }
}

fn table_exists(connection: &Connection, name: &str) -> Result<bool, String> {
    connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [name],
            |row| row.get::<_, i64>(0),
        )
        .map(|count| count > 0)
        .map_err(|error| format!("failed to check table existence: {error}"))
}

fn sanitized_table_name(cache_key: &str) -> String {
    let sanitized: String = cache_key
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("vectors_{sanitized}")
}

fn load_stored_vectors(connection: &Connection, table: &str) -> Result<Vec<StoredVector>, String> {
    let mut statement = connection
        .prepare(&format!(
            "SELECT chunk_id, provider_name, model_name, dimensions,
                    normalized, content_hash, embedding
             FROM [{table}]"
        ))
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

fn create_vector_table_sql(table_name: &str, model: &StoredModel) -> String {
    let distance_metric = if model.normalized { "cosine" } else { "l2" };
    format!(
        "CREATE VIRTUAL TABLE [{table_name}] USING vec0(
            chunk_id text primary key,
            provider_name text,
            model_name text,
            dimensions integer,
            normalized integer,
            content_hash text,
            embedding float[{dimensions}] distance_metric={distance_metric}
        );",
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

    fn test_model(cache_key: &str, name: &str, dims: usize) -> StoredModel {
        StoredModel {
            cache_key: cache_key.to_string(),
            provider_name: "openai-compatible".to_string(),
            model_name: name.to_string(),
            dimensions: dims,
            normalized: true,
        }
    }

    fn test_vector(
        model: &StoredModel,
        chunk_id: &str,
        hash: &str,
        embedding: Vec<f32>,
    ) -> StoredVector {
        StoredVector {
            chunk_id: chunk_id.to_string(),
            provider_name: model.provider_name.clone(),
            model_name: model.model_name.clone(),
            dimensions: model.dimensions,
            normalized: model.normalized,
            content_hash: hash.to_string(),
            embedding,
        }
    }

    #[test]
    fn sqlite_vec_store_roundtrips_vectors() {
        register_sqlite_vec_extension();
        let connection = Connection::open_in_memory().expect("in-memory db should open");
        let mut store = SqliteVecStore::new(&connection).expect("store should initialize");
        let model = test_model("openai-compatible:fixture", "fixture", 2);

        store.replace_model(&model).expect("model should be stored");
        store
            .upsert(&[
                test_vector(&model, "chunk-1", "hash-1", vec![1.0, 0.0]),
                test_vector(&model, "chunk-2", "hash-2", vec![0.0, 1.0]),
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
    fn switching_models_preserves_old_vectors() {
        register_sqlite_vec_extension();
        let connection = Connection::open_in_memory().expect("in-memory db should open");
        let mut store = SqliteVecStore::new(&connection).expect("store should initialize");

        let model_a = test_model("openai-compatible:first", "first", 2);
        let model_b = test_model("openai-compatible:second", "second", 3);

        // Index model A.
        store.replace_model(&model_a).expect("model A should init");
        store
            .upsert(&[test_vector(&model_a, "chunk-1", "hash-1", vec![1.0, 0.0])])
            .expect("model A vector should insert");

        // Switch to model B.
        store.replace_model(&model_b).expect("model B should init");
        assert_eq!(
            store.current_model().expect("model should load"),
            Some(model_b.clone())
        );
        // Model B has no vectors yet.
        assert!(store.load_hashes().expect("hashes should load").is_empty());

        // Switch back to model A — vectors should still be there.
        store
            .set_active_model(&model_a.cache_key)
            .expect("switch back should work");
        let hashes = store.load_hashes().expect("hashes should load");
        assert_eq!(hashes.get("chunk-1"), Some(&"hash-1".to_string()));

        // Verify list_models shows both.
        let models = store.list_models().expect("list should work");
        assert_eq!(models.len(), 2);
    }

    #[test]
    fn drop_model_removes_table_and_registry() {
        register_sqlite_vec_extension();
        let connection = Connection::open_in_memory().expect("in-memory db should open");
        let mut store = SqliteVecStore::new(&connection).expect("store should initialize");

        let model = test_model("openai-compatible:drop-me", "drop-me", 2);
        store.replace_model(&model).expect("model should init");
        store
            .upsert(&[test_vector(&model, "chunk-1", "hash-1", vec![1.0, 0.0])])
            .expect("vector should insert");

        let dropped = store
            .drop_model(&model.cache_key)
            .expect("drop should succeed");
        assert!(dropped);

        let models = store.list_models().expect("list should work");
        assert!(models.is_empty());

        // Dropping again returns false.
        let dropped_again = store
            .drop_model(&model.cache_key)
            .expect("second drop should succeed");
        assert!(!dropped_again);
    }

    #[test]
    fn delete_chunks_all_models_removes_from_every_table() {
        register_sqlite_vec_extension();
        let connection = Connection::open_in_memory().expect("in-memory db should open");
        let mut store = SqliteVecStore::new(&connection).expect("store should initialize");

        let model_a = test_model("openai-compatible:a", "a", 2);
        let model_b = test_model("openai-compatible:b", "b", 2);

        // Index same chunks in both models.
        store.replace_model(&model_a).expect("model A should init");
        store
            .upsert(&[
                test_vector(&model_a, "chunk-1", "hash-1", vec![1.0, 0.0]),
                test_vector(&model_a, "chunk-2", "hash-2", vec![0.0, 1.0]),
            ])
            .expect("model A vectors should insert");

        store.replace_model(&model_b).expect("model B should init");
        store
            .upsert(&[
                test_vector(&model_b, "chunk-1", "hash-1", vec![1.0, 0.0]),
                test_vector(&model_b, "chunk-2", "hash-2", vec![0.0, 1.0]),
            ])
            .expect("model B vectors should insert");

        // Delete chunk-1 from all models.
        store
            .delete_chunks_all_models(&["chunk-1".to_string()])
            .expect("cross-model delete should succeed");

        // Check model A.
        store
            .set_active_model(&model_a.cache_key)
            .expect("switch to A");
        let hashes_a = store.load_hashes().expect("hashes A");
        assert!(!hashes_a.contains_key("chunk-1"));
        assert!(hashes_a.contains_key("chunk-2"));

        // Check model B.
        store
            .set_active_model(&model_b.cache_key)
            .expect("switch to B");
        let hashes_b = store.load_hashes().expect("hashes B");
        assert!(!hashes_b.contains_key("chunk-1"));
        assert!(hashes_b.contains_key("chunk-2"));
    }

    #[test]
    fn legacy_migration_moves_vectors_to_namespaced_table() {
        register_sqlite_vec_extension();
        let connection = Connection::open_in_memory().expect("in-memory db should open");

        // Set up legacy state manually (simulating pre-migration database).
        connection
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS vector_index_state (
                    id INTEGER PRIMARY KEY CHECK (id = 1),
                    provider_name TEXT NOT NULL,
                    model_name TEXT NOT NULL,
                    dimensions INTEGER NOT NULL,
                    normalized INTEGER NOT NULL
                );
                INSERT INTO vector_index_state (id, provider_name, model_name, dimensions, normalized)
                VALUES (1, 'openai-compatible', 'legacy-model', 2, 1);",
            )
            .expect("legacy state should create");

        // Create old-style "vectors" table.
        connection
            .execute_batch(
                "CREATE VIRTUAL TABLE vectors USING vec0(
                    chunk_id text primary key,
                    provider_name text,
                    model_name text,
                    dimensions integer,
                    normalized integer,
                    content_hash text,
                    embedding float[2] distance_metric=cosine
                );",
            )
            .expect("legacy vectors table should create");

        connection
            .execute(
                "INSERT INTO vectors (chunk_id, provider_name, model_name, dimensions, normalized, content_hash, embedding)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    "chunk-legacy",
                    "openai-compatible",
                    "legacy-model",
                    2_i64,
                    1_i64,
                    "hash-legacy",
                    vector_to_blob(&[0.5, 0.5]),
                ],
            )
            .expect("legacy vector should insert");

        // Now create the store — this should trigger migration.
        let store =
            SqliteVecStore::new(&connection).expect("store should initialize with migration");

        // Verify active model.
        let model = store
            .current_model()
            .expect("model should load")
            .expect("model should exist");
        assert_eq!(model.cache_key, "openai-compatible:legacy-model");
        assert_eq!(model.model_name, "legacy-model");

        // Verify vectors migrated.
        let hashes = store.load_hashes().expect("hashes should load");
        assert_eq!(hashes.get("chunk-legacy"), Some(&"hash-legacy".to_string()));

        // Verify old table is gone.
        assert!(!table_exists(&connection, "vectors").expect("table check should work"));

        // Verify new namespaced table exists.
        let expected_table = sanitized_table_name("openai-compatible:legacy-model");
        assert!(table_exists(&connection, &expected_table).expect("table check should work"));
    }

    #[test]
    fn sanitized_table_name_handles_special_characters() {
        assert_eq!(
            sanitized_table_name("openai-compatible:text-embedding-3-small"),
            "vectors_openai_compatible_text_embedding_3_small"
        );
        assert_eq!(
            sanitized_table_name("my_custom_key"),
            "vectors_my_custom_key"
        );
    }
}
