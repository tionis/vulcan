//! Reminder occurrences for inline tasks, and the occurrence identity shared
//! with `TaskNotes` reminders.
//!
//! An occurrence is one delivery of one reminder: a task, which of its
//! reminders, and the instant it fires. Its id is stable across rescans and
//! line moves, so delivery ledgers can record what they already sent.

use super::TaskReminderItem;
use crate::AppError;
use jiff::civil::{Date, DateTime, Time};
use jiff::tz::{AmbiguousOffset, TimeZone};
use jiff::{SignedDuration, Timestamp, Zoned};
use serde_json::Value;
use std::collections::HashMap;
use vulcan_core::config::{TaskDueDateReminders, TaskRemindersConfig, TasksConfig};
use vulcan_core::expression::functions::parse_date_like_string;
use vulcan_core::tasks::{inline_task_block_id, inline_task_title, parse_inline_reminder_value};
use vulcan_core::{parse_iso8601_duration_ms, IndexedTaskNote, NoteRecord, TaskNotesReminder};

const OCCURRENCE_ID_DOMAIN: &str = "vulcan-reminder-occurrence-v1";

/// The stable id of one reminder delivery.
#[must_use]
pub fn reminder_occurrence_id(task_key: &str, reminder_id: &str, notify_at_ms: i64) -> String {
    let mut hasher = blake3::Hasher::new();
    for part in [
        OCCURRENCE_ID_DOMAIN,
        task_key,
        reminder_id,
        &notify_at_ms.to_string(),
    ] {
        hasher.update(part.as_bytes());
        hasher.update(&[0]);
    }
    hasher.finalize().to_hex()[..32].to_string()
}

/// The task key of a `TaskNotes` task: its note path.
#[must_use]
pub fn tasknote_task_key(path: &str) -> String {
    format!("tasknote:{path}")
}

/// The task key of an inline task: its `🆔`, else its block id, else a hash
/// of its path and title. Editing the text of a task without an id makes a
/// new key, so its reminders become new occurrences.
#[must_use]
pub fn inline_task_key(path: &str, text: &str, id: Option<&str>) -> String {
    if let Some(id) = id.map(str::trim).filter(|id| !id.is_empty()) {
        return format!("id:{id}");
    }
    if let Some(block) = inline_task_block_id(text) {
        return format!("block:{path}#{block}");
    }
    let mut hasher = blake3::Hasher::new();
    hasher.update(path.as_bytes());
    hasher.update(&[0]);
    hasher.update(inline_task_title(text).as_bytes());
    format!("text:{path}:{}", &hasher.finalize().to_hex()[..16])
}

/// The timezone reminder wall-clock times are read in.
#[derive(Debug, Clone)]
pub struct ReminderTimezone(TimeZone);

impl ReminderTimezone {
    /// `tasks.reminders.timezone`, or the device's local timezone.
    pub fn from_config(config: &TaskRemindersConfig) -> Result<Self, AppError> {
        match config.timezone.as_deref() {
            Some(name) => Self::named(name).ok_or_else(|| {
                AppError::operation(format!(
                    "tasks.reminders.timezone is not an IANA timezone name: {name}"
                ))
            }),
            None => Ok(Self::local()),
        }
    }

    /// The IANA timezone `name`, such as `Europe/Berlin`.
    #[must_use]
    pub fn named(name: &str) -> Option<Self> {
        TimeZone::get(name).ok().map(Self)
    }

    /// The device's local timezone.
    #[must_use]
    pub fn local() -> Self {
        Self(TimeZone::system())
    }

    fn zoned(&self, at_ms: i64) -> Zoned {
        Timestamp::from_millisecond(at_ms)
            .unwrap_or(Timestamp::UNIX_EPOCH)
            .to_zoned(self.0.clone())
    }

    /// The calendar date at `now_ms` (Unix ms) in this timezone.
    #[must_use]
    pub fn date_at(&self, now_ms: i64) -> Date {
        self.zoned(now_ms).date()
    }

    /// The wall-clock time at `at_ms` in this timezone.
    #[must_use]
    pub fn local_datetime(&self, at_ms: i64) -> DateTime {
        self.zoned(at_ms).datetime()
    }

    /// The wall-clock time at `at_ms` in this timezone, read as if it were
    /// UTC, for callers that render wall-clock milliseconds.
    #[must_use]
    pub fn wall_clock_ms(&self, at_ms: i64) -> i64 {
        self.local_datetime(at_ms)
            .to_zoned(TimeZone::UTC)
            .map_or(at_ms, |wall| wall.timestamp().as_millisecond())
    }

    /// `at_ms` as `YYYY-MM-DD HH:MM` wall-clock time in this timezone.
    #[must_use]
    pub fn format_local(&self, at_ms: i64) -> String {
        self.zoned(at_ms).strftime("%Y-%m-%d %H:%M").to_string()
    }

    /// `at_ms` as an RFC 3339 date-time with this timezone's offset, such as
    /// `2026-11-19T09:00:00+01:00` (`Z` for UTC).
    #[must_use]
    pub fn format_rfc3339(&self, at_ms: i64) -> String {
        let zoned = self.zoned(at_ms);
        let base = zoned.strftime("%Y-%m-%dT%H:%M:%S").to_string();
        if zoned.offset() == jiff::tz::Offset::UTC {
            format!("{base}Z")
        } else {
            format!("{base}{}", zoned.strftime("%:z"))
        }
    }

    /// The instant (Unix ms) of a wall-clock time. A time inside a DST gap
    /// fires at the first valid minute after it; a repeated time fires at
    /// its first occurrence.
    #[must_use]
    pub fn instant_ms(&self, local: DateTime) -> Option<i64> {
        (0..=180).find_map(|minutes| {
            let candidate = local.checked_add(SignedDuration::from_mins(minutes)).ok()?;
            let ambiguous = self.0.to_ambiguous_timestamp(candidate);
            if matches!(ambiguous.offset(), AmbiguousOffset::Gap { .. }) {
                return None;
            }
            ambiguous.earlier().ok().map(Timestamp::as_millisecond)
        })
    }
}

/// Parsed `[tasks.reminders]` settings.
#[derive(Debug, Clone)]
pub struct ReminderSettings {
    pub inline: bool,
    pub due_dates: bool,
    pub default_time: Time,
    pub timezone: ReminderTimezone,
}

impl ReminderSettings {
    pub fn from_config(config: &TaskRemindersConfig) -> Result<Self, AppError> {
        let default_time = Time::strptime("%H:%M", config.default_time.trim()).map_err(|_| {
            AppError::operation(format!(
                "tasks.reminders.default_time must be HH:MM: {}",
                config.default_time
            ))
        })?;
        Ok(Self {
            inline: config.inline,
            due_dates: config.due_dates == TaskDueDateReminders::Notify,
            default_time,
            timezone: ReminderTimezone::from_config(config)?,
        })
    }
}

/// The instant (Unix ms) a `TaskNotes` reminder fires.
///
/// Times without a zone (`2026-11-08T12:00`) are wall-clock times in `tasks.reminders.timezone`,
/// as `TaskNotes` reads them in the device's timezone. A relative reminder on a date without a
/// time is anchored at `tasks.reminders.default_time` rather than midnight.
#[must_use]
pub fn tasknote_reminder_instant(
    task: &IndexedTaskNote,
    reminder: &TaskNotesReminder,
    settings: &ReminderSettings,
) -> Option<i64> {
    if reminder.reminder_type.eq_ignore_ascii_case("absolute") {
        return reminder
            .absolute_time
            .as_deref()
            .and_then(|value| zoned_instant(value, settings));
    }
    let anchor = match reminder.related_to.as_deref() {
        Some("scheduled") => task.scheduled.as_deref(),
        Some("due") => task.due.as_deref(),
        _ => None,
    }?;
    let offset_ms = reminder
        .offset
        .as_deref()
        .and_then(parse_iso8601_duration_ms)?;
    zoned_instant(anchor, settings).map(|anchor_ms| anchor_ms + offset_ms)
}

/// A date or date-time from task frontmatter as an instant: an explicit offset is honored, a
/// floating time is read in the reminder timezone, and a bare date means the default time.
pub(super) fn zoned_instant(value: &str, settings: &ReminderSettings) -> Option<i64> {
    let value = value.trim();
    if let Some(at_ms) = rfc3339_ms(value) {
        return Some(at_ms);
    }
    for format in [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%dT%H:%M",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%d %H:%M",
    ] {
        if let Ok(local) = DateTime::strptime(format, value) {
            return settings.timezone.instant_ms(local);
        }
    }
    if let Ok(date) = Date::strptime("%Y-%m-%d", value) {
        return settings
            .timezone
            .instant_ms(date.to_datetime(settings.default_time));
    }
    parse_date_like_string(value)
}

/// An RFC 3339 date-time with an offset or `Z`, as Unix ms.
pub(crate) fn rfc3339_ms(value: &str) -> Option<i64> {
    // A civil time alone is not an instant; jiff's parser would otherwise
    // accept bare dates and times only with an offset anyway.
    value
        .parse::<Timestamp>()
        .ok()
        .map(Timestamp::as_millisecond)
}

/// Give absolute `TaskNotes` reminders written without a zone, such as
/// `2026-11-19T09:00` or a bare date, the explicit offset of the reminder
/// timezone. `TaskNotes` writes absolute reminders as RFC 3339 instants and its
/// mdbase schema requires them; a floating time fails validation. Times that
/// already carry an offset, and reminders that are not absolute, are kept.
pub(super) fn anchor_absolute_reminders(reminders: &mut [Value], config: &TaskRemindersConfig) {
    let Ok(settings) = ReminderSettings::from_config(config) else {
        return;
    };
    for reminder in reminders {
        let Some(object) = reminder.as_object_mut() else {
            continue;
        };
        if !object
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(|kind| kind.eq_ignore_ascii_case("absolute"))
        {
            continue;
        }
        let Some(time) = object.get("absoluteTime").and_then(Value::as_str) else {
            continue;
        };
        if rfc3339_ms(time.trim()).is_some() {
            continue;
        }
        if let Some(at_ms) = zoned_instant(time, &settings) {
            let anchored = settings.timezone.format_rfc3339(at_ms);
            object.insert("absoluteTime".to_string(), Value::String(anchored));
        }
    }
}

/// Reminders of open inline tasks that fire at or before `deadline_ms`.
pub(super) fn inline_reminder_items(
    note_index: &HashMap<String, NoteRecord>,
    tasks_config: &TasksConfig,
    settings: &ReminderSettings,
    now_ms: i64,
    deadline_ms: i64,
    format_instant: impl Fn(i64) -> String,
) -> Vec<TaskReminderItem> {
    if !settings.inline && !settings.due_dates {
        return Vec::new();
    }
    let global_filter = tasks_config
        .global_filter
        .as_deref()
        .map(str::trim)
        .filter(|filter| !filter.is_empty());
    let mut items = Vec::new();

    for note in note_index.values() {
        for task in &note.tasks {
            if task.properties.get("taskSource").and_then(Value::as_str) == Some("file")
                || task.completed
                || matches!(task.status_type.as_str(), "CANCELLED" | "NON_TASK" | "DONE")
                || global_filter.is_some_and(|filter| !task.text.contains(filter))
            {
                continue;
            }
            let remind = settings
                .inline
                .then(|| property_text(task.properties.get("remind")))
                .flatten()
                .and_then(|value| parse_inline_reminder_value(&value))
                .map(|reminder| ("remind", "absolute", None, reminder.date, reminder.time));
            let due = || {
                settings
                    .due_dates
                    .then(|| property_text(task.properties.get("due")))
                    .flatten()
                    .and_then(|value| Date::strptime("%Y-%m-%d", value.get(..10)?).ok())
                    .map(|date| ("due", "due", Some("due".to_string()), date, None))
            };
            let Some((reminder_id, reminder_type, related_to, date, time)) = remind.or_else(due)
            else {
                continue;
            };
            let Some(notify_at) = settings
                .timezone
                .instant_ms(date.to_datetime(time.unwrap_or(settings.default_time)))
            else {
                continue;
            };
            if notify_at > deadline_ms {
                continue;
            }

            let id = property_text(task.properties.get("id"));
            let task_key = inline_task_key(&note.document_path, &task.text, id.as_deref());
            let mut title = inline_task_title(&task.text);
            if tasks_config.remove_global_filter {
                if let Some(filter) = global_filter {
                    title = title
                        .split_whitespace()
                        .filter(|word| *word != filter)
                        .collect::<Vec<_>>()
                        .join(" ");
                }
            }
            let recurring = property_text(task.properties.get("recurrence")).is_some();
            items.push(TaskReminderItem {
                path: note.document_path.clone(),
                title,
                status: task.status_name.clone(),
                priority: property_text(task.properties.get("priority"))
                    .unwrap_or_else(|| "normal".to_string()),
                reminder_id: reminder_id.to_string(),
                reminder_type: reminder_type.to_string(),
                related_to,
                description: None,
                notify_at: format_instant(notify_at),
                overdue: notify_at < now_ms,
                occurrence: recurring
                    .then(|| property_text(task.properties.get("due")))
                    .flatten()
                    .and_then(|due| due.get(..10).map(ToOwned::to_owned)),
                source: "inline".to_string(),
                task: format!("{}:{}", note.document_path, task.line_number),
                occurrence_id: reminder_occurrence_id(&task_key, reminder_id, notify_at),
                task_key,
                created_ms: None,
            });
        }
    }
    items
}

fn property_text(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(text) => Some(text.trim().to_string()).filter(|text| !text.is_empty()),
        Value::Array(values) => values.iter().find_map(|value| property_text(Some(value))),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn berlin_settings() -> ReminderSettings {
        ReminderSettings {
            inline: true,
            due_dates: false,
            default_time: jiff::civil::time(9, 0, 0, 0),
            timezone: ReminderTimezone::named("Europe/Berlin").expect("known zone"),
        }
    }

    fn task(scheduled: &str) -> IndexedTaskNote {
        IndexedTaskNote {
            title: "Think".to_string(),
            status: "open".to_string(),
            priority: "normal".to_string(),
            due: None,
            scheduled: Some(scheduled.to_string()),
            completed_date: None,
            date_created: None,
            date_modified: None,
            archived: false,
            tags: Vec::new(),
            contexts: Vec::new(),
            projects: Vec::new(),
            time_estimate: None,
            recurrence: None,
            recurrence_anchor: None,
            complete_instances: Vec::new(),
            skipped_instances: Vec::new(),
            blocked_by: Vec::new(),
            reminders: Vec::new(),
            time_entries: Vec::new(),
            custom_fields: serde_json::Map::new(),
        }
    }

    fn relative(offset: &str) -> TaskNotesReminder {
        TaskNotesReminder {
            id: "r".to_string(),
            reminder_type: "relative".to_string(),
            related_to: Some("scheduled".to_string()),
            offset: Some(offset.to_string()),
            absolute_time: None,
            description: None,
        }
    }

    fn utc(text: &str) -> i64 {
        text.parse::<Timestamp>()
            .expect("valid timestamp")
            .as_millisecond()
    }

    #[test]
    fn tasknote_reminders_read_floating_times_in_the_reminder_timezone() {
        let settings = berlin_settings();
        // 12:00 in Berlin (CET, UTC+1) is 11:00 UTC.
        assert_eq!(
            tasknote_reminder_instant(&task("2026-11-08T12:00:00"), &relative("-PT0M"), &settings),
            Some(utc("2026-11-08T11:00:00Z"))
        );
        assert_eq!(
            tasknote_reminder_instant(&task("2026-11-08T12:00"), &relative("-PT15M"), &settings),
            Some(utc("2026-11-08T10:45:00Z"))
        );
        // An explicit offset wins over the reminder timezone.
        assert_eq!(
            tasknote_reminder_instant(&task("2026-11-08T12:00:00Z"), &relative("-PT0M"), &settings),
            Some(utc("2026-11-08T12:00:00Z"))
        );
        // A bare date is anchored at the default time, not midnight.
        assert_eq!(
            tasknote_reminder_instant(&task("2026-11-08"), &relative("-PT0M"), &settings),
            Some(utc("2026-11-08T08:00:00Z"))
        );
        let absolute = TaskNotesReminder {
            reminder_type: "absolute".to_string(),
            related_to: None,
            offset: None,
            absolute_time: Some("2026-07-01T08:30:00".to_string()),
            ..relative("-PT0M")
        };
        // Summer time: 08:30 CEST is 06:30 UTC.
        assert_eq!(
            tasknote_reminder_instant(&task("2026-11-08"), &absolute, &settings),
            Some(utc("2026-07-01T06:30:00Z"))
        );
    }

    #[test]
    fn occurrence_ids_are_stable_and_distinct() {
        let first = reminder_occurrence_id("id:abc", "remind", 1_000);
        assert_eq!(first, reminder_occurrence_id("id:abc", "remind", 1_000));
        assert_ne!(first, reminder_occurrence_id("id:abc", "remind", 2_000));
        assert_ne!(first, reminder_occurrence_id("id:abd", "remind", 1_000));
        assert_ne!(first, reminder_occurrence_id("id:abc", "due", 1_000));
        assert_eq!(first.len(), 32);
    }

    #[test]
    fn task_keys_prefer_ids_then_block_ids_then_text() {
        assert_eq!(inline_task_key("a.md", "Call ^b", Some("T-1")), "id:T-1");
        assert_eq!(inline_task_key("a.md", "Call ^b", None), "block:a.md#b");
        let by_text = inline_task_key("a.md", "Call X ⏰ 2026-11-07 10:00", None);
        assert_eq!(
            by_text,
            inline_task_key("a.md", "Call X ⏰ 2026-11-08 09:00 📅 2026-11-09", None),
            "rescheduling keeps the key; only the occurrence instant changes"
        );
        assert_ne!(by_text, inline_task_key("a.md", "Call Y", None));
        assert_ne!(by_text, inline_task_key("b.md", "Call X", None));
    }

    #[test]
    fn named_timezones_resolve_wall_times_including_dst_gaps() {
        let berlin = ReminderTimezone::named("Europe/Berlin").expect("known zone");
        let at = |text: &str| {
            berlin
                .instant_ms(DateTime::strptime("%Y-%m-%d %H:%M", text).unwrap())
                .unwrap()
        };
        // 10:00 CET is 09:00 UTC.
        assert_eq!(at("2026-11-07 10:00"), utc("2026-11-07T09:00:00Z"));
        // 02:30 does not exist on 2026-03-29 in Berlin: fire at 03:00 CEST.
        assert_eq!(at("2026-03-29 02:30"), utc("2026-03-29T01:00:00Z"));
        // 02:30 repeats on 2026-10-25: fire at the first one (CEST).
        assert_eq!(at("2026-10-25 02:30"), utc("2026-10-25T00:30:00Z"));
    }

    #[test]
    fn settings_reject_bad_times_and_timezones() {
        let mut config = TaskRemindersConfig::default();
        assert!(ReminderSettings::from_config(&config).is_ok());
        config.default_time = "9am".to_string();
        assert!(ReminderSettings::from_config(&config).is_err());
        config.default_time = "09:00".to_string();
        config.timezone = Some("Mars/Olympus".to_string());
        assert!(ReminderSettings::from_config(&config).is_err());
    }

    #[test]
    fn floating_absolute_reminders_get_the_reminder_timezone_offset() {
        let config = TaskRemindersConfig {
            timezone: Some("Europe/Berlin".to_string()),
            default_time: "08:30".to_string(),
            ..TaskRemindersConfig::default()
        };
        let mut reminders = vec![
            serde_json::json!({"id": "a", "type": "absolute", "absoluteTime": "2026-11-19T09:00"}),
            serde_json::json!({"id": "b", "type": "absolute", "absoluteTime": "2026-07-01T09:00:00"}),
            serde_json::json!({"id": "c", "type": "absolute", "absoluteTime": "2026-11-20"}),
            serde_json::json!({"id": "d", "type": "absolute", "absoluteTime": "2026-11-19T08:00:00.000Z"}),
            serde_json::json!({"id": "e", "type": "relative", "relatedTo": "due", "offset": "-PT15M"}),
        ];
        let untouched = reminders[3..].to_vec();
        anchor_absolute_reminders(&mut reminders, &config);
        let times = reminders[..3]
            .iter()
            .map(|reminder| reminder["absoluteTime"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            times,
            [
                "2026-11-19T09:00:00+01:00",
                "2026-07-01T09:00:00+02:00",
                "2026-11-20T08:30:00+01:00",
            ]
        );
        assert_eq!(reminders[3..], untouched[..], "zoned and relative kept");
    }
}
