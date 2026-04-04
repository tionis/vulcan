use crate::{browse_tui, refresh_mode_for_target, Cli, CliError, OutputFormat, RefreshTarget};
use std::io::{self, IsTerminal};
use vulcan_core::VaultPaths;

pub(crate) fn handle_browse_command(
    cli: &Cli,
    paths: &VaultPaths,
    stdout_is_tty: bool,
    no_commit: bool,
) -> Result<(), CliError> {
    if cli.output != OutputFormat::Human || !stdout_is_tty || !io::stdin().is_terminal() {
        return Err(CliError::operation(
            "browse requires an interactive terminal with `--output human`",
        ));
    }
    let refresh_mode = refresh_mode_for_target(paths, cli, RefreshTarget::Browse);
    browse_tui::run_browse_tui(paths, refresh_mode, no_commit).map_err(CliError::operation)
}
