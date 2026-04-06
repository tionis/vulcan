use crate::config::EmbeddingProviderConfig;
use crate::graph::resolve_note_reference;
use crate::write_lock::acquire_write_lock;
use crate::{load_vault_config, CacheDatabase, CacheError, VaultPaths};
use rusqlite::{params, Connection};
use serde::Serialize;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::error::Error;
use std::fmt::Write as _;
use std::fmt::{Display, Formatter};
use std::time::Instant;
use ulid::Ulid;
use vulcan_embed::{
    EmbeddingInput, EmbeddingProvider, OpenAICompatibleConfig, OpenAICompatibleProvider,
    SqliteVecStore, StoredModel, StoredVector, VectorQuery, VectorStore,
};

// Re-export for CLI consumers.
pub use vulcan_embed::StoredModelInfo;

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
    pub dry_run: bool,
    pub verbose: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VectorIndexReport {
    pub dry_run: bool,
    pub provider_name: String,
    pub model_name: String,
    pub endpoint_url: String,
    pub api_key_env: Option<String>,
    pub api_key_set: bool,
    pub dimensions: usize,
    pub batch_size: usize,
    pub max_concurrency: usize,
    pub indexed: usize,
    pub skipped: usize,
    pub failed: usize,
    pub batches: usize,
    pub rebuilt_index: bool,
    pub elapsed_seconds: f64,
    pub rate_per_second: f64,
    /// Per-failure details: (path, chunk ID, error message).
    pub failure_details: Vec<(String, String, String)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VectorIndexPhase {
    Preparing,
    Embedding,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VectorIndexProgress {
    pub dry_run: bool,
    pub provider_name: String,
    pub model_name: String,
    pub endpoint_url: String,
    pub api_key_env: Option<String>,
    pub api_key_set: bool,
    pub batch_size: usize,
    pub max_concurrency: usize,
    pub phase: VectorIndexPhase,
    pub pending: usize,
    pub processed: usize,
    pub indexed: usize,
    pub skipped: usize,
    pub failed: usize,
    pub batches_completed: usize,
    pub total_batches: usize,
    /// Failures from the most recent batch: (path, chunk ID, error message).
    pub batch_failures: Vec<(String, String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VectorQueueReport {
    pub provider_name: String,
    pub model_name: String,
    pub active_provider_name: Option<String>,
    pub active_model_name: Option<String>,
    pub active_dimensions: Option<usize>,
    pub indexed_chunks: usize,
    pub expected_chunks: usize,
    pub pending_chunks: usize,
    pub stale_vectors: usize,
    pub model_mismatch: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VectorRepairQuery {
    pub provider: Option<String>,
    pub dry_run: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VectorRepairReport {
    pub dry_run: bool,
    pub provider_name: String,
    pub model_name: String,
    pub active_provider_name: Option<String>,
    pub active_model_name: Option<String>,
    pub active_dimensions: Option<usize>,
    pub indexed_chunks: usize,
    pub expected_chunks: usize,
    pub pending_chunks: usize,
    pub stale_vectors: usize,
    pub model_mismatch: bool,
    pub repaired: bool,
    pub index_report: Option<VectorIndexReport>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VectorRebuildQuery {
    pub provider: Option<String>,
    pub dry_run: bool,
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
    pub dry_run: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ClusterReport {
    pub dry_run: bool,
    pub provider_name: String,
    pub model_name: String,
    pub dimensions: usize,
    pub cluster_count: usize,
    pub clusters: Vec<ClusterSummary>,
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

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ClusterSummary {
    pub cluster_id: usize,
    pub cluster_label: String,
    pub keywords: Vec<String>,
    pub chunk_count: usize,
    pub document_count: usize,
    pub exemplar_document_path: String,
    pub exemplar_heading_path: Vec<String>,
    pub exemplar_snippet: String,
    pub top_documents: Vec<ClusterDocumentCount>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClusterDocumentCount {
    pub document_path: String,
    pub chunk_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelatedNotesQuery {
    pub provider: Option<String>,
    pub note: String,
    pub limit: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RelatedNotesReport {
    pub provider_name: String,
    pub model_name: String,
    pub dimensions: usize,
    pub note_path: String,
    pub hits: Vec<RelatedNoteHit>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RelatedNoteHit {
    pub document_path: String,
    pub heading_path: Vec<String>,
    pub snippet: String,
    pub similarity: f32,
    pub matched_chunks: usize,
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
    index_vectors_with_progress(paths, query, |_| {})
}

#[allow(clippy::too_many_lines)]
pub fn index_vectors_with_progress<F>(
    paths: &VaultPaths,
    query: &VectorIndexQuery,
    mut on_progress: F,
) -> Result<VectorIndexReport, VectorIndexError>
where
    F: FnMut(VectorIndexProgress),
{
    let loaded = load_embedding_provider(paths, query.provider.as_deref())?;
    let provider = loaded.provider;
    let cache_key = loaded.cache_key;
    let provider_metadata = provider.metadata();
    let started_at = Instant::now();
    let mut report = VectorIndexReport {
        dry_run: query.dry_run,
        provider_name: provider_metadata.provider_name.clone(),
        model_name: provider_metadata.model_name.clone(),
        endpoint_url: loaded.endpoint_url,
        api_key_env: loaded.api_key_env,
        api_key_set: loaded.api_key_set,
        dimensions: provider_metadata.dimensions,
        batch_size: provider_metadata.max_batch_size.max(1),
        max_concurrency: provider_metadata.max_concurrency.max(1),
        indexed: 0,
        skipped: 0,
        failed: 0,
        batches: 0,
        rebuilt_index: false,
        elapsed_seconds: 0.0,
        rate_per_second: 0.0,
        failure_details: Vec::new(),
    };
    let batch_size = report.batch_size;
    let max_concurrency = report.max_concurrency;
    let mut initialized_skip_count = false;
    let mut planned_batches = None;
    let mut pending_total = None;

    loop {
        let database = open_existing_cache(paths)?;
        let connection = database.connection();
        let store = SqliteVecStore::new(connection).map_err(VectorError::Store)?;
        let chunks = load_indexable_chunks(connection)?;
        let active_model = store.current_model().map_err(VectorError::Store)?;
        let requested_model = provider_model_from_metadata(&cache_key, &provider.metadata());
        let model_matches = active_model
            .as_ref()
            .is_some_and(|model| same_model(model, &requested_model));
        if active_model.is_some() && !model_matches {
            report.rebuilt_index = true;
        }

        // Use SQL-side comparison to determine pending and stale chunks.
        let current_pairs: Vec<(String, String)> = if model_matches {
            chunks
                .iter()
                .map(|c| (c.chunk_id.clone(), c.content_hash.clone()))
                .collect()
        } else {
            // Model mismatch: treat everything as pending, nothing stale.
            Vec::new()
        };
        let (pending_ids, stale_chunk_ids) = if model_matches {
            store
                .pending_and_stale_chunks(&current_pairs)
                .map_err(VectorError::Store)?
        } else {
            // All chunks pending when model changed.
            let all_pending = chunks.iter().map(|c| c.chunk_id.clone()).collect();
            (all_pending, Vec::new())
        };
        let pending_count = pending_ids.len();

        if !initialized_skip_count {
            report.skipped = chunks.len().saturating_sub(pending_count);
            initialized_skip_count = true;
        }

        if query.dry_run {
            report.indexed = pending_count;
            report.batches = report.indexed.div_ceil(batch_size);
            finalize_index_report(&mut report, started_at);
            on_progress(VectorIndexProgress {
                dry_run: report.dry_run,
                provider_name: report.provider_name.clone(),
                model_name: report.model_name.clone(),
                endpoint_url: report.endpoint_url.clone(),
                api_key_env: report.api_key_env.clone(),
                api_key_set: report.api_key_set,
                batch_size,
                max_concurrency,
                phase: VectorIndexPhase::Completed,
                pending: report.indexed,
                processed: 0,
                indexed: report.indexed,
                skipped: report.skipped,
                failed: 0,
                batches_completed: 0,
                total_batches: report.batches,
                batch_failures: Vec::new(),
            });
            return Ok(report);
        }

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
        if pending_total.is_none() {
            pending_total = Some(pending_count);
            planned_batches = Some(pending_count.div_ceil(batch_size));
            on_progress(VectorIndexProgress {
                dry_run: report.dry_run,
                provider_name: report.provider_name.clone(),
                model_name: report.model_name.clone(),
                endpoint_url: report.endpoint_url.clone(),
                api_key_env: report.api_key_env.clone(),
                api_key_set: report.api_key_set,
                batch_size,
                max_concurrency,
                phase: VectorIndexPhase::Preparing,
                pending: pending_count,
                processed: 0,
                indexed: 0,
                skipped: report.skipped,
                failed: 0,
                batches_completed: 0,
                total_batches: planned_batches.unwrap_or(0),
                batch_failures: Vec::new(),
            });
        }

        // Build a set of pending chunk IDs for fast lookup.
        let pending_id_set: HashSet<String> = pending_ids.into_iter().collect();
        let pending_chunks = chunks
            .into_iter()
            .filter(|chunk| pending_id_set.contains(&chunk.chunk_id))
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
                cache_key: cache_key.clone(),
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
        let mut diagnostic_failures = Vec::new();
        let mut batch_failures = Vec::new();

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
                    batch_failures.push((
                        current_chunk.document_path.clone(),
                        chunk.chunk_id.clone(),
                        error.message,
                    ));
                    diagnostic_failures.push((
                        current_chunk.document_id.clone(),
                        chunk.chunk_id.clone(),
                        batch_failures.last().unwrap().2.clone(),
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
            &diagnostic_failures,
        )?;

        report
            .failure_details
            .extend(batch_failures.iter().cloned());

        on_progress(VectorIndexProgress {
            dry_run: report.dry_run,
            provider_name: report.provider_name.clone(),
            model_name: report.model_name.clone(),
            endpoint_url: report.endpoint_url.clone(),
            api_key_env: report.api_key_env.clone(),
            api_key_set: report.api_key_set,
            batch_size,
            max_concurrency,
            phase: VectorIndexPhase::Embedding,
            pending: pending_total.unwrap_or(report.indexed + report.failed),
            processed: report.indexed + report.failed,
            indexed: report.indexed,
            skipped: report.skipped,
            failed: report.failed,
            batches_completed: report.batches,
            total_batches: planned_batches.unwrap_or(report.batches),
            batch_failures,
        });
    }

    finalize_index_report(&mut report, started_at);
    on_progress(VectorIndexProgress {
        dry_run: report.dry_run,
        provider_name: report.provider_name.clone(),
        model_name: report.model_name.clone(),
        endpoint_url: report.endpoint_url.clone(),
        api_key_env: report.api_key_env.clone(),
        api_key_set: report.api_key_set,
        batch_size,
        max_concurrency,
        phase: VectorIndexPhase::Completed,
        pending: pending_total.unwrap_or(0),
        processed: report.indexed + report.failed,
        indexed: report.indexed,
        skipped: report.skipped,
        failed: report.failed,
        batches_completed: report.batches,
        total_batches: planned_batches.unwrap_or(report.batches),
        batch_failures: Vec::new(),
    });

    Ok(report)
}

pub fn inspect_vector_queue(
    paths: &VaultPaths,
    provider: Option<&str>,
) -> Result<VectorQueueReport, VectorError> {
    let status = load_vector_index_status(paths, provider)?;
    Ok(VectorQueueReport {
        provider_name: status.provider_name,
        model_name: status.model_name,
        active_provider_name: status
            .active_model
            .as_ref()
            .map(|model| model.provider_name.clone()),
        active_model_name: status
            .active_model
            .as_ref()
            .map(|model| model.model_name.clone()),
        active_dimensions: status.active_model.as_ref().map(|model| model.dimensions),
        indexed_chunks: status.indexed_chunks,
        expected_chunks: status.expected_chunks,
        pending_chunks: status.pending_chunks,
        stale_vectors: status.stale_chunk_ids.len(),
        model_mismatch: status.model_mismatch,
    })
}

pub fn list_vector_models(
    paths: &VaultPaths,
) -> Result<Vec<vulcan_embed::StoredModelInfo>, VectorError> {
    let database = open_existing_cache(paths)?;
    let connection = database.connection();
    let store = SqliteVecStore::new(connection).map_err(VectorError::Store)?;
    store.list_models().map_err(VectorError::Store)
}

pub fn drop_vector_model(paths: &VaultPaths, cache_key: &str) -> Result<bool, VectorError> {
    let _lock = acquire_write_lock(paths)?;
    let database = open_existing_cache(paths)?;
    let connection = database.connection();
    let mut store = SqliteVecStore::new(connection).map_err(VectorError::Store)?;
    let dropped = store.drop_model(cache_key).map_err(VectorError::Store)?;
    if dropped {
        clear_cluster_rows(connection, None)?;
    }
    Ok(dropped)
}

pub fn repair_vectors(
    paths: &VaultPaths,
    query: &VectorRepairQuery,
) -> Result<VectorRepairReport, VectorError> {
    repair_vectors_with_progress(paths, query, |_| {})
}

pub fn repair_vectors_with_progress<F>(
    paths: &VaultPaths,
    query: &VectorRepairQuery,
    mut on_progress: F,
) -> Result<VectorRepairReport, VectorError>
where
    F: FnMut(VectorIndexProgress),
{
    let status = load_vector_index_status(paths, query.provider.as_deref())?;
    let mut index_report = None;
    let mut repaired = false;

    if !query.dry_run {
        if !status.stale_chunk_ids.is_empty() {
            delete_vector_chunks(paths, &status.stale_chunk_ids)?;
            repaired = true;
        }

        if status.model_mismatch {
            index_report = Some(rebuild_vectors_with_progress(
                paths,
                &VectorRebuildQuery {
                    provider: query.provider.clone(),
                    dry_run: false,
                },
                &mut on_progress,
            )?);
            repaired = true;
        } else if status.pending_chunks > 0 {
            index_report = Some(index_vectors_with_progress(
                paths,
                &VectorIndexQuery {
                    provider: query.provider.clone(),
                    dry_run: false,
                    verbose: false,
                },
                &mut on_progress,
            )?);
            repaired = true;
        }
    }

    Ok(VectorRepairReport {
        dry_run: query.dry_run,
        provider_name: status.provider_name,
        model_name: status.model_name,
        active_provider_name: status
            .active_model
            .as_ref()
            .map(|model| model.provider_name.clone()),
        active_model_name: status
            .active_model
            .as_ref()
            .map(|model| model.model_name.clone()),
        active_dimensions: status.active_model.as_ref().map(|model| model.dimensions),
        indexed_chunks: status.indexed_chunks,
        expected_chunks: status.expected_chunks,
        pending_chunks: status.pending_chunks,
        stale_vectors: status.stale_chunk_ids.len(),
        model_mismatch: status.model_mismatch,
        repaired,
        index_report,
    })
}

pub fn rebuild_vectors(
    paths: &VaultPaths,
    query: &VectorRebuildQuery,
) -> Result<VectorIndexReport, VectorError> {
    rebuild_vectors_with_progress(paths, query, |_| {})
}

pub fn rebuild_vectors_with_progress<F>(
    paths: &VaultPaths,
    query: &VectorRebuildQuery,
    mut on_progress: F,
) -> Result<VectorIndexReport, VectorError>
where
    F: FnMut(VectorIndexProgress),
{
    let status = load_vector_index_status(paths, query.provider.as_deref())?;
    if query.dry_run {
        return Ok(VectorIndexReport {
            dry_run: true,
            provider_name: status.provider_name,
            model_name: status.model_name,
            endpoint_url: String::new(),
            api_key_env: None,
            api_key_set: false,
            dimensions: status
                .active_model
                .as_ref()
                .map_or(0, |model| model.dimensions),
            batch_size: status.batch_size,
            max_concurrency: status.max_concurrency,
            indexed: status.expected_chunks,
            skipped: 0,
            failed: 0,
            batches: status.expected_chunks.div_ceil(status.batch_size.max(1)),
            rebuilt_index: true,
            elapsed_seconds: 0.0,
            rate_per_second: 0.0,
            failure_details: Vec::new(),
        });
    }

    clear_vector_index(paths)?;
    let mut report = index_vectors_with_progress(
        paths,
        &VectorIndexQuery {
            provider: query.provider.clone(),
            dry_run: false,
            verbose: false,
        },
        &mut on_progress,
    )?;
    report.rebuilt_index = true;
    Ok(report)
}

pub fn query_related_notes(
    paths: &VaultPaths,
    query: &RelatedNotesQuery,
) -> Result<RelatedNotesReport, VectorError> {
    let neighbor_report = query_vector_neighbors(
        paths,
        &VectorNeighborsQuery {
            provider: query.provider.clone(),
            text: None,
            note: Some(query.note.clone()),
            limit: query.limit.max(1).saturating_mul(8),
        },
    )?;

    let note_path = neighbor_report.note_path.clone().ok_or_else(|| {
        VectorError::InvalidQuery("related-note queries require --note".to_string())
    })?;
    let mut grouped = HashMap::<String, RelatedNoteHit>::new();

    for hit in neighbor_report.hits {
        let similarity = (1.0 - hit.distance).clamp(-1.0, 1.0);
        grouped
            .entry(hit.document_path.clone())
            .and_modify(|current| {
                current.matched_chunks += 1;
                if similarity > current.similarity {
                    current.similarity = similarity;
                    current.heading_path.clone_from(&hit.heading_path);
                    current.snippet.clone_from(&hit.snippet);
                }
            })
            .or_insert_with(|| RelatedNoteHit {
                document_path: hit.document_path,
                heading_path: hit.heading_path,
                snippet: hit.snippet,
                similarity,
                matched_chunks: 1,
            });
    }

    let mut hits = grouped.into_values().collect::<Vec<_>>();
    hits.sort_by(|left, right| {
        right
            .similarity
            .partial_cmp(&left.similarity)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| right.matched_chunks.cmp(&left.matched_chunks))
            .then_with(|| left.document_path.cmp(&right.document_path))
    });
    hits.truncate(query.limit.max(1));

    Ok(RelatedNotesReport {
        provider_name: neighbor_report.provider_name,
        model_name: neighbor_report.model_name,
        dimensions: neighbor_report.dimensions,
        note_path,
        hits,
    })
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

    let loaded = load_embedding_provider(paths, query.provider.as_deref())?;
    let provider = loaded.provider;
    let database = open_existing_cache(paths)?;
    let connection = database.connection();
    let store = SqliteVecStore::new(connection).map_err(VectorError::Store)?;
    let active_model = store
        .current_model()
        .map_err(VectorError::Store)?
        .ok_or(VectorError::MissingVectorIndex)?;
    let requested_model = provider_model_from_metadata(&loaded.cache_key, &provider.metadata());
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

/// Heap entry for the bounded top-N similarity scan in [`vector_duplicates_with_progress`].
///
/// Negating the similarity bits turns Rust's max-heap (`BinaryHeap`) into a min-heap so the root
/// always holds the *lowest* similarity in the current top-N set, enabling cheap eviction.
struct DuplicatesHeapEntry {
    neg_sim_bits: u32,
    pair: VectorDuplicatePair,
}
impl PartialEq for DuplicatesHeapEntry {
    fn eq(&self, other: &Self) -> bool {
        self.neg_sim_bits == other.neg_sim_bits
    }
}
impl Eq for DuplicatesHeapEntry {}
impl Ord for DuplicatesHeapEntry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.neg_sim_bits.cmp(&other.neg_sim_bits)
    }
}
impl PartialOrd for DuplicatesHeapEntry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

struct DuplicateScanRow<'a> {
    document_path: &'a str,
    chunk_id: &'a str,
    embedding: &'a [f32],
}

fn tracked_duplicate_similarity_floor(
    heap: &BinaryHeap<DuplicatesHeapEntry>,
    threshold: f32,
    limit: usize,
) -> f32 {
    if heap.len() < limit {
        threshold
    } else {
        heap.peek()
            .map_or(threshold, |entry| f32::from_bits(!entry.neg_sim_bits))
            .max(threshold)
    }
}

fn push_duplicate_candidate(
    heap: &mut BinaryHeap<DuplicatesHeapEntry>,
    pair: VectorDuplicatePair,
    threshold: f32,
    limit: usize,
) -> f32 {
    heap.push(DuplicatesHeapEntry {
        neg_sim_bits: (!pair.similarity.to_bits()),
        pair,
    });

    if heap.len() > limit {
        heap.pop();
    }

    tracked_duplicate_similarity_floor(heap, threshold, limit)
}

fn collect_vector_duplicate_pairs<F>(
    rows: &[DuplicateScanRow<'_>],
    threshold: f32,
    limit: usize,
    progress: F,
) -> Vec<VectorDuplicatePair>
where
    F: Fn(usize, usize),
{
    let n = rows.len();
    let mut heap: BinaryHeap<DuplicatesHeapEntry> = BinaryHeap::with_capacity(limit + 1);
    let mut min_tracked = threshold;

    for (left_index, left) in rows.iter().enumerate() {
        progress(left_index, n);
        for right in rows.iter().skip(left_index + 1) {
            let similarity = cosine_similarity(left.embedding, right.embedding);
            if similarity < min_tracked {
                continue;
            }

            min_tracked = push_duplicate_candidate(
                &mut heap,
                VectorDuplicatePair {
                    left_document_path: left.document_path.to_string(),
                    left_chunk_id: left.chunk_id.to_string(),
                    right_document_path: right.document_path.to_string(),
                    right_chunk_id: right.chunk_id.to_string(),
                    similarity,
                },
                threshold,
                limit,
            );
        }
    }
    progress(n, n);

    let mut pairs: Vec<VectorDuplicatePair> = heap.into_iter().map(|entry| entry.pair).collect();
    pairs.sort_by(|left, right| {
        right
            .similarity
            .partial_cmp(&left.similarity)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.left_chunk_id.cmp(&right.left_chunk_id))
            .then_with(|| left.right_chunk_id.cmp(&right.right_chunk_id))
    });
    pairs
}

pub fn vector_duplicates(
    paths: &VaultPaths,
    query: &VectorDuplicatesQuery,
) -> Result<VectorDuplicatesReport, VectorDuplicatesError> {
    vector_duplicates_with_progress(paths, query, |_, _| {})
}

/// Like [`vector_duplicates`] but calls `progress(completed_rows, total_rows)` periodically so
/// callers can render a progress bar or emit incremental output.
pub fn vector_duplicates_with_progress<F>(
    paths: &VaultPaths,
    query: &VectorDuplicatesQuery,
    progress: F,
) -> Result<VectorDuplicatesReport, VectorDuplicatesError>
where
    F: Fn(usize, usize),
{
    let loaded = load_embedding_provider(paths, query.provider.as_deref())?;
    let provider = loaded.provider;
    let database = open_existing_cache(paths)?;
    let connection = database.connection();
    let store = SqliteVecStore::new(connection).map_err(VectorError::Store)?;
    let active_model = store
        .current_model()
        .map_err(VectorError::Store)?
        .ok_or(VectorError::MissingVectorIndex)?;
    validate_active_model(&active_model, &loaded.cache_key, &provider)?;

    let vectors = store.load_vectors().map_err(VectorError::Store)?;
    let chunks = load_chunks_by_ids(
        connection,
        &vectors
            .iter()
            .map(|vector| vector.chunk_id.clone())
            .collect::<Vec<_>>(),
    )?;
    let limit = query.limit.max(1);
    let rows = vectors
        .iter()
        .filter_map(|vector| {
            chunks.get(&vector.chunk_id).map(|chunk| DuplicateScanRow {
                document_path: chunk.document_path.as_str(),
                chunk_id: vector.chunk_id.as_str(),
                embedding: &vector.embedding,
            })
        })
        .collect::<Vec<_>>();
    let pairs = collect_vector_duplicate_pairs(&rows, query.threshold, limit, progress);

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

    let database = open_existing_cache(paths)?;
    let connection = database.connection();
    let store = SqliteVecStore::new(connection).map_err(VectorError::Store)?;
    let active_model = store
        .current_model()
        .map_err(VectorError::Store)?
        .ok_or(VectorError::MissingVectorIndex)?;
    validate_requested_provider(&active_model, query.provider.as_deref())?;

    let vectors = store.load_vectors().map_err(VectorError::Store)?;
    if vectors.is_empty() {
        return Err(VectorError::MissingVectorIndex);
    }

    let cluster_count = query.clusters.min(vectors.len());
    let clustering = kmeans(&vectors, cluster_count);
    let chunks = load_chunks_by_ids(
        connection,
        &vectors
            .iter()
            .map(|vector| vector.chunk_id.clone())
            .collect::<Vec<_>>(),
    )?;
    let clusters = cluster_summaries(
        &vectors,
        &clustering.assignments,
        &clustering.centroids,
        cluster_count,
        &chunks,
    );
    let labels = clusters
        .iter()
        .map(|cluster| (cluster.cluster_id, cluster.cluster_label.clone()))
        .collect::<HashMap<_, _>>();
    let report_assignments = vectors
        .iter()
        .zip(clustering.assignments.iter().copied())
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

    if !query.dry_run {
        let _lock = acquire_write_lock(paths)?;
        let database = open_existing_cache(paths)?;
        persist_cluster_assignments(database.connection(), &active_model, &report_assignments)?;
    }

    Ok(ClusterReport {
        dry_run: query.dry_run,
        provider_name: active_model.provider_name,
        model_name: active_model.model_name,
        dimensions: active_model.dimensions,
        cluster_count,
        clusters,
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

#[derive(Debug, Clone)]
struct VectorIndexStatus {
    provider_name: String,
    model_name: String,
    batch_size: usize,
    max_concurrency: usize,
    active_model: Option<StoredModel>,
    expected_chunks: usize,
    indexed_chunks: usize,
    pending_chunks: usize,
    stale_chunk_ids: Vec<String>,
    model_mismatch: bool,
}

fn load_vector_index_status(
    paths: &VaultPaths,
    requested_provider: Option<&str>,
) -> Result<VectorIndexStatus, VectorError> {
    let config = load_embedding_config(paths, requested_provider)?;
    let requested_model = configured_model_from_config(&config);
    let database = open_existing_cache(paths)?;
    let connection = database.connection();
    let store = SqliteVecStore::new(connection).map_err(VectorError::Store)?;
    let chunks = load_indexable_chunks(connection)?;
    let current_chunk_ids = chunks
        .iter()
        .map(|chunk| chunk.chunk_id.clone())
        .collect::<HashSet<_>>();
    let vectors = store.load_vectors().map_err(VectorError::Store)?;
    let hashes = vectors
        .iter()
        .map(|vector| (vector.chunk_id.clone(), vector.content_hash.clone()))
        .collect::<HashMap<_, _>>();
    let stale_chunk_ids = vectors
        .iter()
        .filter(|vector| !current_chunk_ids.contains(&vector.chunk_id))
        .map(|vector| vector.chunk_id.clone())
        .collect::<Vec<_>>();
    let active_model = store.current_model().map_err(VectorError::Store)?;
    let model_mismatch = active_model
        .as_ref()
        .is_some_and(|model| !same_model(model, &requested_model));
    let pending_chunks = if model_mismatch {
        chunks.len()
    } else {
        chunks
            .iter()
            .filter(|chunk| hashes.get(&chunk.chunk_id) != Some(&chunk.content_hash))
            .count()
    };

    Ok(VectorIndexStatus {
        provider_name: requested_model.provider_name,
        model_name: requested_model.model_name,
        batch_size: config.max_batch_size.unwrap_or(32).max(1),
        max_concurrency: config.max_concurrency.unwrap_or(4).max(1),
        active_model,
        expected_chunks: chunks.len(),
        indexed_chunks: vectors.len(),
        pending_chunks,
        stale_chunk_ids,
        model_mismatch,
    })
}

fn load_embedding_config(
    paths: &VaultPaths,
    requested_provider: Option<&str>,
) -> Result<EmbeddingProviderConfig, VectorError> {
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

    Ok(config)
}

struct LoadedProvider {
    provider: OpenAICompatibleProvider,
    cache_key: String,
    endpoint_url: String,
    api_key_env: Option<String>,
    api_key_set: bool,
}

fn load_embedding_provider(
    paths: &VaultPaths,
    requested_provider: Option<&str>,
) -> Result<LoadedProvider, VectorError> {
    let config = load_embedding_config(paths, requested_provider)?;

    let cache_key = config.effective_cache_key();
    let api_key_env = config.api_key_env.clone();
    let api_key = resolve_api_key(&config)?;
    let api_key_set = api_key.is_some();
    let endpoint_url = format!("{}/embeddings", config.base_url.trim_end_matches('/'));
    let provider = OpenAICompatibleProvider::new(OpenAICompatibleConfig {
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
    .map_err(VectorError::Provider)?;
    Ok(LoadedProvider {
        provider,
        cache_key,
        endpoint_url,
        api_key_env,
        api_key_set,
    })
}

fn finalize_index_report(report: &mut VectorIndexReport, started_at: Instant) {
    report.elapsed_seconds = started_at.elapsed().as_secs_f64();
    let indexed = f64::from(u32::try_from(report.indexed).unwrap_or(u32::MAX));
    report.rate_per_second = if report.elapsed_seconds > 0.0 {
        indexed / report.elapsed_seconds
    } else {
        indexed
    };
}

fn validate_active_model(
    active_model: &StoredModel,
    cache_key: &str,
    provider: &OpenAICompatibleProvider,
) -> Result<(), VectorError> {
    let requested_model = provider_model_from_metadata(cache_key, &provider.metadata());
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

fn validate_requested_provider(
    active_model: &StoredModel,
    requested_provider: Option<&str>,
) -> Result<(), VectorError> {
    if let Some(requested_provider) = requested_provider {
        if requested_provider != active_model.provider_name {
            return Err(VectorError::UnsupportedProvider {
                provider: requested_provider.to_string(),
            });
        }
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

fn provider_model_from_metadata(
    cache_key: &str,
    metadata: &vulcan_embed::ModelMetadata,
) -> StoredModel {
    StoredModel {
        cache_key: cache_key.to_string(),
        provider_name: metadata.provider_name.clone(),
        model_name: metadata.model_name.clone(),
        dimensions: metadata.dimensions,
        normalized: metadata.normalized,
    }
}

fn configured_model_from_config(config: &EmbeddingProviderConfig) -> StoredModel {
    StoredModel {
        cache_key: config.effective_cache_key(),
        provider_name: config.provider_name().to_string(),
        model_name: config.model.clone(),
        dimensions: 0,
        normalized: config.normalized.unwrap_or(true),
    }
}

fn same_model(left: &StoredModel, right: &StoredModel) -> bool {
    left.cache_key == right.cache_key
        && left.normalized == right.normalized
        && (left.dimensions == right.dimensions || right.dimensions == 0)
}

fn delete_vector_chunks(paths: &VaultPaths, chunk_ids: &[String]) -> Result<(), VectorError> {
    if chunk_ids.is_empty() {
        return Ok(());
    }

    let _lock = acquire_write_lock(paths)?;
    let database = open_existing_cache(paths)?;
    let connection = database.connection();
    let mut store = SqliteVecStore::new(connection).map_err(VectorError::Store)?;
    store
        .delete_chunks_all_models(chunk_ids)
        .map_err(VectorError::Store)?;
    clear_cluster_rows(connection, None)?;
    Ok(())
}

fn clear_vector_index(paths: &VaultPaths) -> Result<(), VectorError> {
    let _lock = acquire_write_lock(paths)?;
    let database = open_existing_cache(paths)?;
    let connection = database.connection();
    let mut store = SqliteVecStore::new(connection).map_err(VectorError::Store)?;
    if let Some(model) = store.current_model().map_err(VectorError::Store)? {
        store
            .drop_model(&model.cache_key)
            .map_err(VectorError::Store)?;
    }
    clear_cluster_rows(connection, None)?;
    Ok(())
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

struct KMeansResult {
    assignments: Vec<usize>,
    centroids: Vec<Vec<f32>>,
}

fn kmeans(vectors: &[StoredVector], cluster_count: usize) -> KMeansResult {
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

    KMeansResult {
        assignments,
        centroids,
    }
}

fn cluster_summaries(
    vectors: &[StoredVector],
    assignments: &[usize],
    centroids: &[Vec<f32>],
    cluster_count: usize,
    chunks: &HashMap<String, IndexedChunk>,
) -> Vec<ClusterSummary> {
    let mut summaries = Vec::new();

    for (cluster_id, centroid) in centroids.iter().enumerate().take(cluster_count) {
        let mut members = vectors
            .iter()
            .zip(assignments.iter().copied())
            .filter_map(|(vector, assigned_cluster)| {
                if assigned_cluster != cluster_id {
                    return None;
                }
                let chunk = chunks.get(&vector.chunk_id)?;
                Some((chunk, cosine_distance(&vector.embedding, centroid)))
            })
            .collect::<Vec<_>>();
        members.sort_by(|left, right| {
            left.1
                .partial_cmp(&right.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(left.0.document_path.cmp(&right.0.document_path))
                .then(left.0.heading_path.cmp(&right.0.heading_path))
        });
        let Some((exemplar, _)) = members.first() else {
            continue;
        };
        let keywords = cluster_keywords(&members);

        let mut top_documents = members
            .iter()
            .fold(HashMap::<String, usize>::new(), |mut counts, (chunk, _)| {
                *counts.entry(chunk.document_path.clone()).or_insert(0) += 1;
                counts
            })
            .into_iter()
            .map(|(document_path, chunk_count)| ClusterDocumentCount {
                document_path,
                chunk_count,
            })
            .collect::<Vec<_>>();
        top_documents.sort_by(|left, right| {
            right
                .chunk_count
                .cmp(&left.chunk_count)
                .then(left.document_path.cmp(&right.document_path))
        });
        top_documents.truncate(5);

        summaries.push(ClusterSummary {
            cluster_id,
            cluster_label: if keywords.is_empty() {
                cluster_location_label(exemplar)
            } else {
                keywords.join(", ")
            },
            keywords,
            chunk_count: members.len(),
            document_count: members
                .iter()
                .map(|(chunk, _)| chunk.document_path.as_str())
                .collect::<HashSet<_>>()
                .len(),
            exemplar_document_path: exemplar.document_path.clone(),
            exemplar_heading_path: exemplar.heading_path.clone(),
            exemplar_snippet: snippet_from_content(&exemplar.content),
            top_documents,
        });
    }

    summaries
}

fn cluster_keywords(members: &[(&IndexedChunk, f32)]) -> Vec<String> {
    let mut counts = HashMap::<String, usize>::new();

    for (chunk, _) in members {
        let mut seen = HashSet::new();
        for token in chunk
            .heading_path
            .iter()
            .flat_map(|heading| heading.split(|character: char| !character.is_alphanumeric()))
            .chain(
                chunk
                    .content
                    .split(|character: char| !character.is_alphanumeric()),
            )
        {
            if let Some(token) = normalize_cluster_keyword(token) {
                seen.insert(token);
            }
        }
        for token in seen {
            *counts.entry(token).or_insert(0) += 1;
        }
    }

    let mut ranked = counts.into_iter().collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        right
            .1
            .cmp(&left.1)
            .then_with(|| right.0.len().cmp(&left.0.len()))
            .then_with(|| left.0.cmp(&right.0))
    });
    ranked.into_iter().map(|(token, _)| token).take(4).collect()
}

fn normalize_cluster_keyword(token: &str) -> Option<String> {
    let normalized = token.trim_matches('_').to_lowercase();
    if normalized.chars().count() < 4
        || normalized
            .chars()
            .all(|character| character.is_ascii_digit())
        || matches!(
            normalized.as_str(),
            "about"
                | "also"
                | "because"
                | "been"
                | "being"
                | "between"
                | "could"
                | "file"
                | "files"
                | "from"
                | "have"
                | "into"
                | "just"
                | "more"
                | "note"
                | "notes"
                | "section"
                | "should"
                | "than"
                | "that"
                | "their"
                | "them"
                | "there"
                | "these"
                | "this"
                | "used"
                | "using"
                | "vault"
                | "what"
                | "when"
                | "where"
                | "which"
                | "will"
                | "with"
                | "your"
        )
    {
        return None;
    }

    Some(normalized)
}

fn cluster_location_label(chunk: &IndexedChunk) -> String {
    if chunk.heading_path.is_empty() {
        chunk.document_path.clone()
    } else {
        format!(
            "{} > {}",
            chunk.document_path,
            chunk.heading_path.join(" > ")
        )
    }
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

        let first_report = index_vectors(
            &paths,
            &VectorIndexQuery {
                provider: None,
                dry_run: false,
                verbose: false,
            },
        )
        .expect("vector index should succeed");
        let second_report = index_vectors(
            &paths,
            &VectorIndexQuery {
                provider: None,
                dry_run: false,
                verbose: false,
            },
        )
        .expect("second vector index should succeed");

        assert_eq!(first_report.indexed, 4);
        assert_eq!(first_report.failed, 0);
        assert_eq!(first_report.batch_size, 8);
        assert_eq!(first_report.max_concurrency, 1);
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
        index_vectors(
            &paths,
            &VectorIndexQuery {
                provider: None,
                dry_run: false,
                verbose: false,
            },
        )
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
    fn vectors_index_extracted_attachment_chunks() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("attachments", &vault_root);
        write_attachment_sidecar(
            &vault_root,
            "assets/guide.pdf.txt",
            "dashboard manual reference",
        );
        write_attachment_sidecar(&vault_root, "assets/logo.png.txt", "dashboard logo");
        let server = MockEmbeddingServer::spawn();
        write_embedding_and_extraction_config(&vault_root, &server.base_url());
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("full scan should succeed");
        let index_report = index_vectors(
            &paths,
            &VectorIndexQuery {
                provider: None,
                dry_run: false,
                verbose: false,
            },
        )
        .expect("vector index should succeed");
        let neighbor_report = query_vector_neighbors(
            &paths,
            &VectorNeighborsQuery {
                provider: None,
                text: Some("dashboard".to_string()),
                note: None,
                limit: 8,
            },
        )
        .expect("neighbors query should succeed");

        assert!(index_report.indexed >= 4);
        assert!(neighbor_report
            .hits
            .iter()
            .any(|hit| hit.document_path == "assets/guide.pdf"));
        assert!(neighbor_report
            .hits
            .iter()
            .any(|hit| hit.document_path == "assets/logo.png"));
        server.shutdown();
    }

    #[test]
    fn duplicate_scan_floor_tracks_current_worst_retained_pair() {
        let mut heap = BinaryHeap::new();
        let threshold = 0.5;
        let limit = 2;

        let floor = push_duplicate_candidate(
            &mut heap,
            VectorDuplicatePair {
                left_document_path: "DocA.md".to_string(),
                left_chunk_id: "chunk-a".to_string(),
                right_document_path: "DocB.md".to_string(),
                right_chunk_id: "chunk-b".to_string(),
                similarity: 0.6,
            },
            threshold,
            limit,
        );
        assert!((floor - 0.5).abs() < f32::EPSILON);

        let floor = push_duplicate_candidate(
            &mut heap,
            VectorDuplicatePair {
                left_document_path: "DocC.md".to_string(),
                left_chunk_id: "chunk-c".to_string(),
                right_document_path: "DocD.md".to_string(),
                right_chunk_id: "chunk-d".to_string(),
                similarity: 0.8,
            },
            threshold,
            limit,
        );
        assert!((floor - 0.6).abs() < f32::EPSILON);

        let floor = push_duplicate_candidate(
            &mut heap,
            VectorDuplicatePair {
                left_document_path: "DocE.md".to_string(),
                left_chunk_id: "chunk-e".to_string(),
                right_document_path: "DocF.md".to_string(),
                right_chunk_id: "chunk-f".to_string(),
                similarity: 0.9,
            },
            threshold,
            limit,
        );
        assert!((floor - 0.8).abs() < f32::EPSILON);
        assert_eq!(heap.len(), 2);
        assert!(
            (tracked_duplicate_similarity_floor(&heap, threshold, limit) - 0.8).abs()
                < f32::EPSILON
        );
    }

    #[test]
    #[ignore = "benchmark-style regression test; run manually with --ignored --nocapture"]
    fn vector_duplicates_benchmark_large_synthetic_scan() {
        let document_paths = (0..1_200_usize)
            .map(|index| format!("Bench/Doc{index:04}.md"))
            .collect::<Vec<_>>();
        let chunk_ids = (0..1_200_usize)
            .map(|index| format!("chunk-{index:04}"))
            .collect::<Vec<_>>();
        let embeddings = (0..1_200_usize)
            .map(|index| {
                let cluster = (index % 12) as f32;
                let offset = (index / 12) as f32 * 0.0001;
                vec![1.0, cluster * 0.01 + offset, offset, 0.5]
            })
            .collect::<Vec<_>>();
        let rows = document_paths
            .iter()
            .zip(chunk_ids.iter())
            .zip(embeddings.iter())
            .map(|((document_path, chunk_id), embedding)| DuplicateScanRow {
                document_path: document_path.as_str(),
                chunk_id: chunk_id.as_str(),
                embedding,
            })
            .collect::<Vec<_>>();

        let started = Instant::now();
        let pairs = collect_vector_duplicate_pairs(&rows, 0.99, 25, |_, _| {});
        eprintln!(
            "scanned {} synthetic vectors in {:?}, retained {} pairs",
            rows.len(),
            started.elapsed(),
            pairs.len()
        );

        assert!(!pairs.is_empty());
        assert!(pairs.len() <= 25);
        assert!(pairs
            .windows(2)
            .all(|window| window[0].similarity >= window[1].similarity));
    }

    #[test]
    #[ignore = "benchmark-style regression test; run manually with --ignored --nocapture"]
    fn vector_duplicates_benchmark_large_vault() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        fs::create_dir_all(vault_root.join("Bench")).expect("benchmark dir should be created");
        let note_count = 1_200_usize;
        let cluster_count = 12_usize;

        for index in 0..note_count {
            let cluster = index % cluster_count;
            fs::write(
                vault_root.join(format!("Bench/Doc{index:04}.md")),
                format!("# Cluster {cluster}\n\nRepeated benchmark note for cluster {cluster}.\n"),
            )
            .expect("benchmark note should be written");
        }

        let server = MockEmbeddingServer::spawn();
        write_embedding_config(&vault_root, &server.base_url());
        let paths = VaultPaths::new(&vault_root);

        let scan_started = Instant::now();
        scan_vault(&paths, ScanMode::Full).expect("full scan should succeed");
        let scan_elapsed = scan_started.elapsed();

        let index_started = Instant::now();
        index_vectors(
            &paths,
            &VectorIndexQuery {
                provider: None,
                dry_run: false,
                verbose: false,
            },
        )
        .expect("vector index should succeed");
        let index_elapsed = index_started.elapsed();

        let duplicate_started = Instant::now();
        let report = vector_duplicates(
            &paths,
            &VectorDuplicatesQuery {
                provider: None,
                threshold: 0.99,
                limit: 25,
            },
        )
        .expect("duplicates query should succeed");
        let duplicate_elapsed = duplicate_started.elapsed();

        eprintln!(
            "bench vault: {} notes, scan {:?}, index {:?}, duplicates {:?}, retained {} pairs",
            note_count,
            scan_elapsed,
            index_elapsed,
            duplicate_elapsed,
            report.pairs.len()
        );

        assert!(!report.pairs.is_empty());
        assert!(report.pairs.len() <= 25);
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
        index_vectors(
            &paths,
            &VectorIndexQuery {
                provider: None,
                dry_run: false,
                verbose: false,
            },
        )
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
        index_vectors(
            &paths,
            &VectorIndexQuery {
                provider: None,
                dry_run: false,
                verbose: false,
            },
        )
        .expect("vector index should succeed");

        let report = cluster_vectors(
            &paths,
            &ClusterQuery {
                provider: None,
                clusters: 2,
                dry_run: false,
            },
        )
        .expect("cluster command should succeed");
        let database = CacheDatabase::open(&paths).expect("database should open");

        assert_eq!(report.cluster_count, 2);
        assert_eq!(report.clusters.len(), 2);
        assert_eq!(report.assignments.len(), 4);
        assert!(report.clusters.iter().all(|cluster| {
            cluster.chunk_count >= 1
                && !cluster.top_documents.is_empty()
                && !cluster.cluster_label.is_empty()
                && !cluster.keywords.is_empty()
        }));
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

    #[test]
    fn vectors_index_dry_run_reports_pending_chunks_without_writing_vectors() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("basic", &vault_root);
        let server = MockEmbeddingServer::spawn();
        write_embedding_config(&vault_root, &server.base_url());
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("full scan should succeed");
        let report = index_vectors(
            &paths,
            &VectorIndexQuery {
                provider: None,
                dry_run: true,
                verbose: false,
            },
        )
        .expect("dry-run vector index should succeed");
        let database = CacheDatabase::open(&paths).expect("database should open");
        let store = SqliteVecStore::new(database.connection()).expect("store should initialize");

        assert!(report.dry_run);
        assert_eq!(report.indexed, 4);
        assert_eq!(report.skipped, 0);
        assert!(store
            .current_model()
            .expect("current model should load")
            .is_none());
        server.shutdown();
    }

    #[test]
    fn vector_index_progress_reports_preparation_and_completion() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("basic", &vault_root);
        let server = MockEmbeddingServer::spawn();
        write_embedding_config(&vault_root, &server.base_url());
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("full scan should succeed");
        let mut events = Vec::new();
        let report = index_vectors_with_progress(
            &paths,
            &VectorIndexQuery {
                provider: None,
                dry_run: false,
                verbose: false,
            },
            |progress| events.push(progress),
        )
        .expect("vector index should succeed");

        assert_eq!(report.batch_size, 8);
        assert_eq!(report.max_concurrency, 1);
        assert!(!events.is_empty());
        assert_eq!(
            events.first().expect("first event should exist").phase,
            VectorIndexPhase::Preparing
        );
        assert_eq!(events.first().expect("first event should exist").pending, 4);
        assert_eq!(
            events.last().expect("last event should exist").phase,
            VectorIndexPhase::Completed
        );
        assert_eq!(events.last().expect("last event should exist").indexed, 4);
        server.shutdown();
    }

    #[test]
    fn cluster_vectors_dry_run_does_not_persist_assignments() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("basic", &vault_root);
        let server = MockEmbeddingServer::spawn();
        write_embedding_config(&vault_root, &server.base_url());
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("full scan should succeed");
        index_vectors(
            &paths,
            &VectorIndexQuery {
                provider: None,
                dry_run: false,
                verbose: false,
            },
        )
        .expect("vector index should succeed");
        let report = cluster_vectors(
            &paths,
            &ClusterQuery {
                provider: None,
                clusters: 2,
                dry_run: true,
            },
        )
        .expect("dry-run cluster command should succeed");
        let database = CacheDatabase::open(&paths).expect("database should open");

        assert!(report.dry_run);
        assert_eq!(report.cluster_count, 2);
        assert_eq!(report.clusters.len(), 2);
        assert!(report
            .clusters
            .iter()
            .all(|cluster| !cluster.keywords.is_empty()));
        assert_eq!(
            database
                .connection()
                .query_row("SELECT COUNT(*) FROM vector_clusters", [], |row| row
                    .get::<_, i64>(0))
                .expect("cluster row count should be readable"),
            0
        );
        server.shutdown();
    }

    #[test]
    fn cluster_vectors_uses_stored_index_without_api_key_env() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("basic", &vault_root);
        let server = MockEmbeddingServer::spawn();
        write_embedding_config(&vault_root, &server.base_url());
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("full scan should succeed");
        index_vectors(
            &paths,
            &VectorIndexQuery {
                provider: None,
                dry_run: false,
                verbose: false,
            },
        )
        .expect("vector index should succeed");
        fs::write(
            vault_root.join(".vulcan/config.toml"),
            format!(
                "[embedding]\nprovider = \"openai-compatible\"\nbase_url = \"{}\"\nmodel = \"fixture\"\napi_key_env = \"EMBEDDING_API_KEY\"\nmax_batch_size = 8\nmax_concurrency = 1\n",
                server.base_url()
            ),
        )
        .expect("config should rewrite");

        let report = cluster_vectors(
            &paths,
            &ClusterQuery {
                provider: None,
                clusters: 2,
                dry_run: true,
            },
        )
        .expect("cluster command should use the stored index");

        assert_eq!(report.cluster_count, 2);
        server.shutdown();
    }

    #[test]
    fn inspect_vector_queue_reports_pending_chunks_after_model_change() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("basic", &vault_root);
        let server = MockEmbeddingServer::spawn();
        write_embedding_config(&vault_root, &server.base_url());
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("full scan should succeed");
        index_vectors(
            &paths,
            &VectorIndexQuery {
                provider: None,
                dry_run: false,
                verbose: false,
            },
        )
        .expect("vector index should succeed");
        fs::write(
            vault_root.join(".vulcan/config.toml"),
            format!(
                "[embedding]\nprovider = \"openai-compatible\"\nbase_url = \"{}\"\nmodel = \"migrated\"\nmax_batch_size = 8\nmax_concurrency = 1\n",
                server.base_url()
            ),
        )
        .expect("config should rewrite");

        let report = inspect_vector_queue(&paths, None).expect("queue status should succeed");

        assert!(report.model_mismatch);
        assert_eq!(report.expected_chunks, 4);
        assert_eq!(report.pending_chunks, 4);
        server.shutdown();
    }

    #[test]
    fn repair_vectors_recovers_missing_rows() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("basic", &vault_root);
        let server = MockEmbeddingServer::spawn();
        write_embedding_config(&vault_root, &server.base_url());
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("full scan should succeed");
        index_vectors(
            &paths,
            &VectorIndexQuery {
                provider: None,
                dry_run: false,
                verbose: false,
            },
        )
        .expect("vector index should succeed");
        fs::write(
            vault_root.join("Home.md"),
            "---\naliases:\n  - Start\ntags:\n  - dashboard\n---\n\n# Home\n\nUpdated dashboard plans.\n",
        )
        .expect("updated note should write");
        scan_vault(&paths, ScanMode::Incremental).expect("incremental scan should succeed");

        let report = repair_vectors(
            &paths,
            &VectorRepairQuery {
                provider: None,
                dry_run: false,
            },
        )
        .expect("vector repair should succeed");

        assert!(report.repaired);
        assert_eq!(report.pending_chunks, 1);
        let queue = inspect_vector_queue(&paths, None).expect("queue status should succeed");
        assert_eq!(queue.pending_chunks, 0);
        server.shutdown();
    }

    #[test]
    fn rebuild_vectors_dry_run_reports_full_reindex_scope() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("basic", &vault_root);
        let server = MockEmbeddingServer::spawn();
        write_embedding_config(&vault_root, &server.base_url());
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("full scan should succeed");

        let report = rebuild_vectors(
            &paths,
            &VectorRebuildQuery {
                provider: None,
                dry_run: true,
            },
        )
        .expect("dry-run rebuild should succeed");

        assert!(report.dry_run);
        assert!(report.rebuilt_index);
        assert_eq!(report.indexed, 4);
        server.shutdown();
    }

    #[test]
    fn query_related_notes_groups_neighbors_by_document() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("basic", &vault_root);
        let server = MockEmbeddingServer::spawn();
        write_embedding_config(&vault_root, &server.base_url());
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("full scan should succeed");
        index_vectors(
            &paths,
            &VectorIndexQuery {
                provider: None,
                dry_run: false,
                verbose: false,
            },
        )
        .expect("vector index should succeed");

        let report = query_related_notes(
            &paths,
            &RelatedNotesQuery {
                provider: None,
                note: "Home".to_string(),
                limit: 2,
            },
        )
        .expect("related notes query should succeed");

        assert_eq!(report.note_path, "Home.md");
        assert!(!report.hits.is_empty());
        assert!(report.hits.iter().all(|hit| hit.document_path != "Home.md"));
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

    fn write_embedding_and_extraction_config(vault_root: &Path, base_url: &str) {
        fs::create_dir_all(vault_root.join(".vulcan")).expect("config dir should exist");
        fs::write(
            vault_root.join(".vulcan/config.toml"),
            format!(
                "[embedding]\nprovider = \"openai-compatible\"\nbase_url = \"{base_url}\"\nmodel = \"fixture\"\nmax_batch_size = 8\nmax_concurrency = 1\n\n[extraction]\ncommand = \"sh\"\nargs = [\"-c\", \"cat \\\"$1.txt\\\"\", \"sh\", \"{{path}}\"]\nextensions = [\"pdf\", \"png\"]\nmax_output_bytes = 4096\n"
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

    fn write_attachment_sidecar(vault_root: &Path, relative_path: &str, contents: &str) {
        let path = vault_root.join(relative_path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("sidecar parent should exist");
        }
        fs::write(path, contents).expect("sidecar should write");
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
                        stream
                            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                            .expect("read timeout should be configurable");
                        let Some(request) = read_request_fallible(&mut stream) else {
                            continue;
                        };
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

    fn read_request_fallible(stream: &mut std::net::TcpStream) -> Option<CapturedRequest> {
        let mut buffer = Vec::new();
        let mut header_end = None;

        loop {
            let mut chunk = [0_u8; 1024];
            let bytes_read = match stream.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => n,
                Err(_) => return None,
            };
            buffer.extend_from_slice(&chunk[..bytes_read]);
            if let Some(position) = find_subslice(&buffer, b"\r\n\r\n") {
                header_end = Some(position + 4);
                break;
            }
        }

        let header_end = header_end?;
        let header_text = String::from_utf8(buffer[..header_end].to_vec()).ok()?;
        let content_length = header_text.lines().find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .and_then(|value| value.trim().parse::<usize>().ok())
        })?;
        let mut body_bytes = buffer[header_end..].to_vec();
        while body_bytes.len() < content_length {
            let mut chunk = vec![0_u8; content_length - body_bytes.len()];
            let bytes_read = match stream.read(chunk.as_mut_slice()) {
                Ok(0) => break,
                Ok(n) => n,
                Err(_) => return None,
            };
            body_bytes.extend_from_slice(&chunk[..bytes_read]);
        }

        Some(CapturedRequest {
            body: serde_json::from_slice(&body_bytes).ok()?,
        })
    }

    fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
    }
}
