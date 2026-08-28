//! Reconstructed per-wiki synchronization state for daemon projections.

use crate::registry::{RegistryError, WikiId, WikiRegistration, WikiRegistry};
use crate::supervisor::{SupervisedSyncJob, SupervisorError, SyncSupervisor};
use serde::Serialize;
use std::error::Error;
use std::fmt::{Display, Formatter};
use vulcan_app::sync_conflicts::list_sync_conflicts_with_state_store;
use vulcan_app::sync_state::{repository_state_key, SyncJournal, SyncJournalPhase, SyncStateStore};
use vulcan_core::VaultPaths;
use vulcan_sync::{SyncErrorCategory, SyncJobState, SyncJobTrigger, SyncState, SyncStatus};

pub const DAEMON_SYNC_STATUS_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DaemonSyncStatusSource {
    Job,
    Journal,
    ApplyMarker,
    Conflict,
    Registration,
    Idle,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DaemonWikiSyncStatus {
    pub version: u32,
    pub wiki_id: String,
    pub paused: bool,
    pub source: DaemonSyncStatusSource,
    pub recovery_required: bool,
    #[serde(flatten)]
    pub status: SyncStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transaction_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job: Option<SupervisedSyncJob>,
}

#[derive(Debug)]
pub enum DaemonSyncStatusError {
    Registry(RegistryError),
    Supervisor(SupervisorError),
    State(String),
}

impl Display for DaemonSyncStatusError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Registry(error) => Display::fmt(error, formatter),
            Self::Supervisor(error) => Display::fmt(error, formatter),
            Self::State(detail) => formatter.write_str(detail),
        }
    }
}

impl Error for DaemonSyncStatusError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Registry(error) => Some(error),
            Self::Supervisor(error) => Some(error),
            Self::State(_) => None,
        }
    }
}

impl From<RegistryError> for DaemonSyncStatusError {
    fn from(error: RegistryError) -> Self {
        Self::Registry(error)
    }
}

impl From<SupervisorError> for DaemonSyncStatusError {
    fn from(error: SupervisorError) -> Self {
        Self::Supervisor(error)
    }
}

pub fn wiki_sync_status(
    registry: &WikiRegistry,
    supervisor: &SyncSupervisor,
    state_store: &SyncStateStore,
    wiki_id: &WikiId,
) -> Result<DaemonWikiSyncStatus, DaemonSyncStatusError> {
    let registration = registry
        .load()?
        .vaults
        .into_iter()
        .find(|registration| &registration.id == wiki_id)
        .ok_or_else(|| RegistryError::UnknownWiki(wiki_id.clone()))?;
    reconstruct_sync_status(&registration, supervisor, state_store)
}

fn reconstruct_sync_status(
    registration: &WikiRegistration,
    supervisor: &SyncSupervisor,
    state_store: &SyncStateStore,
) -> Result<DaemonWikiSyncStatus, DaemonSyncStatusError> {
    let jobs = supervisor.list()?;
    let relevant_jobs = jobs
        .iter()
        .filter(|job| job.job.wiki_id.as_deref() == Some(registration.id.as_str()))
        .collect::<Vec<_>>();
    if let Some(job) = relevant_jobs
        .iter()
        .rev()
        .find(|job| matches!(job.job.state, SyncJobState::Queued | SyncJobState::Running))
    {
        return Ok(report_from_active_job(registration, job));
    }

    let repository_key = repository_state_key(&registration.path);
    let journal = state_store
        .load(&repository_key)
        .map_err(|error| DaemonSyncStatusError::State(error.to_string()))?;
    if let Some(journal) = &journal {
        if let Some(report) = report_from_apply_marker(registration, state_store, journal) {
            return Ok(report);
        }
        return Ok(report_from_journal(registration, journal));
    }

    let conflicts =
        list_sync_conflicts_with_state_store(&VaultPaths::new(&registration.path), state_store)
            .map_err(|error| DaemonSyncStatusError::State(error.to_string()))?;
    if conflicts.count > 0 {
        return Ok(base_report(
            registration,
            DaemonSyncStatusSource::Conflict,
            SyncState::Conflicted,
            conflicts.count,
            Some("unresolved preserved synchronization conflicts".to_string()),
        ));
    }
    if registration.sync_paused {
        return Ok(base_report(
            registration,
            DaemonSyncStatusSource::Registration,
            SyncState::Paused,
            0,
            Some("automatic synchronization is paused".to_string()),
        ));
    }
    if let Some(job) = relevant_jobs.last() {
        return Ok(report_from_terminal_job(registration, job));
    }
    Ok(base_report(
        registration,
        DaemonSyncStatusSource::Idle,
        SyncState::Clean,
        0,
        None,
    ))
}

fn report_from_active_job(
    registration: &WikiRegistration,
    job: &SupervisedSyncJob,
) -> DaemonWikiSyncStatus {
    let status = job.job.status.clone().unwrap_or_else(|| SyncStatus {
        state: if job.triggers.contains(&SyncJobTrigger::Watch)
            && !job.triggers.contains(&SyncJobTrigger::Recovery)
        {
            SyncState::Dirty
        } else {
            SyncState::CapturePending
        },
        backend: "git".to_string(),
        vault: registration.path.clone(),
        local_revision: None,
        remote_revision: None,
        accepted_revision: None,
        unresolved_conflicts: 0,
        detail: Some(if job.job.state == SyncJobState::Queued {
            "synchronization is queued".to_string()
        } else {
            "synchronization is running".to_string()
        }),
    });
    DaemonWikiSyncStatus {
        version: DAEMON_SYNC_STATUS_VERSION,
        wiki_id: registration.id.as_str().to_string(),
        paused: registration.sync_paused,
        source: DaemonSyncStatusSource::Job,
        recovery_required: job.triggers.contains(&SyncJobTrigger::Recovery),
        status,
        transaction_id: None,
        job: Some(job.clone()),
    }
}

fn report_from_apply_marker(
    registration: &WikiRegistration,
    state_store: &SyncStateStore,
    journal: &SyncJournal,
) -> Option<DaemonWikiSyncStatus> {
    let git_dir = journal.git_dir.as_deref()?;
    match state_store.load_apply_marker(git_dir) {
        Ok(Some(marker)) => Some(DaemonWikiSyncStatus {
            version: DAEMON_SYNC_STATUS_VERSION,
            wiki_id: registration.id.as_str().to_string(),
            paused: registration.sync_paused,
            source: DaemonSyncStatusSource::ApplyMarker,
            recovery_required: true,
            status: SyncStatus {
                state: SyncState::Applying,
                backend: "git".to_string(),
                vault: registration.path.clone(),
                local_revision: Some(marker.expected_revision),
                remote_revision: None,
                accepted_revision: Some(marker.accepted),
                unresolved_conflicts: 0,
                detail: Some("worktree application may have been interrupted".to_string()),
            },
            transaction_id: Some(marker.transaction_id.to_string().to_ascii_lowercase()),
            job: None,
        }),
        Ok(None) => None,
        Err(error) => Some(DaemonWikiSyncStatus {
            version: DAEMON_SYNC_STATUS_VERSION,
            wiki_id: registration.id.as_str().to_string(),
            paused: registration.sync_paused,
            source: DaemonSyncStatusSource::ApplyMarker,
            recovery_required: true,
            status: SyncStatus {
                state: SyncState::Error,
                backend: "git".to_string(),
                vault: registration.path.clone(),
                local_revision: journal.local_snapshot.clone(),
                remote_revision: None,
                accepted_revision: journal.accepted.clone(),
                unresolved_conflicts: 0,
                detail: Some(format!("cannot read sync apply marker: {error}")),
            },
            transaction_id: Some(journal.transaction_id.to_string().to_ascii_lowercase()),
            job: None,
        }),
    }
}

fn report_from_journal(
    registration: &WikiRegistration,
    journal: &SyncJournal,
) -> DaemonWikiSyncStatus {
    let state = match journal.phase {
        SyncJournalPhase::Preparing => SyncState::CapturePending,
        SyncJournalPhase::Capturing => SyncState::Capturing,
        SyncJournalPhase::Captured => SyncState::CapturedUnpushed,
        SyncJournalPhase::Fetching => SyncState::Fetching,
        SyncJournalPhase::Fetched => SyncState::Fetched,
        SyncJournalPhase::Merging => SyncState::Merging,
        SyncJournalPhase::Pushing => SyncState::Pushing,
        SyncJournalPhase::Applying | SyncJournalPhase::Verifying => SyncState::Applying,
        SyncJournalPhase::Conflicted => SyncState::Conflicted,
        SyncJournalPhase::Paused => SyncState::Paused,
        SyncJournalPhase::Error => SyncState::Error,
    };
    DaemonWikiSyncStatus {
        version: DAEMON_SYNC_STATUS_VERSION,
        wiki_id: registration.id.as_str().to_string(),
        paused: registration.sync_paused,
        source: DaemonSyncStatusSource::Journal,
        recovery_required: journal.phase.requires_recovery(),
        status: SyncStatus {
            state,
            backend: "git".to_string(),
            vault: registration.path.clone(),
            local_revision: journal.local_snapshot.clone(),
            remote_revision: None,
            accepted_revision: journal.accepted.clone(),
            unresolved_conflicts: usize::from(journal.phase == SyncJournalPhase::Conflicted),
            detail: journal.error.clone(),
        },
        transaction_id: Some(journal.transaction_id.to_string().to_ascii_lowercase()),
        job: None,
    }
}

fn report_from_terminal_job(
    registration: &WikiRegistration,
    job: &SupervisedSyncJob,
) -> DaemonWikiSyncStatus {
    let state = job.job.status.as_ref().map_or_else(
        || match job.job.error.as_ref().map(|error| error.category) {
            Some(SyncErrorCategory::Network | SyncErrorCategory::Authentication) => {
                SyncState::Offline
            }
            Some(_) => SyncState::Error,
            None if job.job.state == SyncJobState::Cancelled => SyncState::Paused,
            None => SyncState::Clean,
        },
        |status| status.state,
    );
    let mut status = job.job.status.clone().unwrap_or_else(|| SyncStatus {
        state,
        backend: "git".to_string(),
        vault: registration.path.clone(),
        local_revision: None,
        remote_revision: None,
        accepted_revision: None,
        unresolved_conflicts: 0,
        detail: job.job.error.as_ref().map(|error| error.message.clone()),
    });
    status.state = state;
    DaemonWikiSyncStatus {
        version: DAEMON_SYNC_STATUS_VERSION,
        wiki_id: registration.id.as_str().to_string(),
        paused: registration.sync_paused,
        source: DaemonSyncStatusSource::Job,
        recovery_required: job.job.error.as_ref().is_some_and(|error| error.retryable),
        status,
        transaction_id: None,
        job: Some(job.clone()),
    }
}

fn base_report(
    registration: &WikiRegistration,
    source: DaemonSyncStatusSource,
    state: SyncState,
    unresolved_conflicts: usize,
    detail: Option<String>,
) -> DaemonWikiSyncStatus {
    DaemonWikiSyncStatus {
        version: DAEMON_SYNC_STATUS_VERSION,
        wiki_id: registration.id.as_str().to_string(),
        paused: registration.sync_paused,
        source,
        recovery_required: false,
        status: SyncStatus {
            state,
            backend: "git".to_string(),
            vault: registration.path.clone(),
            local_revision: None,
            remote_revision: None,
            accepted_revision: None,
            unresolved_conflicts,
            detail,
        },
        transaction_id: None,
        job: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{AddWikiRequest, WikiId};
    use tempfile::tempdir;
    use vulcan_app::sync_state::SyncJournal;
    use vulcan_sync::{SyncError, SyncErrorCategory};

    fn setup() -> (
        tempfile::TempDir,
        WikiRegistry,
        SyncSupervisor,
        SyncStateStore,
        WikiId,
    ) {
        let temporary = tempdir().expect("temporary directory");
        let vault = temporary.path().join("vault");
        std::fs::create_dir(&vault).expect("vault directory");
        let registry = WikiRegistry::at(temporary.path().join("daemon.toml"));
        let id = WikiId::parse("alpha").expect("wiki id");
        registry
            .add(
                &AddWikiRequest {
                    id: id.clone(),
                    path: vault,
                    groups: Vec::new(),
                    git_dir: None,
                    permissions_profile: None,
                    sync_backend: Some("git".to_string()),
                    platform_profile: None,
                },
                false,
            )
            .expect("register wiki");
        let supervisor =
            SyncSupervisor::at(temporary.path().join("jobs.json")).expect("supervisor");
        let state_store = SyncStateStore::at(temporary.path().join("state"));
        (temporary, registry, supervisor, state_store, id)
    }

    #[test]
    fn queued_watch_work_reconstructs_as_dirty() {
        let (_temporary, registry, supervisor, state_store, id) = setup();
        let registration = registry.show(&id).expect("registration").registration;
        supervisor
            .enqueue(id.as_str(), &registration.path, SyncJobTrigger::Watch)
            .expect("enqueue watch");

        let report =
            wiki_sync_status(&registry, &supervisor, &state_store, &id).expect("sync status");
        assert_eq!(report.status.state, SyncState::Dirty);
        assert_eq!(report.source, DaemonSyncStatusSource::Job);
        assert!(report.job.is_some());
    }

    #[test]
    fn durable_journal_reconstructs_precise_phase_without_a_job() {
        let (_temporary, registry, supervisor, state_store, id) = setup();
        let registration = registry.show(&id).expect("registration").registration;
        let mut journal = SyncJournal::preparing(&registration.path, "origin", "refs/heads/live")
            .expect("journal");
        journal.phase = SyncJournalPhase::Fetched;
        state_store.save(&journal).expect("save journal");

        let report =
            wiki_sync_status(&registry, &supervisor, &state_store, &id).expect("sync status");
        assert_eq!(report.status.state, SyncState::Fetched);
        assert_eq!(report.source, DaemonSyncStatusSource::Journal);
        assert!(report.recovery_required);
        assert_eq!(
            report.transaction_id,
            Some(journal.transaction_id.to_string().to_ascii_lowercase())
        );
    }

    #[test]
    fn retryable_network_failure_reconstructs_as_offline() {
        let (_temporary, registry, supervisor, state_store, id) = setup();
        let registration = registry.show(&id).expect("registration").registration;
        let queued = supervisor
            .enqueue(id.as_str(), &registration.path, SyncJobTrigger::Manual)
            .expect("enqueue");
        supervisor.claim_next().expect("claim").expect("job");
        supervisor
            .complete(
                &queued.job.job.id,
                SyncJobState::Failed,
                None,
                Some(SyncError::new(
                    SyncErrorCategory::Network,
                    "remote unavailable",
                    true,
                )),
            )
            .expect("complete");

        let report =
            wiki_sync_status(&registry, &supervisor, &state_store, &id).expect("sync status");
        assert_eq!(report.status.state, SyncState::Offline);
        assert!(report.recovery_required);
        assert_eq!(report.status.detail.as_deref(), Some("remote unavailable"));
    }
}
