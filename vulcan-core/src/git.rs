use crate::config::{GitConfig, GitScope};
use serde::Serialize;
use std::collections::BTreeSet;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug)]
pub enum GitError {
    CommandFailed(String),
    Io(std::io::Error),
    Utf8(std::string::FromUtf8Error),
}

impl Display for GitError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CommandFailed(message) => formatter.write_str(message),
            Self::Io(error) => write!(formatter, "{error}"),
            Self::Utf8(error) => write!(formatter, "{error}"),
        }
    }
}

impl Error for GitError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Utf8(error) => Some(error),
            Self::CommandFailed(_) => None,
        }
    }
}

impl From<std::io::Error> for GitError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<std::string::FromUtf8Error> for GitError {
    fn from(error: std::string::FromUtf8Error) -> Self {
        Self::Utf8(error)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AutoCommitReport {
    pub committed: bool,
    pub message: String,
    pub files: Vec<String>,
    pub sha: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitLogEntry {
    pub commit: String,
    pub author_name: String,
    pub author_email: String,
    pub committed_at: String,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitStatusReport {
    pub clean: bool,
    pub staged: Vec<String>,
    pub unstaged: Vec<String>,
    pub untracked: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitCommitReport {
    pub committed: bool,
    pub message: String,
    pub files: Vec<String>,
    pub sha: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitBlameLine {
    pub line_number: usize,
    pub commit: String,
    pub author_name: String,
    pub author_email: String,
    pub author_time: i64,
    pub author_tz: String,
    pub summary: String,
    pub line: String,
}

impl GitStatusReport {
    #[must_use]
    pub fn changed_paths(&self) -> Vec<String> {
        self.staged
            .iter()
            .chain(&self.unstaged)
            .chain(&self.untracked)
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }
}

/// Where a vault sits inside its enclosing Git work tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitRepositoryLayout {
    /// Absolute path of the enclosing Git work tree.
    pub work_tree: PathBuf,
    /// The vault root relative to the work tree with `/` separators, without a
    /// trailing slash. Empty when the vault is the work-tree root.
    pub vault_prefix: String,
}

impl GitRepositoryLayout {
    #[must_use]
    pub fn is_nested(&self) -> bool {
        !self.vault_prefix.is_empty()
    }

    /// Converts a vault-relative path into a work-tree-relative path.
    #[must_use]
    pub fn repository_path(&self, vault_path: &str) -> String {
        if self.vault_prefix.is_empty() {
            vault_path.to_string()
        } else {
            format!("{}/{vault_path}", self.vault_prefix)
        }
    }
}

/// Describes the Git work tree containing `vault_root`, or `None` when the
/// vault is not inside a Git work tree.
#[must_use]
pub fn git_repository_layout(vault_root: &Path) -> Option<GitRepositoryLayout> {
    let stdout = run_git_capture(vault_root, |command| {
        command.args(["rev-parse", "--show-toplevel", "--show-prefix"]);
    })
    .ok()?;
    let mut lines = stdout.lines();
    let work_tree = PathBuf::from(lines.next()?.trim());
    let vault_prefix = normalize_git_path(lines.next().unwrap_or_default().trim())
        .trim_end_matches('/')
        .to_string();
    Some(GitRepositoryLayout {
        work_tree,
        vault_prefix,
    })
}

#[must_use]
pub fn is_git_repo(vault_root: &Path) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(vault_root)
        .args(["rev-parse", "--git-dir"])
        .output()
        .is_ok_and(|output| output.status.success())
}

pub fn auto_commit(
    vault_root: &Path,
    config: &GitConfig,
    action: &str,
    changed_files: &[String],
) -> Result<AutoCommitReport, GitError> {
    if !config.auto_commit {
        return Ok(AutoCommitReport {
            committed: false,
            message: "auto-commit disabled".to_string(),
            files: Vec::new(),
            sha: None,
        });
    }

    if !is_git_repo(vault_root) {
        return Ok(AutoCommitReport {
            committed: false,
            message: "auto-commit enabled but the vault is not a git repository".to_string(),
            files: Vec::new(),
            sha: None,
        });
    }

    let _repository_lock = acquire_repository_mutation_lock(vault_root)?;

    let candidate_paths = resolve_commit_paths(vault_root, config, changed_files)?;
    if candidate_paths.is_empty() {
        return Ok(AutoCommitReport {
            committed: false,
            message: "no eligible files to auto-commit".to_string(),
            files: Vec::new(),
            sha: None,
        });
    }

    let stageable_paths = candidate_paths
        .into_iter()
        .filter_map(|path| match stageable_path(vault_root, &path) {
            Ok(true) => Some(Ok(path)),
            Ok(false) => None,
            Err(error) => Some(Err(error)),
        })
        .collect::<Result<Vec<_>, _>>()?;
    if stageable_paths.is_empty() {
        return Ok(AutoCommitReport {
            committed: false,
            message: "no tracked or existing files remained after filtering".to_string(),
            files: Vec::new(),
            sha: None,
        });
    }

    run_git(vault_root, "stage changes", |command| {
        command.arg("add").arg("--all").arg("--");
        for path in &stageable_paths {
            command.arg(path);
        }
    })?;

    let staged_paths = staged_paths(vault_root)?
        .into_iter()
        .filter(|path| !path_is_excluded(path, &config.exclude))
        .collect::<Vec<_>>();
    if staged_paths.is_empty() {
        return Ok(AutoCommitReport {
            committed: false,
            message: "no staged changes matched the auto-commit scope".to_string(),
            files: Vec::new(),
            sha: None,
        });
    }

    let message = render_commit_message(&config.message, action, &staged_paths);
    run_git(vault_root, "create commit", |command| {
        command.arg("commit").arg("-m").arg(&message).arg("--");
        for path in &staged_paths {
            command.arg(path);
        }
    })?;
    let sha = run_git_capture(vault_root, |command| {
        command.args(["rev-parse", "HEAD"]);
    })?
    .trim()
    .to_string();

    Ok(AutoCommitReport {
        committed: true,
        message,
        files: staged_paths,
        sha: Some(sha),
    })
}

/// What a batched commit may take from the working tree, read without
/// changing the repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BatchCommitCandidates {
    /// `HEAD` when the batch was read, or `None` on an unborn branch.
    pub head: Option<String>,
    /// Changed, deleted, and untracked paths outside the exclusions.
    pub paths: Vec<String>,
    /// Paths with staged changes. They belong to whoever staged them, so a
    /// batch neither commits nor unstages them.
    pub left_staged: Vec<String>,
    /// Why no batch may be committed now, such as a merge in progress.
    pub blocked: Option<String>,
}

/// One commit of a batch: its message and the paths it records.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BatchCommitGroup {
    pub message: String,
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct BatchCommitRecord {
    pub sha: String,
    pub message: String,
    pub files: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum BatchCommitOutcome {
    Committed {
        commits: Vec<BatchCommitRecord>,
    },
    /// The working tree or `HEAD` moved after the groups were planned.
    Changed {
        current: BatchCommitCandidates,
    },
    Blocked {
        reason: String,
    },
}

/// Reads the paths a batched commit would record. Exclusions follow the
/// `[git]` configuration, and `.vulcan/` is never committed.
pub fn batch_commit_candidates(
    vault_root: &Path,
    config: &GitConfig,
) -> Result<BatchCommitCandidates, GitError> {
    ensure_git_repo(vault_root)?;
    let head = run_git_output(vault_root, |command| {
        command.args(["rev-parse", "-q", "--verify", "HEAD"]);
    })?;
    let head = head
        .status
        .success()
        .then(|| String::from_utf8(head.stdout))
        .transpose()?
        .map(|sha| sha.trim().to_string());
    let blocked = batch_commit_blocker(vault_root)?;
    let status = git_status(vault_root)?;
    let staged = status.staged.iter().cloned().collect::<BTreeSet<_>>();
    let paths = status
        .unstaged
        .iter()
        .chain(&status.untracked)
        .filter(|path| !staged.contains(*path))
        .filter_map(|path| validate_git_path(path).ok())
        .filter(|path| !path.is_empty() && !path_is_excluded(path, &config.exclude))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    Ok(BatchCommitCandidates {
        head,
        paths,
        left_staged: staged.into_iter().collect(),
        blocked,
    })
}

/// Reports work in progress that a batched commit must not interleave with:
/// a merge, rebase, cherry-pick, revert, or bisect, unmerged paths, or a
/// detached `HEAD`.
fn batch_commit_blocker(vault_root: &Path) -> Result<Option<String>, GitError> {
    let git_dir = PathBuf::from(
        run_git_capture(vault_root, |command| {
            command.args(["rev-parse", "--absolute-git-dir"]);
        })?
        .trim(),
    );
    for (marker, operation) in [
        ("MERGE_HEAD", "a merge"),
        ("rebase-merge", "a rebase"),
        ("rebase-apply", "a rebase or patch application"),
        ("CHERRY_PICK_HEAD", "a cherry-pick"),
        ("REVERT_HEAD", "a revert"),
        ("BISECT_LOG", "a bisect"),
    ] {
        if git_dir.join(marker).exists() {
            return Ok(Some(format!("{operation} is in progress")));
        }
    }
    let branch = run_git_output(vault_root, |command| {
        command.args(["symbolic-ref", "-q", "HEAD"]);
    })?;
    if !branch.status.success() {
        return Ok(Some("HEAD is detached".to_string()));
    }
    let unmerged = run_git_capture(vault_root, |command| {
        command.args(["diff", "--name-only", "--diff-filter=U"]);
    })?;
    if !unmerged.trim().is_empty() {
        return Ok(Some("the repository has unmerged paths".to_string()));
    }
    Ok(None)
}

/// Commits `groups` in order under the repository mutation lock, after
/// checking that the working tree still matches `planned`. Each commit records
/// only its own paths, so paths someone else staged stay staged. If a commit
/// fails, the paths not yet committed are unstaged again and earlier commits
/// of the batch are kept.
pub fn commit_batch_groups(
    vault_root: &Path,
    config: &GitConfig,
    planned: &BatchCommitCandidates,
    groups: &[BatchCommitGroup],
) -> Result<BatchCommitOutcome, GitError> {
    let _repository_lock = acquire_repository_mutation_lock(vault_root)?;
    let current = batch_commit_candidates(vault_root, config)?;
    if let Some(reason) = current.blocked {
        return Ok(BatchCommitOutcome::Blocked { reason });
    }
    if current.head != planned.head || current.paths != planned.paths {
        return Ok(BatchCommitOutcome::Changed { current });
    }
    let planned_paths = planned.paths.iter().collect::<BTreeSet<_>>();
    let mut grouped = BTreeSet::new();
    for group in groups {
        if group.paths.is_empty() {
            return Err(GitError::CommandFailed(
                "a batched commit group has no paths".to_string(),
            ));
        }
        for path in &group.paths {
            if !planned_paths.contains(path) || !grouped.insert(path) {
                return Err(GitError::CommandFailed(format!(
                    "batched commit groups must name each planned path once; `{path}` was not"
                )));
            }
        }
    }
    if grouped.len() != planned_paths.len() {
        return Err(GitError::CommandFailed(
            "batched commit groups must cover every planned path".to_string(),
        ));
    }
    if groups.is_empty() {
        return Ok(BatchCommitOutcome::Committed {
            commits: Vec::new(),
        });
    }
    run_git(vault_root, "stage batched changes", |command| {
        command.arg("add").arg("--all").arg("--");
        for path in &planned.paths {
            command.arg(path);
        }
    })?;
    let mut commits = Vec::with_capacity(groups.len());
    for (position, group) in groups.iter().enumerate() {
        let result = run_git(vault_root, "create batched commit", |command| {
            command
                .arg("commit")
                .arg("-m")
                .arg(&group.message)
                .arg("--");
            for path in &group.paths {
                command.arg(path);
            }
        })
        .and_then(|()| {
            run_git_capture(vault_root, |command| {
                command.args(["rev-parse", "HEAD"]);
            })
        });
        match result {
            Ok(sha) => commits.push(BatchCommitRecord {
                sha: sha.trim().to_string(),
                message: group.message.clone(),
                files: group.paths.clone(),
            }),
            Err(error) => {
                let remaining = groups[position..]
                    .iter()
                    .flat_map(|group| group.paths.iter())
                    .collect::<Vec<_>>();
                // Leave the uncommitted paths as they were before the batch:
                // changed in the work tree, not staged.
                let _ = run_git(vault_root, "unstage uncommitted batch paths", |command| {
                    command.args(["reset", "-q", "--"]);
                    for path in &remaining {
                        command.arg(path);
                    }
                });
                return Err(error);
            }
        }
    }
    Ok(BatchCommitOutcome::Committed { commits })
}

/// The patch of each candidate path against `HEAD`, in candidate order, or
/// `None` once the patches together exceed `max_bytes`. Untracked files show
/// as additions; binary files show as Git's one-line binary notice.
pub fn batch_commit_patches(
    vault_root: &Path,
    candidates: &BatchCommitCandidates,
    max_bytes: usize,
) -> Result<Option<Vec<(String, String)>>, GitError> {
    let untracked = git_status(vault_root)?
        .untracked
        .into_iter()
        .collect::<BTreeSet<_>>();
    let mut total = 0_usize;
    let mut patches = Vec::with_capacity(candidates.paths.len());
    for path in &candidates.paths {
        let patch = if candidates.head.is_none() || untracked.contains(path) {
            addition_patch(vault_root, path, max_bytes.saturating_sub(total))?
        } else {
            render_git_path_diff(vault_root, path, false)?
        };
        total = total.saturating_add(patch.len());
        if total > max_bytes {
            return Ok(None);
        }
        patches.push((path.clone(), patch));
    }
    Ok(Some(patches))
}

/// A unified-diff-style addition of a new file, naming only its
/// vault-relative path. Content beyond `limit` bytes is not read, so the
/// caller's byte bound applies before the whole file is loaded.
fn addition_patch(vault_root: &Path, path: &str, limit: usize) -> Result<String, GitError> {
    use std::io::Read;

    let mut bytes = Vec::new();
    fs::File::open(vault_root.join(path))
        .map_err(GitError::Io)?
        .take(limit.saturating_add(1) as u64)
        .read_to_end(&mut bytes)
        .map_err(GitError::Io)?;
    if bytes.contains(&0) {
        return Ok(format!("Binary file {path} added\n"));
    }
    let text = String::from_utf8_lossy(&bytes);
    let mut addition = format!("--- /dev/null\n+++ b/{path}\n");
    for line in text.lines() {
        addition.push('+');
        addition.push_str(line);
        addition.push('\n');
    }
    Ok(addition)
}

/// The commit message `template` produces for `files`, as per-write
/// auto-commits render it.
#[must_use]
pub fn render_auto_commit_message(template: &str, action: &str, files: &[String]) -> String {
    render_commit_message(template, action, files)
}

pub fn git_log(
    vault_root: &Path,
    file_path: &str,
    limit: usize,
) -> Result<Vec<GitLogEntry>, GitError> {
    ensure_git_repo(vault_root)?;
    let file_path = validate_git_path(file_path)?;
    collect_git_log(vault_root, limit, Some(&file_path))
}

pub fn git_recent_log(vault_root: &Path, limit: usize) -> Result<Vec<GitLogEntry>, GitError> {
    ensure_git_repo(vault_root)?;
    collect_git_log(vault_root, limit, None)
}

pub fn git_status(vault_root: &Path) -> Result<GitStatusReport, GitError> {
    ensure_git_repo(vault_root)?;
    let worktree_prefix = run_git_capture(vault_root, |command| {
        command.args(["rev-parse", "--show-prefix"]);
    })?;
    let worktree_prefix = worktree_prefix.trim();
    let stdout = run_git_capture(vault_root, |command| {
        command.args(["status", "--short", "--untracked-files=all", "--", "."]);
    })?;

    let mut staged = BTreeSet::new();
    let mut unstaged = BTreeSet::new();
    let mut untracked = BTreeSet::new();

    for line in stdout.lines().filter(|line| !line.is_empty()) {
        let bytes = line.as_bytes();
        if bytes.len() < 3 {
            continue;
        }
        let x = bytes[0] as char;
        let y = bytes[1] as char;
        let path = parse_status_path(&line[3..]);
        let path = path
            .strip_prefix(worktree_prefix)
            .unwrap_or(&path)
            .to_string();
        if path.is_empty() {
            continue;
        }

        if x == '?' && y == '?' {
            untracked.insert(path);
            continue;
        }

        if x != ' ' {
            staged.insert(path.clone());
        }
        if y != ' ' {
            unstaged.insert(path);
        }
    }

    Ok(GitStatusReport {
        clean: staged.is_empty() && unstaged.is_empty() && untracked.is_empty(),
        staged: staged.into_iter().collect(),
        unstaged: unstaged.into_iter().collect(),
        untracked: untracked.into_iter().collect(),
    })
}

pub fn git_diff(vault_root: &Path, path: Option<&str>) -> Result<String, GitError> {
    ensure_git_repo(vault_root)?;
    let status = git_status(vault_root)?;
    let untracked_paths = status.untracked.iter().cloned().collect::<BTreeSet<_>>();
    let paths = match path {
        Some(path) => {
            let normalized = validate_git_path(path)?;
            if path_is_excluded(&normalized, &[]) {
                return Err(GitError::CommandFailed(format!(
                    "refusing to diff an internal path: {normalized}"
                )));
            }
            vec![normalized]
        }
        None => status
            .changed_paths()
            .into_iter()
            .filter(|path| !path_is_excluded(path, &[]))
            .collect::<Vec<_>>(),
    };

    render_git_diff_for_paths(vault_root, &paths, &untracked_paths)
}

pub fn git_commit(vault_root: &Path, message: &str) -> Result<GitCommitReport, GitError> {
    ensure_git_repo(vault_root)?;
    let _repository_lock = acquire_repository_mutation_lock(vault_root)?;
    let commit_paths = git_status(vault_root)?
        .changed_paths()
        .into_iter()
        .filter(|path| !path_is_excluded(path, &[]))
        .collect::<Vec<_>>();
    if commit_paths.is_empty() {
        return Ok(GitCommitReport {
            committed: false,
            message: "no eligible files to commit".to_string(),
            files: Vec::new(),
            sha: None,
        });
    }

    run_git(vault_root, "stage changes", |command| {
        command.arg("add").arg("--all").arg("--");
        for path in &commit_paths {
            command.arg(path);
        }
    })?;

    let staged = staged_paths(vault_root)?
        .into_iter()
        .filter(|path| !path_is_excluded(path, &[]))
        .collect::<Vec<_>>();
    if staged.is_empty() {
        return Ok(GitCommitReport {
            committed: false,
            message: "no eligible files remained after filtering".to_string(),
            files: Vec::new(),
            sha: None,
        });
    }

    run_git(vault_root, "create commit", |command| {
        command.arg("commit").arg("-m").arg(message).arg("--");
        for path in &staged {
            command.arg(path);
        }
    })?;
    let sha = run_git_capture(vault_root, |command| {
        command.args(["rev-parse", "HEAD"]);
    })?
    .trim()
    .to_string();

    Ok(GitCommitReport {
        committed: true,
        message: message.to_string(),
        files: staged,
        sha: Some(sha),
    })
}

pub fn git_blame(vault_root: &Path, path: &str) -> Result<Vec<GitBlameLine>, GitError> {
    ensure_git_repo(vault_root)?;
    let normalized = validate_git_path(path)?;
    if path_is_excluded(&normalized, &[]) {
        return Err(GitError::CommandFailed(format!(
            "refusing to blame an internal path: {normalized}"
        )));
    }

    let stdout = run_git_capture(vault_root, |command| {
        command
            .arg("blame")
            .arg("--line-porcelain")
            .arg("--")
            .arg(&normalized);
    })?;

    parse_git_blame(&stdout)
}

fn ensure_git_repo(vault_root: &Path) -> Result<(), GitError> {
    if is_git_repo(vault_root) {
        Ok(())
    } else {
        Err(GitError::CommandFailed(
            "vault is not a git repository".to_string(),
        ))
    }
}

fn run_git(
    vault_root: &Path,
    action: &str,
    configure: impl FnOnce(&mut Command),
) -> Result<(), GitError> {
    let output = run_git_output(vault_root, configure)?;
    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8(output.stderr)?;
    Err(GitError::CommandFailed(format!(
        "git failed to {action}: {}",
        stderr.trim()
    )))
}

fn run_git_capture(
    vault_root: &Path,
    configure: impl FnOnce(&mut Command),
) -> Result<String, GitError> {
    let output = run_git_output(vault_root, configure)?;
    if !output.status.success() {
        let stderr = String::from_utf8(output.stderr)?;
        return Err(GitError::CommandFailed(stderr.trim().to_string()));
    }

    Ok(String::from_utf8(output.stdout)?)
}

fn run_git_output(
    vault_root: &Path,
    configure: impl FnOnce(&mut Command),
) -> Result<std::process::Output, GitError> {
    let mut command = Command::new("git");
    command.arg("-C").arg(vault_root);
    configure(&mut command);
    Ok(command.output()?)
}

fn resolve_commit_paths(
    vault_root: &Path,
    config: &GitConfig,
    changed_files: &[String],
) -> Result<Vec<String>, GitError> {
    let paths = match config.scope {
        GitScope::VulcanOnly => changed_files.to_vec(),
        GitScope::All => git_status(vault_root)?.changed_paths(),
    };

    Ok(paths
        .into_iter()
        .filter_map(|path| validate_git_path(&path).ok())
        .filter(|path| !path.is_empty())
        .filter(|path| !path_is_excluded(path, &config.exclude))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect())
}

fn stageable_path(vault_root: &Path, path: &str) -> Result<bool, GitError> {
    if vault_root.join(path).exists() {
        return Ok(true);
    }

    let output = run_git_output(vault_root, |command| {
        command
            .args(["ls-files", "--error-unmatch", "--"])
            .arg(path);
    })?;
    Ok(output.status.success())
}

fn staged_paths(vault_root: &Path) -> Result<Vec<String>, GitError> {
    let stdout = run_git_capture(vault_root, |command| {
        command.args(["diff", "--cached", "--name-only", "--relative", "--", "."]);
    })?;

    Ok(stdout
        .lines()
        .map(normalize_git_path)
        .filter(|line| !line.is_empty())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect())
}

fn acquire_repository_mutation_lock(
    vault_root: &Path,
) -> Result<vulcan_sync::RepositoryLock, GitError> {
    let git_dir = run_git_capture(vault_root, |command| {
        command.args(["rev-parse", "--absolute-git-dir"]);
    })?;
    let git_dir = PathBuf::from(git_dir.trim());
    vulcan_sync::RepositoryLock::acquire(&git_dir).map_err(|error| {
        GitError::CommandFailed(format!(
            "cannot acquire the Vulcan repository mutation lock: {error}"
        ))
    })
}

fn collect_git_log(
    vault_root: &Path,
    limit: usize,
    file_path: Option<&str>,
) -> Result<Vec<GitLogEntry>, GitError> {
    let limit = limit.max(1);
    let stdout = run_git_capture(vault_root, |command| {
        command.args([
            "log",
            "--date=iso-strict",
            &format!("-n{limit}"),
            "--pretty=format:%H%x1f%an%x1f%ae%x1f%ad%x1f%s",
        ]);
        if let Some(file_path) = file_path {
            command.arg("--follow").arg("--").arg(file_path);
        } else {
            command.arg("--").arg(".");
        }
    })?;

    Ok(stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(parse_git_log_line)
        .collect())
}

fn render_git_diff_for_paths(
    vault_root: &Path,
    paths: &[String],
    untracked_paths: &BTreeSet<String>,
) -> Result<String, GitError> {
    let mut diffs = Vec::new();
    for path in paths {
        let diff = render_git_path_diff(vault_root, path, untracked_paths.contains(path))?;
        if !diff.trim().is_empty() {
            diffs.push(diff);
        }
    }
    Ok(diffs.join(""))
}

fn render_git_path_diff(
    vault_root: &Path,
    path: &str,
    untracked: bool,
) -> Result<String, GitError> {
    let output = if untracked {
        let empty_path =
            std::env::temp_dir().join(format!("vulcan-empty-diff-{}", ulid::Ulid::new()));
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&empty_path)
            .map_err(GitError::Io)?;
        let output = run_git_output(vault_root, |command| {
            command
                .args(["diff", "--no-index", "--no-color"])
                .arg(&empty_path)
                .arg(vault_root.join(path));
        });
        let _ = fs::remove_file(&empty_path);
        output?
    } else {
        run_git_output(vault_root, |command| {
            command.args(["diff", "--no-color", "HEAD", "--"]).arg(path);
        })?
    };

    if untracked {
        if !matches!(output.status.code(), Some(0 | 1)) {
            let stderr = String::from_utf8(output.stderr)?;
            return Err(GitError::CommandFailed(stderr.trim().to_string()));
        }
    } else if !output.status.success() {
        let stderr = String::from_utf8(output.stderr)?;
        return Err(GitError::CommandFailed(stderr.trim().to_string()));
    }

    Ok(String::from_utf8(output.stdout)?)
}

fn parse_git_log_line(line: &str) -> Option<GitLogEntry> {
    let mut parts = line.split('\u{1f}');
    Some(GitLogEntry {
        commit: parts.next()?.to_string(),
        author_name: parts.next()?.to_string(),
        author_email: parts.next()?.to_string(),
        committed_at: parts.next()?.to_string(),
        summary: parts.next()?.to_string(),
    })
}

fn parse_status_path(path: &str) -> String {
    let trimmed = path.trim();
    let renamed = trimmed
        .rsplit_once(" -> ")
        .map_or(trimmed, |(_, destination)| destination);
    normalize_git_path(renamed.trim_matches('"'))
}

fn normalize_git_path(path: &str) -> String {
    path.replace('\\', "/").trim_start_matches("./").to_string()
}

fn validate_git_path(path: &str) -> Result<String, GitError> {
    let candidate = Path::new(path);
    if candidate.is_absolute()
        || candidate.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir | std::path::Component::RootDir
            )
        })
    {
        return Err(GitError::CommandFailed(format!(
            "git path must stay within the vault: {path}"
        )));
    }
    Ok(normalize_git_path(path))
}

fn path_is_excluded(path: &str, exclude: &[String]) -> bool {
    path_matches_pattern(path, ".vulcan")
        || exclude
            .iter()
            .map(|pattern| normalize_git_path(pattern))
            .any(|pattern| path_matches_pattern(path, &pattern))
}

fn path_matches_pattern(path: &str, pattern: &str) -> bool {
    let normalized_pattern = pattern.trim_end_matches('/');
    path == normalized_pattern
        || path
            .strip_prefix(normalized_pattern)
            .is_some_and(|rest| rest.starts_with('/'))
}

fn render_commit_message(template: &str, action: &str, files: &[String]) -> String {
    let count = files.len().to_string();
    let display = if files.len() <= 5 {
        files.join(", ")
    } else {
        format!("{}, +{} more", files[..5].join(", "), files.len() - 5)
    };

    template
        .replace("{action}", action)
        .replace("{files}", &display)
        .replace("{count}", &count)
}

#[derive(Debug, Default)]
struct BlameLineBuilder {
    line_number: usize,
    commit: String,
    author_name: String,
    author_email: String,
    author_time: i64,
    author_tz: String,
    summary: String,
}

fn parse_git_blame(stdout: &str) -> Result<Vec<GitBlameLine>, GitError> {
    let mut lines = Vec::new();
    let mut current = BlameLineBuilder::default();

    for line in stdout.lines() {
        if let Some(text) = line.strip_prefix('\t') {
            lines.push(GitBlameLine {
                line_number: current.line_number,
                commit: current.commit.clone(),
                author_name: current.author_name.clone(),
                author_email: current.author_email.clone(),
                author_time: current.author_time,
                author_tz: current.author_tz.clone(),
                summary: current.summary.clone(),
                line: text.to_string(),
            });
            current = BlameLineBuilder::default();
            continue;
        }

        if current.commit.is_empty() {
            let mut parts = line.split_whitespace();
            current.commit = parts.next().unwrap_or_default().to_string();
            let _ = parts.next();
            current.line_number = parts
                .next()
                .and_then(|part| part.parse::<usize>().ok())
                .unwrap_or_default();
            continue;
        }

        if let Some(value) = line.strip_prefix("author ") {
            current.author_name = value.to_string();
        } else if let Some(value) = line.strip_prefix("author-mail ") {
            current.author_email = value
                .trim()
                .trim_start_matches('<')
                .trim_end_matches('>')
                .to_string();
        } else if let Some(value) = line.strip_prefix("author-time ") {
            current.author_time = value.parse::<i64>().unwrap_or_default();
        } else if let Some(value) = line.strip_prefix("author-tz ") {
            current.author_tz = value.to_string();
        } else if let Some(value) = line.strip_prefix("summary ") {
            current.summary = value.to_string();
        }
    }

    if lines.is_empty() {
        return Err(GitError::CommandFailed(
            "git blame returned no lines".to_string(),
        ));
    }

    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;
    use tempfile::TempDir;

    fn run_git_ok(vault_root: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(vault_root)
            .args(args)
            .status()
            .expect("git should launch");
        assert!(status.success(), "git command failed: {args:?}");
    }

    fn init_git_repo(vault_root: &Path) {
        run_git_ok(vault_root, &["-c", "init.defaultBranch=main", "init"]);
        run_git_ok(vault_root, &["config", "user.name", "Vulcan Test"]);
        run_git_ok(vault_root, &["config", "user.email", "vulcan@example.com"]);
    }

    fn commit_all(vault_root: &Path, message: &str) {
        run_git_ok(vault_root, &["add", "."]);
        run_git_ok(vault_root, &["commit", "-m", message]);
    }

    fn batch_config() -> GitConfig {
        GitConfig {
            auto_commit: true,
            exclude: vec!["Private".to_string()],
            ..GitConfig::default()
        }
    }

    fn git_stdout(vault_root: &Path, args: &[&str]) -> String {
        run_git_capture(vault_root, |command| {
            command.args(args);
        })
        .expect("git output")
    }

    fn batch_fixture() -> TempDir {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let root = temp_dir.path();
        init_git_repo(root);
        fs::create_dir_all(root.join("Projects")).expect("projects");
        fs::create_dir_all(root.join("Private")).expect("private");
        fs::write(root.join("Home.md"), "home\n").expect("home");
        fs::write(root.join("Gone.md"), "gone\n").expect("gone");
        fs::write(root.join("Staged.md"), "staged\n").expect("staged");
        commit_all(root, "Initial");
        fs::write(root.join("Home.md"), "home edited\n").expect("home edit");
        fs::remove_file(root.join("Gone.md")).expect("delete");
        fs::write(root.join("Projects/Plan.md"), "plan\n").expect("new note");
        fs::write(root.join("Private/Diary.md"), "secret\n").expect("excluded");
        fs::write(root.join("Staged.md"), "staged by hand\n").expect("staged edit");
        run_git_ok(root, &["add", "Staged.md"]);
        temp_dir
    }

    #[test]
    fn batch_candidates_leave_staged_and_excluded_paths_alone() {
        let temp_dir = batch_fixture();
        let candidates =
            batch_commit_candidates(temp_dir.path(), &batch_config()).expect("candidates");

        assert_eq!(
            candidates.paths,
            ["Gone.md", "Home.md", "Projects/Plan.md"].map(String::from)
        );
        assert_eq!(candidates.left_staged, ["Staged.md".to_string()]);
        assert!(candidates.blocked.is_none());
        assert!(candidates.head.is_some());
    }

    #[test]
    fn batch_groups_commit_in_order_and_keep_manual_staging() {
        let temp_dir = batch_fixture();
        let root = temp_dir.path();
        let config = batch_config();
        let planned = batch_commit_candidates(root, &config).expect("candidates");
        let groups = [
            BatchCommitGroup {
                message: "Plan the project".to_string(),
                paths: vec!["Projects/Plan.md".to_string()],
            },
            BatchCommitGroup {
                message: "Tidy the home note".to_string(),
                paths: vec!["Gone.md".to_string(), "Home.md".to_string()],
            },
        ];

        let BatchCommitOutcome::Committed { commits } =
            commit_batch_groups(root, &config, &planned, &groups).expect("commit")
        else {
            panic!("batch should commit");
        };
        assert_eq!(commits.len(), 2);
        assert_eq!(
            git_stdout(root, &["log", "--format=%s", "-3"]),
            "Tidy the home note\nPlan the project\nInitial\n"
        );
        assert_eq!(
            git_stdout(root, &["show", "--name-only", "--format=", "HEAD"]),
            "Gone.md\nHome.md\n"
        );
        let status = git_status(root).expect("status");
        assert_eq!(status.staged, ["Staged.md".to_string()]);
        assert_eq!(status.untracked, ["Private/Diary.md".to_string()]);
        assert_eq!(
            git_stdout(root, &["diff", "--cached", "--", "Staged.md"])
                .lines()
                .last(),
            Some("+staged by hand")
        );
    }

    #[test]
    fn batch_commit_refuses_a_plan_the_work_tree_has_outgrown() {
        let temp_dir = batch_fixture();
        let root = temp_dir.path();
        let config = batch_config();
        let planned = batch_commit_candidates(root, &config).expect("candidates");
        fs::write(root.join("Late.md"), "late\n").expect("late edit");

        let outcome = commit_batch_groups(
            root,
            &config,
            &planned,
            &[BatchCommitGroup {
                message: "batch".to_string(),
                paths: planned.paths.clone(),
            }],
        )
        .expect("outcome");
        let BatchCommitOutcome::Changed { current } = outcome else {
            panic!("a changed work tree should be reported");
        };
        assert!(current.paths.contains(&"Late.md".to_string()));
        assert_eq!(git_stdout(root, &["rev-list", "--count", "HEAD"]), "1\n");
    }

    #[test]
    fn batch_commit_rejects_groups_that_do_not_cover_the_plan_exactly() {
        let temp_dir = batch_fixture();
        let root = temp_dir.path();
        let config = batch_config();
        let planned = batch_commit_candidates(root, &config).expect("candidates");
        for groups in [
            vec![BatchCommitGroup {
                message: "partial".to_string(),
                paths: vec!["Home.md".to_string()],
            }],
            vec![BatchCommitGroup {
                message: "foreign".to_string(),
                paths: vec![
                    "Gone.md".to_string(),
                    "Home.md".to_string(),
                    "Projects/Plan.md".to_string(),
                    "Staged.md".to_string(),
                ],
            }],
        ] {
            assert!(commit_batch_groups(root, &config, &planned, &groups).is_err());
        }
        assert_eq!(git_stdout(root, &["rev-list", "--count", "HEAD"]), "1\n");
        assert_eq!(
            git_status(root).expect("status").staged,
            ["Staged.md".to_string()]
        );
    }

    #[test]
    fn batch_commit_waits_out_merges_and_detached_heads() {
        let temp_dir = batch_fixture();
        let root = temp_dir.path();
        let config = batch_config();
        let git_dir = PathBuf::from(git_stdout(root, &["rev-parse", "--absolute-git-dir"]).trim());
        let head = git_stdout(root, &["rev-parse", "HEAD"]);
        fs::write(git_dir.join("MERGE_HEAD"), &head).expect("merge marker");
        let candidates = batch_commit_candidates(root, &config).expect("candidates");
        assert_eq!(
            candidates.blocked.as_deref(),
            Some("a merge is in progress")
        );
        let outcome = commit_batch_groups(root, &config, &candidates, &[]).expect("outcome");
        assert!(matches!(outcome, BatchCommitOutcome::Blocked { .. }));
        fs::remove_file(git_dir.join("MERGE_HEAD")).expect("clear merge");

        run_git_ok(root, &["checkout", "-q", "--detach"]);
        assert_eq!(
            batch_commit_candidates(root, &config)
                .expect("candidates")
                .blocked
                .as_deref(),
            Some("HEAD is detached")
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_batch_commit_unstages_the_paths_it_did_not_commit() {
        use std::os::unix::fs::PermissionsExt;

        let temp_dir = batch_fixture();
        let root = temp_dir.path();
        let hook = PathBuf::from(git_stdout(root, &["rev-parse", "--absolute-git-dir"]).trim())
            .join("hooks/commit-msg");
        fs::create_dir_all(hook.parent().expect("hooks directory")).expect("hooks");
        fs::write(&hook, "#!/bin/sh\n! grep -q refuse \"$1\"\n").expect("hook");
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).expect("hook mode");
        let config = batch_config();
        let planned = batch_commit_candidates(root, &config).expect("candidates");
        let groups = [
            BatchCommitGroup {
                message: "Plan the project".to_string(),
                paths: vec!["Projects/Plan.md".to_string()],
            },
            BatchCommitGroup {
                message: "refuse this one".to_string(),
                paths: vec!["Gone.md".to_string(), "Home.md".to_string()],
            },
        ];

        assert!(commit_batch_groups(root, &config, &planned, &groups).is_err());
        assert_eq!(
            git_stdout(root, &["log", "--format=%s", "-1"]),
            "Plan the project\n"
        );
        let status = git_status(root).expect("status");
        assert_eq!(status.staged, ["Staged.md".to_string()]);
        assert_eq!(status.unstaged, ["Gone.md", "Home.md"].map(String::from));
    }

    #[test]
    fn batch_patches_name_only_vault_paths_and_respect_their_budget() {
        let temp_dir = batch_fixture();
        let root = temp_dir.path();
        let candidates = batch_commit_candidates(root, &batch_config()).expect("candidates");
        let patches = batch_commit_patches(root, &candidates, 64 * 1024)
            .expect("patches")
            .expect("within budget");

        let by_path = patches
            .into_iter()
            .collect::<std::collections::BTreeMap<_, _>>();
        assert!(by_path["Home.md"].contains("+home edited"));
        assert!(by_path["Gone.md"].contains("-gone"));
        assert_eq!(
            by_path["Projects/Plan.md"],
            "--- /dev/null\n+++ b/Projects/Plan.md\n+plan\n"
        );
        let root_text = root.to_string_lossy();
        assert!(by_path
            .values()
            .all(|patch| !patch.contains(root_text.as_ref())));
        assert!(batch_commit_patches(root, &candidates, 16)
            .expect("patches")
            .is_none());
    }

    #[test]
    fn batch_commit_works_on_an_unborn_branch() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let root = temp_dir.path();
        init_git_repo(root);
        fs::write(root.join("First.md"), "first\n").expect("first note");
        let config = batch_config();
        let planned = batch_commit_candidates(root, &config).expect("candidates");
        assert!(planned.head.is_none());

        let outcome = commit_batch_groups(
            root,
            &config,
            &planned,
            &[BatchCommitGroup {
                message: "First note".to_string(),
                paths: planned.paths.clone(),
            }],
        )
        .expect("commit");
        assert!(
            matches!(outcome, BatchCommitOutcome::Committed { ref commits } if commits.len() == 1)
        );
    }

    #[test]
    fn is_git_repo_detects_initialized_repository() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        assert!(!is_git_repo(temp_dir.path()));
        init_git_repo(temp_dir.path());
        assert!(is_git_repo(temp_dir.path()));
    }

    #[test]
    fn git_log_returns_entries_for_a_tracked_file() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        init_git_repo(temp_dir.path());

        let note_path = temp_dir.path().join("Home.md");
        fs::write(&note_path, "# Home\n").expect("note should be written");
        run_git_ok(temp_dir.path(), &["add", "Home.md"]);
        run_git_ok(temp_dir.path(), &["commit", "-m", "Add home"]);

        fs::write(&note_path, "# Home\nUpdated\n").expect("note should be updated");
        run_git_ok(temp_dir.path(), &["add", "Home.md"]);
        run_git_ok(temp_dir.path(), &["commit", "-m", "Update home"]);

        let entries = git_log(temp_dir.path(), "Home.md", 10).expect("git log should succeed");

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].summary, "Update home");
        assert_eq!(entries[1].summary, "Add home");
        assert_eq!(entries[0].author_name, "Vulcan Test");
        assert_eq!(entries[0].author_email, "vulcan@example.com");
    }

    #[test]
    fn git_status_reports_staged_unstaged_and_untracked_files() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        init_git_repo(temp_dir.path());
        fs::write(temp_dir.path().join("Tracked.md"), "alpha\n").expect("tracked note");
        commit_all(temp_dir.path(), "Initial");

        fs::write(temp_dir.path().join("Tracked.md"), "beta\n").expect("tracked note update");
        fs::write(temp_dir.path().join("Draft.md"), "draft\n").expect("untracked note");
        run_git_ok(temp_dir.path(), &["add", "Tracked.md"]);
        fs::write(temp_dir.path().join("Tracked.md"), "gamma\n").expect("unstaged note update");

        let status = git_status(temp_dir.path()).expect("git status should succeed");

        assert!(!status.clean);
        assert_eq!(status.staged, vec!["Tracked.md".to_string()]);
        assert_eq!(status.unstaged, vec!["Tracked.md".to_string()]);
        assert_eq!(status.untracked, vec!["Draft.md".to_string()]);
        assert_eq!(
            status.changed_paths(),
            vec!["Draft.md".to_string(), "Tracked.md".to_string()]
        );
    }

    #[test]
    fn auto_commit_commits_only_requested_files() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        init_git_repo(temp_dir.path());
        fs::write(temp_dir.path().join("Home.md"), "home\n").expect("home note");
        fs::write(temp_dir.path().join("Other.md"), "other\n").expect("other note");
        commit_all(temp_dir.path(), "Initial");

        fs::write(temp_dir.path().join("Home.md"), "updated\n").expect("home update");
        fs::write(temp_dir.path().join("Other.md"), "changed\n").expect("other update");

        let report = auto_commit(
            temp_dir.path(),
            &GitConfig {
                auto_commit: true,
                message: "vulcan {action}: {files}".to_string(),
                ..GitConfig::default()
            },
            "edit",
            &[String::from("Home.md")],
        )
        .expect("auto-commit should succeed");

        assert!(report.committed);
        assert_eq!(report.files, vec!["Home.md".to_string()]);
        assert_eq!(report.message, "vulcan edit: Home.md");
        assert!(report.sha.is_some());

        let status = git_status(temp_dir.path()).expect("status should succeed");
        assert_eq!(status.unstaged, vec!["Other.md".to_string()]);
    }

    #[test]
    fn explicit_commit_respects_the_shared_repository_mutation_lock() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        init_git_repo(temp_dir.path());
        fs::write(temp_dir.path().join("Home.md"), "home\n").expect("home note");
        commit_all(temp_dir.path(), "Initial");
        fs::write(temp_dir.path().join("Home.md"), "changed\n").expect("home update");
        let git_dir = PathBuf::from(
            run_git_capture(temp_dir.path(), |command| {
                command.args(["rev-parse", "--absolute-git-dir"]);
            })
            .expect("git dir")
            .trim(),
        );
        let _held = vulcan_sync::RepositoryLock::acquire(&git_dir).expect("repository lock");

        let error = git_commit(temp_dir.path(), "blocked").expect_err("lock contention");
        assert!(error
            .to_string()
            .contains("another Vulcan mutation holds the repository lock"));
        assert_eq!(
            fs::read_to_string(temp_dir.path().join("Home.md")).expect("home contents"),
            "changed\n"
        );
    }

    #[test]
    fn auto_commit_rejects_while_sync_holds_the_repository_lock() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        init_git_repo(temp_dir.path());
        fs::write(temp_dir.path().join("Home.md"), "home\n").expect("home note");
        commit_all(temp_dir.path(), "Initial");
        fs::write(temp_dir.path().join("Home.md"), "changed\n").expect("home update");
        let git_dir = PathBuf::from(
            run_git_capture(temp_dir.path(), |command| {
                command.args(["rev-parse", "--absolute-git-dir"]);
            })
            .expect("git dir")
            .trim(),
        );
        let _sync_lock =
            vulcan_sync::RepositoryLock::acquire(&git_dir).expect("simulated sync lock");

        let error = auto_commit(
            temp_dir.path(),
            &GitConfig {
                auto_commit: true,
                ..GitConfig::default()
            },
            "edit",
            &["Home.md".to_string()],
        )
        .expect_err("auto-commit must not overlap sync");
        assert!(error
            .to_string()
            .contains("another Vulcan mutation holds the repository lock"));
        assert_eq!(
            git_status(temp_dir.path()).expect("status").unstaged,
            ["Home.md"]
        );
    }

    #[test]
    fn auto_commit_excludes_internal_and_configured_paths() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        init_git_repo(temp_dir.path());
        fs::create_dir_all(temp_dir.path().join(".vulcan")).expect("vulcan dir");
        fs::create_dir_all(temp_dir.path().join(".obsidian")).expect("obsidian dir");
        fs::write(temp_dir.path().join("Home.md"), "home\n").expect("home note");
        fs::write(temp_dir.path().join(".vulcan/cache.db"), "cache\n").expect("cache db");
        fs::write(temp_dir.path().join(".obsidian/workspace.json"), "{}\n")
            .expect("workspace config");
        commit_all(temp_dir.path(), "Initial");

        fs::write(temp_dir.path().join("Home.md"), "updated\n").expect("home update");
        fs::write(temp_dir.path().join(".vulcan/cache.db"), "cache2\n").expect("cache update");
        fs::write(
            temp_dir.path().join(".obsidian/workspace.json"),
            "{\"open\":true}\n",
        )
        .expect("workspace update");

        let report = auto_commit(
            temp_dir.path(),
            &GitConfig {
                auto_commit: true,
                exclude: vec![".obsidian/workspace.json".to_string()],
                ..GitConfig::default()
            },
            "scan",
            &[
                String::from("Home.md"),
                String::from(".vulcan/cache.db"),
                String::from(".obsidian/workspace.json"),
            ],
        )
        .expect("auto-commit should succeed");

        assert!(report.committed);
        assert_eq!(report.files, vec!["Home.md".to_string()]);
        let status = git_status(temp_dir.path()).expect("status should succeed");
        assert_eq!(
            status.changed_paths(),
            vec![
                ".obsidian/workspace.json".to_string(),
                ".vulcan/cache.db".to_string()
            ]
        );
    }

    #[test]
    fn auto_commit_all_scope_uses_git_status_paths() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        init_git_repo(temp_dir.path());
        fs::write(temp_dir.path().join("A.md"), "a\n").expect("a note");
        fs::write(temp_dir.path().join("B.md"), "b\n").expect("b note");
        commit_all(temp_dir.path(), "Initial");

        fs::write(temp_dir.path().join("A.md"), "aa\n").expect("a update");
        fs::write(temp_dir.path().join("B.md"), "bb\n").expect("b update");

        let report = auto_commit(
            temp_dir.path(),
            &GitConfig {
                auto_commit: true,
                scope: GitScope::All,
                message: "sync {count}".to_string(),
                ..GitConfig::default()
            },
            "scan",
            &[],
        )
        .expect("auto-commit should succeed");

        assert!(report.committed);
        assert_eq!(report.files, vec!["A.md".to_string(), "B.md".to_string()]);
        assert_eq!(report.message, "sync 2");
        assert!(
            git_status(temp_dir.path())
                .expect("status should succeed")
                .clean
        );
    }

    #[test]
    fn nested_vault_git_operations_reject_and_ignore_outside_paths() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        fs::create_dir(&vault_root).expect("vault should create");
        init_git_repo(temp_dir.path());
        fs::write(vault_root.join("Note.md"), "inside\n").expect("vault note should write");
        fs::write(temp_dir.path().join("Sibling.md"), "outside\n").expect("sibling should write");
        commit_all(temp_dir.path(), "Initial");

        fs::write(vault_root.join("Note.md"), "inside changed\n")
            .expect("vault note should update");
        fs::write(temp_dir.path().join("Sibling.md"), "outside changed\n")
            .expect("sibling should update");

        let error =
            git_diff(&vault_root, Some("../Sibling.md")).expect_err("parent path must be rejected");
        assert!(
            error.to_string().contains("must stay within the vault"),
            "unexpected error: {error}"
        );

        let report = auto_commit(
            &vault_root,
            &GitConfig {
                auto_commit: true,
                scope: GitScope::All,
                message: "vault only".to_string(),
                ..GitConfig::default()
            },
            "scan",
            &["../Sibling.md".to_string(), "Note.md".to_string()],
        )
        .expect("vault commit should succeed");

        assert_eq!(report.files, vec!["Note.md".to_string()]);
        let sibling_status = run_git_capture(temp_dir.path(), |command| {
            command.args(["status", "--short", "--", "Sibling.md"]);
        })
        .expect("sibling status should load");
        assert!(!sibling_status.trim().is_empty());
    }

    #[test]
    fn repository_layout_reports_the_vault_prefix() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        init_git_repo(temp_dir.path());
        let vault_root = temp_dir.path().join("site/docs");
        fs::create_dir_all(&vault_root).expect("vault should create");

        let nested = git_repository_layout(&vault_root).expect("nested layout");
        assert_eq!(nested.vault_prefix, "site/docs");
        assert!(nested.is_nested());
        assert_eq!(nested.repository_path("a/b.md"), "site/docs/a/b.md");
        assert_eq!(
            fs::canonicalize(&nested.work_tree).expect("work tree"),
            fs::canonicalize(temp_dir.path()).expect("temp dir")
        );

        let root = git_repository_layout(temp_dir.path()).expect("root layout");
        assert_eq!(root.vault_prefix, "");
        assert!(!root.is_nested());
        assert_eq!(root.repository_path("a.md"), "a.md");
    }

    #[test]
    fn nested_vault_status_log_and_commit_stay_vault_relative() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("docs");
        fs::create_dir_all(vault_root.join("guide")).expect("vault should create");
        init_git_repo(temp_dir.path());
        fs::write(vault_root.join("index.md"), "home\n").expect("home");
        fs::write(temp_dir.path().join("README.md"), "readme\n").expect("readme");
        commit_all(temp_dir.path(), "Initial");
        fs::write(temp_dir.path().join("README.md"), "readme changed\n").expect("readme");
        commit_all(temp_dir.path(), "Outside only");

        fs::write(vault_root.join("index.md"), "home changed\n").expect("home");
        fs::write(vault_root.join("guide/setup.md"), "setup\n").expect("setup");
        fs::write(temp_dir.path().join("main.rs"), "fn main() {}\n").expect("code");

        let status = git_status(&vault_root).expect("status");
        assert_eq!(status.unstaged, vec!["index.md".to_string()]);
        assert_eq!(status.untracked, vec!["guide/setup.md".to_string()]);

        let history = git_recent_log(&vault_root, 10).expect("log");
        assert_eq!(
            history
                .iter()
                .map(|entry| entry.summary.as_str())
                .collect::<Vec<_>>(),
            vec!["Initial"]
        );

        let report = git_commit(&vault_root, "Docs").expect("commit");
        assert_eq!(
            report.files,
            vec!["guide/setup.md".to_string(), "index.md".to_string()]
        );
        let outside = run_git_capture(temp_dir.path(), |command| {
            command.args(["status", "--short", "--", "main.rs"]);
        })
        .expect("outside status");
        assert!(outside.starts_with("??"), "outside file stays untracked");
    }

    #[test]
    fn git_recent_log_returns_repo_history_without_a_path_filter() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        init_git_repo(temp_dir.path());
        fs::write(temp_dir.path().join("Home.md"), "home\n").expect("home note");
        commit_all(temp_dir.path(), "Add home");
        fs::write(temp_dir.path().join("Other.md"), "other\n").expect("other note");
        commit_all(temp_dir.path(), "Add other");

        let entries = git_recent_log(temp_dir.path(), 10).expect("git log should succeed");

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].summary, "Add other");
        assert_eq!(entries[1].summary, "Add home");
    }

    #[test]
    fn git_diff_includes_untracked_files_and_filters_internal_paths() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        init_git_repo(temp_dir.path());
        fs::create_dir_all(temp_dir.path().join(".vulcan")).expect("vulcan dir");
        fs::write(temp_dir.path().join("Home.md"), "home\n").expect("home note");
        fs::write(temp_dir.path().join(".vulcan/cache.db"), "cache\n").expect("cache file");
        commit_all(temp_dir.path(), "Initial");

        fs::write(temp_dir.path().join("Home.md"), "updated\n").expect("home update");
        fs::write(temp_dir.path().join("Draft.md"), "draft\n").expect("draft note");
        fs::write(temp_dir.path().join(".vulcan/cache.db"), "cache2\n").expect("cache update");

        let diff = git_diff(temp_dir.path(), None).expect("git diff should succeed");

        assert!(diff.contains("Home.md"));
        assert!(diff.contains("Draft.md"));
        assert!(!diff.contains(".vulcan/cache.db"));
    }

    #[test]
    fn git_commit_stages_vault_files_but_skips_internal_state() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        init_git_repo(temp_dir.path());
        fs::create_dir_all(temp_dir.path().join(".vulcan")).expect("vulcan dir");
        fs::write(temp_dir.path().join("Home.md"), "home\n").expect("home note");
        fs::write(temp_dir.path().join(".vulcan/cache.db"), "cache\n").expect("cache file");
        commit_all(temp_dir.path(), "Initial");

        fs::write(temp_dir.path().join("Home.md"), "updated\n").expect("home update");
        fs::write(temp_dir.path().join(".vulcan/cache.db"), "cache2\n").expect("cache update");

        let report = git_commit(temp_dir.path(), "Commit home").expect("git commit should work");

        assert!(report.committed);
        assert_eq!(report.files, vec!["Home.md".to_string()]);
        assert_eq!(report.message, "Commit home");
        assert!(report.sha.is_some());
        assert_eq!(
            git_status(temp_dir.path())
                .expect("status should succeed")
                .changed_paths(),
            vec![".vulcan/cache.db".to_string()]
        );
    }

    #[test]
    fn git_blame_reports_line_metadata() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        init_git_repo(temp_dir.path());
        let note_path = temp_dir.path().join("Home.md");
        fs::write(&note_path, "alpha\nbeta\n").expect("note should be written");
        commit_all(temp_dir.path(), "Initial");
        fs::write(&note_path, "alpha\nbeta updated\n").expect("note should be updated");
        run_git_ok(temp_dir.path(), &["add", "Home.md"]);
        run_git_ok(temp_dir.path(), &["commit", "-m", "Update beta"]);

        let lines = git_blame(temp_dir.path(), "Home.md").expect("git blame should succeed");

        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].line_number, 1);
        assert_eq!(lines[0].line, "alpha");
        assert_eq!(lines[1].line_number, 2);
        assert_eq!(lines[1].line, "beta updated");
        assert_eq!(lines[1].summary, "Update beta");
        assert_eq!(lines[1].author_name, "Vulcan Test");
        assert_eq!(lines[1].author_email, "vulcan@example.com");
    }
}
