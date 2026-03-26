use crate::editor::{open_in_editor, with_terminal_suspended};
use crate::note_picker::{handle_picker_key, NotePickerState, PickerAction};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use std::io;
use std::time::Duration;
use vulcan_core::{list_note_identities, scan_vault, NoteIdentity, ScanMode, VaultPaths};

pub fn run_browse_tui(paths: &VaultPaths) -> Result<(), io::Error> {
    let mut state = BrowseState::new(paths.clone(), load_notes(paths).map_err(io::Error::other)?);

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.hide_cursor()?;

    let result = run_event_loop(&mut terminal, &mut state);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

fn load_notes(paths: &VaultPaths) -> Result<Vec<NoteIdentity>, String> {
    list_note_identities(paths).map_err(|error| error.to_string())
}

fn run_event_loop(
    terminal: &mut Terminal<ratatui::backend::CrosstermBackend<io::Stdout>>,
    state: &mut BrowseState,
) -> Result<(), io::Error> {
    loop {
        terminal.draw(|frame| draw(frame, state))?;

        if !event::poll(Duration::from_millis(200))? {
            continue;
        }

        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match state.handle_key(key.code) {
                BrowseAction::Continue => {}
                BrowseAction::Quit => break,
                BrowseAction::Edit(path) => {
                    let absolute = state.paths.vault_root().join(&path);
                    let paths = state.paths.clone();
                    let edit_result = with_terminal_suspended(terminal, || {
                        open_in_editor(&absolute)?;
                        scan_vault(&paths, ScanMode::Incremental)
                            .map(|_| ())
                            .map_err(|error| error.to_string())
                    });
                    match edit_result {
                        Ok(()) => {
                            if let Err(error) = state.reload_notes() {
                                state.set_status(error);
                            } else {
                                state.set_status(format!("Updated {path}."));
                            }
                        }
                        Err(error) => {
                            state.picker.refresh_preview();
                            state.set_status(error.to_string());
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

fn draw(frame: &mut Frame<'_>, state: &BrowseState) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(12),
            Constraint::Length(5),
        ])
        .split(frame.area());

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(52), Constraint::Percentage(48)])
        .split(layout[1]);

    let query = Paragraph::new(state.picker.query().to_string()).block(
        Block::default()
            .title("Browse (/ fuzzy search)")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Yellow)),
    );
    frame.render_widget(query, layout[0]);

    let filtered = state.picker.filtered_notes();
    let items = filtered
        .iter()
        .map(|(_, note)| {
            let aliases = if note.aliases.is_empty() {
                String::new()
            } else {
                format!(" [{}]", note.aliases.join(", "))
            };
            ListItem::new(format!("{}{}", note.path, aliases))
        })
        .collect::<Vec<_>>();
    let list = List::new(items)
        .highlight_style(Style::default().bg(Color::DarkGray))
        .block(
            Block::default()
                .title("Notes")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        );
    let mut list_state = ListState::default();
    list_state.select(state.picker.selected_index());
    frame.render_stateful_widget(list, body[0], &mut list_state);

    let preview_title = state
        .picker
        .selected_path()
        .map_or_else(|| "Preview".to_string(), |path| format!("Preview: {path}"));
    let preview = Paragraph::new(state.picker.preview_lines())
        .block(
            Block::default()
                .title(preview_title)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(preview, body[1]);

    let footer = Paragraph::new(vec![
        Line::from("Keys: Enter/e edit, Esc quit, j/k move"),
        Line::from("      type to filter by path, filename, or alias"),
        Line::from(format!(
            "Notes: {} filtered / {} total",
            state.picker.filtered_count(),
            state.picker.total_notes()
        )),
        Line::from(format!("Status: {}", state.status_line())),
    ])
    .block(
        Block::default()
            .title("Browse")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan)),
    )
    .wrap(Wrap { trim: false });
    frame.render_widget(footer, layout[2]);
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum BrowseAction {
    Continue,
    Quit,
    Edit(String),
}

#[derive(Debug, Clone)]
struct BrowseState {
    paths: VaultPaths,
    picker: NotePickerState,
    status: String,
}

impl BrowseState {
    fn new(paths: VaultPaths, notes: Vec<NoteIdentity>) -> Self {
        Self::with_query(paths, notes, "")
    }

    fn with_query(paths: VaultPaths, notes: Vec<NoteIdentity>, query: &str) -> Self {
        Self {
            paths: paths.clone(),
            picker: NotePickerState::new(paths, notes, query),
            status: "Ready.".to_string(),
        }
    }

    fn handle_key(&mut self, code: KeyCode) -> BrowseAction {
        match code {
            KeyCode::Esc => BrowseAction::Quit,
            KeyCode::Enter | KeyCode::Char('e') => {
                if let Some(path) = self.picker.selected_path().map(str::to_string) {
                    BrowseAction::Edit(path)
                } else {
                    self.set_status("No matching note selected.");
                    BrowseAction::Continue
                }
            }
            _ => {
                self.clear_status();
                match handle_picker_key(&mut self.picker, code) {
                    PickerAction::Continue => BrowseAction::Continue,
                    PickerAction::Cancel => BrowseAction::Quit,
                    PickerAction::Select => unreachable!("Enter is handled before picker actions"),
                }
            }
        }
    }

    fn reload_notes(&mut self) -> Result<(), String> {
        let notes = load_notes(&self.paths)?;
        self.picker.replace_notes_preserve_selection(notes);
        Ok(())
    }

    fn set_status(&mut self, status: impl Into<String>) {
        self.status = status.into();
    }

    fn clear_status(&mut self) {
        self.status.clear();
    }

    fn status_line(&self) -> &str {
        if self.status.is_empty() {
            "Ready."
        } else {
            &self.status
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;
    use tempfile::TempDir;

    fn note(path: &str, aliases: &[&str]) -> NoteIdentity {
        NoteIdentity {
            path: path.to_string(),
            filename: path
                .rsplit('/')
                .next()
                .unwrap_or(path)
                .trim_end_matches(".md")
                .to_string(),
            aliases: aliases.iter().map(|alias| (*alias).to_string()).collect(),
        }
    }

    fn write_note(root: &Path, relative_path: &str, contents: &str) {
        let absolute = root.join(relative_path);
        if let Some(parent) = absolute.parent() {
            fs::create_dir_all(parent).expect("note parent should be created");
        }
        fs::write(absolute, contents).expect("note should be written");
    }

    #[test]
    fn enter_requests_edit_for_selected_note() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        write_note(temp_dir.path(), "Projects/Alpha.md", "Alpha");
        let mut state = BrowseState::with_query(
            paths,
            vec![
                note("Projects/Alpha.md", &[]),
                note("Projects/Beta.md", &[]),
            ],
            "alpha",
        );

        let action = state.handle_key(KeyCode::Enter);

        assert_eq!(action, BrowseAction::Edit("Projects/Alpha.md".to_string()));
        assert_eq!(state.picker.query(), "alpha");
        assert_eq!(state.picker.selected_path(), Some("Projects/Alpha.md"));
    }

    #[test]
    fn e_requests_edit_for_selected_note() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        write_note(temp_dir.path(), "Projects/Alpha.md", "Alpha");
        let mut state = BrowseState::with_query(paths, vec![note("Projects/Alpha.md", &[])], "");

        let action = state.handle_key(KeyCode::Char('e'));

        assert_eq!(action, BrowseAction::Edit("Projects/Alpha.md".to_string()));
    }

    #[test]
    fn enter_without_match_sets_status() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        let mut state = BrowseState::with_query(paths, vec![note("Projects/Alpha.md", &[])], "zzz");

        let action = state.handle_key(KeyCode::Enter);

        assert_eq!(action, BrowseAction::Continue);
        assert_eq!(state.status_line(), "No matching note selected.");
    }

    #[test]
    fn replacing_notes_preserves_selected_path() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        write_note(temp_dir.path(), "Projects/Alpha.md", "Alpha");
        write_note(temp_dir.path(), "Projects/Beta.md", "Beta");
        let mut state = BrowseState::with_query(
            paths,
            vec![
                note("Projects/Alpha.md", &[]),
                note("Projects/Beta.md", &[]),
            ],
            "",
        );

        assert_eq!(state.picker.selected_path(), Some("Projects/Alpha.md"));

        state.picker.replace_notes_preserve_selection(vec![
            note("Projects/Alpha.md", &["Start"]),
            note("Projects/Gamma.md", &[]),
        ]);

        assert_eq!(state.picker.query(), "");
        assert_eq!(state.picker.selected_path(), Some("Projects/Alpha.md"));
        assert_eq!(state.picker.filtered_count(), 2);
    }
}
