#![allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    clippy::fn_params_excessive_bools
)]

use crate::commit::AutoCommitPolicy;
use crate::output::ListOutputControls;
use crate::resolve::resolve_note_argument;
use crate::{
    warn_auto_commit_if_needed, Cli, CliError, NoteAppendMode, NoteAppendOptions, NoteCommand,
    NoteGetOptions, NotePatchOptions,
};
use vulcan_core::{query_backlinks, query_links, VaultPaths};

pub(crate) fn handle_note_command(
    cli: &Cli,
    paths: &VaultPaths,
    command: &NoteCommand,
    interactive_note_selection: bool,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_stdout_color: bool,
    use_stderr_color: bool,
) -> Result<(), CliError> {
    match command {
        NoteCommand::Get {
            note,
            heading,
            block_ref,
            lines,
            match_pattern,
            context,
            no_frontmatter,
            raw,
        } => {
            let report = crate::run_note_get_command(
                paths,
                NoteGetOptions {
                    note,
                    heading: heading.as_deref(),
                    block_ref: block_ref.as_deref(),
                    lines: lines.as_deref(),
                    match_pattern: match_pattern.as_deref(),
                    context: *context,
                    no_frontmatter: *no_frontmatter,
                    raw: *raw,
                },
            )?;
            crate::print_note_get_report(cli.output, &report)
        }
        NoteCommand::Set {
            note,
            file,
            no_frontmatter,
            check,
            no_commit,
        } => {
            let auto_commit = AutoCommitPolicy::for_mutation(paths, *no_commit);
            warn_auto_commit_if_needed(&auto_commit, cli.quiet);
            let report = crate::run_note_set_command(
                paths,
                note,
                file.as_ref(),
                *no_frontmatter,
                *check,
                cli.output,
                use_stderr_color,
                cli.quiet,
            )?;
            auto_commit
                .commit(paths, "note-set", std::slice::from_ref(&report.path))
                .map_err(CliError::operation)?;
            crate::print_note_set_report(cli.output, &report)
        }
        NoteCommand::Create {
            path,
            template,
            frontmatter,
            check,
            no_commit,
        } => {
            let auto_commit = AutoCommitPolicy::for_mutation(paths, *no_commit);
            warn_auto_commit_if_needed(&auto_commit, cli.quiet);
            let report = crate::run_note_create_command(
                paths,
                path,
                template.as_deref(),
                frontmatter,
                *check,
                cli.output,
                use_stderr_color,
                cli.quiet,
            )?;
            auto_commit
                .commit(paths, "note-create", &report.changed_paths)
                .map_err(CliError::operation)?;
            crate::print_note_create_report(cli.output, &report)
        }
        NoteCommand::Append {
            note_or_text,
            text,
            heading,
            prepend,
            append: _,
            periodic,
            date,
            vars,
            check,
            no_commit,
        } => {
            let auto_commit = AutoCommitPolicy::for_mutation(paths, *no_commit);
            warn_auto_commit_if_needed(&auto_commit, cli.quiet);
            let (note, text) = match (*periodic, text.as_deref()) {
                (Some(_), None) => (None, note_or_text.as_str()),
                (None, Some(text)) => (Some(note_or_text.as_str()), text),
                (Some(_), Some(_)) => {
                    return Err(CliError::operation(format!(
                        "`note append --periodic` accepts only the appended text; got unexpected note argument `{note_or_text}`"
                    )));
                }
                (None, None) => {
                    return Err(CliError::operation(format!(
                        "`note append` requires both NOTE and TEXT; got only `{note_or_text}`"
                    )));
                }
            };
            let report = crate::run_note_append_command(
                paths,
                NoteAppendOptions {
                    note,
                    text,
                    mode: if *prepend {
                        NoteAppendMode::Prepend
                    } else if heading.is_some() {
                        NoteAppendMode::AfterHeading
                    } else {
                        NoteAppendMode::Append
                    },
                    heading: heading.as_deref(),
                    periodic: *periodic,
                    date: date.as_deref(),
                    vars,
                    check: *check,
                },
                cli.output,
                use_stderr_color,
                cli.quiet,
            )?;
            auto_commit
                .commit(paths, "note-append", std::slice::from_ref(&report.path))
                .map_err(CliError::operation)?;
            crate::print_note_append_report(cli.output, &report)
        }
        NoteCommand::Patch {
            note,
            find,
            replace,
            all,
            check,
            dry_run,
            no_commit,
        } => {
            let auto_commit = AutoCommitPolicy::for_mutation(paths, *no_commit);
            warn_auto_commit_if_needed(&auto_commit, cli.quiet);
            let report = crate::run_note_patch_command(
                paths,
                NotePatchOptions {
                    note,
                    find,
                    replace,
                    replace_all: *all,
                    check: *check,
                    dry_run: *dry_run,
                },
                cli.output,
                use_stderr_color,
                cli.quiet,
            )?;
            if !*dry_run {
                auto_commit
                    .commit(paths, "note-patch", std::slice::from_ref(&report.path))
                    .map_err(CliError::operation)?;
            }
            crate::print_note_patch_report(cli.output, &report)
        }
        NoteCommand::Info { note } => {
            let report = crate::run_note_info_command(paths, note)?;
            crate::print_note_info_report(cli.output, &report)
        }
        NoteCommand::History { note, limit } => {
            let report = crate::run_note_history_command(paths, note, *limit)?;
            crate::print_note_history_report(cli.output, &report)
        }
        NoteCommand::Links { note, export } => {
            let note =
                resolve_note_argument(paths, note.as_deref(), interactive_note_selection, "note")?;
            let report = query_links(paths, &note).map_err(CliError::operation)?;
            let export = crate::resolve_cli_export(export)?;
            crate::print_links_report(
                cli.output,
                &report,
                list_controls,
                stdout_is_tty,
                use_stdout_color,
                export.as_ref(),
            )?;
            Ok(())
        }
        NoteCommand::Backlinks { note, export } => {
            let note =
                resolve_note_argument(paths, note.as_deref(), interactive_note_selection, "note")?;
            let report = query_backlinks(paths, &note).map_err(CliError::operation)?;
            let export = crate::resolve_cli_export(export)?;
            crate::print_backlinks_report(
                cli.output,
                &report,
                list_controls,
                stdout_is_tty,
                use_stdout_color,
                export.as_ref(),
            )?;
            Ok(())
        }
        NoteCommand::Doctor { note } => {
            let report = crate::run_note_doctor_command(paths, note)?;
            crate::print_note_doctor_report(cli.output, &report)
        }
        NoteCommand::Diff { note, since } => {
            let report = crate::run_diff_command(paths, Some(note), since.as_deref(), false)?;
            crate::print_diff_report(cli.output, &report)
        }
    }
}
