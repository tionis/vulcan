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
        .env("XDG_CONFIG_HOME", root.join("config"))
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

#[test]
fn stale_legacy_sync_actor_state_is_ignored() {
    let temporary = TempDir::new().expect("temporary directory");
    let root = temporary.path();
    let state = root.join("state/vulcan/sync/repositories");
    fs::create_dir_all(&state).expect("legacy sync state directory");
    let legacy = br#"{"version":1,"device_id":"01arz3ndektsv4rrffq69g5fav"}"#;
    fs::write(state.join("_device.json"), legacy).expect("legacy identity");

    let shown = json_output(&run(root, &["--output", "json", "device", "show"]));
    assert_eq!(shown["status"], "uninitialized");
    assert!(shown["device_id"].is_null());
    assert!(!root.join("data/vulcan/device").exists());

    let initialized = json_output(&run(root, &["--output", "json", "device", "init"]));
    assert_eq!(initialized["identity"]["status"], "ready");
    assert!(initialized["identity"]["device_id"]
        .as_str()
        .is_some_and(|id| id.starts_with("vdev1_")));
    assert_eq!(
        fs::read(state.join("_device.json")).expect("legacy state retained"),
        legacy
    );
}

#[cfg(unix)]
#[test]
fn repair_permissions_restricts_loose_identity_storage() {
    use std::os::unix::fs::PermissionsExt;
    let temporary = TempDir::new().expect("temporary directory");
    let root = temporary.path();
    let directory = root.join("data/vulcan/device");
    json_output(&run(root, &["--output", "json", "device", "init"]));
    fs::set_permissions(
        directory.join("id_ed25519"),
        fs::Permissions::from_mode(0o640),
    )
    .expect("loosen key");

    let shown = json_output(&run(root, &["--output", "json", "device", "show"]));
    assert_eq!(shown["status"], "invalid");

    let preview = json_output(&run(
        root,
        &[
            "--output",
            "json",
            "device",
            "repair-permissions",
            "--dry-run",
        ],
    ));
    assert_eq!(preview["dry_run"], true);
    assert_eq!(preview["repaired"][0], "id_ed25519");

    let repaired = json_output(&run(
        root,
        &["--output", "json", "device", "repair-permissions"],
    ));
    assert_eq!(repaired["identity"]["status"], "ready");
    assert!(!repaired.to_string().contains(root.to_str().unwrap()));
}

#[test]
#[allow(clippy::too_many_lines)] // One ordered bind/opt-out/foreign/unbind lifecycle reads best unbroken.
fn transport_binding_roundtrip_is_dry_run_safe_and_owns_only_its_git_config() {
    let temporary = TempDir::new().expect("temporary directory");
    let root = temporary.path();
    let vault = root.join("vault");
    fs::create_dir_all(&vault).expect("vault directory");
    for args in [
        &["init", "--quiet"][..],
        &["remote", "add", "origin", "git@forge.example:o/r.git"],
    ] {
        assert!(ProcessCommand::new("git")
            .current_dir(&vault)
            .args(args)
            .status()
            .expect("git")
            .success());
    }
    let vault_arg = vault.to_str().expect("utf-8 vault path");
    let transport = |extra: &[&str]| {
        let mut args = vec![
            "--vault",
            vault_arg,
            "--output",
            "json",
            "sync",
            "transport",
        ];
        args.extend_from_slice(extra);
        run(root, &args)
    };

    // Binding never initializes the identity implicitly.
    let refused = transport(&["bind"]);
    assert!(!refused.status.success());
    let refusal = format!(
        "{}{}",
        String::from_utf8_lossy(&refused.stdout),
        String::from_utf8_lossy(&refused.stderr)
    );
    assert!(refusal.contains("vulcan device init"), "{refusal}");

    json_output(&run(root, &["device", "init", "--output", "json"]));
    // The default configures plain git too: nothing to remember to pass.
    let preview = json_output(&transport(&["bind", "--dry-run"]));
    assert_eq!(preview["dry_run"], true);
    assert_eq!(preview["changed"], true);
    assert_eq!(preview["git_config_written"], true);
    assert_eq!(
        json_output(&transport(&["status"]))["state"],
        "not_bound",
        "dry run must not bind"
    );
    let plain_git = |args: &[&str]| {
        let output = ProcessCommand::new("git")
            .current_dir(&vault)
            .args(args)
            .output()
            .expect("git");
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    };
    assert_eq!(
        plain_git(&["config", "--local", "--get", "core.sshCommand"]),
        ""
    );

    let bound = json_output(&transport(&["bind"]));
    assert_eq!(bound["git_config_written"], true);
    assert!(
        plain_git(&["config", "--local", "--get", "core.sshCommand"])
            .contains("device ssh-command")
    );
    let status = json_output(&transport(&["status"]));
    assert_eq!(status["state"], "usable");
    assert_eq!(status["git_config"], "managed");
    let rendered = status.to_string();
    assert!(
        !rendered.contains("id_ed25519"),
        "key path must not be reported"
    );
    assert!(
        !plain_git(&["config", "--local", "--get", "core.sshCommand"]).contains("id_ed25519"),
        "nor written to Git config"
    );

    // Opting out removes the Vulcan-owned value but keeps the binding.
    let opted_out = json_output(&transport(&["bind", "--no-git-config"]));
    assert_eq!(opted_out["git_config_removed"], true);
    assert_eq!(
        plain_git(&["config", "--local", "--get", "core.sshCommand"]),
        ""
    );
    let status = json_output(&transport(&["status"]));
    assert_eq!(
        (status["state"].as_str(), status["git_config"].as_str()),
        (Some("usable"), Some("not_managed"))
    );

    // Another tool's value is left alone and never blocks the default bind.
    plain_git(&["config", "--local", "core.sshCommand", "ssh -i /mine"]);
    let skipped = json_output(&transport(&["bind"]));
    assert_eq!(skipped["git_config_written"], false);
    assert!(skipped["git_config_skipped"]
        .as_str()
        .unwrap()
        .contains("left alone"));
    assert_eq!(
        plain_git(&["config", "--local", "--get", "core.sshCommand"]),
        "ssh -i /mine"
    );
    // `--git-config` insists, so it refuses instead.
    assert!(!transport(&["bind", "--git-config"]).status.success());
    plain_git(&["config", "--local", "--unset", "core.sshCommand"]);

    json_output(&transport(&["bind"]));
    let unbound = json_output(&transport(&["unbind"]));
    assert_eq!(unbound["git_config_removed"], true);
    assert_eq!(json_output(&transport(&["status"]))["state"], "not_bound");
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

/// Two installations, each with its own identity, state, and vault clone of
/// the same bare remote.
fn two_installations(root: &Path, remote: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let admin = root.join("admin");
    let device = root.join("device");
    for installation in [&admin, &device] {
        let vault = installation.join("vault");
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
        fs::write(vault.join("Home.md"), "note\n").expect("note");
        git(&vault, &["add", "Home.md"]);
        git(&vault, &["commit", "--quiet", "-m", "initial"]);
    }
    (admin, device)
}

#[test]
#[allow(clippy::too_many_lines)] // One ordered admin/device lifecycle reads best unbroken.
fn placeholder_is_claimed_by_the_device_and_revocation_sticks() {
    let temporary = TempDir::new().expect("temporary directory");
    let root = temporary.path();
    let remote = root.join("remote.git");
    git(root, &["init", "--bare", "--quiet", "remote.git"]);
    let (admin, device) = two_installations(root, &remote);
    let admin_vault = admin.join("vault");
    let device_vault = device.join("vault");
    let admin_vault_arg = admin_vault.to_str().unwrap();
    let device_vault_arg = device_vault.to_str().unwrap();

    let device_key = json_output(&run(&device, &["device", "init", "--output", "json"]));
    let device_id = device_key["identity"]["device_id"]
        .as_str()
        .expect("device id")
        .to_owned();
    let public_key = json_output(&run(&device, &["device", "public-key", "--output", "json"]));
    let key_file = root.join("device.pub");
    fs::write(
        &key_file,
        format!(
            "{} laptop@home\n",
            public_key["public_key"].as_str().unwrap()
        ),
    )
    .expect("key file");

    let devices = |vault: &str, extra: &[&str]| {
        let mut args = vec!["--vault", vault, "--output", "json", "sync", "devices"];
        args.extend_from_slice(extra);
        run(
            if vault == admin_vault_arg {
                &admin
            } else {
                &device
            },
            &args,
        )
    };

    let preview = json_output(&devices(
        admin_vault_arg,
        &[
            "register",
            "--public-key",
            key_file.to_str().unwrap(),
            "--label",
            "Laptop",
            "--dry-run",
        ],
    ));
    assert_eq!(preview["action"], "created");
    assert_eq!(preview["dry_run"], true);
    let created = json_output(&devices(
        admin_vault_arg,
        &[
            "register",
            "--public-key",
            key_file.to_str().unwrap(),
            "--label",
            "Laptop",
        ],
    ));
    assert_eq!(created["device_id"], device_id);
    assert_eq!(created["status"], "placeholder");

    let listed = json_output(&devices(admin_vault_arg, &["list"]));
    let entry = &listed["registrations"]["registrations"][0];
    assert_eq!(entry["status"], "placeholder");
    assert_eq!(entry["label"], "Laptop");
    assert_eq!(entry["current_device"], false);

    // The device's first sync claims its placeholder.
    let sync = json_output(&run(
        &device,
        &[
            "--vault",
            device_vault_arg,
            "--output",
            "json",
            "sync",
            "run",
        ],
    ));
    assert_eq!(sync["registration"]["outcome"], "claimed");
    let listed = json_output(&devices(admin_vault_arg, &["list"]));
    assert_eq!(
        listed["registrations"]["registrations"][0]["status"],
        "registered"
    );
    let own = json_output(&devices(device_vault_arg, &["list", "--offline"]));
    assert_eq!(
        own["registrations"]["registrations"][0]["current_device"],
        true
    );
    assert_eq!(own["registrations"]["observation"], "not_requested");

    let revoked = json_output(&devices(admin_vault_arg, &["revoke", &device_id]));
    assert_eq!(revoked["status"], "revoked");
    let listed = json_output(&devices(admin_vault_arg, &["list"]));
    assert_eq!(
        listed["registrations"]["registrations"][0]["status"],
        "revoked"
    );

    // Partial IDs are refused; unregister then forgets the tombstone.
    assert!(!devices(admin_vault_arg, &["revoke", "vdev1_short"])
        .status
        .success());
    let removed = json_output(&devices(admin_vault_arg, &["unregister", &device_id]));
    assert_eq!(removed["action"], "removed");
    let listed = json_output(&devices(admin_vault_arg, &["list"]));
    assert_eq!(listed["registrations"]["count"], 0);
}

#[test]
fn device_config_commands_edit_only_the_device_file_and_never_store_secrets() {
    let temporary = TempDir::new().expect("temporary directory");
    let root = temporary.path();
    let config = |args: &[&str]| {
        let mut full = vec!["--output", "json", "device", "config"];
        full.extend_from_slice(args);
        run(root, &full)
    };
    let file = root.join("config").join("vulcan").join("device.toml");

    // Defaults apply without a file, and reading never creates one.
    let shown = json_output(&config(&["show"]));
    assert_eq!(shown["exists"], false);
    assert_eq!(shown["config"]["transport"]["default"], "device-key");
    assert!(!file.exists());

    // A dry run validates and writes nothing.
    let preview = json_output(&config(&["set-transport", "ambient", "--dry-run"]));
    assert_eq!(
        (preview["dry_run"].as_bool(), preview["changed"].as_bool()),
        (Some(true), Some(true))
    );
    assert!(!file.exists());

    json_output(&config(&["set-transport", "ambient"]));
    json_output(&config(&[
        "set-forge",
        "Forge.Example.com",
        "--kind",
        "forgejo",
        "--oauth-client-id",
        "client-abc",
        "--token-env",
        "FORGE_TOKEN",
        "--transport",
        "device-key",
    ]));
    json_output(&config(&[
        "set-forge",
        "github.com",
        "--transport",
        "ambient",
    ]));
    let shown = json_output(&config(&["show"]));
    assert_eq!(shown["exists"], true);
    assert_eq!(shown["config"]["transport"]["default"], "ambient");
    let forges = shown["config"]["forge"].as_array().expect("forge entries");
    assert_eq!(forges.len(), 2);
    assert_eq!(
        forges[0]["host"], "forge.example.com",
        "hosts are lowercased and sorted"
    );
    assert_eq!(forges[0]["oauth_client_id"], "client-abc");
    assert_eq!(forges[1]["host"], "github.com");
    assert!(forges[1].get("kind").is_none());

    // Unsafe input is refused before anything is written.
    let before = fs::read_to_string(&file).expect("device.toml");
    assert!(!config(&["set-forge", "https://evil.example"])
        .status
        .success());
    assert!(
        !config(&["set-forge", "forge.other.example", "--token-env", "TOKEN"])
            .status
            .success(),
        "a credential needs a kind"
    );
    assert!(!config(&[
        "set-forge",
        "forge.other.example",
        "--kind",
        "forgejo",
        "--token-env",
        "A B"
    ])
    .status
    .success());
    assert_eq!(fs::read_to_string(&file).unwrap(), before);

    // Only the variable name is stored; no file other than device.toml appears.
    assert!(before.contains("FORGE_TOKEN") && !before.to_lowercase().contains("secret"));
    let entries = fs::read_dir(file.parent().unwrap()).unwrap().count();
    assert_eq!(entries, 1, "only device.toml was written");

    json_output(&config(&["remove-forge", "github.com"]));
    assert!(!config(&["remove-forge", "github.com"]).status.success());
    assert_eq!(
        json_output(&config(&["show"]))["config"]["forge"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn replace_swaps_the_identity_archives_the_old_one_and_is_dry_run_safe() {
    let temporary = TempDir::new().expect("temporary directory");
    let root = temporary.path();

    let missing = run(root, &["device", "replace"]);
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("device init"));

    json_output(&run(root, &["device", "init", "--output", "json"]));
    let old = json_output(&run(root, &["device", "show", "--output", "json"]));
    let old_id = old["device_id"].as_str().expect("old id").to_owned();

    let preview = json_output(&run(
        root,
        &["device", "replace", "--dry-run", "--output", "json"],
    ));
    assert_eq!(preview["dry_run"], true);
    assert_eq!(preview["activated"], false);
    assert!(!root.join("data/vulcan/device-staged").exists());
    let still = json_output(&run(root, &["device", "show", "--output", "json"]));
    assert_eq!(still["device_id"], old_id.as_str());

    let done = json_output(&run(root, &["device", "replace", "--output", "json"]));
    assert_eq!(done["activated"], true);
    assert_eq!(done["old_device_id"], old_id.as_str());
    let new_id = done["new_device_id"].as_str().expect("new id").to_owned();
    assert_ne!(new_id, old_id);
    assert!(
        !done.to_string().contains("PRIVATE KEY"),
        "no key material in output"
    );
    let shown = json_output(&run(root, &["device", "show", "--output", "json"]));
    assert_eq!(shown["device_id"], new_id.as_str());
    assert_eq!(shown["status"], "ready");

    // No vaults are registered, so retiring the old device reaches nothing, and says so.
    let revoked = json_output(&run(
        root,
        &["devices", "revoke", &old_id, "--output", "json"],
    ));
    assert_eq!(revoked["complete"], 0);
    assert_eq!(revoked["incomplete"], 0);
}
