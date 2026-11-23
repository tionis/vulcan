//! Permission-checked MCP access to vault Git history and commits.

use std::collections::BTreeSet;

use serde_json::Value;
use vulcan_core::{PermissionGuard, ProfilePermissionGuard, VaultPaths, Verbosity};

use crate::git_ops;
use crate::mcp_protocol::{
    McpGitBlameArgs, McpGitCommitArgs, McpGitDiffArgs, McpGitLogArgs, McpMethodError,
};
use crate::mcp_session::McpClientAttribution;

const DEFAULT_LOG_LIMIT: usize = 20;
const MAX_LOG_LIMIT: usize = 200;

fn tool_error(error: impl std::fmt::Display) -> McpMethodError {
    McpMethodError::tool(error.to_string())
}

fn to_value(report: impl serde::Serialize) -> Result<Value, McpMethodError> {
    serde_json::to_value(report).map_err(|error| McpMethodError::internal(error.to_string()))
}

pub fn git_status(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
) -> Result<Value, McpMethodError> {
    guard.check_git().map_err(tool_error)?;
    to_value(git_ops::status_report(paths).map_err(tool_error)?)
}

pub fn git_diff(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    args: &McpGitDiffArgs,
) -> Result<Value, McpMethodError> {
    guard.check_git().map_err(tool_error)?;
    to_value(git_ops::diff_report(paths, args.path.as_deref()).map_err(tool_error)?)
}

pub fn git_log(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    args: &McpGitLogArgs,
) -> Result<Value, McpMethodError> {
    guard.check_git().map_err(tool_error)?;
    let limit = args
        .limit
        .unwrap_or(DEFAULT_LOG_LIMIT)
        .clamp(1, MAX_LOG_LIMIT);
    to_value(git_ops::log_report(paths, limit).map_err(tool_error)?)
}

pub fn git_blame(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    args: &McpGitBlameArgs,
) -> Result<Value, McpMethodError> {
    guard.check_git().map_err(tool_error)?;
    let path = git_ops::normalize_git_scope_path(&args.path);
    guard.check_read_path(&path).map_err(tool_error)?;
    to_value(git_ops::blame_report(paths, &path).map_err(tool_error)?)
}

/// Commit this session's writes (the default), the named `paths`, or with
/// `scope: "all"` every vault change, attributed to the MCP client.
pub fn git_commit(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    profile_name: &str,
    client: &McpClientAttribution,
    written_paths: &mut BTreeSet<String>,
    args: &McpGitCommitArgs,
) -> Result<Value, McpMethodError> {
    guard.check_git().map_err(tool_error)?;
    let scope = args.scope.as_deref().unwrap_or(if args.paths.is_some() {
        "paths"
    } else {
        "session"
    });
    let only =
        match (scope, &args.paths) {
            ("session", None) => Some(written_paths.iter().cloned().collect::<Vec<_>>()),
            ("paths", Some(named)) => Some(
                named
                    .iter()
                    .map(|path| path.trim().trim_start_matches("./").to_string())
                    .collect(),
            ),
            ("all", None) => None,
            _ => return Err(McpMethodError::invalid_params(
                "`git_commit.scope` is \"session\" (default), \"all\", or \"paths\" with `paths`",
            )),
        };
    if only.as_ref().is_some_and(Vec::is_empty) {
        return Ok(serde_json::json!({
            "committed": false,
            "scope": scope,
            "message": "this session has no uncommitted writes; pass scope \"all\" to commit every vault change",
            "files": [],
        }));
    }
    let trailers = client.trailers(profile_name);
    let report = git_ops::commit_request_with_hooks(
        paths,
        Some(profile_name),
        &vulcan_core::GitCommitRequest {
            message: &args.message,
            only_paths: only.as_deref(),
            trailers: &trailers,
        },
        Verbosity::Quiet,
    )
    .map_err(tool_error)?;
    // Forget writes that are committed now, by this or any other commit.
    let changed = vulcan_core::git_status(paths.vault_root())
        .map(|status| status.changed_paths().into_iter().collect::<BTreeSet<_>>())
        .map_err(tool_error)?;
    written_paths.retain(|path| changed.contains(path));
    let mut value = to_value(report)?;
    if let Some(object) = value.as_object_mut() {
        object.insert("scope".to_string(), Value::from(scope));
        if !written_paths.is_empty() {
            object.insert(
                "session_uncommitted".to_string(),
                Value::from(written_paths.iter().cloned().collect::<Vec<_>>()),
            );
        }
    }
    Ok(value)
}
