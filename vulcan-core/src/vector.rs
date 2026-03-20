use crate::config::EmbeddingProviderConfig;
use crate::graph::resolve_note_reference;
use crate::write_lock::acquire_write_lock;
use crate::{load_vault_config, CacheDatabase, CacheError, VaultPaths};
use rusqlite::{params, Connection};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt::Write as _;
use std::fmt::{Display, Formatter};
use std::time::Instant;
use ulid::Ulid;
use vulcan_embed::{
    EmbeddingInput, EmbeddingProvider, OpenAICompatibleConfig, OpenAICompatibleProvider,
    SqliteVecStore, StoredModel, StoredVector, VectorQuery, VectorStore,
};

#[derive(Debug)]
pub enum VectorError {
    CacheMissing,
    Cache(CacheError),
    Graph(crate::GraphQueryError),
    Io(std::io::Error),
    MissingEmbeddingConfig,
    MissingVectorIndex,
    NoteHasNoChunks {
        path: String,
    },
    Provider(String),
    Sqlite(rusqlite::Error),
    Store(String),
    UnsupportedProvider {
        provider: String,
    },
    VectorIndexModelMismatch {
        indexed_model: String,
        requested_model: String,
    },
    InvalidQuery(String),
}

impl Display for VectorError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CacheMissing => {
                formatter.write_str("cache is missing; run `vulcan scan` before using vectors")
            }
            Self::Cache(error) => write!(formatter, "{error}"),
            Self::Graph(error) => write!(formatter, "{error}"),
            Self::Io(error) => write!(formatter, "{error}"),
            Self::MissingEmbeddingConfig => formatter.write_str(
                "no embedding provider is configured; set [.vulcan/config.toml] [embedding]",
            ),
            Self::MissingVectorIndex => {
                formatter.write_str("vector index is missing; run `vulcan vectors index`")
            }
            Self::NoteHasNoChunks { path } => write!(formatter, "note has no indexable chunks: {path}"),
            Self::Provider(message) | Self::Store(message) => write!(formatter, "{message}"),
            Self::Sqlite(error) => write!(formatter, "{error}"),
            Self::UnsupportedProvider { provider } => {
                write!(formatter, "unsupported embedding provider: {provider}")
            }
            Self::VectorIndexModelMismatch {
                indexed_model,
                requested_model,
            } => write!(
                formatter,
                "active vector index model '{indexed_model}' does not match requested model '{requested_model}'; rerun `vulcan vectors index`"
            ),
            Self::InvalidQuery(message) => formatter.write_str(message),
        }
    }
}

impl Error for VectorError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Cache(error) => Some(error),
            Self::Graph(error) => Some(error),
            Self::Io(error) => Some(error),
            Self::Sqlite(error) => Some(error),
            Self::CacheMissing
            | Self::MissingEmbeddingConfig
            | Self::MissingVectorIndex
            | Self::NoteHasNoChunks { .. }
            | Self::Provider(_)
            | Self::Store(_)
            | Self::UnsupportedProvider { .. }
            | Self::VectorIndexModelMismatch { .. }
            | Self::InvalidQuery(_) => None,
        }
    }
}

impl From<CacheError> for VectorError {
    fn from(error: CacheError) -> Self {
        Self::Cache(error)
    }
}

impl From<crate::GraphQueryError> for VectorError {
    fn from(error: crate::GraphQueryError) -> Self {
        Self::Graph(error)
    }
}

impl From<std::io::Error> for VectorError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<rusqlite::Error> for VectorError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}

pub type VectorIndexError = VectorError;
pub type VectorDuplicatesError = VectorError;
pub type ClusterError = VectorError;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VectorIndexQuery {
    pub provider: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VectorIndexReport {
    pub provider_name: String,
    pub model_name: String,
    pub dimensions: usize,
    pub indexed: usize,
    pub skipped: usize,
    pub failed: usize,
    pub batches: usize,
    pub rebuilt_index: bool,
    pub elapsed_seconds: f64,
    pub rate_per_second: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VectorNeighborsQuery {
    pub provider: Option<String>,
    pub text: Option<String>,
    pub note: Option<String>,
    pub limit: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VectorNeighborsReport {
    pub provider_name: String,
    pub model_name: String,
    pub dimensions: usize,
    pub query_text: Option<String>,
    pub note_path: Option<String>,
    pub hits: Vec<VectorNeighborHit>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VectorNeighborHit {
    pub document_path: String,
    pub chunk_id: String,
    pub heading_path: Vec<String>,
    pub snippet: String,
    pub distance: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VectorDuplicatesQuery {
    pub provider: Option<String>,
    pub threshold: f32,
    pub limit: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VectorDuplicatesReport {
    pub provider_name: String,
    pub model_name: String,
    pub dimensions: usize,
    pub threshold: f32,
    pub pairs: Vec<VectorDuplicatePair>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VectorDuplicatePair {
    pub left_document_path: String,
    pub left_chunk_id: String,
    pub right_document_path: String,
    pub right_chunk_id: String,
    pub similarity: f32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClusterQuery {
    pub provider: Option<String>,
    pub clusters: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ClusterReport {
    pub provider_name: String,
    pub model_name: String,
    pub dimensions: usize,
    pub cluster_count: usize,
    pub assignments: Vec<ClusterAssignment>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ClusterAssignment {
    pub cluster_id: usize,
    pub cluster_label: String,
    pub document_path: String,
    pub chunk_id: String,
    pub heading_path: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct IndexedChunk {
    chunk_id: String,
    document_id: String,
    document_path: String,
    heading_path: Vec<String>,
    content: String,
    content_hash: String,
}

#[allow(clippy::too_many_lines)]
pub fn index_vectors(
    paths: &VaultPaths,
    query: &VectorIndexQuery,
) -> Result<VectorIndexReport, VectorIndexError> {
    let provider = load_embedding_provider(paths, query.provider.as_deref())?;
    let provider_metadata = provider.metadata();
    let started_at = Instant::now();
    let mut report = VectorIndexReport {
        provider_name: provider_metadata.provider_name.clone(),
        model_name: provider_metadata.model_name.clone(),
        dimensions: provider_metadata.dimensions,
        indexed: 0,
        skipped: 0,
        failed: 0,
        batches: 0,
        rebuilt_index: false,
        elapsed_seconds: 0.0,
        rate_per_second: 0.0,
    };
    let batch_size = provider_metadata.max_batch_size.max(1);
    let mut initialized_skip_count = false;

    loop {
        let database = open_existing_cache(paths)?;
        let connection = database.connection();
        let store = SqliteVecStore::new(connection).map_err(VectorError::Store)?;
        let chunks = load_indexable_chunks(connection)?;
        let active_model = store.current_model().map_err(VectorError::Store)?;
        let requested_model = provider_model_from_metadata(&provider.metadata());
        let model_matches = active_model
            .as_ref()
            .is_some_and(|model| same_model(model, &requested_model));
        let hashes = if model_matches {
            store.load_hashes().map_err(VectorError::Store)?
        } else {
            HashMap::new()
        };
        if active_model.is_some() && !model_matches {
            report.rebuilt_index = true;
        }
        if !initialized_skip_count {
            report.skipped = chunks
                .iter()
                .filter(|chunk| hashes.get(&chunk.chunk_id) == Some(&chunk.content_hash))
                .count();
            initialized_skip_count = true;
        }

        let current_chunk_ids = chunks
            .iter()
            .map(|chunk| chunk.chunk_id.clone())
            .collect::<HashSet<_>>();
        let stale_chunk_ids = if model_matches {
            hashes
                .keys()
                .filter(|chunk_id| !current_chunk_ids.contains(*chunk_id))
                .cloned()
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        if !stale_chunk_ids.is_empty() {
            let _lock = acquire_write_lock(paths)?;
            let database = open_existing_cache(paths)?;
            let connection = database.connection();
            let mut store = SqliteVecStore::new(connection).map_err(VectorError::Store)?;
            store
                .delete_chunks(&stale_chunk_ids)
                .map_err(VectorError::Store)?;
            clear_cluster_rows(connection, None)?;
            continue;
        }

        let pending_chunks = chunks
            .into_iter()
            .filter(|chunk| hashes.get(&chunk.chunk_id) != Some(&chunk.content_hash))
            .take(batch_size)
            .collect::<Vec<_>>();
        if pending_chunks.is_empty() {
            break;
        }

        report.batches += 1;
        let inputs = pending_chunks
            .iter()
            .map(|chunk| EmbeddingInput {
                id: Ulid::new(),
                text: chunk.content.clone(),
            })
            .collect::<Vec<_>>();
        let results = provider.embed_batch(&inputs);
        let successful_dimension = results
            .iter()
            .find_map(|result| result.as_ref().ok().map(Vec::len));
        if let Some(dimensions) = successful_dimension {
            report.dimensions = dimensions;
        }

        let _lock = acquire_write_lock(paths)?;
        let database = open_existing_cache(paths)?;
        let connection = database.connection();
        let mut store = SqliteVecStore::new(connection).map_err(VectorError::Store)?;
        let fresh_chunks = load_chunks_by_ids(
            connection,
            &pending_chunks
                .iter()
                .map(|chunk| chunk.chunk_id.clone())
                .collect::<Vec<_>>(),
        )?;
        if let Some(dimensions) = successful_dimension {
            let model = StoredModel {
                provider_name: provider.metadata().provider_name.clone(),
                model_name: provider.metadata().model_name.clone(),
                dimensions,
                normalized: provider.metadata().normalized,
            };
            if store
                .current_model()
                .map_err(VectorError::Store)?
                .as_ref()
                .is_some_and(|active| !same_model(active, &model))
            {
                report.rebuilt_index = true;
            }
            store.replace_model(&model).map_err(VectorError::Store)?;
        }

        let mut vectors = Vec::new();
        let mut failures = Vec::new();

        for (chunk, result) in pending_chunks.iter().zip(results) {
            let Some(current_chunk) = fresh_chunks.get(&chunk.chunk_id) else {
                continue;
            };
            if current_chunk.content_hash != chunk.content_hash {
                continue;
            }

            match result {
                Ok(embedding) => {
                    report.indexed += 1;
                    vectors.push(StoredVector {
                        chunk_id: chunk.chunk_id.clone(),
                        provider_name: provider.metadata().provider_name.clone(),
                        model_name: provider.metadata().model_name.clone(),
                        dimensions: embedding.len(),
                        normalized: provider.metadata().normalized,
                        content_hash: chunk.content_hash.clone(),
                        embedding,
                    });
                }
                Err(error) => {
                    report.failed += 1;
                    failures.push((
                        current_chunk.document_id.clone(),
                        chunk.chunk_id.clone(),
                        error.message,
                    ));
                }
            }
        }

        if !vectors.is_empty() {
            store.upsert(&vectors).map_err(VectorError::Store)?;
            clear_cluster_rows(connection, None)?;
        }
        refresh_embedding_diagnostics(
            connection,
            &pending_chunks,
            &fresh_chunks,
            &provider.metadata().provider_name,
            &provider.metadata().model_name,
            &failures,
        )?;
    }

    report.elapsed_seconds = started_at.elapsed().as_secs_f64();
    let indexed = f64::from(u32::try_from(report.indexed).unwrap_or(u32::MAX));
    report.rate_per_second = if report.elapsed_seconds > 0.0 {
        indexed / report.elapsed_seconds
    } else {
        indexed
    };

    Ok(report)
}

pub fn query_vector_neighbors(
    paths: &VaultPaths,
    query: &VectorNeighborsQuery,
) -> Result<VectorNeighborsReport, VectorError> {
    if query.text.is_some() == query.note.is_some() {
        return Err(VectorError::InvalidQuery(
            "provide exactly one of a query text or --note".to_string(),
        ));
    }

    let provider = load_embedding_provider(paths, query.provider.as_deref())?;
    let database = open_existing_cache(paths)?;
    let connection = database.connection();
    let store = SqliteVecStore::new(connection).map_err(VectorError::Store)?;
    let active_model = store
        .current_model()
        .map_err(VectorError::Store)?
        .ok_or(VectorError::MissingVectorIndex)?;
    let requested_model = provider_model_from_metadata(&provider.metadata());
    if active_model.provider_name != requested_model.provider_name
        || active_model.model_name != requested_model.model_name
    {
        return Err(VectorError::VectorIndexModelMismatch {
            indexed_model: format!("{}:{}", active_model.provider_name, active_model.model_name),
            requested_model: format!(
                "{}:{}",
                requested_model.provider_name, requested_model.model_name
            ),
        });
    }

    let (query_text, note_path) = if let Some(text) = query.text.as_ref() {
        (text.clone(), None)
    } else {
        let note_identifier = query
            .note
            .as_deref()
            .expect("validated note query should be present");
        let note = resolve_note_reference(paths, note_identifier)?;
        let chunk_text = load_note_chunk_text(connection, &note.id)?;
        if chunk_text.trim().is_empty() {
            return Err(VectorError::NoteHasNoChunks { path: note.path });
        }
        (chunk_text, Some(note.path))
    };

    let embedding = provider.embed_batch(&[EmbeddingInput {
        id: Ulid::new(),
        text: query_text.clone(),
    }]);
    let vector = embedding
        .into_iter()
        .next()
        .expect("single query embedding should produce one result")
        .map_err(|error| VectorError::Provider(error.message))?;
    if vector.len() != active_model.dimensions {
        return Err(VectorError::VectorIndexModelMismatch {
            indexed_model: format!(
                "{}:{}:{}",
                active_model.provider_name, active_model.model_name, active_model.dimensions
            ),
            requested_model: format!(
                "{}:{}:{}",
                provider.metadata().provider_name,
                provider.metadata().model_name,
                vector.len()
            ),
        });
    }

    let hits = store
        .query(&VectorQuery {
            embedding: vector,
            limit: query.limit.max(1),
            provider_name: active_model.provider_name.clone(),
            model_name: active_model.model_name.clone(),
            dimensions: active_model.dimensions,
        })
        .map_err(VectorError::Store)?;
    let hydrated_hits = hydrate_vector_hits(connection, &hits, note_path.as_deref())?;

    Ok(VectorNeighborsReport {
        provider_name: active_model.provider_name,
        model_name: active_model.model_name,
        dimensions: active_model.dimensions,
        query_text: query.text.clone(),
        note_path,
        hits: hydrated_hits,
    })
}

pub fn vector_duplicates(
    paths: &VaultPaths,
    query: &VectorDuplicatesQuery,
) -> Result<VectorDuplicatesReport, VectorDuplicatesError> {
    let provider = load_embedding_provider(paths, query.provider.as_deref())?;
    let database = open_existing_cache(paths)?;
    let connection = database.connection();
    let store = SqliteVecStore::new(connection).map_err(VectorError::Store)?;
    let active_model = store
        .current_model()
        .map_err(VectorError::Store)?
        .ok_or(VectorError::MissingVectorIndex)?;
    validate_active_model(&active_model, &provider)?;

    let vectors = store.load_vectors().map_err(VectorError::Store)?;
    let chunks = load_chunks_by_ids(
        connection,
        &vectors
            .iter()
            .map(|vector| vector.chunk_id.clone())
            .collect::<Vec<_>>(),
    )?;
    let mut pairs = Vec::new();

    for (left_index, left) in vectors.iter().enumerate() {
        for right in vectors.iter().skip(left_index + 1) {
            let similarity = cosine_similarity(&left.embedding, &right.embedding);
            if similarity < query.threshold {
                continue;
            }

            let Some(left_chunk) = chunks.get(&left.chunk_id) else {
                continue;
            };
            let Some(right_chunk) = chunks.get(&right.chunk_id) else {
                continue;
            };
            pairs.push(VectorDuplicatePair {
                left_document_path: left_chunk.document_path.clone(),
                left_chunk_id: left.chunk_id.clone(),
                right_document_path: right_chunk.document_path.clone(),
                right_chunk_id: right.chunk_id.clone(),
                similarity,
            });
        }
    }

    pairs.sort_by(|left, right| {
        right
            .similarity
            .partial_cmp(&left.similarity)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.left_chunk_id.cmp(&right.left_chunk_id))
            .then_with(|| left.right_chunk_id.cmp(&right.right_chunk_id))
    });
    pairs.truncate(query.limit.max(1));

    Ok(VectorDuplicatesReport {
        provider_name: active_model.provider_name,
        model_name: active_model.model_name,
        dimensions: active_model.dimensions,
        threshold: query.threshold,
        pairs,
    })
}

pub fn cluster_vectors(
    paths: &VaultPaths,
    query: &ClusterQuery,
) -> Result<ClusterReport, ClusterError> {
    if query.clusters == 0 {
        return Err(VectorError::InvalidQuery(
            "cluster count must be at least 1".to_string(),
        ));
    }

    let provider = load_embedding_provider(paths, query.provider.as_deref())?;
    let database = open_existing_cache(paths)?;
    let connection = database.connection();
    let store = SqliteVecStore::new(connection).map_err(VectorError::Store)?;
    let active_model = store
        .current_model()
        .map_err(VectorError::Store)?
        .ok_or(VectorError::MissingVectorIndex)?;
    validate_active_model(&active_model, &provider)?;

    let vectors = store.load_vectors().map_err(VectorError::Store)?;
    if vectors.is_empty() {
        return Err(VectorError::MissingVectorIndex);
    }

    let cluster_count = query.clusters.min(vectors.len());
    let assignments = kmeans_assignments(&vectors, cluster_count);
    let chunks = load_chunks_by_ids(
        connection,
        &vectors
            .iter()
            .map(|vector| vector.chunk_id.clone())
            .collect::<Vec<_>>(),
    )?;
    let labels = cluster_labels(&vectors, &assignments, cluster_count, &chunks);
    let report_assignments = vectors
        .iter()
        .zip(assignments.iter().copied())
        .filter_map(|(vector, cluster_id)| {
            let chunk = chunks.get(&vector.chunk_id)?;
            Some(ClusterAssignment {
                cluster_id,
                cluster_label: labels
                    .get(&cluster_id)
                    .cloned()
                    .unwrap_or_else(|| format!("Cluster {}", cluster_id + 1)),
                document_path: chunk.document_path.clone(),
                chunk_id: vector.chunk_id.clone(),
                heading_path: chunk.heading_path.clone(),
            })
        })
        .collect::<Vec<_>>();

    let _lock = acquire_write_lock(paths)?;
    let database = open_existing_cache(paths)?;
    persist_cluster_assignments(database.connection(), &active_model, &report_assignments)?;

    Ok(ClusterReport {
        provider_name: active_model.provider_name,
        model_name: active_model.model_name,
        dimensions: active_model.dimensions,
        cluster_count,
        assignments: report_assignments,
    })
}

pub(crate) fn query_hybrid_candidates(
    paths: &VaultPaths,
    provider: Option<&str>,
    text: &str,
    limit: usize,
) -> Result<Vec<VectorNeighborHit>, VectorError> {
    query_vector_neighbors(
        paths,
        &VectorNeighborsQuery {
            provider: provider.map(ToOwned::to_owned),
            text: Some(text.to_string()),
            note: None,
            limit,
        },
    )
    .map(|report| report.hits)
}

fn load_embedding_provider(
    paths: &VaultPaths,
    requested_provider: Option<&str>,
) -> Result<OpenAICompatibleProvider, VectorError> {
    let config = load_vault_config(paths)
        .config
        .embedding
        .ok_or(VectorError::MissingEmbeddingConfig)?;
    if let Some(requested_provider) = requested_provider {
        if requested_provider != config.provider_name() {
            return Err(VectorError::UnsupportedProvider {
                provider: requested_provider.to_string(),
            });
        }
    }
    if config.provider_name() != "openai-compatible" {
        return Err(VectorError::UnsupportedProvider {
            provider: config.provider_name().to_string(),
        });
    }

    let api_key = resolve_api_key(&config)?;
    OpenAICompatibleProvider::new(OpenAICompatibleConfig {
        provider_name: config.provider_name().to_string(),
        base_url: config.base_url.clone(),
        api_key,
        model_name: config.model.clone(),
        normalized: config.normalized.unwrap_or(true),
        max_batch_size: config.max_batch_size.unwrap_or(32),
        max_input_tokens: config.max_input_tokens.unwrap_or(8_192),
        max_concurrency: config.max_concurrency.unwrap_or(4),
        ..OpenAICompatibleConfig::default()
    })
    .map_err(VectorError::Provider)
}

fn validate_active_model(
    active_model: &StoredModel,
    provider: &OpenAICompatibleProvider,
) -> Result<(), VectorError> {
    let requested_model = provider_model_from_metadata(&provider.metadata());
    if active_model.provider_name != requested_model.provider_name
        || active_model.model_name != requested_model.model_name
    {
        return Err(VectorError::VectorIndexModelMismatch {
            indexed_model: format!("{}:{}", active_model.provider_name, active_model.model_name),
            requested_model: format!(
                "{}:{}",
                requested_model.provider_name, requested_model.model_name
            ),
        });
    }

    Ok(())
}

fn resolve_api_key(config: &EmbeddingProviderConfig) -> Result<Option<String>, VectorError> {
    let Some(variable) = config.api_key_env.as_deref() else {
        return Ok(None);
    };

    std::env::var(variable).map(Some).map_err(|_| {
        VectorError::Provider(format!("embedding API key env var is not set: {variable}"))
    })
}

fn open_existing_cache(paths: &VaultPaths) -> Result<CacheDatabase, VectorError> {
    if !paths.cache_db().exists() {
        return Err(VectorError::CacheMissing);
    }

    CacheDatabase::open(paths).map_err(VectorError::from)
}

fn provider_model_from_metadata(metadata: &vulcan_embed::ModelMetadata) -> StoredModel {
    StoredModel {
        provider_name: metadata.provider_name.clone(),
        model_name: metadata.model_name.clone(),
        dimensions: metadata.dimensions,
        normalized: metadata.normalized,
    }
}

fn same_model(left: &StoredModel, right: &StoredModel) -> bool {
    left.provider_name == right.provider_name
        && left.model_name == right.model_name
        && left.normalized == right.normalized
        && (left.dimensions == right.dimensions || right.dimensions == 0)
}

fn load_indexable_chunks(connection: &Connection) -> Result<Vec<IndexedChunk>, VectorError> {
    let mut statement = connection.prepare(
        "
        SELECT chunks.id, chunks.document_id, documents.path, chunks.heading_path, chunks.content, chunks.content_hash
        FROM chunks
        JOIN documents ON documents.id = chunks.document_id
        ORDER BY documents.path, chunks.sequence_index
        ",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(IndexedChunk {
            chunk_id: row.get(0)?,
            document_id: row.get(1)?,
            document_path: row.get(2)?,
            heading_path: parse_heading_path(&row.get::<_, String>(3)?)
                .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?,
            content: row.get(4)?,
            content_hash: hash_to_hex(&row.get::<_, Vec<u8>>(5)?),
        })
    })?;

    rows.collect::<Result<Vec<_>, _>>()
        .map_err(VectorError::from)
}

fn load_chunks_by_ids(
    connection: &Connection,
    chunk_ids: &[String],
) -> Result<HashMap<String, IndexedChunk>, VectorError> {
    let chunks = load_indexable_chunks(connection)?;
    let chunk_id_set = chunk_ids.iter().collect::<HashSet<_>>();
    Ok(chunks
        .into_iter()
        .filter(|chunk| chunk_id_set.contains(&chunk.chunk_id))
        .map(|chunk| (chunk.chunk_id.clone(), chunk))
        .collect())
}

fn load_note_chunk_text(connection: &Connection, document_id: &str) -> Result<String, VectorError> {
    let mut statement = connection.prepare(
        "
        SELECT content
        FROM chunks
        WHERE document_id = ?1
        ORDER BY sequence_index
        ",
    )?;
    let rows = statement.query_map([document_id], |row| row.get::<_, String>(0))?;
    let chunks = rows.collect::<Result<Vec<_>, _>>()?;

    Ok(chunks.join("\n\n"))
}

fn hydrate_vector_hits(
    connection: &Connection,
    hits: &[vulcan_embed::VectorSearchResult],
    excluded_document_path: Option<&str>,
) -> Result<Vec<VectorNeighborHit>, VectorError> {
    let chunk_ids = hits
        .iter()
        .map(|hit| hit.chunk_id.clone())
        .collect::<Vec<_>>();
    let chunks = load_chunks_by_ids(connection, &chunk_ids)?;

    Ok(hits
        .iter()
        .filter_map(|hit| {
            let chunk = chunks.get(&hit.chunk_id)?;
            if excluded_document_path == Some(chunk.document_path.as_str()) {
                return None;
            }
            Some(VectorNeighborHit {
                document_path: chunk.document_path.clone(),
                chunk_id: hit.chunk_id.clone(),
                heading_path: chunk.heading_path.clone(),
                snippet: snippet_from_content(&chunk.content),
                distance: hit.distance,
            })
        })
        .collect())
}

fn kmeans_assignments(vectors: &[StoredVector], cluster_count: usize) -> Vec<usize> {
    let mut centroids = vectors
        .iter()
        .take(cluster_count)
        .map(|vector| vector.embedding.clone())
        .collect::<Vec<_>>();
    let mut assignments = vec![0_usize; vectors.len()];

    for _ in 0..16 {
        let mut changed = false;
        for (index, vector) in vectors.iter().enumerate() {
            let (best_cluster, _) = centroids
                .iter()
                .enumerate()
                .map(|(cluster_id, centroid)| {
                    (cluster_id, cosine_distance(&vector.embedding, centroid))
                })
                .min_by(|left, right| {
                    left.1
                        .partial_cmp(&right.1)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .expect("k-means should always have at least one centroid");
            if assignments[index] != best_cluster {
                assignments[index] = best_cluster;
                changed = true;
            }
        }

        let mut sums = vec![vec![0.0_f32; centroids[0].len()]; cluster_count];
        let mut counts = vec![0_usize; cluster_count];
        for (vector, cluster_id) in vectors.iter().zip(assignments.iter().copied()) {
            counts[cluster_id] += 1;
            for (value, sum) in vector.embedding.iter().zip(sums[cluster_id].iter_mut()) {
                *sum += *value;
            }
        }
        for cluster_id in 0..cluster_count {
            if counts[cluster_id] == 0 {
                continue;
            }
            let divisor = f32::from(u16::try_from(counts[cluster_id]).unwrap_or(u16::MAX));
            for value in &mut sums[cluster_id] {
                *value /= divisor;
            }
            normalize_in_place(&mut sums[cluster_id]);
            centroids[cluster_id].clone_from(&sums[cluster_id]);
        }

        if !changed {
            break;
        }
    }

    assignments
}

fn cluster_labels(
    vectors: &[StoredVector],
    assignments: &[usize],
    cluster_count: usize,
    chunks: &HashMap<String, IndexedChunk>,
) -> HashMap<usize, String> {
    let mut labels = HashMap::new();

    for cluster_id in 0..cluster_count {
        let label = vectors
            .iter()
            .zip(assignments.iter().copied())
            .find_map(|(vector, assigned_cluster)| {
                if assigned_cluster != cluster_id {
                    return None;
                }
                chunks.get(&vector.chunk_id).map(|chunk| {
                    if chunk.heading_path.is_empty() {
                        chunk.document_path.clone()
                    } else {
                        format!(
                            "{} > {}",
                            chunk.document_path,
                            chunk.heading_path.join(" > ")
                        )
                    }
                })
            })
            .unwrap_or_else(|| format!("Cluster {}", cluster_id + 1));
        labels.insert(cluster_id, label);
    }

    labels
}

fn persist_cluster_assignments(
    connection: &Connection,
    model: &StoredModel,
    assignments: &[ClusterAssignment],
) -> Result<(), VectorError> {
    let transaction = connection.unchecked_transaction()?;
    clear_cluster_rows_tx(&transaction, Some(model))?;
    for assignment in assignments {
        transaction.execute(
            "
            INSERT INTO vector_clusters (
                provider_name,
                model_name,
                dimensions,
                cluster_id,
                cluster_label,
                chunk_id
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            ",
            params![
                &model.provider_name,
                &model.model_name,
                i64::try_from(model.dimensions).unwrap_or(i64::MAX),
                i64::try_from(assignment.cluster_id).unwrap_or(i64::MAX),
                &assignment.cluster_label,
                &assignment.chunk_id,
            ],
        )?;
    }
    transaction.commit()?;
    Ok(())
}

fn clear_cluster_rows(
    connection: &Connection,
    model: Option<&StoredModel>,
) -> Result<(), VectorError> {
    let transaction = connection.unchecked_transaction()?;
    clear_cluster_rows_tx(&transaction, model)?;
    transaction.commit()?;
    Ok(())
}

fn clear_cluster_rows_tx(
    transaction: &rusqlite::Transaction<'_>,
    model: Option<&StoredModel>,
) -> Result<(), rusqlite::Error> {
    if let Some(model) = model {
        transaction.execute(
            "
            DELETE FROM vector_clusters
            WHERE provider_name = ?1 AND model_name = ?2 AND dimensions = ?3
            ",
            params![
                &model.provider_name,
                &model.model_name,
                i64::try_from(model.dimensions).unwrap_or(i64::MAX),
            ],
        )?;
    } else {
        transaction.execute("DELETE FROM vector_clusters", [])?;
    }

    Ok(())
}

fn refresh_embedding_diagnostics(
    connection: &Connection,
    pending_chunks: &[IndexedChunk],
    fresh_chunks: &HashMap<String, IndexedChunk>,
    provider_name: &str,
    model_name: &str,
    failures: &[(String, String, String)],
) -> Result<(), VectorError> {
    let transaction = connection.unchecked_transaction()?;
    for chunk in pending_chunks {
        if let Some(current_chunk) = fresh_chunks.get(&chunk.chunk_id) {
            transaction.execute(
                "
                DELETE FROM diagnostics
                WHERE kind = 'embedding_error'
                  AND document_id = ?1
                  AND json_extract(detail, '$.chunk_id') = ?2
                  AND json_extract(detail, '$.provider_name') = ?3
                  AND json_extract(detail, '$.model_name') = ?4
                ",
                params![
                    &current_chunk.document_id,
                    &chunk.chunk_id,
                    provider_name,
                    model_name,
                ],
            )?;
        }
    }

    for (document_id, chunk_id, message) in failures {
        transaction.execute(
            "
            INSERT INTO diagnostics (id, document_id, kind, message, detail, created_at)
            VALUES (?1, ?2, 'embedding_error', ?3, ?4, strftime('%s', 'now'))
            ",
            params![
                Ulid::new().to_string(),
                document_id,
                message,
                serde_json::json!({
                    "chunk_id": chunk_id,
                    "provider_name": provider_name,
                    "model_name": model_name,
                })
                .to_string(),
            ],
        )?;
    }

    transaction.commit()?;
    Ok(())
}

fn parse_heading_path(value: &str) -> Result<Vec<String>, serde_json::Error> {
    serde_json::from_str(value)
}

fn snippet_from_content(content: &str) -> String {
    let flattened = content.split_whitespace().collect::<Vec<_>>().join(" ");
    if flattened.chars().count() <= 160 {
        flattened
    } else {
        flattened.chars().take(157).collect::<String>() + "..."
    }
}

fn hash_to_hex(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(&mut encoded, "{byte:02x}");
    }
    encoded
}

fn cosine_similarity(left: &[f32], right: &[f32]) -> f32 {
    let dot = left
        .iter()
        .zip(right.iter())
        .map(|(left_value, right_value)| left_value * right_value)
        .sum::<f32>();
    let left_norm = left.iter().map(|value| value * value).sum::<f32>().sqrt();
    let right_norm = right.iter().map(|value| value * value).sum::<f32>().sqrt();
    if left_norm == 0.0 || right_norm == 0.0 {
        0.0
    } else {
        dot / (left_norm * right_norm)
    }
}

fn cosine_distance(left: &[f32], right: &[f32]) -> f32 {
    1.0 - cosine_similarity(left, right)
}

fn normalize_in_place(values: &mut [f32]) {
    let norm = values.iter().map(|value| value * value).sum::<f32>().sqrt();
    if norm == 0.0 {
        return;
    }
    for value in values {
        *value /= norm;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{scan_vault, ScanMode};
    use serde_json::Value;
    use std::fs;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::path::Path;
    use std::thread;
    use tempfile::TempDir;

    #[test]
    fn vectors_index_embeds_chunks_and_skips_unchanged_rows() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("basic", &vault_root);
        let server = MockEmbeddingServer::spawn();
        write_embedding_config(&vault_root, &server.base_url());
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("full scan should succeed");

        let first_report = index_vectors(&paths, &VectorIndexQuery { provider: None })
            .expect("vector index should succeed");
        let second_report = index_vectors(&paths, &VectorIndexQuery { provider: None })
            .expect("second vector index should succeed");

        assert_eq!(first_report.indexed, 4);
        assert_eq!(first_report.failed, 0);
        assert_eq!(second_report.indexed, 0);
        assert_eq!(second_report.skipped, 4);
        server.shutdown();
    }

    #[test]
    fn vector_neighbors_returns_ranked_hits_for_query_text() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("basic", &vault_root);
        let server = MockEmbeddingServer::spawn();
        write_embedding_config(&vault_root, &server.base_url());
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("full scan should succeed");
        index_vectors(&paths, &VectorIndexQuery { provider: None })
            .expect("vector index should succeed");

        let report = query_vector_neighbors(
            &paths,
            &VectorNeighborsQuery {
                provider: None,
                text: Some("dashboard".to_string()),
                note: None,
                limit: 3,
            },
        )
        .expect("neighbors query should succeed");

        assert_eq!(report.hits.len(), 3);
        assert_eq!(report.hits[0].document_path, "Home.md");
        assert!(report.hits[0].distance <= report.hits[1].distance);
        server.shutdown();
    }

    #[test]
    fn vector_duplicates_reports_high_similarity_pairs() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("basic", &vault_root);
        let server = MockEmbeddingServer::spawn();
        write_embedding_config(&vault_root, &server.base_url());
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("full scan should succeed");
        index_vectors(&paths, &VectorIndexQuery { provider: None })
            .expect("vector index should succeed");

        let report = vector_duplicates(
            &paths,
            &VectorDuplicatesQuery {
                provider: None,
                threshold: 0.7,
                limit: 5,
            },
        )
        .expect("duplicates query should succeed");

        assert!(!report.pairs.is_empty());
        assert!(report.pairs[0].similarity >= 0.7);
        server.shutdown();
    }

    #[test]
    fn cluster_vectors_persists_cluster_assignments() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("basic", &vault_root);
        let server = MockEmbeddingServer::spawn();
        write_embedding_config(&vault_root, &server.base_url());
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("full scan should succeed");
        index_vectors(&paths, &VectorIndexQuery { provider: None })
            .expect("vector index should succeed");

        let report = cluster_vectors(
            &paths,
            &ClusterQuery {
                provider: None,
                clusters: 2,
            },
        )
        .expect("cluster command should succeed");
        let database = CacheDatabase::open(&paths).expect("database should open");

        assert_eq!(report.cluster_count, 2);
        assert_eq!(report.assignments.len(), 4);
        assert_eq!(
            database
                .connection()
                .query_row("SELECT COUNT(*) FROM vector_clusters", [], |row| row
                    .get::<_, i64>(0))
                .expect("cluster row count should be readable"),
            4
        );
        server.shutdown();
    }

    fn write_embedding_config(vault_root: &Path, base_url: &str) {
        fs::create_dir_all(vault_root.join(".vulcan")).expect("config dir should exist");
        fs::write(
            vault_root.join(".vulcan/config.toml"),
            format!(
                "[embedding]\nprovider = \"openai-compatible\"\nbase_url = \"{base_url}\"\nmodel = \"fixture\"\nmax_batch_size = 8\nmax_concurrency = 1\n"
            ),
        )
        .expect("config should write");
    }

    fn copy_fixture_vault(name: &str, destination: &Path) {
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../tests/fixtures/vaults")
            .join(name);

        copy_dir_recursive(&source, destination);
    }

    fn copy_dir_recursive(source: &Path, destination: &Path) {
        fs::create_dir_all(destination).expect("destination directory should be created");

        for entry in fs::read_dir(source).expect("source directory should be readable") {
            let entry = entry.expect("directory entry should be readable");
            let file_type = entry.file_type().expect("file type should be readable");
            let target = destination.join(entry.file_name());

            if file_type.is_dir() {
                copy_dir_recursive(&entry.path(), &target);
            } else {
                fs::copy(entry.path(), target).expect("fixture file should copy");
            }
        }
    }

    struct MockEmbeddingServer {
        address: String,
        shutdown_tx: std::sync::mpsc::Sender<()>,
        handle: Option<thread::JoinHandle<()>>,
    }

    impl MockEmbeddingServer {
        fn spawn() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("listener should bind");
            listener
                .set_nonblocking(true)
                .expect("listener should be configurable");
            let address = listener
                .local_addr()
                .expect("listener should expose a local address");
            let (shutdown_tx, shutdown_rx) = std::sync::mpsc::channel();

            let handle = thread::spawn(move || loop {
                if shutdown_rx.try_recv().is_ok() {
                    break;
                }

                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let request = read_request(&mut stream);
                        let inputs = request
                            .body
                            .get("input")
                            .and_then(Value::as_array)
                            .expect("request should include input");
                        let body = serde_json::json!({
                            "data": inputs.iter().enumerate().map(|(index, input)| {
                                serde_json::json!({
                                    "index": index,
                                    "embedding": embedding_for_input(input.as_str().unwrap_or_default()),
                                })
                            }).collect::<Vec<_>>(),
                        })
                        .to_string();
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        );
                        stream
                            .write_all(response.as_bytes())
                            .expect("response should write");
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(std::time::Duration::from_millis(10));
                    }
                    Err(error) => panic!("unexpected mock server error: {error}"),
                }
            });

            Self {
                address: format!("http://{address}/v1"),
                shutdown_tx,
                handle: Some(handle),
            }
        }

        fn base_url(&self) -> String {
            self.address.clone()
        }

        fn shutdown(mut self) {
            let _ = self.shutdown_tx.send(());
            if let Some(handle) = self.handle.take() {
                handle.join().expect("mock server should join");
            }
        }
    }

    fn embedding_for_input(input: &str) -> Vec<f32> {
        if input.contains("dashboard") || input.contains("Home links") {
            vec![1.0, 0.0]
        } else if input.contains("Bob") || input.contains("ownership") {
            vec![0.0, 1.0]
        } else if input.contains("Alpha") || input.contains("Project") {
            vec![0.75, 0.25]
        } else {
            vec![0.5, 0.5]
        }
    }

    #[derive(Debug)]
    struct CapturedRequest {
        body: Value,
    }

    fn read_request(stream: &mut std::net::TcpStream) -> CapturedRequest {
        let mut buffer = Vec::new();
        let mut header_end = None;

        loop {
            let mut chunk = [0_u8; 1024];
            let bytes_read = stream.read(&mut chunk).expect("request should read");
            if bytes_read == 0 {
                break;
            }
            buffer.extend_from_slice(&chunk[..bytes_read]);
            if let Some(position) = find_subslice(&buffer, b"\r\n\r\n") {
                header_end = Some(position + 4);
                break;
            }
        }

        let header_end = header_end.expect("request should contain headers");
        let header_text = String::from_utf8(buffer[..header_end].to_vec()).expect("utf8 headers");
        let content_length = header_text
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .and_then(|value| value.trim().parse::<usize>().ok())
            })
            .expect("request should include content length");
        let mut body_bytes = buffer[header_end..].to_vec();
        while body_bytes.len() < content_length {
            let mut chunk = vec![0_u8; content_length - body_bytes.len()];
            let bytes_read = stream.read(chunk.as_mut_slice()).expect("body should read");
            if bytes_read == 0 {
                break;
            }
            body_bytes.extend_from_slice(&chunk[..bytes_read]);
        }

        CapturedRequest {
            body: serde_json::from_slice(&body_bytes).expect("body should parse"),
        }
    }

    fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
    }
}
