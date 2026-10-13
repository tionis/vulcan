//! Drives a real Helix through tmux against the built binary, with the
//! configuration `vulcan editor helix print-config` prints.
//!
//! Needs `hx` (or `VULCAN_HELIX`, a path to it) and `tmux` on `PATH`; set
//! `HELIX_RUNTIME` if Helix cannot find its runtime. Skipped without them,
//! unless `VULCAN_REQUIRE_HELIX` is set, which turns that into a failure.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

fn helix() -> Option<PathBuf> {
    let helix = std::env::var_os("VULCAN_HELIX").map_or_else(|| PathBuf::from("hx"), PathBuf::from);
    let version = Command::new(&helix).arg("--version").output().ok()?;
    let tmux = Command::new("tmux").arg("-V").output().ok()?;
    (version.status.success() && tmux.status.success()).then_some(helix)
}

/// A Helix session in a private tmux server, whose socket lives in the
/// test's temporary directory.
struct Session {
    socket: PathBuf,
}

impl Session {
    fn tmux(&self) -> Command {
        let mut command = Command::new("tmux");
        command.arg("-S").arg(&self.socket);
        command
    }

    fn keys(&self, keys: &[&str]) {
        let status = self
            .tmux()
            .args(["send-keys", "-t", "hx"])
            .args(keys)
            .status()
            .unwrap();
        assert!(status.success());
        std::thread::sleep(Duration::from_millis(250));
    }

    /// Type a typable command and run it.
    fn command(&self, command: &str) {
        self.keys(&[command, "Enter"]);
    }

    fn pane(&self) -> String {
        let output = self
            .tmux()
            .args(["capture-pane", "-p", "-t", "hx"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn wait(&self, what: &str, mut condition: impl FnMut(&str) -> bool) {
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(15) {
            if condition(&self.pane()) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!(
            "timed out waiting until {what}; the screen:\n{}",
            self.pane()
        );
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.tmux().arg("kill-server").status();
    }
}

fn write(path: &Path, contents: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

/// A vault, a user's Helix configuration with Vulcan's merged in by
/// `vulcan editor helix install` (and `vulcan` on `PATH`), and Helix
/// started on `Home.md` once the server is attached.
fn start_helix(helix: &Path, temp: &Path) -> (PathBuf, Session) {
    let vault = temp.join("vault");
    let binary = env!("CARGO_BIN_EXE_vulcan");
    write(
        &vault.join("Home.md"),
        "# Home\n\n- [ ] Water plants\n\nSee [[Todo]] and [[Missing]].\n",
    );
    write(&vault.join("Todo.md"), "- [ ] Open item\n");
    write(
        &vault.join(".vulcan/config.toml"),
        "[periodic.daily]\nfolder = \"Journal\"\nformat = \"YYYY-MM-DD\"\n",
    );
    for args in [
        &["index", "init", "--no-import"][..],
        &["index", "scan"][..],
    ] {
        let status = Command::new(binary)
            .arg("--vault")
            .arg(&vault)
            .args(args)
            .output()
            .unwrap()
            .status;
        assert!(status.success());
    }

    // The user's own configuration, which the install merges into.
    let config_dir = temp.join("config/helix");
    write(
        &config_dir.join("languages.toml"),
        "[[language]]\nname = \"markdown\"\nsoft-wrap.enable = true\n",
    );
    write(
        &config_dir.join("config.toml"),
        "[editor]\nline-number = \"relative\"\n\n[keys.normal.space.v]\nz = \":echo mine\"\n",
    );
    let installed = Command::new(binary)
        .args(["--output", "json", "editor", "helix", "install", "--dir"])
        .arg(&config_dir)
        .output()
        .unwrap();
    assert!(
        installed.status.success(),
        "{}",
        String::from_utf8_lossy(&installed.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&installed.stdout).unwrap();
    assert_eq!(report["backups"].as_array().unwrap().len(), 2, "{report}");
    let config = std::fs::read_to_string(config_dir.join("config.toml")).unwrap();
    assert!(config.contains(r#"z = ":echo mine""#) && config.contains("vulcan.task.toggle"));
    let bin = temp.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::os::unix::fs::symlink(binary, bin.join("vulcan")).unwrap();

    let session = Session {
        socket: temp.join("tmux.sock"),
    };
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut start = session.tmux();
    start
        .args([
            "new-session",
            "-d",
            "-s",
            "hx",
            "-x",
            "160",
            "-y",
            "45",
            "-c",
        ])
        .arg(&vault)
        .args(["env", &format!("PATH={path}")])
        .arg(format!("HOME={}", temp.display()))
        .arg(format!("XDG_CONFIG_HOME={}", temp.join("config").display()));
    if let Some(runtime) = std::env::var_os("HELIX_RUNTIME") {
        start.arg(format!(
            "HELIX_RUNTIME={}",
            PathBuf::from(runtime).display()
        ));
    }
    let status = start.arg(helix).arg("Home.md").status().unwrap();
    assert!(status.success(), "tmux could not start Helix");
    // The unresolved [[Missing]] shows once the server is attached.
    session.wait("the server reports a diagnostic", |screen| {
        screen.contains("● 1")
    });

    (vault, session)
}

#[test]
fn helix_runs_vulcan_through_its_configuration() {
    let Some(helix) = helix() else {
        assert!(
            std::env::var_os("VULCAN_REQUIRE_HELIX").is_none(),
            "VULCAN_REQUIRE_HELIX is set but Helix (hx or VULCAN_HELIX) or tmux is missing"
        );
        eprintln!("skipping: needs Helix and tmux");
        return;
    };
    let temp = tempfile::tempdir().unwrap();
    let (vault, session) = start_helix(&helix, temp.path());

    // A keybinding runs a command with the buffer and cursor filled in.
    session.command(":goto 3");
    session.keys(&["Space", "v", "x"]);
    session.wait("the task is completed", |screen| {
        screen.contains("- [x] Water plants")
    });
    session.command(":write");
    session.wait("the note is saved", |_| {
        std::fs::read_to_string(vault.join("Home.md"))
            .unwrap()
            .contains("- [x]")
    });

    // The daily note is created and opened (window/showDocument).
    session.keys(&["Space", "v", "d"]);
    session.wait("the daily note is open", |screen| {
        screen.contains("Journal/")
    });
    assert_eq!(std::fs::read_dir(vault.join("Journal")).unwrap().count(), 1);

    // Renaming a link moves the note and rewrites the open buffer.
    session.command(":open Home.md");
    session.command(":goto 5");
    session.keys(&["g", "h", "w", "w", "w"]);
    session.keys(&["Space", "r"]);
    session.wait("the rename prompt", |screen| screen.contains("rename-to:"));
    session.keys(&["C-w", "Chores", "Enter"]);
    session.wait("the link is rewritten", |screen| {
        screen.contains("[[Chores]]")
    });
    assert!(vault.join("Chores.md").exists() && !vault.join("Todo.md").exists());
    session.command(":write");

    // Moving the open note: Helix renames it, without stalling on the
    // rename it announces back to the server.
    let started = Instant::now();
    session.command(
        r#":lsp-workspace-command vulcan.note.move {"from": "%{buffer_name}", "to": "archive/Home.md"}"#,
    );
    session.wait("the buffer follows the note", |screen| {
        screen.contains("archive/Home.md")
    });
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "{:?}",
        started.elapsed()
    );
    assert!(vault.join("archive/Home.md").exists() && !vault.join("Home.md").exists());

    // A code action reopens the task.
    session.command(":goto 3");
    session.keys(&["Space", "a"]);
    session.wait("the code action menu", |screen| {
        screen.contains("Reopen task")
    });
    session.keys(&["Enter"]);
    session.wait("the task is reopened", |screen| {
        screen.contains("- [ ] Water plants")
    });

    // The selection goes to the inbox through stdin.
    session.command(":goto 5");
    session.keys(&["x", "Space", "v", "c"]);
    session.wait("the capture lands in the inbox", |_| {
        std::fs::read_to_string(vault.join("Inbox.md"))
            .is_ok_and(|inbox| inbox.contains("See [[Chores]]"))
    });

    // Go to definition follows a link.
    session.keys(&["g", "h", "w", "w", "w", "g", "d"]);
    session.wait("the linked note is open", |screen| {
        screen.contains("Chores.md")
    });
}
