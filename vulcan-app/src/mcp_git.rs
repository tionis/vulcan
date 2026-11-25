//! Permission-checked MCP access to vault Git history and commits.

use serde_json::Value;
use vulcan_core::{PermissionGuard, ProfilePermissionGuard, VaultPaths, Verbosity};

use crate::git_ops;
use crate::mcp_protocol::{
    McpGitBlameArgs, McpGitCommitArgs, McpGitDiffArgs, McpGitLogArgs, McpMethodError,
};
use crate::mcp_session::McpClientAttribution;

const DEFAULT_LOG_LIMIT: usize = 20;
const MAX_LOG_LIMIT: usize = 200;
const MAX_LOG_OFFSET: usize = 2_000;
const DEFAULT_BLAME_LINES: usize = 200;
const MAX_BLAME_LINES: usize = 2_000;

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
    let limit = crate::mcp_paging::check_limit(
        "git_log",
        args.limit.unwrap_or(DEFAULT_LOG_LIMIT),
        MAX_LOG_LIMIT,
    )?;
    let offset = crate::mcp_paging::decode_cursor("git_log", args.cursor.as_deref())?;
    if offset > MAX_LOG_OFFSET {
        return Err(McpMethodError::invalid_argument(
            format!("git_log pages stop after {MAX_LOG_OFFSET} commits"),
            Some("cursor".to_string()),
        ));
    }
    let full = crate::mcp_paging::wants_full_detail("git_log", args.detail.as_deref())?;
    // One more than the page, to learn whether another page follows.
    let mut value = to_value(git_ops::log_report(paths, offset + limit + 1).map_err(tool_error)?)?;
    let entries = value
        .get_mut("entries")
        .and_then(Value::as_array_mut)
        .map_or_else(Vec::new, std::mem::take);
    let (start, end) = crate::mcp_paging::page_bounds(offset, limit, entries.len());
    let rows = entries[start..end]
        .iter()
        .map(|entry| {
            if full {
                entry.clone()
            } else {
                let mut row = crate::mcp_paging::concise_row(
                    entry,
                    &[
                        ("commit", "commit"),
                        ("committed_at", "committed_at"),
                        ("summary", "summary"),
                        ("author_name", "author"),
                    ],
                );
                if let Some(commit) = row.get("commit").and_then(Value::as_str) {
                    row["commit"] = Value::String(commit.chars().take(12).collect());
                }
                row
            }
        })
        .collect::<Vec<_>>();
    Ok(serde_json::json!({
        "entries": rows,
        "page": crate::mcp_paging::page_json(limit, start, end, None, entries.len() > end),
    }))
}

pub fn git_blame(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    args: &McpGitBlameArgs,
) -> Result<Value, McpMethodError> {
    guard.check_git().map_err(tool_error)?;
    let path = git_ops::normalize_git_scope_path(&args.path);
    guard.check_read_path(&path).map_err(tool_error)?;
    let limit = crate::mcp_paging::check_limit(
        "git_blame",
        args.limit.unwrap_or(DEFAULT_BLAME_LINES),
        MAX_BLAME_LINES,
    )?;
    let offset = crate::mcp_paging::decode_cursor("git_blame", args.cursor.as_deref())?;
    let full = crate::mcp_paging::wants_full_detail("git_blame", args.detail.as_deref())?;
    let report = to_value(git_ops::blame_report(paths, &path).map_err(tool_error)?)?;
    let lines = report
        .get("lines")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let (start, end) = crate::mcp_paging::page_bounds(offset, limit, lines.len());
    let page = &lines[start..end];
    let mut value = serde_json::json!({
        "path": path,
        "page": crate::mcp_paging::page_json(limit, start, end, Some(lines.len()), end < lines.len()),
    });
    if full {
        value["lines"] = Value::Array(page.to_vec());
    } else {
        value["ranges"] = Value::Array(blame_ranges(page));
    }
    Ok(value)
}

/// Consecutive lines from one commit folded into a range: the concise form
/// of a blame.
fn blame_ranges(lines: &[Value]) -> Vec<Value> {
    let mut ranges: Vec<Value> = Vec::new();
    for line in lines {
        let commit = line
            .get("commit")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let number = line
            .get("line_number")
            .and_then(Value::as_u64)
            .unwrap_or_default();
        if let Some(last) = ranges.last_mut() {
            let same = last.get("commit").and_then(Value::as_str)
                == Some(&commit.chars().take(12).collect::<String>()[..]);
            if same && last["end_line"].as_u64() == Some(number.saturating_sub(1)) {
                last["end_line"] = Value::from(number);
                continue;
            }
        }
        ranges.push(serde_json::json!({
            "start_line": number,
            "end_line": number,
            "commit": commit.chars().take(12).collect::<String>(),
            "author": line.get("author_name"),
            "author_time": line.get("author_time"),
            "summary": line.get("summary"),
        }));
    }
    ranges
}

/// Commit the named `paths`, every vault change (`all`), or everything in a
/// changeset worktree, attributed to the MCP client.
pub fn git_commit(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    profile_name: &str,
    client: &McpClientAttribution,
    in_changeset: bool,
    args: &McpGitCommitArgs,
) -> Result<Value, McpMethodError> {
    guard.check_git().map_err(tool_error)?;
    let only = match (&args.paths, args.all) {
        (Some(_), true) => {
            return Err(McpMethodError::invalid_argument(
                "pass `paths` or `all`, not both",
                Some("all".to_string()),
            ))
        }
        (Some(named), false) => {
            let named = named
                .iter()
                .map(|path| path.trim().trim_start_matches("./").to_string())
                .collect::<Vec<_>>();
            if named.is_empty() {
                return Err(McpMethodError::invalid_argument(
                    "`paths` names no file",
                    Some("paths".to_string()),
                ));
            }
            Some(named)
        }
        (None, true) => None,
        // A changeset worktree is private to whoever works in it.
        (None, false) if in_changeset => None,
        (None, false) => {
            return Err(McpMethodError::invalid_argument(
                "name the files to commit in `paths`, or pass `all: true` to commit every vault change (check `git_status` first: it includes other writers' edits)",
                Some("paths".to_string()),
            ))
        }
    };
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
    to_value(report)
}
