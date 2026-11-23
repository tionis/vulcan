//! Permission-checked MCP access to vault Git history and commits.

use serde_json::Value;
use vulcan_core::{PermissionGuard, ProfilePermissionGuard, VaultPaths, Verbosity};

use crate::git_ops;
use crate::mcp_protocol::{
    McpGitBlameArgs, McpGitCommitArgs, McpGitDiffArgs, McpGitLogArgs, McpMethodError,
};

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

pub fn git_commit(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    profile_name: &str,
    args: &McpGitCommitArgs,
) -> Result<Value, McpMethodError> {
    guard.check_git().map_err(tool_error)?;
    to_value(
        git_ops::commit_with_hooks(paths, Some(profile_name), &args.message, Verbosity::Quiet)
            .map_err(tool_error)?,
    )
}
