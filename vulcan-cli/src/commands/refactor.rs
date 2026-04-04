#![allow(clippy::too_many_lines)]

use crate::commit::AutoCommitPolicy;
use crate::output::ListOutputControls;
use crate::resolve::resolve_note_argument;
use crate::{warn_auto_commit_if_needed, Cli, CliError, RefactorCommand, SuggestCommand};
use vulcan_core::{
    bulk_replace, link_mentions, merge_tags, move_note, rename_alias, rename_block_ref,
    rename_heading, rename_property, suggest_duplicates, suggest_mentions, VaultPaths,
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
            warn_auto_commit_if_needed(&auto_commit);
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
            warn_auto_commit_if_needed(&auto_commit);
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
            warn_auto_commit_if_needed(&auto_commit);
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
            warn_auto_commit_if_needed(&auto_commit);
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
            warn_auto_commit_if_needed(&auto_commit);
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
            find,
            replace,
            dry_run,
            no_commit,
        } => {
            let auto_commit = AutoCommitPolicy::for_mutation(paths, *no_commit);
            warn_auto_commit_if_needed(&auto_commit);
            let report = bulk_replace(paths, filters, find, replace, *dry_run)
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
            warn_auto_commit_if_needed(&auto_commit);
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
            warn_auto_commit_if_needed(&auto_commit);
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
