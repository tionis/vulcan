use crate::CliError;
use serde_json::json;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use vulcan_app::serve::{
    route_request as route_serve_request, ServeHealthState, ServeRequest as AppServeRequest,
    ServeResponse as AppServeResponse, ServeRouteOptions,
};
use vulcan_core::{watch_vault_until, VaultPaths, WatchOptions};

#[derive(Debug, Clone)]
pub struct ServeOptions {
    pub bind: String,
    pub watch: bool,
    pub debounce_ms: u64,
    pub auth_token: Option<String>,
    pub permissions: Option<String>,
}

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug)]
pub struct ServeHandle {
    addr: SocketAddr,
    shutdown: Arc<AtomicBool>,
    join_handle: Option<thread::JoinHandle<Result<(), CliError>>>,
}

#[derive(Debug)]
struct Request {
    request: AppServeRequest,
    headers: HashMap<String, String>,
}

#[cfg(test)]
impl ServeHandle {
    #[must_use]
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn shutdown(mut self) -> Result<(), CliError> {
        self.shutdown.store(true, Ordering::SeqCst);
        if let Some(join_handle) = self.join_handle.take() {
            join_handle
                .join()
                .map_err(|_| CliError::operation("serve thread panicked"))??;
        }
        Ok(())
    }
}

pub fn serve_forever(paths: &VaultPaths, options: &ServeOptions) -> Result<(), CliError> {
    let mut handle = spawn_server(paths.clone(), options.clone())?;
    if let Some(join_handle) = handle.join_handle.take() {
        join_handle
            .join()
            .map_err(|_| CliError::operation("serve thread panicked"))??;
    }
    Ok(())
}

pub fn spawn_server(paths: VaultPaths, options: ServeOptions) -> Result<ServeHandle, CliError> {
    let bind_addr = parse_bind_addr(&options.bind, options.auth_token.is_some())?;
    let listener = TcpListener::bind(bind_addr).map_err(CliError::operation)?;
    listener
        .set_nonblocking(true)
        .map_err(CliError::operation)?;
    let addr = listener.local_addr().map_err(CliError::operation)?;
    let shutdown = Arc::new(AtomicBool::new(false));
    let state = Arc::new(Mutex::new(ServeHealthState::default()));
    let join_shutdown = Arc::clone(&shutdown);
    let join_state = Arc::clone(&state);

    let join_handle = thread::spawn(move || {
        let watch_handle = if options.watch {
            let watch_paths = paths.clone();
            let watch_shutdown = Arc::clone(&join_shutdown);
            let watch_state = Arc::clone(&join_state);
            let watch_options = WatchOptions {
                debounce_ms: options.debounce_ms,
            };
            Some(thread::spawn(move || {
                let result = watch_vault_until(
                    &watch_paths,
                    &watch_options,
                    || watch_shutdown.load(Ordering::SeqCst),
                    |report| {
                        if let Ok(mut state) = watch_state.lock() {
                            state.last_watch_report = Some(report);
                            state.watch_error = None;
                        }
                        Ok::<_, std::convert::Infallible>(())
                    },
                );
                if let Err(error) = result {
                    if let Ok(mut state) = watch_state.lock() {
                        state.watch_error = Some(error.to_string());
                    }
                }
            }))
        } else {
            None
        };

        let result = run_server_loop(&paths, &listener, &options, &join_shutdown, &join_state);
        join_shutdown.store(true, Ordering::SeqCst);
        if let Some(watch_handle) = watch_handle {
            watch_handle
                .join()
                .map_err(|_| CliError::operation("watch thread panicked"))?;
        }
        result
    });

    Ok(ServeHandle {
        addr,
        shutdown,
        join_handle: Some(join_handle),
    })
}

fn run_server_loop(
    paths: &VaultPaths,
    listener: &TcpListener,
    options: &ServeOptions,
    shutdown: &Arc<AtomicBool>,
    state: &Arc<Mutex<ServeHealthState>>,
) -> Result<(), CliError> {
    while !shutdown.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((mut stream, _)) => {
                let _ = stream.set_nonblocking(false);
                let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                let response = match read_request(&mut stream) {
                    Ok(request) => route_request(paths, options, state, &request),
                    Err(error) => response_error(400, error),
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

fn route_request(
    paths: &VaultPaths,
    options: &ServeOptions,
    state: &Arc<Mutex<ServeHealthState>>,
    request: &Request,
) -> AppServeResponse {
    if let Some(expected_token) = options.auth_token.as_deref() {
        let actual_token = request
            .headers
            .get("x-vulcan-token")
            .map(String::as_str)
            .unwrap_or_default();
        if actual_token != expected_token {
            return response_error(401, "missing or invalid X-Vulcan-Token header");
        }
    }

    let state = state
        .lock()
        .ok()
        .map(|state| state.clone())
        .unwrap_or_default();
    route_serve_request(
        paths,
        &ServeRouteOptions {
            permissions: options.permissions.clone(),
            watch_enabled: options.watch,
        },
        &state,
        &request.request,
    )
}

fn response_error(status: u16, message: impl Into<String>) -> AppServeResponse {
    AppServeResponse {
        status,
        body: json!({
            "ok": false,
            "error": message.into(),
        }),
    }
}

fn parse_bind_addr(bind: &str, allow_remote: bool) -> Result<SocketAddr, CliError> {
    let addr = SocketAddr::from_str(bind).map_err(|_| {
        CliError::operation("serve bind address must be a socket address like 127.0.0.1:3210")
    })?;
    if !addr.ip().is_loopback() && !allow_remote {
        return Err(CliError::operation(
            "non-loopback serve binds require --auth-token",
        ));
    }
    Ok(addr)
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
    let raw_target = parts
        .next()
        .ok_or_else(|| "missing HTTP request target".to_string())?;
    let (path, query) = parse_target(raw_target);
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_string()))
        .collect::<HashMap<_, _>>();

    Ok(Request {
        request: AppServeRequest {
            method,
            path,
            query,
        },
        headers,
    })
}

fn parse_target(target: &str) -> (String, HashMap<String, Vec<String>>) {
    let (path, query) = target
        .split_once('?')
        .map_or((target, ""), |(path, query)| (path, query));
    let mut params = HashMap::<String, Vec<String>>::new();
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let (key, value) = pair
            .split_once('=')
            .map_or((pair, ""), |(key, value)| (key, value));
        params
            .entry(percent_decode(key))
            .or_default()
            .push(percent_decode(value));
    }
    (path.to_string(), params)
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

fn write_response(
    stream: &mut TcpStream,
    response: &AppServeResponse,
) -> Result<(), std::io::Error> {
    let status_text = match response.status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Internal Server Error",
    };
    let body = serde_json::to_vec(&response.body).expect("response JSON should serialize");
    let headers = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        response.status,
        status_text,
        body.len()
    );
    stream.write_all(headers.as_bytes())?;
    stream.write_all(&body)?;
    stream.flush()
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::fs;
    use tempfile::TempDir;
    use vulcan_core::{scan_vault, CacheDatabase, ScanMode};

    #[test]
    fn serve_rejects_non_loopback_without_auth_token() {
        let error = parse_bind_addr("0.0.0.0:3210", false).expect_err("bind should be rejected");
        assert_eq!(
            error.to_string(),
            "non-loopback serve binds require --auth-token"
        );
    }

    #[test]
    fn serve_handles_repeated_queries_without_restarting() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("basic", &vault_root);
        scan_vault(&VaultPaths::new(&vault_root), ScanMode::Full).expect("scan should succeed");

        let handle = spawn_server(
            VaultPaths::new(&vault_root),
            ServeOptions {
                bind: "127.0.0.1:0".to_string(),
                watch: false,
                debounce_ms: 50,
                auth_token: None,
                permissions: None,
            },
        )
        .expect("server should start");
        let response = get_json(handle.addr(), "/search?q=dashboard&limit=1", None);
        let repeat_response = get_json(handle.addr(), "/graph/stats", None);

        assert_eq!(response["ok"], true);
        assert_eq!(response["result"]["hits"][0]["document_path"], "Home.md");
        assert_eq!(repeat_response["ok"], true);
        assert_eq!(repeat_response["result"]["note_count"], 3);

        handle.shutdown().expect("server should shut down");
    }

    #[test]
    fn serve_search_supports_sort_query_param() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        fs::create_dir_all(&vault_root).expect("vault root should exist");
        fs::create_dir_all(vault_root.join(".vulcan")).expect(".vulcan dir should be created");
        fs::write(vault_root.join("Alpha.md"), "dashboard").expect("alpha note should write");
        fs::write(vault_root.join("Beta.md"), "dashboard").expect("beta note should write");
        fs::write(vault_root.join("Gamma.md"), "dashboard").expect("gamma note should write");
        let paths = VaultPaths::new(&vault_root);
        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");

        let database = CacheDatabase::open(&paths).expect("database should open");
        database
            .connection()
            .execute(
                "UPDATE documents SET file_mtime = ? WHERE path = ?",
                (100_i64, "Alpha.md"),
            )
            .expect("alpha mtime should update");
        database
            .connection()
            .execute(
                "UPDATE documents SET file_mtime = ? WHERE path = ?",
                (300_i64, "Beta.md"),
            )
            .expect("beta mtime should update");
        database
            .connection()
            .execute(
                "UPDATE documents SET file_mtime = ? WHERE path = ?",
                (200_i64, "Gamma.md"),
            )
            .expect("gamma mtime should update");

        let handle = spawn_server(
            paths,
            ServeOptions {
                bind: "127.0.0.1:0".to_string(),
                watch: false,
                debounce_ms: 50,
                auth_token: None,
                permissions: None,
            },
        )
        .expect("server should start");

        let response = get_json(
            handle.addr(),
            "/search?q=dashboard&sort=modified-newest",
            None,
        );
        let hits = response["result"]["hits"]
            .as_array()
            .expect("hits should be an array");
        let ordered_paths = hits
            .iter()
            .map(|hit| {
                hit["document_path"]
                    .as_str()
                    .expect("document path should be a string")
                    .to_string()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            ordered_paths,
            vec![
                "Beta.md".to_string(),
                "Gamma.md".to_string(),
                "Alpha.md".to_string(),
            ]
        );

        handle.shutdown().expect("server should shut down");
    }

    #[test]
    fn serve_search_supports_match_case_and_matched_line() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        fs::create_dir_all(&vault_root).expect("vault root should exist");
        fs::create_dir_all(vault_root.join(".vulcan")).expect(".vulcan dir should be created");
        fs::write(vault_root.join("Upper.md"), "Bob builds dashboards.")
            .expect("upper note should write");
        fs::write(vault_root.join("Lower.md"), "bob builds dashboards.")
            .expect("lower note should write");
        scan_vault(&VaultPaths::new(&vault_root), ScanMode::Full).expect("scan should succeed");

        let handle = spawn_server(
            VaultPaths::new(&vault_root),
            ServeOptions {
                bind: "127.0.0.1:0".to_string(),
                watch: false,
                debounce_ms: 50,
                auth_token: None,
                permissions: None,
            },
        )
        .expect("server should start");

        let response = get_json(handle.addr(), "/search?q=Bob&match_case=true", None);
        let hits = response["result"]["hits"]
            .as_array()
            .expect("hits should be an array");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["document_path"], "Upper.md");
        assert_eq!(hits[0]["matched_line"], 1);

        handle.shutdown().expect("server should shut down");
    }

    #[test]
    fn serve_exposes_dataview_endpoints() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("dataview", &vault_root);
        scan_vault(&VaultPaths::new(&vault_root), ScanMode::Full).expect("scan should succeed");

        let handle = spawn_server(
            VaultPaths::new(&vault_root),
            ServeOptions {
                bind: "127.0.0.1:0".to_string(),
                watch: false,
                debounce_ms: 50,
                auth_token: None,
                permissions: None,
            },
        )
        .expect("server should start");

        let inline = get_json(handle.addr(), "/dataview/inline?file=Dashboard", None);
        assert_eq!(inline["ok"], true);
        assert_eq!(inline["result"]["file"], "Dashboard.md");
        assert_eq!(inline["result"]["results"][0]["value"], "draft");

        let query = get_json(
            handle.addr(),
            "/dataview/query?dql=TABLE%20status%20FROM%20%22Projects%22%20SORT%20file.name%20ASC",
            None,
        );
        assert_eq!(query["ok"], true);
        assert_eq!(query["result"]["query_type"], "table");
        assert_eq!(query["result"]["result_count"], 2);

        let query_js = get_json(
            handle.addr(),
            "/dataview/query-js?js=dv.current%28%29.status&file=Dashboard",
            None,
        );
        if cfg!(feature = "js_runtime") {
            assert_eq!(query_js["ok"], true);
            assert_eq!(query_js["result"]["value"], "draft");
        } else {
            assert_eq!(query_js["ok"], false);
            assert!(query_js["error"]
                .as_str()
                .is_some_and(|error| error.contains("js_runtime")));
        }

        let eval = get_json(handle.addr(), "/dataview/eval?file=Dashboard", None);
        assert_eq!(eval["ok"], true);
        assert_eq!(eval["result"]["blocks"].as_array().map(Vec::len), Some(2));
        if cfg!(feature = "js_runtime") {
            assert_eq!(eval["result"]["blocks"][1]["result"]["engine"], "js");
            assert_eq!(
                eval["result"]["blocks"][1]["result"]["data"]["outputs"][0]["rows"],
                serde_json::json!([["draft"]])
            );
        } else {
            assert_eq!(
                eval["result"]["blocks"][1]["result"],
                serde_json::Value::Null
            );
            assert!(eval["result"]["blocks"][1]["error"]
                .as_str()
                .is_some_and(|error| error.contains("js_runtime")));
        }

        handle.shutdown().expect("server should shut down");
    }

    #[test]
    #[cfg_attr(
        target_os = "macos",
        ignore = "FSEvents does not reliably deliver events in CI"
    )]
    fn serve_watch_refreshes_search_results() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("basic", &vault_root);
        scan_vault(&VaultPaths::new(&vault_root), ScanMode::Full).expect("scan should succeed");

        let handle = spawn_server(
            VaultPaths::new(&vault_root),
            ServeOptions {
                bind: "127.0.0.1:0".to_string(),
                watch: true,
                debounce_ms: 50,
                auth_token: None,
                permissions: None,
            },
        )
        .expect("server should start");

        for _ in 0..50 {
            let health = get_json(handle.addr(), "/health", None);
            if health["last_watch_report"].is_object() {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }

        let before = get_json(handle.addr(), "/search?q=moonshot", None);
        assert!(before["result"]["hits"]
            .as_array()
            .expect("hits should be an array")
            .is_empty());

        fs::write(
            vault_root.join("Home.md"),
            "---\naliases:\n  - Start\ntags:\n  - dashboard\n---\n\n# Home\n\nMoonshot plans live here.\n",
        )
        .expect("updated note should be written");

        let mut refreshed = None;
        for _ in 0..100 {
            if let Some(candidate) = try_get_json(handle.addr(), "/search?q=moonshot", None) {
                let hits = candidate["result"]["hits"]
                    .as_array()
                    .expect("hits should be an array");
                if !hits.is_empty() {
                    refreshed = Some(candidate);
                    break;
                }
            }
            thread::sleep(Duration::from_millis(100));
        }

        let refreshed = refreshed.expect("watch-backed search should refresh");
        assert_eq!(refreshed["result"]["hits"][0]["document_path"], "Home.md");

        handle.shutdown().expect("server should shut down");
    }

    #[test]
    fn serve_honors_auth_token() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("basic", &vault_root);
        scan_vault(&VaultPaths::new(&vault_root), ScanMode::Full).expect("scan should succeed");

        let handle = spawn_server(
            VaultPaths::new(&vault_root),
            ServeOptions {
                bind: "127.0.0.1:0".to_string(),
                watch: false,
                debounce_ms: 50,
                auth_token: Some("secret".to_string()),
                permissions: None,
            },
        )
        .expect("server should start");

        let unauthorized = get_json(handle.addr(), "/health", None);
        let authorized = get_json(handle.addr(), "/health", Some("secret"));

        assert_eq!(unauthorized["ok"], false);
        assert_eq!(authorized["ok"], true);

        handle.shutdown().expect("server should shut down");
    }

    #[test]
    fn serve_applies_permission_filters_and_denies_js_execution() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("dataview", &vault_root);
        fs::create_dir_all(vault_root.join(".vulcan")).expect("config dir should exist");
        fs::write(
            vault_root.join(".vulcan/config.toml"),
            r#"[permissions.profiles.projects_only]
read = { allow = ["folder:Projects/**"] }
write = "none"
refactor = "none"
git = "deny"
network = "deny"
index = "deny"
config = "none"
execute = "deny"
shell = "deny"
"#,
        )
        .expect("config should be written");
        scan_vault(&VaultPaths::new(&vault_root), ScanMode::Full).expect("scan should succeed");

        let handle = spawn_server(
            VaultPaths::new(&vault_root),
            ServeOptions {
                bind: "127.0.0.1:0".to_string(),
                watch: false,
                debounce_ms: 50,
                auth_token: None,
                permissions: Some("projects_only".to_string()),
            },
        )
        .expect("server should start");

        let notes = get_json(handle.addr(), "/notes", None);
        let paths = notes["result"]["notes"]
            .as_array()
            .expect("notes should be an array")
            .iter()
            .map(|note| {
                note["document_path"]
                    .as_str()
                    .expect("document path should be a string")
                    .to_string()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            paths,
            vec![
                "Projects/Alpha.md".to_string(),
                "Projects/Beta.md".to_string()
            ]
        );

        let search = get_json(handle.addr(), "/search?q=draft", None);
        assert_eq!(
            search["result"]["hits"]
                .as_array()
                .expect("hits should be an array")
                .len(),
            0
        );

        let inline = get_json(handle.addr(), "/dataview/inline?file=Dashboard", None);
        assert_eq!(inline["ok"], false);
        assert!(inline["error"]
            .as_str()
            .is_some_and(|error| error.contains("does not allow read `Dashboard.md`")));

        let query_js = get_json(
            handle.addr(),
            "/dataview/query-js?js=dv.current%28%29.status&file=Projects/Alpha",
            None,
        );
        assert_eq!(query_js["ok"], false);
        assert!(query_js["error"]
            .as_str()
            .is_some_and(|error| error.contains("does not allow execute access")));

        handle.shutdown().expect("server should shut down");
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

    fn try_get_json(addr: SocketAddr, path: &str, token: Option<&str>) -> Option<Value> {
        let mut stream = TcpStream::connect(addr).ok()?;
        let mut request = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n");
        if let Some(token) = token {
            request.push_str("X-Vulcan-Token: ");
            request.push_str(token);
            request.push_str("\r\n");
        }
        request.push_str("\r\n");
        stream.write_all(request.as_bytes()).ok()?;
        let mut response = String::new();
        stream.read_to_string(&mut response).ok()?;
        let body = response.split("\r\n\r\n").nth(1)?;
        serde_json::from_str(body).ok()
    }

    fn get_json(addr: SocketAddr, path: &str, token: Option<&str>) -> Value {
        let mut stream = TcpStream::connect(addr).expect("server should accept connections");
        let mut request = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n");
        if let Some(token) = token {
            request.push_str("X-Vulcan-Token: ");
            request.push_str(token);
            request.push_str("\r\n");
        }
        request.push_str("\r\n");
        stream
            .write_all(request.as_bytes())
            .expect("request should write");
        let mut response = String::new();
        stream
            .read_to_string(&mut response)
            .expect("response should read");
        let body = response
            .split("\r\n\r\n")
            .nth(1)
            .expect("response should contain a body");
        serde_json::from_str(body).expect("response body should parse")
    }
}
