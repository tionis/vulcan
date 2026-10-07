use crate::{browse_tui, refresh_mode_for_target, Cli, CliError, OutputFormat, RefreshTarget};
use std::io::{self, IsTerminal};
use vulcan_core::{PermissionFilter, PermissionGuard, ProfilePermissionGuard, VaultPaths};

pub(crate) fn handle_browse_command(
    cli: &Cli,
    paths: &VaultPaths,
    stdout_is_tty: bool,
    no_commit: bool,
) -> Result<(), CliError> {
    let guard = crate::selected_permission_guard(cli, paths)?;
    let authority = BrowseAuthority::new(guard, cli.permissions.clone())?;
    if cli.output != OutputFormat::Human || !stdout_is_tty || !io::stdin().is_terminal() {
        return Err(CliError::operation(
            "browse requires an interactive terminal with `--output human`",
        ));
    }
    let refresh_mode = refresh_mode_for_target(paths, cli, RefreshTarget::Browse);
    browse_tui::run_browse_tui(paths, refresh_mode, no_commit, authority)
        .map_err(CliError::operation)
}

/// What the selected profile lets one browse session see and change. Every
/// browse surface consults it: lists, search, tags, links, doctor rows,
/// calendar events, and previews show only readable notes; editing needs
/// read, write, and execute authority; creating needs write and execute;
/// moving needs refactor authority at both paths; Git history and
/// auto-commit need Git authority; background refreshes need index
/// authority. Profiles with policy hooks are refused, because their dynamic
/// decisions cannot be proved for every surface.
#[derive(Debug, Clone)]
pub(crate) struct BrowseAuthority {
    guard: ProfilePermissionGuard,
    profile: Option<String>,
    filter: PermissionFilter,
}

impl BrowseAuthority {
    pub(crate) fn new(
        guard: ProfilePermissionGuard,
        profile: Option<String>,
    ) -> Result<Self, CliError> {
        if guard.has_policy_hook() {
            return Err(CliError::operation(format!(
                "browse cannot apply the policy hook of profile `{}`; use scoped commands such as `note get`, `search`, `bases eval`, or `tasks list` instead",
                guard.profile_name()
            )));
        }
        let filter = guard.read_filter();
        Ok(Self {
            guard,
            profile,
            filter,
        })
    }

    /// The default profile's authority, for tests.
    #[cfg(test)]
    pub(crate) fn default_for(paths: &VaultPaths) -> Self {
        let guard = ProfilePermissionGuard::new(
            paths,
            vulcan_core::resolve_permission_profile(paths, None).expect("default profile"),
        );
        Self::new(guard, None).expect("default profile has no policy hook")
    }

    pub(crate) fn guard(&self) -> &ProfilePermissionGuard {
        &self.guard
    }

    pub(crate) fn profile(&self) -> Option<&str> {
        self.profile.as_deref()
    }

    /// The read filter for indexed queries; unrestricted when it allows all.
    pub(crate) fn read_filter(&self) -> Option<&PermissionFilter> {
        (!self.filter.path_permission().is_unrestricted()).then_some(&self.filter)
    }

    pub(crate) fn can_read(&self, path: &str) -> bool {
        self.guard.check_read_path(path).is_ok()
    }

    pub(crate) fn check_edit(&self, path: &str) -> Result<(), String> {
        self.guard
            .check_read_path(path)
            .and_then(|()| self.guard.check_write_path(path))
            .and_then(|()| self.guard.check_execute())
            .map_err(|error| error.to_string())
    }

    pub(crate) fn check_create(&self, path: &str) -> Result<(), String> {
        self.guard
            .check_write_path(path)
            .and_then(|()| self.guard.check_execute())
            .map_err(|error| error.to_string())
    }

    pub(crate) fn check_move(&self, source: &str, destination: &str) -> Result<(), String> {
        self.guard
            .check_refactor_path(source)
            .and_then(|()| self.guard.check_refactor_path(destination))
            .map_err(|error| error.to_string())
    }

    pub(crate) fn check_git(&self) -> Result<(), String> {
        self.guard.check_git().map_err(|error| error.to_string())
    }

    pub(crate) fn can_index(&self) -> bool {
        self.guard.check_index().is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::BrowseAuthority;
    use std::fs;
    use tempfile::tempdir;
    use vulcan_core::{resolve_permission_profile, ProfilePermissionGuard, VaultPaths};

    #[test]
    fn browse_scopes_restricted_profiles_and_refuses_policy_hooks() {
        let temp = tempdir().unwrap();
        let paths = VaultPaths::new(temp.path());
        fs::create_dir_all(temp.path().join(".vulcan")).unwrap();
        fs::write(
            temp.path().join(".vulcan/config.toml"),
            "[permissions.profiles.scoped]\nread = { allow = [\"folder:Public/**\"] }\nwrite = { allow = [\"folder:Public/**\"] }\n\n[permissions.profiles.hooked]\nread = \"all\"\npolicy_hook = \"hook.js\"\n",
        )
        .unwrap();
        fs::write(temp.path().join("hook.js"), "export default () => true;\n").unwrap();
        let authority = |profile: &str| {
            let selection = resolve_permission_profile(&paths, Some(profile)).unwrap();
            BrowseAuthority::new(
                ProfilePermissionGuard::new(&paths, selection),
                Some(profile.to_string()),
            )
        };
        let scoped = authority("scoped").unwrap();
        assert!(scoped.can_read("Public/a.md"));
        assert!(!scoped.can_read("Private/a.md"));
        assert!(scoped.read_filter().is_some());
        assert!(
            scoped.check_create("Public/b.md").is_err(),
            "no execute grant"
        );
        assert!(scoped.check_move("Public/a.md", "Public/b.md").is_err());
        assert!(scoped.check_git().is_err());
        let unrestricted = authority("unrestricted").unwrap();
        assert!(unrestricted.read_filter().is_none());
        assert!(unrestricted.check_edit("Private/a.md").is_ok());
        let error = authority("hooked").unwrap_err();
        assert!(error.to_string().contains("policy hook"), "{error}");
    }
}
