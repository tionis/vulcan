use crate::{CliError, DescribeFormatArg, OutputFormat};

pub(crate) fn handle_help_command(
    output: OutputFormat,
    topic: &[String],
    search: Option<&str>,
) -> Result<(), CliError> {
    crate::print_help_command(output, topic, search)
}

pub(crate) fn handle_describe_command(
    output: OutputFormat,
    format: DescribeFormatArg,
) -> Result<(), CliError> {
    crate::print_describe_report(output, format)
}
