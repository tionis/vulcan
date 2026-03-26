use serde::Serialize;
use std::error::Error;
use std::fmt::{Display, Formatter};
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
pub struct GitLogEntry {
    pub commit: String,
    pub author_name: String,
    pub author_email: String,
    pub committed_at: String,
    pub summary: String,
}

pub fn is_git_repo(vault_root: &Path) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(vault_root)
        .args(["rev-parse", "--git-dir"])
        .output()
        .is_ok_and(|output| output.status.success())
}

pub fn git_log(
    vault_root: &Path,
    file_path: &str,
    limit: usize,
) -> Result<Vec<GitLogEntry>, GitError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(vault_root)
        .args([
            "log",
            "--follow",
            "--date=iso-strict",
            &format!("-n{limit}"),
            "--pretty=format:%H%x1f%an%x1f%ae%x1f%ad%x1f%s",
            "--",
            file_path,
        ])
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8(output.stderr)?;
        return Err(GitError::CommandFailed(stderr.trim().to_string()));
    }

    let stdout = String::from_utf8(output.stdout)?;
    Ok(stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(parse_git_log_line)
        .collect())
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;
    use tempfile::TempDir;

    fn run_git(vault_root: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(vault_root)
            .args(args)
            .status()
            .expect("git should launch");
        assert!(status.success(), "git command failed: {:?}", args);
    }

    fn init_git_repo(vault_root: &Path) {
        run_git(vault_root, &["init"]);
        run_git(vault_root, &["config", "user.name", "Vulcan Test"]);
        run_git(vault_root, &["config", "user.email", "vulcan@example.com"]);
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
        run_git(temp_dir.path(), &["add", "Home.md"]);
        run_git(temp_dir.path(), &["commit", "-m", "Add home"]);

        fs::write(&note_path, "# Home\nUpdated\n").expect("note should be updated");
        run_git(temp_dir.path(), &["add", "Home.md"]);
        run_git(temp_dir.path(), &["commit", "-m", "Update home"]);

        let entries = git_log(temp_dir.path(), "Home.md", 10).expect("git log should succeed");

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].summary, "Update home");
        assert_eq!(entries[1].summary, "Add home");
        assert_eq!(entries[0].author_name, "Vulcan Test");
        assert_eq!(entries[0].author_email, "vulcan@example.com");
    }
}
