//! Complete direct-mode vault synchronization workflows.

use crate::sync_state::{SyncApplyMarker, SyncJournal, SyncJournalPhase, SyncStateStore};
use crate::{scan::refresh_cache_incrementally, AppError};
use fs2::FileExt;
use serde::Serialize;
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use vulcan_core::{load_vault_config, ScanSummary, VaultConfig, VaultPaths};
use vulcan_sync::GitEngine;

pub use vulcan_sync::{
    GitBranchSync, GitBranchSyncAction, GitCloneRequest, GitDetachedRecoveryReport,
    GitDetachedRecoveryRequest, GitDeviceBackup, GitDeviceBackupOutcome, GitInstallation,
    GitObjectFormat, GitPlatformPolicy, GitPlatformPreflight, GitPlatformProfile, GitRefName,
    GitRemote, GitRepository, GitRepositoryLayout, GitRepositoryRequirements, GitSyncAction,
    GitSyncConflict, GitSyncDeviceId, GitSyncObserver, GitSyncObserverError, GitSyncOptions,
    GitSyncOutcome, GitSyncPause, GitSyncPauseReason, GitSyncPhase, GitSyncProgress, GitSyncRefs,
    GitSyncReport, GitUnattendedRepositoryState, SyncCancellationToken, SyncJobState,
};

/// Controls whether a finite file synchronization cycle composes Markdown
/// knowledge services. The default preserves legacy vault behavior.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SyncContentProfile {
    #[default]
    Knowledge,
    FilesOnly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitCloneReport {
    pub installation: GitInstallation,
    pub repository: GitRepository,
}

/// Recreates a lost detached Git directory after anchoring the untouched
/// materialized worktree in the replacement object database.
pub fn recover_detached_git_vault(
    request: &GitDetachedRecoveryRequest,
) -> Result<GitDetachedRecoveryReport, AppError> {
    vulcan_sync::GitCliEngine::default()
        .recover_detached_repository(request)
        .map_err(AppError::operation)
}

/// Clones a Git-backed vault without requiring registration or a daemon.
pub fn clone_git_vault(request: &GitCloneRequest) -> Result<GitCloneReport, AppError> {
    clone_git_vault_with_ssh_command(request, None)
}

/// As [`clone_git_vault`], authenticating with an exact `GIT_SSH_COMMAND`
/// (the device key) when given. The command applies to this clone only: it is
/// not written into the repository's configuration.
pub fn clone_git_vault_with_ssh_command(
    request: &GitCloneRequest,
    ssh_command: Option<&str>,
) -> Result<GitCloneReport, AppError> {
    let engine = match ssh_command {
        Some(command) => vulcan_sync::GitCliEngine::default().with_ssh_command(command),
        None => vulcan_sync::GitCliEngine::default(),
    };
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
    pub cache_refresh_error: Option<String>,
    /// Present only when this sync created, claimed, or found revoked this
    /// device's registration, or when registering failed (never fatal).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub registration: Option<crate::sync_registration::SelfRegistrationReport>,
    pub operational_stats: SyncOperationalStats,
    pub state: VaultSyncStateReport,
    /// Per-device Obsidian files this sync still shares because no
    /// `.gitignore` ignores them; `vulcan sync doctor --fix` adds the entries.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub device_files: Vec<String>,
}

/// Explains a device backup that had to be bridged: the remote backup for
/// this device ID held history this checkout did not write. Once after a
/// re-clone or a Git directory recovery that is expected; repeatedly it means
/// two checkouts on this device, or two installations sharing a copy of the
/// device key, publish under the same ID. Both histories are always kept.
#[must_use]
pub fn device_backup_warning(backup: Option<&GitDeviceBackup>) -> Option<String> {
    let backup = backup.filter(|backup| {
        backup.outcome == GitDeviceBackupOutcome::Bridged && backup.foreign_history
    })?;
    Some(format!(
        "the remote backup of device `{}` held history this checkout did not write; both were kept. If this repeats, another checkout of this vault on this device or another installation with a copy of this device key is syncing under the same ID: remove the extra checkout, or run `vulcan device replace` on the other installation so it gets its own identity",
        backup.device_id.as_str()
    ))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SyncOperationalStats {
    pub version: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_subprocesses: Option<u64>,
    pub elapsed_ms: u64,
    pub backend_cycle_ms: u64,
    pub cache_refresh_ms: u64,
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
    pub platform_policy: GitPlatformPolicy,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform_preflight: Option<GitPlatformPreflight>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub journal: Option<SyncJournal>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub apply_marker: Option<SyncApplyMarker>,
    pub checks: Vec<SyncDoctorCheck>,
    /// Repairs `vulcan sync doctor --fix` applied, or would apply in a dry run.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub fixes: Vec<SyncDoctorFix>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SyncDoctorFix {
    /// The check the repair addresses.
    pub code: String,
    pub dry_run: bool,
    pub message: String,
}

/// The code of the check that `apply_sync_doctor_fixes` can repair.
pub const SYNC_DOCTOR_DEVICE_FILES: &str = "sync.device-files";
/// How the `sync.device-files` note starts while ignored device files are
/// still tracked, which `--fix` can untrack.
const DEVICE_FILES_PENDING: &str = "the .gitignore ignores";

/// Applies the deterministic repairs for the warnings in `report` and records
/// them in `report.fixes`. Today that is one: ignoring per-device Obsidian
/// files, which edits the vault's `.gitignore` exactly as
/// `vulcan sync ignore-device-files` does. Nothing else changes.
pub fn apply_sync_doctor_fixes(
    paths: &VaultPaths,
    report: &mut SyncDoctorReport,
    dry_run: bool,
) -> Result<(), AppError> {
    let needs_device_files = report.checks.iter().any(|check| {
        check.code == SYNC_DOCTOR_DEVICE_FILES
            && (check.severity == SyncDoctorSeverity::Warning
                || check.message.starts_with(DEVICE_FILES_PENDING))
    });
    if needs_device_files {
        let ignored = crate::sync_device_files::ignore_device_files(paths, dry_run)?;
        let mut steps = Vec::new();
        if !ignored.added.is_empty() {
            steps.push(format!(
                "{} {} to {}",
                if dry_run { "would add" } else { "added" },
                ignored.added.join(", "),
                ignored.gitignore
            ));
        }
        if !ignored.untracked.is_empty() {
            steps.push(format!(
                "{} {} from the index, keeping this device's copy",
                if dry_run { "would remove" } else { "removed" },
                ignored.untracked.join(", ")
            ));
        }
        if !steps.is_empty() {
            report.fixes.push(SyncDoctorFix {
                code: SYNC_DOCTOR_DEVICE_FILES.to_string(),
                dry_run,
                message: format!(
                    "{}; commit the change to stop sharing them (devices that pull it lose their copy, which Obsidian recreates)",
                    steps.join(" and ")
                ),
            });
        }
    }
    Ok(())
}

/// Inspects a Git-backed vault and its device-local recovery state without mutation.
#[must_use]
pub fn doctor_git_vault(paths: &VaultPaths, options: &GitSyncOptions) -> SyncDoctorReport {
    doctor_git_vault_for_platform(paths, options, GitPlatformProfile::native())
}

/// Inspects a Git-backed vault for an explicit registered target platform.
#[must_use]
pub fn doctor_git_vault_for_platform(
    paths: &VaultPaths,
    options: &GitSyncOptions,
    platform: GitPlatformProfile,
) -> SyncDoctorReport {
    let state_store = SyncStateStore::user_default().ok();
    doctor_git_vault_with_optional_state(
        paths,
        options,
        platform,
        state_store.as_ref(),
        SyncContentProfile::Knowledge,
        false,
    )
}

/// Inspects a repository using the selected content profile and optional
/// unattended files-only safety policy.
#[must_use]
pub fn doctor_git_vault_for_profile(
    paths: &VaultPaths,
    options: &GitSyncOptions,
    platform: GitPlatformProfile,
    profile: SyncContentProfile,
    unattended: bool,
) -> SyncDoctorReport {
    let state_store = SyncStateStore::user_default().ok();
    doctor_git_vault_with_optional_state(
        paths,
        options,
        platform,
        state_store.as_ref(),
        profile,
        unattended,
    )
}

/// State-store-aware form of [`doctor_git_vault_for_profile`].
#[must_use]
pub fn doctor_git_vault_with_profile_and_state_store(
    paths: &VaultPaths,
    options: &GitSyncOptions,
    platform: GitPlatformProfile,
    state_store: &SyncStateStore,
    profile: SyncContentProfile,
    unattended: bool,
) -> SyncDoctorReport {
    doctor_git_vault_with_optional_state(
        paths,
        options,
        platform,
        Some(state_store),
        profile,
        unattended,
    )
}

#[must_use]
pub fn doctor_git_vault_with_state_store(
    paths: &VaultPaths,
    options: &GitSyncOptions,
    state_store: &SyncStateStore,
) -> SyncDoctorReport {
    doctor_git_vault_with_optional_state(
        paths,
        options,
        GitPlatformProfile::native(),
        Some(state_store),
        SyncContentProfile::Knowledge,
        false,
    )
}

#[allow(clippy::too_many_lines)] // Keep the ordered diagnostic sequence in one report builder.
fn doctor_git_vault_with_optional_state(
    paths: &VaultPaths,
    options: &GitSyncOptions,
    platform: GitPlatformProfile,
    state_store: Option<&SyncStateStore>,
    profile: SyncContentProfile,
    unattended: bool,
) -> SyncDoctorReport {
    let base_engine = vulcan_sync::GitCliEngine::default();
    let engine = match state_store {
        Some(state) => crate::sync_transport::apply_transport_with_state(base_engine, paths, state),
        None => crate::sync_transport::apply_transport(base_engine, paths),
    }
    .with_command_timeout(options.command_timeout);
    let (effective_options, policy_severity, policy_detail) =
        configured_options_for_doctor(paths, options);
    let options = &effective_options;
    let mut report = initial_doctor_report(paths, options, platform);
    doctor_check(
        &mut report,
        "sync.merge-policy",
        policy_severity,
        policy_detail,
    );
    doctor_device_identity(state_store, &mut report);
    doctor_transport(paths, &options.remote, &mut report);
    doctor_file_synchronizer(paths, &mut report);

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

    if unattended && profile == SyncContentProfile::FilesOnly {
        doctor_unattended_files_only_state(&engine, &repository, &mut report);
    } else {
        match engine.safety_state(&repository) {
        Ok(safety) if safety.staged_changes => doctor_check(
            &mut report,
            "git.safety",
            SyncDoctorSeverity::Info,
            "the normal Git index has staged changes; sync captures worktree bytes and never touches the index",
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
    }

    match engine.repository_requirements(&repository, paths.vault_root()) {
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

    doctor_platform_tree(&engine, &repository, options, &mut report);
    doctor_device_files(&engine, &repository, paths, options, &mut report);
    doctor_repository_lock(&repository, &mut report);
    doctor_journal(paths, state_store, &mut report);
    doctor_apply_marker(state_store, &repository, &mut report);
    if profile == SyncContentProfile::Knowledge {
        doctor_cache(paths, &mut report);
    }
    finish_doctor_report(report)
}

fn configured_options_for_doctor(
    paths: &VaultPaths,
    options: &GitSyncOptions,
) -> (GitSyncOptions, SyncDoctorSeverity, String) {
    match configured_git_sync_options(paths, options) {
        Ok(options) => (
            options,
            SyncDoctorSeverity::Pass,
            "sync configuration is valid".to_string(),
        ),
        Err(error) => (
            options.clone(),
            SyncDoctorSeverity::Error,
            error.to_string(),
        ),
    }
}

fn doctor_unattended_files_only_state(
    engine: &dyn GitEngine,
    repository: &GitRepository,
    report: &mut SyncDoctorReport,
) {
    match engine.unattended_repository_state(repository) {
        Ok(state) => {
            let mut unsafe_conditions = Vec::new();
            if !state.attached_head {
                unsafe_conditions.push("HEAD is detached".to_string());
            }
            if state.staged_changes {
                unsafe_conditions.push("the normal Git index has staged changes".to_string());
            }
            if state.worktree_count != 1 {
                unsafe_conditions.push(format!(
                    "the repository has {} linked worktrees (exactly one is supported)",
                    state.worktree_count
                ));
            }
            if let Some(operation) = state.operation {
                unsafe_conditions.push(format!("a Git {operation} operation is in progress"));
            }
            if !state.nested_repositories.is_empty() {
                unsafe_conditions.push(format!(
                    "nested repositories or submodules are present: {}",
                    state.nested_repositories.join(", ")
                ));
            }
            if unsafe_conditions.is_empty() {
                doctor_check(
                    report,
                    "git.files-only-safety",
                    SyncDoctorSeverity::Pass,
                    "repository state permits unattended files-only synchronization; Vulcan's repository lock does not exclude external Git processes, so avoid branch switches or Git operations while a job runs",
                );
            } else {
                doctor_check(
                    report,
                    "git.files-only-safety",
                    SyncDoctorSeverity::Error,
                    format!(
                        "unattended files-only synchronization is paused because {}; resolve these conditions or use an explicit manual synchronization",
                        unsafe_conditions.join("; ")
                    ),
                );
            }
        }
        Err(error) => doctor_check(
            report,
            "git.files-only-safety",
            SyncDoctorSeverity::Error,
            format!("cannot verify unattended files-only repository safety: {error}"),
        ),
    }
}

fn initial_doctor_report(
    paths: &VaultPaths,
    options: &GitSyncOptions,
    platform: GitPlatformProfile,
) -> SyncDoctorReport {
    SyncDoctorReport {
        version: SYNC_DOCTOR_VERSION,
        healthy: true,
        vault: paths.vault_root().to_path_buf(),
        remote: options.remote.clone(),
        device_id: None,
        installation: None,
        repository: None,
        remote_revision: None,
        requirements: None,
        platform_policy: platform.policy(),
        platform_preflight: None,
        journal: None,
        apply_marker: None,
        checks: Vec::new(),
        fixes: Vec::new(),
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

/// Reports a device-key transport binding. Unbound vaults add no check, so
/// ambient SSH setups keep their existing doctor output.
fn doctor_transport(paths: &VaultPaths, remote: &GitRemote, report: &mut SyncDoctorReport) {
    use crate::sync_transport::{transport_status, GitConfigState, GitTransportState};
    let status = match transport_status(paths) {
        Ok(status) => status,
        Err(error) => {
            doctor_check(
                report,
                "sync.transport",
                SyncDoctorSeverity::Error,
                error.to_string(),
            );
            return;
        }
    };
    match status.state {
        GitTransportState::NotBound if status.git_config == GitConfigState::Orphaned => {
            doctor_check(
                report,
                "sync.transport",
                SyncDoctorSeverity::Warning,
                status.diagnostic.unwrap_or_default(),
            );
        }
        GitTransportState::NotBound => {
            // Not an error: the user may deliberately use ambient SSH. A hint is
            // given only where the device-key policy applies to this remote.
            let config = crate::device_config::DeviceConfigStore::user_default()
                .and_then(|store| store.load())
                .unwrap_or_default();
            let url = crate::sync_transport::remote_url(paths.vault_root(), remote.as_str());
            if let Some(hint) = url
                .ok()
                .and_then(|url| crate::vault_enroll::enrollment_hint(&url, &config))
            {
                doctor_check(report, "sync.transport", SyncDoctorSeverity::Info, hint);
            }
        }
        GitTransportState::DeviceKeyUnavailable => doctor_check(
            report,
            "sync.transport",
            SyncDoctorSeverity::Error,
            format!(
                "Git is bound to the device key but it cannot authenticate: {}; sync fails closed",
                status.diagnostic.unwrap_or_default()
            ),
        ),
        GitTransportState::Usable => {
            let drift = matches!(
                status.git_config,
                GitConfigState::Missing | GitConfigState::Foreign | GitConfigState::Stale
            );
            doctor_check(
                report,
                "sync.transport",
                if drift {
                    SyncDoctorSeverity::Warning
                } else {
                    SyncDoctorSeverity::Pass
                },
                if drift {
                    "Git is bound to the device key, but the repository's core.sshCommand drifted (missing, replaced, or naming a vulcan that no longer exists); rerun `vulcan sync transport bind`"
                } else {
                    "Git sync authenticates with the bound device key; remote authorization is not checked offline"
                },
            );
        }
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
            .map(|filter| {
                format!(
                    "{} ({} paths, {})",
                    filter.name,
                    filter.path_count,
                    if filter.ready() {
                        "ready"
                    } else {
                        "unavailable"
                    }
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        let severity = if requirements
            .required_filters
            .iter()
            .any(|filter| !filter.ready())
        {
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

/// Warns when the latest capture tracks per-device Obsidian files that no
/// shared `.gitignore` ignores: every pane change then becomes a sync commit
/// and one device's layout spreads to the others.
fn doctor_device_files(
    engine: &dyn GitEngine,
    repository: &GitRepository,
    paths: &VaultPaths,
    options: &GitSyncOptions,
    report: &mut SyncDoctorReport,
) {
    let revision = GitSyncRefs::for_options(options)
        .ok()
        .and_then(|refs| engine.read_ref(repository, &refs.local).ok().flatten())
        .or_else(|| engine.head_commit(repository).ok().flatten());
    let Some(revision) = revision else {
        return;
    };
    match crate::sync_device_files::shared_device_files(
        engine,
        repository,
        paths.vault_root(),
        &revision,
    ) {
        Ok(shared) if !shared.unignored.is_empty() => doctor_check(
            report,
            SYNC_DOCTOR_DEVICE_FILES,
            SyncDoctorSeverity::Warning,
            format!(
                "per-device Obsidian files are synced: {}; run `vulcan sync doctor --fix` so each device keeps its own copy",
                shared.unignored.join(", ")
            ),
        ),
        Ok(shared) if !shared.pending.is_empty() => doctor_check(
            report,
            SYNC_DOCTOR_DEVICE_FILES,
            SyncDoctorSeverity::Info,
            format!(
                "{DEVICE_FILES_PENDING} {} but the last commit still shares them; commit their removal from the index (`vulcan sync doctor --fix` stages it)",
                shared.pending.join(", ")
            ),
        ),
        Ok(_) => {}
        Err(error) => doctor_check(
            report,
            SYNC_DOCTOR_DEVICE_FILES,
            SyncDoctorSeverity::Info,
            format!("cannot inspect per-device files: {error}"),
        ),
    }
}

fn doctor_platform_tree(
    engine: &dyn GitEngine,
    repository: &GitRepository,
    options: &GitSyncOptions,
    report: &mut SyncDoctorReport,
) {
    let local = match GitSyncRefs::for_options(options) {
        Ok(refs) => engine.read_ref(repository, &refs.local),
        Err(error) => {
            doctor_check(
                report,
                "platform.tree",
                SyncDoctorSeverity::Error,
                format!("cannot derive the local sync ref: {error}"),
            );
            return;
        }
    };
    let revision = match local {
        Ok(Some(revision)) => Some(revision),
        Ok(None) => match engine.head_commit(repository) {
            Ok(revision) => revision,
            Err(error) => {
                doctor_check(
                    report,
                    "platform.tree",
                    SyncDoctorSeverity::Error,
                    format!("cannot select a tree for platform preflight: {error}"),
                );
                return;
            }
        },
        Err(error) => {
            doctor_check(
                report,
                "platform.tree",
                SyncDoctorSeverity::Error,
                format!("cannot inspect the local sync candidate: {error}"),
            );
            return;
        }
    };
    let Some(revision) = revision else {
        doctor_check(
            report,
            "platform.tree",
            SyncDoctorSeverity::Info,
            "the unborn repository has no immutable tree to preflight yet",
        );
        return;
    };
    let entries = match engine.tree_entries(repository, &revision) {
        Ok(entries) => entries,
        Err(error) => {
            doctor_check(
                report,
                "platform.tree",
                SyncDoctorSeverity::Error,
                format!("cannot inspect the selected Git tree: {error}"),
            );
            return;
        }
    };
    let preflight =
        vulcan_sync::inspect_git_tree_platform(revision, &entries, report.platform_policy.clone());
    for diagnostic in &preflight.diagnostics {
        let severity = match diagnostic.severity {
            vulcan_sync::GitPlatformDiagnosticSeverity::Pass => SyncDoctorSeverity::Pass,
            vulcan_sync::GitPlatformDiagnosticSeverity::Info => SyncDoctorSeverity::Info,
            vulcan_sync::GitPlatformDiagnosticSeverity::Warning => SyncDoctorSeverity::Warning,
            vulcan_sync::GitPlatformDiagnosticSeverity::Error => SyncDoctorSeverity::Error,
        };
        let examples = if diagnostic.paths.is_empty() {
            String::new()
        } else {
            format!("; examples: {}", diagnostic.paths.join(", "))
        };
        doctor_check(
            report,
            &diagnostic.code,
            severity,
            format!(
                "{} ({} path(s)){examples}",
                diagnostic.message, diagnostic.count
            ),
        );
    }
    report.platform_preflight = Some(preflight);
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
        Err(error) if error.kind() == fs2::lock_contended_error().kind() => doctor_check(
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
    let work_tree = match crate::sync_state::sync_work_tree(paths.vault_root()) {
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

/// The nearest directory at or above `path` that Syncthing replicates,
/// recognized by the `.stfolder` marker at its root.
#[must_use]
pub fn syncthing_folder_root(path: &Path) -> Option<PathBuf> {
    let start = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    start
        .ancestors()
        .find(|directory| directory.join(".stfolder").exists())
        .map(Path::to_path_buf)
}

/// Explains why Git sync and Syncthing on the same tree conflict.
#[must_use]
pub fn syncthing_overlap_warning(folder: &Path) -> String {
    format!(
        "`{}` is a Syncthing folder (.stfolder): Syncthing replicates this vault's files and its .git directory while Git sync also moves them, so the two race and Syncthing leaves *.sync-conflict-* copies. Use one: exclude the vault from Syncthing (or ignore .git there and stop Git sync on the other devices), or register the vault with --no-sync",
        folder.display()
    )
}

/// How deep `#include` lines in `.stignore` are followed.
const STIGNORE_INCLUDE_DEPTH: usize = 4;

/// Whether the Syncthing folder at `folder` ignores the Git directory of the
/// repository holding `vault`, judged from its `.stignore` and the files it
/// `#include`s. Only plain patterns are recognized (`.git`, `**/.git`, and
/// the directory's path anchored at the folder root); anything else counts
/// as not ignored, so the overlap warning stays.
#[must_use]
pub fn syncthing_ignores_git(folder: &Path, vault: &Path) -> bool {
    let folder = folder
        .canonicalize()
        .unwrap_or_else(|_| folder.to_path_buf());
    let vault = vault.canonicalize().unwrap_or_else(|_| vault.to_path_buf());
    let Some(git_dir) = vault
        .ancestors()
        .take_while(|directory| directory.starts_with(&folder))
        .map(|directory| directory.join(".git"))
        .find(|candidate| candidate.exists())
    else {
        return false;
    };
    let Ok(relative) = git_dir.strip_prefix(&folder) else {
        return false;
    };
    let anchored = format!("/{}", relative.to_string_lossy().replace('\\', "/"));
    let mut patterns = Vec::new();
    collect_stignore_patterns(&folder, &folder.join(".stignore"), 0, &mut patterns);
    patterns.iter().any(|pattern| {
        let pattern = pattern.trim_end_matches('/');
        pattern == ".git" || pattern == "**/.git" || pattern == anchored
    })
}

fn collect_stignore_patterns(folder: &Path, file: &Path, depth: usize, out: &mut Vec<String>) {
    let Ok(contents) = std::fs::read_to_string(file) else {
        return;
    };
    for line in contents.lines() {
        let line = line.trim();
        if let Some(include) = line.strip_prefix("#include ") {
            // Canonical, so `..` and symlinks cannot leave the folder.
            let target = folder.join(include.trim()).canonicalize().ok();
            if let Some(target) = target.filter(|target| target.starts_with(folder)) {
                if depth < STIGNORE_INCLUDE_DEPTH {
                    collect_stignore_patterns(folder, &target, depth + 1, out);
                }
            }
            continue;
        }
        if line.is_empty() || line.starts_with("//") || line.starts_with('!') {
            continue;
        }
        let mut pattern = line;
        while let Some(rest) = pattern
            .strip_prefix("(?d)")
            .or_else(|| pattern.strip_prefix("(?i)"))
        {
            pattern = rest;
        }
        out.push(pattern.to_owned());
    }
}

/// The note for a Syncthing folder that keeps the Git directory to itself:
/// the working files are replicated, so only one device may run Git sync.
#[must_use]
pub fn syncthing_git_ignored_note(folder: &Path) -> String {
    format!(
        "`{}` is a Syncthing folder that ignores .git: Syncthing replicates the working files and Git sync carries the history, which is safe while only this device runs Git sync for it; register it with --no-sync on the other devices",
        folder.display()
    )
}

fn doctor_file_synchronizer(paths: &VaultPaths, report: &mut SyncDoctorReport) {
    if let Some(folder) = syncthing_folder_root(paths.vault_root()) {
        if syncthing_ignores_git(&folder, paths.vault_root()) {
            doctor_check(
                report,
                "sync.file-synchronizer",
                SyncDoctorSeverity::Info,
                syncthing_git_ignored_note(&folder),
            );
            return;
        }
        doctor_check(
            report,
            "sync.file-synchronizer",
            SyncDoctorSeverity::Warning,
            syncthing_overlap_warning(&folder),
        );
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
    sync_git_vault_with_profile(paths, options, SyncContentProfile::Knowledge)
}

pub fn sync_git_vault_with_profile(
    paths: &VaultPaths,
    options: &GitSyncOptions,
    profile: SyncContentProfile,
) -> Result<VaultSyncReport, AppError> {
    sync_git_vault_with_profile_and_state_store(
        paths,
        options,
        profile,
        &SyncStateStore::user_default()?,
    )
}

/// Runs one profile-aware direct cycle against an explicit state store.
pub fn sync_git_vault_with_profile_and_state_store(
    paths: &VaultPaths,
    options: &GitSyncOptions,
    profile: SyncContentProfile,
    state_store: &SyncStateStore,
) -> Result<VaultSyncReport, AppError> {
    let mut observer = vulcan_sync::IgnoreGitSyncProgress;
    sync_git_vault_with_profile_and_observer_and_engine(
        &crate::sync_transport::apply_transport_with_state(
            vulcan_sync::GitCliEngine::default(),
            paths,
            state_store,
        )
        .with_command_timeout(options.command_timeout),
        paths,
        options,
        state_store,
        &SyncCancellationToken::default(),
        &mut observer,
        profile,
    )
}

/// Runs one direct finite cycle while forwarding durable progress to a caller.
pub fn sync_git_vault_with_progress(
    paths: &VaultPaths,
    options: &GitSyncOptions,
    observer: &mut dyn GitSyncObserver,
) -> Result<VaultSyncReport, AppError> {
    sync_git_vault_with_profile_and_progress(
        paths,
        options,
        observer,
        SyncContentProfile::Knowledge,
    )
}

pub fn sync_git_vault_with_profile_and_progress(
    paths: &VaultPaths,
    options: &GitSyncOptions,
    observer: &mut dyn GitSyncObserver,
    profile: SyncContentProfile,
) -> Result<VaultSyncReport, AppError> {
    let state_store = SyncStateStore::user_default()?;
    let engine = crate::sync_transport::apply_transport_with_state(
        vulcan_sync::GitCliEngine::default(),
        paths,
        &state_store,
    )
    .with_command_timeout(options.command_timeout);
    sync_git_vault_with_profile_and_observer_and_engine(
        &engine,
        paths,
        options,
        &state_store,
        &SyncCancellationToken::default(),
        observer,
        profile,
    )
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
    let engine = crate::sync_transport::apply_transport_with_state(
        vulcan_sync::GitCliEngine::default(),
        paths,
        state_store,
    )
    .with_command_timeout(options.command_timeout);
    sync_git_vault_with_observer_and_engine(
        &engine,
        paths,
        options,
        state_store,
        cancellation,
        delegate,
    )
}

/// Runs one finite Git synchronization cycle with a caller-owned engine.
///
/// Long-running callers can reuse a cloned engine across cycles so immutable
/// installation metadata remains cached while each clone keeps its own timeout.
pub fn sync_git_vault_with_observer_and_engine(
    engine: &dyn GitEngine,
    paths: &VaultPaths,
    options: &GitSyncOptions,
    state_store: &SyncStateStore,
    cancellation: &SyncCancellationToken,
    delegate: &mut dyn GitSyncObserver,
) -> Result<VaultSyncReport, AppError> {
    sync_git_vault_with_profile_and_observer_and_engine(
        engine,
        paths,
        options,
        state_store,
        cancellation,
        delegate,
        SyncContentProfile::Knowledge,
    )
}

/// Runs one finite Git synchronization cycle with an explicit managed
/// directory profile. Files-only mode skips Markdown tree validation and
/// cache refresh while retaining the same file reconciliation engine.
#[allow(clippy::too_many_lines)] // Keep journal, backend, conflict, and cache stages visibly ordered.
pub fn sync_git_vault_with_profile_and_observer_and_engine(
    engine: &dyn GitEngine,
    paths: &VaultPaths,
    options: &GitSyncOptions,
    state_store: &SyncStateStore,
    cancellation: &SyncCancellationToken,
    delegate: &mut dyn GitSyncObserver,
    profile: SyncContentProfile,
) -> Result<VaultSyncReport, AppError> {
    sync_git_vault_with_profile_and_observer_and_engine_policy(
        engine,
        paths,
        options,
        state_store,
        cancellation,
        delegate,
        profile,
        false,
    )
}

/// Runs a profile-aware synchronization cycle with the extra repository
/// preflight required for unattended files-only operation.
#[allow(clippy::too_many_arguments)] // Finite sync composes independent repository, state, observer, and policy inputs.
pub fn sync_git_vault_with_profile_and_observer_and_engine_policy(
    engine: &dyn GitEngine,
    paths: &VaultPaths,
    options: &GitSyncOptions,
    state_store: &SyncStateStore,
    cancellation: &SyncCancellationToken,
    delegate: &mut dyn GitSyncObserver,
    profile: SyncContentProfile,
    unattended: bool,
) -> Result<VaultSyncReport, AppError> {
    sync_git_vault_cycle(
        engine,
        paths,
        options,
        state_store,
        cancellation,
        delegate,
        profile,
        unattended,
    )
}

/// One finite synchronization transaction.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn sync_git_vault_cycle(
    engine: &dyn GitEngine,
    paths: &VaultPaths,
    options: &GitSyncOptions,
    state_store: &SyncStateStore,
    cancellation: &SyncCancellationToken,
    delegate: &mut dyn GitSyncObserver,
    profile: SyncContentProfile,
    unattended: bool,
) -> Result<VaultSyncReport, AppError> {
    let started = Instant::now();
    let subprocesses_before = engine.subprocess_count();
    check_sync_start(cancellation)?;
    // Sync replicates the whole enclosing work tree, so durable state is keyed
    // on the discovered repository root while vault configuration and the
    // vault write lock come from the vault itself, which may be nested below
    // that root (for example an MkDocs `docs/` vault).
    let work_tree = discovered_work_tree(engine, paths);
    let vault_paths = sync_vault_paths(paths, &work_tree);
    let paths = &vault_paths;
    let options = configured_git_sync_options(paths, options)?;
    let branch_guard = if unattended && profile == SyncContentProfile::FilesOnly {
        validate_unattended_files_only_repository(engine, paths)?
    } else {
        None
    };
    let mut journal = SyncJournal::preparing(
        &work_tree,
        options.remote.to_string(),
        vulcan_sync::DEFAULT_REMOTE_LIVE_REF.to_string(),
    )?;
    let journal_path = state_store.journal_path(&journal.repository_key)?;
    let previous = state_store.load(&journal.repository_key)?;
    let recovered_from = previous
        .as_ref()
        .filter(|journal| journal.phase.requires_recovery())
        .cloned();
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
        engine,
        branch_guard,
    };
    let backend_started = Instant::now();
    let sync_result = run_sync_backend_with_vault_lock(
        engine,
        paths,
        &effective_options,
        cancellation,
        &mut observer,
    )?;
    let sync = match sync_result {
        Ok(sync) => sync,
        Err(error) => {
            let classified = vulcan_sync::classify_git_sync_error(&error);
            if !options.dry_run {
                journal.error = Some(error.to_string());
                if let Err(state_error) = state_store.save(&journal) {
                    return Err(AppError::operation(format!(
                        "{error}; additionally failed to retain the recovery journal: {state_error}"
                    )));
                }
            }
            return Err(AppError::sync(classified));
        }
    };
    let backend_cycle = backend_started.elapsed();
    let registration = if options.dry_run {
        None
    } else {
        crate::sync_registration::self_register_after_sync(
            engine,
            &sync.repository,
            &options.remote,
            state_store.identity(),
            sync.registration_tips.as_deref(),
        )
    };
    journal.git_dir = Some(sync.repository.git_dir.clone());
    journal.local_snapshot = sync.local_snapshot.as_ref().map(ToString::to_string);
    journal.phase = if sync.outcome == GitSyncOutcome::Paused {
        SyncJournalPhase::Paused
    } else {
        SyncJournalPhase::Verifying
    };
    if !options.dry_run {
        state_store.save(&journal)?;
    }
    let (cache_refresh, cache_refresh_error, cache_refresh_duration) =
        if profile == SyncContentProfile::Knowledge {
            refresh_cache_after_sync_with_timing(paths, &sync, &options)
        } else {
            (None, None, Duration::ZERO)
        };
    let (repository_key, retained) = retain_sync_journal(
        state_store,
        options.dry_run,
        sync.outcome,
        journal,
        previous,
    )?;
    let timings = SyncOperationTimings::new(
        subprocesses_before,
        started,
        [backend_cycle, cache_refresh_duration],
    );
    let operational_stats = sync_operational_stats(engine, &timings);
    let device_files = if profile == SyncContentProfile::Knowledge {
        unignored_device_files(engine, paths, &sync)
    } else {
        Vec::new()
    };
    Ok(VaultSyncReport {
        sync,
        cache_refresh,
        cache_refresh_error,
        registration,
        operational_stats,
        state: VaultSyncStateReport {
            repository_key,
            journal_path,
            recovered_from,
            retained,
        },
        device_files,
    })
}

/// The per-device files this sync's capture still shares. Best effort: the
/// answer only drives a hint, so an inspection error means no hint, and a
/// vault without a `.obsidian` directory costs nothing.
fn unignored_device_files(
    engine: &dyn GitEngine,
    paths: &VaultPaths,
    sync: &vulcan_sync::GitSyncReport,
) -> Vec<String> {
    if !paths.vault_root().join(".obsidian").is_dir() {
        return Vec::new();
    }
    // A preview captures nothing, so it reads the last snapshot; files the
    // work tree already ignores leave with the next capture either way.
    let Some(revision) = sync.local_snapshot.as_ref().or(sync.local_before.as_ref()) else {
        return Vec::new();
    };
    crate::sync_device_files::shared_device_files(
        engine,
        &sync.repository,
        paths.vault_root(),
        revision,
    )
    .map_or_default(|shared| shared.unignored)
}

fn run_sync_backend_with_vault_lock(
    engine: &dyn GitEngine,
    paths: &VaultPaths,
    options: &GitSyncOptions,
    cancellation: &SyncCancellationToken,
    observer: &mut dyn GitSyncObserver,
) -> Result<Result<vulcan_sync::GitSyncReport, vulcan_sync::GitSyncError>, AppError> {
    // The backend acquires the repository lock inside this guard, preserving
    // vault-before-repository ordering. Uninitialized plain Git vaults have no
    // application lock yet and remain supported.
    let vault_lock = (!options.dry_run && paths.vulcan_dir().is_dir())
        .then(|| vulcan_core::write_lock::acquire_write_lock(paths))
        .transpose()
        .map_err(AppError::operation)?;
    let result = vulcan_sync::sync_git_once_with_control(
        engine,
        paths.vault_root(),
        options,
        cancellation,
        observer,
    );
    drop(vault_lock);
    Ok(result)
}

fn validate_unattended_files_only_repository(
    engine: &dyn GitEngine,
    paths: &VaultPaths,
) -> Result<Option<GitRefName>, AppError> {
    let repository = engine
        .discover_repository(paths.vault_root())
        .map_err(|error| {
            unattended_files_only_error(
                format!(
            "cannot verify repository safety for unattended files-only synchronization: {error}"
        ),
                true,
            )
        })?;
    let state = engine
        .unattended_repository_state(&repository)
        .map_err(|error| {
            unattended_files_only_error(
                format!(
            "cannot verify repository safety for unattended files-only synchronization: {error}"
        ),
                true,
            )
        })?;
    let mut unsafe_conditions = Vec::new();
    if !state.attached_head {
        unsafe_conditions.push("HEAD is detached".to_string());
    }
    if state.staged_changes {
        unsafe_conditions.push("the normal Git index has staged changes".to_string());
    }
    if state.worktree_count != 1 {
        unsafe_conditions.push(format!(
            "the repository has {} linked worktrees (exactly one is supported)",
            state.worktree_count
        ));
    }
    if let Some(operation) = state.operation {
        unsafe_conditions.push(format!("a Git {operation} operation is in progress"));
    }
    if !state.nested_repositories.is_empty() {
        unsafe_conditions.push(format!(
            "nested repositories or submodules are present: {}",
            state.nested_repositories.join(", ")
        ));
    }
    if unsafe_conditions.is_empty() {
        Ok(state.head_reference)
    } else {
        Err(unattended_files_only_error(format!(
            "unattended files-only synchronization is paused because {}; resolve the repository state or run a manual synchronization",
            unsafe_conditions.join("; ")
        ), false))
    }
}

fn unattended_files_only_error(message: String, retryable: bool) -> AppError {
    AppError::sync(vulcan_sync::SyncError::new(
        vulcan_sync::SyncErrorCategory::Repository,
        message,
        retryable,
    ))
}

fn refresh_cache_after_sync_with_timing(
    paths: &VaultPaths,
    sync: &GitSyncReport,
    options: &GitSyncOptions,
) -> (Option<ScanSummary>, Option<String>, Duration) {
    let required = !options.dry_run
        && sync.actions.contains(&GitSyncAction::WorktreeApplied)
        && paths.cache_db().is_file();
    let started = Instant::now();
    let (report, error) = refresh_cache_after_sync(paths, sync, options);
    let duration = if required {
        started.elapsed()
    } else {
        Duration::ZERO
    };
    (report, error, duration)
}

fn retain_sync_journal(
    state_store: &SyncStateStore,
    dry_run: bool,
    outcome: GitSyncOutcome,
    journal: SyncJournal,
    previous: Option<SyncJournal>,
) -> Result<(String, Option<SyncJournal>), AppError> {
    let repository_key = journal.repository_key.clone();
    let retained = if dry_run {
        previous
    } else if outcome == GitSyncOutcome::Paused {
        Some(journal)
    } else {
        state_store.clear(&repository_key)?;
        None
    };
    Ok((repository_key, retained))
}

struct SyncOperationTimings {
    subprocesses_before: Option<u64>,
    elapsed: Duration,
    backend_cycle: Duration,
    cache_refresh: Duration,
}

impl SyncOperationTimings {
    fn new(subprocesses_before: Option<u64>, started: Instant, stages: [Duration; 2]) -> Self {
        Self {
            subprocesses_before,
            elapsed: started.elapsed(),
            backend_cycle: stages[0],
            cache_refresh: stages[1],
        }
    }
}

fn sync_operational_stats(
    engine: &dyn GitEngine,
    timings: &SyncOperationTimings,
) -> SyncOperationalStats {
    SyncOperationalStats {
        version: 2,
        git_subprocesses: timings
            .subprocesses_before
            .zip(engine.subprocess_count())
            .map(|(before, after)| after.saturating_sub(before)),
        elapsed_ms: duration_millis(timings.elapsed),
        backend_cycle_ms: duration_millis(timings.backend_cycle),
        cache_refresh_ms: duration_millis(timings.cache_refresh),
    }
}

fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn refresh_cache_after_sync(
    paths: &VaultPaths,
    sync: &GitSyncReport,
    options: &GitSyncOptions,
) -> (Option<ScanSummary>, Option<String>) {
    // Sync changes files only by moving the branch.
    let files_changed = sync.outcome == GitSyncOutcome::Pulled;
    let should_refresh = !options.dry_run && files_changed && paths.cache_db().is_file();
    // The vault tree is already applied and verified; a cache refresh
    // failure must not report the successful sync as failed or retain a
    // misleading recovery journal. The rebuildable cache stays stale until
    // the next refresh and the warning rides the report.
    match should_refresh
        .then(|| refresh_cache_incrementally(paths))
        .transpose()
    {
        Ok(report) => (report, None),
        Err(error) => (None, Some(error.to_string())),
    }
}

fn check_sync_start(cancellation: &SyncCancellationToken) -> Result<(), AppError> {
    if cancellation.is_cancelled() {
        Err(AppError::operation(
            "synchronization was cancelled before the transaction started",
        ))
    } else {
        Ok(())
    }
}

/// Resolves the discovered repository work-tree root, or the invocation path
/// when discovery cannot complete so the retained error journal still names it.
fn discovered_work_tree(engine: &dyn GitEngine, paths: &VaultPaths) -> PathBuf {
    engine
        .discover_repository(paths.vault_root())
        .ok()
        .and_then(|repository| repository.work_tree)
        .unwrap_or_else(|| paths.vault_root().to_path_buf())
}

/// Chooses the vault whose configuration and write lock govern a sync
/// started at `paths`: the invocation directory when it is an initialized
/// vault, otherwise the nearest initialized vault between it and the work-tree
/// root, otherwise the work-tree root itself.
fn sync_vault_paths(paths: &VaultPaths, work_tree: &Path) -> VaultPaths {
    if paths.vulcan_dir().is_dir() {
        return paths.clone();
    }
    let start = fs::canonicalize(paths.vault_root()).unwrap_or_else(|_| paths.vault_root().into());
    start
        .ancestors()
        .take_while(|directory| directory.starts_with(work_tree))
        .find(|directory| directory.join(vulcan_core::paths::VULCAN_DIR_NAME).is_dir())
        .map_or_else(|| VaultPaths::new(work_tree), VaultPaths::new)
}

/// Applies the shared vault merge policy and device-local automation ceiling.
///
/// Caller-supplied automation is also a ceiling, so configuration can never
/// increase automation selected by an embedding application.
pub fn configured_git_sync_options(
    paths: &VaultPaths,
    options: &GitSyncOptions,
) -> Result<GitSyncOptions, AppError> {
    let config = load_sync_config(paths)?;
    let mut effective = options.clone();
    effective.branch_push = options.branch_push && config.sync.branch_push;
    effective.publish_device_backup =
        options.publish_device_backup && config.sync.publish_device_backup;
    if !config.sync.branch_pull.is_git() {
        effective.branch_pull = config.sync.branch_pull;
    }
    Ok(effective)
}

fn load_sync_config(paths: &VaultPaths) -> Result<VaultConfig, AppError> {
    let loaded = load_vault_config(paths);
    if let Some(diagnostic) = loaded
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.kind == vulcan_core::ConfigDiagnosticKind::ParseFailure)
    {
        return Err(AppError::operation(format!(
            "cannot synchronize with malformed configuration at {}: {}",
            diagnostic.path.display(),
            diagnostic.message
        )));
    }
    Ok(loaded.config)
}

struct JournalSyncObserver<'a> {
    state_store: &'a SyncStateStore,
    journal: &'a mut SyncJournal,
    persist: bool,
    delegate: &'a mut dyn GitSyncObserver,
    engine: &'a dyn GitEngine,
    branch_guard: Option<GitRefName>,
}

impl GitSyncObserver for JournalSyncObserver<'_> {
    fn progress(&mut self, progress: &GitSyncProgress) -> Result<(), GitSyncObserverError> {
        if progress.phase == GitSyncPhase::Applying {
            if let Some(expected) = &self.branch_guard {
                let actual = self
                    .engine
                    .head_reference(&progress.repository)
                    .map_err(|error| GitSyncObserverError::new(error.to_string()))?;
                if actual.as_ref() != Some(expected) {
                    return Err(GitSyncObserverError::new(format!(
                        "repository branch changed during unattended files-only synchronization (expected `{expected}`, found `{actual:?}`); synchronized files were not applied, retry after Git activity stops"
                    )));
                }
            }
        }
        self.journal.phase = match progress.phase {
            GitSyncPhase::Preparing => SyncJournalPhase::Preparing,
            GitSyncPhase::Capturing => SyncJournalPhase::Capturing,
            GitSyncPhase::Captured => SyncJournalPhase::Captured,
            GitSyncPhase::BackingUp => SyncJournalPhase::BackingUp,
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

    #[test]
    fn doctor_warns_when_syncthing_also_replicates_the_vault() {
        let temporary = tempfile::tempdir().unwrap();
        let vault = temporary.path().join("Sync/notes");
        std::fs::create_dir_all(&vault).unwrap();
        let paths = VaultPaths::new(&vault);
        let store = SyncStateStore::at(temporary.path().join("state"));
        let doctor = || {
            doctor_git_vault_with_optional_state(
                &paths,
                &GitSyncOptions::default(),
                GitPlatformProfile::native(),
                Some(&store),
                SyncContentProfile::Knowledge,
                false,
            )
        };
        assert!(!doctor()
            .checks
            .iter()
            .any(|check| check.code == "sync.file-synchronizer"));

        std::fs::create_dir(temporary.path().join("Sync/.stfolder")).unwrap();
        assert_eq!(
            syncthing_folder_root(&vault),
            Some(temporary.path().join("Sync").canonicalize().unwrap())
        );
        assert!(doctor().checks.iter().any(|check| {
            check.code == "sync.file-synchronizer"
                && check.severity == SyncDoctorSeverity::Warning
                && check.message.contains(".stfolder")
        }));
    }

    #[test]
    fn a_syncthing_folder_that_ignores_git_is_only_noted() {
        let temporary = tempdir().expect("temporary directory");
        let folder = temporary.path().join("Comics");
        fs::create_dir_all(folder.join(".stfolder")).unwrap();
        fs::create_dir_all(folder.join(".git")).unwrap();
        assert!(!syncthing_ignores_git(&folder, &folder), "no .stignore");

        // Through an include, with comments, flags and a negation around it.
        fs::write(
            folder.join(".stignore"),
            "#include .stglobalignore\n// local\n!/keep\n",
        )
        .unwrap();
        fs::write(folder.join(".stglobalignore"), "// set: vcs\n(?d).git\n").unwrap();
        assert!(syncthing_ignores_git(&folder, &folder));
        // A vault in a subdirectory shares the repository's .git.
        fs::create_dir_all(folder.join("docs")).unwrap();
        assert!(syncthing_ignores_git(&folder, &folder.join("docs")));

        // Anchored patterns must name the directory itself.
        fs::write(folder.join(".stglobalignore"), "/other/.git\n").unwrap();
        assert!(!syncthing_ignores_git(&folder, &folder));
        fs::write(folder.join(".stglobalignore"), "/.git/\n").unwrap();
        assert!(syncthing_ignores_git(&folder, &folder));
        // A negated pattern does not ignore it.
        fs::write(folder.join(".stglobalignore"), "!.git\n").unwrap();
        assert!(!syncthing_ignores_git(&folder, &folder));
        // Includes cannot escape the folder.
        fs::write(temporary.path().join("outside"), ".git\n").unwrap();
        fs::write(folder.join(".stignore"), "#include ../outside\n").unwrap();
        assert!(!syncthing_ignores_git(&folder, &folder));
    }
    use std::fs;
    use std::io::Write;
    use std::path::Path;
    use std::process::{Command, Stdio};
    use tempfile::tempdir;
    use vulcan_core::{initialize_vulcan_dir, properties::load_note_index, scan_vault, ScanMode};

    struct StructuredSyncFixture {
        _temporary: tempfile::TempDir,
        writer: std::path::PathBuf,
        reader: std::path::PathBuf,
        store: SyncStateStore,
    }

    const RECOVERABLE_JOURNAL_PHASES: [SyncJournalPhase; 11] = [
        SyncJournalPhase::Preparing,
        SyncJournalPhase::Capturing,
        SyncJournalPhase::Captured,
        SyncJournalPhase::BackingUp,
        SyncJournalPhase::Fetching,
        SyncJournalPhase::Fetched,
        SyncJournalPhase::Merging,
        SyncJournalPhase::Pushing,
        SyncJournalPhase::Applying,
        SyncJournalPhase::Verifying,
        SyncJournalPhase::Error,
    ];

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

    fn git_stdout_with_stdin(path: &Path, arguments: &[&str], input: &[u8]) -> String {
        let mut child = Command::new("git")
            .current_dir(path)
            .args(arguments)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("Git should launch");
        child
            .stdin
            .take()
            .expect("Git stdin")
            .write_all(input)
            .expect("write Git stdin");
        let output = child.wait_with_output().expect("Git should finish");
        assert!(output.status.success(), "Git failed: {arguments:?}");
        String::from_utf8(output.stdout)
            .expect("Git output should be UTF-8")
            .trim()
            .to_string()
    }

    #[test]
    fn unattended_files_only_preflight_accepts_simple_checkout_and_rejects_staging() {
        let temporary = tempdir().expect("temporary directory");
        let repository_path = temporary.path().join("vault");
        fs::create_dir(&repository_path).expect("repository directory");
        git(
            &repository_path,
            &["-c", "init.defaultBranch=main", "init", "--quiet"],
        );
        git(&repository_path, &["config", "user.name", "Vulcan Test"]);
        git(
            &repository_path,
            &["config", "user.email", "vulcan@example.invalid"],
        );
        fs::write(repository_path.join("file.bin"), b"bytes").expect("file");
        git(&repository_path, &["add", "file.bin"]);
        git(&repository_path, &["commit", "--quiet", "-m", "initial"]);
        let engine = vulcan_sync::GitCliEngine::default();
        let paths = VaultPaths::new(&repository_path);

        validate_unattended_files_only_repository(&engine, &paths)
            .expect("plain attached checkout should be accepted");

        fs::write(repository_path.join("file.bin"), b"staged").expect("edit");
        git(&repository_path, &["add", "file.bin"]);
        let error = validate_unattended_files_only_repository(&engine, &paths)
            .expect_err("staged work must pause unattended sync");
        assert_eq!(
            error.sync_error().map(|error| error.category),
            Some(vulcan_sync::SyncErrorCategory::Repository)
        );
        assert!(error.to_string().contains("staged changes"));

        git(&repository_path, &["reset", "--quiet"]);
        let nested = repository_path.join("nested");
        fs::create_dir(&nested).expect("nested repository directory");
        git(&nested, &["init", "--quiet"]);
        let error = validate_unattended_files_only_repository(&engine, &paths)
            .expect_err("nested repository must pause unattended sync");
        assert!(error.to_string().contains("nested/.git"));
    }

    fn assert_dry_run_recovers_journal_phase(
        paths: &VaultPaths,
        store: &SyncStateStore,
        writer: &Path,
        phase: SyncJournalPhase,
    ) {
        let mut interrupted =
            SyncJournal::preparing(writer, "origin", "refs/heads/__vulcan-sync/live")
                .expect("journal");
        interrupted.phase = phase;
        store.save(&interrupted).expect("interrupted journal");

        let planned = sync_git_vault_with_state_store(
            paths,
            &GitSyncOptions {
                dry_run: true,
                ..GitSyncOptions::default()
            },
            store,
        )
        .expect("recovery plan");
        assert_eq!(
            planned
                .state
                .recovered_from
                .as_ref()
                .map(|journal| (journal.transaction_id, journal.phase)),
            Some((interrupted.transaction_id, phase))
        );
        assert_eq!(
            store
                .load(&interrupted.repository_key)
                .expect("load unchanged journal"),
            Some(interrupted)
        );
    }

    fn structured_sync_fixture(files: &[(&str, &str)]) -> StructuredSyncFixture {
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
        // Windows runners default to autocrlf; the assertions compare LF bytes.
        git(&writer, &["config", "core.autocrlf", "false"]);
        git(
            &writer,
            &[
                "remote",
                "add",
                "origin",
                remote.to_str().expect("remote path"),
            ],
        );
        for (path, contents) in files {
            let target = writer.join(path);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).expect("fixture parent");
            }
            fs::write(target, contents).expect("fixture file");
        }
        git(&writer, &["add", "--all", "--", "."]);
        git(&writer, &["commit", "--quiet", "-m", "base"]);
        git(&writer, &["push", "--quiet", "-u", "origin", "main"]);
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
                "-c",
                "core.autocrlf=false",
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
        git(&reader, &["config", "core.autocrlf", "false"]);
        sync_git_vault_with_state_store(
            &VaultPaths::new(&reader),
            &GitSyncOptions::default(),
            &store,
        )
        .expect("reader baseline");
        StructuredSyncFixture {
            _temporary: temporary,
            writer,
            reader,
            store,
        }
    }

    #[test]
    fn direct_sync_waits_for_the_shared_vault_write_lock() {
        let fixture = structured_sync_fixture(&[("Home.md", "base\n")]);
        let paths = VaultPaths::new(&fixture.writer);
        initialize_vulcan_dir(&paths).expect("initialize vault coordination directory");
        let held = vulcan_core::write_lock::acquire_write_lock(&paths).expect("vault lock");
        let store = fixture.store.clone();
        let worker_paths = paths.clone();
        let worker = std::thread::spawn(move || {
            sync_git_vault_with_state_store(&worker_paths, &GitSyncOptions::default(), &store)
        });
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            !worker.is_finished(),
            "sync must wait for a direct vault writer"
        );
        drop(held);
        worker.join().expect("sync thread").expect("sync result");
    }

    #[test]
    fn nested_vault_sync_uses_vault_lock_and_cache_with_repository_state() {
        let fixture = structured_sync_fixture(&[
            ("docs/index.md", "# Home\n"),
            ("src/main.rs", "fn main() {}\n"),
        ]);
        let expected_key = crate::sync_state::repository_state_key(
            &fs::canonicalize(&fixture.reader).expect("canonical reader"),
        );
        let reader_docs = VaultPaths::new(fixture.reader.join("docs"));
        initialize_vulcan_dir(&reader_docs).expect("initialize nested vault");
        vulcan_core::scan_vault(&reader_docs, vulcan_core::ScanMode::Full)
            .expect("initial nested vault scan");

        fs::write(fixture.writer.join("docs/index.md"), "# Home\n\nRemote.\n")
            .expect("remote note edit");
        fs::write(
            fixture.writer.join("src/main.rs"),
            "fn main() { todo!() }\n",
        )
        .expect("remote code edit");
        git(
            &fixture.writer,
            &["commit", "--quiet", "-am", "remote edits"],
        );
        sync_git_vault_with_state_store(
            &VaultPaths::new(&fixture.writer),
            &GitSyncOptions::default(),
            &fixture.store,
        )
        .expect("writer publish");

        // The nested vault's own write lock serializes sync with vault writers.
        let held = vulcan_core::write_lock::acquire_write_lock(&reader_docs).expect("vault lock");
        let store = fixture.store.clone();
        let worker_paths = reader_docs.clone();
        let worker = std::thread::spawn(move || {
            sync_git_vault_with_state_store(&worker_paths, &GitSyncOptions::default(), &store)
        });
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            !worker.is_finished(),
            "nested vault sync must wait for the vault write lock"
        );
        drop(held);
        let report = worker.join().expect("sync thread").expect("nested sync");

        assert_eq!(report.state.repository_key, expected_key);
        // Conflict, proposal, and doctor workflows key state the same way.
        assert_eq!(
            crate::sync_state::repository_state_key(
                &crate::sync_state::sync_work_tree(reader_docs.vault_root()).expect("work tree")
            ),
            expected_key
        );
        assert_eq!(
            fs::read_to_string(fixture.reader.join("src/main.rs")).expect("code"),
            "fn main() { todo!() }\n",
            "sync carries commits to the whole repository work tree"
        );
        let refreshed = report.cache_refresh.expect("nested vault cache refresh");
        assert_eq!(refreshed.updated, 1);
        let listed = crate::sync_conflicts::list_sync_conflicts_with_state_store(
            &reader_docs,
            &fixture.store,
        )
        .expect("conflict listing from the nested vault");
        assert_eq!(listed.count, 0);
        assert!(!fixture.reader.join(".vulcan").exists());
    }

    #[test]
    fn subdirectory_invocation_keys_state_on_the_repository_root() {
        let fixture = structured_sync_fixture(&[("Home.md", "base\n")]);
        let expected_key = crate::sync_state::repository_state_key(
            &fs::canonicalize(&fixture.writer).expect("canonical writer"),
        );
        fs::create_dir(fixture.writer.join("sub")).expect("subdirectory");
        let report = sync_git_vault_with_state_store(
            &VaultPaths::new(fixture.writer.join("sub")),
            &GitSyncOptions::default(),
            &fixture.store,
        )
        .expect("subdirectory sync");
        assert_eq!(report.state.repository_key, expected_key);
        drop(fixture);
    }

    #[test]
    fn configured_branch_policy_only_narrows_caller_options() {
        let temporary = tempdir().expect("temporary directory");
        fs::create_dir(temporary.path().join(".vulcan")).expect("Vulcan directory");
        let paths = VaultPaths::new(temporary.path());
        let defaults = configured_git_sync_options(&paths, &GitSyncOptions::default())
            .expect("default options");
        assert!(defaults.branch_push);

        let caller = GitSyncOptions {
            branch_push: false,
            ..GitSyncOptions::default()
        };
        let kept = configured_git_sync_options(&paths, &caller).expect("caller options");
        assert!(!kept.branch_push);

        fs::write(
            temporary.path().join(".vulcan/config.toml"),
            "[sync]\nworktree = \"none\"\nbranch_push = false\n",
        )
        .expect("shared config");
        let configured = configured_git_sync_options(&paths, &GitSyncOptions::default())
            .expect("configured options");
        assert!(!configured.branch_push);
        assert_eq!(
            configured.branch_pull,
            vulcan_sync::BranchPullPolicy::Git,
            "an unset branch_pull leaves the device's Git configuration in charge"
        );

        fs::write(
            temporary.path().join(".vulcan/config.toml"),
            "[sync]\nbranch_pull = \"ff-only\"\n",
        )
        .expect("shared branch pull policy");
        let configured = configured_git_sync_options(&paths, &GitSyncOptions::default())
            .expect("configured branch pull policy");
        assert_eq!(
            configured.branch_pull,
            vulcan_sync::BranchPullPolicy::FfOnly
        );
    }

    #[test]
    fn malformed_vault_configuration_blocks_sync_before_transaction_state() {
        let temporary = tempdir().expect("temporary directory");
        fs::create_dir(temporary.path().join(".vulcan")).expect("Vulcan directory");
        fs::write(
            temporary.path().join(".vulcan/config.toml"),
            "[sync\nbranch_push = false\n",
        )
        .expect("malformed config");

        let error = configured_git_sync_options(
            &VaultPaths::new(temporary.path()),
            &GitSyncOptions::default(),
        )
        .expect_err("malformed config must fail closed");

        assert!(error
            .to_string()
            .contains("cannot synchronize with malformed configuration"));

        fs::write(temporary.path().join(".vulcan/config.toml"), "[sync]\n")
            .expect("valid shared config");
        fs::write(
            temporary.path().join(".vulcan/config.local.toml"),
            "[sync\nbranch_pull = \"merge\"\n",
        )
        .expect("malformed local config");
        assert!(configured_git_sync_options(
            &VaultPaths::new(temporary.path()),
            &GitSyncOptions::default()
        )
        .is_err());

        fs::remove_file(temporary.path().join(".vulcan/config.local.toml"))
            .expect("remove malformed local config");

        // Unrelated device-state parse failures must not block sync.
        fs::write(temporary.path().join(".vulcan/config.toml"), "[sync]\n")
            .expect("valid shared config");
        fs::create_dir(temporary.path().join(".obsidian")).expect("Obsidian directory");
        fs::write(temporary.path().join(".obsidian/app.json"), "{ not json\n")
            .expect("malformed Obsidian config");
        assert!(configured_git_sync_options(
            &VaultPaths::new(temporary.path()),
            &GitSyncOptions::default()
        )
        .is_ok());
    }

    #[test]
    fn a_bound_transport_failure_still_captures_local_bytes_first() {
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
                "ssh://git@forge.invalid/owner/vault.git",
            ],
        );
        fs::write(vault.join("Home.md"), "initial\n").expect("note");
        git(&vault, &["add", "Home.md"]);
        git(&vault, &["commit", "--quiet", "-m", "initial"]);
        // An upstream as if the branch had been pushed before.
        git(&vault, &["config", "branch.main.remote", "origin"]);
        git(&vault, &["config", "branch.main.merge", "refs/heads/main"]);
        git(&vault, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        let paths = VaultPaths::new(&vault);
        let state_store = SyncStateStore::at(temporary.path().join("state/sync/repositories"));
        state_store
            .load_or_create_device_id(true)
            .expect("identity is created");
        crate::sync_transport::bind_transport_with_store(
            &paths,
            &state_store,
            state_store.identity(),
            "origin",
            crate::sync_transport::GitConfigMode::Skip,
            false,
            Path::new("/usr/bin/vulcan"),
        )
        .expect("bind");
        // The device key becomes unavailable after binding.
        fs::remove_file(state_store.identity().private_key_path().expect("key path"))
            .expect("remove private key");
        fs::write(vault.join("Draft.md"), "unsaved local work\n").expect("draft");

        let error =
            sync_git_vault_with_state_store(&paths, &GitSyncOptions::default(), &state_store)
                .expect_err("the bound transport cannot authenticate");
        assert!(
            error
                .to_string()
                .contains("device-key Git transport unavailable"),
            "{error}"
        );

        // Local bytes were captured under a local ref before any network use.
        let refs = git_stdout(
            &vault,
            &["for-each-ref", "--format=%(refname)", "refs/vulcan"],
        );
        let preserved = refs.lines().any(|reference| {
            git_stdout(&vault, &["ls-tree", "-r", "--name-only", reference])
                .lines()
                .any(|path| path == "Draft.md")
        });
        assert!(
            preserved,
            "no local ref preserved Draft.md; refs were:\n{refs}"
        );
        assert_eq!(
            fs::read_to_string(vault.join("Draft.md")).expect("draft survives"),
            "unsaved local work\n"
        );
    }

    #[test]
    fn first_sync_registers_the_device_once_without_touching_the_work_tree() {
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
        fs::write(vault.join("Home.md"), "initial\n").expect("note");
        git(&vault, &["add", "Home.md"]);
        git(&vault, &["commit", "--quiet", "-m", "initial"]);
        git(&vault, &["push", "--quiet", "-u", "origin", "HEAD"]);
        let paths = VaultPaths::new(&vault);
        let state_store = SyncStateStore::at(temporary.path().join("state/sync/repositories"));

        let dry = sync_git_vault_with_state_store(
            &paths,
            &GitSyncOptions {
                dry_run: true,
                ..GitSyncOptions::default()
            },
            &state_store,
        )
        .expect("dry run");
        assert!(dry.registration.is_none());
        assert!(git_stdout(
            &remote,
            &["for-each-ref", "refs/heads/__vulcan-sync/registrations"]
        )
        .is_empty());

        let first =
            sync_git_vault_with_state_store(&paths, &GitSyncOptions::default(), &state_store)
                .expect("first sync");
        let device_id = state_store
            .load_or_create_device_id(false)
            .expect("identity")
            .expect("created by sync");
        assert_eq!(
            first.registration.expect("registration reported").outcome,
            Some(crate::sync_registration::SelfRegistrationOutcome::Created)
        );
        let refs = git_stdout(
            &remote,
            &[
                "for-each-ref",
                "--format=%(refname)",
                "refs/heads/__vulcan-sync/registrations",
            ],
        );
        assert_eq!(
            refs.trim(),
            format!(
                "refs/heads/__vulcan-sync/registrations/{}",
                device_id.as_str()
            )
        );

        let second =
            sync_git_vault_with_state_store(&paths, &GitSyncOptions::default(), &state_store)
                .expect("second sync");
        assert!(
            second.registration.is_none(),
            "already registered stays quiet"
        );
        assert!(
            !vault.join(".vulcan").exists(),
            "no device-local state in the work tree"
        );
        let published = git_stdout(
            &remote,
            &["ls-tree", "-r", "--name-only", "refs/heads/main"],
        );
        assert_eq!(
            published.trim(),
            "Home.md",
            "the registration never reaches the synced tree"
        );
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
        git(&writer, &["push", "--quiet", "-u", "origin", "main"]);

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
        git(&writer, &["add", "Remote.md"]);
        git(&writer, &["commit", "--quiet", "-m", "remote note"]);
        sync_git_vault_with_state_store(&writer_paths, &GitSyncOptions::default(), &state_store)
            .expect("writer push");
        let report = sync_git_vault_with_state_store(
            &reader_paths,
            &GitSyncOptions::default(),
            &state_store,
        )
        .expect("reader synchronization");

        assert_eq!(report.sync.outcome, GitSyncOutcome::Pulled);
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
    fn files_only_profile_applies_remote_files_without_refreshing_or_creating_an_index() {
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
        let state_store = SyncStateStore::at(temporary.path().join("state"));
        let writer_paths = VaultPaths::new(&writer);
        git(&writer, &["push", "--quiet", "-u", "origin", "main"]);

        let reader = temporary.path().join("reader");
        git(
            temporary.path(),
            &[
                "clone",
                "--quiet",
                writer.to_str().expect("writer"),
                reader.to_str().expect("reader"),
            ],
        );
        git(
            &reader,
            &[
                "remote",
                "set-url",
                "origin",
                remote.to_str().expect("remote"),
            ],
        );
        let reader_paths = VaultPaths::new(&reader);
        assert!(!reader_paths.cache_db().exists());
        sync_git_vault_with_profile_and_state_store(
            &reader_paths,
            &GitSyncOptions::default(),
            SyncContentProfile::FilesOnly,
            &state_store,
        )
        .expect("establish reader sync baseline");

        fs::write(writer.join("Remote.md"), "remote note\n").expect("remote note");
        git(&writer, &["add", "Remote.md"]);
        git(&writer, &["commit", "--quiet", "-m", "remote note"]);
        sync_git_vault_with_state_store(&writer_paths, &GitSyncOptions::default(), &state_store)
            .expect("writer push");
        let report = sync_git_vault_with_profile_and_state_store(
            &reader_paths,
            &GitSyncOptions::default(),
            SyncContentProfile::FilesOnly,
            &state_store,
        )
        .expect("files-only reader sync");

        assert!(
            reader.join("Remote.md").is_file(),
            "outcome {:?}",
            report.sync.outcome
        );
        assert!(report.cache_refresh.is_none());
        assert!(report.cache_refresh_error.is_none());
        assert!(!reader_paths.cache_db().exists());
    }

    #[test]
    fn cache_refresh_failure_does_not_fail_a_successful_sync() {
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
        git(&writer, &["config", "core.autocrlf", "false"]);
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
        git(&writer, &["push", "--quiet", "-u", "origin", "main"]);

        let reader = temporary.path().join("reader");
        git(
            temporary.path(),
            &[
                "-c",
                "core.autocrlf=false",
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
        git(&reader, &["config", "core.autocrlf", "false"]);
        let reader_paths = VaultPaths::new(&reader);
        initialize_vulcan_dir(&reader_paths).expect("initialize reader cache");
        scan_vault(&reader_paths, ScanMode::Full).expect("initial reader scan");
        // Corrupt the rebuildable cache; the sync itself must still succeed.
        fs::write(reader_paths.cache_db(), b"not a sqlite database").expect("corrupt cache");

        fs::write(writer.join("Remote.md"), "remote note\n").expect("remote note");
        git(&writer, &["add", "Remote.md"]);
        git(&writer, &["commit", "--quiet", "-m", "remote note"]);
        sync_git_vault_with_state_store(&writer_paths, &GitSyncOptions::default(), &state_store)
            .expect("writer push");
        let report = sync_git_vault_with_state_store(
            &reader_paths,
            &GitSyncOptions::default(),
            &state_store,
        )
        .expect("reader synchronization succeeds despite the cache failure");

        assert_eq!(report.sync.outcome, GitSyncOutcome::Pulled);
        assert!(report.cache_refresh.is_none());
        assert!(report.cache_refresh_error.is_some());
        assert_eq!(
            fs::read_to_string(reader.join("Remote.md")).expect("applied note"),
            "remote note\n"
        );
        assert!(state_store
            .load(&report.state.repository_key)
            .expect("load cleared journal")
            .is_none());
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
            git(&writer, &["push", "--quiet", "-u", "origin", "HEAD"]);
            (temporary, remote, writer)
        };
        let paths = VaultPaths::new(&writer);
        let store = SyncStateStore::at(temporary.path().join("state"));
        for phase in RECOVERABLE_JOURNAL_PHASES {
            assert_dry_run_recovers_journal_phase(&paths, &store, &writer, phase);
        }
        assert_eq!(
            store
                .load_or_create_device_id(false)
                .expect("no identity yet"),
            None
        );

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
        assert_eq!(
            store
                .load_or_create_device_id(false)
                .expect("no identity yet"),
            None
        );

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
        let error = sync_git_vault_with_state_store(&paths, &GitSyncOptions::default(), &store)
            .expect_err("non-repository must fail");
        let sync_error = error.sync_error().expect("typed sync error");
        assert_eq!(
            sync_error.category,
            vulcan_sync::SyncErrorCategory::Repository
        );
        assert!(!sync_error.retryable);

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
    fn staged_sync_proceeds_without_touching_the_normal_index() {
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
        git(&vault, &["push", "--quiet", "-u", "origin", "HEAD"]);
        fs::write(vault.join("Home.md"), "staged\n").expect("staged note");
        git(&vault, &["add", "Home.md"]);
        let paths = VaultPaths::new(&vault);
        let store = SyncStateStore::at(temporary.path().join("state"));

        let report = sync_git_vault_with_state_store(&paths, &GitSyncOptions::default(), &store)
            .expect("sync with staged changes");

        assert_ne!(report.sync.outcome, GitSyncOutcome::Paused);
        assert!(report.sync.pause.is_none());
        assert!(
            git_stdout(&vault, &["diff", "--cached", "--name-only"]).contains("Home.md"),
            "the staged index entry must survive synchronization untouched"
        );
    }

    #[test]
    fn progress_journal_records_failed_device_backup_publication_and_snapshot() {
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
        // The device backup publishes only when the vault opts in.
        fs::create_dir_all(vault.join(".vulcan")).expect("Vulcan directory");
        fs::write(
            vault.join(".vulcan/config.toml"),
            "[sync]\npublish_device_backup = true\n",
        )
        .expect("opt in to the device backup");
        let paths = VaultPaths::new(&vault);
        let store = SyncStateStore::at(temporary.path().join("state"));

        assert!(sync_git_vault_with_state_store(
            &paths,
            &GitSyncOptions {
                publish_device_backup: true,
                ..GitSyncOptions::default()
            },
            &store
        )
        .is_err());

        let key = crate::sync_state::repository_state_key(
            &fs::canonicalize(&vault).expect("canonical vault"),
        );
        let journal = store
            .load(&key)
            .expect("load journal")
            .expect("retained publication journal");
        assert_eq!(journal.phase, SyncJournalPhase::BackingUp);
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
        assert!(!report.checks.iter().any(|check| check.code == "git.remote"));
        assert!(report.checks.iter().any(|check| {
            check.code == "sync.device-identity" && check.severity == SyncDoctorSeverity::Info
        }));
        assert!(report.checks.iter().any(|check| {
            check.code == "cache.coherence" && check.severity == SyncDoctorSeverity::Info
        }));
        assert_eq!(report.platform_policy.profile, GitPlatformProfile::native());
        assert!(report
            .platform_preflight
            .as_ref()
            .is_some_and(|preflight| { preflight.compatible && preflight.entries == 2 }));
        assert!(!store.root().exists());

        let files_only = doctor_git_vault_with_profile_and_state_store(
            &paths,
            &GitSyncOptions::default(),
            GitPlatformProfile::native(),
            &store,
            SyncContentProfile::FilesOnly,
            true,
        );
        assert!(files_only.checks.iter().any(|check| {
            check.code == "git.files-only-safety" && check.severity == SyncDoctorSeverity::Pass
        }));
        assert!(!files_only
            .checks
            .iter()
            .any(|check| check.code.starts_with("cache.")));
    }

    #[test]
    fn configured_worktree_none_is_a_no_op_and_sync_never_publishes_live() {
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
        fs::create_dir_all(vault.join(".vulcan")).expect("vault directory");
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
        fs::write(
            vault.join(".vulcan/config.toml"),
            "[sync]\nworktree = \"none\"\n",
        )
        .expect("shared config");
        fs::write(vault.join("Home.md"), "home\n").expect("home note");
        git(&vault, &["add", "--all"]);
        git(&vault, &["commit", "--quiet", "-m", "initial"]);
        git(&vault, &["push", "--quiet", "-u", "origin", "main"]);
        fs::write(vault.join("script.sh"), "echo hi\n").expect("script");
        git(&vault, &["add", "script.sh"]);
        git(&vault, &["commit", "--quiet", "-m", "script"]);
        fs::write(vault.join("Home.md"), "uncommitted\n").expect("uncommitted edit");
        let paths = VaultPaths::new(&vault);
        let store = SyncStateStore::at(temporary.path().join("state"));

        let report = sync_git_vault_with_state_store(&paths, &GitSyncOptions::default(), &store)
            .expect("committed-history sync");

        assert_eq!(report.sync.outcome, GitSyncOutcome::Pushed);
        assert!(report.state.retained.is_none());
        assert_eq!(
            git_stdout(&remote, &["rev-parse", "refs/heads/main"]),
            git_stdout(&vault, &["rev-parse", "HEAD"])
        );
        assert!(git_stdout(
            &remote,
            &[
                "for-each-ref",
                "--format=%(refname)",
                vulcan_sync::DEFAULT_REMOTE_LIVE_REF
            ]
        )
        .is_empty());

        let doctor = doctor_git_vault_with_state_store(&paths, &GitSyncOptions::default(), &store);
        assert!(!doctor
            .checks
            .iter()
            .any(|check| check.code == "sync.worktree"));
        assert!(!doctor.checks.iter().any(|check| check.code == "git.remote"));
    }

    #[test]
    fn sync_doctor_accepts_cache_ignores_of_a_subdirectory_vault() {
        let temporary = tempdir().expect("temporary directory");
        let repository = temporary.path().join("repository");
        let vault = repository.join("docs");
        fs::create_dir_all(vault.join(".vulcan")).expect("vault state directory");
        git(
            &repository,
            &["-c", "init.defaultBranch=main", "init", "--quiet"],
        );
        git(&repository, &["config", "user.name", "Vulcan Test"]);
        git(
            &repository,
            &["config", "user.email", "vulcan@example.invalid"],
        );
        fs::write(vault.join(".vulcan/.gitignore"), "*\n!.gitignore\n").expect("ignore file");
        fs::write(vault.join("Home.md"), "home\n").expect("home note");
        git(&repository, &["add", "--all"]);
        git(&repository, &["commit", "--quiet", "-m", "initial"]);
        let store = SyncStateStore::at(temporary.path().join("state"));

        let report = doctor_git_vault_with_state_store(
            &VaultPaths::new(&vault),
            &GitSyncOptions::default(),
            &store,
        );

        let check = report
            .checks
            .iter()
            .find(|check| check.code == "git.internal-ignore")
            .expect("cache ignore check");
        assert_eq!(check.severity, SyncDoctorSeverity::Pass, "{check:?}");
    }

    #[test]
    fn only_a_device_backup_bridged_with_foreign_history_warns() {
        let oid = vulcan_sync::GitOid::parse("1".repeat(40)).expect("oid");
        let backup = |outcome| GitDeviceBackup {
            device_id: vulcan_sync::GitSyncDeviceId::parse(format!("vdev1_{}", "a".repeat(52)))
                .expect("device ID"),
            reference: vulcan_sync::GitRefName::parse("refs/heads/__vulcan-sync/devices/p/d")
                .expect("ref"),
            snapshot_revision: oid.clone(),
            published_revision: oid.clone(),
            outcome,
            foreign_history: false,
        };
        let foreign = |outcome| GitDeviceBackup {
            foreign_history: true,
            ..backup(outcome)
        };
        assert_eq!(device_backup_warning(None), None);
        // A bridge over history this checkout rewrote itself is routine.
        assert_eq!(
            device_backup_warning(Some(&backup(GitDeviceBackupOutcome::Bridged))),
            None
        );
        for outcome in [
            GitDeviceBackupOutcome::Current,
            GitDeviceBackupOutcome::Published,
        ] {
            assert_eq!(device_backup_warning(Some(&backup(outcome))), None);
        }
        let warning = device_backup_warning(Some(&foreign(GitDeviceBackupOutcome::Bridged)))
            .expect("bridged warning");
        assert!(warning.contains(&format!("vdev1_{}", "a".repeat(52))));
        assert!(warning.contains("vulcan device replace"));
    }

    #[test]
    fn sync_doctor_warns_about_tracked_per_device_files_in_a_subdirectory_vault() {
        let temporary = tempdir().expect("temporary directory");
        let repository = temporary.path().join("repository");
        let vault = repository.join("docs");
        fs::create_dir_all(vault.join(".obsidian")).expect("obsidian directory");
        git(
            &repository,
            &["-c", "init.defaultBranch=main", "init", "--quiet"],
        );
        git(&repository, &["config", "user.name", "Vulcan Test"]);
        git(
            &repository,
            &["config", "user.email", "vulcan@example.invalid"],
        );
        fs::write(vault.join(".obsidian/workspace.json"), "{}\n").expect("layout");
        fs::write(vault.join("Home.md"), "home\n").expect("home note");
        git(&repository, &["add", "--all"]);
        git(&repository, &["commit", "--quiet", "-m", "initial"]);
        let store = SyncStateStore::at(temporary.path().join("state"));
        let paths = VaultPaths::new(&vault);
        let device_files = || {
            doctor_git_vault_with_state_store(&paths, &GitSyncOptions::default(), &store)
                .checks
                .into_iter()
                .find(|check| check.code == "sync.device-files")
        };

        let check = device_files().expect("device-file warning");
        assert_eq!(check.severity, SyncDoctorSeverity::Warning);
        assert!(check.message.contains(".obsidian/workspace.json"));
        assert!(check.message.contains("vulcan sync doctor --fix"));

        // A dry run of the repair names it and writes nothing.
        let mut report =
            doctor_git_vault_with_state_store(&paths, &GitSyncOptions::default(), &store);
        apply_sync_doctor_fixes(&paths, &mut report, true).expect("dry-run fix");
        assert_eq!(report.fixes.len(), 1);
        assert!(report.fixes[0].dry_run);
        assert!(report.fixes[0]
            .message
            .contains("/.obsidian/workspace.json"));
        assert!(!vault.join(".gitignore").exists());

        assert!(report.fixes[0].message.contains("would remove"));

        apply_sync_doctor_fixes(&paths, &mut report, false).expect("fix");
        assert!(fs::read_to_string(vault.join(".gitignore"))
            .expect("gitignore")
            .contains("/.obsidian/workspace.json"));
        // The fix stages the untracking but keeps the file on disk.
        assert!(vault.join(".obsidian/workspace.json").is_file());
        assert_eq!(
            git_stdout(&repository, &["ls-files", "docs/.obsidian/workspace.json"]),
            ""
        );
        // Until it is committed the file is still shared, and the doctor
        // says so; repeating the fix finds nothing left to change.
        let pending = device_files().expect("pending note");
        assert_eq!(pending.severity, SyncDoctorSeverity::Info);
        assert!(pending.message.contains("commit their removal"));
        let mut again =
            doctor_git_vault_with_state_store(&paths, &GitSyncOptions::default(), &store);
        apply_sync_doctor_fixes(&paths, &mut again, false).expect("nothing to fix");
        assert!(again.fixes.is_empty());

        git(&repository, &["add", "--all"]);
        git(
            &repository,
            &["commit", "--quiet", "-m", "ignore and untrack"],
        );
        assert_eq!(device_files(), None);
        assert!(vault.join(".obsidian/workspace.json").is_file());
    }

    #[test]
    fn sync_doctor_fix_untracks_device_files_a_gitignore_already_ignores() {
        let temporary = tempdir().expect("temporary directory");
        let vault = temporary.path().join("vault");
        fs::create_dir_all(vault.join(".obsidian")).expect("obsidian directory");
        git(
            &vault,
            &["-c", "init.defaultBranch=main", "init", "--quiet"],
        );
        git(&vault, &["config", "user.name", "Vulcan Test"]);
        git(&vault, &["config", "user.email", "vulcan@example.invalid"]);
        fs::write(vault.join(".obsidian/workspace.json"), "{}\n").expect("layout");
        git(&vault, &["add", "--all"]);
        git(&vault, &["commit", "--quiet", "-m", "initial"]);
        // Someone added the entry by hand but never untracked the file.
        fs::write(vault.join(".gitignore"), "/.obsidian/workspace.json\n").expect("gitignore");
        let store = SyncStateStore::at(temporary.path().join("state"));
        let paths = VaultPaths::new(&vault);
        let mut report =
            doctor_git_vault_with_state_store(&paths, &GitSyncOptions::default(), &store);
        apply_sync_doctor_fixes(&paths, &mut report, false).expect("fix");
        assert_eq!(report.fixes.len(), 1, "{:?}", report.checks);
        assert!(report.fixes[0]
            .message
            .contains("removed .obsidian/workspace.json from the index"));
        assert_eq!(
            git_stdout(&vault, &["ls-files", ".obsidian/workspace.json"]),
            ""
        );
        assert!(vault.join(".obsidian/workspace.json").is_file());
    }

    #[test]
    fn sync_reports_shared_device_files_until_the_gitignore_covers_them() {
        let fixture =
            structured_sync_fixture(&[(".obsidian/workspace.json", "{}\n"), ("Home.md", "home\n")]);
        let paths = VaultPaths::new(&fixture.writer);
        let sync = || {
            sync_git_vault_with_state_store(&paths, &GitSyncOptions::default(), &fixture.store)
                .expect("sync")
        };
        assert_eq!(sync().device_files, [".obsidian/workspace.json"]);
        // Sync only reports the files; it never edits the `.gitignore` itself.
        assert!(!fixture.writer.join(".gitignore").exists());

        let ignored = crate::sync_device_files::ignore_device_files(&paths, false).expect("ignore");
        assert_eq!(ignored.untracked, [".obsidian/workspace.json"]);
        // Sync carries only commits, so the staged untracking travels as one.
        git(&fixture.writer, &["add", ".gitignore"]);
        git(
            &fixture.writer,
            &["commit", "--quiet", "-m", "ignore device files"],
        );
        assert!(sync().device_files.is_empty());
        assert!(fixture.writer.join(".obsidian/workspace.json").is_file());
        let reader = VaultPaths::new(&fixture.reader);
        assert!(sync_git_vault_with_state_store(
            &reader,
            &GitSyncOptions::default(),
            &fixture.store
        )
        .expect("reader sync")
        .device_files
        .is_empty());
    }

    #[test]
    fn sync_doctor_applies_an_explicit_target_platform_without_host_dependence() {
        let temporary = tempdir().expect("temporary directory");
        let vault = temporary.path().join("vault");
        fs::create_dir(&vault).expect("vault directory");
        git(
            &vault,
            &["-c", "init.defaultBranch=main", "init", "--quiet"],
        );
        git(&vault, &["config", "user.name", "Vulcan Test"]);
        git(&vault, &["config", "user.email", "vulcan@example.invalid"]);
        git(&vault, &["config", "core.ignoreCase", "false"]);
        git(&vault, &["config", "core.protectNTFS", "false"]);
        let blob = git_stdout_with_stdin(&vault, &["hash-object", "-w", "--stdin"], b"fixture\n");
        for path in ["CON.txt", "Notes/Alpha.md", "notes/alpha.md"] {
            git(
                &vault,
                &[
                    "update-index",
                    "--add",
                    "--cacheinfo",
                    "100644",
                    &blob,
                    path,
                ],
            );
        }
        let tree = git_stdout(&vault, &["write-tree"]);
        let commit = git_stdout(&vault, &["commit-tree", &tree, "-m", "portable fixtures"]);
        git(&vault, &["update-ref", "refs/heads/main", &commit]);
        let paths = VaultPaths::new(&vault);
        let store = SyncStateStore::at(temporary.path().join("state"));

        let report = doctor_git_vault_with_optional_state(
            &paths,
            &GitSyncOptions::default(),
            GitPlatformProfile::AndroidShared,
            Some(&store),
            SyncContentProfile::Knowledge,
            false,
        );

        assert!(!report.healthy);
        assert_eq!(
            report.platform_policy.profile,
            GitPlatformProfile::AndroidShared
        );
        assert!(report
            .platform_preflight
            .as_ref()
            .is_some_and(|preflight| !preflight.compatible));
        for code in ["platform.case-collision", "platform.reserved-name"] {
            assert!(report.checks.iter().any(|check| {
                check.code == code && check.severity == SyncDoctorSeverity::Error
            }));
        }
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
    fn sync_doctor_reports_unavailable_round_trip_filter_drivers() {
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
        fs::write(vault.join(".gitattributes"), "*.protected filter=missing\n")
            .expect("attributes");
        fs::write(vault.join("asset.protected"), "protected bytes\n").expect("asset");
        git(&vault, &["add", ".gitattributes", "asset.protected"]);
        git(&vault, &["commit", "--quiet", "-m", "filtered asset"]);
        let report = doctor_git_vault_with_state_store(
            &VaultPaths::new(&vault),
            &GitSyncOptions::default(),
            &SyncStateStore::at(temporary.path().join("state")),
        );

        assert!(!report.healthy);
        let requirement = report
            .requirements
            .as_ref()
            .and_then(|requirements| requirements.required_filters.first())
            .expect("filter requirement");
        assert_eq!(requirement.name, "missing");
        assert!(!requirement.ready());
        assert!(report.checks.iter().any(|check| {
            check.code == "git.filters" && check.severity == SyncDoctorSeverity::Error
        }));
    }
}
