#![allow(clippy::needless_pass_by_value, clippy::too_many_lines)]

use assert_cmd::Command;
use predicates::prelude::*;
use rusqlite::Connection;
use serde_json::Value;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command as ProcessCommand, Stdio};
use std::thread;
use tempfile::TempDir;
use vulcan_core::{CacheDatabase, VaultPaths};
use zip::ZipArchive;

const FIXED_NOW: &str = "2026-04-04T12:00:00Z";

fn run_git_ok(vault_root: &Path, args: &[&str]) {
    let status = ProcessCommand::new("git")
        .arg("-C")
        .arg(vault_root)
        .args(args)
        .status()
        .expect("git should launch");
    assert!(status.success(), "git command failed: {args:?}");
}

fn init_git_repo(vault_root: &Path) {
    run_git_ok(vault_root, &["-c", "init.defaultBranch=main", "init"]);
    run_git_ok(vault_root, &["config", "user.name", "Vulcan Test"]);
    run_git_ok(vault_root, &["config", "user.email", "vulcan@example.com"]);
}

fn commit_all(vault_root: &Path, message: &str) {
    run_git_ok(vault_root, &["add", "."]);
    run_git_ok(vault_root, &["commit", "-m", message]);
}

fn cargo_vulcan_fixed_now() -> Command {
    let mut command = Command::cargo_bin("vulcan").expect("binary should build");
    command.env("VULCAN_FIXED_NOW", FIXED_NOW);
    command
}

fn cargo_vulcan_at_time(fixed_now: &str) -> Command {
    let mut command = Command::cargo_bin("vulcan").expect("binary should build");
    command.env("VULCAN_FIXED_NOW", fixed_now);
    command
}

fn has_extension(path: &str, extension: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.eq_ignore_ascii_case(extension))
}

fn write_plugin_file(vault_root: &Path, name: &str, source: &str) {
    let plugin_dir = vault_root.join(".vulcan/plugins");
    fs::create_dir_all(&plugin_dir).expect("plugin dir should be created");
    fs::write(plugin_dir.join(format!("{name}.js")), source).expect("plugin file should write");
}

fn test_host_exec_argv(command: &str) -> Vec<String> {
    #[cfg(target_os = "windows")]
    {
        vec![
            "powershell".to_string(),
            "-NoProfile".to_string(),
            "-Command".to_string(),
            command.to_string(),
        ]
    }
    #[cfg(not(target_os = "windows"))]
    {
        vec![
            "/bin/sh".to_string(),
            "-lc".to_string(),
            command.to_string(),
        ]
    }
}

fn test_host_output_command(text: &str) -> String {
    #[cfg(target_os = "windows")]
    {
        format!("[Console]::Out.Write('{text}')")
    }
    #[cfg(not(target_os = "windows"))]
    {
        format!("printf %s {text}")
    }
}

struct McpSession {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl McpSession {
    fn start(vault_root: &Path, extra_args: &[&str]) -> Self {
        let mut command = ProcessCommand::new(assert_cmd::cargo::cargo_bin("vulcan"));
        command
            .env("VULCAN_FIXED_NOW", FIXED_NOW)
            .args(["--vault", vault_root.to_str().expect("utf-8"), "mcp"])
            .args(extra_args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = command.spawn().expect("mcp server should start");
        let stdin = child.stdin.take().expect("stdin should be piped");
        let stdout = BufReader::new(child.stdout.take().expect("stdout should be piped"));
        Self {
            child,
            stdin,
            stdout,
        }
    }

    fn send(&mut self, request: Value) -> Vec<Value> {
        let id = request.get("id").cloned();
        writeln!(self.stdin, "{request}").expect("request should write");
        self.stdin.flush().expect("request should flush");

        let mut messages = Vec::new();
        loop {
            let mut line = String::new();
            let bytes = self
                .stdout
                .read_line(&mut line)
                .expect("mcp server should respond");
            assert!(bytes > 0, "mcp server closed stdout unexpectedly");
            let message: Value =
                serde_json::from_str(line.trim_end()).expect("mcp should emit valid JSON");
            let matches_id = id
                .as_ref()
                .is_some_and(|expected| message.get("id") == Some(expected));
            messages.push(message);
            if matches_id {
                return messages;
            }
        }
    }

    fn send_notification(&mut self, notification: Value) {
        writeln!(self.stdin, "{notification}").expect("notification should write");
        self.stdin.flush().expect("notification should flush");
    }

    fn finish(mut self) -> Vec<Value> {
        drop(self.stdin);
        let mut output = String::new();
        self.stdout
            .read_to_string(&mut output)
            .expect("mcp stdout should read");
        let status = self.child.wait().expect("mcp server should exit");
        assert!(
            status.success(),
            "mcp server exited unsuccessfully: {status}"
        );
        output
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).expect("mcp should emit valid JSON"))
            .collect()
    }
}

fn start_mcp_session_with_xdg(
    vault_root: &Path,
    config_home: &str,
    extra_args: &[&str],
) -> McpSession {
    let mut command = ProcessCommand::new(assert_cmd::cargo::cargo_bin("vulcan"));
    command
        .env("VULCAN_FIXED_NOW", FIXED_NOW)
        .env("XDG_CONFIG_HOME", config_home)
        .args(["--vault", vault_root.to_str().expect("utf-8"), "mcp"])
        .args(extra_args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = command.spawn().expect("mcp server should start");
    let stdin = child.stdin.take().expect("stdin should be piped");
    let stdout = BufReader::new(child.stdout.take().expect("stdout should be piped"));
    McpSession {
        child,
        stdin,
        stdout,
    }
}

struct McpHttpSession {
    child: Child,
    bind_addr: String,
    endpoint: String,
    auth_token: Option<String>,
}

impl McpHttpSession {
    fn start(
        vault_root: &Path,
        endpoint: &str,
        auth_token: Option<&str>,
        extra_args: &[&str],
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener should bind");
        let bind_addr = listener
            .local_addr()
            .expect("listener should expose its local address")
            .to_string();
        drop(listener);

        let mut command = ProcessCommand::new(assert_cmd::cargo::cargo_bin("vulcan"));
        command
            .env("VULCAN_FIXED_NOW", FIXED_NOW)
            .args(["--vault", vault_root.to_str().expect("utf-8"), "mcp"])
            .args([
                "--transport",
                "http",
                "--bind",
                &bind_addr,
                "--endpoint",
                endpoint,
            ])
            .args(extra_args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Some(token) = auth_token {
            command.args(["--auth-token", token]);
        }

        let mut child = command.spawn().expect("MCP HTTP server should start");
        wait_for_http_server_ready(&mut child, &bind_addr);

        Self {
            child,
            bind_addr,
            endpoint: endpoint.to_string(),
            auth_token: auth_token.map(ToOwned::to_owned),
        }
    }

    fn post(&self, payload: &Value, session_id: Option<&str>) -> HttpResponse {
        self.post_with_auth(payload, session_id, true)
    }

    fn post_with_auth(
        &self,
        payload: &Value,
        session_id: Option<&str>,
        include_auth: bool,
    ) -> HttpResponse {
        let body = serde_json::to_vec(payload).expect("payload should serialize");
        let mut headers = vec![
            ("Content-Type".to_string(), "application/json".to_string()),
            (
                "Accept".to_string(),
                "application/json, text/event-stream".to_string(),
            ),
            ("MCP-Protocol-Version".to_string(), "2025-06-18".to_string()),
        ];
        if let Some(session_id) = session_id {
            headers.push(("Mcp-Session-Id".to_string(), session_id.to_string()));
        }
        if include_auth {
            if let Some(token) = self.auth_token.as_deref() {
                headers.push(("Authorization".to_string(), format!("Bearer {token}")));
            }
        }
        self.request("POST", &headers, Some(&body))
    }

    fn delete(&self, session_id: &str) -> HttpResponse {
        let mut headers = vec![("Mcp-Session-Id".to_string(), session_id.to_string())];
        if let Some(token) = self.auth_token.as_deref() {
            headers.push(("Authorization".to_string(), format!("Bearer {token}")));
        }
        self.request("DELETE", &headers, None)
    }

    fn open_sse(&self, session_id: &str) -> McpSseStream {
        let mut headers = vec![
            ("Accept".to_string(), "text/event-stream".to_string()),
            ("Mcp-Session-Id".to_string(), session_id.to_string()),
        ];
        if let Some(token) = self.auth_token.as_deref() {
            headers.push(("Authorization".to_string(), format!("Bearer {token}")));
        }

        let mut stream = TcpStream::connect(&self.bind_addr).expect("SSE connection should open");
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .expect("read timeout should be configurable");
        let mut request = format!(
            "GET {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n",
            self.endpoint, self.bind_addr
        );
        for (name, value) in &headers {
            request.push_str(name);
            request.push_str(": ");
            request.push_str(value);
            request.push_str("\r\n");
        }
        request.push_str("\r\n");
        stream
            .write_all(request.as_bytes())
            .expect("SSE request should write");
        stream.flush().expect("SSE request should flush");

        let mut reader = BufReader::new(stream);
        let (status_line, headers) = read_http_status_and_headers(&mut reader);
        assert_eq!(status_line, "HTTP/1.1 200 OK");
        assert_eq!(
            headers.get("content-type").map(String::as_str),
            Some("text/event-stream")
        );
        McpSseStream { reader }
    }

    fn request(
        &self,
        method: &str,
        headers: &[(String, String)],
        body: Option<&[u8]>,
    ) -> HttpResponse {
        let mut stream = TcpStream::connect(&self.bind_addr).expect("HTTP connection should open");
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .expect("read timeout should be configurable");
        let mut request = format!(
            "{method} {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n",
            self.endpoint, self.bind_addr
        );
        for (name, value) in headers {
            request.push_str(name);
            request.push_str(": ");
            request.push_str(value);
            request.push_str("\r\n");
        }
        if let Some(body) = body {
            request.push_str("Content-Length: ");
            request.push_str(&body.len().to_string());
            request.push_str("\r\n");
        }
        request.push_str("\r\n");
        stream
            .write_all(request.as_bytes())
            .expect("request headers should write");
        if let Some(body) = body {
            stream.write_all(body).expect("request body should write");
        }
        stream.flush().expect("request should flush");
        let _ = stream.shutdown(Shutdown::Write);
        read_http_response(stream)
    }
}

impl Drop for McpHttpSession {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

struct McpSseStream {
    reader: BufReader<TcpStream>,
}

impl McpSseStream {
    fn read_event(&mut self) -> Value {
        let mut payload_lines = Vec::new();
        loop {
            let mut line = String::new();
            let bytes = self
                .reader
                .read_line(&mut line)
                .expect("SSE stream should be readable");
            assert!(bytes > 0, "SSE stream closed before an event arrived");
            let line = line.trim_end_matches(&['\r', '\n'][..]);
            if line.is_empty() {
                if payload_lines.is_empty() {
                    continue;
                }
                let payload = payload_lines.join("\n");
                return serde_json::from_str(&payload).expect("SSE event payload should parse");
            }
            if let Some(payload) = line.strip_prefix("data: ") {
                payload_lines.push(payload.to_string());
            }
        }
    }
}

struct HttpResponse {
    status_line: String,
    headers: std::collections::BTreeMap<String, String>,
    body: Vec<u8>,
}

impl HttpResponse {
    fn json_body(&self) -> Value {
        serde_json::from_slice(&self.body).expect("HTTP response body should contain valid JSON")
    }
}

fn wait_for_http_server_ready(child: &mut Child, bind_addr: &str) {
    for _ in 0..100 {
        if TcpStream::connect(bind_addr).is_ok() {
            return;
        }
        if let Some(status) = child.try_wait().expect("child status should be readable") {
            panic!("MCP HTTP server exited before it started listening: {status}");
        }
        thread::sleep(std::time::Duration::from_millis(20));
    }
    panic!("MCP HTTP server did not start listening at {bind_addr}");
}

fn read_http_response(mut stream: TcpStream) -> HttpResponse {
    let mut bytes = Vec::new();
    stream
        .read_to_end(&mut bytes)
        .expect("HTTP response should be readable");
    parse_http_response_bytes(&bytes)
}

fn parse_http_response_bytes(bytes: &[u8]) -> HttpResponse {
    let header_end =
        find_subslice(bytes, b"\r\n\r\n").expect("HTTP response should contain headers");
    let header_text =
        String::from_utf8(bytes[..header_end].to_vec()).expect("HTTP headers should be utf-8");
    let mut lines = header_text.lines();
    let status_line = lines
        .next()
        .expect("HTTP response should include a status line")
        .to_string();
    let headers = lines
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            Some((name.trim().to_ascii_lowercase(), value.trim().to_string()))
        })
        .collect();
    HttpResponse {
        status_line,
        headers,
        body: bytes[header_end + 4..].to_vec(),
    }
}

fn read_http_status_and_headers(
    reader: &mut BufReader<TcpStream>,
) -> (String, std::collections::BTreeMap<String, String>) {
    let mut status_line = String::new();
    reader
        .read_line(&mut status_line)
        .expect("HTTP response should start with a status line");
    let status_line = status_line.trim_end_matches(&['\r', '\n'][..]).to_string();

    let mut headers = std::collections::BTreeMap::new();
    loop {
        let mut line = String::new();
        let bytes = reader
            .read_line(&mut line)
            .expect("HTTP response headers should be readable");
        assert!(bytes > 0, "HTTP response closed before header terminator");
        let line = line.trim_end_matches(&['\r', '\n'][..]);
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }
    (status_line, headers)
}

fn cargo_vulcan_with_xdg_config(config_home: &str) -> Command {
    let mut command = Command::cargo_bin("vulcan").expect("binary should build");
    command.env("XDG_CONFIG_HOME", config_home);
    command
}

fn trust_and_scan_vault(config_home: &str, vault_root: &str) {
    cargo_vulcan_with_xdg_config(config_home)
        .args(["--vault", vault_root, "trust", "add"])
        .assert()
        .success();

    cargo_vulcan_with_xdg_config(config_home)
        .args(["--vault", vault_root, "index", "scan", "--full"])
        .assert()
        .success();
}

struct PluginTestFixture {
    _temp_dir: TempDir,
    vault_root: PathBuf,
    vault_root_str: String,
    config_home_str: String,
    blocked_file: PathBuf,
    allowed_file: PathBuf,
}

fn build_plugin_test_fixture() -> PluginTestFixture {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join("Projects")).expect("projects dir should exist");
    fs::write(vault_root.join("Projects/Alpha.md"), "hello\n").expect("note should write");
    fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should exist");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        r#"
[permissions.profiles.plugin]
read = "all"
write = "none"
refactor = "none"
git = "deny"
network = "deny"
index = "deny"
config = "none"
execute = "allow"
shell = "deny"

[plugins.lint]
enabled = true
events = ["on_note_write"]
permission_profile = "plugin"
sandbox = "strict"
"#,
    )
    .expect("config should write");
    write_plugin_file(
        &vault_root,
        "lint",
        r#"
function on_note_write(event) {
  if (!event.content.includes("approved")) {
    throw new Error("plugin blocked write");
  }
}

function main(event, ctx) {
  return { kind: event.kind, plugin: ctx.plugin.name };
}
"#,
    );

    let config_home = temp_dir.path().join("xdg");
    fs::create_dir_all(&config_home).expect("xdg dir should exist");
    let blocked_file = temp_dir.path().join("blocked.md");
    fs::write(&blocked_file, "draft\n").expect("blocked input should write");
    let allowed_file = temp_dir.path().join("allowed.md");
    fs::write(&allowed_file, "approved change\n").expect("allowed input should write");

    PluginTestFixture {
        _temp_dir: temp_dir,
        vault_root_str: vault_root
            .to_str()
            .expect("vault path should be valid utf-8")
            .to_string(),
        config_home_str: config_home
            .to_str()
            .expect("config home path should be valid utf-8")
            .to_string(),
        vault_root,
        blocked_file,
        allowed_file,
    }
}

#[test]
fn help_mentions_global_flags_and_core_commands() {
    let mut command = Command::cargo_bin("vulcan").expect("binary should build");

    command.arg("--help").assert().success().stdout(
        predicate::str::contains("--vault <VAULT>")
            .and(predicate::str::contains("--output <OUTPUT>"))
            .and(predicate::str::contains("--refresh <REFRESH>"))
            .and(predicate::str::contains("--verbose"))
            .and(predicate::str::contains("index"))
            .and(predicate::str::contains("graph"))
            .and(predicate::str::contains("ls"))
            .and(predicate::str::contains("dataview"))
            .and(predicate::str::contains("tasks"))
            .and(predicate::str::contains("kanban"))
            .and(predicate::str::contains("browse"))
            .and(predicate::str::contains("note"))
            .and(predicate::str::contains("bases"))
            .and(predicate::str::contains("search"))
            .and(predicate::str::contains("tags"))
            .and(predicate::str::contains("properties"))
            .and(predicate::str::contains("vectors"))
            .and(predicate::str::contains("edit"))
            .and(predicate::str::contains("doctor"))
            .and(predicate::str::contains("cache"))
            .and(predicate::str::contains("refactor"))
            .and(predicate::str::contains("saved"))
            .and(predicate::str::contains("checkpoint"))
            .and(predicate::str::contains("changes"))
            .and(predicate::str::contains("today"))
            .and(predicate::str::contains("daily"))
            .and(predicate::str::contains("periodic"))
            .and(predicate::str::contains("git"))
            .and(predicate::str::contains("web"))
            .and(predicate::str::contains("inbox"))
            .and(predicate::str::contains("template"))
            .and(predicate::str::contains("export"))
            .and(predicate::str::contains("config"))
            .and(predicate::str::contains("automation"))
            .and(predicate::str::contains("plugin"))
            .and(predicate::str::contains("run"))
            .and(predicate::str::contains("render"))
            .and(predicate::str::contains("help"))
            .and(predicate::str::contains("describe"))
            .and(predicate::str::contains("completions"))
            .and(predicate::str::contains("open"))
            .and(predicate::str::contains(
                "Initialize, scan, rebuild, repair, watch, and serve index state",
            ))
            .and(predicate::str::contains("Search indexed note content"))
            .and(predicate::str::contains(
                "Generate shell completion scripts",
            ))
            .and(predicate::str::contains("Quick start:"))
            .and(predicate::str::contains("Command groups"))
            .and(predicate::str::contains("Notes:"))
            .and(predicate::str::contains("Query:"))
            .and(predicate::str::contains("Tasks:"))
            .and(predicate::str::contains("Index:"))
            .and(predicate::str::contains("Interactive:"))
            .and(predicate::str::contains("Scripting:"))
            .and(predicate::str::contains("Setup:"))
            .and(predicate::str::contains("vulcan help <command>"))
            .and(predicate::str::contains(
                "Machine-readable schema: vulcan describe",
            ))
            .and(predicate::str::contains(
                "Override automatic cache refresh with --refresh <off|blocking|background>",
            ))
            .and(predicate::str::contains("--color <COLOR>"))
            .and(predicate::str::contains("--color always|never|auto"))
            .and(predicate::str::is_match(r"(?m)^\s+notes\s").unwrap().not())
            .and(
                predicate::str::is_match(r"(?m)^\s+cluster\s")
                    .unwrap()
                    .not(),
            )
            .and(
                predicate::str::is_match(r"(?m)^\s+related\s")
                    .unwrap()
                    .not(),
            )
            .and(predicate::str::is_match(r"(?m)^\s+weekly\s").unwrap().not())
            .and(
                predicate::str::is_match(r"(?m)^\s+monthly\s")
                    .unwrap()
                    .not(),
            )
            .and(predicate::str::is_match(r"(?m)^\s+diff\s").unwrap().not())
            .and(predicate::str::is_match(r"(?m)^\s+batch\s").unwrap().not()),
    );
}

#[test]
fn help_markdown_output_emits_raw_markdown() {
    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["--output", "markdown", "help"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("# help")
                .and(predicate::str::contains("## Interactive"))
                .and(predicate::str::contains("## Scripting & Tools"))
                .and(predicate::str::contains("- `render`"))
                .and(predicate::str::contains('\x1b').not()),
        );
}

#[test]
fn color_never_suppresses_ansi_in_help() {
    // With --color never, vulcan help output must not contain any ANSI escape codes.
    let output = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["--color", "never", "help"])
        .output()
        .expect("command should run");
    assert!(output.status.success(), "vulcan help should succeed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains('\x1b'),
        "--color never should not emit ANSI escape codes"
    );
}

#[test]
fn color_always_emits_ansi_in_help() {
    // With --color always, vulcan help output must contain ANSI escape codes even
    // when stdout is not a TTY (which it is not in tests).
    let output = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["--color", "always", "help"])
        .output()
        .expect("command should run");
    assert!(output.status.success(), "vulcan help should succeed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains('\x1b'),
        "--color always should emit ANSI escape codes"
    );
}

#[test]
fn no_color_env_suppresses_ansi_in_help() {
    // NO_COLOR env var (even with --color auto, the default) must suppress ANSI codes.
    let output = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .env("NO_COLOR", "1")
        .arg("help")
        .output()
        .expect("command should run");
    assert!(output.status.success(), "vulcan help should succeed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    // In a non-TTY test environment with NO_COLOR set, no ANSI codes expected.
    assert!(
        !stdout.contains('\x1b'),
        "NO_COLOR should suppress ANSI escape codes"
    );
}

#[test]
fn commands_with_new_after_help_include_examples() {
    // Verify that previously-missing after_help sections are now present.
    let checks: &[(&[&str], &str)] = &[
        (&["graph", "--help"], "vulcan graph path"),
        (&["graph", "--help"], "vulcan graph hubs"),
        (&["checkpoint", "--help"], "vulcan checkpoint create"),
        (&["checkpoint", "--help"], "vulcan checkpoint list"),
        (&["export", "--help"], "vulcan export search-index"),
        (&["cache", "--help"], "vulcan cache inspect"),
        (&["cache", "--help"], "vulcan cache verify"),
        (&["doctor", "--help"], "vulcan doctor"),
        (&["doctor", "--help"], "vulcan doctor --fix"),
        (&["vectors", "--help"], "vulcan vectors index"),
        (&["vectors", "--help"], "vulcan vectors cluster"),
        (&["vectors", "--help"], "vulcan vectors neighbors"),
        (&["changes", "--help"], "vulcan changes"),
        (&["periodic", "--help"], "vulcan periodic weekly"),
        (&["automation", "--help"], "vulcan automation list"),
        (&["plugin", "--help"], "vulcan plugin enable lint"),
        (&["trust", "--help"], "vulcan trust add"),
        (&["refactor", "--help"], "vulcan refactor rename-heading"),
    ];
    for (args, expected) in checks {
        Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args(*args)
            .assert()
            .success()
            .stdout(predicate::str::contains(*expected));
    }
}

#[test]
fn help_topic_js_plugins_describes_hook_api() {
    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["help", "js.plugins"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("on_note_write(event, ctx)")
                .and(predicate::str::contains("on_pre_commit"))
                .and(predicate::str::contains("vulcan plugin run <name>"))
                .and(predicate::str::contains("trusted vault")),
        );
}

#[test]
fn help_topics_cover_custom_tools_host_execution_and_surface_comparison() {
    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["help", "js.tools"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("tools.list()")
                .and(predicate::str::contains("ctx.secrets.require(name)"))
                .and(predicate::str::contains("result, text")),
        );

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["help", "js.host"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("host.exec(argv, opts?)")
                .and(predicate::str::contains("host.shell(command, opts?)"))
                .and(predicate::str::contains("requires `execute`")),
        );

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["help", "automation-surfaces"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("Use a skill to teach a workflow.")
                .and(predicate::str::contains("Use a custom tool"))
                .and(predicate::str::contains("Use a plugin")),
        );
}

#[test]
fn plugin_list_and_enable_disable_round_trip() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault root should exist");
    write_plugin_file(
        &vault_root,
        "lint",
        "function main() { return { ok: true }; }\n",
    );
    let vault_root_str = vault_root
        .to_str()
        .expect("vault path should be valid utf-8")
        .to_string();

    let list_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "--output",
            "json",
            "plugin",
            "list",
        ])
        .assert()
        .success();
    let list_json = parse_stdout_json(&list_assert);
    let plugins = list_json["plugins"]
        .as_array()
        .expect("plugins should be an array");
    assert_eq!(plugins.len(), 1);
    assert_eq!(plugins[0]["name"], "lint");
    assert_eq!(plugins[0]["registered"], false);
    assert_eq!(plugins[0]["enabled"], false);
    assert_eq!(plugins[0]["exists"], true);

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["--vault", &vault_root_str, "plugin", "enable", "lint"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Enabled plugin lint"));

    let config_text = fs::read_to_string(vault_root.join(".vulcan/config.toml"))
        .expect("config should be written");
    assert!(config_text.contains("[plugins.lint]"));
    assert!(config_text.contains("enabled = true"));

    let disable_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "--output",
            "json",
            "plugin",
            "disable",
            "lint",
        ])
        .assert()
        .success();
    let disable_json = parse_stdout_json(&disable_assert);
    assert_eq!(disable_json["name"], "lint");
    assert_eq!(disable_json["enabled"], false);
    assert_eq!(disable_json["updated"], true);
}

#[test]
fn custom_tool_commands_round_trip() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault root should exist");
    initialize_vulcan_dir(&vault_root);
    let config_home = temp_dir.path().join("config");
    fs::create_dir_all(&config_home).expect("config home should exist");
    let schema_path = temp_dir.path().join("tool-input-schema.json");
    fs::write(
        &schema_path,
        r#"{
  "type": "object",
  "additionalProperties": false,
  "properties": {
    "note": { "type": "string" }
  },
  "required": ["note"]
}"#,
    )
    .expect("schema should be written");
    let vault_root_str = vault_root.to_str().expect("utf-8").to_string();

    let init_assert = cargo_vulcan_fixed_now()
        .env("XDG_CONFIG_HOME", &config_home)
        .args([
            "--vault",
            &vault_root_str,
            "--output",
            "json",
            "tool",
            "init",
            "summarize_meeting",
            "--description",
            "Summarize one meeting note.",
        ])
        .assert()
        .success();
    let init_json = parse_stdout_json(&init_assert);
    assert_eq!(
        init_json["manifest_path"],
        ".agents/tools/summarize_meeting/TOOL.md"
    );

    let list_before_assert = cargo_vulcan_fixed_now()
        .env("XDG_CONFIG_HOME", &config_home)
        .args([
            "--vault",
            &vault_root_str,
            "--output",
            "json",
            "tool",
            "list",
        ])
        .assert()
        .success();
    let list_before_json = parse_stdout_json(&list_before_assert);
    let tools = list_before_json["tools"]
        .as_array()
        .expect("tools should be an array");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["name"], "summarize_meeting");
    assert_eq!(tools[0]["callable"], false);

    let show_assert = cargo_vulcan_fixed_now()
        .env("XDG_CONFIG_HOME", &config_home)
        .args([
            "--vault",
            &vault_root_str,
            "--output",
            "json",
            "tool",
            "show",
            "summarize_meeting",
        ])
        .assert()
        .success();
    let show_json = parse_stdout_json(&show_assert);
    assert_eq!(show_json["name"], "summarize_meeting");
    assert!(show_json["body"]
        .as_str()
        .expect("body should be a string")
        .contains("When to use"));

    let set_assert = cargo_vulcan_fixed_now()
        .env("XDG_CONFIG_HOME", &config_home)
        .args([
            "--vault",
            &vault_root_str,
            "--output",
            "json",
            "tool",
            "set",
            "summarize_meeting",
            "--description",
            "Summarize meeting notes into JSON.",
            "--timeout-ms",
            "2500",
            "--read-only",
            "--secret",
            "api=MEETING_API_KEY",
            "--input-schema-file",
            schema_path.to_str().expect("schema path should be utf-8"),
        ])
        .assert()
        .success();
    let set_json = parse_stdout_json(&set_assert);
    assert_eq!(set_json["updated"], true);

    let validate_assert = cargo_vulcan_fixed_now()
        .env("XDG_CONFIG_HOME", &config_home)
        .args([
            "--vault",
            &vault_root_str,
            "--output",
            "json",
            "tool",
            "validate",
        ])
        .assert()
        .success();
    let validate_json = parse_stdout_json(&validate_assert);
    assert_eq!(validate_json["valid"], true);

    run_scan(&vault_root);

    cargo_vulcan_fixed_now()
        .env("XDG_CONFIG_HOME", &config_home)
        .args(["--vault", &vault_root_str, "trust", "add"])
        .assert()
        .success();

    let list_after_assert = cargo_vulcan_fixed_now()
        .env("XDG_CONFIG_HOME", &config_home)
        .args([
            "--vault",
            &vault_root_str,
            "--output",
            "json",
            "tool",
            "list",
        ])
        .assert()
        .success();
    let list_after_json = parse_stdout_json(&list_after_assert);
    assert_eq!(list_after_json["tools"][0]["callable"], true);

    let run_assert = cargo_vulcan_fixed_now()
        .env("XDG_CONFIG_HOME", &config_home)
        .env("MEETING_API_KEY", "secret-token")
        .args([
            "--vault",
            &vault_root_str,
            "--output",
            "json",
            "tool",
            "run",
            "summarize_meeting",
            "--input-json",
            r#"{"note":"Meetings/Weekly.md"}"#,
        ])
        .assert()
        .success();
    let run_json = parse_stdout_json(&run_assert);
    assert_eq!(run_json["name"], "summarize_meeting");
    assert_eq!(run_json["result"]["ok"], true);
    assert_eq!(run_json["result"]["tool"], "summarize_meeting");
    assert_eq!(run_json["result"]["received"]["note"], "Meetings/Weekly.md");
}

#[test]
fn custom_tool_init_rejects_builtin_and_reserved_meta_names() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault root should exist");
    initialize_vulcan_dir(&vault_root);
    let vault_root_str = vault_root.to_str().expect("utf-8").to_string();

    for blocked_name in ["search", "tool_pack_enable"] {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args([
                "--vault",
                &vault_root_str,
                "tool",
                "init",
                blocked_name,
                "--description",
                "Should fail.",
            ])
            .assert()
            .failure();
        let stderr = String::from_utf8(assert.get_output().stderr.clone())
            .expect("stderr should be valid utf-8");
        assert!(
            stderr.contains("collides with a reserved or built-in tool name"),
            "tool init should reject `{blocked_name}`, got: {stderr}"
        );
    }
}

#[test]
fn run_js_runtime_can_list_get_and_call_custom_tools() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join(".agents/tools/echo")).expect("tool dir should exist");
    initialize_vulcan_dir(&vault_root);
    fs::write(
        vault_root.join(".agents/tools/echo/TOOL.md"),
        r"---
name: echo_tool
description: Echo one value.
input_schema:
  type: object
  additionalProperties: false
  properties:
    value:
      type: string
  required:
    - value
---

Echo docs for the JS runtime test.
",
    )
    .expect("manifest should write");
    fs::write(
        vault_root.join(".agents/tools/echo/main.js"),
        "function main(input) {\n  return { echoed: input.value, upper: String(input.value).toUpperCase() };\n}\n",
    )
    .expect("entrypoint should write");

    let config_home = temp_dir.path().join("config");
    fs::create_dir_all(&config_home).expect("config home should exist");
    let vault_root_str = vault_root.to_str().expect("utf-8").to_string();
    let config_home_str = config_home.to_str().expect("utf-8").to_string();

    trust_and_scan_vault(&config_home_str, &vault_root_str);

    let assert = cargo_vulcan_with_xdg_config(&config_home_str)
        .args([
            "--vault",
            &vault_root_str,
            "--output",
            "json",
            "run",
            "-e",
            r#"({
                listed: tools.list().map((tool) => ({ name: tool.name, callable: tool.callable })),
                described: tools.get("echo_tool").body.includes("Echo docs"),
                called: tools.call("echo_tool", { value: "alpha" })
            })"#,
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(
        json["value"]["listed"],
        serde_json::json!([{ "name": "echo_tool", "callable": true }])
    );
    assert_eq!(json["value"]["described"], true);
    assert_eq!(json["value"]["called"]["echoed"], "alpha");
    assert_eq!(json["value"]["called"]["upper"], "ALPHA");
}

#[test]
fn tool_run_can_use_host_exec_with_permissions() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join(".agents/tools/env_echo")).expect("tool dir should exist");
    initialize_vulcan_dir(&vault_root);
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        r#"
[permissions.profiles.exec_only]
read = "all"
write = "none"
refactor = "none"
git = "deny"
network = "deny"
index = "deny"
config = "read"
execute = "allow"
shell = "deny"
"#,
    )
    .expect("config should write");
    fs::write(
        vault_root.join(".agents/tools/env_echo/TOOL.md"),
        r"---
name: env_echo_tool
description: Echo one env var through host.exec.
permission_profile: exec_only
input_schema:
  type: object
---
",
    )
    .expect("manifest should write");
    let argv = serde_json::to_string(&test_host_exec_argv(&test_host_output_command("alpha")))
        .expect("argv json should serialize");
    fs::write(
        vault_root.join(".agents/tools/env_echo/main.js"),
        format!("function main() {{\n  return host.exec({argv});\n}}\n"),
    )
    .expect("entrypoint should write");

    let config_home = temp_dir.path().join("config");
    fs::create_dir_all(&config_home).expect("config home should exist");
    let vault_root_str = vault_root.to_str().expect("utf-8").to_string();
    let config_home_str = config_home.to_str().expect("utf-8").to_string();

    trust_and_scan_vault(&config_home_str, &vault_root_str);

    let run_assert = cargo_vulcan_with_xdg_config(&config_home_str)
        .args([
            "--vault",
            &vault_root_str,
            "--output",
            "json",
            "tool",
            "run",
            "env_echo_tool",
            "--input-json",
            "{}",
        ])
        .assert()
        .success();
    let run_json = parse_stdout_json(&run_assert);
    assert_eq!(run_json["name"], "env_echo_tool");
    assert_eq!(run_json["result"]["success"], true);
    assert_eq!(run_json["result"]["stdout"], "alpha");
    assert_eq!(run_json["result"]["timed_out"], false);
    assert_eq!(run_json["result"]["invocation"]["kind"], "exec");
}

#[test]
fn plugin_set_and_delete_manage_full_registration_surface() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault root should exist");
    initialize_vulcan_dir(&vault_root);
    let vault_root_str = vault_root.to_str().expect("utf-8").to_string();

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "plugin",
            "set",
            "lint",
            "--path",
            ".vulcan/plugins/lint.js",
            "--enable",
            "--add-event",
            "on_pre_commit",
            "--add-event",
            "on_note_write",
            "--sandbox",
            "strict",
            "--permission-profile",
            "readonly",
            "--description",
            "Lint staged note writes",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Updated plugin lint"));

    let config_text = fs::read_to_string(vault_root.join(".vulcan/config.toml"))
        .expect("config should be written");
    assert!(config_text.contains("[plugins.lint]"));
    assert!(config_text.contains("enabled = true"));
    assert!(config_text.contains("events = ["));
    assert!(config_text.contains("\"on_note_write\""));
    assert!(config_text.contains("\"on_pre_commit\""));
    assert!(config_text.contains("sandbox = \"strict\""));
    assert!(config_text.contains("permission_profile = \"readonly\""));
    assert!(config_text.contains("description = \"Lint staged note writes\""));

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["--vault", &vault_root_str, "plugin", "delete", "lint"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Updated plugin lint"));

    let config_text = fs::read_to_string(vault_root.join(".vulcan/config.toml"))
        .expect("config should still be readable");
    assert!(!config_text.contains("[plugins.lint]"));
}

#[test]
fn trusted_plugin_run_and_note_write_hook_work() {
    let fixture = build_plugin_test_fixture();

    cargo_vulcan_with_xdg_config(&fixture.config_home_str)
        .args(["--vault", &fixture.vault_root_str, "plugin", "run", "lint"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("trusted vault"));

    trust_and_scan_vault(&fixture.config_home_str, &fixture.vault_root_str);

    let run_assert = cargo_vulcan_with_xdg_config(&fixture.config_home_str)
        .args([
            "--vault",
            &fixture.vault_root_str,
            "--output",
            "json",
            "plugin",
            "run",
            "lint",
        ])
        .assert()
        .success();
    let run_json = parse_stdout_json(&run_assert);
    assert_eq!(run_json["value"]["kind"], "manual");
    assert_eq!(run_json["value"]["plugin"], "lint");

    cargo_vulcan_with_xdg_config(&fixture.config_home_str)
        .args([
            "--vault",
            &fixture.vault_root_str,
            "note",
            "set",
            "Projects/Alpha.md",
            "--file",
            fixture
                .blocked_file
                .to_str()
                .expect("blocked file path should be valid utf-8"),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("plugin blocked write"));
    assert_eq!(
        fs::read_to_string(fixture.vault_root.join("Projects/Alpha.md")).expect("note should read"),
        "hello\n"
    );

    cargo_vulcan_with_xdg_config(&fixture.config_home_str)
        .args([
            "--vault",
            &fixture.vault_root_str,
            "note",
            "set",
            "Projects/Alpha.md",
            "--file",
            fixture
                .allowed_file
                .to_str()
                .expect("allowed file path should be valid utf-8"),
        ])
        .assert()
        .success();
    assert_eq!(
        fs::read_to_string(fixture.vault_root.join("Projects/Alpha.md")).expect("note should read"),
        "approved change\n"
    );
}

#[test]
fn trusted_plugin_run_accepts_subset_permission_profiles() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should exist");
    fs::write(vault_root.join("Home.md"), "hello\n").expect("note should write");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        r#"
[permissions.profiles.agent]
read = "all"
write = { allow = ["folder:Projects/**"] }
refactor = "none"
git = "deny"
network = { allow = true, domains = ["example.com"] }
index = "deny"
config = "read"
execute = "allow"
shell = "deny"
cpu_limit_ms = 5000
memory_limit_mb = 64

[permissions.profiles.plugin]
read = { allow = ["note:Home.md"] }
write = "none"
refactor = "none"
git = "deny"
network = "deny"
index = "deny"
config = "none"
execute = "allow"
shell = "deny"
cpu_limit_ms = 100
memory_limit_mb = 32

[plugins.lint]
enabled = true
permission_profile = "plugin"
sandbox = "strict"
"#,
    )
    .expect("config should write");
    write_plugin_file(
        &vault_root,
        "lint",
        "function main(event, ctx) { return { profile: ctx.plugin.permission_profile, kind: event.kind }; }\n",
    );
    let config_home = temp_dir.path().join("xdg");
    fs::create_dir_all(&config_home).expect("xdg dir should exist");
    let config_home_str = config_home
        .to_str()
        .expect("config home path should be valid utf-8")
        .to_string();
    let vault_root_str = vault_root
        .to_str()
        .expect("vault path should be valid utf-8")
        .to_string();

    trust_and_scan_vault(&config_home_str, &vault_root_str);

    let assert = cargo_vulcan_with_xdg_config(&config_home_str)
        .args([
            "--vault",
            &vault_root_str,
            "--permissions",
            "agent",
            "--output",
            "json",
            "plugin",
            "run",
            "lint",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);
    assert_eq!(json["value"]["profile"], "plugin");
    assert_eq!(json["value"]["kind"], "manual");
}

#[test]
fn query_auto_detection_announces_dataview_queries() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--refresh",
            "off",
            "query",
            "LIST FROM #index",
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("(detected as Dataview query)"));
}

#[test]
fn config_edit_requires_an_interactive_terminal() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join(".vulcan")).expect("config dir should exist");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        "[web.search]\nbackend = \"duckduckgo\"\n",
    )
    .expect("config should write");

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "config",
            "edit",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "config edit requires an interactive terminal",
        ));
}

#[test]
fn config_aliases_expand_before_clap_parsing() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);
    fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should exist");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        "[aliases]\ntoday = \"query --format count\"\n",
    )
    .expect("config should be written");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "today",
        ])
        .assert()
        .success();
    let payload = parse_stdout_json(&assert);

    assert!(
        payload["count"].as_u64().is_some_and(|count| count > 0),
        "expanded alias should execute the replacement command"
    );
}

#[test]
fn config_show_aliases_includes_builtin_and_overridden_aliases() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should exist");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        "[aliases]\ntoday = \"daily show\"\nship = \"query --where 'status = shipped'\"\n",
    )
    .expect("config should be written");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "config",
            "show",
            "aliases",
        ])
        .assert()
        .success();
    let payload = parse_stdout_json(&assert);

    assert_eq!(payload["config"]["today"], "daily show");
    assert_eq!(payload["config"]["q"], "query");
    assert_eq!(payload["config"]["t"], "tasks list");
    assert_eq!(
        payload["config"]["ship"],
        "query --where 'status = shipped'"
    );
}

#[test]
fn query_fields_aligned_table_output_in_tty_mode() {
    // When --fields is given and stdout is a TTY (simulated via --color always to
    // force the aligned path) the output should have a header row and separator.
    // We cannot force a real TTY in tests, so we verify the non-TTY path (field=value)
    // still works, and the --color always path produces aligned headers when fields
    // are requested via the json output shape.
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);
    let vault_root_str = vault_root.to_str().expect("valid utf-8");

    // Non-TTY (pipe) path: field=value form for a note with known properties.
    // Using top-level field names as they appear in the JSON rows.
    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root_str,
            "--fields",
            "file_name,document_path",
            "notes",
            "--where",
            "status = active",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("file_name=Alpha"))
        .stdout(predicate::str::contains("document_path=Projects/Alpha.md"));

    // The help for query --format table should mention the format option.
    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["query", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("table"));
}

#[test]
fn tags_command_lists_and_filters_indexed_tags() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);

    let json_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tags",
            "--sort",
            "name",
            "--where",
            "file.path starts_with \"Projects/\"",
            "--where",
            "status = active",
        ])
        .assert()
        .success();
    let rows = parse_stdout_json_lines(&json_assert);

    assert_eq!(
        rows,
        vec![
            serde_json::json!({ "tag": "project", "count": 1 }),
            serde_json::json!({ "tag": "work", "count": 1 }),
        ]
    );

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "tags",
            "--count",
            "--sort",
            "name",
        ])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("dashboard (1)")
                .and(predicate::str::contains("people/team (1)"))
                .and(predicate::str::contains("work (1)")),
        );
}

#[test]
fn properties_command_lists_counts_and_types() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("mixed-properties", &vault_root);
    run_scan(&vault_root);

    let json_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "properties",
            "--sort",
            "name",
            "--type",
        ])
        .assert()
        .success();
    let rows = parse_stdout_json_lines(&json_assert);

    assert!(rows.iter().any(|row| {
        row == &serde_json::json!({
            "property": "due",
            "count": 3,
            "types": ["date", "text"],
        })
    }));
    assert!(rows.iter().any(|row| {
        row == &serde_json::json!({
            "property": "status",
            "count": 3,
            "types": ["list", "text"],
        })
    }));

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "properties",
            "--count",
            "--type",
            "--sort",
            "name",
        ])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("due (3) [date, text]")
                .and(predicate::str::contains("reviewed (3) [boolean, text]"))
                .and(predicate::str::contains("status (3) [list, text]")),
        );
}

#[test]
fn config_import_templater_json_output_reports_mappings() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    initialize_vulcan_dir(&vault_root);
    fs::create_dir_all(vault_root.join(".obsidian/plugins/templater-obsidian"))
        .expect("templater plugin dir should be created");
    fs::write(
        vault_root.join(".obsidian/plugins/templater-obsidian/data.json"),
        r#"{
          "command_timeout": 12,
          "templates_folder": "Templater/Templates",
          "templates_pairs": [["slugify", "bun run slugify"]],
          "trigger_on_file_creation": true,
          "enable_system_commands": true,
          "user_scripts_folder": "Scripts/User",
          "startup_templates": ["Startup"],
          "intellisense_render": 4
        }"#,
    )
    .expect("templater plugin config should be written");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .env_remove("KAGI_API_KEY")
        .env_remove("EXA_API_KEY")
        .env_remove("TAVILY_API_KEY")
        .env_remove("BRAVE_API_KEY")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "config",
            "import",
            "templater",
            "--no-commit",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["plugin"], "templater");
    assert_eq!(
        json["mappings"][0]["target"],
        Value::String("templates.templater_folder".to_string())
    );
    assert_eq!(
        json["mappings"][0]["value"],
        Value::String("Templater/Templates".to_string())
    );

    let rendered =
        fs::read_to_string(vault_root.join(".vulcan/config.toml")).expect("config should exist");
    assert!(rendered.contains("[templates]"));
    assert!(rendered.contains("templater_folder = \"Templater/Templates\""));
    assert!(rendered.contains("command_timeout = 12"));
    assert!(rendered.contains("[[templates.templates_pairs]]"));
    assert!(rendered.contains("name = \"slugify\""));
    assert!(rendered.contains("enable_system_commands = true"));
    assert!(rendered.contains("user_scripts_folder = \"Scripts/User\""));
    assert!(rendered.contains("startup_templates = [\"Startup\"]"));
    assert!(rendered.contains("intellisense_render = 4"));
}

#[test]
fn config_import_core_json_output_reports_sources_and_target_file() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    initialize_vulcan_dir(&vault_root);
    fs::create_dir_all(vault_root.join(".obsidian")).expect("obsidian dir should be created");
    fs::write(
        vault_root.join(".obsidian/app.json"),
        r#"{
          "useMarkdownLinks": true,
          "newLinkFormat": "shortest",
          "attachmentFolderPath": "Assets",
          "strictLineBreaks": true
        }"#,
    )
    .expect("app config should be written");
    fs::write(
        vault_root.join(".obsidian/templates.json"),
        r#"{
          "dateFormat": "YYYY-MM-DD",
          "timeFormat": "HH:mm",
          "folder": "Templates"
        }"#,
    )
    .expect("templates config should be written");
    fs::write(
        vault_root.join(".obsidian/types.json"),
        r#"{
          "effort": {"type": "number"},
          "reviewed": {"type": "checkbox"}
        }"#,
    )
    .expect("types config should be written");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .env_remove("KAGI_API_KEY")
        .env_remove("EXA_API_KEY")
        .env_remove("TAVILY_API_KEY")
        .env_remove("BRAVE_API_KEY")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "config",
            "import",
            "core",
            "--no-commit",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["plugin"], "core");
    assert_eq!(json["dry_run"], false);
    assert_eq!(json["target_file"], ".vulcan/config.toml");
    assert!(json["source_paths"].as_array().is_some_and(|paths| {
        paths.iter().any(|path| path == ".obsidian/app.json")
            && paths.iter().any(|path| path == ".obsidian/templates.json")
            && paths.iter().any(|path| path == ".obsidian/types.json")
    }));
    assert!(json["mappings"].as_array().is_some_and(|mappings| mappings
        .iter()
        .any(|mapping| mapping["target"] == "templates.obsidian_folder"
            && mapping["value"] == "Templates")));
    assert!(json["mappings"].as_array().is_some_and(|mappings| mappings
        .iter()
        .any(|mapping| mapping["target"] == "property_types.reviewed"
            && mapping["value"] == "checkbox")));

    let config =
        fs::read_to_string(vault_root.join(".vulcan/config.toml")).expect("config should exist");
    assert!(config.contains("[links]"));
    assert!(config.contains("style = \"markdown\""));
    assert!(config.contains("resolution = \"shortest\""));
    assert!(config.contains("attachment_folder = \"Assets\""));
    assert!(config.contains("strict_line_breaks = true"));
    assert!(config.contains("[templates]"));
    assert!(config.contains("obsidian_folder = \"Templates\""));
    assert!(config.contains("[property_types]"));
    assert!(config.contains("effort = \"number\""));
    assert!(config.contains("reviewed = \"checkbox\""));
}

#[test]
fn config_import_dataview_json_output_reports_mappings() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    initialize_vulcan_dir(&vault_root);
    fs::create_dir_all(vault_root.join(".obsidian/plugins/dataview"))
        .expect("dataview plugin dir should be created");
    fs::write(
        vault_root.join(".obsidian/plugins/dataview/data.json"),
        r#"{
          "inlineQueryPrefix": "dv:",
          "inlineJsQueryPrefix": "$dv:",
          "enableDataviewJs": false,
          "enableInlineDataviewJs": true,
          "taskCompletionTracking": true,
          "taskCompletionUseEmojiShorthand": true,
          "taskCompletionText": "done-on",
          "recursiveSubTaskCompletion": true,
          "showResultCount": false,
          "defaultDateFormat": "yyyy-MM-dd",
          "defaultDateTimeFormat": "yyyy-MM-dd HH:mm",
          "timezone": "+02:00",
          "maxRecursiveRenderDepth": 7,
          "tableIdColumnName": "Document",
          "tableGroupColumnName": "Bucket"
        }"#,
    )
    .expect("dataview config should be written");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .env_remove("KAGI_API_KEY")
        .env_remove("EXA_API_KEY")
        .env_remove("TAVILY_API_KEY")
        .env_remove("BRAVE_API_KEY")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "config",
            "import",
            "dataview",
            "--no-commit",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["plugin"], "dataview");
    assert_eq!(json["dry_run"], false);
    assert_eq!(json["target_file"], ".vulcan/config.toml");
    assert!(json["mappings"]
        .as_array()
        .is_some_and(|mappings| mappings.iter().any(|mapping| mapping["target"]
            == "dataview.inline_query_prefix"
            && mapping["value"] == "dv:")));

    let config =
        fs::read_to_string(vault_root.join(".vulcan/config.toml")).expect("config should exist");
    assert!(config.contains("[dataview]"));
    assert!(config.contains("inline_query_prefix = \"dv:\""));
    assert!(config.contains("enable_dataview_js = false"));
    assert!(config.contains("group_column_name = \"Bucket\""));
}

#[test]
fn config_import_list_json_output_reports_detectable_sources() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join(".obsidian/plugins/dataview"))
        .expect("dataview plugin dir should be created");
    fs::write(
        vault_root.join(".obsidian/app.json"),
        r#"{"useMarkdownLinks": true}"#,
    )
    .expect("app config should be written");
    fs::write(
        vault_root.join(".obsidian/plugins/dataview/data.json"),
        r#"{"inlineQueryPrefix":"dv:"}"#,
    )
    .expect("dataview config should be written");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "config",
            "import",
            "--list",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    let importers = json["importers"]
        .as_array()
        .expect("importers should be an array");
    assert!(importers.iter().any(|item| {
        item["plugin"] == "core"
            && item["detected"] == true
            && item["source_paths"]
                .as_array()
                .is_some_and(|paths| paths.iter().any(|path| path == ".obsidian/app.json"))
    }));
    assert!(importers
        .iter()
        .any(|item| item["plugin"] == "dataview" && item["detected"] == true));
    assert!(importers
        .iter()
        .any(|item| item["plugin"] == "templater" && item["detected"] == false));
}

#[test]
fn config_import_all_dry_run_aggregates_detected_sources() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join(".obsidian/plugins/dataview"))
        .expect("dataview plugin dir should be created");
    fs::write(
        vault_root.join(".obsidian/app.json"),
        r#"{
          "useMarkdownLinks": true,
          "newLinkFormat": "shortest"
        }"#,
    )
    .expect("app config should be written");
    fs::write(
        vault_root.join(".obsidian/plugins/dataview/data.json"),
        r#"{"inlineQueryPrefix":"dv:","tableGroupColumnName":"Bucket"}"#,
    )
    .expect("dataview config should be written");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "config",
            "import",
            "--all",
            "--dry-run",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["dry_run"], true);
    assert_eq!(json["detected_count"], 2);
    assert_eq!(json["imported_count"], 2);
    assert!(json["reports"]
        .as_array()
        .is_some_and(|reports| reports.iter().any(|report| report["plugin"] == "core")));
    assert!(json["reports"]
        .as_array()
        .is_some_and(|reports| reports.iter().any(|report| report["plugin"] == "dataview")));
    assert!(!vault_root.join(".vulcan/config.toml").exists());
}

#[test]
fn config_show_reports_effective_config_and_selected_sections() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should be created");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        r#"[templates]
obsidian_folder = "Shared Templates"

[periodic.daily]
template = "Daily Shared"
"#,
    )
    .expect("shared config should be written");
    fs::write(
        vault_root.join(".vulcan/config.local.toml"),
        r#"[periodic.daily]
template = "Daily Local"

[web.search]
backend = "brave"
"#,
    )
    .expect("local config should be written");

    let full_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "config",
            "show",
        ])
        .assert()
        .success();
    let full_json = parse_stdout_json(&full_assert);

    assert_eq!(full_json["section"], Value::Null);
    assert_eq!(
        full_json["config"]["templates"]["obsidian_folder"],
        "Shared Templates"
    );
    assert_eq!(
        full_json["config"]["periodic"]["daily"]["template"],
        "Daily Local"
    );
    assert_eq!(full_json["diagnostics"], Value::Array(Vec::new()));

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "config",
            "show",
            "periodic.daily",
        ])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("[periodic.daily]")
                .and(predicate::str::contains("template = \"Daily Local\"")),
        );
}

#[test]
fn config_get_reads_scalar_values_and_rejects_sections() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should be created");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        r#"[periodic.daily]
template = "Daily Shared"
"#,
    )
    .expect("shared config should be written");
    fs::write(
        vault_root.join(".vulcan/config.local.toml"),
        r#"[periodic.daily]
template = "Daily Local"

[web.search]
backend = "brave"
"#,
    )
    .expect("local config should be written");

    let json_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "config",
            "get",
            "periodic.daily.template",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&json_assert);

    assert_eq!(json["key"], "periodic.daily.template");
    assert_eq!(json["value"], "Daily Local");
    assert_eq!(json["diagnostics"], Value::Array(Vec::new()));

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "config",
            "get",
            "web.search.backend",
        ])
        .assert()
        .success()
        .stdout("brave\n");

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "config",
            "get",
            "periodic.daily",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "config key `periodic.daily` resolves to a section; use `vulcan config show periodic.daily` instead",
        ));
}

#[test]
fn config_set_writes_validated_values_and_supports_dry_run() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should be created");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        r#"[periodic.daily]
template = "Daily Shared"
"#,
    )
    .expect("shared config should be written");

    let json_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "config",
            "set",
            "periodic.daily.template",
            "Templates/Daily",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&json_assert);

    assert_eq!(json["key"], "periodic.daily.template");
    assert_eq!(json["value"], "Templates/Daily");
    assert_eq!(json["config_path"], ".vulcan/config.toml");
    assert_eq!(json["created_config"], false);
    assert_eq!(json["updated"], true);
    assert_eq!(json["dry_run"], false);
    assert_eq!(json["diagnostics"], Value::Array(Vec::new()));
    assert!(fs::read_to_string(vault_root.join(".vulcan/config.toml"))
        .expect("shared config should be readable")
        .contains("template = \"Templates/Daily\""));

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "config",
            "set",
            "periodic.daily.enabled",
            "not-a-bool",
            "--dry-run",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("expects a boolean value"));

    assert_eq!(
        fs::read_to_string(vault_root.join(".vulcan/config.toml"))
            .expect("shared config should still be readable"),
        "[periodic.daily]\ntemplate = \"Templates/Daily\"\n"
    );
}

#[test]
fn config_list_includes_creatable_optional_and_dynamic_keys() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    initialize_vulcan_dir(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root.to_str().expect("utf-8"),
            "--output",
            "json",
            "config",
            "list",
            "plugins",
        ])
        .assert()
        .success();
    let report = parse_stdout_json(&assert);
    let entries = report["entries"]
        .as_array()
        .expect("entries should be an array");
    assert!(entries.iter().any(|entry| entry["key"] == "plugins.<name>"));
    assert!(entries
        .iter()
        .any(|entry| entry["key"] == "plugins.<name>.events"));
    assert!(entries
        .iter()
        .any(|entry| entry["preferred_command"] == "vulcan plugin set"));

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root.to_str().expect("utf-8"),
            "--output",
            "json",
            "config",
            "list",
            "embedding",
        ])
        .assert()
        .success();
    let report = parse_stdout_json(&assert);
    let entries = report["entries"]
        .as_array()
        .expect("entries should be an array");
    assert!(entries
        .iter()
        .any(|entry| entry["key"] == "embedding.provider"));
    assert!(entries
        .iter()
        .any(|entry| entry["key"] == "embedding.model"));
}

#[test]
fn help_config_json_matches_generated_snapshot() {
    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["help", "config", "--output", "json"])
        .assert()
        .success();

    assert_json_snapshot("help_config.json", &parse_stdout_json(&assert));
}

#[test]
fn help_config_markdown_matches_reference_doc() {
    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["help", "config", "--output", "markdown"])
        .assert()
        .success();
    let actual = String::from_utf8(assert.get_output().stdout.clone())
        .expect("help output should be valid utf-8")
        .replace("\r\n", "\n");
    let expected = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../docs/reference/config.md"),
    )
    .expect("reference doc should be readable")
    .replace("\r\n", "\n");

    assert_eq!(actual, expected);
}

#[test]
fn config_set_target_local_creates_absent_sections_and_unset_prunes_them() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    initialize_vulcan_dir(&vault_root);
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        "[web.search]\nbackend = \"duckduckgo\"\n",
    )
    .expect("shared config should write");

    let set_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root.to_str().expect("utf-8"),
            "--output",
            "json",
            "config",
            "set",
            "embedding.model",
            "text-embedding-3-small",
            "--target",
            "local",
        ])
        .assert()
        .success();
    let set_json = parse_stdout_json(&set_assert);
    assert_eq!(set_json["config_path"], ".vulcan/config.local.toml");
    let local_config = fs::read_to_string(vault_root.join(".vulcan/config.local.toml"))
        .expect("local config should exist");
    assert!(local_config.contains("[embedding]"));
    assert!(local_config.contains("model = \"text-embedding-3-small\""));

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root.to_str().expect("utf-8"),
            "--output",
            "json",
            "config",
            "set",
            "web.search.backend",
            "brave",
            "--target",
            "local",
        ])
        .assert()
        .success();

    let show_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root.to_str().expect("utf-8"),
            "--output",
            "json",
            "config",
            "show",
            "web.search",
        ])
        .assert()
        .success();
    let show_json = parse_stdout_json(&show_assert);
    assert_eq!(show_json["config"]["backend"], "brave");

    let unset_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root.to_str().expect("utf-8"),
            "--output",
            "json",
            "config",
            "unset",
            "embedding.model",
            "--target",
            "local",
        ])
        .assert()
        .success();
    let unset_json = parse_stdout_json(&unset_assert);
    assert_eq!(unset_json["removed"], true);
    let local_config = fs::read_to_string(vault_root.join(".vulcan/config.local.toml"))
        .expect("local config should still exist");
    assert!(!local_config.contains("[embedding]"));
}

#[test]
fn config_alias_commands_manage_aliases_without_manual_toml() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    initialize_vulcan_dir(&vault_root);

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root.to_str().expect("utf-8"),
            "config",
            "alias",
            "set",
            "ship",
            "query --where 'status = shipped'",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Set aliases.ship"));

    let list_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root.to_str().expect("utf-8"),
            "--output",
            "json",
            "config",
            "alias",
            "list",
        ])
        .assert()
        .success();
    let list_json = parse_stdout_json(&list_assert);
    assert_eq!(
        list_json["config"]["ship"],
        "query --where 'status = shipped'"
    );

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root.to_str().expect("utf-8"),
            "config",
            "alias",
            "delete",
            "ship",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Removed aliases.ship"));
}

#[test]
fn config_permission_profile_commands_create_update_and_delete_profiles() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    initialize_vulcan_dir(&vault_root);

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root.to_str().expect("utf-8"),
            "config",
            "permissions",
            "profile",
            "create",
            "agent",
            "--clone",
            "readonly",
        ])
        .assert()
        .success();

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root.to_str().expect("utf-8"),
            "config",
            "permissions",
            "profile",
            "set",
            "agent",
            "network",
            "{ allow = true, domains = [\"example.com\"] }",
        ])
        .assert()
        .success();

    let show_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root.to_str().expect("utf-8"),
            "--output",
            "json",
            "config",
            "permissions",
            "profile",
            "show",
            "agent",
        ])
        .assert()
        .success();
    let show_json = parse_stdout_json(&show_assert);
    assert_eq!(show_json["config"]["read"], "all");
    assert_eq!(show_json["config"]["network"]["allow"], true);
    assert_eq!(show_json["config"]["network"]["domains"][0], "example.com");

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root.to_str().expect("utf-8"),
            "config",
            "permissions",
            "profile",
            "delete",
            "agent",
        ])
        .assert()
        .success();

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root.to_str().expect("utf-8"),
            "config",
            "permissions",
            "profile",
            "show",
            "agent",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown config section"));
}

#[test]
fn config_set_legacy_alias_keys_write_nested_storage_paths() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    initialize_vulcan_dir(&vault_root);

    let json_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "config",
            "set",
            "link_style",
            "markdown",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&json_assert);

    assert_eq!(json["key"], "link_style");
    assert_eq!(json["value"], "markdown");
    assert_eq!(json["config_path"], ".vulcan/config.toml");
    assert_eq!(json["created_config"], true);
    assert_eq!(json["updated"], true);
    assert_eq!(json["dry_run"], false);
    assert!(fs::read_to_string(vault_root.join(".vulcan/config.toml"))
        .expect("shared config should be readable")
        .contains("[links]\nstyle = \"markdown\"\n"));
}

#[test]
fn config_import_kanban_json_output_reports_mappings() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    initialize_vulcan_dir(&vault_root);
    fs::create_dir_all(vault_root.join(".obsidian/plugins/obsidian-kanban"))
        .expect("kanban plugin dir should be created");
    fs::write(
        vault_root.join(".obsidian/plugins/obsidian-kanban/data.json"),
        r#"{
          "date-trigger": "DUE",
          "time-trigger": "AT",
          "date-format": "DD/MM/YYYY",
          "time-format": "HH:mm:ss",
          "date-display-format": "ddd DD MMM",
          "metadata-keys": [
            {
              "metadataKey": "status",
              "label": "Status",
              "shouldHideLabel": true,
              "containsMarkdown": true
            }
          ],
          "archive-with-date": true,
          "archive-date-separator": " :: ",
          "new-card-insertion-method": "prepend",
          "show-search": false
        }"#,
    )
    .expect("kanban plugin config should be written");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "config",
            "import",
            "kanban",
            "--no-commit",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["plugin"], "kanban");
    assert_eq!(json["created_config"], true);
    assert_eq!(json["updated"], true);
    assert!(json["mappings"].as_array().is_some_and(|mappings| mappings
        .iter()
        .any(|mapping| mapping["target"] == "kanban.date_trigger" && mapping["value"] == "DUE")));

    let config =
        fs::read_to_string(vault_root.join(".vulcan/config.toml")).expect("config should exist");
    assert!(config.contains("[kanban]"));
    assert!(config.contains("date_trigger = \"DUE\""));
    assert!(config.contains("date_display_format = \"ddd DD MMM\""));
    assert!(config.contains("[[kanban.metadata_keys]]"));
    assert!(config.contains("metadata_key = \"status\""));
    assert!(config.contains("should_hide_label = true"));
    assert!(config.contains("contains_markdown = true"));
    assert!(config.contains("archive_date_separator = \" :: \""));
    assert!(config.contains("show_search = false"));
}

#[test]
fn config_import_periodic_notes_json_output_reports_mappings() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    initialize_vulcan_dir(&vault_root);
    fs::create_dir_all(vault_root.join(".obsidian/plugins/periodic-notes"))
        .expect("periodic plugin dir should be created");
    fs::write(
        vault_root.join(".obsidian/daily-notes.json"),
        r#"{
          "folder": "Journal/Daily",
          "format": "YYYY-MM-DD",
          "template": "Templates/Daily.md"
        }"#,
    )
    .expect("daily notes config should be written");
    fs::write(
        vault_root.join(".obsidian/plugins/periodic-notes/data.json"),
        r#"{
          "weekly": {
            "enabled": true,
            "folder": "Journal/Weekly",
            "format": "GGGG-[W]WW",
            "templatePath": "Templates/Weekly.md"
          },
          "monthly": {
            "enabled": true,
            "folder": "Journal/Monthly",
            "format": "YYYY-MM",
            "templatePath": "Templates/Monthly.md"
          }
        }"#,
    )
    .expect("periodic notes config should be written");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "config",
            "import",
            "periodic-notes",
            "--no-commit",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["plugin"], "periodic-notes");
    assert!(json["source_paths"].as_array().is_some_and(|paths| {
        paths
            .iter()
            .any(|path| path == ".obsidian/daily-notes.json")
            && paths
                .iter()
                .any(|path| path == ".obsidian/plugins/periodic-notes/data.json")
    }));
    assert!(json["mappings"].as_array().is_some_and(|mappings| mappings
        .iter()
        .any(|mapping| mapping["target"] == "periodic.daily.folder"
            && mapping["value"] == "Journal/Daily")));
    assert!(json["mappings"].as_array().is_some_and(|mappings| mappings
        .iter()
        .any(|mapping| mapping["target"] == "periodic.weekly.format"
            && mapping["value"] == "GGGG-[W]WW")));

    let rendered =
        fs::read_to_string(vault_root.join(".vulcan/config.toml")).expect("config should exist");
    assert!(rendered.contains("[periodic.daily]"));
    assert!(rendered.contains("folder = \"Journal/Daily\""));
    assert!(rendered.contains("template = \"Templates/Daily.md\""));
    assert!(rendered.contains("[periodic.weekly]"));
    assert!(rendered.contains("format = \"GGGG-[W]WW\""));
    assert!(rendered.contains("[periodic.monthly]"));
}

#[test]
fn daily_today_creates_note_from_template_and_updates_cache() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join(".vulcan/templates"))
        .expect("template dir should be created");
    fs::write(
        vault_root.join(".vulcan/templates/daily.md"),
        "# {{title}}\n\n## Log\n",
    )
    .expect("daily template should be written");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "daily",
            "today",
            "--no-edit",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);
    let path = json["path"]
        .as_str()
        .expect("path should be present")
        .to_string();
    let rendered = fs::read_to_string(vault_root.join(&path))
        .expect("daily note should be created")
        .replace("\r\n", "\n");

    assert!(json["created"].as_bool().is_some_and(|created| created));
    assert!(path.starts_with("Journal/Daily/"));
    assert!(rendered.contains("## Log"));

    let database =
        CacheDatabase::open(&VaultPaths::new(&vault_root)).expect("database should open");
    assert!(document_paths(&database)
        .iter()
        .any(|document_path| document_path == &path));
}

#[test]
fn daily_append_creates_note_and_appends_under_heading() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    initialize_vulcan_dir(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "daily",
            "append",
            "Called Alice",
            "--heading",
            "## Log",
            "--date",
            "2026-04-03",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);
    let path = json["path"]
        .as_str()
        .expect("path should be present")
        .to_string();
    let rendered = fs::read_to_string(vault_root.join(&path))
        .expect("daily note should be readable")
        .replace("\r\n", "\n");

    assert_eq!(path, "Journal/Daily/2026-04-03.md");
    assert!(json["created"].as_bool().is_some_and(|created| created));
    assert!(json["appended"].as_bool().is_some_and(|appended| appended));
    assert!(rendered.contains("## Log\n\nCalled Alice\n"));
}

#[test]
fn note_get_json_output_supports_composable_selectors() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    write_note_crud_sample(&vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "note",
            "get",
            "Dashboard",
            "--heading",
            "Tasks",
            "--match",
            "TODO",
            "--context",
            "1",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["path"], "Dashboard.md");
    assert_eq!(json["frontmatter"]["status"], "active");
    assert_eq!(
        json["content"],
        "Before\nTODO first\nContext after\n### Nested\nTODO nested\n"
    );
    assert_eq!(json["metadata"]["heading"], "Tasks");
    assert_eq!(json["metadata"]["match_pattern"], "TODO");
    assert_eq!(json["metadata"]["match_count"], 2);
    assert_eq!(json["metadata"]["section_id"], "dashboard/tasks@9");
    assert_eq!(json["metadata"]["total_lines"], 18);
    assert_eq!(json["metadata"]["has_more_before"], true);
    assert_eq!(json["metadata"]["has_more_after"], true);
    assert_eq!(json["metadata"]["line_spans"][0]["start_line"], 10);
    assert_eq!(json["metadata"]["line_spans"][0]["end_line"], 14);
}

#[test]
fn note_outline_and_section_reads_expose_semantic_spans() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    write_note_crud_sample(&vault_root);
    run_scan(&vault_root);
    let vault_root_str = vault_root
        .to_str()
        .expect("vault path should be valid utf-8")
        .to_string();

    let outline_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "--output",
            "json",
            "note",
            "outline",
            "Dashboard",
        ])
        .assert()
        .success();
    let outline = parse_stdout_json(&outline_assert);
    let tasks_section = outline["sections"]
        .as_array()
        .expect("sections should be an array")
        .iter()
        .find(|section| section["heading"] == "Tasks")
        .cloned()
        .expect("Tasks section should be present");

    assert_eq!(outline["path"], "Dashboard.md");
    assert_eq!(outline["total_lines"], 18);
    assert_eq!(outline["frontmatter_span"]["start_line"], 1);
    assert_eq!(tasks_section["id"], "dashboard/tasks@9");
    assert_eq!(tasks_section["start_line"], 9);
    assert_eq!(tasks_section["end_line"], 14);

    let section_id = tasks_section["id"]
        .as_str()
        .expect("section id should be a string");
    let get_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "--output",
            "json",
            "note",
            "get",
            "Dashboard",
            "--section",
            section_id,
        ])
        .assert()
        .success();
    let section_read = parse_stdout_json(&get_assert);

    assert_eq!(
        section_read["content"],
        "## Tasks\nBefore\nTODO first\nContext after\n### Nested\nTODO nested\n"
    );
    assert_eq!(section_read["metadata"]["section_id"], section_id);
    assert_eq!(section_read["metadata"]["line_spans"][0]["start_line"], 9);
    assert_eq!(section_read["metadata"]["line_spans"][0]["end_line"], 14);
}

#[test]
fn note_outline_supports_section_scopes_and_relative_depth_limits() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    write_note_crud_sample(&vault_root);
    run_scan(&vault_root);
    let vault_root_str = vault_root
        .to_str()
        .expect("vault path should be valid utf-8")
        .to_string();

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "--output",
            "json",
            "note",
            "outline",
            "Dashboard",
            "--section",
            "dashboard/tasks@9",
            "--depth",
            "1",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["path"], "Dashboard.md");
    assert_eq!(json["scope_section"]["id"], "dashboard/tasks@9");
    let sections = json["sections"]
        .as_array()
        .expect("sections should be an array");
    assert_eq!(sections.len(), 1);
    assert_eq!(sections[0]["heading"], "Nested");
    assert_eq!(sections[0]["id"], "dashboard/tasks/nested@13");
    assert!(json["block_refs"]
        .as_array()
        .expect("block refs should be an array")
        .is_empty());
}

#[test]
fn note_outline_human_output_shows_heading_markers_and_nested_metadata() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    write_note_crud_sample(&vault_root);
    run_scan(&vault_root);

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--color",
            "never",
            "note",
            "outline",
            "Dashboard",
        ])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("Sections")
                .and(predicate::str::contains("# Dashboard"))
                .and(predicate::str::contains("## Tasks"))
                .and(predicate::str::contains("lines: 9-14"))
                .and(predicate::str::contains("id: dashboard/tasks@9"))
                .and(predicate::str::contains("Block refs")),
        );
}

#[test]
fn note_outline_human_output_uses_color_when_requested() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    write_note_crud_sample(&vault_root);
    run_scan(&vault_root);

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--color",
            "always",
            "note",
            "outline",
            "Dashboard",
        ])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("\u{1b}[36m#")
                .and(predicate::str::contains("\u{1b}[1mDashboard")),
        );
}

#[test]
#[allow(clippy::too_many_lines)]
fn note_outline_get_and_patch_support_absolute_markdown_paths_outside_vaults() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let workspace = temp_dir.path().join("workspace");
    let docs_dir = temp_dir.path().join("docs");
    fs::create_dir_all(&workspace).expect("workspace dir should exist");
    fs::create_dir_all(&docs_dir).expect("docs dir should exist");
    let note_path = docs_dir.join("Large.md");
    fs::write(
        &note_path,
        concat!(
            "# Root\n",
            "Body\n",
            "^root-block\n",
            "## Child\n",
            "Child body\n",
            "### Grandchild\n",
            "Grandchild body\n",
        ),
    )
    .expect("external markdown file should be written");
    let note_path_str = note_path
        .to_str()
        .expect("note path should be valid utf-8")
        .to_string();

    let outline_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .current_dir(&workspace)
        .args([
            "--output",
            "json",
            "note",
            "outline",
            &note_path_str,
            "--section",
            "root@1",
            "--depth",
            "1",
        ])
        .assert()
        .success();
    let outline = parse_stdout_json(&outline_assert);
    assert_eq!(outline["path"], note_path_str);
    assert_eq!(outline["scope_section"]["id"], "root@1");
    assert_eq!(outline["sections"][0]["id"], "root/child@4");
    assert_eq!(
        outline["sections"]
            .as_array()
            .expect("sections should be an array")
            .len(),
        1
    );

    let get_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .current_dir(&workspace)
        .args([
            "--output",
            "json",
            "note",
            "get",
            &note_path_str,
            "--section",
            "root/child@4",
        ])
        .assert()
        .success();
    let get_json = parse_stdout_json(&get_assert);
    assert_eq!(get_json["path"], note_path_str);
    assert_eq!(
        get_json["content"],
        "## Child\nChild body\n### Grandchild\nGrandchild body\n"
    );

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .current_dir(&workspace)
        .args([
            "note",
            "patch",
            &note_path_str,
            "--find",
            "Child body",
            "--replace",
            "Kid body",
            "--dry-run",
        ])
        .assert()
        .success()
        .stdout(
            predicate::str::contains(&note_path_str)
                .and(predicate::str::contains("Child body -> Kid body")),
        );
    assert_eq!(
        fs::read_to_string(&note_path).expect("external markdown file should remain readable"),
        concat!(
            "# Root\n",
            "Body\n",
            "^root-block\n",
            "## Child\n",
            "Child body\n",
            "### Grandchild\n",
            "Grandchild body\n",
        )
    );
}

#[test]
fn note_get_human_output_adds_line_numbers_unless_raw() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    write_note_crud_sample(&vault_root);
    run_scan(&vault_root);
    let vault_root_str = vault_root
        .to_str()
        .expect("vault path should be valid utf-8")
        .to_string();

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "note",
            "get",
            "Dashboard",
            "--match",
            "TODO",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("11: TODO first").and(predicate::str::contains("--")));

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "note",
            "get",
            "Dashboard",
            "--match",
            "TODO",
            "--raw",
        ])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("TODO first")
                .and(predicate::str::contains("TODO nested"))
                .and(predicate::str::contains("11:").not())
                .and(predicate::str::contains("--").not()),
        );
}

#[test]
fn note_get_html_mode_renders_selected_markdown() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    write_note_crud_sample(&vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "note",
            "get",
            "Dashboard",
            "--mode",
            "html",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);
    let html = json["content"]
        .as_str()
        .expect("html content should be serialized as a string");

    assert_eq!(json["metadata"]["mode"], "html");
    assert!(html.contains("<h1 id=\"dashboard\">Dashboard</h1>"));
    assert!(html.contains("<h2 id=\"tasks\">Tasks</h2>"));
    assert!(html.contains("TODO first"));
    assert!(!html.contains("status: active"));
}

#[test]
fn note_get_markdown_output_preserves_raw_markdown() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    write_note_crud_sample(&vault_root);
    run_scan(&vault_root);

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "markdown",
            "note",
            "get",
            "Dashboard",
        ])
        .assert()
        .success()
        .stdout(
            predicate::str::starts_with("---\nstatus: active\n")
                .and(predicate::str::contains("# Dashboard"))
                .and(predicate::str::contains("## Tasks"))
                .and(predicate::str::contains("^done-item"))
                .and(predicate::str::contains("1: ").not()),
        );
}

#[test]
fn note_patch_supports_semantic_scope_selectors() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    write_note_crud_sample(&vault_root);
    run_scan(&vault_root);
    let vault_root_str = vault_root
        .to_str()
        .expect("vault path should be valid utf-8")
        .to_string();

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "--output",
            "json",
            "note",
            "patch",
            "Dashboard",
            "--heading",
            "Nested",
            "--find",
            "TODO",
            "--replace",
            "DONE",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);
    let updated = fs::read_to_string(vault_root.join("Dashboard.md"))
        .expect("Dashboard.md should remain readable")
        .replace("\r\n", "\n");

    assert_eq!(json["path"], "Dashboard.md");
    assert_eq!(json["heading"], "Nested");
    assert_eq!(json["section_id"], "dashboard/tasks/nested@13");
    assert_eq!(json["match_count"], 1);
    assert_eq!(json["line_spans"][0]["start_line"], 13);
    assert_eq!(json["line_spans"][0]["end_line"], 14);
    assert!(updated.contains("TODO first"));
    assert!(updated.contains("DONE nested"));
    assert!(!updated.contains("TODO nested"));
}

#[test]
fn note_checkbox_updates_markdown_checkboxes_by_line_and_scope_index() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    write_note_checkbox_sample(&vault_root);
    run_scan(&vault_root);
    let vault_root_str = vault_root
        .to_str()
        .expect("vault path should be valid utf-8")
        .to_string();

    let first_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "--output",
            "json",
            "note",
            "checkbox",
            "Checklist",
            "--line",
            "3",
        ])
        .assert()
        .success();
    let first_json = parse_stdout_json(&first_assert);

    assert_eq!(first_json["path"], "Checklist.md");
    assert_eq!(first_json["line_number"], 3);
    assert_eq!(first_json["checkbox_index"], 1);
    assert_eq!(first_json["state"], "checked");
    assert_eq!(first_json["before"], "- [ ] Alpha");
    assert_eq!(first_json["after"], "- [x] Alpha");

    let outline_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "--output",
            "json",
            "note",
            "outline",
            "Checklist",
        ])
        .assert()
        .success();
    let outline = parse_stdout_json(&outline_assert);
    let phase_a_id = outline["sections"]
        .as_array()
        .expect("sections should be an array")
        .iter()
        .find(|section| section["heading"] == "Phase A")
        .and_then(|section| section["id"].as_str())
        .expect("Phase A section should be present")
        .to_string();

    let second_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "--output",
            "json",
            "note",
            "checkbox",
            "Checklist",
            "--section",
            &phase_a_id,
            "--index",
            "2",
            "--state",
            "unchecked",
        ])
        .assert()
        .success();
    let second_json = parse_stdout_json(&second_assert);
    let rendered = fs::read_to_string(vault_root.join("Checklist.md"))
        .expect("Checklist.md should be readable")
        .replace("\r\n", "\n");

    assert_eq!(second_json["path"], "Checklist.md");
    assert_eq!(second_json["section_id"], phase_a_id);
    assert_eq!(second_json["checkbox_index"], 2);
    assert_eq!(second_json["line_number"], 4);
    assert_eq!(second_json["state"], "unchecked");
    assert_eq!(second_json["before"], "- [x] Beta");
    assert_eq!(second_json["after"], "- [ ] Beta");
    assert!(rendered.contains("- [x] Alpha"));
    assert!(rendered.contains("- [ ] Beta"));
    assert!(rendered.contains("- [ ] Gamma"));
}

#[test]
fn note_checkbox_supports_absolute_markdown_paths_outside_vaults() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let workspace = temp_dir.path().join("workspace");
    let docs_dir = temp_dir.path().join("docs");
    fs::create_dir_all(&workspace).expect("workspace dir should exist");
    fs::create_dir_all(&docs_dir).expect("docs dir should exist");
    let note_path = docs_dir.join("Checklist.md");
    fs::write(&note_path, "# Tasks\n- [ ] Ship it\n").expect("external markdown file should exist");
    let note_path_str = note_path
        .to_str()
        .expect("note path should be valid utf-8")
        .to_string();

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .current_dir(&workspace)
        .args([
            "note",
            "checkbox",
            &note_path_str,
            "--line",
            "2",
            "--state",
            "checked",
            "--dry-run",
        ])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("Dry run: would set checkbox")
                .and(predicate::str::contains(&note_path_str)),
        );
    assert_eq!(
        fs::read_to_string(&note_path).expect("external markdown file should remain readable"),
        "# Tasks\n- [ ] Ship it\n"
    );

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .current_dir(&workspace)
        .args([
            "--output",
            "json",
            "note",
            "checkbox",
            &note_path_str,
            "--line",
            "2",
            "--state",
            "checked",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["path"], note_path_str);
    assert_eq!(json["state"], "checked");
    assert_eq!(
        fs::read_to_string(&note_path).expect("external markdown file should be updated"),
        "# Tasks\n- [x] Ship it\n"
    );
}

#[test]
fn note_info_json_output_reports_summary_metadata() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    write_note_crud_sample(&vault_root);
    fs::write(vault_root.join("Reference.md"), "# Reference\n")
        .expect("reference note should exist");
    fs::write(vault_root.join("Inbox.md"), "[[Dashboard]]\n").expect("backlink note should exist");
    let dashboard = fs::read_to_string(vault_root.join("Dashboard.md"))
        .expect("dashboard note should be readable");
    fs::write(
        vault_root.join("Dashboard.md"),
        format!("{dashboard}\n[[Reference]]\n"),
    )
    .expect("dashboard note should be updated");
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "note",
            "info",
            "Dashboard.md",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["path"], "Dashboard.md");
    assert_eq!(json["matched_by"], "path");
    assert_eq!(json["heading_count"], 4);
    assert_eq!(json["outgoing_link_count"], 1);
    assert_eq!(json["backlink_count"], 1);
    assert_eq!(json["alias_count"], 0);
    assert_eq!(json["tag_count"], 1);
    assert_eq!(json["tags"], serde_json::json!(["project"]));
    assert_eq!(
        json["frontmatter_keys"],
        serde_json::json!(["status", "tags"])
    );
    assert!(json["file_size"].as_i64().is_some_and(|size| size > 0));
    assert!(json["word_count"].as_u64().is_some_and(|count| count > 0));
    assert!(json["created_at_ms"]
        .as_i64()
        .is_some_and(|value| value > 0));
    assert!(json["modified_at_ms"]
        .as_i64()
        .is_some_and(|value| value > 0));
    assert!(json["created_at"]
        .as_str()
        .is_some_and(|value| !value.is_empty()));
    assert!(json["modified_at"]
        .as_str()
        .is_some_and(|value| !value.is_empty()));
}

#[test]
fn note_set_preserves_frontmatter_and_reports_check_diagnostics() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    write_note_crud_sample(&vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "note",
            "set",
            "Dashboard",
            "--no-frontmatter",
            "--check",
        ])
        .write_stdin("Replacement line\n\n[[Missing]]\n")
        .assert()
        .success();
    let json = parse_stdout_json(&assert);
    let rendered = fs::read_to_string(vault_root.join("Dashboard.md"))
        .expect("Dashboard.md should be readable")
        .replace("\r\n", "\n");

    assert_eq!(json["path"], "Dashboard.md");
    assert_eq!(json["preserved_frontmatter"], true);
    assert_eq!(json["checked"], true);
    assert!(json["diagnostics"]
        .as_array()
        .is_some_and(
            |diagnostics| diagnostics.iter().any(|diagnostic| diagnostic["message"]
                .as_str()
                .is_some_and(|message| message.contains("Unresolved link target")))
        ));
    assert!(rendered.starts_with("---\nstatus: active\ntags:\n  - project\n---\n"));
    assert!(rendered.contains("Replacement line"));
    assert!(!rendered.contains("Intro line"));
}

#[test]
fn note_create_uses_template_and_frontmatter_bindings() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join(".vulcan/templates"))
        .expect("template directory should be created");
    fs::write(
        vault_root.join(".vulcan/templates/brief.md"),
        concat!(
            "---\n",
            "status: draft\n",
            "tags:\n",
            "  - seed\n",
            "---\n",
            "# {{title}}\n",
            "\n",
            "Template body\n",
        ),
    )
    .expect("template should be written");
    run_scan(&vault_root);
    let vault_root_str = vault_root
        .to_str()
        .expect("vault path should be valid utf-8")
        .to_string();

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "--output",
            "json",
            "note",
            "create",
            "Inbox/Idea",
            "--template",
            "brief",
            "--frontmatter",
            "reviewed=true",
        ])
        .write_stdin("Extra details\n")
        .assert()
        .success();
    let json = parse_stdout_json(&assert);
    let rendered = fs::read_to_string(vault_root.join("Inbox/Idea.md"))
        .expect("created note should be readable")
        .replace("\r\n", "\n");

    assert_eq!(json["path"], "Inbox/Idea.md");
    assert_eq!(json["template"], "brief");
    assert_eq!(json["engine"], "native");
    assert!(rendered.contains("status: draft"));
    assert!(rendered.contains("reviewed: true"));
    assert!(rendered.contains("# Idea"));
    assert!(rendered.contains("Template body\n\nExtra details\n"));

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["--vault", &vault_root_str, "note", "create", "Inbox/Idea"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("already exists"));
}

#[test]
fn note_append_under_heading_reports_check_diagnostics() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    write_note_crud_sample(&vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "note",
            "append",
            "Dashboard",
            "-",
            "--heading",
            "## Done",
            "--check",
        ])
        .write_stdin("[[Missing]]\n")
        .assert()
        .success();
    let json = parse_stdout_json(&assert);
    let rendered = fs::read_to_string(vault_root.join("Dashboard.md"))
        .expect("Dashboard.md should be readable")
        .replace("\r\n", "\n");

    assert_eq!(json["path"], "Dashboard.md");
    assert_eq!(json["heading"], "## Done");
    assert!(json["diagnostics"]
        .as_array()
        .is_some_and(
            |diagnostics| diagnostics.iter().any(|diagnostic| diagnostic["message"]
                .as_str()
                .is_some_and(|message| message.contains("Unresolved link target")))
        ));
    assert!(rendered.contains("## Done\n\n[[Missing]]\n\nFinished line"));
}

#[test]
fn note_append_prepend_renders_quickadd_value_tokens() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    write_note_crud_sample(&vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "note",
            "append",
            "Dashboard",
            "- {{VALUE:title|case:slug}}",
            "--prepend",
            "--var",
            "title=Release Planning",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);
    let rendered = fs::read_to_string(vault_root.join("Dashboard.md"))
        .expect("Dashboard.md should be readable")
        .replace("\r\n", "\n");

    assert_eq!(json["path"], "Dashboard.md");
    assert_eq!(json["mode"], "prepend");
    assert!(rendered.starts_with(
        "---\nstatus: active\ntags:\n  - project\n---\n- release-planning\n\n# Dashboard\n"
    ));
}

#[test]
fn note_append_periodic_creates_note_and_renders_quickadd_tokens() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    initialize_vulcan_dir(&vault_root);

    let assert = cargo_vulcan_fixed_now()
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "note",
            "append",
            "- {{VALUE:title|case:slug}} due {{VDATE:due,YYYY-MM-DD}}",
            "--periodic",
            "daily",
            "--date",
            "2026-04-03",
            "--var",
            "title=Release Planning",
            "--var",
            "due=tomorrow",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);
    let rendered = fs::read_to_string(vault_root.join("Journal/Daily/2026-04-03.md"))
        .expect("daily note should be readable")
        .replace("\r\n", "\n");

    assert_eq!(json["path"], "Journal/Daily/2026-04-03.md");
    assert_eq!(json["mode"], "append");
    assert_eq!(json["period_type"], "daily");
    assert_eq!(json["reference_date"], "2026-04-03");
    assert!(json["created"].as_bool().is_some_and(|created| created));
    assert!(rendered.contains("- release-planning due 2026-04-05\n"));
}

#[test]
fn today_alias_json_output_opens_daily_note() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    initialize_vulcan_dir(&vault_root);

    let assert = cargo_vulcan_fixed_now()
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "today",
            "--no-edit",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["period_type"], "daily");
    assert_eq!(json["reference_date"], "2026-04-04");
    assert_eq!(json["path"], "Journal/Daily/2026-04-04.md");
    assert!(json["created"].as_bool().is_some_and(|created| created));
    assert!(json["opened_editor"]
        .as_bool()
        .is_some_and(|opened| !opened));
    assert!(vault_root.join("Journal/Daily/2026-04-04.md").exists());
}

#[test]
fn note_append_uses_quickadd_global_variables_from_config() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    write_note_crud_sample(&vault_root);
    fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should be created");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        r#"[quickadd]
global_variables = { agenda = "- {{VALUE:title|case:slug}} due {{VDATE:due,YYYY-MM-DD}}" }
"#,
    )
    .expect("quickadd config should be written");
    run_scan(&vault_root);

    let assert = cargo_vulcan_fixed_now()
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "note",
            "append",
            "Dashboard",
            "{{GLOBAL_VAR:AGENDA}}",
            "--var",
            "title=Release Planning",
            "--var",
            "due=tomorrow",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);
    let rendered = fs::read_to_string(vault_root.join("Dashboard.md"))
        .expect("Dashboard.md should be readable")
        .replace("\r\n", "\n");

    assert_eq!(json["path"], "Dashboard.md");
    assert_eq!(json["mode"], "append");
    assert!(rendered.contains("- release-planning due 2026-04-05\n"));
}

#[test]
fn note_patch_enforces_match_safety_and_supports_regex_dry_runs() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault root should be created");
    fs::write(
        vault_root.join("Patch.md"),
        "TODO 2026-04-03\nTODO 2026-05-01\n",
    )
    .expect("Patch.md should be written");
    run_scan(&vault_root);
    let vault_root_str = vault_root
        .to_str()
        .expect("vault path should be valid utf-8")
        .to_string();

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "note",
            "patch",
            "Patch",
            "--find",
            "TODO",
            "--replace",
            "DONE",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("rerun with --all"));

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "--output",
            "json",
            "note",
            "patch",
            "Patch",
            "--find",
            "/2026-\\d{2}-\\d{2}/",
            "--replace",
            "DATE",
            "--all",
            "--dry-run",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);
    let rendered = fs::read_to_string(vault_root.join("Patch.md"))
        .expect("Patch.md should be readable")
        .replace("\r\n", "\n");

    assert_eq!(json["path"], "Patch.md");
    assert_eq!(json["dry_run"], true);
    assert_eq!(json["regex"], true);
    assert_eq!(json["match_count"], 2);
    assert_eq!(json["changes"][0]["before"], "2026-04-03");
    assert_eq!(json["changes"][0]["after"], "DATE");
    assert_eq!(rendered, "TODO 2026-04-03\nTODO 2026-05-01\n");
}

#[test]
fn note_history_json_output_filters_commits_to_one_note() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault dir should be created");
    init_git_repo(&vault_root);
    fs::write(vault_root.join("Dashboard.md"), "# Dashboard\n")
        .expect("dashboard note should exist");
    fs::write(vault_root.join("Other.md"), "# Other\n").expect("other note should exist");
    commit_all(&vault_root, "Initial");

    fs::write(vault_root.join("Dashboard.md"), "# Dashboard\nUpdated\n")
        .expect("dashboard note should update");
    run_git_ok(&vault_root, &["add", "Dashboard.md"]);
    run_git_ok(&vault_root, &["commit", "-m", "Update dashboard"]);

    fs::write(vault_root.join("Other.md"), "# Other\nUpdated\n").expect("other note should update");
    run_git_ok(&vault_root, &["add", "Other.md"]);
    run_git_ok(&vault_root, &["commit", "-m", "Update other"]);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "note",
            "history",
            "Dashboard.md",
            "--limit",
            "10",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);
    let entries = json["entries"]
        .as_array()
        .expect("entries should be an array");

    assert_eq!(json["path"], "Dashboard.md");
    assert_eq!(json["limit"], 10);
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0]["summary"], "Update dashboard");
    assert_eq!(entries[1]["summary"], "Initial");
}

#[test]
fn daily_list_json_includes_events_in_range() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join(".vulcan")).expect("config dir should be created");
    fs::create_dir_all(vault_root.join("Journal/Daily")).expect("daily dir should be created");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        "[periodic.daily]\nschedule_heading = \"Schedule\"\n",
    )
    .expect("config should be written");
    fs::write(
        vault_root.join("Journal/Daily/2026-04-03.md"),
        "# 2026-04-03\n\n## Schedule\n- 09:00 Team standup\n- 14:00-15:30 Dentist #personal\n",
    )
    .expect("first daily note should be written");
    fs::write(
        vault_root.join("Journal/Daily/2026-04-04.md"),
        "# 2026-04-04\n\n## Schedule\n- all-day Company offsite\n",
    )
    .expect("second daily note should be written");
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "daily",
            "list",
            "--from",
            "2026-04-03",
            "--to",
            "2026-04-04",
        ])
        .assert()
        .success();
    let rows = parse_stdout_json_lines(&assert);

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["date"], "2026-04-03");
    assert_eq!(rows[0]["event_count"], 2);
    assert!(rows[0]["events"].as_array().is_some_and(|events| {
        events.iter().any(|event| event["title"] == "Team standup")
            && events.iter().any(|event| event["title"] == "Dentist")
    }));
    assert_eq!(rows[1]["date"], "2026-04-04");
    assert_eq!(rows[1]["event_count"], 1);
    assert_eq!(rows[1]["events"][0]["start_time"], "all-day");
}

#[test]
fn daily_export_ics_writes_calendar_file() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join(".vulcan")).expect("config dir should be created");
    fs::create_dir_all(vault_root.join("Journal/Daily")).expect("daily dir should be created");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        "[periodic.daily]\nschedule_heading = \"Schedule\"\n",
    )
    .expect("config should be written");
    fs::write(
        vault_root.join("Journal/Daily/2026-04-03.md"),
        "# 2026-04-03\n\n## Schedule\n- 09:00-10:00 Team standup @location(Zoom)\n- 14:00 Dentist #personal\n",
    )
    .expect("first daily note should be written");
    fs::write(
        vault_root.join("Journal/Daily/2026-04-04.md"),
        "# 2026-04-04\n\n## Schedule\n- all-day Company offsite\n",
    )
    .expect("second daily note should be written");
    run_scan(&vault_root);

    let calendar_path = vault_root.join("exports/journal.ics");
    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "daily",
            "export-ics",
            "--from",
            "2026-04-03",
            "--to",
            "2026-04-04",
            "--path",
            calendar_path
                .to_str()
                .expect("calendar path should be valid utf-8"),
            "--calendar-name",
            "Journal",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);
    let rendered = fs::read_to_string(&calendar_path).expect("calendar file should be written");

    assert_eq!(json["from"], "2026-04-03");
    assert_eq!(json["to"], "2026-04-04");
    assert_eq!(json["calendar_name"], "Journal");
    assert_eq!(json["note_count"], 2);
    assert_eq!(json["event_count"], 3);
    assert_eq!(json["path"], calendar_path.to_string_lossy().as_ref());
    assert!(rendered.contains("BEGIN:VCALENDAR\r\n"));
    assert!(rendered.contains("SUMMARY:Team standup\r\n"));
    assert!(rendered.contains("LOCATION:Zoom\r\n"));
    assert!(rendered.contains("DTSTART;VALUE=DATE:20260404\r\n"));
}

#[test]
fn git_status_json_output_lists_only_vault_changes() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should be created");
    init_git_repo(&vault_root);
    fs::write(vault_root.join("Home.md"), "home\n").expect("home note should be written");
    fs::write(vault_root.join(".vulcan/cache.db"), "cache\n").expect("cache should be written");
    commit_all(&vault_root, "Initial");

    fs::write(vault_root.join("Home.md"), "home updated\n").expect("home note should update");
    run_git_ok(&vault_root, &["add", "Home.md"]);
    fs::write(vault_root.join("Draft.md"), "draft\n").expect("draft note should be written");
    fs::write(vault_root.join(".vulcan/cache.db"), "cache2\n").expect("cache should update");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "git",
            "status",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["clean"], false);
    assert_eq!(json["staged"], serde_json::json!(["Home.md"]));
    assert_eq!(json["unstaged"], serde_json::json!([]));
    assert_eq!(json["untracked"], serde_json::json!(["Draft.md"]));
}

#[test]
fn git_log_json_output_lists_recent_commits() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault dir should be created");
    init_git_repo(&vault_root);
    fs::write(vault_root.join("Home.md"), "home\n").expect("home note should be written");
    commit_all(&vault_root, "Add home");
    fs::write(vault_root.join("Other.md"), "other\n").expect("other note should be written");
    commit_all(&vault_root, "Add other");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "git",
            "log",
            "--limit",
            "2",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["limit"], 2);
    assert_eq!(json["entries"][0]["summary"], "Add other");
    assert_eq!(json["entries"][1]["summary"], "Add home");
}

#[test]
fn git_diff_json_output_reports_changed_paths_and_filters_internal_state() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should be created");
    init_git_repo(&vault_root);
    fs::write(vault_root.join("Home.md"), "home\n").expect("home note should be written");
    fs::write(vault_root.join(".vulcan/cache.db"), "cache\n").expect("cache should be written");
    commit_all(&vault_root, "Initial");

    fs::write(vault_root.join("Home.md"), "home updated\n").expect("home note should update");
    fs::write(vault_root.join(".vulcan/cache.db"), "cache2\n").expect("cache should update");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "git",
            "diff",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["path"], Value::Null);
    assert_eq!(json["changed_paths"], serde_json::json!(["Home.md"]));
    assert!(json["diff"]
        .as_str()
        .is_some_and(|diff| diff.contains("Home.md") && !diff.contains(".vulcan/cache.db")));
}

#[test]
fn git_commit_json_output_stages_vault_files_but_skips_internal_state() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should be created");
    init_git_repo(&vault_root);
    fs::write(vault_root.join("Home.md"), "home\n").expect("home note should be written");
    fs::write(vault_root.join(".vulcan/cache.db"), "cache\n").expect("cache should be written");
    commit_all(&vault_root, "Initial");

    fs::write(vault_root.join("Home.md"), "home updated\n").expect("home note should update");
    fs::write(vault_root.join(".vulcan/cache.db"), "cache2\n").expect("cache should update");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "git",
            "commit",
            "-m",
            "Update home",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);
    let status = ProcessCommand::new("git")
        .arg("-C")
        .arg(&vault_root)
        .args(["status", "--short"])
        .output()
        .expect("git status should launch");

    assert_eq!(json["committed"], true);
    assert_eq!(json["message"], "Update home");
    assert_eq!(json["files"], serde_json::json!(["Home.md"]));
    assert!(json["sha"].as_str().is_some_and(|sha| !sha.is_empty()));
    let rendered_status =
        String::from_utf8(status.stdout).expect("git status output should be valid utf-8");
    assert!(rendered_status.contains(".vulcan/cache.db"));
    assert!(!rendered_status.contains("Home.md"));
}

#[test]
fn git_blame_json_output_returns_line_metadata() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault dir should be created");
    init_git_repo(&vault_root);
    fs::write(vault_root.join("Home.md"), "alpha\nbeta\n").expect("home note should be written");
    commit_all(&vault_root, "Initial");
    fs::write(vault_root.join("Home.md"), "alpha\nbeta updated\n")
        .expect("home note should update");
    run_git_ok(&vault_root, &["add", "Home.md"]);
    run_git_ok(&vault_root, &["commit", "-m", "Update beta"]);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "git",
            "blame",
            "Home.md",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["path"], "Home.md");
    assert_eq!(json["lines"][0]["line_number"], 1);
    assert_eq!(json["lines"][0]["line"], "alpha");
    assert_eq!(json["lines"][1]["line_number"], 2);
    assert_eq!(json["lines"][1]["line"], "beta updated");
    assert_eq!(json["lines"][1]["summary"], "Update beta");
    assert_eq!(json["lines"][1]["author_name"], "Vulcan Test");
}

#[test]
fn git_help_documents_sandboxed_operations() {
    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["git", "--help"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("status")
                .and(predicate::str::contains("log"))
                .and(predicate::str::contains("diff"))
                .and(predicate::str::contains("commit"))
                .and(predicate::str::contains("blame"))
                .and(predicate::str::contains("`.vulcan/`")),
        );
}

#[test]
fn web_search_json_output_uses_configured_backend_and_env_key() {
    let server = MockWebServer::spawn();
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join(".vulcan")).expect("config dir should be created");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        format!(
            "[web.search]\nbackend = \"kagi\"\napi_key_env = \"TEST_KAGI_TOKEN\"\nbase_url = \"{}\"\n",
            server.url("/api/v0/search")
        ),
    )
    .expect("config should be written");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .env("TEST_KAGI_TOKEN", "test-token")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "web",
            "search",
            "release notes",
            "--limit",
            "2",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);
    server.shutdown();

    assert_eq!(json["backend"], "kagi");
    assert_eq!(json["query"], "release notes");
    assert_eq!(json["results"][0]["title"], "Release Notes");
    assert_eq!(json["results"][1]["url"], "https://example.com/status");
}

#[test]
fn web_search_defaults_to_duckduckgo_without_api_key() {
    let server = MockWebServer::spawn();
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join(".vulcan")).expect("config dir should be created");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        format!("[web.search]\nbase_url = \"{}\"\n", server.url("/html/")),
    )
    .expect("config should be written");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "web",
            "search",
            "release notes",
            "--limit",
            "2",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);
    server.shutdown();

    assert_eq!(json["backend"], "duckduckgo");
    assert_eq!(json["results"][0]["title"], "Release Notes");
    assert_eq!(json["results"][1]["url"], "https://example.com/status");
}

#[test]
fn web_search_auto_falls_back_to_duckduckgo_without_api_keys() {
    let server = MockWebServer::spawn();
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join(".vulcan")).expect("config dir should be created");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        format!(
            "[web.search]\nbackend = \"auto\"\nbase_url = \"{}\"\n",
            server.url("/html/")
        ),
    )
    .expect("config should be written");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .env_remove("KAGI_API_KEY")
        .env_remove("EXA_API_KEY")
        .env_remove("TAVILY_API_KEY")
        .env_remove("BRAVE_API_KEY")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "web",
            "search",
            "release notes",
            "--limit",
            "2",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);
    server.shutdown();

    assert_eq!(json["backend"], "duckduckgo");
    assert_eq!(json["results"][0]["title"], "Release Notes");
    assert_eq!(json["results"][1]["snippet"], "Current project status.");
}

#[test]
fn web_search_exa_backend_uses_x_api_key_header_and_parses_text_field() {
    let server = MockWebServer::spawn();
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join(".vulcan")).expect("config dir should be created");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        format!(
            "[web.search]\nbackend = \"exa\"\napi_key_env = \"TEST_EXA_KEY\"\nbase_url = \"{}\"\n",
            server.url("/exa/search")
        ),
    )
    .expect("config should be written");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .env("TEST_EXA_KEY", "test-exa-key")
        .args([
            "--vault",
            vault_root.to_str().expect("vault path should be utf-8"),
            "--output",
            "json",
            "web",
            "search",
            "release notes",
            "--limit",
            "2",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);
    server.shutdown();

    assert_eq!(json["backend"], "exa");
    assert_eq!(json["query"], "release notes");
    assert_eq!(json["results"][0]["title"], "Release Notes");
    assert_eq!(json["results"][0]["url"], "https://example.com/release");
    assert_eq!(json["results"][1]["url"], "https://example.com/status");
}

#[test]
fn web_search_tavily_backend_posts_json_and_parses_content_field() {
    let server = MockWebServer::spawn();
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join(".vulcan")).expect("config dir should be created");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        format!(
            "[web.search]\nbackend = \"tavily\"\napi_key_env = \"TEST_TAVILY_KEY\"\nbase_url = \"{}\"\n",
            server.url("/tavily/search")
        ),
    )
    .expect("config should be written");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .env("TEST_TAVILY_KEY", "test-tavily-key")
        .args([
            "--vault",
            vault_root.to_str().expect("vault path should be utf-8"),
            "--output",
            "json",
            "web",
            "search",
            "release notes",
            "--limit",
            "2",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);
    server.shutdown();

    assert_eq!(json["backend"], "tavily");
    assert_eq!(json["results"][0]["title"], "Release Notes");
    assert_eq!(json["results"][1]["snippet"], "Current project status.");
}

#[test]
fn web_search_brave_backend_uses_subscription_token_header_and_parses_description() {
    let server = MockWebServer::spawn();
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join(".vulcan")).expect("config dir should be created");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        format!(
            "[web.search]\nbackend = \"brave\"\napi_key_env = \"TEST_BRAVE_KEY\"\nbase_url = \"{}\"\n",
            server.url("/brave/search")
        ),
    )
    .expect("config should be written");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .env("TEST_BRAVE_KEY", "test-brave-key")
        .args([
            "--vault",
            vault_root.to_str().expect("vault path should be utf-8"),
            "--output",
            "json",
            "web",
            "search",
            "release notes",
            "--limit",
            "2",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);
    server.shutdown();

    assert_eq!(json["backend"], "brave");
    assert_eq!(json["results"][0]["title"], "Release Notes");
    assert_eq!(json["results"][0]["url"], "https://example.com/release");
    assert_eq!(json["results"][1]["snippet"], "Current project status.");
}

#[test]
fn web_search_ollama_backend_uses_bearer_auth_and_parses_content_field() {
    let server = MockWebServer::spawn();
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join(".vulcan")).expect("config dir should be created");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        format!(
            "[web.search]\nbackend = \"ollama\"\napi_key_env = \"TEST_OLLAMA_KEY\"\nbase_url = \"{}\"\n",
            server.url("/api/web_search")
        ),
    )
    .expect("config should be written");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .env("TEST_OLLAMA_KEY", "test-ollama-key")
        .args([
            "--vault",
            vault_root.to_str().expect("vault path should be utf-8"),
            "--output",
            "json",
            "web",
            "search",
            "release notes",
            "--limit",
            "2",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);
    server.shutdown();

    assert_eq!(json["backend"], "ollama");
    assert_eq!(json["query"], "release notes");
    assert_eq!(json["results"][0]["title"], "Release Notes");
    assert_eq!(json["results"][0]["url"], "https://example.com/release");
    assert_eq!(json["results"][1]["snippet"], "Current project status.");
}

#[test]
fn web_search_disabled_backend_fails_cleanly() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join(".vulcan")).expect("config dir should be created");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        "[web.search]\nbackend = \"disabled\"\n",
    )
    .expect("config should be written");

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root.to_str().expect("vault path should be utf-8"),
            "web",
            "search",
            "release notes",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("web search is disabled"));
}

#[test]
fn web_search_auto_prefers_api_key_backends_over_duckduckgo() {
    // With EXA_API_KEY set, auto mode should prefer Exa over DuckDuckGo.
    let server = MockWebServer::spawn();
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join(".vulcan")).expect("config dir should be created");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        // No backend specified → defaults to auto; override base_url for Exa via explicit config
        // We configure exa base_url to point at our mock, but in auto mode the URL falls
        // through to the default. Instead test with --backend flag override.
        format!(
            "[web.search]\nbackend = \"exa\"\napi_key_env = \"TEST_EXA_KEY\"\nbase_url = \"{}\"\n",
            server.url("/exa/search")
        ),
    )
    .expect("config should be written");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .env("TEST_EXA_KEY", "test-exa-key")
        .args([
            "--vault",
            vault_root.to_str().expect("vault path should be utf-8"),
            "--output",
            "json",
            "web",
            "search",
            "release notes",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);
    server.shutdown();

    // When configured as exa, should use exa not duckduckgo
    assert_eq!(json["backend"], "exa");
    assert!(json["results"].as_array().is_some_and(|r| !r.is_empty()));
}

#[test]
fn web_fetch_markdown_json_output_extracts_article_content() {
    let server = MockWebServer::spawn();
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "web",
            "fetch",
            &server.url("/article"),
            "--mode",
            "markdown",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);
    server.shutdown();

    assert_eq!(json["status"], 200);
    assert_eq!(json["content_type"], "text/html");
    assert_eq!(json["mode"], "markdown");
    assert!(json.get("extraction_mode").is_none());
    assert!(json["content"]
        .as_str()
        .is_some_and(|content| content.contains("Release Summary")
            && content.contains("Shipped & stable.")
            && !content.contains("skip me")));
}

#[test]
fn web_fetch_markdown_json_output_strips_page_chrome_for_docs_pages() {
    let server = MockWebServer::spawn();
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "web",
            "fetch",
            &server.url("/generic-page"),
            "--mode",
            "markdown",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);
    server.shutdown();

    assert_eq!(json["status"], 200);
    assert!(json.get("extraction_mode").is_none());
    assert!(json["content"]
        .as_str()
        .is_some_and(|content| !content.contains("Site Nav")
            && content.contains("Docs")
            && content.contains("Short")));
}

#[test]
fn web_fetch_markdown_errors_when_no_readable_content_is_found() {
    let server = MockWebServer::spawn();
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "web",
            "fetch",
            &server.url("/empty"),
            "--mode",
            "markdown",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "could not extract readable main content",
        ));
    server.shutdown();
}

#[test]
fn web_fetch_raw_save_writes_response_body() {
    let server = MockWebServer::spawn();
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    let output_path = temp_dir.path().join("page.bin");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "web",
            "fetch",
            &server.url("/raw"),
            "--mode",
            "raw",
            "--save",
            output_path
                .to_str()
                .expect("output path should be valid utf-8"),
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);
    server.shutdown();
    let rendered = fs::read(&output_path).expect("saved output should be readable");

    assert_eq!(json["status"], 200);
    assert_eq!(json["saved"], output_path.to_string_lossy().as_ref());
    assert_eq!(rendered, b"raw-body");
}

#[test]
fn web_help_documents_modes_and_backends() {
    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["web", "--help"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("search")
                .and(predicate::str::contains("fetch"))
                .and(predicate::str::contains("duckduckgo"))
                .and(predicate::str::contains("auto"))
                .and(predicate::str::contains("robots.txt"))
                .and(predicate::str::contains("[web.search]")),
        );
}

#[test]
fn periodic_list_and_gaps_report_expected_notes() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join("Journal/Daily")).expect("daily dir should be created");
    fs::write(
        vault_root.join("Journal/Daily/2026-04-01.md"),
        "# 2026-04-01\n",
    )
    .expect("first daily note should be written");
    fs::write(
        vault_root.join("Journal/Daily/2026-04-03.md"),
        "# 2026-04-03\n",
    )
    .expect("second daily note should be written");
    run_scan(&vault_root);

    let list_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "periodic",
            "list",
            "--type",
            "daily",
        ])
        .assert()
        .success();
    let list_rows = parse_stdout_json_lines(&list_assert);
    assert_eq!(list_rows.len(), 2);
    assert_eq!(list_rows[0]["path"], "Journal/Daily/2026-04-01.md");
    assert_eq!(list_rows[1]["path"], "Journal/Daily/2026-04-03.md");

    let gaps_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "periodic",
            "gaps",
            "--type",
            "daily",
            "--from",
            "2026-04-01",
            "--to",
            "2026-04-03",
        ])
        .assert()
        .success();
    let gap_rows = parse_stdout_json_lines(&gaps_assert);
    assert_eq!(gap_rows.len(), 1);
    assert_eq!(gap_rows[0]["date"], "2026-04-02");
    assert_eq!(gap_rows[0]["expected_path"], "Journal/Daily/2026-04-02.md");
}

#[test]
fn dataview_inline_json_output_evaluates_expressions() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("dataview", &vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "dataview",
            "inline",
            "Dashboard",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["file"], Value::String("Dashboard.md".to_string()));
    assert_eq!(json["results"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        json["results"][0]["expression"],
        Value::String("this.status".to_string())
    );
    assert_eq!(
        json["results"][0]["value"],
        Value::String("draft".to_string())
    );
    assert_eq!(json["results"][0]["error"], Value::Null);
}

#[test]
fn dataview_inline_json_output_reports_expression_errors() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault root should exist");
    fs::write(vault_root.join("Broken.md"), "`= (`\n").expect("note should be written");
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "dataview",
            "inline",
            "Broken",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["file"], Value::String("Broken.md".to_string()));
    assert_eq!(json["results"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        json["results"][0]["expression"],
        Value::String("(".to_string())
    );
    assert_eq!(json["results"][0]["value"], Value::Null);
    assert!(json["results"][0]["error"]
        .as_str()
        .is_some_and(|error| !error.is_empty()));
}

#[test]
fn dataview_query_json_output_evaluates_dql_strings() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("dataview", &vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "dataview",
            "query",
            r#"TABLE status, priority FROM "Projects" SORT file.name ASC"#,
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["query_type"], "table");
    assert_eq!(
        json["columns"],
        serde_json::json!(["File", "status", "priority"])
    );
    assert_eq!(json["rows"].as_array().map(Vec::len), Some(2));
    assert_eq!(
        json["rows"][0]["File"],
        Value::String("[[Projects/Alpha]]".to_string())
    );
    assert_eq!(
        json["rows"][0]["status"],
        Value::String("active".to_string())
    );
    assert_eq!(json["rows"][0]["priority"].as_f64(), Some(1.0));
    assert_eq!(
        json["rows"][1]["File"],
        Value::String("[[Projects/Beta]]".to_string())
    );
}

#[test]
fn dataview_query_json_output_surfaces_unsupported_dql_diagnostics() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("dataview", &vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "dataview",
            "query",
            r#"TABLE status.slugify() AS slug, mystery(status) AS surprise FROM "Projects" SORT file.name ASC"#,
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["query_type"], "table");
    assert_eq!(json["rows"].as_array().map(Vec::len), Some(2));
    assert_eq!(json["rows"][0]["slug"], Value::Null);
    assert_eq!(json["rows"][0]["surprise"], Value::Null);
    assert!(json["diagnostics"]
        .as_array()
        .is_some_and(|diagnostics| diagnostics.len() >= 2));
    assert!(json["diagnostics"]
        .as_array()
        .is_some_and(
            |diagnostics| diagnostics.iter().any(|diagnostic| diagnostic["message"]
                .as_str()
                .is_some_and(|message| message.contains("unknown method `slugify`")))
        ));
    assert!(json["diagnostics"]
        .as_array()
        .is_some_and(
            |diagnostics| diagnostics.iter().any(|diagnostic| diagnostic["message"]
                .as_str()
                .is_some_and(|message| message.contains("unknown function `mystery`")))
        ));
}

#[test]
fn dataview_query_js_json_output_evaluates_snippets() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("dataview", &vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "dataview",
            "query-js",
            r##"dv.list(dv.pages("#project").file.name.sort().array()); dv.execute('TABLE status FROM "Projects" SORT file.name ASC');"##,
            "--file",
            "Dashboard",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["outputs"].as_array().map(Vec::len), Some(2));
    assert_eq!(
        json["outputs"][0],
        serde_json::json!({
            "kind": "list",
            "items": ["Alpha", "Beta", "Dashboard"]
        })
    );
    assert_eq!(json["outputs"][1]["kind"], "query");
    assert_eq!(json["outputs"][1]["result"]["query_type"], "table");
    assert_eq!(json["outputs"][1]["result"]["result_count"], 2);
    assert_eq!(
        json["value"]["query_type"],
        Value::String("table".to_string())
    );
}

#[test]
fn dataview_eval_json_output_evaluates_selected_block() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("dataview", &vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "dataview",
            "eval",
            "Dashboard",
            "--block",
            "0",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["file"], Value::String("Dashboard.md".to_string()));
    assert_eq!(json["blocks"].as_array().map(Vec::len), Some(1));
    assert_eq!(json["blocks"][0]["block_index"], Value::Number(0.into()));
    assert_eq!(
        json["blocks"][0]["language"],
        Value::String("dataview".to_string())
    );
    assert_eq!(json["blocks"][0]["error"], Value::Null);
    assert_eq!(json["blocks"][0]["result"]["engine"], "dql");
    assert_eq!(json["blocks"][0]["result"]["data"]["query_type"], "table");
    assert_eq!(
        json["blocks"][0]["result"]["data"]["columns"],
        serde_json::json!(["File", "status", "priority"])
    );
    assert_eq!(
        json["blocks"][0]["result"]["data"]["result_count"],
        Value::Number(2.into())
    );
    assert_eq!(
        json["blocks"][0]["result"]["data"]["rows"][0],
        serde_json::json!({
            "File": "[[Projects/Alpha]]",
            "status": "active",
            "priority": 1.0
        })
    );
    assert_eq!(
        json["blocks"][0]["result"]["data"]["rows"][1],
        serde_json::json!({
            "File": "[[Dashboard]]",
            "status": "draft",
            "priority": [2.0, 3.0]
        })
    );
}

#[test]
fn dataview_eval_json_output_defaults_to_all_indexed_blocks() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("dataview", &vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "dataview",
            "eval",
            "Dashboard",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["blocks"].as_array().map(Vec::len), Some(2));
    assert_eq!(
        json["blocks"][0]["language"],
        Value::String("dataview".to_string())
    );
    assert_eq!(json["blocks"][0]["error"], Value::Null);
    assert_eq!(json["blocks"][0]["result"]["engine"], "dql");
    assert_eq!(
        json["blocks"][1]["language"],
        Value::String("dataviewjs".to_string())
    );
    assert_eq!(json["blocks"][1]["error"], Value::Null);
    assert_eq!(json["blocks"][1]["result"]["engine"], "js");
    assert_eq!(
        json["blocks"][1]["result"]["data"]["outputs"],
        serde_json::json!([
            {
                "kind": "table",
                "headers": ["Status"],
                "rows": [["draft"]]
            }
        ])
    );
}

#[test]
fn dataview_eval_human_output_keeps_empty_table_headers() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("dataview", &vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "dataview",
            "eval",
            "Dashboard",
            "--block",
            "0",
        ])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone())
        .expect("stdout should be valid utf-8");

    assert!(stdout.contains("| File | status | priority |"));
    assert!(stdout.contains("| --- | --- | --- |"));
    assert!(stdout.contains("| [[Projects/Alpha]] | active | 1.0 |"));
    assert!(stdout.contains("| [[Dashboard]] | draft | [2.0,3.0] |"));
    assert!(stdout.contains("2 result(s)"));
}

#[test]
fn dataview_eval_human_output_escapes_pipes_inside_table_cells() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("dataview", &vault_root);
    fs::write(
        vault_root.join("Pipe Dashboard.md"),
        concat!(
            "```dataviewjs\n",
            "dv.table([\"Value\"], [[\"work | break\"]]);\n",
            "```\n",
        ),
    )
    .expect("note should be written");
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "dataview",
            "eval",
            "Pipe Dashboard",
        ])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone())
        .expect("stdout should be valid utf-8");

    assert!(stdout.contains("| Value |"));
    assert!(stdout.contains("| --- |"));
    assert!(stdout.contains("| work \\| break |"));
}

#[test]
fn dataview_eval_json_output_preserves_js_equality_operators() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("dataview", &vault_root);
    fs::write(
        vault_root.join("Equality Dashboard.md"),
        concat!(
            "```dataviewjs\n",
            "const checks = [\n",
            "  typeof \"x\" === \"string\",\n",
            "  1 == 1,\n",
            "  1 != 2,\n",
            "  2 !== 3,\n",
            "];\n",
            "dv.paragraph(String(checks.every(Boolean)));\n",
            "```\n",
        ),
    )
    .expect("note should be written");
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "dataview",
            "eval",
            "Equality Dashboard",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["blocks"][0]["language"], "dataviewjs");
    assert_eq!(json["blocks"][0]["error"], Value::Null);
    assert_eq!(
        json["blocks"][0]["result"]["data"]["outputs"],
        serde_json::json!([
            {
                "kind": "paragraph",
                "text": "true"
            }
        ])
    );
}

#[test]
fn dataview_eval_json_output_preserves_js_comment_markers_and_hash_literals() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("dataview", &vault_root);
    fs::write(
        vault_root.join("Literal Dashboard.md"),
        concat!(
            "```dataviewjs\n",
            "const text = \"%%keep%% | #notatag\";\n",
            "dv.paragraph(text);\n",
            "```\n",
        ),
    )
    .expect("note should be written");
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "dataview",
            "eval",
            "Literal Dashboard",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["blocks"][0]["language"], "dataviewjs");
    assert_eq!(json["blocks"][0]["error"], Value::Null);
    assert_eq!(
        json["blocks"][0]["result"]["data"]["outputs"],
        serde_json::json!([
            {
                "kind": "paragraph",
                "text": "%%keep%% | #notatag"
            }
        ])
    );
}

#[test]
fn dataview_eval_human_output_shows_unsupported_dql_diagnostics() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("dataview", &vault_root);
    fs::write(
        vault_root.join("Unsupported.md"),
        concat!(
            "```dataview\n",
            "TABLE status.slugify() AS slug\n",
            "FROM \"Projects\"\n",
            "SORT file.name ASC\n",
            "```\n",
        ),
    )
    .expect("unsupported note should be written");
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "dataview",
            "eval",
            "Unsupported",
        ])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone())
        .expect("stdout should be valid utf-8");

    assert!(stdout.contains("File | slug"));
    assert!(stdout.contains("[[Projects/Alpha]] | null"));
    assert!(stdout.contains("Diagnostics:"));
    assert!(stdout.contains("unknown method `slugify`"));
}

fn write_tasks_cli_fixture(vault_root: &Path) {
    fs::create_dir_all(vault_root.join(".vulcan")).expect("config dir should exist");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        "[tasks]\nglobal_filter = \"#task\"\nglobal_query = \"not done\"\nremove_global_filter = true\n",
    )
    .expect("config should be written");
    fs::write(
        vault_root.join("Tasks.md"),
        concat!(
            "# Sprint\n\n",
            "- [ ] Write docs #task\n",
            "- [x] Ship release #task\n",
            "- [x] Archive misc #misc\n",
            "- [ ] Plan backlog #task\n",
        ),
    )
    .expect("tasks note should be written");
    fs::write(
        vault_root.join("Dashboard.md"),
        concat!(
            "```tasks\n",
            "done\n",
            "```\n\n",
            "```tasks\n",
            "path includes Tasks\n",
            "```\n",
        ),
    )
    .expect("dashboard note should be written");
}

fn write_tasks_dependency_fixture(vault_root: &Path) {
    fs::create_dir_all(vault_root.join(".vulcan")).expect("config dir should exist");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        "[tasks]\nglobal_filter = \"#task\"\nremove_global_filter = true\n",
    )
    .expect("config should be written");
    fs::write(
        vault_root.join("Tasks.md"),
        concat!(
            "- [ ] Write docs #task 🆔 WRITE-1\n",
            "- [ ] Ship release #task 🆔 SHIP-1\n",
            "- [ ] Publish docs #task ⛔ SHIP-1\n",
            "- [ ] Prep launch #task ⛔ MISSING-1\n",
            "- [ ] Archive misc #misc ⛔ WRITE-1\n",
        ),
    )
    .expect("dependency note should be written");
}

fn write_tasks_recurrence_fixture(vault_root: &Path) {
    fs::create_dir_all(vault_root.join(".vulcan")).expect("config dir should exist");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        "[tasks]\nglobal_filter = \"#task\"\nremove_global_filter = true\n",
    )
    .expect("config should be written");
    fs::write(
        vault_root.join("Recurring.md"),
        concat!(
            "- [ ] Review sprint #task ⏳ 2026-03-30 🔁 every 2 weeks\n",
            "- [ ] Close books #task ⏳ 2026-02-15 [repeat:: every month on the 15th]\n",
            "- [ ] Publish notes #task ⏳ 2026-03-26 [repeat:: RRULE:FREQ=WEEKLY;INTERVAL=2;BYDAY=TH]\n",
            "- [ ] Ignore misc #misc ⏳ 2026-03-30 🔁 every 2 weeks\n",
        ),
    )
    .expect("recurring note should be written");
}

fn write_tasks_import_fixture(vault_root: &Path) {
    fs::create_dir_all(vault_root.join(".obsidian/plugins/obsidian-tasks-plugin"))
        .expect("tasks plugin dir should exist");
    fs::write(
        vault_root.join(".obsidian/plugins/obsidian-tasks-plugin/data.json"),
        r##"{
          "globalFilter": "#task",
          "globalQuery": "not done",
          "removeGlobalFilter": true,
          "setCreatedDate": true,
          "recurrenceOnCompletion": "next-line",
          "statusSettings": {
            "coreStatuses": [
              { "symbol": " ", "name": "Todo", "type": "TODO", "nextStatusSymbol": ">" },
              { "symbol": "x", "name": "Done", "type": "DONE", "nextStatusSymbol": " " }
            ],
            "customStatuses": [
              { "symbol": ">", "name": "Waiting", "type": "IN_PROGRESS", "nextStatusSymbol": "x" },
              { "symbol": "~", "name": "Parked", "type": "NON_TASK" }
            ]
          }
        }"##,
    )
    .expect("tasks plugin config should be written");
}

#[allow(clippy::too_many_lines)]
fn write_tasknotes_import_fixture(vault_root: &Path) {
    fs::create_dir_all(vault_root.join(".obsidian/plugins/tasknotes"))
        .expect("tasknotes plugin dir should exist");
    fs::create_dir_all(vault_root.join("Views Source"))
        .expect("tasknotes view source dir should exist");
    fs::write(
        vault_root.join("Views Source/tasks-custom.base"),
        concat!(
            "# All Tasks\n\n",
            "views:\n",
            "  - type: tasknotesTaskList\n",
            "    name: \"All Tasks\"\n",
            "    order:\n",
            "      - note.status\n",
            "      - note.priority\n",
            "      - note.due\n",
        ),
    )
    .expect("task list base should be written");
    fs::write(
        vault_root.join("Views Source/kanban-custom.base"),
        concat!(
            "# Kanban\n\n",
            "views:\n",
            "  - type: tasknotesKanban\n",
            "    name: \"Kanban\"\n",
            "    order:\n",
            "      - note.status\n",
            "      - note.priority\n",
            "    groupBy:\n",
            "      property: note.status\n",
            "      direction: ASC\n",
        ),
    )
    .expect("kanban base should be written");
    fs::write(
        vault_root.join("Views Source/relationships-custom.base"),
        concat!(
            "# Relationships\n\n",
            "views:\n",
            "  - type: tasknotesTaskList\n",
            "    name: \"Projects\"\n",
            "    filters:\n",
            "      and:\n",
            "        - list(this.projects).contains(file.asLink())\n",
            "    order:\n",
            "      - note.projects\n",
        ),
    )
    .expect("relationships base should be written");
    fs::write(
        vault_root.join("Views Source/agenda-custom.base"),
        concat!(
            "# Agenda\n\n",
            "views:\n",
            "  - type: tasknotesCalendar\n",
            "    name: \"Agenda\"\n",
        ),
    )
    .expect("agenda base should be written");
    fs::write(
        vault_root.join(".obsidian/plugins/tasknotes/data.json"),
        r##"{
          "tasksFolder": "Tasks",
          "archiveFolder": "Archive",
          "taskTag": "task",
          "taskIdentificationMethod": "tag",
          "taskPropertyName": "isTask",
          "taskPropertyValue": "yes",
          "excludedFolders": "Archive, Someday",
          "defaultTaskStatus": "in-progress",
          "defaultTaskPriority": "high",
          "fieldMapping": {
            "due": "deadline",
            "timeEstimate": "estimateMinutes",
            "archiveTag": "archived-task"
          },
          "customStatuses": [
            {
              "id": "blocked",
              "value": "blocked",
              "label": "Blocked",
              "color": "#ff8800",
              "isCompleted": false,
              "order": 4,
              "autoArchive": false,
              "autoArchiveDelay": 15
            }
          ],
          "customPriorities": [
            {
              "id": "urgent",
              "value": "urgent",
              "label": "Urgent",
              "color": "#ff0000",
              "weight": 9
            }
          ],
          "userFields": [
            {
              "id": "effort",
              "displayName": "Effort",
              "key": "effort",
              "type": "number"
            }
          ],
          "enableNaturalLanguageInput": false,
          "nlpDefaultToScheduled": true,
          "nlpLanguage": "de",
          "nlpTriggers": {
            "triggers": [
              { "propertyId": "contexts", "trigger": "context:", "enabled": true },
              { "propertyId": "tags", "trigger": "#", "enabled": true }
            ]
          },
          "taskCreationDefaults": {
            "defaultContexts": "@office, @home",
            "defaultTags": "work, urgent",
            "defaultProjects": "[[Projects/Alpha]], [[Projects/Beta]]",
            "defaultTimeEstimate": 45,
            "defaultDueDate": "tomorrow",
            "defaultScheduledDate": "today",
            "defaultRecurrence": "weekly",
            "defaultReminders": [
              {
                "id": "rem-relative",
                "type": "relative",
                "relatedTo": "due",
                "offset": 15,
                "unit": "minutes",
                "direction": "before",
                "description": "Before due"
              }
            ]
          },
          "calendarViewSettings": { "defaultView": "month" },
          "pomodoroWorkDuration": 25,
          "pomodoroShortBreakDuration": 5,
          "pomodoroLongBreakDuration": 15,
          "pomodoroLongBreakInterval": 4,
          "pomodoroStorageLocation": "daily-notes",
          "pomodoroNotifications": true,
          "enableTaskLinkOverlay": true,
          "uiLanguage": "de",
          "icsIntegration": { "enabled": true },
          "savedViews": [{ "id": "today", "name": "Today" }],
          "enableAPI": true,
          "webhooks": [{ "url": "https://example.test/hook" }],
          "enableBases": true,
          "commandFileMapping": {
            "open-tasks-view": "Views Source/tasks-custom.base",
            "open-kanban-view": "Views Source/kanban-custom.base",
            "relationships": "Views Source/relationships-custom.base",
            "open-agenda-view": "Views Source/agenda-custom.base"
          },
          "enableGoogleCalendar": true,
          "googleOAuthClientId": "google-client",
          "enableMicrosoftCalendar": true,
          "microsoftOAuthClientId": "microsoft-client"
        }"##,
    )
    .expect("tasknotes plugin config should be written");
}

fn write_kanban_cli_fixture(vault_root: &Path) {
    fs::create_dir_all(vault_root.join("Projects")).expect("projects dir should exist");
    fs::write(
        vault_root.join("Projects/Alpha.md"),
        "---\nstatus: active\nowner: Ops\n---\n# Alpha\n",
    )
    .expect("linked note should be written");
    fs::write(
        vault_root.join("Board.md"),
        concat!(
            "---\n",
            "kanban-plugin: board\n",
            "date-trigger: DUE\n",
            "time-trigger: AT\n",
            "---\n\n",
            "## Todo\n\n",
            "- Release DUE{2026-04-01} AT{09:30} #ship [[Projects/Alpha]] [priority:: high]\n",
            "- [/] Waiting on review [owner:: Ops]\n\n",
            "## Done\n\n",
            "- Shipped DUE{2026-04-03}\n",
        ),
    )
    .expect("board should be written");
}

fn write_kanban_archive_fixture(vault_root: &Path) {
    fs::write(
        vault_root.join("Board.md"),
        concat!(
            "---\n",
            "kanban-plugin: board\n",
            "---\n\n",
            "## Todo\n\n",
            "- Build release\n\n",
            "## Done\n\n",
            "- Shipped\n\n",
            "***\n\n",
            "## Archive\n\n",
            "- Old card\n",
        ),
    )
    .expect("board should be written");
}

#[test]
fn tasks_query_json_output_evaluates_tasks_dsl() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault root should exist");
    write_tasks_cli_fixture(&vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "query",
            "done",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["result_count"], Value::Number(1.into()));
    assert_eq!(json["tasks"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        json["tasks"][0]["text"],
        Value::String("Ship release".to_string())
    );
    assert_eq!(json["tasks"][0]["tags"], Value::Array(Vec::new()));
}

#[test]
fn tasks_eval_json_output_evaluates_selected_block_with_defaults() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault root should exist");
    write_tasks_cli_fixture(&vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "eval",
            "Dashboard",
            "--block",
            "1",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["file"], Value::String("Dashboard.md".to_string()));
    assert_eq!(json["blocks"].as_array().map(Vec::len), Some(1));
    assert_eq!(json["blocks"][0]["block_index"], Value::Number(1.into()));
    assert_eq!(
        json["blocks"][0]["source"],
        Value::String("path includes Tasks".to_string())
    );
    assert_eq!(
        json["blocks"][0]["effective_source"],
        Value::String("tag includes #task\nnot done\npath includes Tasks".to_string())
    );
    assert_eq!(
        json["blocks"][0]["result"]["result_count"],
        Value::Number(2.into())
    );
    assert_eq!(
        json["blocks"][0]["result"]["tasks"][0]["text"],
        Value::String("Write docs".to_string())
    );
    assert_eq!(
        json["blocks"][0]["result"]["tasks"][1]["text"],
        Value::String("Plan backlog".to_string())
    );
}

#[test]
fn tasks_list_json_output_accepts_tasks_dsl_filters() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault root should exist");
    write_tasks_cli_fixture(&vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "list",
            "--filter",
            "not done",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["result_count"], Value::Number(2.into()));
    assert_eq!(json["tasks"].as_array().map(Vec::len), Some(2));
    assert_eq!(
        json["tasks"][0]["text"],
        Value::String("Write docs".to_string())
    );
    assert_eq!(
        json["tasks"][1]["text"],
        Value::String("Plan backlog".to_string())
    );
}

#[test]
fn tasks_list_json_output_accepts_dataview_expression_filters() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault root should exist");
    write_tasks_cli_fixture(&vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "list",
            "--filter",
            "completed",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["result_count"], Value::Number(1.into()));
    assert_eq!(
        json["tasks"][0]["text"],
        Value::String("Ship release".to_string())
    );
}

#[test]
fn tasks_next_json_output_lists_upcoming_recurring_instances() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault root should exist");
    write_tasks_recurrence_fixture(&vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "next",
            "4",
            "--from",
            "2026-03-29",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(
        json["reference_date"],
        Value::String("2026-03-29".to_string())
    );
    assert_eq!(json["result_count"], Value::Number(4.into()));
    assert_eq!(json["occurrences"].as_array().map(Vec::len), Some(4));
    assert_eq!(
        json["occurrences"][0]["date"],
        Value::String("2026-03-30".to_string())
    );
    assert_eq!(
        json["occurrences"][0]["task"]["recurrenceRule"],
        Value::String("FREQ=WEEKLY;INTERVAL=2".to_string())
    );
    assert_eq!(
        json["occurrences"][1]["date"],
        Value::String("2026-04-09".to_string())
    );
    assert_eq!(
        json["occurrences"][1]["task"]["recurrenceRule"],
        Value::String("FREQ=WEEKLY;INTERVAL=2;BYDAY=TH".to_string())
    );
    assert_eq!(
        json["occurrences"][2]["date"],
        Value::String("2026-04-13".to_string())
    );
    assert_eq!(json["occurrences"][2]["sequence"], Value::Number(2.into()));
    assert_eq!(
        json["occurrences"][3]["date"],
        Value::String("2026-04-15".to_string())
    );
    assert_eq!(
        json["occurrences"][3]["task"]["recurrence"],
        Value::String("every month on the 15th".to_string())
    );
    assert_eq!(
        json["occurrences"][3]["task"]["recurrenceMonthDay"],
        Value::Number(15.into())
    );
}

#[test]
fn config_import_tasks_json_output_writes_config_and_reports_mapping() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault root should exist");
    initialize_vulcan_dir(&vault_root);
    write_tasks_import_fixture(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "config",
            "import",
            "tasks",
            "--apply",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["plugin"], Value::String("tasks".to_string()));
    assert_eq!(json["created_config"], Value::Bool(true));
    assert_eq!(json["updated"], Value::Bool(true));
    assert!(json["mappings"]
        .as_array()
        .is_some_and(|mappings| mappings.iter().any(|mapping| {
            mapping["target"] == "tasks.global_filter" && mapping["value"] == "#task"
        })));

    let rendered =
        fs::read_to_string(vault_root.join(".vulcan/config.toml")).expect("config should exist");
    assert!(rendered.contains("[tasks]"));
    assert!(rendered.contains("global_filter = \"#task\""));
    assert!(rendered.contains("global_query = \"not done\""));
    assert!(rendered.contains("remove_global_filter = true"));
    assert!(rendered.contains("[[tasks.statuses.definitions]]"));
    assert!(rendered.contains("name = \"Waiting\""));
}

#[test]
fn config_import_preview_shows_diff_without_writing_files() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault root should exist");
    write_tasks_import_fixture(&vault_root);

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "config",
            "import",
            "tasks",
            "--preview",
        ])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("diff:")
                .and(predicate::str::contains("--- a/.vulcan/config.toml"))
                .and(predicate::str::contains("+++ b/.vulcan/config.toml"))
                .and(predicate::str::contains("+[tasks]"))
                .and(predicate::str::contains("+global_filter = \"#task\"")),
        );

    assert!(!vault_root.join(".vulcan/config.toml").exists());
}

#[test]
#[allow(clippy::too_many_lines)]
fn config_import_tasknotes_json_output_writes_config_and_reports_mapping() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("tasknotes", &vault_root);
    write_tasknotes_import_fixture(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "config",
            "import",
            "tasknotes",
            "--no-commit",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["plugin"], Value::String("tasknotes".to_string()));
    assert_eq!(json["created_config"], Value::Bool(true));
    assert_eq!(json["updated"], Value::Bool(true));
    assert!(json["mappings"]
        .as_array()
        .is_some_and(|mappings| mappings.iter().any(|mapping| {
            mapping["target"] == "tasknotes.tasks_folder" && mapping["value"] == "Tasks"
        })));
    assert!(json["mappings"]
        .as_array()
        .is_some_and(|mappings| mappings.iter().any(|mapping| {
            mapping["target"] == "tasknotes.field_mapping.due" && mapping["value"] == "deadline"
        })));
    assert!(json["mappings"]
        .as_array()
        .is_some_and(|mappings| mappings.iter().any(|mapping| {
            mapping["target"] == "tasknotes.pomodoro.storage_location"
                && mapping["value"] == "daily-note"
        })));
    assert!(json["mappings"]
        .as_array()
        .is_some_and(|mappings| mappings.iter().any(|mapping| {
            mapping["target"] == "tasknotes.task_creation_defaults.default_reminders"
                && mapping["value"]
                    .as_array()
                    .is_some_and(|reminders| reminders.len() == 1)
        })));
    assert!(json["skipped"]
        .as_array()
        .is_some_and(|skipped| skipped.iter().any(|item| {
            item["source"] == "calendarViewSettings"
                && item["reason"] == "calendar view settings are not yet supported"
        })));
    assert!(json["skipped"]
        .as_array()
        .is_some_and(|skipped| skipped.iter().any(|item| {
            item["reason"] == "advanced pomodoro automation settings are not yet supported"
        })));
    assert!(json["migrated_files"]
        .as_array()
        .is_some_and(|files| files.iter().any(|item| {
            item["source"] == "Views Source/tasks-custom.base"
                && item["target"] == "TaskNotes/Views/tasks-default.base"
                && item["action"] == "copy"
        })));
    assert!(json["migrated_files"]
        .as_array()
        .is_some_and(|files| files.iter().any(|item| {
            item["source"] == "Views Source/kanban-custom.base"
                && item["target"] == "TaskNotes/Views/kanban-default.base"
                && item["action"] == "copy"
        })));
    assert!(json["migrated_files"]
        .as_array()
        .is_some_and(|files| files.iter().any(|item| {
            item["source"] == "Views Source/relationships-custom.base"
                && item["target"] == "TaskNotes/Views/relationships.base"
                && item["action"] == "copy"
        })));
    assert!(json["skipped"]
        .as_array()
        .is_some_and(|skipped| skipped.iter().any(|item| {
            item["source"] == "commandFileMapping.open-agenda-view"
                && item["reason"].as_str().is_some_and(|reason| {
                    reason.contains("unsupported view types: tasknotesCalendar")
                })
        })));

    let rendered =
        fs::read_to_string(vault_root.join(".vulcan/config.toml")).expect("config should exist");
    assert!(rendered.contains("[tasknotes]"));
    assert!(rendered.contains("tasks_folder = \"Tasks\""));
    assert!(rendered.contains("archive_folder = \"Archive\""));
    assert!(rendered.contains("task_tag = \"task\""));
    assert!(rendered.contains("task_property_name = \"isTask\""));
    assert!(rendered.contains("[tasknotes.field_mapping]"));
    assert!(rendered.contains("due = \"deadline\""));
    assert!(rendered.contains("[[tasknotes.statuses]]"));
    assert!(rendered.contains("value = \"blocked\""));
    assert!(rendered.contains("[[tasknotes.priorities]]"));
    assert!(rendered.contains("value = \"urgent\""));
    assert!(rendered.contains("[[tasknotes.user_fields]]"));
    assert!(rendered.contains("displayName = \"Effort\""));
    assert!(rendered.contains("[tasknotes.pomodoro]"));
    assert!(rendered.contains("storage_location = \"daily-note\""));
    assert!(rendered.contains("[tasknotes.task_creation_defaults]"));
    assert!(rendered.contains("default_due_date = \"tomorrow\""));
    assert!(rendered.contains("[[tasknotes.task_creation_defaults.default_reminders]]"));
    assert!(rendered.contains("id = \"rem-relative\""));
    let migrated_tasks = fs::read_to_string(vault_root.join("TaskNotes/Views/tasks-default.base"))
        .expect("migrated task list base should exist");
    assert!(migrated_tasks.starts_with("source: tasknotes\n\n# All Tasks\n"));
    assert!(!vault_root
        .join("TaskNotes/Views/agenda-default.base")
        .exists());

    run_scan(&vault_root);
    let view_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "view",
            "show",
            "tasks-default",
        ])
        .assert()
        .success();
    let view_json = parse_stdout_json(&view_assert);
    assert_eq!(view_json["file"], "TaskNotes/Views/tasks-default.base");
    assert_eq!(view_json["views"][0]["view_type"], "tasknotesTaskList");
    assert!(view_json["views"][0]["rows"]
        .as_array()
        .is_some_and(|rows| rows
            .iter()
            .any(|row| row["document_path"] == "TaskNotes/Tasks/Write Docs.md")));
}

#[test]
fn tasks_blocked_json_output_lists_blockers() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault root should exist");
    write_tasks_dependency_fixture(&vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "blocked",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["tasks"].as_array().map(Vec::len), Some(2));
    assert_eq!(
        json["tasks"][0]["task"]["text"],
        Value::String("Publish docs ⛔ SHIP-1".to_string())
    );
    assert_eq!(
        json["tasks"][0]["blockers"][0]["blocker_id"],
        Value::String("SHIP-1".to_string())
    );
    assert_eq!(
        json["tasks"][0]["blockers"][0]["blocker_completed"],
        Value::Bool(false)
    );
    assert_eq!(
        json["tasks"][1]["task"]["text"],
        Value::String("Prep launch ⛔ MISSING-1".to_string())
    );
    assert_eq!(
        json["tasks"][1]["blockers"][0]["resolved"],
        Value::Bool(false)
    );
}

#[test]
fn tasks_graph_json_output_lists_nodes_and_edges() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault root should exist");
    write_tasks_dependency_fixture(&vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "graph",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["nodes"].as_array().map(Vec::len), Some(4));
    assert_eq!(json["edges"].as_array().map(Vec::len), Some(2));
    assert_eq!(
        json["edges"][0]["blocker_id"],
        Value::String("SHIP-1".to_string())
    );
    assert_eq!(json["edges"][0]["resolved"], Value::Bool(true));
    assert_eq!(
        json["edges"][1]["blocker_id"],
        Value::String("MISSING-1".to_string())
    );
    assert_eq!(json["edges"][1]["resolved"], Value::Bool(false));
}

#[test]
fn tasks_list_json_output_includes_tasknotes_file_tasks() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("tasknotes", &vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "list",
            "--filter",
            "status.type is in_progress",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["result_count"], Value::Number(1.into()));
    assert_eq!(
        json["tasks"][0]["text"],
        Value::String("Write docs".to_string())
    );
    assert_eq!(
        json["tasks"][0]["id"],
        Value::String("[[TaskNotes/Tasks/Write Docs]]".to_string())
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn tasks_list_json_output_supports_source_filters_and_archived_toggle() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("tasknotes", &vault_root);
    fs::write(
        vault_root.join("Inbox.md"),
        concat!(
            "- [ ] Inline follow-up #ops 🗓️ 2026-04-09\n",
            "- [x] Inline shipped #ops\n"
        ),
    )
    .expect("inline task fixture should be written");
    fs::write(
        vault_root.join("TaskNotes/Tasks/Archived Flag.md"),
        concat!(
            "---\n",
            "title: \"Archived flag\"\n",
            "status: \"done\"\n",
            "priority: \"low\"\n",
            "tags: [\"task\", \"archived\"]\n",
            "---\n"
        ),
    )
    .expect("archived task fixture should be written");
    run_scan(&vault_root);

    let file_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "list",
            "--source",
            "tasknotes",
            "--status",
            "in-progress",
            "--priority",
            "high",
            "--due-before",
            "2026-04-11",
            "--project",
            "[[Projects/Website]]",
            "--context",
            "@desk",
            "--sort-by",
            "due",
            "--group-by",
            "source",
        ])
        .assert()
        .success();
    let file_json = parse_stdout_json(&file_assert);

    assert_eq!(file_json["result_count"], Value::Number(1.into()));
    assert_eq!(
        file_json["tasks"][0]["text"],
        Value::String("Write docs".to_string())
    );
    assert_eq!(
        file_json["tasks"][0]["taskSource"],
        Value::String("file".to_string())
    );
    assert_eq!(
        file_json["groups"][0]["key"],
        Value::String("file".to_string())
    );

    let inline_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "list",
            "--source",
            "inline",
        ])
        .assert()
        .success();
    let inline_json = parse_stdout_json(&inline_assert);

    assert_eq!(inline_json["result_count"], Value::Number(2.into()));
    assert!(inline_json["tasks"]
        .as_array()
        .expect("tasks should be an array")
        .iter()
        .all(|task| task["taskSource"] == Value::String("inline".to_string())));

    let archived_default_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "list",
            "--source",
            "tasknotes",
        ])
        .assert()
        .success();
    let archived_default_json = parse_stdout_json(&archived_default_assert);
    assert_eq!(
        archived_default_json["result_count"],
        Value::Number(2.into())
    );

    let archived_all_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "list",
            "--source",
            "tasknotes",
            "--include-archived",
        ])
        .assert()
        .success();
    let archived_all_json = parse_stdout_json(&archived_all_assert);
    assert_eq!(archived_all_json["result_count"], Value::Number(3.into()));
    assert!(archived_all_json["tasks"]
        .as_array()
        .expect("tasks should be an array")
        .iter()
        .any(|task| task["text"] == Value::String("Archived flag".to_string())));
}

#[test]
fn tasks_list_json_output_defaults_source_from_config() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("tasknotes", &vault_root);
    fs::write(
        vault_root.join("Inbox.md"),
        concat!(
            "- [ ] Inline follow-up #ops 🗓️ 2026-04-09\n",
            "- [x] Inline shipped #ops\n"
        ),
    )
    .expect("inline task fixture should be written");
    fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should exist");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        "[tasks]\ndefault_source = \"inline\"\n",
    )
    .expect("config should be written");
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "list",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["result_count"], Value::Number(2.into()));
    assert!(json["tasks"]
        .as_array()
        .expect("tasks should be an array")
        .iter()
        .all(|task| task["taskSource"] == Value::String("inline".to_string())));
}

#[test]
fn tasks_list_dql_filter_keeps_sort_and_group_options() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("tasknotes", &vault_root);
    fs::write(
        vault_root.join("Inbox.md"),
        concat!(
            "- [ ] Inline follow-up #ops 🗓️ 2026-04-09\n",
            "- [x] Inline shipped #ops\n"
        ),
    )
    .expect("inline task fixture should be written");
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "list",
            "--filter",
            "completed && taskSource = \"inline\"",
            "--sort-by",
            "source",
            "--group-by",
            "source",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["result_count"], Value::Number(1.into()));
    assert_eq!(
        json["tasks"][0]["text"],
        Value::String("Inline shipped #ops".to_string())
    );
    assert_eq!(json["groups"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        json["groups"][0]["field"],
        Value::String("source".to_string())
    );
    assert_eq!(
        json["groups"][0]["key"],
        Value::String("inline".to_string())
    );
}

#[test]
fn tasks_next_and_graph_json_output_support_tasknotes_recurrence_and_dependencies() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("tasknotes", &vault_root);
    run_scan(&vault_root);

    let next_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "next",
            "2",
            "--from",
            "2026-04-04",
        ])
        .assert()
        .success();
    let next_json = parse_stdout_json(&next_assert);

    assert_eq!(next_json["result_count"], Value::Number(2.into()));
    assert_eq!(
        next_json["occurrences"][0]["date"],
        Value::String("2026-04-10".to_string())
    );
    assert_eq!(
        next_json["occurrences"][0]["task"]["id"],
        Value::String("[[TaskNotes/Tasks/Write Docs]]".to_string())
    );

    let graph_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "graph",
        ])
        .assert()
        .success();
    let graph_json = parse_stdout_json(&graph_assert);

    assert_eq!(graph_json["nodes"].as_array().map(Vec::len), Some(2));
    assert_eq!(graph_json["edges"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        graph_json["edges"][0]["blocker_id"],
        Value::String("[[TaskNotes/Tasks/Prep Outline]]".to_string())
    );
    assert_eq!(
        graph_json["edges"][0]["relation_type"],
        Value::String("FINISHTOSTART".to_string())
    );
    assert_eq!(
        graph_json["edges"][0]["gap"],
        Value::String("P1D".to_string())
    );
    assert_eq!(graph_json["edges"][0]["resolved"], Value::Bool(true));
}

#[test]
fn tasks_view_list_json_output_reports_available_tasknotes_views() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("tasknotes", &vault_root);
    write_tasknotes_views_fixture(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "view",
            "list",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    let views = json["views"].as_array().expect("views should be an array");
    assert!(views.iter().any(|view| {
        view["file"] == "TaskNotes/Views/tasks-default.base"
            && view["view_name"] == "Tasks"
            && view["view_type"] == "tasknotesTaskList"
            && view["supported"] == true
    }));
    assert!(views.iter().any(|view| {
        view["file"] == "TaskNotes/Views/kanban-default.base"
            && view["view_name"] == "Kanban Board"
            && view["view_type"] == "tasknotesKanban"
            && view["supported"] == true
    }));
}

#[test]
fn tasks_view_list_json_output_includes_saved_view_aliases() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("tasknotes", &vault_root);
    fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should be created");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        r#"[tasknotes]

[[tasknotes.saved_views]]
id = "blocked"
name = "Blocked Tasks"

[tasknotes.saved_views.query]
type = "group"
id = "root"
conjunction = "and"
sortKey = "due"
sortDirection = "asc"

[[tasknotes.saved_views.query.children]]
type = "condition"
id = "status-filter"
property = "status"
operator = "is"
value = "in-progress"
"#,
    )
    .expect("config should be written");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "view",
            "list",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    let views = json["views"].as_array().expect("views should be an array");
    assert!(views.iter().any(|view| {
        view["file"] == "config.tasknotes.saved_views.blocked"
            && view["file_stem"] == "blocked"
            && view["view_name"] == "Blocked Tasks"
            && view["view_type"] == "tasknotesTaskList"
            && view["supported"] == true
    }));
}

#[test]
fn tasks_view_json_output_evaluates_named_tasknotes_views() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("tasknotes", &vault_root);
    write_tasknotes_views_fixture(&vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "view",
            "show",
            "Tasks",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(
        json["file"],
        Value::String("TaskNotes/Views/tasks-default.base".to_string())
    );
    assert_eq!(json["views"].as_array().map(Vec::len), Some(1));
    assert_eq!(json["views"][0]["name"], Value::String("Tasks".to_string()));
    assert_eq!(
        json["views"][0]["view_type"],
        Value::String("tasknotesTaskList".to_string())
    );
    assert_eq!(json["views"][0]["rows"].as_array().map(Vec::len), Some(2));
    assert_eq!(
        json["views"][0]["rows"][0]["document_path"],
        Value::String("TaskNotes/Tasks/Prep Outline.md".to_string())
    );
    assert_eq!(
        json["views"][0]["rows"][1]["cells"]["efficiencyRatio"],
        Value::Number(67.into())
    );
}

#[test]
fn tasks_view_json_output_evaluates_saved_view_aliases() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("tasknotes", &vault_root);
    fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should be created");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        r#"[tasknotes]

[[tasknotes.saved_views]]
id = "blocked"
name = "Blocked Tasks"

[tasknotes.saved_views.query]
type = "group"
id = "root"
conjunction = "and"
sortKey = "due"
sortDirection = "asc"

[[tasknotes.saved_views.query.children]]
type = "condition"
id = "status-filter"
property = "status"
operator = "is"
value = "in-progress"
"#,
    )
    .expect("config should be written");
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "view",
            "show",
            "blocked",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(
        json["file"],
        Value::String("config.tasknotes.saved_views.blocked".to_string())
    );
    assert_eq!(json["views"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        json["views"][0]["name"],
        Value::String("Blocked Tasks".to_string())
    );
    assert_eq!(json["diagnostics"].as_array().map(Vec::len), Some(0));
    assert_eq!(json["views"][0]["rows"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        json["views"][0]["rows"][0]["document_path"],
        Value::String("TaskNotes/Tasks/Write Docs.md".to_string())
    );
}

#[test]
fn tasks_show_json_output_reports_tasknote_details() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("tasknotes", &vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "show",
            "Write Docs",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["path"], "TaskNotes/Tasks/Write Docs.md");
    assert_eq!(json["title"], "Write docs");
    assert_eq!(json["status"], "in-progress");
    assert_eq!(json["status_type"], "IN_PROGRESS");
    assert_eq!(json["archived"], false);
    assert_eq!(json["priority"], "high");
    assert_eq!(json["due"], "2026-04-10");
    assert_eq!(json["contexts"], serde_json::json!(["@desk", "@work"]));
    assert_eq!(
        json["projects"],
        serde_json::json!(["[[Projects/Website]]"])
    );
    assert_eq!(json["blocked_by"].as_array().map(Vec::len), Some(1));
    assert_eq!(json["reminders"].as_array().map(Vec::len), Some(1));
    assert_eq!(json["time_entries"].as_array().map(Vec::len), Some(1));
    assert_eq!(json["total_time_minutes"], serde_json::json!(60));
    assert_eq!(json["active_time_minutes"], serde_json::json!(0));
    assert_eq!(json["estimate_remaining_minutes"], serde_json::json!(30));
    assert_eq!(json["efficiency_ratio"], serde_json::json!(67));
    assert_eq!(json["custom_fields"]["effort"], serde_json::json!(3.0));
    assert_eq!(json["frontmatter"]["title"], "Write docs");
    assert_eq!(json["body"], "Write the docs body.\n");
}

#[test]
fn tasks_track_start_stop_and_status_json_output_manage_time_entries() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("tasknotes", &vault_root);
    run_scan(&vault_root);

    let start_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "track",
            "start",
            "Prep Outline",
            "--description",
            "Deep work",
            "--no-commit",
        ])
        .assert()
        .success();
    let start_json = parse_stdout_json(&start_assert);

    assert_eq!(start_json["action"], "start");
    assert_eq!(start_json["path"], "TaskNotes/Tasks/Prep Outline.md");
    assert_eq!(start_json["session"]["description"], "Deep work");
    assert_eq!(start_json["session"]["active"], true);

    let status_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "track",
            "status",
        ])
        .assert()
        .success();
    let status_json = parse_stdout_json(&status_assert);
    assert_eq!(status_json["total_active_sessions"], serde_json::json!(1));
    assert_eq!(
        status_json["active_sessions"][0]["path"],
        "TaskNotes/Tasks/Prep Outline.md"
    );

    let stop_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "track",
            "stop",
            "--no-commit",
        ])
        .assert()
        .success();
    let stop_json = parse_stdout_json(&stop_assert);

    assert_eq!(stop_json["action"], "stop");
    assert_eq!(stop_json["path"], "TaskNotes/Tasks/Prep Outline.md");
    assert_eq!(stop_json["session"]["active"], false);
    assert!(stop_json["session"]["end_time"].is_string());

    let show_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "show",
            "Prep Outline",
        ])
        .assert()
        .success();
    let show_json = parse_stdout_json(&show_assert);
    assert_eq!(show_json["time_entries"].as_array().map(Vec::len), Some(1));
    assert!(show_json["time_entries"][0]["endTime"].is_string());
}

#[test]
fn tasks_track_log_and_summary_json_output_report_totals() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("tasknotes", &vault_root);
    run_scan(&vault_root);

    let log_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "track",
            "log",
            "Write Docs",
        ])
        .assert()
        .success();
    let log_json = parse_stdout_json(&log_assert);
    assert_eq!(log_json["path"], "TaskNotes/Tasks/Write Docs.md");
    assert_eq!(log_json["entries"].as_array().map(Vec::len), Some(1));
    assert_eq!(log_json["total_time_minutes"], serde_json::json!(60));
    assert_eq!(
        log_json["estimate_remaining_minutes"],
        serde_json::json!(30)
    );
    assert_eq!(log_json["efficiency_ratio"], serde_json::json!(67));

    let summary_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "track",
            "summary",
            "--period",
            "all",
        ])
        .assert()
        .success();
    let summary_json = parse_stdout_json(&summary_assert);
    assert_eq!(summary_json["period"], "all");
    assert_eq!(summary_json["total_minutes"], serde_json::json!(60));
    assert_eq!(summary_json["tasks_with_time"], serde_json::json!(1));
    assert_eq!(
        summary_json["top_tasks"][0]["path"],
        "TaskNotes/Tasks/Write Docs.md"
    );
    assert_eq!(
        summary_json["top_projects"][0]["project"],
        "[[Projects/Website]]"
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn tasks_pomodoro_start_stop_and_status_json_output_manage_task_storage() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("tasknotes", &vault_root);
    fs::create_dir_all(vault_root.join(".vulcan")).expect("config dir should exist");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        concat!(
            "[tasknotes.pomodoro]\n",
            "work_duration = 30\n",
            "short_break = 5\n",
            "long_break = 20\n",
            "long_break_interval = 4\n",
            "storage_location = \"task\"\n",
        ),
    )
    .expect("config should be written");
    run_scan(&vault_root);

    let start_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "pomodoro",
            "start",
            "Write Docs",
            "--no-commit",
        ])
        .assert()
        .success();
    let start_json = parse_stdout_json(&start_assert);

    assert_eq!(start_json["action"], "start");
    assert_eq!(
        start_json["storage_note_path"],
        "TaskNotes/Tasks/Write Docs.md"
    );
    assert_eq!(start_json["task_path"], "TaskNotes/Tasks/Write Docs.md");
    assert_eq!(start_json["session"]["planned_duration_minutes"], 30);
    assert_eq!(start_json["session"]["active"], true);
    assert_eq!(start_json["suggested_break_type"], "short-break");
    assert_eq!(start_json["suggested_break_minutes"], 5);

    let task_file = fs::read_to_string(vault_root.join("TaskNotes/Tasks/Write Docs.md"))
        .expect("task file should be readable");
    assert!(task_file.contains("pomodoros:"));
    assert!(task_file.contains("plannedDuration: 30"));

    let status_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "pomodoro",
            "status",
        ])
        .assert()
        .success();
    let status_json = parse_stdout_json(&status_assert);
    assert_eq!(
        status_json["active"]["storage_note_path"],
        "TaskNotes/Tasks/Write Docs.md"
    );
    assert_eq!(
        status_json["active"]["task_path"],
        "TaskNotes/Tasks/Write Docs.md"
    );
    assert_eq!(status_json["active"]["session"]["active"], true);

    let stop_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "pomodoro",
            "stop",
            "Write Docs",
            "--no-commit",
        ])
        .assert()
        .success();
    let stop_json = parse_stdout_json(&stop_assert);

    assert_eq!(stop_json["action"], "stop");
    assert_eq!(stop_json["task_path"], "TaskNotes/Tasks/Write Docs.md");
    assert_eq!(stop_json["session"]["active"], false);
    assert_eq!(stop_json["session"]["interrupted"], true);
    assert!(stop_json["session"]["end_time"].is_string());

    let show_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "show",
            "Write Docs",
        ])
        .assert()
        .success();
    let show_json = parse_stdout_json(&show_assert);
    assert!(show_json["frontmatter"]["pomodoros"].is_array());
    assert_eq!(
        show_json["frontmatter"]["pomodoros"][0]["interrupted"],
        true
    );
}

#[test]
fn tasks_pomodoro_daily_note_storage_completes_due_sessions() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("tasknotes", &vault_root);
    fs::create_dir_all(vault_root.join(".vulcan")).expect("config dir should exist");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        concat!(
            "[tasknotes.pomodoro]\n",
            "work_duration = 1\n",
            "short_break = 3\n",
            "long_break = 20\n",
            "long_break_interval = 1\n",
            "storage_location = \"daily-note\"\n",
        ),
    )
    .expect("config should be written");
    run_scan(&vault_root);

    let start_assert = cargo_vulcan_fixed_now()
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "pomodoro",
            "start",
            "Prep Outline",
            "--no-commit",
        ])
        .assert()
        .success();
    let start_json = parse_stdout_json(&start_assert);
    let daily_note_path = start_json["storage_note_path"]
        .as_str()
        .expect("storage note path should be a string");
    assert_eq!(daily_note_path, "Journal/Daily/2026-04-04.md");

    let start_time = start_json["session"]["start_time"]
        .as_str()
        .expect("start time should be a string")
        .to_string();
    let daily_note = vault_root.join(daily_note_path);
    let updated = fs::read_to_string(&daily_note)
        .expect("daily note should be readable")
        .replace(&start_time, "2026-04-04T08:00:00Z");
    fs::write(&daily_note, updated).expect("daily note should be updated");
    run_scan(&vault_root);

    let status_assert = cargo_vulcan_fixed_now()
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "pomodoro",
            "status",
        ])
        .assert()
        .success();
    let status_json = parse_stdout_json(&status_assert);

    assert!(status_json["active"].is_null());
    assert_eq!(status_json["completed_work_sessions"], 1);
    assert_eq!(status_json["suggested_break_type"], "long-break");
    assert_eq!(status_json["suggested_break_minutes"], 20);

    let rendered = fs::read_to_string(&daily_note).expect("daily note should still be readable");
    assert!(rendered.contains("completed: true"));
    assert!(rendered.contains("taskPath: TaskNotes/Tasks/Prep Outline.md"));
}

#[test]
fn tasks_due_json_output_lists_due_tasknotes() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("tasknotes", &vault_root);
    let write_docs_path = vault_root.join("TaskNotes/Tasks/Write Docs.md");
    let write_docs = fs::read_to_string(&write_docs_path).expect("write docs fixture should exist");
    fs::write(
        &write_docs_path,
        write_docs.replace("due: \"2026-04-10\"", "due: \"2999-01-01\""),
    )
    .expect("write docs due date should be updated");
    fs::write(
        vault_root.join("TaskNotes/Tasks/Old Task.md"),
        concat!(
            "---\n",
            "title: \"Old task\"\n",
            "status: \"open\"\n",
            "priority: \"low\"\n",
            "due: \"2000-01-01\"\n",
            "tags: [\"task\"]\n",
            "dateCreated: \"1999-12-31T08:00:00Z\"\n",
            "dateModified: \"1999-12-31T08:00:00Z\"\n",
            "---\n",
        ),
    )
    .expect("old task should be written");
    run_scan(&vault_root);

    let due_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "due",
            "--within",
            "2000y",
        ])
        .assert()
        .success();
    let due_json = parse_stdout_json(&due_assert);
    let tasks = due_json["tasks"]
        .as_array()
        .expect("tasks should be an array");

    assert!(tasks
        .iter()
        .any(|task| task["path"] == "TaskNotes/Tasks/Write Docs.md" && task["overdue"] == false));
    assert!(tasks
        .iter()
        .any(|task| task["path"] == "TaskNotes/Tasks/Old Task.md" && task["overdue"] == true));
}

#[test]
fn tasks_reminders_json_output_evaluates_relative_and_absolute_reminders() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("tasknotes", &vault_root);
    fs::write(
        vault_root.join("TaskNotes/Tasks/Future Reminder.md"),
        concat!(
            "---\n",
            "title: \"Future reminder\"\n",
            "status: \"open\"\n",
            "priority: \"normal\"\n",
            "tags: [\"task\"]\n",
            "reminders:\n",
            "  - id: \"abs-1\"\n",
            "    type: \"absolute\"\n",
            "    absoluteTime: \"2999-01-01T09:00:00Z\"\n",
            "    description: \"Far future\"\n",
            "dateCreated: \"2026-04-01T08:00:00Z\"\n",
            "dateModified: \"2026-04-01T08:00:00Z\"\n",
            "---\n",
        ),
    )
    .expect("future reminder task should be written");
    run_scan(&vault_root);

    let reminders_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "reminders",
            "--upcoming",
            "2000y",
        ])
        .assert()
        .success();
    let reminders_json = parse_stdout_json(&reminders_assert);
    let reminders = reminders_json["reminders"]
        .as_array()
        .expect("reminders should be an array");

    assert!(reminders.iter().any(|reminder| {
        reminder["path"] == "TaskNotes/Tasks/Write Docs.md"
            && reminder["reminder_id"] == "rem-1"
            && reminder["notify_at"] == "2026-04-09T23:45:00Z"
    }));
    assert!(reminders.iter().any(|reminder| {
        reminder["path"] == "TaskNotes/Tasks/Future Reminder.md"
            && reminder["reminder_id"] == "abs-1"
            && reminder["reminder_type"] == "absolute"
            && reminder["notify_at"] == "2999-01-01T09:00:00Z"
    }));
}

#[test]
fn tasks_set_json_output_updates_tasknote_frontmatter_and_rescans() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("tasknotes", &vault_root);
    run_scan(&vault_root);

    let set_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "set",
            "Write Docs",
            "due",
            "2026-04-12",
            "--no-commit",
        ])
        .assert()
        .success();
    let set_json = parse_stdout_json(&set_assert);

    assert_eq!(set_json["action"], "set");
    assert_eq!(set_json["path"], "TaskNotes/Tasks/Write Docs.md");

    let show_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "show",
            "Write Docs",
        ])
        .assert()
        .success();
    let show_json = parse_stdout_json(&show_assert);
    assert_eq!(show_json["due"], "2026-04-12");
}

#[test]
fn tasks_complete_json_output_marks_non_recurring_task_done() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("tasknotes", &vault_root);
    fs::write(
        vault_root.join("TaskNotes/Tasks/Ship Release.md"),
        concat!(
            "---\n",
            "title: \"Ship release\"\n",
            "status: \"open\"\n",
            "priority: \"normal\"\n",
            "tags: [\"task\", \"release\"]\n",
            "dateCreated: \"2026-04-01T08:00:00Z\"\n",
            "dateModified: \"2026-04-01T08:00:00Z\"\n",
            "---\n",
            "\n",
            "Ship the release checklist.\n",
        ),
    )
    .expect("tasknote fixture should be written");
    run_scan(&vault_root);

    let complete_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "complete",
            "Ship Release",
            "--no-commit",
        ])
        .assert()
        .success();
    let complete_json = parse_stdout_json(&complete_assert);

    assert_eq!(complete_json["action"], "complete");
    assert_eq!(complete_json["path"], "TaskNotes/Tasks/Ship Release.md");

    let show_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "show",
            "Ship Release",
        ])
        .assert()
        .success();
    let show_json = parse_stdout_json(&show_assert);
    assert_eq!(show_json["status"], "done");
    assert_eq!(show_json["completed"], true);
    assert!(show_json["completed_date"]
        .as_str()
        .is_some_and(|value| value.len() == 10));
}

#[test]
fn tasks_complete_json_output_records_recurring_instance_completion() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("tasknotes", &vault_root);
    run_scan(&vault_root);

    let complete_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "complete",
            "Write Docs",
            "--date",
            "2026-04-10",
            "--no-commit",
        ])
        .assert()
        .success();
    let complete_json = parse_stdout_json(&complete_assert);

    assert_eq!(complete_json["action"], "complete");

    let show_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "show",
            "Write Docs",
        ])
        .assert()
        .success();
    let show_json = parse_stdout_json(&show_assert);
    assert!(show_json["complete_instances"]
        .as_array()
        .is_some_and(|items| items.iter().any(|item| item == "2026-04-10")));

    let next_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "next",
            "2",
            "--from",
            "2026-04-04",
        ])
        .assert()
        .success();
    let next_json = parse_stdout_json(&next_assert);
    assert_eq!(next_json["occurrences"][0]["date"], "2026-04-17");
    assert_eq!(next_json["occurrences"][1]["date"], "2026-04-24");
}

#[test]
fn tasks_complete_json_output_marks_inline_tasks_done() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("tasknotes", &vault_root);
    fs::write(
        vault_root.join("Inbox.md"),
        "- [ ] Ship docs #ops\n- [ ] Follow up later\n",
    )
    .expect("inline task fixture should be written");
    run_scan(&vault_root);

    let complete_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "complete",
            "Ship docs #ops",
            "--date",
            "2026-04-09",
            "--no-commit",
        ])
        .assert()
        .success();
    let complete_json = parse_stdout_json(&complete_assert);

    assert_eq!(complete_json["action"], "complete");
    assert_eq!(complete_json["path"], "Inbox.md");
    assert_eq!(
        complete_json["changes"][0]["before"],
        Value::String("- [ ] Ship docs #ops".to_string())
    );
    assert_eq!(
        complete_json["changes"][0]["after"],
        Value::String("- [x] Ship docs #ops ✅ 2026-04-09".to_string())
    );

    let updated = fs::read_to_string(vault_root.join("Inbox.md"))
        .expect("updated inline task note should be readable");
    assert!(updated.contains("- [x] Ship docs #ops ✅ 2026-04-09"));

    let list_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "list",
            "--source",
            "inline",
            "--filter",
            "status is done and description includes \"Ship docs\"",
        ])
        .assert()
        .success();
    let list_json = parse_stdout_json(&list_assert);

    assert_eq!(list_json["result_count"], Value::Number(1.into()));
    assert_eq!(
        list_json["tasks"][0]["text"],
        Value::String("Ship docs #ops ✅ 2026-04-09".to_string())
    );
}

#[test]
fn tasks_convert_json_output_turns_existing_note_into_tasknote() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("tasknotes", &vault_root);
    fs::create_dir_all(vault_root.join("Notes")).expect("notes dir should be created");
    fs::write(
        vault_root.join("Notes/Idea.md"),
        concat!(
            "---\n",
            "owner: Alice\n",
            "tags:\n",
            "  - research\n",
            "---\n",
            "\n",
            "# Follow up\n",
            "\n",
            "Need task details.\n",
        ),
    )
    .expect("source note should be written");
    run_scan(&vault_root);

    let convert_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "convert",
            "Notes/Idea.md",
            "--no-commit",
        ])
        .assert()
        .success();
    let convert_json = parse_stdout_json(&convert_assert);

    assert_eq!(convert_json["action"], "convert");
    assert_eq!(convert_json["mode"], "note");
    assert_eq!(convert_json["source_path"], "Notes/Idea.md");
    assert_eq!(convert_json["target_path"], "Notes/Idea.md");
    assert_eq!(convert_json["title"], "Idea");
    assert_eq!(convert_json["created"], Value::Bool(false));
    assert_eq!(convert_json["frontmatter"]["owner"], "Alice");
    assert_eq!(convert_json["frontmatter"]["title"], "Idea");
    assert_eq!(convert_json["frontmatter"]["status"], "open");
    assert_eq!(convert_json["frontmatter"]["priority"], "normal");
    assert!(convert_json["frontmatter"]["tags"]
        .as_array()
        .is_some_and(|tags| tags.iter().any(|tag| tag == "research")));
    assert!(convert_json["frontmatter"]["tags"]
        .as_array()
        .is_some_and(|tags| tags.iter().any(|tag| tag == "task")));

    let updated =
        fs::read_to_string(vault_root.join("Notes/Idea.md")).expect("updated note should exist");
    assert!(updated.contains("owner: Alice"));
    assert!(updated.contains("title: Idea"));
    assert!(updated.contains("status: open"));
    assert!(updated.contains("priority: normal"));
    assert!(updated.contains("- research"));
    assert!(updated.contains("- task"));

    let show_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "show",
            "Notes/Idea.md",
        ])
        .assert()
        .success();
    let show_json = parse_stdout_json(&show_assert);

    assert_eq!(show_json["title"], "Idea");
    assert_eq!(show_json["status"], "open");
    assert_eq!(show_json["priority"], "normal");
    assert_eq!(show_json["body"], "# Follow up\n\nNeed task details.\n");
}

#[test]
fn tasks_convert_json_output_converts_checkbox_line_into_task_file() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("tasknotes", &vault_root);
    fs::create_dir_all(vault_root.join("Notes")).expect("notes dir should be created");
    fs::write(
        vault_root.join("Notes/Inbox.md"),
        concat!(
            "# Inbox\n",
            "\n",
            "- [ ] Ship docs due 2026-04-10 @desk #ops\n",
            "- [ ] Leave alone\n",
        ),
    )
    .expect("source note should be written");
    run_scan(&vault_root);

    let convert_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "convert",
            "Notes/Inbox.md",
            "--line",
            "3",
            "--no-commit",
        ])
        .assert()
        .success();
    let convert_json = parse_stdout_json(&convert_assert);

    assert_eq!(convert_json["action"], "convert");
    assert_eq!(convert_json["mode"], "line");
    assert_eq!(convert_json["source_path"], "Notes/Inbox.md");
    assert_eq!(convert_json["target_path"], "TaskNotes/Tasks/Ship docs.md");
    assert_eq!(convert_json["line_number"], Value::Number(3.into()));
    assert_eq!(convert_json["title"], "Ship docs");
    assert_eq!(convert_json["created"], Value::Bool(true));
    assert_eq!(
        convert_json["source_changes"][0]["before"],
        Value::String("- [ ] Ship docs due 2026-04-10 @desk #ops".to_string())
    );
    assert_eq!(
        convert_json["source_changes"][0]["after"],
        Value::String("- [[TaskNotes/Tasks/Ship docs]]".to_string())
    );
    assert_eq!(convert_json["frontmatter"]["due"], "2026-04-10");
    assert_eq!(
        convert_json["frontmatter"]["contexts"],
        serde_json::json!(["@desk"])
    );
    assert!(convert_json["frontmatter"]["tags"]
        .as_array()
        .is_some_and(|tags| tags.iter().any(|tag| tag == "ops")));
    assert!(convert_json["frontmatter"]["tags"]
        .as_array()
        .is_some_and(|tags| tags.iter().any(|tag| tag == "task")));

    let updated =
        fs::read_to_string(vault_root.join("Notes/Inbox.md")).expect("updated note should exist");
    assert!(updated.contains("- [[TaskNotes/Tasks/Ship docs]]"));
    assert!(updated.contains("- [ ] Leave alone"));

    let show_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "show",
            "TaskNotes/Tasks/Ship docs.md",
        ])
        .assert()
        .success();
    let show_json = parse_stdout_json(&show_assert);

    assert_eq!(show_json["title"], "Ship docs");
    assert_eq!(show_json["due"], "2026-04-10");
    assert_eq!(show_json["contexts"], serde_json::json!(["@desk"]));
    assert_eq!(show_json["body"], "");
}

#[test]
fn tasks_convert_json_output_converts_heading_section_into_task_file() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("tasknotes", &vault_root);
    fs::create_dir_all(vault_root.join("Notes")).expect("notes dir should be created");
    fs::write(
        vault_root.join("Notes/Plan.md"),
        concat!(
            "# Plan\n",
            "\n",
            "## Ship release\n",
            "\n",
            "Coordinate docs.\n",
            "- [ ] Notify team\n",
            "\n",
            "## Later\n",
            "\n",
            "Other notes.\n",
        ),
    )
    .expect("source note should be written");
    run_scan(&vault_root);

    let convert_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "convert",
            "Notes/Plan.md",
            "--line",
            "3",
            "--no-commit",
        ])
        .assert()
        .success();
    let convert_json = parse_stdout_json(&convert_assert);

    assert_eq!(convert_json["mode"], "line");
    assert_eq!(
        convert_json["target_path"],
        "TaskNotes/Tasks/Ship release.md"
    );
    assert_eq!(convert_json["title"], "Ship release");
    assert!(convert_json["source_changes"][0]["before"]
        .as_str()
        .is_some_and(|before| before.contains("## Ship release")));
    assert_eq!(
        convert_json["source_changes"][0]["after"],
        Value::String("- [[TaskNotes/Tasks/Ship release]]".to_string())
    );
    assert_eq!(
        convert_json["body"],
        "Coordinate docs.\n- [ ] Notify team\n"
    );

    let updated =
        fs::read_to_string(vault_root.join("Notes/Plan.md")).expect("updated note should exist");
    assert!(updated.contains("- [[TaskNotes/Tasks/Ship release]]"));
    assert!(updated.contains("## Later"));
    assert!(!updated.contains("## Ship release"));

    let show_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "show",
            "TaskNotes/Tasks/Ship release.md",
        ])
        .assert()
        .success();
    let show_json = parse_stdout_json(&show_assert);

    assert_eq!(show_json["title"], "Ship release");
    assert_eq!(show_json["body"], "Coordinate docs.\n- [ ] Notify team\n");
}

#[test]
fn tasks_create_json_output_appends_inline_task_to_default_inbox_note() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("tasknotes", &vault_root);
    fs::create_dir_all(vault_root.join(".vulcan")).expect("config dir should exist");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        concat!(
            "[tasks]\n",
            "global_filter = \"#task\"\n",
            "remove_global_filter = true\n",
            "set_created_date = true\n",
        ),
    )
    .expect("config should be written");

    let create_assert = cargo_vulcan_fixed_now()
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "create",
            "Review release tomorrow @desk #ops high priority",
            "--no-commit",
        ])
        .assert()
        .success();
    let create_json = parse_stdout_json(&create_assert);

    assert_eq!(create_json["action"], "create");
    assert_eq!(create_json["path"], "Inbox.md");
    assert_eq!(create_json["task"], "Inbox.md:1");
    assert_eq!(create_json["created_note"], true);
    assert_eq!(create_json["used_nlp"], true);
    assert_eq!(create_json["due"], "2026-04-05");
    assert_eq!(create_json["priority"], "high");
    assert_eq!(create_json["contexts"], serde_json::json!(["@desk"]));
    assert_eq!(create_json["tags"], serde_json::json!(["ops", "task"]));

    let updated = fs::read_to_string(vault_root.join("Inbox.md")).expect("inbox note should exist");
    assert_eq!(
        updated,
        "- [ ] Review release @desk #ops #task 🗓️ 2026-04-05 ➕ 2026-04-04 🔺\n"
    );

    let list_assert = cargo_vulcan_fixed_now()
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "list",
            "--source",
            "inline",
        ])
        .assert()
        .success();
    let list_json = parse_stdout_json(&list_assert);
    assert_eq!(list_json["result_count"], Value::Number(1.into()));
    assert_eq!(list_json["tasks"][0]["path"], "Inbox.md");
    assert_eq!(
        list_json["tasks"][0]["text"],
        "Review release @desk #ops 🗓️ 2026-04-05 ➕ 2026-04-04 🔺"
    );
}

#[test]
fn tasks_create_json_output_honors_explicit_target_and_flags() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("tasknotes", &vault_root);
    run_scan(&vault_root);

    let create_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "create",
            "Ship checklist",
            "--in",
            "Website",
            "--due",
            "2026-04-12",
            "--priority",
            "low",
            "--no-commit",
        ])
        .assert()
        .success();
    let create_json = parse_stdout_json(&create_assert);

    assert_eq!(create_json["path"], "Projects/Website.md");
    assert_eq!(create_json["task"], "Projects/Website.md:3");
    assert_eq!(create_json["created_note"], false);
    assert_eq!(create_json["due"], "2026-04-12");
    assert_eq!(create_json["priority"], "low");
    assert_eq!(create_json["line"], "- [ ] Ship checklist 🗓️ 2026-04-12 🔽");

    let updated = fs::read_to_string(vault_root.join("Projects/Website.md"))
        .expect("project note should exist");
    assert_eq!(
        updated,
        "# Website\n\n- [ ] Ship checklist 🗓️ 2026-04-12 🔽\n"
    );

    let list_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "list",
            "--source",
            "inline",
            "--filter",
            "path includes Website",
        ])
        .assert()
        .success();
    let list_json = parse_stdout_json(&list_assert);
    assert_eq!(list_json["result_count"], Value::Number(1.into()));
    assert_eq!(list_json["tasks"][0]["due"], "2026-04-12");
    assert_eq!(list_json["tasks"][0]["priority"], "low");
}

#[test]
fn tasks_reschedule_json_output_updates_tasknote_due_date() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("tasknotes", &vault_root);
    run_scan(&vault_root);

    let reschedule_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "reschedule",
            "Write Docs",
            "--due",
            "2026-04-12",
            "--no-commit",
        ])
        .assert()
        .success();
    let reschedule_json = parse_stdout_json(&reschedule_assert);

    assert_eq!(reschedule_json["action"], "reschedule");
    assert_eq!(reschedule_json["path"], "TaskNotes/Tasks/Write Docs.md");

    let show_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "show",
            "Write Docs",
        ])
        .assert()
        .success();
    let show_json = parse_stdout_json(&show_assert);
    assert_eq!(show_json["due"], "2026-04-12");
}

#[test]
fn tasks_reschedule_json_output_replaces_inline_due_marker() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("tasknotes", &vault_root);
    fs::write(
        vault_root.join("Inbox.md"),
        "- [ ] Review release #ops 🗓️ 2026-04-09\n",
    )
    .expect("inline task note should be written");
    run_scan(&vault_root);

    let reschedule_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "reschedule",
            "Inbox.md:1",
            "--due",
            "2026-04-11",
            "--no-commit",
        ])
        .assert()
        .success();
    let reschedule_json = parse_stdout_json(&reschedule_assert);

    assert_eq!(reschedule_json["action"], "reschedule");
    assert_eq!(reschedule_json["path"], "Inbox.md");
    assert_eq!(
        reschedule_json["changes"][0]["after"],
        "- [ ] Review release #ops 🗓️ 2026-04-11"
    );

    let updated =
        fs::read_to_string(vault_root.join("Inbox.md")).expect("updated note should exist");
    assert_eq!(updated, "- [ ] Review release #ops 🗓️ 2026-04-11\n");

    let list_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "list",
            "--source",
            "inline",
        ])
        .assert()
        .success();
    let list_json = parse_stdout_json(&list_assert);
    assert_eq!(list_json["result_count"], Value::Number(1.into()));
    assert_eq!(list_json["tasks"][0]["due"], "2026-04-11");
}

#[test]
fn tasks_archive_json_output_moves_completed_task_into_archive_folder() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("tasknotes", &vault_root);
    run_scan(&vault_root);

    let archive_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "archive",
            "Prep Outline",
            "--no-commit",
        ])
        .assert()
        .success();
    let archive_json = parse_stdout_json(&archive_assert);

    assert_eq!(archive_json["action"], "archive");
    assert_eq!(
        archive_json["moved_from"],
        "TaskNotes/Tasks/Prep Outline.md"
    );
    assert_eq!(
        archive_json["moved_to"],
        "TaskNotes/Archive/Prep Outline.md"
    );
    assert!(!vault_root.join("TaskNotes/Tasks/Prep Outline.md").exists());
    assert!(vault_root
        .join("TaskNotes/Archive/Prep Outline.md")
        .exists());

    let show_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "show",
            "TaskNotes/Archive/Prep Outline.md",
        ])
        .assert()
        .success();
    let show_json = parse_stdout_json(&show_assert);
    assert_eq!(show_json["archived"], true);
    assert!(show_json["tags"]
        .as_array()
        .is_some_and(|tags| tags.iter().any(|tag| tag == "archived")));
}

#[test]
fn task_commands_process_due_tasknotes_auto_archive() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("tasknotes", &vault_root);
    fs::create_dir_all(vault_root.join(".vulcan")).expect("config dir should exist");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        concat!(
            "[tasknotes]\n",
            "archive_folder = \"TaskNotes/Archive\"\n",
            "\n",
            "[[tasknotes.statuses]]\n",
            "id = \"open\"\n",
            "value = \"open\"\n",
            "label = \"Open\"\n",
            "color = \"#808080\"\n",
            "isCompleted = false\n",
            "order = 1\n",
            "autoArchive = false\n",
            "autoArchiveDelay = 5\n",
            "\n",
            "[[tasknotes.statuses]]\n",
            "id = \"done\"\n",
            "value = \"done\"\n",
            "label = \"Done\"\n",
            "color = \"#00aa00\"\n",
            "isCompleted = true\n",
            "order = 2\n",
            "autoArchive = true\n",
            "autoArchiveDelay = 5\n",
        ),
    )
    .expect("tasknotes config should be written");
    fs::write(
        vault_root.join("TaskNotes/Tasks/Old Done.md"),
        concat!(
            "---\n",
            "title: Old Done\n",
            "status: done\n",
            "priority: normal\n",
            "completedDate: 2026-04-03T09:00:00Z\n",
            "tags:\n",
            "  - task\n",
            "---\n",
            "\n",
            "Archived soon.\n",
        ),
    )
    .expect("completed task should be written");

    let list_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "list",
            "--source",
            "file",
        ])
        .assert()
        .success();
    let list_json = parse_stdout_json(&list_assert);

    assert!(!vault_root.join("TaskNotes/Tasks/Old Done.md").exists());
    assert!(vault_root.join("TaskNotes/Archive/Old Done.md").exists());
    assert!(list_json["tasks"].as_array().is_some_and(|tasks| tasks
        .iter()
        .all(|task| task["path"] != "TaskNotes/Tasks/Old Done.md")));
}

#[test]
fn tasks_edit_json_output_opens_editor_and_rescans_tasknote() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("tasknotes", &vault_root);
    run_scan(&vault_root);
    let editor = write_test_editor(
        temp_dir.path(),
        concat!(
            "---\n",
            "title: \"Write docs updated\"\n",
            "status: \"in-progress\"\n",
            "priority: \"high\"\n",
            "tags: [\"task\", \"docs\"]\n",
            "dateCreated: \"2026-04-02T08:00:00Z\"\n",
            "dateModified: \"2026-04-03T11:00:00Z\"\n",
            "---\n",
            "\n",
            "Updated task body.\n",
        ),
    );

    let edit_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .env("EDITOR", editor)
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "edit",
            "Write Docs",
            "--no-commit",
        ])
        .assert()
        .success();
    let edit_json = parse_stdout_json(&edit_assert);

    assert_eq!(edit_json["path"], "TaskNotes/Tasks/Write Docs.md");
    assert_eq!(edit_json["created"], false);
    assert_eq!(edit_json["rescanned"], true);

    let show_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "show",
            "Write Docs",
        ])
        .assert()
        .success();
    let show_json = parse_stdout_json(&show_assert);
    assert_eq!(show_json["title"], "Write docs updated");
    assert_eq!(show_json["body"], "Updated task body.\n");
}

#[test]
fn tasks_add_json_output_creates_tasknote_from_natural_language() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("tasknotes", &vault_root);

    let add_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "add",
            "Buy groceries 2026-04-10 at 3pm @home #errands high priority",
            "--no-commit",
        ])
        .assert()
        .success();
    let add_json = parse_stdout_json(&add_assert);

    assert_eq!(add_json["action"], "add");
    assert_eq!(add_json["used_nlp"], true);
    assert_eq!(add_json["path"], "TaskNotes/Tasks/Buy groceries.md");
    assert_eq!(add_json["title"], "Buy groceries");
    assert_eq!(add_json["priority"], "high");
    assert_eq!(add_json["due"], "2026-04-10T15:00:00");
    assert_eq!(add_json["contexts"], serde_json::json!(["@home"]));
    assert_eq!(add_json["tags"], serde_json::json!(["task", "errands"]));
    assert!(vault_root.join("TaskNotes/Tasks/Buy groceries.md").exists());

    let show_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "show",
            "TaskNotes/Tasks/Buy groceries.md",
        ])
        .assert()
        .success();
    let show_json = parse_stdout_json(&show_assert);
    assert_eq!(show_json["title"], "Buy groceries");
    assert_eq!(show_json["due"], "2026-04-10T15:00:00");
    assert_eq!(show_json["contexts"], serde_json::json!(["@home"]));
    assert!(show_json["tags"]
        .as_array()
        .is_some_and(|tags| tags.iter().any(|tag| tag == "errands")));
}

#[test]
fn tasks_add_json_output_honors_explicit_flags_and_no_nlp() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("tasknotes", &vault_root);

    let add_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "add",
            "Literal @home #tag",
            "--no-nlp",
            "--status",
            "in-progress",
            "--priority",
            "high",
            "--due",
            "2026-04-12",
            "--scheduled",
            "2026-04-11",
            "--context",
            "@desk",
            "--project",
            "Projects/Website",
            "--tag",
            "docs",
            "--no-commit",
        ])
        .assert()
        .success();
    let add_json = parse_stdout_json(&add_assert);

    assert_eq!(add_json["used_nlp"], false);
    assert_eq!(add_json["title"], "Literal @home #tag");
    assert_eq!(add_json["status"], "in-progress");
    assert_eq!(add_json["priority"], "high");
    assert_eq!(add_json["due"], "2026-04-12");
    assert_eq!(add_json["scheduled"], "2026-04-11");
    assert_eq!(add_json["contexts"], serde_json::json!(["@desk"]));
    assert_eq!(
        add_json["projects"],
        serde_json::json!(["[[Projects/Website]]"])
    );
    assert_eq!(add_json["tags"], serde_json::json!(["task", "docs"]));

    let show_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "show",
            "TaskNotes/Tasks/Literal @home #tag.md",
        ])
        .assert()
        .success();
    let show_json = parse_stdout_json(&show_assert);
    assert_eq!(show_json["title"], "Literal @home #tag");
    assert_eq!(show_json["status"], "in-progress");
    assert_eq!(show_json["due"], "2026-04-12");
    assert_eq!(show_json["contexts"], serde_json::json!(["@desk"]));
}

#[test]
fn tasks_add_json_output_applies_template_frontmatter_and_body() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("tasknotes", &vault_root);
    fs::create_dir_all(vault_root.join(".vulcan")).expect("config dir should exist");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        "[templates]\nobsidian_folder = \"Templates\"\n",
    )
    .expect("template config should be written");
    fs::create_dir_all(vault_root.join("Templates")).expect("templates dir should exist");
    fs::write(
        vault_root.join("Templates/Task.md"),
        concat!(
            "---\n",
            "owner: ops\n",
            "tags: [templated]\n",
            "---\n",
            "\n",
            "Template body.\n",
        ),
    )
    .expect("task template should be written");

    let add_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "add",
            "Template demo",
            "--template",
            "Task",
            "--no-commit",
        ])
        .assert()
        .success();
    let add_json = parse_stdout_json(&add_assert);

    assert_eq!(add_json["template"], "Task");
    assert_eq!(add_json["frontmatter"]["owner"], "ops");
    assert_eq!(add_json["body"], "Template body.\n");
    assert_eq!(add_json["tags"], serde_json::json!(["task"]));

    let source = fs::read_to_string(vault_root.join("TaskNotes/Tasks/Template demo.md"))
        .expect("created task should exist");
    assert!(source.contains("owner: ops"));
    assert!(source.contains("Template body."));
}

#[test]
fn tasks_add_json_output_applies_default_reminders_from_config() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("tasknotes", &vault_root);
    fs::create_dir_all(vault_root.join(".vulcan")).expect("config dir should exist");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        concat!(
            "[tasknotes.task_creation_defaults]\n",
            "\n",
            "[[tasknotes.task_creation_defaults.default_reminders]]\n",
            "id = \"rem-relative\"\n",
            "type = \"relative\"\n",
            "related_to = \"due\"\n",
            "offset = 15\n",
            "unit = \"minutes\"\n",
            "direction = \"before\"\n",
            "description = \"Before due\"\n",
        ),
    )
    .expect("tasknotes config should be written");

    let add_assert = cargo_vulcan_fixed_now()
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "add",
            "Prep launch tomorrow",
            "--no-commit",
        ])
        .assert()
        .success();
    let add_json = parse_stdout_json(&add_assert);

    assert_eq!(add_json["due"], "2026-04-05");
    assert_eq!(
        add_json["frontmatter"]["reminders"][0]["id"],
        Value::String("rem-relative".to_string())
    );
    assert_eq!(
        add_json["frontmatter"]["reminders"][0]["offset"],
        Value::String("-PT15M".to_string())
    );

    let show_assert = cargo_vulcan_fixed_now()
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "show",
            "TaskNotes/Tasks/Prep launch.md",
        ])
        .assert()
        .success();
    let show_json = parse_stdout_json(&show_assert);
    assert_eq!(show_json["reminders"][0]["relatedTo"], "due");
    assert_eq!(show_json["reminders"][0]["offset"], "-PT15M");
}

#[test]
fn tasks_add_json_output_respects_configured_nlp_language() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("tasknotes", &vault_root);
    fs::create_dir_all(vault_root.join(".vulcan")).expect("config dir should exist");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        "[tasknotes]\nnlp_language = \"de\"\n",
    )
    .expect("tasknotes config should be written");

    let add_assert = cargo_vulcan_fixed_now()
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "add",
            "Bericht morgen @arbeit dringend",
            "--no-commit",
        ])
        .assert()
        .success();
    let add_json = parse_stdout_json(&add_assert);

    assert_eq!(add_json["title"], "Bericht");
    assert_eq!(add_json["due"], "2026-04-05");
    assert_eq!(add_json["priority"], "high");
    assert_eq!(add_json["contexts"], serde_json::json!(["@arbeit"]));
}

#[test]
fn kanban_list_json_output_lists_indexed_boards() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault root should exist");
    write_kanban_cli_fixture(&vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "kanban",
            "list",
        ])
        .assert()
        .success();
    let json_lines = parse_stdout_json_lines(&assert);

    assert_eq!(json_lines.len(), 1);
    assert_eq!(json_lines[0]["path"], Value::String("Board.md".to_string()));
    assert_eq!(json_lines[0]["title"], Value::String("Board".to_string()));
    assert_eq!(json_lines[0]["format"], Value::String("board".to_string()));
    assert_eq!(json_lines[0]["column_count"], Value::Number(2.into()));
    assert_eq!(json_lines[0]["card_count"], Value::Number(3.into()));
}

#[test]
fn kanban_show_json_output_returns_columns_and_verbose_cards() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault root should exist");
    write_kanban_cli_fixture(&vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "kanban",
            "show",
            "Board",
            "--verbose",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["path"], Value::String("Board.md".to_string()));
    assert_eq!(json["title"], Value::String("Board".to_string()));
    assert_eq!(json["date_trigger"], Value::String("DUE".to_string()));
    assert_eq!(json["time_trigger"], Value::String("AT".to_string()));
    assert_eq!(json["columns"].as_array().map(Vec::len), Some(2));
    assert_eq!(
        json["columns"][0]["name"],
        Value::String("Todo".to_string())
    );
    assert_eq!(
        json["columns"][0]["cards"].as_array().map(Vec::len),
        Some(2)
    );
    assert_eq!(
        json["columns"][0]["cards"][0]["date"],
        Value::String("2026-04-01".to_string())
    );
    assert_eq!(
        json["columns"][0]["cards"][0]["time"],
        Value::String("09:30".to_string())
    );
    assert_eq!(
        json["columns"][0]["cards"][1]["task"]["status_type"],
        Value::String("IN_PROGRESS".to_string())
    );
}

#[test]
fn kanban_show_json_output_inherits_linked_page_metadata_for_wikilink_cards() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join("Projects")).expect("projects dir should exist");
    fs::write(
        vault_root.join("Projects/Alpha.md"),
        concat!(
            "---\n",
            "status: active\n",
            "owner: Ops\n",
            "tags:\n",
            "  - client\n",
            "---\n\n",
            "# Alpha\n",
        ),
    )
    .expect("linked note should be written");
    fs::write(
        vault_root.join("Board.md"),
        concat!(
            "---\n",
            "kanban-plugin: board\n",
            "---\n\n",
            "## Todo\n\n",
            "- [[Projects/Alpha]]\n",
        ),
    )
    .expect("board should be written");
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "kanban",
            "show",
            "Board",
            "--verbose",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(
        json["columns"][0]["cards"][0]["metadata"]["status"],
        Value::String("active".to_string())
    );
    assert_eq!(
        json["columns"][0]["cards"][0]["metadata"]["owner"],
        Value::String("Ops".to_string())
    );
    assert_eq!(
        json["columns"][0]["cards"][0]["metadata"]["file"]["path"],
        Value::String("Projects/Alpha.md".to_string())
    );
    assert!(json["columns"][0]["cards"][0]["metadata"]["file"]["tags"]
        .as_array()
        .is_some_and(|tags| tags.contains(&Value::String("client".to_string()))));
}

#[test]
fn kanban_show_json_output_includes_archive_when_requested() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault root should exist");
    write_kanban_archive_fixture(&vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "kanban",
            "show",
            "Board",
            "--include-archive",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["columns"].as_array().map(Vec::len), Some(3));
    assert_eq!(
        json["columns"][2]["name"],
        Value::String("Archive".to_string())
    );
    assert_eq!(
        json["columns"][2]["cards"][0]["text"],
        Value::String("Old card".to_string())
    );
}

#[test]
fn kanban_cards_json_output_filters_by_column_and_status() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault root should exist");
    write_kanban_cli_fixture(&vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "kanban",
            "cards",
            "Board",
            "--column",
            "Todo",
            "--status",
            "IN_PROGRESS",
        ])
        .assert()
        .success();
    let json_lines = parse_stdout_json_lines(&assert);

    assert_eq!(json_lines.len(), 1);
    assert_eq!(
        json_lines[0]["board_path"],
        Value::String("Board.md".to_string())
    );
    assert_eq!(
        json_lines[0]["column_filter"],
        Value::String("Todo".to_string())
    );
    assert_eq!(
        json_lines[0]["status_filter"],
        Value::String("IN_PROGRESS".to_string())
    );
    assert_eq!(json_lines[0]["column"], Value::String("Todo".to_string()));
    assert_eq!(
        json_lines[0]["text"],
        Value::String("Waiting on review [owner:: Ops]".to_string())
    );
    assert_eq!(
        json_lines[0]["task_status_type"],
        Value::String("IN_PROGRESS".to_string())
    );
    assert_eq!(
        json_lines[0]["inline_fields"]["owner"],
        Value::String("Ops".to_string())
    );
}

#[test]
fn kanban_archive_json_output_moves_cards_into_archive() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault root should exist");
    write_kanban_archive_fixture(&vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "kanban",
            "archive",
            "Board",
            "Build release",
            "--no-commit",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["path"], Value::String("Board.md".to_string()));
    assert_eq!(json["source_column"], Value::String("Todo".to_string()));
    assert_eq!(json["archive_column"], Value::String("Archive".to_string()));
    assert_eq!(
        json["card_text"],
        Value::String("Build release".to_string())
    );
    assert_eq!(json["created_archive_column"], Value::Bool(false));
    assert_eq!(json["dry_run"], Value::Bool(false));
    assert_eq!(json["rescanned"], Value::Bool(true));

    let source = fs::read_to_string(vault_root.join("Board.md")).expect("board should be readable");
    assert!(!source.contains("## Todo\n\n- Build release\n"));
    assert!(source.contains("## Archive\n\n- Old card\n- Build release\n"));

    let show_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "kanban",
            "show",
            "Board",
            "--include-archive",
        ])
        .assert()
        .success();
    let board = parse_stdout_json(&show_assert);

    assert_eq!(board["columns"].as_array().map(Vec::len), Some(3));
    assert_eq!(
        board["columns"][0]["cards"].as_array().map(Vec::len),
        Some(0)
    );
    assert_eq!(
        board["columns"][2]["cards"].as_array().map(Vec::len),
        Some(2)
    );
}

#[test]
fn kanban_archive_dry_run_json_output_leaves_board_unchanged() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault root should exist");
    write_kanban_archive_fixture(&vault_root);
    run_scan(&vault_root);

    let original =
        fs::read_to_string(vault_root.join("Board.md")).expect("board should be readable");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "kanban",
            "archive",
            "Board",
            "Build release",
            "--dry-run",
            "--no-commit",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["dry_run"], Value::Bool(true));
    assert_eq!(json["rescanned"], Value::Bool(false));

    let after = fs::read_to_string(vault_root.join("Board.md")).expect("board should be readable");
    assert_eq!(after, original);
}

#[test]
fn kanban_move_json_output_moves_cards_between_columns() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault root should exist");
    fs::write(
        vault_root.join("Board.md"),
        concat!(
            "---\n",
            "kanban-plugin: board\n",
            "---\n\n",
            "## Todo\n\n",
            "- Build release\n\n",
            "## Doing\n\n",
            "- Review QA\n\n",
            "## Done\n\n",
            "- Shipped\n",
        ),
    )
    .expect("board should be written");
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "kanban",
            "move",
            "Board",
            "Build release",
            "Done",
            "--no-commit",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["path"], Value::String("Board.md".to_string()));
    assert_eq!(json["source_column"], Value::String("Todo".to_string()));
    assert_eq!(json["target_column"], Value::String("Done".to_string()));
    assert_eq!(
        json["card_text"],
        Value::String("Build release".to_string())
    );
    assert_eq!(json["dry_run"], Value::Bool(false));
    assert_eq!(json["rescanned"], Value::Bool(true));

    let show_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "kanban",
            "show",
            "Board",
            "--verbose",
        ])
        .assert()
        .success();
    let board = parse_stdout_json(&show_assert);

    assert_eq!(
        board["columns"][0]["cards"].as_array().map(Vec::len),
        Some(0)
    );
    assert_eq!(
        board["columns"][2]["cards"].as_array().map(Vec::len),
        Some(2)
    );
    assert_eq!(
        board["columns"][2]["cards"][1]["text"],
        Value::String("Build release".to_string())
    );
}

#[test]
fn kanban_add_json_output_inserts_cards_using_column_ordering() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault root should exist");
    fs::write(
        vault_root.join("Board.md"),
        concat!(
            "---\n",
            "kanban-plugin: board\n",
            "---\n\n",
            "## Todo\n\n",
            "- Existing card\n\n",
            "## Done\n\n",
            "- Shipped\n",
        ),
    )
    .expect("board should be written");
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "kanban",
            "add",
            "Board",
            "Todo",
            "Build release",
            "--no-commit",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["path"], Value::String("Board.md".to_string()));
    assert_eq!(json["column"], Value::String("Todo".to_string()));
    assert_eq!(
        json["card_text"],
        Value::String("Build release".to_string())
    );
    assert_eq!(json["dry_run"], Value::Bool(false));
    assert_eq!(json["rescanned"], Value::Bool(true));

    let show_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "kanban",
            "show",
            "Board",
            "--verbose",
        ])
        .assert()
        .success();
    let board = parse_stdout_json(&show_assert);

    assert_eq!(
        board["columns"][0]["cards"].as_array().map(Vec::len),
        Some(2)
    );
    assert_eq!(
        board["columns"][0]["cards"][1]["text"],
        Value::String("Build release".to_string())
    );
}

#[test]
fn dataview_query_human_output_respects_display_result_count_setting() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("dataview", &vault_root);
    fs::create_dir_all(vault_root.join(".vulcan")).expect("config dir should exist");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        "[dataview]\ndisplay_result_count = false\n",
    )
    .expect("config should be written");
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "dataview",
            "query",
            r#"TABLE status FROM "Projects" SORT file.name ASC"#,
        ])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone())
        .expect("stdout should be valid utf-8");

    assert!(stdout.contains("| File | status |"));
    assert!(stdout.contains("| --- | --- |"));
    assert!(stdout.contains("| [[Projects/Alpha]] | active |"));
    assert!(stdout.contains("| [[Projects/Beta]] | backlog |"));
    assert!(!stdout.contains("result(s)"));
}

#[test]
fn dataview_query_human_output_omits_empty_list_and_task_messages() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("dataview", &vault_root);
    run_scan(&vault_root);

    let empty_list = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "dataview",
            "query",
            r#"LIST FROM "Projects" WHERE priority > 99"#,
        ])
        .assert()
        .success();
    let empty_list_stdout = String::from_utf8(empty_list.get_output().stdout.clone())
        .expect("stdout should be valid utf-8");
    assert_eq!(empty_list_stdout, "");

    let empty_task = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "dataview",
            "query",
            r#"TASK FROM "Projects" WHERE file.name = "Alpha" AND completed"#,
        ])
        .assert()
        .success();
    let empty_task_stdout = String::from_utf8(empty_task.get_output().stdout.clone())
        .expect("stdout should be valid utf-8");
    assert_eq!(empty_task_stdout, "");
}

#[test]
fn dataview_plugin_display_settings_affect_human_output() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("dataview", &vault_root);
    fs::create_dir_all(vault_root.join(".obsidian/plugins/dataview"))
        .expect("plugin dir should exist");
    fs::write(
        vault_root.join(".obsidian/plugins/dataview/data.json"),
        r#"{
          "displayResultCount": false,
          "primaryColumnName": "Document",
          "groupColumnName": "Bucket"
        }"#,
    )
    .expect("plugin settings should be written");
    run_scan(&vault_root);

    let table_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "dataview",
            "query",
            r#"TABLE status FROM "Projects" SORT file.name ASC"#,
        ])
        .assert()
        .success();
    let table_stdout = String::from_utf8(table_assert.get_output().stdout.clone())
        .expect("stdout should be valid utf-8");
    assert!(table_stdout.contains("| Document | status |"));
    assert!(table_stdout.contains("| --- | --- |"));
    assert!(!table_stdout.contains("result(s)"));

    let grouped_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "dataview",
            "query",
            r#"TABLE length(rows) AS count FROM "Projects" GROUP BY status SORT key ASC"#,
        ])
        .assert()
        .success();
    let grouped_stdout = String::from_utf8(grouped_assert.get_output().stdout.clone())
        .expect("stdout should be valid utf-8");
    assert!(grouped_stdout.contains("| Bucket | count |"));
    assert!(grouped_stdout.contains("| --- | --- |"));
    assert!(!grouped_stdout.contains("result(s)"));
}

#[test]
fn notes_json_output_includes_evaluated_inline_expressions() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("dataview", &vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "--fields",
            "document_path,inline_expressions",
            "notes",
            "--where",
            "status = draft",
        ])
        .assert()
        .success();
    let json_lines = parse_stdout_json_lines(&assert);

    assert_eq!(json_lines.len(), 1);
    assert_eq!(json_lines[0]["document_path"], "Dashboard.md");
    assert_eq!(
        json_lines[0]["inline_expressions"].as_array().map(Vec::len),
        Some(1)
    );
    assert_eq!(
        json_lines[0]["inline_expressions"][0]["expression"],
        Value::String("this.status".to_string())
    );
    assert_eq!(
        json_lines[0]["inline_expressions"][0]["value"],
        Value::String("draft".to_string())
    );
    assert_eq!(json_lines[0]["inline_expressions"][0]["error"], Value::Null);
}

#[test]
fn query_help_documents_filter_shortcuts() {
    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["query", "--help"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("Query DSL syntax:")
                .and(predicate::str::contains(
                    "Repeat --where to combine filters with AND.",
                ))
                .and(predicate::str::contains(
                    "file.path | file.name | file.ext | file.mtime",
                ))
                .and(predicate::str::contains("Shortcut operators:"))
                .and(predicate::str::contains(
                    "vulcan query --where 'status = done'",
                ))
                .and(predicate::str::contains(
                    "Bare `query` defaults to `from notes`.",
                )),
        );
}

#[test]
fn legacy_notes_help_routes_to_query_help() {
    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["notes", "--help"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains(
                "Run a Vulcan query, Dataview DQL query, or --where shortcut query",
            )
            .and(predicate::str::contains(
                "vulcan query --where 'status = done'",
            )),
        );
}

#[test]
fn search_help_documents_query_and_filter_syntax() {
    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["search", "--help"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("Search query syntax:")
                .and(predicate::str::contains(
                    "plain terms are ANDed: dashboard status",
                ))
                .and(predicate::str::contains("tag:index"))
                .and(predicate::str::contains("[status:done]"))
                .and(predicate::str::contains("/\\d{4}-\\d{2}-\\d{2}/"))
                .and(predicate::str::contains("section:(dog cat)"))
                .and(predicate::str::contains("task:docs"))
                .and(predicate::str::contains("task-todo:followup"))
                .and(predicate::str::contains("ignore-case:Bob"))
                .and(predicate::str::contains("--match-case"))
                .and(predicate::str::contains("--sort <SORT>"))
                .and(predicate::str::contains("vulcan search Bob --match-case"))
                .and(predicate::str::contains(
                    "vulcan search dashboard --sort path-desc",
                ))
                .and(predicate::str::contains(
                    "Use --raw-query to pass SQLite FTS5 syntax through unchanged.",
                ))
                .and(predicate::str::contains("Filter syntax:"))
                .and(predicate::str::contains(
                    "vulcan search dashboard --where 'reviewed = true'",
                )),
        );
}

#[test]
fn browse_help_documents_modes_and_actions() {
    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["browse", "--help"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("Browse modes:")
                .and(predicate::str::contains("Ctrl-F"))
                .and(predicate::str::contains("Ctrl-T"))
                .and(predicate::str::contains("background"))
                .and(predicate::str::contains(
                    "Printable characters always extend the active query or prompt",
                ))
                .and(predicate::str::contains("Ctrl-S"))
                .and(predicate::str::contains("Alt-C"))
                .and(predicate::str::contains(
                    "vulcan --refresh background browse",
                ))
                .and(predicate::str::contains("vulcan browse --no-commit")),
        );
}

#[test]
fn edit_help_documents_picker_and_rescan_behavior() {
    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["edit", "--help"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("Behavior:")
                .and(predicate::str::contains(
                    "If NOTE is omitted in an interactive terminal",
                ))
                .and(predicate::str::contains("After the editor exits"))
                .and(predicate::str::contains("vulcan edit --new Inbox/Idea")),
        );
}

#[test]
fn diff_help_documents_comparison_sources() {
    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["diff", "--help"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("Comparison source:")
                .and(predicate::str::contains(
                    "git-backed vaults compare the note against git HEAD",
                ))
                .and(predicate::str::contains(
                    "cache-level changes since the last scan",
                ))
                .and(predicate::str::contains(
                    "vulcan diff --since weekly Projects/Alpha",
                )),
        );
}

#[test]
fn inbox_and_template_help_document_config_and_variables() {
    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["inbox", "--help"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("Configuration:")
                .and(predicate::str::contains(
                    "Inbox settings live under [inbox]",
                ))
                .and(predicate::str::contains("vulcan inbox --file scratch.txt")),
        );

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["template", "--help"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("Template source:")
                .and(predicate::str::contains(
                    "If .obsidian/templates.json or the Templater plugin configures a template folder",
                ))
                .and(predicate::str::contains(
                    "{{title}} {{date}} {{time}} {{datetime}} {{uuid}}",
                ))
                .and(predicate::str::contains(
                    "{{date:YYYY-MM-DD}} {{time:HH:mm}}",
                ))
                .and(predicate::str::contains(
                    "Default template date/time formats live under [templates]",
                ))
                .and(predicate::str::contains("web_allowlist"))
                .and(predicate::str::contains("--engine auto"))
                .and(predicate::str::contains("--var key=value"))
                .and(predicate::str::contains("vulcan template --list"))
                .and(predicate::str::contains(
                    "vulcan template insert daily --prepend",
                ))
                .and(predicate::str::contains(
                    "vulcan template preview daily --path Journal/Today",
                ))
                .and(predicate::str::contains(
                    "Vulcan creates <date>-<template-name>.md",
                )),
        );
}

#[test]
fn template_preview_renders_templater_templates_with_var_bindings() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join(".vulcan/templates"))
        .expect("template dir should be created");
    fs::write(
        vault_root.join(".vulcan/templates/preview.md"),
        "<%* tR += tp.file.title.toUpperCase(); %>\nProject <% tp.system.prompt(\"Project\") %>\nPath <% tp.obsidian.normalizePath(\"Notes/Plan\") %>\n",
    )
    .expect("template should be written");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "template",
            "preview",
            "preview",
            "--path",
            "Notes/Plan",
            "--engine",
            "templater",
            "--var",
            "project=Vulcan",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["engine"], "templater");
    assert_eq!(json["path"], "Notes/Plan.md");
    let content = json["content"]
        .as_str()
        .expect("preview content should be a string");
    assert!(content.contains("PLAN"));
    assert!(content.contains("Project Vulcan"));
    assert!(content.contains("Path Notes/Plan.md"));
}

#[test]
fn render_human_output_formats_markdown_files() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let markdown_path = temp_dir.path().join("sample.md");
    fs::write(
        &markdown_path,
        concat!(
            "---\n",
            "title: Sample\n",
            "tags:\n",
            "  - demo\n",
            "---\n",
            "\n",
            "# Title\n",
            "\n",
            "| Name | Hours |\n",
            "| --- | ---: |\n",
            "| Alpha | 2 |\n",
        ),
    )
    .expect("markdown file should be written");

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "render",
            markdown_path
                .to_str()
                .expect("markdown path should be valid utf-8"),
        ])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("---\ntitle: Sample\ntags:\n  - demo\n---")
                .and(predicate::str::contains("Title"))
                .and(predicate::str::contains("| Name  | Hours |"))
                .and(predicate::str::contains("| ----- | ----: |"))
                .and(predicate::str::contains("# Title").not()),
        );
}

#[test]
fn render_markdown_output_echoes_raw_stdin() {
    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["--output", "markdown", "render"])
        .write_stdin("# Title\n\nBody\n")
        .assert()
        .success()
        .stdout(predicate::eq("# Title\n\nBody\n"));
}

#[test]
fn template_insert_renders_templater_syntax_against_target_note() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join(".vulcan/templates"))
        .expect("template dir should be created");
    fs::create_dir_all(vault_root.join("Projects")).expect("projects dir should be created");
    fs::write(
        vault_root.join(".vulcan/templates/status.md"),
        "Status <% tp.frontmatter.status %>\nTitle <% tp.file.title %>\nToday <% tp.date.now(\"YYYY-MM-DD\") %>\n",
    )
    .expect("template should be written");
    fs::write(
        vault_root.join("Projects/Alpha.md"),
        "---\nstatus: active\n---\n# Existing\n",
    )
    .expect("target note should be written");
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "template",
            "insert",
            "status",
            "Projects/Alpha",
            "--engine",
            "templater",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["engine"], "templater");
    assert_eq!(json["note"], "Projects/Alpha.md");
    let updated = fs::read_to_string(vault_root.join("Projects/Alpha.md"))
        .expect("updated note should exist");
    assert!(updated.contains("Status active"));
    assert!(updated.contains("Title Alpha"));
    assert!(updated
        .lines()
        .any(|line| line.starts_with("Today ") && line.len() == "Today ".len() + 10));
}

#[test]
fn template_preview_reports_diagnostics_for_mutating_helpers() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join(".vulcan/templates"))
        .expect("template dir should be created");
    fs::write(
        vault_root.join(".vulcan/templates/mutate.md"),
        "<%* await tp.file.create_new(\"Child body\", \"Child\") %>",
    )
    .expect("template should be written");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "template",
            "preview",
            "mutate",
            "--engine",
            "templater",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["engine"], "templater");
    let diagnostics = json["diagnostics"]
        .as_array()
        .expect("diagnostics should be an array");
    assert!(diagnostics.iter().any(|item| item
        .as_str()
        .is_some_and(|message| message.contains("disabled during template preview"))));
    assert!(!vault_root.join("Child.md").exists());
}

#[test]
fn bases_and_describe_help_document_runtime_surfaces() {
    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["bases", "--help"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("Evaluate and maintain Bases views")
                .and(predicate::str::contains("create"))
                .and(predicate::str::contains("view-add"))
                .and(predicate::str::contains(
                    "`create` derives folder and equality frontmatter from the first view; the TUI `n` hotkey uses the current view.",
                ))
                .and(predicate::str::contains(
                    "Mutating bases commands support --dry-run and --no-commit.",
                )),
        );

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["describe", "--help"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("Output:")
                .and(predicate::str::contains("runtime CLI schema"))
                .and(predicate::str::contains(
                    "vulcan --output json describe > vulcan-schema.json",
                )),
        );

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["help", "--help"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("Show integrated command and concept documentation")
                .and(predicate::str::contains("help query"))
                .and(predicate::str::contains("help note get --output json"))
                .and(predicate::str::contains("help --search graph")),
        );
}

#[test]
fn init_json_output_creates_default_config() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault dir should be created");
    let mut command = Command::cargo_bin("vulcan").expect("binary should build");

    let assert = command
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "init",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["created_config"], true);
    assert_eq!(json["created_cache"], true);
    assert!(vault_root.join(".vulcan/config.toml").exists());
    assert!(vault_root.join(".vulcan/cache.db").exists());
    assert!(vault_root.join(".vulcan/.gitignore").exists());
    assert_eq!(
        fs::read_to_string(vault_root.join(".vulcan/.gitignore"))
            .expect("gitignore should be readable"),
        "*\n!.gitignore\n!config.toml\nconfig.local.toml\n!reports/\nreports/*\n!reports/*.toml\n"
    );
    assert!(json.get("support_files").is_none());
}

#[test]
fn init_import_applies_detected_sources_and_reports_them() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join(".obsidian/plugins/dataview"))
        .expect("dataview plugin dir should be created");
    fs::write(
        vault_root.join(".obsidian/app.json"),
        r#"{
          "useMarkdownLinks": true,
          "newLinkFormat": "relative"
        }"#,
    )
    .expect("app config should be written");
    fs::write(
        vault_root.join(".obsidian/plugins/dataview/data.json"),
        r#"{"inlineQueryPrefix":"dv:"}"#,
    )
    .expect("dataview config should be written");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "init",
            "--import",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["created_config"], true);
    assert!(json["imported"].is_object());
    assert_eq!(json["imported"]["imported_count"], 2);
    assert!(json["importable_sources"]
        .as_array()
        .is_some_and(|sources| {
            sources.iter().any(|source| source["plugin"] == "core")
                && sources.iter().any(|source| source["plugin"] == "dataview")
        }));

    let rendered =
        fs::read_to_string(vault_root.join(".vulcan/config.toml")).expect("config should exist");
    assert!(rendered.contains("[links]"));
    assert!(rendered.contains("style = \"markdown\""));
    assert!(rendered.contains("[dataview]"));
    assert!(rendered.contains("inline_query_prefix = \"dv:\""));
}

#[test]
fn init_agent_files_writes_agents_template_and_default_skills() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault dir should be created");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "init",
            "--agent-files",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert!(vault_root.join("AGENTS.md").exists());
    assert!(vault_root
        .join(".agents/skills/note-operations/SKILL.md")
        .exists());
    assert!(vault_root
        .join(".agents/skills/js-api-guide/SKILL.md")
        .exists());
    assert!(vault_root.join("AI/Prompts/summarize-note.md").exists());
    assert!(vault_root.join("AI/Prompts/daily-review.md").exists());
    assert!(fs::read_to_string(vault_root.join("AGENTS.md"))
        .expect("agents template should be readable")
        .contains("Use Vulcan as the primary automation surface"));
    assert!(json["support_files"].as_array().is_some_and(|items| items
        .iter()
        .any(|item| item["path"] == "AGENTS.md")
        && items
            .iter()
            .any(|item| item["path"] == ".agents/skills/js-api-guide/SKILL.md")
        && items
            .iter()
            .any(|item| item["path"] == "AI/Prompts/summarize-note.md")));
}

#[test]
fn init_agent_files_optionally_scaffolds_example_tool() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault dir should be created");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "init",
            "--agent-files",
            "--example-tool",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    let manifest_path = vault_root.join(".agents/tools/summarize_note/TOOL.md");
    let entrypoint_path = vault_root.join(".agents/tools/summarize_note/main.js");
    assert!(manifest_path.exists());
    assert!(entrypoint_path.exists());
    assert!(fs::read_to_string(&manifest_path)
        .expect("example tool manifest should be readable")
        .contains("name: summarize_note"));
    assert!(fs::read_to_string(&entrypoint_path)
        .expect("example tool entrypoint should be readable")
        .contains("vault.note"));
    assert!(json["support_files"].as_array().is_some_and(|items| items
        .iter()
        .any(|item| item["path"] == ".agents/tools/summarize_note/TOOL.md")
        && items
            .iter()
            .any(|item| item["path"] == ".agents/tools/summarize_note/main.js")));
}

#[test]
fn skill_list_and_get_surface_bundled_skills() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault dir should be created");

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root.to_str().expect("utf-8"),
            "agent",
            "install",
        ])
        .assert()
        .success();

    let list_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root.to_str().expect("utf-8"),
            "--output",
            "json",
            "skill",
            "list",
        ])
        .assert()
        .success();
    let list_json = parse_stdout_json(&list_assert);
    assert!(list_json["skills"].as_array().is_some_and(|skills| skills
        .iter()
        .any(|skill| skill["name"] == "note-operations")));

    let get_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root.to_str().expect("utf-8"),
            "--output",
            "json",
            "skill",
            "get",
            "note-operations",
        ])
        .assert()
        .success();
    let get_json = parse_stdout_json(&get_assert);
    assert_eq!(get_json["name"].as_str(), Some("note-operations"));
    assert_eq!(get_json["path"].as_str(), Some("note-operations/SKILL.md"));
    assert!(get_json["body"]
        .as_str()
        .is_some_and(|body| body.contains("note outline")));
}

#[test]
fn skill_commands_respect_read_permissions() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault dir should be created");

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root.to_str().expect("utf-8"),
            "agent",
            "install",
        ])
        .assert()
        .success();

    fs::create_dir_all(vault_root.join(".vulcan")).expect("config dir should exist");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        r#"[permissions.profiles.blind]
read = "none"
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

    let list_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root.to_str().expect("utf-8"),
            "--permissions",
            "blind",
            "--output",
            "json",
            "skill",
            "list",
        ])
        .assert()
        .success();
    let list_json = parse_stdout_json(&list_assert);
    assert_eq!(list_json["skills"].as_array().map(Vec::len), Some(0));

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root.to_str().expect("utf-8"),
            "--permissions",
            "blind",
            "skill",
            "get",
            "note-operations",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "permission denied: profile `blind` does not allow read `.agents/skills/note-operations/SKILL.md`",
        ));
}

#[test]
fn agent_print_config_reports_runtime_contract() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault dir should be created");

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root.to_str().expect("utf-8"),
            "agent",
            "install",
        ])
        .assert()
        .success();

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root.to_str().expect("utf-8"),
            "--output",
            "json",
            "agent",
            "print-config",
            "--runtime",
            "pi",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["runtime"].as_str(), Some("pi"));
    assert_eq!(json["profiles"]["write"].as_str(), Some("agent"));
    assert_eq!(json["profiles"]["readonly"].as_str(), Some("readonly"));
    assert!(json["commands"]["describe_openai_tools"]
        .as_str()
        .is_some_and(|value| value.contains("--permissions agent")));
    assert!(json["commands"]["skill_list"]
        .as_str()
        .is_some_and(|value| value.ends_with("skill list")));
    assert!(json["commands"]["help_json"]
        .as_str()
        .is_some_and(|value| value.ends_with("help assistant-integration")));
    assert!(json["snippets"]["write_enabled"]
        .as_str()
        .is_some_and(|value| value.contains("describe --format openai-tools")));
}

#[test]
fn agent_import_previews_and_copies_external_harness_assets() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join(".claude/commands")).expect("claude prompt dir");
    fs::create_dir_all(vault_root.join(".codex/skills/review")).expect("codex skill dir");
    fs::write(vault_root.join("CLAUDE.md"), "# Claude\n").expect("instructions");
    fs::write(vault_root.join(".claude/commands/triage.md"), "# Triage\n").expect("prompt");
    fs::write(
        vault_root.join(".codex/skills/review/SKILL.md"),
        "---\nname: review\n---\nReview the change.\n",
    )
    .expect("skill");

    let preview_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root.to_str().expect("utf-8"),
            "--output",
            "json",
            "agent",
            "import",
        ])
        .assert()
        .success();
    let preview_json = parse_stdout_json(&preview_assert);
    assert_eq!(preview_json["detected_count"].as_u64(), Some(3));
    assert!(preview_json["items"].as_array().is_some_and(|items| items
        .iter()
        .any(|item| item["destination_path"] == "AGENTS.md" && item["status"] == "would_create")
        && items
            .iter()
            .any(|item| item["destination_path"] == "AI/Prompts/triage.md"
                && item["status"] == "would_create")
        && items.iter().any(
            |item| item["destination_path"] == ".agents/skills/review/SKILL.md"
                && item["status"] == "would_create"
        )));

    let apply_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root.to_str().expect("utf-8"),
            "--output",
            "json",
            "agent",
            "import",
            "--apply",
        ])
        .assert()
        .success();
    let apply_json = parse_stdout_json(&apply_assert);
    assert_eq!(apply_json["imported_count"].as_u64(), Some(3));
    assert!(vault_root.join("AGENTS.md").exists());
    assert!(vault_root.join("AI/Prompts/triage.md").exists());
    assert!(vault_root.join(".agents/skills/review/SKILL.md").exists());
    assert_eq!(
        fs::read_to_string(vault_root.join("AGENTS.md")).expect("agents file"),
        "# Claude\n"
    );
}

#[test]
fn agent_import_can_symlink_external_assets() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join(".claude/commands")).expect("claude prompt dir");
    fs::write(vault_root.join(".claude/commands/triage.md"), "# Triage\n").expect("prompt");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root.to_str().expect("utf-8"),
            "--output",
            "json",
            "agent",
            "import",
            "--apply",
            "--symlink",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);
    assert_eq!(json["imported_count"].as_u64(), Some(1));
    let destination = vault_root.join("AI/Prompts/triage.md");
    let metadata = fs::symlink_metadata(&destination).expect("symlink metadata");
    assert!(metadata.file_type().is_symlink());
}

#[test]
fn describe_openai_tools_includes_skill_helpers() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault dir should be created");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root.to_str().expect("utf-8"),
            "--output",
            "json",
            "describe",
            "--format",
            "openai-tools",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);
    let tools = json["tools"].as_array().expect("tool list");
    assert!(tools
        .iter()
        .any(|tool| tool["function"]["name"] == "skill_list"));
    assert!(tools
        .iter()
        .any(|tool| tool["function"]["name"] == "skill_get"));
    assert!(tools
        .iter()
        .any(|tool| tool["function"]["name"] == "agent_print_config"));
}

#[test]
fn help_assistant_integration_topic_is_available() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault dir should be created");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root.to_str().expect("utf-8"),
            "--output",
            "json",
            "help",
            "assistant-integration",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);
    assert_eq!(json["name"].as_str(), Some("assistant-integration"));
    assert!(json["body"]
        .as_str()
        .is_some_and(|body| body.contains("--permissions agent")));
}

#[test]
fn agent_install_overwrite_refreshes_bundled_skill_contents() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault dir should be created");

    let initial_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "agent",
            "install",
        ])
        .assert()
        .success();
    let initial_json = parse_stdout_json(&initial_assert);
    assert!(initial_json["support_files"]
        .as_array()
        .is_some_and(|items| items.iter().any(|item| item["path"]
            == ".agents/skills/note-operations/SKILL.md"
            && item["status"] == "created")));
    assert!(initial_json["support_files"]
        .as_array()
        .is_some_and(|items| items
            .iter()
            .any(|item| item["path"] == "AI/Prompts/summarize-note.md"
                && item["status"] == "created")));

    let skill_path = vault_root.join(".agents/skills/note-operations/SKILL.md");
    fs::write(&skill_path, "customized\n").expect("skill should be editable");

    let kept_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "agent",
            "install",
        ])
        .assert()
        .success();
    let kept_json = parse_stdout_json(&kept_assert);
    assert_eq!(
        fs::read_to_string(&skill_path).expect("skill should be readable"),
        "customized\n"
    );
    assert!(kept_json["support_files"]
        .as_array()
        .is_some_and(|items| items.iter().any(|item| item["path"]
            == ".agents/skills/note-operations/SKILL.md"
            && item["status"] == "kept")));

    let overwrite_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "agent",
            "install",
            "--overwrite",
        ])
        .assert()
        .success();
    let overwrite_json = parse_stdout_json(&overwrite_assert);

    let refreshed = fs::read_to_string(&skill_path).expect("skill should be readable");
    assert!(refreshed.contains("# Note Operations"));
    assert!(overwrite_json["support_files"]
        .as_array()
        .is_some_and(|items| items.iter().any(|item| item["path"]
            == ".agents/skills/note-operations/SKILL.md"
            && item["status"] == "updated")));
}

#[test]
fn agent_install_uses_configured_assistant_folders() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join(".vulcan")).expect("vault dir should be created");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        "[assistant]\nprompts_folder = \"Support/Prompts\"\nskills_folder = \"Support/Skills\"\n",
    )
    .expect("config should be written");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "agent",
            "install",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert!(vault_root
        .join("Support/Skills/note-operations/SKILL.md")
        .exists());
    assert!(vault_root
        .join("Support/Prompts/summarize-note.md")
        .exists());
    assert!(json["support_files"].as_array().is_some_and(|items| items
        .iter()
        .any(|item| item["path"] == "Support/Skills/note-operations/SKILL.md")
        && items
            .iter()
            .any(|item| item["path"] == "Support/Prompts/summarize-note.md")));
}

#[test]
fn agent_install_example_tool_uses_configured_tools_folder() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join(".vulcan")).expect("vault dir should be created");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        "[assistant]\nprompts_folder = \"Support/Prompts\"\nskills_folder = \"Support/Skills\"\ntools_folder = \"Support/Tools\"\n",
    )
    .expect("config should be written");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "agent",
            "install",
            "--example-tool",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert!(vault_root
        .join("Support/Tools/summarize_note/TOOL.md")
        .exists());
    assert!(vault_root
        .join("Support/Tools/summarize_note/main.js")
        .exists());
    assert!(json["support_files"].as_array().is_some_and(|items| items
        .iter()
        .any(|item| item["path"] == "Support/Tools/summarize_note/TOOL.md")
        && items
            .iter()
            .any(|item| item["path"] == "Support/Tools/summarize_note/main.js")));
}

#[test]
fn scan_json_output_indexes_fixture_vault() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    let mut command = Command::cargo_bin("vulcan").expect("binary should build");

    let assert = command
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "scan",
            "--full",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);
    let database = CacheDatabase::open(&VaultPaths::new(&vault_root)).expect("db should open");

    assert_eq!(json["mode"], "full");
    assert_eq!(json["discovered"], 3);
    assert_eq!(json["added"], 3);
    assert_eq!(
        document_paths(&database),
        vec![
            "Home.md".to_string(),
            "People/Bob.md".to_string(),
            "Projects/Alpha.md".to_string(),
        ]
    );
}

#[test]
fn cache_backed_commands_refresh_before_running_by_default() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault dir should be created");
    fs::write(vault_root.join("Home.md"), "# Home\nNo links yet.\n")
        .expect("home note should be written");
    fs::write(vault_root.join("Projects.md"), "# Alpha\n").expect("alpha note should be written");
    run_scan(&vault_root);
    fs::write(
        vault_root.join("Home.md"),
        "# Home\nNow links to [[Projects]].\n",
    )
    .expect("updated home note should be written");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "backlinks",
            "Projects",
        ])
        .assert()
        .success();
    let rows = parse_stdout_json_lines(&assert);

    assert!(rows
        .iter()
        .any(|row| row["source_path"] == Value::String("Home.md".to_string())));
}

#[test]
fn refresh_off_keeps_stale_cache_for_one_shot_commands() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault dir should be created");
    fs::write(vault_root.join("Home.md"), "# Home\nNo links yet.\n")
        .expect("home note should be written");
    fs::write(vault_root.join("Projects.md"), "# Alpha\n").expect("alpha note should be written");
    run_scan(&vault_root);
    fs::write(
        vault_root.join("Home.md"),
        "# Home\nNow links to [[Projects]].\n",
    )
    .expect("updated home note should be written");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "--refresh",
            "off",
            "backlinks",
            "Projects",
        ])
        .assert()
        .success();
    let rows = parse_stdout_json_lines(&assert);

    assert!(
        rows.is_empty(),
        "stale cache should not include new backlink"
    );
}

#[test]
fn doctor_json_output_reports_clean_basic_vault() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);
    let mut command = Command::cargo_bin("vulcan").expect("binary should build");

    let assert = command
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "doctor",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["summary"]["unresolved_links"], 0);
    assert_eq!(json["summary"]["ambiguous_links"], 0);
    assert_eq!(json["summary"]["parse_failures"], 0);
    assert_eq!(json["summary"]["missing_index_rows"], 0);
    assert_eq!(json["summary"]["orphan_notes"], 0);
}

#[test]
fn doctor_json_output_reports_broken_frontmatter_vault() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("broken-frontmatter", &vault_root);
    run_scan(&vault_root);
    let mut command = Command::cargo_bin("vulcan").expect("binary should build");

    let assert = command
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "doctor",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["summary"]["parse_failures"], 1);
    assert_eq!(json["parse_failures"][0]["document_path"], "Broken.md");
}

#[test]
fn doctor_json_output_reports_dataview_specific_issues() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join(".obsidian")).expect("obsidian dir should be created");
    fs::write(
        vault_root.join(".obsidian/types.json"),
        "{\n  \"priority\": \"number\"\n}\n",
    )
    .expect("types config should be written");
    fs::write(
        vault_root.join("Dashboard.md"),
        concat!(
            "priority:: high\n\n",
            "```dataview\n",
            "TABLE FROM\n",
            "```\n\n",
            "```dataviewjs\n",
            "dv.current()\n",
            "```\n",
        ),
    )
    .expect("note should be written");
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "doctor",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["summary"]["parse_failures"], 1);
    assert_eq!(json["summary"]["type_mismatches"], 1);
    assert_eq!(
        json["summary"]["unsupported_syntax"],
        Value::Number(usize::from(!cfg!(feature = "js_runtime")).into())
    );
    assert!(json["parse_failures"][0]["message"]
        .as_str()
        .is_some_and(|message| message.contains("Dataview block 0")));
    assert_eq!(json["type_mismatches"][0]["document_path"], "Dashboard.md");
    if cfg!(feature = "js_runtime") {
        assert_eq!(json["unsupported_syntax"], serde_json::json!([]));
    } else {
        assert_eq!(
            json["unsupported_syntax"][0]["document_path"],
            "Dashboard.md"
        );
    }
}

#[test]
fn doctor_fix_json_output_plans_repairs_for_uninitialized_vault() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    let mut command = Command::cargo_bin("vulcan").expect("binary should build");

    let assert = command
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "doctor",
            "--fix",
            "--dry-run",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);
    let fixes = json["fixes"].as_array().expect("fixes should be an array");

    assert_eq!(json["dry_run"], true);
    assert!(fixes.iter().any(|fix| fix["kind"] == "initialize"));
    assert!(fixes.iter().any(|fix| fix["kind"] == "scan"));
}

#[test]
fn scan_json_output_requires_initialized_vulcan_dir() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault root should be created");
    fs::write(vault_root.join("Home.md"), "# Home\n").expect("note should be written");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "scan",
            "--full",
        ])
        .assert()
        .failure();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["code"], "operation_failed");
    assert!(json["error"]
        .as_str()
        .is_some_and(|error| error.contains("Run `vulcan init`")));
}

#[test]
fn rename_property_json_output_reports_planned_file_changes() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("refactors", &vault_root);
    let mut command = Command::cargo_bin("vulcan").expect("binary should build");

    let assert = command
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "rename-property",
            "status",
            "phase",
            "--dry-run",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["action"], "rename_property");
    assert_eq!(json["dry_run"], true);
    assert_eq!(json["files"][0]["path"], "Home.md");
    assert_eq!(json["files"][0]["changes"][0]["before"], "status");
    assert_eq!(json["files"][0]["changes"][0]["after"], "phase");
}

#[test]
fn graph_path_json_output_returns_note_path_chain() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);
    let mut command = Command::cargo_bin("vulcan").expect("binary should build");

    let assert = command
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "graph",
            "path",
            "Bob",
            "Home",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(
        json["path"],
        serde_json::json!(["People/Bob.md", "Projects/Alpha.md", "Home.md"])
    );
}

#[test]
fn graph_moc_and_trends_json_output_report_candidates_and_history() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);
    fs::write(vault_root.join("Extra.md"), "# Extra\n\n[[Home]]\n")
        .expect("extra note should write");
    run_incremental_scan(&vault_root);

    let moc_rows = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args([
                "--vault",
                vault_root
                    .to_str()
                    .expect("vault path should be valid utf-8"),
                "--output",
                "json",
                "graph",
                "moc",
            ])
            .assert()
            .success();
        parse_stdout_json_lines(&assert)
    };
    assert_eq!(moc_rows[0]["document_path"], "Home.md");
    assert!(moc_rows[0]["reasons"]
        .as_array()
        .expect("reasons should be an array")
        .iter()
        .any(|reason| reason.as_str().unwrap_or_default().contains("index")));

    let trends = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args([
                "--vault",
                vault_root
                    .to_str()
                    .expect("vault path should be valid utf-8"),
                "--output",
                "json",
                "graph",
                "trends",
                "--limit",
                "2",
            ])
            .assert()
            .success();
        parse_stdout_json(&assert)
    };
    let points = trends["points"]
        .as_array()
        .expect("points should be an array");
    assert_eq!(points.len(), 2);
    assert_eq!(points[0]["note_count"], 3);
    assert_eq!(points[1]["note_count"], 4);
}

#[test]
fn checkpoint_and_changes_json_output_track_named_baselines() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);

    let checkpoint = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args([
                "--vault",
                vault_root
                    .to_str()
                    .expect("vault path should be valid utf-8"),
                "--output",
                "json",
                "checkpoint",
                "create",
                "baseline",
            ])
            .assert()
            .success();
        parse_stdout_json(&assert)
    };
    assert_eq!(checkpoint["name"], "baseline");
    assert_eq!(checkpoint["source"], "manual");

    fs::write(
        vault_root.join("Home.md"),
        "# Home\n\nUpdated dashboard links.\n",
    )
    .expect("updated note should write");
    run_incremental_scan(&vault_root);

    let checkpoint_rows = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args([
                "--vault",
                vault_root
                    .to_str()
                    .expect("vault path should be valid utf-8"),
                "--output",
                "json",
                "checkpoint",
                "list",
            ])
            .assert()
            .success();
        parse_stdout_json_lines(&assert)
    };
    assert!(checkpoint_rows
        .iter()
        .any(|row| row["name"] == "baseline" && row["source"] == "manual"));

    let changes = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args([
                "--vault",
                vault_root
                    .to_str()
                    .expect("vault path should be valid utf-8"),
                "--output",
                "json",
                "changes",
                "--checkpoint",
                "baseline",
            ])
            .assert()
            .success();
        parse_stdout_json_lines(&assert)
    };
    assert!(changes.iter().any(|row| {
        row["anchor"] == "baseline" && row["kind"] == "note" && row["path"] == "Home.md"
    }));
}

#[test]
fn cache_verify_json_output_reports_healthy_fixture_cache() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);
    let mut command = Command::cargo_bin("vulcan").expect("binary should build");

    let assert = command
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "cache",
            "verify",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["healthy"], true);
    assert!(json["checks"]
        .as_array()
        .expect("checks should be an array")
        .iter()
        .all(|check| check["ok"] == true));
}

#[test]
fn links_json_output_supports_alias_lookup() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);
    let mut command = Command::cargo_bin("vulcan").expect("binary should build");

    let assert = command
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "links",
            "Start",
        ])
        .assert()
        .success();
    let json_lines = parse_stdout_json_lines(&assert);

    assert_eq!(json_lines.len(), 2);
    assert_eq!(json_lines[0]["note_path"], "Home.md");
    assert_eq!(json_lines[0]["matched_by"], "alias");
}

#[test]
fn backlinks_json_output_lists_sources() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);
    let mut command = Command::cargo_bin("vulcan").expect("binary should build");

    let assert = command
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "backlinks",
            "Projects/Alpha",
        ])
        .assert()
        .success();
    let json_lines = parse_stdout_json_lines(&assert);

    assert_eq!(json_lines[0]["note_path"], "Projects/Alpha.md");
    assert_eq!(
        json_lines
            .iter()
            .map(|row| row["source_path"].as_str().unwrap_or_default().to_string())
            .collect::<Vec<_>>(),
        vec!["Home.md".to_string(), "People/Bob.md".to_string()]
    );
}

#[test]
fn note_commands_without_arguments_fail_cleanly_in_non_interactive_mode() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);
    let vault_root_str = vault_root
        .to_str()
        .expect("vault path should be valid utf-8")
        .to_string();

    let links_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["--vault", &vault_root_str, "--output", "json", "links"])
        .assert()
        .failure();
    let links_json = parse_stdout_json(&links_assert);
    assert_eq!(links_json["code"], "operation_failed");
    assert!(links_json["error"]
        .as_str()
        .is_some_and(|message| message
            .contains("missing note; provide a note identifier or run interactively")));

    let related_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["--vault", &vault_root_str, "--output", "json", "related"])
        .assert()
        .failure();
    let related_json = parse_stdout_json(&related_assert);
    assert_eq!(related_json["code"], "operation_failed");
    assert!(related_json["error"]
        .as_str()
        .is_some_and(|message| message
            .contains("missing note; provide a note identifier or run interactively")));

    let suggest_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "--output",
            "json",
            "suggest",
            "mentions",
            "Missing",
        ])
        .assert()
        .failure();
    let suggest_json = parse_stdout_json(&suggest_assert);
    assert_eq!(suggest_json["code"], "operation_failed");
    assert!(suggest_json["error"]
        .as_str()
        .is_some_and(|message| message.contains("note not found")));
}

#[test]
fn json_error_output_is_structured_for_invalid_arguments() {
    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["--output", "json", "init", "--import", "--no-import"])
        .assert()
        .failure();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["code"], "invalid_arguments");
    assert!(json["error"]
        .as_str()
        .is_some_and(|message| message.contains("cannot be used with")));
}

#[test]
fn links_json_output_supports_fields_limit_and_offset() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);
    let mut command = Command::cargo_bin("vulcan").expect("binary should build");

    let assert = command
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "--fields",
            "resolved_target_path,resolution_status",
            "--limit",
            "1",
            "--offset",
            "1",
            "links",
            "Start",
        ])
        .assert()
        .success();
    let json_lines = parse_stdout_json_lines(&assert);

    assert_eq!(json_lines.len(), 1);
    assert_eq!(
        json_lines[0],
        serde_json::json!({
            "resolved_target_path": "People/Bob.md",
            "resolution_status": "resolved"
        })
    );
}

#[test]
fn search_json_output_returns_ranked_hits_and_supports_filters() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);
    let mut command = Command::cargo_bin("vulcan").expect("binary should build");

    let assert = command
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "--fields",
            "document_path,snippet",
            "--limit",
            "1",
            "search",
            "Robert",
            "--path-prefix",
            "People/",
        ])
        .assert()
        .success();
    let json_lines = parse_stdout_json_lines(&assert);

    assert_eq!(json_lines.len(), 1);
    assert_eq!(
        json_lines[0]["document_path"],
        serde_json::Value::String("People/Bob.md".to_string())
    );
    assert!(json_lines[0]["snippet"]
        .as_str()
        .expect("snippet should be a string")
        .contains("Bob"));
}

#[test]
fn search_json_output_includes_section_metadata_for_follow_up_reads() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "--fields",
            "document_path,section_id,line_spans",
            "search",
            "references",
        ])
        .assert()
        .success();
    let json_lines = parse_stdout_json_lines(&assert);

    assert_eq!(json_lines.len(), 1);
    assert_eq!(json_lines[0]["document_path"], "Projects/Alpha.md");
    assert_eq!(json_lines[0]["section_id"], "alpha/status@12");
    let start_line = json_lines[0]["line_spans"][0]["start_line"]
        .as_u64()
        .expect("start line should be numeric");
    let end_line = json_lines[0]["line_spans"][0]["end_line"]
        .as_u64()
        .expect("end line should be numeric");
    assert!(start_line <= 14);
    assert!(end_line >= 14);
}

#[test]
fn search_json_output_supports_explain_fuzzy_and_where_filters() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("mixed-properties", &vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "--fields",
            "document_path,effective_query,parsed_query_explanation,explain",
            "search",
            "releese",
            "--where",
            "reviewed = true",
            "--fuzzy",
            "--explain",
        ])
        .assert()
        .success();
    let rows = parse_stdout_json_lines(&assert);

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["document_path"], "Done.md");
    assert!(rows[0]["effective_query"]
        .as_str()
        .expect("effective query should be a string")
        .contains("release"));
    assert!(rows[0]["parsed_query_explanation"]
        .as_array()
        .expect("parsed query explanation should be an array")
        .iter()
        .any(|line| line == "TERM releese"));
    assert_eq!(rows[0]["explain"]["strategy"], "keyword");
}

#[test]
fn search_explain_human_output_includes_grouped_query_plan() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "search",
            "(dashboard or bob) -(\"owned by\" draft)",
            "--explain",
        ])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("Query plan:")
                .and(predicate::str::contains("AND"))
                .and(predicate::str::contains("OR"))
                .and(predicate::str::contains("NOT")),
        );
}

#[test]
fn search_inline_file_content_and_match_case_operators_work() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault root should exist");
    fs::write(
        vault_root.join("Meeting.md"),
        "# Notes\nReleaseAlias checklist",
    )
    .expect("meeting note should write");
    fs::write(
        vault_root.join("Reference.md"),
        "---\naliases:\n  - ReleaseAlias\n---\n\n# Reference\nnothing else",
    )
    .expect("reference note should write");
    fs::write(vault_root.join("People.md"), "Bob\nbob").expect("people note should write");
    run_scan(&vault_root);

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "search",
            "content:ReleaseAlias",
        ])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("\"document_path\":\"Meeting.md\"")
                .and(predicate::str::contains("Reference.md").not()),
        );

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "search",
            "match-case:Bob",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"document_path\":\"People.md\""));
}

#[test]
fn search_line_and_block_operators_work() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault root should exist");
    fs::write(vault_root.join("SameLine.md"), "mix flour\noven ready").expect("note should write");
    fs::write(vault_root.join("SplitLine.md"), "mix\nflour").expect("note should write");
    fs::write(
        vault_root.join("SameBlock.md"),
        "mix flour\nstir well\n\nserve",
    )
    .expect("note should write");
    fs::write(vault_root.join("SplitBlock.md"), "mix\n\nflour").expect("note should write");
    run_scan(&vault_root);

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "search",
            "line:(mix flour)",
        ])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("\"document_path\":\"SameLine.md\"")
                .and(predicate::str::contains("SplitLine.md").not()),
        );

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "search",
            "block:(mix flour)",
        ])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("\"document_path\":\"SameBlock.md\"")
                .and(predicate::str::contains("SplitBlock.md").not()),
        );
}

#[test]
fn search_section_operator_works_across_heading_chunks() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault root should exist");
    fs::write(
        vault_root.join("SameSection.md"),
        "# Plan\n\ndog checklist\n\ncat summary",
    )
    .expect("note should write");
    fs::write(
        vault_root.join("SplitSection.md"),
        "# Dogs\n\ndog checklist\n\n# Cats\n\ncat summary",
    )
    .expect("note should write");
    run_scan(&vault_root);

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "search",
            "section:(dog cat)",
        ])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("\"document_path\":\"SameSection.md\"")
                .and(predicate::str::contains("SplitSection.md").not()),
        );
}

#[test]
fn search_inline_bracket_property_filters_work() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("mixed-properties", &vault_root);
    run_scan(&vault_root);

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "--fields",
            "document_path,parsed_query_explanation",
            "search",
            "release [status:done]",
            "--explain",
        ])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("\"document_path\":\"Done.md\"")
                .and(predicate::str::contains("Backlog.md").not())
                .and(predicate::str::contains("WHERE [status:done]")),
        );

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "search",
            "[status:done OR backlog]",
        ])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("\"document_path\":\"Done.md\"")
                .and(predicate::str::contains("\"document_path\":\"Backlog.md\"")),
        );
}

#[test]
fn search_inline_regex_filters_work() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join("Journal")).expect("journal dir should exist");
    fs::write(vault_root.join("Notes.md"), "Meeting on 2026-03-26.").expect("note should write");
    fs::write(
        vault_root.join("Journal/2026-03-26.md"),
        "Daily notes without a date in body.",
    )
    .expect("note should write");
    run_scan(&vault_root);

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "--fields",
            "document_path,parsed_query_explanation",
            "search",
            "/\\d{4}-\\d{2}-\\d{2}/",
            "--explain",
        ])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("\"document_path\":\"Notes.md\"")
                .and(predicate::str::contains("Journal/2026-03-26.md").not())
                .and(predicate::str::contains(
                    "REGEX /\\\\d{4}-\\\\d{2}-\\\\d{2}/",
                )),
        );

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "search",
            "path:/2026-03-\\d{2}/",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "\"document_path\":\"Journal/2026-03-26.md\"",
        ));
}

#[test]
fn search_task_operators_work() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault root should exist");
    fs::write(
        vault_root.join("Tasks.md"),
        "- [ ] write docs\n- [x] ship release\nplain write docs note",
    )
    .expect("note should write");
    fs::write(vault_root.join("Body.md"), "write docs outside of tasks")
        .expect("note should write");
    run_scan(&vault_root);

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "--fields",
            "document_path,parsed_query_explanation",
            "search",
            "task:write",
            "--explain",
        ])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("\"document_path\":\"Tasks.md\"")
                .and(predicate::str::contains("Body.md").not())
                .and(predicate::str::contains("FILTER task:write")),
        );

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "search",
            "task-todo:write",
        ])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("\"document_path\":\"Tasks.md\"")
                .and(predicate::str::contains("Body.md").not()),
        );

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "search",
            "task-done:ship",
        ])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("\"document_path\":\"Tasks.md\"")
                .and(predicate::str::contains("Body.md").not()),
        );
}

#[test]
fn search_sort_orders_results_and_reports_sort_plan() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault root should exist");
    fs::write(vault_root.join("Alpha.md"), "dashboard").expect("alpha note should write");
    fs::write(vault_root.join("Beta.md"), "dashboard").expect("beta note should write");
    fs::write(vault_root.join("Gamma.md"), "dashboard").expect("gamma note should write");
    run_scan(&vault_root);

    let paths = VaultPaths::new(&vault_root);
    let database = CacheDatabase::open(&paths).expect("db should open");
    let set_mtime = |path: &str, mtime: i64| {
        database
            .connection()
            .execute(
                "UPDATE documents SET file_mtime = ? WHERE path = ?",
                (mtime, path),
            )
            .expect("document mtime should update");
    };
    set_mtime("Alpha.md", 100);
    set_mtime("Beta.md", 300);
    set_mtime("Gamma.md", 200);

    let vault_root_str = vault_root
        .to_str()
        .expect("vault path should be valid utf-8")
        .to_string();
    let search_rows = |fields: &str, sort: &str, explain: bool| {
        let mut args = vec![
            "--vault",
            vault_root_str.as_str(),
            "--refresh",
            "off",
            "--output",
            "json",
            "--fields",
            fields,
            "search",
            "dashboard",
            "--sort",
            sort,
        ];
        if explain {
            args.push("--explain");
        }
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args(args)
            .assert()
            .success();
        parse_stdout_json_lines(&assert)
    };
    let document_paths = |rows: &[Value]| {
        rows.iter()
            .map(|row| {
                row["document_path"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            })
            .collect::<Vec<_>>()
    };

    let path_desc_rows = search_rows("document_path", "path-desc", false);
    assert_eq!(
        document_paths(&path_desc_rows),
        vec![
            "Gamma.md".to_string(),
            "Beta.md".to_string(),
            "Alpha.md".to_string(),
        ]
    );

    let modified_rows = search_rows(
        "document_path,parsed_query_explanation",
        "modified-newest",
        true,
    );
    assert_eq!(
        document_paths(&modified_rows),
        vec![
            "Beta.md".to_string(),
            "Gamma.md".to_string(),
            "Alpha.md".to_string(),
        ]
    );
    assert!(modified_rows[0]["parsed_query_explanation"]
        .as_array()
        .expect("parsed query explanation should be an array")
        .iter()
        .any(|line| line == "SORT modified-newest"));
}

#[test]
fn search_match_case_flag_reports_matched_line_and_no_result_suggestions() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault root should exist");
    fs::write(vault_root.join("Upper.md"), "Bob builds dashboards.")
        .expect("upper note should write");
    fs::write(vault_root.join("Lower.md"), "bob builds dashboards.")
        .expect("lower note should write");
    run_scan(&vault_root);

    let vault_root_str = vault_root
        .to_str()
        .expect("vault path should be valid utf-8")
        .to_string();

    let match_case_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "--output",
            "json",
            "--fields",
            "document_path,matched_line",
            "search",
            "Bob",
            "--match-case",
        ])
        .assert()
        .success();
    let match_case_rows = parse_stdout_json_lines(&match_case_assert);
    assert_eq!(match_case_rows.len(), 1);
    assert_eq!(match_case_rows[0]["document_path"], "Upper.md");
    assert_eq!(match_case_rows[0]["matched_line"], 1);

    let no_result_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "--output",
            "json",
            "--fields",
            "no_results,parsed_query_explanation",
            "search",
            "contents:Bob task-todo:ship",
            "--explain",
        ])
        .assert()
        .success();
    let no_result_rows = parse_stdout_json_lines(&no_result_assert);
    assert_eq!(no_result_rows.len(), 1);
    assert_eq!(no_result_rows[0]["no_results"], true);
    let explanation = no_result_rows[0]["parsed_query_explanation"]
        .as_array()
        .expect("parsed query explanation should be an array");
    assert!(explanation
        .iter()
        .any(|line| line == "SUGGESTION did you mean `content:` instead of `contents:`?"));
    assert!(explanation
        .iter()
        .any(|line| line == "SUGGESTION no tasks found in matched files for `task-todo:`"));
}

#[test]
fn notes_json_output_filters_and_sorts_property_queries() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("mixed-properties", &vault_root);
    run_scan(&vault_root);
    let mut command = Command::cargo_bin("vulcan").expect("binary should build");

    let assert = command
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "--fields",
            "document_path,properties",
            "notes",
            "--where",
            "estimate > 2",
            "--sort",
            "due",
        ])
        .assert()
        .success();
    let json_lines = parse_stdout_json_lines(&assert);

    assert_eq!(json_lines.len(), 2);
    assert_eq!(json_lines[0]["document_path"], "Done.md");
    assert_eq!(json_lines[1]["document_path"], "Backlog.md");
    assert_eq!(json_lines[0]["properties"]["status"], "done");
}

#[test]
fn notes_json_output_supports_inline_field_and_file_namespace_filters() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault root should exist");
    fs::write(
        vault_root.join("Large.md"),
        format!("due:: 2020-01-01\n\n{}\n", "x".repeat(12_000)),
    )
    .expect("large note should be written");
    fs::write(vault_root.join("Small.md"), "due:: 2099-01-01\n")
        .expect("small note should be written");
    run_scan(&vault_root);

    let overdue = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "--fields",
            "document_path",
            "notes",
            "--where",
            "due < date(today)",
        ])
        .assert()
        .success();
    let overdue_rows = parse_stdout_json_lines(&overdue);
    assert_eq!(overdue_rows.len(), 1);
    assert_eq!(overdue_rows[0]["document_path"], "Large.md");

    let large_files = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "--fields",
            "document_path",
            "notes",
            "--where",
            "file.size > 10000",
        ])
        .assert()
        .success();
    let large_file_rows = parse_stdout_json_lines(&large_files);
    assert_eq!(large_file_rows.len(), 1);
    assert_eq!(large_file_rows[0]["document_path"], "Large.md");
}

#[test]
fn search_json_output_supports_has_property_filter() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("mixed-properties", &vault_root);
    run_scan(&vault_root);
    let mut command = Command::cargo_bin("vulcan").expect("binary should build");

    let assert = command
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "--fields",
            "document_path",
            "search",
            "release",
            "--has-property",
            "empty_text",
        ])
        .assert()
        .success();
    let json_lines = parse_stdout_json_lines(&assert);

    assert_eq!(json_lines.len(), 1);
    assert_eq!(json_lines[0]["document_path"], "Done.md");
}

#[test]
fn bases_eval_json_output_returns_rows_and_diagnostics() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("bases", &vault_root);
    run_scan(&vault_root);
    let mut command = Command::cargo_bin("vulcan").expect("binary should build");

    let assert = command
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "bases",
            "eval",
            "release.base",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["views"][0]["name"], "Release Table");
    assert_eq!(
        json["views"][0]["filters"],
        serde_json::json!([
            "file.ext = \"md\"",
            "status starts_with \"b\"",
            "estimate > 2"
        ])
    );
    assert_eq!(json["views"][0]["group_by"]["property"], "status");
    assert_eq!(json["views"][0]["columns"][1]["display_name"], "Due");
    assert_eq!(json["views"][0]["rows"][0]["document_path"], "Backlog.md");
    assert_eq!(json["views"][0]["rows"][0]["group_value"], "backlog");
    assert_eq!(
        json["views"][0]["rows"][0]["formulas"]["note_name"],
        "Backlog"
    );
    assert!(json["diagnostics"]
        .as_array()
        .expect("diagnostics should be an array")
        .iter()
        .any(|diagnostic| diagnostic["message"] == "unsupported view type `board`"));
}

#[test]
fn bases_eval_json_fields_stream_rows() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("bases", &vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "--fields",
            "document_path,group_value,cells",
            "bases",
            "eval",
            "release.base",
        ])
        .assert()
        .success();
    let rows = parse_stdout_json_lines(&assert);

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["document_path"], "Backlog.md");
    assert_eq!(rows[0]["group_value"], "backlog");
    assert_eq!(rows[0]["cells"]["note_name"], "Backlog");
}

#[test]
fn bases_human_output_is_compact_and_grouped() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("bases", &vault_root);
    run_scan(&vault_root);

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "bases",
            "eval",
            "release.base",
        ])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("Release Table")
                .and(predicate::str::contains("Grouped by: Status"))
                .and(predicate::str::contains("Group: backlog"))
                .and(predicate::str::contains("| Name | Due |"))
                .and(predicate::str::contains("| --- | --- |"))
                .and(predicate::str::contains("| Backlog |")),
        );
}

#[test]
fn bases_tui_json_output_falls_back_to_eval_report() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("bases", &vault_root);
    run_scan(&vault_root);

    let tui_json = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args([
                "--vault",
                vault_root
                    .to_str()
                    .expect("vault path should be valid utf-8"),
                "--output",
                "json",
                "bases",
                "tui",
                "release.base",
            ])
            .assert()
            .success();
        parse_stdout_json(&assert)
    };

    let eval_json = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args([
                "--vault",
                vault_root
                    .to_str()
                    .expect("vault path should be valid utf-8"),
                "--output",
                "json",
                "bases",
                "eval",
                "release.base",
            ])
            .assert()
            .success();
        parse_stdout_json(&assert)
    };

    assert_eq!(tui_json, eval_json);
}

#[test]
fn search_notes_and_bases_support_file_exports() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("bases", &vault_root);
    run_scan(&vault_root);
    let search_export = temp_dir.path().join("search.csv");
    let notes_export = temp_dir.path().join("notes.jsonl");
    let bases_export = temp_dir.path().join("bases.csv");

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "search",
            "release",
            "--export",
            "csv",
            "--export-path",
            search_export
                .to_str()
                .expect("search export path should be valid utf-8"),
        ])
        .assert()
        .success();
    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "notes",
            "--where",
            "reviewed = true",
            "--export",
            "jsonl",
            "--export-path",
            notes_export
                .to_str()
                .expect("notes export path should be valid utf-8"),
        ])
        .assert()
        .success();
    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "bases",
            "eval",
            "release.base",
            "--export",
            "csv",
            "--export-path",
            bases_export
                .to_str()
                .expect("bases export path should be valid utf-8"),
        ])
        .assert()
        .success();

    let search_csv = fs::read_to_string(&search_export).expect("search export should exist");
    let notes_jsonl = fs::read_to_string(&notes_export).expect("notes export should exist");
    let bases_csv = fs::read_to_string(&bases_export).expect("bases export should exist");

    assert!(search_csv.contains("document_path"));
    assert!(search_csv.contains("Backlog.md"));
    assert_eq!(notes_jsonl.lines().count(), 2);
    assert!(notes_jsonl.contains("\"document_path\":\"Backlog.md\""));
    assert!(bases_csv.contains("document_path"));
    assert!(bases_csv.contains("Backlog.md"));
}

#[test]
#[allow(clippy::too_many_lines)]
fn graph_links_changes_and_cluster_support_file_exports() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    let server = MockEmbeddingServer::spawn();
    write_embedding_config(&vault_root, &server.base_url());
    run_scan(&vault_root);
    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "checkpoint",
            "create",
            "baseline",
        ])
        .assert()
        .success();
    fs::write(
        vault_root.join("Home.md"),
        "---\naliases:\n  - Start\ntags:\n  - dashboard\n---\n\n# Home\n\nUpdated dashboard links.\n",
    )
    .expect("updated note should write");
    run_incremental_scan(&vault_root);
    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "vectors",
            "index",
        ])
        .assert()
        .success();
    let links_export = temp_dir.path().join("links.csv");
    let hubs_export = temp_dir.path().join("hubs.jsonl");
    let changes_export = temp_dir.path().join("changes.csv");
    let cluster_export = temp_dir.path().join("cluster.jsonl");

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "links",
            "Bob",
            "--export",
            "csv",
            "--export-path",
            links_export
                .to_str()
                .expect("links export path should be valid utf-8"),
        ])
        .assert()
        .success();
    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "graph",
            "hubs",
            "--export",
            "jsonl",
            "--export-path",
            hubs_export
                .to_str()
                .expect("hubs export path should be valid utf-8"),
        ])
        .assert()
        .success();
    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "changes",
            "--checkpoint",
            "baseline",
            "--export",
            "csv",
            "--export-path",
            changes_export
                .to_str()
                .expect("changes export path should be valid utf-8"),
        ])
        .assert()
        .success();
    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "cluster",
            "--clusters",
            "2",
            "--export",
            "jsonl",
            "--export-path",
            cluster_export
                .to_str()
                .expect("cluster export path should be valid utf-8"),
        ])
        .assert()
        .success();

    let links_csv = fs::read_to_string(&links_export).expect("links export should exist");
    let hubs_jsonl = fs::read_to_string(&hubs_export).expect("hubs export should exist");
    let changes_csv = fs::read_to_string(&changes_export).expect("changes export should exist");
    let cluster_jsonl = fs::read_to_string(&cluster_export).expect("cluster export should exist");

    assert!(links_csv.contains("Projects/Alpha.md"));
    assert!(links_csv.contains("[[Projects/Alpha#Status]]"));
    assert!(hubs_jsonl.contains("\"document_path\":\"Projects/Alpha.md\""));
    assert!(changes_csv.contains("baseline"));
    assert!(changes_csv.contains("Home.md"));
    assert!(cluster_jsonl.contains("\"cluster_label\""));
    server.shutdown();
}

#[test]
fn export_search_index_writes_static_json_payload() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);
    let export_path = temp_dir.path().join("search-index.json");

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "export",
            "search-index",
            "--path",
            export_path
                .to_str()
                .expect("export path should be valid utf-8"),
            "--pretty",
        ])
        .assert()
        .success();

    let payload: Value = serde_json::from_str(
        &fs::read_to_string(&export_path).expect("search index export should exist"),
    )
    .expect("search index export should parse");

    assert_eq!(payload["version"], 1);
    assert_eq!(payload["documents"], 3);
    assert!(payload["entries"]
        .as_array()
        .expect("entries should be an array")
        .iter()
        .any(|entry| {
            entry["document_path"] == "Home.md"
                && entry["content"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("dashboard")
        }));
}

#[test]
fn export_markdown_combines_matched_notes() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "export",
            "markdown",
            r#"from notes where file.path matches "^(Home|Projects/Alpha)\.md$""#,
            "--title",
            "Project export",
        ])
        .assert()
        .success();

    let out = String::from_utf8_lossy(&assert.get_output().stdout);
    assert!(out.contains("# Project export"));
    assert!(out.contains("## Home.md"));
    assert!(out.contains("## Projects/Alpha.md"));
    assert!(out.contains("Home links to [[Projects/Alpha]]"));
    assert!(out.contains("Owned by [[People/Bob]]"));
}

fn build_export_transform_vault() -> (TempDir, PathBuf) {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join("Projects")).expect("projects dir should exist");
    fs::create_dir_all(vault_root.join("People")).expect("people dir should exist");
    fs::create_dir_all(vault_root.join("assets")).expect("assets dir should exist");
    fs::write(
        vault_root.join("Home.md"),
        concat!(
            "# Home\n\n",
            "Visible [[Projects/Alpha]].\n\n",
            "> [!secret gm]- Internal\n",
            "> Hidden [[People/Bob]].\n",
            "> ![[assets/secret.png]]\n\n",
            "![[assets/public.png]]\n",
        ),
    )
    .expect("home note should write");
    fs::write(vault_root.join("Projects/Alpha.md"), "# Alpha\n").expect("alpha note should write");
    fs::write(vault_root.join("People/Bob.md"), "# Bob\n").expect("bob note should write");
    fs::write(vault_root.join("assets/public.png"), b"public").expect("public asset should write");
    fs::write(vault_root.join("assets/secret.png"), b"secret").expect("secret asset should write");
    run_scan(&vault_root);
    (temp_dir, vault_root)
}

fn build_export_heading_transform_vault() -> (TempDir, PathBuf) {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join("Projects")).expect("projects dir should exist");
    fs::create_dir_all(vault_root.join("People")).expect("people dir should exist");
    fs::create_dir_all(vault_root.join("assets")).expect("assets dir should exist");
    fs::write(
        vault_root.join("Home.md"),
        concat!(
            "# Home\n\n",
            "Visible [[Projects/Alpha]].\n\n",
            "## Scratch\n\n",
            "Hidden [[People/Bob]].\n\n",
            "![[assets/secret.png]]\n\n",
            "## Public\n\n",
            "![[assets/public.png]]\n",
        ),
    )
    .expect("home note should write");
    fs::write(vault_root.join("Projects/Alpha.md"), "# Alpha\n").expect("alpha note should write");
    fs::write(vault_root.join("People/Bob.md"), "# Bob\n").expect("bob note should write");
    fs::write(vault_root.join("assets/public.png"), b"public").expect("public asset should write");
    fs::write(vault_root.join("assets/secret.png"), b"secret").expect("secret asset should write");
    run_scan(&vault_root);
    (temp_dir, vault_root)
}

fn build_export_metadata_transform_vault() -> (TempDir, PathBuf) {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join("Projects")).expect("projects dir should exist");
    fs::create_dir_all(vault_root.join("People")).expect("people dir should exist");
    fs::create_dir_all(vault_root.join("assets")).expect("assets dir should exist");
    fs::write(
        vault_root.join("Home.md"),
        concat!(
            "---\n",
            "public_label: visible\n",
            "email: home@example.com\n",
            "contact: \"[[People/Bob]]\"\n",
            "secret-attachment: \"![[assets/secret-frontmatter.png]]\"\n",
            "---\n\n",
            "# Home\n\n",
            "public:: visible\n",
            "secret:: hidden\n",
            "asset:: ![[assets/secret-inline.png]]\n",
            "Visible [[Projects/Alpha]].\n\n",
            "`= [[Projects/Alpha]].email`\n\n",
            "![[assets/public.png]]\n",
        ),
    )
    .expect("home note should write");
    fs::write(
        vault_root.join("Projects/Alpha.md"),
        concat!(
            "---\n",
            "email: alpha@example.com\n",
            "---\n\n",
            "# Alpha\n"
        ),
    )
    .expect("alpha note should write");
    fs::write(vault_root.join("People/Bob.md"), "# Bob\n").expect("bob note should write");
    fs::write(vault_root.join("assets/public.png"), b"public").expect("public asset should write");
    fs::write(
        vault_root.join("assets/secret-frontmatter.png"),
        b"frontmatter",
    )
    .expect("frontmatter asset should write");
    fs::write(vault_root.join("assets/secret-inline.png"), b"inline")
        .expect("inline asset should write");
    run_scan(&vault_root);
    (temp_dir, vault_root)
}

fn build_export_replacement_transform_vault() -> (TempDir, PathBuf) {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join("Projects")).expect("projects dir should exist");
    fs::create_dir_all(vault_root.join("People")).expect("people dir should exist");
    fs::create_dir_all(vault_root.join("assets")).expect("assets dir should exist");
    fs::write(
        vault_root.join("Home.md"),
        concat!(
            "---\n",
            "email: home@example.com\n",
            "contact: \"[[People/Bob]]\"\n",
            "asset: \"![[assets/secret.png]]\"\n",
            "---\n\n",
            "# Home\n\n",
            "Visible [[People/Bob]].\n\n",
            "`= [[Projects/Alpha]].email`\n\n",
            "![[assets/secret.png]]\n",
        ),
    )
    .expect("home note should write");
    fs::write(
        vault_root.join("Projects/Alpha.md"),
        concat!(
            "---\n",
            "email: alpha@example.com\n",
            "---\n\n",
            "# Alpha\n"
        ),
    )
    .expect("alpha note should write");
    fs::write(vault_root.join("People/Bob.md"), "# Bob\n").expect("bob note should write");
    fs::write(vault_root.join("People/Alice.md"), "# Alice\n").expect("alice note should write");
    fs::write(vault_root.join("assets/public.png"), b"public").expect("public asset should write");
    fs::write(vault_root.join("assets/secret.png"), b"secret").expect("secret asset should write");
    run_scan(&vault_root);
    (temp_dir, vault_root)
}

#[test]
fn export_json_exclude_callout_removes_hidden_content_and_links() {
    let (_temp_dir, vault_root) = build_export_transform_vault();

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "export",
            "json",
            r#"from notes where file.path = "Home.md""#,
            "--exclude-callout",
            "secret gm",
            "--pretty",
        ])
        .assert()
        .success();
    let json: Value = serde_json::from_slice(&assert.get_output().stdout)
        .expect("json export should emit valid JSON");

    let content = json["notes"][0]["content"].as_str().unwrap_or_default();
    let links = json["notes"][0]["links"]
        .as_array()
        .expect("links should be an array");
    assert!(content.contains("Visible [[Projects/Alpha]]."));
    assert!(!content.contains("Hidden [[People/Bob]]."));
    assert!(!content.contains("assets/secret.png"));
    assert!(links.iter().any(|value| value == "[[Projects/Alpha]]"));
    assert!(!links.iter().any(|value| value == "[[People/Bob]]"));
}

#[test]
fn export_json_exclude_heading_removes_hidden_sections_and_links() {
    let (_temp_dir, vault_root) = build_export_heading_transform_vault();

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "export",
            "json",
            r#"from notes where file.path = "Home.md""#,
            "--exclude-heading",
            "scratch",
            "--pretty",
        ])
        .assert()
        .success();
    let json: Value = serde_json::from_slice(&assert.get_output().stdout)
        .expect("json export should emit valid JSON");

    let content = json["notes"][0]["content"].as_str().unwrap_or_default();
    let links = json["notes"][0]["links"]
        .as_array()
        .expect("links should be an array");
    assert!(content.contains("Visible [[Projects/Alpha]]."));
    assert!(content.contains("## Public"));
    assert!(!content.contains("## Scratch"));
    assert!(!content.contains("Hidden [[People/Bob]]."));
    assert!(!content.contains("assets/secret.png"));
    assert!(links.iter().any(|value| value == "[[Projects/Alpha]]"));
    assert!(!links.iter().any(|value| value == "[[People/Bob]]"));
}

#[test]
fn export_json_metadata_transforms_redact_note_metadata_and_cross_note_lookups() {
    let (_temp_dir, vault_root) = build_export_metadata_transform_vault();

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "export",
            "json",
            r#"from notes where file.path matches "^(Home|Projects/Alpha)\.md$""#,
            "--exclude-frontmatter-key",
            "email",
            "--exclude-frontmatter-key",
            "contact",
            "--exclude-frontmatter-key",
            "secret-attachment",
            "--exclude-inline-field",
            "secret",
            "--exclude-inline-field",
            "asset",
            "--pretty",
        ])
        .assert()
        .success();
    let json: Value = serde_json::from_slice(&assert.get_output().stdout)
        .expect("json export should emit valid JSON");

    let notes = json["notes"].as_array().expect("notes should be an array");
    let home = notes
        .iter()
        .find(|note| note["document_path"] == "Home.md")
        .expect("home note should be exported");
    let alpha = notes
        .iter()
        .find(|note| note["document_path"] == "Projects/Alpha.md")
        .expect("alpha note should be exported");

    let home_content = home["content"].as_str().unwrap_or_default();
    assert!(home_content.contains("Visible [[Projects/Alpha]]."));
    assert!(!home_content.contains("secret:: hidden"));
    assert!(!home_content.contains("[[People/Bob]]"));
    assert!(!home_content.contains("secret-frontmatter.png"));
    assert!(!home_content.contains("secret-inline.png"));

    let home_links = home["links"].as_array().expect("links should be an array");
    assert!(home_links.iter().any(|value| value == "[[Projects/Alpha]]"));
    assert!(!home_links.iter().any(|value| value == "[[People/Bob]]"));

    let home_frontmatter = home["frontmatter"]
        .as_object()
        .expect("frontmatter should be an object");
    assert_eq!(
        home_frontmatter.get("public_label"),
        Some(&Value::String("visible".to_string()))
    );
    assert!(!home_frontmatter.contains_key("email"));
    assert!(!home_frontmatter.contains_key("contact"));
    assert!(!home_frontmatter.contains_key("secret-attachment"));

    let home_properties = home["properties"]
        .as_object()
        .expect("properties should be an object");
    assert_eq!(
        home_properties.get("public"),
        Some(&Value::String("visible".to_string()))
    );
    assert!(!home_properties.contains_key("secret"));
    assert!(!home_properties.contains_key("asset"));
    assert!(!home_properties.contains_key("email"));
    assert!(!home_properties.contains_key("contact"));

    let home_inline = home["inline_expressions"]
        .as_array()
        .expect("inline expressions should be an array");
    assert_eq!(home_inline.len(), 1);
    assert_eq!(home_inline[0]["expression"], "[[Projects/Alpha]].email");
    assert_eq!(home_inline[0]["value"], Value::Null);

    let alpha_frontmatter = alpha["frontmatter"]
        .as_object()
        .expect("frontmatter should be an object");
    assert!(!alpha_frontmatter.contains_key("email"));
    let alpha_properties = alpha["properties"]
        .as_object()
        .expect("properties should be an object");
    assert!(!alpha_properties.contains_key("email"));
}

#[test]
fn export_json_replacement_transforms_rewrite_content_links_and_metadata() {
    let (_temp_dir, vault_root) = build_export_replacement_transform_vault();

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "export",
            "json",
            r#"from notes where file.path matches "^(Home|Projects/Alpha)\.md$""#,
            "--replace-rule",
            "literal",
            "[[People/Bob]]",
            "[[People/Alice]]",
            "--replace-rule",
            "regex",
            "[A-Za-z0-9._%+-]+@example\\.com",
            "redacted",
            "--pretty",
        ])
        .assert()
        .success();
    let json: Value = serde_json::from_slice(&assert.get_output().stdout)
        .expect("json export should emit valid JSON");

    let notes = json["notes"].as_array().expect("notes should be an array");
    let home = notes
        .iter()
        .find(|note| note["document_path"] == "Home.md")
        .expect("home note should be exported");
    let alpha = notes
        .iter()
        .find(|note| note["document_path"] == "Projects/Alpha.md")
        .expect("alpha note should be exported");

    let home_content = home["content"].as_str().unwrap_or_default();
    assert!(home_content.contains("Visible [[People/Alice]]."));
    assert!(!home_content.contains("[[People/Bob]]"));

    let home_links = home["links"].as_array().expect("links should be an array");
    assert!(home_links.iter().any(|value| value == "[[People/Alice]]"));
    assert!(!home_links.iter().any(|value| value == "[[People/Bob]]"));

    let home_frontmatter = home["frontmatter"]
        .as_object()
        .expect("frontmatter should be an object");
    assert_eq!(
        home_frontmatter.get("email"),
        Some(&Value::String("redacted".to_string()))
    );
    assert_eq!(
        home_frontmatter.get("contact"),
        Some(&Value::String("[[People/Alice]]".to_string()))
    );

    let home_properties = home["properties"]
        .as_object()
        .expect("properties should be an object");
    assert_eq!(
        home_properties.get("email"),
        Some(&Value::String("redacted".to_string()))
    );
    assert_eq!(
        home_properties.get("contact"),
        Some(&Value::String("[[People/Alice]]".to_string()))
    );

    let home_inline = home["inline_expressions"]
        .as_array()
        .expect("inline expressions should be an array");
    assert_eq!(home_inline.len(), 1);
    assert_eq!(home_inline[0]["expression"], "[[Projects/Alpha]].email");
    assert_eq!(
        home_inline[0]["value"],
        Value::String("redacted".to_string())
    );

    let alpha_frontmatter = alpha["frontmatter"]
        .as_object()
        .expect("frontmatter should be an object");
    assert_eq!(
        alpha_frontmatter.get("email"),
        Some(&Value::String("redacted".to_string()))
    );
    let alpha_properties = alpha["properties"]
        .as_object()
        .expect("properties should be an object");
    assert_eq!(
        alpha_properties.get("email"),
        Some(&Value::String("redacted".to_string()))
    );
}

#[test]
fn export_zip_exclude_callout_skips_hidden_attachments() {
    let (temp_dir, vault_root) = build_export_transform_vault();
    let export_path = temp_dir.path().join("public.zip");

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "export",
            "zip",
            r#"from notes where file.path = "Home.md""#,
            "--exclude-callout",
            "secret gm",
            "-o",
            export_path
                .to_str()
                .expect("export path should be valid utf-8"),
        ])
        .assert()
        .success();

    let file = fs::File::open(&export_path).expect("zip export should exist");
    let mut archive = ZipArchive::new(file).expect("zip export should open");
    let mut names = Vec::new();
    for index in 0..archive.len() {
        names.push(
            archive
                .by_index(index)
                .expect("zip entry should be readable")
                .name()
                .to_string(),
        );
    }

    assert!(names.contains(&"Home.md".to_string()));
    assert!(names.contains(&"assets/public.png".to_string()));
    assert!(!names.contains(&"assets/secret.png".to_string()));

    let mut notes_json = String::new();
    archive
        .by_name(".vulcan-export/notes.json")
        .expect("notes manifest should exist")
        .read_to_string(&mut notes_json)
        .expect("notes manifest should be readable");
    assert!(!notes_json.contains("assets/secret.png"));
}

#[test]
fn export_zip_metadata_transforms_skip_hidden_metadata_attachments() {
    let (temp_dir, vault_root) = build_export_metadata_transform_vault();
    let export_path = temp_dir.path().join("public.zip");

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "export",
            "zip",
            r#"from notes where file.path = "Home.md""#,
            "--exclude-frontmatter-key",
            "secret-attachment",
            "--exclude-inline-field",
            "asset",
            "-o",
            export_path
                .to_str()
                .expect("export path should be valid utf-8"),
        ])
        .assert()
        .success();

    let file = fs::File::open(&export_path).expect("zip export should exist");
    let mut archive = ZipArchive::new(file).expect("zip export should open");
    let mut names = Vec::new();
    for index in 0..archive.len() {
        names.push(
            archive
                .by_index(index)
                .expect("zip entry should be readable")
                .name()
                .to_string(),
        );
    }

    assert!(names.contains(&"Home.md".to_string()));
    assert!(names.contains(&"assets/public.png".to_string()));
    assert!(!names.contains(&"assets/secret-frontmatter.png".to_string()));
    assert!(!names.contains(&"assets/secret-inline.png".to_string()));

    let mut notes_json = String::new();
    archive
        .by_name(".vulcan-export/notes.json")
        .expect("notes manifest should exist")
        .read_to_string(&mut notes_json)
        .expect("notes manifest should be readable");
    assert!(!notes_json.contains("secret-frontmatter.png"));
    assert!(!notes_json.contains("secret-inline.png"));
}

#[test]
fn export_zip_replacement_transforms_rewrite_attachment_references() {
    let (temp_dir, vault_root) = build_export_replacement_transform_vault();
    let export_path = temp_dir.path().join("public.zip");

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "export",
            "zip",
            r#"from notes where file.path = "Home.md""#,
            "--replace-rule",
            "literal",
            "assets/secret.png",
            "assets/public.png",
            "-o",
            export_path
                .to_str()
                .expect("export path should be valid utf-8"),
        ])
        .assert()
        .success();

    let file = fs::File::open(&export_path).expect("zip export should exist");
    let mut archive = ZipArchive::new(file).expect("zip export should open");
    let mut names = Vec::new();
    for index in 0..archive.len() {
        names.push(
            archive
                .by_index(index)
                .expect("zip entry should be readable")
                .name()
                .to_string(),
        );
    }

    assert!(names.contains(&"Home.md".to_string()));
    assert!(names.contains(&"assets/public.png".to_string()));
    assert!(!names.contains(&"assets/secret.png".to_string()));

    let mut exported_home = String::new();
    archive
        .by_name("Home.md")
        .expect("exported note should exist")
        .read_to_string(&mut exported_home)
        .expect("exported note should be readable");
    assert!(exported_home.contains("assets/public.png"));
    assert!(!exported_home.contains("assets/secret.png"));

    let mut notes_json = String::new();
    archive
        .by_name(".vulcan-export/notes.json")
        .expect("notes manifest should exist")
        .read_to_string(&mut notes_json)
        .expect("notes manifest should be readable");
    assert!(notes_json.contains("assets/public.png"));
    assert!(!notes_json.contains("assets/secret.png"));
}

#[test]
fn export_zip_exclude_heading_skips_hidden_section_attachments() {
    let (temp_dir, vault_root) = build_export_heading_transform_vault();
    let export_path = temp_dir.path().join("public.zip");

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "export",
            "zip",
            r#"from notes where file.path = "Home.md""#,
            "--exclude-heading",
            "scratch",
            "-o",
            export_path
                .to_str()
                .expect("export path should be valid utf-8"),
        ])
        .assert()
        .success();

    let file = fs::File::open(&export_path).expect("zip export should exist");
    let mut archive = ZipArchive::new(file).expect("zip export should open");
    let mut names = Vec::new();
    for index in 0..archive.len() {
        names.push(
            archive
                .by_index(index)
                .expect("zip entry should be readable")
                .name()
                .to_string(),
        );
    }

    assert!(names.contains(&"Home.md".to_string()));
    assert!(names.contains(&"assets/public.png".to_string()));
    assert!(!names.contains(&"assets/secret.png".to_string()));
}

#[test]
fn export_epub_exclude_callout_filters_backlinks_and_hidden_assets() {
    let (temp_dir, vault_root) = build_export_transform_vault();
    let export_path = temp_dir.path().join("public.epub");

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "export",
            "epub",
            r#"from notes where file.path matches "^(Home|Projects/Alpha|People/Bob)\.md$""#,
            "--exclude-callout",
            "secret gm",
            "--backlinks",
            "-o",
            export_path
                .to_str()
                .expect("export path should be valid utf-8"),
        ])
        .assert()
        .success();

    let file = fs::File::open(&export_path).expect("epub export should exist");
    let mut archive = ZipArchive::new(file).expect("epub export should open");

    let mut names = Vec::new();
    for index in 0..archive.len() {
        names.push(
            archive
                .by_index(index)
                .expect("archive entry should be readable")
                .name()
                .to_string(),
        );
    }
    let media_entries = names
        .iter()
        .filter(|name| name.starts_with("OEBPS/media/asset-"))
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(media_entries.len(), 1);
    assert!(media_entries.iter().all(|name| has_extension(name, "png")));

    let mut chapter_by_note = std::collections::HashMap::new();
    for name in names
        .iter()
        .filter(|name| name.starts_with("OEBPS/text/chapter-"))
    {
        let mut chapter = String::new();
        archive
            .by_name(name)
            .expect("chapter should exist")
            .read_to_string(&mut chapter)
            .expect("chapter should be readable");
        if chapter.contains("Home.md") {
            chapter_by_note.insert("Home.md", chapter);
        } else if chapter.contains("Projects/Alpha.md") {
            chapter_by_note.insert("Projects/Alpha.md", chapter);
        } else if chapter.contains("People/Bob.md") {
            chapter_by_note.insert("People/Bob.md", chapter);
        }
    }

    let home_chapter = chapter_by_note
        .get("Home.md")
        .expect("home chapter should be captured");
    let alpha_chapter = chapter_by_note
        .get("Projects/Alpha.md")
        .expect("alpha chapter should be captured");
    let bob_chapter = chapter_by_note
        .get("People/Bob.md")
        .expect("bob chapter should be captured");

    assert!(home_chapter.contains("asset-embed asset-embed-image"));
    assert!(!home_chapter.contains("Hidden [[People/Bob]]."));
    assert!(!home_chapter.contains("assets/secret.png"));
    assert!(alpha_chapter.contains("<section class=\"backlinks\">"));
    assert!(alpha_chapter.contains(">Home</a>"));
    assert!(!bob_chapter.contains(">Home</a>"));
}

#[test]
fn export_json_emits_note_metadata_and_content() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "export",
            "json",
            r#"from notes where file.path = "Home.md""#,
            "--pretty",
        ])
        .assert()
        .success();
    let json: Value = serde_json::from_slice(&assert.get_output().stdout)
        .expect("json export should emit valid JSON");

    assert_eq!(json["result_count"], Value::Number(1.into()));
    assert_eq!(json["notes"][0]["document_path"], "Home.md");
    assert_eq!(json["notes"][0]["file_name"], "Home");
    assert_eq!(
        json["notes"][0]["frontmatter"]["aliases"][0],
        Value::String("Start".to_string())
    );
    assert!(json["notes"][0]["content"]
        .as_str()
        .unwrap_or_default()
        .contains("Home links to [[Projects/Alpha]]"));
}

#[test]
fn export_csv_writes_query_rows() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);
    let export_path = temp_dir.path().join("notes.csv");

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "export",
            "csv",
            r#"from notes where file.path = "Projects/Alpha.md""#,
            "-o",
            export_path
                .to_str()
                .expect("export path should be valid utf-8"),
        ])
        .assert()
        .success();

    let csv = fs::read_to_string(&export_path).expect("csv export should exist");
    assert!(csv.starts_with(
        "document_path,file_name,file_ext,file_mtime,tags,starred,properties,inline_expressions,query"
    ));
    assert!(csv.contains("Projects/Alpha.md"));
    assert!(csv.contains("Alpha"));
}

#[test]
fn export_graph_json_format_emits_nodes_and_edges() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "export",
            "graph",
            "--format",
            "json",
        ])
        .assert()
        .success();

    let parsed: Value = serde_json::from_slice(&assert.get_output().stdout)
        .expect("graph export json should be valid JSON");
    assert!(parsed["nodes"]
        .as_array()
        .is_some_and(|nodes| !nodes.is_empty()));
    assert!(parsed["edges"]
        .as_array()
        .is_some_and(|edges| !edges.is_empty()));
}

#[test]
fn export_zip_includes_notes_attachments_and_manifest() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("attachments", &vault_root);
    run_scan(&vault_root);
    let export_path = temp_dir.path().join("attachments.zip");

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "export",
            "zip",
            r#"from notes where file.path = "Home.md""#,
            "-o",
            export_path
                .to_str()
                .expect("export path should be valid utf-8"),
        ])
        .assert()
        .success();

    let file = fs::File::open(&export_path).expect("zip export should exist");
    let mut archive = ZipArchive::new(file).expect("zip export should open");
    let mut names = Vec::new();
    for index in 0..archive.len() {
        names.push(
            archive
                .by_index(index)
                .expect("zip entry should be readable")
                .name()
                .to_string(),
        );
    }

    assert!(names.contains(&"Home.md".to_string()));
    assert!(names.contains(&"assets/logo.png".to_string()));
    assert!(names.contains(&"assets/guide.pdf".to_string()));
    assert!(names.contains(&"audio/theme.mp3".to_string()));
    assert!(names.contains(&".vulcan-export/manifest.json".to_string()));
    assert!(names.contains(&".vulcan-export/notes.json".to_string()));

    let mut manifest = String::new();
    archive
        .by_name(".vulcan-export/manifest.json")
        .expect("manifest should exist")
        .read_to_string(&mut manifest)
        .expect("manifest should be readable");
    let manifest_json: Value =
        serde_json::from_str(&manifest).expect("manifest should be valid JSON");
    assert_eq!(manifest_json["result_count"], Value::Number(1.into()));
    assert!(manifest_json["attachments"]
        .as_array()
        .expect("attachments should be an array")
        .iter()
        .any(|path| path == "assets/logo.png"));
}

#[test]
fn export_epub_writes_book_archive_with_nav_and_backlinks() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);
    let export_path = temp_dir.path().join("team.epub");

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "export",
            "epub",
            "from notes",
            "-o",
            export_path
                .to_str()
                .expect("export path should be valid utf-8"),
            "--title",
            "Team Notes",
            "--author",
            "Vulcan",
            "--backlinks",
        ])
        .assert()
        .success();

    let file = fs::File::open(&export_path).expect("epub export should exist");
    let mut archive = ZipArchive::new(file).expect("epub export should open");

    let mut mimetype = String::new();
    archive
        .by_name("mimetype")
        .expect("mimetype should exist")
        .read_to_string(&mut mimetype)
        .expect("mimetype should be readable");
    assert_eq!(mimetype, "application/epub+zip");

    let mut nav = String::new();
    archive
        .by_name("OEBPS/nav.xhtml")
        .expect("nav should exist")
        .read_to_string(&mut nav)
        .expect("nav should be readable");
    assert!(nav.contains("Team Notes"));
    assert!(nav.contains("Contents"));
    assert!(nav.contains("Status"));

    let mut chapter_paths = Vec::new();
    for index in 0..archive.len() {
        let name = archive
            .by_index(index)
            .expect("archive entry should be readable")
            .name()
            .to_string();
        if name.starts_with("OEBPS/text/chapter-") {
            chapter_paths.push(name);
        }
    }
    assert_eq!(chapter_paths.len(), 3);

    let mut chapter_by_note = std::collections::HashMap::new();
    for path in &chapter_paths {
        let mut chapter = String::new();
        archive
            .by_name(path)
            .expect("chapter should exist")
            .read_to_string(&mut chapter)
            .expect("chapter should be readable");
        if chapter.contains("Home.md") {
            chapter_by_note.insert("Home.md", (path.clone(), chapter));
        } else if chapter.contains("People/Bob.md") {
            chapter_by_note.insert("People/Bob.md", (path.clone(), chapter));
        } else if chapter.contains("Projects/Alpha.md") {
            chapter_by_note.insert("Projects/Alpha.md", (path.clone(), chapter));
        }
    }

    let alpha_file = chapter_by_note
        .get("Projects/Alpha.md")
        .expect("alpha chapter should be captured")
        .0
        .trim_start_matches("OEBPS/text/")
        .to_string();
    let bob_chapter = &chapter_by_note
        .get("People/Bob.md")
        .expect("bob chapter should be captured")
        .1;
    let alpha_chapter = &chapter_by_note
        .get("Projects/Alpha.md")
        .expect("alpha chapter should be captured")
        .1;

    assert!(!alpha_chapter.contains("frontmatter-box"));
    assert!(bob_chapter.contains(&format!("href=\"{alpha_file}#status\"")));
    assert!(alpha_chapter.contains("<section class=\"backlinks\">"));
    assert!(alpha_chapter.contains(">Home</a>"));
    assert!(alpha_chapter.contains(">People/Bob</a>"));
}

#[test]
fn export_epub_bundles_and_rewrites_referenced_assets() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("attachments", &vault_root);

    let guide_path = vault_root.join("Notes/Guide.md");
    let mut guide = fs::read_to_string(&guide_path).expect("guide note should be readable");
    guide.push_str("\n[Manual](../assets/guide.pdf)\n");
    fs::write(&guide_path, guide).expect("guide note should be updated");

    run_scan(&vault_root);
    let export_path = temp_dir.path().join("attachments.epub");

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "export",
            "epub",
            "from notes",
            "-o",
            export_path
                .to_str()
                .expect("export path should be valid utf-8"),
        ])
        .assert()
        .success();

    let file = fs::File::open(&export_path).expect("epub export should exist");
    let mut archive = ZipArchive::new(file).expect("epub export should open");

    let mut names = Vec::new();
    for index in 0..archive.len() {
        names.push(
            archive
                .by_index(index)
                .expect("archive entry should be readable")
                .name()
                .to_string(),
        );
    }

    let media_entries = names
        .iter()
        .filter(|name| name.starts_with("OEBPS/media/asset-"))
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(media_entries.len(), 3);
    assert!(media_entries.iter().any(|name| has_extension(name, "png")));
    assert!(media_entries.iter().any(|name| has_extension(name, "pdf")));
    assert!(media_entries.iter().any(|name| has_extension(name, "mp3")));

    let mut content_opf = String::new();
    archive
        .by_name("OEBPS/content.opf")
        .expect("package should exist")
        .read_to_string(&mut content_opf)
        .expect("package should be readable");
    assert!(content_opf.contains("media-type=\"image/png\""));
    assert!(content_opf.contains("media-type=\"application/pdf\""));
    assert!(content_opf.contains("media-type=\"audio/mpeg\""));

    let mut chapter_by_note = std::collections::HashMap::new();
    for name in names
        .iter()
        .filter(|name| name.starts_with("OEBPS/text/chapter-"))
    {
        let mut chapter = String::new();
        archive
            .by_name(name)
            .expect("chapter should exist")
            .read_to_string(&mut chapter)
            .expect("chapter should be readable");
        if chapter.contains("Home.md") {
            chapter_by_note.insert("Home.md", chapter);
        } else if chapter.contains("Notes/Guide.md") {
            chapter_by_note.insert("Notes/Guide.md", chapter);
        }
    }

    let home_chapter = chapter_by_note
        .get("Home.md")
        .expect("home chapter should be captured");
    let guide_chapter = chapter_by_note
        .get("Notes/Guide.md")
        .expect("guide chapter should be captured");

    assert!(home_chapter.contains("class=\"asset-embed asset-embed-image\""));
    assert!(home_chapter.contains("src=\"../media/asset-"));
    assert!(home_chapter.contains(".png\""));
    assert!(home_chapter.contains("class=\"asset-embed asset-embed-link\""));
    assert!(home_chapter.contains(".pdf\""));
    assert!(home_chapter.contains("class=\"asset-embed asset-embed-audio\""));
    assert!(home_chapter.contains(".mp3\""));

    assert!(guide_chapter.contains("class=\"asset-embed asset-embed-image\""));
    assert!(guide_chapter.contains("src=\"../media/asset-"));
    assert!(guide_chapter.contains(".png\""));
    assert!(guide_chapter.contains(">Manual</a>"));
    assert!(guide_chapter.contains("href=\"../media/asset-"));
    assert!(guide_chapter.contains(".pdf\""));
}

#[test]
fn export_epub_frontmatter_flag_renders_collapsible_yaml_panel() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);
    let export_path = temp_dir.path().join("home.epub");

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "export",
            "epub",
            r#"from notes where file.path = "Home.md""#,
            "-o",
            export_path
                .to_str()
                .expect("export path should be valid utf-8"),
            "--frontmatter",
        ])
        .assert()
        .success();

    let file = fs::File::open(&export_path).expect("epub export should exist");
    let mut archive = ZipArchive::new(file).expect("epub export should open");
    let mut chapter = String::new();
    archive
        .by_name("OEBPS/text/chapter-001.xhtml")
        .expect("chapter should exist")
        .read_to_string(&mut chapter)
        .expect("chapter should be readable");

    assert!(chapter.contains("<details class=\"frontmatter-box\">"));
    assert!(chapter.contains("<summary>Frontmatter</summary>"));
    assert!(chapter.contains("<pre><code>---"));
    assert!(chapter.contains("aliases:"));
    assert!(chapter.contains("- Start"));
    assert!(chapter.contains("tags:"));
    assert!(chapter.contains("- dashboard"));
    assert!(chapter.contains("<h1 id=\"home\">Home</h1>"));
}

#[test]
#[allow(clippy::too_many_lines)]
fn export_profiles_list_and_run_named_epub_profile() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);

    let vault_root_str = vault_root
        .to_str()
        .expect("vault path should be valid utf-8")
        .to_string();

    let create_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "--output",
            "json",
            "export",
            "profile",
            "create",
            "team_book",
            "--format",
            "epub",
            r#"from notes where file.path matches "^(Home|Projects/Alpha)\.md$""#,
            "-o",
            "exports/team-book.epub",
            "--title",
            "Team Book",
            "--author",
            "Vulcan",
            "--backlinks",
            "--frontmatter",
        ])
        .assert()
        .success();
    let create_json = parse_stdout_json(&create_assert);
    assert_eq!(create_json["name"], "team_book");
    assert_eq!(create_json["created_config"], Value::Bool(true));
    assert_eq!(create_json["action"], "created");
    assert_eq!(create_json["profile"]["format"], "epub");
    assert_eq!(create_json["profile"]["path"], "exports/team-book.epub");
    assert_eq!(create_json["profile"]["title"], "Team Book");
    assert_eq!(create_json["profile"]["author"], "Vulcan");
    assert_eq!(create_json["profile"]["backlinks"], Value::Bool(true));
    assert_eq!(create_json["profile"]["frontmatter"], Value::Bool(true));

    let profiles_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "--output",
            "json",
            "export",
            "profile",
            "list",
        ])
        .assert()
        .success();
    let profiles_json = parse_stdout_json(&profiles_assert);
    let profiles = profiles_json
        .as_array()
        .expect("profiles output should be an array");
    assert_eq!(profiles.len(), 1);
    assert_eq!(profiles[0]["name"], "team_book");
    assert_eq!(profiles[0]["format"], "epub");
    assert_eq!(profiles[0]["path"], "exports/team-book.epub");
    assert_eq!(
        profiles[0]["resolved_path"],
        vault_root
            .join("exports/team-book.epub")
            .display()
            .to_string()
    );

    let export_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "--output",
            "json",
            "export",
            "profile",
            "run",
            "team_book",
        ])
        .assert()
        .success();
    let export_json = parse_stdout_json(&export_assert);
    assert_eq!(export_json["name"], "team_book");
    assert_eq!(export_json["format"], "epub");
    assert_eq!(
        export_json["summary"]["path"],
        vault_root
            .join("exports/team-book.epub")
            .display()
            .to_string()
    );
    assert_eq!(
        export_json["summary"]["result_count"],
        Value::Number(2.into())
    );

    let show_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "--output",
            "json",
            "export",
            "profile",
            "show",
            "team_book",
        ])
        .assert()
        .success();
    let show_json = parse_stdout_json(&show_assert);
    assert_eq!(show_json["name"], "team_book");
    assert_eq!(show_json["profile"]["format"], "epub");
    assert_eq!(show_json["profile"]["path"], "exports/team-book.epub");
    assert_eq!(show_json["profile"]["title"], "Team Book");

    let export_path = vault_root.join("exports/team-book.epub");
    let file = fs::File::open(&export_path).expect("epub export should exist");
    let mut archive = ZipArchive::new(file).expect("epub export should open");

    let mut chapter_paths = Vec::new();
    for index in 0..archive.len() {
        let name = archive
            .by_index(index)
            .expect("archive entry should be readable")
            .name()
            .to_string();
        if name.starts_with("OEBPS/text/chapter-") {
            chapter_paths.push(name);
        }
    }
    assert_eq!(chapter_paths.len(), 2);

    let mut saw_frontmatter = false;
    let mut saw_backlinks = false;
    for path in chapter_paths {
        let mut chapter = String::new();
        archive
            .by_name(&path)
            .expect("chapter should exist")
            .read_to_string(&mut chapter)
            .expect("chapter should be readable");
        saw_frontmatter |= chapter.contains("frontmatter-box");
        saw_backlinks |= chapter.contains("<section class=\"backlinks\">");
    }

    assert!(saw_frontmatter);
    assert!(saw_backlinks);

    let delete_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "--output",
            "json",
            "export",
            "profile",
            "delete",
            "team_book",
        ])
        .assert()
        .success();
    let delete_json = parse_stdout_json(&delete_assert);
    assert_eq!(delete_json["name"], "team_book");
    assert_eq!(delete_json["deleted"], Value::Bool(true));

    let profiles_after_delete_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "--output",
            "json",
            "export",
            "profile",
            "list",
        ])
        .assert()
        .success();
    let profiles_after_delete_json = parse_stdout_json(&profiles_after_delete_assert);
    assert_eq!(
        profiles_after_delete_json
            .as_array()
            .expect("profiles output should be an array")
            .len(),
        0
    );
    let config_contents =
        fs::read_to_string(vault_root.join(".vulcan/config.toml")).expect("config should exist");
    assert!(!config_contents.contains("team_book"));
}

#[test]
fn export_profile_set_updates_profile_fields() {
    let (_temp_dir, vault_root) = build_export_transform_vault();
    let vault_root_str = vault_root
        .to_str()
        .expect("vault path should be valid utf-8")
        .to_string();

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "--output",
            "json",
            "export",
            "profile",
            "create",
            "team_book",
            "--format",
            "epub",
            r#"from notes where file.path matches "^(Home|Projects/Alpha)\.md$""#,
            "-o",
            "exports/team.epub",
            "--title",
            "Team Book",
        ])
        .assert()
        .success();

    let set_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "--output",
            "json",
            "export",
            "profile",
            "set",
            "team_book",
            "--backlinks",
            "--frontmatter",
            "--author",
            "Vulcan",
        ])
        .assert()
        .success();
    let set_json = parse_stdout_json(&set_assert);
    assert_eq!(set_json["action"], "updated");
    assert_eq!(set_json["profile"]["author"], "Vulcan");
    assert_eq!(set_json["profile"]["backlinks"], Value::Bool(true));
    assert_eq!(set_json["profile"]["frontmatter"], Value::Bool(true));

    let show_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "--output",
            "json",
            "export",
            "profile",
            "show",
            "team_book",
        ])
        .assert()
        .success();
    let show_json = parse_stdout_json(&show_assert);
    assert_eq!(show_json["profile"]["author"], "Vulcan");
    assert_eq!(show_json["profile"]["backlinks"], Value::Bool(true));
    assert_eq!(show_json["profile"]["frontmatter"], Value::Bool(true));
}

#[test]
fn export_profile_create_and_run_support_content_transforms() {
    let (_temp_dir, vault_root) = build_export_replacement_transform_vault();
    let vault_root_str = vault_root
        .to_str()
        .expect("vault path should be valid utf-8")
        .to_string();

    let create_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "--output",
            "json",
            "export",
            "profile",
            "create",
            "public_json",
            "--format",
            "json",
            r#"from notes where file.path matches "^(Home|Projects/Alpha)\.md$""#,
            "-o",
            "exports/public.json",
        ])
        .assert()
        .success();
    let create_json = parse_stdout_json(&create_assert);
    assert_eq!(create_json["profile"]["format"], "json");
    assert!(create_json["profile"]["content_transforms"].is_null());

    let add_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "--output",
            "json",
            "export",
            "profile",
            "rule",
            "add",
            "public_json",
            "--replace-rule",
            "literal",
            "[[People/Bob]]",
            "[[People/Alice]]",
            "--replace-rule",
            "regex",
            "[A-Za-z0-9._%+-]+@example\\.com",
            "redacted",
        ])
        .assert()
        .success();
    let add_json = parse_stdout_json(&add_assert);
    assert_eq!(add_json["action"], "added");
    assert_eq!(add_json["rule_index"], 1);
    assert_eq!(add_json["rule"]["replace"][0]["pattern"], "[[People/Bob]]");
    assert_eq!(
        add_json["rule"]["replace"][0]["replacement"],
        "[[People/Alice]]"
    );
    assert!(add_json["rule"]["replace"][0]["regex"].is_null());
    assert_eq!(
        add_json["rule"]["replace"][1]["pattern"],
        "[A-Za-z0-9._%+-]+@example\\.com"
    );
    assert_eq!(add_json["rule"]["replace"][1]["replacement"], "redacted");
    assert_eq!(add_json["rule"]["replace"][1]["regex"], Value::Bool(true));

    let run_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "--output",
            "json",
            "export",
            "profile",
            "run",
            "public_json",
        ])
        .assert()
        .success();
    let run_json = parse_stdout_json(&run_assert);
    assert_eq!(run_json["name"], "public_json");

    let exported = fs::read_to_string(vault_root.join("exports/public.json"))
        .expect("profile output should exist");
    assert!(exported.contains("[[People/Alice]]"));
    assert!(!exported.contains("[[People/Bob]]"));
    assert!(exported.contains("redacted"));

    let config_contents =
        fs::read_to_string(vault_root.join(".vulcan/config.toml")).expect("config should exist");
    assert!(config_contents.contains("[[export.profiles.public_json.content_transforms]]"));
    assert!(config_contents.contains("[[export.profiles.public_json.content_transforms.replace]]"));
}

#[test]
fn export_profile_create_rejects_format_specific_invalid_flags() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "export",
            "profile",
            "create",
            "graph_bad",
            "--format",
            "graph",
            "from notes",
            "-o",
            "exports/graph.json",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "does not use `query` or `query_json` for graph exports",
        ));
}

#[test]
fn export_profile_create_rejects_content_transforms_for_unsupported_formats() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "export",
            "profile",
            "create",
            "search_index_bad",
            "--format",
            "search-index",
            "-o",
            "exports/search.json",
        ])
        .assert()
        .success();

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "export",
            "profile",
            "rule",
            "add",
            "search_index_bad",
            "--exclude-callout",
            "secret gm",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "only supports `content_transforms` for markdown, json, epub, and zip exports",
        ));
}

#[test]
#[allow(clippy::too_many_lines)]
fn export_profile_rule_move_reorders_replacement_rules() {
    let (_temp_dir, vault_root) = build_export_replacement_transform_vault();
    let vault_root_str = vault_root
        .to_str()
        .expect("vault path should be valid utf-8")
        .to_string();

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "export",
            "profile",
            "create",
            "public_json",
            "--format",
            "json",
            r#"from notes where file.path = "Home.md""#,
            "-o",
            "exports/public.json",
        ])
        .assert()
        .success();

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "export",
            "profile",
            "rule",
            "add",
            "public_json",
            "--replace-rule",
            "literal",
            "[[People/Bob]]",
            "[[People/Alice]]",
        ])
        .assert()
        .success();

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "export",
            "profile",
            "rule",
            "add",
            "public_json",
            "--replace-rule",
            "literal",
            "[[People/Alice]]",
            "[[People/Carol]]",
        ])
        .assert()
        .success();

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "export",
            "profile",
            "run",
            "public_json",
        ])
        .assert()
        .success();
    let exported_before = fs::read_to_string(vault_root.join("exports/public.json"))
        .expect("profile output should exist");
    assert!(exported_before.contains("[[People/Carol]]"));
    assert!(!exported_before.contains("[[People/Alice]]"));

    let move_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "--output",
            "json",
            "export",
            "profile",
            "rule",
            "move",
            "public_json",
            "2",
            "--before",
            "1",
        ])
        .assert()
        .success();
    let move_json = parse_stdout_json(&move_assert);
    assert_eq!(move_json["action"], "moved");
    assert_eq!(move_json["previous_rule_index"], 2);
    assert_eq!(move_json["rule_index"], 1);

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "export",
            "profile",
            "run",
            "public_json",
        ])
        .assert()
        .success();
    let exported_after = fs::read_to_string(vault_root.join("exports/public.json"))
        .expect("profile output should exist");
    assert!(exported_after.contains("[[People/Alice]]"));
    assert!(!exported_after.contains("[[People/Carol]]"));
}

fn build_export_transform_rule_vault() -> (TempDir, PathBuf) {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join("Projects")).expect("projects dir should exist");
    fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should exist");
    fs::write(
        vault_root.join("Home.md"),
        concat!(
            "# Home\n\n",
            "Visible home text.\n\n",
            "> [!secret gm]\n",
            "> Hidden home details.\n",
        ),
    )
    .expect("home note should write");
    fs::write(
        vault_root.join("Projects/Alpha.md"),
        concat!(
            "# Alpha\n\n",
            "Visible alpha text.\n\n",
            "> [!secret gm]\n",
            "> Hidden alpha details.\n",
        ),
    )
    .expect("alpha note should write");
    fs::write(vault_root.join("People.md"), "# People\n").expect("people note should write");
    run_scan(&vault_root);
    (temp_dir, vault_root)
}

#[test]
fn export_profile_content_transform_rule_query_only_applies_to_matching_exported_notes() {
    let (_temp_dir, vault_root) = build_export_transform_rule_vault();
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        r#"
[export.profiles.public_json]
format = "json"
query = 'from notes where file.path matches "^(Home|Projects/Alpha)\.md$"'
path = "exports/public.json"

[[export.profiles.public_json.content_transforms]]
query = 'from notes where file.path = "Home.md"'
exclude_callouts = ["secret gm"]
"#,
    )
    .expect("config should be written");

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "export",
            "profile",
            "run",
            "public_json",
        ])
        .assert()
        .success();

    let exported = fs::read_to_string(vault_root.join("exports/public.json"))
        .expect("profile export should exist");
    let json: Value = serde_json::from_str(&exported).expect("export should be valid json");
    let notes = json["notes"].as_array().expect("notes should be an array");
    assert_eq!(notes.len(), 2);

    let home = notes
        .iter()
        .find(|note| note["document_path"] == "Home.md")
        .expect("home note should be exported");
    let alpha = notes
        .iter()
        .find(|note| note["document_path"] == "Projects/Alpha.md")
        .expect("alpha note should be exported");

    assert!(!home["content"]
        .as_str()
        .unwrap_or_default()
        .contains("Hidden home details."));
    assert!(alpha["content"]
        .as_str()
        .unwrap_or_default()
        .contains("Hidden alpha details."));
}

#[test]
fn export_profile_content_transform_rule_query_does_not_expand_export_selection() {
    let (_temp_dir, vault_root) = build_export_transform_vault();
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        r#"
[export.profiles.public_json]
format = "json"
query = 'from notes where file.path = "Home.md"'
path = "exports/public.json"

[[export.profiles.public_json.content_transforms]]
query = 'from notes where file.path = "People/Bob.md"'
exclude_callouts = ["secret gm"]
"#,
    )
    .expect("config should be written");

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "export",
            "profile",
            "run",
            "public_json",
        ])
        .assert()
        .success();

    let exported = fs::read_to_string(vault_root.join("exports/public.json"))
        .expect("profile export should exist");
    let json: Value = serde_json::from_str(&exported).expect("export should be valid json");
    let notes = json["notes"].as_array().expect("notes should be an array");
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0]["document_path"], "Home.md");
    assert!(notes[0]["content"]
        .as_str()
        .unwrap_or_default()
        .contains("Hidden [[People/Bob]]."));
}

#[test]
fn export_json_rejects_invalid_regex_replacement_rules() {
    let (_temp_dir, vault_root) = build_export_replacement_transform_vault();

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "export",
            "json",
            r#"from notes where file.path = "Home.md""#,
            "--replace-rule",
            "regex",
            "(",
            "[redacted]",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "content transform replacement rule 1 has invalid regex pattern",
        ));
}

#[test]
fn export_profile_run_rejects_invalid_regex_replacement_rules() {
    let (_temp_dir, vault_root) = build_export_replacement_transform_vault();
    fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should exist");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        r#"
[export.profiles.public_json]
format = "json"
query = 'from notes where file.path = "Home.md"'
path = "exports/public.json"

[[export.profiles.public_json.content_transforms]]
[[export.profiles.public_json.content_transforms.replace]]
pattern = "("
replacement = "[redacted]"
regex = true
"#,
    )
    .expect("config should be written");

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "export",
            "profile",
            "run",
            "public_json",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "content_transforms rule 1 replace entry 1 in export profile `public_json` has invalid regex pattern",
        ));
}

#[test]
#[allow(clippy::too_many_lines)]
fn export_epub_renders_dynamic_content_tag_indexes_and_tree_nav() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("bases", &vault_root);
    fs::create_dir_all(vault_root.join("Guides/Nested")).expect("guides dir should exist");
    fs::create_dir_all(vault_root.join("People")).expect("people dir should exist");
    fs::create_dir_all(vault_root.join("Projects")).expect("projects dir should exist");

    fs::write(
        vault_root.join("Guides/Intro.md"),
        concat!(
            "---\n",
            "status: draft\n",
            "tags:\n",
            "  - guide\n",
            "  - project\n",
            "---\n",
            "# Intro\n\n",
            "owner:: [[People/Bob]]\n\n",
            "`= this.status`\n\n",
            "See #guide and #project.\n\n",
            "```dataview\n",
            "TABLE status, reviewed\n",
            "FROM \"Projects\"\n",
            "SORT file.name ASC\n",
            "```\n\n",
            "```dataviewjs\n",
            "dv.table([\"Person\"], [[dv.page(\"People/Bob\").file.name]])\n",
            "```\n\n",
            "![[release.base#Release Table]]\n",
        ),
    )
    .expect("Intro.md should be written");
    fs::write(
        vault_root.join("Guides/Nested/Deep.md"),
        concat!(
            "---\n",
            "tags:\n",
            "  - guide/deep\n",
            "---\n",
            "# Deep\n\n",
            "Nested guide links [[Guides/Intro]].\n",
        ),
    )
    .expect("Deep.md should be written");
    fs::write(
        vault_root.join("People/Bob.md"),
        concat!(
            "---\n",
            "tags:\n",
            "  - people/team\n",
            "---\n",
            "# Bob\n\n",
            "Bob is here.\n",
        ),
    )
    .expect("Bob.md should be written");
    fs::write(
        vault_root.join("Projects/Alpha.md"),
        concat!(
            "---\n",
            "status: active\n",
            "reviewed: true\n",
            "---\n",
            "# Alpha\n",
        ),
    )
    .expect("Alpha.md should be written");
    fs::write(
        vault_root.join("Projects/Beta.md"),
        concat!(
            "---\n",
            "status: backlog\n",
            "reviewed: true\n",
            "---\n",
            "# Beta\n",
        ),
    )
    .expect("Beta.md should be written");

    run_scan(&vault_root);

    let export_path = temp_dir.path().join("guides.epub");
    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "export",
            "epub",
            r#"from notes where file.path starts_with "Guides/""#,
            "-o",
            export_path
                .to_str()
                .expect("export path should be valid utf-8"),
            "--title",
            "Guides",
        ])
        .assert()
        .success();

    let file = fs::File::open(&export_path).expect("epub export should exist");
    let mut archive = ZipArchive::new(file).expect("epub export should open");

    let mut nav = String::new();
    archive
        .by_name("OEBPS/nav.xhtml")
        .expect("nav should exist")
        .read_to_string(&mut nav)
        .expect("nav should be readable");
    assert!(!nav.contains("toc-directory-label\">Guides<"));
    assert!(nav.contains("toc-directory-label\">Nested<"));
    assert!(nav.contains("toc-directory-label\">Tags<"));
    assert!(nav.contains("tags/tag-guide.xhtml"));

    let mut intro_chapter = String::new();
    archive
        .by_name("OEBPS/text/chapter-001.xhtml")
        .expect("intro chapter should exist")
        .read_to_string(&mut intro_chapter)
        .expect("intro chapter should be readable");
    assert!(intro_chapter.contains("dataview-inline-field"));
    assert!(intro_chapter.contains(">owner<"));
    assert!(intro_chapter.contains("People/Bob</a>"));
    assert!(!intro_chapter.contains("owner:: [[People/Bob]]"));
    assert!(!intro_chapter.contains("`= this.status`"));
    assert!(intro_chapter.contains(">draft<"));
    assert!(intro_chapter.contains("<table>"));
    assert!(intro_chapter.contains(">active<"));
    assert!(intro_chapter.contains(">backlog<"));
    assert!(intro_chapter.contains(">Person<"));
    assert!(intro_chapter.contains("Columns: Name, Due, note_name"));
    assert!(intro_chapter.contains("href=\"../tags/tag-guide.xhtml\""));
    assert!(intro_chapter.contains("href=\"../tags/tag-project.xhtml\""));

    let mut deep_chapter = String::new();
    archive
        .by_name("OEBPS/text/chapter-002.xhtml")
        .expect("deep chapter should exist")
        .read_to_string(&mut deep_chapter)
        .expect("deep chapter should be readable");
    assert!(deep_chapter.contains("Guides/Nested/Deep.md"));

    let mut guide_tag = String::new();
    archive
        .by_name("OEBPS/tags/tag-guide.xhtml")
        .expect("guide tag page should exist")
        .read_to_string(&mut guide_tag)
        .expect("guide tag page should be readable");
    assert!(guide_tag.contains("Tag #guide"));
    assert!(guide_tag.contains(">Intro</a>"));
}

#[test]
fn export_sqlite_writes_notes_links_tags_and_tasks_tables() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("tasknotes", &vault_root);
    run_scan(&vault_root);
    let export_path = temp_dir.path().join("tasknotes.db");

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "export",
            "sqlite",
            r#"from notes where file.path matches "^TaskNotes/(Tasks|Archive)/""#,
            "-o",
            export_path
                .to_str()
                .expect("export path should be valid utf-8"),
        ])
        .assert()
        .success();

    let connection = Connection::open(&export_path).expect("sqlite export should open");
    let note_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM notes", [], |row| row.get(0))
        .expect("notes table should be queryable");
    let link_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM links", [], |row| row.get(0))
        .expect("links table should be queryable");
    let tag_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM tags", [], |row| row.get(0))
        .expect("tags table should be queryable");
    let task_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM tasks", [], |row| row.get(0))
        .expect("tasks table should be queryable");

    assert!(note_count >= 3);
    assert!(link_count >= 1);
    assert!(tag_count >= 1);
    assert!(task_count >= 2);

    let exported_note: String = connection
        .query_row(
            "SELECT document_path FROM notes WHERE document_path = 'TaskNotes/Tasks/Write Docs.md'",
            [],
            |row| row.get(0),
        )
        .expect("write docs note should be exported");
    assert_eq!(exported_note, "TaskNotes/Tasks/Write Docs.md");
}

#[test]
#[allow(clippy::too_many_lines)]
fn saved_reports_can_be_listed_run_and_batched() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("bases", &vault_root);
    run_scan(&vault_root);

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--fields",
            "document_path,rank",
            "--limit",
            "1",
            "saved",
            "search",
            "weekly-search",
            "release",
            "--description",
            "weekly release hits",
            "--export",
            "jsonl",
            "--export-path",
            "exports/search.jsonl",
        ])
        .assert()
        .success();
    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--fields",
            "document_path,group_value",
            "saved",
            "bases",
            "release-table",
            "release.base",
            "--export",
            "csv",
            "--export-path",
            "exports/release.csv",
        ])
        .assert()
        .success();

    let list_rows = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args([
                "--vault",
                vault_root
                    .to_str()
                    .expect("vault path should be valid utf-8"),
                "--output",
                "json",
                "saved",
                "list",
            ])
            .assert()
            .success();
        parse_stdout_json_lines(&assert)
    };
    assert_eq!(list_rows.len(), 2);
    assert_eq!(list_rows[0]["name"], "release-table");
    assert_eq!(list_rows[1]["name"], "weekly-search");

    let show_json = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args([
                "--vault",
                vault_root
                    .to_str()
                    .expect("vault path should be valid utf-8"),
                "--output",
                "json",
                "saved",
                "show",
                "weekly-search",
            ])
            .assert()
            .success();
        parse_stdout_json(&assert)
    };
    assert_eq!(show_json["name"], "weekly-search");
    assert_eq!(show_json["kind"], "search");

    let run_rows = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args([
                "--vault",
                vault_root
                    .to_str()
                    .expect("vault path should be valid utf-8"),
                "--output",
                "json",
                "saved",
                "run",
                "weekly-search",
            ])
            .assert()
            .success();
        parse_stdout_json_lines(&assert)
    };
    assert_eq!(run_rows.len(), 1);
    assert_eq!(run_rows[0]["document_path"], "Backlog.md");
    assert!(vault_root.join("exports/search.jsonl").exists());

    let batch_json = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args([
                "--vault",
                vault_root
                    .to_str()
                    .expect("vault path should be valid utf-8"),
                "--output",
                "json",
                "batch",
                "--all",
            ])
            .assert()
            .success();
        let mut json = parse_stdout_json(&assert);
        replace_string_recursively(&mut json, &vault_root.display().to_string(), "<vault>");
        // Normalize any remaining backslash path separators (Windows) to forward slashes.
        replace_string_recursively(&mut json, "\\", "/");
        json
    };
    assert_eq!(batch_json["succeeded"], 2);
    assert_eq!(batch_json["failed"], 0);
    assert!(vault_root.join("exports/search.jsonl").exists());
    assert!(vault_root.join("exports/release.csv").exists());
}

#[test]
fn doctor_and_cache_verify_support_issue_exit_codes() {
    let broken_dir = TempDir::new().expect("temp dir should be created");
    let broken_vault = broken_dir.path().join("vault");
    copy_fixture_vault("broken-frontmatter", &broken_vault);
    run_scan(&broken_vault);

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            broken_vault
                .to_str()
                .expect("broken vault path should be valid utf-8"),
            "doctor",
            "--fail-on-issues",
        ])
        .assert()
        .code(2)
        .stdout(predicate::str::contains("parse failures: 1"));

    let cache_dir = TempDir::new().expect("temp dir should be created");
    let cache_vault = cache_dir.path().join("vault");
    copy_fixture_vault("basic", &cache_vault);
    run_scan(&cache_vault);
    let paths = VaultPaths::new(&cache_vault);
    let mut database = CacheDatabase::open(&paths).expect("cache should open");
    database
        .with_transaction(|transaction| {
            transaction
                .execute("DELETE FROM search_chunk_content", [])
                .expect("search rows should delete");
            Ok::<_, vulcan_core::CacheError>(())
        })
        .expect("cache mutation should succeed");

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            cache_vault
                .to_str()
                .expect("cache vault path should be valid utf-8"),
            "cache",
            "verify",
            "--fail-on-errors",
        ])
        .assert()
        .code(2)
        .stdout(predicate::str::contains("Cache healthy: false"));
}

#[test]
fn automation_run_executes_saved_reports_and_health_checks() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--fields",
            "document_path,rank",
            "--limit",
            "1",
            "saved",
            "search",
            "weekly-search",
            "dashboard",
            "--description",
            "weekly dashboard hits",
            "--export",
            "jsonl",
            "--export-path",
            "exports/search.jsonl",
        ])
        .assert()
        .success();

    let automation_json = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args([
                "--vault",
                vault_root
                    .to_str()
                    .expect("vault path should be valid utf-8"),
                "--output",
                "json",
                "automation",
                "run",
                "--scan",
                "--doctor",
                "--verify-cache",
                "weekly-search",
                "--fail-on-issues",
            ])
            .assert()
            .success();
        parse_stdout_json(&assert)
    };

    assert_eq!(
        automation_json["actions"],
        serde_json::json!(["scan", "doctor", "cache_verify", "saved_reports"])
    );
    assert_eq!(automation_json["issues_detected"], false);
    assert_eq!(automation_json["cache_verify"]["healthy"], true);
    assert_eq!(automation_json["reports"]["succeeded"], 1);
    assert!(vault_root.join("exports/search.jsonl").exists());
}

fn replace_string_recursively(value: &mut Value, pattern: &str, replacement: &str) {
    match value {
        Value::Object(object) => {
            for nested in object.values_mut() {
                replace_string_recursively(nested, pattern, replacement);
            }
        }
        Value::Array(values) => {
            for nested in values {
                replace_string_recursively(nested, pattern, replacement);
            }
        }
        Value::String(string) => {
            if string.contains(pattern) {
                *string = string.replace(pattern, replacement);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

#[test]
fn search_json_output_supports_limit_and_offset() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);
    let mut command = Command::cargo_bin("vulcan").expect("binary should build");

    let assert = command
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "--fields",
            "document_path,heading_path",
            "--limit",
            "1",
            "--offset",
            "1",
            "search",
            "Alpha",
        ])
        .assert()
        .success();
    let json_lines = parse_stdout_json_lines(&assert);

    assert_eq!(json_lines.len(), 1);
    assert_eq!(
        json_lines[0],
        serde_json::json!({
            "document_path": "Projects/Alpha.md",
            "heading_path": ["Alpha", "Status"]
        })
    );
}

#[test]
fn search_json_output_matches_snapshot() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);
    let mut command = Command::cargo_bin("vulcan").expect("binary should build");

    let assert = command
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "--fields",
            "document_path,heading_path,query,tag,path_prefix,snippet",
            "search",
            "dashboard",
        ])
        .assert()
        .success();

    assert_json_snapshot_lines(
        "search_basic_dashboard.json",
        &parse_stdout_json_lines(&assert),
    );
}

#[test]
fn vectors_index_and_neighbors_json_output_work_end_to_end() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    let server = MockEmbeddingServer::spawn();
    write_embedding_config(&vault_root, &server.base_url());
    run_scan(&vault_root);

    let mut index_command = Command::cargo_bin("vulcan").expect("binary should build");
    let index_assert = index_command
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "vectors",
            "index",
        ])
        .assert()
        .success();
    let index_json = parse_stdout_json(&index_assert);

    assert_eq!(index_json["indexed"], 4);
    assert_eq!(index_json["failed"], 0);

    let mut neighbors_command = Command::cargo_bin("vulcan").expect("binary should build");
    let neighbors_assert = neighbors_command
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "--fields",
            "document_path,distance",
            "--limit",
            "1",
            "vectors",
            "neighbors",
            "dashboard",
        ])
        .assert()
        .success();
    let neighbor_rows = parse_stdout_json_lines(&neighbors_assert);

    assert_eq!(neighbor_rows.len(), 1);
    assert_eq!(neighbor_rows[0]["document_path"], "Home.md");
    server.shutdown();
}

#[test]
fn search_human_output_is_multi_line_and_readable() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "search",
            "dashboard",
        ])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("1. Home.md > Home")
                .and(predicate::str::contains("\n   Rank: "))
                .and(predicate::str::contains("\n   Snippet: Home"))
                .and(predicate::str::contains(
                    "The [dashboard] note uses the tag #index.",
                )),
        );
}

#[test]
fn search_hybrid_json_output_combines_vector_and_keyword_results() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    let server = MockEmbeddingServer::spawn();
    write_embedding_config(&vault_root, &server.base_url());
    run_scan(&vault_root);
    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "vectors",
            "index",
        ])
        .assert()
        .success();

    let mut command = Command::cargo_bin("vulcan").expect("binary should build");
    let assert = command
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "--fields",
            "document_path,mode",
            "--limit",
            "2",
            "search",
            "dashboard",
            "--mode",
            "hybrid",
        ])
        .assert()
        .success();
    let rows = parse_stdout_json_lines(&assert);

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["mode"], "hybrid");
    assert_eq!(rows[0]["document_path"], "Home.md");
    server.shutdown();
}

#[test]
fn vectors_duplicates_and_cluster_json_output_work() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    let server = MockEmbeddingServer::spawn();
    write_embedding_config(&vault_root, &server.base_url());
    run_scan(&vault_root);
    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "vectors",
            "index",
        ])
        .assert()
        .success();

    let duplicates_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "--fields",
            "left_document_path,right_document_path,similarity",
            "vectors",
            "duplicates",
            "--threshold",
            "0.7",
        ])
        .assert()
        .success();
    let duplicate_rows = parse_stdout_json_lines(&duplicates_assert);

    assert!(!duplicate_rows.is_empty());
    assert!(
        duplicate_rows[0]["similarity"]
            .as_f64()
            .expect("similarity should be numeric")
            >= 0.7
    );

    let cluster_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "--fields",
            "cluster_id,cluster_label,keywords,chunk_count,document_count",
            "cluster",
            "--clusters",
            "2",
        ])
        .assert()
        .success();
    let cluster_rows = parse_stdout_json_lines(&cluster_assert);

    assert_eq!(cluster_rows.len(), 2);
    assert!(cluster_rows[0]["chunk_count"].as_u64().unwrap_or_default() >= 1);
    assert!(!cluster_rows[0]["cluster_label"]
        .as_str()
        .expect("cluster label should be a string")
        .is_empty());
    assert!(!cluster_rows[0]["keywords"]
        .as_array()
        .expect("keywords should be an array")
        .is_empty());
    server.shutdown();
}

#[test]
fn vectors_repair_queue_and_related_json_output_work() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    let server = MockEmbeddingServer::spawn();
    write_embedding_config(&vault_root, &server.base_url());
    run_scan(&vault_root);
    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "vectors",
            "index",
        ])
        .assert()
        .success();
    fs::write(
        vault_root.join("Home.md"),
        "---\naliases:\n  - Start\ntags:\n  - dashboard\n---\n\n# Home\n\nUpdated dashboard plans.\n",
    )
    .expect("updated note should write");
    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "scan",
        ])
        .assert()
        .success();

    let repair_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "vectors",
            "repair",
            "--dry-run",
        ])
        .assert()
        .success();
    let repair_json = parse_stdout_json(&repair_assert);
    assert_eq!(repair_json["pending_chunks"], 1);

    let queue_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "vectors",
            "queue",
            "status",
        ])
        .assert()
        .success();
    let queue_json = parse_stdout_json(&queue_assert);
    assert_eq!(queue_json["pending_chunks"], 1);

    let related_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "--fields",
            "document_path,similarity,matched_chunks",
            "related",
            "Home",
        ])
        .assert()
        .success();
    let related_rows = parse_stdout_json_lines(&related_assert);
    assert!(!related_rows.is_empty());
    assert_ne!(related_rows[0]["document_path"], "Home.md");
    server.shutdown();
}

#[test]
fn scan_human_output_reports_progress_on_stderr() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "scan",
            "--full",
        ])
        .assert()
        .success()
        .stderr(
            predicate::str::contains("Discovered 3 files; running full scan...")
                .and(predicate::str::contains("Scanned 3/3 files"))
                .and(predicate::str::contains("Resolving links...")),
        );
}

#[test]
fn vectors_index_human_output_reports_progress_and_throughput_settings() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    let server = MockEmbeddingServer::spawn();
    write_embedding_config(&vault_root, &server.base_url());
    run_scan(&vault_root);

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "vectors",
            "index",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("batch size 8, concurrency 1"))
        .stderr(
            predicate::str::contains("Indexing 4 vector chunks with openai-compatible:fixture")
                .and(predicate::str::contains("Completed batch 1/1")),
        );

    server.shutdown();
}

#[test]
fn scan_json_output_matches_snapshot() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    let mut command = Command::cargo_bin("vulcan").expect("binary should build");

    let assert = command
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "scan",
            "--full",
        ])
        .assert()
        .success();

    assert_json_snapshot("scan_basic_full.json", &parse_stdout_json(&assert));
}

#[test]
fn doctor_json_output_matches_snapshot() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("broken-frontmatter", &vault_root);
    run_scan(&vault_root);
    let mut command = Command::cargo_bin("vulcan").expect("binary should build");

    let assert = command
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "doctor",
        ])
        .assert()
        .success();

    assert_json_snapshot(
        "doctor_broken_frontmatter.json",
        &parse_stdout_json(&assert),
    );
}

#[test]
fn move_json_output_supports_dry_run_and_apply() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("move-rewrite", &vault_root);
    run_scan(&vault_root);
    let mut dry_run_command = Command::cargo_bin("vulcan").expect("binary should build");

    let dry_run = dry_run_command
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "move",
            "Projects/Alpha.md",
            "Archive/Alpha.md",
            "--dry-run",
        ])
        .assert()
        .success();
    let dry_run_json = parse_stdout_json(&dry_run);

    assert_eq!(dry_run_json["dry_run"], true);
    assert_eq!(dry_run_json["destination_path"], "Archive/Alpha.md");
    assert!(vault_root.join("Projects/Alpha.md").exists());

    let mut move_command = Command::cargo_bin("vulcan").expect("binary should build");
    let applied = move_command
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "move",
            "Projects/Alpha.md",
            "Archive/Alpha.md",
        ])
        .assert()
        .success();
    let applied_json = parse_stdout_json(&applied);

    assert_eq!(applied_json["dry_run"], false);
    assert!(vault_root.join("Archive/Alpha.md").exists());
    assert!(fs::read_to_string(vault_root.join("Home.md"))
        .expect("home should be readable")
        .contains("[[Archive/Alpha#Status]]"));
}

#[test]
fn note_rename_json_output_renames_in_place_and_rewrites_links() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("move-rewrite", &vault_root);
    run_scan(&vault_root);
    let vault_root_str = vault_root
        .to_str()
        .expect("vault path should be valid utf-8")
        .to_string();

    let dry_run = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "--output",
            "json",
            "note",
            "rename",
            "Alpha",
            "Beta",
            "--dry-run",
        ])
        .assert()
        .success();
    let dry_run_json = parse_stdout_json(&dry_run);

    assert_eq!(dry_run_json["dry_run"], true);
    assert_eq!(dry_run_json["source_path"], "Projects/Alpha.md");
    assert_eq!(dry_run_json["destination_path"], "Projects/Beta.md");
    assert!(vault_root.join("Projects/Alpha.md").exists());

    let applied = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "--output",
            "json",
            "note",
            "rename",
            "Alpha",
            "Beta",
        ])
        .assert()
        .success();
    let applied_json = parse_stdout_json(&applied);

    assert_eq!(applied_json["dry_run"], false);
    assert_eq!(applied_json["destination_path"], "Projects/Beta.md");
    assert!(!vault_root.join("Projects/Alpha.md").exists());
    assert!(vault_root.join("Projects/Beta.md").exists());
    assert!(fs::read_to_string(vault_root.join("Home.md"))
        .expect("home should be readable")
        .contains("[[Projects/Beta#Status]]"));
}

#[test]
fn note_delete_json_output_reports_dangling_backlinks_and_removes_file() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("move-rewrite", &vault_root);
    run_scan(&vault_root);
    let vault_root_str = vault_root
        .to_str()
        .expect("vault path should be valid utf-8")
        .to_string();

    let dry_run = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "--output",
            "json",
            "note",
            "delete",
            "Alpha",
            "--dry-run",
        ])
        .assert()
        .success();
    let dry_run_json = parse_stdout_json(&dry_run);

    assert_eq!(dry_run_json["dry_run"], true);
    assert_eq!(dry_run_json["deleted"], false);
    assert_eq!(dry_run_json["path"], "Projects/Alpha.md");
    assert!(
        dry_run_json["backlink_count"]
            .as_u64()
            .expect("backlink count should be numeric")
            >= 2
    );
    let dry_run_sources = dry_run_json["backlinks"]
        .as_array()
        .expect("backlinks should be an array")
        .iter()
        .filter_map(|item| item["source_path"].as_str())
        .collect::<Vec<_>>();
    assert!(dry_run_sources.contains(&"Home.md"));
    assert!(dry_run_sources.contains(&"People/Bob.md"));
    assert!(vault_root.join("Projects/Alpha.md").exists());

    let applied = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "--output",
            "json",
            "note",
            "delete",
            "Alpha",
        ])
        .assert()
        .success();
    let applied_json = parse_stdout_json(&applied);

    assert_eq!(applied_json["dry_run"], false);
    assert_eq!(applied_json["deleted"], true);
    assert!(
        applied_json["backlink_count"]
            .as_u64()
            .expect("backlink count should be numeric")
            >= 2
    );
    assert!(!vault_root.join("Projects/Alpha.md").exists());

    let doctor = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["--vault", &vault_root_str, "--output", "json", "doctor"])
        .assert()
        .success();
    let doctor_json = parse_stdout_json(&doctor);
    assert!(
        doctor_json["summary"]["unresolved_links"]
            .as_u64()
            .expect("doctor summary should include unresolved link count")
            >= 1
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn suggest_and_rewrite_json_outputs_cover_linking_and_duplicates() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("suggestions", &vault_root);
    run_scan(&vault_root);
    let vault_root_str = vault_root
        .to_str()
        .expect("vault path should be valid utf-8")
        .to_string();

    let mentions_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "--output",
            "json",
            "--fields",
            "source_path,matched_text,target_path,candidate_count,status",
            "suggest",
            "mentions",
            "Home",
        ])
        .assert()
        .success();
    let mention_rows = parse_stdout_json_lines(&mentions_assert);
    assert!(mention_rows.iter().any(|row| {
        row["matched_text"] == "Bob"
            && row["target_path"] == "People/Bob.md"
            && row["status"] == "unambiguous"
    }));
    assert!(mention_rows.iter().any(|row| {
        row["matched_text"] == "Alpha"
            && row["target_path"].is_null()
            && row["candidate_count"] == 2
            && row["status"] == "ambiguous"
    }));

    let duplicates_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "--output",
            "json",
            "--fields",
            "kind,value,paths,left_path,right_path,score",
            "suggest",
            "duplicates",
        ])
        .assert()
        .success();
    let duplicate_rows = parse_stdout_json_lines(&duplicates_assert);
    assert!(duplicate_rows
        .iter()
        .any(|row| row["kind"] == "duplicate_title" && row["value"] == "Alpha"));
    assert!(duplicate_rows
        .iter()
        .any(|row| row["kind"] == "alias_collision" && row["value"] == "Guide"));
    assert!(duplicate_rows.iter().any(|row| {
        row["kind"] == "merge_candidate"
            && row["left_path"] == "Archive/Alpha.md"
            && row["right_path"] == "Projects/Alpha.md"
    }));

    let link_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "--output",
            "json",
            "link-mentions",
            "Home",
            "--dry-run",
        ])
        .assert()
        .success();
    let link_json = parse_stdout_json(&link_assert);
    assert_eq!(link_json["action"], "link_mentions");
    assert_eq!(link_json["dry_run"], true);
    assert_eq!(link_json["files"][0]["path"], "Home.md");

    let rewrite_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "--output",
            "json",
            "rewrite",
            "--find",
            "Guide",
            "--replace",
            "Manual",
            "--dry-run",
        ])
        .assert()
        .success();
    let rewrite_json = parse_stdout_json(&rewrite_assert);
    assert_eq!(rewrite_json["action"], "bulk_replace");
    assert_eq!(rewrite_json["dry_run"], true);
    assert!(rewrite_json["files"]
        .as_array()
        .expect("files should be an array")
        .iter()
        .any(|file| file["path"] == "Home.md"));
}

#[test]
fn rebuild_and_repair_json_output_support_dry_run() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);

    let rebuild_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "rebuild",
            "--dry-run",
        ])
        .assert()
        .success();
    let rebuild_json = parse_stdout_json(&rebuild_assert);
    assert_eq!(rebuild_json["dry_run"], true);
    assert_eq!(rebuild_json["discovered"], 3);

    let repair_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "repair",
            "fts",
            "--dry-run",
        ])
        .assert()
        .success();
    let repair_json = parse_stdout_json(&repair_assert);
    assert_eq!(repair_json["dry_run"], true);
    assert_eq!(repair_json["indexed_documents"], 3);
    assert_eq!(repair_json["indexed_chunks"], 4);
}

#[test]
fn describe_json_output_exposes_runtime_command_schema() {
    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["--output", "json", "describe"])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["name"], "vulcan");
    assert!(json["after_help"]
        .as_str()
        .expect("after_help should be a string")
        .contains("vulcan help <command>"));
    assert!(json["commands"]
        .as_array()
        .expect("commands should be an array")
        .iter()
        .any(|command| command["name"] == "index"));
    assert!(json["commands"]
        .as_array()
        .expect("commands should be an array")
        .iter()
        .any(|command| command["name"] == "edit"));
    assert!(json["commands"]
        .as_array()
        .expect("commands should be an array")
        .iter()
        .any(|command| command["name"] == "browse"));
    assert!(json["commands"]
        .as_array()
        .expect("commands should be an array")
        .iter()
        .any(|command| command["name"] == "help"));
    assert!(json["commands"]
        .as_array()
        .expect("commands should be an array")
        .iter()
        .any(|command| command["name"] == "run"));
    assert!(json["commands"]
        .as_array()
        .expect("commands should be an array")
        .iter()
        .any(|command| command["name"] == "note"));
    assert!(json["commands"]
        .as_array()
        .expect("commands should be an array")
        .iter()
        .any(|command| command["name"] == "tasks"));
    assert!(json["commands"]
        .as_array()
        .expect("commands should be an array")
        .iter()
        .find(|command| command["name"] == "browse")
        .and_then(|command| command["after_help"].as_str())
        .expect("browse after_help should be present")
        .contains("Browse modes:"));
    assert!(json["commands"]
        .as_array()
        .expect("commands should be an array")
        .iter()
        .find(|command| command["name"] == "template")
        .and_then(|command| command["after_help"].as_str())
        .expect("template after_help should be present")
        .contains("Template source:"));
    assert!(json["commands"]
        .as_array()
        .expect("commands should be an array")
        .iter()
        .find(|command| command["name"] == "note")
        .and_then(|command| command["after_help"].as_str())
        .expect("note after_help should be present")
        .contains("Subcommands:"));
    assert!(json["commands"]
        .as_array()
        .expect("commands should be an array")
        .iter()
        .find(|command| command["name"] == "query")
        .and_then(|command| command["after_help"].as_str())
        .expect("query after_help should be present")
        .contains("Shortcuts:"));
    assert!(json["commands"]
        .as_array()
        .expect("commands should be an array")
        .iter()
        .all(|command| command["name"] != "repair"));
}

#[test]
fn help_json_output_returns_structured_topic_docs() {
    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["--output", "json", "help", "query"])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["name"], "query");
    assert_eq!(json["kind"], "command");
    assert!(json["summary"]
        .as_str()
        .expect("summary should be present")
        .contains("Run a Vulcan query"));
    assert!(json["body"]
        .as_str()
        .expect("body should be present")
        .contains("Query DSL syntax:"));

    let scripting_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["--output", "json", "help", "scripting"])
        .assert()
        .success();
    let scripting_json = parse_stdout_json(&scripting_assert);
    assert_eq!(scripting_json["kind"], "concept");
    assert!(scripting_json["body"]
        .as_str()
        .expect("body should be present")
        .contains("DataviewJS evaluation"));

    let js_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["--output", "json", "help", "js.vault"])
        .assert()
        .success();
    let js_json = parse_stdout_json(&js_assert);
    assert_eq!(js_json["name"], "js.vault");
    assert!(js_json["body"]
        .as_str()
        .expect("body should be present")
        .contains("vault.daily.today()"));

    let note_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["--output", "json", "help", "note"])
        .assert()
        .success();
    let note_json = parse_stdout_json(&note_assert);
    assert!(note_json["body"]
        .as_str()
        .expect("body should be present")
        .contains("## Subcommands"));
    assert!(note_json["body"]
        .as_str()
        .expect("body should be present")
        .contains("e.g. vulcan note get Dashboard"));

    let overview_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["--output", "json", "help"])
        .assert()
        .success();
    let overview_json = parse_stdout_json(&overview_assert);
    let overview_body = overview_json["body"]
        .as_str()
        .expect("body should be present");
    assert!(overview_body.contains("## Notes"));
    assert!(overview_body.contains("`vulcan describe` exports the same command surface"));
    assert!(!overview_body.contains("## Command Tree"));
}

#[test]
fn help_human_output_uses_grouped_overview_and_parent_subcommand_examples() {
    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["help"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("Notes")
                .and(predicate::str::contains(
                    "- `note get` — Open a note, resolve its path, or print frontmatter",
                ))
                .and(predicate::str::contains(
                    "`vulcan describe` exports the same command surface",
                ))
                .and(predicate::str::contains("Command Tree").not()),
        );

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["help", "note"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("Subcommands")
                .and(predicate::str::contains("e.g. vulcan note get Dashboard"))
                .and(predicate::str::contains("- `note get`").not()),
        );
}

#[test]
fn describe_is_hidden_from_root_help_but_still_has_direct_help() {
    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .arg("--help")
        .assert()
        .success()
        .stdout(
            predicate::str::contains("Machine-readable schema: vulcan describe")
                .and(predicate::str::contains("Describe the CLI schema and command surface").not()),
        );

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Machine-readable command schema").not());

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["describe"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("Machine-readable Vulcan tool schema")
                .and(predicate::str::contains("`vulcan --output json describe`"))
                .and(predicate::str::contains(
                    "`vulcan help` or `vulcan help <command>`",
                ))
                .and(predicate::str::contains("Commands:").not())
                .and(predicate::str::contains("- index:").not()),
        );

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["describe", "--help"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("Describe the CLI schema and command surface").and(
                predicate::str::contains("vulcan --output json describe > vulcan-schema.json"),
            ),
        );
}

#[test]
fn saved_and_automation_help_document_end_to_end_workflow() {
    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["saved", "--help"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("Saved report definitions live under .vulcan/reports.")
                .and(predicate::str::contains(
                    "vulcan saved create search weekly dashboard --where 'reviewed = true' --description 'weekly dashboard'",
                ))
                .and(predicate::str::contains("vulcan saved list"))
                .and(predicate::str::contains("vulcan saved delete weekly"))
                .and(predicate::str::contains(
                    "vulcan saved run weekly --export jsonl --export-path exports/weekly.jsonl",
                ))
                .and(predicate::str::contains(
                    "vulcan automation run weekly --scan --doctor",
                )),
        );

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["automation", "--help"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("`automation run` is intended for CI, cron jobs")
                .and(predicate::str::contains("vulcan automation list"))
                .and(predicate::str::contains("--all / --all-reports"))
                .and(predicate::str::contains("--fail-on-issues"))
                .and(predicate::str::contains(
                    "vulcan automation run --all --verify-cache --repair-fts --fail-on-issues",
                )),
        );
}

#[test]
fn run_json_output_executes_script_files_and_named_scripts() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("dataview", &vault_root);
    run_scan(&vault_root);

    fs::create_dir_all(vault_root.join(".vulcan/scripts")).expect("scripts dir should exist");
    fs::write(
        vault_root.join(".vulcan/scripts/runtime-demo.js"),
        concat!(
            "#!/usr/bin/env -S vulcan run --script\n",
            "console.log(help(vault.search));\n",
            "({ note: vault.note(\"Projects/Alpha\").file.name, hits: vault.search(\"Alpha\", { limit: 1 }).hits.length });\n",
        ),
    )
    .expect("named script should be written");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "run",
            "runtime-demo",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["value"]["note"], "Alpha");
    assert_eq!(json["value"]["hits"], 1);
    let help_text = json["outputs"][0]["text"]
        .as_str()
        .expect("help text should be rendered");
    assert!(
        help_text.contains("vault.search(query: string, opts?: { limit?: number }): SearchReport")
    );
    assert!(help_text.contains("Parameters:"));
    assert!(help_text.contains("Example:"));
    assert!(help_text.contains("See also: vault.notes(), vault.query()"));
}

#[test]
fn run_json_output_reports_timeout_failures() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("dataview", &vault_root);
    run_scan(&vault_root);

    let script_path = vault_root.join("runtime-timeout.js");
    fs::write(&script_path, "while (true) {}\n").expect("timeout script should be written");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "run",
            script_path
                .to_str()
                .expect("script path should be valid utf-8"),
            "--timeout",
            "200ms",
        ])
        .assert()
        .failure();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["code"], "operation_failed");
    assert!(json["error"]
        .as_str()
        .expect("error should be present")
        .contains("timed out after 200 ms"));
}

#[test]
fn run_json_output_enforces_sandbox_levels_and_supports_configured_script_roots() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("dataview", &vault_root);
    run_scan(&vault_root);

    fs::create_dir_all(vault_root.join(".vulcan")).expect("config dir should exist");
    fs::create_dir_all(vault_root.join("Runtime/Scripts")).expect("scripts dir should exist");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        "[js_runtime]\ndefault_sandbox = \"fs\"\nscripts_folder = \"Runtime/Scripts\"\n",
    )
    .expect("config should be written");
    fs::write(
        vault_root.join("Runtime/Scripts/mutate.js"),
        r##"
        const created = vault.transaction((tx) => {
          const note = tx.create("Scratch", { content: "# Scratch\n\n## Log\n" });
          tx.append("Scratch", "Follow-up", { heading: "Log" });
          tx.update("Scratch", "status", "draft");
          tx.unset("Scratch", "status");
          return note;
        });
        ({ path: created.path, headings: vault.note("Scratch").headings.length, hasStatus: vault.note("Scratch").frontmatter.status !== undefined });
        "##,
    )
    .expect("script should be written");

    let strict_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "run",
            "mutate",
            "--sandbox",
            "strict",
        ])
        .assert()
        .failure();
    let strict_json = parse_stdout_json(&strict_assert);
    assert_eq!(strict_json["code"], "operation_failed");
    assert!(strict_json["error"]
        .as_str()
        .is_some_and(|message| message.contains("requires --sandbox fs or higher")));

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "run",
            "mutate",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["value"]["path"], "Scratch.md");
    assert_eq!(json["value"]["headings"], 2);
    assert_eq!(json["value"]["hasStatus"], false);

    let scratch =
        fs::read_to_string(vault_root.join("Scratch.md")).expect("scratch note should exist");
    assert!(scratch.contains("## Log"));
    assert!(scratch.contains("Follow-up"));
    assert!(!scratch.contains("status: draft"));
}

#[test]
fn run_json_output_net_sandbox_exposes_web_helpers() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("dataview", &vault_root);
    run_scan(&vault_root);

    let listener = TcpListener::bind("127.0.0.1:0").expect("listener should bind");
    let address = listener
        .local_addr()
        .expect("listener should have a local address");
    let base_url = format!("http://{address}");
    fs::create_dir_all(vault_root.join(".vulcan")).expect("config dir should exist");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        format!(
            "[web.search]\nbackend = \"kagi\"\napi_key_env = \"VULCAN_JS_TEST_KAGI_KEY\"\nbase_url = \"{base_url}/search\"\n"
        ),
    )
    .expect("config should be written");
    std::env::set_var("VULCAN_JS_TEST_KAGI_KEY", "test-key");

    let handle = thread::spawn(move || {
        for _ in 0..3 {
            let (mut stream, _) = listener.accept().expect("connection should be accepted");
            let mut buffer = [0_u8; 4096];
            let read = stream
                .read(&mut buffer)
                .expect("request should be readable");
            let request = String::from_utf8_lossy(&buffer[..read]);
            let path = request
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .unwrap_or("/");
            let (content_type, body) = if path.starts_with("/search") {
                (
                    "application/json",
                    r#"{"meta":{"id":"test"},"data":[{"t":"Alpha","url":"http://example.com/alpha","snippet":"Alpha snippet"}]}"#,
                )
            } else if path == "/robots.txt" {
                ("text/plain", "User-agent: *\nAllow: /\n")
            } else {
                (
                    "text/html",
                    "<html><body><main><h1>Alpha page</h1><p>Fetched content.</p></main></body></html>",
                )
            };
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                content_type,
                body
            );
            stream
                .write_all(response.as_bytes())
                .expect("response should be written");
        }
    });

    let script_path = vault_root.join("runtime-web.js");
    fs::write(
        &script_path,
        format!(
            r#"
            const search = web.search("Alpha", {{ limit: 1 }});
            const fetched = web.fetch("{base_url}/article", {{ mode: "markdown" }});
            ({{
              title: search.results[0].title,
              status: fetched.status,
              containsAlpha: fetched.content.includes("Alpha page")
            }});
            "#
        ),
    )
    .expect("script should be written");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "run",
            script_path
                .to_str()
                .expect("script path should be valid utf-8"),
            "--sandbox",
            "net",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    handle.join().expect("server thread should finish");

    assert_eq!(json["value"]["title"], "Alpha");
    assert_eq!(json["value"]["status"], 200);
    assert_eq!(json["value"]["containsAlpha"], true);
}

#[test]
fn web_cli_and_js_entrypoints_share_normalized_reports() {
    let server = MockWebServer::spawn();
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("dataview", &vault_root);
    run_scan(&vault_root);

    fs::create_dir_all(vault_root.join(".vulcan")).expect("config dir should exist");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        format!(
            "[web.search]\nbackend = \"duckduckgo\"\nbase_url = \"{}\"\n",
            server.url("/html/")
        ),
    )
    .expect("config should be written");

    let cli_search_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "web",
            "search",
            "Alpha",
            "--limit",
            "2",
        ])
        .assert()
        .success();
    let cli_search = parse_stdout_json(&cli_search_assert);

    let cli_fetch_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "web",
            "fetch",
            &server.url("/article"),
            "--mode",
            "markdown",
        ])
        .assert()
        .success();
    let cli_fetch = parse_stdout_json(&cli_fetch_assert);

    let js_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "run",
            "--sandbox",
            "net",
            "-e",
            &format!(
                r#"({{
                    search: web.search("Alpha", {{ limit: 2 }}),
                    fetched: web.fetch("{}", {{ mode: "markdown" }})
                }})"#,
                server.url("/article")
            ),
        ])
        .assert()
        .success();
    let js = parse_stdout_json(&js_assert);

    server.shutdown();

    assert_eq!(js["value"]["search"], cli_search);
    assert_eq!(js["value"]["fetched"], cli_fetch);
}

#[test]
fn describe_openai_and_mcp_formats_export_tool_definitions() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join(".agents/tools/summarize")).expect("tool dir should exist");
    initialize_vulcan_dir(&vault_root);
    fs::write(
        vault_root.join(".agents/tools/summarize/TOOL.md"),
        r"---
name: summarize_tool
description: Summarize one note.
input_schema:
  type: object
  additionalProperties: false
  properties:
    note:
      type: string
  required:
    - note
---
",
    )
    .expect("manifest should write");
    fs::write(
        vault_root.join(".agents/tools/summarize/main.js"),
        "function main(input) {\n  return { note: input.note };\n}\n",
    )
    .expect("entrypoint should write");
    let config_home = temp_dir.path().join("config");
    fs::create_dir_all(&config_home).expect("config home should exist");
    let vault_root_str = vault_root.to_str().expect("utf-8").to_string();
    let config_home_str = config_home.to_str().expect("utf-8").to_string();
    trust_and_scan_vault(&config_home_str, &vault_root_str);

    let openai_assert = cargo_vulcan_with_xdg_config(&config_home_str)
        .args([
            "--vault",
            &vault_root_str,
            "--output",
            "json",
            "describe",
            "--format",
            "openai-tools",
        ])
        .assert()
        .success();
    let openai_json = parse_stdout_json(&openai_assert);
    let openai_tools = openai_json["tools"]
        .as_array()
        .expect("tools should be an array");
    assert!(openai_tools
        .iter()
        .any(|tool| tool["function"]["name"] == "query"));
    assert!(openai_tools
        .iter()
        .any(|tool| tool["function"]["name"] == "note_get"));
    assert!(openai_tools
        .iter()
        .any(|tool| tool["function"]["name"] == "summarize_tool"));

    let mcp_assert = cargo_vulcan_with_xdg_config(&config_home_str)
        .args([
            "--vault",
            &vault_root_str,
            "--output",
            "json",
            "describe",
            "--format",
            "mcp",
            "--tool-pack",
            "search,custom",
        ])
        .assert()
        .success();
    let mcp_json = parse_stdout_json(&mcp_assert);
    let mcp_tools = mcp_json["tools"]
        .as_array()
        .expect("tools should be an array");
    assert!(mcp_tools.iter().any(|tool| tool["name"] == "search"));
    assert!(mcp_tools
        .iter()
        .any(|tool| tool["inputSchema"]["type"] == "object"));
    assert!(mcp_tools.iter().any(|tool| tool["name"] == "summarize_tool"
        && tool["toolPacks"] == serde_json::json!(["custom"])));
}

#[test]
fn completions_command_emits_shell_script() {
    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["completions", "bash"])
        .assert()
        .success()
        .stdout(predicate::str::contains("vulcan").and(predicate::str::contains("complete")));
}

#[test]
fn browse_requires_interactive_terminal() {
    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["browse"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "browse requires an interactive terminal",
        ));
}

#[test]
fn fish_completions_command_emits_shell_script() {
    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["completions", "fish"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("complete -c vulcan")
                .and(predicate::str::contains("Search indexed note content")),
        );
}

#[test]
fn query_command_dsl_returns_matching_notes() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("mixed-properties", &vault_root);
    run_scan(&vault_root);

    // DSL query: status = backlog
    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "--fields",
            "document_path",
            "query",
            "from notes where status = backlog",
        ])
        .assert()
        .success();
    let rows = parse_stdout_json_lines(&assert);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["document_path"], "Backlog.md");
}

#[test]
fn query_command_json_payload_returns_matching_notes() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("mixed-properties", &vault_root);
    run_scan(&vault_root);

    let json_payload =
        r#"{"source":"notes","predicates":[{"field":"status","operator":"eq","value":"done"}]}"#;

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "--fields",
            "document_path",
            "query",
            "--json",
            json_payload,
        ])
        .assert()
        .success();
    let rows = parse_stdout_json_lines(&assert);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["document_path"], "Done.md");
}

#[test]
fn query_command_explain_includes_ast_in_json() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("mixed-properties", &vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "query",
            "--explain",
            "from notes where status = done",
        ])
        .assert()
        .success();
    let result = parse_stdout_json(&assert);
    assert!(
        result.get("query").is_some(),
        "explain output should include query AST"
    );
    assert!(
        result.get("notes").is_some(),
        "explain output should include notes"
    );
    assert_eq!(result["query"]["source"], "notes");
}

#[test]
fn query_command_dsl_order_and_limit() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("mixed-properties", &vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "--fields",
            "document_path",
            "query",
            "from notes order by file.path limit 1",
        ])
        .assert()
        .success();
    let rows = parse_stdout_json_lines(&assert);
    assert_eq!(rows.len(), 1, "limit 1 should return exactly one note");
}

#[test]
fn query_command_rejects_both_dsl_and_json() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("mixed-properties", &vault_root);
    run_scan(&vault_root);

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "query",
            "from notes",
            "--json",
            r#"{"source":"notes"}"#,
        ])
        .assert()
        .failure();
}

#[test]
fn query_command_lists_available_fields_with_examples() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("mixed-properties", &vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "query",
            "--list-fields",
        ])
        .assert()
        .success();
    let rows = parse_stdout_json_lines(&assert);

    let path_field = rows
        .iter()
        .find(|row| row["field"] == "file.path")
        .expect("file.path field should be present");
    assert_eq!(path_field["kind"], "builtin");
    assert_eq!(
        path_field["supports"],
        serde_json::json!(["where", "sort", "fields"])
    );
    assert_eq!(path_field["types"], serde_json::json!(["text"]));
    assert_eq!(path_field["example"], "Backlog.md");

    let status_field = rows
        .iter()
        .find(|row| row["field"] == "status")
        .expect("status field should be present");
    assert_eq!(status_field["kind"], "property");
    assert_eq!(
        status_field["supports"],
        serde_json::json!(["where", "sort", "fields"])
    );
    assert_eq!(status_field["types"], serde_json::json!(["list", "text"]));
    assert_eq!(status_field["example"], "backlog");
}

#[test]
fn notes_command_fields_support_property_keys_and_file_aliases() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("mixed-properties", &vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "--fields",
            "file.path,status",
            "notes",
            "--sort",
            "file.path",
        ])
        .assert()
        .success();
    let rows = parse_stdout_json_lines(&assert);

    assert_eq!(
        rows.first().expect("notes output should include rows"),
        &serde_json::json!({
            "file.path": "Backlog.md",
            "status": "backlog"
        })
    );
}

#[test]
fn query_command_results_match_notes_command() {
    // Prove equivalence: query DSL and notes --where produce identical results
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("mixed-properties", &vault_root);
    run_scan(&vault_root);

    let query_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "--fields",
            "document_path",
            "query",
            "from notes where status = backlog",
        ])
        .assert()
        .success();
    let query_paths: Vec<String> = parse_stdout_json_lines(&query_assert)
        .into_iter()
        .filter_map(|v| v["document_path"].as_str().map(str::to_string))
        .collect();

    let notes_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "--fields",
            "document_path",
            "notes",
            "--where",
            "status = backlog",
        ])
        .assert()
        .success();
    let notes_paths: Vec<String> = parse_stdout_json_lines(&notes_assert)
        .into_iter()
        .filter_map(|v| v["document_path"].as_str().map(str::to_string))
        .collect();

    assert_eq!(
        query_paths, notes_paths,
        "query DSL and notes --where should return identical results"
    );
}

#[test]
fn query_command_matches_operator_filters_notes() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("mixed-properties", &vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "query",
            "--format",
            "paths",
            "from notes where file.name matches \"^D\"",
        ])
        .assert()
        .success();
    let rows = parse_stdout_json_lines(&assert);

    assert_eq!(rows, vec![Value::String("Done.md".to_string())]);
}

#[test]
fn ls_command_supports_glob_and_count_format() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("mixed-properties", &vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "ls",
            "--glob",
            "D*",
            "--format",
            "count",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["count"], Value::Number(1.into()));
}

#[test]
fn search_regex_flag_runs_explicit_regex_queries() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("mixed-properties", &vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "--fields",
            "document_path,matched_line",
            "search",
            "--regex",
            "release\\s+readiness",
        ])
        .assert()
        .success();
    let rows = parse_stdout_json_lines(&assert);

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["document_path"], "Done.md");
    assert!(rows[0]["matched_line"].as_u64().is_some());
}

#[test]
fn update_command_sets_property_on_matching_notes() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("mixed-properties", &vault_root);
    run_scan(&vault_root);

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "update",
            "--where",
            "status = backlog",
            "--key",
            "reviewed",
            "--value",
            "true",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Applied"));

    let backlog_content =
        fs::read_to_string(vault_root.join("Backlog.md")).expect("Backlog.md should be readable");
    assert!(
        backlog_content.contains("reviewed: true"),
        "backlog note should have reviewed: true after update"
    );

    let done_content =
        fs::read_to_string(vault_root.join("Done.md")).expect("Done.md should be readable");
    assert!(
        done_content.contains("reviewed: true"),
        "done note should be unchanged (already true)"
    );
}

#[test]
fn note_update_command_reads_note_paths_from_stdin() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("mixed-properties", &vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .write_stdin("Backlog.md\nMixed.md\nBacklog.md\n")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "note",
            "update",
            "--stdin",
            "--key",
            "status",
            "--value",
            "done",
            "--no-commit",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);
    assert_eq!(
        json["files"]
            .as_array()
            .expect("files should be an array")
            .len(),
        2
    );

    let backlog =
        fs::read_to_string(vault_root.join("Backlog.md")).expect("Backlog.md should be readable");
    let mixed =
        fs::read_to_string(vault_root.join("Mixed.md")).expect("Mixed.md should be readable");
    assert!(backlog.contains("status: done"));
    assert!(mixed.contains("status: done"));
}

#[test]
fn update_command_dry_run_does_not_modify_files() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("mixed-properties", &vault_root);
    run_scan(&vault_root);

    let original =
        fs::read_to_string(vault_root.join("Backlog.md")).expect("Backlog.md should be readable");

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "update",
            "--where",
            "status = backlog",
            "--key",
            "priority",
            "--value",
            "high",
            "--dry-run",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Dry run"));

    let after =
        fs::read_to_string(vault_root.join("Backlog.md")).expect("Backlog.md should be readable");
    assert_eq!(original, after, "dry run should not modify the file");
}

#[test]
fn unset_command_removes_property_from_matching_notes() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("mixed-properties", &vault_root);
    run_scan(&vault_root);

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "unset",
            "--where",
            "status = backlog",
            "--key",
            "estimate",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Applied"));

    let backlog_content =
        fs::read_to_string(vault_root.join("Backlog.md")).expect("Backlog.md should be readable");
    assert!(
        !backlog_content.contains("estimate:"),
        "estimate property should be removed from backlog note"
    );

    let done_content =
        fs::read_to_string(vault_root.join("Done.md")).expect("Done.md should be readable");
    assert!(
        done_content.contains("estimate:"),
        "done note should be unaffected"
    );
}

#[test]
fn note_unset_command_reads_note_paths_from_stdin() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("mixed-properties", &vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .write_stdin("Backlog.md\nDone.md\n")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "note",
            "unset",
            "--stdin",
            "--key",
            "due",
            "--no-commit",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);
    assert_eq!(
        json["files"]
            .as_array()
            .expect("files should be an array")
            .len(),
        2
    );

    let backlog =
        fs::read_to_string(vault_root.join("Backlog.md")).expect("Backlog.md should be readable");
    let done = fs::read_to_string(vault_root.join("Done.md")).expect("Done.md should be readable");
    let mixed =
        fs::read_to_string(vault_root.join("Mixed.md")).expect("Mixed.md should be readable");
    assert!(!backlog.contains("due:"));
    assert!(!done.contains("due:"));
    assert!(mixed.contains("due: someday"));
}

#[test]
fn unset_command_dry_run_does_not_modify_files() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("mixed-properties", &vault_root);
    run_scan(&vault_root);

    let original =
        fs::read_to_string(vault_root.join("Done.md")).expect("Done.md should be readable");

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "unset",
            "--where",
            "status = done",
            "--key",
            "estimate",
            "--dry-run",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Dry run"));

    let after = fs::read_to_string(vault_root.join("Done.md")).expect("Done.md should be readable");
    assert_eq!(original, after, "dry run should not modify the file");
}

#[test]
fn update_command_json_output_includes_mutation_report() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("mixed-properties", &vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "update",
            "--where",
            "status = backlog",
            "--key",
            "flagged",
            "--value",
            "true",
            "--dry-run",
        ])
        .assert()
        .success();

    let json = parse_stdout_json(&assert);
    assert_eq!(json["dry_run"], true);
    assert_eq!(json["key"], "flagged");
    assert_eq!(json["value"], "true");
    assert!(
        json["filters"].as_array().is_some(),
        "JSON output should include filters"
    );
}

#[test]
fn refactor_rewrite_command_reads_note_paths_from_stdin() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("mixed-properties", &vault_root);
    run_scan(&vault_root);

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .write_stdin("Backlog.md\nDone.md\n")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "refactor",
            "rewrite",
            "--stdin",
            "--find",
            "release",
            "--replace",
            "launch",
            "--no-commit",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("bulk_replace"));

    let backlog =
        fs::read_to_string(vault_root.join("Backlog.md")).expect("Backlog.md should be readable");
    let done = fs::read_to_string(vault_root.join("Done.md")).expect("Done.md should be readable");
    let mixed =
        fs::read_to_string(vault_root.join("Mixed.md")).expect("Mixed.md should be readable");
    assert!(backlog.contains("launch planning"));
    assert!(done.contains("launch readiness"));
    assert!(mixed.contains("release risk"));
}

#[test]
fn bases_view_add_command_creates_view_and_previews_rows() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("bases", &vault_root);
    run_scan(&vault_root);

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "bases",
            "view-add",
            "release.base",
            "Sprint",
            "--filter",
            "status = backlog",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Sprint"));

    let contents = fs::read_to_string(vault_root.join("release.base"))
        .expect("release.base should be readable");
    assert!(
        contents.contains("Sprint"),
        "Sprint view should be in the file"
    );
}

#[test]
fn bases_view_delete_command_removes_view() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("bases", &vault_root);
    run_scan(&vault_root);

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "bases",
            "view-delete",
            "release.base",
            "Board",
        ])
        .assert()
        .success();

    let contents = fs::read_to_string(vault_root.join("release.base"))
        .expect("release.base should be readable");
    assert!(
        !contents.contains("Board"),
        "Board view should be removed from the file"
    );
}

#[test]
fn bases_view_rename_command_renames_view() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("bases", &vault_root);
    run_scan(&vault_root);

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "bases",
            "view-rename",
            "release.base",
            "Release Table",
            "Renamed",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Renamed"));

    let contents = fs::read_to_string(vault_root.join("release.base"))
        .expect("release.base should be readable");
    assert!(
        contents.contains("Renamed"),
        "new name should be in the file"
    );
    assert!(
        !contents.contains("Release Table"),
        "old name should be gone"
    );
}

#[test]
fn bases_view_edit_command_adds_filter() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("bases", &vault_root);
    run_scan(&vault_root);

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "bases",
            "view-edit",
            "release.base",
            "Release Table",
            "--add-filter",
            "reviewed = true",
        ])
        .assert()
        .success();

    let contents = fs::read_to_string(vault_root.join("release.base"))
        .expect("release.base should be readable");
    assert!(
        contents.contains("reviewed = true"),
        "added filter should be in the file"
    );
}

#[test]
fn command_json_outputs_match_composite_snapshot() {
    assert_json_snapshot("commands_composite.json", &build_command_snapshot());
}

#[test]
fn edit_new_creates_note_and_updates_cache() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault root should exist");
    run_scan(&vault_root);
    let editor = write_test_editor(temp_dir.path(), "Created by test");
    let vault_root_str = vault_root
        .to_str()
        .expect("vault path should be valid utf-8")
        .to_string();

    let edit_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .env("EDITOR", editor)
        .args([
            "--vault",
            &vault_root_str,
            "--output",
            "json",
            "edit",
            "--new",
            "Notes/Idea.md",
        ])
        .assert()
        .success();
    let edit_json = parse_stdout_json(&edit_assert);

    assert_eq!(edit_json["path"], "Notes/Idea.md");
    assert_eq!(edit_json["created"], true);
    assert_eq!(edit_json["rescanned"], true);
    assert_eq!(
        fs::read_to_string(vault_root.join("Notes/Idea.md"))
            .expect("new note should be readable")
            .replace("\r\n", "\n"),
        "Created by test\n"
    );

    let notes_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "--output",
            "json",
            "--fields",
            "document_path",
            "notes",
        ])
        .assert()
        .success();
    let note_rows = parse_stdout_json_lines(&notes_assert);
    assert!(note_rows
        .iter()
        .any(|row| row["document_path"] == "Notes/Idea.md"));
}

#[test]
fn saved_create_and_delete_manage_report_files() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);
    let vault_root_str = vault_root
        .to_str()
        .expect("vault path should be valid utf-8")
        .to_string();

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "saved",
            "create",
            "notes",
            "open-notes",
            "--where",
            "status = open",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Saved report: open-notes"));
    assert!(vault_root.join(".vulcan/reports/open-notes.toml").exists());

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["--vault", &vault_root_str, "saved", "delete", "open-notes"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Deleted saved report open-notes"));
    assert!(!vault_root.join(".vulcan/reports/open-notes.toml").exists());
}

#[test]
fn saved_report_and_export_outputs_match_snapshot() {
    assert_json_snapshot(
        "saved_reports_and_exports.json",
        &build_saved_report_snapshot(),
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn hardening_vault_cli_flow_covers_scan_query_mutate_refactor_export_and_rerun() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("hardening", &vault_root);
    let vault_root_str = vault_root.to_str().expect("utf-8").to_string();

    let init_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "--output",
            "json",
            "index",
            "init",
        ])
        .assert()
        .success();
    let init_json = parse_stdout_json(&init_assert);
    assert_eq!(init_json["vault_root"], vault_root_str);

    let scan_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "--output",
            "json",
            "scan",
            "--full",
        ])
        .assert()
        .success();
    let scan_json = parse_stdout_json(&scan_assert);
    assert_eq!(scan_json["mode"], "full");
    assert!(
        scan_json["discovered"]
            .as_u64()
            .is_some_and(|count| count >= 10),
        "expected the hardening fixture to discover a broad mix of files, got: {scan_json}"
    );

    let query_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "--output",
            "json",
            "--fields",
            "document_path",
            "query",
            "from notes where status = active order by file.name asc",
        ])
        .assert()
        .success();
    let query_rows = parse_stdout_json_lines(&query_assert);
    let query_paths = query_rows
        .iter()
        .map(|row| {
            row["document_path"]
                .as_str()
                .expect("document_path should be a string")
                .to_string()
        })
        .collect::<Vec<_>>();
    assert!(query_paths.contains(&"Projects/Alpha.md".to_string()));
    assert!(query_paths.contains(&"TaskNotes/Projects/Release.md".to_string()));

    let search_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "--output",
            "json",
            "--fields",
            "document_path",
            "search",
            "docs",
        ])
        .assert()
        .success();
    let search_rows = parse_stdout_json_lines(&search_assert);
    let search_paths = search_rows
        .iter()
        .map(|row| {
            row["document_path"]
                .as_str()
                .expect("document_path should be a string")
                .to_string()
        })
        .collect::<Vec<_>>();
    assert!(search_paths.contains(&"Home.md".to_string()));
    assert!(search_paths.contains(&"TaskNotes/Tasks/Write Docs.md".to_string()));

    let dataview_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "--output",
            "json",
            "dataview",
            "query",
            "TABLE file.day, project FROM \"Journal/Daily\" SORT file.day ASC",
        ])
        .assert()
        .success();
    let dataview_json = parse_stdout_json(&dataview_assert);
    assert_eq!(dataview_json["query_type"], "table");
    assert_eq!(dataview_json["result_count"], 1);

    let daily_assert = cargo_vulcan_at_time("2026-04-05T12:00:00Z")
        .args([
            "--vault",
            &vault_root_str,
            "--output",
            "json",
            "daily",
            "today",
            "--no-edit",
        ])
        .assert()
        .success();
    let daily_json = parse_stdout_json(&daily_assert);
    assert_eq!(daily_json["path"], "Journal/Daily/2026-04-05.md");
    assert_eq!(daily_json["created"], true);
    let created_daily = fs::read_to_string(vault_root.join("Journal/Daily/2026-04-05.md"))
        .expect("daily note should be created");
    assert!(created_daily.contains("## Log"));

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "--output",
            "json",
            "note",
            "append",
            "Home.md",
            "Escalate [[Projects/Beta]].",
            "--no-commit",
        ])
        .assert()
        .success();
    let appended_home =
        fs::read_to_string(vault_root.join("Home.md")).expect("home note should be readable");
    assert!(appended_home.contains("Escalate [[Projects/Beta]]."));

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "--output",
            "json",
            "refactor",
            "move",
            "Projects/Beta.md",
            "Archive/Beta.md",
            "--no-commit",
        ])
        .assert()
        .success();
    assert!(!vault_root.join("Projects/Beta.md").exists());
    assert!(vault_root.join("Archive/Beta.md").exists());
    let rewritten_home =
        fs::read_to_string(vault_root.join("Home.md")).expect("home note should be readable");
    assert!(!rewritten_home.contains("[[Projects/Beta]]"));
    assert!(
        rewritten_home.contains("[[Beta]]") || rewritten_home.contains("[[Archive/Beta]]"),
        "move rewrite should update Home.md to the new target, got: {rewritten_home}"
    );

    let export_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "export",
            "json",
            r#"from notes where file.path = "Archive/Beta.md""#,
            "--pretty",
        ])
        .assert()
        .success();
    let export_json = parse_stdout_json(&export_assert);
    assert_eq!(export_json["result_count"], 1);
    assert_eq!(export_json["notes"][0]["document_path"], "Archive/Beta.md");

    let doctor_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["--vault", &vault_root_str, "--output", "json", "doctor"])
        .assert()
        .success();
    let doctor_json = parse_stdout_json(&doctor_assert);
    assert_eq!(doctor_json["summary"]["parse_failures"], 1);
    assert_eq!(doctor_json["summary"]["unresolved_links"], 0);

    for _ in 0..2 {
        let rerun_assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args(["--vault", &vault_root_str, "--output", "json", "scan"])
            .assert()
            .success();
        let rerun_json = parse_stdout_json(&rerun_assert);
        assert_eq!(rerun_json["mode"], "incremental");
        assert_eq!(rerun_json["added"], 0);
        assert_eq!(rerun_json["updated"], 0);
        assert_eq!(rerun_json["deleted"], 0);
        assert!(
            rerun_json["unchanged"]
                .as_u64()
                .is_some_and(|count| count >= 10),
            "incremental reruns should settle into unchanged-only scans, got: {rerun_json}"
        );
    }
}

#[test]
#[ignore = "regenerates the checked-in composite command snapshot"]
fn regenerate_command_json_snapshot() {
    write_json_snapshot("commands_composite.json", &build_command_snapshot());
}

#[test]
#[ignore = "regenerates the checked-in saved report snapshot"]
fn regenerate_saved_report_snapshot() {
    write_json_snapshot(
        "saved_reports_and_exports.json",
        &build_saved_report_snapshot(),
    );
}

#[test]
#[ignore = "regenerates the checked-in config help snapshot"]
fn regenerate_help_config_snapshot() {
    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["help", "config", "--output", "json"])
        .assert()
        .success();
    write_json_snapshot("help_config.json", &parse_stdout_json(&assert));
}

fn parse_stdout_json(assert: &assert_cmd::assert::Assert) -> Value {
    serde_json::from_slice(&assert.get_output().stdout).expect("stdout should contain valid json")
}

fn parse_stdout_json_lines(assert: &assert_cmd::assert::Assert) -> Vec<Value> {
    String::from_utf8(assert.get_output().stdout.clone())
        .expect("stdout should be valid utf-8")
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("each line should contain valid json"))
        .collect()
}

fn document_paths(database: &CacheDatabase) -> Vec<String> {
    let mut statement = database
        .connection()
        .prepare("SELECT path FROM documents ORDER BY path")
        .expect("statement should prepare");
    let rows = statement
        .query_map([], |row| row.get(0))
        .expect("query should succeed");

    rows.map(|row| row.expect("row should deserialize"))
        .collect()
}

fn initialize_vulcan_dir(vault_root: &Path) {
    fs::create_dir_all(vault_root.join(".vulcan")).expect(".vulcan dir should be created");
}

fn run_scan(vault_root: &Path) {
    initialize_vulcan_dir(vault_root);

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "scan",
            "--full",
        ])
        .assert()
        .success();
}

fn run_incremental_scan(vault_root: &Path) {
    initialize_vulcan_dir(vault_root);

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "scan",
        ])
        .assert()
        .success();
}

fn write_note_crud_sample(vault_root: &Path) {
    fs::create_dir_all(vault_root).expect("vault root should be created");
    fs::write(
        vault_root.join("Dashboard.md"),
        concat!(
            "---\n",
            "status: active\n",
            "tags:\n",
            "  - project\n",
            "---\n",
            "# Dashboard\n",
            "\n",
            "Intro line\n",
            "## Tasks\n",
            "Before\n",
            "TODO first\n",
            "Context after\n",
            "### Nested\n",
            "TODO nested\n",
            "## Done\n",
            "Finished line\n",
            "- Item line\n",
            "^done-item\n",
        ),
    )
    .expect("Dashboard.md should be written");
}

fn write_note_checkbox_sample(vault_root: &Path) {
    fs::create_dir_all(vault_root).expect("vault root should be created");
    fs::write(
        vault_root.join("Checklist.md"),
        concat!(
            "# Checklist\n",
            "## Phase A\n",
            "- [ ] Alpha\n",
            "- [x] Beta\n",
            "### Nested\n",
            "- [ ] Gamma\n",
            "## Phase B\n",
            "- [ ] Delta\n",
        ),
    )
    .expect("Checklist.md should be written");
}

fn write_test_editor(base: &Path, body: &str) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let script = base.join("editor.sh");
        fs::write(
            &script,
            format!("#!/bin/sh\nprintf '%s\\n' '{body}' > \"$1\"\n"),
        )
        .expect("editor script should be written");
        let mut permissions = fs::metadata(&script)
            .expect("editor script metadata should load")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script, permissions).expect("editor script should be executable");
        format!("sh {}", script.display())
    }

    #[cfg(windows)]
    {
        let script = base.join("editor.cmd");
        let payload = base.join("editor-payload.txt");
        let payload_body = if body.ends_with('\n') {
            body.to_string()
        } else {
            format!("{body}\r\n")
        };
        fs::write(&payload, payload_body).expect("editor payload should be written");
        fs::write(
            &script,
            format!("@echo off\r\ntype \"{}\" > \"%~1\"\r\n", payload.display()),
        )
        .expect("editor script should be written");
        format!("cmd /C {}", script.display())
    }
}

fn replace_field_recursively(value: &mut Value, field: &str, replacement: &Value) {
    match value {
        Value::Object(object) => {
            if let Some(slot) = object.get_mut(field) {
                *slot = replacement.clone();
            }
            for nested in object.values_mut() {
                replace_field_recursively(nested, field, replacement);
            }
        }
        Value::Array(values) => {
            for nested in values {
                replace_field_recursively(nested, field, replacement);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn assert_json_snapshot(name: &str, value: &Value) {
    let snapshot_path = snapshot_path(name);
    let expected = fs::read_to_string(snapshot_path)
        .expect("snapshot should be readable")
        .replace("\r\n", "\n");
    let actual = serde_json::to_string_pretty(value).expect("json should serialize");

    assert_eq!(actual, expected.trim_end_matches('\n'));
}

fn assert_json_snapshot_lines(name: &str, values: &[Value]) {
    let snapshot_path = snapshot_path(name);
    let expected = fs::read_to_string(snapshot_path)
        .expect("snapshot should be readable")
        .replace("\r\n", "\n");
    let actual = serde_json::to_string_pretty(values).expect("json should serialize");

    assert_eq!(actual, expected.trim_end_matches('\n'));
}

fn write_json_snapshot(name: &str, value: &Value) {
    let snapshot_path = snapshot_path(name);
    if let Some(parent) = snapshot_path.parent() {
        fs::create_dir_all(parent).expect("snapshot directory should exist");
    }
    fs::write(
        snapshot_path,
        serde_json::to_string_pretty(value).expect("snapshot should serialize"),
    )
    .expect("snapshot should write");
}

fn snapshot_path(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/snapshots")
        .join(name)
}

#[allow(clippy::too_many_lines)]
fn build_command_snapshot() -> Value {
    let temp_dir = TempDir::new().expect("temp dir should be created");

    let init_root = temp_dir.path().join("init-vault");
    fs::create_dir_all(&init_root).expect("init vault should exist");
    let init_json = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args([
                "--vault",
                init_root
                    .to_str()
                    .expect("vault path should be valid utf-8"),
                "--output",
                "json",
                "index",
                "init",
            ])
            .assert()
            .success();
        let mut json = parse_stdout_json(&assert);
        json["vault_root"] = Value::String("<vault>".to_string());
        json["cache_path"] = Value::String("<vault>/.vulcan/cache.db".to_string());
        json["config_path"] = Value::String("<vault>/.vulcan/config.toml".to_string());
        json
    };

    let basic_root = temp_dir.path().join("basic");
    copy_fixture_vault("basic", &basic_root);
    let basic_root_str = basic_root
        .to_str()
        .expect("vault path should be valid utf-8")
        .to_string();
    let scan_json = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args([
                "--vault",
                &basic_root_str,
                "--output",
                "json",
                "index",
                "scan",
                "--full",
            ])
            .assert()
            .success();
        parse_stdout_json(&assert)
    };
    let rebuild_json = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args([
                "--vault",
                &basic_root_str,
                "--output",
                "json",
                "index",
                "rebuild",
                "--dry-run",
            ])
            .assert()
            .success();
        parse_stdout_json(&assert)
    };
    let repair_json = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args([
                "--vault",
                &basic_root_str,
                "--output",
                "json",
                "index",
                "repair",
                "fts",
                "--dry-run",
            ])
            .assert()
            .success();
        parse_stdout_json(&assert)
    };
    let links_json = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args([
                "--vault",
                &basic_root_str,
                "--output",
                "json",
                "--fields",
                "note_path,raw_text,resolved_target_path,resolution_status",
                "note",
                "links",
                "Start",
            ])
            .assert()
            .success();
        Value::Array(parse_stdout_json_lines(&assert))
    };
    let backlinks_json = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args([
                "--vault",
                &basic_root_str,
                "--output",
                "json",
                "--fields",
                "note_path,source_path,raw_text",
                "note",
                "backlinks",
                "Projects/Alpha",
            ])
            .assert()
            .success();
        Value::Array(parse_stdout_json_lines(&assert))
    };
    let search_json = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args([
                "--vault",
                &basic_root_str,
                "--output",
                "json",
                "--fields",
                "document_path,heading_path,query,snippet",
                "search",
                "dashboard",
            ])
            .assert()
            .success();
        Value::Array(parse_stdout_json_lines(&assert))
    };
    let describe_json = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args(["--output", "json", "describe"])
            .assert()
            .success();
        parse_stdout_json(&assert)
    };

    let mixed_root = temp_dir.path().join("mixed");
    copy_fixture_vault("mixed-properties", &mixed_root);
    run_scan(&mixed_root);
    let mixed_root_str = mixed_root
        .to_str()
        .expect("vault path should be valid utf-8")
        .to_string();
    let notes_json = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args([
                "--vault",
                &mixed_root_str,
                "--output",
                "json",
                "--fields",
                "document_path,properties",
                "notes",
                "--where",
                "estimate > 2",
                "--sort",
                "due",
            ])
            .assert()
            .success();
        Value::Array(parse_stdout_json_lines(&assert))
    };

    let bases_root = temp_dir.path().join("bases");
    copy_fixture_vault("bases", &bases_root);
    run_scan(&bases_root);
    let bases_root_str = bases_root
        .to_str()
        .expect("vault path should be valid utf-8")
        .to_string();
    let bases_json = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args([
                "--vault",
                &bases_root_str,
                "--output",
                "json",
                "bases",
                "eval",
                "release.base",
            ])
            .assert()
            .success();
        let mut json = parse_stdout_json(&assert);
        replace_field_recursively(&mut json, "file_mtime", &serde_json::json!(0));
        json
    };
    let suggestions_root = temp_dir.path().join("suggestions");
    copy_fixture_vault("suggestions", &suggestions_root);
    run_scan(&suggestions_root);
    let suggestions_root_str = suggestions_root
        .to_str()
        .expect("vault path should be valid utf-8")
        .to_string();
    let suggest_mentions_json = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args([
                "--vault",
                &suggestions_root_str,
                "--output",
                "json",
                "--fields",
                "source_path,matched_text,target_path,candidate_count,status",
                "refactor",
                "suggest",
                "mentions",
                "Home",
            ])
            .assert()
            .success();
        Value::Array(parse_stdout_json_lines(&assert))
    };
    let suggest_duplicates_json = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args([
                "--vault",
                &suggestions_root_str,
                "--output",
                "json",
                "--fields",
                "kind,value,left_path,right_path,score",
                "refactor",
                "suggest",
                "duplicates",
            ])
            .assert()
            .success();
        Value::Array(parse_stdout_json_lines(&assert))
    };
    let link_mentions_json = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args([
                "--vault",
                &suggestions_root_str,
                "--output",
                "json",
                "refactor",
                "link-mentions",
                "Home",
                "--dry-run",
            ])
            .assert()
            .success();
        parse_stdout_json(&assert)
    };
    let rewrite_json = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args([
                "--vault",
                &suggestions_root_str,
                "--output",
                "json",
                "refactor",
                "rewrite",
                "--find",
                "Guide",
                "--replace",
                "Manual",
                "--dry-run",
            ])
            .assert()
            .success();
        parse_stdout_json(&assert)
    };

    let move_root = temp_dir.path().join("move");
    copy_fixture_vault("move-rewrite", &move_root);
    run_scan(&move_root);
    let move_root_str = move_root
        .to_str()
        .expect("vault path should be valid utf-8")
        .to_string();
    let move_json = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args([
                "--vault",
                &move_root_str,
                "--output",
                "json",
                "refactor",
                "move",
                "Projects/Alpha.md",
                "Archive/Alpha.md",
                "--dry-run",
            ])
            .assert()
            .success();
        parse_stdout_json(&assert)
    };

    let doctor_root = temp_dir.path().join("broken");
    copy_fixture_vault("broken-frontmatter", &doctor_root);
    run_scan(&doctor_root);
    let doctor_root_str = doctor_root
        .to_str()
        .expect("vault path should be valid utf-8")
        .to_string();
    let doctor_json = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args(["--vault", &doctor_root_str, "--output", "json", "doctor"])
            .assert()
            .success();
        parse_stdout_json(&assert)
    };

    let vectors_root = temp_dir.path().join("vectors");
    copy_fixture_vault("basic", &vectors_root);
    let server = MockEmbeddingServer::spawn();
    write_embedding_config(&vectors_root, &server.base_url());
    run_scan(&vectors_root);
    let vectors_root_str = vectors_root
        .to_str()
        .expect("vault path should be valid utf-8")
        .to_string();
    let vectors_index_json = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args([
                "--vault",
                &vectors_root_str,
                "--output",
                "json",
                "vectors",
                "index",
            ])
            .assert()
            .success();
        let mut json = parse_stdout_json(&assert);
        json["elapsed_seconds"] = serde_json::json!(0.0);
        json["rate_per_second"] = serde_json::json!(0.0);
        json["endpoint_url"] = serde_json::json!("http://127.0.0.1:0/v1/embeddings");
        json
    };
    let vectors_neighbors_json = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args([
                "--vault",
                &vectors_root_str,
                "--output",
                "json",
                "--fields",
                "document_path,distance",
                "--limit",
                "2",
                "vectors",
                "neighbors",
                "dashboard",
            ])
            .assert()
            .success();
        Value::Array(parse_stdout_json_lines(&assert))
    };
    let vectors_duplicates_json = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args([
                "--vault",
                &vectors_root_str,
                "--output",
                "json",
                "--fields",
                "left_document_path,right_document_path,similarity",
                "vectors",
                "duplicates",
                "--threshold",
                "0.7",
            ])
            .assert()
            .success();
        Value::Array(parse_stdout_json_lines(&assert))
    };
    let cluster_json = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args([
                "--vault",
                &vectors_root_str,
                "--output",
                "json",
                "--fields",
                "cluster_id,cluster_label,keywords,chunk_count,document_count",
                "cluster",
                "--clusters",
                "2",
            ])
            .assert()
            .success();
        Value::Array(parse_stdout_json_lines(&assert))
    };
    server.shutdown();

    serde_json::json!({
        "index_init": init_json,
        "index_scan": scan_json,
        "index_rebuild": rebuild_json,
        "index_repair_fts": repair_json,
        "note_links": links_json,
        "note_backlinks": backlinks_json,
        "search": search_json,
        "notes": notes_json,
        "bases": bases_json,
        "refactor_suggest_mentions": suggest_mentions_json,
        "refactor_suggest_duplicates": suggest_duplicates_json,
        "refactor_link_mentions": link_mentions_json,
        "refactor_rewrite": rewrite_json,
        "refactor_move": move_json,
        "doctor": doctor_json,
        "describe": describe_json,
        "vectors_index": vectors_index_json,
        "vectors_neighbors": vectors_neighbors_json,
        "vectors_duplicates": vectors_duplicates_json,
        "cluster": cluster_json,
    })
}

#[allow(clippy::too_many_lines)]
fn build_saved_report_snapshot() -> Value {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("bases");
    copy_fixture_vault("bases", &vault_root);
    run_scan(&vault_root);
    let vault_root_str = vault_root
        .to_str()
        .expect("vault path should be valid utf-8")
        .to_string();

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "--fields",
            "document_path,rank",
            "--limit",
            "1",
            "saved",
            "search",
            "weekly-search",
            "release",
            "--description",
            "weekly release hits",
            "--export",
            "jsonl",
            "--export-path",
            "exports/search.jsonl",
        ])
        .assert()
        .success();
    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "--fields",
            "document_path,group_value",
            "saved",
            "bases",
            "release-table",
            "release.base",
            "--export",
            "csv",
            "--export-path",
            "exports/release.csv",
        ])
        .assert()
        .success();

    let list_json = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args([
                "--vault",
                &vault_root_str,
                "--output",
                "json",
                "saved",
                "list",
            ])
            .assert()
            .success();
        Value::Array(parse_stdout_json_lines(&assert))
    };
    let show_json = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args([
                "--vault",
                &vault_root_str,
                "--output",
                "json",
                "saved",
                "show",
                "weekly-search",
            ])
            .assert()
            .success();
        parse_stdout_json(&assert)
    };
    let run_json = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args([
                "--vault",
                &vault_root_str,
                "--output",
                "json",
                "saved",
                "run",
                "weekly-search",
            ])
            .assert()
            .success();
        Value::Array(parse_stdout_json_lines(&assert))
    };
    let batch_json = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args([
                "--vault",
                &vault_root_str,
                "--output",
                "json",
                "batch",
                "--all",
            ])
            .assert()
            .success();
        let mut json = parse_stdout_json(&assert);
        replace_string_recursively(&mut json, &vault_root.display().to_string(), "<vault>");
        // Normalize any remaining backslash path separators (Windows) to forward slashes.
        replace_string_recursively(&mut json, "\\", "/");
        json
    };
    let search_export = fs::read_to_string(vault_root.join("exports/search.jsonl"))
        .expect("search export should exist")
        .replace("\r\n", "\n");
    let bases_export = fs::read_to_string(vault_root.join("exports/release.csv"))
        .expect("bases export should exist")
        .replace("\r\n", "\n");

    serde_json::json!({
        "saved_list": list_json,
        "saved_show": show_json,
        "saved_run": run_json,
        "batch": batch_json,
        "search_export": search_export,
        "bases_export": bases_export,
    })
}

fn copy_fixture_vault(name: &str, destination: &Path) {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../tests/fixtures/vaults")
        .join(name);

    copy_dir_recursive(&source, destination);
    fs::create_dir_all(destination.join(".vulcan")).expect(".vulcan dir should be created");
}

fn copy_dir_recursive(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).expect("destination directory should be created");

    for entry in fs::read_dir(source).expect("source directory should be readable") {
        let entry = entry.expect("directory entry should be readable");
        let file_type = entry.file_type().expect("file type should be readable");
        // Skip .vulcan/ directories — they are test artifacts, not fixture content.
        if entry.file_name() == ".vulcan" {
            continue;
        }
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

fn write_tasknotes_views_fixture(vault_root: &Path) {
    fs::create_dir_all(vault_root.join("TaskNotes/Views"))
        .expect("tasknotes views directory should be created");
    fs::write(
        vault_root.join("TaskNotes/Views/tasks-default.base"),
        concat!(
            "source:\n",
            "  type: tasknotes\n",
            "  config:\n",
            "    type: tasknotesTaskList\n",
            "    includeArchived: false\n",
            "views:\n",
            "  - type: tasknotesTaskList\n",
            "    name: Tasks\n",
            "    order:\n",
            "      - file.name\n",
            "      - priorityWeight\n",
            "      - efficiencyRatio\n",
            "      - urgencyScore\n",
            "    sort:\n",
            "      - column: file.name\n",
            "        direction: ASC\n",
        ),
    )
    .expect("tasks default base should be written");
    fs::write(
        vault_root.join("TaskNotes/Views/kanban-default.base"),
        concat!(
            "source:\n",
            "  type: tasknotes\n",
            "  config:\n",
            "    type: tasknotesKanban\n",
            "    includeArchived: false\n",
            "views:\n",
            "  - type: tasknotesKanban\n",
            "    name: Kanban Board\n",
            "    order:\n",
            "      - file.name\n",
            "      - status\n",
            "    groupBy:\n",
            "      property: status\n",
            "      direction: ASC\n",
        ),
    )
    .expect("kanban default base should be written");
}

fn write_embedding_config(vault_root: &Path, base_url: &str) {
    std::env::set_var("VULCAN_TEST_OPENAI_API_KEY", "fixture-key");
    fs::create_dir_all(vault_root.join(".vulcan")).expect("config directory should exist");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        format!(
            "[embedding]\nprovider = \"openai-compatible\"\nbase_url = \"{base_url}\"\napi_key_env = \"VULCAN_TEST_OPENAI_API_KEY\"\nmodel = \"fixture\"\nmax_batch_size = 8\nmax_concurrency = 1\n"
        ),
    )
    .expect("embedding config should be written");
}

struct MockEmbeddingServer {
    address: String,
    shutdown_tx: std::sync::mpsc::Sender<()>,
    handle: Option<thread::JoinHandle<()>>,
}

impl MockEmbeddingServer {
    fn spawn() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener should bind");
        listener
            .set_nonblocking(true)
            .expect("listener should support nonblocking mode");
        let address = listener
            .local_addr()
            .expect("listener should expose its local address");
        let (shutdown_tx, shutdown_rx) = std::sync::mpsc::channel();

        let handle = thread::spawn(move || loop {
            if shutdown_rx.try_recv().is_ok() {
                break;
            }

            match listener.accept() {
                Ok((mut stream, _)) => {
                    stream
                        .set_nonblocking(false)
                        .expect("stream should support blocking mode");
                    stream
                        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                        .expect("read timeout should be configurable");
                    let request = read_request(&mut stream);
                    let inputs = request
                        .body
                        .get("input")
                        .and_then(Value::as_array)
                        .expect("request should include input");
                    let body = serde_json::json!({
                        "data": inputs.iter().enumerate().map(|(index, input)| {
                            serde_json::json!({
                                "index": index,
                                "embedding": embedding_for_input(input.as_str().unwrap_or_default()),
                            })
                        }).collect::<Vec<_>>(),
                    })
                    .to_string();
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    stream
                        .write_all(response.as_bytes())
                        .expect("response should write");
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(error) => panic!("unexpected mock server error: {error}"),
            }
        });

        Self {
            address: format!("http://{address}/v1"),
            shutdown_tx,
            handle: Some(handle),
        }
    }

    fn base_url(&self) -> String {
        self.address.clone()
    }

    fn shutdown(mut self) {
        let _ = self.shutdown_tx.send(());
        if let Some(handle) = self.handle.take() {
            handle.join().expect("mock server should join");
        }
    }
}

struct MockWebServer {
    address: String,
    shutdown_tx: std::sync::mpsc::Sender<()>,
    handle: Option<thread::JoinHandle<()>>,
}

impl MockWebServer {
    #[allow(clippy::too_many_lines)]
    fn spawn() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener should bind");
        listener
            .set_nonblocking(true)
            .expect("listener should support nonblocking mode");
        let address = listener
            .local_addr()
            .expect("listener should expose its local address");
        let (shutdown_tx, shutdown_rx) = std::sync::mpsc::channel();

        let handle = thread::spawn(move || loop {
            if shutdown_rx.try_recv().is_ok() {
                break;
            }

            match listener.accept() {
                Ok((mut stream, _)) => {
                    stream
                        .set_nonblocking(false)
                        .expect("stream should support blocking mode");
                    let request = read_header_request(&mut stream);
                    if request.path.starts_with("/api/v0/search") {
                        let auth = request
                            .headers
                            .get("authorization")
                            .cloned()
                            .unwrap_or_default();
                        let status_line = if auth == "Bot test-token" {
                            "HTTP/1.1 200 OK"
                        } else {
                            "HTTP/1.1 401 Unauthorized"
                        };
                        let body = if auth == "Bot test-token" {
                            serde_json::json!({
                                "data": [
                                    {
                                        "title": "Release Notes",
                                        "url": "https://example.com/release",
                                        "snippet": "Everything that shipped this week."
                                    },
                                    {
                                        "title": "Status Update",
                                        "url": "https://example.com/status",
                                        "snippet": "Current project status."
                                    }
                                ]
                            })
                            .to_string()
                        } else {
                            serde_json::json!({ "error": "unauthorized" }).to_string()
                        };
                        write_http_response(
                            &mut stream,
                            status_line,
                            "application/json",
                            body.as_bytes(),
                        );
                    } else if request.path.starts_with("/api/web_search") {
                        let auth = request
                            .headers
                            .get("authorization")
                            .cloned()
                            .unwrap_or_default();
                        let (status_line, body) = if auth == "Bearer test-ollama-key" {
                            (
                                "HTTP/1.1 200 OK",
                                serde_json::json!({
                                    "results": [
                                        {
                                            "title": "Release Notes",
                                            "url": "https://example.com/release",
                                            "content": "Everything that shipped this week."
                                        },
                                        {
                                            "title": "Status Update",
                                            "url": "https://example.com/status",
                                            "content": "Current project status."
                                        }
                                    ]
                                })
                                .to_string(),
                            )
                        } else {
                            (
                                "HTTP/1.1 401 Unauthorized",
                                serde_json::json!({ "error": "unauthorized" }).to_string(),
                            )
                        };
                        write_http_response(
                            &mut stream,
                            status_line,
                            "application/json",
                            body.as_bytes(),
                        );
                    } else if request.path.starts_with("/exa/search") {
                        let api_key = request
                            .headers
                            .get("x-api-key")
                            .cloned()
                            .unwrap_or_default();
                        let (status_line, body) = if api_key == "test-exa-key" {
                            (
                                "HTTP/1.1 200 OK",
                                serde_json::json!({
                                    "results": [
                                        {
                                            "title": "Release Notes",
                                            "url": "https://example.com/release",
                                            "text": "Everything that shipped this week."
                                        },
                                        {
                                            "title": "Status Update",
                                            "url": "https://example.com/status",
                                            "text": "Current project status."
                                        }
                                    ]
                                })
                                .to_string(),
                            )
                        } else {
                            (
                                "HTTP/1.1 401 Unauthorized",
                                serde_json::json!({ "error": "unauthorized" }).to_string(),
                            )
                        };
                        write_http_response(
                            &mut stream,
                            status_line,
                            "application/json",
                            body.as_bytes(),
                        );
                    } else if request.path.starts_with("/tavily/search") {
                        // Tavily sends API key in request body; always return success for tests
                        let body = serde_json::json!({
                            "results": [
                                {
                                    "title": "Release Notes",
                                    "url": "https://example.com/release",
                                    "content": "Everything that shipped this week."
                                },
                                {
                                    "title": "Status Update",
                                    "url": "https://example.com/status",
                                    "content": "Current project status."
                                }
                            ]
                        })
                        .to_string();
                        write_http_response(
                            &mut stream,
                            "HTTP/1.1 200 OK",
                            "application/json",
                            body.as_bytes(),
                        );
                    } else if request.path.starts_with("/brave/search") {
                        let token = request
                            .headers
                            .get("x-subscription-token")
                            .cloned()
                            .unwrap_or_default();
                        let (status_line, body) = if token == "test-brave-key" {
                            (
                                "HTTP/1.1 200 OK",
                                serde_json::json!({
                                    "web": {
                                        "results": [
                                            {
                                                "title": "Release Notes",
                                                "url": "https://example.com/release",
                                                "description": "Everything that shipped this week."
                                            },
                                            {
                                                "title": "Status Update",
                                                "url": "https://example.com/status",
                                                "description": "Current project status."
                                            }
                                        ]
                                    }
                                })
                                .to_string(),
                            )
                        } else {
                            (
                                "HTTP/1.1 401 Unauthorized",
                                serde_json::json!({ "error": "unauthorized" }).to_string(),
                            )
                        };
                        write_http_response(
                            &mut stream,
                            status_line,
                            "application/json",
                            body.as_bytes(),
                        );
                    } else if request.path.starts_with("/html/") {
                        write_http_response(
                            &mut stream,
                            "HTTP/1.1 200 OK",
                            "text/html",
                            br#"<!doctype html><html><body>
<div class="result">
  <a class="result__a" href="https://example.com/release">Release Notes</a>
  <a class="result__snippet">Everything that shipped this week.</a>
</div>
<div class="result">
  <a class="result__a" href="https://duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fstatus">Status Update</a>
  <div class="result__snippet">Current project status.</div>
</div>
</body></html>"#,
                        );
                    } else if request.path == "/robots.txt" {
                        write_http_response(
                            &mut stream,
                            "HTTP/1.1 200 OK",
                            "text/plain",
                            b"User-agent: *\nDisallow:\n",
                        );
                    } else if request.path == "/article" {
                        write_http_response(
                            &mut stream,
                            "HTTP/1.1 200 OK",
                            "text/html",
                            br"<!doctype html><html><body><nav>skip me</nav><article><h1>Release Summary</h1><p>Shipped &amp; stable. This release paragraph is intentionally long enough for rs-trafilatura to keep the extraction focused on the main content instead of the surrounding chrome.</p></article></body></html>",
                        );
                    } else if request.path == "/generic-page" {
                        write_http_response(
                            &mut stream,
                            "HTTP/1.1 200 OK",
                            "text/html",
                            br"<!doctype html><html><body><nav>Site Nav</nav><main><h1>Docs</h1><p>Short</p></main></body></html>",
                        );
                    } else if request.path == "/empty" {
                        write_http_response(
                            &mut stream,
                            "HTTP/1.1 200 OK",
                            "text/html",
                            br"<!doctype html><html><body></body></html>",
                        );
                    } else if request.path == "/raw" {
                        write_http_response(
                            &mut stream,
                            "HTTP/1.1 200 OK",
                            "application/octet-stream",
                            b"raw-body",
                        );
                    } else {
                        write_http_response(
                            &mut stream,
                            "HTTP/1.1 404 Not Found",
                            "text/plain",
                            b"not found",
                        );
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(error) => panic!("unexpected mock server error: {error}"),
            }
        });

        Self {
            address: format!("http://{address}"),
            shutdown_tx,
            handle: Some(handle),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.address, path)
    }

    fn shutdown(mut self) {
        let _ = self.shutdown_tx.send(());
        if let Some(handle) = self.handle.take() {
            handle.join().expect("mock server should join");
        }
    }
}

#[derive(Debug)]
struct CapturedRequest {
    body: Value,
}

#[derive(Debug)]
struct CapturedHeaderRequest {
    path: String,
    headers: std::collections::BTreeMap<String, String>,
}

fn read_request(stream: &mut std::net::TcpStream) -> CapturedRequest {
    let mut buffer = Vec::new();
    let mut header_end = None;

    loop {
        let mut chunk = [0_u8; 1024];
        let bytes_read = stream.read(&mut chunk).expect("request should be readable");
        if bytes_read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..bytes_read]);
        if let Some(position) = find_subslice(&buffer, b"\r\n\r\n") {
            header_end = Some(position + 4);
            break;
        }
    }

    let header_end = header_end.expect("request should contain headers");
    let header_text = String::from_utf8(buffer[..header_end].to_vec()).expect("utf8 headers");
    let content_length = header_text
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .and_then(|value| value.trim().parse::<usize>().ok())
        })
        .expect("request should include content length");
    let mut body_bytes = buffer[header_end..].to_vec();
    while body_bytes.len() < content_length {
        let mut chunk = vec![0_u8; content_length - body_bytes.len()];
        let bytes_read = stream
            .read(chunk.as_mut_slice())
            .expect("body should be readable");
        if bytes_read == 0 {
            break;
        }
        body_bytes.extend_from_slice(&chunk[..bytes_read]);
    }

    CapturedRequest {
        body: serde_json::from_slice(&body_bytes).expect("request body should parse"),
    }
}

fn read_header_request(stream: &mut std::net::TcpStream) -> CapturedHeaderRequest {
    let mut buffer = Vec::new();

    loop {
        let mut chunk = [0_u8; 1024];
        let bytes_read = stream.read(&mut chunk).expect("request should be readable");
        if bytes_read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..bytes_read]);
        if find_subslice(&buffer, b"\r\n\r\n").is_some() {
            break;
        }
    }

    let request = String::from_utf8(buffer).expect("request should be utf8");
    let mut lines = request.lines();
    let request_line = lines
        .next()
        .expect("request should start with a request line");
    let path = request_line
        .split_whitespace()
        .nth(1)
        .expect("request line should include a path")
        .to_string();
    let headers = lines
        .take_while(|line| !line.trim().is_empty())
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            Some((name.trim().to_ascii_lowercase(), value.trim().to_string()))
        })
        .collect();

    CapturedHeaderRequest { path, headers }
}

fn write_http_response(
    stream: &mut std::net::TcpStream,
    status_line: &str,
    content_type: &str,
    body: &[u8],
) {
    let headers = format!(
        "{status_line}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream
        .write_all(headers.as_bytes())
        .expect("headers should write");
    stream.write_all(body).expect("body should write");
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn embedding_for_input(input: &str) -> Vec<f32> {
    if input.contains("dashboard") || input.contains("Home links") {
        vec![1.0, 0.0]
    } else if input.contains("Bob") || input.contains("ownership") {
        vec![0.0, 1.0]
    } else if input.contains("Alpha") || input.contains("Project") {
        vec![0.75, 0.25]
    } else {
        vec![0.5, 0.5]
    }
}

/// Returns true if `text` contains any ANSI escape sequence byte sequences.
fn contains_ansi(text: &str) -> bool {
    text.contains('\x1b')
}

#[test]
fn json_output_contains_no_ansi_escape_codes() {
    // Commands that produce JSON output should never include ANSI escape codes.
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault = temp_dir.path();
    fs::write(vault.join("note.md"), "---\ntitle: Test\n---\n# Test\n")
        .expect("note should be written");

    let commands_with_json_output: &[&[&str]] = &[
        &["scan", "--output", "json"],
        &["query", "--output", "json"],
        &["doctor", "--output", "json"],
        &["tags", "--output", "json"],
        &["graph", "analytics", "--output", "json"],
    ];

    for args in commands_with_json_output {
        let output = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args(*args)
            .arg("--vault")
            .arg(vault)
            .output()
            .expect("command should run");

        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            !contains_ansi(&stdout),
            "ANSI codes found in JSON stdout for {:?}: {:?}",
            args,
            &stdout[..stdout.len().min(200)]
        );
    }
}

#[test]
fn json_output_error_is_structured() {
    // When --output json is requested and a command fails, the error should be
    // output as {"error": "...", "code": "..."} on stdout (not unstructured stderr text).
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault = temp_dir.path();

    // Requesting a note that doesn't exist should produce a structured error
    let output = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["note", "get", "nonexistent-note-xyz", "--output", "json"])
        .arg("--vault")
        .arg(vault)
        .output()
        .expect("command should run");

    assert_ne!(
        output.status.code(),
        Some(0),
        "should exit with non-zero code on error"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.trim().is_empty(),
        "JSON error output should be on stdout"
    );

    let parsed: Value =
        serde_json::from_str(stdout.trim()).expect("JSON error output should be valid JSON");
    assert!(
        parsed.get("error").is_some(),
        "JSON error should have 'error' field, got: {stdout}"
    );
    assert!(
        parsed.get("code").is_some(),
        "JSON error should have 'code' field, got: {stdout}"
    );
    assert!(
        !contains_ansi(&stdout),
        "JSON error output should not contain ANSI codes"
    );
}

#[test]
fn quiet_flag_suppresses_warnings_but_not_primary_output() {
    // --quiet should suppress warnings (e.g. auto-commit warnings) but still emit
    // the primary structured output so pipelines can process results.
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault = temp_dir.path();
    // Create a git-backed vault so that auto-commit warnings might fire
    init_git_repo(vault);
    fs::write(vault.join("note.md"), "---\ntitle: A\n---\n# A\n").expect("note should be written");
    commit_all(vault, "initial");
    // Enable auto-commit in config
    fs::create_dir_all(vault.join(".vulcan")).expect("vulcan dir");
    fs::write(
        vault.join(".vulcan/config.toml"),
        "[git]\nauto_commit = true\n",
    )
    .expect("config should write");

    // With --quiet, no warnings on stderr, but scan summary should still work
    let output = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["scan", "--quiet", "--output", "json"])
        .arg("--vault")
        .arg(vault)
        .output()
        .expect("command should run");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("warning:"),
        "--quiet should suppress warnings, but got: {stderr}"
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: Value =
        serde_json::from_str(stdout.trim()).expect("JSON scan output should be valid JSON");
    assert!(
        parsed.get("discovered").is_some()
            || parsed.get("added").is_some()
            || parsed.get("total").is_some(),
        "scan JSON output should contain count fields, got: {stdout}"
    );
}

#[test]
fn commands_exit_cleanly_when_stdout_pipe_closes_early() {
    let mut child = std::process::Command::new(assert_cmd::cargo::cargo_bin("vulcan"))
        .args(["completions", "fish"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("command should start");

    let mut stdout = child.stdout.take().expect("stdout should be piped");
    let mut buffer = [0_u8; 4096];
    let read = stdout
        .read(&mut buffer)
        .expect("should read some bytes before closing stdout");
    assert!(
        read > 0,
        "command should emit output before the pipe closes"
    );
    drop(stdout);

    let output = child
        .wait_with_output()
        .expect("command should exit after stdout closes");
    assert!(
        output.status.success(),
        "command should exit successfully on broken pipe, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("panicked at"),
        "broken pipe should not surface as a panic: {stderr}"
    );
}

// ── Phase 9.19.6: Missing commands ──────────────────────────────────────────

#[test]
fn status_json_output_reports_vault_root_and_note_count() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root.to_str().expect("utf-8"),
            "--output",
            "json",
            "status",
        ])
        .assert()
        .success();

    let out = String::from_utf8_lossy(&assert.get_output().stdout);
    let parsed: Value = serde_json::from_str(out.trim()).expect("status should emit valid JSON");
    assert!(
        parsed.get("vault_root").is_some(),
        "status JSON must include vault_root, got: {out}"
    );
    assert!(
        parsed.get("note_count").is_some(),
        "status JSON must include note_count, got: {out}"
    );
    let note_count = parsed["note_count"]
        .as_u64()
        .expect("note_count should be integer");
    assert!(
        note_count > 0,
        "basic vault should have at least one indexed note"
    );
}

#[test]
fn status_human_output_shows_vault_and_notes() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["--vault", vault_root.to_str().expect("utf-8"), "status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Notes").and(predicate::str::contains("Vault")));
}

#[test]
fn graph_export_json_format_emits_nodes_and_edges() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root.to_str().expect("utf-8"),
            "--output",
            "json",
            "graph",
            "export",
            "--format",
            "json",
        ])
        .assert()
        .success();

    let out = String::from_utf8_lossy(&assert.get_output().stdout);
    let parsed: Value =
        serde_json::from_str(out.trim()).expect("graph export json should be valid JSON");
    assert!(
        parsed.get("nodes").is_some(),
        "json export should have nodes, got: {out}"
    );
    assert!(
        parsed.get("edges").is_some(),
        "json export should have edges, got: {out}"
    );
    let nodes = parsed["nodes"].as_array().expect("nodes should be array");
    assert!(
        !nodes.is_empty(),
        "basic vault should have at least one node"
    );
    // Each node should have id and path
    let first = &nodes[0];
    assert!(first.get("id").is_some(), "node should have id");
    assert!(first.get("path").is_some(), "node should have path");
}

#[test]
fn graph_export_dot_format_emits_digraph_syntax() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root.to_str().expect("utf-8"),
            "graph",
            "export",
            "--format",
            "dot",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("digraph"));
}

#[test]
fn graph_export_graphml_format_emits_xml() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root.to_str().expect("utf-8"),
            "graph",
            "export",
            "--format",
            "graphml",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("<?xml").and(predicate::str::contains("<graphml")));
}

#[test]
fn template_list_subcommand_lists_available_templates() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join(".vulcan/templates"))
        .expect("template dir should be created");
    fs::write(
        vault_root.join(".vulcan/templates/daily.md"),
        "# Daily Note\n\n{{date}}\n",
    )
    .expect("template should be written");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root.to_str().expect("utf-8"),
            "--output",
            "json",
            "template",
            "list",
        ])
        .assert()
        .success();

    let out = String::from_utf8_lossy(&assert.get_output().stdout);
    let parsed: Value =
        serde_json::from_str(out.trim()).expect("template list should emit valid JSON");
    let templates = parsed["templates"]
        .as_array()
        .expect("templates should be an array");
    assert!(!templates.is_empty(), "should list at least one template");
    let names: Vec<&str> = templates
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    assert!(
        names.iter().any(|n| n.contains("daily")),
        "should list a template matching 'daily', got: {names:?}"
    );
}

#[test]
fn template_show_subcommand_displays_template_contents() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join(".vulcan/templates"))
        .expect("template dir should be created");
    fs::write(
        vault_root.join(".vulcan/templates/meeting.md"),
        "# Meeting Notes\n\nDate: {{date}}\n",
    )
    .expect("template should be written");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root.to_str().expect("utf-8"),
            "--output",
            "json",
            "template",
            "show",
            "meeting",
        ])
        .assert()
        .success();

    let out = String::from_utf8_lossy(&assert.get_output().stdout);
    let parsed: Value =
        serde_json::from_str(out.trim()).expect("template show should emit valid JSON");
    let name = parsed["name"].as_str().expect("name should be a string");
    assert!(
        name.contains("meeting"),
        "name should contain 'meeting', got: {name}"
    );
    let content = parsed["content"]
        .as_str()
        .expect("content should be a string");
    assert!(
        content.contains("Meeting Notes"),
        "content should include template body, got: {content}"
    );
}

#[test]
fn periodic_show_subcommand_is_generalized_from_daily_show() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    initialize_vulcan_dir(&vault_root);
    fs::create_dir_all(vault_root.join("Journal/Daily")).expect("daily dir");

    fs::write(
        vault_root.join("Journal/Daily/2026-04-04.md"),
        "---\ntags:\n  - daily\n---\n# 2026-04-04\n\nTest daily note.\n",
    )
    .expect("daily note should write");

    // `periodic show --type daily --date 2026-04-04` should behave like `daily show 2026-04-04`
    cargo_vulcan_fixed_now()
        .args([
            "--vault",
            vault_root.to_str().expect("utf-8"),
            "--output",
            "json",
            "periodic",
            "show",
            "--type",
            "daily",
            "--date",
            "2026-04-04",
        ])
        .assert()
        .success();
}

#[test]
fn periodic_append_subcommand_delegates_to_daily_append() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    initialize_vulcan_dir(&vault_root);
    fs::create_dir_all(vault_root.join("Journal/Daily")).expect("daily dir");
    fs::write(
        vault_root.join("Journal/Daily/2026-04-04.md"),
        "---\ntags:\n  - daily\n---\n# 2026-04-04\n\nOriginal content.\n",
    )
    .expect("daily note should write");

    cargo_vulcan_fixed_now()
        .args([
            "--vault",
            vault_root.to_str().expect("utf-8"),
            "--output",
            "json",
            "periodic",
            "append",
            "Added via periodic append.",
            "--type",
            "daily",
            "--date",
            "2026-04-04",
            "--no-commit",
        ])
        .assert()
        .success();

    let content = fs::read_to_string(vault_root.join("Journal/Daily/2026-04-04.md"))
        .expect("note should still exist");
    assert!(
        content.contains("Added via periodic append."),
        "append should have written text to the note"
    );
}

#[test]
fn mcp_server_negotiates_protocol_and_advertises_native_capabilities() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);

    let mut session = McpSession::start(&vault_root, &[]);
    let messages = session.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "test", "version": "0.0.1" }
        }
    }));
    let response = messages.last().expect("initialize response");

    assert_eq!(response["jsonrpc"].as_str(), Some("2.0"));
    assert_eq!(response["id"].as_u64(), Some(1));
    assert_eq!(
        response["result"]["protocolVersion"].as_str(),
        Some("2025-06-18")
    );
    assert_eq!(
        response["result"]["capabilities"]["tools"]["listChanged"].as_bool(),
        Some(true)
    );
    assert_eq!(
        response["result"]["capabilities"]["prompts"]["listChanged"].as_bool(),
        Some(true)
    );
    assert_eq!(
        response["result"]["capabilities"]["resources"]["listChanged"].as_bool(),
        Some(true)
    );
    assert!(
        response["result"]["capabilities"]
            .get("completions")
            .is_some(),
        "completions capability should be advertised"
    );

    session.send_notification(serde_json::json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized"
    }));
    let trailing = session.finish();
    assert!(
        trailing.is_empty(),
        "initialized notification should not emit a response"
    );
}

#[test]
fn mcp_http_transport_negotiates_sessions_and_calls_tools() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);

    let session = McpHttpSession::start(&vault_root, "/streamable-mcp", None, &[]);
    let initialize = session.post(
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "0.0.1" }
            }
        }),
        None,
    );
    assert_eq!(initialize.status_line, "HTTP/1.1 200 OK");
    let session_id = initialize
        .headers
        .get("mcp-session-id")
        .cloned()
        .expect("initialize should return an MCP session id");
    let initialize_json = initialize.json_body();
    assert_eq!(
        initialize_json["result"]["protocolVersion"].as_str(),
        Some("2025-06-18")
    );

    let tools = session.post(
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list"
        }),
        Some(&session_id),
    );
    assert_eq!(tools.status_line, "HTTP/1.1 200 OK");
    let tools_json = tools.json_body();
    let tools = tools_json["result"]["tools"]
        .as_array()
        .expect("tools/list should return a tool array");
    assert!(tools.iter().any(|tool| tool["name"] == "note_get"));

    let note_get = session.post(
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "note_get",
                "arguments": { "note": "Projects/Alpha.md" }
            }
        }),
        Some(&session_id),
    );
    assert_eq!(note_get.status_line, "HTTP/1.1 200 OK");
    let note_get_json = note_get.json_body();
    assert_eq!(
        note_get_json["result"]["structuredContent"]["path"].as_str(),
        Some("Projects/Alpha.md")
    );

    let delete = session.delete(&session_id);
    assert_eq!(delete.status_line, "HTTP/1.1 204 No Content");

    let after_delete = session.post(
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/list"
        }),
        Some(&session_id),
    );
    assert_eq!(after_delete.status_line, "HTTP/1.1 404 Not Found");
    assert!(
        after_delete.json_body()["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("unknown Mcp-Session-Id")),
        "deleted sessions should no longer accept follow-up requests"
    );
}

#[test]
fn mcp_http_transport_streams_list_changed_notifications_over_sse() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);

    fs::write(vault_root.join("AGENTS.md"), "# Initial\n").expect("agents file should write");
    fs::create_dir_all(vault_root.join("AI/Prompts")).expect("prompts dir");
    let prompt_path = vault_root.join("AI/Prompts/summarize-note.md");
    fs::write(
        &prompt_path,
        r"---
name: summarize-note
---
Summarize {{note}}.
",
    )
    .expect("prompt file should write");

    let session = McpHttpSession::start(&vault_root, "/mcp", None, &[]);
    let initialize = session.post(
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "0.0.1" }
            }
        }),
        None,
    );
    let session_id = initialize
        .headers
        .get("mcp-session-id")
        .cloned()
        .expect("initialize should return a session id");

    let mut sse = session.open_sse(&session_id);
    fs::write(
        &prompt_path,
        r"---
name: summarize-note
description: updated
---
Summarize {{note}} with updated instructions.
",
    )
    .expect("prompt file should update");
    fs::write(vault_root.join("AGENTS.md"), "# Updated\n").expect("agents file should update");

    let first = sse.read_event();
    let second = sse.read_event();
    let methods = [first["method"].clone(), second["method"].clone()];
    assert!(
        methods
            .iter()
            .any(|method| method == "notifications/prompts/list_changed"),
        "SSE stream should include a prompts/list_changed notification"
    );
    assert!(
        methods
            .iter()
            .any(|method| method == "notifications/resources/list_changed"),
        "SSE stream should include a resources/list_changed notification"
    );
}

#[test]
fn mcp_http_transport_adaptive_pack_mutation_refreshes_visible_tools() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);

    let session =
        McpHttpSession::start(&vault_root, "/mcp", None, &["--tool-pack-mode", "adaptive"]);
    let initialize = session.post(
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "0.0.1" }
            }
        }),
        None,
    );
    let session_id = initialize
        .headers
        .get("mcp-session-id")
        .cloned()
        .expect("initialize should return a session id");

    let before = session.post(
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list"
        }),
        Some(&session_id),
    );
    let before_tools = before.json_body()["result"]["tools"]
        .as_array()
        .expect("tools/list should return a tool array")
        .clone();
    assert!(
        before_tools
            .iter()
            .any(|tool| tool["name"] == "tool_pack_enable"),
        "adaptive mode should expose the bootstrap tool-pack mutators"
    );
    assert!(
        !before_tools.iter().any(|tool| tool["name"] == "web_search"),
        "adaptive mode should keep web tools hidden before their pack is enabled"
    );

    let mut sse = session.open_sse(&session_id);
    let enable = session.post(
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "tool_pack_enable",
                "arguments": { "packs": ["web"] }
            }
        }),
        Some(&session_id),
    );
    let enable_json = enable.json_body();
    assert_eq!(
        enable_json["result"]["structuredContent"]["selectedToolPacks"],
        serde_json::json!(["notes-read", "search", "status", "web", "tool-packs"])
    );

    let notification = sse.read_event();
    assert_eq!(
        notification["method"].as_str(),
        Some("notifications/tools/list_changed")
    );

    let after = session.post(
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/list"
        }),
        Some(&session_id),
    );
    let after_tools = after.json_body()["result"]["tools"]
        .as_array()
        .expect("tools/list should return a tool array")
        .clone();
    assert!(
        after_tools.iter().any(|tool| tool["name"] == "web_search"),
        "enabled packs should become visible over HTTP without restarting the session"
    );
}

#[test]
fn mcp_http_transport_enforces_auth_tokens() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);

    let session = McpHttpSession::start(&vault_root, "/secure-mcp", Some("secret-token"), &[]);
    let unauthorized = session.post_with_auth(
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "0.0.1" }
            }
        }),
        None,
        false,
    );
    assert_eq!(unauthorized.status_line, "HTTP/1.1 401 Unauthorized");
    assert!(
        unauthorized.json_body()["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("missing or invalid authentication token")),
        "requests without the configured token should be rejected"
    );

    let authorized = session.post(
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "0.0.1" }
            }
        }),
        None,
    );
    assert_eq!(authorized.status_line, "HTTP/1.1 200 OK");
    assert!(
        authorized.headers.contains_key("mcp-session-id"),
        "authorized initialize should succeed and return a session id"
    );
}

#[test]
fn mcp_server_exposes_default_read_search_status_tools_and_structured_results() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);

    let mut session = McpSession::start(&vault_root, &[]);
    let _ = session.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": { "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "test", "version": "0.0.1" } }
    }));

    let messages = session.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list"
    }));
    let tools = messages
        .last()
        .and_then(|response| response["result"]["tools"].as_array())
        .expect("tools/list should return a tool array");

    for expected in ["note_get", "note_outline", "search", "status"] {
        assert!(
            tools.iter().any(|tool| tool["name"] == expected),
            "default pack selection should expose `{expected}`"
        );
    }
    for hidden in [
        "note_create",
        "note_append",
        "note_patch",
        "note_info",
        "note_set",
        "note_delete",
        "web_search",
        "web_fetch",
        "config_show",
        "index_scan",
        "browse",
        "edit",
        "open",
        "bases_tui",
        "mcp",
    ] {
        assert!(
            !tools.iter().any(|tool| tool["name"] == hidden),
            "default pack selection should not expose `{hidden}`"
        );
    }

    let note_get = tools
        .iter()
        .find(|tool| tool["name"] == "note_get")
        .expect("note_get tool should exist");
    assert_eq!(note_get["title"].as_str(), Some("Read Note Content"));
    assert_eq!(note_get["toolPacks"], serde_json::json!(["notes-read"]));
    assert_eq!(
        note_get["annotations"]["readOnlyHint"].as_bool(),
        Some(true)
    );
    assert!(
        note_get.get("outputSchema").is_some(),
        "note_get should advertise an output schema"
    );

    let messages = session.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {
            "name": "note_get",
            "arguments": { "note": "Projects/Alpha.md" }
        }
    }));
    let result = &messages.last().expect("tools/call response")["result"];
    assert_eq!(result["isError"].as_bool(), Some(false));
    assert_eq!(
        result["structuredContent"]["path"].as_str(),
        Some("Projects/Alpha.md")
    );
    assert_eq!(result["content"][0]["type"].as_str(), Some("text"));
    assert!(session.finish().is_empty());
}

#[test]
fn mcp_server_exposes_custom_tools_and_tool_resources_when_custom_pack_selected() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    fs::create_dir_all(vault_root.join(".agents/tools/summarize")).expect("tool dir should exist");
    fs::write(
        vault_root.join(".agents/tools/summarize/TOOL.md"),
        r"---
name: summarize_tool
title: Summarize Tool
description: Summarize one note.
input_schema:
  type: object
  additionalProperties: false
  properties:
    note:
      type: string
  required:
    - note
---

Summarize tool documentation.
",
    )
    .expect("manifest should write");
    fs::write(
        vault_root.join(".agents/tools/summarize/main.js"),
        "function main(input) {\n  return { result: { note: input.note, upper: String(input.note).toUpperCase() }, text: `summarized ${input.note}` };\n}\n",
    )
    .expect("entrypoint should write");

    let config_home = temp_dir.path().join("config");
    fs::create_dir_all(&config_home).expect("config home should exist");
    let vault_root_str = vault_root.to_str().expect("utf-8").to_string();
    let config_home_str = config_home.to_str().expect("utf-8").to_string();
    trust_and_scan_vault(&config_home_str, &vault_root_str);

    let mut session =
        start_mcp_session_with_xdg(&vault_root, &config_home_str, &["--tool-pack", "custom"]);
    let _ = session.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": { "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "test", "version": "0.0.1" } }
    }));

    let tools = session.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list"
    }));
    let tools = tools
        .last()
        .and_then(|response| response["result"]["tools"].as_array())
        .expect("tools/list should return a tool array");
    assert!(tools.iter().any(|tool| tool["name"] == "summarize_tool"));

    let resources = session.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "resources/list"
    }));
    let resources = resources
        .last()
        .and_then(|response| response["result"]["resources"].as_array())
        .expect("resources/list should return resources");
    assert!(resources
        .iter()
        .any(|resource| resource["uri"] == "vulcan://assistant/tools/index"));

    let index = session.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "resources/read",
        "params": { "uri": "vulcan://assistant/tools/index" }
    }));
    assert!(index
        .last()
        .and_then(|response| response["result"]["contents"][0]["text"].as_str())
        .is_some_and(|text| text.contains("summarize_tool")));

    let detail = session.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 5,
        "method": "resources/read",
        "params": { "uri": "vulcan://assistant/tools/summarize_tool" }
    }));
    assert!(detail
        .last()
        .and_then(|response| response["result"]["contents"][0]["text"].as_str())
        .is_some_and(|text| text.contains("Summarize tool documentation.")));

    let result = session.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 6,
        "method": "tools/call",
        "params": {
            "name": "summarize_tool",
            "arguments": { "note": "Projects/Alpha.md" }
        }
    }));
    let result = &result.last().expect("tools/call response")["result"];
    assert_eq!(result["isError"].as_bool(), Some(false));
    assert_eq!(
        result["structuredContent"]["note"].as_str(),
        Some("Projects/Alpha.md")
    );
    assert_eq!(
        result["content"][0]["text"].as_str(),
        Some("summarized Projects/Alpha.md")
    );
    assert!(session.finish().is_empty());
}

#[test]
fn mcp_custom_pack_hides_profile_denied_custom_tools() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    fs::create_dir_all(vault_root.join(".agents/tools/shell")).expect("tool dir should exist");
    fs::create_dir_all(vault_root.join(".vulcan")).expect("config dir should exist");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        r#"
[permissions.profiles.readonly]
read = "all"
write = "none"
refactor = "none"
git = "deny"
network = "deny"
index = "deny"
config = "read"
execute = "allow"
shell = "deny"

[permissions.profiles.sheller]
read = "all"
write = "none"
refactor = "none"
git = "deny"
network = "deny"
index = "deny"
config = "read"
execute = "allow"
shell = "allow"
"#,
    )
    .expect("config should write");
    fs::write(
        vault_root.join(".agents/tools/shell/TOOL.md"),
        r"---
name: shell_tool
description: Requires shell permission.
permission_profile: sheller
input_schema:
  type: object
---

Shell tool documentation.
",
    )
    .expect("manifest should write");
    fs::write(
        vault_root.join(".agents/tools/shell/main.js"),
        "function main() { return { ok: true }; }\n",
    )
    .expect("entrypoint should write");

    let config_home = temp_dir.path().join("config");
    fs::create_dir_all(&config_home).expect("config home should exist");
    let vault_root_str = vault_root.to_str().expect("utf-8").to_string();
    let config_home_str = config_home.to_str().expect("utf-8").to_string();
    trust_and_scan_vault(&config_home_str, &vault_root_str);

    let mut session = start_mcp_session_with_xdg(
        &vault_root,
        &config_home_str,
        &["--permissions", "readonly", "--tool-pack", "custom"],
    );
    let _ = session.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": { "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "test", "version": "0.0.1" } }
    }));

    let tools = session.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list"
    }));
    let tools = tools
        .last()
        .and_then(|response| response["result"]["tools"].as_array())
        .expect("tools/list should return a tool array");
    assert!(
        !tools.iter().any(|tool| tool["name"] == "shell_tool"),
        "custom pack should hide tools whose declared profile is broader than the active profile"
    );

    let resources = session.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "resources/list"
    }));
    let resources = resources
        .last()
        .and_then(|response| response["result"]["resources"].as_array())
        .expect("resources/list should return resources");
    assert!(
        !resources
            .iter()
            .any(|resource| resource["uri"] == "vulcan://assistant/tools/index"),
        "hidden custom tools should not expose the custom-tool index resource"
    );

    let result = session.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "tools/call",
        "params": {
            "name": "shell_tool",
            "arguments": {}
        }
    }));
    let result = &result.last().expect("tools/call response")["result"];
    assert_eq!(result["isError"].as_bool(), Some(true));
    assert!(
        result["content"][0]["text"]
            .as_str()
            .is_some_and(|text| text.contains(
                "permission denied: tool `shell_tool` is not available under profile `readonly`"
            )),
        "profile-denied custom tools should stay unavailable even when the custom pack is enabled"
    );
    assert!(session.finish().is_empty());
}

#[test]
fn mcp_server_composes_requested_canonical_tool_packs() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);

    let mut session = McpSession::start(
        &vault_root,
        &[
            "--tool-pack",
            "notes-read,notes-manage",
            "--tool-pack",
            "web",
        ],
    );
    let _ = session.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": { "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "test", "version": "0.0.1" } }
    }));

    let tools = session.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list"
    }));
    let tools = tools
        .last()
        .and_then(|response| response["result"]["tools"].as_array())
        .expect("tools/list should return a tool array");

    for expected in [
        "note_get",
        "note_outline",
        "note_info",
        "note_set",
        "note_delete",
        "web_search",
        "web_fetch",
    ] {
        assert!(
            tools.iter().any(|tool| tool["name"] == expected),
            "composed tool packs should expose `{expected}`"
        );
    }
    for hidden in [
        "search",
        "status",
        "note_create",
        "config_show",
        "index_scan",
    ] {
        assert!(
            !tools.iter().any(|tool| tool["name"] == hidden),
            "unselected packs should keep `{hidden}` hidden"
        );
    }

    assert!(session.finish().is_empty());
}

#[test]
fn mcp_adaptive_tool_pack_tools_expand_visible_registry() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);

    let mut session = McpSession::start(&vault_root, &["--tool-pack-mode", "adaptive"]);
    let _ = session.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": { "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "test", "version": "0.0.1" } }
    }));

    let tools = session.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list"
    }));
    let tools = tools
        .last()
        .and_then(|response| response["result"]["tools"].as_array())
        .expect("tools/list should return a tool array");
    for expected in [
        "note_get",
        "note_outline",
        "search",
        "status",
        "tool_pack_list",
        "tool_pack_enable",
        "tool_pack_disable",
        "tool_pack_set",
    ] {
        assert!(
            tools.iter().any(|tool| tool["name"] == expected),
            "adaptive mode should expose `{expected}`"
        );
    }
    for hidden in ["web_search", "note_create", "config_show", "index_scan"] {
        assert!(
            !tools.iter().any(|tool| tool["name"] == hidden),
            "adaptive mode should not expose `{hidden}` before enabling its pack"
        );
    }

    let state = session.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {
            "name": "tool_pack_list",
            "arguments": {}
        }
    }));
    let state = &state.last().expect("tool_pack_list response")["result"]["structuredContent"];
    assert_eq!(state["mode"].as_str(), Some("adaptive"));
    assert_eq!(
        state["selectedToolPacks"],
        serde_json::json!(["notes-read", "search", "status", "tool-packs"])
    );

    let enable = session.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "tools/call",
        "params": {
            "name": "tool_pack_enable",
            "arguments": { "packs": ["web", "notes-write"] }
        }
    }));
    assert!(
        enable
            .iter()
            .any(|message| message["method"] == "notifications/tools/list_changed"),
        "enabling new tool packs should emit tools/list_changed"
    );
    let state = &enable.last().expect("tool_pack_enable response")["result"]["structuredContent"];
    assert_eq!(
        state["selectedToolPacks"],
        serde_json::json!([
            "notes-read",
            "search",
            "status",
            "notes-write",
            "web",
            "tool-packs",
        ])
    );

    let tools = session.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 5,
        "method": "tools/list"
    }));
    let tools = tools
        .last()
        .and_then(|response| response["result"]["tools"].as_array())
        .expect("tools/list should return a tool array");
    for expected in [
        "tool_pack_list",
        "tool_pack_enable",
        "tool_pack_disable",
        "tool_pack_set",
        "note_create",
        "note_append",
        "note_patch",
        "web_search",
        "web_fetch",
    ] {
        assert!(
            tools.iter().any(|tool| tool["name"] == expected),
            "enabled packs should expose `{expected}`"
        );
    }

    assert!(session.finish().is_empty());
}

#[test]
fn mcp_adaptive_pack_schema_includes_custom_selector() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);

    let describe_assert = cargo_vulcan_fixed_now()
        .args([
            "--vault",
            vault_root.to_str().expect("utf-8"),
            "--output",
            "json",
            "describe",
            "--format",
            "mcp",
            "--tool-pack-mode",
            "adaptive",
        ])
        .assert()
        .success();
    let describe_tool = parse_stdout_json(&describe_assert)["tools"]
        .as_array()
        .expect("describe should return tool definitions")
        .iter()
        .find(|tool| tool["name"] == "tool_pack_enable")
        .cloned()
        .expect("tool_pack_enable should be exported");
    let describe_enum = describe_tool["inputSchema"]["properties"]["packs"]["items"]["enum"]
        .as_array()
        .expect("describe input schema should expose an enum");
    assert!(
        describe_enum
            .iter()
            .any(|value| value.as_str() == Some("custom")),
        "describe --format mcp should advertise the custom tool pack as a valid selector"
    );

    let mut session = McpSession::start(&vault_root, &["--tool-pack-mode", "adaptive"]);
    let _ = session.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": { "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "test", "version": "0.0.1" } }
    }));
    let tools = session.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list"
    }));
    let live_tool = tools
        .last()
        .and_then(|response| response["result"]["tools"].as_array())
        .expect("tools/list should return a tool array")
        .iter()
        .find(|tool| tool["name"] == "tool_pack_enable")
        .cloned()
        .expect("tool_pack_enable should be visible in adaptive mode");
    let live_enum = live_tool["inputSchema"]["properties"]["packs"]["items"]["enum"]
        .as_array()
        .expect("live input schema should expose an enum");
    assert!(
        live_enum
            .iter()
            .any(|value| value.as_str() == Some("custom")),
        "live MCP tool definitions should advertise the custom pack selector too"
    );
    assert!(session.finish().is_empty());
}

#[test]
fn mcp_server_surfaces_prompts_resources_and_completions() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);

    fs::write(
        vault_root.join("AGENTS.md"),
        "# Vault instructions\n\nBe precise.\n",
    )
    .expect("agents file should write");
    fs::create_dir_all(vault_root.join("AI/Prompts")).expect("prompts dir");
    fs::write(
        vault_root.join("AI/Prompts/summarize-note.md"),
        r"---
name: summarize-note
title: Summarize Note
description: Summarize one note
arguments:
  - name: note
    description: Note to summarize
    required: true
    completion: note
---
Summarize {{note}}.
",
    )
    .expect("prompt file should write");
    fs::create_dir_all(vault_root.join(".agents/skills/daily-review")).expect("skills dir");
    fs::write(
        vault_root.join(".agents/skills/daily-review/SKILL.md"),
        r"---
name: daily-review
description: Review the day
tools:
  - note_get
---
Use this skill to review the day.
",
    )
    .expect("skill file should write");

    let mut session = McpSession::start(&vault_root, &[]);
    let _ = session.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": { "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "test", "version": "0.0.1" } }
    }));

    let prompts = session.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "prompts/list"
    }));
    let prompts = prompts
        .last()
        .and_then(|response| response["result"]["prompts"].as_array())
        .expect("prompts/list should return prompt definitions");
    assert!(prompts
        .iter()
        .any(|prompt| prompt["name"] == "summarize-note"));

    let prompt = session.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "prompts/get",
        "params": {
            "name": "summarize-note",
            "arguments": { "note": "Projects/Alpha.md" }
        }
    }));
    let prompt_text = prompt
        .last()
        .and_then(|response| response["result"]["messages"][0]["content"]["text"].as_str())
        .expect("prompts/get should return text content");
    assert!(
        prompt_text.contains("Projects/Alpha.md"),
        "rendered prompt should include the provided note argument"
    );

    let resources = session.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "resources/list"
    }));
    let resources = resources
        .last()
        .and_then(|response| response["result"]["resources"].as_array())
        .expect("resources/list should return resources");
    let resource_uris = resources
        .iter()
        .filter_map(|resource| resource["uri"].as_str())
        .collect::<Vec<_>>();
    for uri in [
        "vulcan://help/overview",
        "vulcan://assistant/prompts/index",
        "vulcan://assistant/skills/index",
        "vulcan://assistant/agents",
        "vulcan://assistant/config",
    ] {
        assert!(
            resource_uris.contains(&uri),
            "resources/list should expose `{uri}`"
        );
    }

    let templates = session.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 5,
        "method": "resources/templates/list"
    }));
    let templates = templates
        .last()
        .and_then(|response| response["result"]["resourceTemplates"].as_array())
        .expect("resources/templates/list should return templates");
    assert!(templates
        .iter()
        .any(|template| template["uriTemplate"] == "vulcan://help/{topic}"));
    assert!(templates
        .iter()
        .any(|template| template["uriTemplate"] == "vulcan://assistant/skills/{name}"));

    let help = session.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 6,
        "method": "resources/read",
        "params": { "uri": "vulcan://help/overview" }
    }));
    let help_text = help
        .last()
        .and_then(|response| response["result"]["contents"][0]["text"].as_str())
        .expect("resources/read should return help JSON text");
    let help_json: Value = serde_json::from_str(help_text).expect("help resource should be JSON");
    assert_eq!(help_json["name"].as_str(), Some("help"));

    let note_completion = session.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 7,
        "method": "completion/complete",
        "params": {
            "ref": { "type": "ref/prompt", "name": "summarize-note" },
            "argument": { "name": "note", "value": "Pro" }
        }
    }));
    let note_values = note_completion
        .last()
        .and_then(|response| response["result"]["completion"]["values"].as_array())
        .expect("prompt completion should return values");
    assert!(
        note_values.iter().any(|value| {
            value
                .as_str()
                .is_some_and(|value| value.contains("Projects/Alpha"))
        }),
        "prompt completion should include note suggestions"
    );

    let help_completion = session.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 8,
        "method": "completion/complete",
        "params": {
            "ref": { "type": "ref/resource", "uri": "vulcan://help/{topic}" },
            "argument": { "name": "topic", "value": "note/" }
        }
    }));
    let help_values = help_completion
        .last()
        .and_then(|response| response["result"]["completion"]["values"].as_array())
        .expect("resource completion should return values");
    assert!(
        help_values
            .iter()
            .any(|value| value.as_str() == Some("note/get")),
        "resource completion should include command-topic help entries"
    );
    assert!(session.finish().is_empty());
}

#[test]
fn mcp_server_permission_filters_resources_prompts_and_hidden_tools() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);

    fs::create_dir_all(vault_root.join(".vulcan")).expect("config dir should exist");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        r#"[permissions.profiles.blind]
read = "none"
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
    fs::write(vault_root.join("AGENTS.md"), "# Hidden\n").expect("agents file should write");
    fs::create_dir_all(vault_root.join("AI/Prompts")).expect("prompts dir");
    fs::write(
        vault_root.join("AI/Prompts/summarize-note.md"),
        r"---
name: summarize-note
arguments:
  - name: note
    required: true
    completion: note
---
Summarize {{note}}.
",
    )
    .expect("prompt file should write");
    fs::create_dir_all(vault_root.join(".agents/skills/daily-review")).expect("skills dir");
    fs::write(
        vault_root.join(".agents/skills/daily-review/SKILL.md"),
        "Use this skill.\n",
    )
    .expect("skill file should write");

    let mut session = McpSession::start(
        &vault_root,
        &[
            "--permissions",
            "blind",
            "--tool-pack",
            "notes-read,notes-write,web,config",
        ],
    );
    let _ = session.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": { "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "test", "version": "0.0.1" } }
    }));

    let tools = session.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list"
    }));
    let tools = tools
        .last()
        .and_then(|response| response["result"]["tools"].as_array())
        .expect("tools/list should return tools");
    for hidden in ["note_get", "note_create", "web_search", "config_show"] {
        assert!(
            !tools.iter().any(|tool| tool["name"] == hidden),
            "blind profile should hide `{hidden}`"
        );
    }

    let prompts = session.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "prompts/list"
    }));
    let prompts = prompts
        .last()
        .and_then(|response| response["result"]["prompts"].as_array())
        .expect("prompts/list should return a prompt array");
    assert!(prompts.is_empty(), "blind profile should hide prompts");

    let resources = session.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "resources/list"
    }));
    let resources = resources
        .last()
        .and_then(|response| response["result"]["resources"].as_array())
        .expect("resources/list should return resources");
    let resource_uris = resources
        .iter()
        .filter_map(|resource| resource["uri"].as_str())
        .collect::<Vec<_>>();
    assert!(resource_uris.contains(&"vulcan://help/overview"));
    for hidden in [
        "vulcan://assistant/agents",
        "vulcan://assistant/prompts/index",
        "vulcan://assistant/skills/index",
        "vulcan://assistant/config",
    ] {
        assert!(
            !resource_uris.contains(&hidden),
            "blind profile should hide `{hidden}`"
        );
    }

    let note_get = session.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 5,
        "method": "tools/call",
        "params": {
            "name": "note_get",
            "arguments": { "note": "Projects/Alpha.md" }
        }
    }));
    let note_get = note_get.last().expect("note_get response");
    assert_eq!(note_get["result"]["isError"].as_bool(), Some(true));
    assert!(
        note_get["result"]["content"][0]["text"]
            .as_str()
            .is_some_and(|text| text.contains(
                "permission denied: tool `note_get` requires read access under profile `blind`"
            )),
        "hidden tool calls should fail with a permission error"
    );

    let prompt = session.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 6,
        "method": "prompts/get",
        "params": {
            "name": "summarize-note",
            "arguments": { "note": "Projects/Alpha.md" }
        }
    }));
    assert!(
        prompt
            .last()
            .and_then(|response| response["error"]["message"].as_str())
            .is_some_and(|message| message.contains("not available under profile `blind`")),
        "hidden prompts should reject direct access"
    );

    let agents = session.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 7,
        "method": "resources/read",
        "params": { "uri": "vulcan://assistant/agents" }
    }));
    let agents = agents.last().expect("agents response");
    assert_eq!(agents["error"]["code"].as_i64(), Some(-32002));
    assert!(
        agents["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("permission denied")),
        "hidden resources should reject direct access"
    );
    assert!(session.finish().is_empty());
}

#[test]
fn mcp_server_accepts_reserved_meta_on_list_requests() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);

    let mut session = McpSession::start(&vault_root, &[]);
    let _ = session.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": { "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "test", "version": "0.0.1" } }
    }));

    let tools = session.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list",
        "params": {
            "_meta": { "progressToken": 1 }
        }
    }));
    let tools = tools
        .last()
        .and_then(|response| response["result"]["tools"].as_array())
        .expect("tools/list should accept reserved _meta params");
    assert!(tools.iter().any(|tool| tool["name"] == "note_get"));

    let resources = session.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "resources/list",
        "params": {
            "_meta": { "progressToken": 2 }
        }
    }));
    let resources = resources
        .last()
        .and_then(|response| response["result"]["resources"].as_array())
        .expect("resources/list should accept reserved _meta params");
    assert!(resources
        .iter()
        .any(|resource| resource["uri"] == "vulcan://help/overview"));

    assert!(session.finish().is_empty());
}

#[test]
fn mcp_server_emits_prompt_and_resource_list_changed_notifications() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);

    fs::write(vault_root.join("AGENTS.md"), "# Initial\n").expect("agents file should write");
    fs::create_dir_all(vault_root.join("AI/Prompts")).expect("prompts dir");
    let prompt_path = vault_root.join("AI/Prompts/summarize-note.md");
    fs::write(
        &prompt_path,
        r"---
name: summarize-note
---
Summarize {{note}}.
",
    )
    .expect("prompt file should write");

    let mut session = McpSession::start(&vault_root, &[]);
    let _ = session.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": { "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "test", "version": "0.0.1" } }
    }));
    let _ = session.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "prompts/list"
    }));

    fs::write(
        &prompt_path,
        r"---
name: summarize-note
description: updated
---
Summarize {{note}} with new instructions.
",
    )
    .expect("updated prompt should write");
    fs::write(vault_root.join("AGENTS.md"), "# Updated\n").expect("agents file should update");

    let messages = session.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "prompts/list"
    }));
    assert!(
        messages
            .iter()
            .any(|message| message["method"] == "notifications/prompts/list_changed"),
        "prompt changes should emit a prompts/list_changed notification"
    );
    assert!(
        messages
            .iter()
            .any(|message| message["method"] == "notifications/resources/list_changed"),
        "assistant file changes should emit a resources/list_changed notification"
    );
    assert!(session.finish().is_empty());
}

#[test]
fn mcp_server_emits_tool_and_resource_list_changed_notifications_for_custom_tools() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    fs::create_dir_all(vault_root.join(".agents/tools/summarize")).expect("tool dir should exist");
    let manifest_path = vault_root.join(".agents/tools/summarize/TOOL.md");
    fs::write(
        &manifest_path,
        r"---
name: summarize_tool
description: Summarize one note.
input_schema:
  type: object
---
",
    )
    .expect("manifest should write");
    fs::write(
        vault_root.join(".agents/tools/summarize/main.js"),
        "function main() { return { ok: true }; }\n",
    )
    .expect("entrypoint should write");

    let config_home = temp_dir.path().join("config");
    fs::create_dir_all(&config_home).expect("config home should exist");
    let vault_root_str = vault_root.to_str().expect("utf-8").to_string();
    let config_home_str = config_home.to_str().expect("utf-8").to_string();
    trust_and_scan_vault(&config_home_str, &vault_root_str);

    let mut session =
        start_mcp_session_with_xdg(&vault_root, &config_home_str, &["--tool-pack", "custom"]);
    let _ = session.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": { "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "test", "version": "0.0.1" } }
    }));
    let _ = session.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list"
    }));

    fs::write(
        &manifest_path,
        r"---
name: summarize_tool
description: Updated summary tool.
input_schema:
  type: object
---
",
    )
    .expect("updated manifest should write");

    let messages = session.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/list"
    }));
    assert!(
        messages
            .iter()
            .any(|message| message["method"] == "notifications/tools/list_changed"),
        "custom tool changes should emit a tools/list_changed notification"
    );
    assert!(
        messages
            .iter()
            .any(|message| message["method"] == "notifications/resources/list_changed"),
        "custom tool documentation changes should emit a resources/list_changed notification"
    );
    assert!(session.finish().is_empty());
}

#[test]
fn mcp_structured_outputs_match_cli_json_reports() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);

    let mut session = McpSession::start(&vault_root, &[]);
    let _ = session.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": { "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "test", "version": "0.0.1" } }
    }));

    let note_get = session.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": "note_get",
            "arguments": { "note": "Projects/Alpha.md" }
        }
    }));
    let mcp_note_get =
        note_get.last().expect("note_get response")["result"]["structuredContent"].clone();
    let cli_note_get: Value = serde_json::from_slice(
        &cargo_vulcan_fixed_now()
            .args([
                "--vault",
                vault_root.to_str().expect("utf-8"),
                "--output",
                "json",
                "note",
                "get",
                "Projects/Alpha.md",
            ])
            .assert()
            .success()
            .get_output()
            .stdout,
    )
    .expect("cli note get json should parse");
    assert_eq!(mcp_note_get, cli_note_get);

    let status = session.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {
            "name": "status",
            "arguments": {}
        }
    }));
    let mcp_status = status.last().expect("status response")["result"]["structuredContent"].clone();
    let cli_status: Value = serde_json::from_slice(
        &cargo_vulcan_fixed_now()
            .args([
                "--vault",
                vault_root.to_str().expect("utf-8"),
                "--output",
                "json",
                "status",
            ])
            .assert()
            .success()
            .get_output()
            .stdout,
    )
    .expect("cli status json should parse");
    assert_eq!(mcp_status, cli_status);
    assert!(session.finish().is_empty());
}

#[test]
fn note_get_html_uses_shared_html_renderer() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("dataview", &vault_root);
    run_scan(&vault_root);

    let output = cargo_vulcan_fixed_now()
        .args([
            "--vault",
            vault_root.to_str().expect("utf-8"),
            "note",
            "get",
            "Dashboard",
            "--mode",
            "html",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let html = String::from_utf8(output).expect("html output should be utf-8");

    assert!(html.contains("<h2 id=\"lists\">Lists</h2>"));
    assert!(html.contains("class=\"dataview-inline-field\""));
    assert!(html.contains("class=\"dql-table\""));
    assert!(html.contains("DataviewJS disabled"));
}

#[test]
fn render_html_uses_shared_html_renderer_for_vault_files() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("dataview", &vault_root);
    run_scan(&vault_root);
    let dashboard = vault_root.join("Dashboard.md");

    let output = cargo_vulcan_fixed_now()
        .args([
            "--vault",
            vault_root.to_str().expect("utf-8"),
            "render",
            "--mode",
            "html",
            dashboard.to_str().expect("utf-8"),
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let html = String::from_utf8(output).expect("html output should be utf-8");

    assert!(html.contains("<h2 id=\"lists\">Lists</h2>"));
    assert!(html.contains("class=\"dql-table\""));
    assert!(html.contains("DataviewJS disabled"));
}

#[test]
fn describe_mcp_matches_live_registry_for_same_pack_selection() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);

    let describe_assert = cargo_vulcan_fixed_now()
        .args([
            "--vault",
            vault_root.to_str().expect("utf-8"),
            "--output",
            "json",
            "describe",
            "--format",
            "mcp",
            "--tool-pack",
            "notes-read,search,web",
        ])
        .assert()
        .success();
    let mut describe_tools = parse_stdout_json(&describe_assert)["tools"]
        .as_array()
        .expect("describe should return a tool array")
        .iter()
        .map(|tool| {
            serde_json::json!({
                "name": tool["name"],
                "title": tool["title"],
                "description": tool["description"],
                "inputSchema": tool["inputSchema"],
                "outputSchema": tool["outputSchema"],
                "annotations": tool["annotations"],
                "toolPacks": tool["toolPacks"],
            })
        })
        .collect::<Vec<_>>();
    describe_tools.sort_by(|left, right| left["name"].as_str().cmp(&right["name"].as_str()));

    let mut session = McpSession::start(&vault_root, &["--tool-pack", "notes-read,search,web"]);
    let _ = session.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": { "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "test", "version": "0.0.1" } }
    }));
    let tools = session.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list"
    }));
    let mut live_tools = tools
        .last()
        .and_then(|response| response["result"]["tools"].as_array())
        .expect("tools/list should return a tool array")
        .clone();
    live_tools.sort_by(|left, right| left["name"].as_str().cmp(&right["name"].as_str()));

    assert_eq!(
        describe_tools, live_tools,
        "describe --format mcp and live MCP exposure should stay in sync for the same tool-pack selection"
    );
    assert!(session.finish().is_empty());
}

#[test]
fn mcp_server_filters_and_rejects_tools_under_readonly_permissions() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);

    let mut session = McpSession::start(
        &vault_root,
        &[
            "--permissions",
            "readonly",
            "--tool-pack",
            "notes-read,notes-manage,web,index",
        ],
    );
    let _ = session.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": { "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "test", "version": "0.0.1" } }
    }));

    let tools = session.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list"
    }));
    let tools = tools
        .last()
        .and_then(|response| response["result"]["tools"].as_array())
        .expect("tools/list should return a tool array");
    assert!(tools.iter().any(|tool| tool["name"] == "note_get"));
    assert!(!tools.iter().any(|tool| tool["name"] == "note_set"));
    assert!(!tools.iter().any(|tool| tool["name"] == "web_search"));
    assert!(!tools.iter().any(|tool| tool["name"] == "index_scan"));

    let note_get = session.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {
            "name": "note_get",
            "arguments": { "note": "Projects/Alpha.md" }
        }
    }));
    assert!(
        note_get
            .last()
            .is_some_and(|response| response.get("result").is_some()),
        "note_get should remain callable under readonly permissions"
    );

    let note_set = session.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "tools/call",
        "params": {
            "name": "note_set",
            "arguments": { "note": "Projects/Alpha.md", "content": "updated" }
        }
    }));
    let note_set = note_set.last().expect("note_set response");
    assert_eq!(note_set["result"]["isError"].as_bool(), Some(true));
    assert_eq!(
        note_set["result"]["content"][0]["text"].as_str(),
        Some("permission denied: tool `note_set` requires write access under profile `readonly`")
    );
    assert!(session.finish().is_empty());
}

#[test]
fn readonly_cli_profile_rejects_note_writes() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root.to_str().expect("utf-8"),
            "--permissions",
            "readonly",
            "note",
            "create",
            "Scratch",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "permission denied: profile `readonly` does not allow write `Scratch`",
        ));
}

#[test]
#[allow(clippy::too_many_lines)]
fn sandboxed_cli_profile_rejects_refactor_git_network_config_execute_and_index_commands() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    init_git_repo(&vault_root);
    fs::create_dir_all(vault_root.join(".vulcan")).expect("config dir should exist");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        r#"[permissions.profiles.sandboxed]
read = "all"
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

    let vault_root_str = vault_root.to_str().expect("utf-8").to_string();

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "--permissions",
            "sandboxed",
            "refactor",
            "move",
            "Projects/Alpha.md",
            "Archive/Alpha.md",
            "--dry-run",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "permission denied: profile `sandboxed` does not allow refactor `Projects/Alpha.md`",
        ));

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "--permissions",
            "sandboxed",
            "scan",
            "--full",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "permission denied: profile `sandboxed` does not allow index access",
        ));

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "--permissions",
            "sandboxed",
            "web",
            "fetch",
            "https://example.com/article",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "permission denied: profile `sandboxed` does not allow network access to `https://example.com/article`",
        ));

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "--permissions",
            "sandboxed",
            "git",
            "status",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "permission denied: profile `sandboxed` does not allow git access",
        ));

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "--permissions",
            "sandboxed",
            "config",
            "show",
            "permissions",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "permission denied: profile `sandboxed` does not allow config read access",
        ));

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "--permissions",
            "sandboxed",
            "plugin",
            "enable",
            "lint",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "permission denied: profile `sandboxed` does not allow config write access",
        ));

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "--permissions",
            "sandboxed",
            "plugin",
            "run",
            "lint",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "permission denied: profile `sandboxed` does not allow execute access",
        ));
}

#[test]
fn permission_profile_filters_query_results() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    fs::create_dir_all(vault_root.join(".vulcan")).expect("config dir should exist");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        r#"[permissions.profiles.projects_only]
read = { allow = ["folder:Projects/**"] }
"#,
    )
    .expect("config should be written");
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root.to_str().expect("utf-8"),
            "--permissions",
            "projects_only",
            "--output",
            "json",
            "query",
        ])
        .assert()
        .success();
    let rows = parse_stdout_json_lines(&assert);
    let paths = rows
        .iter()
        .map(|note| {
            note["document_path"]
                .as_str()
                .expect("document path should be a string")
                .to_string()
        })
        .collect::<Vec<_>>();

    assert_eq!(paths, vec!["Projects/Alpha.md".to_string()]);
}

#[test]
fn config_show_permissions_lists_active_and_available_profiles() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    fs::create_dir_all(vault_root.join(".vulcan")).expect("config dir should exist");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        r#"[permissions.profiles.projects_only]
read = { allow = ["folder:Projects/**"] }
"#,
    )
    .expect("config should be written");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root.to_str().expect("utf-8"),
            "--output",
            "json",
            "config",
            "show",
            "permissions",
        ])
        .assert()
        .success();
    let report = parse_stdout_json(&assert);
    let available = report["available_permission_profiles"]
        .as_array()
        .expect("available profiles should be an array")
        .iter()
        .map(|value| value.as_str().expect("profile should be a string"))
        .collect::<Vec<_>>();

    assert_eq!(
        report["active_permission_profile"].as_str(),
        Some("unrestricted")
    );
    assert!(available.contains(&"projects_only"));
    assert!(available.contains(&"readonly"));
    assert!(available.contains(&"unrestricted"));
    assert!(report["config"]["profiles"]["projects_only"].is_object());
}

#[test]
fn policy_hooks_can_deny_reads_after_static_profile_checks() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join(".vulcan/plugins")).expect("plugin dir should exist");
    fs::write(vault_root.join("Home.md"), "home\n").expect("home note should write");
    fs::write(vault_root.join("Secrets.md"), "secret\n").expect("secret note should write");
    fs::write(
        vault_root.join(".vulcan/plugins/guard.js"),
        r#"
function policy_hook(input) {
  if (input.action === "read" && input.resource === "Secrets.md") {
    return { decision: "deny", reason: "secret note blocked" };
  }
  return "pass";
}
"#,
    )
    .expect("policy hook should write");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        r#"[permissions.profiles.guarded]
read = "all"
write = "none"
refactor = "none"
git = "deny"
network = "deny"
index = "deny"
config = "read"
execute = "deny"
shell = "deny"
policy_hook = ".vulcan/plugins/guard.js"
"#,
    )
    .expect("config should be written");

    let config_home = temp_dir.path().join("xdg");
    fs::create_dir_all(&config_home).expect("xdg dir should exist");
    let config_home_str = config_home
        .to_str()
        .expect("config home path should be valid utf-8")
        .to_string();
    let vault_root_str = vault_root
        .to_str()
        .expect("vault path should be valid utf-8")
        .to_string();

    trust_and_scan_vault(&config_home_str, &vault_root_str);

    cargo_vulcan_with_xdg_config(&config_home_str)
        .args([
            "--vault",
            &vault_root_str,
            "--permissions",
            "guarded",
            "note",
            "get",
            "Secrets.md",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("secret note blocked"));

    let assert = cargo_vulcan_with_xdg_config(&config_home_str)
        .args([
            "--vault",
            &vault_root_str,
            "--permissions",
            "guarded",
            "--output",
            "json",
            "note",
            "get",
            "Home.md",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);
    assert_eq!(json["path"], "Home.md");
}

#[test]
fn complete_note_context_returns_note_paths() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);

    let output = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "complete",
            "note",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8_lossy(&output);
    assert!(
        text.lines().count() > 0,
        "complete note should return at least one candidate"
    );
    // All output lines should look like filenames or paths
    for line in text.lines() {
        assert!(!line.is_empty(), "no blank lines in completion output");
    }
}

#[test]
fn complete_daily_date_context_returns_keywords_and_dates() {
    let output = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["complete", "daily-date"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8_lossy(&output);
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines.contains(&"today"), "should include 'today'");
    assert!(lines.contains(&"yesterday"), "should include 'yesterday'");
    assert!(lines.contains(&"tomorrow"), "should include 'tomorrow'");
    let iso_dates: Vec<_> = lines
        .iter()
        .filter(|l| l.len() == 10 && l.chars().nth(4) == Some('-'))
        .collect();
    assert_eq!(iso_dates.len(), 14, "should have 14 past ISO dates");
}

#[test]
fn complete_daily_date_includes_existing_note_dates() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault dir should be created");
    fs::create_dir_all(vault_root.join(".vulcan")).expect(".vulcan dir should be created");
    fs::create_dir_all(vault_root.join("Journal/Daily")).expect("daily dir should be created");

    fs::write(
        vault_root.join(".vulcan/config.toml"),
        "[periodic.daily]\nfolder = \"Journal/Daily\"\n",
    )
    .expect("config should be written");

    // Create two daily notes with dates far enough back they won't be in the
    // last-14-days hardcoded list.
    for date in &["2024-01-15", "2023-06-03"] {
        fs::write(
            vault_root.join(format!("Journal/Daily/{date}.md")),
            format!("# {date}\n\nA daily note.\n"),
        )
        .expect("daily note should be written");
    }
    run_scan(&vault_root);

    let vault_str = vault_root.to_str().expect("vault path should be utf-8");
    let output = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["--vault", vault_str, "complete", "daily-date"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8_lossy(&output);
    let lines: Vec<&str> = text.lines().collect();

    assert!(lines.contains(&"today"), "should still include 'today'");
    assert!(
        lines.contains(&"2024-01-15"),
        "should include existing daily note date 2024-01-15"
    );
    assert!(
        lines.contains(&"2023-06-03"),
        "should include existing daily note date 2023-06-03"
    );
    // Existing dates should not be duplicated
    assert_eq!(
        lines.iter().filter(|&&l| l == "2024-01-15").count(),
        1,
        "each date should appear exactly once"
    );
}

#[test]
fn complete_unknown_context_returns_empty() {
    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["complete", "nonexistent-context-xyz"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
}

fn fish_is_available() -> bool {
    ProcessCommand::new("fish")
        .arg("--version")
        .output()
        .is_ok()
}

fn user_home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
        .or_else(|| {
            let drive = std::env::var_os("HOMEDRIVE")?;
            let path = std::env::var_os("HOMEPATH")?;
            let mut home = PathBuf::from(drive);
            home.push(path);
            Some(home)
        })
}

fn write_fish_completion_script(temp_dir: &TempDir) -> PathBuf {
    let completions = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["completions", "fish"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let script_path = temp_dir.path().join("vulcan.fish");
    fs::write(&script_path, completions).expect("completion script should write");
    script_path
}

#[test]
fn fish_completions_include_dynamic_hook() {
    let output = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["completions", "fish"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8_lossy(&output);
    assert!(
        text.contains("__fish_vulcan_dynamic_complete_bases_file"),
        "fish completions should hook bases-file for bases subcommands"
    );
    assert!(
        text.contains("eval tui create view-add view-delete view-rename"),
        "fish completions should cover all bases file-taking subcommands"
    );
    assert!(
        text.contains("function __fish_vulcan_completion_prefix_args"),
        "fish completions should include the helper that replays leading global args"
    );
    assert!(
        text.contains("complete -c vulcan -e"),
        "fish completions should clear stale vulcan completion definitions before re-registering them"
    );
    assert!(
        text.contains("__fish_vulcan_complete_vault_path_arg"),
        "fish completions should include the dedicated vault-relative path helper"
    );
}

#[test]
fn fish_resourcing_completions_replaces_stale_definitions() {
    if !fish_is_available() {
        return;
    }

    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);

    let fish_home = temp_dir.path().join("fish-home");
    let config_home = fish_home.join(".config");
    let data_home = fish_home.join(".local/share");
    fs::create_dir_all(config_home.join("fish")).expect("fish config dir should exist");
    fs::create_dir_all(data_home.join("fish/generated_completions"))
        .expect("fish data dir should exist");

    let script_path = write_fish_completion_script(&temp_dir);
    let output = ProcessCommand::new("fish")
        .env("HOME", &fish_home)
        .env("XDG_CONFIG_HOME", &config_home)
        .env("XDG_DATA_HOME", &data_home)
        .arg("-c")
        .arg(format!(
            "complete -c vulcan -n '__fish_seen_subcommand_from note; and __fish_seen_subcommand_from info' -f -a 'BROKEN'; source '{}'; complete -C 'vulcan --vault {} note info '",
            script_path.display(),
            vault_root.display()
        ))
        .output()
        .expect("fish should launch");
    assert!(
        output.status.success(),
        "fish completion helper should succeed after re-sourcing: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.lines().any(|line| line.starts_with("Home\t")),
        "re-sourced fish completions should still return dynamic note candidates, got: {text}"
    );
    assert!(
        !text.contains("BROKEN"),
        "re-sourcing fish completions should replace stale definitions, got: {text}"
    );
}

#[test]
fn fish_vault_path_completion_uses_selected_vault_outside_cwd() {
    if !fish_is_available() {
        return;
    }

    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);

    let outside_dir = temp_dir.path().join("outside");
    fs::create_dir_all(&outside_dir).expect("outside dir should exist");
    fs::write(outside_dir.join("Hazard.md"), "# hazard\n").expect("outside file should write");

    let script_path = write_fish_completion_script(&temp_dir);

    let output = ProcessCommand::new("fish")
        .arg("-c")
        .arg(format!(
            "cd '{}'; source '{}'; complete -C 'vulcan --vault {} note create H'",
            outside_dir.display(),
            script_path.display(),
            vault_root.display()
        ))
        .output()
        .expect("fish should launch");
    assert!(
        output.status.success(),
        "fish completion helper should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.lines().any(|line| line.starts_with("Home.md\t")),
        "note create should complete paths from the selected vault, got: {text}"
    );
    assert!(
        !text.contains("Hazard.md"),
        "note create should not complete files from the current working directory, got: {text}"
    );
}

#[test]
fn fish_note_completion_uses_selected_vault_outside_cwd() {
    if !fish_is_available() {
        return;
    }

    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);

    let outside_dir = temp_dir.path().join("outside");
    fs::create_dir_all(&outside_dir).expect("outside dir should exist");
    let script_path = write_fish_completion_script(&temp_dir);

    let output = ProcessCommand::new("fish")
        .arg("-c")
        .arg(format!(
            "cd '{}'; source '{}'; complete -C 'vulcan --vault {} note info H'; complete -C 'vulcan --vault {} backlinks H'",
            outside_dir.display(),
            script_path.display(),
            vault_root.display(),
            vault_root.display()
        ))
        .output()
        .expect("fish should launch");
    assert!(
        output.status.success(),
        "fish completion helper should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.lines()
            .any(|line| line.starts_with("Home\t") || line.starts_with("Home.md\t")),
        "note-style completions should come from the selected vault, got: {text}"
    );
}

#[test]
fn fish_completion_expands_tilde_prefixed_vault_argument() {
    if !fish_is_available() {
        return;
    }
    let Some(home_dir) = user_home_dir() else {
        return;
    };

    let temp_dir = TempDir::new_in(&home_dir).expect("temp dir under home should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    fs::write(vault_root.join("Dashboards.base"), "views: []\n").expect("base file should write");
    run_scan(&vault_root);

    let relative_vault = vault_root
        .strip_prefix(&home_dir)
        .expect("vault should be inside the home directory");
    let tilde_vault = format!("~/{}", relative_vault.display());
    let script_path = write_fish_completion_script(&temp_dir);

    let note_output = ProcessCommand::new("fish")
        .arg("-c")
        .arg(format!(
            "source '{}'; complete -C 'vulcan --vault {} note info '",
            script_path.display(),
            tilde_vault
        ))
        .output()
        .expect("fish should launch");
    assert!(
        note_output.status.success(),
        "fish note completion should succeed: {}",
        String::from_utf8_lossy(&note_output.stderr)
    );
    let note_text = String::from_utf8_lossy(&note_output.stdout);
    assert!(
        note_text.lines().any(|line| line.starts_with("Home\t")),
        "note info should complete notes for tilde-prefixed vaults, got: {note_text}"
    );

    let backlinks_output = ProcessCommand::new("fish")
        .arg("-c")
        .arg(format!(
            "source '{}'; complete -C 'vulcan --vault {} backlinks '",
            script_path.display(),
            tilde_vault
        ))
        .output()
        .expect("fish should launch");
    assert!(
        backlinks_output.status.success(),
        "fish backlinks completion should succeed: {}",
        String::from_utf8_lossy(&backlinks_output.stderr)
    );
    let backlinks_text = String::from_utf8_lossy(&backlinks_output.stdout);
    assert!(
        backlinks_text
            .lines()
            .any(|line| line.starts_with("Home\t")),
        "backlinks should complete notes for tilde-prefixed vaults, got: {backlinks_text}"
    );

    let bases_output = ProcessCommand::new("fish")
        .arg("-c")
        .arg(format!(
            "source '{}'; complete -C 'vulcan --vault {} bases eval '",
            script_path.display(),
            tilde_vault
        ))
        .output()
        .expect("fish should launch");
    assert!(
        bases_output.status.success(),
        "fish bases completion should succeed: {}",
        String::from_utf8_lossy(&bases_output.stderr)
    );
    let bases_text = String::from_utf8_lossy(&bases_output.stdout);
    assert!(
        bases_text
            .lines()
            .any(|line| line.starts_with("Dashboards.base\t")),
        "bases eval should complete .base files for tilde-prefixed vaults, got: {bases_text}"
    );

    let create_output = ProcessCommand::new("fish")
        .arg("-c")
        .arg(format!(
            "source '{}'; complete -C 'vulcan --vault {} note create '",
            script_path.display(),
            tilde_vault
        ))
        .output()
        .expect("fish should launch");
    assert!(
        create_output.status.success(),
        "fish note create completion should succeed: {}",
        String::from_utf8_lossy(&create_output.stderr)
    );
    let create_text = String::from_utf8_lossy(&create_output.stdout);
    assert!(
        create_text.lines().any(|line| line.starts_with("Home.md\t")),
        "note create should complete vault-relative paths for tilde-prefixed vaults, got: {create_text}"
    );
}

#[test]
fn fish_bases_eval_completion_uses_selected_vault_outside_cwd() {
    if !fish_is_available() {
        return;
    }

    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    fs::create_dir_all(vault_root.join("Dashboards")).expect("dashboards dir should exist");
    fs::write(vault_root.join("Dashboards/People.base"), "views: []\n")
        .expect("base file should write");
    run_scan(&vault_root);

    let outside_dir = temp_dir.path().join("outside");
    fs::create_dir_all(&outside_dir).expect("outside dir should exist");
    fs::write(outside_dir.join("Dashboards.base"), "views: []\n")
        .expect("outside base file should write");

    let script_path = write_fish_completion_script(&temp_dir);
    let output = ProcessCommand::new("fish")
        .arg("-c")
        .arg(format!(
            "cd '{}'; source '{}'; complete -C 'vulcan --vault {} bases eval D'",
            outside_dir.display(),
            script_path.display(),
            vault_root.display()
        ))
        .output()
        .expect("fish should launch");
    assert!(
        output.status.success(),
        "fish completion helper should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.lines()
            .any(|line| line.starts_with("Dashboards/People.base\t")),
        "bases eval should complete .base files from the selected vault, got: {text}"
    );
    assert!(
        !text.contains("Dashboards.base"),
        "bases eval should not complete files from the current working directory, got: {text}"
    );
}
