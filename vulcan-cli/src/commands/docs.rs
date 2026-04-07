use crate::{CliError, DescribeFormatArg, OutputFormat};

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
    output: OutputFormat,
    format: DescribeFormatArg,
) -> Result<(), CliError> {
    crate::print_describe_report(output, format)
}
