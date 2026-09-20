//! Bounded fan-out of filesystem hints and authoritative post-scan events.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, Weak};

const MAX_CONSUMERS: usize = 32;
const MAX_QUEUE_CAPACITY: usize = 1_024;
const MAX_FILTER_PREFIXES: usize = 128;
const MAX_EVENT_PATHS: usize = 512;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ObservationConsumerId(String);

impl ObservationConsumerId {
    pub fn parse(value: impl Into<String>) -> Result<Self, ObservationError> {
        let value = value.into();
        if value.is_empty()
            || value.len() > 96
            || value.chars().any(char::is_control)
            || value.contains(['/', '\\'])
        {
            return Err(ObservationError::InvalidConsumerId(value));
        }
        Ok(Self(value))
    }
}

impl Display for ObservationConsumerId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservationConsumerKind {
    Index,
    Sync,
    AutoCommit,
    Preview,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservationFilter {
    /// Empty means all vault-relative paths.
    pub include_prefixes: BTreeSet<String>,
    pub exclude_prefixes: BTreeSet<String>,
}

impl Default for ObservationFilter {
    fn default() -> Self {
        Self {
            include_prefixes: BTreeSet::new(),
            exclude_prefixes: BTreeSet::from([".vulcan/".to_string()]),
        }
    }
}

impl ObservationFilter {
    fn accepts(&self, path: &str) -> bool {
        !self
            .exclude_prefixes
            .iter()
            .any(|prefix| path.starts_with(prefix))
            && (self.include_prefixes.is_empty()
                || self
                    .include_prefixes
                    .iter()
                    .any(|prefix| path.starts_with(prefix)))
    }

    fn validate(&self) -> Result<(), ObservationError> {
        let count = self
            .include_prefixes
            .len()
            .saturating_add(self.exclude_prefixes.len());
        if count > MAX_FILTER_PREFIXES {
            return Err(ObservationError::TooManyFilterPrefixes(count));
        }
        if self
            .include_prefixes
            .iter()
            .chain(&self.exclude_prefixes)
            .any(|prefix| prefix.len() > 512 || prefix.starts_with('/') || prefix.contains(".."))
        {
            return Err(ObservationError::InvalidFilterPrefix);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservationConsumerPolicy {
    pub kind: ObservationConsumerKind,
    pub queue_capacity: usize,
    pub quiet_period_ms: u64,
    pub maximum_dirty_ms: u64,
    pub filter: ObservationFilter,
}

impl ObservationConsumerPolicy {
    fn validate(&self) -> Result<(), ObservationError> {
        if self.queue_capacity == 0 || self.queue_capacity > MAX_QUEUE_CAPACITY {
            return Err(ObservationError::InvalidQueueCapacity(self.queue_capacity));
        }
        if self.quiet_period_ms == 0 || self.maximum_dirty_ms < self.quiet_period_ms {
            return Err(ObservationError::InvalidDebounce);
        }
        self.filter.validate()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilesystemHint {
    pub sequence: u64,
    pub paths: BTreeSet<String>,
    pub safety_rescan: bool,
    pub watcher_errors: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostScanEvent {
    pub scan_generation: u64,
    pub changed_paths: BTreeSet<String>,
    pub scan_errors: Vec<String>,
    pub fingerprint: String,
}

impl PostScanEvent {
    pub fn new(
        scan_generation: u64,
        changed_paths: BTreeSet<String>,
        scan_errors: Vec<String>,
    ) -> Result<Self, ObservationError> {
        validate_event_bounds(&changed_paths)?;
        let canonical = serde_json::to_vec(&(
            scan_generation,
            changed_paths.iter().collect::<Vec<_>>(),
            &scan_errors,
        ))
        .map_err(|error| ObservationError::Fingerprint(error.to_string()))?;
        let fingerprint = blake3::hash(&canonical).to_hex().to_string();
        Ok(Self {
            scan_generation,
            changed_paths,
            scan_errors,
            fingerprint,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObservationEvent {
    FilesystemHint(FilesystemHint),
    PostScan(PostScanEvent),
}

impl ObservationEvent {
    fn filtered(&self, filter: &ObservationFilter) -> Option<Self> {
        match self {
            Self::FilesystemHint(event) => {
                let mut event = event.clone();
                event.paths.retain(|path| filter.accepts(path));
                if event.paths.is_empty() && !event.safety_rescan && event.watcher_errors.is_empty()
                {
                    None
                } else {
                    Some(Self::FilesystemHint(event))
                }
            }
            Self::PostScan(event) => Some(Self::PostScan(event.clone())),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishReport {
    pub delivered: Vec<ObservationConsumerId>,
    pub reconciliation_required: Vec<ObservationConsumerId>,
}

#[derive(Debug, Clone)]
pub struct VaultObservationHub {
    inner: Arc<HubInner>,
}

#[derive(Debug)]
struct HubInner {
    consumers: Mutex<BTreeMap<ObservationConsumerId, ConsumerEntry>>,
    next_generation: AtomicU64,
}

#[derive(Debug)]
struct ConsumerEntry {
    generation: u64,
    policy: ObservationConsumerPolicy,
    sender: mpsc::SyncSender<ObservationEvent>,
    reconciliation_required: Arc<AtomicBool>,
    dropped_events: Arc<AtomicU64>,
}

impl Default for VaultObservationHub {
    fn default() -> Self {
        Self {
            inner: Arc::new(HubInner {
                consumers: Mutex::new(BTreeMap::new()),
                next_generation: AtomicU64::new(1),
            }),
        }
    }
}

impl VaultObservationHub {
    pub fn subscribe(
        &self,
        id: ObservationConsumerId,
        policy: ObservationConsumerPolicy,
    ) -> Result<ObservationSubscription, ObservationError> {
        policy.validate()?;
        let mut consumers = self
            .inner
            .consumers
            .lock()
            .map_err(|_| ObservationError::Poisoned)?;
        if consumers.contains_key(&id) {
            return Err(ObservationError::DuplicateConsumer(id));
        }
        if consumers.len() >= MAX_CONSUMERS {
            return Err(ObservationError::TooManyConsumers(consumers.len() + 1));
        }
        let generation = self.inner.next_generation.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = mpsc::sync_channel(policy.queue_capacity);
        let reconciliation_required = Arc::new(AtomicBool::new(true));
        let dropped_events = Arc::new(AtomicU64::new(0));
        consumers.insert(
            id.clone(),
            ConsumerEntry {
                generation,
                policy: policy.clone(),
                sender,
                reconciliation_required: Arc::clone(&reconciliation_required),
                dropped_events: Arc::clone(&dropped_events),
            },
        );
        Ok(ObservationSubscription {
            id,
            generation,
            policy,
            receiver,
            reconciliation_required,
            dropped_events,
            hub: Arc::downgrade(&self.inner),
        })
    }

    pub fn publish(&self, event: &ObservationEvent) -> Result<PublishReport, ObservationError> {
        validate_observation_event(event)?;
        let consumers = self
            .inner
            .consumers
            .lock()
            .map_err(|_| ObservationError::Poisoned)?;
        let mut report = PublishReport {
            delivered: Vec::new(),
            reconciliation_required: Vec::new(),
        };
        for (id, consumer) in consumers.iter() {
            let Some(event) = event.filtered(&consumer.policy.filter) else {
                continue;
            };
            match consumer.sender.try_send(event) {
                Ok(()) => report.delivered.push(id.clone()),
                Err(mpsc::TrySendError::Full(_)) => {
                    consumer
                        .reconciliation_required
                        .store(true, Ordering::Release);
                    consumer.dropped_events.fetch_add(1, Ordering::Relaxed);
                    report.reconciliation_required.push(id.clone());
                }
                Err(mpsc::TrySendError::Disconnected(_)) => {
                    consumer
                        .reconciliation_required
                        .store(true, Ordering::Release);
                    report.reconciliation_required.push(id.clone());
                }
            }
        }
        Ok(report)
    }
}

#[derive(Debug)]
pub struct ObservationSubscription {
    id: ObservationConsumerId,
    generation: u64,
    pub policy: ObservationConsumerPolicy,
    receiver: mpsc::Receiver<ObservationEvent>,
    reconciliation_required: Arc<AtomicBool>,
    dropped_events: Arc<AtomicU64>,
    hub: Weak<HubInner>,
}

impl ObservationSubscription {
    pub fn try_recv(&self) -> Result<ObservationEvent, mpsc::TryRecvError> {
        self.receiver.try_recv()
    }

    #[must_use]
    pub fn take_reconciliation_required(&self) -> bool {
        self.reconciliation_required.swap(false, Ordering::AcqRel)
    }

    #[must_use]
    pub fn dropped_events(&self) -> u64 {
        self.dropped_events.load(Ordering::Acquire)
    }
}

impl Drop for ObservationSubscription {
    fn drop(&mut self) {
        let Some(hub) = self.hub.upgrade() else {
            return;
        };
        if let Ok(mut consumers) = hub.consumers.lock() {
            let remove = consumers
                .get(&self.id)
                .is_some_and(|consumer| consumer.generation == self.generation);
            if remove {
                consumers.remove(&self.id);
            }
        };
    }
}

#[derive(Debug)]
pub enum ObservationError {
    InvalidConsumerId(String),
    DuplicateConsumer(ObservationConsumerId),
    TooManyConsumers(usize),
    InvalidQueueCapacity(usize),
    InvalidDebounce,
    TooManyFilterPrefixes(usize),
    InvalidFilterPrefix,
    TooManyEventPaths(usize),
    Fingerprint(String),
    Poisoned,
}

impl Display for ObservationError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidConsumerId(id) => write!(formatter, "invalid observation consumer `{id}`"),
            Self::DuplicateConsumer(id) => write!(formatter, "observation consumer `{id}` exists"),
            Self::TooManyConsumers(count) => {
                write!(
                    formatter,
                    "observation consumer count {count} exceeds {MAX_CONSUMERS}"
                )
            }
            Self::InvalidQueueCapacity(capacity) => write!(
                formatter,
                "observation queue capacity {capacity} must be between 1 and {MAX_QUEUE_CAPACITY}"
            ),
            Self::InvalidDebounce => formatter.write_str(
                "observation maximum dirty age must be at least its non-zero quiet period",
            ),
            Self::TooManyFilterPrefixes(count) => write!(
                formatter,
                "observation filter prefix count {count} exceeds {MAX_FILTER_PREFIXES}"
            ),
            Self::InvalidFilterPrefix => formatter.write_str("invalid observation filter prefix"),
            Self::TooManyEventPaths(count) => write!(
                formatter,
                "observation event path count {count} exceeds {MAX_EVENT_PATHS}"
            ),
            Self::Fingerprint(detail) => {
                write!(formatter, "cannot fingerprint scan event: {detail}")
            }
            Self::Poisoned => formatter.write_str("observation hub lock is poisoned"),
        }
    }
}

impl Error for ObservationError {}

fn validate_observation_event(event: &ObservationEvent) -> Result<(), ObservationError> {
    match event {
        ObservationEvent::FilesystemHint(event) => validate_event_bounds(&event.paths),
        ObservationEvent::PostScan(event) => validate_event_bounds(&event.changed_paths),
    }
}

fn validate_event_bounds(paths: &BTreeSet<String>) -> Result<(), ObservationError> {
    if paths.len() > MAX_EVENT_PATHS {
        return Err(ObservationError::TooManyEventPaths(paths.len()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(kind: ObservationConsumerKind, capacity: usize) -> ObservationConsumerPolicy {
        ObservationConsumerPolicy {
            kind,
            queue_capacity: capacity,
            quiet_period_ms: 100,
            maximum_dirty_ms: 1_000,
            filter: ObservationFilter::default(),
        }
    }

    fn hint(sequence: u64, path: &str) -> ObservationEvent {
        ObservationEvent::FilesystemHint(FilesystemHint {
            sequence,
            paths: BTreeSet::from([path.to_string()]),
            safety_rescan: false,
            watcher_errors: vec![],
        })
    }

    #[test]
    fn two_consumers_share_events_with_independent_filters() {
        let hub = VaultObservationHub::default();
        let index = hub
            .subscribe(
                ObservationConsumerId::parse("index").unwrap(),
                policy(ObservationConsumerKind::Index, 4),
            )
            .unwrap();
        let mut preview_policy = policy(ObservationConsumerKind::Preview, 4);
        preview_policy.filter.include_prefixes = BTreeSet::from(["Public/".to_string()]);
        let preview = hub
            .subscribe(
                ObservationConsumerId::parse("preview").unwrap(),
                preview_policy,
            )
            .unwrap();
        assert!(index.take_reconciliation_required());
        assert!(preview.take_reconciliation_required());

        let report = hub.publish(&hint(1, "Private/Note.md")).unwrap();
        assert_eq!(
            report.delivered,
            [ObservationConsumerId::parse("index").unwrap()]
        );
        assert!(index.try_recv().is_ok());
        assert!(matches!(preview.try_recv(), Err(mpsc::TryRecvError::Empty)));
    }

    #[test]
    fn a_slow_consumer_overflow_does_not_block_another_consumer() {
        let hub = VaultObservationHub::default();
        let slow = hub
            .subscribe(
                ObservationConsumerId::parse("slow").unwrap(),
                policy(ObservationConsumerKind::Preview, 1),
            )
            .unwrap();
        let fast = hub
            .subscribe(
                ObservationConsumerId::parse("fast").unwrap(),
                policy(ObservationConsumerKind::Sync, 4),
            )
            .unwrap();
        assert!(slow.take_reconciliation_required());
        assert!(fast.take_reconciliation_required());
        hub.publish(&hint(1, "One.md")).unwrap();
        assert!(fast.try_recv().is_ok());
        let report = hub.publish(&hint(2, "Two.md")).unwrap();

        assert!(report
            .reconciliation_required
            .contains(&ObservationConsumerId::parse("slow").unwrap()));
        assert!(report
            .delivered
            .contains(&ObservationConsumerId::parse("fast").unwrap()));
        assert!(slow.take_reconciliation_required());
        assert_eq!(slow.dropped_events(), 1);
        assert!(fast.try_recv().is_ok());
    }

    #[test]
    fn dropping_one_consumer_keeps_the_shared_observation_live() {
        let hub = VaultObservationHub::default();
        let first = hub
            .subscribe(
                ObservationConsumerId::parse("first").unwrap(),
                policy(ObservationConsumerKind::Index, 2),
            )
            .unwrap();
        let second = hub
            .subscribe(
                ObservationConsumerId::parse("second").unwrap(),
                policy(ObservationConsumerKind::Preview, 2),
            )
            .unwrap();
        drop(first);

        let report = hub.publish(&hint(1, "Note.md")).unwrap();
        assert_eq!(
            report.delivered,
            [ObservationConsumerId::parse("second").unwrap()]
        );
        assert!(second.try_recv().is_ok());
    }

    #[test]
    fn post_scan_fingerprints_are_stable_and_distinct_from_raw_hints() {
        let paths = BTreeSet::from(["A.md".to_string(), "B.md".to_string()]);
        let first = PostScanEvent::new(7, paths.clone(), vec![]).unwrap();
        let second = PostScanEvent::new(7, paths, vec![]).unwrap();
        assert_eq!(first.fingerprint, second.fingerprint);
        assert_ne!(
            first.fingerprint,
            PostScanEvent::new(8, BTreeSet::new(), vec![])
                .unwrap()
                .fingerprint
        );
        assert_eq!(first.fingerprint.len(), 64);
    }
}
