//! Repository-advertised realtime sync wake-up listeners.

use crate::registry::{RegistryError, WikiRegistration, WikiRegistry};
use crate::supervisor::{SupervisorError, SyncSupervisor};
use reqwest::redirect::Policy;
use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use vulcan_core::{
    resolve_permission_profile, PermissionGuard, ProfilePermissionGuard, VaultPaths,
};
use vulcan_sync::{
    refresh_notification_advertisement, DiscoveredNotificationAdvertisement, GitCliEngine,
    GitEngine, GitRemote, NotificationEndpoint, SyncJobTrigger,
};

const STOP_POLL: Duration = Duration::from_millis(50);
const GIT_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotificationRuntimeOptions {
    pub registry_refresh_ms: u64,
    pub advertisement_refresh_ms: u64,
    pub initial_backoff_ms: u64,
    pub maximum_backoff_ms: u64,
    pub connect_timeout_ms: u64,
    /// Enables operational stderr lines for advertisement discovery and
    /// wake-up enqueueing. Off by default; set from `--verbose`.
    pub verbose: bool,
}

impl Default for NotificationRuntimeOptions {
    fn default() -> Self {
        Self {
            registry_refresh_ms: 1_000,
            advertisement_refresh_ms: 5 * 60 * 1_000,
            initial_backoff_ms: 1_000,
            maximum_backoff_ms: 60_000,
            connect_timeout_ms: 15_000,
            verbose: false,
        }
    }
}

#[derive(Debug)]
pub enum NotificationRuntimeError {
    InvalidOptions(String),
    Registry(RegistryError),
    Supervisor(SupervisorError),
    HttpClient(String),
    Worker(String),
}

impl Display for NotificationRuntimeError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidOptions(detail) | Self::HttpClient(detail) | Self::Worker(detail) => {
                formatter.write_str(detail)
            }
            Self::Registry(error) => Display::fmt(error, formatter),
            Self::Supervisor(error) => Display::fmt(error, formatter),
        }
    }
}

impl Error for NotificationRuntimeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Registry(error) => Some(error),
            Self::Supervisor(error) => Some(error),
            Self::InvalidOptions(_) | Self::HttpClient(_) | Self::Worker(_) => None,
        }
    }
}

impl From<RegistryError> for NotificationRuntimeError {
    fn from(error: RegistryError) -> Self {
        Self::Registry(error)
    }
}

impl From<SupervisorError> for NotificationRuntimeError {
    fn from(error: SupervisorError) -> Self {
        Self::Supervisor(error)
    }
}

struct ListenerTask {
    registration: WikiRegistration,
    stop: Arc<AtomicBool>,
    handle: JoinHandle<Result<(), NotificationRuntimeError>>,
}

/// Reconciles one interruptible long-poll listener per active Git wiki with the
/// device-local registry. Notifications enqueue the existing finite sync job;
/// this runtime never performs synchronization itself.
///
/// All listeners share one HTTP client and connection pool, so subscriptions
/// against the same relay origin multiplex over HTTP/2 when the relay
/// negotiates it and otherwise reuse HTTP/1.1 keep-alive connections.
pub async fn run_notification_runtime_until(
    registry: WikiRegistry,
    supervisor: Arc<SyncSupervisor>,
    options: NotificationRuntimeOptions,
    should_stop: Arc<AtomicBool>,
) -> Result<(), NotificationRuntimeError> {
    validate_options(&options)?;
    let client = build_notification_client(Duration::from_millis(options.connect_timeout_ms))?;
    let registry_refresh = Duration::from_millis(options.registry_refresh_ms);
    let mut listeners = BTreeMap::<String, ListenerTask>::new();

    loop {
        if should_stop.load(Ordering::Acquire) {
            stop_all_listeners(&mut listeners).await;
            return Ok(());
        }
        if let Err(error) =
            reconcile_listeners(&registry, &supervisor, options, &client, &mut listeners).await
        {
            stop_all_listeners(&mut listeners).await;
            return Err(error);
        }
        if wait_for_stop(&should_stop, registry_refresh).await {
            stop_all_listeners(&mut listeners).await;
            return Ok(());
        }
    }
}

fn build_notification_client(
    connect_timeout: Duration,
) -> Result<reqwest::Client, NotificationRuntimeError> {
    reqwest::Client::builder()
        .redirect(Policy::none())
        .connect_timeout(connect_timeout)
        .build()
        .map_err(|_| {
            NotificationRuntimeError::HttpClient(
                "failed to initialize the notification HTTP client".to_string(),
            )
        })
}

fn validate_options(options: &NotificationRuntimeOptions) -> Result<(), NotificationRuntimeError> {
    if options.registry_refresh_ms == 0
        || options.advertisement_refresh_ms == 0
        || options.initial_backoff_ms == 0
        || options.maximum_backoff_ms < options.initial_backoff_ms
        || options.connect_timeout_ms == 0
    {
        return Err(NotificationRuntimeError::InvalidOptions(
            "notification runtime intervals must be nonzero and maximum backoff must not be less than initial backoff"
                .to_string(),
        ));
    }
    Ok(())
}

fn desired_listeners(registrations: Vec<WikiRegistration>) -> BTreeMap<String, WikiRegistration> {
    registrations
        .into_iter()
        .filter(|registration| {
            !registration.sync_paused
                && registration
                    .sync_backend
                    .as_deref()
                    .is_none_or(|backend| backend == "git")
        })
        .map(|registration| (registration.id.as_str().to_string(), registration))
        .collect()
}

async fn reconcile_listeners(
    registry: &WikiRegistry,
    supervisor: &Arc<SyncSupervisor>,
    options: NotificationRuntimeOptions,
    client: &reqwest::Client,
    listeners: &mut BTreeMap<String, ListenerTask>,
) -> Result<(), NotificationRuntimeError> {
    let desired = desired_listeners(registry.load()?.vaults);
    let completed = listeners
        .iter()
        .filter(|(_, task)| task.handle.is_finished())
        .map(|(id, _)| id.clone())
        .collect::<Vec<_>>();
    for id in completed {
        if let Some(task) = listeners.remove(&id) {
            task.handle.await.map_err(|error| {
                NotificationRuntimeError::Worker(format!(
                    "notification listener for wiki `{id}` panicked: {error}"
                ))
            })??;
        }
    }

    let stale = listeners
        .iter()
        .filter(|(id, task)| {
            desired.get(*id).is_none_or(|registration| {
                listener_registration_changed(&task.registration, registration)
            })
        })
        .map(|(id, _)| id.clone())
        .collect::<Vec<_>>();
    let mut stale_tasks = Vec::with_capacity(stale.len());
    for id in stale {
        if let Some(task) = listeners.remove(&id) {
            stale_tasks.push(task);
        }
    }
    stop_listeners(stale_tasks).await;

    for (id, registration) in desired {
        listeners.entry(id).or_insert_with(|| {
            spawn_listener(
                registration,
                Arc::clone(supervisor),
                options,
                client.clone(),
                options.verbose,
            )
        });
    }
    Ok(())
}

fn listener_registration_changed(current: &WikiRegistration, desired: &WikiRegistration) -> bool {
    current.id != desired.id
        || current.registration_id != desired.registration_id
        || current.path != desired.path
        || current.git_dir != desired.git_dir
        || current.permissions_profile != desired.permissions_profile
        || current.sync_backend != desired.sync_backend
        || current.sync_paused != desired.sync_paused
}

fn spawn_listener(
    registration: WikiRegistration,
    supervisor: Arc<SyncSupervisor>,
    options: NotificationRuntimeOptions,
    client: reqwest::Client,
    verbose: bool,
) -> ListenerTask {
    if verbose {
        eprintln!("{}", listener_line(registration.id.as_str()));
    }
    let stop = Arc::new(AtomicBool::new(false));
    let task_stop = Arc::clone(&stop);
    let task_registration = registration.clone();
    let handle = tokio::spawn(async move {
        run_listener(task_registration, supervisor, options, client, task_stop).await
    });
    ListenerTask {
        registration,
        stop,
        handle,
    }
}

async fn stop_all_listeners(listeners: &mut BTreeMap<String, ListenerTask>) {
    stop_listeners(std::mem::take(listeners).into_values().collect()).await;
}

async fn stop_listeners(tasks: Vec<ListenerTask>) {
    for task in &tasks {
        task.stop.store(true, Ordering::Release);
    }
    for task in tasks {
        let _ = task.handle.await;
    }
}

async fn run_listener(
    registration: WikiRegistration,
    supervisor: Arc<SyncSupervisor>,
    options: NotificationRuntimeOptions,
    client: reqwest::Client,
    stop: Arc<AtomicBool>,
) -> Result<(), NotificationRuntimeError> {
    let advertisement_refresh = Duration::from_millis(options.advertisement_refresh_ms);
    let mut endpoint = None;
    let mut refresh_at = Instant::now();
    let mut backoff = Backoff::new(
        Duration::from_millis(options.initial_backoff_ms),
        Duration::from_millis(options.maximum_backoff_ms),
    );
    let mut last_diagnostic = None;

    loop {
        if stop.load(Ordering::Acquire) {
            return Ok(());
        }
        if endpoint.is_none() || Instant::now() >= refresh_at {
            match refresh_for_registration_interruptible(&registration, &stop).await {
                RefreshResult::Stopped => return Ok(()),
                RefreshResult::Advertisement(discovered) => {
                    if options.verbose {
                        eprintln!(
                            "{}",
                            advertisement_line(
                                registration.id.as_str(),
                                &discovered.advertisement.endpoint
                            )
                        );
                    }
                    endpoint = Some(discovered.advertisement.endpoint);
                    refresh_at = Instant::now() + advertisement_refresh;
                    backoff.reset();
                    last_diagnostic = None;
                }
                RefreshResult::Missing => {
                    endpoint = None;
                    last_diagnostic = None;
                    if wait_for_stop(&stop, advertisement_refresh).await {
                        return Ok(());
                    }
                    continue;
                }
                RefreshResult::Unavailable(detail) => {
                    report_diagnostic_once(registration.id.as_str(), &detail, &mut last_diagnostic);
                    if wait_for_stop(&stop, backoff.next_delay()).await {
                        return Ok(());
                    }
                    continue;
                }
            }
        }

        let current = endpoint
            .as_ref()
            .expect("a refreshed listener always has an endpoint");
        tokio::select! {
            () = wait_until_stopped(&stop) => return Ok(()),
            () = tokio::time::sleep_until(refresh_at) => {}
            result = poll_endpoint(&client, current) => {
                match result {
                    PollResult::Wake => {
                        if options.verbose {
                            eprintln!(
                                "{}",
                                wake_line(registration.id.as_str(), current)
                            );
                        }
                        supervisor.enqueue(
                            registration.id.as_str(),
                            &registration.path,
                            SyncJobTrigger::RemoteNotification,
                        )?;
                        endpoint = None;
                        refresh_at = Instant::now();
                        backoff.reset();
                        last_diagnostic = None;
                    }
                    PollResult::Unavailable(detail) => {
                        report_diagnostic_once(
                            registration.id.as_str(),
                            &detail,
                            &mut last_diagnostic,
                        );
                        let delay = backoff.next_delay().min(
                            refresh_at.saturating_duration_since(Instant::now()),
                        );
                        if wait_for_stop(&stop, delay).await {
                            return Ok(());
                        }
                    }
                }
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum PollResult {
    Wake,
    Unavailable(String),
}

async fn poll_endpoint(client: &reqwest::Client, endpoint: &NotificationEndpoint) -> PollResult {
    match client.get(endpoint.expose_url().clone()).send().await {
        Ok(response) if response.status().is_success() => {
            drop(response);
            PollResult::Wake
        }
        Ok(response) => {
            let detail = format!(
                "notification endpoint at {} ({}) returned HTTP {}",
                endpoint.origin(),
                endpoint.fingerprint(),
                response.status().as_u16(),
            );
            drop(response);
            PollResult::Unavailable(detail)
        }
        Err(_) => PollResult::Unavailable(format!(
            "notification endpoint at {} ({}) is unavailable",
            endpoint.origin(),
            endpoint.fingerprint(),
        )),
    }
}

enum RefreshResult {
    Advertisement(DiscoveredNotificationAdvertisement),
    Missing,
    Unavailable(String),
    Stopped,
}

async fn refresh_for_registration_interruptible(
    registration: &WikiRegistration,
    stop: &Arc<AtomicBool>,
) -> RefreshResult {
    let registration = registration.clone();
    let refresh = tokio::task::spawn_blocking(move || refresh_for_registration(&registration));
    tokio::select! {
        () = wait_until_stopped(stop) => RefreshResult::Stopped,
        result = refresh => match result {
            Ok(Ok(Some(discovered))) => RefreshResult::Advertisement(discovered),
            Ok(Ok(None)) => RefreshResult::Missing,
            Ok(Err(detail)) => RefreshResult::Unavailable(detail),
            Err(error) => RefreshResult::Unavailable(format!(
                "notification advertisement worker failed: {error}"
            )),
        }
    }
}

fn refresh_for_registration(
    registration: &WikiRegistration,
) -> Result<Option<DiscoveredNotificationAdvertisement>, String> {
    let paths = VaultPaths::new(&registration.path);
    let selection = resolve_permission_profile(&paths, registration.permissions_profile.as_deref())
        .map_err(|error| error.to_string())?;
    let guard = ProfilePermissionGuard::new(&paths, selection);
    guard.check_git().map_err(|error| error.to_string())?;

    let engine = GitCliEngine::default().with_command_timeout(GIT_COMMAND_TIMEOUT);
    let repository = engine
        .discover_repository(&registration.path)
        .map_err(|error| error.to_string())?;
    let remote = GitRemote::parse("origin").map_err(|error| error.to_string())?;
    let discovered = refresh_notification_advertisement(&engine, &repository, &remote)
        .map_err(|error| error.to_string())?;
    if let Some(discovered) = &discovered {
        guard
            .check_network(discovered.advertisement.endpoint.origin())
            .map_err(|error| error.to_string())?;
    }
    Ok(discovered)
}

fn report_diagnostic_once(wiki_id: &str, detail: &str, previous: &mut Option<String>) {
    if previous.as_deref() != Some(detail) {
        eprintln!("notification listener for wiki `{wiki_id}`: {detail}");
        *previous = Some(detail.to_string());
    }
}

/// Renders a listener start as a verbose log line. Fires before advertisement
/// discovery, so it only states that the wiki has a listener.
#[must_use]
fn listener_line(wiki_id: &str) -> String {
    format!("sync notification: listener started for wiki `{wiki_id}`")
}

/// Renders an advertisement discovery as a verbose log line. Identifies the
/// endpoint by origin and fingerprint only — never the subscribe URL.
#[must_use]
fn advertisement_line(wiki_id: &str, endpoint: &NotificationEndpoint) -> String {
    format!(
        "sync notification: wiki `{wiki_id}` discovered {} ({})",
        endpoint.origin(),
        endpoint.fingerprint(),
    )
}

/// Renders a wake-up enqueue as a verbose log line. Identifies the endpoint
/// by origin and fingerprint only — never the subscribe URL.
#[must_use]
fn wake_line(wiki_id: &str, endpoint: &NotificationEndpoint) -> String {
    format!(
        "sync notification: wiki `{wiki_id}` wake-up from {} ({}) enqueued",
        endpoint.origin(),
        endpoint.fingerprint(),
    )
}

async fn wait_until_stopped(stop: &Arc<AtomicBool>) {
    while !stop.load(Ordering::Acquire) {
        tokio::time::sleep(STOP_POLL).await;
    }
}

async fn wait_for_stop(stop: &Arc<AtomicBool>, duration: Duration) -> bool {
    tokio::select! {
        () = wait_until_stopped(stop) => true,
        () = tokio::time::sleep(duration) => false,
    }
}

struct Backoff {
    initial: Duration,
    maximum: Duration,
    current: Duration,
}

impl Backoff {
    fn new(initial: Duration, maximum: Duration) -> Self {
        Self {
            initial,
            maximum,
            current: initial,
        }
    }

    fn reset(&mut self) {
        self.current = self.initial;
    }

    fn next_delay(&mut self) -> Duration {
        let delay = jittered_delay(self.current, random_entropy()).min(self.maximum);
        self.current = self.current.saturating_mul(2).min(self.maximum);
        delay
    }
}

fn random_entropy() -> u64 {
    let mut bytes = [0_u8; 8];
    getrandom::fill(&mut bytes).map_or(0, |()| u64::from_le_bytes(bytes))
}

fn jittered_delay(base: Duration, entropy: u64) -> Duration {
    let factor = 750_u128 + u128::from(entropy % 501);
    let millis = base.as_millis().saturating_mul(factor) / 1_000;
    Duration::from_millis(u64::try_from(millis).unwrap_or(u64::MAX).max(1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{AddWikiRequest, WikiId};
    use axum::http::StatusCode;
    use axum::routing::get;
    use axum::Router;
    use std::fs;
    use std::future::{pending, IntoFuture};
    use std::path::Path;
    use std::process::Command;
    use std::sync::atomic::AtomicUsize;
    use tempfile::tempdir;
    use tokio::net::TcpListener;

    #[test]
    fn desired_listener_set_and_restart_identity_follow_effective_registration() {
        let registration = |id: &str, paused: bool, backend: Option<&str>| WikiRegistration {
            id: WikiId::parse(id).expect("wiki id"),
            registration_id: ulid::Ulid::new(),
            path: Path::new("/").join(id),
            groups: Vec::new(),
            git_dir: None,
            permissions_profile: None,
            sync_backend: backend.map(str::to_string),
            platform_profile: None,
            sync_paused: paused,
        };
        let desired = desired_listeners(vec![
            registration("alpha", false, None),
            registration("beta", false, Some("git")),
            registration("paused", true, Some("git")),
            registration("other", false, Some("seafile")),
        ]);
        assert_eq!(
            desired.keys().cloned().collect::<Vec<_>>(),
            ["alpha", "beta"]
        );

        let current = registration("alpha", false, Some("git"));
        let mut metadata_only = current.clone();
        metadata_only.groups.push("daily".to_string());
        metadata_only.platform_profile = Some("android_shared".to_string());
        assert!(!listener_registration_changed(&current, &metadata_only));
        let mut permissions = current.clone();
        permissions.permissions_profile = Some("automation".to_string());
        assert!(listener_registration_changed(&current, &permissions));
    }

    #[test]
    fn verbose_notification_lines_redact_the_subscribe_url() {
        let advertisement = vulcan_sync::NotificationAdvertisement::parse(
            br#"{"version":1,"transport":"http_long_poll","subscribe_url":"https://relay.example/h/secret-channel?pubsub=true"}"#,
        )
        .expect("advertisement");
        let endpoint = &advertisement.endpoint;
        let discovered = advertisement_line("alpha", endpoint);
        assert!(discovered.contains("alpha"));
        assert!(discovered.contains("https://relay.example"));
        assert!(discovered.contains(endpoint.fingerprint()));
        assert!(!discovered.contains("secret-channel"));
        let wake = wake_line("alpha", endpoint);
        assert!(wake.contains("alpha"));
        assert!(wake.contains("https://relay.example"));
        assert!(!wake.contains("secret-channel"));
    }

    #[test]
    fn verbose_listener_line_names_the_wiki() {
        assert_eq!(
            listener_line("alpha"),
            "sync notification: listener started for wiki `alpha`"
        );
    }

    #[test]
    fn exponential_backoff_is_bounded_and_jittered() {
        assert_eq!(
            jittered_delay(Duration::from_millis(1_000), 0),
            Duration::from_millis(750)
        );
        assert_eq!(
            jittered_delay(Duration::from_millis(1_000), 500),
            Duration::from_millis(1_250)
        );
        let mut backoff = Backoff::new(Duration::from_millis(100), Duration::from_millis(200));
        let _ = backoff.next_delay();
        let _ = backoff.next_delay();
        let _ = backoff.next_delay();
        assert_eq!(backoff.current, Duration::from_millis(200));
        backoff.reset();
        assert_eq!(backoff.current, Duration::from_millis(100));
    }

    #[tokio::test]
    async fn advertised_wakes_coalesce_and_listener_shutdown_interrupts_long_poll() {
        let requests = Arc::new(AtomicUsize::new(0));
        let handler_requests = Arc::clone(&requests);
        let app = Router::new().route(
            "/{*path}",
            get(move || {
                let requests = Arc::clone(&handler_requests);
                async move {
                    if requests.fetch_add(1, Ordering::AcqRel) < 2 {
                        StatusCode::NO_CONTENT
                    } else {
                        pending::<StatusCode>().await
                    }
                }
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("notification listener");
        let address = listener.local_addr().expect("listener address");
        let server = tokio::spawn(axum::serve(listener, app).into_future());

        let temporary = tempdir().expect("temporary directory");
        let vault = create_advertised_repository(
            temporary.path(),
            &format!("http://{address}/private?pubsub=true"),
        );

        let registry = WikiRegistry::at(temporary.path().join("daemon.toml"));
        registry
            .add(
                &AddWikiRequest {
                    id: WikiId::parse("alpha").expect("wiki id"),
                    path: vault.clone(),
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
        let stop = Arc::new(AtomicBool::new(false));
        let runtime = tokio::spawn(run_notification_runtime_until(
            registry,
            Arc::clone(&supervisor),
            NotificationRuntimeOptions {
                registry_refresh_ms: 10,
                advertisement_refresh_ms: 60_000,
                initial_backoff_ms: 10,
                maximum_backoff_ms: 50,
                connect_timeout_ms: 100,
                verbose: true,
            },
            Arc::clone(&stop),
        ));

        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if requests.load(Ordering::Acquire) >= 3
                    && supervisor.list().is_ok_and(|jobs| {
                        jobs.len() == 1
                            && jobs[0]
                                .triggers
                                .contains(&SyncJobTrigger::RemoteNotification)
                    })
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("remote notification job");
        stop.store(true, Ordering::Release);
        tokio::time::timeout(Duration::from_secs(2), runtime)
            .await
            .expect("bounded runtime shutdown")
            .expect("runtime task")
            .expect("notification runtime");
        assert_eq!(supervisor.list().expect("jobs").len(), 1);
        assert!(requests.load(Ordering::Acquire) >= 3);
        server.abort();
    }

    #[tokio::test]
    async fn two_wikis_share_one_client_against_the_same_relay() {
        let requests = Arc::new(AtomicUsize::new(0));
        let handler_requests = Arc::clone(&requests);
        let app = Router::new().route(
            "/{*path}",
            get(move || {
                let requests = Arc::clone(&handler_requests);
                async move {
                    if requests.fetch_add(1, Ordering::AcqRel) < 4 {
                        StatusCode::NO_CONTENT
                    } else {
                        pending::<StatusCode>().await
                    }
                }
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("notification listener");
        let address = listener.local_addr().expect("listener address");
        let server = tokio::spawn(axum::serve(listener, app).into_future());

        let temporary = tempdir().expect("temporary directory");
        let endpoint = format!("http://{address}/shared?pubsub=true");
        let first = temporary.path().join("first");
        let second = temporary.path().join("second");
        std::fs::create_dir(&first).expect("first fixture directory");
        std::fs::create_dir(&second).expect("second fixture directory");
        let vault_alpha = create_advertised_repository(&first, &endpoint);
        let vault_beta = create_advertised_repository(&second, &endpoint);

        let registry = WikiRegistry::at(temporary.path().join("daemon.toml"));
        for (id, vault) in [("alpha", vault_alpha), ("beta", vault_beta)] {
            registry
                .add(
                    &AddWikiRequest {
                        id: WikiId::parse(id).expect("wiki id"),
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
        }
        let supervisor =
            Arc::new(SyncSupervisor::at(temporary.path().join("jobs.json")).expect("supervisor"));
        let stop = Arc::new(AtomicBool::new(false));
        let runtime = tokio::spawn(run_notification_runtime_until(
            registry,
            Arc::clone(&supervisor),
            NotificationRuntimeOptions {
                registry_refresh_ms: 10,
                advertisement_refresh_ms: 60_000,
                initial_backoff_ms: 10,
                maximum_backoff_ms: 50,
                connect_timeout_ms: 100,
                verbose: true,
            },
            Arc::clone(&stop),
        ));

        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if requests.load(Ordering::Acquire) >= 5
                    && supervisor.list().is_ok_and(|jobs| {
                        jobs.len() == 2
                            && jobs.iter().all(|job| {
                                job.triggers.contains(&SyncJobTrigger::RemoteNotification)
                            })
                    })
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("remote notification jobs for both wikis");
        stop.store(true, Ordering::Release);
        tokio::time::timeout(Duration::from_secs(2), runtime)
            .await
            .expect("bounded runtime shutdown")
            .expect("runtime task")
            .expect("notification runtime");
        server.abort();
    }

    #[tokio::test]
    async fn notification_http_client_does_not_follow_redirects() {
        let target_requests = Arc::new(AtomicUsize::new(0));
        let handler_requests = Arc::clone(&target_requests);
        let target = Router::new().route(
            "/target",
            get(move || {
                let requests = Arc::clone(&handler_requests);
                async move {
                    requests.fetch_add(1, Ordering::AcqRel);
                    StatusCode::NO_CONTENT
                }
            }),
        );
        let target_listener = TcpListener::bind("127.0.0.1:0").await.expect("target");
        let target_address = target_listener.local_addr().expect("target address");
        let target_server = tokio::spawn(axum::serve(target_listener, target).into_future());

        let redirect = Router::new().route(
            "/source",
            get(move || async move {
                axum::response::Redirect::temporary(&format!("http://{target_address}/target"))
            }),
        );
        let redirect_listener = TcpListener::bind("127.0.0.1:0").await.expect("redirect");
        let redirect_address = redirect_listener.local_addr().expect("redirect address");
        let redirect_server = tokio::spawn(axum::serve(redirect_listener, redirect).into_future());
        let advertisement = vulcan_sync::NotificationAdvertisement::parse(
            format!(
                r#"{{"version":1,"transport":"http_long_poll","subscribe_url":"http://{redirect_address}/source"}}"#
            )
            .as_bytes(),
        )
        .expect("advertisement");
        let client = build_notification_client(Duration::from_secs(5)).expect("client");
        let result = poll_endpoint(&client, &advertisement.endpoint).await;
        assert!(
            matches!(result, PollResult::Unavailable(ref detail) if detail.ends_with("HTTP 307"))
        );
        assert!(!format!("{result:?}").contains("/source"));
        tokio::time::sleep(Duration::from_millis(25)).await;
        assert_eq!(target_requests.load(Ordering::Acquire), 0);
        redirect_server.abort();
        target_server.abort();
    }

    #[test]
    fn advertisement_discovery_applies_the_registration_git_permission_first() {
        let temporary = tempdir().expect("temporary directory");
        let vault = create_advertised_repository(
            temporary.path(),
            "https://patch.example/private?pubsub=true",
        );
        let registration = WikiRegistration {
            id: WikiId::parse("alpha").expect("wiki id"),
            registration_id: ulid::Ulid::new(),
            path: vault,
            groups: Vec::new(),
            git_dir: None,
            permissions_profile: Some("readonly".to_string()),
            sync_backend: Some("git".to_string()),
            platform_profile: None,
            sync_paused: false,
        };

        let error = refresh_for_registration(&registration).expect_err("Git must be denied");
        assert!(error.contains("git access"));
        assert!(!error.contains("private"));
    }

    fn create_advertised_repository(root: &Path, endpoint: &str) -> std::path::PathBuf {
        let remote = root.join("remote.git");
        let vault = root.join("vault");
        run_git(root, &["init", "--bare", remote.to_str().expect("remote")]);
        fs::create_dir(&vault).expect("vault directory");
        run_git(&vault, &["init"]);
        run_git(&vault, &["config", "user.name", "Vulcan Tests"]);
        run_git(&vault, &["config", "user.email", "vulcan@example.invalid"]);
        run_git(
            &vault,
            &["remote", "add", "origin", remote.to_str().expect("remote")],
        );
        fs::write(
            vault.join("notification.json"),
            format!(r#"{{"version":1,"transport":"http_long_poll","subscribe_url":"{endpoint}"}}"#),
        )
        .expect("advertisement");
        run_git(&vault, &["add", "notification.json"]);
        run_git(&vault, &["commit", "-m", "advertise notifications"]);
        run_git(
            &vault,
            &["push", "origin", "HEAD:refs/vulcan/notifications"],
        );
        vault
    }

    fn run_git(directory: &Path, arguments: &[&str]) {
        let output = Command::new("git")
            .args(arguments)
            .current_dir(directory)
            .output()
            .expect("run Git");
        assert!(
            output.status.success(),
            "git {arguments:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
