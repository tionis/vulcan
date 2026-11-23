//! Changesets: a branch and a Git worktree outside the vault where one agent
//! session can stage a larger edit, then land it on the vault's branch or
//! abandon it.
//!
//! The worktree lives in Vulcan's user state directory, not in the vault, so
//! the vault's watchers, batched auto-commit, and sync never see the work in
//! progress. Landing happens under the vault write lock (the lock sync also
//! takes): fast-forward the vault's checked-out branch to the changeset, after
//! rebasing the changeset onto that branch if it moved. A rebase conflict
//! aborts and keeps the changeset untouched; a fast-forward that would
//! overwrite uncommitted vault files is refused by Git and keeps it as well.
//! Git is the registry: changesets are the worktrees on `vulcan/changeset/*`
//! branches, so there is no second record to drift.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Serialize;
use vulcan_core::{GitCommitRequest, VaultPaths, Verbosity};

use crate::AppError;

/// Branch namespace for changesets.
pub const CHANGESET_BRANCH_PREFIX: &str = "vulcan/changeset/";
const CHANGESETS_DIR: &str = "changesets";
const MAX_TITLE_CHARS: usize = 120;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChangesetInfo {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub branch: String,
    /// The worktree; tools attached to the changeset read and write here.
    pub worktree: PathBuf,
    /// Commits on the changeset that the vault's branch does not have.
    pub ahead: usize,
    /// Uncommitted paths in the worktree (Vulcan's own `.vulcan/` excluded).
    pub uncommitted: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChangesetLandReport {
    pub id: String,
    /// The vault branch the changeset landed on.
    pub branch: String,
    pub landed: bool,
    /// Commits the vault branch gained.
    pub commits: usize,
    /// Whether the changeset had to be rebased because the branch moved.
    pub rebased: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
    /// The leftover-changes commit `land` made, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub committed: Option<String>,
}

fn git(root: &Path, args: &[&str]) -> Result<std::process::Output, AppError> {
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .map_err(|error| AppError::operation(format!("cannot run git: {error}")))
}

fn git_text(root: &Path, args: &[&str]) -> Result<String, AppError> {
    let output = git(root, args)?;
    if !output.status.success() {
        return Err(AppError::operation(format!(
            "git {} failed: {}",
            args.first().copied().unwrap_or_default(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn canonical(path: &Path) -> Result<PathBuf, AppError> {
    path.canonicalize()
        .map_err(|error| AppError::operation(format!("cannot resolve {}: {error}", path.display())))
}

/// The vault must be the root of its repository: a changeset checks out the
/// whole repository, and the worktree must be a vault of the same shape.
fn ensure_repository_root(vault: &VaultPaths) -> Result<PathBuf, AppError> {
    let root = canonical(vault.vault_root())?;
    let top = git_text(&root, &["rev-parse", "--show-toplevel"])
        .map_err(|_| AppError::operation("changesets need the vault to be a Git repository"))?;
    if canonical(Path::new(&top))? != root {
        return Err(AppError::operation(
            "changesets need the vault at the root of its Git repository",
        ));
    }
    Ok(root)
}

/// Where this vault's changeset worktrees live.
fn changesets_root(vault_root: &Path) -> Result<PathBuf, AppError> {
    let state = vulcan_core::paths::vulcan_user_state_dir()
        .ok_or_else(|| AppError::operation("could not determine the user state directory"))?;
    let key = blake3::hash(vault_root.to_string_lossy().as_bytes()).to_hex();
    Ok(state.join(CHANGESETS_DIR).join(&key[..32]))
}

/// Whether `root` is a changeset worktree of some vault on this device.
#[must_use]
pub fn is_changeset_worktree(root: &Path) -> bool {
    let (Some(state), Ok(root)) = (
        vulcan_core::paths::vulcan_user_state_dir(),
        root.canonicalize(),
    ) else {
        return false;
    };
    let base = state.join(CHANGESETS_DIR);
    let base = base.canonicalize().unwrap_or(base);
    root.starts_with(base)
}

fn validate_id(id: &str) -> Result<(), AppError> {
    if id.is_empty()
        || id.len() > 64
        || !id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-')
    {
        return Err(AppError::operation(format!("`{id}` is not a changeset id")));
    }
    Ok(())
}

fn current_branch(root: &Path) -> Result<String, AppError> {
    git_text(root, &["symbolic-ref", "--quiet", "--short", "HEAD"])
        .map_err(|_| AppError::operation("the vault has no branch checked out (detached HEAD)"))
}

fn uncommitted(worktree: &Path) -> Result<Vec<String>, AppError> {
    Ok(vulcan_core::git_status(worktree)
        .map_err(AppError::operation)?
        .changed_paths()
        .into_iter()
        .filter(|path| path != ".vulcan" && !path.starts_with(".vulcan/"))
        .collect())
}

fn info(root: &Path, base: &str, id: String, worktree: PathBuf) -> Result<ChangesetInfo, AppError> {
    let branch = format!("{CHANGESET_BRANCH_PREFIX}{id}");
    let ahead = git_text(root, &["rev-list", "--count", &format!("{base}..{branch}")])?
        .parse()
        .unwrap_or(0);
    let title = git_text(
        root,
        &["config", "--get", &format!("branch.{branch}.description")],
    )
    .ok()
    .filter(|title| !title.is_empty());
    let uncommitted = if worktree.is_dir() {
        uncommitted(&worktree)?
    } else {
        Vec::new()
    };
    Ok(ChangesetInfo {
        id,
        title,
        branch,
        worktree,
        ahead,
        uncommitted,
    })
}

/// Every changeset of the vault: worktrees on `vulcan/changeset/*` branches.
pub fn list_changesets(vault: &VaultPaths) -> Result<Vec<ChangesetInfo>, AppError> {
    let root = ensure_repository_root(vault)?;
    let base = current_branch(&root)?;
    let listing = git_text(&root, &["worktree", "list", "--porcelain"])?;
    let mut changesets = Vec::new();
    let mut worktree = None;
    for line in listing.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            worktree = Some(PathBuf::from(path));
        } else if let Some(reference) = line.strip_prefix("branch refs/heads/") {
            if let (Some(id), Some(path)) = (
                reference.strip_prefix(CHANGESET_BRANCH_PREFIX),
                worktree.take(),
            ) {
                changesets.push(info(&root, &base, id.to_string(), path)?);
            }
        }
    }
    Ok(changesets)
}

/// One changeset by id.
pub fn find_changeset(vault: &VaultPaths, id: &str) -> Result<ChangesetInfo, AppError> {
    validate_id(id)?;
    list_changesets(vault)?
        .into_iter()
        .find(|changeset| changeset.id == id)
        .ok_or_else(|| AppError::operation(format!("no changeset `{id}` in this vault")))
}

/// The vault paths of a changeset's worktree.
#[must_use]
pub fn changeset_paths(changeset: &ChangesetInfo) -> VaultPaths {
    VaultPaths::new(&changeset.worktree)
}

/// Start a changeset at the vault's current commit and index its worktree.
pub fn begin_changeset(vault: &VaultPaths, title: Option<&str>) -> Result<ChangesetInfo, AppError> {
    let root = ensure_repository_root(vault)?;
    let base = current_branch(&root)?;
    let id = ulid::Ulid::new().to_string().to_ascii_lowercase();
    let branch = format!("{CHANGESET_BRANCH_PREFIX}{id}");
    let directory = changesets_root(&root)?;
    std::fs::create_dir_all(&directory).map_err(AppError::operation)?;
    let worktree = directory.join(&id);
    let worktree_text = worktree.to_string_lossy().to_string();
    git_text(
        &root,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            &branch,
            &worktree_text,
            "HEAD",
        ],
    )?;
    if let Some(title) = title.map(str::trim).filter(|title| !title.is_empty()) {
        let title: String = title
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(MAX_TITLE_CHARS)
            .collect();
        git_text(
            &root,
            &["config", &format!("branch.{branch}.description"), &title],
        )?;
    }
    let paths = VaultPaths::new(&worktree);
    let prepared = vulcan_core::paths::initialize_vulcan_dir(&paths)
        .map_err(AppError::operation)
        .and_then(|()| {
            vulcan_core::scan_vault(&paths, vulcan_core::ScanMode::Full)
                .map(|_| ())
                .map_err(AppError::operation)
        });
    if let Err(error) = prepared {
        let _ = remove(&root, &branch, &worktree);
        return Err(error);
    }
    info(&root, &base, id, worktree)
}

fn remove(root: &Path, branch: &str, worktree: &Path) -> Result<(), AppError> {
    let worktree_text = worktree.to_string_lossy().to_string();
    if worktree.exists() {
        git_text(root, &["worktree", "remove", "--force", &worktree_text])?;
    } else {
        git_text(root, &["worktree", "prune"])?;
    }
    git_text(root, &["branch", "-D", "--quiet", branch])?;
    Ok(())
}

/// Delete a changeset without landing it.
pub fn abandon_changeset(vault: &VaultPaths, id: &str) -> Result<ChangesetInfo, AppError> {
    let root = ensure_repository_root(vault)?;
    let changeset = find_changeset(vault, id)?;
    remove(&root, &changeset.branch, &changeset.worktree)?;
    Ok(changeset)
}

/// What `land` commits first, when the worktree has uncommitted changes.
pub struct ChangesetLandCommit<'a> {
    pub message: &'a str,
    pub trailers: &'a [(String, String)],
    pub permission_profile: Option<&'a str>,
}

/// Land a changeset on the vault's checked-out branch and remove it.
///
/// Uncommitted worktree changes are committed first with `commit`, or refuse
/// the landing without one. A conflict while rebasing onto a moved branch
/// aborts and keeps the changeset as it was, naming the conflicting paths.
pub fn land_changeset(
    vault: &VaultPaths,
    id: &str,
    commit: Option<&ChangesetLandCommit<'_>>,
) -> Result<ChangesetLandReport, AppError> {
    let root = ensure_repository_root(vault)?;
    let changeset = find_changeset(vault, id)?;
    let worktree = changeset.worktree.clone();
    let mut committed = None;
    if !changeset.uncommitted.is_empty() {
        let Some(commit) = commit else {
            return Err(AppError::operation(format!(
                "changeset `{id}` has uncommitted changes ({}); commit them or pass a message to land them",
                changeset.uncommitted.join(", ")
            )));
        };
        let report = crate::git_ops::commit_request_with_hooks(
            &VaultPaths::new(&worktree),
            commit.permission_profile,
            &GitCommitRequest {
                message: commit.message,
                only_paths: None,
                trailers: commit.trailers,
            },
            Verbosity::Quiet,
        )?;
        committed = report.sha;
    }

    let lock = vulcan_core::write_lock::acquire_write_lock(vault).map_err(AppError::operation)?;
    let base = current_branch(&root)?;
    let branch = changeset.branch.clone();
    let ahead: usize = git_text(
        &root,
        &["rev-list", "--count", &format!("{base}..{branch}")],
    )?
    .parse()
    .unwrap_or(0);
    if ahead == 0 {
        drop(lock);
        remove(&root, &branch, &worktree)?;
        return Ok(ChangesetLandReport {
            id: id.to_string(),
            branch: base,
            landed: false,
            commits: 0,
            rebased: false,
            head: None,
            committed,
        });
    }
    let fast_forward = git(&root, &["merge-base", "--is-ancestor", &base, &branch])?
        .status
        .success();
    if !fast_forward {
        let rebase = git(&worktree, &["rebase", "--quiet", &base])?;
        if !rebase.status.success() {
            let conflicts = git_text(&worktree, &["diff", "--name-only", "--diff-filter=U"])
                .unwrap_or_default();
            let _ = git(&worktree, &["rebase", "--abort"]);
            let conflicts = conflicts.lines().collect::<Vec<_>>().join(", ");
            return Err(AppError::operation(format!(
                "`{base}` moved and changeset `{id}` does not rebase onto it cleanly{}; the changeset is unchanged",
                if conflicts.is_empty() {
                    String::new()
                } else {
                    format!(" (conflicts in {conflicts})")
                }
            )));
        }
    }
    let merge = git(&root, &["merge", "--ff-only", "--quiet", &branch])?;
    if !merge.status.success() {
        return Err(AppError::operation(format!(
            "cannot fast-forward `{base}` to changeset `{id}`: {}; the changeset is kept",
            String::from_utf8_lossy(&merge.stderr).trim()
        )));
    }
    let head = git_text(&root, &["rev-parse", "HEAD"])?;
    drop(lock);
    remove(&root, &branch, &worktree)?;
    // Refresh the vault's index; the daemon's watcher would get there too.
    let _ = vulcan_core::scan_vault(vault, vulcan_core::ScanMode::Incremental);
    Ok(ChangesetLandReport {
        id: id.to_string(),
        branch: base,
        landed: true,
        commits: ahead,
        rebased: !fast_forward,
        head: Some(head),
        committed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn run(root: &Path, args: &[&str]) {
        let output = git(root, args).unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn vault() -> (tempfile::TempDir, tempfile::TempDir, VaultPaths) {
        let state = tempfile::tempdir().unwrap();
        std::env::set_var("XDG_STATE_HOME", state.path());
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        run(root, &["init", "--quiet", "-b", "main"]);
        run(root, &["config", "user.email", "test@example.invalid"]);
        run(root, &["config", "user.name", "Test"]);
        fs::write(root.join("Home.md"), "# Home\n").unwrap();
        fs::write(root.join("Other.md"), "# Other\n").unwrap();
        run(root, &["add", "."]);
        run(root, &["commit", "--quiet", "-m", "init"]);
        let paths = VaultPaths::new(root);
        vulcan_core::paths::initialize_vulcan_dir(&paths).unwrap();
        (state, dir, paths)
    }

    fn commit_in(root: &Path, file: &str, text: &str) {
        fs::write(root.join(file), text).unwrap();
        run(root, &["add", file]);
        run(root, &["commit", "--quiet", "-m", file]);
    }

    // One test, because the state directory is process-wide.
    #[test]
    fn changesets_stage_land_rebase_and_refuse_conflicts() {
        let _lock = crate::trust::test_env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let previous = std::env::var_os("XDG_STATE_HOME");
        let (_state, _dir, vault) = vault();
        let root = vault.vault_root().to_path_buf();

        // Begin: a worktree outside the vault, indexed, on its own branch.
        let first = begin_changeset(&vault, Some("Rework  home")).unwrap();
        assert_eq!(first.title.as_deref(), Some("Rework home"));
        assert!(!first.worktree.starts_with(&root));
        assert!(is_changeset_worktree(&first.worktree));
        assert!(!is_changeset_worktree(&root));
        assert!(first.worktree.join(".vulcan/cache.db").exists());
        assert!(first.uncommitted.is_empty());

        // Uncommitted work refuses to land without a message.
        fs::write(first.worktree.join("Home.md"), "# Home\n\nReworked.\n").unwrap();
        let error = land_changeset(&vault, &first.id, None).unwrap_err();
        assert!(error.to_string().contains("Home.md"), "{error}");
        assert_eq!(
            fs::read_to_string(root.join("Home.md")).unwrap(),
            "# Home\n"
        );

        // The vault moved meanwhile; landing commits, rebases, fast-forwards.
        commit_in(&root, "Other.md", "# Other\n\nElsewhere.\n");
        let trailers = vec![("Vulcan-MCP-Profile".to_string(), "agent".to_string())];
        let landed = land_changeset(
            &vault,
            &first.id,
            Some(&ChangesetLandCommit {
                message: "Rework home",
                trailers: &trailers,
                permission_profile: None,
            }),
        )
        .unwrap();
        assert!(landed.landed && landed.rebased, "{landed:?}");
        assert_eq!(landed.commits, 1);
        assert_eq!(landed.branch, "main");
        assert!(fs::read_to_string(root.join("Home.md"))
            .unwrap()
            .contains("Reworked."));
        let message = git_text(&root, &["log", "-1", "--format=%B"]).unwrap();
        assert!(message.contains("Vulcan-MCP-Profile: agent"), "{message}");
        assert!(!first.worktree.exists());
        assert!(list_changesets(&vault).unwrap().is_empty());
        assert!(git_text(&root, &["branch", "--list", "vulcan/*"])
            .unwrap()
            .is_empty());

        // A conflicting change keeps the changeset untouched.
        let second = begin_changeset(&vault, None).unwrap();
        commit_in(&second.worktree, "Home.md", "# Home\n\nTheirs.\n");
        commit_in(&root, "Home.md", "# Home\n\nOurs.\n");
        let before = git_text(&second.worktree, &["rev-parse", "HEAD"]).unwrap();
        let error = land_changeset(&vault, &second.id, None).unwrap_err();
        assert!(
            error.to_string().contains("conflicts in Home.md"),
            "{error}"
        );
        assert_eq!(
            git_text(&second.worktree, &["rev-parse", "HEAD"]).unwrap(),
            before
        );
        assert_eq!(list_changesets(&vault).unwrap()[0].ahead, 1);

        // Abandon removes it; a fast-forward-only landing needs no rebase.
        abandon_changeset(&vault, &second.id).unwrap();
        assert!(!second.worktree.exists());
        let third = begin_changeset(&vault, None).unwrap();
        commit_in(&third.worktree, "New.md", "# New\n");
        let landed = land_changeset(&vault, &third.id, None).unwrap();
        assert!(landed.landed && !landed.rebased);
        assert!(root.join("New.md").exists());
        assert!(find_changeset(&vault, &third.id).is_err());
        assert!(find_changeset(&vault, "../escape").is_err());

        match previous {
            Some(value) => std::env::set_var("XDG_STATE_HOME", value),
            None => std::env::remove_var("XDG_STATE_HOME"),
        }
    }
}
