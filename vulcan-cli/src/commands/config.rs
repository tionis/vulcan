use crate::app_config::{
    self, ConfigGetReport, ConfigListReport, ConfigSetReport, ConfigShowReport, ConfigTarget,
    ConfigUnsetReport, ConfigValueKind,
};
use crate::commit::AutoCommitPolicy;
use crate::config_tui;
use crate::output::print_json;
use crate::{
    warn_auto_commit_if_needed, Cli, CliError, ConfigAliasCommand, ConfigCommand,
    ConfigImportListReport, ConfigPermissionsCommand, ConfigPermissionsProfileCommand,
    ConfigTargetArg, OutputFormat,
};
use serde_json::Value;
use std::io::IsTerminal;
use toml::Value as TomlValue;
use vulcan_core::{load_permission_profiles, PermissionGuard, PermissionProfile, VaultPaths};

#[allow(clippy::too_many_lines)]
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
        ConfigCommand::List { section } => {
            crate::selected_permission_guard(cli, paths)?
                .check_config_read()
                .map_err(CliError::operation)?;
            run_config_list(paths, cli.output, section.as_deref())
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
            target,
            dry_run,
            no_commit,
        } => {
            crate::selected_permission_guard(cli, paths)?
                .check_config_write()
                .map_err(CliError::operation)?;
            run_config_set(
                paths,
                cli.output,
                key,
                value,
                config_target(*target),
                *dry_run,
                *no_commit,
                cli.quiet,
            )
        }
        ConfigCommand::Unset {
            key,
            target,
            dry_run,
            no_commit,
        } => {
            crate::selected_permission_guard(cli, paths)?
                .check_config_write()
                .map_err(CliError::operation)?;
            run_config_unset(
                paths,
                cli.output,
                key,
                config_target(*target),
                *dry_run,
                *no_commit,
                cli.quiet,
            )
        }
        ConfigCommand::Alias { command } => {
            if command == &ConfigAliasCommand::List {
                crate::selected_permission_guard(cli, paths)?
                    .check_config_read()
                    .map_err(CliError::operation)?;
            } else {
                crate::selected_permission_guard(cli, paths)?
                    .check_config_write()
                    .map_err(CliError::operation)?;
            }
            run_config_alias(paths, cli.output, command, cli.quiet)
        }
        ConfigCommand::Permissions { command } => match command {
            ConfigPermissionsCommand::Profile { command } => match command {
                ConfigPermissionsProfileCommand::List
                | ConfigPermissionsProfileCommand::Show { .. } => {
                    crate::selected_permission_guard(cli, paths)?
                        .check_config_read()
                        .map_err(CliError::operation)?;
                    run_config_permissions_profile(paths, cli.output, command, cli.quiet)
                }
                _ => {
                    crate::selected_permission_guard(cli, paths)?
                        .check_config_write()
                        .map_err(CliError::operation)?;
                    run_config_permissions_profile(paths, cli.output, command, cli.quiet)
                }
            },
        },
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

fn run_config_list(
    paths: &VaultPaths,
    output: OutputFormat,
    section: Option<&str>,
) -> Result<(), CliError> {
    let report = app_config::build_config_list_report(paths, section)?;
    print_config_list_report(output, &report)
}

fn run_config_get(paths: &VaultPaths, output: OutputFormat, key: &str) -> Result<(), CliError> {
    let report = app_config::build_config_get_report(paths, key)?;
    print_config_get_report(output, &report)
}

#[allow(clippy::too_many_arguments)]
fn run_config_set(
    paths: &VaultPaths,
    output: OutputFormat,
    key: &str,
    raw_value: &str,
    target: ConfigTarget,
    dry_run: bool,
    no_commit: bool,
    quiet: bool,
) -> Result<(), CliError> {
    let had_gitignore = paths.gitignore_file().exists();
    let mut report =
        app_config::plan_config_set_report_for_target(paths, key, raw_value, target, dry_run)?;

    if !dry_run && report.updated {
        let auto_commit = AutoCommitPolicy::for_mutation(paths, no_commit);
        warn_auto_commit_if_needed(&auto_commit, quiet);
        report = app_config::apply_config_set_report(paths, report)?;
        auto_commit
            .commit(
                paths,
                "config-set",
                &crate::config_changed_files(paths, &report.config_path, had_gitignore),
                None,
                quiet,
            )
            .map_err(CliError::operation)?;
    }

    print_config_set_report(output, &report)
}

#[allow(clippy::too_many_arguments)]
fn run_config_set_toml(
    paths: &VaultPaths,
    output: OutputFormat,
    key: &str,
    value: &TomlValue,
    target: ConfigTarget,
    dry_run: bool,
    no_commit: bool,
    quiet: bool,
) -> Result<(), CliError> {
    let had_gitignore = paths.gitignore_file().exists();
    let mut report = app_config::plan_config_set_report_to(paths, key, value, target, dry_run)?;

    if !dry_run && report.updated {
        let auto_commit = AutoCommitPolicy::for_mutation(paths, no_commit);
        warn_auto_commit_if_needed(&auto_commit, quiet);
        report = app_config::apply_config_set_report(paths, report)?;
        auto_commit
            .commit(
                paths,
                "config-set",
                &crate::config_changed_files(paths, &report.config_path, had_gitignore),
                None,
                quiet,
            )
            .map_err(CliError::operation)?;
    }

    print_config_set_report(output, &report)
}

fn run_config_unset(
    paths: &VaultPaths,
    output: OutputFormat,
    key: &str,
    target: ConfigTarget,
    dry_run: bool,
    no_commit: bool,
    quiet: bool,
) -> Result<(), CliError> {
    let had_gitignore = paths.gitignore_file().exists();
    let mut report = app_config::plan_config_unset_report(paths, key, target, dry_run)?;

    if !dry_run && report.updated {
        let auto_commit = AutoCommitPolicy::for_mutation(paths, no_commit);
        warn_auto_commit_if_needed(&auto_commit, quiet);
        report = app_config::apply_config_unset_report(paths, report)?;
        auto_commit
            .commit(
                paths,
                "config-unset",
                &crate::config_changed_files(paths, &report.config_path, had_gitignore),
                None,
                quiet,
            )
            .map_err(CliError::operation)?;
    }

    print_config_unset_report(output, &report)
}

fn run_config_alias(
    paths: &VaultPaths,
    output: OutputFormat,
    command: &ConfigAliasCommand,
    quiet: bool,
) -> Result<(), CliError> {
    match command {
        ConfigAliasCommand::List => run_config_show(paths, output, Some("aliases"), None),
        ConfigAliasCommand::Set {
            name,
            expansion,
            target,
            dry_run,
            no_commit,
        } => run_config_set(
            paths,
            output,
            &format!("aliases.{name}"),
            expansion,
            config_target(*target),
            *dry_run,
            *no_commit,
            quiet,
        ),
        ConfigAliasCommand::Delete {
            name,
            target,
            dry_run,
            no_commit,
        } => run_config_unset(
            paths,
            output,
            &format!("aliases.{name}"),
            config_target(*target),
            *dry_run,
            *no_commit,
            quiet,
        ),
    }
}

fn run_config_permissions_profile(
    paths: &VaultPaths,
    output: OutputFormat,
    command: &ConfigPermissionsProfileCommand,
    quiet: bool,
) -> Result<(), CliError> {
    match command {
        ConfigPermissionsProfileCommand::List => {
            run_config_show(paths, output, Some("permissions"), None)
        }
        ConfigPermissionsProfileCommand::Show { name } => run_config_show(
            paths,
            output,
            Some(&format!("permissions.profiles.{name}")),
            Some(name),
        ),
        ConfigPermissionsProfileCommand::Create {
            name,
            clone,
            target,
            dry_run,
            no_commit,
        } => {
            let available = load_permission_profiles(paths);
            let base_profile = if let Some(base) = clone {
                available.profiles.get(base).cloned().ok_or_else(|| {
                    CliError::operation(format!("unknown permission profile `{base}`"))
                })?
            } else {
                PermissionProfile::default()
            };
            let value = TomlValue::try_from(base_profile).map_err(CliError::operation)?;
            run_config_set_toml(
                paths,
                output,
                &format!("permissions.profiles.{name}"),
                &value,
                config_target(*target),
                *dry_run,
                *no_commit,
                quiet,
            )
        }
        ConfigPermissionsProfileCommand::Set {
            name,
            dimension,
            value,
            target,
            dry_run,
            no_commit,
        } => run_config_set(
            paths,
            output,
            &format!("permissions.profiles.{name}.{dimension}"),
            value,
            config_target(*target),
            *dry_run,
            *no_commit,
            quiet,
        ),
        ConfigPermissionsProfileCommand::Delete {
            name,
            target,
            dry_run,
            no_commit,
        } => run_config_unset(
            paths,
            output,
            &format!("permissions.profiles.{name}"),
            config_target(*target),
            *dry_run,
            *no_commit,
            quiet,
        ),
    }
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

fn print_config_list_report(
    output: OutputFormat,
    report: &ConfigListReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Json => print_json(report),
        OutputFormat::Human | OutputFormat::Markdown => {
            if report.entries.is_empty() {
                println!("No config keys matched.");
                return Ok(());
            }
            for entry in &report.entries {
                let kind = match entry.kind {
                    ConfigValueKind::String => "string",
                    ConfigValueKind::Integer => "integer",
                    ConfigValueKind::Float => "float",
                    ConfigValueKind::Boolean => "boolean",
                    ConfigValueKind::Array => "array",
                    ConfigValueKind::Object => "object",
                    ConfigValueKind::Enum => "enum",
                    ConfigValueKind::Flexible => "flexible",
                };
                let target_support = match entry.target_support {
                    app_config::ConfigTargetSupport::SharedOnly => "shared",
                    app_config::ConfigTargetSupport::LocalOnly => "local",
                    app_config::ConfigTargetSupport::SharedAndLocal => "shared|local",
                };
                let value_source = match entry.value_source {
                    app_config::ConfigValueSource::Default => "default",
                    app_config::ConfigValueSource::ObsidianImport => "obsidian_import",
                    app_config::ConfigValueSource::SharedOverride => "shared_override",
                    app_config::ConfigValueSource::LocalOverride => "local_override",
                    app_config::ConfigValueSource::Unset => "unset",
                };
                println!(
                    "{} [{}; source={}; target={}]",
                    entry.key, kind, value_source, target_support
                );
                println!("  {}", entry.description);
                if let Some(value) = &entry.effective_value {
                    println!(
                        "  effective: {}",
                        serde_json::to_string(value).map_err(CliError::operation)?
                    );
                }
                if let Some(default_display) = &entry.default_display {
                    println!("  default: {default_display}");
                } else if let Some(default_value) = &entry.default_value {
                    println!(
                        "  default: {}",
                        serde_json::to_string(default_value).map_err(CliError::operation)?
                    );
                }
                if !entry.enum_values.is_empty() {
                    println!("  values: {}", entry.enum_values.join(", "));
                }
                if let Some(command) = &entry.preferred_command {
                    println!("  preferred: {command}");
                }
                if let Some(example) = entry.examples.first() {
                    println!("  example: {example}");
                }
            }
            Ok(())
        }
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

fn print_config_unset_report(
    output: OutputFormat,
    report: &ConfigUnsetReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Json => print_json(report),
        OutputFormat::Human | OutputFormat::Markdown => {
            if report.dry_run {
                if report.updated {
                    println!(
                        "Would remove {} from {}",
                        report.key,
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
                    "Removed {} from {}",
                    report.key,
                    report.config_path.display()
                );
            } else {
                println!(
                    "No changes for {} in {}",
                    report.key,
                    report.config_path.display()
                );
            }
            Ok(())
        }
    }
}

fn config_target(target: ConfigTargetArg) -> ConfigTarget {
    match target {
        ConfigTargetArg::Shared => ConfigTarget::Shared,
        ConfigTargetArg::Local => ConfigTarget::Local,
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
