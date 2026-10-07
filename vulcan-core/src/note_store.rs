//! Where note queries read the note store (QRY.6,
//! `docs/specs/query-architecture.md` §4.6).
//!
//! Frontends take their readable universe from a [`NoteStore`]: either
//! [`DirectNoteStore`], which opens the cache for each lookup, or a host's
//! retained snapshot ([`crate::note_session`]), which serves identity facts
//! and notes it already loaded at the same row versions.

use crate::note_lookup::IndexedNoteLookup;
use crate::properties::{NoteIndexReadScope, PropertyError};
use crate::VaultPaths;
use std::collections::HashSet;

/// A source of note lookups.
pub trait NoteStore {
    /// An identity lookup over the readable universe of `scope`, restricted
    /// to `within` when given (an already authorized universe, which also
    /// bounds incoming links).
    fn lookup<'s>(
        &'s self,
        scope: NoteIndexReadScope<'_>,
        within: Option<&HashSet<String>>,
    ) -> Result<IndexedNoteLookup<'s>, PropertyError>;
}

/// Reads the cache directly for every lookup.
#[derive(Clone, Copy)]
pub struct DirectNoteStore<'p> {
    paths: &'p VaultPaths,
}

impl<'p> DirectNoteStore<'p> {
    #[must_use]
    pub fn new(paths: &'p VaultPaths) -> Self {
        Self { paths }
    }
}

impl NoteStore for DirectNoteStore<'_> {
    fn lookup<'s>(
        &'s self,
        scope: NoteIndexReadScope<'_>,
        within: Option<&HashSet<String>>,
    ) -> Result<IndexedNoteLookup<'s>, PropertyError> {
        crate::properties::load_indexed_note_lookup_within(self.paths, scope, within)
    }
}
