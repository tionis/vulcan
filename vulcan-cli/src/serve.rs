use crate::{
    run_dataview_eval_command, run_dataview_inline_command, run_dataview_query_command,
    run_dataview_query_js_command, CliError,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use vulcan_core::{
    query_graph_analytics_with_filter, query_notes_with_filter, query_related_notes_with_filter,
    resolve_permission_profile, search_vault_with_filter, watch_vault_until, NoteQuery,
    PermissionFilter, PermissionGuard, ProfilePermissionGuard, RelatedNotesQuery, SearchQuery,
    SearchSort, VaultPaths, WatchOptions, WatchReport,
};

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

#[derive(Debug, Default, Clone)]
struct ServeState {
    watch_error: Option<String>,
    last_watch_report: Option<WatchReport>,
}

#[derive(Debug)]
struct Request {
    method: String,
    path: String,
    query: HashMap<String, Vec<String>>,
    headers: HashMap<String, String>,
}

#[derive(Debug)]
struct Response {
    status: u16,
    body: Value,
}

impl Response {
    fn ok(body: Value) -> Self {
        Self { status: 200, body }
    }

    fn error(status: u16, message: impl Into<String>) -> Self {
        Self {
            status,
            body: json!({
                "ok": false,
                "error": message.into(),
            }),
        }
    }
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
    let state = Arc::new(Mutex::new(ServeState::default()));
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
    state: &Arc<Mutex<ServeState>>,
) -> Result<(), CliError> {
    while !shutdown.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((mut stream, _)) => {
                // Accepted streams may inherit the listener's non-blocking mode
                // on some platforms (macOS). Switch to blocking with a timeout so
                // that read_request can reliably receive the full request.
                let _ = stream.set_nonblocking(false);
                let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                let response = match read_request(&mut stream) {
                    Ok(request) => route_request(paths, options, state, &request),
                    Err(error) => Response::error(400, error),
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

#[allow(clippy::too_many_lines)]
fn route_request(
    paths: &VaultPaths,
    options: &ServeOptions,
    state: &Arc<Mutex<ServeState>>,
    request: &Request,
) -> Response {
    if request.method != "GET" {
        return Response::error(405, "only GET requests are supported");
    }
    if let Some(expected_token) = options.auth_token.as_deref() {
        let actual_token = request
            .headers
            .get("x-vulcan-token")
            .map(String::as_str)
            .unwrap_or_default();
        if actual_token != expected_token {
            return Response::error(401, "missing or invalid X-Vulcan-Token header");
        }
    }
    let permissions = match serve_permission_guard(paths, options) {
        Ok(permissions) => permissions,
        Err(error) => return Response::error(500, error),
    };
    let read_filter = match serve_read_filter(paths, options) {
        Ok(filter) => filter,
        Err(error) => return Response::error(500, error),
    };

    match request.path.as_str() {
        "/" => Response::ok(json!({
            "ok": true,
            "service": "vulcan",
            "endpoints": [
                "/health",
                "/search",
                "/notes",
                "/graph/stats",
                "/related",
                "/dataview/inline",
                "/dataview/query",
                "/dataview/query-js",
                "/dataview/eval"
            ],
        })),
        "/health" => {
            let state = state
                .lock()
                .ok()
                .map(|state| state.clone())
                .unwrap_or_default();
            Response::ok(json!({
                "ok": true,
                "watch_enabled": options.watch,
                "watch_error": state.watch_error,
                "last_watch_report": state.last_watch_report,
            }))
        }
        "/search" => {
            let Some(query) = first_param(&request.query, "q") else {
                return Response::error(400, "missing required query parameter: q");
            };
            let sort = match parse_optional_search_sort(&request.query, "sort") {
                Ok(sort) => sort,
                Err(error) => return Response::error(400, error),
            };
            let search_query = SearchQuery {
                text: query.to_string(),
                tag: first_param(&request.query, "tag").map(ToOwned::to_owned),
                path_prefix: first_param(&request.query, "path_prefix").map(ToOwned::to_owned),
                has_property: first_param(&request.query, "has_property").map(ToOwned::to_owned),
                filters: request.query.get("where").cloned().unwrap_or_default(),
                provider: first_param(&request.query, "provider").map(ToOwned::to_owned),
                mode: match first_param(&request.query, "mode") {
                    Some("hybrid") => vulcan_core::search::SearchMode::Hybrid,
                    _ => vulcan_core::search::SearchMode::Keyword,
                },
                sort,
                match_case: parse_optional_bool(&request.query, "match_case"),
                limit: parse_optional_usize(&request.query, "limit"),
                context_size: parse_optional_usize(&request.query, "context_size").unwrap_or(18),
                raw_query: parse_optional_bool(&request.query, "raw_query").unwrap_or(false),
                fuzzy: parse_optional_bool(&request.query, "fuzzy").unwrap_or(false),
                explain: parse_optional_bool(&request.query, "explain").unwrap_or(false),
            };
            match search_vault_with_filter(paths, &search_query, read_filter.as_ref()) {
                Ok(report) => Response::ok(json!({ "ok": true, "result": report })),
                Err(error) => Response::error(500, error.to_string()),
            }
        }
        "/notes" => {
            let filters = request.query.get("where").cloned().unwrap_or_default();
            let query = NoteQuery {
                filters,
                sort_by: first_param(&request.query, "sort").map(ToOwned::to_owned),
                sort_descending: parse_optional_bool(&request.query, "desc").unwrap_or(false),
            };
            match query_notes_with_filter(paths, &query, read_filter.as_ref()) {
                Ok(mut report) => {
                    let offset = parse_optional_usize(&request.query, "offset").unwrap_or(0);
                    let limit = parse_optional_usize(&request.query, "limit");
                    let start = offset.min(report.notes.len());
                    let end = limit.map_or(report.notes.len(), |limit| {
                        start.saturating_add(limit).min(report.notes.len())
                    });
                    report.notes = report.notes[start..end].to_vec();
                    Response::ok(json!({ "ok": true, "result": report }))
                }
                Err(error) => Response::error(500, error.to_string()),
            }
        }
        "/graph/stats" => match query_graph_analytics_with_filter(paths, read_filter.as_ref()) {
            Ok(report) => Response::ok(json!({ "ok": true, "result": report })),
            Err(error) => Response::error(500, error.to_string()),
        },
        "/related" => {
            let Some(note) = first_param(&request.query, "note") else {
                return Response::error(400, "missing required query parameter: note");
            };
            let query = RelatedNotesQuery {
                provider: first_param(&request.query, "provider").map(ToOwned::to_owned),
                note: note.to_string(),
                limit: parse_optional_usize(&request.query, "limit").unwrap_or(10),
            };
            match query_related_notes_with_filter(paths, &query, read_filter.as_ref()) {
                Ok(report) => Response::ok(json!({ "ok": true, "result": report })),
                Err(error) => Response::error(500, error.to_string()),
            }
        }
        "/dataview/inline" => {
            let Some(file) = first_param(&request.query, "file") else {
                return Response::error(400, "missing required query parameter: file");
            };
            match run_dataview_inline_command(paths, file, Some(&permissions)) {
                Ok(report) => Response::ok(json!({ "ok": true, "result": report })),
                Err(error) => Response::error(500, error.to_string()),
            }
        }
        "/dataview/query" => {
            let Some(dql) = first_param(&request.query, "dql") else {
                return Response::error(400, "missing required query parameter: dql");
            };
            match run_dataview_query_command(paths, dql, read_filter.as_ref()) {
                Ok(result) => Response::ok(json!({ "ok": true, "result": result })),
                Err(error) => Response::error(500, error.to_string()),
            }
        }
        "/dataview/query-js" => {
            let Some(js) = first_param(&request.query, "js") else {
                return Response::error(400, "missing required query parameter: js");
            };
            match run_dataview_query_js_command(
                paths,
                js,
                first_param(&request.query, "file"),
                options.permissions.as_deref(),
            ) {
                Ok(result) => Response::ok(json!({ "ok": true, "result": result })),
                Err(error) => Response::error(500, error.to_string()),
            }
        }
        "/dataview/eval" => {
            let Some(file) = first_param(&request.query, "file") else {
                return Response::error(400, "missing required query parameter: file");
            };
            match run_dataview_eval_command(
                paths,
                file,
                parse_optional_usize(&request.query, "block"),
                options.permissions.as_deref(),
                Some(&permissions),
            ) {
                Ok(report) => Response::ok(json!({ "ok": true, "result": report })),
                Err(error) => Response::error(500, error.to_string()),
            }
        }
        _ => Response::error(404, "unknown endpoint"),
    }
}

fn serve_read_filter(
    paths: &VaultPaths,
    options: &ServeOptions,
) -> Result<Option<PermissionFilter>, String> {
    let filter = serve_permission_guard(paths, options)?.read_filter();
    Ok((!filter.path_permission().is_unrestricted()).then_some(filter))
}

fn serve_permission_guard(
    paths: &VaultPaths,
    options: &ServeOptions,
) -> Result<ProfilePermissionGuard, String> {
    let selection = resolve_permission_profile(paths, options.permissions.as_deref())
        .map_err(|error| error.to_string())?;
    Ok(ProfilePermissionGuard::new(paths, selection))
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
        method,
        path,
        query,
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

fn first_param<'a>(params: &'a HashMap<String, Vec<String>>, key: &str) -> Option<&'a str> {
    params
        .get(key)
        .and_then(|values| values.first())
        .map(String::as_str)
}

fn parse_optional_usize(params: &HashMap<String, Vec<String>>, key: &str) -> Option<usize> {
    first_param(params, key).and_then(|value| value.parse::<usize>().ok())
}

fn parse_optional_bool(params: &HashMap<String, Vec<String>>, key: &str) -> Option<bool> {
    first_param(params, key).and_then(|value| match value {
        "1" | "true" | "yes" => Some(true),
        "0" | "false" | "no" => Some(false),
        _ => None,
    })
}

fn parse_optional_search_sort(
    params: &HashMap<String, Vec<String>>,
    key: &str,
) -> Result<Option<SearchSort>, String> {
    let Some(value) = first_param(params, key) else {
        return Ok(None);
    };

    let sort = match value {
        "relevance" => SearchSort::Relevance,
        "path-asc" => SearchSort::PathAsc,
        "path-desc" => SearchSort::PathDesc,
        "modified-newest" => SearchSort::ModifiedNewest,
        "modified-oldest" => SearchSort::ModifiedOldest,
        "created-newest" => SearchSort::CreatedNewest,
        "created-oldest" => SearchSort::CreatedOldest,
        _ => {
            return Err(format!(
                "invalid search sort `{value}`; expected relevance, path-asc, path-desc, modified-newest, modified-oldest, created-newest, or created-oldest"
            ))
        }
    };

    Ok(Some(sort))
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

fn write_response(stream: &mut TcpStream, response: &Response) -> Result<(), std::io::Error> {
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
