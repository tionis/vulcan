//! Durable, best-effort delivery of daemon attention events.

use crate::alerts::{deliver_desktop, SyncAlert};
use crate::registry::{
    DaemonCommandNotificationConfig, DaemonNotificationConfig, DaemonWebhookFormat,
    DaemonWebhookNotificationConfig, WikiId, WikiRegistry,
};
use crate::supervisor::SyncSupervisor;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tempfile::NamedTempFile;
use vulcan_core::{
    resolve_permission_profile, PermissionGuard, ProfilePermissionGuard, VaultPaths,
};

const LEDGER_VERSION: u32 = 1;
const LEDGER_FILE: &str = "alert-delivery.json";
const MAX_LEDGER_BYTES: u64 = 1024 * 1024;
const MAX_RECORDS: usize = 512;
const MAX_TARGETS_PER_RECORD: usize = 17;
const RETRY_BASE_MS: u64 = 30_000;
const RETRY_MAX_MS: u64 = 3_600_000;
const DELIVERY_TIMEOUT: Duration = Duration::from_secs(10);
const WORKER_POLL: Duration = Duration::from_secs(30);

#[derive(Debug)]
pub enum AlertDeliveryError {
    InvalidState(String),
    Io(std::io::Error),
    Json(serde_json::Error),
}

impl Display for AlertDeliveryError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidState(detail) => formatter.write_str(detail),
            Self::Io(error) => Display::fmt(error, formatter),
            Self::Json(error) => Display::fmt(error, formatter),
        }
    }
}

impl Error for AlertDeliveryError {}

impl From<std::io::Error> for AlertDeliveryError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for AlertDeliveryError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

#[derive(Debug, Clone)]
pub struct AlertDeliverySender {
    sender: mpsc::SyncSender<SyncAlert>,
    ledger: Option<Arc<Mutex<DeliveryLedger>>>,
    sink_ids: Vec<String>,
}

impl AlertDeliverySender {
    /// Persists remote work before waking the delivery worker. Desktop-only
    /// alerts remain best effort and do not enter the durable ledger.
    pub fn enqueue(&self, alert: SyncAlert) -> Result<(), AlertDeliveryError> {
        let ledger_result = if let Some(ledger) = &self.ledger {
            match ledger.lock() {
                Ok(mut ledger) => ledger.record(alert.clone(), &self.sink_ids),
                Err(_) => Err(AlertDeliveryError::InvalidState(
                    "alert delivery ledger lock is poisoned".to_string(),
                )),
            }
        } else {
            Ok(())
        };
        let queue_result = self.sender.try_send(alert).map_err(|error| {
            let reason = match error {
                mpsc::TrySendError::Full(_) => "alert delivery queue is full",
                mpsc::TrySendError::Disconnected(_) => "alert delivery worker is unavailable",
            };
            AlertDeliveryError::InvalidState(reason.to_string())
        });
        ledger_result.and(queue_result)
    }
}

#[derive(Debug)]
pub struct AlertDeliveryWorker {
    handle: thread::JoinHandle<()>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AlertDeliveryStatus {
    pub version: u32,
    pub desktop: bool,
    pub configured_sinks: Vec<AlertSinkStatus>,
    pub retained_events: usize,
    pub pending_deliveries: Vec<PendingAlertDelivery>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AlertSinkStatus {
    pub name: String,
    pub kind: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PendingAlertDelivery {
    pub job_id: String,
    pub wiki_id: String,
    pub event: String,
    pub sink: String,
    pub attempts: u32,
    pub next_attempt_unix_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_failure: Option<String>,
}

pub fn alert_delivery_status(
    config: &DaemonNotificationConfig,
    state_root: &Path,
) -> Result<AlertDeliveryStatus, AlertDeliveryError> {
    let sinks = configured_sinks(config);
    let path = state_root.join("daemon").join(LEDGER_FILE);
    let ledger = DeliveryLedger::load(path)?;
    let pending_deliveries = ledger
        .state
        .records
        .iter()
        .flat_map(|record| {
            record
                .targets
                .iter()
                .filter(|target| !target.delivered)
                .map(move |target| PendingAlertDelivery {
                    job_id: record.alert.job_id.clone(),
                    wiki_id: record.alert.wiki_id.clone(),
                    event: record.alert.event.clone(),
                    sink: target.sink_id.clone(),
                    attempts: target.attempts,
                    next_attempt_unix_ms: target.next_attempt_unix_ms,
                    last_failure: target.last_failure.clone(),
                })
        })
        .collect();
    Ok(AlertDeliveryStatus {
        version: LEDGER_VERSION,
        desktop: config.desktop,
        configured_sinks: sinks
            .iter()
            .filter_map(|sink| match sink {
                DeliverySink::Desktop => None,
                DeliverySink::Webhook(config) => Some(AlertSinkStatus {
                    name: config.name.clone(),
                    kind: match config.format {
                        DaemonWebhookFormat::Json => "webhook",
                        DaemonWebhookFormat::Ntfy => "ntfy",
                    },
                }),
                DeliverySink::Command(config) => Some(AlertSinkStatus {
                    name: config.name.clone(),
                    kind: "command",
                }),
            })
            .collect(),
        retained_events: ledger.state.records.len(),
        pending_deliveries,
    })
}

impl AlertDeliveryWorker {
    pub fn join(self) -> thread::Result<()> {
        self.handle.join()
    }
}

/// Keeps new desktop alerts working when the durable ledger cannot be opened.
/// The startup error remains visible in logs and status so durable delivery can
/// be repaired; this fallback deliberately cannot acknowledge retained work.
#[must_use]
pub fn spawn_best_effort_desktop_delivery() -> (AlertDeliverySender, AlertDeliveryWorker) {
    let (sender, receiver) = mpsc::sync_channel(32);
    let handle = thread::spawn(move || {
        while let Ok(alert) = receiver.recv() {
            if let Err(error) = deliver_desktop(&alert) {
                log_delivery_failure("desktop", delivery_io_reason(&error), 1);
            }
        }
    });
    (
        AlertDeliverySender {
            sender,
            ledger: None,
            sink_ids: Vec::new(),
        },
        AlertDeliveryWorker { handle },
    )
}

/// Creates the delivery boundary. Retained terminal jobs are reconciled into
/// the durable ledger before the worker starts, closing the crash window
/// between supervisor completion and alert enqueue.
pub fn spawn_alert_delivery(
    config: &DaemonNotificationConfig,
    state_root: &Path,
    registry: WikiRegistry,
    supervisor: &SyncSupervisor,
    stop: Arc<AtomicBool>,
) -> Result<Option<(AlertDeliverySender, AlertDeliveryWorker)>, AlertDeliveryError> {
    let sinks = configured_sinks(config);
    if sinks.is_empty() {
        return Ok(None);
    }
    let sink_ids = sinks.iter().map(DeliverySink::id).collect::<Vec<_>>();
    let mut ledger = DeliveryLedger::load(state_root.join("daemon").join(LEDGER_FILE))?;
    ledger.retire_missing_sinks(&sink_ids)?;
    reconcile_retained_jobs(&mut ledger, supervisor, &sink_ids)?;
    let ledger = Some(Arc::new(Mutex::new(ledger)));
    let (sender, receiver) = mpsc::sync_channel(32);
    let worker_ledger = ledger.clone();
    let handle = thread::spawn(move || {
        let client = reqwest::blocking::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(DELIVERY_TIMEOUT)
            .timeout(DELIVERY_TIMEOUT)
            .build();
        process_pending(
            worker_ledger.as_ref(),
            &sinks,
            &registry,
            client.as_ref().ok(),
            &stop,
        );
        loop {
            match receiver.recv_timeout(WORKER_POLL) {
                Ok(_) => {
                    process_pending(
                        worker_ledger.as_ref(),
                        &sinks,
                        &registry,
                        client.as_ref().ok(),
                        &stop,
                    );
                }
                Err(mpsc::RecvTimeoutError::Timeout) => process_pending(
                    worker_ledger.as_ref(),
                    &sinks,
                    &registry,
                    client.as_ref().ok(),
                    &stop,
                ),
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
    });
    Ok(Some((
        AlertDeliverySender {
            sender,
            ledger,
            sink_ids,
        },
        AlertDeliveryWorker { handle },
    )))
}

fn reconcile_retained_jobs(
    ledger: &mut DeliveryLedger,
    supervisor: &SyncSupervisor,
    sink_ids: &[String],
) -> Result<(), AlertDeliveryError> {
    let jobs = supervisor
        .list()
        .map_err(|error| AlertDeliveryError::InvalidState(error.to_string()))?;
    let mut latest = BTreeMap::new();
    for job in jobs {
        if let Some(wiki) = job.job.wiki_id.clone() {
            latest.insert(wiki, job.job);
        }
    }
    for job in latest.into_values() {
        if let Some(alert) = SyncAlert::from_job(&job) {
            ledger.record(alert, sink_ids)?;
        }
    }
    Ok(())
}

#[derive(Debug, Clone)]
enum DeliverySink {
    Desktop,
    Webhook(DaemonWebhookNotificationConfig),
    Command(DaemonCommandNotificationConfig),
}

impl DeliverySink {
    fn id(&self) -> String {
        match self {
            Self::Desktop => "desktop".to_string(),
            Self::Webhook(config) => format!("webhook:{}", config.name),
            Self::Command(config) => format!("command:{}", config.name),
        }
    }

    fn name(&self) -> &str {
        match self {
            Self::Desktop => "desktop",
            Self::Webhook(config) => &config.name,
            Self::Command(config) => &config.name,
        }
    }
}

fn configured_sinks(config: &DaemonNotificationConfig) -> Vec<DeliverySink> {
    config
        .desktop
        .then_some(DeliverySink::Desktop)
        .into_iter()
        .chain(
            config
                .webhooks
                .iter()
                .cloned()
                .map(DeliverySink::Webhook)
                .chain(config.commands.iter().cloned().map(DeliverySink::Command)),
        )
        .collect()
}

fn process_pending(
    ledger: Option<&Arc<Mutex<DeliveryLedger>>>,
    sinks: &[DeliverySink],
    registry: &WikiRegistry,
    client: Option<&reqwest::blocking::Client>,
    stop: &AtomicBool,
) {
    let Some(ledger) = ledger else {
        return;
    };
    let now = unix_time_ms().unwrap_or_default();
    let pending = if let Ok(ledger) = ledger.lock() {
        ledger.pending(now)
    } else {
        log_delivery_failure("ledger", "lock_poisoned", 1);
        return;
    };
    for (job_id, sink_id, alert) in pending {
        if stop.load(Ordering::Acquire) {
            return;
        }
        let Some(sink) = sinks.iter().find(|sink| sink.id() == sink_id) else {
            continue;
        };
        let result = authorize_sink(registry, &alert, sink)
            .and_then(|()| deliver_sink(sink, &alert, client));
        let update =
            ledger
                .lock()
                .map_err(|_| "lock_poisoned")
                .and_then(|mut ledger| match result {
                    Ok(()) => ledger
                        .mark_delivered(&job_id, &sink_id)
                        .map_err(|_| "ledger_write"),
                    Err(reason) => {
                        let attempts = ledger
                            .mark_failed(&job_id, &sink_id, now, reason)
                            .map_err(|_| "ledger_write")?;
                        if attempts == 1 || attempts.is_power_of_two() {
                            log_delivery_failure(sink.name(), reason, attempts);
                        }
                        Ok(())
                    }
                });
        if let Err(reason) = update {
            log_delivery_failure("ledger", reason, 1);
        }
    }
}

fn authorize_sink(
    registry: &WikiRegistry,
    alert: &SyncAlert,
    sink: &DeliverySink,
) -> Result<(), &'static str> {
    let config = registry.load().map_err(|_| "registry_unavailable")?;
    let registration = config
        .vaults
        .iter()
        .find(|registration| registration.id.as_str() == alert.wiki_id)
        .ok_or("registration_unavailable")?;
    let paths = VaultPaths::new(&registration.path);
    let selection = resolve_permission_profile(&paths, registration.permissions_profile.as_deref())
        .map_err(|_| "permission_profile_invalid")?;
    let guard = ProfilePermissionGuard::new(&paths, selection);
    match sink {
        DeliverySink::Desktop => Ok(()),
        DeliverySink::Webhook(config) => guard
            .check_network(&config.url)
            .map_err(|_| "network_permission_denied"),
        DeliverySink::Command(_) => guard
            .check_execute()
            .map_err(|_| "execute_permission_denied"),
    }
}

fn deliver_sink(
    sink: &DeliverySink,
    alert: &SyncAlert,
    client: Option<&reqwest::blocking::Client>,
) -> Result<(), &'static str> {
    match sink {
        DeliverySink::Desktop => deliver_desktop(alert).map_err(|error| delivery_io_reason(&error)),
        DeliverySink::Webhook(config) => {
            deliver_webhook(config, alert, client.ok_or("http_client_unavailable")?)
        }
        DeliverySink::Command(config) => deliver_command(config, alert),
    }
}

fn deliver_webhook(
    config: &DaemonWebhookNotificationConfig,
    alert: &SyncAlert,
    client: &reqwest::blocking::Client,
) -> Result<(), &'static str> {
    let mut request = client
        .post(&config.url)
        .header("User-Agent", "vulcan-daemon/1")
        .header(
            "Idempotency-Key",
            format!("vulcan-sync-alert-{}", alert.job_id),
        );
    if let Some(name) = &config.token_env {
        let token = std::env::var(name).map_err(|_| "credential_unavailable")?;
        request = request.bearer_auth(token);
    }
    request = match config.format {
        DaemonWebhookFormat::Json => request.json(alert),
        DaemonWebhookFormat::Ntfy => request
            .header("Title", alert.desktop_title())
            .header("X-Sequence-ID", format!("vulcan-{}", alert.job_id))
            .header(
                "Priority",
                if alert.severity == crate::alerts::AlertSeverity::Error {
                    "high"
                } else {
                    "default"
                },
            )
            .header("Tags", "warning,vulcan")
            .body(alert.desktop_body()),
    };
    let response = request.send().map_err(|_| "transport_error")?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err("http_status")
    }
}

fn deliver_command(
    config: &DaemonCommandNotificationConfig,
    alert: &SyncAlert,
) -> Result<(), &'static str> {
    let payload = serde_json::to_vec(alert).map_err(|_| "serialization_error")?;
    let mut child = Command::new(&config.program)
        .args(&config.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "command_spawn")?;
    child
        .stdin
        .take()
        .ok_or("command_stdin")?
        .write_all(&payload)
        .map_err(|_| "command_stdin")?;
    wait_for_child(&mut child).map_err(|error| delivery_io_reason(&error))
}

fn wait_for_child(child: &mut std::process::Child) -> std::io::Result<()> {
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return if status.success() {
                Ok(())
            } else {
                Err(std::io::Error::other("notification adapter failed"))
            };
        }
        if started.elapsed() >= DELIVERY_TIMEOUT {
            child.kill()?;
            let _ = child.wait();
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "notification adapter timed out",
            ));
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn delivery_io_reason(error: &std::io::Error) -> &'static str {
    match error.kind() {
        std::io::ErrorKind::NotFound => "helper_not_found",
        std::io::ErrorKind::PermissionDenied => "helper_permission_denied",
        std::io::ErrorKind::TimedOut => "helper_timeout",
        _ => "helper_error",
    }
}

fn log_delivery_failure(sink: &str, reason: &str, attempts: u32) {
    eprintln!(
        "level=warning event=notification_delivery_failed sink={sink} reason={reason} attempts={attempts}"
    );
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DeliveryLedgerState {
    version: u32,
    records: Vec<DeliveryRecord>,
}

impl Default for DeliveryLedgerState {
    fn default() -> Self {
        Self {
            version: LEDGER_VERSION,
            records: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct DeliveryRecord {
    alert: SyncAlert,
    targets: Vec<DeliveryTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct DeliveryTarget {
    sink_id: String,
    delivered: bool,
    attempts: u32,
    next_attempt_unix_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_failure: Option<String>,
}

#[derive(Debug)]
struct DeliveryLedger {
    path: PathBuf,
    state: DeliveryLedgerState,
}

impl DeliveryLedger {
    fn load(path: PathBuf) -> Result<Self, AlertDeliveryError> {
        let state = match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_file() {
                    return Err(AlertDeliveryError::InvalidState(format!(
                        "alert delivery ledger `{}` must be a regular file",
                        path.display()
                    )));
                }
                if metadata.len() > MAX_LEDGER_BYTES {
                    return Err(AlertDeliveryError::InvalidState(format!(
                        "alert delivery ledger `{}` exceeds the byte limit",
                        path.display()
                    )));
                }
                let bytes = fs::read(&path)?;
                let state: DeliveryLedgerState = serde_json::from_slice(&bytes)?;
                if state.version != LEDGER_VERSION || state.records.len() > MAX_RECORDS {
                    return Err(AlertDeliveryError::InvalidState(format!(
                        "alert delivery ledger `{}` has an unsupported version or record count",
                        path.display()
                    )));
                }
                validate_ledger_state(&state, &path)?;
                state
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                DeliveryLedgerState::default()
            }
            Err(error) => return Err(error.into()),
        };
        Ok(Self { path, state })
    }

    fn record(&mut self, alert: SyncAlert, sink_ids: &[String]) -> Result<(), AlertDeliveryError> {
        if let Some(index) = self
            .state
            .records
            .iter()
            .position(|record| record.alert.job_id == alert.job_id)
        {
            let previous = self.state.clone();
            let record = &mut self.state.records[index];
            let existing = record
                .targets
                .iter()
                .map(|target| target.sink_id.as_str())
                .collect::<BTreeSet<_>>();
            let additions = sink_ids
                .iter()
                .filter(|sink_id| !existing.contains(sink_id.as_str()))
                .cloned()
                .collect::<Vec<_>>();
            if additions.is_empty() {
                return Ok(());
            }
            record
                .targets
                .extend(additions.into_iter().map(|sink_id| DeliveryTarget {
                    sink_id,
                    delivered: false,
                    attempts: 0,
                    next_attempt_unix_ms: 0,
                    last_failure: None,
                }));
            return self.save_or_restore(previous);
        }
        let previous = self.state.clone();
        trim_completed_records(&mut self.state.records);
        if self.state.records.len() >= MAX_RECORDS {
            self.state = previous;
            return Err(AlertDeliveryError::InvalidState(
                "alert delivery ledger is full of pending records".to_string(),
            ));
        }
        self.state.records.push(DeliveryRecord {
            alert,
            targets: sink_ids
                .iter()
                .map(|sink_id| DeliveryTarget {
                    sink_id: sink_id.clone(),
                    delivered: false,
                    attempts: 0,
                    next_attempt_unix_ms: 0,
                    last_failure: None,
                })
                .collect(),
        });
        self.save_or_restore(previous)
    }

    fn pending(&self, now: u64) -> Vec<(String, String, SyncAlert)> {
        self.state
            .records
            .iter()
            .flat_map(|record| {
                record
                    .targets
                    .iter()
                    .filter(move |target| !target.delivered && target.next_attempt_unix_ms <= now)
                    .map(move |target| {
                        (
                            record.alert.job_id.clone(),
                            target.sink_id.clone(),
                            record.alert.clone(),
                        )
                    })
            })
            .collect()
    }

    fn mark_delivered(&mut self, job_id: &str, sink_id: &str) -> Result<(), AlertDeliveryError> {
        let previous = self.state.clone();
        let target = self.target_mut(job_id, sink_id)?;
        target.delivered = true;
        self.save_or_restore(previous)
    }

    fn mark_failed(
        &mut self,
        job_id: &str,
        sink_id: &str,
        now: u64,
        reason: &str,
    ) -> Result<u32, AlertDeliveryError> {
        let previous = self.state.clone();
        let target = self.target_mut(job_id, sink_id)?;
        target.attempts = target.attempts.saturating_add(1);
        target.next_attempt_unix_ms = now.saturating_add(retry_delay_ms(target.attempts));
        target.last_failure = Some(reason.to_string());
        let attempts = target.attempts;
        self.save_or_restore(previous)?;
        Ok(attempts)
    }

    fn target_mut(
        &mut self,
        job_id: &str,
        sink_id: &str,
    ) -> Result<&mut DeliveryTarget, AlertDeliveryError> {
        self.state
            .records
            .iter_mut()
            .find(|record| record.alert.job_id == job_id)
            .and_then(|record| {
                record
                    .targets
                    .iter_mut()
                    .find(|target| target.sink_id == sink_id)
            })
            .ok_or_else(|| {
                AlertDeliveryError::InvalidState(
                    "alert delivery target disappeared during dispatch".to_string(),
                )
            })
    }

    fn retire_missing_sinks(&mut self, sink_ids: &[String]) -> Result<(), AlertDeliveryError> {
        let previous = self.state.clone();
        let configured = sink_ids.iter().collect::<BTreeSet<_>>();
        let mut changed = false;
        for record in &mut self.state.records {
            let before = record.targets.len();
            record
                .targets
                .retain(|target| configured.contains(&target.sink_id));
            changed |= record.targets.len() != before;
        }
        if changed {
            self.save_or_restore(previous)?;
        }
        Ok(())
    }

    fn save(&self) -> Result<(), AlertDeliveryError> {
        let parent = self.path.parent().ok_or_else(|| {
            AlertDeliveryError::InvalidState("alert delivery ledger path has no parent".to_string())
        })?;
        fs::create_dir_all(parent)?;
        if fs::symlink_metadata(&self.path).is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            return Err(AlertDeliveryError::InvalidState(format!(
                "alert delivery ledger `{}` must not be a symlink",
                self.path.display()
            )));
        }
        let bytes = serde_json::to_vec_pretty(&self.state)?;
        if bytes.len() as u64 > MAX_LEDGER_BYTES {
            return Err(AlertDeliveryError::InvalidState(
                "alert delivery ledger exceeds the byte limit".to_string(),
            ));
        }
        let mut temporary = NamedTempFile::new_in(parent)?;
        temporary.write_all(&bytes)?;
        temporary.as_file().sync_all()?;
        temporary.persist(&self.path).map_err(|error| error.error)?;
        #[cfg(unix)]
        std::fs::File::open(parent)?.sync_all()?;
        Ok(())
    }

    fn save_or_restore(&mut self, previous: DeliveryLedgerState) -> Result<(), AlertDeliveryError> {
        if let Err(error) = self.save() {
            self.state = previous;
            Err(error)
        } else {
            Ok(())
        }
    }
}

fn validate_ledger_state(
    state: &DeliveryLedgerState,
    path: &Path,
) -> Result<(), AlertDeliveryError> {
    let mut jobs = BTreeSet::new();
    for record in &state.records {
        let alert = &record.alert;
        let valid_job = !alert.job_id.is_empty()
            && alert.job_id.len() <= 128
            && alert
                .job_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_');
        let valid_event = matches!(
            (alert.event.as_str(), alert.state, alert.severity),
            (
                "sync_failed",
                vulcan_sync::SyncJobState::Failed,
                crate::alerts::AlertSeverity::Error
            ) | (
                "sync_conflicted",
                vulcan_sync::SyncJobState::Conflicted,
                crate::alerts::AlertSeverity::Warning
            ) | (
                "sync_paused",
                vulcan_sync::SyncJobState::Paused,
                crate::alerts::AlertSeverity::Warning
            )
        );
        let valid_wiki =
            alert.wiki_id == "<unregistered>" || WikiId::parse(alert.wiki_id.clone()).is_ok();
        if alert.version != 1
            || !valid_job
            || !valid_event
            || !valid_wiki
            || !jobs.insert(&alert.job_id)
            || record.targets.len() > MAX_TARGETS_PER_RECORD
        {
            return Err(invalid_ledger(path));
        }
        let mut targets = BTreeSet::new();
        for target in &record.targets {
            if target.sink_id.is_empty()
                || target.sink_id.len() > 96
                || !target
                    .sink_id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b':' | b'-' | b'_'))
                || target.last_failure.as_ref().is_some_and(|reason| {
                    reason.is_empty()
                        || reason.len() > 64
                        || !reason
                            .bytes()
                            .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
                })
                || !targets.insert(&target.sink_id)
            {
                return Err(invalid_ledger(path));
            }
        }
    }
    Ok(())
}

fn invalid_ledger(path: &Path) -> AlertDeliveryError {
    AlertDeliveryError::InvalidState(format!(
        "alert delivery ledger `{}` contains invalid or duplicate records",
        path.display()
    ))
}

fn trim_completed_records(records: &mut Vec<DeliveryRecord>) {
    while records.len() >= MAX_RECORDS {
        let Some(index) = records
            .iter()
            .position(|record| record.targets.iter().all(|target| target.delivered))
        else {
            break;
        };
        records.remove(index);
    }
}

fn retry_delay_ms(attempts: u32) -> u64 {
    let exponent = attempts.saturating_sub(1).min(7);
    RETRY_BASE_MS
        .saturating_mul(1_u64 << exponent)
        .min(RETRY_MAX_MS)
}

fn unix_time_ms() -> Result<u64, AlertDeliveryError> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| AlertDeliveryError::InvalidState(error.to_string()))?;
    u64::try_from(duration.as_millis()).map_err(|_| {
        AlertDeliveryError::InvalidState("system time exceeds supported range".to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alerts::AlertSeverity;
    use tempfile::tempdir;
    use vulcan_sync::{SyncErrorCategory, SyncJobState};

    fn alert(job_id: &str) -> SyncAlert {
        SyncAlert {
            version: 1,
            event: "sync_failed".to_string(),
            severity: AlertSeverity::Error,
            job_id: job_id.to_string(),
            wiki_id: "alpha".to_string(),
            state: SyncJobState::Failed,
            category: Some(SyncErrorCategory::Network),
            retryable: true,
        }
    }

    fn capture_webhook_request(format: DaemonWebhookFormat) -> String {
        use std::io::Read;
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let address = listener.local_addr().expect("address");
        let receiver = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("read timeout");
            let mut bytes = Vec::new();
            let mut buffer = [0_u8; 4096];
            loop {
                let read = stream.read(&mut buffer).expect("request");
                bytes.extend_from_slice(&buffer[..read]);
                let text = String::from_utf8_lossy(&bytes);
                let Some(header_end) = text.find("\r\n\r\n") else {
                    continue;
                };
                let content_length = text[..header_end]
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length: ")
                            .and_then(|value| value.parse::<usize>().ok())
                    })
                    .expect("content length");
                if bytes.len() >= header_end + 4 + content_length {
                    break;
                }
            }
            stream
                .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n")
                .expect("response");
            String::from_utf8(bytes).expect("request UTF-8")
        });
        let client = reqwest::blocking::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(5))
            .build()
            .expect("client");
        deliver_webhook(
            &DaemonWebhookNotificationConfig {
                name: "test".to_string(),
                url: format!("http://{address}/alert"),
                format,
                token_env: None,
            },
            &alert("job-1"),
            &client,
        )
        .expect("deliver webhook");
        receiver.join().expect("receiver")
    }

    #[test]
    fn durable_ledger_retries_with_backoff_and_survives_reload() {
        let temporary = tempdir().expect("temporary directory");
        let path = temporary.path().join("alerts.json");
        let sinks = vec!["webhook:primary".to_string()];
        let mut ledger = DeliveryLedger::load(path.clone()).expect("ledger");
        ledger.record(alert("job-1"), &sinks).expect("record");
        assert_eq!(ledger.pending(100).len(), 1);
        assert_eq!(
            ledger
                .mark_failed("job-1", "webhook:primary", 100, "transport_error")
                .expect("mark failed"),
            1
        );
        assert!(ledger.pending(30_099).is_empty());
        assert_eq!(ledger.pending(30_100).len(), 1);
        assert_eq!(
            ledger.state.records[0].targets[0].last_failure.as_deref(),
            Some("transport_error")
        );

        let mut reloaded = DeliveryLedger::load(path).expect("reload");
        assert_eq!(reloaded.pending(30_100).len(), 1);
        reloaded
            .mark_delivered("job-1", "webhook:primary")
            .expect("delivered");
        assert!(reloaded.pending(u64::MAX).is_empty());
    }

    #[test]
    fn ledger_deduplicates_jobs_and_retires_removed_sinks() {
        let temporary = tempdir().expect("temporary directory");
        let mut ledger =
            DeliveryLedger::load(temporary.path().join("alerts.json")).expect("ledger");
        let sinks = vec!["webhook:first".to_string(), "command:second".to_string()];
        ledger.record(alert("job-1"), &sinks).expect("record");
        ledger.record(alert("job-1"), &sinks).expect("deduplicate");
        assert_eq!(ledger.state.records.len(), 1);
        ledger
            .record(
                alert("job-1"),
                &[
                    "webhook:first".to_string(),
                    "command:second".to_string(),
                    "desktop".to_string(),
                ],
            )
            .expect("add newly enabled desktop target");
        assert_eq!(ledger.state.records[0].targets.len(), 3);
        assert_eq!(ledger.pending(u64::MAX).len(), 3);
        ledger
            .retire_missing_sinks(&["webhook:first".to_string()])
            .expect("retire");
        assert_eq!(ledger.state.records[0].targets.len(), 1);
        assert_eq!(ledger.state.records[0].targets[0].sink_id, "webhook:first");

        let mut replacements = vec!["desktop".to_string()];
        replacements.extend((0..16).map(|index| format!("webhook:replacement-{index}")));
        ledger
            .retire_missing_sinks(&replacements)
            .expect("remove obsolete target identities");
        ledger
            .record(alert("job-1"), &replacements)
            .expect("add replacement targets within the active bound");
        assert_eq!(ledger.state.records[0].targets.len(), 17);
        DeliveryLedger::load(ledger.path.clone()).expect("rotated ledger remains valid");
    }

    #[test]
    fn malformed_or_forged_ledger_state_is_rejected() {
        let temporary = tempdir().expect("temporary directory");
        let path = temporary.path().join("alerts.json");
        fs::write(&path, b"not json").expect("malformed ledger");
        assert!(matches!(
            DeliveryLedger::load(path.clone()),
            Err(AlertDeliveryError::Json(_))
        ));

        let mut state = DeliveryLedgerState::default();
        state.records.push(DeliveryRecord {
            alert: SyncAlert {
                event: "arbitrary_event".to_string(),
                ..alert("job-1")
            },
            targets: Vec::new(),
        });
        fs::write(&path, serde_json::to_vec(&state).expect("serialize")).expect("forged ledger");
        assert!(matches!(
            DeliveryLedger::load(path),
            Err(AlertDeliveryError::InvalidState(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn failed_ledger_write_rolls_back_in_memory_state() {
        use std::os::unix::fs::symlink;

        let temporary = tempdir().expect("temporary directory");
        let path = temporary.path().join("alerts.json");
        let target = temporary.path().join("target.json");
        fs::write(&target, "untouched").expect("target");
        let mut ledger = DeliveryLedger::load(path.clone()).expect("ledger");
        symlink(&target, &path).expect("symlink");
        assert!(ledger
            .record(alert("job-1"), &["webhook:primary".to_string()])
            .is_err());
        assert!(ledger.state.records.is_empty());
        assert_eq!(fs::read_to_string(target).expect("target"), "untouched");
    }

    #[cfg(unix)]
    #[test]
    fn ledger_failure_does_not_suppress_local_delivery_queue() {
        use std::os::unix::fs::symlink;

        let temporary = tempdir().expect("temporary directory");
        let path = temporary.path().join("alerts.json");
        let target = temporary.path().join("target.json");
        fs::write(&target, "untouched").expect("target");
        let ledger = DeliveryLedger::load(path.clone()).expect("ledger");
        symlink(&target, path).expect("symlink");
        let (sender, receiver) = mpsc::sync_channel(1);
        let delivery = AlertDeliverySender {
            sender,
            ledger: Some(Arc::new(Mutex::new(ledger))),
            sink_ids: vec!["webhook:primary".to_string()],
        };
        assert!(delivery.enqueue(alert("job-1")).is_err());
        assert_eq!(receiver.try_recv().expect("queued alert"), alert("job-1"));
    }

    #[test]
    fn command_delivery_writes_only_the_structured_event_to_stdin() {
        let serialized = serde_json::to_string(&alert("job-1")).expect("serialize");
        assert!(serialized.contains("\"job_id\":\"job-1\""));
        assert!(!serialized.contains("message"));
        assert!(!serialized.contains("url"));
    }

    #[test]
    fn retry_backoff_is_bounded() {
        assert_eq!(retry_delay_ms(1), 30_000);
        assert_eq!(retry_delay_ms(2), 60_000);
        assert_eq!(retry_delay_ms(100), RETRY_MAX_MS);
    }

    #[test]
    fn startup_reconciliation_closes_the_post_completion_enqueue_window() {
        use vulcan_sync::{SyncError, SyncJobTrigger};

        let temporary = tempdir().expect("temporary directory");
        let supervisor =
            SyncSupervisor::at(temporary.path().join("jobs.json")).expect("supervisor");
        let queued = supervisor
            .enqueue("alpha", temporary.path(), SyncJobTrigger::Poll)
            .expect("enqueue");
        supervisor.claim_next().expect("claim").expect("job");
        supervisor
            .complete(
                &queued.job.job.id,
                SyncJobState::Failed,
                None,
                Some(SyncError::new(SyncErrorCategory::Network, "offline", true)),
            )
            .expect("complete");

        let mut ledger =
            DeliveryLedger::load(temporary.path().join("alerts.json")).expect("ledger");
        reconcile_retained_jobs(&mut ledger, &supervisor, &["webhook:primary".to_string()])
            .expect("reconcile");
        assert_eq!(ledger.pending(u64::MAX).len(), 1);
        assert_eq!(ledger.state.records[0].alert.job_id, queued.job.job.id);
    }

    #[test]
    fn delivery_reapplies_each_wikis_network_and_execute_permissions() {
        use crate::registry::{AddWikiRequest, WikiId};

        let temporary = tempdir().expect("temporary directory");
        let vault = temporary.path().join("vault");
        fs::create_dir_all(vault.join(".vulcan")).expect("vault config directory");
        fs::write(
            vault.join(".vulcan/config.toml"),
            "[permissions.profiles.locked]\nnetwork = \"deny\"\nexecute = \"deny\"\n",
        )
        .expect("permissions");
        let registry = WikiRegistry::at(temporary.path().join("daemon.toml"));
        registry
            .add(
                &AddWikiRequest {
                    id: WikiId::parse("alpha").expect("wiki ID"),
                    path: vault,
                    groups: Vec::new(),
                    git_dir: None,
                    permissions_profile: Some("locked".to_string()),
                    sync_backend: Some("git".to_string()),
                    platform_profile: None,
                },
                false,
            )
            .expect("registration");
        let webhook = DeliverySink::Webhook(DaemonWebhookNotificationConfig {
            name: "remote".to_string(),
            url: "https://example.test/alerts".to_string(),
            format: DaemonWebhookFormat::Json,
            token_env: None,
        });
        let command = DeliverySink::Command(DaemonCommandNotificationConfig {
            name: "local".to_string(),
            program: PathBuf::from("/usr/bin/adapter"),
            args: Vec::new(),
        });
        assert_eq!(
            authorize_sink(&registry, &alert("job-1"), &webhook),
            Err("network_permission_denied")
        );
        assert_eq!(
            authorize_sink(&registry, &alert("job-1"), &command),
            Err("execute_permission_denied")
        );
    }

    #[test]
    fn status_exposes_health_without_endpoint_or_program_details() {
        let temporary = tempdir().expect("temporary directory");
        let config = DaemonNotificationConfig {
            desktop: true,
            webhooks: vec![DaemonWebhookNotificationConfig {
                name: "phone".to_string(),
                url: "https://secret-endpoint.example.test/topic".to_string(),
                format: DaemonWebhookFormat::Ntfy,
                token_env: Some("SECRET_TOKEN".to_string()),
            }],
            commands: Vec::new(),
        };
        let mut ledger = DeliveryLedger::load(temporary.path().join("daemon").join(LEDGER_FILE))
            .expect("ledger");
        ledger
            .record(alert("job-1"), &["webhook:phone".to_string()])
            .expect("record");
        let status = alert_delivery_status(&config, temporary.path()).expect("status");
        let json = serde_json::to_string(&status).expect("serialize");
        assert!(json.contains("phone"));
        assert!(json.contains("job-1"));
        assert!(!json.contains("secret-endpoint"));
        assert!(!json.contains("SECRET_TOKEN"));
    }

    #[cfg(unix)]
    #[test]
    fn command_adapter_receives_the_json_event_on_stdin() {
        use std::os::unix::fs::PermissionsExt;

        let temporary = tempdir().expect("temporary directory");
        let adapter = temporary.path().join("adapter");
        let output = temporary.path().join("received.json");
        fs::write(&adapter, "#!/bin/sh\ncat > \"$1\"\n").expect("adapter");
        fs::set_permissions(&adapter, fs::Permissions::from_mode(0o700)).expect("permissions");
        let config = DaemonCommandNotificationConfig {
            name: "capture".to_string(),
            program: adapter,
            args: vec![output.to_string_lossy().into_owned()],
        };
        deliver_command(&config, &alert("job-1")).expect("deliver command");
        let received: SyncAlert =
            serde_json::from_slice(&fs::read(output).expect("output")).expect("event JSON");
        assert_eq!(received, alert("job-1"));
    }

    #[test]
    fn json_webhook_receives_idempotent_secret_minimal_event() {
        let request = capture_webhook_request(DaemonWebhookFormat::Json);
        assert!(request
            .to_ascii_lowercase()
            .contains("idempotency-key: vulcan-sync-alert-job-1"));
        assert!(request.contains("\"job_id\":\"job-1\""));
        assert!(!request.contains("message"));
    }

    #[test]
    fn ntfy_webhook_uses_native_title_priority_tags_and_bounded_body() {
        let request = capture_webhook_request(DaemonWebhookFormat::Ntfy);
        let lower = request.to_ascii_lowercase();
        assert!(lower.contains("title: vulcan sync failed"));
        assert!(lower.contains("priority: high"));
        assert!(lower.contains("tags: warning,vulcan"));
        assert!(lower.contains("x-sequence-id: vulcan-job-1"));
        assert!(request.contains("Wiki `alpha` needs attention"));
        assert!(!request.contains("\"job_id\""));
    }
}
