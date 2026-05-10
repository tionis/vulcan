use crate::app_config::{
    self, ConfigGetReport, ConfigListReport, ConfigSetReport, ConfigShowReport, ConfigTarget,
    ConfigUnsetReport, ConfigValueKind,
};
use crate::commit::AutoCommitPolicy;
use crate::config_tui;
use crate::output::print_json;
use crate::{
    warn_auto_commit_if_needed, Cli, CliError, ConfigAliasCommand, ConfigCommand, ConfigImportArgs,
    ConfigImportCommand, ConfigPermissionsCommand, ConfigPermissionsProfileCommand,
    ConfigTargetArg, OutputFormat,
};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeSet;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use toml::Value as TomlValue;
use vulcan_core::config::QuickAddImporter;
use vulcan_core::{
    all_importers, annotate_import_conflicts, load_permission_profiles, ConfigImportReport,
    CoreImporter, DataviewImporter, ImportTarget, KanbanImporter, PeriodicNotesImporter,
    PermissionGuard, PermissionProfile, PluginImporter, TaskNotesImporter, TasksImporter,
    TemplaterImporter, VaultPaths,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ConfigImportDiscoveryItem {
    pub(crate) plugin: String,
    pub(crate) display_name: String,
    pub(crate) detected: bool,
    pub(crate) source_paths: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct ConfigImportListReport {
    importers: Vec<ConfigImportDiscoveryItem>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct ConfigImportBatchReport {
    pub(crate) dry_run: bool,
    pub(crate) target: ImportTarget,
    pub(crate) detected_count: usize,
    pub(crate) imported_count: usize,
    pub(crate) updated_count: usize,
    pub(crate) reports: Vec<ConfigImportReport>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct ConfigImportRenderedReport {
    #[serde(flatten)]
    report: ConfigImportReport,
    #[serde(skip_serializing_if = "Option::is_none")]
    preview_diff: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct ConfigImportRenderedBatchReport {
    pub(crate) dry_run: bool,
    pub(crate) target: ImportTarget,
    pub(crate) detected_count: usize,
    pub(crate) imported_count: usize,
    pub(crate) updated_count: usize,
    pub(crate) reports: Vec<ConfigImportRenderedReport>,
}

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
                    importers: discover_config_importers(paths)
                        .into_iter()
                        .map(|(_, discovery)| discovery)
                        .collect(),
                };
                return print_config_import_list_report(cli.output, paths, &report);
            }
            if selection.all {
                return run_config_import_batch(paths, cli.output, &selection.args, cli.quiet);
            }
            let Some(command) = selection.command.as_ref() else {
                return Err(CliError::operation(
                    "config import requires a subcommand, --all, or --list",
                ));
            };
            let importer = importer_for_command(command);
            run_config_import(
                paths,
                cli.output,
                importer.as_ref(),
                &selection.args,
                cli.quiet,
            )
        }
    }
}

fn config_import_target(target: ConfigTargetArg) -> ImportTarget {
    match target {
        ConfigTargetArg::Shared => ImportTarget::Shared,
        ConfigTargetArg::Local => ImportTarget::Local,
    }
}

fn importer_for_command(command: &ConfigImportCommand) -> Box<dyn PluginImporter> {
    match command {
        ConfigImportCommand::Core => Box::new(CoreImporter),
        ConfigImportCommand::Dataview => Box::new(DataviewImporter),
        ConfigImportCommand::Templater => Box::new(TemplaterImporter),
        ConfigImportCommand::Quickadd => Box::new(QuickAddImporter),
        ConfigImportCommand::Kanban => Box::new(KanbanImporter),
        ConfigImportCommand::PeriodicNotes => Box::new(PeriodicNotesImporter),
        ConfigImportCommand::TaskNotes => Box::new(TaskNotesImporter),
        ConfigImportCommand::Tasks => Box::new(TasksImporter),
    }
}

pub(crate) fn discover_config_importers(
    paths: &VaultPaths,
) -> Vec<(Box<dyn PluginImporter>, ConfigImportDiscoveryItem)> {
    all_importers()
        .into_iter()
        .map(|importer| {
            let source_paths = importer.source_paths(paths);
            let detected = importer.detect(paths);
            let discovery = ConfigImportDiscoveryItem {
                plugin: importer.name().to_string(),
                display_name: importer.display_name().to_string(),
                detected,
                source_paths,
            };
            (importer, discovery)
        })
        .collect()
}

pub(crate) fn normalize_import_discovery_item(
    paths: &VaultPaths,
    item: &ConfigImportDiscoveryItem,
) -> ConfigImportDiscoveryItem {
    ConfigImportDiscoveryItem {
        plugin: item.plugin.clone(),
        display_name: item.display_name.clone(),
        detected: item.detected,
        source_paths: item
            .source_paths
            .iter()
            .map(|path| relativize_config_import_path(paths, path))
            .collect(),
    }
}

fn run_config_import(
    paths: &VaultPaths,
    output: OutputFormat,
    importer: &dyn PluginImporter,
    args: &ConfigImportArgs,
    quiet: bool,
) -> Result<(), CliError> {
    let target = config_import_target(args.target);
    let report = if args.dry_run {
        importer
            .dry_run_to(paths, target)
            .map_err(CliError::operation)?
    } else {
        let auto_commit = AutoCommitPolicy::for_mutation(paths, args.no_commit);
        warn_auto_commit_if_needed(&auto_commit, quiet);
        let had_gitignore = paths.gitignore_file().exists();
        let report = importer
            .import(paths, target)
            .map_err(CliError::operation)?;
        if report.updated {
            auto_commit
                .commit(
                    paths,
                    &format!("config-import-{}", importer.name()),
                    &config_import_changed_files(paths, had_gitignore, &report),
                    None,
                    quiet,
                )
                .map_err(CliError::operation)?;
        }
        report
    };

    print_config_import_report(output, paths, &report)
}

fn run_config_import_batch(
    paths: &VaultPaths,
    output: OutputFormat,
    args: &ConfigImportArgs,
    quiet: bool,
) -> Result<(), CliError> {
    let target = config_import_target(args.target);
    let discovered = discover_config_importers(paths);
    let importers = discovered
        .into_iter()
        .filter_map(|(importer, discovery)| discovery.detected.then_some(importer))
        .collect::<Vec<_>>();
    let detected_count = importers.len();
    let mut reports = Vec::new();

    if args.dry_run {
        for importer in importers {
            reports.push(
                importer
                    .dry_run_to(paths, target)
                    .map_err(CliError::operation)?,
            );
        }
    } else {
        let auto_commit = AutoCommitPolicy::for_mutation(paths, args.no_commit);
        warn_auto_commit_if_needed(&auto_commit, quiet);
        let had_gitignore = paths.gitignore_file().exists();
        for importer in importers {
            reports.push(
                importer
                    .import(paths, target)
                    .map_err(CliError::operation)?,
            );
        }

        if reports.iter().any(|report| report.updated) {
            let changed_files = config_import_batch_changed_files(paths, had_gitignore, &reports);
            auto_commit
                .commit(paths, "config-import-all", &changed_files, None, quiet)
                .map_err(CliError::operation)?;
        }
    }

    annotate_import_conflicts(&mut reports);
    let updated_count = reports.iter().filter(|report| report.updated).count();
    let report = ConfigImportBatchReport {
        dry_run: args.dry_run,
        target,
        detected_count,
        imported_count: reports.len(),
        updated_count,
        reports,
    };
    print_config_import_batch_report(output, paths, &report)
}

fn print_config_import_list_report(
    output: OutputFormat,
    paths: &VaultPaths,
    report: &ConfigImportListReport,
) -> Result<(), CliError> {
    let normalized = ConfigImportListReport {
        importers: report
            .importers
            .iter()
            .map(|item| normalize_import_discovery_item(paths, item))
            .collect(),
    };
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if normalized.importers.is_empty() {
                println!("No importers are registered.");
                return Ok(());
            }

            for item in &normalized.importers {
                let status = if item.detected { "detected" } else { "missing" };
                println!("- {} [{}]", item.plugin, status);
                println!("  {}", item.display_name);
                for source_path in &item.source_paths {
                    println!("  source: {}", source_path.display());
                }
            }
            Ok(())
        }
        OutputFormat::Json => print_json(&normalized),
    }
}

fn print_config_import_batch_report(
    output: OutputFormat,
    paths: &VaultPaths,
    report: &ConfigImportBatchReport,
) -> Result<(), CliError> {
    let normalized = ConfigImportBatchReport {
        dry_run: report.dry_run,
        target: report.target,
        detected_count: report.detected_count,
        imported_count: report.imported_count,
        updated_count: report.updated_count,
        reports: report
            .reports
            .iter()
            .map(|item| normalize_config_import_report(paths, item))
            .collect(),
    };
    let rendered = render_config_import_batch_report(&normalized);
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            println!(
                "{} {} detected importer{} into {} ({} updated)",
                if normalized.dry_run {
                    "Dry run:"
                } else {
                    "Imported"
                },
                normalized.imported_count,
                if normalized.imported_count == 1 {
                    ""
                } else {
                    "s"
                },
                match normalized.target {
                    ImportTarget::Shared => ".vulcan/config.toml",
                    ImportTarget::Local => ".vulcan/config.local.toml",
                },
                normalized.updated_count
            );
            if normalized.imported_count == 0 {
                println!("  no compatible sources were detected");
            }
            for item in &normalized.reports {
                println!(
                    "  - {}: {}",
                    item.plugin,
                    if item.updated {
                        if item.dry_run {
                            "would update"
                        } else {
                            "updated"
                        }
                    } else {
                        "unchanged"
                    }
                );
                for conflict in &item.conflicts {
                    println!(
                        "    warning: conflict on {} from {}",
                        conflict.key,
                        conflict.sources.join(", ")
                    );
                }
                for file in &item.migrated_files {
                    println!(
                        "    view: {} -> {} ({})",
                        file.source.display(),
                        file.target.display(),
                        render_config_import_migrated_file_action(report.dry_run, file.action)
                    );
                }
                for skipped in &item.skipped {
                    println!("    skipped: {} ({})", skipped.source, skipped.reason);
                }
                if let Some(diff) = rendered
                    .reports
                    .iter()
                    .find(|rendered_report| rendered_report.report.plugin == item.plugin)
                    .and_then(|rendered_report| rendered_report.preview_diff.as_deref())
                {
                    println!("    diff:");
                    for line in diff.lines() {
                        println!("      {line}");
                    }
                }
            }
            Ok(())
        }
        OutputFormat::Json => print_json(&rendered),
    }
}

fn config_import_batch_changed_files(
    paths: &VaultPaths,
    had_gitignore: bool,
    reports: &[ConfigImportReport],
) -> Vec<String> {
    let mut changed = reports
        .iter()
        .filter(|report| report.updated)
        .flat_map(|report| config_import_changed_files(paths, had_gitignore, report))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    changed.sort();
    changed
}

fn print_config_import_report(
    output: OutputFormat,
    paths: &VaultPaths,
    report: &ConfigImportReport,
) -> Result<(), CliError> {
    let report = normalize_config_import_report(paths, report);
    let rendered = render_config_import_report(&report);
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            println!(
                "{} {} settings from {} into {} ({}, {})",
                if report.dry_run {
                    "Dry run:"
                } else {
                    "Imported"
                },
                report.plugin,
                if report.source_paths.is_empty() {
                    report.source_path.display().to_string()
                } else if report.source_paths.len() == 1 {
                    report.source_paths[0].display().to_string()
                } else {
                    format!("{} source files", report.source_paths.len())
                },
                report.target_file.display(),
                if report.created_config {
                    if report.dry_run {
                        "would create config"
                    } else {
                        "created config"
                    }
                } else {
                    "existing config"
                },
                if report.updated {
                    if report.dry_run {
                        "would update"
                    } else {
                        "updated"
                    }
                } else {
                    "unchanged"
                }
            );
            if report.source_paths.len() > 1 {
                println!("  sources:");
                for source_path in &report.source_paths {
                    println!("    {}", source_path.display());
                }
            }
            for mapping in &report.mappings {
                println!(
                    "  {} -> {} = {}",
                    mapping.source,
                    mapping.target,
                    render_config_import_value(&mapping.value)?
                );
            }
            for conflict in &report.conflicts {
                println!(
                    "  warning: conflict on {} from {} (kept {})",
                    conflict.key,
                    conflict.sources.join(", "),
                    render_config_import_value(&conflict.kept_value)?
                );
            }
            for file in &report.migrated_files {
                println!(
                    "  view: {} -> {} ({})",
                    file.source.display(),
                    file.target.display(),
                    render_config_import_migrated_file_action(report.dry_run, file.action)
                );
            }
            for skipped in &report.skipped {
                println!("  skipped: {} ({})", skipped.source, skipped.reason);
            }
            if let Some(diff) = rendered.preview_diff.as_deref() {
                println!("  diff:");
                for line in diff.lines() {
                    println!("    {line}");
                }
            }
            Ok(())
        }
        OutputFormat::Json => print_json(&rendered),
    }
}

pub(crate) fn normalize_config_import_report(
    paths: &VaultPaths,
    report: &ConfigImportReport,
) -> ConfigImportReport {
    let mut report = report.clone();
    report.source_path = relativize_config_import_path(paths, &report.source_path);
    report.source_paths = report
        .source_paths
        .iter()
        .map(|path| relativize_config_import_path(paths, path))
        .collect();
    report.config_path = relativize_config_import_path(paths, &report.config_path);
    report.target_file = relativize_config_import_path(paths, &report.target_file);
    report.migrated_files = report
        .migrated_files
        .iter()
        .map(|file| vulcan_core::ImportMigratedFile {
            source: relativize_config_import_path(paths, &file.source),
            target: relativize_config_import_path(paths, &file.target),
            action: file.action,
        })
        .collect();
    report
}

fn render_config_import_report(report: &ConfigImportReport) -> ConfigImportRenderedReport {
    ConfigImportRenderedReport {
        report: report.clone(),
        preview_diff: config_import_preview_diff(report),
    }
}

fn render_config_import_batch_report(
    report: &ConfigImportBatchReport,
) -> ConfigImportRenderedBatchReport {
    ConfigImportRenderedBatchReport {
        dry_run: report.dry_run,
        target: report.target,
        detected_count: report.detected_count,
        imported_count: report.imported_count,
        updated_count: report.updated_count,
        reports: report
            .reports
            .iter()
            .map(render_config_import_report)
            .collect(),
    }
}

fn config_import_preview_diff(report: &ConfigImportReport) -> Option<String> {
    if !report.dry_run {
        return None;
    }
    let after = report.rendered_contents.as_deref()?;
    let before = report.previous_contents.as_deref().unwrap_or("");
    if before == after {
        return None;
    }
    Some(render_unified_diff(
        before,
        after,
        &format!("a/{}", report.target_file.display()),
        &format!("b/{}", report.target_file.display()),
    ))
}

fn render_unified_diff(before: &str, after: &str, before_label: &str, after_label: &str) -> String {
    let before_lines = before.lines().collect::<Vec<_>>();
    let after_lines = after.lines().collect::<Vec<_>>();
    let operations = diff_lines(&before_lines, &after_lines);
    let mut rendered = format!("--- {before_label}\n+++ {after_label}\n");
    for (prefix, line) in operations {
        rendered.push(prefix);
        rendered.push_str(line);
        rendered.push('\n');
    }
    rendered
}

fn diff_lines<'a>(before: &[&'a str], after: &[&'a str]) -> Vec<(char, &'a str)> {
    let mut lcs = vec![vec![0usize; after.len() + 1]; before.len() + 1];
    for before_index in (0..before.len()).rev() {
        for after_index in (0..after.len()).rev() {
            lcs[before_index][after_index] = if before[before_index] == after[after_index] {
                lcs[before_index + 1][after_index + 1] + 1
            } else {
                lcs[before_index + 1][after_index].max(lcs[before_index][after_index + 1])
            };
        }
    }

    let mut before_index = 0;
    let mut after_index = 0;
    let mut operations = Vec::new();
    while before_index < before.len() && after_index < after.len() {
        if before[before_index] == after[after_index] {
            operations.push((' ', before[before_index]));
            before_index += 1;
            after_index += 1;
        } else if lcs[before_index + 1][after_index] >= lcs[before_index][after_index + 1] {
            operations.push(('-', before[before_index]));
            before_index += 1;
        } else {
            operations.push(('+', after[after_index]));
            after_index += 1;
        }
    }
    while before_index < before.len() {
        operations.push(('-', before[before_index]));
        before_index += 1;
    }
    while after_index < after.len() {
        operations.push(('+', after[after_index]));
        after_index += 1;
    }
    operations
}

fn relativize_config_import_path(paths: &VaultPaths, path: &Path) -> PathBuf {
    let relative_or_original = path
        .strip_prefix(paths.vault_root())
        .map_or_else(|_| path.to_path_buf(), Path::to_path_buf);
    PathBuf::from(relative_or_original.to_string_lossy().replace('\\', "/"))
}

fn render_config_import_value(value: &Value) -> Result<String, CliError> {
    match value {
        Value::Null => Ok("<unset>".to_string()),
        Value::String(text) => Ok(format!("{text:?}")),
        Value::Bool(value_bool) => Ok(value_bool.to_string()),
        Value::Number(number) => Ok(number.to_string()),
        Value::Array(_) | Value::Object(_) => {
            serde_json::to_string(value).map_err(CliError::operation)
        }
    }
}

fn config_import_changed_files(
    paths: &VaultPaths,
    had_gitignore: bool,
    report: &ConfigImportReport,
) -> Vec<String> {
    let mut changed = Vec::new();
    if report.config_updated {
        changed.push(
            report
                .target_file
                .strip_prefix(paths.vault_root())
                .map_or_else(
                    |_| report.target_file.display().to_string(),
                    |path| path.display().to_string(),
                ),
        );
    }
    changed.extend(
        report
            .migrated_files
            .iter()
            .filter(|file| matches!(file.action, vulcan_core::ImportMigratedFileAction::Copy))
            .map(|file| {
                file.target.strip_prefix(paths.vault_root()).map_or_else(
                    |_| file.target.display().to_string(),
                    |path| path.display().to_string(),
                )
            }),
    );
    if report.config_updated && !had_gitignore && paths.gitignore_file().exists() {
        changed.push(".vulcan/.gitignore".to_string());
    }
    changed.sort();
    changed.dedup();
    changed
}

fn render_config_import_migrated_file_action(
    dry_run: bool,
    action: vulcan_core::ImportMigratedFileAction,
) -> &'static str {
    match (dry_run, action) {
        (true, vulcan_core::ImportMigratedFileAction::Copy) => "would copy and validate",
        (false, vulcan_core::ImportMigratedFileAction::Copy) => "copied and validated",
        (true, vulcan_core::ImportMigratedFileAction::ValidateOnly) => "would validate",
        (false, vulcan_core::ImportMigratedFileAction::ValidateOnly) => "validated",
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
