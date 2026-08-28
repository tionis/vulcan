use crate::{
    GitCaptureRequest, GitEngine, GitEngineError, GitInstallation, GitOid, GitPushResult,
    GitRefName, GitRemote, GitRepository, GitSafetyState,
};
use fs2::FileExt;
use serde::Serialize;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::fs::{self, File, OpenOptions};
use std::path::Path;

const SYNC_PROTOCOL_VERSION: u32 = 1;
const DEFAULT_LIVE_REF: &str = "refs/heads/__vulcan-sync/live";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitSyncOptions {
    pub remote: GitRemote,
    pub live_ref: GitRefName,
    pub max_retries: usize,
    pub dry_run: bool,
}

impl Default for GitSyncOptions {
    fn default() -> Self {
        Self {
            remote: GitRemote::parse("origin").expect("the default Git remote is valid"),
            live_ref: GitRefName::parse(DEFAULT_LIVE_REF).expect("the default live ref is valid"),
            max_retries: 4,
            dry_run: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GitSyncOutcome {
    Planned,
    Paused,
    UpToDate,
    Bootstrapped,
    Pushed,
    Pulled,
    Merged,
    Conflicted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GitSyncAction {
    SnapshotCreated,
    Pushed,
    WorktreeApplied,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitSyncRefs {
    pub live: GitRefName,
    pub local: GitRefName,
    pub fetched: GitRefName,
    pub pending: GitRefName,
}

impl GitSyncRefs {
    fn for_options(options: &GitSyncOptions) -> Result<Self, GitSyncError> {
        let profile =
            blake3::hash(format!("{}\0{}", options.remote, options.live_ref).as_bytes()).to_hex();
        let profile = &profile[..16];
        Ok(Self {
            live: options.live_ref.clone(),
            local: GitRefName::parse(format!("refs/vulcan/sync/{profile}/local/live"))?,
            fetched: GitRefName::parse(format!("refs/vulcan/sync/{profile}/remotes/live"))?,
            pending: GitRefName::parse(format!("refs/vulcan/sync/{profile}/pending/live"))?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitSyncConflict {
    pub remote: GitOid,
    pub local: GitOid,
    pub merge_tree: Option<GitOid>,
    pub diagnostics: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitSyncReport {
    pub dry_run: bool,
    pub outcome: GitSyncOutcome,
    pub installation: GitInstallation,
    pub repository: GitRepository,
    pub remote: GitRemote,
    pub refs: GitSyncRefs,
    pub safety: GitSafetyState,
    pub remote_before: Option<GitOid>,
    pub local_before: Option<GitOid>,
    pub local_snapshot: Option<GitOid>,
    pub accepted: Option<GitOid>,
    pub actions: Vec<GitSyncAction>,
    pub retries: usize,
    pub conflict: Option<GitSyncConflict>,
}

impl GitSyncReport {
    fn initial(
        options: &GitSyncOptions,
        installation: GitInstallation,
        repository: GitRepository,
        refs: GitSyncRefs,
        safety: GitSafetyState,
        remote_before: Option<GitOid>,
        local_before: Option<GitOid>,
    ) -> Self {
        Self {
            dry_run: options.dry_run,
            outcome: GitSyncOutcome::Planned,
            installation,
            repository,
            remote: options.remote.clone(),
            refs,
            safety,
            remote_before,
            local_before,
            local_snapshot: None,
            accepted: None,
            actions: Vec::new(),
            retries: 0,
            conflict: None,
        }
    }
}

#[derive(Debug)]
pub enum GitSyncError {
    Git(GitEngineError),
    Locked,
    RetryLimit { attempts: usize },
    Io(std::io::Error),
}

impl Display for GitSyncError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Git(error) => Display::fmt(error, formatter),
            Self::Locked => formatter.write_str(
                "another Vulcan synchronization cycle already holds the repository lock",
            ),
            Self::RetryLimit { attempts } => write!(
                formatter,
                "synchronization did not converge after {attempts} attempts; local snapshots remain preserved"
            ),
            Self::Io(error) => Display::fmt(error, formatter),
        }
    }
}

impl Error for GitSyncError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Git(error) => Some(error),
            Self::Io(error) => Some(error),
            Self::Locked | Self::RetryLimit { .. } => None,
        }
    }
}

impl From<GitEngineError> for GitSyncError {
    fn from(error: GitEngineError) -> Self {
        Self::Git(error)
    }
}

impl From<std::io::Error> for GitSyncError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

pub fn sync_git_once(
    engine: &dyn GitEngine,
    vault_path: &Path,
    options: &GitSyncOptions,
) -> Result<GitSyncReport, GitSyncError> {
    let installation = engine.installation()?;
    let repository = engine.discover_repository(vault_path)?;
    let refs = GitSyncRefs::for_options(options)?;
    let safety = engine.safety_state(&repository)?;
    let local_before = engine.read_ref(&repository, &refs.local)?;
    let remote_before = engine.remote_ref(&repository, &options.remote, &refs.live)?;
    let mut report = GitSyncReport::initial(
        options,
        installation,
        repository,
        refs,
        safety,
        remote_before,
        local_before,
    );
    if options.dry_run {
        return Ok(report);
    }
    if report.safety.staged_changes || report.safety.operation.is_some() {
        report.outcome = GitSyncOutcome::Paused;
        return Ok(report);
    }

    let _lock = RepositoryLock::acquire(&report.repository)?;
    for attempt in 0..options.max_retries.max(1) {
        report.retries = attempt;
        if run_attempt(engine, options, &mut report)? == AttemptResult::Finished {
            return Ok(report);
        }
    }

    Err(GitSyncError::RetryLimit {
        attempts: options.max_retries.max(1),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AttemptResult {
    Retry,
    Finished,
}

fn run_attempt(
    engine: &dyn GitEngine,
    options: &GitSyncOptions,
    report: &mut GitSyncReport,
) -> Result<AttemptResult, GitSyncError> {
    let base = engine
        .read_ref(&report.repository, &report.refs.local)?
        .or(engine.head_commit(&report.repository)?);
    let capture = engine.capture_worktree(
        &report.repository,
        &GitCaptureRequest {
            base,
            target_ref: report.refs.local.clone(),
            message: snapshot_message(&report.refs),
        },
    )?;
    report.local_snapshot = Some(capture.commit.clone());
    if capture.created {
        report.actions.push(GitSyncAction::SnapshotCreated);
    }

    let has_remote = engine
        .remote_ref(&report.repository, &options.remote, &report.refs.live)?
        .is_some();
    let Some((accepted, outcome, pushed)) =
        reconcile(engine, options, report, &capture, has_remote)?
    else {
        return Ok(if report.outcome == GitSyncOutcome::Conflicted {
            AttemptResult::Finished
        } else {
            AttemptResult::Retry
        });
    };

    let verification = engine.capture_worktree(
        &report.repository,
        &GitCaptureRequest {
            base: Some(capture.commit.clone()),
            target_ref: report.refs.local.clone(),
            message: snapshot_message(&report.refs),
        },
    )?;
    if verification.commit != capture.commit {
        return Ok(AttemptResult::Retry);
    }
    if pushed {
        report.actions.push(GitSyncAction::Pushed);
    }
    if verification.tree != engine.tree_oid(&report.repository, &accepted)? {
        engine.apply_tree(&report.repository, &verification.commit, &accepted)?;
        report.actions.push(GitSyncAction::WorktreeApplied);
    }
    engine.update_ref(&report.repository, &report.refs.local, &accepted)?;
    engine.update_ref(&report.repository, &report.refs.fetched, &accepted)?;
    engine.update_ref(&report.repository, &report.refs.pending, &accepted)?;
    report.outcome = outcome;
    report.accepted = Some(accepted);
    Ok(AttemptResult::Finished)
}

fn reconcile(
    engine: &dyn GitEngine,
    options: &GitSyncOptions,
    report: &mut GitSyncReport,
    capture: &crate::GitCapture,
    has_remote: bool,
) -> Result<Option<(GitOid, GitSyncOutcome, bool)>, GitSyncError> {
    if !has_remote {
        return Ok(
            match engine.push_ref(
                &report.repository,
                &options.remote,
                &capture.commit,
                &report.refs.live,
                None,
            )? {
                GitPushResult::Updated => {
                    Some((capture.commit.clone(), GitSyncOutcome::Bootstrapped, true))
                }
                GitPushResult::Rejected => None,
            },
        );
    }
    let remote = engine.fetch_ref(
        &report.repository,
        &options.remote,
        &report.refs.live,
        &report.refs.fetched,
    )?;
    if capture.commit == remote {
        return Ok(Some((remote, GitSyncOutcome::UpToDate, false)));
    }
    if engine.is_ancestor(&report.repository, &remote, &capture.commit)? {
        return Ok(
            match engine.push_ref(
                &report.repository,
                &options.remote,
                &capture.commit,
                &report.refs.live,
                Some(&remote),
            )? {
                GitPushResult::Updated => {
                    Some((capture.commit.clone(), GitSyncOutcome::Pushed, true))
                }
                GitPushResult::Rejected => None,
            },
        );
    }
    if engine.is_ancestor(&report.repository, &capture.commit, &remote)? {
        return Ok(Some((remote, GitSyncOutcome::Pulled, false)));
    }
    merge_divergence(engine, options, report, capture, remote)
}

fn merge_divergence(
    engine: &dyn GitEngine,
    options: &GitSyncOptions,
    report: &mut GitSyncReport,
    capture: &crate::GitCapture,
    remote: GitOid,
) -> Result<Option<(GitOid, GitSyncOutcome, bool)>, GitSyncError> {
    let merge = engine.merge_commits(&report.repository, &remote, &capture.commit)?;
    if !merge.clean {
        report.outcome = GitSyncOutcome::Conflicted;
        report.conflict = Some(GitSyncConflict {
            remote,
            local: capture.commit.clone(),
            merge_tree: merge.tree,
            diagnostics: merge.diagnostics,
        });
        return Ok(None);
    }
    let tree = merge.tree.ok_or_else(|| {
        GitSyncError::Git(GitEngineError::InvalidOutput {
            operation: "merge live sync commits",
            detail: "the clean merge report omitted its tree".to_string(),
        })
    })?;
    let merged = engine.create_commit(
        &report.repository,
        &tree,
        &[remote.clone(), capture.commit.clone()],
        &merge_message(&report.refs),
    )?;
    engine.update_ref(&report.repository, &report.refs.pending, &merged)?;
    Ok(
        match engine.push_ref(
            &report.repository,
            &options.remote,
            &merged,
            &report.refs.live,
            Some(&remote),
        )? {
            GitPushResult::Updated => Some((merged, GitSyncOutcome::Merged, true)),
            GitPushResult::Rejected => None,
        },
    )
}

fn snapshot_message(refs: &GitSyncRefs) -> String {
    format!(
        "vulcan live snapshot\n\nVulcan-Sync-Version: {SYNC_PROTOCOL_VERSION}\nVulcan-Sync-Profile: {}\n",
        refs.local
            .as_str()
            .split('/')
            .nth(3)
            .unwrap_or("unknown")
    )
}

fn merge_message(refs: &GitSyncRefs) -> String {
    format!(
        "vulcan live merge\n\nVulcan-Sync-Version: {SYNC_PROTOCOL_VERSION}\nVulcan-Sync-Profile: {}\n",
        refs.local
            .as_str()
            .split('/')
            .nth(3)
            .unwrap_or("unknown")
    )
}

struct RepositoryLock {
    _file: File,
}

impl RepositoryLock {
    fn acquire(repository: &GitRepository) -> Result<Self, GitSyncError> {
        let lock_path = repository.git_dir.join("vulcan-sync/sync.lock");
        fs::create_dir_all(
            lock_path
                .parent()
                .expect("the sync lock path always has a parent"),
        )?;
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(lock_path)?;
        file.try_lock_exclusive().map_err(|error| {
            if error.kind() == std::io::ErrorKind::WouldBlock {
                GitSyncError::Locked
            } else {
                GitSyncError::Io(error)
            }
        })?;
        Ok(Self { _file: file })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GitCliEngine;
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;
    use tempfile::TempDir;

    fn run_git(path: &Path, arguments: &[&str]) {
        let status = Command::new("git")
            .current_dir(path)
            .args(arguments)
            .status()
            .expect("Git should launch");
        assert!(status.success(), "Git failed: {arguments:?}");
    }

    fn init_repo(path: &Path) {
        run_git(path, &["-c", "init.defaultBranch=main", "init", "--quiet"]);
        run_git(path, &["config", "user.name", "Vulcan Test"]);
        run_git(path, &["config", "user.email", "vulcan@example.invalid"]);
    }

    fn commit_all(path: &Path, message: &str) {
        run_git(path, &["add", "--all", "--", "."]);
        run_git(path, &["commit", "--quiet", "-m", message]);
    }

    fn setup_remote_and_writer() -> (TempDir, PathBuf, PathBuf) {
        let temporary = TempDir::new().expect("temporary directory");
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
            &[
                "remote",
                "add",
                "origin",
                remote.to_str().expect("remote path"),
            ],
        );
        fs::write(writer.join("Home.md"), "initial\n").expect("initial note");
        commit_all(&writer, "initial");
        (temporary, remote, writer)
    }

    fn clone_reader(temporary: &TempDir, remote: &Path, writer: &Path) -> PathBuf {
        let reader = temporary.path().join("reader");
        run_git(
            temporary.path(),
            &[
                "clone",
                "--quiet",
                writer.to_str().expect("writer path"),
                reader.to_str().expect("reader path"),
            ],
        );
        run_git(
            &reader,
            &[
                "remote",
                "set-url",
                "origin",
                remote.to_str().expect("remote path"),
            ],
        );
        run_git(&reader, &["config", "user.name", "Vulcan Test"]);
        run_git(&reader, &["config", "user.email", "vulcan@example.invalid"]);
        reader
    }

    #[test]
    fn first_sync_bootstraps_the_remote_live_ref() {
        let (_temporary, _remote, writer) = setup_remote_and_writer();

        let report = sync_git_once(
            &GitCliEngine::default(),
            &writer,
            &GitSyncOptions::default(),
        )
        .expect("sync should succeed");

        assert_eq!(report.outcome, GitSyncOutcome::Bootstrapped);
        assert!(report.actions.contains(&GitSyncAction::Pushed));
        assert!(!report.actions.contains(&GitSyncAction::WorktreeApplied));
        assert_eq!(report.accepted, report.local_snapshot);
    }

    #[test]
    fn dry_run_does_not_create_local_refs_or_remote_state() {
        let (_temporary, _remote, writer) = setup_remote_and_writer();
        let options = GitSyncOptions {
            dry_run: true,
            ..GitSyncOptions::default()
        };
        let engine = GitCliEngine::default();

        let report = sync_git_once(&engine, &writer, &options).expect("plan should succeed");

        assert_eq!(report.outcome, GitSyncOutcome::Planned);
        assert_eq!(report.remote_before, None);
        assert_eq!(
            engine
                .read_ref(&report.repository, &report.refs.local)
                .expect("local ref"),
            None
        );
        assert_eq!(
            engine
                .remote_ref(&report.repository, &options.remote, &options.live_ref)
                .expect("remote ref"),
            None
        );
    }

    #[test]
    fn staged_changes_pause_before_any_snapshot_or_push() {
        let (_temporary, _remote, writer) = setup_remote_and_writer();
        fs::write(writer.join("Home.md"), "staged\n").expect("staged note");
        run_git(&writer, &["add", "Home.md"]);

        let report = sync_git_once(
            &GitCliEngine::default(),
            &writer,
            &GitSyncOptions::default(),
        )
        .expect("paused sync should report normally");

        assert_eq!(report.outcome, GitSyncOutcome::Paused);
        assert!(report.safety.staged_changes);
        assert_eq!(report.local_snapshot, None);
    }

    #[test]
    fn two_worktrees_pull_and_merge_non_overlapping_changes() {
        let (temporary, remote, writer) = setup_remote_and_writer();
        let engine = GitCliEngine::default();
        sync_git_once(&engine, &writer, &GitSyncOptions::default()).expect("bootstrap sync");
        let reader = clone_reader(&temporary, &remote, &writer);

        fs::write(writer.join("Writer.md"), "from writer\n").expect("writer edit");
        let writer_report =
            sync_git_once(&engine, &writer, &GitSyncOptions::default()).expect("writer push");
        assert_eq!(writer_report.outcome, GitSyncOutcome::Pushed);

        let pull_report =
            sync_git_once(&engine, &reader, &GitSyncOptions::default()).expect("reader pull");
        assert_eq!(pull_report.outcome, GitSyncOutcome::Pulled);
        assert_eq!(
            fs::read_to_string(reader.join("Writer.md")).expect("pulled note"),
            "from writer\n"
        );

        fs::write(writer.join("Writer.md"), "writer revision\n").expect("writer revision");
        fs::write(reader.join("Reader.md"), "from reader\n").expect("reader edit");
        sync_git_once(&engine, &writer, &GitSyncOptions::default()).expect("writer push");
        let merge_report =
            sync_git_once(&engine, &reader, &GitSyncOptions::default()).expect("reader merge");
        assert_eq!(merge_report.outcome, GitSyncOutcome::Merged);
        assert!(merge_report.actions.contains(&GitSyncAction::Pushed));
        assert!(merge_report
            .actions
            .contains(&GitSyncAction::WorktreeApplied));
        assert_eq!(
            fs::read_to_string(reader.join("Writer.md")).expect("merged writer note"),
            "writer revision\n"
        );
        assert_eq!(
            fs::read_to_string(reader.join("Reader.md")).expect("local reader note"),
            "from reader\n"
        );

        let convergence = sync_git_once(&engine, &writer, &GitSyncOptions::default())
            .expect("writer convergence pull");
        assert_eq!(convergence.outcome, GitSyncOutcome::Pulled);
        assert_eq!(
            fs::read_to_string(writer.join("Reader.md")).expect("converged reader note"),
            "from reader\n"
        );
    }

    #[test]
    fn conflicting_edits_are_reported_without_overwriting_local_bytes() {
        let (temporary, remote, writer) = setup_remote_and_writer();
        let engine = GitCliEngine::default();
        sync_git_once(&engine, &writer, &GitSyncOptions::default()).expect("bootstrap sync");
        let reader = clone_reader(&temporary, &remote, &writer);
        sync_git_once(&engine, &reader, &GitSyncOptions::default()).expect("reader baseline");

        fs::write(writer.join("Home.md"), "writer version\n").expect("writer edit");
        fs::write(reader.join("Home.md"), "reader version\n").expect("reader edit");
        sync_git_once(&engine, &writer, &GitSyncOptions::default()).expect("writer push");
        let report =
            sync_git_once(&engine, &reader, &GitSyncOptions::default()).expect("conflict report");

        assert_eq!(report.outcome, GitSyncOutcome::Conflicted);
        assert!(report.conflict.is_some());
        assert_eq!(
            fs::read_to_string(reader.join("Home.md")).expect("preserved reader note"),
            "reader version\n"
        );
        assert_eq!(
            engine
                .read_ref(&report.repository, &report.refs.local)
                .expect("preserved local ref"),
            report.local_snapshot
        );
    }
}
