#![allow(clippy::too_many_arguments)]

use crate::output::ListOutputControls;
use crate::{Cli, CliError, DailyCommand, PeriodicOpenArgs, PeriodicSubcommand};
use vulcan_core::VaultPaths;

pub(crate) fn handle_daily_command(
    cli: &Cli,
    paths: &VaultPaths,
    command: &DailyCommand,
    interactive_note_selection: bool,
    list_controls: &ListOutputControls,
) -> Result<(), CliError> {
    match command {
        DailyCommand::Today { no_edit, no_commit } => {
            let report = crate::run_periodic_open_command(
                paths,
                "daily",
                None,
                *no_edit,
                *no_commit,
                cli.quiet,
                interactive_note_selection,
            )?;
            crate::print_periodic_open_report(cli.output, &report)
        }
        DailyCommand::Show { date } => {
            let report = crate::run_daily_show_command(paths, date.as_deref())?;
            crate::print_daily_show_report(cli.output, &report)
        }
        DailyCommand::List {
            from,
            to,
            week,
            month,
        } => {
            let report = crate::run_daily_list_command(
                paths,
                from.as_deref(),
                to.as_deref(),
                *week,
                *month,
            )?;
            crate::print_daily_list_report(cli.output, &report, list_controls)
        }
        DailyCommand::ExportIcs {
            from,
            to,
            week,
            month,
            path,
            calendar_name,
        } => {
            let report = crate::run_daily_export_ics_command(
                paths,
                from.as_deref(),
                to.as_deref(),
                *week,
                *month,
                path.as_deref(),
                calendar_name.as_deref(),
            )?;
            crate::print_daily_export_ics_report(cli.output, &report)
        }
        DailyCommand::Append {
            text,
            heading,
            date,
            no_commit,
        } => {
            let report = crate::run_daily_append_command(
                paths,
                text,
                heading.as_deref(),
                date.as_deref(),
                *no_commit,
                cli.quiet,
            )?;
            crate::print_daily_append_report(cli.output, &report)
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
    let report = crate::run_periodic_open_command(
        paths,
        "daily",
        None,
        no_edit,
        no_commit,
        cli.quiet,
        interactive_note_selection,
    )?;
    crate::print_periodic_open_report(cli.output, &report)
}

pub(crate) fn handle_weekly_command(
    cli: &Cli,
    paths: &VaultPaths,
    args: &PeriodicOpenArgs,
    interactive_note_selection: bool,
) -> Result<(), CliError> {
    let report = crate::run_periodic_open_command(
        paths,
        "weekly",
        args.date.as_deref(),
        args.no_edit,
        args.no_commit,
        cli.quiet,
        interactive_note_selection,
    )?;
    crate::print_periodic_open_report(cli.output, &report)
}

pub(crate) fn handle_monthly_command(
    cli: &Cli,
    paths: &VaultPaths,
    args: &PeriodicOpenArgs,
    interactive_note_selection: bool,
) -> Result<(), CliError> {
    let report = crate::run_periodic_open_command(
        paths,
        "monthly",
        args.date.as_deref(),
        args.no_edit,
        args.no_commit,
        cli.quiet,
        interactive_note_selection,
    )?;
    crate::print_periodic_open_report(cli.output, &report)
}

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
) -> Result<(), CliError> {
    match command {
        Some(PeriodicSubcommand::List { period_type }) => {
            let report = crate::run_periodic_list_command(paths, period_type.as_deref())?;
            crate::print_periodic_list_report(cli.output, &report, list_controls)
        }
        Some(PeriodicSubcommand::Gaps {
            period_type,
            from,
            to,
        }) => {
            let report = crate::run_periodic_gaps_command(
                paths,
                period_type.as_deref(),
                from.as_deref(),
                to.as_deref(),
            )?;
            crate::print_periodic_gap_report(cli.output, &report, list_controls)
        }
        None => {
            let period_type = period_type.ok_or_else(|| {
                CliError::operation(
                    "`periodic` requires a period type unless `list` or `gaps` is used",
                )
            })?;
            let report = crate::run_periodic_open_command(
                paths,
                period_type,
                date,
                no_edit,
                no_commit,
                cli.quiet,
                interactive_note_selection,
            )?;
            crate::print_periodic_open_report(cli.output, &report)
        }
    }
}
