#![allow(clippy::too_many_lines)]

use crate::commit::AutoCommitPolicy;
use crate::output::print_json;
use crate::output::ListOutputControls;
use crate::resolve::resolve_note_argument;
use crate::{
    resolve_bulk_note_selection, selected_permission_guard, warn_auto_commit_if_needed,
    BulkNoteSelection, Cli, CliError, FolderNotePlacementArg, OutputFormat, RefactorCommand,
    SuggestCommand, SuggestLinkStatusArg,
};
use vulcan_app::decomposition::{
    split_note, MissingFragmentPolicy, SplitNoteReport, SplitNoteRequest,
};
use vulcan_app::folder_notes::{
    convert_folder_notes, FolderNoteConversionReport, FolderNoteConversionRequest,
};
use vulcan_core::{
    accept_link_suggestion, bulk_replace_on_paths, link_mentions, merge_tags, move_note,
    query_notes_with_filter, reject_link_suggestion, rename_alias, rename_block_ref,
    rename_heading, rename_property, suggest_duplicates, suggest_links, suggest_mentions,
    FolderNotePlacement, FolderNotesConfig, LinkSuggestionStatus, NoteQuery, PermissionGuard,
    PluginEvent, VaultPaths,
};

fn dispatch_refactor_plugin_hooks(
    cli: &Cli,
    paths: &VaultPaths,
    action: &str,
    changed_paths: &[String],
) {
    let _ = crate::plugins::dispatch_plugin_event(
        paths,
        cli.permissions.as_deref(),
        PluginEvent::OnRefactor,
        &serde_json::json!({
            "kind": PluginEvent::OnRefactor,
            "action": action,
            "paths": changed_paths,
        }),
        cli.quiet,
    );
}

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
                let changed_paths = crate::refactor_changed_files(&report);
                auto_commit
                    .commit(
                        paths,
                        "rename-alias",
                        &changed_paths,
                        cli.permissions.as_deref(),
                        cli.quiet,
                    )
                    .map_err(CliError::operation)?;
                dispatch_refactor_plugin_hooks(cli, paths, "rename-alias", &changed_paths);
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
                let changed_paths = crate::refactor_changed_files(&report);
                auto_commit
                    .commit(
                        paths,
                        "rename-heading",
                        &changed_paths,
                        cli.permissions.as_deref(),
                        cli.quiet,
                    )
                    .map_err(CliError::operation)?;
                dispatch_refactor_plugin_hooks(cli, paths, "rename-heading", &changed_paths);
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
                let changed_paths = crate::refactor_changed_files(&report);
                auto_commit
                    .commit(
                        paths,
                        "rename-block-ref",
                        &changed_paths,
                        cli.permissions.as_deref(),
                        cli.quiet,
                    )
                    .map_err(CliError::operation)?;
                dispatch_refactor_plugin_hooks(cli, paths, "rename-block-ref", &changed_paths);
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
                let changed_paths = crate::refactor_changed_files(&report);
                auto_commit
                    .commit(
                        paths,
                        "rename-property",
                        &changed_paths,
                        cli.permissions.as_deref(),
                        cli.quiet,
                    )
                    .map_err(CliError::operation)?;
                dispatch_refactor_plugin_hooks(cli, paths, "rename-property", &changed_paths);
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
                let changed_paths = crate::refactor_changed_files(&report);
                auto_commit
                    .commit(
                        paths,
                        "merge-tags",
                        &changed_paths,
                        cli.permissions.as_deref(),
                        cli.quiet,
                    )
                    .map_err(CliError::operation)?;
                dispatch_refactor_plugin_hooks(cli, paths, "merge-tags", &changed_paths);
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
                let changed_paths = crate::refactor_changed_files(&report);
                auto_commit
                    .commit(
                        paths,
                        "rewrite",
                        &changed_paths,
                        cli.permissions.as_deref(),
                        cli.quiet,
                    )
                    .map_err(CliError::operation)?;
                dispatch_refactor_plugin_hooks(cli, paths, "rewrite", &changed_paths);
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
                let changed_paths = crate::move_changed_files(&summary);
                auto_commit
                    .commit(
                        paths,
                        "move",
                        &changed_paths,
                        cli.permissions.as_deref(),
                        cli.quiet,
                    )
                    .map_err(CliError::operation)?;
                dispatch_refactor_plugin_hooks(cli, paths, "move", &changed_paths);
            }
            crate::print_move_summary(cli.output, &summary)
        }
        RefactorCommand::SplitNote {
            source,
            destination,
            from_level,
            through_level,
            keep_source,
            preserve_missing_fragments,
            no_navigation,
            dry_run,
            no_commit,
        } => {
            let auto_commit = AutoCommitPolicy::for_mutation(paths, *no_commit);
            warn_auto_commit_if_needed(&auto_commit, cli.quiet);
            let guard = selected_permission_guard(cli, paths)?;
            if !guard.refactor_filter().path_permission().is_unrestricted() {
                return Err(CliError::operation(
                    "permission denied: split-note requires unrestricted refactor scope because it may rewrite inbound links",
                ));
            }
            let report = split_note(
                paths,
                &SplitNoteRequest {
                    source: source.clone(),
                    destination: destination.clone(),
                    from_level: *from_level,
                    through_level: through_level.unwrap_or(*from_level),
                    keep_source: *keep_source,
                    missing_fragment_policy: if *preserve_missing_fragments {
                        MissingFragmentPolicy::Preserve
                    } else {
                        MissingFragmentPolicy::Error
                    },
                    navigation: !*no_navigation,
                    dry_run: *dry_run,
                },
            )
            .map_err(CliError::operation)?;
            if !dry_run {
                auto_commit
                    .commit(
                        paths,
                        "split-note",
                        &report.changed_paths,
                        cli.permissions.as_deref(),
                        cli.quiet,
                    )
                    .map_err(CliError::operation)?;
                dispatch_refactor_plugin_hooks(cli, paths, "split-note", &report.changed_paths);
            }
            print_split_note_report(cli.output, &report)
        }
        RefactorCommand::FolderNotes {
            from_placement,
            from_name,
            to_placement,
            to_name,
            dry_run,
            no_commit,
        } => {
            let auto_commit = AutoCommitPolicy::for_mutation(paths, *no_commit);
            warn_auto_commit_if_needed(&auto_commit, cli.quiet);
            let guard = selected_permission_guard(cli, paths)?;
            if !guard.refactor_filter().path_permission().is_unrestricted() {
                return Err(CliError::operation(
                    "permission denied: folder-note conversion requires unrestricted refactor scope under the selected profile",
                ));
            }
            let source = from_placement
                .zip(from_name.as_ref())
                .map(|(placement, name)| FolderNotesConfig {
                    placement: core_folder_note_placement(placement),
                    name: name.clone(),
                });
            let report = convert_folder_notes(
                paths,
                &FolderNoteConversionRequest {
                    source,
                    destination: FolderNotesConfig {
                        placement: core_folder_note_placement(*to_placement),
                        name: to_name.clone(),
                    },
                    dry_run: *dry_run,
                },
            )
            .map_err(CliError::operation)?;
            if !dry_run {
                auto_commit
                    .commit(
                        paths,
                        "folder-notes",
                        &report.changed_paths,
                        cli.permissions.as_deref(),
                        cli.quiet,
                    )
                    .map_err(CliError::operation)?;
                dispatch_refactor_plugin_hooks(cli, paths, "folder-notes", &report.changed_paths);
            }
            print_folder_note_conversion(cli.output, &report)
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
                let changed_paths = crate::refactor_changed_files(&report);
                auto_commit
                    .commit(
                        paths,
                        "link-mentions",
                        &changed_paths,
                        cli.permissions.as_deref(),
                        cli.quiet,
                    )
                    .map_err(CliError::operation)?;
                dispatch_refactor_plugin_hooks(cli, paths, "link-mentions", &changed_paths);
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

fn core_folder_note_placement(value: FolderNotePlacementArg) -> FolderNotePlacement {
    match value {
        FolderNotePlacementArg::Inside => FolderNotePlacement::Inside,
        FolderNotePlacementArg::Outside => FolderNotePlacement::Outside,
    }
}

fn print_folder_note_conversion(
    output: OutputFormat,
    report: &FolderNoteConversionReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Json => print_json(report),
        OutputFormat::Human | OutputFormat::Markdown => {
            let action = if report.dry_run {
                "Would move"
            } else {
                "Moved"
            };
            for entry in &report.moves {
                println!(
                    "{action} {} -> {} ({})",
                    entry.source_path, entry.destination_path, entry.folder
                );
            }
            if report.moves.is_empty() {
                println!("No folder notes needed moving.");
            }
            if report.config_updated {
                println!(
                    "{} folder-note config: {}",
                    if report.dry_run {
                        "Would update"
                    } else {
                        "Updated"
                    },
                    report.config_path.display()
                );
            }
            Ok(())
        }
    }
}

fn print_split_note_report(output: OutputFormat, report: &SplitNoteReport) -> Result<(), CliError> {
    match output {
        OutputFormat::Json => print_json(report),
        OutputFormat::Human | OutputFormat::Markdown => {
            let action = if report.dry_run {
                "Would create"
            } else {
                "Created"
            };
            println!(
                "{} note tree from {} at {} ({} notes)",
                if report.dry_run {
                    "Planned"
                } else {
                    "Materialized"
                },
                report.source_path,
                report.destination_root,
                report.notes.len()
            );
            for note in &report.notes {
                println!("{action} {} ({})", note.path, note.title);
            }
            let rewrite_count = report
                .rewritten_files
                .iter()
                .map(|file| file.changes.len())
                .sum::<usize>();
            if rewrite_count > 0 {
                println!(
                    "{} {rewrite_count} link(s) across {} file(s).",
                    if report.dry_run {
                        "Would rewrite"
                    } else {
                        "Rewrote"
                    },
                    report.rewritten_files.len()
                );
            }
            if report.source_retained {
                println!("Retained source note {}.", report.source_path);
            } else if report.dry_run {
                println!("Would replace source note {}.", report.source_path);
            } else {
                println!("Replaced source note {}.", report.source_path);
            }
            for diagnostic in &report.diagnostics {
                println!("Warning [{}]: {}", diagnostic.code, diagnostic.message);
            }
            Ok(())
        }
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
        SuggestCommand::Links {
            note,
            min_score,
            accept,
            reject,
            status,
            accepted,
            apply,
            dry_run,
            export,
        } => {
            let export = crate::resolve_cli_export(export)?;
            if *accepted && status.is_some() {
                return Err(CliError::operation(
                    "`suggest links` accepts either --accepted or --status, not both",
                ));
            }
            if let Some(id) = accept {
                let suggestion = accept_link_suggestion(paths, id).map_err(CliError::operation)?;
                return crate::print_link_suggestions_report(
                    cli.output,
                    &vulcan_core::LinkSuggestionsReport {
                        suggestions: vec![suggestion],
                    },
                    list_controls,
                    stdout_is_tty,
                    use_stdout_color,
                    export.as_ref(),
                );
            }
            if let Some(id) = reject {
                let suggestion = reject_link_suggestion(paths, id).map_err(CliError::operation)?;
                return crate::print_link_suggestions_report(
                    cli.output,
                    &vulcan_core::LinkSuggestionsReport {
                        suggestions: vec![suggestion],
                    },
                    list_controls,
                    stdout_is_tty,
                    use_stdout_color,
                    export.as_ref(),
                );
            }
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
            let status = if *accepted {
                Some(LinkSuggestionStatus::Accepted)
            } else if *apply {
                Some(LinkSuggestionStatus::Pending)
            } else {
                status.map(|status| match status {
                    SuggestLinkStatusArg::Pending => LinkSuggestionStatus::Pending,
                    SuggestLinkStatusArg::Accepted => LinkSuggestionStatus::Accepted,
                    SuggestLinkStatusArg::Rejected => LinkSuggestionStatus::Rejected,
                })
            };
            let min_score = min_score.parse::<f64>().map_err(|error| {
                CliError::operation(format!("invalid --min-score `{min_score}`: {error}"))
            })?;
            let mut report = suggest_links(
                paths,
                note.as_deref(),
                list_controls.limit,
                min_score,
                status,
            )
            .map_err(CliError::operation)?;
            if *apply && !*dry_run {
                let mut accepted = Vec::new();
                for suggestion in &report.suggestions {
                    accepted.push(
                        accept_link_suggestion(paths, &suggestion.id)
                            .map_err(CliError::operation)?,
                    );
                }
                report = vulcan_core::LinkSuggestionsReport {
                    suggestions: accepted,
                };
            }
            crate::print_link_suggestions_report(
                cli.output,
                &report,
                list_controls,
                stdout_is_tty,
                use_stdout_color,
                export.as_ref(),
            )
        }
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
