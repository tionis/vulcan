use crate::editor::open_in_editor;
use crate::output::print_json;
use crate::resolve::{interactive_note_selection_allowed, resolve_note_argument};
use crate::{run_incremental_scan, Cli, CliError, OutputFormat};
use serde::Serialize;
use std::fs;
use std::path::Path;
use std::process::Command as ProcessCommand;
use vulcan_core::paths::{normalize_relative_input_path, RelativePathOptions};
use vulcan_core::{
    git_status, query_change_report, resolve_note_reference, ChangeAnchor, VaultPaths,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct EditReport {
    pub(crate) path: String,
    pub(crate) created: bool,
    pub(crate) rescanned: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct DiffReport {
    pub(crate) path: String,
    pub(crate) anchor: String,
    pub(crate) source: String,
    pub(crate) status: String,
    pub(crate) changed: bool,
    pub(crate) changed_kinds: Vec<String>,
    pub(crate) diff: Option<String>,
}

fn resolve_edit_path(
    paths: &VaultPaths,
    cli: &Cli,
    stdout_is_tty: bool,
    use_stderr_color: bool,
    note: Option<&str>,
    new: bool,
) -> Result<(String, bool), CliError> {
    if new {
        let note = note.ok_or_else(|| {
            CliError::operation("`edit --new` requires a relative note path such as Notes/Idea.md")
        })?;
        let path = normalize_relative_input_path(
            note,
            RelativePathOptions {
                expected_extension: Some("md"),
                append_extension_if_missing: true,
            },
        )
        .map_err(CliError::operation)?;
        return Ok((path, true));
    }

    if !paths.cache_db().exists() {
        run_incremental_scan(paths, cli.output, use_stderr_color, cli.quiet)?;
    }

    let interactive = interactive_note_selection_allowed(cli, stdout_is_tty);
    let note = resolve_note_argument(paths, note, interactive, "note")?;
    let resolved = resolve_note_reference(paths, &note).map_err(CliError::operation)?;
    Ok((resolved.path, false))
}

pub(crate) fn run_edit_command(
    paths: &VaultPaths,
    cli: &Cli,
    stdout_is_tty: bool,
    use_stderr_color: bool,
    note: Option<&str>,
    new: bool,
) -> Result<EditReport, CliError> {
    let (relative_path, creating_new_note) =
        resolve_edit_path(paths, cli, stdout_is_tty, use_stderr_color, note, new)?;
    let absolute_path = paths.vault_root().join(&relative_path);
    let mut created = false;
    if creating_new_note {
        if let Some(parent) = absolute_path.parent() {
            fs::create_dir_all(parent).map_err(CliError::operation)?;
        }
        if !absolute_path.exists() {
            fs::write(&absolute_path, "").map_err(CliError::operation)?;
            created = true;
        }
    } else if !absolute_path.is_file() {
        return Err(CliError::operation(format!(
            "note does not exist on disk: {relative_path}"
        )));
    }

    open_in_editor(&absolute_path).map_err(CliError::operation)?;
    run_incremental_scan(paths, cli.output, use_stderr_color, cli.quiet)?;

    Ok(EditReport {
        path: relative_path,
        created,
        rescanned: true,
    })
}

pub(crate) fn run_diff_command(
    paths: &VaultPaths,
    note: Option<&str>,
    since: Option<&str>,
    interactive_note_selection: bool,
) -> Result<DiffReport, CliError> {
    let note = resolve_note_argument(paths, note, interactive_note_selection, "note")?;
    let resolved = resolve_note_reference(paths, &note).map_err(CliError::operation)?;

    if let Some(checkpoint) = since {
        return diff_report_from_change_anchor(
            paths,
            &resolved.path,
            &ChangeAnchor::Checkpoint(checkpoint.to_string()),
            format!("checkpoint:{checkpoint}"),
        );
    }

    if vulcan_core::is_git_repo(paths.vault_root()) {
        return diff_report_from_git(paths, &resolved.path);
    }

    diff_report_from_change_anchor(
        paths,
        &resolved.path,
        &ChangeAnchor::LastScan,
        "last_scan".to_string(),
    )
}

fn diff_report_from_git(paths: &VaultPaths, path: &str) -> Result<DiffReport, CliError> {
    let status = git_status(paths.vault_root()).map_err(CliError::operation)?;
    let untracked = status.untracked.iter().any(|candidate| candidate == path);
    let diff = render_git_diff(paths.vault_root(), path, untracked)?;
    let changed = !diff.trim().is_empty();

    Ok(DiffReport {
        path: path.to_string(),
        anchor: "HEAD".to_string(),
        source: "git_head".to_string(),
        status: if untracked {
            "new".to_string()
        } else if changed {
            "changed".to_string()
        } else {
            "unchanged".to_string()
        },
        changed,
        changed_kinds: if changed {
            vec!["note".to_string()]
        } else {
            Vec::new()
        },
        diff: changed.then_some(diff),
    })
}

fn diff_report_from_change_anchor(
    paths: &VaultPaths,
    path: &str,
    anchor: &ChangeAnchor,
    anchor_label: String,
) -> Result<DiffReport, CliError> {
    let report = query_change_report(paths, anchor).map_err(CliError::operation)?;
    let mut changed_kinds = Vec::new();
    let note_status = report
        .notes
        .iter()
        .find(|item| item.path == path)
        .map(|item| item.status);

    if note_status.is_some() {
        changed_kinds.push("note".to_string());
    }
    if report.links.iter().any(|item| item.path == path) {
        changed_kinds.push("links".to_string());
    }
    if report.properties.iter().any(|item| item.path == path) {
        changed_kinds.push("properties".to_string());
    }
    if report.embeddings.iter().any(|item| item.path == path) {
        changed_kinds.push("embeddings".to_string());
    }

    let status = match note_status {
        Some(ChangeKindStatus::Added) => "new",
        Some(ChangeKindStatus::Deleted) => "deleted",
        Some(ChangeKindStatus::Updated) => "changed",
        None => {
            if changed_kinds.is_empty() {
                "unchanged"
            } else {
                "changed"
            }
        }
    }
    .to_string();

    Ok(DiffReport {
        path: path.to_string(),
        anchor: anchor_label,
        source: "cache".to_string(),
        changed: status != "unchanged",
        status,
        changed_kinds,
        diff: None,
    })
}

type ChangeKindStatus = vulcan_core::ChangeStatus;

fn render_git_diff(vault_root: &Path, path: &str, untracked: bool) -> Result<String, CliError> {
    let output = if untracked {
        let empty_path = std::env::temp_dir().join(format!(
            "vulcan-empty-diff-{}-{}",
            std::process::id(),
            path.replace('/', "_")
        ));
        fs::write(&empty_path, "").map_err(CliError::operation)?;
        let output = ProcessCommand::new("git")
            .arg("-C")
            .arg(vault_root)
            .args(["diff", "--no-index", "--no-color"])
            .arg(&empty_path)
            .arg(vault_root.join(path))
            .output()
            .map_err(CliError::operation)?;
        let _ = fs::remove_file(&empty_path);
        output
    } else {
        ProcessCommand::new("git")
            .arg("-C")
            .arg(vault_root)
            .args(["diff", "--no-color", "HEAD", "--", path])
            .output()
            .map_err(CliError::operation)?
    };

    if untracked {
        if !matches!(output.status.code(), Some(0 | 1)) {
            return Err(CliError::operation(String::from_utf8_lossy(&output.stderr)));
        }
    } else if !output.status.success() {
        return Err(CliError::operation(String::from_utf8_lossy(&output.stderr)));
    }

    String::from_utf8(output.stdout).map_err(CliError::operation)
}

pub(crate) fn print_edit_report(output: OutputFormat, report: &EditReport) {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if report.created {
                println!("Created and edited {}", report.path);
            } else {
                println!("Edited {}", report.path);
            }
        }
        OutputFormat::Json => {
            print_json(report).expect("edit report JSON serialization should succeed");
        }
    }
}

pub(crate) fn print_diff_report(output: OutputFormat, report: &DiffReport) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if let Some(diff) = report.diff.as_deref() {
                if diff.trim().is_empty() {
                    println!("No changes in {} since {}.", report.path, report.anchor);
                } else {
                    println!("Diff for {} against {}:", report.path, report.anchor);
                    print!("{diff}");
                    if !diff.ends_with('\n') {
                        println!();
                    }
                }
            } else if report.changed {
                println!(
                    "{} changed since {} ({})",
                    report.path,
                    report.anchor,
                    report.changed_kinds.join(", ")
                );
            } else {
                println!("No changes in {} since {}.", report.path, report.anchor);
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}
