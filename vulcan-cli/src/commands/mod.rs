pub(crate) mod agent;
pub(crate) mod bases;
pub(crate) mod browse;
pub(crate) mod cache;
pub(crate) mod completions;
pub(crate) mod config;
pub(crate) mod dataview;
pub(crate) mod docs;
pub(crate) mod edit;
pub(crate) mod graph;
pub(crate) mod inbox;
pub(crate) mod index;
pub(crate) mod kanban;
pub(crate) mod note;
pub(crate) mod open;
pub(crate) mod periodic;
pub(crate) mod plugin;
pub(crate) mod query;
pub(crate) mod refactor;
pub(crate) mod runtime;
pub(crate) mod skill;
pub(crate) mod status;
pub(crate) mod tasks;
pub(crate) mod template;
pub(crate) mod tool;
pub(crate) mod tool_init;
#[cfg(feature = "vectors")]
pub(crate) mod vectors;

#[cfg(not(feature = "vectors"))]
pub(crate) mod vectors {
    use vulcan_core::VaultPaths;

    use crate::{Cli, CliError, ListOutputControls, VectorsCommand};

    pub(crate) fn handle_vectors_command(
        _cli: &Cli,
        _paths: &VaultPaths,
        _command: &VectorsCommand,
        _interactive_note_selection: bool,
        _list_controls: &ListOutputControls,
        _stdout_is_tty: bool,
        _use_stdout_color: bool,
        _use_stderr_color: bool,
    ) -> Result<(), CliError> {
        Err(CliError::operation(
            "the `vectors` command requires a build with the `vectors` feature enabled",
        ))
    }
}
