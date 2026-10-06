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

/// Loads stored-field records for the given paths, in any order.
pub type StoredNoteLoader<'a> = Box<dyn Fn(&[&str]) -> Result<Vec<NoteRecord>, PropertyError> + 'a>;

/// Hydrates stored-field records (tags, links, inlinks, tasks, lists),
/// returning them in any order.
pub type NoteHydrator<'a> =
    Box<dyn Fn(Vec<NoteRecord>) -> Result<Vec<NoteRecord>, PropertyError> + 'a>;

/// Distinct notes loaded one at a time before the rest load in one batch;
/// bounds a query that touches many notes to the cost of an eager load.
const LAZY_LOAD_LIMIT: usize = 32;

/// One readable note's identity facts and lookup key.
pub struct IndexedIdentity {
    pub path: String,
    /// Unique basename, or `/path` when basenames collide.
    pub key: String,
    pub file_name: String,
    pub aliases: Vec<String>,
}

/// A note lookup over identity facts (QRY.4): links resolve without loading
/// any note, stored fields load for the notes a query prefetches or reads,
/// and file objects hydrate only for the notes it dereferences. The
/// identities are the caller's readable universe.
pub struct IndexedNoteLookup<'a> {
    identities: Vec<IndexedIdentity>,
    by_path: HashMap<String, usize>,
    by_key: HashMap<String, usize>,
    stored: Vec<OnceCell<NoteRecord>>,
    hydrated: Vec<OnceCell<NoteRecord>>,
    load_stored: StoredNoteLoader<'a>,
    hydrate: NoteHydrator<'a>,
    stored_misses: Cell<usize>,
    hydrated_misses: Cell<usize>,
    error: RefCell<Option<PropertyError>>,
}

impl<'a> IndexedNoteLookup<'a> {
    #[must_use]
    pub fn new(
        identities: Vec<IndexedIdentity>,
        load_stored: StoredNoteLoader<'a>,
        hydrate: NoteHydrator<'a>,
    ) -> Self {
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
            stored: identities.iter().map(|_| OnceCell::new()).collect(),
            hydrated: identities.iter().map(|_| OnceCell::new()).collect(),
            identities,
            by_path,
            by_key,
            load_stored,
            hydrate,
            stored_misses: Cell::new(0),
            hydrated_misses: Cell::new(0),
            error: RefCell::new(None),
        }
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
            .filter_map(|path| self.by_path.get(path).copied())
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
            .map(|index| self.identities[*index].path.as_str())
            .collect::<Vec<_>>();
        match (self.load_stored)(&paths) {
            Ok(records) => {
                for mut record in records {
                    if let Some(index) = self.by_path.get(&record.document_path) {
                        record.aliases.clone_from(&self.identities[*index].aliases);
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
                    if let Some(index) = self.by_path.get(&record.document_path) {
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
        cells: &[OnceCell<NoteRecord>],
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
        self.stored[index].get()
    }

    /// The note at `path` with its file object hydrated.
    pub fn hydrated_at(&self, path: &str) -> Option<&NoteRecord> {
        let index = *self.by_path.get(path)?;
        if self.hydrated[index].get().is_none() {
            Self::on_miss(index, &self.hydrated_misses, &self.hydrated, |indexes| {
                self.hydrate_batch(indexes);
            });
        }
        self.hydrated[index].get()
    }

    /// Whether the note at `path` has been hydrated.
    pub fn is_hydrated(&self, path: &str) -> bool {
        self.by_path
            .get(path)
            .is_some_and(|index| self.hydrated[*index].get().is_some())
    }
}

impl NoteLookup for IndexedNoteLookup<'_> {
    fn note(&self, key: &str) -> Option<&NoteRecord> {
        self.stored_at(*self.by_key.get(key)?)
    }

    fn notes(&self) -> Box<dyn Iterator<Item = &NoteRecord> + '_> {
        let all = (0..self.identities.len()).collect::<Vec<_>>();
        let missing = all
            .iter()
            .copied()
            .filter(|index| self.stored[*index].get().is_none())
            .collect::<Vec<_>>();
        self.load_stored_batch(&missing);
        Box::new(self.stored.iter().filter_map(OnceCell::get))
    }

    fn hydrated<'b>(&'b self, note: &'b NoteRecord) -> Cow<'b, NoteRecord> {
        self.hydrated_at(&note.document_path)
            .map_or(Cow::Borrowed(note), Cow::Borrowed)
    }

    fn paths(&self) -> Box<dyn Iterator<Item = &str> + '_> {
        Box::new(
            self.identities
                .iter()
                .map(|identity| identity.path.as_str()),
        )
    }

    fn note_at(&self, path: &str) -> Option<&NoteRecord> {
        self.stored_at(*self.by_path.get(path)?)
    }

    fn resolve(&self, source_path: &str, target: &str) -> Option<&NoteRecord> {
        let path = resolve_identity(
            || {
                self.identities.iter().map(|identity| NoteIdentity {
                    path: &identity.path,
                    file_name: &identity.file_name,
                    aliases: &identity.aliases,
                })
            },
            |key| {
                self.by_key
                    .get(key)
                    .map(|index| self.identities[*index].path.as_str())
            },
            source_path,
            target,
        )?;
        self.note_at(path)
    }
}

#[cfg(test)]
mod tests;
