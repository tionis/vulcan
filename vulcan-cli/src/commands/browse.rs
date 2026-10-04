use crate::{browse_tui, refresh_mode_for_target, Cli, CliError, OutputFormat, RefreshTarget};
use std::io::{self, IsTerminal};
use vulcan_core::{PermissionGuard, ProfilePermissionGuard, VaultPaths};

pub(crate) fn handle_browse_command(
    cli: &Cli,
    paths: &VaultPaths,
    stdout_is_tty: bool,
    no_commit: bool,
) -> Result<(), CliError> {
    let guard = crate::selected_permission_guard(cli, paths)?;
    ensure_browse_authority(&guard)?;
    if cli.output != OutputFormat::Human || !stdout_is_tty || !io::stdin().is_terminal() {
        return Err(CliError::operation(
            "browse requires an interactive terminal with `--output human`",
        ));
    }
    let refresh_mode = refresh_mode_for_target(paths, cli, RefreshTarget::Browse);
    browse_tui::run_browse_tui(paths, refresh_mode, no_commit).map_err(CliError::operation)
}

/// The browse TUI reads, searches, moves, edits, and commits across the whole
/// vault without per-surface authorization. Until those surfaces carry the
/// caller's guard, refuse any profile that a whole-vault session would exceed.
pub(crate) fn ensure_browse_authority(guard: &ProfilePermissionGuard) -> Result<(), CliError> {
    let grant = guard.grant();
    let unrestricted = grant.read.is_unrestricted()
        && grant.write.is_unrestricted()
        && grant.refactor.is_unrestricted()
        && grant.git
        && grant.execute
        && !guard.has_policy_hook();
    if unrestricted {
        Ok(())
    } else {
        Err(CliError::operation(format!(
            "browse does not yet enforce restricted permission profiles; profile `{}` is restricted. \
             Use scoped commands such as `note get`, `search`, `bases eval`, or `tasks list` instead",
            guard.profile_name()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::ensure_browse_authority;
    use std::fs;
    use tempfile::tempdir;
    use vulcan_core::{resolve_permission_profile, ProfilePermissionGuard, VaultPaths};

    #[test]
    fn browse_refuses_restricted_profiles() {
        let temp = tempdir().unwrap();
        let paths = VaultPaths::new(temp.path());
        fs::create_dir_all(temp.path().join(".vulcan")).unwrap();
        fs::write(
            temp.path().join(".vulcan/config.toml"),
            "[permissions.profiles.scoped]\nread = { allow = [\"folder:Public/**\"] }\n",
        )
        .unwrap();
        let guard = |profile: Option<&str>| {
            ProfilePermissionGuard::new(
                &paths,
                resolve_permission_profile(&paths, profile).unwrap(),
            )
        };
        assert!(ensure_browse_authority(&guard(Some("unrestricted"))).is_ok());
        let error = ensure_browse_authority(&guard(Some("scoped"))).unwrap_err();
        assert!(error.to_string().contains("restricted"), "{error}");
        assert!(ensure_browse_authority(&guard(Some("readonly"))).is_err());
    }
}
