#![allow(clippy::too_many_lines)]

use crate::commit::AutoCommitPolicy;
use crate::editor::open_in_editor;
use crate::output::{print_json, render_human_value, ListOutputControls};
use crate::{
    print_bases_report, print_edit_report, resolve_cli_export, run_incremental_scan,
    warn_auto_commit_if_needed, Cli, CliError, EditReport, OutputFormat, TasksCommand,
    TasksListSourceArg, TasksPomodoroCommand, TasksTrackCommand, TasksTrackSummaryPeriodArg,
    TasksViewCommand,
};
use serde_json::Value;
use vulcan_app::tasks::{
    apply_task_add, apply_task_archive, apply_task_complete, apply_task_convert, apply_task_create,
    apply_task_pomodoro_start, apply_task_pomodoro_stop, apply_task_reschedule, apply_task_set,
    apply_task_track_start, apply_task_track_stop, build_task_due_report,
    build_task_pomodoro_status_report, build_task_reminders_report, build_task_show_report,
    build_task_track_log_report, build_task_track_status_report, build_task_track_summary_report,
    build_tasks_blocked_report, build_tasks_eval_report, build_tasks_graph_report,
    build_tasks_list_report, build_tasks_next_report, build_tasks_query_result,
    build_tasks_view_list_report, build_tasks_view_report,
    process_due_tasknote_auto_archives as app_process_due_tasknote_auto_archives, TaskAddReport,
    TaskAddRequest as AppTaskAddRequest, TaskArchiveRequest as AppTaskArchiveRequest,
    TaskCompleteRequest as AppTaskCompleteRequest, TaskConvertReport,
    TaskConvertRequest as AppTaskConvertRequest, TaskCreateReport,
    TaskCreateRequest as AppTaskCreateRequest, TaskDueReport, TaskEvalRequest, TaskListRequest,
    TaskMutationReport, TaskNotesViewListReport, TaskPomodoroReport,
    TaskPomodoroStartRequest as AppTaskPomodoroStartRequest, TaskPomodoroStatusReport,
    TaskPomodoroStopRequest as AppTaskPomodoroStopRequest, TaskRemindersReport,
    TaskRescheduleRequest as AppTaskRescheduleRequest, TaskSetRequest as AppTaskSetRequest,
    TaskShowReport, TaskTrackLogReport, TaskTrackReport,
    TaskTrackStartRequest as AppTaskTrackStartRequest, TaskTrackStatusReport,
    TaskTrackStopRequest as AppTaskTrackStopRequest,
    TaskTrackSummaryPeriod as AppTaskTrackSummaryPeriod, TaskTrackSummaryReport,
    TasksBlockedReport, TasksEvalReport, TasksGraphReport, TasksNextReport,
};
use vulcan_core::config::TasksDefaultSource;
use vulcan_core::{BasesEvalReport, TasksQueryResult, VaultPaths};

#[allow(clippy::too_many_arguments)]
pub(crate) fn handle_tasks_command(
    cli: &Cli,
    paths: &VaultPaths,
    command: &TasksCommand,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_stdout_color: bool,
    use_stderr_color: bool,
) -> Result<(), CliError> {
    if should_process_tasknotes_auto_archive(command) {
        process_due_tasknote_auto_archives(
            paths,
            auto_archive_excluded_task(command),
            cli.output,
            use_stderr_color,
            cli.quiet,
        )?;
    }

    match command {
        TasksCommand::Add {
            text,
            no_nlp,
            status,
            priority,
            due,
            scheduled,
            contexts,
            projects,
            tags,
            template,
            dry_run,
            no_commit,
        } => {
            let auto_commit = AutoCommitPolicy::for_mutation(paths, *no_commit);
            warn_auto_commit_if_needed(&auto_commit, cli.quiet);
            let report = run_tasks_add_command(
                paths,
                text,
                *no_nlp,
                status.as_deref(),
                priority.as_deref(),
                due.as_deref(),
                scheduled.as_deref(),
                contexts,
                projects,
                tags,
                template.as_deref(),
                *dry_run,
                cli.output,
                use_stderr_color,
                cli.quiet,
            )?;
            if !*dry_run {
                auto_commit
                    .commit(
                        paths,
                        "tasks add",
                        &report.changed_paths,
                        cli.permissions.as_deref(),
                        cli.quiet,
                    )
                    .map_err(CliError::operation)?;
            }
            print_task_add_report(cli.output, &report)
        }
        TasksCommand::Show { task } => {
            let report = run_tasks_show_command(paths, task)?;
            print_task_show_report(cli.output, &report)
        }
        TasksCommand::Edit { task, no_commit } => {
            let auto_commit = AutoCommitPolicy::for_mutation(paths, *no_commit);
            warn_auto_commit_if_needed(&auto_commit, cli.quiet);
            let report =
                run_tasks_edit_command(paths, task, cli.output, use_stderr_color, cli.quiet)?;
            auto_commit
                .commit(
                    paths,
                    "tasks edit",
                    std::slice::from_ref(&report.path),
                    cli.permissions.as_deref(),
                    cli.quiet,
                )
                .map_err(CliError::operation)?;
            print_edit_report(cli.output, &report);
            Ok(())
        }
        TasksCommand::Set {
            task,
            property,
            value,
            dry_run,
            no_commit,
        } => {
            let auto_commit = AutoCommitPolicy::for_mutation(paths, *no_commit);
            warn_auto_commit_if_needed(&auto_commit, cli.quiet);
            let report = run_tasks_set_command(
                paths,
                task,
                property,
                value,
                *dry_run,
                cli.output,
                use_stderr_color,
                cli.quiet,
            )?;
            if !*dry_run {
                auto_commit
                    .commit(
                        paths,
                        "tasks set",
                        &report.changed_paths,
                        cli.permissions.as_deref(),
                        cli.quiet,
                    )
                    .map_err(CliError::operation)?;
            }
            print_task_mutation_report(cli.output, &report)
        }
        TasksCommand::Complete {
            task,
            date,
            dry_run,
            no_commit,
        } => {
            let auto_commit = AutoCommitPolicy::for_mutation(paths, *no_commit);
            warn_auto_commit_if_needed(&auto_commit, cli.quiet);
            let report = run_tasks_complete_command(
                paths,
                task,
                date.as_deref(),
                *dry_run,
                cli.output,
                use_stderr_color,
                cli.quiet,
            )?;
            if !*dry_run {
                auto_commit
                    .commit(
                        paths,
                        "tasks complete",
                        &report.changed_paths,
                        cli.permissions.as_deref(),
                        cli.quiet,
                    )
                    .map_err(CliError::operation)?;
            }
            print_task_mutation_report(cli.output, &report)
        }
        TasksCommand::Archive {
            task,
            dry_run,
            no_commit,
        } => {
            let auto_commit = AutoCommitPolicy::for_mutation(paths, *no_commit);
            warn_auto_commit_if_needed(&auto_commit, cli.quiet);
            let report = run_tasks_archive_command(
                paths,
                task,
                *dry_run,
                cli.output,
                use_stderr_color,
                cli.quiet,
            )?;
            if !*dry_run {
                auto_commit
                    .commit(
                        paths,
                        "tasks archive",
                        &report.changed_paths,
                        cli.permissions.as_deref(),
                        cli.quiet,
                    )
                    .map_err(CliError::operation)?;
            }
            print_task_mutation_report(cli.output, &report)
        }
        TasksCommand::Convert {
            file,
            line,
            dry_run,
            no_commit,
        } => {
            let auto_commit = AutoCommitPolicy::for_mutation(paths, *no_commit);
            warn_auto_commit_if_needed(&auto_commit, cli.quiet);
            let report = run_tasks_convert_command(
                paths,
                file,
                *line,
                *dry_run,
                cli.output,
                use_stderr_color,
                cli.quiet,
            )?;
            if !*dry_run {
                auto_commit
                    .commit(
                        paths,
                        "tasks convert",
                        &report.changed_paths,
                        cli.permissions.as_deref(),
                        cli.quiet,
                    )
                    .map_err(CliError::operation)?;
            }
            print_task_convert_report(cli.output, &report)
        }
        TasksCommand::Create {
            text,
            note,
            due,
            priority,
            dry_run,
            no_commit,
        } => {
            let auto_commit = AutoCommitPolicy::for_mutation(paths, *no_commit);
            warn_auto_commit_if_needed(&auto_commit, cli.quiet);
            let report = run_tasks_create_command(
                paths,
                TasksCreateOptions {
                    text,
                    note: note.as_deref(),
                    due: due.as_deref(),
                    priority: priority.as_deref(),
                    dry_run: *dry_run,
                },
                cli.output,
                use_stderr_color,
                cli.quiet,
            )?;
            if !*dry_run {
                auto_commit
                    .commit(
                        paths,
                        "tasks create",
                        &report.changed_paths,
                        cli.permissions.as_deref(),
                        cli.quiet,
                    )
                    .map_err(CliError::operation)?;
            }
            print_task_create_report(cli.output, &report)
        }
        TasksCommand::Reschedule {
            task,
            due,
            dry_run,
            no_commit,
        } => {
            let auto_commit = AutoCommitPolicy::for_mutation(paths, *no_commit);
            warn_auto_commit_if_needed(&auto_commit, cli.quiet);
            let report = run_tasks_reschedule_command(
                paths,
                task,
                due,
                *dry_run,
                cli.output,
                use_stderr_color,
                cli.quiet,
            )?;
            if !*dry_run {
                auto_commit
                    .commit(
                        paths,
                        "tasks reschedule",
                        &report.changed_paths,
                        cli.permissions.as_deref(),
                        cli.quiet,
                    )
                    .map_err(CliError::operation)?;
            }
            print_task_mutation_report(cli.output, &report)
        }
        TasksCommand::Query { query } => {
            let result = run_tasks_query_command(paths, query)?;
            print_tasks_query_result(cli.output, &result)
        }
        TasksCommand::Eval { file, block } => {
            let report = run_tasks_eval_command(paths, file, *block)?;
            print_tasks_eval_report(cli.output, &report)
        }
        TasksCommand::List {
            filter,
            source,
            status,
            priority,
            due_before,
            due_after,
            project,
            context,
            group_by,
            sort_by,
            include_archived,
        } => {
            let result = run_tasks_list_command(
                paths,
                TasksListOptions {
                    filter: filter.as_deref(),
                    source: *source,
                    status: status.as_deref(),
                    priority: priority.as_deref(),
                    due_before: due_before.as_deref(),
                    due_after: due_after.as_deref(),
                    project: project.as_deref(),
                    context: context.as_deref(),
                    group_by: group_by.as_deref(),
                    sort_by: sort_by.as_deref(),
                    include_archived: *include_archived,
                },
            )?;
            print_tasks_query_result(cli.output, &result)
        }
        TasksCommand::Next { count, from } => {
            let report = run_tasks_next_command(paths, *count, from.as_deref())?;
            print_tasks_next_report(cli.output, &report)
        }
        TasksCommand::Blocked => {
            let report = run_tasks_blocked_command(paths)?;
            print_tasks_blocked_report(cli.output, &report)
        }
        TasksCommand::Graph => {
            let report = run_tasks_graph_command(paths)?;
            print_tasks_graph_report(cli.output, &report)
        }
        TasksCommand::Track { command } => match command {
            TasksTrackCommand::Start {
                task,
                description,
                dry_run,
                no_commit,
            } => {
                let auto_commit = AutoCommitPolicy::for_mutation(paths, *no_commit);
                warn_auto_commit_if_needed(&auto_commit, cli.quiet);
                let report = run_tasks_track_start_command(
                    paths,
                    task,
                    description.as_deref(),
                    *dry_run,
                    cli.output,
                    use_stderr_color,
                    cli.quiet,
                )?;
                if !*dry_run {
                    auto_commit
                        .commit(
                            paths,
                            "tasks track start",
                            &report.changed_paths,
                            cli.permissions.as_deref(),
                            cli.quiet,
                        )
                        .map_err(CliError::operation)?;
                }
                print_task_track_report(cli.output, &report)
            }
            TasksTrackCommand::Stop {
                task,
                dry_run,
                no_commit,
            } => {
                let auto_commit = AutoCommitPolicy::for_mutation(paths, *no_commit);
                warn_auto_commit_if_needed(&auto_commit, cli.quiet);
                let report = run_tasks_track_stop_command(
                    paths,
                    task.as_deref(),
                    *dry_run,
                    cli.output,
                    use_stderr_color,
                    cli.quiet,
                )?;
                if !*dry_run {
                    auto_commit
                        .commit(
                            paths,
                            "tasks track stop",
                            &report.changed_paths,
                            cli.permissions.as_deref(),
                            cli.quiet,
                        )
                        .map_err(CliError::operation)?;
                }
                print_task_track_report(cli.output, &report)
            }
            TasksTrackCommand::Status => {
                let report = run_tasks_track_status_command(paths)?;
                print_task_track_status_report(cli.output, &report)
            }
            TasksTrackCommand::Log { task } => {
                let report = run_tasks_track_log_command(paths, task)?;
                print_task_track_log_report(cli.output, &report)
            }
            TasksTrackCommand::Summary { period } => {
                let report = run_tasks_track_summary_command(paths, *period)?;
                print_task_track_summary_report(cli.output, &report)
            }
        },
        TasksCommand::Pomodoro { command } => match command {
            TasksPomodoroCommand::Start {
                task,
                dry_run,
                no_commit,
            } => {
                let auto_commit = AutoCommitPolicy::for_mutation(paths, *no_commit);
                warn_auto_commit_if_needed(&auto_commit, cli.quiet);
                let report = run_tasks_pomodoro_start_command(
                    paths,
                    task,
                    *dry_run,
                    cli.output,
                    use_stderr_color,
                    cli.quiet,
                )?;
                if !*dry_run {
                    auto_commit
                        .commit(
                            paths,
                            "tasks pomodoro start",
                            &report.changed_paths,
                            cli.permissions.as_deref(),
                            cli.quiet,
                        )
                        .map_err(CliError::operation)?;
                }
                print_task_pomodoro_report(cli.output, &report)
            }
            TasksPomodoroCommand::Stop {
                task,
                dry_run,
                no_commit,
            } => {
                let auto_commit = AutoCommitPolicy::for_mutation(paths, *no_commit);
                warn_auto_commit_if_needed(&auto_commit, cli.quiet);
                let report = run_tasks_pomodoro_stop_command(
                    paths,
                    task.as_deref(),
                    *dry_run,
                    cli.output,
                    use_stderr_color,
                    cli.quiet,
                )?;
                if !*dry_run {
                    auto_commit
                        .commit(
                            paths,
                            "tasks pomodoro stop",
                            &report.changed_paths,
                            cli.permissions.as_deref(),
                            cli.quiet,
                        )
                        .map_err(CliError::operation)?;
                }
                print_task_pomodoro_report(cli.output, &report)
            }
            TasksPomodoroCommand::Status => {
                let report = run_tasks_pomodoro_status_command(
                    paths,
                    cli.output,
                    use_stderr_color,
                    cli.quiet,
                )?;
                print_task_pomodoro_status_report(cli.output, &report)
            }
        },
        TasksCommand::Reminders { upcoming } => {
            let report = run_tasks_reminders_command(paths, upcoming)?;
            print_task_reminders_report(cli.output, &report)
        }
        TasksCommand::Due { within } => {
            let report = run_tasks_due_command(paths, within)?;
            print_task_due_report(cli.output, &report)
        }
        TasksCommand::View { command } => match command {
            TasksViewCommand::Show { name, export } => {
                let report = run_tasks_view_command(paths, name)?;
                let export = resolve_cli_export(export)?;
                print_bases_report(
                    cli.output,
                    &report,
                    list_controls,
                    stdout_is_tty,
                    use_stdout_color,
                    export.as_ref(),
                )
            }
            TasksViewCommand::List => {
                let report = run_tasks_view_list_command(paths)?;
                print_tasknotes_view_list_report(cli.output, &report)
            }
        },
    }
}

pub(crate) fn run_tasks_query_command(
    paths: &VaultPaths,
    source: &str,
) -> Result<TasksQueryResult, CliError> {
    build_tasks_query_result(paths, source).map_err(CliError::operation)
}

fn run_tasks_view_list_command(paths: &VaultPaths) -> Result<TaskNotesViewListReport, CliError> {
    build_tasks_view_list_report(paths).map_err(CliError::operation)
}

fn run_tasks_view_command(paths: &VaultPaths, name: &str) -> Result<BasesEvalReport, CliError> {
    build_tasks_view_report(paths, name).map_err(CliError::operation)
}

#[allow(
    clippy::fn_params_excessive_bools,
    clippy::too_many_arguments,
    clippy::too_many_lines
)]
fn run_tasks_add_command(
    paths: &VaultPaths,
    text: &str,
    no_nlp: bool,
    status: Option<&str>,
    priority: Option<&str>,
    due: Option<&str>,
    scheduled: Option<&str>,
    contexts: &[String],
    projects: &[String],
    tags: &[String],
    template: Option<&str>,
    dry_run: bool,
    output: OutputFormat,
    use_stderr_color: bool,
    quiet: bool,
) -> Result<TaskAddReport, CliError> {
    let report = apply_task_add(
        paths,
        &AppTaskAddRequest {
            text: text.to_string(),
            no_nlp,
            status: status.map(ToOwned::to_owned),
            priority: priority.map(ToOwned::to_owned),
            due: due.map(ToOwned::to_owned),
            scheduled: scheduled.map(ToOwned::to_owned),
            contexts: contexts.to_vec(),
            projects: projects.to_vec(),
            tags: tags.to_vec(),
            template: template.map(ToOwned::to_owned),
            dry_run,
        },
    )
    .map_err(CliError::operation)?;
    if !report.dry_run && !report.changed_paths.is_empty() {
        run_incremental_scan(paths, output, use_stderr_color, quiet)?;
    }
    Ok(report)
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct TasksCreateOptions<'a> {
    pub(crate) text: &'a str,
    pub(crate) note: Option<&'a str>,
    pub(crate) due: Option<&'a str>,
    pub(crate) priority: Option<&'a str>,
    pub(crate) dry_run: bool,
}

pub(crate) fn run_tasks_create_command(
    paths: &VaultPaths,
    options: TasksCreateOptions<'_>,
    output: OutputFormat,
    use_stderr_color: bool,
    quiet: bool,
) -> Result<TaskCreateReport, CliError> {
    let TasksCreateOptions {
        text,
        note,
        due,
        priority,
        dry_run,
    } = options;
    let report = apply_task_create(
        paths,
        &AppTaskCreateRequest {
            text: text.to_string(),
            note: note.map(ToOwned::to_owned),
            due: due.map(ToOwned::to_owned),
            priority: priority.map(ToOwned::to_owned),
            dry_run,
        },
    )
    .map_err(CliError::operation)?;
    if !report.dry_run && !report.changed_paths.is_empty() {
        run_incremental_scan(paths, output, use_stderr_color, quiet)?;
    }
    Ok(report)
}

pub(crate) fn run_tasks_reschedule_command(
    paths: &VaultPaths,
    task: &str,
    due: &str,
    dry_run: bool,
    output: OutputFormat,
    use_stderr_color: bool,
    quiet: bool,
) -> Result<TaskMutationReport, CliError> {
    let report = apply_task_reschedule(
        paths,
        &AppTaskRescheduleRequest {
            task: task.to_string(),
            due: due.to_string(),
            dry_run,
        },
    )
    .map_err(CliError::operation)?;
    if !report.dry_run && !report.changed_paths.is_empty() {
        run_incremental_scan(paths, output, use_stderr_color, quiet)?;
    }
    Ok(report)
}

fn run_tasks_convert_command(
    paths: &VaultPaths,
    file: &str,
    line: Option<i64>,
    dry_run: bool,
    output: OutputFormat,
    use_stderr_color: bool,
    quiet: bool,
) -> Result<TaskConvertReport, CliError> {
    let report = apply_task_convert(
        paths,
        &AppTaskConvertRequest {
            file: file.to_string(),
            line,
            dry_run,
        },
    )
    .map_err(CliError::operation)?;
    if !report.dry_run && !report.changed_paths.is_empty() {
        run_incremental_scan(paths, output, use_stderr_color, quiet)?;
    }
    Ok(report)
}

fn run_tasks_show_command(paths: &VaultPaths, task: &str) -> Result<TaskShowReport, CliError> {
    build_task_show_report(paths, task).map_err(CliError::operation)
}

fn run_tasks_track_start_command(
    paths: &VaultPaths,
    task: &str,
    description: Option<&str>,
    dry_run: bool,
    output: OutputFormat,
    use_stderr_color: bool,
    quiet: bool,
) -> Result<TaskTrackReport, CliError> {
    let report = apply_task_track_start(
        paths,
        &AppTaskTrackStartRequest {
            task: task.to_string(),
            description: description.map(ToOwned::to_owned),
            dry_run,
        },
    )
    .map_err(CliError::operation)?;
    if !report.dry_run && !report.changed_paths.is_empty() {
        run_incremental_scan(paths, output, use_stderr_color, quiet)?;
    }
    Ok(report)
}

fn run_tasks_track_stop_command(
    paths: &VaultPaths,
    task: Option<&str>,
    dry_run: bool,
    output: OutputFormat,
    use_stderr_color: bool,
    quiet: bool,
) -> Result<TaskTrackReport, CliError> {
    let report = apply_task_track_stop(
        paths,
        &AppTaskTrackStopRequest {
            task: task.map(ToOwned::to_owned),
            dry_run,
        },
    )
    .map_err(CliError::operation)?;
    if !report.dry_run && !report.changed_paths.is_empty() {
        run_incremental_scan(paths, output, use_stderr_color, quiet)?;
    }
    Ok(report)
}

fn run_tasks_track_status_command(paths: &VaultPaths) -> Result<TaskTrackStatusReport, CliError> {
    build_task_track_status_report(paths).map_err(CliError::operation)
}

fn run_tasks_track_log_command(
    paths: &VaultPaths,
    task: &str,
) -> Result<TaskTrackLogReport, CliError> {
    build_task_track_log_report(paths, task).map_err(CliError::operation)
}

fn run_tasks_track_summary_command(
    paths: &VaultPaths,
    period: TasksTrackSummaryPeriodArg,
) -> Result<TaskTrackSummaryReport, CliError> {
    let period = match period {
        TasksTrackSummaryPeriodArg::Day => AppTaskTrackSummaryPeriod::Day,
        TasksTrackSummaryPeriodArg::Week => AppTaskTrackSummaryPeriod::Week,
        TasksTrackSummaryPeriodArg::Month => AppTaskTrackSummaryPeriod::Month,
        TasksTrackSummaryPeriodArg::All => AppTaskTrackSummaryPeriod::All,
    };
    build_task_track_summary_report(paths, period).map_err(CliError::operation)
}

fn run_tasks_pomodoro_start_command(
    paths: &VaultPaths,
    task: &str,
    dry_run: bool,
    output: OutputFormat,
    use_stderr_color: bool,
    quiet: bool,
) -> Result<TaskPomodoroReport, CliError> {
    let report = apply_task_pomodoro_start(
        paths,
        &AppTaskPomodoroStartRequest {
            task: task.to_string(),
            dry_run,
        },
    )
    .map_err(CliError::operation)?;
    if !report.dry_run && !report.changed_paths.is_empty() {
        run_incremental_scan(paths, output, use_stderr_color, quiet)?;
    }
    Ok(report)
}

fn run_tasks_pomodoro_stop_command(
    paths: &VaultPaths,
    task: Option<&str>,
    dry_run: bool,
    output: OutputFormat,
    use_stderr_color: bool,
    quiet: bool,
) -> Result<TaskPomodoroReport, CliError> {
    let report = apply_task_pomodoro_stop(
        paths,
        &AppTaskPomodoroStopRequest {
            task: task.map(ToOwned::to_owned),
            dry_run,
        },
    )
    .map_err(CliError::operation)?;
    if !report.dry_run && !report.changed_paths.is_empty() {
        run_incremental_scan(paths, output, use_stderr_color, quiet)?;
    }
    Ok(report)
}

fn run_tasks_pomodoro_status_command(
    paths: &VaultPaths,
    output: OutputFormat,
    use_stderr_color: bool,
    quiet: bool,
) -> Result<TaskPomodoroStatusReport, CliError> {
    let report = build_task_pomodoro_status_report(paths).map_err(CliError::operation)?;
    if !report.changed_paths.is_empty() {
        run_incremental_scan(paths, output, use_stderr_color, quiet)?;
    }
    Ok(report)
}

fn run_tasks_due_command(paths: &VaultPaths, within: &str) -> Result<TaskDueReport, CliError> {
    build_task_due_report(paths, within).map_err(CliError::operation)
}

fn run_tasks_reminders_command(
    paths: &VaultPaths,
    upcoming: &str,
) -> Result<TaskRemindersReport, CliError> {
    build_task_reminders_report(paths, upcoming).map_err(CliError::operation)
}

fn run_tasks_edit_command(
    paths: &VaultPaths,
    task: &str,
    output: OutputFormat,
    use_stderr_color: bool,
    quiet: bool,
) -> Result<EditReport, CliError> {
    let report = build_task_show_report(paths, task).map_err(CliError::operation)?;
    let absolute_path = paths.vault_root().join(&report.path);
    open_in_editor(&absolute_path).map_err(CliError::operation)?;
    run_incremental_scan(paths, output, use_stderr_color, quiet)?;

    Ok(EditReport {
        path: report.path,
        created: false,
        rescanned: true,
    })
}

#[allow(clippy::too_many_arguments)]
fn run_tasks_set_command(
    paths: &VaultPaths,
    task: &str,
    property: &str,
    value: &str,
    dry_run: bool,
    output: OutputFormat,
    use_stderr_color: bool,
    quiet: bool,
) -> Result<TaskMutationReport, CliError> {
    let report = apply_task_set(
        paths,
        &AppTaskSetRequest {
            task: task.to_string(),
            property: property.to_string(),
            value: value.to_string(),
            dry_run,
        },
    )
    .map_err(CliError::operation)?;
    if !report.dry_run && !report.changed_paths.is_empty() {
        run_incremental_scan(paths, output, use_stderr_color, quiet)?;
    }
    Ok(report)
}

#[allow(clippy::too_many_lines)]
pub(crate) fn run_tasks_complete_command(
    paths: &VaultPaths,
    task: &str,
    date: Option<&str>,
    dry_run: bool,
    output: OutputFormat,
    use_stderr_color: bool,
    quiet: bool,
) -> Result<TaskMutationReport, CliError> {
    let report = apply_task_complete(
        paths,
        &AppTaskCompleteRequest {
            task: task.to_string(),
            date: date.map(ToOwned::to_owned),
            dry_run,
        },
    )
    .map_err(CliError::operation)?;
    if !report.dry_run && !report.changed_paths.is_empty() {
        run_incremental_scan(paths, output, use_stderr_color, quiet)?;
    }
    Ok(report)
}

pub(crate) fn process_due_tasknote_auto_archives(
    paths: &VaultPaths,
    exclude_task: Option<&str>,
    output: OutputFormat,
    use_stderr_color: bool,
    quiet: bool,
) -> Result<Vec<String>, CliError> {
    let changed_paths =
        app_process_due_tasknote_auto_archives(paths, exclude_task).map_err(CliError::operation)?;
    if !changed_paths.is_empty() {
        run_incremental_scan(paths, output, use_stderr_color, quiet)?;
    }
    Ok(changed_paths)
}

fn run_tasks_archive_command(
    paths: &VaultPaths,
    task: &str,
    dry_run: bool,
    output: OutputFormat,
    use_stderr_color: bool,
    quiet: bool,
) -> Result<TaskMutationReport, CliError> {
    let report = apply_task_archive(
        paths,
        &AppTaskArchiveRequest {
            task: task.to_string(),
            dry_run,
        },
    )
    .map_err(CliError::operation)?;
    if !report.dry_run && !report.changed_paths.is_empty() {
        run_incremental_scan(paths, output, use_stderr_color, quiet)?;
    }
    Ok(report)
}

fn run_tasks_eval_command(
    paths: &VaultPaths,
    file: &str,
    block: Option<usize>,
) -> Result<TasksEvalReport, CliError> {
    build_tasks_eval_report(
        paths,
        &TaskEvalRequest {
            file: file.to_string(),
            block,
        },
    )
    .map_err(CliError::operation)
}

pub(crate) fn run_tasks_list_command(
    paths: &VaultPaths,
    options: TasksListOptions<'_>,
) -> Result<TasksQueryResult, CliError> {
    build_tasks_list_report(
        paths,
        &TaskListRequest {
            filter: options.filter.map(ToOwned::to_owned),
            source: options.source.map(Into::into),
            status: options.status.map(ToOwned::to_owned),
            priority: options.priority.map(ToOwned::to_owned),
            due_before: options.due_before.map(ToOwned::to_owned),
            due_after: options.due_after.map(ToOwned::to_owned),
            project: options.project.map(ToOwned::to_owned),
            context: options.context.map(ToOwned::to_owned),
            group_by: options.group_by.map(ToOwned::to_owned),
            sort_by: options.sort_by.map(ToOwned::to_owned),
            include_archived: options.include_archived,
        },
    )
    .map_err(CliError::operation)
}

fn run_tasks_next_command(
    paths: &VaultPaths,
    count: usize,
    from: Option<&str>,
) -> Result<TasksNextReport, CliError> {
    build_tasks_next_report(paths, count, from).map_err(CliError::operation)
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct TasksListOptions<'a> {
    pub(crate) filter: Option<&'a str>,
    pub(crate) source: Option<TasksListSourceArg>,
    pub(crate) status: Option<&'a str>,
    pub(crate) priority: Option<&'a str>,
    pub(crate) due_before: Option<&'a str>,
    pub(crate) due_after: Option<&'a str>,
    pub(crate) project: Option<&'a str>,
    pub(crate) context: Option<&'a str>,
    pub(crate) group_by: Option<&'a str>,
    pub(crate) sort_by: Option<&'a str>,
    pub(crate) include_archived: bool,
}

impl From<TasksListSourceArg> for TasksDefaultSource {
    fn from(value: TasksListSourceArg) -> Self {
        match value {
            TasksListSourceArg::Tasknotes => Self::Tasknotes,
            TasksListSourceArg::Inline => Self::Inline,
            TasksListSourceArg::All => Self::All,
        }
    }
}

fn run_tasks_blocked_command(paths: &VaultPaths) -> Result<TasksBlockedReport, CliError> {
    build_tasks_blocked_report(paths).map_err(CliError::operation)
}

fn run_tasks_graph_command(paths: &VaultPaths) -> Result<TasksGraphReport, CliError> {
    build_tasks_graph_report(paths).map_err(CliError::operation)
}

fn print_tasks_query_result(
    output: OutputFormat,
    result: &TasksQueryResult,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            print_tasks_query_result_human(result)?;
            Ok(())
        }
        OutputFormat::Json => print_json(result),
    }
}

fn print_task_show_report(output: OutputFormat, report: &TaskShowReport) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            println!("{}", report.path);
            println!("Title: {}", report.title);
            println!(
                "Status: {} ({}){}",
                report.status,
                report.status_type,
                if report.archived { ", archived" } else { "" }
            );
            println!("Priority: {}", report.priority);
            if let Some(due) = &report.due {
                println!("Due: {due}");
            }
            if let Some(scheduled) = &report.scheduled {
                println!("Scheduled: {scheduled}");
            }
            if let Some(completed_date) = &report.completed_date {
                println!("Completed: {completed_date}");
            }
            if !report.contexts.is_empty() {
                println!("Contexts: {}", report.contexts.join(", "));
            }
            if !report.projects.is_empty() {
                println!("Projects: {}", report.projects.join(", "));
            }
            if !report.tags.is_empty() {
                println!("Tags: {}", report.tags.join(", "));
            }
            if report.total_time_minutes > 0 || report.active_time_minutes > 0 {
                println!("Tracked: {}m", report.total_time_minutes);
            }
            if report.active_time_minutes > 0 {
                println!("Active session: {}m", report.active_time_minutes);
            }
            if let Some(estimate_remaining_minutes) = report.estimate_remaining_minutes {
                println!("Estimate remaining: {estimate_remaining_minutes}m");
            }
            if let Some(efficiency_ratio) = report.efficiency_ratio {
                println!("Efficiency ratio: {efficiency_ratio}%");
            }
            if !report.body.trim().is_empty() {
                println!();
                println!("{}", report.body.trim_end());
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_task_track_report(output: OutputFormat, report: &TaskTrackReport) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            let suffix = if report.dry_run { " (dry-run)" } else { "" };
            println!("{}{}", report.path, suffix);
            println!("Action: {}", report.action);
            println!("Title: {}", report.title);
            println!("Started: {}", report.session.start_time);
            if let Some(end_time) = &report.session.end_time {
                println!("Ended: {end_time}");
            }
            if let Some(description) = &report.session.description {
                println!("Description: {description}");
            }
            println!("Duration: {}m", report.session.duration_minutes);
            println!("Tracked total: {}m", report.total_time_minutes);
            if report.active_time_minutes > 0 {
                println!("Active now: {}m", report.active_time_minutes);
            }
            if let Some(estimate_remaining_minutes) = report.estimate_remaining_minutes {
                println!("Estimate remaining: {estimate_remaining_minutes}m");
            }
            if let Some(efficiency_ratio) = report.efficiency_ratio {
                println!("Efficiency ratio: {efficiency_ratio}%");
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_task_track_status_report(
    output: OutputFormat,
    report: &TaskTrackStatusReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if report.active_sessions.is_empty() {
                println!("No active TaskNotes time tracking sessions.");
                return Ok(());
            }
            for session in &report.active_sessions {
                println!("{}", session.path);
                println!(
                    "- {} [{} / {}] {}m",
                    session.title,
                    session.status,
                    session.priority,
                    session.session.duration_minutes
                );
            }
            println!(
                "{} active session(s), {} total minute(s)",
                report.total_active_sessions, report.total_elapsed_minutes
            );
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_task_track_log_report(
    output: OutputFormat,
    report: &TaskTrackLogReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            println!("{}", report.path);
            println!("Title: {}", report.title);
            println!("Tracked total: {}m", report.total_time_minutes);
            if report.entries.is_empty() {
                println!("No time entries.");
                return Ok(());
            }
            for entry in &report.entries {
                let end_time = entry.end_time.as_deref().unwrap_or("active");
                println!(
                    "- {} -> {} ({}m)",
                    entry.start_time, end_time, entry.duration_minutes
                );
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_task_track_summary_report(
    output: OutputFormat,
    report: &TaskTrackSummaryReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            println!(
                "{} {} -> {}",
                report.period.to_ascii_uppercase(),
                report.from,
                report.to
            );
            println!(
                "Tracked: {}m ({:.1}h) across {} task(s)",
                report.total_minutes, report.total_hours, report.tasks_with_time
            );
            if !report.top_tasks.is_empty() {
                println!("Top tasks:");
                for task in &report.top_tasks {
                    println!("- {} ({}m)", task.path, task.minutes);
                }
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_task_pomodoro_report(
    output: OutputFormat,
    report: &TaskPomodoroReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            let suffix = if report.dry_run { " (dry-run)" } else { "" };
            println!("{}{}", report.storage_note_path, suffix);
            println!("Action: {}", report.action);
            if let Some(task_path) = &report.task_path {
                println!("Task: {task_path}");
            }
            if let Some(title) = &report.title {
                println!("Title: {title}");
            }
            println!("Started: {}", report.session.start_time);
            if let Some(end_time) = &report.session.end_time {
                println!("Ended: {end_time}");
            }
            println!(
                "Planned duration: {}m",
                report.session.planned_duration_minutes
            );
            if report.session.active {
                println!("Remaining: {}s", report.session.remaining_seconds);
            }
            println!(
                "Completed work sessions: {}",
                report.completed_work_sessions
            );
            println!(
                "Suggested break: {} ({}m)",
                report.suggested_break_type, report.suggested_break_minutes
            );
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_task_pomodoro_status_report(
    output: OutputFormat,
    report: &TaskPomodoroStatusReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if let Some(active) = &report.active {
                println!("{}", active.storage_note_path);
                if let Some(task_path) = &active.task_path {
                    println!("Task: {task_path}");
                }
                if let Some(title) = &active.title {
                    println!("Title: {title}");
                }
                println!(
                    "Running: {} ({}s remaining)",
                    active.session.session_type, active.session.remaining_seconds
                );
                println!(
                    "Planned duration: {}m",
                    active.session.planned_duration_minutes
                );
            } else {
                println!("No active TaskNotes pomodoro session.");
            }
            println!(
                "Completed work sessions: {}",
                report.completed_work_sessions
            );
            println!(
                "Suggested break: {} ({}m)",
                report.suggested_break_type, report.suggested_break_minutes
            );
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_task_due_report(output: OutputFormat, report: &TaskDueReport) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if report.tasks.is_empty() {
                println!("No TaskNotes tasks due within {}.", report.within);
                return Ok(());
            }
            for task in &report.tasks {
                let overdue = if task.overdue { " overdue" } else { "" };
                println!("{} {}{}", task.due, task.path, overdue);
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_task_reminders_report(
    output: OutputFormat,
    report: &TaskRemindersReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if report.reminders.is_empty() {
                println!("No TaskNotes reminders due within {}.", report.upcoming);
                return Ok(());
            }
            for reminder in &report.reminders {
                let overdue = if reminder.overdue { " overdue" } else { "" };
                println!(
                    "{} {}#{}{}",
                    reminder.notify_at, reminder.path, reminder.reminder_id, overdue
                );
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_task_add_report(output: OutputFormat, report: &TaskAddReport) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            let suffix = if report.dry_run { " (dry-run)" } else { "" };
            println!("{}{}", report.path, suffix);
            println!("Title: {}", report.title);
            println!("Status: {}", report.status);
            println!("Priority: {}", report.priority);
            if let Some(due) = &report.due {
                println!("Due: {due}");
            }
            if let Some(scheduled) = &report.scheduled {
                println!("Scheduled: {scheduled}");
            }
            if !report.contexts.is_empty() {
                println!("Contexts: {}", report.contexts.join(", "));
            }
            if !report.projects.is_empty() {
                println!("Projects: {}", report.projects.join(", "));
            }
            if !report.tags.is_empty() {
                println!("Tags: {}", report.tags.join(", "));
            }
            if let Some(time_estimate) = report.time_estimate {
                println!("Estimate: {time_estimate}m");
            }
            if let Some(recurrence) = &report.recurrence {
                println!("Recurrence: {recurrence}");
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_task_create_report(
    output: OutputFormat,
    report: &TaskCreateReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            let suffix = if report.dry_run { " (dry-run)" } else { "" };
            println!("{}{}", report.task, suffix);
            if report.created_note {
                println!("Created note: {}", report.path);
            }
            println!("{}", report.line);
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_task_convert_report(
    output: OutputFormat,
    report: &TaskConvertReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            let suffix = if report.dry_run { " (dry-run)" } else { "" };
            if report.source_path == report.target_path {
                println!("{}{}", report.target_path, suffix);
            } else {
                println!("{} -> {}{}", report.source_path, report.target_path, suffix);
            }
            println!("Mode: {}", report.mode);
            println!("Title: {}", report.title);
            if let Some(line_number) = report.line_number {
                println!("Line: {line_number}");
            }
            if report.source_changes.is_empty() && report.task_changes.is_empty() {
                println!("No changes.");
            } else {
                for change in &report.source_changes {
                    println!("- {} -> {}", change.before, change.after);
                }
                for change in &report.task_changes {
                    println!("- {} -> {}", change.before, change.after);
                }
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_task_mutation_report(
    output: OutputFormat,
    report: &TaskMutationReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            let suffix = if report.dry_run { " (dry-run)" } else { "" };
            println!("{}{}", report.path, suffix);
            if let (Some(from), Some(to)) = (&report.moved_from, &report.moved_to) {
                println!("Moved: {from} -> {to}");
            }
            if report.changes.is_empty() {
                println!("No changes.");
            } else {
                for change in &report.changes {
                    println!("- {} -> {}", change.before, change.after);
                }
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_tasknotes_view_list_report(
    output: OutputFormat,
    report: &TaskNotesViewListReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if report.views.is_empty() {
                println!("No TaskNotes views.");
                return Ok(());
            }

            let mut current_file: Option<&str> = None;
            for view in &report.views {
                if current_file != Some(view.file.as_str()) {
                    if current_file.is_some() {
                        println!();
                    }
                    current_file = Some(view.file.as_str());
                    println!("{}", view.file);
                }
                let name = view.view_name.as_deref().unwrap_or("<unnamed>");
                let support = if view.supported { "" } else { " [deferred]" };
                println!("- {name} ({}){support}", view.view_type);
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_tasks_eval_report(output: OutputFormat, report: &TasksEvalReport) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if report.blocks.is_empty() {
                println!("No Tasks blocks in {}", report.file);
                return Ok(());
            }

            println!("Tasks blocks for {}", report.file);
            for (index, block) in report.blocks.iter().enumerate() {
                if index > 0 {
                    println!();
                }
                println!("Block {} (line {})", block.block_index, block.line_number);
                if let Some(error) = &block.error {
                    println!("error: {error}");
                    continue;
                }
                if let Some(result) = &block.result {
                    print_tasks_query_result_human(result)?;
                }
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_tasks_next_report(output: OutputFormat, report: &TasksNextReport) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if report.occurrences.is_empty() {
                println!("No recurring task instances.");
                return Ok(());
            }

            let mut current_date: Option<&str> = None;
            let mut current_path: Option<&str> = None;
            for occurrence in &report.occurrences {
                if current_date != Some(occurrence.date.as_str()) {
                    if current_date.is_some() {
                        println!();
                    }
                    current_date = Some(occurrence.date.as_str());
                    current_path = None;
                    println!("{}", occurrence.date);
                }

                let path = occurrence
                    .task
                    .get("path")
                    .and_then(Value::as_str)
                    .unwrap_or("<unknown>");
                if current_path != Some(path) {
                    current_path = Some(path);
                    println!("{path}");
                }

                let status = occurrence
                    .task
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or(" ");
                let text = occurrence
                    .task
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                println!("- [{status}] {text}");
            }

            println!("{} occurrence(s)", report.result_count);
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_tasks_blocked_report(
    output: OutputFormat,
    report: &TasksBlockedReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if report.tasks.is_empty() {
                println!("No blocked tasks.");
                return Ok(());
            }

            for blocked in &report.tasks {
                let status = blocked
                    .task
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or(" ");
                let path = blocked
                    .task
                    .get("path")
                    .and_then(Value::as_str)
                    .unwrap_or("<unknown>");
                let text = blocked
                    .task
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                println!("{path}");
                println!("- [{status}] {text}");
                for blocker in &blocked.blockers {
                    let relation = blocker.relation_type.as_deref().unwrap_or("FINISHTOSTART");
                    let gap = blocker
                        .gap
                        .as_deref()
                        .map(|value| format!(", gap {value}"))
                        .unwrap_or_default();
                    if blocker.resolved {
                        println!(
                            "  blocked by {} ({}, line {}) [{relation}{gap}; {}]",
                            blocker.blocker_id,
                            blocker.blocker_path.as_deref().unwrap_or("<unknown>"),
                            blocker.blocker_line.unwrap_or_default(),
                            if blocker.blocker_completed == Some(true) {
                                "done"
                            } else {
                                "open"
                            }
                        );
                    } else {
                        println!(
                            "  blocked by {} [{relation}{gap}; unresolved]",
                            blocker.blocker_id
                        );
                    }
                }
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_tasks_graph_report(
    output: OutputFormat,
    report: &TasksGraphReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            println!("Tasks: {}", report.nodes.len());
            println!("Dependencies: {}", report.edges.len());
            if report.edges.is_empty() {
                return Ok(());
            }
            for edge in &report.edges {
                let relation = edge.relation_type.as_deref().unwrap_or("FINISHTOSTART");
                let gap = edge
                    .gap
                    .as_deref()
                    .map(|value| format!(", gap {value}"))
                    .unwrap_or_default();
                if edge.resolved {
                    println!(
                        "- {} -> {} ({}, line {}) [{relation}{gap}]",
                        edge.blocked_key,
                        edge.blocker_id,
                        edge.blocker_path.as_deref().unwrap_or("<unknown>"),
                        edge.blocker_line.unwrap_or_default()
                    );
                } else {
                    println!(
                        "- {} -> {} [{relation}{gap}; unresolved]",
                        edge.blocked_key, edge.blocker_id
                    );
                }
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_tasks_query_result_human(result: &TasksQueryResult) -> Result<(), CliError> {
    if let Some(plan) = &result.plan {
        println!(
            "Plan:\n{}",
            serde_json::to_string_pretty(plan).map_err(CliError::operation)?
        );
        if result.tasks.is_empty() {
            return Ok(());
        }
        println!();
    } else if result.tasks.is_empty() {
        println!("No tasks matched.");
        return Ok(());
    }

    if result.groups.is_empty() {
        print_tasks_by_file_human(&result.tasks);
    } else {
        for (index, group) in result.groups.iter().enumerate() {
            if index > 0 {
                println!();
            }
            println!("{}", render_human_value(&group.key));
            print_tasks_by_file_human(&group.tasks);
        }
    }
    println!("{} task(s)", result.result_count);
    Ok(())
}

fn print_tasks_by_file_human(tasks: &[Value]) {
    let mut current_file: Option<&str> = None;
    for task in tasks {
        let path = task
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or("<unknown>");
        if current_file != Some(path) {
            current_file = Some(path);
            println!("{path}");
        }

        let status = task.get("status").and_then(Value::as_str).unwrap_or(" ");
        let text = task.get("text").and_then(Value::as_str).unwrap_or_default();
        println!("- [{status}] {text}");
    }
}

fn should_process_tasknotes_auto_archive(command: &TasksCommand) -> bool {
    match command {
        TasksCommand::Add { dry_run, .. }
        | TasksCommand::Set { dry_run, .. }
        | TasksCommand::Complete { dry_run, .. }
        | TasksCommand::Archive { dry_run, .. }
        | TasksCommand::Convert { dry_run, .. }
        | TasksCommand::Create { dry_run, .. }
        | TasksCommand::Reschedule { dry_run, .. } => !*dry_run,
        TasksCommand::Track { command } => match command {
            TasksTrackCommand::Start { dry_run, .. } | TasksTrackCommand::Stop { dry_run, .. } => {
                !*dry_run
            }
            TasksTrackCommand::Status
            | TasksTrackCommand::Log { .. }
            | TasksTrackCommand::Summary { .. } => true,
        },
        TasksCommand::Pomodoro { command } => match command {
            TasksPomodoroCommand::Start { dry_run, .. }
            | TasksPomodoroCommand::Stop { dry_run, .. } => !*dry_run,
            TasksPomodoroCommand::Status => true,
        },
        TasksCommand::Show { .. }
        | TasksCommand::Edit { .. }
        | TasksCommand::Query { .. }
        | TasksCommand::Eval { .. }
        | TasksCommand::List { .. }
        | TasksCommand::Next { .. }
        | TasksCommand::Blocked
        | TasksCommand::Graph
        | TasksCommand::Reminders { .. }
        | TasksCommand::Due { .. }
        | TasksCommand::View { .. } => true,
    }
}

fn auto_archive_excluded_task(command: &TasksCommand) -> Option<&str> {
    match command {
        TasksCommand::Show { task }
        | TasksCommand::Edit { task, .. }
        | TasksCommand::Archive { task, .. }
        | TasksCommand::Set { task, .. }
        | TasksCommand::Complete { task, .. }
        | TasksCommand::Reschedule { task, .. } => Some(task.as_str()),
        TasksCommand::Track { command } => match command {
            TasksTrackCommand::Start { task, .. }
            | TasksTrackCommand::Log { task }
            | TasksTrackCommand::Stop {
                task: Some(task), ..
            } => Some(task.as_str()),
            TasksTrackCommand::Status
            | TasksTrackCommand::Summary { .. }
            | TasksTrackCommand::Stop { task: None, .. } => None,
        },
        TasksCommand::Pomodoro { command } => match command {
            TasksPomodoroCommand::Start { task, .. }
            | TasksPomodoroCommand::Stop {
                task: Some(task), ..
            } => Some(task.as_str()),
            TasksPomodoroCommand::Status | TasksPomodoroCommand::Stop { task: None, .. } => None,
        },
        TasksCommand::Add { .. }
        | TasksCommand::Convert { .. }
        | TasksCommand::Create { .. }
        | TasksCommand::Query { .. }
        | TasksCommand::Eval { .. }
        | TasksCommand::List { .. }
        | TasksCommand::Next { .. }
        | TasksCommand::Blocked
        | TasksCommand::Graph
        | TasksCommand::Reminders { .. }
        | TasksCommand::Due { .. }
        | TasksCommand::View { .. } => None,
    }
}
