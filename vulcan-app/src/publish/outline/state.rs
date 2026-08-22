use crate::AppError;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use ulid::Ulid;
use vulcan_core::VaultPaths;

const STATE_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutlinePublishState {
    pub version: u32,
    pub profile: String,
    pub collection_id: String,
    #[serde(default)]
    pub documents: BTreeMap<String, OutlineDocumentMapping>,
}

impl OutlinePublishState {
    #[must_use]
    pub fn empty(profile: impl Into<String>, collection_id: impl Into<String>) -> Self {
        Self {
            version: STATE_VERSION,
            profile: profile.into(),
            collection_id: collection_id.into(),
            documents: BTreeMap::new(),
        }
    }

    pub fn validate(
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
        let mut remote_ids = BTreeSet::new();
        for (source_identity, mapping) in &self.documents {
            if source_identity.is_empty()
                || mapping.remote_document_id.is_empty()
                || mapping.source_path.is_empty()
                || mapping.last_published_content_hash.is_empty()
            {
                return Err(AppError::operation(
                    "Outline mapping state contains an incomplete document entry",
                ));
            }
            if !remote_ids.insert(&mapping.remote_document_id) {
                return Err(AppError::operation(
                    "Outline mapping state assigns one remote document to multiple sources",
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
        }
        Ok(())
    }
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
    #[serde(default)]
    pub pending_create: bool,
    #[serde(default)]
    pub pending_archive: bool,
    #[serde(default)]
    pub attachments: BTreeMap<String, OutlineAttachmentMapping>,
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
}

impl OutlineStateLock {
    #[must_use]
    pub fn state_path(&self) -> &Path {
        &self.state_path
    }

    pub fn save(&self, state: &OutlinePublishState) -> Result<(), AppError> {
        state.validate(&state.profile, &state.collection_id)?;
        let parent = self
            .state_path
            .parent()
            .ok_or_else(|| AppError::operation("Outline state path has no parent"))?;
        fs::create_dir_all(parent).map_err(AppError::operation)?;
        let temp_path = parent.join(format!(".state-{}.tmp", Ulid::new()));
        let mut temp = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp_path)
            .map_err(AppError::operation)?;
        let bytes = serde_json::to_vec_pretty(state).map_err(AppError::operation)?;
        temp.write_all(&bytes).map_err(AppError::operation)?;
        temp.write_all(b"\n").map_err(AppError::operation)?;
        temp.sync_all().map_err(AppError::operation)?;
        drop(temp);
        if let Err(error) = fs::rename(&temp_path, &self.state_path) {
            let _ = fs::remove_file(&temp_path);
            return Err(AppError::operation(error));
        }
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(AppError::operation)
    }
}

impl Drop for OutlineStateLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

pub fn load_outline_state(
    paths: &VaultPaths,
    profile: &str,
    collection_id: &str,
) -> Result<OutlinePublishState, AppError> {
    let state_path = outline_state_path(paths, profile)?;
    if !state_path.exists() {
        return Ok(OutlinePublishState::empty(profile, collection_id));
    }
    let bytes = fs::read(&state_path).map_err(AppError::operation)?;
    let state = serde_json::from_slice::<OutlinePublishState>(&bytes).map_err(|error| {
        AppError::operation(format!(
            "malformed Outline mapping state {}: {error}",
            state_path.display()
        ))
    })?;
    state.validate(profile, collection_id)?;
    Ok(state)
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
    Ok(OutlineStateLock { file, state_path })
}

fn outline_state_path(paths: &VaultPaths, profile: &str) -> Result<PathBuf, AppError> {
    if profile.is_empty()
        || !profile
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err(AppError::operation(
            "Outline profile names may contain only ASCII letters, digits, '-' and '_'",
        ));
    }
    Ok(paths
        .vulcan_dir()
        .join("publish")
        .join("outline")
        .join(format!("{profile}.json")))
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
            pending_create: false,
            pending_archive: false,
            attachments: BTreeMap::new(),
        }
    }

    #[test]
    fn state_is_written_atomically_outside_the_rebuildable_cache() {
        let temp = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temp.path());
        let lock = lock_outline_state(&paths, "wiki").expect("state lock");
        let mut state = OutlinePublishState::empty("wiki", "collection");
        state
            .documents
            .insert("source-id".to_string(), mapping("remote"));
        lock.save(&state).expect("save state");
        assert!(lock.state_path().starts_with(paths.vulcan_dir()));
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
    fn malformed_and_duplicate_mapping_state_is_rejected() {
        let temp = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temp.path());
        let lock = lock_outline_state(&paths, "wiki").expect("state lock");
        fs::write(lock.state_path(), b"not json").expect("malformed state");
        assert!(load_outline_state(&paths, "wiki", "collection").is_err());

        let mut state = OutlinePublishState::empty("wiki", "collection");
        state.documents.insert("one".to_string(), mapping("remote"));
        state.documents.insert("two".to_string(), mapping("remote"));
        assert!(state.validate("wiki", "collection").is_err());
    }
}
