use serde::Serialize;
use std::collections::BTreeMap;
use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::fmt::{Display, Formatter};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const MAX_ERROR_BYTES: usize = 16 * 1024;
const MAX_DIAGNOSTIC_PATH_BYTES: usize = 16 * 1024 * 1024;
const MAX_CONFLICT_BLOB_BYTES: usize = 64 * 1024 * 1024;
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

    fn clone_repository(&self, request: &GitCloneRequest) -> Result<GitRepository, GitEngineError>;

    fn read_ref(
        &self,
        repository: &GitRepository,
        reference: &GitRefName,
    ) -> Result<Option<GitOid>, GitEngineError>;

    fn head_commit(&self, repository: &GitRepository) -> Result<Option<GitOid>, GitEngineError>;

    fn resolve_revision(
        &self,
        repository: &GitRepository,
        revision: &str,
    ) -> Result<GitOid, GitEngineError>;

    fn update_ref(
        &self,
        repository: &GitRepository,
        reference: &GitRefName,
        target: &GitOid,
    ) -> Result<(), GitEngineError>;

    fn create_ref(
        &self,
        repository: &GitRepository,
        reference: &GitRefName,
        target: &GitOid,
    ) -> Result<GitRefCreateResult, GitEngineError>;

    fn compare_and_swap_ref(
        &self,
        repository: &GitRepository,
        reference: &GitRefName,
        target: &GitOid,
        expected: Option<&GitOid>,
    ) -> Result<GitRefUpdateResult, GitEngineError>;

    fn tree_oid(
        &self,
        repository: &GitRepository,
        commit: &GitOid,
    ) -> Result<GitOid, GitEngineError>;

    fn path_object(
        &self,
        repository: &GitRepository,
        commit: &GitOid,
        path: &str,
    ) -> Result<Option<GitPathObject>, GitEngineError>;

    fn changed_paths(
        &self,
        repository: &GitRepository,
        from: &GitOid,
        to: &GitOid,
    ) -> Result<Vec<String>, GitEngineError>;

    fn tree_with_paths(
        &self,
        repository: &GitRepository,
        base: &GitOid,
        target: &GitOid,
        paths: &[String],
    ) -> Result<GitOid, GitEngineError>;

    fn diff_patch(
        &self,
        repository: &GitRepository,
        from: &GitOid,
        to: &GitOid,
        paths: &[String],
    ) -> Result<String, GitEngineError>;

    fn capture_worktree(
        &self,
        repository: &GitRepository,
        request: &GitCaptureRequest,
    ) -> Result<GitCapture, GitEngineError>;

    fn remote_ref(
        &self,
        repository: &GitRepository,
        remote: &GitRemote,
        reference: &GitRefName,
    ) -> Result<Option<GitOid>, GitEngineError>;

    fn fetch_ref(
        &self,
        repository: &GitRepository,
        remote: &GitRemote,
        source: &GitRefName,
        destination: &GitRefName,
    ) -> Result<GitOid, GitEngineError>;

    fn is_ancestor(
        &self,
        repository: &GitRepository,
        ancestor: &GitOid,
        descendant: &GitOid,
    ) -> Result<bool, GitEngineError>;

    fn merge_commits(
        &self,
        repository: &GitRepository,
        accepted_remote: &GitOid,
        local_candidate: &GitOid,
    ) -> Result<GitMerge, GitEngineError>;

    fn resolve_merge_tree(
        &self,
        repository: &GitRepository,
        request: &GitMergeResolutionRequest,
    ) -> Result<GitOid, GitEngineError>;

    fn create_commit(
        &self,
        repository: &GitRepository,
        tree: &GitOid,
        parents: &[GitOid],
        message: &str,
    ) -> Result<GitOid, GitEngineError>;

    fn push_ref(
        &self,
        repository: &GitRepository,
        remote: &GitRemote,
        source: &GitOid,
        destination: &GitRefName,
        expected: Option<&GitOid>,
    ) -> Result<GitPushResult, GitEngineError>;

    fn apply_tree(
        &self,
        repository: &GitRepository,
        expected_worktree: &GitOid,
        target: &GitOid,
    ) -> Result<(), GitEngineError>;

    fn safety_state(&self, repository: &GitRepository) -> Result<GitSafetyState, GitEngineError>;

    fn repository_requirements(
        &self,
        repository: &GitRepository,
    ) -> Result<GitRepositoryRequirements, GitEngineError>;
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitCloneRequest {
    pub source: String,
    pub work_tree: PathBuf,
    pub git_dir: Option<PathBuf>,
    pub platform: GitPlatformProfile,
}

impl GitCloneRequest {
    pub fn validate(&self) -> Result<(), GitEngineError> {
        if self.source.is_empty()
            || self.source.starts_with('-')
            || self.source.chars().any(char::is_control)
        {
            return Err(GitEngineError::UnsupportedRepository {
                detail: "Git clone source must be non-empty, must not start with `-`, and must not contain control characters".to_string(),
            });
        }
        if self.work_tree.exists() {
            return Err(GitEngineError::UnsupportedRepository {
                detail: format!(
                    "clone worktree destination already exists: {}",
                    self.work_tree.display()
                ),
            });
        }
        if let Some(git_dir) = &self.git_dir {
            if git_dir.exists() {
                return Err(GitEngineError::UnsupportedRepository {
                    detail: format!(
                        "detached Git directory destination already exists: {}",
                        git_dir.display()
                    ),
                });
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GitPlatformProfile {
    LinuxNative,
    WindowsNative,
    AndroidShared,
    OtherNative,
}

impl GitPlatformProfile {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LinuxNative => "linux_native",
            Self::WindowsNative => "windows_native",
            Self::AndroidShared => "android_shared",
            Self::OtherNative => "other_native",
        }
    }

    #[must_use]
    pub const fn native() -> Self {
        if cfg!(target_os = "linux") {
            Self::LinuxNative
        } else if cfg!(target_os = "windows") {
            Self::WindowsNative
        } else {
            Self::OtherNative
        }
    }

    #[must_use]
    pub fn policy(self) -> GitPlatformPolicy {
        match self {
            Self::AndroidShared => GitPlatformPolicy {
                profile: self,
                executable_bits: GitExecutableBitsPolicy::NotRepresentable,
                symlinks: GitSymlinkPolicy::LinkFiles,
                case_only_renames: GitCaseRenamePolicy::IntermediatePath,
                reserved_names: GitReservedNamesPolicy::WindowsPortable,
                path_length: GitPathLengthPolicy::FilesystemDependent,
                timestamp_precision: GitTimestampPolicy::ContentVerified,
                clone_config: vec![
                    GitCloneConfig {
                        key: "core.fileMode",
                        value: "false",
                    },
                    GitCloneConfig {
                        key: "core.symlinks",
                        value: "false",
                    },
                ],
            },
            Self::WindowsNative => GitPlatformPolicy {
                profile: self,
                executable_bits: GitExecutableBitsPolicy::GitProbed,
                symlinks: GitSymlinkPolicy::GitProbed,
                case_only_renames: GitCaseRenamePolicy::IntermediatePath,
                reserved_names: GitReservedNamesPolicy::WindowsRestricted,
                path_length: GitPathLengthPolicy::FilesystemDependent,
                timestamp_precision: GitTimestampPolicy::ContentVerified,
                clone_config: Vec::new(),
            },
            Self::LinuxNative => GitPlatformPolicy {
                profile: self,
                executable_bits: GitExecutableBitsPolicy::GitProbed,
                symlinks: GitSymlinkPolicy::GitProbed,
                case_only_renames: GitCaseRenamePolicy::Native,
                reserved_names: GitReservedNamesPolicy::Native,
                path_length: GitPathLengthPolicy::FilesystemDependent,
                timestamp_precision: GitTimestampPolicy::ContentVerified,
                clone_config: Vec::new(),
            },
            Self::OtherNative => GitPlatformPolicy {
                profile: self,
                executable_bits: GitExecutableBitsPolicy::GitProbed,
                symlinks: GitSymlinkPolicy::GitProbed,
                case_only_renames: GitCaseRenamePolicy::IntermediatePath,
                reserved_names: GitReservedNamesPolicy::Native,
                path_length: GitPathLengthPolicy::FilesystemDependent,
                timestamp_precision: GitTimestampPolicy::ContentVerified,
                clone_config: Vec::new(),
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GitExecutableBitsPolicy {
    GitProbed,
    NotRepresentable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GitSymlinkPolicy {
    GitProbed,
    LinkFiles,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GitCaseRenamePolicy {
    Native,
    IntermediatePath,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GitReservedNamesPolicy {
    Native,
    WindowsRestricted,
    WindowsPortable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GitPathLengthPolicy {
    FilesystemDependent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GitTimestampPolicy {
    ContentVerified,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitPlatformPolicy {
    pub profile: GitPlatformProfile,
    pub executable_bits: GitExecutableBitsPolicy,
    pub symlinks: GitSymlinkPolicy,
    pub case_only_renames: GitCaseRenamePolicy,
    pub reserved_names: GitReservedNamesPolicy,
    pub path_length: GitPathLengthPolicy,
    pub timestamp_precision: GitTimestampPolicy,
    #[serde(skip)]
    clone_config: Vec<GitCloneConfig>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GitCloneConfig {
    key: &'static str,
    value: &'static str,
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

impl GitRepository {
    fn command_path(&self) -> &Path {
        self.work_tree.as_deref().unwrap_or(&self.git_dir)
    }

    fn require_work_tree(&self) -> Result<&Path, GitEngineError> {
        self.work_tree
            .as_deref()
            .ok_or_else(|| GitEngineError::UnsupportedRepository {
                detail: "working-tree synchronization cannot operate on a bare repository"
                    .to_string(),
            })
    }

    fn sync_index(&self) -> PathBuf {
        self.git_dir.join("vulcan-sync/index")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct GitOid(String);

impl GitOid {
    pub fn parse(value: impl Into<String>) -> Result<Self, GitEngineError> {
        let value = value.into();
        if matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            Ok(Self(value.to_ascii_lowercase()))
        } else {
            Err(GitEngineError::InvalidObjectId(value))
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for GitOid {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct GitRefName(String);

impl GitRefName {
    pub fn parse(value: impl Into<String>) -> Result<Self, GitEngineError> {
        let value = value.into();
        let invalid_component = value.split('/').any(|component| {
            component.is_empty()
                || component == "."
                || component == ".."
                || component.ends_with('.')
                || component
                    .as_bytes()
                    .get(component.len().saturating_sub(5)..)
                    .is_some_and(|suffix| suffix.eq_ignore_ascii_case(b".lock"))
        });
        let invalid_byte = value.bytes().any(|byte| {
            byte <= b' '
                || byte == 0x7f
                || matches!(byte, b'~' | b'^' | b':' | b'?' | b'*' | b'[' | b'\\')
        });
        if value.starts_with("refs/")
            && !value.ends_with('/')
            && !value.contains("..")
            && !value.contains("@{")
            && !invalid_component
            && !invalid_byte
        {
            Ok(Self(value))
        } else {
            Err(GitEngineError::InvalidRefName(value))
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for GitRefName {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct GitRemote(String);

impl GitRemote {
    pub fn parse(value: impl Into<String>) -> Result<Self, GitEngineError> {
        let value = value.into();
        if !value.is_empty()
            && !value.starts_with('-')
            && !value
                .bytes()
                .any(|byte| byte.is_ascii_control() || byte == b' ')
        {
            Ok(Self(value))
        } else {
            Err(GitEngineError::InvalidRemote(value))
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for GitRemote {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitCaptureRequest {
    pub base: Option<GitOid>,
    pub target_ref: GitRefName,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitCapture {
    pub commit: GitOid,
    pub tree: GitOid,
    pub created: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitPathObject {
    pub oid: GitOid,
    pub mode: String,
    pub kind: String,
    #[serde(skip)]
    pub data: Option<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitMerge {
    pub clean: bool,
    pub tree: Option<GitOid>,
    pub base: Option<GitOid>,
    pub conflict_paths: Vec<String>,
    pub diagnostics: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GitConflictSide {
    Base,
    Local,
    Remote,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitMergeResolutionRequest {
    pub base: GitOid,
    pub accepted_remote: GitOid,
    pub local_candidate: GitOid,
    pub paths: Vec<String>,
    pub side: GitConflictSide,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GitPushResult {
    Updated,
    Rejected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GitRefCreateResult {
    Created,
    Exists,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GitRefUpdateResult {
    Updated,
    Stale,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitSafetyState {
    pub staged_changes: bool,
    pub operation: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitFilterRequirement {
    pub name: String,
    pub path_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitRepositoryRequirements {
    pub tracked_paths: usize,
    pub ignored_internal_paths: Vec<String>,
    pub required_filters: Vec<GitFilterRequirement>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_lfs_available: Option<bool>,
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
    InvalidObjectId(String),
    InvalidRefName(String),
    InvalidRemote(String),
    UnsupportedRepository {
        detail: String,
    },
    WorktreeChanged,
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
            Self::InvalidObjectId(value) => write!(formatter, "invalid Git object ID: `{value}`"),
            Self::InvalidRefName(value) => write!(formatter, "invalid Git ref name: `{value}`"),
            Self::InvalidRemote(value) => write!(formatter, "invalid Git remote: `{value}`"),
            Self::UnsupportedRepository { detail } => formatter.write_str(detail),
            Self::WorktreeChanged => formatter.write_str(
                "the working tree changed repeatedly while it was being captured; retry the sync",
            ),
            Self::Io(error) => Display::fmt(error, formatter),
        }
    }
}

impl Error for GitEngineError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::ExecutableUnavailable { source, .. } | Self::Io(source) => Some(source),
            Self::CommandFailed { .. }
            | Self::InvalidOutput { .. }
            | Self::InvalidObjectId(_)
            | Self::InvalidRefName(_)
            | Self::InvalidRemote(_)
            | Self::UnsupportedRepository { .. }
            | Self::WorktreeChanged => None,
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
        let output = self.execute(command)?;
        ensure_success(operation, output)
    }

    fn execute(&self, mut command: Command) -> Result<Output, GitEngineError> {
        command.output().map_err(|source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                GitEngineError::ExecutableUnavailable {
                    executable: self.executable.clone(),
                    source,
                }
            } else {
                GitEngineError::Io(source)
            }
        })
    }

    fn repository_command(&self, repository: &GitRepository) -> Command {
        let mut command = self.command();
        command.arg("-C").arg(repository.command_path());
        command
    }

    fn repository_output(
        &self,
        repository: &GitRepository,
        operation: &'static str,
        arguments: impl IntoIterator<Item = impl AsRef<OsStr>>,
    ) -> Result<Output, GitEngineError> {
        let mut command = self.repository_command(repository);
        command.args(arguments);
        let output = self.execute(command)?;
        ensure_success(operation, output)
    }

    fn repository_capture(
        &self,
        repository: &GitRepository,
        operation: &'static str,
        arguments: impl IntoIterator<Item = impl AsRef<OsStr>>,
    ) -> Result<String, GitEngineError> {
        let output = self.repository_output(repository, operation, arguments)?;
        decode_stdout(operation, output.stdout)
    }

    fn index_command(
        &self,
        repository: &GitRepository,
        index_path: &Path,
    ) -> Result<Command, GitEngineError> {
        repository.require_work_tree()?;
        let mut command = self.repository_command(repository);
        command.env("GIT_INDEX_FILE", index_path);
        Ok(command)
    }

    fn index_output(
        &self,
        repository: &GitRepository,
        index_path: &Path,
        operation: &'static str,
        arguments: impl IntoIterator<Item = impl AsRef<OsStr>>,
    ) -> Result<Output, GitEngineError> {
        let mut command = self.index_command(repository, index_path)?;
        command.args(arguments);
        let output = self.execute(command)?;
        ensure_success(operation, output)
    }

    fn index_capture(
        &self,
        repository: &GitRepository,
        index_path: &Path,
        operation: &'static str,
        arguments: impl IntoIterator<Item = impl AsRef<OsStr>>,
    ) -> Result<String, GitEngineError> {
        let output = self.index_output(repository, index_path, operation, arguments)?;
        decode_stdout(operation, output.stdout)
    }

    fn commit_tree(
        &self,
        repository: &GitRepository,
        tree: &GitOid,
        parents: &[GitOid],
        message: &str,
    ) -> Result<GitOid, GitEngineError> {
        let mut command = self.repository_command(repository);
        command.arg("commit-tree").arg(tree.as_str());
        for parent in parents {
            command.arg("-p").arg(parent.as_str());
        }
        command
            .env("GIT_AUTHOR_NAME", "Vulcan Sync")
            .env("GIT_AUTHOR_EMAIL", "sync@vulcan.invalid")
            .env("GIT_COMMITTER_NAME", "Vulcan Sync")
            .env("GIT_COMMITTER_EMAIL", "sync@vulcan.invalid")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().map_err(|source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                GitEngineError::ExecutableUnavailable {
                    executable: self.executable.clone(),
                    source,
                }
            } else {
                GitEngineError::Io(source)
            }
        })?;
        child
            .stdin
            .take()
            .ok_or_else(|| GitEngineError::InvalidOutput {
                operation: "create a commit",
                detail: "Git stdin was unavailable".to_string(),
            })?
            .write_all(message.as_bytes())?;
        let output = child.wait_with_output()?;
        let output = ensure_success("create a commit", output)?;
        GitOid::parse(decode_stdout("create a commit", output.stdout)?.trim())
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

    fn clone_repository(&self, request: &GitCloneRequest) -> Result<GitRepository, GitEngineError> {
        request.validate()?;
        let mut command = self.command();
        command.arg("clone");
        let policy = request.platform.policy();
        for setting in &policy.clone_config {
            command
                .arg("--config")
                .arg(format!("{}={}", setting.key, setting.value));
        }
        if let Some(git_dir) = &request.git_dir {
            command.arg("--separate-git-dir").arg(git_dir);
        }
        command
            .arg("--")
            .arg(&request.source)
            .arg(&request.work_tree);
        let output = self.execute(command)?;
        if !output.status.success() {
            return Err(redact_clone_source(
                command_failed("clone Git repository", &output),
                &request.source,
            ));
        }
        self.discover_repository(&request.work_tree)
    }

    fn read_ref(
        &self,
        repository: &GitRepository,
        reference: &GitRefName,
    ) -> Result<Option<GitOid>, GitEngineError> {
        let mut command = self.repository_command(repository);
        command
            .args(["rev-parse", "--verify", "--quiet"])
            .arg(format!("{}^{{commit}}", reference.as_str()));
        let output = self.execute(command)?;
        if output.status.success() {
            return GitOid::parse(decode_stdout("read a Git ref", output.stdout)?.trim()).map(Some);
        }
        if output.status.code() == Some(1) && output.stdout.is_empty() && output.stderr.is_empty() {
            return Ok(None);
        }
        Err(command_failed("read a Git ref", &output))
    }

    fn head_commit(&self, repository: &GitRepository) -> Result<Option<GitOid>, GitEngineError> {
        let mut command = self.repository_command(repository);
        command.args(["rev-parse", "--verify", "--quiet", "HEAD^{commit}"]);
        let output = self.execute(command)?;
        if output.status.success() {
            return GitOid::parse(decode_stdout("read HEAD", output.stdout)?.trim()).map(Some);
        }
        if output.status.code() == Some(1) && output.stdout.is_empty() && output.stderr.is_empty() {
            return Ok(None);
        }
        Err(command_failed("read HEAD", &output))
    }

    fn resolve_revision(
        &self,
        repository: &GitRepository,
        revision: &str,
    ) -> Result<GitOid, GitEngineError> {
        validate_revision(revision)?;
        let mut command = self.repository_command(repository);
        command.args(["rev-parse", "--verify", "--end-of-options"]);
        command.arg(format!("{revision}^{{commit}}"));
        let output = self.execute(command)?;
        let output = ensure_success("resolve a Git revision", output)?;
        GitOid::parse(decode_stdout("resolve a Git revision", output.stdout)?.trim())
    }

    fn update_ref(
        &self,
        repository: &GitRepository,
        reference: &GitRefName,
        target: &GitOid,
    ) -> Result<(), GitEngineError> {
        self.repository_output(
            repository,
            "update a local Git ref",
            ["update-ref", reference.as_str(), target.as_str()],
        )?;
        Ok(())
    }

    fn create_ref(
        &self,
        repository: &GitRepository,
        reference: &GitRefName,
        target: &GitOid,
    ) -> Result<GitRefCreateResult, GitEngineError> {
        let mut command = self.repository_command(repository);
        command.args(["update-ref", reference.as_str(), target.as_str(), ""]);
        let output = self.execute(command)?;
        if output.status.success() {
            return Ok(GitRefCreateResult::Created);
        }
        let detail = format!(
            "{}\n{}",
            bounded_lossy(&output.stdout),
            bounded_lossy(&output.stderr)
        );
        if detail.contains("cannot lock ref") && detail.contains("reference already exists") {
            Ok(GitRefCreateResult::Exists)
        } else {
            Err(command_failed("create a local Git ref", &output))
        }
    }

    fn compare_and_swap_ref(
        &self,
        repository: &GitRepository,
        reference: &GitRefName,
        target: &GitOid,
        expected: Option<&GitOid>,
    ) -> Result<GitRefUpdateResult, GitEngineError> {
        let mut command = self.repository_command(repository);
        command
            .args(["update-ref", reference.as_str(), target.as_str()])
            .arg(expected.map_or("", GitOid::as_str));
        let output = self.execute(command)?;
        if output.status.success() {
            return Ok(GitRefUpdateResult::Updated);
        }
        let detail = format!(
            "{}\n{}",
            bounded_lossy(&output.stdout),
            bounded_lossy(&output.stderr)
        );
        if detail.contains("cannot lock ref")
            && (detail.contains("is at")
                || detail.contains("reference already exists")
                || detail.contains("reference is missing"))
        {
            Ok(GitRefUpdateResult::Stale)
        } else {
            Err(command_failed("compare and swap a local Git ref", &output))
        }
    }

    fn tree_oid(
        &self,
        repository: &GitRepository,
        commit: &GitOid,
    ) -> Result<GitOid, GitEngineError> {
        let expression = format!("{}^{{tree}}", commit.as_str());
        let stdout = self.repository_capture(
            repository,
            "resolve a commit tree",
            ["rev-parse", "--verify", expression.as_str()],
        )?;
        GitOid::parse(stdout.trim())
    }

    fn path_object(
        &self,
        repository: &GitRepository,
        commit: &GitOid,
        path: &str,
    ) -> Result<Option<GitPathObject>, GitEngineError> {
        validate_repository_path(path)?;
        let mut command = self.repository_command(repository);
        command
            .args(["ls-tree", "-z"])
            .arg(commit.as_str())
            .args(["--", path]);
        let output = self.execute(command)?;
        let output = ensure_success("inspect a conflicted path object", output)?;
        if output.stdout.is_empty() {
            return Ok(None);
        }
        if output.stdout.last() != Some(&0) {
            return Err(GitEngineError::InvalidOutput {
                operation: "inspect a conflicted path object",
                detail: "expected a NUL-terminated ls-tree record".to_string(),
            });
        }
        let record =
            std::str::from_utf8(&output.stdout[..output.stdout.len() - 1]).map_err(|error| {
                GitEngineError::InvalidOutput {
                    operation: "inspect a conflicted path object",
                    detail: error.to_string(),
                }
            })?;
        if record.contains('\0') {
            return Err(GitEngineError::InvalidOutput {
                operation: "inspect a conflicted path object",
                detail: "the exact path query returned multiple objects".to_string(),
            });
        }
        let (metadata, found_path) =
            record
                .split_once('\t')
                .ok_or_else(|| GitEngineError::InvalidOutput {
                    operation: "inspect a conflicted path object",
                    detail: "expected `<mode> <type> <oid>\\t<path>`".to_string(),
                })?;
        if found_path != path {
            return Err(GitEngineError::InvalidOutput {
                operation: "inspect a conflicted path object",
                detail: format!("expected path `{path}`, received `{found_path}`"),
            });
        }
        let mut metadata = metadata.split_whitespace();
        let mode = metadata.next().unwrap_or_default();
        let kind = metadata.next().unwrap_or_default();
        let oid = metadata.next().unwrap_or_default();
        if mode.is_empty() || kind.is_empty() || oid.is_empty() || metadata.next().is_some() {
            return Err(GitEngineError::InvalidOutput {
                operation: "inspect a conflicted path object",
                detail: "invalid ls-tree object metadata".to_string(),
            });
        }
        let oid = GitOid::parse(oid)?;
        let data = if kind == "blob" {
            let output = self.repository_output(
                repository,
                "read a conflicted blob",
                ["cat-file", "blob", oid.as_str()],
            )?;
            if output.stdout.len() > MAX_CONFLICT_BLOB_BYTES {
                return Err(GitEngineError::InvalidOutput {
                    operation: "read a conflicted blob",
                    detail: format!(
                        "blob `{oid}` exceeds the {MAX_CONFLICT_BLOB_BYTES} byte preservation limit"
                    ),
                });
            }
            Some(output.stdout)
        } else {
            None
        };
        Ok(Some(GitPathObject {
            oid,
            mode: mode.to_string(),
            kind: kind.to_string(),
            data,
        }))
    }

    fn changed_paths(
        &self,
        repository: &GitRepository,
        from: &GitOid,
        to: &GitOid,
    ) -> Result<Vec<String>, GitEngineError> {
        let mut command = self.repository_command(repository);
        command.args([
            "diff-tree",
            "--no-commit-id",
            "--name-only",
            "--no-renames",
            "-r",
            "-z",
            from.as_str(),
            to.as_str(),
            "--",
        ]);
        let output = self.execute(command)?;
        let output = ensure_success("list changed Git paths", output)?;
        parse_nul_paths("list changed Git paths", &output.stdout)
    }

    fn tree_with_paths(
        &self,
        repository: &GitRepository,
        base: &GitOid,
        target: &GitOid,
        paths: &[String],
    ) -> Result<GitOid, GitEngineError> {
        repository.require_work_tree()?;
        let index_path = repository.sync_index();
        let index_parent = index_path
            .parent()
            .expect("the sync index path always has a parent");
        std::fs::create_dir_all(index_parent)?;
        remove_file_if_present(&index_path)?;
        self.index_output(
            repository,
            &index_path,
            "seed a proposed tree",
            ["read-tree", base.as_str()],
        )?;
        for path in paths {
            validate_repository_path(path)?;
            if let Some(object) = self.path_object(repository, target, path)? {
                let mut command = self.index_command(repository, &index_path)?;
                command
                    .args(["update-index", "--add", "--cacheinfo"])
                    .arg(&object.mode)
                    .arg(object.oid.as_str())
                    .arg(path);
                ensure_success("add a path to a proposed tree", self.execute(command)?)?;
            } else {
                let mut command = self.index_command(repository, &index_path)?;
                command
                    .args(["update-index", "--force-remove", "--"])
                    .arg(path);
                ensure_success("remove a path from a proposed tree", self.execute(command)?)?;
            }
        }
        GitOid::parse(
            self.index_capture(
                repository,
                &index_path,
                "write a proposed tree",
                ["write-tree"],
            )?
            .trim(),
        )
    }

    fn diff_patch(
        &self,
        repository: &GitRepository,
        from: &GitOid,
        to: &GitOid,
        paths: &[String],
    ) -> Result<String, GitEngineError> {
        for path in paths {
            validate_repository_path(path)?;
        }
        let mut command = self.repository_command(repository);
        command.args([
            "diff",
            "--binary",
            "--no-ext-diff",
            "--no-textconv",
            from.as_str(),
            to.as_str(),
            "--",
        ]);
        command.args(paths);
        let output = self.execute(command)?;
        let output = ensure_success("render a proposed Git patch", output)?;
        if output.stdout.len() > MAX_DIAGNOSTIC_PATH_BYTES {
            return Err(GitEngineError::InvalidOutput {
                operation: "render a proposed Git patch",
                detail: format!("patch exceeds the {MAX_DIAGNOSTIC_PATH_BYTES} byte report limit"),
            });
        }
        String::from_utf8(output.stdout).map_err(|error| GitEngineError::InvalidOutput {
            operation: "render a proposed Git patch",
            detail: error.to_string(),
        })
    }

    fn capture_worktree(
        &self,
        repository: &GitRepository,
        request: &GitCaptureRequest,
    ) -> Result<GitCapture, GitEngineError> {
        repository.require_work_tree()?;
        let index_path = repository.sync_index();
        let index_parent = index_path
            .parent()
            .expect("the sync index path always has a parent");
        std::fs::create_dir_all(index_parent)?;
        remove_file_if_present(&index_path)?;

        match &request.base {
            Some(base) => {
                self.index_output(
                    repository,
                    &index_path,
                    "seed the sync index",
                    ["read-tree", base.as_str()],
                )?;
            }
            None => {
                self.index_output(
                    repository,
                    &index_path,
                    "initialize the sync index",
                    ["read-tree", "--empty"],
                )?;
            }
        }

        let mut stable_tree = None;
        for _ in 0..3 {
            self.index_output(
                repository,
                &index_path,
                "capture the working tree",
                ["add", "-A", "--", "."],
            )?;
            let first = GitOid::parse(
                self.index_capture(
                    repository,
                    &index_path,
                    "write the captured tree",
                    ["write-tree"],
                )?
                .trim(),
            )?;
            self.index_output(
                repository,
                &index_path,
                "verify the working-tree capture",
                ["add", "-A", "--", "."],
            )?;
            let second = GitOid::parse(
                self.index_capture(
                    repository,
                    &index_path,
                    "verify the captured tree",
                    ["write-tree"],
                )?
                .trim(),
            )?;
            if first == second {
                stable_tree = Some(second);
                break;
            }
        }
        let tree = stable_tree.ok_or(GitEngineError::WorktreeChanged)?;

        if let Some(base) = &request.base {
            if self.tree_oid(repository, base)? == tree {
                self.update_ref(repository, &request.target_ref, base)?;
                return Ok(GitCapture {
                    commit: base.clone(),
                    tree,
                    created: false,
                });
            }
        }

        let parents = request.base.iter().cloned().collect::<Vec<_>>();
        let commit = self.commit_tree(repository, &tree, &parents, &request.message)?;
        self.update_ref(repository, &request.target_ref, &commit)?;
        Ok(GitCapture {
            commit,
            tree,
            created: true,
        })
    }

    fn remote_ref(
        &self,
        repository: &GitRepository,
        remote: &GitRemote,
        reference: &GitRefName,
    ) -> Result<Option<GitOid>, GitEngineError> {
        let mut command = self.repository_command(repository);
        command
            .args(["ls-remote", "--exit-code", "--refs", "--"])
            .arg(remote.as_str())
            .arg(reference.as_str());
        let output = self.execute(command)?;
        if output.status.code() == Some(2) && output.stdout.is_empty() {
            return Ok(None);
        }
        let output = ensure_success("query a remote Git ref", output)?;
        let stdout = decode_stdout("query a remote Git ref", output.stdout)?;
        let mut lines = stdout.lines().filter(|line| !line.trim().is_empty());
        let Some(line) = lines.next() else {
            return Ok(None);
        };
        if lines.next().is_some() {
            return Err(GitEngineError::InvalidOutput {
                operation: "query a remote Git ref",
                detail: "the exact ref query returned more than one result".to_string(),
            });
        }
        let (oid, found_ref) =
            line.split_once('\t')
                .ok_or_else(|| GitEngineError::InvalidOutput {
                    operation: "query a remote Git ref",
                    detail: format!("expected `<oid>\\t<ref>`, received `{line}`"),
                })?;
        if found_ref != reference.as_str() {
            return Err(GitEngineError::InvalidOutput {
                operation: "query a remote Git ref",
                detail: format!("expected `{reference}`, received `{found_ref}`"),
            });
        }
        GitOid::parse(oid).map(Some)
    }

    fn fetch_ref(
        &self,
        repository: &GitRepository,
        remote: &GitRemote,
        source: &GitRefName,
        destination: &GitRefName,
    ) -> Result<GitOid, GitEngineError> {
        let refspec = format!("+{}:{}", source.as_str(), destination.as_str());
        let arguments = vec![
            OsString::from("fetch"),
            OsString::from("--no-tags"),
            OsString::from("--force"),
            OsString::from("--"),
            OsString::from(remote.as_str()),
            OsString::from(refspec),
        ];
        self.repository_output(repository, "fetch the live sync ref", arguments)?;
        self.read_ref(repository, destination)?
            .ok_or_else(|| GitEngineError::InvalidOutput {
                operation: "fetch the live sync ref",
                detail: format!("Git did not update `{destination}`"),
            })
    }

    fn is_ancestor(
        &self,
        repository: &GitRepository,
        ancestor: &GitOid,
        descendant: &GitOid,
    ) -> Result<bool, GitEngineError> {
        let mut command = self.repository_command(repository);
        command
            .args(["merge-base", "--is-ancestor"])
            .arg(ancestor.as_str())
            .arg(descendant.as_str());
        let output = self.execute(command)?;
        match output.status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(command_failed("compare commit ancestry", &output)),
        }
    }

    fn merge_commits(
        &self,
        repository: &GitRepository,
        accepted_remote: &GitOid,
        local_candidate: &GitOid,
    ) -> Result<GitMerge, GitEngineError> {
        let mut command = self.repository_command(repository);
        command
            .args(["merge-tree", "--write-tree", "--name-only", "-z"])
            .arg(accepted_remote.as_str())
            .arg(local_candidate.as_str());
        let output = self.execute(command)?;
        let clean = output.status.success();
        if !clean && output.status.code() != Some(1) {
            return Err(command_failed("merge live sync commits", &output));
        }
        let (tree, conflict_paths, mut diagnostics) =
            parse_merge_tree_output(&output.stdout, clean)?;
        let stderr = bounded_lossy(&output.stderr);
        if !stderr.is_empty() {
            if !diagnostics.is_empty() {
                diagnostics.push('\n');
            }
            diagnostics.push_str(&stderr);
        }
        let base = self.merge_base(repository, accepted_remote, local_candidate)?;
        Ok(GitMerge {
            clean,
            tree,
            base,
            conflict_paths,
            diagnostics,
        })
    }

    fn resolve_merge_tree(
        &self,
        repository: &GitRepository,
        request: &GitMergeResolutionRequest,
    ) -> Result<GitOid, GitEngineError> {
        repository.require_work_tree()?;
        if request.paths.is_empty() {
            return Err(GitEngineError::UnsupportedRepository {
                detail: "a merge resolution must name at least one conflicted path".to_string(),
            });
        }
        let index_path = repository.sync_index();
        std::fs::create_dir_all(
            index_path
                .parent()
                .expect("the sync index path always has a parent"),
        )?;
        remove_file_if_present(&index_path)?;
        self.index_output(
            repository,
            &index_path,
            "prepare a conflicted merge resolution",
            [
                "read-tree",
                "-m",
                request.base.as_str(),
                request.accepted_remote.as_str(),
                request.local_candidate.as_str(),
            ],
        )?;
        let selected = match request.side {
            GitConflictSide::Base => &request.base,
            GitConflictSide::Local => &request.local_candidate,
            GitConflictSide::Remote => &request.accepted_remote,
        };
        for path in &request.paths {
            validate_repository_path(path)?;
            if let Some(object) = self.path_object(repository, selected, path)? {
                let mut command = self.index_command(repository, &index_path)?;
                command
                    .args(["update-index", "--add", "--cacheinfo"])
                    .arg(&object.mode)
                    .arg(object.oid.as_str())
                    .arg(path);
                let output = self.execute(command)?;
                ensure_success("select a preserved conflict side", output)?;
            } else {
                let mut command = self.index_command(repository, &index_path)?;
                command
                    .args(["update-index", "--force-remove", "--"])
                    .arg(path);
                let output = self.execute(command)?;
                ensure_success("select a preserved conflict deletion", output)?;
            }
        }
        GitOid::parse(
            self.index_capture(
                repository,
                &index_path,
                "write the resolved merge tree",
                ["write-tree"],
            )?
            .trim(),
        )
    }

    fn create_commit(
        &self,
        repository: &GitRepository,
        tree: &GitOid,
        parents: &[GitOid],
        message: &str,
    ) -> Result<GitOid, GitEngineError> {
        self.commit_tree(repository, tree, parents, message)
    }

    fn push_ref(
        &self,
        repository: &GitRepository,
        remote: &GitRemote,
        source: &GitOid,
        destination: &GitRefName,
        expected: Option<&GitOid>,
    ) -> Result<GitPushResult, GitEngineError> {
        let lease = format!(
            "--force-with-lease={}:{}",
            destination.as_str(),
            expected.map_or("", GitOid::as_str)
        );
        let refspec = format!("{}:{}", source.as_str(), destination.as_str());
        let mut command = self.repository_command(repository);
        command
            .args(["push", "--porcelain"])
            .arg(lease)
            .arg("--")
            .arg(remote.as_str())
            .arg(refspec);
        let output = self.execute(command)?;
        if output.status.success() {
            return Ok(GitPushResult::Updated);
        }
        let combined = format!(
            "{}\n{}",
            bounded_lossy(&output.stdout),
            bounded_lossy(&output.stderr)
        );
        if [
            "stale info",
            "[rejected]",
            "fetch first",
            "non-fast-forward",
        ]
        .iter()
        .any(|needle| combined.contains(needle))
        {
            return Ok(GitPushResult::Rejected);
        }
        Err(command_failed("push the live sync ref", &output))
    }

    fn apply_tree(
        &self,
        repository: &GitRepository,
        expected_worktree: &GitOid,
        target: &GitOid,
    ) -> Result<(), GitEngineError> {
        repository.require_work_tree()?;
        let index_path = repository.sync_index();
        std::fs::create_dir_all(
            index_path
                .parent()
                .expect("the sync index path always has a parent"),
        )?;
        remove_file_if_present(&index_path)?;
        self.index_output(
            repository,
            &index_path,
            "seed the worktree application index",
            ["read-tree", expected_worktree.as_str()],
        )?;
        self.index_output(
            repository,
            &index_path,
            "verify the worktree before applying sync",
            ["add", "-A", "--", "."],
        )?;
        let actual_tree = GitOid::parse(
            self.index_capture(
                repository,
                &index_path,
                "verify the worktree before applying sync",
                ["write-tree"],
            )?
            .trim(),
        )?;
        if self.tree_oid(repository, expected_worktree)? != actual_tree {
            return Err(GitEngineError::WorktreeChanged);
        }

        self.index_output(
            repository,
            &index_path,
            "restore the pre-application index",
            ["read-tree", expected_worktree.as_str()],
        )?;
        self.index_output(
            repository,
            &index_path,
            "apply the accepted sync tree",
            ["read-tree", "--reset", "-u", target.as_str()],
        )?;
        self.index_output(
            repository,
            &index_path,
            "verify the applied sync tree",
            ["add", "-A", "--", "."],
        )?;
        let applied_tree = GitOid::parse(
            self.index_capture(
                repository,
                &index_path,
                "verify the applied sync tree",
                ["write-tree"],
            )?
            .trim(),
        )?;
        if self.tree_oid(repository, target)? != applied_tree {
            return Err(GitEngineError::InvalidOutput {
                operation: "verify the applied sync tree",
                detail: format!(
                    "expected tree {}, materialized tree {}",
                    self.tree_oid(repository, target)?,
                    applied_tree
                ),
            });
        }
        Ok(())
    }

    fn safety_state(&self, repository: &GitRepository) -> Result<GitSafetyState, GitEngineError> {
        repository.require_work_tree()?;
        let mut command = self.repository_command(repository);
        command.args(["diff", "--cached", "--quiet", "--"]);
        let output = self.execute(command)?;
        let staged_changes = match output.status.code() {
            Some(0) => false,
            Some(1) => true,
            _ => return Err(command_failed("inspect staged changes", &output)),
        };
        let operations = [
            ("MERGE_HEAD", "merge"),
            ("CHERRY_PICK_HEAD", "cherry-pick"),
            ("REVERT_HEAD", "revert"),
            ("BISECT_LOG", "bisect"),
            ("rebase-merge", "rebase"),
            ("rebase-apply", "rebase"),
        ];
        let operation = operations
            .iter()
            .find(|(path, _)| {
                repository.git_dir.join(path).exists() || repository.common_dir.join(path).exists()
            })
            .map(|(_, name)| (*name).to_string());
        Ok(GitSafetyState {
            staged_changes,
            operation,
        })
    }

    fn repository_requirements(
        &self,
        repository: &GitRepository,
    ) -> Result<GitRepositoryRequirements, GitEngineError> {
        repository.require_work_tree()?;
        let tracked = self.repository_output(
            repository,
            "list tracked paths for sync diagnostics",
            ["ls-files", "-z"],
        )?;
        if tracked.stdout.len() > MAX_DIAGNOSTIC_PATH_BYTES {
            return Err(GitEngineError::InvalidOutput {
                operation: "list tracked paths for sync diagnostics",
                detail: format!(
                    "tracked path list exceeds the {MAX_DIAGNOSTIC_PATH_BYTES} byte diagnostic limit"
                ),
            });
        }
        let tracked_paths = nul_fields("list tracked paths for sync diagnostics", &tracked.stdout)?;

        let mut command = self.repository_command(repository);
        command
            .args(["check-attr", "-z", "--stdin", "filter"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().map_err(GitEngineError::Io)?;
        child
            .stdin
            .take()
            .ok_or_else(|| GitEngineError::InvalidOutput {
                operation: "inspect configured Git filters",
                detail: "Git diagnostic command did not expose stdin".to_string(),
            })?
            .write_all(&tracked.stdout)?;
        let attributes = child.wait_with_output()?;
        let attributes = ensure_success("inspect configured Git filters", attributes)?;
        if attributes.stdout.len() > MAX_DIAGNOSTIC_PATH_BYTES.saturating_mul(4) {
            return Err(GitEngineError::InvalidOutput {
                operation: "inspect configured Git filters",
                detail: "Git filter diagnostic output exceeds the bounded response limit"
                    .to_string(),
            });
        }
        let fields = nul_fields("inspect configured Git filters", &attributes.stdout)?;
        if fields.len() % 3 != 0 {
            return Err(GitEngineError::InvalidOutput {
                operation: "inspect configured Git filters",
                detail: "expected NUL-delimited path/attribute/value triples".to_string(),
            });
        }
        let mut filters = BTreeMap::<String, usize>::new();
        for triple in fields.chunks_exact(3) {
            let value = triple[2];
            if !matches!(value, "unspecified" | "unset" | "set" | "") {
                *filters.entry(value.to_string()).or_default() += 1;
            }
        }
        let required_filters = filters
            .into_iter()
            .map(|(name, path_count)| GitFilterRequirement { name, path_count })
            .collect::<Vec<_>>();
        let git_lfs_available = required_filters
            .iter()
            .any(|filter| filter.name == "lfs")
            .then(|| {
                let mut command = self.repository_command(repository);
                command.args(["lfs", "version"]);
                self.execute(command)
                    .is_ok_and(|output| output.status.success())
            });

        let mut ignored_internal_paths = Vec::new();
        for path in [
            ".vulcan/cache.db",
            ".vulcan/cache.db-wal",
            ".vulcan/cache.db-shm",
        ] {
            let mut command = self.repository_command(repository);
            command.args(["check-ignore", "--quiet", "--", path]);
            let output = self.execute(command)?;
            match output.status.code() {
                Some(0) => ignored_internal_paths.push(path.to_string()),
                Some(1) => {}
                _ => return Err(command_failed("inspect ignored Vulcan state", &output)),
            }
        }

        Ok(GitRepositoryRequirements {
            tracked_paths: tracked_paths.len(),
            ignored_internal_paths,
            required_filters,
            git_lfs_available,
        })
    }
}

impl GitCliEngine {
    fn merge_base(
        &self,
        repository: &GitRepository,
        left: &GitOid,
        right: &GitOid,
    ) -> Result<Option<GitOid>, GitEngineError> {
        let mut command = self.repository_command(repository);
        command
            .args(["merge-base", "--all"])
            .arg(left.as_str())
            .arg(right.as_str());
        let output = self.execute(command)?;
        match output.status.code() {
            Some(0) => {
                let stdout = decode_stdout("find the merge base", output.stdout)?;
                let mut bases = stdout.lines().filter(|line| !line.trim().is_empty());
                let base = bases.next().map(GitOid::parse).transpose()?;
                if bases.next().is_some() {
                    return Err(GitEngineError::UnsupportedRepository {
                        detail: "sync conflict identity does not yet support multiple merge bases"
                            .to_string(),
                    });
                }
                Ok(base)
            }
            Some(1) => Ok(None),
            _ => Err(command_failed("find the merge base", &output)),
        }
    }
}

fn ensure_success(operation: &'static str, output: Output) -> Result<Output, GitEngineError> {
    if output.status.success() {
        Ok(output)
    } else {
        Err(command_failed(operation, &output))
    }
}

fn command_failed(operation: &'static str, output: &Output) -> GitEngineError {
    let stderr = bounded_lossy(&output.stderr);
    let stdout = bounded_lossy(&output.stdout);
    GitEngineError::CommandFailed {
        operation,
        exit_code: output.status.code(),
        stderr: if stderr.is_empty() { stdout } else { stderr },
    }
}

fn redact_clone_source(mut error: GitEngineError, source: &str) -> GitEngineError {
    if let GitEngineError::CommandFailed { stderr, .. } = &mut error {
        *stderr = stderr.replace(source, "<redacted clone source>");
    }
    error
}

fn decode_stdout(operation: &'static str, stdout: Vec<u8>) -> Result<String, GitEngineError> {
    String::from_utf8(stdout).map_err(|error| GitEngineError::InvalidOutput {
        operation,
        detail: error.to_string(),
    })
}

fn parse_merge_tree_output(
    stdout: &[u8],
    clean: bool,
) -> Result<(Option<GitOid>, Vec<String>, String), GitEngineError> {
    if stdout.last() != Some(&0) {
        return Err(GitEngineError::InvalidOutput {
            operation: "merge live sync commits",
            detail: "expected NUL-delimited merge-tree output".to_string(),
        });
    }
    let fields = stdout
        .split(|byte| *byte == 0)
        .map(|field| {
            std::str::from_utf8(field).map_err(|error| GitEngineError::InvalidOutput {
                operation: "merge live sync commits",
                detail: error.to_string(),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let tree = fields
        .first()
        .filter(|value| !value.is_empty())
        .map(|value| GitOid::parse(*value))
        .transpose()?;
    if tree.is_none() {
        return Err(GitEngineError::InvalidOutput {
            operation: "merge live sync commits",
            detail: "merge-tree did not return a tree object ID".to_string(),
        });
    }
    if clean {
        return Ok((tree, Vec::new(), String::new()));
    }
    let separator = fields
        .iter()
        .enumerate()
        .skip(1)
        .find(|(_, value)| value.is_empty())
        .map(|(index, _)| index)
        .ok_or_else(|| GitEngineError::InvalidOutput {
            operation: "merge live sync commits",
            detail: "conflicted merge-tree output omitted its path separator".to_string(),
        })?;
    let mut conflict_paths = fields[1..separator]
        .iter()
        .map(|path| (*path).to_string())
        .collect::<Vec<_>>();
    conflict_paths.sort();
    conflict_paths.dedup();
    let details = fields[separator + 1..]
        .chunks_exact(4)
        .filter_map(|record| (!record[3].is_empty()).then_some(record[3].trim()))
        .collect::<Vec<_>>();
    let diagnostics = details.join("\n");
    Ok((tree, conflict_paths, diagnostics))
}

fn nul_fields<'a>(
    operation: &'static str,
    bytes: &'a [u8],
) -> Result<Vec<&'a str>, GitEngineError> {
    let mut fields = bytes
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty())
        .map(|field| {
            std::str::from_utf8(field).map_err(|error| GitEngineError::InvalidOutput {
                operation,
                detail: error.to_string(),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    if bytes.last().is_some_and(|byte| *byte != 0) {
        return Err(GitEngineError::InvalidOutput {
            operation,
            detail: "expected a trailing NUL delimiter".to_string(),
        });
    }
    fields.shrink_to_fit();
    Ok(fields)
}

fn remove_file_if_present(path: &Path) -> Result<(), GitEngineError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(GitEngineError::Io(error)),
    }
}

fn validate_revision(revision: &str) -> Result<(), GitEngineError> {
    if revision.is_empty()
        || revision.starts_with('-')
        || revision.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(GitEngineError::UnsupportedRepository {
            detail: "a Git revision must be non-empty, must not start with `-`, and must not contain control characters".to_string(),
        });
    }
    Ok(())
}

fn parse_nul_paths(operation: &'static str, bytes: &[u8]) -> Result<Vec<String>, GitEngineError> {
    if bytes.len() > MAX_DIAGNOSTIC_PATH_BYTES {
        return Err(GitEngineError::InvalidOutput {
            operation,
            detail: format!("path output exceeds the {MAX_DIAGNOSTIC_PATH_BYTES} byte limit"),
        });
    }
    bytes
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| {
            String::from_utf8(path.to_vec()).map_err(|error| GitEngineError::InvalidOutput {
                operation,
                detail: error.to_string(),
            })
        })
        .collect()
}

fn validate_repository_path(path: &str) -> Result<(), GitEngineError> {
    if path.is_empty()
        || path.starts_with('/')
        || path.contains('\0')
        || path.chars().any(char::is_control)
        || path.split('/').any(|part| part.is_empty() || part == "..")
    {
        Err(GitEngineError::UnsupportedRepository {
            detail: format!("unsafe conflict path `{path}` returned by Git"),
        })
    } else {
        Ok(())
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

    fn run_git_capture(current_dir: &Path, arguments: &[&str]) -> String {
        let output = Command::new("git")
            .current_dir(current_dir)
            .args(arguments)
            .output()
            .expect("Git should launch in tests");
        assert!(
            output.status.success(),
            "Git failed with arguments {arguments:?}"
        );
        String::from_utf8(output.stdout)
            .expect("Git test output should be UTF-8")
            .trim()
            .to_string()
    }

    fn init_repo(path: &Path) {
        run_git(path, &["-c", "init.defaultBranch=main", "init", "--quiet"]);
        run_git(path, &["config", "user.name", "Vulcan Test"]);
        run_git(path, &["config", "user.email", "vulcan@example.invalid"]);
    }

    fn commit_all(path: &Path, message: &str) -> GitOid {
        run_git(path, &["add", "--all", "--", "."]);
        run_git(path, &["commit", "--quiet", "-m", message]);
        GitOid::parse(run_git_capture(path, &["rev-parse", "HEAD"]))
            .expect("HEAD should be an object ID")
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
    fn clones_colocated_and_detached_worktrees() {
        let temporary = TempDir::new().expect("temporary directory");
        let source = temporary.path().join("source");
        fs::create_dir(&source).expect("source directory");
        init_repo(&source);
        fs::write(source.join("Home.md"), "# Home\n").expect("source note");
        #[cfg(unix)]
        std::os::unix::fs::symlink("Home.md", source.join("Shortcut.md")).expect("source symlink");
        commit_all(&source, "initial");
        let engine = GitCliEngine::default();

        let colocated = temporary.path().join("colocated");
        let repository = engine
            .clone_repository(&GitCloneRequest {
                source: source.display().to_string(),
                work_tree: colocated.clone(),
                git_dir: None,
                platform: GitPlatformProfile::LinuxNative,
            })
            .expect("colocated clone");
        assert_eq!(repository.layout, GitRepositoryLayout::Colocated);
        assert!(colocated.join("Home.md").is_file());
        #[cfg(unix)]
        assert!(colocated
            .join("Shortcut.md")
            .symlink_metadata()
            .expect("native symlink metadata")
            .file_type()
            .is_symlink());

        let detached = temporary.path().join("detached");
        let detached_git = temporary.path().join("detached.git");
        let repository = engine
            .clone_repository(&GitCloneRequest {
                source: source.display().to_string(),
                work_tree: detached.clone(),
                git_dir: Some(detached_git.clone()),
                platform: GitPlatformProfile::AndroidShared,
            })
            .expect("detached clone");
        assert_eq!(repository.layout, GitRepositoryLayout::Detached);
        assert!(detached.join(".git").is_file());
        assert!(detached_git.join("HEAD").is_file());
        assert!(detached.join("Home.md").is_file());
        #[cfg(unix)]
        assert_eq!(
            fs::read_to_string(detached.join("Shortcut.md")).expect("link file contents"),
            "Home.md"
        );
        assert_eq!(
            run_git_capture(&detached, &["config", "--bool", "core.fileMode"]),
            "false"
        );
        assert_eq!(
            run_git_capture(&detached, &["config", "--bool", "core.symlinks"]),
            "false"
        );
    }

    #[test]
    fn platform_policies_are_explicit_and_serializable() {
        let native = GitPlatformProfile::LinuxNative.policy();
        assert_eq!(native.executable_bits, GitExecutableBitsPolicy::GitProbed);
        assert!(native.clone_config.is_empty());

        let android = GitPlatformProfile::AndroidShared.policy();
        assert_eq!(
            android.executable_bits,
            GitExecutableBitsPolicy::NotRepresentable
        );
        assert_eq!(android.symlinks, GitSymlinkPolicy::LinkFiles);
        assert_eq!(
            android.case_only_renames,
            GitCaseRenamePolicy::IntermediatePath
        );
        assert_eq!(android.clone_config.len(), 2);
        assert_eq!(
            serde_json::to_value(android).expect("policy should serialize"),
            serde_json::json!({
                "profile": "android_shared",
                "executable_bits": "not_representable",
                "symlinks": "link_files",
                "case_only_renames": "intermediate_path",
                "reserved_names": "windows_portable",
                "path_length": "filesystem_dependent",
                "timestamp_precision": "content_verified"
            })
        );
    }

    #[test]
    fn clone_request_rejects_ambiguous_or_existing_destinations() {
        let temporary = TempDir::new().expect("temporary directory");
        assert!(GitCloneRequest {
            source: "--upload-pack=malicious".to_string(),
            work_tree: temporary.path().join("new"),
            git_dir: None,
            platform: GitPlatformProfile::native(),
        }
        .validate()
        .is_err());
        assert!(GitCloneRequest {
            source: "source".to_string(),
            work_tree: temporary.path().to_path_buf(),
            git_dir: None,
            platform: GitPlatformProfile::native(),
        }
        .validate()
        .is_err());
    }

    #[test]
    fn clone_errors_do_not_expose_the_supplied_source() {
        let source = "https://secret@example.invalid/missing.git?token=secret";
        let error = redact_clone_source(
            GitEngineError::CommandFailed {
                operation: "clone Git repository",
                exit_code: Some(128),
                stderr: format!("fatal: repository '{source}' not found"),
            },
            source,
        );
        let rendered = error.to_string();
        assert!(!rendered.contains("secret@example"));
        assert!(!rendered.contains("token=secret"));
        assert!(rendered.contains("<redacted clone source>"));
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

    #[test]
    fn validates_object_ids_refs_and_remotes() {
        assert!(GitOid::parse("a".repeat(40)).is_ok());
        assert!(GitOid::parse("not-an-object").is_err());
        assert!(GitRefName::parse("refs/vulcan/sync/local/live").is_ok());
        assert!(GitRefName::parse("refs/heads/../../main").is_err());
        assert!(GitRefName::parse("-c core.fsmonitor=true").is_err());
        assert!(GitRemote::parse("origin").is_ok());
        assert!(GitRemote::parse("--upload-pack=evil").is_err());
    }

    #[test]
    fn alternate_index_capture_preserves_the_normal_index() {
        let temporary = TempDir::new().expect("temporary directory");
        init_repo(temporary.path());
        fs::write(temporary.path().join("Home.md"), "initial\n").expect("initial note");
        fs::write(temporary.path().join("Staged.md"), "initial\n").expect("staged note");
        let head = commit_all(temporary.path(), "initial");
        fs::write(temporary.path().join("Home.md"), "updated\n").expect("updated note");
        fs::write(temporary.path().join("Staged.md"), "staged\n").expect("staged update");
        run_git(temporary.path(), &["add", "Staged.md"]);
        let normal_index_before = run_git_capture(temporary.path(), &["write-tree"]);
        let engine = GitCliEngine::default();
        let repository = engine
            .discover_repository(temporary.path())
            .expect("repository");
        let local_ref = GitRefName::parse("refs/vulcan/sync/local/live").expect("ref");

        let capture = engine
            .capture_worktree(
                &repository,
                &GitCaptureRequest {
                    base: Some(head),
                    target_ref: local_ref.clone(),
                    message: "vulcan sync snapshot\n".to_string(),
                },
            )
            .expect("capture should succeed");

        assert!(capture.created);
        assert_eq!(
            engine.read_ref(&repository, &local_ref).expect("read ref"),
            Some(capture.commit.clone())
        );
        assert_eq!(
            run_git_capture(temporary.path(), &["write-tree"]),
            normal_index_before
        );
        let second = engine
            .capture_worktree(
                &repository,
                &GitCaptureRequest {
                    base: Some(capture.commit.clone()),
                    target_ref: local_ref,
                    message: "vulcan sync snapshot\n".to_string(),
                },
            )
            .expect("unchanged capture should succeed");
        assert!(!second.created);
        assert_eq!(second.commit, capture.commit);
    }

    #[test]
    fn push_and_fetch_hidden_ref_use_compare_and_swap() {
        let temporary = TempDir::new().expect("temporary directory");
        let remote_path = temporary.path().join("remote.git");
        run_git(
            temporary.path(),
            &[
                "init",
                "--quiet",
                "--bare",
                remote_path.to_str().expect("remote path"),
            ],
        );
        let worktree = temporary.path().join("wiki");
        fs::create_dir(&worktree).expect("worktree");
        init_repo(&worktree);
        run_git(
            &worktree,
            &[
                "remote",
                "add",
                "origin",
                remote_path.to_str().expect("remote path"),
            ],
        );
        fs::write(worktree.join("Home.md"), "home\n").expect("note");
        let head = commit_all(&worktree, "initial");
        let engine = GitCliEngine::default();
        let repository = engine.discover_repository(&worktree).expect("repository");
        let remote = GitRemote::parse("origin").expect("remote");
        let live = GitRefName::parse("refs/heads/__vulcan-sync/live").expect("live ref");
        let fetched = GitRefName::parse("refs/vulcan/sync/remotes/origin/live").expect("ref");

        assert_eq!(
            engine
                .remote_ref(&repository, &remote, &live)
                .expect("query"),
            None
        );
        assert_eq!(
            engine
                .push_ref(&repository, &remote, &head, &live, None)
                .expect("initial push"),
            GitPushResult::Updated
        );
        assert_eq!(
            engine
                .remote_ref(&repository, &remote, &live)
                .expect("query"),
            Some(head.clone())
        );
        assert_eq!(
            engine
                .fetch_ref(&repository, &remote, &live, &fetched)
                .expect("fetch"),
            head
        );
    }

    #[test]
    fn create_ref_never_replaces_an_existing_checkpoint() {
        let temporary = TempDir::new().expect("temporary directory");
        init_repo(temporary.path());
        fs::write(temporary.path().join("Home.md"), "initial\n").expect("initial note");
        let initial = commit_all(temporary.path(), "initial");
        fs::write(temporary.path().join("Home.md"), "changed\n").expect("changed note");
        let changed = commit_all(temporary.path(), "changed");
        let engine = GitCliEngine::default();
        let repository = engine
            .discover_repository(temporary.path())
            .expect("repository");
        let checkpoint =
            GitRefName::parse("refs/vulcan/checkpoints/recovery/test").expect("checkpoint ref");

        assert_eq!(
            engine
                .create_ref(&repository, &checkpoint, &initial)
                .expect("checkpoint creation"),
            GitRefCreateResult::Created
        );
        assert_eq!(
            engine
                .create_ref(&repository, &checkpoint, &changed)
                .expect("checkpoint collision"),
            GitRefCreateResult::Exists
        );
        assert_eq!(
            engine
                .read_ref(&repository, &checkpoint)
                .expect("checkpoint read"),
            Some(initial)
        );
    }

    #[test]
    fn semantic_plumbing_builds_exact_grouped_trees_and_updates_refs_with_cas() {
        let temporary = TempDir::new().expect("temporary directory");
        init_repo(temporary.path());
        fs::create_dir(temporary.path().join("Area")).expect("area directory");
        fs::write(temporary.path().join("Area/One.md"), "one\n").expect("area note");
        fs::write(temporary.path().join("Root.md"), "root\n").expect("root note");
        fs::write(temporary.path().join("Old.md"), "old\n").expect("old note");
        let from = commit_all(temporary.path(), "initial");

        fs::write(temporary.path().join("Area/One.md"), "one changed\n").expect("area change");
        fs::write(temporary.path().join("Area/Two.md"), "two\n").expect("new area note");
        fs::write(temporary.path().join("Root.md"), "root changed\n").expect("root change");
        fs::remove_file(temporary.path().join("Old.md")).expect("old note removal");
        let to = commit_all(temporary.path(), "target");

        let engine = GitCliEngine::default();
        let repository = engine
            .discover_repository(temporary.path())
            .expect("repository");
        assert_eq!(
            engine
                .resolve_revision(&repository, "HEAD~1")
                .expect("resolved revision"),
            from
        );
        let changed = engine
            .changed_paths(&repository, &from, &to)
            .expect("changed paths");
        assert_eq!(
            changed,
            vec!["Area/One.md", "Area/Two.md", "Old.md", "Root.md"]
        );

        let area_paths = changed[..2].to_vec();
        let area_tree = engine
            .tree_with_paths(&repository, &from, &to, &area_paths)
            .expect("area tree");
        let area_commit = engine
            .create_commit(
                &repository,
                &area_tree,
                std::slice::from_ref(&from),
                "Update Area\n",
            )
            .expect("area commit");
        let root_tree = engine
            .tree_with_paths(&repository, &area_commit, &to, &changed[2..])
            .expect("root tree");
        assert_eq!(
            root_tree,
            engine.tree_oid(&repository, &to).expect("target tree")
        );
        let patch = engine
            .diff_patch(&repository, &from, &area_commit, &area_paths)
            .expect("area patch");
        assert!(patch.contains("Area/One.md"));
        assert!(patch.contains("Area/Two.md"));
        assert!(!patch.contains("Root.md"));

        let proposal = GitRefName::parse("refs/vulcan/proposals/semantic/test").expect("ref");
        assert_eq!(
            engine
                .compare_and_swap_ref(&repository, &proposal, &area_commit, None)
                .expect("create proposal"),
            GitRefUpdateResult::Updated
        );
        assert_eq!(
            engine
                .compare_and_swap_ref(&repository, &proposal, &to, None)
                .expect("stale create"),
            GitRefUpdateResult::Stale
        );
        assert_eq!(
            engine
                .compare_and_swap_ref(&repository, &proposal, &to, Some(&area_commit))
                .expect("advance proposal"),
            GitRefUpdateResult::Updated
        );
    }

    #[test]
    fn applying_a_tree_updates_files_without_touching_the_normal_index() {
        let temporary = TempDir::new().expect("temporary directory");
        init_repo(temporary.path());
        fs::write(temporary.path().join("Home.md"), "initial\n").expect("initial note");
        let head = commit_all(temporary.path(), "initial");
        let normal_index = run_git_capture(temporary.path(), &["write-tree"]);
        fs::write(temporary.path().join("Home.md"), "accepted\n").expect("accepted note");
        fs::write(temporary.path().join("New.md"), "new\n").expect("new note");
        let engine = GitCliEngine::default();
        let repository = engine
            .discover_repository(temporary.path())
            .expect("repository");
        let local_ref = GitRefName::parse("refs/vulcan/sync/local/live").expect("ref");
        let accepted = engine
            .capture_worktree(
                &repository,
                &GitCaptureRequest {
                    base: Some(head.clone()),
                    target_ref: local_ref,
                    message: "accepted\n".to_string(),
                },
            )
            .expect("capture");
        fs::write(temporary.path().join("Home.md"), "initial\n").expect("restore note");
        fs::remove_file(temporary.path().join("New.md")).expect("remove new note");

        engine
            .apply_tree(&repository, &head, &accepted.commit)
            .expect("tree should apply");

        assert_eq!(
            fs::read_to_string(temporary.path().join("Home.md")).expect("home"),
            "accepted\n"
        );
        assert_eq!(
            fs::read_to_string(temporary.path().join("New.md")).expect("new"),
            "new\n"
        );
        assert_eq!(
            run_git_capture(temporary.path(), &["write-tree"]),
            normal_index
        );
    }

    #[test]
    fn safety_state_detects_staged_changes() {
        let temporary = TempDir::new().expect("temporary directory");
        init_repo(temporary.path());
        fs::write(temporary.path().join("Home.md"), "initial\n").expect("initial note");
        commit_all(temporary.path(), "initial");
        let engine = GitCliEngine::default();
        let repository = engine
            .discover_repository(temporary.path())
            .expect("repository");
        assert!(
            !engine
                .safety_state(&repository)
                .expect("state")
                .staged_changes
        );

        fs::write(temporary.path().join("Home.md"), "staged\n").expect("updated note");
        run_git(temporary.path(), &["add", "Home.md"]);
        assert!(
            engine
                .safety_state(&repository)
                .expect("state")
                .staged_changes
        );
    }

    #[test]
    fn repository_requirements_report_ignored_state_and_filters() {
        let temporary = TempDir::new().expect("temporary directory");
        init_repo(temporary.path());
        fs::write(temporary.path().join(".gitignore"), ".vulcan/cache.db*\n")
            .expect("ignore rules");
        fs::write(
            temporary.path().join(".gitattributes"),
            "*.bin filter=lfs\n",
        )
        .expect("attributes");
        fs::write(temporary.path().join("asset.bin"), "pointer\n").expect("asset");
        commit_all(temporary.path(), "requirements");
        let engine = GitCliEngine::default();
        let repository = engine
            .discover_repository(temporary.path())
            .expect("repository");

        let requirements = engine
            .repository_requirements(&repository)
            .expect("requirements");

        assert_eq!(requirements.tracked_paths, 3);
        assert_eq!(
            requirements.ignored_internal_paths,
            [
                ".vulcan/cache.db",
                ".vulcan/cache.db-wal",
                ".vulcan/cache.db-shm"
            ]
        );
        assert_eq!(
            requirements.required_filters,
            [GitFilterRequirement {
                name: "lfs".to_string(),
                path_count: 1,
            }]
        );
        assert!(requirements.git_lfs_available.is_some());
    }
}
