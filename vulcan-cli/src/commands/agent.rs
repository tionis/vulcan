use crate::{selected_permission_guard, AgentCommand, Cli, CliError};
use vulcan_core::{PermissionGuard, VaultPaths};

pub(crate) fn handle_agent_command(
    cli: &Cli,
    paths: &VaultPaths,
    command: &AgentCommand,
) -> Result<(), CliError> {
    match command {
        AgentCommand::Install(args) => {
            let guard = selected_permission_guard(cli, paths)?;
            for path in crate::bundled_support_relative_paths() {
                guard.check_write_path(path).map_err(CliError::operation)?;
            }
            let report = crate::run_agent_install_command(paths, args)?;
            crate::print_agent_install_summary(cli.output, paths, &report)
        }
    }
}
