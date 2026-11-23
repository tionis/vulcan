use serde::Deserialize;
use serde_json::json;
use std::path::{Path, PathBuf};
use vulcan_core::Verbosity;
use vulcan_core::{
    auto_commit, git_status, is_git_repo, load_vault_config, vulcan_user_config_dir,
    AutoCommitReport, GitCommitStrategy, GitConfig, GitTrigger, PluginEvent, VaultPaths,
};

/// The device-local daemon registry, named as the daemon names it.
const DAEMON_REGISTRY_FILE: &str = "daemon.toml";

#[derive(Debug, Clone)]
pub enum AutoCommitPolicy {
    Disabled,
    NotGitRepo,
    /// `strategy = "batched"`: the daemon commits accumulated changes, so a
    /// single write does not.
    Batched,
    /// Git sync's hidden-ref snapshots own automatic capture for this vault
    /// on this device, so ordinary per-write commits stand down.
    OwnedBySync {
        wiki_id: String,
    },
    Enabled(GitConfig),
}

/// The component that owns automatic capture of a vault's changes on this
/// device. Exactly one may: Git sync's hidden-ref snapshots when the vault is
/// registered with the Git sync backend and syncs its worktree, otherwise
/// Vulcan's own auto-commit. In committed-history-only mode
/// (`sync.worktree = "none"`) sync moves only commits, so auto-commit is what
/// turns edits into history that sync can publish.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureOwner {
    AutoCommit,
    HiddenRefSync { wiki_id: String },
}

#[derive(Deserialize)]
struct RegistryView {
    #[serde(default, rename = "vault")]
    vaults: Vec<RegisteredVaultView>,
}

#[derive(Deserialize)]
struct RegisteredVaultView {
    id: String,
    path: PathBuf,
    #[serde(default)]
    sync_backend: Option<String>,
}

/// Reports who owns automatic capture for `vault_root`, from the daemon
/// registry of this device. A missing or unreadable registry leaves capture
/// with auto-commit, as it was before the vault was registered.
#[must_use]
pub fn capture_owner(vault_root: &Path) -> CaptureOwner {
    vulcan_user_config_dir().map_or(CaptureOwner::AutoCommit, |directory| {
        capture_owner_in_registry(&directory.join(DAEMON_REGISTRY_FILE), vault_root)
    })
}

/// [`capture_owner`] against the registry at `registry`. A registration
/// without a backend predates `--no-sync` and syncs with Git, as the daemon
/// treats it.
#[must_use]
pub fn capture_owner_in_registry(registry: &Path, vault_root: &Path) -> CaptureOwner {
    let Ok(source) = std::fs::read_to_string(registry) else {
        return CaptureOwner::AutoCommit;
    };
    let Ok(view) = toml::from_str::<RegistryView>(&source) else {
        return CaptureOwner::AutoCommit;
    };
    let Ok(root) = std::fs::canonicalize(vault_root) else {
        return CaptureOwner::AutoCommit;
    };
    view.vaults
        .into_iter()
        .find(|vault| {
            vault
                .sync_backend
                .as_deref()
                .is_none_or(|backend| backend == "git")
                && std::fs::canonicalize(&vault.path).is_ok_and(|path| path == root)
        })
        .filter(|_| sync_captures_worktree(vault_root))
        .map_or(CaptureOwner::AutoCommit, |vault| {
            CaptureOwner::HiddenRefSync { wiki_id: vault.id }
        })
}

/// Whether Git sync snapshots this vault's worktree (`sync.worktree` is
/// `all` or `paths`), so that uncommitted edits reach other devices without
/// commits. In `none` mode only commits sync.
#[must_use]
pub fn sync_captures_worktree(vault_root: &Path) -> bool {
    load_vault_config(&VaultPaths::new(vault_root))
        .config
        .sync
        .worktree
        .syncs_live()
}

/// What became of a write's changes, for reports to agents that cannot run
/// Git themselves.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CommitOutcome {
    pub status: CommitStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha: Option<String>,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CommitStatus {
    /// The write was committed (`sha`).
    Committed,
    /// Batched auto-commit commits it after the vault has been quiet.
    Batched,
    /// Git sync snapshots uncommitted edits and carries them to other devices.
    CapturedBySync,
    /// The change stays uncommitted until someone commits it.
    Uncommitted,
}

/// The checked-out commit, or `None` outside a repository or before the
/// first commit.
#[must_use]
pub fn head_revision(vault_root: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--verify", "--quiet", "HEAD"])
        .current_dir(vault_root)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|head| !head.is_empty())
}

/// Explains what happened to a write made while `HEAD` was `before`.
#[must_use]
pub fn commit_outcome(paths: &VaultPaths, before: Option<&str>, no_commit: bool) -> CommitOutcome {
    let after = head_revision(paths.vault_root());
    if after.is_some() && after.as_deref() != before {
        return CommitOutcome {
            status: CommitStatus::Committed,
            sha: after,
            detail: "the change was committed".to_string(),
        };
    }
    let uncommitted = |detail: String| CommitOutcome {
        status: CommitStatus::Uncommitted,
        sha: None,
        detail,
    };
    if no_commit {
        return uncommitted("`no_commit` was set; the change is not committed".to_string());
    }
    let policy = AutoCommitPolicy::for_mutation(paths, false);
    if !matches!(policy, AutoCommitPolicy::Enabled(_))
        && crate::changesets::is_changeset_worktree(paths.vault_root())
    {
        // Nothing watches a changeset worktree, so batched auto-commit and
        // sync never take its edits; landing does.
        return uncommitted(
            "this is a changeset; `git_commit` or `changeset_land` with a message commits it"
                .to_string(),
        );
    }
    match policy {
        AutoCommitPolicy::Batched => CommitOutcome {
            status: CommitStatus::Batched,
            sha: None,
            detail: "batched auto-commit commits this once the vault has been quiet".to_string(),
        },
        AutoCommitPolicy::OwnedBySync { wiki_id } => CommitOutcome {
            status: CommitStatus::CapturedBySync,
            sha: None,
            detail: format!("Git sync for `{wiki_id}` carries uncommitted edits to other devices"),
        },
        AutoCommitPolicy::NotGitRepo => {
            uncommitted("the vault is not a Git repository, so nothing is committed".to_string())
        }
        AutoCommitPolicy::Enabled(_) => uncommitted("there was nothing to commit".to_string()),
        AutoCommitPolicy::Disabled if is_git_repo(paths.vault_root()) => uncommitted(
            if matches!(capture_owner(paths.vault_root()), CaptureOwner::AutoCommit)
                && registered_git_sync(paths.vault_root())
            {
                "auto-commit is off and this vault syncs only commits, so the change stays on this device until it is committed; enable `[git] auto_commit` with `strategy = \"batched\"`".to_string()
            } else {
                "auto-commit is off, so the change is not committed".to_string()
            },
        ),
        AutoCommitPolicy::Disabled => uncommitted("auto-commit is off".to_string()),
    }
}

/// Whether this device registers the vault with the Git sync backend.
fn registered_git_sync(vault_root: &Path) -> bool {
    let Some(registry) = vulcan_user_config_dir().map(|dir| dir.join(DAEMON_REGISTRY_FILE)) else {
        return false;
    };
    let Ok(source) = std::fs::read_to_string(registry) else {
        return false;
    };
    let Ok(view) = toml::from_str::<RegistryView>(&source) else {
        return false;
    };
    let Ok(root) = std::fs::canonicalize(vault_root) else {
        return false;
    };
    view.vaults.iter().any(|vault| {
        vault
            .sync_backend
            .as_deref()
            .is_none_or(|backend| backend == "git")
            && std::fs::canonicalize(&vault.path).is_ok_and(|path| path == root)
    })
}

impl AutoCommitPolicy {
    #[must_use]
    pub fn for_mutation(paths: &VaultPaths, no_commit: bool) -> Self {
        Self::load(paths, GitTrigger::Mutation, no_commit)
    }

    #[must_use]
    pub fn for_scan(paths: &VaultPaths, no_commit: bool) -> Self {
        Self::load(paths, GitTrigger::Scan, no_commit)
    }

    fn load(paths: &VaultPaths, trigger: GitTrigger, no_commit: bool) -> Self {
        if no_commit {
            return Self::Disabled;
        }
        Self::from_config(paths, &load_vault_config(paths).config.git, trigger)
    }

    /// The policy `config` sets for a write or scan, before `--no-commit`.
    #[must_use]
    pub fn from_config(paths: &VaultPaths, config: &GitConfig, trigger: GitTrigger) -> Self {
        if !config.auto_commit {
            return Self::Disabled;
        }
        if config.strategy == GitCommitStrategy::Batched {
            return Self::Batched;
        }
        if config.trigger != trigger {
            return Self::Disabled;
        }

        if !is_git_repo(paths.vault_root()) {
            return Self::NotGitRepo;
        }
        if let CaptureOwner::HiddenRefSync { wiki_id } = capture_owner(paths.vault_root()) {
            return Self::OwnedBySync { wiki_id };
        }

        Self::Enabled(config.clone())
    }

    #[must_use]
    pub fn is_enabled(&self) -> bool {
        matches!(self, Self::Enabled(_))
    }

    #[must_use]
    pub fn warning(&self) -> Option<String> {
        match self {
            Self::NotGitRepo => {
                Some("auto-commit is enabled, but this vault is not a git repository".to_string())
            }
            Self::OwnedBySync { wiki_id } => Some(format!(
                "auto-commit is enabled, but Git sync for `{wiki_id}` captures this vault's changes; no commit was made"
            )),
            Self::Disabled | Self::Batched | Self::Enabled(_) => None,
        }
    }

    pub fn commit(
        &self,
        paths: &VaultPaths,
        action: &str,
        changed_files: &[String],
        permission_profile: Option<&str>,
        verbosity: Verbosity,
    ) -> Result<Option<AutoCommitReport>, String> {
        note_written_paths(changed_files);
        let Self::Enabled(config) = self else {
            return Ok(None);
        };

        let candidate_files = if changed_files.is_empty() {
            git_status(paths.vault_root())
                .map_err(|error| error.to_string())?
                .changed_paths()
        } else {
            changed_files.to_vec()
        };
        if candidate_files.is_empty() {
            return Ok(None);
        }

        crate::plugins::dispatch_plugin_event(
            paths,
            permission_profile,
            PluginEvent::OnPreCommit,
            &json!({
                "kind": PluginEvent::OnPreCommit,
                "action": action,
                "files": candidate_files,
            }),
            verbosity,
        )
        .map_err(|error| error.to_string())?;

        let report = auto_commit(paths.vault_root(), config, action, &candidate_files)
            .map_err(|error| error.to_string())?;
        if report.committed {
            let _ = crate::plugins::dispatch_plugin_event(
                paths,
                permission_profile,
                PluginEvent::OnPostCommit,
                &json!({
                    "kind": PluginEvent::OnPostCommit,
                    "action": action,
                    "files": report.files,
                    "sha": report.sha,
                    "message": report.message,
                }),
                verbosity,
            );
            Ok(Some(report))
        } else {
            Ok(None)
        }
    }
}

thread_local! {
    static WRITTEN_PATHS: std::cell::RefCell<Option<std::collections::BTreeSet<String>>> =
        const { std::cell::RefCell::new(None) };
}

/// Every mutation hands its changed files to [`AutoCommitPolicy::commit`],
/// whatever the policy, so that is where a caller learns what it wrote.
fn note_written_paths(changed_files: &[String]) {
    WRITTEN_PATHS.with(|recorder| {
        if let Some(paths) = recorder.borrow_mut().as_mut() {
            paths.extend(changed_files.iter().cloned());
        }
    });
}

/// Run `operation` and return the vault-relative paths its mutations wrote,
/// as they reported them for committing. Nested calls each see their own
/// writes and pass them on to the enclosing recorder.
pub fn record_written_paths<T>(
    operation: impl FnOnce() -> T,
) -> (T, std::collections::BTreeSet<String>) {
    let outer =
        WRITTEN_PATHS.with(|recorder| recorder.replace(Some(std::collections::BTreeSet::new())));
    let result = operation();
    let written = WRITTEN_PATHS
        .with(|recorder| recorder.replace(outer))
        .unwrap_or_default();
    note_written_paths(&written.iter().cloned().collect::<Vec<_>>());
    (result, written)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn policy_is_disabled_by_default_and_no_commit_overrides_configuration() {
        let temporary = tempdir().expect("temporary vault");
        let paths = VaultPaths::new(temporary.path());
        assert!(matches!(
            AutoCommitPolicy::for_mutation(&paths, false),
            AutoCommitPolicy::Disabled
        ));

        fs::create_dir_all(paths.vulcan_dir()).expect("configuration directory");
        fs::write(paths.config_file(), "[git]\nauto_commit = true\n").expect("git configuration");
        assert!(matches!(
            AutoCommitPolicy::for_mutation(&paths, true),
            AutoCommitPolicy::Disabled
        ));
        assert!(matches!(
            AutoCommitPolicy::for_mutation(&paths, false),
            AutoCommitPolicy::NotGitRepo
        ));
    }

    #[test]
    fn git_sync_registrations_own_capture_and_no_sync_registrations_do_not() {
        let temporary = tempdir().expect("temporary directory");
        let synced = temporary.path().join("synced");
        let legacy = temporary.path().join("legacy");
        let plain = temporary.path().join("plain");
        let unregistered = temporary.path().join("unregistered");
        for vault in [&synced, &legacy, &plain, &unregistered] {
            fs::create_dir_all(vault).expect("vault directory");
        }
        let registry = temporary.path().join("daemon.toml");
        fs::write(
            &registry,
            format!(
                "bind = \"127.0.0.1:0\"\n\n[[vault]]\nid = \"synced\"\npath = {synced:?}\nsync_backend = \"git\"\nfuture_field = 1\n\n[[vault]]\nid = \"legacy\"\npath = {legacy:?}\n\n[[vault]]\nid = \"plain\"\npath = {plain:?}\nsync_backend = \"none\"\n",
            ),
        )
        .expect("registry");

        assert_eq!(
            capture_owner_in_registry(&registry, &synced.join(".")),
            CaptureOwner::HiddenRefSync {
                wiki_id: "synced".to_string()
            }
        );
        assert_eq!(
            capture_owner_in_registry(&registry, &legacy),
            CaptureOwner::HiddenRefSync {
                wiki_id: "legacy".to_string()
            }
        );
        assert_eq!(
            capture_owner_in_registry(&registry, &plain),
            CaptureOwner::AutoCommit
        );
        assert_eq!(
            capture_owner_in_registry(&registry, &unregistered),
            CaptureOwner::AutoCommit
        );
        // In committed-history-only mode sync moves only commits, so
        // auto-commit owns capture even for a Git registration.
        fs::create_dir_all(synced.join(".vulcan")).expect("config directory");
        fs::write(
            synced.join(".vulcan/config.toml"),
            "[sync]\nworktree = \"none\"\n",
        )
        .expect("committed-only config");
        assert_eq!(
            capture_owner_in_registry(&registry, &synced),
            CaptureOwner::AutoCommit
        );
        fs::write(&registry, "not = [valid").expect("broken registry");
        assert_eq!(
            capture_owner_in_registry(&registry, &synced),
            CaptureOwner::AutoCommit
        );
    }

    #[test]
    fn batched_strategy_defers_per_write_commits_to_the_daemon() {
        let temporary = tempdir().expect("temporary vault");
        let paths = VaultPaths::new(temporary.path());
        fs::create_dir_all(paths.vulcan_dir()).expect("configuration directory");
        fs::write(
            paths.config_file(),
            "[git]\nauto_commit = true\nstrategy = \"batched\"\n",
        )
        .expect("git configuration");
        let policy = AutoCommitPolicy::for_mutation(&paths, false);
        assert!(matches!(policy, AutoCommitPolicy::Batched));
        assert!(policy.warning().is_none());
        assert!(!policy.is_enabled());
        assert!(matches!(
            AutoCommitPolicy::for_scan(&paths, false),
            AutoCommitPolicy::Batched
        ));
        let owned = AutoCommitPolicy::OwnedBySync {
            wiki_id: "notes".to_string(),
        };
        assert!(owned
            .warning()
            .is_some_and(|warning| warning.contains("`notes`")));
    }

    #[test]
    fn scan_trigger_does_not_enable_mutation_commits() {
        let temporary = tempdir().expect("temporary vault");
        let paths = VaultPaths::new(temporary.path());
        fs::create_dir_all(paths.vulcan_dir()).expect("configuration directory");
        fs::write(
            paths.config_file(),
            "[git]\nauto_commit = true\ntrigger = \"scan\"\n",
        )
        .expect("git configuration");
        assert!(matches!(
            AutoCommitPolicy::for_mutation(&paths, false),
            AutoCommitPolicy::Disabled
        ));
        assert!(matches!(
            AutoCommitPolicy::for_scan(&paths, false),
            AutoCommitPolicy::NotGitRepo
        ));
    }
}
