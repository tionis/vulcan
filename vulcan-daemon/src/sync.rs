//! Registry-aware finite synchronization orchestration.

use crate::registry::{RegistryError, WikiId, WikiRegistration, WikiRegistry};
use crate::supervisor::{ClaimedSyncJob, SupervisedSyncJob, SupervisorError, SyncSupervisor};
use serde::Serialize;
use std::error::Error;
use std::fmt::{Display, Formatter};
use vulcan_app::sync::{
    sync_git_vault, sync_git_vault_with_observer, GitSyncObserverError, GitSyncOptions,
    GitSyncOutcome, GitSyncPhase, GitSyncProgress, VaultSyncReport,
};
use vulcan_app::sync_state::SyncStateStore;
use vulcan_core::{
    resolve_permission_profile, PermissionGuard, ProfilePermissionGuard, VaultPaths,
};
use vulcan_sync::{
    GitSyncObserver, SyncError, SyncErrorCategory, SyncJobState, SyncState, SyncStatus,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DaemonSyncExecution {
    pub job: SupervisedSyncJob,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report: Option<VaultSyncReport>,
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
    let Some(claimed) = supervisor.claim_next()? else {
        return Ok(None);
    };
    Ok(Some(execute_claimed_job(
        supervisor,
        registry,
        options,
        state_store,
        &claimed,
    )?))
}

fn execute_claimed_job(
    supervisor: &SyncSupervisor,
    registry: &WikiRegistry,
    options: &GitSyncOptions,
    state_store: &SyncStateStore,
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
    let mut observer = SupervisorProgressObserver {
        supervisor,
        job_id: &id,
        vault: &registration.path,
    };
    match sync_git_vault_with_observer(
        &paths,
        options,
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
    if registration.path != claimed.job.job.vault {
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
            GitSyncPhase::Preparing | GitSyncPhase::Capturing => SyncState::CapturePending,
            GitSyncPhase::Captured | GitSyncPhase::Pushing => SyncState::CapturedUnpushed,
            GitSyncPhase::Fetching => SyncState::Fetching,
            GitSyncPhase::Merging => SyncState::Merging,
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
    EmptyGroup(String),
}

impl Display for RegisteredSyncError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Registry(error) => Display::fmt(error, formatter),
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
            Self::EmptyGroup(_) => None,
        }
    }
}

impl From<RegistryError> for RegisteredSyncError {
    fn from(error: RegistryError) -> Self {
        Self::Registry(error)
    }
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
                sync_git_vault(&paths, options).map_err(|error| error.to_string())
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
    use vulcan_sync::{SyncJobState, SyncJobTrigger};

    fn git(path: &Path, arguments: &[&str]) {
        let status = Command::new("git")
            .current_dir(path)
            .args(arguments)
            .status()
            .expect("Git should launch");
        assert!(status.success(), "Git failed: {arguments:?}");
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

        let execution = execute_next_sync_job_with_state_store(
            &supervisor,
            &registry,
            &GitSyncOptions::default(),
            &state_store,
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
