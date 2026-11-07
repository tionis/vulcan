//! Times typed in a chat reply: `in 2h`, `90m`, `15:00`, `tomorrow`,
//! `tomorrow 15:00`, `friday at 9am`, `2026-11-20 08:30`.

use crate::tasks::ReminderTimezone;
use chrono::{NaiveDate, NaiveDateTime, NaiveTime, Timelike};
use vulcan_core::config::TaskNotesConfig;
use vulcan_core::expression::functions::parse_duration_string;
use vulcan_core::parse_tasknote_natural_language;

/// A parsed reply time: a wall-clock instant, or a whole day.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatWhen {
    At(NaiveDateTime),
    Day(NaiveDate),
}

impl ChatWhen {
    /// The wall-clock time, with `default_time` for a whole day.
    #[must_use]
    pub fn at_or(self, default_time: NaiveTime) -> NaiveDateTime {
        match self {
            Self::At(at) => at,
            Self::Day(day) => day.and_time(default_time),
        }
    }
}

/// Parse `text` as a time relative to `now_ms`, in `timezone`. `None` when
/// the text is not just a time: a reply must not move a task on a guess.
#[must_use]
pub fn parse_chat_when(
    text: &str,
    config: &TaskNotesConfig,
    timezone: ReminderTimezone,
    now_ms: i64,
) -> Option<ChatWhen> {
    let text = text.trim().trim_end_matches(['.', '!']).trim();
    if text.is_empty() {
        return None;
    }
    let local_now = timezone.local_datetime(now_ms);
    let duration = text.strip_prefix("in ").unwrap_or(text).replace(' ', "");
    if let Some(ms) = parse_duration_string(&duration).filter(|ms| *ms > 0) {
        let at = local_now + chrono::Duration::milliseconds(ms);
        // Up to the next whole minute, the precision of reminders.
        let rounded = at.with_second(0)?.with_nanosecond(0)?;
        let rounded = if rounded < at {
            rounded + chrono::Duration::minutes(1)
        } else {
            rounded
        };
        return Some(ChatWhen::At(rounded));
    }
    if let Some(time) = clock_time(text) {
        let today = local_now.date().and_time(time);
        // A time already past today means tomorrow.
        return Some(ChatWhen::At(if today <= local_now {
            today + chrono::Duration::days(1)
        } else {
            today
        }));
    }
    // The task NLP reads dates relative to the reference's UTC day, so pass
    // the local day.
    let reference = local_now
        .date()
        .and_time(NaiveTime::MIN)
        .and_utc()
        .timestamp_millis();
    let parsed = parse_tasknote_natural_language(&format!("x {text}"), config, reference);
    if parsed.title != "x" || parsed.recurrence.is_some() {
        return None;
    }
    let value = parsed.due.or(parsed.scheduled)?;
    if let Ok(at) = NaiveDateTime::parse_from_str(&value, "%Y-%m-%dT%H:%M:%S") {
        return Some(ChatWhen::At(at));
    }
    NaiveDate::parse_from_str(&value, "%Y-%m-%d")
        .ok()
        .map(ChatWhen::Day)
}

/// `15:00`, `9:30`, `3pm`, `3 pm`, `10:15am`.
fn clock_time(text: &str) -> Option<NaiveTime> {
    let text = text.to_ascii_lowercase();
    let (clock, meridiem) = match text.strip_suffix("am").or_else(|| text.strip_suffix("pm")) {
        Some(clock) => (clock.trim(), Some(text.ends_with("pm"))),
        None => (text.as_str(), None),
    };
    let (hour, minute) = clock.split_once(':').unwrap_or((clock, "00"));
    if hour.is_empty() || hour.len() > 2 || minute.len() != 2 {
        return None;
    }
    let (mut hour, minute) = (hour.parse::<u32>().ok()?, minute.parse::<u32>().ok()?);
    match meridiem {
        Some(pm) => {
            if !(1..=12).contains(&hour) {
                return None;
            }
            hour = hour % 12 + if pm { 12 } else { 0 };
        }
        // A bare number is not a time.
        None if !clock.contains(':') => return None,
        None => {}
    }
    NaiveTime::from_hms_opt(hour, minute, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn when(text: &str) -> Option<ChatWhen> {
        // Saturday 2026-11-07 10:20:30 UTC.
        let now = chrono::DateTime::parse_from_rfc3339("2026-11-07T10:20:30Z")
            .unwrap()
            .timestamp_millis();
        parse_chat_when(
            text,
            &TaskNotesConfig::default(),
            ReminderTimezone::Named(chrono_tz::UTC),
            now,
        )
    }

    fn at(text: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M").unwrap()
    }

    #[test]
    fn reads_durations_clock_times_and_date_phrases() {
        assert_eq!(when("in 2h"), Some(ChatWhen::At(at("2026-11-07 12:21"))));
        assert_eq!(when("90m"), Some(ChatWhen::At(at("2026-11-07 11:51"))));
        assert_eq!(when("15:00"), Some(ChatWhen::At(at("2026-11-07 15:00"))));
        assert_eq!(
            when("9:00"),
            Some(ChatWhen::At(at("2026-11-08 09:00"))),
            "past today"
        );
        assert_eq!(when("3pm"), Some(ChatWhen::At(at("2026-11-07 15:00"))));
        assert_eq!(when("12:15 am"), Some(ChatWhen::At(at("2026-11-08 00:15"))));
        assert_eq!(when("12pm"), Some(ChatWhen::At(at("2026-11-07 12:00"))));
        assert_eq!(
            when("tomorrow 15:00"),
            Some(ChatWhen::At(at("2026-11-08 15:00")))
        );
        assert_eq!(
            when("monday at 9am"),
            Some(ChatWhen::At(at("2026-11-09 09:00")))
        );
        assert_eq!(
            when("tomorrow"),
            Some(ChatWhen::Day(NaiveDate::from_ymd_opt(2026, 11, 8).unwrap()))
        );
        assert_eq!(
            when("2026-11-20 08:30"),
            Some(ChatWhen::At(at("2026-11-20 08:30")))
        );
    }

    #[test]
    fn refuses_anything_that_is_not_just_a_time() {
        assert_eq!(when(""), None);
        assert_eq!(when("thanks"), None);
        assert_eq!(when("15"), None, "a bare number");
        assert_eq!(when("25:00"), None);
        assert_eq!(when("tomorrow buy milk"), None);
        assert_eq!(when("every monday"), None);
    }
}
