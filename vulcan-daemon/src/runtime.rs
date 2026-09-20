//! Registry-driven lifecycle and periodic trigger coordination.

use crate::observation::{
    ObservationConsumerId, ObservationConsumerKind, ObservationConsumerPolicy, ObservationError,
    ObservationFilter, VaultObservationHub,
};
use crate::registry::{RegistryError, WikiRegistration, WikiRegistry};
use crate::shutdown::ShutdownSignal;
use crate::supervisor::{SupervisorError, SyncSupervisor, SyncWatchMetadata};
use crate::vault_runtime::{VaultRuntimeCatalog, VaultRuntimeError};
use crate::watch::{
    consume_sync_observations_with_stop, observe_vault_with_stop, DaemonWatchError,
    DaemonWatchOptions,
};
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};
use vulcan_app::sync_state::SyncStateStore;
use vulcan_sync::{GitCliEngine, GitEngine, SyncJobTrigger};

const RUNTIME_STOP_POLL: Duration = Duration::from_millis(50);

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
}

impl Display for SyncTriggerRuntimeError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidOptions(detail) => formatter.write_str(detail),
            Self::Registry(error) => Display::fmt(error, formatter),
            Self::Supervisor(error) => Display::fmt(error, formatter),
            Self::Observation(error) => Display::fmt(error, formatter),
            Self::VaultRuntime(error) => Display::fmt(error, formatter),
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
            Self::InvalidOptions(_) => None,
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
    sync: Option<SyncConsumerTask>,
}

struct SyncConsumerTask {
    stop: Arc<ShutdownSignal>,
    handle: JoinHandle<Result<(), DaemonWatchError>>,
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
    )
}

pub(crate) fn run_sync_trigger_runtime_with_stop(
    registry: &WikiRegistry,
    supervisor: &Arc<SyncSupervisor>,
    state_store: &SyncStateStore,
    options: &SyncTriggerRuntimeOptions,
    stop: &ShutdownSignal,
) -> Result<(), SyncTriggerRuntimeError> {
    run_sync_trigger_runtime(
        registry,
        supervisor,
        state_store,
        options,
        || stop.is_cancelled(),
        Some(stop),
    )
}

fn run_sync_trigger_runtime<S>(
    registry: &WikiRegistry,
    supervisor: &Arc<SyncSupervisor>,
    state_store: &SyncStateStore,
    options: &SyncTriggerRuntimeOptions,
    should_stop: S,
    stop: Option<&ShutdownSignal>,
) -> Result<(), SyncTriggerRuntimeError>
where
    S: Fn() -> bool,
{
    validate_options(options)?;
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
            if let Err(error) = reconcile_watchers(
                registry,
                supervisor,
                state_store,
                options.watch,
                &mut watchers,
                &mut vault_catalog,
            ) {
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

fn reconcile_watchers(
    registry: &WikiRegistry,
    supervisor: &Arc<SyncSupervisor>,
    state_store: &SyncStateStore,
    watch_options: DaemonWatchOptions,
    watchers: &mut BTreeMap<String, WatcherTask>,
    catalog: &mut VaultRuntimeCatalog,
) -> Result<(), SyncTriggerRuntimeError> {
    let config = registry.load()?;
    let desired = desired_observers(&config.vaults, catalog)?;
    let mut failed_this_cycle = BTreeSet::new();
    let finished = watchers
        .iter()
        .filter(|(_, task)| watcher_finished(task))
        .map(|(id, _)| id.clone())
        .collect::<Vec<_>>();
    for id in finished {
        if let Some(task) = watchers.remove(&id) {
            let registration = task.registration.clone();
            let detail = join_watcher(task);
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
            watchers.insert(
                id.clone(),
                spawn_watcher(
                    registration.clone(),
                    Arc::clone(supervisor),
                    state_store.clone(),
                    watch_options,
                )?,
            );
        }
    }
    for (id, registration) in &desired {
        let Some(task) = watchers.get_mut(id) else {
            continue;
        };
        let should_sync = sync_consumer_enabled(registration);
        if should_sync && task.sync.is_none() {
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
        }
        task.registration = registration.clone();
    }
    Ok(())
}

fn watcher_registration_changed(current: &WikiRegistration, desired: &WikiRegistration) -> bool {
    current.id != desired.id
        || current.registration_id != desired.registration_id
        || current.path != desired.path
        || current.git_dir != desired.git_dir
        || current.sync_backend != desired.sync_backend
}

fn spawn_watcher(
    registration: WikiRegistration,
    supervisor: Arc<SyncSupervisor>,
    state_store: SyncStateStore,
    options: DaemonWatchOptions,
) -> Result<WatcherTask, SyncTriggerRuntimeError> {
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
    let sync = sync_consumer_enabled(&registration)
        .then(|| spawn_sync_consumer(registration.clone(), &hub, supervisor, state_store, options))
        .transpose()?;
    Ok(WatcherTask {
        registration,
        hub,
        observer_stop,
        observer,
        sync,
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
    Ok(SyncConsumerTask { stop, handle })
}

fn enqueue_periodic_reconciliation(
    registry: &WikiRegistry,
    supervisor: &SyncSupervisor,
) -> Result<(), SyncTriggerRuntimeError> {
    let config = registry.load()?;
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
    let config = registry.load()?;
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
    if let Some(sync) = task.sync {
        stop_sync_consumer(sync);
    }
    task.observer_stop.cancel();
    let _ = task.observer.join();
}

fn join_watcher(task: WatcherTask) -> String {
    let mut details = Vec::new();
    if let Some(sync) = task.sync {
        sync.stop.cancel();
        details.push(join_task("sync observation consumer", sync.handle));
    }
    task.observer_stop.cancel();
    details.push(join_task("vault observer", task.observer));
    details.join("; ")
}

fn stop_sync_consumer(task: SyncConsumerTask) {
    task.stop.cancel();
    let _ = task.handle.join();
}

fn watcher_finished(task: &WatcherTask) -> bool {
    task.observer.is_finished()
        || task
            .sync
            .as_ref()
            .is_some_and(|sync| sync.handle.is_finished())
}

fn join_task(label: &str, handle: JoinHandle<Result<(), DaemonWatchError>>) -> String {
    match handle.join() {
        Ok(Ok(())) => format!("{label} exited unexpectedly"),
        Ok(Err(error)) => format!("{label} failed: {error}"),
        Err(_) => format!("{label} thread panicked"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{AddWikiRequest, UpdateWikiRequest, WikiId};
    use std::path::PathBuf;
    use std::process::Command;
    use tempfile::tempdir;

    fn registration(id: &str, paused: bool, backend: Option<&str>) -> WikiRegistration {
        WikiRegistration {
            id: WikiId::parse(id).expect("wiki id"),
            registration_id: ulid::Ulid::new(),
            path: PathBuf::from(format!("/{id}")),
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
    }

    #[test]
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
            &supervisor,
            &state_store,
            DaemonWatchOptions::default(),
            &mut watchers,
            &mut catalog,
        )
        .unwrap();
        assert_eq!(watchers.len(), 2);
        assert!(watchers["git-notes"].sync.is_none());
        assert!(watchers["plain"].sync.is_none());
        let observer_id = watchers["git-notes"].observer.thread().id();

        registry
            .update(
                &git_id,
                &UpdateWikiRequest {
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
            &supervisor,
            &state_store,
            DaemonWatchOptions::default(),
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
