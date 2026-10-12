//! Recommended `.gitignore` entries for files Obsidian keeps per device.
//!
//! Sync stops tracking a path once a shared `.gitignore` starts ignoring it
//! and keeps every device's copy, so adding these entries is all it takes:
//! the next sync publishes the `.gitignore` change and drops the files from
//! live history without deleting them anywhere.

use crate::AppError;
use serde::Serialize;
use std::fs;
use vulcan_core::VaultPaths;
use vulcan_sync::{
    tree_ignores, vault_path_prefix, worktree_ignores, GitEngine, GitOid, GitRepository,
    OBSIDIAN_DEVICE_FILES,
};

const HEADER: &str = "# Per-device Obsidian state: each device keeps its own copy.";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IgnoreDeviceFilesReport {
    /// The `.gitignore` that receives the entries, relative to the vault.
    pub gitignore: String,
    /// Entries added, or that a dry run would add.
    pub added: Vec<String>,
    /// Device files the shared `.gitignore` files already ignore.
    pub already_ignored: Vec<String>,
    pub dry_run: bool,
}

/// Adds `.gitignore` entries for the per-device files that the vault's
/// shared `.gitignore` files do not ignore yet. Device-local excludes
/// (`info/exclude`, `core.excludesFile`) do not count: they never reach the
/// other devices.
pub fn ignore_device_files(
    paths: &VaultPaths,
    dry_run: bool,
) -> Result<IgnoreDeviceFilesReport, AppError> {
    let engine = vulcan_sync::GitCliEngine::default();
    let repository = engine
        .discover_repository(paths.vault_root())
        .map_err(AppError::operation)?;
    let work_tree = repository
        .work_tree
        .as_deref()
        .ok_or_else(|| AppError::operation("the vault's Git repository has no work tree"))?;
    let prefix = vault_path_prefix(&repository, paths.vault_root());
    let candidates = OBSIDIAN_DEVICE_FILES.map(|file| format!("{prefix}{file}"));
    let ignored = worktree_ignores(work_tree, &candidates);
    let (mut added, mut already_ignored) = (Vec::new(), Vec::new());
    for (file, ignored) in OBSIDIAN_DEVICE_FILES.iter().zip(ignored) {
        if ignored {
            already_ignored.push((*file).to_string());
        } else {
            added.push(format!("/{file}"));
        }
    }
    let gitignore = paths.vault_root().join(".gitignore");
    if !dry_run && !added.is_empty() {
        let mut contents = match fs::read_to_string(&gitignore) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(AppError::operation(error)),
        };
        if !contents.is_empty() && !contents.ends_with('\n') {
            contents.push('\n');
        }
        if !contents.lines().any(|line| line == HEADER) {
            if !contents.is_empty() {
                contents.push('\n');
            }
            contents.push_str(HEADER);
            contents.push('\n');
        }
        for entry in &added {
            contents.push_str(entry);
            contents.push('\n');
        }
        crate::durable_file::replace(&gitignore, contents.as_bytes())?;
    }
    Ok(IgnoreDeviceFilesReport {
        gitignore: ".gitignore".to_string(),
        added,
        already_ignored,
        dry_run,
    })
}

/// The per-device files `revision` tracks although its `.gitignore` files do
/// not ignore them, relative to the vault root. Force-added files that the
/// rules do ignore are a deliberate choice and are not reported.
pub(crate) fn tracked_device_files(
    engine: &dyn GitEngine,
    repository: &GitRepository,
    vault_root: &std::path::Path,
    revision: &GitOid,
) -> Result<Vec<String>, AppError> {
    let prefix = vault_path_prefix(repository, vault_root);
    let candidates = OBSIDIAN_DEVICE_FILES
        .map(|file| format!("{prefix}{file}"))
        .to_vec();
    let present = engine
        .resolve_paths(repository, revision, &candidates)
        .map_err(AppError::operation)?;
    let ignored =
        tree_ignores(engine, repository, revision, &candidates).map_err(AppError::operation)?;
    Ok(OBSIDIAN_DEVICE_FILES
        .iter()
        .zip(present.iter().zip(ignored))
        .filter(|(_, (present, ignored))| present.is_some() && !ignored)
        .map(|(file, _)| (*file).to_string())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::TempDir;

    fn git(root: &std::path::Path, arguments: &[&str]) -> String {
        let output = Command::new("git")
            .current_dir(root)
            .args(arguments)
            .output()
            .expect("git");
        assert!(output.status.success(), "{arguments:?}: {output:?}");
        String::from_utf8(output.stdout)
            .expect("UTF-8")
            .trim()
            .to_string()
    }

    #[test]
    fn adds_only_missing_entries_and_reports_tracked_files() {
        let temporary = TempDir::new().expect("temporary directory");
        let root = temporary.path();
        git(root, &["init", "--quiet"]);
        git(root, &["config", "user.name", "Vulcan Test"]);
        git(root, &["config", "user.email", "vulcan@example.invalid"]);
        let vault = root.join("vault");
        fs::create_dir_all(vault.join(".obsidian")).expect("obsidian dir");
        for file in OBSIDIAN_DEVICE_FILES {
            fs::write(vault.join(file), "{}\n").expect("device file");
        }
        // A rule elsewhere in the repository already covers the mobile layout.
        fs::write(root.join(".gitignore"), "workspace-mobile.json\n").expect("root ignore");
        fs::write(vault.join(".gitignore"), "cache/").expect("vault ignore");
        git(root, &["add", "--all", "--force", "--", "."]);
        git(root, &["commit", "--quiet", "-m", "base"]);
        let paths = VaultPaths::new(&vault);

        let engine = vulcan_sync::GitCliEngine::default();
        let repository = engine.discover_repository(&vault).expect("repository");
        let head = GitOid::parse(git(root, &["rev-parse", "HEAD"])).expect("head");
        assert_eq!(
            tracked_device_files(&engine, &repository, &vault, &head).expect("tracked"),
            [".obsidian/workspace.json"]
        );

        let preview = ignore_device_files(&paths, true).expect("dry run");
        assert_eq!(preview.added, ["/.obsidian/workspace.json"]);
        assert_eq!(preview.already_ignored, [".obsidian/workspace-mobile.json"]);
        assert_eq!(
            fs::read_to_string(vault.join(".gitignore")).expect("unchanged"),
            "cache/"
        );

        let report = ignore_device_files(&paths, false).expect("apply");
        assert_eq!(report.added, preview.added);
        assert_eq!(
            fs::read_to_string(vault.join(".gitignore")).expect("updated"),
            format!("cache/\n\n{HEADER}\n/.obsidian/workspace.json\n")
        );
        // Running again finds nothing left to add.
        let again = ignore_device_files(&paths, false).expect("idempotent");
        assert!(again.added.is_empty());
        assert_eq!(again.already_ignored.len(), 2);

        git(root, &["commit", "--quiet", "--all", "-m", "ignore"]);
        let head = GitOid::parse(git(root, &["rev-parse", "HEAD"])).expect("head");
        assert!(tracked_device_files(&engine, &repository, &vault, &head)
            .expect("tracked")
            .is_empty());
    }
}
