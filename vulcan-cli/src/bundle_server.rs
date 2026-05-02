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
use vulcan_app::site::{
    build_frontend_bundle as app_build_frontend_bundle, FrontendBundleBuildReport,
    FrontendBundleRequest,
};
use vulcan_core::{watch_vault_until, VaultPaths, WatchOptions};

#[derive(Debug, Clone)]
pub struct FrontendBundleServeOptions {
    pub export_profile_name: String,
    pub site_profile_name: String,
    pub output_dir: PathBuf,
    pub port: u16,
    pub debounce_ms: u64,
    pub pretty: bool,
}

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug)]
pub struct FrontendBundleServeHandle {
    addr: SocketAddr,
    shutdown: Arc<AtomicBool>,
    join_handle: Option<thread::JoinHandle<Result<(), CliError>>>,
}

#[cfg(test)]
impl FrontendBundleServeHandle {
    #[must_use]
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn shutdown(mut self) -> Result<(), CliError> {
        self.shutdown.store(true, Ordering::SeqCst);
        if let Some(join_handle) = self.join_handle.take() {
            join_handle
                .join()
                .map_err(|_| CliError::operation("bundle serve thread panicked"))??;
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
struct FrontendBundleServeState {
    output_dir: PathBuf,
    report: FrontendBundleBuildReport,
    version: u64,
    last_error: Option<String>,
    export_profile_name: String,
    site_profile_name: String,
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

pub fn serve_frontend_bundle_profile(
    paths: &VaultPaths,
    options: &FrontendBundleServeOptions,
) -> Result<(), CliError> {
    let mut handle = spawn_frontend_bundle_server(paths.clone(), options.clone())?;
    if let Some(join_handle) = handle.join_handle.take() {
        join_handle
            .join()
            .map_err(|_| CliError::operation("bundle serve thread panicked"))??;
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
pub fn spawn_frontend_bundle_server(
    paths: VaultPaths,
    options: FrontendBundleServeOptions,
) -> Result<FrontendBundleServeHandle, CliError> {
    let initial_report = app_build_frontend_bundle(
        &paths,
        &FrontendBundleRequest {
            profile: Some(options.site_profile_name.clone()),
            output_dir: options.output_dir.clone(),
            clean: false,
            dry_run: false,
            pretty: options.pretty,
        },
    )
    .map_err(CliError::operation)?;

    let listener = TcpListener::bind(("127.0.0.1", options.port)).map_err(CliError::operation)?;
    listener
        .set_nonblocking(true)
        .map_err(CliError::operation)?;
    let addr = listener.local_addr().map_err(CliError::operation)?;

    let shutdown = Arc::new(AtomicBool::new(false));
    let state = Arc::new(Mutex::new(FrontendBundleServeState {
        output_dir: PathBuf::from(&initial_report.output_dir),
        report: initial_report,
        version: 1,
        last_error: None,
        export_profile_name: options.export_profile_name.clone(),
        site_profile_name: options.site_profile_name.clone(),
    }));
    let join_shutdown = Arc::clone(&shutdown);
    let join_state = Arc::clone(&state);

    let join_handle = thread::spawn(move || {
        let watch_paths = paths.clone();
        let watch_shutdown = Arc::clone(&join_shutdown);
        let watch_state = Arc::clone(&join_state);
        let watch_options = WatchOptions {
            debounce_ms: options.debounce_ms,
        };
        let watch_request = FrontendBundleRequest {
            profile: Some(options.site_profile_name.clone()),
            output_dir: options.output_dir.clone(),
            clean: false,
            dry_run: false,
            pretty: options.pretty,
        };
        let watch_handle = thread::spawn(move || {
            let result = watch_vault_until(
                &watch_paths,
                &watch_options,
                || watch_shutdown.load(Ordering::SeqCst),
                |watch_report| {
                    if watch_report.startup {
                        return Ok::<_, std::convert::Infallible>(());
                    }
                    match app_build_frontend_bundle(&watch_paths, &watch_request) {
                        Ok(report) => {
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
        });

        let result = run_server_loop(&listener, &join_shutdown, &join_state);
        join_shutdown.store(true, Ordering::SeqCst);
        watch_handle
            .join()
            .map_err(|_| CliError::operation("bundle watch thread panicked"))?;
        result
    });

    Ok(FrontendBundleServeHandle {
        addr,
        shutdown,
        join_handle: Some(join_handle),
    })
}

fn run_server_loop(
    listener: &TcpListener,
    shutdown: &Arc<AtomicBool>,
    state: &Arc<Mutex<FrontendBundleServeState>>,
) -> Result<(), CliError> {
    while !shutdown.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((mut stream, _)) => {
                let _ = stream.set_nonblocking(false);
                let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                let response = match read_request(&mut stream) {
                    Ok(request) => {
                        if request.method == "GET" && is_live_reload_sse_path(&request.path) {
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

fn route_request(state: &Arc<Mutex<FrontendBundleServeState>>, request: &Request) -> Response {
    if request.method != "GET" && request.method != "HEAD" {
        return response_text(405, "text/plain; charset=utf-8", "method not allowed");
    }
    if is_live_reload_json_path(&request.path) {
        let payload = state.lock().ok().map_or_else(
            || json!({ "ok": false, "error": "bundle serve state unavailable" }),
            |state| live_reload_payload(&state),
        );
        return response_json(200, &payload);
    }

    let Some((output_dir, body_path)) = state.lock().ok().and_then(|state| {
        resolve_bundle_path(&state.output_dir, &request.path)
            .map(|path| (state.output_dir.clone(), path))
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

fn is_live_reload_json_path(path: &str) -> bool {
    path == "/__vulcan_bundle/live-reload.json"
}

fn is_live_reload_sse_path(path: &str) -> bool {
    path == "/__vulcan_bundle/live-reload.events"
}

fn live_reload_payload(state: &FrontendBundleServeState) -> serde_json::Value {
    json!({
        "ok": true,
        "version": state.version,
        "export_profile": state.export_profile_name,
        "site_profile": state.site_profile_name,
        "output_dir": state.report.output_dir,
        "note_count": state.report.note_count,
        "asset_count": state.report.asset_count,
        "changed_files": state.report.changed_files,
        "deleted_files": state.report.deleted_files,
        "changed_routes": state.report.invalidation.changed_routes,
        "deleted_routes": state.report.invalidation.deleted_routes,
        "changed_assets": state.report.invalidation.changed_assets,
        "deleted_assets": state.report.invalidation.deleted_assets,
        "diagnostics": state.report.diagnostics,
        "last_error": state.last_error,
    })
}

fn serve_live_reload_sse(
    stream: &mut TcpStream,
    shutdown: &Arc<AtomicBool>,
    state: &Arc<Mutex<FrontendBundleServeState>>,
) -> Result<(), std::io::Error> {
    stream.write_all(
        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n",
    )?;
    stream.flush()?;

    let mut last_sent = String::new();
    while !shutdown.load(Ordering::SeqCst) {
        let payload = state.lock().ok().map_or_else(
            || json!({ "ok": false, "error": "bundle serve state unavailable" }),
            |state| live_reload_payload(&state),
        );
        let payload_json = serde_json::to_string(&payload).expect("SSE payload should serialize");
        if payload_json == last_sent {
            stream.write_all(b": keep-alive\r\n\r\n")?;
        } else {
            stream.write_all(b"event: update\r\n")?;
            stream.write_all(b"data: ")?;
            stream.write_all(payload_json.as_bytes())?;
            stream.write_all(b"\r\n\r\n")?;
            last_sent = payload_json;
        }
        stream.flush()?;
        thread::sleep(Duration::from_millis(350));
    }
    Ok(())
}

fn resolve_bundle_path(output_dir: &Path, request_path: &str) -> Option<PathBuf> {
    let normalized = if request_path == "/" || request_path.is_empty() {
        PathBuf::from("frontend-bundle.json")
    } else {
        let trimmed = request_path.trim_start_matches('/');
        let decoded = percent_decode(trimmed);
        let mut relative = PathBuf::new();
        for component in Path::new(&decoded).components() {
            match component {
                Component::Normal(segment) => relative.push(segment),
                Component::CurDir => {}
                _ => return None,
            }
        }
        relative
    };
    let candidate = output_dir.join(&normalized);
    candidate.is_file().then_some(normalized)
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
    Ok(Request {
        method,
        path: percent_decode(target.split('?').next().unwrap_or(target)),
    })
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
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

fn response_json(status: u16, body: &serde_json::Value) -> Response {
    Response {
        status,
        content_type: "application/json",
        body: serde_json::to_vec(body).expect("response JSON should serialize"),
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
        "json" => "application/json",
        "ts" | "md" | "txt" => "text/plain; charset=utf-8",
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
    fn bundle_serve_serves_contract_and_live_reload_state() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("basic", &vault_root);
        fs::write(
            vault_root.join(".vulcan/config.toml"),
            r#"[site.profiles.public]
title = "Public Notes"
home = "Home"
include_paths = ["Home.md", "Projects/Alpha.md"]
search = true
graph = true
"#,
        )
        .expect("config should be written");
        scan_vault(&VaultPaths::new(&vault_root), ScanMode::Full).expect("scan should succeed");

        let handle = spawn_frontend_bundle_server(
            VaultPaths::new(&vault_root),
            FrontendBundleServeOptions {
                export_profile_name: "public_bundle".to_string(),
                site_profile_name: "public".to_string(),
                output_dir: vault_root.join("exports/public-bundle"),
                port: 0,
                debounce_ms: 50,
                pretty: true,
            },
        )
        .expect("bundle server should start");

        let contract = get_json(handle.addr(), "/frontend-bundle.json");
        let live = get_json(handle.addr(), "/__vulcan_bundle/live-reload.json");
        let note = get_json(handle.addr(), "/notes/home/index.json");

        assert_eq!(contract["contract"]["name"], "vulcan_frontend_bundle");
        assert_eq!(live["ok"], true);
        assert_eq!(live["version"], 1);
        assert!(note["body_html"]
            .as_str()
            .is_some_and(|html| html.contains("Home")));

        handle.shutdown().expect("bundle server should shut down");
    }

    #[test]
    #[cfg_attr(
        target_os = "macos",
        ignore = "FSEvents does not reliably deliver events in CI"
    )]
    fn bundle_serve_watch_rebuilds_output_and_bumps_live_reload_version() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("basic", &vault_root);
        fs::write(
            vault_root.join(".vulcan/config.toml"),
            r#"[site.profiles.public]
title = "Public Notes"
home = "Home"
include_paths = ["Home.md", "Projects/Alpha.md"]
search = true
graph = true
"#,
        )
        .expect("config should be written");
        scan_vault(&VaultPaths::new(&vault_root), ScanMode::Full).expect("scan should succeed");

        let handle = spawn_frontend_bundle_server(
            VaultPaths::new(&vault_root),
            FrontendBundleServeOptions {
                export_profile_name: "public_bundle".to_string(),
                site_profile_name: "public".to_string(),
                output_dir: vault_root.join("exports/public-bundle"),
                port: 0,
                debounce_ms: 50,
                pretty: true,
            },
        )
        .expect("bundle server should start");

        thread::sleep(Duration::from_millis(300));
        let initial_live = get_json(handle.addr(), "/__vulcan_bundle/live-reload.json");
        let initial_version = initial_live["version"]
            .as_u64()
            .expect("live version should be numeric");

        fs::write(
            vault_root.join("Home.md"),
            "---\naliases:\n  - Start\n---\n\n# Home\n\nBundle preview lives here.\n",
        )
        .expect("updated note should be written");

        let deadline = std::time::Instant::now() + Duration::from_secs(45);
        let mut updated = None;
        while std::time::Instant::now() < deadline {
            let live = get_json(handle.addr(), "/__vulcan_bundle/live-reload.json");
            if live["version"].as_u64().unwrap_or_default() > initial_version {
                let note = get_json(handle.addr(), "/notes/home/index.json");
                if note["body_html"]
                    .as_str()
                    .is_some_and(|html| html.contains("Bundle preview lives here."))
                {
                    updated = Some(live);
                    break;
                }
            }
            thread::sleep(Duration::from_millis(100));
        }

        assert!(
            updated.is_some(),
            "bundle output should refresh under watch"
        );
        handle.shutdown().expect("bundle server should shut down");
    }

    fn copy_fixture_vault(name: &str, destination: &Path) {
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../tests/fixtures/vaults")
            .join(name);
        copy_dir_recursive(&source, destination);
    }

    fn copy_dir_recursive(source: &Path, destination: &Path) {
        fs::create_dir_all(destination).expect("destination directory should be created");
        for entry in fs::read_dir(source).expect("source directory should be readable") {
            let entry = entry.expect("directory entry should be readable");
            let source_path = entry.path();
            let destination_path = destination.join(entry.file_name());
            if entry
                .file_type()
                .expect("file type should be readable")
                .is_dir()
            {
                copy_dir_recursive(&source_path, &destination_path);
            } else {
                fs::copy(&source_path, &destination_path)
                    .expect("fixture file should copy successfully");
            }
        }
    }

    fn get_json(addr: SocketAddr, path: &str) -> Value {
        let raw = get_text(addr, path);
        serde_json::from_str(&raw).expect("response should be valid JSON")
    }

    fn get_text(addr: SocketAddr, path: &str) -> String {
        let mut stream = TcpStream::connect(addr).expect("TCP connection should succeed");
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
        )
        .expect("request should write");
        stream.flush().expect("request should flush");
        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .expect("response should read");
        let text = String::from_utf8(response).expect("response should be valid UTF-8");
        text.split("\r\n\r\n")
            .nth(1)
            .unwrap_or_default()
            .to_string()
    }
}
