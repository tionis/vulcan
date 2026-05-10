#![allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    clippy::fn_params_excessive_bools
)]

use crate::commit::AutoCommitPolicy;
use crate::output::{print_json, ListOutputControls};
use crate::resolve::resolve_note_argument;
use crate::{
    markdown_heading_level, print_diagnostic_section, print_markdown_output, run_incremental_scan,
    selected_permission_guard, selected_read_permission_filter, warn_auto_commit_if_needed,
    AnsiPalette, Cli, CliError, NoteAppendMode, NoteAppendPeriodicArg, NoteCheckboxState,
    NoteCommand, NoteGetMode, OutputFormat,
};
use regex::Regex;
use serde::Serialize;
use serde_json::Value;
use std::fs;
use std::io::{self, IsTerminal, Read};
use std::path::{Path, PathBuf};
use vulcan_app::notes::{
    apply_note_append, apply_note_create, apply_note_delete, apply_note_patch, apply_note_set,
    diagnose_external_markdown_contents, diagnose_note_contents, parse_note_frontmatter_bindings,
    MarkdownTarget as AppMarkdownTarget, NoteAppendRequest as AppNoteAppendRequest,
    NoteCreateRequest as AppNoteCreateRequest, NoteDeleteRequest as AppNoteDeleteRequest,
    NotePatchRequest as AppNotePatchRequest, NoteSetRequest as AppNoteSetRequest,
};
use vulcan_app::templates::{
    find_frontmatter_block, parse_template_var_bindings, TemplateTimestamp,
};
use vulcan_core::config::load_vault_config;
use vulcan_core::html::HtmlRenderOptions;
use vulcan_core::paths::{normalize_relative_input_path, RelativePathOptions};
use vulcan_core::{
    git_log, move_note, query_backlinks, query_backlinks_with_filter, query_links,
    query_links_with_filter, render_note_fragment_html, render_note_html, render_vault_html,
    resolve_note_reference, BacklinkRecord, DoctorDiagnosticIssue, GitLogEntry,
    GraphConfidenceBreakdown, GraphQueryError, NoteMatchKind, PermissionGuard, PluginEvent,
    RefactorChange, VaultPaths,
};

fn check_read_note_access(cli: &Cli, paths: &VaultPaths, note: &str) -> Result<(), CliError> {
    let guard = selected_permission_guard(cli, paths)?;
    if guard.read_filter().path_permission().is_unrestricted() && !guard.has_policy_hook() {
        return Ok(());
    }
    let resolved = resolve_note_reference(paths, note).map_err(CliError::operation)?;
    guard
        .check_read_path(&resolved.path)
        .map_err(CliError::operation)
}

fn check_write_note_access(cli: &Cli, paths: &VaultPaths, note: &str) -> Result<(), CliError> {
    let guard = selected_permission_guard(cli, paths)?;
    if guard.write_filter().path_permission().is_unrestricted() && !guard.has_policy_hook() {
        return Ok(());
    }
    let resolved = resolve_note_reference(paths, note).map_err(CliError::operation)?;
    guard
        .check_write_path(&resolved.path)
        .map_err(CliError::operation)
}

fn check_write_path_access(cli: &Cli, paths: &VaultPaths, path: &str) -> Result<(), CliError> {
    let guard = selected_permission_guard(cli, paths)?;
    if guard.write_filter().path_permission().is_unrestricted() && !guard.has_policy_hook() {
        return Ok(());
    }
    guard.check_write_path(path).map_err(CliError::operation)
}

fn check_refactor_note_access(cli: &Cli, paths: &VaultPaths, note: &str) -> Result<(), CliError> {
    let guard = selected_permission_guard(cli, paths)?;
    if guard.refactor_filter().path_permission().is_unrestricted() && !guard.has_policy_hook() {
        return Ok(());
    }
    let resolved = resolve_note_reference(paths, note).map_err(CliError::operation)?;
    guard
        .check_refactor_path(&resolved.path)
        .map_err(CliError::operation)
}

fn check_read_markdown_source_access(
    cli: &Cli,
    paths: &VaultPaths,
    note: &str,
) -> Result<(), CliError> {
    let guard = selected_permission_guard(cli, paths)?;
    if guard.read_filter().path_permission().is_unrestricted() && !guard.has_policy_hook() {
        return Ok(());
    }

    let target = resolve_existing_markdown_target(paths, note)?;
    let Some(relative_path) = target.vault_relative_path.as_deref() else {
        return Err(CliError::operation(format!(
            "permission profiles cannot read markdown files outside the selected vault root: {}",
            target.display_path
        )));
    };
    guard
        .check_read_path(relative_path)
        .map_err(CliError::operation)
}

fn check_write_markdown_source_access(
    cli: &Cli,
    paths: &VaultPaths,
    note: &str,
) -> Result<(), CliError> {
    let guard = selected_permission_guard(cli, paths)?;
    if guard.write_filter().path_permission().is_unrestricted() && !guard.has_policy_hook() {
        return Ok(());
    }

    let target = resolve_existing_markdown_target(paths, note)?;
    let Some(relative_path) = target.vault_relative_path.as_deref() else {
        return Err(CliError::operation(format!(
            "permission profiles cannot write markdown files outside the selected vault root: {}",
            target.display_path
        )));
    };
    guard
        .check_write_path(relative_path)
        .map_err(CliError::operation)
}

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
        NoteCommand::Outline {
            note,
            section_id,
            depth,
        } => {
            check_read_markdown_source_access(cli, paths, note)?;
            let report = run_note_outline_command(paths, note, section_id.as_deref(), *depth)?;
            print_note_outline_report(cli.output, &report, use_stdout_color)
        }
        NoteCommand::Checkbox {
            note,
            section_id,
            heading,
            block_ref,
            lines,
            line,
            index,
            state,
            check,
            dry_run,
            no_commit,
        } => {
            let auto_commit = AutoCommitPolicy::for_mutation(paths, *no_commit);
            warn_auto_commit_if_needed(&auto_commit, cli.quiet);
            check_write_markdown_source_access(cli, paths, note)?;
            let report = run_note_checkbox_command(
                paths,
                NoteCheckboxOptions {
                    note,
                    section_id: section_id.as_deref(),
                    heading: heading.as_deref(),
                    block_ref: block_ref.as_deref(),
                    lines: lines.as_deref(),
                    line: *line,
                    index: *index,
                    state: *state,
                    check: *check,
                    dry_run: *dry_run,
                },
                cli.permissions.as_deref(),
                cli.output,
                use_stderr_color,
                cli.quiet,
            )?;
            if !*dry_run && report.changed {
                auto_commit
                    .commit(
                        paths,
                        "note-checkbox",
                        std::slice::from_ref(&report.path),
                        cli.permissions.as_deref(),
                        cli.quiet,
                    )
                    .map_err(CliError::operation)?;
            }
            print_note_checkbox_report(cli.output, &report)
        }
        NoteCommand::Get {
            note,
            mode,
            section_id,
            heading,
            block_ref,
            lines,
            match_pattern,
            context,
            no_frontmatter,
            raw,
        } => {
            check_read_markdown_source_access(cli, paths, note)?;
            let report = run_note_get_command(
                paths,
                NoteGetOptions {
                    note,
                    mode: *mode,
                    section_id: section_id.as_deref(),
                    heading: heading.as_deref(),
                    block_ref: block_ref.as_deref(),
                    lines: lines.as_deref(),
                    match_pattern: match_pattern.as_deref(),
                    context: *context,
                    no_frontmatter: *no_frontmatter,
                    raw: *raw,
                },
            )?;
            print_note_get_report(cli.output, &report, stdout_is_tty, use_stdout_color)
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
            check_write_note_access(cli, paths, note)?;
            let report = run_note_set_command(
                paths,
                note,
                file.as_ref(),
                *no_frontmatter,
                *check,
                cli.permissions.as_deref(),
                cli.output,
                use_stderr_color,
                cli.quiet,
            )?;
            auto_commit
                .commit(
                    paths,
                    "note-set",
                    std::slice::from_ref(&report.path),
                    cli.permissions.as_deref(),
                    cli.quiet,
                )
                .map_err(CliError::operation)?;
            print_note_set_report(cli.output, &report)
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
            check_write_path_access(cli, paths, path)?;
            let report = run_note_create_command(
                paths,
                path,
                template.as_deref(),
                frontmatter,
                *check,
                cli.permissions.as_deref(),
                cli.output,
                use_stderr_color,
                cli.quiet,
            )?;
            auto_commit
                .commit(
                    paths,
                    "note-create",
                    &report.changed_paths,
                    cli.permissions.as_deref(),
                    cli.quiet,
                )
                .map_err(CliError::operation)?;
            print_note_create_report(cli.output, &report)
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
            if let Some(note) = note {
                check_write_note_access(cli, paths, note)?;
            }
            let report = run_note_append_command(
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
                cli.permissions.as_deref(),
                cli.output,
                use_stderr_color,
                cli.quiet,
            )?;
            auto_commit
                .commit(
                    paths,
                    "note-append",
                    std::slice::from_ref(&report.path),
                    cli.permissions.as_deref(),
                    cli.quiet,
                )
                .map_err(CliError::operation)?;
            print_note_append_report(cli.output, &report)
        }
        NoteCommand::Patch {
            note,
            section_id,
            heading,
            block_ref,
            lines,
            find,
            replace,
            all,
            check,
            dry_run,
            no_commit,
        } => {
            let auto_commit = AutoCommitPolicy::for_mutation(paths, *no_commit);
            warn_auto_commit_if_needed(&auto_commit, cli.quiet);
            check_write_markdown_source_access(cli, paths, note)?;
            let report = run_note_patch_command(
                paths,
                NotePatchOptions {
                    note,
                    section_id: section_id.as_deref(),
                    heading: heading.as_deref(),
                    block_ref: block_ref.as_deref(),
                    lines: lines.as_deref(),
                    find,
                    replace,
                    replace_all: *all,
                    check: *check,
                    dry_run: *dry_run,
                },
                cli.permissions.as_deref(),
                cli.output,
                use_stderr_color,
                cli.quiet,
            )?;
            if !*dry_run {
                auto_commit
                    .commit(
                        paths,
                        "note-patch",
                        std::slice::from_ref(&report.path),
                        cli.permissions.as_deref(),
                        cli.quiet,
                    )
                    .map_err(CliError::operation)?;
            }
            print_note_patch_report(cli.output, &report)
        }
        NoteCommand::Update {
            filters,
            stdin,
            key,
            value,
            dry_run,
            no_commit,
        } => crate::commands::query::handle_update_command(
            cli, paths, filters, *stdin, key, value, *dry_run, *no_commit,
        ),
        NoteCommand::Unset {
            filters,
            stdin,
            key,
            dry_run,
            no_commit,
        } => crate::commands::query::handle_unset_command(
            cli, paths, filters, *stdin, key, *dry_run, *no_commit,
        ),
        NoteCommand::Delete {
            note,
            dry_run,
            no_commit,
        } => {
            let auto_commit = AutoCommitPolicy::for_mutation(paths, *no_commit);
            warn_auto_commit_if_needed(&auto_commit, cli.quiet);
            check_write_note_access(cli, paths, note)?;
            let report = run_note_delete_command(
                paths,
                note,
                *dry_run,
                cli.permissions.as_deref(),
                cli.output,
                use_stderr_color,
                cli.quiet,
            )?;
            if !*dry_run {
                auto_commit
                    .commit(
                        paths,
                        "note-delete",
                        std::slice::from_ref(&report.path),
                        cli.permissions.as_deref(),
                        cli.quiet,
                    )
                    .map_err(CliError::operation)?;
            }
            print_note_delete_report(cli.output, &report)
        }
        NoteCommand::Rename {
            note,
            new_name,
            dry_run,
            no_commit,
        } => {
            let auto_commit = AutoCommitPolicy::for_mutation(paths, *no_commit);
            warn_auto_commit_if_needed(&auto_commit, cli.quiet);
            check_refactor_note_access(cli, paths, note)?;
            let resolved = resolve_note_reference(paths, note).map_err(CliError::operation)?;
            let destination = note_rename_destination(&resolved.path, new_name);
            let summary = move_note(paths, &resolved.path, &destination, *dry_run)
                .map_err(CliError::operation)?;
            if !*dry_run {
                auto_commit
                    .commit(
                        paths,
                        "note-rename",
                        &crate::move_changed_files(&summary),
                        cli.permissions.as_deref(),
                        cli.quiet,
                    )
                    .map_err(CliError::operation)?;
            }
            crate::print_move_summary(cli.output, &summary)
        }
        NoteCommand::Info { note } => {
            check_read_note_access(cli, paths, note)?;
            let report = run_note_info_command(paths, note)?;
            print_note_info_report(cli.output, &report)
        }
        NoteCommand::History { note, limit } => {
            let guard = selected_permission_guard(cli, paths)?;
            if guard.has_policy_hook() || !guard.read_filter().path_permission().is_unrestricted() {
                let resolved = resolve_note_reference(paths, note).map_err(CliError::operation)?;
                guard
                    .check_read_path(&resolved.path)
                    .map_err(CliError::operation)?;
            }
            guard.check_git().map_err(CliError::operation)?;
            let report = run_note_history_command(paths, note, *limit)?;
            print_note_history_report(cli.output, &report)
        }
        NoteCommand::Links { note, export } => {
            let read_filter = selected_read_permission_filter(cli, paths)?;
            let note =
                resolve_note_argument(paths, note.as_deref(), interactive_note_selection, "note")?;
            let report = query_links_with_filter(paths, &note, read_filter.as_ref())
                .map_err(CliError::operation)?;
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
            let read_filter = selected_read_permission_filter(cli, paths)?;
            let note =
                resolve_note_argument(paths, note.as_deref(), interactive_note_selection, "note")?;
            let report = query_backlinks_with_filter(paths, &note, read_filter.as_ref())
                .map_err(CliError::operation)?;
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
            check_read_note_access(cli, paths, note)?;
            let report = run_note_doctor_command(paths, note)?;
            print_note_doctor_report(cli.output, &report)
        }
        NoteCommand::Diff { note, since } => {
            let guard = selected_permission_guard(cli, paths)?;
            if guard.has_policy_hook() || !guard.read_filter().path_permission().is_unrestricted() {
                let resolved = resolve_note_reference(paths, note).map_err(CliError::operation)?;
                guard
                    .check_read_path(&resolved.path)
                    .map_err(CliError::operation)?;
            }
            guard.check_git().map_err(CliError::operation)?;
            let report = crate::run_diff_command(paths, Some(note), since.as_deref(), false)?;
            crate::print_diff_report(cli.output, &report)
        }
    }
}

fn note_rename_destination(source_path: &str, new_name: &str) -> String {
    if Path::new(new_name).components().count() > 1 {
        return new_name.to_string();
    }

    match Path::new(source_path)
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        Some(parent) => format!("{}/{new_name}", parent.to_string_lossy()),
        None => new_name.to_string(),
    }
}

// Note command implementation moved from crate root.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct NoteGetReport {
    pub(crate) path: String,
    pub(crate) content: String,
    pub(crate) frontmatter: Option<Value>,
    pub(crate) metadata: NoteGetMetadata,
    #[serde(skip)]
    pub(crate) display_lines: Vec<vulcan_core::NoteSelectedLine>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[allow(clippy::struct_excessive_bools)]
pub(crate) struct NoteGetMetadata {
    pub(crate) mode: String,
    pub(crate) section_id: Option<String>,
    pub(crate) heading: Option<String>,
    pub(crate) block_ref: Option<String>,
    pub(crate) lines: Option<String>,
    pub(crate) match_pattern: Option<String>,
    pub(crate) context: usize,
    pub(crate) no_frontmatter: bool,
    pub(crate) raw: bool,
    pub(crate) match_count: usize,
    pub(crate) total_lines: usize,
    pub(crate) has_more_before: bool,
    pub(crate) has_more_after: bool,
    pub(crate) line_spans: Vec<vulcan_core::NoteLineSpan>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct NoteOutlineReport {
    pub(crate) path: String,
    pub(crate) total_lines: usize,
    pub(crate) frontmatter_span: Option<vulcan_core::NoteLineSpan>,
    pub(crate) scope_section: Option<vulcan_core::NoteOutlineSection>,
    pub(crate) depth_limit: Option<usize>,
    pub(crate) sections: Vec<vulcan_core::NoteOutlineSection>,
    pub(crate) block_refs: Vec<vulcan_core::NoteOutlineBlockRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct NoteCheckboxReport {
    pub(crate) path: String,
    pub(crate) dry_run: bool,
    pub(crate) checked: bool,
    pub(crate) changed: bool,
    pub(crate) section_id: Option<String>,
    pub(crate) heading: Option<String>,
    pub(crate) block_ref: Option<String>,
    pub(crate) lines: Option<String>,
    pub(crate) line_number: usize,
    pub(crate) checkbox_index: usize,
    pub(crate) state: String,
    pub(crate) before_marker: String,
    pub(crate) after_marker: String,
    pub(crate) before: String,
    pub(crate) after: String,
    pub(crate) diagnostics: Vec<DoctorDiagnosticIssue>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct NoteSetReport {
    pub(crate) path: String,
    pub(crate) checked: bool,
    pub(crate) preserved_frontmatter: bool,
    pub(crate) diagnostics: Vec<DoctorDiagnosticIssue>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct NoteCreateReport {
    pub(crate) path: String,
    pub(crate) created: bool,
    pub(crate) checked: bool,
    pub(crate) template: Option<String>,
    pub(crate) engine: Option<String>,
    pub(crate) warnings: Vec<String>,
    pub(crate) diagnostics: Vec<DoctorDiagnosticIssue>,
    #[serde(skip)]
    pub(crate) changed_paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct NoteAppendReport {
    pub(crate) path: String,
    pub(crate) appended: bool,
    pub(crate) mode: String,
    pub(crate) checked: bool,
    pub(crate) created: bool,
    pub(crate) heading: Option<String>,
    pub(crate) period_type: Option<String>,
    pub(crate) reference_date: Option<String>,
    pub(crate) warnings: Vec<String>,
    pub(crate) diagnostics: Vec<DoctorDiagnosticIssue>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct NotePatchReport {
    pub(crate) path: String,
    pub(crate) dry_run: bool,
    pub(crate) checked: bool,
    pub(crate) section_id: Option<String>,
    pub(crate) heading: Option<String>,
    pub(crate) block_ref: Option<String>,
    pub(crate) lines: Option<String>,
    pub(crate) line_spans: Vec<vulcan_core::NoteLineSpan>,
    pub(crate) pattern: String,
    pub(crate) regex: bool,
    pub(crate) replace: String,
    pub(crate) match_count: usize,
    pub(crate) changes: Vec<RefactorChange>,
    pub(crate) diagnostics: Vec<DoctorDiagnosticIssue>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct NoteDeleteReport {
    pub(crate) path: String,
    pub(crate) dry_run: bool,
    pub(crate) deleted: bool,
    pub(crate) backlink_count: usize,
    pub(crate) backlinks: Vec<BacklinkRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct NoteInfoReport {
    pub(crate) path: String,
    pub(crate) matched_by: NoteMatchKind,
    pub(crate) word_count: usize,
    pub(crate) heading_count: usize,
    pub(crate) outgoing_link_count: usize,
    pub(crate) backlink_count: usize,
    pub(crate) alias_count: usize,
    pub(crate) tag_count: usize,
    pub(crate) file_size: i64,
    pub(crate) tags: Vec<String>,
    pub(crate) frontmatter_keys: Vec<String>,
    pub(crate) created_at_ms: Option<i64>,
    pub(crate) created_at: Option<String>,
    pub(crate) modified_at_ms: Option<i64>,
    pub(crate) modified_at: Option<String>,
    pub(crate) link_confidence: GraphConfidenceBreakdown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct NoteHistoryReport {
    pub(crate) path: String,
    pub(crate) limit: usize,
    pub(crate) entries: Vec<GitLogEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct NoteDoctorReport {
    pub(crate) path: String,
    pub(crate) diagnostics: Vec<DoctorDiagnosticIssue>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct NoteGetOptions<'a> {
    pub(crate) note: &'a str,
    pub(crate) mode: NoteGetMode,
    pub(crate) section_id: Option<&'a str>,
    pub(crate) heading: Option<&'a str>,
    pub(crate) block_ref: Option<&'a str>,
    pub(crate) lines: Option<&'a str>,
    pub(crate) match_pattern: Option<&'a str>,
    pub(crate) context: usize,
    pub(crate) no_frontmatter: bool,
    pub(crate) raw: bool,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct NoteCheckboxOptions<'a> {
    pub(crate) note: &'a str,
    pub(crate) section_id: Option<&'a str>,
    pub(crate) heading: Option<&'a str>,
    pub(crate) block_ref: Option<&'a str>,
    pub(crate) lines: Option<&'a str>,
    pub(crate) line: Option<usize>,
    pub(crate) index: Option<usize>,
    pub(crate) state: NoteCheckboxState,
    pub(crate) check: bool,
    pub(crate) dry_run: bool,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct NotePatchOptions<'a> {
    pub(crate) note: &'a str,
    pub(crate) section_id: Option<&'a str>,
    pub(crate) heading: Option<&'a str>,
    pub(crate) block_ref: Option<&'a str>,
    pub(crate) lines: Option<&'a str>,
    pub(crate) find: &'a str,
    pub(crate) replace: &'a str,
    pub(crate) replace_all: bool,
    pub(crate) check: bool,
    pub(crate) dry_run: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct NoteCheckboxCandidate {
    pub(crate) checkbox_index: usize,
    pub(crate) line_number: usize,
    pub(crate) marker: char,
    pub(crate) line: String,
}

#[derive(Debug, Clone)]
pub(crate) struct ExistingMarkdownTarget {
    pub(crate) display_path: String,
    pub(crate) absolute_path: PathBuf,
    pub(crate) vault_relative_path: Option<String>,
    pub(crate) config: vulcan_core::VaultConfig,
}

impl ExistingMarkdownTarget {
    fn read_source(&self) -> Result<String, CliError> {
        fs::read_to_string(&self.absolute_path).map_err(CliError::operation)
    }

    fn is_vault_managed(&self) -> bool {
        self.vault_relative_path.is_some()
    }
}

fn app_markdown_target(target: &ExistingMarkdownTarget) -> AppMarkdownTarget {
    AppMarkdownTarget {
        display_path: target.display_path.clone(),
        absolute_path: target.absolute_path.clone(),
        vault_relative_path: target.vault_relative_path.clone(),
        config: target.config.clone(),
    }
}

pub(crate) fn run_note_get_command(
    paths: &VaultPaths,
    options: NoteGetOptions<'_>,
) -> Result<NoteGetReport, CliError> {
    let NoteGetOptions {
        note,
        mode,
        section_id,
        heading,
        block_ref,
        lines,
        match_pattern,
        context,
        no_frontmatter,
        raw,
    } = options;
    let target = read_existing_markdown_source(paths, note)?;
    let parsed = vulcan_core::parse_document(&target.source, &target.target.config);
    let selection = vulcan_core::read_note(
        &target.source,
        &parsed,
        &vulcan_core::NoteReadOptions {
            heading: heading.map(ToOwned::to_owned),
            section_id: section_id.map(ToOwned::to_owned),
            block_ref: block_ref.map(ToOwned::to_owned),
            lines: lines.map(ToOwned::to_owned),
            match_pattern: match_pattern.map(ToOwned::to_owned),
            context,
            no_frontmatter,
        },
    )
    .map_err(CliError::operation)?;

    let selection_is_full_document =
        selection_covers_full_document(&selection.selected_lines, selection.total_lines);
    let rendered_content = render_note_get_content(
        paths,
        target.target.vault_relative_path.as_deref(),
        &selection.content,
        mode,
        selection_is_full_document && !no_frontmatter,
    );
    let frontmatter = parsed
        .frontmatter
        .as_ref()
        .map(serde_json::to_value)
        .transpose()
        .map_err(CliError::operation)?;

    Ok(NoteGetReport {
        path: target.target.display_path,
        content: rendered_content,
        frontmatter,
        metadata: NoteGetMetadata {
            mode: note_get_mode_name(mode).to_string(),
            section_id: selection.section_id.clone(),
            heading: heading.map(ToOwned::to_owned),
            block_ref: block_ref.map(ToOwned::to_owned),
            lines: lines.map(ToOwned::to_owned),
            match_pattern: match_pattern.map(ToOwned::to_owned),
            context,
            no_frontmatter,
            raw,
            match_count: selection.match_count,
            total_lines: selection.total_lines,
            has_more_before: selection.has_more_before,
            has_more_after: selection.has_more_after,
            line_spans: selection.line_spans.clone(),
        },
        display_lines: selection.selected_lines,
    })
}

pub(crate) fn run_note_outline_command(
    paths: &VaultPaths,
    note: &str,
    section_id: Option<&str>,
    depth: Option<usize>,
) -> Result<NoteOutlineReport, CliError> {
    if matches!(depth, Some(0)) {
        return Err(CliError::operation(
            "`note outline --depth` must be at least 1",
        ));
    }
    let target = read_existing_markdown_source(paths, note)?;
    let parsed = vulcan_core::parse_document(&target.source, &target.target.config);
    let outline = vulcan_core::outline_note(&target.source, &parsed);
    let selection = vulcan_core::select_note_outline(
        &outline,
        &vulcan_core::NoteOutlineOptions {
            section_id: section_id.map(ToOwned::to_owned),
            depth,
        },
    )
    .map_err(CliError::operation)?;

    Ok(NoteOutlineReport {
        path: target.target.display_path,
        total_lines: selection.total_lines,
        frontmatter_span: selection.frontmatter_span,
        scope_section: selection.scope_section,
        depth_limit: depth,
        sections: selection.sections,
        block_refs: selection.block_refs,
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn run_note_checkbox_command(
    paths: &VaultPaths,
    options: NoteCheckboxOptions<'_>,
    permission_profile: Option<&str>,
    output: OutputFormat,
    use_stderr_color: bool,
    quiet: bool,
) -> Result<NoteCheckboxReport, CliError> {
    let NoteCheckboxOptions {
        note,
        section_id,
        heading,
        block_ref,
        lines,
        line,
        index,
        state,
        check,
        dry_run,
    } = options;
    if matches!(line, Some(0)) {
        return Err(CliError::operation(
            "`note checkbox --line` must be at least 1",
        ));
    }
    if matches!(index, Some(0)) {
        return Err(CliError::operation(
            "`note checkbox --index` must be at least 1",
        ));
    }

    let target = read_existing_markdown_source(paths, note)?;
    let selection = resolve_note_checkbox_selection(
        &target.source,
        &target.target.config,
        section_id,
        heading,
        block_ref,
        lines,
    )?;
    let candidates = collect_note_checkbox_candidates(&selection.selected_lines);
    let scope_label = note_checkbox_scope_label(
        section_id,
        heading,
        block_ref,
        lines,
        selection.section_id.as_deref(),
    );
    let candidate = select_note_checkbox_candidate(&candidates, line, index, &scope_label)?;
    let (updated_line, after_marker) = update_note_checkbox_line(&candidate.line, state)?;
    let changed = updated_line != candidate.line;
    let updated_content = if changed {
        replace_source_line(&target.source, candidate.line_number, &updated_line)?
    } else {
        target.source.clone()
    };

    if !dry_run && changed {
        if let Some(relative_path) = target.target.vault_relative_path.as_deref() {
            dispatch_note_write_plugin_hooks(
                paths,
                permission_profile,
                relative_path,
                "checkbox",
                Some(&target.source),
                &updated_content,
                quiet,
            )?;
        }
        fs::write(&target.target.absolute_path, &updated_content).map_err(CliError::operation)?;
        if target.target.is_vault_managed() {
            run_incremental_scan(paths, output, use_stderr_color, quiet)?;
        }
    }
    let diagnostics = maybe_check_markdown_target(paths, &target.target, &updated_content, check)?;

    Ok(NoteCheckboxReport {
        path: target.target.display_path,
        dry_run,
        checked: check,
        changed,
        section_id: selection.section_id,
        heading: heading.map(ToOwned::to_owned),
        block_ref: block_ref.map(ToOwned::to_owned),
        lines: lines.map(ToOwned::to_owned),
        line_number: candidate.line_number,
        checkbox_index: candidate.checkbox_index,
        state: note_checkbox_marker_state_name(after_marker).to_string(),
        before_marker: candidate.marker.to_string(),
        after_marker: after_marker.to_string(),
        before: candidate.line,
        after: updated_line,
        diagnostics,
    })
}

fn render_note_get_content(
    paths: &VaultPaths,
    source_path: Option<&str>,
    content: &str,
    mode: NoteGetMode,
    full_document: bool,
) -> String {
    match mode {
        NoteGetMode::Markdown => content.to_string(),
        NoteGetMode::Html => {
            if full_document {
                source_path.map_or_else(
                    || {
                        render_vault_html(
                            paths,
                            content,
                            &HtmlRenderOptions {
                                full_document: true,
                                ..HtmlRenderOptions::default()
                            },
                        )
                        .html
                    },
                    |path| render_note_html(paths, path, content).html,
                )
            } else {
                render_note_fragment_html(paths, source_path, content).html
            }
        }
    }
}

fn note_get_mode_name(mode: NoteGetMode) -> &'static str {
    match mode {
        NoteGetMode::Markdown => "markdown",
        NoteGetMode::Html => "html",
    }
}

fn resolve_note_checkbox_selection(
    source: &str,
    config: &vulcan_core::VaultConfig,
    section_id: Option<&str>,
    heading: Option<&str>,
    block_ref: Option<&str>,
    lines: Option<&str>,
) -> Result<vulcan_core::NoteReadSelection, CliError> {
    let parsed = vulcan_core::parse_document(source, config);
    vulcan_core::read_note(
        source,
        &parsed,
        &vulcan_core::NoteReadOptions {
            heading: heading.map(ToOwned::to_owned),
            section_id: section_id.map(ToOwned::to_owned),
            block_ref: block_ref.map(ToOwned::to_owned),
            lines: lines.map(ToOwned::to_owned),
            match_pattern: None,
            context: 0,
            no_frontmatter: false,
        },
    )
    .map_err(CliError::operation)
}

fn collect_note_checkbox_candidates(
    lines: &[vulcan_core::NoteSelectedLine],
) -> Vec<NoteCheckboxCandidate> {
    let checkbox =
        Regex::new(r"^(\s*(?:[-*+]|\d+[.)])\s+\[)(.)(\])").expect("checkbox regex should compile");
    lines
        .iter()
        .filter_map(|line| {
            let captures = checkbox.captures(&line.text)?;
            let marker = captures.get(2)?.as_str().chars().next()?;
            Some((line.line_number, marker, line.text.clone()))
        })
        .enumerate()
        .map(
            |(index, (line_number, marker, line))| NoteCheckboxCandidate {
                checkbox_index: index + 1,
                line_number,
                marker,
                line,
            },
        )
        .collect()
}

fn select_note_checkbox_candidate(
    candidates: &[NoteCheckboxCandidate],
    line: Option<usize>,
    index: Option<usize>,
    scope_label: &str,
) -> Result<NoteCheckboxCandidate, CliError> {
    if let Some(line_number) = line {
        return candidates
            .iter()
            .find(|candidate| candidate.line_number == line_number)
            .cloned()
            .ok_or_else(|| {
                let available = note_checkbox_candidate_summary(candidates);
                let detail = if available.is_empty() {
                    String::new()
                } else {
                    format!(" Available checkbox lines: {available}.")
                };
                CliError::operation(format!(
                    "no checkbox found at line {line_number} in {scope_label}.{detail}"
                ))
            });
    }

    if let Some(requested_index) = index {
        return candidates
            .iter()
            .find(|candidate| candidate.checkbox_index == requested_index)
            .cloned()
            .ok_or_else(|| {
                CliError::operation(format!(
                    "checkbox index {requested_index} is out of range for {scope_label}; found {} checkbox{}",
                    candidates.len(),
                    if candidates.len() == 1 { "" } else { "es" }
                ))
            });
    }

    match candidates {
        [] => Err(CliError::operation(format!(
            "no markdown checkboxes found in {scope_label}"
        ))),
        [candidate] => Ok(candidate.clone()),
        _ => Err(CliError::operation(format!(
            "found {} markdown checkboxes in {scope_label}; rerun with --line <n> or --index <n>: {}",
            candidates.len(),
            note_checkbox_candidate_preview(candidates)
        ))),
    }
}

fn note_checkbox_scope_label(
    section_id: Option<&str>,
    heading: Option<&str>,
    block_ref: Option<&str>,
    lines: Option<&str>,
    resolved_section_id: Option<&str>,
) -> String {
    if let Some(section_id) = section_id {
        return format!("section `{section_id}`");
    }
    if let Some(section_id) = resolved_section_id {
        if heading.is_some() || block_ref.is_some() {
            return format!("selected scope `{section_id}`");
        }
    }
    if let Some(heading) = heading {
        return format!("heading `{heading}`");
    }
    if let Some(block_ref) = block_ref {
        return format!("block reference `^{block_ref}`");
    }
    if let Some(lines) = lines {
        return format!("line range `{lines}`");
    }
    "note".to_string()
}

fn note_checkbox_candidate_summary(candidates: &[NoteCheckboxCandidate]) -> String {
    candidates
        .iter()
        .map(|candidate| candidate.line_number.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

fn note_checkbox_candidate_preview(candidates: &[NoteCheckboxCandidate]) -> String {
    candidates
        .iter()
        .map(|candidate| {
            format!(
                "{}:{}",
                candidate.line_number,
                note_checkbox_preview_text(&candidate.line)
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn note_checkbox_preview_text(line: &str) -> String {
    const MAX_PREVIEW_CHARS: usize = 48;
    let preview = line.trim();
    let mut truncated = preview.chars().take(MAX_PREVIEW_CHARS).collect::<String>();
    if preview.chars().count() > MAX_PREVIEW_CHARS {
        truncated.push_str("...");
    }
    truncated
}

fn update_note_checkbox_line(
    line: &str,
    state: NoteCheckboxState,
) -> Result<(String, char), CliError> {
    let checkbox =
        Regex::new(r"^(\s*(?:[-*+]|\d+[.)])\s+\[)(.)(\])").expect("checkbox regex should compile");
    let captures = checkbox.captures(line).ok_or_else(|| {
        CliError::operation(format!(
            "line is not a markdown checkbox and cannot be edited: {line}"
        ))
    })?;
    let full = captures
        .get(0)
        .ok_or_else(|| CliError::operation("failed to locate checkbox marker"))?;
    let prefix = captures.get(1).map_or("", |capture| capture.as_str());
    let marker = captures
        .get(2)
        .and_then(|capture| capture.as_str().chars().next())
        .ok_or_else(|| CliError::operation("failed to read checkbox marker"))?;
    let suffix = captures.get(3).map_or("", |capture| capture.as_str());
    let updated_marker = match state {
        NoteCheckboxState::Toggle => note_checkbox_toggled_marker(marker)?,
        NoteCheckboxState::Checked => 'x',
        NoteCheckboxState::Unchecked => ' ',
    };

    Ok((
        format!(
            "{}{}{}{}{}",
            &line[..full.start()],
            prefix,
            updated_marker,
            suffix,
            &line[full.end()..]
        ),
        updated_marker,
    ))
}

fn note_checkbox_toggled_marker(marker: char) -> Result<char, CliError> {
    match marker {
        ' ' => Ok('x'),
        'x' | 'X' => Ok(' '),
        other => Err(CliError::operation(format!(
            "cannot toggle non-standard checkbox marker `[{other}]`; use `--state checked` or `--state unchecked` to normalize it"
        ))),
    }
}

fn note_checkbox_marker_state_name(marker: char) -> &'static str {
    if marker == ' ' {
        "unchecked"
    } else {
        "checked"
    }
}

fn replace_source_line(
    source: &str,
    line_number: usize,
    updated_line: &str,
) -> Result<String, CliError> {
    let index = line_number
        .checked_sub(1)
        .ok_or_else(|| CliError::operation(format!("invalid line number: {line_number}")))?;
    let mut lines = source
        .split_inclusive('\n')
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    let current = lines
        .get(index)
        .ok_or_else(|| CliError::operation(format!("line {line_number} not found")))?;
    let newline = if current.ends_with("\r\n") {
        "\r\n"
    } else if current.ends_with('\n') {
        "\n"
    } else {
        ""
    };
    lines[index] = format!("{updated_line}{newline}");
    Ok(lines.concat())
}

fn selection_covers_full_document(
    selected: &[vulcan_core::NoteSelectedLine],
    total_lines: usize,
) -> bool {
    selected.len() == total_lines
        && selected
            .iter()
            .enumerate()
            .all(|(expected, actual)| actual.line_number == expected + 1)
}

#[allow(clippy::fn_params_excessive_bools, clippy::too_many_arguments)]
pub(crate) fn run_note_set_command(
    paths: &VaultPaths,
    note: &str,
    file: Option<&PathBuf>,
    no_frontmatter: bool,
    check: bool,
    permission_profile: Option<&str>,
    output: OutputFormat,
    use_stderr_color: bool,
    quiet: bool,
) -> Result<NoteSetReport, CliError> {
    let replacement = note_set_input_text(file)?;
    run_note_set_with_content(
        paths,
        note,
        &replacement,
        no_frontmatter,
        check,
        permission_profile,
        output,
        use_stderr_color,
        quiet,
    )
}

#[allow(clippy::fn_params_excessive_bools, clippy::too_many_arguments)]
pub(crate) fn run_note_set_with_content(
    paths: &VaultPaths,
    note: &str,
    replacement: &str,
    preserve_frontmatter: bool,
    check: bool,
    permission_profile: Option<&str>,
    output: OutputFormat,
    use_stderr_color: bool,
    quiet: bool,
) -> Result<NoteSetReport, CliError> {
    let report = apply_note_set(
        paths,
        &AppNoteSetRequest {
            note: note.to_string(),
            replacement: replacement.to_string(),
            preserve_frontmatter,
        },
        permission_profile,
        quiet,
    )?;
    let diagnostics = maybe_check_note(paths, &report.path, &report.content, check)?;
    run_incremental_scan(paths, output, use_stderr_color, quiet)?;

    Ok(NoteSetReport {
        path: report.path,
        checked: check,
        preserved_frontmatter: report.preserved_frontmatter,
        diagnostics,
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn run_note_create_command(
    paths: &VaultPaths,
    path: &str,
    template: Option<&str>,
    frontmatter: &[String],
    check: bool,
    permission_profile: Option<&str>,
    output: OutputFormat,
    use_stderr_color: bool,
    quiet: bool,
) -> Result<NoteCreateReport, CliError> {
    let body = read_optional_stdin_text().map_err(CliError::operation)?;
    run_note_create_with_body(
        paths,
        path,
        template,
        frontmatter,
        &body,
        check,
        permission_profile,
        output,
        use_stderr_color,
        quiet,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn run_note_create_with_body(
    paths: &VaultPaths,
    path: &str,
    template: Option<&str>,
    frontmatter: &[String],
    body: &str,
    check: bool,
    permission_profile: Option<&str>,
    output: OutputFormat,
    use_stderr_color: bool,
    quiet: bool,
) -> Result<NoteCreateReport, CliError> {
    let report = apply_note_create(
        paths,
        &AppNoteCreateRequest {
            path: path.to_string(),
            template: template.map(ToOwned::to_owned),
            frontmatter: parse_frontmatter_bindings(frontmatter)?,
            body: body.to_string(),
        },
        permission_profile,
        quiet,
    )?;
    let diagnostics = maybe_check_note(paths, &report.path, &report.content, check)?;
    run_incremental_scan(paths, output, use_stderr_color, quiet)?;

    Ok(NoteCreateReport {
        path: report.path,
        created: true,
        checked: check,
        template: report.template,
        engine: report.engine,
        warnings: report.warnings,
        diagnostics,
        changed_paths: report.changed_paths,
    })
}

fn note_append_periodic_type(periodic: NoteAppendPeriodicArg) -> &'static str {
    match periodic {
        NoteAppendPeriodicArg::Daily => "daily",
        NoteAppendPeriodicArg::Weekly => "weekly",
        NoteAppendPeriodicArg::Monthly => "monthly",
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct NoteAppendOptions<'a> {
    pub(crate) note: Option<&'a str>,
    pub(crate) text: &'a str,
    pub(crate) mode: NoteAppendMode,
    pub(crate) heading: Option<&'a str>,
    pub(crate) periodic: Option<NoteAppendPeriodicArg>,
    pub(crate) date: Option<&'a str>,
    pub(crate) vars: &'a [String],
    pub(crate) check: bool,
}

pub(crate) fn run_note_append_command(
    paths: &VaultPaths,
    options: NoteAppendOptions<'_>,
    permission_profile: Option<&str>,
    output: OutputFormat,
    use_stderr_color: bool,
    quiet: bool,
) -> Result<NoteAppendReport, CliError> {
    let NoteAppendOptions {
        note,
        text,
        mode,
        heading,
        periodic,
        date,
        vars,
        check,
    } = options;
    let report = apply_note_append(
        paths,
        &AppNoteAppendRequest {
            note: note.map(ToOwned::to_owned),
            text: note_append_input_text(text)?,
            mode,
            heading: heading.map(ToOwned::to_owned),
            periodic: periodic.map(|value| note_append_periodic_type(value).to_string()),
            date: date.map(ToOwned::to_owned),
            vars: parse_template_var_bindings(vars)?,
        },
        permission_profile,
        quiet,
    )?;
    let diagnostics = maybe_check_note(paths, &report.path, &report.content, check)?;
    run_incremental_scan(paths, output, use_stderr_color, quiet)?;

    Ok(NoteAppendReport {
        path: report.path,
        appended: true,
        mode: report.mode,
        checked: check,
        created: report.created,
        heading: report.heading,
        period_type: report.period_type,
        reference_date: report.reference_date,
        warnings: report.warnings,
        diagnostics,
    })
}

pub(crate) fn run_note_patch_command(
    paths: &VaultPaths,
    options: NotePatchOptions<'_>,
    permission_profile: Option<&str>,
    output: OutputFormat,
    use_stderr_color: bool,
    quiet: bool,
) -> Result<NotePatchReport, CliError> {
    let NotePatchOptions {
        note,
        section_id,
        heading,
        block_ref,
        lines,
        find,
        replace,
        replace_all,
        check,
        dry_run,
    } = options;
    let target = resolve_existing_markdown_target(paths, note)?;
    let report = apply_note_patch(
        paths,
        &AppNotePatchRequest {
            target: app_markdown_target(&target),
            section_id: section_id.map(ToOwned::to_owned),
            heading: heading.map(ToOwned::to_owned),
            block_ref: block_ref.map(ToOwned::to_owned),
            lines: lines.map(ToOwned::to_owned),
            find: find.to_string(),
            replace: replace.to_string(),
            replace_all,
            dry_run,
        },
        permission_profile,
        quiet,
    )?;
    if !report.dry_run && !report.changed_paths.is_empty() {
        run_incremental_scan(paths, output, use_stderr_color, quiet)?;
    }
    let diagnostics = maybe_check_markdown_target(paths, &target, &report.content, check)?;

    Ok(NotePatchReport {
        path: report.path,
        dry_run: report.dry_run,
        checked: check,
        section_id: report.section_id,
        heading: heading.map(ToOwned::to_owned),
        block_ref: block_ref.map(ToOwned::to_owned),
        lines: lines.map(ToOwned::to_owned),
        line_spans: report.line_spans,
        pattern: find.to_string(),
        regex: report.regex,
        replace: replace.to_string(),
        match_count: report.match_count,
        changes: report.changes,
        diagnostics,
    })
}

pub(crate) fn run_note_delete_command(
    paths: &VaultPaths,
    note: &str,
    dry_run: bool,
    permission_profile: Option<&str>,
    output: OutputFormat,
    use_stderr_color: bool,
    quiet: bool,
) -> Result<NoteDeleteReport, CliError> {
    let report = apply_note_delete(
        paths,
        &AppNoteDeleteRequest {
            note: note.to_string(),
            dry_run,
        },
        permission_profile,
        quiet,
    )?;
    if !report.dry_run {
        run_incremental_scan(paths, output, use_stderr_color, quiet)?;
    }

    Ok(NoteDeleteReport {
        path: report.path,
        dry_run: report.dry_run,
        deleted: report.deleted,
        backlink_count: report.backlink_count,
        backlinks: report.backlinks,
    })
}

pub(crate) fn run_note_info_command(
    paths: &VaultPaths,
    note: &str,
) -> Result<NoteInfoReport, CliError> {
    let resolved = resolve_note_reference(paths, note).map_err(CliError::operation)?;
    let absolute_path = paths.vault_root().join(&resolved.path);
    let source = fs::read_to_string(&absolute_path).map_err(CliError::operation)?;
    let metadata = fs::metadata(&absolute_path).map_err(CliError::operation)?;
    let config = load_vault_config(paths).config;
    let parsed = vulcan_core::parse_document(&source, &config);
    let outgoing = query_links(paths, &resolved.path).map_err(CliError::operation)?;
    let backlinks = query_backlinks(paths, &resolved.path).map_err(CliError::operation)?;

    let mut tags = parsed
        .tags
        .iter()
        .map(|tag| tag.tag_text.clone())
        .collect::<Vec<_>>();
    tags.sort();
    tags.dedup();

    let mut frontmatter_keys = parsed
        .frontmatter
        .as_ref()
        .and_then(|frontmatter| frontmatter.as_mapping())
        .map(|mapping| {
            mapping
                .keys()
                .filter_map(|value| value.as_str())
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    frontmatter_keys.sort();

    let modified_at_ms = metadata
        .modified()
        .ok()
        .or_else(|| metadata.created().ok())
        .and_then(system_time_to_millis);
    let created_at_ms = metadata
        .created()
        .ok()
        .or_else(|| metadata.modified().ok())
        .and_then(system_time_to_millis)
        .or(modified_at_ms);

    Ok(NoteInfoReport {
        link_confidence: crate::link_confidence_for_note(paths, &resolved.path)?,
        path: resolved.path,
        matched_by: resolved.matched_by,
        word_count: note_word_count(&source),
        heading_count: parsed.headings.len(),
        outgoing_link_count: outgoing.links.len(),
        backlink_count: backlinks.backlinks.len(),
        alias_count: parsed.aliases.len(),
        tag_count: tags.len(),
        file_size: i64::try_from(metadata.len()).unwrap_or(i64::MAX),
        tags,
        frontmatter_keys,
        created_at_ms,
        created_at: created_at_ms.map(format_utc_timestamp_ms),
        modified_at_ms,
        modified_at: modified_at_ms.map(format_utc_timestamp_ms),
    })
}

pub(crate) fn run_note_history_command(
    paths: &VaultPaths,
    note: &str,
    limit: usize,
) -> Result<NoteHistoryReport, CliError> {
    let relative_path = resolve_existing_note_path(paths, note)?;
    let entries =
        git_log(paths.vault_root(), &relative_path, limit).map_err(CliError::operation)?;
    Ok(NoteHistoryReport {
        path: relative_path,
        limit,
        entries,
    })
}

pub(crate) fn run_note_doctor_command(
    paths: &VaultPaths,
    note: &str,
) -> Result<NoteDoctorReport, CliError> {
    let (relative_path, source) = read_existing_note_source(paths, note)?;
    let diagnostics = diagnose_note_contents(paths, &relative_path, &source)?;
    Ok(NoteDoctorReport {
        path: relative_path,
        diagnostics,
    })
}

fn note_word_count(source: &str) -> usize {
    let body = note_body_without_frontmatter(source);
    body.lines()
        .filter_map(normalize_note_word_line)
        .flat_map(str::split_whitespace)
        .count()
}

fn note_body_without_frontmatter(source: &str) -> &str {
    find_frontmatter_block(source).map_or(source, |(_, _, end)| &source[end..])
}

fn normalize_note_word_line(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    if trimmed.is_empty() || is_block_ref_only_line(trimmed) {
        return None;
    }

    let trimmed = if let Some(level) = markdown_heading_level(trimmed) {
        trimmed[level..].trim()
    } else {
        trimmed
    };
    let trimmed = strip_markdown_list_marker(trimmed).trim();
    (!trimmed.is_empty()).then_some(trimmed)
}

fn is_block_ref_only_line(line: &str) -> bool {
    line.starts_with('^')
        && line.len() > 1
        && line[1..]
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
}

fn strip_markdown_list_marker(line: &str) -> &str {
    let trimmed = line.trim_start();
    for prefix in ["- ", "* ", "+ "] {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            return rest;
        }
    }

    let digits = trimmed.chars().take_while(char::is_ascii_digit).count();
    if digits > 0 {
        let rest = &trimmed[digits..];
        if let Some(rest) = rest.strip_prefix(". ") {
            return rest;
        }
    }

    trimmed
}

fn system_time_to_millis(time: std::time::SystemTime) -> Option<i64> {
    let duration = time.duration_since(std::time::UNIX_EPOCH).ok()?;
    i64::try_from(duration.as_millis()).ok()
}

fn format_utc_timestamp_ms(ms: i64) -> String {
    TemplateTimestamp::from_millis(ms)
        .default_strings()
        .datetime
}

#[derive(Debug, Clone)]
struct ExistingMarkdownSource {
    target: ExistingMarkdownTarget,
    source: String,
}

pub(crate) fn resolve_existing_markdown_target(
    paths: &VaultPaths,
    note: &str,
) -> Result<ExistingMarkdownTarget, CliError> {
    if let Ok(relative_path) = resolve_existing_note_path(paths, note) {
        let absolute_path = paths.vault_root().join(&relative_path);
        return Ok(ExistingMarkdownTarget {
            display_path: relative_path.clone(),
            absolute_path,
            vault_relative_path: Some(relative_path),
            config: load_vault_config(paths).config,
        });
    }

    if note_argument_looks_like_path(note) {
        return resolve_existing_direct_markdown_target(paths, note);
    }

    Err(CliError::operation(format!("note not found: {note}")))
}

fn read_existing_markdown_source(
    paths: &VaultPaths,
    note: &str,
) -> Result<ExistingMarkdownSource, CliError> {
    let target = resolve_existing_markdown_target(paths, note)?;
    let source = target.read_source()?;
    Ok(ExistingMarkdownSource { target, source })
}

fn read_existing_note_source(paths: &VaultPaths, note: &str) -> Result<(String, String), CliError> {
    let relative_path = resolve_existing_note_path(paths, note)?;
    let source =
        fs::read_to_string(paths.vault_root().join(&relative_path)).map_err(CliError::operation)?;
    Ok((relative_path, source))
}

pub(crate) fn resolve_existing_note_path(
    paths: &VaultPaths,
    note: &str,
) -> Result<String, CliError> {
    match resolve_note_reference(paths, note) {
        Ok(resolved) => Ok(resolved.path),
        Err(GraphQueryError::AmbiguousIdentifier { .. }) => Err(CliError::operation(format!(
            "note identifier '{note}' is ambiguous"
        ))),
        Err(GraphQueryError::CacheMissing | GraphQueryError::NoteNotFound { .. }) => {
            let normalized = normalize_note_path(note)?;
            if paths.vault_root().join(&normalized).is_file() {
                Ok(normalized)
            } else {
                Err(CliError::operation(format!("note not found: {note}")))
            }
        }
        Err(error) => Err(CliError::operation(error)),
    }
}

pub(crate) fn normalize_note_path(path: &str) -> Result<String, CliError> {
    normalize_relative_input_path(
        path,
        RelativePathOptions {
            expected_extension: Some("md"),
            append_extension_if_missing: true,
        },
    )
    .map_err(CliError::operation)
}

fn note_argument_looks_like_path(note: &str) -> bool {
    let path = Path::new(note);
    path.is_absolute()
        || path.extension().is_some()
        || note.starts_with('.')
        || path.components().count() > 1
}

fn resolve_existing_direct_markdown_target(
    paths: &VaultPaths,
    note: &str,
) -> Result<ExistingMarkdownTarget, CliError> {
    let current_dir = std::env::current_dir().map_err(CliError::operation)?;
    for candidate in direct_markdown_path_candidates(note) {
        if !has_markdown_extension(&candidate) {
            continue;
        }

        let absolute_candidate = if candidate.is_absolute() {
            candidate.clone()
        } else {
            current_dir.join(&candidate)
        };
        if !absolute_candidate.is_file() {
            continue;
        }

        let absolute_path = fs::canonicalize(&absolute_candidate).map_err(CliError::operation)?;
        let vault_relative_path = paths
            .relative_to_vault(&absolute_path)
            .map(|path| path_buf_to_slash_string(&path));
        let display_path = vault_relative_path.clone().unwrap_or_else(|| {
            if candidate.is_absolute() {
                absolute_candidate.to_string_lossy().into_owned()
            } else {
                candidate.to_string_lossy().into_owned()
            }
        });

        return Ok(ExistingMarkdownTarget {
            display_path,
            absolute_path,
            vault_relative_path: vault_relative_path.clone(),
            config: if vault_relative_path.is_some() {
                load_vault_config(paths).config
            } else {
                vulcan_core::VaultConfig::default()
            },
        });
    }

    Err(CliError::operation(format!("note not found: {note}")))
}

fn direct_markdown_path_candidates(note: &str) -> Vec<PathBuf> {
    let path = PathBuf::from(note);
    let mut candidates = vec![path.clone()];
    if path.extension().is_none() {
        let mut with_extension = path;
        with_extension.set_extension("md");
        candidates.push(with_extension);
    }
    candidates
}

fn has_markdown_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
}

pub(crate) fn path_buf_to_slash_string(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn note_set_input_text(file: Option<&PathBuf>) -> Result<String, CliError> {
    if let Some(file) = file {
        return fs::read_to_string(file).map_err(CliError::operation);
    }
    if io::stdin().is_terminal() {
        return Err(CliError::operation(
            "`note set` requires replacement content on stdin or --file <path>",
        ));
    }
    let mut buffer = String::new();
    io::stdin()
        .read_to_string(&mut buffer)
        .map_err(CliError::operation)?;
    Ok(buffer)
}

fn note_append_input_text(text: &str) -> Result<String, CliError> {
    if text != "-" {
        return Ok(text.to_string());
    }

    let mut buffer = String::new();
    io::stdin()
        .read_to_string(&mut buffer)
        .map_err(CliError::operation)?;
    Ok(buffer)
}

fn dispatch_note_write_plugin_hooks(
    paths: &VaultPaths,
    permission_profile: Option<&str>,
    relative_path: &str,
    operation: &str,
    existing: Option<&str>,
    updated: &str,
    quiet: bool,
) -> Result<(), CliError> {
    crate::plugins::dispatch_plugin_event(
        paths,
        permission_profile,
        PluginEvent::OnNoteWrite,
        &serde_json::json!({
            "kind": PluginEvent::OnNoteWrite,
            "path": relative_path,
            "operation": operation,
            "existed_before": existing.is_some(),
            "previous_content": existing,
            "content": updated,
        }),
        quiet,
    )
}

fn read_optional_stdin_text() -> io::Result<String> {
    if io::stdin().is_terminal() {
        return Ok(String::new());
    }

    let mut buffer = String::new();
    io::stdin().read_to_string(&mut buffer)?;
    Ok(buffer)
}

pub(crate) fn print_note_get_report(
    output: OutputFormat,
    report: &NoteGetReport,
    stdout_is_tty: bool,
    use_color: bool,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human => {
            if report.metadata.mode == "html"
                || report.metadata.raw
                || (report.metadata.lines.is_none() && report.metadata.match_pattern.is_none())
            {
                if report.metadata.mode == "html" || report.metadata.raw {
                    print!("{}", report.content);
                } else {
                    return print_markdown_output(
                        output,
                        &report.content,
                        stdout_is_tty,
                        use_color,
                    );
                }
            } else {
                let mut previous_line = None;
                for line in &report.display_lines {
                    if previous_line.is_some_and(|line_number| line.line_number != line_number + 1)
                    {
                        println!("--");
                    }
                    println!("{}: {}", line.line_number, line.text);
                    previous_line = Some(line.line_number);
                }
            }
            Ok(())
        }
        OutputFormat::Markdown => {
            if report.metadata.mode == "html"
                || report.metadata.raw
                || (report.metadata.lines.is_some() || report.metadata.match_pattern.is_some())
            {
                print!("{}", report.content);
            } else {
                print_markdown_output(output, &report.content, stdout_is_tty, use_color)?;
                return Ok(());
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

pub(crate) fn print_note_outline_report(
    output: OutputFormat,
    report: &NoteOutlineReport,
    use_color: bool,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            let palette = AnsiPalette::new(matches!(output, OutputFormat::Human) && use_color);
            println!("{}", palette.bold(&report.path));
            println!("{} {}", palette.dim("lines:"), report.total_lines);
            println!(
                "{} {}",
                palette.dim("frontmatter:"),
                report
                    .frontmatter_span
                    .as_ref()
                    .map_or_else(|| "none".to_string(), note_line_span_label)
            );
            if let Some(depth_limit) = report.depth_limit {
                println!("{} {depth_limit}", palette.dim("depth:"));
            }
            if let Some(scope_section) = &report.scope_section {
                println!();
                println!("{}", palette.bold("Scope"));
                print_note_outline_section_entry(report, scope_section, palette, true);
            }

            println!();
            println!(
                "{} {}",
                palette.bold("Sections"),
                palette.dim(&format!("({})", report.sections.len()))
            );
            if report.sections.is_empty() {
                println!("  {}", palette.dim("none"));
            } else {
                for section in &report.sections {
                    print_note_outline_section_entry(report, section, palette, false);
                }
            }

            println!();
            println!(
                "{} {}",
                palette.bold("Block refs"),
                palette.dim(&format!("({})", report.block_refs.len()))
            );
            if report.block_refs.is_empty() {
                println!("  {}", palette.dim("none"));
            } else {
                for block_ref in &report.block_refs {
                    let depth = note_outline_block_ref_display_depth(
                        report,
                        block_ref.section_id.as_deref(),
                    );
                    let indent = "  ".repeat(depth + 1);
                    println!("{}{}", indent, palette.green(&format!("^{}", block_ref.id)));
                    println!(
                        "{}  {} {}",
                        indent,
                        palette.dim("lines:"),
                        note_line_span_label(&vulcan_core::NoteLineSpan {
                            start_line: block_ref.start_line,
                            end_line: block_ref.end_line,
                        })
                    );
                    if let Some(section_id) = block_ref.section_id.as_deref() {
                        println!(
                            "{}  {} {}",
                            indent,
                            palette.dim("section:"),
                            palette.yellow(section_id)
                        );
                    }
                }
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn note_line_span_label(span: &vulcan_core::NoteLineSpan) -> String {
    format!("{}-{}", span.start_line, span.end_line)
}

pub(crate) fn print_note_outline_section_entry(
    report: &NoteOutlineReport,
    section: &vulcan_core::NoteOutlineSection,
    palette: AnsiPalette,
    is_scope: bool,
) {
    let depth = if is_scope {
        0
    } else {
        note_outline_display_depth(section, report.scope_section.as_ref()) + 1
    };
    let indent = "  ".repeat(depth);
    println!("{}{}", indent, note_outline_heading_label(section, palette));
    println!(
        "{}  {} {}",
        indent,
        palette.dim("lines:"),
        note_line_span_label(&vulcan_core::NoteLineSpan {
            start_line: section.start_line,
            end_line: section.end_line,
        })
    );
    println!(
        "{}  {} {}",
        indent,
        palette.dim("id:"),
        palette.yellow(&section.id)
    );
}

fn note_outline_heading_label(
    section: &vulcan_core::NoteOutlineSection,
    palette: AnsiPalette,
) -> String {
    match section.heading.as_deref() {
        Some(heading) => {
            let hashes = "#".repeat(usize::from(section.level.max(1)));
            format!("{} {}", palette.cyan(&hashes), palette.bold(heading))
        }
        None => palette.bold("[preamble]"),
    }
}

fn note_outline_display_depth(
    section: &vulcan_core::NoteOutlineSection,
    scope_section: Option<&vulcan_core::NoteOutlineSection>,
) -> usize {
    match scope_section {
        Some(scope) => section
            .heading_path
            .len()
            .saturating_sub(scope.heading_path.len() + 1),
        None => section.heading_path.len().saturating_sub(1),
    }
}

fn note_outline_block_ref_display_depth(
    report: &NoteOutlineReport,
    section_id: Option<&str>,
) -> usize {
    section_id
        .and_then(|id| {
            report
                .scope_section
                .iter()
                .chain(report.sections.iter())
                .find(|section| section.id == id)
        })
        .map_or(0, |section| {
            note_outline_display_depth(section, report.scope_section.as_ref())
        })
}

pub(crate) fn print_note_set_report(
    output: OutputFormat,
    report: &NoteSetReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            println!(
                "Updated {}{}.",
                report.path,
                if report.preserved_frontmatter {
                    " (preserved frontmatter)"
                } else {
                    ""
                }
            );
            print_note_check_warnings(&report.path, &report.diagnostics);
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

pub(crate) fn print_note_create_report(
    output: OutputFormat,
    report: &NoteCreateReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            println!("Created {}.", report.path);
            if let Some(template) = report.template.as_deref() {
                let engine = report.engine.as_deref().unwrap_or("auto");
                println!("Template: {template} ({engine})");
            }
            for warning in &report.warnings {
                eprintln!("warning: {warning}");
            }
            print_note_check_warnings(&report.path, &report.diagnostics);
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

pub(crate) fn print_note_append_report(
    output: OutputFormat,
    report: &NoteAppendReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            let target = report.period_type.as_deref().map_or_else(
                || report.path.clone(),
                |period_type| {
                    if let Some(reference_date) = report.reference_date.as_deref() {
                        format!("{} ({period_type} {reference_date})", report.path)
                    } else {
                        format!("{} ({period_type})", report.path)
                    }
                },
            );
            match report.mode.as_str() {
                "after_heading" => println!(
                    "Appended to {} under {}.",
                    target,
                    report.heading.as_deref().unwrap_or_default()
                ),
                "prepend" => println!("Prepended to {target}."),
                _ => println!("Appended to {target}."),
            }
            if report.created {
                println!("Created missing note first.");
            }
            for warning in &report.warnings {
                eprintln!("warning: {warning}");
            }
            print_note_check_warnings(&report.path, &report.diagnostics);
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

pub(crate) fn print_note_checkbox_report(
    output: OutputFormat,
    report: &NoteCheckboxReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            let target = format!("{} line {}", report.path, report.line_number);
            if report.dry_run {
                if report.changed {
                    println!(
                        "Dry run: would set checkbox {} to {}.",
                        target, report.state
                    );
                } else {
                    println!("Dry run: checkbox {} is already {}.", target, report.state);
                }
            } else if report.changed {
                println!("Set checkbox {} to {}.", target, report.state);
            } else {
                println!("Checkbox {} was already {}.", target, report.state);
            }
            println!(
                "Checkbox #{}: {} -> {}",
                report.checkbox_index, report.before, report.after
            );
            print_note_check_warnings(&report.path, &report.diagnostics);
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

pub(crate) fn print_note_patch_report(
    output: OutputFormat,
    report: &NotePatchReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if report.dry_run {
                println!(
                    "Dry run: would patch {} ({} match{}).",
                    report.path,
                    report.match_count,
                    if report.match_count == 1 { "" } else { "es" }
                );
            } else {
                println!(
                    "Patched {} ({} match{}).",
                    report.path,
                    report.match_count,
                    if report.match_count == 1 { "" } else { "es" }
                );
            }
            for change in &report.changes {
                println!("- {} -> {}", change.before, change.after);
            }
            print_note_check_warnings(&report.path, &report.diagnostics);
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

pub(crate) fn print_note_delete_report(
    output: OutputFormat,
    report: &NoteDeleteReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if report.dry_run {
                println!("Dry run: would delete {}.", report.path);
            } else {
                println!("Deleted {}.", report.path);
            }

            if report.backlinks.is_empty() {
                println!("No inbound links would be left dangling.");
            } else {
                println!(
                    "{} inbound link{} would become unresolved:",
                    report.backlink_count,
                    if report.backlink_count == 1 { "" } else { "s" }
                );
                for backlink in &report.backlinks {
                    println!("- {}: {}", backlink.source_path, backlink.raw_text);
                }
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

pub(crate) fn print_note_info_report(
    output: OutputFormat,
    report: &NoteInfoReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            println!("{}", report.path);
            println!("Matched by: {}", note_match_kind_label(&report.matched_by));
            println!("Words: {}", report.word_count);
            println!("Headings: {}", report.heading_count);
            println!(
                "Links: {} outgoing, {} backlinks",
                report.outgoing_link_count, report.backlink_count
            );
            println!("Aliases: {}", report.alias_count);
            println!("Size: {} bytes", report.file_size);
            println!(
                "Tags: {}",
                if report.tags.is_empty() {
                    "(none)".to_string()
                } else {
                    report
                        .tags
                        .iter()
                        .map(|tag| format!("#{tag}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            );
            println!(
                "Frontmatter: {}",
                if report.frontmatter_keys.is_empty() {
                    "(none)".to_string()
                } else {
                    report.frontmatter_keys.join(", ")
                }
            );
            println!(
                "Created: {}",
                report.created_at.as_deref().unwrap_or("unknown")
            );
            println!(
                "Modified: {}",
                report.modified_at.as_deref().unwrap_or("unknown")
            );
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn note_match_kind_label(kind: &NoteMatchKind) -> &'static str {
    match kind {
        NoteMatchKind::Path => "path",
        NoteMatchKind::Filename => "filename",
        NoteMatchKind::Alias => "alias",
    }
}

pub(crate) fn print_note_history_report(
    output: OutputFormat,
    report: &NoteHistoryReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if report.entries.is_empty() {
                println!("No commits for {}.", report.path);
                return Ok(());
            }
            println!("History for {}:", report.path);
            for entry in &report.entries {
                println!(
                    "- {} {} ({}, {})",
                    entry.commit.chars().take(8).collect::<String>(),
                    entry.summary,
                    entry.author_name,
                    entry.committed_at
                );
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

pub(crate) fn print_note_check_warnings(path: &str, diagnostics: &[DoctorDiagnosticIssue]) {
    for diagnostic in diagnostics {
        eprintln!(
            "warning: {}: {}",
            diagnostic.document_path.as_deref().unwrap_or(path),
            diagnostic.message
        );
    }
}

pub(crate) fn print_note_doctor_report(
    output: OutputFormat,
    report: &NoteDoctorReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            println!("Doctor summary for {}", report.path);
            if report.diagnostics.is_empty() {
                println!("No issues found.");
            } else {
                print_diagnostic_section("Diagnostics", &report.diagnostics);
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn parse_frontmatter_bindings(
    bindings: &[String],
) -> Result<Option<vulcan_app::templates::YamlMapping>, CliError> {
    parse_note_frontmatter_bindings(bindings).map_err(CliError::operation)
}

fn maybe_check_markdown_target(
    paths: &VaultPaths,
    target: &ExistingMarkdownTarget,
    content: &str,
    check: bool,
) -> Result<Vec<DoctorDiagnosticIssue>, CliError> {
    if !check {
        return Ok(Vec::new());
    }

    match target.vault_relative_path.as_deref() {
        Some(relative_path) => {
            diagnose_note_contents(paths, relative_path, content).map_err(Into::into)
        }
        None => diagnose_external_markdown_contents(&target.display_path, &target.config, content)
            .map_err(Into::into),
    }
}

fn maybe_check_note(
    paths: &VaultPaths,
    relative_path: &str,
    content: &str,
    check: bool,
) -> Result<Vec<DoctorDiagnosticIssue>, CliError> {
    if !check {
        return Ok(Vec::new());
    }

    diagnose_note_contents(paths, relative_path, content).map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::note_rename_destination;

    #[test]
    fn note_rename_destination_keeps_same_folder_for_bare_names() {
        assert_eq!(
            note_rename_destination("Projects/Alpha.md", "Beta"),
            "Projects/Beta"
        );
    }

    #[test]
    fn note_rename_destination_keeps_root_notes_at_root() {
        assert_eq!(note_rename_destination("Alpha.md", "Beta.md"), "Beta.md");
    }

    #[test]
    fn note_rename_destination_allows_explicit_destination_paths() {
        assert_eq!(
            note_rename_destination("Projects/Alpha.md", "Archive/Beta"),
            "Archive/Beta"
        );
    }
}
