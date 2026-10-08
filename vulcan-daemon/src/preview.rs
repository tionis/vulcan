//! Reusable live-reload preview of generated output: a static site, a
//! frontend bundle, or any other builder. One session builds once, serves the
//! output on a loopback listener through the shared blocking HTTP transport,
//! publishes live-reload state as JSON and server-sent events, and rebuilds
//! on relevant vault changes. Foreground commands run a session as a
//! temporary host; the builders themselves stay finite `vulcan-app`
//! workflows.

use crate::host::{
    HostStatusHandle, HostSupervisor, RestartPolicy, ServiceDefinition, ServiceId,
    ServiceLifecycleState, ServiceRegistration, ServiceScope,
};
use crate::mcp_http_codec::{write_mcp_http_response, McpHttpRequest, McpHttpResponse};
use crate::mcp_transport::McpHttpListener;
use crate::shutdown::ShutdownSignal;
use serde_json::{json, Value};
use std::io::{self, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use vulcan_core::paths::secure_read;
use vulcan_core::{watch_vault_until, VaultPaths, WatchOptions};

const LIVE_RELOAD_INTERVAL: Duration = Duration::from_millis(350);
const WATCH_STARTUP_TIMEOUT: Duration = Duration::from_secs(5);

/// What a preview serves and how it rebuilds. Implementations wrap a finite
/// builder; the session owns serving, live reload, and rebuild scheduling.
pub trait PreviewBuilder: Send + Sync + 'static {
    type Report: Clone + Send + 'static;

    /// Used in messages, such as `site serve`.
    fn label(&self) -> &'static str;
    /// The service family suffix: `preview.<kind>/<session-id>`, such as
    /// `site` or `bundle`. Lower-case ASCII letters, digits, and `-`.
    fn kind(&self) -> &'static str;
    /// Live-reload endpoints are `/<namespace>/live-reload.json` and
    /// `/<namespace>/live-reload.events`, also under the deploy path.
    fn namespace(&self) -> &'static str;
    fn build(&self) -> Result<Self::Report, String>;
    fn output_dir(&self, report: &Self::Report) -> PathBuf;
    /// The URL prefix the output is deployed under; empty for none.
    fn deploy_path(&self, _report: &Self::Report) -> String {
        String::new()
    }
    /// The output-relative file for a decoded request path, or `None` for
    /// 404. Must never resolve outside the output directory or to a symlink.
    fn resolve(&self, output_dir: &Path, request_path: &str, deploy_path: &str) -> Option<PathBuf>;
    fn content_type(&self, path: &Path) -> &'static str;
    /// Whether a successful rebuild changes what clients should reload.
    fn changed(&self, previous: &Self::Report, next: &Self::Report) -> bool;
    fn live_reload_payload(
        &self,
        report: &Self::Report,
        version: u64,
        last_error: Option<&str>,
    ) -> Value;
    /// Called after each successful rebuild, for logging.
    fn rebuilt(&self, _report: &Self::Report) {}
    /// Called after each failed rebuild, for logging.
    fn rebuild_failed(&self, _error: &str) {}
}

#[derive(Debug, Clone, Copy)]
pub struct PreviewOptions {
    /// Loopback port; 0 picks a free one.
    pub port: u16,
    /// Rebuild on vault changes.
    pub watch: bool,
    pub debounce_ms: u64,
}

struct PreviewState<R> {
    output_dir: PathBuf,
    deploy_path: String,
    report: R,
    version: u64,
    last_error: Option<String>,
}

struct Preview<B: PreviewBuilder> {
    builder: B,
    state: Mutex<PreviewState<B::Report>>,
}

impl<B: PreviewBuilder> Preview<B> {
    fn new(builder: B, report: B::Report) -> Self {
        let state = PreviewState {
            output_dir: builder.output_dir(&report),
            deploy_path: builder.deploy_path(&report),
            report,
            version: 1,
            last_error: None,
        };
        Self {
            builder,
            state: Mutex::new(state),
        }
    }

    fn rebuild(&self) {
        match self.builder.build() {
            Ok(report) => {
                self.builder.rebuilt(&report);
                if let Ok(mut state) = self.state.lock() {
                    let bump =
                        self.builder.changed(&state.report, &report) || state.last_error.is_some();
                    state.output_dir = self.builder.output_dir(&report);
                    state.deploy_path = self.builder.deploy_path(&report);
                    state.report = report;
                    if bump {
                        state.version = state.version.saturating_add(1);
                    }
                    state.last_error = None;
                }
            }
            Err(error) => {
                self.builder.rebuild_failed(&error);
                self.record_error(error);
            }
        }
    }

    /// The last good output stays served; clients see the error.
    fn record_error(&self, error: String) {
        if let Ok(mut state) = self.state.lock() {
            if state.last_error.as_deref() != Some(error.as_str()) {
                state.version = state.version.saturating_add(1);
            }
            state.last_error = Some(error);
        }
    }

    fn live_reload(&self) -> Value {
        self.state.lock().map_or_else(
            |_| json!({ "ok": false, "error": format!("{} state unavailable", self.builder.label()) }),
            |state| {
                self.builder.live_reload_payload(
                    &state.report,
                    state.version,
                    state.last_error.as_deref(),
                )
            },
        )
    }

    fn endpoint(&self, name: &str) -> (String, String) {
        let root = format!("/{}/{name}", self.builder.namespace());
        let deployed = self
            .state
            .lock()
            .ok()
            .map(|state| state.deploy_path.clone())
            .filter(|deploy_path| !deploy_path.is_empty())
            .map_or_else(
                || root.clone(),
                |deploy_path| format!("{deploy_path}{root}"),
            );
        (root, deployed)
    }

    fn is_endpoint(&self, path: &str, name: &str) -> bool {
        let (root, deployed) = self.endpoint(name);
        path == root || path == deployed
    }

    fn handle(&self, request: &McpHttpRequest, stream: &mut TcpStream, stop: &ShutdownSignal) {
        let path = percent_decode(&request.path);
        if request.method == "GET" && self.is_endpoint(&path, "live-reload.events") {
            let _ = self.stream_live_reload(stream, stop);
            return;
        }
        let _ = write_mcp_http_response(stream, &self.route(&request.method, &path));
    }

    fn route(&self, method: &str, path: &str) -> McpHttpResponse {
        if method != "GET" && method != "HEAD" {
            return text(405, "method not allowed");
        }
        if self.is_endpoint(path, "live-reload.json") {
            return McpHttpResponse {
                status: 200,
                content_type: Some("application/json"),
                body: serde_json::to_vec(&self.live_reload())
                    .expect("live-reload payload should serialize"),
                extra_headers: no_store(),
            };
        }
        let Some((output_dir, file)) = self.state.lock().ok().and_then(|state| {
            self.builder
                .resolve(&state.output_dir, path, &state.deploy_path)
                .map(|file| (state.output_dir.clone(), file))
        }) else {
            return text(404, "not found");
        };
        let candidate = output_dir.join(&file);
        match secure_read(&output_dir, &file) {
            Ok(body) => McpHttpResponse {
                status: 200,
                content_type: Some(self.builder.content_type(&candidate)),
                body,
                extra_headers: no_store(),
            },
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound
                        | io::ErrorKind::InvalidInput
                        | io::ErrorKind::PermissionDenied
                ) =>
            {
                text(404, "not found")
            }
            Err(error) => text(
                500,
                format!("failed to read {}: {error}", candidate.display()),
            ),
        }
    }

    fn stream_live_reload(&self, stream: &mut TcpStream, stop: &ShutdownSignal) -> io::Result<()> {
        stream.write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n",
        )?;
        stream.flush()?;
        let mut last_sent = String::new();
        while !stop.is_cancelled() {
            let payload = serde_json::to_string(&self.live_reload())
                .expect("live-reload payload should serialize");
            if payload == last_sent {
                stream.write_all(b": keep-alive\r\n\r\n")?;
            } else {
                stream.write_all(b"event: update\r\ndata: ")?;
                stream.write_all(payload.as_bytes())?;
                stream.write_all(b"\r\n\r\n")?;
                last_sent = payload;
            }
            stream.flush()?;
            if stop.wait_timeout(LIVE_RELOAD_INTERVAL) {
                break;
            }
        }
        Ok(())
    }
}

fn no_store() -> Vec<(String, String)> {
    vec![("Cache-Control".to_string(), "no-store".to_string())]
}

fn text(status: u16, body: impl Into<String>) -> McpHttpResponse {
    McpHttpResponse {
        status,
        content_type: Some("text/plain; charset=utf-8"),
        body: body.into().into_bytes(),
        extra_headers: no_store(),
    }
}

/// A running preview: its listener and optional rebuild watcher run as
/// services of an ephemeral [`HostSupervisor`], with dependency-ordered
/// readiness and reverse shutdown. Dropping it without [`Self::shutdown`]
/// leaves it serving until the process exits, as a foreground command expects.
pub struct PreviewSession {
    addr: SocketAddr,
    stop: Arc<ShutdownSignal>,
    host: HostSupervisor,
}

impl PreviewSession {
    #[must_use]
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Lifecycle state of the session's services.
    #[must_use]
    pub fn status_handle(&self) -> HostStatusHandle {
        self.host.status_handle()
    }

    /// Wait until the session stops, then release its listener and watcher.
    /// A failed service is reported as the error.
    pub fn join(self) -> Result<(), String> {
        while !self.stop.wait_timeout(Duration::from_secs(1)) {}
        let failed = self
            .host
            .status_handle()
            .statuses()
            .map_err(|error| error.to_string())?
            .into_iter()
            .find(|status| status.state == ServiceLifecycleState::Failed);
        self.host.shutdown().map_err(|error| error.to_string())?;
        failed.map_or(Ok(()), |failed| {
            Err(format!(
                "preview service `{}` failed: {}",
                failed.id,
                failed
                    .last_failure
                    .map_or_else(|| "unknown failure".to_string(), |failure| failure.detail)
            ))
        })
    }

    /// Stop serving and watching, then join both.
    pub fn shutdown(self) -> Result<(), String> {
        self.stop.cancel();
        self.join()
    }
}

/// Build once, then serve and (optionally) rebuild on vault changes. A
/// failed initial build, bind, or watch start starts nothing. Rebuilds ignore
/// changes that lie only inside the preview's own output or `.vulcan/`.
pub fn start_preview<B: PreviewBuilder>(
    paths: &VaultPaths,
    builder: B,
    options: PreviewOptions,
) -> Result<PreviewSession, String> {
    let listener = McpHttpListener::bind(SocketAddr::from(([127, 0, 0, 1], options.port)))
        .map_err(|error| error.to_string())?;
    let addr = listener.local_addr();
    let session_id = ulid::Ulid::new().to_string().to_ascii_lowercase();
    let registrations = preview_services(paths, builder, listener, options, &session_id)?;
    let stop = Arc::new(ShutdownSignal::new(false));
    let host =
        HostSupervisor::start_with_signal(registrations, WATCH_STARTUP_TIMEOUT, Arc::clone(&stop))
            .map_err(|error| error.to_string())?;
    Ok(PreviewSession { addr, stop, host })
}

/// Build once and describe one preview session as host services:
/// `preview.<kind>/<session-id>` serves the bound listener and, when
/// watching, depends on `observation.preview/<session-id>`, which rebuilds on
/// relevant changes. A failed initial build returns the build error. Any host
/// can run the registrations; the session owns no daemon-global state.
pub fn preview_services<B: PreviewBuilder>(
    paths: &VaultPaths,
    builder: B,
    listener: McpHttpListener,
    options: PreviewOptions,
    session_id: &str,
) -> Result<Vec<ServiceRegistration>, String> {
    let report = builder.build()?;
    let kind = builder.kind();
    let preview = Arc::new(Preview::new(builder, report));
    let scope = ServiceScope::Instance {
        instance_id: format!("preview-{session_id}"),
    };
    let mut registrations = Vec::new();
    let mut dependencies = Vec::new();
    if options.watch {
        let id = ServiceId::parse(format!("observation.preview/{session_id}"))
            .map_err(|error| error.to_string())?;
        dependencies.push(id.clone());
        registrations.push(rebuild_service(
            id,
            scope.clone(),
            paths.clone(),
            Arc::clone(&preview),
            options,
        ));
    }
    let listener = Arc::new(Mutex::new(Some(listener)));
    registrations.push(ServiceRegistration::new(
        ServiceDefinition {
            id: ServiceId::parse(format!("preview.{kind}/{session_id}"))
                .map_err(|error| error.to_string())?,
            service_kind: "preview".to_string(),
            scope,
            enabled: true,
            required: true,
            dependencies,
            restart: RestartPolicy::Never,
        },
        move |service| {
            let listener = listener
                .lock()
                .map_err(|_| "preview listener state is unavailable".to_string())?
                .take()
                .ok_or_else(|| "preview listener was already consumed".to_string())?;
            service.ready()?;
            let stop = Arc::clone(service.stop());
            let handler_stop = Arc::clone(&stop);
            let preview = Arc::clone(&preview);
            listener
                .serve_with_errors(
                    Some(&stop),
                    move |request, stream| preview.handle(request, stream, &handler_stop),
                    |error| text(error.status, error.message.clone()),
                )
                .map_err(|error| format!("preview listener failed: {error}"))
        },
    ));
    Ok(registrations)
}

/// Ready once the watcher has started; a later watch failure keeps the last
/// output served and shows the error to clients instead of stopping the
/// session.
fn rebuild_service<B: PreviewBuilder>(
    id: ServiceId,
    scope: ServiceScope,
    paths: VaultPaths,
    preview: Arc<Preview<B>>,
    options: PreviewOptions,
) -> ServiceRegistration {
    ServiceRegistration::new(
        ServiceDefinition {
            id,
            service_kind: "observation".to_string(),
            scope,
            enabled: true,
            required: true,
            dependencies: Vec::new(),
            restart: RestartPolicy::Never,
        },
        move |service| {
            let mut ready = false;
            let result = watch_vault_until(
                &paths,
                &WatchOptions {
                    debounce_ms: options.debounce_ms,
                },
                || service.stop().is_cancelled(),
                |report| {
                    if !ready {
                        service.ready()?;
                        ready = true;
                    }
                    if !report.startup && relevant(&paths, &preview, &report.paths) {
                        preview.rebuild();
                    }
                    Ok::<_, String>(())
                },
            );
            match result {
                Err(error) if !ready => Err(format!(
                    "{} watch failed to start: {error}",
                    preview.builder.label()
                )),
                Err(error) => {
                    preview.record_error(error.to_string());
                    while !service.stop().wait_timeout(Duration::from_secs(1)) {}
                    Ok(())
                }
                Ok(()) => Ok(()),
            }
        },
    )
}

/// Whether any changed vault path can affect the output: changes only to the
/// preview's own output (when it lies inside the vault) or to `.vulcan/`
/// cannot. An empty change set (a safety rescan) is always relevant.
fn relevant<B: PreviewBuilder>(
    paths: &VaultPaths,
    preview: &Preview<B>,
    changed: &[String],
) -> bool {
    let output = preview.state.lock().ok().and_then(|state| {
        let root = paths.vault_root().canonicalize().ok()?;
        let output = state.output_dir.canonicalize().ok()?;
        output
            .strip_prefix(root)
            .ok()
            .map(|relative| relative.to_string_lossy().replace('\\', "/"))
    });
    let inside = |path: &str, prefix: &str| {
        !prefix.is_empty()
            && (path == prefix
                || path
                    .strip_prefix(prefix)
                    .is_some_and(|rest| rest.starts_with('/')))
    };
    changed.is_empty()
        || changed.iter().any(|path| {
            !inside(path, ".vulcan") && output.as_deref().is_none_or(|output| !inside(path, output))
        })
}

/// Decode `%XX` escapes and `+` as in the original preview servers;
/// malformed escapes are kept literally, and undecodable results unchanged.
#[must_use]
pub fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => {
                decoded.push(b' ');
                index += 1;
            }
            b'%' if index + 2 < bytes.len() => {
                let byte = std::str::from_utf8(&bytes[index + 1..index + 3])
                    .ok()
                    .and_then(|hex| u8::from_str_radix(hex, 16).ok());
                if let Some(byte) = byte {
                    decoded.push(byte);
                    index += 3;
                } else {
                    decoded.push(bytes[index]);
                    index += 1;
                }
            }
            byte => {
                decoded.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8(decoded).unwrap_or_else(|_| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::thread;

    /// Serves `out/` under the vault; each build reports the next counter
    /// value, or fails while `fail` is set.
    struct Fake {
        out: PathBuf,
        builds: Arc<AtomicU64>,
        fail: Arc<Mutex<Option<String>>>,
    }

    impl PreviewBuilder for Fake {
        type Report = u64;
        fn label(&self) -> &'static str {
            "fake serve"
        }
        fn kind(&self) -> &'static str {
            "fake"
        }
        fn namespace(&self) -> &'static str {
            "__fake"
        }
        fn build(&self) -> Result<u64, String> {
            if let Some(error) = self.fail.lock().unwrap().clone() {
                return Err(error);
            }
            Ok(self.builds.fetch_add(1, Ordering::SeqCst) + 1)
        }
        fn output_dir(&self, _report: &u64) -> PathBuf {
            self.out.clone()
        }
        fn deploy_path(&self, _report: &u64) -> String {
            "/docs".to_string()
        }
        fn resolve(&self, _output: &Path, request: &str, deploy: &str) -> Option<PathBuf> {
            let relative = request.strip_prefix(deploy).unwrap_or(request);
            let relative = relative.trim_start_matches('/');
            Some(PathBuf::from(if relative.is_empty() {
                "index.html"
            } else {
                relative
            }))
        }
        fn content_type(&self, _path: &Path) -> &'static str {
            "text/html; charset=utf-8"
        }
        fn changed(&self, previous: &u64, next: &u64) -> bool {
            previous % 2 != next % 2
        }
        fn live_reload_payload(&self, report: &u64, version: u64, error: Option<&str>) -> Value {
            json!({ "ok": error.is_none(), "build": report, "version": version, "error": error })
        }
    }

    fn fake(vault: &Path) -> (Fake, Arc<AtomicU64>, Arc<Mutex<Option<String>>>) {
        let out = vault.join("out");
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(out.join("index.html"), "<p>hi</p>").unwrap();
        let builds = Arc::new(AtomicU64::new(0));
        let fail = Arc::new(Mutex::new(None));
        (
            Fake {
                out,
                builds: Arc::clone(&builds),
                fail: Arc::clone(&fail),
            },
            builds,
            fail,
        )
    }

    fn get(addr: SocketAddr, method: &str, path: &str) -> (u16, String) {
        let mut stream = TcpStream::connect(addr).unwrap();
        write!(
            stream,
            "{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        let status = response[9..12].parse().unwrap();
        let body = response
            .split_once("\r\n\r\n")
            .map_or_else(String::new, |(_, body)| body.to_string());
        (status, body)
    }

    #[test]
    fn percent_decode_keeps_malformed_escapes() {
        assert_eq!(percent_decode("/a%20b+c"), "/a b c");
        assert_eq!(percent_decode("/bad%zz"), "/bad%zz");
        assert_eq!(percent_decode("/trail%2"), "/trail%2");
        assert_eq!(percent_decode("/%ff"), "/%ff");
    }

    #[test]
    fn rebuilds_bump_the_version_only_for_visible_changes_and_errors() {
        let dir = tempfile::tempdir().unwrap();
        let (builder, _, fail) = fake(dir.path());
        let report = builder.build().unwrap();
        let preview = Preview::new(builder, report);
        assert_eq!(preview.live_reload()["version"], 1);

        preview.rebuild(); // 1 -> 2: changed
        assert_eq!(preview.live_reload()["version"], 2);
        *fail.lock().unwrap() = Some("broken".to_string());
        preview.rebuild();
        preview.rebuild(); // the same error again does not bump
        let payload = preview.live_reload();
        assert_eq!(payload["version"], 3);
        assert_eq!(payload["error"], "broken");
        assert_eq!(payload["build"], 2, "the last good output stays current");

        *fail.lock().unwrap() = None;
        preview.rebuild(); // 2 -> 3 would change anyway; recovery always bumps
        let payload = preview.live_reload();
        assert_eq!(payload["version"], 4);
        assert!(payload["error"].is_null());
    }

    #[test]
    fn changes_inside_the_output_or_vulcan_dir_do_not_rebuild() {
        let dir = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(dir.path());
        let (builder, _, _) = fake(dir.path());
        let report = builder.build().unwrap();
        let preview = Preview::new(builder, report);
        let changed = |list: &[&str]| {
            relevant(
                &paths,
                &preview,
                &list.iter().map(ToString::to_string).collect::<Vec<_>>(),
            )
        };
        assert!(!changed(&["out/index.html", ".vulcan/cache.db"]));
        assert!(changed(&["outline.md"]), "a sibling sharing the prefix");
        assert!(changed(&["out/index.html", "notes/a.md"]));
        assert!(changed(&[]), "a safety rescan is always relevant");
    }

    #[test]
    fn session_serves_output_endpoints_and_stops_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(dir.path());
        let (builder, builds, _) = fake(dir.path());
        let session = start_preview(
            &paths,
            builder,
            PreviewOptions {
                port: 0,
                watch: false,
                debounce_ms: 50,
            },
        )
        .unwrap();
        let addr = session.addr();
        assert_eq!(builds.load(Ordering::SeqCst), 1);

        assert_eq!(get(addr, "GET", "/docs/"), (200, "<p>hi</p>".to_string()));
        assert_eq!(get(addr, "HEAD", "/docs/missing.html").0, 404);
        assert_eq!(get(addr, "POST", "/docs/").0, 405);
        for endpoint in ["/__fake/live-reload.json", "/docs/__fake/live-reload.json"] {
            let (status, body) = get(addr, "GET", endpoint);
            assert_eq!(status, 200);
            let payload: Value = serde_json::from_str(&body).unwrap();
            assert_eq!(payload["version"], 1);
        }

        let mut events = TcpStream::connect(addr).unwrap();
        events
            .write_all(b"GET /docs/__fake/live-reload.events HTTP/1.1\r\nHost: x\r\n\r\n")
            .unwrap();
        events
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut reader = BufReader::new(events);
        let mut seen = String::new();
        while !seen.contains("data: ") {
            let mut line = String::new();
            assert!(reader.read_line(&mut line).unwrap() > 0, "stream ended");
            seen.push_str(&line);
        }
        assert!(seen.contains("text/event-stream"));
        assert!(seen.contains("event: update"));

        session.shutdown().unwrap();
        assert!(TcpStream::connect(addr).is_err() || get_fails(addr));
    }

    /// After shutdown the port may linger in the backlog briefly, but nothing
    /// answers requests.
    fn get_fails(addr: SocketAddr) -> bool {
        let Ok(mut stream) = TcpStream::connect(addr) else {
            return true;
        };
        let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
        let _ = stream.write_all(b"GET / HTTP/1.1\r\n\r\n");
        let mut buffer = [0_u8; 16];
        !matches!(stream.read(&mut buffer), Ok(read) if read > 0)
    }

    fn watched(dir: &Path) -> (PreviewSession, Arc<AtomicU64>) {
        vulcan_core::initialize_vault(&VaultPaths::new(dir)).unwrap();
        let (builder, builds, _) = fake(dir);
        let session = start_preview(
            &VaultPaths::new(dir),
            builder,
            PreviewOptions {
                port: 0,
                watch: true,
                debounce_ms: 50,
            },
        )
        .unwrap();
        (session, builds)
    }

    fn wait_for(mut condition: impl FnMut() -> bool) -> bool {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            if condition() {
                return true;
            }
            thread::sleep(Duration::from_millis(50));
        }
        false
    }

    #[test]
    fn watched_sessions_run_as_host_services_and_rebuild_independently() {
        let first_dir = tempfile::tempdir().unwrap();
        let second_dir = tempfile::tempdir().unwrap();
        let (first, first_builds) = watched(first_dir.path());
        let (second, second_builds) = watched(second_dir.path());

        let statuses = first.status_handle().statuses().unwrap();
        let ids = statuses
            .iter()
            .map(|status| status.id.as_str().to_string())
            .collect::<Vec<_>>();
        assert_eq!(ids.len(), 2, "{ids:?}");
        assert!(ids.iter().any(|id| id.starts_with("preview.fake/")));
        assert!(ids.iter().any(|id| id.starts_with("observation.preview/")));
        assert!(statuses
            .iter()
            .all(|status| status.state == ServiceLifecycleState::Ready));
        assert!(!second
            .status_handle()
            .statuses()
            .unwrap()
            .iter()
            .any(|status| ids.contains(&status.id.as_str().to_string())));

        // Output-only changes are ignored; a note change rebuilds only its vault.
        std::fs::write(first_dir.path().join("out/extra.html"), "x").unwrap();
        thread::sleep(Duration::from_millis(400));
        assert_eq!(first_builds.load(Ordering::SeqCst), 1);
        std::fs::write(first_dir.path().join("note.md"), "# changed").unwrap();
        assert!(wait_for(|| first_builds.load(Ordering::SeqCst) >= 2));
        assert_eq!(second_builds.load(Ordering::SeqCst), 1);

        // Stopping one session leaves the other serving.
        let first_addr = first.addr();
        first.shutdown().unwrap();
        assert!(get_fails(first_addr));
        assert_eq!(get(second.addr(), "GET", "/docs/").0, 200);
        second.shutdown().unwrap();
    }

    fn open_events(addr: SocketAddr) -> BufReader<TcpStream> {
        let mut stream = TcpStream::connect(addr).unwrap();
        stream
            .write_all(b"GET /__fake/live-reload.events HTTP/1.1\r\nHost: x\r\n\r\n")
            .unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        assert!(line.starts_with("HTTP/1.1 200"), "{line}");
        reader
    }

    #[test]
    fn disconnected_live_reload_clients_release_their_connections() {
        let dir = tempfile::tempdir().unwrap();
        let (builder, _, _) = fake(dir.path());
        let session = start_preview(
            &VaultPaths::new(dir.path()),
            builder,
            PreviewOptions {
                port: 0,
                watch: false,
                debounce_ms: 50,
            },
        )
        .unwrap();
        // More streams than the listener admits at once, each dropped by its
        // client: the next keep-alive write fails and frees the slot.
        for _ in 0..3 {
            let streams = (0..40)
                .map(|_| open_events(session.addr()))
                .collect::<Vec<_>>();
            drop(streams);
            thread::sleep(LIVE_RELOAD_INTERVAL * 3);
        }
        assert!(wait_for(|| get(session.addr(), "GET", "/docs/").0 == 200));
        session.shutdown().unwrap();
    }

    #[test]
    fn stopping_a_session_ends_its_live_reload_streams() {
        let dir = tempfile::tempdir().unwrap();
        let (builder, _, _) = fake(dir.path());
        let session = start_preview(
            &VaultPaths::new(dir.path()),
            builder,
            PreviewOptions {
                port: 0,
                watch: false,
                debounce_ms: 50,
            },
        )
        .unwrap();
        let mut events = open_events(session.addr());
        session.shutdown().unwrap();
        let mut rest = String::new();
        let started = std::time::Instant::now();
        events
            .read_to_string(&mut rest)
            .expect("the stream closes instead of timing out");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn a_failed_initial_build_starts_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (builder, _, fail) = fake(dir.path());
        *fail.lock().unwrap() = Some("no output".to_string());
        let result = start_preview(
            &VaultPaths::new(dir.path()),
            builder,
            PreviewOptions {
                port: 0,
                watch: true,
                debounce_ms: 50,
            },
        );
        assert_eq!(result.err().as_deref(), Some("no output"));
    }
}
