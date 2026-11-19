//! One place to apply a planned set of whole-file note changes.
//!
//! Planners (refactors, link rewrites, Kanban edits, note splits, imports,
//! editor actions) produce path, before, and after triples. When any of
//! those paths is an mdbase collection record, every change goes through one
//! validated, journaled mdbase transaction, ordinary companion notes
//! included, so a bulk edit can neither break a record's schema nor skip its
//! lifecycle and recovery. Otherwise the changes are published as one
//! crash-safe ordinary write batch.

use crate::mdbase::{
    apply_managed_mdbase_note_writes, load_mdbase_routing_collection,
    MdbaseManagedNoteWriteBatchRequest, MdbaseManagedNoteWriteChange, MdbaseManagedWriteMode,
    MdbaseWriteOperation,
};
use crate::AppError;
use std::collections::BTreeMap;
use vulcan_core::mdbase::is_mdbase_record_path;
use vulcan_core::ordinary_write::OrdinaryWriteChange;
use vulcan_core::{
    resolve_permission_profile, scan_vault, ProfilePermissionGuard, ScanMode, VaultPaths, Verbosity,
};

/// Which writer published a batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteWriteRoute {
    /// Nothing to write.
    Empty,
    /// At least one path is an mdbase record: one validated transaction.
    Mdbase,
    /// No path is a record: one ordinary write batch.
    Ordinary,
}

/// A planned batch of note changes and how to apply it.
#[derive(Debug, Clone, Copy)]
pub struct PlannedNoteWrite<'a> {
    pub changes: &'a [OrdinaryWriteChange],
    /// Moves in the batch, destination to source; each is also a create and
    /// a delete in `changes`.
    pub renames: Option<&'a BTreeMap<String, String>>,
    pub dry_run: bool,
    pub permission_profile: Option<&'a str>,
    pub verbosity: Verbosity,
}

impl<'a> PlannedNoteWrite<'a> {
    #[must_use]
    pub fn new(changes: &'a [OrdinaryWriteChange], permission_profile: Option<&'a str>) -> Self {
        Self {
            changes,
            renames: None,
            dry_run: false,
            permission_profile,
            verbosity: Verbosity::Quiet,
        }
    }

    #[must_use]
    pub fn renames(mut self, renames: &'a BTreeMap<String, String>) -> Self {
        self.renames = Some(renames);
        self
    }

    #[must_use]
    pub fn dry_run(mut self, dry_run: bool) -> Self {
        self.dry_run = dry_run;
        self
    }

    #[must_use]
    pub fn verbosity(mut self, verbosity: Verbosity) -> Self {
        self.verbosity = verbosity;
        self
    }
}

/// Apply `write`: through one validated mdbase transaction when any path is
/// a collection record, else as one ordinary write batch followed by a cache
/// scan. Each change's `before` is checked against the file, so a note
/// edited since planning fails the whole batch without writing anything.
///
/// A dry run validates an mdbase batch, so it reports what would be
/// rejected, and writes nothing.
///
/// # Errors
///
/// Returns the mdbase validation or conflict error, or the ordinary batch's
/// conflict, having changed no file.
pub fn apply_planned_note_write(
    paths: &VaultPaths,
    write: &PlannedNoteWrite<'_>,
) -> Result<NoteWriteRoute, AppError> {
    apply_planned_note_write_with(paths, write, true)
}

/// [`apply_planned_note_write`] without the cache scan after an ordinary
/// batch, for a caller that refreshes the cache itself.
pub fn apply_planned_note_write_unscanned(
    paths: &VaultPaths,
    write: &PlannedNoteWrite<'_>,
) -> Result<NoteWriteRoute, AppError> {
    apply_planned_note_write_with(paths, write, false)
}

fn apply_planned_note_write_with(
    paths: &VaultPaths,
    write: &PlannedNoteWrite<'_>,
    scan: bool,
) -> Result<NoteWriteRoute, AppError> {
    if write.changes.is_empty() {
        return Ok(NoteWriteRoute::Empty);
    }
    let empty = BTreeMap::new();
    let renames = write.renames.unwrap_or(&empty);
    if route_through_mdbase(paths, write, renames)? {
        return Ok(NoteWriteRoute::Mdbase);
    }
    if write.dry_run {
        return Ok(NoteWriteRoute::Ordinary);
    }
    vulcan_core::ordinary_write::apply_ordinary_write_batch_with_renames_and_preflight(
        paths,
        write.changes,
        renames,
        || recheck_ordinary_routes(paths, write),
    )
    .map_err(|error| AppError::operation_with_code(error.code, error.message))?;
    // A batch with renames already ran its rename-aware scan.
    if scan && renames.is_empty() {
        scan_vault(paths, ScanMode::Incremental).map_err(AppError::operation)?;
    }
    Ok(NoteWriteRoute::Ordinary)
}

/// Send the batch to mdbase when a path is a record; false when none is.
fn route_through_mdbase(
    paths: &VaultPaths,
    write: &PlannedNoteWrite<'_>,
    renames: &BTreeMap<String, String>,
) -> Result<bool, AppError> {
    if !touches_mdbase_record(paths, write)? {
        return Ok(false);
    }
    let changes = write
        .changes
        .iter()
        .map(|change| MdbaseManagedNoteWriteChange {
            path: &change.path,
            before: change.before.as_deref(),
            after: change.after.as_deref(),
        })
        .collect::<Vec<_>>();
    let operation = match (renames.iter().next(), write.changes) {
        (Some((to, from)), _) if renames.len() == 1 => MdbaseWriteOperation::Rename {
            from: from.clone(),
            to: to.clone(),
        },
        (None, [change]) => match (&change.before, &change.after) {
            (None, _) => MdbaseWriteOperation::Create,
            (Some(_), None) => MdbaseWriteOperation::Delete,
            (Some(_), Some(_)) => MdbaseWriteOperation::Update,
        },
        _ => MdbaseWriteOperation::Batch,
    };
    let routed = apply_managed_mdbase_note_writes(
        paths,
        &MdbaseManagedNoteWriteBatchRequest {
            changes: &changes,
            operation,
            mode: MdbaseManagedWriteMode::Validated,
            allow_mixed_paths: true,
            dry_run: write.dry_run,
            permission_profile: write.permission_profile,
            verbosity: write.verbosity,
        },
    )?;
    Ok(routed.is_some())
}

/// Whether any of `changes` is an mdbase collection record, so the batch
/// would go through mdbase.
pub(crate) fn changes_touch_mdbase_record(
    paths: &VaultPaths,
    changes: &[OrdinaryWriteChange],
    permission_profile: Option<&str>,
) -> Result<bool, AppError> {
    touches_mdbase_record(paths, &PlannedNoteWrite::new(changes, permission_profile))
}

fn touches_mdbase_record(
    paths: &VaultPaths,
    write: &PlannedNoteWrite<'_>,
) -> Result<bool, AppError> {
    let selection =
        resolve_permission_profile(paths, write.permission_profile).map_err(AppError::operation)?;
    let guard = ProfilePermissionGuard::new(paths, selection);
    let Some(collection) = load_mdbase_routing_collection(paths, &guard)? else {
        return Ok(false);
    };
    for change in write.changes {
        if is_mdbase_record_path(&collection, &change.path).map_err(AppError::operation)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Under the write lock, refuse an ordinary batch whose path became a record
/// since routing, rather than writing a record unvalidated.
fn recheck_ordinary_routes(paths: &VaultPaths, write: &PlannedNoteWrite<'_>) -> Result<(), String> {
    match touches_mdbase_record(paths, write) {
        Ok(false) => Ok(()),
        Ok(true) => Err("mdbase collection changed during the write; retry the operation".into()),
        Err(error) => Err(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const TASK_TYPE: &str = "---\nkind: mdbase.type\nname: task\nmatch:\n  where:\n    type: task\nschema:\n  dialect: json-schema-2020-12\n  value:\n    type: object\n    required: [title]\n    properties:\n      title: {type: string}\n      due: {type: string, format: date}\n---\n";

    fn vault(collection: bool) -> (tempfile::TempDir, VaultPaths) {
        let directory = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(directory.path());
        vulcan_core::initialize_vulcan_dir(&paths).unwrap();
        let root = directory.path();
        if collection {
            fs::write(root.join("mdbase.yaml"), "spec_version: '0.3.0'\n").unwrap();
            fs::create_dir_all(root.join("_types")).unwrap();
            fs::write(root.join("_types/task.md"), TASK_TYPE).unwrap();
        }
        fs::create_dir_all(root.join("tasks")).unwrap();
        fs::write(
            root.join("tasks/a.md"),
            "---\ntype: task\ntitle: A\ndue: 2026-11-19\n---\n",
        )
        .unwrap();
        fs::write(root.join("Plain.md"), "# Plain\n").unwrap();
        scan_vault(&paths, ScanMode::Full).unwrap();
        (directory, paths)
    }

    fn change(path: &str, before: &str, after: &str) -> OrdinaryWriteChange {
        OrdinaryWriteChange {
            path: path.to_string(),
            before: Some(before.to_string()),
            after: Some(after.to_string()),
        }
    }

    fn read(paths: &VaultPaths, path: &str) -> String {
        fs::read_to_string(paths.vault_root().join(path)).unwrap()
    }

    #[test]
    fn a_batch_touching_a_record_is_validated_as_one_transaction() {
        let (_dir, paths) = vault(true);
        let record = "---\ntype: task\ntitle: A\ndue: 2026-11-19\n---\n";
        let changes = [
            change(
                "tasks/a.md",
                record,
                "---\ntype: task\ntitle: A\ndue: soon\n---\n",
            ),
            change("Plain.md", "# Plain\n", "# Plain\nedited\n"),
        ];
        let error = apply_planned_note_write(&paths, &PlannedNoteWrite::new(&changes, None))
            .expect_err("an invalid record is rejected");
        assert!(error.to_string().contains("format_invalid"), "{error}");
        assert_eq!(read(&paths, "tasks/a.md"), record);
        assert_eq!(read(&paths, "Plain.md"), "# Plain\n", "nothing is written");

        let changes = [
            change(
                "tasks/a.md",
                record,
                "---\ntype: task\ntitle: B\ndue: 2026-11-20\n---\n",
            ),
            change("Plain.md", "# Plain\n", "# Plain\nedited\n"),
        ];
        let route =
            apply_planned_note_write(&paths, &PlannedNoteWrite::new(&changes, None)).unwrap();
        assert_eq!(route, NoteWriteRoute::Mdbase);
        assert!(read(&paths, "tasks/a.md").contains("title: B"));
        assert_eq!(read(&paths, "Plain.md"), "# Plain\nedited\n");
    }

    #[test]
    fn a_dry_run_reports_a_rejection_without_writing() {
        let (_dir, paths) = vault(true);
        let record = "---\ntype: task\ntitle: A\ndue: 2026-11-19\n---\n";
        let changes = [change(
            "tasks/a.md",
            record,
            "---\ntype: task\ndue: 2026-11-19\n---\n",
        )];
        assert!(apply_planned_note_write(
            &paths,
            &PlannedNoteWrite::new(&changes, None).dry_run(true)
        )
        .is_err());
        assert_eq!(read(&paths, "tasks/a.md"), record);
    }

    #[test]
    fn whole_batches_apply_and_stale_preimages_fail() {
        for collection in [false, true] {
            let (_dir, paths) = vault(collection);
            let changes = [change("Plain.md", "# Plain\n", "# Plain\nedited\n")];
            let route =
                apply_planned_note_write(&paths, &PlannedNoteWrite::new(&changes, None)).unwrap();
            // A collection at the vault root holds every note.
            let expected = if collection {
                NoteWriteRoute::Mdbase
            } else {
                NoteWriteRoute::Ordinary
            };
            assert_eq!(route, expected);
            assert_eq!(read(&paths, "Plain.md"), "# Plain\nedited\n");

            // A stale preimage fails without writing.
            let stale = [change("Plain.md", "# Plain\n", "# Plain\nagain\n")];
            assert!(
                apply_planned_note_write(&paths, &PlannedNoteWrite::new(&stale, None)).is_err()
            );
            assert_eq!(read(&paths, "Plain.md"), "# Plain\nedited\n");
        }
    }
}
