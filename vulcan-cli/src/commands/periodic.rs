#![allow(clippy::too_many_arguments)]

use crate::commit::AutoCommitPolicy;
use crate::editor::open_in_editor;
use crate::output::{
    paginated_items, print_json, print_json_lines, print_selected_human_fields, ListOutputControls,
};
use crate::{
    append_at_end, append_under_heading, print_markdown_output, run_incremental_scan,
    warn_auto_commit_if_needed, Cli, CliError, DailyCommand, OutputFormat, PeriodicOpenArgs,
    PeriodicSubcommand, PermissionGuard,
};
use serde::Serialize;
use std::fs;
use std::path::Path;
use vulcan_app::browse::{build_periodic_list_report, PeriodicListItem};
use vulcan_app::notes::{read_note_for_update, write_note_content};
use vulcan_app::periodic::{
    current_local_date_string as app_current_local_date_string, list_daily_notes,
    normalize_date_argument as app_normalize_date_argument, read_daily_note,
    resolve_daily_list_window as app_resolve_daily_list_window,
    resolve_periodic_target as app_resolve_periodic_target, show_periodic_note, DailyListItem,
    DailyNoteReadReport, DailyReadTarget, PeriodicShowReport, PeriodicTarget,
};
use vulcan_core::config::PeriodicConfig;
use vulcan_core::{
    expected_periodic_note_path, export_daily_events_to_ics, load_vault_config,
    period_range_for_date, resolve_periodic_note, step_period_start, VaultPaths,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct PeriodicOpenReport {
    period_type: String,
    reference_date: String,
    start_date: String,
    end_date: String,
    path: String,
    created: bool,
    opened_editor: bool,
    dry_run: bool,
    warnings: Vec<String>,
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

#[allow(clippy::too_many_lines)]
pub(crate) fn handle_daily_command(
    cli: &Cli,
    paths: &VaultPaths,
    command: Option<&DailyCommand>,
    interactive_note_selection: bool,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_stdout_color: bool,
) -> Result<(), CliError> {
    let Some(command) = command else {
        return handle_daily_calendar_command(
            cli,
            paths,
            None,
            false,
            interactive_note_selection,
            list_controls,
        );
    };
    match command {
        DailyCommand::Open {
            date,
            no_edit,
            dry_run,
            no_commit,
        } => handle_daily_open_command(
            cli,
            paths,
            date.as_deref(),
            *no_edit,
            *dry_run,
            *no_commit,
            interactive_note_selection,
        ),
        DailyCommand::Calendar { date, no_commit } => handle_daily_calendar_command(
            cli,
            paths,
            date.as_deref(),
            *no_commit,
            interactive_note_selection,
            list_controls,
        ),
        DailyCommand::Latest => {
            let report = run_daily_latest_command(paths, true)?;
            if let Some(path) = report.path.as_deref() {
                crate::selected_permission_guard(cli, paths)?
                    .check_read_path(path)
                    .map_err(CliError::operation)?;
            }
            print_daily_read_report(cli.output, &report, stdout_is_tty, use_stdout_color)
        }
        DailyCommand::Today { no_edit, no_commit } => {
            check_periodic_write_access(cli, paths, "daily", None)?;
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
            check_periodic_write_access(cli, paths, "daily", date.as_deref())?;
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

#[allow(clippy::fn_params_excessive_bools)]
fn handle_daily_open_command(
    cli: &Cli,
    paths: &VaultPaths,
    date: Option<&str>,
    no_edit: bool,
    dry_run: bool,
    no_commit: bool,
    interactive_note_selection: bool,
) -> Result<(), CliError> {
    let report = if dry_run {
        plan_periodic_open(paths, "daily", date)?
    } else {
        check_periodic_write_access(cli, paths, "daily", date)?;
        run_periodic_open_command(
            paths,
            "daily",
            date,
            no_edit,
            no_commit,
            cli.quiet,
            interactive_note_selection,
        )?
    };
    print_periodic_open_report(cli.output, &report)
}

/// Interactive month picker; without a terminal, lists that month's notes.
fn handle_daily_calendar_command(
    cli: &Cli,
    paths: &VaultPaths,
    date: Option<&str>,
    no_commit: bool,
    interactive: bool,
    list_controls: &ListOutputControls,
) -> Result<(), CliError> {
    let today = current_local_date_string();
    let initial = resolve_calendar_initial_date(date, &today)?;
    if !interactive {
        let config = load_vault_config(paths).config;
        let (start, end) = period_range_for_date(&config.periodic, "monthly", &initial)
            .ok_or_else(|| CliError::operation("failed to resolve monthly date range"))?;
        let report = run_daily_list_command(paths, Some(&start), Some(&end), false, false)?;
        return print_daily_list_report(cli.output, &report, list_controls);
    }

    let auto_commit = AutoCommitPolicy::for_mutation(paths, no_commit);
    warn_auto_commit_if_needed(&auto_commit, cli.quiet);
    let mut open_note = |date: &str| -> Result<String, String> {
        check_periodic_write_access(cli, paths, "daily", Some(date))
            .map_err(|error| error.to_string())?;
        let report =
            run_periodic_open_command(paths, "daily", Some(date), false, no_commit, true, true)
                .map_err(|error| error.to_string())?;
        let action = if report.created { "Created" } else { "Edited" };
        Ok(match report.warnings.first() {
            Some(warning) => format!("{action} {} (warning: {warning})", report.path),
            None => format!("{action} {}", report.path),
        })
    };
    crate::daily_tui::run_daily_calendar_tui(paths, &today, &initial, &mut open_note)
        .map_err(CliError::operation)
}

/// `YYYY-MM` selects the first of that month; anything else goes through the
/// shared date resolver.
fn resolve_calendar_initial_date(date: Option<&str>, today: &str) -> Result<String, CliError> {
    let trimmed = date.map(str::trim).unwrap_or_default();
    let is_month = trimmed.len() == 7
        && trimmed.as_bytes()[4] == b'-'
        && trimmed
            .bytes()
            .enumerate()
            .all(|(index, byte)| index == 4 || byte.is_ascii_digit());
    if is_month {
        return Ok(format!("{trimmed}-01"));
    }
    vulcan_app::periodic::normalize_date_argument_at(date, today).map_err(CliError::operation)
}

pub(crate) fn run_daily_latest_command(
    paths: &VaultPaths,
    include_content: bool,
) -> Result<DailyNoteReadReport, CliError> {
    read_daily_note(paths, DailyReadTarget::Latest, include_content).map_err(CliError::operation)
}

fn print_daily_read_report(
    output: OutputFormat,
    report: &DailyNoteReadReport,
    stdout_is_tty: bool,
    use_stdout_color: bool,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Json => print_json(report),
        OutputFormat::Human | OutputFormat::Markdown if report.exists => print_markdown_output(
            output,
            report.content.as_deref().unwrap_or_default(),
            stdout_is_tty,
            use_stdout_color,
        ),
        OutputFormat::Human | OutputFormat::Markdown => {
            println!("No daily notes found.");
            Ok(())
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
    check_periodic_write_access(cli, paths, "daily", None)?;
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
    check_periodic_write_access(cli, paths, "weekly", args.date.as_deref())?;
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
    check_periodic_write_access(cli, paths, "monthly", args.date.as_deref())?;
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
            check_periodic_write_access(cli, paths, period_type, date.as_deref())?;
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
            check_periodic_write_access(cli, paths, period_type, date)?;
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

fn check_periodic_write_access(
    cli: &Cli,
    paths: &VaultPaths,
    period_type: &str,
    date: Option<&str>,
) -> Result<(), CliError> {
    let config = load_vault_config(paths).config;
    let target = resolve_periodic_target(&config.periodic, period_type, date, true)?;
    crate::selected_permission_guard(cli, paths)?
        .check_write_path(&target.path)
        .map_err(CliError::operation)
}

pub(crate) fn current_local_date_string() -> String {
    app_current_local_date_string()
}

pub(crate) fn normalize_date_argument(date: Option<&str>) -> Result<String, CliError> {
    app_normalize_date_argument(date).map_err(CliError::operation)
}

fn resolve_periodic_target(
    config: &PeriodicConfig,
    period_type: &str,
    date: Option<&str>,
    require_enabled: bool,
) -> Result<PeriodicTarget, CliError> {
    app_resolve_periodic_target(config, period_type, date, require_enabled)
        .map_err(CliError::operation)
}

fn write_periodic_note_if_missing(
    paths: &VaultPaths,
    period_type: &str,
    relative_path: &str,
    warnings: &mut Vec<String>,
    quiet: bool,
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
    let contents = vulcan_app::notes::render_periodic_note_contents(
        paths,
        period_type,
        relative_path,
        warnings,
        None,
    )
    .map_err(CliError::operation)?;
    write_note_content(paths, relative_path, None, &contents, "create", None, quiet)
        .map_err(CliError::operation)?;
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
    let created =
        write_periodic_note_if_missing(paths, period_type, &target.path, &mut warnings, quiet)?;
    let absolute_path = paths.vault_root().join(&target.path);
    let opened_editor = !no_edit && allow_editor;

    if opened_editor {
        open_in_editor(&absolute_path).map_err(CliError::operation)?;
    }

    if created || opened_editor {
        run_incremental_scan(paths, OutputFormat::Human, false, quiet)?;
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
        dry_run: false,
        warnings,
    })
}

/// Resolve what `open` would do without touching the vault.
fn plan_periodic_open(
    paths: &VaultPaths,
    period_type: &str,
    date: Option<&str>,
) -> Result<PeriodicOpenReport, CliError> {
    let config = load_vault_config(paths).config;
    let target = resolve_periodic_target(&config.periodic, period_type, date, true)?;
    let created = !paths.vault_root().join(&target.path).is_file();
    Ok(PeriodicOpenReport {
        period_type: target.period_type,
        reference_date: target.reference_date,
        start_date: target.start_date,
        end_date: target.end_date,
        path: target.path,
        created,
        opened_editor: false,
        dry_run: true,
        warnings: Vec::new(),
    })
}

pub(crate) fn run_daily_show_command(
    paths: &VaultPaths,
    date: Option<&str>,
    period_type: &str,
) -> Result<PeriodicShowReport, CliError> {
    show_periodic_note(paths, date, period_type).map_err(CliError::operation)
}

fn resolve_daily_list_window(
    config: &PeriodicConfig,
    from: Option<&str>,
    to: Option<&str>,
    week: bool,
    month: bool,
) -> Result<(String, String), CliError> {
    app_resolve_daily_list_window(config, from, to, week, month).map_err(CliError::operation)
}

pub(crate) fn run_daily_list_command(
    paths: &VaultPaths,
    from: Option<&str>,
    to: Option<&str>,
    week: bool,
    month: bool,
) -> Result<Vec<DailyListItem>, CliError> {
    list_daily_notes(paths, from, to, week, month).map_err(CliError::operation)
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
        vulcan_core::paths::write_file_atomic(path, &export.content)
            .map_err(CliError::operation)?;
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
    let created =
        write_periodic_note_if_missing(paths, period_type, &target.path, &mut warnings, quiet)?;
    let existing = read_note_for_update(paths, &target.path).map_err(CliError::operation)?;
    let current = existing.as_deref().unwrap_or_default();
    let updated = heading.map_or_else(
        || append_at_end(current, text),
        |heading| append_under_heading(current, heading, text),
    );
    write_note_content(
        paths,
        &target.path,
        existing.as_deref(),
        &updated,
        "append",
        None,
        quiet,
    )
    .map_err(CliError::operation)?;

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
    let today = current_local_date_string();
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
            match (report.dry_run, report.created) {
                (true, true) => println!("Would create {}", report.path),
                (true, false) => println!("Would use {}", report.path),
                (false, true) => println!("Created {}", report.path),
                (false, false) => println!("Using {}", report.path),
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

#[cfg(test)]
mod tests {
    use super::resolve_calendar_initial_date;

    #[test]
    fn calendar_initial_date_accepts_months_and_relative_dates() {
        let today = "2026-10-06";
        let resolve = |value: Option<&str>| {
            resolve_calendar_initial_date(value, today).expect("date should resolve")
        };
        assert_eq!(resolve(None), today);
        assert_eq!(resolve(Some("2026-02")), "2026-02-01");
        assert_eq!(resolve(Some("-1m")), "2026-09-06");
        assert_eq!(resolve(Some("2025-12-24")), "2025-12-24");
        assert!(resolve_calendar_initial_date(Some("nope"), today).is_err());
    }
}
