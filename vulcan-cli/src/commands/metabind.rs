//! `vulcan metabind`: list Meta Bind fields, set bound values, and write
//! view results. The workflow lives in `vulcan_app::metabind`.

use serde::Serialize;
use vulcan_app::metabind::{
    list_metabind_fields, plan_metabind_apply, plan_metabind_set, MetaBindChange, MetaBindWritePlan,
};
use vulcan_core::metabind::{field_markdown, MetaBindLevel, MetaBindOutcome};
use vulcan_core::VaultPaths;

use crate::commands::note::{dispatch_note_write_plugin_hooks, read_existing_markdown_source};
use crate::output::print_json;
use crate::{refresh_after_write, Cli, CliError, MetabindCommand, OutputFormat};

#[derive(Debug, Serialize)]
struct MetaBindWriteReport {
    path: String,
    dry_run: bool,
    changed: bool,
    changes: Vec<MetaBindChange>,
    skipped: Vec<String>,
}

pub(crate) fn handle_metabind_command(
    cli: &Cli,
    paths: &VaultPaths,
    command: &MetabindCommand,
) -> Result<(), CliError> {
    let profile = cli.permissions.as_deref();
    let note = match command {
        MetabindCommand::List { note }
        | MetabindCommand::Set { note, .. }
        | MetabindCommand::Apply { note, .. } => note,
    };
    let source = read_existing_markdown_source(paths, note)?;
    let Some(path) = source.target.vault_relative_path.clone() else {
        return Err(CliError::operation(
            "`vulcan metabind` works on notes inside the vault",
        ));
    };
    let config = &source.target.config;
    let (plan, dry_run) = match command {
        MetabindCommand::List { .. } => {
            let report = list_metabind_fields(paths, config, &path, &source.source, profile)
                .map_err(CliError::operation)?;
            return print_field_list(cli, &report);
        }
        MetabindCommand::Set {
            target,
            value,
            dry_run,
            ..
        } => (
            plan_metabind_set(paths, config, &path, &source.source, target, value, profile),
            *dry_run,
        ),
        MetabindCommand::Apply { dry_run, .. } => (
            plan_metabind_apply(paths, config, &path, &source.source, profile),
            *dry_run,
        ),
    };
    let plan = plan.map_err(CliError::operation)?;
    write_plan(cli, paths, &plan, dry_run)?;
    let report = MetaBindWriteReport {
        path,
        dry_run,
        changed: !plan.writes.is_empty(),
        changes: plan.changes,
        skipped: plan.skipped,
    };
    match cli.output {
        OutputFormat::Json => print_json(&report),
        OutputFormat::Human | OutputFormat::Markdown => {
            if report.changes.is_empty() {
                println!("Nothing to change in {}", report.path);
            }
            for change in &report.changes {
                let before = change
                    .before
                    .as_ref()
                    .map_or_else(|| "<missing>".to_string(), ToString::to_string);
                println!(
                    "{}: {}: {before} → {}",
                    change.path, change.target, change.after
                );
            }
            for skipped in &report.skipped {
                println!("skipped {skipped}");
            }
            if report.dry_run && report.changed {
                println!("Dry run: nothing written");
            }
            Ok(())
        }
    }
}

fn write_plan(
    cli: &Cli,
    paths: &VaultPaths,
    plan: &MetaBindWritePlan,
    dry_run: bool,
) -> Result<(), CliError> {
    if dry_run || plan.writes.is_empty() {
        return Ok(());
    }
    let profile = cli.permissions.as_deref();
    let verbosity = cli.verbosity();
    for write in &plan.writes {
        dispatch_note_write_plugin_hooks(
            paths,
            profile,
            &write.path,
            "metabind",
            Some(&write.before),
            &write.after,
            verbosity,
        )?;
        vulcan_app::notes::write_note_content(
            paths,
            &write.path,
            Some(&write.before),
            &write.after,
            "metabind",
            profile,
            verbosity,
        )
        .map_err(CliError::operation)?;
    }
    let written = plan
        .writes
        .iter()
        .map(|write| write.path.clone())
        .collect::<Vec<_>>();
    refresh_after_write(paths, &written)
}

fn print_field_list(
    cli: &Cli,
    report: &vulcan_app::metabind::MetaBindReport,
) -> Result<(), CliError> {
    let path = &report.path;
    match cli.output {
        OutputFormat::Json => print_json(&report),
        OutputFormat::Human | OutputFormat::Markdown => {
            if report.fields.is_empty() {
                println!("No Meta Bind fields in {path}");
            }
            for field in &report.fields {
                let (_, shown) = field_markdown(field);
                let detail = match &field.outcome {
                    MetaBindOutcome::Input {
                        settings, target, ..
                    } => format!(
                        "{} input → {}",
                        settings.input_type,
                        target.as_deref().unwrap_or("(unbound)")
                    ),
                    MetaBindOutcome::View {
                        view_type,
                        write_target,
                        ..
                    } => match write_target {
                        Some(target) => format!("{view_type} view → {target}"),
                        None => format!("{view_type} view"),
                    },
                    MetaBindOutcome::Buttons { .. } => "button".to_string(),
                    MetaBindOutcome::Embed { .. } => "embed".to_string(),
                    MetaBindOutcome::Hidden => "hidden".to_string(),
                    MetaBindOutcome::Notice { .. } => "notice".to_string(),
                    MetaBindOutcome::Error { .. } => "error".to_string(),
                };
                println!("Line {} ({detail}): {shown}", field.line_number);
                for diagnostic in &field.diagnostics {
                    let level = match diagnostic.level {
                        MetaBindLevel::Error => "error",
                        MetaBindLevel::Warning => "warning",
                        MetaBindLevel::Info => "note",
                    };
                    println!("  {level}: {}", diagnostic.message);
                }
            }
            Ok(())
        }
    }
}
