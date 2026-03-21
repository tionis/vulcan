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
use std::collections::HashSet;
use std::fs;
use std::io;
use std::time::Duration;
use vulcan_core::{list_note_identities, NoteIdentity, VaultPaths};

pub fn pick_note(
    paths: &VaultPaths,
    initial_query: Option<&str>,
    restrict_paths: Option<&[String]>,
) -> Result<Option<String>, io::Error> {
    let mut notes = list_note_identities(paths).map_err(io::Error::other)?;
    if let Some(restrict_paths) = restrict_paths {
        let allowed = restrict_paths.iter().cloned().collect::<HashSet<_>>();
        notes.retain(|note| allowed.contains(&note.path));
    }

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.hide_cursor()?;
    let mut state = NotePickerState::new(paths.clone(), notes, initial_query.unwrap_or_default());

    let result = run_event_loop(&mut terminal, &mut state);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

fn run_event_loop(
    terminal: &mut Terminal<ratatui::backend::CrosstermBackend<io::Stdout>>,
    state: &mut NotePickerState,
) -> Result<Option<String>, io::Error> {
    loop {
        terminal.draw(|frame| draw(frame, state))?;

        if !event::poll(Duration::from_millis(200))? {
            continue;
        }

        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match key.code {
                KeyCode::Esc | KeyCode::Char('q') => return Ok(None),
                KeyCode::Enter => return Ok(state.selected_note().map(|note| note.path.clone())),
                KeyCode::Up | KeyCode::Char('k') => state.move_selection(-1),
                KeyCode::Down | KeyCode::Char('j') => state.move_selection(1),
                KeyCode::Backspace => state.pop_query(),
                KeyCode::Char(character) => state.push_query(character),
                _ => {}
            }
        }
    }
}

fn draw(frame: &mut Frame<'_>, state: &NotePickerState) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(12),
            Constraint::Length(4),
        ])
        .split(frame.area());

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(52), Constraint::Percentage(48)])
        .split(layout[1]);

    let query = Paragraph::new(state.query.clone()).block(
        Block::default()
            .title("Pick Note (/ fuzzy search)")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Yellow)),
    );
    frame.render_widget(query, layout[0]);

    let items = state
        .filtered_notes()
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
                .title("Matches")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        );
    let mut list_state = ListState::default();
    list_state.select(state.selected_index);
    frame.render_stateful_widget(list, body[0], &mut list_state);

    let preview_title = state.selected_note().map_or_else(
        || "Preview".to_string(),
        |note| format!("Preview: {}", note.path),
    );
    let preview = Paragraph::new(state.preview_lines())
        .block(
            Block::default()
                .title(preview_title)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(preview, body[1]);

    let footer = Paragraph::new(vec![
        Line::from("Keys: Enter select, Esc cancel, j/k move"),
        Line::from("      type to filter by path, filename, or alias"),
        Line::from(format!("Matches: {}", state.filtered_notes().len())),
    ])
    .block(
        Block::default()
            .title("Picker")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan)),
    )
    .wrap(Wrap { trim: false });
    frame.render_widget(footer, layout[2]);
}

#[derive(Debug, Clone)]
struct NotePickerState {
    paths: VaultPaths,
    notes: Vec<NoteIdentity>,
    query: String,
    selected_index: Option<usize>,
    preview: Vec<String>,
}

impl NotePickerState {
    fn new(paths: VaultPaths, notes: Vec<NoteIdentity>, query: &str) -> Self {
        let mut state = Self {
            paths,
            notes,
            query: query.to_string(),
            selected_index: None,
            preview: vec!["No notes available.".to_string()],
        };
        state.clamp_selection();
        state
    }

    fn filtered_notes(&self) -> Vec<(i32, &NoteIdentity)> {
        let mut filtered = self
            .notes
            .iter()
            .filter_map(|note| fuzzy_score(note, &self.query).map(|score| (score, note)))
            .collect::<Vec<_>>();
        filtered.sort_by(|(left_score, left), (right_score, right)| {
            right_score
                .cmp(left_score)
                .then_with(|| left.path.cmp(&right.path))
        });
        filtered
    }

    fn selected_note(&self) -> Option<&NoteIdentity> {
        let filtered = self.filtered_notes();
        self.selected_index
            .and_then(|index| filtered.get(index).map(|(_, note)| *note))
    }

    fn move_selection(&mut self, delta: isize) {
        let len = self.filtered_notes().len();
        if len == 0 {
            self.selected_index = None;
            self.preview = vec!["No matches.".to_string()];
            return;
        }
        let current = self.selected_index.unwrap_or(0);
        let step = delta.unsigned_abs();
        let next = if delta.is_negative() {
            current.saturating_sub(step)
        } else {
            current.saturating_add(step)
        }
        .min(len - 1);
        self.selected_index = Some(next);
        self.refresh_preview();
    }

    fn push_query(&mut self, character: char) {
        self.query.push(character);
        self.clamp_selection();
    }

    fn pop_query(&mut self) {
        self.query.pop();
        self.clamp_selection();
    }

    fn clamp_selection(&mut self) {
        let len = self.filtered_notes().len();
        self.selected_index = if len == 0 {
            None
        } else {
            Some(self.selected_index.unwrap_or(0).min(len - 1))
        };
        self.refresh_preview();
    }

    fn refresh_preview(&mut self) {
        self.preview = self.selected_note().map_or_else(
            || vec!["No matches.".to_string()],
            |note| load_preview(&self.paths, &note.path),
        );
    }

    fn preview_lines(&self) -> Vec<Line<'static>> {
        self.preview
            .iter()
            .take(18)
            .map(|line| Line::from(line.clone()))
            .collect()
    }
}

fn fuzzy_score(note: &NoteIdentity, query: &str) -> Option<i32> {
    if query.trim().is_empty() {
        return Some(0);
    }

    let haystack =
        format!("{} {} {}", note.path, note.filename, note.aliases.join(" ")).to_lowercase();
    let needle = query.trim().to_lowercase();

    if haystack.contains(&needle) {
        let exact_path_bonus = if note.path.eq_ignore_ascii_case(&needle) {
            10_000
        } else {
            4_000
        };
        return Some(exact_path_bonus - i32::try_from(note.path.len()).unwrap_or(i32::MAX));
    }

    let mut score = 0_i32;
    let mut last_index = 0_usize;
    let mut streak = 0_i32;
    for character in needle.chars() {
        let offset = haystack[last_index..].find(character)?;
        let absolute = last_index + offset;
        if absolute == last_index {
            streak += 1;
            score += 25 + streak * 5;
        } else {
            streak = 0;
            score += 10;
        }
        if absolute == 0
            || matches!(
                haystack.as_bytes().get(absolute.saturating_sub(1)).copied(),
                Some(b'/' | b' ' | b'[')
            )
        {
            score += 20;
        }
        last_index = absolute + character.len_utf8();
    }

    Some(score)
}

fn load_preview(paths: &VaultPaths, relative_path: &str) -> Vec<String> {
    match fs::read_to_string(paths.vault_root().join(relative_path)) {
        Ok(contents) => contents
            .lines()
            .map(str::to_string)
            .collect::<Vec<_>>()
            .into_iter()
            .take(18)
            .collect(),
        Err(error) => vec![format!("Failed to load preview: {error}")],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn fuzzy_score_prefers_exact_and_substring_matches() {
        let home = note("Home.md", &["Start"]);
        let hub = note("Hub/Home Base.md", &[]);

        assert!(
            fuzzy_score(&home, "home").expect("home should match")
                > fuzzy_score(&hub, "home").expect("hub should match")
        );
        assert!(fuzzy_score(&home, "start").is_some());
    }

    #[test]
    fn fuzzy_score_rejects_non_matching_queries() {
        let note = note("Projects/Alpha.md", &["Initiative Alpha"]);

        assert!(fuzzy_score(&note, "zzz").is_none());
        assert!(fuzzy_score(&note, "alpha").is_some());
    }
}
