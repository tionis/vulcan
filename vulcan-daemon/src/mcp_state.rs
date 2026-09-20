//! Durable device-local authorization state for named remote MCP instances.
//!
//! Raw bearer and refresh-token secrets are never persisted. The state file
//! contains only grants, revocation/audit metadata, and SHA-256 token hashes.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use subtle::ConstantTimeEq;
use tempfile::NamedTempFile;
use ulid::Ulid;
use vulcan_core::PermissionGrant;

use crate::mcp_remote::McpRemoteId;
use crate::registry::WikiId;

pub const MCP_AUTHORIZATION_STATE_VERSION: u32 = 1;
pub const MCP_CONNECTION_GRANT_VERSION: u32 = 1;
pub const MCP_TOKEN_FAMILY_VERSION: u32 = 1;
const STATE_FILE: &str = "mcp-authorizations.json";
const MAX_STATE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_GRANTS: usize = 4_096;
const MAX_TOKEN_FAMILIES: usize = 8_192;
const MAX_USED_REFRESH_TOKENS: usize = 64;
const TOKEN_BYTES: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionGrant {
    pub version: u32,
    pub id: Ulid,
    pub remote_id: McpRemoteId,
    pub remote_instance_id: Ulid,
    pub client_id: String,
    pub subject: String,
    pub wiki_id: WikiId,
    pub permission_profile: String,
    pub approved_permissions: PermissionGrant,
    pub tool_packs: Vec<String>,
    pub scopes: Vec<String>,
    pub audience: String,
    pub created_at: u64,
    pub expires_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<u64>,
}

impl ConnectionGrant {
    #[must_use]
    pub fn is_active_at(&self, now: u64) -> bool {
        self.revoked_at.is_none() && now < self.expires_at
    }

    #[must_use]
    pub fn report(&self) -> ConnectionGrantReport {
        ConnectionGrantReport {
            id: self.id,
            remote_id: self.remote_id.clone(),
            remote_instance_id: self.remote_instance_id,
            client_id: self.client_id.clone(),
            subject: self.subject.clone(),
            wiki_id: self.wiki_id.clone(),
            permission_profile: self.permission_profile.clone(),
            approved_permissions: self.approved_permissions.clone(),
            tool_packs: self.tool_packs.clone(),
            scopes: self.scopes.clone(),
            audience: self.audience.clone(),
            created_at: self.created_at,
            expires_at: self.expires_at,
            last_used_at: self.last_used_at,
            revoked_at: self.revoked_at,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateConnectionGrant {
    pub remote_id: McpRemoteId,
    pub remote_instance_id: Ulid,
    pub client_id: String,
    pub subject: String,
    pub wiki_id: WikiId,
    pub permission_profile: String,
    pub approved_permissions: PermissionGrant,
    pub tool_packs: Vec<String>,
    pub scopes: Vec<String>,
    pub audience: String,
    pub created_at: u64,
    pub expires_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConnectionGrantReport {
    pub id: Ulid,
    pub remote_id: McpRemoteId,
    pub remote_instance_id: Ulid,
    pub client_id: String,
    pub subject: String,
    pub wiki_id: WikiId,
    pub permission_profile: String,
    pub approved_permissions: PermissionGrant,
    pub tool_packs: Vec<String>,
    pub scopes: Vec<String>,
    pub audience: String,
    pub created_at: u64,
    pub expires_at: u64,
    pub last_used_at: Option<u64>,
    pub revoked_at: Option<u64>,
}

#[derive(Clone, PartialEq, Eq)]
pub struct RefreshTokenSecret(String);

impl RefreshTokenSecret {
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for RefreshTokenSecret {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("RefreshTokenSecret([REDACTED])")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssuedRefreshToken {
    pub family_id: Ulid,
    pub secret: RefreshTokenSecret,
    pub expires_at: u64,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenFamily {
    pub version: u32,
    pub id: Ulid,
    pub grant_id: Ulid,
    pub client_id: String,
    pub audience: String,
    pub created_at: u64,
    pub expires_at: u64,
    pub rotation_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<u64>,
    current_refresh_token_hash: String,
    #[serde(default)]
    used_refresh_token_hashes: Vec<String>,
}

impl std::fmt::Debug for TokenFamily {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TokenFamily")
            .field("version", &self.version)
            .field("id", &self.id)
            .field("grant_id", &self.grant_id)
            .field("client_id", &self.client_id)
            .field("audience", &self.audience)
            .field("created_at", &self.created_at)
            .field("expires_at", &self.expires_at)
            .field("rotation_count", &self.rotation_count)
            .field("last_used_at", &self.last_used_at)
            .field("revoked_at", &self.revoked_at)
            .field("current_refresh_token_hash", &"[REDACTED]")
            .field("used_refresh_token_hashes", &"[REDACTED]")
            .finish()
    }
}

impl TokenFamily {
    #[must_use]
    pub fn report(&self) -> TokenFamilyReport {
        TokenFamilyReport {
            id: self.id,
            grant_id: self.grant_id,
            client_id: self.client_id.clone(),
            audience: self.audience.clone(),
            created_at: self.created_at,
            expires_at: self.expires_at,
            rotation_count: self.rotation_count,
            last_used_at: self.last_used_at,
            revoked_at: self.revoked_at,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TokenFamilyReport {
    pub id: Ulid,
    pub grant_id: Ulid,
    pub client_id: String,
    pub audience: String,
    pub created_at: u64,
    pub expires_at: u64,
    pub rotation_count: u32,
    pub last_used_at: Option<u64>,
    pub revoked_at: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct AuthorizationState {
    version: u32,
    #[serde(default)]
    grants: Vec<ConnectionGrant>,
    #[serde(default)]
    token_families: Vec<TokenFamily>,
}

impl Default for AuthorizationState {
    fn default() -> Self {
        Self {
            version: MCP_AUTHORIZATION_STATE_VERSION,
            grants: Vec::new(),
            token_families: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct McpAuthorizationStore {
    path: PathBuf,
}

impl McpAuthorizationStore {
    #[must_use]
    pub fn at(state_root: impl AsRef<Path>) -> Self {
        Self {
            path: state_root.as_ref().join("daemon").join(STATE_FILE),
        }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn create_grant(
        &self,
        request: CreateConnectionGrant,
        dry_run: bool,
    ) -> Result<ConnectionGrantReport, McpStateError> {
        validate_create_grant(&request)?;
        self.mutate(dry_run, |state| {
            let grant = ConnectionGrant {
                version: MCP_CONNECTION_GRANT_VERSION,
                id: Ulid::new(),
                remote_id: request.remote_id,
                remote_instance_id: request.remote_instance_id,
                client_id: request.client_id,
                subject: request.subject,
                wiki_id: request.wiki_id,
                permission_profile: request.permission_profile,
                approved_permissions: request.approved_permissions,
                tool_packs: sorted_unique(request.tool_packs),
                scopes: sorted_unique(request.scopes),
                audience: request.audience,
                created_at: request.created_at,
                expires_at: request.expires_at,
                last_used_at: None,
                revoked_at: None,
            };
            state.grants.push(grant.clone());
            state.grants.sort_by_key(|item| item.id);
            Ok(grant.report())
        })
    }

    pub fn list_grants(
        &self,
        remote: Option<&McpRemoteId>,
    ) -> Result<Vec<ConnectionGrantReport>, McpStateError> {
        let state = self.load()?;
        Ok(state
            .grants
            .iter()
            .filter(|grant| remote.is_none_or(|remote| &grant.remote_id == remote))
            .map(ConnectionGrant::report)
            .collect())
    }

    pub fn show_grant(&self, id: Ulid) -> Result<ConnectionGrantReport, McpStateError> {
        self.load()?
            .grants
            .iter()
            .find(|grant| grant.id == id)
            .map(ConnectionGrant::report)
            .ok_or(McpStateError::UnknownGrant(id))
    }

    pub fn resolve_active_grant(
        &self,
        id: Ulid,
        remote_instance_id: Ulid,
        client_id: &str,
        audience: &str,
        now: u64,
    ) -> Result<ConnectionGrant, McpStateError> {
        let state = self.load()?;
        let grant = state
            .grants
            .into_iter()
            .find(|grant| grant.id == id)
            .ok_or(McpStateError::UnknownGrant(id))?;
        if !grant.is_active_at(now) {
            return Err(McpStateError::InactiveGrant(id));
        }
        if grant.remote_instance_id != remote_instance_id
            || grant.client_id != client_id
            || grant.audience != audience
        {
            return Err(McpStateError::GrantBindingMismatch(id));
        }
        Ok(grant)
    }

    pub fn revoke_grant(
        &self,
        id: Ulid,
        revoked_at: u64,
        dry_run: bool,
    ) -> Result<ConnectionGrantReport, McpStateError> {
        self.mutate(dry_run, |state| {
            let grant = state
                .grants
                .iter_mut()
                .find(|grant| grant.id == id)
                .ok_or(McpStateError::UnknownGrant(id))?;
            grant.revoked_at.get_or_insert(revoked_at);
            let report = grant.report();
            for family in state
                .token_families
                .iter_mut()
                .filter(|family| family.grant_id == id)
            {
                family.revoked_at.get_or_insert(revoked_at);
            }
            Ok(report)
        })
    }

    pub fn issue_refresh_token(
        &self,
        grant_id: Ulid,
        expires_at: u64,
        now: u64,
    ) -> Result<IssuedRefreshToken, McpStateError> {
        let secret = generate_secret()?;
        let hash = token_hash(secret.expose());
        let issued = self.mutate(false, |state| {
            let grant = state
                .grants
                .iter()
                .find(|grant| grant.id == grant_id)
                .ok_or(McpStateError::UnknownGrant(grant_id))?;
            if !grant.is_active_at(now) {
                return Err(McpStateError::InactiveGrant(grant_id));
            }
            if expires_at <= now || expires_at > grant.expires_at {
                return Err(McpStateError::Invalid(
                    "refresh-token expiry must be in the future and no later than its grant"
                        .to_string(),
                ));
            }
            let family = TokenFamily {
                version: MCP_TOKEN_FAMILY_VERSION,
                id: Ulid::new(),
                grant_id,
                client_id: grant.client_id.clone(),
                audience: grant.audience.clone(),
                created_at: now,
                expires_at,
                rotation_count: 0,
                last_used_at: None,
                revoked_at: None,
                current_refresh_token_hash: hash,
                used_refresh_token_hashes: Vec::new(),
            };
            let family_id = family.id;
            state.token_families.push(family);
            state.token_families.sort_by_key(|item| item.id);
            Ok(family_id)
        })?;
        Ok(IssuedRefreshToken {
            family_id: issued,
            secret,
            expires_at,
        })
    }

    pub fn rotate_refresh_token(
        &self,
        family_id: Ulid,
        candidate: &str,
        now: u64,
    ) -> Result<IssuedRefreshToken, McpStateError> {
        let replacement = generate_secret()?;
        let replacement_hash = token_hash(replacement.expose());
        let candidate_hash = token_hash(candidate);
        let outcome = self.mutate(false, |state| {
            let family = state
                .token_families
                .iter_mut()
                .find(|family| family.id == family_id)
                .ok_or(McpStateError::UnknownTokenFamily(family_id))?;
            if family.revoked_at.is_some() || now >= family.expires_at {
                return Err(McpStateError::InactiveTokenFamily(family_id));
            }
            if family
                .used_refresh_token_hashes
                .iter()
                .any(|hash| hashes_equal(hash, &candidate_hash))
            {
                family.revoked_at = Some(now);
                return Ok(RotationOutcome::Replay);
            }
            if !hashes_equal(&family.current_refresh_token_hash, &candidate_hash) {
                return Err(McpStateError::InvalidRefreshToken);
            }
            family.used_refresh_token_hashes.push(std::mem::replace(
                &mut family.current_refresh_token_hash,
                replacement_hash,
            ));
            if family.used_refresh_token_hashes.len() > MAX_USED_REFRESH_TOKENS {
                family.used_refresh_token_hashes.remove(0);
            }
            family.rotation_count = family.rotation_count.saturating_add(1);
            family.last_used_at = Some(now);
            if let Some(grant) = state
                .grants
                .iter_mut()
                .find(|grant| grant.id == family.grant_id)
            {
                grant.last_used_at = Some(now);
            }
            Ok(RotationOutcome::Rotated(family.expires_at))
        })?;
        match outcome {
            RotationOutcome::Rotated(expires_at) => Ok(IssuedRefreshToken {
                family_id,
                secret: replacement,
                expires_at,
            }),
            RotationOutcome::Replay => Err(McpStateError::RefreshTokenReplay(family_id)),
        }
    }

    pub fn list_token_families(
        &self,
        grant_id: Option<Ulid>,
    ) -> Result<Vec<TokenFamilyReport>, McpStateError> {
        Ok(self
            .load()?
            .token_families
            .iter()
            .filter(|family| grant_id.is_none_or(|grant_id| family.grant_id == grant_id))
            .map(TokenFamily::report)
            .collect())
    }

    fn load(&self) -> Result<AuthorizationState, McpStateError> {
        load_state(&self.path)
    }

    fn mutate<T>(
        &self,
        dry_run: bool,
        operation: impl FnOnce(&mut AuthorizationState) -> Result<T, McpStateError>,
    ) -> Result<T, McpStateError> {
        let _lock = StateLock::acquire(&self.path)?;
        let mut state = self.load()?;
        let result = operation(&mut state)?;
        validate_state(&state)?;
        if !dry_run {
            save_state(&self.path, &state)?;
        }
        Ok(result)
    }
}

enum RotationOutcome {
    Rotated(u64),
    Replay,
}

fn validate_create_grant(request: &CreateConnectionGrant) -> Result<(), McpStateError> {
    validate_text(&request.client_id, "OAuth client ID", 2_048)?;
    validate_https_url(&request.subject, "subject")?;
    validate_text(&request.permission_profile, "permission profile", 128)?;
    validate_https_url(&request.audience, "audience")?;
    validate_string_set(&request.tool_packs, "tool packs", 32, 128)?;
    validate_string_set(&request.scopes, "scopes", 32, 128)?;
    if request.expires_at <= request.created_at {
        return Err(McpStateError::Invalid(
            "connection grant expiry must follow its creation time".to_string(),
        ));
    }
    Ok(())
}

fn validate_state(state: &AuthorizationState) -> Result<(), McpStateError> {
    if state.version != MCP_AUTHORIZATION_STATE_VERSION {
        return Err(McpStateError::UnsupportedVersion(state.version));
    }
    if state.grants.len() > MAX_GRANTS || state.token_families.len() > MAX_TOKEN_FAMILIES {
        return Err(McpStateError::Invalid(
            "remote MCP authorization state exceeds configured entry limits".to_string(),
        ));
    }
    let mut grant_ids = BTreeSet::new();
    for grant in &state.grants {
        if grant.version != MCP_CONNECTION_GRANT_VERSION || !grant_ids.insert(grant.id) {
            return Err(McpStateError::Invalid(
                "connection grants contain an unsupported version or duplicate ID".to_string(),
            ));
        }
        validate_create_grant(&CreateConnectionGrant {
            remote_id: grant.remote_id.clone(),
            remote_instance_id: grant.remote_instance_id,
            client_id: grant.client_id.clone(),
            subject: grant.subject.clone(),
            wiki_id: grant.wiki_id.clone(),
            permission_profile: grant.permission_profile.clone(),
            approved_permissions: grant.approved_permissions.clone(),
            tool_packs: grant.tool_packs.clone(),
            scopes: grant.scopes.clone(),
            audience: grant.audience.clone(),
            created_at: grant.created_at,
            expires_at: grant.expires_at,
        })?;
    }
    let mut family_ids = BTreeSet::new();
    for family in &state.token_families {
        if family.version != MCP_TOKEN_FAMILY_VERSION
            || !family_ids.insert(family.id)
            || !grant_ids.contains(&family.grant_id)
            || family.expires_at <= family.created_at
            || family.current_refresh_token_hash.len() != 43
            || family.used_refresh_token_hashes.len() > MAX_USED_REFRESH_TOKENS
            || family
                .used_refresh_token_hashes
                .iter()
                .any(|hash| hash.len() != 43)
        {
            return Err(McpStateError::Invalid(
                "token families contain invalid versions, bindings, timestamps, or hashes"
                    .to_string(),
            ));
        }
    }
    Ok(())
}

fn load_state(path: &Path) -> Result<AuthorizationState, McpStateError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(AuthorizationState::default());
        }
        Err(error) => return Err(McpStateError::Io(error)),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(McpStateError::Invalid(format!(
            "remote MCP authorization state at {} is not a regular file",
            path.display()
        )));
    }
    if metadata.len() > MAX_STATE_BYTES {
        return Err(McpStateError::Invalid(format!(
            "remote MCP authorization state exceeds {MAX_STATE_BYTES} bytes"
        )));
    }
    validate_owner_only(&metadata, path)?;
    let value: serde_json::Value = serde_json::from_slice(&fs::read(path)?)?;
    let state = migrate_state(value)?;
    validate_state(&state)?;
    Ok(state)
}

fn migrate_state(value: serde_json::Value) -> Result<AuthorizationState, McpStateError> {
    let version = value
        .get("version")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| McpStateError::Invalid("authorization state has no version".to_string()))?;
    match version {
        1 => serde_json::from_value(value).map_err(McpStateError::Json),
        version => Err(McpStateError::UnsupportedVersion(
            u32::try_from(version).unwrap_or(u32::MAX),
        )),
    }
}

fn save_state(path: &Path, state: &AuthorizationState) -> Result<(), McpStateError> {
    let parent = path
        .parent()
        .ok_or_else(|| McpStateError::Invalid("authorization state path has no parent".into()))?;
    fs::create_dir_all(parent)?;
    let mut temporary = NamedTempFile::new_in(parent)?;
    temporary.write_all(&serde_json::to_vec_pretty(state)?)?;
    temporary.write_all(b"\n")?;
    temporary.as_file().sync_all()?;
    set_owner_only(temporary.path())?;
    temporary
        .persist(path)
        .map_err(|error| McpStateError::Io(error.error))?;
    Ok(())
}

struct StateLock {
    _file: File,
}

impl StateLock {
    fn acquire(state_path: &Path) -> Result<Self, McpStateError> {
        let parent = state_path.parent().ok_or_else(|| {
            McpStateError::Invalid("authorization state path has no parent".into())
        })?;
        fs::create_dir_all(parent)?;
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(parent.join("mcp-authorizations.lock"))?;
        file.lock_exclusive()?;
        Ok(Self { _file: file })
    }
}

fn generate_secret() -> Result<RefreshTokenSecret, McpStateError> {
    let mut bytes = [0_u8; TOKEN_BYTES];
    getrandom::fill(&mut bytes).map_err(|error| McpStateError::Random(error.to_string()))?;
    Ok(RefreshTokenSecret(URL_SAFE_NO_PAD.encode(bytes)))
}

fn token_hash(token: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(token.as_bytes()))
}

fn hashes_equal(left: &str, right: &str) -> bool {
    left.as_bytes().ct_eq(right.as_bytes()).into()
}

fn sorted_unique(mut values: Vec<String>) -> Vec<String> {
    values.sort();
    values.dedup();
    values
}

fn validate_text(value: &str, label: &str, maximum: usize) -> Result<(), McpStateError> {
    if value.is_empty()
        || value.len() > maximum
        || value
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
    {
        return Err(McpStateError::Invalid(format!(
            "remote MCP {label} must contain 1-{maximum} non-whitespace bytes"
        )));
    }
    Ok(())
}

fn validate_https_url(value: &str, label: &str) -> Result<(), McpStateError> {
    let url = reqwest::Url::parse(value)
        .map_err(|error| McpStateError::Invalid(format!("invalid {label}: {error}")))?;
    if value.len() > 2_048
        || url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(McpStateError::Invalid(format!(
            "remote MCP {label} must be a bounded HTTPS URL without credentials or fragment"
        )));
    }
    Ok(())
}

fn validate_string_set(
    values: &[String],
    label: &str,
    maximum_entries: usize,
    maximum_bytes: usize,
) -> Result<(), McpStateError> {
    if values.is_empty() || values.len() > maximum_entries {
        return Err(McpStateError::Invalid(format!(
            "remote MCP {label} must contain 1-{maximum_entries} entries"
        )));
    }
    let mut unique = BTreeSet::new();
    for value in values {
        validate_text(value, label, maximum_bytes)?;
        if !unique.insert(value) {
            return Err(McpStateError::Invalid(format!(
                "remote MCP {label} contain duplicate `{value}`"
            )));
        }
    }
    Ok(())
}

#[cfg(unix)]
fn set_owner_only(path: &Path) -> Result<(), McpStateError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(unix)]
fn validate_owner_only(metadata: &fs::Metadata, path: &Path) -> Result<(), McpStateError> {
    use std::os::unix::fs::PermissionsExt;
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(McpStateError::Invalid(format!(
            "remote MCP authorization state at {} is accessible by group or other users",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(not(unix))]
#[allow(clippy::permissions_set_readonly_false)]
fn set_owner_only(path: &Path) -> Result<(), McpStateError> {
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_readonly(false);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

#[cfg(not(unix))]
#[allow(clippy::unnecessary_wraps)]
fn validate_owner_only(_metadata: &fs::Metadata, _path: &Path) -> Result<(), McpStateError> {
    Ok(())
}

#[derive(Debug)]
pub enum McpStateError {
    UnknownGrant(Ulid),
    InactiveGrant(Ulid),
    GrantBindingMismatch(Ulid),
    UnknownTokenFamily(Ulid),
    InactiveTokenFamily(Ulid),
    InvalidRefreshToken,
    RefreshTokenReplay(Ulid),
    UnsupportedVersion(u32),
    Invalid(String),
    Random(String),
    Io(std::io::Error),
    Json(serde_json::Error),
}

impl Display for McpStateError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownGrant(id) => write!(formatter, "unknown connection grant `{id}`"),
            Self::InactiveGrant(id) => write!(formatter, "connection grant `{id}` is inactive"),
            Self::GrantBindingMismatch(id) => {
                write!(
                    formatter,
                    "connection grant `{id}` does not match this authority"
                )
            }
            Self::UnknownTokenFamily(id) => write!(formatter, "unknown token family `{id}`"),
            Self::InactiveTokenFamily(id) => write!(formatter, "token family `{id}` is inactive"),
            Self::InvalidRefreshToken => formatter.write_str("invalid refresh token"),
            Self::RefreshTokenReplay(id) => {
                write!(formatter, "refresh-token replay revoked family `{id}`")
            }
            Self::UnsupportedVersion(version) => {
                write!(
                    formatter,
                    "unsupported authorization-state version {version}"
                )
            }
            Self::Invalid(detail) | Self::Random(detail) => formatter.write_str(detail),
            Self::Io(error) => Display::fmt(error, formatter),
            Self::Json(error) => Display::fmt(error, formatter),
        }
    }
}

impl Error for McpStateError {}

impl From<std::io::Error> for McpStateError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for McpStateError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use vulcan_core::{PathPermission, ResourceLimits, ResourceSpecifier};

    fn permission_grant() -> PermissionGrant {
        PermissionGrant {
            read: PathPermission {
                allow: vec![ResourceSpecifier::All],
                deny: Vec::new(),
            },
            write: PathPermission::default(),
            refactor: PathPermission::default(),
            git: false,
            network: false,
            network_domains: Vec::new(),
            index: false,
            config_read: false,
            config_write: false,
            execute: false,
            shell: false,
            limits: ResourceLimits::default(),
        }
    }

    fn grant_request(remote: &str, client: &str, subject: &str) -> CreateConnectionGrant {
        CreateConnectionGrant {
            remote_id: McpRemoteId::parse(remote).expect("remote"),
            remote_instance_id: Ulid::new(),
            client_id: client.to_string(),
            subject: subject.to_string(),
            wiki_id: WikiId::parse("personal").expect("wiki"),
            permission_profile: "readonly".to_string(),
            approved_permissions: permission_grant(),
            tool_packs: vec!["status".to_string(), "notes-read".to_string()],
            scopes: vec!["mcp:tools".to_string()],
            audience: format!("https://mcp.example.test/{remote}"),
            created_at: 1_000,
            expires_at: 10_000,
        }
    }

    #[test]
    fn grants_are_durable_redacted_and_binding_scoped() {
        let temporary = tempdir().expect("temporary");
        let store = McpAuthorizationStore::at(temporary.path());
        let request = grant_request(
            "personal-chatgpt",
            "https://client.example.test/metadata.json",
            "https://identity.example.test/alice",
        );
        let instance_id = request.remote_instance_id;
        let report = store.create_grant(request, false).expect("grant");
        assert_eq!(report.tool_packs, ["notes-read", "status"]);
        assert!(store
            .resolve_active_grant(
                report.id,
                instance_id,
                &report.client_id,
                &report.audience,
                2_000,
            )
            .is_ok());
        assert!(matches!(
            store.resolve_active_grant(
                report.id,
                Ulid::new(),
                &report.client_id,
                &report.audience,
                2_000,
            ),
            Err(McpStateError::GrantBindingMismatch(_))
        ));

        let json = fs::read_to_string(store.path()).expect("state file");
        assert!(!json.contains("refresh_token"));
        let reloaded = McpAuthorizationStore::at(temporary.path());
        assert_eq!(reloaded.show_grant(report.id).expect("reloaded"), report);
    }

    #[test]
    fn dry_run_and_revocation_are_mutation_safe() {
        let temporary = tempdir().expect("temporary");
        let store = McpAuthorizationStore::at(temporary.path());
        let planned = store
            .create_grant(
                grant_request(
                    "personal-chatgpt",
                    "client",
                    "https://identity.example.test/alice",
                ),
                true,
            )
            .expect("planned grant");
        assert!(!store.path().exists());
        assert!(matches!(
            store.show_grant(planned.id),
            Err(McpStateError::UnknownGrant(_))
        ));

        let grant = store
            .create_grant(
                grant_request(
                    "personal-chatgpt",
                    "client",
                    "https://identity.example.test/alice",
                ),
                false,
            )
            .expect("grant");
        store.revoke_grant(grant.id, 2_000, false).expect("revoke");
        assert!(matches!(
            store.resolve_active_grant(
                grant.id,
                grant.remote_instance_id,
                &grant.client_id,
                &grant.audience,
                2_001,
            ),
            Err(McpStateError::InactiveGrant(_))
        ));
    }

    #[test]
    fn refresh_tokens_rotate_and_replay_revokes_the_family() {
        let temporary = tempdir().expect("temporary");
        let store = McpAuthorizationStore::at(temporary.path());
        let grant = store
            .create_grant(
                grant_request(
                    "personal-chatgpt",
                    "client",
                    "https://identity.example.test/alice",
                ),
                false,
            )
            .expect("grant");
        let first = store
            .issue_refresh_token(grant.id, 9_000, 1_001)
            .expect("first token");
        let original = first.secret.expose().to_string();
        let second = store
            .rotate_refresh_token(first.family_id, &original, 2_000)
            .expect("rotate");
        assert_ne!(original, second.secret.expose());
        assert!(matches!(
            store.rotate_refresh_token(first.family_id, &original, 2_001),
            Err(McpStateError::RefreshTokenReplay(_))
        ));
        assert!(matches!(
            store.rotate_refresh_token(first.family_id, second.secret.expose(), 2_002),
            Err(McpStateError::InactiveTokenFamily(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn state_file_requires_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let temporary = tempdir().expect("temporary");
        let store = McpAuthorizationStore::at(temporary.path());
        store
            .create_grant(
                grant_request(
                    "personal-chatgpt",
                    "client",
                    "https://identity.example.test/alice",
                ),
                false,
            )
            .expect("grant");
        assert_eq!(
            fs::metadata(store.path())
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        fs::set_permissions(store.path(), fs::Permissions::from_mode(0o644))
            .expect("loosen permissions");
        assert!(matches!(store.load(), Err(McpStateError::Invalid(_))));
    }
}
