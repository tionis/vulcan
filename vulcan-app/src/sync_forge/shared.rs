//! Shared, non-secret forge settings published to the Git remote.
//!
//! The descriptor is a *proposal*: anyone who can push can change it, so it is
//! never read automatically. An administrator adopts it explicitly with
//! `sync forge init --adopt`, which copies the values into device-local
//! configuration after the same-host check. It carries no secrets and no
//! per-device values (no token variable, no repository path).

use super::{validate_forge_url, validate_oauth_client_id, ForgeKind};
use crate::AppError;
use serde::{Deserialize, Serialize};
use vulcan_core::VaultPaths;
use vulcan_sync::{
    GitEngine, GitOid, GitPushResult, GitRefName, GitRemote, RepositoryLock,
    LOCAL_FORGE_DESCRIPTOR_MIRROR_REF, REMOTE_FORGE_DESCRIPTOR_REF,
};

const SHARED_FORGE_VERSION: u32 = 1;
const SHARED_FORGE_FILE: &str = "forge.json";
const MAX_SHARED_FORGE_BYTES: usize = 2048;
const COMMIT_MESSAGE: &str = "vulcan forge settings\n";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SharedForge {
    pub version: u32,
    pub kind: ForgeKind,
    /// API origin, only when it differs from the one derived from the Git
    /// remote. Adoption still requires it to be on the remote's host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_url: Option<String>,
    /// Public OAuth client ID for `sync forge login`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth_client_id: Option<String>,
}

impl SharedForge {
    #[must_use]
    pub fn new(kind: ForgeKind, api_url: Option<String>, oauth_client_id: Option<String>) -> Self {
        Self {
            version: SHARED_FORGE_VERSION,
            kind,
            api_url,
            oauth_client_id,
        }
    }

    /// Strictly parses a stored descriptor; every field is validated the same
    /// way as local configuration.
    pub fn parse(bytes: &[u8]) -> Result<Self, AppError> {
        if bytes.len() > MAX_SHARED_FORGE_BYTES {
            return Err(AppError::operation(
                "shared forge settings exceed their size limit",
            ));
        }
        let value: Self = serde_json::from_slice(bytes).map_err(|error| {
            AppError::operation(format!("invalid shared forge settings: {error}"))
        })?;
        if value.version != SHARED_FORGE_VERSION {
            return Err(AppError::operation(format!(
                "unsupported shared forge settings version {}",
                value.version
            )));
        }
        if let Some(url) = &value.api_url {
            validate_forge_url(url)?;
        }
        if let Some(client_id) = &value.oauth_client_id {
            validate_oauth_client_id(client_id)?;
        }
        Ok(value)
    }
}

/// The remote descriptor and the revision it was read at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SharedForgeView {
    pub settings: SharedForge,
    pub revision: String,
}

fn remote_ref() -> Result<GitRefName, AppError> {
    GitRefName::parse(REMOTE_FORGE_DESCRIPTOR_REF).map_err(AppError::operation)
}

fn open(
    paths: &VaultPaths,
) -> Result<(vulcan_sync::GitCliEngine, vulcan_sync::GitRepository), AppError> {
    let vault = std::fs::canonicalize(paths.vault_root()).map_err(AppError::operation)?;
    let engine = crate::sync_transport::git_engine(paths);
    let repository = engine
        .discover_repository(&vault)
        .map_err(AppError::operation)?;
    Ok((engine, repository))
}

/// Reads the remote descriptor, if any. A malformed one is an error, never
/// silently ignored, because the administrator is about to rely on it.
pub fn read_shared_forge(
    paths: &VaultPaths,
    remote: &GitRemote,
) -> Result<Option<SharedForgeView>, AppError> {
    let (engine, repository) = open(paths)?;
    let _lock = RepositoryLock::acquire(&repository.git_dir)?;
    read_locked(&engine, &repository, remote).map(|found| {
        found.map(|(settings, revision)| SharedForgeView {
            settings,
            revision: revision.to_string(),
        })
    })
}

fn read_locked(
    engine: &dyn GitEngine,
    repository: &vulcan_sync::GitRepository,
    remote: &GitRemote,
) -> Result<Option<(SharedForge, GitOid)>, AppError> {
    let reference = remote_ref()?;
    let Some(revision) = engine
        .remote_ref(repository, remote, &reference)
        .map_err(AppError::operation)?
    else {
        return Ok(None);
    };
    let mirror =
        GitRefName::parse(LOCAL_FORGE_DESCRIPTOR_MIRROR_REF).map_err(AppError::operation)?;
    let fetched = engine
        .fetch_ref(repository, remote, &reference, &mirror)
        .map_err(AppError::operation)?;
    if fetched != revision {
        return Err(AppError::operation(
            "shared forge settings changed while they were being fetched; retry",
        ));
    }
    let entries = engine
        .tree_entries(repository, &revision)
        .map_err(AppError::operation)?;
    let [entry] = entries.as_slice() else {
        return Err(AppError::operation(
            "shared forge settings must be exactly one file",
        ));
    };
    if entry.path != SHARED_FORGE_FILE
        || entry.kind != "blob"
        || !matches!(entry.mode.as_str(), "100644" | "100755")
    {
        return Err(AppError::operation(
            "shared forge settings must be a regular `forge.json`",
        ));
    }
    let bytes = engine
        .path_object(repository, &revision, SHARED_FORGE_FILE)
        .map_err(AppError::operation)?
        .and_then(|object| object.data)
        .ok_or_else(|| AppError::operation("shared forge settings are unreadable"))?;
    Ok(Some((SharedForge::parse(&bytes)?, revision)))
}

/// Publishes `settings`, returning the new revision, or `None` when the
/// remote already holds exactly these settings.
pub(crate) fn publish_shared_forge(
    paths: &VaultPaths,
    remote: &GitRemote,
    settings: &SharedForge,
) -> Result<Option<String>, AppError> {
    let (engine, repository) = open(paths)?;
    let _lock = RepositoryLock::acquire(&repository.git_dir)?;
    let lease = engine
        .remote_ref(&repository, remote, &remote_ref()?)
        .map_err(AppError::operation)?;
    // An unreadable descriptor is replaceable: publishing is the repair path.
    if let Ok(Some((current, _))) = read_locked(&engine, &repository, remote) {
        if &current == settings {
            return Ok(None);
        }
    }
    let mut bytes = serde_json::to_vec_pretty(settings).map_err(AppError::operation)?;
    bytes.push(b'\n');
    let blob = engine
        .write_blob(&repository, &bytes)
        .map_err(AppError::operation)?;
    let tree = engine
        .create_single_file_tree(&repository, SHARED_FORGE_FILE, &blob)
        .map_err(AppError::operation)?;
    let parents = lease.clone().into_iter().collect::<Vec<_>>();
    let commit = engine
        .create_commit(&repository, &tree, &parents, COMMIT_MESSAGE)
        .map_err(AppError::operation)?;
    match engine
        .push_ref(&repository, remote, &commit, &remote_ref()?, lease.as_ref())
        .map_err(AppError::operation)?
    {
        GitPushResult::Updated => Ok(Some(commit.to_string())),
        GitPushResult::Rejected => Err(AppError::operation(
            "shared forge settings changed concurrently; re-run to read the current settings",
        )),
    }
}
