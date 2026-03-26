use crate::editor::{open_in_editor, with_terminal_suspended};
use crate::note_picker::{handle_picker_key, NotePickerState, PickerAction};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use std::io;
use std::time::Duration;
use vulcan_core::search::SearchMode;
use vulcan_core::{
    list_note_identities, scan_vault, search_vault, NoteIdentity, ScanMode, SearchHit, SearchQuery,
    VaultPaths,
};

const FULL_TEXT_LIMIT: usize = 200;
const FULL_TEXT_CONTEXT_SIZE: usize = 18;

pub fn run_browse_tui(paths: &VaultPaths) -> Result<(), io::Error> {
    if !paths.cache_db().exists() {
        scan_vault(paths, ScanMode::Incremental).map_err(io::Error::other)?;
    }

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
            match state.handle_key(key) {
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
                            if let Err(error) = state.reload_after_edit() {
                                state.set_status(error);
                            } else {
                                state.set_status(format!("Updated {path}."));
                            }
                        }
                        Err(error) => {
                            state.refresh_preview();
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

    let query = Paragraph::new(state.query().to_string()).block(
        Block::default()
            .title(state.query_title())
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Yellow)),
    );
    frame.render_widget(query, layout[0]);

    let items = state
        .list_items()
        .into_iter()
        .map(ListItem::new)
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
    list_state.select(state.selected_index());
    frame.render_stateful_widget(list, body[0], &mut list_state);

    let preview = Paragraph::new(state.preview_lines())
        .block(
            Block::default()
                .title(state.preview_title())
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(preview, body[1]);

    let footer = Paragraph::new(vec![
        Line::from("Keys: Enter/e edit, Ctrl-F full-text, / fuzzy, Esc quit"),
        Line::from(format!("      {}", state.mode_help_line())),
        Line::from(format!(
            "Mode: {} | {} filtered / {} total",
            state.mode.label(),
            state.filtered_count(),
            state.total_notes()
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BrowseMode {
    Fuzzy,
    FullText,
}

impl BrowseMode {
    fn label(self) -> &'static str {
        match self {
            Self::Fuzzy => "fuzzy",
            Self::FullText => "full-text",
        }
    }

    fn query_title(self) -> &'static str {
        match self {
            Self::Fuzzy => "Browse (/ fuzzy search)",
            Self::FullText => "Browse (Ctrl-F full-text)",
        }
    }

    fn help_line(self) -> &'static str {
        match self {
            Self::Fuzzy => "type to filter by path, filename, or alias",
            Self::FullText => "type to search indexed content; preview shows snippets",
        }
    }
}

#[derive(Debug, Clone)]
struct BrowseState {
    paths: VaultPaths,
    picker: NotePickerState,
    full_text: FullTextState,
    mode: BrowseMode,
    status: String,
}

impl BrowseState {
    fn new(paths: VaultPaths, notes: Vec<NoteIdentity>) -> Self {
        Self {
            paths: paths.clone(),
            picker: NotePickerState::new(paths, notes, ""),
            full_text: FullTextState::default(),
            mode: BrowseMode::Fuzzy,
            status: "Ready.".to_string(),
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> BrowseAction {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('f') | KeyCode::Char('F') => {
                    self.clear_status();
                    if let Err(error) = self.switch_mode(BrowseMode::FullText) {
                        self.set_status(error);
                    }
                    return BrowseAction::Continue;
                }
                _ => {}
            }
        }

        match key.code {
            KeyCode::Esc => BrowseAction::Quit,
            KeyCode::Char('/') if self.mode != BrowseMode::Fuzzy => {
                self.clear_status();
                self.mode = BrowseMode::Fuzzy;
                BrowseAction::Continue
            }
            KeyCode::Enter | KeyCode::Char('e') => {
                if let Some(path) = self.selected_path().map(str::to_string) {
                    BrowseAction::Edit(path)
                } else {
                    self.set_status("No matching note selected.");
                    BrowseAction::Continue
                }
            }
            _ => {
                self.clear_status();
                if let Err(error) = self.handle_mode_key(key.code) {
                    self.set_status(error);
                }
                BrowseAction::Continue
            }
        }
    }

    fn handle_mode_key(&mut self, code: KeyCode) -> Result<(), String> {
        match self.mode {
            BrowseMode::Fuzzy => {
                match handle_picker_key(&mut self.picker, code) {
                    PickerAction::Continue | PickerAction::Cancel | PickerAction::Select => {}
                }
                Ok(())
            }
            BrowseMode::FullText => self.full_text.handle_key(&self.paths, code),
        }
    }

    fn switch_mode(&mut self, mode: BrowseMode) -> Result<(), String> {
        self.mode = mode;
        if self.mode == BrowseMode::FullText {
            self.full_text.refresh_results(&self.paths)?;
        }
        Ok(())
    }

    fn reload_after_edit(&mut self) -> Result<(), String> {
        let notes = load_notes(&self.paths)?;
        self.picker.replace_notes_preserve_selection(notes);
        self.full_text.refresh_results(&self.paths)?;
        Ok(())
    }

    fn refresh_preview(&mut self) {
        match self.mode {
            BrowseMode::Fuzzy => self.picker.refresh_preview(),
            BrowseMode::FullText => {}
        }
    }

    fn selected_path(&self) -> Option<&str> {
        match self.mode {
            BrowseMode::Fuzzy => self.picker.selected_path(),
            BrowseMode::FullText => self.full_text.selected_path(),
        }
    }

    fn query(&self) -> &str {
        match self.mode {
            BrowseMode::Fuzzy => self.picker.query(),
            BrowseMode::FullText => self.full_text.query(),
        }
    }

    fn query_title(&self) -> &'static str {
        self.mode.query_title()
    }

    fn mode_help_line(&self) -> &'static str {
        self.mode.help_line()
    }

    fn list_items(&self) -> Vec<String> {
        match self.mode {
            BrowseMode::Fuzzy => self
                .picker
                .filtered_notes()
                .iter()
                .map(|(_, note)| {
                    let aliases = if note.aliases.is_empty() {
                        String::new()
                    } else {
                        format!(" [{}]", note.aliases.join(", "))
                    };
                    format!("{}{}", note.path, aliases)
                })
                .collect(),
            BrowseMode::FullText => self
                .full_text
                .hits
                .iter()
                .map(|hit| format!("{} [{:.3}]", search_hit_location(hit), hit.rank))
                .collect(),
        }
    }

    fn selected_index(&self) -> Option<usize> {
        match self.mode {
            BrowseMode::Fuzzy => self.picker.selected_index(),
            BrowseMode::FullText => self.full_text.selected_index(),
        }
    }

    fn preview_title(&self) -> String {
        match self.mode {
            BrowseMode::Fuzzy => self
                .picker
                .selected_path()
                .map_or_else(|| "Preview".to_string(), |path| format!("Preview: {path}")),
            BrowseMode::FullText => self.full_text.selected_hit().map_or_else(
                || "Snippet Preview".to_string(),
                |hit| format!("Snippet: {}", search_hit_location(hit)),
            ),
        }
    }

    fn preview_lines(&self) -> Vec<Line<'static>> {
        match self.mode {
            BrowseMode::Fuzzy => self.picker.preview_lines(),
            BrowseMode::FullText => self.full_text.preview_lines(),
        }
    }

    fn filtered_count(&self) -> usize {
        match self.mode {
            BrowseMode::Fuzzy => self.picker.filtered_count(),
            BrowseMode::FullText => self.full_text.filtered_count(),
        }
    }

    fn total_notes(&self) -> usize {
        self.picker.total_notes()
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

#[derive(Debug, Clone, Default)]
struct FullTextState {
    query: String,
    hits: Vec<SearchHit>,
    selected_index: Option<usize>,
}

impl FullTextState {
    fn query(&self) -> &str {
        &self.query
    }

    fn filtered_count(&self) -> usize {
        self.hits.len()
    }

    fn selected_index(&self) -> Option<usize> {
        self.selected_index
    }

    fn selected_path(&self) -> Option<&str> {
        self.selected_hit().map(|hit| hit.document_path.as_str())
    }

    fn selected_hit(&self) -> Option<&SearchHit> {
        self.selected_index.and_then(|index| self.hits.get(index))
    }

    fn preview_lines(&self) -> Vec<Line<'static>> {
        match self.selected_hit() {
            Some(hit) => snippet_preview_lines(&hit.snippet),
            None if self.query.trim().is_empty() => {
                vec![Line::from("Type to search note contents.")]
            }
            None => vec![Line::from("No full-text matches.")],
        }
    }

    fn handle_key(&mut self, paths: &VaultPaths, code: KeyCode) -> Result<(), String> {
        match code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_selection(-1);
                Ok(())
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_selection(1);
                Ok(())
            }
            KeyCode::Backspace => {
                self.query.pop();
                self.refresh_results(paths)
            }
            KeyCode::Char(character) => {
                self.query.push(character);
                self.refresh_results(paths)
            }
            _ => Ok(()),
        }
    }

    fn move_selection(&mut self, delta: isize) {
        let len = self.hits.len();
        if len == 0 {
            self.selected_index = None;
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
    }

    fn refresh_results(&mut self, paths: &VaultPaths) -> Result<(), String> {
        let selected_key = self
            .selected_hit()
            .map(|hit| (hit.chunk_id.clone(), hit.document_path.clone()));
        self.hits = self.query_results(paths)?;
        self.selected_index = selected_key.and_then(|(chunk_id, document_path)| {
            self.hits
                .iter()
                .position(|hit| hit.chunk_id == chunk_id && hit.document_path == document_path)
        });
        self.clamp_selection();
        Ok(())
    }

    fn query_results(&self, paths: &VaultPaths) -> Result<Vec<SearchHit>, String> {
        if self.query.trim().is_empty() {
            return Ok(Vec::new());
        }

        search_vault(
            paths,
            &SearchQuery {
                text: self.query.clone(),
                tag: None,
                path_prefix: None,
                has_property: None,
                filters: Vec::new(),
                provider: None,
                mode: SearchMode::Keyword,
                limit: Some(FULL_TEXT_LIMIT),
                context_size: FULL_TEXT_CONTEXT_SIZE,
                raw_query: false,
                fuzzy: false,
                explain: false,
            },
        )
        .map(|report| report.hits)
        .map_err(|error| error.to_string())
    }

    fn clamp_selection(&mut self) {
        let len = self.hits.len();
        self.selected_index = if len == 0 {
            None
        } else {
            Some(self.selected_index.unwrap_or(0).min(len - 1))
        };
    }
}

fn search_hit_location(hit: &SearchHit) -> String {
    if hit.heading_path.is_empty() {
        hit.document_path.clone()
    } else {
        format!("{} > {}", hit.document_path, hit.heading_path.join(" > "))
    }
}

fn snippet_preview_lines(snippet: &str) -> Vec<Line<'static>> {
    let lines = snippet
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(highlighted_snippet_line)
        .collect::<Vec<_>>();
    if lines.is_empty() {
        vec![Line::from("<empty>")]
    } else {
        lines
    }
}

fn highlighted_snippet_line(line: &str) -> Line<'static> {
    let mut spans = Vec::new();
    let mut remaining = line.trim();

    while let Some(start) = remaining.find('[') {
        let (before, after_start) = remaining.split_at(start);
        if !before.is_empty() {
            spans.push(Span::raw(before.to_string()));
        }
        let after_start = &after_start[1..];
        if let Some(end) = after_start.find(']') {
            let (highlighted, after_end) = after_start.split_at(end);
            spans.push(Span::styled(
                highlighted.to_string(),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ));
            remaining = &after_end[1..];
        } else {
            spans.push(Span::raw(format!("[{after_start}")));
            remaining = "";
        }
    }

    if !remaining.is_empty() {
        spans.push(Span::raw(remaining.to_string()));
    }

    Line::from(spans)
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

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(code: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(code), KeyModifiers::CONTROL)
    }

    fn write_note(root: &Path, relative_path: &str, contents: &str) {
        let absolute = root.join(relative_path);
        if let Some(parent) = absolute.parent() {
            fs::create_dir_all(parent).expect("note parent should be created");
        }
        fs::write(absolute, contents).expect("note should be written");
    }

    fn scan_fixture(paths: &VaultPaths) {
        scan_vault(paths, ScanMode::Full).expect("vault scan should succeed");
    }

    #[test]
    fn enter_requests_edit_for_selected_note() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        write_note(temp_dir.path(), "Projects/Alpha.md", "Alpha");
        let mut state = BrowseState::new(
            paths,
            vec![
                note("Projects/Alpha.md", &[]),
                note("Projects/Beta.md", &[]),
            ],
        );
        state.handle_key(key(KeyCode::Char('a')));
        state.handle_key(key(KeyCode::Char('l')));
        state.handle_key(key(KeyCode::Char('p')));
        state.handle_key(key(KeyCode::Char('h')));
        state.handle_key(key(KeyCode::Char('a')));

        let action = state.handle_key(key(KeyCode::Enter));

        assert_eq!(action, BrowseAction::Edit("Projects/Alpha.md".to_string()));
        assert_eq!(state.query(), "alpha");
        assert_eq!(state.selected_path(), Some("Projects/Alpha.md"));
    }

    #[test]
    fn e_requests_edit_for_selected_note() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        write_note(temp_dir.path(), "Projects/Alpha.md", "Alpha");
        let mut state = BrowseState::new(paths, vec![note("Projects/Alpha.md", &[])]);

        let action = state.handle_key(key(KeyCode::Char('e')));

        assert_eq!(action, BrowseAction::Edit("Projects/Alpha.md".to_string()));
    }

    #[test]
    fn ctrl_f_switches_to_full_text_mode() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        write_note(temp_dir.path(), "Projects/Alpha.md", "release dashboard");
        scan_fixture(&paths);
        let mut state = BrowseState::new(paths, vec![note("Projects/Alpha.md", &[])]);

        let action = state.handle_key(ctrl('f'));

        assert_eq!(action, BrowseAction::Continue);
        assert_eq!(state.mode, BrowseMode::FullText);
        assert_eq!(state.query_title(), "Browse (Ctrl-F full-text)");
    }

    #[test]
    fn full_text_query_returns_hits_and_snippet_preview() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        write_note(
            temp_dir.path(),
            "Projects/Alpha.md",
            "# Status\nrelease dashboard summary",
        );
        write_note(temp_dir.path(), "Projects/Beta.md", "planning notes");
        scan_fixture(&paths);
        let mut state = BrowseState::new(
            paths,
            vec![
                note("Projects/Alpha.md", &[]),
                note("Projects/Beta.md", &[]),
            ],
        );

        state.handle_key(ctrl('f'));
        for character in "dashboard".chars() {
            state.handle_key(key(KeyCode::Char(character)));
        }

        assert_eq!(state.mode, BrowseMode::FullText);
        assert_eq!(state.filtered_count(), 1);
        assert_eq!(state.selected_path(), Some("Projects/Alpha.md"));
        assert!(state.preview_title().contains("Projects/Alpha.md"));
        assert_eq!(state.list_items().len(), 1);
        assert!(state.list_items()[0].contains("Projects/Alpha.md"));

        let preview = state.preview_lines();
        assert!(!preview.is_empty());
        assert!(preview
            .iter()
            .flat_map(|line| line.spans.iter())
            .any(|span| span.content == "dashboard"));
    }

    #[test]
    fn slash_returns_to_fuzzy_mode() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        write_note(temp_dir.path(), "Projects/Alpha.md", "release dashboard");
        scan_fixture(&paths);
        let mut state = BrowseState::new(paths, vec![note("Projects/Alpha.md", &[])]);

        state.handle_key(ctrl('f'));
        let action = state.handle_key(key(KeyCode::Char('/')));

        assert_eq!(action, BrowseAction::Continue);
        assert_eq!(state.mode, BrowseMode::Fuzzy);
        assert_eq!(state.query_title(), "Browse (/ fuzzy search)");
    }

    #[test]
    fn highlighted_snippet_parser_marks_bracketed_terms() {
        let line = highlighted_snippet_line("alpha [dashboard] omega");

        assert_eq!(line.spans.len(), 3);
        assert_eq!(line.spans[1].content, "dashboard");
    }
}
