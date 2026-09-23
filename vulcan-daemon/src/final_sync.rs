//! Bounded final synchronization before graceful daemon termination.

use crate::registry::WikiRegistry;
use crate::shutdown::ShutdownSignal;
use crate::supervisor::{SupervisedSyncJob, SupervisorError, SyncSupervisor};
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};
use vulcan_sync::{SyncJobState, SyncJobTrigger};

pub const FINAL_SYNC_GRACE_PERIOD: Duration = Duration::from_secs(30);
const FINAL_SYNC_POLL_INTERVAL: Duration = Duration::from_millis(25);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinalSyncSummary {
    pub requested: usize,
    pub completed: usize,
    pub timed_out: usize,
}

pub async fn run_final_sync(
    registry: &WikiRegistry,
    supervisor: &Arc<SyncSupervisor>,
    grace_period: Duration,
) -> Result<FinalSyncSummary, SupervisorError> {
    let registrations = registry
        .load()
        .map_err(|error| SupervisorError::InvalidState(error.to_string()))?;
    let mut pending = BTreeSet::new();
    for wiki in registrations.vaults.into_iter().filter(|wiki| {
        !wiki.sync_paused
            && wiki
                .sync_backend
                .as_deref()
                .is_none_or(|backend| backend == "git")
    }) {
        let report = supervisor.enqueue(wiki.id.as_str(), &wiki.path, SyncJobTrigger::Shutdown)?;
        pending.insert(report.job.job.id);
    }
    let requested = pending.len();
    let deadline = Instant::now() + grace_period;
    while !pending.is_empty() && Instant::now() < deadline {
        let mut completed = Vec::new();
        for id in &pending {
            if supervisor.get(id)?.is_none_or(|job| is_terminal(&job)) {
                completed.push(id.clone());
            }
        }
        for id in completed {
            pending.remove(&id);
        }
        if !pending.is_empty() {
            tokio::time::sleep(
                FINAL_SYNC_POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())),
            )
            .await;
        }
    }
    let timed_out = pending.len();
    for id in pending {
        let _ = supervisor.cancel(&id);
    }
    Ok(FinalSyncSummary {
        requested,
        completed: requested.saturating_sub(timed_out),
        timed_out,
    })
}

pub async fn run_final_sync_and_cancel(
    registry: &WikiRegistry,
    supervisor: &Arc<SyncSupervisor>,
    shutdown: &ShutdownSignal,
) {
    match run_final_sync(registry, supervisor, FINAL_SYNC_GRACE_PERIOD).await {
        Ok(summary) if summary.timed_out > 0 => eprintln!(
            "level=warning event=daemon_final_sync_timeout requested={} completed={} timed_out={}",
            summary.requested, summary.completed, summary.timed_out
        ),
        Ok(_) => {}
        Err(error) => eprintln!("level=warning event=daemon_final_sync_failed; {error}"),
    }
    shutdown.cancel();
}

fn is_terminal(job: &SupervisedSyncJob) -> bool {
    !matches!(job.job.state, SyncJobState::Queued | SyncJobState::Running)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{AddWikiRequest, UpdateWikiRequest, WikiId};
    use tempfile::tempdir;

    #[tokio::test]
    async fn final_sync_selects_active_git_wikis_and_cancels_at_deadline() {
        let temporary = tempdir().expect("temporary directory");
        let registry = WikiRegistry::at(temporary.path().join("daemon.toml"));
        for (id, backend, paused) in [
            ("active", Some("git"), false),
            ("implicit", None, false),
            ("paused", Some("git"), true),
            ("other", Some("cloud"), false),
        ] {
            let wiki = WikiId::parse(id).expect("wiki ID");
            let path = temporary.path().join(id);
            std::fs::create_dir(&path).expect("vault directory");
            registry
                .add(
                    &AddWikiRequest {
                        profile: None,
                        id: wiki.clone(),
                        path,
                        groups: Vec::new(),
                        git_dir: None,
                        permissions_profile: None,
                        sync_backend: backend.map(str::to_string),
                        platform_profile: None,
                    },
                    false,
                )
                .expect("register wiki");
            if paused {
                registry
                    .update(
                        &wiki,
                        &UpdateWikiRequest {
                            profile: None,
                            groups_to_add: Vec::new(),
                            groups_to_remove: Vec::new(),
                            permissions_profile: None,
                            sync_paused: Some(true),
                        },
                        false,
                    )
                    .expect("pause wiki");
            }
        }
        let supervisor =
            Arc::new(SyncSupervisor::at(temporary.path().join("jobs.json")).expect("supervisor"));
        let summary = run_final_sync(&registry, &supervisor, Duration::ZERO)
            .await
            .expect("final sync");

        assert_eq!(summary.requested, 2);
        assert_eq!(summary.completed, 0);
        assert_eq!(summary.timed_out, 2);
        let jobs = supervisor.list().expect("jobs");
        assert_eq!(jobs.len(), 2);
        assert!(jobs.iter().all(|job| {
            job.triggers.contains(&SyncJobTrigger::Shutdown)
                && job.job.state == SyncJobState::Cancelled
        }));
    }
}
