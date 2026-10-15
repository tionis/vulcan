//! Reads a CLI routes to the resident daemon (Roadmap 10.5, Part B of
//! `docs/specs/index-ownership-and-client-routing.md`).
//!
//! A vault is routable while the daemon's index consumer runs for it. Each
//! read first passes an index barrier, so every change made before the
//! request is indexed, then runs the shared operation from
//! `vulcan_app::routed` over the vault's retained note-store session, and
//! returns a full-fidelity wire report the client prints itself.

use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use vulcan_app::routed::ls::{run_ls, LsRequest};
use vulcan_app::routed::query::{run_query, QueryRequest};
use vulcan_app::routed::search;
use vulcan_app::routed::wire::QueryReportWire;
use vulcan_core::note_session::NoteStoreSession;
use vulcan_core::search::{SearchQuery, SearchReport};
use vulcan_core::{
    resolve_permission_profile, PermissionFilter, PermissionGuard, ProfilePermissionGuard,
    VaultPaths,
};

use crate::scan_runtime::{request_index_barrier, CacheFreshnessState, VaultScanTracker};

/// How long a read waits for its index barrier before declining.
const BARRIER_TIMEOUT: Duration = Duration::from_secs(5);

/// A vault the daemon can answer routed reads for.
pub struct RoutedVault {
    pub paths: VaultPaths,
    pub tracker: Arc<VaultScanTracker>,
    /// The registration's permission ceiling, if it narrows access.
    pub permissions_profile: Option<String>,
    session: NoteStoreSession,
}

impl RoutedVault {
    #[must_use]
    pub fn new(
        paths: VaultPaths,
        tracker: Arc<VaultScanTracker>,
        permissions_profile: Option<String>,
    ) -> Self {
        Self {
            session: NoteStoreSession::new(paths.clone()),
            paths,
            tracker,
            permissions_profile,
        }
    }
}

impl RoutedVault {
    /// Run `read` over the retained session, or the cache directly while
    /// the session has no snapshot.
    fn with_store<T>(
        &self,
        read: impl FnOnce(&dyn vulcan_core::note_store::NoteStore) -> Result<T, vulcan_app::AppError>,
    ) -> Result<T, RoutedReadError> {
        match self.session.snapshot() {
            Some(store) => read(&store),
            None => read(&vulcan_core::note_store::DirectNoteStore::new(&self.paths)),
        }
        .map_err(|error| RoutedReadError::Failed(error.to_string()))
    }
}

/// The routable vaults, by wiki id: filled while index consumers run.
#[derive(Clone, Default)]
pub struct RoutedReadRegistry {
    vaults: Arc<Mutex<HashMap<String, Arc<RoutedVault>>>>,
}

impl RoutedReadRegistry {
    pub fn insert(&self, wiki_id: &str, vault: RoutedVault) {
        lock(&self.vaults).insert(wiki_id.to_string(), Arc::new(vault));
    }

    pub fn remove(&self, wiki_id: &str) {
        lock(&self.vaults).remove(wiki_id);
    }

    #[must_use]
    pub fn get(&self, wiki_id: &str) -> Option<Arc<RoutedVault>> {
        lock(&self.vaults).get(wiki_id).cloned()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A routed `query`: the shared request plus the caller's context.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoutedQuery {
    pub request: QueryRequest,
    /// The caller's `--permissions` profile, resolved by the vault's config.
    pub permissions: Option<String>,
    /// Whether the caller's refresh mode requires a fresh index.
    pub refresh: bool,
}

/// Why a routed read was not answered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "code", content = "message", rename_all = "snake_case")]
pub enum RoutedReadError {
    /// The daemon cannot answer as the client would; run directly instead.
    Unavailable(String),
    /// The read failed as running it directly would (an invalid query or
    /// profile, a missing cache): report it rather than retry.
    Failed(String),
}

impl Display for RoutedReadError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(reason) => write!(formatter, "routed read unavailable: {reason}"),
            Self::Failed(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for RoutedReadError {}

/// A routed `ls`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoutedLs {
    pub request: LsRequest,
    /// The caller's `--permissions` profile, resolved by the vault's config.
    pub permissions: Option<String>,
    /// Whether the caller's refresh mode requires a fresh index.
    pub refresh: bool,
}

/// A routed keyword `search`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoutedSearch {
    pub query: SearchQuery,
    /// The caller's `--permissions` profile, resolved by the vault's config.
    pub permissions: Option<String>,
    /// Whether the caller's refresh mode requires a fresh index.
    pub refresh: bool,
}

/// The checks every routed read passes before it runs: the registration's
/// permission ceiling, the index barrier, and the caller's profile. Returns
/// the read filter the caller's profile imposes, if any.
fn prepare_read(
    vault: &RoutedVault,
    permissions: Option<&str>,
    refresh: bool,
) -> Result<Option<PermissionFilter>, RoutedReadError> {
    if vault.permissions_profile.is_some() {
        // Intersecting a registration's ceiling with the caller's profile
        // could narrow the result below the direct one; decline instead.
        return Err(RoutedReadError::Unavailable(
            "the registration narrows permissions".to_string(),
        ));
    }
    if refresh {
        let completion =
            request_index_barrier(vault.paths.vault_root(), &vault.tracker, BARRIER_TIMEOUT)
                .map_err(|error| RoutedReadError::Unavailable(error.to_string()))?;
        if completion.state != CacheFreshnessState::Fresh {
            return Err(RoutedReadError::Unavailable(format!(
                "the daemon cannot vouch for the index ({:?})",
                completion.state
            )));
        }
    }
    let profile = resolve_permission_profile(&vault.paths, permissions)
        .map_err(|error| RoutedReadError::Failed(error.to_string()))?;
    let guard = ProfilePermissionGuard::new(&vault.paths, profile);
    let filter = guard.read_filter();
    Ok((!filter.path_permission().is_unrestricted()).then_some(filter))
}

/// Answer a routed `query` from the vault's retained session.
pub fn routed_query(
    vault: &RoutedVault,
    query: &RoutedQuery,
) -> Result<QueryReportWire, RoutedReadError> {
    let filter = prepare_read(vault, query.permissions.as_deref(), query.refresh)?;
    let report = vault
        .with_store(|store| run_query(store, &vault.paths, &query.request, filter.as_ref()))?;
    QueryReportWire::from_report(report).ok_or_else(|| {
        RoutedReadError::Unavailable("the report carries a publication selection".to_string())
    })
}

/// Answer a routed `ls` from the vault's retained session.
pub fn routed_ls(vault: &RoutedVault, ls: &RoutedLs) -> Result<QueryReportWire, RoutedReadError> {
    let filter = prepare_read(vault, ls.permissions.as_deref(), ls.refresh)?;
    let report =
        vault.with_store(|store| run_ls(store, &vault.paths, &ls.request, filter.as_ref()))?;
    QueryReportWire::from_report(report).ok_or_else(|| {
        RoutedReadError::Unavailable("the report carries a publication selection".to_string())
    })
}

/// Answer a routed keyword `search` from the vault's fresh cache.
pub fn routed_search(
    vault: &RoutedVault,
    search: &RoutedSearch,
) -> Result<SearchReport, RoutedReadError> {
    if !search::routable(&search.query) {
        return Err(RoutedReadError::Unavailable(
            "hybrid search embeds the query with the client's provider".to_string(),
        ));
    }
    let filter = prepare_read(vault, search.permissions.as_deref(), search.refresh)?;
    search::run_search(&vault.paths, &search.query, filter.as_ref())
        .map_err(|error| RoutedReadError::Failed(error.to_string()))
}
