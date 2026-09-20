//! Transport-neutral service definitions and lifecycle projections for Vulcan hosts.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt::{Display, Formatter};

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
