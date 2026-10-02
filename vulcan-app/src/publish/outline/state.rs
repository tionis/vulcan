use crate::keyed_state::{KeyedStateStore, TrackedMap};
use crate::{device_state, AppError};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use vulcan_core::VaultPaths;

const STATE_VERSION: u32 = 1;
const DOCUMENTS: &str = "documents";
const DUPLICATE_REMOTE_DOCUMENT: &str =
    "Outline mapping state assigns one remote document to multiple sources";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutlinePublishState {
    pub version: u32,
    pub profile: String,
    pub collection_id: String,
    /// One durable row per source; only mutated rows are rewritten on save.
    #[serde(default)]
    pub(crate) documents: TrackedMap<OutlineDocumentMapping>,
}

impl OutlinePublishState {
    #[must_use]
    pub fn empty(profile: impl Into<String>, collection_id: impl Into<String>) -> Self {
        Self {
            version: STATE_VERSION,
            profile: profile.into(),
            collection_id: collection_id.into(),
            documents: TrackedMap::default(),
        }
    }

    fn validate_header(
        &self,
        expected_profile: &str,
        expected_collection: &str,
    ) -> Result<(), AppError> {
        if self.version != STATE_VERSION {
            return Err(AppError::operation(format!(
                "unsupported Outline mapping state version {}",
                self.version
            )));
        }
        if self.profile != expected_profile || self.collection_id != expected_collection {
            return Err(AppError::operation(
                "Outline mapping state belongs to a different profile or collection",
            ));
        }
        Ok(())
    }

    pub fn validate(
        &self,
        expected_profile: &str,
        expected_collection: &str,
    ) -> Result<(), AppError> {
        self.validate_header(expected_profile, expected_collection)?;
        let mut remote_ids = BTreeSet::new();
        for (source_identity, mapping) in &self.documents {
            validate_mapping(source_identity, mapping)?;
            if !remote_ids.insert(&mapping.remote_document_id) {
                return Err(AppError::operation(DUPLICATE_REMOTE_DOCUMENT));
            }
        }
        Ok(())
    }
}

/// Checks one mapping on its own; uniqueness across mappings is enforced by
/// [`OutlinePublishState::validate`] on load and by store claims on save.
fn validate_mapping(
    source_identity: &str,
    mapping: &OutlineDocumentMapping,
) -> Result<(), AppError> {
    if source_identity.is_empty()
        || mapping.remote_document_id.is_empty()
        || mapping.source_path.is_empty()
        || mapping.last_published_content_hash.is_empty()
    {
        return Err(AppError::operation(
            "Outline mapping state contains an incomplete document entry",
        ));
    }
    if mapping
        .last_observed_remote
        .as_ref()
        .is_some_and(|snapshot| snapshot.content_hash.is_empty())
    {
        return Err(AppError::operation(
            "Outline mapping state contains an incomplete remote snapshot",
        ));
    }
    if mapping.attachments.values().any(|attachment| {
        attachment.remote_attachment_id.is_empty()
            || attachment.remote_url.is_empty()
            || attachment.content_hash.is_empty()
            || attachment.owner_remote_document_id.is_empty()
    }) {
        return Err(AppError::operation(
            "Outline mapping state contains an incomplete attachment entry",
        ));
    }
    Ok(())
}

fn remote_document_claims(mapping: &OutlineDocumentMapping) -> Vec<String> {
    vec![mapping.remote_document_id.clone()]
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutlineDocumentMapping {
    pub source_path: String,
    pub source_document_id: String,
    pub remote_document_id: String,
    pub last_published_content_hash: String,
    pub last_published_title: String,
    pub remote_parent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_observed_remote: Option<OutlineRemoteSnapshot>,
    #[serde(default)]
    pub pending_create: bool,
    #[serde(default)]
    pub pending_archive: bool,
    #[serde(default)]
    pub attachments: BTreeMap<String, OutlineAttachmentMapping>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutlineRemoteSnapshot {
    pub content_hash: String,
    pub title: String,
    pub parent_document_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutlineAttachmentMapping {
    pub remote_attachment_id: String,
    pub remote_url: String,
    pub content_hash: String,
    pub owner_remote_document_id: String,
}

pub struct OutlineStateLock {
    file: File,
    state_path: PathBuf,
    store: KeyedStateStore,
}

impl OutlineStateLock {
    #[must_use]
    pub fn state_path(&self) -> &Path {
        &self.state_path
    }

    /// Persists the mappings changed since the last save in one transaction.
    pub fn save(&mut self, state: &mut OutlinePublishState) -> Result<(), AppError> {
        state.validate_header(&state.profile, &state.collection_id)?;
        for key in state.documents.dirty_keys() {
            if let Some(mapping) = state.documents.get(key) {
                validate_mapping(key, mapping)?;
            }
        }
        write_state(&mut self.store, state)?;
        state.documents.mark_persisted();
        Ok(())
    }
}

fn write_state(store: &mut KeyedStateStore, state: &OutlinePublishState) -> Result<(), AppError> {
    store.write(|writer| {
        writer.set_meta("version", &state.version)?;
        writer.set_meta("profile", &state.profile)?;
        writer.set_meta("collection_id", &state.collection_id)?;
        writer.write_dirty(
            DOCUMENTS,
            &state.documents,
            remote_document_claims,
            DUPLICATE_REMOTE_DOCUMENT,
        )?;
        writer.mark_initialized()
    })
}

impl Drop for OutlineStateLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

/// Reads the mapping state without locking or migrating anything.
fn read_state(paths: &VaultPaths, profile: &str) -> Result<Option<OutlinePublishState>, AppError> {
    validate_profile(profile)?;
    let store_path = outline_state_path(paths, profile)?;
    if let Some(store) = KeyedStateStore::open_read_only(&store_path)? {
        return load_from_store(&store).map(Some);
    }
    // State written before the keyed store is read until the next locked
    // operation migrates it.
    let legacy_path = device_state::readable_path(paths, &outline_legacy_relative_path(profile))?;
    if !legacy_path.exists() {
        return Ok(None);
    }
    let bytes = fs::read(&legacy_path).map_err(AppError::operation)?;
    serde_json::from_slice::<OutlinePublishState>(&bytes)
        .map(Some)
        .map_err(|error| {
            AppError::operation(format!(
                "malformed Outline mapping state {}: {error}",
                legacy_path.display()
            ))
        })
}

fn load_from_store(store: &KeyedStateStore) -> Result<OutlinePublishState, AppError> {
    Ok(OutlinePublishState {
        version: required_meta(store, "version")?,
        profile: required_meta(store, "profile")?,
        collection_id: required_meta(store, "collection_id")?,
        documents: store.load_map(DOCUMENTS)?,
    })
}

fn required_meta<T: serde::de::DeserializeOwned>(
    store: &KeyedStateStore,
    key: &str,
) -> Result<T, AppError> {
    store
        .meta(key)?
        .ok_or_else(|| AppError::operation(format!("Outline mapping state is missing `{key}`")))
}

pub fn load_outline_state(
    paths: &VaultPaths,
    profile: &str,
    collection_id: &str,
) -> Result<OutlinePublishState, AppError> {
    let Some(state) = read_state(paths, profile)? else {
        return Ok(OutlinePublishState::empty(profile, collection_id));
    };
    state.validate(profile, collection_id)?;
    Ok(state)
}

pub fn outline_state_collection_id(
    paths: &VaultPaths,
    profile: &str,
) -> Result<Option<String>, AppError> {
    let Some(state) = read_state(paths, profile)? else {
        return Ok(None);
    };
    state.validate(profile, &state.collection_id)?;
    Ok(Some(state.collection_id))
}

pub fn lock_outline_state(paths: &VaultPaths, profile: &str) -> Result<OutlineStateLock, AppError> {
    let state_path = outline_state_path(paths, profile)?;
    let parent = state_path
        .parent()
        .ok_or_else(|| AppError::operation("Outline state path has no parent"))?;
    fs::create_dir_all(parent).map_err(AppError::operation)?;
    let lock_path = state_path.with_extension("lock");
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .map_err(AppError::operation)?;
    file.try_lock_exclusive().map_err(|error| {
        AppError::operation(format!(
            "Outline publisher state is locked by another process: {error}"
        ))
    })?;
    let mut store = KeyedStateStore::open(&state_path)?;
    if !store.is_initialized()? {
        device_state::migrate_file(paths, &outline_legacy_relative_path(profile))?;
        migrate_legacy_state(paths, profile, &mut store)?;
    }
    Ok(OutlineStateLock {
        file,
        state_path,
        store,
    })
}

/// Imports a JSON mapping file into the keyed store, then keeps the file as a
/// `.json.migrated` backup. A failed import leaves the JSON file authoritative.
fn migrate_legacy_state(
    paths: &VaultPaths,
    profile: &str,
    store: &mut KeyedStateStore,
) -> Result<(), AppError> {
    let legacy_path = device_state::path(paths, &outline_legacy_relative_path(profile))?;
    if !legacy_path.is_file() {
        return Ok(());
    }
    let bytes = fs::read(&legacy_path).map_err(AppError::operation)?;
    let state = serde_json::from_slice::<OutlinePublishState>(&bytes).map_err(|error| {
        AppError::operation(format!(
            "malformed Outline mapping state {}: {error}",
            legacy_path.display()
        ))
    })?;
    if state.profile != profile {
        return Err(AppError::operation(
            "Outline mapping state belongs to a different profile or collection",
        ));
    }
    state.validate(profile, &state.collection_id)?;
    write_state(store, &state)?;
    fs::rename(&legacy_path, legacy_path.with_extension("json.migrated"))
        .map_err(AppError::operation)
}

fn outline_state_path(paths: &VaultPaths, profile: &str) -> Result<PathBuf, AppError> {
    validate_profile(profile)?;
    device_state::path(
        paths,
        &PathBuf::from("publish/outline").join(format!("{profile}.sqlite")),
    )
}

fn validate_profile(profile: &str) -> Result<(), AppError> {
    if profile.is_empty()
        || !profile
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err(AppError::operation(
            "Outline profile names may contain only ASCII letters, digits, '-' and '_'",
        ));
    }
    Ok(())
}

/// The JSON file used before the keyed store, still read for migration.
fn outline_legacy_relative_path(profile: &str) -> PathBuf {
    PathBuf::from("publish/outline").join(format!("{profile}.json"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn mapping(remote_id: &str) -> OutlineDocumentMapping {
        OutlineDocumentMapping {
            source_path: "Projects.md".to_string(),
            source_document_id: "cache-id".to_string(),
            remote_document_id: remote_id.to_string(),
            last_published_content_hash: "hash".to_string(),
            last_published_title: "Projects".to_string(),
            remote_parent_id: None,
            last_observed_remote: None,
            pending_create: false,
            pending_archive: false,
            attachments: BTreeMap::new(),
        }
    }

    #[test]
    fn state_is_written_atomically_outside_the_rebuildable_cache() {
        let temp = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temp.path());
        let mut lock = lock_outline_state(&paths, "wiki").expect("state lock");
        let mut state = OutlinePublishState::empty("wiki", "collection");
        state
            .documents
            .insert("source-id".to_string(), mapping("remote"));
        lock.save(&mut state).expect("save state");
        state
            .documents
            .get_mut("source-id")
            .expect("mapping")
            .last_published_title = "Updated Projects".to_string();
        lock.save(&mut state).expect("replace existing state");
        assert!(lock
            .state_path()
            .starts_with(paths.operational_state_dir().expect("state root")));
        assert_ne!(lock.state_path(), paths.cache_db());
        drop(lock);

        assert_eq!(
            load_outline_state(&paths, "wiki", "collection").expect("load state"),
            state
        );
    }

    #[test]
    fn read_only_state_load_does_not_create_directories() {
        let temp = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temp.path());
        let state = load_outline_state(&paths, "wiki", "collection").expect("empty state");
        assert!(state.documents.is_empty());
        assert!(!paths.vulcan_dir().join("publish").exists());
    }

    #[test]
    fn state_without_remote_snapshot_remains_compatible() {
        let mapping: OutlineDocumentMapping = serde_json::from_str(
            r#"{
                "source_path":"Projects.md",
                "source_document_id":"cache-id",
                "remote_document_id":"remote",
                "last_published_content_hash":"hash",
                "last_published_title":"Projects",
                "remote_parent_id":null,
                "pending_create":false,
                "pending_archive":false,
                "attachments":{}
            }"#,
        )
        .expect("legacy mapping");

        assert_eq!(mapping, self::mapping("remote"));
    }

    #[test]
    fn malformed_and_duplicate_mapping_state_is_rejected() {
        let temp = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temp.path());
        let legacy =
            device_state::path(&paths, &outline_legacy_relative_path("wiki")).expect("legacy path");
        fs::create_dir_all(legacy.parent().expect("parent")).expect("state directory");
        fs::write(&legacy, b"not json").expect("malformed state");
        assert!(load_outline_state(&paths, "wiki", "collection").is_err());
        assert!(
            lock_outline_state(&paths, "wiki").is_err(),
            "malformed legacy state must not be migrated"
        );
        fs::remove_file(&legacy).expect("remove malformed state");

        let mut state = OutlinePublishState::empty("wiki", "collection");
        state.documents.insert("one".to_string(), mapping("remote"));
        state.documents.insert("two".to_string(), mapping("remote"));
        assert!(state.validate("wiki", "collection").is_err());

        let mut lock = lock_outline_state(&paths, "wiki").expect("state lock");
        let error = lock
            .save(&mut state)
            .expect_err("the store rejects one remote document for two sources");
        assert!(error.to_string().contains("multiple sources"), "{error}");
    }

    #[test]
    fn legacy_json_state_is_migrated_once_and_kept_as_backup() {
        let temp = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temp.path());
        let mut legacy_state = OutlinePublishState::empty("wiki", "collection");
        legacy_state
            .documents
            .insert("source-id".to_string(), mapping("remote"));
        let legacy =
            device_state::path(&paths, &outline_legacy_relative_path("wiki")).expect("legacy path");
        fs::create_dir_all(legacy.parent().expect("parent")).expect("state directory");
        fs::write(
            &legacy,
            serde_json::to_vec_pretty(&legacy_state).expect("legacy json"),
        )
        .expect("legacy state");
        assert_eq!(
            load_outline_state(&paths, "wiki", "collection").expect("read legacy state"),
            legacy_state
        );

        let mut lock = lock_outline_state(&paths, "wiki").expect("migrating lock");
        assert!(!legacy.exists());
        assert!(legacy.with_extension("json.migrated").is_file());
        let mut state = load_outline_state(&paths, "wiki", "collection").expect("migrated");
        assert_eq!(state, legacy_state);
        assert!(state.documents.dirty_keys().is_empty());

        state.documents.remove("source-id");
        lock.save(&mut state).expect("remove mapping");
        drop(lock);
        assert!(load_outline_state(&paths, "wiki", "collection")
            .expect("reload")
            .documents
            .is_empty());
        drop(lock_outline_state(&paths, "wiki").expect("relock"));
        assert!(
            load_outline_state(&paths, "wiki", "collection")
                .expect("not re-migrated")
                .documents
                .is_empty(),
            "the backup must never be imported again"
        );
    }
}
