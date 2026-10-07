//! The notes an expression can reach by link (QRY.3,
//! `docs/specs/query-architecture.md` §4.2).
//!
//! Evaluation resolves links by path, basename, and alias over stored
//! fields, and reads a linked note's file object (tags, links, inlinks,
//! tasks, lists) only when an expression dereferences it. The eager map
//! implements [`NoteLookup`] with every note hydrated; [`LazyNoteLookup`]
//! hydrates dereferenced notes on demand.

use crate::properties::{NoteRecord, PropertyError};
use std::borrow::Cow;
use std::cell::{Cell, OnceCell, RefCell};
use std::collections::HashMap;
use std::hash::BuildHasher;
use std::sync::Arc;

/// Link-reachable notes for expression evaluation.
pub trait NoteLookup {
    /// A note by lookup key (unique basename, or `/path` for duplicates),
    /// with at least its stored fields and aliases.
    fn note(&self, key: &str) -> Option<&NoteRecord>;

    /// Every note, with at least stored fields and aliases.
    fn notes(&self) -> Box<dyn Iterator<Item = &NoteRecord> + '_>;

    /// `note` with its file-object fields hydrated.
    fn hydrated<'a>(&'a self, note: &'a NoteRecord) -> Cow<'a, NoteRecord> {
        Cow::Borrowed(note)
    }

    /// Every readable note's path.
    fn paths(&self) -> Box<dyn Iterator<Item = &str> + '_> {
        Box::new(self.notes().map(|note| note.document_path.as_str()))
    }

    /// The note at `path`, if readable.
    fn note_at(&self, path: &str) -> Option<&NoteRecord> {
        self.notes().find(|note| note.document_path == path)
    }

    /// The note a link `target` written in `source_path` names: an exact path,
    /// then a lookup key, then the nearest basename or alias match.
    fn resolve(&self, source_path: &str, target: &str) -> Option<&NoteRecord> {
        let path = resolve_identity(
            || {
                self.notes().map(|note| NoteIdentity {
                    path: &note.document_path,
                    file_name: &note.file_name,
                    aliases: &note.aliases,
                })
            },
            |key| self.note(key).map(|note| note.document_path.as_str()),
            source_path,
            target,
        )?;
        self.note_at(path)
    }
}

/// What link resolution reads about a note: identity facts only, never
/// hydrated fields.
#[derive(Clone, Copy)]
pub struct NoteIdentity<'a> {
    pub path: &'a str,
    pub file_name: &'a str,
    pub aliases: &'a [String],
}

/// The path a link `target` written in `source_path` resolves to among
/// `identities`: an exact path (with or without `.md`), then a lookup key,
/// then the basename or alias match nearest to the source folder (basenames
/// before aliases, then folder distance, then path).
pub fn resolve_identity<'a, I: Iterator<Item = NoteIdentity<'a>>>(
    identities: impl Fn() -> I,
    by_key: impl Fn(&str) -> Option<&'a str>,
    source_path: &str,
    target: &str,
) -> Option<&'a str> {
    let target = target.trim();
    let target_no_ext = target.trim_end_matches(".md");
    let target_basename = target_no_ext.rsplit('/').next().unwrap_or(target_no_ext);

    if let Some(identity) = identities()
        .find(|identity| identity.path == target)
        .or_else(|| {
            identities().find(|identity| identity.path.trim_end_matches(".md") == target_no_ext)
        })
    {
        return Some(identity.path);
    }
    if let Some(path) = by_key(target_no_ext) {
        return Some(path);
    }

    let source_folder = source_path
        .rsplit_once('/')
        .map_or("", |(folder, _)| folder);
    identities()
        .filter_map(|identity| {
            // Path-qualified targets may match a folder suffix, but never an
            // unrelated note that merely shares the basename.
            let basename_matches = identity.file_name == target_basename
                && (!target_no_ext.contains('/')
                    || identity
                        .path
                        .trim_end_matches(".md")
                        .ends_with(&format!("/{target_no_ext}")));
            let rank = if basename_matches {
                0_usize
            } else if identity
                .aliases
                .iter()
                .any(|alias| alias == target || alias == target_basename)
            {
                1
            } else {
                return None;
            };
            Some((
                rank,
                folder_distance(source_folder, identity.path),
                identity.path,
            ))
        })
        .min()
        .map(|(_, _, path)| path)
}

fn folder_distance(source_folder: &str, note_path: &str) -> usize {
    let note_folder = note_path.rsplit_once('/').map_or("", |(folder, _)| folder);
    let source_parts: Vec<&str> = source_folder
        .split('/')
        .filter(|part| !part.is_empty())
        .collect();
    let note_parts: Vec<&str> = note_folder
        .split('/')
        .filter(|part| !part.is_empty())
        .collect();
    let common_prefix = source_parts
        .iter()
        .zip(&note_parts)
        .take_while(|(left, right)| left == right)
        .count();
    source_parts.len() + note_parts.len() - (common_prefix * 2)
}

impl<S: BuildHasher> NoteLookup for HashMap<String, NoteRecord, S> {
    fn note(&self, key: &str) -> Option<&NoteRecord> {
        self.get(key)
    }

    fn notes(&self) -> Box<dyn Iterator<Item = &NoteRecord> + '_> {
        Box::new(self.values())
    }
}

/// Loads stored-field records for the given paths (`None`: every readable
/// note, in one scan), in any order.
pub type StoredNoteLoader<'a> =
    Box<dyn Fn(Option<&[&str]>) -> Result<Vec<Arc<NoteRecord>>, PropertyError> + 'a>;

/// Hydrates stored-field records (tags, links, inlinks, tasks, lists),
/// returning them in any order.
pub type NoteHydrator<'a> =
    Box<dyn Fn(Vec<Arc<NoteRecord>>) -> Result<Vec<Arc<NoteRecord>>, PropertyError> + 'a>;

/// Distinct notes loaded one at a time before the rest load in one batch;
/// bounds a query that touches many notes to the cost of an eager load.
const LAZY_LOAD_LIMIT: usize = 32;

/// One readable note's identity facts and lookup key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedIdentity {
    pub path: String,
    /// Unique basename, or `/path` when basenames collide.
    pub key: String,
    pub file_name: String,
    pub aliases: Vec<String>,
    /// The `note_query` row version (schema v28); 0 when unknown.
    pub row_version: i64,
    /// The cache document id; empty when unknown.
    pub document_id: String,
}

/// Give each identity its lookup key: its basename when no other identity
/// shares it, `/path` otherwise.
pub fn assign_lookup_keys(identities: &mut [IndexedIdentity]) {
    let mut counts = HashMap::<String, usize>::new();
    for identity in identities.iter() {
        *counts.entry(identity.file_name.clone()).or_default() += 1;
    }
    for identity in identities.iter_mut() {
        identity.key = if counts[&identity.file_name] > 1 {
            format!("/{}", identity.path)
        } else {
            identity.file_name.clone()
        };
    }
}

/// A readable universe's identity facts, indexed by path and lookup key.
/// Immutable, so hosts can share one across requests (QRY.6).
#[derive(Debug, Default)]
pub struct IdentityIndex {
    identities: Vec<IndexedIdentity>,
    by_path: HashMap<String, usize>,
    by_key: HashMap<String, usize>,
}

impl IdentityIndex {
    #[must_use]
    pub fn new(identities: Vec<IndexedIdentity>) -> Self {
        let by_path = identities
            .iter()
            .enumerate()
            .map(|(index, identity)| (identity.path.clone(), index))
            .collect();
        let by_key = identities
            .iter()
            .enumerate()
            .map(|(index, identity)| (identity.key.clone(), index))
            .collect();
        Self {
            identities,
            by_path,
            by_key,
        }
    }

    /// The identity of the note at `path`, if readable.
    #[must_use]
    pub fn get(&self, path: &str) -> Option<&IndexedIdentity> {
        self.by_path.get(path).map(|index| &self.identities[*index])
    }

    /// The position of the note at `path` in path order, if readable.
    #[must_use]
    pub fn position(&self, path: &str) -> Option<usize> {
        self.by_path.get(path).copied()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.identities.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.identities.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &IndexedIdentity> {
        self.identities.iter()
    }

    /// The identities in path order.
    #[must_use]
    pub fn identities(&self) -> &[IndexedIdentity] {
        &self.identities
    }
}

/// A note lookup over identity facts (QRY.4): links resolve without loading
/// any note, stored fields load for the notes a query prefetches or reads,
/// and file objects hydrate only for the notes it dereferences. The
/// identities are the caller's readable universe. Records are shared, so a
/// host may hand out retained ones.
pub struct IndexedNoteLookup<'a> {
    index: Arc<IdentityIndex>,
    stored: Vec<OnceCell<Arc<NoteRecord>>>,
    hydrated: Vec<OnceCell<Arc<NoteRecord>>>,
    load_stored: StoredNoteLoader<'a>,
    hydrate: NoteHydrator<'a>,
    stored_misses: Cell<usize>,
    hydrated_misses: Cell<usize>,
    error: RefCell<Option<PropertyError>>,
    database: Option<std::rc::Rc<crate::CacheDatabase>>,
    retains_records: bool,
    /// Every universe note's stored record in identity order, when a host
    /// supplied them up front.
    all_stored: Option<Arc<[Arc<NoteRecord>]>>,
}

impl<'a> IndexedNoteLookup<'a> {
    /// Share the cache connection the loaders use with planner queries.
    #[must_use]
    pub fn with_database(mut self, database: std::rc::Rc<crate::CacheDatabase>) -> Self {
        self.database = Some(database);
        self
    }

    /// The cache connection the loaders use, if shared.
    pub fn database(&self) -> Option<&crate::CacheDatabase> {
        self.database.as_deref()
    }

    #[must_use]
    pub fn new(
        identities: Vec<IndexedIdentity>,
        load_stored: StoredNoteLoader<'a>,
        hydrate: NoteHydrator<'a>,
    ) -> Self {
        Self::from_index(
            Arc::new(IdentityIndex::new(identities)),
            load_stored,
            hydrate,
        )
    }

    /// A lookup over a shared identity index.
    #[must_use]
    pub fn from_index(
        index: Arc<IdentityIndex>,
        load_stored: StoredNoteLoader<'a>,
        hydrate: NoteHydrator<'a>,
    ) -> Self {
        Self {
            stored: index.identities.iter().map(|_| OnceCell::new()).collect(),
            hydrated: index.identities.iter().map(|_| OnceCell::new()).collect(),
            index,
            load_stored,
            hydrate,
            stored_misses: Cell::new(0),
            hydrated_misses: Cell::new(0),
            error: RefCell::new(None),
            database: None,
            retains_records: false,
            all_stored: None,
        }
    }

    /// A lookup whose stored records are already loaded: `records` holds
    /// every identity's record in identity order. Hydration still loads on
    /// demand; the stored loader serves nothing the records cover.
    #[must_use]
    pub fn from_index_with_records(
        index: Arc<IdentityIndex>,
        records: Arc<[Arc<NoteRecord>]>,
        load_stored: StoredNoteLoader<'a>,
        hydrate: NoteHydrator<'a>,
    ) -> Self {
        debug_assert_eq!(index.len(), records.len());
        let mut lookup = Self::from_index(index, load_stored, hydrate);
        for (cell, record) in lookup.stored.iter().zip(records.iter()) {
            let _ = cell.set(Arc::clone(record));
        }
        lookup.all_stored = Some(records);
        lookup.retains_records = true;
        lookup
    }

    /// Every universe note's stored record in identity (path) order, when
    /// supplied up front.
    #[must_use]
    pub fn all_stored(&self) -> Option<&[Arc<NoteRecord>]> {
        self.all_stored.as_deref()
    }

    /// Mark the loaders as serving retained records, so loading the stored
    /// fields of every candidate is cheaper than reading their facts in SQL.
    #[must_use]
    pub fn with_retained_records(mut self) -> Self {
        self.retains_records = true;
        self
    }

    /// Whether the loaders serve retained records.
    #[must_use]
    pub fn retains_records(&self) -> bool {
        self.retains_records
    }

    /// The identity facts of the universe.
    #[must_use]
    pub fn identity_index(&self) -> &Arc<IdentityIndex> {
        &self.index
    }

    /// The first load failure, if any. Callers fail the evaluation: a note
    /// read without its fields would be a wrong answer.
    pub fn take_error(&self) -> Option<PropertyError> {
        self.error.borrow_mut().take()
    }

    fn record_error(&self, error: PropertyError) {
        self.error.borrow_mut().get_or_insert(error);
    }

    fn indexes<'b>(&self, paths: impl IntoIterator<Item = &'b str>) -> Vec<usize> {
        paths
            .into_iter()
            .filter_map(|path| self.index.by_path.get(path).copied())
            .collect()
    }

    /// Load the stored fields of the notes at `paths` in one batch.
    pub fn prefetch_stored<'b>(&self, paths: impl IntoIterator<Item = &'b str>) {
        let missing = self
            .indexes(paths)
            .into_iter()
            .filter(|index| self.stored[*index].get().is_none())
            .collect::<Vec<_>>();
        self.load_stored_batch(&missing);
    }

    /// Load and hydrate the notes at `paths` in one batch.
    pub fn prefetch_hydrated<'b>(&self, paths: impl IntoIterator<Item = &'b str>) {
        let missing = self
            .indexes(paths)
            .into_iter()
            .filter(|index| self.hydrated[*index].get().is_none())
            .collect::<Vec<_>>();
        self.hydrate_batch(&missing);
    }

    fn load_stored_batch(&self, indexes: &[usize]) {
        if indexes.is_empty() {
            return;
        }
        let paths = indexes
            .iter()
            .map(|index| self.index.identities[*index].path.as_str())
            .collect::<Vec<_>>();
        // Loading most of the universe scans it instead of listing paths.
        let wanted = (paths.len() * 2 < self.index.identities.len()).then_some(paths.as_slice());
        match (self.load_stored)(wanted) {
            Ok(records) => {
                for mut record in records {
                    if let Some(index) = self.index.by_path.get(&record.document_path) {
                        if self.stored[*index].get().is_some() {
                            continue;
                        }
                        let aliases = &self.index.identities[*index].aliases;
                        if record.aliases != *aliases {
                            Arc::make_mut(&mut record).aliases.clone_from(aliases);
                        }
                        let _ = self.stored[*index].set(record);
                    }
                }
            }
            Err(error) => self.record_error(error),
        }
    }

    fn hydrate_batch(&self, indexes: &[usize]) {
        if indexes.is_empty() {
            return;
        }
        let unloaded = indexes
            .iter()
            .copied()
            .filter(|index| self.stored[*index].get().is_none())
            .collect::<Vec<_>>();
        self.load_stored_batch(&unloaded);
        let records = indexes
            .iter()
            .filter_map(|index| self.stored[*index].get().cloned())
            .collect::<Vec<_>>();
        match (self.hydrate)(records) {
            Ok(records) => {
                for record in records {
                    if let Some(index) = self.index.by_path.get(&record.document_path) {
                        let _ = self.hydrated[*index].set(record);
                    }
                }
            }
            Err(error) => self.record_error(error),
        }
    }

    /// Load on a miss: one note, or after [`LAZY_LOAD_LIMIT`] misses every
    /// note still missing.
    fn on_miss(
        index: usize,
        misses: &Cell<usize>,
        cells: &[OnceCell<Arc<NoteRecord>>],
        load: impl Fn(&[usize]),
    ) {
        misses.set(misses.get() + 1);
        if misses.get() > LAZY_LOAD_LIMIT {
            let rest = (0..cells.len())
                .filter(|index| cells[*index].get().is_none())
                .collect::<Vec<_>>();
            load(&rest);
        } else {
            load(&[index]);
        }
    }

    fn stored_at(&self, index: usize) -> Option<&NoteRecord> {
        if self.stored[index].get().is_none() {
            Self::on_miss(index, &self.stored_misses, &self.stored, |indexes| {
                self.load_stored_batch(indexes);
            });
        }
        self.stored[index].get().map(AsRef::as_ref)
    }

    /// The note at `path` with its file object hydrated.
    pub fn hydrated_at(&self, path: &str) -> Option<&NoteRecord> {
        let index = *self.index.by_path.get(path)?;
        if self.hydrated[index].get().is_none() {
            Self::on_miss(index, &self.hydrated_misses, &self.hydrated, |indexes| {
                self.hydrate_batch(indexes);
            });
        }
        self.hydrated[index].get().map(AsRef::as_ref)
    }

    /// The shared stored-field record at `path`, loading it on a miss.
    pub fn note_arc_at(&self, path: &str) -> Option<Arc<NoteRecord>> {
        let index = *self.index.by_path.get(path)?;
        self.stored_at(index)?;
        self.stored[index].get().cloned()
    }

    /// The shared hydrated record at `path`, hydrating it on a miss.
    pub fn hydrated_arc_at(&self, path: &str) -> Option<Arc<NoteRecord>> {
        self.hydrated_at(path)?;
        self.hydrated[*self.index.by_path.get(path)?].get().cloned()
    }

    /// Every readable note keyed like `build_note_lookup_index`: hydrated
    /// where it was hydrated, stored fields otherwise. Loads any note not yet
    /// loaded.
    pub fn into_index(self) -> Result<HashMap<String, NoteRecord>, PropertyError> {
        let missing = (0..self.index.identities.len())
            .filter(|index| {
                self.stored[*index].get().is_none() && self.hydrated[*index].get().is_none()
            })
            .collect::<Vec<_>>();
        self.load_stored_batch(&missing);
        if let Some(error) = self.take_error() {
            return Err(error);
        }
        Ok(self
            .index
            .identities
            .iter()
            .zip(self.stored.into_iter().zip(self.hydrated))
            .filter_map(|(identity, (stored, hydrated))| {
                hydrated
                    .into_inner()
                    .or_else(|| stored.into_inner())
                    .map(|note| {
                        (
                            identity.key.clone(),
                            Arc::try_unwrap(note).unwrap_or_else(|shared| (*shared).clone()),
                        )
                    })
            })
            .collect())
    }

    /// Whether `path` is in the readable universe; loads nothing.
    pub fn contains(&self, path: &str) -> bool {
        self.index.by_path.contains_key(path)
    }

    /// The path a link `target` written in `source_path` names, from
    /// identity facts alone; see [`resolve_identity`].
    pub fn resolve_path(&self, source_path: &str, target: &str) -> Option<&str> {
        resolve_identity(
            || {
                self.index.identities.iter().map(|identity| NoteIdentity {
                    path: &identity.path,
                    file_name: &identity.file_name,
                    aliases: &identity.aliases,
                })
            },
            |key| {
                self.index
                    .by_key
                    .get(key)
                    .map(|index| self.index.identities[*index].path.as_str())
            },
            source_path,
            target,
        )
    }

    /// Whether the note at `path` has been hydrated.
    pub fn is_hydrated(&self, path: &str) -> bool {
        self.index
            .by_path
            .get(path)
            .is_some_and(|index| self.hydrated[*index].get().is_some())
    }
}

impl NoteLookup for IndexedNoteLookup<'_> {
    fn note(&self, key: &str) -> Option<&NoteRecord> {
        self.stored_at(*self.index.by_key.get(key)?)
    }

    fn notes(&self) -> Box<dyn Iterator<Item = &NoteRecord> + '_> {
        let missing = (0..self.index.identities.len())
            .filter(|index| self.stored[*index].get().is_none())
            .collect::<Vec<_>>();
        self.load_stored_batch(&missing);
        Box::new(
            self.stored
                .iter()
                .filter_map(OnceCell::get)
                .map(AsRef::as_ref),
        )
    }

    fn hydrated<'b>(&'b self, note: &'b NoteRecord) -> Cow<'b, NoteRecord> {
        self.hydrated_at(&note.document_path)
            .map_or(Cow::Borrowed(note), Cow::Borrowed)
    }

    fn paths(&self) -> Box<dyn Iterator<Item = &str> + '_> {
        Box::new(
            self.index
                .identities
                .iter()
                .map(|identity| identity.path.as_str()),
        )
    }

    fn note_at(&self, path: &str) -> Option<&NoteRecord> {
        self.stored_at(*self.index.by_path.get(path)?)
    }

    fn resolve(&self, source_path: &str, target: &str) -> Option<&NoteRecord> {
        self.note_at(self.resolve_path(source_path, target)?)
    }
}

#[cfg(test)]
mod tests;
