use crate::bases_tui;
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
use std::fs;
use std::io;
use std::time::{Duration, SystemTime};
use vulcan_core::paths::{normalize_relative_input_path, RelativePathOptions};
use vulcan_core::search::SearchMode;
use vulcan_core::{
    doctor_vault, evaluate_base_file, git_log, is_git_repo, list_note_identities,
    list_tagged_note_identities, list_tags, move_note, query_backlinks, query_links, query_notes,
    scan_vault, search_vault, BacklinkRecord, DoctorDiagnosticIssue, DoctorLinkIssue, GitLogEntry,
    NamedCount, NoteIdentity, NoteQuery, OutgoingLinkRecord, ResolutionStatus, ScanMode, SearchHit,
    SearchQuery, VaultPaths,
};

const FULL_TEXT_LIMIT: usize = 200;
const FULL_TEXT_CONTEXT_SIZE: usize = 18;

pub fn run_browse_tui(paths: &VaultPaths) -> Result<(), io::Error> {
    if !paths.cache_db().exists() {
        scan_vault(paths, ScanMode::Incremental).map_err(io::Error::other)?;
    }

    let mut state = BrowseState::new(paths.clone(), load_notes(paths).map_err(io::Error::other)?)
        .map_err(io::Error::other)?;

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
                BrowseAction::OpenBaseTui(path) => {
                    let paths = state.paths.clone();
                    let open_result = with_terminal_suspended(terminal, || {
                        let report =
                            evaluate_base_file(&paths, &path).map_err(|error| error.to_string())?;
                        bases_tui::run_bases_tui(&paths, &path, &report)
                            .map_err(|error| error.to_string())?;
                        scan_vault(&paths, ScanMode::Incremental)
                            .map(|_| ())
                            .map_err(|error| error.to_string())
                    });
                    match open_result {
                        Ok(()) => {
                            if let Err(error) = state.reload_after_edit() {
                                state.set_status(error);
                            } else {
                                state.set_status(format!("Opened bases TUI for {path}."));
                            }
                        }
                        Err(error) => state.set_status(error.to_string()),
                    }
                }
                BrowseAction::Create(path) => {
                    let relative_path = match normalize_relative_input_path(
                        &path,
                        RelativePathOptions {
                            expected_extension: Some("md"),
                            append_extension_if_missing: true,
                        },
                    ) {
                        Ok(path) => path,
                        Err(error) => {
                            state.set_status(error.to_string());
                            continue;
                        }
                    };
                    let paths = state.paths.clone();
                    let create_result = with_terminal_suspended(terminal, || {
                        let absolute = paths.vault_root().join(&relative_path);
                        if let Some(parent) = absolute.parent() {
                            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
                        }
                        if !absolute.exists() {
                            fs::write(&absolute, "").map_err(|error| error.to_string())?;
                        }
                        open_in_editor(&absolute)?;
                        scan_vault(&paths, ScanMode::Incremental)
                            .map(|_| ())
                            .map_err(|error| error.to_string())
                    });
                    match create_result {
                        Ok(()) => {
                            state.clear_new_note_prompt();
                            if let Err(error) = state.reload_after_new_note(&relative_path) {
                                state.set_status(error);
                            } else {
                                state.set_status(format!("Created {relative_path}."));
                            }
                        }
                        Err(error) => state.set_status(error.to_string()),
                    }
                }
                BrowseAction::Move {
                    source_path,
                    destination,
                } => match move_note(&state.paths, &source_path, &destination, false) {
                    Ok(summary) => {
                        state.clear_move_prompt();
                        if let Err(error) = state.reload_after_move(&summary.destination_path) {
                            state.set_status(error);
                        } else {
                            state.set_status(format!(
                                "Moved {} -> {}.",
                                summary.source_path, summary.destination_path
                            ));
                        }
                    }
                    Err(error) => state.set_status(error.to_string()),
                },
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
                .title(state.list_title())
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
        Line::from(state.status_bar_line()),
        Line::from(state.key_help_line()),
        Line::from(format!("      {}", state.mode_help_line())),
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
    OpenBaseTui(String),
    Create(String),
    Move {
        source_path: String,
        destination: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BrowseMode {
    Fuzzy,
    FullText,
    Tag,
    Property,
}

impl BrowseMode {
    fn label(self) -> &'static str {
        match self {
            Self::Fuzzy => "fuzzy",
            Self::FullText => "full-text",
            Self::Tag => "tag",
            Self::Property => "property",
        }
    }

    fn query_title(self) -> &'static str {
        match self {
            Self::Fuzzy => "Browse (/ fuzzy search)",
            Self::FullText => "Browse (Ctrl-F full-text)",
            Self::Tag => "Browse (Ctrl-T tag filter)",
            Self::Property => "Browse (Ctrl-P property filter)",
        }
    }

    fn help_line(self) -> &'static str {
        match self {
            Self::Fuzzy => "type to filter by path, filename, or alias",
            Self::FullText => "type to search indexed content; preview shows snippets",
            Self::Tag => "type a tag name; notes show the best matching indexed tag",
            Self::Property => "type a where-style predicate like status = active",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MovePrompt {
    source_path: String,
    destination: String,
}

impl MovePrompt {
    fn new(source_path: &str) -> Self {
        Self {
            source_path: source_path.to_string(),
            destination: source_path.to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct NewNotePrompt {
    path: String,
}

#[derive(Debug, Clone)]
struct BrowseState {
    paths: VaultPaths,
    all_notes: Vec<NoteIdentity>,
    picker: NotePickerState,
    full_text: FullTextState,
    tag_filter: TagFilterState,
    property_filter: PropertyFilterState,
    backlinks_view: Option<BacklinksViewState>,
    links_view: Option<OutgoingLinksViewState>,
    git_view: Option<GitLogViewState>,
    doctor_view: Option<DoctorViewState>,
    new_note_prompt: Option<NewNotePrompt>,
    move_prompt: Option<MovePrompt>,
    last_scan_label: String,
    mode: BrowseMode,
    status: String,
}

impl BrowseState {
    fn new(paths: VaultPaths, notes: Vec<NoteIdentity>) -> Result<Self, String> {
        let tags = if paths.cache_db().exists() {
            list_tags(&paths).map_err(|error| error.to_string())?
        } else {
            Vec::new()
        };
        let last_scan_label = last_scan_label(&paths);
        Ok(Self {
            paths: paths.clone(),
            all_notes: notes.clone(),
            picker: NotePickerState::new(paths.clone(), notes.clone(), ""),
            full_text: FullTextState::default(),
            tag_filter: TagFilterState::new(paths.clone(), tags),
            property_filter: PropertyFilterState::new(paths),
            backlinks_view: None,
            links_view: None,
            git_view: None,
            doctor_view: None,
            new_note_prompt: None,
            move_prompt: None,
            last_scan_label,
            mode: BrowseMode::Fuzzy,
            status: "Ready.".to_string(),
        })
    }

    fn handle_key(&mut self, key: KeyEvent) -> BrowseAction {
        if self.new_note_prompt.is_some() {
            return self.handle_new_note_prompt_key(key.code);
        }
        if self.move_prompt.is_some() {
            return self.handle_move_prompt_key(key.code);
        }
        if self.git_view.is_some() {
            return self.handle_git_key(key.code);
        }
        if self.doctor_view.is_some() {
            return self.handle_doctor_key(key.code);
        }
        if self.backlinks_view.is_some() {
            return self.handle_backlinks_key(key.code);
        }
        if self.links_view.is_some() {
            return self.handle_links_key(key.code);
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('f') | KeyCode::Char('F') => {
                    self.clear_status();
                    if let Err(error) = self.switch_mode(BrowseMode::FullText) {
                        self.set_status(error);
                    }
                    return BrowseAction::Continue;
                }
                KeyCode::Char('t') | KeyCode::Char('T') => {
                    self.clear_status();
                    if let Err(error) = self.switch_mode(BrowseMode::Tag) {
                        self.set_status(error);
                    }
                    return BrowseAction::Continue;
                }
                KeyCode::Char('p') | KeyCode::Char('P') => {
                    self.clear_status();
                    if let Err(error) = self.switch_mode(BrowseMode::Property) {
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
            KeyCode::Char('b')
                if self.mode == BrowseMode::Fuzzy && self.picker.query().is_empty() =>
            {
                self.clear_status();
                if let Err(error) = self.open_backlinks_view() {
                    self.set_status(error);
                }
                BrowseAction::Continue
            }
            KeyCode::Char('l')
                if self.mode == BrowseMode::Fuzzy && self.picker.query().is_empty() =>
            {
                self.clear_status();
                if let Err(error) = self.open_links_view() {
                    self.set_status(error);
                }
                BrowseAction::Continue
            }
            KeyCode::Char('m')
                if self.mode == BrowseMode::Fuzzy && self.picker.query().is_empty() =>
            {
                self.clear_status();
                self.open_move_prompt();
                BrowseAction::Continue
            }
            KeyCode::Char('d')
                if self.mode == BrowseMode::Fuzzy && self.picker.query().is_empty() =>
            {
                self.clear_status();
                if let Err(error) = self.open_doctor_view() {
                    self.set_status(error);
                }
                BrowseAction::Continue
            }
            KeyCode::Char('g')
                if self.mode == BrowseMode::Fuzzy && self.picker.query().is_empty() =>
            {
                self.clear_status();
                if let Err(error) = self.open_git_view() {
                    self.set_status(error);
                }
                BrowseAction::Continue
            }
            KeyCode::Char('n')
                if self.mode == BrowseMode::Fuzzy && self.picker.query().is_empty() =>
            {
                self.clear_status();
                self.new_note_prompt = Some(NewNotePrompt::default());
                BrowseAction::Continue
            }
            KeyCode::Enter | KeyCode::Char('e')
                if self.mode == BrowseMode::Fuzzy
                    && (matches!(key.code, KeyCode::Enter) || self.picker.query().is_empty()) =>
            {
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

    fn handle_git_key(&mut self, code: KeyCode) -> BrowseAction {
        let Some(view) = self.git_view.as_mut() else {
            return BrowseAction::Continue;
        };

        match code {
            KeyCode::Esc => {
                self.git_view = None;
                self.clear_status();
                BrowseAction::Continue
            }
            KeyCode::Up | KeyCode::Char('k') => {
                view.move_selection(-1);
                BrowseAction::Continue
            }
            KeyCode::Down | KeyCode::Char('j') => {
                view.move_selection(1);
                BrowseAction::Continue
            }
            _ => BrowseAction::Continue,
        }
    }

    fn handle_doctor_key(&mut self, code: KeyCode) -> BrowseAction {
        let Some(view) = self.doctor_view.as_mut() else {
            return BrowseAction::Continue;
        };

        match code {
            KeyCode::Esc => {
                self.doctor_view = None;
                self.clear_status();
                BrowseAction::Continue
            }
            KeyCode::Up | KeyCode::Char('k') => {
                view.move_selection(-1);
                BrowseAction::Continue
            }
            KeyCode::Down | KeyCode::Char('j') => {
                view.move_selection(1);
                BrowseAction::Continue
            }
            _ => BrowseAction::Continue,
        }
    }

    fn handle_new_note_prompt_key(&mut self, code: KeyCode) -> BrowseAction {
        let Some(prompt) = self.new_note_prompt.as_mut() else {
            return BrowseAction::Continue;
        };

        match code {
            KeyCode::Esc => {
                self.new_note_prompt = None;
                self.clear_status();
                BrowseAction::Continue
            }
            KeyCode::Enter => {
                let path = prompt.path.trim().to_string();
                if path.is_empty() {
                    self.set_status("New note path cannot be empty.");
                    return BrowseAction::Continue;
                }
                BrowseAction::Create(path)
            }
            KeyCode::Backspace => {
                prompt.path.pop();
                BrowseAction::Continue
            }
            KeyCode::Char(character) => {
                prompt.path.push(character);
                BrowseAction::Continue
            }
            _ => BrowseAction::Continue,
        }
    }

    fn handle_backlinks_key(&mut self, code: KeyCode) -> BrowseAction {
        let Some(view) = self.backlinks_view.as_mut() else {
            return BrowseAction::Continue;
        };

        match code {
            KeyCode::Esc => {
                self.backlinks_view = None;
                self.clear_status();
                BrowseAction::Continue
            }
            KeyCode::Char('o') => match view.selected_path() {
                Some(path) if is_base_path(path) => BrowseAction::OpenBaseTui(path.to_string()),
                _ => {
                    self.set_status("Selected file is not a .base file.");
                    BrowseAction::Continue
                }
            },
            KeyCode::Enter | KeyCode::Char('e') => {
                if let Some(path) = view.selected_path().map(str::to_string) {
                    BrowseAction::Edit(path)
                } else {
                    self.set_status("No backlink source selected.");
                    BrowseAction::Continue
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                view.move_selection(-1);
                BrowseAction::Continue
            }
            KeyCode::Down | KeyCode::Char('j') => {
                view.move_selection(1);
                BrowseAction::Continue
            }
            _ => BrowseAction::Continue,
        }
    }

    fn handle_links_key(&mut self, code: KeyCode) -> BrowseAction {
        let Some(view) = self.links_view.as_mut() else {
            return BrowseAction::Continue;
        };

        match code {
            KeyCode::Esc => {
                self.links_view = None;
                self.clear_status();
                BrowseAction::Continue
            }
            KeyCode::Char('o') => match view.selected_path() {
                Some(path) if is_base_path(path) => BrowseAction::OpenBaseTui(path.to_string()),
                _ => {
                    self.set_status("Selected file is not a .base file.");
                    BrowseAction::Continue
                }
            },
            KeyCode::Enter | KeyCode::Char('e') => {
                if let Some(path) = view.selected_path().map(str::to_string) {
                    BrowseAction::Edit(path)
                } else {
                    self.set_status("Selected link does not resolve to a note.");
                    BrowseAction::Continue
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                view.move_selection(-1);
                BrowseAction::Continue
            }
            KeyCode::Down | KeyCode::Char('j') => {
                view.move_selection(1);
                BrowseAction::Continue
            }
            _ => BrowseAction::Continue,
        }
    }

    fn handle_move_prompt_key(&mut self, code: KeyCode) -> BrowseAction {
        let Some(prompt) = self.move_prompt.as_mut() else {
            return BrowseAction::Continue;
        };

        match code {
            KeyCode::Esc => {
                self.move_prompt = None;
                self.clear_status();
                BrowseAction::Continue
            }
            KeyCode::Enter => {
                let destination = prompt.destination.trim().to_string();
                if destination.is_empty() {
                    self.set_status("Destination path cannot be empty.");
                    return BrowseAction::Continue;
                }
                BrowseAction::Move {
                    source_path: prompt.source_path.clone(),
                    destination,
                }
            }
            KeyCode::Backspace => {
                prompt.destination.pop();
                BrowseAction::Continue
            }
            KeyCode::Char(character) => {
                prompt.destination.push(character);
                BrowseAction::Continue
            }
            _ => BrowseAction::Continue,
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
            BrowseMode::Tag => self
                .tag_filter
                .handle_key(&self.paths, &self.all_notes, code),
            BrowseMode::Property => {
                self.property_filter
                    .handle_key(&self.paths, &self.all_notes, code)
            }
        }
    }

    fn switch_mode(&mut self, mode: BrowseMode) -> Result<(), String> {
        self.mode = mode;
        match self.mode {
            BrowseMode::FullText => self.full_text.refresh_results(&self.paths)?,
            BrowseMode::Tag => self
                .tag_filter
                .refresh_results(&self.paths, &self.all_notes)?,
            BrowseMode::Property => self
                .property_filter
                .refresh_results(&self.paths, &self.all_notes),
            BrowseMode::Fuzzy => {}
        }
        Ok(())
    }

    fn reload_after_edit(&mut self) -> Result<(), String> {
        let notes = load_notes(&self.paths)?;
        self.all_notes = notes.clone();
        self.picker.replace_notes_preserve_selection(notes);
        self.full_text.refresh_results(&self.paths)?;
        self.tag_filter
            .refresh_results(&self.paths, &self.all_notes)?;
        self.property_filter
            .refresh_results(&self.paths, &self.all_notes);
        if let Some(view) = self.git_view.as_mut() {
            view.reload(self.paths.vault_root())?;
        }
        if let Some(view) = self.doctor_view.as_mut() {
            view.reload(&self.paths)?;
        }
        if let Some(view) = self.backlinks_view.as_mut() {
            view.reload(&self.paths)?;
        }
        if let Some(view) = self.links_view.as_mut() {
            view.reload(&self.paths)?;
        }
        self.refresh_last_scan_label();
        Ok(())
    }

    fn reload_after_move(&mut self, destination: &str) -> Result<(), String> {
        self.reload_after_new_note(destination)
    }

    fn reload_after_new_note(&mut self, path: &str) -> Result<(), String> {
        let notes = load_notes(&self.paths)?;
        self.all_notes = notes.clone();
        self.picker.replace_notes_preserve_selection(notes);
        self.picker.select_path(path);
        self.full_text.refresh_results(&self.paths)?;
        self.full_text.select_path(path);
        self.tag_filter
            .refresh_results(&self.paths, &self.all_notes)?;
        self.tag_filter.select_path(path);
        self.property_filter
            .refresh_results(&self.paths, &self.all_notes);
        self.property_filter.select_path(path);
        self.refresh_last_scan_label();
        Ok(())
    }

    fn refresh_preview(&mut self) {
        match self.mode {
            BrowseMode::Fuzzy => self.picker.refresh_preview(),
            BrowseMode::FullText => {}
            BrowseMode::Tag => self.tag_filter.refresh_preview(),
            BrowseMode::Property => self.property_filter.refresh_preview(),
        }
    }

    fn selected_path(&self) -> Option<&str> {
        if let Some(view) = self.git_view.as_ref() {
            return Some(view.path());
        }
        if let Some(view) = self.doctor_view.as_ref() {
            return Some(view.note_path());
        }
        if let Some(view) = self.backlinks_view.as_ref() {
            return view.selected_path();
        }
        if let Some(view) = self.links_view.as_ref() {
            return view.selected_path();
        }
        match self.mode {
            BrowseMode::Fuzzy => self.picker.selected_path(),
            BrowseMode::FullText => self.full_text.selected_path(),
            BrowseMode::Tag => self.tag_filter.selected_path(),
            BrowseMode::Property => self.property_filter.selected_path(),
        }
    }

    fn query(&self) -> &str {
        if let Some(view) = self.git_view.as_ref() {
            return view.path();
        }
        if let Some(view) = self.doctor_view.as_ref() {
            return view.note_path();
        }
        if let Some(view) = self.backlinks_view.as_ref() {
            return view.note_path();
        }
        if let Some(view) = self.links_view.as_ref() {
            return view.note_path();
        }
        if let Some(prompt) = self.new_note_prompt.as_ref() {
            return &prompt.path;
        }
        if let Some(prompt) = self.move_prompt.as_ref() {
            return &prompt.destination;
        }
        match self.mode {
            BrowseMode::Fuzzy => self.picker.query(),
            BrowseMode::FullText => self.full_text.query(),
            BrowseMode::Tag => self.tag_filter.query(),
            BrowseMode::Property => self.property_filter.query(),
        }
    }

    fn query_title(&self) -> String {
        if let Some(view) = self.git_view.as_ref() {
            return format!("Git Log ({})", view.path());
        }
        if let Some(view) = self.doctor_view.as_ref() {
            return format!("Doctor ({})", view.note_path());
        }
        if let Some(view) = self.backlinks_view.as_ref() {
            return format!("Backlinks ({})", view.note_path());
        }
        if let Some(view) = self.links_view.as_ref() {
            return format!("Outgoing Links ({})", view.note_path());
        }
        if self.new_note_prompt.is_some() {
            return "New Note".to_string();
        }
        self.move_prompt.as_ref().map_or_else(
            || self.mode.query_title().to_string(),
            |prompt| format!("Move Note ({})", prompt.source_path),
        )
    }

    fn key_help_line(&self) -> String {
        if self.git_view.is_some() {
            return "Keys: Esc back, j/k move".to_string();
        }
        if self.doctor_view.is_some() {
            return "Keys: Esc back, j/k move".to_string();
        }
        if self.backlinks_view.is_some() {
            return "Keys: Enter/e edit source note, o open base, Esc back, j/k move".to_string();
        }
        if self.links_view.is_some() {
            return "Keys: Enter/e edit target note, o open base, Esc back, j/k move".to_string();
        }
        if self.new_note_prompt.is_some() {
            return "Keys: Enter create, Esc cancel, Backspace edit path".to_string();
        }
        if self.move_prompt.is_some() {
            "Keys: Enter move, Esc cancel, Backspace edit destination".to_string()
        } else {
            "Keys: Enter/e edit, n new, m move, b backlinks, l links, d doctor, g git, Ctrl-F full-text, Ctrl-T tags, Ctrl-P props, / fuzzy, Esc quit".to_string()
        }
    }

    fn mode_help_line(&self) -> String {
        if self.git_view.is_some() {
            return "view git history for the selected file; Esc returns to browse".to_string();
        }
        if self.doctor_view.is_some() {
            return "view doctor diagnostics for the selected note; Esc returns to browse"
                .to_string();
        }
        if self.backlinks_view.is_some() {
            return "view notes that link to the selected note; Esc returns to browse".to_string();
        }
        if self.links_view.is_some() {
            return "view links from the selected note; Esc returns to browse".to_string();
        }
        if self.new_note_prompt.is_some() {
            return "type a relative note path like Inbox/Idea.md, then press Enter".to_string();
        }
        self.move_prompt.as_ref().map_or_else(
            || self.mode.help_line().to_string(),
            |_| "type the destination path, then press Enter to rename or move".to_string(),
        )
    }

    fn list_title(&self) -> &'static str {
        if self.git_view.is_some() {
            "Git Log"
        } else if self.doctor_view.is_some() {
            "Diagnostics"
        } else if self.backlinks_view.is_some() {
            "Backlinks"
        } else if self.links_view.is_some() {
            "Links"
        } else {
            "Notes"
        }
    }

    fn list_items(&self) -> Vec<String> {
        if let Some(view) = self.git_view.as_ref() {
            return view.list_items();
        }
        if let Some(view) = self.doctor_view.as_ref() {
            return view.list_items();
        }
        if let Some(view) = self.backlinks_view.as_ref() {
            return view.list_items();
        }
        if let Some(view) = self.links_view.as_ref() {
            return view.list_items();
        }
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
            BrowseMode::Tag => self.tag_filter.list_items(),
            BrowseMode::Property => self.property_filter.list_items(),
        }
    }

    fn selected_index(&self) -> Option<usize> {
        if let Some(view) = self.git_view.as_ref() {
            return view.selected_index();
        }
        if let Some(view) = self.doctor_view.as_ref() {
            return view.selected_index();
        }
        if let Some(view) = self.backlinks_view.as_ref() {
            return view.selected_index();
        }
        if let Some(view) = self.links_view.as_ref() {
            return view.selected_index();
        }
        match self.mode {
            BrowseMode::Fuzzy => self.picker.selected_index(),
            BrowseMode::FullText => self.full_text.selected_index(),
            BrowseMode::Tag => self.tag_filter.selected_index(),
            BrowseMode::Property => self.property_filter.selected_index(),
        }
    }

    fn preview_title(&self) -> String {
        if let Some(view) = self.git_view.as_ref() {
            return view.preview_title();
        }
        if let Some(view) = self.doctor_view.as_ref() {
            return view.preview_title();
        }
        if let Some(view) = self.backlinks_view.as_ref() {
            return view.preview_title();
        }
        if let Some(view) = self.links_view.as_ref() {
            return view.preview_title();
        }
        match self.mode {
            BrowseMode::Fuzzy => self
                .picker
                .selected_path()
                .map_or_else(|| "Preview".to_string(), |path| format!("Preview: {path}")),
            BrowseMode::FullText => self.full_text.selected_hit().map_or_else(
                || "Snippet Preview".to_string(),
                |hit| format!("Snippet: {}", search_hit_location(hit)),
            ),
            BrowseMode::Tag => self.tag_filter.preview_title(),
            BrowseMode::Property => self.property_filter.preview_title(),
        }
    }

    fn preview_lines(&self) -> Vec<Line<'static>> {
        if let Some(view) = self.git_view.as_ref() {
            return view.preview_lines();
        }
        if let Some(view) = self.doctor_view.as_ref() {
            return view.preview_lines();
        }
        if let Some(view) = self.backlinks_view.as_ref() {
            return view.preview_lines();
        }
        if let Some(view) = self.links_view.as_ref() {
            return view.preview_lines();
        }
        match self.mode {
            BrowseMode::Fuzzy => self.picker.preview_lines(),
            BrowseMode::FullText => self.full_text.preview_lines(),
            BrowseMode::Tag => self.tag_filter.preview_lines(),
            BrowseMode::Property => self.property_filter.preview_lines(),
        }
    }

    fn filtered_count(&self) -> usize {
        if let Some(view) = self.git_view.as_ref() {
            return view.filtered_count();
        }
        if let Some(view) = self.doctor_view.as_ref() {
            return view.filtered_count();
        }
        if let Some(view) = self.backlinks_view.as_ref() {
            return view.filtered_count();
        }
        if let Some(view) = self.links_view.as_ref() {
            return view.filtered_count();
        }
        match self.mode {
            BrowseMode::Fuzzy => self.picker.filtered_count(),
            BrowseMode::FullText => self.full_text.filtered_count(),
            BrowseMode::Tag => self.tag_filter.filtered_count(),
            BrowseMode::Property => self.property_filter.filtered_count(),
        }
    }

    fn total_notes(&self) -> usize {
        self.picker.total_notes()
    }

    fn status_bar_line(&self) -> String {
        format!(
            "Vault: {} | Mode: {} | Notes: {} filtered / {} total | Last scan: {}",
            self.vault_name(),
            self.active_mode_label(),
            self.filtered_count(),
            self.total_notes(),
            self.last_scan_label
        )
    }

    fn set_status(&mut self, status: impl Into<String>) {
        self.status = status.into();
    }

    fn clear_status(&mut self) {
        self.status.clear();
    }

    fn refresh_last_scan_label(&mut self) {
        self.last_scan_label = last_scan_label(&self.paths);
    }

    fn clear_move_prompt(&mut self) {
        self.move_prompt = None;
    }

    fn clear_new_note_prompt(&mut self) {
        self.new_note_prompt = None;
    }

    fn open_move_prompt(&mut self) {
        match self.selected_path() {
            Some(path) => self.move_prompt = Some(MovePrompt::new(path)),
            None => self.set_status("No matching note selected."),
        }
    }

    fn open_backlinks_view(&mut self) -> Result<(), String> {
        let Some(path) = self.selected_path().map(str::to_string) else {
            self.set_status("No matching note selected.");
            return Ok(());
        };
        self.backlinks_view = Some(BacklinksViewState::load(&self.paths, &path)?);
        self.links_view = None;
        self.git_view = None;
        self.doctor_view = None;
        Ok(())
    }

    fn open_links_view(&mut self) -> Result<(), String> {
        let Some(path) = self.selected_path().map(str::to_string) else {
            self.set_status("No matching note selected.");
            return Ok(());
        };
        self.links_view = Some(OutgoingLinksViewState::load(&self.paths, &path)?);
        self.backlinks_view = None;
        self.git_view = None;
        self.doctor_view = None;
        Ok(())
    }

    fn open_doctor_view(&mut self) -> Result<(), String> {
        let Some(path) = self.selected_path().map(str::to_string) else {
            self.set_status("No matching note selected.");
            return Ok(());
        };
        self.doctor_view = Some(DoctorViewState::load(&self.paths, &path)?);
        self.backlinks_view = None;
        self.links_view = None;
        self.git_view = None;
        Ok(())
    }

    fn open_git_view(&mut self) -> Result<(), String> {
        if !is_git_repo(self.paths.vault_root()) {
            return Err("Vault is not a git repo.".to_string());
        }
        let Some(path) = self.selected_path().map(str::to_string) else {
            self.set_status("No matching note selected.");
            return Ok(());
        };
        self.git_view = Some(GitLogViewState::load(self.paths.vault_root(), &path)?);
        self.backlinks_view = None;
        self.links_view = None;
        self.doctor_view = None;
        Ok(())
    }

    fn status_line(&self) -> String {
        if !self.status.is_empty() {
            return self.status.clone();
        }

        if let Some(prompt) = self.move_prompt.as_ref() {
            return format!("Moving {}", prompt.source_path);
        }
        if self.new_note_prompt.is_some() {
            return "Creating new note".to_string();
        }
        if let Some(view) = self.git_view.as_ref() {
            return format!("Git log for {}", view.path());
        }
        if let Some(view) = self.doctor_view.as_ref() {
            return format!("Doctor for {}", view.note_path());
        }
        if let Some(view) = self.backlinks_view.as_ref() {
            return format!("Backlinks for {}", view.note_path());
        }
        if let Some(view) = self.links_view.as_ref() {
            return format!("Outgoing links for {}", view.note_path());
        }

        match self.mode {
            BrowseMode::Tag => self
                .tag_filter
                .active_tag
                .as_deref()
                .map_or_else(|| "Ready.".to_string(), |tag| format!("Tag: #{tag}")),
            BrowseMode::Property => self.property_filter.status_line(),
            BrowseMode::Fuzzy | BrowseMode::FullText => "Ready.".to_string(),
        }
    }

    fn vault_name(&self) -> String {
        self.paths
            .vault_root()
            .file_name()
            .and_then(|name| name.to_str())
            .map_or_else(
                || self.paths.vault_root().display().to_string(),
                ToOwned::to_owned,
            )
    }

    fn active_mode_label(&self) -> &'static str {
        if self.doctor_view.is_some() {
            "doctor"
        } else if self.backlinks_view.is_some() {
            "backlinks"
        } else if self.links_view.is_some() {
            "links"
        } else if self.new_note_prompt.is_some() {
            "new-note"
        } else if self.move_prompt.is_some() {
            "move"
        } else {
            self.mode.label()
        }
    }
}

#[derive(Debug, Clone)]
struct GitLogViewState {
    path: String,
    entries: Vec<GitLogEntry>,
    selected_index: Option<usize>,
}

impl GitLogViewState {
    fn load(vault_root: &std::path::Path, path: &str) -> Result<Self, String> {
        let entries = git_log(vault_root, path, 100).map_err(|error| error.to_string())?;
        let selected_index = (!entries.is_empty()).then_some(0);
        Ok(Self {
            path: path.to_string(),
            entries,
            selected_index,
        })
    }

    fn reload(&mut self, vault_root: &std::path::Path) -> Result<(), String> {
        let selected_commit = self.selected_entry().map(|entry| entry.commit.clone());
        self.entries = git_log(vault_root, &self.path, 100).map_err(|error| error.to_string())?;
        self.selected_index = selected_commit
            .and_then(|commit| self.entries.iter().position(|entry| entry.commit == commit));
        self.clamp_selection();
        Ok(())
    }

    fn path(&self) -> &str {
        &self.path
    }

    fn filtered_count(&self) -> usize {
        self.entries.len()
    }

    fn selected_index(&self) -> Option<usize> {
        self.selected_index
    }

    fn preview_title(&self) -> String {
        self.selected_entry().map_or_else(
            || format!("Git Log: {}", self.path),
            |entry| format!("Commit: {}", short_commit(&entry.commit)),
        )
    }

    fn preview_lines(&self) -> Vec<Line<'static>> {
        let Some(entry) = self.selected_entry() else {
            return vec![Line::from("No git history for this file.")];
        };

        vec![
            Line::from(format!("Summary: {}", entry.summary)),
            Line::from(format!("Commit: {}", entry.commit)),
            Line::from(format!(
                "Author: {} <{}>",
                entry.author_name, entry.author_email
            )),
            Line::from(format!("Date: {}", entry.committed_at)),
        ]
    }

    fn list_items(&self) -> Vec<String> {
        self.entries
            .iter()
            .map(|entry| {
                format!(
                    "{} {} {}",
                    short_commit(&entry.commit),
                    entry.committed_at,
                    entry.summary
                )
            })
            .collect()
    }

    fn move_selection(&mut self, delta: isize) {
        let len = self.entries.len();
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

    fn clamp_selection(&mut self) {
        let len = self.entries.len();
        self.selected_index = if len == 0 {
            None
        } else {
            Some(self.selected_index.unwrap_or(0).min(len - 1))
        };
    }

    fn selected_entry(&self) -> Option<&GitLogEntry> {
        self.selected_index
            .and_then(|index| self.entries.get(index))
    }
}

#[derive(Debug, Clone)]
struct DoctorViewState {
    note_path: String,
    issues: Vec<DoctorIssueRow>,
    selected_index: Option<usize>,
}

#[derive(Debug, Clone)]
struct DoctorIssueRow {
    kind: String,
    message: String,
    detail_lines: Vec<String>,
}

impl DoctorViewState {
    fn load(paths: &VaultPaths, note_path: &str) -> Result<Self, String> {
        let report = doctor_vault(paths).map_err(|error| error.to_string())?;
        let issues = doctor_issues_for_note(note_path, &report);
        let selected_index = (!issues.is_empty()).then_some(0);
        Ok(Self {
            note_path: note_path.to_string(),
            issues,
            selected_index,
        })
    }

    fn reload(&mut self, paths: &VaultPaths) -> Result<(), String> {
        let selected_key = self
            .selected_issue()
            .map(|issue| (issue.kind.clone(), issue.message.clone()));
        let report = doctor_vault(paths).map_err(|error| error.to_string())?;
        self.issues = doctor_issues_for_note(&self.note_path, &report);
        self.selected_index = selected_key.and_then(|key| {
            self.issues
                .iter()
                .position(|issue| (issue.kind.clone(), issue.message.clone()) == key)
        });
        self.clamp_selection();
        Ok(())
    }

    fn note_path(&self) -> &str {
        &self.note_path
    }

    fn filtered_count(&self) -> usize {
        self.issues.len()
    }

    fn selected_index(&self) -> Option<usize> {
        self.selected_index
    }

    fn preview_title(&self) -> String {
        self.selected_issue().map_or_else(
            || format!("Doctor: {}", self.note_path),
            |issue| format!("{}: {}", issue.kind, self.note_path),
        )
    }

    fn preview_lines(&self) -> Vec<Line<'static>> {
        let Some(issue) = self.selected_issue() else {
            return vec![Line::from("No doctor issues for this note.")];
        };

        let mut lines = vec![
            Line::from(format!("Kind: {}", issue.kind)),
            Line::from(format!("Message: {}", issue.message)),
        ];
        lines.extend(issue.detail_lines.iter().cloned().map(Line::from));
        lines
    }

    fn list_items(&self) -> Vec<String> {
        self.issues
            .iter()
            .map(|issue| format!("{}: {}", issue.kind, issue.message))
            .collect()
    }

    fn move_selection(&mut self, delta: isize) {
        let len = self.issues.len();
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

    fn clamp_selection(&mut self) {
        let len = self.issues.len();
        self.selected_index = if len == 0 {
            None
        } else {
            Some(self.selected_index.unwrap_or(0).min(len - 1))
        };
    }

    fn selected_issue(&self) -> Option<&DoctorIssueRow> {
        self.selected_index.and_then(|index| self.issues.get(index))
    }
}

fn doctor_issues_for_note(
    note_path: &str,
    report: &vulcan_core::DoctorReport,
) -> Vec<DoctorIssueRow> {
    let mut issues = Vec::new();

    issues.extend(
        report
            .unresolved_links
            .iter()
            .filter(|issue| issue.document_path.as_deref() == Some(note_path))
            .map(|issue| doctor_link_issue_row("Unresolved link", issue)),
    );
    issues.extend(
        report
            .ambiguous_links
            .iter()
            .filter(|issue| issue.document_path.as_deref() == Some(note_path))
            .map(|issue| doctor_link_issue_row("Ambiguous link", issue)),
    );
    issues.extend(
        report
            .broken_embeds
            .iter()
            .filter(|issue| issue.document_path.as_deref() == Some(note_path))
            .map(|issue| doctor_link_issue_row("Broken embed", issue)),
    );
    issues.extend(
        report
            .parse_failures
            .iter()
            .filter(|issue| issue.document_path.as_deref() == Some(note_path))
            .map(|issue| doctor_diagnostic_issue_row("Parse failure", issue)),
    );
    if report.stale_index_rows.iter().any(|path| path == note_path) {
        issues.push(DoctorIssueRow {
            kind: "Stale index row".to_string(),
            message: format!("Indexed row exists for missing file {note_path}"),
            detail_lines: vec!["Run scan or doctor fix to reconcile the cache.".to_string()],
        });
    }
    if report
        .missing_index_rows
        .iter()
        .any(|path| path == note_path)
    {
        issues.push(DoctorIssueRow {
            kind: "Missing index row".to_string(),
            message: format!("File exists on disk but is missing from the cache: {note_path}"),
            detail_lines: vec!["Run scan or doctor fix to refresh the cache.".to_string()],
        });
    }
    if report.orphan_notes.iter().any(|path| path == note_path) {
        issues.push(DoctorIssueRow {
            kind: "Orphan note".to_string(),
            message: format!("{note_path} has no inbound or outbound note links"),
            detail_lines: Vec::new(),
        });
    }
    issues.extend(
        report
            .html_links
            .iter()
            .filter(|issue| issue.document_path.as_deref() == Some(note_path))
            .map(|issue| doctor_diagnostic_issue_row("HTML link", issue)),
    );

    issues
}

fn doctor_link_issue_row(kind: &str, issue: &DoctorLinkIssue) -> DoctorIssueRow {
    let mut detail_lines = Vec::new();
    if let Some(target) = issue.target.as_deref() {
        detail_lines.push(format!("Target: {target}"));
    }
    if !issue.matches.is_empty() {
        detail_lines.push(format!("Matches: {}", issue.matches.join(", ")));
    }
    DoctorIssueRow {
        kind: kind.to_string(),
        message: issue.message.clone(),
        detail_lines,
    }
}

fn doctor_diagnostic_issue_row(kind: &str, issue: &DoctorDiagnosticIssue) -> DoctorIssueRow {
    let mut detail_lines = Vec::new();
    if let Some(range) = issue.byte_range.as_ref() {
        detail_lines.push(format!("Byte range: {}..{}", range.start, range.end));
    }
    DoctorIssueRow {
        kind: kind.to_string(),
        message: issue.message.clone(),
        detail_lines,
    }
}

#[derive(Debug, Clone)]
struct BacklinksViewState {
    note_path: String,
    backlinks: Vec<BacklinkRecord>,
    selected_index: Option<usize>,
}

impl BacklinksViewState {
    fn load(paths: &VaultPaths, note_path: &str) -> Result<Self, String> {
        let report = query_backlinks(paths, note_path).map_err(|error| error.to_string())?;
        let selected_index = (!report.backlinks.is_empty()).then_some(0);
        Ok(Self {
            note_path: report.note_path,
            backlinks: report.backlinks,
            selected_index,
        })
    }

    fn reload(&mut self, paths: &VaultPaths) -> Result<(), String> {
        let selected_path = self.selected_path().map(str::to_string);
        let report = query_backlinks(paths, &self.note_path).map_err(|error| error.to_string())?;
        self.note_path = report.note_path;
        self.backlinks = report.backlinks;
        self.selected_index = selected_path.and_then(|path| {
            self.backlinks
                .iter()
                .position(|backlink| backlink.source_path == path)
        });
        self.clamp_selection();
        Ok(())
    }

    fn note_path(&self) -> &str {
        &self.note_path
    }

    fn filtered_count(&self) -> usize {
        self.backlinks.len()
    }

    fn selected_index(&self) -> Option<usize> {
        self.selected_index
    }

    fn selected_path(&self) -> Option<&str> {
        self.selected_backlink()
            .map(|backlink| backlink.source_path.as_str())
    }

    fn preview_title(&self) -> String {
        self.selected_path().map_or_else(
            || "Backlink Preview".to_string(),
            |path| format!("Backlink: {path}"),
        )
    }

    fn preview_lines(&self) -> Vec<Line<'static>> {
        let Some(backlink) = self.selected_backlink() else {
            return vec![Line::from("No backlinks.")];
        };

        let mut lines = vec![Line::from(format!("Raw link: {}", backlink.raw_text))];
        if let Some(context) = backlink.context.as_ref() {
            lines.push(Line::from(format!(
                "Line {}:{}",
                context.line, context.column
            )));
            lines.push(Line::from(context.text.clone()));
        }
        lines
    }

    fn list_items(&self) -> Vec<String> {
        self.backlinks
            .iter()
            .map(|backlink| {
                backlink.context.as_ref().map_or_else(
                    || format!("{} [{}]", backlink.source_path, backlink.link_kind),
                    |context| {
                        format!(
                            "{} [{} line {}]",
                            backlink.source_path, backlink.link_kind, context.line
                        )
                    },
                )
            })
            .collect()
    }

    fn move_selection(&mut self, delta: isize) {
        let len = self.backlinks.len();
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

    fn clamp_selection(&mut self) {
        let len = self.backlinks.len();
        self.selected_index = if len == 0 {
            None
        } else {
            Some(self.selected_index.unwrap_or(0).min(len - 1))
        };
    }

    fn selected_backlink(&self) -> Option<&BacklinkRecord> {
        self.selected_index
            .and_then(|index| self.backlinks.get(index))
    }
}

#[derive(Debug, Clone)]
struct OutgoingLinksViewState {
    note_path: String,
    links: Vec<OutgoingLinkRecord>,
    selected_index: Option<usize>,
}

impl OutgoingLinksViewState {
    fn load(paths: &VaultPaths, note_path: &str) -> Result<Self, String> {
        let report = query_links(paths, note_path).map_err(|error| error.to_string())?;
        let selected_index = (!report.links.is_empty()).then_some(0);
        Ok(Self {
            note_path: report.note_path,
            links: report.links,
            selected_index,
        })
    }

    fn reload(&mut self, paths: &VaultPaths) -> Result<(), String> {
        let selected_key = self.selected_link().map(|link| {
            (
                link.resolved_target_path.clone(),
                link.target_path_candidate.clone(),
                link.raw_text.clone(),
            )
        });
        let report = query_links(paths, &self.note_path).map_err(|error| error.to_string())?;
        self.note_path = report.note_path;
        self.links = report.links;
        self.selected_index = selected_key.and_then(|key| {
            self.links.iter().position(|link| {
                (
                    link.resolved_target_path.clone(),
                    link.target_path_candidate.clone(),
                    link.raw_text.clone(),
                ) == key
            })
        });
        self.clamp_selection();
        Ok(())
    }

    fn note_path(&self) -> &str {
        &self.note_path
    }

    fn filtered_count(&self) -> usize {
        self.links.len()
    }

    fn selected_index(&self) -> Option<usize> {
        self.selected_index
    }

    fn selected_path(&self) -> Option<&str> {
        self.selected_link()
            .and_then(|link| link.resolved_target_path.as_deref())
    }

    fn preview_title(&self) -> String {
        self.selected_link().map_or_else(
            || "Link Preview".to_string(),
            |link| format!("Link: {}", outgoing_link_target_label(link)),
        )
    }

    fn preview_lines(&self) -> Vec<Line<'static>> {
        let Some(link) = self.selected_link() else {
            return vec![Line::from("No outgoing links.")];
        };

        let mut lines = vec![
            Line::from(format!("Raw link: {}", link.raw_text)),
            Line::from(format!("Target: {}", outgoing_link_target_label(link))),
            Line::from(format!(
                "Status: {}",
                resolution_status_label(link.resolution_status.clone())
            )),
        ];
        if let Some(context) = link.context.as_ref() {
            lines.push(Line::from(format!(
                "Line {}:{}",
                context.line, context.column
            )));
            lines.push(Line::from(context.text.clone()));
        }
        lines
    }

    fn list_items(&self) -> Vec<String> {
        self.links
            .iter()
            .map(|link| {
                link.context.as_ref().map_or_else(
                    || {
                        format!(
                            "{} [{} {}]",
                            outgoing_link_target_label(link),
                            link.link_kind,
                            resolution_status_label(link.resolution_status.clone())
                        )
                    },
                    |context| {
                        format!(
                            "{} [{} {} line {}]",
                            outgoing_link_target_label(link),
                            link.link_kind,
                            resolution_status_label(link.resolution_status.clone()),
                            context.line
                        )
                    },
                )
            })
            .collect()
    }

    fn move_selection(&mut self, delta: isize) {
        let len = self.links.len();
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

    fn clamp_selection(&mut self) {
        let len = self.links.len();
        self.selected_index = if len == 0 {
            None
        } else {
            Some(self.selected_index.unwrap_or(0).min(len - 1))
        };
    }

    fn selected_link(&self) -> Option<&OutgoingLinkRecord> {
        self.selected_index.and_then(|index| self.links.get(index))
    }
}

fn outgoing_link_target_label(link: &OutgoingLinkRecord) -> String {
    link.resolved_target_path
        .clone()
        .or_else(|| link.target_path_candidate.clone())
        .unwrap_or_else(|| "(self)".to_string())
}

fn short_commit(commit: &str) -> &str {
    commit.get(..7).unwrap_or(commit)
}

fn resolution_status_label(status: ResolutionStatus) -> &'static str {
    match status {
        ResolutionStatus::Resolved => "resolved",
        ResolutionStatus::Unresolved => "unresolved",
        ResolutionStatus::External => "external",
    }
}

fn is_base_path(path: &str) -> bool {
    path.ends_with(".base")
}

fn last_scan_label(paths: &VaultPaths) -> String {
    fs::metadata(paths.cache_db())
        .and_then(|metadata| metadata.modified())
        .map_or_else(|_| "not scanned".to_string(), relative_time_label)
}

fn relative_time_label(timestamp: SystemTime) -> String {
    let elapsed = SystemTime::now()
        .duration_since(timestamp)
        .unwrap_or(Duration::ZERO);
    let seconds = elapsed.as_secs();
    if seconds < 5 {
        "just now".to_string()
    } else if seconds < 60 {
        format!("{seconds}s ago")
    } else if seconds < 3_600 {
        format!("{}m ago", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h ago", seconds / 3_600)
    } else {
        format!("{}d ago", seconds / 86_400)
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

    fn select_path(&mut self, path: &str) {
        if let Some(index) = self
            .hits
            .iter()
            .position(|hit| hit.document_path.as_str() == path)
        {
            self.selected_index = Some(index);
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

#[derive(Debug, Clone)]
struct TagFilterState {
    query: String,
    tags: Vec<NamedCount>,
    active_tag: Option<String>,
    picker: NotePickerState,
}

impl TagFilterState {
    fn new(paths: VaultPaths, tags: Vec<NamedCount>) -> Self {
        Self {
            query: String::new(),
            tags,
            active_tag: None,
            picker: NotePickerState::new(paths, Vec::new(), ""),
        }
    }

    fn query(&self) -> &str {
        &self.query
    }

    fn selected_path(&self) -> Option<&str> {
        self.picker.selected_path()
    }

    fn selected_index(&self) -> Option<usize> {
        self.picker.selected_index()
    }

    fn select_path(&mut self, path: &str) {
        self.picker.select_path(path);
    }

    fn filtered_count(&self) -> usize {
        self.picker.filtered_count()
    }

    fn preview_title(&self) -> String {
        match (self.active_tag.as_deref(), self.picker.selected_path()) {
            (Some(tag), Some(path)) => format!("Preview: {path} [#{tag}]"),
            (Some(tag), None) => format!("Tag Preview: #{tag}"),
            (None, _) => "Tag Preview".to_string(),
        }
    }

    fn preview_lines(&self) -> Vec<Line<'static>> {
        match self.active_tag.as_deref() {
            Some(tag) if self.picker.filtered_count() == 0 => {
                vec![Line::from(format!("No notes matched #{tag}."))]
            }
            Some(_) => self.picker.preview_lines(),
            None if self.query.trim().is_empty() => vec![Line::from("Type to search tags.")],
            None => vec![Line::from("No matching tags.")],
        }
    }

    fn refresh_preview(&mut self) {
        self.picker.refresh_preview();
    }

    fn list_items(&self) -> Vec<String> {
        self.picker
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
            .collect()
    }

    fn handle_key(
        &mut self,
        paths: &VaultPaths,
        all_notes: &[NoteIdentity],
        code: KeyCode,
    ) -> Result<(), String> {
        match code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.picker.move_selection(-1);
                Ok(())
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.picker.move_selection(1);
                Ok(())
            }
            KeyCode::Backspace => {
                self.query.pop();
                self.refresh_results(paths, all_notes)
            }
            KeyCode::Char(character) => {
                self.query.push(character);
                self.refresh_results(paths, all_notes)
            }
            _ => Ok(()),
        }
    }

    fn refresh_results(
        &mut self,
        paths: &VaultPaths,
        all_notes: &[NoteIdentity],
    ) -> Result<(), String> {
        self.active_tag = best_matching_tag(&self.tags, &self.query).map(|tag| tag.name.clone());
        let notes = self.active_tag.as_deref().map_or_else(
            || Ok(Vec::new()),
            |tag| tag_filtered_notes(paths, all_notes, tag),
        )?;
        self.picker.replace_notes_preserve_selection(notes);
        self.picker.set_query("");
        Ok(())
    }
}

#[derive(Debug, Clone)]
struct PropertyFilterState {
    query: String,
    picker: NotePickerState,
    last_error: Option<String>,
}

impl PropertyFilterState {
    fn new(paths: VaultPaths) -> Self {
        Self {
            query: String::new(),
            picker: NotePickerState::new(paths, Vec::new(), ""),
            last_error: None,
        }
    }

    fn query(&self) -> &str {
        &self.query
    }

    fn selected_path(&self) -> Option<&str> {
        self.picker.selected_path()
    }

    fn selected_index(&self) -> Option<usize> {
        self.picker.selected_index()
    }

    fn select_path(&mut self, path: &str) {
        self.picker.select_path(path);
    }

    fn filtered_count(&self) -> usize {
        self.picker.filtered_count()
    }

    fn preview_title(&self) -> String {
        self.picker.selected_path().map_or_else(
            || "Property Preview".to_string(),
            |path| format!("Preview: {path}"),
        )
    }

    fn preview_lines(&self) -> Vec<Line<'static>> {
        if let Some(error) = self.last_error.as_deref() {
            return vec![Line::from(error.to_string())];
        }
        if self.query.trim().is_empty() {
            return vec![Line::from(
                "Type a property predicate like `status = active`.",
            )];
        }
        if self.picker.filtered_count() == 0 {
            return vec![Line::from("No notes matched the predicate.")];
        }
        self.picker.preview_lines()
    }

    fn refresh_preview(&mut self) {
        self.picker.refresh_preview();
    }

    fn list_items(&self) -> Vec<String> {
        self.picker
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
            .collect()
    }

    fn handle_key(
        &mut self,
        paths: &VaultPaths,
        all_notes: &[NoteIdentity],
        code: KeyCode,
    ) -> Result<(), String> {
        match code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.picker.move_selection(-1);
                Ok(())
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.picker.move_selection(1);
                Ok(())
            }
            KeyCode::Backspace => {
                self.query.pop();
                self.refresh_results(paths, all_notes);
                Ok(())
            }
            KeyCode::Char(character) => {
                self.query.push(character);
                self.refresh_results(paths, all_notes);
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn refresh_results(&mut self, paths: &VaultPaths, all_notes: &[NoteIdentity]) {
        self.last_error = None;
        if self.query.trim().is_empty() {
            self.picker.replace_notes_preserve_selection(Vec::new());
            self.picker.set_query("");
            return;
        }

        match property_filtered_notes(paths, all_notes, &self.query) {
            Ok(notes) => {
                self.picker.replace_notes_preserve_selection(notes);
                self.picker.set_query("");
            }
            Err(error) => {
                self.last_error = Some(error);
                self.picker.replace_notes_preserve_selection(Vec::new());
                self.picker.set_query("");
            }
        }
    }

    fn status_line(&self) -> String {
        self.last_error
            .clone()
            .unwrap_or_else(|| "Ready.".to_string())
    }
}

fn best_matching_tag<'a>(tags: &'a [NamedCount], query: &str) -> Option<&'a NamedCount> {
    if query.trim().is_empty() {
        return None;
    }

    let mut matches = tags
        .iter()
        .filter_map(|tag| fuzzy_text_score(&tag.name, query).map(|score| (score, tag)))
        .collect::<Vec<_>>();
    matches.sort_by(|(left_score, left), (right_score, right)| {
        right_score
            .cmp(left_score)
            .then_with(|| right.count.cmp(&left.count))
            .then_with(|| left.name.cmp(&right.name))
    });
    matches.into_iter().map(|(_, tag)| tag).next()
}

fn tag_filtered_notes(
    paths: &VaultPaths,
    all_notes: &[NoteIdentity],
    tag: &str,
) -> Result<Vec<NoteIdentity>, String> {
    let tagged = list_tagged_note_identities(paths, tag).map_err(|error| error.to_string())?;
    Ok(tagged
        .into_iter()
        .filter_map(|identity| {
            all_notes
                .iter()
                .find(|note| note.path == identity.path)
                .cloned()
        })
        .collect())
}

fn property_filtered_notes(
    paths: &VaultPaths,
    all_notes: &[NoteIdentity],
    predicate: &str,
) -> Result<Vec<NoteIdentity>, String> {
    let report = query_notes(
        paths,
        &NoteQuery {
            filters: vec![predicate.trim().to_string()],
            sort_by: Some("file.path".to_string()),
            sort_descending: false,
        },
    )
    .map_err(|error| error.to_string())?;
    Ok(report
        .notes
        .into_iter()
        .filter_map(|record| {
            all_notes
                .iter()
                .find(|note| note.path == record.document_path)
                .cloned()
        })
        .collect())
}

fn fuzzy_text_score(text: &str, query: &str) -> Option<i32> {
    if query.trim().is_empty() {
        return Some(0);
    }

    let haystack = text.to_lowercase();
    let needle = query.trim().to_lowercase();

    if haystack.contains(&needle) {
        return Some(4_000 - i32::try_from(text.len()).unwrap_or(i32::MAX));
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
                Some(b'/' | b'-' | b'_' | b' ')
            )
        {
            score += 20;
        }
        last_index = absolute + character.len_utf8();
    }

    Some(score)
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
    use std::process::Command;
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

    fn run_git(vault_root: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(vault_root)
            .args(args)
            .status()
            .expect("git should launch");
        assert!(status.success(), "git command failed: {:?}", args);
    }

    fn init_git_repo(vault_root: &Path) {
        run_git(vault_root, &["init"]);
        run_git(vault_root, &["config", "user.name", "Vulcan Test"]);
        run_git(vault_root, &["config", "user.email", "vulcan@example.com"]);
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
        )
        .expect("state should build");
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
        let mut state = BrowseState::new(paths, vec![note("Projects/Alpha.md", &[])])
            .expect("state should build");

        let action = state.handle_key(key(KeyCode::Char('e')));

        assert_eq!(action, BrowseAction::Edit("Projects/Alpha.md".to_string()));
    }

    #[test]
    fn m_opens_move_prompt_for_selected_note() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        write_note(temp_dir.path(), "Projects/Alpha.md", "Alpha");
        let mut state = BrowseState::new(paths, vec![note("Projects/Alpha.md", &[])])
            .expect("state should build");

        let action = state.handle_key(key(KeyCode::Char('m')));

        assert_eq!(action, BrowseAction::Continue);
        assert_eq!(state.query_title(), "Move Note (Projects/Alpha.md)");
        assert_eq!(state.query(), "Projects/Alpha.md");
        assert_eq!(
            state.mode_help_line(),
            "type the destination path, then press Enter to rename or move"
        );
    }

    #[test]
    fn move_action_renames_note_and_reloads_selection() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        write_note(temp_dir.path(), "Home.md", "Link to [[Projects/Alpha]].");
        write_note(temp_dir.path(), "Projects/Alpha.md", "# Alpha");
        scan_fixture(&paths);
        let mut state = BrowseState::new(
            paths.clone(),
            vec![note("Home.md", &[]), note("Projects/Alpha.md", &[])],
        )
        .expect("state should build");
        state.picker.select_path("Projects/Alpha.md");

        state.handle_key(key(KeyCode::Char('m')));
        for _ in 0.."Projects/Alpha.md".len() {
            state.handle_key(key(KeyCode::Backspace));
        }
        for character in "Archive/Alpha.md".chars() {
            state.handle_key(key(KeyCode::Char(character)));
        }

        let action = state.handle_key(key(KeyCode::Enter));
        let BrowseAction::Move {
            source_path,
            destination,
        } = action
        else {
            panic!("expected move action");
        };
        move_note(&paths, &source_path, &destination, false).expect("move should succeed");
        state.clear_move_prompt();
        state
            .reload_after_move(&destination)
            .expect("browse state should reload after move");

        assert_eq!(state.selected_path(), Some("Archive/Alpha.md"));
        assert!(temp_dir.path().join("Archive/Alpha.md").exists());
        assert!(!temp_dir.path().join("Projects/Alpha.md").exists());
        let home =
            fs::read_to_string(temp_dir.path().join("Home.md")).expect("home should be readable");
        assert!(!home.contains("[[Projects/Alpha]]"));
        assert!(home.contains("[[Alpha]]"));
    }

    #[test]
    fn b_opens_backlinks_view_with_context() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        write_note(temp_dir.path(), "Home.md", "Link to [[Projects/Alpha]].");
        write_note(
            temp_dir.path(),
            "Daily.md",
            "Second link to [[Projects/Alpha]].",
        );
        write_note(temp_dir.path(), "Projects/Alpha.md", "# Alpha");
        scan_fixture(&paths);
        let mut state = BrowseState::new(
            paths,
            vec![
                note("Daily.md", &[]),
                note("Home.md", &[]),
                note("Projects/Alpha.md", &[]),
            ],
        )
        .expect("state should build");
        state.picker.select_path("Projects/Alpha.md");

        let action = state.handle_key(key(KeyCode::Char('b')));

        assert_eq!(action, BrowseAction::Continue);
        assert_eq!(state.query_title(), "Backlinks (Projects/Alpha.md)");
        assert_eq!(state.list_title(), "Backlinks");
        assert_eq!(state.filtered_count(), 2);
        assert_eq!(state.selected_path(), Some("Daily.md"));
        assert_eq!(
            state.mode_help_line(),
            "view notes that link to the selected note; Esc returns to browse"
        );
        assert!(state.preview_lines().iter().any(|line| line
            .spans
            .iter()
            .any(|span| span.content.contains("[[Projects/Alpha]]"))));
    }

    #[test]
    fn backlinks_view_esc_returns_to_browse() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        write_note(temp_dir.path(), "Home.md", "Link to [[Projects/Alpha]].");
        write_note(temp_dir.path(), "Projects/Alpha.md", "# Alpha");
        scan_fixture(&paths);
        let mut state = BrowseState::new(
            paths,
            vec![note("Home.md", &[]), note("Projects/Alpha.md", &[])],
        )
        .expect("state should build");
        state.picker.select_path("Projects/Alpha.md");
        state.handle_key(key(KeyCode::Char('b')));

        let action = state.handle_key(key(KeyCode::Esc));

        assert_eq!(action, BrowseAction::Continue);
        assert_eq!(state.query_title(), "Browse (/ fuzzy search)");
        assert_eq!(state.selected_path(), Some("Projects/Alpha.md"));
    }

    #[test]
    fn l_opens_outgoing_links_view_with_context() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        write_note(temp_dir.path(), "Home.md", "# Home");
        write_note(temp_dir.path(), "People/Bob.md", "# Bob");
        write_note(
            temp_dir.path(),
            "Projects/Alpha.md",
            "Links: [[Home]] and [[People/Bob|Bob]].",
        );
        scan_fixture(&paths);
        let mut state = BrowseState::new(
            paths,
            vec![
                note("Home.md", &[]),
                note("People/Bob.md", &[]),
                note("Projects/Alpha.md", &[]),
            ],
        )
        .expect("state should build");
        state.picker.select_path("Projects/Alpha.md");

        let action = state.handle_key(key(KeyCode::Char('l')));

        assert_eq!(action, BrowseAction::Continue);
        assert_eq!(state.query_title(), "Outgoing Links (Projects/Alpha.md)");
        assert_eq!(state.list_title(), "Links");
        assert_eq!(state.filtered_count(), 2);
        assert_eq!(state.selected_path(), Some("Home.md"));
        assert_eq!(
            state.mode_help_line(),
            "view links from the selected note; Esc returns to browse"
        );
        assert!(state.preview_lines().iter().any(|line| line
            .spans
            .iter()
            .any(|span| span.content.contains("[[Home]]"))));
    }

    #[test]
    fn links_view_esc_returns_to_browse() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        write_note(temp_dir.path(), "Home.md", "# Home");
        write_note(temp_dir.path(), "Projects/Alpha.md", "Links: [[Home]].");
        scan_fixture(&paths);
        let mut state = BrowseState::new(
            paths,
            vec![note("Home.md", &[]), note("Projects/Alpha.md", &[])],
        )
        .expect("state should build");
        state.picker.select_path("Projects/Alpha.md");
        state.handle_key(key(KeyCode::Char('l')));

        let action = state.handle_key(key(KeyCode::Esc));

        assert_eq!(action, BrowseAction::Continue);
        assert_eq!(state.query_title(), "Browse (/ fuzzy search)");
        assert_eq!(state.selected_path(), Some("Projects/Alpha.md"));
    }

    #[test]
    fn o_requests_bases_tui_for_selected_base_link() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        write_note(
            temp_dir.path(),
            "Projects/Alpha.md",
            "Links: [[release.base]].",
        );
        fs::write(
            temp_dir.path().join("release.base"),
            "filters:\n  and:\n    - 'file.ext == \"md\"'\nviews:\n  - name: Release Table\n    type: table\n",
        )
        .expect("base file should be written");
        scan_fixture(&paths);
        let mut state = BrowseState::new(paths, vec![note("Projects/Alpha.md", &[])])
            .expect("state should build");
        state.picker.select_path("Projects/Alpha.md");
        state.handle_key(key(KeyCode::Char('l')));

        let action = state.handle_key(key(KeyCode::Char('o')));

        assert_eq!(
            action,
            BrowseAction::OpenBaseTui("release.base".to_string())
        );
    }

    #[test]
    fn g_opens_git_log_view_for_selected_note() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        init_git_repo(temp_dir.path());
        let paths = VaultPaths::new(temp_dir.path());
        write_note(temp_dir.path(), "Projects/Alpha.md", "# Alpha\n");
        scan_fixture(&paths);
        run_git(temp_dir.path(), &["add", "Projects/Alpha.md"]);
        run_git(temp_dir.path(), &["commit", "-m", "Add alpha"]);
        write_note(temp_dir.path(), "Projects/Alpha.md", "# Alpha\nUpdated\n");
        run_git(temp_dir.path(), &["add", "Projects/Alpha.md"]);
        run_git(temp_dir.path(), &["commit", "-m", "Update alpha"]);

        let mut state = BrowseState::new(paths, vec![note("Projects/Alpha.md", &[])])
            .expect("state should build");
        state.picker.select_path("Projects/Alpha.md");

        let action = state.handle_key(key(KeyCode::Char('g')));

        assert_eq!(action, BrowseAction::Continue);
        assert_eq!(state.query_title(), "Git Log (Projects/Alpha.md)");
        assert_eq!(state.list_title(), "Git Log");
        assert_eq!(state.filtered_count(), 2);
        assert_eq!(
            state.mode_help_line(),
            "view git history for the selected file; Esc returns to browse"
        );
        assert!(state.list_items()[0].contains("Update alpha"));
    }

    #[test]
    fn git_view_esc_returns_to_browse() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        init_git_repo(temp_dir.path());
        let paths = VaultPaths::new(temp_dir.path());
        write_note(temp_dir.path(), "Projects/Alpha.md", "# Alpha\n");
        scan_fixture(&paths);
        run_git(temp_dir.path(), &["add", "Projects/Alpha.md"]);
        run_git(temp_dir.path(), &["commit", "-m", "Add alpha"]);

        let mut state = BrowseState::new(paths, vec![note("Projects/Alpha.md", &[])])
            .expect("state should build");
        state.picker.select_path("Projects/Alpha.md");
        state.handle_key(key(KeyCode::Char('g')));

        let action = state.handle_key(key(KeyCode::Esc));

        assert_eq!(action, BrowseAction::Continue);
        assert_eq!(state.query_title(), "Browse (/ fuzzy search)");
        assert_eq!(state.selected_path(), Some("Projects/Alpha.md"));
    }

    #[test]
    fn d_opens_doctor_view_for_selected_note() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        write_note(temp_dir.path(), "Home.md", "# Home");
        write_note(
            temp_dir.path(),
            "Projects/Alpha.md",
            "Links: [[Home]] and [[Missing]].",
        );
        scan_fixture(&paths);
        let mut state = BrowseState::new(
            paths,
            vec![note("Home.md", &[]), note("Projects/Alpha.md", &[])],
        )
        .expect("state should build");
        state.picker.select_path("Projects/Alpha.md");

        let action = state.handle_key(key(KeyCode::Char('d')));

        assert_eq!(action, BrowseAction::Continue);
        assert_eq!(state.query_title(), "Doctor (Projects/Alpha.md)");
        assert_eq!(state.list_title(), "Diagnostics");
        assert_eq!(state.filtered_count(), 1);
        assert_eq!(
            state.mode_help_line(),
            "view doctor diagnostics for the selected note; Esc returns to browse"
        );
        assert!(state.list_items()[0].contains("Unresolved link"));
        assert!(state.preview_lines().iter().any(|line| line
            .spans
            .iter()
            .any(|span| span.content.contains("Missing"))));
    }

    #[test]
    fn doctor_view_esc_returns_to_browse() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        write_note(temp_dir.path(), "Home.md", "# Home");
        write_note(
            temp_dir.path(),
            "Projects/Alpha.md",
            "Links: [[Home]] and [[Missing]].",
        );
        scan_fixture(&paths);
        let mut state = BrowseState::new(
            paths,
            vec![note("Home.md", &[]), note("Projects/Alpha.md", &[])],
        )
        .expect("state should build");
        state.picker.select_path("Projects/Alpha.md");
        state.handle_key(key(KeyCode::Char('d')));

        let action = state.handle_key(key(KeyCode::Esc));

        assert_eq!(action, BrowseAction::Continue);
        assert_eq!(state.query_title(), "Browse (/ fuzzy search)");
        assert_eq!(state.selected_path(), Some("Projects/Alpha.md"));
    }

    #[test]
    fn n_opens_new_note_prompt() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        let mut state =
            BrowseState::new(paths, vec![note("Home.md", &[])]).expect("state should build");

        let action = state.handle_key(key(KeyCode::Char('n')));

        assert_eq!(action, BrowseAction::Continue);
        assert_eq!(state.query_title(), "New Note");
        assert_eq!(state.query(), "");
        assert_eq!(
            state.mode_help_line(),
            "type a relative note path like Inbox/Idea.md, then press Enter"
        );
    }

    #[test]
    fn new_note_action_creates_file_and_reloads_selection() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        write_note(temp_dir.path(), "Home.md", "# Home");
        scan_fixture(&paths);
        let mut state = BrowseState::new(paths.clone(), vec![note("Home.md", &[])])
            .expect("state should build");

        state.handle_key(key(KeyCode::Char('n')));
        for character in "Inbox/Idea".chars() {
            state.handle_key(key(KeyCode::Char(character)));
        }

        let action = state.handle_key(key(KeyCode::Enter));
        let BrowseAction::Create(path) = action else {
            panic!("expected create action");
        };
        let relative_path = normalize_relative_input_path(
            &path,
            RelativePathOptions {
                expected_extension: Some("md"),
                append_extension_if_missing: true,
            },
        )
        .expect("path should normalize");
        let absolute = temp_dir.path().join(&relative_path);
        if let Some(parent) = absolute.parent() {
            fs::create_dir_all(parent).expect("new note dir should be created");
        }
        fs::write(&absolute, "").expect("new note should be created");
        scan_fixture(&paths);
        state.clear_new_note_prompt();
        state
            .reload_after_new_note(&relative_path)
            .expect("browse state should reload after create");

        assert_eq!(relative_path, "Inbox/Idea.md");
        assert!(absolute.exists());
        assert_eq!(state.selected_path(), Some("Inbox/Idea.md"));
    }

    #[test]
    fn status_bar_line_shows_vault_mode_counts_and_last_scan() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("browse-vault");
        let paths = VaultPaths::new(&vault_root);
        write_note(&vault_root, "Home.md", "# Home");
        scan_fixture(&paths);
        let state =
            BrowseState::new(paths, vec![note("Home.md", &[])]).expect("state should build");

        let line = state.status_bar_line();

        assert!(line.contains("Vault: browse-vault"));
        assert!(line.contains("Mode: fuzzy"));
        assert!(line.contains("Notes: 1 filtered / 1 total"));
        assert!(line.contains("Last scan:"));
    }

    #[test]
    fn ctrl_f_switches_to_full_text_mode() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        write_note(temp_dir.path(), "Projects/Alpha.md", "release dashboard");
        scan_fixture(&paths);
        let mut state = BrowseState::new(paths, vec![note("Projects/Alpha.md", &[])])
            .expect("state should build");

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
        )
        .expect("state should build");

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
        let mut state = BrowseState::new(paths, vec![note("Projects/Alpha.md", &[])])
            .expect("state should build");

        state.handle_key(ctrl('f'));
        let action = state.handle_key(key(KeyCode::Char('/')));

        assert_eq!(action, BrowseAction::Continue);
        assert_eq!(state.mode, BrowseMode::Fuzzy);
        assert_eq!(state.query_title(), "Browse (/ fuzzy search)");
    }

    #[test]
    fn ctrl_t_switches_to_tag_mode_and_filters_notes() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        write_note(
            temp_dir.path(),
            "Home.md",
            "---\ntags:\n  - dashboard\n---\n\n# Home\nThe dashboard note uses #index.",
        );
        write_note(
            temp_dir.path(),
            "Projects/Alpha.md",
            "---\ntags:\n  - project\n---\n\n# Alpha",
        );
        write_note(
            temp_dir.path(),
            "People/Bob.md",
            "# Bob\nBob uses the tag #people/team.",
        );
        scan_fixture(&paths);
        let mut state = BrowseState::new(
            paths,
            vec![
                note("Home.md", &[]),
                note("Projects/Alpha.md", &[]),
                note("People/Bob.md", &[]),
            ],
        )
        .expect("state should build");

        state.handle_key(ctrl('t'));
        state.handle_key(key(KeyCode::Char('p')));
        state.handle_key(key(KeyCode::Char('e')));
        state.handle_key(key(KeyCode::Char('o')));

        assert_eq!(state.mode, BrowseMode::Tag);
        assert_eq!(state.query_title(), "Browse (Ctrl-T tag filter)");
        assert_eq!(state.selected_path(), Some("People/Bob.md"));
        assert_eq!(state.filtered_count(), 1);
        assert_eq!(state.status_line(), "Tag: #people/team");
        assert!(state.preview_title().contains("#people/team"));
    }

    #[test]
    fn ctrl_p_switches_to_property_mode_and_filters_notes() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        write_note(
            temp_dir.path(),
            "Projects/Alpha.md",
            "---\nstatus: active\n---\n\n# Alpha",
        );
        write_note(
            temp_dir.path(),
            "Projects/Beta.md",
            "---\nstatus: draft\n---\n\n# Beta",
        );
        scan_fixture(&paths);
        let mut state = BrowseState::new(
            paths,
            vec![
                note("Projects/Alpha.md", &[]),
                note("Projects/Beta.md", &[]),
            ],
        )
        .expect("state should build");

        state.handle_key(ctrl('p'));
        for character in "status = active".chars() {
            state.handle_key(key(KeyCode::Char(character)));
        }

        assert_eq!(state.mode, BrowseMode::Property);
        assert_eq!(state.query_title(), "Browse (Ctrl-P property filter)");
        assert_eq!(state.selected_path(), Some("Projects/Alpha.md"));
        assert_eq!(state.filtered_count(), 1);
        assert_eq!(state.status_line(), "Ready.");
    }

    #[test]
    fn property_filter_tracks_invalid_predicates() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        write_note(
            temp_dir.path(),
            "Projects/Alpha.md",
            "---\nstatus: active\n---\n\n# Alpha",
        );
        scan_fixture(&paths);
        let mut state = BrowseState::new(paths, vec![note("Projects/Alpha.md", &[])])
            .expect("state should build");

        state.handle_key(ctrl('p'));
        for character in "status =".chars() {
            state.handle_key(key(KeyCode::Char(character)));
        }

        assert_eq!(state.mode, BrowseMode::Property);
        assert_eq!(state.filtered_count(), 0);
        assert!(state.property_filter.last_error.is_some());
    }

    #[test]
    fn highlighted_snippet_parser_marks_bracketed_terms() {
        let line = highlighted_snippet_line("alpha [dashboard] omega");

        assert_eq!(line.spans.len(), 3);
        assert_eq!(line.spans[1].content, "dashboard");
    }
}
