use crate::output::print_json;
use crate::resolve::resolve_note_argument;
use crate::{CliError, OutputFormat};
use serde::Serialize;
use std::process::Command as ProcessCommand;
use vulcan_core::{resolve_note_reference, VaultPaths};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct OpenReport {
    path: String,
    uri: String,
}

pub(crate) fn run_open_command(
    paths: &VaultPaths,
    note: Option<&str>,
    interactive_note_selection: bool,
) -> Result<OpenReport, CliError> {
    let note = resolve_note_argument(paths, note, interactive_note_selection, "note")?;
    let resolved = resolve_note_reference(paths, &note).map_err(CliError::operation)?;
    let uri = build_obsidian_uri(paths, &resolved.path);
    launch_uri(&uri)?;

    Ok(OpenReport {
        path: resolved.path,
        uri,
    })
}

pub(crate) fn print_open_report(output: OutputFormat, report: &OpenReport) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            println!("Opened {} in Obsidian", report.path);
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn build_obsidian_uri(paths: &VaultPaths, path: &str) -> String {
    let vault_name = paths
        .vault_root()
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("vault");
    format!(
        "obsidian://open?vault={}&file={}",
        percent_encode(vault_name),
        percent_encode(path)
    )
}

fn percent_encode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                char::from(byte).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

fn launch_uri(uri: &str) -> Result<(), CliError> {
    let mut command = ProcessCommand::new(open_uri_program());
    for arg in open_uri_args(uri) {
        command.arg(arg);
    }
    let status = command.status().map_err(CliError::operation)?;
    if status.success() {
        Ok(())
    } else {
        Err(CliError::operation(format!(
            "launcher exited with status {status} for {uri}"
        )))
    }
}

#[cfg(target_os = "linux")]
fn open_uri_program() -> &'static str {
    "xdg-open"
}

#[cfg(target_os = "macos")]
fn open_uri_program() -> &'static str {
    "open"
}

#[cfg(target_os = "windows")]
fn open_uri_program() -> &'static str {
    "cmd"
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn open_uri_program() -> &'static str {
    "xdg-open"
}

#[cfg(target_os = "windows")]
fn open_uri_args(uri: &str) -> Vec<String> {
    vec![
        "/C".to_string(),
        "start".to_string(),
        String::new(),
        uri.to_string(),
    ]
}

#[cfg(not(target_os = "windows"))]
fn open_uri_args(uri: &str) -> Vec<String> {
    vec![uri.to_string()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn percent_encode_keeps_uri_safe_ascii() {
        assert_eq!(
            percent_encode("A note/with spaces.md"),
            "A%20note%2Fwith%20spaces.md"
        );
        assert_eq!(percent_encode("x_y-z.~"), "x_y-z.~");
    }

    #[test]
    fn build_obsidian_uri_encodes_vault_and_note_path() {
        let paths = VaultPaths::new(PathBuf::from("/tmp/My Vault"));
        assert_eq!(
            build_obsidian_uri(&paths, "Folder/Some Note.md"),
            "obsidian://open?vault=My%20Vault&file=Folder%2FSome%20Note.md"
        );
    }
}
