use crate::commit::AutoCommitPolicy;
use crate::output::print_json;
use crate::{inbox_input_text, warn_auto_commit_if_needed, CliError, OutputFormat};
use serde::Serialize;
use std::path::PathBuf;
use vulcan_core::VaultPaths;
use vulcan_core::Verbosity;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct InboxReport {
    pub(crate) path: String,
    pub(crate) appended: bool,
}

pub(crate) fn run_inbox_command(
    paths: &VaultPaths,
    text: Option<&str>,
    file: Option<&PathBuf>,
    no_commit: bool,
    verbosity: Verbosity,
) -> Result<InboxReport, CliError> {
    let auto_commit = AutoCommitPolicy::for_mutation(paths, no_commit);
    warn_auto_commit_if_needed(&auto_commit, verbosity);
    let raw_text = inbox_input_text(text, file)?;
    let relative_path = vulcan_app::inbox::write_inbox_capture(paths, &raw_text, None, verbosity)
        .map_err(CliError::operation)?;
    crate::refresh_after_write(paths, std::slice::from_ref(&relative_path))?;
    auto_commit
        .commit(
            paths,
            "inbox",
            std::slice::from_ref(&relative_path),
            None,
            Verbosity::Normal,
        )
        .map_err(CliError::operation)?;

    Ok(InboxReport {
        path: relative_path,
        appended: true,
    })
}

pub(crate) fn print_inbox_report(
    output: OutputFormat,
    report: &InboxReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            println!("Appended to {}", report.path);
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}
