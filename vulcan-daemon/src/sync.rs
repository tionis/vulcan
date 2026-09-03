//! Registry-aware finite synchronization orchestration.

use crate::registry::{RegistryError, WikiId, WikiRegistration, WikiRegistry};
use crate::supervisor::{
    ClaimedSyncJob, IdempotentEnqueueAggregateSyncReport, SupervisedSyncJob, SupervisorError,
    SyncSupervisor,
};
use serde::Serialize;
use std::error::Error;
use std::fmt::{Display, Formatter};
use vulcan_app::sync::{
    sync_git_vault, sync_git_vault_with_observer_and_engine, GitPlatformProfile,
    GitSyncObserverError, GitSyncOptions, GitSyncOutcome, GitSyncPhase, GitSyncProgress,
    VaultSyncReport,
};
use vulcan_app::sync_state::{same_work_tree, SyncStateStore};
use vulcan_core::{
    resolve_permission_profile, PermissionGuard, ProfilePermissionGuard, VaultPaths,
};
use vulcan_sync::{
    GitBranchSync, GitBranchSyncAction, GitCliEngine, GitRemoteObservation, GitSyncObserver,
    SyncError, SyncErrorCategory, SyncJobState, SyncState, SyncStatus,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DaemonSyncExecution {
    pub job: SupervisedSyncJob,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report: Option<VaultSyncReport>,
}

/// Renders one completed daemon sync execution as a single verbose log line.
/// Contains only wiki identity, triggers, and outcome — never repository
/// credentials or notification endpoint URLs.
#[must_use]
pub fn format_sync_execution(execution: &DaemonSyncExecution) -> String {
    let wiki = execution
        .job
        .job
        .wiki_id
        .as_deref()
        .unwrap_or("<unregistered>");
    let triggers = execution
        .job
        .triggers
        .iter()
        .map(|trigger| format!("{trigger:?}"))
        .collect::<Vec<_>>()
        .join(",");
    match execution.report.as_ref() {
        Some(report) => format!(
            "sync job: wiki `{wiki}` [{triggers}] -> {:?} ({:?}){}{}",
            execution.job.job.state,
            report.sync.outcome,
            format_watch_summary(&execution.job),
            format_branch_summary(report.sync.branch.as_ref()),
        ),
        None => format!(
            "sync job: wiki `{wiki}` [{triggers}] -> {:?}{}",
            execution.job.job.state,
            format_watch_summary(&execution.job),
        ),
    }
}

/// Renders the branch lane of a completed execution, if the report carries
/// one. Empty when the branch lane did not run.
fn format_branch_summary(branch: Option<&GitBranchSync>) -> String {
    let Some(lane) = branch else {
        return String::new();
    };
    let name = lane
        .branch
        .as_str()
        .strip_prefix("refs/heads/")
        .unwrap_or(lane.branch.as_str());
    if lane.pushed {
        format!(" branch `{name}`: {:?}, pushed", lane.action)
    } else {
        format!(" branch `{name}`: {:?}", lane.action)
    }
}

/// Renders a branch lane failure for unconditional logging: a failed pull
/// strategy or a failed push. Returns None when the lane needs no attention.
/// Same redaction posture as other daemon diagnostics: endpoint identity only
/// (the branch lane never handles subscribe URLs), remote names included.
#[must_use]
pub fn format_branch_diagnostic(wiki_id: Option<&str>, branch: &GitBranchSync) -> Option<String> {
    let wiki = wiki_id.unwrap_or("<unregistered>");
    let name = branch
        .branch
        .as_str()
        .strip_prefix("refs/heads/")
        .unwrap_or(branch.branch.as_str());
    if branch.action == GitBranchSyncAction::Failed {
        return Some(format!(
            "branch `{name}` (wiki `{wiki}`) failed: {}",
            branch.detail.as_deref().unwrap_or("unknown reason"),
        ));
    }
    branch
        .push_detail
        .as_deref()
        .map(|detail| format!("branch `{name}` (wiki `{wiki}`) push failed: {detail}"))
}

/// Renders the watcher trigger metadata of a supervised job, so a repeating
/// trigger explains itself: churning paths versus rescan/error conditions.
fn format_watch_summary(job: &SupervisedSyncJob) -> String {
    let Some(watch) = job.watch.as_ref() else {
        return String::new();
    };
    let mut summary = format!(
        " watch events={} untagged={} paths={} rescan={} errors={}",
        watch.event_count,
        watch.untagged_events,
        watch.paths.len(),
        watch.safety_rescan,
        watch.watcher_errors.len(),
    );
    if let Some(first) = watch.watcher_errors.first() {
        use std::fmt::Write;
        let truncated: String = first.chars().take(200).collect();
        write!(summary, " first_error={truncated:?}").expect("write to a String");
    }
    summary
}

/// Claims and executes one queued daemon job through the same cancellable
/// application transaction used by direct CLI synchronization.
pub fn execute_next_sync_job(
    supervisor: &SyncSupervisor,
    registry: &WikiRegistry,
    options: &GitSyncOptions,
) -> Result<Option<DaemonSyncExecution>, SupervisorError> {
    let state_store = SyncStateStore::user_default()
        .map_err(|error| SupervisorError::InvalidState(error.to_string()))?;
    execute_next_sync_job_with_state_store(supervisor, registry, options, &state_store)
}

pub fn execute_next_sync_job_with_state_store(
    supervisor: &SyncSupervisor,
    registry: &WikiRegistry,
    options: &GitSyncOptions,
    state_store: &SyncStateStore,
) -> Result<Option<DaemonSyncExecution>, SupervisorError> {
    execute_next_sync_job_with_state_store_and_engine(
        supervisor,
        registry,
        options,
        state_store,
        &GitCliEngine::default(),
    )
}

pub fn execute_next_sync_job_with_state_store_and_engine(
    supervisor: &SyncSupervisor,
    registry: &WikiRegistry,
    options: &GitSyncOptions,
    state_store: &SyncStateStore,
    engine: &GitCliEngine,
) -> Result<Option<DaemonSyncExecution>, SupervisorError> {
    let Some(claimed) = supervisor.claim_next()? else {
        return Ok(None);
    };
    Ok(Some(execute_claimed_job(
        supervisor,
        registry,
        options,
        state_store,
        engine,
        &claimed,
    )?))
}

fn execute_claimed_job(
    supervisor: &SyncSupervisor,
    registry: &WikiRegistry,
    options: &GitSyncOptions,
    state_store: &SyncStateStore,
    engine: &GitCliEngine,
    claimed: &ClaimedSyncJob,
) -> Result<DaemonSyncExecution, SupervisorError> {
    let id = claimed.job.job.id.clone();
    let registration = match resolve_claimed_registration(registry, claimed) {
        Ok(registration) => registration,
        Err(error) => return complete_execution_error(supervisor, &id, error),
    };
    if registration.sync_paused
        && !claimed
            .job
            .triggers
            .contains(&vulcan_sync::SyncJobTrigger::Manual)
    {
        let status = SyncStatus {
            state: SyncState::Paused,
            backend: claimed.job.job.backend.clone(),
            vault: claimed.job.job.vault.clone(),
            local_revision: None,
            remote_revision: None,
            accepted_revision: None,
            unresolved_conflicts: 0,
            detail: Some("automatic synchronization is paused".to_string()),
        };
        let job = supervisor.complete(&id, SyncJobState::Paused, Some(status), None)?;
        return Ok(DaemonSyncExecution { job, report: None });
    }
    let paths = VaultPaths::new(&registration.path);
    if let Err(error) = check_registration_permission(&paths, &registration) {
        return complete_execution_error(supervisor, &id, error);
    }
    let mut options = match options_for_registration(options, &registration) {
        Ok(options) => options,
        Err(error) => return complete_execution_error(supervisor, &id, error),
    };
    options.remote_observation = remote_observation_for_triggers(&claimed.job.triggers);
    let engine = engine.clone().with_command_timeout(options.command_timeout);
    let mut observer = SupervisorProgressObserver {
        supervisor,
        job_id: &id,
        vault: &registration.path,
    };
    match sync_git_vault_with_observer_and_engine(
        &engine,
        &paths,
        &options,
        state_store,
        &claimed.cancellation,
        &mut observer,
    ) {
        Ok(report) => {
            let status = job_status_from_report(&report);
            let state = match report.sync.outcome {
                GitSyncOutcome::Conflicted => SyncJobState::Conflicted,
                GitSyncOutcome::Paused => SyncJobState::Paused,
                _ => SyncJobState::Succeeded,
            };
            let job = supervisor.complete(&id, state, Some(status), None)?;
            Ok(DaemonSyncExecution {
                job,
                report: Some(report),
            })
        }
        Err(error) => complete_execution_error(
            supervisor,
            &id,
            if claimed.cancellation.is_cancelled() {
                SyncError::new(SyncErrorCategory::Cancelled, error.to_string(), false)
            } else {
                SyncError::new(SyncErrorCategory::Unknown, error.to_string(), true)
            },
        ),
    }
}

fn remote_observation_for_triggers(
    triggers: &[vulcan_sync::SyncJobTrigger],
) -> GitRemoteObservation {
    if triggers.contains(&vulcan_sync::SyncJobTrigger::RemoteNotification) {
        GitRemoteObservation::Fetch
    } else {
        GitRemoteObservation::Query
    }
}

fn resolve_claimed_registration(
    registry: &WikiRegistry,
    claimed: &ClaimedSyncJob,
) -> Result<WikiRegistration, SyncError> {
    let wiki_id = claimed.job.job.wiki_id.as_deref().unwrap_or_default();
    let registration = registry
        .load()
        .map_err(|error| SyncError::new(SyncErrorCategory::Configuration, error.to_string(), true))?
        .vaults
        .into_iter()
        .find(|wiki| wiki.id.as_str() == wiki_id)
        .ok_or_else(|| {
            SyncError::new(
                SyncErrorCategory::Configuration,
                format!("registered wiki `{wiki_id}` is no longer available"),
                false,
            )
        })?;
    if !same_work_tree(&claimed.job.job.vault, &registration.path) {
        return Err(SyncError::new(
            SyncErrorCategory::Configuration,
            "registered wiki path changed after the job was queued",
            false,
        ));
    }
    if registration
        .sync_backend
        .as_deref()
        .is_some_and(|backend| backend != "git")
    {
        return Err(SyncError::new(
            SyncErrorCategory::Unsupported,
            format!(
                "wiki `{wiki_id}` uses unsupported sync backend `{}`",
                registration.sync_backend.as_deref().unwrap_or_default()
            ),
            false,
        ));
    }
    Ok(registration)
}

fn check_registration_permission(
    paths: &VaultPaths,
    registration: &WikiRegistration,
) -> Result<(), SyncError> {
    resolve_permission_profile(paths, registration.permissions_profile.as_deref())
        .map_err(|error| SyncError::new(SyncErrorCategory::Configuration, error.to_string(), false))
        .and_then(|selection| {
            ProfilePermissionGuard::new(paths, selection)
                .check_git()
                .map_err(|error| {
                    SyncError::new(SyncErrorCategory::Configuration, error.to_string(), false)
                })
        })
}

fn options_for_registration(
    options: &GitSyncOptions,
    registration: &WikiRegistration,
) -> Result<GitSyncOptions, SyncError> {
    let platform = registration
        .platform_profile
        .as_deref()
        .map(GitPlatformProfile::parse)
        .transpose()
        .map_err(|error| {
            SyncError::new(
                SyncErrorCategory::Configuration,
                format!("wiki `{}`: {error}", registration.id),
                false,
            )
        })?
        .unwrap_or_else(GitPlatformProfile::native);
    let mut effective = options.clone();
    effective.platform = platform;
    Ok(effective)
}

fn complete_execution_error(
    supervisor: &SyncSupervisor,
    id: &str,
    error: SyncError,
) -> Result<DaemonSyncExecution, SupervisorError> {
    let state = if error.category == SyncErrorCategory::Cancelled {
        SyncJobState::Cancelled
    } else {
        SyncJobState::Failed
    };
    let job = supervisor.complete(id, state, None, Some(error))?;
    Ok(DaemonSyncExecution { job, report: None })
}

struct SupervisorProgressObserver<'a> {
    supervisor: &'a SyncSupervisor,
    job_id: &'a str,
    vault: &'a std::path::Path,
}

impl GitSyncObserver for SupervisorProgressObserver<'_> {
    fn progress(&mut self, progress: &GitSyncProgress) -> Result<(), GitSyncObserverError> {
        let state = match progress.phase {
            GitSyncPhase::Preparing => SyncState::CapturePending,
            GitSyncPhase::Capturing => SyncState::Capturing,
            GitSyncPhase::Captured => SyncState::CapturedUnpushed,
            GitSyncPhase::Fetching => SyncState::Fetching,
            GitSyncPhase::Fetched => SyncState::Fetched,
            GitSyncPhase::Merging => SyncState::Merging,
            GitSyncPhase::Pushing => SyncState::Pushing,
            GitSyncPhase::Applying | GitSyncPhase::Verifying => SyncState::Applying,
            GitSyncPhase::Paused => SyncState::Paused,
            GitSyncPhase::Conflicted => SyncState::Conflicted,
            GitSyncPhase::Completed => SyncState::Clean,
        };
        self.supervisor
            .update_running_status(
                self.job_id,
                SyncStatus {
                    state,
                    backend: "git".to_string(),
                    vault: self.vault.to_path_buf(),
                    local_revision: progress.local_snapshot.as_ref().map(ToString::to_string),
                    remote_revision: None,
                    accepted_revision: progress.accepted.as_ref().map(ToString::to_string),
                    unresolved_conflicts: usize::from(progress.phase == GitSyncPhase::Conflicted),
                    detail: None,
                },
            )
            .map(|_| ())
            .map_err(|error| GitSyncObserverError::new(error.to_string()))
    }
}

fn job_status_from_report(report: &VaultSyncReport) -> SyncStatus {
    let state = match report.sync.outcome {
        GitSyncOutcome::Conflicted => SyncState::Conflicted,
        GitSyncOutcome::Paused => SyncState::Paused,
        _ => SyncState::Clean,
    };
    SyncStatus {
        state,
        backend: "git".to_string(),
        vault: report
            .sync
            .repository
            .work_tree
            .clone()
            .unwrap_or_else(|| report.sync.repository.git_dir.clone()),
        local_revision: report.sync.local_snapshot.as_ref().map(ToString::to_string),
        remote_revision: report
            .sync
            .accepted
            .as_ref()
            .or(report.sync.remote_before.as_ref())
            .map(ToString::to_string),
        accepted_revision: report.sync.accepted.as_ref().map(ToString::to_string),
        unresolved_conflicts: usize::from(report.sync.conflict.is_some()),
        detail: report
            .sync
            .pause
            .as_ref()
            .map(|pause| format!("{:?}", pause.reason)),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegisteredSyncSelection {
    Wiki(WikiId),
    Group(String),
    All,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RegisteredSyncItemReport {
    pub wiki_id: WikiId,
    pub path: std::path::PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report: Option<VaultSyncReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RegisteredSyncReport {
    pub selection: String,
    pub dry_run: bool,
    pub total: usize,
    pub succeeded: usize,
    pub conflicted: usize,
    pub failed: usize,
    pub items: Vec<RegisteredSyncItemReport>,
}

#[derive(Debug)]
pub enum RegisteredSyncError {
    Registry(RegistryError),
    Supervisor(SupervisorError),
    EmptyGroup(String),
}

impl Display for RegisteredSyncError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Registry(error) => Display::fmt(error, formatter),
            Self::Supervisor(error) => Display::fmt(error, formatter),
            Self::EmptyGroup(group) => {
                write!(formatter, "no registered wikis belong to group `{group}`")
            }
        }
    }
}

impl Error for RegisteredSyncError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Registry(error) => Some(error),
            Self::Supervisor(error) => Some(error),
            Self::EmptyGroup(_) => None,
        }
    }
}

impl From<RegistryError> for RegisteredSyncError {
    fn from(error: RegistryError) -> Self {
        Self::Registry(error)
    }
}

impl From<SupervisorError> for RegisteredSyncError {
    fn from(error: SupervisorError) -> Self {
        Self::Supervisor(error)
    }
}

pub fn enqueue_registered_wikis(
    registry: &WikiRegistry,
    supervisor: &SyncSupervisor,
    selection: &RegisteredSyncSelection,
    credential_scope: &str,
    idempotency_key: &str,
) -> Result<IdempotentEnqueueAggregateSyncReport, RegisteredSyncError> {
    let wikis = select_wikis(registry, selection)?;
    supervisor
        .enqueue_aggregate_idempotent(
            credential_scope,
            idempotency_key,
            selection_label(selection),
            wikis
                .into_iter()
                .map(|wiki| (wiki.id.to_string(), wiki.path))
                .collect(),
            vulcan_sync::SyncJobTrigger::Manual,
        )
        .map_err(Into::into)
}

pub fn sync_registered_wikis(
    registry: &WikiRegistry,
    selection: &RegisteredSyncSelection,
    options: &GitSyncOptions,
    permission_profile: Option<&str>,
) -> Result<RegisteredSyncReport, RegisteredSyncError> {
    let wikis = select_wikis(registry, selection)?;
    let mut items = Vec::with_capacity(wikis.len());
    for wiki in wikis {
        items.push(sync_registration(&wiki, options, permission_profile));
    }
    let conflicted = items
        .iter()
        .filter(|item| {
            item.report
                .as_ref()
                .is_some_and(|report| report.sync.outcome == GitSyncOutcome::Conflicted)
        })
        .count();
    let failed = items.iter().filter(|item| item.error.is_some()).count();
    let total = items.len();
    Ok(RegisteredSyncReport {
        selection: selection_label(selection),
        dry_run: options.dry_run,
        total,
        succeeded: total - failed - conflicted,
        conflicted,
        failed,
        items,
    })
}

fn select_wikis(
    registry: &WikiRegistry,
    selection: &RegisteredSyncSelection,
) -> Result<Vec<WikiRegistration>, RegisteredSyncError> {
    let config = registry.load()?;
    match selection {
        RegisteredSyncSelection::Wiki(id) => config
            .vaults
            .into_iter()
            .find(|wiki| &wiki.id == id)
            .map(|wiki| vec![wiki])
            .ok_or_else(|| RegistryError::UnknownWiki(id.clone()).into()),
        RegisteredSyncSelection::Group(group) => {
            let selected = config
                .vaults
                .into_iter()
                .filter(|wiki| wiki.groups.iter().any(|item| item == group))
                .collect::<Vec<_>>();
            if selected.is_empty() {
                return Err(RegisteredSyncError::EmptyGroup(group.clone()));
            }
            Ok(selected)
        }
        RegisteredSyncSelection::All => Ok(config.vaults),
    }
}

fn sync_registration(
    wiki: &WikiRegistration,
    options: &GitSyncOptions,
    permission_profile: Option<&str>,
) -> RegisteredSyncItemReport {
    let paths = VaultPaths::new(&wiki.path);
    let result = resolve_permission_profile(&paths, permission_profile)
        .map_err(|error| error.to_string())
        .and_then(|selection| {
            ProfilePermissionGuard::new(&paths, selection)
                .check_git()
                .map_err(|error| error.to_string())
        })
        .and_then(|()| {
            if wiki
                .sync_backend
                .as_deref()
                .is_none_or(|backend| backend == "git")
            {
                let effective =
                    options_for_registration(options, wiki).map_err(|error| error.to_string())?;
                sync_git_vault(&paths, &effective).map_err(|error| error.to_string())
            } else {
                Err(format!(
                    "wiki `{}` uses unsupported sync backend `{}`",
                    wiki.id,
                    wiki.sync_backend.as_deref().unwrap_or_default()
                ))
            }
        });
    match result {
        Ok(report) => RegisteredSyncItemReport {
            wiki_id: wiki.id.clone(),
            path: wiki.path.clone(),
            report: Some(report),
            error: None,
        },
        Err(error) => RegisteredSyncItemReport {
            wiki_id: wiki.id.clone(),
            path: wiki.path.clone(),
            report: None,
            error: Some(error),
        },
    }
}

fn selection_label(selection: &RegisteredSyncSelection) -> String {
    match selection {
        RegisteredSyncSelection::Wiki(id) => format!("wiki:{id}"),
        RegisteredSyncSelection::Group(group) => format!("group:{group}"),
        RegisteredSyncSelection::All => "all".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{AddWikiRequest, UpdateWikiRequest};
    use crate::supervisor::SyncSupervisor;
    use std::fs;
    use std::path::Path;
    use std::process::Command;
    use tempfile::tempdir;
    use vulcan_app::sync_state::SyncStateStore;
    use vulcan_sync::{
        GitBranchSync, GitBranchSyncAction, GitRefName, SyncJob, SyncJobState, SyncJobTrigger,
    };

    fn git(path: &Path, arguments: &[&str]) {
        let status = Command::new("git")
            .current_dir(path)
            .args(arguments)
            .status()
            .expect("Git should launch");
        assert!(status.success(), "Git failed: {arguments:?}");
    }

    #[test]
    fn verbose_execution_line_reports_wiki_triggers_and_state() {
        let job = |wiki_id: Option<&str>, state: SyncJobState| SupervisedSyncJob {
            job: SyncJob {
                version: vulcan_sync::SYNC_CONTRACT_VERSION,
                id: "job-1".to_string(),
                wiki_id: wiki_id.map(str::to_string),
                backend: "git".to_string(),
                vault: Path::new("/vault").to_path_buf(),
                trigger: SyncJobTrigger::RemoteNotification,
                state,
                status: None,
                error: None,
            },
            triggers: vec![SyncJobTrigger::RemoteNotification],
            watch: None,
        };
        let line = format_sync_execution(&DaemonSyncExecution {
            job: job(Some("alpha"), SyncJobState::Succeeded),
            report: None,
        });
        assert!(line.contains("alpha"), "line should name the wiki: {line}");
        assert!(
            line.contains("RemoteNotification"),
            "line should name the trigger: {line}"
        );
        assert!(
            line.contains("Succeeded"),
            "line should name the state: {line}"
        );
        let anonymous = format_sync_execution(&DaemonSyncExecution {
            job: job(None, SyncJobState::Failed),
            report: None,
        });
        assert!(
            anonymous.contains("<unregistered>"),
            "line should mark missing wiki identity: {anonymous}"
        );

        let mut watched = job(Some("alpha"), SyncJobState::Succeeded);
        watched.watch = Some(crate::supervisor::SyncWatchMetadata {
            event_count: 3,
            untagged_events: 1,
            paths: vec!["Notes/todo.md".to_string()],
            self_generated_transactions: Vec::new(),
            safety_rescan: true,
            watcher_errors: vec!["polling watcher: access denied".to_string()],
        });
        let with_watch = format_sync_execution(&DaemonSyncExecution {
            job: watched,
            report: None,
        });
        for fragment in [
            "watch events=3",
            "untagged=1",
            "paths=1",
            "rescan=true",
            "errors=1",
            "access denied",
        ] {
            assert!(
                with_watch.contains(fragment),
                "line should explain the watch trigger ({fragment}): {with_watch}"
            );
        }
    }

    fn branch_lane_for_test(
        action: GitBranchSyncAction,
        detail: Option<&str>,
        pushed: bool,
        push_detail: Option<&str>,
    ) -> GitBranchSync {
        GitBranchSync {
            branch: GitRefName::parse("refs/heads/main").expect("branch"),
            remote: None,
            upstream: None,
            tracking: None,
            before: None,
            after: None,
            action,
            detail: detail.map(str::to_string),
            pushed,
            push_detail: push_detail.map(str::to_string),
        }
    }

    #[test]
    fn verbose_branch_summary_names_action_and_publication() {
        assert_eq!(format_branch_summary(None), "");
        let pushed = branch_lane_for_test(GitBranchSyncAction::FastForwarded, None, true, None);
        assert_eq!(
            format_branch_summary(Some(&pushed)),
            " branch `main`: FastForwarded, pushed"
        );
        let quiet = branch_lane_for_test(GitBranchSyncAction::UpToDate, None, false, None);
        assert_eq!(
            format_branch_summary(Some(&quiet)),
            " branch `main`: UpToDate"
        );
    }

    #[test]
    fn branch_diagnostic_surfaces_failures_only() {
        assert_eq!(
            format_branch_diagnostic(
                Some("alpha"),
                &branch_lane_for_test(
                    GitBranchSyncAction::Failed,
                    Some("unsupported pull.ff value `bogus`"),
                    false,
                    None,
                ),
            ),
            Some(
                "branch `main` (wiki `alpha`) failed: unsupported pull.ff value `bogus`"
                    .to_string()
            )
        );
        assert_eq!(
            format_branch_diagnostic(
                None,
                &branch_lane_for_test(
                    GitBranchSyncAction::Merged,
                    None,
                    false,
                    Some("remote advanced first"),
                ),
            ),
            Some(
                "branch `main` (wiki `<unregistered>`) push failed: remote advanced first"
                    .to_string()
            )
        );
        assert_eq!(
            format_branch_diagnostic(
                Some("alpha"),
                &branch_lane_for_test(GitBranchSyncAction::Merged, None, true, None),
            ),
            None
        );
        assert_eq!(
            format_branch_diagnostic(
                Some("alpha"),
                &branch_lane_for_test(GitBranchSyncAction::UpToDate, None, false, None),
            ),
            None
        );
    }

    #[test]
    fn only_remote_notification_jobs_select_fetch_first_observation() {
        assert_eq!(
            remote_observation_for_triggers(&[SyncJobTrigger::RemoteNotification]),
            GitRemoteObservation::Fetch
        );
        assert_eq!(
            remote_observation_for_triggers(&[SyncJobTrigger::Poll, SyncJobTrigger::Watch]),
            GitRemoteObservation::Query
        );
        assert_eq!(
            remote_observation_for_triggers(&[
                SyncJobTrigger::Watch,
                SyncJobTrigger::RemoteNotification,
            ]),
            GitRemoteObservation::Fetch
        );
    }

    #[test]
    fn registered_platform_profile_overrides_the_callers_native_default() {
        let temporary = tempdir().expect("temporary directory");
        let path = temporary.path().join("wiki");
        fs::create_dir(&path).expect("wiki directory");
        let registry = WikiRegistry::at(temporary.path().join("daemon.toml"));
        let wiki = registry
            .add(
                &AddWikiRequest {
                    id: WikiId::parse("mobile").expect("wiki ID"),
                    path,
                    groups: Vec::new(),
                    git_dir: None,
                    permissions_profile: None,
                    sync_backend: Some("git".to_string()),
                    platform_profile: Some("android_shared".to_string()),
                },
                false,
            )
            .expect("registration");

        let effective = options_for_registration(&GitSyncOptions::default(), &wiki)
            .expect("registered options");

        assert_eq!(effective.platform, GitPlatformProfile::AndroidShared);
    }

    #[test]
    fn selection_is_sorted_and_empty_groups_are_explicit() {
        let temporary = tempdir().expect("temporary directory");
        let first = temporary.path().join("first");
        let second = temporary.path().join("second");
        fs::create_dir(&first).expect("first wiki");
        fs::create_dir(&second).expect("second wiki");
        let registry = WikiRegistry::at(temporary.path().join("daemon.toml"));
        for (id, path, groups) in [
            ("work", second, vec!["team".to_string()]),
            ("personal", first, vec!["daily".to_string()]),
        ] {
            registry
                .add(
                    &AddWikiRequest {
                        id: WikiId::parse(id).expect("valid ID"),
                        path,
                        groups,
                        git_dir: None,
                        permissions_profile: None,
                        sync_backend: Some("git".to_string()),
                        platform_profile: None,
                    },
                    false,
                )
                .expect("register wiki");
        }

        let all = select_wikis(&registry, &RegisteredSyncSelection::All).expect("select all");
        assert_eq!(all[0].id.as_str(), "personal");
        assert_eq!(all[1].id.as_str(), "work");
        let daily = select_wikis(
            &registry,
            &RegisteredSyncSelection::Group("daily".to_string()),
        )
        .expect("select group");
        assert_eq!(daily.len(), 1);
        assert!(matches!(
            select_wikis(
                &registry,
                &RegisteredSyncSelection::Group("missing".to_string())
            ),
            Err(RegisteredSyncError::EmptyGroup(_))
        ));
    }

    #[test]
    fn registered_selection_enqueues_one_durable_aggregate_with_independent_children() {
        let temporary = tempdir().expect("temporary directory");
        let registry = WikiRegistry::at(temporary.path().join("daemon.toml"));
        for id in ["beta", "alpha"] {
            let path = temporary.path().join(id);
            fs::create_dir(&path).expect("wiki directory");
            registry
                .add(
                    &AddWikiRequest {
                        id: WikiId::parse(id).expect("wiki ID"),
                        path,
                        groups: vec!["team".to_string()],
                        git_dir: None,
                        permissions_profile: None,
                        sync_backend: Some("git".to_string()),
                        platform_profile: None,
                    },
                    false,
                )
                .expect("register wiki");
        }
        let jobs_path = temporary.path().join("jobs.json");
        let supervisor = SyncSupervisor::at(&jobs_path).expect("supervisor");
        let first = enqueue_registered_wikis(
            &registry,
            &supervisor,
            &RegisteredSyncSelection::Group("team".to_string()),
            "credential-a",
            "request-1",
        )
        .expect("enqueue selection");
        assert!(!first.replay);
        assert_eq!(first.aggregate.selection, "group:team");
        assert_eq!(first.aggregate.total, 2);
        assert_eq!(first.aggregate.children[0].wiki_id, "alpha");
        assert_eq!(supervisor.list().expect("child jobs").len(), 2);

        drop(supervisor);
        let restarted = SyncSupervisor::at(jobs_path).expect("restart");
        let replay = enqueue_registered_wikis(
            &registry,
            &restarted,
            &RegisteredSyncSelection::Group("team".to_string()),
            "credential-a",
            "request-1",
        )
        .expect("replay selection");
        assert!(replay.replay);
        assert_eq!(replay.aggregate.id, first.aggregate.id);
    }

    #[test]
    fn supervisor_executes_the_shared_finite_transaction_and_retains_status() {
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
        fs::create_dir(&vault).expect("vault");
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
        fs::write(vault.join("Home.md"), "home\n").expect("note");
        git(&vault, &["add", "Home.md"]);
        git(&vault, &["commit", "--quiet", "-m", "initial"]);
        let registry = WikiRegistry::at(temporary.path().join("daemon.toml"));
        registry
            .add(
                &AddWikiRequest {
                    id: WikiId::parse("alpha").expect("ID"),
                    path: vault.clone(),
                    groups: Vec::new(),
                    git_dir: None,
                    permissions_profile: None,
                    sync_backend: Some("git".to_string()),
                    platform_profile: None,
                },
                false,
            )
            .expect("registration");
        let jobs_path = temporary.path().join("jobs.json");
        let supervisor = SyncSupervisor::at(&jobs_path).expect("supervisor");
        let enqueued = supervisor
            .enqueue("alpha", &vault, SyncJobTrigger::Manual)
            .expect("enqueue");
        let state_store = SyncStateStore::at(temporary.path().join("sync-state"));
        let engine = GitCliEngine::default();

        let execution = execute_next_sync_job_with_state_store_and_engine(
            &supervisor,
            &registry,
            &GitSyncOptions::default(),
            &state_store,
            &engine,
        )
        .expect("execute")
        .expect("job");

        assert_eq!(execution.job.job.id, enqueued.job.job.id);
        assert_eq!(execution.job.job.state, SyncJobState::Succeeded);
        assert_eq!(
            execution.report.as_ref().expect("report").sync.outcome,
            GitSyncOutcome::Bootstrapped
        );
        assert_eq!(
            execution.job.job.status.as_ref().expect("status").state,
            SyncState::Clean
        );
        drop(supervisor);
        assert_eq!(
            SyncSupervisor::at(jobs_path)
                .expect("reloaded supervisor")
                .get(&enqueued.job.job.id)
                .expect("get")
                .expect("persisted job")
                .job
                .state,
            SyncJobState::Succeeded
        );
    }

    #[test]
    fn paused_registration_skips_automatic_jobs_before_git_discovery() {
        let temporary = tempdir().expect("temporary directory");
        let vault = temporary.path().join("vault");
        fs::create_dir(&vault).expect("vault");
        let registry = WikiRegistry::at(temporary.path().join("daemon.toml"));
        let wiki = WikiId::parse("alpha").expect("ID");
        registry
            .add(
                &AddWikiRequest {
                    id: wiki.clone(),
                    path: vault.clone(),
                    groups: Vec::new(),
                    git_dir: None,
                    permissions_profile: None,
                    sync_backend: Some("git".to_string()),
                    platform_profile: None,
                },
                false,
            )
            .expect("registration");
        registry
            .update(
                &wiki,
                &UpdateWikiRequest {
                    groups_to_add: Vec::new(),
                    groups_to_remove: Vec::new(),
                    permissions_profile: None,
                    sync_paused: Some(true),
                },
                false,
            )
            .expect("pause");
        let supervisor =
            SyncSupervisor::at(temporary.path().join("jobs.json")).expect("supervisor");
        supervisor
            .enqueue("alpha", &vault, SyncJobTrigger::Watch)
            .expect("enqueue");

        let execution = execute_next_sync_job_with_state_store(
            &supervisor,
            &registry,
            &GitSyncOptions::default(),
            &SyncStateStore::at(temporary.path().join("sync-state")),
        )
        .expect("execute")
        .expect("job");

        assert_eq!(execution.job.job.state, SyncJobState::Paused);
        assert!(execution.report.is_none());
        assert!(!vault.join(".git").exists());
    }
}
