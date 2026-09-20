//! Transport-neutral service definitions and lifecycle projections for Vulcan hosts.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::shutdown::ShutdownSignal;

const MAX_SERVICE_ID_BYTES: usize = 160;
const MAX_FAILURE_DETAIL_BYTES: usize = 512;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ServiceId(String);

impl ServiceId {
    pub fn parse(value: impl Into<String>) -> Result<Self, HostDefinitionError> {
        let value = value.into();
        if value.is_empty() || value.len() > MAX_SERVICE_ID_BYTES {
            return Err(HostDefinitionError::InvalidServiceId(value));
        }
        if !value.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'.' | b'-' | b'/' | b'_')
        }) || value.starts_with(['.', '-', '/', '_'])
            || value.ends_with(['.', '-', '/', '_'])
            || !value.contains('.')
        {
            return Err(HostDefinitionError::InvalidServiceId(value));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for ServiceId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ServiceScope {
    Global,
    Vault { registration_id: String },
    Instance { instance_id: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum RestartPolicy {
    Never,
    OnFailure,
    BoundedOnFailure {
        max_restarts: u32,
        initial_backoff_ms: u64,
        max_backoff_ms: u64,
    },
}

impl RestartPolicy {
    fn validate(self, id: &ServiceId) -> Result<(), HostDefinitionError> {
        if let Self::BoundedOnFailure {
            max_restarts,
            initial_backoff_ms,
            max_backoff_ms,
        } = self
        {
            if max_restarts == 0 || initial_backoff_ms == 0 || max_backoff_ms < initial_backoff_ms {
                return Err(HostDefinitionError::InvalidRestartPolicy(id.clone()));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceDefinition {
    pub id: ServiceId,
    pub service_kind: String,
    pub scope: ServiceScope,
    pub enabled: bool,
    pub required: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<ServiceId>,
    pub restart: RestartPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceLifecycleState {
    Disabled,
    Starting,
    Ready,
    Degraded,
    Restarting,
    Failed,
    Stopping,
    Stopped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceFailure {
    pub category: String,
    pub detail: String,
}

impl ServiceFailure {
    #[must_use]
    pub fn sanitized(category: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            category: bounded_text(category.into(), MAX_FAILURE_DETAIL_BYTES),
            detail: bounded_text(detail.into(), MAX_FAILURE_DETAIL_BYTES),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceStatus {
    pub id: ServiceId,
    pub service_kind: String,
    pub scope: ServiceScope,
    pub required: bool,
    pub state: ServiceLifecycleState,
    pub ready: bool,
    pub start_count: u32,
    pub restart_count: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_transition_unix_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_failure: Option<ServiceFailure>,
}

#[derive(Debug)]
pub struct ServiceCatalog {
    definitions: BTreeMap<ServiceId, ServiceDefinition>,
    startup_order: Vec<ServiceId>,
    statuses: BTreeMap<ServiceId, ServiceStatus>,
}

impl ServiceCatalog {
    pub fn new(definitions: Vec<ServiceDefinition>) -> Result<Self, HostDefinitionError> {
        let definitions = validate_definitions(definitions)?;
        let startup_order = dependency_order(&definitions)?;
        let statuses = definitions
            .iter()
            .map(|(id, definition)| {
                (
                    id.clone(),
                    ServiceStatus {
                        id: id.clone(),
                        service_kind: definition.service_kind.clone(),
                        scope: definition.scope.clone(),
                        required: definition.required,
                        state: if definition.enabled {
                            ServiceLifecycleState::Stopped
                        } else {
                            ServiceLifecycleState::Disabled
                        },
                        ready: false,
                        start_count: 0,
                        restart_count: 0,
                        last_transition_unix_ms: None,
                        last_failure: None,
                    },
                )
            })
            .collect();
        Ok(Self {
            definitions,
            startup_order,
            statuses,
        })
    }

    #[must_use]
    pub fn startup_order(&self) -> &[ServiceId] {
        &self.startup_order
    }

    #[must_use]
    pub fn shutdown_order(&self) -> Vec<ServiceId> {
        self.startup_order.iter().rev().cloned().collect()
    }

    #[must_use]
    pub fn definition(&self, id: &ServiceId) -> Option<&ServiceDefinition> {
        self.definitions.get(id)
    }

    #[must_use]
    pub fn statuses(&self) -> Vec<ServiceStatus> {
        self.statuses.values().cloned().collect()
    }

    pub fn transition(
        &mut self,
        id: &ServiceId,
        state: ServiceLifecycleState,
        now_unix_ms: u64,
        failure: Option<ServiceFailure>,
    ) -> Result<(), HostDefinitionError> {
        let status = self
            .statuses
            .get_mut(id)
            .ok_or_else(|| HostDefinitionError::UnknownService(id.clone()))?;
        if status.state == ServiceLifecycleState::Disabled
            && state != ServiceLifecycleState::Disabled
        {
            return Err(HostDefinitionError::InvalidTransition {
                id: id.clone(),
                from: status.state,
                to: state,
            });
        }
        if !valid_transition(status.state, state) {
            return Err(HostDefinitionError::InvalidTransition {
                id: id.clone(),
                from: status.state,
                to: state,
            });
        }
        if state == ServiceLifecycleState::Starting {
            status.start_count = status.start_count.saturating_add(1);
        }
        if state == ServiceLifecycleState::Restarting {
            status.restart_count = status.restart_count.saturating_add(1);
        }
        status.state = state;
        status.ready = state == ServiceLifecycleState::Ready;
        status.last_transition_unix_ms = Some(now_unix_ms);
        if failure.is_some() {
            status.last_failure = failure;
        }
        Ok(())
    }

    #[must_use]
    pub fn required_services_ready(&self) -> bool {
        self.definitions.iter().all(|(id, definition)| {
            !definition.enabled
                || !definition.required
                || self.statuses.get(id).is_some_and(|status| status.ready)
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostDefinitionError {
    InvalidServiceId(String),
    InvalidServiceKind(ServiceId),
    InvalidScope(ServiceId),
    InvalidRestartPolicy(ServiceId),
    DuplicateService(ServiceId),
    UnknownDependency {
        service: ServiceId,
        dependency: ServiceId,
    },
    DisabledDependency {
        service: ServiceId,
        dependency: ServiceId,
    },
    DependencyCycle(Vec<ServiceId>),
    UnknownService(ServiceId),
    InvalidTransition {
        id: ServiceId,
        from: ServiceLifecycleState,
        to: ServiceLifecycleState,
    },
}

impl Display for HostDefinitionError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidServiceId(id) => write!(formatter, "invalid host service id `{id}`"),
            Self::InvalidServiceKind(id) => {
                write!(formatter, "host service `{id}` has an invalid service kind")
            }
            Self::InvalidScope(id) => write!(formatter, "host service `{id}` has an invalid scope"),
            Self::InvalidRestartPolicy(id) => {
                write!(
                    formatter,
                    "host service `{id}` has an invalid restart policy"
                )
            }
            Self::DuplicateService(id) => write!(formatter, "duplicate host service `{id}`"),
            Self::UnknownDependency {
                service,
                dependency,
            } => write!(
                formatter,
                "host service `{service}` depends on unknown service `{dependency}`"
            ),
            Self::DisabledDependency {
                service,
                dependency,
            } => write!(
                formatter,
                "enabled host service `{service}` depends on disabled service `{dependency}`"
            ),
            Self::DependencyCycle(ids) => write!(
                formatter,
                "host service dependency cycle includes {}",
                ids.iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::UnknownService(id) => write!(formatter, "unknown host service `{id}`"),
            Self::InvalidTransition { id, from, to } => write!(
                formatter,
                "host service `{id}` cannot transition from {from:?} to {to:?}"
            ),
        }
    }
}

impl Error for HostDefinitionError {}

fn validate_definitions(
    definitions: Vec<ServiceDefinition>,
) -> Result<BTreeMap<ServiceId, ServiceDefinition>, HostDefinitionError> {
    let mut indexed = BTreeMap::new();
    for mut definition in definitions {
        if definition.service_kind.is_empty()
            || definition.service_kind.len() > 64
            || !definition
                .service_kind
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
        {
            return Err(HostDefinitionError::InvalidServiceKind(definition.id));
        }
        validate_scope(&definition)?;
        definition.restart.validate(&definition.id)?;
        definition.dependencies.sort();
        definition.dependencies.dedup();
        let id = definition.id.clone();
        if indexed.insert(id.clone(), definition).is_some() {
            return Err(HostDefinitionError::DuplicateService(id));
        }
    }
    for (id, definition) in &indexed {
        for dependency in &definition.dependencies {
            let Some(target) = indexed.get(dependency) else {
                return Err(HostDefinitionError::UnknownDependency {
                    service: id.clone(),
                    dependency: dependency.clone(),
                });
            };
            if definition.enabled && !target.enabled {
                return Err(HostDefinitionError::DisabledDependency {
                    service: id.clone(),
                    dependency: dependency.clone(),
                });
            }
        }
    }
    Ok(indexed)
}

fn validate_scope(definition: &ServiceDefinition) -> Result<(), HostDefinitionError> {
    let valid = match &definition.scope {
        ServiceScope::Global => true,
        ServiceScope::Vault { registration_id } => valid_scope_identity(registration_id),
        ServiceScope::Instance { instance_id } => valid_scope_identity(instance_id),
    };
    if valid {
        Ok(())
    } else {
        Err(HostDefinitionError::InvalidScope(definition.id.clone()))
    }
}

fn valid_scope_identity(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 160
        && !value.chars().any(char::is_control)
        && !value.contains(['/', '\\'])
}

fn dependency_order(
    definitions: &BTreeMap<ServiceId, ServiceDefinition>,
) -> Result<Vec<ServiceId>, HostDefinitionError> {
    fn visit(
        id: &ServiceId,
        definitions: &BTreeMap<ServiceId, ServiceDefinition>,
        visiting: &mut BTreeSet<ServiceId>,
        visited: &mut BTreeSet<ServiceId>,
        order: &mut Vec<ServiceId>,
    ) -> Result<(), HostDefinitionError> {
        if visited.contains(id) {
            return Ok(());
        }
        if !visiting.insert(id.clone()) {
            return Err(HostDefinitionError::DependencyCycle(
                visiting.iter().cloned().collect(),
            ));
        }
        let definition = definitions
            .get(id)
            .ok_or_else(|| HostDefinitionError::UnknownService(id.clone()))?;
        if definition.enabled {
            for dependency in &definition.dependencies {
                visit(dependency, definitions, visiting, visited, order)?;
            }
            order.push(id.clone());
        }
        visiting.remove(id);
        visited.insert(id.clone());
        Ok(())
    }

    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    let mut order = Vec::new();
    for id in definitions.keys() {
        visit(id, definitions, &mut visiting, &mut visited, &mut order)?;
    }
    Ok(order)
}

const fn valid_transition(from: ServiceLifecycleState, to: ServiceLifecycleState) -> bool {
    use ServiceLifecycleState::{
        Degraded, Disabled, Failed, Ready, Restarting, Starting, Stopped, Stopping,
    };
    matches!(
        (from, to),
        (Disabled, Disabled)
            | (Stopped, Starting | Stopped)
            | (Starting, Ready | Degraded | Failed | Stopping)
            | (Ready, Degraded | Failed | Stopping)
            | (Degraded, Ready | Failed | Restarting | Stopping)
            | (Failed, Restarting | Stopping | Stopped)
            | (Restarting, Starting | Failed | Stopping)
            | (Stopping, Stopped | Failed)
    )
}

fn bounded_text(mut value: String, limit: usize) -> String {
    if value.len() <= limit {
        return value;
    }
    let mut end = limit.saturating_sub(3);
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value.truncate(end);
    value.push_str("...");
    value
}

type ServiceRunner = dyn Fn(ServiceRunContext) -> Result<(), String> + Send + Sync + 'static;

#[derive(Clone)]
pub struct ServiceRegistration {
    pub definition: ServiceDefinition,
    runner: Arc<ServiceRunner>,
}

impl ServiceRegistration {
    pub fn new<F>(definition: ServiceDefinition, runner: F) -> Self
    where
        F: Fn(ServiceRunContext) -> Result<(), String> + Send + Sync + 'static,
    {
        Self {
            definition,
            runner: Arc::new(runner),
        }
    }
}

impl std::fmt::Debug for ServiceRegistration {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ServiceRegistration")
            .field("definition", &self.definition)
            .field("runner", &"<service runner>")
            .finish()
    }
}

#[derive(Clone)]
pub struct ServiceRunContext {
    stop: Arc<ShutdownSignal>,
    readiness: ServiceReadiness,
}

impl ServiceRunContext {
    pub fn ready(&self) -> Result<(), String> {
        self.readiness.ready().map_err(|error| error.to_string())
    }

    #[must_use]
    pub fn stop(&self) -> &Arc<ShutdownSignal> {
        &self.stop
    }
}

#[derive(Clone)]
struct ServiceReadiness {
    id: ServiceId,
    ready: Arc<AtomicBool>,
    catalog: Arc<Mutex<ServiceCatalog>>,
    startup: mpsc::Sender<StartupEvent>,
}

impl ServiceReadiness {
    fn ready(&self) -> Result<(), HostRuntimeError> {
        if self.ready.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        transition_shared(&self.catalog, &self.id, ServiceLifecycleState::Ready, None)?;
        let _ = self.startup.send(StartupEvent::Ready(self.id.clone()));
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct HostStatusHandle {
    catalog: Arc<Mutex<ServiceCatalog>>,
}

impl HostStatusHandle {
    pub fn statuses(&self) -> Result<Vec<ServiceStatus>, HostRuntimeError> {
        Ok(self
            .catalog
            .lock()
            .map_err(|_| HostRuntimeError::Poisoned)?
            .statuses())
    }

    pub fn required_services_ready(&self) -> Result<bool, HostRuntimeError> {
        Ok(self
            .catalog
            .lock()
            .map_err(|_| HostRuntimeError::Poisoned)?
            .required_services_ready())
    }
}

#[derive(Debug)]
struct RunningService {
    id: ServiceId,
    stop: Arc<ShutdownSignal>,
    handle: JoinHandle<()>,
}

#[derive(Debug)]
pub struct HostSupervisor {
    status: HostStatusHandle,
    host_stop: Arc<ShutdownSignal>,
    services: Vec<RunningService>,
}

impl HostSupervisor {
    pub fn start(
        registrations: Vec<ServiceRegistration>,
        startup_timeout: Duration,
    ) -> Result<Self, HostRuntimeError> {
        if startup_timeout.is_zero() {
            return Err(HostRuntimeError::InvalidStartupTimeout);
        }
        let mut runners = BTreeMap::new();
        let mut definitions = Vec::with_capacity(registrations.len());
        for registration in registrations {
            let id = registration.definition.id.clone();
            if runners.insert(id.clone(), registration.runner).is_some() {
                return Err(HostDefinitionError::DuplicateService(id).into());
            }
            definitions.push(registration.definition);
        }
        let catalog = ServiceCatalog::new(definitions)?;
        let startup_order = catalog.startup_order().to_vec();
        let status = HostStatusHandle {
            catalog: Arc::new(Mutex::new(catalog)),
        };
        let host_stop = Arc::new(ShutdownSignal::default());
        let mut supervisor = Self {
            status,
            host_stop,
            services: Vec::new(),
        };

        for id in startup_order {
            let definition = supervisor
                .status
                .catalog
                .lock()
                .map_err(|_| HostRuntimeError::Poisoned)?
                .definition(&id)
                .cloned()
                .ok_or_else(|| HostRuntimeError::UnknownRunner(id.clone()))?;
            let runner = runners
                .remove(&id)
                .ok_or_else(|| HostRuntimeError::UnknownRunner(id.clone()))?;
            let (sender, receiver) = mpsc::channel();
            let service_stop = Arc::new(ShutdownSignal::default());
            let handle = spawn_service_controller(
                definition.clone(),
                runner,
                Arc::clone(&supervisor.status.catalog),
                Arc::clone(&supervisor.host_stop),
                Arc::clone(&service_stop),
                sender,
            )?;
            supervisor.services.push(RunningService {
                id: id.clone(),
                stop: service_stop,
                handle,
            });
            match receiver.recv_timeout(startup_timeout) {
                Ok(StartupEvent::Ready(ready)) if ready == id => {}
                Ok(StartupEvent::Failed(failed, detail)) if failed == id => {
                    if definition.required {
                        supervisor.rollback();
                        return Err(HostRuntimeError::RequiredStartupFailed { id, detail });
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    supervisor.rollback();
                    return Err(HostRuntimeError::StartupTimeout(id));
                }
                Ok(_) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                    supervisor.rollback();
                    return Err(HostRuntimeError::StartupProtocol(id));
                }
            }
        }
        Ok(supervisor)
    }

    #[must_use]
    pub fn status_handle(&self) -> HostStatusHandle {
        self.status.clone()
    }

    #[must_use]
    pub fn shutdown_signal(&self) -> Arc<ShutdownSignal> {
        Arc::clone(&self.host_stop)
    }

    pub fn shutdown(mut self) -> Result<Vec<ServiceStatus>, HostRuntimeError> {
        self.stop_and_join()?;
        self.status.statuses()
    }

    fn rollback(&mut self) {
        self.host_stop.cancel();
        let _ = self.stop_and_join();
    }

    fn stop_and_join(&mut self) -> Result<(), HostRuntimeError> {
        self.host_stop.cancel();
        let mut first_error = None;
        while let Some(service) = self.services.pop() {
            let _ = transition_shared(
                &self.status.catalog,
                &service.id,
                ServiceLifecycleState::Stopping,
                None,
            );
            service.stop.cancel();
            if service.handle.join().is_err() && first_error.is_none() {
                first_error = Some(HostRuntimeError::ControllerPanicked(service.id.clone()));
            }
            let _ = transition_shared(
                &self.status.catalog,
                &service.id,
                ServiceLifecycleState::Stopped,
                None,
            );
        }
        first_error.map_or(Ok(()), Err)
    }
}

#[derive(Debug)]
enum StartupEvent {
    Ready(ServiceId),
    Failed(ServiceId, String),
}

#[derive(Debug)]
pub enum HostRuntimeError {
    Definition(HostDefinitionError),
    InvalidStartupTimeout,
    UnknownRunner(ServiceId),
    Spawn { id: ServiceId, detail: String },
    StartupTimeout(ServiceId),
    StartupProtocol(ServiceId),
    RequiredStartupFailed { id: ServiceId, detail: String },
    ControllerPanicked(ServiceId),
    Clock(String),
    Poisoned,
}

impl Display for HostRuntimeError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Definition(error) => Display::fmt(error, formatter),
            Self::InvalidStartupTimeout => {
                formatter.write_str("host service startup timeout must be non-zero")
            }
            Self::UnknownRunner(id) => write!(formatter, "host service `{id}` has no runner"),
            Self::Spawn { id, detail } => {
                write!(formatter, "cannot start host service `{id}`: {detail}")
            }
            Self::StartupTimeout(id) => {
                write!(
                    formatter,
                    "host service `{id}` did not become ready in time"
                )
            }
            Self::StartupProtocol(id) => {
                write!(
                    formatter,
                    "host service `{id}` violated the readiness protocol"
                )
            }
            Self::RequiredStartupFailed { id, detail } => {
                write!(
                    formatter,
                    "required host service `{id}` failed to start: {detail}"
                )
            }
            Self::ControllerPanicked(id) => {
                write!(formatter, "host service controller `{id}` panicked")
            }
            Self::Clock(detail) => write!(formatter, "host clock error: {detail}"),
            Self::Poisoned => formatter.write_str("host service state lock is poisoned"),
        }
    }
}

impl Error for HostRuntimeError {}

impl From<HostDefinitionError> for HostRuntimeError {
    fn from(error: HostDefinitionError) -> Self {
        Self::Definition(error)
    }
}

fn spawn_service_controller(
    definition: ServiceDefinition,
    runner: Arc<ServiceRunner>,
    catalog: Arc<Mutex<ServiceCatalog>>,
    host_stop: Arc<ShutdownSignal>,
    service_stop: Arc<ShutdownSignal>,
    startup: mpsc::Sender<StartupEvent>,
) -> Result<JoinHandle<()>, HostRuntimeError> {
    let id = definition.id.clone();
    thread::Builder::new()
        .name(format!("vulcan-host-{id}"))
        .spawn(move || {
            let mut attempts = 0_u32;
            loop {
                if service_stop.is_cancelled() {
                    break;
                }
                let state = if attempts == 0 {
                    ServiceLifecycleState::Starting
                } else {
                    let _ = transition_shared(
                        &catalog,
                        &definition.id,
                        ServiceLifecycleState::Restarting,
                        None,
                    );
                    ServiceLifecycleState::Starting
                };
                if transition_shared(&catalog, &definition.id, state, None).is_err() {
                    host_stop.cancel();
                    break;
                }
                let ready = Arc::new(AtomicBool::new(false));
                let context = ServiceRunContext {
                    stop: Arc::clone(&service_stop),
                    readiness: ServiceReadiness {
                        id: definition.id.clone(),
                        ready: Arc::clone(&ready),
                        catalog: Arc::clone(&catalog),
                        startup: startup.clone(),
                    },
                };
                let result = catch_unwind(AssertUnwindSafe(|| runner(context)))
                    .map_err(|_| "service runner panicked".to_string())
                    .and_then(|result| result);
                if service_stop.is_cancelled() {
                    break;
                }
                let detail = result
                    .err()
                    .unwrap_or_else(|| "service exited unexpectedly".to_string());
                if !ready.load(Ordering::Acquire) {
                    let _ = startup.send(StartupEvent::Failed(
                        definition.id.clone(),
                        bounded_text(detail.clone(), MAX_FAILURE_DETAIL_BYTES),
                    ));
                }
                let failure = ServiceFailure::sanitized("service_exit", detail);
                let can_restart = restart_delay(definition.restart, attempts);
                if let Some(delay) = can_restart {
                    let _ = transition_shared(
                        &catalog,
                        &definition.id,
                        ServiceLifecycleState::Degraded,
                        Some(failure),
                    );
                    attempts = attempts.saturating_add(1);
                    if service_stop.wait_timeout(delay) {
                        break;
                    }
                    continue;
                }
                let _ = transition_shared(
                    &catalog,
                    &definition.id,
                    ServiceLifecycleState::Failed,
                    Some(failure),
                );
                if definition.required {
                    host_stop.cancel();
                }
                break;
            }
        })
        .map_err(|error| HostRuntimeError::Spawn {
            id,
            detail: error.to_string(),
        })
}

fn restart_delay(policy: RestartPolicy, completed_restarts: u32) -> Option<Duration> {
    match policy {
        RestartPolicy::OnFailure => Some(exponential_backoff(100, 30_000, completed_restarts)),
        RestartPolicy::BoundedOnFailure {
            max_restarts,
            initial_backoff_ms,
            max_backoff_ms,
        } if completed_restarts < max_restarts => Some(exponential_backoff(
            initial_backoff_ms,
            max_backoff_ms,
            completed_restarts,
        )),
        RestartPolicy::Never | RestartPolicy::BoundedOnFailure { .. } => None,
    }
}

fn exponential_backoff(initial_ms: u64, maximum_ms: u64, exponent: u32) -> Duration {
    let factor = 1_u64.checked_shl(exponent.min(62)).unwrap_or(u64::MAX);
    Duration::from_millis(initial_ms.saturating_mul(factor).min(maximum_ms))
}

fn transition_shared(
    catalog: &Mutex<ServiceCatalog>,
    id: &ServiceId,
    state: ServiceLifecycleState,
    failure: Option<ServiceFailure>,
) -> Result<(), HostRuntimeError> {
    catalog
        .lock()
        .map_err(|_| HostRuntimeError::Poisoned)?
        .transition(id, state, unix_time_ms()?, failure)?;
    Ok(())
}

fn unix_time_ms() -> Result<u64, HostRuntimeError> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| HostRuntimeError::Clock(error.to_string()))?
        .as_millis();
    u64::try_from(millis).map_err(|error| HostRuntimeError::Clock(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;
    use std::sync::mpsc as test_mpsc;

    fn id(value: &str) -> ServiceId {
        ServiceId::parse(value).expect("service id")
    }

    fn service(value: &str, dependencies: &[&str]) -> ServiceDefinition {
        ServiceDefinition {
            id: id(value),
            service_kind: "worker".to_string(),
            scope: ServiceScope::Global,
            enabled: true,
            required: true,
            dependencies: dependencies.iter().map(|value| id(value)).collect(),
            restart: RestartPolicy::Never,
        }
    }

    #[test]
    fn dependency_order_is_deterministic_and_shutdown_reverses_it() {
        let catalog = ServiceCatalog::new(vec![
            service("worker.sync", &["worker.trigger"]),
            service("endpoint.companion", &["worker.sync"]),
            service("worker.trigger", &[]),
        ])
        .expect("catalog");
        assert_eq!(
            catalog.startup_order(),
            &[
                id("worker.trigger"),
                id("worker.sync"),
                id("endpoint.companion")
            ]
        );
        assert_eq!(
            catalog.shutdown_order(),
            vec![
                id("endpoint.companion"),
                id("worker.sync"),
                id("worker.trigger")
            ]
        );
    }

    #[test]
    fn invalid_graphs_and_disabled_dependencies_fail_before_startup() {
        let cycle = ServiceCatalog::new(vec![
            service("worker.alpha", &["worker.beta"]),
            service("worker.beta", &["worker.alpha"]),
        ])
        .expect_err("cycle");
        assert!(matches!(cycle, HostDefinitionError::DependencyCycle(_)));

        let mut disabled = service("worker.disabled", &[]);
        disabled.enabled = false;
        let error = ServiceCatalog::new(vec![
            disabled,
            service("endpoint.required", &["worker.disabled"]),
        ])
        .expect_err("disabled dependency");
        assert!(matches!(
            error,
            HostDefinitionError::DisabledDependency { .. }
        ));
    }

    #[test]
    fn lifecycle_reports_readiness_restarts_and_sanitized_failures() {
        let service_id = id("worker.sync");
        let mut catalog = ServiceCatalog::new(vec![service("worker.sync", &[])])
            .expect("catalog should validate");
        assert!(!catalog.required_services_ready());
        catalog
            .transition(&service_id, ServiceLifecycleState::Starting, 10, None)
            .unwrap();
        catalog
            .transition(&service_id, ServiceLifecycleState::Ready, 20, None)
            .unwrap();
        assert!(catalog.required_services_ready());
        catalog
            .transition(
                &service_id,
                ServiceLifecycleState::Degraded,
                30,
                Some(ServiceFailure::sanitized("worker_error", "x".repeat(900))),
            )
            .unwrap();
        catalog
            .transition(&service_id, ServiceLifecycleState::Restarting, 40, None)
            .unwrap();
        let status = catalog.statuses().pop().expect("status");
        assert_eq!(status.start_count, 1);
        assert_eq!(status.restart_count, 1);
        assert!(!status.ready);
        assert!(status.last_failure.unwrap().detail.len() <= 512);
    }

    #[test]
    fn disabled_services_remain_visible_and_cannot_be_started() {
        let mut definition = service("worker.optional", &[]);
        definition.enabled = false;
        definition.required = false;
        let mut catalog = ServiceCatalog::new(vec![definition]).unwrap();
        let service_id = id("worker.optional");
        assert_eq!(catalog.statuses()[0].state, ServiceLifecycleState::Disabled);
        assert!(catalog.required_services_ready());
        assert!(matches!(
            catalog.transition(&service_id, ServiceLifecycleState::Starting, 1, None),
            Err(HostDefinitionError::InvalidTransition { .. })
        ));
    }

    #[test]
    fn service_ids_scopes_and_restart_bounds_are_validated() {
        assert!(ServiceId::parse("Worker.Sync").is_err());
        assert!(ServiceId::parse("worker").is_err());
        let mut definition = service("worker.sync", &[]);
        definition.scope = ServiceScope::Vault {
            registration_id: "../vault".to_string(),
        };
        assert!(matches!(
            ServiceCatalog::new(vec![definition]),
            Err(HostDefinitionError::InvalidScope(_))
        ));

        let mut definition = service("worker.sync", &[]);
        definition.restart = RestartPolicy::BoundedOnFailure {
            max_restarts: 0,
            initial_backoff_ms: 10,
            max_backoff_ms: 5,
        };
        assert!(matches!(
            ServiceCatalog::new(vec![definition]),
            Err(HostDefinitionError::InvalidRestartPolicy(_))
        ));
    }

    #[test]
    fn supervisor_starts_in_dependency_order_and_stops_in_reverse_order() {
        let (sender, receiver) = test_mpsc::channel();
        let registrations = vec![
            registration_with_events("worker.first", &[], sender.clone()),
            registration_with_events("worker.second", &["worker.first"], sender),
        ];
        let supervisor = HostSupervisor::start(registrations, Duration::from_secs(1)).unwrap();
        assert!(supervisor
            .status_handle()
            .required_services_ready()
            .unwrap());
        let statuses = supervisor.shutdown().unwrap();
        assert!(statuses
            .iter()
            .all(|status| status.state == ServiceLifecycleState::Stopped));
        let events = receiver.try_iter().collect::<Vec<_>>();
        assert_eq!(
            events,
            [
                "start:worker.first",
                "start:worker.second",
                "stop:worker.second",
                "stop:worker.first"
            ]
        );
    }

    #[test]
    fn required_startup_failure_rolls_back_started_services() {
        let (sender, receiver) = test_mpsc::channel();
        let first = registration_with_events("worker.first", &[], sender);
        let second = ServiceRegistration::new(service("worker.second", &["worker.first"]), |_| {
            Err("cannot initialize".to_string())
        });
        let error = HostSupervisor::start(vec![first, second], Duration::from_secs(1))
            .expect_err("required startup should fail");
        assert!(matches!(
            error,
            HostRuntimeError::RequiredStartupFailed { .. }
        ));
        assert_eq!(
            receiver.recv_timeout(Duration::from_secs(1)).unwrap(),
            "start:worker.first"
        );
        assert_eq!(
            receiver.recv_timeout(Duration::from_secs(1)).unwrap(),
            "stop:worker.first"
        );
    }

    #[test]
    fn optional_service_restarts_then_exposes_exhausted_failure() {
        let attempts = Arc::new(AtomicU32::new(0));
        let runner_attempts = Arc::clone(&attempts);
        let mut definition = service("worker.optional", &[]);
        definition.required = false;
        definition.restart = RestartPolicy::BoundedOnFailure {
            max_restarts: 2,
            initial_backoff_ms: 1,
            max_backoff_ms: 2,
        };
        let registration = ServiceRegistration::new(definition, move |context| {
            context.ready()?;
            runner_attempts.fetch_add(1, Ordering::SeqCst);
            Err("boom".to_string())
        });
        let supervisor = HostSupervisor::start(vec![registration], Duration::from_secs(1)).unwrap();
        let status = supervisor.status_handle();
        for _ in 0..100 {
            if status.statuses().unwrap()[0].state == ServiceLifecycleState::Failed {
                break;
            }
            thread::sleep(Duration::from_millis(2));
        }
        let report = status.statuses().unwrap().pop().unwrap();
        assert_eq!(report.state, ServiceLifecycleState::Failed);
        assert_eq!(report.restart_count, 2);
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
        assert!(!supervisor.shutdown_signal().is_cancelled());
        supervisor.shutdown().unwrap();
    }

    #[test]
    fn required_service_exit_requests_host_shutdown() {
        let (release_sender, release_receiver) = test_mpsc::channel();
        let release_receiver = Arc::new(Mutex::new(release_receiver));
        let registration = ServiceRegistration::new(service("worker.required", &[]), {
            let release_receiver = Arc::clone(&release_receiver);
            move |context| {
                context.ready()?;
                release_receiver.lock().unwrap().recv().unwrap();
                Ok(())
            }
        });
        let supervisor = HostSupervisor::start(vec![registration], Duration::from_secs(1)).unwrap();
        let host_stop = supervisor.shutdown_signal();
        release_sender.send(()).unwrap();
        for _ in 0..100 {
            if host_stop.is_cancelled() {
                break;
            }
            thread::sleep(Duration::from_millis(2));
        }
        assert!(host_stop.is_cancelled());
        supervisor.shutdown().unwrap();
    }

    fn registration_with_events(
        id_value: &str,
        dependencies: &[&str],
        sender: test_mpsc::Sender<String>,
    ) -> ServiceRegistration {
        let definition = service(id_value, dependencies);
        let service_id = id_value.to_string();
        ServiceRegistration::new(definition, move |context| {
            sender.send(format!("start:{service_id}")).unwrap();
            context.ready()?;
            while !context.stop().wait_timeout(Duration::from_millis(5)) {}
            sender.send(format!("stop:{service_id}")).unwrap();
            Ok(())
        })
    }
}
