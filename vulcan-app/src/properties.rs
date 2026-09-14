//! Reusable property mutation workflows.

use crate::mdbase::{
    apply_managed_mdbase_note_writes, MdbaseManagedNoteWriteBatchRequest,
    MdbaseManagedNoteWriteChange, MdbaseManagedWriteMode, MdbaseWriteOperation,
};
use crate::AppError;
use std::path::Path;
use vulcan_core::mdbase::{is_mdbase_record_path, load_mdbase_collection};
use vulcan_core::paths::secure_write;
use vulcan_core::write_lock::acquire_write_lock;
use vulcan_core::{
    plan_property_mutations_on_paths, resolve_permission_profile, BulkMutationReport,
    PermissionGuard, ProfilePermissionGuard, RefactorFileReport, ScanMode, VaultPaths,
};

/// Plan and apply a bulk property set or unset while routing mdbase records
/// through their validated, crash-safe batch transaction.
pub fn apply_bulk_property_mutation(
    paths: &VaultPaths,
    note_paths: &[String],
    key: &str,
    value: Option<&str>,
    dry_run: bool,
    permission_profile: Option<&str>,
    quiet: bool,
) -> Result<BulkMutationReport, AppError> {
    let selection =
        resolve_permission_profile(paths, permission_profile).map_err(AppError::operation)?;
    let guard = ProfilePermissionGuard::new(paths, selection);
    for path in note_paths {
        guard
            .check_read_path(path)
            .and_then(|()| guard.check_write_path(path))
            .map_err(AppError::operation)?;
    }

    let planned = plan_property_mutations_on_paths(paths, note_paths, key, value)
        .map_err(AppError::operation)?;
    let collection = load_mdbase_collection(paths.vault_root()).map_err(AppError::operation)?;
    let mut managed_indexes = Vec::new();
    let mut ordinary_indexes = Vec::new();
    for (index, plan) in planned.iter().enumerate() {
        let managed = match &collection {
            Some(collection) => {
                is_mdbase_record_path(collection, &plan.path).map_err(AppError::operation)?
            }
            None => false,
        };
        if managed {
            managed_indexes.push(index);
        } else {
            ordinary_indexes.push(index);
        }
    }

    let managed_changes = managed_indexes
        .iter()
        .map(|index| {
            let plan = &planned[*index];
            MdbaseManagedNoteWriteChange {
                path: &plan.path,
                before: Some(&plan.before),
                after: Some(&plan.after),
            }
        })
        .collect::<Vec<_>>();
    if !managed_changes.is_empty() {
        apply_managed_property_batch(paths, &managed_changes, true, permission_profile, quiet)?;
    }

    if !dry_run {
        if !managed_changes.is_empty() {
            apply_managed_property_batch(
                paths,
                &managed_changes,
                false,
                permission_profile,
                quiet,
            )?;
        }
        if !ordinary_indexes.is_empty() {
            let _lock = acquire_write_lock(paths).map_err(AppError::operation)?;
            for index in ordinary_indexes {
                let plan = &planned[index];
                secure_write(paths.vault_root(), Path::new(&plan.path), &plan.after)
                    .map_err(AppError::operation)?;
            }
            vulcan_core::scan::scan_vault_unlocked(paths, ScanMode::Incremental)
                .map_err(AppError::operation)?;
        }
    }

    Ok(BulkMutationReport {
        dry_run,
        action: if value.is_some() {
            "bulk_update".to_string()
        } else {
            "bulk_unset".to_string()
        },
        filters: Vec::new(),
        key: key.to_string(),
        value: value.map(str::to_string),
        files: planned
            .into_iter()
            .map(|plan| RefactorFileReport {
                path: plan.path,
                changes: plan.changes,
            })
            .collect(),
    })
}

fn apply_managed_property_batch(
    paths: &VaultPaths,
    changes: &[MdbaseManagedNoteWriteChange<'_>],
    dry_run: bool,
    permission_profile: Option<&str>,
    quiet: bool,
) -> Result<(), AppError> {
    let operation = if changes.len() == 1 {
        MdbaseWriteOperation::Update
    } else {
        MdbaseWriteOperation::Batch
    };
    apply_managed_mdbase_note_writes(
        paths,
        &MdbaseManagedNoteWriteBatchRequest {
            changes,
            operation,
            mode: MdbaseManagedWriteMode::Validated,
            dry_run,
            permission_profile,
            quiet,
        },
    )?
    .ok_or_else(|| AppError::operation("mdbase property batch contained no managed records"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;
    use vulcan_core::mdbase::list_mdbase_write_outbox;

    fn fixture() -> (tempfile::TempDir, VaultPaths) {
        let directory = tempdir().expect("temporary directory");
        fs::write(
            directory.path().join("mdbase.yaml"),
            "spec_version: \"0.3.0\"\nsettings:\n  exclude: [Archive/**]\n",
        )
        .expect("collection config");
        fs::create_dir_all(directory.path().join("_types")).expect("type directory");
        fs::write(
            directory.path().join("_types/task.md"),
            "---\nkind: mdbase.type\nname: task\nschema:\n  dialect: json-schema-2020-12\n  value:\n    type: object\n    required: [type, title]\n    properties:\n      type: {const: task}\n      title: {type: string}\n      status: {type: string}\n---\n",
        )
        .expect("task type");
        fs::create_dir_all(directory.path().join("tasks")).expect("records directory");
        for (path, title) in [("tasks/one.md", "One"), ("tasks/two.md", "Two")] {
            fs::write(
                directory.path().join(path),
                format!("---\ntype: task\ntitle: {title}\n---\nBody\n"),
            )
            .expect("record");
        }
        fs::create_dir_all(directory.path().join("Archive")).expect("archive directory");
        fs::write(
            directory.path().join("Archive/ordinary.md"),
            "---\ntitle: Ordinary\n---\nBody\n",
        )
        .expect("ordinary note");
        let paths = VaultPaths::new(directory.path());
        (directory, paths)
    }

    #[test]
    fn managed_property_updates_commit_as_one_validated_batch() {
        let (directory, paths) = fixture();
        let report = apply_bulk_property_mutation(
            &paths,
            &["tasks/one.md".to_string(), "tasks/two.md".to_string()],
            "status",
            Some("done"),
            false,
            None,
            true,
        )
        .expect("property batch should succeed");

        assert_eq!(report.files.len(), 2);
        for path in ["tasks/one.md", "tasks/two.md"] {
            assert!(fs::read_to_string(directory.path().join(path))
                .expect("updated record")
                .contains("status: done"));
        }
        let outbox = list_mdbase_write_outbox(&paths).expect("outbox");
        assert_eq!(outbox.len(), 1);
        assert_eq!(outbox[0].operation, "batch");
        assert_eq!(outbox[0].paths.len(), 2);
    }

    #[test]
    fn invalid_managed_property_batch_fails_before_any_write() {
        let (directory, paths) = fixture();
        let before_one = fs::read_to_string(directory.path().join("tasks/one.md")).unwrap();
        let before_two = fs::read_to_string(directory.path().join("tasks/two.md")).unwrap();

        let error = apply_bulk_property_mutation(
            &paths,
            &["tasks/one.md".to_string(), "tasks/two.md".to_string()],
            "title",
            None,
            false,
            None,
            true,
        )
        .expect_err("required property removal should fail");

        assert!(error.message().contains("schema_required"));
        assert_eq!(
            fs::read_to_string(directory.path().join("tasks/one.md")).unwrap(),
            before_one
        );
        assert_eq!(
            fs::read_to_string(directory.path().join("tasks/two.md")).unwrap(),
            before_two
        );
        assert!(!directory.path().join(".vulcan").exists());
    }

    #[test]
    fn mixed_property_update_journals_records_and_preserves_ordinary_behavior() {
        let (directory, paths) = fixture();
        let report = apply_bulk_property_mutation(
            &paths,
            &[
                "tasks/one.md".to_string(),
                "Archive/ordinary.md".to_string(),
            ],
            "status",
            Some("active"),
            false,
            None,
            true,
        )
        .expect("mixed update should succeed");

        assert_eq!(report.files.len(), 2);
        assert!(fs::read_to_string(directory.path().join("tasks/one.md"))
            .unwrap()
            .contains("status: active"));
        assert!(
            fs::read_to_string(directory.path().join("Archive/ordinary.md"))
                .unwrap()
                .contains("status: active")
        );
        let outbox = list_mdbase_write_outbox(&paths).expect("outbox");
        assert_eq!(outbox.len(), 1);
        assert_eq!(outbox[0].operation, "update");
        assert_eq!(outbox[0].paths[0].path, "tasks/one.md");
    }
}
