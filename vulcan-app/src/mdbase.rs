//! Reusable mdbase collection read and journaled write workflows.

mod query_profile;
mod query_session;
pub use query_session::MdbaseQuerySession;
mod write_lifecycle;
mod write_validation;
pub use query_profile::{build_mdbase_query_report_profiled, MdbaseQueryMetrics};

use crate::{plugins, AppError};
use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::SystemTime;
use vulcan_core::mdbase::{
    apply_mdbase_write_transaction_with_control_filter, authorize_mdbase_write_validation_scope,
    build_mdbase_write_preview_with_control_filter, discover_mdbase_files, is_mdbase_record_path,
    load_mdbase_collection, load_mdbase_records_with_contracts_filtered, mdbase_content_revision,
    MdbaseAuthorizedValidationScope, MdbaseCollection, MdbaseConsistentReadGuard,
    MdbaseContractDefinition, MdbaseContractImplementation, MdbaseContractRegistry,
    MdbaseDiagnostic, MdbaseDiagnosticLevel, MdbaseQueryResult, MdbaseRecordDiagnostic,
    MdbaseRecordDocument, MdbaseTypeDefinition, MdbaseTypeRegistry, MdbaseWriteApplyRequest,
    MdbaseWriteAuthorizationRequest, MdbaseWriteOutcome, MdbaseWritePreview,
    MdbaseWritePreviewChangeRequest, MdbaseWritePreviewRequest, MdbaseWritePreviewVerification,
};
use vulcan_core::{
    auto_commit, initialize_vulcan_dir, load_vault_config, resolve_permission_profile,
    AutoCommitReport, ConfigDiagnosticKind, DataviewJsMutationChange, DataviewJsMutationCommitter,
    GitTrigger, PermissionFilter, PermissionGuard, PluginEvent, ProfilePermissionGuard, ScanMode,
    ScanSummary, VaultConfig, VaultPaths,
};

const DEFAULT_WRITE_PREVIEW_TTL_SECONDS: i64 = 300;
const MAX_WRITE_PREVIEW_TTL_SECONDS: i64 = 3_600;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MdbaseWriteOperation {
    Create,
    Update,
    Delete,
    Rename { from: String, to: String },
    Batch,
}

impl MdbaseWriteOperation {
    fn name(&self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Update => "update",
            Self::Delete => "delete",
            Self::Rename { .. } => "rename",
            Self::Batch => "batch",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MdbaseWriteChangeRequest {
    pub path: String,
    /// Exact proposed UTF-8 Markdown, or `None` to delete the path.
    pub after: Option<String>,
    /// Opaque content revision required at the current path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub if_revision: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MdbaseWritePlanRequest {
    pub caller_id: String,
    pub instance_id: String,
    pub operation: MdbaseWriteOperation,
    pub changes: Vec<MdbaseWriteChangeRequest>,
    /// Legacy caller hints; authoritative membership is derived from exact sources.
    pub matched_types: Vec<String>,
    /// Reserved input: generated values are owned by the planner; must be empty.
    pub generated_values: BTreeMap<String, serde_json::Value>,
    pub permission_profile: Option<String>,
    pub ttl_seconds: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MdbaseWritePlanReport {
    pub dry_run: bool,
    pub permission_profile: String,
    pub authorization: MdbaseAuthorizedValidationScope,
    pub preview: MdbaseWritePreview,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<MdbaseRecordDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MdbaseWriteExecutionOptions {
    pub idempotency_key: String,
    pub no_commit: bool,
    pub quiet: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MdbaseWriteApplyReport {
    pub dry_run: bool,
    pub outcome: MdbaseWriteOutcome,
    pub scan: Option<ScanSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_commit: Option<AutoCommitReport>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub follow_up_errors: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MdbaseManagedWriteMode {
    Validated,
    RawRepair,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MdbaseManagedNoteWriteRequest<'a> {
    pub path: &'a str,
    pub before: Option<&'a str>,
    pub after: Option<&'a str>,
    pub operation: MdbaseWriteOperation,
    pub mode: MdbaseManagedWriteMode,
    pub dry_run: bool,
    pub permission_profile: Option<&'a str>,
    pub quiet: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MdbaseManagedNoteWriteChange<'a> {
    pub path: &'a str,
    pub before: Option<&'a str>,
    pub after: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MdbaseManagedNoteWriteBatchRequest<'a> {
    pub changes: &'a [MdbaseManagedNoteWriteChange<'a>],
    pub operation: MdbaseWriteOperation,
    pub mode: MdbaseManagedWriteMode,
    /// Include ordinary Markdown companions in the same cooperating
    /// transaction when at least one changed path is an mdbase record.
    pub allow_mixed_paths: bool,
    pub dry_run: bool,
    pub permission_profile: Option<&'a str>,
    pub quiet: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MdbaseManagedNoteWriteReport {
    pub mode: MdbaseManagedWriteMode,
    pub plan: MdbaseWritePlanReport,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub apply: Option<MdbaseWriteApplyReport>,
    pub diagnostics: Vec<MdbaseRecordDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MdbaseStatusReport {
    pub collection_root: String,
    pub spec_version: String,
    pub records: usize,
    pub types: usize,
    pub contracts: usize,
    pub nested_collections: Vec<String>,
    pub valid: bool,
    pub diagnostics: Vec<MdbaseDiagnostic>,
}

struct MdbaseJsMutationCommitter {
    paths: VaultPaths,
    permission_profile: Option<String>,
    quiet: bool,
}

impl DataviewJsMutationCommitter for MdbaseJsMutationCommitter {
    fn commit(&self, changes: &[DataviewJsMutationChange]) -> Result<bool, String> {
        let operation = match changes {
            [change] => match (&change.before, &change.after) {
                (None, Some(_)) => MdbaseWriteOperation::Create,
                (Some(_), None) => MdbaseWriteOperation::Delete,
                (Some(_), Some(_)) => MdbaseWriteOperation::Update,
                (None, None) => MdbaseWriteOperation::Batch,
            },
            _ => MdbaseWriteOperation::Batch,
        };
        let changes = changes
            .iter()
            .map(|change| MdbaseManagedNoteWriteChange {
                path: &change.path,
                before: change.before.as_deref(),
                after: change.after.as_deref(),
            })
            .collect::<Vec<_>>();
        apply_managed_mdbase_note_writes(
            &self.paths,
            &MdbaseManagedNoteWriteBatchRequest {
                changes: &changes,
                operation,
                mode: MdbaseManagedWriteMode::Validated,
                allow_mixed_paths: true,
                dry_run: false,
                permission_profile: self.permission_profile.as_deref(),
                quiet: self.quiet,
            },
        )
        .map(|report| report.is_some())
        .map_err(|error| error.to_string())
    }
}

#[must_use]
pub fn mdbase_js_mutation_committer(
    paths: &VaultPaths,
    permission_profile: Option<&str>,
    quiet: bool,
) -> Arc<dyn DataviewJsMutationCommitter> {
    Arc::new(MdbaseJsMutationCommitter {
        paths: paths.clone(),
        permission_profile: permission_profile.map(ToOwned::to_owned),
        quiet,
    })
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MdbaseTypesReport {
    pub types: Vec<MdbaseTypeDefinition>,
    pub diagnostics: Vec<MdbaseDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MdbaseContractEntry {
    pub contract: MdbaseContractDefinition,
    pub implementations: Vec<MdbaseContractImplementation>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MdbaseContractsReport {
    pub contracts: Vec<MdbaseContractEntry>,
    pub diagnostics: Vec<MdbaseDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MdbaseValidationRecord {
    pub path: String,
    pub valid: bool,
    pub types: Vec<String>,
    pub diagnostics: Vec<MdbaseDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MdbaseValidateReport {
    pub valid: bool,
    pub records: Vec<MdbaseValidationRecord>,
    pub diagnostics: Vec<MdbaseDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MdbaseReadReport {
    pub valid: bool,
    pub record: MdbaseRecordDocument,
    pub diagnostics: Vec<MdbaseDiagnostic>,
}

struct LoadedCollection {
    read_guard: Option<MdbaseConsistentReadGuard>,
    control_filter: Option<PermissionFilter>,
    collection: MdbaseCollection,
    types: MdbaseTypeRegistry,
    contracts: MdbaseContractRegistry,
}

impl LoadedCollection {
    fn capture_preview(
        &self,
        request: MdbaseWritePreviewRequest,
    ) -> Result<MdbaseWritePreview, AppError> {
        let preview = build_mdbase_write_preview_with_control_filter(
            &self.collection,
            request,
            self.control_filter.as_ref(),
        )
        .map_err(|error| AppError::operation_with_code(error.code, error.message))?;
        let controls = vulcan_core::mdbase::verify_mdbase_control_snapshots(
            &self.collection,
            &self.types,
            &self.contracts,
            self.control_filter.as_ref(),
        )
        .map_err(|error| match error {
            vulcan_core::mdbase::MdbaseRecordCacheError::PermissionDenied => {
                control_permission_denied()
            }
            vulcan_core::mdbase::MdbaseRecordCacheError::StaleControls => {
                AppError::operation_with_code("stale_state", error)
            }
            error => AppError::operation(error),
        })?;
        if preview.control_revisions != controls {
            return Err(AppError::operation_with_code(
                "stale_state",
                "mdbase controls changed while binding the validated snapshots",
            ));
        }
        Ok(preview)
    }
}

pub fn build_mdbase_status_report(
    paths: &VaultPaths,
    filter: Option<&PermissionFilter>,
) -> Result<MdbaseStatusReport, AppError> {
    let loaded = load_collection_authorized(paths, filter)?;
    let discovery = discover_mdbase_files(&loaded.collection).map_err(AppError::operation)?;
    let records = discovery
        .records
        .iter()
        .filter(|path| allowed(filter, path))
        .count();
    let types = loaded
        .types
        .iter()
        .filter(|definition| allowed(filter, &definition.path))
        .count();
    let contracts = loaded
        .contracts
        .iter()
        .filter(|definition| allowed(filter, &definition.path))
        .count();
    let diagnostics = registry_diagnostics(&loaded, filter);
    Ok(MdbaseStatusReport {
        collection_root: loaded.collection.root.to_string_lossy().into_owned(),
        spec_version: loaded.collection.config.spec_version.clone(),
        records,
        types,
        contracts,
        nested_collections: discovery.nested_collections,
        valid: diagnostics
            .iter()
            .all(|diagnostic| diagnostic.severity != MdbaseDiagnosticLevel::Error),
        diagnostics,
    })
}

pub fn build_mdbase_types_report(
    paths: &VaultPaths,
    filter: Option<&PermissionFilter>,
) -> Result<MdbaseTypesReport, AppError> {
    let loaded = load_collection_authorized(paths, filter)?;
    Ok(MdbaseTypesReport {
        types: loaded
            .types
            .iter()
            .filter(|definition| allowed(filter, &definition.path))
            .cloned()
            .collect(),
        diagnostics: loaded
            .types
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.path.is_empty() || allowed(filter, &diagnostic.path))
            .map(MdbaseDiagnostic::from_type)
            .collect(),
    })
}

pub fn build_mdbase_contracts_report(
    paths: &VaultPaths,
    filter: Option<&PermissionFilter>,
) -> Result<MdbaseContractsReport, AppError> {
    let loaded = load_collection_authorized(paths, filter)?;
    let contracts = loaded
        .contracts
        .iter()
        .filter(|contract| allowed(filter, &contract.path))
        .map(|contract| MdbaseContractEntry {
            contract: contract.clone(),
            implementations: loaded
                .contracts
                .implementations(&contract.identity.id, &contract.identity.version)
                .iter()
                .filter(|implementation| allowed(filter, &implementation.type_path))
                .cloned()
                .collect(),
        })
        .collect();
    let diagnostics = loaded
        .contracts
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.path.is_empty() || allowed(filter, &diagnostic.path))
        .map(MdbaseDiagnostic::from_contract)
        .collect();
    Ok(MdbaseContractsReport {
        contracts,
        diagnostics,
    })
}

pub fn build_mdbase_validate_report(
    paths: &VaultPaths,
    path: Option<&str>,
    filter: Option<&PermissionFilter>,
) -> Result<MdbaseValidateReport, AppError> {
    let loaded = load_collection_authorized(paths, filter)?;
    if let Some(path) = path {
        ensure_allowed(filter, path)?;
    }
    let records = load_mdbase_records_with_contracts_filtered(
        &loaded.collection,
        &loaded.types,
        &loaded.contracts,
        false,
        filter,
    )
    .map_err(AppError::operation)?;
    let records = records
        .records
        .into_iter()
        .filter(|record| match path {
            Some(path) => record.path == path,
            None => true,
        })
        .map(|record| {
            let valid = record.is_valid();
            MdbaseValidationRecord {
                path: record.path,
                valid,
                types: record.types,
                diagnostics: record
                    .diagnostics
                    .iter()
                    .map(MdbaseDiagnostic::from_record)
                    .collect(),
            }
        })
        .collect::<Vec<_>>();
    if path.is_some() && records.is_empty() {
        return Err(AppError::operation("mdbase record was not found"));
    }
    let diagnostics = registry_diagnostics(&loaded, filter);
    let valid = diagnostics
        .iter()
        .all(|diagnostic| diagnostic.severity != MdbaseDiagnosticLevel::Error)
        && records.iter().all(|record| record.valid);
    Ok(MdbaseValidateReport {
        valid,
        records,
        diagnostics,
    })
}

pub fn build_mdbase_read_report(
    paths: &VaultPaths,
    path: &str,
    include_source: bool,
    filter: Option<&PermissionFilter>,
) -> Result<MdbaseReadReport, AppError> {
    ensure_allowed(filter, path)?;
    let loaded = load_collection_authorized(paths, filter)?;
    // Use the filtered collection read so cross-record validation and contract
    // projections have the same visibility semantics as `validate`.
    let records = load_mdbase_records_with_contracts_filtered(
        &loaded.collection,
        &loaded.types,
        &loaded.contracts,
        include_source,
        filter,
    )
    .map_err(AppError::operation)?;
    let record = if let Some(derived) = records.get(path) {
        derived.clone()
    } else {
        return Err(AppError::operation("mdbase record was not found"));
    };
    let mut diagnostics = registry_diagnostics(&loaded, filter);
    diagnostics.extend(record.diagnostics.iter().map(MdbaseDiagnostic::from_record));
    let valid = diagnostics
        .iter()
        .all(|diagnostic| diagnostic.severity != MdbaseDiagnosticLevel::Error);
    Ok(MdbaseReadReport {
        valid,
        record,
        diagnostics,
    })
}

/// Execute a canonical mdbase query over the records visible to the caller.
/// Record source is loaded for `file.body` evaluation, but is returned only
/// when the query explicitly opts into `include_body`.
pub fn build_mdbase_query_report(
    paths: &VaultPaths,
    query: &serde_json::Value,
    filter: Option<&PermissionFilter>,
) -> Result<MdbaseQueryResult, AppError> {
    build_mdbase_query_report_profiled(paths, query, filter, &mut MdbaseQueryMetrics::default())
}

fn load_query_records(
    paths: &VaultPaths,
    loaded: &LoadedCollection,
    filter: Option<&PermissionFilter>,
    metrics: &mut MdbaseQueryMetrics,
    query: &vulcan_core::mdbase::MdbasePreparedQuery,
) -> Result<vulcan_core::mdbase::MdbaseQuerySnapshot, AppError> {
    // The shared cache dependency digest includes the lockfile, while ordinary
    // source queries do not consume it. Cache reuse must not broaden required
    // authority or probe an unreadable lockfile (including its absence).
    if filter.is_some_and(|filter| !filter.is_allowed(vulcan_core::mdbase::MDBASE_LOCK_FILE_NAME)) {
        return load_query_source_records(loaded, filter, metrics);
    }
    load_query_records_with_boundary(paths, loaded, filter, metrics, query, || {})
}

/// Run the indexed query path when every visible record is proven current by
/// stat fingerprint and the plan is physically executable. Any miss or cache
/// failure returns `None`; the caller then runs the ordinary disk-reconciled
/// path, which reports canonical errors. Read-only: never refreshes the cache.
fn try_indexed_query(
    paths: &VaultPaths,
    loaded: &LoadedCollection,
    filter: Option<&PermissionFilter>,
    query: &vulcan_core::mdbase::MdbasePreparedQuery,
    now: chrono::DateTime<chrono::Utc>,
    metrics: &mut MdbaseQueryMetrics,
) -> Option<MdbaseQueryResult> {
    if !indexed_query_allowed(filter) {
        return None;
    }
    let connection = open_query_cache(paths)?;
    try_indexed_query_with(&connection, loaded, filter, query, now, metrics)
}

/// Incrementally refresh an initialized cache for an unrestricted reader.
/// Restricted readers never publish cache rows. Returns whether rows were
/// refreshed; failures leave the ordinary path to reconcile from sources.
fn refresh_query_cache(
    paths: &VaultPaths,
    loaded: &LoadedCollection,
    filter: Option<&PermissionFilter>,
    metrics: &mut MdbaseQueryMetrics,
) -> bool {
    if filter.is_some_and(|filter| !filter.path_permission().is_unrestricted())
        || !paths.cache_db().exists()
    {
        return false;
    }
    let Ok(mut database) = vulcan_core::CacheDatabase::open(paths) else {
        return false;
    };
    metrics.cache_refresh_attempts += 1;
    let start = std::time::Instant::now();
    let refreshed = vulcan_core::mdbase::refresh_mdbase_record_cache(
        &mut database,
        &loaded.collection,
        &loaded.types,
        &loaded.contracts,
    );
    metrics.cache_refresh_seconds += start.elapsed().as_secs_f64();
    refreshed.is_ok()
}

/// Same lockfile rule as cached loads: no cache use without that authority.
fn indexed_query_allowed(filter: Option<&PermissionFilter>) -> bool {
    filter.is_none_or(|filter| filter.is_allowed(vulcan_core::mdbase::MDBASE_LOCK_FILE_NAME))
}

fn open_query_cache(paths: &VaultPaths) -> Option<rusqlite::Connection> {
    rusqlite::Connection::open_with_flags(
        paths.cache_db(),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()
}

fn try_indexed_query_with(
    connection: &rusqlite::Connection,
    loaded: &LoadedCollection,
    filter: Option<&PermissionFilter>,
    query: &vulcan_core::mdbase::MdbasePreparedQuery,
    now: chrono::DateTime<chrono::Utc>,
    metrics: &mut MdbaseQueryMetrics,
) -> Option<MdbaseQueryResult> {
    metrics.indexed_attempts += 1;
    let result = vulcan_core::mdbase::execute_indexed_mdbase_query(
        connection,
        &loaded.collection,
        &loaded.types,
        &loaded.contracts,
        query,
        filter,
        now,
        &mut metrics.indexed,
    )
    .ok()??;
    metrics.indexed_hits += 1;
    metrics.prepared_visible_records = metrics.indexed.visible_records;
    Some(result)
}

fn load_query_source_records(
    loaded: &LoadedCollection,
    filter: Option<&PermissionFilter>,
    metrics: &mut MdbaseQueryMetrics,
) -> Result<vulcan_core::mdbase::MdbaseQuerySnapshot, AppError> {
    metrics.source_loads += 1;
    query_profile::time(&mut metrics.source_fallback_seconds, || {
        load_mdbase_records_with_contracts_filtered(
            &loaded.collection,
            &loaded.types,
            &loaded.contracts,
            false,
            filter,
        )
    })
    .map(vulcan_core::mdbase::MdbaseQuerySnapshot::from_records)
    .map_err(AppError::operation)
}

fn query_cache_error(error: vulcan_core::mdbase::MdbaseRecordCacheError) -> AppError {
    use vulcan_core::mdbase::MdbaseRecordCacheError;
    match error {
        MdbaseRecordCacheError::StaleRecords | MdbaseRecordCacheError::StaleControls => {
            AppError::operation_with_code("stale_state", error.to_string())
        }
        MdbaseRecordCacheError::PermissionDenied => control_permission_denied(),
        _ => AppError::operation(error),
    }
}

#[allow(clippy::too_many_lines)]
fn load_query_records_with_boundary(
    paths: &VaultPaths,
    loaded: &LoadedCollection,
    filter: Option<&PermissionFilter>,
    metrics: &mut MdbaseQueryMetrics,
    query: &vulcan_core::mdbase::MdbasePreparedQuery,
    before_verification: impl FnOnce(),
) -> Result<vulcan_core::mdbase::MdbaseQuerySnapshot, AppError> {
    use vulcan_core::mdbase::{
        backfill_mdbase_stat_fingerprints, capture_cached_mdbase_record_manifest,
        capture_mdbase_record_manifest_with_fingerprints,
        load_cached_mdbase_query_snapshot_profiled, rebuild_mdbase_record_cache,
        refresh_mdbase_record_cache, MdbaseRecordCacheError,
    };
    let unrestricted = filter.is_none_or(|filter| filter.path_permission().is_unrestricted());
    // An unavailable disposable cache must not make canonical records unreadable.
    // Restricted callers do not create, refresh, or repair unrestricted rows.
    let mut database = query_profile::time(&mut metrics.cache_open_seconds, || {
        if unrestricted {
            vulcan_core::CacheDatabase::open(paths).ok()
        } else {
            None
        }
    });
    let readonly = query_profile::time(&mut metrics.cache_open_seconds, || {
        if unrestricted {
            None
        } else {
            rusqlite::Connection::open_with_flags(
                paths.cache_db(),
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .ok()
        }
    });
    // Prefer stat-fingerprint proof against cached revisions; any miss or cache
    // failure (including an older read-only schema) falls back to hashing every
    // visible record. Staleness and denials are never treated as misses.
    let capture = |connection: Option<&rusqlite::Connection>,
                   metrics: &mut MdbaseQueryMetrics,
                   seconds: fn(&mut MdbaseQueryMetrics) -> &mut f64|
     -> Result<_, AppError> {
        let start = std::time::Instant::now();
        let cached = connection.map(|connection| {
            capture_cached_mdbase_record_manifest(
                connection,
                &loaded.collection,
                &loaded.types,
                &loaded.contracts,
                filter,
            )
        });
        let manifest = match cached {
            Some(Ok(Some(manifest))) => {
                metrics.stat_manifests += 1;
                Ok((manifest, None))
            }
            Some(Err(
                error @ (MdbaseRecordCacheError::StaleRecords
                | MdbaseRecordCacheError::StaleControls
                | MdbaseRecordCacheError::PermissionDenied),
            )) => Err(error),
            _ => capture_mdbase_record_manifest_with_fingerprints(
                &loaded.collection,
                &loaded.types,
                &loaded.contracts,
                filter,
            )
            .map(|(manifest, fingerprints)| (manifest, Some(fingerprints))),
        };
        *seconds(metrics) += start.elapsed().as_secs_f64();
        manifest.map_err(query_cache_error)
    };
    let connection = database
        .as_ref()
        .map(vulcan_core::CacheDatabase::connection)
        .or(readonly.as_ref());
    let (manifest, _) = capture(connection, metrics, |metrics| {
        &mut metrics.manifest_before_seconds
    })?;
    metrics.completed_manifests += 1;
    metrics.completed_manifest_records += manifest.len();
    metrics.completed_manifest_bytes += manifest.values().map(|entry| entry.file.size).sum::<u64>();
    let cached = |connection: &rusqlite::Connection, metrics: &mut MdbaseQueryMetrics| {
        metrics.cache_attempts += 1;
        let records = load_cached_mdbase_query_snapshot_profiled(
            connection,
            &loaded.collection,
            &loaded.types,
            &loaded.contracts,
            &manifest,
            filter,
            query,
            &mut metrics.cached_load,
        )
        .ok()
        .flatten();
        metrics.cache_hits += usize::from(records.is_some());
        records
    };
    let mut records = if unrestricted {
        database
            .as_ref()
            .and_then(|database| cached(database.connection(), metrics))
    } else {
        readonly
            .as_ref()
            .and_then(|connection| cached(connection, metrics))
    };
    if records.is_none() && unrestricted {
        if let Some(database) = database.as_mut() {
            // Rebuild discards corrupt derived JSON, never canonical source.
            metrics.cache_refresh_attempts += 1;
            let refresh_start = std::time::Instant::now();
            let refreshed = refresh_mdbase_record_cache(
                database,
                &loaded.collection,
                &loaded.types,
                &loaded.contracts,
            )
            .or_else(|_| {
                metrics.cache_rebuild_attempts += 1;
                rebuild_mdbase_record_cache(
                    database,
                    &loaded.collection,
                    &loaded.types,
                    &loaded.contracts,
                )
            });
            metrics.cache_refresh_seconds += refresh_start.elapsed().as_secs_f64();
            if refreshed.is_ok() {
                records = cached(database.connection(), metrics);
            }
        }
    }
    let records = records.map_or_else(|| load_query_source_records(loaded, filter, metrics), Ok)?;
    before_verification();
    let connection = database
        .as_ref()
        .map(vulcan_core::CacheDatabase::connection)
        .or(readonly.as_ref());
    let (current, fingerprints) = capture(connection, metrics, |metrics| {
        &mut metrics.manifest_after_seconds
    })?;
    metrics.completed_manifests += 1;
    metrics.completed_manifest_records += current.len();
    metrics.completed_manifest_bytes += current.values().map(|entry| entry.file.size).sum::<u64>();
    if current != manifest
        || records.records().records.len() != manifest.len()
        || records.records().records.iter().any(|record| {
            manifest.get(&record.path).is_none_or(|expected| {
                expected.revision != record.revision || expected.file != record.file
            })
        })
    {
        return Err(AppError::operation_with_code(
            "stale_state",
            "mdbase records changed during query preparation; retry the query",
        ));
    }
    // Content capture proved these revisions; record their fingerprints so the
    // next read can prove freshness by stat. Disposable-cache failures are
    // harmless here and must not fail a verified read.
    if let (Some(database), Some(fingerprints)) = (database.as_mut(), fingerprints) {
        let _ = backfill_mdbase_stat_fingerprints(
            database,
            &loaded.collection,
            &current,
            &fingerprints,
        );
    }
    Ok(records)
}

pub fn parse_mdbase_query(source: &str) -> Result<serde_json::Value, AppError> {
    let yaml = serde_yaml::from_str::<serde_yaml::Value>(source)
        .map_err(|error| AppError::operation(format!("invalid query YAML or JSON: {error}")))?;
    serde_json::to_value(yaml).map_err(AppError::operation)
}

/// Build an immutable, authorization-bound dry-run for an mdbase mutation.
///
/// Planning reads exact before-images but performs no canonical or cache
/// mutation and dispatches no plugin lifecycle events.
pub fn plan_mdbase_write(
    paths: &VaultPaths,
    request: &MdbaseWritePlanRequest,
    now: DateTime<Utc>,
) -> Result<MdbaseWritePlanReport, AppError> {
    plan_mdbase_write_in_mode(paths, request, now, MdbaseManagedWriteMode::Validated)
}

fn plan_mdbase_write_in_mode(
    paths: &VaultPaths,
    request: &MdbaseWritePlanRequest,
    now: DateTime<Utc>,
    mode: MdbaseManagedWriteMode,
) -> Result<MdbaseWritePlanReport, AppError> {
    validate_plan_request(request)?;
    let selection = resolve_permission_profile(paths, request.permission_profile.as_deref())
        .map_err(AppError::operation)?;
    let guard = ProfilePermissionGuard::new(paths, selection);
    let filter = write_control_filter(&guard)?;
    let mut loaded = load_collection_authorized(paths, Some(&filter))?;
    for type_name in &request.matched_types {
        if loaded.types.get(type_name).is_none() {
            return Err(AppError::operation(format!(
                "unknown mdbase type in proposed draft: {type_name}"
            )));
        }
    }
    let config = load_write_config(paths)?;
    let affected_paths = request
        .changes
        .iter()
        .map(|change| change.path.clone())
        .collect::<Vec<_>>();
    authorize_affected_paths(&guard, &affected_paths)?;
    let ttl = request
        .ttl_seconds
        .unwrap_or(DEFAULT_WRITE_PREVIEW_TTL_SECONDS);
    let mut preview_request = MdbaseWritePreviewRequest {
        plan_id: ulid::Ulid::new().to_string().to_lowercase(),
        caller_id: request.caller_id.clone(),
        instance_id: request.instance_id.clone(),
        operation: request.operation.name().to_string(),
        issued_at: now,
        expires_at: now + TimeDelta::seconds(ttl),
        permission_revision: permission_revision(guard.selection())?,
        config_revision: config_revision(&config)?,
        changes: request
            .changes
            .iter()
            .map(|change| MdbaseWritePreviewChangeRequest {
                path: change.path.clone(),
                after: change.after.clone(),
                if_revision: change.if_revision.clone(),
            })
            .collect(),
        matched_types: Vec::new(),
        relevant_record_namespaces: Vec::new(),
        generated_values: request.generated_values.clone(),
    };
    let initial = loaded.capture_preview(preview_request.clone())?;
    validate_operation_shape(&request.operation, &initial)?;
    write_validation::reload_controls(&mut loaded)?;
    let clock = vulcan_core::mdbase::MdbaseCelClock::new(
        now,
        loaded
            .collection
            .config
            .settings
            .timezone
            .as_deref()
            .unwrap_or("UTC"),
    )
    .map_err(AppError::operation)?;
    let matched_types = write_validation::affected_membership(&loaded, &initial, &clock)?;
    let authorization = authorize_mdbase_write_validation_scope(
        &loaded.collection,
        &loaded.types,
        "",
        &MdbaseWriteAuthorizationRequest {
            read_paths: affected_paths.clone(),
            write_paths: affected_paths,
            matched_types: matched_types.clone(),
        },
        &guard,
    )
    .map_err(|error| AppError::operation_with_code(error.code, error.message))?;
    preview_request.matched_types = matched_types;
    preview_request
        .relevant_record_namespaces
        .clone_from(&authorization.collection_record_namespaces);
    let preview = loaded.capture_preview(preview_request.clone())?;
    write_validation::check_snapshot_stability(&initial, &preview)?;
    let preview = write_lifecycle::prepare_preview(
        &loaded,
        preview,
        preview_request,
        &request.operation,
        &clock,
        mode,
    )?;
    let diagnostics = write_validation::validate_final_state(&loaded, &preview, &clock, mode)?;
    Ok(MdbaseWritePlanReport {
        dry_run: true,
        permission_profile: guard.selection().name.clone(),
        authorization,
        preview,
        diagnostics,
    })
}

fn authorize_affected_paths(
    guard: &ProfilePermissionGuard,
    affected_paths: &[String],
) -> Result<(), AppError> {
    // Before-images are part of every reviewed plan, including absence
    // preconditions for creates, so affected paths require both capabilities.
    for path in affected_paths {
        guard
            .check_read_path(path)
            .and_then(|()| guard.check_write_path(path))
            .map_err(|_| {
                AppError::operation_with_code(
                    "permission_denied",
                    "permission denied for mdbase affected paths",
                )
            })?;
    }
    Ok(())
}

fn write_control_filter(guard: &ProfilePermissionGuard) -> Result<PermissionFilter, AppError> {
    // Dynamic hooks cannot currently prove complete control-namespace coverage.
    // Do not fall back to a static ceiling that would bypass those decisions.
    let filter = guard.read_filter();
    if guard.has_policy_hook()
        || !filter.is_allowed("mdbase.yaml")
        || !filter.is_allowed(vulcan_core::mdbase::MDBASE_LOCK_FILE_NAME)
    {
        return Err(control_permission_denied());
    }
    Ok(filter)
}

/// Apply an exact reviewed plan through the crash-safe mdbase transaction.
/// Cache refresh is part of consistency; plugin delivery and opt-in Git occur
/// only after the canonical transaction commits and are never repeated for an
/// idempotent replay.
pub fn apply_mdbase_write(
    paths: &VaultPaths,
    plan: &MdbaseWritePlanReport,
    options: &MdbaseWriteExecutionOptions,
    now: DateTime<Utc>,
) -> Result<MdbaseWriteApplyReport, AppError> {
    if !plan.dry_run {
        return Err(AppError::operation("invalid mdbase write plan"));
    }
    let selection = resolve_permission_profile(paths, Some(&plan.permission_profile))
        .map_err(AppError::operation)?;
    let guard = ProfilePermissionGuard::new(paths, selection);
    let filter = write_control_filter(&guard)?;
    let mut loaded = load_collection_authorized(paths, Some(&filter))?;
    let affected_paths = plan
        .preview
        .changes
        .iter()
        .map(|change| change.path.clone())
        .collect::<Vec<_>>();
    let authorization = authorize_mdbase_write_validation_scope(
        &loaded.collection,
        &loaded.types,
        "",
        &MdbaseWriteAuthorizationRequest {
            read_paths: affected_paths.clone(),
            write_paths: affected_paths,
            matched_types: plan.preview.matched_types.clone(),
        },
        &guard,
    )
    .map_err(|error| AppError::operation_with_code(error.code, error.message))?;
    if authorization != plan.authorization {
        return Err(AppError::operation(
            "mdbase write authorization changed; create and review a new preview",
        ));
    }

    let config = load_write_config(paths)?;
    let should_commit =
        !options.no_commit && config.git.auto_commit && config.git.trigger == GitTrigger::Mutation;
    if should_commit {
        guard.check_git().map_err(AppError::operation)?;
    }
    let permission_revision = permission_revision(guard.selection())?;
    let config_revision = config_revision(&config)?;
    let profile = plan.permission_profile.clone();
    let plugin_payload = write_plugin_payload(&plan.preview);
    let quiet = options.quiet;
    let mut scan = None;
    // The consistent-read guard must be released before the transaction takes
    // the exclusive vault lock. Control/type data remains immutable-plan input
    // and is reverified by the transaction itself.
    drop(loaded.read_guard.take());
    initialize_vulcan_dir(paths).map_err(AppError::operation)?;
    let apply_request = MdbaseWriteApplyRequest {
        preview: &plan.preview,
        verification: MdbaseWritePreviewVerification {
            caller_id: &plan.preview.caller_id,
            instance_id: &plan.preview.instance_id,
            operation: &plan.preview.operation,
            permission_revision: &permission_revision,
            config_revision: &config_revision,
            now,
        },
        idempotency_key: &options.idempotency_key,
    };
    let outcome = apply_mdbase_write_transaction_with_control_filter(
        paths,
        &loaded.collection,
        &apply_request,
        Some(&filter),
        || {
            plugins::dispatch_plugin_event(
                paths,
                Some(&profile),
                PluginEvent::OnNoteWrite,
                &plugin_payload,
                quiet,
            )
            .map_err(|error| error.to_string())
        },
        |_| {
            let summary = vulcan_core::scan::scan_vault_unlocked(paths, ScanMode::Incremental)
                .map_err(|error| error.to_string())?;
            scan = Some(summary);
            Ok(())
        },
    )
    .map_err(|error| AppError::operation_with_code(error.code, error.message))?;

    let mut report = MdbaseWriteApplyReport {
        dry_run: false,
        outcome,
        scan,
        auto_commit: None,
        follow_up_errors: Vec::new(),
    };
    if report.outcome.replayed {
        return Ok(report);
    }
    dispatch_committed_path_events(paths, plan, options.quiet);
    if should_commit && report.outcome.follow_up_error.is_none() {
        apply_auto_commit(paths, plan, &config.git, options, &mut report);
    }
    Ok(report)
}

/// Route one generic note mutation through mdbase when its old or proposed
/// path belongs to the collection. `None` means the path is ordinary Markdown
/// and the caller should use its existing mutation workflow.
pub fn apply_managed_mdbase_note_write(
    paths: &VaultPaths,
    request: &MdbaseManagedNoteWriteRequest<'_>,
) -> Result<Option<MdbaseManagedNoteWriteReport>, AppError> {
    let changes = [MdbaseManagedNoteWriteChange {
        path: request.path,
        before: request.before,
        after: request.after,
    }];
    apply_managed_mdbase_note_writes(
        paths,
        &MdbaseManagedNoteWriteBatchRequest {
            changes: &changes,
            operation: request.operation.clone(),
            mode: request.mode,
            allow_mixed_paths: false,
            dry_run: request.dry_run,
            permission_profile: request.permission_profile,
            quiet: request.quiet,
        },
    )
}

/// Route a set of generic note mutations through one mdbase transaction.
/// `None` means none of the paths are collection records. Mixed managed and
/// ordinary Markdown paths are rejected unless the caller explicitly opts into
/// a cooperating transaction; renames may always cross the collection boundary.
/// The complete write stays crash-safe, while validation only governs endpoints
/// that are mdbase records.
pub fn apply_managed_mdbase_note_writes(
    paths: &VaultPaths,
    request: &MdbaseManagedNoteWriteBatchRequest<'_>,
) -> Result<Option<MdbaseManagedNoteWriteReport>, AppError> {
    if request.changes.is_empty() {
        return Ok(None);
    }
    let selection = resolve_permission_profile(paths, request.permission_profile)
        .map_err(AppError::operation)?;
    let guard = ProfilePermissionGuard::new(paths, selection);
    let Some(collection) = load_mdbase_routing_collection(paths, &guard)? else {
        return Ok(None);
    };
    let managed = request
        .changes
        .iter()
        .map(|change| is_mdbase_record_path(&collection, change.path))
        .collect::<Result<Vec<_>, _>>()
        .map_err(AppError::operation)?;
    if managed.iter().all(|managed| !managed) {
        return Ok(None);
    }
    let boundary_rename = matches!(request.operation, MdbaseWriteOperation::Rename { .. });
    if managed.iter().any(|managed| !managed) && !boundary_rename && !request.allow_mixed_paths {
        return Err(AppError::operation(
            "managed mdbase write batches cannot mix collection records with ordinary Markdown paths",
        ));
    }
    // Prove affected-path authority before inspecting record-dependent type
    // membership. The full constraint scope is proved by plan_mdbase_write.
    for change in request.changes {
        guard
            .check_read_path(change.path)
            .and_then(|()| guard.check_write_path(change.path))
            .map_err(AppError::operation)?;
    }

    let now = DateTime::<Utc>::from(SystemTime::now());
    let plan = plan_mdbase_write_in_mode(
        paths,
        &MdbaseWritePlanRequest {
            caller_id: "vulcan-app.managed-write".to_string(),
            instance_id: ulid::Ulid::new().to_string().to_lowercase(),
            operation: request.operation.clone(),
            changes: request
                .changes
                .iter()
                .map(|change| MdbaseWriteChangeRequest {
                    path: change.path.to_string(),
                    after: change.after.map(str::to_string),
                    if_revision: change.before.map(mdbase_content_revision),
                })
                .collect(),
            matched_types: Vec::new(),
            generated_values: BTreeMap::new(),
            permission_profile: request.permission_profile.map(str::to_string),
            ttl_seconds: None,
        },
        now,
        request.mode,
    )?;
    let apply = if request.dry_run {
        None
    } else {
        Some(apply_mdbase_write(
            paths,
            &plan,
            &MdbaseWriteExecutionOptions {
                idempotency_key: ulid::Ulid::new().to_string().to_lowercase(),
                // Existing command adapters retain their established
                // auto-commit boundary and changed-path aggregation.
                no_commit: true,
                quiet: request.quiet,
            },
            DateTime::<Utc>::from(SystemTime::now()),
        )?)
    };
    Ok(Some(MdbaseManagedNoteWriteReport {
        mode: request.mode,
        diagnostics: plan.diagnostics.clone(),
        plan,
        apply,
    }))
}

fn validation_error_summary(diagnostics: &[MdbaseRecordDiagnostic]) -> String {
    diagnostics
        .iter()
        .filter(|diagnostic| {
            diagnostic.severity == vulcan_core::mdbase::MdbaseRecordDiagnosticSeverity::Error
        })
        .take(3)
        .map(|diagnostic| format!("{}: {}", diagnostic.code, diagnostic.message))
        .collect::<Vec<_>>()
        .join("; ")
}

fn validate_plan_request(request: &MdbaseWritePlanRequest) -> Result<(), AppError> {
    if !request.generated_values.is_empty() {
        return Err(AppError::operation_with_code(
            "invalid_input",
            "mdbase generated values must be produced by the write planner",
        ));
    }
    let ttl = request
        .ttl_seconds
        .unwrap_or(DEFAULT_WRITE_PREVIEW_TTL_SECONDS);
    if !(1..=MAX_WRITE_PREVIEW_TTL_SECONDS).contains(&ttl) {
        return Err(AppError::operation(
            "mdbase write preview TTL must be between 1 and 3600 seconds",
        ));
    }
    if request.caller_id.trim().is_empty() || request.instance_id.trim().is_empty() {
        return Err(AppError::operation(
            "mdbase writes require non-empty caller and instance identifiers",
        ));
    }
    Ok(())
}

fn validate_operation_shape(
    operation: &MdbaseWriteOperation,
    preview: &MdbaseWritePreview,
) -> Result<(), AppError> {
    let valid = match operation {
        MdbaseWriteOperation::Create => {
            preview.changes.len() == 1
                && preview.changes[0].before.is_none()
                && preview.changes[0].after.is_some()
        }
        MdbaseWriteOperation::Update => {
            preview.changes.len() == 1
                && preview.changes[0].before.is_some()
                && preview.changes[0].after.is_some()
        }
        MdbaseWriteOperation::Delete => {
            preview.changes.len() == 1
                && preview.changes[0].before.is_some()
                && preview.changes[0].after.is_none()
        }
        MdbaseWriteOperation::Rename { from, to } => {
            from != to
                && preview.changes.iter().any(|change| {
                    change.path == *from && change.before.is_some() && change.after.is_none()
                })
                && preview.changes.iter().any(|change| {
                    change.path == *to && change.before.is_none() && change.after.is_some()
                })
        }
        MdbaseWriteOperation::Batch => !preview.changes.is_empty(),
    };
    if valid {
        Ok(())
    } else {
        Err(AppError::operation(format!(
            "mdbase {} request does not match the observed before/after state",
            operation.name()
        )))
    }
}

fn permission_revision(
    selection: &vulcan_core::ResolvedPermissionProfile,
) -> Result<String, AppError> {
    revision("permission", selection)
}

fn config_revision(config: &VaultConfig) -> Result<String, AppError> {
    revision("config", config)
}

fn load_write_config(paths: &VaultPaths) -> Result<VaultConfig, AppError> {
    let loaded = load_vault_config(paths);
    if let Some(diagnostic) = loaded
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.kind == ConfigDiagnosticKind::ParseFailure)
    {
        return Err(AppError::operation(format!(
            "cannot plan or apply an mdbase write with invalid configuration at {}: {}",
            diagnostic.path.display(),
            diagnostic.message
        )));
    }
    Ok(loaded.config)
}

fn revision(label: &str, value: &impl Serialize) -> Result<String, AppError> {
    let bytes = serde_json::to_vec(value).map_err(AppError::operation)?;
    Ok(format!("{label}:sha256:{:x}", Sha256::digest(bytes)))
}

fn write_plugin_payload(preview: &MdbaseWritePreview) -> serde_json::Value {
    json!({
        "kind": PluginEvent::OnNoteWrite,
        "operation": preview.operation,
        "plan_id": preview.plan_id,
        "changes": preview.changes,
    })
}

fn dispatch_committed_path_events(paths: &VaultPaths, plan: &MdbaseWritePlanReport, quiet: bool) {
    for change in &plan.preview.changes {
        let event = match (&change.before, &change.after) {
            (None, Some(_)) => Some(PluginEvent::OnNoteCreate),
            (Some(_), None) => Some(PluginEvent::OnNoteDelete),
            _ => None,
        };
        if let Some(event) = event {
            let _ = plugins::dispatch_plugin_event(
                paths,
                Some(&plan.permission_profile),
                event,
                &json!({
                    "kind": event,
                    "operation": plan.preview.operation,
                    "plan_id": plan.preview.plan_id,
                    "path": change.path,
                    "content": change.after,
                }),
                quiet,
            );
        }
    }
}

fn apply_auto_commit(
    paths: &VaultPaths,
    plan: &MdbaseWritePlanReport,
    git: &vulcan_core::GitConfig,
    options: &MdbaseWriteExecutionOptions,
    report: &mut MdbaseWriteApplyReport,
) {
    let changed_paths = plan
        .preview
        .changes
        .iter()
        .map(|change| change.path.clone())
        .collect::<Vec<_>>();
    let payload = json!({
        "kind": PluginEvent::OnPreCommit,
        "operation": plan.preview.operation,
        "plan_id": plan.preview.plan_id,
        "files": changed_paths,
    });
    if let Err(error) = plugins::dispatch_plugin_event(
        paths,
        Some(&plan.permission_profile),
        PluginEvent::OnPreCommit,
        &payload,
        options.quiet,
    ) {
        report
            .follow_up_errors
            .push(format!("auto-commit preflight failed: {error}"));
        return;
    }
    match auto_commit(
        paths.vault_root(),
        git,
        &format!("mdbase {}", plan.preview.operation),
        &changed_paths,
    ) {
        Ok(commit) => {
            let post_payload = json!({
                "kind": PluginEvent::OnPostCommit,
                "operation": plan.preview.operation,
                "plan_id": plan.preview.plan_id,
                "commit": commit,
            });
            let _ = plugins::dispatch_plugin_event(
                paths,
                Some(&plan.permission_profile),
                PluginEvent::OnPostCommit,
                &post_payload,
                options.quiet,
            );
            report.auto_commit = Some(commit);
        }
        Err(error) => report
            .follow_up_errors
            .push(format!("auto-commit failed: {error}")),
    }
}

#[cfg(test)]
fn load_collection(paths: &VaultPaths) -> Result<LoadedCollection, AppError> {
    load_collection_authorized(paths, None)
}

fn load_collection_authorized(
    paths: &VaultPaths,
    filter: Option<&PermissionFilter>,
) -> Result<LoadedCollection, AppError> {
    if !allowed(filter, "mdbase.yaml") {
        return Err(control_permission_denied());
    }
    let read_guard =
        vulcan_core::mdbase::acquire_mdbase_consistent_read(paths).map_err(AppError::operation)?;
    let collection = load_mdbase_collection(paths.vault_root())
        .map_err(AppError::operation)?
        .ok_or_else(|| AppError::operation("not an mdbase collection: missing mdbase.yaml"))?;
    let (types, contracts) = load_control_registries(&collection, filter)?;
    Ok(LoadedCollection {
        read_guard,
        control_filter: filter.cloned(),
        collection,
        types,
        contracts,
    })
}

fn load_control_registries(
    collection: &MdbaseCollection,
    filter: Option<&PermissionFilter>,
) -> Result<(MdbaseTypeRegistry, MdbaseContractRegistry), AppError> {
    let types = vulcan_core::mdbase::load_mdbase_type_registry_authorized(collection, filter)
        .map_err(|error| match error {
            vulcan_core::mdbase::MdbaseTypeRegistryError::PermissionDenied => {
                control_permission_denied()
            }
            error => AppError::operation(error),
        })?;
    let contracts =
        vulcan_core::mdbase::load_mdbase_contract_registry_authorized(collection, &types, filter)
            .map_err(|error| match error {
            vulcan_core::mdbase::MdbaseContractRegistryError::PermissionDenied => {
                control_permission_denied()
            }
            error => AppError::operation(error),
        })?;
    Ok((types, contracts))
}

/// Authorize the routing control before observing even its absence. This does
/// not load registries or prove write-integrity scope; managed planning does so
/// after classification. Callers must retain their own authority, not substitute
/// the vault's default profile for an already scoped request.
pub(crate) fn load_mdbase_routing_collection(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
) -> Result<Option<MdbaseCollection>, AppError> {
    if guard.has_policy_hook() || !guard.read_filter().is_allowed("mdbase.yaml") {
        return Err(control_permission_denied());
    }
    load_mdbase_collection(paths.vault_root()).map_err(AppError::operation)
}

fn control_permission_denied() -> AppError {
    AppError::operation_with_code(
        "permission_denied",
        "permission denied for required mdbase controls",
    )
}

fn registry_diagnostics(
    loaded: &LoadedCollection,
    filter: Option<&PermissionFilter>,
) -> Vec<MdbaseDiagnostic> {
    let mut diagnostics = loaded
        .collection
        .diagnostics
        .iter()
        .map(MdbaseDiagnostic::from_config)
        .collect::<Vec<_>>();
    diagnostics.extend(
        loaded
            .types
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.path.is_empty() || allowed(filter, &diagnostic.path))
            .map(MdbaseDiagnostic::from_type),
    );
    diagnostics.extend(
        loaded
            .contracts
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.path.is_empty() || allowed(filter, &diagnostic.path))
            .map(MdbaseDiagnostic::from_contract),
    );
    diagnostics.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.field.cmp(&right.field))
            .then_with(|| left.code.cmp(&right.code))
            .then_with(|| left.message.cmp(&right.message))
    });
    diagnostics
}

fn allowed(filter: Option<&PermissionFilter>, path: &str) -> bool {
    match filter {
        Some(filter) => filter.is_allowed(path),
        None => true,
    }
}

fn ensure_allowed(filter: Option<&PermissionFilter>, path: &str) -> Result<(), AppError> {
    if allowed(filter, path) {
        Ok(())
    } else {
        Err(AppError::operation(format!(
            "read permission denied for mdbase record: {path}"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notes::{
        apply_note_append, apply_note_create, apply_note_delete, apply_note_patch, apply_note_set,
        MarkdownTarget, NoteAppendMode, NoteAppendRequest, NoteCreateRequest, NoteDeleteRequest,
        NotePatchRequest, NoteSetRequest,
    };
    use chrono::TimeZone;
    use std::collections::BTreeMap;
    use std::fs;
    use tempfile::tempdir;
    use vulcan_core::mdbase::{
        apply_mdbase_write_transaction, build_mdbase_write_preview, list_mdbase_write_outbox,
        MdbaseWriteApplyRequest, MdbaseWritePreviewChangeRequest, MdbaseWritePreviewRequest,
        MdbaseWritePreviewVerification,
    };
    use vulcan_core::paths::initialize_vulcan_dir;
    use vulcan_core::permissions::{PathPermission, ResourceSpecifier};
    use vulcan_core::{
        evaluate_dataview_js_with_options, scan_vault, DataviewJsEvalOptions, JsRuntimeSandbox,
        ScanMode,
    };

    fn write_plan_request(
        operation: MdbaseWriteOperation,
        changes: Vec<MdbaseWriteChangeRequest>,
    ) -> MdbaseWritePlanRequest {
        MdbaseWritePlanRequest {
            caller_id: "test-caller".to_string(),
            instance_id: "test-instance".to_string(),
            operation,
            changes,
            matched_types: vec!["task".to_string()],
            generated_values: BTreeMap::new(),
            permission_profile: None,
            ttl_seconds: Some(300),
        }
    }

    pub(super) fn fixture() -> (tempfile::TempDir, VaultPaths) {
        let directory = tempdir().expect("temp directory");
        fs::write(
            directory.path().join("mdbase.yaml"),
            "spec_version: \"0.3.0\"\n",
        )
        .expect("config");
        fs::create_dir_all(directory.path().join("_types")).expect("types directory");
        fs::write(
            directory.path().join("_types/task.md"),
            "---\nkind: mdbase.type\nname: task\nversion: 1\nschema:\n  dialect: json-schema-2020-12\n  value:\n    type: object\n    required: [type, title]\n    properties:\n      type: {const: task}\n      title: {type: string}\ncollection:\n  read_defaults: {status: open}\n---\n",
        )
        .expect("type");
        fs::create_dir_all(directory.path().join("tasks/private")).expect("records directory");
        fs::write(
            directory.path().join("tasks/public.md"),
            "---\ntype: task\ntitle: Public\n---\nBody\n",
        )
        .expect("public record");
        fs::write(
            directory.path().join("tasks/private/secret.md"),
            "---\ntype: task\ntitle: Secret\n---\nHidden\n",
        )
        .expect("private record");
        let paths = VaultPaths::new(directory.path());
        (directory, paths)
    }

    #[test]
    fn read_surface_filters_records_before_validation_and_source_is_opt_in() {
        let (_directory, paths) = fixture();
        let filter = PermissionFilter::new(PathPermission {
            allow: read_control_grant(),
            deny: vec![ResourceSpecifier::Folder("tasks/private/**".to_string())],
        });
        let status = build_mdbase_status_report(&paths, Some(&filter)).expect("status");
        assert_eq!(status.records, 1);

        let report = build_mdbase_validate_report(&paths, None, Some(&filter)).expect("validate");
        assert_eq!(report.records.len(), 1);
        assert_eq!(report.records[0].path, "tasks/public.md");

        let without_source =
            build_mdbase_read_report(&paths, "tasks/public.md", false, Some(&filter))
                .expect("read");
        assert!(without_source.record.document.is_none());
        assert_eq!(
            without_source.record.effective_frontmatter["status"],
            "open"
        );
        let with_source = build_mdbase_read_report(&paths, "tasks/public.md", true, Some(&filter))
            .expect("read with source");
        assert!(with_source.record.document.is_some());
        assert!(
            build_mdbase_read_report(&paths, "tasks/private/secret.md", true, Some(&filter))
                .is_err()
        );
    }

    pub(super) fn read_control_grant() -> Vec<ResourceSpecifier> {
        vec![
            ResourceSpecifier::Note("mdbase.yaml".to_string()),
            ResourceSpecifier::Folder("_types/**".to_string()),
            ResourceSpecifier::Folder("_contracts/**".to_string()),
            ResourceSpecifier::Folder("tasks/**".to_string()),
        ]
    }

    fn assert_read_controls_denied(paths: &VaultPaths, filter: &PermissionFilter) {
        let attempts = [
            build_mdbase_status_report(paths, Some(filter)).map(|_| ()),
            build_mdbase_types_report(paths, Some(filter)).map(|_| ()),
            build_mdbase_contracts_report(paths, Some(filter)).map(|_| ()),
            build_mdbase_validate_report(paths, None, Some(filter)).map(|_| ()),
            build_mdbase_read_report(paths, "tasks/public.md", false, Some(filter)).map(|_| ()),
            build_mdbase_query_report(paths, &serde_json::json!({}), Some(filter)).map(|_| ()),
        ];
        for result in attempts {
            let error = result.unwrap_err();
            assert_eq!(error.code(), Some("permission_denied"));
            assert_eq!(
                error.to_string(),
                "permission denied for required mdbase controls"
            );
        }
    }

    #[test]
    fn read_control_denials_precede_config_parsing_and_namespace_discovery() {
        let (dir, paths) = fixture();
        let filter = PermissionFilter::new(PathPermission {
            allow: vec![ResourceSpecifier::Folder("tasks/**".to_string())],
            deny: Vec::new(),
        });
        for contents in [None, Some("invalid: ["), Some("spec_version: '0.3.0'\n")] {
            if let Some(contents) = contents {
                fs::write(dir.path().join("mdbase.yaml"), contents).unwrap();
            } else {
                fs::remove_file(dir.path().join("mdbase.yaml")).unwrap();
            }
            assert_read_controls_denied(&paths, &filter);
        }
        let filter = PermissionFilter::new(PathPermission {
            allow: read_control_grant(),
            deny: vec![ResourceSpecifier::Note("_types/secret.md".to_string())],
        });
        assert_read_controls_denied(&paths, &filter);
        fs::write(dir.path().join("_types/secret.md"), "invalid: [").unwrap();
        assert_read_controls_denied(&paths, &filter);
        assert!(!dir.path().join(".vulcan").exists());
    }

    #[test]
    fn read_schema_denials_are_fatal_and_independent_of_hidden_file_contents() {
        for contract in [false, true] {
            let (dir, paths) = fixture();
            if contract {
                fs::create_dir(dir.path().join("_contracts")).unwrap();
                fs::write(dir.path().join("_contracts/task.md"), "---\nkind: mdbase.contract\ncontract_type: record\nid: example.task\nversion: 1.0.0\nrecord_schema:\n  dialect: json-schema-2020-12\n  ref: ../hidden.yaml\n---\n").unwrap();
            } else {
                fs::write(dir.path().join("_types/task.md"), "---\nkind: mdbase.type\nname: task\nschema:\n  dialect: json-schema-2020-12\n  value: {$ref: ../hidden.yaml}\n---\n").unwrap();
            }
            let filter = PermissionFilter::new(PathPermission {
                allow: read_control_grant(),
                deny: Vec::new(),
            });
            for contents in [None, Some("invalid: ["), Some("type: object\n")] {
                if let Some(contents) = contents {
                    fs::write(dir.path().join("hidden.yaml"), contents).unwrap();
                }
                assert_read_controls_denied(&paths, &filter);
            }
            let mut allow = read_control_grant();
            allow.push(ResourceSpecifier::Note("hidden.yaml".to_string()));
            let filter = PermissionFilter::new(PathPermission {
                allow,
                deny: Vec::new(),
            });
            assert!(
                build_mdbase_status_report(&paths, Some(&filter))
                    .unwrap()
                    .valid
            );
            assert!(!dir.path().join(".vulcan").exists());
        }
    }

    #[test]
    fn registry_read_surfaces_are_deterministic_and_non_mutating() {
        let (directory, paths) = fixture();
        let before = fs::read(directory.path().join("tasks/public.md")).expect("record bytes");
        let types = build_mdbase_types_report(&paths, None).expect("types");
        let contracts = build_mdbase_contracts_report(&paths, None).expect("contracts");
        assert_eq!(types.types.len(), 1);
        assert_eq!(types.types[0].name, "task");
        assert!(contracts.contracts.is_empty());
        assert_eq!(
            fs::read(directory.path().join("tasks/public.md")).expect("record bytes"),
            before
        );
        assert!(!directory.path().join(".vulcan").exists());
    }

    #[test]
    fn query_filters_effective_values_and_never_leaks_denied_records() {
        let (_directory, paths) = fixture();
        let filter = PermissionFilter::new(PathPermission {
            allow: read_control_grant(),
            deny: vec![ResourceSpecifier::Folder("tasks/private/**".to_string())],
        });
        let report = build_mdbase_query_report(
            &paths,
            &serde_json::json!({
                "types": ["task"],
                "where": "status == \"open\" && file.body.contains(\"Body\")",
                "select": ["title", {"name": "display", "expr": "title + \"!\""}],
                "order_by": [{"field": "file.path"}],
                "group_by": [{"field": "status"}],
                "summaries": [{"field": "title", "function": "count", "name": "tasks"}],
                "include_body": false,
                "frontmatter_mode": "effective"
            }),
            Some(&filter),
        )
        .expect("query");

        assert_eq!(report.meta.total_count, 1);
        assert_eq!(report.results[0].file["path"], "tasks/public.md");
        assert_eq!(
            report.results[0].values.as_ref().unwrap()["display"],
            "Public!"
        );
        assert!(report.results[0].body.is_none());
        assert_eq!(
            report.meta.groups.as_ref().unwrap()[0].summaries["tasks"],
            1
        );
    }

    #[test]
    fn query_cache_rejects_source_and_control_drift_before_publication() {
        for mutation in ["edit", "create", "delete", "control"] {
            let (directory, paths) = fixture();
            let loaded = load_collection_authorized(&paths, None).unwrap();
            let error = load_query_records_with_boundary(
                &paths,
                &loaded,
                None,
                &mut MdbaseQueryMetrics::default(),
                &vulcan_core::mdbase::compile_mdbase_prepared_query(&serde_json::json!({}))
                    .unwrap(),
                || match mutation {
                    "edit" => {
                        fs::write(directory.path().join("tasks/public.md"), "Changed\n").unwrap();
                    }
                    "create" => fs::write(directory.path().join("added.md"), "Added\n").unwrap(),
                    "delete" => fs::remove_file(directory.path().join("tasks/public.md")).unwrap(),
                    "control" => fs::write(
                        directory.path().join("mdbase.yaml"),
                        "spec_version: 0.3.0\n# changed\n",
                    )
                    .unwrap(),
                    _ => unreachable!(),
                },
            )
            .unwrap_err();
            assert_eq!(error.code(), Some("stale_state"), "{mutation}: {error}");
        }
    }

    #[test]
    #[allow(clippy::too_many_lines)] // One public cache lifecycle proves repair and permission-scope isolation.
    fn query_cache_reconciles_sources_repairs_payloads_and_preserves_restricted_scope() {
        let (directory, paths) = fixture();
        let query = json!({"types": ["task"], "select": ["title", "status"],
            "order_by": [{"field": "file.path"}], "include_body": true});
        let first = build_mdbase_query_report(&paths, &query, None).unwrap();
        assert_eq!(first.meta.total_count, 2);
        assert!(!paths.cache_db().exists());
        initialize_vulcan_dir(&paths).unwrap();
        assert_eq!(
            build_mdbase_query_report(&paths, &query, None).unwrap(),
            first
        );
        assert!(paths.cache_db().exists());
        let connection = rusqlite::Connection::open(paths.cache_db()).unwrap();
        let cached_count: i64 = connection
            .query_row(
                "SELECT count(*) FROM mdbase_record_cache WHERE local_record_json IS NOT NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(cached_count, 2);

        let public = directory.path().join("tasks/public.md");
        let original = fs::read_to_string(&public).unwrap();
        let mtime = fs::metadata(&public).unwrap().modified().unwrap();
        fs::write(&public, original.replace("Public", "Edited")).unwrap();
        fs::File::options()
            .write(true)
            .open(&public)
            .unwrap()
            .set_modified(mtime)
            .unwrap();
        let edited = build_mdbase_query_report(&paths, &query, None).unwrap();
        assert_ne!(edited, first);
        fs::write(directory.path().join("tasks/added.md"), &original).unwrap();
        assert_eq!(
            build_mdbase_query_report(&paths, &query, None)
                .unwrap()
                .meta
                .total_count,
            3
        );
        fs::rename(
            directory.path().join("tasks/added.md"),
            directory.path().join("tasks/renamed.md"),
        )
        .unwrap();
        let renamed = build_mdbase_query_report(&paths, &query, None).unwrap();
        assert!(renamed
            .results
            .iter()
            .any(|row| row.file["path"] == "tasks/renamed.md"));
        fs::remove_file(directory.path().join("tasks/renamed.md")).unwrap();
        assert_eq!(
            build_mdbase_query_report(&paths, &query, None).unwrap(),
            edited
        );
        let remaining: i64 = connection
            .query_row("SELECT count(*) FROM mdbase_record_cache", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(remaining, 2, "deleted records must leave the shared cache");

        connection
            .execute(
                "UPDATE mdbase_record_cache SET local_record_json='broken'",
                [],
            )
            .unwrap();
        assert_eq!(
            build_mdbase_query_report(&paths, &query, None).unwrap(),
            edited
        );
        // Hidden invalid source and corrupt cached JSON must not be inspected.
        fs::write(directory.path().join("tasks/private/secret.md"), [0xff]).unwrap();
        connection.execute("UPDATE mdbase_record_cache SET local_record_json='hidden corruption' WHERE path='tasks/private/secret.md'", []).unwrap();
        let mut grants = read_control_grant();
        grants.push(ResourceSpecifier::Note("mdbase.lock.yaml".into()));
        let filter = PermissionFilter::new(PathPermission {
            allow: grants,
            deny: vec![ResourceSpecifier::Folder("tasks/private/**".into())],
        });
        let restricted = build_mdbase_query_report(&paths, &query, Some(&filter)).unwrap();
        assert_eq!(restricted.meta.total_count, 1);
        let hidden: String = connection.query_row("SELECT local_record_json FROM mdbase_record_cache WHERE path='tasks/private/secret.md'", [], |row| row.get(0)).unwrap();
        assert_eq!(hidden, "hidden corruption");
        // Restricted fallback derives changed readable sources but leaves all
        // shared cache rows untouched, rather than publishing a partial scope.
        fs::write(&public, original.replace("Public", "Scoped")).unwrap();
        let scoped = build_mdbase_query_report(&paths, &query, Some(&filter)).unwrap();
        assert_ne!(scoped, restricted);
        let loaded = load_collection_authorized(&paths, Some(&filter)).unwrap();
        let source = load_mdbase_records_with_contracts_filtered(
            &loaded.collection,
            &loaded.types,
            &loaded.contracts,
            true,
            Some(&filter),
        )
        .unwrap();
        let mut oracle = vulcan_core::mdbase::compile_mdbase_prepared_query(&query)
            .unwrap()
            .execute(
                &source,
                &loaded.types,
                &loaded.collection.config.settings.id_field,
                loaded.collection.config.settings.timezone.as_deref(),
                Utc::now(),
            )
            .unwrap();
        oracle
            .diagnostics
            .splice(0..0, registry_diagnostics(&loaded, Some(&filter)));
        assert_eq!(scoped, oracle);
        let hidden_after: String = connection.query_row("SELECT local_record_json FROM mdbase_record_cache WHERE path='tasks/private/secret.md'", [], |row| row.get(0)).unwrap();
        assert_eq!(hidden_after, hidden);
    }

    #[test]
    fn canonical_query_source_accepts_yaml_and_json() {
        assert_eq!(
            parse_mdbase_query("types: [task]\nlimit: 2\n").expect("YAML"),
            serde_json::json!({"types": ["task"], "limit": 2})
        );
        assert_eq!(
            parse_mdbase_query(r#"{"where":"true"}"#).expect("JSON"),
            serde_json::json!({"where": "true"})
        );
    }

    #[test]
    fn preview_refuses_new_revisions_when_loaded_control_bytes_are_stale() {
        let (directory, paths) = fixture();
        let mut loaded = load_collection(&paths).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 9, 13, 12, 0, 0).unwrap();
        let request = MdbaseWritePreviewRequest {
            plan_id: "stale-controls".into(),
            caller_id: "caller".into(),
            instance_id: "instance".into(),
            operation: "update".into(),
            issued_at: now,
            expires_at: now + TimeDelta::minutes(5),
            permission_revision: "grant".into(),
            config_revision: "config".into(),
            changes: vec![MdbaseWritePreviewChangeRequest {
                path: "tasks/public.md".into(),
                after: Some("---\ntype: task\ntitle: Updated\n---\n".into()),
                if_revision: None,
            }],
            matched_types: vec!["task".into()],
            relevant_record_namespaces: Vec::new(),
            generated_values: BTreeMap::new(),
        };
        loaded.capture_preview(request.clone()).unwrap();
        let path = directory.path().join("_types/task.md");
        let source = fs::read_to_string(&path).unwrap();
        fs::write(&path, source.replace("status: open", "status: closed")).unwrap();
        // A fresh standalone capture succeeds; it must not certify the old
        // registry merely because its digest describes current disk contents.
        build_mdbase_write_preview(&loaded.collection, request.clone()).unwrap();
        assert_eq!(
            loaded.capture_preview(request.clone()).unwrap_err().code(),
            Some("stale_state")
        );
        write_validation::reload_controls(&mut loaded).unwrap();
        loaded.capture_preview(request).unwrap();
        assert!(!paths.cache_db().exists());
    }

    #[test]
    fn cooperating_read_reports_a_transaction_that_needs_recovery() {
        let (directory, paths) = fixture();
        initialize_vulcan_dir(&paths).expect("initialize transaction state");
        let collection = load_mdbase_collection(directory.path())
            .expect("load collection")
            .expect("collection");
        let issued_at = Utc.with_ymd_and_hms(2026, 9, 13, 12, 0, 0).unwrap();
        let preview = build_mdbase_write_preview(
            &collection,
            MdbaseWritePreviewRequest {
                plan_id: "read-boundary".to_string(),
                caller_id: "caller".to_string(),
                instance_id: "instance".to_string(),
                operation: "update".to_string(),
                issued_at,
                expires_at: Utc.with_ymd_and_hms(2026, 9, 13, 12, 10, 0).unwrap(),
                permission_revision: "grant:v1".to_string(),
                config_revision: "config:v1".to_string(),
                changes: vec![MdbaseWritePreviewChangeRequest {
                    path: "tasks/public.md".to_string(),
                    after: Some("---\ntype: task\ntitle: Changed\n---\nBody\n".to_string()),
                    if_revision: None,
                }],
                matched_types: vec!["task".to_string()],
                relevant_record_namespaces: vec!["tasks/**".to_string()],
                generated_values: BTreeMap::new(),
            },
        )
        .expect("preview");
        let request = MdbaseWriteApplyRequest {
            preview: &preview,
            verification: MdbaseWritePreviewVerification {
                caller_id: "caller",
                instance_id: "instance",
                operation: "update",
                permission_revision: "grant:v1",
                config_revision: "config:v1",
                now: Utc.with_ymd_and_hms(2026, 9, 13, 12, 1, 0).unwrap(),
            },
            idempotency_key: "read-boundary",
        };
        apply_mdbase_write_transaction(&paths, &collection, &request, |_| {
            Err("simulated cache failure".to_string())
        })
        .expect("canonical write committed");

        let error = build_mdbase_status_report(&paths, None).unwrap_err();
        assert!(error.to_string().contains("recovery is required"));
    }

    #[test]
    fn dry_run_then_apply_updates_cache_and_replay_has_no_follow_up_work() {
        let (directory, paths) = fixture();
        let now = Utc.with_ymd_and_hms(2026, 9, 13, 12, 0, 0).unwrap();
        let plan = plan_mdbase_write(
            &paths,
            &write_plan_request(
                MdbaseWriteOperation::Update,
                vec![MdbaseWriteChangeRequest {
                    path: "tasks/public.md".to_string(),
                    after: Some("---\ntype: task\ntitle: Updated\n---\nBody\n".to_string()),
                    if_revision: None,
                }],
            ),
            now,
        )
        .expect("plan update");
        assert!(plan.dry_run);
        assert_eq!(
            fs::read_to_string(directory.path().join("tasks/public.md")).unwrap(),
            "---\ntype: task\ntitle: Public\n---\nBody\n"
        );
        assert!(!directory.path().join(".vulcan").exists());

        let options = MdbaseWriteExecutionOptions {
            idempotency_key: "update-public".to_string(),
            no_commit: true,
            quiet: true,
        };
        let report =
            apply_mdbase_write(&paths, &plan, &options, now + chrono::Duration::seconds(1))
                .expect("apply update");
        assert!(!report.outcome.replayed);
        assert!(report.scan.is_some());
        assert!(report.follow_up_errors.is_empty());
        assert!(fs::read_to_string(directory.path().join("tasks/public.md"))
            .unwrap()
            .contains("title: Updated"));

        let replay =
            apply_mdbase_write(&paths, &plan, &options, now + chrono::Duration::seconds(2))
                .expect("idempotent replay");
        assert!(replay.outcome.replayed);
        assert!(replay.scan.is_none());
        assert!(replay.auto_commit.is_none());
    }

    #[test]
    fn if_revision_rejects_stale_plans_and_preserves_external_bytes() {
        let (directory, paths) = fixture();
        let now = Utc.with_ymd_and_hms(2026, 9, 13, 12, 0, 0).unwrap();
        let original =
            fs::read_to_string(directory.path().join("tasks/public.md")).expect("original source");
        let revision = mdbase_content_revision(&original);
        let replacement = "---\ntype: task\ntitle: Updated\n---\nBody\n";

        let stale = plan_mdbase_write(
            &paths,
            &write_plan_request(
                MdbaseWriteOperation::Update,
                vec![MdbaseWriteChangeRequest {
                    path: "tasks/public.md".to_string(),
                    after: Some(replacement.to_string()),
                    if_revision: Some("opaque-stale-token".to_string()),
                }],
            ),
            now,
        )
        .expect_err("stale revision should fail while planning");
        assert_eq!(stale.code(), Some("concurrent_modification"));
        assert_eq!(
            fs::read_to_string(directory.path().join("tasks/public.md")).expect("source unchanged"),
            original
        );

        let plan = plan_mdbase_write(
            &paths,
            &write_plan_request(
                MdbaseWriteOperation::Update,
                vec![MdbaseWriteChangeRequest {
                    path: "tasks/public.md".to_string(),
                    after: Some(replacement.to_string()),
                    if_revision: Some(revision),
                }],
            ),
            now,
        )
        .expect("matching revision should plan");
        let external = "---\ntype: task\ntitle: External\n---\nBody\n";
        fs::write(directory.path().join("tasks/public.md"), external).expect("external edit");

        let error = apply_mdbase_write(
            &paths,
            &plan,
            &MdbaseWriteExecutionOptions {
                idempotency_key: "revision-race".to_string(),
                no_commit: true,
                quiet: true,
            },
            now + chrono::Duration::seconds(1),
        )
        .expect_err("changed revision should fail while applying");
        assert_eq!(error.code(), Some("concurrent_modification"));
        assert_eq!(
            fs::read_to_string(directory.path().join("tasks/public.md")).expect("external source"),
            external
        );
        assert!(list_mdbase_write_outbox(&paths).expect("outbox").is_empty());
    }

    #[test]
    fn operation_shapes_cover_create_delete_rename_and_batch_without_mutation() {
        let (directory, paths) = fixture();
        let now = Utc.with_ymd_and_hms(2026, 9, 13, 12, 0, 0).unwrap();
        let create = plan_mdbase_write(
            &paths,
            &write_plan_request(
                MdbaseWriteOperation::Create,
                vec![MdbaseWriteChangeRequest {
                    path: "tasks/new.md".to_string(),
                    after: Some("---\ntype: task\ntitle: New\n---\n".to_string()),
                    if_revision: None,
                }],
            ),
            now,
        )
        .expect("create plan");
        assert_eq!(create.preview.operation, "create");

        let delete = plan_mdbase_write(
            &paths,
            &write_plan_request(
                MdbaseWriteOperation::Delete,
                vec![MdbaseWriteChangeRequest {
                    path: "tasks/public.md".to_string(),
                    after: None,
                    if_revision: None,
                }],
            ),
            now,
        )
        .expect("delete plan");
        assert_eq!(delete.preview.operation, "delete");

        let rename = plan_mdbase_write(
            &paths,
            &write_plan_request(
                MdbaseWriteOperation::Rename {
                    from: "tasks/public.md".to_string(),
                    to: "tasks/renamed.md".to_string(),
                },
                vec![
                    MdbaseWriteChangeRequest {
                        path: "tasks/public.md".to_string(),
                        after: None,
                        if_revision: None,
                    },
                    MdbaseWriteChangeRequest {
                        path: "tasks/renamed.md".to_string(),
                        after: Some("---\ntype: task\ntitle: Public\n---\nBody\n".to_string()),
                        if_revision: None,
                    },
                ],
            ),
            now,
        )
        .expect("rename plan");
        assert_eq!(rename.preview.changes.len(), 2);

        let batch = plan_mdbase_write(
            &paths,
            &write_plan_request(
                MdbaseWriteOperation::Batch,
                vec![
                    MdbaseWriteChangeRequest {
                        path: "tasks/public.md".to_string(),
                        after: Some("---\ntype: task\ntitle: Batched\n---\nBody\n".to_string()),
                        if_revision: None,
                    },
                    MdbaseWriteChangeRequest {
                        path: "tasks/new.md".to_string(),
                        after: Some("---\ntype: task\ntitle: New\n---\n".to_string()),
                        if_revision: None,
                    },
                ],
            ),
            now,
        )
        .expect("batch plan");
        assert_eq!(batch.preview.changes.len(), 2);
        assert!(!directory.path().join("tasks/new.md").exists());
        assert!(directory.path().join("tasks/public.md").exists());
    }

    #[test]
    fn untyped_write_requires_visibility_for_other_types_incoming_constraints() {
        let (directory, paths) = fixture();
        fs::write(
            directory.path().join("_types/task.md"),
            "---\nkind: mdbase.type\nname: task\nschema:\n  dialect: json-schema-2020-12\n  value: {type: object}\ncollection:\n  links:\n    related: {target_type: any, validate_exists: true}\n---\n",
        ).unwrap();
        let source = "An untyped target.\n";
        fs::write(directory.path().join("tasks/public.md"), source).unwrap();
        fs::create_dir_all(paths.config_file().parent().unwrap()).unwrap();
        fs::write(paths.config_file(), "[permissions.profiles.scoped]\nread = { allow = [\"note:tasks/public.md\", \"folder:_types/**\", \"note:mdbase.yaml\", \"note:mdbase.lock.yaml\", \"folder:_contracts/**\"] }\nwrite = { allow = [\"note:tasks/public.md\"] }\n").unwrap();
        let mut request = write_plan_request(
            MdbaseWriteOperation::Delete,
            vec![MdbaseWriteChangeRequest {
                path: "tasks/public.md".to_string(),
                after: None,
                if_revision: None,
            }],
        );
        request.matched_types.clear();
        request.permission_profile = Some("scoped".to_string());
        let now = Utc.with_ymd_and_hms(2026, 9, 13, 12, 0, 0).unwrap();
        let before = plan_mdbase_write(&paths, &request, now).unwrap_err();
        fs::write(
            directory.path().join("tasks/private/secret.md"),
            "---\ntype: task\nrelated: '[[tasks/public]]'\n---\n",
        )
        .unwrap();
        let after = plan_mdbase_write(&paths, &request, now).unwrap_err();
        assert_eq!(before.code(), Some("permission_denied"));
        assert_eq!(before.message(), after.message());
        assert!(!after.message().contains("secret"));
        assert_eq!(
            fs::read_to_string(directory.path().join("tasks/public.md")).unwrap(),
            source
        );
        assert!(list_mdbase_write_outbox(&paths).unwrap().is_empty());
    }

    #[test]
    fn managed_routing_denies_before_config_presence_parsing_or_path_classification() {
        for config in [
            None,
            Some("secret: [invalid"),
            Some("spec_version: '0.3.0'\nsettings:\n  exclude: [Archive/**]\n"),
        ] {
            let directory = tempdir().unwrap();
            let paths = VaultPaths::new(directory.path());
            fs::create_dir(directory.path().join(".vulcan")).unwrap();
            fs::write(paths.config_file(), "[permissions.profiles.scoped]\nread = { allow = [\"folder:tasks/**\", \"folder:Archive/**\"] }\nwrite = { allow = [\"folder:tasks/**\", \"folder:Archive/**\"] }\n").unwrap();
            if let Some(config) = config {
                fs::write(directory.path().join("mdbase.yaml"), config).unwrap();
            }
            let changes = [
                MdbaseManagedNoteWriteChange {
                    path: "tasks/new.md",
                    before: None,
                    after: Some("New\n"),
                },
                MdbaseManagedNoteWriteChange {
                    path: "Archive/ordinary.md",
                    before: None,
                    after: Some("Ordinary\n"),
                },
            ];
            for changes in [&changes[..1], &changes[1..], &changes[..]] {
                for mode in [
                    MdbaseManagedWriteMode::Validated,
                    MdbaseManagedWriteMode::RawRepair,
                ] {
                    for dry_run in [true, false] {
                        let error = apply_managed_mdbase_note_writes(
                            &paths,
                            &MdbaseManagedNoteWriteBatchRequest {
                                changes,
                                operation: MdbaseWriteOperation::Batch,
                                mode,
                                allow_mixed_paths: true,
                                dry_run,
                                permission_profile: Some("scoped"),
                                quiet: true,
                            },
                        )
                        .unwrap_err();
                        assert_eq!(error.code(), Some("permission_denied"));
                        assert_eq!(
                            error.message(),
                            "permission denied for required mdbase controls"
                        );
                    }
                }
            }
            assert!(!directory.path().join("tasks").exists());
            assert!(!directory.path().join("Archive").exists());
            assert!(!paths.cache_db().exists());
        }
    }

    #[test]
    fn authorized_routing_distinguishes_absence_and_exclusion_without_loading_registries() {
        let directory = tempdir().unwrap();
        let paths = VaultPaths::new(directory.path());
        fs::create_dir(directory.path().join(".vulcan")).unwrap();
        fs::write(paths.config_file(), "[permissions.profiles.scoped]\nread = { allow = [\"note:mdbase.yaml\"] }\nwrite = { allow = [] }\n").unwrap();
        let change = [MdbaseManagedNoteWriteChange {
            path: "Archive/ordinary.md",
            before: None,
            after: Some("Ordinary\n"),
        }];
        let request = MdbaseManagedNoteWriteBatchRequest {
            changes: &change,
            operation: MdbaseWriteOperation::Create,
            mode: MdbaseManagedWriteMode::Validated,
            allow_mixed_paths: false,
            dry_run: true,
            permission_profile: Some("scoped"),
            quiet: true,
        };
        assert!(apply_managed_mdbase_note_writes(&paths, &request)
            .unwrap()
            .is_none());
        fs::write(
            directory.path().join("mdbase.yaml"),
            "spec_version: '0.3.0'\nsettings:\n  exclude: [Archive/**]\n",
        )
        .unwrap();
        assert!(apply_managed_mdbase_note_writes(&paths, &request)
            .unwrap()
            .is_none());
        // An authorized parse failure is still an error, never ordinary fallback.
        fs::write(directory.path().join("mdbase.yaml"), "invalid: [").unwrap();
        assert!(apply_managed_mdbase_note_writes(&paths, &request).is_err());
        fs::write(paths.config_file(), "[permissions.profiles.scoped]\nread = { allow = [\"note:mdbase.yaml\"] }\npolicy_hook = 'missing-policy.js'\n").unwrap();
        let error = apply_managed_mdbase_note_writes(&paths, &request).unwrap_err();
        assert_eq!(error.code(), Some("permission_denied"));
        assert_eq!(
            error.message(),
            "permission denied for required mdbase controls"
        );
    }

    #[test]
    fn write_planning_and_apply_require_control_authority_before_config_reads() {
        let (directory, paths) = fixture();
        fs::create_dir(directory.path().join(".vulcan")).unwrap();
        let config = "[permissions.profiles.scoped]\nread = { allow = [\"note:mdbase.yaml\", \"note:mdbase.lock.yaml\", \"folder:_types/**\", \"folder:_contracts/**\", \"folder:tasks/**\"] }\nwrite = { allow = [\"folder:tasks/**\"] }\n";
        fs::write(paths.config_file(), config).unwrap();
        let mut request = write_plan_request(
            MdbaseWriteOperation::Update,
            vec![MdbaseWriteChangeRequest {
                path: "tasks/public.md".into(),
                after: Some("---\ntype: task\ntitle: Updated\n---\n".into()),
                if_revision: None,
            }],
        );
        request.permission_profile = Some("scoped".into());
        let now = Utc.with_ymd_and_hms(2026, 9, 13, 12, 0, 0).unwrap();
        let plan = plan_mdbase_write(&paths, &request, now).unwrap();
        let options = MdbaseWriteExecutionOptions {
            idempotency_key: "denied-controls".into(),
            no_commit: true,
            quiet: true,
        };
        for denied in [
            "note:mdbase.yaml",
            "note:mdbase.lock.yaml",
            "folder:_types/**",
            "folder:_contracts/**",
        ] {
            fs::write(
                paths.config_file(),
                config.replace(&format!("\"{denied}\", "), ""),
            )
            .unwrap();
            assert_eq!(
                plan_mdbase_write(&paths, &request, now).unwrap_err().code(),
                Some("permission_denied")
            );
            assert_eq!(
                apply_mdbase_write(&paths, &plan, &options, now)
                    .unwrap_err()
                    .code(),
                Some("permission_denied")
            );
        }
        fs::write(
            paths.config_file(),
            config.replace("\"note:mdbase.yaml\", ", ""),
        )
        .unwrap();
        for contents in [
            None,
            Some("invalid: [SECRET"),
            Some("spec_version: 0.3.0\n"),
        ] {
            if let Some(contents) = contents {
                fs::write(directory.path().join("mdbase.yaml"), contents).unwrap();
            } else {
                fs::remove_file(directory.path().join("mdbase.yaml")).unwrap();
            }
            for error in [
                plan_mdbase_write(&paths, &request, now).unwrap_err(),
                apply_mdbase_write(&paths, &plan, &options, now).unwrap_err(),
            ] {
                assert_eq!(error.code(), Some("permission_denied"));
                assert_eq!(
                    error.message(),
                    "permission denied for required mdbase controls"
                );
            }
        }
        assert!(!paths.cache_db().exists());
        assert!(fs::read_to_string(directory.path().join("tasks/public.md"))
            .unwrap()
            .contains("title: Public"));
    }

    #[test]
    fn write_controls_deny_hidden_schema_bytes_and_dynamic_policy_hooks() {
        let (directory, paths) = fixture();
        fs::create_dir(directory.path().join(".vulcan")).unwrap();
        let config = "[permissions.profiles.scoped]\nread = { allow = [\"note:mdbase.yaml\", \"note:mdbase.lock.yaml\", \"folder:_types/**\", \"folder:_contracts/**\", \"folder:tasks/**\"] }\nwrite = { allow = [\"folder:tasks/**\"] }\n";
        fs::write(paths.config_file(), config).unwrap();
        let mut request = write_plan_request(
            MdbaseWriteOperation::Update,
            vec![MdbaseWriteChangeRequest {
                path: "tasks/public.md".into(),
                after: Some("---\ntype: task\ntitle: Updated\n---\n".into()),
                if_revision: None,
            }],
        );
        request.permission_profile = Some("scoped".into());
        let now = Utc.with_ymd_and_hms(2026, 9, 13, 12, 0, 0).unwrap();
        let plan = plan_mdbase_write(&paths, &request, now).unwrap();
        fs::write(directory.path().join("_types/task.md"), "---\nkind: mdbase.type\nname: task\nschema:\n  dialect: json-schema-2020-12\n  ref: ../hidden.txt\n---\n").unwrap();
        for contents in [None, Some("invalid: [SECRET"), Some("type: object\n")] {
            if let Some(contents) = contents {
                fs::write(directory.path().join("hidden.txt"), contents).unwrap();
            }
            let error = plan_mdbase_write(&paths, &request, now).unwrap_err();
            assert_eq!(error.code(), Some("permission_denied"));
            assert_eq!(
                error.message(),
                "permission denied for required mdbase controls"
            );
            let error = apply_mdbase_write(
                &paths,
                &plan,
                &MdbaseWriteExecutionOptions {
                    idempotency_key: "hidden-schema".into(),
                    no_commit: true,
                    quiet: true,
                },
                now,
            )
            .unwrap_err();
            assert_eq!(error.code(), Some("permission_denied"));
        }
        let config = config.replace(
            "\"note:mdbase.yaml\"",
            "\"note:mdbase.yaml\", \"note:hidden.txt\"",
        );
        fs::write(paths.config_file(), &config).unwrap();
        assert!(plan_mdbase_write(&paths, &request, now).is_ok());
        fs::write(
            paths.config_file(),
            format!("{config}policy_hook = 'policy.js'\n"),
        )
        .unwrap();
        let error = plan_mdbase_write(&paths, &request, now).unwrap_err();
        assert_eq!(error.code(), Some("permission_denied"));
        assert_eq!(
            error.message(),
            "permission denied for required mdbase controls"
        );
        assert!(!paths.cache_db().exists());
    }

    #[test]
    fn operation_shape_mismatch_is_rejected() {
        let (_directory, paths) = fixture();
        let now = Utc.with_ymd_and_hms(2026, 9, 13, 12, 0, 0).unwrap();
        let error = plan_mdbase_write(
            &paths,
            &write_plan_request(
                MdbaseWriteOperation::Create,
                vec![MdbaseWriteChangeRequest {
                    path: "tasks/public.md".to_string(),
                    after: Some("replacement".to_string()),
                    if_revision: None,
                }],
            ),
            now,
        )
        .unwrap_err();
        assert!(error.to_string().contains("does not match"));
    }

    #[test]
    fn managed_batch_rejects_mixed_record_and_ordinary_paths() {
        let (directory, paths) = fixture();
        let record = fs::read_to_string(directory.path().join("tasks/public.md")).unwrap();
        let type_file = fs::read_to_string(directory.path().join("_types/task.md")).unwrap();
        let changes = [
            MdbaseManagedNoteWriteChange {
                path: "tasks/public.md",
                before: Some(&record),
                after: Some(&record),
            },
            MdbaseManagedNoteWriteChange {
                path: "_types/task.md",
                before: Some(&type_file),
                after: Some(&type_file),
            },
        ];

        let error = apply_managed_mdbase_note_writes(
            &paths,
            &MdbaseManagedNoteWriteBatchRequest {
                changes: &changes,
                operation: MdbaseWriteOperation::Batch,
                mode: MdbaseManagedWriteMode::Validated,
                allow_mixed_paths: false,
                dry_run: true,
                permission_profile: None,
                quiet: true,
            },
        )
        .expect_err("mixed managed batch should fail");

        assert!(error.message().contains("cannot mix"));
        assert!(!directory.path().join(".vulcan").exists());
    }

    #[test]
    fn generic_note_set_uses_validated_journaled_mdbase_write() {
        let (directory, paths) = fixture();
        let replacement = "---\ntype: task\ntitle: Changed through note set\n---\nBody\n";

        let report = apply_note_set(
            &paths,
            &NoteSetRequest {
                note: "tasks/public.md".to_string(),
                replacement: replacement.to_string(),
                preserve_frontmatter: false,
            },
            None,
            true,
        )
        .expect("managed note set should succeed");

        assert_eq!(report.content, replacement);
        assert_eq!(
            fs::read_to_string(directory.path().join("tasks/public.md")).expect("record source"),
            replacement
        );
        let outbox = list_mdbase_write_outbox(&paths).expect("outbox should be readable");
        assert_eq!(outbox.len(), 1);
        assert_eq!(outbox[0].operation, "update");
        assert_eq!(outbox[0].paths.len(), 1);
        assert_eq!(outbox[0].paths[0].path, "tasks/public.md");
    }

    #[test]
    fn generic_note_create_append_patch_and_delete_share_the_mdbase_journal() {
        let (directory, paths) = fixture();
        let created = "---\ntype: task\ntitle: New\n---\nBody\n";
        apply_note_create(
            &paths,
            &NoteCreateRequest {
                path: "tasks/new.md".to_string(),
                template: None,
                frontmatter: None,
                body: created.to_string(),
            },
            None,
            true,
        )
        .expect("managed note create should succeed");
        apply_note_append(
            &paths,
            &NoteAppendRequest {
                note: Some("tasks/new.md".to_string()),
                text: "Extra\n".to_string(),
                mode: NoteAppendMode::Append,
                heading: None,
                periodic: None,
                date: None,
                vars: std::collections::HashMap::default(),
            },
            None,
            true,
        )
        .expect("managed note append should succeed");
        apply_note_patch(
            &paths,
            &NotePatchRequest {
                target: MarkdownTarget {
                    display_path: "tasks/new.md".to_string(),
                    absolute_path: directory.path().join("tasks/new.md"),
                    vault_relative_path: Some("tasks/new.md".to_string()),
                    config: VaultConfig::default(),
                },
                section_id: None,
                heading: None,
                block_ref: None,
                lines: None,
                find: "Extra".to_string(),
                replace: "Patched".to_string(),
                replace_all: false,
                dry_run: false,
            },
            None,
            true,
        )
        .expect("managed note patch should succeed");
        apply_note_delete(
            &paths,
            &NoteDeleteRequest {
                note: "tasks/new.md".to_string(),
                dry_run: false,
            },
            None,
            true,
        )
        .expect("managed note delete should succeed");

        assert!(!directory.path().join("tasks/new.md").exists());
        let outbox = list_mdbase_write_outbox(&paths).expect("outbox should be readable");
        assert_eq!(outbox.len(), 4);
        assert_eq!(
            outbox
                .iter()
                .map(|event| event.operation.as_str())
                .collect::<Vec<_>>(),
            ["create", "update", "update", "delete"]
        );
    }

    #[cfg(feature = "js_runtime")]
    #[test]
    fn managed_note_template_rejects_side_effect_without_publishing_either_file() {
        let (directory, paths) = fixture();
        let template_dir = directory.path().join(".vulcan/templates");
        fs::create_dir_all(&template_dir).expect("template dir");
        let template_path = template_dir.join("task.md");
        fs::write(
            &template_path,
            "<%* await tp.file.create_new('Side body', 'Side'); %>---\ntype: task\ntitle: New\n---\nBody\n",
        )
        .expect("template");

        apply_note_create(
            &paths,
            &NoteCreateRequest {
                path: "tasks/new.md".to_string(),
                template: Some("task".to_string()),
                frontmatter: None,
                body: String::new(),
            },
            None,
            true,
        )
        .expect_err("managed side effect must remain forbidden");
        assert!(!directory.path().join("Side.md").exists());
        assert!(!directory.path().join("tasks/new.md").exists());
        assert!(list_mdbase_write_outbox(&paths).expect("outbox").is_empty());
    }

    #[test]
    fn generic_and_direct_validated_writes_reject_the_same_invalid_draft() {
        let (directory, paths) = fixture();
        let original = fs::read_to_string(directory.path().join("tasks/public.md"))
            .expect("original record source");
        let invalid = "---\ntype: task\n---\nBody\n";

        let direct_error = apply_managed_mdbase_note_write(
            &paths,
            &MdbaseManagedNoteWriteRequest {
                path: "tasks/public.md",
                before: Some(&original),
                after: Some(invalid),
                operation: MdbaseWriteOperation::Update,
                mode: MdbaseManagedWriteMode::Validated,
                dry_run: true,
                permission_profile: None,
                quiet: true,
            },
        )
        .expect_err("direct managed write should reject invalid source");
        let note_error = apply_note_set(
            &paths,
            &NoteSetRequest {
                note: "tasks/public.md".to_string(),
                replacement: invalid.to_string(),
                preserve_frontmatter: false,
            },
            None,
            true,
        )
        .expect_err("generic note set should reject invalid source");

        assert_eq!(direct_error.message(), note_error.message());
        assert!(note_error.message().contains("schema_required"));
        assert_eq!(
            fs::read_to_string(directory.path().join("tasks/public.md")).expect("record source"),
            original
        );
        assert!(list_mdbase_write_outbox(&paths)
            .expect("outbox should be readable")
            .is_empty());
    }

    #[test]
    fn managed_write_uses_its_before_image_as_a_revision_precondition() {
        let (directory, paths) = fixture();
        let original =
            fs::read_to_string(directory.path().join("tasks/public.md")).expect("original source");
        let external = "---\ntype: task\ntitle: External\n---\nBody\n";
        fs::write(directory.path().join("tasks/public.md"), external).expect("external edit");

        let error = apply_managed_mdbase_note_write(
            &paths,
            &MdbaseManagedNoteWriteRequest {
                path: "tasks/public.md",
                before: Some(&original),
                after: Some("---\ntype: task\ntitle: Proposed\n---\nBody\n"),
                operation: MdbaseWriteOperation::Update,
                mode: MdbaseManagedWriteMode::Validated,
                dry_run: false,
                permission_profile: None,
                quiet: true,
            },
        )
        .expect_err("stale before-image must reject the managed write");

        assert_eq!(error.code(), Some("concurrent_modification"));
        assert_eq!(
            fs::read_to_string(directory.path().join("tasks/public.md")).expect("external source"),
            external
        );
        assert!(!paths
            .operational_state_dir()
            .expect("state root")
            .join("mdbase-write/journal.json")
            .exists());
    }

    #[test]
    fn script_transaction_commits_managed_changes_as_one_journal_batch() {
        let (directory, paths) = fixture();
        initialize_vulcan_dir(&paths).expect("initialize transaction state");
        scan_vault(&paths, ScanMode::Full).expect("scan fixture");

        evaluate_dataview_js_with_options(
            &paths,
            r#"
            vault.transaction((tx) => {
              tx.set("tasks/public", "---\ntype: task\ntitle: Updated\n---\nBody\n");
              tx.create("tasks/new", {
                content: "Body",
                frontmatter: { type: "task", title: "New" }
              });
            });
            "#,
            None,
            DataviewJsEvalOptions {
                sandbox: Some(JsRuntimeSandbox::Fs),
                mutation_committer: Some(mdbase_js_mutation_committer(&paths, None, true)),
                ..DataviewJsEvalOptions::default()
            },
        )
        .expect("script transaction should commit");

        assert!(fs::read_to_string(directory.path().join("tasks/public.md"))
            .expect("updated record")
            .contains("title: Updated"));
        assert!(directory.path().join("tasks/new.md").exists());
        let outbox = list_mdbase_write_outbox(&paths).expect("outbox");
        assert_eq!(outbox.len(), 1);
        assert_eq!(outbox[0].operation, "batch");
        assert_eq!(outbox[0].paths.len(), 2);
    }

    #[test]
    fn script_routing_denies_config_without_falling_back_to_ordinary_writes() {
        let (directory, paths) = fixture();
        initialize_vulcan_dir(&paths).unwrap();
        scan_vault(&paths, ScanMode::Full).unwrap();
        fs::write(paths.config_file(), "[permissions.profiles.scoped]\nread = { allow = [\"folder:tasks/**\"] }\nwrite = { allow = [\"folder:tasks/**\"] }\nexecute = 'allow'\n").unwrap();
        let original = fs::read_to_string(directory.path().join("tasks/public.md")).unwrap();
        for config in [Some("hidden: [invalid"), None] {
            match config {
                Some(config) => fs::write(directory.path().join("mdbase.yaml"), config).unwrap(),
                None => fs::remove_file(directory.path().join("mdbase.yaml")).unwrap(),
            }
            let error = evaluate_dataview_js_with_options(
                &paths,
                r#"vault.set("tasks/public", "---\ntype: task\ntitle: Updated\n---\nBody\n")"#,
                None,
                DataviewJsEvalOptions {
                    sandbox: Some(JsRuntimeSandbox::Fs),
                    permission_profile: Some("scoped".into()),
                    mutation_committer: Some(mdbase_js_mutation_committer(
                        &paths,
                        Some("scoped"),
                        true,
                    )),
                    ..DataviewJsEvalOptions::default()
                },
            )
            .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("permission denied for required mdbase controls"),
                "{error}"
            );
            assert!(!error.to_string().contains("hidden"));
            assert_eq!(
                fs::read_to_string(directory.path().join("tasks/public.md")).unwrap(),
                original
            );
            assert!(list_mdbase_write_outbox(&paths).unwrap().is_empty());
        }
    }

    #[test]
    fn standalone_script_write_preserves_its_journal_operation() {
        let (directory, paths) = fixture();
        initialize_vulcan_dir(&paths).expect("initialize transaction state");
        scan_vault(&paths, ScanMode::Full).expect("scan fixture");

        evaluate_dataview_js_with_options(
            &paths,
            r#"vault.set("tasks/public", "---\ntype: task\ntitle: Updated\n---\nBody\n")"#,
            None,
            DataviewJsEvalOptions {
                sandbox: Some(JsRuntimeSandbox::Fs),
                mutation_committer: Some(mdbase_js_mutation_committer(&paths, None, true)),
                ..DataviewJsEvalOptions::default()
            },
        )
        .expect("standalone script write should commit");

        assert!(fs::read_to_string(directory.path().join("tasks/public.md"))
            .expect("updated record")
            .contains("title: Updated"));
        let outbox = list_mdbase_write_outbox(&paths).expect("outbox");
        assert_eq!(outbox.len(), 1);
        assert_eq!(outbox[0].operation, "update");
        assert_eq!(outbox[0].paths.len(), 1);
    }

    #[test]
    fn invalid_script_write_restores_source_without_a_journal() {
        let (directory, paths) = fixture();
        initialize_vulcan_dir(&paths).expect("initialize transaction state");
        scan_vault(&paths, ScanMode::Full).expect("scan fixture");
        let original =
            fs::read_to_string(directory.path().join("tasks/public.md")).expect("original record");

        let error = evaluate_dataview_js_with_options(
            &paths,
            r#"vault.set("tasks/public", "---\ntype: task\n---\nBody\n")"#,
            None,
            DataviewJsEvalOptions {
                sandbox: Some(JsRuntimeSandbox::Fs),
                mutation_committer: Some(mdbase_js_mutation_committer(&paths, None, true)),
                ..DataviewJsEvalOptions::default()
            },
        )
        .expect_err("invalid script write should fail");

        assert!(error.to_string().contains("schema_required"));
        assert_eq!(
            fs::read_to_string(directory.path().join("tasks/public.md")).expect("record source"),
            original
        );
        assert!(list_mdbase_write_outbox(&paths).expect("outbox").is_empty());
    }

    #[test]
    fn explicit_raw_repair_keeps_diagnostics_and_uses_the_write_journal() {
        let (directory, paths) = fixture();
        let original = fs::read_to_string(directory.path().join("tasks/public.md"))
            .expect("original record source");
        let invalid = "---\ntype: task\n---\nBody\n";

        let report = apply_managed_mdbase_note_write(
            &paths,
            &MdbaseManagedNoteWriteRequest {
                path: "tasks/public.md",
                before: Some(&original),
                after: Some(invalid),
                operation: MdbaseWriteOperation::Update,
                mode: MdbaseManagedWriteMode::RawRepair,
                dry_run: false,
                permission_profile: None,
                quiet: true,
            },
        )
        .expect("raw repair should run")
        .expect("record path should be managed");

        assert!(report.diagnostics.iter().any(|diagnostic| {
            diagnostic.severity == vulcan_core::mdbase::MdbaseRecordDiagnosticSeverity::Error
        }));
        assert_eq!(
            fs::read_to_string(directory.path().join("tasks/public.md")).expect("record source"),
            invalid
        );
        assert_eq!(
            list_mdbase_write_outbox(&paths)
                .expect("outbox should be readable")
                .len(),
            1
        );
    }
}
