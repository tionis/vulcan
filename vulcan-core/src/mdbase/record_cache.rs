use super::{
    load_mdbase_records_with_contracts, MdbaseCollection, MdbaseContractRegistry,
    MdbaseContractView, MdbaseRecordDiagnostic, MdbaseRecordError, MdbaseTypeRegistry,
    MDBASE_LOCK_FILE_NAME, MDBASE_RECORD_MODEL_VERSION,
};
use crate::cache::{CacheDatabase, CacheError};
use crate::paths::secure_read_to_string;
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{Display, Formatter};
use std::fs;
use std::path::{Path, PathBuf};

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

#[derive(Debug)]
pub enum MdbaseRecordCacheError {
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
    let mut paths = BTreeSet::new();
    paths.insert(PathBuf::from("mdbase.yaml"));
    if collection.root.join(MDBASE_LOCK_FILE_NAME).is_file() {
        paths.insert(PathBuf::from(MDBASE_LOCK_FILE_NAME));
    }
    collect_dependency_tree(
        &collection.root,
        Path::new(&collection.config.settings.types_folder),
        true,
        &mut paths,
    )?;
    collect_dependency_tree(
        &collection.root,
        Path::new(&collection.config.settings.contracts_folder),
        true,
        &mut paths,
    )?;
    collect_dependency_tree(&collection.root, Path::new(""), false, &mut paths)?;

    let mut digest = Sha256::new();
    digest.update(MDBASE_RECORD_MODEL_VERSION.to_be_bytes());
    for path in paths {
        let contents = secure_read_to_string(&collection.root, &path).map_err(|source| {
            MdbaseRecordCacheError::Read {
                path: collection.root.join(&path),
                source,
            }
        })?;
        let path = path.to_string_lossy().replace('\\', "/");
        digest.update(u64::try_from(path.len()).unwrap_or(u64::MAX).to_be_bytes());
        digest.update(path.as_bytes());
        digest.update(
            u64::try_from(contents.len())
                .unwrap_or(u64::MAX)
                .to_be_bytes(),
        );
        digest.update(contents.as_bytes());
    }
    Ok(format!("sha256:{:x}", digest.finalize()))
}

fn collect_dependency_tree(
    root: &Path,
    relative: &Path,
    include_all_files: bool,
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
            if include_all_files {
                return Err(MdbaseRecordCacheError::Read {
                    path: entry_path,
                    source: std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "symlinked control dependencies are not allowed",
                    ),
                });
            }
            continue;
        }
        if file_type.is_dir() {
            let name = entry.file_name();
            if relative.as_os_str().is_empty()
                && matches!(name.to_str(), Some(".git" | ".vulcan" | ".mdbase"))
            {
                continue;
            }
            if !child.as_os_str().is_empty() && entry_path.join("mdbase.yaml").is_file() {
                continue;
            }
            collect_dependency_tree(root, &child, include_all_files, paths)?;
        } else if file_type.is_file()
            && (include_all_files
                || child.extension().and_then(|extension| extension.to_str()) == Some("json"))
        {
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
    let dependency_digest = mdbase_record_dependency_digest(collection)?;
    let collection_root = cache_collection_root(collection)?;
    let records = load_mdbase_records_with_contracts(collection, types, contracts, false)?;
    let next = records
        .records
        .into_iter()
        .map(|record| {
            let path = record.path.clone();
            (
                path,
                MdbaseCachedRecord {
                    collection_root: collection_root.clone(),
                    path: record.path,
                    revision: record.revision,
                    dependency_digest: dependency_digest.clone(),
                    record_model_version: MDBASE_RECORD_MODEL_VERSION,
                    types: record.types,
                    effective_frontmatter: record.effective_frontmatter,
                    display: record.display,
                    contract_views: record.contract_views,
                    diagnostics: record.diagnostics,
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    let previous = load_collection_cache(database.connection(), &collection_root)?;
    let dependency_changed = previous.values().any(|record| {
        record.dependency_digest != dependency_digest
            || record.record_model_version != MDBASE_RECORD_MODEL_VERSION
    });
    let deleted = previous
        .keys()
        .filter(|path| !next.contains_key(*path))
        .count();
    let added = next
        .keys()
        .filter(|path| !previous.contains_key(*path))
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
                    contract_views_json, diagnostics_json
             FROM mdbase_record_cache
             WHERE collection_root = ?1 AND path = ?2 AND revision = ?3
               AND dependency_digest = ?4 AND record_model_version = ?5",
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
                contract_views_json, diagnostics_json
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
    transaction.execute(
        "INSERT INTO mdbase_record_cache (
            collection_root, path, revision, dependency_digest, record_model_version,
            types_json, effective_frontmatter_json, display_json,
            contract_views_json, diagnostics_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(collection_root, path) DO UPDATE SET
            revision = excluded.revision,
            dependency_digest = excluded.dependency_digest,
            record_model_version = excluded.record_model_version,
            types_json = excluded.types_json,
            effective_frontmatter_json = excluded.effective_frontmatter_json,
            display_json = excluded.display_json,
            contract_views_json = excluded.contract_views_json,
            diagnostics_json = excluded.diagnostics_json",
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
    fn dependency_digest_tracks_config_types_contracts_and_external_schemas() {
        let directory = tempdir().expect("collection directory");
        write(
            &directory.path().join("mdbase.yaml"),
            "spec_version: 0.3.0\n",
        );
        write(&directory.path().join("_types/type.md"), "type v1\n");
        write(
            &directory.path().join("_contracts/contract.md"),
            "contract v1\n",
        );
        write(&directory.path().join("schemas/value.json"), "{}\n");
        let collection = load_mdbase_collection(directory.path())
            .expect("collection should load")
            .expect("collection should exist");

        let initial = mdbase_record_dependency_digest(&collection).expect("initial digest");
        write(
            &directory.path().join("mdbase.yaml"),
            "spec_version: 0.3.0\n# changed\n",
        );
        let config = mdbase_record_dependency_digest(&collection).expect("config digest");
        assert_ne!(initial, config);

        write(&directory.path().join("_types/type.md"), "type v2\n");
        let type_file = mdbase_record_dependency_digest(&collection).expect("type digest");
        assert_ne!(config, type_file);

        write(
            &directory.path().join("_contracts/contract.md"),
            "contract v2\n",
        );
        let contract = mdbase_record_dependency_digest(&collection).expect("contract digest");
        assert_ne!(type_file, contract);

        write(
            &directory.path().join("schemas/value.json"),
            "{\"type\":\"object\"}\n",
        );
        let schema = mdbase_record_dependency_digest(&collection).expect("schema digest");
        assert_ne!(contract, schema);
    }
}
