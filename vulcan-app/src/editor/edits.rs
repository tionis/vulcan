//! Planning link-safe renames and moves for an editor.
//!
//! Every planned file change is routed by whether the file is open in the
//! editor (`docs/specs/editor-integration.md`, "Mutation contract"): open
//! files become versioned edits the editor applies to its buffer, and closed
//! files become one journaled, preimage-checked batch on disk. Plans are
//! computed from disk, so an open buffer that differs from its file (unsaved
//! changes) is refused rather than overwritten.

use super::{EditorRange, EditorSession};
use crate::AppError;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;
use vulcan_core::editor::{element_at, EditorElement, LineIndex, TextPosition};
use vulcan_core::ordinary_write::OrdinaryWriteChange;
use vulcan_core::LinkKind;

/// One replacement in a document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorTextEdit {
    pub range: EditorRange,
    pub new_text: String,
}

/// Edits to one open document, valid only at `version`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorDocumentEdit {
    pub path: String,
    pub version: i32,
    pub edits: Vec<EditorTextEdit>,
}

/// A moved document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EditorMove {
    pub from: String,
    pub to: String,
}

/// A rename or move, split between the editor and the disk.
#[derive(Debug, Clone)]
pub struct EditorEditPlan {
    /// `move`, `rename_heading`, or `rename_block_ref`.
    pub action: &'static str,
    /// The dry-run report of the equivalent CLI command.
    pub report: serde_json::Value,
    /// Changes to files not open in the editor, published as one batch.
    pub disk: Vec<OrdinaryWriteChange>,
    /// Rename hints (destination to source) for the disk batch.
    pub disk_renames: BTreeMap<String, String>,
    /// Edits to documents open in the editor.
    pub open: Vec<EditorDocumentEdit>,
    /// A move of an open document. The editor performs it so its buffer
    /// follows the file; `open` edits address the document's old path.
    pub editor_rename: Option<EditorMove>,
    /// The moved document, when the plan moves one.
    pub moved: Option<EditorMove>,
}

/// What a rename at a cursor position would change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorRenameTarget {
    /// The text the editor highlights.
    pub range: EditorRange,
    /// The current name, offered as the starting value.
    pub placeholder: String,
}

/// What a plan rewrites, to recompute edits for unsaved buffers.
enum Rewrite {
    Move {
        from: String,
        to: String,
    },
    Heading {
        note: String,
        old: String,
        new: String,
    },
    Block {
        note: String,
        old: String,
        new: String,
    },
}

/// `text` with the last occurrence of `old` replaced by `new`.
fn replace_last(text: &str, old: &str, new: &str) -> Option<String> {
    let at = text.rfind(old)?;
    Some(format!("{}{new}{}", &text[..at], &text[at + old.len()..]))
}

/// The thing a cursor renames.
enum Renamable {
    Note { path: String, name: String },
    Heading { path: String, heading: String },
    Block { path: String, block: String },
}

impl EditorSession {
    /// What a rename at `position` would rename, or `None` when nothing
    /// renamable is under the cursor.
    pub fn prepare_rename(
        &self,
        path: &str,
        position: TextPosition,
    ) -> Result<Option<EditorRenameTarget>, AppError> {
        let Some((renamable, range)) = self.renamable_at(path, position)? else {
            return Ok(None);
        };
        let placeholder = match renamable {
            Renamable::Note { name, .. } => name,
            Renamable::Heading { heading, .. } => heading,
            Renamable::Block { block, .. } => block,
        };
        Ok(Some(EditorRenameTarget { range, placeholder }))
    }

    /// Plan renaming what is under the cursor: a linked note (moved within
    /// its folder, or to a vault path when `new_name` contains `/`), a
    /// heading, or a block id, with every link to it rewritten.
    pub fn plan_rename(
        &self,
        path: &str,
        position: TextPosition,
        new_name: &str,
    ) -> Result<EditorEditPlan, AppError> {
        let Some((renamable, _)) = self.renamable_at(path, position)? else {
            return Err(AppError::operation_with_code(
                "invalid_request",
                "nothing to rename here",
            ));
        };
        match renamable {
            Renamable::Note { path, .. } => {
                let destination = move_destination(&path, new_name);
                self.plan_note_move(&path, &destination)
            }
            Renamable::Heading { path, heading } => {
                let plan = vulcan_core::refactor::plan_rename_heading(
                    self.paths(),
                    &path,
                    &heading,
                    new_name,
                )
                .map_err(refactor_error)?;
                let rewrite = Rewrite::Heading {
                    note: path,
                    old: heading,
                    new: new_name.to_string(),
                };
                self.split_plan(
                    "rename_heading",
                    to_json(&plan.report),
                    plan.changes,
                    None,
                    &rewrite,
                )
            }
            Renamable::Block { path, block } => {
                let plan = vulcan_core::refactor::plan_rename_block_ref(
                    self.paths(),
                    &path,
                    &block,
                    new_name,
                )
                .map_err(refactor_error)?;
                let rewrite = Rewrite::Block {
                    note: path,
                    old: block,
                    new: new_name.to_string(),
                };
                self.split_plan(
                    "rename_block_ref",
                    to_json(&plan.report),
                    plan.changes,
                    None,
                    &rewrite,
                )
            }
        }
    }

    /// Plan moving the note at `from` to `to` (vault-relative), rewriting
    /// every link to it and its relative links.
    pub fn plan_note_move(&self, from: &str, to: &str) -> Result<EditorEditPlan, AppError> {
        let plan = vulcan_core::move_rewrite::plan_ordinary_note_move(self.paths(), from, to)
            .map_err(|error| AppError::operation_with_code("conflict", error))?;
        let moved = EditorMove {
            from: plan.summary.source_path.clone(),
            to: plan.summary.destination_path.clone(),
        };
        let report = to_json(&plan.summary);
        if moved.from == moved.to {
            return Err(AppError::operation_with_code(
                "invalid_request",
                format!("{} is already at {}", moved.from, moved.to),
            ));
        }
        if self.paths().vault_root().join(&moved.to).exists() {
            return Err(AppError::operation_with_code(
                "conflict",
                format!("{} already exists", moved.to),
            ));
        }
        let rewrite = Rewrite::Move {
            from: moved.from.clone(),
            to: moved.to.clone(),
        };
        self.split_plan("move", report, plan.changes, Some(moved), &rewrite)
    }

    /// Plan the link rewrites for files the editor is about to rename
    /// itself (`workspace/willRenameFiles`). The plan has no disk renames:
    /// the editor moves the files, and `disk` holds the rewrites of closed
    /// files to publish once the rename happened, addressed by their final
    /// paths.
    pub fn plan_client_renames(
        &self,
        renames: &[EditorMove],
    ) -> Result<Vec<EditorEditPlan>, AppError> {
        let mut plans = Vec::new();
        for rename in renames {
            if !super::is_markdown_path(&rename.from) {
                continue;
            }
            let mut plan = self.plan_note_move(&rename.from, &rename.to)?;
            // The editor moves the file; its rewritten contents (relative
            // links) go to the new path once it exists.
            if let Some(index) = plan
                .disk
                .iter()
                .position(|change| change.path == rename.from)
            {
                let source = plan.disk.remove(index);
                if let Some(created) = plan.disk.iter_mut().find(|change| change.path == rename.to)
                {
                    created.before = source.before;
                    if created.before == created.after {
                        plan.disk.retain(|change| change.path != rename.to);
                    }
                }
            }
            plan.disk_renames.clear();
            plan.editor_rename = None;
            plans.push(plan);
        }
        Ok(plans)
    }

    fn renamable_at(
        &self,
        path: &str,
        position: TextPosition,
    ) -> Result<Option<(Renamable, EditorRange)>, AppError> {
        let Some(document) = self.document(path) else {
            return Ok(None);
        };
        let Some(offset) = self.offset(path, position) else {
            return Ok(None);
        };
        let text = &document.text;
        let index = &document.line_index;
        let found = match element_at(text, &document.parsed, offset) {
            Some(EditorElement::Heading { heading, range, .. }) => {
                let start = range.start + text[range.clone()].find(&heading.text).unwrap_or(0);
                Some((
                    Renamable::Heading {
                        path: path.to_string(),
                        heading: heading.text.clone(),
                    },
                    start..start + heading.text.len(),
                ))
            }
            Some(EditorElement::BlockId { block_ref, range }) => Some((
                Renamable::Block {
                    path: path.to_string(),
                    block: block_ref.block_id_text.clone(),
                },
                range.start + 1..range.end,
            )),
            Some(EditorElement::Link { link, range, .. })
                if !matches!(link.link_kind, LinkKind::External) =>
            {
                let resolver = self.resolver()?;
                let resolution = resolver.resolve(path, link, self.config().link_resolution);
                let Some(target) = resolution.resolved_target_id else {
                    return Ok(None);
                };
                let raw = &text[range.clone()];
                let candidate = link.target_path_candidate.as_deref().unwrap_or_default();
                let target_start = raw
                    .find(candidate)
                    .map_or(range.start, |at| range.start + at);
                if let Some(block) = link.target_block.as_deref() {
                    let at = raw.rfind(block).map_or(range.start, |at| range.start + at);
                    Some((
                        Renamable::Block {
                            path: target,
                            block: block.to_string(),
                        },
                        at..at + block.len(),
                    ))
                } else if let Some(heading) = link.target_heading.as_deref() {
                    let at = raw
                        .rfind(heading)
                        .map_or(range.start, |at| range.start + at);
                    Some((
                        Renamable::Heading {
                            path: target,
                            heading: heading.to_string(),
                        },
                        at..at + heading.len(),
                    ))
                } else if super::is_markdown_path(&target) {
                    let name = Path::new(&target)
                        .file_stem()
                        .and_then(|stem| stem.to_str())
                        .unwrap_or_default()
                        .to_string();
                    Some((
                        Renamable::Note { path: target, name },
                        target_start..target_start + candidate.len(),
                    ))
                } else {
                    None
                }
            }
            _ => None,
        };
        Ok(found.map(|(renamable, range)| (renamable, self.range(text, index, range))))
    }

    /// Route planned changes to the editor or the disk.
    ///
    /// Plans come from disk. An open buffer whose text matches its file gets
    /// the planned edits; one with unsaved changes gets edits recomputed from
    /// its own text by `rewrite`, which also catches links that exist only in
    /// unsaved text. Only a moved note with unsaved changes is refused, since
    /// the editor must rename its file.
    fn split_plan(
        &self,
        action: &'static str,
        report: serde_json::Value,
        changes: Vec<OrdinaryWriteChange>,
        moved: Option<EditorMove>,
        rewrite: &Rewrite,
    ) -> Result<EditorEditPlan, AppError> {
        let mut plan = EditorEditPlan {
            action,
            report,
            disk: Vec::new(),
            disk_renames: BTreeMap::new(),
            open: Vec::new(),
            editor_rename: None,
            moved: moved.clone(),
        };
        let moved_open = moved
            .as_ref()
            .is_some_and(|moved| self.document(&moved.from).is_some());
        let mut dirty = Vec::new();
        let mut moved_contents = None;
        let mut moved_after = None;

        for change in changes {
            let is_source = moved
                .as_ref()
                .is_some_and(|moved| change.path == moved.from);
            let is_destination = moved.as_ref().is_some_and(|moved| change.path == moved.to);
            if moved_open && (is_source || is_destination) {
                if is_source {
                    let document = self.document(&change.path).expect("checked open");
                    if Some(&document.text) != change.before.as_ref() {
                        dirty.push(change.path.clone());
                    }
                    moved_contents = Some((change.before.unwrap_or_default(), document.version));
                } else {
                    moved_after = change.after;
                }
                continue;
            }
            match self.document(&change.path) {
                Some(document) => {
                    let edits = if Some(&document.text) == change.before.as_ref() {
                        text_edits(self, &document.text, &change.after.unwrap_or_default())
                    } else {
                        self.buffer_rewrite(&change.path, document, rewrite)?
                    };
                    if !edits.is_empty() {
                        plan.open.push(EditorDocumentEdit {
                            edits,
                            path: change.path,
                            version: document.version,
                        });
                    }
                }
                None => plan.disk.push(change),
            }
        }
        self.rewrite_untouched_unsaved(&mut plan, moved.as_ref(), rewrite)?;
        if !dirty.is_empty() {
            dirty.sort();
            dirty.dedup();
            return Err(AppError::operation_with_code(
                "conflict",
                format!(
                    "save or discard unsaved changes to the moved note first: {}",
                    dirty.join(", ")
                ),
            ));
        }
        if let Some(moved) = moved {
            if moved_open {
                let (before, version) = moved_contents.unwrap_or_default();
                let after = moved_after.unwrap_or_else(|| before.clone());
                if before != after {
                    plan.open.push(EditorDocumentEdit {
                        path: moved.from.clone(),
                        version,
                        edits: text_edits(self, &before, &after),
                    });
                }
                plan.editor_rename = Some(moved);
            } else {
                plan.disk_renames.insert(moved.to, moved.from);
            }
        }
        Ok(plan)
    }

    /// Rewrite unsaved buffers the disk plan did not touch: they may link
    /// to the target in text that is not saved yet.
    fn rewrite_untouched_unsaved(
        &self,
        plan: &mut EditorEditPlan,
        moved: Option<&EditorMove>,
        rewrite: &Rewrite,
    ) -> Result<(), AppError> {
        let planned = plan
            .open
            .iter()
            .map(|document| document.path.clone())
            .chain(moved.map(|moved| moved.from.clone()))
            .collect::<std::collections::BTreeSet<_>>();
        let mut unsaved = self
            .open_paths()
            .filter(|path| !planned.contains(*path))
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        unsaved.sort();
        for path in unsaved {
            let document = self.document(&path).expect("listed as open");
            let on_disk = std::fs::read_to_string(self.paths().vault_root().join(&path)).ok();
            if on_disk.as_deref() == Some(document.text.as_str()) {
                continue;
            }
            let edits = self.buffer_rewrite(&path, document, rewrite)?;
            if !edits.is_empty() {
                plan.open.push(EditorDocumentEdit {
                    path,
                    version: document.version,
                    edits,
                });
            }
        }
        Ok(())
    }

    /// Edits for an unsaved buffer, computed from its own text: links to the
    /// moved note get their new target, links to the renamed heading or block
    /// their new subpath, and the renamed heading or block label itself is
    /// renamed when this is its note.
    fn buffer_rewrite(
        &self,
        path: &str,
        document: &super::OpenDocument,
        rewrite: &Rewrite,
    ) -> Result<Vec<EditorTextEdit>, AppError> {
        let resolver = self.resolver()?;
        let mode = self.config().link_resolution;
        let text = &document.text;
        let mut replacements = Vec::<(std::ops::Range<usize>, String)>::new();
        let documents_after = match rewrite {
            Rewrite::Move { from, to } => resolver
                .documents()
                .iter()
                .map(|document| {
                    if &document.path == from {
                        to.clone()
                    } else {
                        document.path.clone()
                    }
                })
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        };
        for link in &document.parsed.links {
            if matches!(link.link_kind, LinkKind::External) {
                continue;
            }
            let Some(target) = resolver.resolve(path, link, mode).resolved_target_id else {
                continue;
            };
            let range = vulcan_core::editor::link_range(link);
            let replacement = match rewrite {
                Rewrite::Move { from, to } if &target == from => {
                    Some(vulcan_core::move_rewrite::rewrite_link_destination(
                        link,
                        path,
                        Some(to),
                        link.target_heading.as_deref(),
                        link.target_block.as_deref(),
                        &documents_after,
                        mode,
                        self.config().link_style,
                    ))
                }
                Rewrite::Heading { note, old, new } if &target == note => link
                    .target_heading
                    .as_deref()
                    .filter(|heading| super::navigation::same_heading(heading, old))
                    .and_then(|_| {
                        replace_last(&link.raw_text, &format!("#{old}"), &format!("#{new}"))
                    }),
                Rewrite::Block { note, old, new } if &target == note => link
                    .target_block
                    .as_deref()
                    .filter(|block| block.eq_ignore_ascii_case(old))
                    .and_then(|_| {
                        replace_last(&link.raw_text, &format!("#^{old}"), &format!("#^{new}"))
                    }),
                _ => None,
            };
            if let Some(replacement) = replacement.filter(|new| new != &link.raw_text) {
                replacements.push((range, replacement));
            }
        }
        match rewrite {
            Rewrite::Heading { note, old, new } if note == path => {
                for heading in &document.parsed.headings {
                    if super::navigation::same_heading(&heading.text, old) {
                        let line = vulcan_core::editor::heading_range(text, heading);
                        if let Some(at) = text[line.clone()].rfind(&heading.text) {
                            let start = line.start + at;
                            replacements.push((start..start + heading.text.len(), new.clone()));
                        }
                    }
                }
            }
            Rewrite::Block { note, old, new } if note == path => {
                for block in &document.parsed.block_refs {
                    if block.block_id_text.eq_ignore_ascii_case(old) {
                        let label = vulcan_core::editor::block_id_range(text, block);
                        replacements.push((label, format!("^{new}")));
                    }
                }
            }
            _ => {}
        }
        Ok(replacements
            .into_iter()
            .map(|(range, new_text)| EditorTextEdit {
                range: self.range(text, &document.line_index, range),
                new_text,
            })
            .collect())
    }

    /// Publish the plan's closed-file changes as one journaled batch and
    /// index them. Fails without writing when any file changed since
    /// planning.
    pub fn apply_disk_changes(
        &self,
        changes: &[OrdinaryWriteChange],
        renames: &BTreeMap<String, String>,
    ) -> Result<(), AppError> {
        if changes.is_empty() {
            return Ok(());
        }
        vulcan_core::ordinary_write::apply_ordinary_write_batch_with_renames_and_preflight(
            self.paths(),
            changes,
            renames,
            || Ok(()),
        )
        .map_err(|error| AppError::operation_with_code("conflict", error.message))?;
        self.invalidate_cache();
        Ok(())
    }
}

/// The batch undoing `changes`, and its rename hints. Applied with the
/// planned contents as preimages, it restores every file still holding them
/// and refuses (changing nothing) when any was edited since.
#[must_use]
pub fn inverse_changes(
    changes: &[OrdinaryWriteChange],
    renames: &BTreeMap<String, String>,
) -> (Vec<OrdinaryWriteChange>, BTreeMap<String, String>) {
    let inverse = changes
        .iter()
        .map(|change| OrdinaryWriteChange {
            path: change.path.clone(),
            before: change.after.clone(),
            after: change.before.clone(),
        })
        .collect();
    let renames = renames
        .iter()
        .map(|(destination, source)| (source.clone(), destination.clone()))
        .collect();
    (inverse, renames)
}

/// The vault path a note moves to when renamed to `new_name`: a sibling in
/// the same folder, or a vault path when the name contains `/` (a leading
/// `/` names the vault root).
fn move_destination(from: &str, new_name: &str) -> String {
    let trimmed = new_name.trim();
    let from_root = trimmed.starts_with('/');
    let name = trimmed.trim_start_matches('/');
    let name = if Path::new(name)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
    {
        name.to_string()
    } else {
        format!("{name}.md")
    };
    if from_root || name.contains('/') {
        return name;
    }
    match from.rsplit_once('/') {
        Some((folder, _)) => format!("{folder}/{name}"),
        None => name,
    }
}

/// Small edits turning `before` into `after`: per changed line when the line
/// count is unchanged (the usual case for link rewrites), otherwise one edit
/// spanning the changed region.
pub(super) fn text_edits(
    session: &EditorSession,
    before: &str,
    after: &str,
) -> Vec<EditorTextEdit> {
    let index = LineIndex::new(before);
    let edit = |range: std::ops::Range<usize>, new_text: &str| EditorTextEdit {
        range: session.range(before, &index, range),
        new_text: new_text.to_string(),
    };
    let before_lines = before.split_inclusive('\n').collect::<Vec<_>>();
    let after_lines = after.split_inclusive('\n').collect::<Vec<_>>();
    if before_lines.len() == after_lines.len() {
        let mut edits = Vec::new();
        let mut offset = 0;
        for (old, new) in before_lines.iter().zip(&after_lines) {
            if old != new {
                let (start, old_end, new_end) = changed_span(old, new);
                edits.push(edit(offset + start..offset + old_end, &new[start..new_end]));
            }
            offset += old.len();
        }
        return edits;
    }
    let (start, old_end, new_end) = changed_span(before, after);
    vec![edit(start..old_end, &after[start..new_end])]
}

/// The differing middle of two texts: its start, and its end in each.
fn changed_span(old: &str, new: &str) -> (usize, usize, usize) {
    let mut prefix = old
        .bytes()
        .zip(new.bytes())
        .take_while(|(left, right)| left == right)
        .count();
    while !old.is_char_boundary(prefix) || !new.is_char_boundary(prefix) {
        prefix -= 1;
    }
    let mut suffix = old[prefix..]
        .bytes()
        .rev()
        .zip(new[prefix..].bytes().rev())
        .take_while(|(left, right)| left == right)
        .count();
    while !old.is_char_boundary(old.len() - suffix) || !new.is_char_boundary(new.len() - suffix) {
        suffix -= 1;
    }
    (prefix, old.len() - suffix, new.len() - suffix)
}

fn to_json(value: &impl Serialize) -> serde_json::Value {
    serde_json::to_value(value).unwrap_or(serde_json::Value::Null)
}

fn refactor_error(error: vulcan_core::refactor::RefactorError) -> AppError {
    AppError::operation_with_code("invalid_request", error)
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::*;

    const HOME: &str = "# Home\n\nSee [[Alpha]] and [[Alpha#Status]] 😀 [[Alpha#^blk]].\n";
    const ALPHA: &str = "# Alpha\n\n## Status\n\nGreen ^blk\n\nBack to [[Home]].\n";

    fn session() -> (tempfile::TempDir, EditorSession) {
        let (dir, mut session) = vault(&[
            ("Home.md", HOME),
            ("notes/Alpha.md", ALPHA),
            ("Other.md", "Also [[Alpha]].\n"),
        ]);
        session.open("Home.md", 1, HOME.to_string());
        (dir, session)
    }

    #[test]
    fn prepares_renames_for_links_headings_and_blocks() {
        let (_dir, session) = session();
        let target = |needle: &str, shift: usize| {
            session
                .prepare_rename("Home.md", position_of(HOME, needle, shift))
                .unwrap()
                .map(|target| target.placeholder)
        };
        assert_eq!(target("[[Alpha]]", 3).as_deref(), Some("Alpha"));
        assert_eq!(target("Status]]", 1).as_deref(), Some("Status"));
        assert_eq!(target("^blk]]", 1).as_deref(), Some("blk"));
        assert_eq!(target("# Home", 3).as_deref(), Some("Home"));
        assert_eq!(target("See", 1), None);
        let range = session
            .prepare_rename("Home.md", position_of(HOME, "[[Alpha]]", 3))
            .unwrap()
            .unwrap()
            .range;
        assert_eq!(range.start, position_of(HOME, "Alpha]]", 0));
        assert_eq!(range.end, position_of(HOME, "]] and", 0));
    }

    #[test]
    fn moving_a_closed_note_writes_disk_and_edits_open_buffers() {
        let (dir, session) = session();
        let plan = session
            .plan_rename("Home.md", position_of(HOME, "[[Alpha]]", 3), "Beta")
            .unwrap();
        assert_eq!(plan.action, "move");
        assert_eq!(
            plan.moved,
            Some(EditorMove {
                from: "notes/Alpha.md".into(),
                to: "notes/Beta.md".into(),
            })
        );
        assert_eq!(plan.editor_rename, None);
        assert_eq!(
            plan.disk_renames,
            BTreeMap::from([("notes/Beta.md".to_string(), "notes/Alpha.md".to_string())])
        );
        let disk_paths = plan
            .disk
            .iter()
            .map(|change| change.path.as_str())
            .collect::<Vec<_>>();
        assert!(disk_paths.contains(&"Other.md"));
        assert!(!disk_paths.contains(&"Home.md"));
        assert_eq!(plan.open.len(), 1);
        let home = apply(HOME, &plan.open[0].edits);
        assert_eq!(
            home,
            "# Home\n\nSee [[Beta]] and [[Beta#Status]] 😀 [[Beta#^blk]].\n"
        );
        // Nothing is written until the plan is applied.
        assert!(dir.path().join("notes/Alpha.md").exists());

        session
            .apply_disk_changes(&plan.disk, &plan.disk_renames)
            .unwrap();
        assert!(!dir.path().join("notes/Alpha.md").exists());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("Other.md")).unwrap(),
            "Also [[Beta]].\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("Home.md")).unwrap(),
            HOME,
            "open buffers are never written"
        );

        // The inverse batch restores the vault.
        let (inverse, renames) = inverse_changes(&plan.disk, &plan.disk_renames);
        session.apply_disk_changes(&inverse, &renames).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("notes/Alpha.md")).unwrap(),
            ALPHA
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("Other.md")).unwrap(),
            "Also [[Alpha]].\n"
        );
    }

    #[test]
    fn moving_an_open_note_lets_the_editor_rename_it() {
        let (_dir, mut session) = session();
        session.open("notes/Alpha.md", 4, ALPHA.to_string());
        let plan = session
            .plan_note_move("notes/Alpha.md", "Alpha.md")
            .unwrap();
        assert_eq!(
            plan.editor_rename,
            Some(EditorMove {
                from: "notes/Alpha.md".into(),
                to: "Alpha.md".into(),
            })
        );
        assert!(plan.disk_renames.is_empty());
        assert!(plan
            .disk
            .iter()
            .all(|change| change.path != "notes/Alpha.md" && change.path != "Alpha.md"));
        // The moved note links `[[Home]]` by name, which still resolves from
        // its new folder, so its buffer needs no edit.
        assert!(plan.open.iter().all(|edit| edit.path != "notes/Alpha.md"));
    }

    #[test]
    fn unsaved_buffers_are_rewritten_from_their_own_text() {
        let (dir, mut session) = session();
        // Other.md has an unsaved extra link; Draft.md exists only in the
        // editor and links the note too.
        session.open(
            "Other.md",
            2,
            "Also [[Alpha]] and [[Alpha#Status]] (unsaved).\n".into(),
        );
        session.open("Draft.md", 1, "Draft about [[Alpha]].\n".into());
        let plan = session
            .plan_rename("Home.md", position_of(HOME, "[[Alpha]]", 3), "Beta")
            .unwrap();
        let edited = |path: &str, text: &str| {
            let document = plan
                .open
                .iter()
                .find(|document| document.path == path)
                .unwrap_or_else(|| panic!("{path} is edited"));
            apply(text, &document.edits)
        };
        assert_eq!(
            edited(
                "Other.md",
                "Also [[Alpha]] and [[Alpha#Status]] (unsaved).\n"
            ),
            "Also [[Beta]] and [[Beta#Status]] (unsaved).\n"
        );
        assert_eq!(
            edited("Draft.md", "Draft about [[Alpha]].\n"),
            "Draft about [[Beta]].\n"
        );
        // The unsaved buffer's file is not part of the disk batch.
        assert!(plan.disk.iter().all(|change| change.path != "Other.md"));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("Other.md")).unwrap(),
            "Also [[Alpha]].\n"
        );
    }

    #[test]
    fn unsaved_heading_renames_rename_the_heading_in_its_buffer() {
        let (_dir, mut session) = session();
        let unsaved = "# Alpha\n\n## Status\n\nGreen, edited ^blk\n\nSee [[#Status]].\n";
        session.open("notes/Alpha.md", 3, unsaved.into());
        let plan = session
            .plan_rename("Home.md", position_of(HOME, "Status]]", 1), "Progress")
            .unwrap();
        let alpha = plan
            .open
            .iter()
            .find(|document| document.path == "notes/Alpha.md")
            .expect("the heading's note is edited");
        assert_eq!(alpha.version, 3);
        assert_eq!(
            apply(unsaved, &alpha.edits),
            "# Alpha\n\n## Progress\n\nGreen, edited ^blk\n\nSee [[#Progress]].\n"
        );
    }

    #[test]
    fn a_moved_note_with_unsaved_changes_is_refused() {
        let (_dir, mut session) = session();
        session.open("notes/Alpha.md", 2, "# Alpha (unsaved)\n".into());
        let error = session
            .plan_note_move("notes/Alpha.md", "Alpha.md")
            .unwrap_err();
        assert_eq!(error.code(), Some("conflict"));
        assert!(error.to_string().contains("notes/Alpha.md"));
    }

    #[test]
    fn renames_headings_and_blocks_through_links() {
        let (dir, session) = session();
        let plan = session
            .plan_rename("Home.md", position_of(HOME, "Status]]", 1), "Progress")
            .unwrap();
        assert_eq!(plan.action, "rename_heading");
        assert_eq!(plan.moved, None);
        assert_eq!(
            apply(HOME, &plan.open[0].edits),
            "# Home\n\nSee [[Alpha]] and [[Alpha#Progress]] 😀 [[Alpha#^blk]].\n"
        );
        session
            .apply_disk_changes(&plan.disk, &plan.disk_renames)
            .unwrap();
        assert!(std::fs::read_to_string(dir.path().join("notes/Alpha.md"))
            .unwrap()
            .contains("## Progress"));

        let plan = session
            .plan_rename("Home.md", position_of(HOME, "^blk", 1), "done")
            .unwrap();
        assert_eq!(plan.action, "rename_block_ref");
        assert!(apply(HOME, &plan.open[0].edits).contains("[[Alpha#^done]]"));
    }

    #[test]
    fn client_renames_rewrite_closed_files_at_their_new_paths() {
        let (dir, session) = session();
        let plans = session
            .plan_client_renames(&[EditorMove {
                from: "Other.md".into(),
                to: "deep/Other.md".into(),
            }])
            .unwrap();
        assert_eq!(plans.len(), 1);
        assert!(plans[0].disk_renames.is_empty());
        assert!(plans[0].editor_rename.is_none());
        // `Other.md` links `[[Alpha]]` by name, so moving it changes nothing
        // inside it, and nothing links to it.
        assert!(plans[0].disk.iter().all(|change| change.path != "Other.md"));
        std::fs::create_dir_all(dir.path().join("deep")).unwrap();
        std::fs::rename(
            dir.path().join("Other.md"),
            dir.path().join("deep/Other.md"),
        )
        .unwrap();
        session
            .apply_disk_changes(&plans[0].disk, &BTreeMap::new())
            .unwrap();
    }

    #[test]
    fn computes_destinations_and_minimal_edits() {
        assert_eq!(move_destination("a/b/Note.md", "New"), "a/b/New.md");
        assert_eq!(move_destination("Note.md", "x/New.md"), "x/New.md");
        assert_eq!(move_destination("a/Note.md", "/top"), "top.md");
        assert_eq!(changed_span("a😀b", "a😀c"), (5, 6, 6));
        assert_eq!(changed_span("same", "same"), (4, 4, 4));
        assert_eq!(changed_span("ab", "aXb"), (1, 1, 2));
    }
}
