//! `vulcan numerals`: evaluate Numerals calculator blocks and write their
//! `@[name::result]` values. The workflow lives in `vulcan_app::numerals`.

use serde::Serialize;
use vulcan_app::numerals::{evaluate_numerals_note, plan_numerals_apply, NumeralsChange};
use vulcan_core::numerals::outcome_markdown;
use vulcan_core::VaultPaths;

use crate::commands::note::{dispatch_note_write_plugin_hooks, read_existing_markdown_source};
use crate::output::print_json;
use crate::{refresh_after_write, Cli, CliError, NumeralsCommand, OutputFormat};

#[derive(Debug, Serialize)]
struct NumeralsApplyReport {
    path: String,
    dry_run: bool,
    changed: bool,
    changes: Vec<NumeralsChange>,
}

pub(crate) fn handle_numerals_command(
    cli: &Cli,
    paths: &VaultPaths,
    command: &NumeralsCommand,
) -> Result<(), CliError> {
    let profile = cli.permissions.as_deref();
    match command {
        NumeralsCommand::Eval { note } => {
            let target = read_existing_markdown_source(paths, note)?;
            let path = target
                .target
                .vault_relative_path
                .clone()
                .unwrap_or_else(|| target.target.display_path.clone());
            let report = evaluate_numerals_note(
                paths,
                &target.target.config,
                &path,
                &target.source,
                profile,
            )
            .map_err(CliError::operation)?;
            match cli.output {
                OutputFormat::Json => print_json(&report),
                OutputFormat::Human | OutputFormat::Markdown => {
                    if !report.enabled {
                        println!(
                            "Numerals is off for this vault; set `numerals.enabled = true` or run `vulcan config import numerals`"
                        );
                    } else if report.regions.is_empty() {
                        println!("No Numerals blocks or inline calculations in {path}");
                    }
                    for region in &report.regions {
                        let (block, markdown) = outcome_markdown(&region.outcome);
                        if block {
                            println!("Line {}:\n{markdown}\n", region.line_number);
                        } else {
                            println!("Line {}: {markdown}", region.line_number);
                        }
                    }
                    Ok(())
                }
            }
        }
        NumeralsCommand::Apply { note, dry_run } => run_numerals_apply(cli, paths, note, *dry_run),
    }
}

fn run_numerals_apply(
    cli: &Cli,
    paths: &VaultPaths,
    note: &str,
    dry_run: bool,
) -> Result<(), CliError> {
    let profile = cli.permissions.as_deref();
    let target = read_existing_markdown_source(paths, note)?;
    let Some(relative_path) = target.target.vault_relative_path.clone() else {
        return Err(CliError::operation(
            "`numerals apply` writes notes inside the vault only",
        ));
    };
    let report = evaluate_numerals_note(
        paths,
        &target.target.config,
        &relative_path,
        &target.source,
        profile,
    )
    .map_err(CliError::operation)?;
    let plan = plan_numerals_apply(&target.source, &report);
    let changed = !plan.changes.is_empty();
    if changed && !dry_run {
        let verbosity = cli.verbosity();
        dispatch_note_write_plugin_hooks(
            paths,
            profile,
            &relative_path,
            "numerals",
            Some(&target.source),
            &plan.updated,
            verbosity,
        )?;
        vulcan_app::notes::write_note_content(
            paths,
            &relative_path,
            Some(&target.source),
            &plan.updated,
            "numerals",
            profile,
            verbosity,
        )
        .map_err(CliError::operation)?;
        refresh_after_write(paths, std::slice::from_ref(&relative_path))?;
    }
    let output = NumeralsApplyReport {
        path: target.target.display_path,
        dry_run,
        changed,
        changes: plan.changes,
    };
    match cli.output {
        OutputFormat::Json => print_json(&output),
        OutputFormat::Human | OutputFormat::Markdown => {
            if output.changes.is_empty() {
                println!("No @[name::result] lines to update in {}", output.path);
            }
            for change in &output.changes {
                println!("{}:{}", output.path, change.line);
                println!("- {}", change.before);
                println!("+ {}", change.after);
            }
            if output.dry_run && output.changed {
                println!("Dry run: nothing written");
            }
            Ok(())
        }
    }
}
