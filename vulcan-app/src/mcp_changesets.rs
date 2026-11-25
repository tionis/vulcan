//! MCP changeset tools: stage edits in a worktree outside the vault, then
//! land or abandon them (see [`crate::changesets`]). Tools work in one by
//! naming it in their `changeset` argument.

use serde_json::{json, Value};
use vulcan_core::{PermissionGuard, ProfilePermissionGuard, VaultPaths};

use crate::changesets::{self, ChangesetLandCommit};
use crate::mcp_authority::McpClientAttribution;
use crate::mcp_protocol::{
    McpChangesetBeginArgs, McpChangesetIdArgs, McpChangesetLandArgs, McpMethodError,
};

fn tool_error(error: impl std::fmt::Display) -> McpMethodError {
    McpMethodError::tool(error.to_string())
}

fn to_value(report: impl serde::Serialize) -> Result<Value, McpMethodError> {
    serde_json::to_value(report).map_err(|error| McpMethodError::internal(error.to_string()))
}

pub fn changeset_begin(
    vault: &VaultPaths,
    guard: &ProfilePermissionGuard,
    args: &McpChangesetBeginArgs,
) -> Result<Value, McpMethodError> {
    guard.check_git().map_err(tool_error)?;
    let changeset =
        changesets::begin_changeset(vault, args.title.as_deref()).map_err(tool_error)?;
    to_value(&changeset)
}

pub fn changeset_list(
    vault: &VaultPaths,
    guard: &ProfilePermissionGuard,
) -> Result<Value, McpMethodError> {
    guard.check_git().map_err(tool_error)?;
    let pruned = changesets::prune_empty_changesets(vault);
    let changesets = changesets::list_changesets(vault).map_err(tool_error)?;
    let mut value = json!({ "changesets": changesets });
    if !pruned.is_empty() {
        value["pruned"] = json!(pruned
            .iter()
            .map(|changeset| changeset.id.as_str())
            .collect::<Vec<_>>());
    }
    Ok(value)
}

pub fn changeset_land(
    vault: &VaultPaths,
    guard: &ProfilePermissionGuard,
    profile_name: &str,
    client: &McpClientAttribution,
    args: &McpChangesetLandArgs,
) -> Result<Value, McpMethodError> {
    guard.check_git().map_err(tool_error)?;
    let trailers = client.trailers(profile_name);
    let commit = args.message.as_deref().map(|message| ChangesetLandCommit {
        message,
        trailers: &trailers,
        permission_profile: Some(profile_name),
    });
    let report =
        changesets::land_changeset(vault, &args.id, commit.as_ref()).map_err(tool_error)?;
    to_value(report)
}

pub fn changeset_abandon(
    vault: &VaultPaths,
    guard: &ProfilePermissionGuard,
    args: &McpChangesetIdArgs,
) -> Result<Value, McpMethodError> {
    guard.check_git().map_err(tool_error)?;
    let changeset = changesets::abandon_changeset(vault, &args.id).map_err(tool_error)?;
    Ok(json!({ "abandoned": changeset }))
}

/// The worktree of the changeset a tool call names, with a guard for it
/// under the caller's permission selection. Working in a changeset needs Git
/// access, since its work only reaches the vault by landing.
pub fn changeset_worktree(
    vault: &VaultPaths,
    guard: &ProfilePermissionGuard,
    id: &str,
) -> Result<(VaultPaths, ProfilePermissionGuard), McpMethodError> {
    guard.check_git().map_err(tool_error)?;
    let changeset = changesets::find_changeset(vault, id).map_err(|error| {
        McpMethodError::tool_coded(
            crate::mcp_protocol::McpToolErrorCode::NotFound,
            format!("{error}; `changeset_list` shows the open changesets (a landed or abandoned one is gone)"),
            Some("changeset".to_string()),
        )
    })?;
    let paths = changesets::changeset_paths(&changeset);
    let guard = ProfilePermissionGuard::new(&paths, guard.selection().clone());
    Ok((paths, guard))
}
