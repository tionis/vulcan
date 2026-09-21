use crate::CliError;
#[cfg(test)]
use std::io::{Read, Write};
#[cfg(test)]
use std::net::TcpStream;
use std::net::{SocketAddr, TcpListener};
use std::str::FromStr;
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::Duration;
use vulcan_app::serve::ServeRouteOptions;
use vulcan_core::{VaultPaths, WatchOptions};
use vulcan_daemon::host::{HostSupervisor, ServiceLifecycleState};
use vulcan_daemon::shutdown::ShutdownSignal;
use vulcan_daemon::vault_http::{
    vault_listener_service, vault_watch_service, VaultHttpSecurity, VaultHttpState,
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
    shutdown: Arc<ShutdownSignal>,
    join_handle: Option<thread::JoinHandle<Result<(), CliError>>>,
}

struct TemporaryHostStart {
    listener: TcpListener,
    paths: VaultPaths,
    options: ServeOptions,
    http_state: VaultHttpState,
    shutdown: Arc<ShutdownSignal>,
    startup: mpsc::SyncSender<Result<(), String>>,
}

#[cfg(test)]
impl ServeHandle {
    #[must_use]
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn shutdown(mut self) -> Result<(), CliError> {
        self.shutdown.cancel();
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

pub fn spawn_server(paths: VaultPaths, mut options: ServeOptions) -> Result<ServeHandle, CliError> {
    if options.auth_token.is_none() {
        #[cfg(test)]
        let token = "generated-test-token".to_string();
        #[cfg(not(test))]
        let token = ulid::Ulid::new().to_string();
        eprintln!("Vulcan serve token: {token}");
        options.auth_token = Some(token);
    }
    let bind_addr = parse_bind_addr(&options.bind, options.auth_token.is_some())?;
    let listener = TcpListener::bind(bind_addr).map_err(CliError::operation)?;
    listener
        .set_nonblocking(true)
        .map_err(CliError::operation)?;
    let addr = listener.local_addr().map_err(CliError::operation)?;
    let shutdown = Arc::new(ShutdownSignal::default());
    let token = options
        .auth_token
        .clone()
        .expect("serve token is generated before binding");
    let expected_host = addr.to_string();
    let localhost_host = format!("localhost:{}", addr.port());
    let http_state = VaultHttpState::new(
        paths.clone(),
        ServeRouteOptions {
            permissions: options.permissions.clone(),
            watch_enabled: options.watch,
        },
        VaultHttpSecurity::new(
            token,
            vec![expected_host.clone(), localhost_host.clone()],
            vec![
                format!("http://{expected_host}"),
                format!("http://{localhost_host}"),
            ],
        ),
    )
    .map_err(CliError::operation)?;
    let join_shutdown = Arc::clone(&shutdown);
    let (startup_sender, startup_receiver) = mpsc::sync_channel(1);
    let start = TemporaryHostStart {
        listener,
        paths,
        options,
        http_state,
        shutdown: join_shutdown,
        startup: startup_sender,
    };
    let join_handle = thread::spawn(move || start.run());

    match startup_receiver.recv_timeout(Duration::from_secs(10)) {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            shutdown.cancel();
            let _ = join_handle.join();
            return Err(CliError::operation(error));
        }
        Err(error) => {
            shutdown.cancel();
            let _ = join_handle.join();
            return Err(CliError::operation(format!(
                "temporary HTTP host did not become ready: {error}"
            )));
        }
    }

    Ok(ServeHandle {
        addr,
        shutdown,
        join_handle: Some(join_handle),
    })
}

impl TemporaryHostStart {
    fn run(self) -> Result<(), CliError> {
        let Self {
            listener,
            paths,
            options,
            http_state,
            shutdown,
            startup,
        } = self;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(CliError::operation);
        let runtime = match runtime {
            Ok(runtime) => runtime,
            Err(error) => {
                let _ = startup.send(Err(error.to_string()));
                return Err(error);
            }
        };
        let listener = {
            let _runtime = runtime.enter();
            tokio::net::TcpListener::from_std(listener).map_err(CliError::operation)
        };
        let listener = match listener {
            Ok(listener) => listener,
            Err(error) => {
                let _ = startup.send(Err(error.to_string()));
                return Err(error);
            }
        };
        let state = http_state.health_handle();
        let registrations = (|| {
            let mut registrations = Vec::new();
            let mut dependencies = Vec::new();
            if options.watch {
                let watch = vault_watch_service(
                    paths,
                    state,
                    WatchOptions {
                        debounce_ms: options.debounce_ms,
                    },
                )
                .map_err(CliError::operation)?;
                dependencies.push(watch.definition.id.clone());
                registrations.push(watch);
            }
            registrations.push(
                vault_listener_service(
                    listener,
                    http_state,
                    runtime.handle().clone(),
                    dependencies,
                )
                .map_err(CliError::operation)?,
            );
            Ok::<_, CliError>(registrations)
        })();
        let registrations = match registrations {
            Ok(registrations) => registrations,
            Err(error) => {
                let _ = startup.send(Err(error.to_string()));
                return Err(error);
            }
        };
        let host = HostSupervisor::start_with_signal(
            registrations,
            Duration::from_secs(10),
            Arc::clone(&shutdown),
        );
        let host = match host {
            Ok(host) => host,
            Err(error) => {
                let _ = startup.send(Err(error.to_string()));
                return Err(CliError::operation(error));
            }
        };
        let _ = startup.send(Ok(()));
        while !shutdown.wait_timeout(Duration::from_secs(1)) {}
        let failed = host
            .status_handle()
            .statuses()
            .map_err(CliError::operation)?
            .into_iter()
            .find(|status| status.state == ServiceLifecycleState::Failed);
        host.shutdown().map_err(CliError::operation)?;
        if let Some(failed) = failed {
            return Err(CliError::operation(format!(
                "temporary service `{}` failed: {}",
                failed.id,
                failed
                    .last_failure
                    .map_or_else(|| "unknown failure".to_string(), |failure| failure.detail)
            )));
        }
        Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::fs;
    use std::time::Duration;
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
    fn temporary_host_reports_bind_conflicts_without_leaking_listener_state() {
        let vault = TempDir::new().expect("vault");
        fs::create_dir(vault.path().join(".vulcan")).expect("config directory");
        fs::write(vault.path().join("Home.md"), "# Home\n").expect("note");
        let paths = VaultPaths::new(vault.path());
        scan_vault(&paths, ScanMode::Full).expect("scan");
        let occupied = TcpListener::bind("127.0.0.1:0").expect("occupied listener");
        let address = occupied.local_addr().expect("address");
        let options = ServeOptions {
            bind: address.to_string(),
            watch: false,
            debounce_ms: 50,
            auth_token: Some("secret".to_string()),
            permissions: None,
        };

        assert!(spawn_server(paths.clone(), options.clone()).is_err());
        drop(occupied);
        let handle = spawn_server(paths, options).expect("port is reusable after conflict");
        assert_eq!(
            get_json(handle.addr(), "/health", Some("secret"))["ok"],
            true
        );
        handle.shutdown().expect("temporary host shutdown");
    }

    #[test]
    fn partial_watcher_startup_failure_rolls_back_the_listener() {
        let temporary = TempDir::new().expect("temporary directory");
        let missing_vault = temporary.path().join("missing-vault");
        let probe = TcpListener::bind("127.0.0.1:0").expect("port probe");
        let address = probe.local_addr().expect("address");
        drop(probe);
        let error = spawn_server(
            VaultPaths::new(&missing_vault),
            ServeOptions {
                bind: address.to_string(),
                watch: true,
                debounce_ms: 50,
                auth_token: Some("secret".to_string()),
                permissions: None,
            },
        )
        .expect_err("missing watched vault must fail startup");
        assert!(error.to_string().contains("service"));
        let rebound = TcpListener::bind(address).expect("failed startup releases listener");
        drop(rebound);
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

        let bad_host = get_json_with_authority(handle.addr(), "evil.example", None);
        let bad_origin = get_json_with_authority(
            handle.addr(),
            &handle.addr().to_string(),
            Some("https://evil.example"),
        );
        assert_eq!(bad_host["error"], "forbidden Host header");
        assert_eq!(bad_origin["error"], "forbidden Origin header");

        let mut malformed = TcpStream::connect(handle.addr()).expect("connection");
        malformed
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("read timeout");
        malformed
            .write_all(b"GET /health HTTP/1.1\r\nHost: bad host value\r\nConnection: close\r\n\r\n")
            .expect("malformed request write");
        let mut malformed_response = String::new();
        malformed
            .read_to_string(&mut malformed_response)
            .expect("malformed response read");
        assert!(
            malformed_response.starts_with("HTTP/1.1 400")
                || malformed_response.starts_with("HTTP/1.1 403"),
            "invalid authority must fail closed: {malformed_response}"
        );
        assert_eq!(
            get_json(handle.addr(), "/health", Some("secret"))["ok"],
            true,
            "malformed connections must not poison the listener"
        );

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
        let token = token.or(Some("generated-test-token"));
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
        let token = token.or(Some("generated-test-token"));
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

    fn get_json_with_authority(addr: SocketAddr, host: &str, origin: Option<&str>) -> Value {
        let mut stream = TcpStream::connect(addr).expect("server should accept connections");
        let mut request = format!(
            "GET /health HTTP/1.1\r\nHost: {host}\r\nX-Vulcan-Token: secret\r\nConnection: close\r\n"
        );
        if let Some(origin) = origin {
            request.push_str("Origin: ");
            request.push_str(origin);
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
        serde_json::from_str(response.split("\r\n\r\n").nth(1).expect("response body"))
            .expect("response JSON")
    }
}
