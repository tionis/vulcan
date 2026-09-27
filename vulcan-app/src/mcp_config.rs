//! Permission-aware MCP configuration reports and mutations.

use vulcan_core::{PermissionGuard, ProfilePermissionGuard, VaultPaths};

use crate::commit::AutoCommitPolicy;
use crate::config::{
    apply_config_set_report, build_config_show_report, config_set_changed_files,
    plan_config_set_report, ConfigSetReport, ConfigShowReport,
};
use crate::mcp_protocol::{McpConfigSetArgs, McpConfigShowArgs, McpMethodError};

pub fn config_show(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    profile_name: &str,
    args: &McpConfigShowArgs,
) -> Result<ConfigShowReport, McpMethodError> {
    guard
        .check_config_read()
        .map_err(|error| McpMethodError::tool(error.to_string()))?;
    build_config_show_report(paths, args.section.as_deref(), Some(profile_name))
        .map_err(|error| McpMethodError::tool(error.to_string()))
}

pub fn config_set(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    profile_name: &str,
    args: &McpConfigSetArgs,
) -> Result<ConfigSetReport, McpMethodError> {
    guard
        .check_config_write()
        .map_err(|error| McpMethodError::tool(error.to_string()))?;
    let had_gitignore = paths.gitignore_file().exists();
    let mut report = plan_config_set_report(paths, &args.key, &args.value, args.dry_run)
        .map_err(|error| McpMethodError::tool(error.to_string()))?;
    if !args.dry_run && report.updated {
        report = apply_config_set_report(paths, report)
            .map_err(|error| McpMethodError::tool(error.to_string()))?;
        AutoCommitPolicy::for_mutation(paths, args.no_commit)
            .commit(
                paths,
                "config-set",
                &config_set_changed_files(paths, had_gitignore),
                Some(profile_name),
                true,
            )
            .map_err(McpMethodError::tool)?;
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use vulcan_core::{initialize_vulcan_dir, resolve_permission_profile};

    #[test]
    fn config_set_dry_run_and_live_apply_share_a_permission_boundary() {
        let temporary = tempfile::tempdir().expect("temporary vault");
        let paths = VaultPaths::new(temporary.path());
        initialize_vulcan_dir(&paths).expect("initialize vault");
        assert!(!paths.config_file().exists());
        let readonly = ProfilePermissionGuard::new(
            &paths,
            resolve_permission_profile(&paths, Some("readonly")).expect("readonly profile"),
        );
        let unrestricted = ProfilePermissionGuard::new(
            &paths,
            resolve_permission_profile(&paths, Some("unrestricted")).expect("unrestricted profile"),
        );
        let mut args = McpConfigSetArgs {
            key: "periodic.daily.template".to_string(),
            value: "Templates/Daily".to_string(),
            dry_run: true,
            no_commit: true,
        };
        assert!(matches!(
            config_set(&paths, &readonly, "readonly", &args),
            Err(McpMethodError::Tool { .. })
        ));
        let preview =
            config_set(&paths, &unrestricted, "unrestricted", &args).expect("dry-run config set");
        assert!(preview.dry_run);
        assert!(!paths.config_file().exists());
        args.dry_run = false;
        let applied =
            config_set(&paths, &unrestricted, "unrestricted", &args).expect("live config set");
        assert!(applied.updated);
        assert!(!applied.dry_run);
        assert!(applied.created_config);
        assert!(fs::read_to_string(paths.config_file())
            .expect("config after apply")
            .contains("Templates/Daily"));
        let shown = config_show(
            &paths,
            &unrestricted,
            "unrestricted",
            &McpConfigShowArgs {
                section: Some("periodic.daily".to_string()),
            },
        )
        .expect("config show");
        assert_eq!(shown.config["template"], "Templates/Daily");
    }
}
