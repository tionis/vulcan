//! Hosted incremental indexing, cache freshness, and completed-scan barriers.

use crate::observation::{
    ObservationEvent, ObservationSubscription, PostScanEvent, VaultObservationHub,
};
use crate::shutdown::ShutdownSignal;
use serde::Serialize;
use std::collections::BTreeSet;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::path::Path;
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use vulcan_core::{initialize_vulcan_dir, VaultPaths};

const MAX_SCAN_ERROR_BYTES: usize = 512;
const INDEX_WAIT_POLL: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheFreshnessState {
    Unknown,
    Dirty,
    Fresh,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScanCompletion {
    pub generation: u64,
    pub state: CacheFreshnessState,
    pub completed_unix_ms: Option<u64>,
    pub fingerprint: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Default)]
struct ScanTrackerState {
    requested_generation: u64,
    completion: Option<ScanCompletion>,
}

#[derive(Debug, Default)]
pub struct VaultScanTracker {
    state: Mutex<ScanTrackerState>,
    changed: Condvar,
}

impl VaultScanTracker {
    pub fn mark_dirty(&self) -> u64 {
        let mut state = self.state.lock().expect("scan tracker lock");
        state.requested_generation = state.requested_generation.saturating_add(1);
        let generation = state.requested_generation;
        state.completion = Some(ScanCompletion {
            generation,
            state: CacheFreshnessState::Dirty,
            completed_unix_ms: None,
            fingerprint: None,
            error: None,
        });
        self.changed.notify_all();
        generation
    }

    #[must_use]
    pub fn status(&self) -> ScanCompletion {
        self.state
            .lock()
            .expect("scan tracker lock")
            .completion
            .clone()
            .unwrap_or(ScanCompletion {
                generation: 0,
                state: CacheFreshnessState::Unknown,
                completed_unix_ms: None,
                fingerprint: None,
                error: None,
            })
    }

    pub fn wait_for_generation(
        &self,
        generation: u64,
        timeout: Duration,
    ) -> Result<ScanCompletion, ScanBarrierError> {
        let state = self.state.lock().map_err(|_| ScanBarrierError::Poisoned)?;
        let (state, result) = self
            .changed
            .wait_timeout_while(state, timeout, |state| {
                state.completion.as_ref().is_none_or(|completion| {
                    completion.generation < generation
                        || completion.state == CacheFreshnessState::Dirty
                })
            })
            .map_err(|_| ScanBarrierError::Poisoned)?;
        if result.timed_out() {
            return Err(ScanBarrierError::Timeout { generation });
        }
        state
            .completion
            .clone()
            .ok_or(ScanBarrierError::Timeout { generation })
    }

    fn complete(&self, completion: ScanCompletion) {
        let mut state = self.state.lock().expect("scan tracker lock");
        if completion.generation >= state.requested_generation {
            state.completion = Some(completion);
            self.changed.notify_all();
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanBarrierError {
    Timeout { generation: u64 },
    Poisoned,
}

impl Display for ScanBarrierError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Timeout { generation } => {
                write!(
                    formatter,
                    "scan generation {generation} did not complete before timeout"
                )
            }
            Self::Poisoned => formatter.write_str("scan tracker lock is poisoned"),
        }
    }
}

impl Error for ScanBarrierError {}

pub fn consume_index_observations_with_stop(
    vault: &Path,
    hub: &VaultObservationHub,
    subscription: &ObservationSubscription,
    tracker: &VaultScanTracker,
    stop: &ShutdownSignal,
) -> Result<(), String> {
    stop.register_current_thread();
    let quiet = Duration::from_millis(subscription.policy.quiet_period_ms);
    let maximum = Duration::from_millis(subscription.policy.maximum_dirty_ms);
    let mut pending = PendingScan::startup(tracker.mark_dirty());
    let _ = subscription.take_reconciliation_required();
    loop {
        if stop.is_cancelled() {
            return Ok(());
        }
        if subscription.take_reconciliation_required() {
            pending.merge_reconciliation(tracker.mark_dirty());
        }
        let now = Instant::now();
        if pending.ready(now, quiet, maximum) {
            run_incremental_scan(vault, hub, tracker, &mut pending)?;
            continue;
        }
        let timeout = pending
            .next_timeout(now, quiet, maximum)
            .unwrap_or(INDEX_WAIT_POLL)
            .min(INDEX_WAIT_POLL);
        match subscription.recv_timeout(timeout) {
            Ok(ObservationEvent::FilesystemHint(event)) => {
                pending.merge_hint(tracker.mark_dirty(), event.paths, event.safety_rescan);
            }
            Ok(ObservationEvent::PostScan(_)) | Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Err("vault observation channel closed unexpectedly".to_string());
            }
        }
    }
}

#[derive(Debug)]
struct PendingScan {
    generation: u64,
    first_dirty: Option<Instant>,
    last_dirty: Option<Instant>,
    changed_paths: BTreeSet<String>,
    safety_rescan: bool,
}

impl PendingScan {
    fn startup(generation: u64) -> Self {
        let now = Instant::now();
        Self {
            generation,
            first_dirty: Some(now),
            last_dirty: Some(now.checked_sub(Duration::from_secs(60)).unwrap_or(now)),
            changed_paths: BTreeSet::new(),
            safety_rescan: true,
        }
    }

    fn merge_hint(&mut self, generation: u64, paths: BTreeSet<String>, safety_rescan: bool) {
        let now = Instant::now();
        self.generation = generation;
        self.first_dirty.get_or_insert(now);
        self.last_dirty = Some(now);
        self.changed_paths.extend(paths);
        self.safety_rescan |= safety_rescan;
    }

    fn merge_reconciliation(&mut self, generation: u64) {
        self.merge_hint(generation, BTreeSet::new(), true);
    }

    fn ready(&self, now: Instant, quiet: Duration, maximum: Duration) -> bool {
        self.first_dirty.is_some_and(|first| {
            now.saturating_duration_since(first) >= maximum
                || self
                    .last_dirty
                    .is_some_and(|last| now.saturating_duration_since(last) >= quiet)
        })
    }

    fn next_timeout(&self, now: Instant, quiet: Duration, maximum: Duration) -> Option<Duration> {
        let first = self.first_dirty?;
        let last = self.last_dirty.unwrap_or(first);
        Some(
            last.checked_add(quiet)
                .unwrap_or(now)
                .saturating_duration_since(now)
                .min(
                    first
                        .checked_add(maximum)
                        .unwrap_or(now)
                        .saturating_duration_since(now),
                ),
        )
    }

    fn take(&mut self) -> (u64, BTreeSet<String>, bool) {
        let result = (
            self.generation,
            std::mem::take(&mut self.changed_paths),
            self.safety_rescan,
        );
        self.first_dirty = None;
        self.last_dirty = None;
        self.safety_rescan = false;
        result
    }
}

fn run_incremental_scan(
    vault: &Path,
    hub: &VaultObservationHub,
    tracker: &VaultScanTracker,
    pending: &mut PendingScan,
) -> Result<(), String> {
    let (generation, changed_paths, _safety_rescan) = pending.take();
    let paths = VaultPaths::new(vault);
    let result = initialize_vulcan_dir(&paths)
        .map_err(|error| error.to_string())
        .and_then(|()| {
            vulcan_app::scan::refresh_cache_incrementally(&paths).map_err(|error| error.to_string())
        });
    let errors = result
        .as_ref()
        .err()
        .map(|error| vec![bounded_error(error)])
        .unwrap_or_default();
    let event = PostScanEvent::new(generation, changed_paths, errors.clone())
        .map_err(|error| error.to_string())?;
    let completion = ScanCompletion {
        generation,
        state: if errors.is_empty() {
            CacheFreshnessState::Fresh
        } else {
            CacheFreshnessState::Error
        },
        completed_unix_ms: Some(unix_time_ms()?),
        fingerprint: Some(event.fingerprint.clone()),
        error: errors.into_iter().next(),
    };
    tracker.complete(completion);
    hub.publish(&ObservationEvent::PostScan(event))
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn bounded_error(error: &str) -> String {
    let mut value = error.to_string();
    if value.len() <= MAX_SCAN_ERROR_BYTES {
        return value;
    }
    let mut end = MAX_SCAN_ERROR_BYTES.saturating_sub(3);
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value.truncate(end);
    value.push_str("...");
    value
}

fn unix_time_ms() -> Result<u64, String> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_millis();
    u64::try_from(millis).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observation::{
        FilesystemHint, ObservationConsumerId, ObservationConsumerKind, ObservationConsumerPolicy,
        ObservationFilter,
    };
    use std::fs;
    use std::sync::Arc;
    use std::thread;
    use tempfile::tempdir;
    use vulcan_core::properties::load_note_index;

    fn subscription(hub: &VaultObservationHub) -> ObservationSubscription {
        hub.subscribe(
            ObservationConsumerId::parse("index").unwrap(),
            ObservationConsumerPolicy {
                kind: ObservationConsumerKind::Index,
                queue_capacity: 8,
                quiet_period_ms: 10,
                maximum_dirty_ms: 50,
                filter: ObservationFilter::default(),
            },
        )
        .unwrap()
    }

    #[test]
    fn index_consumer_scans_and_exposes_a_completed_generation_barrier() {
        let temporary = tempdir().unwrap();
        fs::write(temporary.path().join("First.md"), "# First\n").unwrap();
        let hub = VaultObservationHub::default();
        let subscription = subscription(&hub);
        let tracker = Arc::new(VaultScanTracker::default());
        let stop = Arc::new(ShutdownSignal::default());
        let thread_tracker = Arc::clone(&tracker);
        let thread_stop = Arc::clone(&stop);
        let vault = temporary.path().to_path_buf();
        let thread_hub = hub.clone();
        let worker = thread::spawn(move || {
            consume_index_observations_with_stop(
                &vault,
                &thread_hub,
                &subscription,
                &thread_tracker,
                &thread_stop,
            )
        });
        let initial = tracker
            .wait_for_generation(1, Duration::from_secs(5))
            .unwrap();
        assert_eq!(initial.state, CacheFreshnessState::Fresh);

        fs::write(temporary.path().join("Second.md"), "# Second\n").unwrap();
        hub.publish(&ObservationEvent::FilesystemHint(FilesystemHint {
            sequence: 1,
            event_count: 1,
            untagged_events: 1,
            paths: BTreeSet::from(["Second.md".to_string()]),
            self_generated_transactions: BTreeSet::new(),
            safety_rescan: false,
            watcher_errors: vec![],
        }))
        .unwrap();
        let second = tracker
            .wait_for_generation(2, Duration::from_secs(5))
            .unwrap();
        assert_eq!(second.state, CacheFreshnessState::Fresh);
        assert_ne!(initial.fingerprint, second.fingerprint);
        let index = load_note_index(&VaultPaths::new(temporary.path())).unwrap();
        assert!(index.values().any(|note| note.document_path == "Second.md"));
        stop.cancel();
        worker.join().unwrap().unwrap();
    }

    #[test]
    fn scan_barrier_times_out_without_claiming_freshness() {
        let tracker = VaultScanTracker::default();
        let error = tracker
            .wait_for_generation(1, Duration::from_millis(1))
            .unwrap_err();
        assert_eq!(error, ScanBarrierError::Timeout { generation: 1 });
        assert_eq!(tracker.status().state, CacheFreshnessState::Unknown);
    }
}
