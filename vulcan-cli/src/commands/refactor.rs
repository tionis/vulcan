#![allow(clippy::too_many_lines)]

use crate::commit::AutoCommitPolicy;
use crate::output::ListOutputControls;
use crate::resolve::resolve_note_argument;
use crate::{
    resolve_bulk_note_selection, selected_permission_guard, warn_auto_commit_if_needed,
    BulkNoteSelection, Cli, CliError, RefactorCommand, SuggestCommand,
};
use vulcan_core::{
    bulk_replace_on_paths, link_mentions, merge_tags, move_note, query_notes_with_filter,
    rename_alias, rename_block_ref, rename_heading, rename_property, suggest_duplicates,
    suggest_mentions, NoteQuery, PermissionGuard, VaultPaths,
};

pub(crate) fn handle_refactor_command(
    cli: &Cli,
    paths: &VaultPaths,
    command: &RefactorCommand,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_stdout_color: bool,
) -> Result<(), CliError> {
    match command {
        RefactorCommand::RenameAlias {
            note,
            old,
            new,
            dry_run,
            no_commit,
        } => {
            let auto_commit = AutoCommitPolicy::for_mutation(paths, *no_commit);
            warn_auto_commit_if_needed(&auto_commit, cli.quiet);
            selected_permission_guard(cli, paths)?
                .check_refactor_path(note)
                .map_err(CliError::operation)?;
            let report =
                rename_alias(paths, note, old, new, *dry_run).map_err(CliError::operation)?;
            if !dry_run {
                auto_commit
                    .commit(
                        paths,
                        "rename-alias",
                        &crate::refactor_changed_files(&report),
                    )
                    .map_err(CliError::operation)?;
            }
            crate::print_refactor_report(cli.output, &report)
        }
        RefactorCommand::RenameHeading {
            note,
            old,
            new,
            dry_run,
            no_commit,
        } => {
            let auto_commit = AutoCommitPolicy::for_mutation(paths, *no_commit);
            warn_auto_commit_if_needed(&auto_commit, cli.quiet);
            selected_permission_guard(cli, paths)?
                .check_refactor_path(note)
                .map_err(CliError::operation)?;
            let report =
                rename_heading(paths, note, old, new, *dry_run).map_err(CliError::operation)?;
            if !dry_run {
                auto_commit
                    .commit(
                        paths,
                        "rename-heading",
                        &crate::refactor_changed_files(&report),
                    )
                    .map_err(CliError::operation)?;
            }
            crate::print_refactor_report(cli.output, &report)
        }
        RefactorCommand::RenameBlockRef {
            note,
            old,
            new,
            dry_run,
            no_commit,
        } => {
            let auto_commit = AutoCommitPolicy::for_mutation(paths, *no_commit);
            warn_auto_commit_if_needed(&auto_commit, cli.quiet);
            selected_permission_guard(cli, paths)?
                .check_refactor_path(note)
                .map_err(CliError::operation)?;
            let report =
                rename_block_ref(paths, note, old, new, *dry_run).map_err(CliError::operation)?;
            if !dry_run {
                auto_commit
                    .commit(
                        paths,
                        "rename-block-ref",
                        &crate::refactor_changed_files(&report),
                    )
                    .map_err(CliError::operation)?;
            }
            crate::print_refactor_report(cli.output, &report)
        }
        RefactorCommand::RenameProperty {
            old,
            new,
            dry_run,
            no_commit,
        } => {
            let auto_commit = AutoCommitPolicy::for_mutation(paths, *no_commit);
            warn_auto_commit_if_needed(&auto_commit, cli.quiet);
            let guard = selected_permission_guard(cli, paths)?;
            if !guard.refactor_filter().path_permission().is_unrestricted() {
                return Err(CliError::operation(
                    "permission denied: rename-property requires unrestricted refactor scope under the selected profile",
                ));
            }
            let report = rename_property(paths, old, new, *dry_run).map_err(CliError::operation)?;
            if !dry_run {
                auto_commit
                    .commit(
                        paths,
                        "rename-property",
                        &crate::refactor_changed_files(&report),
                    )
                    .map_err(CliError::operation)?;
            }
            crate::print_refactor_report(cli.output, &report)
        }
        RefactorCommand::MergeTags {
            source,
            dest,
            dry_run,
            no_commit,
        } => {
            let auto_commit = AutoCommitPolicy::for_mutation(paths, *no_commit);
            warn_auto_commit_if_needed(&auto_commit, cli.quiet);
            let guard = selected_permission_guard(cli, paths)?;
            if !guard.refactor_filter().path_permission().is_unrestricted() {
                return Err(CliError::operation(
                    "permission denied: merge-tags requires unrestricted refactor scope under the selected profile",
                ));
            }
            let report = merge_tags(paths, source, dest, *dry_run).map_err(CliError::operation)?;
            if !dry_run {
                auto_commit
                    .commit(paths, "merge-tags", &crate::refactor_changed_files(&report))
                    .map_err(CliError::operation)?;
            }
            crate::print_refactor_report(cli.output, &report)
        }
        RefactorCommand::Rewrite {
            filters,
            stdin,
            find,
            replace,
            dry_run,
            no_commit,
        } => {
            let auto_commit = AutoCommitPolicy::for_mutation(paths, *no_commit);
            warn_auto_commit_if_needed(&auto_commit, cli.quiet);
            let guard = selected_permission_guard(cli, paths)?;
            let selection = resolve_bulk_note_selection(filters, *stdin)?;
            let note_paths = match &selection {
                BulkNoteSelection::Filters(filters) => query_notes_with_filter(
                    paths,
                    &NoteQuery {
                        filters: filters.clone(),
                        sort_by: None,
                        sort_descending: false,
                    },
                    Some(&guard.read_filter()),
                )
                .map_err(CliError::operation)?
                .notes
                .into_iter()
                .map(|note| note.document_path)
                .collect::<Vec<_>>(),
                BulkNoteSelection::Paths(note_paths) => note_paths.clone(),
            };
            for path in &note_paths {
                guard
                    .check_refactor_path(path)
                    .map_err(CliError::operation)?;
            }
            let report = bulk_replace_on_paths(paths, &note_paths, find, replace, *dry_run)
                .map_err(CliError::operation)?;
            if !dry_run {
                auto_commit
                    .commit(paths, "rewrite", &crate::refactor_changed_files(&report))
                    .map_err(CliError::operation)?;
            }
            crate::print_refactor_report(cli.output, &report)
        }
        RefactorCommand::Move {
            source,
            dest,
            dry_run,
            no_commit,
        } => {
            let auto_commit = AutoCommitPolicy::for_mutation(paths, *no_commit);
            warn_auto_commit_if_needed(&auto_commit, cli.quiet);
            let guard = selected_permission_guard(cli, paths)?;
            guard
                .check_refactor_path(source)
                .map_err(CliError::operation)?;
            guard
                .check_refactor_path(dest)
                .map_err(CliError::operation)?;
            let summary = move_note(paths, source, dest, *dry_run).map_err(CliError::operation)?;
            if !dry_run {
                auto_commit
                    .commit(paths, "move", &crate::move_changed_files(&summary))
                    .map_err(CliError::operation)?;
            }
            crate::print_move_summary(cli.output, &summary)
        }
        RefactorCommand::LinkMentions {
            note,
            dry_run,
            no_commit,
        } => {
            let auto_commit = AutoCommitPolicy::for_mutation(paths, *no_commit);
            warn_auto_commit_if_needed(&auto_commit, cli.quiet);
            let guard = selected_permission_guard(cli, paths)?;
            if !guard.refactor_filter().path_permission().is_unrestricted() {
                return Err(CliError::operation(
                    "permission denied: link-mentions requires unrestricted refactor scope under the selected profile",
                ));
            }
            let report =
                link_mentions(paths, note.as_deref(), *dry_run).map_err(CliError::operation)?;
            if !dry_run {
                auto_commit
                    .commit(
                        paths,
                        "link-mentions",
                        &crate::refactor_changed_files(&report),
                    )
                    .map_err(CliError::operation)?;
            }
            crate::print_refactor_report(cli.output, &report)
        }
        RefactorCommand::Suggest { command } => handle_suggest_command(
            cli,
            paths,
            command,
            false,
            list_controls,
            stdout_is_tty,
            use_stdout_color,
        ),
    }
}

pub(crate) fn handle_suggest_command(
    cli: &Cli,
    paths: &VaultPaths,
    command: &SuggestCommand,
    interactive_note_selection: bool,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_stdout_color: bool,
) -> Result<(), CliError> {
    match command {
        SuggestCommand::Mentions { note, export } => {
            let note = if note.is_some() || interactive_note_selection {
                Some(resolve_note_argument(
                    paths,
                    note.as_deref(),
                    interactive_note_selection,
                    "note",
                )?)
            } else {
                None
            };
            let report = suggest_mentions(paths, note.as_deref()).map_err(CliError::operation)?;
            let export = crate::resolve_cli_export(export)?;
            crate::print_mention_suggestions_report(
                cli.output,
                &report,
                list_controls,
                stdout_is_tty,
                use_stdout_color,
                export.as_ref(),
            )
        }
        SuggestCommand::Duplicates { export } => {
            let report = suggest_duplicates(paths).map_err(CliError::operation)?;
            let export = crate::resolve_cli_export(export)?;
            crate::print_duplicate_suggestions_report(
                cli.output,
                &report,
                list_controls,
                stdout_is_tty,
                use_stdout_color,
                export.as_ref(),
            )
        }
    }
}
