use crate::commit::AutoCommitPolicy;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::time::Duration;
use toml::Value as TomlValue;
use vulcan_core::{
    ensure_vulcan_dir, validate_vulcan_overrides_toml, ConfigDiagnostic, VaultPaths,
};

const FOOTER_HEIGHT: u16 = 7;

pub fn run_config_tui(paths: &VaultPaths, no_commit: bool, quiet: bool) -> Result<(), io::Error> {
    let mut state = ConfigTuiState::load(paths.clone()).map_err(io::Error::other)?;
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

    let result = run_event_loop(&mut terminal, &mut state, &auto_commit, quiet);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

fn run_event_loop(
    terminal: &mut Terminal<ratatui::backend::CrosstermBackend<io::Stdout>>,
    state: &mut ConfigTuiState,
    auto_commit: &AutoCommitPolicy,
    quiet: bool,
) -> Result<(), io::Error> {
    loop {
        terminal.draw(|frame| draw(frame, state))?;

        if !event::poll(Duration::from_millis(200))? {
            continue;
        }

        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => match state.handle_key(key) {
                ConfigTuiAction::Continue => {}
                ConfigTuiAction::Quit => break,
                ConfigTuiAction::Save => {
                    state.save(auto_commit, quiet);
                }
                ConfigTuiAction::Unset => state.unset_selected(),
            },
            Event::Paste(text) => state.handle_paste(&text),
            _ => {}
        }
    }

    Ok(())
}

fn draw(frame: &mut Frame<'_>, state: &ConfigTuiState) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(12),
            Constraint::Length(FOOTER_HEIGHT),
        ])
        .split(frame.area());

    draw_header(frame, state, layout[0]);
    draw_body(frame, state, layout[1]);
    draw_footer(frame, state, layout[2]);

    if let ConfigInputMode::Edit { .. } = state.input_mode {
        draw_edit_overlay(frame, state);
    }
}

fn draw_header(frame: &mut Frame<'_>, state: &ConfigTuiState, area: Rect) {
    let title = if state.dirty {
        format!(
            "Config Editor: {} [modified]",
            state.paths.config_file().display()
        )
    } else {
        format!("Config Editor: {}", state.paths.config_file().display())
    };
    let header = Paragraph::new(Line::from(vec![
        Span::styled(title, Style::default().fg(Color::Cyan)),
        Span::raw("  "),
        Span::styled(
            state.selected_category().title.clone(),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
    ]))
    .block(Block::default().borders(Borders::ALL));
    frame.render_widget(header, area);
}

fn draw_body(frame: &mut Frame<'_>, state: &ConfigTuiState, area: Rect) {
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(24),
            Constraint::Percentage(33),
            Constraint::Percentage(43),
        ])
        .split(area);

    draw_categories(frame, state, columns[0]);
    draw_entries(frame, state, columns[1]);
    draw_detail(frame, state, columns[2]);
}

fn draw_categories(frame: &mut Frame<'_>, state: &ConfigTuiState, area: Rect) {
    let items = state
        .categories
        .iter()
        .map(|category| {
            let count = category.entries.len();
            ListItem::new(Line::from(format!("{} ({count})", category.title)))
        })
        .collect::<Vec<_>>();
    let list = List::new(items)
        .block(
            Block::default()
                .title("Categories")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .highlight_style(Style::default().bg(Color::DarkGray));
    let mut list_state = ListState::default();
    list_state.select(Some(state.selected_category));
    frame.render_stateful_widget(list, area, &mut list_state);
}

fn draw_entries(frame: &mut Frame<'_>, state: &ConfigTuiState, area: Rect) {
    let items = state
        .selected_category()
        .entries
        .iter()
        .map(|entry| {
            let summary = state.display_summary(entry);
            let mut spans = vec![Span::raw(entry.label.clone())];
            if !summary.is_empty() {
                spans.push(Span::raw(" = "));
                spans.push(Span::styled(summary, Style::default().fg(Color::Gray)));
            }
            if state.local_value(entry).is_some() {
                spans.push(Span::styled(" [local]", Style::default().fg(Color::Yellow)));
            } else if state.shared_value(entry).is_some() {
                spans.push(Span::styled(" [shared]", Style::default().fg(Color::Green)));
            } else {
                spans.push(Span::styled(" [default]", Style::default().fg(Color::Blue)));
            }
            ListItem::new(Line::from(spans))
        })
        .collect::<Vec<_>>();
    let list = List::new(items)
        .block(
            Block::default()
                .title("Settings")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .highlight_style(Style::default().bg(Color::DarkGray));
    let mut list_state = ListState::default();
    list_state.select(Some(state.selected_entry));
    frame.render_stateful_widget(list, area, &mut list_state);
}

fn draw_detail(frame: &mut Frame<'_>, state: &ConfigTuiState, area: Rect) {
    let entry = state.selected_entry();
    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                "Path: ",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(entry.display_path.clone()),
        ]),
        Line::from(""),
        Line::from(entry.description.clone()),
        Line::from(""),
    ];
    lines.extend(render_value_block(
        "Shared override",
        state.shared_value(entry),
    ));
    lines.push(Line::from(""));
    lines.extend(render_value_block(
        "Local override",
        state.local_value(entry),
    ));
    lines.push(Line::from(""));
    lines.extend(render_value_block(
        "Effective value",
        state.effective_value(entry).as_ref(),
    ));

    if state.local_value(entry).is_some() {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![Span::styled(
            "config.local.toml overrides the shared value until that local override is removed.",
            Style::default().fg(Color::Yellow),
        )]));
    }

    let detail = Paragraph::new(lines)
        .block(
            Block::default()
                .title("Detail")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(detail, area);
}

fn render_value_block<'a>(title: &'a str, value: Option<&'a TomlValue>) -> Vec<Line<'static>> {
    let heading = Line::from(vec![Span::styled(
        title.to_string(),
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )]);
    let body = match value {
        Some(value) => toml_value_multiline(value)
            .lines()
            .map(|line| Line::from(line.to_string()))
            .collect::<Vec<_>>(),
        None => vec![Line::from("<unset>".to_string())],
    };
    std::iter::once(heading).chain(body).collect()
}

fn draw_footer(frame: &mut Frame<'_>, state: &ConfigTuiState, area: Rect) {
    let footer = Paragraph::new(vec![
        Line::from("Arrows/Tab navigate  Enter edit  u unset  Ctrl-S save  r revert  q quit"),
        Line::from(state.selected_category().description.clone()),
        Line::from(state.mode_help_line()),
        Line::from(format!(
            "Diagnostics: {} warning{}",
            state.diagnostics.len(),
            if state.diagnostics.len() == 1 {
                ""
            } else {
                "s"
            }
        )),
        Line::from(state.status.clone()),
    ])
    .block(
        Block::default()
            .title("Status")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan)),
    )
    .wrap(Wrap { trim: false });
    frame.render_widget(footer, area);
}

fn draw_edit_overlay(frame: &mut Frame<'_>, state: &ConfigTuiState) {
    let area = centered_rect(70, 35, frame.area());
    frame.render_widget(Clear, area);
    let entry = state.selected_entry();
    let mut lines = vec![
        Line::from(format!("Edit {}", entry.display_path)),
        Line::from("Enter a TOML literal. Bare text is stored as a string."),
        Line::from(""),
    ];
    if let ConfigInputMode::Edit { buffer, error } = &state.input_mode {
        lines.push(Line::from(format!("Value: {buffer}")));
        lines.push(Line::from(""));
        if let Some(error) = error {
            lines.push(Line::from(vec![Span::styled(
                error.clone(),
                Style::default().fg(Color::Red),
            )]));
        } else {
            lines.push(Line::from(
                "Press Enter to apply, Esc to cancel, Backspace to edit.",
            ));
        }
    }
    let paragraph = Paragraph::new(lines)
        .block(
            Block::default()
                .title("Edit Value")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn centered_rect(horizontal_percent: u16, vertical_percent: u16, area: Rect) -> Rect {
    let popup = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - vertical_percent) / 2),
            Constraint::Percentage(vertical_percent),
            Constraint::Percentage((100 - vertical_percent) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - horizontal_percent) / 2),
            Constraint::Percentage(horizontal_percent),
            Constraint::Percentage((100 - horizontal_percent) / 2),
        ])
        .split(popup[1])[1]
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ConfigCategory {
    key: String,
    title: String,
    description: String,
    entries: Vec<ConfigEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ConfigEntry {
    display_path: String,
    display_segments: Vec<String>,
    storage_segments: Vec<String>,
    label: String,
    description: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ConfigInputMode {
    Normal,
    Edit {
        buffer: String,
        error: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConfigTuiAction {
    Continue,
    Quit,
    Save,
    Unset,
}

struct ConfigTuiState {
    paths: VaultPaths,
    diagnostics: Vec<ConfigDiagnostic>,
    effective_toml: TomlValue,
    shared_toml: TomlValue,
    baseline_shared_toml: TomlValue,
    local_toml: TomlValue,
    categories: Vec<ConfigCategory>,
    selected_category: usize,
    selected_entry: usize,
    input_mode: ConfigInputMode,
    status: String,
    dirty: bool,
    confirm_discard: bool,
}

impl ConfigTuiState {
    fn load(paths: VaultPaths) -> Result<Self, String> {
        let display = crate::app_config::build_config_show_report(&paths, None, None)
            .map_err(|error| error.to_string())?;
        let effective_toml = display.rendered_toml.clone();
        let shared_toml = crate::app_config::load_config_file_toml(paths.config_file())
            .map_err(|error| error.to_string())?;
        let local_toml = crate::app_config::load_config_file_toml(paths.local_config_file())
            .map_err(|error| error.to_string())?;
        let categories = build_categories(&effective_toml, &shared_toml, &local_toml);
        let status = if display.diagnostics.is_empty() {
            "Loaded .vulcan/config.toml. Edit a setting, then press Ctrl-S to save.".to_string()
        } else {
            format!(
                "Loaded config with {} warning{}.",
                display.diagnostics.len(),
                if display.diagnostics.len() == 1 {
                    ""
                } else {
                    "s"
                }
            )
        };
        Ok(Self {
            paths,
            diagnostics: display.diagnostics,
            effective_toml,
            shared_toml: shared_toml.clone(),
            baseline_shared_toml: shared_toml,
            local_toml,
            categories,
            selected_category: 0,
            selected_entry: 0,
            input_mode: ConfigInputMode::Normal,
            status,
            dirty: false,
            confirm_discard: false,
        })
    }

    fn handle_key(&mut self, key: KeyEvent) -> ConfigTuiAction {
        if matches!(self.input_mode, ConfigInputMode::Normal) {
            return self.handle_normal_key(key);
        }

        self.confirm_discard = false;
        let mut apply_buffer = None;
        let mut cancel_edit = false;
        if let ConfigInputMode::Edit { buffer, error } = &mut self.input_mode {
            match key.code {
                KeyCode::Esc => cancel_edit = true,
                KeyCode::Enter => apply_buffer = Some(buffer.clone()),
                KeyCode::Backspace => {
                    buffer.pop();
                    *error = None;
                }
                KeyCode::Char(character)
                    if !key.modifiers.contains(KeyModifiers::CONTROL)
                        && !key.modifiers.contains(KeyModifiers::ALT) =>
                {
                    buffer.push(character);
                    *error = None;
                }
                KeyCode::Tab => {
                    buffer.push('\t');
                    *error = None;
                }
                _ => {}
            }
        }

        if cancel_edit {
            self.input_mode = ConfigInputMode::Normal;
            self.status = "Cancelled edit.".to_string();
            return ConfigTuiAction::Continue;
        }

        if let Some(buffer) = apply_buffer {
            if let Err(message) = self.apply_buffered_edit(&buffer) {
                if let ConfigInputMode::Edit { error, .. } = &mut self.input_mode {
                    *error = Some(message);
                }
            }
        }

        ConfigTuiAction::Continue
    }

    fn handle_paste(&mut self, text: &str) {
        if let ConfigInputMode::Edit { buffer, error } = &mut self.input_mode {
            buffer.push_str(text);
            *error = None;
        }
    }

    fn handle_normal_key(&mut self, key: KeyEvent) -> ConfigTuiAction {
        self.confirm_discard = false;
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.move_entry(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_entry(1),
            KeyCode::Left | KeyCode::BackTab => self.move_category(-1),
            KeyCode::Right | KeyCode::Tab => self.move_category(1),
            KeyCode::Enter | KeyCode::Char('e') => self.open_edit(),
            KeyCode::Char('u') => return ConfigTuiAction::Unset,
            KeyCode::Char('r') => self.revert_changes(),
            KeyCode::Char('q') => {
                if self.dirty {
                    if self.confirm_discard {
                        return ConfigTuiAction::Quit;
                    }
                    self.confirm_discard = true;
                    self.status =
                        "Unsaved changes. Press q again to discard, or Ctrl-S to save.".to_string();
                } else {
                    return ConfigTuiAction::Quit;
                }
            }
            KeyCode::Esc => {
                if self.dirty {
                    self.confirm_discard = true;
                    self.status =
                        "Unsaved changes. Press q to discard, or Ctrl-S to save.".to_string();
                }
            }
            KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return ConfigTuiAction::Save;
            }
            _ => {}
        }
        ConfigTuiAction::Continue
    }

    fn open_edit(&mut self) {
        let entry = self.selected_entry().clone();
        let current = self
            .shared_value(&entry)
            .or_else(|| self.local_value(&entry))
            .cloned()
            .or_else(|| self.effective_value(&entry));
        let buffer = current.as_ref().map_or_else(String::new, toml_literal);
        self.input_mode = ConfigInputMode::Edit {
            buffer,
            error: None,
        };
        self.status = format!("Editing {}.", entry.display_path);
    }

    fn apply_buffered_edit(&mut self, buffer: &str) -> Result<(), String> {
        let trimmed = buffer.trim();
        if trimmed.is_empty() {
            return Err(
                "Empty values are not allowed. Use `u` to unset the shared override.".to_string(),
            );
        }

        let value = parse_toml_literal(trimmed)?;
        let entry = self.selected_entry().clone();
        let mut updated = self.shared_toml.clone();
        let storage_segments = entry
            .storage_segments
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        crate::app_config::set_config_toml_value(&mut updated, &storage_segments, value)
            .map_err(|error| error.to_string())?;
        let rendered = toml::to_string_pretty(&updated).map_err(|error| error.to_string())?;
        validate_vulcan_overrides_toml(&rendered).map_err(|error| error.to_string())?;

        self.shared_toml = updated;
        self.dirty = self.shared_toml != self.baseline_shared_toml;
        self.input_mode = ConfigInputMode::Normal;
        self.refresh_categories(Some(entry.display_path.clone()));
        if self.local_value(self.selected_entry()).is_some() {
            self.status = format!(
                "Updated {} in shared config. A local override still wins for the effective value.",
                entry.display_path
            );
        } else {
            self.status = format!("Updated {} in the working copy.", entry.display_path);
        }
        Ok(())
    }

    fn unset_selected(&mut self) {
        let entry = self.selected_entry().clone();
        let storage_segments = entry
            .storage_segments
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let removed = match crate::app_config::remove_config_toml_value(
            &mut self.shared_toml,
            &storage_segments,
        ) {
            Ok(removed) => removed,
            Err(error) => {
                self.status = error.to_string();
                return;
            }
        };
        if !removed {
            self.status = format!("{} has no shared override to remove.", entry.display_path);
            return;
        }
        self.dirty = self.shared_toml != self.baseline_shared_toml;
        self.refresh_categories(Some(entry.display_path.clone()));
        self.status = if self.local_value(self.selected_entry()).is_some() {
            format!(
                "Removed the shared override for {}. config.local.toml still overrides it.",
                entry.display_path
            )
        } else {
            format!("Removed the shared override for {}.", entry.display_path)
        };
    }

    fn revert_changes(&mut self) {
        if !self.dirty {
            self.status = "No pending changes to revert.".to_string();
            return;
        }
        let selected = self.selected_entry().display_path.clone();
        self.shared_toml = self.baseline_shared_toml.clone();
        self.dirty = false;
        self.input_mode = ConfigInputMode::Normal;
        self.refresh_categories(Some(selected));
        self.status = "Reverted unsaved changes.".to_string();
    }

    fn save(&mut self, auto_commit: &AutoCommitPolicy, quiet: bool) {
        let rendered = match toml::to_string_pretty(&self.shared_toml) {
            Ok(rendered) => rendered,
            Err(error) => {
                self.set_status(error.to_string());
                return;
            }
        };
        if let Err(error) = validate_vulcan_overrides_toml(&rendered) {
            self.set_status(error.to_string());
            return;
        }

        let config_path = self.paths.config_file().to_path_buf();
        let previous_contents = fs::read_to_string(&config_path).ok();
        if previous_contents.as_deref() == Some(rendered.as_str()) {
            self.dirty = false;
            self.confirm_discard = false;
            self.status = "No config changes to save.".to_string();
            return;
        }

        let had_gitignore = self.paths.gitignore_file().exists();
        if let Err(error) = ensure_vulcan_dir(&self.paths) {
            self.set_status(error.to_string());
            return;
        }
        if let Err(error) = fs::write(&config_path, &rendered) {
            self.set_status(error.to_string());
            return;
        }

        let commit_result = auto_commit.commit(
            &self.paths,
            "config-edit",
            &crate::config_set_changed_files(&self.paths, had_gitignore),
            None,
            quiet,
        );

        match Self::load(self.paths.clone()) {
            Ok(mut reloaded) => {
                reloaded.status = if let Err(error) = commit_result {
                    format!("Saved .vulcan/config.toml, but auto-commit failed: {error}")
                } else {
                    "Saved .vulcan/config.toml.".to_string()
                };
                *self = reloaded;
            }
            Err(error) => {
                self.baseline_shared_toml = self.shared_toml.clone();
                self.dirty = false;
                self.confirm_discard = false;
                self.status = if let Err(commit_error) = commit_result {
                    format!(
                        "Saved .vulcan/config.toml, but reload failed ({error}) and auto-commit failed: {commit_error}"
                    )
                } else {
                    format!("Saved .vulcan/config.toml, but reload failed: {error}")
                };
            }
        }
    }

    fn selected_category(&self) -> &ConfigCategory {
        &self.categories[self.selected_category]
    }

    fn selected_entry(&self) -> &ConfigEntry {
        &self.selected_category().entries[self.selected_entry]
    }

    fn shared_value(&self, entry: &ConfigEntry) -> Option<&TomlValue> {
        get_toml_value(&self.shared_toml, &entry.storage_segments)
    }

    fn local_value(&self, entry: &ConfigEntry) -> Option<&TomlValue> {
        get_toml_value(&self.local_toml, &entry.storage_segments)
    }

    fn effective_value(&self, entry: &ConfigEntry) -> Option<TomlValue> {
        get_toml_value(&self.effective_toml, &entry.display_segments).cloned()
    }

    fn display_summary(&self, entry: &ConfigEntry) -> String {
        self.local_value(entry)
            .or_else(|| self.shared_value(entry))
            .cloned()
            .or_else(|| self.effective_value(entry))
            .map_or_else(String::new, |value| summarize_toml_value(&value))
    }

    fn mode_help_line(&self) -> String {
        match self.input_mode {
            ConfigInputMode::Normal => {
                if self.confirm_discard {
                    "Discard is armed until you press another key.".to_string()
                } else {
                    "Browse categories on the left, settings in the middle, and details on the right."
                        .to_string()
                }
            }
            ConfigInputMode::Edit { .. } => {
                "Editing one TOML literal in memory. Save writes the whole shared config file."
                    .to_string()
            }
        }
    }

    fn move_category(&mut self, delta: isize) {
        if self.categories.is_empty() {
            return;
        }
        self.selected_category = wrap_index(self.selected_category, self.categories.len(), delta);
        self.selected_entry = self
            .selected_entry
            .min(self.selected_category().entries.len().saturating_sub(1));
        self.status = format!("Category: {}.", self.selected_category().title);
    }

    fn move_entry(&mut self, delta: isize) {
        if self.selected_category().entries.is_empty() {
            return;
        }
        self.selected_entry = wrap_index(
            self.selected_entry,
            self.selected_category().entries.len(),
            delta,
        );
    }

    fn refresh_categories(&mut self, selected_path: Option<String>) {
        self.categories =
            build_categories(&self.effective_toml, &self.shared_toml, &self.local_toml);
        if self.categories.is_empty() {
            self.selected_category = 0;
            self.selected_entry = 0;
            return;
        }

        if let Some(selected_path) = selected_path {
            if let Some((category_index, entry_index)) = self.find_entry(&selected_path) {
                self.selected_category = category_index;
                self.selected_entry = entry_index;
                return;
            }
        }

        self.selected_category = self
            .selected_category
            .min(self.categories.len().saturating_sub(1));
        self.selected_entry = self
            .selected_entry
            .min(self.selected_category().entries.len().saturating_sub(1));
    }

    fn find_entry(&self, display_path: &str) -> Option<(usize, usize)> {
        self.categories
            .iter()
            .enumerate()
            .find_map(|(category_index, category)| {
                category
                    .entries
                    .iter()
                    .position(|entry| entry.display_path == display_path)
                    .map(|entry_index| (category_index, entry_index))
            })
    }

    fn set_status(&mut self, status: String) {
        self.status = status;
    }
}

fn build_categories(
    effective_toml: &TomlValue,
    shared_toml: &TomlValue,
    local_toml: &TomlValue,
) -> Vec<ConfigCategory> {
    let mut display_paths = BTreeSet::new();
    collect_leaf_paths(effective_toml, &mut Vec::new(), &mut display_paths);

    let mut shared_paths = BTreeSet::new();
    collect_leaf_paths(shared_toml, &mut Vec::new(), &mut shared_paths);
    display_paths.extend(
        shared_paths
            .into_iter()
            .map(|segments| storage_path_to_display_path(&segments)),
    );

    let mut local_paths = BTreeSet::new();
    collect_leaf_paths(local_toml, &mut Vec::new(), &mut local_paths);
    display_paths.extend(
        local_paths
            .into_iter()
            .map(|segments| storage_path_to_display_path(&segments)),
    );

    let mut grouped = BTreeMap::<String, ConfigCategory>::new();
    for display_segments in display_paths {
        let display_path = display_segments.join(".");
        let storage_segments = display_path_to_storage_path(&display_segments);
        let descriptor = category_descriptor(&display_segments);
        let entry = ConfigEntry {
            label: entry_label(&display_segments),
            description: option_description(&display_path),
            display_path: display_path.clone(),
            display_segments,
            storage_segments,
        };
        grouped
            .entry(descriptor.key.to_string())
            .or_insert_with(|| ConfigCategory {
                key: descriptor.key.to_string(),
                title: descriptor.title.to_string(),
                description: descriptor.description.to_string(),
                entries: Vec::new(),
            })
            .entries
            .push(entry);
    }

    let mut categories = grouped.into_values().collect::<Vec<_>>();
    categories.sort_by_key(|category| category_order(&category.key));
    for category in &mut categories {
        category
            .entries
            .sort_by(|left, right| left.display_path.cmp(&right.display_path));
    }
    categories
}

fn collect_leaf_paths(
    value: &TomlValue,
    prefix: &mut Vec<String>,
    out: &mut BTreeSet<Vec<String>>,
) {
    match value {
        TomlValue::Table(table) => {
            for (key, child) in table {
                prefix.push(key.clone());
                collect_leaf_paths(child, prefix, out);
                prefix.pop();
            }
        }
        _ => {
            if !prefix.is_empty() {
                out.insert(prefix.clone());
            }
        }
    }
}

fn get_toml_value<'a>(value: &'a TomlValue, path: &[String]) -> Option<&'a TomlValue> {
    let mut current = value;
    for segment in path {
        current = current.as_table()?.get(segment)?;
    }
    Some(current)
}

fn parse_toml_literal(raw: &str) -> Result<TomlValue, String> {
    let wrapped = format!("value = {raw}\n");
    if let Some(value) = wrapped
        .parse::<TomlValue>()
        .ok()
        .and_then(|value| value.get("value").cloned())
    {
        return Ok(value);
    }

    if looks_like_plain_string(raw) {
        Ok(TomlValue::String(raw.to_string()))
    } else {
        Err("Invalid TOML literal. Quote strings or fix the structured value.".to_string())
    }
}

fn looks_like_plain_string(raw: &str) -> bool {
    !raw.chars().any(|character| {
        matches!(
            character,
            '{' | '}' | '[' | ']' | ',' | '\n' | '\r' | '"' | '\''
        )
    })
}

fn toml_literal(value: &TomlValue) -> String {
    match value {
        TomlValue::String(text) => format!("{text:?}"),
        TomlValue::Integer(number) => number.to_string(),
        TomlValue::Float(number) => number.to_string(),
        TomlValue::Boolean(value_bool) => value_bool.to_string(),
        TomlValue::Datetime(datetime) => datetime.to_string(),
        TomlValue::Array(_) | TomlValue::Table(_) => value.to_string(),
    }
}

fn summarize_toml_value(value: &TomlValue) -> String {
    let summary = match value {
        TomlValue::String(text) => text.clone(),
        TomlValue::Integer(number) => number.to_string(),
        TomlValue::Float(number) => number.to_string(),
        TomlValue::Boolean(value_bool) => value_bool.to_string(),
        TomlValue::Datetime(datetime) => datetime.to_string(),
        TomlValue::Array(values) => format!(
            "[{} item{}]",
            values.len(),
            if values.len() == 1 { "" } else { "s" }
        ),
        TomlValue::Table(values) => format!(
            "{{{} key{}}}",
            values.len(),
            if values.len() == 1 { "" } else { "s" }
        ),
    };
    truncate_summary(&summary, 48)
}

fn truncate_summary(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    text.chars()
        .take(limit.saturating_sub(1))
        .collect::<String>()
        + "…"
}

fn toml_value_multiline(value: &TomlValue) -> String {
    match value {
        TomlValue::String(_)
        | TomlValue::Integer(_)
        | TomlValue::Float(_)
        | TomlValue::Boolean(_)
        | TomlValue::Datetime(_) => toml_literal(value),
        TomlValue::Array(_) | TomlValue::Table(_) => {
            serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
        }
    }
}

fn storage_path_to_display_path(storage_segments: &[String]) -> Vec<String> {
    match storage_segments {
        [first, second] if first == "links" && second == "resolution" => {
            vec!["link_resolution".to_string()]
        }
        [first, second] if first == "links" && second == "style" => {
            vec!["link_style".to_string()]
        }
        [first, second] if first == "links" && second == "attachment_folder" => {
            vec!["attachment_folder".to_string()]
        }
        _ => storage_segments.to_vec(),
    }
}

fn display_path_to_storage_path(display_segments: &[String]) -> Vec<String> {
    match display_segments {
        [segment] if segment == "link_resolution" => {
            vec!["links".to_string(), "resolution".to_string()]
        }
        [segment] if segment == "link_style" => {
            vec!["links".to_string(), "style".to_string()]
        }
        [segment] if segment == "attachment_folder" => {
            vec!["links".to_string(), "attachment_folder".to_string()]
        }
        _ => display_segments.to_vec(),
    }
}

struct CategoryDescriptor {
    key: &'static str,
    title: &'static str,
    description: &'static str,
}

fn category_descriptor(display_segments: &[String]) -> CategoryDescriptor {
    match display_segments.first().map(String::as_str) {
        Some("link_resolution" | "link_style" | "attachment_folder" | "strict_line_breaks") => {
            CategoryDescriptor {
                key: "links",
                title: "Links",
                description:
                    "Link formatting, resolution rules, attachment paths, and Markdown compatibility.",
            }
        }
        Some("property_types") => CategoryDescriptor {
            key: "properties",
            title: "Properties",
            description: "Typed frontmatter and property parsing overrides.",
        },
        Some("templates") => CategoryDescriptor {
            key: "templates",
            title: "Templates",
            description: "Template folders, triggers, and Templater-compatible defaults.",
        },
        Some("periodic") => CategoryDescriptor {
            key: "periodic",
            title: "Periodic Notes",
            description: "Daily, weekly, monthly, quarterly, and yearly note generation settings.",
        },
        Some("tasks") => CategoryDescriptor {
            key: "tasks",
            title: "Tasks",
            description: "Tasks query defaults, statuses, and recurrence behavior.",
        },
        Some("tasknotes") => CategoryDescriptor {
            key: "tasknotes",
            title: "TaskNotes",
            description: "TaskNotes folders, statuses, NLP, pomodoro, and saved views.",
        },
        Some("kanban") => CategoryDescriptor {
            key: "kanban",
            title: "Kanban",
            description: "Kanban board formatting, archiving, and display preferences.",
        },
        Some("dataview") => CategoryDescriptor {
            key: "dataview",
            title: "Dataview",
            description: "Dataview compatibility flags, rendering behavior, and JS limits.",
        },
        Some("js_runtime") => CategoryDescriptor {
            key: "js_runtime",
            title: "JS Runtime",
            description: "Sandbox defaults, runtime memory limits, and script locations.",
        },
        Some("web") => CategoryDescriptor {
            key: "web",
            title: "Web",
            description: "Web search backend selection and API endpoint configuration.",
        },
        Some("plugins") => CategoryDescriptor {
            key: "plugins",
            title: "Plugins",
            description: "Registered event-driven plugin settings for the current vault.",
        },
        Some("permissions") => CategoryDescriptor {
            key: "permissions",
            title: "Permissions",
            description: "Static permission profiles used by plugins, MCP, and scripted callers.",
        },
        Some("aliases") => CategoryDescriptor {
            key: "aliases",
            title: "Aliases",
            description: "Custom top-level CLI command aliases expanded before clap parsing.",
        },
        Some(_) | None => CategoryDescriptor {
            key: "general",
            title: "General",
            description: "Top-level vault configuration not covered by a more specific section.",
        },
    }
}

fn category_order(key: &str) -> usize {
    match key {
        "general" => 0,
        "links" => 1,
        "properties" => 2,
        "templates" => 3,
        "periodic" => 4,
        "tasks" => 5,
        "tasknotes" => 6,
        "kanban" => 7,
        "dataview" => 8,
        "js_runtime" => 9,
        "web" => 10,
        "plugins" => 11,
        "permissions" => 12,
        "aliases" => 13,
        _ => 50,
    }
}

fn entry_label(display_segments: &[String]) -> String {
    let category_key = category_descriptor(display_segments).key;
    if category_key == "general" || display_segments.len() == 1 {
        return display_segments.join(".");
    }
    display_segments[1..].join(".")
}

fn option_description(path: &str) -> String {
    match path {
        "link_resolution" => "Choose whether new links resolve relative to the current file or the vault root.".to_string(),
        "link_style" => "Select wikilink or Markdown link formatting for generated links.".to_string(),
        "attachment_folder" => "Override the preferred folder for new attachments.".to_string(),
        "strict_line_breaks" => "Mirror Obsidian's strict line break behavior when rendering Markdown.".to_string(),
        _ if path.starts_with("periodic.") => {
            "Periodic note folder, filename format, template, cadence, and schedule heading.".to_string()
        }
        _ if path.starts_with("templates.") => {
            "Template discovery, file triggers, folder mappings, and shell integration.".to_string()
        }
        _ if path.starts_with("tasks.") => {
            "Task query defaults, status sets, created-date behavior, and recurrence settings.".to_string()
        }
        _ if path.starts_with("tasknotes.") => {
            "TaskNotes task storage, metadata mapping, automation defaults, and saved view settings.".to_string()
        }
        _ if path.starts_with("kanban.") => {
            "Kanban board metadata keys, archiving, layout, and card creation settings.".to_string()
        }
        _ if path.starts_with("dataview.") => {
            "Dataview rendering compatibility, inline query prefixes, and JS execution limits.".to_string()
        }
        _ if path.starts_with("js_runtime.") => {
            "Default sandbox, memory, stack, timeout, and script folder settings for `vulcan run`.".to_string()
        }
        _ if path.starts_with("web.search.") => {
            "Configure the preferred web search provider, API key env var, and base URL.".to_string()
        }
        _ if path.starts_with("web.") => {
            "Shared web client settings such as the user agent used by fetch/search helpers.".to_string()
        }
        _ if path.starts_with("permissions.profiles.") => {
            "Static permission profile rule used to restrict reads, writes, network, shell, or runtime limits.".to_string()
        }
        _ if path.starts_with("plugins.") => {
            "Per-plugin registration, hook subscription, sandbox, and permission profile settings.".to_string()
        }
        _ if path.starts_with("aliases.") => {
            "Alias expansion for short custom commands like `today = \"query --format count\"`.".to_string()
        }
        _ if path.starts_with("property_types.") => {
            "Explicit type overrides for frontmatter properties discovered in the vault.".to_string()
        }
        _ => format!("Edit `{path}` in `.vulcan/config.toml`."),
    }
}

fn wrap_index(current: usize, len: usize, delta: isize) -> usize {
    if len == 0 {
        return 0;
    }
    match delta.cmp(&0) {
        std::cmp::Ordering::Less => {
            if current == 0 {
                len - 1
            } else {
                current - 1
            }
        }
        std::cmp::Ordering::Greater => {
            if current + 1 >= len {
                0
            } else {
                current + 1
            }
        }
        std::cmp::Ordering::Equal => current,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use tempfile::TempDir;

    fn sample_state() -> ConfigTuiState {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        fs::create_dir_all(vault_root.join(".vulcan")).expect("config dir should exist");
        fs::write(
            vault_root.join(".vulcan/config.toml"),
            concat!(
                "[periodic.daily]\n",
                "folder = \"Journal/Daily\"\n",
                "\n",
                "[web.search]\n",
                "backend = \"duckduckgo\"\n",
            ),
        )
        .expect("config should write");
        fs::write(
            vault_root.join(".vulcan/config.local.toml"),
            "[periodic.daily]\ntemplate = \"Templates/Local\"\n",
        )
        .expect("local config should write");
        let paths = VaultPaths::new(vault_root);
        let mut state = ConfigTuiState::load(paths).expect("state should load");
        if let Some((category_index, entry_index)) = state.find_entry("periodic.daily.folder") {
            state.selected_category = category_index;
            state.selected_entry = entry_index;
        }
        state
    }

    #[test]
    fn builds_categories_with_descriptions() {
        let state = sample_state();

        assert!(state
            .categories
            .iter()
            .any(|category| category.title == "Periodic Notes"));
        let entry = state
            .categories
            .iter()
            .flat_map(|category| category.entries.iter())
            .find(|entry| entry.display_path == "periodic.daily.folder")
            .expect("periodic entry should exist");
        assert!(entry.description.contains("Periodic note"));
    }

    #[test]
    fn apply_buffered_edit_updates_working_copy() {
        let mut state = sample_state();
        state
            .apply_buffered_edit("\"Journal/Updated\"")
            .expect("edit should apply");

        let entry = state.selected_entry().clone();
        assert_eq!(
            state.shared_value(&entry),
            Some(&TomlValue::String("Journal/Updated".to_string()))
        );
        assert!(state.dirty);
    }

    #[test]
    fn apply_buffered_edit_rejects_invalid_values() {
        let mut state = sample_state();
        let result = state.apply_buffered_edit("{ invalid");

        assert!(result.is_err());
        let entry = state.selected_entry().clone();
        assert_eq!(
            state.shared_value(&entry),
            Some(&TomlValue::String("Journal/Daily".to_string()))
        );
        assert!(!state.dirty);
    }

    #[test]
    fn unset_selected_removes_shared_override_without_touching_local_override() {
        let mut state = sample_state();
        let (category_index, entry_index) = state
            .find_entry("periodic.daily.template")
            .expect("template entry should exist");
        state.selected_category = category_index;
        state.selected_entry = entry_index;

        state.unset_selected();

        let entry = state.selected_entry().clone();
        assert!(state.shared_value(&entry).is_none());
        assert_eq!(
            state.local_value(&entry),
            Some(&TomlValue::String("Templates/Local".to_string()))
        );
    }

    #[test]
    fn draw_handles_small_terminal_sizes() {
        let state = sample_state();
        let backend = TestBackend::new(48, 14);
        let mut terminal = Terminal::new(backend).expect("terminal should build");
        terminal
            .draw(|frame| draw(frame, &state))
            .expect("config tui should render");
    }

    #[test]
    fn tasknotes_storage_alias_reverse_mapping_is_identity() {
        let storage = vec!["tasknotes".to_string(), "tasks_folder".to_string()];
        assert_eq!(storage_path_to_display_path(&storage), storage);
    }
}
