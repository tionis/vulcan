//! `vulcan daily calendar`: pick a day on a month grid and edit its daily note.

use crate::calendar_view::{CalendarViewState, CALENDAR_EVENT_PREVIEW_LIMIT};
use crate::editor::with_terminal_suspended;
use crate::note_picker::{Redraw, TerminalRestore};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use std::fs;
use std::io;
use vulcan_core::VaultPaths;

/// Width of the month grid: seven 4-column day cells plus borders.
const CALENDAR_WIDTH: u16 = 7 * 4 + 2;
/// Header, up to six weeks, legend, and borders.
const CALENDAR_HEIGHT: u16 = 1 + 6 + 1 + 2;
const KEY_HELP: &str = "Enter/e edit or create  arrows/hjkl move  [/] or PgUp/PgDn month  \
     Home/End month edges  t today  type YYYY-MM-DD to jump  q/Esc quit";

/// Creates the note for a date when missing, opens it in the editor, and
/// returns a status line. Runs with the terminal suspended.
pub(crate) type OpenDailyNote<'a> = dyn FnMut(&str) -> Result<String, String> + 'a;

pub(crate) fn run_daily_calendar_tui(
    paths: &VaultPaths,
    today: &str,
    initial: &str,
    open_note: &mut OpenDailyNote<'_>,
) -> Result<(), io::Error> {
    let mut state =
        DailyCalendarState::new(paths.clone(), today, initial).map_err(io::Error::other)?;

    enable_raw_mode()?;
    let _restore = TerminalRestore;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.hide_cursor()?;

    let result = run_event_loop(&mut terminal, &mut state, open_note);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

fn run_event_loop(
    terminal: &mut Terminal<ratatui::backend::CrosstermBackend<io::Stdout>>,
    state: &mut DailyCalendarState,
    open_note: &mut OpenDailyNote<'_>,
) -> Result<(), io::Error> {
    let mut redraw = Redraw::default();
    loop {
        if redraw.take() {
            terminal.draw(|frame| draw(frame, state))?;
        }
        let event = event::read()?;
        redraw.event(&event);
        let Event::Key(key) = event else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match state.handle_key(key) {
            DailyCalendarAction::Continue => {}
            DailyCalendarAction::Quit => return Ok(()),
            DailyCalendarAction::Open(date) => {
                let mut status = String::new();
                let result = with_terminal_suspended(terminal, || {
                    status = open_note(&date)?;
                    Ok(())
                });
                match result {
                    Ok(()) => state.status = Some(status),
                    Err(error) => state.status = Some(format!("Error: {error}")),
                }
                if let Err(error) = state.reload() {
                    state.status = Some(format!("Error: {error}"));
                }
                redraw.invalidate();
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DailyCalendarAction {
    Continue,
    Quit,
    Open(String),
}

struct DailyCalendarState {
    paths: VaultPaths,
    calendar: CalendarViewState,
    preview: Vec<String>,
    status: Option<String>,
}

impl DailyCalendarState {
    fn new(paths: VaultPaths, today: &str, initial: &str) -> Result<Self, String> {
        let calendar = CalendarViewState::new_at(&paths, today, initial)?;
        let mut state = Self {
            paths,
            calendar,
            preview: Vec::new(),
            status: None,
        };
        state.refresh_preview();
        Ok(state)
    }

    fn handle_key(&mut self, key: KeyEvent) -> DailyCalendarAction {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return if key.code == KeyCode::Char('c') {
                DailyCalendarAction::Quit
            } else {
                DailyCalendarAction::Continue
            };
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => DailyCalendarAction::Quit,
            KeyCode::Enter | KeyCode::Char('e') => {
                if self.calendar.target_path().is_some() {
                    DailyCalendarAction::Open(self.calendar.selected_date_iso())
                } else {
                    self.status = Some("Daily notes are disabled in config.".to_string());
                    DailyCalendarAction::Continue
                }
            }
            code => {
                self.status = None;
                if let Err(error) = self.calendar.handle_key(&self.paths, code) {
                    self.status = Some(format!("Error: {error}"));
                }
                self.refresh_preview();
                DailyCalendarAction::Continue
            }
        }
    }

    fn reload(&mut self) -> Result<(), String> {
        let result = self.calendar.refresh(&self.paths);
        self.refresh_preview();
        result
    }

    fn refresh_preview(&mut self) {
        self.preview = match self.calendar.selected_existing_path() {
            Some(path) => match fs::read_to_string(self.paths.vault_root().join(path)) {
                Ok(contents) if contents.trim().is_empty() => vec!["<empty note>".to_string()],
                Ok(contents) => contents.lines().map(str::to_string).collect(),
                Err(error) => vec![format!("Failed to read {path}: {error}")],
            },
            None => match self.calendar.target_path() {
                Some(path) => vec![
                    "No daily note for this day yet.".to_string(),
                    String::new(),
                    format!("Press Enter to create {path}"),
                    "from the configured daily template.".to_string(),
                ],
                None => vec!["Daily notes are disabled in config.".to_string()],
            },
        };
    }

    fn preview_title(&self) -> String {
        let date = self.calendar.selected_date_iso();
        match self.calendar.selected_existing_path() {
            Some(path) => format!("{date}  {path}"),
            None => format!("{date}  (no note)"),
        }
    }

    fn event_lines(&self) -> Vec<Line<'static>> {
        let events = self.calendar.selected_events();
        if events.is_empty() {
            return vec![Line::from("No events")];
        }
        let mut lines = events
            .iter()
            .take(CALENDAR_EVENT_PREVIEW_LIMIT)
            .map(|event| {
                Line::from(match event.end_time.as_deref() {
                    Some(end_time) => format!("{}-{} {}", event.start_time, end_time, event.title),
                    None => format!("{} {}", event.start_time, event.title),
                })
            })
            .collect::<Vec<_>>();
        let hidden = events.len().saturating_sub(CALENDAR_EVENT_PREVIEW_LIMIT);
        if hidden > 0 {
            lines.push(Line::from(format!("... {hidden} more")));
        }
        lines
    }

    fn footer_line(&self) -> String {
        match (self.status.as_deref(), self.calendar.query()) {
            (Some(status), _) => status.to_string(),
            (None, query) if !query.is_empty() => format!("Jump: {query}"),
            (None, _) => KEY_HELP.to_string(),
        }
    }
}

fn draw(frame: &mut Frame<'_>, state: &DailyCalendarState) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(CALENDAR_HEIGHT), Constraint::Length(3)])
        .split(frame.area());
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(CALENDAR_WIDTH), Constraint::Min(20)])
        .split(layout[0]);
    let sidebar = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(CALENDAR_HEIGHT), Constraint::Min(3)])
        .split(body[0]);

    let border = Style::default().fg(Color::Cyan);
    frame.render_widget(
        Paragraph::new(state.calendar.calendar_lines()).block(
            Block::default()
                .title(state.calendar.title())
                .borders(Borders::ALL)
                .border_style(border),
        ),
        sidebar[0],
    );
    frame.render_widget(
        Paragraph::new(state.event_lines())
            .block(
                Block::default()
                    .title("Events")
                    .borders(Borders::ALL)
                    .border_style(border),
            )
            .wrap(Wrap { trim: false }),
        sidebar[1],
    );
    draw_preview(frame, state, body[1], border);
    frame.render_widget(
        Paragraph::new(state.footer_line())
            .block(
                Block::default()
                    .title("Daily notes")
                    .borders(Borders::ALL)
                    .border_style(border),
            )
            .style(if state.status.is_some() {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            })
            .wrap(Wrap { trim: false }),
        layout[1],
    );
}

fn draw_preview(frame: &mut Frame<'_>, state: &DailyCalendarState, area: Rect, border: Style) {
    // Only materialize the lines that can be visible.
    let visible = usize::from(area.height.saturating_sub(2));
    let lines = state
        .preview
        .iter()
        .take(visible)
        .map(|line| Line::from(line.clone()))
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .title(state.preview_title())
                    .borders(Borders::ALL)
                    .border_style(border),
            )
            .wrap(Wrap { trim: false }),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use tempfile::TempDir;
    use vulcan_core::{scan_vault, ScanMode};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn fixture() -> (TempDir, VaultPaths) {
        let temp_dir = TempDir::new().expect("temp dir");
        let root = temp_dir.path();
        fs::create_dir_all(root.join(".vulcan")).expect("vulcan dir");
        fs::write(
            root.join(".vulcan/config.toml"),
            "[periodic.daily]\nfolder = \"Journal/Daily\"\nschedule_heading = \"Schedule\"\n",
        )
        .expect("config");
        fs::create_dir_all(root.join("Journal/Daily")).expect("daily dir");
        fs::write(
            root.join("Journal/Daily/2026-10-05.md"),
            "# Monday\n\nShipped the calendar.\n\n## Schedule\n- 09:00 Standup\n",
        )
        .expect("daily note");
        let paths = VaultPaths::new(root);
        scan_vault(&paths, ScanMode::Full).expect("scan");
        (temp_dir, paths)
    }

    #[test]
    fn starts_on_initial_date_and_previews_existing_note() {
        let (_temp, paths) = fixture();
        let state = DailyCalendarState::new(paths, "2026-10-06", "2026-10-05").expect("state");

        assert_eq!(state.calendar.selected_date_iso(), "2026-10-05");
        assert_eq!(
            state.preview_title(),
            "2026-10-05  Journal/Daily/2026-10-05.md"
        );
        assert!(state.preview.iter().any(|line| line.contains("Shipped")));
        assert_eq!(state.calendar.selected_events().len(), 1);
    }

    #[test]
    fn moving_to_a_missing_day_offers_creation_and_enter_opens_it() {
        let (_temp, paths) = fixture();
        let mut state = DailyCalendarState::new(paths, "2026-10-06", "2026-10-05").expect("state");

        assert_eq!(
            state.handle_key(key(KeyCode::Right)),
            DailyCalendarAction::Continue
        );
        assert_eq!(state.calendar.selected_date_iso(), "2026-10-06");
        assert!(state.preview_title().ends_with("(no note)"));
        assert!(state
            .preview
            .iter()
            .any(|line| line.contains("Journal/Daily/2026-10-06.md")));
        assert_eq!(
            state.handle_key(key(KeyCode::Enter)),
            DailyCalendarAction::Open("2026-10-06".to_string())
        );
    }

    #[test]
    fn navigation_keys_jump_months_today_and_typed_dates() {
        let (_temp, paths) = fixture();
        let mut state = DailyCalendarState::new(paths, "2026-10-06", "2026-10").expect("state");
        assert_eq!(state.calendar.selected_date_iso(), "2026-10-01");

        state.handle_key(key(KeyCode::Char(']')));
        assert_eq!(state.calendar.selected_date_iso(), "2026-11-01");
        state.handle_key(key(KeyCode::Char('t')));
        assert_eq!(state.calendar.selected_date_iso(), "2026-10-06");
        state.handle_key(key(KeyCode::Char('k')));
        assert_eq!(state.calendar.selected_date_iso(), "2026-09-29");
        for character in "2025-12-24".chars() {
            state.handle_key(key(KeyCode::Char(character)));
        }
        assert_eq!(state.calendar.selected_date_iso(), "2025-12-24");
        assert_eq!(state.footer_line(), "Jump: 2025-12-24");
        assert_eq!(
            state.handle_key(key(KeyCode::Char('q'))),
            DailyCalendarAction::Quit
        );
    }

    #[test]
    fn reload_picks_up_notes_created_outside_the_view() {
        let (temp, paths) = fixture();
        let mut state =
            DailyCalendarState::new(paths.clone(), "2026-10-06", "2026-10-06").expect("state");
        assert!(state.calendar.selected_existing_path().is_none());

        fs::write(
            temp.path().join("Journal/Daily/2026-10-06.md"),
            "# Tuesday\n\nWrote this from the editor.\n",
        )
        .expect("note");
        scan_vault(&paths, ScanMode::Incremental).expect("scan");
        state.reload().expect("reload");

        assert_eq!(
            state.calendar.selected_existing_path(),
            Some("Journal/Daily/2026-10-06.md")
        );
        assert!(state
            .preview
            .iter()
            .any(|line| line.contains("from the editor")));
    }

    #[test]
    fn renders_grid_preview_and_key_help() {
        let (_temp, paths) = fixture();
        let state = DailyCalendarState::new(paths, "2026-10-06", "2026-10-05").expect("state");
        let mut terminal = Terminal::new(TestBackend::new(100, 20)).expect("terminal");
        terminal.draw(|frame| draw(frame, &state)).expect("draw");
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect::<String>();

        assert!(rendered.contains("Calendar (October 2026)"));
        assert!(rendered.contains("Shipped the calendar."));
        assert!(rendered.contains("09:00 Standup"));
        assert!(rendered.contains("t today"));
    }
}
