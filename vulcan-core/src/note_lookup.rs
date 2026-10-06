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
use std::collections::{HashMap, HashSet};
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
}

impl<S: BuildHasher> NoteLookup for HashMap<String, NoteRecord, S> {
    fn note(&self, key: &str) -> Option<&NoteRecord> {
        self.get(key)
    }

    fn notes(&self) -> Box<dyn Iterator<Item = &NoteRecord> + '_> {
        Box::new(self.values())
    }
}

/// Hydrates notes for a [`LazyNoteLookup`]; receives stored-field copies and
/// returns them hydrated, in any order.
pub type NoteHydrator<'a> =
    Box<dyn Fn(Vec<NoteRecord>) -> Result<Vec<NoteRecord>, PropertyError> + 'a>;

/// Distinct notes hydrated one at a time before the rest are hydrated in
/// one batch; bounds a query that dereferences many notes to the cost of
/// eager hydration.
const LAZY_HYDRATION_LIMIT: usize = 32;

/// A lookup over stored-field notes, some already hydrated, that hydrates
/// any other note the first time an expression dereferences it.
pub struct LazyNoteLookup<'a> {
    notes: &'a HashMap<String, NoteRecord>,
    hydrated_paths: &'a HashSet<String>,
    cells: HashMap<&'a str, OnceCell<NoteRecord>>,
    hydrator: NoteHydrator<'a>,
    misses: Cell<usize>,
    error: RefCell<Option<PropertyError>>,
}

impl<'a> LazyNoteLookup<'a> {
    /// `notes` whose entries at `hydrated_paths` are already hydrated.
    #[must_use]
    pub fn new(
        notes: &'a HashMap<String, NoteRecord>,
        hydrated_paths: &'a HashSet<String>,
        hydrator: NoteHydrator<'a>,
    ) -> Self {
        let cells = notes
            .values()
            .filter(|note| !hydrated_paths.contains(&note.document_path))
            .map(|note| (note.document_path.as_str(), OnceCell::new()))
            .collect();
        Self {
            notes,
            hydrated_paths,
            cells,
            hydrator,
            misses: Cell::new(0),
            error: RefCell::new(None),
        }
    }

    /// The first hydration failure, if any. Callers fail the evaluation:
    /// a note seen without its file object would be a wrong answer.
    pub fn take_error(&self) -> Option<PropertyError> {
        self.error.borrow_mut().take()
    }

    fn hydrate(&self, path: &str) {
        let misses = self.misses.get() + 1;
        self.misses.set(misses);
        let batch = if misses > LAZY_HYDRATION_LIMIT {
            self.notes
                .values()
                .filter(|note| {
                    self.cells
                        .get(note.document_path.as_str())
                        .is_some_and(|cell| cell.get().is_none())
                })
                .cloned()
                .collect::<Vec<_>>()
        } else {
            self.notes
                .values()
                .filter(|note| note.document_path == path)
                .cloned()
                .collect()
        };
        match (self.hydrator)(batch) {
            Ok(hydrated) => {
                for note in hydrated {
                    if let Some(cell) = self.cells.get(note.document_path.as_str()) {
                        let _ = cell.set(note);
                    }
                }
            }
            Err(error) => {
                self.error.borrow_mut().get_or_insert(error);
            }
        }
    }
}

impl NoteLookup for LazyNoteLookup<'_> {
    fn note(&self, key: &str) -> Option<&NoteRecord> {
        self.notes.get(key)
    }

    fn notes(&self) -> Box<dyn Iterator<Item = &NoteRecord> + '_> {
        Box::new(self.notes.values())
    }

    fn hydrated<'b>(&'b self, note: &'b NoteRecord) -> Cow<'b, NoteRecord> {
        if self.hydrated_paths.contains(&note.document_path) {
            return Cow::Borrowed(note);
        }
        let Some(cell) = self.cells.get(note.document_path.as_str()) else {
            // Not part of this universe (for example an overlay row).
            return Cow::Borrowed(note);
        };
        if cell.get().is_none() {
            self.hydrate(&note.document_path);
        }
        cell.get().map_or(Cow::Borrowed(note), Cow::Borrowed)
    }
}

#[cfg(test)]
mod tests;
