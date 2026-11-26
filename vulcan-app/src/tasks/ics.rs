//! `TaskNotes` tasks as an iCalendar feed: one event per open occurrence in a
//! date window, with each `TaskNotes` reminder as an alarm on its event.
//!
//! Calendar apps fire alarms, not task reminders, so this is how reminders
//! reach a phone without a notifier. Alarm instants are the ones
//! `tasks reminders` reports: floating times are read in
//! `tasks.reminders.timezone`, and a date without a time anchors at
//! `tasks.reminders.default_time`.

use super::reminders::{tasknote_reminder_instant, zoned_instant, ReminderSettings};
use super::{date_part, shift_date_keeping_time, TaskNoteRecord};
use crate::AppError;
use jiff::civil::Date;
use jiff::Timestamp;
use serde::Serialize;
use std::fmt::Write as _;
use vulcan_core::tasks::next_recurrence_occurrence;
use vulcan_core::{parse_tasknote_reminders, IndexedTaskNote};

/// The default window length when `--to` is not given.
pub const DEFAULT_TASK_ICS_WINDOW_DAYS: i64 = 60;
/// Occurrences one recurring task contributes at most, whatever the window.
const MAX_OCCURRENCES_PER_TASK: usize = 400;
/// Length of a timed event whose task has no time estimate.
const DEFAULT_EVENT_MINUTES: i64 = 30;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskIcsRequest {
    /// First day of the window (`YYYY-MM-DD`, `today`, `-1w`, ...); today by default.
    pub from: Option<String>,
    /// Last day of the window; 60 days after `from` by default.
    pub to: Option<String>,
    pub calendar_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskIcsExport {
    pub calendar_name: String,
    pub from: String,
    pub to: String,
    pub task_count: usize,
    pub event_count: usize,
    pub alarm_count: usize,
    #[serde(skip)]
    pub content: String,
}

/// One task occurrence, ready to render.
struct TaskEvent {
    uid: String,
    title: String,
    start: EventStart,
    description: String,
    categories: Vec<String>,
    alarms: Vec<(i64, String)>,
}

enum EventStart {
    AllDay(Date),
    Timed { start_ms: i64, minutes: i64 },
}

impl EventStart {
    fn sort_key(&self) -> (String, i64) {
        match self {
            Self::AllDay(day) => (day.to_string(), 0),
            Self::Timed { start_ms, .. } => (
                utc_timestamp(*start_ms).strftime("%Y-%m-%d").to_string(),
                *start_ms,
            ),
        }
    }
}

/// The window's first and last day from `request`, in the reminder timezone.
pub(super) fn task_ics_window(
    request: &TaskIcsRequest,
    settings: &ReminderSettings,
    now_ms: i64,
) -> Result<(Date, Date), AppError> {
    let today = settings.timezone.date_at(now_ms).to_string();
    let parse = |value: &str| {
        Date::strptime("%Y-%m-%d", value)
            .map_err(|_| AppError::operation(format!("invalid date: {value}")))
    };
    let from = parse(&crate::periodic::normalize_date_argument_at(
        request.from.as_deref(),
        &today,
    )?)?;
    let to = match request.to.as_deref() {
        Some(value) => parse(&crate::periodic::normalize_date_argument_at(
            Some(value),
            &today,
        )?)?,
        None => from
            .checked_add(jiff::Span::new().days(DEFAULT_TASK_ICS_WINDOW_DAYS))
            .map_err(|_| AppError::operation(format!("date out of range: {from}")))?,
    };
    if to < from {
        return Err(AppError::operation(format!(
            "the window ends before it starts: {from} > {to}"
        )));
    }
    Ok((from, to))
}

/// Renders the open `TaskNotes` tasks among `records` whose occurrences fall
/// in `from..=to`. `now_ms` stamps the events.
pub(super) fn render_task_ics(
    records: &[TaskNoteRecord],
    settings: &ReminderSettings,
    window: (Date, Date),
    calendar_name: Option<&str>,
    now_ms: i64,
) -> TaskIcsExport {
    let (from, to) = window;
    let calendar_name = calendar_name
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or("Vulcan Tasks")
        .to_string();
    let mut events = Vec::new();
    let mut task_count = 0;
    for record in records
        .iter()
        .filter(|record| !record.indexed.archived && !record.completed)
    {
        let before = events.len();
        events.extend(task_events(record, settings, from, to));
        if events.len() > before {
            task_count += 1;
        }
    }
    events.sort_by(|left, right| {
        left.start
            .sort_key()
            .cmp(&right.start.sort_key())
            .then_with(|| left.uid.cmp(&right.uid))
    });

    let stamp = ics_utc(now_ms);
    let mut content = String::new();
    let mut alarm_count = 0;
    push_line(&mut content, "BEGIN:VCALENDAR");
    push_line(&mut content, "VERSION:2.0");
    push_line(&mut content, "PRODID:-//Vulcan//Tasks//EN");
    push_line(&mut content, "CALSCALE:GREGORIAN");
    push_line(
        &mut content,
        &format!("X-WR-CALNAME:{}", escape_text(&calendar_name)),
    );
    for event in &events {
        alarm_count += event.alarms.len();
        write_event(&mut content, event, &stamp);
    }
    push_line(&mut content, "END:VCALENDAR");

    TaskIcsExport {
        calendar_name,
        from: from.to_string(),
        to: to.to_string(),
        task_count,
        event_count: events.len(),
        alarm_count,
        content,
    }
}

/// The events of one open task: its current occurrence and, for a recurring
/// task, the later ones in the window that are neither completed nor skipped.
fn task_events(
    record: &TaskNoteRecord,
    settings: &ReminderSettings,
    from: Date,
    to: Date,
) -> Vec<TaskEvent> {
    let task = &record.indexed;
    // A task sits on its scheduled day, else on its due day.
    let Some(anchor) = task.scheduled.as_deref().or(task.due.as_deref()) else {
        return Vec::new();
    };
    let Some(current) = date_part(anchor).and_then(|day| day.parse::<Date>().ok()) else {
        return Vec::new();
    };
    let excluded = task
        .complete_instances
        .iter()
        .chain(&task.skipped_instances)
        .filter_map(|day| date_part(day).map(ToOwned::to_owned))
        .collect::<Vec<_>>();

    // The current instance counts even off the rule (a rescheduled task).
    let mut days = Vec::new();
    if (from..=to).contains(&current) && !excluded.contains(&current.to_string()) {
        days.push(current);
    }
    if let Some(rule) = task.recurrence.as_deref() {
        let mut after = current.max(from.yesterday().unwrap_or(from));
        while days.len() < MAX_OCCURRENCES_PER_TASK {
            let Some(next) = next_recurrence_occurrence(
                rule,
                &current.to_string(),
                &after.to_string(),
                &excluded,
            )
            .and_then(|day| day.parse::<Date>().ok()) else {
                break;
            };
            if next > to {
                break;
            }
            if next != current {
                days.push(next);
            }
            after = next;
        }
    }

    let recurring = task.recurrence.is_some();
    days.into_iter()
        .filter_map(|day| {
            let shift = i64::from(current.until(day).ok()?.get_days());
            let occurrence = shifted(task, shift);
            task_event(record, &occurrence, settings, day, recurring, shift == 0)
        })
        .collect()
}

/// `task` with its scheduled and due dates moved by `days`, keeping times.
fn shifted(task: &IndexedTaskNote, days: i64) -> IndexedTaskNote {
    let mut occurrence = task.clone();
    if days != 0 {
        let shift = |value: &Option<String>| {
            value
                .as_deref()
                .map(|value| shift_date_keeping_time(value, days).unwrap_or_else(|| value.into()))
        };
        occurrence.scheduled = shift(&task.scheduled);
        occurrence.due = shift(&task.due);
    }
    occurrence
}

fn task_event(
    record: &TaskNoteRecord,
    occurrence: &IndexedTaskNote,
    settings: &ReminderSettings,
    day: Date,
    recurring: bool,
    current: bool,
) -> Option<TaskEvent> {
    let anchor = occurrence
        .scheduled
        .as_deref()
        .or(occurrence.due.as_deref())?;
    let start = if has_time(anchor) {
        EventStart::Timed {
            start_ms: zoned_instant(anchor, settings)?,
            minutes: occurrence
                .time_estimate
                .filter(|minutes| *minutes >= 1.0)
                .map_or(DEFAULT_EVENT_MINUTES, estimate_minutes),
        }
    } else {
        EventStart::AllDay(day)
    };

    let mut alarms = Vec::new();
    for reminder in parse_tasknote_reminders(&occurrence.reminders) {
        // An absolute reminder belongs to the instance it was set on.
        if reminder.reminder_type.eq_ignore_ascii_case("absolute") && !current {
            continue;
        }
        if let Some(at) = tasknote_reminder_instant(occurrence, &reminder, settings) {
            let text = reminder
                .description
                .clone()
                .filter(|text| !text.trim().is_empty())
                .unwrap_or_else(|| occurrence.title.clone());
            alarms.push((at, text));
        }
    }

    let mut description = vec![
        format!("Status: {}", occurrence.status),
        format!("Priority: {}", occurrence.priority),
    ];
    if let Some(scheduled) = occurrence.scheduled.as_deref() {
        description.push(format!("Scheduled: {scheduled}"));
    }
    if let Some(due) = occurrence.due.as_deref() {
        description.push(format!("Due: {due}"));
    }
    description.push(format!("Source: {}", record.path));

    // A recurring task's occurrences are separate events; a one-off task
    // keeps its UID when rescheduled, so calendars move the event.
    let uid_source = if recurring {
        format!("{}|{day}", record.path)
    } else {
        record.path.clone()
    };
    Some(TaskEvent {
        uid: format!(
            "{}@vulcan-tasks",
            &blake3::hash(uid_source.as_bytes()).to_hex()[..32]
        ),
        title: occurrence.title.clone(),
        start,
        description: description.join("\n"),
        categories: occurrence
            .contexts
            .iter()
            .chain(&occurrence.projects)
            .chain(&occurrence.tags)
            .map(|value| category_name(value))
            .filter(|value| !value.is_empty())
            .collect(),
        alarms,
    })
}

/// A context, project, or tag as a calendar category: `@home` is `home`, and
/// a project link `[[Projects/Website|Site]]` is `Site` (else `Website`).
fn category_name(value: &str) -> String {
    let value = value.trim().trim_start_matches(['@', '#', '+']);
    let Some(link) = value
        .strip_prefix("[[")
        .and_then(|rest| rest.strip_suffix("]]"))
    else {
        return value.to_string();
    };
    if let Some((_, alias)) = link.split_once('|') {
        return alias.trim().to_string();
    }
    let target = link.split('#').next().unwrap_or(link);
    target
        .rsplit('/')
        .next()
        .unwrap_or(target)
        .trim()
        .to_string()
}

/// A time estimate in whole minutes, capped at a week.
#[allow(clippy::cast_possible_truncation)]
fn estimate_minutes(minutes: f64) -> i64 {
    // Rounded and clamped, so the cast cannot truncate.
    minutes.round().clamp(1.0, 7.0 * 24.0 * 60.0) as i64
}

fn has_time(value: &str) -> bool {
    value.trim().len() > 10
}

fn write_event(content: &mut String, event: &TaskEvent, stamp: &str) {
    push_line(content, "BEGIN:VEVENT");
    push_line(content, &format!("UID:{}", event.uid));
    push_line(content, &format!("DTSTAMP:{stamp}"));
    match &event.start {
        EventStart::AllDay(day) => {
            push_line(content, &format!("DTSTART;VALUE=DATE:{}", ics_date(*day)));
            push_line(
                content,
                &format!(
                    "DTEND;VALUE=DATE:{}",
                    ics_date(day.tomorrow().unwrap_or(*day))
                ),
            );
        }
        EventStart::Timed { start_ms, minutes } => {
            push_line(content, &format!("DTSTART:{}", ics_utc(*start_ms)));
            push_line(
                content,
                &format!("DTEND:{}", ics_utc(start_ms + minutes * 60_000)),
            );
        }
    }
    push_line(content, &format!("SUMMARY:{}", escape_text(&event.title)));
    push_line(
        content,
        &format!("DESCRIPTION:{}", escape_text(&event.description)),
    );
    if !event.categories.is_empty() {
        let categories = event
            .categories
            .iter()
            .map(|category| escape_text(category))
            .collect::<Vec<_>>()
            .join(",");
        push_line(content, &format!("CATEGORIES:{categories}"));
    }
    push_line(content, "TRANSP:TRANSPARENT");
    for (at, text) in &event.alarms {
        push_line(content, "BEGIN:VALARM");
        push_line(content, "ACTION:DISPLAY");
        push_line(
            content,
            &format!("TRIGGER;VALUE=DATE-TIME:{}", ics_utc(*at)),
        );
        push_line(content, &format!("DESCRIPTION:{}", escape_text(text)));
        push_line(content, "END:VALARM");
    }
    push_line(content, "END:VEVENT");
}

fn ics_date(day: Date) -> String {
    day.strftime("%Y%m%d").to_string()
}

fn ics_utc(at_ms: i64) -> String {
    utc_timestamp(at_ms).strftime("%Y%m%dT%H%M%SZ").to_string()
}

fn utc_timestamp(at_ms: i64) -> Timestamp {
    Timestamp::from_millisecond(at_ms).unwrap_or(Timestamp::UNIX_EPOCH)
}

fn escape_text(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\r', "")
        .replace('\n', "\\n")
        .replace(';', "\\;")
        .replace(',', "\\,")
}

/// Appends one content line, folded at 75 octets as RFC 5545 requires,
/// never inside a UTF-8 character.
fn push_line(content: &mut String, line: &str) {
    let mut width = 0;
    for character in line.chars() {
        let size = character.len_utf8();
        if width + size > 75 {
            content.push_str("\r\n ");
            width = 1;
        }
        content.push(character);
        width += size;
    }
    let _ = write!(content, "\r\n");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::reminders::ReminderTimezone;
    use serde_json::json;

    fn settings() -> ReminderSettings {
        ReminderSettings {
            inline: false,
            due_dates: false,
            default_time: jiff::civil::time(9, 0, 0, 0),
            timezone: ReminderTimezone::named("Europe/Berlin").unwrap(),
        }
    }

    #[allow(clippy::needless_pass_by_value)]
    fn task(path: &str, fields: serde_json::Value) -> TaskNoteRecord {
        let mut value = json!({
            "title": path.trim_end_matches(".md"),
            "status": "open",
            "priority": "normal",
            "due": null,
            "scheduled": null,
            "completed_date": null,
            "date_created": null,
            "date_modified": null,
            "archived": false,
            "tags": [],
            "contexts": [],
            "projects": [],
            "time_estimate": null,
            "recurrence": null,
            "recurrence_anchor": null,
            "complete_instances": [],
            "skipped_instances": [],
            "blocked_by": [],
            "reminders": [],
            "time_entries": [],
            "custom_fields": {},
        });
        for (key, field) in fields.as_object().unwrap() {
            value[key] = field.clone();
        }
        TaskNoteRecord {
            path: path.to_string(),
            indexed: serde_json::from_value(value).unwrap(),
            completed: false,
        }
    }

    fn day(value: &str) -> Date {
        value.parse().unwrap()
    }

    fn unfold(content: &str) -> String {
        content.replace("\r\n ", "")
    }

    #[test]
    fn one_off_tasks_become_events_with_reminder_alarms() {
        let records = vec![
            task(
                "Dentist.md",
                json!({
                    "due": "2026-11-06T14:00",
                    "time_estimate": 45.0,
                    "contexts": ["@town"],
                    "reminders": [
                        {"id": "r1", "type": "relative", "relatedTo": "due", "offset": "-PT1H", "description": "Leave now"},
                        {"id": "r2", "type": "absolute", "absoluteTime": "2026-11-05T09:00"},
                    ],
                }),
            ),
            task("Taxes.md", json!({"due": "2026-11-20"})),
            task("Later.md", json!({"due": "2027-03-01"})),
            task("Undated.md", json!({})),
        ];
        let export = render_task_ics(
            &records,
            &settings(),
            (day("2026-11-01"), day("2026-11-30")),
            None,
            0,
        );
        let content = unfold(&export.content);

        assert_eq!(
            (export.task_count, export.event_count, export.alarm_count),
            (2, 2, 2)
        );
        assert!(content.starts_with("BEGIN:VCALENDAR\r\n"));
        assert!(content.contains("X-WR-CALNAME:Vulcan Tasks\r\n"));
        // 14:00 in Berlin (CET) is 13:00 UTC; 45 minutes long.
        assert!(content.contains("DTSTART:20261106T130000Z\r\nDTEND:20261106T134500Z\r\n"));
        assert!(content
            .contains("TRIGGER;VALUE=DATE-TIME:20261106T120000Z\r\nDESCRIPTION:Leave now\r\n"));
        assert!(
            content.contains("TRIGGER;VALUE=DATE-TIME:20261105T080000Z\r\nDESCRIPTION:Dentist\r\n")
        );
        assert!(content.contains("CATEGORIES:town\r\n"));
        // A date-only task is an all-day event.
        assert!(content.contains("DTSTART;VALUE=DATE:20261120\r\nDTEND;VALUE=DATE:20261121\r\n"));
        assert!(!content.contains("Later") && !content.contains("Undated"));
        assert!(export.content.split("\r\n").all(|line| line.len() <= 75));
    }

    #[test]
    fn recurring_tasks_repeat_their_relative_reminders_on_each_open_occurrence() {
        let records = vec![task(
            "Standup.md",
            json!({
                "scheduled": "2026-11-02T09:30",
                "recurrence": "DTSTART:20261102;FREQ=WEEKLY;BYDAY=MO",
                "skipped_instances": ["2026-11-16"],
                "reminders": [
                    {"id": "r1", "type": "relative", "relatedTo": "scheduled", "offset": "-PT10M"},
                    {"id": "r2", "type": "absolute", "absoluteTime": "2026-11-01T20:00"},
                ],
            }),
        )];
        let export = render_task_ics(
            &records,
            &settings(),
            (day("2026-11-01"), day("2026-11-30")),
            Some("Work"),
            0,
        );
        let content = unfold(&export.content);

        // Mondays 2, 9, 23, 30 November; the 16th was skipped.
        assert_eq!(export.event_count, 4, "{content}");
        for start in [
            "20261102T083000Z",
            "20261109T083000Z",
            "20261123T083000Z",
            "20261130T083000Z",
        ] {
            assert!(content.contains(&format!("DTSTART:{start}")), "{start}");
        }
        assert!(!content.contains("DTSTART:20261116"));
        assert!(content.contains("TRIGGER;VALUE=DATE-TIME:20261123T082000Z"));
        // The absolute reminder fires once, with the current instance.
        assert_eq!(
            content
                .matches("TRIGGER;VALUE=DATE-TIME:20261101T190000Z")
                .count(),
            1
        );
        assert_eq!(export.alarm_count, 5);
        // Each occurrence is its own event.
        let uids = content
            .lines()
            .filter(|line| line.starts_with("UID:"))
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(uids.len(), 4);
        assert!(content.contains("X-WR-CALNAME:Work\r\n"));
    }

    #[test]
    fn categories_name_contexts_tags_and_project_links() {
        assert_eq!(category_name("@home"), "home");
        assert_eq!(category_name("#work"), "work");
        assert_eq!(category_name("[[Projects/Website]]"), "Website");
        assert_eq!(category_name("[[Projects/Website|Site]]"), "Site");
        assert_eq!(category_name("[[Plan#Goals]]"), "Plan");
    }

    #[test]
    fn long_lines_fold_without_splitting_characters() {
        let mut content = String::new();
        push_line(&mut content, &format!("SUMMARY:{}", "ü".repeat(60)));
        for line in content.split("\r\n").filter(|line| !line.is_empty()) {
            assert!(line.len() <= 75, "{line}");
        }
        assert_eq!(unfold(&content), format!("SUMMARY:{}\r\n", "ü".repeat(60)));
    }

    #[test]
    fn windows_default_to_sixty_days_and_reject_reversed_ranges() {
        let now = "2026-11-13T12:00:00Z"
            .parse::<Timestamp>()
            .unwrap()
            .as_millisecond();
        let (from, to) = task_ics_window(&TaskIcsRequest::default(), &settings(), now).unwrap();
        assert_eq!((from, to), (day("2026-11-13"), day("2027-01-12")));
        let reversed = TaskIcsRequest {
            from: Some("2026-11-10".to_string()),
            to: Some("2026-11-01".to_string()),
            calendar_name: None,
        };
        assert!(task_ics_window(&reversed, &settings(), now).is_err());
    }
}
