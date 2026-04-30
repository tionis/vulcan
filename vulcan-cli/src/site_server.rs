use crate::CliError;
use serde_json::json;
use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use vulcan_app::site::{build_site as app_build_site, SiteBuildReport, SiteBuildRequest};
use vulcan_core::{watch_vault_until, VaultPaths, WatchOptions};

#[derive(Debug, Clone)]
pub struct SiteServeOptions {
    pub profile: Option<String>,
    pub output_dir: Option<PathBuf>,
    pub port: u16,
    pub watch: bool,
    pub debounce_ms: u64,
    pub strict: bool,
    pub fail_on_warning: bool,
}

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug)]
pub struct SiteServeHandle {
    addr: SocketAddr,
    shutdown: Arc<AtomicBool>,
    join_handle: Option<thread::JoinHandle<Result<(), CliError>>>,
}

#[cfg(test)]
impl SiteServeHandle {
    #[must_use]
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn shutdown(mut self) -> Result<(), CliError> {
        self.shutdown.store(true, Ordering::SeqCst);
        if let Some(join_handle) = self.join_handle.take() {
            join_handle
                .join()
                .map_err(|_| CliError::operation("site serve thread panicked"))??;
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
struct SiteServeState {
    output_dir: PathBuf,
    report: SiteBuildReport,
    version: u64,
    last_error: Option<String>,
}

#[derive(Debug)]
struct Request {
    method: String,
    path: String,
}

#[derive(Debug)]
struct Response {
    status: u16,
    content_type: &'static str,
    body: Vec<u8>,
    cache_control: Option<&'static str>,
}

pub(crate) fn site_build_policy_error(
    report: &SiteBuildReport,
    strict: bool,
    fail_on_warning: bool,
) -> Option<String> {
    if !(strict || fail_on_warning) {
        return None;
    }
    let diagnostics = report
        .diagnostics
        .iter()
        .filter(|diagnostic| matches!(diagnostic.level.as_str(), "warn" | "error"))
        .collect::<Vec<_>>();
    if diagnostics.is_empty() {
        return None;
    }
    let preview = diagnostics
        .iter()
        .take(3)
        .map(|diagnostic| match diagnostic.source_path.as_deref() {
            Some(path) => format!(
                "[{}] {} {} ({path})",
                diagnostic.level, diagnostic.kind, diagnostic.message
            ),
            None => format!(
                "[{}] {} {}",
                diagnostic.level, diagnostic.kind, diagnostic.message
            ),
        })
        .collect::<Vec<_>>()
        .join("; ");
    Some(format!(
        "site build for profile `{}` reported {} publish diagnostic(s): {}",
        report.profile,
        diagnostics.len(),
        preview
    ))
}

pub(crate) fn build_site_with_policy(
    paths: &VaultPaths,
    request: &SiteBuildRequest,
    strict: bool,
    fail_on_warning: bool,
) -> Result<SiteBuildReport, CliError> {
    if strict || fail_on_warning {
        let mut preflight = request.clone();
        preflight.dry_run = true;
        let preflight_report = app_build_site(paths, &preflight).map_err(CliError::operation)?;
        if let Some(message) = site_build_policy_error(&preflight_report, strict, fail_on_warning) {
            return Err(CliError::operation(message));
        }
    }
    app_build_site(paths, request).map_err(CliError::operation)
}

pub fn serve_site_forever(paths: &VaultPaths, options: &SiteServeOptions) -> Result<(), CliError> {
    let mut handle = spawn_site_server(paths.clone(), options.clone())?;
    if let Some(join_handle) = handle.join_handle.take() {
        join_handle
            .join()
            .map_err(|_| CliError::operation("site serve thread panicked"))??;
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
pub fn spawn_site_server(
    paths: VaultPaths,
    options: SiteServeOptions,
) -> Result<SiteServeHandle, CliError> {
    let initial_request = SiteBuildRequest {
        profile: options.profile.clone(),
        output_dir: options.output_dir.clone(),
        clean: false,
        dry_run: false,
    };
    let initial_report = build_site_with_policy(
        &paths,
        &initial_request,
        options.strict,
        options.fail_on_warning,
    )?;
    let initial_output_dir = PathBuf::from(&initial_report.output_dir);

    let listener = TcpListener::bind(("127.0.0.1", options.port)).map_err(CliError::operation)?;
    listener
        .set_nonblocking(true)
        .map_err(CliError::operation)?;
    let addr = listener.local_addr().map_err(CliError::operation)?;

    let shutdown = Arc::new(AtomicBool::new(false));
    let state = Arc::new(Mutex::new(SiteServeState {
        output_dir: initial_output_dir,
        report: initial_report,
        version: 1,
        last_error: None,
    }));
    let join_shutdown = Arc::clone(&shutdown);
    let join_state = Arc::clone(&state);

    let join_handle = thread::spawn(move || {
        let watch_handle = if options.watch {
            let watch_paths = paths.clone();
            let watch_shutdown = Arc::clone(&join_shutdown);
            let watch_state = Arc::clone(&join_state);
            let watch_request = SiteBuildRequest {
                profile: options.profile.clone(),
                output_dir: options.output_dir.clone(),
                clean: false,
                dry_run: false,
            };
            let watch_options = WatchOptions {
                debounce_ms: options.debounce_ms,
            };
            let watch_strict = options.strict;
            let watch_fail_on_warning = options.fail_on_warning;
            Some(thread::spawn(move || {
                let result = watch_vault_until(
                    &watch_paths,
                    &watch_options,
                    || watch_shutdown.load(Ordering::SeqCst),
                    |watch_report| {
                        if watch_report.startup {
                            return Ok::<_, std::convert::Infallible>(());
                        }
                        match build_site_with_policy(
                            &watch_paths,
                            &watch_request,
                            watch_strict,
                            watch_fail_on_warning,
                        ) {
                            Ok(report) => {
                                log_watch_rebuild_success(&report);
                                if let Ok(mut state) = watch_state.lock() {
                                    let should_bump = !report.changed_files.is_empty()
                                        || !report.deleted_files.is_empty()
                                        || state.report.diagnostics != report.diagnostics
                                        || state.last_error.is_some();
                                    state.output_dir = PathBuf::from(&report.output_dir);
                                    state.report = report;
                                    if should_bump {
                                        state.version = state.version.saturating_add(1);
                                    }
                                    state.last_error = None;
                                }
                            }
                            Err(error) => {
                                let error_message = error.to_string();
                                eprintln!("site serve rebuild failed: {error_message}");
                                if let Ok(mut state) = watch_state.lock() {
                                    if state.last_error.as_deref() != Some(error_message.as_str()) {
                                        state.version = state.version.saturating_add(1);
                                    }
                                    state.last_error = Some(error_message);
                                }
                            }
                        }
                        Ok::<_, std::convert::Infallible>(())
                    },
                );
                if let Err(error) = result {
                    let error_message = error.to_string();
                    if let Ok(mut state) = watch_state.lock() {
                        if state.last_error.as_deref() != Some(error_message.as_str()) {
                            state.version = state.version.saturating_add(1);
                        }
                        state.last_error = Some(error_message);
                    }
                }
            }))
        } else {
            None
        };

        let result = run_server_loop(&listener, &join_shutdown, &join_state);
        join_shutdown.store(true, Ordering::SeqCst);
        if let Some(watch_handle) = watch_handle {
            watch_handle
                .join()
                .map_err(|_| CliError::operation("site watch thread panicked"))?;
        }
        result
    });

    Ok(SiteServeHandle {
        addr,
        shutdown,
        join_handle: Some(join_handle),
    })
}

fn run_server_loop(
    listener: &TcpListener,
    shutdown: &Arc<AtomicBool>,
    state: &Arc<Mutex<SiteServeState>>,
) -> Result<(), CliError> {
    while !shutdown.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((mut stream, _)) => {
                let _ = stream.set_nonblocking(false);
                let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                let response = match read_request(&mut stream) {
                    Ok(request) => {
                        if request.method == "GET"
                            && is_live_reload_sse_request(state, &request.path)
                        {
                            let mut sse_stream = stream;
                            let sse_shutdown = Arc::clone(shutdown);
                            let sse_state = Arc::clone(state);
                            thread::spawn(move || {
                                let _ = serve_live_reload_sse(
                                    &mut sse_stream,
                                    &sse_shutdown,
                                    &sse_state,
                                );
                            });
                            continue;
                        }
                        route_request(state, &request)
                    }
                    Err(error) => response_text(400, "text/plain; charset=utf-8", error),
                };
                write_response(&mut stream, &response).map_err(CliError::operation)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(20));
            }
            Err(error) => return Err(CliError::operation(error)),
        }
    }

    Ok(())
}

fn route_request(state: &Arc<Mutex<SiteServeState>>, request: &Request) -> Response {
    if request.method != "GET" && request.method != "HEAD" {
        return response_text(405, "text/plain; charset=utf-8", "method not allowed");
    }

    let deploy_path = state
        .lock()
        .ok()
        .map(|state| state.report.deploy_path.clone())
        .unwrap_or_default();
    if is_live_reload_json_path(&request.path, &deploy_path) {
        let payload = state.lock().ok().map_or_else(
            || {
                json!({
                    "ok": false,
                    "error": "site serve state unavailable",
                })
            },
            |state| live_reload_payload(&state),
        );
        return response_json(200, &payload);
    }

    let Some((output_dir, body_path)) = state.lock().ok().and_then(|state| {
        resolve_site_path(&state.output_dir, &request.path, &state.report.deploy_path)
            .map(|body_path| (state.output_dir.clone(), body_path))
    }) else {
        return response_text(404, "text/plain; charset=utf-8", "not found");
    };

    let candidate = output_dir.join(body_path);
    match fs::read(&candidate) {
        Ok(body) => Response {
            status: 200,
            content_type: content_type_for_path(&candidate),
            body,
            cache_control: Some("no-store"),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            response_text(404, "text/plain; charset=utf-8", "not found")
        }
        Err(error) => response_text(
            500,
            "text/plain; charset=utf-8",
            format!("failed to read {}: {error}", candidate.display()),
        ),
    }
}

fn is_live_reload_json_path(request_path: &str, deploy_path: &str) -> bool {
    request_path == "/__vulcan_site/live-reload.json"
        || request_path == live_reload_json_path(deploy_path)
}

fn is_live_reload_sse_request(state: &Arc<Mutex<SiteServeState>>, request_path: &str) -> bool {
    let deploy_path = state
        .lock()
        .ok()
        .map(|state| state.report.deploy_path.clone())
        .unwrap_or_default();
    request_path == "/__vulcan_site/live-reload.events"
        || request_path == live_reload_sse_path(&deploy_path)
}

fn live_reload_json_path(deploy_path: &str) -> String {
    if deploy_path.is_empty() {
        "/__vulcan_site/live-reload.json".to_string()
    } else {
        format!("{deploy_path}/__vulcan_site/live-reload.json")
    }
}

fn live_reload_sse_path(deploy_path: &str) -> String {
    if deploy_path.is_empty() {
        "/__vulcan_site/live-reload.events".to_string()
    } else {
        format!("{deploy_path}/__vulcan_site/live-reload.events")
    }
}

fn live_reload_payload(state: &SiteServeState) -> serde_json::Value {
    let diagnostics = state
        .report
        .diagnostics
        .iter()
        .filter(|diagnostic| matches!(diagnostic.level.as_str(), "warn" | "error"))
        .collect::<Vec<_>>();
    json!({
        "ok": true,
        "version": state.version,
        "profile": state.report.profile,
        "note_count": state.report.note_count,
        "page_count": state.report.page_count,
        "asset_count": state.report.asset_count,
        "changed_files": state.report.changed_files,
        "deleted_files": state.report.deleted_files,
        "diagnostics": diagnostics,
        "last_error": state.last_error,
    })
}

fn serve_live_reload_sse(
    stream: &mut TcpStream,
    shutdown: &Arc<AtomicBool>,
    state: &Arc<Mutex<SiteServeState>>,
) -> Result<(), std::io::Error> {
    stream.write_all(
        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n",
    )?;
    stream.flush()?;

    let mut last_sent = String::new();
    while !shutdown.load(Ordering::SeqCst) {
        let payload = state.lock().ok().map_or_else(
            || json!({ "ok": false, "error": "site serve state unavailable" }),
            |state| live_reload_payload(&state),
        );
        let payload_json = serde_json::to_string(&payload).expect("SSE payload should serialize");
        if payload_json == last_sent {
            stream.write_all(b": keep-alive\r\n\r\n")?;
            stream.flush()?;
        } else {
            stream.write_all(b"event: update\r\n")?;
            stream.write_all(b"data: ")?;
            stream.write_all(payload_json.as_bytes())?;
            stream.write_all(b"\r\n\r\n")?;
            stream.flush()?;
            last_sent = payload_json;
        }
        thread::sleep(Duration::from_millis(350));
    }
    Ok(())
}

fn log_watch_rebuild_success(report: &SiteBuildReport) {
    let diagnostics = report
        .diagnostics
        .iter()
        .filter(|diagnostic| matches!(diagnostic.level.as_str(), "warn" | "error"))
        .collect::<Vec<_>>();
    if diagnostics.is_empty() {
        if !report.changed_files.is_empty() || !report.deleted_files.is_empty() {
            eprintln!(
                "site serve rebuilt `{}`: {} changed, {} deleted",
                report.profile,
                report.changed_files.len(),
                report.deleted_files.len()
            );
        }
        return;
    }
    let preview = diagnostics
        .iter()
        .take(3)
        .map(|diagnostic| match diagnostic.source_path.as_deref() {
            Some(path) => format!(
                "[{}] {} {} ({path})",
                diagnostic.level, diagnostic.kind, diagnostic.message
            ),
            None => format!(
                "[{}] {} {}",
                diagnostic.level, diagnostic.kind, diagnostic.message
            ),
        })
        .collect::<Vec<_>>()
        .join("; ");
    eprintln!(
        "site serve rebuilt `{}` with {} publish diagnostic(s): {}",
        report.profile,
        diagnostics.len(),
        preview
    );
}

fn resolve_site_path(output_dir: &Path, request_path: &str, deploy_path: &str) -> Option<PathBuf> {
    let relative_request = strip_deploy_path(request_path, deploy_path).unwrap_or(request_path);
    let normalized = if relative_request.is_empty() || relative_request == "/" {
        PathBuf::from("index.html")
    } else {
        let trimmed = relative_request.trim_start_matches('/');
        let decoded = percent_decode(trimmed);
        let mut relative = PathBuf::new();
        for component in Path::new(&decoded).components() {
            match component {
                Component::Normal(segment) => relative.push(segment),
                Component::CurDir => {}
                _ => return None,
            }
        }
        if relative_request.ends_with('/') {
            relative.push("index.html");
        }
        relative
    };

    let direct = output_dir.join(&normalized);
    if direct.is_file() {
        return Some(normalized);
    }

    if direct.is_dir() {
        let nested = normalized.join("index.html");
        if output_dir.join(&nested).is_file() {
            return Some(nested);
        }
    }

    if normalized.extension().is_none() {
        let nested = normalized.join("index.html");
        if output_dir.join(&nested).is_file() {
            return Some(nested);
        }
    }

    None
}

fn strip_deploy_path<'a>(request_path: &'a str, deploy_path: &str) -> Option<&'a str> {
    if deploy_path.is_empty() {
        return Some(request_path);
    }
    if request_path == deploy_path {
        return Some("/");
    }
    request_path.strip_prefix(deploy_path)
}

fn read_request(stream: &mut TcpStream) -> Result<Request, String> {
    let mut buffer = Vec::new();
    let mut header_end = None;

    loop {
        let mut chunk = [0_u8; 1024];
        let bytes_read = stream.read(&mut chunk).map_err(|error| error.to_string())?;
        if bytes_read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..bytes_read]);
        if let Some(position) = find_subslice(&buffer, b"\r\n\r\n") {
            header_end = Some(position + 4);
            break;
        }
        if buffer.len() > 32 * 1024 {
            return Err("request headers exceed 32 KiB".to_string());
        }
    }

    let header_end = header_end.ok_or_else(|| "incomplete HTTP request".to_string())?;
    let header_text = String::from_utf8(buffer[..header_end].to_vec())
        .map_err(|_| "request headers are not valid UTF-8".to_string())?;
    let mut lines = header_text.lines();
    let request_line = lines
        .next()
        .ok_or_else(|| "missing HTTP request line".to_string())?;
    let mut parts = request_line.split_whitespace();
    let method = parts
        .next()
        .ok_or_else(|| "missing HTTP method".to_string())?
        .to_string();
    let target = parts
        .next()
        .ok_or_else(|| "missing HTTP request target".to_string())?;
    let (path, _) = parse_target(target);

    Ok(Request { method, path })
}

fn parse_target(target: &str) -> (String, Vec<(String, String)>) {
    let (path, query) = target
        .split_once('?')
        .map_or((target, ""), |(path, query)| (path, query));
    let params = query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (key, value) = pair
                .split_once('=')
                .map_or((pair, ""), |(key, value)| (key, value));
            (percent_decode(key), percent_decode(value))
        })
        .collect::<Vec<_>>();
    (percent_decode(path), params)
}

fn percent_decode(value: &str) -> String {
    let mut decoded = Vec::with_capacity(value.len());
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => {
                decoded.push(b' ');
                index += 1;
            }
            b'%' if index + 2 < bytes.len() => {
                let hex = &value[index + 1..index + 3];
                if let Ok(byte) = u8::from_str_radix(hex, 16) {
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

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn response_json(status: u16, body: &serde_json::Value) -> Response {
    Response {
        status,
        content_type: "application/json",
        body: serde_json::to_vec(&body).expect("response JSON should serialize"),
        cache_control: Some("no-store"),
    }
}

fn response_text(status: u16, content_type: &'static str, body: impl Into<String>) -> Response {
    Response {
        status,
        content_type,
        body: body.into().into_bytes(),
        cache_control: Some("no-store"),
    }
}

fn write_response(stream: &mut TcpStream, response: &Response) -> Result<(), std::io::Error> {
    let status_text = match response.status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Internal Server Error",
    };
    let mut headers = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n",
        response.status,
        status_text,
        response.content_type,
        response.body.len()
    );
    if let Some(cache_control) = response.cache_control {
        headers.push_str("Cache-Control: ");
        headers.push_str(cache_control);
        headers.push_str("\r\n");
    }
    headers.push_str("\r\n");
    stream.write_all(headers.as_bytes())?;
    stream.write_all(&response.body)?;
    stream.flush()
}

fn content_type_for_path(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "html" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" => "application/javascript; charset=utf-8",
        "json" => "application/json",
        "xml" => "application/xml; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::fs;
    use tempfile::TempDir;
    use vulcan_core::{scan_vault, ScanMode};

    #[test]
    fn site_serve_serves_static_output_and_live_reload_state() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("basic", &vault_root);
        fs::write(
            vault_root.join(".vulcan/config.toml"),
            r#"[site.profiles.public]
title = "Public Notes"
base_url = "https://notes.example.com"
home = "Home"
output_dir = ".vulcan/site/public"
include_paths = ["Home.md", "Projects/Alpha.md"]
search = true
graph = true
rss = true
"#,
        )
        .expect("config should be written");
        scan_vault(&VaultPaths::new(&vault_root), ScanMode::Full).expect("scan should succeed");

        let handle = spawn_site_server(
            VaultPaths::new(&vault_root),
            SiteServeOptions {
                profile: Some("public".to_string()),
                output_dir: None,
                port: 0,
                watch: false,
                debounce_ms: 50,
                strict: false,
                fail_on_warning: false,
            },
        )
        .expect("site server should start");

        let index = get_text(handle.addr(), "/");
        let live = get_json(handle.addr(), "/__vulcan_site/live-reload.json");
        let search = get_json(handle.addr(), "/assets/search-index.json");

        assert!(index.contains("Public Notes"));
        assert!(index.contains("Built by Vulcan static site builder"));
        assert_eq!(live["ok"], true);
        assert_eq!(live["version"], 1);
        assert!(live["last_error"].is_null());
        assert!(search["entries"]
            .as_array()
            .is_some_and(|entries| !entries.is_empty()));

        handle.shutdown().expect("site server should shut down");
    }

    #[test]
    #[cfg_attr(
        target_os = "macos",
        ignore = "FSEvents does not reliably deliver events in CI"
    )]
    fn site_serve_watch_rebuilds_output_and_bumps_live_reload_version() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("basic", &vault_root);
        fs::write(
            vault_root.join(".vulcan/config.toml"),
            r#"[site.profiles.public]
title = "Public Notes"
home = "Home"
output_dir = ".vulcan/site/public"
include_paths = ["Home.md", "Projects/Alpha.md"]
search = true
graph = true
"#,
        )
        .expect("config should be written");
        scan_vault(&VaultPaths::new(&vault_root), ScanMode::Full).expect("scan should succeed");

        let handle = spawn_site_server(
            VaultPaths::new(&vault_root),
            SiteServeOptions {
                profile: Some("public".to_string()),
                output_dir: None,
                port: 0,
                watch: true,
                debounce_ms: 50,
                strict: false,
                fail_on_warning: false,
            },
        )
        .expect("site server should start");

        let initial_live = get_json(handle.addr(), "/__vulcan_site/live-reload.json");
        let initial_version = initial_live["version"]
            .as_u64()
            .expect("live version should be numeric");
        let before = get_text(handle.addr(), "/notes/home/");
        assert!(!before.contains("Moonshot preview"));

        fs::write(
            vault_root.join("Home.md"),
            "---\naliases:\n  - Start\n---\n\n# Home\n\nMoonshot preview lives here.\n",
        )
        .expect("updated note should be written");

        let mut reloaded_html = None;
        for _ in 0..120 {
            let live = get_json(handle.addr(), "/__vulcan_site/live-reload.json");
            if live["version"].as_u64().unwrap_or_default() > initial_version {
                let html = get_text(handle.addr(), "/notes/home/");
                if html.contains("Moonshot preview lives here.") {
                    reloaded_html = Some(html);
                    break;
                }
            }
            thread::sleep(Duration::from_millis(100));
        }

        assert!(
            reloaded_html.is_some(),
            "watch-backed site output should refresh"
        );
        handle.shutdown().expect("site server should shut down");
    }

    #[test]
    #[cfg_attr(
        target_os = "macos",
        ignore = "FSEvents does not reliably deliver events in CI"
    )]
    fn site_serve_watch_strict_keeps_last_good_output_on_publish_diagnostic() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should exist");
        fs::write(
            vault_root.join("Home.md"),
            "# Home

Baseline public page.
",
        )
        .expect("home note should write");
        fs::write(
            vault_root.join("Private.md"),
            "---
tags:
  - private
---

# Private

Hidden note.
",
        )
        .expect("private note should write");
        fs::write(
            vault_root.join(".vulcan/config.toml"),
            r#"[site.profiles.public]
title = "Public Notes"
home = "Home"
output_dir = ".vulcan/site/public"
include_paths = ["Home.md", "Private.md"]
exclude_tags = ["private"]
link_policy = "warn"
"#,
        )
        .expect("config should be written");
        scan_vault(&VaultPaths::new(&vault_root), ScanMode::Full).expect("scan should succeed");

        let handle = spawn_site_server(
            VaultPaths::new(&vault_root),
            SiteServeOptions {
                profile: Some("public".to_string()),
                output_dir: None,
                port: 0,
                watch: true,
                debounce_ms: 50,
                strict: true,
                fail_on_warning: false,
            },
        )
        .expect("site server should start");

        let initial_live = get_json(handle.addr(), "/__vulcan_site/live-reload.json");
        let initial_version = initial_live["version"]
            .as_u64()
            .expect("live version should be numeric");
        let before = get_text(handle.addr(), "/notes/home/");
        assert!(before.contains("Baseline public page."));

        fs::write(
            vault_root.join("Home.md"),
            "# Home

Strict preview should block this. See [[Private]].
",
        )
        .expect("updated note should be written");

        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        let mut observed_error = None;
        while std::time::Instant::now() < deadline {
            let live = get_json(handle.addr(), "/__vulcan_site/live-reload.json");
            if live["last_error"].as_str().is_some() {
                observed_error = Some(live);
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }

        let live = observed_error.expect("strict watch mode should report a publish diagnostic");
        assert!(
            live["version"].as_u64().unwrap_or_default() > initial_version,
            "live reload version should advance when strict-mode diagnostics change: initial={}, live={}",
            initial_version,
            live["version"].as_u64().unwrap_or_default()
        );
        assert!(live["last_error"]
            .as_str()
            .is_some_and(|message| message.contains("publish diagnostic")));
        let after = get_text(handle.addr(), "/notes/home/");
        assert!(after.contains("Baseline public page."));
        assert!(!after.contains("Strict preview should block this."));

        handle.shutdown().expect("site server should shut down");
    }

    #[test]
    fn site_serve_live_reload_payload_includes_publish_diagnostics() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should exist");
        fs::write(
            vault_root.join("Home.md"),
            "# Home\n\nThis page links to [[Private]].\n",
        )
        .expect("home note should write");
        fs::write(
            vault_root.join("Private.md"),
            "---\ntags:\n  - private\n---\n\n# Private\n",
        )
        .expect("private note should write");
        fs::write(
            vault_root.join(".vulcan/config.toml"),
            r#"[site.profiles.public]
title = "Public Notes"
home = "Home"
output_dir = ".vulcan/site/public"
include_paths = ["Home.md", "Private.md"]
exclude_tags = ["private"]
link_policy = "warn"
"#,
        )
        .expect("config should be written");
        scan_vault(&VaultPaths::new(&vault_root), ScanMode::Full).expect("scan should succeed");

        let handle = spawn_site_server(
            VaultPaths::new(&vault_root),
            SiteServeOptions {
                profile: Some("public".to_string()),
                output_dir: None,
                port: 0,
                watch: false,
                debounce_ms: 50,
                strict: false,
                fail_on_warning: false,
            },
        )
        .expect("site server should start");

        let live = get_json(handle.addr(), "/__vulcan_site/live-reload.json");
        let diagnostics = live["diagnostics"]
            .as_array()
            .expect("diagnostics should be an array");
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0]["kind"], "unpublished_link_target");
        assert!(live["changed_files"]
            .as_array()
            .is_some_and(|entries| !entries.is_empty()));

        handle.shutdown().expect("site server should shut down");
    }

    #[test]
    #[cfg_attr(
        target_os = "macos",
        ignore = "FSEvents does not reliably deliver events in CI"
    )]
    fn site_serve_watch_streams_sse_live_reload_updates() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("basic", &vault_root);
        fs::write(
            vault_root.join(".vulcan/config.toml"),
            r#"[site.profiles.public]
title = "Public Notes"
home = "Home"
output_dir = ".vulcan/site/public"
include_paths = ["Home.md", "Projects/Alpha.md"]
search = true
graph = true
"#,
        )
        .expect("config should be written");
        scan_vault(&VaultPaths::new(&vault_root), ScanMode::Full).expect("scan should succeed");

        let handle = spawn_site_server(
            VaultPaths::new(&vault_root),
            SiteServeOptions {
                profile: Some("public".to_string()),
                output_dir: None,
                port: 0,
                watch: true,
                debounce_ms: 50,
                strict: false,
                fail_on_warning: false,
            },
        )
        .expect("site server should start");

        let mut initial_stream =
            open_sse_stream(handle.addr(), "/__vulcan_site/live-reload.events");
        let initial = read_sse_event(&mut initial_stream);
        let initial_version = initial["version"]
            .as_u64()
            .expect("initial SSE version should be numeric");
        drop(initial_stream);

        fs::write(
            vault_root.join("Home.md"),
            "---\naliases:\n  - Start\n---\n\n# Home\n\nSSE preview lives here.\n",
        )
        .expect("updated note should be written");

        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        let mut updated_version = None;
        while std::time::Instant::now() < deadline {
            let live = get_json(handle.addr(), "/__vulcan_site/live-reload.json");
            if live["version"].as_u64().unwrap_or_default() > initial_version {
                updated_version = live["version"].as_u64();
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }

        let updated_version = updated_version.expect("watch build should advance the live version");
        let mut updated_stream =
            open_sse_stream(handle.addr(), "/__vulcan_site/live-reload.events");
        let event = read_sse_event(&mut updated_stream);
        assert_eq!(
            event["version"].as_u64().unwrap_or_default(),
            updated_version
        );
        assert!(event["changed_files"]
            .as_array()
            .is_some_and(|entries| entries.iter().any(|value| {
                value
                    .as_str()
                    .is_some_and(|path| path.contains("notes/home/index.html"))
            })));

        handle.shutdown().expect("site server should shut down");
    }

    #[test]
    fn site_serve_supports_prefixed_routes_and_live_reload_endpoints() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("basic", &vault_root);
        fs::write(
            vault_root.join(".vulcan/config.toml"),
            r#"[site.profiles.public]
title = "Public Notes"
base_url = "https://notes.example.com"
deploy_path = "/garden"
home = "Home"
output_dir = ".vulcan/site/public"
include_paths = ["Home.md", "Projects/Alpha.md"]
search = true
graph = true
"#,
        )
        .expect("config should be written");
        scan_vault(&VaultPaths::new(&vault_root), ScanMode::Full).expect("scan should succeed");

        let handle = spawn_site_server(
            VaultPaths::new(&vault_root),
            SiteServeOptions {
                profile: Some("public".to_string()),
                output_dir: None,
                port: 0,
                watch: false,
                debounce_ms: 50,
                strict: false,
                fail_on_warning: false,
            },
        )
        .expect("site server should start");

        let root_index = get_text(handle.addr(), "/");
        let prefixed_index = get_text(handle.addr(), "/garden/");
        let prefixed_note = get_text(handle.addr(), "/garden/notes/home/");
        let prefixed_live = get_json(handle.addr(), "/garden/__vulcan_site/live-reload.json");

        assert!(root_index.contains(r#"href="/garden/""#));
        assert!(prefixed_index.contains(r#"href="/garden/assets/vulcan-site.css""#));
        assert!(prefixed_note.contains("Home links to"));
        assert_eq!(prefixed_live["ok"], true);
        assert_eq!(prefixed_live["profile"], "public");

        handle.shutdown().expect("site server should shut down");
    }

    fn copy_fixture_vault(name: &str, destination: &std::path::Path) {
        let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../tests/fixtures/vaults")
            .join(name);
        copy_dir_recursive(&source, destination);
        fs::create_dir_all(destination.join(".vulcan")).expect(".vulcan dir should be created");
    }

    fn copy_dir_recursive(source: &std::path::Path, destination: &std::path::Path) {
        fs::create_dir_all(destination).expect("destination directory should be created");

        for entry in fs::read_dir(source).expect("source directory should be readable") {
            let entry = entry.expect("directory entry should be readable");
            let file_type = entry.file_type().expect("file type should be readable");
            let target = destination.join(entry.file_name());

            if file_type.is_dir() {
                copy_dir_recursive(&entry.path(), &target);
            } else if file_type.is_file() {
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent).expect("parent directory should exist");
                }
                fs::copy(entry.path(), target).expect("file should be copied");
            }
        }
    }

    fn get_text(addr: SocketAddr, path: &str) -> String {
        let mut stream = TcpStream::connect(addr).expect("server should accept connections");
        let request = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
        stream
            .write_all(request.as_bytes())
            .expect("request should write");
        let mut response = String::new();
        stream
            .read_to_string(&mut response)
            .expect("response should read");
        response
            .split("\r\n\r\n")
            .nth(1)
            .expect("response should contain body")
            .to_string()
    }

    fn get_json(addr: SocketAddr, path: &str) -> Value {
        let body = get_text(addr, path);
        serde_json::from_str(&body).expect("response body should parse as JSON")
    }

    fn open_sse_stream(addr: SocketAddr, path: &str) -> TcpStream {
        let mut stream = TcpStream::connect(addr).expect("server should accept connections");
        let request = format!(
            "GET {path} HTTP/1.1\r\nHost: {addr}\r\nAccept: text/event-stream\r\nConnection: close\r\n\r\n"
        );
        stream
            .write_all(request.as_bytes())
            .expect("SSE request should write");
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .expect("SSE read timeout should set");
        stream
    }

    fn read_sse_event(stream: &mut TcpStream) -> Value {
        let mut buffer = Vec::new();
        let mut headers_done = false;
        loop {
            let mut chunk = [0_u8; 1024];
            let bytes = stream
                .read(&mut chunk)
                .expect("SSE stream should be readable");
            assert!(bytes > 0, "SSE stream closed before an event arrived");
            buffer.extend_from_slice(&chunk[..bytes]);
            if !headers_done {
                if let Some(index) = find_subslice(&buffer, b"\r\n\r\n") {
                    buffer.drain(..index + 4);
                    headers_done = true;
                } else {
                    continue;
                }
            }
            if let Some(index) = find_subslice(&buffer, b"\r\n\r\n") {
                let frame = String::from_utf8_lossy(&buffer[..index]).to_string();
                buffer.drain(..index + 4);
                if frame.starts_with(':') {
                    continue;
                }
                if let Some(payload) = frame
                    .lines()
                    .find_map(|line| line.strip_prefix("data: ").map(ToOwned::to_owned))
                {
                    return serde_json::from_str(&payload).expect("SSE event payload should parse");
                }
            }
        }
    }
}
