//! Reminder occurrences for inline tasks, and the occurrence identity shared
//! with `TaskNotes` reminders.
//!
//! An occurrence is one delivery of one reminder: a task, which of its
//! reminders, and the instant it fires. Its id is stable across rescans and
//! line moves, so delivery ledgers can record what they already sent.

use super::TaskReminderItem;
use crate::AppError;
use chrono::{NaiveDate, NaiveDateTime, NaiveTime, TimeZone};
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
#[derive(Debug, Clone, Copy)]
pub enum ReminderTimezone {
    Named(chrono_tz::Tz),
    Local,
}

impl ReminderTimezone {
    /// `tasks.reminders.timezone`, or the device's local timezone.
    pub fn from_config(config: &TaskRemindersConfig) -> Result<Self, AppError> {
        match config.timezone.as_deref() {
            Some(name) => name.parse::<chrono_tz::Tz>().map(Self::Named).map_err(|_| {
                AppError::operation(format!(
                    "tasks.reminders.timezone is not an IANA timezone name: {name}"
                ))
            }),
            None => Ok(Self::Local),
        }
    }

    /// The calendar date at `now_ms` (Unix ms) in this timezone.
    #[must_use]
    pub fn date_at(self, now_ms: i64) -> NaiveDate {
        let instant = chrono::DateTime::from_timestamp_millis(now_ms).unwrap_or_default();
        match self {
            Self::Named(tz) => instant.with_timezone(&tz).date_naive(),
            Self::Local => instant.with_timezone(&chrono::Local).date_naive(),
        }
    }

    /// The wall-clock time at `at_ms` in this timezone.
    #[must_use]
    pub fn local_datetime(self, at_ms: i64) -> NaiveDateTime {
        let instant = chrono::DateTime::from_timestamp_millis(at_ms).unwrap_or_default();
        match self {
            Self::Named(tz) => instant.with_timezone(&tz).naive_local(),
            Self::Local => instant.with_timezone(&chrono::Local).naive_local(),
        }
    }

    /// `at_ms` as `YYYY-MM-DD HH:MM` wall-clock time in this timezone.
    #[must_use]
    pub fn format_local(self, at_ms: i64) -> String {
        let instant = chrono::DateTime::from_timestamp_millis(at_ms).unwrap_or_default();
        match self {
            Self::Named(tz) => instant
                .with_timezone(&tz)
                .format("%Y-%m-%d %H:%M")
                .to_string(),
            Self::Local => instant
                .with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M")
                .to_string(),
        }
    }

    /// The instant (Unix ms) of a wall-clock time. A time inside a DST gap
    /// fires at the first valid minute after it; a repeated time fires at
    /// its first occurrence.
    #[must_use]
    pub fn instant_ms(self, local: NaiveDateTime) -> Option<i64> {
        (0..=180).find_map(|minutes| {
            let candidate = local + chrono::Duration::minutes(minutes);
            match self {
                Self::Named(tz) => tz
                    .from_local_datetime(&candidate)
                    .earliest()
                    .map(|at| at.timestamp_millis()),
                Self::Local => chrono::Local
                    .from_local_datetime(&candidate)
                    .earliest()
                    .map(|at| at.timestamp_millis()),
            }
        })
    }
}

/// Parsed `[tasks.reminders]` settings.
#[derive(Debug, Clone)]
pub struct ReminderSettings {
    pub inline: bool,
    pub due_dates: bool,
    pub default_time: NaiveTime,
    pub timezone: ReminderTimezone,
}

impl ReminderSettings {
    pub fn from_config(config: &TaskRemindersConfig) -> Result<Self, AppError> {
        let default_time =
            NaiveTime::parse_from_str(config.default_time.trim(), "%H:%M").map_err(|_| {
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
fn zoned_instant(value: &str, settings: &ReminderSettings) -> Option<i64> {
    let value = value.trim();
    if let Ok(at) = chrono::DateTime::parse_from_rfc3339(value) {
        return Some(at.timestamp_millis());
    }
    for format in [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%dT%H:%M",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%d %H:%M",
    ] {
        if let Ok(local) = NaiveDateTime::parse_from_str(value, format) {
            return settings.timezone.instant_ms(local);
        }
    }
    if let Ok(date) = NaiveDate::parse_from_str(value, "%Y-%m-%d") {
        return settings
            .timezone
            .instant_ms(date.and_time(settings.default_time));
    }
    parse_date_like_string(value)
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
                    .and_then(|value| NaiveDate::parse_from_str(value.get(..10)?, "%Y-%m-%d").ok())
                    .map(|date| ("due", "due", Some("due".to_string()), date, None))
            };
            let Some((reminder_id, reminder_type, related_to, date, time)) = remind.or_else(due)
            else {
                continue;
            };
            let Some(notify_at) = settings
                .timezone
                .instant_ms(date.and_time(time.unwrap_or(settings.default_time)))
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
            default_time: NaiveTime::from_hms_opt(9, 0, 0).expect("valid time"),
            timezone: ReminderTimezone::Named(chrono_tz::Europe::Berlin),
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
        chrono::DateTime::parse_from_rfc3339(text)
            .expect("valid timestamp")
            .timestamp_millis()
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
        let berlin = ReminderTimezone::Named(chrono_tz::Europe::Berlin);
        let at = |text: &str| {
            berlin
                .instant_ms(NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M").unwrap())
                .unwrap()
        };
        // 10:00 CET is 09:00 UTC.
        assert_eq!(
            at("2026-11-07 10:00"),
            chrono::Utc
                .with_ymd_and_hms(2026, 11, 7, 9, 0, 0)
                .unwrap()
                .timestamp_millis()
        );
        // 02:30 does not exist on 2026-03-29 in Berlin: fire at 03:00 CEST.
        assert_eq!(
            at("2026-03-29 02:30"),
            chrono::Utc
                .with_ymd_and_hms(2026, 3, 29, 1, 0, 0)
                .unwrap()
                .timestamp_millis()
        );
        // 02:30 repeats on 2026-10-25: fire at the first one (CEST).
        assert_eq!(
            at("2026-10-25 02:30"),
            chrono::Utc
                .with_ymd_and_hms(2026, 10, 25, 0, 30, 0)
                .unwrap()
                .timestamp_millis()
        );
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
}
