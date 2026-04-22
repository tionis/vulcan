use crate::{CliError, DescribeFormatArg, McpToolPackArg, McpToolPackModeArg, OutputFormat};
use vulcan_core::VaultPaths;

pub(crate) fn handle_help_command(
    output: OutputFormat,
    topic: &[String],
    search: Option<&str>,
    stdout_is_tty: bool,
    use_color: bool,
) -> Result<(), CliError> {
    crate::print_help_command(output, topic, search, stdout_is_tty, use_color)
}

pub(crate) fn handle_describe_command(
    paths: &VaultPaths,
    output: OutputFormat,
    format: DescribeFormatArg,
    tool_pack: &[McpToolPackArg],
    tool_pack_mode: McpToolPackModeArg,
    requested_profile: Option<&str>,
) -> Result<(), CliError> {
    crate::print_describe_report(
        paths,
        output,
        format,
        tool_pack,
        tool_pack_mode,
        requested_profile,
    )
}
