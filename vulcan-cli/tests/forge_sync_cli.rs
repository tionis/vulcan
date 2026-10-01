use assert_cmd::Command;
use serde_json::Value;
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tempfile::TempDir;

const TOKEN: &str = "super-secret-forge-token";

type Keys = Arc<Mutex<Vec<(u64, String, String)>>>;

struct FakeForgejo {
    url: String,
    keys: Keys,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Drop for FakeForgejo {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[allow(clippy::too_many_lines)] // A small self-contained HTTP fake reads best unbroken.
fn serve() -> FakeForgejo {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    listener.set_nonblocking(true).expect("nonblocking");
    let url = format!("http://{}", listener.local_addr().unwrap());
    let keys: Keys = Arc::default();
    let stop = Arc::new(AtomicBool::new(false));
    let (thread_keys, thread_stop) = (Arc::clone(&keys), Arc::clone(&stop));
    let handle = std::thread::spawn(move || {
        let mut next_id = 0_u64;
        while !thread_stop.load(Ordering::SeqCst) {
            let Ok((mut stream, _)) = listener.accept() else {
                std::thread::sleep(Duration::from_millis(5));
                continue;
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut buffer = Vec::new();
            let mut chunk = [0_u8; 4096];
            let (head, body) = loop {
                let read = stream.read(&mut chunk).unwrap_or(0);
                buffer.extend_from_slice(&chunk[..read]);
                let text = String::from_utf8_lossy(&buffer).into_owned();
                if let Some(end) = text.find("\r\n\r\n") {
                    let head = text[..end].to_owned();
                    let length = head
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())
                                .flatten()
                        })
                        .unwrap_or(0);
                    if buffer.len() >= end + 4 + length || read == 0 {
                        break (head, text[end + 4..].to_owned());
                    }
                } else if read == 0 {
                    break (text, String::new());
                }
            };
            let authorized = head
                .to_ascii_lowercase()
                .contains(&format!("authorization: token {TOKEN}"));
            let first = head.lines().next().unwrap_or_default().to_owned();
            let mut parts = first.split_whitespace();
            let method = parts.next().unwrap_or_default();
            let path = parts
                .next()
                .unwrap_or_default()
                .split('?')
                .next()
                .unwrap_or_default()
                .to_owned();
            let (status, payload) = if !authorized {
                (401, r#"{"message":"bad token"}"#.to_owned())
            } else if let Some(rest) = path.strip_prefix("/api/v1/repos/owner/vault/keys") {
                let mut keys = thread_keys.lock().unwrap();
                match (method, rest) {
                    ("GET", "") => (
                        200,
                        serde_json::to_string(
                            &keys
                                .iter()
                                .map(|(id, key, title)| serde_json::json!({"id": id, "key": key, "title": title, "read_only": false}))
                                .collect::<Vec<_>>(),
                        )
                        .unwrap(),
                    ),
                    ("POST", "") => {
                        let parsed: Value = serde_json::from_str(&body).unwrap_or_default();
                        next_id += 1;
                        let entry = (
                            next_id,
                            parsed["key"].as_str().unwrap_or_default().to_owned(),
                            parsed["title"].as_str().unwrap_or_default().to_owned(),
                        );
                        let rendered = serde_json::json!({"id": entry.0, "key": entry.1, "title": entry.2, "read_only": parsed["read_only"]}).to_string();
                        keys.push(entry);
                        (201, rendered)
                    }
                    ("DELETE", id) => {
                        let id: u64 = id.trim_start_matches('/').parse().unwrap_or(0);
                        keys.retain(|entry| entry.0 != id);
                        (204, String::new())
                    }
                    _ => (405, "{}".to_owned()),
                }
            } else {
                (404, r#"{"message":"not found"}"#.to_owned())
            };
            let response = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                payload.len()
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });
    FakeForgejo {
        url,
        keys,
        stop,
        handle: Some(handle),
    }
}

fn run(root: &Path, token: Option<&str>, args: &[&str]) -> std::process::Output {
    let home = root.join("home");
    fs::create_dir_all(&home).expect("home");
    let mut command = Command::cargo_bin("vulcan").expect("vulcan binary");
    command
        .current_dir(root)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env_remove("FORGE_TOKEN");
    if let Some(token) = token {
        command.env("FORGE_TOKEN", token);
    }
    command.args(args).output().expect("command output")
}

fn json(output: &std::process::Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("JSON output")
}

fn failure_text(output: &std::process::Output) -> String {
    assert!(!output.status.success(), "expected failure");
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn git(dir: &Path, args: &[&str]) {
    let output = ProcessCommand::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .expect("git");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn installation(root: &Path, name: &str, remote: &Path) -> PathBuf {
    let base = root.join(name);
    let vault = base.join("vault");
    fs::create_dir_all(&vault).expect("vault");
    git(
        &vault,
        &["-c", "init.defaultBranch=main", "init", "--quiet"],
    );
    git(&vault, &["config", "user.name", "Test"]);
    git(&vault, &["config", "user.email", "test@example.invalid"]);
    git(
        &vault,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    fs::write(vault.join("Home.md"), "note\n").unwrap();
    git(&vault, &["add", "Home.md"]);
    git(&vault, &["commit", "--quiet", "-m", "initial"]);
    base
}

#[test]
#[allow(clippy::too_many_lines)] // One ordered admin lifecycle reads best unbroken.
fn forge_sync_installs_registered_devices_and_removes_revoked_ones() {
    let temporary = TempDir::new().expect("temporary directory");
    let root = temporary.path();
    let remote = root.join("remote.git");
    git(root, &["init", "--bare", "--quiet", "remote.git"]);
    let admin = installation(root, "admin", &remote);
    let device = installation(root, "device", &remote);
    let vault = admin.join("vault");
    let vault = vault.to_str().unwrap();
    let forge = serve();
    let admin_run = |token: Option<&str>, args: &[&str]| {
        let mut full = vec!["--vault", vault, "--output", "json", "sync"];
        full.extend_from_slice(args);
        run(&admin, token, &full)
    };

    // No forge configured yet.
    let error = failure_text(&admin_run(Some(TOKEN), &["forge", "sync"]));
    assert!(error.contains("sync forge set"), "{error}");

    // Settings reject an unsafe destination and never store the token.
    assert!(!admin_run(
        None,
        &[
            "forge",
            "set",
            "--url",
            "http://git.example.com",
            "--repo",
            "owner/vault",
            "--token-env",
            "FORGE_TOKEN"
        ]
    )
    .status
    .success());
    let set = json(&admin_run(
        None,
        &[
            "forge",
            "set",
            "--url",
            &forge.url,
            "--repo",
            "owner/vault",
            "--token-env",
            "FORGE_TOKEN",
        ],
    ));
    assert_eq!(set["changed"], true);
    let shown = json(&admin_run(None, &["forge", "show"]));
    assert_eq!(shown["config"]["token_env"], "FORGE_TOKEN");
    assert_eq!(shown["token_env_set"], false);
    assert_eq!(
        json(&admin_run(Some(TOKEN), &["forge", "show"]))["token_env_set"],
        true
    );

    // A device registers a placeholder.
    let key = json(&run(&device, None, &["device", "init", "--output", "json"]));
    let device_id = key["identity"]["device_id"].as_str().unwrap().to_owned();
    let public = json(&run(
        &device,
        None,
        &["device", "public-key", "--output", "json"],
    ));
    let key_file = root.join("device.pub");
    fs::write(&key_file, public["public_key"].as_str().unwrap()).unwrap();
    json(&admin_run(
        None,
        &[
            "devices",
            "register",
            "--public-key",
            key_file.to_str().unwrap(),
            "--label",
            "Laptop",
        ],
    ));

    // The token is required, and its value is never echoed.
    let error = failure_text(&admin_run(None, &["forge", "sync"]));
    assert!(error.contains("FORGE_TOKEN"), "{error}");
    let wrong = failure_text(&admin_run(Some("wrong-token"), &["forge", "sync"]));
    assert!(wrong.contains("check the API token"), "{wrong}");
    assert!(!wrong.contains("wrong-token"));

    // A dry run shows the plan and changes nothing.
    let preview = json(&admin_run(Some(TOKEN), &["forge", "sync", "--dry-run"]));
    assert_eq!(preview["entries"][0]["action"], "add");
    assert_eq!(preview["entries"][0]["result"], "planned");
    assert!(forge.keys.lock().unwrap().is_empty());

    let applied = json(&admin_run(Some(TOKEN), &["forge", "sync"]));
    assert_eq!(applied["applied"], 1);
    {
        let keys = forge.keys.lock().unwrap();
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].2, format!("vulcan-device:{device_id} Laptop"));
        assert_eq!(keys[0].1, public["public_key"].as_str().unwrap());
    }
    assert_eq!(
        json(&admin_run(Some(TOKEN), &["forge", "sync"]))["entries"][0]["action"],
        "present"
    );

    // Revocation removes exactly that key; an unrelated key stays.
    forge
        .keys
        .lock()
        .unwrap()
        .push((99, "ssh-ed25519 AAAAci".to_owned(), "ci runner".to_owned()));
    json(&admin_run(None, &["devices", "revoke", &device_id]));
    let revoked = json(&admin_run(Some(TOKEN), &["forge", "sync"]));
    assert_eq!(revoked["entries"][0]["action"], "remove");
    assert_eq!(revoked["foreign_keys"], 1);
    let keys = forge.keys.lock().unwrap();
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].2, "ci runner");
    drop(keys);

    let cleared = json(&admin_run(None, &["forge", "clear"]));
    assert_eq!(cleared["changed"], true);
    assert!(json(&admin_run(None, &["forge", "show"]))["config"].is_null());

    // Nothing the commands print or store contains the token.
    for file in walk(&root.join("admin")) {
        if let Ok(text) = fs::read_to_string(&file) {
            assert!(!text.contains(TOKEN), "{} leaked the token", file.display());
        }
    }
}

fn walk(directory: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Ok(entries) = fs::read_dir(directory) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                files.extend(walk(&path));
            } else {
                files.push(path);
            }
        }
    }
    files
}

fn extra_vault(base: &Path, name: &str, remote: &Path) -> PathBuf {
    let vault = base.join(name);
    fs::create_dir_all(&vault).expect("vault");
    git(
        &vault,
        &["-c", "init.defaultBranch=main", "init", "--quiet"],
    );
    git(&vault, &["config", "user.name", "Test"]);
    git(&vault, &["config", "user.email", "test@example.invalid"]);
    git(
        &vault,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    fs::write(vault.join("Home.md"), "note\n").unwrap();
    git(&vault, &["add", "Home.md"]);
    git(&vault, &["commit", "--quiet", "-m", "initial"]);
    vault
}

#[test]
#[allow(clippy::too_many_lines)] // One ordered fleet scenario reads best unbroken.
fn fleet_view_and_all_wikis_keep_every_vault_independent() {
    let temporary = TempDir::new().expect("temporary directory");
    let root = temporary.path();
    let admin = root.join("admin");
    let forge = serve();
    let mut vaults = Vec::new();
    for name in ["alpha", "beta", "gamma"] {
        let remote = root.join(format!("{name}.git"));
        git(
            root,
            &["init", "--bare", "--quiet", remote.to_str().unwrap()],
        );
        vaults.push((name, extra_vault(&admin, name, &remote)));
    }
    let run_admin = |token: Option<&str>, args: &[&str]| run(&admin, token, args);
    for (name, vault) in &vaults {
        let added = run_admin(None, &["vault", "add", name, vault.to_str().unwrap()]);
        assert!(
            added.status.success(),
            "{}",
            String::from_utf8_lossy(&added.stderr)
        );
    }
    // alpha points at the repository the fake forge serves; beta at one it does not.
    let set = |wiki: &str, repo: &str| {
        json(&run_admin(
            None,
            &[
                "--output",
                "json",
                "sync",
                "forge",
                "set",
                "--wiki",
                wiki,
                "--url",
                &forge.url,
                "--repo",
                repo,
                "--token-env",
                "FORGE_TOKEN",
            ],
        ));
    };
    set("alpha", "owner/vault");
    set("beta", "owner/missing");

    // A device registers in alpha only.
    let device = root.join("device");
    json(&run(&device, None, &["device", "init", "--output", "json"]));
    let public = json(&run(
        &device,
        None,
        &["device", "public-key", "--output", "json"],
    ));
    let key_file = root.join("device.pub");
    fs::write(&key_file, public["public_key"].as_str().unwrap()).unwrap();
    json(&run_admin(
        None,
        &[
            "--output",
            "json",
            "sync",
            "devices",
            "register",
            "--wiki",
            "alpha",
            "--public-key",
            key_file.to_str().unwrap(),
        ],
    ));

    // `--all-wikis` and `--wiki` are mutually exclusive.
    assert!(!run_admin(
        Some(TOKEN),
        &["sync", "forge", "sync", "--all-wikis", "--wiki", "alpha"]
    )
    .status
    .success());

    // beta fails, gamma is skipped, alpha still succeeds: results are independent.
    let output = run_admin(
        Some(TOKEN),
        &["--output", "json", "sync", "forge", "sync", "--all-wikis"],
    );
    assert!(
        !output.status.success(),
        "a failed vault makes the command fail"
    );
    // The report comes first; the failure is then reported as a second document.
    let report: Value = serde_json::Deserializer::from_slice(&output.stdout)
        .into_iter::<Value>()
        .next()
        .expect("report is printed even on failure")
        .expect("report is valid JSON");
    assert_eq!(
        (
            report["synced"].as_u64(),
            report["skipped"].as_u64(),
            report["failed"].as_u64()
        ),
        (Some(1), Some(1), Some(1))
    );
    let outcome = |wiki: &str| {
        report["wikis"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["wiki_id"] == wiki)
            .unwrap()
            .clone()
    };
    assert_eq!(outcome("alpha")["outcome"], "synced");
    assert_eq!(outcome("alpha")["report"]["applied"], 1);
    assert_eq!(outcome("beta")["outcome"], "failed");
    assert!(outcome("beta")["reason"].as_str().unwrap().contains("404"));
    assert_eq!(outcome("gamma")["outcome"], "skipped");
    assert_eq!(forge.keys.lock().unwrap().len(), 1);

    // Offline fleet view: no remote is asked, so the device's own state is unknown.
    let offline = json(&run_admin(
        None,
        &["--output", "json", "devices", "list", "--offline"],
    ));
    let by_wiki = |report: &Value, wiki: &str| {
        report["vaults"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["wiki_id"] == wiki)
            .unwrap()
            .clone()
    };
    assert_eq!(
        by_wiki(&offline, "alpha")["transport"]["state"],
        "not_bound"
    );
    assert_eq!(
        by_wiki(&offline, "alpha")["this_device_registration"],
        "unknown"
    );

    // After the admin device itself syncs alpha, only alpha shows it as registered.
    json(&run_admin(None, &["device", "init", "--output", "json"]));
    json(&run_admin(
        None,
        &[
            "--vault",
            vaults[0].1.to_str().unwrap(),
            "--output",
            "json",
            "sync",
            "run",
        ],
    ));
    let online = json(&run_admin(None, &["--output", "json", "devices", "list"]));
    assert_eq!(
        by_wiki(&online, "alpha")["this_device_registration"],
        "registered"
    );
    assert_eq!(
        by_wiki(&online, "beta")["this_device_registration"],
        "not_registered"
    );
    assert_eq!(
        by_wiki(&online, "gamma")["this_device_registration"],
        "not_registered"
    );
    assert_eq!(
        by_wiki(&online, "alpha")["sync_inventory"]["registrations"]["count"],
        2,
        "alpha lists the placeholder and this device; no other vault's records"
    );
    assert_eq!(
        by_wiki(&online, "beta")["sync_inventory"]["registrations"]["count"],
        0
    );
}

#[test]
#[allow(clippy::too_many_lines)] // One ordered two-administrator scenario reads best unbroken.
fn init_publishes_non_secret_settings_that_another_administrator_adopts() {
    let temporary = TempDir::new().expect("temporary directory");
    let root = temporary.path();
    let remote = root.join("remote.git");
    git(root, &["init", "--bare", "--quiet", "remote.git"]);
    let first = installation(root, "first", &remote);
    let second = installation(root, "second", &remote);
    let forge = serve();
    let on = |who: &Path, token: Option<&str>, args: &[&str]| {
        let vault = who.join("vault");
        let mut full = vec![
            "--vault",
            vault.to_str().unwrap(),
            "--output",
            "json",
            "sync",
            "forge",
        ];
        full.extend_from_slice(args);
        run(who, token, &full)
    };

    // The remote is a local path, so nothing can be derived: the host check
    // must be confirmed explicitly rather than silently skipped.
    let refused = failure_text(&on(
        &first,
        None,
        &[
            "init",
            "--kind",
            "forgejo",
            "--url",
            &forge.url,
            "--repo",
            "owner/vault",
            "--oauth-client-id",
            "client-abc",
        ],
    ));
    assert!(refused.contains("--allow-other-host"), "{refused}");

    // Nothing is published by a dry run.
    let preview = json(&on(
        &first,
        None,
        &[
            "init",
            "--kind",
            "forgejo",
            "--url",
            &forge.url,
            "--repo",
            "owner/vault",
            "--oauth-client-id",
            "client-abc",
            "--allow-other-host",
            "--publish",
            "--dry-run",
        ],
    ));
    assert_eq!(preview["saved"], false);
    assert_eq!(preview["published"], false);

    let published = json(&on(
        &first,
        None,
        &[
            "init",
            "--kind",
            "forgejo",
            "--url",
            &forge.url,
            "--repo",
            "owner/vault",
            "--oauth-client-id",
            "client-abc",
            "--token-env",
            "MY_PRIVATE_TOKEN_NAME",
            "--allow-other-host",
            "--publish",
        ],
    ));
    assert_eq!(published["saved"], true);
    assert_eq!(published["published"], true);

    // What reached the remote carries no secret and no device-specific value.
    let blob = ProcessCommand::new("git")
        .current_dir(&remote)
        .args(["show", "refs/heads/__vulcan-sync/forge:forge.json"])
        .output()
        .expect("git show");
    let shared = String::from_utf8(blob.stdout).expect("utf-8 descriptor");
    assert!(shared.contains("client-abc"), "{shared}");
    assert!(
        !shared.contains("MY_PRIVATE_TOKEN_NAME") && !shared.contains(TOKEN),
        "{shared}"
    );
    assert!(
        !shared.contains("owner/vault"),
        "the repository is derived, not shared: {shared}"
    );

    // The second administrator sees a proposal and saves nothing by default.
    let proposal = json(&on(&second, None, &["init"]));
    assert_eq!(
        proposal["shared"]["settings"]["oauth_client_id"],
        "client-abc"
    );
    assert_eq!(proposal["saved"], false);
    assert!(json(&on(&second, None, &["show"]))["config"].is_null());

    // Adoption still needs the same explicit host confirmation, and the
    // device-specific token variable stays local to this machine.
    assert!(!on(&second, None, &["init", "--adopt"]).status.success());
    let adopted = json(&on(
        &second,
        None,
        &[
            "init",
            "--adopt",
            "--allow-other-host",
            "--repo",
            "owner/vault",
            "--token-env",
            "FORGE_TOKEN",
        ],
    ));
    assert_eq!(adopted["saved"], true);
    let shown = json(&on(&second, None, &["show"]));
    assert_eq!(shown["config"]["oauth_client_id"], "client-abc");
    assert_eq!(shown["config"]["token_env"], "FORGE_TOKEN");
    assert_eq!(shown["config"]["url"], forge.url);

    // The adopted settings drive a real `forge sync` against the (fake) forge.
    let plan = json(&on(&second, Some(TOKEN), &["sync", "--dry-run"]));
    assert_eq!(plan["repo"], "owner/vault");
    assert_eq!(plan["dry_run"], true);
}
