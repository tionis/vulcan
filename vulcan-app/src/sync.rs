//! Complete direct-mode vault synchronization workflows.

use crate::sync_conflicts::{conflict_groups, SyncConflictRecord, SyncConflictStore};
use crate::sync_state::{SyncApplyMarker, SyncJournal, SyncJournalPhase, SyncStateStore};
use crate::{scan::refresh_cache_incrementally, AppError};
use fs2::FileExt;
use serde::{Serialize, Serializer};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use vulcan_core::{
    load_vault_config, parse_document, LinkResolutionProblem, ResolverDocument, ResolverIndex,
    ResolverLink, ScanSummary, VaultConfig, VaultPaths,
};
use vulcan_sync::{GitAutomaticMergeValidation, GitEngine};

const MAX_SYNC_REPORT_CONFLICT_RECORD_PATHS: usize = 16;
const MAX_SYNC_REPORT_CONFLICT_RECORD_DIAGNOSTIC_CHARS: usize = 4096;

pub use vulcan_sync::{
    GitBranchSync, GitBranchSyncAction, GitCloneRequest, GitDetachedRecoveryReport,
    GitDetachedRecoveryRequest, GitDeviceBackup, GitDeviceBackupOutcome, GitInstallation,
    GitObjectFormat, GitPlatformPolicy, GitPlatformPreflight, GitPlatformProfile, GitRefName,
    GitRemote, GitRemoteObservation, GitRepository, GitRepositoryLayout, GitRepositoryRequirements,
    GitSyncAction, GitSyncConflict, GitSyncDeviceId, GitSyncObserver, GitSyncObserverError,
    GitSyncOptions, GitSyncOutcome, GitSyncPause, GitSyncPauseReason, GitSyncPhase,
    GitSyncPreviewFileState, GitSyncProgress, GitSyncRefs, GitSyncReport,
    GitUnattendedRepositoryState, SyncCancellationToken,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(serialize_with = "serialize_sync_report_conflict_record")]
    pub conflict_record: Option<SyncConflictRecord>,
    pub operational_stats: SyncOperationalStats,
    pub state: VaultSyncStateReport,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SyncOperationalStats {
    pub version: u32,
    pub automatic_resolution_paths: usize,
    pub conflict_paths: usize,
    pub conflict_groups: usize,
    pub formatting_candidate_paths: usize,
    pub preserved_input_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_subprocesses: Option<u64>,
    pub elapsed_ms: u64,
    pub backend_cycle_ms: u64,
    pub conflict_state_ms: u64,
    pub cache_refresh_ms: u64,
}

#[derive(Serialize)]
struct SyncConflictRecordReportSummary<'a> {
    version: u32,
    id: &'a str,
    repository_key: &'a str,
    scope: vulcan_sync::GitConflictScope,
    base_revision: Option<&'a str>,
    local_revision: &'a str,
    remote_revision: &'a str,
    paths: Vec<&'a str>,
    paths_returned: usize,
    paths_complete: bool,
    path_count: usize,
    group_count: usize,
    policy_version: u32,
    policy_hash: &'a str,
    preserved_record_ref: Option<&'a str>,
    provenance_revision: Option<&'a str>,
    projection: Option<&'a crate::sync_conflicts::SyncConflictProjectionRecord>,
    diagnostics: String,
    diagnostics_complete: bool,
    detail_conflict_id: &'a str,
}

#[allow(clippy::ref_option)] // serde's field serializer receives a reference to the field type.
fn serialize_sync_report_conflict_record<S>(
    record: &Option<SyncConflictRecord>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let summary = record.as_ref().map(|record| {
        let paths_returned = record
            .paths
            .len()
            .min(MAX_SYNC_REPORT_CONFLICT_RECORD_PATHS);
        let paths = record.paths[..paths_returned]
            .iter()
            .map(|path| path.path.as_str())
            .collect();
        let mut diagnostic_chars = record.diagnostics.chars();
        let diagnostics = diagnostic_chars
            .by_ref()
            .take(MAX_SYNC_REPORT_CONFLICT_RECORD_DIAGNOSTIC_CHARS)
            .collect::<String>();
        let diagnostics_complete = diagnostic_chars.next().is_none();
        SyncConflictRecordReportSummary {
            version: record.version,
            id: &record.id,
            repository_key: &record.repository_key,
            scope: record.scope,
            base_revision: record.base_revision.as_deref(),
            local_revision: &record.local_revision,
            remote_revision: &record.remote_revision,
            paths,
            paths_returned,
            paths_complete: paths_returned == record.paths.len(),
            path_count: record.paths.len(),
            group_count: conflict_groups(record).len(),
            policy_version: record.policy_version,
            policy_hash: &record.policy_hash,
            preserved_record_ref: record.preserved_record_ref.as_deref(),
            provenance_revision: record.provenance_revision.as_deref(),
            projection: record.projection.as_ref(),
            diagnostics,
            diagnostics_complete,
            detail_conflict_id: &record.id,
        }
    });
    summary.serialize(serializer)
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
    pub platform_policy: GitPlatformPolicy,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform_preflight: Option<GitPlatformPreflight>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub journal: Option<SyncJournal>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub apply_marker: Option<SyncApplyMarker>,
    pub checks: Vec<SyncDoctorCheck>,
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
        configured_options_for_doctor_profile(paths, options, profile);
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
    doctor_platform_tree(&engine, &repository, options, &mut report);
    doctor_repository_lock(&repository, &mut report);
    doctor_journal(paths, state_store, &mut report);
    doctor_apply_marker(state_store, &repository, &mut report);
    if profile == SyncContentProfile::Knowledge {
        doctor_cache(paths, &mut report);
    }
    finish_doctor_report(report)
}

fn configured_options_for_doctor_profile(
    paths: &VaultPaths,
    options: &GitSyncOptions,
    profile: SyncContentProfile,
) -> (GitSyncOptions, SyncDoctorSeverity, String) {
    match configured_git_sync_options_for_profile(paths, options, profile) {
        Ok(options) => {
            let detail = format!(
                "merge policy v{} is valid with automation ceiling {:?}",
                options.merge_policy.version, options.merge_automation
            );
            (options, SyncDoctorSeverity::Pass, detail)
        }
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
        live_ref: options.live_ref.clone(),
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
    let state_store = SyncStateStore::user_default()?;
    let mut observer = vulcan_sync::IgnoreGitSyncProgress;
    sync_git_vault_with_profile_and_observer_and_engine(
        &crate::sync_transport::apply_transport_with_state(
            vulcan_sync::GitCliEngine::default(),
            paths,
            &state_store,
        )
        .with_command_timeout(options.command_timeout),
        paths,
        options,
        &state_store,
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
    let (report, carried) = sync_git_vault_cycle(
        engine,
        paths,
        options,
        state_store,
        cancellation,
        delegate,
        profile,
        unattended,
    )?;
    if !carried {
        return Ok(report);
    }
    // Carrying an overtaken conflict forward wrote merged local changes into
    // the worktree; publish them in this same synchronization.
    let (report, _) = sync_git_vault_cycle(
        engine,
        paths,
        options,
        state_store,
        cancellation,
        delegate,
        profile,
        unattended,
    )?;
    Ok(report)
}

/// One finite synchronization transaction, and whether it carried a conflict
/// forward by writing merged local changes that still need publishing.
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
) -> Result<(VaultSyncReport, bool), AppError> {
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
    let options = configured_git_sync_options_for_profile(paths, options, profile)?;
    let branch_guard = if unattended && profile == SyncContentProfile::FilesOnly {
        validate_unattended_files_only_repository(engine, paths)?
    } else {
        None
    };
    let mut journal = SyncJournal::preparing(
        &work_tree,
        options.remote.to_string(),
        options.live_ref.to_string(),
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
    let validation_config = (profile == SyncContentProfile::Knowledge)
        .then(|| load_validated_sync_config(paths))
        .transpose()?;
    let mut observer = JournalSyncObserver {
        state_store,
        journal: &mut journal,
        persist: !options.dry_run,
        delegate,
        profile,
        engine,
        branch_guard,
        tree_validator: validation_config.map(VaultTreeValidator::new),
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
    let conflict_state_started = Instant::now();
    let (conflict_record, carried) = persist_and_update_conflicts(
        engine,
        paths,
        &effective_options,
        &sync,
        &mut journal,
        state_store,
        !options.dry_run,
    )?;
    let conflict_state = conflict_state_started.elapsed();
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
        [backend_cycle, conflict_state, cache_refresh_duration],
    );
    let operational_stats =
        sync_operational_stats(engine, &timings, &sync, conflict_record.as_ref());
    Ok((
        VaultSyncReport {
            sync,
            cache_refresh,
            cache_refresh_error,
            registration,
            conflict_record,
            operational_stats,
            state: VaultSyncStateReport {
                repository_key,
                journal_path,
                recovered_from,
                retained,
            },
        },
        carried,
    ))
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
    } else if matches!(outcome, GitSyncOutcome::Paused | GitSyncOutcome::Conflicted) {
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
    conflict_state: Duration,
    cache_refresh: Duration,
}

impl SyncOperationTimings {
    fn new(subprocesses_before: Option<u64>, started: Instant, stages: [Duration; 3]) -> Self {
        Self {
            subprocesses_before,
            elapsed: started.elapsed(),
            backend_cycle: stages[0],
            conflict_state: stages[1],
            cache_refresh: stages[2],
        }
    }
}

fn sync_operational_stats(
    engine: &dyn GitEngine,
    timings: &SyncOperationTimings,
    sync: &GitSyncReport,
    conflict_record: Option<&SyncConflictRecord>,
) -> SyncOperationalStats {
    let automatic_resolution_paths = sync.automatic_resolutions.len();
    let conflict_paths = conflict_record.map_or(0, |record| record.paths.len());
    let conflict_groups = conflict_record.map_or(0, |record| conflict_groups(record).len());
    let formatting_candidate_paths = conflict_record.map_or(0, |record| {
        record
            .paths
            .iter()
            .filter(|path| {
                path.classification
                    .as_ref()
                    .is_some_and(|classification| classification.formatting_candidate)
            })
            .count()
    });
    let preserved_input_bytes = conflict_record.map_or(0, |record| {
        record.paths.iter().fold(0_u64, |total, path| {
            [&path.base, &path.local, &path.remote]
                .into_iter()
                .filter_map(|side| side.bytes)
                .fold(total, u64::saturating_add)
        })
    });
    SyncOperationalStats {
        version: 1,
        automatic_resolution_paths,
        conflict_paths,
        conflict_groups,
        formatting_candidate_paths,
        preserved_input_bytes,
        git_subprocesses: timings
            .subprocesses_before
            .zip(engine.subprocess_count())
            .map(|(before, after)| after.saturating_sub(before)),
        elapsed_ms: duration_millis(timings.elapsed),
        backend_cycle_ms: duration_millis(timings.backend_cycle),
        conflict_state_ms: duration_millis(timings.conflict_state),
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
    let should_refresh = !options.dry_run
        && sync.actions.contains(&GitSyncAction::WorktreeApplied)
        && paths.cache_db().is_file();
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

fn supersede_obsolete_conflicts(
    engine: &dyn GitEngine,
    paths: &VaultPaths,
    options: &GitSyncOptions,
    sync: &GitSyncReport,
    current_conflict: Option<&SyncConflictRecord>,
    state_store: &SyncStateStore,
    journal: &SyncJournal,
) -> Result<bool, AppError> {
    let store = SyncConflictStore::from_state_store(state_store);
    if let Some(record) = current_conflict {
        let current_revision = record
            .provenance_revision
            .as_deref()
            .unwrap_or(&record.remote_revision);
        store.supersede_unresolved_except(
            &journal.repository_key,
            Some(&record.id),
            current_revision,
        )?;
        // Older conflicts the new one does not cover may have been overtaken
        // too. The worktree holds the frontier only once the new conflict's
        // projection was applied; the new conflict itself is never overtaken.
        let applied = record
            .projection
            .as_ref()
            .is_some_and(|projection| projection.published && projection.applied);
        if !applied {
            return Ok(false);
        }
        let frontier = vulcan_sync::GitOid::parse(current_revision).map_err(AppError::operation)?;
        return Ok(carry_forward_overtaken_conflicts(
            engine, paths, options, sync, &store, journal, &frontier,
        ));
    }
    if matches!(
        sync.outcome,
        GitSyncOutcome::Paused | GitSyncOutcome::Planned
    ) {
        return Ok(false);
    }
    let current_revision = sync
        .accepted
        .as_ref()
        .or(sync.remote_before.as_ref())
        .or(sync.local_before.as_ref());
    if let Some(current_revision) = current_revision {
        store.supersede_unresolved_except(
            &journal.repository_key,
            None,
            current_revision.as_str(),
        )?;
    }
    // The accepted live frontier this cycle ended on; overtaken conflicts are
    // judged against it, never against a merely local revision.
    let Some(frontier) = sync.accepted.as_ref().or(sync.remote_before.as_ref()) else {
        return Ok(false);
    };
    Ok(carry_forward_overtaken_conflicts(
        engine, paths, options, sync, &store, journal, frontier,
    ))
}

/// Carries overtaken conflicts forward under the vault and repository locks.
/// Carrying forward is opportunistic: if the locks are busy or it fails, the
/// conflicts stay as they are and the next sync tries again.
fn carry_forward_overtaken_conflicts(
    engine: &dyn GitEngine,
    paths: &VaultPaths,
    options: &GitSyncOptions,
    sync: &GitSyncReport,
    store: &SyncConflictStore,
    journal: &SyncJournal,
    frontier: &vulcan_sync::GitOid,
) -> bool {
    let Ok(_vault_lock) = paths
        .vulcan_dir()
        .is_dir()
        .then(|| vulcan_core::write_lock::acquire_write_lock(paths))
        .transpose()
    else {
        return false;
    };
    let Ok(_repository_lock) = vulcan_sync::RepositoryLock::acquire(&sync.repository.git_dir)
    else {
        return false;
    };
    store
        .carry_forward_stale_unresolved(
            engine,
            &sync.repository,
            &journal.repository_key,
            frontier,
            options,
        )
        .is_ok_and(|summary| !summary.written_paths.is_empty())
}

fn persist_and_update_conflicts(
    engine: &dyn GitEngine,
    paths: &VaultPaths,
    options: &GitSyncOptions,
    sync: &GitSyncReport,
    journal: &mut SyncJournal,
    state_store: &SyncStateStore,
    persist: bool,
) -> Result<(Option<SyncConflictRecord>, bool), AppError> {
    let record = persist_sync_conflict(engine, sync, journal, state_store, persist)?;
    let carried = update_conflict_lifecycle(
        engine,
        paths,
        options,
        sync,
        record.as_ref(),
        state_store,
        journal,
        !persist,
    )?;
    Ok((record, carried))
}

#[allow(clippy::too_many_arguments)]
fn update_conflict_lifecycle(
    engine: &dyn GitEngine,
    paths: &VaultPaths,
    options: &GitSyncOptions,
    sync: &GitSyncReport,
    current_conflict: Option<&SyncConflictRecord>,
    state_store: &SyncStateStore,
    journal: &mut SyncJournal,
    dry_run: bool,
) -> Result<bool, AppError> {
    if dry_run {
        return Ok(false);
    }
    match supersede_obsolete_conflicts(
        engine,
        paths,
        options,
        sync,
        current_conflict,
        state_store,
        journal,
    ) {
        Ok(carried) => Ok(carried),
        Err(error) => {
            journal.error = Some(error.to_string());
            state_store.save(journal)?;
            Err(error)
        }
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
    configured_git_sync_options_for_profile(paths, options, SyncContentProfile::Knowledge)
}

pub fn configured_git_sync_options_for_profile(
    paths: &VaultPaths,
    options: &GitSyncOptions,
    profile: SyncContentProfile,
) -> Result<GitSyncOptions, AppError> {
    let config = load_sync_config(paths, profile == SyncContentProfile::Knowledge)?;
    let mut effective = options.clone();
    if let Some(policy) = config.sync.merge_policy {
        effective.merge_policy = policy;
    }
    if config.sync.merge_automation == vulcan_sync::MergeAutomation::RequireReview
        || options.merge_automation == vulcan_sync::MergeAutomation::RequireReview
    {
        effective.merge_automation = vulcan_sync::MergeAutomation::RequireReview;
    }
    effective
        .merge_policy
        .validate()
        .map_err(AppError::operation)?;
    Ok(effective)
}

pub(crate) fn load_validated_sync_config(paths: &VaultPaths) -> Result<VaultConfig, AppError> {
    load_sync_config(paths, true)
}

fn load_sync_config(
    paths: &VaultPaths,
    validate_tree_policy: bool,
) -> Result<VaultConfig, AppError> {
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
    if validate_tree_policy {
        loaded
            .config
            .sync
            .tree_validation
            .validate()
            .map_err(AppError::operation)?;
    }
    if let Some(policy) = &loaded.config.sync.merge_policy {
        policy.validate().map_err(AppError::operation)?;
    }
    Ok(loaded.config)
}

pub(crate) fn validate_git_merge_tree(
    config: &VaultConfig,
    engine: &dyn GitEngine,
    request: &GitAutomaticMergeValidation<'_>,
) -> Result<(), AppError> {
    VaultTreeValidator::new(config.clone())
        .validate(engine, request)
        .map_err(AppError::operation)
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
    profile: SyncContentProfile,
    engine: &'a dyn GitEngine,
    branch_guard: Option<GitRefName>,
    tree_validator: Option<VaultTreeValidator>,
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

    fn validate_automatic_merge(
        &mut self,
        engine: &dyn GitEngine,
        request: &GitAutomaticMergeValidation<'_>,
    ) -> Result<Vec<vulcan_sync::GitAutomaticValidationCheck>, GitSyncObserverError> {
        if self.profile == SyncContentProfile::FilesOnly {
            let base = engine
                .tree_oid(request.repository, request.base)
                .map_err(|error| GitSyncObserverError::new(error.to_string()))?;
            let local = engine
                .tree_oid(request.repository, request.local_candidate)
                .map_err(|error| GitSyncObserverError::new(error.to_string()))?;
            let remote = engine
                .tree_oid(request.repository, request.accepted_remote)
                .map_err(|error| GitSyncObserverError::new(error.to_string()))?;
            if (local == base && request.merged_tree == &remote)
                || (remote == base && request.merged_tree == &local)
            {
                return self.delegate.validate_automatic_merge(engine, request);
            }
            return Err(GitSyncObserverError::new(
                "files-only profile requires review of concurrent merges to preserve shared accepted bytes across knowledge and files-only devices",
            ));
        }
        let Some(validator) = &self.tree_validator else {
            return Ok(Vec::new());
        };
        validator.validate(engine, request)?;
        let mut checks = vec![
            vulcan_sync::GitAutomaticValidationCheck::WholeTreeLinksValid,
            vulcan_sync::GitAutomaticValidationCheck::MassDeletionPolicy,
        ];
        for check in self.delegate.validate_automatic_merge(engine, request)? {
            if !checks.contains(&check) {
                checks.push(check);
            }
        }
        Ok(checks)
    }
}

const MAX_VALIDATED_MARKDOWN_FILES: usize = 100_000;
const MAX_VALIDATED_MARKDOWN_BYTES: usize = 512 * 1024 * 1024;
const MAX_VALIDATED_CANVAS_FILES: usize = 10_000;
const MAX_VALIDATED_CANVAS_BYTES: usize = 64 * 1024 * 1024;

struct VaultTreeValidator {
    config: VaultConfig,
}

impl VaultTreeValidator {
    fn new(config: VaultConfig) -> Self {
        Self { config }
    }

    fn validate(
        &self,
        engine: &dyn GitEngine,
        request: &GitAutomaticMergeValidation<'_>,
    ) -> Result<(), GitSyncObserverError> {
        self.config
            .sync
            .tree_validation
            .validate()
            .map_err(GitSyncObserverError::new)?;
        let fingerprint = merge_validation_fingerprint(&self.config);
        let mut cache = GitTreeAnalysisCache {
            store: MergeValidationStore::open(request.repository, &fingerprint),
            ..GitTreeAnalysisCache::default()
        };
        let local = analyze_git_tree(
            engine,
            request.repository,
            request.local_candidate,
            &self.config,
            &mut cache,
        )?;
        let remote = analyze_git_tree(
            engine,
            request.repository,
            request.accepted_remote,
            &self.config,
            &mut cache,
        )?;
        let merged = analyze_git_tree(
            engine,
            request.repository,
            request.merged_tree,
            &self.config,
            &mut cache,
        )?;

        if let Some(store) = &cache.store {
            let referenced = local
                .blobs
                .iter()
                .chain(&remote.blobs)
                .chain(&merged.blobs)
                .collect::<BTreeSet<_>>();
            store.prune_unless(&referenced);
        }

        let candidate_paths = local
            .paths
            .union(&remote.paths)
            .cloned()
            .collect::<BTreeSet<_>>();
        let deleted = candidate_paths.difference(&merged.paths).count();
        let limits = &self.config.sync.tree_validation;
        let exceeds_percent = (deleted as u128) * 100
            > (candidate_paths.len() as u128) * u128::from(limits.max_deleted_percent);
        if deleted > limits.max_deleted_paths && exceeds_percent {
            return Err(GitSyncObserverError::new(format!(
                "automatic merge would delete {deleted} of {} candidate paths, exceeding the shared limits of {} paths and {} percent",
                candidate_paths.len(), limits.max_deleted_paths, limits.max_deleted_percent
            )));
        }

        let existing_problems = local
            .link_problems
            .union(&remote.link_problems)
            .cloned()
            .collect::<BTreeSet<_>>();
        if let Some(problem) = merged.link_problems.difference(&existing_problems).next() {
            return Err(GitSyncObserverError::new(format!(
                "automatic merge introduces a new {} {} link-resolution problem in `{}` for target `{}`",
                problem.problem, problem.kind, problem.source_path, problem.target
            )));
        }
        Ok(())
    }
}

struct GitTreeAnalysis {
    paths: BTreeSet<String>,
    /// Markdown and Canvas blobs the analysis read.
    blobs: BTreeSet<vulcan_sync::GitOid>,
    link_problems: BTreeSet<LinkProblemKey>,
}

/// Blob analyses shared by the trees of one validation, backed by the
/// persistent [`MergeValidationStore`] when it is available.
#[derive(Default)]
struct GitTreeAnalysisCache {
    markdown: BTreeMap<vulcan_sync::GitOid, CachedMarkdown>,
    canvas: BTreeMap<vulcan_sync::GitOid, CachedCanvas>,
    store: Option<MergeValidationStore>,
}

/// What the merge gate needs from a Markdown blob. Blobs are immutable, so an
/// entry stays valid while the parser and the vault config are unchanged; see
/// [`merge_validation_fingerprint`].
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct CachedMarkdown {
    bytes: usize,
    aliases: Vec<String>,
    links: Vec<CachedLink>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct CachedLink {
    target: Option<String>,
    kind: CachedLinkKind,
}

#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum CachedLinkKind {
    Wikilink,
    Markdown,
    Embed,
    External,
}

impl From<vulcan_core::LinkKind> for CachedLinkKind {
    fn from(kind: vulcan_core::LinkKind) -> Self {
        match kind {
            vulcan_core::LinkKind::Wikilink => Self::Wikilink,
            vulcan_core::LinkKind::Markdown => Self::Markdown,
            vulcan_core::LinkKind::Embed => Self::Embed,
            vulcan_core::LinkKind::External => Self::External,
        }
    }
}

impl From<CachedLinkKind> for vulcan_core::LinkKind {
    fn from(kind: CachedLinkKind) -> Self {
        match kind {
            CachedLinkKind::Wikilink => Self::Wikilink,
            CachedLinkKind::Markdown => Self::Markdown,
            CachedLinkKind::Embed => Self::Embed,
            CachedLinkKind::External => Self::External,
        }
    }
}

impl CachedMarkdown {
    fn from_source(source: &str, config: &VaultConfig) -> Self {
        let parsed = parse_document(source, config);
        Self {
            bytes: source.len(),
            aliases: parsed.aliases,
            links: parsed
                .links
                .into_iter()
                .map(|link| CachedLink {
                    target: link.target_path_candidate,
                    kind: link.link_kind.into(),
                })
                .collect(),
        }
    }
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct CachedCanvas {
    bytes: usize,
    references: Vec<String>,
}

const MERGE_VALIDATION_STORE_VERSION: i64 = 1;
/// Rows beyond the blobs of the last validated trees that may accumulate
/// before old note versions are pruned.
const MERGE_VALIDATION_STORE_SLACK: usize = 1_024;
const MERGE_VALIDATION_LOOKUP_CHUNK: usize = 512;
const STORED_MARKDOWN: i64 = 0;
const STORED_CANVAS: i64 = 1;

/// Identifies everything besides blob content that shapes a stored analysis:
/// the store format, the parser, the build, and the vault configuration.
fn merge_validation_fingerprint(config: &VaultConfig) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&MERGE_VALIDATION_STORE_VERSION.to_le_bytes());
    hasher.update(&vulcan_core::PARSER_VERSION.to_le_bytes());
    hasher.update(env!("CARGO_PKG_VERSION").as_bytes());
    hasher.update(&serde_json::to_vec(config).unwrap_or_default());
    hasher.finalize().to_hex().to_string()
}

fn merge_validation_store_path(repository: &vulcan_sync::GitRepository) -> PathBuf {
    repository
        .git_dir
        .join("vulcan-sync/merge-validation-cache.sqlite")
}

/// Persistent analyses of Markdown and Canvas blobs keyed by blob ID, so a
/// merge parses only content it has not seen. Lookups are point queries for
/// the blobs a validation needs and new analyses are appended, so neither
/// reading nor writing scales with the size of the store. Every operation is
/// best effort: a store that cannot be used leaves validation uncached.
struct MergeValidationStore {
    connection: rusqlite::Connection,
}

impl MergeValidationStore {
    fn open(repository: &vulcan_sync::GitRepository, fingerprint: &str) -> Option<Self> {
        let path = merge_validation_store_path(repository);
        fs::create_dir_all(path.parent()?).ok()?;
        Self::open_at(&path, fingerprint).or_else(|| {
            // An unreadable or corrupt store is rebuilt from scratch once.
            let _ = fs::remove_file(&path);
            Self::open_at(&path, fingerprint)
        })
    }

    fn open_at(path: &Path, fingerprint: &str) -> Option<Self> {
        let connection = rusqlite::Connection::open(path).ok()?;
        connection.busy_timeout(Duration::from_secs(5)).ok()?;
        connection
            .execute_batch(
                "PRAGMA journal_mode = WAL;
                 PRAGMA synchronous = NORMAL;
                 CREATE TABLE IF NOT EXISTS meta (
                     key TEXT PRIMARY KEY,
                     value TEXT NOT NULL
                 ) WITHOUT ROWID;
                 CREATE TABLE IF NOT EXISTS blobs (
                     oid TEXT PRIMARY KEY,
                     kind INTEGER NOT NULL,
                     summary BLOB NOT NULL
                 ) WITHOUT ROWID;",
            )
            .ok()?;
        let stored = connection
            .query_row(
                "SELECT value FROM meta WHERE key = 'fingerprint'",
                [],
                |row| row.get::<_, String>(0),
            )
            .ok();
        if stored.as_deref() != Some(fingerprint) {
            connection.execute_batch("DELETE FROM blobs").ok()?;
            connection
                .execute(
                    "INSERT OR REPLACE INTO meta (key, value) VALUES ('fingerprint', ?1)",
                    [fingerprint],
                )
                .ok()?;
        }
        Some(Self { connection })
    }

    /// Adds every stored analysis among `oids` to `cache`.
    fn load_into(&self, oids: &[&vulcan_sync::GitOid], cache: &mut GitTreeAnalysisCache) {
        for chunk in oids.chunks(MERGE_VALIDATION_LOOKUP_CHUNK) {
            let placeholders = vec!["?"; chunk.len()].join(",");
            let Ok(mut statement) = self.connection.prepare(&format!(
                "SELECT oid, kind, summary FROM blobs WHERE oid IN ({placeholders})"
            )) else {
                return;
            };
            let Ok(rows) = statement.query_map(
                rusqlite::params_from_iter(chunk.iter().map(|oid| oid.as_str())),
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, Vec<u8>>(2)?,
                    ))
                },
            ) else {
                return;
            };
            for (oid, kind, summary) in rows.flatten() {
                let Ok(oid) = vulcan_sync::GitOid::parse(oid) else {
                    continue;
                };
                // A row that no longer decodes is treated as absent and
                // reparsed from the blob.
                if kind == STORED_MARKDOWN {
                    if let Ok(markdown) = serde_json::from_slice::<CachedMarkdown>(&summary) {
                        cache.markdown.insert(oid, markdown);
                    }
                } else if kind == STORED_CANVAS {
                    if let Ok(canvas) = serde_json::from_slice::<CachedCanvas>(&summary) {
                        cache.canvas.insert(oid, canvas);
                    }
                }
            }
        }
    }

    fn insert(&mut self, additions: &GitTreeAnalysisCache) {
        let Ok(transaction) = self.connection.transaction() else {
            return;
        };
        {
            let Ok(mut statement) = transaction
                .prepare("INSERT OR IGNORE INTO blobs (oid, kind, summary) VALUES (?1, ?2, ?3)")
            else {
                return;
            };
            let markdown = additions.markdown.iter().filter_map(|(oid, entry)| {
                Some((oid, STORED_MARKDOWN, serde_json::to_vec(entry).ok()?))
            });
            let canvas = additions.canvas.iter().filter_map(|(oid, entry)| {
                Some((oid, STORED_CANVAS, serde_json::to_vec(entry).ok()?))
            });
            for (oid, kind, summary) in markdown.chain(canvas) {
                if statement
                    .execute(rusqlite::params![oid.as_str(), kind, summary])
                    .is_err()
                {
                    return;
                }
            }
        }
        let _ = transaction.commit();
    }

    /// Drops analyses of blobs outside `referenced` once they outnumber it, so
    /// the store stays proportional to the vault rather than to its history.
    fn prune_unless(&self, referenced: &BTreeSet<&vulcan_sync::GitOid>) {
        let Ok(rows) = self
            .connection
            .query_row("SELECT count(*) FROM blobs", [], |row| row.get::<_, i64>(0))
        else {
            return;
        };
        let limit = referenced
            .len()
            .saturating_mul(2)
            .saturating_add(MERGE_VALIDATION_STORE_SLACK);
        if usize::try_from(rows).unwrap_or(usize::MAX) <= limit {
            return;
        }
        let _ = (|| -> rusqlite::Result<()> {
            self.connection.execute_batch(
                "CREATE TEMP TABLE IF NOT EXISTS referenced (oid TEXT PRIMARY KEY) WITHOUT ROWID;
                 DELETE FROM referenced;",
            )?;
            {
                let mut statement = self
                    .connection
                    .prepare("INSERT OR IGNORE INTO referenced (oid) VALUES (?1)")?;
                for oid in referenced {
                    statement.execute([oid.as_str()])?;
                }
            }
            self.connection.execute_batch(
                "DELETE FROM blobs WHERE oid NOT IN (SELECT oid FROM referenced);
                 DELETE FROM referenced;",
            )
        })();
    }
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct LinkProblemKey {
    source_path: String,
    target: String,
    kind: &'static str,
    problem: &'static str,
}

fn analyze_git_tree(
    engine: &dyn GitEngine,
    repository: &vulcan_sync::GitRepository,
    revision: &vulcan_sync::GitOid,
    config: &VaultConfig,
    cache: &mut GitTreeAnalysisCache,
) -> Result<GitTreeAnalysis, GitSyncObserverError> {
    let entries = engine
        .tree_entries(repository, revision)
        .map_err(|error| GitSyncObserverError::new(error.to_string()))?;
    let paths = entries
        .iter()
        .map(|entry| entry.path.clone())
        .collect::<BTreeSet<_>>();
    let markdown_entries = entries
        .iter()
        .filter(|entry| markdown_path(&entry.path))
        .collect::<Vec<_>>();
    if markdown_entries.len() > MAX_VALIDATED_MARKDOWN_FILES {
        return Err(GitSyncObserverError::new(format!(
            "automatic merge tree exceeds the {MAX_VALIDATED_MARKDOWN_FILES} Markdown-file validation limit"
        )));
    }
    let canvas_entries = entries
        .iter()
        .filter(|entry| canvas_path(&entry.path))
        .collect::<Vec<_>>();
    if canvas_entries.len() > MAX_VALIDATED_CANVAS_FILES {
        return Err(GitSyncObserverError::new(format!(
            "automatic merge tree exceeds the {MAX_VALIDATED_CANVAS_FILES} Canvas-file validation limit"
        )));
    }

    cache_tree_content(
        engine,
        repository,
        &markdown_entries,
        &canvas_entries,
        config,
        cache,
    )?;

    let blobs = markdown_entries
        .iter()
        .chain(&canvas_entries)
        .map(|entry| entry.oid.clone())
        .collect::<BTreeSet<_>>();
    let mut parsed_documents = Vec::with_capacity(markdown_entries.len());
    let mut total_bytes = 0_usize;
    for entry in markdown_entries {
        let cached = cache
            .markdown
            .get(&entry.oid)
            .expect("every requested Markdown blob was cached");
        total_bytes = total_bytes.saturating_add(cached.bytes);
        if total_bytes > MAX_VALIDATED_MARKDOWN_BYTES {
            return Err(GitSyncObserverError::new(format!(
                "automatic merge tree exceeds the {MAX_VALIDATED_MARKDOWN_BYTES}-byte Markdown validation limit"
            )));
        }
        parsed_documents.push((entry.path.clone(), cached));
    }

    let resolver_documents = parsed_documents
        .iter()
        .map(|(path, parsed)| ResolverDocument {
            id: path.clone(),
            path: path.clone(),
            filename: Path::new(path)
                .file_stem()
                .or_else(|| Path::new(path).file_name())
                .and_then(|name| name.to_str())
                .unwrap_or(path)
                .to_string(),
            aliases: parsed.aliases.clone(),
        })
        .collect::<Vec<_>>();
    let resolver = ResolverIndex::build(&resolver_documents);
    let mut link_problems = BTreeSet::new();
    resolve_document_links(&resolver, config, &parsed_documents, &mut link_problems);
    let mut canvas_references = Vec::new();
    let mut canvas_bytes = 0_usize;
    for entry in canvas_entries {
        let cached = cache
            .canvas
            .get(&entry.oid)
            .expect("every requested Canvas blob was cached");
        canvas_bytes = canvas_bytes.saturating_add(cached.bytes);
        if canvas_bytes > MAX_VALIDATED_CANVAS_BYTES {
            return Err(GitSyncObserverError::new(format!(
                "automatic merge tree exceeds the {MAX_VALIDATED_CANVAS_BYTES}-byte Canvas validation limit"
            )));
        }
        canvas_references.extend(
            cached
                .references
                .iter()
                .cloned()
                .map(|target| (entry.path.clone(), target)),
        );
    }
    resolve_canvas_links(&resolver, config, &canvas_references, &mut link_problems);
    Ok(GitTreeAnalysis {
        paths,
        blobs,
        link_problems,
    })
}

fn cache_tree_content(
    engine: &dyn GitEngine,
    repository: &vulcan_sync::GitRepository,
    markdown_entries: &[&vulcan_sync::GitTreeEntry],
    canvas_entries: &[&vulcan_sync::GitTreeEntry],
    config: &VaultConfig,
    cache: &mut GitTreeAnalysisCache,
) -> Result<(), GitSyncObserverError> {
    for entry in markdown_entries.iter().chain(canvas_entries) {
        if entry.kind != "blob" {
            let label = if markdown_path(&entry.path) {
                "Markdown"
            } else {
                "Canvas"
            };
            return Err(GitSyncObserverError::new(format!(
                "{label} path `{}` is not a regular Git blob",
                entry.path
            )));
        }
    }
    let markdown = markdown_entries
        .iter()
        .filter(|entry| !cache.markdown.contains_key(&entry.oid))
        .map(|entry| (entry.oid.clone(), *entry))
        .collect::<BTreeMap<_, _>>();
    let canvas = canvas_entries
        .iter()
        .filter(|entry| !cache.canvas.contains_key(&entry.oid))
        .map(|entry| (entry.oid.clone(), *entry))
        .collect::<BTreeMap<_, _>>();
    if let Some(store) = cache.store.take() {
        let wanted = markdown.keys().chain(canvas.keys()).collect::<Vec<_>>();
        store.load_into(&wanted, cache);
        cache.store = Some(store);
    }
    let markdown = markdown
        .into_iter()
        .filter(|(oid, _)| !cache.markdown.contains_key(oid))
        .collect::<BTreeMap<_, _>>();
    let canvas = canvas
        .into_iter()
        .filter(|(oid, _)| !cache.canvas.contains_key(oid))
        .collect::<BTreeMap<_, _>>();
    let mut missing = markdown
        .keys()
        .chain(canvas.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    if missing.is_empty() {
        return Ok(());
    }
    let objects = missing.iter().cloned().collect::<Vec<_>>();
    // Parsing may succeed for a prefix before the stream or Git process fails.
    // Publish projections atomically so a retry cannot skip unvalidated input.
    let mut additions = GitTreeAnalysisCache::default();
    engine
        .visit_blobs(repository, &objects, &mut |oid, data| {
            let invalid = |detail| vulcan_sync::GitEngineError::InvalidOutput {
                operation: "validate Git tree content",
                detail,
            };
            if !missing.remove(oid) {
                return Err(invalid(format!("unexpected or duplicate blob `{oid}`")));
            }
            if let Some(entry) = markdown.get(oid) {
                let source = std::str::from_utf8(&data).map_err(|_| {
                    invalid(format!("Markdown path `{}` is not valid UTF-8", entry.path))
                })?;
                additions
                    .markdown
                    .insert(oid.clone(), CachedMarkdown::from_source(source, config));
            }
            if let Some(entry) = canvas.get(oid) {
                additions.canvas.insert(
                    oid.clone(),
                    CachedCanvas {
                        bytes: data.len(),
                        references: parse_canvas_file_references(&entry.path, &data)
                            .map_err(|error| invalid(error.to_string()))?,
                    },
                );
            }
            Ok(())
        })
        .map_err(|error| GitSyncObserverError::new(error.to_string()))?;
    if let Some(oid) = missing.first() {
        return Err(GitSyncObserverError::new(format!(
            "blob `{oid}` has no data"
        )));
    }
    if let Some(store) = cache.store.as_mut() {
        store.insert(&additions);
    }
    cache.markdown.extend(additions.markdown);
    cache.canvas.extend(additions.canvas);
    Ok(())
}

fn resolve_document_links(
    resolver: &ResolverIndex,
    config: &VaultConfig,
    parsed_documents: &[(String, &CachedMarkdown)],
    link_problems: &mut BTreeSet<LinkProblemKey>,
) {
    for (path, parsed) in parsed_documents {
        for link in &parsed.links {
            let link_kind = vulcan_core::LinkKind::from(link.kind);
            let resolution = resolver.resolve(
                &ResolverLink {
                    source_document_id: path.clone(),
                    source_path: path.clone(),
                    target_path_candidate: link.target.clone(),
                    link_kind,
                },
                config.link_resolution,
            );
            let Some(problem) = resolution.problem else {
                continue;
            };
            link_problems.insert(LinkProblemKey {
                source_path: path.clone(),
                target: link.target.clone().unwrap_or_default(),
                kind: match link_kind {
                    vulcan_core::LinkKind::Wikilink => "wikilink",
                    vulcan_core::LinkKind::Markdown => "markdown",
                    vulcan_core::LinkKind::Embed => "embed",
                    vulcan_core::LinkKind::External => "external",
                },
                problem: match problem {
                    LinkResolutionProblem::Unresolved => "unresolved",
                    LinkResolutionProblem::Ambiguous(_) => "ambiguous",
                    LinkResolutionProblem::OutsideVault => "outside_vault",
                },
            });
        }
    }
}

fn resolve_canvas_links(
    resolver: &ResolverIndex,
    config: &VaultConfig,
    references: &[(String, String)],
    link_problems: &mut BTreeSet<LinkProblemKey>,
) {
    for (path, target) in references {
        let resolution = resolver.resolve(
            &ResolverLink {
                source_document_id: path.clone(),
                source_path: path.clone(),
                target_path_candidate: Some(target.clone()),
                link_kind: vulcan_core::LinkKind::Wikilink,
            },
            config.link_resolution,
        );
        let Some(problem) = resolution.problem else {
            continue;
        };
        link_problems.insert(LinkProblemKey {
            source_path: path.clone(),
            target: target.clone(),
            kind: "canvas",
            problem: match problem {
                LinkResolutionProblem::Unresolved => "unresolved",
                LinkResolutionProblem::Ambiguous(_) => "ambiguous",
                LinkResolutionProblem::OutsideVault => "outside_vault",
            },
        });
    }
}

fn markdown_path(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(extension.to_ascii_lowercase().as_str(), "md" | "markdown")
        })
}

fn canvas_path(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("canvas"))
}

/// Collects `file` references from Canvas node objects so whole-tree link
/// validation covers embedded note references alongside Markdown links.
fn parse_canvas_file_references(
    path: &str,
    data: &[u8],
) -> Result<Vec<String>, GitSyncObserverError> {
    let mut references = Vec::new();
    let source = std::str::from_utf8(data).map_err(|_| {
        GitSyncObserverError::new(format!("Canvas path `{path}` is not valid UTF-8"))
    })?;
    let canvas: serde_json::Value = serde_json::from_str(source).map_err(|error| {
        GitSyncObserverError::new(format!("Canvas path `{path}` is not valid JSON: {error}"))
    })?;
    let nodes = canvas
        .get("nodes")
        .and_then(|nodes| nodes.as_array())
        .ok_or_else(|| {
            GitSyncObserverError::new(format!("Canvas path `{path}` has no nodes array"))
        })?;
    for node in nodes {
        if let Some(file) = node.get("file").and_then(|file| file.as_str()) {
            references.push(file.to_string());
        }
    }
    Ok(references)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use std::path::Path;
    use std::process::{Command, Stdio};
    use tempfile::tempdir;
    use vulcan_core::{initialize_vulcan_dir, properties::load_note_index, scan_vault, ScanMode};
    use vulcan_sync::{GitConflictScope, MergeAutomation, MergeResolution};

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

    #[test]
    fn serialized_vault_sync_conflict_records_are_bounded_summaries() {
        #[derive(Serialize)]
        struct Wrapper {
            #[serde(serialize_with = "serialize_sync_report_conflict_record")]
            conflict_record: Option<SyncConflictRecord>,
        }

        let absent_side = || crate::sync_conflicts::SyncConflictSideRecord {
            revision: "revision".to_string(),
            object_id: None,
            mode: None,
            kind: None,
            artifact: None,
            content_hash: None,
            bytes: None,
        };
        let paths = (0..10_000)
            .map(|index| crate::sync_conflicts::SyncConflictPathRecord {
                path: format!("notes/{index:05}.md"),
                group_id: format!("group-{index:05}"),
                group_kind: crate::sync_conflicts::SyncConflictGroupKind::Path,
                classification: None,
                base: absent_side(),
                local: absent_side(),
                remote: absent_side(),
            })
            .collect();
        let record = SyncConflictRecord {
            version: crate::sync_conflicts::SYNC_CONFLICT_RECORD_VERSION,
            id: "conflict-1".to_string(),
            repository_key: "repository".to_string(),
            work_tree: Path::new("/vault").to_path_buf(),
            base_revision: Some("base".to_string()),
            local_revision: "local".to_string(),
            remote_revision: "remote".to_string(),
            scope: GitConflictScope::Paths,
            policy_version: 1,
            policy_hash: "policy".to_string(),
            preserved_base_ref: None,
            preserved_local_ref: "refs/local".to_string(),
            preserved_remote_ref: "refs/remote".to_string(),
            preserved_record_ref: Some("refs/record".to_string()),
            provenance_revision: Some("provenance".to_string()),
            projection: None,
            paths,
            diagnostics: "d".repeat(10_000),
            carried_from: None,
        };

        let encoded = serde_json::to_vec(&Wrapper {
            conflict_record: Some(record),
        })
        .expect("serialize summary");
        let value: serde_json::Value = serde_json::from_slice(&encoded).expect("JSON");
        let summary = &value["conflict_record"];
        assert_eq!(summary["path_count"], 10_000);
        assert_eq!(summary["group_count"], 10_000);
        assert_eq!(summary["paths"].as_array().expect("paths").len(), 16);
        assert_eq!(summary["paths_complete"], false);
        assert_eq!(
            summary["diagnostics"].as_str().expect("diagnostics").len(),
            4096
        );
        assert_eq!(summary["diagnostics_complete"], false);
        assert_eq!(summary["detail_conflict_id"], "conflict-1");
        assert!(
            encoded.len() < 16 * 1024,
            "summary was {} bytes",
            encoded.len()
        );
    }

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
            "sync replicates the whole repository work tree"
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
        assert_eq!(records.len(), 1);
        assert_eq!(&records[0], record);
    }

    fn assert_overlapping_text_classification(record: &SyncConflictRecord) {
        let classification = record.paths[0]
            .classification
            .as_ref()
            .expect("structured conflict classification");
        assert_eq!(
            classification.class,
            vulcan_sync::GitConflictClass::OverlappingText
        );
        assert_eq!(
            classification.diagnostic_code,
            "sync.conflict.overlapping-text"
        );
        assert_eq!(
            classification.effective_resolution,
            vulcan_sync::MergeResolution::RequireReview
        );
    }

    fn assert_projection_candidate(record: &SyncConflictRecord) {
        let projection = record.projection.as_ref().expect("projection candidate");
        assert!(projection.published);
        assert!(projection.applied);
    }

    fn assert_projected_worktree(reader: &Path) {
        assert_eq!(
            fs::read_to_string(reader.join("Home.md")).expect("accepted remote bytes"),
            "writer\n"
        );
        assert!(!reader.join(".sync-conflicts").exists());
    }

    fn assert_conflict_artifacts(store: &SyncStateStore, record: &SyncConflictRecord) {
        let root = store
            .root()
            .join(&record.repository_key)
            .join("conflicts")
            .join(&record.id);
        let read = |artifact: &Option<std::path::PathBuf>| {
            fs::read(root.join(artifact.as_ref().expect("artifact path"))).expect("artifact bytes")
        };
        assert_eq!(read(&record.paths[0].base.artifact), b"base\n");
        assert_eq!(read(&record.paths[0].local.artifact), b"reader\n");
        assert_eq!(read(&record.paths[0].remote.artifact), b"writer\n");
    }

    fn assert_conflict_operational_stats(report: &VaultSyncReport) {
        assert_eq!(report.operational_stats.version, 1);
        assert_eq!(report.operational_stats.automatic_resolution_paths, 0);
        assert_eq!(report.operational_stats.conflict_paths, 1);
        assert_eq!(report.operational_stats.conflict_groups, 1);
        assert_eq!(report.operational_stats.formatting_candidate_paths, 0);
        assert_eq!(report.operational_stats.preserved_input_bytes, 19);
        assert!(report
            .operational_stats
            .git_subprocesses
            .is_some_and(|count| count > 0));
        assert_eq!(report.operational_stats.cache_refresh_ms, 0);
        let serialized = serde_json::to_string(&report.operational_stats).expect("serialize stats");
        for private_value in ["Home.md", "writer", "reader"] {
            assert!(!serialized.contains(private_value));
        }
    }

    #[test]
    fn configured_sync_options_load_policy_and_only_reduce_automation() {
        let temporary = tempdir().expect("temporary directory");
        fs::create_dir(temporary.path().join(".vulcan")).expect("Vulcan directory");
        fs::write(
            temporary.path().join(".vulcan/config.toml"),
            r#"[sync.merge_policy]
version = 1
rules = [{ id = "review-all", selector = { glob = "**", kinds = [] }, resolution = "require_review" }]
"#,
        )
        .expect("shared policy");
        fs::write(
            temporary.path().join(".vulcan/config.local.toml"),
            "[sync]\nmerge_automation = \"require_review\"\n",
        )
        .expect("local ceiling");
        let paths = VaultPaths::new(temporary.path());

        let configured = configured_git_sync_options(&paths, &GitSyncOptions::default())
            .expect("configured options");

        assert_eq!(configured.merge_policy.rules[0].id, "review-all");
        assert_eq!(
            configured.merge_policy.rules[0].resolution,
            MergeResolution::RequireReview
        );
        assert_eq!(configured.merge_automation, MergeAutomation::RequireReview);

        fs::write(
            temporary.path().join(".vulcan/config.local.toml"),
            "[sync]\nmerge_automation = \"allow_policy\"\n",
        )
        .expect("permissive local ceiling");
        let configured = configured_git_sync_options(
            &paths,
            &GitSyncOptions {
                merge_automation: MergeAutomation::RequireReview,
                ..GitSyncOptions::default()
            },
        )
        .expect("caller ceiling");
        assert_eq!(configured.merge_automation, MergeAutomation::RequireReview);
    }

    #[test]
    fn malformed_vault_configuration_blocks_sync_before_transaction_state() {
        let temporary = tempdir().expect("temporary directory");
        fs::create_dir(temporary.path().join(".vulcan")).expect("Vulcan directory");
        fs::write(
            temporary.path().join(".vulcan/config.toml"),
            "[sync.merge_policy\nversion = 1\n",
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
            "[sync\nmerge_automation = \"require_review\"\n",
        )
        .expect("malformed local config");
        assert!(configured_git_sync_options(
            &VaultPaths::new(temporary.path()),
            &GitSyncOptions::default()
        )
        .is_err());

        fs::remove_file(temporary.path().join(".vulcan/config.local.toml"))
            .expect("remove malformed local config");
        fs::write(
            temporary.path().join(".vulcan/config.toml"),
            "[sync.tree_validation]\nmax_deleted_percent = 101\n",
        )
        .expect("invalid validation config");
        assert!(configured_git_sync_options(
            &VaultPaths::new(temporary.path()),
            &GitSyncOptions::default()
        )
        .is_err());

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
    fn automatic_merge_with_new_link_ambiguity_is_preserved_as_a_conflict() {
        let fixture = structured_sync_fixture(&[
            ("Home.md", "[[Target]]\n"),
            ("data.json", "{\"base\":true}\n"),
        ]);
        fs::create_dir(fixture.writer.join("Writer")).expect("writer folder");
        fs::write(fixture.writer.join("Writer/Target.md"), "writer target\n")
            .expect("writer target");
        fs::write(
            fixture.writer.join("data.json"),
            "{\"base\":true,\"writer\":1}\n",
        )
        .expect("writer JSON");
        fs::create_dir(fixture.reader.join("Reader")).expect("reader folder");
        fs::write(fixture.reader.join("Reader/Target.md"), "reader target\n")
            .expect("reader target");
        fs::write(
            fixture.reader.join("data.json"),
            "{\"base\":true,\"reader\":2}\n",
        )
        .expect("reader JSON");
        sync_git_vault_with_state_store(
            &VaultPaths::new(&fixture.writer),
            &GitSyncOptions::default(),
            &fixture.store,
        )
        .expect("writer push");

        let report = sync_git_vault_with_state_store(
            &VaultPaths::new(&fixture.reader),
            &GitSyncOptions::default(),
            &fixture.store,
        )
        .expect("validation conflict");

        assert_eq!(report.sync.outcome, GitSyncOutcome::Conflicted);
        assert!(report.sync.automatic_resolutions.is_empty());
        assert!(report
            .sync
            .conflict
            .as_ref()
            .expect("conflict")
            .diagnostics
            .contains("introduces a new ambiguous wikilink link-resolution problem"));
        assert!(fixture.reader.join("Writer/Target.md").exists());
        assert!(fixture.reader.join("Reader/Target.md").exists());
        let projection = report
            .conflict_record
            .as_ref()
            .and_then(|record| record.projection.as_ref())
            .expect("safe projection");
        assert!(projection.published);
        assert!(projection.applied);
    }

    /// Writer and reader edit `Home.md` from `base`, the reader syncs into a
    /// conflict, then the writer replaces its version with `writer_later`
    /// and the reader syncs again. Returns the first conflict and the report.
    fn overtaken_conflict(
        base: &str,
        writer_first: &str,
        reader: &str,
        writer_later: &str,
    ) -> (StructuredSyncFixture, SyncConflictRecord, VaultSyncReport) {
        let fixture = structured_sync_fixture(&[("Home.md", base)]);
        fs::write(fixture.writer.join("Home.md"), writer_first).expect("writer edit");
        fs::write(fixture.reader.join("Home.md"), reader).expect("reader edit");
        sync_git_vault_with_state_store(
            &VaultPaths::new(&fixture.writer),
            &GitSyncOptions::default(),
            &fixture.store,
        )
        .expect("writer push");
        let first = sync_git_vault_with_state_store(
            &VaultPaths::new(&fixture.reader),
            &GitSyncOptions::default(),
            &fixture.store,
        )
        .expect("first conflict")
        .conflict_record
        .expect("first conflict record");
        sync_git_vault_with_state_store(
            &VaultPaths::new(&fixture.writer),
            &GitSyncOptions::default(),
            &fixture.store,
        )
        .expect("writer observes the projection");
        fs::write(fixture.writer.join("Home.md"), writer_later).expect("writer advances");
        sync_git_vault_with_state_store(
            &VaultPaths::new(&fixture.writer),
            &GitSyncOptions::default(),
            &fixture.store,
        )
        .expect("writer advances live ref");
        let later = sync_git_vault_with_state_store(
            &VaultPaths::new(&fixture.reader),
            &GitSyncOptions::default(),
            &fixture.store,
        )
        .expect("later successful sync");
        assert_ne!(later.sync.outcome, GitSyncOutcome::Conflicted);
        (fixture, first, later)
    }

    fn conflict_listing(
        fixture: &StructuredSyncFixture,
    ) -> crate::sync_conflicts::SyncConflictListReport {
        crate::sync_conflicts::list_sync_conflicts_with_state_store(
            &VaultPaths::new(&fixture.reader),
            &fixture.store,
        )
        .expect("conflicts")
    }

    fn historical_conflict(
        fixture: &StructuredSyncFixture,
        id: &str,
    ) -> crate::sync_conflicts::SyncConflictDetailReport {
        crate::sync_conflicts::get_sync_conflict_with_state_store(
            &VaultPaths::new(&fixture.reader),
            id,
            &fixture.store,
        )
        .expect("conflict evidence is retained")
    }

    #[test]
    fn overtaken_conflict_that_still_overlaps_moves_to_a_resolvable_replacement() {
        let (fixture, first, _) =
            overtaken_conflict("base\n", "writer one\n", "reader\n", "writer two\n");

        let listed = conflict_listing(&fixture);
        assert_eq!(listed.count, 1);
        assert_eq!(listed.superseded_count, 1);
        let historical = historical_conflict(&fixture, &first.id);
        assert_eq!(
            historical.resolution,
            crate::sync_conflicts::SyncConflictResolutionState::Superseded
        );
        let replacement_id = historical
            .supersession
            .expect("supersession metadata")
            .replacement_conflict_id
            .expect("replacement conflict");
        let replacement = historical_conflict(&fixture, &replacement_id);
        assert_eq!(
            replacement.record.carried_from.as_deref(),
            Some(first.id.as_str())
        );
        assert_eq!(replacement.record.paths.len(), 1);
        let path = &replacement.record.paths[0];
        assert_eq!(path.path, "Home.md");
        // The replacement compares this device's version with what is live now.
        assert_eq!(path.local.bytes, Some("reader\n".len() as u64));
        assert_eq!(path.remote.bytes, Some("writer two\n".len() as u64));

        let resolved = crate::sync_conflicts::resolve_sync_conflict_with_state_store(
            &VaultPaths::new(&fixture.reader),
            &replacement_id,
            &crate::sync_conflicts::ResolveSyncConflictOptions {
                side: crate::sync_conflicts::SyncConflictResolutionSide::Local,
                group_ids: Vec::new(),
                remote: vulcan_sync::GitRemote::parse("origin").expect("remote"),
                live_ref: vulcan_sync::GitRefName::parse("refs/heads/__vulcan-sync/live")
                    .expect("live ref"),
                dry_run: false,
            },
            &fixture.store,
        )
        .expect("replacement resolves against the live version");
        assert_eq!(resolved.remaining_groups.unwrap_or_default(), 0);
        sync_git_vault_with_state_store(
            &VaultPaths::new(&fixture.writer),
            &GitSyncOptions::default(),
            &fixture.store,
        )
        .expect("writer receives the resolution");
        assert_eq!(
            fs::read_to_string(fixture.writer.join("Home.md")).expect("published"),
            "reader\n"
        );
        assert_eq!(conflict_listing(&fixture).count, 0);
    }

    #[test]
    fn replacement_conflict_accepts_reviewed_file_content() {
        let (fixture, first, _) =
            overtaken_conflict("base\n", "writer one\n", "reader\n", "writer two\n");
        let replacement_id = historical_conflict(&fixture, &first.id)
            .supersession
            .and_then(|supersession| supersession.replacement_conflict_id)
            .expect("replacement conflict");
        let approval_options = crate::sync_proposals::ApproveResolutionProposalOptions {
            remote: vulcan_sync::GitRemote::parse("origin").expect("remote"),
            live_ref: vulcan_sync::GitRefName::parse("refs/heads/__vulcan-sync/live")
                .expect("live ref"),
            dry_run: false,
            automatic: false,
        };
        let proposal = crate::sync_proposals::create_supplied_resolution_proposal_with_state_store(
            &VaultPaths::new(&fixture.reader),
            &replacement_id,
            &crate::sync_proposals::ResolutionProposalOptions {
                permission_profile: "unrestricted".to_string(),
                focused_context: Vec::new(),
                allow_broad_context: false,
                // No explicit selection: a replacement covers all its groups.
                group_ids: Vec::new(),
            },
            &approval_options,
            vec![crate::sync_proposals::ResolutionAgentPathOutput {
                path: "Home.md".to_string(),
                content: b"reader and writer two\n".to_vec(),
            }],
            &SyncCancellationToken::default(),
            &fixture.store,
        )
        .expect("reviewed proposal on the replacement");
        crate::sync_proposals::approve_resolution_proposal_with_state_store(
            &VaultPaths::new(&fixture.reader),
            &replacement_id,
            &proposal.proposal_id,
            &approval_options,
            &SyncCancellationToken::default(),
            &fixture.store,
        )
        .expect("reviewed approval");
        assert_eq!(
            fs::read_to_string(fixture.reader.join("Home.md")).expect("reviewed"),
            "reader and writer two\n"
        );
        assert_eq!(conflict_listing(&fixture).count, 0);
    }

    #[test]
    fn overtaken_conflict_that_now_merges_cleanly_is_published_automatically() {
        let (fixture, first, later) = overtaken_conflict(
            "a\nb\nc\n",
            "writer\nb\nc\n",
            "reader\nb\nc\n",
            "a\nb\nwriter later\n",
        );

        assert!(later.sync.accepted.is_some());
        let listed = conflict_listing(&fixture);
        assert_eq!(listed.count, 0);
        assert_eq!(listed.superseded_count, 1);
        let supersession = historical_conflict(&fixture, &first.id)
            .supersession
            .expect("supersession metadata");
        assert_eq!(supersession.replacement_conflict_id, None);
        // The reader's change is merged and published in the same sync.
        let merged = "reader\nb\nwriter later\n";
        assert_eq!(
            fs::read_to_string(fixture.reader.join("Home.md")).expect("merged locally"),
            merged
        );
        sync_git_vault_with_state_store(
            &VaultPaths::new(&fixture.writer),
            &GitSyncOptions::default(),
            &fixture.store,
        )
        .expect("writer receives the merge");
        assert_eq!(
            fs::read_to_string(fixture.writer.join("Home.md")).expect("published"),
            merged
        );
    }

    #[test]
    fn overtaken_conflict_is_carried_forward_by_a_sync_that_conflicts_elsewhere() {
        let fixture = structured_sync_fixture(&[("A.md", "base a\n"), ("B.md", "base b\n")]);
        let sync = |root: &std::path::Path| {
            sync_git_vault_with_state_store(
                &VaultPaths::new(root),
                &GitSyncOptions::default(),
                &fixture.store,
            )
            .expect("sync")
        };
        fs::write(fixture.writer.join("A.md"), "remote a\n").expect("remote A");
        fs::write(fixture.reader.join("A.md"), "local a\n").expect("local A");
        sync(&fixture.writer);
        let first = sync(&fixture.reader)
            .conflict_record
            .expect("conflict on A");

        // The writer changes A again and, in the same push, B; the reader also
        // changed B, so its next sync records a new conflict on B only.
        sync(&fixture.writer);
        fs::write(fixture.writer.join("A.md"), "remote a two\n").expect("writer advances A");
        fs::write(fixture.writer.join("B.md"), "remote b\n").expect("remote B");
        fs::write(fixture.reader.join("B.md"), "local b\n").expect("local B");
        sync(&fixture.writer);
        let later = sync(&fixture.reader);
        assert_eq!(later.sync.outcome, GitSyncOutcome::Conflicted);
        let second = later.conflict_record.expect("conflict on B");
        assert_eq!(
            second
                .paths
                .iter()
                .map(|path| path.path.as_str())
                .collect::<Vec<_>>(),
            ["B.md"]
        );

        let replacement_id = historical_conflict(&fixture, &first.id)
            .supersession
            .and_then(|supersession| supersession.replacement_conflict_id)
            .expect("the overtaken A conflict was carried forward");
        assert_ne!(replacement_id, second.id);
        let replacement = historical_conflict(&fixture, &replacement_id);
        assert_eq!(replacement.record.paths[0].path, "A.md");
        assert_eq!(
            replacement.record.paths[0].remote.bytes,
            Some("remote a two\n".len() as u64)
        );
        assert_eq!(conflict_listing(&fixture).count, 2);
    }

    #[test]
    fn overtaken_rename_conflict_moves_to_a_replacement_as_one_group() {
        let fixture = structured_sync_fixture(&[("Note.md", "shared body\n")]);
        let sync = |root: &std::path::Path| {
            sync_git_vault_with_state_store(
                &VaultPaths::new(root),
                &GitSyncOptions::default(),
                &fixture.store,
            )
            .expect("sync")
        };
        fs::rename(
            fixture.writer.join("Note.md"),
            fixture.writer.join("Writer.md"),
        )
        .expect("writer rename");
        fs::rename(
            fixture.reader.join("Note.md"),
            fixture.reader.join("Reader.md"),
        )
        .expect("reader rename");
        sync(&fixture.writer);
        let first = sync(&fixture.reader)
            .conflict_record
            .expect("rename conflict");
        assert!(conflict_groups(&first)
            .iter()
            .all(|group| group.kind == crate::sync_conflicts::SyncConflictGroupKind::Structural));

        // The writer recreates the original path, overtaking the conflict.
        sync(&fixture.writer);
        fs::write(fixture.writer.join("Note.md"), "recreated\n").expect("writer recreates");
        sync(&fixture.writer);
        let later = sync(&fixture.reader);
        assert_ne!(later.sync.outcome, GitSyncOutcome::Conflicted);

        let replacement_id = historical_conflict(&fixture, &first.id)
            .supersession
            .and_then(|supersession| supersession.replacement_conflict_id)
            .expect("the structural conflict was carried forward");
        let replacement = historical_conflict(&fixture, &replacement_id).record;
        let groups = conflict_groups(&replacement);
        assert_eq!(groups.len(), 1);
        assert_eq!(
            groups[0].kind,
            crate::sync_conflicts::SyncConflictGroupKind::Structural
        );
        assert_eq!(groups[0].paths, ["Note.md"]);

        crate::sync_conflicts::resolve_sync_conflict_with_state_store(
            &VaultPaths::new(&fixture.reader),
            &replacement_id,
            &crate::sync_conflicts::ResolveSyncConflictOptions {
                side: crate::sync_conflicts::SyncConflictResolutionSide::Local,
                group_ids: Vec::new(),
                remote: vulcan_sync::GitRemote::parse("origin").expect("remote"),
                live_ref: vulcan_sync::GitRefName::parse("refs/heads/__vulcan-sync/live")
                    .expect("live ref"),
                dry_run: false,
            },
            &fixture.store,
        )
        .expect("structural replacement resolves");
        assert!(!fixture.reader.join("Note.md").exists());
        assert!(fixture.reader.join("Reader.md").exists());
        assert_eq!(conflict_listing(&fixture).count, 0);
    }

    #[test]
    fn overtaken_conflict_already_matching_the_live_version_is_retired() {
        let (fixture, first, _) =
            overtaken_conflict("base\n", "writer one\n", "reader\n", "reader\n");

        let listed = conflict_listing(&fixture);
        assert_eq!(listed.count, 0);
        assert_eq!(listed.superseded_count, 1);
        let supersession = historical_conflict(&fixture, &first.id)
            .supersession
            .expect("supersession metadata");
        assert_eq!(supersession.replacement_conflict_id, None);
        assert_eq!(
            fs::read_to_string(fixture.reader.join("Home.md")).expect("live version"),
            "reader\n"
        );
    }

    #[test]
    fn partially_overtaken_conflict_moves_its_remaining_groups_to_one_replacement() {
        let fixture = structured_sync_fixture(&[("A.md", "base a\n"), ("B.md", "base b\n")]);
        fs::write(fixture.writer.join("A.md"), "remote a\n").expect("remote A");
        fs::write(fixture.writer.join("B.md"), "remote b\n").expect("remote B");
        fs::write(fixture.reader.join("A.md"), "local a\n").expect("local A");
        fs::write(fixture.reader.join("B.md"), "local b\n").expect("local B");
        sync_git_vault_with_state_store(
            &VaultPaths::new(&fixture.writer),
            &GitSyncOptions::default(),
            &fixture.store,
        )
        .expect("writer conflict inputs");
        let conflict = sync_git_vault_with_state_store(
            &VaultPaths::new(&fixture.reader),
            &GitSyncOptions::default(),
            &fixture.store,
        )
        .expect("reader conflict")
        .conflict_record
        .expect("durable conflict");

        // Only A changes again on the accepted frontier; B is untouched.
        sync_git_vault_with_state_store(
            &VaultPaths::new(&fixture.writer),
            &GitSyncOptions::default(),
            &fixture.store,
        )
        .expect("writer observes the projection");
        fs::write(fixture.writer.join("A.md"), "remote a two\n").expect("writer advances A");
        sync_git_vault_with_state_store(
            &VaultPaths::new(&fixture.writer),
            &GitSyncOptions::default(),
            &fixture.store,
        )
        .expect("writer advances live ref");

        let side_options =
            |group_ids: Vec<String>| crate::sync_conflicts::ResolveSyncConflictOptions {
                side: crate::sync_conflicts::SyncConflictResolutionSide::Local,
                group_ids,
                remote: vulcan_sync::GitRemote::parse("origin").expect("remote"),
                live_ref: vulcan_sync::GitRefName::parse("refs/heads/__vulcan-sync/live")
                    .expect("live ref"),
                dry_run: true,
            };
        let group_a = conflict
            .paths
            .iter()
            .find(|item| item.path == "A.md")
            .expect("A conflict path")
            .group_id
            .clone();
        // Before this device syncs, the refusal names the way forward.
        let stale = crate::sync_conflicts::resolve_sync_conflict_with_state_store(
            &VaultPaths::new(&fixture.reader),
            &conflict.id,
            &side_options(vec![group_a]),
            &fixture.store,
        )
        .expect_err("a changed group cannot use its out-of-date sides");
        assert!(stale.to_string().contains("vulcan sync run"), "{stale}");

        let later = sync_git_vault_with_state_store(
            &VaultPaths::new(&fixture.reader),
            &GitSyncOptions::default(),
            &fixture.store,
        )
        .expect("later successful sync");
        assert_ne!(later.sync.outcome, GitSyncOutcome::Conflicted);
        let listed = conflict_listing(&fixture);
        assert_eq!(listed.count, 1);
        assert_eq!(listed.superseded_count, 1);
        let replacement_id = historical_conflict(&fixture, &conflict.id)
            .supersession
            .and_then(|supersession| supersession.replacement_conflict_id)
            .expect("replacement conflict");
        let replacement = historical_conflict(&fixture, &replacement_id);
        let mut paths = replacement
            .record
            .paths
            .iter()
            .map(|path| path.path.as_str())
            .collect::<Vec<_>>();
        paths.sort_unstable();
        assert_eq!(paths, ["A.md", "B.md"]);

        let mut apply = side_options(Vec::new());
        apply.dry_run = false;
        crate::sync_conflicts::resolve_sync_conflict_with_state_store(
            &VaultPaths::new(&fixture.reader),
            &replacement_id,
            &apply,
            &fixture.store,
        )
        .expect("replacement resolves every remaining group");
        for (path, content) in [("A.md", "local a\n"), ("B.md", "local b\n")] {
            assert_eq!(
                fs::read_to_string(fixture.reader.join(path)).expect("resolved file"),
                content
            );
        }
        assert_eq!(conflict_listing(&fixture).count, 0);
    }

    #[test]
    fn whole_side_resolution_survives_unrelated_live_advancement() {
        let fixture = structured_sync_fixture(&[("A.md", "base a\n"), ("B.md", "base b\n")]);
        fs::write(fixture.writer.join("A.md"), "remote a\n").expect("remote A");
        fs::write(fixture.writer.join("B.md"), "remote b\n").expect("remote B");
        fs::write(fixture.reader.join("A.md"), "local a\n").expect("local A");
        fs::write(fixture.reader.join("B.md"), "local b\n").expect("local B");
        sync_git_vault_with_state_store(
            &VaultPaths::new(&fixture.writer),
            &GitSyncOptions::default(),
            &fixture.store,
        )
        .expect("writer conflict inputs");
        let conflict = sync_git_vault_with_state_store(
            &VaultPaths::new(&fixture.reader),
            &GitSyncOptions::default(),
            &fixture.store,
        )
        .expect("reader conflict")
        .conflict_record
        .expect("durable conflict");

        // Another device moves the live ref without touching the conflict.
        sync_git_vault_with_state_store(
            &VaultPaths::new(&fixture.writer),
            &GitSyncOptions::default(),
            &fixture.store,
        )
        .expect("writer observes the conflict projection");
        fs::write(fixture.writer.join("Unrelated.md"), "later\n").expect("unrelated edit");
        sync_git_vault_with_state_store(
            &VaultPaths::new(&fixture.writer),
            &GitSyncOptions::default(),
            &fixture.store,
        )
        .expect("unrelated live advancement");
        let later = sync_git_vault_with_state_store(
            &VaultPaths::new(&fixture.reader),
            &GitSyncOptions::default(),
            &fixture.store,
        )
        .expect("reader receives the unrelated edit");
        assert_ne!(later.sync.outcome, GitSyncOutcome::Conflicted);
        // An earlier attempt prepared a whole resolution that never
        // published, as when another device won the race; it must not
        // pin the choice to the old live version.
        crate::sync_conflicts::SyncConflictStore::from_state_store(&fixture.store)
            .save_resolution(
                &conflict.repository_key,
                &crate::sync_conflicts::SyncConflictResolutionRecord {
                    version: crate::sync_conflicts::SYNC_CONFLICT_RESOLUTION_VERSION,
                    conflict_id: conflict.id.clone(),
                    side: Some(crate::sync_conflicts::SyncConflictResolutionSide::Local),
                    proposal_id: None,
                    base_revision: conflict.base_revision.clone().expect("base"),
                    local_revision: conflict.local_revision.clone(),
                    remote_revision: conflict.remote_revision.clone(),
                    live_input_revision: None,
                    recovery_revision: conflict.local_revision.clone(),
                    resolved_tree: conflict.local_revision.clone(),
                    resolution_commit: conflict.local_revision.clone(),
                    published: false,
                    applied: false,
                },
            )
            .expect("abandoned whole resolution");

        let resolved = crate::sync_conflicts::resolve_sync_conflict_with_state_store(
            &VaultPaths::new(&fixture.reader),
            &conflict.id,
            &crate::sync_conflicts::ResolveSyncConflictOptions {
                side: crate::sync_conflicts::SyncConflictResolutionSide::Local,
                group_ids: Vec::new(),
                remote: vulcan_sync::GitRemote::parse("origin").expect("remote"),
                live_ref: vulcan_sync::GitRefName::parse("refs/heads/__vulcan-sync/live")
                    .expect("live ref"),
                dry_run: false,
            },
            &fixture.store,
        )
        .expect("whole side choice applies to every unfinished group");
        assert_eq!(resolved.remaining_groups, Some(0));
        for (path, content) in [
            ("A.md", "local a\n"),
            ("B.md", "local b\n"),
            ("Unrelated.md", "later\n"),
        ] {
            assert_eq!(
                fs::read_to_string(fixture.reader.join(path)).expect("resolved file"),
                content
            );
        }
        let listed = crate::sync_conflicts::list_sync_conflicts_with_state_store(
            &VaultPaths::new(&fixture.reader),
            &fixture.store,
        )
        .expect("conflicts");
        assert_eq!(listed.count, 0);
    }

    #[test]
    fn grouped_side_resolution_survives_restart_and_unrelated_live_advancement() {
        let fixture = structured_sync_fixture(&[("A.md", "base a\n"), ("B.md", "base b\n")]);
        fs::write(fixture.writer.join("A.md"), "remote a\n").expect("remote A");
        fs::write(fixture.writer.join("B.md"), "remote b\n").expect("remote B");
        fs::write(fixture.reader.join("A.md"), "local a\n").expect("local A");
        fs::write(fixture.reader.join("B.md"), "local b\n").expect("local B");
        sync_git_vault_with_state_store(
            &VaultPaths::new(&fixture.writer),
            &GitSyncOptions::default(),
            &fixture.store,
        )
        .expect("writer conflict inputs");
        let conflict = sync_git_vault_with_state_store(
            &VaultPaths::new(&fixture.reader),
            &GitSyncOptions::default(),
            &fixture.store,
        )
        .expect("reader conflict")
        .conflict_record
        .expect("durable conflict");
        let group_for = |path: &str| {
            conflict
                .paths
                .iter()
                .find(|item| item.path == path)
                .expect("conflict path")
                .group_id
                .clone()
        };
        let options = |group_id: String| crate::sync_conflicts::ResolveSyncConflictOptions {
            side: crate::sync_conflicts::SyncConflictResolutionSide::Local,
            group_ids: vec![group_id],
            remote: vulcan_sync::GitRemote::parse("origin").expect("remote"),
            live_ref: vulcan_sync::GitRefName::parse("refs/heads/__vulcan-sync/live")
                .expect("live ref"),
            dry_run: false,
        };

        let first = crate::sync_conflicts::resolve_sync_conflict_with_state_store(
            &VaultPaths::new(&fixture.reader),
            &conflict.id,
            &options(group_for("A.md")),
            &fixture.store,
        )
        .expect("first group resolution");
        assert_eq!(first.remaining_groups, Some(1));
        assert_eq!(
            fs::read_to_string(fixture.reader.join("A.md")).expect("resolved A"),
            "local a\n"
        );
        assert_eq!(
            fs::read_to_string(fixture.reader.join("B.md")).expect("pending B"),
            "remote b\n"
        );

        fs::write(fixture.reader.join("Unrelated.md"), "later\n").expect("unrelated edit");
        sync_git_vault_with_state_store(
            &VaultPaths::new(&fixture.reader),
            &GitSyncOptions::default(),
            &fixture.store,
        )
        .expect("unrelated live advancement");
        let listed = crate::sync_conflicts::list_sync_conflicts_with_state_store(
            &VaultPaths::new(&fixture.reader),
            &fixture.store,
        )
        .expect("conflict remains actionable after restart");
        assert_eq!(listed.count, 1);

        let second = crate::sync_conflicts::resolve_sync_conflict_with_state_store(
            &VaultPaths::new(&fixture.reader),
            &conflict.id,
            &options(group_for("B.md")),
            &fixture.store,
        )
        .expect("second group resolution");
        assert_eq!(second.remaining_groups, Some(0));
        assert_eq!(
            fs::read_to_string(fixture.reader.join("B.md")).expect("resolved B"),
            "local b\n"
        );
        assert_eq!(
            fs::read_to_string(fixture.reader.join("Unrelated.md"))
                .expect("unrelated edit retained"),
            "later\n"
        );
    }

    #[test]
    fn successful_automatic_merge_reports_whole_tree_validation_evidence() {
        let fixture = structured_sync_fixture(&[("data.json", "{\"base\":true}\n")]);
        fs::write(
            fixture.writer.join("data.json"),
            "{\"base\":true,\"writer\":1}\n",
        )
        .expect("writer JSON");
        fs::write(
            fixture.reader.join("data.json"),
            "{\"base\":true,\"reader\":2}\n",
        )
        .expect("reader JSON");
        sync_git_vault_with_state_store(
            &VaultPaths::new(&fixture.writer),
            &GitSyncOptions::default(),
            &fixture.store,
        )
        .expect("writer push");

        let report = sync_git_vault_with_state_store(
            &VaultPaths::new(&fixture.reader),
            &GitSyncOptions::default(),
            &fixture.store,
        )
        .expect("structured merge");

        assert_eq!(report.sync.outcome, GitSyncOutcome::Merged);
        let checks = &report.sync.automatic_resolutions[0].validation.checks;
        assert!(checks.contains(&vulcan_sync::GitAutomaticValidationCheck::WholeTreeLinksValid));
        assert!(checks.contains(&vulcan_sync::GitAutomaticValidationCheck::MassDeletionPolicy));
    }

    #[test]
    fn whole_tree_validator_rejects_a_tree_over_the_shared_deletion_ceiling() {
        let temporary = tempdir().expect("temporary directory");
        git(
            temporary.path(),
            &["-c", "init.defaultBranch=main", "init", "--quiet"],
        );
        git(temporary.path(), &["config", "user.name", "Vulcan Test"]);
        git(
            temporary.path(),
            &["config", "user.email", "vulcan@example.invalid"],
        );
        fs::write(temporary.path().join("Keep.md"), "keep\n").expect("kept note");
        fs::write(temporary.path().join("Cleanup.md"), "remove\n").expect("removed note");
        git(temporary.path(), &["add", "--all", "--", "."]);
        git(temporary.path(), &["commit", "--quiet", "-m", "candidate"]);
        let candidate =
            vulcan_sync::GitOid::parse(git_stdout(temporary.path(), &["rev-parse", "HEAD"]))
                .expect("candidate oid");
        fs::remove_file(temporary.path().join("Cleanup.md")).expect("remove note");
        git(temporary.path(), &["add", "--all", "--", "."]);
        git(temporary.path(), &["commit", "--quiet", "-m", "merged"]);
        let merged_commit =
            vulcan_sync::GitOid::parse(git_stdout(temporary.path(), &["rev-parse", "HEAD"]))
                .expect("merged commit oid");
        let engine = vulcan_sync::GitCliEngine::default();
        let repository = engine
            .discover_repository(temporary.path())
            .expect("repository");
        let merged_tree = engine
            .tree_oid(&repository, &merged_commit)
            .expect("merged tree");
        let mut config = VaultConfig::default();
        config.sync.tree_validation.max_deleted_paths = 0;
        config.sync.tree_validation.max_deleted_percent = 0;

        let error = VaultTreeValidator::new(config)
            .validate(
                &engine,
                &GitAutomaticMergeValidation {
                    repository: &repository,
                    base: &candidate,
                    local_candidate: &candidate,
                    accepted_remote: &candidate,
                    merged_tree: &merged_tree,
                    resolved_paths: &[],
                },
            )
            .expect_err("deletion ceiling must reject the tree");

        assert!(error
            .to_string()
            .contains("exceeding the shared limits of 0 paths and 0 percent"));
    }

    /// Writes an executable test script and waits until it can be executed.
    /// Another test thread that forks while the script is open for writing
    /// briefly holds that descriptor, and executing the script then fails with
    /// ETXTBSY until the forked child execs. One successful run proves no such
    /// descriptor remains, so later executions are reliable.
    #[cfg(unix)]
    fn write_test_script(path: &Path, content: &str) {
        use std::os::unix::fs::PermissionsExt;

        fs::write(path, content).expect("write test script");
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))
            .expect("make test script executable");
        for _ in 0..500 {
            match std::process::Command::new(path)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
            {
                Err(error) if error.raw_os_error() == Some(libc::ETXTBSY) => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                _ => return,
            }
        }
        panic!("test script {} stayed busy", path.display());
    }

    #[cfg(unix)]
    #[test]
    fn whole_tree_validation_batches_git_work_independent_of_note_count() {
        let temporary = tempdir().expect("temporary directory");
        git(
            temporary.path(),
            &["-c", "init.defaultBranch=main", "init", "--quiet"],
        );
        git(temporary.path(), &["config", "user.name", "Vulcan Test"]);
        git(
            temporary.path(),
            &["config", "user.email", "vulcan@example.invalid"],
        );
        for index in 0..200 {
            fs::write(
                temporary.path().join(format!("Note-{index}.md")),
                format!("# Note {index}\n\n[[Note-{}]]\n", (index + 1) % 200),
            )
            .expect("note");
        }
        git(temporary.path(), &["add", "--all", "--", "."]);
        git(temporary.path(), &["commit", "--quiet", "-m", "notes"]);

        let trace = temporary.path().join("git-trace");
        let wrapper = temporary.path().join("git-wrapper");
        write_test_script(
            &wrapper,
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexec git \"$@\"\n",
                trace.display()
            ),
        );
        let engine = vulcan_sync::GitCliEngine::new(&wrapper);
        let repository = engine
            .discover_repository(temporary.path())
            .expect("repository");
        let commit =
            vulcan_sync::GitOid::parse(git_stdout(temporary.path(), &["rev-parse", "HEAD"]))
                .expect("commit");
        let tree = engine.tree_oid(&repository, &commit).expect("tree");
        fs::write(&trace, "").expect("reset trace");

        VaultTreeValidator::new(VaultConfig::default())
            .validate(
                &engine,
                &GitAutomaticMergeValidation {
                    repository: &repository,
                    base: &commit,
                    local_candidate: &commit,
                    accepted_remote: &commit,
                    merged_tree: &tree,
                    resolved_paths: &[],
                },
            )
            .expect("validation");

        let commands = fs::read_to_string(&trace).expect("trace");
        assert_eq!(commands.lines().count(), 4, "commands:\n{commands}");
        assert_eq!(
            commands
                .lines()
                .filter(|command| command.contains("cat-file --batch"))
                .count(),
            1,
            "commands:\n{commands}"
        );

        fs::write(&trace, "").expect("reset trace");
        VaultTreeValidator::new(VaultConfig::default())
            .validate(
                &engine,
                &GitAutomaticMergeValidation {
                    repository: &repository,
                    base: &commit,
                    local_candidate: &commit,
                    accepted_remote: &commit,
                    merged_tree: &tree,
                    resolved_paths: &[],
                },
            )
            .expect("cached validation");
        let commands = fs::read_to_string(&trace).expect("trace");
        assert!(
            commands
                .lines()
                .all(|command| !command.contains("cat-file --batch")),
            "a repeated validation must reuse parsed blobs:\n{commands}"
        );
    }

    #[test]
    fn cached_whole_tree_validation_still_rejects_a_new_broken_link() {
        let temporary = tempdir().expect("temporary directory");
        git(
            temporary.path(),
            &["-c", "init.defaultBranch=main", "init", "--quiet"],
        );
        git(temporary.path(), &["config", "user.name", "Vulcan Test"]);
        git(
            temporary.path(),
            &["config", "user.email", "vulcan@example.invalid"],
        );
        fs::write(temporary.path().join("Source.md"), "[[Target]]\n").expect("source");
        fs::write(temporary.path().join("Target.md"), "target\n").expect("target");
        git(temporary.path(), &["add", "--all", "--", "."]);
        git(temporary.path(), &["commit", "--quiet", "-m", "linked"]);
        let commit =
            vulcan_sync::GitOid::parse(git_stdout(temporary.path(), &["rev-parse", "HEAD"]))
                .expect("commit");
        fs::remove_file(temporary.path().join("Target.md")).expect("remove target");
        git(temporary.path(), &["add", "--all", "--", "."]);
        git(temporary.path(), &["commit", "--quiet", "-m", "broken"]);
        let broken =
            vulcan_sync::GitOid::parse(git_stdout(temporary.path(), &["rev-parse", "HEAD"]))
                .expect("broken commit");
        let engine = vulcan_sync::GitCliEngine::default();
        let repository = engine
            .discover_repository(temporary.path())
            .expect("repository");
        let tree = engine.tree_oid(&repository, &commit).expect("tree");
        let broken_tree = engine.tree_oid(&repository, &broken).expect("broken tree");
        let mut config = VaultConfig::default();
        config.sync.tree_validation.max_deleted_paths = 10;
        let request = |merged_tree| GitAutomaticMergeValidation {
            repository: &repository,
            base: &commit,
            local_candidate: &commit,
            accepted_remote: &commit,
            merged_tree,
            resolved_paths: &[],
        };

        VaultTreeValidator::new(config.clone())
            .validate(&engine, &request(&tree))
            .expect("warm the cache with a valid tree");
        assert!(merge_validation_store_path(&repository).is_file());
        let error = VaultTreeValidator::new(config)
            .validate(&engine, &request(&broken_tree))
            .expect_err("a cached analysis must still catch the new broken link");

        assert!(error.to_string().contains("introduces a new"), "{error}");
    }

    #[test]
    fn merge_validation_store_is_scoped_to_its_configuration_and_recovers() {
        let temporary = tempdir().expect("temporary directory");
        git(temporary.path(), &["init", "--quiet"]);
        let engine = vulcan_sync::GitCliEngine::default();
        let repository = engine
            .discover_repository(temporary.path())
            .expect("repository");
        let oid = engine
            .write_blob(&repository, b"[[Target]]\n")
            .expect("blob");
        let config = VaultConfig::default();
        let fingerprint = merge_validation_fingerprint(&config);
        let stored = |fingerprint: &str| {
            let mut cache = GitTreeAnalysisCache::default();
            MergeValidationStore::open(&repository, fingerprint)
                .expect("store")
                .load_into(&[&oid], &mut cache);
            cache.markdown.contains_key(&oid)
        };
        let mut additions = GitTreeAnalysisCache::default();
        additions.markdown.insert(
            oid.clone(),
            CachedMarkdown::from_source("[[Target]]\n", &config),
        );
        MergeValidationStore::open(&repository, &fingerprint)
            .expect("store")
            .insert(&additions);

        assert!(stored(&fingerprint));
        let mut other = VaultConfig::default();
        other.sync.tree_validation.max_deleted_paths += 1;
        assert!(!stored(&merge_validation_fingerprint(&other)));
        assert!(
            !stored(&fingerprint),
            "a new configuration clears the store"
        );

        let path = merge_validation_store_path(&repository);
        let _ = fs::remove_file(path.with_extension("sqlite-wal"));
        let _ = fs::remove_file(path.with_extension("sqlite-shm"));
        fs::write(&path, b"not a database").expect("corrupt the store");
        assert!(!stored(&fingerprint), "a corrupt store is rebuilt empty");
    }

    #[test]
    fn merge_validation_store_prunes_once_old_versions_outnumber_the_trees() {
        let temporary = tempdir().expect("temporary directory");
        git(temporary.path(), &["init", "--quiet"]);
        let engine = vulcan_sync::GitCliEngine::default();
        let repository = engine
            .discover_repository(temporary.path())
            .expect("repository");
        let config = VaultConfig::default();
        let fingerprint = merge_validation_fingerprint(&config);
        let mut store = MergeValidationStore::open(&repository, &fingerprint).expect("store");
        let mut additions = GitTreeAnalysisCache::default();
        let oids = (0..MERGE_VALIDATION_STORE_SLACK + 64)
            .map(|index| {
                vulcan_sync::GitOid::parse(format!("{index:040x}")).expect("synthetic oid")
            })
            .collect::<Vec<_>>();
        for oid in &oids {
            additions
                .markdown
                .insert(oid.clone(), CachedMarkdown::from_source("note\n", &config));
        }
        store.insert(&additions);
        let count = |store: &MergeValidationStore| {
            store
                .connection
                .query_row("SELECT count(*) FROM blobs", [], |row| row.get::<_, i64>(0))
                .expect("count")
        };

        let referenced = oids.iter().take(4).collect::<BTreeSet<_>>();
        store.prune_unless(&referenced);
        assert_eq!(count(&store), 4);
        store.prune_unless(&referenced);
        assert_eq!(count(&store), 4, "a small store is left alone");
    }

    #[test]
    fn whole_tree_blob_cache_shares_oids_across_paths_types_and_passes() {
        let temporary = tempdir().unwrap();
        git(temporary.path(), &["init", "--quiet"]);
        let engine = vulcan_sync::GitCliEngine::default();
        let repository = engine.discover_repository(temporary.path()).unwrap();
        let oid = engine
            .write_blob(&repository, br#"{"nodes":[],"edges":[]}"#)
            .unwrap();
        let entry = |path: &str| vulcan_sync::GitTreeEntry {
            path: path.to_string(),
            oid: oid.clone(),
            mode: "100644".to_string(),
            kind: "blob".to_string(),
        };
        let first = entry("first.md");
        let second = entry("second.md");
        let canvas = entry("board.canvas");
        let config = VaultConfig::default();
        let mut cache = GitTreeAnalysisCache::default();
        let before = engine.subprocess_count().unwrap();
        for _ in 0..3 {
            cache_tree_content(
                &engine,
                &repository,
                &[&first, &second],
                &[&canvas],
                &config,
                &mut cache,
            )
            .unwrap();
        }
        assert_eq!(cache.markdown.len(), 1);
        assert_eq!(cache.canvas.len(), 1);
        assert_eq!(engine.subprocess_count().unwrap() - before, 1);
    }

    #[cfg(unix)]
    #[test]
    fn whole_tree_blob_cache_publishes_nothing_after_late_stream_or_process_failure() {
        let temporary = tempdir().unwrap();
        git(temporary.path(), &["init", "--quiet"]);
        let real_engine = vulcan_sync::GitCliEngine::default();
        let repository = real_engine.discover_repository(temporary.path()).unwrap();
        let entry = |path: &str, digit: char| vulcan_sync::GitTreeEntry {
            path: path.to_string(),
            oid: vulcan_sync::GitOid::parse(digit.to_string().repeat(40)).unwrap(),
            mode: "100644".to_string(),
            kind: "blob".to_string(),
        };
        let existing = entry("existing.md", '0');
        let markdown = entry("new.md", '1');
        let canvas = entry("new.canvas", '2');
        let last = entry("last.md", '3');
        let wrapper = temporary.path().join("git-wrapper");
        let engine = vulcan_sync::GitCliEngine::new(&wrapper);
        let config = VaultConfig::default();
        let mut cache = GitTreeAnalysisCache::default();
        cache.markdown.insert(
            existing.oid.clone(),
            CachedMarkdown::from_source("keep", &config),
        );
        let prefix = format!(
            "{} blob 2\\nhi\\n{} blob 12\\n{{\"nodes\":[]}}\\n",
            markdown.oid, canvas.oid
        );
        for suffix in [
            format!("printf '{} blob 4\\nab'; exit 0", last.oid),
            format!("printf '{} blob 4\\nlast\\n'; exit 7", last.oid),
        ] {
            write_test_script(
                &wrapper,
                &format!("#!/bin/sh\ncat >/dev/null\nprintf '{prefix}'\n{suffix}\n"),
            );
            let error = cache_tree_content(
                &engine,
                &repository,
                &[&existing, &markdown, &last],
                &[&canvas],
                &config,
                &mut cache,
            )
            .expect_err("late failure");
            assert!(
                error.to_string().contains("blob content") || error.to_string().contains('7'),
                "{error}"
            );
            assert_eq!(
                cache.markdown.keys().collect::<Vec<_>>(),
                vec![&existing.oid]
            );
            assert_eq!(cache.markdown[&existing.oid].bytes, 4);
            assert!(cache.canvas.is_empty());
        }
        write_test_script(
            &wrapper,
            &format!(
                "#!/bin/sh\ncat >/dev/null\nprintf '{prefix}{} blob 4\\nlast\\n'\n",
                last.oid
            ),
        );
        cache_tree_content(
            &engine,
            &repository,
            &[&existing, &markdown, &last],
            &[&canvas],
            &config,
            &mut cache,
        )
        .unwrap();
        assert_eq!(cache.markdown.len(), 3);
        assert_eq!(cache.canvas.len(), 1);
        assert_eq!(engine.subprocess_count(), Some(3));
    }

    #[test]
    fn whole_tree_blob_cache_rejects_invalid_content_and_missing_objects() {
        let temporary = tempdir().unwrap();
        git(temporary.path(), &["init", "--quiet"]);
        let engine = vulcan_sync::GitCliEngine::default();
        let repository = engine.discover_repository(temporary.path()).unwrap();
        for (path, data, message) in [
            ("bad.md", b"\xff".as_slice(), "not valid UTF-8"),
            ("bad.canvas", b"\xff".as_slice(), "not valid UTF-8"),
            ("bad.canvas", b"{".as_slice(), "not valid JSON"),
            ("bad.canvas", b"{}".as_slice(), "no nodes array"),
        ] {
            let entry = vulcan_sync::GitTreeEntry {
                path: path.to_string(),
                oid: engine.write_blob(&repository, data).unwrap(),
                mode: "100644".to_string(),
                kind: "blob".to_string(),
            };
            let entries = [&entry];
            let (markdown, canvas) = if markdown_path(path) {
                (entries.as_slice(), &[][..])
            } else {
                (&[][..], entries.as_slice())
            };
            let error = cache_tree_content(
                &engine,
                &repository,
                markdown,
                canvas,
                &VaultConfig::default(),
                &mut GitTreeAnalysisCache::default(),
            )
            .expect_err("invalid content");
            assert!(error.to_string().contains(message), "{error}");
        }
        let missing = vulcan_sync::GitTreeEntry {
            path: "missing.md".to_string(),
            oid: vulcan_sync::GitOid::parse("1111111111111111111111111111111111111111").unwrap(),
            mode: "100644".to_string(),
            kind: "blob".to_string(),
        };
        assert!(cache_tree_content(
            &engine,
            &repository,
            &[&missing],
            &[],
            &VaultConfig::default(),
            &mut GitTreeAnalysisCache::default()
        )
        .is_err());
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
        let live = git_stdout(
            &remote,
            &[
                "ls-tree",
                "-r",
                "--name-only",
                "refs/heads/__vulcan-sync/live",
            ],
        );
        assert_eq!(
            live.trim(),
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
        assert_eq!(application.additions, 1, "{:?}", application.paths);
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
        sync_git_vault_with_state_store(&writer_paths, &GitSyncOptions::default(), &state_store)
            .expect("bootstrap sync");

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
        sync_git_vault_with_profile(
            &reader_paths,
            &GitSyncOptions::default(),
            SyncContentProfile::FilesOnly,
        )
        .expect("establish reader sync baseline");

        fs::write(writer.join("Remote.md"), "remote note\n").expect("remote note");
        sync_git_vault_with_state_store(&writer_paths, &GitSyncOptions::default(), &state_store)
            .expect("writer push");
        let report = sync_git_vault_with_profile(
            &reader_paths,
            &GitSyncOptions::default(),
            SyncContentProfile::FilesOnly,
        )
        .expect("files-only reader sync");

        assert!(
            reader.join("Remote.md").is_file(),
            "outcome {:?}, conflict {:?}",
            report.sync.outcome,
            report.sync.conflict
        );
        assert!(report.cache_refresh.is_none());
        assert!(report.cache_refresh_error.is_none());
        assert!(!reader_paths.cache_db().exists());
    }

    #[test]
    fn clean_merge_with_new_link_ambiguity_is_preserved_as_a_conflict() {
        // Disjoint same-name additions merge cleanly in Git but leave
        // [[Widget]] newly ambiguous: the base and both candidates only
        // ever see zero or one candidate, so the ambiguity must block the
        // automatic merge even though Git reports no conflicts.
        let fixture = structured_sync_fixture(&[("Home.md", "[[Widget]]\n")]);
        fs::create_dir(fixture.writer.join("Writer")).expect("writer folder");
        fs::write(fixture.writer.join("Writer/Widget.md"), "writer widget\n")
            .expect("writer widget");
        fs::create_dir(fixture.reader.join("Reader")).expect("reader folder");
        fs::write(fixture.reader.join("Reader/Widget.md"), "reader widget\n")
            .expect("reader widget");
        sync_git_vault_with_state_store(
            &VaultPaths::new(&fixture.writer),
            &GitSyncOptions::default(),
            &fixture.store,
        )
        .expect("writer push");

        let report = sync_git_vault_with_state_store(
            &VaultPaths::new(&fixture.reader),
            &GitSyncOptions::default(),
            &fixture.store,
        )
        .expect("validation conflict");

        assert_eq!(report.sync.outcome, GitSyncOutcome::Conflicted);
        let conflict = report.sync.conflict.as_ref().expect("conflict");
        assert_eq!(conflict.scope, GitConflictScope::TreeValidation);
        assert!(conflict
            .diagnostics
            .contains("introduces a new ambiguous wikilink link-resolution problem"));

        let resolved = crate::sync_conflicts::resolve_sync_conflict_with_state_store(
            &VaultPaths::new(&fixture.reader),
            &conflict.id,
            &crate::sync_conflicts::ResolveSyncConflictOptions {
                side: crate::sync_conflicts::SyncConflictResolutionSide::Local,
                group_ids: Vec::new(),
                remote: vulcan_sync::GitRemote::parse("origin").expect("remote"),
                live_ref: vulcan_sync::GitRefName::parse("refs/heads/__vulcan-sync/live")
                    .expect("live ref"),
                dry_run: false,
            },
            &fixture.store,
        )
        .expect("resolve whole-tree validation conflict");
        assert_eq!(
            resolved.outcome,
            crate::sync_conflicts::ResolveSyncConflictOutcome::Resolved
        );
        assert!(fixture.reader.join("Reader/Widget.md").exists());
        assert!(!fixture.reader.join("Writer/Widget.md").exists());
    }

    #[test]
    fn files_only_profile_keeps_concurrent_merge_for_review_without_markdown_validation() {
        let fixture = structured_sync_fixture(&[("Home.md", "[[Widget]]\n")]);
        let reader_paths = VaultPaths::new(&fixture.reader);
        assert!(!reader_paths.cache_db().exists());
        fs::create_dir(fixture.writer.join("Writer")).expect("writer folder");
        fs::write(fixture.writer.join("Writer/Widget.md"), "writer widget\n")
            .expect("writer widget");
        fs::create_dir(fixture.reader.join("Reader")).expect("reader folder");
        fs::write(fixture.reader.join("Reader/Widget.md"), "reader widget\n")
            .expect("reader widget");
        sync_git_vault_with_state_store(
            &VaultPaths::new(&fixture.writer),
            &GitSyncOptions::default(),
            &fixture.store,
        )
        .expect("writer push");

        let report = sync_git_vault_with_profile(
            &VaultPaths::new(&fixture.reader),
            &GitSyncOptions::default(),
            SyncContentProfile::FilesOnly,
        )
        .expect("files-only synchronization");

        assert_eq!(report.sync.outcome, GitSyncOutcome::Conflicted);
        assert!(report
            .sync
            .conflict
            .as_ref()
            .expect("review conflict")
            .diagnostics
            .contains("files-only profile requires review"));
        assert!(!fixture.reader.join("Writer/Widget.md").exists());
        assert!(fixture.reader.join("Reader/Widget.md").is_file());
        assert!(!reader_paths.cache_db().exists());
    }

    #[test]
    fn clean_merge_with_new_canvas_ambiguity_is_preserved_as_a_conflict() {
        // The canvas references Widget.md while each side holds exactly one
        // same-name note; the clean merge leaves two, so the embedded
        // reference is newly ambiguous and must block the automatic merge.
        let fixture = structured_sync_fixture(&[("Board.canvas", "{\"nodes\":[],\"edges\":[]}")]);
        fs::write(
            fixture.writer.join("Board.canvas"),
            "{\"nodes\":[{\"id\":\"n1\",\"type\":\"file\",\"file\":\"Widget.md\"}],\"edges\":[]}",
        )
        .expect("writer canvas");
        fs::create_dir(fixture.writer.join("Writer")).expect("writer folder");
        fs::write(fixture.writer.join("Writer/Widget.md"), "writer widget\n")
            .expect("writer widget");
        fs::create_dir(fixture.reader.join("Reader")).expect("reader folder");
        fs::write(fixture.reader.join("Reader/Widget.md"), "reader widget\n")
            .expect("reader widget");
        sync_git_vault_with_state_store(
            &VaultPaths::new(&fixture.writer),
            &GitSyncOptions::default(),
            &fixture.store,
        )
        .expect("writer push");

        let report = sync_git_vault_with_state_store(
            &VaultPaths::new(&fixture.reader),
            &GitSyncOptions::default(),
            &fixture.store,
        )
        .expect("validation conflict");

        assert_eq!(report.sync.outcome, GitSyncOutcome::Conflicted);
        assert!(report
            .sync
            .conflict
            .as_ref()
            .expect("conflict")
            .diagnostics
            .contains("introduces a new ambiguous canvas link-resolution problem"));
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
        sync_git_vault_with_state_store(&writer_paths, &GitSyncOptions::default(), &state_store)
            .expect("bootstrap sync");

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
        sync_git_vault_with_state_store(&writer_paths, &GitSyncOptions::default(), &state_store)
            .expect("writer push");
        let report = sync_git_vault_with_state_store(
            &reader_paths,
            &GitSyncOptions::default(),
            &state_store,
        )
        .expect("reader synchronization succeeds despite the cache failure");

        assert!(matches!(
            report.sync.outcome,
            GitSyncOutcome::Pulled | GitSyncOutcome::Merged
        ));
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

    #[cfg(unix)]
    #[test]
    fn platform_preflight_failure_retains_the_captured_recovery_journal() {
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
        fs::write(vault.join("Home.md"), "home\n").expect("home note");
        git(&vault, &["add", "Home.md"]);
        git(&vault, &["commit", "--quiet", "-m", "initial"]);
        fs::write(vault.join("CON.txt"), "preserve me\n").expect("reserved note");
        let paths = VaultPaths::new(&vault);
        let store = SyncStateStore::at(temporary.path().join("state"));
        let options = GitSyncOptions {
            platform: GitPlatformProfile::AndroidShared,
            ..GitSyncOptions::default()
        };

        assert!(sync_git_vault_with_state_store(&paths, &options, &store).is_err());

        let key = crate::sync_state::repository_state_key(
            &fs::canonicalize(&vault).expect("canonical vault"),
        );
        let journal = store
            .load(&key)
            .expect("load journal")
            .expect("retained platform journal");
        assert_eq!(journal.phase, SyncJournalPhase::BackingUp);
        assert!(journal.local_snapshot.is_some());
        assert!(journal.expected_worktree_tree.is_some());
        assert!(journal
            .error
            .as_deref()
            .is_some_and(|error| error.contains("platform `android_shared`")));
        assert_eq!(
            fs::read_to_string(vault.join("CON.txt")).expect("preserved bytes"),
            "preserve me\n"
        );
        assert!(
            !git_stdout(
                &vault,
                &["ls-remote", "origin", "refs/heads/__vulcan-sync/devices/*",],
            )
            .is_empty(),
            "platform-incompatible bytes must still reach the remote device safety namespace"
        );
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
    fn sync_doctor_reports_missing_objects_behind_sync_refs() {
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

        let options = GitSyncOptions::default();
        let refs = GitSyncRefs::for_options(&options).expect("sync refs");
        let local_ref = refs
            .local
            .as_str()
            .strip_prefix("refs/")
            .expect("local ref path");
        let local_ref_path = vault.join(".git").join("refs").join(local_ref);
        fs::create_dir_all(local_ref_path.parent().expect("ref parent")).expect("ref parent");
        fs::write(
            &local_ref_path,
            "1111111111111111111111111111111111111111\n",
        )
        .expect("dangling sync ref");

        let report = doctor_git_vault_with_state_store(
            &VaultPaths::new(&vault),
            &options,
            &SyncStateStore::at(temporary.path().join("state")),
        );

        assert!(!report.healthy);
        assert!(report.checks.iter().any(|check| {
            check.code == "git.refs"
                && check.severity == SyncDoctorSeverity::Error
                && check
                    .message
                    .contains("does not resolve to a readable commit object")
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
        assert_conflict_operational_stats(&report);
        let record = report.conflict_record.expect("durable conflict record");
        assert_eq!(record.paths.len(), 1);
        assert_eq!(record.paths[0].path, "Home.md");
        assert!(record.preserved_record_ref.is_some());
        assert!(record.provenance_revision.is_some());
        assert_projection_candidate(&record);
        assert_overlapping_text_classification(&record);
        assert_conflict_artifacts(&store, &record);
        assert_conflict_read_workflows(&VaultPaths::new(&reader), &store, &record);
        assert_projected_worktree(&reader);
    }

    mod storm_tests;
}
