#![allow(clippy::too_many_lines)]

use crate::commit::AutoCommitPolicy;
use crate::output::ListOutputControls;
use crate::{
    warn_auto_commit_if_needed, Cli, CliError, TasksCommand, TasksPomodoroCommand,
    TasksTrackCommand, TasksViewCommand,
};
use vulcan_core::VaultPaths;

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
        crate::process_due_tasknote_auto_archives(
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
            let report = crate::run_tasks_add_command(
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
                    .commit(paths, "tasks add", &report.changed_paths)
                    .map_err(CliError::operation)?;
            }
            crate::print_task_add_report(cli.output, &report)
        }
        TasksCommand::Show { task } => {
            let report = crate::run_tasks_show_command(paths, task)?;
            crate::print_task_show_report(cli.output, &report)
        }
        TasksCommand::Edit { task, no_commit } => {
            let auto_commit = AutoCommitPolicy::for_mutation(paths, *no_commit);
            warn_auto_commit_if_needed(&auto_commit, cli.quiet);
            let report = crate::run_tasks_edit_command(paths, task, cli.output, use_stderr_color, cli.quiet)?;
            auto_commit
                .commit(paths, "tasks edit", std::slice::from_ref(&report.path))
                .map_err(CliError::operation)?;
            crate::print_edit_report(cli.output, &report);
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
            let report = crate::run_tasks_set_command(
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
                    .commit(paths, "tasks set", &report.changed_paths)
                    .map_err(CliError::operation)?;
            }
            crate::print_task_mutation_report(cli.output, &report)
        }
        TasksCommand::Complete {
            task,
            date,
            dry_run,
            no_commit,
        } => {
            let auto_commit = AutoCommitPolicy::for_mutation(paths, *no_commit);
            warn_auto_commit_if_needed(&auto_commit, cli.quiet);
            let report = crate::run_tasks_complete_command(
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
                    .commit(paths, "tasks complete", &report.changed_paths)
                    .map_err(CliError::operation)?;
            }
            crate::print_task_mutation_report(cli.output, &report)
        }
        TasksCommand::Archive {
            task,
            dry_run,
            no_commit,
        } => {
            let auto_commit = AutoCommitPolicy::for_mutation(paths, *no_commit);
            warn_auto_commit_if_needed(&auto_commit, cli.quiet);
            let report = crate::run_tasks_archive_command(
                paths,
                task,
                *dry_run,
                cli.output,
                use_stderr_color,
                cli.quiet,
            )?;
            if !*dry_run {
                auto_commit
                    .commit(paths, "tasks archive", &report.changed_paths)
                    .map_err(CliError::operation)?;
            }
            crate::print_task_mutation_report(cli.output, &report)
        }
        TasksCommand::Convert {
            file,
            line,
            dry_run,
            no_commit,
        } => {
            let auto_commit = AutoCommitPolicy::for_mutation(paths, *no_commit);
            warn_auto_commit_if_needed(&auto_commit, cli.quiet);
            let report = crate::run_tasks_convert_command(
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
                    .commit(paths, "tasks convert", &report.changed_paths)
                    .map_err(CliError::operation)?;
            }
            crate::print_task_convert_report(cli.output, &report)
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
            let report = crate::run_tasks_create_command(
                paths,
                crate::TasksCreateOptions {
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
                    .commit(paths, "tasks create", &report.changed_paths)
                    .map_err(CliError::operation)?;
            }
            crate::print_task_create_report(cli.output, &report)
        }
        TasksCommand::Reschedule {
            task,
            due,
            dry_run,
            no_commit,
        } => {
            let auto_commit = AutoCommitPolicy::for_mutation(paths, *no_commit);
            warn_auto_commit_if_needed(&auto_commit, cli.quiet);
            let report = crate::run_tasks_reschedule_command(
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
                    .commit(paths, "tasks reschedule", &report.changed_paths)
                    .map_err(CliError::operation)?;
            }
            crate::print_task_mutation_report(cli.output, &report)
        }
        TasksCommand::Query { query } => {
            let result = crate::run_tasks_query_command(paths, query)?;
            crate::print_tasks_query_result(cli.output, &result)
        }
        TasksCommand::Eval { file, block } => {
            let report = crate::run_tasks_eval_command(paths, file, *block)?;
            crate::print_tasks_eval_report(cli.output, &report)
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
            let result = crate::run_tasks_list_command(
                paths,
                crate::TasksListOptions {
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
            crate::print_tasks_query_result(cli.output, &result)
        }
        TasksCommand::Next { count, from } => {
            let report = crate::run_tasks_next_command(paths, *count, from.as_deref())?;
            crate::print_tasks_next_report(cli.output, &report)
        }
        TasksCommand::Blocked => {
            let report = crate::run_tasks_blocked_command(paths)?;
            crate::print_tasks_blocked_report(cli.output, &report)
        }
        TasksCommand::Graph => {
            let report = crate::build_tasks_graph_report(paths)?;
            crate::print_tasks_graph_report(cli.output, &report)
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
                let report = crate::run_tasks_track_start_command(
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
                        .commit(paths, "tasks track start", &report.changed_paths)
                        .map_err(CliError::operation)?;
                }
                crate::print_task_track_report(cli.output, &report)
            }
            TasksTrackCommand::Stop {
                task,
                dry_run,
                no_commit,
            } => {
                let auto_commit = AutoCommitPolicy::for_mutation(paths, *no_commit);
                warn_auto_commit_if_needed(&auto_commit, cli.quiet);
                let report = crate::run_tasks_track_stop_command(
                    paths,
                    task.as_deref(),
                    *dry_run,
                    cli.output,
                    use_stderr_color,
                    cli.quiet,
                )?;
                if !*dry_run {
                    auto_commit
                        .commit(paths, "tasks track stop", &report.changed_paths)
                        .map_err(CliError::operation)?;
                }
                crate::print_task_track_report(cli.output, &report)
            }
            TasksTrackCommand::Status => {
                let report = crate::run_tasks_track_status_command(paths)?;
                crate::print_task_track_status_report(cli.output, &report)
            }
            TasksTrackCommand::Log { task } => {
                let report = crate::run_tasks_track_log_command(paths, task)?;
                crate::print_task_track_log_report(cli.output, &report)
            }
            TasksTrackCommand::Summary { period } => {
                let report = crate::run_tasks_track_summary_command(paths, *period)?;
                crate::print_task_track_summary_report(cli.output, &report)
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
                let report = crate::run_tasks_pomodoro_start_command(
                    paths,
                    task,
                    *dry_run,
                    cli.output,
                    use_stderr_color,
                    cli.quiet,
                )?;
                if !*dry_run {
                    auto_commit
                        .commit(paths, "tasks pomodoro start", &report.changed_paths)
                        .map_err(CliError::operation)?;
                }
                crate::print_task_pomodoro_report(cli.output, &report)
            }
            TasksPomodoroCommand::Stop {
                task,
                dry_run,
                no_commit,
            } => {
                let auto_commit = AutoCommitPolicy::for_mutation(paths, *no_commit);
                warn_auto_commit_if_needed(&auto_commit, cli.quiet);
                let report = crate::run_tasks_pomodoro_stop_command(
                    paths,
                    task.as_deref(),
                    *dry_run,
                    cli.output,
                    use_stderr_color,
                    cli.quiet,
                )?;
                if !*dry_run {
                    auto_commit
                        .commit(paths, "tasks pomodoro stop", &report.changed_paths)
                        .map_err(CliError::operation)?;
                }
                crate::print_task_pomodoro_report(cli.output, &report)
            }
            TasksPomodoroCommand::Status => {
                let report =
                    crate::run_tasks_pomodoro_status_command(paths, cli.output, use_stderr_color, cli.quiet)?;
                crate::print_task_pomodoro_status_report(cli.output, &report)
            }
        },
        TasksCommand::Reminders { upcoming } => {
            let report = crate::run_tasks_reminders_command(paths, upcoming)?;
            crate::print_task_reminders_report(cli.output, &report)
        }
        TasksCommand::Due { within } => {
            let report = crate::run_tasks_due_command(paths, within)?;
            crate::print_task_due_report(cli.output, &report)
        }
        TasksCommand::View { command } => match command {
            TasksViewCommand::Show { name, export } => {
                let report = crate::run_tasks_view_command(paths, name)?;
                let export = crate::resolve_cli_export(export)?;
                crate::print_bases_report(
                    cli.output,
                    &report,
                    list_controls,
                    stdout_is_tty,
                    use_stdout_color,
                    export.as_ref(),
                )
            }
            TasksViewCommand::List => {
                let report = crate::run_tasks_view_list_command(paths)?;
                crate::print_tasknotes_view_list_report(cli.output, &report)
            }
        },
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
