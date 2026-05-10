use crate::commit::AutoCommitPolicy;
use crate::output::print_json;
use crate::{
    append_at_end, append_under_heading, inbox_input_text, render_inbox_entry,
    run_incremental_scan, warn_auto_commit_if_needed, CliError, OutputFormat,
};
use serde::Serialize;
use std::fs;
use std::path::PathBuf;
use vulcan_app::templates::{template_variables_for_path, TemplateTimestamp};
use vulcan_core::paths::{normalize_relative_input_path, RelativePathOptions};
use vulcan_core::VaultPaths;

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
    quiet: bool,
) -> Result<InboxReport, CliError> {
    let auto_commit = AutoCommitPolicy::for_mutation(paths, no_commit);
    warn_auto_commit_if_needed(&auto_commit, quiet);
    let inbox_config = vulcan_core::load_vault_config(paths).config.inbox;
    let relative_path = normalize_relative_input_path(
        &inbox_config.path,
        RelativePathOptions {
            expected_extension: Some("md"),
            append_extension_if_missing: true,
        },
    )
    .map_err(CliError::operation)?;

    let raw_text = inbox_input_text(text, file)?;
    let variables = template_variables_for_path(&relative_path, TemplateTimestamp::current());
    let rendered_entry = render_inbox_entry(&inbox_config.format, &raw_text, &variables);
    let entry = if inbox_config.timestamp {
        format!("{} {}", variables.datetime, rendered_entry)
    } else {
        rendered_entry
    };

    let absolute_path = paths.vault_root().join(&relative_path);
    if let Some(parent) = absolute_path.parent() {
        fs::create_dir_all(parent).map_err(CliError::operation)?;
    }
    let existing = fs::read_to_string(&absolute_path).unwrap_or_default();
    let updated = if let Some(heading) = inbox_config.heading.as_deref() {
        append_under_heading(&existing, heading, &entry)
    } else {
        append_at_end(&existing, &entry)
    };
    fs::write(&absolute_path, updated).map_err(CliError::operation)?;
    run_incremental_scan(paths, OutputFormat::Human, false, false)?;
    auto_commit
        .commit(
            paths,
            "inbox",
            std::slice::from_ref(&relative_path),
            None,
            false,
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
