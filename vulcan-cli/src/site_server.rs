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
                                if let Ok(mut state) = watch_state.lock() {
                                    state.output_dir = PathBuf::from(&report.output_dir);
                                    state.report = report;
                                    state.version = state.version.saturating_add(1);
                                    state.last_error = None;
                                }
                            }
                            Err(error) => {
                                if let Ok(mut state) = watch_state.lock() {
                                    state.last_error = Some(error.to_string());
                                }
                            }
                        }
                        Ok::<_, std::convert::Infallible>(())
                    },
                );
                if let Err(error) = result {
                    if let Ok(mut state) = watch_state.lock() {
                        state.last_error = Some(error.to_string());
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
                    Ok(request) => route_request(state, &request),
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

    if request.path == "/__vulcan_site/live-reload.json" {
        let payload = state.lock().ok().map_or_else(
            || {
                json!({
                    "ok": false,
                    "error": "site serve state unavailable",
                })
            },
            |state| {
                json!({
                    "ok": true,
                    "version": state.version,
                    "profile": state.report.profile,
                    "note_count": state.report.note_count,
                    "page_count": state.report.page_count,
                    "asset_count": state.report.asset_count,
                    "last_error": state.last_error,
                })
            },
        );
        return response_json(200, &payload);
    }

    let Some((output_dir, body_path)) = state.lock().ok().and_then(|state| {
        resolve_site_path(&state.output_dir, &request.path)
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

fn resolve_site_path(output_dir: &Path, request_path: &str) -> Option<PathBuf> {
    let normalized = if request_path.is_empty() || request_path == "/" {
        PathBuf::from("index.html")
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
        if request_path.ends_with('/') {
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
        assert_eq!(
            live["version"].as_u64().unwrap_or_default(),
            initial_version
        );
        assert!(live["last_error"]
            .as_str()
            .is_some_and(|message| message.contains("publish diagnostic")));
        let after = get_text(handle.addr(), "/notes/home/");
        assert!(after.contains("Baseline public page."));
        assert!(!after.contains("Strict preview should block this."));

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
}
