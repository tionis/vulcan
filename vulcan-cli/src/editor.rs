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
    let parts = split_command_line(&editor)?;
    let program = parts.first().map_or("vi", String::as_str);
    let mut command = ProcessCommand::new(program);
    for arg in parts.iter().skip(1) {
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

fn split_command_line(command: &str) -> Result<Vec<String>, String> {
    let mut arguments = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut chars = command.chars().peekable();

    while let Some(ch) = chars.next() {
        match quote {
            Some(delimiter) => {
                if ch == delimiter {
                    quote = None;
                } else if delimiter == '"' && ch == '\\' {
                    match chars.peek().copied() {
                        Some('"' | '\\') => {
                            current.push(chars.next().expect("peeked character should exist"));
                        }
                        _ => current.push(ch),
                    }
                } else {
                    current.push(ch);
                }
            }
            None => match ch {
                '"' | '\'' => quote = Some(ch),
                _ if ch.is_whitespace() => {
                    if !current.is_empty() {
                        arguments.push(std::mem::take(&mut current));
                    }
                }
                _ => current.push(ch),
            },
        }
    }

    if quote.is_some() {
        return Err(format!(
            "failed to parse editor `{command}`: unterminated quote"
        ));
    }
    if !current.is_empty() {
        arguments.push(current);
    }
    if arguments.is_empty() {
        return Err("failed to parse editor command: no executable specified".to_string());
    }

    Ok(arguments)
}

#[cfg(test)]
mod tests {
    use super::split_command_line;

    #[test]
    fn split_command_line_handles_basic_commands() {
        assert_eq!(
            split_command_line("vim -f").expect("command should parse"),
            vec!["vim", "-f"]
        );
    }

    #[test]
    fn split_command_line_preserves_quoted_executable_and_args() {
        assert_eq!(
            split_command_line("\"C:\\Program Files\\Editor\\editor.exe\" --wait")
                .expect("command should parse"),
            vec!["C:\\Program Files\\Editor\\editor.exe", "--wait"]
        );
    }

    #[test]
    fn split_command_line_supports_cmd_wrapper() {
        assert_eq!(
            split_command_line("cmd /C \"C:\\temp\\editor.cmd\"").expect("command should parse"),
            vec!["cmd", "/C", "C:\\temp\\editor.cmd"]
        );
    }

    #[test]
    fn split_command_line_rejects_unterminated_quotes() {
        let error = split_command_line("\"unterminated").expect_err("command should fail");
        assert!(error.contains("unterminated quote"));
    }
}
