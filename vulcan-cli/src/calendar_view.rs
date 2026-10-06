//! Month-grid model of configured daily notes, shared by the browse TUI's
//! calendar mode and the standalone `daily calendar` picker.

use crossterm::event::KeyCode;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use std::collections::BTreeMap;
use vulcan_app::browse::{list_daily_note_events, load_periodic_config};
use vulcan_core::{expected_periodic_note_path, PeriodicConfig, PeriodicStartOfWeek, VaultPaths};

pub(crate) const CALENDAR_EVENT_PREVIEW_LIMIT: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CalendarDate {
    year: i64,
    month: i64,
    day: i64,
}

impl CalendarDate {
    pub(crate) fn iso_string(self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct CalendarDayCell {
    date: CalendarDate,
    path: Option<String>,
    expected_path: Option<String>,
    events: Vec<vulcan_core::PeriodicEvent>,
    in_month: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct CalendarViewState {
    query: String,
    config: PeriodicConfig,
    start_of_week: PeriodicStartOfWeek,
    visible_month: CalendarDate,
    selected_date: CalendarDate,
    today: CalendarDate,
    cells: Vec<CalendarDayCell>,
}

impl CalendarViewState {
    pub(crate) fn new(paths: &VaultPaths) -> Result<Self, String> {
        let today = crate::commands::periodic::current_local_date_string();
        Self::new_at(paths, &today, &today)
    }

    /// Build a calendar with `today` highlighted and `selected` (`YYYY-MM-DD`
    /// or `YYYY-MM`) as the initial selection.
    pub(crate) fn new_at(paths: &VaultPaths, today: &str, selected: &str) -> Result<Self, String> {
        let today = parse_calendar_date(today)
            .ok_or_else(|| format!("invalid calendar date for today: {today}"))?;
        let selected_date = parse_calendar_date(selected)
            .or_else(|| parse_calendar_month(selected))
            .ok_or_else(|| format!("invalid calendar date: {selected}"))?;
        let config = load_periodic_config(paths);
        let start_of_week = calendar_start_of_week(&config);
        let mut state = Self {
            query: String::new(),
            config,
            start_of_week,
            visible_month: calendar_month_start(selected_date),
            selected_date,
            today,
            cells: Vec::new(),
        };
        state.refresh(paths)?;
        Ok(state)
    }

    pub(crate) fn refresh(&mut self, paths: &VaultPaths) -> Result<(), String> {
        self.config = load_periodic_config(paths);
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

    pub(crate) fn handle_key(&mut self, paths: &VaultPaths, code: KeyCode) -> Result<(), String> {
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
            KeyCode::PageUp | KeyCode::Char('[') => {
                self.query.clear();
                self.selected_date = shift_calendar_month(self.selected_date, -1);
                self.refresh(paths)
            }
            KeyCode::PageDown | KeyCode::Char(']') => {
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
            KeyCode::Char('t') => {
                self.query.clear();
                self.selected_date = self.today;
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

    pub(crate) fn apply_query(&mut self) {
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

    pub(crate) fn selected_date_iso(&self) -> String {
        self.selected_date.iso_string()
    }

    pub(crate) fn selected_events(&self) -> &[vulcan_core::PeriodicEvent] {
        self.selected_cell()
            .map_or(&[], |cell| cell.events.as_slice())
    }

    pub(crate) fn query(&self) -> &str {
        &self.query
    }

    pub(crate) fn title(&self) -> String {
        format!("Calendar ({})", calendar_month_label(self.visible_month))
    }

    pub(crate) fn list_items(&self, range: std::ops::Range<usize>) -> Vec<String> {
        self.cells
            .iter()
            .filter(|cell| cell.in_month && cell.path.is_some())
            .skip(range.start)
            .take(range.len())
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

    pub(crate) fn selected_index(&self) -> Option<usize> {
        self.cells
            .iter()
            .position(|cell| cell.date == self.selected_date)
    }

    pub(crate) fn selected_existing_path(&self) -> Option<&str> {
        self.selected_cell().and_then(|cell| cell.path.as_deref())
    }

    pub(crate) fn target_path(&self) -> Option<String> {
        self.selected_cell()
            .and_then(|cell| cell.path.clone().or_else(|| cell.expected_path.clone()))
    }

    pub(crate) fn preview_title(&self) -> String {
        format!("Calendar: {}", self.selected_date.iso_string())
    }

    pub(crate) fn preview_lines(&self) -> Vec<Line<'static>> {
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

    pub(crate) fn filtered_count(&self) -> usize {
        self.cells
            .iter()
            .filter(|cell| cell.in_month && cell.path.is_some())
            .count()
    }

    pub(crate) fn calendar_lines(&self) -> Vec<Line<'static>> {
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
                if cell.date == self.today {
                    style = style.add_modifier(Modifier::UNDERLINED);
                }
                if cell.date == self.selected_date {
                    style = style.bg(Color::DarkGray).add_modifier(Modifier::BOLD);
                }
                spans.push(Span::styled(text, style));
            }
            lines.push(Line::from(spans));
        }

        lines.push(Line::from("+ note  * events  _ today"));
        lines
    }

    pub(crate) fn selected_cell(&self) -> Option<&CalendarDayCell> {
        self.cells
            .iter()
            .find(|cell| cell.date == self.selected_date)
    }
}

pub(crate) fn calendar_start_of_week(config: &PeriodicConfig) -> PeriodicStartOfWeek {
    config
        .note("weekly")
        .map_or(PeriodicStartOfWeek::Monday, |weekly| weekly.start_of_week)
}

pub(crate) fn calendar_weekday_headers(start_of_week: PeriodicStartOfWeek) -> [&'static str; 7] {
    match start_of_week {
        PeriodicStartOfWeek::Monday => ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"],
        PeriodicStartOfWeek::Sunday => ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"],
        PeriodicStartOfWeek::Saturday => ["Sat", "Sun", "Mon", "Tue", "Wed", "Thu", "Fri"],
    }
}

pub(crate) fn calendar_month_label(date: CalendarDate) -> String {
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

pub(crate) fn parse_calendar_month(value: &str) -> Option<CalendarDate> {
    let mut parts = value.split('-');
    let year = parts.next()?.parse().ok()?;
    let month = parts.next()?.parse().ok()?;
    (parts.next().is_none() && (1..=12).contains(&month)).then_some(CalendarDate {
        year,
        month,
        day: 1,
    })
}

pub(crate) fn parse_calendar_date(value: &str) -> Option<CalendarDate> {
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

pub(crate) fn valid_calendar_date(year: i64, month: i64, day: i64) -> bool {
    if !(1..=12).contains(&month) {
        return false;
    }
    (1..=days_in_month(year, month)).contains(&day)
}

pub(crate) fn calendar_month_start(date: CalendarDate) -> CalendarDate {
    CalendarDate {
        year: date.year,
        month: date.month,
        day: 1,
    }
}

pub(crate) fn calendar_month_end(date: CalendarDate) -> CalendarDate {
    CalendarDate {
        year: date.year,
        month: date.month,
        day: days_in_month(date.year, date.month),
    }
}

pub(crate) fn shift_calendar_month(date: CalendarDate, delta: i64) -> CalendarDate {
    let month_index = date.year * 12 + (date.month - 1) + delta;
    let year = month_index.div_euclid(12);
    let month = month_index.rem_euclid(12) + 1;
    CalendarDate {
        year,
        month,
        day: date.day.min(days_in_month(year, month)),
    }
}

pub(crate) fn add_calendar_days(date: CalendarDate, delta: i64) -> CalendarDate {
    let shifted = civil_from_days(days_from_civil(date.year, date.month, date.day) + delta);
    CalendarDate {
        year: shifted.year,
        month: shifted.month,
        day: shifted.day,
    }
}

pub(crate) fn calendar_weekday_index(
    date: CalendarDate,
    start_of_week: PeriodicStartOfWeek,
) -> usize {
    usize::try_from(days_since_week_start(
        days_from_civil(date.year, date.month, date.day),
        start_of_week,
    ))
    .unwrap_or(0)
}

pub(crate) fn days_since_week_start(
    days_since_epoch: i64,
    start_of_week: PeriodicStartOfWeek,
) -> i64 {
    let weekday = (days_since_epoch + 3).rem_euclid(7);
    let week_start = match start_of_week {
        PeriodicStartOfWeek::Monday => 0,
        PeriodicStartOfWeek::Sunday => 6,
        PeriodicStartOfWeek::Saturday => 5,
    };
    (weekday - week_start).rem_euclid(7)
}

pub(crate) fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

pub(crate) fn is_leap_year(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

pub(crate) fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
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

pub(crate) fn civil_from_days(days: i64) -> CalendarDate {
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
