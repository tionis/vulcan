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
use std::io;
use std::time::Duration;
use toml::Value as TomlValue;
use vulcan_core::{ConfigDiagnostic, VaultPaths};

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
                ConfigTuiAction::ToggleTarget => state.toggle_target(),
                ConfigTuiAction::Create => state.open_create(),
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

    if matches!(
        state.input_mode,
        ConfigInputMode::Edit { .. } | ConfigInputMode::CreateDynamic { .. }
    ) {
        draw_input_overlay(frame, state);
    }
}

fn draw_header(frame: &mut Frame<'_>, state: &ConfigTuiState, area: Rect) {
    let title = if state.dirty {
        "Config Editor [modified]".to_string()
    } else {
        "Config Editor".to_string()
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
        Span::raw("  "),
        Span::styled(
            format!("target: {}", state.selected_target_label()),
            Style::default().fg(Color::Green),
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
            let (source_label, source_color) = ConfigTuiState::entry_source_badge(entry);
            spans.push(Span::styled(
                format!(" [{source_label}]"),
                Style::default().fg(source_color),
            ));
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
        Line::from(vec![
            Span::styled(
                "Type: ",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(ConfigTuiState::entry_kind_label(entry)),
        ]),
        Line::from(vec![
            Span::styled(
                "Target: ",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(ConfigTuiState::entry_target_label(entry)),
        ]),
        Line::from(vec![
            Span::styled(
                "Source: ",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(ConfigTuiState::value_source_label(entry).to_string()),
        ]),
        Line::from(""),
        Line::from(entry.description.clone()),
        Line::from(""),
    ];
    if !entry.enum_values.is_empty() {
        lines.push(Line::from(format!(
            "Allowed values: {}",
            entry.enum_values.join(", ")
        )));
        lines.push(Line::from(""));
    }
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
    if let Some(default_value) = entry.default_value.as_ref() {
        lines.push(Line::from(""));
        lines.extend(render_value_block("Default value", Some(default_value)));
    }

    if let Some(command) = entry.preferred_command.as_deref() {
        lines.push(Line::from(""));
        lines.push(Line::from(format!("Preferred command: {command}")));
    }

    if let Some(example) = entry.examples.first() {
        lines.push(Line::from(""));
        lines.push(Line::from(format!("Example: {example}")));
    }

    if entry.key.contains('<') {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![Span::styled(
            "This is a schema template. Press Enter or `n` to create a concrete entry.",
            Style::default().fg(Color::Yellow),
        )]));
    } else if state.local_value(entry).is_some() {
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
        Line::from(
            "Arrows/Tab navigate  Enter edit/create  n create  t target  u unset  Ctrl-S save  r revert  q quit",
        ),
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

fn draw_input_overlay(frame: &mut Frame<'_>, state: &ConfigTuiState) {
    let area = centered_rect(70, 35, frame.area());
    frame.render_widget(Clear, area);
    let entry = state.selected_entry();
    let lines = state.overlay_lines(entry);
    let paragraph = Paragraph::new(lines)
        .block(
            Block::default()
                .title(state.overlay_title(entry))
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

#[derive(Debug, Clone, PartialEq)]
struct ConfigCategory {
    key: String,
    title: String,
    description: String,
    entries: Vec<ConfigEntry>,
}

#[derive(Debug, Clone, PartialEq)]
struct ConfigEntry {
    key: String,
    storage_key: String,
    display_path: String,
    display_segments: Vec<String>,
    storage_segments: Vec<String>,
    label: String,
    description: String,
    kind: crate::app_config::ConfigValueKind,
    enum_values: Vec<String>,
    target_support: crate::app_config::ConfigTargetSupport,
    value_source: crate::app_config::ConfigValueSource,
    default_value: Option<TomlValue>,
    default_display: Option<String>,
    examples: Vec<String>,
    preferred_command: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ValueEditorMode {
    RawToml,
    Enum {
        options: Vec<String>,
        selected: usize,
    },
    StringList,
    ScalarMap,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ConfigInputMode {
    Normal,
    Edit {
        buffer: String,
        editor: ValueEditorMode,
        error: Option<String>,
    },
    CreateDynamic {
        name: String,
        value: String,
        editing_value: bool,
        error: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConfigTuiAction {
    Continue,
    Quit,
    Save,
    Unset,
    ToggleTarget,
    Create,
}

struct ConfigTuiState {
    paths: VaultPaths,
    diagnostics: Vec<ConfigDiagnostic>,
    effective_toml: TomlValue,
    shared_toml: TomlValue,
    baseline_shared_toml: TomlValue,
    local_toml: TomlValue,
    baseline_local_toml: TomlValue,
    categories: Vec<ConfigCategory>,
    selected_category: usize,
    selected_entry: usize,
    input_mode: ConfigInputMode,
    selected_target: crate::app_config::ConfigTarget,
    status: String,
    dirty: bool,
    confirm_discard: bool,
}

impl ConfigTuiState {
    fn load(paths: VaultPaths) -> Result<Self, String> {
        let shared_toml = crate::app_config::load_config_file_toml(paths.config_file())
            .map_err(|error| error.to_string())?;
        let local_toml = crate::app_config::load_config_file_toml(paths.local_config_file())
            .map_err(|error| error.to_string())?;
        let (effective_toml, diagnostics, categories) =
            rebuild_schema_state(&paths, &shared_toml, &local_toml)?;
        let status = if diagnostics.is_empty() {
            "Loaded .vulcan/config.toml. Edit a setting, then press Ctrl-S to save.".to_string()
        } else {
            format!(
                "Loaded config with {} warning{}.",
                diagnostics.len(),
                if diagnostics.len() == 1 { "" } else { "s" }
            )
        };
        Ok(Self {
            paths,
            diagnostics,
            effective_toml,
            shared_toml: shared_toml.clone(),
            baseline_shared_toml: shared_toml,
            local_toml: local_toml.clone(),
            baseline_local_toml: local_toml,
            categories,
            selected_category: 0,
            selected_entry: 0,
            input_mode: ConfigInputMode::Normal,
            selected_target: crate::app_config::ConfigTarget::Shared,
            status,
            dirty: false,
            confirm_discard: false,
        })
    }

    #[allow(clippy::too_many_lines)]
    fn handle_key(&mut self, key: KeyEvent) -> ConfigTuiAction {
        if matches!(self.input_mode, ConfigInputMode::Normal) {
            return self.handle_normal_key(key);
        }

        self.confirm_discard = false;
        let mut apply_buffer = None;
        let mut apply_create = None;
        let mut create_focus = None;
        let mut cancel_edit = false;
        match &mut self.input_mode {
            ConfigInputMode::Edit {
                buffer,
                editor,
                error,
            } => match editor {
                ValueEditorMode::Enum { options, selected } => match key.code {
                    KeyCode::Esc => cancel_edit = true,
                    KeyCode::Enter => {
                        apply_buffer = options.get(*selected).cloned();
                    }
                    KeyCode::Up | KeyCode::Left | KeyCode::Char('k') => {
                        *selected = wrap_index(*selected, options.len(), -1);
                        if let Some(value) = options.get(*selected) {
                            *buffer = value.clone();
                        }
                        *error = None;
                    }
                    KeyCode::Down | KeyCode::Right | KeyCode::Tab | KeyCode::Char('j') => {
                        *selected = wrap_index(*selected, options.len(), 1);
                        if let Some(value) = options.get(*selected) {
                            *buffer = value.clone();
                        }
                        *error = None;
                    }
                    KeyCode::Char(character)
                        if !key.modifiers.contains(KeyModifiers::CONTROL)
                            && !key.modifiers.contains(KeyModifiers::ALT) =>
                    {
                        let needle = character.to_ascii_lowercase().to_string();
                        if let Some((index, value)) = options
                            .iter()
                            .enumerate()
                            .find(|(_, value)| value.to_ascii_lowercase().starts_with(&needle))
                        {
                            *selected = index;
                            *buffer = value.clone();
                        }
                        *error = None;
                    }
                    _ => {}
                },
                ValueEditorMode::RawToml
                | ValueEditorMode::StringList
                | ValueEditorMode::ScalarMap => match key.code {
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
                        *error = None;
                    }
                    _ => {}
                },
            },
            ConfigInputMode::CreateDynamic {
                name,
                value,
                editing_value,
                error,
            } => match key.code {
                KeyCode::Esc => cancel_edit = true,
                KeyCode::Enter if !*editing_value => {
                    *editing_value = true;
                    create_focus = Some((name.clone(), *editing_value));
                }
                KeyCode::Enter => apply_create = Some((name.clone(), value.clone())),
                KeyCode::Backspace => {
                    if *editing_value {
                        value.pop();
                    } else {
                        name.pop();
                    }
                    *error = None;
                }
                KeyCode::Tab => {
                    *editing_value = !*editing_value;
                    *error = None;
                    create_focus = Some((name.clone(), *editing_value));
                }
                KeyCode::Char(character)
                    if !key.modifiers.contains(KeyModifiers::CONTROL)
                        && !key.modifiers.contains(KeyModifiers::ALT) =>
                {
                    if *editing_value {
                        value.push(character);
                    } else {
                        name.push(character);
                    }
                    *error = None;
                }
                _ => {}
            },
            ConfigInputMode::Normal => {}
        }

        if cancel_edit {
            self.input_mode = ConfigInputMode::Normal;
            self.status = "Cancelled edit.".to_string();
            return ConfigTuiAction::Continue;
        }

        if let Some((name, value)) = apply_create {
            if let Err(message) = self.apply_create_dynamic_entry(&name, &value) {
                if let ConfigInputMode::CreateDynamic { error, .. } = &mut self.input_mode {
                    *error = Some(message);
                }
            }
            return ConfigTuiAction::Continue;
        }

        if let Some(buffer) = apply_buffer {
            if let Err(message) = self.apply_buffered_edit(&buffer) {
                if let ConfigInputMode::Edit { error, .. } = &mut self.input_mode {
                    *error = Some(message);
                }
            }
        }

        if let Some((name, editing_value)) = create_focus {
            let entry = self.selected_entry().clone();
            self.status = self.create_dynamic_status(&entry, &name, editing_value);
        }

        ConfigTuiAction::Continue
    }

    fn handle_paste(&mut self, text: &str) {
        match &mut self.input_mode {
            ConfigInputMode::Edit { buffer, error, .. } => {
                buffer.push_str(text);
                *error = None;
            }
            ConfigInputMode::CreateDynamic {
                name,
                value,
                editing_value,
                error,
            } => {
                if *editing_value {
                    value.push_str(text);
                } else {
                    name.push_str(text);
                }
                *error = None;
            }
            ConfigInputMode::Normal => {}
        }
    }

    fn handle_normal_key(&mut self, key: KeyEvent) -> ConfigTuiAction {
        self.confirm_discard = false;
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.move_entry(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_entry(1),
            KeyCode::Left | KeyCode::BackTab => self.move_category(-1),
            KeyCode::Right | KeyCode::Tab => self.move_category(1),
            KeyCode::Enter | KeyCode::Char('e') => {
                if self.selected_entry().key.contains('<') {
                    return ConfigTuiAction::Create;
                }
                self.open_edit();
            }
            KeyCode::Char('n') => return ConfigTuiAction::Create,
            KeyCode::Char('t') => return ConfigTuiAction::ToggleTarget,
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
            KeyCode::Esc if self.dirty => {
                self.confirm_discard = true;
                self.status = "Unsaved changes. Press q to discard, or Ctrl-S to save.".to_string();
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
        if entry.key.contains('<') {
            self.open_create();
            return;
        }
        if !entry.target_support.allows(self.selected_target) {
            self.status = format!(
                "{} can only be edited in {}.",
                entry.display_path,
                Self::entry_target_label(&entry)
            );
            return;
        }
        let current = self
            .target_value(&entry)
            .cloned()
            .or_else(|| self.effective_value(&entry))
            .or_else(|| entry.default_value.clone());
        let editor = editor_mode_for_entry(&entry, current.as_ref());
        let buffer = editor_buffer(&editor, current.as_ref());
        self.input_mode = ConfigInputMode::Edit {
            buffer,
            editor,
            error: None,
        };
        self.status = format!(
            "Editing {} in {}.",
            entry.display_path,
            self.selected_target_label()
        );
    }

    fn open_create(&mut self) {
        let entry = self.selected_entry().clone();
        if !entry.key.contains('<') {
            self.status = format!("{} is already concrete.", entry.display_path);
            return;
        }
        if !entry.target_support.allows(self.selected_target) {
            self.status = format!(
                "{} can only be created in {}.",
                entry.display_path,
                Self::entry_target_label(&entry)
            );
            return;
        }
        let mut value = entry
            .default_value
            .as_ref()
            .map(toml_literal)
            .unwrap_or_default();
        if value.is_empty() {
            value = match entry.kind {
                crate::app_config::ConfigValueKind::Object => "{}".to_string(),
                crate::app_config::ConfigValueKind::Array => "[]".to_string(),
                crate::app_config::ConfigValueKind::Boolean => "true".to_string(),
                _ => String::new(),
            };
        }
        self.input_mode = ConfigInputMode::CreateDynamic {
            name: String::new(),
            value,
            editing_value: false,
            error: None,
        };
        self.status = self.create_dynamic_status(&entry, "", false);
    }

    fn apply_buffered_edit(&mut self, buffer: &str) -> Result<(), String> {
        let entry = self.selected_entry().clone();
        let editor = self.active_editor_mode(&entry);
        let value = parse_editor_input(&entry, buffer, &editor)?;
        crate::app_config::plan_config_set_report_to(
            &self.paths,
            &entry.display_path,
            &value,
            self.selected_target,
            true,
        )
        .map_err(|error| error.to_string())?;

        let mut updated = self.target_toml().clone();
        let storage_segments = entry
            .storage_segments
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        crate::app_config::set_config_toml_value(&mut updated, &storage_segments, value)
            .map_err(|error| error.to_string())?;
        let rendered = toml::to_string_pretty(&updated).map_err(|error| error.to_string())?;
        vulcan_core::validate_vulcan_overrides_toml(&rendered)
            .map_err(|error| error.to_string())?;

        *self.target_toml_mut() = updated;
        self.refresh_from_working_state(Some(entry.display_path.clone()))?;
        self.input_mode = ConfigInputMode::Normal;
        if self.selected_target == crate::app_config::ConfigTarget::Shared
            && self.local_value(self.selected_entry()).is_some()
        {
            self.status = format!(
                "Updated {} in shared config. A local override still wins for the effective value.",
                entry.display_path
            );
        } else {
            self.status = format!(
                "Updated {} in the {} working copy.",
                entry.display_path,
                self.selected_target_label()
            );
        }
        Ok(())
    }

    fn apply_create_dynamic_entry(&mut self, name: &str, value: &str) -> Result<(), String> {
        let entry = self.selected_entry().clone();
        if !entry.key.contains('<') {
            return Err(format!("{} is already concrete.", entry.display_path));
        }
        let trimmed_name = name.trim();
        if trimmed_name.is_empty() {
            return Err("A concrete name is required.".to_string());
        }
        if trimmed_name.contains('.') || trimmed_name.contains('<') || trimmed_name.contains('>') {
            return Err("Names may not contain `.`, `<`, or `>`.".to_string());
        }

        let concrete_key = entry.key.replace("<name>", trimmed_name);
        let raw_value = if value.trim().is_empty() {
            match entry.kind {
                crate::app_config::ConfigValueKind::Object => "{}",
                crate::app_config::ConfigValueKind::Array => "[]",
                _ => return Err("A TOML value is required for this entry.".to_string()),
            }
        } else {
            value.trim()
        };
        let parsed_value = parse_entry_literal(&entry, raw_value)?;
        crate::app_config::plan_config_set_report_to(
            &self.paths,
            &concrete_key,
            &parsed_value,
            self.selected_target,
            true,
        )
        .map_err(|error| error.to_string())?;

        let mut updated = self.target_toml().clone();
        let storage_segments = storage_segments_for_key(&concrete_key);
        let storage_refs = storage_segments
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        crate::app_config::set_config_toml_value(&mut updated, &storage_refs, parsed_value)
            .map_err(|error| error.to_string())?;
        let rendered = toml::to_string_pretty(&updated).map_err(|error| error.to_string())?;
        vulcan_core::validate_vulcan_overrides_toml(&rendered)
            .map_err(|error| error.to_string())?;

        *self.target_toml_mut() = updated;
        self.refresh_from_working_state(Some(concrete_key.clone()))?;
        self.input_mode = ConfigInputMode::Normal;
        self.status = format!(
            "Created {} in {}.",
            concrete_key,
            self.selected_target_label()
        );
        Ok(())
    }

    fn unset_selected(&mut self) {
        let entry = self.selected_entry().clone();
        if entry.key.contains('<') {
            self.status = "Select a concrete config key before unsetting it.".to_string();
            return;
        }
        if !entry.target_support.allows(self.selected_target) {
            self.status = format!(
                "{} can only be unset in {}.",
                entry.display_path,
                Self::entry_target_label(&entry)
            );
            return;
        }
        let storage_segments = entry
            .storage_segments
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let removed = match crate::app_config::remove_config_toml_value(
            self.target_toml_mut(),
            &storage_segments,
        ) {
            Ok(removed) => removed,
            Err(error) => {
                self.status = error.to_string();
                return;
            }
        };
        if !removed {
            self.status = format!(
                "{} has no {} override to remove.",
                entry.display_path,
                self.selected_target_label()
            );
            return;
        }
        if let Err(error) = self.refresh_from_working_state(Some(entry.display_path.clone())) {
            self.status = error;
            return;
        }
        self.status = if self.selected_target == crate::app_config::ConfigTarget::Shared
            && self.local_value(self.selected_entry()).is_some()
        {
            format!(
                "Removed the shared override for {}. config.local.toml still overrides it.",
                entry.display_path
            )
        } else if self.selected_target == crate::app_config::ConfigTarget::Local {
            format!("Removed the local override for {}.", entry.display_path)
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
        self.local_toml = self.baseline_local_toml.clone();
        self.dirty = false;
        self.input_mode = ConfigInputMode::Normal;
        if let Err(error) = self.refresh_from_working_state(Some(selected)) {
            self.status = error;
            return;
        }
        self.status = "Reverted unsaved changes.".to_string();
    }

    fn save(&mut self, auto_commit: &AutoCommitPolicy, quiet: bool) {
        let had_gitignore = self.paths.gitignore_file().exists();
        let mut changed_files = BTreeSet::new();

        for target in [
            crate::app_config::ConfigTarget::Shared,
            crate::app_config::ConfigTarget::Local,
        ] {
            let value = match target {
                crate::app_config::ConfigTarget::Shared => &self.shared_toml,
                crate::app_config::ConfigTarget::Local => &self.local_toml,
            };
            let baseline = match target {
                crate::app_config::ConfigTarget::Shared => &self.baseline_shared_toml,
                crate::app_config::ConfigTarget::Local => &self.baseline_local_toml,
            };
            if value == baseline {
                continue;
            }

            let rendered = match toml::to_string_pretty(value) {
                Ok(rendered) => rendered,
                Err(error) => {
                    self.set_status(error.to_string());
                    return;
                }
            };
            let planned = match crate::app_config::plan_config_document_save_for_target(
                &self.paths,
                &rendered,
                target,
            ) {
                Ok(report) => report,
                Err(error) => {
                    self.set_status(error.to_string());
                    return;
                }
            };
            if !planned.updated {
                continue;
            }
            if let Err(error) =
                crate::app_config::apply_config_document_save(&self.paths, planned.clone())
            {
                self.set_status(error.to_string());
                return;
            }
            for path in
                crate::config_changed_files(&self.paths, &planned.config_path, had_gitignore)
            {
                changed_files.insert(path);
            }
        }

        if changed_files.is_empty() {
            self.dirty = false;
            self.confirm_discard = false;
            self.status = "No config changes to save.".to_string();
            return;
        }

        let commit_result = auto_commit.commit(
            &self.paths,
            "config-edit",
            &changed_files.into_iter().collect::<Vec<_>>(),
            None,
            quiet,
        );

        match Self::load(self.paths.clone()) {
            Ok(mut reloaded) => {
                reloaded.status = if let Err(error) = commit_result {
                    format!("Saved config changes, but auto-commit failed: {error}")
                } else {
                    "Saved config changes.".to_string()
                };
                *self = reloaded;
            }
            Err(error) => {
                self.baseline_shared_toml = self.shared_toml.clone();
                self.baseline_local_toml = self.local_toml.clone();
                self.dirty = self.shared_toml != self.baseline_shared_toml
                    || self.local_toml != self.baseline_local_toml;
                self.confirm_discard = false;
                self.status = if let Err(commit_error) = commit_result {
                    format!(
                        "Saved config changes, but reload failed ({error}) and auto-commit failed: {commit_error}"
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

    fn selected_target_label(&self) -> &'static str {
        match self.selected_target {
            crate::app_config::ConfigTarget::Shared => "shared",
            crate::app_config::ConfigTarget::Local => "local",
        }
    }

    fn target_toml(&self) -> &TomlValue {
        match self.selected_target {
            crate::app_config::ConfigTarget::Shared => &self.shared_toml,
            crate::app_config::ConfigTarget::Local => &self.local_toml,
        }
    }

    fn target_toml_mut(&mut self) -> &mut TomlValue {
        match self.selected_target {
            crate::app_config::ConfigTarget::Shared => &mut self.shared_toml,
            crate::app_config::ConfigTarget::Local => &mut self.local_toml,
        }
    }

    fn shared_value(&self, entry: &ConfigEntry) -> Option<&TomlValue> {
        if entry.key.contains('<') {
            return None;
        }
        get_toml_value(&self.shared_toml, &entry.storage_segments)
    }

    fn local_value(&self, entry: &ConfigEntry) -> Option<&TomlValue> {
        if entry.key.contains('<') {
            return None;
        }
        get_toml_value(&self.local_toml, &entry.storage_segments)
    }

    fn effective_value(&self, entry: &ConfigEntry) -> Option<TomlValue> {
        if entry.key.contains('<') {
            return None;
        }
        get_toml_value(&self.effective_toml, &entry.display_segments).cloned()
    }

    fn target_value(&self, entry: &ConfigEntry) -> Option<&TomlValue> {
        match self.selected_target {
            crate::app_config::ConfigTarget::Shared => self.shared_value(entry),
            crate::app_config::ConfigTarget::Local => self.local_value(entry),
        }
    }

    fn display_summary(&self, entry: &ConfigEntry) -> String {
        if entry.key.contains('<') {
            return "create...".to_string();
        }
        self.target_value(entry)
            .cloned()
            .or_else(|| self.effective_value(entry))
            .or_else(|| entry.default_value.clone())
            .map_or_else(String::new, |value| summarize_toml_value(&value))
    }

    fn mode_help_line(&self) -> String {
        match &self.input_mode {
            ConfigInputMode::Normal => {
                if self.confirm_discard {
                    "Discard is armed until you press another key.".to_string()
                } else {
                    "Browse categories on the left, settings in the middle, and use `t` to switch the active target."
                        .to_string()
                }
            }
            ConfigInputMode::Edit { editor, .. } => match editor {
                ValueEditorMode::RawToml => {
                    "Editing one TOML literal in memory. Save writes any changed shared and local config files."
                        .to_string()
                }
                ValueEditorMode::Enum { .. } => {
                    "Pick from the allowed values with arrows or j/k, then press Enter to apply."
                        .to_string()
                }
                ValueEditorMode::StringList => {
                    "Edit a comma-separated string list. Save writes any changed shared and local config files."
                        .to_string()
                }
                ValueEditorMode::ScalarMap => {
                    "Edit comma-separated key=value pairs for a simple scalar map."
                        .to_string()
                }
            },
            ConfigInputMode::CreateDynamic { editing_value, .. } => {
                if *editing_value {
                    "Creating a concrete entry. The popup shows the exact key that will be written; Enter applies the value, Tab returns to the name field."
                        .to_string()
                } else {
                    "Creating a concrete entry. Type the name to replace `<name>`, then press Enter to move to the value field."
                        .to_string()
                }
            }
        }
    }

    fn create_dynamic_status(
        &self,
        entry: &ConfigEntry,
        name: &str,
        editing_value: bool,
    ) -> String {
        let preview = preview_dynamic_key(&entry.display_path, name);
        if editing_value {
            format!(
                "Creating {} in {}. Enter the TOML value to write, then press Enter again to apply.",
                preview,
                self.selected_target_label()
            )
        } else {
            format!(
                "Creating {} in {}. Type the concrete name, then press Enter to move to the value field.",
                preview,
                self.selected_target_label()
            )
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

    fn refresh_from_working_state(&mut self, selected_path: Option<String>) -> Result<(), String> {
        let (effective_toml, diagnostics, categories) =
            rebuild_schema_state(&self.paths, &self.shared_toml, &self.local_toml)?;
        self.effective_toml = effective_toml;
        self.diagnostics = diagnostics;
        self.categories = categories;
        self.dirty = self.shared_toml != self.baseline_shared_toml
            || self.local_toml != self.baseline_local_toml;
        if self.categories.is_empty() {
            self.selected_category = 0;
            self.selected_entry = 0;
            return Ok(());
        }

        if let Some(selected_path) = selected_path {
            if let Some((category_index, entry_index)) = self.find_entry(&selected_path) {
                self.selected_category = category_index;
                self.selected_entry = entry_index;
                return Ok(());
            }
        }

        self.selected_category = self
            .selected_category
            .min(self.categories.len().saturating_sub(1));
        self.selected_entry = self
            .selected_entry
            .min(self.selected_category().entries.len().saturating_sub(1));
        Ok(())
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

    fn toggle_target(&mut self) {
        let entry = self.selected_entry().clone();
        match entry.target_support {
            crate::app_config::ConfigTargetSupport::SharedOnly => {
                self.selected_target = crate::app_config::ConfigTarget::Shared;
                self.status = format!("{} is shared-only.", entry.display_path);
            }
            crate::app_config::ConfigTargetSupport::LocalOnly => {
                self.selected_target = crate::app_config::ConfigTarget::Local;
                self.status = format!("{} is local-only.", entry.display_path);
            }
            crate::app_config::ConfigTargetSupport::SharedAndLocal => {
                self.selected_target = match self.selected_target {
                    crate::app_config::ConfigTarget::Shared => {
                        crate::app_config::ConfigTarget::Local
                    }
                    crate::app_config::ConfigTarget::Local => {
                        crate::app_config::ConfigTarget::Shared
                    }
                };
                self.status = format!("Active target: {}.", self.selected_target_label());
            }
        }
    }

    fn entry_kind_label(entry: &ConfigEntry) -> &'static str {
        match entry.kind {
            crate::app_config::ConfigValueKind::String => "string",
            crate::app_config::ConfigValueKind::Integer => "integer",
            crate::app_config::ConfigValueKind::Float => "float",
            crate::app_config::ConfigValueKind::Boolean => "boolean",
            crate::app_config::ConfigValueKind::Array => "array",
            crate::app_config::ConfigValueKind::Object => "object",
            crate::app_config::ConfigValueKind::Enum => "enum",
            crate::app_config::ConfigValueKind::Flexible => "flexible",
        }
    }

    fn entry_target_label(entry: &ConfigEntry) -> &'static str {
        match entry.target_support {
            crate::app_config::ConfigTargetSupport::SharedOnly => "shared",
            crate::app_config::ConfigTargetSupport::LocalOnly => "local",
            crate::app_config::ConfigTargetSupport::SharedAndLocal => "shared|local",
        }
    }

    fn value_source_label(entry: &ConfigEntry) -> &'static str {
        match entry.value_source {
            crate::app_config::ConfigValueSource::Default => "default",
            crate::app_config::ConfigValueSource::ObsidianImport => "obsidian import",
            crate::app_config::ConfigValueSource::SharedOverride => "shared override",
            crate::app_config::ConfigValueSource::LocalOverride => "local override",
            crate::app_config::ConfigValueSource::Unset => "unset",
        }
    }

    fn entry_source_badge(entry: &ConfigEntry) -> (&'static str, Color) {
        match entry.value_source {
            crate::app_config::ConfigValueSource::Default => ("default", Color::Blue),
            crate::app_config::ConfigValueSource::ObsidianImport => ("import", Color::Magenta),
            crate::app_config::ConfigValueSource::SharedOverride => ("shared", Color::Green),
            crate::app_config::ConfigValueSource::LocalOverride => ("local", Color::Yellow),
            crate::app_config::ConfigValueSource::Unset => ("unset", Color::DarkGray),
        }
    }

    fn overlay_title(&self, entry: &ConfigEntry) -> &'static str {
        match self.input_mode {
            ConfigInputMode::Edit { ref editor, .. } => match editor {
                ValueEditorMode::RawToml => "Edit Value",
                ValueEditorMode::Enum { .. } => "Pick Value",
                ValueEditorMode::StringList => "Edit List",
                ValueEditorMode::ScalarMap => "Edit Map",
            },
            ConfigInputMode::CreateDynamic { .. } => {
                if entry.key.starts_with("export.profiles.") {
                    "Create Export Profile Entry"
                } else {
                    "Create Entry"
                }
            }
            ConfigInputMode::Normal => "Config Editor",
        }
    }

    #[allow(clippy::too_many_lines)]
    fn overlay_lines(&self, entry: &ConfigEntry) -> Vec<Line<'static>> {
        match &self.input_mode {
            ConfigInputMode::Edit {
                buffer,
                editor,
                error,
            } => match editor {
                ValueEditorMode::RawToml => {
                    let mut lines = vec![
                        Line::from(format!(
                            "Edit {} in {}",
                            entry.display_path,
                            self.selected_target_label()
                        )),
                        Line::from("Enter a TOML literal. Bare text is stored as a string."),
                        Line::from(""),
                        Line::from(format!("Value: {buffer}")),
                        Line::from(""),
                    ];
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
                    lines
                }
                ValueEditorMode::Enum { options, selected } => {
                    let mut lines = vec![
                        Line::from(format!(
                            "Choose {} in {}",
                            entry.display_path,
                            self.selected_target_label()
                        )),
                        Line::from("Use arrows or j/k to select one allowed value."),
                        Line::from(""),
                    ];
                    for (index, option) in options.iter().enumerate() {
                        lines.push(Line::from(format!(
                            "{}{}",
                            if index == *selected { "> " } else { "  " },
                            option
                        )));
                    }
                    lines.push(Line::from(""));
                    if let Some(error) = error {
                        lines.push(Line::from(vec![Span::styled(
                            error.clone(),
                            Style::default().fg(Color::Red),
                        )]));
                    } else {
                        lines.push(Line::from("Press Enter to apply, Esc to cancel."));
                    }
                    lines
                }
                ValueEditorMode::StringList => {
                    let mut lines = vec![
                        Line::from(format!(
                            "Edit {} in {}",
                            entry.display_path,
                            self.selected_target_label()
                        )),
                        Line::from(
                            "Enter a comma-separated list of strings. Quotes are optional unless one item contains commas.",
                        ),
                        Line::from(""),
                        Line::from(format!("Items: {buffer}")),
                        Line::from(""),
                    ];
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
                    lines
                }
                ValueEditorMode::ScalarMap => {
                    let mut lines = vec![
                        Line::from(format!(
                            "Edit {} in {}",
                            entry.display_path,
                            self.selected_target_label()
                        )),
                        Line::from(
                            "Enter comma-separated key=value pairs. Values may be bare strings, numbers, booleans, or quoted strings.",
                        ),
                        Line::from(""),
                        Line::from(format!("Pairs: {buffer}")),
                        Line::from(""),
                    ];
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
                    lines
                }
            },
            ConfigInputMode::CreateDynamic {
                name,
                value,
                editing_value,
                error,
            } => {
                let preview = preview_dynamic_key(&entry.display_path, name);
                let mut lines = vec![
                    Line::from(format!(
                        "Create {} in {}",
                        entry.display_path,
                        self.selected_target_label()
                    )),
                    Line::from(if *editing_value {
                        "Step 2 of 2: enter the value to write."
                    } else {
                        "Step 1 of 2: choose the concrete name."
                    }),
                    Line::from(format!("Will create: {preview}")),
                    Line::from(""),
                    Line::from(format!(
                        "{}Name: {}",
                        if *editing_value { "  " } else { "> " },
                        name
                    )),
                    Line::from(format!(
                        "{}Value: {}",
                        if *editing_value { "> " } else { "  " },
                        value
                    )),
                    Line::from(""),
                ];
                if let Some(error) = error {
                    lines.push(Line::from(vec![Span::styled(
                        error.clone(),
                        Style::default().fg(Color::Red),
                    )]));
                } else if entry.key.starts_with("export.profiles.") {
                    lines.push(Line::from(
                        "Export profiles are discoverable here, but `vulcan export profile ...` remains the preferred write path.",
                    ));
                } else {
                    lines.push(Line::from(
                        "Press Enter to advance/apply, Tab to switch fields, Esc to cancel.",
                    ));
                }
                lines
            }
            ConfigInputMode::Normal => vec![Line::from(String::new())],
        }
    }

    fn active_editor_mode(&self, entry: &ConfigEntry) -> ValueEditorMode {
        match &self.input_mode {
            ConfigInputMode::Edit { editor, .. } => editor.clone(),
            ConfigInputMode::Normal | ConfigInputMode::CreateDynamic { .. } => {
                let current = self
                    .target_value(entry)
                    .cloned()
                    .or_else(|| self.effective_value(entry))
                    .or_else(|| entry.default_value.clone());
                editor_mode_for_entry(entry, current.as_ref())
            }
        }
    }
}

fn build_categories(entries: &[crate::app_config::ConfigListEntry]) -> Vec<ConfigCategory> {
    let mut grouped = BTreeMap::<String, ConfigCategory>::new();
    for entry in entries {
        let display_segments = parse_key_segments(&entry.key);
        let storage_segments = parse_key_segments(&entry.storage_key);
        let default_value = entry
            .default_value
            .as_ref()
            .and_then(|value: &serde_json::Value| TomlValue::try_from(value.clone()).ok());
        let category = grouped
            .entry(entry.section.clone())
            .or_insert_with(|| ConfigCategory {
                key: entry.section.clone(),
                title: entry.section_title.clone(),
                description: entry.section_description.clone(),
                entries: Vec::new(),
            });
        category.entries.push(ConfigEntry {
            key: entry.key.clone(),
            storage_key: entry.storage_key.clone(),
            display_path: entry.key.clone(),
            display_segments,
            storage_segments,
            label: entry_label(entry),
            description: entry.description.clone(),
            kind: entry.kind.clone(),
            enum_values: entry.enum_values.clone(),
            target_support: entry.target_support,
            value_source: entry.value_source,
            default_value,
            default_display: entry.default_display.clone(),
            examples: entry.examples.clone(),
            preferred_command: entry.preferred_command.clone(),
        });
    }

    let mut categories = grouped.into_values().collect::<Vec<_>>();
    categories.sort_by(|left, right| {
        category_order(&left.key)
            .cmp(&category_order(&right.key))
            .then_with(|| left.key.cmp(&right.key))
    });
    for category in &mut categories {
        category
            .entries
            .sort_by(|left, right| left.display_path.cmp(&right.display_path));
    }
    categories
}

fn parse_key_segments(key: &str) -> Vec<String> {
    key.split('.')
        .filter(|segment| !segment.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn entry_label(entry: &crate::app_config::ConfigListEntry) -> String {
    if entry.key.contains("<name>") {
        return match entry.key.as_str() {
            "aliases.<name>" => "new alias...".to_string(),
            "permissions.profiles.<name>" => "new profile...".to_string(),
            "plugins.<name>" => "new plugin...".to_string(),
            "export.profiles.<name>" => "new export profile...".to_string(),
            "site.profiles.<name>" => "new site profile...".to_string(),
            _ => entry.key.clone(),
        };
    }
    let display_segments = parse_key_segments(&entry.key);
    if entry.section == "general" || display_segments.len() == 1 {
        entry.key.clone()
    } else {
        display_segments[1..].join(".")
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
        "export" => 14,
        "site" => 15,
        _ => 50,
    }
}

fn rebuild_schema_state(
    paths: &VaultPaths,
    shared_toml: &TomlValue,
    local_toml: &TomlValue,
) -> Result<(TomlValue, Vec<ConfigDiagnostic>, Vec<ConfigCategory>), String> {
    let display = crate::app_config::build_config_show_report_from_overrides(
        paths,
        shared_toml,
        local_toml,
        None,
        None,
    )
    .map_err(|error| error.to_string())?;
    let list = crate::app_config::build_config_list_report_from_overrides(
        paths,
        shared_toml,
        local_toml,
        None,
    )
    .map_err(|error| error.to_string())?;
    Ok((
        display.rendered_toml,
        display.diagnostics,
        build_categories(&list.entries),
    ))
}

fn storage_segments_for_key(key: &str) -> Vec<String> {
    display_path_to_storage_path(&parse_key_segments(key))
}

fn preview_dynamic_key(template_key: &str, name: &str) -> String {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        template_key.to_string()
    } else {
        template_key.replace("<name>", trimmed)
    }
}

fn get_toml_value<'a>(value: &'a TomlValue, path: &[String]) -> Option<&'a TomlValue> {
    let mut current = value;
    for segment in path {
        current = current.as_table()?.get(segment)?;
    }
    Some(current)
}

fn editor_mode_for_entry(entry: &ConfigEntry, current: Option<&TomlValue>) -> ValueEditorMode {
    if !entry.enum_values.is_empty() {
        let selected = current
            .and_then(|value| value.as_str())
            .and_then(|value| entry.enum_values.iter().position(|option| option == value))
            .unwrap_or(0);
        return ValueEditorMode::Enum {
            options: entry.enum_values.clone(),
            selected,
        };
    }

    if supports_string_list_editor(entry, current) {
        return ValueEditorMode::StringList;
    }

    if supports_scalar_map_editor(entry, current) {
        return ValueEditorMode::ScalarMap;
    }

    ValueEditorMode::RawToml
}

fn supports_string_list_editor(entry: &ConfigEntry, current: Option<&TomlValue>) -> bool {
    match current {
        Some(TomlValue::Array(values)) if !values.is_empty() => {
            values.iter().all(TomlValue::is_str)
        }
        Some(TomlValue::Array(_)) | None => {
            matches!(
                entry.display_path.as_str(),
                "templates.web_allowlist" | "git.exclude"
            ) || (entry.display_path.starts_with("plugins.")
                && entry.display_path.ends_with(".events"))
        }
        _ => false,
    }
}

fn supports_scalar_map_editor(entry: &ConfigEntry, current: Option<&TomlValue>) -> bool {
    match current {
        Some(TomlValue::Table(values)) if !values.is_empty() => values
            .values()
            .all(|value| !matches!(value, TomlValue::Array(_) | TomlValue::Table(_))),
        Some(TomlValue::Table(_)) | None => matches!(
            entry.display_path.as_str(),
            "quickadd.global_variables" | "kanban.table_sizing"
        ),
        _ => false,
    }
}

fn editor_buffer(editor: &ValueEditorMode, current: Option<&TomlValue>) -> String {
    match editor {
        ValueEditorMode::RawToml => current.map_or_else(String::new, toml_literal),
        ValueEditorMode::Enum { options, selected } => options
            .get(*selected)
            .cloned()
            .or_else(|| {
                current
                    .and_then(|value| value.as_str())
                    .map(ToOwned::to_owned)
            })
            .unwrap_or_default(),
        ValueEditorMode::StringList => current
            .and_then(|value| value.as_array())
            .map(|values| {
                values
                    .iter()
                    .map(|value| {
                        value
                            .as_str()
                            .map_or_else(|| toml_literal(value), ToOwned::to_owned)
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default(),
        ValueEditorMode::ScalarMap => current
            .and_then(|value| value.as_table())
            .map(|table| {
                table
                    .iter()
                    .map(|(key, value)| format!("{key}={}", toml_literal(value)))
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default(),
    }
}

fn parse_editor_input(
    entry: &ConfigEntry,
    raw: &str,
    editor: &ValueEditorMode,
) -> Result<TomlValue, String> {
    match editor {
        ValueEditorMode::RawToml => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return Err(format!(
                    "Empty values are not allowed. Use `u` to unset the {} override.",
                    entry_target_scope_label(entry)
                ));
            }
            parse_entry_literal(entry, trimmed)
        }
        ValueEditorMode::Enum { options, .. } => {
            let trimmed = raw.trim();
            if let Some(value) = options.iter().find(|value| value.as_str() == trimmed) {
                Ok(TomlValue::String(value.clone()))
            } else {
                Err(format!("Expected one of: {}.", options.join(", ")))
            }
        }
        ValueEditorMode::StringList => parse_string_list_input(raw),
        ValueEditorMode::ScalarMap => parse_scalar_map_input(raw),
    }
}

fn parse_entry_literal(entry: &ConfigEntry, raw: &str) -> Result<TomlValue, String> {
    match parse_toml_literal(raw) {
        Ok(value) => Ok(value),
        Err(_error)
            if entry.kind == crate::app_config::ConfigValueKind::String
                && looks_like_unstructured_string(raw) =>
        {
            Ok(TomlValue::String(raw.to_string()))
        }
        Err(error) => Err(error),
    }
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

fn parse_string_list_input(raw: &str) -> Result<TomlValue, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(TomlValue::Array(Vec::new()));
    }

    let values = split_unquoted_segments(trimmed, ',')?
        .into_iter()
        .filter_map(|segment| {
            let item = segment.trim();
            if item.is_empty() {
                None
            } else {
                Some(parse_string_list_item(item))
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(TomlValue::Array(values))
}

fn parse_string_list_item(raw: &str) -> Result<TomlValue, String> {
    if is_quoted_text(raw) {
        match parse_toml_literal(raw)? {
            TomlValue::String(text) => Ok(TomlValue::String(text)),
            _ => Err("Quoted list items must parse as strings.".to_string()),
        }
    } else {
        Ok(TomlValue::String(raw.to_string()))
    }
}

fn parse_scalar_map_input(raw: &str) -> Result<TomlValue, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(TomlValue::Table(toml::map::Map::new()));
    }

    let mut table = toml::map::Map::new();
    for segment in split_unquoted_segments(trimmed, ',')? {
        let pair = segment.trim();
        if pair.is_empty() {
            continue;
        }
        let Some((raw_key, raw_value)) = split_unquoted_pair(pair, '=') else {
            return Err(format!(
                "Expected `key=value` pairs, but `{pair}` is missing `=`."
            ));
        };
        let key = raw_key.trim();
        if !is_valid_inline_map_key(key) {
            return Err(format!(
                "Invalid key `{key}`. Use bare keys containing letters, digits, `_`, or `-`."
            ));
        }
        let value = parse_toml_literal(raw_value.trim())?;
        if matches!(value, TomlValue::Array(_) | TomlValue::Table(_)) {
            return Err(format!(
                "Map editor values must be scalar; `{key}` used a nested structure."
            ));
        }
        table.insert(key.to_string(), value);
    }
    Ok(TomlValue::Table(table))
}

fn split_unquoted_pair(raw: &str, separator: char) -> Option<(&str, &str)> {
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;

    for (index, character) in raw.char_indices() {
        if in_double {
            if escaped {
                escaped = false;
                continue;
            }
            match character {
                '\\' => escaped = true,
                '"' => in_double = false,
                _ => {}
            }
            continue;
        }
        if in_single {
            if character == '\'' {
                in_single = false;
            }
            continue;
        }
        match character {
            '"' => in_double = true,
            '\'' => in_single = true,
            value if value == separator => return Some((&raw[..index], &raw[index + 1..])),
            _ => {}
        }
    }

    None
}

fn split_unquoted_segments(raw: &str, separator: char) -> Result<Vec<String>, String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;

    for character in raw.chars() {
        if in_double {
            current.push(character);
            if escaped {
                escaped = false;
            } else {
                match character {
                    '\\' => escaped = true,
                    '"' => in_double = false,
                    _ => {}
                }
            }
            continue;
        }
        if in_single {
            current.push(character);
            if character == '\'' {
                in_single = false;
            }
            continue;
        }

        match character {
            '"' => {
                in_double = true;
                current.push(character);
            }
            '\'' => {
                in_single = true;
                current.push(character);
            }
            value if value == separator => {
                parts.push(current.trim().to_string());
                current.clear();
            }
            _ => current.push(character),
        }
    }

    if in_single || in_double {
        return Err("Unterminated quoted string.".to_string());
    }

    parts.push(current.trim().to_string());
    Ok(parts)
}

fn is_quoted_text(raw: &str) -> bool {
    (raw.starts_with('"') && raw.ends_with('"') && raw.len() >= 2)
        || (raw.starts_with('\'') && raw.ends_with('\'') && raw.len() >= 2)
}

fn is_valid_inline_map_key(key: &str) -> bool {
    !key.is_empty()
        && key
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
}

fn entry_target_scope_label(entry: &ConfigEntry) -> &'static str {
    match entry.target_support {
        crate::app_config::ConfigTargetSupport::SharedOnly => "shared",
        crate::app_config::ConfigTargetSupport::LocalOnly => "local",
        crate::app_config::ConfigTargetSupport::SharedAndLocal => "selected",
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

fn looks_like_unstructured_string(raw: &str) -> bool {
    !raw.chars()
        .any(|character| matches!(character, '{' | '}' | '[' | ']' | ',' | '\n' | '\r'))
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

#[cfg(test)]
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
    use ratatui::buffer::Buffer;
    use ratatui::Terminal;
    use tempfile::TempDir;

    fn sample_state() -> ConfigTuiState {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        std::fs::create_dir_all(vault_root.join(".vulcan")).expect("config dir should exist");
        std::fs::write(
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
        std::fs::write(
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

    fn render_text(state: &ConfigTuiState, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("terminal should build");
        terminal
            .draw(|frame| draw(frame, state))
            .expect("config tui should render");
        buffer_text(terminal.backend().buffer())
    }

    fn buffer_text(buffer: &Buffer) -> String {
        let area = *buffer.area();
        (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
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
    fn schema_driven_state_lists_dynamic_placeholders_when_absent() {
        let state = sample_state();

        assert!(state.find_entry("aliases.<name>").is_some());
        assert!(state.find_entry("plugins.<name>").is_some());
        assert!(state.find_entry("permissions.profiles.<name>").is_some());
        assert!(state.find_entry("site.profiles.<name>").is_some());
    }

    #[test]
    fn create_dynamic_alias_and_unset_round_trip_updates_working_copy() {
        let mut state = sample_state();
        let (category_index, entry_index) = state
            .find_entry("aliases.<name>")
            .expect("alias placeholder should exist");
        state.selected_category = category_index;
        state.selected_entry = entry_index;

        state
            .apply_create_dynamic_entry("ship", "query --where 'status = shipped'")
            .expect("dynamic alias should be created");
        let (category_index, entry_index) = state
            .find_entry("aliases.ship")
            .expect("concrete alias should exist after creation");
        state.selected_category = category_index;
        state.selected_entry = entry_index;

        let entry = state.selected_entry().clone();
        assert_eq!(
            state.shared_value(&entry),
            Some(&TomlValue::String(
                "query --where 'status = shipped'".to_string()
            ))
        );

        state.unset_selected();
        assert!(state.find_entry("aliases.ship").is_none());
    }

    #[test]
    fn create_dynamic_permission_profile_rebuilds_state_in_place() {
        let mut state = sample_state();
        let (category_index, entry_index) = state
            .find_entry("permissions.profiles.<name>")
            .expect("profile placeholder should exist");
        state.selected_category = category_index;
        state.selected_entry = entry_index;

        state
            .apply_create_dynamic_entry("agent", "{}")
            .expect("permission profile should be created");

        let (category_index, entry_index) = state
            .find_entry("permissions.profiles.agent")
            .expect("concrete profile should exist after creation");
        state.selected_category = category_index;
        state.selected_entry = entry_index;

        let entry = state.selected_entry().clone();
        assert!(matches!(
            state.shared_value(&entry),
            Some(TomlValue::Table(table)) if table.is_empty()
        ));
        assert!(state.find_entry("permissions.profiles.agent.git").is_some());
        assert!(state
            .find_entry("permissions.profiles.agent.read")
            .is_some());
        assert!(state.dirty);
    }

    #[test]
    fn create_dynamic_site_profile_rebuilds_state_in_place() {
        let mut state = sample_state();
        let (category_index, entry_index) = state
            .find_entry("site.profiles.<name>")
            .expect("site profile placeholder should exist");
        state.selected_category = category_index;
        state.selected_entry = entry_index;

        state
            .apply_create_dynamic_entry("public", "{}")
            .expect("site profile should be created");

        let (category_index, entry_index) = state
            .find_entry("site.profiles.public")
            .expect("concrete site profile should exist after creation");
        state.selected_category = category_index;
        state.selected_entry = entry_index;

        let entry = state.selected_entry().clone();
        assert!(matches!(
            state.shared_value(&entry),
            Some(TomlValue::Table(table)) if table.is_empty()
        ));
        assert!(state.find_entry("site.profiles.public.title").is_some());
        assert!(state
            .find_entry("site.profiles.public.link_policy")
            .is_some());
        assert!(state.dirty);
    }

    #[test]
    fn create_dynamic_overlay_is_rendered_and_previews_concrete_key() {
        let mut state = sample_state();
        let (category_index, entry_index) = state
            .find_entry("permissions.profiles.<name>")
            .expect("profile placeholder should exist");
        state.selected_category = category_index;
        state.selected_entry = entry_index;

        state.open_create();
        let initial = render_text(&state, 90, 24);
        assert!(initial.contains("Create Entry"));
        assert!(initial.contains("Step 1 of 2: choose the concrete name."));
        assert!(initial.contains("Will create: permissions.profiles.<name>"));
        assert!(initial.contains("> Name:"));

        for character in "agent".chars() {
            state.handle_key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
        }
        state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        let value_step = render_text(&state, 90, 24);
        assert!(value_step.contains("Step 2 of 2: enter the value to write."));
        assert!(value_step.contains("Will create: permissions.profiles.agent"));
        assert!(value_step.contains("> Value: {}"));
        assert!(state
            .status
            .contains("Creating permissions.profiles.agent in shared."));
    }

    #[test]
    fn toggled_local_target_edits_local_working_copy() {
        let mut state = sample_state();
        let (category_index, entry_index) = state
            .find_entry("periodic.daily.folder")
            .expect("periodic entry should exist");
        state.selected_category = category_index;
        state.selected_entry = entry_index;

        state.toggle_target();
        assert_eq!(state.selected_target_label(), "local");
        state
            .apply_buffered_edit("\"Journal/Local\"")
            .expect("local edit should apply");

        let entry = state.selected_entry().clone();
        assert_eq!(
            state.local_value(&entry),
            Some(&TomlValue::String("Journal/Local".to_string()))
        );
        assert_eq!(
            state.shared_value(&entry),
            Some(&TomlValue::String("Journal/Daily".to_string()))
        );
    }

    #[test]
    fn enum_editor_cycles_allowed_values_without_raw_toml() {
        let mut state = sample_state();
        let (category_index, entry_index) = state
            .find_entry("web.search.backend")
            .expect("web backend entry should exist");
        state.selected_category = category_index;
        state.selected_entry = entry_index;

        state.open_edit();
        match &state.input_mode {
            ConfigInputMode::Edit {
                editor: ValueEditorMode::Enum { selected, .. },
                ..
            } => assert_eq!(*selected, 1),
            mode => panic!("expected enum editor, got {mode:?}"),
        }

        state.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        state.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        let entry = state.selected_entry().clone();
        assert_eq!(
            state.shared_value(&entry),
            Some(&TomlValue::String("kagi".to_string()))
        );
    }

    #[test]
    fn string_list_editor_parses_comma_separated_items() {
        let mut state = sample_state();
        let (category_index, entry_index) = state
            .find_entry("templates.web_allowlist")
            .expect("web allowlist entry should exist");
        state.selected_category = category_index;
        state.selected_entry = entry_index;

        state.open_edit();
        match &state.input_mode {
            ConfigInputMode::Edit {
                editor: ValueEditorMode::StringList,
                ..
            } => {}
            mode => panic!("expected string list editor, got {mode:?}"),
        }

        state
            .apply_buffered_edit("docs.example.com, api.example.com")
            .expect("string list edit should apply");

        let entry = state.selected_entry().clone();
        assert_eq!(
            state.shared_value(&entry),
            Some(&TomlValue::Array(vec![
                TomlValue::String("docs.example.com".to_string()),
                TomlValue::String("api.example.com".to_string()),
            ]))
        );
    }

    #[test]
    fn scalar_map_editor_parses_key_value_pairs() {
        let mut state = sample_state();
        let (category_index, entry_index) = state
            .find_entry("quickadd.global_variables")
            .expect("quickadd global variables entry should exist");
        state.selected_category = category_index;
        state.selected_entry = entry_index;

        state.open_edit();
        match &state.input_mode {
            ConfigInputMode::Edit {
                editor: ValueEditorMode::ScalarMap,
                ..
            } => {}
            mode => panic!("expected scalar map editor, got {mode:?}"),
        }

        state
            .apply_buffered_edit("team=\"ops\", env=prod")
            .expect("scalar map edit should apply");

        let entry = state.selected_entry().clone();
        assert_eq!(
            state.shared_value(&entry),
            Some(&TomlValue::Table(toml::map::Map::from_iter([
                ("team".to_string(), TomlValue::String("ops".to_string())),
                ("env".to_string(), TomlValue::String("prod".to_string())),
            ])))
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
