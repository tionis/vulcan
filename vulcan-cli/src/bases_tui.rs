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
use std::io;
use std::time::Duration;
use vulcan_core::{BasesEvalReport, BasesEvaluatedView, BasesRow};

const MAX_TABLE_COLUMNS: usize = 5;

pub fn run_bases_tui(report: &BasesEvalReport) -> Result<(), io::Error> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let mut state = BasesTuiState::new(report.clone());

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
            if !state.handle_key(key) {
                break;
            }
        }
    }

    Ok(())
}

fn draw(frame: &mut Frame<'_>, state: &BasesTuiState) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(12),
            Constraint::Length(7),
        ])
        .split(frame.area());

    draw_tabs(frame, state, layout[0]);
    draw_body(frame, state, layout[1]);
    draw_footer(frame, state, layout[2]);

    if state.search_mode {
        draw_search_overlay(frame, state);
    }
}

fn draw_tabs(frame: &mut Frame<'_>, state: &BasesTuiState, area: Rect) {
    let titles = state
        .report
        .views
        .iter()
        .map(|view| Line::from(view.name.clone().unwrap_or_else(|| view.view_type.clone())))
        .collect::<Vec<_>>();
    let tabs = Tabs::new(titles)
        .block(
            Block::default()
                .title("Bases TUI")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .highlight_style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
        .select(state.active_view);
    frame.render_widget(tabs, area);
}

fn draw_body(frame: &mut Frame<'_>, state: &BasesTuiState, area: Rect) {
    let layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
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
        .highlight_style(Style::default().bg(Color::DarkGray))
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
    let content = if let Some(row) = state.selected_row() {
        selected_row_lines(row, state.active_view())
    } else {
        vec![Line::from("No rows.")]
    };
    let detail = Paragraph::new(content)
        .block(
            Block::default()
                .title("Detail")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(detail, area);
}

fn draw_footer(frame: &mut Frame<'_>, state: &BasesTuiState, area: Rect) {
    let layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(65), Constraint::Percentage(35)])
        .split(area);

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
    frame.render_widget(diagnostics, layout[0]);

    let status = Paragraph::new(vec![
        Line::from(format!(
            "Rows: {}  Selected: {}",
            state.filtered_rows().len(),
            state.selected_row.map_or(0, |index| index + 1)
        )),
        Line::from(format!(
            "Group mode: {}",
            if state.group_mode { "on" } else { "off" }
        )),
        Line::from(format!(
            "Filter: {}",
            if state.search.is_empty() {
                "<none>"
            } else {
                state.search.as_str()
            }
        )),
        Line::from("Keys: q quit, / filter, g group, tab next view"),
        Line::from("      arrows/jk move, backtab prev view"),
    ])
    .block(
        Block::default()
            .title("Status")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan)),
    )
    .wrap(Wrap { trim: false });
    frame.render_widget(status, layout[1]);
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

#[derive(Debug, Clone)]
struct BasesTuiState {
    report: BasesEvalReport,
    active_view: usize,
    selected_row: Option<usize>,
    search: String,
    search_mode: bool,
    group_mode: bool,
}

impl BasesTuiState {
    fn new(report: BasesEvalReport) -> Self {
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
        Self {
            report,
            active_view: 0,
            selected_row,
            search: String::new(),
            search_mode: false,
            group_mode,
        }
    }

    fn active_view(&self) -> &BasesEvaluatedView {
        &self.report.views[self.active_view]
    }

    fn filtered_rows(&self) -> Vec<usize> {
        let query = self.search.trim().to_lowercase();
        self.active_view()
            .rows
            .iter()
            .enumerate()
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
            .and_then(|index| self.active_view().rows.get(index))
    }

    fn handle_key(&mut self, key: KeyEvent) -> bool {
        if self.search_mode {
            return self.handle_search_key(key);
        }

        match key.code {
            KeyCode::Char('q') => false,
            KeyCode::Tab => {
                self.next_view();
                true
            }
            KeyCode::BackTab => {
                self.previous_view();
                true
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_selection(1);
                true
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_selection(-1);
                true
            }
            KeyCode::Char('/') => {
                self.search_mode = true;
                true
            }
            KeyCode::Char('g') => {
                if self.active_view().group_by.is_some() {
                    self.group_mode = !self.group_mode;
                }
                true
            }
            _ => true,
        }
    }

    fn handle_search_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Esc => {
                self.search_mode = false;
            }
            KeyCode::Enter => {
                self.search_mode = false;
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
        true
    }

    fn next_view(&mut self) {
        if self.report.views.is_empty() {
            return;
        }
        self.active_view = (self.active_view + 1) % self.report.views.len();
        self.group_mode = self.active_view().group_by.is_some();
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
        self.group_mode = self.active_view().group_by.is_some();
        self.clamp_selection();
    }

    fn move_selection(&mut self, delta: isize) {
        let row_count = self.filtered_rows().len();
        if row_count == 0 {
            self.selected_row = None;
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
    }

    fn clamp_selection(&mut self) {
        let row_count = self.filtered_rows().len();
        self.selected_row = if row_count == 0 {
            None
        } else {
            Some(self.selected_row.unwrap_or(0).min(row_count - 1))
        };
    }

    fn table_rows(&self) -> (Vec<String>, Vec<Vec<String>>, Option<usize>) {
        let view = self.active_view();
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
}

fn selected_row_lines(row: &BasesRow, view: &BasesEvaluatedView) -> Vec<Line<'static>> {
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
    use vulcan_core::{BasesColumn, BasesDiagnostic, BasesGroupBy};

    fn sample_report() -> BasesEvalReport {
        BasesEvalReport {
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
        }
    }

    #[test]
    fn state_filters_rows_by_query() {
        let mut state = BasesTuiState::new(sample_report());
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
        let mut state = BasesTuiState::new(sample_report());
        assert!(state.group_mode);

        state.next_view();
        assert!(!state.group_mode);
    }

    #[test]
    fn table_rows_include_group_column_when_enabled() {
        let state = BasesTuiState::new(sample_report());
        let (headers, rows, selected) = state.table_rows();

        assert_eq!(headers[0], "Category");
        assert_eq!(rows[0][0], "Armor");
        assert_eq!(selected, Some(0));
    }
}
