//! Daemon-specific assembly around the reusable hosted-service supervisor.

use crate::host::{
    HostRuntimeError, HostSupervisor, RestartPolicy, ServiceDefinition, ServiceId,
    ServiceRegistration, ServiceScope,
};
use crate::http::{serve_companion_with_shutdown, CompanionHttpState};
use crate::shutdown::ShutdownSignal;
use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::runtime::Handle;

const SERVICE_STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
pub(crate) const DAEMON_SERVICE_REGISTRATION_LIMIT: usize = 8;

/// Outlasts the sequential per-service startup budget for the complete daemon
/// graph, with an additional ten seconds for process setup and readiness
/// polling outside the host supervisor.
pub const DAEMON_READINESS_TIMEOUT: Duration = Duration::from_secs(100);

const _: () = assert!(
    DAEMON_READINESS_TIMEOUT.as_secs()
        > SERVICE_STARTUP_TIMEOUT.as_secs() * DAEMON_SERVICE_REGISTRATION_LIMIT as u64
);

/// Binds the companion endpoint before service startup so its stable address
/// can be included in the eventual runtime readiness record.
pub async fn bind_companion_listener(
    requested: SocketAddr,
) -> Result<(TcpListener, SocketAddr), io::Error> {
    let listener = TcpListener::bind(requested).await?;
    let bound = listener.local_addr()?;
    Ok((listener, bound))
}

/// Starts one daemon service graph using the shared startup, rollback, and
/// status-persistence policy.
pub fn start_daemon_host(
    registrations: Vec<ServiceRegistration>,
    stop: Arc<ShutdownSignal>,
    status_path: PathBuf,
) -> Result<HostSupervisor, HostRuntimeError> {
    HostSupervisor::start_persisted_with_signal(
        registrations,
        SERVICE_STARTUP_TIMEOUT,
        stop,
        status_path,
    )
}

/// Adapts the already-bound companion listener into the blocking service
/// runner contract. The listener is intentionally non-restarting: a listener
/// failure is required-host failure, while reusing a consumed Tokio listener
/// could accidentally change the advertised endpoint.
pub fn companion_listener_service(
    listener: TcpListener,
    state: CompanionHttpState,
    runtime: Handle,
    ingress_stop: Arc<ShutdownSignal>,
    dependencies: Vec<ServiceId>,
) -> Result<ServiceRegistration, HostRuntimeError> {
    let id = ServiceId::parse("listener.companion")?;
    let listener = Arc::new(Mutex::new(Some(listener)));
    Ok(ServiceRegistration::new(
        ServiceDefinition {
            id,
            service_kind: "listener".to_string(),
            scope: ServiceScope::Global,
            enabled: true,
            required: true,
            dependencies,
            restart: RestartPolicy::Never,
        },
        move |service| {
            let listener = listener
                .lock()
                .map_err(|_| "companion listener state is unavailable".to_string())?
                .take()
                .ok_or_else(|| "companion listener was already consumed".to_string())?;
            service.ready()?;
            let stop = Arc::clone(service.stop());
            let ingress_stop = Arc::clone(&ingress_stop);
            let result = runtime
                .block_on(serve_companion_with_shutdown(
                    listener,
                    state.clone(),
                    async move {
                        tokio::select! {
                            () = stop.cancelled() => {}
                            () = ingress_stop.cancelled() => {}
                        }
                    },
                ))
                .map_err(|error| format!("companion listener failed: {error}"));
            if result.is_ok() {
                while !service.stop().wait_timeout(Duration::from_secs(1)) {}
            }
            result
        },
    ))
}
