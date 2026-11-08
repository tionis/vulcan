//! `vulcan daemon plan` / `apply` over a declarative `daemon.toml`
//! (`docs/specs/declarative-device-config.md`, Roadmap 10.12.3–10.12.5).
//! Every test passes `--no-service` so the real per-user service manager is
//! never touched.

use assert_cmd::Command;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use tempfile::TempDir;

struct Device {
    _temporary: TempDir,
    root: PathBuf,
}

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Run {
    fn json(&self) -> Value {
        serde_json::from_str(&self.stdout)
            .unwrap_or_else(|error| panic!("{error}: {}\n{}", self.stdout, self.stderr))
    }
}

impl Device {
    fn new() -> Self {
        let temporary = TempDir::new().expect("temporary directory");
        let root = temporary.path().to_path_buf();
        fs::create_dir_all(root.join("home")).expect("home");
        fs::create_dir_all(root.join("config/vulcan")).expect("config");
        Self {
            _temporary: temporary,
            root,
        }
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.root.join(relative)
    }

    fn daemon_toml(&self) -> PathBuf {
        self.path("config/vulcan/daemon.toml")
    }

    fn identities(&self) -> PathBuf {
        self.path("config/vulcan/daemon.identities.toml")
    }

    fn write_config(&self, text: &str) {
        fs::write(self.daemon_toml(), text).expect("daemon.toml");
    }

    fn vault_dir(&self, name: &str) -> PathBuf {
        let path = self.path(name);
        fs::create_dir_all(path.join(".vulcan")).expect("vault");
        path
    }

    fn run(&self, args: &[&str]) -> Run {
        let output = Command::cargo_bin("vulcan")
            .expect("vulcan binary")
            .current_dir(&self.root)
            .env("HOME", self.path("home"))
            .env("XDG_CONFIG_HOME", self.path("config"))
            .env("XDG_DATA_HOME", self.path("data"))
            .env("XDG_STATE_HOME", self.path("state"))
            .env("GIT_AUTHOR_NAME", "Vulcan Test")
            .env("GIT_AUTHOR_EMAIL", "vulcan@example.invalid")
            .env("GIT_COMMITTER_NAME", "Vulcan Test")
            .env("GIT_COMMITTER_EMAIL", "vulcan@example.invalid")
            .args(args)
            .output()
            .expect("vulcan output");
        Run {
            code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    fn plan(&self, extra: &[&str]) -> Run {
        let mut args = vec!["--output", "json", "daemon", "plan", "--no-service"];
        args.extend_from_slice(extra);
        self.run(&args)
    }

    fn apply(&self, extra: &[&str]) -> Run {
        let mut args = vec!["--output", "json", "daemon", "apply", "--no-service"];
        args.extend_from_slice(extra);
        self.run(&args)
    }
}

fn git(directory: &Path, args: &[&str]) {
    let output = ProcessCommand::new("git")
        .current_dir(directory)
        .args(["-c", "user.name=Vulcan Test"])
        .args(["-c", "user.email=vulcan@example.invalid"])
        .args(["-c", "init.defaultBranch=main"])
        .args(args)
        .output()
        .expect("git");
    assert!(output.status.success(), "git {args:?}: {output:?}");
}

/// A bare repository holding one committed vault.
fn remote_repository(device: &Device, name: &str) -> PathBuf {
    let seed = device.path(&format!("{name}-seed"));
    fs::create_dir_all(seed.join(".vulcan")).expect("seed");
    fs::write(seed.join("Home.md"), "# Home\n").expect("note");
    git(&seed, &["init"]);
    git(&seed, &["add", "."]);
    git(&seed, &["commit", "-m", "seed"]);
    let bare = device.path(&format!("{name}.git"));
    git(
        &device.root,
        &[
            "clone",
            "--bare",
            seed.to_str().unwrap(),
            bare.to_str().unwrap(),
        ],
    );
    bare
}

fn item<'a>(plan: &'a Value, kind: &str, id: &str) -> &'a Value {
    plan["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|item| item["kind"] == kind && item["id"] == id)
        .unwrap_or_else(|| panic!("no {kind} {id} in {plan:#}"))
}

#[test]
fn apply_stores_identities_and_a_second_plan_is_empty() {
    let device = Device::new();
    let notes = device.vault_dir("notes");
    device.write_config(&format!(
        "# hand written\n[[vault]]\nid = \"notes\"\npath = \"{}\"\n",
        notes.display()
    ));

    let plan = device.plan(&[]);
    assert_eq!(plan.code, 2, "{}{}", plan.stdout, plan.stderr);
    assert_eq!(
        item(&plan.json(), "identity", "vault.notes")["action"],
        "create"
    );

    let dry = device.apply(&["--dry-run"]);
    assert_eq!(dry.code, 2, "{}", dry.stderr);
    assert!(!device.identities().exists(), "--dry-run writes nothing");

    let applied = device.apply(&[]);
    assert_eq!(applied.code, 0, "{}{}", applied.stdout, applied.stderr);
    assert!(fs::read_to_string(device.identities())
        .expect("identities")
        .contains("[vault.notes]"));
    assert!(fs::read_to_string(device.daemon_toml())
        .expect("daemon.toml")
        .starts_with("# hand written\n"));

    let again = device.plan(&[]);
    assert_eq!(again.code, 0, "{}{}", again.stdout, again.stderr);
}

#[test]
fn apply_clones_missing_vaults_and_reports_bad_ones_without_stopping() {
    let device = Device::new();
    let good = remote_repository(&device, "good");
    let other = remote_repository(&device, "other");
    // An existing checkout whose remote is not the declared source.
    let wrong = device.path("wrong");
    git(
        &device.root,
        &["clone", other.to_str().unwrap(), wrong.to_str().unwrap()],
    );
    device.write_config(&format!(
        "[[vault]]\nid = \"good\"\npath = \"{}\"\nsource = \"{}\"\nsync_backend = \"git\"\n\n\
         [[vault]]\nid = \"lost\"\npath = \"{}\"\n\n\
         [[vault]]\nid = \"wrong\"\npath = \"{}\"\nsource = \"{}\"\n",
        device.path("good").display(),
        good.display(),
        device.path("lost").display(),
        wrong.display(),
        good.display(),
    ));

    let plan = device.plan(&[]);
    assert_eq!(plan.code, 1, "vault errors make plan fail");
    let plan = plan.json();
    assert_eq!(item(&plan, "vault", "good")["action"], "clone");
    assert_eq!(item(&plan, "vault", "lost")["action"], "error");
    let wrong_item = item(&plan, "vault", "wrong");
    assert_eq!(wrong_item["action"], "error");
    assert!(wrong_item["reason"]
        .as_str()
        .unwrap()
        .contains("but `source` is"));

    let applied = device.apply(&[]);
    assert_eq!(applied.code, 1, "{}{}", applied.stdout, applied.stderr);
    assert!(device.path("good/Home.md").is_file(), "{}", applied.stdout);
    let report = applied.json();
    let clone_step = report["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|step| step["id"] == "good")
        .expect("clone step");
    assert_eq!(clone_step["ok"], true, "{report:#}");

    let plan = device.plan(&[]).json();
    assert_eq!(item(&plan, "vault", "good")["action"], "ok");
}

#[test]
fn a_reviewed_file_is_checked_without_touching_the_live_config() {
    let device = Device::new();
    let notes = device.vault_dir("notes");
    let live = format!(
        "[[vault]]\nid = \"notes\"\npath = \"{}\"\n",
        notes.display()
    );
    device.write_config(&live);
    assert_eq!(device.apply(&[]).code, 0);

    let extra = device.vault_dir("extra");
    let reviewed = device.path("new-daemon.toml");
    fs::write(
        &reviewed,
        format!(
            "bind = \"127.0.0.1:3999\"\n{live}\n[[vault]]\nid = \"extra\"\npath = \"{}\"\n",
            extra.display()
        ),
    )
    .unwrap();
    let file = reviewed.to_str().unwrap();
    let plan = device.plan(&["--file", file]);
    assert_eq!(plan.code, 2);
    let plan = plan.json();
    assert_eq!(plan["live"], false);
    assert_eq!(item(&plan, "vault", "extra")["action"], "create");
    assert_eq!(item(&plan, "config", "bind")["action"], "update");

    let applied = device.apply(&["--file", file]);
    assert_eq!(applied.code, 2);
    assert_eq!(fs::read_to_string(device.daemon_toml()).unwrap(), live);
    assert!(!fs::read_to_string(device.identities())
        .unwrap()
        .contains("extra"));
}

#[test]
fn undeclared_state_is_drift_until_pruned() {
    let device = Device::new();
    let notes = device.vault_dir("notes");
    let old = device.vault_dir("old");
    device.write_config(&format!(
        "[[vault]]\nid = \"notes\"\npath = \"{}\"\n\n[[vault]]\nid = \"old\"\npath = \"{}\"\n",
        notes.display(),
        old.display()
    ));
    assert_eq!(device.apply(&[]).code, 0);
    let scans = device.path("state/vulcan/sync/repositories/daemon/scans");
    fs::create_dir_all(&scans).unwrap();
    let stale_scan = scans.join("01ARZ3NDEKTSV4RRFFQ69G5FAV.json");
    fs::write(&stale_scan, "{}").unwrap();
    // Remove `old` by hand: its identity stays as drift.
    device.write_config(&format!(
        "[[vault]]\nid = \"notes\"\npath = \"{}\"\n",
        notes.display()
    ));

    let plan = device.plan(&[]);
    assert_eq!(plan.code, 0, "drift alone is not a pending change");
    let plan = plan.json();
    assert_eq!(item(&plan, "identity", "vault.old")["action"], "drift");
    assert_eq!(
        item(&plan, "scan_state", stale_scan.to_str().unwrap())["action"],
        "drift"
    );
    assert_eq!(device.plan(&["--prune"]).code, 2);

    assert_eq!(device.apply(&[]).code, 0);
    assert!(stale_scan.exists(), "apply without --prune keeps drift");
    assert!(fs::read_to_string(device.identities())
        .unwrap()
        .contains("[vault.old]"));

    let pruned = device.apply(&["--prune"]);
    assert_eq!(pruned.code, 0, "{}{}", pruned.stdout, pruned.stderr);
    assert!(!stale_scan.exists());
    assert!(!fs::read_to_string(device.identities())
        .unwrap()
        .contains("[vault.old]"));
    assert!(old.is_dir(), "vault files are never deleted");
    assert_eq!(device.plan(&["--prune"]).code, 0);
}

#[test]
fn remote_ceilings_resolve_from_daemon_toml() {
    let device = Device::new();
    let notes = device.vault_dir("notes");
    let config = |profiles: &str| {
        format!(
            "[[vault]]\nid = \"notes\"\npath = \"{}\"\n\n\
             [[mcp_remote]]\nversion = 1\nid = \"notes\"\nbind = \"127.0.0.1:18765\"\n\
             public_url = \"https://notes.example.invalid/mcp\"\n\n\
             [mcp_remote.authentication]\ntype = \"indie_auth\"\nidentity = \"https://me.example.invalid/\"\n\n\
             [[mcp_remote.vaults]]\nwiki_id = \"notes\"\nceiling_profile = \"notes-rw\"\n\
             default_profile = \"notes-rw\"\ntool_packs = [\"notes-read\"]\n{profiles}",
            notes.display()
        )
    };
    device.write_config(&config(""));
    let plan = device.plan(&[]);
    assert_eq!(plan.code, 1);
    assert_eq!(item(&plan.json(), "mcp_remote", "notes")["action"], "error");
    let refused = device.apply(&[]);
    assert_eq!(refused.code, 1);
    assert!(
        !device.identities().exists(),
        "errors stop apply before any change"
    );

    device.write_config(&config(
        "\n[permissions.profiles.notes-rw]\nread = \"all\"\nwrite = \"all\"\n",
    ));
    let plan = device.plan(&[]);
    assert_eq!(plan.code, 2, "{}", plan.stdout);
    assert_eq!(item(&plan.json(), "mcp_remote", "notes")["action"], "ok");
    assert_eq!(device.apply(&[]).code, 0);
    assert!(fs::read_to_string(device.identities())
        .unwrap()
        .contains("[mcp_remote.notes]"));
}

#[test]
fn a_managed_file_refuses_cli_edits_unless_forced() {
    let device = Device::new();
    let notes = device.vault_dir("notes");
    device.write_config(&format!(
        "managed_by = \"chezmoi\"\n\n[[vault]]\nid = \"notes\"\npath = \"{}\"\n",
        notes.display()
    ));
    let before = fs::read_to_string(device.daemon_toml()).unwrap();
    let extra = device.vault_dir("extra");

    let refused = device.run(&["vault", "add", "extra", extra.to_str().unwrap()]);
    assert_ne!(refused.code, 0);
    let message = format!("{}{}", refused.stdout, refused.stderr);
    assert!(message.contains("managed by chezmoi"), "{message}");
    assert!(
        message.contains("[[vault]]") && message.contains("id = \"extra\""),
        "{message}"
    );
    assert_eq!(fs::read_to_string(device.daemon_toml()).unwrap(), before);

    let device_target = device.run(&[
        "--vault",
        notes.to_str().unwrap(),
        "config",
        "set",
        "periodic.daily.template",
        "Day",
        "--target",
        "device",
    ]);
    assert_ne!(device_target.code, 0);
    assert_eq!(fs::read_to_string(device.daemon_toml()).unwrap(), before);

    let forced = device.run(&[
        "--force-local-edit",
        "vault",
        "add",
        "extra",
        extra.to_str().unwrap(),
    ]);
    assert_eq!(forced.code, 0, "{}{}", forced.stdout, forced.stderr);
    assert!(
        forced.stderr.contains("next render by chezmoi reverts"),
        "{}",
        forced.stderr
    );
    assert!(fs::read_to_string(device.daemon_toml())
        .unwrap()
        .contains("id = \"extra\""));

    let plan = device.plan(&[]).json();
    assert_eq!(plan["managed_by"], "chezmoi");
}
