//! A long-lived host's retained note store (QRY.6,
//! `docs/specs/query-architecture.md` §4.6).
//!
//! A [`NoteStoreSession`] hands out [`NoteStoreSnapshot`]s: pooled read-only
//! cache connections, each holding one read transaction for one request.
//! Every write section commits what a note query can observe in a single
//! transaction that also advances the store clock (`note_store_clock`,
//! schema v28): a scan's rows, link resolution, and durable link decisions
//! together, and an accepted suggestion's inferred link with its own clock
//! advance. So any read transaction sees the cache between complete writes,
//! and its clock names that state. Readers never take or wait for the vault
//! lock, and a write is visible to every request that begins after it
//! commits. Only an interrupted ordinary write batch, found while no writer
//! holds the lock, makes [`NoteStoreSession::snapshot`] return `None`, so the
//! direct path can report the recovery it needs.
//!
//! Inside a snapshot the session reuses what earlier snapshots loaded, bound
//! to the versions that describe it:
//!
//! - identity facts per read scope while the store clock is unchanged;
//! - stored-field records per path while the row's `row_version` is the
//!   identity's and the bookmarks are the same;
//! - hydrated file objects per scope while the clock, configuration, and
//!   bookmark set are unchanged, since incoming links, tasks, and lists
//!   depend on rows other than the note's own.
//!
//! Scopes with a policy hook, or restricted to an explicit universe, are not
//! retained; they still read the snapshot's transaction.

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

/// Idle read-only connections kept for concurrent requests.
const IDLE_CONNECTIONS: usize = 8;
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

/// A pooled connection and the cache file it opened, as (device, inode).
struct Pooled {
    database: CacheDatabase,
    file: Option<(u64, u64)>,
}

/// Stored-field records retained across snapshots.
#[derive(Default)]
struct RetainedStored {
    store_id: String,
    bookmarks: HashSet<String>,
    /// Path to the row version the record describes, and the record.
    notes: HashMap<String, (i64, Arc<NoteRecord>)>,
}

/// A scope's stored records in identity order, with the bookmark set they
/// were loaded under.
type ScopeRecords = (Arc<HashSet<String>>, Arc<[Arc<NoteRecord>]>);

/// One read scope's identities at one clock, and its stored records in
/// identity order once a request needed all of them.
struct RetainedScope {
    clock: Clock,
    index: Arc<IdentityIndex>,
    /// With the bookmark set the records were loaded under.
    records: Mutex<Option<ScopeRecords>>,
}

/// Hydrated records of one scope, valid for one key.
struct RetainedHydrated {
    clock: Clock,
    config: Arc<VaultConfig>,
    bookmarks: Arc<HashSet<String>>,
    notes: HashMap<String, Arc<NoteRecord>>,
}

/// Work counters since the session began, for diagnostics and tests.
#[derive(Debug, Default)]
pub struct NoteSessionCounters {
    /// Snapshots handed out.
    pub snapshots: AtomicU64,
    /// Requests that found an interrupted write and took the direct path.
    pub snapshots_unavailable: AtomicU64,
    pub connections_opened: AtomicU64,
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
    idle: Mutex<Vec<Pooled>>,
    identities: Mutex<HashMap<String, Arc<RetainedScope>>>,
    /// Per-scope locks so one request loads a scope's identities while
    /// concurrent requests at the same clock wait for them.
    loaders: Mutex<HashMap<String, Arc<Mutex<()>>>>,
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
            loaders: Mutex::new(HashMap::new()),
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

    /// A read transaction over the current cache, or `None` when there is no
    /// cache, it cannot be read, or an interrupted ordinary write batch
    /// needs recovery. Callers then use the direct path, which reports it.
    #[must_use]
    pub fn snapshot(&self) -> Option<NoteStoreSnapshot<'_>> {
        let snapshot = self.begin();
        if snapshot.is_none() {
            count(&self.counters.snapshots_unavailable, 1);
        }
        snapshot
    }

    fn begin(&self) -> Option<NoteStoreSnapshot<'_>> {
        if !self.paths.cache_db().exists() {
            return None;
        }
        // A batch journal while a writer holds the lock is that write in
        // progress; without a writer it is an interrupted write.
        if let Ok(Some(_guard)) = crate::write_lock::try_acquire_read_lock(&self.paths) {
            crate::ordinary_write::ensure_no_pending_ordinary_write_batch(&self.paths).ok()?;
        }
        let file = cache_file_identity(&self.paths);
        let reused = {
            let mut idle = lock(&self.idle);
            idle.iter()
                .position(|pooled| pooled.file == file)
                .map(|index| idle.swap_remove(index))
        };
        let pooled = if let Some(pooled) = reused {
            pooled
        } else {
            count(&self.counters.connections_opened, 1);
            Pooled {
                database: CacheDatabase::open(&self.paths).ok()?,
                file,
            }
        };
        let connection = pooled.database.connection();
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
        let Some(clock) = clock else {
            let _ = connection.execute_batch("ROLLBACK");
            return None;
        };
        count(&self.counters.snapshots, 1);
        Some(NoteStoreSnapshot {
            session: self,
            database: Some(Rc::new(pooled.database)),
            file: pooled.file,
            clock,
            bookmarks: Arc::new(crate::properties::load_bookmarked_paths(
                self.paths.vault_root(),
            )),
            config: Arc::new(crate::load_vault_config(&self.paths).config),
        })
    }

    fn give_back(&self, pooled: Pooled) {
        let mut idle = lock(&self.idle);
        if idle.len() < IDLE_CONNECTIONS {
            idle.push(pooled);
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

/// One request's view of the note store: a read transaction plus the
/// session's retained notes at matching versions. The connection returns
/// to the session's pool when dropped.
pub struct NoteStoreSnapshot<'s> {
    session: &'s NoteStoreSession,
    database: Option<Rc<CacheDatabase>>,
    file: Option<(u64, u64)>,
    clock: Clock,
    bookmarks: Arc<HashSet<String>>,
    config: Arc<VaultConfig>,
}

impl NoteStoreSnapshot<'_> {
    /// The store clock version the snapshot reads.
    #[must_use]
    pub fn clock_version(&self) -> i64 {
        self.clock.version
    }

    fn database(&self) -> &Rc<CacheDatabase> {
        self.database.as_ref().expect("held until drop")
    }

    /// A retainable scope's identities, reused while the clock matches and
    /// loaded single-flight otherwise.
    fn scope(
        &self,
        scope_key: &str,
        scope: NoteIndexReadScope<'_>,
    ) -> Result<Arc<RetainedScope>, PropertyError> {
        let current = || {
            lock(&self.session.identities)
                .get(scope_key)
                .filter(|retained| retained.clock == self.clock)
                .cloned()
        };
        if let Some(retained) = current() {
            count(&self.session.counters.identity_reuses, 1);
            return Ok(retained);
        }
        let loader = Arc::clone(
            lock(&self.session.loaders)
                .entry(scope_key.to_string())
                .or_default(),
        );
        let _loading = lock(&loader);
        if let Some(retained) = current() {
            count(&self.session.counters.identity_reuses, 1);
            return Ok(retained);
        }
        count(&self.session.counters.identity_loads, 1);
        let readable = load_readable_identities(self.database(), scope, None)?;
        let retained = Arc::new(RetainedScope {
            clock: self.clock.clone(),
            index: Arc::new(IdentityIndex::new(readable.identities)),
            records: Mutex::new(None),
        });
        let mut identities = lock(&self.session.identities);
        if identities.len() >= RETAINED_SCOPES && !identities.contains_key(scope_key) {
            identities.clear();
            lock(&self.session.loaders).retain(|key, _| key == scope_key);
        }
        identities.insert(scope_key.to_string(), Arc::clone(&retained));
        Ok(retained)
    }

    /// Every stored record of `scope` in identity order, built once per
    /// clock and bookmark set from retained records. `None` if a record is
    /// missing, so callers load lazily instead.
    fn scope_records(
        &self,
        scope: &RetainedScope,
    ) -> Result<Option<Arc<[Arc<NoteRecord>]>>, PropertyError> {
        let mut records = lock(&scope.records);
        if let Some((bookmarks, records)) = records.as_ref() {
            if **bookmarks == *self.bookmarks {
                return Ok(Some(Arc::clone(records)));
            }
        }
        let mut by_path = self
            .load_stored_retained(&scope.index, None)?
            .into_iter()
            .map(|record| (record.document_path.clone(), record))
            .collect::<HashMap<_, _>>();
        let ordered = scope
            .index
            .iter()
            .map(|identity| by_path.remove(&identity.path))
            .collect::<Option<Vec<_>>>();
        let Some(ordered) = ordered else {
            return Ok(None);
        };
        let ordered = Arc::<[Arc<NoteRecord>]>::from(ordered);
        *records = Some((Arc::clone(&self.bookmarks), Arc::clone(&ordered)));
        Ok(Some(ordered))
    }

    fn retained_lookup(
        &self,
        scope_key: String,
        scope: &RetainedScope,
        filter: Option<crate::PermissionFilter>,
    ) -> Result<IndexedNoteLookup<'_>, PropertyError> {
        let index = Arc::clone(&scope.index);
        let stored_index = Arc::clone(&index);
        let load_stored =
            move |wanted: Option<&[&str]>| self.load_stored_retained(&stored_index, wanted);
        let hydrate = move |notes: Vec<Arc<NoteRecord>>| {
            self.hydrate_retained(&scope_key, filter.as_ref(), notes)
        };
        let lookup = match self.scope_records(scope)? {
            Some(records) => IndexedNoteLookup::from_index_with_records(
                index,
                records,
                Box::new(load_stored),
                Box::new(hydrate),
            ),
            None => IndexedNoteLookup::from_index(index, Box::new(load_stored), Box::new(hydrate))
                .with_retained_records(),
        };
        Ok(lookup.with_database(Rc::clone(self.database())))
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
            retained.clock == self.clock
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
                clock: self.clock.clone(),
                config: Arc::clone(&self.config),
                bookmarks: Arc::clone(&self.bookmarks),
                notes: HashMap::new(),
            });
        if !is_current(entry) {
            *entry = RetainedHydrated {
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
        let filter = match scope {
            NoteIndexReadScope::Filter(filter) => filter.cloned(),
            NoteIndexReadScope::Guard(guard) => Some(guard.read_filter()),
        };
        let retained = self.scope(&scope_key, scope)?;
        self.retained_lookup(scope_key, &retained, filter)
    }
}

impl Drop for NoteStoreSnapshot<'_> {
    fn drop(&mut self) {
        let Some(database) = self.database.take() else {
            return;
        };
        // Every lookup borrowed the snapshot, so none outlives it.
        if let Ok(database) = Rc::try_unwrap(database) {
            if database.connection().execute_batch("ROLLBACK").is_ok() {
                self.session.give_back(Pooled {
                    database,
                    file: self.file,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests;
