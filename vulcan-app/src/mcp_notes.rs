//! Permission-checked MCP note mutation workflows shared by transports.

use serde_json::Value;
use std::collections::BTreeMap;
use vulcan_core::paths::{normalize_relative_input_path, RelativePathOptions};
use vulcan_core::Verbosity;
use vulcan_core::{load_vault_config, PermissionGuard, ProfilePermissionGuard, VaultPaths};

use crate::commit::AutoCommitPolicy;
use crate::mcp_access::{
    check_write_markdown_source_access, check_write_note_access, check_write_path_access,
};
use crate::mcp_assistant::json_value_to_string;
use crate::mcp_protocol::{
    McpMethodError, McpNoteAppendArgs, McpNoteCreateArgs, McpNoteDeleteArgs, McpNoteMoveArgs,
    McpNotePatchArgs, McpNoteSetArgs,
};
use crate::notes::{
    apply_note_append_expecting, apply_note_create, apply_note_delete_expecting,
    apply_note_patch_expecting, apply_note_set_expecting, finish_note_append_report,
    finish_note_create_report, finish_note_patch_report, finish_note_set_report,
    parse_note_frontmatter_bindings, resolve_existing_markdown_target, NoteAppendCommandReport,
    NoteAppendMode, NoteAppendRequest, NoteCreateCommandReport, NoteCreateRequest,
    NoteDeleteReport, NoteDeleteRequest, NotePatchCommandReport, NotePatchRequest,
    NoteSetCommandReport, NoteSetRequest,
};
use crate::periodic::resolve_periodic_target;
use crate::scan::refresh_cache_for_paths;
use crate::templates::parse_template_var_bindings;

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
        Verbosity::Quiet,
    )
    .map_err(|error| McpMethodError::tool(error.to_string()))?;
    let report = finish_note_create_report(paths, applied, args.check)
        .map_err(|error| McpMethodError::tool(error.to_string()))?;
    refresh_cache_for_paths(paths, &report.changed_paths)
        .map_err(|error| McpMethodError::tool(error.to_string()))?;
    AutoCommitPolicy::for_mutation(paths, args.no_commit)
        .commit(
            paths,
            "note-create",
            &report.changed_paths,
            Some(profile_name),
            Verbosity::Quiet,
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

pub fn note_append(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    profile_name: &str,
    args: McpNoteAppendArgs,
) -> Result<NoteAppendCommandReport, McpMethodError> {
    let periodic = parse_periodic_arg(args.periodic.clone())?;
    if args.note.is_some() == periodic.is_some() {
        return Err(McpMethodError::invalid_params(
            "`note_append` requires exactly one of `note` or `periodic`",
        ));
    }
    if let Some(note) = args.note.as_deref() {
        check_write_note_access(paths, guard, note)?;
    } else if let Some(periodic) = periodic.as_deref() {
        let config = load_vault_config(paths).config;
        let target =
            resolve_periodic_target(&config.periodic, periodic, args.date.as_deref(), true)
                .map_err(|error| McpMethodError::tool(error.to_string()))?;
        check_write_path_access(guard, &target.path)?;
    }
    let vars = parse_template_var_bindings(&template_var_bindings(&args.vars))
        .map_err(|error| McpMethodError::tool(error.to_string()))?;
    let idempotency = match args.idempotency_key.as_deref() {
        Some(key) if !args.dry_run => {
            let fingerprint = append_fingerprint(&args, periodic.as_deref());
            if let Some(report) = append_ledger::replay(paths, key, &fingerprint)? {
                return Ok(report);
            }
            Some((key.to_string(), fingerprint))
        }
        _ => None,
    };
    let applied = apply_note_append_expecting(
        paths,
        &NoteAppendRequest {
            note: args.note,
            text: args.text,
            mode: parse_note_append_mode(args.mode.as_deref(), args.heading.is_some())?,
            heading: args.heading,
            periodic,
            date: args.date,
            vars,
            dry_run: args.dry_run,
        },
        args.expected_hash.as_deref(),
        Some(profile_name),
        Verbosity::Quiet,
    )
    .map_err(|error| McpMethodError::tool(error.to_string()))?;
    let changed = applied.changed_paths.clone();
    let report = finish_note_append_report(paths, applied, args.check)
        .map_err(|error| McpMethodError::tool(error.to_string()))?;
    if report.dry_run {
        return Ok(report);
    }
    if let Some((key, fingerprint)) = idempotency {
        append_ledger::record(paths, key, fingerprint, &report);
    }
    refresh_cache_for_paths(paths, &changed)
        .map_err(|error| McpMethodError::tool(error.to_string()))?;
    AutoCommitPolicy::for_mutation(paths, args.no_commit)
        .commit(
            paths,
            "note-append",
            std::slice::from_ref(&report.path),
            Some(profile_name),
            Verbosity::Quiet,
        )
        .map_err(|error| McpMethodError::tool(error.clone()))?;
    Ok(report)
}

/// What a repeated idempotency key must match: the append's target and text.
fn append_fingerprint(args: &McpNoteAppendArgs, periodic: Option<&str>) -> String {
    let value = serde_json::json!([
        args.note,
        args.text,
        args.mode,
        args.heading,
        periodic,
        args.date,
        args.vars,
    ]);
    blake3::hash(value.to_string().as_bytes())
        .to_hex()
        .to_string()
}

/// Appends made under an idempotency key in this process, so a client that
/// retries after a lost response does not append twice. Bounded in size
/// and age; a retry after a restart or a day appends again.
mod append_ledger {
    use super::{McpMethodError, NoteAppendCommandReport, VaultPaths};
    use std::collections::VecDeque;
    use std::path::PathBuf;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    const CAPACITY: usize = 1024;
    const MAX_AGE: Duration = Duration::from_hours(24);

    struct Entry {
        vault: PathBuf,
        key: String,
        fingerprint: String,
        recorded: Instant,
        report: NoteAppendCommandReport,
    }

    static LEDGER: Mutex<VecDeque<Entry>> = Mutex::new(VecDeque::new());

    pub(super) fn replay(
        paths: &VaultPaths,
        key: &str,
        fingerprint: &str,
    ) -> Result<Option<NoteAppendCommandReport>, McpMethodError> {
        if key.is_empty() || key.len() > 256 {
            return Err(McpMethodError::invalid_params(
                "`note_append.idempotency_key` must be 1 to 256 bytes",
            ));
        }
        let mut ledger = LEDGER
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ledger.retain(|entry| entry.recorded.elapsed() < MAX_AGE);
        let Some(entry) = ledger
            .iter()
            .find(|entry| entry.vault == paths.vault_root() && entry.key == key)
        else {
            return Ok(None);
        };
        if entry.fingerprint != fingerprint {
            return Err(McpMethodError::invalid_params(
                "`note_append.idempotency_key` was already used for a different append",
            ));
        }
        let mut report = entry.report.clone();
        report.replayed = true;
        Ok(Some(report))
    }

    pub(super) fn record(
        paths: &VaultPaths,
        key: String,
        fingerprint: String,
        report: &NoteAppendCommandReport,
    ) {
        let mut ledger = LEDGER
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if ledger.len() >= CAPACITY {
            ledger.pop_front();
        }
        ledger.push_back(Entry {
            vault: paths.vault_root().to_path_buf(),
            key,
            fingerprint,
            recorded: Instant::now(),
            report: report.clone(),
        });
    }
}

fn parse_note_append_mode(
    mode: Option<&str>,
    has_heading: bool,
) -> Result<NoteAppendMode, McpMethodError> {
    match mode {
        None | Some("after_heading") if has_heading => Ok(NoteAppendMode::AfterHeading),
        None | Some("append") => Ok(NoteAppendMode::Append),
        Some("prepend") => Ok(NoteAppendMode::Prepend),
        Some("after_heading") => Err(McpMethodError::invalid_params(
            "`note_append.mode = after_heading` requires `heading`",
        )),
        Some(other) => Err(McpMethodError::invalid_params(format!(
            "unsupported `note_append.mode`: {other}"
        ))),
    }
}

fn parse_periodic_arg(value: Option<String>) -> Result<Option<String>, McpMethodError> {
    let Some(value) = value else {
        return Ok(None);
    };
    match value.as_str() {
        "daily" | "weekly" | "monthly" => Ok(Some(value)),
        other => Err(McpMethodError::invalid_params(format!(
            "unsupported `note_append.periodic`: {other}"
        ))),
    }
}

fn template_var_bindings(vars: &BTreeMap<String, String>) -> Vec<String> {
    vars.iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect()
}

pub fn note_patch(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    profile_name: &str,
    args: McpNotePatchArgs,
) -> Result<NotePatchCommandReport, McpMethodError> {
    check_write_markdown_source_access(paths, guard, &args.note)?;
    let request = NotePatchRequest {
        target: resolve_existing_markdown_target(paths, &args.note)
            .map_err(|error| McpMethodError::tool(error.to_string()))?,
        section_id: args.section_id,
        heading: args.heading,
        block_ref: args.block_ref,
        lines: args.lines,
        find: args.find,
        replace: args.replace,
        replace_all: args.all,
        dry_run: args.dry_run,
    };
    let applied = apply_note_patch_expecting(
        paths,
        &request,
        args.expected_hash.as_deref(),
        Some(profile_name),
        Verbosity::Quiet,
    )
    .map_err(|error| McpMethodError::tool(error.to_string()))?;
    if !applied.dry_run && !applied.changed_paths.is_empty() {
        refresh_cache_for_paths(paths, &applied.changed_paths)
            .map_err(|error| McpMethodError::tool(error.to_string()))?;
    }
    let report = finish_note_patch_report(paths, &request, applied, args.check)
        .map_err(|error| McpMethodError::tool(error.to_string()))?;
    if !args.dry_run {
        AutoCommitPolicy::for_mutation(paths, args.no_commit)
            .commit(
                paths,
                "note-patch",
                std::slice::from_ref(&report.path),
                Some(profile_name),
                Verbosity::Quiet,
            )
            .map_err(|error| McpMethodError::tool(error.clone()))?;
    }
    Ok(report)
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
    let applied = apply_note_set_expecting(
        paths,
        &NoteSetRequest {
            note: args.note,
            replacement: args.content,
            preserve_frontmatter: args.preserve_frontmatter,
        },
        args.expected_hash.as_deref(),
        Some(profile_name),
        Verbosity::Quiet,
    )
    .map_err(|error| McpMethodError::tool(error.to_string()))?;
    let changed = applied.changed_paths.clone();
    let report = finish_note_set_report(paths, applied, args.check)
        .map_err(|error| McpMethodError::tool(error.to_string()))?;
    refresh_cache_for_paths(paths, &changed)
        .map_err(|error| McpMethodError::tool(error.to_string()))?;
    AutoCommitPolicy::for_mutation(paths, args.no_commit)
        .commit(
            paths,
            "note-set",
            std::slice::from_ref(&report.path),
            Some(profile_name),
            Verbosity::Quiet,
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
    let mut report = apply_note_delete_expecting(
        paths,
        &NoteDeleteRequest {
            note: args.note,
            dry_run: args.dry_run,
        },
        args.expected_hash.as_deref(),
        Some(profile_name),
        Verbosity::Quiet,
    )
    .map_err(|error| McpMethodError::tool(error.to_string()))?;
    if !args.dry_run {
        refresh_cache_for_paths(paths, &report.changed_paths)
            .map_err(|error| McpMethodError::tool(error.to_string()))?;
        AutoCommitPolicy::for_mutation(paths, args.no_commit)
            .commit(
                paths,
                "note-delete",
                &report.changed_paths,
                Some(profile_name),
                Verbosity::Quiet,
            )
            .map_err(|error| McpMethodError::tool(error.clone()))?;
    }
    report
        .backlinks
        .retain(|backlink| guard.check_read_path(&backlink.source_path).is_ok());
    report.backlink_count = report.backlinks.len();
    Ok(report)
}

/// Moves or renames a note and rewrites references to it, as `note rename`
/// and `refactor move` do, under the connection's refactor and write grants.
pub fn note_move(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    profile_name: &str,
    args: &McpNoteMoveArgs,
) -> Result<vulcan_core::MoveSummary, McpMethodError> {
    let resolved = vulcan_core::resolve_note_reference(paths, &args.note)
        .map_err(|error| McpMethodError::tool(error.to_string()))?;
    guard
        .check_refactor_path(&resolved.path)
        .map_err(|error| McpMethodError::tool(error.to_string()))?;
    let destination = normalize_relative_input_path(
        &args.destination,
        RelativePathOptions {
            expected_extension: Some("md"),
            append_extension_if_missing: true,
        },
    )
    .map_err(|error| McpMethodError::tool(error.to_string()))?;
    check_write_path_access(guard, &destination)?;
    let summary = crate::browse::move_note_with_profile(
        paths,
        &resolved.path,
        &destination,
        args.dry_run,
        Some(profile_name),
    )
    .map_err(|error| McpMethodError::tool(error.to_string()))?;
    if !args.dry_run {
        let changed: Vec<String> = std::iter::once(summary.source_path.clone())
            .chain(std::iter::once(summary.destination_path.clone()))
            .chain(summary.rewritten_files.iter().map(|file| file.path.clone()))
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        AutoCommitPolicy::for_mutation(paths, args.no_commit)
            .commit(
                paths,
                "note-move",
                &changed,
                Some(profile_name),
                Verbosity::Quiet,
            )
            .map_err(|error| McpMethodError::tool(error.clone()))?;
    }
    Ok(summary)
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
    fn a_retried_append_with_the_same_key_appends_once() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(temporary.path());
        initialize_vulcan_dir(&paths).unwrap();
        fs::write(temporary.path().join("Log.md"), "# Log\n").unwrap();
        scan_vault(&paths, ScanMode::Full).unwrap();
        let writable = ProfilePermissionGuard::new(
            &paths,
            resolve_permission_profile(&paths, Some("unrestricted")).unwrap(),
        );
        let args = |text: &str| {
            serde_json::from_value::<McpNoteAppendArgs>(json!({
                "note": "Log.md", "text": text, "no_commit": true,
                "idempotency_key": "retry-test-1",
            }))
            .unwrap()
        };
        let first = note_append(&paths, &writable, "unrestricted", args("- once")).unwrap();
        assert!(!first.replayed);
        let again = note_append(&paths, &writable, "unrestricted", args("- once")).unwrap();
        assert!(again.replayed);
        let content = fs::read_to_string(temporary.path().join("Log.md")).unwrap();
        assert_eq!(content.matches("- once").count(), 1, "{content}");
        // The same key for a different append is a caller error.
        assert!(note_append(&paths, &writable, "unrestricted", args("- other")).is_err());
    }

    #[test]
    fn append_checks_target_authority_and_mode_before_mutation() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(temporary.path());
        initialize_vulcan_dir(&paths).unwrap();
        fs::write(temporary.path().join("Home.md"), "# Home\n").unwrap();
        scan_vault(&paths, ScanMode::Full).unwrap();
        let readonly = ProfilePermissionGuard::new(
            &paths,
            resolve_permission_profile(&paths, Some("readonly")).unwrap(),
        );
        let writable = ProfilePermissionGuard::new(
            &paths,
            resolve_permission_profile(&paths, Some("unrestricted")).unwrap(),
        );
        let args = || {
            serde_json::from_value::<McpNoteAppendArgs>(json!({
                "note": "Home.md", "text": "Appended text", "no_commit": true,
            }))
            .unwrap()
        };
        assert!(note_append(&paths, &readonly, "readonly", args()).is_err());
        assert_eq!(
            fs::read_to_string(temporary.path().join("Home.md")).unwrap(),
            "# Home\n"
        );
        let invalid = serde_json::from_value::<McpNoteAppendArgs>(json!({
            "note": "Home.md", "text": "ignored", "mode": "after_heading",
        }))
        .unwrap();
        assert!(matches!(
            note_append(&paths, &writable, "unrestricted", invalid),
            Err(McpMethodError::JsonRpc { code: -32602, .. })
        ));
        note_append(&paths, &writable, "unrestricted", args()).unwrap();
        assert!(fs::read_to_string(temporary.path().join("Home.md"))
            .unwrap()
            .contains("Appended text"));
    }

    #[test]
    fn patch_enforces_write_scope_and_keeps_dry_run_mutation_free() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(temporary.path());
        initialize_vulcan_dir(&paths).unwrap();
        let target = temporary.path().join("Home.md");
        fs::write(&target, "# Home\nold\n").unwrap();
        scan_vault(&paths, ScanMode::Full).unwrap();
        let readonly = ProfilePermissionGuard::new(
            &paths,
            resolve_permission_profile(&paths, Some("readonly")).unwrap(),
        );
        let writable = ProfilePermissionGuard::new(
            &paths,
            resolve_permission_profile(&paths, Some("unrestricted")).unwrap(),
        );
        let args = |dry_run| {
            serde_json::from_value::<McpNotePatchArgs>(json!({
                "note": "Home.md", "find": "old", "replace": "new",
                "dry_run": dry_run, "no_commit": true,
            }))
            .unwrap()
        };
        assert!(note_patch(&paths, &readonly, "readonly", args(false)).is_err());
        assert_eq!(fs::read_to_string(&target).unwrap(), "# Home\nold\n");
        let preview = note_patch(&paths, &writable, "unrestricted", args(true)).unwrap();
        assert!(preview.dry_run);
        assert_eq!(preview.match_count, 1);
        assert_eq!(fs::read_to_string(&target).unwrap(), "# Home\nold\n");
        let applied = note_patch(&paths, &writable, "unrestricted", args(false)).unwrap();
        assert!(!applied.dry_run);
        assert_eq!(fs::read_to_string(&target).unwrap(), "# Home\nnew\n");
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
