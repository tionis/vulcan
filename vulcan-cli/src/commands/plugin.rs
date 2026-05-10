use crate::output::print_json;
use crate::{
    app_config, config_changed_files, config_target, plugins, print_dataview_js_result,
    selected_permission_guard, warn_auto_commit_if_needed, AutoCommitPolicy, Cli, CliError,
    OutputFormat, PluginCommand, PluginEventArg, PluginSandboxArg,
};
use serde::Serialize;
use std::path::PathBuf;
use vulcan_core::{PermissionGuard, PluginEvent, VaultPaths};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct PluginListReport {
    plugins: Vec<plugins::PluginDescriptor>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct PluginToggleReport {
    name: String,
    enabled: bool,
    updated: bool,
    registered: bool,
    config_path: PathBuf,
    path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct PluginConfigWriteReport {
    name: String,
    updated: bool,
    dry_run: bool,
    config_path: PathBuf,
    operations: Vec<String>,
}

#[allow(clippy::too_many_lines)]
pub(crate) fn handle_plugin_command(
    cli: &Cli,
    paths: &VaultPaths,
    command: &PluginCommand,
    stdout_is_tty: bool,
    use_stdout_color: bool,
) -> Result<(), CliError> {
    match command {
        PluginCommand::List => {
            selected_permission_guard(cli, paths)?
                .check_config_read()
                .map_err(CliError::operation)?;
            print_plugin_list_report(
                cli.output,
                &PluginListReport {
                    plugins: plugins::list_plugins(paths),
                },
            )
        }
        PluginCommand::Enable {
            name,
            target,
            dry_run,
            no_commit,
        } => {
            selected_permission_guard(cli, paths)?
                .check_config_write()
                .map_err(CliError::operation)?;
            run_plugin_toggle_command(
                paths,
                cli.output,
                name,
                true,
                config_target(*target),
                *dry_run,
                *no_commit,
                cli.quiet,
            )
        }
        PluginCommand::Disable {
            name,
            target,
            dry_run,
            no_commit,
        } => {
            selected_permission_guard(cli, paths)?
                .check_config_write()
                .map_err(CliError::operation)?;
            run_plugin_toggle_command(
                paths,
                cli.output,
                name,
                false,
                config_target(*target),
                *dry_run,
                *no_commit,
                cli.quiet,
            )
        }
        PluginCommand::Set {
            name,
            path,
            clear_path,
            enable,
            disable,
            add_events,
            remove_events,
            sandbox,
            clear_sandbox,
            permission_profile,
            clear_permission_profile,
            description,
            clear_description,
            target,
            dry_run,
            no_commit,
        } => {
            selected_permission_guard(cli, paths)?
                .check_config_write()
                .map_err(CliError::operation)?;
            run_plugin_set_command(
                paths,
                cli.output,
                name,
                path.as_deref(),
                *clear_path,
                *enable,
                *disable,
                add_events,
                remove_events,
                *sandbox,
                *clear_sandbox,
                permission_profile.as_deref(),
                *clear_permission_profile,
                description.as_deref(),
                *clear_description,
                config_target(*target),
                *dry_run,
                *no_commit,
                cli.quiet,
            )
        }
        PluginCommand::Delete {
            name,
            target,
            dry_run,
            no_commit,
        } => {
            selected_permission_guard(cli, paths)?
                .check_config_write()
                .map_err(CliError::operation)?;
            run_plugin_delete_command(
                paths,
                cli.output,
                name,
                config_target(*target),
                *dry_run,
                *no_commit,
                cli.quiet,
            )
        }
        PluginCommand::Run { name } => {
            selected_permission_guard(cli, paths)?
                .check_execute()
                .map_err(CliError::operation)?;
            let result = plugins::run_plugin(paths, cli.permissions.as_deref(), name)?;
            print_dataview_js_result(cli.output, &result, false, stdout_is_tty, use_stdout_color)
        }
    }
}

#[allow(clippy::too_many_arguments, clippy::fn_params_excessive_bools)]
fn run_plugin_toggle_command(
    paths: &VaultPaths,
    output: OutputFormat,
    name: &str,
    enabled: bool,
    target: app_config::ConfigTarget,
    dry_run: bool,
    no_commit: bool,
    quiet: bool,
) -> Result<(), CliError> {
    let had_gitignore = paths.gitignore_file().exists();
    let current = plugin_descriptor_for_name(paths, name);
    let operations = vec![app_config::ConfigMutationOperation::Set {
        key: format!("plugins.{name}.enabled"),
        value: toml::Value::Boolean(enabled),
    }];
    let mut batch = app_config::plan_config_batch_report(paths, &operations, target, dry_run)?;

    if !dry_run && batch.updated {
        let auto_commit = AutoCommitPolicy::for_mutation(paths, no_commit);
        warn_auto_commit_if_needed(&auto_commit, quiet);
        batch = app_config::apply_config_batch_report(paths, batch)?;
        auto_commit
            .commit(
                paths,
                "plugin-config",
                &config_changed_files(paths, &batch.config_path, had_gitignore),
                None,
                quiet,
            )
            .map_err(CliError::operation)?;
    }

    let report = PluginToggleReport {
        name: name.to_string(),
        enabled,
        updated: batch.updated,
        registered: current.registered || batch.updated,
        config_path: batch.config_path,
        path: current.path,
    };
    print_plugin_toggle_report(output, &report)
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    clippy::fn_params_excessive_bools
)]
fn run_plugin_set_command(
    paths: &VaultPaths,
    output: OutputFormat,
    name: &str,
    path: Option<&str>,
    clear_path: bool,
    enable: bool,
    disable: bool,
    add_events: &[PluginEventArg],
    remove_events: &[PluginEventArg],
    sandbox: Option<PluginSandboxArg>,
    clear_sandbox: bool,
    permission_profile: Option<&str>,
    clear_permission_profile: bool,
    description: Option<&str>,
    clear_description: bool,
    target: app_config::ConfigTarget,
    dry_run: bool,
    no_commit: bool,
    quiet: bool,
) -> Result<(), CliError> {
    let mut operations = Vec::new();
    let mut labels = Vec::new();
    let current = plugin_descriptor_for_name(paths, name);

    if let Some(path) = path {
        operations.push(app_config::ConfigMutationOperation::Set {
            key: format!("plugins.{name}.path"),
            value: toml::Value::String(path.to_string()),
        });
        labels.push(format!("set path = {path}"));
    }
    if clear_path {
        operations.push(app_config::ConfigMutationOperation::Unset {
            key: format!("plugins.{name}.path"),
        });
        labels.push("clear path".to_string());
    }
    if enable {
        operations.push(app_config::ConfigMutationOperation::Set {
            key: format!("plugins.{name}.enabled"),
            value: toml::Value::Boolean(true),
        });
        labels.push("enable".to_string());
    }
    if disable {
        operations.push(app_config::ConfigMutationOperation::Set {
            key: format!("plugins.{name}.enabled"),
            value: toml::Value::Boolean(false),
        });
        labels.push("disable".to_string());
    }
    if !add_events.is_empty() || !remove_events.is_empty() {
        let mut events = current.events;
        for event in add_events.iter().copied().map(plugin_event) {
            if !events.contains(&event) {
                events.push(event);
            }
        }
        events.retain(|event| {
            !remove_events
                .iter()
                .copied()
                .map(plugin_event)
                .any(|candidate| candidate == *event)
        });
        events.sort();
        if events.is_empty() {
            operations.push(app_config::ConfigMutationOperation::Unset {
                key: format!("plugins.{name}.events"),
            });
            labels.push("clear events".to_string());
        } else {
            operations.push(app_config::ConfigMutationOperation::Set {
                key: format!("plugins.{name}.events"),
                value: toml::Value::Array(
                    events
                        .iter()
                        .map(|event| toml::Value::String(event.handler_name().to_string()))
                        .collect(),
                ),
            });
            labels.push("update events".to_string());
        }
    }
    if let Some(sandbox) = sandbox {
        operations.push(app_config::ConfigMutationOperation::Set {
            key: format!("plugins.{name}.sandbox"),
            value: toml::Value::String(plugin_sandbox(sandbox).to_string()),
        });
        labels.push("set sandbox".to_string());
    }
    if clear_sandbox {
        operations.push(app_config::ConfigMutationOperation::Unset {
            key: format!("plugins.{name}.sandbox"),
        });
        labels.push("clear sandbox".to_string());
    }
    if let Some(permission_profile) = permission_profile {
        operations.push(app_config::ConfigMutationOperation::Set {
            key: format!("plugins.{name}.permission_profile"),
            value: toml::Value::String(permission_profile.to_string()),
        });
        labels.push(format!("set permission profile = {permission_profile}"));
    }
    if clear_permission_profile {
        operations.push(app_config::ConfigMutationOperation::Unset {
            key: format!("plugins.{name}.permission_profile"),
        });
        labels.push("clear permission profile".to_string());
    }
    if let Some(description) = description {
        operations.push(app_config::ConfigMutationOperation::Set {
            key: format!("plugins.{name}.description"),
            value: toml::Value::String(description.to_string()),
        });
        labels.push("set description".to_string());
    }
    if clear_description {
        operations.push(app_config::ConfigMutationOperation::Unset {
            key: format!("plugins.{name}.description"),
        });
        labels.push("clear description".to_string());
    }
    if operations.is_empty() {
        return Err(CliError::operation(
            "plugin set requires at least one change flag",
        ));
    }

    let had_gitignore = paths.gitignore_file().exists();
    let mut batch = app_config::plan_config_batch_report(paths, &operations, target, dry_run)?;
    if !dry_run && batch.updated {
        let auto_commit = AutoCommitPolicy::for_mutation(paths, no_commit);
        warn_auto_commit_if_needed(&auto_commit, quiet);
        batch = app_config::apply_config_batch_report(paths, batch)?;
        auto_commit
            .commit(
                paths,
                "plugin-config",
                &config_changed_files(paths, &batch.config_path, had_gitignore),
                None,
                quiet,
            )
            .map_err(CliError::operation)?;
    }

    print_plugin_config_write_report(
        output,
        &PluginConfigWriteReport {
            name: name.to_string(),
            updated: batch.updated,
            dry_run,
            config_path: batch.config_path,
            operations: labels,
        },
    )
}

fn run_plugin_delete_command(
    paths: &VaultPaths,
    output: OutputFormat,
    name: &str,
    target: app_config::ConfigTarget,
    dry_run: bool,
    no_commit: bool,
    quiet: bool,
) -> Result<(), CliError> {
    let had_gitignore = paths.gitignore_file().exists();
    let operations = vec![app_config::ConfigMutationOperation::Unset {
        key: format!("plugins.{name}"),
    }];
    let mut batch = app_config::plan_config_batch_report(paths, &operations, target, dry_run)?;
    if !dry_run && batch.updated {
        let auto_commit = AutoCommitPolicy::for_mutation(paths, no_commit);
        warn_auto_commit_if_needed(&auto_commit, quiet);
        batch = app_config::apply_config_batch_report(paths, batch)?;
        auto_commit
            .commit(
                paths,
                "plugin-config",
                &config_changed_files(paths, &batch.config_path, had_gitignore),
                None,
                quiet,
            )
            .map_err(CliError::operation)?;
    }
    print_plugin_config_write_report(
        output,
        &PluginConfigWriteReport {
            name: name.to_string(),
            updated: batch.updated,
            dry_run,
            config_path: batch.config_path,
            operations: vec!["delete registration".to_string()],
        },
    )
}

fn plugin_descriptor_for_name(paths: &VaultPaths, name: &str) -> plugins::PluginDescriptor {
    plugins::list_plugins(paths)
        .into_iter()
        .find(|plugin| plugin.name == name)
        .unwrap_or_else(|| plugins::PluginDescriptor {
            name: name.to_string(),
            path: vulcan_app::plugins::plugin_default_config_path(name),
            exists: false,
            registered: false,
            enabled: false,
            events: Vec::new(),
            sandbox: None,
            permission_profile: None,
            description: None,
        })
}

fn plugin_event(event: PluginEventArg) -> PluginEvent {
    match event {
        PluginEventArg::OnNoteWrite => PluginEvent::OnNoteWrite,
        PluginEventArg::OnNoteCreate => PluginEvent::OnNoteCreate,
        PluginEventArg::OnNoteDelete => PluginEvent::OnNoteDelete,
        PluginEventArg::OnPreCommit => PluginEvent::OnPreCommit,
        PluginEventArg::OnPostCommit => PluginEvent::OnPostCommit,
        PluginEventArg::OnScanComplete => PluginEvent::OnScanComplete,
        PluginEventArg::OnRefactor => PluginEvent::OnRefactor,
    }
}

fn plugin_sandbox(sandbox: PluginSandboxArg) -> &'static str {
    match sandbox {
        PluginSandboxArg::Strict => "strict",
        PluginSandboxArg::Fs => "fs",
        PluginSandboxArg::Net => "net",
        PluginSandboxArg::None => "none",
    }
}

fn print_plugin_list_report(
    output: OutputFormat,
    report: &PluginListReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Json => print_json(report),
        OutputFormat::Human | OutputFormat::Markdown => {
            if report.plugins.is_empty() {
                println!("No plugins.");
                return Ok(());
            }
            for plugin in &report.plugins {
                let state = if plugin.enabled {
                    "enabled"
                } else {
                    "disabled"
                };
                let registration = if plugin.registered {
                    "registered"
                } else {
                    "discovered"
                };
                let availability = if plugin.exists {
                    "available"
                } else {
                    "missing"
                };
                let events = if plugin.events.is_empty() {
                    "manual-only".to_string()
                } else {
                    plugin
                        .events
                        .iter()
                        .map(|event| event.handler_name())
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                println!(
                    "- {} [{}; {}; {}] {}",
                    plugin.name,
                    state,
                    registration,
                    availability,
                    plugin.path.display()
                );
                println!("  events: {events}");
            }
            Ok(())
        }
    }
}

fn print_plugin_toggle_report(
    output: OutputFormat,
    report: &PluginToggleReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Json => print_json(report),
        OutputFormat::Human | OutputFormat::Markdown => {
            let action = if report.enabled {
                "Enabled"
            } else {
                "Disabled"
            };
            if report.updated {
                println!(
                    "{action} plugin {} in {}",
                    report.name,
                    report.config_path.display()
                );
            } else {
                println!(
                    "No changes for plugin {} in {}",
                    report.name,
                    report.config_path.display()
                );
            }
            Ok(())
        }
    }
}

fn print_plugin_config_write_report(
    output: OutputFormat,
    report: &PluginConfigWriteReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Json => print_json(report),
        OutputFormat::Human | OutputFormat::Markdown => {
            if report.dry_run {
                if report.updated {
                    println!(
                        "Would update plugin {} in {}",
                        report.name,
                        report.config_path.display()
                    );
                } else {
                    println!(
                        "No changes for plugin {} in {}",
                        report.name,
                        report.config_path.display()
                    );
                }
            } else if report.updated {
                println!(
                    "Updated plugin {} in {}",
                    report.name,
                    report.config_path.display()
                );
            } else {
                println!(
                    "No changes for plugin {} in {}",
                    report.name,
                    report.config_path.display()
                );
            }
            if !report.operations.is_empty() {
                println!("  {}", report.operations.join(", "));
            }
            Ok(())
        }
    }
}
