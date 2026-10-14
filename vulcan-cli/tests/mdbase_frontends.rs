//! Every mdbase query frontend answers a canonical query with the same
//! envelope as the shared service: the in-process `vulcan-app` call, the
//! CLI, the Python wrapper and shell script in `docs/examples/mdbase`, and
//! the HTTP routes of `vulcan serve` (the daemon's per-vault HTTP state),
//! both through its retained watched session and without watching. Errors
//! fail on every frontend with the same message, and a permission profile
//! narrows every frontend alike. Physical-plan parity between the two
//! execution paths these frontends use is covered by the `vulcan-app` tests.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;
use vulcan_core::{PermissionGuard, VaultPaths};

const PROFILE: &str = "public";

fn write(path: &Path, contents: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

/// A collection with two types, read defaults, invalid records, links, and
/// a private folder a permission profile hides.
fn collection(root: &Path) {
    std::fs::create_dir_all(root).unwrap();
    vulcan_core::initialize_vulcan_dir(&VaultPaths::new(root)).unwrap();
    write(&root.join("mdbase.yaml"), "spec_version: \"0.3.0\"\n");
    write(
        &root.join(".vulcan/config.toml"),
        &format!(
            "[permissions.profiles.{PROFILE}]\nread = {{ allow = [\"*\"], deny = [\"folder:private/**\"] }}\nwrite = \"all\"\nindex = \"allow\"\n"
        ),
    );
    write(
        &root.join("_types/task.md"),
        "---\nkind: mdbase.type\nname: task\nversion: 1\nschema:\n  dialect: json-schema-2020-12\n  value:\n    type: object\n    required: [title]\n    properties:\n      priority: {type: integer}\n      project: {type: string}\ncollection:\n  read_defaults: {status: open, priority: 1}\n---\n",
    );
    write(
        &root.join("_types/contact.md"),
        "---\nkind: mdbase.type\nname: contact\nversion: 1\nschema:\n  dialect: json-schema-2020-12\n  value:\n    type: object\n    required: [name]\n---\n",
    );
    let tasks = [
        (
            "tasks/alpha.md",
            "title: Alpha\npriority: 3\nproject: \"[[Ada]]\"\n",
        ),
        ("tasks/beta.md", "title: Beta\nstatus: done\n"),
        (
            "tasks/gamma.md",
            "title: Gamma\nstatus: open\npriority: 2\n",
        ),
        ("tasks/delta.md", "title: Delta\npriority: high\n"),
        ("tasks/untitled.md", "status: open\n"),
        ("private/secret.md", "title: Secret\npriority: 5\n"),
    ];
    for (path, frontmatter) in tasks {
        write(
            &root.join(path),
            &format!("---\ntype: task\n{frontmatter}---\nSee [[Ada]].\n"),
        );
    }
    write(
        &root.join("people/Ada.md"),
        "---\ntype: contact\nname: Ada\nemail: ada@example.invalid\n---\n",
    );
    write(
        &root.join("people/nameless.md"),
        "---\ntype: contact\n---\n",
    );
    let status = Command::new(binary())
        .arg("--vault")
        .arg(root)
        .args(["index", "scan"])
        .output()
        .unwrap()
        .status;
    assert!(status.success());
}

fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_vulcan"))
}

/// Canonical queries covering selection, defaults, ordering, paging,
/// grouping, projections, and bodies.
fn queries() -> Vec<Value> {
    vec![
        json!({"types": ["task"]}),
        json!({
            "types": ["task"], "where": "status == 'open'",
            "select": ["file.path", "title", "status", "priority"],
            "order_by": [{"field": "title", "direction": "asc"}], "limit": 2,
        }),
        json!({
            "types": ["task"], "where": "priority >= 2",
            "order_by": [{"field": "priority", "direction": "desc"}],
        }),
        json!({
            "types": ["task"], "select": [{"name": "open", "expr": "status == 'open'"}],
            "group_by": [{"field": "status"}],
        }),
        json!({"types": ["contact"], "select": ["file.path", "name", "email"], "include_body": true}),
        json!({"where": "file.path.startsWith('people/')", "order_by": [{"field": "file.path"}]}),
        json!({"types": ["task"], "limit": 1, "offset": 1, "order_by": [{"field": "file.path"}]}),
    ]
}

/// Queries every frontend must refuse, with the same code and message.
fn invalid_queries() -> Vec<Value> {
    vec![
        json!({"where": "status =="}),
        json!({"types": ["task"], "select": ["file.path", "path"]}),
    ]
}

/// The outcome of one query: the envelope, or the error code and message.
type Outcome = Result<Value, (String, String)>;

fn failure(body: &Value) -> (String, String) {
    let text = |key: &str| body[key].as_str().unwrap_or_default().to_string();
    (text("code"), text("error"))
}

fn in_process(root: &Path, query: &Value, profile: Option<&str>) -> Outcome {
    let paths = VaultPaths::new(root);
    let filter = profile.map(|profile| {
        let selection = vulcan_core::resolve_permission_profile(&paths, Some(profile)).unwrap();
        vulcan_core::ProfilePermissionGuard::new(&paths, selection).read_filter()
    });
    vulcan_app::mdbase::build_mdbase_query_report(&paths, query, filter.as_ref())
        .map(|report| serde_json::to_value(report).unwrap())
        .map_err(|error| {
            let code = error.code().unwrap_or("operation_failed");
            (code.to_string(), error.to_string())
        })
}

fn cli(root: &Path, query: &Value, profile: Option<&str>) -> Outcome {
    let mut command = Command::new(binary());
    command.arg("--vault").arg(root).args(["--output", "json"]);
    if let Some(profile) = profile {
        command.args(["--permissions", profile]);
    }
    let output = command
        .args(["mdbase", "query", &query.to_string()])
        .output()
        .unwrap();
    let body: Value = serde_json::from_slice(&output.stdout).unwrap_or(Value::Null);
    if output.status.success() {
        Ok(body)
    } else {
        Err(failure(&body))
    }
}

/// The Python wrapper, run once per profile over every query.
fn python(root: &Path, queries: &[Value], profile: Option<&str>) -> Option<Vec<Outcome>> {
    let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("../docs/examples/mdbase");
    let script = format!(
        r"
import json, sys
sys.path.insert(0, {examples:?})
from collection import Collection, CollectionError
collection = Collection({root:?}, permissions={profile})
for query in json.loads(sys.stdin.read()):
    try:
        print(json.dumps({{'ok': collection.query(query)}}))
    except CollectionError as error:
        print(json.dumps({{'error': error.message, 'code': error.code}}))
",
        examples = examples.display().to_string(),
        root = root.display().to_string(),
        profile = profile.map_or("None".to_string(), |profile| format!("{profile:?}")),
    );
    let mut child = Command::new("python3")
        .args(["-c", &script])
        .env("VULCAN", binary())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(Value::from(queries.to_vec()).to_string().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Some(
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(|line| {
                let line: Value = serde_json::from_str(line).unwrap();
                match line.get("ok") {
                    Some(envelope) => Ok(envelope.clone()),
                    None => Err(failure(&line)),
                }
            })
            .collect(),
    )
}

/// A `vulcan serve` process and its address and token.
struct Server {
    child: Child,
    address: String,
    token: String,
}

impl Server {
    fn start(root: &Path, watch: bool, profile: Option<&str>) -> Self {
        let mut command = Command::new(binary());
        command.arg("--vault").arg(root);
        if let Some(profile) = profile {
            command.args(["--permissions", profile]);
        }
        command.args(["serve", "--bind", "127.0.0.1:0"]);
        if !watch {
            command.arg("--no-watch");
        }
        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut lines = BufReader::new(child.stderr.take().unwrap()).lines();
        let (mut address, mut token) = (None, None);
        while address.is_none() || token.is_none() {
            let line = lines
                .next()
                .expect("serve reports its token and address")
                .unwrap();
            if let Some(rest) = line.strip_prefix("Vulcan serve token: ") {
                token = Some(rest.trim().to_string());
            } else if let Some(rest) = line.strip_prefix("Vulcan serve listening on http://") {
                address = Some(rest.trim().to_string());
            }
        }
        // Keep draining stderr so the server never blocks on it.
        std::thread::spawn(move || lines.for_each(drop));
        Self {
            child,
            address: address.unwrap(),
            token: token.unwrap(),
        }
    }

    fn query(&self, query: &Value) -> Outcome {
        let encoded = query
            .to_string()
            .bytes()
            .map(|byte| match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                    (byte as char).to_string()
                }
                byte => format!("%{byte:02X}"),
            })
            .collect::<String>();
        let mut stream = TcpStream::connect(&self.address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .unwrap();
        write!(
            stream,
            "GET /mdbase/query?query={encoded} HTTP/1.1\r\nHost: {}\r\nx-vulcan-token: {}\r\nConnection: close\r\n\r\n",
            self.address, self.token
        )
        .unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).unwrap();
        let response = String::from_utf8(response).unwrap();
        let (head, body) = response.split_once("\r\n\r\n").unwrap();
        let body = if head
            .to_ascii_lowercase()
            .contains("transfer-encoding: chunked")
        {
            dechunk(body)
        } else {
            body.to_string()
        };
        // The HTTP transport wraps the envelope: `{ok, result}` or
        // `{ok: false, error, code}`.
        let body: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
        if head.starts_with("HTTP/1.1 200") && body["ok"] == true {
            Ok(body["result"].clone())
        } else {
            // Invalid queries are the caller's to correct, not server faults.
            if body["code"] == "invalid_query" {
                assert!(head.starts_with("HTTP/1.1 400"), "{head}");
            }
            Err(failure(&body))
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn dechunk(body: &str) -> String {
    let mut output = String::new();
    let mut rest = body;
    while let Some((size, after)) = rest.split_once("\r\n") {
        let size = usize::from_str_radix(size.trim(), 16).unwrap_or(0);
        if size == 0 {
            break;
        }
        output.push_str(&after[..size]);
        rest = &after[size + 2..];
    }
    output
}

#[test]
fn every_frontend_answers_canonical_queries_alike() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("collection");
    collection(&root);
    let mut all = queries();
    all.extend(invalid_queries());

    for profile in [None, Some(PROFILE)] {
        let expected = all
            .iter()
            .map(|query| in_process(&root, query, profile))
            .collect::<Vec<_>>();
        // The fixture exercises what the comparison is for.
        assert!(expected[..queries().len()].iter().all(Result::is_ok));
        assert!(expected[queries().len()..].iter().all(|outcome| outcome
            .as_ref()
            .is_err_and(|(code, _)| code == "invalid_query")));
        let tasks = expected[0].as_ref().unwrap()["results"]
            .as_array()
            .unwrap()
            .len();
        assert_eq!(tasks, if profile.is_some() { 5 } else { 6 });

        for (query, expected) in all.iter().zip(&expected) {
            assert_eq!(
                &cli(&root, query, profile),
                expected,
                "CLI, {profile:?}, {query}"
            );
        }
        if let Some(outcomes) = python(&root, &all, profile) {
            for ((query, expected), outcome) in all.iter().zip(&expected).zip(outcomes) {
                assert_eq!(&outcome, expected, "Python, {profile:?}, {query}");
            }
        } else {
            eprintln!("python3 is unavailable; skipping the Python frontend");
        }
        for watch in [false, true] {
            let server = Server::start(&root, watch, profile);
            for (query, expected) in all.iter().zip(&expected) {
                let outcome = server.query(query);
                assert_eq!(
                    &outcome, expected,
                    "HTTP (watch {watch}), {profile:?}, {query}"
                );
            }
        }
    }
}

/// The shell example's task list is exactly the canonical query it builds.
#[cfg(unix)]
#[test]
fn the_shell_task_list_matches_its_canonical_query() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("collection");
    collection(&root);
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../docs/examples/mdbase/task-list.sh");
    for status in ["open", "done"] {
        let output = Command::new("sh")
            .arg(&script)
            .arg(&root)
            .arg(status)
            .env("VULCAN", binary())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let listed = String::from_utf8(output.stdout).unwrap();
        let listed = listed
            .lines()
            .filter(|line| !line.starts_with(char::is_whitespace) && !line.contains(" result"))
            .map(str::to_string)
            .collect::<Vec<_>>();
        let expected = in_process(
            &root,
            &json!({
                "types": ["task"], "where": format!("status == \"{status}\""),
                "order_by": [{"field": "title"}],
            }),
            None,
        )
        .unwrap();
        let expected = expected["results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|record| record["file"]["path"].as_str().unwrap().to_string())
            .collect::<Vec<_>>();
        assert!(!expected.is_empty());
        assert_eq!(listed, expected, "status {status}");
    }
}
