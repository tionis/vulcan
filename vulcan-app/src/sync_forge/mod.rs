//! Forge deploy-key reconciliation (Roadmap 12.21.3).
//!
//! `forge sync` makes a repository's deploy keys agree with the device
//! registrations in its Git remote. The registration list is trusted as
//! written; see `docs/specs/device-transport-auth.md`. Adapter configuration is
//! device-local and never read from vault files, because a synced value could
//! redirect an administrator's token to another host.

use crate::device_identity::identity_from_public_key;
use crate::sync_registration::{
    list_registrations, RegistrationObservation, RegistrationStatus, RegistrationSummary,
};
use crate::sync_state::SyncStateStore;
use crate::AppError;
use serde::{Deserialize, Serialize};
use vulcan_core::VaultPaths;
use vulcan_sync::{GitRemote, GitSyncDeviceId, GitSyncDeviceIdKind};

mod derive;
#[cfg(feature = "web")]
mod forgejo;
mod init;
#[cfg(feature = "web")]
mod oauth;
mod shared;

pub use derive::{derive_forge_target, ForgeTarget};
#[cfg(feature = "web")]
pub use forgejo::ForgejoDeployKeys;
pub use init::{forge_init, ForgeInitReport, ForgeInitRequest};
#[cfg(feature = "web")]
pub use oauth::{
    forge_login, forge_logout, forge_oauth_status, open_in_browser, resolve_forge_credential,
    CredentialSource, ForgeCredential, ForgeLoginReport, ForgeLogoutReport, ForgeOAuthStatus,
    DEFAULT_LOGIN_TIMEOUT,
};
pub use shared::{read_shared_forge, SharedForge, SharedForgeView};

pub const SYNC_FORGE_REPORT_VERSION: u32 = 1;
const FORGE_CONFIG_FILE: &str = "forge.json";
const FORGE_CONFIG_VERSION: u32 = 1;
const MAX_FORGE_CONFIG_BYTES: u64 = 4 * 1024;
/// Title prefix that marks a deploy key as Vulcan-managed.
pub const DEVICE_KEY_TITLE_PREFIX: &str = "vulcan-device:";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForgeKind {
    Forgejo,
}

impl ForgeKind {
    /// `(authorize, token)` endpoint paths for the OAuth authorization-code
    /// flow, relative to the forge's base URL. Everything provider-specific
    /// lives here, so no forge is special-cased elsewhere.
    #[must_use]
    pub const fn oauth_paths(self) -> (&'static str, &'static str) {
        match self {
            Self::Forgejo => ("login/oauth/authorize", "login/oauth/access_token"),
        }
    }
}

/// Device-local forge settings for one vault. Holds the *name* of a token
/// environment variable and/or a public OAuth client ID, never a secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForgeConfig {
    pub version: u32,
    pub kind: ForgeKind,
    pub url: String,
    /// `owner/name`.
    pub repo: String,
    /// Environment variable holding an API token (the fallback credential).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_env: Option<String>,
    /// Public OAuth client ID for `sync forge login`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth_client_id: Option<String>,
}

impl ForgeConfig {
    pub fn new(
        kind: ForgeKind,
        url: &str,
        repo: &str,
        token_env: Option<&str>,
        oauth_client_id: Option<&str>,
    ) -> Result<Self, AppError> {
        let config = Self {
            version: FORGE_CONFIG_VERSION,
            kind,
            url: url.trim().to_owned(),
            repo: repo.trim().to_owned(),
            token_env: token_env.map(|value| value.trim().to_owned()),
            oauth_client_id: oauth_client_id.map(|value| value.trim().to_owned()),
        };
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<(), AppError> {
        if self.version != FORGE_CONFIG_VERSION {
            return Err(AppError::operation(format!(
                "unsupported forge configuration version {}",
                self.version
            )));
        }
        validate_forge_url(&self.url)?;
        self.owner_and_repo()?;
        if let Some(name) = &self.token_env {
            let env_ok = !name.is_empty()
                && name.len() <= 128
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                && !name.starts_with(|c: char| c.is_ascii_digit());
            if !env_ok {
                return Err(AppError::operation(
                    "token environment variable must be a plain name such as FORGEJO_TOKEN",
                ));
            }
        }
        if let Some(client_id) = &self.oauth_client_id {
            validate_oauth_client_id(client_id)?;
        }
        if self.token_env.is_none() && self.oauth_client_id.is_none() {
            return Err(AppError::operation(
                "configure a credential: --oauth-client-id for `sync forge login`, --token-env for an API token, or both",
            ));
        }
        Ok(())
    }

    /// Splits the validated `owner/name` pair.
    pub fn owner_and_repo(&self) -> Result<(&str, &str), AppError> {
        let segment_ok = |segment: &str| {
            !segment.is_empty()
                && segment.len() <= 100
                && segment != "."
                && segment != ".."
                && !(segment.len() >= 4
                    && segment[segment.len() - 4..].eq_ignore_ascii_case(".git"))
                && segment
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        };
        match self.repo.split_once('/') {
            Some((owner, name)) if segment_ok(owner) && segment_ok(name) => Ok((owner, name)),
            _ => Err(AppError::operation(
                "repository must be `owner/name` using letters, digits, `.`, `_`, and `-`",
            )),
        }
    }
}

/// OAuth client IDs are public identifiers (often UUIDs); keep them to a safe
/// charset so they can sit in URLs and shared descriptors.
pub(crate) fn validate_oauth_client_id(client_id: &str) -> Result<(), AppError> {
    let ok = !client_id.is_empty()
        && client_id.len() <= 128
        && client_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~'));
    if ok {
        Ok(())
    } else {
        Err(AppError::operation(
            "OAuth client ID must be 1-128 letters, digits, `-`, `_`, `.`, or `~`",
        ))
    }
}

/// The forge URL receives the API token, so require HTTPS except for loopback
/// (local testing) and forbid credentials, queries, and fragments.
pub(crate) fn validate_forge_url(text: &str) -> Result<(), AppError> {
    let url = reqwest_free_parse(text)?;
    let loopback = matches!(url.host.as_str(), "localhost" | "127.0.0.1" | "[::1]");
    if url.scheme == "https" || (url.scheme == "http" && loopback) {
        Ok(())
    } else {
        Err(AppError::operation(
            "forge URL must use https (plain http is allowed only for localhost)",
        ))
    }
}

struct ParsedUrl {
    scheme: String,
    host: String,
}

/// Minimal URL screening that does not depend on the optional `web` feature.
fn reqwest_free_parse(text: &str) -> Result<ParsedUrl, AppError> {
    let invalid = || {
        AppError::operation(
            "forge URL must be an http(s) origin without credentials, query, or fragment",
        )
    };
    let (scheme, rest) = text.split_once("://").ok_or_else(invalid)?;
    if !matches!(scheme, "http" | "https")
        || rest.is_empty()
        || text.contains(|c: char| c.is_whitespace() || c.is_control())
        || rest.contains(['?', '#', '@', '\\'])
    {
        return Err(invalid());
    }
    let authority = rest.split('/').next().unwrap_or_default();
    let host = if let Some(stripped) = authority.strip_prefix('[') {
        let end = stripped.find(']').ok_or_else(invalid)?;
        format!("[{}]", &stripped[..end])
    } else {
        authority.split(':').next().unwrap_or_default().to_owned()
    };
    if host.is_empty() {
        return Err(invalid());
    }
    Ok(ParsedUrl {
        scheme: scheme.to_owned(),
        host: host.to_ascii_lowercase(),
    })
}

pub(crate) fn load_config(
    paths: &VaultPaths,
    state: &SyncStateStore,
) -> Result<Option<ForgeConfig>, AppError> {
    let Some(bytes) = state.read_vault_local(paths, FORGE_CONFIG_FILE, MAX_FORGE_CONFIG_BYTES)?
    else {
        return Ok(None);
    };
    let config: ForgeConfig = serde_json::from_slice(&bytes)
        .map_err(|error| AppError::operation(format!("invalid forge configuration: {error}")))?;
    config.validate()?;
    Ok(Some(config))
}

/// Returns the saved configuration for this vault, if any.
pub fn show_forge_config(paths: &VaultPaths) -> Result<Option<ForgeConfig>, AppError> {
    load_config(paths, &SyncStateStore::user_default()?)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ForgeConfigChangeReport {
    pub version: u32,
    pub dry_run: bool,
    pub changed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<ForgeConfig>,
}

pub fn set_forge_config(
    paths: &VaultPaths,
    config: &ForgeConfig,
    dry_run: bool,
) -> Result<ForgeConfigChangeReport, AppError> {
    set_forge_config_with(paths, &SyncStateStore::user_default()?, config, dry_run)
}

pub(crate) fn set_forge_config_with(
    paths: &VaultPaths,
    state: &SyncStateStore,
    config: &ForgeConfig,
    dry_run: bool,
) -> Result<ForgeConfigChangeReport, AppError> {
    config.validate()?;
    let changed = load_config(paths, state)?.as_ref() != Some(config);
    if changed && !dry_run {
        let bytes = serde_json::to_vec_pretty(config).map_err(AppError::operation)?;
        state.write_vault_local(paths, FORGE_CONFIG_FILE, &bytes)?;
    }
    Ok(ForgeConfigChangeReport {
        version: SYNC_FORGE_REPORT_VERSION,
        dry_run,
        changed,
        config: Some(config.clone()),
    })
}

pub fn clear_forge_config(
    paths: &VaultPaths,
    dry_run: bool,
) -> Result<ForgeConfigChangeReport, AppError> {
    clear_forge_config_with(paths, &SyncStateStore::user_default()?, dry_run)
}

fn clear_forge_config_with(
    paths: &VaultPaths,
    state: &SyncStateStore,
    dry_run: bool,
) -> Result<ForgeConfigChangeReport, AppError> {
    let existing = load_config(paths, state)?;
    if existing.is_some() && !dry_run {
        state.remove_vault_local(paths, FORGE_CONFIG_FILE)?;
    }
    Ok(ForgeConfigChangeReport {
        version: SYNC_FORGE_REPORT_VERSION,
        dry_run,
        changed: existing.is_some(),
        config: None,
    })
}

/// One deploy key as the forge reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DeployKey {
    pub id: u64,
    pub title: String,
    /// OpenSSH public key text as stored by the forge.
    pub key: String,
    pub read_only: bool,
}

impl DeployKey {
    /// `algorithm base64` without any comment, for matching registrations.
    fn key_identity(&self) -> Option<String> {
        let mut fields = self.key.split_whitespace();
        Some(format!("{} {}", fields.next()?, fields.next()?))
    }

    /// The device a Vulcan-marked key was installed for.
    fn marker_device(&self) -> Option<String> {
        let rest = self.title.strip_prefix(DEVICE_KEY_TITLE_PREFIX)?;
        let id = rest.split_whitespace().next()?;
        let parsed = GitSyncDeviceId::parse(id).ok()?;
        (parsed.kind() == GitSyncDeviceIdKind::SshKeyV1).then(|| parsed.as_str().to_owned())
    }
}

/// Forge-specific I/O. Everything else is shared, so planning is testable
/// against a fake forge.
pub trait ForgeDeployKeyAdapter {
    fn list_deploy_keys(&self) -> Result<Vec<DeployKey>, AppError>;
    /// Adds a write-capable deploy key.
    fn add_deploy_key(&self, public_key: &str, title: &str) -> Result<DeployKey, AppError>;
    fn remove_deploy_key(&self, id: u64) -> Result<(), AppError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ForgeSyncAction {
    Add,
    /// A Vulcan-managed key for this device exists but is read-only; it is
    /// removed and re-added because the same key cannot be added twice.
    Replace,
    Remove,
    /// The device already has its key.
    Present,
    /// Revoked, and no key remains.
    AlreadyAbsent,
    /// A key with this public key exists but is not Vulcan-managed, so it is
    /// left alone.
    ForeignKeyKept,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ForgeSyncResult {
    Planned,
    Applied,
    Failed,
    Unchanged,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ForgeSyncEntry {
    pub device_id: String,
    pub fingerprint: String,
    pub status: RegistrationStatus,
    pub action: ForgeSyncAction,
    pub result: ForgeSyncResult,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip)]
    public_key: String,
    #[serde(skip)]
    title: String,
    #[serde(skip)]
    remove_key_ids: Vec<u64>,
}

/// A Vulcan-marked forge key that matches no registration. Never removed
/// automatically; revoke or unregister deliberately.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ForgeOrphanKey {
    pub key_id: u64,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ForgeSyncReport {
    pub version: u32,
    pub remote: GitRemote,
    pub repo: String,
    pub dry_run: bool,
    pub entries: Vec<ForgeSyncEntry>,
    pub orphans: Vec<ForgeOrphanKey>,
    /// Deploy keys without the Vulcan marker; never modified.
    pub foreign_keys: usize,
    /// Registration records that were malformed and ignored.
    pub ignored_registrations: usize,
    pub applied: usize,
    pub failed: usize,
}

/// Title for a key Vulcan installs.
fn device_title(registration: &RegistrationSummary) -> String {
    match &registration.label {
        Some(label) => format!(
            "{DEVICE_KEY_TITLE_PREFIX}{} {label}",
            registration.device_id
        ),
        None => format!("{DEVICE_KEY_TITLE_PREFIX}{}", registration.device_id),
    }
}

/// Pure planning over the registration list and the forge's current keys.
#[must_use]
pub fn plan_forge_sync(
    registrations: &[RegistrationSummary],
    keys: &[DeployKey],
) -> (Vec<ForgeSyncEntry>, Vec<ForgeOrphanKey>, usize) {
    let mut entries = Vec::new();
    for registration in registrations {
        let matching = keys
            .iter()
            .filter(|key| key.key_identity().as_deref() == Some(registration.public_key.as_str()))
            .collect::<Vec<_>>();
        let managed = matching
            .iter()
            .copied()
            .filter(|key| key.marker_device().is_some())
            .collect::<Vec<_>>();
        let (action, detail, remove_key_ids) = match registration.status {
            RegistrationStatus::Placeholder | RegistrationStatus::Registered => {
                if let Some(read_only) = managed.iter().find(|key| key.read_only) {
                    (
                        ForgeSyncAction::Replace,
                        Some("the installed key is read-only; sync needs write access".to_owned()),
                        vec![read_only.id],
                    )
                } else if !managed.is_empty() {
                    (ForgeSyncAction::Present, None, Vec::new())
                } else if !matching.is_empty() {
                    (
                        ForgeSyncAction::ForeignKeyKept,
                        Some(
                            "an existing deploy key without the Vulcan marker already has this key"
                                .to_owned(),
                        ),
                        Vec::new(),
                    )
                } else {
                    (ForgeSyncAction::Add, None, Vec::new())
                }
            }
            RegistrationStatus::Revoked => {
                if !managed.is_empty() {
                    (
                        ForgeSyncAction::Remove,
                        None,
                        managed.iter().map(|key| key.id).collect(),
                    )
                } else if !matching.is_empty() {
                    (
                        ForgeSyncAction::ForeignKeyKept,
                        Some("revoked, but the deploy key is not Vulcan-managed; remove it at the forge".to_owned()),
                        Vec::new(),
                    )
                } else {
                    (ForgeSyncAction::AlreadyAbsent, None, Vec::new())
                }
            }
        };
        entries.push(ForgeSyncEntry {
            device_id: registration.device_id.clone(),
            fingerprint: registration.fingerprint.clone(),
            status: registration.status,
            action,
            result: ForgeSyncResult::Planned,
            detail,
            public_key: registration.public_key.clone(),
            title: device_title(registration),
            remove_key_ids,
        });
    }
    let known = registrations
        .iter()
        .map(|registration| registration.public_key.as_str())
        .collect::<Vec<_>>();
    let mut orphans = Vec::new();
    let mut foreign = 0;
    for key in keys {
        if key.title.starts_with(DEVICE_KEY_TITLE_PREFIX) {
            let identity = key.key_identity();
            if !identity
                .as_deref()
                .is_some_and(|identity| known.contains(&identity))
            {
                orphans.push(ForgeOrphanKey {
                    key_id: key.id,
                    title: key.title.clone(),
                    device_id: key.marker_device(),
                    fingerprint: identity.and_then(|identity| {
                        identity_from_public_key(&identity, true)
                            .ok()
                            .map(|parsed| parsed.fingerprint)
                    }),
                });
            }
        } else {
            foreign += 1;
        }
    }
    (entries, orphans, foreign)
}

fn apply_entry(adapter: &dyn ForgeDeployKeyAdapter, entry: &mut ForgeSyncEntry) {
    let outcome = (|| {
        for id in &entry.remove_key_ids {
            adapter.remove_deploy_key(*id)?;
        }
        if matches!(
            entry.action,
            ForgeSyncAction::Add | ForgeSyncAction::Replace
        ) {
            adapter.add_deploy_key(&entry.public_key, &entry.title)?;
        }
        Ok::<(), AppError>(())
    })();
    match outcome {
        Ok(()) => entry.result = ForgeSyncResult::Applied,
        Err(error) => {
            entry.result = ForgeSyncResult::Failed;
            entry.detail = Some(if entry.action == ForgeSyncAction::Replace {
                format!("{error}; the old key may already be removed, so re-run to converge")
            } else {
                error.to_string()
            });
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthorizeAction {
    Added,
    /// A Vulcan-managed read-only key was replaced with a write-capable one.
    Replaced,
    AlreadyPresent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ForgeAuthorizeReport {
    pub version: u32,
    pub repo: String,
    pub device_id: String,
    pub fingerprint: String,
    pub action: AuthorizeAction,
    pub dry_run: bool,
}

/// Installs one device's key as a Vulcan-managed deploy key, without needing a
/// registration. This is how an administrator-capable user authorizes their own
/// device before it can reach the repository at all. Idempotent: a key with
/// this public key already present is left alone, and only a Vulcan-managed
/// read-only key is replaced.
pub fn authorize_device(
    adapter: &dyn ForgeDeployKeyAdapter,
    repo: &str,
    device_id: &str,
    public_key: &str,
    label: Option<&str>,
    dry_run: bool,
) -> Result<ForgeAuthorizeReport, AppError> {
    let identity = identity_from_public_key(public_key, false)?;
    if identity.device_id != device_id {
        return Err(AppError::operation(
            "the device ID does not match its public key; refusing to authorize it",
        ));
    }
    if let Some(label) = label {
        crate::sync_devices::validate_device_name(label)?;
    }
    let keys = adapter.list_deploy_keys()?;
    let matching = keys
        .iter()
        .filter(|key| key.key_identity().as_deref() == Some(identity.public_key.as_str()))
        .collect::<Vec<_>>();
    let title = match label {
        Some(label) => format!("{DEVICE_KEY_TITLE_PREFIX}{device_id} {label}"),
        None => format!("{DEVICE_KEY_TITLE_PREFIX}{device_id}"),
    };
    let read_only_managed = matching
        .iter()
        .find(|key| key.read_only && key.marker_device().is_some());
    let action = if let Some(key) = read_only_managed {
        if !dry_run {
            // The same public key cannot be added twice, so the old one goes first.
            adapter.remove_deploy_key(key.id)?;
            adapter.add_deploy_key(&identity.public_key, &title)?;
        }
        AuthorizeAction::Replaced
    } else if matching.is_empty() {
        if !dry_run {
            adapter.add_deploy_key(&identity.public_key, &title)?;
        }
        AuthorizeAction::Added
    } else {
        AuthorizeAction::AlreadyPresent
    };
    Ok(ForgeAuthorizeReport {
        version: SYNC_FORGE_REPORT_VERSION,
        repo: repo.to_owned(),
        device_id: device_id.to_owned(),
        fingerprint: identity.fingerprint,
        action,
        dry_run,
    })
}

/// Reconciles deploy keys with the remote's registrations.
///
/// Registrations must be observed from the remote: a failed or partial read
/// changes nothing. Removal comes only from an explicit `revoked` record,
/// never from absence, so an empty list cannot delete keys.
pub fn forge_sync(
    paths: &VaultPaths,
    remote: &GitRemote,
    adapter: &dyn ForgeDeployKeyAdapter,
    repo: &str,
    dry_run: bool,
) -> Result<ForgeSyncReport, AppError> {
    let registrations = list_registrations(paths, remote, true)?;
    if registrations.observation != RegistrationObservation::Observed {
        return Err(AppError::operation(
            "registrations could not be read from the remote, so no deploy keys were changed",
        ));
    }
    let keys = adapter.list_deploy_keys()?;
    let (mut entries, orphans, foreign_keys) = plan_forge_sync(&registrations.registrations, &keys);
    let mut applied = 0;
    let mut failed = 0;
    for entry in &mut entries {
        let mutating = matches!(
            entry.action,
            ForgeSyncAction::Add | ForgeSyncAction::Replace | ForgeSyncAction::Remove
        );
        if !mutating {
            entry.result = ForgeSyncResult::Unchanged;
        } else if !dry_run {
            apply_entry(adapter, entry);
            match entry.result {
                ForgeSyncResult::Applied => applied += 1,
                ForgeSyncResult::Failed => failed += 1,
                _ => {}
            }
        }
    }
    Ok(ForgeSyncReport {
        version: SYNC_FORGE_REPORT_VERSION,
        remote: remote.clone(),
        repo: repo.to_owned(),
        dry_run,
        entries,
        orphans,
        foreign_keys,
        ignored_registrations: registrations.rejected.len(),
        applied,
        failed,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ForgeRemoveReport {
    pub repo: String,
    pub device_id: String,
    /// Forge IDs of the Vulcan-marked keys for this device that were (or would be) removed.
    pub removed: Vec<u64>,
    pub dry_run: bool,
}

/// Removes only the Vulcan-marked deploy keys installed for `device_id`.
/// Unlike `forge_sync` this never adds a key and never looks at other
/// devices, so retiring one device cannot change anything else. Keys without
/// the Vulcan marker are never touched.
pub fn remove_device_keys(
    adapter: &dyn ForgeDeployKeyAdapter,
    repo: &str,
    device_id: &str,
    dry_run: bool,
) -> Result<ForgeRemoveReport, AppError> {
    let mut removed = Vec::new();
    for key in adapter.list_deploy_keys()? {
        if key.marker_device().as_deref() == Some(device_id) {
            if !dry_run {
                adapter.remove_deploy_key(key.id)?;
            }
            removed.push(key.id);
        }
    }
    Ok(ForgeRemoveReport {
        repo: repo.to_owned(),
        device_id: device_id.to_owned(),
        removed,
        dry_run,
    })
}

#[cfg(test)]
mod tests;
