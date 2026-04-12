use crate::app_config::{self, ConfigGetReport, ConfigSetReport, ConfigShowReport};
use crate::commit::AutoCommitPolicy;
use crate::config_tui;
use crate::output::print_json;
use crate::{
    warn_auto_commit_if_needed, Cli, CliError, ConfigCommand, ConfigImportListReport, OutputFormat,
};
use serde_json::Value;
use std::io::IsTerminal;
use vulcan_core::{PermissionGuard, VaultPaths};

pub(crate) fn handle_config_command(
    cli: &Cli,
    paths: &VaultPaths,
    command: &ConfigCommand,
    stdout_is_tty: bool,
) -> Result<(), CliError> {
    match command {
        ConfigCommand::Show { section } => {
            crate::selected_permission_guard(cli, paths)?
                .check_config_read()
                .map_err(CliError::operation)?;
            run_config_show(
                paths,
                cli.output,
                section.as_deref(),
                cli.permissions.as_deref(),
            )
        }
        ConfigCommand::Get { key } => {
            crate::selected_permission_guard(cli, paths)?
                .check_config_read()
                .map_err(CliError::operation)?;
            run_config_get(paths, cli.output, key)
        }
        ConfigCommand::Edit { no_commit } => {
            crate::selected_permission_guard(cli, paths)?
                .check_config_write()
                .map_err(CliError::operation)?;
            if cli.output != OutputFormat::Human
                || !stdout_is_tty
                || !std::io::stdin().is_terminal()
            {
                return Err(CliError::operation(
                    "config edit requires an interactive terminal with `--output human`",
                ));
            }
            config_tui::run_config_tui(paths, *no_commit, cli.quiet).map_err(CliError::operation)
        }
        ConfigCommand::Set {
            key,
            value,
            dry_run,
            no_commit,
        } => {
            crate::selected_permission_guard(cli, paths)?
                .check_config_write()
                .map_err(CliError::operation)?;
            run_config_set(
                paths, cli.output, key, value, *dry_run, *no_commit, cli.quiet,
            )
        }
        ConfigCommand::Import(selection) => {
            crate::selected_permission_guard(cli, paths)?
                .check_config_write()
                .map_err(CliError::operation)?;
            if selection.command.is_some() && (selection.all || selection.list) {
                return Err(CliError::operation(
                    "config import accepts either a subcommand, --all, or --list",
                ));
            }
            if selection.list {
                let report = ConfigImportListReport {
                    importers: crate::discover_config_importers(paths)
                        .into_iter()
                        .map(|(_, discovery)| discovery)
                        .collect(),
                };
                return crate::print_config_import_list_report(cli.output, paths, &report);
            }
            if selection.all {
                return crate::run_config_import_batch(
                    paths,
                    cli.output,
                    &selection.args,
                    cli.quiet,
                );
            }
            let Some(command) = selection.command.as_ref() else {
                return Err(CliError::operation(
                    "config import requires a subcommand, --all, or --list",
                ));
            };
            let importer = crate::importer_for_command(command);
            crate::run_config_import(
                paths,
                cli.output,
                importer.as_ref(),
                &selection.args,
                cli.quiet,
            )
        }
    }
}

fn run_config_show(
    paths: &VaultPaths,
    output: OutputFormat,
    section: Option<&str>,
    selected_permission_profile: Option<&str>,
) -> Result<(), CliError> {
    let report = app_config::build_config_show_report(paths, section, selected_permission_profile)?;
    print_config_show_report(output, &report)
}

fn run_config_get(paths: &VaultPaths, output: OutputFormat, key: &str) -> Result<(), CliError> {
    let report = app_config::build_config_get_report(paths, key)?;
    print_config_get_report(output, &report)
}

fn run_config_set(
    paths: &VaultPaths,
    output: OutputFormat,
    key: &str,
    raw_value: &str,
    dry_run: bool,
    no_commit: bool,
    quiet: bool,
) -> Result<(), CliError> {
    let had_gitignore = paths.gitignore_file().exists();
    let mut report = app_config::plan_config_set_report(paths, key, raw_value, dry_run)?;

    if !dry_run && report.updated {
        let auto_commit = AutoCommitPolicy::for_mutation(paths, no_commit);
        warn_auto_commit_if_needed(&auto_commit, quiet);
        report = app_config::apply_config_set_report(paths, report)?;
        auto_commit
            .commit(
                paths,
                "config-set",
                &crate::config_set_changed_files(paths, had_gitignore),
                None,
                quiet,
            )
            .map_err(CliError::operation)?;
    }

    print_config_set_report(output, &report)
}

fn print_config_show_report(
    output: OutputFormat,
    report: &ConfigShowReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            let rendered_value = match report.section.as_deref() {
                Some(section) => {
                    crate::wrap_config_section_toml(section, report.rendered_toml.clone())
                }
                None => report.rendered_toml.clone(),
            };
            let rendered = toml::to_string_pretty(&rendered_value).map_err(CliError::operation)?;
            print!("{rendered}");
            if let Some(active_profile) = report.active_permission_profile.as_deref() {
                println!("# active_permission_profile = \"{active_profile}\"");
                if !report.available_permission_profiles.is_empty() {
                    println!(
                        "# available_permission_profiles = [{}]",
                        report
                            .available_permission_profiles
                            .iter()
                            .map(|name| format!("\"{name}\""))
                            .collect::<Vec<_>>()
                            .join(", ")
                    );
                }
            }
            for diagnostic in &report.diagnostics {
                eprintln!(
                    "warning: {}: {}",
                    diagnostic.path.display(),
                    diagnostic.message
                );
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_config_get_report(output: OutputFormat, report: &ConfigGetReport) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            print_config_value_human(&report.value)?;
            for diagnostic in &report.diagnostics {
                eprintln!(
                    "warning: {}: {}",
                    diagnostic.path.display(),
                    diagnostic.message
                );
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_config_set_report(output: OutputFormat, report: &ConfigSetReport) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            let rendered_value =
                serde_json::to_string(&report.value).map_err(CliError::operation)?;
            if report.dry_run {
                if report.updated {
                    println!(
                        "Would set {} = {} in {}",
                        report.key,
                        rendered_value,
                        report.config_path.display()
                    );
                } else {
                    println!(
                        "No changes for {} in {}",
                        report.key,
                        report.config_path.display()
                    );
                }
            } else if report.updated {
                println!(
                    "Set {} = {} in {}",
                    report.key,
                    rendered_value,
                    report.config_path.display()
                );
            } else {
                println!(
                    "No changes for {} in {}",
                    report.key,
                    report.config_path.display()
                );
            }
            for diagnostic in &report.diagnostics {
                eprintln!(
                    "warning: {}: {}",
                    diagnostic.path.display(),
                    diagnostic.message
                );
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_config_value_human(value: &Value) -> Result<(), CliError> {
    match value {
        Value::Null => println!("null"),
        Value::Bool(value) => println!("{value}"),
        Value::Number(value) => println!("{value}"),
        Value::String(value) => println!("{value}"),
        Value::Array(_) => {
            let rendered = serde_json::to_string_pretty(value).map_err(CliError::operation)?;
            println!("{rendered}");
        }
        Value::Object(_) => {
            return Err(CliError::operation(
                "config get cannot print section objects in human mode",
            ));
        }
    }
    Ok(())
}
