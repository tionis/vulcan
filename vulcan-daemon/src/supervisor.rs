//! Durable, transport-independent synchronization job supervision.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tempfile::NamedTempFile;
use ulid::Ulid;
use vulcan_sync::{
    SyncCancellationToken, SyncError, SyncJob, SyncJobState, SyncJobTrigger, SyncState, SyncStatus,
    SYNC_CONTRACT_VERSION,
};

pub const SYNC_SUPERVISOR_STATE_VERSION: u32 = 1;
const MAX_STATE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_RETAINED_JOBS: usize = 256;
const MAX_WATCH_PATHS: usize = 256;
const MAX_WATCH_TRANSACTIONS: usize = 16;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncWatchMetadata {
    pub event_count: usize,
    pub untagged_events: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub self_generated_transactions: Vec<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub safety_rescan: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub watcher_errors: Vec<String>,
}

#[allow(clippy::trivially_copy_pass_by_ref)]
const fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupervisedSyncJob {
    #[serde(flatten)]
    pub job: SyncJob,
    pub triggers: Vec<SyncJobTrigger>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watch: Option<SyncWatchMetadata>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PersistedSupervisorState {
    version: u32,
    jobs: Vec<SupervisedSyncJob>,
}

impl Default for PersistedSupervisorState {
    fn default() -> Self {
        Self {
            version: SYNC_SUPERVISOR_STATE_VERSION,
            jobs: Vec::new(),
        }
    }
}

#[derive(Debug)]
struct SupervisorInner {
    state: PersistedSupervisorState,
    queue: VecDeque<String>,
    cancellations: BTreeMap<String, SyncCancellationToken>,
}

#[derive(Debug)]
pub struct SyncSupervisor {
    state_path: PathBuf,
    inner: Mutex<SupervisorInner>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EnqueueSyncReport {
    pub job: SupervisedSyncJob,
    pub coalesced: bool,
}

#[derive(Debug, Clone)]
pub struct ClaimedSyncJob {
    pub job: SupervisedSyncJob,
    pub cancellation: SyncCancellationToken,
}

#[derive(Debug)]
pub enum SupervisorError {
    Io(std::io::Error),
    Json(serde_json::Error),
    InvalidState(String),
    UnknownJob(String),
    Poisoned,
}

impl Display for SupervisorError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => Display::fmt(error, formatter),
            Self::Json(error) => Display::fmt(error, formatter),
            Self::InvalidState(detail) => formatter.write_str(detail),
            Self::UnknownJob(id) => write!(formatter, "unknown synchronization job `{id}`"),
            Self::Poisoned => formatter.write_str("synchronization supervisor lock is poisoned"),
        }
    }
}

impl Error for SupervisorError {}

impl From<std::io::Error> for SupervisorError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for SupervisorError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

impl SyncSupervisor {
    pub fn user_default() -> Result<Self, SupervisorError> {
        let sync_state = vulcan_app::sync_state::SyncStateStore::user_default()
            .map_err(|error| SupervisorError::InvalidState(error.to_string()))?;
        Self::at(sync_state.root().join("daemon/jobs.json"))
    }

    pub fn at(state_path: impl Into<PathBuf>) -> Result<Self, SupervisorError> {
        let state_path = state_path.into();
        let mut state = load_state(&state_path)?;
        let mut queue = VecDeque::new();
        let mut cancellations = BTreeMap::new();
        for supervised in &mut state.jobs {
            if supervised.job.state == SyncJobState::Running {
                supervised.job.state = SyncJobState::Queued;
                supervised.job.status = Some(status_for(
                    &supervised.job,
                    SyncState::CapturePending,
                    Some("recovered after daemon restart".to_string()),
                ));
                push_trigger(&mut supervised.triggers, SyncJobTrigger::Recovery);
            }
            if supervised.job.state == SyncJobState::Queued {
                queue.push_back(supervised.job.id.clone());
                cancellations.insert(supervised.job.id.clone(), SyncCancellationToken::default());
            }
        }
        Ok(Self {
            state_path,
            inner: Mutex::new(SupervisorInner {
                state,
                queue,
                cancellations,
            }),
        })
    }

    pub fn enqueue(
        &self,
        wiki_id: impl Into<String>,
        vault: impl Into<PathBuf>,
        trigger: SyncJobTrigger,
    ) -> Result<EnqueueSyncReport, SupervisorError> {
        self.enqueue_inner(wiki_id.into(), vault.into(), trigger, None)
    }

    pub fn enqueue_watch(
        &self,
        wiki_id: impl Into<String>,
        vault: impl Into<PathBuf>,
        metadata: SyncWatchMetadata,
    ) -> Result<EnqueueSyncReport, SupervisorError> {
        let trigger = if metadata.safety_rescan {
            SyncJobTrigger::Recovery
        } else {
            SyncJobTrigger::Watch
        };
        self.enqueue_inner(wiki_id.into(), vault.into(), trigger, Some(metadata))
    }

    fn enqueue_inner(
        &self,
        wiki_id: String,
        vault: PathBuf,
        trigger: SyncJobTrigger,
        watch: Option<SyncWatchMetadata>,
    ) -> Result<EnqueueSyncReport, SupervisorError> {
        let mut inner = self.inner.lock().map_err(|_| SupervisorError::Poisoned)?;
        if let Some(existing) = inner.state.jobs.iter_mut().find(|candidate| {
            candidate.job.wiki_id.as_deref() == Some(wiki_id.as_str())
                && candidate.job.state == SyncJobState::Queued
        }) {
            push_trigger(&mut existing.triggers, trigger);
            merge_watch_metadata(&mut existing.watch, watch);
            let report = EnqueueSyncReport {
                job: existing.clone(),
                coalesced: true,
            };
            persist_state(&self.state_path, &inner.state)?;
            return Ok(report);
        }
        let id = Ulid::new().to_string().to_ascii_lowercase();
        let job = SyncJob {
            version: SYNC_CONTRACT_VERSION,
            id: id.clone(),
            wiki_id: Some(wiki_id),
            backend: "git".to_string(),
            vault,
            trigger,
            state: SyncJobState::Queued,
            status: None,
            error: None,
        };
        let supervised = SupervisedSyncJob {
            job,
            triggers: vec![trigger],
            watch,
        };
        inner.state.jobs.push(supervised.clone());
        inner.queue.push_back(id.clone());
        inner
            .cancellations
            .insert(id, SyncCancellationToken::default());
        trim_terminal_jobs(&mut inner.state.jobs);
        persist_state(&self.state_path, &inner.state)?;
        Ok(EnqueueSyncReport {
            job: supervised,
            coalesced: false,
        })
    }

    pub fn claim_next(&self) -> Result<Option<ClaimedSyncJob>, SupervisorError> {
        let mut inner = self.inner.lock().map_err(|_| SupervisorError::Poisoned)?;
        let mut deferred = VecDeque::new();
        let claimed = loop {
            let Some(id) = inner.queue.pop_front() else {
                break None;
            };
            let Some(index) = inner
                .state
                .jobs
                .iter()
                .position(|candidate| candidate.job.id == id)
            else {
                continue;
            };
            if inner.state.jobs[index].job.state != SyncJobState::Queued {
                continue;
            }
            let wiki_id = inner.state.jobs[index].job.wiki_id.clone();
            let wiki_running = inner.state.jobs.iter().any(|candidate| {
                candidate.job.wiki_id == wiki_id && candidate.job.state == SyncJobState::Running
            });
            if wiki_running {
                deferred.push_back(id);
                continue;
            }
            inner.state.jobs[index].job.state = SyncJobState::Running;
            let status = status_for(
                &inner.state.jobs[index].job,
                SyncState::CapturePending,
                None,
            );
            inner.state.jobs[index].job.status = Some(status);
            let job = inner.state.jobs[index].clone();
            let cancellation = inner.cancellations.entry(id).or_default().clone();
            break Some(ClaimedSyncJob { job, cancellation });
        };
        inner.queue.extend(deferred);
        persist_state(&self.state_path, &inner.state)?;
        Ok(claimed)
    }

    pub fn complete(
        &self,
        id: &str,
        state: SyncJobState,
        status: Option<SyncStatus>,
        error: Option<SyncError>,
    ) -> Result<SupervisedSyncJob, SupervisorError> {
        if matches!(state, SyncJobState::Queued | SyncJobState::Running) {
            return Err(SupervisorError::InvalidState(
                "completion requires a terminal job state".to_string(),
            ));
        }
        let mut inner = self.inner.lock().map_err(|_| SupervisorError::Poisoned)?;
        let job = inner
            .state
            .jobs
            .iter_mut()
            .find(|candidate| candidate.job.id == id)
            .ok_or_else(|| SupervisorError::UnknownJob(id.to_string()))?;
        job.job.state = state;
        job.job.status = status;
        job.job.error = error;
        let completed = job.clone();
        inner.cancellations.remove(id);
        persist_state(&self.state_path, &inner.state)?;
        Ok(completed)
    }

    pub fn update_running_status(
        &self,
        id: &str,
        status: SyncStatus,
    ) -> Result<SupervisedSyncJob, SupervisorError> {
        let mut inner = self.inner.lock().map_err(|_| SupervisorError::Poisoned)?;
        let job = inner
            .state
            .jobs
            .iter_mut()
            .find(|candidate| candidate.job.id == id)
            .ok_or_else(|| SupervisorError::UnknownJob(id.to_string()))?;
        if job.job.state != SyncJobState::Running {
            return Err(SupervisorError::InvalidState(format!(
                "synchronization job `{id}` is not running"
            )));
        }
        job.job.status = Some(status);
        let updated = job.clone();
        persist_state(&self.state_path, &inner.state)?;
        Ok(updated)
    }

    pub fn cancel(&self, id: &str) -> Result<SupervisedSyncJob, SupervisorError> {
        let mut inner = self.inner.lock().map_err(|_| SupervisorError::Poisoned)?;
        let index = inner
            .state
            .jobs
            .iter()
            .position(|candidate| candidate.job.id == id)
            .ok_or_else(|| SupervisorError::UnknownJob(id.to_string()))?;
        match inner.state.jobs[index].job.state {
            SyncJobState::Queued => {
                inner.state.jobs[index].job.state = SyncJobState::Cancelled;
                inner.cancellations.remove(id);
            }
            SyncJobState::Running => {
                if let Some(cancellation) = inner.cancellations.get(id) {
                    cancellation.cancel();
                }
            }
            _ => {}
        }
        let job = inner.state.jobs[index].clone();
        persist_state(&self.state_path, &inner.state)?;
        Ok(job)
    }

    pub fn get(&self, id: &str) -> Result<Option<SupervisedSyncJob>, SupervisorError> {
        let inner = self.inner.lock().map_err(|_| SupervisorError::Poisoned)?;
        Ok(inner
            .state
            .jobs
            .iter()
            .find(|candidate| candidate.job.id == id)
            .cloned())
    }

    pub fn list(&self) -> Result<Vec<SupervisedSyncJob>, SupervisorError> {
        let inner = self.inner.lock().map_err(|_| SupervisorError::Poisoned)?;
        Ok(inner.state.jobs.clone())
    }
}

fn status_for(job: &SyncJob, state: SyncState, detail: Option<String>) -> SyncStatus {
    SyncStatus {
        state,
        backend: job.backend.clone(),
        vault: job.vault.clone(),
        local_revision: None,
        remote_revision: None,
        accepted_revision: None,
        unresolved_conflicts: 0,
        detail,
    }
}

fn push_trigger(triggers: &mut Vec<SyncJobTrigger>, trigger: SyncJobTrigger) {
    if !triggers.contains(&trigger) {
        triggers.push(trigger);
    }
}

fn merge_watch_metadata(
    current: &mut Option<SyncWatchMetadata>,
    incoming: Option<SyncWatchMetadata>,
) {
    let Some(incoming) = incoming else {
        return;
    };
    let current = current.get_or_insert_with(SyncWatchMetadata::default);
    current.event_count = current.event_count.saturating_add(incoming.event_count);
    current.untagged_events = current
        .untagged_events
        .saturating_add(incoming.untagged_events);
    current.safety_rescan |= incoming.safety_rescan;
    merge_bounded(&mut current.paths, incoming.paths, MAX_WATCH_PATHS);
    merge_bounded(
        &mut current.self_generated_transactions,
        incoming.self_generated_transactions,
        MAX_WATCH_TRANSACTIONS,
    );
    merge_bounded(
        &mut current.watcher_errors,
        incoming.watcher_errors,
        MAX_WATCH_TRANSACTIONS,
    );
}

fn merge_bounded(current: &mut Vec<String>, incoming: Vec<String>, limit: usize) {
    current.extend(incoming);
    current.sort();
    current.dedup();
    current.truncate(limit);
}

fn trim_terminal_jobs(jobs: &mut Vec<SupervisedSyncJob>) {
    let mut terminal = jobs
        .iter()
        .filter(|job| !matches!(job.job.state, SyncJobState::Queued | SyncJobState::Running))
        .count();
    while terminal > MAX_RETAINED_JOBS {
        let Some(index) = jobs
            .iter()
            .position(|job| !matches!(job.job.state, SyncJobState::Queued | SyncJobState::Running))
        else {
            break;
        };
        jobs.remove(index);
        terminal -= 1;
    }
}

fn load_state(path: &Path) -> Result<PersistedSupervisorState, SupervisorError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(PersistedSupervisorState::default());
        }
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(SupervisorError::InvalidState(format!(
            "supervisor state at {} is not a regular file",
            path.display()
        )));
    }
    if metadata.len() > MAX_STATE_BYTES {
        return Err(SupervisorError::InvalidState(format!(
            "supervisor state exceeds the {MAX_STATE_BYTES} byte limit"
        )));
    }
    let state: PersistedSupervisorState = serde_json::from_slice(&fs::read(path)?)?;
    if state.version != SYNC_SUPERVISOR_STATE_VERSION {
        return Err(SupervisorError::InvalidState(format!(
            "unsupported supervisor state version {}",
            state.version
        )));
    }
    Ok(state)
}

fn persist_state(path: &Path, state: &PersistedSupervisorState) -> Result<(), SupervisorError> {
    let parent = path.parent().ok_or_else(|| {
        SupervisorError::InvalidState("supervisor state path has no parent".to_string())
    })?;
    fs::create_dir_all(parent)?;
    let bytes = serde_json::to_vec_pretty(state)?;
    if bytes.len() as u64 > MAX_STATE_BYTES {
        return Err(SupervisorError::InvalidState(format!(
            "supervisor state exceeds the {MAX_STATE_BYTES} byte limit"
        )));
    }
    let mut temporary = NamedTempFile::new_in(parent)?;
    temporary.write_all(&bytes)?;
    temporary.write_all(b"\n")?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn supervisor(root: &Path) -> SyncSupervisor {
        SyncSupervisor::at(root.join("jobs.json")).expect("supervisor")
    }

    #[test]
    fn queued_triggers_coalesce_and_different_wikis_can_run_together() {
        let temporary = tempdir().expect("temporary directory");
        let supervisor = supervisor(temporary.path());
        let first = supervisor
            .enqueue(
                "alpha",
                temporary.path().join("alpha"),
                SyncJobTrigger::Watch,
            )
            .expect("enqueue alpha");
        let coalesced = supervisor
            .enqueue(
                "alpha",
                temporary.path().join("alpha"),
                SyncJobTrigger::Poll,
            )
            .expect("coalesce alpha");
        assert!(coalesced.coalesced);
        assert_eq!(first.job.job.id, coalesced.job.job.id);
        assert_eq!(
            coalesced.job.triggers,
            vec![SyncJobTrigger::Watch, SyncJobTrigger::Poll]
        );
        supervisor
            .enqueue(
                "beta",
                temporary.path().join("beta"),
                SyncJobTrigger::Manual,
            )
            .expect("enqueue beta");

        let alpha = supervisor
            .claim_next()
            .expect("claim alpha")
            .expect("alpha job");
        let beta = supervisor
            .claim_next()
            .expect("claim beta")
            .expect("beta job");
        assert_ne!(alpha.job.job.wiki_id, beta.job.job.wiki_id);
        assert!(supervisor.claim_next().expect("empty queue").is_none());
    }

    #[test]
    fn trigger_during_running_job_creates_one_follow_up() {
        let temporary = tempdir().expect("temporary directory");
        let supervisor = supervisor(temporary.path());
        let initial = supervisor
            .enqueue("alpha", temporary.path(), SyncJobTrigger::Manual)
            .expect("initial");
        supervisor.claim_next().expect("claim").expect("job");
        let follow_up = supervisor
            .enqueue("alpha", temporary.path(), SyncJobTrigger::Watch)
            .expect("follow up");
        assert_ne!(initial.job.job.id, follow_up.job.job.id);
        assert!(supervisor.claim_next().expect("blocked").is_none());
        supervisor
            .complete(&initial.job.job.id, SyncJobState::Succeeded, None, None)
            .expect("complete");
        assert_eq!(
            supervisor
                .claim_next()
                .expect("claim follow up")
                .expect("follow up job")
                .job
                .job
                .id,
            follow_up.job.job.id
        );
    }

    #[test]
    fn restart_requeues_running_jobs_as_recovery() {
        let temporary = tempdir().expect("temporary directory");
        let state_path = temporary.path().join("jobs.json");
        let supervisor = SyncSupervisor::at(&state_path).expect("supervisor");
        supervisor
            .enqueue("alpha", temporary.path(), SyncJobTrigger::Watch)
            .expect("enqueue");
        let running = supervisor.claim_next().expect("claim").expect("job");
        drop(supervisor);

        let restarted = SyncSupervisor::at(&state_path).expect("restart");
        let recovered = restarted
            .claim_next()
            .expect("claim recovered")
            .expect("recovered job");
        assert_eq!(running.job.job.id, recovered.job.job.id);
        assert!(recovered.job.triggers.contains(&SyncJobTrigger::Recovery));
    }

    #[test]
    fn watch_metadata_coalesces_bounded_and_survives_restart() {
        let temporary = tempdir().expect("temporary directory");
        let state_path = temporary.path().join("jobs.json");
        let supervisor = SyncSupervisor::at(&state_path).expect("supervisor");
        let first = supervisor
            .enqueue_watch(
                "alpha",
                temporary.path(),
                SyncWatchMetadata {
                    event_count: 2,
                    untagged_events: 1,
                    paths: vec!["b.md".to_string(), "a.md".to_string()],
                    self_generated_transactions: vec!["tx-b".to_string()],
                    safety_rescan: false,
                    watcher_errors: Vec::new(),
                },
            )
            .expect("first watch batch");
        assert!(!first.coalesced);
        let second = supervisor
            .enqueue_watch(
                "alpha",
                temporary.path(),
                SyncWatchMetadata {
                    event_count: 3,
                    untagged_events: 2,
                    paths: vec!["a.md".to_string(), "c.md".to_string()],
                    self_generated_transactions: vec!["tx-a".to_string()],
                    safety_rescan: true,
                    watcher_errors: vec!["overflow".to_string()],
                },
            )
            .expect("coalesced watch batch");
        assert!(second.coalesced);
        assert!(second.job.triggers.contains(&SyncJobTrigger::Watch));
        assert!(second.job.triggers.contains(&SyncJobTrigger::Recovery));
        let metadata = second.job.watch.expect("watch metadata");
        assert_eq!(metadata.event_count, 5);
        assert_eq!(metadata.untagged_events, 3);
        assert_eq!(metadata.paths, vec!["a.md", "b.md", "c.md"]);
        assert_eq!(metadata.self_generated_transactions, vec!["tx-a", "tx-b"]);
        assert!(metadata.safety_rescan);
        drop(supervisor);

        let restarted = SyncSupervisor::at(&state_path).expect("restart");
        assert_eq!(
            restarted
                .list()
                .expect("jobs")
                .into_iter()
                .next()
                .expect("job")
                .watch,
            Some(metadata)
        );
    }

    #[test]
    fn cancellation_is_immediate_for_queued_and_cooperative_for_running_jobs() {
        let temporary = tempdir().expect("temporary directory");
        let supervisor = supervisor(temporary.path());
        let queued = supervisor
            .enqueue("alpha", temporary.path(), SyncJobTrigger::Manual)
            .expect("enqueue queued");
        assert_eq!(
            supervisor
                .cancel(&queued.job.job.id)
                .expect("cancel queued")
                .job
                .state,
            SyncJobState::Cancelled
        );
        let running = supervisor
            .enqueue("beta", temporary.path(), SyncJobTrigger::Manual)
            .expect("enqueue running");
        let claimed = supervisor.claim_next().expect("claim").expect("job");
        supervisor
            .cancel(&running.job.job.id)
            .expect("cancel running");
        assert!(claimed.cancellation.is_cancelled());
        assert_eq!(
            supervisor
                .get(&running.job.job.id)
                .expect("get")
                .expect("running")
                .job
                .state,
            SyncJobState::Running
        );
    }
}
