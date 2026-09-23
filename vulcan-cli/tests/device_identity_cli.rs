use assert_cmd::Command;
use serde_json::Value;
use std::fs;
use std::path::Path;
use std::process::Command as ProcessCommand;
use tempfile::TempDir;

fn run(root: &Path, args: &[&str]) -> std::process::Output {
    let home = root.join("home");
    fs::create_dir_all(&home).expect("home directory");
    Command::cargo_bin("vulcan")
        .expect("vulcan binary")
        .current_dir(root)
        .env("HOME", &home)
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_STATE_HOME", root.join("state"))
        .args(args)
        .output()
        .expect("device command output")
}

fn json_output(output: &std::process::Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("device JSON output")
}

#[test]
fn installation_inventory_keeps_registry_and_per_vault_remote_state_scoped() {
    let temporary = TempDir::new().expect("temporary directory");
    let root = temporary.path();
    let config = root.join("config");
    let state = root.join("state");
    let data = root.join("data");
    let home = root.join("home");
    let vault = root.join("vault");
    let named_id = "01arz3ndektsv4rrffq69g5fav";
    fs::create_dir_all(&home).expect("home directory");
    fs::create_dir_all(&vault).expect("vault directory");
    let git = ProcessCommand::new("git")
        .current_dir(&vault)
        .args(["init", "--quiet"])
        .status()
        .expect("git init");
    assert!(git.success());
    let add_remote = ProcessCommand::new("git")
        .current_dir(&vault)
        .args(["remote", "add", "origin", "../missing-remote.git"])
        .status()
        .expect("add local missing remote");
    assert!(add_remote.success());
    let labels = vault.join(".vulcan/device-names");
    fs::create_dir_all(&labels).expect("shared device labels");
    fs::write(
        labels.join(format!("{named_id}.json")),
        format!("{{\"version\":1,\"device_id\":\"{named_id}\",\"name\":\"Office Laptop\"}}\n"),
    )
    .expect("shared device label");

    let run_vulcan = |args: &[&str]| {
        Command::cargo_bin("vulcan")
            .expect("vulcan binary")
            .current_dir(root)
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", &config)
            .env("XDG_STATE_HOME", &state)
            .env("XDG_DATA_HOME", &data)
            .args(args)
            .output()
            .expect("vulcan command output")
    };
    let added = run_vulcan(&["vault", "add", "personal", vault.to_str().unwrap()]);
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );

    let output = run_vulcan(&["--output", "json", "devices", "list"]);
    let report = json_output(&output);
    assert_eq!(report["version"], 1);
    assert_eq!(
        report["identity"]["source"],
        "installation_device_identity_store"
    );
    assert_eq!(report["identity"]["freshness"], "current_local_read");
    assert_eq!(report["vaults"].as_array().unwrap().len(), 1);
    let wiki = &report["vaults"][0];
    assert_eq!(wiki["wiki_id"], "personal");
    assert_eq!(wiki["registration_source"], "user_registry");
    assert_eq!(wiki["local_state"], "observed");
    assert_eq!(
        wiki["sync_inventory"]["remote_observation"]["state"],
        "unavailable"
    );
    assert_eq!(
        wiki["sync_inventory"]["remote_observation"]["error"],
        "Remote ref observation failed; check configured remote availability and access."
    );
    assert_eq!(
        wiki["sync_inventory"]["named_without_backup"][0]["device_id"],
        named_id
    );
    assert_eq!(
        wiki["sync_inventory"]["named_without_backup"][0]["name"],
        "Office Laptop"
    );

    assert_offline_inventory(root, &vault, named_id);
}

fn assert_offline_inventory(root: &Path, vault: &Path, named_id: &str) {
    let home = root.join("home");
    let config = root.join("config");
    let state = root.join("state");
    let data = root.join("data");
    let trace = root.join("git-trace.log");
    let offline = Command::cargo_bin("vulcan")
        .expect("vulcan binary")
        .current_dir(root)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &config)
        .env("XDG_STATE_HOME", &state)
        .env("XDG_DATA_HOME", &data)
        .env("GIT_TRACE", &trace)
        .args(["--output", "json", "devices", "list", "--offline"])
        .output()
        .expect("offline installation inventory output");
    let offline_report = json_output(&offline);
    let offline_wiki = &offline_report["vaults"][0];
    assert_eq!(offline_wiki["remote_freshness"], "not_requested");
    assert_eq!(
        offline_wiki["sync_inventory"]["remote_observation"]["state"],
        "not_requested"
    );
    assert_eq!(
        offline_wiki["sync_inventory"]["named_without_local_recovery"][0]["name"],
        "Office Laptop"
    );
    assert_eq!(
        offline_wiki["sync_inventory"]["named_without_backup"]
            .as_array()
            .unwrap()
            .len(),
        0,
        "without remote observation, do not assert that the named device has no remote backup"
    );

    let sync_offline = Command::cargo_bin("vulcan")
        .expect("vulcan binary")
        .current_dir(root)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &config)
        .env("XDG_STATE_HOME", &state)
        .env("XDG_DATA_HOME", &data)
        .env("GIT_TRACE", &trace)
        .args([
            "--vault",
            vault.to_str().unwrap(),
            "--output",
            "json",
            "sync",
            "devices",
            "list",
            "--offline",
        ])
        .output()
        .expect("offline per-vault inventory output");
    let sync_report = json_output(&sync_offline);
    assert_eq!(sync_report["remote_observation"]["state"], "not_requested");
    assert_eq!(
        sync_report["named_without_local_recovery"][0]["device_id"],
        named_id
    );
    assert_eq!(
        sync_report["named_without_backup"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    assert_no_remote_git_trace(&trace);

    let human = Command::cargo_bin("vulcan")
        .expect("vulcan binary")
        .current_dir(root)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &config)
        .env("XDG_STATE_HOME", &state)
        .env("XDG_DATA_HOME", &data)
        .args(["devices", "list", "--offline"])
        .output()
        .expect("offline human inventory output");
    assert!(
        human.status.success(),
        "{}",
        String::from_utf8_lossy(&human.stderr)
    );
    let text = String::from_utf8_lossy(&human.stdout);
    assert!(text.contains("does not establish trust or authorization"));
    assert!(text.contains("Remote safety backups: not requested (--offline); unknown"));
    assert!(text.contains("Office Laptop"));
    assert!(text.contains(named_id));
}

fn assert_no_remote_git_trace(trace: &Path) {
    let trace_text = fs::read_to_string(trace).expect("Git trace output");
    assert!(
        trace_text.contains("rev-parse"),
        "local Git checks should run"
    );
    assert!(
        !trace_text.contains("ls-remote"),
        "offline list must not invoke Git remote refs: {trace_text}"
    );
}

#[cfg(unix)]
#[test]
fn local_identity_commands_are_vault_independent_and_state_free_until_init() {
    let temporary = TempDir::new().expect("temporary directory");
    let root = temporary.path();
    let directory = root.join("data/vulcan/device");

    let absent = json_output(&run(root, &["--output", "json", "device", "show"]));
    assert_eq!(absent["status"], "uninitialized");
    assert!(!directory.exists());

    let preview = json_output(&run(
        root,
        &["--output", "json", "device", "init", "--dry-run"],
    ));
    assert_eq!(preview["dry_run"], true);
    assert_eq!(preview["created"], false);
    assert!(preview["identity"]["device_id"].is_null());
    assert!(!directory.exists());

    let created = json_output(&run(root, &["--output", "json", "device", "init"]));
    assert_eq!(created["created"], true);
    assert_eq!(created["identity"]["status"], "ready");
    let id = created["identity"]["device_id"]
        .as_str()
        .expect("new full device ID");
    assert!(id.starts_with("vdev1_"));
    assert_eq!(id.len(), 58);
    assert!(directory.join("identity.json").exists());
    assert!(directory.join("id_ed25519").exists());

    let shown = json_output(&run(root, &["--output", "json", "device", "show"]));
    assert_eq!(shown["status"], "ready");
    assert_eq!(shown["device_id"], id);
    assert_eq!(shown["sync_identity_state"], "key_pending_rollout");
    assert!(shown["sync_actor_id"].is_null());
    assert!(shown.get("public_key").is_none());
    assert!(
        !String::from_utf8_lossy(&run(root, &["device", "show"]).stdout)
            .contains("BEGIN OPENSSH PRIVATE KEY")
    );

    let exported = json_output(&run(root, &["--output", "json", "device", "public-key"]));
    assert!(exported["public_key"]
        .as_str()
        .expect("public key field")
        .starts_with("ssh-ed25519 "));
    assert_eq!(exported["version"], 1);

    let repeated = json_output(&run(root, &["--output", "json", "device", "init"]));
    assert_eq!(repeated["created"], false);
    assert_eq!(repeated["identity"]["device_id"], id);
}

#[cfg(windows)]
#[test]
fn local_identity_init_fails_closed_until_private_acl_support_is_available() {
    let temporary = TempDir::new().expect("temporary directory");
    let root = temporary.path();
    let preview = json_output(&run(
        root,
        &["--output", "json", "device", "init", "--dry-run"],
    ));
    assert_eq!(preview["dry_run"], true);
    let applied = run(root, &["device", "init"]);
    assert!(!applied.status.success());
    assert!(!root.join("data/vulcan/device").exists());
}

#[test]
fn local_identity_show_reports_legacy_ulid_without_creating_key_material() {
    let temporary = TempDir::new().expect("temporary directory");
    let root = temporary.path();
    let state = root.join("state/vulcan/sync/repositories");
    fs::create_dir_all(&state).expect("legacy sync state directory");
    fs::write(
        state.join("_device.json"),
        br#"{"version":1,"device_id":"01arz3ndektsv4rrffq69g5fav"}"#,
    )
    .expect("legacy identity");

    let shown = json_output(&run(root, &["--output", "json", "device", "show"]));
    assert_eq!(shown["status"], "legacy");
    assert!(shown["device_id"].is_null());
    assert_eq!(shown["sync_actor_id"], "01arz3ndektsv4rrffq69g5fav");
    assert!(!root.join("data/vulcan/device").exists());

    #[cfg(unix)]
    {
        let initialized = json_output(&run(root, &["--output", "json", "device", "init"]));
        assert_eq!(initialized["identity"]["status"], "ready");
        assert_eq!(
            initialized["identity"]["sync_actor_id"],
            "01arz3ndektsv4rrffq69g5fav"
        );
        assert_ne!(
            initialized["identity"]["device_id"],
            initialized["identity"]["sync_actor_id"]
        );
        assert_eq!(
            fs::read(state.join("_device.json")).expect("legacy state retained"),
            br#"{"version":1,"device_id":"01arz3ndektsv4rrffq69g5fav"}"#
        );
    }
}

#[test]
fn malformed_legacy_actor_does_not_hide_local_identity_inspection() {
    let temporary = TempDir::new().expect("temporary directory");
    let root = temporary.path();
    let state = root.join("state/vulcan/sync/repositories");
    fs::create_dir_all(&state).expect("legacy sync state directory");
    fs::write(state.join("_device.json"), b"not-json").expect("malformed legacy state");

    let shown = json_output(&run(root, &["--output", "json", "device", "show"]));
    assert_eq!(shown["status"], "uninitialized");
    assert_eq!(shown["sync_identity_state"], "legacy_unavailable");
    assert!(shown["sync_actor_id"].is_null());
    assert!(shown["diagnostic"]
        .as_str()
        .unwrap()
        .contains("vulcan sync doctor"));
    assert!(!root.join("data/vulcan/device").exists());

    #[cfg(unix)]
    {
        let initialized = json_output(&run(root, &["--output", "json", "device", "init"]));
        assert_eq!(initialized["identity"]["status"], "ready");
        assert_eq!(
            initialized["identity"]["sync_identity_state"],
            "legacy_unavailable"
        );
        let ready = json_output(&run(root, &["--output", "json", "device", "show"]));
        assert_eq!(ready["status"], "ready");
        assert_eq!(ready["device_id"], initialized["identity"]["device_id"]);
    }
}
