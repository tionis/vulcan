#![allow(clippy::too_many_lines)]

use crate::commit::AutoCommitPolicy;
use crate::output::ListOutputControls;
use crate::{warn_auto_commit_if_needed, Cli, CliError, KanbanCommand};
use vulcan_core::{list_kanban_boards, load_kanban_board, VaultPaths};

pub(crate) fn handle_kanban_command(
    cli: &Cli,
    paths: &VaultPaths,
    command: &KanbanCommand,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_stdout_color: bool,
) -> Result<(), CliError> {
    match command {
        KanbanCommand::List => {
            let boards = list_kanban_boards(paths).map_err(CliError::operation)?;
            crate::print_kanban_board_list(
                cli.output,
                &boards,
                list_controls,
                stdout_is_tty,
                use_stdout_color,
            )
        }
        KanbanCommand::Show {
            board,
            verbose,
            include_archive,
        } => {
            let report =
                load_kanban_board(paths, board, *include_archive).map_err(CliError::operation)?;
            crate::print_kanban_board_report(cli.output, &report, *verbose)
        }
        KanbanCommand::Cards {
            board,
            column,
            status,
        } => {
            let report = crate::run_kanban_cards_command(
                paths,
                board,
                column.as_deref(),
                status.as_deref(),
            )?;
            crate::print_kanban_cards_report(
                cli.output,
                &report,
                list_controls,
                stdout_is_tty,
                use_stdout_color,
            )
        }
        KanbanCommand::Archive {
            board,
            card,
            dry_run,
            no_commit,
        } => {
            let auto_commit = AutoCommitPolicy::for_mutation(paths, *no_commit);
            warn_auto_commit_if_needed(&auto_commit, cli.quiet);
            let report = crate::run_kanban_archive_command(paths, board, card, *dry_run)?;
            if !*dry_run {
                auto_commit
                    .commit(
                        paths,
                        "kanban-archive",
                        &crate::kanban_archive_changed_files(&report),
                    )
                    .map_err(CliError::operation)?;
            }
            crate::print_kanban_archive_report(cli.output, &report)
        }
        KanbanCommand::Move {
            board,
            card,
            target_column,
            dry_run,
            no_commit,
        } => {
            let auto_commit = AutoCommitPolicy::for_mutation(paths, *no_commit);
            warn_auto_commit_if_needed(&auto_commit, cli.quiet);
            let report =
                crate::run_kanban_move_command(paths, board, card, target_column, *dry_run)?;
            if !*dry_run {
                auto_commit
                    .commit(
                        paths,
                        "kanban-move",
                        &crate::kanban_move_changed_files(&report),
                    )
                    .map_err(CliError::operation)?;
            }
            crate::print_kanban_move_report(cli.output, &report)
        }
        KanbanCommand::Add {
            board,
            column,
            text,
            dry_run,
            no_commit,
        } => {
            let auto_commit = AutoCommitPolicy::for_mutation(paths, *no_commit);
            warn_auto_commit_if_needed(&auto_commit, cli.quiet);
            let report = crate::run_kanban_add_command(paths, board, column, text, *dry_run)?;
            if !*dry_run {
                auto_commit
                    .commit(
                        paths,
                        "kanban-add",
                        &crate::kanban_add_changed_files(&report),
                    )
                    .map_err(CliError::operation)?;
            }
            crate::print_kanban_add_report(cli.output, &report)
        }
    }
}
