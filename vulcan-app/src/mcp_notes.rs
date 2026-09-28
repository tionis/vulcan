//! Permission-checked MCP note mutation workflows shared by transports.

use serde_json::Value;
use std::collections::BTreeMap;
use vulcan_core::paths::{normalize_relative_input_path, RelativePathOptions};
use vulcan_core::{PermissionGuard, ProfilePermissionGuard, VaultPaths};

use crate::commit::AutoCommitPolicy;
use crate::mcp_access::{check_write_note_access, check_write_path_access};
use crate::mcp_assistant::json_value_to_string;
use crate::mcp_protocol::{McpMethodError, McpNoteCreateArgs, McpNoteDeleteArgs, McpNoteSetArgs};
use crate::notes::{
    apply_note_create, apply_note_delete, apply_note_set, finish_note_create_report,
    finish_note_set_report, parse_note_frontmatter_bindings, NoteCreateCommandReport,
    NoteCreateRequest, NoteDeleteReport, NoteDeleteRequest, NoteSetCommandReport, NoteSetRequest,
};
use crate::scan::refresh_cache_incrementally;

pub fn note_create(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    profile_name: &str,
    args: McpNoteCreateArgs,
) -> Result<NoteCreateCommandReport, McpMethodError> {
    let normalized_path = normalize_relative_input_path(
        &args.path,
        RelativePathOptions {
            expected_extension: Some("md"),
            append_extension_if_missing: true,
        },
    )
    .map_err(|error| McpMethodError::tool(error.to_string()))?;
    check_write_path_access(guard, &normalized_path)?;
    let frontmatter = parse_note_frontmatter_bindings(&frontmatter_bindings(&args.frontmatter))
        .map_err(|error| McpMethodError::tool(error.to_string()))?;
    let applied = apply_note_create(
        paths,
        &NoteCreateRequest {
            path: normalized_path,
            template: args.template,
            frontmatter,
            body: args.body,
        },
        Some(profile_name),
        true,
    )
    .map_err(|error| McpMethodError::tool(error.to_string()))?;
    let report = finish_note_create_report(paths, applied, args.check)
        .map_err(|error| McpMethodError::tool(error.to_string()))?;
    refresh_cache_incrementally(paths).map_err(|error| McpMethodError::tool(error.to_string()))?;
    AutoCommitPolicy::for_mutation(paths, args.no_commit)
        .commit(
            paths,
            "note-create",
            &report.changed_paths,
            Some(profile_name),
            true,
        )
        .map_err(|error| McpMethodError::tool(error.clone()))?;
    Ok(report)
}

fn frontmatter_bindings(frontmatter: &BTreeMap<String, Value>) -> Vec<String> {
    frontmatter
        .iter()
        .map(|(key, value)| format!("{key}={}", json_value_to_string(value)))
        .collect()
}

pub fn note_set(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    profile_name: &str,
    args: McpNoteSetArgs,
) -> Result<NoteSetCommandReport, McpMethodError> {
    if !args.confirm {
        return Err(McpMethodError::invalid_params(
            "`note_set.confirm` must be true because this replaces the full note body",
        ));
    }
    check_write_note_access(paths, guard, &args.note)?;
    let applied = apply_note_set(
        paths,
        &NoteSetRequest {
            note: args.note,
            replacement: args.content,
            preserve_frontmatter: args.preserve_frontmatter,
        },
        Some(profile_name),
        true,
    )
    .map_err(|error| McpMethodError::tool(error.to_string()))?;
    let report = finish_note_set_report(paths, applied, args.check)
        .map_err(|error| McpMethodError::tool(error.to_string()))?;
    refresh_cache_incrementally(paths).map_err(|error| McpMethodError::tool(error.to_string()))?;
    AutoCommitPolicy::for_mutation(paths, args.no_commit)
        .commit(
            paths,
            "note-set",
            std::slice::from_ref(&report.path),
            Some(profile_name),
            true,
        )
        .map_err(|error| McpMethodError::tool(error.clone()))?;
    Ok(report)
}

pub fn note_delete(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    profile_name: &str,
    args: McpNoteDeleteArgs,
) -> Result<NoteDeleteReport, McpMethodError> {
    if !args.dry_run && !args.confirm {
        return Err(McpMethodError::invalid_params(
            "`note_delete.confirm` must be true unless `dry_run` is true",
        ));
    }
    check_write_note_access(paths, guard, &args.note)?;
    let mut report = apply_note_delete(
        paths,
        &NoteDeleteRequest {
            note: args.note,
            dry_run: args.dry_run,
        },
        Some(profile_name),
        true,
    )
    .map_err(|error| McpMethodError::tool(error.to_string()))?;
    if !args.dry_run {
        refresh_cache_incrementally(paths)
            .map_err(|error| McpMethodError::tool(error.to_string()))?;
        AutoCommitPolicy::for_mutation(paths, args.no_commit)
            .commit(
                paths,
                "note-delete",
                &report.changed_paths,
                Some(profile_name),
                true,
            )
            .map_err(|error| McpMethodError::tool(error.clone()))?;
    }
    report
        .backlinks
        .retain(|backlink| guard.check_read_path(&backlink.source_path).is_ok());
    report.backlink_count = report.backlinks.len();
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;
    use vulcan_core::paths::initialize_vulcan_dir;
    use vulcan_core::{resolve_permission_profile, scan_vault, ScanMode};

    #[test]
    fn create_normalizes_markdown_path_and_refuses_readonly_grant() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(temporary.path());
        initialize_vulcan_dir(&paths).unwrap();
        let readonly = ProfilePermissionGuard::new(
            &paths,
            resolve_permission_profile(&paths, Some("readonly")).unwrap(),
        );
        let writable = ProfilePermissionGuard::new(
            &paths,
            resolve_permission_profile(&paths, Some("unrestricted")).unwrap(),
        );
        let args = || {
            serde_json::from_value::<McpNoteCreateArgs>(json!({
                "path": "New", "body": "# New\n", "frontmatter": {"status": "draft"},
                "no_commit": true,
            }))
            .unwrap()
        };
        assert!(note_create(&paths, &readonly, "readonly", args()).is_err());
        assert!(!temporary.path().join("New.md").exists());
        let report = note_create(&paths, &writable, "unrestricted", args()).unwrap();
        assert_eq!(report.path, "New.md");
        let content = fs::read_to_string(temporary.path().join("New.md")).unwrap();
        assert!(content.contains("status: draft"));
        assert!(content.contains("# New"));
    }

    #[test]
    fn set_and_delete_require_confirmation_and_writable_grant() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(temporary.path());
        initialize_vulcan_dir(&paths).unwrap();
        fs::write(temporary.path().join("Home.md"), "# Original\n").unwrap();
        scan_vault(&paths, ScanMode::Full).unwrap();
        let readonly = ProfilePermissionGuard::new(
            &paths,
            resolve_permission_profile(&paths, Some("readonly")).unwrap(),
        );
        let writable = ProfilePermissionGuard::new(
            &paths,
            resolve_permission_profile(&paths, Some("unrestricted")).unwrap(),
        );
        let set_args = |confirm| {
            serde_json::from_value::<McpNoteSetArgs>(json!({
                "note": "Home.md", "content": "# Replaced\n", "confirm": confirm,
                "no_commit": true,
            }))
            .unwrap()
        };
        assert!(note_set(&paths, &writable, "unrestricted", set_args(false)).is_err());
        assert!(note_set(&paths, &readonly, "readonly", set_args(true)).is_err());
        assert_eq!(
            fs::read_to_string(temporary.path().join("Home.md")).unwrap(),
            "# Original\n"
        );
        note_set(&paths, &writable, "unrestricted", set_args(true)).unwrap();
        assert_eq!(
            fs::read_to_string(temporary.path().join("Home.md")).unwrap(),
            "# Replaced\n"
        );
        let delete_args = |dry_run, confirm| {
            serde_json::from_value::<McpNoteDeleteArgs>(json!({
                "note": "Home.md", "dry_run": dry_run, "confirm": confirm,
                "no_commit": true,
            }))
            .unwrap()
        };
        assert!(note_delete(&paths, &writable, "unrestricted", delete_args(false, false)).is_err());
        assert!(note_delete(&paths, &readonly, "readonly", delete_args(false, true)).is_err());
        assert!(
            note_delete(&paths, &writable, "unrestricted", delete_args(true, false))
                .unwrap()
                .dry_run
        );
        assert!(temporary.path().join("Home.md").exists());
        assert!(
            note_delete(&paths, &writable, "unrestricted", delete_args(false, true))
                .unwrap()
                .deleted
        );
        assert!(!temporary.path().join("Home.md").exists());
    }
}
