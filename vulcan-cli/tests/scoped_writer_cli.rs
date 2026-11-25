//! A profile scoped to some folders can write there without being granted
//! reads of the mdbase control files: writing profiles read them implicitly,
//! unless a path deny names them.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use vulcan_core::VaultPaths;

fn write(path: &Path, contents: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_vulcan"))
}

fn vault(root: &Path, profiles: &str) {
    vulcan_core::initialize_vulcan_dir(&VaultPaths::new(root)).unwrap();
    // `--permissions` selects this device's own profile.
    write(&root.join(".vulcan/config.local.toml"), profiles);
}

fn run(root: &Path, profile: &str, args: &[&str], stdin: &str) -> Output {
    let mut child = Command::new(binary())
        .arg("--vault")
        .arg(root)
        .args(["--permissions", profile])
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn a_folder_scoped_writer_writes_in_a_vault_without_mdbase() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    vault(
        root,
        concat!(
            "[permissions.profiles.worlds]\n",
            "read = { allow = [\"folder:Worlds/**\"], deny = [\"tag:rating/explicit\"] }\n",
            "write = { allow = [\"folder:Worlds/**\"], deny = [\"tag:rating/explicit\"] }\n",
        ),
    );
    write(&root.join("Worlds/a.md"), "a\n");
    let scanned = Command::new(binary())
        .arg("--vault")
        .arg(root)
        .args(["index", "scan"])
        .output()
        .unwrap();
    assert!(scanned.status.success());

    let created = run(root, "worlds", &["note", "create", "Worlds/b.md"], "body\n");
    assert!(created.status.success(), "{}", stderr(&created));
    let appended = run(
        root,
        "worlds",
        &["note", "append", "Worlds/a.md", "more"],
        "",
    );
    assert!(appended.status.success(), "{}", stderr(&appended));
    assert!(std::fs::read_to_string(root.join("Worlds/a.md"))
        .unwrap()
        .contains("more"));
    // The exemption covers controls only.
    let outside = run(root, "worlds", &["note", "create", "Other.md"], "body\n");
    assert!(!outside.status.success());
}

#[test]
fn a_folder_scoped_writer_is_validated_against_controls_it_cannot_list() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    vault(
        root,
        concat!(
            "[permissions.profiles.tasks]\n",
            "read = { allow = [\"folder:tasks/**\"] }\n",
            "write = { allow = [\"folder:tasks/**\"] }\n",
            "[permissions.profiles.no-types]\n",
            "read = { allow = [\"folder:tasks/**\"], deny = [\"folder:_types/**\"] }\n",
            "write = { allow = [\"folder:tasks/**\"] }\n",
        ),
    );
    write(&root.join("mdbase.yaml"), "spec_version: \"0.3.0\"\n");
    write(
        &root.join("_types/task.md"),
        "---\nkind: mdbase.type\nname: task\nversion: 1\nschema:\n  dialect: json-schema-2020-12\n  value:\n    type: object\n    required: [title]\n---\n",
    );

    let valid = run(
        root,
        "tasks",
        &["note", "create", "tasks/ok.md"],
        "---\ntype: task\ntitle: Ok\n---\n",
    );
    assert!(valid.status.success(), "{}", stderr(&valid));
    let invalid = run(
        root,
        "tasks",
        &["note", "create", "tasks/untitled.md"],
        "---\ntype: task\n---\n",
    );
    assert!(!invalid.status.success());
    assert!(
        !stderr(&invalid).contains("permission denied"),
        "{}",
        stderr(&invalid)
    );
    assert!(!root.join("tasks/untitled.md").exists());
    // Reading the controls for validation does not make them readable.
    let listed = run(root, "tasks", &["note", "get", "_types/task.md"], "");
    assert!(!listed.status.success());

    // An explicit path deny still hides the controls, so the write is refused.
    let denied = run(
        root,
        "no-types",
        &["note", "create", "tasks/other.md"],
        "---\ntype: task\ntitle: Other\n---\n",
    );
    assert!(!denied.status.success());
    assert!(
        stderr(&denied).contains("permission denied for required mdbase controls"),
        "{}",
        stderr(&denied)
    );
}
