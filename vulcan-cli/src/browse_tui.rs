use crate::bases_tui;
use crate::commit::AutoCommitPolicy;
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
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::Path;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, SystemTime};
use vulcan_core::paths::{normalize_relative_input_path, RelativePathOptions};
use vulcan_core::properties::load_note_index;
use vulcan_core::search::{SearchMode, SearchSort};
use vulcan_core::{
    doctor_vault, evaluate_base_file, evaluate_dataview_js_query, evaluate_dql,
    evaluate_note_inline_expressions, expected_periodic_note_path, git_log, is_git_repo,
    list_daily_note_events, list_kanban_boards, list_note_identities, list_tagged_note_identities,
    list_tags, load_dataview_blocks, load_kanban_board, load_vault_config, move_note,
    query_backlinks, query_links, query_notes, scan_vault, search_vault, AutoScanMode,
    BacklinkRecord, DataviewJsOutput, DoctorDiagnosticIssue, DoctorLinkIssue, DqlEvalError,
    DqlQueryResult, GitLogEntry, KanbanBoardRecord, NamedCount, NoteIdentity, NoteQuery,
    OutgoingLinkRecord, PeriodicConfig, PeriodicStartOfWeek, ResolutionStatus, ScanMode,
    ScanSummary, SearchHit, SearchQuery, VaultPaths,
};

const FULL_TEXT_LIMIT: usize = 200;
const FULL_TEXT_CONTEXT_SIZE: usize = 18;
const DATAVIEW_PREVIEW_LINE_LIMIT: usize = 24;
const DATAVIEW_INLINE_PREVIEW_LIMIT: usize = 4;
const DATAVIEW_BLOCK_PREVIEW_LIMIT: usize = 6;
const CALENDAR_EVENT_PREVIEW_LIMIT: usize = 8;

pub fn run_browse_tui(
    paths: &VaultPaths,
    refresh_mode: AutoScanMode,
    no_commit: bool,
) -> Result<(), io::Error> {
    let background_refresh = prepare_browse_refresh(paths, refresh_mode)?;

    let mut state = BrowseState::new(paths.clone(), load_notes(paths).map_err(io::Error::other)?)
        .map_err(io::Error::other)?;
    state.background_refresh = background_refresh;
    let auto_commit = AutoCommitPolicy::for_mutation(paths, no_commit);
    if let Some(message) = auto_commit.warning() {
        state.set_status(format!("Warning: {message}."));
    }

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.hide_cursor()?;

    let result = run_event_loop(&mut terminal, &mut state, &auto_commit);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

fn load_notes(paths: &VaultPaths) -> Result<Vec<NoteIdentity>, String> {
    list_note_identities(paths).map_err(|error| error.to_string())
}

#[allow(clippy::too_many_lines)]
fn run_event_loop(
    terminal: &mut Terminal<ratatui::backend::CrosstermBackend<io::Stdout>>,
    state: &mut BrowseState,
    auto_commit: &AutoCommitPolicy,
) -> Result<(), io::Error> {
    loop {
        state.poll_background_refresh();
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
                            } else if let Err(error) = auto_commit.commit(
                                &state.paths,
                                "browse",
                                std::slice::from_ref(&path),
                            ) {
                                state.set_status(format!(
                                    "Updated {path}, but auto-commit failed: {error}"
                                ));
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
                            } else if let Err(error) = auto_commit.commit(
                                &state.paths,
                                "browse",
                                std::slice::from_ref(&relative_path),
                            ) {
                                state.set_status(format!(
                                    "Created {relative_path}, but auto-commit failed: {error}"
                                ));
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
                        } else if let Err(error) = auto_commit.commit(
                            &state.paths,
                            "browse",
                            &std::iter::once(summary.source_path.clone())
                                .chain(std::iter::once(summary.destination_path.clone()))
                                .chain(summary.rewritten_files.iter().map(|file| file.path.clone()))
                                .collect::<BTreeSet<_>>()
                                .into_iter()
                                .collect::<Vec<_>>(),
                        ) {
                            state.set_status(format!(
                                "Moved {} -> {}, but auto-commit failed: {error}",
                                summary.source_path, summary.destination_path
                            ));
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

fn prepare_browse_refresh(
    paths: &VaultPaths,
    refresh_mode: AutoScanMode,
) -> Result<Option<BackgroundRefreshState>, io::Error> {
    if !paths.cache_db().exists() {
        scan_vault(paths, ScanMode::Incremental).map_err(io::Error::other)?;
        return Ok(None);
    }

    match refresh_mode {
        AutoScanMode::Off => Ok(None),
        AutoScanMode::Blocking => {
            scan_vault(paths, ScanMode::Incremental).map_err(io::Error::other)?;
            Ok(None)
        }
        AutoScanMode::Background => Ok(Some(BackgroundRefreshState::spawn(paths.clone()))),
    }
}

#[allow(clippy::too_many_lines)]
fn draw(frame: &mut Frame<'_>, state: &BrowseState) {
    let area = frame.area();
    if area.height < 7 || area.width < 32 {
        let compact = Paragraph::new(vec![
            Line::from(state.status_bar_line()),
            Line::from(format!(
                "Selected: {}",
                state.selected_path().unwrap_or("none")
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
        frame.render_widget(compact, area);
        return;
    }

    let query_height = if area.height >= 10 { 3 } else { 2 };
    let footer_height = if area.height >= 14 { 5 } else { 3 };
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(query_height),
            Constraint::Min(1),
            Constraint::Length(footer_height),
        ])
        .split(area);

    let body = Layout::default()
        .direction(if area.width < 90 {
            Direction::Vertical
        } else {
            Direction::Horizontal
        })
        .constraints([Constraint::Percentage(52), Constraint::Percentage(48)])
        .split(layout[1]);

    let query = Paragraph::new(state.query().to_string()).block(
        Block::default()
            .title(state.query_title())
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Yellow)),
    );
    frame.render_widget(query, layout[0]);

    if let Some(view) = state.kanban_view.as_ref() {
        draw_kanban_board_view(frame, layout[1], view);
        let footer_lines = if footer_height >= 5 {
            vec![
                Line::from(state.status_bar_line()),
                Line::from(state.key_help_line()),
                Line::from(format!("      {}", state.mode_help_line())),
                Line::from(format!("Status: {}", state.status_line())),
            ]
        } else {
            vec![
                Line::from(state.status_bar_line()),
                Line::from(format!("Status: {}", state.status_line())),
            ]
        };
        let footer = Paragraph::new(footer_lines)
            .block(
                Block::default()
                    .title("Browse")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan)),
            )
            .wrap(Wrap { trim: false });
        frame.render_widget(footer, layout[2]);
        return;
    }

    if state.mode == BrowseMode::Calendar
        && state.git_view.is_none()
        && state.doctor_view.is_none()
        && state.backlinks_view.is_none()
        && state.links_view.is_none()
        && state.new_note_prompt.is_none()
        && state.move_prompt.is_none()
    {
        draw_calendar_view(frame, body[0], &state.calendar);
        let preview = Paragraph::new(state.preview_lines())
            .block(
                Block::default()
                    .title(state.preview_title())
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan)),
            )
            .wrap(Wrap { trim: false });
        frame.render_widget(preview, body[1]);

        let footer_lines = if footer_height >= 5 {
            vec![
                Line::from(state.status_bar_line()),
                Line::from(state.key_help_line()),
                Line::from(format!("      {}", state.mode_help_line())),
                Line::from(format!("Status: {}", state.status_line())),
            ]
        } else {
            vec![
                Line::from(state.status_bar_line()),
                Line::from(format!("Status: {}", state.status_line())),
            ]
        };
        let footer = Paragraph::new(footer_lines)
            .block(
                Block::default()
                    .title("Browse")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan)),
            )
            .wrap(Wrap { trim: false });
        frame.render_widget(footer, layout[2]);
        return;
    }

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

    let show_full_text_explain = state.mode == BrowseMode::FullText
        && state.full_text.show_explain()
        && state.git_view.is_none()
        && state.doctor_view.is_none()
        && state.backlinks_view.is_none()
        && state.links_view.is_none()
        && state.new_note_prompt.is_none()
        && state.move_prompt.is_none();

    if show_full_text_explain {
        let preview_layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
            .split(body[1]);
        let preview = Paragraph::new(state.preview_lines())
            .block(
                Block::default()
                    .title(state.preview_title())
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan)),
            )
            .wrap(Wrap { trim: false });
        frame.render_widget(preview, preview_layout[0]);

        let explain = Paragraph::new(state.full_text.explain_preview_lines())
            .block(
                Block::default()
                    .title("Explain (Ctrl-E, PageUp/PageDown)")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Yellow)),
            )
            .wrap(Wrap { trim: false });
        frame.render_widget(explain, preview_layout[1]);
    } else {
        let preview = Paragraph::new(state.preview_lines())
            .block(
                Block::default()
                    .title(state.preview_title())
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan)),
            )
            .wrap(Wrap { trim: false });
        frame.render_widget(preview, body[1]);
    }

    let footer_lines = if footer_height >= 5 {
        vec![
            Line::from(state.status_bar_line()),
            Line::from(state.key_help_line()),
            Line::from(format!("      {}", state.mode_help_line())),
            Line::from(format!("Status: {}", state.status_line())),
        ]
    } else {
        vec![
            Line::from(state.status_bar_line()),
            Line::from(format!("Status: {}", state.status_line())),
        ]
    };
    let footer = Paragraph::new(footer_lines)
        .block(
            Block::default()
                .title("Browse")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(footer, layout[2]);
}

fn draw_calendar_view(
    frame: &mut Frame<'_>,
    area: ratatui::layout::Rect,
    view: &CalendarViewState,
) {
    let paragraph = Paragraph::new(view.calendar_lines())
        .block(
            Block::default()
                .title(view.title())
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn draw_kanban_board_view(
    frame: &mut Frame<'_>,
    area: ratatui::layout::Rect,
    view: &KanbanBoardViewState,
) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(68), Constraint::Percentage(32)])
        .split(area);

    let column_count = view.board.columns.len().max(1);
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(vec![
            Constraint::Ratio(
                1,
                column_count.try_into().unwrap_or(1)
            );
            column_count
        ])
        .split(layout[0]);

    for (index, column_area) in columns.iter().copied().enumerate() {
        let Some(column) = view.board.columns.get(index) else {
            continue;
        };
        let lines = if column.cards.is_empty() {
            vec![Line::from("No cards.")]
        } else {
            column
                .cards
                .iter()
                .enumerate()
                .map(|(card_index, card)| {
                    let label = format_kanban_card_label(card);
                    if index == view.selected_column && card_index == view.selected_card {
                        Line::from(vec![Span::styled(
                            label,
                            Style::default()
                                .bg(Color::DarkGray)
                                .fg(Color::Yellow)
                                .add_modifier(Modifier::BOLD),
                        )])
                    } else {
                        Line::from(label)
                    }
                })
                .collect()
        };
        let border_style = if index == view.selected_column {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default().fg(Color::Cyan)
        };
        let paragraph = Paragraph::new(lines)
            .block(
                Block::default()
                    .title(format!("{} ({})", column.name, column.card_count))
                    .borders(Borders::ALL)
                    .border_style(border_style),
            )
            .wrap(Wrap { trim: false });
        frame.render_widget(paragraph, column_area);
    }

    let preview = Paragraph::new(view.preview_lines())
        .block(
            Block::default()
                .title(view.preview_title())
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(preview, layout[1]);
}

fn format_kanban_card_label(card: &vulcan_core::KanbanCardRecord) -> String {
    if let Some(task) = card.task.as_ref() {
        format!("[{}] {}", task.status_char, card.text)
    } else {
        format!("- {}", card.text)
    }
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
    Calendar,
}

impl BrowseMode {
    fn label(self) -> &'static str {
        match self {
            Self::Fuzzy => "fuzzy",
            Self::FullText => "full-text",
            Self::Tag => "tag",
            Self::Property => "property",
            Self::Calendar => "calendar",
        }
    }

    fn query_title(self) -> &'static str {
        match self {
            Self::Fuzzy => "Browse (/ fuzzy search)",
            Self::FullText => "Browse (Ctrl-F full-text)",
            Self::Tag => "Browse (Ctrl-T tag filter)",
            Self::Property => "Browse (Ctrl-P property filter)",
            Self::Calendar => "Browse (Ctrl-Y calendar)",
        }
    }

    fn help_line(self) -> &'static str {
        match self {
            Self::Fuzzy => "type to filter by path, filename, or alias",
            Self::FullText => {
                "type to search indexed content; inline operators, Ctrl-S sort, Alt-C case, Ctrl-E explain"
            }
            Self::Tag => "type a tag name; notes show the best matching indexed tag",
            Self::Property => "type a where-style predicate like status = active",
            Self::Calendar => {
                "arrow keys move the selected day; PageUp/PageDown change month; type YYYY-MM or YYYY-MM-DD to jump"
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PreviewMode {
    File,
    Dataview,
}

impl PreviewMode {
    fn toggle(self) -> Self {
        match self {
            Self::File => Self::Dataview,
            Self::Dataview => Self::File,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Dataview => "dataview",
        }
    }
}

#[derive(Debug, Clone, Default)]
struct CachedDataviewPreview {
    path: Option<String>,
    lines: Vec<String>,
    stale: bool,
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

#[derive(Debug)]
struct BackgroundRefreshState {
    receiver: Receiver<Result<ScanSummary, String>>,
}

impl BackgroundRefreshState {
    fn spawn(paths: VaultPaths) -> Self {
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let result =
                scan_vault(&paths, ScanMode::Incremental).map_err(|error| error.to_string());
            let _ = sender.send(result);
        });
        Self { receiver }
    }

    fn try_finish(&self) -> Option<Result<ScanSummary, String>> {
        match self.receiver.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err(
                "background refresh thread ended unexpectedly".to_string(),
            )),
        }
    }
}

#[derive(Debug)]
struct BrowseState {
    paths: VaultPaths,
    all_notes: Vec<NoteIdentity>,
    picker: NotePickerState,
    full_text: FullTextState,
    tag_filter: TagFilterState,
    property_filter: PropertyFilterState,
    calendar: CalendarViewState,
    kanban_view: Option<KanbanBoardViewState>,
    backlinks_view: Option<BacklinksViewState>,
    links_view: Option<OutgoingLinksViewState>,
    git_view: Option<GitLogViewState>,
    doctor_view: Option<DoctorViewState>,
    new_note_prompt: Option<NewNotePrompt>,
    move_prompt: Option<MovePrompt>,
    background_refresh: Option<BackgroundRefreshState>,
    last_scan_label: String,
    mode: BrowseMode,
    preview_mode: PreviewMode,
    dataview_preview: CachedDataviewPreview,
    status: String,
}

impl BrowseState {
    #[allow(clippy::needless_pass_by_value)]
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
            property_filter: PropertyFilterState::new(paths.clone()),
            calendar: CalendarViewState::new(&paths)?,
            kanban_view: None,
            backlinks_view: None,
            links_view: None,
            git_view: None,
            doctor_view: None,
            new_note_prompt: None,
            move_prompt: None,
            background_refresh: None,
            last_scan_label,
            mode: BrowseMode::Fuzzy,
            preview_mode: PreviewMode::File,
            dataview_preview: CachedDataviewPreview::default(),
            status: "Ready.".to_string(),
        })
    }

    #[allow(clippy::too_many_lines)]
    fn handle_key(&mut self, key: KeyEvent) -> BrowseAction {
        if self.new_note_prompt.is_some() {
            return self.handle_new_note_prompt_key(key.code);
        }
        if self.move_prompt.is_some() {
            return self.handle_move_prompt_key(key.code);
        }
        if self.kanban_view.is_some() {
            return self.handle_kanban_key(key.code);
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
                KeyCode::Char('v' | 'V') => {
                    self.clear_status();
                    self.toggle_preview_mode();
                    return BrowseAction::Continue;
                }
                KeyCode::Char('e' | 'E') => {
                    self.clear_status();
                    if self.mode == BrowseMode::FullText {
                        if let Err(error) = self.handle_mode_key(key) {
                            self.set_status(error);
                        } else {
                            self.refresh_dataview_preview_if_needed();
                        }
                        return BrowseAction::Continue;
                    }
                    return self.edit_selected_path();
                }
                KeyCode::Char('n' | 'N') => {
                    self.clear_status();
                    self.new_note_prompt = Some(NewNotePrompt::default());
                    return BrowseAction::Continue;
                }
                KeyCode::Char('r' | 'R') => {
                    self.clear_status();
                    self.open_move_prompt();
                    return BrowseAction::Continue;
                }
                KeyCode::Char('b' | 'B') => {
                    self.clear_status();
                    if let Err(error) = self.open_backlinks_view() {
                        self.set_status(error);
                    }
                    return BrowseAction::Continue;
                }
                KeyCode::Char('o' | 'O') => {
                    self.clear_status();
                    if let Err(error) = self.open_links_view() {
                        self.set_status(error);
                    }
                    return BrowseAction::Continue;
                }
                KeyCode::Char('d' | 'D') => {
                    self.clear_status();
                    if let Err(error) = self.open_doctor_view() {
                        self.set_status(error);
                    }
                    return BrowseAction::Continue;
                }
                KeyCode::Char('g' | 'G') => {
                    self.clear_status();
                    if let Err(error) = self.open_git_view() {
                        self.set_status(error);
                    }
                    return BrowseAction::Continue;
                }
                KeyCode::Char('f' | 'F') => {
                    self.clear_status();
                    if let Err(error) = self.switch_mode(BrowseMode::FullText) {
                        self.set_status(error);
                    }
                    return BrowseAction::Continue;
                }
                KeyCode::Char('t' | 'T') => {
                    self.clear_status();
                    if let Err(error) = self.switch_mode(BrowseMode::Tag) {
                        self.set_status(error);
                    }
                    return BrowseAction::Continue;
                }
                KeyCode::Char('p' | 'P') => {
                    self.clear_status();
                    if let Err(error) = self.switch_mode(BrowseMode::Property) {
                        self.set_status(error);
                    }
                    return BrowseAction::Continue;
                }
                KeyCode::Char('y' | 'Y') => {
                    self.clear_status();
                    if let Err(error) = self.switch_mode(BrowseMode::Calendar) {
                        self.set_status(error);
                    }
                    return BrowseAction::Continue;
                }
                _ => {}
            }
        }

        if key.modifiers.is_empty()
            && key.code == KeyCode::Char('o')
            && self.query().is_empty()
            && self.selected_note_is_kanban_board()
        {
            self.clear_status();
            if let Err(error) = self.open_kanban_view() {
                self.set_status(error);
            }
            return BrowseAction::Continue;
        }

        match key.code {
            KeyCode::Esc => BrowseAction::Quit,
            KeyCode::Char('/') if self.mode != BrowseMode::Fuzzy => {
                self.clear_status();
                self.mode = BrowseMode::Fuzzy;
                self.refresh_dataview_preview_if_needed();
                BrowseAction::Continue
            }
            KeyCode::Enter => self.edit_selected_path(),
            _ => {
                self.clear_status();
                if let Err(error) = self.handle_mode_key(key) {
                    self.set_status(error);
                } else {
                    self.refresh_dataview_preview_if_needed();
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
                self.refresh_dataview_preview_if_needed();
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

    fn handle_kanban_key(&mut self, code: KeyCode) -> BrowseAction {
        let Some(view) = self.kanban_view.as_mut() else {
            return BrowseAction::Continue;
        };

        match code {
            KeyCode::Esc => {
                self.kanban_view = None;
                self.clear_status();
                self.refresh_dataview_preview_if_needed();
                BrowseAction::Continue
            }
            KeyCode::Left | KeyCode::Char('h') => {
                view.move_column(-1);
                BrowseAction::Continue
            }
            KeyCode::Right | KeyCode::Char('l') => {
                view.move_column(1);
                BrowseAction::Continue
            }
            KeyCode::Up | KeyCode::Char('k') => {
                view.move_card(-1);
                BrowseAction::Continue
            }
            KeyCode::Down | KeyCode::Char('j') => {
                view.move_card(1);
                BrowseAction::Continue
            }
            KeyCode::Enter | KeyCode::Char('e') => BrowseAction::Edit(view.path().to_string()),
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
                self.refresh_dataview_preview_if_needed();
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
                self.refresh_dataview_preview_if_needed();
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
                self.refresh_dataview_preview_if_needed();
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

    fn handle_mode_key(&mut self, key: KeyEvent) -> Result<(), String> {
        match self.mode {
            BrowseMode::Fuzzy => {
                match handle_picker_key(&mut self.picker, key.code) {
                    PickerAction::Continue | PickerAction::Cancel | PickerAction::Select => {}
                }
                Ok(())
            }
            BrowseMode::FullText => self.full_text.handle_key(&self.paths, key),
            BrowseMode::Tag => self
                .tag_filter
                .handle_key(&self.paths, &self.all_notes, key.code),
            BrowseMode::Property => {
                self.property_filter
                    .handle_key(&self.paths, &self.all_notes, key.code);
                Ok(())
            }
            BrowseMode::Calendar => self.calendar.handle_key(&self.paths, key.code),
        }
    }

    fn edit_selected_path(&mut self) -> BrowseAction {
        if self.mode == BrowseMode::Calendar
            && self.git_view.is_none()
            && self.doctor_view.is_none()
            && self.backlinks_view.is_none()
            && self.links_view.is_none()
            && self.new_note_prompt.is_none()
            && self.move_prompt.is_none()
        {
            if let Some(path) = self.calendar.selected_existing_path().map(str::to_string) {
                return BrowseAction::Edit(path);
            }
            if let Some(path) = self.calendar.target_path() {
                return BrowseAction::Create(path);
            }
        }
        if let Some(path) = self.selected_path().map(str::to_string) {
            BrowseAction::Edit(path)
        } else {
            self.set_status("No matching note selected.");
            BrowseAction::Continue
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
            BrowseMode::Calendar => self.calendar.refresh(&self.paths)?,
            BrowseMode::Fuzzy => {}
        }
        self.refresh_dataview_preview_if_needed();
        Ok(())
    }

    fn reload_after_edit(&mut self) -> Result<(), String> {
        let notes = load_notes(&self.paths)?;
        self.all_notes.clone_from(&notes);
        self.picker.replace_notes_preserve_selection(notes);
        self.full_text.refresh_results(&self.paths)?;
        self.tag_filter
            .refresh_results(&self.paths, &self.all_notes)?;
        self.property_filter
            .refresh_results(&self.paths, &self.all_notes);
        self.calendar.refresh(&self.paths)?;
        if let Some(view) = self.kanban_view.as_mut() {
            view.reload(&self.paths)?;
        }
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
        self.invalidate_dataview_preview();
        self.refresh_dataview_preview_if_needed();
        Ok(())
    }

    fn poll_background_refresh(&mut self) {
        let result = self
            .background_refresh
            .as_ref()
            .and_then(BackgroundRefreshState::try_finish);
        let Some(result) = result else {
            return;
        };
        self.background_refresh = None;
        self.apply_background_refresh_result(result);
    }

    fn apply_background_refresh_result(&mut self, result: Result<ScanSummary, String>) {
        match result {
            Ok(summary) => {
                if let Err(error) = self.reload_after_edit() {
                    self.set_status(error);
                    return;
                }

                if summary.added == 0 && summary.updated == 0 && summary.deleted == 0 {
                    self.set_status("Background refresh found no changes.");
                } else {
                    self.set_status(format!(
                        "Background refresh updated cache: {} added, {} updated, {} deleted.",
                        summary.added, summary.updated, summary.deleted
                    ));
                }
            }
            Err(error) => self.set_status(format!("Background refresh failed: {error}")),
        }
    }

    fn reload_after_move(&mut self, destination: &str) -> Result<(), String> {
        self.reload_after_new_note(destination)
    }

    fn reload_after_new_note(&mut self, path: &str) -> Result<(), String> {
        let notes = load_notes(&self.paths)?;
        self.all_notes.clone_from(&notes);
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
        self.calendar.refresh(&self.paths)?;
        self.refresh_last_scan_label();
        self.invalidate_dataview_preview();
        self.refresh_dataview_preview_if_needed();
        Ok(())
    }

    fn refresh_preview(&mut self) {
        match self.mode {
            BrowseMode::Fuzzy => self.picker.refresh_preview(),
            BrowseMode::FullText | BrowseMode::Calendar => {}
            BrowseMode::Tag => self.tag_filter.refresh_preview(),
            BrowseMode::Property => self.property_filter.refresh_preview(),
        }
        self.refresh_dataview_preview_if_needed();
    }

    fn uses_note_preview(&self) -> bool {
        self.kanban_view.is_none()
            && self.git_view.is_none()
            && self.doctor_view.is_none()
            && self.backlinks_view.is_none()
            && self.links_view.is_none()
            && self.new_note_prompt.is_none()
            && self.move_prompt.is_none()
    }

    fn uses_dataview_preview(&self) -> bool {
        self.uses_note_preview()
            && self.mode != BrowseMode::Calendar
            && self.preview_mode == PreviewMode::Dataview
    }

    fn toggle_preview_mode(&mut self) {
        if self.mode == BrowseMode::Calendar {
            self.set_status("Calendar mode uses a fixed event preview.");
            return;
        }
        self.preview_mode = self.preview_mode.toggle();
        if self.preview_mode == PreviewMode::Dataview {
            self.refresh_dataview_preview();
        }
        self.set_status(format!("Preview mode: {}.", self.preview_mode.label()));
    }

    fn invalidate_dataview_preview(&mut self) {
        self.dataview_preview.stale = true;
    }

    fn refresh_dataview_preview_if_needed(&mut self) {
        if self.uses_dataview_preview() {
            self.refresh_dataview_preview();
        }
    }

    fn refresh_dataview_preview(&mut self) {
        let selected_path = self.selected_path().map(ToOwned::to_owned);
        if !self.dataview_preview.stale && self.dataview_preview.path == selected_path {
            return;
        }

        self.dataview_preview.path.clone_from(&selected_path);
        self.dataview_preview.lines = selected_path.map_or_else(
            || vec!["No matching note selected.".to_string()],
            |path| build_dataview_preview(&self.paths, &path),
        );
        self.dataview_preview.stale = false;
    }

    fn selected_path(&self) -> Option<&str> {
        if let Some(view) = self.kanban_view.as_ref() {
            return Some(view.path());
        }
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
            BrowseMode::Calendar => self.calendar.selected_existing_path(),
        }
    }

    fn query(&self) -> &str {
        if let Some(view) = self.kanban_view.as_ref() {
            return view.path();
        }
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
            BrowseMode::Calendar => self.calendar.query(),
        }
    }

    fn query_title(&self) -> String {
        if let Some(view) = self.kanban_view.as_ref() {
            return format!("Kanban ({})", view.path());
        }
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
        if self.kanban_view.is_some() {
            return "Keys: Esc back, h/l switch columns, j/k move cards, Enter/e edit board"
                .to_string();
        }
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
            match self.mode {
                BrowseMode::Fuzzy => "Keys: type to filter, Up/Down move, Enter/Ctrl-E edit, Ctrl-V preview, o open Kanban board when query is empty, Ctrl-N new, Ctrl-R move, Ctrl-B backlinks, Ctrl-O links, Ctrl-D doctor, Ctrl-G git, Ctrl-F full-text, Ctrl-T tags, Ctrl-P props, Ctrl-Y calendar, Esc quit".to_string(),
                BrowseMode::FullText => "Keys: type to search, Backspace edit query, Up/Down move, Enter edit, Ctrl-V preview, Ctrl-E explain, Ctrl-S sort, Alt-C case, Ctrl-B backlinks, Ctrl-O links, Ctrl-D doctor, Ctrl-G git, Ctrl-Y calendar, / fuzzy, Esc quit".to_string(),
                BrowseMode::Tag => "Keys: type to filter tags, Backspace edit query, Up/Down move, Enter/Ctrl-E edit, Ctrl-V preview, o open Kanban board when query is empty, Ctrl-B backlinks, Ctrl-O links, Ctrl-D doctor, Ctrl-G git, Ctrl-Y calendar, / fuzzy, Esc quit".to_string(),
                BrowseMode::Property => "Keys: type a predicate, Backspace edit query, Up/Down move, Enter/Ctrl-E edit, Ctrl-V preview, o open Kanban board when query is empty, Ctrl-B backlinks, Ctrl-O links, Ctrl-D doctor, Ctrl-G git, Ctrl-Y calendar, / fuzzy, Esc quit".to_string(),
                BrowseMode::Calendar => "Keys: arrows move day, PageUp/PageDown change month, Home/End jump month, Enter edit or create daily note, type YYYY-MM or YYYY-MM-DD to jump, / fuzzy, Esc quit".to_string(),
            }
        }
    }

    fn mode_help_line(&self) -> String {
        if self.kanban_view.is_some() {
            return "view Kanban columns side-by-side for the selected board; Esc returns to browse"
                .to_string();
        }
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
        if self.kanban_view.is_some() {
            "Kanban"
        } else if self.git_view.is_some() {
            "Git Log"
        } else if self.doctor_view.is_some() {
            "Diagnostics"
        } else if self.backlinks_view.is_some() {
            "Backlinks"
        } else if self.links_view.is_some() {
            "Links"
        } else if self.mode == BrowseMode::Calendar {
            "Calendar"
        } else {
            "Notes"
        }
    }

    fn list_items(&self) -> Vec<String> {
        if let Some(view) = self.kanban_view.as_ref() {
            return view
                .board
                .columns
                .iter()
                .map(|column| format!("{} ({})", column.name, column.card_count))
                .collect();
        }
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
                .map(|hit| {
                    let suffix = if self.full_text.sort() == SearchSort::Relevance {
                        format!(" [{:.3}]", hit.rank)
                    } else if let Some(line) = hit.matched_line {
                        format!(" [line {line}]")
                    } else {
                        String::new()
                    };
                    format!("{}{}", search_hit_location(hit), suffix)
                })
                .collect(),
            BrowseMode::Tag => self.tag_filter.list_items(),
            BrowseMode::Property => self.property_filter.list_items(),
            BrowseMode::Calendar => self.calendar.list_items(),
        }
    }

    fn selected_index(&self) -> Option<usize> {
        if let Some(view) = self.kanban_view.as_ref() {
            return Some(view.selected_column);
        }
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
            BrowseMode::Calendar => self.calendar.selected_index(),
        }
    }

    fn preview_title(&self) -> String {
        if let Some(view) = self.kanban_view.as_ref() {
            return view.preview_title();
        }
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
        if self.uses_dataview_preview() {
            return self.selected_path().map_or_else(
                || "Dataview Preview".to_string(),
                |path| format!("Dataview: {path}"),
            );
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
            BrowseMode::Calendar => self.calendar.preview_title(),
        }
    }

    fn preview_lines(&self) -> Vec<Line<'static>> {
        if let Some(view) = self.kanban_view.as_ref() {
            return view.preview_lines();
        }
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
        if self.uses_dataview_preview() {
            return self
                .dataview_preview
                .lines
                .iter()
                .cloned()
                .map(Line::from)
                .collect();
        }
        match self.mode {
            BrowseMode::Fuzzy => self.picker.preview_lines(),
            BrowseMode::FullText => self.full_text.preview_lines(),
            BrowseMode::Tag => self.tag_filter.preview_lines(),
            BrowseMode::Property => self.property_filter.preview_lines(),
            BrowseMode::Calendar => self.calendar.preview_lines(),
        }
    }

    fn filtered_count(&self) -> usize {
        if let Some(view) = self.kanban_view.as_ref() {
            return view.filtered_count();
        }
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
            BrowseMode::Calendar => self.calendar.filtered_count(),
        }
    }

    fn total_notes(&self) -> usize {
        self.picker.total_notes()
    }

    fn status_bar_line(&self) -> String {
        let preview_meta = (self.uses_note_preview() && self.mode != BrowseMode::Calendar)
            .then(|| format!(" | Preview: {}", self.preview_mode.label()));
        let full_text_meta = (self.mode == BrowseMode::FullText
            && self.kanban_view.is_none()
            && self.git_view.is_none()
            && self.doctor_view.is_none()
            && self.backlinks_view.is_none()
            && self.links_view.is_none()
            && self.new_note_prompt.is_none()
            && self.move_prompt.is_none())
        .then(|| {
            format!(
                " | Search: {} | Case: {} | Explain: {}",
                search_sort_label(self.full_text.sort()),
                if self.full_text.match_case() {
                    "sensitive"
                } else {
                    "insensitive"
                },
                if self.full_text.show_explain() {
                    "on"
                } else {
                    "off"
                }
            )
        });
        format!(
            "Vault: {} | Mode: {} | Notes: {} filtered / {} total{} | Last scan: {}{}",
            self.vault_name(),
            self.active_mode_label(),
            self.filtered_count(),
            self.total_notes(),
            preview_meta.as_deref().unwrap_or_default(),
            self.last_scan_label,
            if self.background_refresh.is_some() {
                " | Refresh: background"
            } else {
                ""
            },
        ) + full_text_meta.as_deref().unwrap_or("")
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

    fn open_kanban_view(&mut self) -> Result<(), String> {
        let Some(path) = self.selected_path().map(str::to_string) else {
            self.set_status("No matching note selected.");
            return Ok(());
        };
        self.kanban_view = Some(KanbanBoardViewState::load(&self.paths, &path)?);
        self.backlinks_view = None;
        self.links_view = None;
        self.git_view = None;
        self.doctor_view = None;
        Ok(())
    }

    fn open_backlinks_view(&mut self) -> Result<(), String> {
        let Some(path) = self.selected_path().map(str::to_string) else {
            self.set_status("No matching note selected.");
            return Ok(());
        };
        self.backlinks_view = Some(BacklinksViewState::load(&self.paths, &path)?);
        self.kanban_view = None;
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
        self.kanban_view = None;
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
        self.kanban_view = None;
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
        self.kanban_view = None;
        self.backlinks_view = None;
        self.links_view = None;
        self.doctor_view = None;
        Ok(())
    }

    fn selected_note_is_kanban_board(&self) -> bool {
        let Some(path) = self.selected_path() else {
            return false;
        };
        list_kanban_boards(&self.paths)
            .map(|boards| boards.into_iter().any(|board| board.path == path))
            .unwrap_or(false)
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
        if let Some(view) = self.kanban_view.as_ref() {
            return format!(
                "Kanban board {} ({} columns, {} cards)",
                view.path(),
                view.board.columns.len(),
                view.filtered_count()
            );
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
        if self.background_refresh.is_some() {
            return "Refreshing cache in background.".to_string();
        }

        match self.mode {
            BrowseMode::FullText if self.full_text.show_explain() => self
                .full_text
                .explain_summary()
                .unwrap_or_else(|| "Ready.".to_string()),
            BrowseMode::Tag => self
                .tag_filter
                .active_tag
                .as_deref()
                .map_or_else(|| "Ready.".to_string(), |tag| format!("Tag: #{tag}")),
            BrowseMode::Property => self.property_filter.status_line(),
            BrowseMode::Calendar => {
                format!("Calendar day: {}", self.calendar.selected_date.iso_string())
            }
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
        if self.kanban_view.is_some() {
            "kanban"
        } else if self.doctor_view.is_some() {
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
struct KanbanBoardViewState {
    board: KanbanBoardRecord,
    selected_column: usize,
    selected_card: usize,
}

impl KanbanBoardViewState {
    fn load(paths: &VaultPaths, board: &str) -> Result<Self, String> {
        let board = load_kanban_board(paths, board, false).map_err(|error| error.to_string())?;
        Ok(Self {
            board,
            selected_column: 0,
            selected_card: 0,
        })
    }

    fn reload(&mut self, paths: &VaultPaths) -> Result<(), String> {
        let board_path = self.board.path.clone();
        let selected_column_name = self
            .board
            .columns
            .get(self.selected_column)
            .map(|column| column.name.clone());
        let selected_card_id = self.selected_card().map(|card| card.id.clone());
        self.board =
            load_kanban_board(paths, &board_path, false).map_err(|error| error.to_string())?;
        if let Some(column_name) = selected_column_name.as_deref() {
            self.selected_column = self
                .board
                .columns
                .iter()
                .position(|column| column.name == column_name)
                .unwrap_or(0);
        } else {
            self.selected_column = 0;
        }
        if let Some(card_id) = selected_card_id.as_deref() {
            self.selected_card = self
                .board
                .columns
                .get(self.selected_column)
                .and_then(|column| column.cards.iter().position(|card| card.id == card_id))
                .unwrap_or(0);
        } else {
            self.selected_card = 0;
        }
        self.clamp_selection();
        Ok(())
    }

    fn path(&self) -> &str {
        &self.board.path
    }

    fn filtered_count(&self) -> usize {
        self.board
            .columns
            .iter()
            .map(|column| column.card_count)
            .sum()
    }

    fn preview_title(&self) -> String {
        self.selected_card().map_or_else(
            || format!("Kanban: {}", self.path()),
            |card| format!("Card: {}", card.text),
        )
    }

    fn preview_lines(&self) -> Vec<Line<'static>> {
        let Some(column) = self.board.columns.get(self.selected_column) else {
            return vec![Line::from("No Kanban columns in this board.")];
        };
        let Some(card) = self.selected_card() else {
            return vec![
                Line::from(format!("Column: {}", column.name)),
                Line::from("No cards in the selected column."),
            ];
        };

        let mut lines = vec![
            Line::from(format!("Board: {}", self.board.title)),
            Line::from(format!("Column: {}", column.name)),
            Line::from(format!("Line: {}", card.line_number)),
            Line::from(format!("Text: {}", card.text)),
        ];
        if let Some(task) = card.task.as_ref() {
            lines.push(Line::from(format!(
                "Task: {} ({})",
                task.status_name, task.status_type
            )));
        }
        if !card.tags.is_empty() {
            lines.push(Line::from(format!("Tags: {}", card.tags.join(", "))));
        }
        if !card.outlinks.is_empty() {
            lines.push(Line::from(format!("Links: {}", card.outlinks.join(", "))));
        }
        if let Some(date) = card.date.as_deref() {
            lines.push(Line::from(format!("Date: {date}")));
        }
        if let Some(time) = card.time.as_deref() {
            lines.push(Line::from(format!("Time: {time}")));
        }
        if let Some(block_id) = card.block_id.as_deref() {
            lines.push(Line::from(format!("Block: ^{block_id}")));
        }
        if let Some(inline_fields) = json_preview_line("Inline fields", &card.inline_fields) {
            lines.push(Line::from(inline_fields));
        }
        if let Some(metadata) = json_preview_line("Metadata", &card.metadata) {
            lines.push(Line::from(metadata));
        }
        lines
    }

    fn move_column(&mut self, delta: isize) {
        let len = self.board.columns.len();
        if len == 0 {
            self.selected_column = 0;
            self.selected_card = 0;
            return;
        }
        let current = self.selected_column;
        let step = delta.unsigned_abs();
        self.selected_column = if delta.is_negative() {
            current.saturating_sub(step)
        } else {
            current.saturating_add(step)
        }
        .min(len - 1);
        self.clamp_selection();
    }

    fn move_card(&mut self, delta: isize) {
        let Some(column) = self.board.columns.get(self.selected_column) else {
            self.selected_card = 0;
            return;
        };
        let len = column.cards.len();
        if len == 0 {
            self.selected_card = 0;
            return;
        }
        let current = self.selected_card;
        let step = delta.unsigned_abs();
        self.selected_card = if delta.is_negative() {
            current.saturating_sub(step)
        } else {
            current.saturating_add(step)
        }
        .min(len - 1);
    }

    fn clamp_selection(&mut self) {
        let Some(column) = self.board.columns.get(self.selected_column) else {
            self.selected_column = 0;
            self.selected_card = 0;
            return;
        };
        if column.cards.is_empty() {
            self.selected_card = 0;
        } else {
            self.selected_card = self.selected_card.min(column.cards.len() - 1);
        }
    }

    fn selected_card(&self) -> Option<&vulcan_core::KanbanCardRecord> {
        self.board
            .columns
            .get(self.selected_column)
            .and_then(|column| column.cards.get(self.selected_card))
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
                resolution_status_label(&link.resolution_status)
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
                            resolution_status_label(&link.resolution_status)
                        )
                    },
                    |context| {
                        format!(
                            "{} [{} {} line {}]",
                            outgoing_link_target_label(link),
                            link.link_kind,
                            resolution_status_label(&link.resolution_status),
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

fn resolution_status_label(status: &ResolutionStatus) -> &'static str {
    match status {
        ResolutionStatus::Resolved => "resolved",
        ResolutionStatus::Unresolved => "unresolved",
        ResolutionStatus::External => "external",
    }
}

fn is_base_path(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("base"))
}

fn json_preview_line(label: &str, value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::Object(object) if object.is_empty() => None,
        Value::Array(items) if items.is_empty() => None,
        _ => Some(format!("{label}: {value}")),
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CalendarDate {
    year: i64,
    month: i64,
    day: i64,
}

impl CalendarDate {
    fn iso_string(self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

#[derive(Debug, Clone)]
struct CalendarDayCell {
    date: CalendarDate,
    path: Option<String>,
    expected_path: Option<String>,
    events: Vec<vulcan_core::PeriodicEvent>,
    in_month: bool,
}

#[derive(Debug, Clone)]
struct CalendarViewState {
    query: String,
    config: PeriodicConfig,
    start_of_week: PeriodicStartOfWeek,
    visible_month: CalendarDate,
    selected_date: CalendarDate,
    cells: Vec<CalendarDayCell>,
}

impl CalendarViewState {
    fn new(paths: &VaultPaths) -> Result<Self, String> {
        let selected_date = parse_calendar_date(&super::current_utc_date_string())
            .ok_or_else(|| "failed to resolve today's date for the calendar view".to_string())?;
        let config = load_vault_config(paths).config.periodic;
        let start_of_week = calendar_start_of_week(&config);
        let mut state = Self {
            query: String::new(),
            config,
            start_of_week,
            visible_month: calendar_month_start(selected_date),
            selected_date,
            cells: Vec::new(),
        };
        state.refresh(paths)?;
        Ok(state)
    }

    fn refresh(&mut self, paths: &VaultPaths) -> Result<(), String> {
        self.config = load_vault_config(paths).config.periodic;
        self.start_of_week = calendar_start_of_week(&self.config);
        self.visible_month = calendar_month_start(self.selected_date);

        let month_start = self.visible_month;
        let month_end = calendar_month_end(month_start);
        let notes = if paths.cache_db().exists() {
            list_daily_note_events(paths, &month_start.iso_string(), &month_end.iso_string())
                .map_err(|error| error.to_string())?
        } else {
            Vec::new()
        };

        let notes_by_date = notes
            .into_iter()
            .map(|item| (item.date.clone(), item))
            .collect::<BTreeMap<_, _>>();

        let grid_start = add_calendar_days(
            month_start,
            -i64::try_from(calendar_weekday_index(month_start, self.start_of_week)).unwrap_or(0),
        );
        let end_offset = 6_i64
            - i64::try_from(calendar_weekday_index(month_end, self.start_of_week)).unwrap_or(0);
        let grid_end = add_calendar_days(month_end, end_offset);
        let total_days = days_from_civil(grid_end.year, grid_end.month, grid_end.day)
            - days_from_civil(grid_start.year, grid_start.month, grid_start.day)
            + 1;

        self.cells = (0..total_days)
            .map(|offset| {
                let date = add_calendar_days(grid_start, offset);
                let key = date.iso_string();
                let note = notes_by_date.get(&key);
                CalendarDayCell {
                    in_month: date.year == month_start.year && date.month == month_start.month,
                    path: note.map(|item| item.path.clone()),
                    expected_path: expected_periodic_note_path(&self.config, "daily", &key),
                    events: note.map_or_else(Vec::new, |item| item.events.clone()),
                    date,
                }
            })
            .collect();
        Ok(())
    }

    fn handle_key(&mut self, paths: &VaultPaths, code: KeyCode) -> Result<(), String> {
        match code {
            KeyCode::Left | KeyCode::Char('h') => {
                self.query.clear();
                self.selected_date = add_calendar_days(self.selected_date, -1);
                self.refresh(paths)
            }
            KeyCode::Right | KeyCode::Char('l') => {
                self.query.clear();
                self.selected_date = add_calendar_days(self.selected_date, 1);
                self.refresh(paths)
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.query.clear();
                self.selected_date = add_calendar_days(self.selected_date, -7);
                self.refresh(paths)
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.query.clear();
                self.selected_date = add_calendar_days(self.selected_date, 7);
                self.refresh(paths)
            }
            KeyCode::PageUp => {
                self.query.clear();
                self.selected_date = shift_calendar_month(self.selected_date, -1);
                self.refresh(paths)
            }
            KeyCode::PageDown => {
                self.query.clear();
                self.selected_date = shift_calendar_month(self.selected_date, 1);
                self.refresh(paths)
            }
            KeyCode::Home => {
                self.query.clear();
                self.selected_date = calendar_month_start(self.selected_date);
                self.refresh(paths)
            }
            KeyCode::End => {
                self.query.clear();
                self.selected_date = calendar_month_end(self.selected_date);
                self.refresh(paths)
            }
            KeyCode::Backspace => {
                self.query.pop();
                self.apply_query();
                self.refresh(paths)
            }
            KeyCode::Char(character) if character.is_ascii_digit() || character == '-' => {
                self.query.push(character);
                self.apply_query();
                self.refresh(paths)
            }
            _ => Ok(()),
        }
    }

    fn apply_query(&mut self) {
        let query = self.query.trim();
        if let Some(date) = parse_calendar_date(query) {
            self.selected_date = date;
        } else if let Some(month) = parse_calendar_month(query) {
            self.selected_date = CalendarDate {
                year: month.year,
                month: month.month,
                day: self
                    .selected_date
                    .day
                    .min(days_in_month(month.year, month.month)),
            };
        }
    }

    fn query(&self) -> &str {
        &self.query
    }

    fn title(&self) -> String {
        format!("Calendar ({})", calendar_month_label(self.visible_month))
    }

    fn list_items(&self) -> Vec<String> {
        self.cells
            .iter()
            .filter(|cell| cell.in_month)
            .filter_map(|cell| {
                cell.path.as_ref().map(|path| {
                    format!(
                        "{} {} ({} event(s))",
                        cell.date.iso_string(),
                        path,
                        cell.events.len()
                    )
                })
            })
            .collect()
    }

    fn selected_index(&self) -> Option<usize> {
        self.cells
            .iter()
            .position(|cell| cell.date == self.selected_date)
    }

    fn selected_existing_path(&self) -> Option<&str> {
        self.selected_cell().and_then(|cell| cell.path.as_deref())
    }

    fn target_path(&self) -> Option<String> {
        self.selected_cell()
            .and_then(|cell| cell.path.clone().or_else(|| cell.expected_path.clone()))
    }

    fn preview_title(&self) -> String {
        format!("Calendar: {}", self.selected_date.iso_string())
    }

    fn preview_lines(&self) -> Vec<Line<'static>> {
        let Some(cell) = self.selected_cell() else {
            return vec![Line::from("No day selected.")];
        };

        let mut lines = vec![Line::from(format!("Date: {}", cell.date.iso_string()))];
        match (cell.path.as_deref(), cell.expected_path.as_deref()) {
            (Some(path), _) => lines.push(Line::from(format!("Note: {path}"))),
            (None, Some(path)) => lines.push(Line::from(format!("Note: missing ({path})"))),
            (None, None) => lines.push(Line::from("Note: daily notes are disabled in config")),
        }
        lines.push(Line::from(format!("Events: {}", cell.events.len())));
        if cell.events.is_empty() {
            lines.push(Line::from("- no events"));
        } else {
            for event in cell.events.iter().take(CALENDAR_EVENT_PREVIEW_LIMIT) {
                let label = match event.end_time.as_deref() {
                    Some(end_time) => {
                        format!("- {}-{} {}", event.start_time, end_time, event.title)
                    }
                    None => format!("- {} {}", event.start_time, event.title),
                };
                lines.push(Line::from(label));
            }
            let hidden = cell
                .events
                .len()
                .saturating_sub(CALENDAR_EVENT_PREVIEW_LIMIT);
            if hidden > 0 {
                lines.push(Line::from(format!("... {hidden} more event(s)")));
            }
        }
        lines
    }

    fn filtered_count(&self) -> usize {
        self.cells
            .iter()
            .filter(|cell| cell.in_month && cell.path.is_some())
            .count()
    }

    fn calendar_lines(&self) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        let header = calendar_weekday_headers(self.start_of_week)
            .into_iter()
            .map(|label| {
                Span::styled(
                    format!("{label:^4}"),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )
            })
            .collect::<Vec<_>>();
        lines.push(Line::from(header));

        for week in self.cells.chunks(7) {
            let mut spans = Vec::new();
            for cell in week {
                let marker = if !cell.events.is_empty() {
                    '*'
                } else if cell.path.is_some() {
                    '+'
                } else {
                    ' '
                };
                let text = format!("{:>2}{marker} ", cell.date.day);
                let mut style = if cell.in_month {
                    Style::default().fg(Color::White)
                } else {
                    Style::default().fg(Color::DarkGray)
                };
                if cell.path.is_some() {
                    style = style.fg(Color::Cyan);
                }
                if !cell.events.is_empty() {
                    style = style.fg(Color::Yellow);
                }
                if cell.date == self.selected_date {
                    style = style.bg(Color::DarkGray).add_modifier(Modifier::BOLD);
                }
                spans.push(Span::styled(text, style));
            }
            lines.push(Line::from(spans));
        }

        lines.push(Line::from("Legend: + note  * note with events"));
        lines
    }

    fn selected_cell(&self) -> Option<&CalendarDayCell> {
        self.cells
            .iter()
            .find(|cell| cell.date == self.selected_date)
    }
}

fn calendar_start_of_week(config: &PeriodicConfig) -> PeriodicStartOfWeek {
    config
        .note("weekly")
        .map_or(PeriodicStartOfWeek::Monday, |weekly| weekly.start_of_week)
}

fn calendar_weekday_headers(start_of_week: PeriodicStartOfWeek) -> [&'static str; 7] {
    match start_of_week {
        PeriodicStartOfWeek::Monday => ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"],
        PeriodicStartOfWeek::Sunday => ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"],
        PeriodicStartOfWeek::Saturday => ["Sat", "Sun", "Mon", "Tue", "Wed", "Thu", "Fri"],
    }
}

fn calendar_month_label(date: CalendarDate) -> String {
    const MONTHS: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    let index = usize::try_from(date.month.saturating_sub(1)).unwrap_or(0);
    let name = MONTHS.get(index).copied().unwrap_or("Unknown");
    format!("{name} {}", date.year)
}

fn parse_calendar_month(value: &str) -> Option<CalendarDate> {
    let mut parts = value.split('-');
    let year = parts.next()?.parse().ok()?;
    let month = parts.next()?.parse().ok()?;
    (parts.next().is_none() && (1..=12).contains(&month)).then_some(CalendarDate {
        year,
        month,
        day: 1,
    })
}

fn parse_calendar_date(value: &str) -> Option<CalendarDate> {
    let mut parts = value.split('-');
    let year = parts.next()?.parse().ok()?;
    let month = parts.next()?.parse().ok()?;
    let day = parts.next()?.parse().ok()?;
    (parts.next().is_none() && valid_calendar_date(year, month, day)).then_some(CalendarDate {
        year,
        month,
        day,
    })
}

fn valid_calendar_date(year: i64, month: i64, day: i64) -> bool {
    if !(1..=12).contains(&month) {
        return false;
    }
    (1..=days_in_month(year, month)).contains(&day)
}

fn calendar_month_start(date: CalendarDate) -> CalendarDate {
    CalendarDate {
        year: date.year,
        month: date.month,
        day: 1,
    }
}

fn calendar_month_end(date: CalendarDate) -> CalendarDate {
    CalendarDate {
        year: date.year,
        month: date.month,
        day: days_in_month(date.year, date.month),
    }
}

fn shift_calendar_month(date: CalendarDate, delta: i64) -> CalendarDate {
    let month_index = date.year * 12 + (date.month - 1) + delta;
    let year = month_index.div_euclid(12);
    let month = month_index.rem_euclid(12) + 1;
    CalendarDate {
        year,
        month,
        day: date.day.min(days_in_month(year, month)),
    }
}

fn add_calendar_days(date: CalendarDate, delta: i64) -> CalendarDate {
    let shifted = civil_from_days(days_from_civil(date.year, date.month, date.day) + delta);
    CalendarDate {
        year: shifted.year,
        month: shifted.month,
        day: shifted.day,
    }
}

fn calendar_weekday_index(date: CalendarDate, start_of_week: PeriodicStartOfWeek) -> usize {
    usize::try_from(days_since_week_start(
        days_from_civil(date.year, date.month, date.day),
        start_of_week,
    ))
    .unwrap_or(0)
}

fn days_since_week_start(days_since_epoch: i64, start_of_week: PeriodicStartOfWeek) -> i64 {
    let weekday = (days_since_epoch + 3).rem_euclid(7);
    let week_start = match start_of_week {
        PeriodicStartOfWeek::Monday => 0,
        PeriodicStartOfWeek::Sunday => 6,
        PeriodicStartOfWeek::Saturday => 5,
    };
    (weekday - week_start).rem_euclid(7)
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

fn is_leap_year(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let adjusted_year = year - i64::from(month <= 2);
    let adjusted_month = if month <= 2 { month + 9 } else { month - 3 };
    let era = if adjusted_year >= 0 {
        adjusted_year
    } else {
        adjusted_year - 399
    } / 400;
    let year_of_era = adjusted_year - era * 400;
    let day_of_year = (153 * adjusted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn civil_from_days(days: i64) -> CalendarDate {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    };

    CalendarDate {
        year: if month <= 2 { year + 1 } else { year },
        month,
        day,
    }
}

#[derive(Debug, Clone, Default)]
struct FullTextState {
    query: String,
    hits: Vec<SearchHit>,
    selected_index: Option<usize>,
    sort: SearchSort,
    match_case: bool,
    show_explain: bool,
    explain_scroll: usize,
    plan_lines: Vec<String>,
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

    fn handle_key(&mut self, paths: &VaultPaths, key: KeyEvent) -> Result<(), String> {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('s' | 'S') => {
                    self.sort = next_search_sort(self.sort);
                    self.refresh_results(paths)?;
                }
                KeyCode::Char('e' | 'E') => {
                    self.show_explain = !self.show_explain;
                    self.explain_scroll = 0;
                    self.refresh_results(paths)?;
                }
                _ => {}
            }
            return Ok(());
        }

        if key.modifiers.contains(KeyModifiers::ALT) && matches!(key.code, KeyCode::Char('c' | 'C'))
        {
            self.match_case = !self.match_case;
            self.explain_scroll = 0;
            self.refresh_results(paths)?;
            return Ok(());
        }

        match key.code {
            KeyCode::Up => {
                self.move_selection(-1);
                Ok(())
            }
            KeyCode::Down => {
                self.move_selection(1);
                Ok(())
            }
            KeyCode::PageUp => {
                self.scroll_explain(-1);
                Ok(())
            }
            KeyCode::PageDown => {
                self.scroll_explain(1);
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
        let (hits, plan_lines) = self.query_results(paths)?;
        self.hits = hits;
        self.plan_lines = plan_lines;
        self.selected_index = selected_key.and_then(|(chunk_id, document_path)| {
            self.hits
                .iter()
                .position(|hit| hit.chunk_id == chunk_id && hit.document_path == document_path)
        });
        self.clamp_selection();
        self.clamp_explain_scroll();
        Ok(())
    }

    fn query_results(&self, paths: &VaultPaths) -> Result<(Vec<SearchHit>, Vec<String>), String> {
        if self.query.trim().is_empty() {
            return Ok((Vec::new(), Vec::new()));
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
                sort: Some(self.sort),
                match_case: self.match_case.then_some(true),
                limit: Some(FULL_TEXT_LIMIT),
                context_size: FULL_TEXT_CONTEXT_SIZE,
                raw_query: false,
                fuzzy: false,
                explain: self.show_explain,
            },
        )
        .map(|report| {
            (
                report.hits,
                report
                    .plan
                    .map_or_else(Vec::new, |plan| plan.parsed_query_explanation),
            )
        })
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

    fn scroll_explain(&mut self, delta: isize) {
        let step = delta.unsigned_abs();
        self.explain_scroll = if delta.is_negative() {
            self.explain_scroll.saturating_sub(step)
        } else {
            self.explain_scroll.saturating_add(step)
        };
        self.clamp_explain_scroll();
    }

    fn clamp_explain_scroll(&mut self) {
        let max_scroll = self.plan_lines.len().saturating_sub(1);
        self.explain_scroll = self.explain_scroll.min(max_scroll);
    }

    fn show_explain(&self) -> bool {
        self.show_explain
    }

    fn explain_preview_lines(&self) -> Vec<Line<'static>> {
        if !self.show_explain {
            return Vec::new();
        }
        if self.query.trim().is_empty() {
            return vec![Line::from("Type a full-text query, then toggle explain.")];
        }
        if self.plan_lines.is_empty() {
            return vec![Line::from("No parsed query plan available.")];
        }

        self.plan_lines
            .iter()
            .skip(self.explain_scroll)
            .map(|line| Line::from(line.clone()))
            .collect()
    }

    fn explain_summary(&self) -> Option<String> {
        self.show_explain.then(|| {
            if self.plan_lines.is_empty() {
                "Explain: no parsed query plan available".to_string()
            } else {
                format!(
                    "Explain: {}",
                    self.plan_lines
                        .iter()
                        .take(3)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(" | ")
                )
            }
        })
    }

    fn sort(&self) -> SearchSort {
        self.sort
    }

    fn match_case(&self) -> bool {
        self.match_case
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
            KeyCode::Up => {
                self.picker.move_selection(-1);
                Ok(())
            }
            KeyCode::Down => {
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

    fn handle_key(&mut self, paths: &VaultPaths, all_notes: &[NoteIdentity], code: KeyCode) {
        match code {
            KeyCode::Up => {
                self.picker.move_selection(-1);
            }
            KeyCode::Down => {
                self.picker.move_selection(1);
            }
            KeyCode::Backspace => {
                self.query.pop();
                self.refresh_results(paths, all_notes);
            }
            KeyCode::Char(character) => {
                self.query.push(character);
                self.refresh_results(paths, all_notes);
            }
            _ => {}
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

fn build_dataview_preview(paths: &VaultPaths, path: &str) -> Vec<String> {
    let note_index = match load_note_index(paths) {
        Ok(note_index) => note_index,
        Err(error) => return vec![format!("Failed to load Dataview note index: {error}")],
    };
    let Some(note) = note_index
        .values()
        .find(|note| note.document_path.as_str() == path)
    else {
        return vec![format!("Selected note is not indexed: {path}")];
    };

    let inline_results = evaluate_note_inline_expressions(note, &note_index);
    let blocks = match load_dataview_blocks(paths, path, None) {
        Ok(blocks) => blocks,
        Err(DqlEvalError::Message(message))
            if message.starts_with("no Dataview blocks found in ") =>
        {
            Vec::new()
        }
        Err(error) => return vec![format!("Failed to load Dataview blocks: {error}")],
    };

    if inline_results.is_empty() && blocks.is_empty() {
        return vec!["No Dataview inline expressions or blocks.".to_string()];
    }

    let mut lines = Vec::new();
    append_dataview_inline_preview(&mut lines, &inline_results);
    append_dataview_block_preview(paths, &mut lines, &blocks);
    truncate_preview_strings(
        lines,
        DATAVIEW_PREVIEW_LINE_LIMIT,
        "Dataview preview truncated",
    )
}

fn append_dataview_inline_preview(
    lines: &mut Vec<String>,
    inline_results: &[vulcan_core::EvaluatedInlineExpression],
) {
    if inline_results.is_empty() {
        return;
    }

    lines.push(format!("Inline expressions ({})", inline_results.len()));
    let visible = inline_results
        .iter()
        .take(DATAVIEW_INLINE_PREVIEW_LIMIT)
        .map(|result| {
            result.error.as_ref().map_or_else(
                || {
                    format!(
                        "- {} => {}",
                        result.expression,
                        render_preview_value(&result.value)
                    )
                },
                |error| format!("- {} => error: {error}", result.expression),
            )
        });
    lines.extend(visible);

    let hidden_count = inline_results
        .len()
        .saturating_sub(DATAVIEW_INLINE_PREVIEW_LIMIT);
    if hidden_count > 0 {
        lines.push(format!("... {hidden_count} more inline expression(s)"));
    }
}

fn append_dataview_block_preview(
    paths: &VaultPaths,
    lines: &mut Vec<String>,
    blocks: &[vulcan_core::DataviewBlockRecord],
) {
    if blocks.is_empty() {
        return;
    }

    if !lines.is_empty() {
        lines.push(String::new());
    }
    lines.push(format!("Dataview blocks ({})", blocks.len()));
    for block in blocks {
        lines.push(format!(
            "Block {} ({}, line {})",
            block.block_index, block.language, block.line_number
        ));
        let block_lines = dataview_block_preview_lines(paths, block);
        let visible_lines = block_lines
            .iter()
            .take(DATAVIEW_BLOCK_PREVIEW_LIMIT)
            .map(|line| format!("  {line}"));
        lines.extend(visible_lines);

        let hidden_count = block_lines
            .len()
            .saturating_sub(DATAVIEW_BLOCK_PREVIEW_LIMIT);
        if hidden_count > 0 {
            lines.push(format!("  ... {hidden_count} more line(s)"));
        }
    }
}

fn dataview_block_preview_lines(
    paths: &VaultPaths,
    block: &vulcan_core::DataviewBlockRecord,
) -> Vec<String> {
    match block.language.as_str() {
        "dataview" => match evaluate_dql(paths, &block.source, Some(&block.file)) {
            Ok(result) => dql_preview_lines(&result),
            Err(error) => vec![format!("error: {error}")],
        },
        "dataviewjs" => match evaluate_dataview_js_query(paths, &block.source, Some(&block.file)) {
            Ok(result) => dataview_js_preview_lines(&result),
            Err(error) => vec![format!("error: {error}")],
        },
        language => vec![format!(
            "error: unsupported Dataview block language `{language}`"
        )],
    }
}

fn dql_preview_lines(result: &DqlQueryResult) -> Vec<String> {
    let mut lines = match result.query_type {
        vulcan_core::dql::DqlQueryType::Table => dql_table_preview_lines(result),
        vulcan_core::dql::DqlQueryType::List => dql_list_preview_lines(result),
        vulcan_core::dql::DqlQueryType::Task => dql_task_preview_lines(result),
        vulcan_core::dql::DqlQueryType::Calendar => dql_calendar_preview_lines(result),
    };

    if result.rows.is_empty() {
        lines.push("No results.".to_string());
    } else {
        lines.push(format!("{} result(s)", result.result_count));
    }

    for diagnostic in &result.diagnostics {
        lines.push(format!("Diagnostic: {}", diagnostic.message));
    }
    lines
}

fn dql_table_preview_lines(result: &DqlQueryResult) -> Vec<String> {
    let mut lines = Vec::new();
    if !result.columns.is_empty() {
        lines.push(result.columns.join(" | "));
    }

    for row in result.rows.iter().take(3) {
        lines.push(
            result
                .columns
                .iter()
                .map(|column| render_preview_value(&row[column]))
                .collect::<Vec<_>>()
                .join(" | "),
        );
    }
    lines
}

fn dql_list_preview_lines(result: &DqlQueryResult) -> Vec<String> {
    result
        .rows
        .iter()
        .take(4)
        .map(|row| match result.columns.as_slice() {
            [column] => format!("- {}", render_preview_value(&row[column])),
            [left, right, ..] => format!(
                "- {}: {}",
                render_preview_value(&row[left]),
                render_preview_value(&row[right])
            ),
            [] => format!("- {}", render_preview_value(row)),
        })
        .collect()
}

fn dql_task_preview_lines(result: &DqlQueryResult) -> Vec<String> {
    result
        .rows
        .iter()
        .take(4)
        .map(|row| {
            let file_column = result.columns.first().map_or("File", String::as_str);
            let file = row[file_column].as_str().unwrap_or("<unknown>");
            let status = row["status"].as_str().unwrap_or(" ");
            let text = render_preview_value(&row["text"]);
            format!("{file}: - [{status}] {text}")
        })
        .collect()
}

fn dql_calendar_preview_lines(result: &DqlQueryResult) -> Vec<String> {
    result
        .rows
        .iter()
        .take(4)
        .map(|row| {
            let file_column = result.columns.get(1).map_or("File", String::as_str);
            let date = row["date"].as_str().unwrap_or_default();
            let file = render_preview_value(&row[file_column]);
            format!("{date}: {file}")
        })
        .collect()
}

fn dataview_js_preview_lines(result: &vulcan_core::DataviewJsResult) -> Vec<String> {
    let mut lines = Vec::new();

    if result.outputs.is_empty() {
        if let Some(value) = &result.value {
            lines.push(render_preview_value(value));
        } else {
            lines.push("No DataviewJS output.".to_string());
        }
        return lines;
    }

    for output in &result.outputs {
        match output {
            DataviewJsOutput::Query { result } => lines.extend(dql_preview_lines(result)),
            DataviewJsOutput::Table { headers, rows } => {
                if !headers.is_empty() {
                    lines.push(headers.join(" | "));
                }
                lines.extend(rows.iter().take(3).map(|row| {
                    row.iter()
                        .map(render_preview_value)
                        .collect::<Vec<_>>()
                        .join(" | ")
                }));
            }
            DataviewJsOutput::List { items } => lines.extend(
                items
                    .iter()
                    .take(4)
                    .map(|item| format!("- {}", render_preview_value(item))),
            ),
            DataviewJsOutput::TaskList {
                tasks,
                group_by_file: _,
            } => lines.extend(tasks.iter().take(4).map(dataview_js_task_preview_line)),
            DataviewJsOutput::Paragraph { text } | DataviewJsOutput::Span { text } => {
                lines.push(text.clone());
            }
            DataviewJsOutput::Header { level, text } => {
                lines.push(format!("{} {text}", "#".repeat((*level).max(1))));
            }
            DataviewJsOutput::Element {
                element,
                text,
                attrs: _,
            } => {
                lines.push(format!("<{element}> {text}"));
            }
        }
    }

    lines
}

fn dataview_js_task_preview_line(task: &Value) -> String {
    let file = task
        .get("path")
        .and_then(Value::as_str)
        .or_else(|| {
            task.get("file")
                .and_then(|file| file.get("path"))
                .and_then(Value::as_str)
        })
        .unwrap_or("<unknown>");
    let status = task.get("status").and_then(Value::as_str).unwrap_or(" ");
    let text = task
        .get("text")
        .map(render_preview_value)
        .unwrap_or_default();
    format!("{file}: - [{status}] {text}")
}

fn render_preview_value(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        _ => serde_json::to_string(value).expect("Dataview preview values should serialize"),
    }
}

fn truncate_preview_strings(
    mut lines: Vec<String>,
    limit: usize,
    truncated_label: &str,
) -> Vec<String> {
    if lines.len() <= limit {
        return lines;
    }

    let hidden_count = lines.len() - limit;
    lines.truncate(limit.saturating_sub(1));
    lines.push(format!(
        "... {truncated_label}; {hidden_count} more line(s)"
    ));
    lines
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

fn next_search_sort(sort: SearchSort) -> SearchSort {
    match sort {
        SearchSort::Relevance => SearchSort::PathAsc,
        SearchSort::PathAsc => SearchSort::PathDesc,
        SearchSort::PathDesc => SearchSort::ModifiedNewest,
        SearchSort::ModifiedNewest => SearchSort::ModifiedOldest,
        SearchSort::ModifiedOldest => SearchSort::CreatedNewest,
        SearchSort::CreatedNewest => SearchSort::CreatedOldest,
        SearchSort::CreatedOldest => SearchSort::Relevance,
    }
}

fn search_sort_label(sort: SearchSort) -> &'static str {
    match sort {
        SearchSort::Relevance => "relevance",
        SearchSort::PathAsc => "path-asc",
        SearchSort::PathDesc => "path-desc",
        SearchSort::ModifiedNewest => "modified-newest",
        SearchSort::ModifiedOldest => "modified-oldest",
        SearchSort::CreatedNewest => "created-newest",
        SearchSort::CreatedOldest => "created-oldest",
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
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
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

    fn alt(code: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(code), KeyModifiers::ALT)
    }

    fn write_note(root: &Path, relative_path: &str, contents: &str) {
        let absolute = root.join(relative_path);
        if let Some(parent) = absolute.parent() {
            fs::create_dir_all(parent).expect("note parent should be created");
        }
        fs::write(absolute, contents).expect("note should be written");
    }

    fn write_periodic_config(root: &Path, start_of_week: &str) {
        fs::create_dir_all(root.join(".vulcan")).expect("config dir should be created");
        fs::write(
            root.join(".vulcan/config.toml"),
            format!(
                "[periodic.daily]\nfolder = \"Journal/Daily\"\nformat = \"YYYY-MM-DD\"\nschedule_heading = \"Schedule\"\n\n[periodic.weekly]\nstart_of_week = \"{start_of_week}\"\n"
            ),
        )
        .expect("periodic config should be written");
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
        assert!(status.success(), "git command failed: {args:?}");
    }

    fn init_git_repo(vault_root: &Path) {
        run_git(vault_root, &["-c", "init.defaultBranch=main", "init"]);
        run_git(vault_root, &["config", "user.name", "Vulcan Test"]);
        run_git(vault_root, &["config", "user.email", "vulcan@example.com"]);
    }

    fn set_document_mtime(paths: &VaultPaths, path: &str, mtime: i64) {
        let database = vulcan_core::CacheDatabase::open(paths).expect("database should open");
        database
            .connection()
            .execute(
                "UPDATE documents SET file_mtime = ? WHERE path = ?",
                (mtime, path),
            )
            .expect("document mtime should update");
    }

    fn preview_text(lines: &[Line<'static>]) -> String {
        lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<Vec<_>>()
            .join("\n")
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
    fn ctrl_e_requests_edit_for_selected_note() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        write_note(temp_dir.path(), "Projects/Alpha.md", "Alpha");
        let mut state = BrowseState::new(paths, vec![note("Projects/Alpha.md", &[])])
            .expect("state should build");

        let action = state.handle_key(ctrl('e'));

        assert_eq!(action, BrowseAction::Edit("Projects/Alpha.md".to_string()));
    }

    #[test]
    fn lowercase_action_keys_extend_the_fuzzy_query() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        let mut state = BrowseState::new(
            paths,
            vec![
                note("Beta.md", &[]),
                note("Meeting.md", &[]),
                note("Notes.md", &[]),
            ],
        )
        .expect("state should build");

        assert_eq!(
            state.handle_key(key(KeyCode::Char('b'))),
            BrowseAction::Continue
        );
        assert_eq!(
            state.handle_key(key(KeyCode::Char('m'))),
            BrowseAction::Continue
        );
        assert_eq!(
            state.handle_key(key(KeyCode::Char('n'))),
            BrowseAction::Continue
        );
        assert_eq!(
            state.handle_key(key(KeyCode::Char('o'))),
            BrowseAction::Continue
        );
        assert_eq!(
            state.handle_key(key(KeyCode::Char('j'))),
            BrowseAction::Continue
        );
        assert_eq!(
            state.handle_key(key(KeyCode::Char('k'))),
            BrowseAction::Continue
        );

        assert_eq!(state.query(), "bmnojk");
        assert_eq!(state.query_title(), "Browse (/ fuzzy search)");
    }

    #[test]
    fn ctrl_r_opens_move_prompt_for_selected_note() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        write_note(temp_dir.path(), "Projects/Alpha.md", "Alpha");
        let mut state = BrowseState::new(paths, vec![note("Projects/Alpha.md", &[])])
            .expect("state should build");

        let action = state.handle_key(ctrl('r'));

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

        state.handle_key(ctrl('r'));
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
    fn ctrl_b_opens_backlinks_view_with_context() {
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

        let action = state.handle_key(ctrl('b'));

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
        state.handle_key(ctrl('b'));

        let action = state.handle_key(key(KeyCode::Esc));

        assert_eq!(action, BrowseAction::Continue);
        assert_eq!(state.query_title(), "Browse (/ fuzzy search)");
        assert_eq!(state.selected_path(), Some("Projects/Alpha.md"));
    }

    #[test]
    fn ctrl_o_opens_outgoing_links_view_with_context() {
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

        let action = state.handle_key(ctrl('o'));

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
        state.handle_key(ctrl('o'));

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
        state.handle_key(ctrl('o'));

        let action = state.handle_key(key(KeyCode::Char('o')));

        assert_eq!(
            action,
            BrowseAction::OpenBaseTui("release.base".to_string())
        );
    }

    #[test]
    fn o_opens_kanban_board_view_for_selected_board() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        write_note(
            temp_dir.path(),
            "Board.md",
            concat!(
                "---\n",
                "kanban-plugin: board\n",
                "---\n\n",
                "## Todo\n\n",
                "- Build release\n",
                "- [/] Review checklist\n\n",
                "## Done\n\n",
                "- Shipped\n",
            ),
        );
        scan_fixture(&paths);
        let mut state =
            BrowseState::new(paths, vec![note("Board.md", &[])]).expect("state should build");
        state.picker.select_path("Board.md");

        let action = state.handle_key(key(KeyCode::Char('o')));

        assert_eq!(action, BrowseAction::Continue);
        assert_eq!(state.query_title(), "Kanban (Board.md)");
        assert_eq!(state.selected_path(), Some("Board.md"));
        assert_eq!(state.filtered_count(), 3);
        assert_eq!(
            state.mode_help_line(),
            "view Kanban columns side-by-side for the selected board; Esc returns to browse"
        );
        assert!(preview_text(&state.preview_lines()).contains("Build release"));
    }

    #[test]
    fn kanban_view_esc_returns_to_browse() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        write_note(
            temp_dir.path(),
            "Board.md",
            concat!(
                "---\n",
                "kanban-plugin: board\n",
                "---\n\n",
                "## Todo\n\n",
                "- Build release\n",
            ),
        );
        scan_fixture(&paths);
        let mut state =
            BrowseState::new(paths, vec![note("Board.md", &[])]).expect("state should build");
        state.picker.select_path("Board.md");
        state.handle_key(key(KeyCode::Char('o')));

        let action = state.handle_key(key(KeyCode::Esc));

        assert_eq!(action, BrowseAction::Continue);
        assert_eq!(state.query_title(), "Browse (/ fuzzy search)");
        assert_eq!(state.selected_path(), Some("Board.md"));
    }

    #[test]
    fn ctrl_g_opens_git_log_view_for_selected_note() {
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

        let action = state.handle_key(ctrl('g'));

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
        state.handle_key(ctrl('g'));

        let action = state.handle_key(key(KeyCode::Esc));

        assert_eq!(action, BrowseAction::Continue);
        assert_eq!(state.query_title(), "Browse (/ fuzzy search)");
        assert_eq!(state.selected_path(), Some("Projects/Alpha.md"));
    }

    #[test]
    fn ctrl_d_opens_doctor_view_for_selected_note() {
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

        let action = state.handle_key(ctrl('d'));

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
        state.handle_key(ctrl('d'));

        let action = state.handle_key(key(KeyCode::Esc));

        assert_eq!(action, BrowseAction::Continue);
        assert_eq!(state.query_title(), "Browse (/ fuzzy search)");
        assert_eq!(state.selected_path(), Some("Projects/Alpha.md"));
    }

    #[test]
    fn ctrl_n_opens_new_note_prompt() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        let mut state =
            BrowseState::new(paths, vec![note("Home.md", &[])]).expect("state should build");

        let action = state.handle_key(ctrl('n'));

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

        state.handle_key(ctrl('n'));
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
    fn status_bar_line_marks_background_refresh() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("browse-vault");
        let paths = VaultPaths::new(&vault_root);
        write_note(&vault_root, "Home.md", "# Home");
        scan_fixture(&paths);
        let mut state =
            BrowseState::new(paths, vec![note("Home.md", &[])]).expect("state should build");
        let (_sender, receiver) = mpsc::channel();
        state.background_refresh = Some(BackgroundRefreshState { receiver });
        state.clear_status();

        assert!(state.status_bar_line().contains("Refresh: background"));
        assert_eq!(state.status_line(), "Refreshing cache in background.");
    }

    #[test]
    fn background_refresh_result_reloads_notes_with_minimal_disruption() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        write_note(temp_dir.path(), "Home.md", "# Home");
        scan_fixture(&paths);
        let mut state = BrowseState::new(paths.clone(), vec![note("Home.md", &[])])
            .expect("state should build");
        state.picker.select_path("Home.md");
        state.handle_key(key(KeyCode::Char('h')));
        state.handle_key(key(KeyCode::Char('o')));
        state.handle_key(key(KeyCode::Char('m')));
        state.handle_key(key(KeyCode::Char('e')));

        write_note(temp_dir.path(), "Projects/Alpha.md", "# Alpha");
        let summary = scan_vault(&paths, ScanMode::Incremental).expect("scan should succeed");
        state.apply_background_refresh_result(Ok(summary));

        assert_eq!(state.total_notes(), 2);
        assert_eq!(state.selected_path(), Some("Home.md"));
        assert_eq!(state.query(), "home");
        assert!(state
            .status_line()
            .contains("Background refresh updated cache"));
    }

    #[test]
    fn draw_handles_small_terminal_sizes() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("browse-vault");
        let paths = VaultPaths::new(&vault_root);
        write_note(&vault_root, "Home.md", "# Home");
        scan_fixture(&paths);
        let state =
            BrowseState::new(paths, vec![note("Home.md", &[])]).expect("state should build");

        let backend = TestBackend::new(36, 8);
        let mut terminal = Terminal::new(backend).expect("terminal should build");
        terminal
            .draw(|frame| draw(frame, &state))
            .expect("small browse render should succeed");
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
    fn full_text_query_keeps_j_and_k_characters() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        write_note(temp_dir.path(), "Projects/Jekyll.md", "jekyll knowledge");
        scan_fixture(&paths);
        let mut state = BrowseState::new(paths, vec![note("Projects/Jekyll.md", &[])])
            .expect("state should build");

        state.handle_key(ctrl('f'));
        state.handle_key(key(KeyCode::Char('j')));
        state.handle_key(key(KeyCode::Char('k')));

        assert_eq!(state.mode, BrowseMode::FullText);
        assert_eq!(state.query(), "jk");
    }

    #[test]
    fn full_text_query_supports_inline_operator_syntax() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        write_note(
            temp_dir.path(),
            "Done.md",
            "---\nstatus: done\n---\n\nrelease dashboard",
        );
        write_note(
            temp_dir.path(),
            "Backlog.md",
            "---\nstatus: backlog\n---\n\nrelease dashboard",
        );
        scan_fixture(&paths);
        let mut state =
            BrowseState::new(paths, vec![note("Done.md", &[]), note("Backlog.md", &[])])
                .expect("state should build");

        state.handle_key(ctrl('f'));
        for character in "[status:done]".chars() {
            state.handle_key(key(KeyCode::Char(character)));
        }

        assert_eq!(state.mode, BrowseMode::FullText);
        assert_eq!(state.filtered_count(), 1);
        assert_eq!(state.selected_path(), Some("Done.md"));
    }

    #[test]
    fn full_text_ctrl_s_cycles_sort_and_reorders_hits() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        write_note(temp_dir.path(), "Alpha.md", "dashboard");
        write_note(temp_dir.path(), "Beta.md", "dashboard");
        scan_fixture(&paths);
        let mut state = BrowseState::new(
            paths.clone(),
            vec![note("Alpha.md", &[]), note("Beta.md", &[])],
        )
        .expect("state should build");

        set_document_mtime(&paths, "Alpha.md", 100);
        set_document_mtime(&paths, "Beta.md", 300);

        state.handle_key(ctrl('f'));
        for character in "dashboard".chars() {
            state.handle_key(key(KeyCode::Char(character)));
        }
        assert_eq!(state.full_text.sort(), SearchSort::Relevance);

        state.handle_key(ctrl('s'));
        assert_eq!(state.full_text.sort(), SearchSort::PathAsc);

        state.handle_key(ctrl('s'));
        assert_eq!(state.full_text.sort(), SearchSort::PathDesc);
        assert!(state.list_items()[0].contains("Beta.md"));

        state.handle_key(ctrl('s'));
        assert_eq!(state.full_text.sort(), SearchSort::ModifiedNewest);
        assert!(state.list_items()[0].contains("Beta.md"));
    }

    #[test]
    fn full_text_alt_c_toggles_case_sensitive_results() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        write_note(temp_dir.path(), "Upper.md", "Bob builds dashboards.");
        write_note(temp_dir.path(), "Lower.md", "bob builds dashboards.");
        scan_fixture(&paths);
        let mut state = BrowseState::new(paths, vec![note("Upper.md", &[]), note("Lower.md", &[])])
            .expect("state should build");

        state.handle_key(ctrl('f'));
        for character in "Bob".chars() {
            state.handle_key(key(KeyCode::Char(character)));
        }
        assert_eq!(state.filtered_count(), 2);

        state.handle_key(alt('c'));

        assert!(state.full_text.match_case());
        assert_eq!(state.filtered_count(), 1);
        assert_eq!(state.selected_path(), Some("Upper.md"));
    }

    #[test]
    fn full_text_ctrl_e_toggles_explain_and_updates_status() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        write_note(
            temp_dir.path(),
            "Release.md",
            "---\nstatus: done\n---\n\nrelease dashboard",
        );
        scan_fixture(&paths);
        let mut state =
            BrowseState::new(paths, vec![note("Release.md", &[])]).expect("state should build");

        state.handle_key(ctrl('f'));
        for character in "dashboard [status:done] task-todo:ship".chars() {
            state.handle_key(key(KeyCode::Char(character)));
        }
        let action = state.handle_key(ctrl('e'));

        assert_eq!(action, BrowseAction::Continue);
        assert!(state.full_text.show_explain());
        let before_scroll = state.full_text.explain_preview_lines();
        assert!(!before_scroll.is_empty());
        assert!(state.status_line().contains("Explain:"));
        assert!(state.key_help_line().contains("Ctrl-E explain"));

        state.handle_key(key(KeyCode::PageDown));
        let after_scroll = state.full_text.explain_preview_lines();
        assert_ne!(before_scroll, after_scroll);
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
    fn tag_filter_query_keeps_j_and_k_characters() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        write_note(
            temp_dir.path(),
            "Projects/Jekyll.md",
            "---\ntags:\n  - jekyll\n---\n\n# Jekyll",
        );
        scan_fixture(&paths);
        let mut state = BrowseState::new(paths, vec![note("Projects/Jekyll.md", &[])])
            .expect("state should build");

        state.handle_key(ctrl('t'));
        state.handle_key(key(KeyCode::Char('j')));
        state.handle_key(key(KeyCode::Char('k')));

        assert_eq!(state.mode, BrowseMode::Tag);
        assert_eq!(state.query(), "jk");
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
    fn property_filter_query_keeps_j_and_k_characters() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        write_note(
            temp_dir.path(),
            "Projects/Jekyll.md",
            "---\nkind: jekyll\n---\n\n# Jekyll",
        );
        scan_fixture(&paths);
        let mut state = BrowseState::new(paths, vec![note("Projects/Jekyll.md", &[])])
            .expect("state should build");

        state.handle_key(ctrl('p'));
        state.handle_key(key(KeyCode::Char('j')));
        state.handle_key(key(KeyCode::Char('k')));

        assert_eq!(state.mode, BrowseMode::Property);
        assert_eq!(state.query(), "jk");
    }

    #[test]
    fn ctrl_y_switches_to_calendar_mode_and_previews_daily_events() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        write_periodic_config(temp_dir.path(), "sunday");
        let paths = VaultPaths::new(temp_dir.path());
        write_note(
            temp_dir.path(),
            "Journal/Daily/2026-04-03.md",
            "# 2026-04-03\n\n## Schedule\n- 09:00-10:00 Team standup\n",
        );
        write_note(
            temp_dir.path(),
            "Journal/Daily/2026-04-04.md",
            "# 2026-04-04\n\n## Schedule\n- all-day Company offsite\n",
        );
        scan_fixture(&paths);
        let mut state = BrowseState::new(
            paths,
            vec![
                note("Journal/Daily/2026-04-03.md", &[]),
                note("Journal/Daily/2026-04-04.md", &[]),
            ],
        )
        .expect("state should build");

        state.handle_key(ctrl('y'));
        for character in "2026-04-04".chars() {
            state.handle_key(key(KeyCode::Char(character)));
        }

        assert_eq!(state.mode, BrowseMode::Calendar);
        assert_eq!(state.query_title(), "Browse (Ctrl-Y calendar)");
        assert_eq!(state.selected_path(), Some("Journal/Daily/2026-04-04.md"));
        assert_eq!(state.filtered_count(), 2);
        let calendar = preview_text(&state.calendar.calendar_lines());
        assert!(calendar.contains("Sun"));
        assert!(calendar.contains("Mon"));
        assert!(calendar.contains(" 3* "));
        assert!(calendar.contains(" 4* "));
        assert!(calendar.contains("Legend: + note  * note with events"));
        assert!(preview_text(&state.preview_lines()).contains("Company offsite"));
        assert!(state
            .key_help_line()
            .contains("type YYYY-MM or YYYY-MM-DD to jump"));
    }

    #[test]
    fn calendar_mode_enter_creates_missing_daily_note() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        write_periodic_config(temp_dir.path(), "monday");
        let paths = VaultPaths::new(temp_dir.path());
        write_note(
            temp_dir.path(),
            "Journal/Daily/2026-04-03.md",
            "# 2026-04-03\n\n## Schedule\n- 09:00 Team standup\n",
        );
        scan_fixture(&paths);
        let mut state = BrowseState::new(paths, vec![note("Journal/Daily/2026-04-03.md", &[])])
            .expect("state should build");

        state.handle_key(ctrl('y'));
        for character in "2026-04-05".chars() {
            state.handle_key(key(KeyCode::Char(character)));
        }

        let action = state.handle_key(key(KeyCode::Enter));

        assert_eq!(
            action,
            BrowseAction::Create("Journal/Daily/2026-04-05.md".to_string())
        );
        assert!(preview_text(&state.preview_lines()).contains("Note: missing"));
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
    fn ctrl_v_toggles_dataview_preview_for_selected_note() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        write_note(
            temp_dir.path(),
            "Dashboard.md",
            "---\nstatus: draft\n---\n\n`= this.status`\n\n```dataview\nLIST FROM #project\n```\n",
        );
        write_note(
            temp_dir.path(),
            "Projects/Alpha.md",
            "---\nreviewed: true\n---\n\n# Alpha\n#project\n",
        );
        scan_fixture(&paths);
        let mut state = BrowseState::new(
            paths,
            vec![note("Dashboard.md", &[]), note("Projects/Alpha.md", &[])],
        )
        .expect("state should build");
        state.picker.select_path("Dashboard.md");

        assert!(state.preview_title().contains("Dashboard.md"));

        let action = state.handle_key(ctrl('v'));

        assert_eq!(action, BrowseAction::Continue);
        assert_eq!(state.preview_title(), "Dataview: Dashboard.md");
        assert_eq!(state.status_line(), "Preview mode: dataview.");
        assert!(state.status_bar_line().contains("Preview: dataview"));

        let preview = preview_text(&state.preview_lines());
        assert!(preview.contains("Inline expressions (1)"));
        assert!(preview.contains("this.status => draft"));
        assert!(preview.contains("Dataview blocks (1)"));
        assert!(preview.contains("Block 0 (dataview"));

        state.handle_key(ctrl('v'));
        assert_eq!(state.status_line(), "Preview mode: file.");
        assert_eq!(state.preview_title(), "Preview: Dashboard.md");
    }

    #[test]
    fn dataview_preview_refreshes_when_selected_note_changes() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        write_note(
            temp_dir.path(),
            "Alpha.md",
            "---\nstatus: draft\n---\n\n`= this.status`\n",
        );
        write_note(temp_dir.path(), "Beta.md", "# Beta\n");
        scan_fixture(&paths);
        let mut state = BrowseState::new(paths, vec![note("Alpha.md", &[]), note("Beta.md", &[])])
            .expect("state should build");
        state.picker.select_path("Alpha.md");
        state.handle_key(ctrl('v'));

        assert!(preview_text(&state.preview_lines()).contains("this.status => draft"));

        state.picker.select_path("Beta.md");
        state.refresh_dataview_preview_if_needed();

        assert_eq!(state.preview_title(), "Dataview: Beta.md");
        assert_eq!(
            preview_text(&state.preview_lines()),
            "No Dataview inline expressions or blocks."
        );
    }

    #[test]
    fn ctrl_v_switches_full_text_preview_from_snippet_to_dataview_details() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        write_note(
            temp_dir.path(),
            "Dashboard.md",
            "---\nstatus: draft\n---\n\nrelease dashboard\n\n`= this.status`\n",
        );
        scan_fixture(&paths);
        let mut state =
            BrowseState::new(paths, vec![note("Dashboard.md", &[])]).expect("state should build");

        state.handle_key(ctrl('f'));
        for character in "dashboard".chars() {
            state.handle_key(key(KeyCode::Char(character)));
        }

        assert!(state.preview_title().contains("Snippet:"));
        assert!(preview_text(&state.preview_lines()).contains("dashboard"));

        state.handle_key(ctrl('v'));

        assert_eq!(state.preview_title(), "Dataview: Dashboard.md");
        assert!(preview_text(&state.preview_lines()).contains("this.status => draft"));
    }

    #[test]
    fn highlighted_snippet_parser_marks_bracketed_terms() {
        let line = highlighted_snippet_line("alpha [dashboard] omega");

        assert_eq!(line.spans.len(), 3);
        assert_eq!(line.spans[1].content, "dashboard");
    }
}
