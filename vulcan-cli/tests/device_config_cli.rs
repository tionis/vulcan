//! Device-local settings written to and moved into `daemon.toml`
//! (`docs/specs/declarative-device-config.md`, Roadmap 10.12.2).

use assert_cmd::Command;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

struct Device {
    _temporary: TempDir,
    root: PathBuf,
    vault: PathBuf,
}

impl Device {
    /// A device with one vault, registered in a hand-written `daemon.toml`.
    fn new() -> Self {
        let temporary = TempDir::new().expect("temporary directory");
        let root = temporary.path().to_path_buf();
        let vault = root.join("vault");
        fs::create_dir_all(vault.join(".vulcan")).expect("vault directory");
        fs::create_dir_all(root.join("home")).expect("home directory");
        let device = Self {
            _temporary: temporary,
            root,
            vault,
        };
        fs::create_dir_all(device.daemon_toml().parent().expect("config directory"))
            .expect("config directory");
        fs::write(
            device.daemon_toml(),
            format!(
                "# my device\n[[vault]]\nid = \"notes\"\npath = \"{}\"\n",
                device.vault.display()
            ),
        )
        .expect("daemon.toml");
        device
    }

    fn daemon_toml(&self) -> PathBuf {
        self.root.join("config/vulcan/daemon.toml")
    }

    fn local_config(&self) -> PathBuf {
        self.vault.join(".vulcan/config.local.toml")
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        Command::cargo_bin("vulcan")
            .expect("vulcan binary")
            .current_dir(&self.root)
            .env("HOME", self.root.join("home"))
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_DATA_HOME", self.root.join("data"))
            .env("XDG_STATE_HOME", self.root.join("state"))
            .arg("--vault")
            .arg(&self.vault)
            .args(["--output", "json"])
            .args(args)
            .output()
            .expect("vulcan output")
    }

    fn json(&self, args: &[&str]) -> Value {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "vulcan {args:?}\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).expect("json output")
    }

    fn fails(&self, args: &[&str]) -> String {
        let output = self.run(args);
        assert!(!output.status.success(), "vulcan {args:?} should fail");
        format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    }

    fn device_file(&self) -> toml::Table {
        read_table(&self.daemon_toml())
    }
}

fn read_table(path: &Path) -> toml::Table {
    fs::read_to_string(path)
        .expect("readable file")
        .parse()
        .expect("valid toml")
}

fn list_entry<'a>(list: &'a Value, key: &str) -> &'a Value {
    list["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .find(|entry| entry["key"] == key)
        .unwrap_or_else(|| panic!("no entry for {key}"))
}

#[test]
fn target_device_writes_settings_and_profiles_into_daemon_toml() {
    let device = Device::new();

    device.json(&[
        "config",
        "set",
        "periodic.daily.template",
        "Templates/Day",
        "--target",
        "device",
    ]);
    device.json(&[
        "config",
        "permissions",
        "profile",
        "create",
        "agent",
        "--clone",
        "readonly",
        "--target",
        "device",
    ]);

    let text = fs::read_to_string(device.daemon_toml()).expect("daemon.toml");
    assert!(
        text.starts_with("# my device\n"),
        "comments survive: {text}"
    );
    let file = device.device_file();
    let vault = &file["vault"].as_array().expect("vaults")[0];
    assert_eq!(
        vault["config"]["periodic"]["daily"]["template"].as_str(),
        Some("Templates/Day")
    );
    assert!(
        file["permissions"]["profiles"].get("agent").is_some(),
        "profiles go to the device-global [permissions]: {text}"
    );
    assert!(!device.local_config().exists());
    let shared = fs::read_to_string(device.vault.join(".vulcan/config.toml")).unwrap_or_default();
    assert!(!shared.contains("Templates/Day"), "{shared}");

    let value = device.json(&["config", "get", "periodic.daily.template"]);
    assert_eq!(value["value"], "Templates/Day");
    let list = device.json(&["config", "list", "periodic.daily"]);
    assert_eq!(
        list_entry(&list, "periodic.daily.template")["value_source"],
        "device_override"
    );

    device.json(&[
        "config",
        "unset",
        "periodic.daily.template",
        "--target",
        "device",
    ]);
    let file = device.device_file();
    let vault = &file["vault"].as_array().expect("vaults")[0];
    assert!(vault.get("config").is_none(), "empty config table removed");
}

#[test]
fn target_device_refuses_an_unregistered_vault() {
    let device = Device::new();
    fs::write(device.daemon_toml(), "").expect("empty daemon.toml");
    let error = device.fails(&[
        "config",
        "set",
        "periodic.daily.template",
        "Day",
        "--target",
        "device",
    ]);
    assert!(error.contains("vulcan vault add"), "{error}");
}

#[test]
fn profile_move_takes_a_local_profile_into_daemon_toml() {
    let device = Device::new();
    fs::write(
        device.local_config(),
        "[permissions.profiles.agent]\nread = \"all\"\n\n[periodic.daily]\ntemplate = \"Day\"\n",
    )
    .expect("local config");

    let preview = device.json(&[
        "config",
        "permissions",
        "profile",
        "move",
        "agent",
        "--dry-run",
    ]);
    assert_eq!(preview["dry_run"], true);
    assert_eq!(preview["keys"][0], "permissions.profiles.agent");
    assert_eq!(preview["removes_local_file"], false);
    assert!(!device.device_file().contains_key("permissions"));

    device.json(&["config", "permissions", "profile", "move", "agent"]);
    assert_eq!(
        device.device_file()["permissions"]["profiles"]["agent"]["read"].as_str(),
        Some("all")
    );
    let local = read_table(&device.local_config());
    assert!(local.get("permissions").is_none(), "{local:?}");
    assert_eq!(local["periodic"]["daily"]["template"].as_str(), Some("Day"));

    // The profile now resolves from the device file.
    let shown = device.json(&["config", "permissions", "profile", "show", "agent"]);
    assert_eq!(shown["config"]["read"], "all");
}

#[test]
fn profile_move_refuses_a_different_device_definition() {
    let device = Device::new();
    let mut text = fs::read_to_string(device.daemon_toml()).expect("daemon.toml");
    text.push_str("\n[permissions.profiles.agent]\nread = \"none\"\n");
    fs::write(device.daemon_toml(), text).expect("daemon.toml");
    fs::write(
        device.local_config(),
        "[permissions.profiles.agent]\nread = \"all\"\n",
    )
    .expect("local config");
    let error = device.fails(&["config", "permissions", "profile", "move", "agent"]);
    assert!(error.contains("differently"), "{error}");
    assert!(device.local_config().exists());
}

#[test]
fn local_move_empties_config_local_toml_into_the_vault_entry() {
    let device = Device::new();
    fs::write(
        device.local_config(),
        "[periodic.daily]\ntemplate = \"Day\"\n\n[git]\nauto_commit = true\n",
    )
    .expect("local config");

    let preview = device.json(&["config", "local", "move", "--dry-run"]);
    assert_eq!(preview["removes_local_file"], true);
    assert!(device.local_config().exists());

    let report = device.json(&["config", "local", "move"]);
    let mut keys = report["keys"]
        .as_array()
        .expect("keys")
        .iter()
        .map(|key| key.as_str().expect("key").to_string())
        .collect::<Vec<_>>();
    keys.sort();
    assert_eq!(keys, ["git.auto_commit", "periodic.daily.template"]);
    assert!(!device.local_config().exists());
    let file = device.device_file();
    let config = &file["vault"].as_array().expect("vaults")[0]["config"];
    assert_eq!(
        config["periodic"]["daily"]["template"].as_str(),
        Some("Day")
    );
    assert_eq!(config["git"]["auto_commit"].as_bool(), Some(true));

    let value = device.json(&["config", "get", "periodic.daily.template"]);
    assert_eq!(value["value"], "Day");
}

#[test]
fn local_move_refuses_conflicting_device_settings() {
    let device = Device::new();
    let mut text = fs::read_to_string(device.daemon_toml()).expect("daemon.toml");
    text.push_str("\n[vault.config.periodic.daily]\ntemplate = \"Other\"\n");
    fs::write(device.daemon_toml(), text).expect("daemon.toml");
    fs::write(
        device.local_config(),
        "[periodic.daily]\ntemplate = \"Day\"\n",
    )
    .expect("local config");
    let error = device.fails(&["config", "local", "move"]);
    assert!(error.contains("periodic.daily.template"), "{error}");
    assert!(device.local_config().exists());
}
