//! Vault refactors that respect mdbase collections.
//!
//! `vulcan-core` plans each refactor; the plan is applied through
//! [`crate::note_writes`], so any rewritten collection record is validated
//! and journaled with the rest of the batch instead of being overwritten
//! directly.

use crate::note_writes::{apply_planned_note_write, PlannedNoteWrite};
use crate::AppError;
use vulcan_core::refactor::{RefactorReport, RefactorWritePlan};
use vulcan_core::{VaultPaths, Verbosity};

/// Apply a planned refactor, or with `dry_run` only validate it, and return
/// its report.
///
/// # Errors
///
/// Returns the planning error, an mdbase validation error for a rewritten
/// record, or a conflict when a file changed since planning.
pub fn apply_refactor_plan(
    paths: &VaultPaths,
    plan: RefactorWritePlan,
    dry_run: bool,
    permission_profile: Option<&str>,
) -> Result<RefactorReport, AppError> {
    apply_planned_note_write(
        paths,
        &PlannedNoteWrite::new(&plan.changes, permission_profile)
            .dry_run(dry_run)
            .verbosity(Verbosity::Quiet),
    )?;
    let mut report = plan.report;
    report.dry_run = dry_run;
    Ok(report)
}

/// [`vulcan_core::rename_property`] across the vault.
pub fn rename_property(
    paths: &VaultPaths,
    old_key: &str,
    new_key: &str,
    dry_run: bool,
    permission_profile: Option<&str>,
) -> Result<RefactorReport, AppError> {
    let plan = vulcan_core::refactor::plan_rename_property(paths, old_key, new_key)
        .map_err(AppError::operation)?;
    apply_refactor_plan(paths, plan, dry_run, permission_profile)
}

/// [`vulcan_core::merge_tags`] across the vault.
pub fn merge_tags(
    paths: &VaultPaths,
    source_tag: &str,
    destination_tag: &str,
    dry_run: bool,
    permission_profile: Option<&str>,
) -> Result<RefactorReport, AppError> {
    let plan = vulcan_core::refactor::plan_merge_tags(paths, source_tag, destination_tag)
        .map_err(AppError::operation)?;
    apply_refactor_plan(paths, plan, dry_run, permission_profile)
}

/// [`vulcan_core::rename_alias`] on one note.
pub fn rename_alias(
    paths: &VaultPaths,
    note: &str,
    old_alias: &str,
    new_alias: &str,
    dry_run: bool,
    permission_profile: Option<&str>,
) -> Result<RefactorReport, AppError> {
    let plan = vulcan_core::refactor::plan_rename_alias(paths, note, old_alias, new_alias)
        .map_err(AppError::operation)?;
    apply_refactor_plan(paths, plan, dry_run, permission_profile)
}

/// [`vulcan_core::rename_heading`] and the links into it.
pub fn rename_heading(
    paths: &VaultPaths,
    note: &str,
    old_heading: &str,
    new_heading: &str,
    dry_run: bool,
    permission_profile: Option<&str>,
) -> Result<RefactorReport, AppError> {
    let plan = vulcan_core::refactor::plan_rename_heading(paths, note, old_heading, new_heading)
        .map_err(AppError::operation)?;
    apply_refactor_plan(paths, plan, dry_run, permission_profile)
}

/// [`vulcan_core::rename_block_ref`] and the links into it.
pub fn rename_block_ref(
    paths: &VaultPaths,
    note: &str,
    old_block_ref: &str,
    new_block_ref: &str,
    dry_run: bool,
    permission_profile: Option<&str>,
) -> Result<RefactorReport, AppError> {
    let plan =
        vulcan_core::refactor::plan_rename_block_ref(paths, note, old_block_ref, new_block_ref)
            .map_err(AppError::operation)?;
    apply_refactor_plan(paths, plan, dry_run, permission_profile)
}

/// [`vulcan_core::set_note_property`] on one note.
pub fn set_note_property(
    paths: &VaultPaths,
    note: &str,
    key: &str,
    value: Option<&str>,
    dry_run: bool,
    permission_profile: Option<&str>,
) -> Result<RefactorReport, AppError> {
    let plan = vulcan_core::refactor::plan_set_note_property(paths, note, key, value)
        .map_err(AppError::operation)?;
    apply_refactor_plan(paths, plan, dry_run, permission_profile)
}

/// [`vulcan_core::link_mentions`] in one note, or the whole vault.
pub fn link_mentions(
    paths: &VaultPaths,
    note: Option<&str>,
    dry_run: bool,
    permission_profile: Option<&str>,
) -> Result<RefactorReport, AppError> {
    let plan =
        vulcan_core::suggestions::plan_link_mentions(paths, note).map_err(AppError::operation)?;
    apply_refactor_plan(paths, plan, dry_run, permission_profile)
}

/// [`vulcan_core::bulk_replace`] over the notes matching `filters`.
pub fn bulk_replace(
    paths: &VaultPaths,
    filters: &[String],
    find: &str,
    replace: &str,
    dry_run: bool,
    permission_profile: Option<&str>,
) -> Result<RefactorReport, AppError> {
    let plan = vulcan_core::suggestions::plan_bulk_replace(paths, filters, find, replace)
        .map_err(AppError::operation)?;
    apply_refactor_plan(paths, plan, dry_run, permission_profile)
}

/// [`vulcan_core::bulk_replace_on_paths`] over the given notes.
pub fn bulk_replace_on_paths(
    paths: &VaultPaths,
    note_paths: &[String],
    find: &str,
    replace: &str,
    dry_run: bool,
    permission_profile: Option<&str>,
) -> Result<RefactorReport, AppError> {
    let plan =
        vulcan_core::suggestions::plan_bulk_replace_on_paths(paths, note_paths, find, replace)
            .map_err(AppError::operation)?;
    apply_refactor_plan(paths, plan, dry_run, permission_profile)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use vulcan_core::{initialize_vulcan_dir, scan_vault, ScanMode};

    const TASK_TYPE: &str = "---\nkind: mdbase.type\nname: task\nmatch:\n  where:\n    type: task\nschema:\n  dialect: json-schema-2020-12\n  value:\n    type: object\n    required: [title]\n    properties:\n      title: {type: string}\n      due: {type: string, format: date}\n---\n";
    const RECORD: &str = "---\ntype: task\ntitle: A\ndue: 2026-11-19\n---\nSee Plain.\n";

    fn vault() -> (tempfile::TempDir, VaultPaths) {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let paths = VaultPaths::new(root);
        initialize_vulcan_dir(&paths).unwrap();
        fs::write(root.join("mdbase.yaml"), "spec_version: '0.3.0'\n").unwrap();
        fs::create_dir_all(root.join("_types")).unwrap();
        fs::write(root.join("_types/task.md"), TASK_TYPE).unwrap();
        fs::write(root.join("a.md"), RECORD).unwrap();
        fs::write(root.join("Plain.md"), "---\ntitle: Plain\n---\n# Plain\n").unwrap();
        scan_vault(&paths, ScanMode::Full).unwrap();
        (directory, paths)
    }

    fn read(paths: &VaultPaths, path: &str) -> String {
        fs::read_to_string(paths.vault_root().join(path)).unwrap()
    }

    #[test]
    fn a_refactor_that_breaks_a_record_writes_nothing() {
        let (_dir, paths) = vault();
        // Renaming `title` drops a required property from the record.
        let error = rename_property(&paths, "title", "name", false, None)
            .expect_err("the record would lose a required property");
        assert!(error.to_string().contains("required"), "{error}");
        assert_eq!(read(&paths, "a.md"), RECORD);
        assert!(read(&paths, "Plain.md").contains("title: Plain"));

        // A dry run reports the same rejection.
        assert!(rename_property(&paths, "title", "name", true, None).is_err());

        let error = bulk_replace_on_paths(
            &paths,
            &["a.md".to_string()],
            "2026-11-19",
            "soon",
            false,
            None,
        )
        .expect_err("an invalid date is rejected");
        assert!(error.to_string().contains("format_invalid"), "{error}");
        assert_eq!(read(&paths, "a.md"), RECORD);
    }

    #[test]
    fn a_valid_refactor_rewrites_records_and_ordinary_notes_together() {
        let (_dir, paths) = vault();
        let report = bulk_replace_on_paths(
            &paths,
            &["a.md".to_string(), "Plain.md".to_string()],
            "Plain",
            "Simple",
            false,
            None,
        )
        .unwrap();
        assert_eq!(report.files.len(), 2);
        assert!(!report.dry_run);
        assert!(read(&paths, "a.md").contains("See Simple."));
        assert!(read(&paths, "Plain.md").contains("# Simple"));

        let report =
            set_note_property(&paths, "a", "due", Some("2026-11-20"), false, None).unwrap();
        assert_eq!(report.files.len(), 1);
        assert!(read(&paths, "a.md").contains("due: 2026-11-20"));
        assert!(set_note_property(&paths, "a", "due", Some("later"), false, None).is_err());
    }
}
