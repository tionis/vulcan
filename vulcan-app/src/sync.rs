//! Complete direct-mode vault synchronization workflows.

use crate::sync_conflicts::{SyncConflictRecord, SyncConflictStore};
use crate::sync_state::{SyncApplyMarker, SyncJournal, SyncJournalPhase, SyncStateStore};
use crate::{scan::refresh_cache_incrementally, AppError};
use fs2::FileExt;
use serde::Serialize;
use std::fs::OpenOptions;
use vulcan_core::{ScanSummary, VaultPaths};
use vulcan_sync::{GitEngine, GitSyncObserver};

pub use vulcan_sync::{
    GitCloneRequest, GitInstallation, GitObjectFormat, GitPlatformPolicy, GitPlatformProfile,
    GitRefName, GitRemote, GitRepository, GitRepositoryLayout, GitRepositoryRequirements,
    GitSyncAction, GitSyncConflict, GitSyncDeviceId, GitSyncObserverError, GitSyncOptions,
    GitSyncOutcome, GitSyncPause, GitSyncPauseReason, GitSyncPhase, GitSyncProgress, GitSyncRefs,
    GitSyncReport, SyncCancellationToken,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conflict_record: Option<SyncConflictRecord>,
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

pub const SYNC_DOCTOR_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncDoctorSeverity {
    Pass,
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SyncDoctorCheck {
    pub code: String,
    pub severity: SyncDoctorSeverity,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SyncDoctorReport {
    pub version: u32,
    pub healthy: bool,
    pub vault: std::path::PathBuf,
    pub remote: GitRemote,
    pub live_ref: GitRefName,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub installation: Option<GitInstallation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository: Option<GitRepository>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_revision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requirements: Option<GitRepositoryRequirements>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub journal: Option<SyncJournal>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub apply_marker: Option<SyncApplyMarker>,
    pub checks: Vec<SyncDoctorCheck>,
}

/// Inspects a Git-backed vault and its device-local recovery state without mutation.
#[must_use]
pub fn doctor_git_vault(paths: &VaultPaths, options: &GitSyncOptions) -> SyncDoctorReport {
    let state_store = SyncStateStore::user_default().ok();
    doctor_git_vault_with_optional_state(paths, options, state_store.as_ref())
}

#[must_use]
pub fn doctor_git_vault_with_state_store(
    paths: &VaultPaths,
    options: &GitSyncOptions,
    state_store: &SyncStateStore,
) -> SyncDoctorReport {
    doctor_git_vault_with_optional_state(paths, options, Some(state_store))
}

fn doctor_git_vault_with_optional_state(
    paths: &VaultPaths,
    options: &GitSyncOptions,
    state_store: Option<&SyncStateStore>,
) -> SyncDoctorReport {
    let engine = vulcan_sync::GitCliEngine::default();
    let mut report = initial_doctor_report(paths, options);
    doctor_device_identity(state_store, &mut report);

    match engine.installation() {
        Ok(installation) => {
            doctor_check(
                &mut report,
                "git.installation",
                SyncDoctorSeverity::Pass,
                format!(
                    "using {} version {}",
                    installation.executable.display(),
                    installation.version.raw
                ),
            );
            report.installation = Some(installation);
        }
        Err(error) => {
            doctor_check(
                &mut report,
                "git.installation",
                SyncDoctorSeverity::Error,
                format!(
                    "{error}; install Git with the Linux system package manager, Git for Windows, or `pkg install git` in Termux, then ensure it is available on PATH"
                ),
            );
            return finish_doctor_without_repository(paths, state_store, report);
        }
    }

    let repository = match engine.discover_repository(paths.vault_root()) {
        Ok(repository) => repository,
        Err(error) => {
            doctor_check(
                &mut report,
                "git.repository",
                SyncDoctorSeverity::Error,
                error.to_string(),
            );
            return finish_doctor_without_repository(paths, state_store, report);
        }
    };
    doctor_repository_layout(&mut report, &repository);
    report.repository = Some(repository.clone());

    match engine.safety_state(&repository) {
        Ok(safety) if safety.staged_changes => doctor_check(
            &mut report,
            "git.safety",
            SyncDoctorSeverity::Warning,
            "the normal Git index has staged changes; worktree application will pause",
        ),
        Ok(safety) if safety.operation.is_some() => doctor_check(
            &mut report,
            "git.safety",
            SyncDoctorSeverity::Warning,
            format!(
                "a Git {} operation is in progress; worktree application will pause",
                safety.operation.as_deref().unwrap_or("unknown")
            ),
        ),
        Ok(_) => doctor_check(
            &mut report,
            "git.safety",
            SyncDoctorSeverity::Pass,
            "the normal index and repository operation state permit synchronization",
        ),
        Err(error) => doctor_check(
            &mut report,
            "git.safety",
            SyncDoctorSeverity::Error,
            error.to_string(),
        ),
    }

    match engine.repository_requirements(&repository) {
        Ok(requirements) => {
            doctor_repository_requirements(&mut report, &requirements);
            report.requirements = Some(requirements);
        }
        Err(error) => doctor_check(
            &mut report,
            "git.requirements",
            SyncDoctorSeverity::Error,
            error.to_string(),
        ),
    }

    doctor_refs(&engine, &repository, options, &mut report);
    doctor_repository_lock(&repository, &mut report);
    doctor_journal(paths, state_store, &mut report);
    doctor_apply_marker(state_store, &repository, &mut report);
    doctor_cache(paths, &mut report);
    finish_doctor_report(report)
}

fn initial_doctor_report(paths: &VaultPaths, options: &GitSyncOptions) -> SyncDoctorReport {
    SyncDoctorReport {
        version: SYNC_DOCTOR_VERSION,
        healthy: true,
        vault: paths.vault_root().to_path_buf(),
        remote: options.remote.clone(),
        live_ref: options.live_ref.clone(),
        device_id: None,
        installation: None,
        repository: None,
        remote_revision: None,
        requirements: None,
        journal: None,
        apply_marker: None,
        checks: Vec::new(),
    }
}

fn doctor_device_identity(state_store: Option<&SyncStateStore>, report: &mut SyncDoctorReport) {
    let Some(state_store) = state_store else {
        doctor_check(
            report,
            "sync.device-identity",
            SyncDoctorSeverity::Info,
            "device-local sync state is unavailable; no identity was created",
        );
        return;
    };
    match state_store.load_or_create_device_id(false) {
        Ok(Some(device_id)) => {
            report.device_id = Some(device_id.as_str().to_string());
            doctor_check(
                report,
                "sync.device-identity",
                SyncDoctorSeverity::Pass,
                format!(
                    "stable device identity `{}` is available",
                    device_id.as_str()
                ),
            );
        }
        Ok(None) => doctor_check(
            report,
            "sync.device-identity",
            SyncDoctorSeverity::Info,
            "device identity will be created by the first mutating sync; doctor made no changes",
        ),
        Err(error) => doctor_check(
            report,
            "sync.device-identity",
            SyncDoctorSeverity::Error,
            error.to_string(),
        ),
    }
}

fn finish_doctor_without_repository(
    paths: &VaultPaths,
    state_store: Option<&SyncStateStore>,
    mut report: SyncDoctorReport,
) -> SyncDoctorReport {
    doctor_journal(paths, state_store, &mut report);
    doctor_cache(paths, &mut report);
    finish_doctor_report(report)
}

fn doctor_repository_layout(report: &mut SyncDoctorReport, repository: &GitRepository) {
    let (severity, message) = match repository.layout {
        GitRepositoryLayout::Colocated => (
            SyncDoctorSeverity::Pass,
            format!(
                "colocated Git directory at {}",
                repository.git_dir.display()
            ),
        ),
        GitRepositoryLayout::Detached => (
            SyncDoctorSeverity::Pass,
            format!("detached Git directory at {}", repository.git_dir.display()),
        ),
        GitRepositoryLayout::Bare => (
            SyncDoctorSeverity::Error,
            "bare repositories cannot materialize a synchronized vault worktree".to_string(),
        ),
        _ => (
            SyncDoctorSeverity::Error,
            "the repository layout is not supported by this Vulcan version".to_string(),
        ),
    };
    doctor_check(report, "git.layout", severity, message);
    let (severity, message) = match &repository.object_format {
        GitObjectFormat::Sha1 => (SyncDoctorSeverity::Pass, "SHA-1 object format".to_string()),
        GitObjectFormat::Sha256 => (
            SyncDoctorSeverity::Pass,
            "SHA-256 object format".to_string(),
        ),
        GitObjectFormat::Other(format) => (
            SyncDoctorSeverity::Warning,
            format!("unrecognized Git object format `{format}`"),
        ),
        _ => (
            SyncDoctorSeverity::Warning,
            "unrecognized Git object format".to_string(),
        ),
    };
    doctor_check(report, "git.object-format", severity, message);
}

fn doctor_repository_requirements(
    report: &mut SyncDoctorReport,
    requirements: &GitRepositoryRequirements,
) {
    let required_ignores = 3;
    if requirements.ignored_internal_paths.len() == required_ignores {
        doctor_check(
            report,
            "git.internal-ignore",
            SyncDoctorSeverity::Pass,
            "rebuildable cache database files are ignored by Git",
        );
    } else {
        doctor_check(
            report,
            "git.internal-ignore",
            SyncDoctorSeverity::Warning,
            "add .vulcan/cache.db* to an applicable .gitignore before synchronizing",
        );
    }
    if requirements.required_filters.is_empty() {
        doctor_check(
            report,
            "git.filters",
            SyncDoctorSeverity::Pass,
            "tracked paths do not require Git clean/smudge filters",
        );
    } else {
        let filters = requirements
            .required_filters
            .iter()
            .map(|filter| format!("{} ({})", filter.name, filter.path_count))
            .collect::<Vec<_>>()
            .join(", ");
        let severity = if requirements.git_lfs_available == Some(false) {
            SyncDoctorSeverity::Error
        } else {
            SyncDoctorSeverity::Info
        };
        doctor_check(
            report,
            "git.filters",
            severity,
            format!("required Git filters: {filters}"),
        );
    }
}

fn doctor_refs(
    engine: &dyn GitEngine,
    repository: &GitRepository,
    options: &GitSyncOptions,
    report: &mut SyncDoctorReport,
) {
    let refs = match GitSyncRefs::for_options(options) {
        Ok(refs) => refs,
        Err(error) => {
            doctor_check(
                report,
                "git.refs",
                SyncDoctorSeverity::Error,
                error.to_string(),
            );
            return;
        }
    };
    let mut revisions = Vec::new();
    for (name, reference) in [
        ("local", &refs.local),
        ("fetched", &refs.fetched),
        ("pending", &refs.pending),
    ] {
        match engine.read_ref(repository, reference) {
            Ok(Some(revision)) => match engine.tree_oid(repository, &revision) {
                Ok(_) => revisions.push((name, revision)),
                Err(error) => doctor_check(
                    report,
                    "git.objects",
                    SyncDoctorSeverity::Error,
                    format!("{name} ref points to an unreadable object: {error}"),
                ),
            },
            Ok(None) => {}
            Err(error) => doctor_check(
                report,
                "git.refs",
                SyncDoctorSeverity::Error,
                format!("cannot read {name} ref: {error}"),
            ),
        }
    }
    if revisions.is_empty() {
        doctor_check(
            report,
            "git.refs",
            SyncDoctorSeverity::Info,
            "no local Vulcan sync refs exist yet",
        );
    } else if revisions.len() == 3 && revisions.windows(2).all(|pair| pair[0].1 == pair[1].1) {
        doctor_check(
            report,
            "git.refs",
            SyncDoctorSeverity::Pass,
            format!("{} readable local sync ref(s) agree", revisions.len()),
        );
    } else {
        doctor_check(
            report,
            "git.refs",
            SyncDoctorSeverity::Warning,
            "local, fetched, and pending sync refs do not yet agree",
        );
    }

    let local_revision = revisions
        .iter()
        .find(|(name, _)| *name == "local")
        .map(|(_, revision)| revision.clone());
    match engine.remote_ref(repository, &options.remote, &refs.live) {
        Ok(Some(revision)) => {
            report.remote_revision = Some(revision.to_string());
            if local_revision
                .as_ref()
                .is_some_and(|local| local != &revision)
            {
                doctor_check(
                    report,
                    "git.remote",
                    SyncDoctorSeverity::Warning,
                    format!("remote live ref {revision} differs from the local candidate"),
                );
            } else {
                doctor_check(
                    report,
                    "git.remote",
                    SyncDoctorSeverity::Pass,
                    format!("remote live ref resolves to {revision}"),
                );
            }
        }
        Ok(None) => doctor_check(
            report,
            "git.remote",
            SyncDoctorSeverity::Info,
            "remote live ref does not exist yet; the first sync can bootstrap it",
        ),
        Err(error) => doctor_check(
            report,
            "git.remote",
            SyncDoctorSeverity::Warning,
            format!("remote could not be inspected: {error}"),
        ),
    }
}

fn doctor_repository_lock(repository: &GitRepository, report: &mut SyncDoctorReport) {
    let path = repository.git_dir.join("vulcan-sync/sync.lock");
    let file = match OpenOptions::new().read(true).write(true).open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            doctor_check(
                report,
                "git.lock",
                SyncDoctorSeverity::Pass,
                "no sync cycle currently holds the repository lock",
            );
            return;
        }
        Err(error) => {
            doctor_check(
                report,
                "git.lock",
                SyncDoctorSeverity::Warning,
                format!("cannot inspect {}: {error}", path.display()),
            );
            return;
        }
    };
    match file.try_lock_exclusive() {
        Ok(()) => doctor_check(
            report,
            "git.lock",
            SyncDoctorSeverity::Pass,
            "the persistent lock file is currently unlocked",
        ),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => doctor_check(
            report,
            "git.lock",
            SyncDoctorSeverity::Info,
            "a sync cycle currently holds the repository lock",
        ),
        Err(error) => doctor_check(
            report,
            "git.lock",
            SyncDoctorSeverity::Warning,
            format!("cannot test the repository lock: {error}"),
        ),
    }
}

fn doctor_journal(
    paths: &VaultPaths,
    state_store: Option<&SyncStateStore>,
    report: &mut SyncDoctorReport,
) {
    let work_tree = match std::fs::canonicalize(paths.vault_root()) {
        Ok(path) => path,
        Err(error) => {
            doctor_check(
                report,
                "state.journal",
                SyncDoctorSeverity::Error,
                error.to_string(),
            );
            return;
        }
    };
    let key = crate::sync_state::repository_state_key(&work_tree);
    let Some(store) = state_store else {
        doctor_check(
            report,
            "state.journal",
            SyncDoctorSeverity::Warning,
            "cannot determine the user-state directory; set XDG_STATE_HOME or HOME",
        );
        return;
    };
    match store.load(&key) {
        Ok(Some(journal)) => {
            let severity = if journal.phase.requires_recovery()
                || journal.phase == SyncJournalPhase::Conflicted
            {
                SyncDoctorSeverity::Warning
            } else {
                SyncDoctorSeverity::Info
            };
            doctor_check(
                report,
                "state.journal",
                severity,
                format!(
                    "retained transaction {} is in {:?} phase at {}",
                    journal.transaction_id,
                    journal.phase,
                    store.journal_path(&key).map_or_else(
                        |_| "<invalid path>".to_string(),
                        |path| path.display().to_string()
                    )
                ),
            );
            report.journal = Some(journal);
        }
        Ok(None) => doctor_check(
            report,
            "state.journal",
            SyncDoctorSeverity::Pass,
            "no retained transaction journal requires review",
        ),
        Err(error) => doctor_check(
            report,
            "state.journal",
            SyncDoctorSeverity::Error,
            error.to_string(),
        ),
    }
}

fn doctor_apply_marker(
    state_store: Option<&SyncStateStore>,
    repository: &GitRepository,
    report: &mut SyncDoctorReport,
) {
    let Some(store) = state_store else {
        return;
    };
    match store.load_apply_marker(&repository.git_dir) {
        Ok(Some(marker)) => {
            doctor_check(
                report,
                "state.apply-marker",
                SyncDoctorSeverity::Error,
                format!(
                    "transaction {} may have been interrupted while applying {} over {}; rerun sync to recapture and verify the worktree",
                    marker.transaction_id, marker.accepted, marker.expected_revision
                ),
            );
            report.apply_marker = Some(marker);
        }
        Ok(None) => doctor_check(
            report,
            "state.apply-marker",
            SyncDoctorSeverity::Pass,
            "no interrupted worktree application marker is present",
        ),
        Err(error) => doctor_check(
            report,
            "state.apply-marker",
            SyncDoctorSeverity::Error,
            error.to_string(),
        ),
    }
}

fn doctor_cache(paths: &VaultPaths, report: &mut SyncDoctorReport) {
    if !paths.cache_db().exists() {
        doctor_check(
            report,
            "cache.coherence",
            SyncDoctorSeverity::Info,
            "the optional rebuildable cache is not initialized",
        );
        return;
    }
    match vulcan_core::doctor_vault(paths) {
        Ok(cache)
            if cache.summary.stale_index_rows == 0 && cache.summary.missing_index_rows == 0 =>
        {
            doctor_check(
                report,
                "cache.coherence",
                SyncDoctorSeverity::Pass,
                "the cache file inventory agrees with the materialized vault",
            );
        }
        Ok(cache) => doctor_check(
            report,
            "cache.coherence",
            SyncDoctorSeverity::Warning,
            format!(
                "cache inventory has {} stale and {} missing path(s); run vulcan scan",
                cache.summary.stale_index_rows, cache.summary.missing_index_rows
            ),
        ),
        Err(error) => doctor_check(
            report,
            "cache.coherence",
            SyncDoctorSeverity::Error,
            error.to_string(),
        ),
    }
}

fn doctor_check(
    report: &mut SyncDoctorReport,
    code: &str,
    severity: SyncDoctorSeverity,
    message: impl Into<String>,
) {
    report.checks.push(SyncDoctorCheck {
        code: code.to_string(),
        severity,
        message: message.into(),
    });
}

fn finish_doctor_report(mut report: SyncDoctorReport) -> SyncDoctorReport {
    report.healthy = report
        .checks
        .iter()
        .all(|check| check.severity != SyncDoctorSeverity::Error);
    report
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
    let mut observer = vulcan_sync::IgnoreGitSyncProgress;
    sync_git_vault_with_observer(paths, options, state_store, cancellation, &mut observer)
}

/// Runs one finite Git synchronization cycle while forwarding durable progress
/// to a caller-owned observer after each journal transition is persisted.
pub fn sync_git_vault_with_observer(
    paths: &VaultPaths,
    options: &GitSyncOptions,
    state_store: &SyncStateStore,
    cancellation: &SyncCancellationToken,
    delegate: &mut dyn GitSyncObserver,
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
    let mut effective_options = options.clone();
    effective_options.device_id = state_store
        .load_or_create_device_id(!options.dry_run)?
        .unwrap_or_else(GitSyncDeviceId::anonymous);
    if !options.dry_run {
        state_store.save(&journal)?;
    }
    let mut observer = JournalSyncObserver {
        state_store,
        journal: &mut journal,
        persist: !options.dry_run,
        delegate,
    };
    let sync = match vulcan_sync::sync_git_once_with_control(
        &engine,
        paths.vault_root(),
        &effective_options,
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
    let conflict_record =
        persist_sync_conflict(&engine, &sync, &mut journal, state_store, !options.dry_run)?;
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
        conflict_record,
        state: VaultSyncStateReport {
            repository_key,
            journal_path,
            recovered_from,
            retained,
        },
    })
}

fn persist_sync_conflict(
    engine: &dyn GitEngine,
    sync: &GitSyncReport,
    journal: &mut SyncJournal,
    state_store: &SyncStateStore,
    persist_journal: bool,
) -> Result<Option<SyncConflictRecord>, AppError> {
    let result = sync
        .conflict
        .as_ref()
        .map(|conflict| {
            SyncConflictStore::from_state_store(state_store).persist(
                engine,
                &sync.repository,
                &journal.repository_key,
                conflict,
            )
        })
        .transpose();
    match result {
        Ok(record) => Ok(record),
        Err(error) => {
            journal.error = Some(error.to_string());
            if persist_journal {
                state_store.save(journal)?;
            }
            Err(error)
        }
    }
}

struct JournalSyncObserver<'a> {
    state_store: &'a SyncStateStore,
    journal: &'a mut SyncJournal,
    persist: bool,
    delegate: &'a mut dyn GitSyncObserver,
}

impl GitSyncObserver for JournalSyncObserver<'_> {
    fn progress(&mut self, progress: &GitSyncProgress) -> Result<(), GitSyncObserverError> {
        self.journal.phase = match progress.phase {
            GitSyncPhase::Preparing => SyncJournalPhase::Preparing,
            GitSyncPhase::Capturing => SyncJournalPhase::Capturing,
            GitSyncPhase::Captured => SyncJournalPhase::Captured,
            GitSyncPhase::Fetching => SyncJournalPhase::Fetching,
            GitSyncPhase::Fetched => SyncJournalPhase::Fetched,
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
            if progress.phase == GitSyncPhase::Applying {
                let marker = SyncApplyMarker::from_journal(self.journal)
                    .map_err(|error| GitSyncObserverError::new(error.to_string()))?;
                self.state_store
                    .save_apply_marker(&progress.repository.git_dir, &marker)
                    .map_err(|error| GitSyncObserverError::new(error.to_string()))?;
            } else if progress.phase == GitSyncPhase::Completed {
                self.state_store
                    .clear_apply_marker(&progress.repository.git_dir)
                    .map_err(|error| GitSyncObserverError::new(error.to_string()))?;
            }
        }
        self.delegate.progress(progress)
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

    fn git_stdout(path: &Path, arguments: &[&str]) -> String {
        let output = Command::new("git")
            .current_dir(path)
            .args(arguments)
            .output()
            .expect("Git should launch");
        assert!(output.status.success(), "Git failed: {arguments:?}");
        String::from_utf8(output.stdout)
            .expect("Git output should be UTF-8")
            .trim()
            .to_string()
    }

    fn assert_conflict_read_workflows(
        paths: &VaultPaths,
        store: &SyncStateStore,
        record: &SyncConflictRecord,
    ) {
        let listed = crate::sync_conflicts::list_sync_conflicts_with_state_store(paths, store)
            .expect("list workflow");
        assert_eq!(listed.count, 1);
        assert_eq!(listed.conflicts[0].id, record.id);
        let detail =
            crate::sync_conflicts::get_sync_conflict_with_state_store(paths, &record.id, store)
                .expect("detail workflow");
        assert_eq!(&detail.record, record);
        let records = SyncConflictStore::from_state_store(store)
            .list(&record.repository_key)
            .expect("list conflict records");
        assert_eq!(records, [record.clone()]);
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
        let application = report
            .sync
            .application
            .as_ref()
            .expect("accepted tree application plan");
        assert_eq!(application.additions, 1);
        assert_eq!(application.updates, 0);
        assert_eq!(application.deletions, 0);
        assert_eq!(application.type_changes, 0);
        assert_eq!(application.paths[0].path, "Remote.md");
        assert_eq!(
            state_store
                .load_apply_marker(&report.sync.repository.git_dir)
                .expect("cleared apply marker"),
            None
        );
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
        assert!(!store.root().join("_device.json").exists());

        fs::write(writer.join("Home.md"), "changed before recovery\n").expect("changed note");

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
        assert!(store.root().join("_device.json").is_file());
        let device_id = store
            .load_or_create_device_id(false)
            .expect("load device identity")
            .expect("device identity");
        let snapshot = report.sync.local_snapshot.as_ref().expect("local snapshot");
        let message = git_stdout(&writer, &["show", "-s", "--format=%B", snapshot.as_str()]);
        assert!(message.contains(&format!("Vulcan-Sync-Device: {}", device_id.as_str())));
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
    fn staged_sync_retains_a_paused_journal_after_capture() {
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
                remote.to_str().expect("remote path"),
            ],
        );
        fs::write(vault.join("Home.md"), "initial\n").expect("initial note");
        git(&vault, &["add", "Home.md"]);
        git(&vault, &["commit", "--quiet", "-m", "initial"]);
        fs::write(vault.join("Home.md"), "staged\n").expect("staged note");
        git(&vault, &["add", "Home.md"]);
        let paths = VaultPaths::new(&vault);
        let store = SyncStateStore::at(temporary.path().join("state"));

        let report = sync_git_vault_with_state_store(&paths, &GitSyncOptions::default(), &store)
            .expect("paused sync");

        assert_eq!(report.sync.outcome, GitSyncOutcome::Paused);
        assert_eq!(
            report.sync.pause.as_ref().map(|pause| pause.reason),
            Some(GitSyncPauseReason::StagedChanges)
        );
        let journal = report.state.retained.expect("retained paused journal");
        assert_eq!(journal.phase, SyncJournalPhase::Paused);
        assert_eq!(
            journal.local_snapshot,
            report.sync.local_snapshot.as_ref().map(ToString::to_string)
        );
        assert_eq!(
            store
                .load(&report.state.repository_key)
                .expect("stored journal"),
            Some(journal)
        );
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

    #[test]
    fn sync_doctor_reports_clean_layout_refs_ignores_and_optional_cache() {
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
                remote.to_str().expect("remote path"),
            ],
        );
        fs::write(vault.join(".gitignore"), ".vulcan/cache.db*\n").expect("ignore file");
        fs::write(vault.join("Home.md"), "home\n").expect("home note");
        git(&vault, &["add", ".gitignore", "Home.md"]);
        git(&vault, &["commit", "--quiet", "-m", "initial"]);
        let paths = VaultPaths::new(&vault);
        let store = SyncStateStore::at(temporary.path().join("state"));

        let report = doctor_git_vault_with_state_store(&paths, &GitSyncOptions::default(), &store);

        assert!(report.healthy);
        assert_eq!(report.version, SYNC_DOCTOR_VERSION);
        assert!(report.installation.is_some());
        assert_eq!(
            report.repository.as_ref().map(|item| item.layout),
            Some(GitRepositoryLayout::Colocated)
        );
        assert!(report
            .requirements
            .as_ref()
            .is_some_and(|requirements| { requirements.ignored_internal_paths.len() == 3 }));
        assert!(report
            .checks
            .iter()
            .any(|check| check.code == "git.remote" && check.severity == SyncDoctorSeverity::Info));
        assert!(report.checks.iter().any(|check| {
            check.code == "sync.device-identity" && check.severity == SyncDoctorSeverity::Info
        }));
        assert!(report.checks.iter().any(|check| {
            check.code == "cache.coherence" && check.severity == SyncDoctorSeverity::Info
        }));
        assert!(!store.root().exists());
    }

    #[test]
    fn sync_doctor_surfaces_recovery_journals() {
        let temporary = tempdir().expect("temporary directory");
        let vault = temporary.path().join("vault");
        fs::create_dir(&vault).expect("vault directory");
        git(
            &vault,
            &["-c", "init.defaultBranch=main", "init", "--quiet"],
        );
        git(&vault, &["config", "user.name", "Vulcan Test"]);
        git(&vault, &["config", "user.email", "vulcan@example.invalid"]);
        fs::write(vault.join(".gitignore"), ".vulcan/cache.db*\n").expect("ignore file");
        fs::write(vault.join("Home.md"), "home\n").expect("home note");
        git(&vault, &["add", ".gitignore", "Home.md"]);
        git(&vault, &["commit", "--quiet", "-m", "initial"]);
        let paths = VaultPaths::new(&vault);
        let store = SyncStateStore::at(temporary.path().join("state"));
        let mut journal = SyncJournal::preparing(
            paths.vault_root(),
            "origin",
            "refs/heads/__vulcan-sync/live",
        )
        .expect("journal");
        journal.phase = SyncJournalPhase::Applying;
        let head = git_stdout(&vault, &["rev-parse", "HEAD"]);
        journal.local_snapshot = Some(head.clone());
        journal.accepted = Some(head);
        store.save(&journal).expect("save journal");
        let repository = vulcan_sync::GitCliEngine::default()
            .discover_repository(&vault)
            .expect("repository");
        let marker = SyncApplyMarker::from_journal(&journal).expect("apply marker");
        store
            .save_apply_marker(&repository.git_dir, &marker)
            .expect("save apply marker");

        let report = doctor_git_vault_with_state_store(&paths, &GitSyncOptions::default(), &store);

        assert_eq!(report.journal, Some(journal));
        assert_eq!(report.apply_marker, Some(marker));
        assert!(!report.healthy);
        assert!(report.checks.iter().any(|check| {
            check.code == "state.journal" && check.severity == SyncDoctorSeverity::Warning
        }));
        assert!(report.checks.iter().any(|check| {
            check.code == "state.apply-marker" && check.severity == SyncDoctorSeverity::Error
        }));
    }

    #[test]
    fn conflicted_sync_persists_immutable_records_and_all_file_sides() {
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
        fs::write(writer.join("Home.md"), "base\n").expect("base note");
        git(&writer, &["add", "Home.md"]);
        git(&writer, &["commit", "--quiet", "-m", "base"]);
        let store = SyncStateStore::at(temporary.path().join("state"));
        sync_git_vault_with_state_store(
            &VaultPaths::new(&writer),
            &GitSyncOptions::default(),
            &store,
        )
        .expect("bootstrap");

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
        sync_git_vault_with_state_store(
            &VaultPaths::new(&reader),
            &GitSyncOptions::default(),
            &store,
        )
        .expect("reader baseline");
        fs::write(writer.join("Home.md"), "writer\n").expect("writer edit");
        fs::write(reader.join("Home.md"), "reader\n").expect("reader edit");
        sync_git_vault_with_state_store(
            &VaultPaths::new(&writer),
            &GitSyncOptions::default(),
            &store,
        )
        .expect("writer sync");

        let report = sync_git_vault_with_state_store(
            &VaultPaths::new(&reader),
            &GitSyncOptions::default(),
            &store,
        )
        .expect("conflict report");
        let record = report.conflict_record.expect("durable conflict record");
        assert_eq!(record.paths.len(), 1);
        assert_eq!(record.paths[0].path, "Home.md");
        let conflict_root = store
            .root()
            .join(&record.repository_key)
            .join("conflicts")
            .join(&record.id);
        let read_artifact = |artifact: &Option<std::path::PathBuf>| {
            fs::read(conflict_root.join(artifact.as_ref().expect("artifact path")))
                .expect("artifact bytes")
        };
        assert_eq!(read_artifact(&record.paths[0].base.artifact), b"base\n");
        assert_eq!(read_artifact(&record.paths[0].local.artifact), b"reader\n");
        assert_eq!(read_artifact(&record.paths[0].remote.artifact), b"writer\n");
        assert_conflict_read_workflows(&VaultPaths::new(&reader), &store, &record);
        assert_eq!(
            fs::read_to_string(reader.join("Home.md")).expect("reader bytes"),
            "reader\n"
        );
    }
}
