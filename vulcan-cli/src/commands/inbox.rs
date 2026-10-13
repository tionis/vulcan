use crate::commit::AutoCommitPolicy;
use crate::output::print_json;
use crate::{
    inbox_input_text, run_incremental_scan, warn_auto_commit_if_needed, CliError, OutputFormat,
};
use serde::Serialize;
use std::fs;
use std::path::PathBuf;
use vulcan_app::notes::{read_note_for_update, write_note_content};
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
    let relative_path =
        vulcan_app::inbox::inbox_path(&vulcan_core::load_vault_config(paths).config.inbox)
            .map_err(CliError::operation)?;
    let raw_text = inbox_input_text(text, file)?;
    let absolute_path = paths.vault_root().join(&relative_path);
    if let Some(parent) = absolute_path.parent() {
        fs::create_dir_all(parent).map_err(CliError::operation)?;
    }
    let existing = read_note_for_update(paths, &relative_path).map_err(CliError::operation)?;
    let (_, updated) = vulcan_app::inbox::plan_inbox_capture(
        paths,
        existing.as_deref().unwrap_or_default(),
        &raw_text,
    )
    .map_err(CliError::operation)?;
    write_note_content(
        paths,
        &relative_path,
        existing.as_deref(),
        &updated,
        "inbox",
        None,
        verbosity,
    )
    .map_err(CliError::operation)?;
    run_incremental_scan(paths, OutputFormat::Human, false, Verbosity::Normal)?;
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
