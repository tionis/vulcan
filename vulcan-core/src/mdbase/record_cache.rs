use super::control_access::ControlAccess;
use super::{
    load_mdbase_records_with_contracts, MdbaseCollection, MdbaseContractRegistry,
    MdbaseContractView, MdbaseRecordDiagnostic, MdbaseRecordError, MdbaseRecordFileMetadata,
    MdbaseTypeRegistry, MDBASE_LOCK_FILE_NAME, MDBASE_RECORD_MODEL_VERSION,
};
use crate::cache::{CacheDatabase, CacheError};
use crate::paths::secure_read_to_string;
use crate::permissions::PermissionFilter;
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{Display, Formatter};
use std::fs;
use std::path::{Path, PathBuf};

mod control_dependencies;
type ControlSources = Vec<(PathBuf, String)>;

const TYPE_CANDIDATE_SQL: &str = "SELECT DISTINCT membership.path
    FROM json_each(?4) AS wanted
    CROSS JOIN mdbase_record_types AS membership
    CROSS JOIN mdbase_record_cache AS record
      ON record.collection_root = membership.collection_root AND record.path = membership.path
    WHERE membership.collection_root = ?1 AND membership.type_name = wanted.value
      AND record.dependency_digest = ?2 AND record.record_model_version = ?3
      AND record.metadata_json IS NOT NULL
    ORDER BY membership.path";
const ALL_CANDIDATE_SQL: &str = "SELECT path FROM mdbase_record_cache
    WHERE collection_root = ?1 AND dependency_digest = ?2 AND record_model_version = ?3
      AND metadata_json IS NOT NULL
    ORDER BY path";

/// Select cached candidate paths for the type-membership part of a structured
/// plan. All other predicates, diagnostics, sorting, grouping and pagination
/// remain residual: this is NOT a query result or a freshness/authority proof.
///
/// Callers must establish a complete coherent current cache snapshot and control
/// authority before use, then apply the rest of the plan to authorized records.
/// Paths are filtered before returning and no record JSON is hydrated. Cache
/// diagnostics cannot be reused blindly across permission scopes. The canonical
/// source-derived query service does not yet use this primitive.
pub fn select_cached_mdbase_candidate_paths(
    connection: &Connection,
    collection: &MdbaseCollection,
    plan: &crate::query::StructuredQueryPlan,
    dependency_digest: &str,
    filter: Option<&PermissionFilter>,
) -> Result<Vec<String>, MdbaseRecordCacheError> {
    let mut parameters = vec![
        rusqlite::types::Value::Text(cache_collection_root(collection)?),
        rusqlite::types::Value::Text(dependency_digest.to_string()),
        rusqlite::types::Value::Integer(i64::from(MDBASE_RECORD_MODEL_VERSION)),
    ];
    let sql = if plan.types.is_empty() {
        ALL_CANDIDATE_SQL
    } else {
        let types = plan
            .types
            .iter()
            .map(|name| name.to_ascii_lowercase())
            .collect::<BTreeSet<_>>();
        parameters.push(rusqlite::types::Value::Text(serde_json::to_string(&types)?));
        TYPE_CANDIDATE_SQL
    };
    let mut statement = connection.prepare_cached(sql)?;
    let rows = statement.query_map(rusqlite::params_from_iter(parameters), |row| {
        row.get::<_, String>(0)
    })?;
    let mut paths = Vec::new();
    for row in rows {
        let path = row?;
        if filter.is_none_or(|filter| filter.is_allowed(&path)) {
            paths.push(path);
        }
    }
    Ok(paths)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MdbaseCachedMetadata {
    /// Authored values only; missing properties remain absent. Never fill these
    /// from effective defaults or projected contract fields.
    pub frontmatter: serde_json::Value,
    pub file: MdbaseRecordFileMetadata,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MdbaseCachedRecord {
    pub collection_root: String,
    pub path: String,
    pub revision: String,
    pub dependency_digest: String,
    pub record_model_version: u32,
    pub types: Vec<String>,
    pub effective_frontmatter: serde_json::Value,
    pub display: Option<serde_json::Value>,
    pub contract_views: Vec<MdbaseContractView>,
    pub diagnostics: Vec<MdbaseRecordDiagnostic>,
    /// Absent on migrated legacy rows until source-derived refresh. This is not
    /// a complete record: body, exact source, and permission-scoped links are not
    /// included. Collection diagnostics still require scope-aware evaluation.
    #[serde(default)]
    pub metadata: Option<MdbaseCachedMetadata>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MdbaseRecordCacheRefresh {
    pub dependency_digest: String,
    pub dependency_changed: bool,
    pub added: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub deleted: usize,
}

/// Content-derived revisions for every authoritative mdbase control class.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MdbaseControlRevisions {
    pub config: String,
    pub types: String,
    pub contracts: String,
    pub schemas: String,
    pub combined: String,
}

#[derive(Debug)]
pub enum MdbaseRecordCacheError {
    PermissionDenied,
    StaleControls,
    Records(MdbaseRecordError),
    Cache(CacheError),
    Database(rusqlite::Error),
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    Json(serde_json::Error),
}

impl Display for MdbaseRecordCacheError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::StaleControls => write!(
                formatter,
                "mdbase control snapshots are stale; reload the collection and registries"
            ),
            Self::PermissionDenied => {
                write!(formatter, "permission denied for required mdbase controls")
            }
            Self::Records(error) => write!(formatter, "failed to derive mdbase records: {error}"),
            Self::Cache(error) => write!(formatter, "failed to update the Vulcan cache: {error}"),
            Self::Database(error) => {
                write!(formatter, "failed to access mdbase cache rows: {error}")
            }
            Self::Read { path, source } => write!(
                formatter,
                "failed to read mdbase dependency {}: {source}",
                path.display()
            ),
            Self::Json(error) => write!(formatter, "invalid mdbase cache JSON: {error}"),
        }
    }
}

impl std::error::Error for MdbaseRecordCacheError {}

impl From<MdbaseRecordError> for MdbaseRecordCacheError {
    fn from(error: MdbaseRecordError) -> Self {
        Self::Records(error)
    }
}

impl From<CacheError> for MdbaseRecordCacheError {
    fn from(error: CacheError) -> Self {
        Self::Cache(error)
    }
}

impl From<rusqlite::Error> for MdbaseRecordCacheError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Database(error)
    }
}

impl From<serde_json::Error> for MdbaseRecordCacheError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

/// Hash all semantic control inputs without including ordinary Markdown records.
pub fn mdbase_record_dependency_digest(
    collection: &MdbaseCollection,
) -> Result<String, MdbaseRecordCacheError> {
    Ok(mdbase_control_revisions(collection)?.combined)
}

/// Snapshot authoritative control files without consulting derived cache rows.
pub fn mdbase_control_revisions(
    collection: &MdbaseCollection,
) -> Result<MdbaseControlRevisions, MdbaseRecordCacheError> {
    mdbase_control_revisions_authorized(collection, None)
}

/// Capture controls under a collection-relative read ceiling. Required namespace
/// coverage and config/lockfile authority are checked before filesystem probes,
/// including when those files or folders do not exist. Every transitive schema
/// reference is authorized before opening it or observing its absence.
/// This does not implement dynamic policy hooks or authorize ordinary records.
pub fn mdbase_control_revisions_authorized(
    collection: &MdbaseCollection,
    filter: Option<&PermissionFilter>,
) -> Result<MdbaseControlRevisions, MdbaseRecordCacheError> {
    Ok(capture_control_revisions(collection, filter)?.revisions)
}

struct ControlCapture {
    revisions: MdbaseControlRevisions,
    snapshot: super::control_snapshot::ControlSnapshot,
    types: BTreeSet<PathBuf>,
    contracts: BTreeSet<PathBuf>,
}

/// Bind the actual registry inputs (including invalid/missing schemas) to a
/// current authorized revision capture, without recompiling any schema.
pub fn verify_mdbase_control_snapshots(
    collection: &MdbaseCollection,
    types: &MdbaseTypeRegistry,
    contracts: &MdbaseContractRegistry,
    filter: Option<&PermissionFilter>,
) -> Result<MdbaseControlRevisions, MdbaseRecordCacheError> {
    // Public settings can be changed by a library caller. Do not use altered
    // control-folder paths for discovery before rejecting the unbound config.
    if collection.config != collection.loaded_config {
        return Err(MdbaseRecordCacheError::StaleControls);
    }
    let capture = capture_control_revisions(collection, filter)?;
    if capture.snapshot.conflicted
        || capture.snapshot.observed.get(Path::new("mdbase.yaml"))
            != Some(&Some(collection.source_revision.clone()))
        || types.config_revision != collection.source_revision
        || contracts.config_revision != collection.source_revision
        || contracts.type_snapshot != types.snapshot
        || !types
            .snapshot
            .matches(&capture.snapshot.observed, &capture.types)
        || !contracts
            .snapshot
            .matches(&capture.snapshot.observed, &capture.contracts)
    {
        return Err(MdbaseRecordCacheError::StaleControls);
    }
    Ok(capture.revisions)
}

fn capture_control_revisions(
    collection: &MdbaseCollection,
    filter: Option<&PermissionFilter>,
) -> Result<ControlCapture, MdbaseRecordCacheError> {
    let access = ControlAccess::new(filter);
    if !access.path_allowed("mdbase.yaml")
        || !access.path_allowed(MDBASE_LOCK_FILE_NAME)
        || !access.folder_allowed(&collection.config.settings.types_folder)
        || !access.folder_allowed(&collection.config.settings.contracts_folder)
    {
        return Err(MdbaseRecordCacheError::PermissionDenied);
    }
    let mut config_paths = BTreeSet::new();
    config_paths.insert(PathBuf::from("mdbase.yaml"));
    if collection.root.join(MDBASE_LOCK_FILE_NAME).is_file() {
        config_paths.insert(PathBuf::from(MDBASE_LOCK_FILE_NAME));
    }

    let mut type_paths = BTreeSet::new();
    collect_dependency_tree(
        &collection.root,
        Path::new(&collection.config.settings.types_folder),
        &mut type_paths,
    )?;
    let mut contract_paths = BTreeSet::new();
    collect_dependency_tree(
        &collection.root,
        Path::new(&collection.config.settings.contracts_folder),
        &mut contract_paths,
    )?;
    let (config, config_sources) =
        digest_dependency_paths(&collection.root, "config", &config_paths, &access)?;
    let (types, type_sources) =
        digest_dependency_paths(&collection.root, "types", &type_paths, &access)?;
    let (contracts, contract_sources) =
        digest_dependency_paths(&collection.root, "contracts", &contract_paths, &access)?;
    let mut snapshot = super::control_snapshot::ControlSnapshot::default();
    for (path, source) in config_sources
        .iter()
        .chain(&type_sources)
        .chain(&contract_sources)
    {
        snapshot.observe(path, Some(source.as_bytes()));
    }
    let sources = control_dependencies::schema_sources(
        &collection.root,
        type_sources.into_iter().chain(contract_sources),
        &access,
    )?;
    let mut schema_digest = Sha256::new();
    schema_digest.update(b"referenced-schemas-v1");
    for (path, source) in sources {
        snapshot.observe(&path, source.as_deref());
        let path = path.to_string_lossy().replace('\\', "/");
        schema_digest.update((path.len() as u64).to_be_bytes());
        schema_digest.update(path.as_bytes());
        match source {
            Some(bytes) => {
                schema_digest.update([1]);
                schema_digest.update((bytes.len() as u64).to_be_bytes());
                schema_digest.update(bytes);
            }
            None => schema_digest.update([0]),
        }
    }
    let schemas = format!("sha256:{:x}", schema_digest.finalize());

    let mut digest = Sha256::new();
    digest.update(MDBASE_RECORD_MODEL_VERSION.to_be_bytes());
    for revision in [&config, &types, &contracts, &schemas] {
        digest.update(
            u64::try_from(revision.len())
                .unwrap_or(u64::MAX)
                .to_be_bytes(),
        );
        digest.update(revision.as_bytes());
    }
    Ok(ControlCapture {
        snapshot,
        types: type_paths
            .into_iter()
            .filter(|path| super::has_extension(path, "md"))
            .collect(),
        contracts: contract_paths
            .into_iter()
            .filter(|path| super::has_extension(path, "md"))
            .collect(),
        revisions: MdbaseControlRevisions {
            config,
            types,
            contracts,
            schemas,
            combined: format!("sha256:{:x}", digest.finalize()),
        },
    })
}

fn digest_dependency_paths(
    root: &Path,
    domain: &str,
    paths: &BTreeSet<PathBuf>,
    access: &ControlAccess<'_>,
) -> Result<(String, ControlSources), MdbaseRecordCacheError> {
    let mut digest = Sha256::new();
    let mut sources = Vec::new();
    digest.update(domain.as_bytes());
    for path in paths {
        if !access.path_allowed(&path.to_string_lossy().replace('\\', "/")) {
            return Err(MdbaseRecordCacheError::PermissionDenied);
        }
        let contents =
            secure_read_to_string(root, path).map_err(|source| MdbaseRecordCacheError::Read {
                path: root.join(path),
                source,
            })?;
        let normalized_path = path.to_string_lossy().replace('\\', "/");
        digest.update(
            u64::try_from(normalized_path.len())
                .unwrap_or(u64::MAX)
                .to_be_bytes(),
        );
        digest.update(normalized_path.as_bytes());
        digest.update(
            u64::try_from(contents.len())
                .unwrap_or(u64::MAX)
                .to_be_bytes(),
        );
        digest.update(contents.as_bytes());
        sources.push((path.clone(), contents));
    }
    Ok((format!("sha256:{:x}", digest.finalize()), sources))
}

fn collect_dependency_tree(
    root: &Path,
    relative: &Path,
    paths: &mut BTreeSet<PathBuf>,
) -> Result<(), MdbaseRecordCacheError> {
    let directory = root.join(relative);
    if !directory.exists() {
        return Ok(());
    }
    let mut entries = fs::read_dir(&directory)
        .map_err(|source| MdbaseRecordCacheError::Read {
            path: directory.clone(),
            source,
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| MdbaseRecordCacheError::Read {
            path: directory.clone(),
            source,
        })?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let entry_path = entry.path();
        let child = entry_path
            .strip_prefix(root)
            .expect("walked dependency remains below collection root")
            .to_path_buf();
        let file_type = entry
            .file_type()
            .map_err(|source| MdbaseRecordCacheError::Read {
                path: entry_path.clone(),
                source,
            })?;
        if file_type.is_symlink() {
            return Err(MdbaseRecordCacheError::Read {
                path: entry_path,
                source: std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "symlinked control dependencies are not allowed",
                ),
            });
        }
        if file_type.is_dir() {
            let name = entry.file_name();
            if relative.as_os_str().is_empty()
                && matches!(name.to_str(), Some(".git" | ".vulcan" | ".mdbase"))
            {
                continue;
            }
            collect_dependency_tree(root, &child, paths)?;
        } else if file_type.is_file() {
            paths.insert(child);
        }
    }
    Ok(())
}

/// Refresh changed projections, invalidate dependency-stale rows, and remove deleted records.
pub fn refresh_mdbase_record_cache(
    database: &mut CacheDatabase,
    collection: &MdbaseCollection,
    types: &MdbaseTypeRegistry,
    contracts: &MdbaseContractRegistry,
) -> Result<MdbaseRecordCacheRefresh, MdbaseRecordCacheError> {
    update_mdbase_record_cache(database, collection, types, contracts, false)
}

/// Rebuild all derived rows for one collection without changing source files.
pub fn rebuild_mdbase_record_cache(
    database: &mut CacheDatabase,
    collection: &MdbaseCollection,
    types: &MdbaseTypeRegistry,
    contracts: &MdbaseContractRegistry,
) -> Result<MdbaseRecordCacheRefresh, MdbaseRecordCacheError> {
    update_mdbase_record_cache(database, collection, types, contracts, true)
}

fn update_mdbase_record_cache(
    database: &mut CacheDatabase,
    collection: &MdbaseCollection,
    types: &MdbaseTypeRegistry,
    contracts: &MdbaseContractRegistry,
    rebuild: bool,
) -> Result<MdbaseRecordCacheRefresh, MdbaseRecordCacheError> {
    update_mdbase_record_cache_with_boundary(database, collection, types, contracts, rebuild, || {})
}

fn update_mdbase_record_cache_with_boundary(
    database: &mut CacheDatabase,
    collection: &MdbaseCollection,
    types: &MdbaseTypeRegistry,
    contracts: &MdbaseContractRegistry,
    rebuild: bool,
    before_publication: impl FnOnce(),
) -> Result<MdbaseRecordCacheRefresh, MdbaseRecordCacheError> {
    let controls = verify_mdbase_control_snapshots(collection, types, contracts, None)?;
    let dependency_digest = controls.combined.clone();
    let collection_root = cache_collection_root(collection)?;
    let next = derive_collection_cache(
        collection,
        types,
        contracts,
        &collection_root,
        &dependency_digest,
    )?;
    // Rebuild must not deserialize disposable payloads: damaged JSON is one of
    // the reasons callers need it. Only headers are needed for change counts.
    let previous = if rebuild {
        BTreeMap::new()
    } else {
        load_collection_cache(database.connection(), &collection_root)?
    };
    let previous_headers = if rebuild {
        let mut statement = database.connection().prepare(
            "SELECT path, dependency_digest, record_model_version FROM mdbase_record_cache WHERE collection_root = ?1",
        )?;
        let rows = statement.query_map([&collection_root], |row| {
            Ok((
                row.get::<_, String>(0)?,
                (row.get::<_, String>(1)?, row.get::<_, u32>(2)?),
            ))
        })?;
        rows.collect::<Result<BTreeMap<_, _>, _>>()?
    } else {
        previous
            .iter()
            .map(|(path, record)| {
                (
                    path.clone(),
                    (
                        record.dependency_digest.clone(),
                        record.record_model_version,
                    ),
                )
            })
            .collect()
    };
    let dependency_changed = previous_headers.values().any(|(digest, version)| {
        digest != &dependency_digest || *version != MDBASE_RECORD_MODEL_VERSION
    });
    let deleted = previous_headers
        .keys()
        .filter(|path| !next.contains_key(*path))
        .count();
    let added = next
        .keys()
        .filter(|path| !previous_headers.contains_key(*path))
        .count();
    let updated = if rebuild {
        next.len().saturating_sub(added)
    } else {
        next.iter()
            .filter(|(path, record)| previous.get(*path).is_some_and(|old| old != *record))
            .count()
    };
    let unchanged = if rebuild {
        0
    } else {
        next.len().saturating_sub(added + updated)
    };

    before_publication();
    if verify_mdbase_control_snapshots(collection, types, contracts, None)? != controls {
        return Err(MdbaseRecordCacheError::StaleControls);
    }
    database.with_transaction(|transaction| {
        if rebuild {
            transaction.execute(
                "DELETE FROM mdbase_record_cache WHERE collection_root = ?1",
                [&collection_root],
            )?;
        } else {
            for path in previous.keys().filter(|path| !next.contains_key(*path)) {
                transaction.execute(
                    "DELETE FROM mdbase_record_cache WHERE collection_root = ?1 AND path = ?2",
                    params![collection_root, path],
                )?;
            }
        }
        for (path, record) in &next {
            if !rebuild && previous.get(path) == Some(record) {
                continue;
            }
            store_cached_record(transaction, record)?;
        }
        Ok::<_, MdbaseRecordCacheError>(())
    })?;

    Ok(MdbaseRecordCacheRefresh {
        dependency_digest,
        dependency_changed,
        added,
        updated,
        unchanged,
        deleted,
    })
}

fn derive_collection_cache(
    collection: &MdbaseCollection,
    types: &MdbaseTypeRegistry,
    contracts: &MdbaseContractRegistry,
    collection_root: &str,
    dependency_digest: &str,
) -> Result<BTreeMap<String, MdbaseCachedRecord>, MdbaseRecordCacheError> {
    let records = load_mdbase_records_with_contracts(collection, types, contracts, false)?;
    Ok(records
        .records
        .into_iter()
        .map(|record| {
            let path = record.path.clone();
            (
                path,
                MdbaseCachedRecord {
                    collection_root: collection_root.to_string(),
                    path: record.path,
                    revision: record.revision,
                    dependency_digest: dependency_digest.to_string(),
                    record_model_version: MDBASE_RECORD_MODEL_VERSION,
                    types: record.types,
                    effective_frontmatter: record.effective_frontmatter,
                    display: record.display,
                    contract_views: record.contract_views,
                    diagnostics: record.diagnostics,
                    metadata: Some(MdbaseCachedMetadata {
                        frontmatter: record.frontmatter,
                        file: record.file,
                    }),
                },
            )
        })
        .collect())
}

/// Read a projection only when both its source revision and dependency set are current.
pub fn get_cached_mdbase_record(
    connection: &Connection,
    collection: &MdbaseCollection,
    path: &str,
    revision: &str,
    dependency_digest: &str,
) -> Result<Option<MdbaseCachedRecord>, MdbaseRecordCacheError> {
    let collection_root = cache_collection_root(collection)?;
    connection
        .query_row(
            "SELECT collection_root, path, revision, dependency_digest, record_model_version,
                    types_json, effective_frontmatter_json, display_json,
                    contract_views_json, diagnostics_json, metadata_json
             FROM mdbase_record_cache
             WHERE collection_root = ?1 AND path = ?2 AND revision = ?3
               AND dependency_digest = ?4 AND record_model_version = ?5
               AND metadata_json IS NOT NULL",
            params![
                collection_root,
                path,
                revision,
                dependency_digest,
                MDBASE_RECORD_MODEL_VERSION
            ],
            cached_record_from_row,
        )
        .optional()
        .map_err(MdbaseRecordCacheError::Database)
}

fn load_collection_cache(
    connection: &Connection,
    collection_root: &str,
) -> Result<BTreeMap<String, MdbaseCachedRecord>, MdbaseRecordCacheError> {
    let mut statement = connection.prepare(
        "SELECT collection_root, path, revision, dependency_digest, record_model_version,
                types_json, effective_frontmatter_json, display_json,
                contract_views_json, diagnostics_json, metadata_json
         FROM mdbase_record_cache WHERE collection_root = ?1 ORDER BY path",
    )?;
    let rows = statement.query_map([collection_root], cached_record_from_row)?;
    rows.map(|row| {
        let record = row?;
        Ok((record.path.clone(), record))
    })
    .collect()
}

fn cached_record_from_row(row: &rusqlite::Row<'_>) -> Result<MdbaseCachedRecord, rusqlite::Error> {
    let model_version = row.get::<_, u32>(4)?;
    let types_json = row.get::<_, String>(5)?;
    let effective_json = row.get::<_, String>(6)?;
    let display_json = row.get::<_, Option<String>>(7)?;
    let contract_views_json = row.get::<_, String>(8)?;
    let diagnostics_json = row.get::<_, String>(9)?;
    let metadata_json = row.get::<_, Option<String>>(10)?;
    Ok(MdbaseCachedRecord {
        collection_root: row.get(0)?,
        path: row.get(1)?,
        revision: row.get(2)?,
        dependency_digest: row.get(3)?,
        record_model_version: model_version,
        types: parse_json_column(5, &types_json)?,
        effective_frontmatter: parse_json_column(6, &effective_json)?,
        display: display_json
            .as_deref()
            .map(|value| parse_json_column(7, value))
            .transpose()?,
        contract_views: parse_json_column(8, &contract_views_json)?,
        diagnostics: parse_json_column(9, &diagnostics_json)?,
        metadata: metadata_json
            .as_deref()
            .map(|value| parse_json_column(10, value))
            .transpose()?,
    })
}

fn parse_json_column<T: DeserializeOwned>(
    column: usize,
    value: &str,
) -> Result<T, rusqlite::Error> {
    serde_json::from_str(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            column,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
}

fn store_cached_record(
    transaction: &Transaction<'_>,
    record: &MdbaseCachedRecord,
) -> Result<(), MdbaseRecordCacheError> {
    let types = serde_json::to_string(&record.types)?;
    let effective = serde_json::to_string(&record.effective_frontmatter)?;
    let display = record
        .display
        .as_ref()
        .map(serde_json::to_string)
        .transpose()?;
    let contract_views = serde_json::to_string(&record.contract_views)?;
    let diagnostics = serde_json::to_string(&record.diagnostics)?;
    let metadata = record
        .metadata
        .as_ref()
        .map(serde_json::to_string)
        .transpose()?;
    transaction.execute(
        "INSERT INTO mdbase_record_cache (
            collection_root, path, revision, dependency_digest, record_model_version,
            types_json, effective_frontmatter_json, display_json,
            contract_views_json, diagnostics_json, metadata_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
         ON CONFLICT(collection_root, path) DO UPDATE SET
            revision = excluded.revision,
            dependency_digest = excluded.dependency_digest,
            record_model_version = excluded.record_model_version,
            types_json = excluded.types_json,
            effective_frontmatter_json = excluded.effective_frontmatter_json,
            display_json = excluded.display_json,
            contract_views_json = excluded.contract_views_json,
            diagnostics_json = excluded.diagnostics_json,
            metadata_json = excluded.metadata_json",
        params![
            record.collection_root,
            record.path,
            record.revision,
            record.dependency_digest,
            record.record_model_version,
            types,
            effective,
            display,
            contract_views,
            diagnostics,
            metadata,
        ],
    )?;
    Ok(())
}

fn cache_collection_root(collection: &MdbaseCollection) -> Result<String, MdbaseRecordCacheError> {
    fs::canonicalize(&collection.root)
        .map(|path| path.to_string_lossy().replace('\\', "/"))
        .map_err(|source| MdbaseRecordCacheError::Read {
            path: collection.root.clone(),
            source,
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mdbase::{
        load_mdbase_collection, load_mdbase_contract_registry, load_mdbase_record,
        load_mdbase_type_registry,
    };
    use crate::paths::VaultPaths;
    use std::fs;
    use tempfile::tempdir;

    fn write(path: &Path, contents: &str) {
        fs::create_dir_all(path.parent().expect("fixture parent")).expect("fixture directory");
        fs::write(path, contents).expect("fixture file");
    }

    fn load_registries(
        root: &Path,
    ) -> (MdbaseCollection, MdbaseTypeRegistry, MdbaseContractRegistry) {
        let collection = load_mdbase_collection(root)
            .expect("collection should load")
            .expect("collection should exist");
        let types = load_mdbase_type_registry(&collection).expect("types should load");
        let contracts =
            load_mdbase_contract_registry(&collection, &types).expect("contracts should load");
        (collection, types, contracts)
    }

    #[test]
    fn rebuild_repairs_unreadable_disposable_payloads_without_rewriting_notes() {
        let directory = tempdir().unwrap();
        write(
            &directory.path().join("mdbase.yaml"),
            "spec_version: 0.3.0\n",
        );
        let source = "---\nempty: ''\n---\nExact body\n";
        write(&directory.path().join("a.md"), source);
        let paths = VaultPaths::new(directory.path());
        crate::initialize_vulcan_dir(&paths).unwrap();
        let mut database = CacheDatabase::open(&paths).unwrap();
        let (collection, types, contracts) = load_registries(directory.path());
        let first =
            refresh_mdbase_record_cache(&mut database, &collection, &types, &contracts).unwrap();
        let record = load_mdbase_record(&collection, &types, "a.md", false).unwrap();
        for column in [
            "metadata_json",
            "effective_frontmatter_json",
            "diagnostics_json",
        ] {
            database
                .connection()
                .execute(
                    &format!("UPDATE mdbase_record_cache SET {column} = '{{'"),
                    [],
                )
                .unwrap();
            assert!(get_cached_mdbase_record(
                database.connection(),
                &collection,
                "a.md",
                &record.revision,
                &first.dependency_digest
            )
            .is_err());
            let rebuilt =
                rebuild_mdbase_record_cache(&mut database, &collection, &types, &contracts)
                    .unwrap();
            assert_eq!((rebuilt.added, rebuilt.updated, rebuilt.deleted), (0, 1, 0));
            let cached = get_cached_mdbase_record(
                database.connection(),
                &collection,
                "a.md",
                &record.revision,
                &rebuilt.dependency_digest,
            )
            .unwrap()
            .unwrap();
            assert_eq!(cached.metadata.unwrap().frontmatter, record.frontmatter);
            assert_eq!(
                fs::read_to_string(directory.path().join("a.md")).unwrap(),
                source
            );
        }
    }

    #[test]
    #[allow(clippy::too_many_lines)] // One snapshot progresses through metadata drift and payload repair.
    fn cached_metadata_preserves_persisted_values_and_tracks_metadata_only_changes() {
        let directory = tempdir().unwrap();
        write(
            &directory.path().join("mdbase.yaml"),
            "spec_version: 0.3.0\n",
        );
        write(&directory.path().join("_types/task.md"),
            "---\nkind: mdbase.type\nname: task\nschema:\n  dialect: json-schema-2020-12\n  value: {type: object}\ncollection:\n  read_defaults: {status: open}\n---\n");
        let source = "---\ntype: task\nnull_value: null\nempty: ''\nzero: 0\nflag: false\nlist: []\nnested: {key: value}\n---\nCanonical body\n";
        write(&directory.path().join("a.md"), source);
        let paths = VaultPaths::new(directory.path());
        crate::initialize_vulcan_dir(&paths).unwrap();
        let mut database = CacheDatabase::open(&paths).unwrap();
        let (collection, types, contracts) = load_registries(directory.path());
        let first =
            refresh_mdbase_record_cache(&mut database, &collection, &types, &contracts).unwrap();
        let record = load_mdbase_record(&collection, &types, "a.md", false).unwrap();
        let cached = get_cached_mdbase_record(
            database.connection(),
            &collection,
            "a.md",
            &record.revision,
            &first.dependency_digest,
        )
        .unwrap()
        .unwrap();
        let metadata = cached.metadata.unwrap();
        assert_eq!(metadata.frontmatter, record.frontmatter);
        assert_eq!(metadata.file, record.file);
        assert!(!metadata
            .frontmatter
            .as_object()
            .unwrap()
            .contains_key("status"));
        assert_eq!(cached.effective_frontmatter["status"], "open");
        assert!(metadata.frontmatter["null_value"].is_null());
        assert_eq!(metadata.frontmatter["empty"], "");
        let encoded = serde_json::to_value(&metadata).unwrap();
        assert!(encoded.get("body").is_none());
        assert!(encoded.get("document").is_none());
        assert_eq!(
            fs::read_to_string(directory.path().join("a.md")).unwrap(),
            source
        );

        fs::File::options()
            .write(true)
            .open(directory.path().join("a.md"))
            .unwrap()
            .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(100))
            .unwrap();
        let changed =
            refresh_mdbase_record_cache(&mut database, &collection, &types, &contracts).unwrap();
        assert_eq!(changed.updated, 1);
        let next = get_cached_mdbase_record(
            database.connection(),
            &collection,
            "a.md",
            &record.revision,
            &changed.dependency_digest,
        )
        .unwrap()
        .unwrap();
        assert_eq!(next.revision, record.revision);
        assert_ne!(next.metadata.unwrap().file.mtime, metadata.file.mtime);

        // A legacy or damaged incomplete payload must not be returned as current.
        database
            .connection()
            .execute("UPDATE mdbase_record_cache SET metadata_json = NULL", [])
            .unwrap();
        assert!(get_cached_mdbase_record(
            database.connection(),
            &collection,
            "a.md",
            &record.revision,
            &changed.dependency_digest
        )
        .unwrap()
        .is_none());
        let plan =
            super::super::compile_mdbase_query(&serde_json::json!({"types": ["task"]})).unwrap();
        assert!(select_cached_mdbase_candidate_paths(
            database.connection(),
            &collection,
            &plan,
            &changed.dependency_digest,
            None
        )
        .unwrap()
        .is_empty());
        let repaired =
            refresh_mdbase_record_cache(&mut database, &collection, &types, &contracts).unwrap();
        assert_eq!(repaired.updated, 1);
        assert!(get_cached_mdbase_record(
            database.connection(),
            &collection,
            "a.md",
            &record.revision,
            &repaired.dependency_digest
        )
        .unwrap()
        .unwrap()
        .metadata
        .is_some());
    }

    #[test]
    #[allow(clippy::too_many_lines)] // One indexed corpus exercises plan, scope, and clearing together.
    fn indexed_candidates_preserve_membership_scope_and_leave_other_work_residual() {
        use crate::permissions::{PathPermission, ResourceSpecifier};
        let directory = tempdir().unwrap();
        write(
            &directory.path().join("mdbase.yaml"),
            "spec_version: 0.3.0\n",
        );
        let paths = VaultPaths::new(directory.path());
        crate::initialize_vulcan_dir(&paths).unwrap();
        let mut database = CacheDatabase::open(&paths).unwrap();
        let (collection, _, _) = load_registries(directory.path());
        let root = cache_collection_root(&collection).unwrap();
        database
            .with_transaction(|transaction| {
                for i in 0..10_000 {
                    let types = if i % 100 == 0 {
                        vec!["Task".to_string(), "task".to_string()]
                    } else {
                        vec!["contact".to_string()]
                    };
                    store_cached_record(
                        transaction,
                        &MdbaseCachedRecord {
                            collection_root: root.clone(),
                            path: format!("record-{i:05}.md"),
                            revision: "revision".to_string(),
                            dependency_digest: "controls".to_string(),
                            record_model_version: MDBASE_RECORD_MODEL_VERSION,
                            types,
                            effective_frontmatter: serde_json::json!({"status": "open"}),
                            display: None,
                            contract_views: vec![],
                            diagnostics: vec![],
                            metadata: Some(MdbaseCachedMetadata {
                                frontmatter: serde_json::json!({"status": "open"}),
                                file: MdbaseRecordFileMetadata {
                                    path: format!("record-{i:05}.md"),
                                    name: format!("record-{i:05}.md"),
                                    basename: format!("record-{i:05}"),
                                    ext: "md".to_string(),
                                    folder: String::new(),
                                    size: 0,
                                    mtime: None,
                                    ctime: None,
                                },
                            }),
                        },
                    )?;
                }
                Ok::<_, MdbaseRecordCacheError>(())
            })
            .unwrap();
        let mut plan = super::super::compile_mdbase_query(&serde_json::json!({
            "types": ["TASK", "task"], "where": "false", "limit": 1, "offset": 9
        }))
        .unwrap();
        database
            .connection()
            .execute(
                "INSERT INTO mdbase_record_cache SELECT collection_root || '/other', path,
             revision, dependency_digest, record_model_version, types_json,
             effective_frontmatter_json, display_json, contract_views_json, diagnostics_json, metadata_json
             FROM mdbase_record_cache WHERE path = 'record-00000.md'",
                [],
            )
            .unwrap();
        let select = |plan: &crate::query::StructuredQueryPlan,
                      filter: Option<&PermissionFilter>| {
            select_cached_mdbase_candidate_paths(
                database.connection(),
                &collection,
                plan,
                "controls",
                filter,
            )
            .unwrap()
        };
        let candidates = select(&plan, None);
        assert_eq!(candidates.len(), 100); // no residual predicate or pagination pushed prematurely
        assert_eq!(candidates[0], "record-00000.md");
        assert_eq!(candidates[99], "record-09900.md");
        let filter = PermissionFilter::new(PathPermission {
            allow: vec![ResourceSpecifier::All],
            deny: vec![ResourceSpecifier::Note("record-00000.md".to_string())],
        });
        assert_eq!(select(&plan, Some(&filter)).len(), 99);
        let details = database
            .connection()
            .prepare(&format!("EXPLAIN QUERY PLAN {TYPE_CANDIDATE_SQL}"))
            .unwrap()
            .query_map(
                params![root, "controls", MDBASE_RECORD_MODEL_VERSION, "[\"task\"]"],
                |row| row.get::<_, String>(3),
            )
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(
            details.iter().any(
                |detail| detail.contains("SEARCH membership USING PRIMARY KEY")
                    && detail.contains("collection_root=? AND type_name=?")
            ),
            "{details:?}"
        );
        {
            let mut statement = database.connection().prepare(TYPE_CANDIDATE_SQL).unwrap();
            let selected = statement
                .query_map(
                    params![root, "controls", MDBASE_RECORD_MODEL_VERSION, "[\"task\"]"],
                    |row| row.get::<_, String>(0),
                )
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            assert_eq!(selected, candidates);
            assert_eq!(
                statement.get_status(rusqlite::StatementStatus::FullscanStep),
                0
            );
            let steps = statement.get_status(rusqlite::StatementStatus::VmStep);
            assert!(
                steps < 5_000,
                "selection visited unrelated rows: {steps} VM steps"
            );
        }
        plan.types = vec!["task'); DROP TABLE mdbase_record_cache; --".to_string()];
        assert!(select(&plan, None).is_empty());
        plan.types.clear();
        assert_eq!(select(&plan, None).len(), 10_000);
        assert!(select_cached_mdbase_candidate_paths(
            database.connection(),
            &collection,
            &plan,
            "stale",
            None
        )
        .unwrap()
        .is_empty());
        database.connection().execute("UPDATE mdbase_record_cache SET record_model_version = 0 WHERE path = 'record-00000.md'", []).unwrap();
        assert_eq!(select(&plan, None).len(), 9_999);
        database.clear_all().unwrap();
        let count: i64 = database
            .connection()
            .query_row("SELECT count(*) FROM mdbase_record_types", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn stale_control_snapshots_never_replace_published_cache_rows() {
        for target in [
            "mdbase.yaml",
            "_types/task.md",
            "_types/added.md",
            "_contracts/added.md",
            "schema.txt",
        ] {
            for rebuild in [false, true] {
                let directory = tempdir().unwrap();
                write(
                    &directory.path().join("mdbase.yaml"),
                    "spec_version: 0.3.0\n",
                );
                write(&directory.path().join("_types/task.md"), "---\nkind: mdbase.type\nname: task\nschema:\n  dialect: json-schema-2020-12\n  ref: ../schema.txt\ncollection:\n  read_defaults: {status: open}\n---\n");
                write(&directory.path().join("schema.txt"), "type: object\n");
                write(&directory.path().join("a.md"), "---\ntype: task\n---\n");
                let paths = VaultPaths::new(directory.path());
                crate::initialize_vulcan_dir(&paths).unwrap();
                let mut database = CacheDatabase::open(&paths).unwrap();
                let (collection, types, contracts) = load_registries(directory.path());
                refresh_mdbase_record_cache(&mut database, &collection, &types, &contracts)
                    .unwrap();
                let root = cache_collection_root(&collection).unwrap();
                let before = load_collection_cache(database.connection(), &root).unwrap();
                let path = directory.path().join(target);
                let source = fs::read_to_string(&path).unwrap_or_default();
                write(&path, &format!("{source}\n# changed\n"));
                assert!(
                    matches!(
                        update_mdbase_record_cache(
                            &mut database,
                            &collection,
                            &types,
                            &contracts,
                            rebuild
                        ),
                        Err(MdbaseRecordCacheError::StaleControls)
                    ),
                    "{target}"
                );
                assert_eq!(
                    load_collection_cache(database.connection(), &root).unwrap(),
                    before
                );
                let (fresh, fresh_types, fresh_contracts) = load_registries(directory.path());
                update_mdbase_record_cache(
                    &mut database,
                    &fresh,
                    &fresh_types,
                    &fresh_contracts,
                    rebuild,
                )
                .unwrap();
            }
        }
    }

    #[test]
    fn missing_malformed_and_nested_uppercase_schema_controls_are_bound() {
        for initial in [None, Some("invalid: ["), Some("type: object\n")] {
            let directory = tempdir().unwrap();
            write(
                &directory.path().join("mdbase.yaml"),
                "spec_version: 0.3.0\n",
            );
            write(
                &directory.path().join("_types/nested/mdbase.yaml"),
                "spec_version: 0.3.0\n",
            );
            write(&directory.path().join("_types/nested/task.MD"), "---\nkind: mdbase.type\nname: task\nschema:\n  dialect: json-schema-2020-12\n  ref: ../../schema.txt\n---\n");
            if let Some(initial) = initial {
                write(&directory.path().join("schema.txt"), initial);
            }
            let (collection, types, contracts) = load_registries(directory.path());
            verify_mdbase_control_snapshots(&collection, &types, &contracts, None).unwrap();
            write(
                &directory.path().join("schema.txt"),
                "type: object\nrequired: [title]\n",
            );
            assert!(matches!(
                verify_mdbase_control_snapshots(&collection, &types, &contracts, None),
                Err(MdbaseRecordCacheError::StaleControls)
            ));
            let fresh_types = load_mdbase_type_registry(&collection).unwrap();
            assert!(matches!(
                verify_mdbase_control_snapshots(&collection, &fresh_types, &contracts, None),
                Err(MdbaseRecordCacheError::StaleControls)
            ));
            let fresh_contracts = load_mdbase_contract_registry(&collection, &fresh_types).unwrap();
            verify_mdbase_control_snapshots(&collection, &fresh_types, &fresh_contracts, None)
                .unwrap();
            fs::remove_file(directory.path().join("_types/nested/task.MD")).unwrap();
            assert!(matches!(
                verify_mdbase_control_snapshots(&collection, &fresh_types, &fresh_contracts, None),
                Err(MdbaseRecordCacheError::StaleControls)
            ));
        }
    }

    #[test]
    fn failed_schema_reads_are_not_evidence_of_absence() {
        let directory = tempdir().unwrap();
        write(
            &directory.path().join("mdbase.yaml"),
            "spec_version: 0.3.0\n",
        );
        write(&directory.path().join("_types/task.md"), "---\nkind: mdbase.type\nname: task\nschema:\n  dialect: json-schema-2020-12\n  ref: ../schema.txt\n---\n");
        fs::create_dir(directory.path().join("schema.txt")).unwrap();
        let (collection, types, contracts) = load_registries(directory.path());
        assert!(!types.diagnostics.is_empty());
        fs::remove_dir(directory.path().join("schema.txt")).unwrap();
        assert!(matches!(
            verify_mdbase_control_snapshots(&collection, &types, &contracts, None),
            Err(MdbaseRecordCacheError::StaleControls)
        ));
        let types = load_mdbase_type_registry(&collection).unwrap();
        let contracts = load_mdbase_contract_registry(&collection, &types).unwrap();
        verify_mdbase_control_snapshots(&collection, &types, &contracts, None).unwrap();
        let mut altered = collection.clone();
        altered.config.settings.types_folder = "mdbase.yaml".into();
        assert!(matches!(
            verify_mdbase_control_snapshots(&altered, &types, &contracts, None),
            Err(MdbaseRecordCacheError::StaleControls)
        ));
    }

    #[test]
    fn contract_schema_observations_survive_failed_compilation_and_repair() {
        let directory = tempdir().unwrap();
        write(
            &directory.path().join("mdbase.yaml"),
            "spec_version: 0.3.0\n",
        );
        write(&directory.path().join("_contracts/note.md"), "---\nkind: mdbase.contract\ncontract_type: record\nid: example.note\nversion: 1.0.0\nrecord_schema:\n  dialect: json-schema-2020-12\n  ref: ../contract.txt\n---\n");
        let (collection, types, contracts) = load_registries(directory.path());
        assert!(!contracts.diagnostics.is_empty());
        verify_mdbase_control_snapshots(&collection, &types, &contracts, None).unwrap();
        write(&directory.path().join("contract.txt"), "type: object\n");
        assert!(matches!(
            verify_mdbase_control_snapshots(&collection, &types, &contracts, None),
            Err(MdbaseRecordCacheError::StaleControls)
        ));
        let contracts = load_mdbase_contract_registry(&collection, &types).unwrap();
        assert!(
            contracts.diagnostics.is_empty(),
            "{:?}",
            contracts.diagnostics
        );
        verify_mdbase_control_snapshots(&collection, &types, &contracts, None).unwrap();
        write(&directory.path().join("contract.txt"), "type: string\n");
        assert!(matches!(
            verify_mdbase_control_snapshots(&collection, &types, &contracts, None),
            Err(MdbaseRecordCacheError::StaleControls)
        ));
    }

    #[test]
    fn control_drift_during_derivation_aborts_before_cache_publication() {
        let directory = tempdir().unwrap();
        write(
            &directory.path().join("mdbase.yaml"),
            "spec_version: 0.3.0\n",
        );
        write(&directory.path().join("a.md"), "A\n");
        let paths = VaultPaths::new(directory.path());
        crate::initialize_vulcan_dir(&paths).unwrap();
        let mut database = CacheDatabase::open(&paths).unwrap();
        let (collection, types, contracts) = load_registries(directory.path());
        refresh_mdbase_record_cache(&mut database, &collection, &types, &contracts).unwrap();
        let root = cache_collection_root(&collection).unwrap();
        let before = load_collection_cache(database.connection(), &root).unwrap();
        write(&directory.path().join("a.md"), "Changed record\n");
        for rebuild in [false, true] {
            let error = update_mdbase_record_cache_with_boundary(
                &mut database,
                &collection,
                &types,
                &contracts,
                rebuild,
                || {
                    write(
                        &directory.path().join("mdbase.yaml"),
                        "spec_version: 0.3.0\n# external edit\n",
                    );
                },
            )
            .unwrap_err();
            assert!(matches!(error, MdbaseRecordCacheError::StaleControls));
            assert_eq!(
                load_collection_cache(database.connection(), &root).unwrap(),
                before
            );
            write(
                &directory.path().join("mdbase.yaml"),
                "spec_version: 0.3.0\n",
            );
        }
    }

    #[test]
    #[allow(clippy::too_many_lines)] // Sequential refresh/rebuild lifecycle shares the same cache state.
    fn refresh_reuses_records_and_invalidates_record_and_dependency_changes() {
        let directory = tempdir().expect("collection directory");
        write(
            &directory.path().join("mdbase.yaml"),
            "spec_version: 0.3.0\n",
        );
        write(
            &directory.path().join("schemas/task.json"),
            r#"{"type":"object","properties":{"type":{"const":"task"},"status":{"type":"string"}}}"#,
        );
        write(
            &directory.path().join("_types/task.md"),
            "---\nkind: mdbase.type\nname: task\nversion: 1\nschema:\n  dialect: json-schema-2020-12\n  ref: ../schemas/task.json\ncollection:\n  read_defaults: {status: open}\n---\n",
        );
        write(&directory.path().join("a.md"), "---\ntype: task\n---\na\n");
        write(&directory.path().join("b.md"), "---\ntype: task\n---\nb\n");
        crate::initialize_vulcan_dir(&VaultPaths::new(directory.path()))
            .expect("cache directory should initialize");
        let mut database =
            CacheDatabase::open(&VaultPaths::new(directory.path())).expect("cache should open");
        let (collection, types, contracts) = load_registries(directory.path());

        let first = refresh_mdbase_record_cache(&mut database, &collection, &types, &contracts)
            .expect("first refresh");
        let type_plan =
            super::super::compile_mdbase_query(&serde_json::json!({"types": ["task"]})).unwrap();
        assert_eq!(
            select_cached_mdbase_candidate_paths(
                database.connection(),
                &collection,
                &type_plan,
                &first.dependency_digest,
                None
            )
            .unwrap(),
            ["a.md", "b.md"]
        );
        assert_eq!(
            (first.added, first.updated, first.unchanged, first.deleted),
            (2, 0, 0, 0)
        );
        let second = refresh_mdbase_record_cache(&mut database, &collection, &types, &contracts)
            .expect("second refresh");
        assert_eq!(
            (
                second.added,
                second.updated,
                second.unchanged,
                second.deleted
            ),
            (0, 0, 2, 0)
        );
        assert!(!second.dependency_changed);

        write(
            &directory.path().join("a.md"),
            "---\ntype: task\n---\nchanged\n",
        );
        let record_change =
            refresh_mdbase_record_cache(&mut database, &collection, &types, &contracts)
                .expect("record refresh");
        assert_eq!((record_change.updated, record_change.unchanged), (1, 1));
        assert!(!record_change.dependency_changed);

        write(
            &directory.path().join("schemas/task.json"),
            r#"{"type":"object","properties":{"type":{"const":"task"},"status":{"enum":["open","done"]}}}"#,
        );
        let (collection, types, contracts) = load_registries(directory.path());
        let schema_change =
            refresh_mdbase_record_cache(&mut database, &collection, &types, &contracts)
                .expect("schema refresh");
        assert!(schema_change.dependency_changed);
        assert_eq!((schema_change.updated, schema_change.unchanged), (2, 0));

        let a = load_mdbase_record(&collection, &types, "a.md", false).expect("record");
        let cached = get_cached_mdbase_record(
            database.connection(),
            &collection,
            "a.md",
            &a.revision,
            &schema_change.dependency_digest,
        )
        .expect("cache read")
        .expect("current projection");
        assert_eq!(cached.effective_frontmatter["status"], "open");
        assert!(get_cached_mdbase_record(
            database.connection(),
            &collection,
            "a.md",
            "sha256:stale",
            &schema_change.dependency_digest,
        )
        .expect("stale cache read")
        .is_none());

        fs::remove_file(directory.path().join("b.md")).expect("record should delete");
        let deletion = refresh_mdbase_record_cache(&mut database, &collection, &types, &contracts)
            .expect("deletion refresh");
        assert_eq!((deletion.unchanged, deletion.deleted), (1, 1));

        let rebuilt = rebuild_mdbase_record_cache(&mut database, &collection, &types, &contracts)
            .expect("cache rebuild");
        assert_eq!(
            (rebuilt.added, rebuilt.updated, rebuilt.unchanged),
            (0, 1, 0)
        );
        assert_eq!(
            select_cached_mdbase_candidate_paths(
                database.connection(),
                &collection,
                &type_plan,
                &rebuilt.dependency_digest,
                None
            )
            .unwrap(),
            ["a.md"]
        );
        write(&directory.path().join("a.md"), "Now untyped\n");
        let untyped =
            refresh_mdbase_record_cache(&mut database, &collection, &types, &contracts).unwrap();
        assert!(select_cached_mdbase_candidate_paths(
            database.connection(),
            &collection,
            &type_plan,
            &untyped.dependency_digest,
            None
        )
        .unwrap()
        .is_empty());
        database.clear_all().expect("cache clear");
        let count: i64 = database
            .connection()
            .query_row("SELECT COUNT(*) FROM mdbase_record_cache", [], |row| {
                row.get(0)
            })
            .expect("cache count");
        assert_eq!(count, 0);
    }

    #[test]
    fn old_record_models_are_rejected_and_rederived() {
        let dir = tempdir().unwrap();
        write(&dir.path().join("mdbase.yaml"), "spec_version: '0.3.0'\n");
        write(&dir.path().join("a.md"), "Body\n");
        let paths = VaultPaths::new(dir.path());
        crate::initialize_vulcan_dir(&paths).unwrap();
        let mut database = CacheDatabase::open(&paths).unwrap();
        let (collection, types, contracts) = load_registries(dir.path());
        let first =
            refresh_mdbase_record_cache(&mut database, &collection, &types, &contracts).unwrap();
        let record = load_mdbase_record(&collection, &types, "a.md", false).unwrap();
        database
            .connection()
            .execute(
                "UPDATE mdbase_record_cache SET record_model_version = ?1",
                [MDBASE_RECORD_MODEL_VERSION - 1],
            )
            .unwrap();
        assert!(get_cached_mdbase_record(
            database.connection(),
            &collection,
            "a.md",
            &record.revision,
            &first.dependency_digest
        )
        .unwrap()
        .is_none());
        let refreshed =
            refresh_mdbase_record_cache(&mut database, &collection, &types, &contracts).unwrap();
        assert!(refreshed.dependency_changed);
        assert_eq!((refreshed.updated, refreshed.unchanged), (1, 0));
        let cached = get_cached_mdbase_record(
            database.connection(),
            &collection,
            "a.md",
            &record.revision,
            &refreshed.dependency_digest,
        )
        .unwrap()
        .unwrap();
        assert_eq!(cached.record_model_version, MDBASE_RECORD_MODEL_VERSION);
    }

    fn control_filter(extra: &[&str], denied: &[&str]) -> PermissionFilter {
        use crate::permissions::{PathPermission, ResourceSpecifier};
        PermissionFilter::new(PathPermission {
            allow: ["mdbase.yaml", MDBASE_LOCK_FILE_NAME]
                .into_iter()
                .chain(extra.iter().copied())
                .map(|path| ResourceSpecifier::Note(path.to_string()))
                .chain(
                    ["_types/**", "_contracts/**"]
                        .into_iter()
                        .map(|path| ResourceSpecifier::Folder(path.to_string())),
                )
                .collect(),
            deny: denied
                .iter()
                .copied()
                .map(|path| ResourceSpecifier::Note(path.to_string()))
                .collect(),
        })
    }

    fn assert_control_capture_denied(collection: &MdbaseCollection, filter: &PermissionFilter) {
        let error = mdbase_control_revisions_authorized(collection, Some(filter)).unwrap_err();
        assert!(matches!(error, MdbaseRecordCacheError::PermissionDenied));
        assert_eq!(
            error.to_string(),
            "permission denied for required mdbase controls"
        );
    }

    #[test]
    fn authorized_revisions_deny_config_lock_and_namespaces_before_probing() {
        let directory = tempdir().unwrap();
        write(
            &directory.path().join("mdbase.yaml"),
            "spec_version: 0.3.0\n",
        );
        let collection = load_mdbase_collection(directory.path()).unwrap().unwrap();
        // These inputs are required even when absent. Malformed or newly present
        // hidden bytes must not change the error or reveal the hidden path.
        for path in [
            "mdbase.yaml",
            MDBASE_LOCK_FILE_NAME,
            "_types/hidden.md",
            "_contracts/hidden.md",
        ] {
            let filter = control_filter(&[], &[path]);
            let absolute = directory.path().join(path);
            if absolute.is_file() {
                fs::remove_file(&absolute).unwrap();
            }
            assert_control_capture_denied(&collection, &filter);
            write(&absolute, "SECRET invalid: [");
            assert_control_capture_denied(&collection, &filter);
            write(&absolute, "{}");
            assert_control_capture_denied(&collection, &filter);
        }
        assert!(!directory.path().join(".vulcan").exists());
    }

    #[test]
    fn authorized_revisions_require_configured_not_default_control_namespaces() {
        use crate::permissions::{PathPermission, ResourceSpecifier};
        let directory = tempdir().unwrap();
        write(
            &directory.path().join("mdbase.yaml"),
            "spec_version: 0.3.0\nsettings:\n  types_folder: Schema/Types\n  contracts_folder: Schema/Contracts\n",
        );
        let collection = load_mdbase_collection(directory.path()).unwrap().unwrap();
        assert_control_capture_denied(&collection, &control_filter(&[], &[]));
        let filter = PermissionFilter::new(PathPermission {
            allow: vec![
                ResourceSpecifier::Note("mdbase.yaml".into()),
                ResourceSpecifier::Note(MDBASE_LOCK_FILE_NAME.into()),
                ResourceSpecifier::Folder("Schema/**".into()),
            ],
            deny: Vec::new(),
        });
        assert_eq!(
            mdbase_control_revisions_authorized(&collection, Some(&filter)).unwrap(),
            mdbase_control_revisions(&collection).unwrap()
        );
        write(&directory.path().join("Schema/Types/new.md"), "new control");
        assert_eq!(
            mdbase_control_revisions_authorized(&collection, Some(&filter)).unwrap(),
            mdbase_control_revisions(&collection).unwrap()
        );
    }

    #[test]
    fn authorized_revisions_check_transitive_references_and_match_unrestricted_digest() {
        let directory = tempdir().unwrap();
        write(
            &directory.path().join("mdbase.yaml"),
            "spec_version: 0.3.0\n",
        );
        let collection = load_mdbase_collection(directory.path()).unwrap().unwrap();
        write(
            &directory.path().join("schemas/main.yaml"),
            "$ref: hidden.txt\n",
        );
        for (control, wrapper) in [
            ("_types/task.md", "schema"),
            ("_contracts/task.md", "data_schema"),
        ] {
            write(
                &directory.path().join(control),
                &format!("---\n{wrapper}: {{ref: ../schemas/main.yaml}}\n---\n"),
            );
            let filter = control_filter(&["schemas/main.yaml"], &[]);
            let hidden = directory.path().join("schemas/hidden.txt");
            assert_control_capture_denied(&collection, &filter);
            for source in ["SECRET invalid: [", "{\"type\":\"string\"}"] {
                write(&hidden, source);
                assert_control_capture_denied(&collection, &filter);
            }
            let complete = control_filter(&["schemas/main.yaml", "schemas/hidden.txt"], &[]);
            let captured =
                mdbase_control_revisions_authorized(&collection, Some(&complete)).unwrap();
            assert_eq!(captured, mdbase_control_revisions(&collection).unwrap());
            fs::remove_file(&hidden).unwrap();
            let missing =
                mdbase_control_revisions_authorized(&collection, Some(&complete)).unwrap();
            assert_ne!(captured.schemas, missing.schemas);
            assert_eq!(missing, mdbase_control_revisions(&collection).unwrap());
            fs::remove_file(directory.path().join(control)).unwrap();
        }
        assert!(!directory.path().join(".vulcan").exists());
    }

    #[test]
    fn dependency_digest_tracks_config_types_contracts_and_external_schemas() {
        let directory = tempdir().expect("collection directory");
        write(
            &directory.path().join("mdbase.yaml"),
            "spec_version: 0.3.0\n",
        );
        let type_source =
            "---\nkind: mdbase.type\nschema: {ref: ../schemas/value.json}\n---\ntype v1\n";
        write(&directory.path().join("_types/type.md"), type_source);
        write(
            &directory.path().join("_contracts/contract.md"),
            "contract v1\n",
        );
        write(&directory.path().join("schemas/value.json"), "{}\n");
        let collection = load_mdbase_collection(directory.path())
            .expect("collection should load")
            .expect("collection should exist");

        let initial = mdbase_control_revisions(&collection).expect("initial revisions");
        write(
            &directory.path().join("mdbase.yaml"),
            "spec_version: 0.3.0\n# changed\n",
        );
        let config = mdbase_control_revisions(&collection).expect("config revisions");
        assert_ne!(initial.config, config.config);
        assert_eq!(initial.types, config.types);
        assert_eq!(initial.contracts, config.contracts);
        assert_eq!(initial.schemas, config.schemas);
        assert_ne!(initial.combined, config.combined);

        write(
            &directory.path().join("_types/type.md"),
            &type_source.replace("type v1", "type v2"),
        );
        let type_file = mdbase_control_revisions(&collection).expect("type revisions");
        assert_ne!(config.types, type_file.types);
        assert_eq!(config.contracts, type_file.contracts);
        assert_eq!(config.schemas, type_file.schemas);
        assert_ne!(config.combined, type_file.combined);

        write(
            &directory.path().join("_contracts/contract.md"),
            "contract v2\n",
        );
        let contract = mdbase_control_revisions(&collection).expect("contract revisions");
        assert_eq!(type_file.types, contract.types);
        assert_ne!(type_file.contracts, contract.contracts);
        assert_eq!(type_file.schemas, contract.schemas);
        assert_ne!(type_file.combined, contract.combined);

        write(
            &directory.path().join("schemas/value.json"),
            "{\"type\":\"object\"}\n",
        );
        let schema = mdbase_control_revisions(&collection).expect("schema revisions");
        assert_eq!(contract.types, schema.types);
        assert_eq!(contract.contracts, schema.contracts);
        assert_ne!(contract.schemas, schema.schemas);
        assert_ne!(contract.combined, schema.combined);
    }
}
