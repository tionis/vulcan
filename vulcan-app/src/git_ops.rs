//! Vault-scoped Git inspection and commits shared by the CLI and MCP.
//!
//! Paths are vault-relative, and Vulcan's own `.vulcan/` state is never
//! reported as a change.

use serde::Serialize;
use vulcan_core::{
    git_blame, git_diff, git_recent_log, git_status, GitBlameLine, GitCommitReport, GitLogEntry,
    GitStatusReport, PluginEvent, VaultPaths, Verbosity,
};

use crate::AppError;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitLogReport {
    pub limit: usize,
    pub entries: Vec<GitLogEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitDiffReport {
    pub path: Option<String>,
    pub changed_paths: Vec<String>,
    pub diff: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitBlameReport {
    pub path: String,
    pub lines: Vec<GitBlameLine>,
}

#[must_use]
pub fn normalize_git_scope_path(path: &str) -> String {
    path.replace('\\', "/").trim_start_matches("./").to_string()
}

fn without_vulcan_state(paths: Vec<String>) -> Vec<String> {
    paths
        .into_iter()
        .filter(|path| path != ".vulcan" && !path.starts_with(".vulcan/"))
        .collect()
}

pub fn status_report(paths: &VaultPaths) -> Result<GitStatusReport, AppError> {
    let mut report = git_status(paths.vault_root()).map_err(AppError::operation)?;
    report.staged = without_vulcan_state(report.staged);
    report.unstaged = without_vulcan_state(report.unstaged);
    report.untracked = without_vulcan_state(report.untracked);
    report.clean =
        report.staged.is_empty() && report.unstaged.is_empty() && report.untracked.is_empty();
    Ok(report)
}

pub fn log_report(paths: &VaultPaths, limit: usize) -> Result<GitLogReport, AppError> {
    let entries = git_recent_log(paths.vault_root(), limit).map_err(AppError::operation)?;
    Ok(GitLogReport { limit, entries })
}

pub fn diff_report(paths: &VaultPaths, path: Option<&str>) -> Result<GitDiffReport, AppError> {
    let normalized = path.map(normalize_git_scope_path);
    let changed = without_vulcan_state(
        git_status(paths.vault_root())
            .map_err(AppError::operation)?
            .changed_paths(),
    );
    let changed_paths = match normalized.as_deref() {
        Some(path) => changed
            .into_iter()
            .filter(|candidate| candidate == path)
            .collect(),
        None => changed,
    };
    let diff = git_diff(paths.vault_root(), normalized.as_deref()).map_err(AppError::operation)?;
    Ok(GitDiffReport {
        path: normalized,
        changed_paths,
        diff,
    })
}

pub fn blame_report(paths: &VaultPaths, path: &str) -> Result<GitBlameReport, AppError> {
    let normalized = normalize_git_scope_path(path);
    let lines = git_blame(paths.vault_root(), &normalized).map_err(AppError::operation)?;
    Ok(GitBlameReport {
        path: normalized,
        lines,
    })
}

/// Commits every vault change, running the pre-commit plugin hooks (which
/// may refuse) and the post-commit ones.
pub fn commit_with_hooks(
    paths: &VaultPaths,
    permission_profile: Option<&str>,
    message: &str,
    verbosity: Verbosity,
) -> Result<GitCommitReport, AppError> {
    commit_request_with_hooks(
        paths,
        permission_profile,
        &vulcan_core::GitCommitRequest {
            message,
            only_paths: None,
            trailers: &[],
        },
        verbosity,
    )
}

/// Commit as [`commit_with_hooks`] does, limited to `request.only_paths`
/// and with `request.trailers` appended to the message.
pub fn commit_request_with_hooks(
    paths: &VaultPaths,
    permission_profile: Option<&str>,
    request: &vulcan_core::GitCommitRequest<'_>,
    verbosity: Verbosity,
) -> Result<GitCommitReport, AppError> {
    let message = request.message;
    if message.trim().is_empty() {
        return Err(AppError::operation("a commit needs a message"));
    }
    crate::plugins::dispatch_plugin_event(
        paths,
        permission_profile,
        PluginEvent::OnPreCommit,
        &serde_json::json!({
            "kind": PluginEvent::OnPreCommit,
            "action": "git-commit",
            "message": message,
        }),
        verbosity,
    )?;
    let report =
        vulcan_core::git_commit_with(paths.vault_root(), request).map_err(AppError::operation)?;
    if report.committed {
        let _ = crate::plugins::dispatch_plugin_event(
            paths,
            permission_profile,
            PluginEvent::OnPostCommit,
            &serde_json::json!({
                "kind": PluginEvent::OnPostCommit,
                "action": "git-commit",
                "message": report.message,
                "files": report.files,
                "sha": report.sha,
            }),
            verbosity,
        );
    }
    Ok(report)
}
