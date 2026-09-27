//! MCP note and path preflight checks shared by local and hosted executors.

use crate::mcp_protocol::McpMethodError;
use crate::notes::resolve_existing_markdown_target;
use vulcan_core::{resolve_note_reference, PermissionGuard, ProfilePermissionGuard, VaultPaths};

pub fn check_read_note_access(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    note: &str,
) -> Result<(), McpMethodError> {
    if guard.read_filter().path_permission().is_unrestricted() && !guard.has_policy_hook() {
        return Ok(());
    }
    let resolved = resolve_note_reference(paths, note)
        .map_err(|error| McpMethodError::tool(error.to_string()))?;
    guard
        .check_read_path(&resolved.path)
        .map_err(|error| McpMethodError::tool(error.to_string()))
}

pub fn check_write_note_access(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    note: &str,
) -> Result<(), McpMethodError> {
    if guard.write_filter().path_permission().is_unrestricted() && !guard.has_policy_hook() {
        return Ok(());
    }
    let resolved = resolve_note_reference(paths, note)
        .map_err(|error| McpMethodError::tool(error.to_string()))?;
    guard
        .check_write_path(&resolved.path)
        .map_err(|error| McpMethodError::tool(error.to_string()))
}

pub fn check_write_path_access(
    guard: &ProfilePermissionGuard,
    path: &str,
) -> Result<(), McpMethodError> {
    if guard.write_filter().path_permission().is_unrestricted() && !guard.has_policy_hook() {
        return Ok(());
    }
    guard
        .check_write_path(path)
        .map_err(|error| McpMethodError::tool(error.to_string()))
}

pub fn check_write_markdown_source_access(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    note: &str,
) -> Result<(), McpMethodError> {
    if guard.write_filter().path_permission().is_unrestricted() && !guard.has_policy_hook() {
        return Ok(());
    }
    let target = resolve_existing_markdown_target(paths, note)
        .map_err(|error| McpMethodError::tool(error.to_string()))?;
    let Some(relative_path) = target.vault_relative_path.as_deref() else {
        return Err(McpMethodError::tool(format!(
            "permission profiles cannot write markdown files outside the selected vault root: {}",
            target.display_path
        )));
    };
    guard
        .check_write_path(relative_path)
        .map_err(|error| McpMethodError::tool(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;
    use vulcan_core::resolve_permission_profile;

    #[test]
    fn readonly_guard_rejects_note_and_path_writes() {
        let temporary = tempdir().expect("temporary vault");
        let paths = VaultPaths::new(temporary.path());
        fs::write(temporary.path().join("Note.md"), "# Note\n").expect("note");
        let selection =
            resolve_permission_profile(&paths, Some("readonly")).expect("readonly profile");
        let guard = ProfilePermissionGuard::new(&paths, selection);
        assert!(check_read_note_access(&paths, &guard, "Note.md").is_ok());
        assert!(check_write_note_access(&paths, &guard, "Note.md").is_err());
        assert!(check_write_path_access(&guard, "New.md").is_err());
        assert!(check_write_markdown_source_access(&paths, &guard, "Note.md").is_err());
    }
}
