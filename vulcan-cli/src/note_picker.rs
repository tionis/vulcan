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
    let _restore = TerminalRestore;
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
    run_picker_events(terminal, state, event::read)
}

fn run_picker_events<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    state: &mut NotePickerState,
    mut read: impl FnMut() -> io::Result<Event>,
) -> Result<Option<String>, io::Error> {
    let mut redraw = Redraw::default();
    loop {
        if redraw.take() {
            terminal
                .draw(|frame| draw(frame, state))
                .map_err(|error| io::Error::other(error.to_string()))?;
        }
        let event = read()?;
        redraw.event(&event);
        if let Event::Key(key) = event {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match handle_picker_key(state, key.code) {
                PickerAction::Continue => {}
                PickerAction::Cancel => return Ok(None),
                PickerAction::Select => {
                    return Ok(state.selected_note().map(|note| note.path.clone()));
                }
            }
        }
    }
}

/// Best-effort cleanup also covers setup failures and early error returns.
pub(crate) struct TerminalRestore;

impl Drop for TerminalRestore {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, crossterm::cursor::Show);
    }
}

/// Shared terminal invalidation. There are no animation/status-expiry timers in
/// these screens. Key releases and unrelated terminal events need no frame.
pub(crate) struct Redraw(bool);

impl Default for Redraw {
    fn default() -> Self {
        Self(true)
    }
}

impl Redraw {
    pub(crate) fn invalidate(&mut self) {
        self.0 = true;
    }

    pub(crate) fn event(&mut self, event: &Event) {
        if matches!(
            event,
            Event::Resize(..)
                | Event::Key(crossterm::event::KeyEvent {
                    kind: KeyEventKind::Press,
                    ..
                })
        ) {
            self.invalidate();
        }
    }

    pub(crate) fn take(&mut self) -> bool {
        std::mem::take(&mut self.0)
    }
}

/// Keep selection visible without allocating or formatting off-screen rows.
/// The offset survives redraws; resizing and shrinking results clamp it safely.
#[derive(Debug, Clone, Default)]
pub(crate) struct Viewport(std::cell::Cell<usize>);

#[cfg(test)]
thread_local! {
    pub(crate) static FILTERED_ROWS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(crate) static FORMATTED_ROWS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

pub(crate) fn note_label(note: &NoteIdentity) -> String {
    #[cfg(test)]
    FORMATTED_ROWS.with(|count| count.set(count.get() + 1));
    let aliases = if note.aliases.is_empty() {
        String::new()
    } else {
        format!(" [{}]", note.aliases.join(", "))
    };
    format!("{}{}", note.path, aliases)
}

impl Viewport {
    pub(crate) fn range(
        &self,
        len: usize,
        selected: Option<usize>,
        height: usize,
    ) -> std::ops::Range<usize> {
        let mut start = self.0.get().min(len.saturating_sub(height));
        if let Some(selected) = selected.filter(|index| *index < len) {
            if selected < start {
                start = selected;
            } else if selected >= start.saturating_add(height) {
                start = selected.saturating_add(1).saturating_sub(height);
            }
        }
        self.0.set(start);
        start..start.saturating_add(height).min(len)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PickerAction {
    Continue,
    Cancel,
    Select,
}

pub(crate) fn handle_picker_key(state: &mut NotePickerState, code: KeyCode) -> PickerAction {
    match code {
        KeyCode::Esc => PickerAction::Cancel,
        KeyCode::Enter => PickerAction::Select,
        KeyCode::Up => {
            state.move_selection(-1);
            PickerAction::Continue
        }
        KeyCode::Down => {
            state.move_selection(1);
            PickerAction::Continue
        }
        KeyCode::Backspace => {
            state.pop_query();
            PickerAction::Continue
        }
        KeyCode::Char(character) => {
            state.push_query(character);
            PickerAction::Continue
        }
        _ => PickerAction::Continue,
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

    let range = state.viewport.range(
        state.filtered_count(),
        state.selected_index,
        usize::from(body[0].height.saturating_sub(2)),
    );
    let items = state
        .filtered_notes()
        .skip(range.start)
        .take(range.len())
        .map(|(_, note)| ListItem::new(note_label(note)))
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
    list_state.select(
        state
            .selected_index
            .and_then(|index| range.contains(&index).then(|| index - range.start)),
    );
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
        Line::from("Keys: Enter select, Esc cancel, Up/Down move"),
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
pub(crate) struct NotePickerState {
    paths: VaultPaths,
    notes: Vec<NoteIdentity>,
    query: String,
    selected_index: Option<usize>,
    preview: Vec<String>,
    filtered: Vec<(i32, usize)>,
    viewport: Viewport,
}

impl NotePickerState {
    pub(crate) fn new(paths: VaultPaths, notes: Vec<NoteIdentity>, query: &str) -> Self {
        let mut state = Self {
            paths,
            notes,
            query: query.to_string(),
            selected_index: None,
            preview: vec!["No notes available.".to_string()],
            filtered: Vec::new(),
            viewport: Viewport::default(),
        };
        state.refilter();
        state.clamp_selection();
        state
    }

    pub(crate) fn filtered_notes(&self) -> impl ExactSizeIterator<Item = (i32, &NoteIdentity)> {
        self.filtered
            .iter()
            .map(|(score, index)| (*score, &self.notes[*index]))
    }

    fn refilter(&mut self) {
        #[cfg(test)]
        FILTERED_ROWS.with(|count| count.set(count.get() + self.notes.len()));
        let mut filtered = self
            .notes
            .iter()
            .enumerate()
            .filter_map(|(index, note)| fuzzy_score(note, &self.query).map(|score| (score, index)))
            .collect::<Vec<_>>();
        filtered.sort_by(|(left_score, left), (right_score, right)| {
            right_score
                .cmp(left_score)
                .then_with(|| self.notes[*left].path.cmp(&self.notes[*right].path))
        });
        self.filtered = filtered;
    }

    pub(crate) fn selected_index(&self) -> Option<usize> {
        self.selected_index
    }

    pub(crate) fn query(&self) -> &str {
        &self.query
    }

    pub(crate) fn set_query(&mut self, query: &str) {
        self.query = query.to_string();
        self.refilter();
        self.clamp_selection();
    }

    pub(crate) fn total_notes(&self) -> usize {
        self.notes.len()
    }

    pub(crate) fn filtered_count(&self) -> usize {
        self.filtered_notes().len()
    }

    pub(crate) fn selected_path(&self) -> Option<&str> {
        self.selected_note().map(|note| note.path.as_str())
    }

    fn selected_note(&self) -> Option<&NoteIdentity> {
        self.selected_index.and_then(|index| {
            self.filtered
                .get(index)
                .map(|(_, index)| &self.notes[*index])
        })
    }

    pub(crate) fn move_selection(&mut self, delta: isize) {
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
        self.refilter();
        self.clamp_selection();
    }

    fn pop_query(&mut self) {
        self.query.pop();
        self.refilter();
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

    pub(crate) fn replace_notes_preserve_selection(&mut self, notes: Vec<NoteIdentity>) {
        let selected_path = self.selected_note().map(|note| note.path.clone());
        self.notes = notes;
        self.refilter();
        self.selected_index = selected_path.and_then(|selected_path| {
            self.filtered_notes()
                .position(|(_, note)| note.path == selected_path)
        });
        if self.selected_index.is_none() {
            self.clamp_selection();
        } else {
            self.refresh_preview();
        }
    }

    pub(crate) fn select_path(&mut self, path: &str) {
        let index = self
            .filtered_notes()
            .position(|(_, note)| note.path == path);
        if let Some(index) = index {
            self.selected_index = Some(index);
            self.refresh_preview();
        }
    }

    pub(crate) fn refresh_preview(&mut self) {
        self.preview = self.selected_note().map_or_else(
            || vec!["No matches.".to_string()],
            |note| load_preview(&self.paths, &note.path),
        );
    }

    pub(crate) fn preview_lines(&self) -> Vec<Line<'static>> {
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
    use tempfile::TempDir;

    /// Manual PTY harness: run this ignored test with `VULCAN_TUI_SMOKE` set to
    /// picker, bases or browse. Uses only a temporary synthetic vault.
    #[test]
    #[ignore = "requires a controlling terminal; used for idle CPU and editor restoration smoke checks"]
    fn tui_pty_smoke() {
        let temp = TempDir::new().unwrap();
        fs::write(
            temp.path().join("Alpha.md"),
            "# Alpha\n\nA synthetic note.\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("All.base"),
            "views:\n  - type: table\n    name: All\n",
        )
        .unwrap();
        let paths = VaultPaths::new(temp.path());
        fs::create_dir_all(paths.vulcan_dir()).unwrap();
        vulcan_core::scan_vault(&paths, vulcan_core::ScanMode::Full).unwrap();
        match std::env::var("VULCAN_TUI_SMOKE").as_deref() {
            Ok("bases") => {
                let report = vulcan_core::evaluate_base_file(&paths, "All.base").unwrap();
                crate::bases_tui::run_bases_tui(&paths, "All.base", &report).unwrap();
            }
            Ok("browse") => {
                crate::browse_tui::run_browse_tui(&paths, vulcan_core::AutoScanMode::Off, true)
                    .unwrap();
            }
            _ => {
                pick_note(&paths, None, None).unwrap();
            }
        }
    }

    #[test]
    fn tui_idle_invalidation_and_viewport_boundaries() {
        let mut redraw = Redraw::default();
        let mut renders = usize::from(redraw.take());
        // Sixty seconds at the old 200ms interval, with a synthetic clock.
        for _tick in 0..300 {
            renders += usize::from(redraw.take());
        }
        assert_eq!(renders, 1);
        redraw.event(&Event::Resize(80, 24));
        assert!(redraw.take());
        redraw.invalidate(); // background completion / editor return
        assert!(redraw.take());
        assert!(!redraw.take());

        let viewport = Viewport::default();
        assert_eq!(viewport.range(10_000, Some(0), 20), 0..20);
        assert_eq!(viewport.range(10_000, Some(500), 20), 481..501);
        assert_eq!(viewport.range(10_000, Some(499), 20), 481..501);
        assert_eq!(viewport.range(10_000, Some(480), 20), 480..500);
        assert_eq!(viewport.range(10_000, Some(9_999), 40), 9_960..10_000);
        assert_eq!(viewport.range(2, Some(1), 20), 0..2);
        assert!(viewport.range(2, Some(1), 0).is_empty());
        assert_eq!(viewport.range(0, None, 20), 0..0);
        eprintln!("idle model: 60s/300 old ticks, frames 301 -> 1");
    }

    #[test]
    fn tui_picker_injected_events_render_only_invalidated_frames() {
        use crossterm::event::{KeyEvent, KeyModifiers};
        let temp = TempDir::new().unwrap();
        let mut state = NotePickerState::new(
            VaultPaths::new(temp.path()),
            vec![note("Alpha.md", &[])],
            "",
        );
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
        let release = Event::Key(KeyEvent::new_with_kind(
            KeyCode::Char('z'),
            KeyModifiers::NONE,
            KeyEventKind::Release,
        ));
        let mut events = std::iter::repeat_n(release, 300).chain([
            Event::Resize(100, 30),
            Event::Key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE)),
            Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        ]);
        assert_eq!(
            run_picker_events(&mut terminal, &mut state, || Ok(events.next().unwrap())).unwrap(),
            None
        );
        assert_eq!(terminal.get_frame().count(), 3);
        assert_eq!(state.query(), "a");
        let error = run_picker_events(&mut terminal, &mut state, || {
            Err(io::Error::other("injected input failure"))
        })
        .unwrap_err();
        assert!(error.to_string().contains("injected input failure"));
    }

    #[test]
    fn tui_picker_large_list_caches_filter_and_formats_viewport() {
        let temp = TempDir::new().unwrap();
        let notes = (0..10_000)
            .map(|index| note(&format!("Note{index:05}.md"), &["alias"]))
            .collect();
        let mut state = NotePickerState::new(VaultPaths::new(temp.path()), notes, "");
        state.move_selection(9_999);
        FILTERED_ROWS.set(0);
        FORMATTED_ROWS.set(0);
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
        terminal.draw(|frame| draw(frame, &state)).unwrap();
        assert_eq!(FILTERED_ROWS.get(), 0);
        assert_eq!(FORMATTED_ROWS.get(), 21); // 30 - query 3 - footer 4 - borders 2
        assert_eq!(state.selected_path(), Some("Note09999.md"));
        state.set_query("note09999");
        assert_eq!(FILTERED_ROWS.get(), 10_000);
        // Fuzzy subsequence matches remain eligible; the exact match ranks first.
        assert_eq!(
            state.filtered_notes().next().unwrap().1.path,
            "Note09999.md"
        );
        assert!(state.filtered_count() < state.total_notes());
        state.replace_notes_preserve_selection(vec![
            note("Note09999.md", &[]),
            note("Other.md", &[]),
        ]);
        assert_eq!(state.selected_path(), Some("Note09999.md"));
        state.set_query("no matches here");
        assert_eq!(state.selected_path(), None);
        terminal.draw(|frame| draw(frame, &state)).unwrap();
        eprintln!("picker: 10000 matches, 100x30 terminal: formatted rows 10000 -> 21; cached redraw filter evaluations 0");
    }

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
    fn char_q_updates_query_instead_of_cancelling() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let state_paths = VaultPaths::new(temp_dir.path());
        let mut state = NotePickerState::new(
            state_paths,
            vec![NoteIdentity {
                path: "Alpha.md".to_string(),
                filename: "Alpha".to_string(),
                aliases: vec!["Start".to_string()],
            }],
            "",
        );

        let action = handle_picker_key(&mut state, KeyCode::Char('q'));

        assert_eq!(action, PickerAction::Continue);
        assert_eq!(state.query, "q");
    }

    #[test]
    fn char_j_and_k_update_query_instead_of_moving() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let state_paths = VaultPaths::new(temp_dir.path());
        let mut state = NotePickerState::new(
            state_paths,
            vec![
                note("Alpha.md", &[]),
                note("Jekyll.md", &[]),
                note("Kappa.md", &[]),
            ],
            "",
        );

        let first = handle_picker_key(&mut state, KeyCode::Char('j'));
        let second = handle_picker_key(&mut state, KeyCode::Char('k'));

        assert_eq!(first, PickerAction::Continue);
        assert_eq!(second, PickerAction::Continue);
        assert_eq!(state.query, "jk");
    }

    #[test]
    fn escape_cancels_picker() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let state_paths = VaultPaths::new(temp_dir.path());
        let mut state = NotePickerState::new(state_paths, Vec::new(), "");

        let action = handle_picker_key(&mut state, KeyCode::Esc);

        assert_eq!(action, PickerAction::Cancel);
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
