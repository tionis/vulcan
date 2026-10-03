//! Reconstructed per-wiki synchronization state for daemon projections.

use crate::registry::{RegistryError, WikiId, WikiRegistration, WikiRegistry};
use crate::supervisor::{SupervisedSyncJob, SupervisorError, SyncSupervisor};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::{Display, Formatter};
use vulcan_app::sync_conflicts::list_sync_conflicts_with_state_store;
use vulcan_app::sync_state::{repository_state_key, SyncJournal, SyncJournalPhase, SyncStateStore};
use vulcan_core::VaultPaths;
pub use vulcan_sync::SyncState;
use vulcan_sync::{SyncErrorCategory, SyncJobState, SyncJobTrigger, SyncStatus};

pub const DAEMON_SYNC_STATUS_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DaemonSyncStatusSource {
    Job,
    Journal,
    ApplyMarker,
    Conflict,
    Registration,
    Idle,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonWikiSyncStatus {
    pub version: u32,
    pub wiki_id: String,
    pub paused: bool,
    pub source: DaemonSyncStatusSource,
    pub recovery_required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_attempt_unix_ms: Option<u64>,
    #[serde(flatten)]
    pub status: SyncStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transaction_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
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
    let jobs = supervisor.list()?;
    SyncStatusInputs::new(&jobs).status(&registration, state_store)
}

/// Request-local borrowed history, grouped once without cloning job payloads.
pub(crate) struct SyncStatusInputs<'a> {
    jobs: BTreeMap<&'a str, Vec<&'a SupervisedSyncJob>>,
}

impl<'a> SyncStatusInputs<'a> {
    pub(crate) fn new(jobs: &'a [SupervisedSyncJob]) -> Self {
        let mut grouped = BTreeMap::<_, Vec<_>>::new();
        for job in jobs {
            if let Some(wiki_id) = job.job.wiki_id.as_deref() {
                grouped.entry(wiki_id).or_default().push(job);
            }
        }
        Self { jobs: grouped }
    }

    pub(crate) fn status(
        &self,
        registration: &WikiRegistration,
        state_store: &SyncStateStore,
    ) -> Result<DaemonWikiSyncStatus, DaemonSyncStatusError> {
        let jobs = self
            .jobs
            .get(registration.id.as_str())
            .map_or(&[][..], Vec::as_slice);
        let mut report = reconstruct_sync_status(registration, jobs, state_store)?;
        report.last_attempt_unix_ms = jobs
            .last()
            .and_then(|job| ulid::Ulid::from_string(&job.job.id).ok())
            .map(|id| id.timestamp_ms());
        Ok(report)
    }
}

fn reconstruct_sync_status(
    registration: &WikiRegistration,
    relevant_jobs: &[&SupervisedSyncJob],
    state_store: &SyncStateStore,
) -> Result<DaemonWikiSyncStatus, DaemonSyncStatusError> {
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
        last_attempt_unix_ms: None,
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
            last_attempt_unix_ms: None,
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
            last_attempt_unix_ms: None,
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
    let state = if journal.error.is_some() {
        SyncState::Error
    } else {
        match journal.phase {
            SyncJournalPhase::Preparing => SyncState::CapturePending,
            SyncJournalPhase::Capturing => SyncState::Capturing,
            SyncJournalPhase::Captured => SyncState::CapturedUnpushed,
            SyncJournalPhase::BackingUp | SyncJournalPhase::Pushing => SyncState::Pushing,
            SyncJournalPhase::Fetching => SyncState::Fetching,
            SyncJournalPhase::Fetched => SyncState::Fetched,
            SyncJournalPhase::Merging => SyncState::Merging,
            SyncJournalPhase::Applying | SyncJournalPhase::Verifying => SyncState::Applying,
            SyncJournalPhase::Conflicted => SyncState::Conflicted,
            SyncJournalPhase::Paused => SyncState::Paused,
            SyncJournalPhase::Error => SyncState::Error,
        }
    };
    DaemonWikiSyncStatus {
        version: DAEMON_SYNC_STATUS_VERSION,
        wiki_id: registration.id.as_str().to_string(),
        paused: registration.sync_paused,
        source: DaemonSyncStatusSource::Journal,
        recovery_required: journal.error.is_some() || journal.phase.requires_recovery(),
        last_attempt_unix_ms: None,
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
        last_attempt_unix_ms: None,
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
        last_attempt_unix_ms: None,
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
                    profile: None,
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

    fn assert_batched_matches(
        registry: &WikiRegistry,
        supervisor: &SyncSupervisor,
        state_store: &SyncStateStore,
        id: &WikiId,
        expected: DaemonSyncStatusSource,
    ) -> DaemonWikiSyncStatus {
        let registration = registry.show(id).unwrap().registration;
        let jobs = supervisor.list().unwrap();
        let batched = SyncStatusInputs::new(&jobs)
            .status(&registration, state_store)
            .unwrap();
        let single = wiki_sync_status(registry, supervisor, state_store, id).unwrap();
        assert_eq!(
            serde_json::to_value(&batched).unwrap(),
            serde_json::to_value(single).unwrap()
        );
        assert_eq!(batched.source, expected);
        batched
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn batched_status_preserves_jobs_recovery_conflict_and_error_precedence() {
        use vulcan_app::sync_state::SyncApplyMarker;
        let (temporary, registry, supervisor, state_store, id) = setup();
        let registration = registry.show(&id).unwrap().registration;
        assert_batched_matches(
            &registry,
            &supervisor,
            &state_store,
            &id,
            DaemonSyncStatusSource::Idle,
        );
        let queued = supervisor
            .enqueue(id.as_str(), &registration.path, SyncJobTrigger::Watch)
            .unwrap();
        assert_batched_matches(
            &registry,
            &supervisor,
            &state_store,
            &id,
            DaemonSyncStatusSource::Job,
        );
        supervisor.claim_next().unwrap().unwrap();
        let mut journal =
            SyncJournal::preparing(&registration.path, "origin", "refs/heads/live").unwrap();
        journal.phase = SyncJournalPhase::Fetched;
        let git_dir = temporary.path().join("private-git");
        std::fs::create_dir(&git_dir).unwrap();
        journal.git_dir = Some(git_dir.clone());
        journal.local_snapshot = Some("a".repeat(40));
        journal.accepted = Some("b".repeat(40));
        state_store.save(&journal).unwrap();
        state_store
            .save_apply_marker(&git_dir, &SyncApplyMarker::from_journal(&journal).unwrap())
            .unwrap();
        // Active work wins even over an interrupted application.
        assert_batched_matches(
            &registry,
            &supervisor,
            &state_store,
            &id,
            DaemonSyncStatusSource::Job,
        );
        supervisor
            .complete(
                &queued.job.job.id,
                SyncJobState::Failed,
                None,
                Some(SyncError::new(SyncErrorCategory::Network, "offline", true)),
            )
            .unwrap();
        let marker = assert_batched_matches(
            &registry,
            &supervisor,
            &state_store,
            &id,
            DaemonSyncStatusSource::ApplyMarker,
        );
        assert!(marker.recovery_required);
        assert!(marker.last_attempt_unix_ms.is_some());
        std::fs::write(git_dir.join("vulcan-sync/apply.json"), b"invalid").unwrap();
        let broken_marker = assert_batched_matches(
            &registry,
            &supervisor,
            &state_store,
            &id,
            DaemonSyncStatusSource::ApplyMarker,
        );
        assert_eq!(broken_marker.status.state, SyncState::Error);
        state_store.clear_apply_marker(&git_dir).unwrap();
        let journal_report = assert_batched_matches(
            &registry,
            &supervisor,
            &state_store,
            &id,
            DaemonSyncStatusSource::Journal,
        );
        assert_eq!(journal_report.status.state, SyncState::Fetched);
        state_store.clear(&journal.repository_key).unwrap();
        let failed = assert_batched_matches(
            &registry,
            &supervisor,
            &state_store,
            &id,
            DaemonSyncStatusSource::Job,
        );
        assert_eq!(failed.status.state, SyncState::Offline);
        let completed = supervisor
            .enqueue(id.as_str(), &registration.path, SyncJobTrigger::Manual)
            .unwrap();
        supervisor.claim_next().unwrap().unwrap();
        supervisor
            .complete(&completed.job.job.id, SyncJobState::Succeeded, None, None)
            .unwrap();
        assert_eq!(
            assert_batched_matches(
                &registry,
                &supervisor,
                &state_store,
                &id,
                DaemonSyncStatusSource::Job
            )
            .job
            .unwrap()
            .job
            .id,
            completed.job.job.id
        );

        // A durable legacy conflict fixture exercises the real conflict reader.
        let conflict_id = "c".repeat(32);
        let directory = state_store
            .root()
            .join(&journal.repository_key)
            .join("conflicts")
            .join(&conflict_id);
        std::fs::create_dir_all(&directory).unwrap();
        let conflict = serde_json::json!({
            "version": 1, "id": conflict_id, "repository_key": journal.repository_key,
            "work_tree": registration.path, "base_revision": "base", "local_revision": "local",
            "remote_revision": "remote", "scope": "paths", "policy_version": 1,
            "policy_hash": "policy", "preserved_base_ref": null,
            "preserved_local_ref": "refs/local", "preserved_remote_ref": "refs/remote",
            "paths": [{"path": "Home.md", "base": {"revision": "base"},
                "local": {"revision": "local"}, "remote": {"revision": "remote"}}],
            "diagnostics": "conflict"
        });
        std::fs::write(
            directory.join("record.json"),
            serde_json::to_vec(&conflict).unwrap(),
        )
        .unwrap();
        let conflicted = assert_batched_matches(
            &registry,
            &supervisor,
            &state_store,
            &id,
            DaemonSyncStatusSource::Conflict,
        );
        assert_eq!(conflicted.status.unresolved_conflicts, 1);
        std::fs::write(directory.join("record.json"), b"invalid").unwrap();
        let jobs = supervisor.list().unwrap();
        let batch_error = SyncStatusInputs::new(&jobs)
            .status(&registration, &state_store)
            .unwrap_err();
        assert_eq!(
            batch_error.to_string(),
            wiki_sync_status(&registry, &supervisor, &state_store, &id)
                .unwrap_err()
                .to_string()
        );
    }

    #[test]
    fn grouped_history_keeps_input_order_and_latest_attempt_even_with_an_older_active_job() {
        let (_temporary, registry, supervisor, state_store, id) = setup();
        let mut registration = registry.show(&id).unwrap().registration;
        let first = supervisor
            .enqueue(id.as_str(), &registration.path, SyncJobTrigger::Watch)
            .unwrap()
            .job;
        let mut unrelated = first.clone();
        unrelated.job.wiki_id = Some("other".to_string());
        let mut latest = first.clone();
        latest.job.id = "invalid-ulid".to_string();
        latest.job.state = SyncJobState::Succeeded;
        let jobs = [first.clone(), unrelated, latest];
        let inputs = SyncStatusInputs::new(&jobs);
        assert_eq!(inputs.jobs["alpha"].len(), 2);
        let active = inputs.status(&registration, &state_store).unwrap();
        assert_eq!(active.job.unwrap(), first);
        assert_eq!(active.last_attempt_unix_ms, None);
        registration.sync_paused = true;
        let empty = SyncStatusInputs::new(&[])
            .status(&registration, &state_store)
            .unwrap();
        assert_eq!(empty.source, DaemonSyncStatusSource::Registration);
        assert_eq!(empty.status.state, SyncState::Paused);
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
        assert!(report.last_attempt_unix_ms.is_some());
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
    fn failed_journal_is_an_error_instead_of_an_active_phase() {
        let (_temporary, registry, supervisor, state_store, id) = setup();
        let registration = registry.show(&id).expect("registration").registration;
        let mut journal = SyncJournal::preparing(&registration.path, "origin", "refs/heads/live")
            .expect("journal");
        journal.phase = SyncJournalPhase::Merging;
        journal.error = Some("git-write-tree failed".to_string());
        state_store.save(&journal).expect("save journal");

        let report =
            wiki_sync_status(&registry, &supervisor, &state_store, &id).expect("sync status");
        assert_eq!(report.status.state, SyncState::Error);
        assert_eq!(report.source, DaemonSyncStatusSource::Journal);
        assert!(report.recovery_required);
        assert_eq!(
            report.status.detail.as_deref(),
            Some("git-write-tree failed")
        );
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
