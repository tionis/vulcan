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
use vulcan_core::tasks::{inline_task_block_id, inline_task_title, parse_inline_reminder_value};
use vulcan_core::NoteRecord;

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
