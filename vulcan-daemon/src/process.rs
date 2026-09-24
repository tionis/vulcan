//! Long-running synchronization daemon process lifecycle.

use crate::alert_delivery::{
    spawn_alert_delivery, spawn_best_effort_desktop_delivery, AlertDeliverySender,
};
use crate::alerts::SyncAlertTracker;
use crate::companion::{CompanionResolutionAgent, CompanionSemanticAgent};
use crate::conflict_worker::run_conflict_worker;
use crate::credentials::{CompanionCredential, CompanionCredentialStore, CredentialError};
use crate::daemon_host::{
    bind_companion_listener, companion_listener_service, start_daemon_host,
    DAEMON_SERVICE_REGISTRATION_LIMIT,
};
use crate::environment::{load_daemon_environment, DaemonEnvironmentError};
use crate::final_sync::run_final_sync_and_cancel;
use crate::host::{
    load_host_status, HostRuntimeError, RestartPolicy, ServiceDefinition, ServiceId,
    ServiceRegistration, ServiceScope, ServiceStatus,
};
use crate::http::CompanionHttpState;
use crate::mutation_scheduler::{MutationScheduler, MutationSchedulerConfig};
use crate::notifications::{
    run_notification_runtime_until, NotificationRuntimeError, NotificationRuntimeOptions,
};
#[cfg(feature = "web")]
use crate::registry::DaemonAgentConfig;
use crate::registry::{RegistryError, WikiRegistrationStatus, WikiRegistry};
use crate::runtime::{
    run_sync_trigger_runtime_with_stop, SyncTriggerRuntimeError, SyncTriggerRuntimeOptions,
};
use crate::scan_runtime::{load_scan_completion, scan_status_path, ScanCompletion};
use crate::semantic_worker::run_semantic_worker;
use crate::service::DaemonServiceDiagnostic;
use crate::shutdown::ShutdownSignal;
use crate::status::{wiki_sync_status, DaemonWikiSyncStatus};
use crate::supervisor::{SupervisorError, SyncSupervisor};
use crate::sync::{
    execute_next_sync_job_with_state_store_and_engine, format_branch_diagnostic,
    format_sync_execution,
};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tempfile::NamedTempFile;
use vulcan_app::sync::GitSyncOptions;
use vulcan_app::sync_state::SyncStateStore;
use vulcan_sync::{cached_notification_advertisement, GitCliEngine, GitEngine};
use vulcan_sync::{GitBranchSync, SyncErrorCategory, SyncJobState, SyncJobTrigger};

pub const DAEMON_RUNTIME_VERSION: u32 = 1;
const RUNTIME_FILE: &str = "runtime.json";
const LOCK_FILE: &str = "process.lock";
const HTTP_RESPONSE_LIMIT: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonRuntimeRecord {
    pub version: u32,
    pub pid: u32,
    pub bind: SocketAddr,
    pub started_unix_ms: u64,
    pub credential_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DaemonStatusReport {
    pub version: u32,
    pub running: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capability_probe_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime: Option<DaemonRuntimeRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uptime_ms: Option<u64>,
    pub registered_wikis: Vec<WikiRegistrationStatus>,
    pub wiki_statuses: Vec<DaemonWikiOperationalStatus>,
    pub services: Vec<ServiceStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service: Option<DaemonServiceDiagnostic>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DaemonNotificationDiscoveryState {
    Discovered,
    NotDiscovered,
    Disabled,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DaemonNotificationDiscoveryStatus {
    pub state: DaemonNotificationDiscoveryState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DaemonWikiOperationalStatus {
    pub wiki_id: String,
    pub path: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sync: Option<DaemonWikiSyncStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sync_error: Option<String>,
    pub cache: ScanCompletion,
    pub notification: DaemonNotificationDiscoveryStatus,
}

#[derive(Debug, Clone)]
pub struct DaemonProcessContext {
    pub registry: WikiRegistry,
    pub state_root: PathBuf,
    /// Enables operational stderr lines (sync executions, notification
    /// wake-ups). Off by default; set from the global `--verbose` flag.
    pub verbose: bool,
}

impl DaemonProcessContext {
    pub fn user_default() -> Result<Self, DaemonProcessError> {
        let state_root = vulcan_core::vulcan_user_state_dir().ok_or_else(|| {
            DaemonProcessError::Configuration(
                "cannot determine the Vulcan user state directory; set XDG_STATE_HOME or HOME"
                    .to_string(),
            )
        })?;
        Ok(Self {
            registry: WikiRegistry::user_default()?,
            state_root,
            verbose: false,
        })
    }

    #[must_use]
    pub fn runtime_path(&self) -> PathBuf {
        self.state_root.join("daemon").join(RUNTIME_FILE)
    }

    fn lock_path(&self) -> PathBuf {
        self.state_root.join("daemon").join(LOCK_FILE)
    }

    fn host_status_path(&self) -> PathBuf {
        self.state_root.join("daemon").join("services.json")
    }
}

#[derive(Debug)]
pub enum DaemonProcessError {
    AlreadyRunning,
    Configuration(String),
    Registry(RegistryError),
    Credential(CredentialError),
    Environment(DaemonEnvironmentError),
    Supervisor(SupervisorError),
    Runtime(SyncTriggerRuntimeError),
    Notifications(NotificationRuntimeError),
    Io(std::io::Error),
    Json(serde_json::Error),
    Host(HostRuntimeError),
    Worker(String),
}

impl Display for DaemonProcessError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyRunning => formatter.write_str("the Vulcan daemon is already running"),
            Self::Configuration(detail) | Self::Worker(detail) => formatter.write_str(detail),
            Self::Registry(error) => Display::fmt(error, formatter),
            Self::Credential(error) => Display::fmt(error, formatter),
            Self::Environment(error) => Display::fmt(error, formatter),
            Self::Supervisor(error) => Display::fmt(error, formatter),
            Self::Runtime(error) => Display::fmt(error, formatter),
            Self::Notifications(error) => Display::fmt(error, formatter),
            Self::Io(error) => Display::fmt(error, formatter),
            Self::Json(error) => Display::fmt(error, formatter),
            Self::Host(error) => Display::fmt(error, formatter),
        }
    }
}

impl Error for DaemonProcessError {}

impl From<RegistryError> for DaemonProcessError {
    fn from(error: RegistryError) -> Self {
        Self::Registry(error)
    }
}

impl From<CredentialError> for DaemonProcessError {
    fn from(error: CredentialError) -> Self {
        Self::Credential(error)
    }
}

impl From<DaemonEnvironmentError> for DaemonProcessError {
    fn from(error: DaemonEnvironmentError) -> Self {
        Self::Environment(error)
    }
}

impl From<SupervisorError> for DaemonProcessError {
    fn from(error: SupervisorError) -> Self {
        Self::Supervisor(error)
    }
}

impl From<std::io::Error> for DaemonProcessError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for DaemonProcessError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

impl From<HostRuntimeError> for DaemonProcessError {
    fn from(error: HostRuntimeError) -> Self {
        Self::Host(error)
    }
}

pub fn run_daemon_foreground(context: &DaemonProcessContext) -> Result<(), DaemonProcessError> {
    run_daemon_foreground_with_services(context, &|_, _, _| Ok(Vec::new()))
}

/// Starts the ordinary daemon graph with host-provided ingress services.
/// The host may add protocol adapters without making the daemon import their
/// presentation-layer types; all added services share normal readiness,
/// shutdown, and status supervision.
pub fn run_daemon_foreground_with_services<F>(
    context: &DaemonProcessContext,
    additional_services: &F,
) -> Result<(), DaemonProcessError>
where
    F: Fn(
        &DaemonProcessContext,
        &crate::registry::DaemonConfig,
        &Arc<MutationScheduler>,
    ) -> Result<Vec<ServiceRegistration>, String>,
{
    let config_directory = context.registry.path().parent().ok_or_else(|| {
        DaemonProcessError::Configuration(
            "daemon registry path has no configuration directory".to_string(),
        )
    })?;
    load_daemon_environment(config_directory)?;
    let config = context.registry.load()?;
    let (resolution_agent, semantic_agent) = configured_agents(&config)?;
    let agents = (resolution_agent.map(Arc::new), semantic_agent.map(Arc::new));
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(run_daemon(
        context,
        config,
        (agents.0.clone(), agents.1.clone()),
        additional_services,
    ))
}

#[allow(clippy::too_many_lines)] // Assembles the one supervised daemon service graph.
async fn run_daemon<F>(
    context: &DaemonProcessContext,
    config: crate::registry::DaemonConfig,
    agents: (
        Option<Arc<CompanionResolutionAgent>>,
        Option<Arc<CompanionSemanticAgent>>,
    ),
    additional_services: &F,
) -> Result<(), DaemonProcessError>
where
    F: Fn(
        &DaemonProcessContext,
        &crate::registry::DaemonConfig,
        &Arc<MutationScheduler>,
    ) -> Result<Vec<ServiceRegistration>, String>,
{
    let daemon_dir = context.state_root.join("daemon");
    fs::create_dir_all(&daemon_dir)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(context.lock_path())?;
    lock.try_lock_exclusive().map_err(|error| {
        if error.kind() == fs2::lock_contended_error().kind() {
            DaemonProcessError::AlreadyRunning
        } else {
            DaemonProcessError::Io(error)
        }
    })?;

    let (resolution_agent, semantic_agent) = agents;
    validate_worker_agents(&config, resolution_agent.as_ref(), semantic_agent.as_ref())?;
    let requested_bind = configured_daemon_bind(&config.bind)?;
    let (listener, bind) = bind_companion_listener(requested_bind)
        .await
        .map_err(|error| {
            DaemonProcessError::Configuration(format!(
                "failed to bind daemon companion listener at {requested_bind}: {error}"
            ))
        })?;
    let credential = CompanionCredentialStore::at(&context.state_root).load_or_create(vec![
        "app://obsidian.md".to_string(),
        "capacitor://localhost".to_string(),
    ])?;
    let record = DaemonRuntimeRecord {
        version: DAEMON_RUNTIME_VERSION,
        pid: std::process::id(),
        bind,
        started_unix_ms: unix_time_ms()?,
        credential_id: credential.id.clone(),
    };
    let state_store = Arc::new(SyncStateStore::at(
        context.state_root.join("sync/repositories"),
    ));
    let supervisor = Arc::new(SyncSupervisor::at(
        state_store.root().join("daemon/jobs.json"),
    )?);
    log_daemon_started(context, bind, config.vaults.len());
    let stop = Arc::new(ShutdownSignal::new(false));
    let ingress_stop = Arc::new(ShutdownSignal::new(false));
    // Publishing the runtime record is the daemon's readiness boundary. Keep
    // it behind required-service startup so every successful authenticated
    // probe observes the corresponding service-health snapshot.
    let state = CompanionHttpState {
        registry: Arc::new(context.registry.clone()),
        supervisor: Arc::clone(&supervisor),
        state_store: Arc::clone(&state_store),
        credential: Arc::new(credential),
        resolution_agent,
        semantic_agent,
        shutdown: Some(Arc::clone(&stop)),
        ingress_shutdown: Some(Arc::clone(&ingress_stop)),
    };
    let runtime = tokio::runtime::Handle::current();
    let scheduler = Arc::new(
        MutationScheduler::new(MutationSchedulerConfig::default())
            .expect("default daemon scheduler limits are valid"),
    );
    let mut registrations = daemon_worker_registrations(
        context,
        &config,
        &supervisor,
        &state_store,
        state.resolution_agent.as_ref(),
        state.semantic_agent.as_ref(),
        &runtime,
    )?;
    registrations.extend(
        additional_services(context, &config, &scheduler)
            .map_err(DaemonProcessError::Configuration)?,
    );
    registrations.push(companion_listener_service(
        listener,
        state.clone(),
        runtime,
        Arc::clone(&ingress_stop),
        vec![service_id("worker.remote-notifications")?],
    )?);
    ensure_daemon_service_budget(registrations.len())?;
    let mut host = start_daemon_host(
        registrations,
        Arc::clone(&stop),
        context.host_status_path(),
        scheduler,
    )?;
    if let Err(error) = write_runtime_record(&context.runtime_path(), &record) {
        let _ = host.shutdown();
        return Err(error);
    }
    let runtime_guard = RuntimeRecordGuard {
        path: context.runtime_path(),
        pid: record.pid,
    };
    let shutdown_stop = Arc::clone(&stop);
    let graceful = wait_for_daemon_shutdown(shutdown_stop, Arc::clone(&ingress_stop)).await;
    if graceful {
        host.stop_services(&graceful_shutdown_services()?)?;
        run_final_sync_and_cancel(&state.registry, &state.supervisor, &stop).await;
    }
    let host_result = host.shutdown();
    drop(runtime_guard);
    host_result?;
    Ok(())
}

fn ensure_daemon_service_budget(service_count: usize) -> Result<(), DaemonProcessError> {
    if service_count > DAEMON_SERVICE_REGISTRATION_LIMIT {
        return Err(DaemonProcessError::Configuration(format!(
            "daemon service graph has {service_count} registrations, exceeding the readiness budget limit of {DAEMON_SERVICE_REGISTRATION_LIMIT}"
        )));
    }
    Ok(())
}

fn log_daemon_started(context: &DaemonProcessContext, bind: SocketAddr, wiki_count: usize) {
    if context.verbose {
        eprintln!("daemon started on {bind} with {wiki_count} registered wiki(s)");
    }
}

fn configured_daemon_bind(value: &str) -> Result<SocketAddr, DaemonProcessError> {
    let bind = value.parse::<SocketAddr>().map_err(|error| {
        DaemonProcessError::Configuration(format!("invalid daemon bind address `{value}`: {error}"))
    })?;
    if !bind.ip().is_loopback() {
        return Err(DaemonProcessError::Configuration(format!(
            "daemon bind address must be loopback, got {bind}"
        )));
    }
    Ok(bind)
}

#[allow(clippy::too_many_lines)] // Declaratively assembles the complete built-in service graph.
fn daemon_worker_registrations(
    context: &DaemonProcessContext,
    config: &crate::registry::DaemonConfig,
    supervisor: &Arc<SyncSupervisor>,
    state_store: &Arc<SyncStateStore>,
    resolution_agent: Option<&Arc<CompanionResolutionAgent>>,
    semantic_agent: Option<&Arc<CompanionSemanticAgent>>,
    runtime: &tokio::runtime::Handle,
) -> Result<Vec<ServiceRegistration>, DaemonProcessError> {
    validate_worker_agents(config, resolution_agent, semantic_agent)?;
    let alert_enabled = config.notifications.desktop
        || !config.notifications.webhooks.is_empty()
        || !config.notifications.commands.is_empty();
    let alert_sender = Arc::new(Mutex::new(None::<AlertDeliverySender>));
    let mut registrations = Vec::new();

    let alert_definition = worker_definition(
        "worker.alert-delivery",
        alert_enabled,
        false,
        &[],
        optional_worker_restart(),
    )?;
    let alert_config = config.notifications.clone();
    let alert_state_root = context.state_root.clone();
    let alert_registry = context.registry.clone();
    let alert_supervisor = Arc::clone(supervisor);
    let alert_slot = Arc::clone(&alert_sender);
    registrations.push(ServiceRegistration::new(
        alert_definition,
        move |service| {
            let (sender, worker) = match spawn_alert_delivery(
                &alert_config,
                &alert_state_root,
                alert_registry.clone(),
                &alert_supervisor,
                Arc::clone(service.stop()),
            ) {
                Ok(Some(delivery)) => delivery,
                Ok(None) => {
                    service.ready()?;
                    while !service.stop().wait_timeout(Duration::from_millis(50)) {}
                    return Ok(());
                }
                Err(error) if alert_config.desktop => {
                    eprintln!(
                        "level=warning event=notification_delivery_failed sink=ledger reason=startup_error; {error}"
                    );
                    spawn_best_effort_desktop_delivery(alert_config.clone())
                }
                Err(error) => return Err(error.to_string()),
            };
            *alert_slot
                .lock()
                .map_err(|_| "alert delivery sender lock is poisoned".to_string())? = Some(sender);
            service.ready()?;
            while !service.stop().wait_timeout(Duration::from_millis(50)) {}
            *alert_slot
                .lock()
                .map_err(|_| "alert delivery sender lock is poisoned".to_string())? = None;
            worker
                .join()
                .map_err(|_| "alert delivery worker panicked".to_string())
        },
    ));

    let sync_dependencies = if alert_enabled {
        vec![service_id("worker.alert-delivery")?]
    } else {
        Vec::new()
    };
    let sync_definition = worker_definition(
        "worker.sync-executor",
        true,
        true,
        &sync_dependencies,
        RestartPolicy::Never,
    )?;
    let sync_registry = context.registry.clone();
    let sync_supervisor = Arc::clone(supervisor);
    let sync_state_store = Arc::clone(state_store);
    let sync_alert_sender = Arc::clone(&alert_sender);
    let verbose = context.verbose;
    registrations.push(ServiceRegistration::new(sync_definition, move |service| {
        service.ready()?;
        run_job_worker(
            &sync_registry,
            &sync_supervisor,
            &sync_state_store,
            service.stop(),
            verbose,
            &sync_alert_sender,
        )
        .map_err(|error| error.to_string())
    }));

    let trigger_definition = worker_definition(
        "worker.sync-trigger",
        true,
        true,
        &[service_id("worker.sync-executor")?],
        RestartPolicy::Never,
    )?;
    let trigger_registry = context.registry.clone();
    let trigger_supervisor = Arc::clone(supervisor);
    let trigger_state_store = Arc::clone(state_store);
    registrations.push(ServiceRegistration::new(
        trigger_definition,
        move |service| {
            service.ready()?;
            run_sync_trigger_runtime_with_stop(
                &trigger_registry,
                &trigger_supervisor,
                &trigger_state_store,
                &SyncTriggerRuntimeOptions::default(),
                service.stop(),
            )
            .map_err(|error| error.to_string())
        },
    ));

    let notification_definition = worker_definition(
        "worker.remote-notifications",
        true,
        true,
        &[service_id("worker.sync-trigger")?],
        RestartPolicy::Never,
    )?;
    let notification_registry = context.registry.clone();
    let notification_supervisor = Arc::clone(supervisor);
    let notification_runtime = runtime.clone();
    registrations.push(ServiceRegistration::new(
        notification_definition,
        move |service| {
            service.ready()?;
            notification_runtime
                .block_on(run_notification_runtime_until(
                    notification_registry.clone(),
                    Arc::clone(&notification_supervisor),
                    NotificationRuntimeOptions {
                        verbose,
                        ..NotificationRuntimeOptions::default()
                    },
                    Arc::clone(service.stop()),
                ))
                .map_err(|error| error.to_string())
        },
    ));

    let conflict_definition = worker_definition(
        "worker.conflict",
        config.conflict_worker.is_some(),
        false,
        &[service_id("worker.sync-executor")?],
        optional_worker_restart(),
    )?;
    let conflict_config = config.conflict_worker.clone();
    let conflict_registry = context.registry.clone();
    let conflict_supervisor = Arc::clone(supervisor);
    let conflict_state_store = Arc::clone(state_store);
    let conflict_state_root = context.state_root.clone();
    let resolution_agent = resolution_agent.cloned();
    registrations.push(ServiceRegistration::new(
        conflict_definition,
        move |service| {
            let config = conflict_config
                .as_ref()
                .ok_or_else(|| "conflict worker is not configured".to_string())?;
            let agent = resolution_agent
                .as_deref()
                .ok_or_else(|| "conflict worker agent is unavailable".to_string())?;
            service.ready()?;
            run_conflict_worker(
                config,
                &conflict_registry,
                &conflict_supervisor,
                &conflict_state_store,
                &conflict_state_root,
                agent,
                service.stop(),
            )
        },
    ));

    let semantic_definition = worker_definition(
        "worker.semantic",
        config.semantic_worker.is_some(),
        false,
        &[service_id("worker.sync-executor")?],
        optional_worker_restart(),
    )?;
    let semantic_config = config.semantic_worker.clone();
    let semantic_registry = context.registry.clone();
    let semantic_supervisor = Arc::clone(supervisor);
    let semantic_state_store = Arc::clone(state_store);
    let semantic_state_root = context.state_root.clone();
    let semantic_agent = semantic_agent.cloned();
    registrations.push(ServiceRegistration::new(
        semantic_definition,
        move |service| {
            let config = semantic_config
                .as_ref()
                .ok_or_else(|| "semantic worker is not configured".to_string())?;
            let agent = semantic_agent
                .as_deref()
                .ok_or_else(|| "semantic worker agent is unavailable".to_string())?;
            service.ready()?;
            run_semantic_worker(
                config,
                &semantic_registry,
                &semantic_supervisor,
                &semantic_state_store,
                &semantic_state_root,
                agent,
                service.stop(),
            )
        },
    ));

    Ok(registrations)
}

fn service_id(value: &str) -> Result<ServiceId, DaemonProcessError> {
    ServiceId::parse(value)
        .map_err(HostRuntimeError::from)
        .map_err(DaemonProcessError::from)
}

fn worker_definition(
    id: &str,
    enabled: bool,
    required: bool,
    dependencies: &[ServiceId],
    restart: RestartPolicy,
) -> Result<ServiceDefinition, DaemonProcessError> {
    Ok(ServiceDefinition {
        id: service_id(id)?,
        service_kind: "worker".to_string(),
        scope: ServiceScope::Global,
        enabled,
        required,
        dependencies: dependencies.to_vec(),
        restart,
    })
}

const fn optional_worker_restart() -> RestartPolicy {
    RestartPolicy::BoundedOnFailure {
        max_restarts: 3,
        initial_backoff_ms: 1_000,
        max_backoff_ms: 30_000,
    }
}

fn validate_worker_agents(
    config: &crate::registry::DaemonConfig,
    resolution_agent: Option<&Arc<CompanionResolutionAgent>>,
    semantic_agent: Option<&Arc<CompanionSemanticAgent>>,
) -> Result<(), DaemonProcessError> {
    if config.semantic_worker.is_some() && semantic_agent.is_none() {
        return Err(DaemonProcessError::Configuration(
            "the semantic worker requires a configured semantic agent".to_string(),
        ));
    }
    if config.conflict_worker.is_some() && resolution_agent.is_none() {
        return Err(DaemonProcessError::Configuration(
            "the conflict worker requires a configured resolution agent".to_string(),
        ));
    }
    Ok(())
}

fn configured_agents(
    config: &crate::registry::DaemonConfig,
) -> Result<
    (
        Option<CompanionResolutionAgent>,
        Option<CompanionSemanticAgent>,
    ),
    DaemonProcessError,
> {
    #[cfg(feature = "web")]
    {
        let resolution = config
            .resolution_agent
            .as_ref()
            .map(|agent| {
                CompanionResolutionAgent::openai_compatible(
                    agent.base_url.clone(),
                    agent.model.clone(),
                    configured_api_key(agent)?,
                )
                .map_err(|error| DaemonProcessError::Configuration(error.to_string()))
            })
            .transpose()?;
        let semantic = config
            .semantic_agent
            .as_ref()
            .map(|agent| {
                CompanionSemanticAgent::openai_compatible(
                    agent.base_url.clone(),
                    agent.model.clone(),
                    configured_api_key(agent)?,
                )
                .map_err(|error| DaemonProcessError::Configuration(error.to_string()))
            })
            .transpose()?;
        Ok((resolution, semantic))
    }
    #[cfg(not(feature = "web"))]
    {
        if config.resolution_agent.is_some() || config.semantic_agent.is_some() {
            return Err(DaemonProcessError::Configuration(
                "daemon agent providers require Vulcan's `web` feature".to_string(),
            ));
        }
        Ok((None, None))
    }
}

#[cfg(feature = "web")]
fn configured_api_key(agent: &DaemonAgentConfig) -> Result<Option<String>, DaemonProcessError> {
    agent
        .api_key_env
        .as_deref()
        .map(|name| {
            std::env::var(name).map_err(|error| {
                DaemonProcessError::Configuration(format!(
                    "daemon agent credential environment variable `{name}` is unavailable: {error}"
                ))
            })
        })
        .transpose()
}

async fn wait_for_daemon_shutdown(
    stop: Arc<ShutdownSignal>,
    ingress_stop: Arc<ShutdownSignal>,
) -> bool {
    tokio::select! {
        () = stop.requested() => {
            ingress_stop.cancel();
            !stop.is_cancelled()
        }
        () = wait_for_termination_signal() => {
            let graceful = stop.begin_shutdown();
            ingress_stop.cancel();
            graceful
        }
    }
}

fn graceful_shutdown_services() -> Result<BTreeSet<ServiceId>, DaemonProcessError> {
    [
        "listener.companion",
        "worker.sync-trigger",
        "worker.remote-notifications",
        "worker.conflict",
        "worker.semantic",
    ]
    .into_iter()
    .map(service_id)
    .collect()
}

#[cfg(unix)]
async fn wait_for_termination_signal() {
    use tokio::signal::unix::{signal, SignalKind};

    let mut terminate = signal(SignalKind::terminate()).expect("install SIGTERM handler");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = terminate.recv() => {}
    }
}

#[cfg(windows)]
async fn wait_for_termination_signal() {
    let mut shutdown = tokio::signal::windows::ctrl_shutdown().expect("install shutdown handler");
    let mut close = tokio::signal::windows::ctrl_close().expect("install close handler");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = shutdown.recv() => {}
        _ = close.recv() => {}
    }
}

#[cfg(not(any(unix, windows)))]
async fn wait_for_termination_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

fn run_job_worker(
    registry: &WikiRegistry,
    supervisor: &SyncSupervisor,
    state_store: &SyncStateStore,
    stop: &ShutdownSignal,
    verbose: bool,
    alert_sender: &Mutex<Option<AlertDeliverySender>>,
) -> Result<(), DaemonProcessError> {
    let engine = vulcan_sync::GitCliEngine::default();
    let mut last_branch_diagnostics = BTreeMap::<String, String>::new();
    let retained = supervisor.list()?;
    let mut alerts = SyncAlertTracker::from_retained_jobs(&retained);
    while !stop.is_cancelled() {
        match execute_next_sync_job_with_state_store_and_engine(
            supervisor,
            registry,
            &GitSyncOptions::default(),
            state_store,
            &engine,
        )? {
            Some(execution) => {
                if verbose {
                    eprintln!("{}", format_sync_execution(&execution));
                }
                if enqueue_busy_recovery(supervisor, &execution)? {
                    continue;
                }
                let reported = alerts.observe(&execution);
                if let Some(alert) = &reported {
                    eprintln!("{}", alert.log_line());
                }
                let sender = alert_sender.lock().ok().and_then(|sender| sender.clone());
                if let Some(sender) = sender {
                    if let Err(error) = sender.observe_job(&execution.job.job, reported.as_ref()) {
                        eprintln!(
                            "level=warning event=notification_delivery_failed sink=dispatcher reason=enqueue_error; {error}",
                        );
                    }
                }
                if let Some(line) = next_branch_diagnostic(
                    execution.job.job.wiki_id.as_deref(),
                    execution
                        .report
                        .as_ref()
                        .and_then(|report| report.sync.branch.as_ref()),
                    &mut last_branch_diagnostics,
                ) {
                    eprintln!("{line}");
                }
            }
            None => supervisor.wait_for_work(stop)?,
        }
    }
    Ok(())
}

/// Gives one transient repository-lock failure an immediate supervised
/// recovery cycle. The first failure is not alerted because the recovery is
/// already durable; a second busy failure carries the `Recovery` trigger and
/// is surfaced normally instead of spinning forever.
fn enqueue_busy_recovery(
    supervisor: &SyncSupervisor,
    execution: &crate::sync::DaemonSyncExecution,
) -> Result<bool, SupervisorError> {
    let retryable_busy = execution.job.job.state == SyncJobState::Failed
        && execution
            .job
            .job
            .error
            .as_ref()
            .is_some_and(|error| error.retryable && error.category == SyncErrorCategory::Busy)
        && !execution.job.triggers.contains(&SyncJobTrigger::Recovery);
    if !retryable_busy {
        return Ok(false);
    }
    let Some(wiki_id) = execution.job.job.wiki_id.as_deref() else {
        return Ok(false);
    };
    supervisor.enqueue(wiki_id, &execution.job.job.vault, SyncJobTrigger::Recovery)?;
    Ok(true)
}

/// Returns a branch lane failure the first time it appears per wiki, so a
/// persistently failing pull strategy or push does not rely on --verbose to
/// be noticed, without repeating every cycle. Clears when the lane recovers.
fn next_branch_diagnostic(
    wiki_id: Option<&str>,
    branch: Option<&GitBranchSync>,
    last: &mut BTreeMap<String, String>,
) -> Option<String> {
    let wiki = wiki_id.unwrap_or("<unregistered>").to_string();
    let diagnostic = branch.and_then(|lane| format_branch_diagnostic(wiki_id, lane));
    if let Some(line) = diagnostic {
        if last.get(&wiki) == Some(&line) {
            return None;
        }
        last.insert(wiki, line.clone());
        return Some(line);
    }
    last.remove(&wiki);
    None
}

pub fn daemon_status(
    context: &DaemonProcessContext,
) -> Result<DaemonStatusReport, DaemonProcessError> {
    let runtime = read_runtime_record(&context.runtime_path())?;
    let registered_wikis = context.registry.list(None)?;
    let state_store = SyncStateStore::at(context.state_root.join("sync/repositories"));
    let services = load_host_status(&context.host_status_path())?
        .map(|report| report.services)
        .unwrap_or_default();
    let supervisor = SyncSupervisor::inspect_at(state_store.root().join("daemon/jobs.json"))?;
    let wiki_statuses = registered_wikis
        .iter()
        .map(|wiki| {
            let sync = wiki_sync_status(
                &context.registry,
                &supervisor,
                &state_store,
                &wiki.registration.id,
            );
            let (sync, sync_error) = match sync {
                Ok(status) => (Some(status), None),
                Err(error) => (None, Some(error.to_string())),
            };
            let cache = if wiki.registration.capabilities().markdown_index {
                load_scan_completion(&scan_status_path(
                    state_store.root(),
                    wiki.registration.registration_id,
                ))
                .map_or_else(ScanCompletion::inspection_error, |status| {
                    status.unwrap_or_else(ScanCompletion::unknown)
                })
            } else {
                ScanCompletion::disabled()
            };
            DaemonWikiOperationalStatus {
                wiki_id: wiki.registration.id.as_str().to_string(),
                path: wiki.registration.path.clone(),
                sync,
                sync_error,
                cache,
                notification: cached_notification_status(wiki),
            }
        })
        .collect();
    let (running, capability_probe_error) = runtime.as_ref().map_or((false, None), |record| {
        match authenticated_request(context, record, "GET", "/capabilities") {
            Ok(()) => (true, None),
            Err(error) => (false, Some(error.to_string())),
        }
    });
    Ok(DaemonStatusReport {
        version: DAEMON_RUNTIME_VERSION,
        running,
        capability_probe_error,
        uptime_ms: running.then(|| {
            runtime
                .as_ref()
                .and_then(|record| {
                    unix_time_ms()
                        .ok()
                        .map(|now| now.saturating_sub(record.started_unix_ms))
                })
                .unwrap_or_default()
        }),
        runtime,
        registered_wikis,
        wiki_statuses,
        services,
        service: None,
    })
}

fn cached_notification_status(wiki: &WikiRegistrationStatus) -> DaemonNotificationDiscoveryStatus {
    let empty = |state| DaemonNotificationDiscoveryStatus {
        state,
        origin: None,
        fingerprint: None,
        revision: None,
    };
    if wiki.registration.sync_paused
        || wiki
            .registration
            .sync_backend
            .as_deref()
            .is_some_and(|backend| backend != "git")
    {
        return empty(DaemonNotificationDiscoveryState::Disabled);
    }
    let engine = GitCliEngine::default();
    let Ok(repository) = engine.discover_repository(&wiki.registration.path) else {
        return empty(DaemonNotificationDiscoveryState::Unavailable);
    };
    match cached_notification_advertisement(&engine, &repository) {
        Ok(Some(discovered)) => DaemonNotificationDiscoveryStatus {
            state: DaemonNotificationDiscoveryState::Discovered,
            origin: Some(discovered.advertisement.endpoint.origin().to_string()),
            fingerprint: Some(discovered.advertisement.endpoint.fingerprint().to_string()),
            revision: Some(discovered.revision.to_string()),
        },
        Ok(None) => empty(DaemonNotificationDiscoveryState::NotDiscovered),
        Err(_) => empty(DaemonNotificationDiscoveryState::Unavailable),
    }
}

pub fn request_daemon_shutdown(
    context: &DaemonProcessContext,
) -> Result<DaemonStatusReport, DaemonProcessError> {
    let record = read_runtime_record(&context.runtime_path())?.ok_or_else(|| {
        DaemonProcessError::Configuration("the Vulcan daemon is not running".to_string())
    })?;
    authenticated_request(context, &record, "POST", "/shutdown")?;
    for _ in 0..700 {
        if read_runtime_record(&context.runtime_path())?.is_none() {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    daemon_status(context)
}

fn authenticated_request(
    context: &DaemonProcessContext,
    record: &DaemonRuntimeRecord,
    method: &str,
    path: &str,
) -> Result<(), DaemonProcessError> {
    let credential = CompanionCredentialStore::at(&context.state_root).load()?;
    if credential.id != record.credential_id {
        return Err(DaemonProcessError::Configuration(
            "daemon runtime credential identity does not match device state".to_string(),
        ));
    }
    send_http_request(record.bind, &credential, method, path)
}

fn send_http_request(
    bind: SocketAddr,
    credential: &CompanionCredential,
    method: &str,
    path: &str,
) -> Result<(), DaemonProcessError> {
    let mut stream = TcpStream::connect_timeout(&bind, Duration::from_millis(500))?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: {bind}\r\nAuthorization: Bearer {}\r\nVulcan-Protocol-Version: 1\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        credential.token
    )?;
    let mut response = Vec::new();
    stream
        .take((HTTP_RESPONSE_LIMIT + 1) as u64)
        .read_to_end(&mut response)?;
    if response.len() > HTTP_RESPONSE_LIMIT {
        return Err(DaemonProcessError::Configuration(
            "daemon HTTP response exceeded its byte limit".to_string(),
        ));
    }
    let status = response
        .split(|byte| *byte == b'\n')
        .next()
        .unwrap_or_default();
    if !status.starts_with(b"HTTP/1.1 2") {
        return Err(DaemonProcessError::Configuration(format!(
            "daemon HTTP request failed: {}",
            String::from_utf8_lossy(status).trim()
        )));
    }
    Ok(())
}

fn unix_time_ms() -> Result<u64, DaemonProcessError> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| DaemonProcessError::Configuration(error.to_string()))?
        .as_millis();
    u64::try_from(millis)
        .map_err(|_| DaemonProcessError::Configuration("system time is out of range".to_string()))
}

fn write_runtime_record(
    path: &Path,
    record: &DaemonRuntimeRecord,
) -> Result<(), DaemonProcessError> {
    let parent = path.parent().ok_or_else(|| {
        DaemonProcessError::Configuration("daemon runtime path has no parent".to_string())
    })?;
    fs::create_dir_all(parent)?;
    let bytes = serde_json::to_vec_pretty(record)?;
    let mut temporary = NamedTempFile::new_in(parent)?;
    temporary.write_all(&bytes)?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(())
}

fn read_runtime_record(path: &Path) -> Result<Option<DaemonRuntimeRecord>, DaemonProcessError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    read_runtime_record_after_metadata(path, &metadata)
}

fn read_runtime_record_after_metadata(
    path: &Path,
    metadata: &fs::Metadata,
) -> Result<Option<DaemonRuntimeRecord>, DaemonProcessError> {
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > 64 * 1024 {
        return Err(DaemonProcessError::Configuration(format!(
            "daemon runtime record at {} is not a bounded regular file",
            path.display()
        )));
    }
    // The daemon removes its record during shutdown. Disappearance after the
    // metadata check is the same stopped state as disappearance before it.
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let record: DaemonRuntimeRecord = serde_json::from_slice(&bytes)?;
    if record.version != DAEMON_RUNTIME_VERSION || !record.bind.ip().is_loopback() {
        return Err(DaemonProcessError::Configuration(format!(
            "invalid daemon runtime record at {}",
            path.display()
        )));
    }
    Ok(Some(record))
}

struct RuntimeRecordGuard {
    path: PathBuf,
    pid: u32,
}

impl Drop for RuntimeRecordGuard {
    fn drop(&mut self) {
        if read_runtime_record(&self.path)
            .ok()
            .flatten()
            .is_some_and(|record| record.pid == self.pid)
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{
        AddWikiRequest, DaemonConfig, DaemonConflictWorkerConfig, ManagedDirectoryProfile, WikiId,
        WikiRegistration,
    };
    use std::process::Command;
    use ulid::Ulid;

    fn git(directory: &Path, arguments: &[&str]) -> bool {
        Command::new("git")
            .current_dir(directory)
            .args(arguments)
            .output()
            .expect("run git")
            .status
            .success()
    }

    fn git_sync_config(temporary: &tempfile::TempDir) -> DaemonConfig {
        let remote = temporary.path().join("remote.git");
        assert!(git(
            temporary.path(),
            &[
                "init",
                "--quiet",
                "--bare",
                remote.to_str().expect("remote")
            ]
        ));
        let vault = temporary.path().join("vault");
        fs::create_dir(&vault).expect("vault directory");
        assert!(git(
            &vault,
            &["-c", "init.defaultBranch=main", "init", "--quiet"]
        ));
        assert!(git(
            &vault,
            &["remote", "add", "origin", remote.to_str().expect("remote")]
        ));
        fs::write(vault.join("Home.md"), "daemon sync\n").expect("note");
        assert!(git(&vault, &["add", "--all"]));
        assert!(git(
            &vault,
            &[
                "-c",
                "user.name=Vulcan Test",
                "-c",
                "user.email=vulcan@example.invalid",
                "commit",
                "--quiet",
                "-m",
                "base"
            ]
        ));
        DaemonConfig {
            bind: "127.0.0.1:0".to_string(),
            vaults: vec![WikiRegistration {
                profile: crate::registry::ManagedDirectoryProfile::Knowledge,
                profile_version: None,
                materialization: crate::registry::MaterializationProfile::Full,
                id: WikiId::parse("notes").expect("wiki ID"),
                registration_id: Ulid::new(),
                path: vault,
                groups: Vec::new(),
                git_dir: None,
                permissions_profile: None,
                sync_backend: Some("git".to_string()),
                platform_profile: None,
                sync_paused: false,
            }],
            ..DaemonConfig::default()
        }
    }

    #[test]
    fn conflict_worker_requires_a_resolution_agent() {
        let config = DaemonConfig {
            conflict_worker: Some(DaemonConflictWorkerConfig {
                wikis: vec![WikiId::parse("notes").expect("wiki ID")],
                remote: "origin".to_string(),
                live_ref: "refs/heads/__vulcan-sync/live".to_string(),
                max_groups_per_run: 1,
                poll_seconds: 30,
            }),
            ..DaemonConfig::default()
        };
        let error = validate_worker_agents(&config, None, None)
            .expect_err("worker without provider must fail");
        assert!(error
            .to_string()
            .contains("conflict worker requires a configured resolution agent"));
    }

    #[test]
    fn daemon_service_graph_cannot_outgrow_the_cli_readiness_budget() {
        ensure_daemon_service_budget(DAEMON_SERVICE_REGISTRATION_LIMIT)
            .expect("configured service limit fits the readiness budget");
        let error = ensure_daemon_service_budget(DAEMON_SERVICE_REGISTRATION_LIMIT + 1)
            .expect_err("oversized service graph must fail closed");
        assert!(error.to_string().contains("exceeding the readiness budget"));
    }

    #[test]
    fn host_provided_ingress_is_ready_and_stops_with_the_daemon() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let registry = WikiRegistry::at(temporary.path().join("daemon.toml"));
        let config = DaemonConfig {
            bind: "127.0.0.1:0".to_string(),
            ..DaemonConfig::default()
        };
        fs::write(
            registry.path(),
            toml::to_string_pretty(&config).expect("serialize config"),
        )
        .expect("write registry");
        let context = DaemonProcessContext {
            registry,
            state_root: temporary.path().join("state"),
            verbose: false,
        };
        let child_context = context.clone();
        let (result_sender, result_receiver) = std::sync::mpsc::channel();
        let daemon = thread::spawn(move || {
            let result = run_daemon_foreground_with_services(&child_context, &|_, _, _| {
                Ok(vec![ServiceRegistration::new(
                    ServiceDefinition {
                        id: ServiceId::parse("listener.test-ingress").expect("service ID"),
                        service_kind: "listener".to_string(),
                        scope: ServiceScope::Global,
                        enabled: true,
                        required: true,
                        dependencies: vec![
                            ServiceId::parse("worker.sync-trigger").expect("dependency")
                        ],
                        restart: RestartPolicy::Never,
                    },
                    |service| {
                        service.ready()?;
                        while !service.stop().wait_timeout(Duration::from_millis(20)) {}
                        Ok(())
                    },
                )])
            });
            result_sender.send(result).expect("send daemon result");
        });

        let status = (0..100)
            .find_map(|_| {
                if let Ok(result) = result_receiver.try_recv() {
                    panic!("daemon stopped before readiness: {result:?}");
                }
                let status = daemon_status(&context).ok()?;
                if status.running {
                    Some(status)
                } else {
                    thread::sleep(Duration::from_millis(25));
                    None
                }
            })
            .expect("daemon becomes ready");
        assert_eq!(status.services.len(), 8);
        assert!(status.services.iter().any(|service| {
            service.id.as_str() == "listener.test-ingress"
                && service.state == crate::host::ServiceLifecycleState::Ready
        }));
        request_daemon_shutdown(&context).expect("request shutdown");
        daemon.join().expect("daemon thread");
        result_receiver
            .recv()
            .expect("daemon result")
            .expect("clean daemon shutdown");
    }

    fn assert_daemon_sync_attempted(context: &DaemonProcessContext) {
        let status = daemon_status(context).expect("synchronized daemon status");
        let wiki_status = status.wiki_statuses.first().expect("per-wiki status");
        assert!(wiki_status
            .sync
            .as_ref()
            .expect("sync status")
            .last_attempt_unix_ms
            .is_some());
        assert_eq!(
            wiki_status.notification.state,
            DaemonNotificationDiscoveryState::NotDiscovered
        );
    }

    #[test]
    fn first_busy_failure_enqueues_one_recovery_before_alerting() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let supervisor =
            SyncSupervisor::at(temporary.path().join("jobs.json")).expect("supervisor");
        let failed = crate::sync::DaemonSyncExecution {
            job: crate::supervisor::SupervisedSyncJob {
                job: vulcan_sync::SyncJob {
                    version: vulcan_sync::SYNC_CONTRACT_VERSION,
                    id: "failed-job".to_string(),
                    wiki_id: Some("notes".to_string()),
                    backend: "git".to_string(),
                    vault: temporary.path().join("vault"),
                    trigger: SyncJobTrigger::Resume,
                    state: SyncJobState::Failed,
                    status: None,
                    error: Some(vulcan_sync::SyncError::new(
                        SyncErrorCategory::Busy,
                        "repository lock is held",
                        true,
                    )),
                },
                triggers: vec![SyncJobTrigger::Resume],
                watch: None,
            },
            report: None,
        };

        assert!(enqueue_busy_recovery(&supervisor, &failed).expect("schedule recovery"));
        let recovery = supervisor
            .claim_next()
            .expect("claim recovery")
            .expect("recovery job");
        assert_eq!(recovery.job.triggers, vec![SyncJobTrigger::Recovery]);

        let mut repeated = failed;
        repeated.job.triggers = vec![SyncJobTrigger::Recovery];
        assert!(!enqueue_busy_recovery(&supervisor, &repeated).expect("bound recovery"));
    }

    #[cfg(feature = "web")]
    #[test]
    fn configured_agents_are_constructed_without_exposing_credentials() {
        let agent = DaemonAgentConfig {
            base_url: "http://127.0.0.1:9/v1".to_string(),
            model: "test-model".to_string(),
            api_key_env: None,
        };
        let config = DaemonConfig {
            resolution_agent: Some(agent.clone()),
            semantic_agent: Some(agent),
            ..DaemonConfig::default()
        };
        let (resolution, semantic) = configured_agents(&config).expect("configured agents");
        assert!(resolution.is_some());
        assert!(semantic.is_some());

        let missing_name = "VULCAN_TEST_MISSING_DAEMON_AGENT_KEY_7F3C9B";
        let missing = DaemonConfig {
            resolution_agent: Some(DaemonAgentConfig {
                base_url: "http://127.0.0.1:9/v1".to_string(),
                model: "test-model".to_string(),
                api_key_env: Some(missing_name.to_string()),
            }),
            ..DaemonConfig::default()
        };
        let error = configured_agents(&missing)
            .err()
            .expect("missing credential must fail");
        assert!(error.to_string().contains(missing_name));
    }

    #[cfg(not(feature = "web"))]
    #[test]
    fn configured_agents_fail_closed_without_web_support() {
        let config = DaemonConfig {
            resolution_agent: Some(crate::registry::DaemonAgentConfig {
                base_url: "http://127.0.0.1:9/v1".to_string(),
                model: "test-model".to_string(),
                api_key_env: None,
            }),
            ..DaemonConfig::default()
        };
        let error = configured_agents(&config)
            .err()
            .expect("provider requires web support");
        assert!(error.to_string().contains("`web` feature"));
    }

    #[test]
    fn foreground_daemon_pulls_the_tracked_branch_on_startup() {
        fn rev_parse(directory: &Path, revision: &str) -> String {
            let output = Command::new("git")
                .current_dir(directory)
                .args(["rev-parse", revision])
                .output()
                .expect("run git rev-parse");
            assert!(output.status.success(), "rev-parse should succeed");
            String::from_utf8(output.stdout)
                .expect("rev-parse output should be UTF-8")
                .trim()
                .to_string()
        }

        let temporary = tempfile::tempdir().expect("temporary directory");
        let registry_path = temporary.path().join("daemon.toml");
        let registry = WikiRegistry::at(registry_path.clone());
        let config = git_sync_config(&temporary);
        let remote = temporary.path().join("remote.git");
        let vault = config.vaults[0].path.clone();
        assert!(git(&vault, &["push", "--quiet", "-u", "origin", "main"]));
        let upstream = temporary.path().join("upstream-work");
        assert!(git(
            temporary.path(),
            &[
                "clone",
                "--quiet",
                "-b",
                "main",
                remote.to_str().expect("remote"),
                upstream.to_str().expect("upstream worktree"),
            ]
        ));
        fs::write(upstream.join("Home.md"), "daemon sync advanced\n").expect("note");
        assert!(git(&upstream, &["add", "--all"]));
        assert!(git(
            &upstream,
            &[
                "-c",
                "user.name=Vulcan Test",
                "-c",
                "user.email=vulcan@example.invalid",
                "commit",
                "--quiet",
                "-m",
                "advanced",
            ]
        ));
        assert!(git(&upstream, &["push", "--quiet", "origin", "main"]));
        let expected = rev_parse(&remote, "refs/heads/main");
        assert_ne!(
            rev_parse(&vault, "HEAD"),
            expected,
            "fixture should start behind the remote"
        );
        fs::write(
            &registry_path,
            toml::to_string_pretty(&config).expect("serialize config"),
        )
        .expect("write registry");
        let context = DaemonProcessContext {
            registry,
            state_root: temporary.path().join("state"),
            verbose: false,
        };
        let child_context = context.clone();
        let (result_sender, result_receiver) = std::sync::mpsc::channel();
        let daemon = thread::spawn(move || {
            let result = run_daemon_foreground(&child_context);
            result_sender.send(result).expect("send daemon result");
        });

        let status = (0..100)
            .find_map(|_| {
                if let Ok(result) = result_receiver.try_recv() {
                    panic!("daemon stopped before readiness: {result:?}");
                }
                let status = daemon_status(&context).ok()?;
                if status.running {
                    Some(status)
                } else {
                    thread::sleep(Duration::from_millis(25));
                    None
                }
            })
            .expect("daemon becomes ready");
        assert_eq!(status.registered_wikis.len(), 1);
        let pulled = (0..200).any(|_| {
            if rev_parse(&vault, "HEAD") == expected {
                true
            } else {
                thread::sleep(Duration::from_millis(50));
                false
            }
        });
        assert!(pulled, "daemon startup should pull the tracked branch");
        let stopped = request_daemon_shutdown(&context).expect("request shutdown");
        assert!(!stopped.running);
        daemon.join().expect("daemon thread");
        result_receiver
            .recv()
            .expect("daemon result channel")
            .expect("daemon result");
    }

    #[test]
    #[allow(clippy::too_many_lines)] // End-to-end lifecycle assertions share one daemon fixture.
    fn foreground_process_reports_status_and_stops_over_authenticated_http() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let registry_path = temporary.path().join("daemon.toml");
        let registry = WikiRegistry::at(registry_path.clone());
        let config = git_sync_config(&temporary);
        let remote = temporary.path().join("remote.git");
        let vault = config.vaults[0].path.clone();
        fs::write(
            &registry_path,
            toml::to_string_pretty(&config).expect("serialize config"),
        )
        .expect("write registry");
        let context = DaemonProcessContext {
            registry,
            state_root: temporary.path().join("state"),
            verbose: false,
        };
        let child_context = context.clone();
        let (result_sender, result_receiver) = std::sync::mpsc::channel();
        let daemon = thread::spawn(move || {
            let result = run_daemon_foreground(&child_context);
            result_sender.send(result).expect("send daemon result");
        });

        let status = (0..100)
            .find_map(|_| {
                if let Ok(result) = result_receiver.try_recv() {
                    panic!("daemon stopped before readiness: {result:?}");
                }
                let status = daemon_status(&context).ok()?;
                if status.running {
                    Some(status)
                } else {
                    thread::sleep(Duration::from_millis(25));
                    None
                }
            })
            .expect("daemon becomes ready");
        assert_eq!(status.registered_wikis.len(), 1);
        assert_eq!(status.wiki_statuses.len(), 1);
        assert_eq!(status.services.len(), 7);
        assert!(status.services.iter().any(|service| {
            service.id.as_str() == "listener.companion"
                && service.state == crate::host::ServiceLifecycleState::Ready
        }));
        assert!(status.services.iter().any(|service| {
            service.id.as_str() == "worker.sync-executor"
                && service.state == crate::host::ServiceLifecycleState::Ready
        }));
        assert!(status.services.iter().any(|service| {
            service.id.as_str() == "worker.semantic"
                && service.state == crate::host::ServiceLifecycleState::Disabled
        }));
        assert!(status.uptime_ms.is_some());
        let daemon_bind = status.runtime.as_ref().expect("runtime record").bind;
        assert!(daemon_bind.ip().is_loopback());
        let synchronized = (0..100).any(|_| {
            if git(
                temporary.path(),
                &[
                    "--git-dir",
                    remote.to_str().expect("remote"),
                    "rev-parse",
                    "--verify",
                    "refs/heads/__vulcan-sync/live",
                ],
            ) {
                true
            } else {
                thread::sleep(Duration::from_millis(50));
                false
            }
        });
        assert!(synchronized, "startup reconciliation should sync the wiki");
        assert_daemon_sync_attempted(&context);
        let cache_became_fresh = (0..100).any(|_| {
            let fresh = daemon_status(&context).is_ok_and(|status| {
                status.wiki_statuses.iter().any(|wiki| {
                    wiki.wiki_id == "notes"
                        && wiki.cache.state == crate::scan_runtime::CacheFreshnessState::Fresh
                        && wiki.cache.generation > 0
                })
            });
            if !fresh {
                thread::sleep(Duration::from_millis(25));
            }
            fresh
        });
        assert!(cache_became_fresh, "hosted cache scan should complete");

        fs::write(vault.join("Last-minute.md"), "captured during shutdown\n")
            .expect("last-minute note");

        let stopped = request_daemon_shutdown(&context).expect("request shutdown");
        assert!(!stopped.running);
        assert!(stopped.services.iter().all(|service| matches!(
            service.state,
            crate::host::ServiceLifecycleState::Stopped
                | crate::host::ServiceLifecycleState::Disabled
        )));
        daemon.join().expect("daemon thread");
        result_receiver
            .recv()
            .expect("daemon result channel")
            .expect("daemon result");
        assert!(!context.runtime_path().exists());
        let rebound = std::net::TcpListener::bind(daemon_bind).expect("companion port released");
        drop(rebound);
        assert!(git(
            temporary.path(),
            &[
                "--git-dir",
                remote.to_str().expect("remote"),
                "cat-file",
                "-e",
                "refs/heads/__vulcan-sync/live:Last-minute.md",
            ]
        ));
        let supervisor = SyncSupervisor::at(
            SyncStateStore::at(context.state_root.join("sync/repositories"))
                .root()
                .join("daemon/jobs.json"),
        )
        .expect("supervisor");
        assert!(supervisor.list().expect("jobs").iter().any(|job| {
            job.triggers.contains(&SyncJobTrigger::Shutdown)
                && job.job.state == SyncJobState::Succeeded
        }));
    }

    #[test]
    fn branch_diagnostics_deduplicate_until_recovery() {
        use vulcan_sync::{GitBranchSync, GitBranchSyncAction, GitRefName};

        fn lane(action: GitBranchSyncAction) -> GitBranchSync {
            GitBranchSync {
                branch: GitRefName::parse("refs/heads/main").expect("branch"),
                remote: None,
                upstream: None,
                tracking: None,
                before: None,
                after: None,
                action,
                detail: Some("boom".to_string()),
                pushed: false,
                push_detail: None,
            }
        }

        let mut last = BTreeMap::new();
        let failed = lane(GitBranchSyncAction::Failed);
        let first = next_branch_diagnostic(Some("alpha"), Some(&failed), &mut last);
        assert!(first.is_some_and(|line| line.contains("alpha") && line.contains("boom")));
        assert_eq!(
            next_branch_diagnostic(Some("alpha"), Some(&failed), &mut last),
            None,
            "identical failures must not repeat every cycle"
        );
        assert_eq!(
            next_branch_diagnostic(Some("alpha"), None, &mut last),
            None,
            "recovery clears without logging"
        );
        assert!(
            next_branch_diagnostic(Some("alpha"), Some(&failed), &mut last).is_some(),
            "a later failure reports again after recovery"
        );
        let mut other = BTreeMap::new();
        assert_eq!(
            next_branch_diagnostic(
                Some("beta"),
                Some(&lane(GitBranchSyncAction::FastForwarded)),
                &mut other,
            ),
            None,
            "healthy lanes never report"
        );
    }

    #[test]
    fn runtime_records_reject_symlinks() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let target = temporary.path().join("target.json");
        fs::write(&target, "{}").expect("target");
        #[cfg(unix)]
        let link = temporary.path().join("runtime.json");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&target, &link).expect("symlink");
            let error = read_runtime_record(&link).expect_err("symlink must fail");
            assert!(error.to_string().contains("bounded regular file"));
        }
    }

    #[test]
    fn runtime_record_disappearing_during_shutdown_is_stopped() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let path = temporary.path().join("runtime.json");
        fs::write(&path, "{}").expect("runtime fixture");
        let metadata = fs::symlink_metadata(&path).expect("metadata before shutdown");
        fs::remove_file(&path).expect("daemon removes runtime record");
        assert!(read_runtime_record_after_metadata(&path, &metadata)
            .expect("disappearance is a stopped state")
            .is_none());
    }

    #[test]
    fn status_distinguishes_an_unresponsive_runtime_from_a_stopped_daemon() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let context = DaemonProcessContext {
            registry: WikiRegistry::at(temporary.path().join("daemon.toml")),
            state_root: temporary.path().join("state"),
            verbose: false,
        };
        let credential = CompanionCredentialStore::at(&context.state_root)
            .load_or_create(vec!["app://obsidian.md".to_string()])
            .expect("companion credential");
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("temporary listener");
        let bind = listener.local_addr().expect("listener address");
        drop(listener);
        write_runtime_record(
            &context.runtime_path(),
            &DaemonRuntimeRecord {
                version: DAEMON_RUNTIME_VERSION,
                pid: std::process::id(),
                bind,
                started_unix_ms: unix_time_ms().expect("current time"),
                credential_id: credential.id,
            },
        )
        .expect("runtime record");

        let status = daemon_status(&context).expect("daemon status");
        assert!(!status.running);
        assert!(status.runtime.is_some());
        assert!(status.capability_probe_error.is_some());
    }

    #[test]
    fn files_only_daemon_status_reports_disabled_cache_without_initializing_index() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let directory = temporary.path().join("media");
        fs::create_dir(&directory).expect("media directory");
        fs::write(directory.join("clip.bin"), b"plain bytes").expect("media file");
        let registry = WikiRegistry::at(temporary.path().join("config/daemon.toml"));
        registry
            .add(
                &AddWikiRequest {
                    id: WikiId::parse("media").expect("wiki ID"),
                    path: directory.clone(),
                    profile: Some(ManagedDirectoryProfile::FilesOnly),
                    groups: Vec::new(),
                    git_dir: None,
                    permissions_profile: None,
                    sync_backend: Some("none".to_string()),
                    platform_profile: None,
                },
                false,
            )
            .expect("register files-only directory");
        let context = DaemonProcessContext {
            registry,
            state_root: temporary.path().join("state"),
            verbose: false,
        };
        let status = daemon_status(&context).expect("offline daemon status");
        assert_eq!(status.wiki_statuses.len(), 1);
        assert_eq!(
            status.wiki_statuses[0].cache.state,
            crate::scan_runtime::CacheFreshnessState::Disabled
        );
        assert!(!directory.join(".vulcan").exists());
    }
}
