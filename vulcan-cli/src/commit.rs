use serde_json::json;
use vulcan_core::{
    auto_commit, git_status, is_git_repo, load_vault_config, AutoCommitReport, GitConfig,
    GitTrigger, PluginEvent, VaultPaths,
};

#[derive(Debug, Clone)]
pub(crate) enum AutoCommitPolicy {
    Disabled,
    NotGitRepo,
    Enabled(GitConfig),
}

impl AutoCommitPolicy {
    pub(crate) fn for_mutation(paths: &VaultPaths, no_commit: bool) -> Self {
        Self::load(paths, GitTrigger::Mutation, no_commit)
    }

    pub(crate) fn for_scan(paths: &VaultPaths, no_commit: bool) -> Self {
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

    pub(crate) fn warning(&self) -> Option<&'static str> {
        match self {
            Self::NotGitRepo => {
                Some("auto-commit is enabled, but this vault is not a git repository")
            }
            Self::Disabled | Self::Enabled(_) => None,
        }
    }

    pub(crate) fn commit(
        &self,
        paths: &VaultPaths,
        action: &str,
        changed_files: &[String],
        permission_profile: Option<&str>,
        quiet: bool,
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
            quiet,
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
                quiet,
            );
            Ok(Some(report))
        } else {
            Ok(None)
        }
    }
}
