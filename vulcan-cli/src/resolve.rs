use crate::{note_picker, Cli, CliError, OutputFormat};
use std::io::{self, IsTerminal};
use vulcan_core::{resolve_note_reference, GraphQueryError, VaultPaths};

pub(crate) fn interactive_note_selection_allowed(cli: &Cli, stdout_is_tty: bool) -> bool {
    cli.output == OutputFormat::Human && stdout_is_tty && io::stdin().is_terminal()
}

pub(crate) fn resolve_note_argument(
    paths: &VaultPaths,
    identifier: Option<&str>,
    interactive: bool,
    prompt: &str,
) -> Result<String, CliError> {
    match identifier {
        Some(identifier) => match resolve_note_reference(paths, identifier) {
            Ok(_) => Ok(identifier.to_string()),
            Err(GraphQueryError::AmbiguousIdentifier { matches, .. }) if interactive => {
                note_picker::pick_note(paths, Some(identifier), Some(&matches))
                    .map_err(CliError::operation)?
                    .ok_or_else(|| CliError::operation(format!("cancelled {prompt} selection")))
            }
            Err(GraphQueryError::NoteNotFound { .. }) if interactive => {
                note_picker::pick_note(paths, Some(identifier), None)
                    .map_err(CliError::operation)?
                    .ok_or_else(|| CliError::operation(format!("cancelled {prompt} selection")))
            }
            Err(error) => Err(CliError::operation(error)),
        },
        None if interactive => note_picker::pick_note(paths, None, None)
            .map_err(CliError::operation)?
            .ok_or_else(|| CliError::operation(format!("cancelled {prompt} selection"))),
        None => Err(CliError::operation(format!(
            "missing {prompt}; provide a note identifier or run interactively"
        ))),
    }
}
