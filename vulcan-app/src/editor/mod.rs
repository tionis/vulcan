//! Transport-neutral editor services (Roadmap 9.40,
//! `docs/specs/editor-integration.md`).
//!
//! An [`EditorSession`] holds the documents an editor has open as an
//! in-memory overlay over the cache. Unsaved text is parsed for its own
//! outline, diagnostics, and links, while cross-document facts come from the
//! cache with open buffers substituted. Nothing here writes buffer text to
//! the cache or to disk; protocol adapters such as `vulcan lsp` translate
//! these typed results into their own wire format.

mod actions;
mod commands;
mod completion;
mod diagnostics;
mod edits;
mod navigation;
mod render;

pub use actions::{EditorAction, EditorActionKind, EditorCodeAction};
pub use commands::EditorTemplatePlacement;
pub use completion::{EditorCompletion, EditorCompletionItem, EditorCompletionKind};
pub use diagnostics::{EditorDiagnostic, EditorDiagnosticSeverity};
pub use edits::{
    inverse_changes, EditorDocumentEdit, EditorEditPlan, EditorMove, EditorRenameTarget,
    EditorTextEdit,
};
pub use navigation::{EditorHover, EditorSymbol, EditorWorkspaceSymbol};
pub use render::{EditorRender, EditorRenderRegion};

use crate::AppError;
use std::collections::HashMap;
use std::ops::Range;
use std::sync::{Arc, Mutex};
use vulcan_core::editor::{
    load_vault_identities, store_clock_version, EditorResolver, LineIndex, OverlayIdentity,
    PositionEncoding, TextPosition, VaultIdentities,
};
use vulcan_core::{load_vault_config, parse_document, ParsedDocument, VaultConfig, VaultPaths};

/// A range in one document, in the session's position encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EditorRange {
    pub start: TextPosition,
    pub end: TextPosition,
}

/// A range in a vault document identified by its vault-relative path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorLocation {
    pub path: String,
    pub range: EditorRange,
}

/// One edit to a document's text, as sent by an editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorTextChange {
    /// The replaced range, or `None` to replace the whole text.
    pub range: Option<EditorRange>,
    pub text: String,
}

/// An open document: the editor's text, which may differ from disk.
#[derive(Debug, Clone)]
pub struct OpenDocument {
    pub version: i32,
    pub text: String,
    pub parsed: ParsedDocument,
    pub line_index: LineIndex,
}

impl OpenDocument {
    fn new(version: i32, text: String, config: &VaultConfig) -> Self {
        Self {
            version,
            parsed: parse_document(&text, config),
            line_index: LineIndex::new(&text),
            text,
        }
    }
}

/// A document's text and parse, from an open buffer or from disk.
pub(crate) struct DocumentView {
    pub text: String,
    pub parsed: ParsedDocument,
    pub line_index: LineIndex,
}

/// Cached identities and the resolver built over them and the overlay.
struct ResolverState {
    identities: Arc<VaultIdentities>,
    /// Overlay generation the resolver was built for.
    overlay_generation: u64,
    resolver: Arc<EditorResolver>,
}

/// One vault's open documents and the cache they overlay.
///
/// Cloning is cheap and yields a snapshot: open documents are shared until
/// the next change, and clones share the cached identities and resolver, so
/// a transport can answer requests from worker threads while its own
/// session keeps receiving edits.
#[derive(Clone)]
pub struct EditorSession {
    paths: VaultPaths,
    config: Arc<VaultConfig>,
    encoding: PositionEncoding,
    documents: HashMap<String, Arc<OpenDocument>>,
    /// Advances whenever an open document's identity may have changed.
    overlay_generation: u64,
    resolver: Arc<Mutex<Option<ResolverState>>>,
    /// The permission profile reads and writes run under, if one was chosen.
    permissions: Option<Arc<vulcan_core::ProfilePermissionGuard>>,
}

impl EditorSession {
    #[must_use]
    pub fn new(paths: VaultPaths, encoding: PositionEncoding) -> Self {
        let config = Arc::new(load_vault_config(&paths).config);
        Self {
            paths,
            config,
            encoding,
            documents: HashMap::new(),
            overlay_generation: 0,
            resolver: Arc::new(Mutex::new(None)),
            permissions: None,
        }
    }

    #[must_use]
    pub fn paths(&self) -> &VaultPaths {
        &self.paths
    }

    #[must_use]
    pub fn config(&self) -> &VaultConfig {
        &self.config
    }

    #[must_use]
    pub fn encoding(&self) -> PositionEncoding {
        self.encoding
    }

    /// Run under the named permission profile: rendering reads only what it
    /// grants, and every planned write is checked against it.
    pub fn set_permission_profile(&mut self, profile: &str) -> Result<(), AppError> {
        let selection = vulcan_core::resolve_permission_profile(&self.paths, Some(profile))
            .map_err(|error| AppError::operation_with_code("permission_denied", error))?;
        self.permissions = Some(Arc::new(vulcan_core::ProfilePermissionGuard::new(
            &self.paths,
            selection,
        )));
        Ok(())
    }

    pub(crate) fn guard(&self) -> Option<&vulcan_core::ProfilePermissionGuard> {
        self.permissions.as_deref()
    }

    /// Whether the permission profile (if any) lets this session read
    /// `path`. Documents the editor itself has open are always readable to
    /// it.
    pub(crate) fn readable(&self, path: &str) -> bool {
        use vulcan_core::PermissionGuard as _;
        self.documents.contains_key(path)
            || self
                .guard()
                .is_none_or(|guard| guard.check_read_path(path).is_ok())
    }

    /// The profile's read filter for cache queries, if a profile is set.
    pub(crate) fn read_filter(&self) -> Option<vulcan_core::PermissionFilter> {
        use vulcan_core::PermissionGuard as _;
        self.guard()
            .map(vulcan_core::ProfilePermissionGuard::read_filter)
    }

    pub(crate) fn permission_profile(&self) -> Option<&str> {
        self.permissions
            .as_deref()
            .map(|guard| guard.selection().name.as_str())
    }

    /// Refuse a plan that reads or writes a path the permission profile
    /// does not grant. Call it before showing or applying the plan.
    pub fn authorize_plan(&self, plan: &EditorEditPlan) -> Result<(), AppError> {
        use vulcan_core::PermissionGuard as _;
        let Some(guard) = self.guard() else {
            return Ok(());
        };
        let renamed = plan
            .editor_rename
            .iter()
            .flat_map(|rename| [&rename.from, &rename.to]);
        for path in plan
            .disk
            .iter()
            .map(|change| &change.path)
            .chain(plan.open.iter().map(|document| &document.path))
            .chain(renamed)
        {
            // A plan's reports quote what it read, so reading counts too.
            guard
                .check_read_path(path)
                .and_then(|()| guard.check_write_path(path))
                .map_err(|error| AppError::operation_with_code("permission_denied", error))?;
        }
        Ok(())
    }

    /// Reload the vault configuration and re-parse open documents.
    pub fn reload_config(&mut self) {
        self.config = Arc::new(load_vault_config(&self.paths).config);
        for document in self.documents.values_mut() {
            *document = Arc::new(OpenDocument::new(
                document.version,
                document.text.clone(),
                &self.config,
            ));
        }
        self.overlay_generation += 1;
    }

    /// The vault-relative path of an absolute file path inside the vault.
    #[must_use]
    pub fn relative_path(&self, path: &std::path::Path) -> Option<String> {
        self.paths.resolve_relative_to_vault(path).map(|relative| {
            relative
                .components()
                .map(|component| component.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/")
        })
    }

    #[must_use]
    pub fn document(&self, path: &str) -> Option<&OpenDocument> {
        self.documents.get(path).map(AsRef::as_ref)
    }

    pub fn open_paths(&self) -> impl Iterator<Item = &str> {
        self.documents.keys().map(String::as_str)
    }

    pub fn open(&mut self, path: &str, version: i32, text: String) {
        self.documents.insert(
            path.to_string(),
            Arc::new(OpenDocument::new(version, text, &self.config)),
        );
        self.overlay_generation += 1;
    }

    /// Apply editor changes in order and re-parse the document.
    ///
    /// Returns where the last change's inserted text ends, which is the
    /// cursor position while the user types.
    pub fn change(
        &mut self,
        path: &str,
        version: i32,
        changes: Vec<EditorTextChange>,
    ) -> Result<Option<TextPosition>, AppError> {
        let encoding = self.encoding;
        let document = self.documents.get_mut(path).ok_or_else(|| {
            AppError::operation_with_code("not_found", format!("{path} is not open"))
        })?;
        let mut text = document.text.clone();
        let mut cursor = None;
        for change in changes {
            match change.range {
                None => {
                    text = change.text;
                    cursor = None;
                }
                Some(range) => {
                    let index = LineIndex::new(&text);
                    let start = index.offset(&text, range.start, encoding);
                    let end = index.offset(&text, range.end, encoding).max(start);
                    text.replace_range(start..end, &change.text);
                    cursor = Some(start + change.text.len());
                }
            }
        }
        *document = Arc::new(OpenDocument::new(version, text, &self.config));
        self.overlay_generation += 1;
        Ok(cursor.map(|offset| {
            document
                .line_index
                .position(&document.text, offset, encoding)
        }))
    }

    /// The cache's store clock, which advances with every committed scan;
    /// `None` without a cache.
    #[must_use]
    pub fn store_clock(&self) -> Option<i64> {
        store_clock_version(&self.paths).ok().flatten()
    }

    /// Whether the vault has a cache to resolve links against.
    #[must_use]
    pub fn has_cache(&self) -> bool {
        self.paths.cache_db().exists()
    }

    /// Bring the cache up to date with disk. Returns `None` for a vault
    /// that was never initialized; the session never initializes one.
    pub fn refresh_index(&self) -> Result<Option<vulcan_core::ScanSummary>, AppError> {
        if !self.paths.vulcan_dir().is_dir() {
            return Ok(None);
        }
        let summary = crate::scan::refresh_cache_incrementally(&self.paths)?;
        self.invalidate_cache();
        Ok(Some(summary))
    }

    /// Rescan the vault on request (`full` rebuilds every document),
    /// subject to the permission profile's `index` grant.
    pub fn rescan(&self, full: bool) -> Result<Option<vulcan_core::ScanSummary>, AppError> {
        use vulcan_core::PermissionGuard as _;
        if let Some(guard) = self.guard() {
            guard
                .check_index()
                .map_err(|error| AppError::operation_with_code("permission_denied", error))?;
        }
        if !full {
            return self.refresh_index();
        }
        if !self.paths.vulcan_dir().is_dir() {
            return Ok(None);
        }
        let summary = vulcan_core::scan_vault(&self.paths, vulcan_core::ScanMode::Full)
            .map_err(AppError::operation)?;
        self.invalidate_cache();
        Ok(Some(summary))
    }

    /// Index files the editor just saved, under the vault write lock.
    pub fn scan_saved(
        &self,
        saved: &[String],
    ) -> Result<Option<vulcan_core::ScanSummary>, AppError> {
        self.scan_changed(saved, &HashMap::new())
    }

    /// Index changed vault-relative paths under the vault write lock.
    /// `renames` maps destinations to sources, so a file renamed outside
    /// Vulcan keeps its identity and incoming links.
    pub fn scan_changed(
        &self,
        changed: &[String],
        renames: &HashMap<String, String>,
    ) -> Result<Option<vulcan_core::ScanSummary>, AppError> {
        if !self.has_cache() {
            return Ok(None);
        }
        let mut paths = changed
            .iter()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        for (destination, source) in renames {
            paths.insert(destination.clone());
            paths.insert(source.clone());
        }
        let _lock = vulcan_core::write_lock::acquire_write_lock(&self.paths)
            .map_err(AppError::operation)?;
        let summary =
            vulcan_core::scan::scan_vault_paths_unlocked_with_renames(&self.paths, &paths, renames)
                .map_err(AppError::operation)?;
        self.invalidate_cache();
        Ok(Some(summary))
    }

    pub fn close(&mut self, path: &str) {
        if self.documents.remove(path).is_some() {
            self.overlay_generation += 1;
        }
    }

    /// Forget cached identities so the next request reloads them.
    pub fn invalidate_cache(&self) {
        *lock(&self.resolver) = None;
    }

    /// A resolver over the current cache with open buffers substituted.
    pub(crate) fn resolver(&self) -> Result<Arc<EditorResolver>, AppError> {
        let clock = store_clock_version(&self.paths).map_err(AppError::operation)?;
        let mut state = lock(&self.resolver);
        if let Some(current) = state.as_ref() {
            if Some(current.identities.clock_version) == clock
                && current.overlay_generation == self.overlay_generation
            {
                return Ok(Arc::clone(&current.resolver));
            }
        }
        let identities = match state.as_ref() {
            Some(previous) if Some(previous.identities.clock_version) == clock => {
                Arc::clone(&previous.identities)
            }
            _ => Arc::new(load_vault_identities(&self.paths).map_err(AppError::operation)?),
        };
        let overlays = self
            .documents
            .iter()
            .map(|(path, document)| OverlayIdentity {
                path: path.clone(),
                aliases: document.parsed.aliases.clone(),
            })
            .collect::<Vec<_>>();
        // Under a permission profile, unreadable notes neither resolve nor
        // complete: links to them look unresolved, as for a missing note.
        let readable = match self.guard() {
            Some(_) => identities
                .documents
                .iter()
                .filter(|document| self.readable(&document.path))
                .cloned()
                .collect::<Vec<_>>(),
            None => identities.documents.clone(),
        };
        let resolver = Arc::new(EditorResolver::new(&readable, &overlays));
        // An older snapshot must not replace a newer snapshot's resolver.
        if state
            .as_ref()
            .is_none_or(|current| current.overlay_generation <= self.overlay_generation)
        {
            *state = Some(ResolverState {
                identities,
                overlay_generation: self.overlay_generation,
                resolver: Arc::clone(&resolver),
            });
        }
        Ok(resolver)
    }

    /// A document's current text: the open buffer, else the file on disk.
    pub(crate) fn view(&self, path: &str) -> Option<DocumentView> {
        if let Some(document) = self.documents.get(path) {
            return Some(DocumentView {
                text: document.text.clone(),
                parsed: document.parsed.clone(),
                line_index: document.line_index.clone(),
            });
        }
        if !is_markdown_path(path) || !self.readable(path) {
            return None;
        }
        let text = std::fs::read_to_string(self.paths.vault_root().join(path)).ok()?;
        Some(DocumentView {
            parsed: parse_document(&text, &self.config),
            line_index: LineIndex::new(&text),
            text,
        })
    }

    pub(crate) fn range(&self, text: &str, index: &LineIndex, range: Range<usize>) -> EditorRange {
        EditorRange {
            start: index.position(text, range.start, self.encoding),
            end: index.position(text, range.end, self.encoding),
        }
    }

    pub(crate) fn offset(&self, path: &str, position: TextPosition) -> Option<usize> {
        let document = self.documents.get(path)?;
        Some(
            document
                .line_index
                .offset(&document.text, position, self.encoding),
        )
    }
}

/// File name of the per-vault editor watch lease inside `.vulcan/`.
pub const EDITOR_WATCH_LOCK_FILE: &str = "editor-watch.lock";

/// The per-vault right to watch the file system for editors.
///
/// Several editor processes may serve one vault; only the holder of this
/// advisory OS lock runs a watcher and scans for external changes, and the
/// others follow the store clock. The lock is released when the lease is
/// dropped or the process exits, so a crashed holder never leaves it stale.
#[derive(Debug)]
pub struct EditorWatchLease {
    _file: std::fs::File,
}

impl EditorWatchLease {
    /// Take the lease if no other process holds it. Returns `None` while
    /// another process holds it or the vault is not initialized.
    pub fn try_acquire(paths: &VaultPaths) -> Result<Option<Self>, AppError> {
        use fs2::FileExt;
        if !paths.vulcan_dir().is_dir() {
            return Ok(None);
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(paths.vulcan_dir().join(EDITOR_WATCH_LOCK_FILE))
            .map_err(AppError::operation)?;
        match file.try_lock_exclusive() {
            Ok(()) => Ok(Some(Self { _file: file })),
            Err(error) if error.kind() == fs2::lock_contended_error().kind() => Ok(None),
            Err(error) => Err(AppError::operation(error)),
        }
    }
}

pub(crate) fn is_markdown_path(path: &str) -> bool {
    std::path::Path::new(path)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use vulcan_core::{scan_vault, ScanMode};

    pub(crate) fn vault(files: &[(&str, &str)]) -> (tempfile::TempDir, EditorSession) {
        let dir = tempfile::tempdir().unwrap();
        for (path, text) in files {
            let target = dir.path().join(path);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::write(target, text).unwrap();
        }
        let paths = VaultPaths::new(dir.path());
        vulcan_core::paths::initialize_vulcan_dir(&paths).unwrap();
        scan_vault(&paths, ScanMode::Full).unwrap();
        let session = EditorSession::new(paths, PositionEncoding::Utf16);
        (dir, session)
    }

    /// The position of the first occurrence of `needle` plus `shift` bytes.
    pub(crate) fn position_of(text: &str, needle: &str, shift: usize) -> TextPosition {
        let offset = text.find(needle).unwrap() + shift;
        LineIndex::new(text).position(text, offset, PositionEncoding::Utf16)
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;

    #[test]
    fn applies_incremental_changes_without_touching_the_cache() {
        let (_dir, mut session) = vault(&[("Alpha.md", "# Alpha\n")]);
        let cache = std::fs::read(session.paths().cache_db()).unwrap();
        session.open("Alpha.md", 1, "# Alpha\nbody\n".into());
        let cursor = session
            .change(
                "Alpha.md",
                2,
                vec![EditorTextChange {
                    range: Some(EditorRange {
                        start: TextPosition { line: 1, column: 0 },
                        end: TextPosition { line: 1, column: 4 },
                    }),
                    text: "[[Beta]]".into(),
                }],
            )
            .unwrap();
        assert_eq!(cursor, Some(TextPosition { line: 1, column: 8 }));
        let document = session.document("Alpha.md").unwrap();
        assert_eq!(document.text, "# Alpha\n[[Beta]]\n");
        assert_eq!(document.version, 2);
        assert_eq!(document.parsed.links.len(), 1);
        assert_eq!(std::fs::read(session.paths().cache_db()).unwrap(), cache);
        assert!(session.change("Missing.md", 1, Vec::new()).is_err());
    }

    #[test]
    fn resolver_follows_overlay_and_cache_changes() {
        let (dir, mut session) = vault(&[("Alpha.md", "# Alpha\n")]);
        let link = parse_document("[[Draft]]", session.config())
            .links
            .remove(0);
        let mode = session.config().link_resolution;
        let resolve = |session: &EditorSession| {
            session
                .resolver()
                .unwrap()
                .resolve("Alpha.md", &link, mode)
                .resolved_target_id
        };
        assert_eq!(resolve(&session), None);
        session.open("Draft.md", 1, String::new());
        assert_eq!(resolve(&session).as_deref(), Some("Draft.md"));
        session.close("Draft.md");
        assert_eq!(resolve(&session), None);

        std::fs::write(dir.path().join("Draft.md"), "x").unwrap();
        let summary = session
            .scan_saved(&["Draft.md".to_string()])
            .unwrap()
            .unwrap();
        assert_eq!(summary.added, 1);
        assert_eq!(resolve(&session).as_deref(), Some("Draft.md"));
    }

    #[test]
    fn clones_are_snapshots_of_open_documents() {
        let (_dir, mut session) = vault(&[("Alpha.md", "# Alpha\n")]);
        session.open("Draft.md", 1, "one".into());
        let snapshot = session.clone();
        session
            .change(
                "Draft.md",
                2,
                vec![EditorTextChange {
                    range: None,
                    text: "two".into(),
                }],
            )
            .unwrap();
        session.close("Alpha.md");
        assert_eq!(snapshot.document("Draft.md").unwrap().text, "one");
        assert_eq!(session.document("Draft.md").unwrap().text, "two");
        // Clones share the cached identities but resolve their own overlay.
        let link = parse_document("[[Draft]]", session.config())
            .links
            .remove(0);
        let mode = session.config().link_resolution;
        for view in [&session, &snapshot] {
            assert_eq!(
                view.resolver()
                    .unwrap()
                    .resolve("Alpha.md", &link, mode)
                    .resolved_target_id
                    .as_deref(),
                Some("Draft.md")
            );
        }
        assert!(session.store_clock().is_some());
    }

    #[test]
    fn only_one_process_holds_the_watch_lease() {
        let (_dir, session) = vault(&[("Alpha.md", "x")]);
        let first = EditorWatchLease::try_acquire(session.paths()).unwrap();
        assert!(first.is_some());
        // fs2 locks are per open file description, so a second open in the
        // same process contends exactly like another process would.
        assert!(EditorWatchLease::try_acquire(session.paths())
            .unwrap()
            .is_none());
        drop(first);
        assert!(EditorWatchLease::try_acquire(session.paths())
            .unwrap()
            .is_some());
    }

    #[test]
    fn rename_hints_keep_document_identity() {
        let (dir, session) = vault(&[("Old.md", "# Note\n"), ("Home.md", "[[Old]]\n")]);
        let before = vulcan_core::query_backlinks(session.paths(), "Old.md").unwrap();
        assert_eq!(before.backlinks.len(), 1);
        std::fs::rename(dir.path().join("Old.md"), dir.path().join("New.md")).unwrap();
        let renames = HashMap::from([("New.md".to_string(), "Old.md".to_string())]);
        let summary = session.scan_changed(&[], &renames).unwrap().unwrap();
        assert_eq!(summary.deleted, 0, "{summary:?}");
        assert!(vulcan_core::query_backlinks(session.paths(), "Old.md").is_err());
        assert!(vulcan_core::query_backlinks(session.paths(), "New.md").is_ok());
    }

    #[test]
    fn permission_profiles_limit_writes_and_reads() {
        let (dir, mut session) = vault(&[
            ("Projects/Plan.md", "- [ ] Ship\n"),
            ("Private/Diary.md", "- [ ] Secret\n"),
        ]);
        std::fs::write(
            dir.path().join(".vulcan/config.toml"),
            "[permissions.profiles.projects]\nread = { allow = [\"folder:Projects/**\"] }\nwrite = { allow = [\"folder:Projects/**\"] }\n",
        )
        .unwrap();
        assert!(session.set_permission_profile("missing").is_err());
        session.set_permission_profile("projects").unwrap();

        let allowed = session
            .plan_task_toggle("Projects/Plan.md", 0, "2026-10-13")
            .unwrap();
        session.authorize_plan(&allowed).unwrap();
        let denied = session
            .plan_task_toggle("Private/Diary.md", 0, "2026-10-13")
            .unwrap();
        let error = session.authorize_plan(&denied).unwrap_err();
        assert_eq!(error.code(), Some("permission_denied"));

        // An unreadable note renders like a missing one.
        assert_eq!(
            session
                .render(
                    "Private/Diary.md",
                    None,
                    Some(vulcan_core::DataviewJsRenderPolicy::Off),
                )
                .unwrap(),
            None
        );
    }

    #[test]
    fn permission_profiles_hide_unreadable_notes_from_navigation() {
        let (dir, mut session) = vault(&[
            (
                "Projects/Plan.md",
                "---\nowner: me\n---\n#visible [[Diary]] [[Home]]\n",
            ),
            ("Home.md", "# Home\n"),
            (
                "Private/Diary.md",
                "---\nmood: secret\n---\n# Diary #hidden\n[[Home]]\n",
            ),
        ]);
        std::fs::write(
            dir.path().join(".vulcan/config.toml"),
            "[permissions.profiles.projects]\nread = { allow = [\"folder:Projects/**\", \"note:Home.md\"] }\n",
        )
        .unwrap();
        session.set_permission_profile("projects").unwrap();
        let text = "---\nowner: me\n---\n#visible [[Diary]] [[Home]]\n";
        session.open("Projects/Plan.md", 1, text.into());

        // A link to an unreadable note looks like a link to a missing one.
        let diagnostics = session
            .diagnostics("Projects/Plan.md", None)
            .unwrap()
            .unwrap();
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code, "unresolved-link");
        assert_eq!(
            session
                .definition("Projects/Plan.md", position_of(text, "Diary", 0))
                .unwrap(),
            None
        );
        assert!(session.workspace_symbols("diary", 10).unwrap().is_empty());
        // Backlinks from unreadable notes are not listed.
        let locations = session
            .references("Projects/Plan.md", position_of(text, "Home]]", 0))
            .unwrap();
        assert!(locations
            .iter()
            .all(|location| location.path != "Private/Diary.md"));
        assert!(!locations.is_empty());

        session.open("Draft.md", 1, "[[Di".into());
        let completion = session
            .completion("Draft.md", TextPosition { line: 0, column: 4 })
            .unwrap()
            .unwrap();
        assert!(completion.items.is_empty(), "{:?}", completion.items);
        session.open("Tags.md", 1, "#hid".into());
        let completion = session
            .completion("Tags.md", TextPosition { line: 0, column: 4 })
            .unwrap()
            .unwrap();
        assert!(completion.items.is_empty(), "{:?}", completion.items);
        session.open("Props.md", 1, "---\nmoo\n---\n".into());
        let completion = session
            .completion("Props.md", TextPosition { line: 1, column: 3 })
            .unwrap()
            .unwrap();
        assert!(completion.items.is_empty(), "{:?}", completion.items);
    }

    #[test]
    fn never_initializes_a_vault() {
        let dir = tempfile::tempdir().unwrap();
        let session = EditorSession::new(VaultPaths::new(dir.path()), PositionEncoding::Utf8);
        assert!(!session.has_cache());
        assert_eq!(session.refresh_index().unwrap(), None);
        assert_eq!(session.scan_saved(&["a.md".into()]).unwrap(), None);
        assert!(EditorWatchLease::try_acquire(session.paths())
            .unwrap()
            .is_none());
        assert!(!dir.path().join(".vulcan").exists());
    }
}
