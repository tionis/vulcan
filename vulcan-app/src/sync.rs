//! Complete direct-mode vault synchronization workflows.

use crate::sync_state::{SyncJournal, SyncJournalPhase, SyncStateStore};
use crate::{scan::refresh_cache_incrementally, AppError};
use serde::Serialize;
use vulcan_core::{ScanSummary, VaultPaths};
use vulcan_sync::{GitEngine, GitSyncObserver};

pub use vulcan_sync::{
    GitCloneRequest, GitInstallation, GitPlatformPolicy, GitPlatformProfile, GitRefName, GitRemote,
    GitRepository, GitRepositoryLayout, GitSyncAction, GitSyncConflict, GitSyncObserverError,
    GitSyncOptions, GitSyncOutcome, GitSyncPhase, GitSyncProgress, GitSyncRefs, GitSyncReport,
    SyncCancellationToken,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitCloneReport {
    pub installation: GitInstallation,
    pub repository: GitRepository,
}

/// Clones a Git-backed vault without requiring registration or a daemon.
pub fn clone_git_vault(request: &GitCloneRequest) -> Result<GitCloneReport, AppError> {
    let engine = vulcan_sync::GitCliEngine::default();
    let installation = engine.installation().map_err(AppError::operation)?;
    let repository = engine
        .clone_repository(request)
        .map_err(AppError::operation)?;
    Ok(GitCloneReport {
        installation,
        repository,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VaultSyncReport {
    #[serde(flatten)]
    pub sync: GitSyncReport,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_refresh: Option<ScanSummary>,
    pub state: VaultSyncStateReport,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VaultSyncStateReport {
    pub repository_key: String,
    pub journal_path: std::path::PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovered_from: Option<SyncJournal>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retained: Option<SyncJournal>,
}

/// Runs one finite Git synchronization cycle directly against a vault path.
///
/// The workflow does not require registration or a daemon. If an initialized
/// cache exists and the accepted tree changes local files, it refreshes that
/// derived cache only after the worktree has been verified and applied.
pub fn sync_git_vault(
    paths: &VaultPaths,
    options: &GitSyncOptions,
) -> Result<VaultSyncReport, AppError> {
    let state_store = SyncStateStore::user_default()?;
    sync_git_vault_with_state_store(paths, options, &state_store)
}

/// Runs one finite Git synchronization cycle using an explicit state store.
///
/// The explicit form supports embedding and isolated tests while preserving
/// the same crash-recovery behavior as the user-default workflow.
pub fn sync_git_vault_with_state_store(
    paths: &VaultPaths,
    options: &GitSyncOptions,
    state_store: &SyncStateStore,
) -> Result<VaultSyncReport, AppError> {
    sync_git_vault_with_control(
        paths,
        options,
        state_store,
        &SyncCancellationToken::default(),
    )
}

pub fn sync_git_vault_with_control(
    paths: &VaultPaths,
    options: &GitSyncOptions,
    state_store: &SyncStateStore,
    cancellation: &SyncCancellationToken,
) -> Result<VaultSyncReport, AppError> {
    if cancellation.is_cancelled() {
        return Err(AppError::operation(
            "synchronization was cancelled before the transaction started",
        ));
    }
    let mut journal = SyncJournal::preparing(
        paths.vault_root(),
        options.remote.to_string(),
        options.live_ref.to_string(),
    )?;
    let journal_path = state_store.journal_path(&journal.repository_key)?;
    let previous = state_store.load(&journal.repository_key)?;
    let recovered_from = previous
        .as_ref()
        .filter(|journal| journal.phase.requires_recovery())
        .cloned();
    let engine = vulcan_sync::GitCliEngine::default();
    if !options.dry_run {
        state_store.save(&journal)?;
    }
    let mut observer = JournalSyncObserver {
        state_store,
        journal: &mut journal,
        persist: !options.dry_run,
    };
    let sync = match vulcan_sync::sync_git_once_with_control(
        &engine,
        paths.vault_root(),
        options,
        cancellation,
        &mut observer,
    ) {
        Ok(sync) => sync,
        Err(error) => {
            if !options.dry_run {
                journal.error = Some(error.to_string());
                if let Err(state_error) = state_store.save(&journal) {
                    return Err(AppError::operation(format!(
                        "{error}; additionally failed to retain the recovery journal: {state_error}"
                    )));
                }
            }
            return Err(AppError::operation(error));
        }
    };
    journal.git_dir = Some(sync.repository.git_dir.clone());
    journal.local_snapshot = sync.local_snapshot.as_ref().map(ToString::to_string);
    journal.accepted = sync.accepted.as_ref().map(ToString::to_string);
    journal.phase = match sync.outcome {
        GitSyncOutcome::Paused => SyncJournalPhase::Paused,
        GitSyncOutcome::Conflicted => SyncJournalPhase::Conflicted,
        _ => SyncJournalPhase::Verifying,
    };
    if !options.dry_run {
        state_store.save(&journal)?;
    }
    let should_refresh = !options.dry_run
        && sync.actions.contains(&GitSyncAction::WorktreeApplied)
        && paths.cache_db().is_file();
    let cache_refresh = match should_refresh
        .then(|| refresh_cache_incrementally(paths))
        .transpose()
    {
        Ok(report) => report,
        Err(error) => {
            journal.error = Some(error.to_string());
            state_store.save(&journal)?;
            return Err(error);
        }
    };
    let repository_key = journal.repository_key.clone();
    let retained = if options.dry_run {
        previous
    } else if matches!(
        sync.outcome,
        GitSyncOutcome::Paused | GitSyncOutcome::Conflicted
    ) {
        Some(journal)
    } else {
        state_store.clear(&journal.repository_key)?;
        None
    };
    Ok(VaultSyncReport {
        sync,
        cache_refresh,
        state: VaultSyncStateReport {
            repository_key,
            journal_path,
            recovered_from,
            retained,
        },
    })
}

struct JournalSyncObserver<'a> {
    state_store: &'a SyncStateStore,
    journal: &'a mut SyncJournal,
    persist: bool,
}

impl GitSyncObserver for JournalSyncObserver<'_> {
    fn progress(&mut self, progress: &GitSyncProgress) -> Result<(), GitSyncObserverError> {
        self.journal.phase = match progress.phase {
            GitSyncPhase::Preparing => SyncJournalPhase::Preparing,
            GitSyncPhase::Capturing => SyncJournalPhase::Capturing,
            GitSyncPhase::Captured => SyncJournalPhase::Captured,
            GitSyncPhase::Fetching => SyncJournalPhase::Fetching,
            GitSyncPhase::Merging => SyncJournalPhase::Merging,
            GitSyncPhase::Pushing => SyncJournalPhase::Pushing,
            GitSyncPhase::Applying => SyncJournalPhase::Applying,
            GitSyncPhase::Verifying | GitSyncPhase::Completed => SyncJournalPhase::Verifying,
            GitSyncPhase::Paused => SyncJournalPhase::Paused,
            GitSyncPhase::Conflicted => SyncJournalPhase::Conflicted,
        };
        self.journal.git_dir = Some(progress.repository.git_dir.clone());
        self.journal.local_snapshot = progress.local_snapshot.as_ref().map(ToString::to_string);
        self.journal.expected_worktree_tree = progress.local_tree.as_ref().map(ToString::to_string);
        self.journal.accepted = progress.accepted.as_ref().map(ToString::to_string);
        self.journal.error = None;
        if self.persist {
            self.state_store
                .save(self.journal)
                .map_err(|error| GitSyncObserverError::new(error.to_string()))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;
    use std::process::Command;
    use tempfile::tempdir;
    use vulcan_core::{initialize_vulcan_dir, properties::load_note_index, scan_vault, ScanMode};

    fn git(path: &Path, arguments: &[&str]) {
        let status = Command::new("git")
            .current_dir(path)
            .args(arguments)
            .status()
            .expect("Git should launch");
        assert!(status.success(), "Git failed: {arguments:?}");
    }

    #[test]
    fn applied_remote_tree_refreshes_an_existing_cache() {
        let temporary = tempdir().expect("temporary directory");
        let remote = temporary.path().join("remote.git");
        git(
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
        git(
            &writer,
            &["-c", "init.defaultBranch=main", "init", "--quiet"],
        );
        git(&writer, &["config", "user.name", "Vulcan Test"]);
        git(&writer, &["config", "user.email", "vulcan@example.invalid"]);
        git(
            &writer,
            &[
                "remote",
                "add",
                "origin",
                remote.to_str().expect("remote path"),
            ],
        );
        fs::write(writer.join("Home.md"), "initial\n").expect("initial note");
        git(&writer, &["add", "Home.md"]);
        git(&writer, &["commit", "--quiet", "-m", "initial"]);
        let writer_paths = VaultPaths::new(&writer);
        let state_store = SyncStateStore::at(temporary.path().join("state"));
        sync_git_vault_with_state_store(&writer_paths, &GitSyncOptions::default(), &state_store)
            .expect("bootstrap sync");

        let reader = temporary.path().join("reader");
        git(
            temporary.path(),
            &[
                "clone",
                "--quiet",
                writer.to_str().expect("writer path"),
                reader.to_str().expect("reader path"),
            ],
        );
        git(
            &reader,
            &[
                "remote",
                "set-url",
                "origin",
                remote.to_str().expect("remote path"),
            ],
        );
        let reader_paths = VaultPaths::new(&reader);
        initialize_vulcan_dir(&reader_paths).expect("initialize reader cache");
        scan_vault(&reader_paths, ScanMode::Full).expect("initial reader scan");

        fs::write(writer.join("Remote.md"), "remote note\n").expect("remote note");
        sync_git_vault_with_state_store(&writer_paths, &GitSyncOptions::default(), &state_store)
            .expect("writer push");
        let report = sync_git_vault_with_state_store(
            &reader_paths,
            &GitSyncOptions::default(),
            &state_store,
        )
        .expect("reader synchronization");

        assert!(matches!(
            report.sync.outcome,
            GitSyncOutcome::Pulled | GitSyncOutcome::Merged
        ));
        assert!(report.cache_refresh.is_some());
        assert!(load_note_index(&reader_paths)
            .expect("reader index")
            .values()
            .any(|note| note.document_path == "Remote.md"));
    }

    #[test]
    fn direct_sync_recovers_and_clears_an_interrupted_journal() {
        let (temporary, _remote, writer) = {
            let temporary = tempdir().expect("temporary directory");
            let remote = temporary.path().join("remote.git");
            git(
                temporary.path(),
                &[
                    "init",
                    "--quiet",
                    "--bare",
                    remote.to_str().expect("remote"),
                ],
            );
            let writer = temporary.path().join("writer");
            fs::create_dir(&writer).expect("writer directory");
            git(
                &writer,
                &["-c", "init.defaultBranch=main", "init", "--quiet"],
            );
            git(&writer, &["config", "user.name", "Vulcan Test"]);
            git(&writer, &["config", "user.email", "vulcan@example.invalid"]);
            git(
                &writer,
                &["remote", "add", "origin", remote.to_str().expect("remote")],
            );
            fs::write(writer.join("Home.md"), "initial\n").expect("initial note");
            git(&writer, &["add", "Home.md"]);
            git(&writer, &["commit", "--quiet", "-m", "initial"]);
            (temporary, remote, writer)
        };
        let paths = VaultPaths::new(&writer);
        let store = SyncStateStore::at(temporary.path().join("state"));
        let mut interrupted =
            SyncJournal::preparing(&writer, "origin", "refs/heads/__vulcan-sync/live")
                .expect("journal");
        interrupted.phase = SyncJournalPhase::Applying;
        store.save(&interrupted).expect("interrupted journal");

        let planned = sync_git_vault_with_state_store(
            &paths,
            &GitSyncOptions {
                dry_run: true,
                ..GitSyncOptions::default()
            },
            &store,
        )
        .expect("recovery plan");
        assert_eq!(
            planned
                .state
                .recovered_from
                .as_ref()
                .map(|journal| journal.transaction_id),
            Some(interrupted.transaction_id)
        );
        assert_eq!(
            store
                .load(&interrupted.repository_key)
                .expect("load unchanged journal"),
            Some(interrupted.clone())
        );

        let report = sync_git_vault_with_state_store(&paths, &GitSyncOptions::default(), &store)
            .expect("recovering sync");

        assert_eq!(
            report
                .state
                .recovered_from
                .as_ref()
                .map(|journal| journal.transaction_id),
            Some(interrupted.transaction_id)
        );
        assert_eq!(
            store
                .load(&report.state.repository_key)
                .expect("load cleared journal"),
            None
        );
    }

    #[test]
    fn failed_sync_retains_an_error_journal() {
        let temporary = tempdir().expect("temporary directory");
        let vault = temporary.path().join("vault");
        fs::create_dir(&vault).expect("vault directory");
        let paths = VaultPaths::new(&vault);
        let store = SyncStateStore::at(temporary.path().join("state"));
        assert!(
            sync_git_vault_with_state_store(&paths, &GitSyncOptions::default(), &store).is_err()
        );

        let key = crate::sync_state::repository_state_key(
            &fs::canonicalize(&vault).expect("canonical vault"),
        );
        let journal = store
            .load(&key)
            .expect("load journal")
            .expect("retained error journal");
        assert_eq!(journal.phase, SyncJournalPhase::Preparing);
        assert!(journal.error.is_some());
    }

    #[test]
    fn progress_journal_retains_the_precise_failed_phase_and_snapshot() {
        let temporary = tempdir().expect("temporary directory");
        let vault = temporary.path().join("vault");
        fs::create_dir(&vault).expect("vault directory");
        git(
            &vault,
            &["-c", "init.defaultBranch=main", "init", "--quiet"],
        );
        git(&vault, &["config", "user.name", "Vulcan Test"]);
        git(&vault, &["config", "user.email", "vulcan@example.invalid"]);
        git(
            &vault,
            &[
                "remote",
                "add",
                "origin",
                temporary
                    .path()
                    .join("missing.git")
                    .to_str()
                    .expect("remote path"),
            ],
        );
        fs::write(vault.join("Home.md"), "initial\n").expect("initial note");
        git(&vault, &["add", "Home.md"]);
        git(&vault, &["commit", "--quiet", "-m", "initial"]);
        let paths = VaultPaths::new(&vault);
        let store = SyncStateStore::at(temporary.path().join("state"));

        assert!(
            sync_git_vault_with_state_store(&paths, &GitSyncOptions::default(), &store).is_err()
        );

        let key = crate::sync_state::repository_state_key(
            &fs::canonicalize(&vault).expect("canonical vault"),
        );
        let journal = store
            .load(&key)
            .expect("load journal")
            .expect("retained fetch journal");
        assert_eq!(journal.phase, SyncJournalPhase::Fetching);
        assert!(journal.local_snapshot.is_some());
        assert!(journal.git_dir.is_some());
        assert!(journal.error.is_some());
    }
}
