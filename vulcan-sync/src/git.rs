use serde::Serialize;
use std::error::Error;
use std::ffi::OsStr;
use std::fmt::{Display, Formatter};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const MAX_ERROR_BYTES: usize = 16 * 1024;
const REPOSITORY_ENVIRONMENT_OVERRIDES: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_NAMESPACE",
];

/// A typed boundary over the repository implementation used by Git-backed sync.
pub trait GitEngine: Send + Sync {
    fn kind(&self) -> GitEngineKind;

    fn installation(&self) -> Result<GitInstallation, GitEngineError>;

    fn discover_repository(&self, path: &Path) -> Result<GitRepository, GitEngineError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum GitEngineKind {
    Cli,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitInstallation {
    pub engine: GitEngineKind,
    pub executable: PathBuf,
    pub version: GitVersion,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitVersion {
    pub raw: String,
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
    pub vendor_suffix: Option<String>,
}

impl GitVersion {
    fn parse(stdout: &str) -> Result<Self, GitEngineError> {
        let raw = stdout.trim();
        let version =
            raw.strip_prefix("git version ")
                .ok_or_else(|| GitEngineError::InvalidOutput {
                    operation: "inspect Git installation",
                    detail: format!("expected `git version ...`, received `{raw}`"),
                })?;
        let numeric_prefix_len = version
            .bytes()
            .take_while(|byte| byte.is_ascii_digit() || *byte == b'.')
            .count();
        let numeric = version[..numeric_prefix_len].trim_end_matches('.');
        let numeric_len = numeric.len();
        let mut components = numeric.split('.');
        let major = parse_version_component(components.next(), "major", raw)?;
        let minor = parse_version_component(components.next(), "minor", raw)?;
        let patch = components
            .next()
            .map(|value| parse_version_component(Some(value), "patch", raw))
            .transpose()?
            .unwrap_or(0);
        let vendor_suffix = version[numeric_len..]
            .strip_prefix('.')
            .filter(|suffix| !suffix.is_empty())
            .map(ToOwned::to_owned);

        Ok(Self {
            raw: raw.to_string(),
            major,
            minor,
            patch,
            vendor_suffix,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum GitObjectFormat {
    Sha1,
    Sha256,
    Other(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum GitRepositoryLayout {
    Colocated,
    Detached,
    Bare,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitRepository {
    pub git_dir: PathBuf,
    pub common_dir: PathBuf,
    pub work_tree: Option<PathBuf>,
    pub layout: GitRepositoryLayout,
    pub object_format: GitObjectFormat,
}

#[derive(Debug)]
pub enum GitEngineError {
    ExecutableUnavailable {
        executable: PathBuf,
        source: std::io::Error,
    },
    CommandFailed {
        operation: &'static str,
        exit_code: Option<i32>,
        stderr: String,
    },
    InvalidOutput {
        operation: &'static str,
        detail: String,
    },
    Io(std::io::Error),
}

impl Display for GitEngineError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ExecutableUnavailable { executable, source } => write!(
                formatter,
                "Git executable `{}` is unavailable: {source}",
                executable.display()
            ),
            Self::CommandFailed {
                operation,
                exit_code,
                stderr,
            } => {
                write!(formatter, "Git failed to {operation}")?;
                if let Some(exit_code) = exit_code {
                    write!(formatter, " (exit code {exit_code})")?;
                }
                if !stderr.is_empty() {
                    write!(formatter, ": {stderr}")?;
                }
                Ok(())
            }
            Self::InvalidOutput { operation, detail } => {
                write!(
                    formatter,
                    "Git returned invalid output while trying to {operation}: {detail}"
                )
            }
            Self::Io(error) => Display::fmt(error, formatter),
        }
    }
}

impl Error for GitEngineError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::ExecutableUnavailable { source, .. } | Self::Io(source) => Some(source),
            Self::CommandFailed { .. } | Self::InvalidOutput { .. } => None,
        }
    }
}

impl From<std::io::Error> for GitEngineError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitCliEngine {
    executable: PathBuf,
}

impl Default for GitCliEngine {
    fn default() -> Self {
        Self::new("git")
    }
}

impl GitCliEngine {
    #[must_use]
    pub fn new(executable: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
        }
    }

    #[must_use]
    pub fn executable(&self) -> &Path {
        &self.executable
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.executable);
        for variable in REPOSITORY_ENVIRONMENT_OVERRIDES {
            command.env_remove(variable);
        }
        command
            .env("LC_ALL", "C")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_OPTIONAL_LOCKS", "0");
        command
    }

    fn output(
        &self,
        operation: &'static str,
        repository_path: Option<&Path>,
        arguments: impl IntoIterator<Item = impl AsRef<OsStr>>,
    ) -> Result<Output, GitEngineError> {
        let mut command = self.command();
        if let Some(repository_path) = repository_path {
            command.arg("-C").arg(repository_path);
        }
        command.args(arguments);
        let output = command.output().map_err(|source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                GitEngineError::ExecutableUnavailable {
                    executable: self.executable.clone(),
                    source,
                }
            } else {
                GitEngineError::Io(source)
            }
        })?;
        if output.status.success() {
            return Ok(output);
        }

        Err(GitEngineError::CommandFailed {
            operation,
            exit_code: output.status.code(),
            stderr: bounded_lossy(&output.stderr),
        })
    }

    fn capture(
        &self,
        operation: &'static str,
        repository_path: Option<&Path>,
        arguments: impl IntoIterator<Item = impl AsRef<OsStr>>,
    ) -> Result<String, GitEngineError> {
        let output = self.output(operation, repository_path, arguments)?;
        String::from_utf8(output.stdout).map_err(|error| GitEngineError::InvalidOutput {
            operation,
            detail: error.to_string(),
        })
    }

    fn rev_parse(
        &self,
        path: &Path,
        operation: &'static str,
        argument: &str,
    ) -> Result<String, GitEngineError> {
        self.capture(operation, Some(path), ["rev-parse", argument])
            .map(|value| value.trim().to_string())
    }
}

impl GitEngine for GitCliEngine {
    fn kind(&self) -> GitEngineKind {
        GitEngineKind::Cli
    }

    fn installation(&self) -> Result<GitInstallation, GitEngineError> {
        let stdout = self.capture("inspect Git installation", None, ["--version"])?;
        Ok(GitInstallation {
            engine: self.kind(),
            executable: self.executable.clone(),
            version: GitVersion::parse(&stdout)?,
        })
    }

    fn discover_repository(&self, path: &Path) -> Result<GitRepository, GitEngineError> {
        let git_dir = absolute_path(self.rev_parse(
            path,
            "discover the repository Git directory",
            "--absolute-git-dir",
        )?)?;
        let common_dir = absolute_path(
            self.capture(
                "discover the repository common directory",
                Some(path),
                ["rev-parse", "--path-format=absolute", "--git-common-dir"],
            )
            .map(|value| value.trim().to_string())?,
        )?;
        let is_bare = parse_git_bool(
            &self.rev_parse(
                path,
                "determine whether the repository is bare",
                "--is-bare-repository",
            )?,
            "determine whether the repository is bare",
        )?;
        let object_format = match self
            .rev_parse(
                path,
                "discover the repository object format",
                "--show-object-format",
            )?
            .as_str()
        {
            "sha1" => GitObjectFormat::Sha1,
            "sha256" => GitObjectFormat::Sha256,
            other => GitObjectFormat::Other(other.to_string()),
        };

        let work_tree = if is_bare {
            None
        } else {
            Some(absolute_path(self.rev_parse(
                path,
                "discover the repository worktree",
                "--show-toplevel",
            )?)?)
        };
        let layout = match &work_tree {
            None => GitRepositoryLayout::Bare,
            Some(work_tree) if colocated_git_dir(work_tree, &git_dir) => {
                GitRepositoryLayout::Colocated
            }
            Some(_) => GitRepositoryLayout::Detached,
        };

        Ok(GitRepository {
            git_dir,
            common_dir,
            work_tree,
            layout,
            object_format,
        })
    }
}

fn parse_version_component(
    value: Option<&str>,
    name: &str,
    raw: &str,
) -> Result<u32, GitEngineError> {
    value
        .filter(|value| !value.is_empty())
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| GitEngineError::InvalidOutput {
            operation: "inspect Git installation",
            detail: format!("could not parse the {name} component from `{raw}`"),
        })
}

fn parse_git_bool(value: &str, operation: &'static str) -> Result<bool, GitEngineError> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(GitEngineError::InvalidOutput {
            operation,
            detail: format!("expected `true` or `false`, received `{value}`"),
        }),
    }
}

fn absolute_path(value: String) -> Result<PathBuf, GitEngineError> {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        Ok(path)
    } else {
        Err(GitEngineError::InvalidOutput {
            operation: "discover repository paths",
            detail: format!("expected an absolute path, received `{}`", path.display()),
        })
    }
}

fn colocated_git_dir(work_tree: &Path, git_dir: &Path) -> bool {
    let expected = work_tree.join(".git");
    expected == git_dir
        || match (expected.canonicalize(), git_dir.canonicalize()) {
            (Ok(expected), Ok(actual)) => expected == actual,
            (Err(_), _) | (_, Err(_)) => false,
        }
}

fn bounded_lossy(bytes: &[u8]) -> String {
    let truncated = bytes.len() > MAX_ERROR_BYTES;
    let bytes = &bytes[..bytes.len().min(MAX_ERROR_BYTES)];
    let mut rendered = String::from_utf8_lossy(bytes).trim().to_string();
    if truncated {
        rendered.push_str("… [truncated]");
    }
    rendered
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn run_git(current_dir: &Path, arguments: &[&str]) {
        let status = Command::new("git")
            .current_dir(current_dir)
            .args(arguments)
            .status()
            .expect("Git should launch in tests");
        assert!(status.success(), "Git failed with arguments {arguments:?}");
    }

    #[test]
    fn parses_vendor_and_patchless_versions() {
        assert_eq!(
            GitVersion::parse("git version 2.51.0.windows.1\n").expect("version should parse"),
            GitVersion {
                raw: "git version 2.51.0.windows.1".to_string(),
                major: 2,
                minor: 51,
                patch: 0,
                vendor_suffix: Some("windows.1".to_string()),
            }
        );
        assert_eq!(
            GitVersion::parse("git version 2.42\n")
                .expect("version should parse")
                .patch,
            0
        );
    }

    #[test]
    fn installation_reports_cli_version() {
        let installation = GitCliEngine::default()
            .installation()
            .expect("installed Git should be detected");

        assert_eq!(installation.engine, GitEngineKind::Cli);
        assert_eq!(installation.executable, PathBuf::from("git"));
        assert!(installation.version.major >= 2);
    }

    #[test]
    fn missing_executable_is_distinct_from_command_failure() {
        let error = GitCliEngine::new("definitely-not-a-vulcan-git-executable")
            .installation()
            .expect_err("missing executable should fail");

        assert!(matches!(
            error,
            GitEngineError::ExecutableUnavailable { .. }
        ));
    }

    #[test]
    fn non_repository_is_a_command_failure() {
        let temporary = TempDir::new().expect("temporary directory");
        let error = GitCliEngine::default()
            .discover_repository(temporary.path())
            .expect_err("plain directory should not be a repository");

        assert!(matches!(
            error,
            GitEngineError::CommandFailed {
                operation: "discover the repository Git directory",
                ..
            }
        ));
    }

    #[test]
    fn discovers_colocated_repository_from_nested_directory() {
        let temporary = TempDir::new().expect("temporary directory");
        run_git(temporary.path(), &["init", "--quiet"]);
        let nested = temporary.path().join("notes/projects");
        fs::create_dir_all(&nested).expect("nested directory");

        let repository = GitCliEngine::default()
            .discover_repository(&nested)
            .expect("repository should be discovered");

        assert_eq!(repository.layout, GitRepositoryLayout::Colocated);
        assert_eq!(repository.object_format, GitObjectFormat::Sha1);
        assert_eq!(
            repository.work_tree,
            Some(temporary.path().canonicalize().expect("canonical root"))
        );
        assert_eq!(
            repository.git_dir,
            temporary
                .path()
                .join(".git")
                .canonicalize()
                .expect("canonical Git directory")
        );
        assert_eq!(repository.common_dir, repository.git_dir);
    }

    #[test]
    fn discovers_detached_git_directory() {
        let temporary = TempDir::new().expect("temporary directory");
        let work_tree = temporary.path().join("wiki");
        let git_dir = temporary.path().join("git-data");
        run_git(
            temporary.path(),
            &[
                "init",
                "--quiet",
                "--separate-git-dir",
                git_dir.to_str().expect("UTF-8 test path"),
                work_tree.to_str().expect("UTF-8 test path"),
            ],
        );

        let repository = GitCliEngine::default()
            .discover_repository(&work_tree)
            .expect("detached repository should be discovered");

        assert_eq!(repository.layout, GitRepositoryLayout::Detached);
        assert_eq!(
            repository.work_tree,
            Some(work_tree.canonicalize().expect("canonical worktree"))
        );
        assert_eq!(
            repository.git_dir,
            git_dir.canonicalize().expect("canonical Git directory")
        );
    }

    #[test]
    fn discovers_bare_repository() {
        let temporary = TempDir::new().expect("temporary directory");
        let bare = temporary.path().join("wiki.git");
        run_git(
            temporary.path(),
            &[
                "init",
                "--quiet",
                "--bare",
                bare.to_str().expect("test path"),
            ],
        );

        let repository = GitCliEngine::default()
            .discover_repository(&bare)
            .expect("bare repository should be discovered");

        assert_eq!(repository.layout, GitRepositoryLayout::Bare);
        assert_eq!(repository.work_tree, None);
        assert_eq!(repository.git_dir, repository.common_dir);
    }

    #[test]
    fn repository_report_has_stable_json_shape() {
        let report = GitRepository {
            git_dir: PathBuf::from("/data/wiki.git"),
            common_dir: PathBuf::from("/data/wiki.git"),
            work_tree: Some(PathBuf::from("/storage/wiki")),
            layout: GitRepositoryLayout::Detached,
            object_format: GitObjectFormat::Sha1,
        };

        assert_eq!(
            serde_json::to_value(report).expect("report should serialize"),
            serde_json::json!({
                "git_dir": "/data/wiki.git",
                "common_dir": "/data/wiki.git",
                "work_tree": "/storage/wiki",
                "layout": "detached",
                "object_format": "sha1"
            })
        );
    }

    #[test]
    fn error_output_is_bounded() {
        let rendered = bounded_lossy(&vec![b'x'; MAX_ERROR_BYTES + 10]);
        assert!(rendered.ends_with("… [truncated]"));
        assert!(rendered.len() < MAX_ERROR_BYTES + 32);
    }

    #[test]
    fn owned_arguments_are_accepted_without_a_shell() {
        let engine = GitCliEngine::default();
        let arguments = vec![std::ffi::OsString::from("--version")];
        let output = engine
            .output("inspect Git installation", None, arguments)
            .expect("owned arguments should work");
        assert!(output.status.success());
    }
}
