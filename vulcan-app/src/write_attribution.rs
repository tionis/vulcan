//! Who wrote what, for batched auto-commits.
//!
//! A batched commit lands long after the write, by which time the writer is
//! gone. MCP sessions therefore record, per changed path, the commit trailers
//! that name them (`Vulcan-MCP-*`); the batch pass appends those trailers to
//! the commit that takes the path and then forgets it. The ledger is device
//! operational state beside the vault's locks, not part of the rebuildable
//! cache, and is bounded and self-pruning: entries for paths that are no
//! longer changed, or that a later commit already took, are dropped.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use vulcan_core::durable::{self, Durability};
use vulcan_core::VaultPaths;

use crate::commit::AutoCommitPolicy;

const LEDGER_FILE: &str = "write-attribution.json";
const LOCK_FILE: &str = "write-attribution.lock";
const MAX_LEDGER_BYTES: u64 = 4 * 1024 * 1024;
const MAX_PATHS: usize = 10_000;
const MAX_WRITERS_PER_PATH: usize = 8;

/// Commit trailers naming one writer, as `(key, value)` pairs.
pub type Trailers = Vec<(String, String)>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Writer {
    trailers: Trailers,
    /// Unix seconds of the writer's latest write to the path.
    at: i64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Ledger {
    paths: BTreeMap<String, Vec<Writer>>,
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX)
        })
}

fn state_dir(paths: &VaultPaths) -> Option<PathBuf> {
    paths
        .operational_state_dir()
        .ok()
        .filter(|dir| dir.is_dir())
}

/// Run `update` on the ledger under its lock and save the result.
fn with_ledger<T>(dir: &Path, update: impl FnOnce(&mut Ledger) -> T) -> std::io::Result<T> {
    let lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(dir.join(LOCK_FILE))?;
    lock.lock_exclusive()?;
    let file = dir.join(LEDGER_FILE);
    let mut ledger: Ledger = durable::read_json(&file, MAX_LEDGER_BYTES)
        .ok()
        .flatten()
        .unwrap_or_default();
    let result = update(&mut ledger);
    if ledger.paths.is_empty() {
        durable::remove(&file, Durability::Full)?;
    } else {
        durable::replace_json(&file, &ledger, Durability::Full)?;
    }
    Ok(result)
}

/// Record that the writer named by `trailers` changed `written`, when the
/// vault commits by batches. Best effort: attribution never fails a write.
pub fn record_batched_writes(paths: &VaultPaths, written: &BTreeSet<String>, trailers: &Trailers) {
    if written.is_empty() || trailers.is_empty() {
        return;
    }
    if !matches!(
        AutoCommitPolicy::for_mutation(paths, false),
        AutoCommitPolicy::Batched
    ) {
        return;
    }
    let Some(dir) = state_dir(paths) else {
        return;
    };
    let at = now();
    let _ = with_ledger(&dir, |ledger| {
        for path in written {
            if !ledger.paths.contains_key(path) && ledger.paths.len() >= MAX_PATHS {
                continue;
            }
            let writers = ledger.paths.entry(path.clone()).or_default();
            if let Some(writer) = writers.iter_mut().find(|w| &w.trailers == trailers) {
                writer.at = at;
            } else if writers.len() < MAX_WRITERS_PER_PATH {
                writers.push(Writer {
                    trailers: trailers.clone(),
                    at,
                });
            }
        }
    });
}

/// Unix seconds of the last commit that touched `path`, if any.
fn last_commit_time(vault_root: &Path, path: &str) -> Option<i64> {
    let output = Command::new("git")
        .arg("-C")
        .arg(vault_root)
        .args(["log", "-1", "--format=%ct", "--", path])
        .output()
        .ok()?;
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}

/// The writers of each of `candidates`, after dropping entries for paths that
/// are no longer changed and entries a later commit already took.
#[must_use]
pub fn writers_for(paths: &VaultPaths, candidates: &[String]) -> BTreeMap<String, Vec<Trailers>> {
    let Some(dir) = state_dir(paths) else {
        return BTreeMap::new();
    };
    let candidates = candidates.iter().collect::<BTreeSet<_>>();
    with_ledger(&dir, |ledger| {
        ledger.paths.retain(|path, writers| {
            if !candidates.contains(path) {
                return false;
            }
            if let Some(committed) = last_commit_time(paths.vault_root(), path) {
                // A commit at or after the write took it (the same second
                // counts: a later edit by someone else stays unattributed
                // rather than misattributed).
                writers.retain(|writer| writer.at > committed);
            }
            !writers.is_empty()
        });
        ledger
            .paths
            .iter()
            .map(|(path, writers)| {
                (
                    path.clone(),
                    writers
                        .iter()
                        .map(|writer| writer.trailers.clone())
                        .collect(),
                )
            })
            .collect()
    })
    .unwrap_or_default()
}

/// Forget the writers of paths a commit has taken.
pub fn forget(paths: &VaultPaths, committed: &[String]) {
    let Some(dir) = state_dir(paths) else {
        return;
    };
    let _ = with_ledger(&dir, |ledger| {
        for path in committed {
            ledger.paths.remove(path);
        }
    });
}

/// `groups` with each commit message naming the writers of its paths.
#[must_use]
pub fn attribute_groups(
    paths: &VaultPaths,
    candidates: &[String],
    groups: &[vulcan_core::BatchCommitGroup],
) -> Vec<vulcan_core::BatchCommitGroup> {
    let writers = writers_for(paths, candidates);
    groups
        .iter()
        .map(|group| vulcan_core::BatchCommitGroup {
            message: vulcan_core::git::message_with_trailers(
                &group.message,
                &trailers_for_group(&writers, &group.paths),
            ),
            paths: group.paths.clone(),
        })
        .collect()
}

/// The trailers for a commit of `group`: every distinct writer of its paths,
/// in first-seen order.
#[must_use]
pub fn trailers_for_group(
    writers: &BTreeMap<String, Vec<Trailers>>,
    group: &[String],
) -> Vec<(String, String)> {
    let mut seen = BTreeSet::new();
    let mut trailers = Vec::new();
    for path in group {
        for writer in writers.get(path).into_iter().flatten() {
            if seen.insert(writer.clone()) {
                trailers.extend(writer.iter().cloned());
            }
        }
    }
    trailers
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn git(root: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
            .args(args)
            .current_dir(root)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    fn trailers(client: &str) -> Trailers {
        vec![
            ("Vulcan-MCP-Client".to_string(), client.to_string()),
            ("Vulcan-MCP-Profile".to_string(), "agent".to_string()),
        ]
    }

    #[test]
    fn the_ledger_attributes_batched_paths_and_prunes_itself() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        git(root, &["init", "--quiet"]);
        let paths = VaultPaths::new(root);
        vulcan_core::paths::initialize_vulcan_dir(&paths).unwrap();
        let written = BTreeSet::from(["A.md".to_string(), "B.md".to_string()]);

        // Without batched commits nothing is recorded.
        record_batched_writes(&paths, &written, &trailers("one"));
        assert!(!paths.vulcan_dir().join(LEDGER_FILE).exists());

        fs::write(
            paths.config_file(),
            "[git]\nauto_commit = true\nstrategy = \"batched\"\n",
        )
        .unwrap();
        record_batched_writes(&paths, &written, &trailers("one"));
        record_batched_writes(&paths, &written, &trailers("one"));
        record_batched_writes(
            &paths,
            &BTreeSet::from(["B.md".to_string()]),
            &trailers("two"),
        );

        let candidates = vec!["A.md".to_string(), "B.md".to_string()];
        let writers = writers_for(&paths, &candidates);
        assert_eq!(writers["A.md"], vec![trailers("one")]);
        assert_eq!(writers["B.md"], vec![trailers("one"), trailers("two")]);
        let group = trailers_for_group(&writers, &candidates);
        assert_eq!(group.len(), 4, "{group:?}");
        assert_eq!(group[0].1, "one");
        assert_eq!(group[2].1, "two");

        // A path no longer changed is dropped; forgetting clears the rest.
        let writers = writers_for(&paths, &["B.md".to_string()]);
        assert!(!writers.contains_key("A.md"));
        forget(&paths, &["B.md".to_string()]);
        assert!(writers_for(&paths, &["B.md".to_string()]).is_empty());
        assert!(!paths.vulcan_dir().join(LEDGER_FILE).exists());

        // A commit made after the write already took it.
        record_batched_writes(
            &paths,
            &BTreeSet::from(["C.md".to_string()]),
            &trailers("one"),
        );
        fs::write(root.join("C.md"), "c\n").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        git(root, &["add", "C.md"]);
        git(root, &["commit", "--quiet", "-m", "by hand"]);
        assert!(writers_for(&paths, &["C.md".to_string()]).is_empty());
    }
}
