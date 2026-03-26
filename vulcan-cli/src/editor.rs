use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io;
use std::path::Path;
use std::process::Command as ProcessCommand;

type CliTerminal = Terminal<CrosstermBackend<io::Stdout>>;

pub(crate) fn with_terminal_suspended<F>(
    terminal: &mut CliTerminal,
    operation: F,
) -> Result<(), io::Error>
where
    F: FnOnce() -> Result<(), String>,
{
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    let operation_result = operation();

    execute!(terminal.backend_mut(), EnterAlternateScreen)?;
    enable_raw_mode()?;
    terminal.hide_cursor()?;
    terminal.clear()?;

    operation_result.map_err(io::Error::other)
}

pub(crate) fn open_in_editor(path: &Path) -> Result<(), String> {
    let editor = std::env::var("VISUAL")
        .ok()
        .or_else(|| std::env::var("EDITOR").ok())
        .unwrap_or_else(|| "vi".to_string());
    let mut parts = editor.split_whitespace();
    let program = parts.next().unwrap_or("vi");
    let mut command = ProcessCommand::new(program);
    for arg in parts {
        command.arg(arg);
    }
    let status = command
        .arg(path)
        .status()
        .map_err(|error| format!("failed to launch editor `{editor}`: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("editor `{editor}` exited with status {status}"))
    }
}
