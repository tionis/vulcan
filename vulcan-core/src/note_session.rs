//! A long-lived host's retained note store (QRY.6,
//! `docs/specs/query-architecture.md` §4.6).
//!
//! A [`NoteStoreSession`] hands out [`NoteStoreSnapshot`]s: read-only cache
//! connections holding an open read transaction that was begun while no
//! cooperating writer held the vault lock. Every Vulcan cache write runs in
//! an exclusive write section that advances the write epoch when it ends,
//! so a snapshot pinned at epoch `E` shows exactly the cache after every
//! write section that completed by `E`, and none of a later one. It stays
//! valid while the epoch is still `E`, including while a writer is inside
//! its section: serving the state from before an incomplete write is
//! linearizable. Readers therefore never wait for writers, and never see a
//! half-applied write section. When no idle snapshot is pinned at the
//! current epoch and a writer holds the lock, [`NoteStoreSession::snapshot`]
//! returns `None` and callers use the direct, read-locked path.
//!
//! Inside a snapshot the session reuses what earlier snapshots loaded,
//! bound to the versions that describe it:
//!
//! - identity facts per read scope, while the store clock is unchanged
//!   (`note_store_clock`, schema v28, advanced by every `note_query` change);
//! - stored-field records per path, while the row's `row_version` is the
//!   identity's and the bookmarks are the same;
//! - hydrated file objects per scope, only within one write epoch, clock,
//!   configuration, and bookmark set, since incoming links, tasks, and lists
//!   depend on rows other than the note's own.
//!
//! Scopes with a policy hook, or restricted to an explicit universe, are not
//! retained; they still read the pinned snapshot.

use crate::config::VaultConfig;
use crate::note_lookup::{IdentityIndex, IndexedNoteLookup};
use crate::note_store::NoteStore;
use crate::properties::{
    hydrate_shared_notes, load_readable_identities, load_stored_notes, NoteIndexReadScope,
    NoteRecord, PropertyError, ReadableIdentities,
};
use crate::{CacheDatabase, VaultPaths};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

/// Idle pinned connections kept for concurrent requests.
const IDLE_SNAPSHOTS: usize = 8;
/// Retained records per map; beyond it new records are not retained.
const RETAINED_NOTES: usize = 100_000;
/// Read scopes with retained identities and hydrated records.
const RETAINED_SCOPES: usize = 8;

/// The store clock a snapshot read: which cache file, at which version.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Clock {
    store_id: String,
    version: i64,
}

/// A pooled connection, possibly pinned at a write epoch.
struct Pinned {
    database: CacheDatabase,
    /// The cache file the connection opened, as (device, inode).
    file: Option<(u64, u64)>,
    /// The epoch and clock of the open read transaction, if any.
    pin: Option<(u64, Clock)>,
}

impl Pinned {
    fn unpin(&mut self) {
        if self.pin.take().is_some() {
            let _ = self.database.connection().execute_batch("ROLLBACK");
        }
    }
}

/// Stored-field records retained across snapshots.
#[derive(Default)]
struct RetainedStored {
    store_id: String,
    bookmarks: HashSet<String>,
    /// Path to the row version the record describes, and the record.
    notes: HashMap<String, (i64, Arc<NoteRecord>)>,
}

/// Hydrated records of one scope, valid for one key.
struct RetainedHydrated {
    epoch: u64,
    clock: Clock,
    config: Arc<VaultConfig>,
    bookmarks: Arc<HashSet<String>>,
    notes: HashMap<String, Arc<NoteRecord>>,
}

/// Work counters since the session began, for diagnostics and tests.
#[derive(Debug, Default)]
pub struct NoteSessionCounters {
    pub snapshots_pinned: AtomicU64,
    pub snapshots_reused: AtomicU64,
    /// Requests that found no snapshot and took the direct path.
    pub snapshots_unavailable: AtomicU64,
    pub identity_loads: AtomicU64,
    pub identity_reuses: AtomicU64,
    pub stored_loaded: AtomicU64,
    pub stored_reused: AtomicU64,
    pub hydrated_loaded: AtomicU64,
    pub hydrated_reused: AtomicU64,
}

fn count(counter: &AtomicU64, by: usize) {
    counter.fetch_add(u64::try_from(by).unwrap_or(u64::MAX), Ordering::Relaxed);
}

/// Retained note-store state shared by concurrent requests.
pub struct NoteStoreSession {
    paths: VaultPaths,
    counters: NoteSessionCounters,
    idle: Mutex<Vec<Pinned>>,
    identities: Mutex<HashMap<String, (Clock, Arc<IdentityIndex>)>>,
    stored: RwLock<RetainedStored>,
    hydrated: RwLock<HashMap<String, RetainedHydrated>>,
}

impl NoteStoreSession {
    #[must_use]
    pub fn new(paths: VaultPaths) -> Self {
        Self {
            paths,
            counters: NoteSessionCounters::default(),
            idle: Mutex::new(Vec::new()),
            identities: Mutex::new(HashMap::new()),
            stored: RwLock::new(RetainedStored::default()),
            hydrated: RwLock::new(HashMap::new()),
        }
    }

    #[must_use]
    pub fn paths(&self) -> &VaultPaths {
        &self.paths
    }

    #[must_use]
    pub fn counters(&self) -> &NoteSessionCounters {
        &self.counters
    }

    /// A snapshot consistent with every completed cooperating write, or
    /// `None` when none can be pinned without waiting for a writer, an
    /// interrupted ordinary write needs recovery, or there is no current
    /// cache. Callers then use the direct path, which reports or waits.
    #[must_use]
    pub fn snapshot(&self) -> Option<NoteStoreSnapshot<'_>> {
        let epoch = crate::write_lock::read_write_epoch(&self.paths).ok()?;
        let reusable = {
            let mut idle = lock(&self.idle);
            release_stale(&mut idle, epoch);
            idle.iter()
                .position(|pinned| pinned.pin.as_ref().is_some_and(|pin| pin.0 == epoch))
                .map(|index| idle.swap_remove(index))
        };
        let pinned = if let Some(pinned) = reusable {
            count(&self.counters.snapshots_reused, 1);
            pinned
        } else {
            let Some(pinned) = self.pin() else {
                count(&self.counters.snapshots_unavailable, 1);
                return None;
            };
            count(&self.counters.snapshots_pinned, 1);
            pinned
        };
        let (epoch, clock) = pinned.pin.clone()?;
        let bookmarks = Arc::new(crate::properties::load_bookmarked_paths(
            self.paths.vault_root(),
        ));
        let config = Arc::new(crate::load_vault_config(&self.paths).config);
        Some(NoteStoreSnapshot {
            session: self,
            database: Some(Rc::new(pinned.database)),
            file: pinned.file,
            epoch,
            clock,
            bookmarks,
            config,
        })
    }

    /// Unpin idle snapshots of an earlier epoch so they no longer hold old
    /// WAL frames. Hosts call this periodically; [`Self::snapshot`] does too.
    pub fn release_stale(&self) {
        if let Ok(epoch) = crate::write_lock::read_write_epoch(&self.paths) {
            release_stale(&mut lock(&self.idle), epoch);
        }
    }

    /// Pin a connection while no writer is in its section.
    fn pin(&self) -> Option<Pinned> {
        if !self.paths.cache_db().exists() {
            return None;
        }
        let guard = crate::write_lock::try_acquire_read_lock(&self.paths).ok()??;
        crate::ordinary_write::ensure_no_pending_ordinary_write_batch(&self.paths).ok()?;
        let epoch = crate::write_lock::read_write_epoch(&self.paths).ok()?;
        let file = cache_file_identity(&self.paths);
        let reused = {
            let mut idle = lock(&self.idle);
            idle.iter()
                .position(|pinned| pinned.pin.is_none() && pinned.file == file)
                .map(|index| idle.swap_remove(index))
        };
        let mut pinned = match reused {
            Some(pinned) => pinned,
            None => Pinned {
                database: CacheDatabase::open(&self.paths).ok()?,
                file,
                pin: None,
            },
        };
        let connection = pinned.database.connection();
        connection.execute_batch("BEGIN").ok()?;
        // The first read starts the snapshot.
        let clock = connection
            .query_row(
                "SELECT store_id, version FROM note_store_clock",
                [],
                |row| {
                    Ok(Clock {
                        store_id: row.get(0)?,
                        version: row.get(1)?,
                    })
                },
            )
            .ok();
        drop(guard);
        let Some(clock) = clock else {
            let _ = connection.execute_batch("ROLLBACK");
            return None;
        };
        pinned.pin = Some((epoch, clock));
        Some(pinned)
    }

    fn give_back(&self, pinned: Pinned) {
        let mut idle = lock(&self.idle);
        if idle.len() < IDLE_SNAPSHOTS {
            idle.push(pinned);
        }
    }
}

fn release_stale(idle: &mut [Pinned], epoch: u64) {
    for pinned in idle.iter_mut() {
        if pinned.pin.as_ref().is_some_and(|pin| pin.0 != epoch) {
            pinned.unpin();
        }
    }
}

#[cfg(unix)]
fn cache_file_identity(paths: &VaultPaths) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::metadata(paths.cache_db()).ok()?;
    Some((metadata.dev(), metadata.ino()))
}

#[cfg(not(unix))]
fn cache_file_identity(_paths: &VaultPaths) -> Option<(u64, u64)> {
    None
}

/// Retained state stays usable after a panicking request: every guarded
/// value is a cache checked against versions before use.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn read<T>(lock: &RwLock<T>) -> std::sync::RwLockReadGuard<'_, T> {
    lock.read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn write<T>(lock: &RwLock<T>) -> std::sync::RwLockWriteGuard<'_, T> {
    lock.write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// One request's view of the note store: a pinned read transaction plus the
/// session's retained notes at matching versions. Returned to the session's
/// pool when dropped.
pub struct NoteStoreSnapshot<'s> {
    session: &'s NoteStoreSession,
    database: Option<Rc<CacheDatabase>>,
    file: Option<(u64, u64)>,
    epoch: u64,
    clock: Clock,
    bookmarks: Arc<HashSet<String>>,
    config: Arc<VaultConfig>,
}

impl NoteStoreSnapshot<'_> {
    /// The write epoch the snapshot is pinned at.
    #[must_use]
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    fn database(&self) -> &Rc<CacheDatabase> {
        self.database.as_ref().expect("held until drop")
    }

    /// Identities of a retainable scope, reused while the clock matches.
    fn identities(
        &self,
        scope_key: &str,
        scope: NoteIndexReadScope<'_>,
    ) -> Result<(Arc<IdentityIndex>, ReadableIdentities), PropertyError> {
        let retained = lock(&self.session.identities)
            .get(scope_key)
            .filter(|(clock, _)| *clock == self.clock)
            .map(|(_, index)| Arc::clone(index));
        if let Some(index) = retained {
            count(&self.session.counters.identity_reuses, 1);
            let filter = match scope {
                NoteIndexReadScope::Filter(filter) => filter.cloned(),
                NoteIndexReadScope::Guard(guard) => Some(guard.read_filter()),
            };
            return Ok((
                index,
                ReadableIdentities {
                    identities: Vec::new(),
                    filter,
                    readable_sources: None,
                },
            ));
        }
        count(&self.session.counters.identity_loads, 1);
        let mut readable = load_readable_identities(self.database(), scope, None)?;
        let index = Arc::new(IdentityIndex::new(std::mem::take(&mut readable.identities)));
        let mut identities = lock(&self.session.identities);
        if identities.len() >= RETAINED_SCOPES && !identities.contains_key(scope_key) {
            identities.clear();
        }
        identities.insert(
            scope_key.to_string(),
            (self.clock.clone(), Arc::clone(&index)),
        );
        Ok((index, readable))
    }

    fn retained_lookup(
        &self,
        scope_key: String,
        index: Arc<IdentityIndex>,
        filter: Option<crate::PermissionFilter>,
    ) -> IndexedNoteLookup<'_> {
        let stored_index = Arc::clone(&index);
        let load_stored =
            move |wanted: Option<&[&str]>| self.load_stored_retained(&stored_index, wanted);
        let hydrate = move |notes: Vec<Arc<NoteRecord>>| {
            self.hydrate_retained(&scope_key, filter.as_ref(), notes)
        };
        IndexedNoteLookup::from_index(index, Box::new(load_stored), Box::new(hydrate))
            .with_database(Rc::clone(self.database()))
    }

    fn load_stored_retained(
        &self,
        index: &IdentityIndex,
        wanted: Option<&[&str]>,
    ) -> Result<Vec<Arc<NoteRecord>>, PropertyError> {
        let wanted = wanted.map_or_else(
            || {
                index
                    .iter()
                    .map(|identity| identity.path.as_str())
                    .collect()
            },
            <[&str]>::to_vec,
        );
        let mut found = Vec::with_capacity(wanted.len());
        let mut missing = Vec::new();
        {
            let retained = read(&self.session.stored);
            let current =
                retained.store_id == self.clock.store_id && retained.bookmarks == *self.bookmarks;
            for path in wanted {
                let Some(identity) = index.get(path) else {
                    continue;
                };
                match retained.notes.get(path) {
                    Some((version, note))
                        if current
                            && identity.row_version != 0
                            && *version == identity.row_version =>
                    {
                        found.push(Arc::clone(note));
                    }
                    _ => missing.push(path),
                }
            }
        }
        count(&self.session.counters.stored_reused, found.len());
        count(&self.session.counters.stored_loaded, missing.len());
        if missing.is_empty() {
            return Ok(found);
        }
        let scan = missing.len() * 2 >= index.len();
        let loaded = load_stored_notes(
            self.database().connection(),
            self.session.paths.vault_root(),
            &self.bookmarks,
            (!scan).then_some(missing.as_slice()),
        )?;
        let missing = missing.into_iter().collect::<HashSet<_>>();
        let mut retained = write(&self.session.stored);
        if retained.store_id != self.clock.store_id || retained.bookmarks != *self.bookmarks {
            *retained = RetainedStored {
                store_id: self.clock.store_id.clone(),
                bookmarks: (*self.bookmarks).clone(),
                notes: HashMap::new(),
            };
        }
        for stored in loaded {
            let path = stored.record.document_path.as_str();
            if !missing.contains(path) {
                continue;
            }
            let Some(identity) = index.get(path) else {
                continue;
            };
            let mut record = stored.record;
            record.aliases.clone_from(&identity.aliases);
            let record = Arc::new(record);
            if stored.ctime_recorded
                && identity.row_version != 0
                && retained.notes.len() < RETAINED_NOTES
            {
                retained.notes.insert(
                    identity.path.clone(),
                    (identity.row_version, Arc::clone(&record)),
                );
            }
            found.push(record);
        }
        Ok(found)
    }

    fn hydrate_retained(
        &self,
        scope_key: &str,
        filter: Option<&crate::PermissionFilter>,
        notes: Vec<Arc<NoteRecord>>,
    ) -> Result<Vec<Arc<NoteRecord>>, PropertyError> {
        let is_current = |retained: &RetainedHydrated| {
            retained.epoch == self.epoch
                && retained.clock == self.clock
                && retained.config == self.config
                && retained.bookmarks == self.bookmarks
        };
        let mut hydrated = Vec::with_capacity(notes.len());
        let mut missing = Vec::new();
        {
            let retained = read(&self.session.hydrated);
            let current = retained
                .get(scope_key)
                .filter(|retained| is_current(retained));
            for note in notes {
                match current.and_then(|retained| retained.notes.get(&note.document_path)) {
                    Some(hit) => hydrated.push(Arc::clone(hit)),
                    None => missing.push(note),
                }
            }
        }
        count(&self.session.counters.hydrated_reused, hydrated.len());
        count(&self.session.counters.hydrated_loaded, missing.len());
        if missing.is_empty() {
            return Ok(hydrated);
        }
        let loaded = hydrate_shared_notes(
            self.database().connection(),
            &self.config,
            filter,
            None,
            missing,
        )?;
        let mut retained = write(&self.session.hydrated);
        if retained.len() >= RETAINED_SCOPES && !retained.contains_key(scope_key) {
            retained.clear();
        }
        let entry = retained
            .entry(scope_key.to_string())
            .or_insert_with(|| RetainedHydrated {
                epoch: self.epoch,
                clock: self.clock.clone(),
                config: Arc::clone(&self.config),
                bookmarks: Arc::clone(&self.bookmarks),
                notes: HashMap::new(),
            });
        if !is_current(entry) {
            *entry = RetainedHydrated {
                epoch: self.epoch,
                clock: self.clock.clone(),
                config: Arc::clone(&self.config),
                bookmarks: Arc::clone(&self.bookmarks),
                notes: HashMap::new(),
            };
        }
        for note in &loaded {
            if entry.notes.len() < RETAINED_NOTES {
                entry
                    .notes
                    .insert(note.document_path.clone(), Arc::clone(note));
            }
        }
        hydrated.extend(loaded);
        Ok(hydrated)
    }

    /// A lookup over the pinned transaction that retains nothing.
    fn unretained_lookup(&self, readable: ReadableIdentities) -> IndexedNoteLookup<'_> {
        let ReadableIdentities {
            identities,
            filter,
            readable_sources,
        } = readable;
        let load_stored = move |wanted: Option<&[&str]>| {
            Ok(load_stored_notes(
                self.database().connection(),
                self.session.paths.vault_root(),
                &self.bookmarks,
                wanted,
            )?
            .into_iter()
            .map(|stored| Arc::new(stored.record))
            .collect())
        };
        let hydrate = move |notes| {
            hydrate_shared_notes(
                self.database().connection(),
                &self.config,
                filter.as_ref(),
                readable_sources.as_ref(),
                notes,
            )
        };
        IndexedNoteLookup::new(identities, Box::new(load_stored), Box::new(hydrate))
            .with_database(Rc::clone(self.database()))
    }
}

impl NoteStore for NoteStoreSnapshot<'_> {
    fn lookup<'s>(
        &'s self,
        scope: NoteIndexReadScope<'_>,
        within: Option<&HashSet<String>>,
    ) -> Result<IndexedNoteLookup<'s>, PropertyError> {
        let policy = match scope {
            NoteIndexReadScope::Guard(guard) => guard.has_policy_hook(),
            NoteIndexReadScope::Filter(_) => false,
        };
        if policy || within.is_some() {
            let readable = load_readable_identities(self.database(), scope, within)?;
            return Ok(self.unretained_lookup(readable));
        }
        let scope_key = match scope {
            NoteIndexReadScope::Filter(filter) => format!("{filter:?}"),
            NoteIndexReadScope::Guard(guard) => format!("{:?}", Some(guard.read_filter())),
        };
        let (index, readable) = self.identities(&scope_key, scope)?;
        Ok(self.retained_lookup(scope_key, index, readable.filter))
    }
}

impl Drop for NoteStoreSnapshot<'_> {
    fn drop(&mut self) {
        let Some(database) = self.database.take() else {
            return;
        };
        // Every lookup borrowed the snapshot, so none outlives it.
        if let Ok(database) = Rc::try_unwrap(database) {
            self.session.give_back(Pinned {
                database,
                file: self.file,
                pin: Some((self.epoch, self.clock.clone())),
            });
        }
    }
}

#[cfg(test)]
mod tests;
