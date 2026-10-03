//! Durable device-local authorization state for named remote MCP instances.
//!
//! Raw bearer and refresh-token secrets are never persisted. The state store
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
use std::path::{Path, PathBuf};
use subtle::ConstantTimeEq;
use ulid::Ulid;
use vulcan_app::keyed_state::{KeyedStateStore, TrackedMap};
use vulcan_core::PermissionGrant;

use crate::mcp_remote::McpRemoteId;
use crate::registry::WikiId;

pub const MCP_AUTHORIZATION_STATE_VERSION: u32 = 1;
pub const MCP_CONNECTION_GRANT_VERSION: u32 = 1;
pub const MCP_TOKEN_FAMILY_VERSION: u32 = 1;
const STATE_FILE: &str = "mcp-authorizations.sqlite";
const LEGACY_STATE_FILE: &str = "mcp-authorizations.json";
const MAX_LEGACY_STATE_BYTES: u64 = 8 * 1024 * 1024;
const GRANTS: &str = "grants";
const TOKEN_FAMILIES: &str = "token-families";
/// How long an expired or revoked grant or token family stays listed before
/// it is dropped.
pub const INACTIVE_RETENTION_SECONDS: u64 = 30 * 24 * 60 * 60;
const GRANT_USE_RESOLUTION_SECONDS: u64 = 60;
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

/// Grants and token families held in memory by management operations, which
/// are rare and may scan everything. Per-request paths read single rows.
#[derive(Debug, Default)]
struct AuthorizationState {
    grants: TrackedMap<ConnectionGrant>,
    token_families: TrackedMap<TokenFamily>,
}

/// The JSON layout used before the `SQLite` store.
#[derive(Deserialize)]
struct LegacyAuthorizationState {
    #[serde(default)]
    grants: Vec<ConnectionGrant>,
    #[serde(default)]
    token_families: Vec<TokenFamily>,
}

/// Device-local remote MCP authorization state.
///
/// Grants and token families are rows of one owner-only `SQLite` store, so an
/// authenticated request reads only its own grant instead of parsing every
/// grant ever issued. Writers serialize on a lock file; readers never block.
/// Grants and token families that ended more than [`INACTIVE_RETENTION_SECONDS`]
/// ago are dropped whenever a new grant or token family is recorded.
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
            prune_inactive(state, request.created_at);
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
            let report = grant.report();
            state.grants.insert(grant.id.to_string(), grant);
            Ok(report)
        })
    }

    pub fn list_grants(
        &self,
        remote: Option<&McpRemoteId>,
    ) -> Result<Vec<ConnectionGrantReport>, McpStateError> {
        let state = self.load()?;
        Ok(state
            .grants
            .values()
            .filter(|grant| remote.is_none_or(|remote| &grant.remote_id == remote))
            .map(ConnectionGrant::report)
            .collect())
    }

    pub fn show_grant(&self, id: Ulid) -> Result<ConnectionGrantReport, McpStateError> {
        self.grant(id)?
            .as_ref()
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
        let grant = self.grant(id)?.ok_or(McpStateError::UnknownGrant(id))?;
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
                .get_mut(&id.to_string())
                .ok_or(McpStateError::UnknownGrant(id))?;
            grant.revoked_at.get_or_insert(revoked_at);
            let report = grant.report();
            revoke_families(state, &BTreeSet::from([id]), revoked_at);
            Ok(report)
        })
    }

    pub fn mark_grant_used(
        &self,
        id: Ulid,
        now: u64,
    ) -> Result<ConnectionGrantReport, McpStateError> {
        self.update_active_grant(id, now, |grant| Ok(touch_grant(grant, now)))
    }

    /// Persist a narrower effective permission boundary for an existing grant.
    /// A later profile edit cannot restore authority without a new consent grant.
    pub fn attenuate_grant_permissions(
        &self,
        id: Ulid,
        current_permissions: &PermissionGrant,
        now: u64,
    ) -> Result<ConnectionGrantReport, McpStateError> {
        self.update_active_grant(id, now, |grant| attenuate_grant(grant, current_permissions))
    }

    /// Records one authenticated use of a grant: attenuates it to the current
    /// permissions and refreshes its last use, writing only when either changed.
    pub fn record_grant_use(
        &self,
        id: Ulid,
        current_permissions: &PermissionGrant,
        now: u64,
    ) -> Result<ConnectionGrantReport, McpStateError> {
        self.update_active_grant(id, now, |grant| {
            let attenuated = attenuate_grant(grant, current_permissions)?;
            Ok(touch_grant(grant, now) | attenuated)
        })
    }

    pub fn revoke_remote_grants(
        &self,
        remote: &McpRemoteId,
        revoked_at: u64,
        dry_run: bool,
    ) -> Result<Vec<ConnectionGrantReport>, McpStateError> {
        self.revoke_matching_grants(dry_run, revoked_at, |grant| &grant.remote_id == remote)
    }

    pub fn revoke_remote_wiki_grants(
        &self,
        remote: &McpRemoteId,
        wiki: &WikiId,
        revoked_at: u64,
        dry_run: bool,
    ) -> Result<Vec<ConnectionGrantReport>, McpStateError> {
        self.revoke_matching_grants(dry_run, revoked_at, |grant| {
            &grant.remote_id == remote && &grant.wiki_id == wiki
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
            prune_inactive(state, now);
            let grant = state
                .grants
                .get(&grant_id.to_string())
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
            state.token_families.insert(family_id.to_string(), family);
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
        let _lock = StateLock::acquire(&self.path)?;
        let mut store = self.open_writable()?;
        let mut family =
            read_family(&store, family_id)?.ok_or(McpStateError::UnknownTokenFamily(family_id))?;
        if family.revoked_at.is_some() || now >= family.expires_at {
            return Err(McpStateError::InactiveTokenFamily(family_id));
        }
        if family
            .used_refresh_token_hashes
            .iter()
            .any(|hash| hashes_equal(hash, &candidate_hash))
        {
            family.revoked_at = Some(now);
            store.write(|writer| writer.put(TOKEN_FAMILIES, &family_id.to_string(), &family))?;
            return Err(McpStateError::RefreshTokenReplay(family_id));
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
        let mut grant = read_grant(&store, family.grant_id)?;
        if let Some(grant) = &mut grant {
            grant.last_used_at = Some(now);
        }
        validate_family(&family, grant.as_ref())?;
        store.write(|writer| {
            writer.put(TOKEN_FAMILIES, &family_id.to_string(), &family)?;
            if let Some(grant) = &grant {
                writer.put(GRANTS, &grant.id.to_string(), grant)?;
            }
            Ok(())
        })?;
        Ok(IssuedRefreshToken {
            family_id,
            secret: replacement,
            expires_at: family.expires_at,
        })
    }

    pub fn list_token_families(
        &self,
        grant_id: Option<Ulid>,
    ) -> Result<Vec<TokenFamilyReport>, McpStateError> {
        Ok(self
            .load()?
            .token_families
            .values()
            .filter(|family| grant_id.is_none_or(|grant_id| family.grant_id == grant_id))
            .map(TokenFamily::report)
            .collect())
    }

    fn revoke_matching_grants(
        &self,
        dry_run: bool,
        revoked_at: u64,
        matches: impl Fn(&ConnectionGrant) -> bool,
    ) -> Result<Vec<ConnectionGrantReport>, McpStateError> {
        self.mutate(dry_run, |state| {
            let ids = state
                .grants
                .values()
                .filter(|grant| matches(grant))
                .map(|grant| grant.id)
                .collect::<BTreeSet<_>>();
            let mut reports = Vec::with_capacity(ids.len());
            for id in &ids {
                let grant = state
                    .grants
                    .get_mut(&id.to_string())
                    .expect("matched grant is present");
                grant.revoked_at.get_or_insert(revoked_at);
                reports.push(grant.report());
            }
            revoke_families(state, &ids, revoked_at);
            Ok(reports)
        })
    }

    /// Applies `apply` to one active grant, writing it only when `apply`
    /// reports a change. The common unchanged case takes no lock and writes
    /// nothing; a change is re-applied to a fresh read under the writer lock.
    fn update_active_grant(
        &self,
        id: Ulid,
        now: u64,
        apply: impl Fn(&mut ConnectionGrant) -> Result<bool, McpStateError>,
    ) -> Result<ConnectionGrantReport, McpStateError> {
        let mut grant = active_grant(self.grant(id)?, id, now)?;
        if !apply(&mut grant)? {
            return Ok(grant.report());
        }
        let _lock = StateLock::acquire(&self.path)?;
        let mut store = self.open_writable()?;
        let mut grant = active_grant(read_grant(&store, id)?, id, now)?;
        if apply(&mut grant)? {
            validate_grant(&grant)?;
            store.write(|writer| writer.put(GRANTS, &id.to_string(), &grant))?;
        }
        Ok(grant.report())
    }

    /// One grant, read without loading any other.
    fn grant(&self, id: Ulid) -> Result<Option<ConnectionGrant>, McpStateError> {
        match self.open_readable()? {
            Some(store) => read_grant(&store, id),
            None => Ok(self.load()?.grants.get(&id.to_string()).cloned()),
        }
    }

    /// Everything, for management operations.
    fn load(&self) -> Result<AuthorizationState, McpStateError> {
        let state = match self.open_readable()? {
            Some(store) => AuthorizationState {
                grants: store.load_map(GRANTS)?,
                token_families: store.load_map(TOKEN_FAMILIES)?,
            },
            None => load_legacy_state(&self.legacy_path())?.unwrap_or_default(),
        };
        validate_state(&state)?;
        Ok(state)
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
            let mut store = self.open_writable()?;
            store.write(|writer| {
                writer.write_dirty(GRANTS, &state.grants, |_| Vec::new(), "duplicate grant")?;
                writer.write_dirty(
                    TOKEN_FAMILIES,
                    &state.token_families,
                    |_| Vec::new(),
                    "duplicate token family",
                )
            })?;
        }
        Ok(result)
    }

    /// The initialized store, or `None` before the first write. Never creates
    /// or migrates anything, so reads and dry runs leave no files behind.
    fn open_readable(&self) -> Result<Option<KeyedStateStore>, McpStateError> {
        if !check_private_file(&self.path)? {
            return Ok(None);
        }
        Ok(KeyedStateStore::open_read_only(&self.path)?)
    }

    /// Opens the store for writing, importing the JSON state of earlier
    /// versions once and keeping it as `mcp-authorizations.json.migrated`.
    /// The caller holds the writer lock.
    fn open_writable(&self) -> Result<KeyedStateStore, McpStateError> {
        check_private_file(&self.path)?;
        let mut store = KeyedStateStore::open(&self.path)?;
        if store.is_initialized()? {
            return Ok(store);
        }
        let legacy_path = self.legacy_path();
        let legacy = load_legacy_state(&legacy_path)?;
        store.write(|writer| {
            if let Some(state) = &legacy {
                writer.write_dirty(GRANTS, &state.grants, |_| Vec::new(), "duplicate grant")?;
                writer.write_dirty(
                    TOKEN_FAMILIES,
                    &state.token_families,
                    |_| Vec::new(),
                    "duplicate token family",
                )?;
            }
            writer.mark_initialized()
        })?;
        if legacy.is_some() {
            fs::rename(&legacy_path, legacy_path.with_extension("json.migrated"))?;
        }
        Ok(store)
    }

    fn legacy_path(&self) -> PathBuf {
        self.path.with_file_name(LEGACY_STATE_FILE)
    }
}

fn read_grant(store: &KeyedStateStore, id: Ulid) -> Result<Option<ConnectionGrant>, McpStateError> {
    let grant = store.entry::<ConnectionGrant>(GRANTS, &id.to_string())?;
    if let Some(grant) = &grant {
        if grant.id != id {
            return Err(McpStateError::Invalid(
                "connection grant is stored under another ID".to_string(),
            ));
        }
        validate_grant(grant)?;
    }
    Ok(grant)
}

fn read_family(store: &KeyedStateStore, id: Ulid) -> Result<Option<TokenFamily>, McpStateError> {
    let family = store.entry::<TokenFamily>(TOKEN_FAMILIES, &id.to_string())?;
    if let Some(family) = &family {
        if family.id != id {
            return Err(McpStateError::Invalid(
                "token family is stored under another ID".to_string(),
            ));
        }
    }
    Ok(family)
}

fn active_grant(
    grant: Option<ConnectionGrant>,
    id: Ulid,
    now: u64,
) -> Result<ConnectionGrant, McpStateError> {
    let grant = grant.ok_or(McpStateError::UnknownGrant(id))?;
    if !grant.is_active_at(now) {
        return Err(McpStateError::InactiveGrant(id));
    }
    Ok(grant)
}

/// Narrows a grant to `current`, which may never widen it. Returns whether
/// the grant changed.
fn attenuate_grant(
    grant: &mut ConnectionGrant,
    current: &PermissionGrant,
) -> Result<bool, McpStateError> {
    if !current.is_subset_of(&grant.approved_permissions) {
        return Err(McpStateError::GrantPermissionWidening(grant.id));
    }
    if current == &grant.approved_permissions {
        return Ok(false);
    }
    grant.approved_permissions = current.clone();
    Ok(true)
}

/// Records a use, at most once a minute so busy clients do not write on every
/// request. Returns whether the grant changed.
fn touch_grant(grant: &mut ConnectionGrant, now: u64) -> bool {
    let stale = grant
        .last_used_at
        .is_none_or(|last_used| now.saturating_sub(last_used) >= GRANT_USE_RESOLUTION_SECONDS);
    if stale {
        grant.last_used_at = Some(now);
    }
    stale
}

fn revoke_families(state: &mut AuthorizationState, grant_ids: &BTreeSet<Ulid>, revoked_at: u64) {
    let keys = state
        .token_families
        .iter()
        .filter(|(_, family)| grant_ids.contains(&family.grant_id))
        .map(|(key, _)| key.clone())
        .collect::<Vec<_>>();
    for key in keys {
        if let Some(family) = state.token_families.get_mut(&key) {
            family.revoked_at.get_or_insert(revoked_at);
        }
    }
}

/// Drops grants and token families that ended (expired or were revoked) more
/// than [`INACTIVE_RETENTION_SECONDS`] before `now`, along with the families
/// of dropped grants. They can never authorize again; keeping them only grew
/// the state with every connection ever made.
fn prune_inactive(state: &mut AuthorizationState, now: u64) {
    let cutoff = now.saturating_sub(INACTIVE_RETENTION_SECONDS);
    let ended = |revoked_at: Option<u64>, expires_at: u64| {
        revoked_at.map_or(expires_at, |revoked_at| revoked_at.min(expires_at)) < cutoff
    };
    let grants = state
        .grants
        .iter()
        .filter(|(_, grant)| ended(grant.revoked_at, grant.expires_at))
        .map(|(key, _)| key.clone())
        .collect::<Vec<_>>();
    for key in grants {
        state.grants.remove(&key);
    }
    let families = state
        .token_families
        .iter()
        .filter(|(_, family)| {
            ended(family.revoked_at, family.expires_at)
                || !state.grants.contains_key(&family.grant_id.to_string())
        })
        .map(|(key, _)| key.clone())
        .collect::<Vec<_>>();
    for key in families {
        state.token_families.remove(&key);
    }
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
    if state.grants.len() > MAX_GRANTS || state.token_families.len() > MAX_TOKEN_FAMILIES {
        return Err(McpStateError::Invalid(
            "remote MCP authorization state exceeds configured entry limits".to_string(),
        ));
    }
    for (key, grant) in &state.grants {
        if *key != grant.id.to_string() {
            return Err(McpStateError::Invalid(
                "connection grant is stored under another ID".to_string(),
            ));
        }
        validate_grant(grant)?;
    }
    for (key, family) in &state.token_families {
        if *key != family.id.to_string() {
            return Err(McpStateError::Invalid(
                "token family is stored under another ID".to_string(),
            ));
        }
        validate_family(family, state.grants.get(&family.grant_id.to_string()))?;
    }
    Ok(())
}

fn validate_grant(grant: &ConnectionGrant) -> Result<(), McpStateError> {
    if grant.version != MCP_CONNECTION_GRANT_VERSION {
        return Err(McpStateError::Invalid(
            "connection grants contain an unsupported version".to_string(),
        ));
    }
    McpRemoteId::parse(grant.remote_id.as_str())
        .map_err(|error| McpStateError::Invalid(error.to_string()))?;
    WikiId::parse(grant.wiki_id.as_str())
        .map_err(|error| McpStateError::Invalid(error.to_string()))?;
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
    })
}

fn validate_family(
    family: &TokenFamily,
    bound_grant: Option<&ConnectionGrant>,
) -> Result<(), McpStateError> {
    if family.version != MCP_TOKEN_FAMILY_VERSION
        || bound_grant.is_none_or(|grant| {
            grant.id != family.grant_id
                || family.client_id != grant.client_id
                || family.audience != grant.audience
        })
        || family.expires_at <= family.created_at
        || family.current_refresh_token_hash.len() != 43
        || family.used_refresh_token_hashes.len() > MAX_USED_REFRESH_TOKENS
        || family
            .used_refresh_token_hashes
            .iter()
            .any(|hash| hash.len() != 43)
    {
        return Err(McpStateError::Invalid(
            "token families contain invalid versions, bindings, timestamps, or hashes".to_string(),
        ));
    }
    Ok(())
}

/// Refuses a state file that is not a private regular file. Returns whether
/// it exists.
fn check_private_file(path: &Path) -> Result<bool, McpStateError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(McpStateError::Io(error)),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(McpStateError::Invalid(format!(
            "remote MCP authorization state at {} is not a regular file",
            path.display()
        )));
    }
    validate_owner_only(&metadata, path)?;
    Ok(true)
}

/// Reads the JSON state written before the `SQLite` store, if it is present.
fn load_legacy_state(path: &Path) -> Result<Option<AuthorizationState>, McpStateError> {
    if !check_private_file(path)? {
        return Ok(None);
    }
    let Some(bytes) = vulcan_core::durable::read_bounded(path, MAX_LEGACY_STATE_BYTES)? else {
        return Ok(None);
    };
    let value: serde_json::Value = serde_json::from_slice(&bytes)?;
    let version = value
        .get("version")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| McpStateError::Invalid("authorization state has no version".to_string()))?;
    if version != u64::from(MCP_AUTHORIZATION_STATE_VERSION) {
        return Err(McpStateError::UnsupportedVersion(
            u32::try_from(version).unwrap_or(u32::MAX),
        ));
    }
    let legacy: LegacyAuthorizationState = serde_json::from_value(value)?;
    let mut grant_ids = BTreeSet::new();
    let mut family_ids = BTreeSet::new();
    if !legacy.grants.iter().all(|grant| grant_ids.insert(grant.id))
        || !legacy
            .token_families
            .iter()
            .all(|family| family_ids.insert(family.id))
    {
        return Err(McpStateError::Invalid(
            "remote MCP authorization state contains duplicate IDs".to_string(),
        ));
    }
    let state = AuthorizationState {
        grants: legacy
            .grants
            .into_iter()
            .map(|grant| (grant.id.to_string(), grant))
            .collect(),
        token_families: legacy
            .token_families
            .into_iter()
            .map(|family| (family.id.to_string(), family))
            .collect(),
    };
    validate_state(&state)?;
    Ok(Some(state))
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

#[cfg(windows)]
fn validate_owner_only(_metadata: &fs::Metadata, path: &Path) -> Result<(), McpStateError> {
    vulcan_app::windows_acl::verify_private_path(path).map_err(|error| {
        McpStateError::Invalid(format!(
            "remote MCP authorization state at {} is not private to the current user: {error}",
            path.display()
        ))
    })
}

#[cfg(not(any(unix, windows)))]
#[allow(clippy::unnecessary_wraps)]
fn validate_owner_only(_metadata: &fs::Metadata, _path: &Path) -> Result<(), McpStateError> {
    Ok(())
}

#[derive(Debug)]
pub enum McpStateError {
    UnknownGrant(Ulid),
    InactiveGrant(Ulid),
    GrantBindingMismatch(Ulid),
    GrantPermissionWidening(Ulid),
    UnknownTokenFamily(Ulid),
    InactiveTokenFamily(Ulid),
    InvalidRefreshToken,
    RefreshTokenReplay(Ulid),
    UnsupportedVersion(u32),
    Invalid(String),
    Random(String),
    Io(std::io::Error),
    Json(serde_json::Error),
    Store(String),
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
            Self::GrantPermissionWidening(id) => {
                write!(
                    formatter,
                    "connection grant `{id}` cannot regain narrowed permissions"
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
            Self::Invalid(detail) | Self::Random(detail) | Self::Store(detail) => {
                formatter.write_str(detail)
            }
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

impl From<vulcan_app::AppError> for McpStateError {
    fn from(error: vulcan_app::AppError) -> Self {
        Self::Store(error.to_string())
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

        let stored = fs::read(store.path()).expect("state file");
        assert!(!stored
            .windows(b"refresh_token".len())
            .any(|window| window == b"refresh_token"));
        let reloaded = McpAuthorizationStore::at(temporary.path());
        assert_eq!(reloaded.show_grant(report.id).expect("reloaded"), report);
    }

    #[test]
    fn grant_permissions_only_attenuate_and_survive_restart() {
        let temporary = tempdir().expect("temporary");
        let store = McpAuthorizationStore::at(temporary.path());
        let original = permission_grant();
        let grant = store
            .create_grant(
                grant_request(
                    "personal",
                    "https://client.example.test/metadata.json",
                    "https://identity.example.test/alice",
                ),
                false,
            )
            .expect("grant");
        let mut narrowed = original.clone();
        narrowed.read = PathPermission::default();
        let updated = store
            .attenuate_grant_permissions(grant.id, &narrowed, 2_000)
            .expect("narrow grant");
        assert_eq!(updated.approved_permissions, narrowed);
        let reloaded = McpAuthorizationStore::at(temporary.path());
        assert_eq!(
            reloaded
                .show_grant(grant.id)
                .expect("reloaded")
                .approved_permissions,
            narrowed
        );
        assert!(matches!(
            reloaded.attenuate_grant_permissions(grant.id, &original, 2_001),
            Err(McpStateError::GrantPermissionWidening(_))
        ));
        assert_eq!(
            reloaded
                .show_grant(grant.id)
                .expect("unchanged")
                .approved_permissions,
            narrowed
        );
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
        let used = store
            .mark_grant_used(grant.id, 1_500)
            .expect("mark grant used");
        assert_eq!(used.last_used_at, Some(1_500));
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

    #[test]
    fn revoking_a_remote_revokes_only_its_grants_and_token_families() {
        let temporary = tempdir().expect("temporary");
        let store = McpAuthorizationStore::at(temporary.path());
        let first = store
            .create_grant(
                grant_request("personal-chatgpt", "client-a", "https://id.test/alice"),
                false,
            )
            .expect("first grant");
        let second = store
            .create_grant(
                grant_request("work-chatgpt", "client-b", "https://id.test/bob"),
                false,
            )
            .expect("second grant");
        let first_token = store
            .issue_refresh_token(first.id, 9_000, 1_001)
            .expect("first refresh token");
        let second_token = store
            .issue_refresh_token(second.id, 9_000, 1_001)
            .expect("second refresh token");

        let revoked = store
            .revoke_remote_grants(&first.remote_id, 2_000, false)
            .expect("revoke remote");
        assert_eq!(revoked.len(), 1);
        assert_eq!(revoked[0].id, first.id);
        assert!(matches!(
            store.rotate_refresh_token(first_token.family_id, first_token.secret.expose(), 2_001),
            Err(McpStateError::InactiveTokenFamily(_))
        ));
        assert!(store
            .rotate_refresh_token(second_token.family_id, second_token.secret.expose(), 2_001)
            .is_ok());
    }

    #[test]
    fn revoking_one_remote_wiki_is_dry_run_safe_and_preserves_other_vaults() {
        let temporary = tempdir().expect("temporary");
        let store = McpAuthorizationStore::at(temporary.path());
        let personal = store
            .create_grant(
                grant_request("shared", "client-a", "https://id.test/alice"),
                false,
            )
            .expect("personal grant");
        let mut team_request = grant_request("shared", "client-b", "https://id.test/bob");
        team_request.wiki_id = WikiId::parse("team").expect("wiki ID");
        let team = store.create_grant(team_request, false).expect("team grant");
        let personal_refresh = store
            .issue_refresh_token(personal.id, 9_000, 1_001)
            .expect("personal refresh");
        let team_refresh = store
            .issue_refresh_token(team.id, 9_000, 1_001)
            .expect("team refresh");

        let preview = store
            .revoke_remote_wiki_grants(&team.remote_id, &team.wiki_id, 2_000, true)
            .expect("preview");
        assert_eq!(preview.len(), 1);
        assert_eq!(preview[0].id, team.id);
        assert!(store
            .show_grant(team.id)
            .expect("unchanged")
            .revoked_at
            .is_none());

        let revoked = store
            .revoke_remote_wiki_grants(&team.remote_id, &team.wiki_id, 2_000, false)
            .expect("revoke team");
        assert_eq!(revoked[0].revoked_at, Some(2_000));
        assert!(matches!(
            store.rotate_refresh_token(team_refresh.family_id, team_refresh.secret.expose(), 2_001),
            Err(McpStateError::InactiveTokenFamily(_))
        ));
        assert!(store
            .rotate_refresh_token(
                personal_refresh.family_id,
                personal_refresh.secret.expose(),
                2_001
            )
            .is_ok());
        assert!(store
            .show_grant(personal.id)
            .expect("personal")
            .revoked_at
            .is_none());
    }

    fn write_private(path: &Path, contents: &[u8]) {
        fs::create_dir_all(path.parent().expect("parent")).expect("state directory");
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        std::io::Write::write_all(&mut options.open(path).expect("create"), contents)
            .expect("write");
    }

    #[test]
    fn json_state_is_readable_before_and_migrated_once_by_a_write() {
        let source = tempdir().expect("source");
        let original = McpAuthorizationStore::at(source.path());
        let grant = original
            .create_grant(
                grant_request("personal", "client", "https://id.test/alice"),
                false,
            )
            .expect("grant");
        let refresh = original
            .issue_refresh_token(grant.id, 9_000, 1_001)
            .expect("refresh");
        let state = original.load().expect("state");
        let legacy = serde_json::json!({
            "version": MCP_AUTHORIZATION_STATE_VERSION,
            "grants": state.grants.values().collect::<Vec<_>>(),
            "token_families": state.token_families.values().collect::<Vec<_>>(),
        });

        let temporary = tempdir().expect("temporary");
        let store = McpAuthorizationStore::at(temporary.path());
        let legacy_path = store.legacy_path();
        write_private(&legacy_path, &serde_json::to_vec(&legacy).expect("json"));

        // Reads and dry runs use the JSON state without creating the store.
        assert_eq!(store.show_grant(grant.id).expect("legacy grant"), grant);
        assert!(store
            .resolve_active_grant(
                grant.id,
                grant.remote_instance_id,
                &grant.client_id,
                &grant.audience,
                2_000,
            )
            .is_ok());
        store
            .revoke_grant(grant.id, 2_000, true)
            .expect("dry-run revoke");
        assert!(!store.path().exists());
        assert!(legacy_path.is_file());

        // The first write imports everything and keeps the JSON as a backup.
        let rotated = store
            .rotate_refresh_token(refresh.family_id, refresh.secret.expose(), 2_000)
            .expect("rotate migrated family");
        assert!(store.path().is_file());
        assert!(!legacy_path.exists());
        assert!(legacy_path.with_extension("json.migrated").is_file());
        assert_eq!(store.list_grants(None).expect("grants").len(), 1);
        assert_eq!(
            store.show_grant(grant.id).expect("grant").last_used_at,
            Some(2_000)
        );
        let reopened = McpAuthorizationStore::at(temporary.path());
        assert!(reopened
            .rotate_refresh_token(refresh.family_id, rotated.secret.expose(), 2_001)
            .is_ok());
    }

    #[test]
    fn ended_grants_and_families_are_pruned_after_the_retention_window() {
        let temporary = tempdir().expect("temporary");
        let store = McpAuthorizationStore::at(temporary.path());
        let expired = store
            .create_grant(
                grant_request("personal", "client-a", "https://id.test/a"),
                false,
            )
            .expect("expired grant");
        store
            .issue_refresh_token(expired.id, 9_000, 1_001)
            .expect("expired family");
        let mut revocable = grant_request("personal", "client-b", "https://id.test/b");
        revocable.expires_at = 10 * INACTIVE_RETENTION_SECONDS;
        let revoked = store.create_grant(revocable, false).expect("revoked grant");
        let mut long_lived = grant_request("personal", "client-c", "https://id.test/c");
        long_lived.expires_at = 10 * INACTIVE_RETENTION_SECONDS;
        let active = store.create_grant(long_lived, false).expect("active grant");
        let old_family = store
            .issue_refresh_token(active.id, 9_000, 1_001)
            .expect("old family");
        let later = 10_000 + INACTIVE_RETENTION_SECONDS;
        store
            .revoke_grant(revoked.id, later - 1, false)
            .expect("recent revocation");

        // Within the window everything is still listed.
        let mut next = grant_request("personal", "client-d", "https://id.test/d");
        next.created_at = later - 1;
        next.expires_at = later + 1_000;
        store
            .create_grant(next, false)
            .expect("grant inside window");
        assert_eq!(store.list_grants(None).expect("grants").len(), 4);

        let mut next = grant_request("personal", "client-e", "https://id.test/e");
        next.created_at = later + 1;
        next.expires_at = later + 1_000;
        store.create_grant(next, false).expect("grant after window");
        let remaining = store
            .list_grants(None)
            .expect("grants")
            .into_iter()
            .map(|grant| grant.id)
            .collect::<BTreeSet<_>>();
        assert!(!remaining.contains(&expired.id));
        assert!(remaining.contains(&revoked.id));
        assert!(remaining.contains(&active.id));
        assert!(store
            .list_token_families(Some(expired.id))
            .expect("families")
            .is_empty());
        assert!(store
            .list_token_families(Some(active.id))
            .expect("families")
            .iter()
            .all(|family| family.id != old_family.family_id));
    }

    #[test]
    fn grant_use_attenuates_and_records_at_most_once_a_minute() {
        let temporary = tempdir().expect("temporary");
        let store = McpAuthorizationStore::at(temporary.path());
        let grant = store
            .create_grant(
                grant_request("personal", "client", "https://id.test/a"),
                false,
            )
            .expect("grant");
        let original = permission_grant();
        let first = store
            .record_grant_use(grant.id, &original, 1_500)
            .expect("first use");
        assert_eq!(first.last_used_at, Some(1_500));
        let quick = store
            .record_grant_use(grant.id, &original, 1_530)
            .expect("quick use");
        assert_eq!(quick.last_used_at, Some(1_500));

        let mut narrowed = original.clone();
        narrowed.read = PathPermission::default();
        let attenuated = store
            .record_grant_use(grant.id, &narrowed, 1_540)
            .expect("attenuating use");
        assert_eq!(attenuated.approved_permissions, narrowed);
        let stored = store.show_grant(grant.id).expect("stored");
        assert_eq!(stored.approved_permissions, narrowed);
        assert_eq!(stored.last_used_at, Some(1_500));
        assert!(matches!(
            store.record_grant_use(grant.id, &original, 1_600),
            Err(McpStateError::GrantPermissionWidening(_))
        ));
        assert_eq!(
            store
                .record_grant_use(grant.id, &narrowed, 1_600)
                .expect("later use")
                .last_used_at,
            Some(1_600)
        );
        assert!(matches!(
            store.record_grant_use(grant.id, &narrowed, 10_000),
            Err(McpStateError::InactiveGrant(_))
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
