//! Durable device-local conflict records and preserved file artifacts.

use crate::sync_state::SyncStateStore;
use crate::AppError;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use tempfile::NamedTempFile;
use vulcan_sync::{GitEngine, GitOid, GitRepository, GitSyncConflict};

pub const SYNC_CONFLICT_RECORD_VERSION: u32 = 1;
const MAX_CONFLICT_RECORD_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncConflictRecord {
    pub version: u32,
    pub id: String,
    pub repository_key: String,
    pub work_tree: PathBuf,
    pub base_revision: Option<String>,
    pub local_revision: String,
    pub remote_revision: String,
    pub policy_version: u32,
    pub policy_hash: String,
    pub preserved_base_ref: Option<String>,
    pub preserved_local_ref: String,
    pub preserved_remote_ref: String,
    pub paths: Vec<SyncConflictPathRecord>,
    pub diagnostics: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncConflictPathRecord {
    pub path: String,
    pub base: SyncConflictSideRecord,
    pub local: SyncConflictSideRecord,
    pub remote: SyncConflictSideRecord,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncConflictSideRecord {
    pub revision: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub object_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SyncConflictSummary {
    pub id: String,
    pub paths: Vec<String>,
    pub base_revision: Option<String>,
    pub local_revision: String,
    pub remote_revision: String,
    pub policy_version: u32,
    pub resolution: SyncConflictResolutionState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncConflictResolutionState {
    Unresolved,
    Resolved,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SyncConflictListReport {
    pub vault: PathBuf,
    pub repository_key: String,
    pub count: usize,
    pub conflicts: Vec<SyncConflictSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SyncConflictDetailReport {
    pub record: SyncConflictRecord,
    pub resolution: SyncConflictResolutionState,
}

pub fn list_sync_conflicts(
    paths: &vulcan_core::VaultPaths,
) -> Result<SyncConflictListReport, AppError> {
    let state_store = SyncStateStore::user_default()?;
    list_sync_conflicts_with_state_store(paths, &state_store)
}

pub fn list_sync_conflicts_with_state_store(
    paths: &vulcan_core::VaultPaths,
    state_store: &SyncStateStore,
) -> Result<SyncConflictListReport, AppError> {
    let work_tree = fs::canonicalize(paths.vault_root()).map_err(AppError::operation)?;
    let repository_key = crate::sync_state::repository_state_key(&work_tree);
    let records = SyncConflictStore::from_state_store(state_store).list(&repository_key)?;
    let conflicts = records
        .into_iter()
        .map(|record| SyncConflictSummary {
            id: record.id,
            paths: record.paths.into_iter().map(|path| path.path).collect(),
            base_revision: record.base_revision,
            local_revision: record.local_revision,
            remote_revision: record.remote_revision,
            policy_version: record.policy_version,
            resolution: SyncConflictResolutionState::Unresolved,
        })
        .collect::<Vec<_>>();
    Ok(SyncConflictListReport {
        vault: work_tree,
        repository_key,
        count: conflicts.len(),
        conflicts,
    })
}

pub fn get_sync_conflict(
    paths: &vulcan_core::VaultPaths,
    conflict_id: &str,
) -> Result<SyncConflictDetailReport, AppError> {
    let state_store = SyncStateStore::user_default()?;
    get_sync_conflict_with_state_store(paths, conflict_id, &state_store)
}

pub fn get_sync_conflict_with_state_store(
    paths: &vulcan_core::VaultPaths,
    conflict_id: &str,
    state_store: &SyncStateStore,
) -> Result<SyncConflictDetailReport, AppError> {
    let work_tree = fs::canonicalize(paths.vault_root()).map_err(AppError::operation)?;
    let repository_key = crate::sync_state::repository_state_key(&work_tree);
    let record =
        SyncConflictStore::from_state_store(state_store).get(&repository_key, conflict_id)?;
    Ok(SyncConflictDetailReport {
        record,
        resolution: SyncConflictResolutionState::Unresolved,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncConflictStore {
    root: PathBuf,
}

impl SyncConflictStore {
    #[must_use]
    pub fn from_state_store(state_store: &SyncStateStore) -> Self {
        Self {
            root: state_store.root().to_path_buf(),
        }
    }

    #[must_use]
    pub fn at(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn persist(
        &self,
        engine: &dyn GitEngine,
        repository: &GitRepository,
        repository_key: &str,
        conflict: &GitSyncConflict,
    ) -> Result<SyncConflictRecord, AppError> {
        validate_hex_id("repository key", repository_key)?;
        validate_hex_id("conflict ID", &conflict.id)?;
        let directory = self.conflict_directory(repository_key, &conflict.id)?;
        let record_path = directory.join("record.json");
        if record_path.exists() {
            let existing = self.get(repository_key, &conflict.id)?;
            verify_record_inputs(&existing, conflict)?;
            return Ok(existing);
        }
        let work_tree = repository.work_tree.clone().ok_or_else(|| {
            AppError::operation("cannot preserve a sync conflict for a bare repository")
        })?;
        fs::create_dir_all(directory.join("artifacts")).map_err(AppError::operation)?;
        let mut paths = Vec::with_capacity(conflict.paths.len());
        for (index, path) in conflict.paths.iter().enumerate() {
            paths.push(SyncConflictPathRecord {
                path: path.clone(),
                base: preserve_side(
                    engine,
                    repository,
                    &directory,
                    index,
                    "base",
                    conflict.base.as_ref(),
                    path,
                )?,
                local: preserve_side(
                    engine,
                    repository,
                    &directory,
                    index,
                    "local",
                    Some(&conflict.local),
                    path,
                )?,
                remote: preserve_side(
                    engine,
                    repository,
                    &directory,
                    index,
                    "remote",
                    Some(&conflict.remote),
                    path,
                )?,
            });
        }
        let record = SyncConflictRecord {
            version: SYNC_CONFLICT_RECORD_VERSION,
            id: conflict.id.clone(),
            repository_key: repository_key.to_string(),
            work_tree,
            base_revision: conflict.base.as_ref().map(ToString::to_string),
            local_revision: conflict.local.to_string(),
            remote_revision: conflict.remote.to_string(),
            policy_version: conflict.policy_version,
            policy_hash: conflict.policy_hash.clone(),
            preserved_base_ref: conflict
                .preserved_refs
                .base
                .as_ref()
                .map(ToString::to_string),
            preserved_local_ref: conflict.preserved_refs.local.to_string(),
            preserved_remote_ref: conflict.preserved_refs.remote.to_string(),
            paths,
            diagnostics: conflict.diagnostics.clone(),
        };
        write_json_noclobber(&record_path, &record)?;
        Ok(record)
    }

    pub fn list(&self, repository_key: &str) -> Result<Vec<SyncConflictRecord>, AppError> {
        validate_hex_id("repository key", repository_key)?;
        let root = self.root.join(repository_key).join("conflicts");
        let entries = match fs::read_dir(root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(AppError::operation(error)),
        };
        let mut records = Vec::new();
        for entry in entries {
            let entry = entry.map_err(AppError::operation)?;
            if !entry.file_type().map_err(AppError::operation)?.is_dir() {
                continue;
            }
            let id = entry.file_name().to_string_lossy().to_string();
            validate_hex_id("conflict ID", &id)?;
            records.push(self.get(repository_key, &id)?);
        }
        records.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(records)
    }

    pub fn get(
        &self,
        repository_key: &str,
        conflict_id: &str,
    ) -> Result<SyncConflictRecord, AppError> {
        let path = self
            .conflict_directory(repository_key, conflict_id)?
            .join("record.json");
        let metadata = fs::metadata(&path).map_err(AppError::operation)?;
        if metadata.len() > MAX_CONFLICT_RECORD_BYTES {
            return Err(AppError::operation(format!(
                "sync conflict record at {} exceeds the {} byte limit",
                path.display(),
                MAX_CONFLICT_RECORD_BYTES
            )));
        }
        let record: SyncConflictRecord =
            serde_json::from_slice(&fs::read(&path).map_err(AppError::operation)?)
                .map_err(AppError::operation)?;
        validate_record(&record, repository_key, conflict_id)?;
        Ok(record)
    }

    fn conflict_directory(
        &self,
        repository_key: &str,
        conflict_id: &str,
    ) -> Result<PathBuf, AppError> {
        validate_hex_id("repository key", repository_key)?;
        validate_hex_id("conflict ID", conflict_id)?;
        Ok(self
            .root
            .join(repository_key)
            .join("conflicts")
            .join(conflict_id))
    }
}

fn preserve_side(
    engine: &dyn GitEngine,
    repository: &GitRepository,
    conflict_directory: &Path,
    index: usize,
    side: &str,
    revision: Option<&GitOid>,
    path: &str,
) -> Result<SyncConflictSideRecord, AppError> {
    let Some(revision) = revision else {
        return Ok(SyncConflictSideRecord {
            revision: "absent".to_string(),
            object_id: None,
            mode: None,
            kind: None,
            artifact: None,
            content_hash: None,
            bytes: None,
        });
    };
    let object = engine
        .path_object(repository, revision, path)
        .map_err(AppError::operation)?;
    let Some(object) = object else {
        return Ok(SyncConflictSideRecord {
            revision: revision.to_string(),
            object_id: None,
            mode: None,
            kind: None,
            artifact: None,
            content_hash: None,
            bytes: None,
        });
    };
    let (artifact, content_hash, bytes) = if let Some(data) = object.data {
        let relative = PathBuf::from(format!("artifacts/{index:04}-{side}.bin"));
        let path = conflict_directory.join(&relative);
        write_bytes_noclobber(&path, &data)?;
        (
            Some(relative),
            Some(blake3::hash(&data).to_hex().to_string()),
            Some(data.len() as u64),
        )
    } else {
        (None, None, None)
    };
    Ok(SyncConflictSideRecord {
        revision: revision.to_string(),
        object_id: Some(object.oid.to_string()),
        mode: Some(object.mode),
        kind: Some(object.kind),
        artifact,
        content_hash,
        bytes,
    })
}

fn write_json_noclobber(path: &Path, value: &SyncConflictRecord) -> Result<(), AppError> {
    let parent = path
        .parent()
        .ok_or_else(|| AppError::operation("conflict record has no parent directory"))?;
    let bytes = serde_json::to_vec_pretty(value).map_err(AppError::operation)?;
    let mut temporary = NamedTempFile::new_in(parent).map_err(AppError::operation)?;
    temporary.write_all(&bytes).map_err(AppError::operation)?;
    temporary.write_all(b"\n").map_err(AppError::operation)?;
    temporary
        .as_file()
        .sync_all()
        .map_err(AppError::operation)?;
    temporary
        .persist_noclobber(path)
        .map_err(|error| AppError::operation(error.error))?;
    Ok(())
}

fn write_bytes_noclobber(path: &Path, bytes: &[u8]) -> Result<(), AppError> {
    if path.exists() {
        let existing = fs::read(path).map_err(AppError::operation)?;
        if existing == bytes {
            return Ok(());
        }
        return Err(AppError::operation(format!(
            "immutable conflict artifact differs at {}",
            path.display()
        )));
    }
    let parent = path
        .parent()
        .ok_or_else(|| AppError::operation("conflict artifact has no parent directory"))?;
    let mut temporary = NamedTempFile::new_in(parent).map_err(AppError::operation)?;
    temporary.write_all(bytes).map_err(AppError::operation)?;
    temporary
        .as_file()
        .sync_all()
        .map_err(AppError::operation)?;
    temporary
        .persist_noclobber(path)
        .map_err(|error| AppError::operation(error.error))?;
    Ok(())
}

fn validate_record(
    record: &SyncConflictRecord,
    repository_key: &str,
    conflict_id: &str,
) -> Result<(), AppError> {
    if record.version != SYNC_CONFLICT_RECORD_VERSION
        || record.repository_key != repository_key
        || record.id != conflict_id
    {
        return Err(AppError::operation(
            "sync conflict record version or identity mismatch",
        ));
    }
    Ok(())
}

fn verify_record_inputs(
    record: &SyncConflictRecord,
    conflict: &GitSyncConflict,
) -> Result<(), AppError> {
    if record.base_revision.as_deref() != conflict.base.as_ref().map(GitOid::as_str)
        || record.local_revision != conflict.local.as_str()
        || record.remote_revision != conflict.remote.as_str()
        || record.policy_version != conflict.policy_version
        || record.policy_hash != conflict.policy_hash
        || record
            .paths
            .iter()
            .map(|path| &path.path)
            .ne(conflict.paths.iter())
    {
        return Err(AppError::operation(format!(
            "immutable conflict record `{}` does not match the current conflict inputs",
            conflict.id
        )));
    }
    Ok(())
}

fn validate_hex_id(label: &str, value: &str) -> Result<(), AppError> {
    if value.len() == 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        Ok(())
    } else {
        Err(AppError::operation(format!("invalid {label} `{value}`")))
    }
}
