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
/// registered with the Git sync backend, otherwise Vulcan's own auto-commit.
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
        .map_or(CaptureOwner::AutoCommit, |vault| {
            CaptureOwner::HiddenRefSync { wiki_id: vault.id }
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
