use crate::bases_tui;
use crate::browse_preview::{PreviewWorker, RenderedNote};
use crate::calendar_view::CalendarViewState;
use crate::commands::browse::BrowseAuthority;
use crate::commit::AutoCommitPolicy;
use crate::editor::{open_in_editor, with_terminal_suspended};
use crate::note_picker::{
    handle_picker_key, NotePickerState, PickerAction, Redraw, TerminalRestore, Viewport,
};
use crate::preview_images::{
    halfblock_lines, image_cells, is_previewable_image, load_preview_image, PREVIEW_UPSCALE,
};
use crate::terminal_markdown::{
    laid_out_links, render_styled_lines, to_ratatui_lines, LaidOutLink, SpanLink,
    TerminalMarkdownOptions,
};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::protocol::StatefulProtocol;
use ratatui_image::{Resize, StatefulImage};
use serde_json::Value;
use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, SystemTime};
use vulcan_app::browse::{
    doctor_vault, evaluate_base_file_with_guard, git_log, is_git_repo, list_kanban_boards,
    list_note_identities, list_tagged_note_identities, list_tags, load_kanban_board,
    move_note_with_profile, query_notes, refresh_browse_cache,
};
use vulcan_app::scan::refresh_cache_incrementally;
use vulcan_core::paths::{normalize_relative_input_path, RelativePathOptions};
#[cfg(test)]
use vulcan_core::scan_vault;
use vulcan_core::search::{SearchMode, SearchSort};
use vulcan_core::PermissionFilter;
#[cfg(test)]
use vulcan_core::ScanMode;
use vulcan_core::Verbosity;
use vulcan_core::{
    AutoScanMode, BacklinkRecord, DoctorDiagnosticIssue, DoctorLinkIssue, GitLogEntry,
    KanbanBoardRecord, NamedCount, NoteIdentity, NoteQuery, OutgoingLinkRecord, ResolutionStatus,
    ScanSummary, SearchHit, SearchQuery, VaultPaths,
};

const FULL_TEXT_LIMIT: usize = 200;
const FULL_TEXT_CONTEXT_SIZE: usize = 18;

pub(crate) fn run_browse_tui(
    paths: &VaultPaths,
    refresh_mode: AutoScanMode,
    no_commit: bool,
    authority: BrowseAuthority,
) -> Result<(), io::Error> {
    // Without index authority the session reads the cache as it is.
    let refresh_mode = if authority.can_index() {
        refresh_mode
    } else {
        AutoScanMode::Off
    };
    let background_refresh = prepare_browse_refresh(paths, refresh_mode)?;

    let notes = load_notes(paths, &authority).map_err(io::Error::other)?;
    let mut state = BrowseState::new_with_authority(paths.clone(), notes, authority)
        .map_err(io::Error::other)?;
    state.background_refresh = background_refresh;
    let auto_commit = AutoCommitPolicy::for_mutation(paths, no_commit);
    if let Some(message) = auto_commit.warning() {
        state.set_status(format!("Warning: {message}."));
    }

    enable_raw_mode()?;
    let _restore = TerminalRestore;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.hide_cursor()?;
    // Pick how images are drawn once, before reading events.
    state.image_mode = detect_image_mode();
    state.rendered_preview.layout.replace(None);

    let result = run_event_loop(&mut terminal, &mut state, &auto_commit);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

/// Refresh the index after an edit, when the profile may index.
fn refresh_after_change(paths: &VaultPaths, can_index: bool) -> Result<(), String> {
    if !can_index {
        return Ok(());
    }
    refresh_cache_incrementally(paths)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn load_notes(
    paths: &VaultPaths,
    authority: &BrowseAuthority,
) -> Result<Vec<NoteIdentity>, String> {
    let mut notes = list_note_identities(paths).map_err(|error| error.to_string())?;
    notes.retain(|note| authority.can_read(&note.path));
    Ok(notes)
}

#[allow(clippy::too_many_lines)]
fn run_event_loop(
    terminal: &mut Terminal<ratatui::backend::CrosstermBackend<io::Stdout>>,
    state: &mut BrowseState,
    auto_commit: &AutoCommitPolicy,
) -> Result<(), io::Error> {
    let mut redraw = Redraw::default();
    loop {
        if state.poll_background_refresh() {
            redraw.invalidate();
        }
        if state.poll_rendered_preview() {
            redraw.invalidate();
        }
        if redraw.take() {
            terminal.draw(|frame| draw(frame, state))?;
        }

        // Only poll while a finite refresh is outstanding. Once it completes,
        // read blocks without idle wakeups, just like the picker and Bases.
        // The same holds while a rendered preview is being evaluated.
        let poll_interval = if state.rendered_preview.pending {
            Some(Duration::from_millis(50))
        } else {
            state
                .background_refresh
                .is_some()
                .then(|| Duration::from_millis(200))
        };
        if let Some(interval) = poll_interval {
            if !event::poll(interval)? {
                continue;
            }
        }

        let event = event::read()?;
        redraw.event(&event);
        if let Event::Key(key) = event {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match state.handle_key(key) {
                BrowseAction::Continue => {}
                BrowseAction::Quit => break,
                BrowseAction::Edit(path) => {
                    if let Err(error) = state.authority.check_edit(&path) {
                        state.set_status(error);
                        continue;
                    }
                    let absolute = state.paths.vault_root().join(&path);
                    let paths = state.paths.clone();
                    let can_index = state.authority.can_index();
                    let edit_result = with_terminal_suspended(terminal, || {
                        open_in_editor(&absolute)?;
                        refresh_after_change(&paths, can_index)
                    });
                    match edit_result {
                        Ok(()) => {
                            if let Err(error) = state.reload_after_edit() {
                                state.set_status(error);
                            } else if let Err(error) =
                                state.commit(auto_commit, std::slice::from_ref(&path))
                            {
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
                    let can_index = state.authority.can_index();
                    let guard = state.authority.guard().clone();
                    let open_result = with_terminal_suspended(terminal, || {
                        let report = evaluate_base_file_with_guard(&paths, &path, &guard)
                            .map_err(|error| error.to_string())?;
                        bases_tui::run_bases_tui(&paths, &path, &report, Some(guard))
                            .map_err(|error| error.to_string())?;
                        refresh_after_change(&paths, can_index)
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
                    if let Err(error) = state.authority.check_create(&relative_path) {
                        state.set_status(error);
                        continue;
                    }
                    let paths = state.paths.clone();
                    let can_index = state.authority.can_index();
                    let create_result = with_terminal_suspended(terminal, || {
                        let absolute = paths.vault_root().join(&relative_path);
                        if let Some(parent) = absolute.parent() {
                            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
                        }
                        if !absolute.exists() {
                            fs::write(&absolute, "").map_err(|error| error.to_string())?;
                        }
                        open_in_editor(&absolute)?;
                        refresh_after_change(&paths, can_index)
                    });
                    match create_result {
                        Ok(()) => {
                            state.clear_new_note_prompt();
                            if let Err(error) = state.reload_after_new_note(&relative_path) {
                                state.set_status(error);
                            } else if let Err(error) =
                                state.commit(auto_commit, std::slice::from_ref(&relative_path))
                            {
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
                } => match state
                    .authority
                    .check_move(&source_path, &destination)
                    .and_then(|()| {
                        move_note_with_profile(
                            &state.paths,
                            &source_path,
                            &destination,
                            false,
                            state.authority.profile(),
                        )
                        .map_err(|error| error.to_string())
                    }) {
                    Ok(summary) => {
                        state.clear_move_prompt();
                        if let Err(error) = state.reload_after_move(&summary.destination_path) {
                            state.set_status(error);
                        } else if let Err(error) = state.commit(
                            auto_commit,
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
                    Err(error) => state.set_status(error),
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
        refresh_browse_cache(paths).map_err(io::Error::other)?;
        return Ok(None);
    }

    match refresh_mode {
        AutoScanMode::Off => Ok(None),
        AutoScanMode::Blocking => {
            refresh_browse_cache(paths).map_err(io::Error::other)?;
            Ok(None)
        }
        AutoScanMode::Background => Ok(Some(BackgroundRefreshState::spawn(paths.clone()))),
    }
}

/// The most rows an image takes in the preview before the pane's height is
/// known.
const MAX_IMAGE_ROWS: u16 = 30;
/// Half-blocks show two pixels per cell, as a font twice as tall as wide.
const HALFBLOCK_FONT_SIZE: (u16, u16) = (10, 20);

/// Whether a laid-out line holds nothing but images (and blanks), so a shown
/// image can take its place.
fn holds_only_images(line: &[crate::terminal_markdown::StyledSpan]) -> bool {
    line.iter()
        .all(|span| matches!(span.link, Some(SpanLink::Image(_))) || span.text.trim().is_empty())
}

/// How browse draws preview images.
#[derive(Debug)]
enum ImageMode {
    /// A terminal graphics protocol, at full resolution.
    Graphics { picker: Picker, label: &'static str },
    /// Half-block characters, in any true-color terminal.
    HalfBlocks,
    /// Not at all; placeholders only.
    Off,
}

impl ImageMode {
    fn label(&self) -> &'static str {
        match self {
            Self::Graphics { label, .. } => label,
            Self::HalfBlocks => "half-blocks",
            Self::Off => "off",
        }
    }

    fn graphics(picker: Picker) -> Self {
        let label = match picker.protocol_type() {
            ProtocolType::Kitty => "kitty",
            ProtocolType::Sixel => "sixel",
            ProtocolType::Iterm2 => "iterm2",
            ProtocolType::Halfblocks => return Self::HalfBlocks,
        };
        Self::Graphics { picker, label }
    }
}

/// Choose how to draw preview images. `VULCAN_IMAGE_PROTOCOL` (`kitty`,
/// `sixel`, `iterm2`, `halfblocks`, or `off`) forces a mode. Otherwise the
/// terminal is asked for a graphics protocol, except inside tmux, which
/// usually does not pass graphics through and may not answer the query: a
/// query left waiting would swallow the next keystroke.
fn detect_image_mode() -> ImageMode {
    let forced = std::env::var("VULCAN_IMAGE_PROTOCOL")
        .ok()
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty());
    let forced_protocol = match forced.as_deref() {
        Some("off" | "none") => return ImageMode::Off,
        Some("halfblocks" | "half-blocks") => return ImageMode::HalfBlocks,
        Some("kitty") => Some(ProtocolType::Kitty),
        Some("sixel") => Some(ProtocolType::Sixel),
        Some("iterm2" | "iterm") => Some(ProtocolType::Iterm2),
        _ => None,
    };
    if forced_protocol.is_none() && std::env::var_os("TMUX").is_some() {
        return ImageMode::HalfBlocks;
    }
    let mut picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());
    if let Some(protocol) = forced_protocol {
        picker.set_protocol_type(protocol);
    }
    ImageMode::graphics(picker)
}

/// Encoded images for the graphics protocol, by vault-relative path.
#[derive(Default)]
struct ImageProtocols(std::cell::RefCell<HashMap<String, StatefulProtocol>>);

impl std::fmt::Debug for ImageProtocols {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ImageProtocols")
            .field("images", &self.0.borrow().len())
            .finish()
    }
}

enum PreviewImage {
    /// Drawn as half-block text lines.
    Lines(Vec<Line<'static>>),
    /// Drawn by the terminal's graphics protocol over reserved lines.
    Reserved {
        path: String,
        columns: u16,
        rows: u16,
    },
}

/// `path` as a vault-relative path with `.` and `..` resolved; `None` when
/// it would leave the vault.
fn normalize_vault_relative(path: &Path) -> Option<String> {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::Normal(part) => parts.push(part.to_str()?),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                parts.pop()?;
            }
            _ => return None,
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// Record image files under `root/relative` by lowercased name, skipping
/// hidden folders such as `.git` and `.vulcan`. The first path in sorted
/// order wins.
fn collect_image_files(root: &Path, relative: &Path, files: &mut HashMap<String, String>) {
    let Ok(entries) = fs::read_dir(root.join(relative)) else {
        return;
    };
    let mut entries = entries.filter_map(Result::ok).collect::<Vec<_>>();
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        let path = relative.join(name);
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => collect_image_files(root, &path, files),
            Ok(kind) if kind.is_file() && is_previewable_image(name) => {
                if let Some(path) = normalize_vault_relative(&path) {
                    files.entry(name.to_lowercase()).or_insert(path);
                }
            }
            _ => {}
        }
    }
}

fn is_external_link(target: &str) -> bool {
    target.contains("://") || target.starts_with("mailto:")
}

/// A link target as a note reference: no `#heading` or `^block`, `%20`
/// decoded as in Markdown links.
fn decode_link_target(target: &str) -> String {
    let note = target.split(['#', '^']).next().unwrap_or_default();
    let mut decoded = Vec::with_capacity(note.len());
    let bytes = note.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(&note[index + 1..index + 3], 16) {
                decoded.push(byte);
                index += 3;
                continue;
            }
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&decoded).trim().to_string()
}

/// Open `url` with the platform's default handler, detached from browse.
fn open_external_link(url: &str) -> io::Result<()> {
    let mut command = if cfg!(target_os = "macos") {
        let mut command = std::process::Command::new("open");
        command.arg(url);
        command
    } else if cfg!(windows) {
        let mut command = std::process::Command::new("cmd");
        command.args(["/C", "start", "", url]);
        command
    } else {
        let mut command = std::process::Command::new("xdg-open");
        command.arg(url);
        command
    };
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
}

fn draw_note_preview(frame: &mut Frame<'_>, state: &BrowseState, area: Rect) {
    frame.render_widget(note_preview_widget(state, area), area);
    state.draw_preview_images(frame, area);
}

fn note_preview_widget(state: &BrowseState, area: Rect) -> Paragraph<'static> {
    state
        .rendered_preview
        .visible_rows
        .set(area.height.saturating_sub(2));
    let paragraph = Paragraph::new(state.preview_lines_for_width(area.width.saturating_sub(2)))
        .block(
            Block::default()
                .title(state.preview_title())
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .scroll((state.preview_scroll(), 0));
    // Rendered lines are laid out to the pane width already; wrapping them
    // again would shift the rows images are drawn over.
    if state.uses_rendered_preview() {
        paragraph
    } else {
        paragraph.wrap(Wrap { trim: false })
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
        draw_note_preview(frame, state, body[1]);

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

    let range = state.viewport.range(
        state.filtered_count(),
        state.selected_index(),
        usize::from(body[0].height.saturating_sub(2)),
    );
    let items = state
        .list_items(range.clone())
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
    list_state.select(
        state
            .selected_index()
            .and_then(|index| range.contains(&index).then(|| index - range.start)),
    );
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
        draw_note_preview(frame, state, preview_layout[0]);

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
        draw_note_preview(frame, state, body[1]);
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

/// The selected note rendered with its dynamic content evaluated.
#[derive(Debug, Default)]
struct RenderedPreview {
    path: Option<String>,
    /// The note's file contents, shown in source mode.
    source: String,
    markdown: String,
    stale: bool,
    /// The source is shown while the worker evaluates the note.
    pending: bool,
    scroll: u16,
    /// Lines laid out for the last pane width.
    layout: std::cell::RefCell<Option<PreviewLayout>>,
    /// The link Tab selected, as an index into the layout's links.
    selected_link: Option<usize>,
    /// Rows the preview pane showed at the last draw, to keep the selected
    /// link in view.
    visible_rows: std::cell::Cell<u16>,
}

#[derive(Debug)]
struct PreviewLayout {
    width: u16,
    lines: Vec<Line<'static>>,
    /// Followable links (not images), in reading order.
    links: Vec<LaidOutLink>,
    /// Images drawn over reserved blank lines by a terminal graphics
    /// protocol.
    images: Vec<PlacedImage>,
}

#[derive(Debug)]
struct PlacedImage {
    /// The first reserved line.
    first_line: usize,
    columns: u16,
    rows: u16,
    /// Vault-relative image path.
    path: String,
}

/// Evaluated previews of recently viewed notes, so moving back to a note
/// shows it at once. An entry is used only while the note's source is
/// unchanged, and the cache clears whenever the index changes.
#[derive(Debug, Default)]
struct EvaluatedPreviewCache {
    entries: std::collections::HashMap<String, (String, String)>,
    order: std::collections::VecDeque<String>,
}

impl EvaluatedPreviewCache {
    const CAPACITY: usize = 64;

    fn get(&self, path: &str, source: &str) -> Option<&str> {
        self.entries
            .get(path)
            .filter(|(cached_source, _)| cached_source == source)
            .map(|(_, markdown)| markdown.as_str())
    }

    fn insert(&mut self, path: String, source: String, markdown: String) {
        if self
            .entries
            .insert(path.clone(), (source, markdown))
            .is_none()
        {
            self.order.push_back(path);
            while self.order.len() > Self::CAPACITY {
                if let Some(oldest) = self.order.pop_front() {
                    self.entries.remove(&oldest);
                }
            }
        }
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
    }
}

/// Lines a PageUp/PageDown press scrolls the rendered preview.
const PREVIEW_SCROLL_STEP: i32 = 10;

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
            let result = refresh_browse_cache(&paths).map_err(|error| error.to_string());
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
    viewport: Viewport,
    mode: BrowseMode,
    /// Ctrl-V flips the preview away from the mode's default: rendered
    /// notes show their source, and full-text snippets show the rendered note.
    preview_toggled: bool,
    rendered_preview: RenderedPreview,
    preview_worker: PreviewWorker,
    evaluated_previews: EvaluatedPreviewCache,
    /// Decoded preview images by vault-relative path; `None` when decoding
    /// failed.
    preview_images: std::cell::RefCell<HashMap<String, Result<image::DynamicImage, &'static str>>>,
    /// How preview images are drawn.
    image_mode: ImageMode,
    /// Encoded images for the graphics protocol, by vault-relative path.
    image_protocols: ImageProtocols,
    /// Image files by lowercased name, for links that give only a name.
    image_files: std::cell::RefCell<Option<HashMap<String, String>>>,
    /// The vault's preferred attachment folder.
    attachment_folder: PathBuf,
    /// The vault's Obsidian `strictLineBreaks` setting.
    strict_line_breaks: bool,
    status: String,
    authority: BrowseAuthority,
}

impl BrowseState {
    #[cfg(test)]
    fn new(paths: VaultPaths, notes: Vec<NoteIdentity>) -> Result<Self, String> {
        let authority = BrowseAuthority::default_for(&paths);
        Self::new_with_authority(paths, notes, authority)
    }

    #[allow(clippy::needless_pass_by_value)]
    fn new_with_authority(
        paths: VaultPaths,
        notes: Vec<NoteIdentity>,
        authority: BrowseAuthority,
    ) -> Result<Self, String> {
        let tags = if paths.cache_db().exists() {
            match authority.read_filter() {
                Some(filter) => vulcan_core::list_tags_with_filter(&paths, Some(filter))
                    .map_err(|error| error.to_string()),
                None => list_tags(&paths).map_err(|error| error.to_string()),
            }?
        } else {
            Vec::new()
        };
        let last_scan_label = last_scan_label(&paths);
        let mut state = Self {
            paths: paths.clone(),
            all_notes: notes.clone(),
            picker: NotePickerState::new(paths.clone(), notes.clone(), ""),
            full_text: FullTextState {
                read_filter: authority.read_filter().cloned(),
                ..FullTextState::default()
            },
            tag_filter: TagFilterState::new(paths.clone(), tags),
            property_filter: PropertyFilterState::new(paths.clone()),
            calendar: CalendarViewState::new_scoped(&paths, authority.read_filter().cloned())?,
            kanban_view: None,
            backlinks_view: None,
            links_view: None,
            git_view: None,
            doctor_view: None,
            new_note_prompt: None,
            move_prompt: None,
            background_refresh: None,
            last_scan_label,
            viewport: Viewport::default(),
            mode: BrowseMode::Fuzzy,
            preview_toggled: false,
            rendered_preview: RenderedPreview {
                stale: true,
                ..RenderedPreview::default()
            },
            preview_worker: PreviewWorker::spawn(
                paths.clone(),
                authority
                    .read_filter()
                    .is_some()
                    .then(|| authority.guard().clone()),
                authority.profile().map(ToOwned::to_owned),
            ),
            evaluated_previews: EvaluatedPreviewCache::default(),
            preview_images: std::cell::RefCell::new(HashMap::new()),
            image_files: std::cell::RefCell::new(None),
            image_mode: ImageMode::HalfBlocks,
            image_protocols: ImageProtocols::default(),
            attachment_folder: vulcan_core::load_vault_config(&paths)
                .config
                .attachment_folder,
            strict_line_breaks: vulcan_core::load_vault_config(&paths)
                .config
                .strict_line_breaks,
            status: "Ready.".to_string(),
            authority,
        };
        state.refresh_rendered_preview_if_needed();
        Ok(state)
    }

    /// Auto-commit `changed` under the session's profile; without Git
    /// authority nothing is committed.
    fn commit(&self, auto_commit: &AutoCommitPolicy, changed: &[String]) -> Result<(), String> {
        if self.authority.check_git().is_err() {
            return Ok(());
        }
        auto_commit
            .commit(
                &self.paths,
                "browse",
                changed,
                self.authority.profile(),
                Verbosity::Normal,
            )
            .map(|_| ())
            .map_err(|error| error.to_string())
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
                            self.refresh_rendered_preview_if_needed();
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

        if self.uses_rendered_preview() {
            match key.code {
                KeyCode::Tab => {
                    self.select_preview_link(1);
                    return BrowseAction::Continue;
                }
                KeyCode::BackTab => {
                    self.select_preview_link(-1);
                    return BrowseAction::Continue;
                }
                KeyCode::Enter => {
                    if let Some(SpanLink::Link(target)) = self.selected_preview_link() {
                        self.follow_preview_link(&target);
                        return BrowseAction::Continue;
                    }
                }
                KeyCode::Esc if self.rendered_preview.selected_link.is_some() => {
                    self.rendered_preview.selected_link = None;
                    self.clear_status();
                    return BrowseAction::Continue;
                }
                _ => {}
            }
        }

        if matches!(key.code, KeyCode::PageUp | KeyCode::PageDown)
            && self.uses_file_preview()
            && self.mode != BrowseMode::FullText
        {
            self.scroll_rendered_preview(if key.code == KeyCode::PageUp {
                -PREVIEW_SCROLL_STEP
            } else {
                PREVIEW_SCROLL_STEP
            });
            return BrowseAction::Continue;
        }

        match key.code {
            KeyCode::Esc => BrowseAction::Quit,
            KeyCode::Char('/') if self.mode != BrowseMode::Fuzzy => {
                self.clear_status();
                self.mode = BrowseMode::Fuzzy;
                self.refresh_rendered_preview_if_needed();
                BrowseAction::Continue
            }
            KeyCode::Enter => self.edit_selected_path(),
            _ => {
                self.clear_status();
                if let Err(error) = self.handle_mode_key(key) {
                    self.set_status(error);
                } else {
                    self.refresh_rendered_preview_if_needed();
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
                self.refresh_rendered_preview_if_needed();
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
                self.refresh_rendered_preview_if_needed();
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
                self.refresh_rendered_preview_if_needed();
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
                self.refresh_rendered_preview_if_needed();
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
                self.refresh_rendered_preview_if_needed();
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
        self.refresh_rendered_preview_if_needed();
        Ok(())
    }

    fn reload_after_edit(&mut self) -> Result<(), String> {
        let notes = load_notes(&self.paths, &self.authority)?;
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
            view.reload(&self.paths, &self.authority)?;
        }
        if let Some(view) = self.backlinks_view.as_mut() {
            view.reload(&self.paths, self.authority.read_filter())?;
        }
        if let Some(view) = self.links_view.as_mut() {
            view.reload(&self.paths, self.authority.read_filter())?;
        }
        self.refresh_last_scan_label();
        self.invalidate_rendered_preview();
        self.refresh_rendered_preview_if_needed();
        Ok(())
    }

    fn poll_background_refresh(&mut self) -> bool {
        let result = self
            .background_refresh
            .as_ref()
            .and_then(BackgroundRefreshState::try_finish);
        let Some(result) = result else {
            return false;
        };
        self.background_refresh = None;
        self.apply_background_refresh_result(result);
        true
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
        let notes = load_notes(&self.paths, &self.authority)?;
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
        self.invalidate_rendered_preview();
        self.refresh_rendered_preview_if_needed();
        Ok(())
    }

    fn refresh_preview(&mut self) {
        match self.mode {
            BrowseMode::Fuzzy => self.picker.refresh_preview(),
            BrowseMode::FullText | BrowseMode::Calendar => {}
            BrowseMode::Tag => self.tag_filter.refresh_preview(),
            BrowseMode::Property => self.property_filter.refresh_preview(),
        }
        self.refresh_rendered_preview_if_needed();
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

    /// Whether the preview shows the rendered note. Without a selection the
    /// mode's own empty-state preview stays visible.
    fn uses_rendered_preview(&self) -> bool {
        self.uses_note_preview() && self.selected_path().is_some() && self.wants_rendered_preview()
    }

    /// Whether the preview shows the selected note's whole source.
    fn uses_source_preview(&self) -> bool {
        self.uses_note_preview()
            && self.selected_path().is_some()
            && self.preview_toggled
            && matches!(
                self.mode,
                BrowseMode::Fuzzy | BrowseMode::Tag | BrowseMode::Property
            )
    }

    /// Whether the preview shows the selected note's file, rendered or not.
    fn uses_file_preview(&self) -> bool {
        self.uses_rendered_preview() || self.uses_source_preview()
    }

    /// The note's source with a dim line-number gutter.
    fn source_preview_lines(&self) -> Vec<Line<'static>> {
        let source = &self.rendered_preview.source;
        let width = source.lines().count().max(1).to_string().len();
        source
            .lines()
            .enumerate()
            .map(|(index, line)| {
                Line::from(vec![
                    Span::styled(
                        format!("{:>width$} ", index + 1),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::raw(line.to_string()),
                ])
            })
            .collect()
    }

    fn wants_rendered_preview(&self) -> bool {
        match self.mode {
            BrowseMode::Calendar => false,
            BrowseMode::FullText => self.preview_toggled,
            BrowseMode::Fuzzy | BrowseMode::Tag | BrowseMode::Property => !self.preview_toggled,
        }
    }

    fn preview_label(&self) -> &'static str {
        if self.wants_rendered_preview() {
            "rendered"
        } else if self.mode == BrowseMode::FullText {
            "snippet"
        } else {
            "source"
        }
    }

    fn toggle_preview_mode(&mut self) {
        if self.mode == BrowseMode::Calendar {
            self.set_status("Calendar mode uses a fixed event preview.");
            return;
        }
        self.preview_toggled = !self.preview_toggled;
        self.refresh_rendered_preview_if_needed();
        self.set_status(format!("Preview mode: {}.", self.preview_label()));
    }

    /// Mark the rendered preview and the loaded note index out of date, for
    /// example after a scan or an edit.
    fn invalidate_rendered_preview(&mut self) {
        self.rendered_preview.stale = true;
        self.evaluated_previews.clear();
        self.preview_images.borrow_mut().clear();
        self.image_protocols.0.borrow_mut().clear();
        self.image_files.replace(None);
        self.preview_worker.reset();
    }

    fn refresh_rendered_preview_if_needed(&mut self) {
        if self.uses_file_preview() {
            self.refresh_rendered_preview();
        }
    }

    fn refresh_rendered_preview(&mut self) {
        let selected_path = self.selected_path().map(ToOwned::to_owned);
        if !self.rendered_preview.stale && self.rendered_preview.path == selected_path {
            return;
        }
        if self.rendered_preview.path != selected_path {
            self.rendered_preview.scroll = 0;
        }
        // Show the source at once; the worker swaps in the evaluated note.
        self.rendered_preview.pending = false;
        self.rendered_preview.markdown = match selected_path.as_deref() {
            None => {
                self.rendered_preview.source.clear();
                "*No matching note selected.*".to_string()
            }
            Some(path) => match fs::read_to_string(self.paths.vault_root().join(path)) {
                Ok(source) => {
                    self.rendered_preview.source.clone_from(&source);
                    if let Some(markdown) = self.evaluated_previews.get(path, &source) {
                        markdown.to_string()
                    } else {
                        self.preview_worker.request(path);
                        self.rendered_preview.pending = true;
                        source
                    }
                }
                Err(error) => {
                    self.rendered_preview.source.clear();
                    format!("> [!danger] Could not read {path}\n> {error}")
                }
            },
        };
        self.rendered_preview.path = selected_path;
        self.rendered_preview.layout.replace(None);
        self.rendered_preview.selected_link = None;
        self.rendered_preview.stale = false;
    }

    /// Take the evaluated note from the worker; true when the preview changed.
    fn poll_rendered_preview(&mut self) -> bool {
        if !self.rendered_preview.pending {
            return false;
        }
        let Some(note) = self.preview_worker.try_take() else {
            return false;
        };
        self.apply_rendered_note(note)
    }

    fn apply_rendered_note(&mut self, note: RenderedNote) -> bool {
        if self.rendered_preview.path.as_deref() != Some(note.path.as_str()) {
            return false;
        }
        self.evaluated_previews.insert(
            note.path,
            self.rendered_preview.source.clone(),
            note.markdown.clone(),
        );
        self.rendered_preview.markdown = note.markdown;
        self.rendered_preview.pending = false;
        self.rendered_preview.layout.replace(None);
        self.rendered_preview.selected_link = None;
        true
    }

    /// Wait for the worker so tests see the evaluated note.
    #[cfg(test)]
    fn settle_rendered_preview(&mut self) {
        if self.rendered_preview.pending {
            if let Some(note) = self.preview_worker.wait() {
                self.apply_rendered_note(note);
            }
        }
    }

    fn ensure_preview_layout(&self, width: u16) {
        if self
            .rendered_preview
            .layout
            .borrow()
            .as_ref()
            .is_some_and(|layout| layout.width == width)
        {
            return;
        }
        let styled = render_styled_lines(
            &self.rendered_preview.markdown,
            TerminalMarkdownOptions {
                width: Some(usize::from(width.max(20))),
                styled: true,
                preserve_line_breaks: !self.strict_line_breaks,
            },
        );
        let all_links = laid_out_links(&styled);
        // A shown image replaces its `[image: ...]` line when the line holds
        // nothing else, and goes right below it otherwise.
        let mut images_by_line = HashMap::<usize, Vec<String>>::new();
        for link in &all_links {
            if let SpanLink::Image(source) = &link.target {
                let line = link.segments.last().map_or(0, |segment| segment.0);
                images_by_line.entry(line).or_default().push(source.clone());
            }
        }
        let mut laid_out = Vec::with_capacity(styled.len());
        let mut placed = Vec::new();
        let mut line_starts = Vec::with_capacity(styled.len());
        for (index, line) in to_ratatui_lines(&styled).into_iter().enumerate() {
            let shown = images_by_line
                .get(&index)
                .into_iter()
                .flatten()
                .map(|source| self.preview_image(source, width))
                .collect::<Vec<_>>();
            let replaces_line = !shown.is_empty()
                && shown.iter().all(Result::is_ok)
                && holds_only_images(&styled[index]);
            line_starts.push(laid_out.len());
            if !replaces_line {
                laid_out.push(line);
            }
            for image in shown {
                match image {
                    Ok(PreviewImage::Lines(lines)) => laid_out.extend(lines),
                    Ok(PreviewImage::Reserved {
                        path,
                        columns,
                        rows,
                    }) => {
                        placed.push(PlacedImage {
                            first_line: laid_out.len(),
                            columns,
                            rows,
                            path,
                        });
                        laid_out.extend((0..rows).map(|_| Line::default()));
                    }
                    Err(reason) => laid_out.push(Line::styled(
                        format!("  image not shown: {reason}"),
                        Style::default().fg(Color::DarkGray),
                    )),
                }
            }
        }
        let followable = all_links
            .into_iter()
            .filter(|link| matches!(link.target, SpanLink::Link(_)))
            .map(|mut link| {
                for segment in &mut link.segments {
                    segment.0 = line_starts.get(segment.0).copied().unwrap_or(segment.0);
                }
                link
            })
            .collect();
        self.rendered_preview.layout.replace(Some(PreviewLayout {
            width,
            lines: laid_out,
            links: followable,
            images: placed,
        }));
    }

    /// How an image embedded in the preview is shown: rows reserved for a
    /// terminal graphics protocol when the terminal has one, half-block lines
    /// otherwise, or nothing when it cannot be found, read, or decoded.
    fn preview_image(&self, source: &str, width: u16) -> Result<PreviewImage, &'static str> {
        if matches!(self.image_mode, ImageMode::Off) {
            return Err("images are turned off by VULCAN_IMAGE_PROTOCOL=off");
        }
        let path = self.resolve_preview_image(source)?;
        let mut cache = self.preview_images.borrow_mut();
        let image = cache
            .entry(path.clone())
            .or_insert_with(|| load_preview_image(&self.paths.vault_root().join(&path)))
            .as_ref()
            .map_err(|reason| *reason)?;
        // Images may use the pane's height once it is known.
        let visible = self.rendered_preview.visible_rows.get();
        let max_rows = if visible > 0 {
            visible.saturating_sub(1).max(8)
        } else {
            MAX_IMAGE_ROWS
        };
        Ok(match &self.image_mode {
            ImageMode::Graphics { picker, .. } => {
                let (columns, rows) =
                    image_cells(image, width, max_rows, picker.font_size(), PREVIEW_UPSCALE);
                PreviewImage::Reserved {
                    path,
                    columns,
                    rows,
                }
            }
            ImageMode::HalfBlocks | ImageMode::Off => {
                // Half-blocks show two pixels per cell, like a 1:2 font.
                let (columns, rows) =
                    image_cells(image, width, max_rows, HALFBLOCK_FONT_SIZE, PREVIEW_UPSCALE);
                PreviewImage::Lines(halfblock_lines(image, columns, rows))
            }
        })
    }

    /// Draw the layout's reserved images with the terminal graphics
    /// protocol, over the preview `area` (including its border). An image
    /// cut off at the bottom is cropped; one scrolled past the top is left
    /// out until it comes back into view.
    fn draw_preview_images(&self, frame: &mut Frame<'_>, area: Rect) {
        let ImageMode::Graphics { picker, .. } = &self.image_mode else {
            return;
        };
        if !self.uses_rendered_preview() {
            return;
        }
        let layout = self.rendered_preview.layout.borrow();
        let Some(layout) = layout.as_ref() else {
            return;
        };
        let inner = Rect {
            x: area.x.saturating_add(1),
            y: area.y.saturating_add(1),
            width: area.width.saturating_sub(2),
            height: area.height.saturating_sub(2),
        };
        let scroll = usize::from(self.preview_scroll());
        let images = self.preview_images.borrow();
        let mut protocols = self.image_protocols.0.borrow_mut();
        for placed in &layout.images {
            let Some(row) = placed.first_line.checked_sub(scroll) else {
                continue;
            };
            let Ok(row) = u16::try_from(row) else {
                continue;
            };
            if row >= inner.height {
                continue;
            }
            let Some(Ok(image)) = images.get(&placed.path) else {
                continue;
            };
            let height = placed.rows.min(inner.height - row);
            let target = Rect {
                y: inner.y + row,
                width: placed.columns.min(inner.width),
                height,
                ..inner
            };
            let protocol = protocols
                .entry(placed.path.clone())
                .or_insert_with(|| picker.new_resize_protocol(image.clone()));
            // The rows were sized to the image, so scaling fills them; an
            // image cut off at the bottom is cropped instead of shrunk.
            let widget = if height < placed.rows {
                StatefulImage::default().resize(Resize::Crop(None))
            } else {
                StatefulImage::default().resize(Resize::Scale(None))
            };
            frame.render_stateful_widget(widget, target, protocol);
        }
    }

    /// The vault-relative path of an image the preview links, found as
    /// Obsidian does: next to the note, from the vault root, in the
    /// attachment folder, then by file name anywhere in the vault. Only
    /// files the session's profile can read are used.
    fn resolve_preview_image(&self, source: &str) -> Result<String, &'static str> {
        if is_external_link(source) {
            return Err("remote images are not loaded");
        }
        let source = decode_link_target(source);
        if !is_previewable_image(&source) {
            return Err("only PNG, JPEG, GIF, WebP, and BMP images are shown");
        }
        let note_dir = self
            .rendered_preview
            .path
            .as_deref()
            .and_then(|path| Path::new(path).parent())
            .map(Path::to_path_buf)
            .unwrap_or_default();
        let candidates = [
            note_dir.join(&source),
            PathBuf::from(&source),
            self.attachment_folder.join(&source),
        ];
        let found = candidates
            .iter()
            .filter_map(|candidate| normalize_vault_relative(candidate))
            .find(|candidate| self.paths.vault_root().join(candidate).is_file())
            .or_else(|| {
                let name = Path::new(&source).file_name()?.to_str()?.to_lowercase();
                self.image_files_by_name().get(&name).cloned()
            })
            .ok_or("no such image in the vault")?;
        // An image the profile cannot read looks missing, so its existence
        // does not leak.
        if self.authority.can_read(&found) {
            Ok(found)
        } else {
            Err("no such image in the vault")
        }
    }

    /// Image files in the vault by lowercased file name, built on first use.
    fn image_files_by_name(&self) -> std::cell::Ref<'_, HashMap<String, String>> {
        if self.image_files.borrow().is_none() {
            let mut files = HashMap::new();
            collect_image_files(self.paths.vault_root(), Path::new(""), &mut files);
            self.image_files.replace(Some(files));
        }
        std::cell::Ref::map(self.image_files.borrow(), |files| {
            files.as_ref().expect("image files were just indexed")
        })
    }

    fn rendered_preview_lines(&self, width: u16) -> Vec<Line<'static>> {
        self.ensure_preview_layout(width);
        let layout = self.rendered_preview.layout.borrow();
        let layout = layout.as_ref().expect("layout was just built");
        let mut lines = layout.lines.clone();
        if let Some(link) = self
            .rendered_preview
            .selected_link
            .and_then(|index| layout.links.get(index))
        {
            for &(line, first, last) in &link.segments {
                if let Some(spans) = lines.get_mut(line).map(|line| &mut line.spans) {
                    for span in spans.iter_mut().take(last + 1).skip(first) {
                        span.style = span.style.add_modifier(Modifier::REVERSED);
                    }
                }
            }
        }
        lines
    }

    /// Select the next (`step` 1) or previous (`step` -1) link in the
    /// rendered preview and scroll it into view.
    fn select_preview_link(&mut self, step: isize) {
        // Each link's first line, read before the selection changes.
        let link_lines = self
            .rendered_preview
            .layout
            .borrow()
            .as_ref()
            .map(|layout| {
                layout
                    .links
                    .iter()
                    .map(|link| link.segments[0].0)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if link_lines.is_empty() {
            self.set_status("No links in this preview.");
            return;
        }
        let count = link_lines.len();
        let next = match self.rendered_preview.selected_link {
            Some(index) => (index.cast_signed() + step).rem_euclid(count.cast_signed()),
            None if step < 0 => count.cast_signed() - 1,
            None => 0,
        }
        .cast_unsigned();
        self.rendered_preview.selected_link = Some(next);
        let line = u16::try_from(link_lines[next]).unwrap_or(u16::MAX);
        // Before the first draw the pane height is unknown; leave the scroll.
        let rows = self.rendered_preview.visible_rows.get();
        if rows > 0
            && (line < self.rendered_preview.scroll || line >= self.rendered_preview.scroll + rows)
        {
            self.rendered_preview.scroll = line.saturating_sub(rows / 3);
        }
        self.set_status(format!(
            "Link {}/{count}: Enter follows it, Esc clears the selection.",
            next + 1
        ));
    }

    fn selected_preview_link(&self) -> Option<SpanLink> {
        let layout = self.rendered_preview.layout.borrow();
        let index = self.rendered_preview.selected_link?;
        layout
            .as_ref()?
            .links
            .get(index)
            .map(|link| link.target.clone())
    }

    /// Follow the selected link: a note opens in browse, a URL in the
    /// default browser.
    fn follow_preview_link(&mut self, target: &str) {
        if is_external_link(target) {
            match open_external_link(target) {
                Ok(()) => self.set_status(format!("Opened {target} in the default browser.")),
                Err(error) => self.set_status(format!("Could not open {target}: {error}")),
            }
            return;
        }
        let note = decode_link_target(target);
        if note.is_empty() {
            self.set_status("That link points within this note.");
            return;
        }
        let resolved = vulcan_core::resolve_note_reference_with_filter(
            &self.paths,
            &note,
            self.authority.read_filter(),
        );
        match resolved {
            Ok(reference) if self.authority.can_read(&reference.path) => {
                self.open_note_in_browse(&reference.path);
            }
            _ => self.set_status(format!("No readable note matches `{note}`.")),
        }
    }

    fn open_note_in_browse(&mut self, path: &str) {
        self.mode = BrowseMode::Fuzzy;
        self.preview_toggled = false;
        self.picker.set_query("");
        self.picker.select_path(path);
        self.refresh_preview();
        self.set_status(format!("Opened {path}."));
    }

    fn scroll_rendered_preview(&mut self, delta: i32) {
        let line_count = if self.uses_source_preview() {
            self.rendered_preview.source.lines().count()
        } else {
            self.rendered_preview
                .layout
                .borrow()
                .as_ref()
                .map_or(0, |layout| layout.lines.len())
        };
        let max_scroll = u16::try_from(line_count.saturating_sub(1)).unwrap_or(u16::MAX);
        let next = i32::from(self.rendered_preview.scroll) + delta;
        self.rendered_preview.scroll = u16::try_from(next.max(0))
            .unwrap_or(u16::MAX)
            .min(max_scroll);
    }

    fn preview_scroll(&self) -> u16 {
        if self.uses_file_preview() {
            self.rendered_preview.scroll
        } else {
            0
        }
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
                BrowseMode::Fuzzy => "Keys: type to filter, Up/Down move, Enter/Ctrl-E edit, Ctrl-V source/rendered, PgUp/PgDn scroll preview, Tab/Shift-Tab select link, Enter follow link, o open Kanban board when query is empty, Ctrl-N new, Ctrl-R move, Ctrl-B backlinks, Ctrl-O links, Ctrl-D doctor, Ctrl-G git, Ctrl-F full-text, Ctrl-T tags, Ctrl-P props, Ctrl-Y calendar, Esc quit".to_string(),
                BrowseMode::FullText => "Keys: type to search, Backspace edit query, Up/Down move, Enter edit, Ctrl-V rendered note, Ctrl-E explain, Ctrl-S sort, Alt-C case, Ctrl-B backlinks, Ctrl-O links, Ctrl-D doctor, Ctrl-G git, Ctrl-Y calendar, / fuzzy, Esc quit".to_string(),
                BrowseMode::Tag => "Keys: type to filter tags, Backspace edit query, Up/Down move, Enter/Ctrl-E edit, Ctrl-V source/rendered, PgUp/PgDn scroll preview, Tab/Shift-Tab select link, Enter follow link, o open Kanban board when query is empty, Ctrl-B backlinks, Ctrl-O links, Ctrl-D doctor, Ctrl-G git, Ctrl-Y calendar, / fuzzy, Esc quit".to_string(),
                BrowseMode::Property => "Keys: type a predicate, Backspace edit query, Up/Down move, Enter/Ctrl-E edit, Ctrl-V source/rendered, PgUp/PgDn scroll preview, Tab/Shift-Tab select link, Enter follow link, o open Kanban board when query is empty, Ctrl-B backlinks, Ctrl-O links, Ctrl-D doctor, Ctrl-G git, Ctrl-Y calendar, / fuzzy, Esc quit".to_string(),
                BrowseMode::Calendar => "Keys: arrows move day, PageUp/PageDown or [/] change month, Home/End jump month, t today, Enter edit or create daily note, type YYYY-MM or YYYY-MM-DD to jump, / fuzzy, Esc quit".to_string(),
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

    fn list_items(&self, range: std::ops::Range<usize>) -> Vec<String> {
        if let Some(view) = self.kanban_view.as_ref() {
            return view
                .board
                .columns
                .iter()
                .skip(range.start)
                .take(range.len())
                .map(|column| format!("{} ({})", column.name, column.card_count))
                .collect();
        }
        if let Some(view) = self.git_view.as_ref() {
            return view.list_items(range);
        }
        if let Some(view) = self.doctor_view.as_ref() {
            return view.list_items(range);
        }
        if let Some(view) = self.backlinks_view.as_ref() {
            return view.list_items(range);
        }
        if let Some(view) = self.links_view.as_ref() {
            return view.list_items(range);
        }
        match self.mode {
            BrowseMode::Fuzzy => self
                .picker
                .filtered_notes()
                .skip(range.start)
                .take(range.len())
                .map(|(_, note)| crate::note_picker::note_label(note))
                .collect(),
            BrowseMode::FullText => self
                .full_text
                .hits
                .iter()
                .skip(range.start)
                .take(range.len())
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
            BrowseMode::Tag => self.tag_filter.list_items(range),
            BrowseMode::Property => self.property_filter.list_items(range),
            BrowseMode::Calendar => self.calendar.list_items(range),
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
        let title = self.mode_preview_title();
        if !self.uses_rendered_preview() {
            return title;
        }
        let evaluating = if self.rendered_preview.pending {
            " (evaluating…)"
        } else {
            ""
        };
        // Keep mode context such as an active tag in the title.
        match (title.strip_prefix("Preview: "), self.selected_path()) {
            (Some(rest), _) => format!("Rendered: {rest}{evaluating}"),
            (None, Some(path)) => format!("Rendered: {path}{evaluating}"),
            (None, None) => title,
        }
    }

    fn mode_preview_title(&self) -> String {
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

    #[cfg(test)]
    fn preview_lines(&self) -> Vec<Line<'static>> {
        self.preview_lines_for_width(80)
    }

    /// Preview lines for a pane `width` columns wide (inside its borders).
    fn preview_lines_for_width(&self, width: u16) -> Vec<Line<'static>> {
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
        if self.uses_rendered_preview() {
            return self.rendered_preview_lines(width);
        }
        if self.uses_source_preview() {
            return self.source_preview_lines();
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
        let preview_meta =
            (self.uses_note_preview() && self.mode != BrowseMode::Calendar).then(|| {
                format!(
                    " | Preview: {} | Images: {}",
                    self.preview_label(),
                    self.image_mode.label()
                )
            });
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
        self.backlinks_view = Some(BacklinksViewState::load(
            &self.paths,
            &path,
            self.authority.read_filter(),
        )?);
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
        self.links_view = Some(OutgoingLinksViewState::load(
            &self.paths,
            &path,
            self.authority.read_filter(),
        )?);
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
        self.doctor_view = Some(DoctorViewState::load(&self.paths, &path, &self.authority)?);
        self.kanban_view = None;
        self.backlinks_view = None;
        self.links_view = None;
        self.git_view = None;
        Ok(())
    }

    fn open_git_view(&mut self) -> Result<(), String> {
        self.authority.check_git()?;
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
            .is_ok_and(|boards| boards.into_iter().any(|board| board.path == path))
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
                format!("Calendar day: {}", self.calendar.selected_date_iso())
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

    fn list_items(&self, range: std::ops::Range<usize>) -> Vec<String> {
        self.entries
            .iter()
            .skip(range.start)
            .take(range.len())
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
    fn load(
        paths: &VaultPaths,
        note_path: &str,
        authority: &BrowseAuthority,
    ) -> Result<Self, String> {
        let report = doctor_vault(paths).map_err(|error| error.to_string())?;
        let issues = doctor_issues_for_note(note_path, &report, authority);
        let selected_index = (!issues.is_empty()).then_some(0);
        Ok(Self {
            note_path: note_path.to_string(),
            issues,
            selected_index,
        })
    }

    fn reload(&mut self, paths: &VaultPaths, authority: &BrowseAuthority) -> Result<(), String> {
        let selected_key = self
            .selected_issue()
            .map(|issue| (issue.kind.clone(), issue.message.clone()));
        let report = doctor_vault(paths).map_err(|error| error.to_string())?;
        self.issues = doctor_issues_for_note(&self.note_path, &report, authority);
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

    fn list_items(&self, range: std::ops::Range<usize>) -> Vec<String> {
        self.issues
            .iter()
            .skip(range.start)
            .take(range.len())
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

/// The doctor rows of one readable note, showing only readable candidate
/// paths. Restricted sessions omit the orphan row, since whether a note has
/// links can depend on notes they cannot see.
fn doctor_issues_for_note(
    note_path: &str,
    report: &vulcan_core::DoctorReport,
    authority: &BrowseAuthority,
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
            .map(|issue| {
                let mut issue = issue.clone();
                issue.matches.retain(|path| authority.can_read(path));
                doctor_link_issue_row("Ambiguous link", &issue)
            }),
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
    if authority.read_filter().is_none() && report.orphan_notes.iter().any(|path| path == note_path)
    {
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
    fn load(
        paths: &VaultPaths,
        note_path: &str,
        filter: Option<&PermissionFilter>,
    ) -> Result<Self, String> {
        let report = vulcan_core::query_backlinks_with_filter(paths, note_path, filter)
            .map_err(|error| error.to_string())?;
        let selected_index = (!report.backlinks.is_empty()).then_some(0);
        Ok(Self {
            note_path: report.note_path,
            backlinks: report.backlinks,
            selected_index,
        })
    }

    fn reload(
        &mut self,
        paths: &VaultPaths,
        filter: Option<&PermissionFilter>,
    ) -> Result<(), String> {
        let selected_path = self.selected_path().map(str::to_string);
        let report = vulcan_core::query_backlinks_with_filter(paths, &self.note_path, filter)
            .map_err(|error| error.to_string())?;
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

    fn list_items(&self, range: std::ops::Range<usize>) -> Vec<String> {
        self.backlinks
            .iter()
            .skip(range.start)
            .take(range.len())
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
    fn load(
        paths: &VaultPaths,
        note_path: &str,
        filter: Option<&PermissionFilter>,
    ) -> Result<Self, String> {
        let report = vulcan_core::query_links_with_filter(paths, note_path, filter)
            .map_err(|error| error.to_string())?;
        let selected_index = (!report.links.is_empty()).then_some(0);
        Ok(Self {
            note_path: report.note_path,
            links: report.links,
            selected_index,
        })
    }

    fn reload(
        &mut self,
        paths: &VaultPaths,
        filter: Option<&PermissionFilter>,
    ) -> Result<(), String> {
        let selected_key = self.selected_link().map(|link| {
            (
                link.resolved_target_path.clone(),
                link.target_path_candidate.clone(),
                link.raw_text.clone(),
            )
        });
        let report = vulcan_core::query_links_with_filter(paths, &self.note_path, filter)
            .map_err(|error| error.to_string())?;
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

    fn list_items(&self, range: std::ops::Range<usize>) -> Vec<String> {
        self.links
            .iter()
            .skip(range.start)
            .take(range.len())
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

#[derive(Debug, Clone, Default)]
struct FullTextState {
    query: String,
    /// Restricts hits, before ranking limits, to readable notes.
    read_filter: Option<PermissionFilter>,
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

        vulcan_core::search::search_vault_with_filter(
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
            self.read_filter.as_ref(),
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

    fn list_items(&self, range: std::ops::Range<usize>) -> Vec<String> {
        self.picker
            .filtered_notes()
            .skip(range.start)
            .take(range.len())
            .map(|(_, note)| crate::note_picker::note_label(note))
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

    fn list_items(&self, range: std::ops::Range<usize>) -> Vec<String> {
        self.picker
            .filtered_notes()
            .skip(range.start)
            .take(range.len())
            .map(|(_, note)| crate::note_picker::note_label(note))
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

/// The selected note's Dataview results. Restricted sessions evaluate inline
/// expressions and blocks inside the profile's readable universe.
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
    use vulcan_app::browse::move_note;

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
        fs::create_dir_all(paths.vulcan_dir()).expect(".vulcan dir should be created");
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

    /// A restricted profile browses only what it can read: lists, search,
    /// backlinks, doctor candidates, and Dataview previews leave hidden
    /// notes out, and changes need the matching grants.
    #[test]
    fn restricted_profiles_browse_only_readable_notes() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let root = temp_dir.path();
        let paths = VaultPaths::new(root);
        write_note(
            root,
            "Public/Home.md",
            "dashboard [[Public/Target]] and [[Dup]]\n\n```dataview\nLIST FROM \"\"\n```\n",
        );
        write_note(root, "Public/Target.md", "# Target");
        write_note(root, "Public/Dup.md", "# Dup");
        write_note(root, "Private/Dup.md", "# Dup");
        write_note(
            root,
            "Private/Secret.md",
            "dashboard secret [[Public/Target]]",
        );
        scan_fixture(&paths);
        fs::write(
            paths.config_file(),
            "[permissions.profiles.scoped]\nread = { allow = [\"folder:Public/**\"] }\nwrite = { allow = [\"folder:Public/**\"] }\n",
        )
        .unwrap();
        let selection = vulcan_core::resolve_permission_profile(&paths, Some("scoped")).unwrap();
        let authority = BrowseAuthority::new(
            vulcan_core::ProfilePermissionGuard::new(&paths, selection),
            Some("scoped".to_string()),
        )
        .unwrap();
        let notes = load_notes(&paths, &authority).unwrap();
        assert!(notes.iter().all(|note| note.path.starts_with("Public/")));
        assert_eq!(notes.len(), 3);
        assert!(authority.check_git().is_err());
        assert!(authority.check_create("Private/New.md").is_err());
        let mut state =
            BrowseState::new_with_authority(paths, notes, authority).expect("state should build");
        state.picker.select_path("Public/Home.md");
        state.refresh_rendered_preview_if_needed();
        state.settle_rendered_preview();
        let preview = preview_text(&state.preview_lines());
        assert!(preview.contains("Public/Target"), "{preview}");
        assert!(!preview.contains("Private"), "{preview}");

        state.handle_key(ctrl('f'));
        for character in "dashboard".chars() {
            state.handle_key(key(KeyCode::Char(character)));
        }
        assert_eq!(state.filtered_count(), 1);
        assert_eq!(state.selected_path(), Some("Public/Home.md"));
        state.switch_mode(BrowseMode::Fuzzy).unwrap();

        state.picker.select_path("Public/Target.md");
        state.handle_key(ctrl('b'));
        assert_eq!(state.filtered_count(), 1);
        assert_eq!(state.selected_path(), Some("Public/Home.md"));
        state.handle_key(key(KeyCode::Esc));

        // Doctor rows show readable candidates only, and no orphan verdict
        // that could depend on hidden links.
        let mut report = doctor_vault(&state.paths).unwrap();
        report.ambiguous_links.push(vulcan_core::DoctorLinkIssue {
            document_path: Some("Public/Home.md".to_string()),
            message: "ambiguous".to_string(),
            target: Some("Dup".to_string()),
            matches: vec!["Private/Dup.md".to_string(), "Public/Dup.md".to_string()],
        });
        report.orphan_notes.push("Public/Home.md".to_string());
        let rows = |authority: &BrowseAuthority| {
            format!(
                "{:?}",
                doctor_issues_for_note("Public/Home.md", &report, authority)
            )
        };
        let scoped = rows(&state.authority);
        assert!(scoped.contains("Public/Dup.md"), "{scoped}");
        assert!(!scoped.contains("Private"), "{scoped}");
        assert!(!scoped.contains("Orphan"), "{scoped}");
        let full = rows(&BrowseAuthority::default_for(&state.paths));
        assert!(
            full.contains("Private/Dup.md") && full.contains("Orphan"),
            "{full}"
        );
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
        assert!(state.list_items(0..usize::MAX)[0].contains("Update alpha"));
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
        assert!(state.list_items(0..usize::MAX)[0].contains("Unresolved link"));
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
    fn tui_background_completion_invalidates_without_keyboard_input() {
        let temp = TempDir::new().unwrap();
        let paths = VaultPaths::new(temp.path());
        write_note(temp.path(), "Home.md", "# Home");
        scan_fixture(&paths);
        let mut state = BrowseState::new(paths.clone(), vec![note("Home.md", &[])]).unwrap();
        let (sender, receiver) = mpsc::channel();
        state.background_refresh = Some(BackgroundRefreshState { receiver });
        let mut redraw = Redraw::default();
        let mut renders = usize::from(redraw.take());
        for _tick in 0..300 {
            if state.poll_background_refresh() {
                redraw.invalidate();
            }
            renders += usize::from(redraw.take());
        }
        assert_eq!(renders, 1);
        write_note(temp.path(), "Added.md", "# Added");
        sender
            .send(Ok(scan_vault(&paths, ScanMode::Incremental).unwrap()))
            .unwrap();
        assert!(state.poll_background_refresh());
        redraw.invalidate();
        renders += usize::from(redraw.take());
        assert_eq!(renders, 2);
        assert_eq!(state.total_notes(), 2);
        assert!(state.background_refresh.is_none());
        assert!(!state.poll_background_refresh());
        let (sender, receiver) = mpsc::channel();
        state.background_refresh = Some(BackgroundRefreshState { receiver });
        drop(sender);
        assert!(state.poll_background_refresh());
        assert!(state.status_line().contains("ended unexpectedly"));
        assert!(state.background_refresh.is_none());
        eprintln!("browse: 300 empty background polls: 1 initial frame; completion adds 1 frame without input");
    }

    #[test]
    fn tui_browse_large_list_formats_only_the_selected_window() {
        use crate::note_picker::{FILTERED_ROWS, FORMATTED_ROWS};
        let temp = TempDir::new().unwrap();
        let paths = VaultPaths::new(temp.path());
        write_note(temp.path(), "Home.md", "# Home");
        scan_fixture(&paths);
        let notes = (0..10_000)
            .map(|index| note(&format!("Note{index:05}.md"), &[]))
            .collect();
        let mut state = BrowseState::new(paths, notes).unwrap();
        state.picker.move_selection(9_999);
        FILTERED_ROWS.set(0);
        FORMATTED_ROWS.set(0);
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|frame| draw(frame, &state)).unwrap();
        assert_eq!(FORMATTED_ROWS.get(), 20); // query 3, footer 5, borders 2
        assert_eq!(FILTERED_ROWS.get(), 0);
        assert_eq!(state.selected_path(), Some("Note09999.md"));
        assert_eq!(state.list_items(9_999..10_000), vec!["Note09999.md"]);
        eprintln!("browse: 10000 matches, 100x30 terminal: formatted rows 10000 -> 20; redraw filter evaluations 0");
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
        assert_eq!(state.list_items(0..usize::MAX).len(), 1);
        assert!(state.list_items(0..usize::MAX)[0].contains("Projects/Alpha.md"));

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
        assert!(state.list_items(0..usize::MAX)[0].contains("Beta.md"));

        state.handle_key(ctrl('s'));
        assert_eq!(state.full_text.sort(), SearchSort::ModifiedNewest);
        assert!(state.list_items(0..usize::MAX)[0].contains("Beta.md"));
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
        assert!(calendar.contains("+ note  * events  _ today"));
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
    fn notes_preview_rendered_and_ctrl_v_shows_their_source() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        write_note(
            temp_dir.path(),
            "Dashboard.md",
            "---\nstatus: draft\n---\n\nStatus: `= this.status`\n\n```dataview\nLIST FROM #project\n```\n",
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
        state.refresh_rendered_preview_if_needed();
        state.settle_rendered_preview();

        assert_eq!(state.preview_title(), "Rendered: Dashboard.md");
        assert!(state.status_bar_line().contains("Preview: rendered"));
        let preview = preview_text(&state.preview_lines());
        assert!(preview.contains("Status: draft"), "{preview}");
        assert!(preview.contains("Projects/Alpha"), "{preview}");
        assert!(!preview.contains("```"), "{preview}");

        let action = state.handle_key(ctrl('v'));

        assert_eq!(action, BrowseAction::Continue);
        assert_eq!(state.status_line(), "Preview mode: source.");
        assert_eq!(state.preview_title(), "Preview: Dashboard.md");
        assert!(preview_text(&state.preview_lines()).contains("`= this.status`"));

        state.handle_key(ctrl('v'));
        state.settle_rendered_preview();
        assert_eq!(state.status_line(), "Preview mode: rendered.");
        assert_eq!(state.preview_title(), "Rendered: Dashboard.md");
    }

    #[cfg(feature = "js_runtime")]
    #[test]
    fn rendered_preview_runs_dataviewjs() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        write_note(
            temp_dir.path(),
            "Dashboard.md",
            "```dataviewjs\ndv.paragraph('from the js block')\n```\n",
        );
        scan_fixture(&paths);
        let mut state =
            BrowseState::new(paths, vec![note("Dashboard.md", &[])]).expect("state should build");
        state.picker.select_path("Dashboard.md");
        state.refresh_rendered_preview_if_needed();
        state.settle_rendered_preview();

        let preview = preview_text(&state.preview_lines());
        assert!(preview.contains("from the js block"), "{preview}");
    }

    #[test]
    fn rendered_preview_refreshes_when_selected_note_changes() {
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
        state.refresh_rendered_preview_if_needed();
        state.settle_rendered_preview();

        assert!(preview_text(&state.preview_lines()).contains("draft"));

        state.picker.select_path("Beta.md");
        state.refresh_rendered_preview_if_needed();
        state.settle_rendered_preview();

        assert_eq!(state.preview_title(), "Rendered: Beta.md");
        assert_eq!(preview_text(&state.preview_lines()), "Beta");
    }

    #[test]
    fn rendered_preview_shows_the_source_until_evaluation_finishes() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        write_note(
            temp_dir.path(),
            "Alpha.md",
            "---\nstatus: draft\n---\n\nStatus: `= this.status`\n",
        );
        scan_fixture(&paths);
        let mut state =
            BrowseState::new(paths, vec![note("Alpha.md", &[])]).expect("state should build");
        state.picker.select_path("Alpha.md");
        state.refresh_rendered_preview_if_needed();

        assert!(state.rendered_preview.pending);
        assert_eq!(state.preview_title(), "Rendered: Alpha.md (evaluating…)");
        assert!(preview_text(&state.preview_lines()).contains("this.status"));

        state.settle_rendered_preview();
        assert!(!state.rendered_preview.pending);
        assert_eq!(state.preview_title(), "Rendered: Alpha.md");
        assert!(preview_text(&state.preview_lines()).contains("Status: draft"));
    }

    #[test]
    fn source_preview_shows_the_whole_file_and_scrolls() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        let long_note = (1..=40)
            .map(|line| format!("Line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        write_note(temp_dir.path(), "Long.md", &long_note);
        scan_fixture(&paths);
        let mut state =
            BrowseState::new(paths, vec![note("Long.md", &[])]).expect("state should build");
        state.picker.select_path("Long.md");
        state.refresh_rendered_preview_if_needed();
        state.handle_key(ctrl('v'));

        let preview = state.preview_lines();
        assert_eq!(preview.len(), 40);
        assert_eq!(preview_text(&preview[39..]), "40 \nLine 40");
        state.handle_key(key(KeyCode::PageDown));
        assert_eq!(state.preview_scroll(), 10);
    }

    #[test]
    fn revisited_notes_reuse_their_evaluated_preview_until_they_change() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        write_note(
            temp_dir.path(),
            "Alpha.md",
            "---\nstatus: draft\n---\n\nStatus: `= this.status`\n",
        );
        write_note(temp_dir.path(), "Beta.md", "# Beta\n");
        scan_fixture(&paths);
        let mut state = BrowseState::new(
            paths.clone(),
            vec![note("Alpha.md", &[]), note("Beta.md", &[])],
        )
        .expect("state should build");
        state.picker.select_path("Alpha.md");
        state.refresh_rendered_preview_if_needed();
        state.settle_rendered_preview();
        state.picker.select_path("Beta.md");
        state.refresh_rendered_preview_if_needed();
        state.settle_rendered_preview();

        state.picker.select_path("Alpha.md");
        state.refresh_rendered_preview_if_needed();
        assert!(!state.rendered_preview.pending);
        assert!(preview_text(&state.preview_lines()).contains("Status: draft"));

        // A changed source is evaluated again.
        write_note(
            temp_dir.path(),
            "Alpha.md",
            "---\nstatus: draft\n---\n\nNow: `= this.status`\n",
        );
        state.picker.select_path("Beta.md");
        state.refresh_rendered_preview_if_needed();
        state.settle_rendered_preview();
        state.picker.select_path("Alpha.md");
        state.refresh_rendered_preview_if_needed();
        assert!(state.rendered_preview.pending);
    }

    #[test]
    fn tab_selects_preview_links_and_enter_follows_them() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        write_note(
            temp_dir.path(),
            "Home.md",
            "See [[Target]] and [the site](https://example.test).\n",
        );
        write_note(temp_dir.path(), "Target.md", "# Target\n");
        scan_fixture(&paths);
        let mut state = BrowseState::new(paths, vec![note("Home.md", &[]), note("Target.md", &[])])
            .expect("state should build");
        state.picker.select_path("Home.md");
        state.refresh_rendered_preview_if_needed();
        state.settle_rendered_preview();
        let _ = state.preview_lines();

        state.handle_key(key(KeyCode::Tab));
        assert_eq!(
            state.status_line(),
            "Link 1/2: Enter follows it, Esc clears the selection."
        );
        let highlighted = state
            .preview_lines()
            .iter()
            .flat_map(|line| line.spans.iter())
            .filter(|span| span.style.add_modifier.contains(Modifier::REVERSED))
            .map(|span| span.content.to_string())
            .collect::<String>();
        assert_eq!(highlighted, "Target");

        // Shift-Tab wraps back from the first link to the last.
        state.handle_key(key(KeyCode::BackTab));
        assert!(state.status_line().starts_with("Link 2/2"));
        state.handle_key(key(KeyCode::Tab));

        assert_eq!(
            state.handle_key(key(KeyCode::Enter)),
            BrowseAction::Continue
        );
        assert_eq!(state.selected_path(), Some("Target.md"));
        assert_eq!(state.status_line(), "Opened Target.md.");
    }

    #[test]
    fn esc_clears_a_link_selection_before_quitting() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        write_note(temp_dir.path(), "Home.md", "See [[Target]].\n");
        write_note(temp_dir.path(), "Target.md", "# Target\n");
        scan_fixture(&paths);
        let mut state = BrowseState::new(paths, vec![note("Home.md", &[]), note("Target.md", &[])])
            .expect("state should build");
        state.picker.select_path("Home.md");
        state.refresh_rendered_preview_if_needed();
        state.settle_rendered_preview();
        let _ = state.preview_lines();

        state.handle_key(key(KeyCode::Tab));
        assert_eq!(state.handle_key(key(KeyCode::Esc)), BrowseAction::Continue);
        assert_eq!(state.rendered_preview.selected_link, None);
        // Without a selection Enter edits again and Esc quits.
        assert_eq!(state.handle_key(key(KeyCode::Esc)), BrowseAction::Quit);
    }

    #[test]
    fn links_to_unreadable_notes_are_not_followed() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let root = temp_dir.path();
        let paths = VaultPaths::new(root);
        write_note(root, "Public/Home.md", "See [[Private/Secret]].\n");
        write_note(root, "Private/Secret.md", "# Secret\n");
        scan_fixture(&paths);
        fs::write(
            paths.config_file(),
            "[permissions.profiles.scoped]\nread = { allow = [\"folder:Public/**\"] }\nwrite = { allow = [] }\n",
        )
        .unwrap();
        let selection = vulcan_core::resolve_permission_profile(&paths, Some("scoped")).unwrap();
        let authority = BrowseAuthority::new(
            vulcan_core::ProfilePermissionGuard::new(&paths, selection),
            Some("scoped".to_string()),
        )
        .unwrap();
        let notes = load_notes(&paths, &authority).unwrap();
        let mut state =
            BrowseState::new_with_authority(paths, notes, authority).expect("state should build");
        state.picker.select_path("Public/Home.md");
        state.refresh_rendered_preview_if_needed();
        state.settle_rendered_preview();
        let _ = state.preview_lines();

        state.handle_key(key(KeyCode::Tab));
        state.handle_key(key(KeyCode::Enter));

        assert_eq!(state.selected_path(), Some("Public/Home.md"));
        assert_eq!(
            state.status_line(),
            "No readable note matches `Private/Secret`."
        );
    }

    fn write_png(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        image::RgbaImage::from_pixel(100, 100, image::Rgba([200, 30, 30, 255]))
            .save(path)
            .expect("png should be written");
    }

    fn halfblock_rows(lines: &[Line<'static>]) -> usize {
        lines
            .iter()
            .filter(|line| line.spans.iter().any(|span| span.content == "▀"))
            .count()
    }

    #[test]
    fn images_render_below_their_line_and_links_stay_selectable() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let root = temp_dir.path();
        let paths = VaultPaths::new(root);
        write_note(
            root,
            "Home.md",
            "Intro\n\n![[pic.png]]\n\nSee [[Target]].\n",
        );
        write_note(root, "Target.md", "# Target\n");
        // Found by name, as Obsidian does, though it lives in a subfolder.
        write_png(&root.join("assets/pic.png"));
        scan_fixture(&paths);
        let mut state = BrowseState::new(paths, vec![note("Home.md", &[]), note("Target.md", &[])])
            .expect("state should build");
        state.picker.select_path("Home.md");
        state.refresh_rendered_preview_if_needed();
        state.settle_rendered_preview();

        let lines = state.preview_lines();
        // 100x100 pixels, enlarged three times, at the half-block cell size
        // of 10x20 pixels: 30 columns by 15 rows.
        assert_eq!(halfblock_rows(&lines), 15, "{lines:?}");
        assert!(lines
            .iter()
            .filter(|line| line.spans.iter().any(|span| span.content == "▀"))
            .all(|line| line.spans.len() == 30));
        // The image takes the place of its `[image: ...]` line.
        assert!(!preview_text(&lines).contains("[image:"));

        state.handle_key(key(KeyCode::Tab));
        let highlighted = state
            .preview_lines()
            .iter()
            .flat_map(|line| line.spans.iter())
            .filter(|span| span.style.add_modifier.contains(Modifier::REVERSED))
            .map(|span| span.content.to_string())
            .collect::<String>();
        assert_eq!(highlighted, "Target");
    }

    #[test]
    fn graphics_protocol_images_reserve_rows_and_draw_over_them() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let root = temp_dir.path();
        let paths = VaultPaths::new(root);
        write_note(root, "Home.md", "![[pic.png]]\n\nSee [[Target]].\n");
        write_note(root, "Target.md", "# Target\n");
        // A 40x60 pixel image.
        let pic = root.join("pic.png");
        image::RgbaImage::from_pixel(40, 60, image::Rgba([10, 120, 200, 255]))
            .save(&pic)
            .expect("png should be written");
        scan_fixture(&paths);
        let mut state = BrowseState::new(paths, vec![note("Home.md", &[]), note("Target.md", &[])])
            .expect("state should build");
        let mut picker = Picker::halfblocks();
        picker.set_protocol_type(ProtocolType::Kitty);
        state.image_mode = ImageMode::graphics(picker);
        state.picker.select_path("Home.md");
        state.refresh_rendered_preview_if_needed();
        state.settle_rendered_preview();

        let lines = state.preview_lines();
        assert_eq!(halfblock_rows(&lines), 0);
        {
            let layout = state.rendered_preview.layout.borrow();
            let placed = &layout.as_ref().expect("layout").images;
            assert_eq!(placed.len(), 1);
            // 40x60 pixels enlarged three times at a 10x20 font: 12 columns
            // by 9 rows.
            assert_eq!((placed[0].columns, placed[0].rows), (12, 9));
            assert_eq!(placed[0].path, "pic.png");
        }
        // Links after the image keep their place.
        state.handle_key(key(KeyCode::Tab));
        assert!(state.status_line().starts_with("Link 1/1"));

        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(120, 40))
            .expect("test terminal should build");
        terminal
            .draw(|frame| draw(frame, &state))
            .expect("browse should draw");
        let buffer = terminal.backend().buffer();
        // The image is drawn exactly on its reserved rows: its first line
        // is the preview's first line, right under the border.
        let title_row = (0..buffer.area.height)
            .find(|&y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
                    .contains("Rendered: Home.md")
            })
            .expect("preview title should be drawn");
        let image_row = (0..buffer.area.height)
            .find(|&y| (0..buffer.area.width).any(|x| buffer[(x, y)].symbol().contains('\u{1b}')))
            .expect("the kitty protocol should transmit the image");
        assert_eq!(image_row, title_row + 1);
        assert!(!preview_text(&state.preview_lines()).contains("[image:"));
        assert!(
            buffer
                .content()
                .iter()
                .any(|cell| cell.symbol().contains('\u{1b}')),
            "the kitty protocol should transmit the image"
        );
    }

    #[test]
    fn images_outside_the_profile_are_not_shown() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let root = temp_dir.path();
        let paths = VaultPaths::new(root);
        write_note(root, "Public/Home.md", "![[secret.png]]\n");
        write_png(&root.join("Private/secret.png"));
        scan_fixture(&paths);
        fs::write(
            paths.config_file(),
            "[permissions.profiles.scoped]\nread = { allow = [\"folder:Public/**\"] }\nwrite = { allow = [] }\n",
        )
        .unwrap();
        let selection = vulcan_core::resolve_permission_profile(&paths, Some("scoped")).unwrap();
        let authority = BrowseAuthority::new(
            vulcan_core::ProfilePermissionGuard::new(&paths, selection),
            Some("scoped".to_string()),
        )
        .unwrap();
        let notes = load_notes(&paths, &authority).unwrap();
        let mut state =
            BrowseState::new_with_authority(paths, notes, authority).expect("state should build");
        state.picker.select_path("Public/Home.md");
        state.refresh_rendered_preview_if_needed();
        state.settle_rendered_preview();

        let preview = preview_text(&state.preview_lines());
        assert_eq!(halfblock_rows(&state.preview_lines()), 0);
        assert!(
            preview.contains("image not shown: no such image in the vault"),
            "{preview}"
        );
    }

    #[test]
    fn images_that_cannot_be_shown_say_why() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let root = temp_dir.path();
        let paths = VaultPaths::new(root);
        write_note(
            root,
            "Home.md",
            "![](https://example.test/pic.png)\n\n![[missing.png]]\n\n![[clip.mp4]]\n",
        );
        scan_fixture(&paths);
        let mut state =
            BrowseState::new(paths, vec![note("Home.md", &[])]).expect("state should build");
        state.picker.select_path("Home.md");
        state.refresh_rendered_preview_if_needed();
        state.settle_rendered_preview();

        let preview = preview_text(&state.preview_lines());
        for reason in [
            "image not shown: remote images are not loaded",
            "image not shown: no such image in the vault",
            "image not shown: only PNG, JPEG, GIF, WebP, and BMP images are shown",
        ] {
            assert!(preview.contains(reason), "{reason}: {preview}");
        }
        assert!(state.status_bar_line().contains("Images: half-blocks"));
    }

    #[test]
    fn image_paths_cannot_leave_the_vault() {
        assert_eq!(
            normalize_vault_relative(Path::new("Notes/../assets/./pic.png")).as_deref(),
            Some("assets/pic.png")
        );
        assert_eq!(normalize_vault_relative(Path::new("../outside.png")), None);
        assert_eq!(normalize_vault_relative(Path::new("/etc/passwd.png")), None);
    }

    #[test]
    fn page_keys_scroll_the_rendered_preview() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        let long_note = (1..=40)
            .map(|line| format!("Line {line}"))
            .collect::<Vec<_>>()
            .join("\n\n");
        write_note(temp_dir.path(), "Long.md", &long_note);
        scan_fixture(&paths);
        let mut state =
            BrowseState::new(paths, vec![note("Long.md", &[])]).expect("state should build");
        state.picker.select_path("Long.md");
        state.refresh_rendered_preview_if_needed();
        let _ = state.preview_lines();

        state.handle_key(key(KeyCode::PageDown));
        assert_eq!(state.preview_scroll(), 10);
        state.handle_key(key(KeyCode::PageUp));
        state.handle_key(key(KeyCode::PageUp));
        assert_eq!(state.preview_scroll(), 0);
    }

    #[test]
    fn ctrl_v_switches_full_text_preview_from_snippet_to_rendered_note() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        write_note(
            temp_dir.path(),
            "Dashboard.md",
            "---\nstatus: draft\n---\n\nrelease dashboard\n\nStatus: `= this.status`\n",
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
        state.settle_rendered_preview();

        assert_eq!(state.preview_title(), "Rendered: Dashboard.md");
        assert!(preview_text(&state.preview_lines()).contains("Status: draft"));
    }

    #[test]
    fn highlighted_snippet_parser_marks_bracketed_terms() {
        let line = highlighted_snippet_line("alpha [dashboard] omega");

        assert_eq!(line.spans.len(), 3);
        assert_eq!(line.spans[1].content, "dashboard");
    }
}
