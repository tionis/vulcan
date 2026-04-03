use crate::config::{GitConfig, GitScope};
use serde::Serialize;
use std::collections::BTreeSet;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::fs;
use std::path::Path;
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
        command.arg("commit").arg("-m").arg(&message);
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

pub fn git_log(
    vault_root: &Path,
    file_path: &str,
    limit: usize,
) -> Result<Vec<GitLogEntry>, GitError> {
    ensure_git_repo(vault_root)?;
    collect_git_log(vault_root, limit, Some(file_path))
}

pub fn git_recent_log(vault_root: &Path, limit: usize) -> Result<Vec<GitLogEntry>, GitError> {
    ensure_git_repo(vault_root)?;
    collect_git_log(vault_root, limit, None)
}

pub fn git_status(vault_root: &Path) -> Result<GitStatusReport, GitError> {
    ensure_git_repo(vault_root)?;
    let stdout = run_git_capture(vault_root, |command| {
        command.args(["status", "--short", "--untracked-files=all"]);
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
            let normalized = normalize_git_path(path);
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
        command.arg("commit").arg("-m").arg(message);
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
    let normalized = normalize_git_path(path);
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
        .map(|path| normalize_git_path(&path))
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
        command.args(["diff", "--cached", "--name-only"]);
    })?;

    Ok(stdout
        .lines()
        .map(normalize_git_path)
        .filter(|line| !line.is_empty())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect())
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
        let empty_path = std::env::temp_dir().join(format!(
            "vulcan-empty-diff-{}-{}",
            std::process::id(),
            path.replace('/', "_")
        ));
        fs::write(&empty_path, "").map_err(GitError::Io)?;
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
        run_git_ok(vault_root, &["init"]);
        run_git_ok(vault_root, &["config", "user.name", "Vulcan Test"]);
        run_git_ok(vault_root, &["config", "user.email", "vulcan@example.com"]);
    }

    fn commit_all(vault_root: &Path, message: &str) {
        run_git_ok(vault_root, &["add", "."]);
        run_git_ok(vault_root, &["commit", "-m", message]);
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
