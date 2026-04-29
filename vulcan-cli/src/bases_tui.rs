use crate::create_note_from_bases_view;
use crate::editor::{open_in_editor, with_terminal_suspended};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Cell, Clear, Paragraph, Row, Table, TableState, Tabs, Wrap,
};
use ratatui::{Frame, Terminal};
use serde_json::Value;
use std::fs;
use std::io;
use std::time::Duration;
use vulcan_core::{
    evaluate_base_file, scan_vault, set_note_property, BasesEvalReport, BasesEvaluatedView,
    BasesRow, ScanMode, VaultPaths,
};

const MAX_TABLE_COLUMNS: usize = 5;
const DETAIL_PREVIEW_LINES: usize = 12;
const PREVIEW_SCROLL_STEP: u16 = 8;

pub fn run_bases_tui(
    paths: &VaultPaths,
    base_file: &str,
    report: &BasesEvalReport,
) -> Result<(), io::Error> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.hide_cursor()?;
    let mut state = BasesTuiState::new(paths.clone(), base_file.to_string(), report.clone());

    let result = run_event_loop(&mut terminal, &mut state);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

fn run_event_loop(
    terminal: &mut Terminal<ratatui::backend::CrosstermBackend<io::Stdout>>,
    state: &mut BasesTuiState,
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
                TuiAction::Continue => {}
                TuiAction::Quit => break,
                TuiAction::CreateNote => {
                    let paths = state.paths.clone();
                    let base_file = state.base_file.clone();
                    let view_index = state.active_view;
                    let mut created_path = None;
                    let create_result = with_terminal_suspended(terminal, || {
                        let report = create_note_from_bases_view(
                            &paths, &base_file, view_index, None, false,
                        )
                        .map_err(|error| error.to_string())?;
                        created_path = Some(report.path.clone());
                        let absolute = paths.vault_root().join(&report.path);
                        open_in_editor(&absolute)?;
                        scan_vault(&paths, ScanMode::Incremental)
                            .map_err(|error| error.to_string())?;
                        Ok(())
                    });
                    match create_result {
                        Ok(()) => {
                            if let Err(error) = state.reload_report() {
                                state.set_status(error);
                            } else {
                                let created_path =
                                    created_path.unwrap_or_else(|| "new note".to_string());
                                state.set_status(format!("Created {created_path}."));
                            }
                        }
                        Err(error) => state.set_status(error.to_string()),
                    }
                }
                TuiAction::OpenBaseEditor => {
                    let path = state.paths.vault_root().join(&state.base_file);
                    let edit_result = with_terminal_suspended(terminal, || open_in_editor(&path));
                    match edit_result {
                        Ok(()) => {
                            if let Err(error) = state.reload_report() {
                                state.set_status(error);
                            } else {
                                state.set_status(format!("Reloaded {}.", state.base_file));
                            }
                        }
                        Err(error) => state.set_status(error.to_string()),
                    }
                }
                TuiAction::OpenSelectedNoteEditor(path) => {
                    let absolute = state.paths.vault_root().join(&path);
                    let edit_result =
                        with_terminal_suspended(terminal, || open_in_editor(&absolute));
                    match edit_result {
                        Ok(()) => {
                            if let Err(error) = state.refresh_after_note_edit() {
                                state.set_status(error);
                            } else {
                                state.set_status(format!("Updated {path}."));
                            }
                        }
                        Err(error) => state.set_status(error.to_string()),
                    }
                }
            }
        }
    }

    Ok(())
}

fn draw(frame: &mut Frame<'_>, state: &BasesTuiState) {
    if state.preview_expanded {
        draw_preview_screen(frame, state);
    } else {
        draw_standard_screen(frame, state);
    }

    match state.input_mode {
        InputMode::Normal => {}
        InputMode::Search => draw_search_overlay(frame, state),
        InputMode::EditProperty => draw_property_overlay(frame, state),
    }
}

fn draw_standard_screen(frame: &mut Frame<'_>, state: &BasesTuiState) {
    let footer_height = if state.show_diagnostics { 7 } else { 6 };
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(12),
            Constraint::Length(footer_height),
        ])
        .split(frame.area());

    draw_tabs(frame, state, layout[0]);
    draw_body(frame, state, layout[1]);
    draw_footer(frame, state, layout[2]);
}

fn draw_preview_screen(frame: &mut Frame<'_>, state: &BasesTuiState) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(12),
            Constraint::Length(5),
        ])
        .split(frame.area());

    draw_tabs(frame, state, layout[0]);
    draw_full_preview(frame, state, layout[1]);
    draw_preview_status(frame, state, layout[2]);
}

fn draw_tabs(frame: &mut Frame<'_>, state: &BasesTuiState, area: Rect) {
    let titles = if state.report.views.is_empty() {
        vec![Line::from("No Views")]
    } else {
        state
            .report
            .views
            .iter()
            .map(|view| Line::from(view.name.clone().unwrap_or_else(|| view.view_type.clone())))
            .collect::<Vec<_>>()
    };
    let tabs = Tabs::new(titles)
        .block(
            Block::default()
                .title(format!("Bases TUI: {}", state.base_file))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .highlight_style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
        .select(
            state
                .active_view
                .min(state.report.views.len().saturating_sub(1)),
        );
    frame.render_widget(tabs, area);
}

fn draw_body(frame: &mut Frame<'_>, state: &BasesTuiState, area: Rect) {
    let layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(54), Constraint::Percentage(46)])
        .split(area);

    draw_table(frame, state, layout[0]);
    draw_detail(frame, state, layout[1]);
}

fn draw_table(frame: &mut Frame<'_>, state: &BasesTuiState, area: Rect) {
    let (headers, rows, selected_index) = state.table_rows();
    let widths = vec![Constraint::Length(18); headers.len()];
    let header = Row::new(headers.into_iter().map(Cell::from)).style(
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    );
    let rendered_rows = rows.into_iter().map(Row::new).collect::<Vec<_>>();
    let table = Table::new(rendered_rows, widths)
        .header(header)
        .row_highlight_style(Style::default().bg(Color::DarkGray))
        .block(
            Block::default()
                .title("Rows")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        );
    let mut table_state = TableState::default();
    table_state.select(selected_index);
    frame.render_stateful_widget(table, area, &mut table_state);
}

fn draw_detail(frame: &mut Frame<'_>, state: &BasesTuiState, area: Rect) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
        .split(area);

    let detail = Paragraph::new(state.selected_row_lines())
        .block(
            Block::default()
                .title("Detail")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(detail, layout[0]);

    let preview_title = state
        .preview
        .path
        .as_deref()
        .map_or_else(|| "Preview".to_string(), |path| format!("Preview: {path}"));
    let preview = Paragraph::new(state.preview_excerpt_lines(DETAIL_PREVIEW_LINES))
        .block(
            Block::default()
                .title(preview_title)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(preview, layout[1]);
}

fn draw_full_preview(frame: &mut Frame<'_>, state: &BasesTuiState, area: Rect) {
    let preview_title = state
        .preview
        .path
        .as_deref()
        .map_or_else(|| "Preview".to_string(), |path| format!("Preview: {path}"));
    let preview = Paragraph::new(state.preview_full_lines())
        .block(
            Block::default()
                .title(preview_title)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow)),
        )
        .wrap(Wrap { trim: false })
        .scroll((state.preview_scroll, 0));
    frame.render_widget(preview, area);
}

fn draw_footer(frame: &mut Frame<'_>, state: &BasesTuiState, area: Rect) {
    if state.show_diagnostics {
        let layout = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
            .split(area);
        draw_diagnostics(frame, state, layout[0]);
        draw_status(frame, state, layout[1]);
    } else {
        draw_status(frame, state, area);
    }
}

fn draw_diagnostics(frame: &mut Frame<'_>, state: &BasesTuiState, area: Rect) {
    let diagnostics = if state.report.diagnostics.is_empty() {
        vec![Line::from("No diagnostics.")]
    } else {
        state
            .report
            .diagnostics
            .iter()
            .take(5)
            .map(|diagnostic| {
                Line::from(format!(
                    "{}{}",
                    diagnostic
                        .path
                        .as_deref()
                        .map(|path| format!("{path}: "))
                        .unwrap_or_default(),
                    diagnostic.message
                ))
            })
            .collect::<Vec<_>>()
    };
    let diagnostics = Paragraph::new(diagnostics)
        .block(
            Block::default()
                .title("Diagnostics")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(diagnostics, area);
}

fn draw_status(frame: &mut Frame<'_>, state: &BasesTuiState, area: Rect) {
    let mut lines = vec![
        Line::from(format!(
            "Rows: {}  Selected: {}  Diagnostics: {}",
            state.filtered_rows().len(),
            state.selected_row.map_or(0, |index| index + 1),
            if state.show_diagnostics {
                "shown"
            } else {
                "hidden"
            }
        )),
        Line::from(format!(
            "Group mode: {}  Filter: {}",
            if state.group_mode { "on" } else { "off" },
            if state.search.is_empty() {
                "<none>"
            } else {
                state.search.as_str()
            }
        )),
        Line::from("Keys: q quit, / filter, g group, d diagnostics, Enter preview"),
        Line::from("      e edit property, n create note, o edit note, b edit .base"),
        Line::from("      tab next view"),
    ];
    if let Some(message) = state.status_message.as_deref() {
        lines.push(Line::from(message.to_string()));
    }

    let status = Paragraph::new(lines)
        .block(
            Block::default()
                .title("Status")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(status, area);
}

fn draw_preview_status(frame: &mut Frame<'_>, state: &BasesTuiState, area: Rect) {
    let mut lines = vec![
        Line::from("Preview keys: Esc close, j/k scroll, PgUp/PgDn page"),
        Line::from("              e edit property, n create note, o edit note, b edit .base"),
    ];
    if let Some(message) = state.status_message.as_deref() {
        lines.push(Line::from(message.to_string()));
    }

    let status = Paragraph::new(lines)
        .block(
            Block::default()
                .title("Preview")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(status, area);
}

fn draw_search_overlay(frame: &mut Frame<'_>, state: &BasesTuiState) {
    let area = centered_rect(frame.area(), 60, 3);
    frame.render_widget(Clear, area);
    let input = Paragraph::new(state.search.clone()).block(
        Block::default()
            .title("Filter (/ to edit, Enter to apply, Esc to cancel)")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Yellow)),
    );
    frame.render_widget(input, area);
}

fn draw_property_overlay(frame: &mut Frame<'_>, state: &BasesTuiState) {
    let area = centered_rect(frame.area(), 70, 4);
    frame.render_widget(Clear, area);
    let input = Paragraph::new(state.property_input.clone()).block(
        Block::default()
            .title("Edit property (key=value, empty value removes)")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Yellow)),
    );
    frame.render_widget(input, area);
}

fn centered_rect(area: Rect, width_percent: u16, height: u16) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(50),
            Constraint::Length(height),
            Constraint::Percentage(50),
        ])
        .split(area);
    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - width_percent) / 2),
            Constraint::Percentage(width_percent),
            Constraint::Percentage((100 - width_percent) / 2),
        ])
        .split(vertical[1]);
    horizontal[1]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InputMode {
    Normal,
    Search,
    EditProperty,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TuiAction {
    Continue,
    Quit,
    CreateNote,
    OpenSelectedNoteEditor(String),
    OpenBaseEditor,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PreviewContent {
    path: Option<String>,
    lines: Vec<String>,
}

#[derive(Debug, Clone)]
struct BasesTuiState {
    paths: VaultPaths,
    base_file: String,
    report: BasesEvalReport,
    active_view: usize,
    selected_row: Option<usize>,
    search: String,
    input_mode: InputMode,
    property_input: String,
    group_mode: bool,
    show_diagnostics: bool,
    preview_expanded: bool,
    preview_scroll: u16,
    preview: PreviewContent,
    status_message: Option<String>,
}

impl BasesTuiState {
    fn new(paths: VaultPaths, base_file: String, report: BasesEvalReport) -> Self {
        let group_mode = report
            .views
            .first()
            .and_then(|view| view.group_by.as_ref())
            .is_some();
        let selected_row = report
            .views
            .first()
            .filter(|view| !view.rows.is_empty())
            .map(|_| 0);
        let mut state = Self {
            paths,
            base_file,
            report,
            active_view: 0,
            selected_row,
            search: String::new(),
            input_mode: InputMode::Normal,
            property_input: String::new(),
            group_mode,
            show_diagnostics: false,
            preview_expanded: false,
            preview_scroll: 0,
            preview: PreviewContent {
                path: None,
                lines: vec!["No preview available.".to_string()],
            },
            status_message: None,
        };
        state.refresh_preview();
        state
    }

    fn active_view(&self) -> Option<&BasesEvaluatedView> {
        self.report.views.get(self.active_view)
    }

    fn filtered_rows(&self) -> Vec<usize> {
        let query = self.search.trim().to_lowercase();
        self.active_view()
            .into_iter()
            .flat_map(|view| view.rows.iter().enumerate())
            .filter(|(_, row)| {
                query.is_empty() || row_search_text(row).to_lowercase().contains(&query)
            })
            .map(|(index, _)| index)
            .collect()
    }

    fn selected_row(&self) -> Option<&BasesRow> {
        let rows = self.filtered_rows();
        self.selected_row
            .and_then(|index| rows.get(index).copied())
            .and_then(|index| self.active_view().and_then(|view| view.rows.get(index)))
    }

    fn selected_row_path(&self) -> Option<String> {
        self.selected_row().map(|row| row.document_path.clone())
    }

    fn set_status(&mut self, message: impl Into<String>) {
        self.status_message = Some(message.into());
    }

    fn handle_key(&mut self, key: KeyEvent) -> TuiAction {
        match self.input_mode {
            InputMode::Search => {
                self.handle_search_key(key);
                return TuiAction::Continue;
            }
            InputMode::EditProperty => {
                self.handle_property_key(key);
                return TuiAction::Continue;
            }
            InputMode::Normal => {}
        }

        if self.preview_expanded {
            return self.handle_preview_key(key);
        }

        match key.code {
            KeyCode::Char('q') => TuiAction::Quit,
            KeyCode::Tab => {
                self.next_view();
                TuiAction::Continue
            }
            KeyCode::BackTab => {
                self.previous_view();
                TuiAction::Continue
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_selection(1);
                TuiAction::Continue
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_selection(-1);
                TuiAction::Continue
            }
            KeyCode::Char('/') => {
                self.input_mode = InputMode::Search;
                TuiAction::Continue
            }
            KeyCode::Char('g') => {
                if self
                    .active_view()
                    .and_then(|view| view.group_by.as_ref())
                    .is_some()
                {
                    self.group_mode = !self.group_mode;
                }
                TuiAction::Continue
            }
            KeyCode::Char('d') => {
                self.show_diagnostics = !self.show_diagnostics;
                TuiAction::Continue
            }
            KeyCode::Enter => {
                self.preview_expanded = self.selected_row().is_some();
                TuiAction::Continue
            }
            KeyCode::Char('e') => {
                if self.selected_row().is_some() {
                    self.property_input.clear();
                    self.input_mode = InputMode::EditProperty;
                } else {
                    self.set_status("No selected row to edit.");
                }
                TuiAction::Continue
            }
            KeyCode::Char('n') => {
                if self.active_view().is_some() {
                    TuiAction::CreateNote
                } else {
                    self.set_status("No active view to create from.");
                    TuiAction::Continue
                }
            }
            KeyCode::Char('o') => self.selected_row_path().map_or_else(
                || {
                    self.set_status("No selected note to edit.");
                    TuiAction::Continue
                },
                TuiAction::OpenSelectedNoteEditor,
            ),
            KeyCode::Char('b') => TuiAction::OpenBaseEditor,
            _ => TuiAction::Continue,
        }
    }

    fn handle_preview_key(&mut self, key: KeyEvent) -> TuiAction {
        match key.code {
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => {
                self.preview_expanded = false;
            }
            KeyCode::Down | KeyCode::Char('j') => self.scroll_preview(1),
            KeyCode::Up | KeyCode::Char('k') => self.scroll_preview(-1),
            KeyCode::PageDown => self.scroll_preview(i32::from(PREVIEW_SCROLL_STEP)),
            KeyCode::PageUp => self.scroll_preview(-i32::from(PREVIEW_SCROLL_STEP)),
            KeyCode::Char('e') => {
                if self.selected_row().is_some() {
                    self.property_input.clear();
                    self.input_mode = InputMode::EditProperty;
                } else {
                    self.set_status("No selected row to edit.");
                }
            }
            KeyCode::Char('n') => return TuiAction::CreateNote,
            KeyCode::Char('o') => {
                return self.selected_row_path().map_or_else(
                    || {
                        self.set_status("No selected note to edit.");
                        TuiAction::Continue
                    },
                    TuiAction::OpenSelectedNoteEditor,
                );
            }
            KeyCode::Char('b') => return TuiAction::OpenBaseEditor,
            _ => {}
        }
        TuiAction::Continue
    }

    fn handle_search_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.input_mode = InputMode::Normal;
            }
            KeyCode::Enter => {
                self.input_mode = InputMode::Normal;
                self.clamp_selection();
            }
            KeyCode::Backspace => {
                self.search.pop();
                self.clamp_selection();
            }
            KeyCode::Char(character) => {
                self.search.push(character);
                self.clamp_selection();
            }
            _ => {}
        }
    }

    fn handle_property_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.input_mode = InputMode::Normal;
            }
            KeyCode::Enter => {
                if let Err(error) = self.apply_property_edit() {
                    self.set_status(error);
                }
                self.input_mode = InputMode::Normal;
            }
            KeyCode::Backspace => {
                self.property_input.pop();
            }
            KeyCode::Char(character) => {
                self.property_input.push(character);
            }
            _ => {}
        }
    }

    fn next_view(&mut self) {
        if self.report.views.is_empty() {
            return;
        }
        self.active_view = (self.active_view + 1) % self.report.views.len();
        self.group_mode = self
            .active_view()
            .and_then(|view| view.group_by.as_ref())
            .is_some();
        self.clamp_selection();
    }

    fn previous_view(&mut self) {
        if self.report.views.is_empty() {
            return;
        }
        self.active_view = if self.active_view == 0 {
            self.report.views.len() - 1
        } else {
            self.active_view - 1
        };
        self.group_mode = self
            .active_view()
            .and_then(|view| view.group_by.as_ref())
            .is_some();
        self.clamp_selection();
    }

    fn move_selection(&mut self, delta: isize) {
        let row_count = self.filtered_rows().len();
        if row_count == 0 {
            self.selected_row = None;
            self.refresh_preview();
            return;
        }

        let current = self.selected_row.unwrap_or(0);
        let step = delta.unsigned_abs();
        let next = if delta.is_negative() {
            current.saturating_sub(step)
        } else {
            current.saturating_add(step)
        }
        .min(row_count - 1);
        self.selected_row = Some(next);
        self.refresh_preview();
    }

    fn clamp_selection(&mut self) {
        let row_count = self.filtered_rows().len();
        self.selected_row = if row_count == 0 {
            None
        } else {
            Some(self.selected_row.unwrap_or(0).min(row_count - 1))
        };
        self.refresh_preview();
    }

    fn table_rows(&self) -> (Vec<String>, Vec<Vec<String>>, Option<usize>) {
        let Some(view) = self.active_view() else {
            return (vec!["Path".to_string()], Vec::new(), None);
        };
        let filtered = self.filtered_rows();
        let mut headers = Vec::new();
        let mut keys = Vec::new();
        if self.group_mode {
            if let Some(group_by) = view.group_by.as_ref() {
                headers.push(group_by.display_name.clone());
                keys.push("__group__".to_string());
            }
        }
        headers.push("Path".to_string());
        keys.push("file.path".to_string());
        for column in view
            .columns
            .iter()
            .take(MAX_TABLE_COLUMNS.saturating_sub(headers.len()))
        {
            headers.push(column.display_name.clone());
            keys.push(column.key.clone());
        }

        let rows = filtered
            .iter()
            .map(|row_index| {
                let row = &view.rows[*row_index];
                keys.iter()
                    .map(|key| {
                        if key == "__group__" {
                            row.group_value
                                .as_ref()
                                .map(render_value)
                                .filter(|value| !value.is_empty() && value != "null")
                                .unwrap_or_else(|| "-".to_string())
                        } else {
                            table_cell_value(row, key)
                        }
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();

        (headers, rows, self.selected_row)
    }

    fn selected_row_lines(&self) -> Vec<Line<'static>> {
        let Some(row) = self.selected_row() else {
            return vec![Line::from("No rows.")];
        };
        let Some(view) = self.active_view() else {
            return vec![Line::from("No active view.")];
        };

        let mut lines = vec![
            Line::from(vec![
                Span::styled("Path: ", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(row.document_path.clone()),
            ]),
            Line::from(vec![
                Span::styled("File: ", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(format!("{}.{}", row.file_name, row.file_ext)),
            ]),
        ];

        if let Some(group_value) = row.group_value.as_ref() {
            lines.push(Line::from(vec![
                Span::styled("Group: ", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(render_value(group_value)),
            ]));
        }
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Cells",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )));
        for column in &view.columns {
            lines.push(Line::from(format!(
                "{}: {}",
                column.display_name,
                table_cell_value(row, &column.key)
            )));
        }

        if !row.formulas.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "Formulas",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )));
            for (key, value) in &row.formulas {
                lines.push(Line::from(format!("{key}: {}", render_value(value))));
            }
        }

        lines
    }

    fn preview_excerpt_lines(&self, limit: usize) -> Vec<Line<'static>> {
        self.preview
            .lines
            .iter()
            .take(limit)
            .map(|line| Line::from(line.clone()))
            .collect()
    }

    fn preview_full_lines(&self) -> Vec<Line<'static>> {
        self.preview
            .lines
            .iter()
            .map(|line| Line::from(line.clone()))
            .collect()
    }

    fn refresh_preview(&mut self) {
        self.preview_scroll = 0;
        self.preview = self.selected_row_path().map_or(
            PreviewContent {
                path: None,
                lines: vec!["No preview available.".to_string()],
            },
            |path| PreviewContent {
                lines: load_preview_lines(&self.paths, &path),
                path: Some(path),
            },
        );
    }

    fn scroll_preview(&mut self, delta: i32) {
        if delta.is_negative() {
            self.preview_scroll = self
                .preview_scroll
                .saturating_sub(u16::try_from(delta.unsigned_abs()).unwrap_or(u16::MAX));
        } else {
            self.preview_scroll = self
                .preview_scroll
                .saturating_add(u16::try_from(delta).unwrap_or(u16::MAX));
        }
    }

    fn refresh_after_note_edit(&mut self) -> Result<(), String> {
        scan_vault(&self.paths, ScanMode::Incremental).map_err(|error| error.to_string())?;
        self.reload_report()
    }

    fn reload_report(&mut self) -> Result<(), String> {
        let current_view = self.active_view().map(|view| {
            (
                view.name.clone(),
                view.view_type.clone(),
                view.group_by.is_some(),
            )
        });
        let selected_path = self.selected_row_path();
        let report =
            evaluate_base_file(&self.paths, &self.base_file).map_err(|error| error.to_string())?;
        self.report = report;
        self.active_view = current_view
            .and_then(|(name, view_type, _)| {
                self.report
                    .views
                    .iter()
                    .position(|view| view.name == name && view.view_type == view_type)
            })
            .unwrap_or(0);
        self.group_mode = self
            .active_view()
            .and_then(|view| view.group_by.as_ref())
            .is_some();
        self.selected_row = selected_path
            .as_deref()
            .and_then(|path| {
                self.active_view().and_then(|view| {
                    let filtered = self.filtered_rows();
                    filtered
                        .iter()
                        .position(|index| view.rows[*index].document_path == path)
                })
            })
            .or_else(|| {
                self.active_view()
                    .filter(|view| !view.rows.is_empty())
                    .map(|_| 0)
            });
        self.refresh_preview();
        Ok(())
    }

    fn apply_property_edit(&mut self) -> Result<(), String> {
        let Some(note_path) = self.selected_row_path() else {
            return Err("No selected note to edit.".to_string());
        };
        let (key, value) = parse_property_edit_input(&self.property_input)?;
        let report = set_note_property(&self.paths, &note_path, &key, value.as_deref(), false)
            .map_err(|error| error.to_string())?;
        self.reload_report()?;
        if value.is_some() {
            if report.files.is_empty() {
                self.set_status(format!("No changes for `{key}` in {note_path}."));
            } else {
                self.set_status(format!("Updated `{key}` in {note_path}."));
            }
        } else if report.files.is_empty() {
            self.set_status(format!("`{key}` was already absent in {note_path}."));
        } else {
            self.set_status(format!("Removed `{key}` from {note_path}."));
        }
        self.property_input.clear();
        Ok(())
    }
}

fn parse_property_edit_input(input: &str) -> Result<(String, Option<String>), String> {
    let Some((key, raw_value)) = input.split_once('=') else {
        return Err("expected property edit in the form `key=value`".to_string());
    };
    let key = key.trim();
    if key.is_empty() {
        return Err("property key must not be empty".to_string());
    }
    if key.chars().any(char::is_control) {
        return Err("property key must not contain control characters".to_string());
    }
    let value = raw_value.trim();
    Ok((
        key.to_string(),
        (!value.is_empty()).then(|| value.to_string()),
    ))
}

fn load_preview_lines(paths: &VaultPaths, relative_path: &str) -> Vec<String> {
    match fs::read_to_string(paths.vault_root().join(relative_path)) {
        Ok(contents) => {
            let lines = contents.lines().map(str::to_string).collect::<Vec<_>>();
            if lines.is_empty() {
                vec!["<empty file>".to_string()]
            } else {
                lines
            }
        }
        Err(error) => vec![format!("Failed to load preview: {error}")],
    }
}

fn table_cell_value(row: &BasesRow, key: &str) -> String {
    match key {
        "file.path" => row.document_path.clone(),
        "file.name" => row.file_name.clone(),
        "file.ext" => row.file_ext.clone(),
        "file.mtime" => row.file_mtime.to_string(),
        _ => row
            .cells
            .get(key)
            .or_else(|| row.formulas.get(key))
            .or_else(|| row.properties.get(key))
            .map(render_value)
            .filter(|value| !value.is_empty() && value != "null")
            .unwrap_or_else(|| "-".to_string()),
    }
}

fn row_search_text(row: &BasesRow) -> String {
    let mut parts = vec![
        row.document_path.clone(),
        row.file_name.clone(),
        row.file_ext.clone(),
    ];
    parts.extend(
        row.cells
            .values()
            .chain(row.formulas.values())
            .chain(
                row.properties
                    .as_object()
                    .into_iter()
                    .flat_map(|object| object.values()),
            )
            .map(render_value),
    );
    parts.join(" ")
}

fn render_value(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => value.clone(),
        other => serde_json::to_string(other).unwrap_or_else(|_| other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeMap;
    use tempfile::TempDir;
    use vulcan_core::{BasesColumn, BasesDiagnostic, BasesGroupBy};

    fn sample_state() -> BasesTuiState {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        std::fs::create_dir_all(vault_root.join("Gear")).expect("gear dir should exist");
        std::fs::write(vault_root.join("Gear/A.md"), "# A\n\nArmor note.\n")
            .expect("a note should write");
        std::fs::write(vault_root.join("Gear/B.md"), "# B\n\nWeapons note.\n")
            .expect("b note should write");
        std::fs::write(vault_root.join("Gear/C.md"), "# C\n\nRating note.\n")
            .expect("c note should write");
        let report = BasesEvalReport {
            file: "Indexes/Gear.base".to_string(),
            views: vec![
                BasesEvaluatedView {
                    name: Some("Gear".to_string()),
                    view_type: "table".to_string(),
                    filters: vec![],
                    sort_by: None,
                    sort_descending: false,
                    columns: vec![
                        BasesColumn {
                            key: "category".to_string(),
                            display_name: "Category".to_string(),
                        },
                        BasesColumn {
                            key: "rating".to_string(),
                            display_name: "Rating".to_string(),
                        },
                    ],
                    group_by: Some(BasesGroupBy {
                        property: "category".to_string(),
                        display_name: "Category".to_string(),
                        descending: false,
                    }),
                    rows: vec![
                        BasesRow {
                            document_path: "Gear/A.md".to_string(),
                            file_name: "A".to_string(),
                            file_ext: "md".to_string(),
                            file_mtime: 1,
                            properties: json!({"category": "Armor", "rating": 2}),
                            formulas: BTreeMap::new(),
                            cells: BTreeMap::from([
                                ("category".to_string(), json!("Armor")),
                                ("rating".to_string(), json!(2)),
                            ]),
                            group_value: Some(json!("Armor")),
                        },
                        BasesRow {
                            document_path: "Gear/B.md".to_string(),
                            file_name: "B".to_string(),
                            file_ext: "md".to_string(),
                            file_mtime: 2,
                            properties: json!({"category": "Weapons", "rating": 5}),
                            formulas: BTreeMap::new(),
                            cells: BTreeMap::from([
                                ("category".to_string(), json!("Weapons")),
                                ("rating".to_string(), json!(5)),
                            ]),
                            group_value: Some(json!("Weapons")),
                        },
                    ],
                },
                BasesEvaluatedView {
                    name: Some("Flat".to_string()),
                    view_type: "table".to_string(),
                    filters: vec![],
                    sort_by: None,
                    sort_descending: false,
                    columns: vec![BasesColumn {
                        key: "rating".to_string(),
                        display_name: "Rating".to_string(),
                    }],
                    group_by: None,
                    rows: vec![BasesRow {
                        document_path: "Gear/C.md".to_string(),
                        file_name: "C".to_string(),
                        file_ext: "md".to_string(),
                        file_mtime: 3,
                        properties: json!({"rating": 3}),
                        formulas: BTreeMap::new(),
                        cells: BTreeMap::from([("rating".to_string(), json!(3))]),
                        group_value: None,
                    }],
                },
            ],
            diagnostics: vec![BasesDiagnostic {
                path: Some("views.Flat.filters".to_string()),
                message: "unsupported filter".to_string(),
            }],
        };

        let paths = VaultPaths::new(&vault_root);
        let state = BasesTuiState::new(paths, "Indexes/Gear.base".to_string(), report);
        std::mem::forget(temp_dir);
        state
    }

    #[test]
    fn state_filters_rows_by_query() {
        let mut state = sample_state();
        state.search = "weapon".to_string();

        assert_eq!(state.filtered_rows(), vec![1]);
        assert_eq!(
            state
                .selected_row()
                .expect("row should exist")
                .document_path,
            "Gear/B.md"
        );
    }

    #[test]
    fn state_toggles_group_mode_only_for_grouped_views() {
        let mut state = sample_state();
        assert!(state.group_mode);

        state.next_view();
        assert!(!state.group_mode);
    }

    #[test]
    fn table_rows_include_group_column_when_enabled() {
        let state = sample_state();
        let (headers, rows, selected) = state.table_rows();

        assert_eq!(headers[0], "Category");
        assert_eq!(rows[0][0], "Armor");
        assert_eq!(selected, Some(0));
    }

    #[test]
    fn diagnostics_are_hidden_by_default_and_can_be_toggled() {
        let mut state = sample_state();
        assert!(!state.show_diagnostics);

        state.handle_key(KeyEvent::from(KeyCode::Char('d')));
        assert!(state.show_diagnostics);
    }

    #[test]
    fn preview_loads_selected_note_contents() {
        let state = sample_state();
        assert_eq!(state.preview.path.as_deref(), Some("Gear/A.md"));
        assert!(state.preview.lines.iter().any(|line| line == "# A"));
    }

    #[test]
    fn parse_property_edit_input_supports_updates_and_removals() {
        assert_eq!(
            parse_property_edit_input("status=done").expect("edit input should parse"),
            ("status".to_string(), Some("done".to_string()))
        );
        assert_eq!(
            parse_property_edit_input("status=").expect("edit input should parse"),
            ("status".to_string(), None)
        );
        assert!(parse_property_edit_input("=").is_err());
    }

    #[test]
    fn n_hotkey_creates_note_from_active_view() {
        let mut state = sample_state();

        let action = state.handle_key(KeyEvent::from(KeyCode::Char('n')));

        assert_eq!(action, TuiAction::CreateNote);
    }
}
