#![allow(clippy::too_many_arguments)]

use crate::commit::AutoCommitPolicy;
use crate::editor::open_in_editor;
use crate::output::{
    paginated_items, print_json, print_json_lines, print_selected_human_fields, ListOutputControls,
};
use crate::{
    append_at_end, append_under_heading, print_markdown_output, run_incremental_scan,
    warn_auto_commit_if_needed, Cli, CliError, DailyCommand, OutputFormat, PeriodicOpenArgs,
    PeriodicSubcommand,
};
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use vulcan_app::browse::{build_periodic_list_report, PeriodicListItem};
use vulcan_app::templates::{
    load_named_template, render_loaded_template, LoadedTemplateRenderRequest, TemplateEngineKind,
    TemplateRunMode,
};
use vulcan_core::config::PeriodicConfig;
use vulcan_core::expression::functions::{date_components, parse_date_like_string};
use vulcan_core::{
    expected_periodic_note_path, export_daily_events_to_ics, list_daily_note_events,
    load_events_for_periodic_note, load_vault_config, period_range_for_date, resolve_periodic_note,
    step_period_start, VaultPaths,
};

#[derive(Debug, Clone, PartialEq, Eq)]
struct PeriodicTarget {
    period_type: String,
    reference_date: String,
    start_date: String,
    end_date: String,
    path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct PeriodicEventReport {
    start_time: String,
    end_time: Option<String>,
    title: String,
    metadata: Value,
    tags: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct PeriodicOpenReport {
    period_type: String,
    reference_date: String,
    start_date: String,
    end_date: String,
    path: String,
    created: bool,
    opened_editor: bool,
    warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct PeriodicShowReport {
    period_type: String,
    reference_date: String,
    start_date: String,
    pub(crate) path: String,
    end_date: String,
    content: String,
    events: Vec<PeriodicEventReport>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct DailyListItem {
    period_type: String,
    date: String,
    pub(crate) path: String,
    event_count: usize,
    events: Vec<PeriodicEventReport>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct PeriodicGapItem {
    period_type: String,
    date: String,
    expected_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct DailyAppendReport {
    period_type: String,
    reference_date: String,
    start_date: String,
    end_date: String,
    path: String,
    created: bool,
    heading: Option<String>,
    appended: bool,
    warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct DailyIcsExportReport {
    from: String,
    to: String,
    calendar_name: String,
    note_count: usize,
    event_count: usize,
    path: Option<String>,
    content: String,
}

pub(crate) fn handle_daily_command(
    cli: &Cli,
    paths: &VaultPaths,
    command: &DailyCommand,
    interactive_note_selection: bool,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_stdout_color: bool,
) -> Result<(), CliError> {
    match command {
        DailyCommand::Today { no_edit, no_commit } => {
            let report = run_periodic_open_command(
                paths,
                "daily",
                None,
                *no_edit,
                *no_commit,
                cli.quiet,
                interactive_note_selection,
            )?;
            print_periodic_open_report(cli.output, &report)
        }
        DailyCommand::Show { date } => {
            let report = run_daily_show_command(paths, date.as_deref(), "daily")?;
            print_daily_show_report(cli.output, &report, stdout_is_tty, use_stdout_color)
        }
        DailyCommand::List {
            from,
            to,
            week,
            month,
        } => {
            let report =
                run_daily_list_command(paths, from.as_deref(), to.as_deref(), *week, *month)?;
            print_daily_list_report(cli.output, &report, list_controls)
        }
        DailyCommand::ExportIcs {
            from,
            to,
            week,
            month,
            path,
            calendar_name,
        } => {
            let report = run_daily_export_ics_command(
                paths,
                from.as_deref(),
                to.as_deref(),
                *week,
                *month,
                path.as_deref(),
                calendar_name.as_deref(),
            )?;
            print_daily_export_ics_report(cli.output, &report)
        }
        DailyCommand::Append {
            text,
            heading,
            date,
            no_commit,
        } => {
            let report = run_daily_append_command(
                paths,
                text,
                heading.as_deref(),
                date.as_deref(),
                *no_commit,
                cli.quiet,
                "daily",
            )?;
            print_daily_append_report(cli.output, &report)
        }
    }
}

pub(crate) fn handle_today_command(
    cli: &Cli,
    paths: &VaultPaths,
    no_edit: bool,
    no_commit: bool,
    interactive_note_selection: bool,
) -> Result<(), CliError> {
    let report = run_periodic_open_command(
        paths,
        "daily",
        None,
        no_edit,
        no_commit,
        cli.quiet,
        interactive_note_selection,
    )?;
    print_periodic_open_report(cli.output, &report)
}

pub(crate) fn handle_weekly_command(
    cli: &Cli,
    paths: &VaultPaths,
    args: &PeriodicOpenArgs,
    interactive_note_selection: bool,
) -> Result<(), CliError> {
    let report = run_periodic_open_command(
        paths,
        "weekly",
        args.date.as_deref(),
        args.no_edit,
        args.no_commit,
        cli.quiet,
        interactive_note_selection,
    )?;
    print_periodic_open_report(cli.output, &report)
}

pub(crate) fn handle_monthly_command(
    cli: &Cli,
    paths: &VaultPaths,
    args: &PeriodicOpenArgs,
    interactive_note_selection: bool,
) -> Result<(), CliError> {
    let report = run_periodic_open_command(
        paths,
        "monthly",
        args.date.as_deref(),
        args.no_edit,
        args.no_commit,
        cli.quiet,
        interactive_note_selection,
    )?;
    print_periodic_open_report(cli.output, &report)
}

#[allow(clippy::fn_params_excessive_bools)]
pub(crate) fn handle_periodic_command(
    cli: &Cli,
    paths: &VaultPaths,
    command: Option<&PeriodicSubcommand>,
    period_type: Option<&str>,
    date: Option<&str>,
    no_edit: bool,
    no_commit: bool,
    interactive_note_selection: bool,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_stdout_color: bool,
) -> Result<(), CliError> {
    match command {
        Some(PeriodicSubcommand::List { period_type }) => {
            let report = run_periodic_list_command(paths, period_type.as_deref())?;
            print_periodic_list_report(cli.output, &report, list_controls)
        }
        Some(PeriodicSubcommand::Gaps {
            period_type,
            from,
            to,
        }) => {
            let report = run_periodic_gaps_command(
                paths,
                period_type.as_deref(),
                from.as_deref(),
                to.as_deref(),
            )?;
            print_periodic_gap_report(cli.output, &report, list_controls)
        }
        Some(PeriodicSubcommand::Show { period_type, date }) => {
            let report = run_daily_show_command(paths, date.as_deref(), period_type)?;
            print_daily_show_report(cli.output, &report, stdout_is_tty, use_stdout_color)
        }
        Some(PeriodicSubcommand::Append {
            text,
            period_type,
            heading,
            date,
            no_commit,
        }) => {
            let report = run_daily_append_command(
                paths,
                text,
                heading.as_deref(),
                date.as_deref(),
                *no_commit,
                cli.quiet,
                period_type,
            )?;
            print_daily_append_report(cli.output, &report)
        }
        Some(PeriodicSubcommand::Weekly { args }) => {
            handle_weekly_command(cli, paths, args, interactive_note_selection)
        }
        Some(PeriodicSubcommand::Monthly { args }) => {
            handle_monthly_command(cli, paths, args, interactive_note_selection)
        }
        Some(PeriodicSubcommand::ExportIcs {
            period_type,
            from,
            to,
            path,
            calendar_name,
        }) => {
            let report = run_periodic_export_ics_command(
                paths,
                period_type,
                from.as_deref(),
                to.as_deref(),
                path.as_deref(),
                calendar_name.as_deref(),
            )?;
            print_daily_export_ics_report(cli.output, &report)
        }
        None => {
            let period_type = period_type.ok_or_else(|| {
                CliError::operation(
                    "`periodic` requires a period type unless `list` or `gaps` is used",
                )
            })?;
            let report = run_periodic_open_command(
                paths,
                period_type,
                date,
                no_edit,
                no_commit,
                cli.quiet,
                interactive_note_selection,
            )?;
            print_periodic_open_report(cli.output, &report)
        }
    }
}

pub(crate) fn current_utc_date_string() -> String {
    vulcan_app::templates::TemplateTimestamp::current().default_date_string()
}

fn normalize_date_argument(date: Option<&str>) -> Result<String, CliError> {
    match date
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase)
    {
        None => Ok(current_utc_date_string()),
        Some(value) if value == "today" => Ok(current_utc_date_string()),
        Some(value) => {
            let timestamp = parse_date_like_string(&value)
                .ok_or_else(|| CliError::operation(format!("invalid date: {value}")))?;
            let (year, month, day, _, _, _, _) = date_components(timestamp);
            Ok(format!("{year:04}-{month:02}-{day:02}"))
        }
    }
}

fn resolve_periodic_target(
    config: &PeriodicConfig,
    period_type: &str,
    date: Option<&str>,
    require_enabled: bool,
) -> Result<PeriodicTarget, CliError> {
    let note = config
        .note(period_type)
        .ok_or_else(|| CliError::operation(format!("unknown periodic note type: {period_type}")))?;
    if require_enabled && !note.enabled {
        return Err(CliError::operation(format!(
            "periodic note type `{period_type}` is disabled in config"
        )));
    }

    let reference_date = normalize_date_argument(date)?;
    let (start_date, end_date) = period_range_for_date(config, period_type, &reference_date)
        .ok_or_else(|| {
            CliError::operation(format!(
                "failed to resolve period range for `{period_type}` and {reference_date}"
            ))
        })?;
    let path =
        expected_periodic_note_path(config, period_type, &reference_date).ok_or_else(|| {
            CliError::operation(format!(
                "failed to resolve note path for `{period_type}` and {reference_date}"
            ))
        })?;

    Ok(PeriodicTarget {
        period_type: period_type.to_string(),
        reference_date,
        start_date,
        end_date,
        path,
    })
}

fn render_periodic_note_contents(
    paths: &VaultPaths,
    period_type: &str,
    relative_path: &str,
    warnings: &mut Vec<String>,
) -> Result<String, CliError> {
    let config = load_vault_config(paths).config;
    let template_name = config
        .periodic
        .note(period_type)
        .and_then(|note| note.template.as_deref());
    let Some(template_name) = template_name else {
        return Ok(String::new());
    };

    let loaded = match load_named_template(paths, &config, template_name) {
        Ok(loaded) => loaded,
        Err(error) => {
            warnings.push(format!(
                "failed to resolve periodic template `{template_name}` for `{period_type}`: {error}"
            ));
            return Ok(String::new());
        }
    };
    let rendered = render_loaded_template(
        paths,
        &config,
        &loaded,
        &LoadedTemplateRenderRequest {
            target_path: relative_path,
            target_contents: None,
            engine: TemplateEngineKind::Auto,
            vars: &HashMap::new(),
            allow_mutations: true,
            run_mode: TemplateRunMode::Create,
        },
    )?;
    warnings.extend(loaded.template.warning);
    warnings.extend(rendered.warnings);
    warnings.extend(rendered.diagnostics);
    Ok(rendered.content)
}

fn write_periodic_note_if_missing(
    paths: &VaultPaths,
    period_type: &str,
    relative_path: &str,
    warnings: &mut Vec<String>,
) -> Result<bool, CliError> {
    let absolute_path = paths.vault_root().join(relative_path);
    if absolute_path.is_file() {
        return Ok(false);
    }
    if absolute_path.exists() {
        return Err(CliError::operation(format!(
            "path exists but is not a note file: {relative_path}"
        )));
    }

    if let Some(parent) = absolute_path.parent() {
        fs::create_dir_all(parent).map_err(CliError::operation)?;
    }
    let contents = render_periodic_note_contents(paths, period_type, relative_path, warnings)?;
    fs::write(&absolute_path, contents).map_err(CliError::operation)?;
    Ok(true)
}

fn commit_periodic_changes_if_needed(
    auto_commit: &AutoCommitPolicy,
    paths: &VaultPaths,
    period_type: &str,
    changed_path: &str,
    quiet: bool,
) -> Result<(), CliError> {
    let changed_file = changed_path.to_string();
    auto_commit
        .commit(
            paths,
            &format!("{period_type}-note"),
            std::slice::from_ref(&changed_file),
            None,
            quiet,
        )
        .map_err(CliError::operation)?;
    Ok(())
}

#[allow(clippy::fn_params_excessive_bools)]
fn run_periodic_open_command(
    paths: &VaultPaths,
    period_type: &str,
    date: Option<&str>,
    no_edit: bool,
    no_commit: bool,
    quiet: bool,
    allow_editor: bool,
) -> Result<PeriodicOpenReport, CliError> {
    let auto_commit = AutoCommitPolicy::for_mutation(paths, no_commit);
    warn_auto_commit_if_needed(&auto_commit, quiet);

    let config = load_vault_config(paths).config;
    let target = resolve_periodic_target(&config.periodic, period_type, date, true)?;
    let mut warnings = Vec::new();
    let created = write_periodic_note_if_missing(paths, period_type, &target.path, &mut warnings)?;
    let absolute_path = paths.vault_root().join(&target.path);
    let opened_editor = !no_edit && allow_editor;

    if opened_editor {
        open_in_editor(&absolute_path).map_err(CliError::operation)?;
    }

    if created || opened_editor {
        run_incremental_scan(paths, OutputFormat::Human, false, false)?;
        commit_periodic_changes_if_needed(&auto_commit, paths, period_type, &target.path, quiet)?;
    }

    Ok(PeriodicOpenReport {
        period_type: target.period_type,
        reference_date: target.reference_date,
        start_date: target.start_date,
        end_date: target.end_date,
        path: target.path,
        created,
        opened_editor,
        warnings,
    })
}

fn load_daily_events_for_path(
    paths: &VaultPaths,
    relative_path: &str,
) -> Result<Vec<PeriodicEventReport>, CliError> {
    load_events_for_periodic_note(paths, relative_path)
        .map(|events| {
            events
                .into_iter()
                .map(|event| PeriodicEventReport {
                    start_time: event.start_time,
                    end_time: event.end_time,
                    title: event.title,
                    metadata: event.metadata,
                    tags: event.tags,
                })
                .collect()
        })
        .map_err(CliError::operation)
}

pub(crate) fn run_daily_show_command(
    paths: &VaultPaths,
    date: Option<&str>,
    period_type: &str,
) -> Result<PeriodicShowReport, CliError> {
    let config = load_vault_config(paths).config;
    let target = resolve_periodic_target(&config.periodic, period_type, date, false)?;
    let resolved = resolve_periodic_note(
        paths.vault_root(),
        &config.periodic,
        period_type,
        &target.reference_date,
    )
    .unwrap_or_else(|| target.path.clone());
    let absolute_path = paths.vault_root().join(&resolved);
    if !absolute_path.is_file() {
        return Err(CliError::operation(format!(
            "{period_type} note does not exist on disk: {}",
            target.path
        )));
    }

    let events = if period_type == "daily" {
        load_daily_events_for_path(paths, &resolved)?
    } else {
        Vec::new()
    };

    Ok(PeriodicShowReport {
        period_type: period_type.to_string(),
        reference_date: target.reference_date,
        start_date: target.start_date,
        end_date: target.end_date,
        path: resolved.clone(),
        content: fs::read_to_string(&absolute_path).map_err(CliError::operation)?,
        events,
    })
}

fn resolve_daily_list_window(
    config: &PeriodicConfig,
    from: Option<&str>,
    to: Option<&str>,
    week: bool,
    month: bool,
) -> Result<(String, String), CliError> {
    let today = current_utc_date_string();
    if week {
        return period_range_for_date(config, "weekly", &today)
            .ok_or_else(|| CliError::operation("failed to resolve weekly date range"));
    }
    if month {
        return period_range_for_date(config, "monthly", &today)
            .ok_or_else(|| CliError::operation("failed to resolve monthly date range"));
    }

    let start = normalize_date_argument(from)?;
    let end = match to {
        Some(value) => normalize_date_argument(Some(value))?,
        None if from.is_some() => start.clone(),
        None => today,
    };
    if start > end {
        return Err(CliError::operation(format!(
            "start date must be before or equal to end date: {start} > {end}"
        )));
    }
    Ok((start, end))
}

pub(crate) fn run_daily_list_command(
    paths: &VaultPaths,
    from: Option<&str>,
    to: Option<&str>,
    week: bool,
    month: bool,
) -> Result<Vec<DailyListItem>, CliError> {
    let config = load_vault_config(paths).config;
    let (start, end) = resolve_daily_list_window(&config.periodic, from, to, week, month)?;
    list_daily_note_events(paths, &start, &end)
        .map(|items| {
            items
                .into_iter()
                .map(|item| {
                    let events = item
                        .events
                        .into_iter()
                        .map(|event| PeriodicEventReport {
                            start_time: event.start_time,
                            end_time: event.end_time,
                            title: event.title,
                            metadata: event.metadata,
                            tags: event.tags,
                        })
                        .collect::<Vec<_>>();
                    DailyListItem {
                        period_type: "daily".to_string(),
                        date: item.date,
                        path: item.path,
                        event_count: events.len(),
                        events,
                    }
                })
                .collect()
        })
        .map_err(CliError::operation)
}

fn run_periodic_export_ics_command(
    paths: &VaultPaths,
    _period_type: &str,
    from: Option<&str>,
    to: Option<&str>,
    path: Option<&Path>,
    calendar_name: Option<&str>,
) -> Result<DailyIcsExportReport, CliError> {
    run_daily_export_ics_command(paths, from, to, false, false, path, calendar_name)
}

fn run_daily_export_ics_command(
    paths: &VaultPaths,
    from: Option<&str>,
    to: Option<&str>,
    week: bool,
    month: bool,
    path: Option<&Path>,
    calendar_name: Option<&str>,
) -> Result<DailyIcsExportReport, CliError> {
    let config = load_vault_config(paths).config;
    let (start, end) = resolve_daily_list_window(&config.periodic, from, to, week, month)?;
    let export = export_daily_events_to_ics(paths, &start, &end, calendar_name)
        .map_err(CliError::operation)?;

    let written_path = path.map(|path| path.to_string_lossy().into_owned());
    if let Some(path) = path {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).map_err(CliError::operation)?;
        }
        fs::write(path, &export.content).map_err(CliError::operation)?;
    }

    Ok(DailyIcsExportReport {
        from: start,
        to: end,
        calendar_name: export.calendar_name,
        note_count: export.note_count,
        event_count: export.event_count,
        path: written_path,
        content: export.content,
    })
}

fn run_daily_append_command(
    paths: &VaultPaths,
    text: &str,
    heading: Option<&str>,
    date: Option<&str>,
    no_commit: bool,
    quiet: bool,
    period_type: &str,
) -> Result<DailyAppendReport, CliError> {
    let auto_commit = AutoCommitPolicy::for_mutation(paths, no_commit);
    warn_auto_commit_if_needed(&auto_commit, quiet);

    let config = load_vault_config(paths).config;
    let target = resolve_periodic_target(&config.periodic, period_type, date, true)?;
    let mut warnings = Vec::new();
    let created = write_periodic_note_if_missing(paths, period_type, &target.path, &mut warnings)?;
    let absolute_path = paths.vault_root().join(&target.path);
    let existing = fs::read_to_string(&absolute_path).unwrap_or_default();
    let updated = heading.map_or_else(
        || append_at_end(&existing, text),
        |heading| append_under_heading(&existing, heading, text),
    );
    fs::write(&absolute_path, updated).map_err(CliError::operation)?;

    run_incremental_scan(paths, OutputFormat::Human, false, false)?;
    commit_periodic_changes_if_needed(&auto_commit, paths, period_type, &target.path, quiet)?;

    Ok(DailyAppendReport {
        period_type: target.period_type,
        reference_date: target.reference_date,
        start_date: target.start_date,
        end_date: target.end_date,
        path: target.path,
        created,
        heading: heading.map(ToOwned::to_owned),
        appended: true,
        warnings,
    })
}

fn validate_periodic_type(config: &PeriodicConfig, period_type: &str) -> Result<(), CliError> {
    if config.note(period_type).is_none() {
        return Err(CliError::operation(format!(
            "unknown periodic note type: {period_type}"
        )));
    }
    Ok(())
}

fn run_periodic_list_command(
    paths: &VaultPaths,
    period_type: Option<&str>,
) -> Result<Vec<PeriodicListItem>, CliError> {
    build_periodic_list_report(paths, period_type).map_err(CliError::operation)
}

fn resolve_gap_range_for_type(
    config: &PeriodicConfig,
    period_type: &str,
    from: Option<&str>,
    to: Option<&str>,
) -> Result<(String, String), CliError> {
    let today = current_utc_date_string();
    let from_date = match from {
        Some(value) => normalize_date_argument(Some(value))?,
        None if to.is_some() => normalize_date_argument(to)?,
        None => today.clone(),
    };
    let to_date = match to {
        Some(value) => normalize_date_argument(Some(value))?,
        None if from.is_some() => from_date.clone(),
        None => today,
    };
    if from_date > to_date {
        return Err(CliError::operation(format!(
            "start date must be before or equal to end date: {from_date} > {to_date}"
        )));
    }

    let start = period_range_for_date(config, period_type, &from_date)
        .ok_or_else(|| {
            CliError::operation(format!(
                "failed to resolve period range for `{period_type}` and {from_date}"
            ))
        })?
        .0;
    let end = period_range_for_date(config, period_type, &to_date)
        .ok_or_else(|| {
            CliError::operation(format!(
                "failed to resolve period range for `{period_type}` and {to_date}"
            ))
        })?
        .0;

    Ok((start, end))
}

fn run_periodic_gaps_command(
    paths: &VaultPaths,
    period_type: Option<&str>,
    from: Option<&str>,
    to: Option<&str>,
) -> Result<Vec<PeriodicGapItem>, CliError> {
    let config = load_vault_config(paths).config;
    let types = if let Some(period_type) = period_type {
        validate_periodic_type(&config.periodic, period_type)?;
        vec![period_type.to_string()]
    } else {
        config
            .periodic
            .notes
            .iter()
            .filter_map(|(name, note)| note.enabled.then_some(name.clone()))
            .collect::<Vec<_>>()
    };
    if types.is_empty() {
        return Err(CliError::operation(
            "no enabled periodic note types are configured",
        ));
    }

    let mut gaps = Vec::new();
    for period_type in types {
        let (range_start, range_end) =
            resolve_gap_range_for_type(&config.periodic, &period_type, from, to)?;
        let mut current = range_start;
        while current <= range_end {
            if resolve_periodic_note(paths.vault_root(), &config.periodic, &period_type, &current)
                .is_none()
            {
                let expected_path =
                    expected_periodic_note_path(&config.periodic, &period_type, &current)
                        .ok_or_else(|| {
                            CliError::operation(format!(
                        "failed to resolve expected note path for `{period_type}` and {current}"
                    ))
                        })?;
                gaps.push(PeriodicGapItem {
                    period_type: period_type.clone(),
                    date: current.clone(),
                    expected_path,
                });
            }
            current =
                step_period_start(&config.periodic, &period_type, &current).ok_or_else(|| {
                    CliError::operation(format!(
                        "failed to step periodic range for `{period_type}` at {current}"
                    ))
                })?;
        }
    }

    Ok(gaps)
}

fn print_periodic_open_report(
    output: OutputFormat,
    report: &PeriodicOpenReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if report.created {
                println!("Created {}", report.path);
            } else {
                println!("Using {}", report.path);
            }
            println!(
                "{} period: {} to {}",
                report.period_type, report.start_date, report.end_date
            );
            if report.opened_editor {
                println!("Opened in editor.");
            }
            for warning in &report.warnings {
                eprintln!("Warning: {warning}");
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_daily_show_report(
    output: OutputFormat,
    report: &PeriodicShowReport,
    stdout_is_tty: bool,
    use_color: bool,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            print_markdown_output(output, &report.content, stdout_is_tty, use_color)
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_daily_list_report(
    output: OutputFormat,
    items: &[DailyListItem],
    list_controls: &ListOutputControls,
) -> Result<(), CliError> {
    let visible = paginated_items(items, list_controls);
    let rows = visible
        .iter()
        .map(|item| serde_json::to_value(item).expect("daily list row should serialize"))
        .collect::<Vec<_>>();
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if visible.is_empty() {
                println!("No daily notes in range.");
                return Ok(());
            }
            if let Some(fields) = list_controls.fields.as_deref() {
                for row in &rows {
                    print_selected_human_fields(row, fields);
                }
                return Ok(());
            }
            for item in visible {
                println!("{} ({})", item.date, item.path);
                if item.events.is_empty() {
                    println!("- no events");
                    continue;
                }
                for event in &item.events {
                    match &event.end_time {
                        Some(end_time) => {
                            println!("- {}-{} {}", event.start_time, end_time, event.title);
                        }
                        None => println!("- {} {}", event.start_time, event.title),
                    }
                }
            }
            Ok(())
        }
        OutputFormat::Json => print_json_lines(rows, list_controls.fields.as_deref()),
    }
}

fn print_daily_export_ics_report(
    output: OutputFormat,
    report: &DailyIcsExportReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if let Some(path) = report.path.as_deref() {
                println!(
                    "Wrote {} event(s) from {} daily note(s) to {}",
                    report.event_count, report.note_count, path
                );
                println!("Range: {} to {}", report.from, report.to);
                println!("Calendar: {}", report.calendar_name);
                Ok(())
            } else {
                print!("{}", report.content);
                Ok(())
            }
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_daily_append_report(
    output: OutputFormat,
    report: &DailyAppendReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if report.created {
                println!("Created {}", report.path);
            }
            println!("Appended to {}", report.path);
            if let Some(heading) = report.heading.as_deref() {
                println!("Heading: {heading}");
            }
            for warning in &report.warnings {
                eprintln!("Warning: {warning}");
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_periodic_list_report(
    output: OutputFormat,
    items: &[PeriodicListItem],
    list_controls: &ListOutputControls,
) -> Result<(), CliError> {
    let visible = paginated_items(items, list_controls);
    let rows = visible
        .iter()
        .map(|item| serde_json::to_value(item).expect("periodic list row should serialize"))
        .collect::<Vec<_>>();
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if visible.is_empty() {
                println!("No indexed periodic notes.");
                return Ok(());
            }
            if let Some(fields) = list_controls.fields.as_deref() {
                for row in &rows {
                    print_selected_human_fields(row, fields);
                }
                return Ok(());
            }
            let mut current_type: Option<&str> = None;
            for item in visible {
                if current_type != Some(item.period_type.as_str()) {
                    current_type = Some(item.period_type.as_str());
                    println!("{}", item.period_type);
                }
                println!(
                    "- {} {} ({} event(s))",
                    item.date.as_deref().unwrap_or("-"),
                    item.path,
                    item.event_count
                );
            }
            Ok(())
        }
        OutputFormat::Json => print_json_lines(rows, list_controls.fields.as_deref()),
    }
}

fn print_periodic_gap_report(
    output: OutputFormat,
    items: &[PeriodicGapItem],
    list_controls: &ListOutputControls,
) -> Result<(), CliError> {
    let visible = paginated_items(items, list_controls);
    let rows = visible
        .iter()
        .map(|item| serde_json::to_value(item).expect("periodic gap row should serialize"))
        .collect::<Vec<_>>();
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if visible.is_empty() {
                println!("No periodic gaps in range.");
                return Ok(());
            }
            if let Some(fields) = list_controls.fields.as_deref() {
                for row in &rows {
                    print_selected_human_fields(row, fields);
                }
                return Ok(());
            }
            let mut current_type: Option<&str> = None;
            for item in visible {
                if current_type != Some(item.period_type.as_str()) {
                    current_type = Some(item.period_type.as_str());
                    println!("{}", item.period_type);
                }
                println!("- {} -> {}", item.date, item.expected_path);
            }
            Ok(())
        }
        OutputFormat::Json => print_json_lines(rows, list_controls.fields.as_deref()),
    }
}
