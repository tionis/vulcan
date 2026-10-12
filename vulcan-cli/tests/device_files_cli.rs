//! Per-device Obsidian files: sync only hints, `sync doctor --fix` repairs,
//! and setting a vault up for sync ignores them unless asked not to.

use assert_cmd::Command;
use serde_json::Value;
use std::fs;
use std::path::Path;
use std::process::Command as ProcessCommand;
use tempfile::TempDir;

const LAYOUT: &str = ".obsidian/workspace.json";

fn git(directory: &Path, args: &[&str]) {
    let output = ProcessCommand::new("git")
        .current_dir(directory)
        .args(["-c", "user.name=Vulcan Test"])
        .args(["-c", "user.email=vulcan@example.invalid"])
        .args(args)
        .output()
        .expect("git");
    assert!(output.status.success(), "git {args:?}: {output:?}");
}

fn vulcan(root: &Path, args: &[&str]) -> std::process::Output {
    let home = root.join("home");
    fs::create_dir_all(&home).expect("home directory");
    Command::cargo_bin("vulcan")
        .expect("vulcan binary")
        .current_dir(root)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("GIT_AUTHOR_NAME", "Vulcan Test")
        .env("GIT_AUTHOR_EMAIL", "vulcan@example.invalid")
        .env("GIT_COMMITTER_NAME", "Vulcan Test")
        .env("GIT_COMMITTER_EMAIL", "vulcan@example.invalid")
        .args(args)
        .output()
        .expect("vulcan output")
}

fn succeeded(output: &std::process::Output) -> (String, String) {
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        output.status.success(),
        "stdout: {stdout}\nstderr: {stderr}"
    );
    (stdout, stderr)
}

/// An Obsidian vault that tracks its layout, with a bare remote.
fn obsidian_vault(root: &Path, name: &str) -> std::path::PathBuf {
    let remote = root.join(format!("{name}.git"));
    git(
        root,
        &[
            "-c",
            "init.defaultBranch=main",
            "init",
            "--quiet",
            "--bare",
            remote.to_str().unwrap(),
        ],
    );
    let vault = root.join(name);
    fs::create_dir_all(vault.join(".obsidian")).expect("obsidian directory");
    git(
        &vault,
        &["-c", "init.defaultBranch=main", "init", "--quiet"],
    );
    git(
        &vault,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    fs::write(vault.join(LAYOUT), "{}\n").expect("layout");
    fs::write(vault.join("Home.md"), "home\n").expect("note");
    git(&vault, &["add", "--all"]);
    git(&vault, &["commit", "--quiet", "-m", "initial"]);
    vault
}

#[test]
fn sync_hints_and_doctor_fix_ignores_per_device_files() {
    let temporary = TempDir::new().expect("temporary directory");
    let root = temporary.path();
    let vault = obsidian_vault(root, "vault");
    let path = vault.to_str().unwrap();

    let (_, stderr) = succeeded(&vulcan(root, &["--vault", path, "sync", "run"]));
    assert!(stderr.contains(&format!("hint: {LAYOUT} is per-device and synced")));
    assert!(stderr.contains("vulcan sync doctor --fix"));
    assert!(!vault.join(".gitignore").exists(), "sync must not edit it");
    let (json, _) = succeeded(&vulcan(
        root,
        &["--vault", path, "--output", "json", "sync", "run"],
    ));
    let report: Value = serde_json::from_str(&json).expect("sync JSON");
    assert_eq!(report["device_files"], serde_json::json!([LAYOUT]));
    let (_, stderr) = succeeded(&vulcan(root, &["--vault", path, "sync", "status"]));
    assert!(stderr.contains("hint:"), "{stderr}");

    let (stdout, _) = succeeded(&vulcan(
        root,
        &["--vault", path, "sync", "doctor", "--fix", "--dry-run"],
    ));
    assert!(stdout.contains("would fix\tsync.device-files"));
    assert!(!vault.join(".gitignore").exists());

    let (json, _) = succeeded(&vulcan(
        root,
        &[
            "--vault", path, "--output", "json", "sync", "doctor", "--fix",
        ],
    ));
    let doctor: Value = serde_json::from_str(&json).expect("doctor JSON");
    assert_eq!(doctor["fixes"][0]["code"], "sync.device-files");
    assert_eq!(doctor["fixes"][0]["dry_run"], false);
    assert!(fs::read_to_string(vault.join(".gitignore"))
        .expect("gitignore")
        .contains(&format!("/{LAYOUT}")));

    // Fixed but not yet synced: no more hint, and the doctor only notes it.
    let (_, stderr) = succeeded(&vulcan(root, &["--vault", path, "sync", "status"]));
    assert!(!stderr.contains("hint:"), "{stderr}");
    let (stdout, _) = succeeded(&vulcan(root, &["--vault", path, "sync", "doctor"]));
    assert!(stdout.contains("info\tsync.device-files"), "{stdout}");
    let (_, stderr) = succeeded(&vulcan(root, &["--vault", path, "sync", "run"]));
    assert!(!stderr.contains("hint:"), "{stderr}");
    assert!(vault.join(LAYOUT).is_file(), "the device keeps its copy");
}

#[test]
fn vault_add_ignores_per_device_files_unless_asked_to_keep_them() {
    let temporary = TempDir::new().expect("temporary directory");
    let root = temporary.path();

    let vault = obsidian_vault(root, "notes");
    let (json, _) = succeeded(&vulcan(
        root,
        &[
            "--output",
            "json",
            "vault",
            "add",
            "notes",
            vault.to_str().unwrap(),
            "--no-device-key",
        ],
    ));
    let added: Value = serde_json::from_str(&json).expect("add JSON");
    assert!(added["device_files"]["added"]
        .as_array()
        .expect("added entries")
        .contains(&Value::from(format!("/{LAYOUT}"))));
    assert!(fs::read_to_string(vault.join(".gitignore"))
        .expect("gitignore")
        .contains(&format!("/{LAYOUT}")));

    let kept = obsidian_vault(root, "kept");
    let (stdout, _) = succeeded(&vulcan(
        root,
        &[
            "vault",
            "add",
            "kept",
            kept.to_str().unwrap(),
            "--no-device-key",
            "--keep-device-files",
        ],
    ));
    assert!(!stdout.contains(".gitignore"), "{stdout}");
    assert!(!kept.join(".gitignore").exists());

    // Cloning is setting up too: the fresh checkout gets the entries.
    git(
        &kept,
        &["push", "--quiet", "origin", "HEAD:refs/heads/main"],
    );
    let clone = root.join("clone");
    let remote = root.join("kept.git");
    let (stdout, _) = succeeded(&vulcan(
        root,
        &[
            "sync",
            "clone",
            remote.to_str().unwrap(),
            clone.to_str().unwrap(),
            "--no-device-key",
        ],
    ));
    assert!(stdout.contains("Added"), "{stdout}");
    assert!(fs::read_to_string(clone.join(".gitignore"))
        .expect("clone gitignore")
        .contains(&format!("/{LAYOUT}")));

    // A vault registered without sync is left alone.
    let local = obsidian_vault(root, "local");
    succeeded(&vulcan(
        root,
        &[
            "vault",
            "add",
            "local",
            local.to_str().unwrap(),
            "--no-sync",
        ],
    ));
    assert!(!local.join(".gitignore").exists());
}
