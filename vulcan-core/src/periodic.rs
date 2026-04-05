use crate::config::{PeriodicCadenceUnit, PeriodicConfig, PeriodicNoteConfig, PeriodicStartOfWeek};
use crate::{CacheDatabase, VaultPaths};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt::{Display, Formatter, Write as _};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeriodicNoteMatch {
    pub period_type: String,
    pub start_date: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeriodicEvent {
    pub start_time: String,
    pub end_time: Option<String>,
    pub title: String,
    pub metadata: Value,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DailyNoteEvents {
    pub date: String,
    pub path: String,
    pub events: Vec<PeriodicEvent>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeriodicEventOccurrence {
    pub date: String,
    pub path: String,
    pub start_time: String,
    pub end_time: Option<String>,
    pub title: String,
    pub metadata: Value,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeriodicIcsExport {
    pub calendar_name: String,
    pub note_count: usize,
    pub event_count: usize,
    pub content: String,
}

#[derive(Debug)]
pub enum PeriodicError {
    Cache(crate::CacheError),
    Json(serde_json::Error),
    Sqlite(rusqlite::Error),
}

impl Display for PeriodicError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cache(error) => write!(formatter, "{error}"),
            Self::Json(error) => write!(formatter, "{error}"),
            Self::Sqlite(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for PeriodicError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Cache(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::Sqlite(error) => Some(error),
        }
    }
}

impl From<crate::CacheError> for PeriodicError {
    fn from(error: crate::CacheError) -> Self {
        Self::Cache(error)
    }
}

impl From<rusqlite::Error> for PeriodicError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}

impl From<serde_json::Error> for PeriodicError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DateParts {
    year: i64,
    month: i64,
    day: i64,
}

impl DateParts {
    fn iso_string(self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }

    fn month_index(self) -> i64 {
        self.year * 12 + (self.month - 1)
    }

    fn quarter_index(self) -> i64 {
        self.year * 4 + (quarter_for_month(self.month) - 1)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PeriodDefinition {
    unit: PeriodicCadenceUnit,
    interval: i64,
    anchor: DateParts,
    start_of_week: PeriodicStartOfWeek,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum FormatPart {
    Literal(String),
    Year,
    Month,
    Day,
    Week,
    IsoWeek,
    IsoWeekYear,
    Quarter,
}

#[derive(Debug, Clone, Copy, Default)]
struct ParsedFormatValues {
    year: Option<i64>,
    month: Option<i64>,
    day: Option<i64>,
    week: Option<i64>,
    iso_week: Option<i64>,
    iso_week_year: Option<i64>,
    quarter: Option<i64>,
}

#[must_use]
pub fn expected_periodic_note_path(
    config: &PeriodicConfig,
    period_type: &str,
    date: &str,
) -> Option<String> {
    let entry = config.note(period_type)?;
    let start = period_start(period_type, parse_iso_date(date)?, entry)?;
    let file_name = format!("{}.md", render_period_name(start, &entry.format, entry)?);
    let folder = normalize_folder(&entry.folder);
    Some(if folder.is_empty() {
        file_name
    } else {
        format!("{folder}/{file_name}")
    })
}

#[must_use]
pub fn resolve_periodic_note(
    vault_root: &Path,
    config: &PeriodicConfig,
    period_type: &str,
    date: &str,
) -> Option<String> {
    let path = expected_periodic_note_path(config, period_type, date)?;
    vault_root.join(&path).is_file().then_some(path)
}

#[must_use]
pub fn resolve_daily_note(
    vault_root: &Path,
    config: &PeriodicConfig,
    date: &str,
) -> Option<String> {
    resolve_periodic_note(vault_root, config, "daily", date)
}

#[must_use]
pub fn match_periodic_note_path(
    config: &PeriodicConfig,
    relative_path: &str,
) -> Option<PeriodicNoteMatch> {
    let path = relative_path.strip_suffix(".md")?;
    let (folder, file_name) = path
        .rsplit_once('/')
        .map_or(("", path), |(folder, name)| (folder, name));

    for (period_type, note_config) in &config.notes {
        let expected_folder = normalize_folder(&note_config.folder);
        if expected_folder != folder {
            continue;
        }
        let start_date =
            parse_period_name(file_name, &note_config.format, period_type, note_config)?;
        return Some(PeriodicNoteMatch {
            period_type: period_type.clone(),
            start_date,
        });
    }

    None
}

#[must_use]
pub fn period_range_for_date(
    config: &PeriodicConfig,
    period_type: &str,
    date: &str,
) -> Option<(String, String)> {
    let entry = config.note(period_type)?;
    let start = period_start(period_type, parse_iso_date(date)?, entry)?;
    let end = period_end(period_type, start, entry)?;
    Some((start.iso_string(), end.iso_string()))
}

#[must_use]
pub fn step_period_start(
    config: &PeriodicConfig,
    period_type: &str,
    start_date: &str,
) -> Option<String> {
    let entry = config.note(period_type)?;
    let start = parse_iso_date(start_date)?;
    let next = next_period_start(period_type, start, entry)?;
    Some(next.iso_string())
}

#[must_use]
pub fn today_utc_string() -> String {
    let seconds = crate::current_utc_timestamp_ms().div_euclid(1_000);
    civil_from_days(seconds.div_euclid(86_400)).iso_string()
}

pub fn load_events_for_periodic_note(
    paths: &VaultPaths,
    relative_path: &str,
) -> Result<Vec<PeriodicEvent>, PeriodicError> {
    let database = CacheDatabase::open(paths)?;
    let mut statement = database.connection().prepare(
        "
        SELECT events.start_time, events.end_time, events.title, events.metadata_json, events.tags_json
        FROM events
        JOIN documents ON documents.id = events.document_id
        WHERE documents.path = ?1
        ORDER BY
            CASE WHEN events.start_time = 'all-day' THEN 0 ELSE 1 END,
            events.start_time,
            events.byte_offset
        ",
    )?;
    let rows = statement.query_map([relative_path], deserialize_periodic_event_row)?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(PeriodicError::from)
}

pub fn list_daily_note_events(
    paths: &VaultPaths,
    start: &str,
    end: &str,
) -> Result<Vec<DailyNoteEvents>, PeriodicError> {
    let database = CacheDatabase::open(paths)?;
    let mut statement = database.connection().prepare(
        "
        SELECT
            documents.periodic_date,
            documents.path,
            events.start_time,
            events.end_time,
            events.title,
            events.metadata_json,
            events.tags_json
        FROM documents
        LEFT JOIN events ON events.document_id = documents.id
        WHERE documents.periodic_type = 'daily'
          AND documents.periodic_date >= ?1
          AND documents.periodic_date <= ?2
        ORDER BY
            documents.periodic_date,
            documents.path,
            CASE WHEN events.start_time = 'all-day' THEN 0 ELSE 1 END,
            events.start_time,
            events.byte_offset
        ",
    )?;
    let rows = statement.query_map([start, end], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, Option<String>>(4)?,
            row.get::<_, Option<String>>(5)?,
            row.get::<_, Option<String>>(6)?,
        ))
    })?;

    let mut items = Vec::<DailyNoteEvents>::new();
    for row in rows {
        let (date, path, start_time, end_time, title, metadata_json, tags_json) = row?;
        let needs_new_item = items
            .last()
            .map_or(true, |item| item.date != date || item.path != path);
        if needs_new_item {
            items.push(DailyNoteEvents {
                date: date.clone(),
                path: path.clone(),
                events: Vec::new(),
            });
        }

        if let Some(start_time) = start_time {
            let event = PeriodicEvent {
                start_time,
                end_time,
                title: title.unwrap_or_default(),
                metadata: deserialize_metadata(metadata_json.as_deref())?,
                tags: deserialize_tags(tags_json.as_deref())?,
            };
            if let Some(item) = items.last_mut() {
                item.events.push(event);
            }
        }
    }

    Ok(items)
}

pub fn list_events_between(
    paths: &VaultPaths,
    start: &str,
    end: &str,
) -> Result<Vec<PeriodicEventOccurrence>, PeriodicError> {
    let daily_notes = list_daily_note_events(paths, start, end)?;
    Ok(daily_notes
        .into_iter()
        .flat_map(|note| {
            let date = note.date;
            let path = note.path;
            note.events
                .into_iter()
                .map(move |event| PeriodicEventOccurrence {
                    date: date.clone(),
                    path: path.clone(),
                    start_time: event.start_time,
                    end_time: event.end_time,
                    title: event.title,
                    metadata: event.metadata,
                    tags: event.tags,
                })
        })
        .collect())
}

pub fn export_daily_events_to_ics(
    paths: &VaultPaths,
    start: &str,
    end: &str,
    calendar_name: Option<&str>,
) -> Result<PeriodicIcsExport, PeriodicError> {
    let daily_notes = list_daily_note_events(paths, start, end)?;
    let events = daily_event_occurrences(&daily_notes);
    let calendar_name = calendar_name
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("Vulcan Daily Events")
        .to_string();
    let content = render_ics_calendar(&calendar_name, &events);

    Ok(PeriodicIcsExport {
        calendar_name,
        note_count: daily_notes.len(),
        event_count: events.len(),
        content,
    })
}

fn daily_event_occurrences(daily_notes: &[DailyNoteEvents]) -> Vec<PeriodicEventOccurrence> {
    daily_notes
        .iter()
        .flat_map(|note| {
            note.events
                .iter()
                .cloned()
                .map(|event| PeriodicEventOccurrence {
                    date: note.date.clone(),
                    path: note.path.clone(),
                    start_time: event.start_time,
                    end_time: event.end_time,
                    title: event.title,
                    metadata: event.metadata,
                    tags: event.tags,
                })
        })
        .collect()
}

fn render_ics_calendar(calendar_name: &str, events: &[PeriodicEventOccurrence]) -> String {
    let dtstamp = current_utc_timestamp();
    let mut content = String::new();
    content.push_str("BEGIN:VCALENDAR\r\n");
    content.push_str("VERSION:2.0\r\n");
    content.push_str("PRODID:-//Vulcan//Periodic Notes//EN\r\n");
    content.push_str("CALSCALE:GREGORIAN\r\n");
    let _ = writeln!(content, "X-WR-CALNAME:{}\r", escape_ics_text(calendar_name));
    for event in events {
        write_ics_event(&mut content, event, &dtstamp);
    }
    content.push_str("END:VCALENDAR\r\n");
    content
}

fn write_ics_event(content: &mut String, event: &PeriodicEventOccurrence, dtstamp: &str) {
    content.push_str("BEGIN:VEVENT\r\n");
    let uid_source = format!(
        "{}|{}|{}|{}|{}",
        event.path,
        event.date,
        event.start_time,
        event.end_time.as_deref().unwrap_or(""),
        event.title
    );
    let _ = writeln!(
        content,
        "UID:{}@vulcan\r",
        blake3::hash(uid_source.as_bytes()).to_hex()
    );
    let _ = writeln!(content, "DTSTAMP:{dtstamp}\r");
    write_ics_event_times(content, event);
    let _ = writeln!(content, "SUMMARY:{}\r", escape_ics_text(&event.title));
    if let Some(location) = event.metadata.get("location").and_then(Value::as_str) {
        let _ = writeln!(content, "LOCATION:{}\r", escape_ics_text(location));
    }
    if !event.tags.is_empty() {
        let _ = writeln!(
            content,
            "CATEGORIES:{}\r",
            event
                .tags
                .iter()
                .map(|tag| escape_ics_text(tag))
                .collect::<Vec<_>>()
                .join(",")
        );
    }
    let description = event_description(event);
    if !description.is_empty() {
        let _ = writeln!(content, "DESCRIPTION:{}\r", escape_ics_text(&description));
    }
    content.push_str("END:VEVENT\r\n");
}

fn write_ics_event_times(content: &mut String, event: &PeriodicEventOccurrence) {
    if event.start_time == "all-day" {
        let _ = writeln!(
            content,
            "DTSTART;VALUE=DATE:{}\r",
            compact_ics_date(&event.date)
        );
        let next_day = add_days(
            parse_iso_date(&event.date).unwrap_or(DateParts {
                year: 1970,
                month: 1,
                day: 1,
            }),
            1,
        );
        let _ = writeln!(
            content,
            "DTEND;VALUE=DATE:{}\r",
            compact_ics_date(&next_day.iso_string())
        );
        return;
    }

    if let Some(start_at) = compact_ics_datetime(&event.date, &event.start_time) {
        let _ = writeln!(content, "DTSTART:{start_at}\r");
        if let Some(end_time) = event.end_time.as_deref() {
            if let Some(end_at) = compact_ics_datetime(&event.date, end_time) {
                let _ = writeln!(content, "DTEND:{end_at}\r");
            }
        }
    }
}

fn deserialize_periodic_event_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PeriodicEvent> {
    let metadata_json: String = row.get(3)?;
    let tags_json: String = row.get(4)?;
    Ok(PeriodicEvent {
        start_time: row.get(0)?,
        end_time: row.get(1)?,
        title: row.get(2)?,
        metadata: deserialize_metadata(Some(&metadata_json)).map_err(to_sqlite_error)?,
        tags: deserialize_tags(Some(&tags_json)).map_err(to_sqlite_error)?,
    })
}

fn deserialize_metadata(raw: Option<&str>) -> Result<Value, PeriodicError> {
    raw.filter(|value| !value.is_empty())
        .map(serde_json::from_str)
        .transpose()?
        .map_or(Ok(Value::Null), Ok)
}

fn deserialize_tags(raw: Option<&str>) -> Result<Vec<String>, PeriodicError> {
    raw.filter(|value| !value.is_empty())
        .map(serde_json::from_str)
        .transpose()?
        .map_or(Ok(Vec::new()), Ok)
}

fn to_sqlite_error(error: PeriodicError) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
}

fn compact_ics_date(date: &str) -> String {
    date.replace('-', "")
}

fn compact_ics_datetime(date: &str, time: &str) -> Option<String> {
    let (hours, minutes) = time.split_once(':')?;
    Some(format!(
        "{}T{}{}00",
        compact_ics_date(date),
        hours.trim(),
        minutes.trim()
    ))
}

fn escape_ics_text(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace(';', "\\;")
        .replace(',', "\\,")
}

fn event_description(event: &PeriodicEventOccurrence) -> String {
    let mut lines = vec![format!("Source: {}", event.path)];
    if !event.tags.is_empty() {
        lines.push(format!("Tags: {}", event.tags.join(", ")));
    }
    if let Some(metadata) = event.metadata.as_object() {
        for (key, value) in metadata {
            let rendered = match value {
                Value::Null => continue,
                Value::String(text) => text.clone(),
                _ => value.to_string(),
            };
            lines.push(format!("{key}: {rendered}"));
        }
    }
    lines.join("\n")
}

fn current_utc_timestamp() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .try_into()
        .unwrap_or(i64::MAX);
    let date = civil_from_days(seconds.div_euclid(86_400));
    let seconds_of_day = seconds.rem_euclid(86_400);
    let hour = seconds_of_day.div_euclid(3_600);
    let minute = seconds_of_day.rem_euclid(3_600).div_euclid(60);
    let second = seconds_of_day.rem_euclid(60);
    format!(
        "{:04}{:02}{:02}T{:02}{:02}{:02}Z",
        date.year, date.month, date.day, hour, minute, second
    )
}

fn normalize_folder(folder: &Path) -> String {
    folder
        .components()
        .filter_map(|component| match component {
            std::path::Component::CurDir => None,
            other => Some(other.as_os_str().to_string_lossy().into_owned()),
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn period_start(
    period_type: &str,
    date: DateParts,
    config: &PeriodicNoteConfig,
) -> Option<DateParts> {
    let definition = period_definition(period_type, config)?;
    match definition.unit {
        PeriodicCadenceUnit::Days => {
            let anchor_days = days_from_civil(
                definition.anchor.year,
                definition.anchor.month,
                definition.anchor.day,
            );
            let target_days = days_from_civil(date.year, date.month, date.day);
            Some(civil_from_days(
                anchor_days
                    + (target_days - anchor_days).div_euclid(definition.interval)
                        * definition.interval,
            ))
        }
        PeriodicCadenceUnit::Weeks => {
            let anchor_start = week_start(definition.anchor, definition.start_of_week);
            let target_start = week_start(date, definition.start_of_week);
            let diff_weeks =
                (days_from_civil(target_start.year, target_start.month, target_start.day)
                    - days_from_civil(anchor_start.year, anchor_start.month, anchor_start.day))
                .div_euclid(7);
            Some(add_days(
                anchor_start,
                diff_weeks.div_euclid(definition.interval) * definition.interval * 7,
            ))
        }
        PeriodicCadenceUnit::Months => {
            let start_month_index = definition.anchor.month_index()
                + (date.month_index() - definition.anchor.month_index())
                    .div_euclid(definition.interval)
                    * definition.interval;
            Some(date_from_month_index(start_month_index))
        }
        PeriodicCadenceUnit::Quarters => {
            let start_quarter_index = definition.anchor.quarter_index()
                + (date.quarter_index() - definition.anchor.quarter_index())
                    .div_euclid(definition.interval)
                    * definition.interval;
            Some(date_from_quarter_index(start_quarter_index))
        }
        PeriodicCadenceUnit::Years => Some(DateParts {
            year: definition.anchor.year
                + (date.year - definition.anchor.year).div_euclid(definition.interval)
                    * definition.interval,
            month: 1,
            day: 1,
        }),
    }
}

fn period_end(
    period_type: &str,
    start: DateParts,
    config: &PeriodicNoteConfig,
) -> Option<DateParts> {
    Some(add_days(next_period_start(period_type, start, config)?, -1))
}

fn next_period_start(
    period_type: &str,
    start: DateParts,
    config: &PeriodicNoteConfig,
) -> Option<DateParts> {
    let definition = period_definition(period_type, config)?;
    match definition.unit {
        PeriodicCadenceUnit::Days => Some(add_days(start, definition.interval)),
        PeriodicCadenceUnit::Weeks => Some(add_days(start, definition.interval * 7)),
        PeriodicCadenceUnit::Months => Some(date_from_month_index(
            start.month_index() + definition.interval,
        )),
        PeriodicCadenceUnit::Quarters => Some(date_from_quarter_index(
            start.quarter_index() + definition.interval,
        )),
        PeriodicCadenceUnit::Years => Some(DateParts {
            year: start.year + definition.interval,
            month: 1,
            day: 1,
        }),
    }
}

fn period_definition(period_type: &str, config: &PeriodicNoteConfig) -> Option<PeriodDefinition> {
    let unit = config.unit.or_else(|| built_in_unit(period_type))?;
    let interval = i64::try_from(config.interval.max(1)).ok()?;
    let anchor = period_anchor(config, unit);
    Some(PeriodDefinition {
        unit,
        interval,
        anchor,
        start_of_week: config.start_of_week,
    })
}

fn built_in_unit(period_type: &str) -> Option<PeriodicCadenceUnit> {
    match period_type {
        "daily" => Some(PeriodicCadenceUnit::Days),
        "weekly" => Some(PeriodicCadenceUnit::Weeks),
        "monthly" => Some(PeriodicCadenceUnit::Months),
        "quarterly" => Some(PeriodicCadenceUnit::Quarters),
        "yearly" => Some(PeriodicCadenceUnit::Years),
        _ => None,
    }
}

fn period_anchor(config: &PeriodicNoteConfig, unit: PeriodicCadenceUnit) -> DateParts {
    let raw_anchor = config
        .anchor_date
        .as_deref()
        .and_then(parse_iso_date)
        .unwrap_or(DateParts {
            year: 1970,
            month: 1,
            day: 1,
        });
    match unit {
        PeriodicCadenceUnit::Days => raw_anchor,
        PeriodicCadenceUnit::Weeks => week_start(raw_anchor, config.start_of_week),
        PeriodicCadenceUnit::Months => DateParts {
            year: raw_anchor.year,
            month: raw_anchor.month,
            day: 1,
        },
        PeriodicCadenceUnit::Quarters => DateParts {
            year: raw_anchor.year,
            month: (quarter_for_month(raw_anchor.month) - 1) * 3 + 1,
            day: 1,
        },
        PeriodicCadenceUnit::Years => DateParts {
            year: raw_anchor.year,
            month: 1,
            day: 1,
        },
    }
}

fn render_period_name(
    start: DateParts,
    format: &str,
    config: &PeriodicNoteConfig,
) -> Option<String> {
    let parts = tokenize_format(format)?;
    let (iso_week_year, iso_week) = iso_week_components(start);
    let custom_week = custom_week_number(start, config.start_of_week);
    let quarter = quarter_for_month(start.month);
    let mut rendered = String::new();

    for part in parts {
        match part {
            FormatPart::Literal(text) => rendered.push_str(&text),
            FormatPart::Year => {
                let _ = write!(rendered, "{:04}", start.year);
            }
            FormatPart::Month => {
                let _ = write!(rendered, "{:02}", start.month);
            }
            FormatPart::Day => {
                let _ = write!(rendered, "{:02}", start.day);
            }
            FormatPart::Week => {
                let _ = write!(rendered, "{custom_week:02}");
            }
            FormatPart::IsoWeek => {
                let _ = write!(rendered, "{iso_week:02}");
            }
            FormatPart::IsoWeekYear => {
                let _ = write!(rendered, "{iso_week_year:04}");
            }
            FormatPart::Quarter => rendered.push_str(&quarter.to_string()),
        }
    }

    Some(rendered)
}

fn parse_period_name(
    file_name: &str,
    format: &str,
    period_type: &str,
    config: &PeriodicNoteConfig,
) -> Option<String> {
    let parts = tokenize_format(format)?;
    let mut remaining = file_name;
    let mut values = ParsedFormatValues::default();

    for part in parts {
        match part {
            FormatPart::Literal(text) => {
                remaining = remaining.strip_prefix(&text)?;
            }
            FormatPart::Year => {
                let (value, rest) = parse_fixed_digits(remaining, 4)?;
                values.year = Some(value);
                remaining = rest;
            }
            FormatPart::Month => {
                let (value, rest) = parse_fixed_digits(remaining, 2)?;
                values.month = Some(value);
                remaining = rest;
            }
            FormatPart::Day => {
                let (value, rest) = parse_fixed_digits(remaining, 2)?;
                values.day = Some(value);
                remaining = rest;
            }
            FormatPart::Week => {
                let (value, rest) = parse_fixed_digits(remaining, 2)?;
                values.week = Some(value);
                remaining = rest;
            }
            FormatPart::IsoWeek => {
                let (value, rest) = parse_fixed_digits(remaining, 2)?;
                values.iso_week = Some(value);
                remaining = rest;
            }
            FormatPart::IsoWeekYear => {
                let (value, rest) = parse_fixed_digits(remaining, 4)?;
                values.iso_week_year = Some(value);
                remaining = rest;
            }
            FormatPart::Quarter => {
                let (value, rest) = parse_fixed_digits(remaining, 1)?;
                values.quarter = Some(value);
                remaining = rest;
            }
        }
    }

    if !remaining.is_empty() {
        return None;
    }

    let candidate = date_from_format_values(period_type, values, config)?;
    period_start(period_type, candidate, config).map(DateParts::iso_string)
}

fn date_from_format_values(
    period_type: &str,
    values: ParsedFormatValues,
    config: &PeriodicNoteConfig,
) -> Option<DateParts> {
    let unit = period_definition(period_type, config)?.unit;
    match unit {
        PeriodicCadenceUnit::Days => Some(DateParts {
            year: values.year?,
            month: values.month?,
            day: values.day?,
        }),
        PeriodicCadenceUnit::Weeks => {
            if let (Some(year), Some(month), Some(day)) = (values.year, values.month, values.day) {
                return Some(DateParts { year, month, day });
            }
            if let (Some(week_year), Some(week)) = (values.iso_week_year, values.iso_week) {
                return iso_week_start(week_year, week);
            }
            let year = values.year?;
            let week = values.week?;
            custom_week_start(year, week, config.start_of_week)
        }
        PeriodicCadenceUnit::Months => Some(DateParts {
            year: values.year?,
            month: values.month?,
            day: values.day.unwrap_or(1),
        }),
        PeriodicCadenceUnit::Quarters => {
            if let (Some(year), Some(month)) = (values.year, values.month) {
                return Some(DateParts {
                    year,
                    month,
                    day: values.day.unwrap_or(1),
                });
            }
            Some(DateParts {
                year: values.year?,
                month: (values.quarter? - 1) * 3 + 1,
                day: 1,
            })
        }
        PeriodicCadenceUnit::Years => Some(DateParts {
            year: values.year?,
            month: values.month.unwrap_or(1),
            day: values.day.unwrap_or(1),
        }),
    }
}

fn tokenize_format(format: &str) -> Option<Vec<FormatPart>> {
    let mut parts = Vec::new();
    let mut index = 0_usize;
    while index < format.len() {
        let remainder = &format[index..];
        if let Some(rest) = remainder.strip_prefix('[') {
            let end = rest.find(']')?;
            parts.push(FormatPart::Literal(rest[..end].to_string()));
            index += end + 2;
            continue;
        }
        if remainder.starts_with("GGGG") {
            parts.push(FormatPart::IsoWeekYear);
            index += 4;
            continue;
        }
        if remainder.starts_with("YYYY") {
            parts.push(FormatPart::Year);
            index += 4;
            continue;
        }
        if remainder.starts_with("WW") {
            parts.push(FormatPart::IsoWeek);
            index += 2;
            continue;
        }
        if remainder.starts_with("ww") {
            parts.push(FormatPart::Week);
            index += 2;
            continue;
        }
        if remainder.starts_with("MM") {
            parts.push(FormatPart::Month);
            index += 2;
            continue;
        }
        if remainder.starts_with("DD") {
            parts.push(FormatPart::Day);
            index += 2;
            continue;
        }
        if remainder.starts_with('Q') {
            parts.push(FormatPart::Quarter);
            index += 1;
            continue;
        }

        let ch = remainder.chars().next()?;
        match parts.last_mut() {
            Some(FormatPart::Literal(text)) => text.push(ch),
            _ => parts.push(FormatPart::Literal(ch.to_string())),
        }
        index += ch.len_utf8();
    }

    Some(parts)
}

fn parse_fixed_digits(input: &str, width: usize) -> Option<(i64, &str)> {
    let prefix = input.get(..width)?;
    if !prefix.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    Some((prefix.parse().ok()?, &input[width..]))
}

fn parse_iso_date(date: &str) -> Option<DateParts> {
    let mut parts = date.split('-');
    let year = parts.next()?.parse().ok()?;
    let month = parts.next()?.parse().ok()?;
    let day = parts.next()?.parse().ok()?;
    (parts.next().is_none() && valid_date(year, month, day)).then_some(DateParts {
        year,
        month,
        day,
    })
}

fn valid_date(year: i64, month: i64, day: i64) -> bool {
    if !(1..=12).contains(&month) {
        return false;
    }
    (1..=days_in_month(year, month)).contains(&day)
}

fn quarter_for_month(month: i64) -> i64 {
    ((month - 1) / 3) + 1
}

fn add_days(date: DateParts, delta: i64) -> DateParts {
    civil_from_days(days_from_civil(date.year, date.month, date.day) + delta)
}

fn date_from_month_index(index: i64) -> DateParts {
    let year = index.div_euclid(12);
    let month = index.rem_euclid(12) + 1;
    DateParts {
        year,
        month,
        day: 1,
    }
}

fn date_from_quarter_index(index: i64) -> DateParts {
    let year = index.div_euclid(4);
    let quarter = index.rem_euclid(4) + 1;
    DateParts {
        year,
        month: (quarter - 1) * 3 + 1,
        day: 1,
    }
}

fn week_start(date: DateParts, start_of_week: PeriodicStartOfWeek) -> DateParts {
    let days = days_from_civil(date.year, date.month, date.day);
    civil_from_days(days - days_since_week_start(days, start_of_week))
}

fn custom_week_start(
    year: i64,
    week: i64,
    start_of_week: PeriodicStartOfWeek,
) -> Option<DateParts> {
    if week < 1 {
        return None;
    }
    let jan1 = DateParts {
        year,
        month: 1,
        day: 1,
    };
    Some(add_days(week_start(jan1, start_of_week), (week - 1) * 7))
}

fn custom_week_number(date: DateParts, start_of_week: PeriodicStartOfWeek) -> i64 {
    let year_start = week_start(
        DateParts {
            year: date.year,
            month: 1,
            day: 1,
        },
        start_of_week,
    );
    (days_from_civil(date.year, date.month, date.day)
        - days_from_civil(year_start.year, year_start.month, year_start.day))
    .div_euclid(7)
        + 1
}

fn iso_week_start(year: i64, week: i64) -> Option<DateParts> {
    if !(1..=53).contains(&week) {
        return None;
    }
    let jan4 = DateParts {
        year,
        month: 1,
        day: 4,
    };
    let jan4_days = days_from_civil(jan4.year, jan4.month, jan4.day);
    let monday = jan4_days - (iso_weekday(jan4) - 1);
    Some(civil_from_days(monday + (week - 1) * 7))
}

fn iso_week_components(date: DateParts) -> (i64, i64) {
    let weekday = iso_weekday(date);
    let ordinal =
        days_from_civil(date.year, date.month, date.day) - days_from_civil(date.year, 1, 1) + 1;
    let mut week = (ordinal - weekday + 10).div_euclid(7);
    let mut week_year = date.year;

    if week < 1 {
        week_year -= 1;
        week = iso_weeks_in_year(week_year);
    } else if week > iso_weeks_in_year(week_year) {
        week_year += 1;
        week = 1;
    }

    (week_year, week)
}

fn iso_weekday(date: DateParts) -> i64 {
    (days_from_civil(date.year, date.month, date.day) + 3).rem_euclid(7) + 1
}

fn iso_weeks_in_year(year: i64) -> i64 {
    let jan1 = iso_weekday(DateParts {
        year,
        month: 1,
        day: 1,
    });
    let dec31 = iso_weekday(DateParts {
        year,
        month: 12,
        day: 31,
    });
    if jan1 == 4 || dec31 == 4 {
        53
    } else {
        52
    }
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

fn civil_from_days(days: i64) -> DateParts {
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

    DateParts {
        year: if month <= 2 { year + 1 } else { year },
        month,
        day,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{PeriodicCadenceUnit, PeriodicConfig};
    use crate::{scan_vault, ScanMode, VaultPaths};
    use std::fs;
    use std::path::PathBuf;
    use tempfile::tempdir;

    #[test]
    fn daily_paths_render_and_resolve() {
        let config = PeriodicConfig::default();

        assert_eq!(
            expected_periodic_note_path(&config, "daily", "2026-04-03"),
            Some("Journal/Daily/2026-04-03.md".to_string())
        );
        assert_eq!(
            match_periodic_note_path(&config, "Journal/Daily/2026-04-03.md"),
            Some(PeriodicNoteMatch {
                period_type: "daily".to_string(),
                start_date: "2026-04-03".to_string(),
            })
        );
    }

    #[test]
    fn weekly_paths_respect_configured_week_start() {
        let mut config = PeriodicConfig::default();
        config.note_mut("weekly").start_of_week = PeriodicStartOfWeek::Sunday;

        assert_eq!(
            period_range_for_date(&config, "weekly", "2026-04-03"),
            Some(("2026-03-29".to_string(), "2026-04-04".to_string()))
        );
        assert_eq!(
            expected_periodic_note_path(&config, "weekly", "2026-04-03"),
            Some("Journal/Weekly/2026-W14.md".to_string())
        );
    }

    #[test]
    fn quarterly_resolution_round_trips() {
        let config = PeriodicConfig::default();

        assert_eq!(
            expected_periodic_note_path(&config, "quarterly", "2026-11-02"),
            Some("Journal/Quarterly/2026-Q4.md".to_string())
        );
        assert_eq!(
            match_periodic_note_path(&config, "Journal/Quarterly/2026-Q4.md"),
            Some(PeriodicNoteMatch {
                period_type: "quarterly".to_string(),
                start_date: "2026-10-01".to_string(),
            })
        );
    }

    #[test]
    fn custom_folder_and_iso_week_tokens_round_trip() {
        let mut config = PeriodicConfig::default();
        let weekly = config.note_mut("weekly");
        weekly.folder = PathBuf::from("Weekly");
        weekly.format = "GGGG-[W]WW".to_string();

        assert_eq!(
            expected_periodic_note_path(&config, "weekly", "2027-01-01"),
            Some("Weekly/2026-W53.md".to_string())
        );
        assert_eq!(
            match_periodic_note_path(&config, "Weekly/2026-W53.md"),
            Some(PeriodicNoteMatch {
                period_type: "weekly".to_string(),
                start_date: "2026-12-28".to_string(),
            })
        );
    }

    #[test]
    fn step_period_advances_across_month_and_year_boundaries() {
        let config = PeriodicConfig::default();

        assert_eq!(
            step_period_start(&config, "monthly", "2026-12-01"),
            Some("2027-01-01".to_string())
        );
        assert_eq!(
            step_period_start(&config, "yearly", "2026-01-01"),
            Some("2027-01-01".to_string())
        );
    }

    #[test]
    fn custom_biweekly_periods_align_to_anchor_dates() {
        let mut config = PeriodicConfig::default();
        let sprint = config.note_mut("sprint");
        sprint.enabled = true;
        sprint.folder = PathBuf::from("Journal/Sprints");
        sprint.format = "YYYY-[Sprint]-MM-DD".to_string();
        sprint.unit = Some(PeriodicCadenceUnit::Weeks);
        sprint.interval = 2;
        sprint.anchor_date = Some("2026-01-05".to_string());

        assert_eq!(
            period_range_for_date(&config, "sprint", "2026-01-15"),
            Some(("2026-01-05".to_string(), "2026-01-18".to_string()))
        );
        assert_eq!(
            expected_periodic_note_path(&config, "sprint", "2026-01-15"),
            Some("Journal/Sprints/2026-Sprint-01-05.md".to_string())
        );
        assert_eq!(
            step_period_start(&config, "sprint", "2026-01-05"),
            Some("2026-01-19".to_string())
        );
        assert_eq!(
            match_periodic_note_path(&config, "Journal/Sprints/2026-Sprint-01-19.md"),
            Some(PeriodicNoteMatch {
                period_type: "sprint".to_string(),
                start_date: "2026-01-19".to_string(),
            })
        );
    }

    #[test]
    fn custom_multi_month_periods_round_trip_from_month_format() {
        let mut config = PeriodicConfig::default();
        let release = config.note_mut("release");
        release.enabled = true;
        release.folder = PathBuf::from("Journal/Releases");
        release.format = "YYYY-MM".to_string();
        release.unit = Some(PeriodicCadenceUnit::Months);
        release.interval = 2;
        release.anchor_date = Some("2026-02-01".to_string());

        assert_eq!(
            period_range_for_date(&config, "release", "2026-05-15"),
            Some(("2026-04-01".to_string(), "2026-05-31".to_string()))
        );
        assert_eq!(
            expected_periodic_note_path(&config, "release", "2026-05-15"),
            Some("Journal/Releases/2026-04.md".to_string())
        );
        assert_eq!(
            match_periodic_note_path(&config, "Journal/Releases/2026-04.md"),
            Some(PeriodicNoteMatch {
                period_type: "release".to_string(),
                start_date: "2026-04-01".to_string(),
            })
        );
    }

    #[test]
    fn loads_daily_events_and_exports_ics() {
        let temp_dir = tempdir().expect("temp dir should be created");
        let vault_root = temp_dir.path();
        fs::create_dir_all(vault_root.join(".vulcan")).expect("config dir should exist");
        fs::write(
            vault_root.join(".vulcan/config.toml"),
            "[periodic.daily]\nschedule_heading = \"Schedule\"\n",
        )
        .expect("config should be written");
        fs::create_dir_all(vault_root.join("Journal/Daily"))
            .expect("daily directory should be created");
        fs::write(
            vault_root.join("Journal/Daily/2026-04-03.md"),
            "# 2026-04-03\n\n## Schedule\n- 09:00-10:00 Team standup @location(Zoom)\n- 14:00 Dentist #personal\n",
        )
        .expect("first daily note should be written");
        fs::write(
            vault_root.join("Journal/Daily/2026-04-04.md"),
            "# 2026-04-04\n\n## Schedule\n- all-day Company offsite\n",
        )
        .expect("second daily note should be written");

        let paths = VaultPaths::new(vault_root);
        scan_vault(&paths, ScanMode::Full).expect("vault should scan");

        let daily = list_daily_note_events(&paths, "2026-04-03", "2026-04-04")
            .expect("daily events should load");
        assert_eq!(daily.len(), 2);
        assert_eq!(daily[0].path, "Journal/Daily/2026-04-03.md");
        assert_eq!(daily[0].events.len(), 2);
        assert_eq!(daily[0].events[0].title, "Team standup");
        assert_eq!(
            daily[0].events[0].metadata.get("location"),
            Some(&Value::String("Zoom".to_string()))
        );
        assert_eq!(daily[1].events[0].start_time, "all-day");

        let occurrences = list_events_between(&paths, "2026-04-03", "2026-04-04")
            .expect("event occurrences should load");
        assert_eq!(occurrences.len(), 3);
        assert_eq!(occurrences[2].title, "Company offsite");

        let ics = export_daily_events_to_ics(&paths, "2026-04-03", "2026-04-04", Some("Journal"))
            .expect("ics export should succeed");
        assert_eq!(ics.calendar_name, "Journal");
        assert_eq!(ics.note_count, 2);
        assert_eq!(ics.event_count, 3);
        assert!(ics.content.contains("BEGIN:VCALENDAR\r\n"));
        assert!(ics.content.contains("SUMMARY:Team standup\r\n"));
        assert!(ics.content.contains("LOCATION:Zoom\r\n"));
        assert!(ics.content.contains("DTSTART:20260403T090000\r\n"));
        assert!(ics.content.contains("DTSTART;VALUE=DATE:20260404\r\n"));
        assert!(ics.content.contains("CATEGORIES:#personal\r\n"));
    }
}
