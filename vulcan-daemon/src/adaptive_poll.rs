//! Adaptive remote polling for Git-synced wikis.
//!
//! Each active wiki polls its remote on its own schedule. Activity (a local
//! edit or commit, a manual or resume sync, or a poll that found new
//! revisions) drops the wiki to its minimum interval; every poll that follows
//! doubles the interval up to the maximum. A busy wiki is checked every few
//! seconds and an idle one a few times an hour, with no forge-side setup.
//! The bounds come from the vault's `[sync] poll_min_seconds` and
//! `poll_max_seconds`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::supervisor::SupervisedSyncJob;
use vulcan_sync::{SyncJobState, SyncJobTrigger};

/// Minimum and maximum interval for one wiki.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PollBounds {
    pub min: Duration,
    pub max: Duration,
}

impl PollBounds {
    pub(crate) fn from_seconds((min, max): (u64, u64)) -> Self {
        let min = Duration::from_secs(min.max(1));
        Self {
            min,
            max: Duration::from_secs(max).max(min),
        }
    }
}

#[derive(Debug, Clone)]
struct Schedule {
    path: PathBuf,
    bounds: PollBounds,
    interval: Duration,
    next_due: Instant,
}

/// The revisions a completed job reported; a change between two polls means
/// another device published something.
type Revisions = (Option<String>, Option<String>, Option<String>);

#[derive(Debug, Default)]
pub(crate) struct AdaptivePolling {
    schedules: BTreeMap<String, Schedule>,
    observed_jobs: BTreeSet<String>,
    revisions: BTreeMap<String, Revisions>,
}

impl AdaptivePolling {
    /// Brings schedules in line with the active wikis. New wikis start at
    /// their minimum interval; `load_bounds` is consulted only for them.
    pub(crate) fn reconcile<I, F>(&mut self, active: I, now: Instant, mut load_bounds: F)
    where
        I: IntoIterator<Item = (String, PathBuf)>,
        F: FnMut(&PathBuf) -> PollBounds,
    {
        let mut seen = BTreeSet::new();
        for (id, path) in active {
            seen.insert(id.clone());
            match self.schedules.get_mut(&id) {
                Some(schedule) if schedule.path == path => {}
                _ => {
                    let bounds = load_bounds(&path);
                    self.schedules.insert(
                        id,
                        Schedule {
                            path,
                            bounds,
                            interval: bounds.min,
                            next_due: now + bounds.min,
                        },
                    );
                }
            }
        }
        self.schedules.retain(|id, _| seen.contains(id));
        self.revisions.retain(|id, _| seen.contains(id));
    }

    /// Looks at terminal jobs not seen before and resets the interval of
    /// every wiki that showed activity.
    pub(crate) fn observe_jobs(&mut self, jobs: &[SupervisedSyncJob], now: Instant) {
        let mut retained = BTreeSet::new();
        for job in jobs {
            retained.insert(job.job.id.clone());
            if matches!(job.job.state, SyncJobState::Queued | SyncJobState::Running)
                || self.observed_jobs.contains(&job.job.id)
            {
                continue;
            }
            self.observed_jobs.insert(job.job.id.clone());
            let Some(wiki) = job.job.wiki_id.clone() else {
                continue;
            };
            let local_activity = job
                .triggers
                .iter()
                .chain(std::iter::once(&job.job.trigger))
                .any(|trigger| *trigger != SyncJobTrigger::Poll);
            let remote_activity = job.job.status.as_ref().is_some_and(|status| {
                let revisions = (
                    status.local_revision.clone(),
                    status.remote_revision.clone(),
                    status.accepted_revision.clone(),
                );
                let changed = self
                    .revisions
                    .get(&wiki)
                    .is_some_and(|previous| *previous != revisions);
                self.revisions.insert(wiki.clone(), revisions);
                changed
            });
            if local_activity || remote_activity {
                self.mark_active(&wiki, now);
            }
        }
        // Retained jobs are bounded by the supervisor; forget evicted ones.
        self.observed_jobs.retain(|id| retained.contains(id));
    }

    /// Drops a wiki to its minimum interval.
    pub(crate) fn mark_active(&mut self, wiki: &str, now: Instant) {
        if let Some(schedule) = self.schedules.get_mut(wiki) {
            schedule.interval = schedule.bounds.min;
            schedule.next_due = schedule.next_due.min(now + schedule.bounds.min);
        }
    }

    /// Returns the wikis whose poll is due and backs each off: the next poll
    /// waits twice as long, up to the maximum. `load_bounds` refreshes the
    /// bounds so configuration changes apply from the next poll.
    pub(crate) fn take_due<F>(&mut self, now: Instant, mut load_bounds: F) -> Vec<(String, PathBuf)>
    where
        F: FnMut(&PathBuf) -> PollBounds,
    {
        let mut due = Vec::new();
        for (id, schedule) in &mut self.schedules {
            if schedule.next_due > now {
                continue;
            }
            schedule.bounds = load_bounds(&schedule.path);
            schedule.interval = schedule
                .interval
                .saturating_mul(2)
                .clamp(schedule.bounds.min, schedule.bounds.max);
            schedule.next_due = now + schedule.interval;
            due.push((id.clone(), schedule.path.clone()));
        }
        due
    }

    pub(crate) fn next_due(&self) -> Option<Instant> {
        self.schedules
            .values()
            .map(|schedule| schedule.next_due)
            .min()
    }

    #[cfg(test)]
    fn interval(&self, wiki: &str) -> Option<Duration> {
        self.schedules.get(wiki).map(|schedule| schedule.interval)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vulcan_sync::{SyncJob, SyncState, SyncStatus};

    const MIN: Duration = Duration::from_secs(15);
    const MAX: Duration = Duration::from_secs(120);

    fn bounds(_: &PathBuf) -> PollBounds {
        PollBounds { min: MIN, max: MAX }
    }

    fn active(polling: &mut AdaptivePolling, now: Instant) {
        polling.reconcile([("wiki".to_string(), PathBuf::from("/vault"))], now, bounds);
    }

    fn job(id: &str, trigger: SyncJobTrigger, remote: &str) -> SupervisedSyncJob {
        SupervisedSyncJob {
            job: SyncJob {
                version: 1,
                id: id.to_string(),
                wiki_id: Some("wiki".to_string()),
                backend: "git".to_string(),
                vault: PathBuf::from("/vault"),
                trigger,
                state: SyncJobState::Succeeded,
                status: Some(SyncStatus {
                    state: SyncState::Clean,
                    backend: "git".to_string(),
                    vault: PathBuf::from("/vault"),
                    local_revision: Some("local".to_string()),
                    remote_revision: Some(remote.to_string()),
                    accepted_revision: None,
                    unresolved_conflicts: 0,
                    detail: None,
                }),
                error: None,
            },
            triggers: vec![trigger],
            watch: None,
        }
    }

    #[test]
    fn idle_polls_back_off_to_the_maximum() {
        let start = Instant::now();
        let mut polling = AdaptivePolling::default();
        active(&mut polling, start);
        assert_eq!(polling.next_due(), Some(start + MIN));
        assert!(polling.take_due(start, bounds).is_empty());

        let mut intervals = Vec::new();
        for _ in 0..5 {
            let now = polling.next_due().expect("scheduled");
            assert_eq!(polling.take_due(now, bounds).len(), 1);
            intervals.push(polling.interval("wiki").expect("interval").as_secs());
        }
        assert_eq!(intervals, [30, 60, 120, 120, 120]);
    }

    #[test]
    fn local_activity_and_new_remote_revisions_reset_the_interval() {
        let start = Instant::now();
        let mut polling = AdaptivePolling::default();
        active(&mut polling, start);
        for _ in 0..4 {
            let now = polling.next_due().expect("scheduled");
            polling.take_due(now, bounds);
        }
        assert_eq!(polling.interval("wiki"), Some(MAX));

        // A poll that finds nothing new keeps the backoff.
        let later = start + Duration::from_secs(1_000);
        polling.observe_jobs(&[job("a", SyncJobTrigger::Poll, "r1")], later);
        polling.observe_jobs(&[job("b", SyncJobTrigger::Poll, "r1")], later);
        assert_eq!(polling.interval("wiki"), Some(MAX));

        // A poll that sees another device's revision resets it.
        polling.observe_jobs(&[job("c", SyncJobTrigger::Poll, "r2")], later);
        assert_eq!(polling.interval("wiki"), Some(MIN));
        assert!(
            polling.next_due().is_some_and(|due| due <= later + MIN),
            "activity never pushes a due poll later"
        );

        for _ in 0..4 {
            let now = polling.next_due().expect("scheduled");
            polling.take_due(now, bounds);
        }
        // A local edit or commit (a watch-triggered job) resets it too, once.
        let watched = job("d", SyncJobTrigger::Watch, "r2");
        polling.observe_jobs(std::slice::from_ref(&watched), later);
        assert_eq!(polling.interval("wiki"), Some(MIN));
        let now = polling.next_due().expect("scheduled");
        polling.take_due(now, bounds);
        polling.observe_jobs(&[watched], now);
        assert_eq!(
            polling.interval("wiki"),
            Some(MIN * 2),
            "an already observed job is not activity again"
        );
    }

    #[test]
    fn unfinished_jobs_wait_and_removed_wikis_stop_polling() {
        let start = Instant::now();
        let mut polling = AdaptivePolling::default();
        active(&mut polling, start);
        let mut running = job("a", SyncJobTrigger::Watch, "r1");
        running.job.state = SyncJobState::Running;
        polling.take_due(start + MIN, bounds);
        polling.observe_jobs(&[running], start + MIN);
        assert_eq!(polling.interval("wiki"), Some(MIN * 2));

        polling.reconcile(Vec::new(), start, bounds);
        assert_eq!(polling.next_due(), None);
    }

    #[test]
    fn bounds_clamp_degenerate_configuration() {
        assert_eq!(
            PollBounds::from_seconds((0, 0)),
            PollBounds {
                min: Duration::from_secs(1),
                max: Duration::from_secs(1)
            }
        );
        assert_eq!(
            PollBounds::from_seconds((30, 10)).max,
            Duration::from_secs(30)
        );
    }
}
