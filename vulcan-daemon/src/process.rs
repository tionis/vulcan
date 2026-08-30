//! Long-running synchronization daemon process lifecycle.

use crate::companion::{CompanionResolutionAgent, CompanionSemanticAgent};
use crate::credentials::{CompanionCredential, CompanionCredentialStore, CredentialError};
use crate::http::{serve_companion_with_shutdown, CompanionHttpState};
#[cfg(feature = "web")]
use crate::registry::DaemonAgentConfig;
use crate::registry::{RegistryError, WikiRegistrationStatus, WikiRegistry};
use crate::runtime::{
    run_sync_trigger_runtime_until, SyncTriggerRuntimeError, SyncTriggerRuntimeOptions,
};
use crate::semantic_worker::spawn_semantic_worker;
use crate::supervisor::{SupervisorError, SyncSupervisor};
use crate::sync::execute_next_sync_job_with_state_store;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tempfile::NamedTempFile;
use tokio::net::TcpListener;
use vulcan_app::sync::GitSyncOptions;
use vulcan_app::sync_state::SyncStateStore;

pub const DAEMON_RUNTIME_VERSION: u32 = 1;
const RUNTIME_FILE: &str = "runtime.json";
const LOCK_FILE: &str = "process.lock";
const JOB_POLL: Duration = Duration::from_millis(100);
const SHUTDOWN_POLL: Duration = Duration::from_millis(50);
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
    pub runtime: Option<DaemonRuntimeRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uptime_ms: Option<u64>,
    pub registered_wikis: Vec<WikiRegistrationStatus>,
}

#[derive(Debug, Clone)]
pub struct DaemonProcessContext {
    pub registry: WikiRegistry,
    pub state_root: PathBuf,
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
        })
    }

    #[must_use]
    pub fn runtime_path(&self) -> PathBuf {
        self.state_root.join("daemon").join(RUNTIME_FILE)
    }

    fn lock_path(&self) -> PathBuf {
        self.state_root.join("daemon").join(LOCK_FILE)
    }
}

#[derive(Debug)]
pub enum DaemonProcessError {
    AlreadyRunning,
    Configuration(String),
    Registry(RegistryError),
    Credential(CredentialError),
    Supervisor(SupervisorError),
    Runtime(SyncTriggerRuntimeError),
    Io(std::io::Error),
    Json(serde_json::Error),
    Worker(String),
}

impl Display for DaemonProcessError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyRunning => formatter.write_str("the Vulcan daemon is already running"),
            Self::Configuration(detail) | Self::Worker(detail) => formatter.write_str(detail),
            Self::Registry(error) => Display::fmt(error, formatter),
            Self::Credential(error) => Display::fmt(error, formatter),
            Self::Supervisor(error) => Display::fmt(error, formatter),
            Self::Runtime(error) => Display::fmt(error, formatter),
            Self::Io(error) => Display::fmt(error, formatter),
            Self::Json(error) => Display::fmt(error, formatter),
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

pub fn run_daemon_foreground(context: &DaemonProcessContext) -> Result<(), DaemonProcessError> {
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
    ))
}

async fn run_daemon(
    context: &DaemonProcessContext,
    config: crate::registry::DaemonConfig,
    agents: (
        Option<Arc<CompanionResolutionAgent>>,
        Option<Arc<CompanionSemanticAgent>>,
    ),
) -> Result<(), DaemonProcessError> {
    let daemon_dir = context.state_root.join("daemon");
    fs::create_dir_all(&daemon_dir)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(context.lock_path())?;
    lock.try_lock_exclusive().map_err(|error| {
        if error.kind() == std::io::ErrorKind::WouldBlock {
            DaemonProcessError::AlreadyRunning
        } else {
            DaemonProcessError::Io(error)
        }
    })?;

    let (resolution_agent, semantic_agent) = agents;
    if config.semantic_worker.is_some() && semantic_agent.is_none() {
        return Err(DaemonProcessError::Configuration(
            "the semantic worker requires a configured semantic agent".to_string(),
        ));
    }
    let requested_bind = config.bind.parse::<SocketAddr>().map_err(|error| {
        DaemonProcessError::Configuration(format!(
            "invalid daemon bind address `{}`: {error}",
            config.bind
        ))
    })?;
    if !requested_bind.ip().is_loopback() {
        return Err(DaemonProcessError::Configuration(format!(
            "daemon bind address must be loopback, got {requested_bind}"
        )));
    }
    let listener = TcpListener::bind(requested_bind).await?;
    let bind = listener.local_addr()?;
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
    write_runtime_record(&context.runtime_path(), &record)?;
    let runtime_guard = RuntimeRecordGuard {
        path: context.runtime_path(),
        pid: record.pid,
    };

    let state_store = Arc::new(SyncStateStore::at(
        context.state_root.join("sync/repositories"),
    ));
    let supervisor = Arc::new(SyncSupervisor::at(
        state_store.root().join("daemon/jobs.json"),
    )?);
    let stop = Arc::new(AtomicBool::new(false));
    let workers = DaemonWorkers::spawn(
        context,
        &config,
        &supervisor,
        &state_store,
        semantic_agent.as_ref(),
        &stop,
    );
    let state = CompanionHttpState {
        registry: Arc::new(context.registry.clone()),
        supervisor,
        state_store,
        credential: Arc::new(credential),
        resolution_agent,
        semantic_agent,
        shutdown: Some(Arc::clone(&stop)),
    };
    let shutdown_stop = Arc::clone(&stop);
    let serve = serve_companion_with_shutdown(listener, state, async move {
        tokio::select! {
            () = wait_for_stop(Arc::clone(&shutdown_stop)) => {}
            signal = tokio::signal::ctrl_c() => {
                if signal.is_ok() {
                    shutdown_stop.store(true, Ordering::Release);
                }
            }
        }
    })
    .await;
    stop.store(true, Ordering::Release);
    let workers_result = workers.join();
    drop(runtime_guard);
    serve?;
    workers_result?;
    Ok(())
}

struct DaemonWorkers {
    trigger: thread::JoinHandle<Result<(), DaemonProcessError>>,
    sync: thread::JoinHandle<Result<(), DaemonProcessError>>,
    semantic: Option<thread::JoinHandle<Result<(), String>>>,
}

impl DaemonWorkers {
    fn spawn(
        context: &DaemonProcessContext,
        config: &crate::registry::DaemonConfig,
        supervisor: &Arc<SyncSupervisor>,
        state_store: &Arc<SyncStateStore>,
        semantic_agent: Option<&Arc<CompanionSemanticAgent>>,
        stop: &Arc<AtomicBool>,
    ) -> Self {
        Self {
            trigger: spawn_trigger_runtime(
                context.registry.clone(),
                Arc::clone(supervisor),
                Arc::clone(state_store),
                Arc::clone(stop),
            ),
            sync: spawn_job_worker(
                context.registry.clone(),
                Arc::clone(supervisor),
                Arc::clone(state_store),
                Arc::clone(stop),
            ),
            semantic: config.semantic_worker.clone().map(|worker_config| {
                spawn_semantic_worker(
                    worker_config,
                    context.registry.clone(),
                    Arc::clone(supervisor),
                    Arc::clone(state_store),
                    context.state_root.clone(),
                    Arc::clone(semantic_agent.expect("semantic worker agent was validated")),
                    Arc::clone(stop),
                )
            }),
        }
    }

    fn join(self) -> Result<(), DaemonProcessError> {
        self.trigger.join().map_err(|_| {
            DaemonProcessError::Worker("daemon trigger runtime panicked".to_string())
        })??;
        self.sync
            .join()
            .map_err(|_| DaemonProcessError::Worker("daemon sync worker panicked".to_string()))??;
        if let Some(worker) = self.semantic {
            worker
                .join()
                .map_err(|_| {
                    DaemonProcessError::Worker("daemon semantic worker panicked".to_string())
                })?
                .map_err(DaemonProcessError::Worker)?;
        }
        Ok(())
    }
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

async fn wait_for_stop(stop: Arc<AtomicBool>) {
    while !stop.load(Ordering::Acquire) {
        tokio::time::sleep(SHUTDOWN_POLL).await;
    }
}

fn spawn_trigger_runtime(
    registry: WikiRegistry,
    supervisor: Arc<SyncSupervisor>,
    state_store: Arc<SyncStateStore>,
    stop: Arc<AtomicBool>,
) -> thread::JoinHandle<Result<(), DaemonProcessError>> {
    thread::spawn(move || {
        let result = run_sync_trigger_runtime_until(
            &registry,
            &supervisor,
            &state_store,
            &SyncTriggerRuntimeOptions::default(),
            || stop.load(Ordering::Acquire),
        )
        .map_err(DaemonProcessError::Runtime);
        if result.is_err() {
            stop.store(true, Ordering::Release);
        }
        result
    })
}

fn spawn_job_worker(
    registry: WikiRegistry,
    supervisor: Arc<SyncSupervisor>,
    state_store: Arc<SyncStateStore>,
    stop: Arc<AtomicBool>,
) -> thread::JoinHandle<Result<(), DaemonProcessError>> {
    thread::spawn(move || {
        let result = (|| {
            while !stop.load(Ordering::Acquire) {
                let execution = execute_next_sync_job_with_state_store(
                    &supervisor,
                    &registry,
                    &GitSyncOptions::default(),
                    &state_store,
                )?;
                if execution.is_none() {
                    thread::sleep(JOB_POLL);
                }
            }
            Ok(())
        })();
        if result.is_err() {
            stop.store(true, Ordering::Release);
        }
        result
    })
}

pub fn daemon_status(
    context: &DaemonProcessContext,
) -> Result<DaemonStatusReport, DaemonProcessError> {
    let runtime = read_runtime_record(&context.runtime_path())?;
    let registered_wikis = context.registry.list(None)?;
    let running = runtime.as_ref().is_some_and(|record| {
        authenticated_request(context, record, "GET", "/capabilities").is_ok()
    });
    Ok(DaemonStatusReport {
        version: DAEMON_RUNTIME_VERSION,
        running,
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
    })
}

pub fn request_daemon_shutdown(
    context: &DaemonProcessContext,
) -> Result<DaemonStatusReport, DaemonProcessError> {
    let record = read_runtime_record(&context.runtime_path())?.ok_or_else(|| {
        DaemonProcessError::Configuration("the Vulcan daemon is not running".to_string())
    })?;
    authenticated_request(context, &record, "POST", "/shutdown")?;
    for _ in 0..100 {
        if TcpStream::connect_timeout(&record.bind, Duration::from_millis(50)).is_err() {
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
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink()
                || !metadata.is_file()
                || metadata.len() > 64 * 1024
            {
                return Err(DaemonProcessError::Configuration(format!(
                    "daemon runtime record at {} is not a bounded regular file",
                    path.display()
                )));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    let record: DaemonRuntimeRecord = serde_json::from_slice(&fs::read(path)?)?;
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
    use crate::registry::{DaemonConfig, WikiId, WikiRegistration};
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
    fn foreground_process_reports_status_and_stops_over_authenticated_http() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let registry_path = temporary.path().join("daemon.toml");
        let registry = WikiRegistry::at(registry_path.clone());
        let config = git_sync_config(&temporary);
        let remote = temporary.path().join("remote.git");
        fs::write(
            &registry_path,
            toml::to_string_pretty(&config).expect("serialize config"),
        )
        .expect("write registry");
        let context = DaemonProcessContext {
            registry,
            state_root: temporary.path().join("state"),
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
        assert!(status.uptime_ms.is_some());
        assert!(status
            .runtime
            .expect("runtime record")
            .bind
            .ip()
            .is_loopback());
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

        let stopped = request_daemon_shutdown(&context).expect("request shutdown");
        assert!(!stopped.running);
        daemon.join().expect("daemon thread");
        result_receiver
            .recv()
            .expect("daemon result channel")
            .expect("daemon result");
        assert!(!context.runtime_path().exists());
    }

    #[test]
    fn runtime_records_reject_symlinks() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let target = temporary.path().join("target.json");
        fs::write(&target, "{}").expect("target");
        let link = temporary.path().join("runtime.json");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&target, &link).expect("symlink");
            let error = read_runtime_record(&link).expect_err("symlink must fail");
            assert!(error.to_string().contains("bounded regular file"));
        }
    }
}
