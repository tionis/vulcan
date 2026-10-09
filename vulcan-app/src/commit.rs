use serde_json::json;
use vulcan_core::Verbosity;
use vulcan_core::{
    auto_commit, git_status, is_git_repo, load_vault_config, AutoCommitReport, GitConfig,
    GitTrigger, PluginEvent, VaultPaths,
};

#[derive(Debug, Clone)]
pub enum AutoCommitPolicy {
    Disabled,
    NotGitRepo,
    Enabled(GitConfig),
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

        let config = load_vault_config(paths).config.git;
        if !config.auto_commit || config.trigger != trigger {
            return Self::Disabled;
        }

        if !is_git_repo(paths.vault_root()) {
            return Self::NotGitRepo;
        }

        Self::Enabled(config)
    }

    #[must_use]
    pub fn warning(&self) -> Option<&'static str> {
        match self {
            Self::NotGitRepo => {
                Some("auto-commit is enabled, but this vault is not a git repository")
            }
            Self::Disabled | Self::Enabled(_) => None,
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
