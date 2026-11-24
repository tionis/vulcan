use crate::{detached_recovery_ref, local_recovery_ref_namespaces};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::fmt::{Display, Formatter};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
#[cfg(all(test, unix))]
use std::time::Instant;
use tempfile::{NamedTempFile, TempDir};
use ulid::Ulid;
use wait_timeout::ChildExt;

/// A synchronous blob consumer. Drop each payload after processing to retain
/// only one raw blob; callbacks may run on the scoped Git stdout reader thread.
pub type GitBlobVisitor<'a> = dyn FnMut(&GitOid, Vec<u8>) -> Result<(), GitEngineError> + Send + 'a;

const MAX_ERROR_BYTES: usize = 16 * 1024;
const MAX_DIAGNOSTIC_PATH_BYTES: usize = 16 * 1024 * 1024;
const MAX_CONFLICT_BLOB_BYTES: usize = 64 * 1024 * 1024;
/// Paths per scoped `ls-tree`, keeping each command line well bounded.
const SCOPED_TREE_LISTING_CHUNK: usize = 256;
const SCOPED_TREE_LISTING_MAX_CHUNKS: usize = 4;
const MAX_CONFLICT_BLOB_BATCH_BYTES: usize = 256 * 1024 * 1024;
const DEFAULT_GIT_COMMAND_TIMEOUT: Duration = Duration::from_secs(300);
#[cfg(unix)]
const TERMINATION_GRACE_PERIOD: Duration = Duration::from_secs(2);
#[cfg(windows)]
const WINDOWS_GIT_CREATION_FLAGS: u32 = 0x0800_0000; // CREATE_NO_WINDOW
const REQUIREMENTS_CACHE_VERSION: u32 = 1;
const DEVICE_LOCAL_VULCAN_PATHS: [&str; 8] = [
    ".vulcan/config.local.toml",
    ".vulcan/cache.db",
    ".vulcan/cache.db-wal",
    ".vulcan/cache.db-shm",
    ".vulcan/write.lock",
    ".vulcan/write.intent",
    ".vulcan/editor-watch.lock",
    ".vulcan/index-owner.lock",
];
/// Excludes for the capture's `git add`. The final character is a bracket
/// expression so Git treats each as a wildcard pattern: a literal exclude
/// that names an ignored file is reported as an explicitly requested ignored
/// addition and fails the capture. Excluding them keeps the capture from
/// reading files another process holds locked (Windows refuses reads of a
/// held `write.lock`) and from hashing the cache.
const DEVICE_LOCAL_CAPTURE_EXCLUDES: [&str; 8] = [
    ":(exclude).vulcan/config.local.tom[l]",
    ":(exclude).vulcan/cache.d[b]",
    ":(exclude).vulcan/cache.db-wa[l]",
    ":(exclude).vulcan/cache.db-sh[m]",
    ":(exclude).vulcan/write.loc[k]",
    ":(exclude).vulcan/write.inten[t]",
    ":(exclude).vulcan/editor-watch.loc[k]",
    ":(exclude).vulcan/index-owner.loc[k]",
];
const WORKTREE_CAPTURE_PATHS: [&str; 9] = [
    ".",
    ":(exclude).vulcan/config.local.toml",
    ":(exclude).vulcan/cache.db",
    ":(exclude).vulcan/cache.db-wal",
    ":(exclude).vulcan/cache.db-shm",
    ":(exclude).vulcan/write.lock",
    ":(exclude).vulcan/write.intent",
    ":(exclude).vulcan/editor-watch.lock",
    ":(exclude).vulcan/index-owner.lock",
];
const REPOSITORY_ENVIRONMENT_OVERRIDES: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_NAMESPACE",
    // Environment-injected configuration would silently change engine
    // behavior (aliases, filters, protocol settings) for every command.
    "GIT_CONFIG_COUNT",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_SYSTEM",
    "GIT_CONFIG_NOSYSTEM",
    "XDG_CONFIG_HOME",
];

/// A typed boundary over the repository implementation used by Git-backed sync.
/// Exact remote ref tips in request order, plus best-effort refs beneath an
/// optional namespace (`None` when not requested or not parseable).
pub type GitRemoteRefs = (Vec<Option<GitOid>>, Option<Vec<GitReference>>);

pub trait GitEngine: Send + Sync {
    fn kind(&self) -> GitEngineKind;

    /// Returns the number of successfully spawned Git subprocesses on the
    /// current thread when the engine can observe that boundary.
    fn subprocess_count(&self) -> Option<u64> {
        None
    }

    fn installation(&self) -> Result<GitInstallation, GitEngineError>;

    fn discover_repository(&self, path: &Path) -> Result<GitRepository, GitEngineError>;

    fn clone_repository(&self, request: &GitCloneRequest) -> Result<GitRepository, GitEngineError>;

    fn read_ref(
        &self,
        repository: &GitRepository,
        reference: &GitRefName,
    ) -> Result<Option<GitOid>, GitEngineError>;

    /// Lists direct refs beneath one validated namespace without resolving or
    /// following symbolic refs.
    fn list_refs(
        &self,
        repository: &GitRepository,
        prefix: &GitRefName,
    ) -> Result<Vec<GitReference>, GitEngineError>;

    fn head_commit(&self, repository: &GitRepository) -> Result<Option<GitOid>, GitEngineError>;

    fn head_reference(
        &self,
        repository: &GitRepository,
    ) -> Result<Option<GitRefName>, GitEngineError>;

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

    fn delete_ref(
        &self,
        repository: &GitRepository,
        reference: &GitRefName,
        expected: &GitOid,
    ) -> Result<GitRefDeleteResult, GitEngineError>;

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

    /// Reads selected path objects while enforcing both unique-object reads
    /// and logical per-path aggregate bytes before cloning shared blob data.
    fn path_objects_bounded(
        &self,
        repository: &GitRepository,
        revision: &GitOid,
        paths: &[String],
        per_blob_limit: usize,
        total_limit: usize,
    ) -> Result<BTreeMap<String, GitPathObject>, GitEngineError> {
        let entries = self.tree_entries_for_paths(repository, revision, paths)?;
        let blob_ids = entries
            .iter()
            .filter(|entry| entry.kind == "blob")
            .map(|entry| entry.oid.clone())
            .collect::<Vec<_>>();
        let blobs = self.read_blobs_bounded(repository, &blob_ids, per_blob_limit, total_limit)?;
        let mut logical_total = 0_usize;
        entries
            .into_iter()
            .map(|entry| {
                let data = if entry.kind == "blob" {
                    let data =
                        blobs
                            .get(&entry.oid)
                            .ok_or_else(|| GitEngineError::InvalidOutput {
                                operation: "read selected Git path objects",
                                detail: format!("batch response omitted blob `{}`", entry.oid),
                            })?;
                    logical_total = logical_total.checked_add(data.len()).ok_or_else(|| {
                        GitEngineError::InvalidOutput {
                            operation: "read selected Git path objects",
                            detail: "logical selected-path blob size overflowed".to_string(),
                        }
                    })?;
                    if logical_total > total_limit {
                        return Err(blob_batch_limit_error(total_limit));
                    }
                    Some(data.clone())
                } else {
                    None
                };
                Ok((
                    entry.path,
                    GitPathObject {
                        oid: entry.oid,
                        mode: entry.mode,
                        kind: entry.kind,
                        data,
                    },
                ))
            })
            .collect()
    }

    /// Reads a set of known blob objects through one repository command.
    /// Duplicate object IDs are collapsed in the returned map.
    fn read_blobs(
        &self,
        repository: &GitRepository,
        objects: &[GitOid],
    ) -> Result<BTreeMap<GitOid, Vec<u8>>, GitEngineError>;

    /// Reads known blobs while enforcing individual and aggregate byte limits.
    /// Engines should reject declared oversize objects before materializing
    /// their contents; the default preserves compatibility for other engines.
    fn read_blobs_bounded(
        &self,
        repository: &GitRepository,
        objects: &[GitOid],
        per_blob_limit: usize,
        total_limit: usize,
    ) -> Result<BTreeMap<GitOid, Vec<u8>>, GitEngineError> {
        let blobs = self.read_blobs(repository, objects)?;
        let mut total = 0_usize;
        for (oid, data) in &blobs {
            if data.len() > per_blob_limit {
                return Err(blob_limit_error(oid, per_blob_limit));
            }
            total = total
                .checked_add(data.len())
                .ok_or_else(|| GitEngineError::InvalidOutput {
                    operation: "read Git blobs in a batch",
                    detail: "aggregate blob size overflowed".to_string(),
                })?;
            if total > total_limit {
                return Err(blob_batch_limit_error(total_limit));
            }
        }
        Ok(blobs)
    }

    fn changed_paths(
        &self,
        repository: &GitRepository,
        from: &GitOid,
        to: &GitOid,
    ) -> Result<Vec<String>, GitEngineError>;

    /// Lists every leaf entry in a commit or tree without loading object contents.
    fn tree_entries(
        &self,
        repository: &GitRepository,
        revision: &GitOid,
    ) -> Result<Vec<GitTreeEntry>, GitEngineError>;

    /// Builds a tree from `base` (a tree or commit; empty when `None`) with
    /// `entries` added or replaced. Uses a scratch index, so no index or
    /// worktree state is touched; the entries' objects must already exist.
    fn tree_with_entries(
        &self,
        repository: &GitRepository,
        base: Option<&GitOid>,
        entries: &[GitTreeEntry],
    ) -> Result<GitOid, GitEngineError>;

    /// Lists the direct children of a commit's or tree's root, with trees as
    /// `tree` entries, without recursing.
    fn tree_children(
        &self,
        repository: &GitRepository,
        revision: &GitOid,
    ) -> Result<Vec<GitTreeEntry>, GitEngineError>;

    /// Writes a tree whose direct children are exactly `entries`, whose
    /// paths are single names and whose objects already exist.
    fn make_tree(
        &self,
        repository: &GitRepository,
        entries: &[GitTreeEntry],
    ) -> Result<GitOid, GitEngineError>;

    /// Resolves each `<revision>:<path>` to its object, `None` when absent, in
    /// one Git process.
    fn resolve_paths(
        &self,
        repository: &GitRepository,
        revision: &GitOid,
        paths: &[String],
    ) -> Result<Vec<Option<GitOid>>, GitEngineError>;

    /// Lists every leaf entry beneath the directory `prefix` in a commit or
    /// tree, without listing the rest of the tree or loading contents.
    fn tree_entries_under(
        &self,
        repository: &GitRepository,
        revision: &GitOid,
        prefix: &str,
    ) -> Result<Vec<GitTreeEntry>, GitEngineError>;

    /// Lists the leaf entries at exactly `paths` in a commit or tree, without
    /// loading object contents. Engines should keep the work proportional to
    /// the selected paths rather than to the whole tree.
    fn tree_entries_for_paths(
        &self,
        repository: &GitRepository,
        revision: &GitOid,
        paths: &[String],
    ) -> Result<Vec<GitTreeEntry>, GitEngineError> {
        let selected = paths.iter().map(String::as_str).collect::<BTreeSet<_>>();
        Ok(self
            .tree_entries(repository, revision)?
            .into_iter()
            .filter(|entry| selected.contains(entry.path.as_str()))
            .collect())
    }

    /// Validates a unified patch against one immutable revision without writing Git objects.
    fn check_patch(
        &self,
        repository: &GitRepository,
        base: &GitOid,
        patch: &[u8],
    ) -> Result<Vec<String>, GitEngineError>;

    fn capture_worktree(
        &self,
        repository: &GitRepository,
        request: &GitCaptureRequest,
    ) -> Result<GitCapture, GitEngineError>;

    /// Hashes a stable worktree snapshot into a tree without creating a commit or ref.
    /// With a worktree scope, only scoped paths come from the worktree and the
    /// rest from `base`, so comparing against `base` compares the scope.
    fn snapshot_worktree_tree(
        &self,
        repository: &GitRepository,
        base: Option<&GitOid>,
    ) -> Result<GitOid, GitEngineError>;

    /// Checks the complete worktree against a tree without writing Git objects,
    /// refs, or the user's normal index. Implementations may use a private
    /// temporary index so paths added by the expected tree are not mistaken for
    /// unrelated untracked files.
    fn worktree_matches_tree(
        &self,
        repository: &GitRepository,
        expected: &GitOid,
    ) -> Result<bool, GitEngineError>;

    fn remote_ref(
        &self,
        repository: &GitRepository,
        remote: &GitRemote,
        reference: &GitRefName,
    ) -> Result<Option<GitOid>, GitEngineError>;

    /// Lists direct remote refs beneath one validated namespace without
    /// fetching their objects or updating local refs.
    fn list_remote_refs(
        &self,
        repository: &GitRepository,
        remote: &GitRemote,
        prefix: &GitRefName,
    ) -> Result<Vec<GitReference>, GitEngineError>;

    /// Observes several exact refs and, optionally, every ref beneath `prefix`
    /// in one remote trip. The returned tips follow the order of `references`.
    ///
    /// Exact refs are strict, like [`GitEngine::remote_ref`]. The prefixed refs
    /// are best effort, as in [`GitEngine::remote_ref_with_prefix`].
    fn remote_refs(
        &self,
        repository: &GitRepository,
        remote: &GitRemote,
        references: &[&GitRefName],
        prefix: Option<&GitRefName>,
    ) -> Result<GitRemoteRefs, GitEngineError>;

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

    fn merge_base(
        &self,
        repository: &GitRepository,
        left: &GitOid,
        right: &GitOid,
    ) -> Result<Option<GitOid>, GitEngineError>;

    fn merge_commits(
        &self,
        repository: &GitRepository,
        accepted_remote: &GitOid,
        local_candidate: &GitOid,
    ) -> Result<GitMerge, GitEngineError>;

    fn create_commit(
        &self,
        repository: &GitRepository,
        tree: &GitOid,
        parents: &[GitOid],
        message: &str,
    ) -> Result<GitOid, GitEngineError>;

    fn create_reproducible_commit(
        &self,
        repository: &GitRepository,
        tree: &GitOid,
        parents: &[GitOid],
        message: &str,
    ) -> Result<GitOid, GitEngineError>;

    /// Writes blob bytes directly to the object store without touching the
    /// worktree, the user's index, or any temporary files.
    fn write_blob(&self, repository: &GitRepository, data: &[u8])
        -> Result<GitOid, GitEngineError>;

    /// Writes multiple blobs while preserving input order. Engines may
    /// override this to use one bounded process transaction.
    fn write_blobs(
        &self,
        repository: &GitRepository,
        data: &[&[u8]],
    ) -> Result<Vec<GitOid>, GitEngineError> {
        data.iter()
            .map(|data| self.write_blob(repository, data))
            .collect()
    }

    /// Creates a single-file `100644` tree for one blob without touching the
    /// worktree, the user's index, or any temporary files.
    fn create_single_file_tree(
        &self,
        repository: &GitRepository,
        path: &str,
        blob: &GitOid,
    ) -> Result<GitOid, GitEngineError>;

    fn push_ref(
        &self,
        repository: &GitRepository,
        remote: &GitRemote,
        source: &GitOid,
        destination: &GitRefName,
        expected: Option<&GitOid>,
    ) -> Result<GitPushResult, GitEngineError>;

    /// Deletes several remote refs, each only while it still names its
    /// expected commit. The CLI engine sends one push; the outcomes follow
    /// the input order, and a ref already gone may be reported as `Stale`.
    fn delete_remote_refs(
        &self,
        repository: &GitRepository,
        remote: &GitRemote,
        deletions: &[(GitRefName, GitOid)],
    ) -> Result<Vec<GitRefDeleteResult>, GitEngineError> {
        deletions
            .iter()
            .map(|(reference, expected)| {
                self.delete_remote_ref(repository, remote, reference, expected)
            })
            .collect()
    }

    fn delete_remote_ref(
        &self,
        repository: &GitRepository,
        remote: &GitRemote,
        reference: &GitRefName,
        expected: &GitOid,
    ) -> Result<GitRefDeleteResult, GitEngineError>;

    /// Resolves the configured upstream of a local branch using Git's native
    /// refspec mapping. Returns `None` when the branch tracks nothing.
    fn branch_upstream(
        &self,
        repository: &GitRepository,
        branch: &GitRefName,
    ) -> Result<Option<GitBranchUpstream>, GitEngineError>;

    /// Reads the pull strategy for a branch exactly as `git pull` resolves
    /// it: `branch.<name>.rebase` overrides `pull.rebase`, plus `pull.ff`.
    fn branch_pull_config(
        &self,
        repository: &GitRepository,
        branch: &GitRefName,
    ) -> Result<BranchPullConfig, GitEngineError>;

    /// Fast-forwards a branch with `git merge --ff-only`, refusing to
    /// overwrite dirty worktree content. Requires a worktree.
    fn fast_forward_branch(
        &self,
        repository: &GitRepository,
        branch: &GitRefName,
        target: &GitOid,
    ) -> Result<FastForwardOutcome, GitEngineError>;

    /// Merges a revision into the checked-out branch with `--no-edit`,
    /// leaving conflicts in place for the caller to detect. Requires a
    /// worktree and never opens an editor. Pass `no_ff` to honor
    /// `pull.ff=false` with an explicit merge commit.
    fn merge_branch(
        &self,
        repository: &GitRepository,
        target: &GitOid,
        no_ff: bool,
    ) -> Result<MergeBranchOutcome, GitEngineError>;

    /// Reports why rebasing the checked-out branch onto `upstream` could
    /// rewrite commits something else builds on: a replayed commit is also
    /// reachable from another branch, tag, remote-tracking ref, or another
    /// worktree's HEAD, or the range holds merge commits a rebase would
    /// flatten. Vulcan's own refs (`refs/vulcan/*` and the `__vulcan-sync`
    /// branch namespace, local or remote-tracking) are not consulted; they
    /// always contain HEAD. `None` means the rebase rewrites only commits
    /// that exist on this branch alone.
    fn rebase_hazard(
        &self,
        repository: &GitRepository,
        branch: &GitRefName,
        upstream: &GitOid,
    ) -> Result<Option<RebaseHazard>, GitEngineError>;

    /// Replays the checked-out branch's own commits onto `upstream` in a
    /// temporary detached worktree, asking `resolver` for the content of each
    /// conflicted file. Neither the branch nor the checkout changes; apply
    /// the result with [`GitEngine::apply_branch_replay`]. Commits that
    /// become empty on the upstream are dropped, as `git rebase` does.
    /// Hooks do not run and commits are not signed. Declines when a rebase
    /// hazard exists, the commits touch a path with a Git `filter`
    /// attribute, a conflict involves a binary file, symlink, submodule, or
    /// deletion, or the resolver fails or leaves conflict markers.
    fn prepare_branch_replay(
        &self,
        repository: &GitRepository,
        branch: &GitRefName,
        upstream: &GitOid,
        resolver: &mut dyn BranchReplayResolver,
    ) -> Result<Result<PreparedBranchReplay, String>, GitEngineError>;

    /// Moves the checked-out branch from the prepared replay's `from` to its
    /// `to`: a two-way checkout that refuses to overwrite uncommitted work,
    /// then a compare-and-swap. Declines, changing nothing, when uncommitted
    /// work is in the way or the branch moved since the replay was prepared.
    fn apply_branch_replay(
        &self,
        repository: &GitRepository,
        branch: &GitRefName,
        prepared: PreparedBranchReplay,
    ) -> Result<BranchReplayOutcome, GitEngineError>;

    /// Prepares and applies a replay in one step, for callers that already
    /// hold the repository lock and nothing slow runs in the resolver.
    fn replay_branch(
        &self,
        repository: &GitRepository,
        branch: &GitRefName,
        upstream: &GitOid,
        resolver: &mut dyn BranchReplayResolver,
    ) -> Result<BranchReplayOutcome, GitEngineError> {
        match self.prepare_branch_replay(repository, branch, upstream, resolver)? {
            Ok(prepared) => self.apply_branch_replay(repository, branch, prepared),
            Err(reason) => Ok(BranchReplayOutcome::Declined { reason }),
        }
    }

    /// Rebases the checked-out branch onto a revision, leaving conflicts in
    /// place for the caller to detect. Requires a worktree.
    fn rebase_branch(
        &self,
        repository: &GitRepository,
        upstream: &GitOid,
        merges: bool,
    ) -> Result<RebaseOutcome, GitEngineError>;

    /// Aborts a conflicted merge or rebase that `merge_branch` or
    /// `rebase_branch` left in place, restoring the branch, index, and
    /// worktree to their state before the operation started. Returns
    /// whether an operation was aborted.
    fn abort_branch_operation(&self, repository: &GitRepository) -> Result<bool, GitEngineError>;

    /// Fast-forwards the checked-out branch from `expected` to `target`
    /// without touching worktree bytes. The normal index takes the two-way
    /// index-only merge of a fast-forward checkout: entries equal to
    /// `expected` move to `target`, other staged entries are preserved, and a
    /// staged entry on a path `target` also changes refuses the advance with
    /// the index untouched. The branch then moves by compare-and-swap.
    fn advance_checked_out_branch(
        &self,
        repository: &GitRepository,
        branch: &GitRefName,
        expected: &GitOid,
        target: &GitOid,
    ) -> Result<BranchAdvanceOutcome, GitEngineError>;

    fn safety_state(&self, repository: &GitRepository) -> Result<GitSafetyState, GitEngineError>;

    /// Read-only repository facts used to decide whether unattended
    /// files-only synchronization is safe for an active development checkout.
    fn unattended_repository_state(
        &self,
        repository: &GitRepository,
    ) -> Result<GitUnattendedRepositoryState, GitEngineError>;

    /// `vault_root` locates the vault's `.vulcan/` state inside the work
    /// tree, which need not be the repository root.
    fn repository_requirements(
        &self,
        repository: &GitRepository,
        vault_root: &Path,
    ) -> Result<GitRepositoryRequirements, GitEngineError>;

    fn persist_repository_requirements_cache(
        &self,
        _repository: &GitRepository,
    ) -> Result<(), GitEngineError> {
        Ok(())
    }
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitDetachedRecoveryRequest {
    pub source: String,
    pub work_tree: PathBuf,
    pub git_dir: PathBuf,
    pub platform: GitPlatformProfile,
}

impl GitDetachedRecoveryRequest {
    pub fn validate(&self) -> Result<(), GitEngineError> {
        validate_detached_recovery_request(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitDetachedRecoveryReport {
    pub installation: GitInstallation,
    pub repository: GitRepository,
    pub recovery_ref: GitRefName,
    pub recovery_commit: GitOid,
    pub recovery_tree: GitOid,
    pub fetched_remote: GitRemote,
    pub possibly_lost_hidden_ref_namespaces: Vec<String>,
}

impl GitCloneRequest {
    pub fn validate(&self) -> Result<(), GitEngineError> {
        validate_git_source(&self.source)?;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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

    pub fn parse(value: &str) -> Result<Self, GitEngineError> {
        match value {
            "linux_native" => Ok(Self::LinuxNative),
            "windows_native" => Ok(Self::WindowsNative),
            "android_shared" => Ok(Self::AndroidShared),
            "other_native" => Ok(Self::OtherNative),
            _ => Err(GitEngineError::UnsupportedRepository {
                detail: format!("unknown Git platform profile `{value}`"),
            }),
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
                        key: "core.autocrlf",
                        value: "false",
                    },
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
                clone_config: vec![GitCloneConfig {
                    key: "core.autocrlf",
                    value: "false",
                }],
            },
            Self::LinuxNative => GitPlatformPolicy {
                profile: self,
                executable_bits: GitExecutableBitsPolicy::GitProbed,
                symlinks: GitSymlinkPolicy::GitProbed,
                case_only_renames: GitCaseRenamePolicy::Native,
                reserved_names: GitReservedNamesPolicy::Native,
                path_length: GitPathLengthPolicy::FilesystemDependent,
                timestamp_precision: GitTimestampPolicy::ContentVerified,
                clone_config: vec![GitCloneConfig {
                    key: "core.autocrlf",
                    value: "false",
                }],
            },
            Self::OtherNative => GitPlatformPolicy {
                profile: self,
                executable_bits: GitExecutableBitsPolicy::GitProbed,
                symlinks: GitSymlinkPolicy::GitProbed,
                case_only_renames: GitCaseRenamePolicy::IntermediatePath,
                reserved_names: GitReservedNamesPolicy::Native,
                path_length: GitPathLengthPolicy::FilesystemDependent,
                timestamp_precision: GitTimestampPolicy::ContentVerified,
                clone_config: vec![GitCloneConfig {
                    key: "core.autocrlf",
                    value: "false",
                }],
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitExecutableBitsPolicy {
    GitProbed,
    NotRepresentable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitSymlinkPolicy {
    GitProbed,
    LinkFiles,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitCaseRenamePolicy {
    Native,
    IntermediatePath,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitReservedNamesPolicy {
    Native,
    WindowsRestricted,
    WindowsPortable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitPathLengthPolicy {
    FilesystemDependent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitTimestampPolicy {
    ContentVerified,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

impl<'de> Deserialize<'de> for GitOid {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(serde::de::Error::custom)
    }
}

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
pub struct GitReference {
    pub name: GitRefName,
    pub target: GitOid,
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

/// A local branch and its configured upstream tracking ref.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitBranchUpstream {
    pub branch: GitRefName,
    pub remote: GitRemote,
    /// The branch ref as known on the remote (e.g. `refs/heads/main`).
    pub merge_ref: GitRefName,
    /// The local remote-tracking ref (e.g. `refs/remotes/origin/main`).
    pub tracking_ref: GitRefName,
}

/// One conflicted text file met while replaying a local commit: its
/// content in the commit's parent (`base`), on the branch being replayed
/// onto (`upstream`), and in the local commit (`local`). `None` is absent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchReplayConflict {
    pub path: String,
    pub base: Option<String>,
    pub upstream: Option<String>,
    pub local: Option<String>,
}

/// Supplies the resolved content of conflicted files during a replay.
pub trait BranchReplayResolver {
    /// Returns the full resolved content of every path in `conflicts`.
    /// `subject` is the local commit's message subject.
    fn resolve(
        &mut self,
        subject: &str,
        conflicts: &[BranchReplayConflict],
    ) -> Result<Vec<(String, String)>, String>;
}

/// Replayed commits ready to become the branch tip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PreparedBranchReplay {
    /// The branch tip the replay started from.
    pub from: GitOid,
    /// The replayed tip.
    pub to: GitOid,
    /// Files the resolver supplied.
    pub resolved: Vec<String>,
    /// `commit.gpgSign` is on, but the replayed commits are unsigned.
    pub signatures_dropped: bool,
}

/// What a branch replay did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum BranchReplayOutcome {
    /// The branch moved from `from` to `to`; `resolved` lists the files the
    /// resolver supplied.
    Replayed {
        from: GitOid,
        to: GitOid,
        resolved: Vec<String>,
        /// `commit.gpgSign` is on, but the replayed commits are unsigned.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        signatures_dropped: bool,
    },
    /// Nothing changed, for the stated reason.
    Declined { reason: String },
}

/// Why a guarded rebase declined to run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RebaseHazard {
    /// A commit the rebase would replay is also reachable from this ref
    /// (a branch, tag, remote-tracking ref, or `worktree:<path>`).
    SharedWith(String),
    /// The commits to replay include merge commits.
    MergeCommits,
}

/// How `pull.ff` resolves for a branch pull.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PullFastForward {
    /// Fast-forward when possible, merge otherwise.
    Always,
    /// Never fast-forward; always create a merge commit.
    Never,
    /// Fast-forward or refuse when diverged.
    Only,
}

/// How `pull.rebase` / `branch.<name>.rebase` resolves for a branch pull.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PullRebase {
    Never,
    Yes,
    Merges,
    Interactive,
}

/// The pull strategy of one branch, resolved exactly as `git pull` reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct BranchPullConfig {
    pub fast_forward: PullFastForward,
    pub rebase: PullRebase,
}

/// Outcome of `git merge --ff-only` on a branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FastForwardOutcome {
    UpToDate,
    Advanced,
    NotFastForwardable,
    BlockedDirty,
}

/// Outcome of `git merge --no-edit` on the checked-out branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeBranchOutcome {
    UpToDate,
    Merged,
    Conflicted,
    BlockedDirty,
}

/// Outcome of `git rebase` on the checked-out branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RebaseOutcome {
    UpToDate,
    Rebased,
    Conflicted,
    BlockedDirty,
}

/// Outcome of an index-only fast-forward of the checked-out branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BranchAdvanceOutcome {
    Advanced,
    /// A staged entry differs from both sides on a path the target changes.
    BlockedStaged,
    /// The branch or `HEAD` no longer identifies the expected revision.
    Stale,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitCaptureRequest {
    pub base: Option<GitOid>,
    pub target_ref: GitRefName,
    /// Exact target value observed by a serialized caller, if it already equals `base`.
    pub target_before: Option<GitOid>,
    pub message: String,
    /// Extra parent recorded after `base`. Capture always creates a commit
    /// when set, even for an unchanged tree, so the caller passes it only
    /// when `base` does not already contain it.
    pub link: Option<GitOid>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitCapture {
    pub commit: GitOid,
    pub tree: GitOid,
    pub created: bool,
}

struct GitWorktreeTreeCapture {
    tree: GitOid,
    reused_base: bool,
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
pub struct GitTreeEntry {
    pub path: String,
    pub oid: GitOid,
    pub mode: String,
    pub kind: String,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GitRefDeleteResult {
    Deleted,
    Missing,
    Stale,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitSafetyState {
    pub staged_changes: bool,
    pub operation: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitUnattendedRepositoryState {
    pub attached_head: bool,
    pub head_reference: Option<GitRefName>,
    pub staged_changes: bool,
    pub worktree_count: usize,
    pub operation: Option<String>,
    pub nested_repositories: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitFilterRequirement {
    pub name: String,
    pub path_count: usize,
    pub clean_configured: bool,
    pub smudge_configured: bool,
    pub process_configured: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub executable_available: Option<bool>,
}

impl GitFilterRequirement {
    #[must_use]
    pub fn ready(&self) -> bool {
        (self.process_configured || (self.clean_configured && self.smudge_configured))
            && self.executable_available != Some(false)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitRepositoryRequirements {
    pub tracked_paths: usize,
    pub ignored_internal_paths: Vec<String>,
    pub required_filters: Vec<GitFilterRequirement>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_lfs_available: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GitRequirementsCache {
    version: u32,
    key: String,
    filters: BTreeMap<String, usize>,
}

struct GitRequirementsEnvironment {
    configured_filter_keys: BTreeSet<String>,
    attribute_sources: Vec<(String, String)>,
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
    CommandTimedOut {
        operation: &'static str,
        timeout: Duration,
    },
    RemoteRefMissing {
        remote: GitRemote,
        reference: GitRefName,
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
    ApplyWouldOverwrite {
        paths: Vec<String>,
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
            Self::CommandTimedOut { operation, timeout } => write!(
                formatter,
                "Git timed out while trying to {operation} after {:.3} seconds",
                timeout.as_secs_f64()
            ),
            Self::RemoteRefMissing { remote, reference } => write!(
                formatter,
                "Git remote `{remote}` does not contain `{reference}`"
            ),
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
            Self::ApplyWouldOverwrite { paths } => write!(
                formatter,
                "applying the accepted tree would overwrite untracked worktree files excluded by Git ignore rules; move or remove them first: {}",
                paths
                    .iter()
                    .take(8)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
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
            | Self::CommandTimedOut { .. }
            | Self::RemoteRefMissing { .. }
            | Self::InvalidOutput { .. }
            | Self::InvalidObjectId(_)
            | Self::InvalidRefName(_)
            | Self::InvalidRemote(_)
            | Self::UnsupportedRepository { .. }
            | Self::ApplyWouldOverwrite { .. }
            | Self::WorktreeChanged => None,
        }
    }
}

impl From<std::io::Error> for GitEngineError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

/// Text to replace in Git's stderr, and the label replacing it.
type StderrRedaction = (Box<[u8]>, &'static str);

#[derive(Debug, Clone)]
pub struct GitCliEngine {
    executable: PathBuf,
    command_timeout: Duration,
    installation: Arc<OnceLock<GitInstallation>>,
    pending_requirements: Arc<Mutex<BTreeMap<PathBuf, GitRequirementsCache>>>,
    subprocess_count: Arc<AtomicU64>,
    /// Exact `GIT_SSH_COMMAND` for every spawned Git process. Never logged.
    ssh_command: Option<Arc<str>>,
    /// Text replaced in every Git process's stderr before it can reach an
    /// error, report, or journal, in order: for example the device key file
    /// that ssh may name.
    stderr_redactions: Arc<[StderrRedaction]>,
}

impl PartialEq for GitCliEngine {
    fn eq(&self, other: &Self) -> bool {
        self.executable == other.executable
            && self.command_timeout == other.command_timeout
            && self.ssh_command == other.ssh_command
    }
}

impl Eq for GitCliEngine {}

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
            command_timeout: DEFAULT_GIT_COMMAND_TIMEOUT,
            installation: Arc::new(OnceLock::new()),
            pending_requirements: Arc::new(Mutex::new(BTreeMap::new())),
            subprocess_count: Arc::new(AtomicU64::new(0)),
            ssh_command: None,
            stderr_redactions: Arc::from(Vec::new()),
        }
    }

    /// Replaces every occurrence of `text` in Git's stderr with `label`.
    /// Redactions apply in the order added, so add longer texts first.
    #[must_use]
    pub fn with_stderr_redaction(mut self, text: &str, label: &'static str) -> Self {
        if !text.is_empty() {
            let mut redactions = self.stderr_redactions.to_vec();
            redactions.push((text.as_bytes().into(), label));
            self.stderr_redactions = Arc::from(redactions);
        }
        self
    }

    fn redact_stderr(&self, mut stderr: Vec<u8>) -> Vec<u8> {
        for (text, label) in self.stderr_redactions.iter() {
            stderr = replace_bytes(&stderr, text, label.as_bytes());
        }
        stderr
    }

    /// Authenticates every spawned Git process with this exact SSH command,
    /// overriding any ambient `GIT_SSH`/`GIT_SSH_COMMAND` and repository
    /// `core.sshCommand`. The value is a shell command line as Git expects.
    #[must_use]
    pub fn with_ssh_command(mut self, command: impl Into<String>) -> Self {
        self.ssh_command = Some(Arc::from(command.into()));
        self
    }

    #[must_use]
    pub fn executable(&self) -> &Path {
        &self.executable
    }

    #[must_use]
    pub fn command_timeout(&self) -> Duration {
        self.command_timeout
    }

    #[must_use]
    pub fn with_command_timeout(mut self, timeout: Duration) -> Self {
        self.command_timeout = timeout.max(Duration::from_millis(1));
        self
    }

    /// Starts an independently counted transaction while retaining immutable
    /// installation and repository-requirement caches.
    #[must_use]
    pub fn with_fresh_metrics(mut self) -> Self {
        self.subprocess_count = Arc::new(AtomicU64::new(0));
        self
    }

    fn record_subprocess(&self) {
        self.subprocess_count.fetch_add(1, Ordering::Relaxed);
    }

    fn observed_subprocess_count(&self) -> u64 {
        self.subprocess_count.load(Ordering::Relaxed)
    }

    /// Recreates a registered detached Git directory without checking remote
    /// content out over the materialized worktree.
    ///
    /// The existing `.git` file must point at the requested, currently absent
    /// Git directory. The complete worktree is captured under a fresh hidden
    /// recovery ref before the remote is configured or fetched.
    pub fn recover_detached_repository(
        &self,
        request: &GitDetachedRecoveryRequest,
    ) -> Result<GitDetachedRecoveryReport, GitEngineError> {
        request.validate()?;
        std::fs::create_dir_all(request.git_dir.parent().ok_or_else(|| {
            GitEngineError::UnsupportedRepository {
                detail: "the detached Git directory must have a parent directory".to_string(),
            }
        })?)?;
        let installation = self.installation()?;
        let mut command = self.command();
        command.args(["init", "--bare"]);
        command.arg(git_cli_path(&request.git_dir));
        ensure_success(
            "initialize the replacement detached Git directory",
            self.execute(command)?,
        )?;

        self.configure_detached_repository(request)?;
        write_gitdir_pointer(&request.work_tree, &request.git_dir)?;
        let repository = self.discover_repository(&request.work_tree)?;
        if repository.layout != GitRepositoryLayout::Detached
            || !same_prospective_path(
                &repository.git_dir,
                &prospective_absolute_path(&request.git_dir)?,
            )
        {
            return Err(GitEngineError::UnsupportedRepository {
                detail: "the recreated repository did not resolve to the registered detached Git directory"
                    .to_string(),
            });
        }

        let recovery_ref = detached_recovery_ref(&Ulid::new().to_string())?;
        let capture = self.capture_worktree(
            &repository,
            &GitCaptureRequest {
                base: None,
                target_ref: recovery_ref.clone(),
                target_before: None,
                message: "vulcan sync recovery: capture materialized worktree after detached Git directory loss\n"
                    .to_string(),
                link: None,
            },
        )?;

        let remote = GitRemote::parse("origin")?;
        let mut command = self.repository_command(&repository);
        command.args(["remote", "add", remote.as_str(), &request.source]);
        let output = self.execute(command)?;
        if !output.status.success() {
            return Err(redact_clone_source(
                command_failed("configure the replacement repository remote", &output),
                &request.source,
            ));
        }
        let mut command = self.repository_command(&repository);
        command.args(["fetch", "--prune", remote.as_str()]);
        let output = self.execute(command)?;
        if !output.status.success() {
            return Err(redact_clone_source(
                command_failed("fetch the replacement repository", &output),
                &request.source,
            ));
        }

        Ok(GitDetachedRecoveryReport {
            installation,
            repository,
            recovery_ref,
            recovery_commit: capture.commit,
            recovery_tree: capture.tree,
            fetched_remote: remote,
            possibly_lost_hidden_ref_namespaces: lost_detached_ref_namespaces(),
        })
    }

    fn configure_detached_repository(
        &self,
        request: &GitDetachedRecoveryRequest,
    ) -> Result<(), GitEngineError> {
        for (key, value) in [
            ("core.bare", OsStr::new("false")),
            ("core.worktree", request.work_tree.as_os_str()),
        ] {
            let mut command = self.command();
            command
                .arg("--git-dir")
                .arg(git_cli_path(&request.git_dir))
                .args(["config", key])
                .arg(if key == "core.worktree" {
                    git_cli_path(Path::new(value))
                } else {
                    value.to_os_string()
                });
            ensure_success(
                "configure the replacement detached repository",
                self.execute(command)?,
            )?;
        }
        for setting in request.platform.policy().clone_config {
            let mut command = self.command();
            command
                .arg("--git-dir")
                .arg(git_cli_path(&request.git_dir))
                .args(["config", setting.key, setting.value]);
            ensure_success(
                "configure the replacement detached repository platform policy",
                self.execute(command)?,
            )?;
        }
        Ok(())
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
        if let Some(ssh_command) = &self.ssh_command {
            command
                .env_remove("GIT_SSH")
                .env("GIT_SSH_COMMAND", &**ssh_command);
        }
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
            command.arg("-C").arg(git_cli_path(repository_path));
        }
        command.args(arguments);
        let output = self.execute(command)?;
        ensure_success(operation, output)
    }

    fn execute(&self, mut command: Command) -> Result<Output, GitEngineError> {
        self.execute_command(&mut command, "run a Git command", None)
    }

    fn execute_with_input(
        &self,
        mut command: Command,
        operation: &'static str,
        input: &[u8],
    ) -> Result<Output, GitEngineError> {
        self.execute_command(&mut command, operation, Some(input))
    }

    fn execute_command(
        &self,
        command: &mut Command,
        operation: &'static str,
        input: Option<&[u8]>,
    ) -> Result<Output, GitEngineError> {
        command
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        configure_git_process(command);
        let mut child = crate::process::spawn_retrying_busy(command).map_err(|source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                GitEngineError::ExecutableUnavailable {
                    executable: self.executable.clone(),
                    source,
                }
            } else {
                GitEngineError::Io(source)
            }
        })?;
        self.record_subprocess();
        let mut stdout = child
            .stdout
            .take()
            .expect("piped Git stdout must be available");
        let mut stderr = child
            .stderr
            .take()
            .expect("piped Git stderr must be available");
        let stdin = input
            .map(|_| {
                child
                    .stdin
                    .take()
                    .ok_or_else(|| GitEngineError::InvalidOutput {
                        operation,
                        detail: "Git command did not expose stdin".to_string(),
                    })
            })
            .transpose()?;

        std::thread::scope(|scope| {
            let stdout_reader = scope.spawn(move || {
                let mut bytes = Vec::new();
                stdout.read_to_end(&mut bytes).map(|_| bytes)
            });
            let stderr_reader = scope.spawn(move || {
                let mut bytes = Vec::new();
                stderr.read_to_end(&mut bytes).map(|_| bytes)
            });
            let stdin_writer = input
                .zip(stdin)
                .map(|(input, mut stdin)| scope.spawn(move || stdin.write_all(input)));
            let (status, timed_out) =
                if let Some(status) = child.wait_timeout(self.command_timeout)? {
                    (status, false)
                } else {
                    terminate_process_group(&mut child);
                    (child.wait()?, true)
                };
            let stdout = join_reader(stdout_reader, operation, "stdout")?;
            let stderr = join_reader(stderr_reader, operation, "stderr")?;
            if let Some(writer) = stdin_writer {
                let write_result = writer.join().map_err(|_| GitEngineError::InvalidOutput {
                    operation,
                    detail: "Git stdin writer panicked".to_string(),
                })?;
                if status.success() {
                    write_result?;
                }
            }
            if timed_out {
                return Err(GitEngineError::CommandTimedOut {
                    operation,
                    timeout: self.command_timeout,
                });
            }
            Ok(Output {
                status,
                stdout,
                stderr: self.redact_stderr(stderr),
            })
        })
    }

    fn read_batch_blobs_with_limits(
        &self,
        command: Command,
        input: &[u8],
        expected: &[GitOid],
        per_blob_limit: usize,
        total_limit: usize,
    ) -> Result<BTreeMap<GitOid, Vec<u8>>, GitEngineError> {
        let mut blobs = BTreeMap::new();
        self.visit_batch_blobs_with_limits(
            command,
            input,
            expected,
            (per_blob_limit, total_limit),
            &mut |oid, data| {
                blobs.insert(oid.clone(), data);
                Ok(())
            },
        )?;
        Ok(blobs)
    }

    fn visit_batch_blobs_with_limits(
        &self,
        command: Command,
        input: &[u8],
        expected: &[GitOid],
        limits: (usize, usize),
        visitor: &mut GitBlobVisitor<'_>,
    ) -> Result<(), GitEngineError> {
        self.visit_batch_objects_with_limits(command, input, expected, "blob", limits, visitor)
    }

    /// Streams `cat-file --batch` objects of type `kind` in `expected` order.
    fn visit_batch_objects_with_limits(
        &self,
        mut command: Command,
        input: &[u8],
        expected: &[GitOid],
        kind: &str,
        limits: (usize, usize),
        visitor: &mut GitBlobVisitor<'_>,
    ) -> Result<(), GitEngineError> {
        const OPERATION: &str = "read Git blobs in a batch";
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        configure_git_process(&mut command);
        let mut child = crate::process::spawn_retrying_busy(&mut command).map_err(|source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                GitEngineError::ExecutableUnavailable {
                    executable: self.executable.clone(),
                    source,
                }
            } else {
                GitEngineError::Io(source)
            }
        })?;
        self.record_subprocess();
        let stdout = child
            .stdout
            .take()
            .expect("piped Git stdout must be available");
        let mut stderr = child
            .stderr
            .take()
            .expect("piped Git stderr must be available");
        let mut stdin = child
            .stdin
            .take()
            .expect("piped Git stdin must be available");
        std::thread::scope(|scope| {
            let stdout_reader = scope.spawn(move || {
                visit_batch_objects_reader(
                    expected,
                    BufReader::new(stdout),
                    kind,
                    limits.0,
                    limits.1,
                    visitor,
                )
            });
            let stderr_reader = scope.spawn(move || {
                let mut bytes = Vec::new();
                (&mut stderr)
                    .take(MAX_ERROR_BYTES as u64)
                    .read_to_end(&mut bytes)?;
                std::io::copy(&mut stderr, &mut std::io::sink())?;
                Ok(bytes)
            });
            let stdin_writer = scope.spawn(move || stdin.write_all(input));
            let (status, timed_out) =
                if let Some(status) = child.wait_timeout(self.command_timeout)? {
                    (status, false)
                } else {
                    terminate_process_group(&mut child);
                    (child.wait()?, true)
                };
            let blobs = stdout_reader
                .join()
                .map_err(|_| GitEngineError::InvalidOutput {
                    operation: OPERATION,
                    detail: "Git stdout reader panicked".to_string(),
                })?;
            let stderr = join_reader(stderr_reader, OPERATION, "stderr")?;
            let write_result = stdin_writer
                .join()
                .map_err(|_| GitEngineError::InvalidOutput {
                    operation: OPERATION,
                    detail: "Git stdin writer panicked".to_string(),
                })?;
            if timed_out {
                return Err(GitEngineError::CommandTimedOut {
                    operation: OPERATION,
                    timeout: self.command_timeout,
                });
            }
            blobs?;
            if status.success() {
                write_result?;
                Ok(())
            } else {
                Err(command_failed(
                    OPERATION,
                    &Output {
                        status,
                        stdout: Vec::new(),
                        stderr,
                    },
                ))
            }
        })
    }

    fn repository_command(&self, repository: &GitRepository) -> Command {
        let mut command = self.command();
        if let Some(work_tree) = &repository.work_tree {
            // Anchor every repository command at the worktree root. Prefix-
            // relative pathspecs such as `.` and `:(exclude)` otherwise
            // resolve against the caller's working directory, which silently
            // scopes capture and verification to a subdirectory.
            command.current_dir(work_tree);
        }
        command
            .arg("--git-dir")
            .arg(git_cli_path(&repository.git_dir));
        if let Some(work_tree) = &repository.work_tree {
            command.arg("--work-tree").arg(git_cli_path(work_tree));
        }
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

    fn add_worktree_to_index(
        &self,
        repository: &GitRepository,
        index_path: &Path,
        operation: &'static str,
    ) -> Result<(), GitEngineError> {
        // Add the worktree without device-local state, then remove any that a
        // commit tracked from the alternate index below.
        let mut command = self.index_command(repository, index_path)?;
        command.args(["add", "-A", "--", "."]);
        command.args(DEVICE_LOCAL_CAPTURE_EXCLUDES);
        ensure_success(operation, self.execute(command)?)?;
        let mut command = self.index_command(repository, index_path)?;
        command.args(["update-index", "--force-remove", "--"]);
        command.args(DEVICE_LOCAL_VULCAN_PATHS);
        ensure_success(operation, self.execute(command)?)?;
        Ok(())
    }

    /// Tracked paths of the captured `tree` that a `.gitignore` changed since
    /// `base` newly ignores. The capture stops tracking them; every device
    /// keeps its copy (see [`crate::tree_ignore`]).
    fn newly_ignored_in_capture(
        &self,
        repository: &GitRepository,
        base: &GitOid,
        tree: &GitOid,
    ) -> Result<Vec<String>, GitEngineError> {
        let mut command = self.repository_command(repository);
        command.args([
            "diff-tree",
            "-r",
            "-z",
            "--name-only",
            "--no-renames",
            "--diff-filter=AM",
            base.as_str(),
            tree.as_str(),
            "--",
            ":(glob)**/.gitignore",
        ]);
        let output = ensure_success("compare .gitignore files", self.execute(command)?)?;
        let directories = nul_fields("compare .gitignore files", &output.stdout)?
            .into_iter()
            .filter_map(|path| crate::tree_ignore::ignore_file_directory(path).map(str::to_string))
            .collect::<BTreeSet<_>>();
        if directories.is_empty() {
            return Ok(Vec::new());
        }
        let entries = if directories.contains("") {
            self.tree_entries(repository, tree)?
        } else {
            let mut entries = Vec::new();
            for directory in &directories {
                entries.extend(self.tree_entries_under(repository, tree, directory)?);
            }
            entries
        };
        let paths = entries
            .into_iter()
            .filter(|entry| entry.kind == "blob")
            .map(|entry| entry.path)
            .filter(|path| crate::tree_ignore::ignore_file_directory(path).is_none())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        crate::tree_ignore::newly_ignored(self, repository, base, tree, &paths)
    }

    /// One capture pass: stage the worktree, stop tracking newly ignored
    /// paths, and write the tree. `untracked` is computed on the first pass
    /// and reused, so verification passes agree.
    fn capture_pass(
        &self,
        repository: &GitRepository,
        index_path: &Path,
        base: Option<&GitOid>,
        untracked: &mut Option<Vec<String>>,
        operation: &'static str,
    ) -> Result<GitOid, GitEngineError> {
        self.add_worktree_to_index(repository, index_path, operation)?;
        let write_tree = || -> Result<GitOid, GitEngineError> {
            GitOid::parse(
                self.index_capture(repository, index_path, operation, ["write-tree"])?
                    .trim(),
            )
        };
        let tree = write_tree()?;
        let Some(base) = base else {
            return Ok(tree);
        };
        if untracked.is_none() {
            *untracked = Some(self.newly_ignored_in_capture(repository, base, &tree)?);
        }
        let paths = untracked.as_deref().unwrap_or_default();
        if paths.is_empty() {
            return Ok(tree);
        }
        let mut command = self.index_command(repository, index_path)?;
        command.args(["update-index", "--force-remove", "--"]);
        command.args(paths);
        ensure_success(operation, self.execute(command)?)?;
        write_tree()
    }

    fn prepare_sync_index(
        &self,
        repository: &GitRepository,
        index_path: &Path,
        base: Option<&GitOid>,
        operation: &'static str,
    ) -> Result<bool, GitEngineError> {
        std::fs::create_dir_all(
            index_path
                .parent()
                .expect("the sync index path always has a parent"),
        )?;
        let regular_index = std::fs::symlink_metadata(index_path)
            .is_ok_and(|metadata| metadata.file_type().is_file());
        if let Some(base) = base.filter(|_| regular_index) {
            let mut command = self.index_command(repository, index_path)?;
            command
                .args(["diff-index", "--cached", "--quiet"])
                .arg(base.as_str())
                .arg("--");
            if self.execute(command)?.status.success() {
                return Ok(true);
            }
        }

        // Reset a surviving index to the base instead of rebuilding it: a
        // single-tree `read-tree --reset` keeps the cached stat data of
        // entries that match, so the capture's `git add -A` re-reads only
        // files that changed. Rebuilding would run every file through the
        // clean filters (Git LFS) again on each cycle with uncommitted edits.
        if let Some(base) = base.filter(|_| regular_index) {
            let mut command = self.index_command(repository, index_path)?;
            command.args(["read-tree", "--reset"]).arg(base.as_str());
            if self.execute(command)?.status.success() {
                return Ok(false);
            }
        }
        remove_file_if_present(index_path)?;
        let arguments = match base {
            Some(base) => vec![OsString::from("read-tree"), OsString::from(base.as_str())],
            None => vec![OsString::from("read-tree"), OsString::from("--empty")],
        };
        self.index_output(repository, index_path, operation, arguments)?;
        Ok(false)
    }

    fn patch_command(
        &self,
        repository: &GitRepository,
        index_path: &Path,
        operation: &'static str,
        arguments: &[&str],
        patch: &[u8],
    ) -> Result<Output, GitEngineError> {
        let mut command = self.index_command(repository, index_path)?;
        command.args(arguments);
        ensure_success(
            operation,
            self.execute_with_input(command, operation, patch)?,
        )
    }

    fn path_object_metadata(
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
        let output = ensure_success("inspect a Git path object", output)?;
        if output.stdout.is_empty() {
            return Ok(None);
        }
        if output.stdout.last() != Some(&0) {
            return Err(GitEngineError::InvalidOutput {
                operation: "inspect a Git path object",
                detail: "expected a NUL-terminated ls-tree record".to_string(),
            });
        }
        let record =
            std::str::from_utf8(&output.stdout[..output.stdout.len() - 1]).map_err(|error| {
                GitEngineError::InvalidOutput {
                    operation: "inspect a Git path object",
                    detail: error.to_string(),
                }
            })?;
        if record.contains('\0') {
            return Err(GitEngineError::InvalidOutput {
                operation: "inspect a Git path object",
                detail: "the exact path query returned multiple objects".to_string(),
            });
        }
        let (metadata, found_path) =
            record
                .split_once('\t')
                .ok_or_else(|| GitEngineError::InvalidOutput {
                    operation: "inspect a Git path object",
                    detail: "expected `<mode> <type> <oid>\\t<path>`".to_string(),
                })?;
        if found_path != path {
            return Err(GitEngineError::InvalidOutput {
                operation: "inspect a Git path object",
                detail: format!("expected path `{path}`, received `{found_path}`"),
            });
        }
        let mut metadata = metadata.split_whitespace();
        let mode = metadata.next().unwrap_or_default();
        let kind = metadata.next().unwrap_or_default();
        let oid = metadata.next().unwrap_or_default();
        if mode.is_empty() || kind.is_empty() || oid.is_empty() || metadata.next().is_some() {
            return Err(GitEngineError::InvalidOutput {
                operation: "inspect a Git path object",
                detail: "invalid ls-tree object metadata".to_string(),
            });
        }
        Ok(Some(GitPathObject {
            oid: GitOid::parse(oid)?,
            mode: mode.to_string(),
            kind: kind.to_string(),
            data: None,
        }))
    }

    fn commit_tree(
        &self,
        repository: &GitRepository,
        tree: &GitOid,
        parents: &[GitOid],
        message: &str,
    ) -> Result<GitOid, GitEngineError> {
        self.commit_tree_with_reproducible_identity(repository, tree, parents, message, false)
    }

    fn snapshot_worktree_tree_inner(
        &self,
        repository: &GitRepository,
        base: Option<&GitOid>,
    ) -> Result<GitOid, GitEngineError> {
        self.capture_worktree_tree_inner(repository, base, false)
            .map(|capture| capture.tree)
    }

    fn capture_worktree_tree_inner(
        &self,
        repository: &GitRepository,
        base: Option<&GitOid>,
        reuse_clean_base: bool,
    ) -> Result<GitWorktreeTreeCapture, GitEngineError> {
        repository.require_work_tree()?;
        let index_path = repository.sync_index();
        let reused =
            self.prepare_sync_index(repository, &index_path, base, "seed the sync index")?;
        if reuse_clean_base
            && reused
            && self.prepared_index_matches_worktree(repository, &index_path)?
        {
            let base = base.expect("a reusable sync index has a base revision");
            return Ok(GitWorktreeTreeCapture {
                tree: self.tree_oid(repository, base)?,
                reused_base: true,
            });
        }
        let mut untracked = None;
        for _ in 0..3 {
            let first = self.capture_pass(
                repository,
                &index_path,
                base,
                &mut untracked,
                "capture the working tree",
            )?;
            let second = self.capture_pass(
                repository,
                &index_path,
                base,
                &mut untracked,
                "verify the working-tree capture",
            )?;
            if first == second {
                return Ok(GitWorktreeTreeCapture {
                    tree: second,
                    reused_base: false,
                });
            }
        }
        Err(GitEngineError::WorktreeChanged)
    }

    fn prepared_index_matches_worktree(
        &self,
        repository: &GitRepository,
        index_path: &Path,
    ) -> Result<bool, GitEngineError> {
        self.heal_racy_sync_index(repository, index_path)?;
        let pathspecs = WORKTREE_CAPTURE_PATHS
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        let mut command = self.index_command(repository, index_path)?;
        command.args(["diff-files", "--quiet", "--"]);
        command.args(&pathspecs);
        let tracked = self.execute(command)?;
        if !tracked.status.success() {
            if tracked.status.code() == Some(1) {
                return Ok(false);
            }
            return Err(command_failed("compare the working tree", &tracked));
        }
        let mut command = self.index_command(repository, index_path)?;
        command.args(["ls-files", "--others", "--exclude-standard", "--"]);
        command.args(&pathspecs);
        let output = ensure_success("list untracked worktree paths", self.execute(command)?)?;
        Ok(
            decode_stdout("list untracked worktree paths", output.stdout)?
                .trim()
                .is_empty(),
        )
    }

    /// Rewrites the sync index with fresh stat data when some entry is racily
    /// clean, so later comparisons stop re-reading and re-filtering those files.
    /// Only stat data changes; entries whose content differs stay dirty.
    fn heal_racy_sync_index(
        &self,
        repository: &GitRepository,
        index_path: &Path,
    ) -> Result<(), GitEngineError> {
        let hash_len = match repository.object_format {
            GitObjectFormat::Sha1 => 20,
            GitObjectFormat::Sha256 => 32,
            GitObjectFormat::Other(_) => return Ok(()),
        };
        if crate::racy_index::may_have_racy_entries(index_path, hash_len) {
            let mut command = self.index_command(repository, index_path)?;
            command.args(["update-index", "-q", "--refresh"]);
            ensure_success(
                "refresh racily clean sync index entries",
                self.execute(command)?,
            )?;
        }
        Ok(())
    }

    /// All best common ancestors of `left` and `right`, in Git's order.
    fn merge_bases(
        &self,
        repository: &GitRepository,
        left: &GitOid,
        right: &GitOid,
    ) -> Result<Vec<GitOid>, GitEngineError> {
        let mut command = self.repository_command(repository);
        command
            .args(["merge-base", "--all"])
            .arg(left.as_str())
            .arg(right.as_str());
        let output = self.execute(command)?;
        match output.status.code() {
            Some(0) => decode_stdout("find the merge base", output.stdout)?
                .lines()
                .filter(|line| !line.trim().is_empty())
                .map(GitOid::parse)
                .collect(),
            Some(1) => Ok(Vec::new()),
            _ => Err(command_failed("find the merge base", &output)),
        }
    }

    /// The single base a three-way merge of `left` and `right` is measured
    /// against. Criss-crossed history has several best common ancestors;
    /// like Git's own recursive strategy, they are folded into one virtual
    /// base commit whose tree merges them, keeping any conflict markers.
    /// Bases are folded in sorted order into reproducible commits, so the
    /// virtual base, and every conflict identity derived from it, is the same
    /// on every device and for either merge direction.
    fn virtual_merge_base(
        &self,
        repository: &GitRepository,
        left: &GitOid,
        right: &GitOid,
    ) -> Result<Option<GitOid>, GitEngineError> {
        let mut bases = self.merge_bases(repository, left, right)?;
        bases.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        let mut bases = bases.into_iter();
        let Some(mut virtual_base) = bases.next() else {
            return Ok(None);
        };
        for base in bases {
            let mut command = self.repository_command(repository);
            command
                .args([
                    "merge-tree",
                    "--write-tree",
                    "--no-messages",
                    "--allow-unrelated-histories",
                ])
                .arg(virtual_base.as_str())
                .arg(base.as_str());
            let output = self.execute(command)?;
            if !output.status.success() && output.status.code() != Some(1) {
                return Err(command_failed("merge criss-crossed merge bases", &output));
            }
            let stdout = decode_stdout("merge criss-crossed merge bases", output.stdout)?;
            let tree = stdout
                .lines()
                .next()
                .ok_or_else(|| GitEngineError::InvalidOutput {
                    operation: "merge criss-crossed merge bases",
                    detail: "Git did not report a merged tree".to_string(),
                })
                .and_then(|line| GitOid::parse(line.trim()))?;
            virtual_base = self.commit_tree_with_reproducible_identity(
                repository,
                &tree,
                &[virtual_base.clone(), base],
                "vulcan virtual merge base\n",
                true,
            )?;
        }
        Ok(Some(virtual_base))
    }

    fn commit_tree_with_reproducible_identity(
        &self,
        repository: &GitRepository,
        tree: &GitOid,
        parents: &[GitOid],
        message: &str,
        reproducible: bool,
    ) -> Result<GitOid, GitEngineError> {
        // A fixed Vulcan Sync author/committer for device-owned sync state.
        let mut command = self.repository_command(repository);
        command.arg("commit-tree").arg(tree.as_str());
        for parent in parents {
            command.arg("-p").arg(parent.as_str());
        }
        command
            .env("GIT_AUTHOR_NAME", "Vulcan Sync")
            .env("GIT_AUTHOR_EMAIL", "sync@vulcan.invalid")
            .env("GIT_COMMITTER_NAME", "Vulcan Sync")
            .env("GIT_COMMITTER_EMAIL", "sync@vulcan.invalid");
        if reproducible {
            command
                .env("GIT_AUTHOR_DATE", "@0 +0000")
                .env("GIT_COMMITTER_DATE", "@0 +0000");
        }
        let output = self.execute_with_input(command, "create a commit", message.as_bytes())?;
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

    /// Reads branch and repository pull-strategy configuration in one process.
    /// Keys are matched exactly in code, so branch names with pattern
    /// characters need no escaping.
    fn git_pull_config_map(
        &self,
        repository: &GitRepository,
    ) -> Result<BTreeMap<String, String>, GitEngineError> {
        let mut command = self.repository_command(repository);
        command.args([
            "config",
            "--get-regexp",
            r"^(branch\..+\.(remote|merge|rebase)|pull\.(ff|rebase))$",
        ]);
        let output = self.execute(command)?;
        if !output.status.success() && output.status.code() != Some(1) {
            return Err(command_failed("read Git configuration", &output));
        }
        let stdout = decode_stdout("read Git configuration", output.stdout)?;
        let mut values = BTreeMap::new();
        for line in stdout.lines() {
            if let Some((key, value)) = line.split_once(' ') {
                values.insert(key.to_string(), value.to_string());
            }
        }
        Ok(values)
    }
}

/// The vault root's `/`-terminated path inside the work tree, or empty when
/// the vault is the work tree root or cannot be placed inside it.
/// The vault root's `/`-terminated path inside the repository work tree,
/// or empty when the vault is the work tree root.
#[must_use]
pub fn vault_path_prefix(repository: &GitRepository, vault_root: &Path) -> String {
    vault_work_tree_prefix(repository, vault_root)
}

fn vault_work_tree_prefix(repository: &GitRepository, vault_root: &Path) -> String {
    let Some(work_tree) = repository.work_tree.as_deref() else {
        return String::new();
    };
    let canonical =
        |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let (work_tree, vault_root) = (canonical(work_tree), canonical(vault_root));
    let Ok(relative) = vault_root.strip_prefix(&work_tree) else {
        return String::new();
    };
    let mut prefix = String::new();
    for component in relative.components() {
        let Some(component) = component.as_os_str().to_str() else {
            return String::new();
        };
        prefix.push_str(component);
        prefix.push('/');
    }
    prefix
}

/// Git directory markers for an in-progress operation, shared by the safety
/// state and the branch lane gate so both recognize the same states.
pub(crate) const GIT_OPERATION_MARKERS: &[(&str, &str)] = &[
    ("MERGE_HEAD", "merge"),
    ("CHERRY_PICK_HEAD", "cherry-pick"),
    ("REVERT_HEAD", "revert"),
    ("BISECT_LOG", "bisect"),
    ("rebase-merge", "rebase"),
    ("rebase-apply", "rebase"),
];

impl GitEngine for GitCliEngine {
    fn kind(&self) -> GitEngineKind {
        GitEngineKind::Cli
    }

    fn subprocess_count(&self) -> Option<u64> {
        Some(self.observed_subprocess_count())
    }

    fn installation(&self) -> Result<GitInstallation, GitEngineError> {
        if let Some(installation) = self.installation.get() {
            return Ok(installation.clone());
        }
        let stdout = self.capture("inspect Git installation", None, ["--version"])?;
        let installation = GitInstallation {
            engine: self.kind(),
            executable: self.executable.clone(),
            version: GitVersion::parse(&stdout)?,
        };
        let _ = self.installation.set(installation.clone());
        Ok(installation)
    }

    fn discover_repository(&self, path: &Path) -> Result<GitRepository, GitEngineError> {
        let properties = self.capture(
            "discover the repository Git directory",
            Some(path),
            [
                "rev-parse",
                "--path-format=absolute",
                "--absolute-git-dir",
                "--git-common-dir",
                "--is-bare-repository",
                "--show-object-format",
            ],
        )?;
        let mut properties = properties.lines();
        let git_dir = absolute_path(properties.next().unwrap_or_default().to_string())?;
        let common_dir = absolute_path(properties.next().unwrap_or_default().to_string())?;
        let is_bare = parse_git_bool(
            properties.next().unwrap_or_default(),
            "determine whether the repository is bare",
        )?;
        let object_format = match properties.next().unwrap_or_default() {
            "sha1" => GitObjectFormat::Sha1,
            "sha256" => GitObjectFormat::Sha256,
            other => GitObjectFormat::Other(other.to_string()),
        };
        if properties.next().is_some() {
            return Err(GitEngineError::InvalidOutput {
                operation: "discover repository paths",
                detail: "expected exactly two paths, a bare flag, and an object format".to_string(),
            });
        }

        let work_tree = if is_bare {
            None
        } else {
            Some(canonical_work_tree(self.rev_parse(
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
            command.arg("--separate-git-dir").arg(git_cli_path(git_dir));
        }
        command
            .arg("--")
            .arg(&request.source)
            .arg(git_cli_path(&request.work_tree));
        let output = self.execute(command)?;
        if !output.status.success() {
            return Err(redact_clone_source(
                command_failed("clone Git repository", &output),
                &request.source,
            ));
        }
        match self.discover_repository(&request.work_tree) {
            Err(GitEngineError::CommandFailed {
                exit_code: Some(128),
                ref stderr,
                ..
            }) if stderr.starts_with("fatal: detected dubious ownership in repository at ") => {
                // Shared filesystems (notably Android storage) can report a different
                // owner even for a checkout we just created. Trust only this successful
                // clone, never arbitrary repositories encountered during discovery.
                let work_tree = request
                    .work_tree
                    .canonicalize()
                    .map_err(GitEngineError::Io)?;
                let mut command = self.command();
                // Git interprets a trailing `/*` as a trust wildcard, so it
                // cannot safely represent this unusual literal directory name.
                if work_tree.file_name() == Some(OsStr::new("*")) {
                    return Err(GitEngineError::UnsupportedRepository {
                        detail: "cannot automatically trust a worktree named `*`; choose a different clone destination".to_string(),
                    });
                }
                command
                    .args(["config", "--global", "--add", "safe.directory"])
                    .arg(git_cli_path(&work_tree));
                ensure_success("trust the cloned Git worktree", self.execute(command)?)?;
                self.discover_repository(&request.work_tree)
            }
            result => result,
        }
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
            let mut raw_command = self.repository_command(repository);
            raw_command
                .args(["rev-parse", "--verify", "--quiet"])
                .arg(reference.as_str());
            let raw_output = self.execute(raw_command)?;
            if raw_output.status.success() {
                let target = decode_stdout("inspect an unreadable Git ref", raw_output.stdout)?;
                return Err(GitEngineError::InvalidOutput {
                    operation: "read a Git ref",
                    detail: format!(
                        "{} exists at {} but does not resolve to a readable commit object",
                        reference.as_str(),
                        target.trim()
                    ),
                });
            }
            if raw_output.status.code() == Some(1)
                && raw_output.stdout.is_empty()
                && raw_output.stderr.is_empty()
            {
                return Ok(None);
            }
            return Err(command_failed("inspect an unreadable Git ref", &raw_output));
        }
        Err(command_failed("read a Git ref", &output))
    }

    fn list_refs(
        &self,
        repository: &GitRepository,
        prefix: &GitRefName,
    ) -> Result<Vec<GitReference>, GitEngineError> {
        let mut command = self.repository_command(repository);
        command
            .args(["for-each-ref", "--format=%(refname)%00%(objectname)%00"])
            .arg(prefix.as_str());
        let output = ensure_success("list Git refs", self.execute(command)?)?;
        parse_reference_list(&output.stdout)
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

    fn head_reference(
        &self,
        repository: &GitRepository,
    ) -> Result<Option<GitRefName>, GitEngineError> {
        let mut command = self.repository_command(repository);
        command.args(["symbolic-ref", "--quiet", "HEAD"]);
        let output = self.execute(command)?;
        if output.status.success() {
            return GitRefName::parse(decode_stdout("read the HEAD ref", output.stdout)?.trim())
                .map(Some);
        }
        if output.status.code() == Some(1) && output.stdout.is_empty() && output.stderr.is_empty() {
            return Ok(None);
        }
        Err(command_failed("read the HEAD ref", &output))
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
        if matches!(self.read_ref(repository, reference), Ok(Some(_))) {
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
        if let Ok(current) = self.read_ref(repository, reference) {
            let still_expected = match (current.as_ref(), expected) {
                (Some(current), Some(expected)) => current == expected,
                (None, None) => true,
                _ => false,
            };
            if !still_expected {
                return Ok(GitRefUpdateResult::Stale);
            }
        }
        Err(command_failed("compare and swap a local Git ref", &output))
    }

    fn delete_ref(
        &self,
        repository: &GitRepository,
        reference: &GitRefName,
        expected: &GitOid,
    ) -> Result<GitRefDeleteResult, GitEngineError> {
        match self.read_ref(repository, reference)? {
            None => return Ok(GitRefDeleteResult::Missing),
            Some(current) if current != *expected => return Ok(GitRefDeleteResult::Stale),
            Some(_) => {}
        }
        let mut command = self.repository_command(repository);
        command
            .args(["update-ref", "-d", reference.as_str()])
            .arg(expected.as_str());
        let output = self.execute(command)?;
        if output.status.success() {
            return Ok(GitRefDeleteResult::Deleted);
        }
        if self
            .read_ref(repository, reference)
            .is_ok_and(|current| current.as_ref().is_none_or(|current| current != expected))
        {
            return Ok(GitRefDeleteResult::Stale);
        }
        Err(command_failed("delete a local Git ref with lease", &output))
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
        let Some(mut object) = self.path_object_metadata(repository, commit, path)? else {
            return Ok(None);
        };
        object.data = if object.kind == "blob" {
            Some(
                self.read_blobs_bounded(
                    repository,
                    std::slice::from_ref(&object.oid),
                    MAX_CONFLICT_BLOB_BYTES,
                    MAX_CONFLICT_BLOB_BYTES,
                )?
                .remove(&object.oid)
                .ok_or_else(|| GitEngineError::InvalidOutput {
                    operation: "read a conflicted blob",
                    detail: format!("batch response omitted blob `{}`", object.oid),
                })?,
            )
        } else {
            None
        };
        Ok(Some(object))
    }

    fn read_blobs(
        &self,
        repository: &GitRepository,
        objects: &[GitOid],
    ) -> Result<BTreeMap<GitOid, Vec<u8>>, GitEngineError> {
        self.read_blobs_bounded(
            repository,
            objects,
            MAX_CONFLICT_BLOB_BYTES,
            MAX_CONFLICT_BLOB_BATCH_BYTES,
        )
    }

    fn read_blobs_bounded(
        &self,
        repository: &GitRepository,
        objects: &[GitOid],
        per_blob_limit: usize,
        total_limit: usize,
    ) -> Result<BTreeMap<GitOid, Vec<u8>>, GitEngineError> {
        let objects = objects.iter().cloned().collect::<BTreeSet<_>>();
        if objects.is_empty() {
            return Ok(BTreeMap::new());
        }
        let mut input = String::new();
        for object in &objects {
            writeln!(input, "{object}").expect("writing to a String cannot fail");
        }
        let mut command = self.repository_command(repository);
        command.args(["cat-file", "--batch"]);
        self.read_batch_blobs_with_limits(
            command,
            input.as_bytes(),
            &objects.into_iter().collect::<Vec<_>>(),
            per_blob_limit,
            total_limit,
        )
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

    fn tree_entries(
        &self,
        repository: &GitRepository,
        revision: &GitOid,
    ) -> Result<Vec<GitTreeEntry>, GitEngineError> {
        let output = self.repository_output(
            repository,
            "list Git tree entries",
            ["ls-tree", "-r", "-z", revision.as_str()],
        )?;
        parse_tree_entries(&output.stdout)
    }

    fn tree_with_entries(
        &self,
        repository: &GitRepository,
        base: Option<&GitOid>,
        entries: &[GitTreeEntry],
    ) -> Result<GitOid, GitEngineError> {
        let scratch = tempfile::tempdir_in(&repository.git_dir)?;
        let index_path = scratch.path().join("index");
        match base {
            Some(base) => self.index_output(
                repository,
                &index_path,
                "seed a tree",
                ["read-tree", base.as_str()],
            )?,
            None => self.index_output(
                repository,
                &index_path,
                "seed an empty tree",
                ["read-tree", "--empty"],
            )?,
        };
        if !entries.is_empty() {
            let mut input = Vec::new();
            for entry in entries {
                validate_repository_path(&entry.path)?;
                input.extend_from_slice(
                    format!("{} {}\t", entry.mode, entry.oid.as_str()).as_bytes(),
                );
                input.extend_from_slice(entry.path.as_bytes());
                input.push(0);
            }
            let mut command = self.index_command(repository, &index_path)?;
            command.args(["update-index", "-z", "--index-info"]);
            ensure_success(
                "add entries to a tree",
                self.execute_with_input(command, "add entries to a tree", &input)?,
            )?;
        }
        GitOid::parse(
            self.index_capture(repository, &index_path, "write a tree", ["write-tree"])?
                .trim(),
        )
    }

    fn tree_children(
        &self,
        repository: &GitRepository,
        revision: &GitOid,
    ) -> Result<Vec<GitTreeEntry>, GitEngineError> {
        let output = self.repository_output(
            repository,
            "list a Git tree's children",
            ["ls-tree", "-z", "--full-tree", revision.as_str()],
        )?;
        parse_tree_entries(&output.stdout)
    }

    fn make_tree(
        &self,
        repository: &GitRepository,
        entries: &[GitTreeEntry],
    ) -> Result<GitOid, GitEngineError> {
        let mut input = Vec::new();
        for entry in entries {
            if entry.path.is_empty() || entry.path.contains('/') || entry.path.contains('\0') {
                return Err(GitEngineError::UnsupportedRepository {
                    detail: format!("`{}` is not a single tree entry name", entry.path),
                });
            }
            input.extend_from_slice(
                format!("{} {} {}\t", entry.mode, entry.kind, entry.oid.as_str()).as_bytes(),
            );
            input.extend_from_slice(entry.path.as_bytes());
            input.push(0);
        }
        let mut command = self.repository_command(repository);
        command.args(["mktree", "-z"]);
        let output = ensure_success(
            "write a tree",
            self.execute_with_input(command, "write a tree", &input)?,
        )?;
        GitOid::parse(decode_stdout("write a tree", output.stdout)?.trim())
    }

    fn resolve_paths(
        &self,
        repository: &GitRepository,
        revision: &GitOid,
        paths: &[String],
    ) -> Result<Vec<Option<GitOid>>, GitEngineError> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        let mut input = Vec::new();
        for path in paths {
            validate_repository_path(path)?;
            input.extend_from_slice(format!("{}:{path}\n", revision.as_str()).as_bytes());
        }
        let mut command = self.repository_command(repository);
        command.args(["cat-file", "--batch-check=%(objectname)"]);
        let output = ensure_success(
            "resolve tree paths",
            self.execute_with_input(command, "resolve tree paths", &input)?,
        )?;
        let text = decode_stdout("resolve tree paths", output.stdout)?;
        let resolved = text
            .lines()
            .map(|line| {
                if line.ends_with(" missing") {
                    Ok(None)
                } else {
                    GitOid::parse(line.trim()).map(Some)
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        if resolved.len() != paths.len() {
            return Err(GitEngineError::InvalidOutput {
                operation: "resolve tree paths",
                detail: "Git answered a different number of paths".to_string(),
            });
        }
        Ok(resolved)
    }

    fn tree_entries_under(
        &self,
        repository: &GitRepository,
        revision: &GitOid,
        prefix: &str,
    ) -> Result<Vec<GitTreeEntry>, GitEngineError> {
        validate_repository_path(prefix)?;
        let mut command = self.repository_command(repository);
        command.env("GIT_LITERAL_PATHSPECS", "1").args([
            "ls-tree",
            "-r",
            "-z",
            "--full-tree",
            revision.as_str(),
            "--",
            prefix,
        ]);
        let output = ensure_success("list a Git subtree", self.execute(command)?)?;
        let directory = format!("{prefix}/");
        Ok(parse_tree_entries(&output.stdout)?
            .into_iter()
            .filter(|entry| entry.path.starts_with(&directory))
            .collect())
    }

    fn tree_entries_for_paths(
        &self,
        repository: &GitRepository,
        revision: &GitOid,
        paths: &[String],
    ) -> Result<Vec<GitTreeEntry>, GitEngineError> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        let selected = paths.iter().map(String::as_str).collect::<BTreeSet<_>>();
        // Past this many processes one full listing is cheaper.
        if paths.len() > SCOPED_TREE_LISTING_CHUNK * SCOPED_TREE_LISTING_MAX_CHUNKS {
            return Ok(self
                .tree_entries(repository, revision)?
                .into_iter()
                .filter(|entry| selected.contains(entry.path.as_str()))
                .collect());
        }
        let mut entries = Vec::with_capacity(paths.len());
        for chunk in paths.chunks(SCOPED_TREE_LISTING_CHUNK) {
            let mut command = self.repository_command(repository);
            // Conflict paths are literal names, never patterns.
            command.env("GIT_LITERAL_PATHSPECS", "1").args([
                "ls-tree",
                "-r",
                "-z",
                "--full-tree",
                revision.as_str(),
                "--",
            ]);
            for path in chunk {
                validate_repository_path(path)?;
                command.arg(path);
            }
            let output = ensure_success("list Git tree entries for paths", self.execute(command)?)?;
            // A directory path lists its children; keep exact matches only.
            entries.extend(
                parse_tree_entries(&output.stdout)?
                    .into_iter()
                    .filter(|entry| selected.contains(entry.path.as_str())),
            );
        }
        Ok(entries)
    }

    fn check_patch(
        &self,
        repository: &GitRepository,
        base: &GitOid,
        patch: &[u8],
    ) -> Result<Vec<String>, GitEngineError> {
        if patch.len() > MAX_DIAGNOSTIC_PATH_BYTES {
            return Err(GitEngineError::InvalidOutput {
                operation: "validate a supplied Git patch",
                detail: format!("patch exceeds the {MAX_DIAGNOSTIC_PATH_BYTES} byte limit"),
            });
        }
        reject_unsupported_patch_operations(patch)?;
        let temporary = TempDir::new()?;
        let index = temporary.path().join("index");
        self.index_output(
            repository,
            &index,
            "seed a supplied patch index",
            ["read-tree", base.as_str()],
        )?;
        self.patch_command(
            repository,
            &index,
            "validate a supplied Git patch",
            &["apply", "--cached", "--check", "--whitespace=nowarn", "-"],
            patch,
        )?;
        let output = self.patch_command(
            repository,
            &index,
            "inspect supplied Git patch paths",
            &["apply", "--numstat", "-z", "--whitespace=nowarn", "-"],
            patch,
        )?;
        parse_patch_paths(&output.stdout)
    }

    fn capture_worktree(
        &self,
        repository: &GitRepository,
        request: &GitCaptureRequest,
    ) -> Result<GitCapture, GitEngineError> {
        let reuse_clean_base = request.base.is_some() && request.base == request.target_before;
        let tree_capture =
            self.capture_worktree_tree_inner(repository, request.base.as_ref(), reuse_clean_base)?;
        let tree = tree_capture.tree;

        if let Some(base) = request.base.as_ref().filter(|_| request.link.is_none()) {
            if tree_capture.reused_base || self.tree_oid(repository, base)? == tree {
                if request.target_before.as_ref() != Some(base) {
                    self.update_ref(repository, &request.target_ref, base)?;
                }
                return Ok(GitCapture {
                    commit: base.clone(),
                    tree,
                    created: false,
                });
            }
        }

        let parents = request
            .base
            .iter()
            .chain(&request.link)
            .cloned()
            .collect::<Vec<_>>();
        let commit = self.commit_tree(repository, &tree, &parents, &request.message)?;
        self.update_ref(repository, &request.target_ref, &commit)?;
        Ok(GitCapture {
            commit,
            tree,
            created: true,
        })
    }

    fn snapshot_worktree_tree(
        &self,
        repository: &GitRepository,
        base: Option<&GitOid>,
    ) -> Result<GitOid, GitEngineError> {
        self.snapshot_worktree_tree_inner(repository, base)
    }

    fn worktree_matches_tree(
        &self,
        repository: &GitRepository,
        expected: &GitOid,
    ) -> Result<bool, GitEngineError> {
        repository.require_work_tree()?;
        let index_path = repository.sync_index();
        let reused = self.prepare_sync_index(
            repository,
            &index_path,
            Some(expected),
            "seed the worktree comparison index",
        )?;

        if !reused {
            let mut command = self.index_command(repository, &index_path)?;
            command.arg("update-index").arg("--refresh");
            let refreshed = self.execute(command)?;
            if !refreshed.status.success() {
                if refreshed.status.code() == Some(1) {
                    return Ok(false);
                }
                return Err(command_failed(
                    "refresh the worktree comparison",
                    &refreshed,
                ));
            }
        }

        self.prepared_index_matches_worktree(repository, &index_path)
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

    fn list_remote_refs(
        &self,
        repository: &GitRepository,
        remote: &GitRemote,
        prefix: &GitRefName,
    ) -> Result<Vec<GitReference>, GitEngineError> {
        let pattern = format!("{}/*", prefix.as_str().trim_end_matches('/'));
        let mut command = self.repository_command(repository);
        command
            .args(["ls-remote", "--refs", "--"])
            .arg(remote.as_str())
            .arg(pattern);
        let output = ensure_success("list remote Git refs", self.execute(command)?)?;
        parse_remote_reference_list(&output.stdout, prefix)
    }

    fn remote_refs(
        &self,
        repository: &GitRepository,
        remote: &GitRemote,
        references: &[&GitRefName],
        prefix: Option<&GitRefName>,
    ) -> Result<GitRemoteRefs, GitEngineError> {
        const OPERATION: &str = "query remote Git refs";
        let mut command = self.repository_command(repository);
        command
            .args(["ls-remote", "--refs", "--"])
            .arg(remote.as_str());
        for reference in references {
            command.arg(reference.as_str());
        }
        let required_prefix =
            prefix.map(|prefix| format!("{}/", prefix.as_str().trim_end_matches('/')));
        if let Some(required_prefix) = &required_prefix {
            command.arg(format!("{required_prefix}*"));
        }
        let output = ensure_success(OPERATION, self.execute(command)?)?;
        let stdout = decode_stdout(OPERATION, output.stdout)?;
        let mut tips: Vec<Option<GitOid>> = vec![None; references.len()];
        let mut seen = BTreeSet::new();
        let mut namespaced = Vec::new();
        for line in stdout.lines().filter(|line| !line.trim().is_empty()) {
            let (oid, name) =
                line.split_once('\t')
                    .ok_or_else(|| GitEngineError::InvalidOutput {
                        operation: OPERATION,
                        detail: format!("expected `<oid>\\t<ref>`, received `{line}`"),
                    })?;
            if references
                .iter()
                .any(|reference| reference.as_str() == name)
            {
                if !seen.insert(name) {
                    return Err(GitEngineError::InvalidOutput {
                        operation: OPERATION,
                        detail: format!("`{name}` was returned more than once"),
                    });
                }
                let oid = GitOid::parse(oid)?;
                for (tip, reference) in tips.iter_mut().zip(references) {
                    if reference.as_str() == name {
                        *tip = Some(oid.clone());
                    }
                }
            } else if required_prefix
                .as_deref()
                .is_some_and(|required| name.starts_with(required))
            {
                namespaced.push(line);
            } else {
                return Err(GitEngineError::InvalidOutput {
                    operation: OPERATION,
                    detail: format!("received unexpected remote ref `{name}`"),
                });
            }
        }
        let listed = prefix.and_then(|prefix| {
            parse_remote_reference_list(namespaced.join("\n").as_bytes(), prefix).ok()
        });
        Ok((tips, listed))
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
            OsString::from("--refmap="),
            OsString::from("--"),
            OsString::from(remote.as_str()),
            OsString::from(refspec),
        ];
        let mut command = self.repository_command(repository);
        command.args(arguments);
        let output = self.execute(command)?;
        if !output.status.success() {
            if matches!(self.remote_ref(repository, remote, source), Ok(None)) {
                return Err(GitEngineError::RemoteRefMissing {
                    remote: remote.clone(),
                    reference: source.clone(),
                });
            }
            return Err(command_failed("fetch the live sync ref", &output));
        }
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

    fn merge_base(
        &self,
        repository: &GitRepository,
        left: &GitOid,
        right: &GitOid,
    ) -> Result<Option<GitOid>, GitEngineError> {
        // Criss-crossed history has several best common ancestors. Every one
        // is a real ancestor of both sides, so callers that only need shared
        // ancestry take the first Git reports; merges synthesize a virtual
        // base instead (see `merge_commits`).
        Ok(self
            .merge_bases(repository, left, right)?
            .into_iter()
            .next())
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
        let base = self.virtual_merge_base(repository, accepted_remote, local_candidate)?;
        Ok(GitMerge {
            clean,
            tree,
            base,
            conflict_paths,
            diagnostics,
        })
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

    fn create_reproducible_commit(
        &self,
        repository: &GitRepository,
        tree: &GitOid,
        parents: &[GitOid],
        message: &str,
    ) -> Result<GitOid, GitEngineError> {
        self.commit_tree_with_reproducible_identity(repository, tree, parents, message, true)
    }

    fn write_blob(
        &self,
        repository: &GitRepository,
        data: &[u8],
    ) -> Result<GitOid, GitEngineError> {
        let mut command = self.repository_command(repository);
        command.args(["hash-object", "-w", "--stdin"]);
        let output = ensure_success(
            "write a Git blob",
            self.execute_with_input(command, "write a Git blob", data)?,
        )?;
        GitOid::parse(decode_stdout("write a Git blob", output.stdout)?.trim())
    }

    fn write_blobs(
        &self,
        repository: &GitRepository,
        data: &[&[u8]],
    ) -> Result<Vec<GitOid>, GitEngineError> {
        if data.is_empty() {
            return Ok(Vec::new());
        }
        let directory = tempfile::tempdir_in(&repository.git_dir)?;
        let mut input = Vec::new();
        for (index, bytes) in data.iter().enumerate() {
            let path = directory.path().join(format!("blob-{index:06}"));
            std::fs::write(&path, bytes)?;
            writeln!(input, "{}", path.display()).expect("writing to a Vec cannot fail");
        }
        let mut command = self.repository_command(repository);
        command.args(["hash-object", "-w", "--stdin-paths"]);
        let output = ensure_success(
            "write Git blobs in a batch",
            self.execute_with_input(command, "write Git blobs in a batch", &input)?,
        )?;
        let stdout = decode_stdout("write Git blobs in a batch", output.stdout)?;
        let oids = stdout
            .lines()
            .map(GitOid::parse)
            .collect::<Result<Vec<_>, _>>()?;
        if oids.len() != data.len() {
            return Err(GitEngineError::InvalidOutput {
                operation: "write Git blobs in a batch",
                detail: format!(
                    "expected {} object IDs but Git returned {}",
                    data.len(),
                    oids.len()
                ),
            });
        }
        Ok(oids)
    }

    fn create_single_file_tree(
        &self,
        repository: &GitRepository,
        path: &str,
        blob: &GitOid,
    ) -> Result<GitOid, GitEngineError> {
        validate_repository_path(path)?;
        let mut command = self.repository_command(repository);
        command.arg("mktree");
        let input = format!("100644 blob {}\t{path}\n", blob.as_str());
        let output = ensure_success(
            "create a single-file tree",
            self.execute_with_input(command, "create a single-file tree", input.as_bytes())?,
        )?;
        GitOid::parse(decode_stdout("create a single-file tree", output.stdout)?.trim())
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
        if self
            .remote_ref(repository, remote, destination)
            .is_ok_and(|current| current.as_ref() != expected)
        {
            return Ok(GitPushResult::Rejected);
        }
        Err(command_failed("push a sync ref", &output))
    }

    fn delete_remote_refs(
        &self,
        repository: &GitRepository,
        remote: &GitRemote,
        deletions: &[(GitRefName, GitOid)],
    ) -> Result<Vec<GitRefDeleteResult>, GitEngineError> {
        if deletions.is_empty() {
            return Ok(Vec::new());
        }
        let mut command = self.repository_command(repository);
        command.args(["push", "--porcelain"]);
        for (reference, expected) in deletions {
            command.arg(format!(
                "--force-with-lease={}:{}",
                reference.as_str(),
                expected.as_str()
            ));
        }
        command.arg("--").arg(remote.as_str());
        for (reference, _) in deletions {
            command.arg(format!(":{}", reference.as_str()));
        }
        let output = self.execute(command)?;
        // Porcelain reports one `<flag>\t<from>:<to>\t<summary>` line per ref:
        // `-` deleted, `!` rejected. Git reports a failed lease and an
        // already-missing ref alike as stale, so both map to `Stale`.
        let stdout = String::from_utf8_lossy(&output.stdout);
        let outcomes = stdout
            .lines()
            .filter_map(|line| {
                let mut fields = line.split('\t');
                let flag = fields.next()?.chars().next()?;
                let target = fields.next()?.rsplit_once(':')?.1;
                let outcome = match flag {
                    '-' => GitRefDeleteResult::Deleted,
                    '!' => GitRefDeleteResult::Stale,
                    _ => return None,
                };
                Some((target.to_string(), outcome))
            })
            .collect::<BTreeMap<_, _>>();
        if !output.status.success() && outcomes.is_empty() {
            return Err(command_failed(
                "delete remote Git refs with leases",
                &output,
            ));
        }
        Ok(deletions
            .iter()
            .map(|(reference, _)| {
                outcomes
                    .get(reference.as_str())
                    .copied()
                    .unwrap_or(GitRefDeleteResult::Missing)
            })
            .collect())
    }

    fn delete_remote_ref(
        &self,
        repository: &GitRepository,
        remote: &GitRemote,
        reference: &GitRefName,
        expected: &GitOid,
    ) -> Result<GitRefDeleteResult, GitEngineError> {
        match self.remote_ref(repository, remote, reference)? {
            None => return Ok(GitRefDeleteResult::Missing),
            Some(current) if current != *expected => return Ok(GitRefDeleteResult::Stale),
            Some(_) => {}
        }
        let lease = format!(
            "--force-with-lease={}:{}",
            reference.as_str(),
            expected.as_str()
        );
        let refspec = format!(":{}", reference.as_str());
        let mut command = self.repository_command(repository);
        command
            .args(["push", "--porcelain"])
            .arg(lease)
            .arg("--")
            .arg(remote.as_str())
            .arg(refspec);
        let output = self.execute(command)?;
        if output.status.success() {
            return Ok(GitRefDeleteResult::Deleted);
        }
        if self
            .remote_ref(repository, remote, reference)
            .is_ok_and(|current| current.as_ref() != Some(expected))
        {
            return Ok(GitRefDeleteResult::Stale);
        }
        Err(command_failed(
            "delete a remote Git ref with lease",
            &output,
        ))
    }

    fn branch_upstream(
        &self,
        repository: &GitRepository,
        branch: &GitRefName,
    ) -> Result<Option<GitBranchUpstream>, GitEngineError> {
        if !branch.as_str().starts_with("refs/heads/") {
            return Ok(None);
        }
        let mut command = self.repository_command(repository);
        command
            .args([
                "for-each-ref",
                "--format=%(upstream:remotename)%00%(upstream)%00%(upstream:remoteref)",
                "--count=1",
                "--",
            ])
            .arg(branch.as_str());
        let output = self.execute(command)?;
        if !output.status.success() {
            return Err(command_failed("resolve the branch upstream", &output));
        }
        let stdout = decode_stdout("resolve the branch upstream", output.stdout)?;
        let record = stdout.strip_suffix('\n').unwrap_or(&stdout);
        let record = record.strip_suffix('\r').unwrap_or(record);
        if record.is_empty() {
            return Ok(None);
        }
        let mut fields = record.split('\0');
        let Some(remote) = fields.next() else {
            return Ok(None);
        };
        let Some(tracking) = fields.next() else {
            return Err(GitEngineError::InvalidOutput {
                operation: "resolve the branch upstream",
                detail: "Git omitted the local tracking ref".to_string(),
            });
        };
        let Some(merge) = fields.next() else {
            return Err(GitEngineError::InvalidOutput {
                operation: "resolve the branch upstream",
                detail: "Git omitted the remote merge ref".to_string(),
            });
        };
        if remote.is_empty() && tracking.is_empty() && merge.is_empty() {
            let short = branch
                .as_str()
                .strip_prefix("refs/heads/")
                .expect("branch prefix was checked above");
            let config = self.git_pull_config_map(repository)?;
            if config.contains_key(&format!("branch.{short}.remote"))
                || config.contains_key(&format!("branch.{short}.merge"))
            {
                return Err(GitEngineError::InvalidOutput {
                    operation: "resolve the branch upstream",
                    detail: "the configured upstream cannot be resolved by Git".to_string(),
                });
            }
            return Ok(None);
        }
        if fields.next().is_some() || remote.is_empty() || tracking.is_empty() || merge.is_empty() {
            return Err(GitEngineError::InvalidOutput {
                operation: "resolve the branch upstream",
                detail: "Git returned an incomplete upstream description".to_string(),
            });
        }
        Ok(Some(GitBranchUpstream {
            branch: branch.clone(),
            remote: GitRemote::parse(remote).map_err(|value| GitEngineError::InvalidOutput {
                operation: "resolve the branch upstream",
                detail: format!("invalid configured remote `{value}`"),
            })?,
            merge_ref: GitRefName::parse(merge).map_err(|value| GitEngineError::InvalidOutput {
                operation: "resolve the branch upstream",
                detail: format!("invalid merge ref `{value}`"),
            })?,
            tracking_ref: GitRefName::parse(tracking).map_err(|value| {
                GitEngineError::InvalidOutput {
                    operation: "resolve the branch upstream",
                    detail: format!("invalid tracking ref `{value}`"),
                }
            })?,
        }))
    }

    fn branch_pull_config(
        &self,
        repository: &GitRepository,
        branch: &GitRefName,
    ) -> Result<BranchPullConfig, GitEngineError> {
        const OPERATION: &str = "resolve the branch pull strategy";
        let short = branch
            .as_str()
            .strip_prefix("refs/heads/")
            .unwrap_or(branch.as_str());
        let config = self.git_pull_config_map(repository)?;
        let rebase = match config
            .get(&format!("branch.{short}.rebase"))
            .map(String::as_str)
        {
            Some(value) => parse_rebase_value(&value.to_ascii_lowercase(), OPERATION)?,
            None => match config.get("pull.rebase").map(String::as_str) {
                Some(value) => parse_rebase_value(&value.to_ascii_lowercase(), OPERATION)?,
                None => PullRebase::Never,
            },
        };
        let fast_forward = match config
            .get("pull.ff")
            .map(String::as_str)
            .map(str::to_ascii_lowercase)
        {
            None => PullFastForward::Always,
            Some(value) => match value.as_str() {
                "false" | "no" | "off" | "0" => PullFastForward::Never,
                "only" => PullFastForward::Only,
                "true" | "yes" | "on" | "1" => PullFastForward::Always,
                _ => {
                    return Err(GitEngineError::InvalidOutput {
                        operation: OPERATION,
                        detail: format!("unsupported pull.ff value `{value}`"),
                    });
                }
            },
        };
        Ok(BranchPullConfig {
            fast_forward,
            rebase,
        })
    }

    fn fast_forward_branch(
        &self,
        repository: &GitRepository,
        branch: &GitRefName,
        target: &GitOid,
    ) -> Result<FastForwardOutcome, GitEngineError> {
        repository.require_work_tree()?;
        if self.head_reference(repository)?.as_ref() != Some(branch) {
            return Err(GitEngineError::UnsupportedRepository {
                detail: "the checked-out branch changed while pulling".to_string(),
            });
        }
        let mut command = self.repository_command(repository);
        command.args(["merge", "--ff-only", target.as_str()]);
        let output = self.execute(command)?;
        if output.status.success() {
            return Ok(
                if bounded_lossy(&output.stdout).contains("Already up to date.") {
                    FastForwardOutcome::UpToDate
                } else {
                    FastForwardOutcome::Advanced
                },
            );
        }
        let combined = format!(
            "{}\n{}",
            bounded_lossy(&output.stdout),
            bounded_lossy(&output.stderr)
        );
        if combined.contains("Not possible to fast-forward") {
            return Ok(FastForwardOutcome::NotFastForwardable);
        }
        if combined.contains("would be overwritten by merge") {
            return Ok(FastForwardOutcome::BlockedDirty);
        }
        Err(command_failed("fast-forward the branch", &output))
    }

    fn merge_branch(
        &self,
        repository: &GitRepository,
        target: &GitOid,
        no_ff: bool,
    ) -> Result<MergeBranchOutcome, GitEngineError> {
        repository.require_work_tree()?;
        let mut command = self.repository_command(repository);
        command.arg("merge").arg("--no-edit");
        if no_ff {
            command.arg("--no-ff");
        }
        command.arg(target.as_str());
        let output = self.execute(command)?;
        if output.status.success() {
            return Ok(
                if bounded_lossy(&output.stdout).contains("Already up to date.") {
                    MergeBranchOutcome::UpToDate
                } else {
                    MergeBranchOutcome::Merged
                },
            );
        }
        let combined = format!(
            "{}\n{}",
            bounded_lossy(&output.stdout),
            bounded_lossy(&output.stderr)
        );
        if repository.git_dir.join("MERGE_HEAD").exists()
            || combined.contains("Automatic merge failed")
            || combined.contains("CONFLICT")
        {
            return Ok(MergeBranchOutcome::Conflicted);
        }
        if combined.contains("would be overwritten by merge")
            || combined.contains("Your local changes")
        {
            return Ok(MergeBranchOutcome::BlockedDirty);
        }
        Err(command_failed("merge the branch", &output))
    }

    fn rebase_hazard(
        &self,
        repository: &GitRepository,
        branch: &GitRefName,
        upstream: &GitOid,
    ) -> Result<Option<RebaseHazard>, GitEngineError> {
        const OPERATION: &str = "check the commits a rebase would rewrite";
        let count = |revisions: &str| -> Result<usize, GitEngineError> {
            let mut command = self.repository_command(repository);
            command.args(["rev-list", "--count", "--stdin"]);
            let output = self.execute_with_input(command, OPERATION, revisions.as_bytes())?;
            if !output.status.success() {
                return Err(command_failed(OPERATION, &output));
            }
            decode_stdout(OPERATION, output.stdout)?
                .trim()
                .parse()
                .map_err(|_| GitEngineError::InvalidOutput {
                    operation: OPERATION,
                    detail: "rev-list did not print a count".to_string(),
                })
        };
        let range = format!("HEAD\n^{}\n", upstream.as_str());
        let total = count(&range)?;
        if total == 0 {
            return Ok(None);
        }
        let mut merges = self.repository_command(repository);
        merges.args(["rev-list", "--merges", "--count"]);
        merges.arg(format!("{}..HEAD", upstream.as_str()));
        let output = self.execute(merges)?;
        if !output.status.success() {
            return Err(command_failed(OPERATION, &output));
        }
        if decode_stdout(OPERATION, output.stdout)?.trim() != "0" {
            return Ok(Some(RebaseHazard::MergeCommits));
        }

        let mut tips: Vec<(String, String)> = Vec::new();
        let mut refs = self.repository_command(repository);
        refs.args([
            "for-each-ref",
            "--format=%(objectname) %(refname)",
            "refs/heads",
            "refs/remotes",
            "refs/tags",
        ]);
        let output = self.execute(refs)?;
        if !output.status.success() {
            return Err(command_failed(OPERATION, &output));
        }
        for line in decode_stdout(OPERATION, output.stdout)?.lines() {
            if let Some((oid, name)) = line.split_once(' ') {
                // Vulcan's own remote namespace (live, device safety heads,
                // registrations) always contains HEAD; it is not a consumer.
                if name != branch.as_str()
                    && !name.ends_with("/HEAD")
                    && !name
                        .split('/')
                        .any(|part| part.starts_with("__vulcan-sync"))
                {
                    tips.push((oid.to_string(), name.to_string()));
                }
            }
        }
        let mut worktrees = self.repository_command(repository);
        worktrees.args(["worktree", "list", "--porcelain"]);
        let output = self.execute(worktrees)?;
        if output.status.success() {
            let own = repository
                .work_tree
                .as_ref()
                .and_then(|path| path.canonicalize().ok());
            let mut path: Option<std::path::PathBuf> = None;
            for line in decode_stdout(OPERATION, output.stdout)?.lines() {
                if let Some(value) = line.strip_prefix("worktree ") {
                    path = Some(std::path::PathBuf::from(value));
                } else if let Some(oid) = line.strip_prefix("HEAD ") {
                    let current = path.as_ref().and_then(|path| path.canonicalize().ok());
                    if current.is_some() && current != own {
                        let label = path
                            .as_ref()
                            .map_or_else(String::new, |path| path.display().to_string());
                        tips.push((oid.to_string(), format!("worktree:{label}")));
                    }
                }
            }
        }
        if tips.is_empty() {
            return Ok(None);
        }
        let excluded = tips.iter().fold(range.clone(), |mut revisions, (oid, _)| {
            revisions.push('^');
            revisions.push_str(oid);
            revisions.push('\n');
            revisions
        });
        if count(&excluded)? == total {
            return Ok(None);
        }
        for (oid, name) in &tips {
            if count(&format!("{range}^{oid}\n"))? < total {
                return Ok(Some(RebaseHazard::SharedWith(name.clone())));
            }
        }
        Ok(Some(RebaseHazard::SharedWith("another ref".to_string())))
    }

    fn prepare_branch_replay(
        &self,
        repository: &GitRepository,
        branch: &GitRefName,
        upstream: &GitOid,
        resolver: &mut dyn BranchReplayResolver,
    ) -> Result<Result<PreparedBranchReplay, String>, GitEngineError> {
        const OPERATION: &str = "replay the branch";
        repository.require_work_tree()?;
        if self.head_reference(repository)?.as_ref() != Some(branch) {
            return Ok(Err("the branch is no longer checked out".to_string()));
        }
        let Some(expected) = self.read_ref(repository, branch)? else {
            return Ok(Err("the branch has no commits".to_string()));
        };
        if let Some(hazard) = self.rebase_hazard(repository, branch, upstream)? {
            return Ok(Err(match hazard {
                RebaseHazard::SharedWith(name) => format!("local commits are also on {name}"),
                RebaseHazard::MergeCommits => "local commits include merges".to_string(),
            }));
        }
        let mut list = self.repository_command(repository);
        list.args(["rev-list", "--reverse"]).arg(format!(
            "{}..{}",
            upstream.as_str(),
            expected.as_str()
        ));
        let output = self.execute(list)?;
        if !output.status.success() {
            return Err(command_failed(OPERATION, &output));
        }
        let commits = decode_stdout(OPERATION, output.stdout)?
            .lines()
            .map(GitOid::parse)
            .collect::<Result<Vec<_>, _>>()?;
        if commits.is_empty() {
            return Ok(Err("no local commits to replay".to_string()));
        }
        if let Some(filter) = self.replay_filtered_path(repository, &expected, upstream)? {
            return Ok(Err(format!(
                "the commits touch paths with the Git `{filter}` filter, which a replay does not run"
            )));
        }
        let signatures_dropped = self.config_flag(repository, "commit.gpgSign")?;

        let scratch = ScratchWorktree::add(self, repository, upstream)?;
        let (to, resolved_paths) =
            match self.replay_in_worktree(&scratch.directory, &commits, resolver)? {
                Ok(replayed) => replayed,
                Err(reason) => return Ok(Err(reason)),
            };
        drop(scratch);
        Ok(Ok(PreparedBranchReplay {
            from: expected,
            to,
            resolved: resolved_paths,
            signatures_dropped,
        }))
    }

    fn apply_branch_replay(
        &self,
        repository: &GitRepository,
        branch: &GitRefName,
        prepared: PreparedBranchReplay,
    ) -> Result<BranchReplayOutcome, GitEngineError> {
        repository.require_work_tree()?;
        Ok(
            match self.move_checked_out_branch(repository, branch, &prepared.from, &prepared.to)? {
                BranchAdvanceOutcome::Advanced => BranchReplayOutcome::Replayed {
                    from: prepared.from,
                    to: prepared.to,
                    resolved: prepared.resolved,
                    signatures_dropped: prepared.signatures_dropped,
                },
                BranchAdvanceOutcome::BlockedStaged => BranchReplayOutcome::Declined {
                    reason: "uncommitted or staged work would be overwritten".to_string(),
                },
                BranchAdvanceOutcome::Stale => BranchReplayOutcome::Declined {
                    reason: "the branch moved during the replay".to_string(),
                },
            },
        )
    }

    fn rebase_branch(
        &self,
        repository: &GitRepository,
        upstream: &GitOid,
        merges: bool,
    ) -> Result<RebaseOutcome, GitEngineError> {
        repository.require_work_tree()?;
        let mut command = self.repository_command(repository);
        // Never stash implicitly: rebase.autoStash configuration must not
        // hide user state in automation; dirty trees report BlockedDirty.
        command.arg("rebase").arg("--no-autostash");
        if merges {
            command.arg("--rebase-merges");
        }
        command.arg(upstream.as_str());
        let output = self.execute(command)?;
        if output.status.success() {
            return Ok(if bounded_lossy(&output.stdout).contains("is up to date") {
                RebaseOutcome::UpToDate
            } else {
                RebaseOutcome::Rebased
            });
        }
        let combined = format!(
            "{}\n{}",
            bounded_lossy(&output.stdout),
            bounded_lossy(&output.stderr)
        );
        if repository.git_dir.join("rebase-merge").exists()
            || repository.git_dir.join("rebase-apply").exists()
            || combined.contains("Resolve all conflicts")
            || combined.contains("could not apply")
            || combined.contains("CONFLICT")
        {
            return Ok(RebaseOutcome::Conflicted);
        }
        if combined.contains("unstaged changes")
            || combined.contains("Please commit or stash")
            || combined.contains("Your local changes")
        {
            return Ok(RebaseOutcome::BlockedDirty);
        }
        Err(command_failed("rebase the branch", &output))
    }

    fn abort_branch_operation(&self, repository: &GitRepository) -> Result<bool, GitEngineError> {
        repository.require_work_tree()?;
        let operation = if repository.git_dir.join("rebase-merge").exists()
            || repository.git_dir.join("rebase-apply").exists()
        {
            "rebase"
        } else if repository.git_dir.join("MERGE_HEAD").exists() {
            "merge"
        } else {
            return Ok(false);
        };
        let mut command = self.repository_command(repository);
        command.arg(operation).arg("--abort");
        let output = self.execute(command)?;
        if output.status.success() {
            Ok(true)
        } else {
            Err(command_failed("abort the branch operation", &output))
        }
    }

    fn advance_checked_out_branch(
        &self,
        repository: &GitRepository,
        branch: &GitRefName,
        expected: &GitOid,
        target: &GitOid,
    ) -> Result<BranchAdvanceOutcome, GitEngineError> {
        repository.require_work_tree()?;
        if self.head_reference(repository)?.as_ref() != Some(branch)
            || self.read_ref(repository, branch)?.as_ref() != Some(expected)
        {
            return Ok(BranchAdvanceOutcome::Stale);
        }
        // `-i` skips the worktree up-to-date check: the worktree is
        // deliberately left as is, so only the index transition matters.
        let mut command = self.repository_command(repository);
        command.args(["read-tree", "-i", "-m", expected.as_str(), target.as_str()]);
        let output = self.execute(command)?;
        if !output.status.success() {
            if bounded_lossy(&output.stderr).contains("would be overwritten by merge") {
                return Ok(BranchAdvanceOutcome::BlockedStaged);
            }
            return Err(command_failed("merge the branch index", &output));
        }
        if self.compare_and_swap_ref(repository, branch, target, Some(expected))?
            == GitRefUpdateResult::Stale
        {
            // Restore the index transition so it matches the unmoved branch.
            let mut command = self.repository_command(repository);
            command.args(["read-tree", "-i", "-m", target.as_str(), expected.as_str()]);
            let _ = self.execute(command)?;
            return Ok(BranchAdvanceOutcome::Stale);
        }
        // Refresh stat information only; this never stages content.
        let mut command = self.repository_command(repository);
        command.args(["update-index", "-q", "--refresh"]);
        let _ = self.execute(command)?;
        Ok(BranchAdvanceOutcome::Advanced)
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
        let operation = GIT_OPERATION_MARKERS
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

    fn unattended_repository_state(
        &self,
        repository: &GitRepository,
    ) -> Result<GitUnattendedRepositoryState, GitEngineError> {
        repository.require_work_tree()?;
        let safety = self.safety_state(repository)?;
        let head_reference = self.head_reference(repository)?;
        let attached_head = head_reference.is_some();

        let mut worktrees = self.repository_command(repository);
        worktrees.args(["worktree", "list", "--porcelain"]);
        let worktrees = ensure_success("inspect Git worktrees", self.execute(worktrees)?)?;
        let worktree_count = decode_stdout("inspect Git worktrees", worktrees.stdout)?
            .lines()
            .filter(|line| line.starts_with("worktree "))
            .count();

        let mut index = self.repository_command(repository);
        index.args(["ls-files", "--stage", "-z"]);
        let index = ensure_success("inspect nested Git repositories", self.execute(index)?)?;
        let mut nested_repositories: Vec<String> = index
            .stdout
            .split(|byte| *byte == 0)
            .filter_map(|entry| {
                let tab = entry.iter().position(|byte| *byte == b'\t')?;
                let (metadata, path) = entry.split_at(tab);
                metadata
                    .starts_with(b"160000 ")
                    .then(|| String::from_utf8_lossy(&path[1..]).into_owned())
            })
            .collect();
        nested_repositories.extend(find_nested_git_metadata(repository.require_work_tree()?)?);
        nested_repositories.sort();
        nested_repositories.dedup();

        Ok(GitUnattendedRepositoryState {
            attached_head,
            head_reference,
            staged_changes: safety.staged_changes,
            worktree_count,
            operation: safety.operation,
            nested_repositories,
        })
    }

    fn repository_requirements(
        &self,
        repository: &GitRepository,
        vault_root: &Path,
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
        let environment = self.requirements_environment(repository)?;
        let cache_key = Self::requirements_cache_key(
            repository,
            &tracked.stdout,
            &tracked_paths,
            &environment,
        )?;
        let filters = if let Some(filters) = Self::load_requirements_cache(repository, &cache_key) {
            self.clear_staged_requirements_cache(repository)?;
            filters
        } else {
            let filters = self.inspect_path_filters(repository, &tracked.stdout)?;
            self.stage_requirements_cache(
                repository,
                GitRequirementsCache {
                    version: REQUIREMENTS_CACHE_VERSION,
                    key: cache_key,
                    filters: filters.clone(),
                },
            )?;
            filters
        };
        let mut required_filters = Vec::with_capacity(filters.len());
        for (name, path_count) in filters {
            let configured = |direction: &str| {
                environment
                    .configured_filter_keys
                    .contains(&format!("filter.{name}.{direction}").to_ascii_lowercase())
            };
            let clean_configured = configured("clean");
            let smudge_configured = configured("smudge");
            let process_configured = configured("process");
            let executable_available = (name == "lfs").then(|| {
                let mut command = self.repository_command(repository);
                command.args(["lfs", "version"]);
                self.execute(command)
                    .is_ok_and(|output| output.status.success())
            });
            required_filters.push(GitFilterRequirement {
                name,
                path_count,
                clean_configured,
                smudge_configured,
                process_configured,
                executable_available,
            });
        }
        let git_lfs_available = required_filters
            .iter()
            .find(|filter| filter.name == "lfs")
            .and_then(|filter| filter.executable_available);

        let ignored_internal_paths = self.ignored_internal_paths(repository, vault_root)?;

        Ok(GitRepositoryRequirements {
            tracked_paths: tracked_paths.len(),
            ignored_internal_paths,
            required_filters,
            git_lfs_available,
        })
    }

    fn persist_repository_requirements_cache(
        &self,
        repository: &GitRepository,
    ) -> Result<(), GitEngineError> {
        let cache = self
            .pending_requirements
            .lock()
            .map_err(|_| GitEngineError::InvalidOutput {
                operation: "cache Git repository requirements",
                detail: "the requirements cache lock is poisoned".to_string(),
            })?
            .remove(&repository.git_dir);
        if let Some(cache) = cache {
            Self::save_requirements_cache(repository, &cache)?;
        }
        Ok(())
    }
}

fn find_nested_git_metadata(root: &Path) -> Result<Vec<String>, GitEngineError> {
    const MAX_INSPECTED_DIRECTORIES: usize = 100_000;
    fn visit(
        root: &Path,
        directory: &Path,
        found: &mut Vec<String>,
        inspected: &mut usize,
    ) -> Result<(), GitEngineError> {
        *inspected += 1;
        if *inspected > MAX_INSPECTED_DIRECTORIES {
            return Err(GitEngineError::UnsupportedRepository {
                detail: format!(
                    "nested-repository inspection exceeded {MAX_INSPECTED_DIRECTORIES} directories"
                ),
            });
        }
        let entries = std::fs::read_dir(directory).map_err(GitEngineError::Io)?;
        for entry in entries {
            let entry = entry.map_err(GitEngineError::Io)?;
            let path = entry.path();
            let file_type = entry.file_type().map_err(GitEngineError::Io)?;
            if entry.file_name() == ".git" && directory != root {
                let relative = path.strip_prefix(root).unwrap_or(&path);
                found.push(
                    relative
                        .components()
                        .map(|component| component.as_os_str().to_string_lossy().into_owned())
                        .collect::<Vec<_>>()
                        .join("/"),
                );
            } else if file_type.is_dir()
                && !(directory == root
                    && (entry.file_name() == ".git" || entry.file_name() == ".vulcan"))
            {
                visit(root, &path, found, inspected)?;
            }
        }
        Ok(())
    }

    let mut found = Vec::new();
    visit(root, root, &mut found, &mut 0)?;
    Ok(found)
}

impl GitCliEngine {
    fn worktree_command(&self, directory: &Path) -> Command {
        let mut command = self.command();
        command.current_dir(directory);
        command
    }

    /// A command in a replay worktree: no hooks, no signing, no LFS
    /// downloads, and no editor.
    fn replay_command(&self, directory: &Path) -> Command {
        let mut command = self.worktree_command(directory);
        command
            .env("GIT_EDITOR", "true")
            .env("GIT_LFS_SKIP_SMUDGE", "1")
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "commit.gpgSign=false",
            ]);
        command
    }

    fn worktree_git(&self, directory: &Path, arguments: &[&str]) -> Result<Output, GitEngineError> {
        let mut command = self.worktree_command(directory);
        command.args(arguments);
        self.execute(command)
    }

    /// Cherry-picks `commits` in order inside the replay worktree. The outer
    /// `Err` is an engine failure; the inner one is a reason to decline.
    #[allow(clippy::too_many_lines)]
    fn replay_in_worktree(
        &self,
        directory: &Path,
        commits: &[GitOid],
        resolver: &mut dyn BranchReplayResolver,
    ) -> Result<Result<(GitOid, Vec<String>), String>, GitEngineError> {
        const OPERATION: &str = "replay a commit";
        let mut resolved_paths = BTreeSet::new();
        for commit in commits {
            let short = &commit.as_str()[..commit.as_str().len().min(12)];
            // `--allow-empty` keeps commits that were empty to begin with;
            // one the upstream already contains stops and is skipped below.
            let mut pick = self.replay_command(directory);
            pick.args(["cherry-pick", "--allow-empty", commit.as_str()]);
            let picked = self.execute(pick)?;
            if picked.status.success() {
                continue;
            }
            let unmerged = self.worktree_git(directory, &["ls-files", "-u", "-z"])?;
            if !unmerged.status.success() {
                return Err(command_failed(OPERATION, &unmerged));
            }
            if unmerged.stdout.is_empty()
                && self
                    .worktree_git(directory, &["diff", "--cached", "--quiet", "HEAD"])?
                    .status
                    .success()
            {
                let mut skip = self.replay_command(directory);
                skip.args(["cherry-pick", "--skip"]);
                let skipped = self.execute(skip)?;
                if !skipped.status.success() {
                    return Err(command_failed(OPERATION, &skipped));
                }
                continue;
            }
            // `<mode> <oid> <stage>\t<path>` records, NUL-separated.
            let mut stages: BTreeMap<String, [Option<(String, String)>; 3]> = BTreeMap::new();
            for record in unmerged.stdout.split(|byte| *byte == 0) {
                let record = String::from_utf8_lossy(record);
                let Some((meta, path)) = record.split_once('\t') else {
                    continue;
                };
                let fields = meta.split(' ').collect::<Vec<_>>();
                let [mode, oid, stage] = fields.as_slice() else {
                    continue;
                };
                let Some(index) = stage
                    .parse::<usize>()
                    .ok()
                    .and_then(|stage| stage.checked_sub(1))
                else {
                    continue;
                };
                if let Some(slot) = stages.entry(path.to_string()).or_default().get_mut(index) {
                    *slot = Some(((*mode).to_string(), (*oid).to_string()));
                }
            }
            if stages.is_empty() {
                return Ok(Err(format!(
                    "replaying {short} failed without a file conflict: {}",
                    bounded_lossy(&picked.stderr).trim()
                )));
            }
            let mut conflicts = Vec::new();
            for (path, [base, ours, theirs]) in &stages {
                let (Some(ours), Some(theirs)) = (ours, theirs) else {
                    return Ok(Err(format!(
                        "replaying {short} deletes `{path}` on one side"
                    )));
                };
                let mut sides = Vec::new();
                for side in [base.as_ref(), Some(ours), Some(theirs)] {
                    let Some((mode, oid)) = side else {
                        sides.push(None);
                        continue;
                    };
                    if mode != "100644" && mode != "100755" {
                        return Ok(Err(format!("`{path}` is not a regular file")));
                    }
                    let blob = self.worktree_git(directory, &["cat-file", "blob", oid])?;
                    if !blob.status.success() {
                        return Err(command_failed(OPERATION, &blob));
                    }
                    let Ok(text) = String::from_utf8(blob.stdout) else {
                        return Ok(Err(format!("`{path}` is not a text file")));
                    };
                    sides.push(Some(text));
                }
                let mut sides = sides.into_iter();
                conflicts.push(BranchReplayConflict {
                    path: path.clone(),
                    base: sides.next().flatten(),
                    upstream: sides.next().flatten(),
                    local: sides.next().flatten(),
                });
            }
            let subject =
                self.worktree_git(directory, &["log", "-1", "--format=%s", commit.as_str()])?;
            let subject = bounded_lossy(&subject.stdout).trim().to_string();
            let resolutions = match resolver.resolve(&subject, &conflicts) {
                Ok(resolutions) => resolutions,
                Err(error) => return Ok(Err(format!("the resolver failed: {error}"))),
            };
            let expected = stages.keys().cloned().collect::<BTreeSet<_>>();
            let supplied = resolutions
                .iter()
                .map(|(path, _)| path.clone())
                .collect::<BTreeSet<_>>();
            if supplied != expected || supplied.len() != resolutions.len() {
                return Ok(Err(
                    "the resolver did not return exactly the conflicted files".to_string(),
                ));
            }
            for (path, content) in &resolutions {
                if content
                    .lines()
                    .any(|line| line.starts_with("<<<<<<<") || line.starts_with(">>>>>>>"))
                {
                    return Ok(Err(format!(
                        "the resolution of `{path}` still has conflict markers"
                    )));
                }
                std::fs::write(directory.join(path), content).map_err(GitEngineError::Io)?;
            }
            let mut add = self.worktree_command(directory);
            add.args(["add", "--"]).args(supplied.iter());
            let added = self.execute(add)?;
            if !added.status.success() {
                return Err(command_failed(OPERATION, &added));
            }
            let unchanged = self
                .worktree_git(directory, &["diff", "--cached", "--quiet"])?
                .status
                .success();
            let mut finish = self.replay_command(directory);
            finish.arg("cherry-pick");
            finish.arg(if unchanged { "--skip" } else { "--continue" });
            let finished = self.execute(finish)?;
            if !finished.status.success() {
                return Ok(Err(format!(
                    "replaying {short} could not be completed: {}",
                    bounded_lossy(&finished.stderr).trim()
                )));
            }
            resolved_paths.extend(supplied);
        }
        let head = self.worktree_git(directory, &["rev-parse", "HEAD"])?;
        if !head.status.success() {
            return Err(command_failed(OPERATION, &head));
        }
        let head = GitOid::parse(decode_stdout(OPERATION, head.stdout)?.trim())?;
        Ok(Ok((head, resolved_paths.into_iter().collect())))
    }

    /// Moves the checked-out branch from `expected` to an unrelated `target`
    /// with Git writing the worktree: a two-way checkout that refuses to
    /// overwrite uncommitted bytes, then a compare-and-swap of the branch.
    fn move_checked_out_branch(
        &self,
        repository: &GitRepository,
        branch: &GitRefName,
        expected: &GitOid,
        target: &GitOid,
    ) -> Result<BranchAdvanceOutcome, GitEngineError> {
        if self.head_reference(repository)?.as_ref() != Some(branch)
            || self.read_ref(repository, branch)?.as_ref() != Some(expected)
        {
            return Ok(BranchAdvanceOutcome::Stale);
        }
        let mut refresh = self.repository_command(repository);
        refresh.args(["update-index", "-q", "--refresh"]);
        let _ = self.execute(refresh)?;
        let mut checkout = self.repository_command(repository);
        checkout.args(["read-tree", "-m", "-u", expected.as_str(), target.as_str()]);
        let output = self.execute(checkout)?;
        if !output.status.success() {
            let stderr = bounded_lossy(&output.stderr);
            if stderr.contains("not uptodate")
                || stderr.contains("would be overwritten")
                || stderr.contains("would be removed")
            {
                return Ok(BranchAdvanceOutcome::BlockedStaged);
            }
            return Err(command_failed("update branch files", &output));
        }
        // The index and files now hold `target`; only the branch is left.
        if self.compare_and_swap_ref(repository, branch, target, Some(expected))?
            == GitRefUpdateResult::Stale
        {
            // Put the files back only while HEAD still names `expected`;
            // if something else moved it, its owner decides what the
            // checkout should hold.
            if self.head_commit(repository)?.as_ref() == Some(expected) {
                let mut restore = self.repository_command(repository);
                restore.args(["read-tree", "-m", "-u", target.as_str(), expected.as_str()]);
                let _ = self.execute(restore)?;
            }
            return Ok(BranchAdvanceOutcome::Stale);
        }
        let mut refresh = self.repository_command(repository);
        refresh.args(["update-index", "-q", "--refresh"]);
        let _ = self.execute(refresh)?;
        Ok(BranchAdvanceOutcome::Advanced)
    }
}

/// A detached scratch worktree for a replay; removed and pruned on drop,
/// including when the replay panics.
struct ScratchWorktree<'a> {
    engine: &'a GitCliEngine,
    repository: &'a GitRepository,
    directory: PathBuf,
}

impl<'a> ScratchWorktree<'a> {
    fn add(
        engine: &'a GitCliEngine,
        repository: &'a GitRepository,
        at: &GitOid,
    ) -> Result<Self, GitEngineError> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        let directory = repository
            .common_dir
            .join("vulcan-sync")
            .join(format!("replay-{}-{nanos}", std::process::id()));
        if let Some(parent) = directory.parent() {
            std::fs::create_dir_all(parent).map_err(GitEngineError::Io)?;
        }
        let mut add = engine.repository_command(repository);
        add.env("GIT_LFS_SKIP_SMUDGE", "1")
            .args(["-c", "core.hooksPath=/dev/null"])
            .args(["worktree", "add", "--quiet", "--detach"])
            .arg(git_cli_path(&directory))
            .arg(at.as_str());
        let output = engine.execute(add)?;
        let scratch = Self {
            engine,
            repository,
            directory,
        };
        if !output.status.success() {
            return Err(command_failed("replay the branch", &output));
        }
        Ok(scratch)
    }
}

impl Drop for ScratchWorktree<'_> {
    fn drop(&mut self) {
        let mut remove = self.engine.repository_command(self.repository);
        remove
            .args(["worktree", "remove", "--force"])
            .arg(git_cli_path(&self.directory));
        let _ = self.engine.execute(remove);
        let _ = std::fs::remove_dir_all(&self.directory);
        let mut prune = self.engine.repository_command(self.repository);
        prune.args(["worktree", "prune"]);
        let _ = self.engine.execute(prune);
    }
}

impl GitCliEngine {
    /// The first Git `filter` driver set on a path that the local commits
    /// or the upstream changed since they parted.
    fn replay_filtered_path(
        &self,
        repository: &GitRepository,
        head: &GitOid,
        upstream: &GitOid,
    ) -> Result<Option<String>, GitEngineError> {
        const OPERATION: &str = "inspect replayed paths";
        let mut base = self.repository_command(repository);
        base.args(["merge-base", head.as_str(), upstream.as_str()]);
        let base = ensure_success(OPERATION, self.execute(base)?)?;
        let base = decode_stdout(OPERATION, base.stdout)?.trim().to_string();
        let mut paths = Vec::new();
        for side in [head, upstream] {
            let mut diff = self.repository_command(repository);
            diff.args(["diff", "--name-only", "--no-renames", "-z"])
                .arg(&base)
                .arg(side.as_str());
            let diff = ensure_success(OPERATION, self.execute(diff)?)?;
            paths.extend_from_slice(&diff.stdout);
        }
        if paths.is_empty() {
            return Ok(None);
        }
        Ok(self
            .inspect_path_filters(repository, &paths)?
            .into_keys()
            .next())
    }

    /// Whether a boolean Git configuration value is true.
    fn config_flag(&self, repository: &GitRepository, key: &str) -> Result<bool, GitEngineError> {
        let mut command = self.repository_command(repository);
        command.args(["config", "--type=bool", "--get", key]);
        let output = self.execute(command)?;
        Ok(output.status.success() && String::from_utf8_lossy(&output.stdout).trim() == "true")
    }

    fn inspect_path_filters(
        &self,
        repository: &GitRepository,
        tracked_paths: &[u8],
    ) -> Result<BTreeMap<String, usize>, GitEngineError> {
        let mut command = self.repository_command(repository);
        command.args(["check-attr", "-z", "--stdin", "filter"]);
        let attributes =
            self.execute_with_input(command, "inspect configured Git filters", tracked_paths)?;
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
        for triple in fields.as_chunks::<3>().0 {
            let value = triple[2];
            if !matches!(value, "unspecified" | "unset" | "set" | "") {
                *filters.entry(value.to_string()).or_default() += 1;
            }
        }
        Ok(filters)
    }

    fn requirements_cache_key(
        repository: &GitRepository,
        tracked_bytes: &[u8],
        tracked_paths: &[&str],
        environment: &GitRequirementsEnvironment,
    ) -> Result<String, GitEngineError> {
        let work_tree = repository.require_work_tree()?;
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"vulcan-git-requirements-cache-v1\0");
        hasher.update(tracked_bytes);
        for key in &environment.configured_filter_keys {
            hasher.update(key.as_bytes());
            hasher.update(b"\0");
        }

        let mut sources = BTreeSet::new();
        sources.insert(repository.common_dir.join("info/attributes"));
        for path in tracked_paths {
            validate_repository_path(path)?;
            let mut parent = Path::new(path).parent();
            while let Some(directory) = parent {
                sources.insert(work_tree.join(directory).join(".gitattributes"));
                parent = directory.parent();
            }
        }

        for (name, value) in &environment.attribute_sources {
            hasher.update(name.as_bytes());
            hasher.update(b"=");
            hasher.update(value.as_bytes());
            hasher.update(b"\0");
            if !value.is_empty() {
                sources.insert(PathBuf::from(value));
            }
        }
        for source in sources {
            hash_attribute_source(&mut hasher, &source)?;
        }
        Ok(hasher.finalize().to_hex().to_string())
    }

    fn requirements_cache_path(repository: &GitRepository) -> PathBuf {
        repository
            .git_dir
            .join("vulcan-sync/requirements-cache-v1.json")
    }

    fn load_requirements_cache(
        repository: &GitRepository,
        key: &str,
    ) -> Option<BTreeMap<String, usize>> {
        let path = Self::requirements_cache_path(repository);
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            return None;
        };
        if !metadata.file_type().is_file() || metadata.len() > MAX_ERROR_BYTES as u64 {
            return None;
        }
        let Ok(bytes) = std::fs::read(path) else {
            return None;
        };
        let Ok(cache) = serde_json::from_slice::<GitRequirementsCache>(&bytes) else {
            return None;
        };
        if cache.version != REQUIREMENTS_CACHE_VERSION
            || cache.key != key
            || cache
                .filters
                .keys()
                .any(|name| name.is_empty() || name.chars().any(char::is_control))
        {
            return None;
        }
        Some(cache.filters)
    }

    fn stage_requirements_cache(
        &self,
        repository: &GitRepository,
        cache: GitRequirementsCache,
    ) -> Result<(), GitEngineError> {
        self.pending_requirements
            .lock()
            .map_err(|_| GitEngineError::InvalidOutput {
                operation: "cache Git repository requirements",
                detail: "the requirements cache lock is poisoned".to_string(),
            })?
            .insert(repository.git_dir.clone(), cache);
        Ok(())
    }

    fn clear_staged_requirements_cache(
        &self,
        repository: &GitRepository,
    ) -> Result<(), GitEngineError> {
        self.pending_requirements
            .lock()
            .map_err(|_| GitEngineError::InvalidOutput {
                operation: "cache Git repository requirements",
                detail: "the requirements cache lock is poisoned".to_string(),
            })?
            .remove(&repository.git_dir);
        Ok(())
    }

    fn save_requirements_cache(
        repository: &GitRepository,
        cache: &GitRequirementsCache,
    ) -> Result<(), GitEngineError> {
        let path = Self::requirements_cache_path(repository);
        let parent = path
            .parent()
            .expect("the requirements cache path always has a parent");
        std::fs::create_dir_all(parent)?;
        if std::fs::symlink_metadata(&path).is_ok_and(|metadata| !metadata.file_type().is_file()) {
            remove_file_if_present(&path)?;
        }
        let bytes = serde_json::to_vec(cache).map_err(|error| GitEngineError::InvalidOutput {
            operation: "cache Git repository requirements",
            detail: error.to_string(),
        })?;
        let mut temporary = NamedTempFile::new_in(parent)?;
        temporary.write_all(&bytes)?;
        temporary.as_file().sync_all()?;
        temporary
            .persist(path)
            .map_err(|error| GitEngineError::Io(error.error))?;
        Ok(())
    }

    /// The vault's rebuildable cache files that Git ignores, as
    /// work-tree-relative paths. Paths are anchored at the vault, so a vault
    /// in a subdirectory is checked against its own `.vulcan/`.
    fn ignored_internal_paths(
        &self,
        repository: &GitRepository,
        vault_root: &Path,
    ) -> Result<Vec<String>, GitEngineError> {
        let prefix = vault_work_tree_prefix(repository, vault_root);
        let internal_paths = ["cache.db", "cache.db-wal", "cache.db-shm"]
            .map(|name| format!("{prefix}.vulcan/{name}"));
        let mut input = internal_paths.join("\0").into_bytes();
        input.push(0);
        let mut command = self.repository_command(repository);
        command.args(["check-ignore", "-z", "--stdin"]);
        let output = self.execute_with_input(command, "inspect ignored Vulcan state", &input)?;
        if !matches!(output.status.code(), Some(0 | 1)) {
            return Err(command_failed("inspect ignored Vulcan state", &output));
        }
        let ignored = nul_fields("inspect ignored Vulcan state", &output.stdout)?
            .into_iter()
            .collect::<BTreeSet<_>>();
        if ignored
            .iter()
            .any(|path| !internal_paths.iter().any(|internal| internal == path))
        {
            return Err(GitEngineError::InvalidOutput {
                operation: "inspect ignored Vulcan state",
                detail: "Git returned an unexpected ignored path".to_string(),
            });
        }
        Ok(internal_paths
            .into_iter()
            .filter(|path| ignored.contains(path.as_str()))
            .collect())
    }

    fn requirements_environment(
        &self,
        repository: &GitRepository,
    ) -> Result<GitRequirementsEnvironment, GitEngineError> {
        let variables = self.repository_output(
            repository,
            "inspect Git requirement environment",
            ["var", "-l"],
        )?;
        if variables.stdout.len() > MAX_DIAGNOSTIC_PATH_BYTES {
            return Err(GitEngineError::InvalidOutput {
                operation: "inspect Git requirement environment",
                detail: "Git variable output exceeds the bounded response limit".to_string(),
            });
        }
        let variables = decode_stdout("inspect Git requirement environment", variables.stdout)?;
        let mut configured_filter_keys = BTreeSet::new();
        let mut attribute_sources = Vec::new();
        for line in variables.lines() {
            let Some((name, value)) = line.split_once('=') else {
                continue;
            };
            let lower_name = name.to_ascii_lowercase();
            if lower_name.starts_with("filter.")
                && matches!(
                    lower_name.rsplit_once('.').map(|(_, suffix)| suffix),
                    Some("clean" | "smudge" | "process")
                )
            {
                configured_filter_keys.insert(lower_name);
            }
            if matches!(name, "GIT_ATTR_SYSTEM" | "GIT_ATTR_GLOBAL") {
                attribute_sources.push((name.to_string(), value.to_string()));
            }
        }
        Ok(GitRequirementsEnvironment {
            configured_filter_keys,
            attribute_sources,
        })
    }
}

fn join_reader(
    reader: std::thread::ScopedJoinHandle<'_, std::io::Result<Vec<u8>>>,
    operation: &'static str,
    stream: &str,
) -> Result<Vec<u8>, GitEngineError> {
    reader
        .join()
        .map_err(|_| GitEngineError::InvalidOutput {
            operation,
            detail: format!("Git {stream} reader panicked"),
        })?
        .map_err(GitEngineError::from)
}

fn hash_attribute_source(hasher: &mut blake3::Hasher, path: &Path) -> Result<(), GitEngineError> {
    hash_path(hasher, path);
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        hasher.update(b"missing\0");
        return Ok(());
    };
    if metadata.file_type().is_symlink() {
        hasher.update(b"symlink\0");
        hash_path(hasher, &std::fs::read_link(path)?);
        return Ok(());
    }
    if !metadata.file_type().is_file() {
        hasher.update(b"other\0");
        return Ok(());
    }
    if metadata.len() > MAX_DIAGNOSTIC_PATH_BYTES as u64 {
        return Err(GitEngineError::InvalidOutput {
            operation: "hash Git attribute sources",
            detail: format!(
                "attribute file exceeds the {MAX_DIAGNOSTIC_PATH_BYTES} byte limit: {}",
                path.display()
            ),
        });
    }
    hasher.update(b"file\0");
    hasher.update(&std::fs::read(path)?);
    hasher.update(b"\0");
    Ok(())
}

#[cfg(unix)]
fn hash_path(hasher: &mut blake3::Hasher, path: &Path) {
    use std::os::unix::ffi::OsStrExt;
    hasher.update(path.as_os_str().as_bytes());
    hasher.update(b"\0");
}

#[cfg(windows)]
fn hash_path(hasher: &mut blake3::Hasher, path: &Path) {
    use std::os::windows::ffi::OsStrExt;
    for unit in path.as_os_str().encode_wide() {
        hasher.update(&unit.to_le_bytes());
    }
    hasher.update(b"\0");
}

#[cfg(unix)]
fn configure_git_process(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

#[cfg(windows)]
fn configure_git_process(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    command.creation_flags(WINDOWS_GIT_CREATION_FLAGS);
}

#[cfg(not(any(unix, windows)))]
fn configure_git_process(_command: &mut Command) {}

#[cfg(unix)]
fn terminate_process_group(child: &mut Child) {
    let Ok(process_group) = i32::try_from(child.id()) else {
        let _ = child.kill();
        return;
    };
    // SAFETY: the child was placed into a new process group whose ID is
    // its validated positive PID; signaling it does not dereference memory
    // and the caller still reaps with `Child::wait`.
    unsafe {
        libc::kill(-process_group, libc::SIGTERM);
    }
    // SIGTERM lets Git run its lock-file cleanup handlers; escalate to
    // SIGKILL only when the process group ignores the grace period.
    if matches!(child.wait_timeout(TERMINATION_GRACE_PERIOD), Ok(Some(_))) {
        return;
    }
    // SAFETY: same process-group reasoning as the SIGTERM above; the grace
    // period expired without the child exiting.
    unsafe {
        libc::kill(-process_group, libc::SIGKILL);
    }
    let _ = child.kill();
}

#[cfg(not(unix))]
fn terminate_process_group(child: &mut Child) {
    let _ = child.kill();
}

fn validate_git_source(source: &str) -> Result<(), GitEngineError> {
    if source.is_empty() || source.starts_with('-') || source.chars().any(char::is_control) {
        return Err(GitEngineError::UnsupportedRepository {
            detail: "Git source must be non-empty, must not start with `-`, and must not contain control characters"
                .to_string(),
        });
    }
    Ok(())
}

fn validate_detached_recovery_request(
    request: &GitDetachedRecoveryRequest,
) -> Result<(), GitEngineError> {
    validate_git_source(&request.source)?;
    if !request.work_tree.is_dir() {
        return Err(GitEngineError::UnsupportedRepository {
            detail: format!(
                "detached recovery requires the preserved worktree directory: {}",
                request.work_tree.display()
            ),
        });
    }
    if request.git_dir.exists() {
        return Err(GitEngineError::UnsupportedRepository {
            detail: format!(
                "refusing detached recovery because the registered Git directory exists: {}",
                request.git_dir.display()
            ),
        });
    }
    let pointer = request.work_tree.join(".git");
    let metadata = std::fs::symlink_metadata(&pointer).map_err(|error| {
        GitEngineError::UnsupportedRepository {
            detail: format!(
                "detached recovery requires the existing stale Git pointer {}: {error}",
                pointer.display()
            ),
        }
    })?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(GitEngineError::UnsupportedRepository {
            detail: format!(
                "refusing to replace non-regular Git pointer {}",
                pointer.display()
            ),
        });
    }
    let source = std::fs::read_to_string(&pointer)?;
    let raw = source
        .trim()
        .strip_prefix("gitdir:")
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| GitEngineError::UnsupportedRepository {
            detail: format!("invalid stale Git pointer {}", pointer.display()),
        })?;
    let referenced = PathBuf::from(raw);
    let referenced = if referenced.is_absolute() {
        referenced
    } else {
        request.work_tree.join(referenced)
    };
    if !same_prospective_path(&referenced, &request.git_dir) {
        return Err(GitEngineError::UnsupportedRepository {
            detail: format!(
                "refusing destructive reattachment: {} points to {}, not the registered missing Git directory {}",
                pointer.display(),
                referenced.display(),
                request.git_dir.display()
            ),
        });
    }
    if request.git_dir.parent().is_none() {
        return Err(GitEngineError::UnsupportedRepository {
            detail: "the detached Git directory must have a parent directory".to_string(),
        });
    }
    Ok(())
}

fn same_prospective_path(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    let canonical_parent_and_name =
        |path: &Path| Some(path.parent()?.canonicalize().ok()?.join(path.file_name()?));
    matches!(
        (
            canonical_parent_and_name(left),
            canonical_parent_and_name(right)
        ),
        (Some(left), Some(right)) if left == right
    )
}

fn prospective_absolute_path(path: &Path) -> Result<PathBuf, GitEngineError> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

#[cfg(not(windows))]
fn git_cli_path(path: &Path) -> OsString {
    path.as_os_str().to_os_string()
}

#[cfg(windows)]
fn git_cli_path(path: &Path) -> OsString {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};

    const VERBATIM_PREFIX: &[u16] = &[92, 92, 63, 92];
    const UNC_PREFIX: &[u16] = &[85, 78, 67, 92];

    let encoded = path.as_os_str().encode_wide().collect::<Vec<_>>();
    if !encoded.starts_with(VERBATIM_PREFIX) {
        return path.as_os_str().to_os_string();
    }
    let remainder = &encoded[VERBATIM_PREFIX.len()..];
    if remainder.starts_with(UNC_PREFIX) {
        let normalized = [u16::from(b'\\'), u16::from(b'\\')]
            .into_iter()
            .chain(remainder[UNC_PREFIX.len()..].iter().copied())
            .collect::<Vec<_>>();
        return OsString::from_wide(&normalized);
    }
    let is_verbatim_disk = remainder.len() >= 3
        && ((u16::from(b'A')..=u16::from(b'Z')).contains(&remainder[0])
            || (u16::from(b'a')..=u16::from(b'z')).contains(&remainder[0]))
        && remainder[1] == u16::from(b':')
        && (remainder[2] == u16::from(b'\\') || remainder[2] == u16::from(b'/'));
    if is_verbatim_disk {
        OsString::from_wide(remainder)
    } else {
        path.as_os_str().to_os_string()
    }
}

fn write_gitdir_pointer(work_tree: &Path, git_dir: &Path) -> Result<(), GitEngineError> {
    let git_dir = git_cli_path(git_dir);
    let git_dir = git_dir
        .to_str()
        .filter(|git_dir| !git_dir.bytes().any(|byte| byte.is_ascii_control()))
        .ok_or_else(|| GitEngineError::UnsupportedRepository {
            detail: "the detached Git directory path must be single-line valid UTF-8 for the Git pointer file"
                .to_string(),
        })?;
    let pointer = work_tree.join(".git");
    let mut temporary = NamedTempFile::new_in(work_tree)?;
    writeln!(temporary, "gitdir: {git_dir}")?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(&pointer)
        .map_err(|error| GitEngineError::Io(error.error))?;
    Ok(())
}

fn lost_detached_ref_namespaces() -> Vec<String> {
    local_recovery_ref_namespaces()
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

fn parse_reference_list(stdout: &[u8]) -> Result<Vec<GitReference>, GitEngineError> {
    let mut references = Vec::new();
    for line in stdout.split(|byte| *byte == b'\n') {
        if line.is_empty() {
            continue;
        }
        let fields = line.split(|byte| *byte == 0).collect::<Vec<_>>();
        if fields.len() != 3 || !fields[2].is_empty() {
            return Err(GitEngineError::InvalidOutput {
                operation: "list Git refs",
                detail: "expected ref name and object ID as NUL-delimited fields".to_string(),
            });
        }
        let name =
            std::str::from_utf8(fields[0]).map_err(|error| GitEngineError::InvalidOutput {
                operation: "list Git refs",
                detail: error.to_string(),
            })?;
        let target =
            std::str::from_utf8(fields[1]).map_err(|error| GitEngineError::InvalidOutput {
                operation: "list Git refs",
                detail: error.to_string(),
            })?;
        references.push(GitReference {
            name: GitRefName::parse(name)?,
            target: GitOid::parse(target)?,
        });
    }
    references.sort_by(|left, right| left.name.as_str().cmp(right.name.as_str()));
    Ok(references)
}

fn parse_remote_reference_list(
    stdout: &[u8],
    prefix: &GitRefName,
) -> Result<Vec<GitReference>, GitEngineError> {
    const OPERATION: &str = "list remote Git refs";
    const MAX_REFS: usize = 4096;
    let required_prefix = format!("{}/", prefix.as_str().trim_end_matches('/'));
    let text = std::str::from_utf8(stdout).map_err(|error| GitEngineError::InvalidOutput {
        operation: OPERATION,
        detail: error.to_string(),
    })?;
    let mut references = Vec::new();
    let mut names = BTreeSet::new();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        if references.len() == MAX_REFS {
            return Err(GitEngineError::InvalidOutput {
                operation: OPERATION,
                detail: format!("remote returned more than {MAX_REFS} refs beneath {prefix}"),
            });
        }
        let (target, name) =
            line.split_once('\t')
                .ok_or_else(|| GitEngineError::InvalidOutput {
                    operation: OPERATION,
                    detail: format!("expected `<oid>\\t<ref>`, received `{line}`"),
                })?;
        if !name.starts_with(&required_prefix) || !names.insert(name) {
            return Err(GitEngineError::InvalidOutput {
                operation: OPERATION,
                detail: format!("unexpected or duplicate remote ref `{name}`"),
            });
        }
        references.push(GitReference {
            name: GitRefName::parse(name)?,
            target: GitOid::parse(target)?,
        });
    }
    references.sort_by(|left, right| left.name.as_str().cmp(right.name.as_str()));
    Ok(references)
}

fn replace_bytes(haystack: &[u8], needle: &[u8], replacement: &[u8]) -> Vec<u8> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return haystack.to_vec();
    }
    let mut output = Vec::with_capacity(haystack.len());
    let mut index = 0;
    while index < haystack.len() {
        if haystack[index..].starts_with(needle) {
            output.extend_from_slice(replacement);
            index += needle.len();
        } else {
            output.push(haystack[index]);
            index += 1;
        }
    }
    output
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
        .as_chunks::<4>()
        .0
        .iter()
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

fn parse_patch_paths(bytes: &[u8]) -> Result<Vec<String>, GitEngineError> {
    if bytes.len() > MAX_DIAGNOSTIC_PATH_BYTES {
        return Err(GitEngineError::InvalidOutput {
            operation: "inspect supplied Git patch paths",
            detail: format!("path output exceeds the {MAX_DIAGNOSTIC_PATH_BYTES} byte limit"),
        });
    }
    if !bytes.is_empty() && bytes.last() != Some(&0) {
        return Err(GitEngineError::InvalidOutput {
            operation: "inspect supplied Git patch paths",
            detail: "expected NUL-terminated numstat records".to_string(),
        });
    }
    let mut paths = Vec::new();
    for record in bytes
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
    {
        let mut fields = record.splitn(3, |byte| *byte == b'\t');
        let additions = fields.next();
        let deletions = fields.next();
        let path = fields.next().unwrap_or_default();
        if additions.is_none() || deletions.is_none() || path.is_empty() {
            return Err(GitEngineError::UnsupportedRepository {
                detail: "supplied patches must use ordinary single-path hunks; rename/copy records are unsupported"
                    .to_string(),
            });
        }
        let path =
            String::from_utf8(path.to_vec()).map_err(|error| GitEngineError::InvalidOutput {
                operation: "inspect supplied Git patch paths",
                detail: error.to_string(),
            })?;
        validate_repository_path(&path)?;
        paths.push(path);
    }
    paths.sort();
    paths.dedup();
    if paths.is_empty() {
        return Err(GitEngineError::InvalidOutput {
            operation: "inspect supplied Git patch paths",
            detail: "patch contains no changed paths".to_string(),
        });
    }
    Ok(paths)
}

fn reject_unsupported_patch_operations(patch: &[u8]) -> Result<(), GitEngineError> {
    for line in patch.split(|byte| *byte == b'\n') {
        if line == b"+++ /dev/null" || line.starts_with(b"deleted file mode ") {
            return Err(GitEngineError::UnsupportedRepository {
                detail: "supplied conflict patches may not delete files".to_string(),
            });
        }
        if line.starts_with(b"rename from ")
            || line.starts_with(b"rename to ")
            || line.starts_with(b"copy from ")
            || line.starts_with(b"copy to ")
        {
            return Err(GitEngineError::UnsupportedRepository {
                detail: "supplied conflict patches may not rename or copy files".to_string(),
            });
        }
    }
    Ok(())
}

fn remove_file_if_present(path: &Path) -> Result<(), GitEngineError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(GitEngineError::Io(error)),
    }
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

fn parse_tree_entries(bytes: &[u8]) -> Result<Vec<GitTreeEntry>, GitEngineError> {
    const OPERATION: &str = "list Git tree entries";
    if bytes.len() > MAX_DIAGNOSTIC_PATH_BYTES {
        return Err(GitEngineError::InvalidOutput {
            operation: OPERATION,
            detail: format!("tree output exceeds the {MAX_DIAGNOSTIC_PATH_BYTES} byte limit"),
        });
    }
    let mut entries = Vec::new();
    for record in bytes
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
    {
        let separator = record
            .iter()
            .position(|byte| *byte == b'\t')
            .ok_or_else(|| GitEngineError::InvalidOutput {
                operation: OPERATION,
                detail: "tree entry lacks a path separator".to_string(),
            })?;
        let (header, path_with_separator) = record.split_at(separator);
        let path = &path_with_separator[1..];
        let header =
            std::str::from_utf8(header).map_err(|error| GitEngineError::InvalidOutput {
                operation: OPERATION,
                detail: error.to_string(),
            })?;
        let mut fields = header.split_whitespace();
        let mode = fields.next().unwrap_or_default();
        let kind = fields.next().unwrap_or_default();
        let oid = fields.next().unwrap_or_default();
        if mode.is_empty() || kind.is_empty() || oid.is_empty() || fields.next().is_some() {
            return Err(GitEngineError::InvalidOutput {
                operation: OPERATION,
                detail: "expected `<mode> <type> <object>\\t<path>`".to_string(),
            });
        }
        let path =
            String::from_utf8(path.to_vec()).map_err(|error| GitEngineError::InvalidOutput {
                operation: OPERATION,
                detail: error.to_string(),
            })?;
        validate_repository_path(&path)?;
        entries.push(GitTreeEntry {
            path,
            oid: GitOid::parse(oid)?,
            mode: mode.to_string(),
            kind: kind.to_string(),
        });
    }
    Ok(entries)
}

#[cfg(test)]
fn parse_batch_blobs_reader(
    expected: &[GitOid],
    reader: impl BufRead,
    per_blob_limit: usize,
    total_limit: usize,
) -> Result<BTreeMap<GitOid, Vec<u8>>, GitEngineError> {
    let mut blobs = BTreeMap::new();
    visit_batch_blobs_reader(
        expected,
        reader,
        per_blob_limit,
        total_limit,
        &mut |oid, data| {
            blobs.insert(oid.clone(), data);
            Ok(())
        },
    )?;
    Ok(blobs)
}

#[cfg(test)]
fn visit_batch_blobs_reader(
    expected: &[GitOid],
    reader: impl BufRead,
    per_blob_limit: usize,
    total_limit: usize,
    visitor: &mut GitBlobVisitor<'_>,
) -> Result<(), GitEngineError> {
    visit_batch_objects_reader(
        expected,
        reader,
        "blob",
        per_blob_limit,
        total_limit,
        visitor,
    )
}

fn visit_batch_objects_reader(
    expected: &[GitOid],
    mut reader: impl BufRead,
    expected_kind: &str,
    per_blob_limit: usize,
    total_limit: usize,
    visitor: &mut GitBlobVisitor<'_>,
) -> Result<(), GitEngineError> {
    const OPERATION: &str = "read Git blobs in a batch";
    let mut total = 0_usize;
    for expected_oid in expected {
        let mut header = Vec::new();
        let header_bytes = (&mut reader)
            .take(1025)
            .read_until(b'\n', &mut header)
            .map_err(GitEngineError::Io)?;
        if header_bytes == 0 || !header.ends_with(b"\n") {
            return Err(GitEngineError::InvalidOutput {
                operation: OPERATION,
                detail: "batch response omitted an object header terminator".to_string(),
            });
        }
        if header.len() > 1024 {
            return Err(GitEngineError::InvalidOutput {
                operation: OPERATION,
                detail: "batch response object header exceeds 1024 bytes".to_string(),
            });
        }
        header.pop();
        let header =
            std::str::from_utf8(&header).map_err(|error| GitEngineError::InvalidOutput {
                operation: OPERATION,
                detail: error.to_string(),
            })?;
        let mut fields = header.split_whitespace();
        let oid = fields.next().unwrap_or_default();
        let kind = fields.next().unwrap_or_default();
        let size = fields.next().unwrap_or_default();
        if fields.next().is_some() || oid != expected_oid.as_str() || kind != expected_kind {
            return Err(GitEngineError::InvalidOutput {
                operation: OPERATION,
                detail: format!("unexpected batch object header `{header}`"),
            });
        }
        let size = size
            .parse::<usize>()
            .map_err(|error| GitEngineError::InvalidOutput {
                operation: OPERATION,
                detail: format!("invalid blob size in batch header: {error}"),
            })?;
        if size > per_blob_limit {
            return Err(blob_limit_error(expected_oid, per_blob_limit));
        }
        total = total
            .checked_add(size)
            .ok_or_else(|| GitEngineError::InvalidOutput {
                operation: OPERATION,
                detail: "aggregate blob size overflowed".to_string(),
            })?;
        if total > total_limit {
            return Err(blob_batch_limit_error(total_limit));
        }
        let mut data = vec![0_u8; size];
        reader
            .read_exact(&mut data)
            .map_err(|error| GitEngineError::InvalidOutput {
                operation: OPERATION,
                detail: format!("batch response omitted blob content: {error}"),
            })?;
        let mut terminator = [0_u8; 1];
        if reader.read_exact(&mut terminator).is_err() || terminator[0] != b'\n' {
            return Err(GitEngineError::InvalidOutput {
                operation: OPERATION,
                detail: "batch response omitted a blob content terminator".to_string(),
            });
        }
        visitor(expected_oid, data)?;
    }
    if !reader.fill_buf().map_err(GitEngineError::Io)?.is_empty() {
        return Err(GitEngineError::InvalidOutput {
            operation: OPERATION,
            detail: "batch response contained unexpected trailing data".to_string(),
        });
    }
    Ok(())
}

fn blob_limit_error(oid: &GitOid, limit: usize) -> GitEngineError {
    GitEngineError::InvalidOutput {
        operation: "read Git blobs in a batch",
        detail: format!("blob `{oid}` exceeds the {limit} byte read limit"),
    }
}

fn blob_batch_limit_error(limit: usize) -> GitEngineError {
    GitEngineError::InvalidOutput {
        operation: "read Git blobs in a batch",
        detail: format!("aggregate blob contents exceed the {limit} byte read limit"),
    }
}

fn parse_rebase_value(value: &str, operation: &'static str) -> Result<PullRebase, GitEngineError> {
    match value {
        "false" | "no" | "off" | "0" => Ok(PullRebase::Never),
        "true" | "yes" | "on" | "1" => Ok(PullRebase::Yes),
        "merges" | "preserve" => Ok(PullRebase::Merges),
        "interactive" => Ok(PullRebase::Interactive),
        _ => Err(GitEngineError::InvalidOutput {
            operation,
            detail: format!("unsupported pull rebase value `{value}`"),
        }),
    }
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

fn canonical_work_tree(value: String) -> Result<PathBuf, GitEngineError> {
    let path = absolute_path(value)?;
    path.canonicalize()
        .map_err(|error| GitEngineError::InvalidOutput {
            operation: "discover repository paths",
            detail: format!(
                "cannot canonicalize discovered worktree `{}`: {error}",
                path.display()
            ),
        })
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

    #[test]
    fn stderr_redactions_replace_every_occurrence_in_order() {
        let engine = GitCliEngine::default()
            .with_stderr_redaction("/data/device/id_ed25519", "<device key>")
            .with_stderr_redaction("/data/device", "<device key directory>")
            .with_stderr_redaction("", "ignored");
        let redacted = engine.redact_stderr(
            b"Load key \"/data/device/id_ed25519\": bad permissions\n/data/device/x and /data/device/id_ed25519.pub\n"
                .to_vec(),
        );
        assert_eq!(
            String::from_utf8(redacted).expect("utf-8"),
            "Load key \"<device key>\": bad permissions\n<device key directory>/x and <device key>.pub\n"
        );
        assert_eq!(
            GitCliEngine::default().redact_stderr(b"plain".to_vec()),
            b"plain"
        );
        assert_eq!(replace_bytes(b"ab", b"abc", b"x"), b"ab");
    }

    #[test]
    fn ssh_command_overrides_ambient_git_ssh_environment() {
        let plain = GitCliEngine::default().command();
        assert!(plain
            .get_envs()
            .all(|(key, _)| key != "GIT_SSH_COMMAND" && key != "GIT_SSH"));

        let bound = GitCliEngine::default().with_ssh_command("ssh -i '/k' -o IdentitiesOnly=yes");
        let command = bound.command();
        let envs = command.get_envs().collect::<Vec<_>>();
        assert!(envs.contains(&(
            OsStr::new("GIT_SSH_COMMAND"),
            Some(OsStr::new("ssh -i '/k' -o IdentitiesOnly=yes"))
        )));
        assert!(envs.contains(&(OsStr::new("GIT_SSH"), None)));
        assert_ne!(bound, GitCliEngine::default());
    }
    use super::*;
    use std::fs;

    #[cfg(windows)]
    #[test]
    fn windows_git_children_are_created_without_a_console_window() {
        // Keep this in sync with CREATE_NO_WINDOW in the Windows SDK. The
        // shared spawn boundary applies it to every GitCliEngine operation.
        assert_eq!(WINDOWS_GIT_CREATION_FLAGS, 0x0800_0000);
    }

    #[test]
    fn parses_bounded_remote_reference_lists_and_rejects_namespace_escape() {
        let prefix =
            GitRefName::parse("refs/heads/__vulcan-sync/devices/profile").expect("prefix ref");
        let oid = "0123456789012345678901234567890123456789";
        let input = format!(
            "{oid}\t{}/device-b\n{oid}\t{}/device-a\n",
            prefix.as_str(),
            prefix.as_str()
        );
        let parsed = parse_remote_reference_list(input.as_bytes(), &prefix).expect("remote refs");
        assert_eq!(parsed.len(), 2);
        assert!(parsed[0].name.as_str().ends_with("device-a"));
        assert!(parse_remote_reference_list(
            format!("{oid}\trefs/heads/main\n").as_bytes(),
            &prefix
        )
        .is_err());
        assert!(parse_remote_reference_list(
            format!("{oid}\t{prefix}/device-a\n{oid}\t{prefix}/device-a\n").as_bytes(),
            &prefix
        )
        .is_err());
    }

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
        run_git(path, &["config", "core.autocrlf", "false"]);
        run_git(path, &["config", "pull.rebase", "false"]);
        run_git(path, &["config", "pull.ff", "true"]);
    }

    fn commit_all(path: &Path, message: &str) -> GitOid {
        run_git(path, &["add", "--all", "--", "."]);
        run_git(path, &["commit", "--quiet", "-m", message]);
        GitOid::parse(run_git_capture(path, &["rev-parse", "HEAD"]))
            .expect("HEAD should be an object ID")
    }

    fn init_branch_remote() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let temporary = tempfile::TempDir::new().expect("temporary directory");
        let remote = temporary.path().join("remote.git");
        run_git(
            temporary.path(),
            &[
                "init",
                "--quiet",
                "--bare",
                remote.to_str().expect("remote path"),
            ],
        );
        let writer = temporary.path().join("writer");
        fs::create_dir(&writer).expect("writer directory");
        init_repo(&writer);
        run_git(
            &writer,
            &["remote", "add", "origin", remote.to_str().expect("remote")],
        );
        fs::write(writer.join("Home.md"), "base\n").expect("base note");
        commit_all(&writer, "base");
        run_git(&writer, &["push", "--quiet", "-u", "origin", "main"]);
        (temporary, remote, writer)
    }

    const OBSERVED_LIVE: &str = "refs/heads/__vulcan-sync/live";
    const OBSERVED_REGISTRATIONS: &str = "refs/heads/__vulcan-sync/registrations";

    fn registration_names(references: &[GitReference]) -> Vec<String> {
        references
            .iter()
            .map(|reference| reference.name.as_str().to_owned())
            .collect()
    }

    #[test]
    fn one_listing_returns_several_exact_tips_in_request_order() {
        let (_temporary, _remote, writer) = init_branch_remote();
        let engine = GitCliEngine::default();
        let repository = engine.discover_repository(&writer).expect("repository");
        let origin = GitRemote::parse("origin").expect("remote");
        let live = GitRefName::parse(OBSERVED_LIVE).expect("live ref");
        let device = GitRefName::parse("refs/heads/__vulcan-sync/devices/p/d").expect("device ref");
        let missing = GitRefName::parse("refs/heads/missing").expect("missing ref");
        let prefix = GitRefName::parse(OBSERVED_REGISTRATIONS).expect("prefix");
        run_git(
            &writer,
            &[
                "push",
                "--quiet",
                "origin",
                &format!("HEAD:{OBSERVED_LIVE}"),
                &format!("HEAD:{}", device.as_str()),
                &format!("HEAD:{OBSERVED_REGISTRATIONS}/a"),
            ],
        );
        let head = GitOid::parse(run_git_capture(&writer, &["rev-parse", "HEAD"])).expect("head");

        let (tips, registrations) = engine
            .remote_refs(
                &repository,
                &origin,
                &[&device, &missing, &live],
                Some(&prefix),
            )
            .expect("observation");
        assert_eq!(tips, vec![Some(head.clone()), None, Some(head.clone())]);
        assert_eq!(
            registration_names(&registrations.expect("registrations observed")),
            [format!("{OBSERVED_REGISTRATIONS}/a")]
        );

        let (tips, registrations) = engine
            .remote_refs(&repository, &origin, &[&live], None)
            .expect("observation without a namespace");
        assert_eq!(tips, vec![Some(head)]);
        assert_eq!(registrations, None);
    }

    fn advance_remote(temporary: &tempfile::TempDir, remote: &Path, name: &str, contents: &str) {
        let other = temporary.path().join(name);
        run_git(
            temporary.path(),
            &[
                "clone",
                "--quiet",
                "-b",
                "main",
                remote.to_str().expect("remote path"),
                other.to_str().expect("clone path"),
            ],
        );
        run_git(&other, &["config", "user.name", "Vulcan Test"]);
        run_git(&other, &["config", "user.email", "vulcan@example.invalid"]);
        fs::write(other.join("Home.md"), contents).expect("remote note");
        commit_all(&other, "remote advance");
        run_git(&other, &["push", "--quiet", "origin", "main"]);
    }

    fn branch_fixture(writer: &Path) -> (GitCliEngine, GitRepository, GitRemote, GitRefName) {
        let engine = GitCliEngine::default();
        let repository = engine.discover_repository(writer).expect("repository");
        let branch = engine
            .head_reference(&repository)
            .expect("head reference")
            .expect("checked-out branch");
        assert_eq!(branch.as_str(), "refs/heads/main");
        (
            engine,
            repository,
            GitRemote::parse("origin").expect("remote"),
            branch,
        )
    }

    #[test]
    fn branch_upstream_resolves_configured_tracking() {
        let (temporary, _remote, writer) = init_branch_remote();
        let (engine, repository, remote, branch) = branch_fixture(&writer);
        let upstream = engine
            .branch_upstream(&repository, &branch)
            .expect("upstream")
            .expect("tracking upstream");
        assert_eq!(upstream.remote, remote);
        assert_eq!(upstream.branch, branch);
        assert_eq!(upstream.merge_ref.as_str(), "refs/heads/main");
        assert_eq!(upstream.tracking_ref.as_str(), "refs/remotes/origin/main");

        let lonely = temporary.path().join("lonely");
        fs::create_dir(&lonely).expect("lonely directory");
        init_repo(&lonely);
        fs::write(lonely.join("Home.md"), "lonely\n").expect("lonely note");
        commit_all(&lonely, "lonely");
        let lonely_repo = engine.discover_repository(&lonely).expect("repository");
        let lonely_branch = engine
            .head_reference(&lonely_repo)
            .expect("head reference")
            .expect("checked-out branch");
        assert_eq!(
            engine
                .branch_upstream(&lonely_repo, &lonely_branch)
                .expect("upstream query"),
            None
        );
    }

    #[test]
    fn branch_upstream_uses_git_refspec_mapping() {
        let (_temporary, _remote, writer) = init_branch_remote();
        run_git(
            &writer,
            &[
                "config",
                "remote.origin.fetch",
                "+refs/heads/*:refs/custom/origin/*",
            ],
        );
        let (engine, repository, remote, branch) = branch_fixture(&writer);

        let upstream = engine
            .branch_upstream(&repository, &branch)
            .expect("upstream")
            .expect("tracking upstream");

        assert_eq!(upstream.remote, remote);
        assert_eq!(upstream.merge_ref.as_str(), "refs/heads/main");
        assert_eq!(upstream.tracking_ref.as_str(), "refs/custom/origin/main");
    }

    #[test]
    fn branch_upstream_supports_the_local_dot_remote() {
        let temporary = TempDir::new().expect("temporary directory");
        init_repo(temporary.path());
        fs::write(temporary.path().join("Home.md"), "home\n").expect("note");
        commit_all(temporary.path(), "initial");
        run_git(temporary.path(), &["branch", "local-base"]);
        run_git(temporary.path(), &["config", "branch.main.remote", "."]);
        run_git(
            temporary.path(),
            &["config", "branch.main.merge", "refs/heads/local-base"],
        );
        let engine = GitCliEngine::default();
        let repository = engine
            .discover_repository(temporary.path())
            .expect("repository");
        let branch = GitRefName::parse("refs/heads/main").expect("branch");

        let upstream = engine
            .branch_upstream(&repository, &branch)
            .expect("upstream")
            .expect("local upstream");

        assert_eq!(upstream.remote.as_str(), ".");
        assert_eq!(upstream.merge_ref.as_str(), "refs/heads/local-base");
        assert_eq!(upstream.tracking_ref.as_str(), "refs/heads/local-base");
    }

    #[test]
    fn branch_pull_config_honors_branch_over_global() {
        let (_temporary, _remote, writer) = init_branch_remote();
        let (engine, repository, _remote, branch) = branch_fixture(&writer);
        assert_eq!(
            engine
                .branch_pull_config(&repository, &branch)
                .expect("pull config"),
            BranchPullConfig {
                fast_forward: PullFastForward::Always,
                rebase: PullRebase::Never,
            }
        );
        run_git(&writer, &["config", "pull.rebase", "true"]);
        run_git(&writer, &["config", "pull.ff", "only"]);
        assert_eq!(
            engine
                .branch_pull_config(&repository, &branch)
                .expect("pull config"),
            BranchPullConfig {
                fast_forward: PullFastForward::Only,
                rebase: PullRebase::Yes,
            }
        );
        run_git(&writer, &["config", "branch.main.rebase", "false"]);
        run_git(&writer, &["config", "branch.main.rebase", "merges"]);
        assert_eq!(
            engine
                .branch_pull_config(&repository, &branch)
                .expect("pull config")
                .rebase,
            PullRebase::Merges
        );
        run_git(&writer, &["config", "pull.ff", "bogus"]);
        assert!(engine.branch_pull_config(&repository, &branch).is_err());
    }

    #[test]
    fn fast_forward_advances_and_reports_state() {
        let (temporary, remote, writer) = init_branch_remote();
        let (engine, repository, remote_name, branch) = branch_fixture(&writer);
        let upstream = engine
            .branch_upstream(&repository, &branch)
            .expect("upstream")
            .expect("tracking upstream");
        advance_remote(&temporary, &remote, "other", "advanced\n");
        let fetched = engine
            .fetch_ref(
                &repository,
                &remote_name,
                &upstream.merge_ref,
                &upstream.tracking_ref,
            )
            .expect("fetch branch");
        assert_eq!(
            engine
                .fast_forward_branch(&repository, &branch, &fetched)
                .expect("fast-forward"),
            FastForwardOutcome::Advanced
        );
        assert_eq!(
            engine.head_commit(&repository).expect("head"),
            Some(fetched.clone())
        );
        assert_eq!(
            engine
                .fast_forward_branch(&repository, &branch, &fetched)
                .expect("repeat fast-forward"),
            FastForwardOutcome::UpToDate
        );
    }

    #[test]
    fn fast_forward_refuses_divergence_and_dirty_overlap() {
        let (temporary, remote, writer) = init_branch_remote();
        let (engine, repository, remote_name, branch) = branch_fixture(&writer);
        let upstream = engine
            .branch_upstream(&repository, &branch)
            .expect("upstream")
            .expect("tracking upstream");
        fs::write(writer.join("Home.md"), "local\n").expect("local note");
        commit_all(&writer, "local");
        advance_remote(&temporary, &remote, "other", "advanced\n");
        let fetched = engine
            .fetch_ref(
                &repository,
                &remote_name,
                &upstream.merge_ref,
                &upstream.tracking_ref,
            )
            .expect("fetch branch");
        assert_eq!(
            engine
                .fast_forward_branch(&repository, &branch, &fetched)
                .expect("diverged fast-forward"),
            FastForwardOutcome::NotFastForwardable
        );
        assert_ne!(
            engine.head_commit(&repository).expect("head"),
            Some(fetched)
        );

        let (temporary, remote, clean) = init_branch_remote();
        let (engine, repository, remote_name, branch) = branch_fixture(&clean);
        let upstream = engine
            .branch_upstream(&repository, &branch)
            .expect("upstream")
            .expect("tracking upstream");
        advance_remote(&temporary, &remote, "other", "advanced\n");
        fs::write(clean.join("Home.md"), "dirty\n").expect("dirty note");
        let fetched = engine
            .fetch_ref(
                &repository,
                &remote_name,
                &upstream.merge_ref,
                &upstream.tracking_ref,
            )
            .expect("fetch branch");
        assert_eq!(
            engine
                .fast_forward_branch(&repository, &branch, &fetched)
                .expect("dirty fast-forward"),
            FastForwardOutcome::BlockedDirty
        );
        assert_ne!(
            engine.head_commit(&repository).expect("head"),
            Some(fetched)
        );
    }

    /// A checkout whose worktree already holds an upstream advance, plus
    /// the old and new branch tips.
    fn checkout_holding_upstream_advance() -> (
        tempfile::TempDir,
        PathBuf,
        GitCliEngine,
        GitRepository,
        GitRefName,
        GitOid,
        GitOid,
    ) {
        let (temporary, remote, writer) = init_branch_remote();
        let (engine, repository, remote_name, branch) = branch_fixture(&writer);
        let before = engine.head_commit(&repository).expect("head").expect("tip");
        advance_remote(&temporary, &remote, "other", "advanced\n");
        let upstream = engine
            .branch_upstream(&repository, &branch)
            .expect("upstream")
            .expect("tracking upstream");
        let fetched = engine
            .fetch_ref(
                &repository,
                &remote_name,
                &upstream.merge_ref,
                &upstream.tracking_ref,
            )
            .expect("fetch branch");
        fs::write(writer.join("Home.md"), "advanced\n").expect("synchronized note");
        (
            temporary, writer, engine, repository, branch, before, fetched,
        )
    }

    fn short_status(writer: &Path) -> String {
        let output = Command::new("git")
            .current_dir(writer)
            .args(["status", "--short"])
            .output()
            .expect("status");
        String::from_utf8(output.stdout).expect("status text")
    }

    #[test]
    fn advance_checked_out_branch_moves_index_without_touching_the_worktree() {
        let (_temporary, writer, engine, repository, branch, before, fetched) =
            checkout_holding_upstream_advance();
        assert_eq!(
            engine
                .fast_forward_branch(&repository, &branch, &fetched)
                .expect("ordinary fast-forward"),
            FastForwardOutcome::BlockedDirty
        );
        fs::write(writer.join("Local.md"), "local\n").expect("untracked note");
        fs::write(writer.join("Staged.md"), "staged\n").expect("staged note");
        run_git(&writer, &["add", "Staged.md"]);
        assert_eq!(
            engine
                .advance_checked_out_branch(&repository, &branch, &before, &fetched)
                .expect("advance"),
            BranchAdvanceOutcome::Advanced
        );
        assert_eq!(
            engine.head_commit(&repository).expect("head"),
            Some(fetched.clone())
        );
        assert_eq!(
            fs::read_to_string(writer.join("Home.md")).expect("note"),
            "advanced\n"
        );
        assert_eq!(short_status(&writer), "A  Staged.md\n?? Local.md\n");
        assert_eq!(
            engine
                .advance_checked_out_branch(&repository, &branch, &before, &fetched)
                .expect("stale advance"),
            BranchAdvanceOutcome::Stale
        );
    }

    #[test]
    fn advance_checked_out_branch_refuses_conflicting_staged_entries() {
        let (_temporary, writer, engine, repository, branch, before, fetched) =
            checkout_holding_upstream_advance();
        fs::write(writer.join("Home.md"), "staged elsewhere\n").expect("staged note");
        run_git(&writer, &["add", "Home.md"]);
        assert_eq!(
            engine
                .advance_checked_out_branch(&repository, &branch, &before, &fetched)
                .expect("advance"),
            BranchAdvanceOutcome::BlockedStaged
        );
        assert_eq!(engine.head_commit(&repository).expect("head"), Some(before));
        assert_eq!(short_status(&writer), "M  Home.md\n");
    }

    #[test]
    fn merge_branch_refuses_rather_than_clobbers_staged_work() {
        let (temporary, remote, writer) = init_branch_remote();
        let (engine, repository, remote_name, branch) = branch_fixture(&writer);
        let upstream = engine
            .branch_upstream(&repository, &branch)
            .expect("upstream")
            .expect("tracking upstream");
        fs::write(writer.join("Local.md"), "local\n").expect("local note");
        commit_all(&writer, "local");
        advance_remote(&temporary, &remote, "other", "advanced\n");
        fs::write(writer.join("Staged.md"), "staged\n").expect("staged note");
        run_git(&writer, &["add", "Staged.md"]);
        let staged_before = run_git_capture(&writer, &["rev-parse", ":Staged.md"]);
        let fetched = engine
            .fetch_ref(
                &repository,
                &remote_name,
                &upstream.merge_ref,
                &upstream.tracking_ref,
            )
            .expect("fetch branch");
        assert_eq!(
            engine
                .merge_branch(&repository, &fetched, false)
                .expect("merge with staged work"),
            MergeBranchOutcome::BlockedDirty
        );
        assert_eq!(
            run_git_capture(&writer, &["rev-parse", ":Staged.md"]),
            staged_before,
            "refused merge must leave staged work alone"
        );
        assert!(writer.join("Staged.md").exists());
    }

    #[test]
    fn merge_branch_merges_cleanly_and_reports_conflicts() {
        let (temporary, remote, writer) = init_branch_remote();
        let (engine, repository, remote_name, branch) = branch_fixture(&writer);
        let upstream = engine
            .branch_upstream(&repository, &branch)
            .expect("upstream")
            .expect("tracking upstream");
        fs::write(writer.join("Local.md"), "local\n").expect("local note");
        commit_all(&writer, "local");
        advance_remote(&temporary, &remote, "other", "advanced\n");
        let fetched = engine
            .fetch_ref(
                &repository,
                &remote_name,
                &upstream.merge_ref,
                &upstream.tracking_ref,
            )
            .expect("fetch branch");
        assert_eq!(
            engine
                .merge_branch(&repository, &fetched, false)
                .expect("merge"),
            MergeBranchOutcome::Merged
        );
        assert!(writer.join("Local.md").exists());
        assert_eq!(
            fs::read_to_string(writer.join("Home.md")).expect("merged note"),
            "advanced\n"
        );

        fs::write(writer.join("Home.md"), "local conflict\n").expect("conflict note");
        commit_all(&writer, "local conflict");
        advance_remote(&temporary, &remote, "second", "remote conflict\n");
        let conflicted = engine
            .fetch_ref(
                &repository,
                &remote_name,
                &upstream.merge_ref,
                &upstream.tracking_ref,
            )
            .expect("fetch branch");
        assert_eq!(
            engine
                .merge_branch(&repository, &conflicted, false)
                .expect("conflicting merge"),
            MergeBranchOutcome::Conflicted
        );
        assert!(repository.git_dir.join("MERGE_HEAD").exists());

        assert!(engine
            .abort_branch_operation(&repository)
            .expect("abort merge"));
        assert!(!repository.git_dir.join("MERGE_HEAD").exists());
        assert_eq!(
            fs::read_to_string(writer.join("Home.md")).expect("restored note"),
            "local conflict\n"
        );
        assert!(!engine
            .abort_branch_operation(&repository)
            .expect("nothing left to abort"));
    }

    #[test]
    fn rebase_branch_never_stashes_implicitly() {
        let (temporary, remote, writer) = init_branch_remote();
        let (engine, repository, remote_name, branch) = branch_fixture(&writer);
        let upstream = engine
            .branch_upstream(&repository, &branch)
            .expect("upstream")
            .expect("tracking upstream");
        fs::write(writer.join("Local.md"), "local\n").expect("local note");
        commit_all(&writer, "local");
        advance_remote(&temporary, &remote, "other", "advanced\n");
        run_git(&writer, &["config", "rebase.autostash", "true"]);
        fs::write(writer.join("Home.md"), "dirty\n").expect("dirty note");
        let fetched = engine
            .fetch_ref(
                &repository,
                &remote_name,
                &upstream.merge_ref,
                &upstream.tracking_ref,
            )
            .expect("fetch branch");
        assert_eq!(
            engine
                .rebase_branch(&repository, &fetched, false)
                .expect("dirty rebase"),
            RebaseOutcome::BlockedDirty
        );
        assert_eq!(
            run_git_capture(&writer, &["stash", "list"]),
            "",
            "rebase.autostash configuration must not stash in automation"
        );
    }

    #[test]
    fn rebase_branch_replays_and_reports_conflicts() {
        let (temporary, remote, writer) = init_branch_remote();
        let (engine, repository, remote_name, branch) = branch_fixture(&writer);
        let upstream = engine
            .branch_upstream(&repository, &branch)
            .expect("upstream")
            .expect("tracking upstream");
        let base = engine
            .head_commit(&repository)
            .expect("head")
            .expect("base commit");
        fs::write(writer.join("Local.md"), "local\n").expect("local note");
        commit_all(&writer, "local");
        advance_remote(&temporary, &remote, "other", "advanced\n");
        let fetched = engine
            .fetch_ref(
                &repository,
                &remote_name,
                &upstream.merge_ref,
                &upstream.tracking_ref,
            )
            .expect("fetch branch");
        assert_eq!(
            engine
                .rebase_branch(&repository, &fetched, false)
                .expect("rebase"),
            RebaseOutcome::Rebased
        );
        let rebased = engine
            .head_commit(&repository)
            .expect("head")
            .expect("head commit");
        assert_ne!(rebased, base);
        assert!(writer.join("Local.md").exists());
        assert_eq!(
            fs::read_to_string(writer.join("Home.md")).expect("rebased note"),
            "advanced\n"
        );
        assert_eq!(
            engine
                .rebase_branch(&repository, &rebased, false)
                .expect("repeat rebase"),
            RebaseOutcome::UpToDate
        );
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
    fn cloned_engines_share_successful_installation_probe() {
        let engine = GitCliEngine::default();
        let installation = engine.installation().expect("first installation probe");
        let mut clone = engine.clone().with_command_timeout(Duration::from_secs(1));
        clone.executable = PathBuf::from("definitely-not-a-vulcan-git-executable");
        assert_eq!(
            clone.installation().expect("shared installation result"),
            installation
        );
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
            repository
                .git_dir
                .canonicalize()
                .expect("canonical discovered Git directory"),
            temporary
                .path()
                .join(".git")
                .canonicalize()
                .expect("canonical Git directory")
        );
        assert_eq!(repository.common_dir, repository.git_dir);
    }

    #[test]
    fn discovered_work_tree_uses_the_platform_canonical_form() {
        let temporary = TempDir::new().expect("temporary directory");
        let discovered = canonical_work_tree(temporary.path().to_string_lossy().into_owned())
            .expect("existing absolute path");

        assert_eq!(
            discovered,
            temporary.path().canonicalize().expect("canonical path")
        );
    }

    #[cfg(unix)]
    #[test]
    fn prospective_paths_accept_equivalent_parent_aliases() {
        let temporary = TempDir::new().expect("temporary directory");
        let actual_parent = temporary.path().join("actual");
        let alias_parent = temporary.path().join("alias");
        fs::create_dir(&actual_parent).expect("actual parent");
        std::os::unix::fs::symlink(&actual_parent, &alias_parent).expect("parent alias");

        assert!(same_prospective_path(
            &actual_parent.join("missing.git"),
            &alias_parent.join("missing.git")
        ));
        assert!(!same_prospective_path(
            &actual_parent.join("missing.git"),
            &alias_parent.join("other.git")
        ));
    }

    #[cfg(windows)]
    #[test]
    fn git_cli_paths_remove_windows_verbatim_prefixes() {
        assert_eq!(
            git_cli_path(Path::new(r"\\?\C:\Users\vulcan\wiki")),
            OsString::from(r"C:\Users\vulcan\wiki")
        );
        assert_eq!(
            git_cli_path(Path::new(r"\\?\UNC\server\share\wiki")),
            OsString::from(r"\\server\share\wiki")
        );
        assert_eq!(
            git_cli_path(Path::new(r"C:\Users\vulcan\wiki")),
            OsString::from(r"C:\Users\vulcan\wiki")
        );
        assert_eq!(
            git_cli_path(Path::new(
                r"\\?\Volume{01234567-89ab-cdef-0123-456789abcdef}\wiki"
            )),
            OsString::from(r"\\?\Volume{01234567-89ab-cdef-0123-456789abcdef}\wiki")
        );
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
            repository
                .git_dir
                .canonicalize()
                .expect("canonical discovered Git directory"),
            git_dir.canonicalize().expect("canonical Git directory")
        );
    }

    #[test]
    #[cfg(unix)]
    fn clone_repairs_ownership_only_for_its_new_worktree() {
        for detached in [false, true] {
            let temporary = TempDir::new().expect("temporary directory");
            let source = temporary.path().join("source");
            fs::create_dir(&source).expect("source directory");
            init_repo(&source);
            fs::write(source.join("Home.md"), "# Home\n").expect("note");
            commit_all(&source, "initial");
            // Keep the real Git ownership check, but simulate Android ownership
            // without chown, root, process-global environment changes, or touching
            // the developer's Git configuration.
            let wrapper = temporary.path().join("git-wrapper");
            // Execute an existing, immutable fixture through a per-test symlink.
            // Writing an executable here can race with parallel fork/exec calls
            // that briefly inherit its writable descriptor, causing ETXTBSY.
            std::os::unix::fs::symlink(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/fixtures/git-ownership-wrapper.sh"),
                &wrapper,
            )
            .expect("wrapper symlink");
            let engine = GitCliEngine::new(&wrapper);
            let global = temporary.path().join("global.config");
            fs::write(&global, "[safe]\n\tdirectory = /unrelated/trusted\n").expect("config");
            let original_config = fs::read(&global).expect("original config");
            let request = |name: &str| GitCloneRequest {
                source: source.display().to_string(),
                work_tree: temporary.path().join(name),
                git_dir: detached.then(|| temporary.path().join(format!("{name}.git"))),
                platform: GitPlatformProfile::AndroidShared,
            };
            engine
                .clone_repository(&request("normal"))
                .expect("normal clone");
            assert_eq!(fs::read(&global).expect("config"), original_config);

            fs::write(temporary.path().join("different-owner"), "").expect("ownership marker");
            assert!(engine.discover_repository(&source).is_err());
            assert_eq!(fs::read(&global).expect("config"), original_config);
            let mut invalid = request("failed");
            invalid.source = temporary.path().join("missing").display().to_string();
            assert!(engine.clone_repository(&invalid).is_err());
            assert_eq!(fs::read(&global).expect("config"), original_config);

            let clone = request("shared vault ' quoted");
            let repository = engine.clone_repository(&clone).expect("ownership repaired");
            let expected = clone.work_tree.canonicalize().expect("canonical worktree");
            assert_eq!(repository.work_tree, Some(expected.clone()));
            assert_eq!(
                engine
                    .capture(
                        "read trust entries",
                        None,
                        ["config", "--global", "--get-all", "safe.directory"]
                    )
                    .expect("trust entries"),
                format!("/unrelated/trusted\n{}\n", expected.display())
            );
            let repaired_config = fs::read(&global).expect("config");
            GitCliEngine::new(&wrapper)
                .discover_repository(&clone.work_tree)
                .expect("trust persists");
            assert_eq!(fs::read(&global).expect("config"), repaired_config);
            assert!(engine.discover_repository(&source).is_err());

            let literal_star = request("*");
            assert!(matches!(
                engine.clone_repository(&literal_star),
                Err(GitEngineError::UnsupportedRepository { .. })
            ));
            assert!(engine.discover_repository(&source).is_err());
            assert_eq!(fs::read(&global).expect("config"), repaired_config);
        }
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
        assert_eq!(
            run_git_capture(&colocated, &["config", "--get", "core.autocrlf"]),
            "false"
        );
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
        assert_eq!(
            run_git_capture(&detached, &["config", "--get", "core.autocrlf"]),
            "false"
        );
    }

    #[test]
    fn detached_recovery_refuses_a_mismatched_pointer_without_mutation() {
        let temporary = TempDir::new().expect("temporary directory");
        let work_tree = temporary.path().join("wiki");
        fs::create_dir(&work_tree).expect("worktree");
        let git_dir = temporary.path().join("private/wiki.git");
        fs::write(
            work_tree.join(".git"),
            format!("gitdir: {}\n", temporary.path().join("other.git").display()),
        )
        .expect("stale pointer");

        let error = GitCliEngine::default()
            .recover_detached_repository(&GitDetachedRecoveryRequest {
                source: "https://example.invalid/wiki.git".to_string(),
                work_tree,
                git_dir: git_dir.clone(),
                platform: GitPlatformProfile::AndroidShared,
            })
            .expect_err("mismatched pointer must fail");

        assert!(error
            .to_string()
            .contains("refusing destructive reattachment"));
        assert!(!git_dir.exists());
    }

    #[test]
    fn platform_policies_are_explicit_and_serializable() {
        let native = GitPlatformProfile::LinuxNative.policy();
        assert_eq!(native.executable_bits, GitExecutableBitsPolicy::GitProbed);
        assert_eq!(native.clone_config.len(), 1);
        assert_eq!(native.clone_config[0].key, "core.autocrlf");
        assert_eq!(native.clone_config[0].value, "false");

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
        assert_eq!(android.clone_config.len(), 3);
        assert_eq!(android.clone_config[0].key, "core.autocrlf");
        assert_eq!(android.clone_config[0].value, "false");
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

    #[cfg(unix)]
    #[test]
    fn command_timeout_terminates_the_spawned_process_group() {
        let timeout = Duration::from_millis(50);
        let engine = GitCliEngine::default().with_command_timeout(timeout);
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 10"]);
        let started = Instant::now();

        let error = engine
            .execute(command)
            .expect_err("the command should time out");

        assert!(matches!(
            error,
            GitEngineError::CommandTimedOut {
                operation: "run a Git command",
                timeout: observed,
            } if observed == timeout
        ));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn command_timeout_has_a_bounded_default_and_nonzero_floor() {
        assert_eq!(
            GitCliEngine::default().command_timeout(),
            Duration::from_secs(300)
        );
        assert_eq!(
            GitCliEngine::default()
                .with_command_timeout(Duration::ZERO)
                .command_timeout(),
            Duration::from_millis(1)
        );
    }

    #[test]
    fn concurrent_command_waits_preserve_completion_and_output() {
        std::thread::scope(|scope| {
            let workers = (0..8)
                .map(|_| {
                    scope.spawn(|| {
                        let engine =
                            GitCliEngine::default().with_command_timeout(Duration::from_secs(5));
                        let mut command = Command::new("git");
                        command.arg("--version");
                        let result = engine.execute(command).unwrap();
                        assert!(result.status.success());
                        assert!(String::from_utf8(result.stdout)
                            .unwrap()
                            .starts_with("git version"));
                    })
                })
                .collect::<Vec<_>>();
            for worker in workers {
                worker.join().unwrap();
            }
        });
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
    fn repository_commands_scrub_repository_and_config_environment() {
        let engine = GitCliEngine::default();
        let command = engine.command();
        let scrubbed = command
            .get_envs()
            .filter(|(_, value)| value.is_none())
            .map(|(key, _)| key.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        for variable in [
            "GIT_DIR",
            "GIT_INDEX_FILE",
            "GIT_WORK_TREE",
            "GIT_CONFIG_PARAMETERS",
            "GIT_CONFIG_COUNT",
            "GIT_CONFIG",
            "GIT_CONFIG_GLOBAL",
            "GIT_CONFIG_SYSTEM",
            "GIT_CONFIG_NOSYSTEM",
            "XDG_CONFIG_HOME",
        ] {
            assert!(
                scrubbed.iter().any(|key| key == variable),
                "missing scrubbed environment variable {variable}"
            );
        }
    }

    #[test]
    fn detached_pointer_rejects_control_characters_in_paths() {
        let temporary = TempDir::new().expect("temporary directory");
        let error = write_gitdir_pointer(temporary.path(), Path::new("/tmp/vulcan\nnewline"))
            .expect_err("control characters must be rejected");
        assert!(error.to_string().contains("single-line"));
        assert!(!temporary.path().join(".git").exists());
    }

    #[test]
    fn capture_commands_anchor_at_the_complete_worktree_root() {
        let temporary = TempDir::new().expect("temporary directory");
        init_repo(temporary.path());
        fs::create_dir(temporary.path().join("sub")).expect("subdirectory");
        fs::write(temporary.path().join("root.md"), "initial\n").expect("root note");
        fs::write(temporary.path().join("sub/note.md"), "initial\n").expect("nested note");
        let head = commit_all(temporary.path(), "initial");
        fs::write(temporary.path().join("root.md"), "updated\n").expect("root update");
        fs::write(temporary.path().join("sub/note.md"), "updated\n").expect("nested update");
        let engine = GitCliEngine::default();
        let repository = engine
            .discover_repository(temporary.path())
            .expect("repository");
        assert_eq!(
            engine.repository_command(&repository).get_current_dir(),
            repository.work_tree.as_deref(),
            "repository commands must not inherit a caller subdirectory"
        );
        let snapshot = engine
            .snapshot_worktree_tree(&repository, Some(&head))
            .expect("root-anchored snapshot");
        let local_ref = GitRefName::parse("refs/vulcan/sync/cwd/live").expect("ref");
        let capture = engine
            .capture_worktree(
                &repository,
                &GitCaptureRequest {
                    base: Some(head.clone()),
                    target_ref: local_ref,
                    target_before: Some(head.clone()),
                    message: "vulcan sync snapshot\n".to_string(),
                    link: None,
                },
            )
            .expect("root-anchored capture");
        let unchanged = engine
            .worktree_matches_tree(&repository, &capture.commit)
            .expect("root-anchored worktree comparison");

        let root_object = engine
            .path_object(&repository, &capture.commit, "root.md")
            .expect("root object query")
            .expect("root note in the capture");
        let nested_object = engine
            .path_object(&repository, &capture.commit, "sub/note.md")
            .expect("nested object query")
            .expect("nested note in the capture");

        assert!(capture.created);
        assert!(unchanged);
        assert_ne!(
            snapshot,
            engine.tree_oid(&repository, &head).expect("head tree")
        );
        assert_eq!(capture.tree, snapshot);
        // The discriminative assertion: changes outside the caller's cwd
        // must still enter the capture.
        assert_eq!(root_object.data.as_deref(), Some(b"updated\n".as_slice()));
        assert_eq!(nested_object.data.as_deref(), Some(b"updated\n".as_slice()));
    }

    #[test]
    fn captures_with_uncommitted_edits_do_not_rehash_unchanged_files() {
        let temporary = TempDir::new().expect("temporary directory");
        let root = temporary.path();
        init_repo(root);
        // A clean filter that counts its runs stands in for Git LFS.
        let counter = root.join(".git").join("clean-runs");
        run_git(
            root,
            &[
                "config",
                "filter.count.clean",
                &format!("sh -c 'echo x >> \"{}\"; cat'", counter.display()),
            ],
        );
        run_git(root, &["config", "filter.count.smudge", "cat"]);
        fs::write(root.join(".gitattributes"), "*.bin filter=count\n").expect("attributes");
        for name in ["a.bin", "b.bin", "c.bin"] {
            fs::write(root.join(name), name).expect("media file");
        }
        let head = commit_all(root, "initial");
        // Uncommitted edits keep the sync index from matching the base.
        fs::write(root.join("Notes.md"), "draft\n").expect("draft");
        // Old timestamps keep Git's racy-entry check out of the count.
        run_git(root, &["status", "--short"]);
        Command::new("touch")
            .args(["-d", "2 hours ago", "a.bin", "b.bin", "c.bin", "Notes.md"])
            .current_dir(root)
            .status()
            .expect("touch");
        let engine = GitCliEngine::default();
        let repository = engine.discover_repository(root).expect("repository");
        let runs = || fs::read_to_string(&counter).map_or(0, |text| text.lines().count());
        let first = engine
            .snapshot_worktree_tree(&repository, Some(&head))
            .expect("first capture");
        let after_first = runs();
        let second = engine
            .snapshot_worktree_tree(&repository, Some(&head))
            .expect("second capture");
        assert_eq!(first, second);
        assert_eq!(
            runs(),
            after_first,
            "the second capture re-ran the clean filter on unchanged files"
        );
        // A changed file is still read again.
        fs::write(root.join("b.bin"), "changed").expect("change");
        let third = engine
            .snapshot_worktree_tree(&repository, Some(&head))
            .expect("third capture");
        assert_ne!(third, second);
        assert!(runs() > after_first);
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
        let refs_before = run_git_capture(
            temporary.path(),
            &["for-each-ref", "--format=%(refname) %(objectname)"],
        );
        let snapshot_tree = engine
            .snapshot_worktree_tree(&repository, Some(&head))
            .expect("tree-only snapshot");
        assert_ne!(
            snapshot_tree,
            engine.tree_oid(&repository, &head).expect("head tree")
        );
        assert_eq!(
            run_git_capture(
                temporary.path(),
                &["for-each-ref", "--format=%(refname) %(objectname)"],
            ),
            refs_before
        );
        assert_eq!(
            run_git_capture(temporary.path(), &["write-tree"]),
            normal_index_before
        );

        let capture = engine
            .capture_worktree(
                &repository,
                &GitCaptureRequest {
                    base: Some(head),
                    target_ref: local_ref.clone(),
                    target_before: None,
                    message: "vulcan sync snapshot\n".to_string(),
                    link: None,
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
                    target_ref: local_ref.clone(),
                    target_before: Some(capture.commit.clone()),
                    message: "vulcan sync snapshot\n".to_string(),
                    link: None,
                },
            )
            .expect("unchanged capture should succeed");
        assert!(!second.created);
        assert_eq!(second.commit, capture.commit.clone());

        fs::write(temporary.path().join("Home.md"), "updated again\n")
            .expect("second worktree update");
        let third = engine
            .capture_worktree(
                &repository,
                &GitCaptureRequest {
                    base: Some(capture.commit.clone()),
                    target_ref: local_ref,
                    target_before: Some(capture.commit.clone()),
                    message: "vulcan sync changed snapshot\n".to_string(),
                    link: None,
                },
            )
            .expect("changed capture should fall back to a full snapshot");
        assert!(third.created);
        assert_ne!(third.commit, capture.commit);
    }

    #[test]
    fn sync_index_reuses_matching_stat_cache_and_reseeds_stale_trees() {
        let temporary = TempDir::new().expect("temporary directory");
        init_repo(temporary.path());
        fs::write(temporary.path().join("Home.md"), "initial\n").expect("initial note");
        let first = commit_all(temporary.path(), "initial");
        let engine = GitCliEngine::default();
        let repository = engine
            .discover_repository(temporary.path())
            .expect("repository");
        let index_path = repository.sync_index();

        engine
            .snapshot_worktree_tree(&repository, Some(&first))
            .expect("initial snapshot");
        assert!(engine
            .prepare_sync_index(&repository, &index_path, Some(&first), "prepare test index",)
            .expect("matching index"));

        fs::write(temporary.path().join("Home.md"), "updated\n").expect("updated note");
        let second = commit_all(temporary.path(), "updated");
        assert!(!engine
            .prepare_sync_index(
                &repository,
                &index_path,
                Some(&second),
                "prepare test index",
            )
            .expect("stale index should be rebuilt"));
        assert!(engine
            .prepare_sync_index(
                &repository,
                &index_path,
                Some(&second),
                "prepare test index",
            )
            .expect("rebuilt index should be reusable"));
    }

    #[test]
    fn snapshots_and_worktree_checks_exclude_device_local_vulcan_files() {
        let temporary = TempDir::new().expect("temporary directory");
        init_repo(temporary.path());
        fs::write(temporary.path().join("Home.md"), "initial\n").expect("note");
        fs::write(
            temporary.path().join(".gitignore"),
            ".vulcan/cache.db*\nignored.tmp\n",
        )
        .expect("gitignore");
        let head = commit_all(temporary.path(), "initial");
        fs::write(temporary.path().join("ignored.tmp"), "ignored bytes\n").expect("ignored file");
        fs::create_dir(temporary.path().join(".vulcan")).expect("Vulcan directory");
        for (path, contents) in [
            (
                "config.local.toml",
                b"[sync]\nagent_auto_accept = true\n".as_slice(),
            ),
            ("cache.db", b"derived cache".as_slice()),
            ("cache.db-wal", b"derived WAL".as_slice()),
            ("cache.db-shm", b"derived shared memory".as_slice()),
            ("write.lock", b"coordination lock".as_slice()),
            ("write.intent", b"writer turnstile".as_slice()),
            ("editor-watch.lock", b"editor watch lease".as_slice()),
            ("index-owner.lock", b"serve 42\n".as_slice()),
        ] {
            fs::write(temporary.path().join(".vulcan").join(path), contents)
                .expect("device-local file");
        }
        // A held lock is unreadable on Windows; the capture must not read it.
        #[cfg(unix)]
        for path in ["write.lock", "cache.db"] {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                temporary.path().join(".vulcan").join(path),
                fs::Permissions::from_mode(0o000),
            )
            .expect("unreadable device-local file");
        }
        let engine = GitCliEngine::default();
        let repository = engine
            .discover_repository(temporary.path())
            .expect("repository");

        let snapshot = engine
            .snapshot_worktree_tree(&repository, Some(&head))
            .expect("snapshot");
        assert_eq!(
            snapshot,
            engine.tree_oid(&repository, &head).expect("head tree")
        );
        assert!(engine
            .worktree_matches_tree(&repository, &head)
            .expect("worktree match"));
        for path in [
            ".vulcan/config.local.toml",
            ".vulcan/cache.db",
            ".vulcan/cache.db-wal",
            ".vulcan/cache.db-shm",
            ".vulcan/write.lock",
            ".vulcan/write.intent",
            ".vulcan/editor-watch.lock",
            ".vulcan/index-owner.lock",
        ] {
            assert!(engine
                .path_object(&repository, &snapshot, path)
                .expect("path lookup")
                .is_none());
        }

        #[cfg(unix)]
        for path in ["write.lock", "cache.db"] {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                temporary.path().join(".vulcan").join(path),
                fs::Permissions::from_mode(0o644),
            )
            .expect("readable device-local file");
        }
        run_git(
            temporary.path(),
            &[
                "add",
                "-f",
                ".vulcan/config.local.toml",
                ".vulcan/cache.db",
                ".vulcan/cache.db-wal",
                ".vulcan/cache.db-shm",
                ".vulcan/write.lock",
                ".vulcan/write.intent",
                ".vulcan/editor-watch.lock",
                ".vulcan/index-owner.lock",
            ],
        );
        let tracked_internal = commit_all(temporary.path(), "track internal state");
        let cleaned = engine
            .snapshot_worktree_tree(&repository, Some(&tracked_internal))
            .expect("clean tracked internal paths");
        assert_eq!(cleaned, snapshot);

        fs::write(temporary.path().join("Home.md"), "changed\n").expect("changed note");
        assert!(!engine
            .worktree_matches_tree(&repository, &head)
            .expect("changed worktree"));
    }

    #[test]
    fn worktree_match_check_is_read_only_and_includes_untracked_paths() {
        let temporary = TempDir::new().expect("temporary directory");
        init_repo(temporary.path());
        fs::write(temporary.path().join("Home.md"), "initial\n").expect("initial note");
        let head = commit_all(temporary.path(), "initial");
        let engine = GitCliEngine::default();
        let repository = engine
            .discover_repository(temporary.path())
            .expect("repository");
        let index_before = run_git_capture(temporary.path(), &["write-tree"]);

        assert!(engine
            .worktree_matches_tree(&repository, &head)
            .expect("matching worktree"));
        fs::write(temporary.path().join("Untracked.md"), "new\n").expect("untracked note");
        assert!(!engine
            .worktree_matches_tree(&repository, &head)
            .expect("untracked worktree"));
        assert_eq!(
            run_git_capture(temporary.path(), &["write-tree"]),
            index_before
        );
    }

    #[test]
    fn trees_built_from_entries_extend_a_base_and_list_by_subtree() {
        let temporary = TempDir::new().expect("temporary directory");
        init_repo(temporary.path());
        fs::write(temporary.path().join("note.md"), "note\n").expect("note");
        let head = commit_all(temporary.path(), "worktree");
        let engine = GitCliEngine::default();
        let repository = engine
            .discover_repository(temporary.path())
            .expect("repository");
        assert!(engine
            .worktree_matches_tree(&repository, &head)
            .expect("compare"));
        let blobs = engine
            .write_blobs(&repository, &[b"x\n", b"y\n", b"z\n", b"w\n"])
            .expect("blobs");
        let entry = |path: &str, oid: &GitOid| GitTreeEntry {
            path: path.to_string(),
            oid: oid.clone(),
            mode: "100644".to_string(),
            kind: "blob".to_string(),
        };
        let first = engine
            .tree_with_entries(
                &repository,
                None,
                &[
                    entry("ab/x", &blobs[0]),
                    entry("ab/y", &blobs[1]),
                    entry("cd/z", &blobs[2]),
                ],
            )
            .expect("tree from nothing");
        let second = engine
            .tree_with_entries(&repository, Some(&first), &[entry("ab/w", &blobs[3])])
            .expect("tree extending a base");

        let mut under = engine
            .tree_entries_under(&repository, &second, "ab")
            .expect("subtree listing");
        under.sort_by(|left, right| left.path.cmp(&right.path));
        assert_eq!(
            under
                .iter()
                .map(|entry| entry.path.as_str())
                .collect::<Vec<_>>(),
            ["ab/w", "ab/x", "ab/y"]
        );
        assert_eq!(under[0].oid, blobs[3]);
        assert!(engine
            .tree_entries_under(&repository, &second, "missing")
            .expect("missing subtree")
            .is_empty());
        // Building trees never disturbs the worktree comparison index.
        assert!(engine
            .worktree_matches_tree(&repository, &head)
            .expect("compare again"));
    }

    #[test]
    fn scoped_tree_listing_returns_exactly_the_selected_entries() {
        let temporary = TempDir::new().expect("temporary directory");
        init_repo(temporary.path());
        fs::create_dir_all(temporary.path().join("notes/sub")).expect("folders");
        for index in 0..300 {
            fs::write(
                temporary.path().join(format!("notes/n{index:03}.md")),
                format!("{index}\n"),
            )
            .expect("note");
        }
        fs::write(temporary.path().join("notes/n00[0-9].md"), "glob-like\n").expect("glob");
        fs::write(temporary.path().join("notes/sub/inner.md"), "inner\n").expect("inner");
        let commit = commit_all(temporary.path(), "tree");
        let engine = GitCliEngine::default();
        let repository = engine
            .discover_repository(temporary.path())
            .expect("repository");

        let selected = [
            "notes/n00[0-9].md",
            "notes/n007.md",
            "notes/sub",
            "missing.md",
        ]
        .map(str::to_string);
        let before = engine.subprocess_count().unwrap_or_default();
        let mut entries = engine
            .tree_entries_for_paths(&repository, &commit, &selected)
            .expect("scoped listing");
        assert_eq!(engine.subprocess_count().unwrap_or_default() - before, 1);
        entries.sort_by(|left, right| left.path.cmp(&right.path));
        // Literal names only: no glob expansion, no directory children.
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.path.as_str())
                .collect::<Vec<_>>(),
            ["notes/n007.md", "notes/n00[0-9].md"]
        );
        let full = engine
            .tree_entries(&repository, &commit)
            .expect("full listing");
        for entry in &entries {
            assert!(full.contains(entry));
        }

        // 300 paths take two bounded commands.
        let many = (0..300)
            .map(|index| format!("notes/n{index:03}.md"))
            .collect::<Vec<_>>();
        let before = engine.subprocess_count().unwrap_or_default();
        let entries = engine
            .tree_entries_for_paths(&repository, &commit, &many)
            .expect("chunked listing");
        assert_eq!(engine.subprocess_count().unwrap_or_default() - before, 2);
        assert_eq!(entries.len(), 300);
    }

    #[test]
    fn batched_remote_deletion_honors_each_lease_in_one_push() {
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
        let first = commit_all(&worktree, "first");
        fs::write(worktree.join("Home.md"), "changed\n").expect("change");
        let second = commit_all(&worktree, "second");
        let engine = GitCliEngine::default();
        let repository = engine.discover_repository(&worktree).expect("repository");
        let remote = GitRemote::parse("origin").expect("remote");
        let reference = |name: &str| {
            GitRefName::parse(format!("refs/heads/__vulcan-sync/conflicts/{name}")).expect("ref")
        };
        for (name, commit) in [("a/record", &first), ("b/record", &second)] {
            engine
                .push_ref(&repository, &remote, commit, &reference(name), None)
                .expect("push");
        }

        let before = engine.subprocess_count().unwrap_or_default();
        let results = engine
            .delete_remote_refs(
                &repository,
                &remote,
                &[
                    (reference("a/record"), first.clone()),
                    // The lease names the wrong commit, so this ref stays.
                    (reference("b/record"), first.clone()),
                    (reference("c/record"), first.clone()),
                ],
            )
            .expect("batched deletion");
        assert_eq!(engine.subprocess_count().unwrap_or_default() - before, 1);
        assert_eq!(
            results,
            [
                GitRefDeleteResult::Deleted,
                GitRefDeleteResult::Stale,
                // Git cannot tell a missing ref from a failed lease.
                GitRefDeleteResult::Stale,
            ]
        );
        assert_eq!(
            engine
                .remote_ref(&repository, &remote, &reference("b/record"))
                .expect("remote b"),
            Some(second)
        );
        assert_eq!(
            engine
                .remote_ref(&repository, &remote, &reference("a/record"))
                .expect("remote a"),
            None
        );
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
            head.clone()
        );
        fs::write(worktree.join("Home.md"), "changed\n").expect("changed note");
        let changed = commit_all(&worktree, "changed");
        assert_eq!(
            engine
                .push_ref(&repository, &remote, &changed, &live, None)
                .expect("stale creation lease"),
            GitPushResult::Rejected
        );
        let stale = GitOid::parse("1111111111111111111111111111111111111111")
            .expect("valid stale object ID");
        assert_eq!(
            engine
                .delete_remote_ref(&repository, &remote, &live, &stale)
                .expect("stale deletion"),
            GitRefDeleteResult::Stale
        );
        assert_eq!(
            engine
                .delete_remote_ref(&repository, &remote, &live, &head)
                .expect("leased deletion"),
            GitRefDeleteResult::Deleted
        );
        assert_eq!(
            engine
                .delete_remote_ref(&repository, &remote, &live, &head)
                .expect("repeated deletion"),
            GitRefDeleteResult::Missing
        );
        assert!(matches!(
            engine.fetch_ref(&repository, &remote, &live, &fetched),
            Err(GitEngineError::RemoteRefMissing {
                remote: missing_remote,
                reference: missing_ref,
            }) if missing_remote == remote && missing_ref == live
        ));
    }

    #[test]
    fn push_and_fetch_custom_ref_use_compare_and_swap_with_git_transport() {
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
        let live = GitRefName::parse("refs/vulcan-sync/v1/live").expect("custom live ref");
        let fetched =
            GitRefName::parse("refs/vulcan/sync/remotes/origin/custom-live").expect("local ref");

        assert_eq!(
            engine
                .push_ref(&repository, &remote, &head, &live, None)
                .expect("initial custom-ref push"),
            GitPushResult::Updated
        );
        assert_eq!(
            engine
                .fetch_ref(&repository, &remote, &live, &fetched)
                .expect("custom-ref fetch"),
            head
        );
        let stale = GitOid::parse("1111111111111111111111111111111111111111")
            .expect("valid stale object ID");
        assert_eq!(
            engine
                .delete_remote_ref(&repository, &remote, &live, &stale)
                .expect("stale custom-ref deletion"),
            GitRefDeleteResult::Stale
        );
        assert_eq!(
            engine
                .delete_remote_ref(&repository, &remote, &live, &head)
                .expect("leased custom-ref deletion"),
            GitRefDeleteResult::Deleted
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
    fn lists_validated_hidden_refs_in_stable_name_order() {
        let temporary = TempDir::new().expect("temporary directory");
        init_repo(temporary.path());
        fs::write(temporary.path().join("Home.md"), "initial\n").expect("note");
        let commit = commit_all(temporary.path(), "initial");
        let engine = GitCliEngine::default();
        let repository = engine
            .discover_repository(temporary.path())
            .expect("repository");
        for reference in [
            "refs/vulcan/checkpoints/recovery/02",
            "refs/vulcan/checkpoints/semantic/01",
            "refs/vulcan/checkpoints/recovery/01",
        ] {
            engine
                .create_ref(
                    &repository,
                    &GitRefName::parse(reference).expect("ref"),
                    &commit,
                )
                .expect("create ref");
        }

        let references = engine
            .list_refs(
                &repository,
                &GitRefName::parse("refs/vulcan/checkpoints").expect("prefix"),
            )
            .expect("list refs");

        assert_eq!(
            references
                .iter()
                .map(|reference| reference.name.as_str())
                .collect::<Vec<_>>(),
            [
                "refs/vulcan/checkpoints/recovery/01",
                "refs/vulcan/checkpoints/recovery/02",
                "refs/vulcan/checkpoints/semantic/01",
            ]
        );
        assert!(references
            .iter()
            .all(|reference| reference.target == commit));
    }

    #[test]
    fn batch_blob_visitor_rejects_incomplete_and_malformed_protocol() {
        let oid = GitOid::parse("1111111111111111111111111111111111111111").unwrap();
        for response in [
            String::new(),
            format!("{oid} missing\n"),
            format!("{oid} tree 0\n\n"),
            format!("{oid} blob invalid\n"),
            format!("{oid} blob 4\nabc"),
            format!("{oid} blob 3\nabc!"),
            format!("{oid} blob 0\n\nextra"),
            "x".repeat(1025),
        ] {
            assert!(visit_batch_blobs_reader(
                std::slice::from_ref(&oid),
                std::io::Cursor::new(response),
                100,
                100,
                &mut |_, _| Ok(())
            )
            .is_err());
        }
    }

    #[test]
    fn batch_blob_visitor_delivers_before_reading_next_payload() {
        let oid = GitOid::parse("1111111111111111111111111111111111111111").unwrap();
        let first = format!("{oid} blob 3\nabc\n");
        // The second header exists but its payload is absent. Delivery of the
        // complete prefix proves the reader did not collect the batch first.
        let response = format!("{first}{oid} blob 8\n");
        let mut delivered = 0;
        let result = visit_batch_blobs_reader(
            &[oid.clone(), oid],
            std::io::Cursor::new(response),
            100,
            100,
            &mut |_, data| {
                assert_eq!(data, b"abc");
                delivered += 1;
                Ok(())
            },
        );
        assert!(result.is_err());
        assert_eq!(delivered, 1);
    }

    #[cfg(unix)]
    #[test]
    fn batch_blob_visitor_drains_stderr_and_times_out_process_groups() {
        let oid = GitOid::parse("1111111111111111111111111111111111111111").unwrap();
        let engine = GitCliEngine::default().with_command_timeout(Duration::from_millis(100));
        for script in ["sleep 10", "printf 'broken\\n'; sleep 10"] {
            let mut command = Command::new("sh");
            command.args(["-c", script]);
            assert!(matches!(
                engine.visit_batch_blobs_with_limits(
                    command,
                    b"",
                    std::slice::from_ref(&oid),
                    (100, 100),
                    &mut |_, _| Ok(())
                ),
                Err(GitEngineError::CommandTimedOut { .. })
            ));
        }
        let engine = GitCliEngine::default();
        let mut command = Command::new("sh");
        command.args([
            "-c",
            &format!("head -c 1048576 /dev/zero >&2; printf '{oid} blob 0\\n\\n'; exit 7"),
        ]);
        let error = engine
            .visit_batch_blobs_with_limits(command, b"", &[oid], (100, 100), &mut |_, _| Ok(()))
            .unwrap_err();
        match error {
            GitEngineError::CommandFailed {
                exit_code, stderr, ..
            } => {
                assert_eq!(exit_code, Some(7));
                assert!(stderr.len() <= MAX_ERROR_BYTES);
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn batch_blob_reader_rejects_declared_limits_before_content() {
        let first = GitOid::parse("1111111111111111111111111111111111111111").expect("OID");
        let oversized = format!("{first} blob 11\n");
        let error = parse_batch_blobs_reader(
            std::slice::from_ref(&first),
            std::io::Cursor::new(oversized),
            10,
            10,
        )
        .expect_err("declared per-blob limit");
        assert!(error.to_string().contains("exceeds the 10 byte read limit"));

        let second = GitOid::parse("2222222222222222222222222222222222222222").expect("OID");
        let aggregate = format!("{first} blob 6\nfirst!\n{second} blob 6\n");
        let error =
            parse_batch_blobs_reader(&[first, second], std::io::Cursor::new(aggregate), 10, 10)
                .expect_err("declared aggregate limit");
        assert!(error
            .to_string()
            .contains("aggregate blob contents exceed the 10 byte read limit"));
    }

    #[test]
    fn git_blob_batch_enforces_caller_aggregate_limit() {
        let temporary = TempDir::new().expect("temporary directory");
        init_repo(temporary.path());
        let engine = GitCliEngine::default();
        let repository = engine
            .discover_repository(temporary.path())
            .expect("repository");
        let first = engine.write_blob(&repository, b"123456").expect("blob");
        let second = engine.write_blob(&repository, b"abcdef").expect("blob");
        let before_batch = engine.subprocess_count().expect("CLI metrics");

        let error = engine
            .read_blobs_bounded(&repository, &[first, second], 10, 10)
            .expect_err("aggregate limit");
        assert!(error
            .to_string()
            .contains("aggregate blob contents exceed the 10 byte read limit"));
        assert_eq!(
            engine.subprocess_count().expect("CLI metrics") - before_batch,
            1,
            "the streaming batch spawn is counted once"
        );
        let fresh = engine.clone().with_fresh_metrics();
        assert_eq!(fresh.subprocess_count(), Some(0));
        assert!(engine.subprocess_count().is_some_and(|count| count > 0));
    }

    #[test]
    fn delete_ref_requires_the_exact_expected_object() {
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
        let proposal =
            GitRefName::parse("refs/vulcan/proposals/semantic/test").expect("proposal ref");
        engine
            .create_ref(&repository, &proposal, &initial)
            .expect("proposal creation");

        assert_eq!(
            engine
                .delete_ref(&repository, &proposal, &changed)
                .expect("stale deletion"),
            GitRefDeleteResult::Stale
        );
        assert_eq!(
            engine.read_ref(&repository, &proposal).expect("read ref"),
            Some(initial.clone())
        );
        assert_eq!(
            engine
                .delete_ref(&repository, &proposal, &initial)
                .expect("leased deletion"),
            GitRefDeleteResult::Deleted
        );
        assert_eq!(
            engine
                .delete_ref(&repository, &proposal, &initial)
                .expect("missing deletion"),
            GitRefDeleteResult::Missing
        );
    }

    #[test]
    fn compare_and_swap_reports_stale_for_missing_refs() {
        let temporary = TempDir::new().expect("temporary directory");
        init_repo(temporary.path());
        fs::write(temporary.path().join("Home.md"), "initial\n").expect("initial note");
        let commit = commit_all(temporary.path(), "initial");
        let engine = GitCliEngine::default();
        let repository = engine
            .discover_repository(temporary.path())
            .expect("repository");
        let reference = GitRefName::parse("refs/vulcan/test/cas").expect("cas ref");

        // CAS with an expected value against a never-created ref: stale, not
        // a hard error.
        assert_eq!(
            engine
                .compare_and_swap_ref(&repository, &reference, &commit, Some(&commit))
                .expect("missing-ref compare and swap"),
            GitRefUpdateResult::Stale
        );

        // The create -> external delete -> CAS race must also report stale.
        engine
            .create_ref(&repository, &reference, &commit)
            .expect("create ref");
        run_git(temporary.path(), &["update-ref", "-d", reference.as_str()]);
        assert_eq!(
            engine
                .compare_and_swap_ref(&repository, &reference, &commit, Some(&commit))
                .expect("raced compare and swap"),
            GitRefUpdateResult::Stale
        );
    }

    #[test]
    fn newly_ignored_paths_leave_the_capture_but_force_added_ones_stay() {
        let temporary = TempDir::new().expect("temporary directory");
        let root = temporary.path();
        init_repo(root);
        fs::create_dir_all(root.join(".obsidian")).expect("obsidian dir");
        fs::write(root.join(".gitignore"), "forced.md\n").expect("ignore file");
        fs::write(root.join(".obsidian/workspace.json"), "{}\n").expect("workspace");
        fs::write(root.join("forced.md"), "forced\n").expect("forced file");
        fs::write(root.join("Home.md"), "home\n").expect("note");
        run_git(root, &["add", "--all", "--force", "--", "."]);
        run_git(root, &["commit", "--quiet", "-m", "base"]);
        let base = GitOid::parse(run_git_capture(root, &["rev-parse", "HEAD"])).expect("base");
        let engine = GitCliEngine::default();
        let repository = engine.discover_repository(root).expect("repository");
        let tracked = |tree: &GitOid| {
            engine
                .tree_entries(&repository, tree)
                .expect("entries")
                .into_iter()
                .map(|entry| entry.path)
                .collect::<Vec<_>>()
        };

        // A force-added file stays tracked when it changes.
        fs::write(root.join("forced.md"), "changed\n").expect("forced edit");
        let tree = engine
            .snapshot_worktree_tree(&repository, Some(&base))
            .expect("capture");
        assert!(tracked(&tree).contains(&"forced.md".to_string()));

        // A newly ignored path leaves the capture, in nested rules too, and
        // the file stays on disk.
        fs::write(root.join(".obsidian/.gitignore"), "workspace.json\n").expect("nested");
        fs::write(root.join(".obsidian/workspace.json"), "{\"a\":1}\n").expect("edit");
        let tree = engine
            .snapshot_worktree_tree(&repository, Some(&base))
            .expect("capture");
        let paths = tracked(&tree);
        assert!(!paths.contains(&".obsidian/workspace.json".to_string()));
        assert!(paths.contains(&".obsidian/.gitignore".to_string()));
        assert!(paths.contains(&"forced.md".to_string()));
        assert!(root.join(".obsidian/workspace.json").is_file());
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
    fn safety_state_detects_every_guarded_git_operation_marker() {
        let temporary = TempDir::new().expect("temporary directory");
        init_repo(temporary.path());
        fs::write(temporary.path().join("Home.md"), "initial\n").expect("initial note");
        commit_all(temporary.path(), "initial");
        let engine = GitCliEngine::default();
        let repository = engine
            .discover_repository(temporary.path())
            .expect("repository");

        for (marker, expected) in [
            ("MERGE_HEAD", "merge"),
            ("CHERRY_PICK_HEAD", "cherry-pick"),
            ("REVERT_HEAD", "revert"),
            ("BISECT_LOG", "bisect"),
            ("rebase-merge", "rebase"),
            ("rebase-apply", "rebase"),
        ] {
            let path = repository.git_dir.join(marker);
            if marker.starts_with("rebase-") {
                fs::create_dir(&path).expect("operation directory");
            } else {
                fs::write(&path, "fixture\n").expect("operation marker");
            }
            assert_eq!(
                engine
                    .safety_state(&repository)
                    .expect("operation safety state")
                    .operation
                    .as_deref(),
                Some(expected),
                "operation marker {marker}"
            );
            if path.is_dir() {
                fs::remove_dir(&path).expect("remove operation directory");
            } else {
                fs::remove_file(&path).expect("remove operation marker");
            }
        }
    }

    #[test]
    fn repository_requirements_check_cache_ignores_at_a_subdirectory_vault() {
        // A vault below the work tree root ignores its cache through its own
        // `.vulcan/.gitignore`; the repository root has no `.vulcan/` at all.
        let temporary = TempDir::new().expect("temporary directory");
        init_repo(temporary.path());
        let vault = temporary.path().join("docs");
        fs::create_dir_all(vault.join(".vulcan")).expect("vault state directory");
        fs::write(vault.join(".vulcan/.gitignore"), "*\n!.gitignore\n").expect("ignore rules");
        fs::write(vault.join("Home.md"), "home\n").expect("note");
        commit_all(temporary.path(), "subdirectory vault");
        let engine = GitCliEngine::default();
        let repository = engine
            .discover_repository(&vault)
            .expect("repository from the vault");

        let requirements = engine
            .repository_requirements(&repository, &vault)
            .expect("requirements");

        assert_eq!(
            requirements.ignored_internal_paths,
            [
                "docs/.vulcan/cache.db",
                "docs/.vulcan/cache.db-wal",
                "docs/.vulcan/cache.db-shm"
            ]
        );
        assert!(engine
            .repository_requirements(&repository, temporary.path())
            .expect("requirements at the root")
            .ignored_internal_paths
            .is_empty());
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
            .repository_requirements(&repository, temporary.path())
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
        assert_eq!(requirements.required_filters.len(), 1);
        let lfs = &requirements.required_filters[0];
        assert_eq!(lfs.name, "lfs");
        assert_eq!(lfs.path_count, 1);
        assert_eq!(lfs.executable_available, requirements.git_lfs_available);
        assert!(requirements.git_lfs_available.is_some());

        let cache_path = GitCliEngine::requirements_cache_path(&repository);
        assert!(
            !cache_path.exists(),
            "read-only inspection must not persist its cache"
        );
        engine
            .persist_repository_requirements_cache(&repository)
            .expect("persist requirements cache");
        assert!(cache_path.is_file());
        assert_eq!(
            engine
                .repository_requirements(&repository, temporary.path())
                .expect("cached requirements"),
            requirements
        );

        fs::write(
            temporary.path().join(".gitattributes"),
            "*.bin filter=replacement\n",
        )
        .expect("changed attributes");
        let changed = engine
            .repository_requirements(&repository, temporary.path())
            .expect("changed requirements");
        assert_eq!(changed.required_filters.len(), 1);
        assert_eq!(changed.required_filters[0].name, "replacement");
    }

    #[test]
    fn repository_requirements_do_not_deadlock_on_large_path_sets() {
        let temporary = TempDir::new().expect("temporary directory");
        init_repo(temporary.path());
        fs::write(temporary.path().join("seed"), "fixture\n").expect("seed file");
        let blob = run_git_capture(temporary.path(), &["hash-object", "-w", "seed"]);
        let mut index_input = Vec::new();
        for index in 0..4_000 {
            write!(
                index_input,
                "100644 {blob}\tlarge-path-set/{index:04}-a-deliberately-long-note-name.md\0"
            )
            .expect("index entry");
        }
        let engine = GitCliEngine::default();
        let mut command = Command::new("git");
        command
            .current_dir(temporary.path())
            .args(["update-index", "-z", "--index-info"]);
        let output = engine
            .execute_with_input(command, "populate a large test index", &index_input)
            .expect("Git should populate the index");
        ensure_success("populate a large test index", output).expect("valid index entries");
        let repository = engine
            .discover_repository(temporary.path())
            .expect("repository");

        let requirements = engine
            .repository_requirements(&repository, temporary.path())
            .expect("requirements should not deadlock");

        assert_eq!(requirements.tracked_paths, 4_000);
        assert!(requirements.required_filters.is_empty());
    }

    #[test]
    fn repository_requirements_distinguish_complete_and_missing_filter_drivers() {
        let temporary = TempDir::new().expect("temporary directory");
        init_repo(temporary.path());
        fs::write(
            temporary.path().join(".gitattributes"),
            "*.ready filter=ready\n*.missing filter=missing\n",
        )
        .expect("attributes");
        fs::write(temporary.path().join("asset.ready"), "ready\n").expect("ready asset");
        fs::write(temporary.path().join("asset.missing"), "missing\n").expect("missing asset");
        commit_all(temporary.path(), "requirements");
        run_git(
            temporary.path(),
            &["config", "filter.ready.clean", "configured-clean"],
        );
        run_git(
            temporary.path(),
            &["config", "filter.ready.smudge", "configured-smudge"],
        );
        let engine = GitCliEngine::default();
        let repository = engine
            .discover_repository(temporary.path())
            .expect("repository");

        let requirements = engine
            .repository_requirements(&repository, temporary.path())
            .expect("requirements");

        let ready = requirements
            .required_filters
            .iter()
            .find(|filter| filter.name == "ready")
            .expect("ready filter");
        assert!(ready.clean_configured);
        assert!(ready.smudge_configured);
        assert!(!ready.process_configured);
        assert!(ready.ready());
        let missing = requirements
            .required_filters
            .iter()
            .find(|filter| filter.name == "missing")
            .expect("missing filter");
        assert!(!missing.ready());
    }
}
