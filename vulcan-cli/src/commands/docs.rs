use crate::{CliError, DescribeFormatArg, OutputFormat};

pub(crate) fn handle_help_command(
    output: OutputFormat,
    topic: &[String],
    search: Option<&str>,
    use_color: bool,
) -> Result<(), CliError> {
    crate::print_help_command(output, topic, search, use_color)
}

pub(crate) fn handle_describe_command(
    output: OutputFormat,
    format: DescribeFormatArg,
) -> Result<(), CliError> {
    crate::print_describe_report(output, format)
}
