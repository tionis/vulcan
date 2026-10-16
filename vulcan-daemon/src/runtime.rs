//! Registry-driven lifecycle and periodic trigger coordination.

use crate::auto_commit_worker::{
    consume_auto_commit_observations_with_stop, sync_owns_capture, AutoCommitContext,
};
use crate::observation::{
    ObservationConsumerId, ObservationConsumerKind, ObservationConsumerPolicy, ObservationError,
    ObservationFilter, VaultObservationHub,
};
use crate::registry::{RegistryError, WikiRegistration, WikiRegistry};
use crate::routed_reads::{RoutedReadRegistry, RoutedVault};
use crate::scan_runtime::{
    consume_index_observations_with_stop, scan_status_path, VaultScanTracker,
};
use crate::shutdown::ShutdownSignal;
use crate::supervisor::{SupervisorError, SyncSupervisor, SyncWatchMetadata};
use crate::vault_runtime::{VaultRuntimeCatalog, VaultRuntimeError};
use crate::watch::{
    consume_sync_observations_with_stop, observe_vault_with_stop, DaemonWatchError,
    DaemonWatchOptions,
};
use crate::watcher_health::WatcherHealthRegistry;
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};
use vulcan_app::sync_state::SyncStateStore;
use vulcan_core::VaultPaths;
use vulcan_sync::{GitCliEngine, GitEngine, SyncJobTrigger};

const RUNTIME_STOP_POLL: Duration = Duration::from_millis(50);
/// The first delay before restarting a consumer that exited.
const CONSUMER_RESTART_MIN: Duration = Duration::from_secs(1);
/// The longest delay between restarts of a consumer that keeps exiting; a
/// consumer that ran at least this long starts its backoff over.
const CONSUMER_RESTART_MAX: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncTriggerRuntimeOptions {
    pub registry_refresh_ms: u64,
    pub remote_poll_ms: u64,
    pub resume_gap_ms: u64,
    pub watch: DaemonWatchOptions,
}

impl Default for SyncTriggerRuntimeOptions {
    fn default() -> Self {
        Self {
            registry_refresh_ms: 1_000,
            remote_poll_ms: 5 * 60 * 1_000,
            resume_gap_ms: 2_000,
            watch: DaemonWatchOptions::default(),
        }
    }
}

#[derive(Debug)]
pub enum SyncTriggerRuntimeError {
    InvalidOptions(String),
    Registry(RegistryError),
    Supervisor(SupervisorError),
    Observation(ObservationError),
    VaultRuntime(VaultRuntimeError),
    Index(String),
}

impl Display for SyncTriggerRuntimeError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidOptions(detail) => formatter.write_str(detail),
            Self::Registry(error) => Display::fmt(error, formatter),
            Self::Supervisor(error) => Display::fmt(error, formatter),
            Self::Observation(error) => Display::fmt(error, formatter),
            Self::VaultRuntime(error) => Display::fmt(error, formatter),
            Self::Index(detail) => write!(formatter, "hosted index setup failed: {detail}"),
        }
    }
}

impl Error for SyncTriggerRuntimeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Registry(error) => Some(error),
            Self::Supervisor(error) => Some(error),
            Self::Observation(error) => Some(error),
            Self::VaultRuntime(error) => Some(error),
            Self::InvalidOptions(_) | Self::Index(_) => None,
        }
    }
}

impl From<RegistryError> for SyncTriggerRuntimeError {
    fn from(error: RegistryError) -> Self {
        Self::Registry(error)
    }
}

impl From<SupervisorError> for SyncTriggerRuntimeError {
    fn from(error: SupervisorError) -> Self {
        Self::Supervisor(error)
    }
}

impl From<ObservationError> for SyncTriggerRuntimeError {
    fn from(error: ObservationError) -> Self {
        Self::Observation(error)
    }
}

impl From<VaultRuntimeError> for SyncTriggerRuntimeError {
    fn from(error: VaultRuntimeError) -> Self {
        Self::VaultRuntime(error)
    }
}

struct WatcherTask {
    registration: WikiRegistration,
    hub: VaultObservationHub,
    observer_stop: Arc<ShutdownSignal>,
    observer: JoinHandle<Result<(), DaemonWatchError>>,
    index: Option<IndexConsumerTask>,
    sync: Option<SyncConsumerTask>,
    auto_commit: Option<SyncConsumerTask>,
    index_restart: ConsumerRestart,
    sync_restart: ConsumerRestart,
    auto_commit_restart: ConsumerRestart,
}

/// Paces restarts of one consumer that exited, so a consumer that keeps
/// failing (a sync consumer on a vault that is not a Git repository) backs
/// off instead of restarting every registry refresh.
#[derive(Debug, Default)]
struct ConsumerRestart {
    failures: u32,
    not_before: Option<Instant>,
}

impl ConsumerRestart {
    fn record_exit(&mut self, ran_for: Duration, now: Instant) {
        if ran_for >= CONSUMER_RESTART_MAX {
            self.failures = 0;
        }
        self.failures = self.failures.saturating_add(1);
        let delay = CONSUMER_RESTART_MIN
            .saturating_mul(1 << (self.failures - 1).min(16))
            .min(CONSUMER_RESTART_MAX);
        self.not_before = Some(now + delay);
    }

    fn ready(&self, now: Instant) -> bool {
        self.not_before.is_none_or(|not_before| now >= not_before)
    }
}

struct IndexConsumerTask {
    stop: Arc<ShutdownSignal>,
    handle: JoinHandle<Result<(), DaemonWatchError>>,
    started: Instant,
    tracker: Arc<VaultScanTracker>,
    /// Withdraws the vault from routed reads when the consumer stops.
    _routed: RoutedRegistration,
}

/// A vault's entry in the routed-read registry, removed on drop.
struct RoutedRegistration {
    registry: RoutedReadRegistry,
    wiki_id: String,
}

impl Drop for RoutedRegistration {
    fn drop(&mut self) {
        self.registry.remove(&self.wiki_id);
    }
}

struct SyncConsumerTask {
    stop: Arc<ShutdownSignal>,
    handle: JoinHandle<Result<(), DaemonWatchError>>,
    started: Instant,
}

/// Reconciles daemon watcher ownership with the device-local registry and adds
/// periodic remote triggers. Job execution remains owned by the supervisor
/// worker and uses the same finite application transaction as direct mode.
pub fn run_sync_trigger_runtime_until<S>(
    registry: &WikiRegistry,
    supervisor: &Arc<SyncSupervisor>,
    state_store: &SyncStateStore,
    options: &SyncTriggerRuntimeOptions,
    should_stop: S,
) -> Result<(), SyncTriggerRuntimeError>
where
    S: Fn() -> bool,
{
    run_sync_trigger_runtime(
        registry,
        supervisor,
        state_store,
        options,
        should_stop,
        None,
        &RoutedReadRegistry::default(),
        &AutoCommitContext::default(),
        &WatcherHealthRegistry::default(),
    )
}

/// [`run_sync_trigger_runtime_until`] stopped by `stop`, publishing each
/// indexed vault in `routed` while its index consumer runs.
#[allow(clippy::too_many_arguments)] // Each is a distinct shared runtime handle.
pub(crate) fn run_sync_trigger_runtime_with_stop(
    registry: &WikiRegistry,
    supervisor: &Arc<SyncSupervisor>,
    state_store: &SyncStateStore,
    options: &SyncTriggerRuntimeOptions,
    stop: &ShutdownSignal,
    routed: &RoutedReadRegistry,
    auto_commit: &AutoCommitContext,
    health: &WatcherHealthRegistry,
) -> Result<(), SyncTriggerRuntimeError> {
    run_sync_trigger_runtime(
        registry,
        supervisor,
        state_store,
        options,
        || stop.is_cancelled(),
        Some(stop),
        routed,
        auto_commit,
        health,
    )
}

#[allow(clippy::too_many_arguments)]
fn run_sync_trigger_runtime<S>(
    registry: &WikiRegistry,
    supervisor: &Arc<SyncSupervisor>,
    state_store: &SyncStateStore,
    options: &SyncTriggerRuntimeOptions,
    should_stop: S,
    stop: Option<&ShutdownSignal>,
    routed: &RoutedReadRegistry,
    auto_commit: &AutoCommitContext,
    health: &WatcherHealthRegistry,
) -> Result<(), SyncTriggerRuntimeError>
where
    S: Fn() -> bool,
{
    validate_options(options)?;
    let consumers = Consumers {
        supervisor,
        state_store,
        watch: options.watch,
        routed,
        auto_commit: AutoCommitContext {
            registry: Some(registry.clone()),
            ..auto_commit.clone()
        },
        health: health.clone(),
    };
    let registry_refresh = Duration::from_millis(options.registry_refresh_ms);
    let remote_poll = Duration::from_millis(options.remote_poll_ms);
    let mut watchers = BTreeMap::<String, WatcherTask>::new();
    let mut vault_catalog = VaultRuntimeCatalog::default();
    let mut next_registry_refresh = Instant::now();
    let mut next_remote_poll = Instant::now() + remote_poll;
    let mut last_wall = SystemTime::now();
    let mut last_monotonic = Instant::now();

    loop {
        if should_stop() {
            stop_all_watchers(&mut watchers);
            return Ok(());
        }
        let now = Instant::now();
        let wall = SystemTime::now();
        if suspend_gap_detected(
            wall.duration_since(last_wall).unwrap_or_default(),
            now.duration_since(last_monotonic),
            Duration::from_millis(options.resume_gap_ms),
        ) {
            if let Err(error) = enqueue_resume_reconciliation(registry, supervisor) {
                stop_all_watchers(&mut watchers);
                return Err(error);
            }
        }
        last_wall = wall;
        last_monotonic = now;
        if now >= next_registry_refresh {
            if let Err(error) =
                reconcile_watchers(registry, &consumers, &mut watchers, &mut vault_catalog)
            {
                stop_all_watchers(&mut watchers);
                return Err(error);
            }
            next_registry_refresh = now + registry_refresh;
        }
        if now >= next_remote_poll {
            if let Err(error) = enqueue_periodic_reconciliation(registry, supervisor) {
                stop_all_watchers(&mut watchers);
                return Err(error);
            }
            next_remote_poll = now + remote_poll;
        }

        let timeout = next_registry_refresh
            .saturating_duration_since(Instant::now())
            .min(next_remote_poll.saturating_duration_since(Instant::now()));
        if !timeout.is_zero() {
            if let Some(stop) = stop {
                stop.wait_timeout(timeout);
            } else {
                thread::sleep(timeout.min(RUNTIME_STOP_POLL));
            }
        }
    }
}

fn validate_options(options: &SyncTriggerRuntimeOptions) -> Result<(), SyncTriggerRuntimeError> {
    if options.registry_refresh_ms == 0 {
        return Err(SyncTriggerRuntimeError::InvalidOptions(
            "sync registry refresh interval must be greater than zero".to_string(),
        ));
    }
    if options.remote_poll_ms == 0 {
        return Err(SyncTriggerRuntimeError::InvalidOptions(
            "sync remote poll interval must be greater than zero".to_string(),
        ));
    }
    if options.resume_gap_ms == 0 {
        return Err(SyncTriggerRuntimeError::InvalidOptions(
            "sync resume gap must be greater than zero".to_string(),
        ));
    }
    if options.watch.debounce_ms == 0 || options.watch.max_dirty_ms < options.watch.debounce_ms {
        return Err(SyncTriggerRuntimeError::InvalidOptions(
            "sync watch timing options are invalid".to_string(),
        ));
    }
    Ok(())
}

fn desired_observers(
    registrations: &[WikiRegistration],
    catalog: &mut VaultRuntimeCatalog,
) -> Result<BTreeMap<String, WikiRegistration>, VaultRuntimeError> {
    Ok(catalog
        .reconcile(registrations, &[])?
        .runtimes
        .into_iter()
        .filter_map(|runtime| runtime.registration)
        .map(|registration| (registration.id.as_str().to_string(), registration))
        .collect())
}

fn sync_consumer_enabled(registration: &WikiRegistration) -> bool {
    !registration.sync_paused
        && registration
            .sync_backend
            .as_deref()
            .is_none_or(|backend| backend == "git")
}

/// What starting a vault's consumers needs, shared by every vault.
struct Consumers<'a> {
    supervisor: &'a Arc<SyncSupervisor>,
    state_store: &'a SyncStateStore,
    watch: DaemonWatchOptions,
    routed: &'a RoutedReadRegistry,
    auto_commit: AutoCommitContext,
    health: WatcherHealthRegistry,
}

fn reconcile_watchers(
    registry: &WikiRegistry,
    consumers: &Consumers<'_>,
    watchers: &mut BTreeMap<String, WatcherTask>,
    catalog: &mut VaultRuntimeCatalog,
) -> Result<(), SyncTriggerRuntimeError> {
    let Consumers {
        supervisor, routed, ..
    } = *consumers;
    let config = registry.poll()?;
    let desired = desired_observers(&config.vaults, catalog)?;
    let mut failed_this_cycle = BTreeSet::new();
    let now = Instant::now();
    reap_exited_consumers(watchers, supervisor, &consumers.health, now)?;
    let finished = watchers
        .iter()
        .filter(|(_, task)| task.observer.is_finished())
        .map(|(id, _)| id.clone())
        .collect::<Vec<_>>();
    for id in finished {
        if let Some(task) = watchers.remove(&id) {
            let registration = task.registration.clone();
            let detail = join_watcher(task);
            consumers
                .health
                .exited(registration.id.as_str(), "observer", &detail);
            if sync_consumer_enabled(&registration) {
                supervisor.enqueue_watch(
                    registration.id.as_str(),
                    &registration.path,
                    SyncWatchMetadata {
                        safety_rescan: true,
                        watcher_errors: vec![detail],
                        ..SyncWatchMetadata::default()
                    },
                )?;
            }
            failed_this_cycle.insert(id);
        }
    }

    let stale = watchers
        .iter()
        .filter(|(id, task)| {
            desired.get(*id).is_none_or(|registration| {
                watcher_registration_changed(&task.registration, registration)
            })
        })
        .map(|(id, _)| id.clone())
        .collect::<Vec<_>>();
    for id in stale {
        if let Some(task) = watchers.remove(&id) {
            stop_watcher(task);
        }
    }

    for (id, registration) in &desired {
        if !watchers.contains_key(id) && !failed_this_cycle.contains(id) {
            watchers.insert(id.clone(), spawn_watcher(registration.clone(), consumers)?);
        }
    }
    for (id, registration) in &desired {
        let Some(task) = watchers.get_mut(id) else {
            continue;
        };
        ensure_consumers(task, registration, consumers, now)?;
        // A changed permission ceiling applies to routed reads at once.
        if task.registration.permissions_profile != registration.permissions_profile {
            if let Some(index) = &task.index {
                routed.insert(
                    registration.id.as_str(),
                    RoutedVault::new(
                        VaultPaths::new(&registration.path),
                        Arc::clone(&index.tracker),
                        registration.permissions_profile.clone(),
                    ),
                );
            }
        }
        task.registration = registration.clone();
    }
    publish_watcher_health(watchers, &desired, &consumers.health);
    Ok(())
}

/// Publishes which observers and consumers run now. Exits and their errors
/// were recorded when they were reaped.
fn publish_watcher_health(
    watchers: &BTreeMap<String, WatcherTask>,
    desired: &BTreeMap<String, WikiRegistration>,
    health: &WatcherHealthRegistry,
) {
    health.retain(&desired.keys().cloned().collect());
    for (id, registration) in desired {
        publish_task_health(id, registration, watchers.get(id), health);
    }
}

fn publish_task_health(
    id: &str,
    registration: &WikiRegistration,
    task: Option<&WatcherTask>,
    health: &WatcherHealthRegistry,
) {
    let Some(task) = task else {
        health.observe(id, "observer", Some(false));
        return;
    };
    health.observe(id, "observer", Some(!task.observer.is_finished()));
    let wanted = |want: bool, running: bool| want.then_some(running);
    health.observe(
        id,
        "index",
        wanted(
            registration.capabilities().markdown_index,
            task.index.is_some(),
        ),
    );
    health.observe(
        id,
        "sync",
        wanted(sync_consumer_enabled(registration), task.sync.is_some()),
    );
    health.observe(
        id,
        "auto-commit",
        wanted(!sync_owns_capture(registration), task.auto_commit.is_some()),
    );
}

/// Starts the consumers `registration` wants that are not running, unless
/// one is backing off after exiting, and stops a sync consumer it no longer
/// wants.
fn ensure_consumers(
    task: &mut WatcherTask,
    registration: &WikiRegistration,
    consumers: &Consumers<'_>,
    now: Instant,
) -> Result<(), SyncTriggerRuntimeError> {
    let Consumers {
        supervisor,
        state_store,
        watch: watch_options,
        routed,
        ..
    } = *consumers;
    if registration.capabilities().markdown_index
        && task.index.is_none()
        && task.index_restart.ready(now)
    {
        task.index = Some(spawn_index_consumer(
            registration.clone(),
            &task.hub,
            state_store.root(),
            watch_options,
            routed,
        )?);
    }
    let should_sync = sync_consumer_enabled(registration);
    if should_sync && task.sync.is_none() && task.sync_restart.ready(now) {
        task.sync = Some(spawn_sync_consumer(
            registration.clone(),
            &task.hub,
            Arc::clone(supervisor),
            state_store.clone(),
            watch_options,
        )?);
    } else if !should_sync {
        if let Some(sync) = task.sync.take() {
            stop_sync_consumer(sync);
        }
        task.sync_restart = ConsumerRestart::default();
    }
    let should_auto_commit = !sync_owns_capture(registration);
    if should_auto_commit && task.auto_commit.is_none() && task.auto_commit_restart.ready(now) {
        task.auto_commit = Some(spawn_auto_commit_consumer(
            registration.clone(),
            &task.hub,
            consumers,
        )?);
    } else if !should_auto_commit {
        if let Some(auto_commit) = task.auto_commit.take() {
            stop_sync_consumer(auto_commit);
        }
        task.auto_commit_restart = ConsumerRestart::default();
    }
    Ok(())
}

/// Joins consumers that exited and schedules their restart. A consumer
/// restarts alone; the observer and the other consumer keep running, so the
/// index stays warm and routable while a failing sync consumer backs off.
fn reap_exited_consumers(
    watchers: &mut BTreeMap<String, WatcherTask>,
    supervisor: &SyncSupervisor,
    health: &WatcherHealthRegistry,
    now: Instant,
) -> Result<(), SupervisorError> {
    for task in watchers.values_mut() {
        let wiki_id = task.registration.id.to_string();
        if task.observer.is_finished() {
            continue;
        }
        if task
            .index
            .as_ref()
            .is_some_and(|index| index.handle.is_finished())
        {
            if let Some(index) = task.index.take() {
                let ran_for = index.started.elapsed();
                // Dropping the task withdraws the vault from routed reads.
                let detail = join_task("index observation consumer", index.handle);
                health.exited(&wiki_id, "index", &detail);
                task.index_restart.record_exit(ran_for, now);
            }
        }
        if task
            .sync
            .as_ref()
            .is_some_and(|sync| sync.handle.is_finished())
        {
            if let Some(sync) = task.sync.take() {
                let ran_for = sync.started.elapsed();
                let detail = join_task("sync observation consumer", sync.handle);
                health.exited(&wiki_id, "sync", &detail);
                task.sync_restart.record_exit(ran_for, now);
                if sync_consumer_enabled(&task.registration) {
                    supervisor.enqueue_watch(
                        task.registration.id.as_str(),
                        &task.registration.path,
                        SyncWatchMetadata {
                            safety_rescan: true,
                            watcher_errors: vec![detail],
                            ..SyncWatchMetadata::default()
                        },
                    )?;
                }
            }
        }
        if task
            .auto_commit
            .as_ref()
            .is_some_and(|auto_commit| auto_commit.handle.is_finished())
        {
            if let Some(auto_commit) = task.auto_commit.take() {
                let ran_for = auto_commit.started.elapsed();
                let detail = join_task("auto-commit observation consumer", auto_commit.handle);
                eprintln!("{detail}");
                health.exited(&wiki_id, "auto-commit", &detail);
                task.auto_commit_restart.record_exit(ran_for, now);
            }
        }
    }
    Ok(())
}

fn watcher_registration_changed(current: &WikiRegistration, desired: &WikiRegistration) -> bool {
    current.id != desired.id
        || current.registration_id != desired.registration_id
        || current.path != desired.path
        || current.git_dir != desired.git_dir
        || current.sync_backend != desired.sync_backend
        || current.profile != desired.profile
}

fn spawn_watcher(
    registration: WikiRegistration,
    consumers: &Consumers<'_>,
) -> Result<WatcherTask, SyncTriggerRuntimeError> {
    let supervisor = Arc::clone(consumers.supervisor);
    let state_store = consumers.state_store.clone();
    let options = consumers.watch;
    let routed = consumers.routed;
    let hub = VaultObservationHub::default();
    let observer_stop = Arc::new(ShutdownSignal::new(false));
    let thread_stop = Arc::clone(&observer_stop);
    let thread_registration = registration.clone();
    let thread_hub = hub.clone();
    let marker_store = state_store.clone();
    let observer = thread::spawn(move || {
        let repository = GitCliEngine::default()
            .discover_repository(&thread_registration.path)
            .ok();
        observe_vault_with_stop(
            &thread_registration.path,
            &options,
            &thread_hub,
            || {
                repository.as_ref().map_or(Ok(None), |repository| {
                    marker_store
                        .load_apply_marker(&repository.git_dir)
                        .map(|marker| {
                            marker.map(|marker| {
                                marker.transaction_id.to_string().to_ascii_lowercase()
                            })
                        })
                        .map_err(|error| error.to_string())
                })
            },
            &thread_stop,
        )
    });
    let index = registration
        .capabilities()
        .markdown_index
        .then(|| {
            spawn_index_consumer(
                registration.clone(),
                &hub,
                state_store.root(),
                options,
                routed,
            )
        })
        .transpose()?;
    let sync = sync_consumer_enabled(&registration)
        .then(|| spawn_sync_consumer(registration.clone(), &hub, supervisor, state_store, options))
        .transpose()?;
    let auto_commit = (!sync_owns_capture(&registration))
        .then(|| spawn_auto_commit_consumer(registration.clone(), &hub, consumers))
        .transpose()?;
    let task = WatcherTask {
        registration,
        hub,
        observer_stop,
        observer,
        index,
        sync,
        auto_commit,
        index_restart: ConsumerRestart::default(),
        sync_restart: ConsumerRestart::default(),
        auto_commit_restart: ConsumerRestart::default(),
    };
    publish_task_health(
        task.registration.id.as_str(),
        &task.registration,
        Some(&task),
        &consumers.health,
    );
    Ok(task)
}

fn spawn_auto_commit_consumer(
    registration: WikiRegistration,
    hub: &VaultObservationHub,
    consumers: &Consumers<'_>,
) -> Result<SyncConsumerTask, ObservationError> {
    let subscription = hub.subscribe(
        ObservationConsumerId::parse(format!("auto-commit-{}", registration.id))?,
        ObservationConsumerPolicy {
            kind: ObservationConsumerKind::AutoCommit,
            queue_capacity: 64,
            quiet_period_ms: consumers.watch.debounce_ms,
            maximum_dirty_ms: consumers.watch.max_dirty_ms,
            filter: ObservationFilter::default(),
        },
    )?;
    let context = consumers.auto_commit.clone();
    let state_root = consumers.state_store.root().to_path_buf();
    // Edits count as paused once the watcher's own dirty bound has passed.
    let quiet = Duration::from_millis(consumers.watch.max_dirty_ms);
    let stop = Arc::new(ShutdownSignal::default());
    let thread_stop = Arc::clone(&stop);
    let handle = thread::spawn(move || {
        consume_auto_commit_observations_with_stop(
            &registration,
            &context,
            &state_root,
            &subscription,
            quiet,
            &thread_stop,
        )
    });
    Ok(SyncConsumerTask {
        stop,
        handle,
        started: Instant::now(),
    })
}

fn spawn_index_consumer(
    registration: WikiRegistration,
    hub: &VaultObservationHub,
    state_root: &std::path::Path,
    options: DaemonWatchOptions,
    routed: &RoutedReadRegistry,
) -> Result<IndexConsumerTask, SyncTriggerRuntimeError> {
    let subscription = hub.subscribe(
        ObservationConsumerId::parse(format!("index-{}", registration.id))?,
        ObservationConsumerPolicy {
            kind: ObservationConsumerKind::Index,
            queue_capacity: 64,
            quiet_period_ms: options.debounce_ms,
            maximum_dirty_ms: options.max_dirty_ms,
            filter: ObservationFilter::default(),
        },
    )?;
    let tracker = Arc::new(
        VaultScanTracker::persisted(scan_status_path(state_root, registration.registration_id))
            .map_err(SyncTriggerRuntimeError::Index)?,
    );
    routed.insert(
        registration.id.as_str(),
        RoutedVault::new(
            VaultPaths::new(&registration.path),
            Arc::clone(&tracker),
            registration.permissions_profile.clone(),
        ),
    );
    let routed = RoutedRegistration {
        registry: routed.clone(),
        wiki_id: registration.id.as_str().to_string(),
    };
    let thread_tracker = Arc::clone(&tracker);
    let stop = Arc::new(ShutdownSignal::default());
    let thread_stop = Arc::clone(&stop);
    let thread_hub = hub.clone();
    let handle = thread::spawn(move || {
        consume_index_observations_with_stop(
            &registration.path,
            &thread_hub,
            &subscription,
            &thread_tracker,
            &thread_stop,
        )
        .map_err(DaemonWatchError::Index)
    });
    Ok(IndexConsumerTask {
        stop,
        handle,
        started: Instant::now(),
        tracker,
        _routed: routed,
    })
}

fn spawn_sync_consumer(
    registration: WikiRegistration,
    hub: &VaultObservationHub,
    supervisor: Arc<SyncSupervisor>,
    state_store: SyncStateStore,
    options: DaemonWatchOptions,
) -> Result<SyncConsumerTask, ObservationError> {
    let subscription = hub.subscribe(
        ObservationConsumerId::parse(format!("sync-{}", registration.id))?,
        ObservationConsumerPolicy {
            kind: ObservationConsumerKind::Sync,
            queue_capacity: 64,
            quiet_period_ms: options.debounce_ms,
            maximum_dirty_ms: options.max_dirty_ms,
            filter: ObservationFilter::default(),
        },
    )?;
    let stop = Arc::new(ShutdownSignal::default());
    let thread_stop = Arc::clone(&stop);
    let handle = thread::spawn(move || {
        consume_sync_observations_with_stop(
            &registration,
            &supervisor,
            &state_store,
            &subscription,
            &thread_stop,
        )
    });
    Ok(SyncConsumerTask {
        stop,
        handle,
        started: Instant::now(),
    })
}

fn enqueue_periodic_reconciliation(
    registry: &WikiRegistry,
    supervisor: &SyncSupervisor,
) -> Result<(), SyncTriggerRuntimeError> {
    let config = registry.poll()?;
    let mut catalog = VaultRuntimeCatalog::default();
    for registration in desired_observers(&config.vaults, &mut catalog)?
        .into_values()
        .filter(sync_consumer_enabled)
    {
        supervisor.enqueue(
            registration.id.as_str(),
            &registration.path,
            SyncJobTrigger::Poll,
        )?;
    }
    Ok(())
}

fn enqueue_resume_reconciliation(
    registry: &WikiRegistry,
    supervisor: &SyncSupervisor,
) -> Result<(), SyncTriggerRuntimeError> {
    let config = registry.poll()?;
    let mut catalog = VaultRuntimeCatalog::default();
    for registration in desired_observers(&config.vaults, &mut catalog)?
        .into_values()
        .filter(sync_consumer_enabled)
    {
        supervisor.enqueue(
            registration.id.as_str(),
            &registration.path,
            SyncJobTrigger::Resume,
        )?;
    }
    Ok(())
}

fn suspend_gap_detected(
    wall_elapsed: Duration,
    monotonic_elapsed: Duration,
    threshold: Duration,
) -> bool {
    wall_elapsed.saturating_sub(monotonic_elapsed) >= threshold
}

fn stop_all_watchers(watchers: &mut BTreeMap<String, WatcherTask>) {
    for (_, task) in std::mem::take(watchers) {
        stop_watcher(task);
    }
}

fn stop_watcher(task: WatcherTask) {
    if let Some(auto_commit) = task.auto_commit {
        stop_sync_consumer(auto_commit);
    }
    if let Some(sync) = task.sync {
        stop_sync_consumer(sync);
    }
    if let Some(index) = task.index {
        stop_index_consumer(index);
    }
    task.observer_stop.cancel();
    let _ = task.observer.join();
}

fn join_watcher(task: WatcherTask) -> String {
    let mut details = Vec::new();
    if let Some(auto_commit) = task.auto_commit {
        auto_commit.stop.cancel();
        details.push(join_task(
            "auto-commit observation consumer",
            auto_commit.handle,
        ));
    }
    if let Some(sync) = task.sync {
        sync.stop.cancel();
        details.push(join_task("sync observation consumer", sync.handle));
    }
    if let Some(index) = task.index {
        index.stop.cancel();
        details.push(join_task("index observation consumer", index.handle));
    }
    task.observer_stop.cancel();
    details.push(join_task("vault observer", task.observer));
    details.join("; ")
}

fn stop_sync_consumer(task: SyncConsumerTask) {
    task.stop.cancel();
    let _ = task.handle.join();
}

fn stop_index_consumer(task: IndexConsumerTask) {
    task.stop.cancel();
    let _ = task.handle.join();
}

fn join_task(label: &str, handle: JoinHandle<Result<(), DaemonWatchError>>) -> String {
    match handle.join() {
        Ok(Ok(())) => format!("{label} exited unexpectedly"),
        Ok(Err(error)) => format!("{label} failed: {error}"),
        Err(_) => format!("{label} thread panicked"),
    }
}

/// A real observer and consumers for one registration, stopped on drop: lets
/// tests elsewhere in the crate exercise a served vault end to end.
#[cfg(test)]
pub(crate) struct TestWatcher(Option<WatcherTask>);

#[cfg(test)]
impl Drop for TestWatcher {
    fn drop(&mut self) {
        if let Some(task) = self.0.take() {
            stop_watcher(task);
        }
    }
}

#[cfg(test)]
pub(crate) fn watch_for_tests(
    registration: WikiRegistration,
    state_root: &std::path::Path,
    routed: &RoutedReadRegistry,
    health: &WatcherHealthRegistry,
) -> TestWatcher {
    let supervisor =
        Arc::new(SyncSupervisor::at(state_root.join("jobs.json")).expect("test supervisor"));
    let state_store = SyncStateStore::at(state_root.join("state"));
    let consumers = Consumers {
        supervisor: &supervisor,
        state_store: &state_store,
        watch: DaemonWatchOptions::default(),
        routed,
        auto_commit: AutoCommitContext::default(),
        health: health.clone(),
    };
    TestWatcher(Some(
        spawn_watcher(registration, &consumers).expect("test watcher"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_consumers<'a>(
        supervisor: &'a Arc<SyncSupervisor>,
        state_store: &'a SyncStateStore,
        routed: &'a RoutedReadRegistry,
    ) -> Consumers<'a> {
        Consumers {
            supervisor,
            state_store,
            watch: DaemonWatchOptions::default(),
            routed,
            auto_commit: AutoCommitContext::default(),
            health: WatcherHealthRegistry::default(),
        }
    }
    use crate::registry::{AddWikiRequest, ManagedDirectoryProfile, UpdateWikiRequest, WikiId};
    use std::path::PathBuf;
    use std::process::Command;
    use tempfile::tempdir;

    fn registration(id: &str, paused: bool, backend: Option<&str>) -> WikiRegistration {
        WikiRegistration {
            id: WikiId::parse(id).expect("wiki id"),
            registration_id: ulid::Ulid::new(),
            path: PathBuf::from(format!("/{id}")),
            work_tree: None,
            profile: ManagedDirectoryProfile::Knowledge,
            profile_version: None,
            materialization: crate::registry::MaterializationProfile::Full,
            groups: Vec::new(),
            git_dir: None,
            permissions_profile: None,
            sync_backend: backend.map(str::to_string),
            platform_profile: None,
            sync_paused: paused,
        }
    }

    #[test]
    fn desired_observer_set_includes_paused_and_non_git_registrations() {
        let temporary = tempdir().unwrap();
        let mut registrations = vec![
            registration("alpha", false, None),
            registration("beta", false, Some("git")),
            registration("paused", true, Some("git")),
            registration("other", false, Some("seafile")),
        ];
        for registration in &mut registrations {
            registration.path = temporary.path().join(registration.id.as_str());
            std::fs::create_dir(&registration.path).unwrap();
        }
        let mut catalog = VaultRuntimeCatalog::default();
        let desired = desired_observers(&registrations, &mut catalog).unwrap();
        assert_eq!(
            desired.keys().cloned().collect::<Vec<_>>(),
            ["alpha", "beta", "other", "paused"]
        );
        assert!(sync_consumer_enabled(desired.get("alpha").unwrap()));
        assert!(sync_consumer_enabled(desired.get("beta").unwrap()));
        assert!(!sync_consumer_enabled(desired.get("paused").unwrap()));
        assert!(!sync_consumer_enabled(desired.get("other").unwrap()));
    }

    #[test]
    fn watcher_restart_identity_tracks_only_watcher_relevant_registration_fields() {
        let current = registration("alpha", false, Some("git"));
        let mut metadata_only = current.clone();
        metadata_only.groups.push("daily".to_string());
        metadata_only.permissions_profile = Some("automation".to_string());
        assert!(!watcher_registration_changed(&current, &metadata_only));

        let mut moved = current.clone();
        moved.path = PathBuf::from("/moved-alpha");
        assert!(watcher_registration_changed(&current, &moved));
        let mut detached = current.clone();
        detached.git_dir = Some(PathBuf::from("/private/git/alpha"));
        assert!(watcher_registration_changed(&current, &detached));
        let mut paused = current.clone();
        paused.sync_paused = true;
        assert!(!watcher_registration_changed(&current, &paused));
        let mut files_only = current.clone();
        files_only.profile = ManagedDirectoryProfile::FilesOnly;
        assert!(watcher_registration_changed(&current, &files_only));
    }

    #[test]
    fn files_only_watcher_does_not_start_index_or_create_cache() {
        let temporary = tempdir().unwrap();
        let directory = temporary.path().join("media");
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(directory.join("notes.md"), "# ordinary file\n").unwrap();
        let mut registration = registration("media", false, Some("none"));
        registration.path = directory.clone();
        registration.profile = ManagedDirectoryProfile::FilesOnly;
        let supervisor = Arc::new(SyncSupervisor::at(temporary.path().join("jobs.json")).unwrap());
        let state_store = SyncStateStore::at(temporary.path().join("state"));
        let watcher = spawn_watcher(
            registration,
            &test_consumers(&supervisor, &state_store, &RoutedReadRegistry::default()),
        )
        .unwrap();
        assert!(watcher.index.is_none());
        assert!(watcher.sync.is_none());
        stop_watcher(watcher);
        assert!(!directory.join(".vulcan/cache.db").exists());
        assert!(!state_store.root().join("daemon/scans").exists());
    }

    #[test]
    fn mixed_profiles_keep_index_consumers_isolated() {
        let temporary = tempdir().unwrap();
        let knowledge_path = temporary.path().join("knowledge");
        let files_path = temporary.path().join("files");
        std::fs::create_dir(&knowledge_path).unwrap();
        std::fs::create_dir(&files_path).unwrap();
        std::fs::write(knowledge_path.join("Home.md"), "# Home\n").unwrap();
        std::fs::write(files_path.join("Home.md"), "ordinary bytes\n").unwrap();
        let mut knowledge = registration("knowledge", false, Some("none"));
        knowledge.path = knowledge_path.clone();
        let mut files = registration("files", false, Some("none"));
        files.path = files_path.clone();
        files.profile = ManagedDirectoryProfile::FilesOnly;
        let supervisor = Arc::new(SyncSupervisor::at(temporary.path().join("jobs.json")).unwrap());
        let state_store = SyncStateStore::at(temporary.path().join("state"));
        let knowledge_watcher = spawn_watcher(
            knowledge,
            &test_consumers(&supervisor, &state_store, &RoutedReadRegistry::default()),
        )
        .unwrap();
        let files_watcher = spawn_watcher(
            files,
            &test_consumers(&supervisor, &state_store, &RoutedReadRegistry::default()),
        )
        .unwrap();
        assert!(knowledge_watcher.index.is_some());
        assert!(files_watcher.index.is_none());
        knowledge_watcher
            .index
            .as_ref()
            .unwrap()
            .tracker
            .wait_for_generation(1, Duration::from_secs(5))
            .unwrap();
        stop_watcher(files_watcher);
        stop_watcher(knowledge_watcher);
        assert!(knowledge_path.join(".vulcan/cache.db").exists());
        assert!(!files_path.join(".vulcan").exists());
    }

    #[test]
    fn consumer_restarts_back_off_and_reset_after_a_long_run() {
        let start = Instant::now();
        let mut restart = ConsumerRestart::default();
        assert!(restart.ready(start));
        let mut delays = Vec::new();
        for _ in 0..12 {
            restart.record_exit(Duration::ZERO, start);
            delays.push(restart.not_before.unwrap() - start);
        }
        assert_eq!(delays[0], CONSUMER_RESTART_MIN);
        assert_eq!(delays[1], CONSUMER_RESTART_MIN * 2);
        assert_eq!(delays[2], CONSUMER_RESTART_MIN * 4);
        assert_eq!(*delays.last().unwrap(), CONSUMER_RESTART_MAX);
        assert!(!restart.ready(start));
        assert!(restart.ready(start + CONSUMER_RESTART_MAX));
        restart.record_exit(CONSUMER_RESTART_MAX, start);
        assert_eq!(restart.not_before, Some(start + CONSUMER_RESTART_MIN));
    }

    /// A sync consumer that cannot start (the vault is not a Git
    /// repository) used to restart the whole watcher, index consumer
    /// included, every registry refresh.
    #[test]
    fn a_failing_sync_consumer_restarts_alone_with_backoff() {
        let temporary = tempdir().expect("temporary directory");
        let vault = temporary.path().join("vault");
        std::fs::create_dir(&vault).expect("vault");
        std::fs::write(vault.join("Home.md"), "# Home\n").expect("note");
        let registry = WikiRegistry::at(temporary.path().join("daemon.toml"));
        registry
            .add(
                &AddWikiRequest {
                    profile: None,
                    id: WikiId::parse("notes").unwrap(),
                    path: vault,
                    groups: vec![],
                    git_dir: None,
                    permissions_profile: None,
                    sync_backend: None,
                    platform_profile: None,
                },
                false,
            )
            .unwrap();
        let supervisor = Arc::new(SyncSupervisor::at(temporary.path().join("jobs.json")).unwrap());
        let state_store = SyncStateStore::at(temporary.path().join("state"));
        let routed = RoutedReadRegistry::default();
        let mut watchers = BTreeMap::new();
        let mut catalog = VaultRuntimeCatalog::default();
        let consumers = test_consumers(&supervisor, &state_store, &routed);
        let reconcile = |watchers: &mut BTreeMap<String, WatcherTask>,
                         catalog: &mut VaultRuntimeCatalog| {
            reconcile_watchers(&registry, &consumers, watchers, catalog).unwrap();
        };
        reconcile(&mut watchers, &mut catalog);
        let task = &watchers["notes"];
        let tracker = Arc::clone(&task.index.as_ref().unwrap().tracker);
        tracker
            .wait_for_generation(1, Duration::from_secs(5))
            .unwrap();
        let observer = task.observer.thread().id();
        let index = task.index.as_ref().unwrap().handle.thread().id();

        let deadline = Instant::now() + Duration::from_secs(4);
        while Instant::now() < deadline {
            thread::sleep(Duration::from_millis(100));
            reconcile(&mut watchers, &mut catalog);
        }
        let task = &watchers["notes"];
        assert_eq!(task.observer.thread().id(), observer);
        assert_eq!(task.index.as_ref().unwrap().handle.thread().id(), index);
        assert_eq!(tracker.status().generation, 1, "no repeated startup scans");
        assert!(routed.get("notes").is_some(), "reads stay routable");
        // Restarts at about 0 s, 1 s, and 3 s rather than one per refresh.
        assert!(
            (1..=3).contains(&task.sync_restart.failures),
            "{} sync restarts",
            task.sync_restart.failures
        );
        // The failures are visible, and the healthy parts stay healthy.
        let health = consumers.health.get("notes").expect("vault health");
        let sync = &health.components["sync"];
        assert!(sync.restarts >= 1, "{sync:?}");
        assert!(sync
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("sync observation consumer")));
        assert!(health.components["observer"].running);
        assert!(health.components["index"].running);
        assert!(!health.components.contains_key("auto-commit"));
        stop_all_watchers(&mut watchers);
    }

    #[test]
    #[allow(clippy::too_many_lines)] // One lifecycle fixture proves pause, indexing, and observer identity together.
    fn sync_pause_toggles_only_the_consumer_and_keeps_plain_observers() {
        let temporary = tempdir().expect("temporary directory");
        let git_vault = temporary.path().join("git-vault");
        let plain_vault = temporary.path().join("plain-vault");
        std::fs::create_dir(&git_vault).expect("Git vault");
        std::fs::create_dir(&plain_vault).expect("plain vault");
        assert!(Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(&git_vault)
            .status()
            .expect("git init")
            .success());
        let registry = WikiRegistry::at(temporary.path().join("daemon.toml"));
        let git_id = WikiId::parse("git-notes").unwrap();
        registry
            .add(
                &AddWikiRequest {
                    profile: None,
                    id: git_id.clone(),
                    path: git_vault,
                    groups: vec![],
                    git_dir: None,
                    permissions_profile: None,
                    sync_backend: Some("git".to_string()),
                    platform_profile: None,
                },
                false,
            )
            .unwrap();
        registry
            .update(
                &git_id,
                &UpdateWikiRequest {
                    profile: None,
                    groups_to_add: vec![],
                    groups_to_remove: vec![],
                    permissions_profile: None,
                    sync_paused: Some(true),
                },
                false,
            )
            .unwrap();
        registry
            .add(
                &AddWikiRequest {
                    profile: None,
                    id: WikiId::parse("plain").unwrap(),
                    path: plain_vault,
                    groups: vec![],
                    git_dir: None,
                    permissions_profile: None,
                    sync_backend: Some("none".to_string()),
                    platform_profile: None,
                },
                false,
            )
            .unwrap();
        let supervisor = Arc::new(SyncSupervisor::at(temporary.path().join("jobs.json")).unwrap());
        let state_store = SyncStateStore::at(temporary.path().join("state"));
        let mut watchers = BTreeMap::new();
        let mut catalog = VaultRuntimeCatalog::default();
        reconcile_watchers(
            &registry,
            &test_consumers(&supervisor, &state_store, &RoutedReadRegistry::default()),
            &mut watchers,
            &mut catalog,
        )
        .unwrap();
        assert_eq!(watchers.len(), 2);
        assert!(watchers["git-notes"].sync.is_none());
        assert!(watchers["plain"].sync.is_none());
        // Git sync owns capture for the Git-backed vault, even while paused;
        // the vault without sync gets the batched auto-commit consumer.
        assert!(watchers["git-notes"].auto_commit.is_none());
        assert!(watchers["plain"].auto_commit.is_some());
        let initial = watchers["git-notes"]
            .index
            .as_ref()
            .unwrap()
            .tracker
            .wait_for_generation(1, Duration::from_secs(5))
            .unwrap();
        assert_eq!(
            initial.state,
            crate::scan_runtime::CacheFreshnessState::Fresh
        );
        std::fs::write(
            watchers["git-notes"].registration.path.join("Paused.md"),
            "# Still indexed\n",
        )
        .unwrap();
        watchers["git-notes"]
            .hub
            .publish(&crate::observation::ObservationEvent::FilesystemHint(
                crate::observation::FilesystemHint {
                    sequence: 1,
                    event_count: 1,
                    untagged_events: 1,
                    paths: BTreeSet::from(["Paused.md".to_string()]),
                    self_generated_transactions: BTreeSet::new(),
                    safety_rescan: false,
                    watcher_errors: vec![],
                    barriers: BTreeSet::new(),
                },
            ))
            .unwrap();
        let indexed = watchers["git-notes"]
            .index
            .as_ref()
            .unwrap()
            .tracker
            .wait_for_generation(2, Duration::from_secs(5))
            .unwrap();
        assert_eq!(
            indexed.state,
            crate::scan_runtime::CacheFreshnessState::Fresh
        );
        let notes = vulcan_core::properties::load_note_index(&vulcan_core::VaultPaths::new(
            &watchers["git-notes"].registration.path,
        ))
        .unwrap();
        assert!(notes.values().any(|note| note.document_path == "Paused.md"));
        let observer_id = watchers["git-notes"].observer.thread().id();

        registry
            .update(
                &git_id,
                &UpdateWikiRequest {
                    profile: None,
                    groups_to_add: vec![],
                    groups_to_remove: vec![],
                    permissions_profile: None,
                    sync_paused: Some(false),
                },
                false,
            )
            .unwrap();
        reconcile_watchers(
            &registry,
            &test_consumers(&supervisor, &state_store, &RoutedReadRegistry::default()),
            &mut watchers,
            &mut catalog,
        )
        .unwrap();
        assert_eq!(watchers["git-notes"].observer.thread().id(), observer_id);
        assert!(watchers["git-notes"].sync.is_some());
        stop_all_watchers(&mut watchers);
    }

    #[test]
    fn runtime_starts_watchers_and_adds_periodic_poll_triggers() {
        let temporary = tempdir().expect("temporary directory");
        let vault = temporary.path().join("vault");
        std::fs::create_dir(&vault).expect("vault directory");
        let output = Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(&vault)
            .output()
            .expect("run git init");
        assert!(output.status.success());
        let registry = WikiRegistry::at(temporary.path().join("daemon.toml"));
        registry
            .add(
                &AddWikiRequest {
                    profile: None,
                    id: WikiId::parse("alpha").expect("wiki id"),
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
            Arc::new(SyncSupervisor::at(temporary.path().join("jobs.json")).expect("supervisor"));
        let state_store = SyncStateStore::at(temporary.path().join("state"));
        let started = Instant::now();
        let observed_periodic_poll = || {
            supervisor.list().is_ok_and(|jobs| {
                jobs.iter()
                    .any(|job| job.triggers.contains(&SyncJobTrigger::Poll))
            })
        };
        run_sync_trigger_runtime_until(
            &registry,
            &supervisor,
            &state_store,
            &SyncTriggerRuntimeOptions {
                registry_refresh_ms: 10,
                remote_poll_ms: 20,
                resume_gap_ms: 2_000,
                watch: DaemonWatchOptions {
                    debounce_ms: 5,
                    max_dirty_ms: 20,
                },
            },
            || observed_periodic_poll() || started.elapsed() >= Duration::from_secs(2),
        )
        .expect("trigger runtime");

        let jobs = supervisor.list().expect("jobs");
        assert_eq!(jobs.len(), 1);
        assert!(jobs[0].triggers.contains(&SyncJobTrigger::Resume));
        assert!(jobs[0].triggers.contains(&SyncJobTrigger::Poll));
    }

    #[test]
    fn suspend_gap_detection_ignores_scheduler_delay_and_detects_wall_clock_jump() {
        let threshold = Duration::from_secs(2);
        assert!(!suspend_gap_detected(
            Duration::from_secs(30),
            Duration::from_secs(30),
            threshold
        ));
        assert!(!suspend_gap_detected(
            Duration::from_secs(31),
            Duration::from_secs(30),
            threshold
        ));
        assert!(suspend_gap_detected(
            Duration::from_secs(90),
            Duration::from_secs(30),
            threshold
        ));
    }

    #[test]
    fn pausing_a_registration_removes_it_from_periodic_reconciliation() {
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
        registry
            .update(
                &id,
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
        let supervisor =
            SyncSupervisor::at(temporary.path().join("jobs.json")).expect("supervisor");

        enqueue_periodic_reconciliation(&registry, &supervisor).expect("periodic trigger");
        assert!(supervisor.list().expect("jobs").is_empty());
    }
}
