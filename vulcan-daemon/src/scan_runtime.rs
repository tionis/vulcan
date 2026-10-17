//! Hosted incremental indexing, cache freshness, and completed-scan barriers.

use crate::observation::{
    ObservationEvent, ObservationSubscription, PostScanEvent, VaultObservationHub,
};
use crate::shutdown::ShutdownSignal;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use vulcan_core::index_owner::{IndexOwnerKind, IndexOwnerLease};
use vulcan_core::watch::{IndexFollower, WatchReport, FOLLOW_POLL, INDEX_OWNER_RETRY};
use vulcan_core::{initialize_vulcan_dir, VaultPaths};

const SCAN_STATUS_VERSION: u32 = 1;
const MAX_SCAN_STATUS_BYTES: u64 = 64 * 1024;

const MAX_SCAN_ERROR_BYTES: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheFreshnessState {
    Disabled,
    Unknown,
    Dirty,
    Fresh,
    /// Another process owns the vault's index and keeps it fresh; this
    /// consumer follows its scans (see `vulcan_core::index_owner`).
    Following,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanCompletion {
    pub generation: u64,
    pub state: CacheFreshnessState,
    pub completed_unix_ms: Option<u64>,
    pub fingerprint: Option<String>,
    pub error: Option<String>,
}

impl ScanCompletion {
    #[must_use]
    pub const fn disabled() -> Self {
        Self {
            generation: 0,
            state: CacheFreshnessState::Disabled,
            completed_unix_ms: None,
            fingerprint: None,
            error: None,
        }
    }

    #[must_use]
    pub const fn unknown() -> Self {
        Self {
            generation: 0,
            state: CacheFreshnessState::Unknown,
            completed_unix_ms: None,
            fingerprint: None,
            error: None,
        }
    }

    #[must_use]
    pub fn inspection_error(error: impl AsRef<str>) -> Self {
        Self {
            generation: 0,
            state: CacheFreshnessState::Error,
            completed_unix_ms: None,
            fingerprint: None,
            error: Some(bounded_error(error.as_ref())),
        }
    }
}

#[derive(Debug, Default)]
struct ScanTrackerState {
    requested_generation: u64,
    completion: Option<ScanCompletion>,
    /// Completed freshness barriers awaiting their requester, oldest first.
    barriers: std::collections::BTreeMap<u64, ScanCompletion>,
}

/// Completed barriers kept for requesters; older ones are dropped.
const MAX_COMPLETED_BARRIERS: usize = 256;

#[derive(Debug)]
pub struct VaultScanTracker {
    state: Mutex<ScanTrackerState>,
    changed: Condvar,
    status_path: Option<PathBuf>,
}

impl Default for VaultScanTracker {
    fn default() -> Self {
        Self {
            state: Mutex::new(ScanTrackerState::default()),
            changed: Condvar::new(),
            status_path: None,
        }
    }
}

impl VaultScanTracker {
    pub fn persisted(path: PathBuf) -> Result<Self, String> {
        let completion = load_scan_completion(&path)?;
        let requested_generation = completion.as_ref().map_or(0, |status| status.generation);
        Ok(Self {
            state: Mutex::new(ScanTrackerState {
                requested_generation,
                completion,
                barriers: std::collections::BTreeMap::new(),
            }),
            changed: Condvar::new(),
            status_path: Some(path),
        })
    }

    pub fn mark_dirty(&self) -> Result<u64, String> {
        let mut state = self.state.lock().expect("scan tracker lock");
        state.requested_generation = state.requested_generation.saturating_add(1);
        let generation = state.requested_generation;
        let completion = ScanCompletion {
            generation,
            state: CacheFreshnessState::Dirty,
            completed_unix_ms: None,
            fingerprint: None,
            error: None,
        };
        self.persist(&completion)?;
        state.completion = Some(completion.clone());
        self.changed.notify_all();
        Ok(generation)
    }

    #[must_use]
    pub fn status(&self) -> ScanCompletion {
        self.state
            .lock()
            .expect("scan tracker lock")
            .completion
            .clone()
            .unwrap_or_else(ScanCompletion::unknown)
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

    /// Mark `barriers` reached: every change observed before them is in the
    /// current completion (or this consumer follows another owner).
    fn complete_barriers(&self, barriers: &BTreeSet<u64>) {
        let mut state = self.state.lock().expect("scan tracker lock");
        let completion = state
            .completion
            .clone()
            .unwrap_or_else(ScanCompletion::unknown);
        for barrier in barriers {
            state.barriers.insert(*barrier, completion.clone());
        }
        while state.barriers.len() > MAX_COMPLETED_BARRIERS {
            state.barriers.pop_first();
        }
        self.changed.notify_all();
    }

    /// Wait until `barrier` is reached, returning the completion it reached.
    pub fn wait_for_barrier(
        &self,
        barrier: u64,
        timeout: Duration,
    ) -> Result<ScanCompletion, ScanBarrierError> {
        let state = self.state.lock().map_err(|_| ScanBarrierError::Poisoned)?;
        let (mut state, result) = self
            .changed
            .wait_timeout_while(state, timeout, |state| {
                !state.barriers.contains_key(&barrier)
            })
            .map_err(|_| ScanBarrierError::Poisoned)?;
        if result.timed_out() {
            return Err(ScanBarrierError::BarrierTimeout { barrier });
        }
        state
            .barriers
            .remove(&barrier)
            .ok_or(ScanBarrierError::BarrierTimeout { barrier })
    }

    fn complete(&self, completion: &ScanCompletion) -> Result<(), String> {
        let mut state = self.state.lock().expect("scan tracker lock");
        if completion.generation >= state.requested_generation {
            self.persist(completion)?;
            state.completion = Some(completion.clone());
            self.changed.notify_all();
        }
        Ok(())
    }

    fn persist(&self, completion: &ScanCompletion) -> Result<(), String> {
        self.status_path
            .as_ref()
            .map_or(Ok(()), |path| persist_scan_completion(path, completion))
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct ScanStatusFile {
    version: u32,
    completion: ScanCompletion,
}

#[must_use]
pub fn scan_status_path(root: &Path, registration_id: ulid::Ulid) -> PathBuf {
    root.join("daemon/scans")
        .join(format!("{registration_id}.json"))
}

pub fn load_scan_completion(path: &Path) -> Result<Option<ScanCompletion>, String> {
    match fs::symlink_metadata(path) {
        Ok(metadata)
            if metadata.file_type().is_symlink()
                || !metadata.is_file()
                || metadata.len() > MAX_SCAN_STATUS_BYTES =>
        {
            return Err(format!(
                "scan status {} is not a bounded regular file",
                path.display()
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    }
    let status: ScanStatusFile =
        serde_json::from_slice(&fs::read(path).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
    if status.version != SCAN_STATUS_VERSION {
        return Err(format!(
            "unsupported scan status version {}",
            status.version
        ));
    }
    Ok(Some(status.completion))
}

fn persist_scan_completion(path: &Path, completion: &ScanCompletion) -> Result<(), String> {
    // The completion decides whether the cache counts as fresh, so it is synced.
    let mut bytes = serde_json::to_vec(&ScanStatusFile {
        version: SCAN_STATUS_VERSION,
        completion: completion.clone(),
    })
    .map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    vulcan_core::durable::replace(path, &bytes, vulcan_core::durable::Durability::Full)
        .map_err(|error| error.to_string())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanBarrierError {
    Timeout { generation: u64 },
    BarrierTimeout { barrier: u64 },
    Io(String),
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
            Self::BarrierTimeout { barrier } => {
                write!(
                    formatter,
                    "index barrier {barrier} was not reached before timeout"
                )
            }
            Self::Io(error) => write!(formatter, "index barrier could not be requested: {error}"),
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
    let paths = VaultPaths::new(vault);
    // Registered vaults are initialized here, as their first scan would do.
    let _ = initialize_vulcan_dir(&paths);
    // Another process may own the vault's index; then follow its scans
    // through the journal and take over when it exits. A lease that cannot
    // be opened leaves this consumer indexing as before.
    let (mut _lease, mut following) =
        match IndexOwnerLease::try_acquire(&paths, IndexOwnerKind::Daemon) {
            Ok(Some(lease)) => (Some(lease), None),
            Ok(None) => (None, Some(IndexFollower::start(&paths))),
            Err(_) => (None, None),
        };
    let mut next_attempt = Instant::now() + INDEX_OWNER_RETRY;
    let mut pending = PendingScan::startup(tracker.mark_dirty()?);
    if following.is_some() {
        publish_followed(hub, tracker, None)?;
    }
    let _ = subscription.take_reconciliation_required();
    loop {
        if stop.is_cancelled() {
            return Ok(());
        }
        if let Some(follower) = following.as_mut() {
            if Instant::now() >= next_attempt {
                if let Ok(Some(lease)) =
                    IndexOwnerLease::try_acquire(&paths, IndexOwnerKind::Daemon)
                {
                    _lease = Some(lease);
                    following = None;
                    pending = PendingScan::startup(tracker.mark_dirty()?);
                    continue;
                }
                next_attempt = Instant::now() + INDEX_OWNER_RETRY;
            }
            if let Some(report) = follower.poll() {
                publish_followed(hub, tracker, Some(&report))?;
            }
            // File system hints are the owner's to act on. Barriers complete
            // at once in the following state: this consumer cannot vouch.
            let _ = subscription.take_reconciliation_required();
            match subscription.recv_timeout(FOLLOW_POLL) {
                Ok(ObservationEvent::FilesystemHint(event)) if !event.barriers.is_empty() => {
                    tracker.complete_barriers(&event.barriers);
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    return Err("vault observation channel closed unexpectedly".to_string());
                }
                _ => {}
            }
            continue;
        }
        if subscription.take_reconciliation_required() {
            pending.merge_reconciliation(tracker.mark_dirty()?);
        }
        let now = Instant::now();
        if pending.ready(now, quiet, maximum) {
            run_incremental_scan(vault, hub, tracker, &mut pending)?;
            continue;
        }
        let timeout = pending.next_timeout(now, quiet, maximum);
        match subscription.recv_until(timeout, || stop.is_cancelled()) {
            Ok(ObservationEvent::FilesystemHint(event)) => {
                let barriers = event.barriers.clone();
                if !event.barrier_only() {
                    pending.merge_hint(tracker.mark_dirty()?, event.paths, event.safety_rescan);
                }
                // Every change observed before a barrier is in this hint or an
                // earlier one: index what is pending now, then release it.
                if !barriers.is_empty() {
                    if pending.is_dirty() {
                        run_incremental_scan(vault, hub, tracker, &mut pending)?;
                    }
                    tracker.complete_barriers(&barriers);
                }
            }
            Ok(ObservationEvent::PostScan(_)) | Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Err("vault observation channel closed unexpectedly".to_string());
            }
        }
    }
}

/// Request a freshness barrier on the vault an index consumer watches, and
/// wait until every change made before this call is indexed. Writes a marker
/// file the watcher reports in order with every earlier change; the consumer
/// indexes what is pending and releases the barrier. The completion says how:
/// `fresh` after an owned scan, `following` when another process owns the
/// index and this consumer cannot vouch, or `error`.
pub fn request_index_barrier(
    vault: &Path,
    tracker: &VaultScanTracker,
    timeout: Duration,
) -> Result<ScanCompletion, ScanBarrierError> {
    let barrier = u64::try_from(ulid::Ulid::new().0 & u128::from(u64::MAX)).unwrap_or_default();
    let marker = vault
        .join(".vulcan")
        .join(format!("{}{barrier}", crate::watch::INDEX_BARRIER_PREFIX));
    fs::write(&marker, b"").map_err(|error| ScanBarrierError::Io(error.to_string()))?;
    let result = tracker.wait_for_barrier(barrier, timeout);
    let _ = fs::remove_file(&marker);
    result
}

/// Publish the owner's scan as this consumer's: a post-scan event naming its
/// changed paths, and a completed generation in the `following` state.
/// `None`, or a reconciliation report, names no paths: everything may have
/// changed.
fn publish_followed(
    hub: &VaultObservationHub,
    tracker: &VaultScanTracker,
    report: Option<&WatchReport>,
) -> Result<(), String> {
    let generation = tracker.mark_dirty()?;
    let paths = report
        .filter(|report| !report.startup)
        .map(|report| report.paths.iter().cloned().collect())
        .unwrap_or_default();
    let event =
        PostScanEvent::new(generation, paths, Vec::new()).map_err(|error| error.to_string())?;
    tracker.complete(&ScanCompletion {
        generation,
        state: CacheFreshnessState::Following,
        completed_unix_ms: Some(unix_time_ms()?),
        fingerprint: Some(event.fingerprint.clone()),
        error: None,
    })?;
    hub.publish(&ObservationEvent::PostScan(event))
        .map(drop)
        .map_err(|error| error.to_string())
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

    fn is_dirty(&self) -> bool {
        self.first_dirty.is_some()
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
    tracker.complete(&completion)?;
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

    #[test]
    fn disabled_cache_status_is_explicit_and_has_no_scan_generation() {
        let status = ScanCompletion::disabled();
        assert_eq!(status.generation, 0);
        assert_eq!(status.state, CacheFreshnessState::Disabled);
        assert_eq!(serde_json::to_value(status).unwrap()["state"], "disabled");
    }
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
        let vault = temporary.path().join("vault");
        fs::create_dir(&vault).unwrap();
        fs::write(vault.join("First.md"), "# First\n").unwrap();
        let hub = VaultObservationHub::default();
        let subscription = subscription(&hub);
        let status_path = temporary.path().join("state/scan.json");
        let tracker = Arc::new(VaultScanTracker::persisted(status_path.clone()).unwrap());
        let stop = Arc::new(ShutdownSignal::default());
        let thread_tracker = Arc::clone(&tracker);
        let thread_stop = Arc::clone(&stop);
        let thread_vault = vault.clone();
        let thread_hub = hub.clone();
        let worker = thread::spawn(move || {
            consume_index_observations_with_stop(
                &thread_vault,
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

        fs::write(vault.join("Second.md"), "# Second\n").unwrap();
        hub.publish(&ObservationEvent::FilesystemHint(FilesystemHint {
            sequence: 1,
            event_count: 1,
            untagged_events: 1,
            paths: BTreeSet::from(["Second.md".to_string()]),
            self_generated_transactions: BTreeSet::new(),
            safety_rescan: false,
            watcher_errors: vec![],
            barriers: BTreeSet::new(),
        }))
        .unwrap();
        let second = tracker
            .wait_for_generation(2, Duration::from_secs(5))
            .unwrap();
        assert_eq!(second.state, CacheFreshnessState::Fresh);
        assert_ne!(initial.fingerprint, second.fingerprint);
        let index = load_note_index(&VaultPaths::new(&vault)).unwrap();
        assert!(index.values().any(|note| note.document_path == "Second.md"));
        assert_eq!(load_scan_completion(&status_path).unwrap(), Some(second));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&status_path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        stop.cancel();
        worker.join().unwrap().unwrap();
    }

    /// While another process owns the vault's index, the consumer reports
    /// its scans from the journal without scanning; it takes over when that
    /// process releases ownership.
    #[test]
    fn index_consumer_follows_the_index_owner_and_takes_over() {
        use vulcan_core::index_owner::{IndexOwnerKind, IndexOwnerLease};
        let temporary = tempdir().unwrap();
        let vault = temporary.path().join("vault");
        fs::create_dir(&vault).unwrap();
        fs::write(vault.join("First.md"), "# First\n").unwrap();
        let paths = VaultPaths::new(&vault);
        initialize_vulcan_dir(&paths).unwrap();
        vulcan_core::scan_vault(&paths, vulcan_core::ScanMode::Full).unwrap();
        let owner = IndexOwnerLease::try_acquire(&paths, IndexOwnerKind::Watch)
            .unwrap()
            .unwrap();

        let hub = VaultObservationHub::default();
        let subscription = subscription(&hub);
        let observer = hub
            .subscribe(
                ObservationConsumerId::parse("observer").unwrap(),
                ObservationConsumerPolicy {
                    kind: ObservationConsumerKind::Index,
                    queue_capacity: 64,
                    quiet_period_ms: 10,
                    maximum_dirty_ms: 50,
                    filter: ObservationFilter::default(),
                },
            )
            .unwrap();
        let tracker = Arc::new(VaultScanTracker::default());
        let stop = Arc::new(ShutdownSignal::default());
        let worker = {
            let (tracker, stop, vault, hub) = (
                Arc::clone(&tracker),
                Arc::clone(&stop),
                vault.clone(),
                hub.clone(),
            );
            thread::spawn(move || {
                consume_index_observations_with_stop(&vault, &hub, &subscription, &tracker, &stop)
            })
        };
        let following = tracker
            .wait_for_generation(1, Duration::from_secs(5))
            .unwrap();
        assert_eq!(following.state, CacheFreshnessState::Following);

        // The owner (this test) scans a new note; the consumer publishes it.
        fs::write(vault.join("Second.md"), "# Second\n").unwrap();
        vulcan_core::scan_vault(&paths, vulcan_core::ScanMode::Incremental).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            assert!(
                Instant::now() < deadline,
                "the owner's change should be published"
            );
            if let Ok(ObservationEvent::PostScan(event)) =
                observer.recv_timeout(Duration::from_millis(100))
            {
                if event.changed_paths.contains("Second.md") {
                    break;
                }
            }
        }
        assert_eq!(tracker.status().state, CacheFreshnessState::Following);

        drop(owner);
        let deadline = Instant::now() + Duration::from_secs(10);
        while tracker.status().state != CacheFreshnessState::Fresh {
            assert!(Instant::now() < deadline, "the consumer should take over");
            thread::sleep(Duration::from_millis(50));
        }
        fs::write(vault.join("Third.md"), "# Third\n").unwrap();
        hub.publish(&ObservationEvent::FilesystemHint(FilesystemHint {
            sequence: 2,
            event_count: 1,
            untagged_events: 1,
            paths: BTreeSet::from(["Third.md".to_string()]),
            self_generated_transactions: BTreeSet::new(),
            safety_rescan: false,
            watcher_errors: vec![],
            barriers: BTreeSet::new(),
        }))
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !load_note_index(&paths)
            .unwrap()
            .values()
            .any(|note| note.document_path == "Third.md")
        {
            assert!(Instant::now() < deadline, "the new owner should index");
            thread::sleep(Duration::from_millis(50));
        }
        stop.cancel();
        worker.join().unwrap().unwrap();
    }

    /// A freshness barrier through the real watcher, hub, and consumer: a
    /// note written just before the request is indexed when it returns, with
    /// the consumer's default quiet period (250 ms) otherwise still pending.
    #[test]
    fn index_barriers_return_after_every_earlier_change_is_indexed() {
        let temporary = tempdir().unwrap();
        let vault = temporary.path().join("vault");
        fs::create_dir(&vault).unwrap();
        fs::write(vault.join("First.md"), "# First\n").unwrap();
        let paths = VaultPaths::new(&vault);
        initialize_vulcan_dir(&paths).unwrap();
        let hub = VaultObservationHub::default();
        let subscription = hub
            .subscribe(
                ObservationConsumerId::parse("index").unwrap(),
                ObservationConsumerPolicy {
                    kind: ObservationConsumerKind::Index,
                    queue_capacity: 64,
                    quiet_period_ms: 250,
                    maximum_dirty_ms: 2_000,
                    filter: ObservationFilter::default(),
                },
            )
            .unwrap();
        let tracker = Arc::new(VaultScanTracker::default());
        let stop = Arc::new(ShutdownSignal::default());
        let observer = {
            let (stop, vault, hub) = (Arc::clone(&stop), vault.clone(), hub.clone());
            thread::spawn(move || {
                crate::watch::observe_vault_with_stop(
                    &vault,
                    &crate::watch::DaemonWatchOptions::default(),
                    &hub,
                    || Ok(None),
                    &stop,
                )
            })
        };
        let consumer = {
            let (tracker, stop, vault, hub) = (
                Arc::clone(&tracker),
                Arc::clone(&stop),
                vault.clone(),
                hub.clone(),
            );
            thread::spawn(move || {
                consume_index_observations_with_stop(&vault, &hub, &subscription, &tracker, &stop)
            })
        };
        tracker
            .wait_for_generation(1, Duration::from_secs(10))
            .unwrap();
        let indexed = |name: &str| {
            load_note_index(&paths)
                .unwrap()
                .values()
                .any(|note| note.document_path == name)
        };
        for round in 0..5 {
            let name = format!("Note{round}.md");
            fs::write(vault.join(&name), "# Note\n").unwrap();
            let completion =
                request_index_barrier(&vault, &tracker, Duration::from_secs(10)).expect("barrier");
            assert_eq!(completion.state, CacheFreshnessState::Fresh, "{round}");
            assert!(indexed(&name), "{name} is indexed when its barrier returns");
        }
        // With nothing pending, a barrier returns without scanning again.
        let before = tracker.status().generation;
        let completion =
            request_index_barrier(&vault, &tracker, Duration::from_secs(10)).expect("barrier");
        assert_eq!(completion.generation, before);

        stop.cancel();
        consumer.join().unwrap().unwrap();
        observer.join().unwrap().unwrap();
        assert!(
            !vault.join(".vulcan").read_dir().unwrap().any(|entry| {
                entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(crate::watch::INDEX_BARRIER_PREFIX)
            }),
            "barrier markers are removed"
        );
    }

    /// A consumer following another index owner releases barriers at once
    /// in the following state: it cannot vouch for freshness.
    #[test]
    fn a_following_consumer_releases_barriers_without_vouching() {
        use vulcan_core::index_owner::{IndexOwnerKind, IndexOwnerLease};
        let temporary = tempdir().unwrap();
        let vault = temporary.path().join("vault");
        fs::create_dir(&vault).unwrap();
        let paths = VaultPaths::new(&vault);
        initialize_vulcan_dir(&paths).unwrap();
        let _owner = IndexOwnerLease::try_acquire(&paths, IndexOwnerKind::Watch)
            .unwrap()
            .unwrap();
        let hub = VaultObservationHub::default();
        let subscription = subscription(&hub);
        let tracker = Arc::new(VaultScanTracker::default());
        let stop = Arc::new(ShutdownSignal::default());
        let consumer = {
            let (tracker, stop, vault, hub) = (
                Arc::clone(&tracker),
                Arc::clone(&stop),
                vault.clone(),
                hub.clone(),
            );
            thread::spawn(move || {
                consume_index_observations_with_stop(&vault, &hub, &subscription, &tracker, &stop)
            })
        };
        tracker
            .wait_for_generation(1, Duration::from_secs(10))
            .unwrap();
        hub.publish(&ObservationEvent::FilesystemHint(FilesystemHint {
            sequence: 1,
            event_count: 0,
            untagged_events: 0,
            paths: BTreeSet::new(),
            self_generated_transactions: BTreeSet::new(),
            safety_rescan: false,
            watcher_errors: vec![],
            barriers: BTreeSet::from([7]),
        }))
        .unwrap();
        let completion = tracker
            .wait_for_barrier(7, Duration::from_secs(10))
            .unwrap();
        assert_eq!(completion.state, CacheFreshnessState::Following);
        stop.cancel();
        consumer.join().unwrap().unwrap();
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
